# Tools in the hands of the court

Today the court can only talk. `Model::draft(&Brief) -> Draft` is one HTTP call,
and the system prompt says, in as many words:

> You cannot run commands or edit files: you are writing a proposal for review.

That sentence is the whole gap. A plan is already cut its own git worktree by
`worktree.rs`, and nothing has ever worked in it. This task gives the court
hands, matching the tool surface Phoenix IDE (`~/dev/phoenix-ide`) offers its
agents.

## What this actually is

Four separable things, in dependency order. The first three are the load-bearing
ones — get them wrong and every tool built afterwards inherits the mistake.

### 0. Push, first, before any of it

The WebSocket layer lands as step one, not as a later cleanup. Two independent
forces make polling untenable the moment the court can act, and they turn out to
be the same force:

- **A tool loop has no fixed length.** "Run the tests and fix what fails" is
  minutes of work and a dozen transcript entries. Poll-and-refetch renders that
  as a spinner that occasionally lurches, and it holds one HTTP request open for
  the duration — a request the browser can drop, which is exactly why
  `draft_plan` had to spawn a detached task to protect the busy mark.
- **`ask_user_question` requires the server to speak first.** A tool that asks
  the King a question and blocks on his answer is a server-initiated message. It
  is not implementable over request/response at all without a queue and a poll,
  which is a worse WebSocket built by accident.

So: an `/ws` endpoint on the Axum router, a per-plan subscription, and the
chamber receiving transcript entries as they are appended rather than refetching
the whole plan. The code already anticipates this — `conversation.rs` carries a
note on `poll_while` saying *"delete this when push lands — do not grow it into
a general polling layer."* Delete it.

What this displaces: `draft_plan`'s spawn-and-await-in-request shape becomes
spawn-and-notify, which is what its own comment says it wants to be. The
detached task stays; what changes is that it now has somewhere to report to. The
busy-mark invariant is unchanged and still needs its guard.

What this does **not** mean: streaming token-by-token from the model.
`copilot.rs` is deliberately non-streaming and can stay that way — the unit of
push here is a transcript entry, not a token. Streaming is a later, separate
choice that this layer makes possible rather than requires.

### 1. The domain gains a shape for "the court did something"

`kingdom-core::Entry` today is `Said(Utterance) | Note(Note)`. There is no
variant for an action. The existing doc comment on `Entry` argues at length why
a note is *not* a third `Speaker` — the same argument applies here, and the same
solution:

```rust
pub enum Entry {
    Said(Utterance),
    Note(Note),
    /// Something the court did, and what came back.
    Did(Deed),
}

pub struct Deed {
    /// Correlation id from the provider, so a result finds its call.
    pub id: String,
    /// Which instrument: "bash", "patch", "browser_click", …
    pub tool: String,
    /// The arguments, verbatim, as JSON.
    pub input: serde_json::Value,
    pub outcome: Option<DeedOutcome>,   // None while in flight
    pub at: Option<Timestamp>,
}
```

A `Deed` is not an `Utterance`: nobody spoke it. But unlike a `Note` it **does**
go back to the model — a tool result the model never sees is a tool call it will
immediately repeat. So `Plan::said()` stays exactly as it is (the one door
between the log and a model, and it still does not open for notes), and gains a
sibling that yields the ordered call/result sequence.

`kingdom-core` must still compile to wasm. `serde_json::Value` is fine; nothing
here may pull in tokio or std::fs.

**Migration:** plan JSON on disk is versioned by `.kingdom/kingdom.json`. A new
`Entry` variant is additive — old documents deserialize unchanged. Verify, don't
assume; add a test that a pre-tools plan document still loads.

### 2. The turn becomes a loop

`Model::draft` returns prose *or* a set of tool calls. `api.rs::draft_plan`
becomes:

```
loop {
    call the model
    if it returned prose -> settle, done
    if it returned tool calls -> record each Deed, run them,
                                record outcomes, go round again
}
```

With a hard cap on iterations, recorded as a `Note` when hit — an agent looping
forever on a paid model is the failure mode worth being loud about.

Constraints to respect:

- The plan is already marked busy for the whole turn, and `draft_plan` already
  spawns a detached task precisely so a browser navigating away cannot leave a
  plan permanently `Drafting`. That reasoning gets *more* important with a loop,
  not less. Keep it.
- `working_on` is the field that already exists for "what is this plan doing
  right now" — set it per tool call (`"Running cargo test"`). This is the
  product's first question ("what is every agent doing right now?") finally
  having a real answer.
