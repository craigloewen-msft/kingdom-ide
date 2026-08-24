# How full the window is

A small bar in the chamber header showing how much of the model's context
window this conversation is filling, and how big that window is.

The problem is one the King has no way to see today: a long conversation gets
quietly closer to the model's limit, and the first evidence of it is a gateway
refusal or a reply that has forgotten the start of the work. The header already
states the provenance facts - city, model, workspace - and this is another one
of exactly that kind.

```
  kingdom-mirror . claude-opus-5 . isolated . [###-------] 31% of 1000K
```

## The number is the provider's, not ours

The count comes from the `usage` block Copilot already returns with every
reply. It is not estimated from the transcript.

This matters because everything the bar is *for* happens outside the
transcript: the system prompt, the project's `AGENTS.md`, and the tool schemas
for a dozen tools are all sent on every turn and are easily tens of thousands
of tokens. A chars/4 estimate over the visible messages would read as
comfortable right up until the request was refused - which is the one moment
the bar exists for. It would also be a fabricated number the user then acts on,
and `copilot.rs` already refuses to do that elsewhere: a model whose window the
catalogue does not declare is dropped from the picker rather than listed with a
guess.

The cost of being honest is that the bar is absent until the court has answered
once, and absent forever under the offline mock, which reports no usage and
declares no window. Both are correct: there is nothing truthful to draw yet.

## Shape of the change

**`kingdom-core/src/model.rs`** - a small type and one field.

```rust
/// How much of a model's context window a conversation is filling.
///
/// Both numbers together, written at the same moment, so they cannot disagree:
/// a count against last week's window would be a percentage of nothing.
pub struct ContextUsage { pub tokens: usize, pub window: usize }
```

with `percent()` returning `Option<u8>` - `None` when the window is zero, so a
provider that declares no limit yields no bar rather than a division by zero -
and a free `window_label(tokens) -> String` (`"1000K"`) beside it. The label
function is shared with the model picker, which formats the same number inline
today; two spellings of `w / 1000` is exactly how the picker and the header
would come to disagree.

On `Plan`: `#[serde(default)] pub context: Option<ContextUsage>`. Defaulted, so
every plan record already on disk loads unchanged and simply shows no bar until
its next turn.

**`kingdom-app/src/llm/mod.rs`** - `take_turn` returns what the model said
*and* what it cost:

```rust
pub struct Answer { pub reply: Reply, pub tokens: Option<usize> }
```

A wrapper rather than a field on each `Reply` variant: the usage is true of the
turn, not of one of its two endings, and putting it on both variants means
every `match` in the loop has to carry it past. `Option` because a provider
that reports nothing must say so rather than claim zero.

Also `fn context_window(&self) -> usize { 0 }` on the `Model` trait - the
window is a fact about the model, and the model is the only thing that knows
it. Defaulting to zero means a provider that does not know its own limit
contributes no bar, which is the same refusal-to-guess as everywhere else.

**`llm/copilot.rs`** - read `usage` off the response, and carry
`context_window` on `CopilotModel`. `open()` already looks the model up in the
catalogue to learn `can_act` and `can_see`; the window is read from that same
entry, one line away, and for the same reason - it is read live rather than off
the plan, so a conversation opened last week is measured against the window the
model has today.

For the count itself, prefer `usage.total_tokens` and fall back to
`prompt_tokens`. Prompt tokens alone are what *was* sent and understate the
window by one reply; the total is the closest honest reading of how full the
window stands at the end of that turn.

**`llm/mock.rs`** - returns `Answer { reply, tokens: None }`. It has no window
and invents no usage.

**`api.rs::converse`** - one `update` immediately after `take_turn` returns,
setting `p.context` when both numbers are known, before the reply is matched.

Deliberately its own write rather than folded into the tool-call recording
below it: that path runs once per *act*, and a round with three tool calls
would write the same usage three times while a round that only speaks would
miss it entirely. One write per round also means `events::publish` pushes it,
so the bar climbs in the header while a long tool loop is still running -
which is when the King most wants to see it moving.

**`components/conversation.rs`** - a span in `chamber-meta`, after
`chamber-workspace`, read through `live` rather than the construction snapshot
(the same trap the `live` doc comment on `ConversationBody` already warns
about: a snapshot renders once and then lies). Shown only when there is a
percent to show. A hairline track, a fill, and one mono label; the full
`31,000 of 1,000,000 tokens` goes on `title`, as the workspace path does.

**`style/components/_conversation.scss`** - `.chamber-context` beside
`.chamber-workspace`. Faint track, `$gold-dim` fill, same 11px mono as its
neighbours. Constant colour: this is provenance, not an alarm, and a header
that changes colour on its own would pull attention from the log, which is
where the work is.

## Tests

Two, and no more.

1. **`copilot.rs`: usage is read off a reply body.** The `/chat/completions`
   shape is not ours and cannot fail at compile time - the same reason the
   catalogue-parsing tests next to it exist. Pins that a reply with no `usage`
   block yields `None` rather than zero, which is the difference between "no
   bar" and "a bar claiming an empty window".

2. **`model.rs`: `percent()` against a zero window is `None`.** The mock
   declares a window of zero and is the default model with no credential, so
   this is a real reachable state, and the alternative is a panic in the view.

No test for the header markup: it renders a number this task does not compute,
and a test asserting the span exists would restate the view.

## Not in scope

- Doing anything about a full window - compaction, warning, or refusing to
  send. This task is the instrument, not the response to it.
- Showing the window before the first reply. It would mean the conversation
  fetching the model catalogue for a number that arrives on its own moments
  later.
- The rail and the map. They do not refetch on a plan's own updates today
  (AGENTS.md section 4, "Live updates beyond a plan's own chamber"), so a bar
  there would sit stale - a wrong number being worse than no number.
