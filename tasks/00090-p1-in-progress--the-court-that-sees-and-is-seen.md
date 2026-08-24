# The court that sees, and is seen

Today the court can drive a browser and cannot look at it, and neither can the
King. `browser_take_screenshot` writes a PNG into the workspace and hands back a
path — to a model with no eyes. Meanwhile a plan can be several minutes into a
browser flow with nothing on screen but a list of tool names.

Both halves of that are the same missing thing: **nobody can see the page.** This
task gives sight to the two parties who need it, and finishes the browser tool
surface while it is in there.

Three pieces, in dependency order:

1. **`read_image`** — and the model layer learning to carry an image at all.
2. **The spyglass** — a live view of the court's browser, in the plan's chamber.
3. **`browser_profile`** — the last unported browser tool.

Only the first is load-bearing for the others' *reasoning*; none of them block
each other in code. Land them in order anyway, because piece 1 is the one that
changes a shared domain type and everything downstream is cheaper once it settles.

## What is already here (so nobody re-ports it)

All ten of Phoenix's `browser_*` tools are already in
`crates/kingdom-app/src/tools/browser.rs`, thin over `kingdom-browser`:
navigate, click, type, key_press, eval, wait_for_selector, take_screenshot,
resize, recent_console_logs, clear_console_logs. **Do not touch them.** The only
missing tool is `browser_profile`.

The push spine is also already built: `herald.rs` fans a plan's changes out over
a per-plan `broadcast` channel, `watch.rs` serves `/watch/plan/{id}`, and
`conversation.rs` holds a working wasm WebSocket client (`PlanWatch`) with
reconnect and drop-safety. Piece 2 is a sibling of that, not a new spine.

