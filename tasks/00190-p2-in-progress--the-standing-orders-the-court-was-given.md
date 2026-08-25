# The standing orders the court was given

The King can read every deed and every word in a chamber, but not the one text
that shaped all of them: the system prompt. When a plan behaves oddly — reaches
for the wrong tool, ignores the project's `AGENTS.md`, acts as though it were
still only proposing — the first question is *what was it actually told?* Today
that is answerable only by reading `system_prompt.rs` and simulating the
assembly in your head.

Give the chamber header a control that reveals the full assembled prompt for
that plan.

## Shape

**Server: one new `#[server]` function in `api.rs`.**

```rust
pub async fn plan_briefing(plan: String) -> Result<String, ServerFnError>
```

It reads the plan and its city under `lock()`, builds `CityBrief::from_city`
exactly as `draft_plan` does, and returns
`SystemPrompt::assemble(&city_brief, &plan.workspace, plan.permissions,
plan.approved_proposal().is_some(), &kingdom_root).render()`.

The important property: it must go through the *same* `assemble` + `render` the
turn loop uses, with the plan's *current* permissions and approval. A second
rendering path would drift from the real one the first time either is touched,
and a viewer that shows a prompt the model never got is worse than no viewer.
It is fetched on demand rather than stored on the plan — the prompt is derived
from the plan, the city on disk and the guidance files, and freezing a copy into
`plans/<id>.json` would both bloat every write and go stale.

Caveat worth stating in the doc comment: this is the prompt *as it would be
assembled now*. A plan approved since its last turn will show the wider remit,
which is the honest answer to "what will it be told next round" and not
necessarily what round 1 read.

**UI: a header button beside the spyglass toggle.**

In `components/conversation.rs`, in `chamber-header`, next to `spyglass-toggle`:

- a `<button class="orders-toggle">` with a scroll glyph (`\u{1F4DC}`) and
  `title="Read the standing orders this plan was given"`.
- clicking toggles a local `RwSignal<bool>`. On first open, a `Action` fetches
  `plan_briefing(id)`; the result is kept in a signal so re-opening does not
  refetch. A refresh happens on `plan_id` change (a new chamber is a new prompt).
- while open, an overlay panel inside `chamber-frame` renders the text in a
  `<pre class="orders-text">`. Plain `<pre>`, deliberately **not** `Prose`: this
  is the literal bytes sent to the model, and rendering its markdown would hide
  exactly the structure (the `<project_guidance>` tags, the block order) that a
  reader opens it to check.
- Escape and a click on the backdrop close it. Loading and error states say so
  in the panel rather than in `state.error`, which belongs to the composer.

Presentation copy stays in the metaphor ("The standing orders", "what the court
was told before it was asked anything"); the code says `briefing` /
`system prompt`.

**Style: `style/main.scss`.** `.orders-toggle` matches `.spyglass-toggle`
exactly (same size, same open state). `.orders-panel` is an overlay over the
chamber column — not a third resizable column: the spyglass already owns that
space, and two panels fighting for it is a layout problem for a thing the King
reads once and dismisses. Monospace, scrollable, `white-space: pre-wrap`.

## Tests

- `kingdom-app --features ssr`: `plan_briefing` on a seeded plan returns a
  non-empty string containing the base prompt's opening and the plan's workspace
  path; a plan under `Full` and one under `Propose` differ in the remit block.
- an unknown plan id is an error, not an empty string.

## Not in scope

- Showing the tool *specs* alongside the prompt. `ToolSpec::for_model` is the
  other half of what the model sees and is a reasonable follow-up, but it needs
  the model handle (`crate::llm::open`) and therefore a credential, which turns
  a read into a network call. Left out on purpose.
- Editing the prompt from here. This is a window, not a lever.
