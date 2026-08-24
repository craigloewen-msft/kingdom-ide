# A court that remembers why it looked

Fix the reasons `claude-opus-5` reads dozens of files without converging.

## What was observed

Five real plans in `~/dev/.kingdom/plans/` (`plan-1` … `plan-5`), all
`copilot/claude-opus-5`, `effort: high`, all `Failed`. Across them: **117 tool
calls** (73 `bash`, 34 `read_file`, 10 `search`), **zero** `propose_plan`, zero
successful outcomes. Three died as `Stopped after 24 rounds without an answer`,
two as `Copilot returned an empty reply.`

The exploration is not random — it is *repetitive*. `plan-4` reads
`conversation.rs` five times at overlapping offsets and `api.rs` four times.
`plan-5` runs seven near-identical greps for the same constant. That is the
signature of an agent that has lost the thread of its own plan, not one that is
learning.

## Root causes

### 1. Reasoning is never round-tripped (the main cause)

`crates/kingdom-app/src/llm/copilot.rs` contains no mention of
`reasoning_content` — not in `parse_acts`, not in `messages`. Kingdom *sends*
`reasoning_effort: high` (`request_body`, line ~373) but discards every thinking
block that comes back.

`messages()` (line 403) reconstructs each past assistant turn as:

```json
{ "role": "assistant", "content": null, "tool_calls": [ … ] }
```

The model's own plan for the investigation — the part of its output that said
*why* it was reading that file and what it intended to do next — is dropped on
every round. So on round N it sees N tool results and no record of its own
intent, and re-derives a strategy from raw output. High effort makes this worse,
not better: the more it thinks, the more is thrown away.

This is also why it re-reads files: nothing in the context says "I already read
this and here is what I concluded."

**Change:** in `messages()`, carry the reasoning back. Preserve
`reasoning_content` (and any opaque `signature`/`encrypted_content` the gateway
returns) on the assistant message alongside `tool_calls`, so a thinking model
sees its own prior reasoning. This needs a domain field: `ToolCall` (or a new
`Turn::Acts` grouping) must carry the reasoning that accompanied the call.

Two secondary bugs surface in the same function and should be fixed with it:

- **Parallel tool calls are split into separate assistant turns.** The loop
  emits one assistant message *per* `ToolCall`. When the model returns three
  calls in one message, the replay presents them as three sequential decisions.
  Group consecutive `Turn::Tool` entries from one reply into a single assistant
  message with a `tool_calls` array, then their `tool` results.
- **Prose alongside tool calls is discarded.** `take_turn` (line 550) returns
  `Reply::Acts` and drops `message["content"]` when both are present. That
  narration is exactly the "here is my plan" text worth keeping.

### 2. `max_tokens: 4096` is why two plans died with "empty reply"

`request_body`, `copilot.rs:370`. Reasoning tokens are billed against the
completion budget. At `effort: high`, opus-5 can spend the entire 4096 thinking
and return empty content — which `take_turn:557` converts into
`ModelError::Refused("Copilot returned an empty reply.")` and `converse` turns
into a **failed plan**. That is `plan-1` and `plan-3`, and it also silently caps
how long a `propose_plan` body can be.

**Change:** derive the budget from the model, rather than picking a bigger
constant. A fixed number is the same bug at a different threshold: it is wrong
for a small model and wasteful for a large one, and it goes stale as the
catalogue moves.

The path already exists and carries two precedents to follow. `option()`
(`copilot.rs:229`) already reads `capabilities.limits.max_context_window_tokens`
into `ModelOption::context_window`, and `open()` (`copilot.rs:151`) already
reaches into the cached catalogue to pull `can_act` and `can_see` off the entry
rather than trusting what the plan recorded. Output budget is the same kind of
fact and travels the same road:

- `kingdom-core/src/model.rs:1511` — add `max_output_tokens: Option<usize>` to
  `ModelOption`, `#[serde(default)]` so a record written before this field
  degrades rather than fails to load (as `can_act` and `can_see` do).
- `copilot.rs::option()` — read `capabilities.limits.max_output_tokens`. Copilot
  has moved this sort of key before, which is why `can_see` reads three
  spellings; read the plausible alternatives (`max_output_tokens`,
  `max_completion_tokens`) rather than one.
- `copilot.rs::open()` — pull it off the catalogue entry beside `can_act`, and
  store it on `CopilotModel`.
