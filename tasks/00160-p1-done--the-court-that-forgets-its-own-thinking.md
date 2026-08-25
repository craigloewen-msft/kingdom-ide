# The court that forgets its own thinking

The King reports his plans "doing random weird things". They are: two live plans
spent 47 and 25 rounds each and proposed nothing. This is task 00110
(*A court that remembers why it looked*) reopening — that task fixed the prose
half of the reasoning round-trip and left the signed half broken, and the test it
shipped cannot see the difference.

## What was observed

Four real plans in `~/dev/.kingdom/plans/`, all `copilot/claude-opus-5`:

| Plan | Decree | Rounds | Outcome |
|---|---|---|---|
| `plan-6` | "make the proposal render markdown" | 47 | still `Drafting`, nothing proposed |
| `plan-7` | "remember the last reasoning level" | 25 | still `Drafting`, nothing proposed |
| `plan-3` | "set up this project" | 45 | nothing proposed |
| `plan-5` | deactivation (soft delete) | 146 | reached `propose_plan` twice |

The wandering is not random, it is **repetition**. Counting the model's own
stored `reasoning.text`:

- **`plan-6`: 25 of 30 reasoning blocks re-derive the same single observation** —
  "the system prompt claims mermaid renders, but nothing renders markdown". It
  rediscovers this fact twenty-five times and never acts on it.
- **`plan-7`: 8 of 13 re-derive "the storage logic looks correct, so the bug must
  be elsewhere"** — then re-reads the same four files looking for the elsewhere.

That is an agent that cannot see what it concluded last round. Same signature as
00110, and for a closely related reason.

## Root causes

### 1. The opaque half of the reasoning round-trip is written back under the wrong key

This is the main cause. `crates/kingdom-app/src/llm/copilot.rs`.

Read (`parse_reasoning`, ~line 855) — three possible keys:

```rust
let opaque = ["reasoning_opaque", "encrypted_content", "signature"]
    .iter()
    .find_map(|key| { … });
```

Written back (`messages`, ~line 511) — **one** key, and not necessarily the one
it came from:

```rust
Some(other) => assistant["reasoning_opaque"] = other.clone(),
```

The key it was read from is not remembered, so a blob that arrived as `signature`
goes back as `reasoning_opaque`. `Reasoning::opaque`'s own doc says these fields
"must be quoted back exactly as they came… carried, not understood" — the code
violates the contract it documents.

**This fires on every round of every plan.** All **111** opaque values across the
four plans are JSON *strings*, never objects — so the `Some(Value::Object(..))`
arm that would preserve the original keys never runs, and the mis-keying arm
always does. Kingdom never sends `reasoning_opaque` itself, so the source key is
the gateway's: `signature` (the Anthropic thinking-block signature) or
`encrypted_content`. A thinking block whose signature is missing is discarded by
the gateway, so the model gets its tool results back with its own thinking
stripped — exactly the failure `messages()`' doc comment claims is fixed:

> *Dropping this is what made long investigations wander and repeat themselves.*

**Why the suite is silent.** `a_models_own_reasoning_is_handed_back_to_it`
(~line 1024) builds `Reasoning { text: Some(..), opaque: None }` and asserts only
on `reasoning_content`. The opaque path has no test at all. The request stays
well-formed and the gateway keeps accepting it, so the failure is invisible
everywhere except in the transcripts.

**Change.** Remember the key the blob was read under and write it back under that
same key. Simplest shape: add the source key to `Reasoning` (e.g.
`opaque_key: Option<String>`, defaulted for old records), or store opaque as a
single-entry map of `key -> value` so the object arm — which already round-trips
correctly — becomes the only arm. Prefer the latter: it deletes the broken branch
rather than adding a field beside it.

**Test.** Assert an opaque blob read from `signature` is replayed under
`signature` and *not* under `reasoning_opaque`. That single assertion is the
whole regression.

### 2. Batch ids collide across turns

`api.rs` ~line 811:

```rust
let batch = format!("{}-{round}", plan_id.as_str());
```

`round` is the `for round in 0..cap` index, which **restarts at 0 every turn**.
Visible in `plan-5`: `plan-5-12` (a `propose_plan`), then the next turn opens
`plan-5-0`, and a later turn opens `plan-5-0` again. Batch ids are not unique
within a plan.

`messages()` groups *consecutive* turns sharing a batch id, and `Plan::turns()`
filters `Entry::Note` out. So when a turn ends on a note rather than a message —
the `Failed` note the server leaves when it dies mid-turn, which both `plan-6`
and `plan-7` carry — two calls from **different turns** become adjacent with
equal ids and are merged into one assistant message. The second turn's reasoning
is then dropped, because only `first.reasoning` is read. It compounds cause 1.