Two module doc comments currently assert that the screencast is *deliberately*
absent — the head of `crates/kingdom-browser/src/lib.rs` and the head of
`crates/kingdom-app/src/tools/browser.rs`. The reasoning given ("they serve
Phoenix UI consumers Kingdom does not have") was correct when written and is
now overtaken: Kingdom is about to have that consumer. **Rewrite both comments
rather than deleting them** — a reader should find out that the decision was
revisited and why, not that it was never made.

---

## Piece 1 — `read_image`, and a `Deed` that can hold a picture

Phoenix's `crates/phoenix-tools/src/read_image.rs` is 150 lines of tool and is
the easy part. The work is underneath it.

### The domain gains images on an outcome

`DeedOutcome::Done { output: String }` is text-only, and `Deed::report()` returns
`&str`. Both need a sibling channel for image payloads:

```rust
pub enum DeedOutcome {
    Done {
        output: String,
        /// Images the tool produced, for a model that can see them.
        ///
        /// Separate from `output` rather than encoded into it: the text is what
        /// the transcript renders and what a blind model is told, and a
        /// megabyte of base64 spliced into that would be unreadable in the
        /// chamber and unusable in a prompt.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        images: Vec<DeedImage>,
    },
    Refused { reason: String },
}

pub struct DeedImage {
    /// `image/png`, `image/jpeg`, …
    pub media_type: String,
    /// Base64, no data-URL prefix.
    pub data: String,
}
```

Constraints:

- **`kingdom-core` must still compile to wasm.** `String` and `Vec` only; no
  base64 crate in core — encoding happens in the tool, which is server-side.
- `#[serde(default)]` so a plan document written before this field existed still
  loads. There is already precedent (`Deed::at`, `ModelOption::can_act`) and
  there should be a test that a pre-images plan JSON deserializes.
- `DeedOutcome::Done` gaining a field breaks every construction site.
  There are ~32 across eight tool modules plus `mock.rs` and `conversation.rs`.
  Add a constructor — `DeedOutcome::done(output)` — and convert the sites to it,
  so the *next* field is a one-line change rather than another 32.

### Images must not be written to the plan's record

This is the sharp edge. `store.rs` writes each plan as one JSON document under
`<kingdom_root>/.kingdom/plans/<id>.json`, and every deed is in it. A court that
takes twenty screenshots would write twenty base64 megabytes into a file that is
rewritten on every update, forever.

Decide this explicitly and write the reason down. The recommended shape: images
live on the in-memory outcome and are **dropped on the way to disk**, with the
text (`"Image loaded: <path> (N bytes)"`) surviving. The path is in the text, the
file is in the workspace, so a reloaded plan can still say what was looked at
even though it can no longer show it. If that is unacceptable, the alternative is
a sidecar file per image under `.kingdom/` — but that invents a second lifetime
to manage and nothing has asked for it.

### The wire: Copilot cannot carry an image on a tool result

Kingdom posts to `/chat/completions`. That format's `role:"tool"` message takes a
string; it has no image part. This is not a guess — Phoenix hits the same wall
and gives up:

> `"dropping images from chat completions tool result — unsupported by this wire format"`
> — `crates/phoenix-llm/src/openai.rs`

**Decision (taken): the image follows the tool result as a synthetic user
message.** In `copilot.rs::messages()`, a deed whose outcome carries images emits
three messages instead of two:

```text
assistant  tool_calls:[…]                      (unchanged)
tool       "Image loaded: /path/x.png (48 KB)"  (unchanged, text only)
user       [{type:"image_url",
             image_url:{url:"data:image/png;base64,…"}}]
```

Chat-completions *does* accept `image_url` parts on user-role content, so this
works against the gateway Kingdom already talks to. Three things to be careful of,
and each deserves a comment where it happens:

- **The synthetic turn exists only on the wire.** It is built inside
  `copilot.rs`, from a `Deed`. It is never a `Turn::Said`, never an `Utterance`,
  never in the transcript. `Brief::turns` is untouched. This matters: the doc on
  `Turn` argues at length that Kingdom's plumbing must not be replayed to a model
  in the King's voice, and a `user` message the King did not say is exactly that
  hazard — contained here by never letting it exist as a domain value.
- Give it a short leading text part ("The image from the previous tool call:") so
  the model is not handed a bare picture with no antecedent.
- The rejected alternative is moving Copilot to the Responses API, where
  `FunctionCallOutput` carries `input_image` parts natively and no synthetic turn
  is needed. That is the *correct* wire format and a much larger change than this
  task — request shape, response parsing and the tool-call path all move. Note it
  as the eventual right answer in the comment, so whoever does that work finds
  this shim rather than rediscovering the problem.

### A model that cannot see must not be offered the tool

Sending an image to a text-only model earns an opaque gateway rejection that
fails the whole turn — the same asymmetric cost that `ModelOption::can_act`
already reasons about, and it should get the same treatment:

- `ModelOption` gains `can_see: bool`, `#[serde(default)]` false, parsed from
  Copilot's `capabilities.supports.vision` in `parse_one`. **Verify the field's
  real name against a live `/models` payload before trusting it** — the existing
  code reads `capabilities.supports.tool_calls`, so the shape is right, but the
  key is not confirmed. Absent means no, for the same reason as `can_act`:
  guessing wrong costs a whole turn in one direction and a slightly weaker
  answer in the other.
- `Model` gains `fn can_see(&self) -> bool`, defaulting false on the trait.
- `api.rs` already filters the tool list on `can_act` (~line 563). Extend that
  one seam: `read_image` is withheld from a model that cannot see. A blind model
  never calls a tool whose result it cannot use, and never wastes the King's turn
  discovering that.
- Defensively, `copilot.rs::messages()` drops images when `!can_see` even if one
  reaches it. The filter is the policy; this is the belt.

### The tool itself

Port `read_image.rs`: resolve through `Workshop::resolve` (**not** Phoenix's
`resolve_path` — the workspace boundary is Kingdom's and it is not optional),
reject non-files, cap at 5 MB, allow png/jpg/jpeg/gif/webp by extension, base64
the bytes. `base64` is already in the lockfile as a transitive dep; add it as a
direct optional dep under `ssr`.

Register it in `tools::all()` next to the browser tools, since that is what it
pairs with, and update `browser_take_screenshot`'s description to point at it —
the screenshot tool's current wording promises a path and stops, which is why a
model has no reason to look.

---

## Piece 2 — The spyglass: watching the court's browser

A canvas in the plan's chamber showing what the court's headless Chrome is
rendering, live. This is the product's first question — *what is this agent doing
right now?* — answered in the most literal way available.

### Naming

The module is the King's **spyglass**: `crates/kingdom-app/src/spyglass.rs`,
serving `/watch/plan/{id}/browser`, with `components/spyglass.rs` in the UI.
`watch.rs` is taken by the chamber socket and the two are genuinely different
things — one carries plans, one carries pixels.

### The broker (`crates/kingdom-browser/src/screencast.rs`)

Port `phoenix-browser/src/screencast.rs` (~270 lines) nearly verbatim. It is
clean, self-contained, and depends on nothing Phoenix-specific but its own
`BrowserError`. Keep its structure exactly, because the structure is the point:

- `Page.startScreencast` forces a paint per frame and is genuinely expensive.
  Start lazily on first viewer, stop on last detach.
- That lifecycle is enforced **structurally**, not by bookkeeping: each viewer
  holds an `Arc<ScreencastBroker>`, the session holds only a `Weak`, and the
  broker's `Drop` aborts the listener task and fires `Page.stopScreencast`. Do
  not "simplify" this into a counter — the counter is the version that leaks a
  screencast when a viewer's task panics.
- `broadcast` channel, capacity 16, JPEG quality 70. A slow viewer that lags
  skips frames rather than stalling the source. For a live view, stale frames are
  worse than missing ones.
- Needs `base64` (CDP delivers frames base64-encoded) and
  `futures`/`tokio` — both already dependencies of `kingdom-browser`.

`BrowserSession` gains a `screencast: Mutex<Weak<ScreencastBroker>>` slot and an
`attach_viewer()` returning `(Arc<Broker>, Receiver, Option<url>)`.
`BrowserSessionManager` gains a way to reach a session **without creating one** —
this is the important one, see below.

### The socket (`crates/kingdom-app/src/spyglass.rs`)

Port the wire format from `phoenix-ide/src/api/browser_view.rs` unchanged; it is
deliberately trivial so both ends can be read side by side:

```text
0x00 → frame:  [0x00][u32 BE jpeg length][jpeg bytes]
0x01 → url:    [0x01][utf-8 url]
0x02 → status: [0x02][utf-8 "no-session" | "started" | "ended" | "error: …"]
```

Axum route registered in `main.rs` beside `watch::ROUTE`. Everything Phoenix's
auth and work-scope resolution does collapses to one line here: the plan id in
the path *is* the session key, exactly as it is for the tools.

**Never lazily create a session.** A viewer attaches to whatever browser the plan
already has; if there is none it is told `no-session` and the socket closes. This
is not a detail to trim — a panel that launches Chrome by being opened would make
the act of *looking* spawn a process, which is precisely the invisible-resource
problem this product exists to expose.

### The panel (`crates/kingdom-app/src/components/spyglass.rs`)

A Leptos component mounting a `<canvas>`, fed by a wasm WebSocket in
`binaryType = "arraybuffer"`. `PlanWatch` in `conversation.rs` is the model to
follow — closure retention, reconnect-with-timeout, and a `Drop` that clears
`onclose` *before* closing so teardown does not schedule the reconnect it is
trying to prevent. That drop-order comment exists because the bug is easy; read
it before writing this one.

Render frames by decoding each JPEG blob to an `ImageBitmap`/`HTMLImageElement`
and drawing to the canvas. `web-sys` needs `HtmlCanvasElement`,
`CanvasRenderingContext2d`, `Blob`, `Url` (and `BinaryType`) added to its feature
list.

Toggled from the chamber — a spyglass button beside the transcript — with three
visible states: not opened, `no-session` ("the court has not opened a browser"),
and live with the current URL in a header. Styling in `style/`, beside the deed
styling.

### View-only, and this is a locked non-goal

No input path back into the page. Not "not yet" — **no**. Phoenix locked this for
its own reasons; Kingdom's are sharper. Two input sources driving one page, with
nothing arbitrating between them, is a textbook instance of the exact collision
class this product was built to surface. Shipping one *inside* the tool that is
supposed to reveal collisions would be self-refuting.

Enforce it where it is visible: `pointer-events: none` on the canvas, so a King
who clicks gets no ambiguous non-response, and no fourth tag byte in the
protocol. Say all of this in the module doc.

### Explicit non-goal: the map does not learn about this

The natural next thought — a city on the map lighting up because a plan holds a
live browser — needs the plan to *know* it has a session, and needs the map to
receive live updates at all. That second thing is listed in `AGENTS.md` §4 as not
built. Both are real and neither is this task. Record what it would take (likely
a field on `Plan` set when a browser deed opens a session, proclaimed by the
herald like anything else) and stop there. Guessing at UI nobody has asked for is
how the lease machinery happened.

---

## Piece 3 — `browser_profile`

The last unported browser tool: `phoenix-tools/src/browser/profile.rs`, ~2,460
lines. Systematic web performance measurement through an `action` discriminator —
`metrics`, `throttle`, `run_scenario`, `cpu_start`/`cpu_stop`/`cpu_summary`,
`trace_start`/`trace_stop`, `gc_heap`, `heap_snapshot`, `coverage_start`/
`coverage_stop`, `why_render`, `help`.

The module doc in `tools/browser.rs` says this was left out because it "serves
Phoenix UI consumers Kingdom does not have." That reasoning does not survive
contact with the file: `browser_profile` returns text to a model and has no UI
consumer at all. It was a size decision wearing a principle's clothes. Correct
the comment when the tool lands.

Port notes:

- Keep the single-tool-with-`action` shape. Phoenix documents why at the top of
  the module and the reason carries over intact: the actions form start/stop
  sub-machines over shared per-session profiling state, and splitting them into
  separate `Tool` structs scatters that state and loses the precondition gates.
  This is the one place Kingdom's one-struct-per-tool norm is deliberately
  broken; say so in the doc, as Phoenix does.
- **The `run_scenario` invariant is the load-bearing one.** It returns raw
  per-run samples and computes no mean, no variance, no significance. A profiler
  that averages for you is a profiler that hides the bimodal distribution which
  was the actual finding. There must be exactly one place samples are emitted and
  it must be the untouched `Vec`. This is worth a test.
- Per-session profiling state (CPU profile in progress, trace in progress,
  coverage started) hangs off `BrowserSession`, alongside the screencast slot
  from piece 2.
- Large output escapes to a workspace file above 4 KB, matching `browser_eval`'s
  existing behaviour — reuse `browser.rs`'s `artifact()`/`write_artifact()`
  rather than adding a second convention.
- Phoenix's Allium spec and `REQ-BT-019.*` markers do not come across; Kingdom
  has no such framework. Translate the requirements it encodes into doc comments
  and tests. Do not leave dangling `REQ-` references pointing at a spec directory
  that does not exist here.

This piece is genuinely separable. If review is getting long, it is the clean
place to cut a follow-up task — nothing in pieces 1 or 2 depends on it.

---

## Suggested order of work

1. `DeedOutcome::Done` gains `images` + the `done()` constructor; convert the ~32
   sites; old-document deserialization test.
2. `read_image` tool, workspace-rooted, registered.
3. `can_see` on `ModelOption`/`Model`, catalogue parsing, tool filtering in
   `api.rs`.
4. Copilot wire: the synthetic user message; store.rs drops image payloads.
   **End-to-end check: the court screenshots a page, reads it, and describes what
   is on it.** Until that works, piece 1 is not done.
5. `screencast.rs` in `kingdom-browser`, with the `Arc`/`Weak` lifetime.
6. `spyglass.rs` socket + route.
7. The Leptos panel, the chamber toggle, styling.
8. `browser_profile`.

## Done when

- The court takes a screenshot, calls `read_image` on it, and says what is in the
  picture — against a real Copilot vision model.
- A model without vision is never offered `read_image`, and no turn fails because
  an image was sent to something that cannot see one.
- A plan record on disk does not grow by a megabyte per screenshot.
- A plan document written before this task still loads, and there is a test.
- The King opens the spyglass mid-flow and watches the court's page change as it
  is driven. Closing the panel stops the screencast; two panels on one plan share
  one.
- Opening a spyglass on a plan with no browser says so and launches nothing.
- The canvas cannot be clicked into.
- `browser_profile` runs a scenario and returns raw per-run samples, unaveraged.
- `cargo test -p kingdom-core`, and
  `cargo test -p kingdom-app --features ssr --no-default-features`, both pass;
  `kingdom-core` still builds for wasm32; the hydrate bundle still builds and
  does not link `kingdom-browser`.
- The two module docs that call the screencast deliberately absent now explain
  the decision that replaced them.

## Risks worth naming

- **`DeedOutcome` is touched by everything.** It is the return type of every
  tool, it is persisted, it is rendered, and it crosses the wasm boundary. The
  field itself is easy; the blast radius is the work. Do it first and alone.
- **The vision capability key is unverified.** `capabilities.supports.vision` is
  inferred from the shape of the neighbouring `tool_calls` flag, not observed.
  If it is absent from the real payload, every model reads as blind and
  `read_image` is never offered — a silent no-op, which is the worst failure mode
  available. Check the live payload early.
- **Screencast is a long-lived CDP subscription and a paint-per-frame cost.** The
  `Arc`/`Weak` lifecycle is the whole defence. Anything that keeps a strong
  reference somewhere unexpected turns "the King glanced at a page once" into a
  permanent tax on that plan's Chrome.
- **A synthetic user message is a lie told to the model, carefully.** It is the
  right lie and it is contained to one function, but it makes the wire transcript
  and the plan transcript differ for the first time. If a future feature
  reconstructs one from the other, this is where it will be wrong.
- **Piece 3 is a third of the diff and independent of the rest.** Split it rather
  than rushing review of it.
