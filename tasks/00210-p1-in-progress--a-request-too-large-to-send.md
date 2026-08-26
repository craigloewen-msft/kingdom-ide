# A request too large to send

A plan dies with `Copilot returned 413: Request Entity Too Large`, and nothing
the King does brings it back. `plan-22` in the `dev` kingdom is the specimen:
302 entries, `copilot/claude-opus-5` at high effort, killed at 20:11:43 and
killed twice more in the following forty seconds -- once for "Keep going", once
for "Ok I have an error about request entity too large so don't do what you were
doing". Identical failure, identical timing, no way out.

This is the failure `00200-p1--an-empty-reply-is-not-the-end-of-a-plan` fixed for
silence, arriving again through a different door. The lesson recorded there is
the one that applies: *the first failure was never the unfixable half -- the
unfixable half was that nothing the King did could change the request.*

## What is actually happening

Measured against the plan's own record:

| On the wire, every round | Size |
|---|---|
| images (6, base64) | **4.02 MB** |
| `reasoning.opaque` | 633 KB |
| tool results (12 KB cap already applied) | 318 KB |
| `reasoning.text` | 192 KB |
| tool call inputs | 145 KB |
| narration + messages | 11 KB |
| **total** | **~5.3 MB** |

Three quarters of that body is pictures, and every one of them had already been
looked at and answered.

### The chain

1. `read_image` puts base64 bytes into `ToolOutcome::Done { images }` on the live
   in-memory plan. Six calls here, the largest an 829 KB PNG -- 1.1 MB once
   base64'd.
2. **Nothing ever takes them out of memory.** `store::without_images` strips on
   the way to *disk*, and deliberately clones so the live plan keeps them.
   `Plan::for_wire` strips `reasoning.opaque` for the *browser*. Neither touches
   what goes to the model. The images sit in the `Mutex<Kingdom>` for the life of
   the process.
3. `copilot::messages()` calls `shown(tool_call)` for **every** tool call in the
   whole transcript, on **every** round. All six pictures ride every request,
   forever.
4. The body crossed the gateway's limit. 413.

### Why the King cannot recover

- 413 is a 4xx that is neither 5xx nor 429, so `take_turn` maps it to
  `ModelError::Refused`, which `is_transient()` says is not worth asking again.
  No retry.
- Retrying unchanged would not help anyway. The request is deterministic:
  `Plan::turns` rebuilds it byte-identically from the transcript, so "Keep going"
  reassembles exactly the 5.3 MB that was just rejected. That is the loop.
- The only relief today is restarting the server, which drops the images from
  memory because disk never had them. That still leaves 1.27 MB of text growing
  round by round, so it buys time rather than a fix.

### The part that misled the King

The header read **257k / 1M tokens**. He had every reason to believe he had three
quarters of his window left, because he did. The gateway's limit is on *bytes*,
and `ContextUsage` cannot see it. Nothing in the UI could have warned him.

## The shape of the fix

`replayed()` already establishes the principle -- bound what goes back on the
wire, and mark honestly what was dropped. Its doc even names the reason: "an
unbounded result is a bill that grows with the square of the conversation." The
same reasoning was simply never applied to images, to `reasoning.opaque`, or to
the body as a whole.

### A. Stop replaying pictures that have already been answered

The primary fix, and it is the code catching up with what the domain already
says. `ToolArtifact`'s own doc: *"`images` is what the model was shown, true for
one turn."* `copilot::messages` treats them as true forever.

Replay images only from the last round or two of tool calls -- a small `RECENT`
window. Older calls keep their text ("Looked at .../foo.png (637003 bytes)."),
which is what the model actually reasons from once it has described what it saw,
and keep their `ToolArtifact` so the chamber still draws the picture for the
King. Where an image is dropped, say so, the way `replayed()` does -- a model
told the picture is no longer attached can take another screenshot; one silently
blinded will cite detail it can no longer see.

This alone takes plan-22 from 5.3 MB to ~1.3 MB.

### B. Give 413 its own error, distinct from a refusal

`ModelError::TooLarge`. A refusal is a considered answer and a retry is futile; a
413 is the gateway declining to *read* the question, and the remedy is to ask a
smaller one. Folding it into `Refused` loses exactly the distinction that decides
whether the plan can recover. This mirrors the `Empty` / `Refused` split for the
same reason and should be argued in the same place, on `is_transient`.

`is_transient()` stays **false** -- resending unchanged is pointless. What
`TooLarge` earns is a retry that *shrinks first*, which is a different question
from "ask again unchanged" and wants its own branch in `converse`.

### C. A budget on the assembled body, not only on each result

`MOST_REPLAYED` bounds one result at 12 KB. Nothing bounds the sum, which is why
318 KB of results across 300 entries sails past a per-item cap. Measure the
assembled body and, when it is over budget, shed in priority order:

1. images beyond the recent window (A already does this)
2. `reasoning.opaque` on the oldest calls -- 633 KB here, half the remaining
   text, and nothing in the UI has ever drawn it. **Only from calls old enough
   that the thinking they sign is no longer live**: `Reasoning::without_opaque`
   warns that the gateway silently discards signed thinking when it is not echoed
   back, and that warning is load-bearing. This is the delicate one and wants its
   own test.
3. tighter truncation on the oldest results, marked as ever

Budget stated in bytes with headroom, because it is a byte limit we are avoiding.

### D. Retry the shrink, and tell the King what happened

On `TooLarge`, `converse` re-assembles under a tighter budget and asks again,
rather than settling. If it is still too large at the floor, the note the plan
dies with should say *what* was too large and by how much -- "the request came to
5.3 MB, of which 4.0 MB was images" is something the King can act on; "Request
Entity Too Large" is not.

## Done when

- Loading a transcript of `plan-22`'s shape and assembling a request produces a
  body under budget. This is the regression test, and the specimen is on disk.
- A transcript with many `read_image` calls replays only the recent ones' bytes,
  and says so where it dropped one.
- A 413 is `ModelError::TooLarge`, not `Refused`, and `converse` shrinks and
  retries rather than settling on the first one.
- `reasoning.opaque` is never dropped from a call recent enough for the gateway
  to still want it echoed -- pinned by a test, per `Reasoning::without_opaque`.
- The existing containment and strip tests still pass: images stay out of
  `store::save`, artifacts stay in, `for_wire` still drops only `opaque`.
- `cargo test -p kingdom-core` and
  `cargo test -p kingdom-app --features ssr --no-default-features`.

## Not in scope

- Reporting wire bytes beside tokens in the chamber header. The King was misled
  by a context bar that was telling the truth about the wrong quantity, and that
  is worth fixing -- but it is a UI change with its own decisions, and this task
  is about the plan not dying.
- Summarising or compacting old history. A real answer to long conversations, and
  a much larger one. Bounding what is replayed is the fix that matches the bug.
- The Responses API migration, which `copilot::shown` already names as the
  correct eventual home for images.
