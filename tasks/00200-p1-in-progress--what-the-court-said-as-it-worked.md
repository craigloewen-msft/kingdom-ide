# What the court said as it worked

The court narrates. It writes "I'll check how the sidebar reads its title" in the
same reply as the `read_file` call, and that sentence is the most useful line in
the round — it is the *reason* for the deeds that follow. Kingdom already
captures it, already persists it, and already hands it back to the model.

It has never once shown it to the King.

```
21:04  ✓  search      narration                              ⌄
21:04  ✓  read_file   crates/kingdom-app/src/api.rs          ⌄
21:05  ✓  bash        cargo test -p kingdom-core             ⌄
```

That is what the chamber draws. What actually arrived from the provider was:

> I want to check two things before I touch anything: where the narration is
> recorded, and whether the store keeps it. Then I'll run the core tests to see
> what I'm working against.

…followed by those three calls. The model's statement of intent is on disk, in
`plans/<id>.json`, under `transcript[].Tool.narration`, and the only reader it
has ever had is `copilot.rs::messages()` putting it back on the wire.

This is a direct hit on the product's first question — *what is every agent doing
right now?* A column of tool names answers "what commands ran". The narration
answers "what it is trying to do", which is what the King is actually reading
for. Phoenix shows it (a `text` block rendered inline beside the `tool_use`
blocks of the same assistant message); Kingdom drops it on the floor at the view
layer and nowhere else.

**Nothing needs to be captured. Nothing needs to be stored. This is a rendering
gap, and it is the whole task.**

---

## What already exists, so this task does not rebuild it

| Piece | Where | State |
|---|---|---|
| `ToolCall::narration: Option<String>` | `kingdom-core/src/model.rs:1206` | exists, `#[serde(default)]` |
| Blank narration dropped rather than stored | `ToolCall::in_reply`, `model.rs:1320` | exists |
| Parsed off every choice, joined with `\n\n` | `copilot.rs:783-820` | exists |
| Recorded on the **first call of a batch** only | `api.rs:1208` | exists |
| Persisted (only `images` are stripped) | `store.rs::without_images` | exists |
| Pushed live over the watch socket | whole-plan publish | exists |
| **Drawn in the chamber** | `components/conversation.rs` | **nothing** |

The one architectural fact that shapes the fix: **narration belongs to a reply,
not to a call.** One reply that asks for six things produced one sentence, and
`api.rs` deliberately puts it on the first call only, so replaying it does not
teach the model it said the same thing six times. The view has to honour that
same grouping — one remark above the batch, not one above each deed.

---

## Piece 1 — the remark, drawn above the deeds it explains

`crates/kingdom-app/src/components/conversation.rs`, in `Transcript`.

The insertion point is the `<For>` body, **not** `ToolCallLine`. Three different
components render an `Entry::Tool` — `Question`, `Subagents`, `ToolCallLine` —
and a batch's first call can be any of them. Putting the remark inside
`ToolCallLine` would silently lose it exactly when the court explains *why it is
stopping to ask you something*, which is the one time the sentence matters most.

So each `Entry::Tool(d)` arm renders a fragment: the remark, where the call
carries one, then whichever view that entry already maps to.

```rust
/// What the court said in the reply that asked for this deed.
///
/// Carried on the first call of a batch and `None` on the rest, so a reply that
/// asked for six things draws one remark and not six. That grouping is not a
/// view decision being made here -- `api.rs` records it that way, for the same
/// reason the model is replayed it that way.
fn remark(tool_call: &ToolCall) -> Option<String> {
    tool_call
        .narration
        .as_deref()
        .map(str::trim)
        .filter(|said| !said.is_empty())
        .map(str::to_string)
}
```

Rendered through `Prose`, like every other piece of the court's writing:

```rust
view! {
    <div class="chat-remark">
        <Prose text=said class="remark-body"/>
    </div>
}
```

`Prose` and not a bare string, because this is model output with the same
markdown in it as everything else the court writes — a backticked path, a short
list, occasionally a fence. The escaping story is already settled there (raw HTML
is escaped, never passed through), so this inherits it rather than opening a
second door.

### Why not a `chat-msg` bubble

Tempting, and wrong. A `.chat-msg` carries a speaker column saying **Court** and
a timestamp, which presents the sentence as an utterance of its own — a separate
thing the court said, followed by some unrelated commands. It was not. It is the
preamble *of* those commands, and the King's reading of the log should make that
obvious without him reconstructing it.

This is the same distinction the file already draws twice, and for the same
reason: a note is not a `Speaker` because nothing uttered it, and a deed is not a
bubble because it is the court *working*. A remark is the third case — something
the court said, but as part of an action rather than instead of one.