**Change.** Make the id unique per turn — carry a turn counter alongside `round`,
or seed it from the transcript length when the turn opens. **Test:** two calls
from different turns separated only by a `Note` must not be replayed as one
assistant message.

### 3. The system prompt tells every model something false

`system_prompt.rs:162`, appended unconditionally to **every** prompt:

```rust
const MERMAID: &str = "The conversation view renders Markdown mermaid code fences as diagrams; …"
```

There is no markdown renderer. No `pulldown-cmark`, no `comrak`, no `inner_html`,
no mermaid.js — `grep -rn mermaid crates/` returns this line and nothing else,
and `conversation.rs:962` states the opposite outright: "markdown renderer -- the
conversation prints every message verbatim".

This is not cosmetic, and `plan-6` is the proof: asked to make proposals render
markdown, the model found the contradiction and then spent **25 of its 30
reasoning blocks** litigating it — "that claim seems aspirational", "that's
actually a false claim in the prompt", "I should flag this discrepancy" — and
never proposed anything. The prompt derailed the very plan that would have fixed
it. It also has every model emitting mermaid fences the King then reads as raw
text.

**Change.** Delete `MERMAID` and its `push_str` (two lines). Restore it the day a
renderer exists. Note `SCREENSHOTS` beside it is *true* — `artifact.rs` does
render those — so this is one bad string, not a bad section.

## Also found — reported, not fixed here

Two things worth the King's eyes that this task deliberately does not build:

- **A credential prefix is on disk in plaintext.** `plan-7` round 19 ran
  `agency auth github … | cut -c1-8` and `gho_Xya5` is now stored in
  `plan-7.json` forever. `store.rs` strips images but redacts nothing. A real fix
  is a redaction pass over tool output, which is its own design decision.
- **A live port collision — the thing this product exists to prevent.** `plan-7`
  ran `cargo leptos serve` with no port override and hit the King's own server on
  `:3000`. The model noticed unaided ("likely the King's own server, so I
  shouldn't kill it") and moved to `:3123`. Nothing in Kingdom detected it; per
  AGENTS.md §4, nothing can yet. Worth logging as the first observed instance.

A one-line prompt hint — *the King's own server may hold :3000; pick a free port*
— is cheap and may be folded in with change 3 while `system_prompt.rs` is open.

## Scope

- `crates/kingdom-app/src/llm/copilot.rs` — the opaque key round-trip, plus a test.
- `crates/kingdom-core/src/model.rs` — `Reasoning`'s opaque shape, if the map form is taken.
- `crates/kingdom-app/src/api.rs` — unique batch ids, plus a test.
- `crates/kingdom-app/src/llm/system_prompt.rs` — delete `MERMAID`; optional port hint.

Three tests total: one per cause. No new fixtures.

## What implementation found that the plan did not

Changing `Reasoning::opaque` to a map is a **stored-format change**, and
`store::load` parses each plan with `serde_json::from_str(..).ok()` — a document
that will not parse is *skipped*. Every plan on disk holds `opaque` as a bare
string, so a strict deserialiser would not have raised an error: it would have
silently emptied the King's rail of every plan that ever thought with a signed
model. Caught before it shipped, and now pinned by
`thinking_recorded_before_opaque_fields_were_keyed_still_loads`.

`Reasoning::opaque` therefore has a custom `deserialize_with` that reads the old
bare value as "no opaque fields" rather than failing. The stale blob is dropped
rather than given an invented key — it is unreplayable either way, since nothing
recorded which field it belonged to, and guessing `signature` would hand a
gateway a blob under a name it may never have used. The prose half still loads.

Verified against the real records: all **13** plans in `~/dev/.kingdom/plans/`
load intact, titles and transcripts included.

Two further notes on what shipped:

- `parse_reasoning` now keeps **every** opaque field present rather than the
  first one found. A provider sending both a signature and an encrypted trace
  needs both back, and `find_map` silently dropped the rest.
- Change 3 replaced `MERMAID` rather than merely deleting it: the freed slot now
  holds `SHARED_MACHINE`, the port-collision hint the plan flagged as optional.

## How to verify

The unit tests pin causes 1 and 2. The real check is behavioural, and both live
plans are ready-made cases: reopen `plan-7` ("remember the last reasoning level")
and confirm it stops re-deriving "the storage logic looks correct" every round
and reaches a `propose_plan`. Its stored transcript is the before-picture.

```bash
cargo test -p kingdom-app --features ssr --no-default-features
cargo test -p kingdom-core
```