- Every transcript append is pushed (step 0). The chamber renders the loop as it
  happens; nothing waits for the turn to end.
- Wire format: Copilot's `/chat/completions` takes `tools` and returns
  `tool_calls`. The mock provider needs a scripted equivalent — a new scenario
  that emits a real tool call and then answers from its result. Without that,
  the whole loop is untestable offline, which is the entire reason `mock.rs`
  exists.
- The `Provider` catalogue already filters on capability. A model that does not
  declare `tool_calls` support must not be offered a tool-using turn; decide
  whether it is filtered out or degrades to prose, and say which in the code.

### 3. The tools

A `Tool` trait mirroring Phoenix's (`name`, `description`, `input_schema`,
`async run(input, ctx)`), stateless, all per-call state via a context. New
module `crates/kingdom-app/src/tools/`, server-only (`#[cfg(feature = "ssr")]`)
for the same reason `llm/` is. Named `tools`, not given a metaphor noun — the
precedent is set at the top of `llm/mod.rs`.

Porting from `~/dev/phoenix-ide/crates/phoenix-tools`, roughly in this order:

| Tool | Phoenix source | Notes |
|---|---|---|
| `think` | `think.rs` | trivial; lands the trait |
| `read_file` | `read_file.rs` | offset/limit, numbered lines |
| `search` | `search.rs` | gitignore-aware; `ignore` + `globset` + `regex` |
| `keyword_search` | `keyword_search.rs` | needs a model call of its own |
| `patch` | `patch/` (~3k lines) | planner/matcher/executor, clipboards, reindent |
| `bash` | `bash/` (~5.5k lines) | run/peek/wait/kill, handle registry, ring buffer, reaper, process groups |
| `tmux` | `tmux/` (~4.5k lines) | own server + socket, for TTY/interactive work |
| `browser_*` | `browser/` + `phoenix-browser` (~5.5k) | chromiumoxide/CDP: navigate, click, type, eval, screenshot, key_press, resize, wait_for_selector, console logs, profiling |
| `read_image` | `read_image.rs` | pairs with screenshot |
| `spawn_agents` | `subagent.rs` | fan-out; a plan spawning plans |
| `ask_user_question` | `ask_user_question.rs` | in scope; see below |
| `skill` | `skill.rs` | needs skill discovery |

Deliberately **not** ported: `commission_review`, `work_scope_inventory`,
`process_inspection`, `propose_task`, `terminal_*`, `mcp`, `bash_check`. They
are bound to Phoenix's workflow engine, state machine, task model or terminal
multiplexer — Kingdom has no counterpart to any of those, and porting them means
porting the machinery underneath. Revisit individually if one earns its place.

**The browser wants its own crate.** Phoenix keeps `phoenix-browser` separate
from `phoenix-tools` because chromiumoxide, a CDP driver, a session manager and
a screencast broker are not a file in a tools module. Follow that: a
`kingdom-browser` crate (native-only, never in the wasm bundle) with the
`browser_*` tool impls thin over it.

### 4. `ask_user_question`, and the King in the loop

This one is not a port; it is the most on-metaphor tool in the set and deserves
building properly. The court, mid-work, turns and asks: *there are two ways to
do this, which do you want?* The King answers, and the work continues. That is
the product's stance — sovereign reviewing proposals — happening inside a single
turn rather than only at its end.

Shape:

- The tool takes 1–4 questions, each with 2–4 options, optional multi-select,
  and accepts a typed free-text answer as well as a listed option. (Phoenix's
  schema; no reason to differ.)
- The call parks: the tool's future waits on a oneshot for the King's reply. The
  plan stays busy, and `working_on` reads as something like *"Waiting on the
  King"* — visibly different from *"Running cargo test"*, because "blocked on a
  human" is one of the three questions this product exists to answer.
- The question is pushed to the chamber (step 0), which renders it as an
  answerable card inline in the transcript, not a modal. The answer goes back
  over a server function and resolves the oneshot.
- The answer is recorded as the `Deed`'s outcome, and it is **not**
  re-answerable: a `Deed` whose outcome is set renders as settled history. A
  human answer is the one tool result that can never be re-derived, which is
  precisely why it must be written down rather than held in memory.

Edge cases that must be decided rather than discovered:

- The King never answers, and closes the tab. The parked call needs a timeout or
  an explicit cancel, and hitting it must clear the busy mark — a plan wedged
  forever waiting on an answer nobody will give is the same trap the detached
  task exists to prevent.