So: no speaker column, no timestamp (the deed below carries one, to the second),
left-aligned and full width so it reads as the header of the block that follows
it. Quieter than a reply addressed to the King, louder than a deed line.

### Keying

`entry_version` needs nothing. It exists to distinguish an entry from a *later
version of itself*, and narration is written once when the call is recorded and
never touched again — unlike `outcome`, which is the entire reason that function
exists. Worth a sentence here because "add a case" is the obvious wrong instinct.

---

## Piece 2 — style

`style/components/_conversation.scss`, beside `.chat-deed`.

```scss
// What the court said in the reply that asked for the deeds below it. Not a
// bubble: nobody was addressed. It is the preamble of an action, so it is drawn
// as the header of the block it belongs to -- flush left, full width, and
// closer to the deed under it than to whatever came before.
.chat-remark { … }
```

The visual claim, in three properties: it sits tight against the deed below it
(small `margin-bottom`, ordinary `margin-top`), it reads at message size rather
than the deed line's 11px monospace, and it takes `$ink-dim` — dimmer than the
court's addressed prose, brighter than a deed line. A left rule (`border-left`)
is worth trying to bind it visually to the deeds beneath; if it fights the deed
borders, drop it.

---

## Piece 3 — the thinking, collapsed *(separable)*

`ToolCall::reasoning.text` is the same story one level down: captured
(`copilot.rs::parse_reasoning`), stored, replayed to the model, never shown. It
is not the same thing as narration and must not be drawn as though it were —
narration is what the court chose to say, reasoning is what it happened to think,
and the second is longer, rambling, and sometimes a provider's summary of itself.

So: **collapsed by default**, one faint line above the remark —

```
   ⌄ thinking (14 lines)
```

— expanding to the text on click. That is exactly Phoenix's `ThinkAside`, and it
is right for the same reason `ToolCallLine` is collapsed: a transcript that
renders every reasoning block in full is unreadable at precisely the moment it
gets interesting.

Listed separately because it is genuinely a separate decision. If the chamber
feels crowded with both, Piece 1 is the one that answers the complaint — strike
this and the task still stands.

---

## Piece 4 — make the state reachable in development

No fixture in `mockdata/` has a tool call at all, and `mock.rs` builds every
reply with `Acts::plain`, which hard-codes `narration: None`. So today the
Proving Grounds cannot show this even once, and nor could anyone reviewing the
change.

Give at least the `Work` and `Subagents` scenarios a narration — `Acts { calls,
reasoning: None, narration: Some(...) }` — phrased as the real thing is: a
sentence saying what it is about to do and why. `Subagents` earns it especially,
because that arm exercises the "first call of a batch is not a `ToolCallLine`"
path from Piece 1.

This is the rule §4 of AGENTS.md already pins for the placeholder court: states
the UI exists to show must be reachable during development, or the code that
draws them rots unseen.

---

## Tests

None of these need a browser.

**`kingdom-app`, `conversation.rs` tests** — the decision the view reads, pinned
as a function so it can be asserted without rendering:

- A call with no narration, and one whose narration is whitespace, both draw
  nothing. (The second is defence in depth: `in_reply` already filters it, but a
  record written by an older build can carry `"  "`.)
- A three-call batch draws exactly one remark — the first call carries it, the
  other two return `None`. This is the test that fails if someone later "tidies"
  `api.rs` into copying the narration onto every call.

**`kingdom-core`, `model.rs` tests** — `in_reply` drops a blank narration rather
than storing it, which is asserted in a doc comment today and nowhere else.

**`kingdom-app`, `store.rs`** — a plan whose tool call carries a narration
survives a save/load round trip with it intact. Cheap, and it guards the one
failure that would make this look broken rather than absent: a chamber that shows
the remark live and loses it on reload.

---

## Out of scope

- **Streaming the narration as it arrives.** It lands with the tool call, in one
  write. Token-by-token needs a streaming provider path Kingdom does not have.
- **The `think` tool.** It already renders as an ordinary deed line whose gist is
  the thought. Whether it deserves Phoenix's aside treatment is a follow-up to
  Piece 3, not this task.
- **Changing what the model is sent.** `copilot.rs::messages()` already replays
  narration as the assistant message's `content`, with `Null` rather than `""`
  when there was none. Nothing here touches the wire.

---

## How the King checks it

```bash
KINGDOM_REALM=kingdom-mirror KINGDOM_SANDBOX=1 cargo leptos watch
```

Issue a decree that lands on the `Work` scenario, and the chamber should read:

```
           I'll start by reading what is already there, since this is a Rust
           project of 214 files and I don't yet know its shape.
21:04  ✓  think    The decree is "…"                        ⌄
```

Then reload the page and confirm the sentence is still there.
