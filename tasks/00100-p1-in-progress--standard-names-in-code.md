# Standard names in the code, the metaphor in the UI

The kingdom metaphor is the product's voice and it stays — in UI copy, in CSS
class names, on the map. But it currently also runs through the *identifiers*,
and that is a tax on every reader who has to learn that `Deed` means tool call
and `Remit` means permissions before they can read a match arm.

This task draws the line: **the metaphor is presentation, not domain.** Type
names, function names, variables, module names and doc comments use the standard
word for the thing. What the King reads on screen does not change at all.

## The line, precisely

| Layer | Vocabulary | Changes here? |
|---|---|---|
| Rust identifiers, module names, doc comments | standard | **yes** |
| UI copy — every string literal the King reads | metaphor | no |
| CSS class names and `style/` | metaphor | no |
| `Kingdom` / `City` / `Plan` | metaphor, and standard enough | no |

`Kingdom`, `City` and `Plan` are deliberately exempt. They are the crate names,
the URL routes and the `.kingdom/` directory on disk, and unlike `Deed` they are
ordinary English for what they hold — a folder, a project, a unit of proposed
work. Renaming them would be a much larger change for much less clarity.

## Rename table

This is the whole of it. Anything not listed keeps its name.

### `kingdom-core::model`

| Now | Becomes |
|---|---|
| `Speaker::King` | `Speaker::User` |
| `Speaker::Court` | `Speaker::Assistant` |
| `Deed` | `ToolCall` |
| `DeedOutcome` | `ToolOutcome` |
| `DeedImage` | `ToolImage` |
| `Entry::Did(Deed)` | `Entry::Tool(ToolCall)` |
| `Entry::Said(Utterance)` | `Entry::Message(Message)` |
| `Turn::Did` / `Turn::Said` | `Turn::Tool` / `Turn::Message` |
| `Utterance` | `Message` |
| `Plan::begin_deed` | `Plan::begin_tool_call` |
| `Plan::settle_deed` | `Plan::settle_tool_call` |
| `Deed::begun` | `ToolCall::started` |
| `Errand` (struct) | `SpawnedBy` |
| `Errand.deed` | `SpawnedBy.tool_call` |
| `Plan.errand_for` | `Plan.spawned_by` |
| `Plan::is_errand` | `Plan::is_subagent` |
| `Plan::sent` | `Plan::spawned` |
| `Kingdom::errands_of` | `Kingdom::subagents_of` |
| `Kingdom::absorb` | `Kingdom::insert` |
| `slug_for_decree` | `slug_for_prompt` |
| `Ward` | `Language` |
| `Building` | `SourceFile` |
| `District` | `Folder` |
| `Building.ward` / `.bulk` | `SourceFile.language` / `.bytes` |

### `kingdom-app`

| Now | Becomes |
|---|---|
| `tools::Remit` | `tools::Permissions` |
| `Remit::Survey` | `Permissions::ReadOnly` |
| `Remit::Full` | `Permissions::Full` |
| `tools::Workshop` | `tools::Sandbox` |
| `Workshop::for_deed` / `::deed` | `Sandbox::for_tool_call` / `::tool_call` |
| `herald.rs` | `events.rs` |
| `herald::proclaim` | `events::publish` |
| `herald::listen` | `events::subscribe` |
| `spyglass.rs` (server) | `screencast.rs` |
| `components/spyglass.rs` | `components/browser_view.rs` |
| component `Spyglass` | component `BrowserView` |
| `Sight` enum | `ConnectionState` |
| `components/decree.rs` | `components/prompt_bar.rs` |
| component `DecreeBar` | component `PromptBar` |
| `tools::spawn_agents` internals: `errand` | `subagent` |
| local `royal` (conversation.rs) | `is_user` |

### `kingdom-core::mockdata` and `sample`

| Now | Becomes |
|---|---|
| `RealmSpec` | `FixtureSpec` |
| `realms()` / `realm()` / `realm_names()` | `fixtures()` / `fixture()` / `fixture_names()` |
| `DEFAULT_REALM` | `DEFAULT_FIXTURE` |
| `CourtFn` | `StarterPlansFn` |
| `RealmSpec::court(..)` builder | `FixtureSpec::starter_plans(..)` |
| `mockdata/court.rs` | `mockdata/starter_plans.rs` |
| `court::default_court` | `starter_plans::default_plans` |
| `sample::populate_court` | `sample::starter_plans` |
| `api::open_court` | `api::seed_starter_plans` |
| `mock::realm_path` | `mock::fixture_path` |

The fixture *names themselves* (`kingdom-mirror`, and whatever else `realms.rs`
defines) are CLI arguments the user types — they do not change. Neither does the
"Proving Grounds" label in the UI, nor the `EnterProvingGrounds` server-fn wire
name, which the browser calls by that string.