- `request_body` — emit `max_tokens` from that value.

**When it is absent.** `context_window` treats a missing value as grounds to
drop the model entirely, on the reasoning that a guess is something the user
acts on. That is too strong here: the user never sees this number, and dropping
a usable model over it would be a worse outcome than a generous default. So
`None` means "fall back", and the fallback should be well clear of what a high
effort turn spends thinking — not the present 4096. Say so in the doc comment,
because the asymmetry with `context_window` is deliberate and looks like an
inconsistency otherwise.

Separately, an empty reply *with* a `finish_reason` of `length` should be
reported as "ran out of output budget", not as a bare refusal — the current
message sends the user hunting in the wrong place. Worth keeping even once the
budget is derived: it is the diagnostic that would have made this bug obvious.

A test is worth one here: a catalogue entry declaring a limit produces a request
carrying it, and one declaring none still produces a workable request. That is
the wiring, and it is the part that silently regresses.

### 3. `bash` refuses a call for a missing `op`, burning a whole round

`crates/kingdom-app/src/tools/bash.rs:164`. Five of 73 `bash` calls were refused
with ``no `op` was given``. The model then re-sent the identical command with
`"op": "run"` added. Each is a wasted round and a wasted turn.

`op` is declared required, but there is one obviously correct default and the
other three ops all require a `handle` the model must already hold.

**Change:** default `op` to `Run` when absent, and drop it from the schema's
`required`. Keep the refusal for an *unrecognised* op.

### 4. Nothing bounds what a tool result costs, on any round

`ToolCall::report` (`kingdom-core/src/model.rs:981`) returns the full output,
and `messages()` sends all of it, every round. Nothing truncates anywhere:
`bash::start` even calls `job.report(settled, usize::MAX)`.

`plan-4` accumulated 250 KB of tool output — roughly 62k tokens — resent in
full on every one of its 24 rounds. One `read_file` of
`kingdom-core/src/model.rs` alone returned 93 KB, because `read_file`'s default
`limit` is 2000 lines and nothing warns that a whole-file read of a large file
is a poor idea.

This interacts badly with commit `e98d166`, which raised `MOST_ROUNDS` from 24
to **500**. The three plans that stopped at 24 would now run to 500, at
quadratically growing cost per round. The cap raise made the bill from this bug
~20x worse; it should not ship without a bound on context.

**Change:**

- Cap what a single tool result contributes to the *replayed* context (head and
  tail, with an elision marker naming how much was dropped). The transcript on
  disk keeps the full text — this bounds only what goes on the wire.
- Have `read_file` say so in its result when it returned a very large file, and
  nudge toward `offset`/`limit` or `search`.

### 5. The system prompt licenses unbounded exploration

`system_prompt.rs`, `PROPOSE` (line 185): *"Read, search, and run whatever you
need in order to understand the work."* Nothing anywhere tells the model that
context is finite, that re-reading is waste, or that a good proposal can be
written from partial knowledge with its assumptions stated. `CityBrief::render`
also lists 40 notable paths, which reads as a checklist.

**Change:** add a short economy directive to `PROPOSE` — prefer `search` to
locate before `read_file` to read, read windows rather than whole files, do not
re-read what is already in the transcript, and propose once you know enough,
naming what you did not check as an assumption rather than exploring until
certain.

## Order

1 and 2 are the ones that turn failures into successes; 3 is a two-line win; 4
is urgent given the 500-round cap; 5 is cheap and helps regardless.

## Verification

- `cargo test -p kingdom-core`
- `cargo test -p kingdom-app --features ssr --no-default-features`
- A test on `messages()` pinning that reasoning survives a replay and that
  parallel calls from one reply become one assistant message — that is the
  regression which caused this, and it is invisible at the type level.
- A test that `bash` with no `op` runs rather than refuses.
- Manually: re-run the `plan-5` prompt ("raise the acting cap to 500") against
  `claude-opus-5` and confirm it reaches `propose_plan` in a handful of rounds.
  It is a one-constant change and previously took 24 rounds without finding it.

## Note

`tasks/00081-p2-done--raise-round-cap-500.md` is marked done and is in the code
(`MOST_ROUNDS = 500`), but the captured transcripts all stopped at 24 — they
predate the change. Re-running them today would produce a much larger bill, not
a better outcome, until 1–4 land.
