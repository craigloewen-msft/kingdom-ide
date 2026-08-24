# Context exhaustion is a state, not a failure

Two changes merged twenty minutes apart. `e98d166` raised `MOST_ROUNDS` from 24
to 500. `6f82e4e` bounded one replayed tool result at `MOST_REPLAYED` = 12 KB.
Neither is wrong. What they expose is that Kingdom has **one number standing in
for two unrelated jobs** — stopping a runaway loop, and staying inside a model's
context window — and only the first of those jobs is actually being done.

`~/dev/phoenix-ide` has solved this, and its answer is not a better-tuned cap.
It is to stop conflating the two, and to make running out of window a *resumable
state* rather than an error.

## What Phoenix does, and why it is right

### The loop guard resets, is deliberately huge, and is not a budget

`executor.rs:1631`:

```rust
/// Default cap on consecutive LLM requests within a single parent-conversation
/// user turn. Distinct from sub-agent `max_turns`: this resets on every
/// `Event::UserMessage`, so a long conversation is never penalised — only a
/// runaway `tool_use` burst within one turn.
///
/// Set deliberately high — this is a backup safety-net, not a budget.
const DEFAULT_PARENT_TOOL_CYCLE_CAP: u32 = 1000;
```

Overridable by `PHOENIX_PARENT_TOOL_CYCLE_CAP`, `0` disables it, and
`parent_tool_cycle_count = 0` is reset on `Event::UserMessage` and on any
authoritative event. When it does fire, the message says the counter resets on
every user turn and names the env var.

**Kingdom already has this shape and does not know it.** `converse` is called
fresh from `draft_plan` on every `say`, so `for round in 0..cap` is *already*
per-user-turn and already resets. `MOST_ROUNDS` is a runaway guard and nothing
else. 500 is fine — arguably it should be higher and env-overridable.

### The window is not guarded. The refusal is classified.

Phoenix makes no attempt to predict the window or refuse pre-emptively. It lets
the provider refuse, and classifies that specific refusal:
`context_length_exceeded` / `max_tokens` become `ErrorKind::ContextWindowExceeded`,
which is marked `NoAutoRetry` and `NotResumable` — with the comment *"Routed to
`ConvState::ContextExhausted`, never a parent `ConvState::Error`, so no resume
affordance is owed here."*

The state machine then intercepts it *before* generic error handling
(`transition.rs`, spec rule `BackendRejectsContextExhausted`):

```rust
let summary = "Context limit reached before the turn could complete. \
    Continue to compact and resume, or start a new conversation.";
ParentState::ContextExhausted { summary }
    .with_effect(Effect::persist_continuation_message(&summary))
    .with_effect(Effect::PersistState)
    .with_effect(Effect::NotifyContextExhausted { summary })
```

### The recovery is a continuation, and the work survives

`ContextExhausted` is a first-class `ConvState` with its own UI surface. The work
actions bar is *hidden* for it and the surface reserved for continuation actions,
because "placing cleanup beside its handoff controls makes an unrelated
destructive action appear to be part of continuation." REQ-BED-031 preserves the
record, the worktree and the branch, and permits terminal actions only when there
is no continuation.

**This is the whole answer.** No threshold to tune, no fraction-of-window
constant to go stale, no dependence on `usage` being accurate. The provider is
the authority on its own window, and the only thing Kingdom has to get right is
what happens when it says no.

## What is wrong in Kingdom today

Overflow surfaces as `ModelError::Refused("Copilot returned 400: ...")` — the
same variant as a bad credential or a content filter. `settle` records it as
`PlanStatus::Failed`. `Failed` is not `is_settled()`, so the composer stays live
and the user can say something — but the transcript that overflowed is the thing
resent, and nothing compacts or windows it (`Plan::turns()` yields everything).
Every retry fails identically.

The plan is permanently dead while presenting as retryable. That is the worst
shape a failure can take here: the King spends his attention — the scarce
resource — on a plan that cannot recover, with nothing telling him so. At 24
rounds this was unreachable. At 500 it is ordinary.

## Shape of the change

### 0. What task 00120 already built

00120 landed on main while this was being written, and it supplies most of the
plumbing this task assumed it would have to add:

- `Model::context_window() -> usize` on the trait, read live from the catalogue
  entry `open()` already fetches. Zero means undeclared.
- `Answer { reply, tokens }` — `take_turn` reports what the turn cost.
- `Plan::context: Option<ContextUsage>`, persisted, with `percent()` saturating
  at 100.
- `converse` already writes it once per round, in a write of its own.

So step 1 below is the only place a number still has to be found, and steps 2
and 3 are unchanged.