### Doc comments

Every `///` and `//!` gets the same treatment: "the court" → "the model" or
"the agent", "a deed" → "a tool call", "the chamber" → "the conversation view",
"the King" → "the user", "proclaiming" → "publishing". This is most of the diff
by line count and most of the value — the prose is where a new reader actually
learns the vocabulary.

One exception: `layout.rs`, `terrain.rs` and `skyline.rs` keep their vocabulary
(`Realm`, `Road`, `CityPlacement`, `Lot`, `Plate`, `settle_kingdom`). Those
modules compute the geometry of a map that literally draws a kingdom with cities
and roads on it. There the metaphor is the subject matter, not a euphemism for
something else, and `Road` is already the clearest name for a line between two
cities. `Ward` is the exception within the exception: it classifies a file by
language and is used far outside the map, so it becomes `Language`.

## The on-disk format

`Speaker`, `Entry` and `ToolOutcome` are serialised into
`<root>/.kingdom/plans/*.json`, so this rename changes the on-disk shape.
Per the decision on this task: **no compatibility shims and no version gate.**
Everyone is assumed current.

Concretely:

- Delete `FORMAT_VERSION`, the `KingdomRecord` struct, and the `kingdom.json`
  write in `save_all`. Verified: `kingdom.json` is *written and never read* by
  anything in the tree, so it is dead weight rather than a format gate.
- No `#[serde(rename)]` anywhere. The JSON gets the new names.
- `load()` is already failure-tolerant — a record it cannot parse is skipped and
  the kingdom opens with what is left. A stale plan file therefore costs that
  plan, not a crash. That is the accepted blast radius.
- Update the `.kingdom/` doc block at the top of `store.rs` to drop the
  `kingdom.json` line.

## Tests

No new tests. Three existing ones embed literal old JSON and need their literals
updated to the new tags (`"Did"` → `"Tool"`, `"King"` → `"User"`, and so on):

- `store.rs::a_plan_recorded_before_images_existed_still_loads`
- `model.rs::a_plan_recorded_before_the_court_had_hands_still_loads`
- `model.rs::a_plan_recorded_before_errands_existed_still_loads`

They keep earning their place: they pin that *additive* fields stay optional,
which is still true and still worth catching. Rename the test functions to match
the new vocabulary but do not weaken what they assert.

The test in `sample.rs` pinning that the starter plans include a failed plan and
one mid-draft stays exactly as it is — see AGENTS.md §4.

## The friction this deliberately accepts

CSS stays metaphorical, so `conversation.rs` will contain `class="deed-mark"` a
few lines from a variable named `tool_call`, and `components/browser_view.rs`
will render `class="spyglass"`. That is the price of the chosen line, and it is
the right price: the class names are a presentational namespace shared with
`style/`, and churning both to satisfy symmetry would double the diff for no
reader's benefit.

## AGENTS.md

§2 currently says *"Use this vocabulary in type names, function names, UI copy,
and commit messages."* That instruction is what would quietly undo this work, so
it is part of the task, not a follow-up.

Rewrite §2 to keep the metaphor table and the sovereign-stance paragraph — both
still true and still load-bearing — and replace the vocabulary instruction with
the rule this task establishes, plus a short translation table (`ToolCall` is
shown as a deed, `Speaker::Assistant` is shown as the Court, and so on) so the
mapping is stated once rather than rediscovered.

Also update the two places elsewhere in AGENTS.md that name a renamed file: the
`herald.rs` and `spyglass.rs` lines in the §3 tree.

## Order of work

One commit per step, so a bisect lands somewhere readable:

1. `kingdom-core` model types and their doc comments.
2. `kingdom-core` mockdata/sample.
3. `kingdom-app` tools (`Permissions`, `Sandbox`) — largest single hop.
4. `kingdom-app` server modules (`events.rs`, `screencast.rs`, `store.rs`,
   `api.rs`), including deleting `kingdom.json`.
5. `kingdom-app` components, identifiers only, CSS strings untouched.
6. AGENTS.md.

## Done when

- `cargo test -p kingdom-core` and
  `cargo test -p kingdom-app --features ssr --no-default-features` pass.
- `cargo leptos build` succeeds — the wasm half compiles too, which is the check
  that `kingdom-core` stayed wasm-clean.
- Grepping `crates/` for
  `court|errand|deed|decree|chamber|remit|workshop|proclaim|spyglass` outside a
  string literal returns nothing.
- The UI is visually and textually identical: the chamber still says "Court",
  still says "The court sent an errand", still says "Raising the spyglass…".
