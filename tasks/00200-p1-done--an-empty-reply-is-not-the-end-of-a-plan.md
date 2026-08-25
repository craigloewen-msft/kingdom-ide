# An empty reply is not the end of a plan

`Copilot returned an empty reply.` killed plan-15 three times in ninety seconds
and nothing the King did brought it back. The message names a symptom, the plan
lands in `Failed`, and every retry fails identically. This task fixes the
unrecoverability first, the misdiagnosis second, and the blindness that made the
cause unknowable third.

## What actually happened

From `~/.kingdom/kingdoms/dev-d5348dc5/plans/plan-15.json`:

| | |
|---|---|
| status | `Failed`, summary `Copilot returned an empty reply.` |
| model | `copilot/claude-opus-5`, effort `high` |
| context | **104,915 / 1,000,000 tokens** |
| `Failed` notes | three, at `…815838`, `…861807`, `…875744` |
| last entries | `think` (29 KB) + two `bash`, batch `…-21`, all settled `Done` |

The King said `Keep going`, then `Keep going!`. Both failed the same way, the
second within 14 seconds.

**It is not context exhaustion.** The window was 10% full. Task 00130's
diagnosis does not apply here, and neither does its fix — this is a different
failure wearing the same clothes.

## Why nothing he did could work

This half is provable from the code and is the reason the bug feels malicious.

`settle(plan_id, Err(e))` records the failure as `NoteKind::Failed`. A `Note` is
deliberately **not** a `Turn` — `Plan::turns()` filters it out, and that
exclusion is load-bearing and correct (Kingdom's plumbing must not be replayed
as the model's prior words).

The consequence nobody costed: **a failed turn leaves no trace in what the model
is sent.** So when the King says `Keep going`, `say` → `draft_plan` → `converse`
rebuilds the brief from `plan.turns()` and sends *the identical payload*, plus
ten characters. Same request, same model, same result.

```mermaid
flowchart LR
  A["Keep going"] --> B["say → draft_plan → converse"]
  B --> C["brief = plan.turns()"]
  C --> D["identical payload"]
  D --> E["empty reply"]
  E --> F["Failed + Note"]
  F -.->|"Note is not a Turn"| C
```

The loop is closed. `Failed` is not `is_settled()`, so the composer stays live
and the plan *presents as retryable while being permanently dead* — the exact
shape task 00130 called "the worst shape a failure can take here", arrived at by
a different road.

And there is **no retry anywhere**: `converse` does `Err(e) => return
settle(plan_id, Err(e))` on the first error. An empty reply — the textbook
transient, where the same request resampled usually succeeds — is treated as
fatal on the first occurrence.

## Why we cannot say which empty reply it was

`copilot.rs` contains **no logging at all** — no `tracing`, no `eprintln`. The
response body is parsed and dropped. When `answer_from` returns this error the
bytes that caused it are gone forever.

That is itself the defect to fix. What follows is not a guess at the one true
cause; it is a list of the paths that all funnel into the same message, each of
which is independently wrong.

### `answer_from` reports "empty" for things that are not empty

**Silently dropped tool calls.** `parse_acts` is a `filter_map` with two `?`:

```rust
let name = call["function"]["name"].as_str()?;   // dropped, silently
id: call["id"].as_str()?.to_string(),            // dropped, silently
```

A reply that asked for three tools in a shape we misread yields `calls == []`,
`text == ""`, and the message "empty reply". The module's own doc says a
malformed call must be kept so the model can be told what it sent — that promise
is kept for bad *arguments* and broken for a bad *envelope*.

**Content as an array of parts.** `message["content"].as_str().unwrap_or_default()`
yields `""` when Copilot returns `content` as `[{"type":"text","text":"…"}]`.
The module already reads three spellings for `can_see` and several for
reasoning, *because the catalogue is not ours*. Content gets one spelling.

**Reasoning is parsed, then thrown away.** A reply carrying `reasoning_content`
and nothing else is a reasoning model spending a round thinking — a real thing
Opus at high effort does. `parse_reasoning` reads it successfully and the empty
branch discards it, reporting silence.

## The change

### 1. An empty reply is retried, not fatal

In `converse`, a small bounded retry (2 attempts, brief backoff) around
`take_turn` for **transient** classes only. `ModelError` gains the distinction —
an empty reply and a 5xx are transient; a missing credential, a 400 and a
content filter are not, and must still fail immediately.

Raced against `halt` like the existing awaits, so Stop still lands promptly.

This alone would have saved plan-15, and it is the smallest change that breaks
the closed loop above.

### 2. A retry that reaches the model differently

If the retries are exhausted, the plan must not re-send a byte-identical payload
forever. The honest minimum: record the failure as something `turns()` *does*
yield, so the model sees `the previous reply came back empty` and is not asked
the same question into the same silence.

This is the one design decision worth flagging: it means a Kingdom-authored
turn, which `Turn`'s doc warns against in the user's voice. It belongs as an
assistant-side or system-side note, never as `Speaker::User`.

### 3. Never call a reply empty when it was not

- `parse_acts` returns what it dropped and why; `answer_from` reports
  `the model asked for N tools whose shape could not be read` and names them.
  Never "empty".
- Read `content` as string **or** array of parts, one small helper.
- A reasoning-only reply is reported as such, distinctly.

### 4. Log the body when the reply cannot be read

A truncated `tracing::warn!` of the response when `answer_from` fails. This bug
cost a full investigation that ended in "unknowable"; the next one should cost a
log line. Bounded, and never at info level — it carries conversation content.

## Tests

1. **A reply whose tool calls lack an `id` is not reported as empty.** The
   silent-drop path, pinned.
2. **`content` as an array of parts is read as prose**, not as silence.
3. **A transient failure is retried and the plan survives**, against a mock
   provider failing once then succeeding — the regression that closes the loop.
4. **A non-transient failure is not retried.** A bad credential must fail fast,
   not three times slowly.

No test for the retry's timing; asserting a sleep restates the constant.

## Not in scope

- **Task 00130's `ContextExhausted` status.** Genuinely different failure, and
  this plan's 10%-full window proves they must not be conflated. The
  classification work here is the seam 00130 will want.
- **Compaction.** Not this bug.
- **The Responses API migration.** Named in the module docs as the real fix for
  the wire shape; reading two content spellings is the cheap version and does
  not foreclose it.

## Verification

- `cargo test -p kingdom-core`
- `cargo test -p kingdom-app --features ssr --no-default-features`