- The plan is finished (merged/archived) while a question is parked. The guard
  in `finish_plan` that refuses to merge under an in-flight draft covers this if
  the plan is still marked busy; confirm it does.

## The boundary

**The workspace is the boundary.** Every tool is rooted at
`plan.workspace.path`. A path that resolves outside it — after symlink
resolution and `..` normalisation — is refused, not clamped. No OS sandbox: the
git worktree plus path checks are the isolation, which is what worktrees were
cut for in the first place.

This is the one invariant worth a test of its own, and it belongs at the seam
(the tool context), not repeated in each tool — a check every tool has to
remember is a check the next tool will forget. Note the sharp edge: a
`WorkspaceMode` with no isolation points at the city itself, so "rooted at the
workspace" is a weaker guarantee there, and it should be, but it must be a
deliberate weaker guarantee rather than an accident.

`bash` is the hole in any path-based scheme — a shell can `cd` anywhere. Root
the process's cwd at the workspace and accept that a determined command escapes;
say so plainly in the module docs rather than implying a guarantee that isn't
there. Full containment is the `nono` sandbox, which the King explicitly chose
not to take on now.

## Where it shows in the UI

A transcript that renders a `Deed` as raw JSON would make the chamber
unreadable at exactly the moment it becomes interesting. `conversation.rs`
matches on `Entry` in one place (line ~378) and gains a third arm: a collapsed
line naming the tool and its outcome, expandable to the full input/output.
`NoteKind` gains nothing; a deed is not a note.

Styling goes in `style/`, alongside the existing note styling.

## Suggested order of work

This is large enough that it should land in reviewable pieces, even inside one
task:

1. **Push**: `/ws`, per-plan subscription, chamber subscribes, `poll_while`
   deleted, `draft_plan` becomes spawn-and-notify
2. `Entry::Did(Deed)` + the `said()`/deeds split + migration test
3. `Tool` trait, workspace-rooted context, the path-escape test, `think`
4. The loop: `Model` returns prose-or-calls; mock scenario; Copilot `tools` wire
5. `conversation.rs` renders a deed, live
6. `ask_user_question` — parked call, answerable card, oneshot resolution
7. `read_file`, `search`, `patch`
8. `bash` (the big one: handles, ring buffer, reaper, signals)
9. `tmux`
10. `kingdom-browser` crate + `browser_*`
11. `read_image`, `keyword_search`, `spawn_agents`, `skill`

Steps 1–6 are the spine. If the shape is wrong, it is wrong there, and every
step after it is cheap by comparison. Step 6 sits in the spine deliberately: it
is the one that proves push works in *both* directions, and a design that only
ever pushes server-to-browser will not survive meeting it later.

## Done when

- The King can decree "run the tests and fix what fails" and watch the court do
  it — each command appearing in the chamber as it happens over the socket, with
  no refetch and no spinner, and `working_on` naming the current step.
- The court can stop mid-work and ask the King a question; he answers in the
  chamber and the work continues in the same turn.
- The work lands in the plan's worktree, and merging it is the existing
  `finish_plan` path, unchanged.
- A tool call that tries to leave the workspace is refused, and there is a test
  saying so.
- `cargo test -p kingdom-core` and
  `cargo test -p kingdom-app --features ssr --no-default-features` pass, and
  `kingdom-core` still builds for wasm32.
- The mock provider can drive a full tool loop with no credential and no
  network.

## Risks worth naming up front

- **The turn loop, not the tools, is the hard part.** Tools are mostly portable
  code. The loop touches the busy-mark invariant, the detached-task reasoning,
  the transcript's meaning, and the polling chamber all at once.
- **Push is now inside this task, and it is not small.** Connection lifecycle,
  reconnect, replay-on-reconnect (a chamber that misses entries while the socket
  was down is worse than one that polls), and multiple tabs on one plan. This is
  the right call — `ask_user_question` is unimplementable without it — but it is
  a second hard problem sitting next to the turn loop, and step 1 should be
  finished and working on the *existing* prose-only draft path before the loop
  is built on top of it.
- **A parked question is a new way for a plan to be stuck.** `ask_user_question`
  introduces indefinite blocking on a human. Timeout and cancel are not polish.
- **`say()` is currently the only way words enter a plan.** Once the court can
  act, "what did this plan do" and "what did this plan say" stop being the same
  question, and the sidebar summary/title derivation may need rethinking.
