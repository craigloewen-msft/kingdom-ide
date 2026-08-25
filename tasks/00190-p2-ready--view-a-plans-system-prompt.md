# View a plan's system prompt from the conversation header

Everything the model was told before it was asked anything is currently
invisible in the UI. When a plan misbehaves — picks the wrong tool, ignores the
project's `AGENTS.md`, acts as though it were still under `Permissions::Propose`
— the first diagnostic question is what the prompt actually contained, and the
only way to answer it today is to read `llm/system_prompt.rs` and simulate the
assembly by hand.

Add a control to the conversation header that reveals the fully assembled system
prompt for that plan.

## Server

One new `#[server]` function in `api.rs`:

```rust
pub async fn plan_briefing(plan: String) -> Result<String, ServerFnError>
```

Under `lock()`, it reads the plan and its city, builds `CityBrief::from_city`
the same way `draft_plan` does, and returns:

```rust
SystemPrompt::assemble(
    &city_brief,
    &plan.workspace,
    plan.permissions,
    plan.approved_proposal().is_some(),
    &kingdom_root,
).render()
```

It must go through the same `assemble` + `render` the turn loop in `converse`
uses. A second rendering path would drift from the real one the first time
either is touched, and a viewer showing a prompt the model never received is
worse than no viewer.

Derived on demand, not stored on the plan: the prompt is a function of the plan,
the city on disk and the guidance files found on the way up. Freezing a copy
into `plans/<id>.json` would grow every write and go stale.

Doc comment should state the caveat: this renders the prompt *as it would be
assembled now*. A plan approved since its last turn shows the widened remit,
which answers "what will the next round be told" rather than "what did round 1
read".

## UI

In `components/conversation.rs`, in the `chamber-header`, beside
`spyglass-toggle`:

- `<button class="orders-toggle">` with a scroll glyph (`\u{1F4DC}`) and a
  `title` attribute.
- Click toggles a local `RwSignal<bool>`. First open runs an `Action` that
  fetches `plan_briefing(id)`; the result is held in a signal so reopening does
  not refetch. Invalidate when `plan_id` changes.
- While open, an overlay panel inside `chamber-frame` renders the text in a
  plain `<pre class="orders-text">` — deliberately **not** `Prose`. This is the
  literal text sent to the model, and rendering its markdown would hide the
  structure (`<project_guidance>` tags, block order) a reader opens it to check.
- Escape and a backdrop click close it. Loading and error states render inside
  the panel, not via `state.error`, which belongs to the composer.

Per AGENTS.md §2, the metaphor appears only in the strings the user reads and in
the CSS class names; the server function, signals and locals use standard terms
(`briefing`, `system prompt`).

## Style

In `style/main.scss`: `.orders-toggle` matches `.spyglass-toggle` (same size and
open state). `.orders-panel` overlays the chamber column rather than becoming a
third resizable column — the spyglass already owns that space, and this is read
once and dismissed. Monospace, scrollable, `white-space: pre-wrap`.

## Tests

`cargo test -p kingdom-app --features ssr --no-default-features`:

- `plan_briefing` on a seeded plan returns a non-empty string containing the
  base prompt's opening line and the plan's workspace path.
- A plan under `Permissions::Full` and one under `Permissions::Propose` differ in
  the permissions block.
- An unknown plan id returns an error, not an empty string.

## Out of scope

- Showing the tool specs alongside the prompt. `ToolSpec::for_model` is the other
  half of what the model sees, but it needs a model handle via `crate::llm::open`
  and therefore a credential, turning a local read into a network call.
- Editing the prompt from this panel. Read-only.
