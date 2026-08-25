# How long a deed is taking, and how long it will wait

A deed on the chamber's line says *what* the court is doing and pulses
"working..." while it does it. It does not say how long it has been doing it,
and it never says how long it is prepared to wait. Both are the same question
the King is actually asking -- **is this progressing, or is it stuck?** -- and
today he answers it by watching the pulse and guessing.

This is squarely the product's first question. A five-minute `cargo build` and a
wedged `browser_wait_for_selector` look identical on the line, and only one of
them is worth interrupting.

Two things to show on the collapsed line:

- **Elapsed.** Ticking while the call is in flight, frozen at the final figure
  once it settles.
- **The wait budget**, where the call has one: `bash` with `wait_seconds`,
  `browser_*` with `timeout`, `tmux_run` with `readiness.timeout_seconds`.

## The one thing the domain cannot answer today

`ToolCall` records `at` when the call begins (`ToolCall::started`) and nothing
when it ends -- `Plan::settle_tool_call` sets only `outcome`. So a *settled*
call's duration is not derivable from a plan document at all, and a reload would
lose every figure the user had been watching. That is the change this rests on.

```rust
pub struct ToolCall {
    // ...
    /// When the result came back. See [`Timestamp`].
    ///
    /// `None` while in flight, and also `None` on a call settled by
    /// `store::reconcile` -- a server that died mid-call genuinely does not
    /// know when the work stopped, and a `now()` written on load would
    /// report the length of the *outage* as the length of the command.
    #[serde(default)]
    pub settled_at: Option<Timestamp>,

    /// How long this call said it would wait, if it said anything.
    #[serde(default)]
    pub waits: Option<WaitBudget>,
}
```

Both `#[serde(default)]`, so every plan document already on disk still loads and
simply renders without a figure -- the same treatment `batch` and `reasoning`
got.

`settled_at` is set inside `settle_tool_call`, not by its callers: that is the
one place a call ends, and a stamp each caller had to remember is one the next
caller will forget.

## Why the wait budget is a type, not a number

The tools mean two genuinely different things by "wait", and flattening them
into one number would put a lie on the line:

| Tool | Budget | What elapsing means |
|---|---|---|
| `bash` (`op=run`, `op=wait`) | `wait_seconds`, default 30 | **Nothing is killed.** The call returns a handle and the command runs on |
| `browser_navigate`, `_eval`, `_take_screenshot`, `_resize` | `timeout`, default 15s | The call fails |
| `browser_wait_for_selector`, `_click`, `_type` | `timeout`, default 30s | The call fails |
| `tmux_run` | `readiness.timeout_seconds`, default 30, only when `readiness` is given | Returns anyway, reporting the text was not seen |
| `ask_user_question` | `PATIENCE`, 30 min | The question expires |
| everything else | none | -- |

So:

```rust
/// How long a tool call is prepared to wait, and what elapsing means.
pub enum WaitBudget {
    /// The call gives up at this point: the deed fails.
    Deadline { seconds: u64 },
    /// The call stops *watching* at this point, but the work continues and can
    /// be returned to. `bash` is the whole reason this variant exists -- see
    /// the module docs there.
    Patience { seconds: u64 },
}
```

Rendered accordingly: `0:42 / 15s` for a deadline, `0:42 of 30s watched` for
patience. A King reading "1:12 / 15s" against a browser call knows something is
wrong; reading the same figure against a `bash` handle he would not.

## Where the budget is read

A new method on the `Tool` trait, defaulting to `None`:

```rust
/// How long this call will wait, read from the arguments the model sent.
fn waits_for(&self, _input: &Value) -> Option<WaitBudget> { None }
```

This matters more than it looks. Every one of those defaults -- 15s, 30s, 30 --
is a constant inside the tool that parses it, and the *only* way the line stays
honest as those change is for the answer to come from the tool itself. A table
in the view would be a second copy of the tool surface, wrong the first time
anybody edited a default and silently wrong from then on.

`api.rs` reads it once, where the call is recorded (beside the existing
`describe(&act.tool, &act.input)`), from the same `act.input` the deed is
written with -- so what the line claims and what the tool will actually do
cannot disagree. `browser_profile` returns `None`: its waits are per-step inside
`run_scenario`, and no single number would be true.

## What the chamber shows

In `ToolCallLine`, one `<span class="deed-took">` between the gist and the
chevron:

- **Settled**, with both stamps: `1:04`. Under a second, `0.4s` -- a `read_file`
  reporting `0:00` reads as broken.
- **Settled**, missing a stamp (an old document, or a reconciled orphan):
  nothing. Silence is the honest rendering of "not known", exactly as `clock()`
  already does for a missing `at`.
- **In flight**: ticks, in place of today's `working...` word rather than beside
  it -- the ticking figure *is* the statement that it is working, and says more.
  With a budget: `0:12 / 30s`. The pulse animation stays.
- **Past a `Deadline` budget**: the figure turns `$failed`. A call past its own
  timeout that has still not returned is the strongest "look at me" the chamber
  can give. Never for `Patience` -- a `bash` handle past `wait_seconds` is
  behaving exactly as designed.

The ticking needs a clock, and there is precedent for the mechanism in
`map/mod.rs::set_interval_stepper`. Two things to get right:

- **One interval for the whole chamber, not one per deed.** A busy turn has
  dozens of settled deeds, and dozens of timers each waking to re-render an
  unchanging string is a cost that grows with the transcript. A single
  `Signal<Timestamp>` in the `Conversation` scope, ticking every second, read
  only by the deeds actually in flight.
- **It must stop.** Cleared on cleanup, and not started at all while the plan
  has nothing in flight -- an idle chamber left open overnight should not wake
  the browser once a second forever.

Browser-only, like `clock()`: under SSR the span renders empty. Nothing is lost,
because the whole app is gated behind an open kingdom and only becomes real on
the client.

## Scope

`ToolCallLine` only. `Subagents` and `Question` render tool calls their own way
and are deliberately left alone: an errand's row already shows its plan's live
status, and putting "29:31 remaining" under a question the King is currently
reading would rush him through a decision the product exists to let him take
slowly.

## Files

| File | Change |
|---|---|
| `crates/kingdom-core/src/model.rs` | `settled_at` and `waits` on `ToolCall`; `WaitBudget`; the stamp inside `settle_tool_call`; an `elapsed()` accessor so the view does no arithmetic |
| `crates/kingdom-app/src/tools/mod.rs` | `Tool::waits_for`, defaulting to `None` |
| `crates/kingdom-app/src/tools/{bash,browser,tmux,ask_user_question}.rs` | implement it, from the same constants each already parses |
| `crates/kingdom-app/src/api.rs` | read the budget onto the call as it is recorded |
| `crates/kingdom-app/src/store.rs` | reconciled orphans keep `settled_at: None` |
| `crates/kingdom-app/src/components/conversation.rs` | the chamber's tick; render elapsed and budget on the deed line |
| `style/components/_conversation.scss` | `.deed-took`, tabular numerals, the overdue colour |

## Tests

- `kingdom-core`: `settle_tool_call` stamps `settled_at`, and `elapsed()` is
  `None` when either stamp is missing.
- `kingdom-core`: a plan document written before this change still deserialises,
  with both fields `None`.
- `kingdom-app`: `bash` reports `Patience` and `browser_click` a `Deadline` --
  the distinction the whole rendering rests on, and one a later edit could
  silently invert.
- `kingdom-app`: `waits_for` reads the model's own `wait_seconds`/`timeout` when
  given, and each tool's default when not.
- `kingdom-app`: the elapsed formatter -- sub-second, minutes, and the missing
  stamp that renders as nothing.