One detail worth keeping deliberately: `converse` updates `p.context` *after*
`take_turn` returns `Ok`, so a turn refused for length never records a reading.
An exhausted plan therefore keeps the last **successful** measurement in its
header — perhaps 87%, not 100%. That is honest rather than untidy: the request
that overflowed was larger than anything ever counted, and writing 100% would be
inventing the one number this design refuses to invent. The status says what
happened; the bar says what was last measured.

### 1. Classify the refusal

`ModelError` gains a variant for it, and `copilot.rs` reads the provider's error
`code` rather than only its prose — `context_length_exceeded` and the sibling
spellings, as `parse_reasoning` already reads several spellings for the same
reason: the catalogue is not ours. The existing `finish_reason == "length"`
branch is the *output* budget and stays distinct; these are different problems
with different fixes and `6f82e4e` was right to separate them.

### 2. Make it a status, not a failure

`PlanStatus::ContextExhausted`, carrying the summary. Distinct from `Failed`
because the two need different affordances: a failed plan should be retried, and
an exhausted one must not be — retrying is precisely what cannot work. This is
the domain change the task rests on, and it is the one Phoenix's
`UserResumePolicy::NotResumable` encodes.

The chamber shows the summary and offers to carry on in a fresh plan, rather
than a live composer that will fail on use.

### 3. Carry the work on

A new plan on the same city and **the same workspace**, opened with a summary of
what the exhausted one was doing. The worktree and branch are untouched, exactly
as REQ-BED-031 requires — the work is on disk and must survive.

How the summary is written is the one open question worth deciding rather than
guessing. The cheapest honest version is the plan's own `summary` plus its
standing proposal; having a model write it is task 00070's territory and can
follow.

### 4. Say what `MOST_ROUNDS` is for

It is a runaway guard, it resets every user turn, and it is deliberately not a
token budget. Its doc says none of that today, which is how it came to be read
as one. Borrow Phoenix's phrasing — "a safety-net, not a budget" — and consider
an env override for the same reason Phoenix has one.

This dissolves the review note's cross-reference problem: once the two caps are
unrelated, `replayed()`'s doc has no reason to name `MOST_ROUNDS` at all, and
cannot go stale when it moves.

## Also worth doing, and cheap

**Images are unbounded on the wire.** `replayed()` bounds the text half of a tool
result and leaves the larger half alone: `shown()` images go back as base64 data
URLs in full, every round, with `read_image` capping a single image at 5 MB. They
are stripped from *disk* but not from the *wire*, so an identical conversation
sends a wildly different payload depending on whether the server restarted.
Bounding this makes exhaustion rarer; it does not replace handling it.

**Reasoning is persisted into a document rewritten on every update.** `store.rs`
strips images with an explicit argument — the file is rewritten on every update
and would grow by a megabyte per screenshot. `ToolCall::reasoning` now serialises
into that same file, and `update()` writes on every tool-call begin *and* settle.
Persisting it is *correct* — unlike an image it must be replayed, and dropping it
reintroduces exactly the bug `6f82e4e` fixed — but the cost was never weighed
against 500 rounds, and `store.rs` is where that weighing belongs.

## Not in scope

- **Compaction.** Phoenix's summary text offers "compact and resume", and that is
  a real feature with real questions (what may be dropped, what must never be).
  Continuation into a fresh plan is the smaller honest version and does not
  foreclose it.
- **Pre-emptive refusal against a token threshold.** An earlier draft proposed
  this, and the reason given was that it would mean estimating tokens. 00120
  removes that objection — `p.context` is now a *measured* count against a
  declared window, so a threshold could be checked honestly.

  It stays out of scope on a better argument. The last reading is the cost of
  the turn that *finished*; refusing on it means predicting the size of the
  round that has not happened yet, whose increment is whatever the next tool
  results weigh. Set the threshold low and conversations die that the provider
  would have accepted; set it high and it never fires before the refusal does.
  The provider's "no" is ground truth and costs one round-trip to obtain — and
  after step 2 that round-trip lands somewhere useful instead of somewhere
  fatal. Pre-empting is an optimisation on top of classification, never a
  substitute for it: a single enormous tool result can overflow a fresh
  conversation with no prior reading that could have caught it.
- **Lowering `MOST_ROUNDS`.** 500 was a deliberate call in task 00081 and real
  work needs the rope.

## Tests

Two.

1. **A context-length refusal is classified, not swallowed into a generic
   `Refused`.** The `/chat/completions` error shape is not ours and cannot fail
   at compile time — the same reason the catalogue-parsing tests beside it exist.
2. **An exhausted plan does not present as retryable.** This is the whole
   behavioural difference from today and the one thing a reader cannot check by
   eye.

No test for the chamber's markup: it renders a state this task sets, and
asserting the span exists would restate the view.

## Verification

- `cargo test -p kingdom-core`
- `cargo test -p kingdom-app --features ssr --no-default-features`
