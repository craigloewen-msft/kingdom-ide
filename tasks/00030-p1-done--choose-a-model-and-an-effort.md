# Choose a model and an effort, per plan

Today every decree is drafted by whatever `KINGDOM_MODEL_PROVIDER` and
`KINGDOM_MODEL` happen to say, which means changing model is an edit to a dotfile
and a server restart. The King cannot pick a cheap model for a throwaway question
and Opus for a design review, and he certainly cannot do it mid-conversation.

This task makes **which model, at what effort** a first-class, visible choice on
the plan itself — a plan is the session, so the choice belongs on it — with the
catalogue read live from Copilot after `agency` mints a credential, and the last
used pair remembered in the browser as the starting default for the next decree.

It serves the product's first question — *what is every agent doing right now?* —
by making the answer include **what it is thinking with**. A plan drafted by
`gpt-5.4-mini` at `low` and one drafted by `claude-opus-5` at `max` are not the
same kind of proposal, and the rail should not render them identically.

---

## What the King sees

The dock's handle row gains a **model chip** beside the existing provider badge:

```
[ Start a new task   → kingdom-ide ]  [ claude-opus-5 · high ▾ ]  [ copilot ✓ ]
```

Clicking it opens a picker panel above the input:

- **Recommended first** — a short vendor-grouped list (Opus, Sonnet, a GPT-5.x, a
  Gemini Flash), plus **`mock`** so the offline model is reachable without
  editing env at all.
- **`Show all N models`** expands the rest, grouped by vendor, each row showing
  its context window.
- An **effort row** underneath — `low · medium · high · xhigh · max` — showing
  only the levels *that model declares*, and hidden entirely for models that
  declare none. Choosing nothing means "the model's own default", which is a
  distinct state from any explicit level and is sent as no field at all.

The chip reflects **the plan under discussion** when one is live, and the
remembered default when starting fresh. Changing it mid-conversation applies from
the next turn onward — the plan records the change, so the rail stays honest
about what drew what.

---

## Design

### 1. `kingdom-core` — the vocabulary (pure, wasm-safe)

```rust
pub enum ModelEffort { None, Minimal, Low, Medium, High, Xhigh, Max }
// wire_name() -> "none" | "minimal" | … ; label() for the UI; ALL for ordering

/// What a decree is drafted with. `effort: None` = the model's own default.
pub struct ModelChoice { pub model: String, pub effort: Option<ModelEffort> }

/// One selectable model, as the picker renders it.
pub struct ModelOption {
    pub id: String,        // "copilot/claude-opus-5" | "mock"
    pub label: String,     // "Claude Opus 5"
    pub vendor: String,    // "Anthropic" | "OpenAI" | … | "Offline"
    pub context_window: usize,
    pub recommended: bool,
    pub efforts: Vec<ModelEffort>,   // empty = no effort control
}

/// The picker's data, plus why it might be short.
pub struct ModelCatalogue {
    pub options: Vec<ModelOption>,
    pub default_id: String,
    pub credential: CredentialState,
    pub detail: String,
}

impl ModelCatalogue {
    /// Resolves a remembered choice against what is actually available. A stale
    /// id, or an effort the model does not declare, degrades to the nearest
    /// valid thing rather than erroring — last week's localStorage must never
    /// be able to wedge today's dock.
    pub fn resolve(&self, wanted: Option<&ModelChoice>) -> ModelChoice;
}
```

The id is namespaced (`copilot/…`, `mock`) so the **provider is derivable from
the choice**. That is what lets the mock be a picker entry rather than an env
var, and it removes the current split-brain where provider and model are set
separately and can disagree.

`Plan` gains `effort: Option<ModelEffort>` beside its existing `model: String`
(which becomes the namespaced id). Both are already rendered — the rail's
`.plan-model` span and the map pip's tooltip — so they pick up the effort for
free once the field exists.

### 2. `kingdom-app/src/llm/catalogue.rs` — the live catalogue (ssr only)

GET `https://api.githubcopilot.com/models` with the same bearer credential and
the same two mandatory headers the chat call already sends
(`Copilot-Integration-Id`, `Editor-Version`). The credential comes from the
existing `credential::resolve(DEFAULT_COPILOT_HELPER)`, so **`agency auth github`
runs automatically and its token is reused for its TTL** — no new auth path, no
second place for the helper contract to drift.

From each entry, keep only what the picker needs:

| From `/models` | Used for |
|---|---|
| `capabilities.type == "chat"` | filter — drops embeddings and `trajectory-compaction` |
| `supported_endpoints` contains `/chat/completions` | filter — we only speak chat completions today; absent means chat-only (older models) |
| `capabilities.limits.max_context_window_tokens` | context window; **absent ⇒ drop the model**, never guess |
| `capabilities.supports.reasoning_effort` | the effort levels offered; absent ⇒ no effort control |
| `name`, `id` | label and api name |

Vendor is derived from the id prefix (`claude`→Anthropic, `gemini`→Google,
`grok`→xAI, `mai-`→Microsoft, `gpt-`/`o1`/`o3`→OpenAI). A small `RECOMMENDED`
set and a `SKIP` set of legacy noise (`gpt-3.5*`, `gpt-4`, `gpt-4o-2024-05-13`)
mirror what has already proven right next door in
`~/dev/phoenix-ide/scripts/phoenix-copilot-env.py`.

Cached in a `Mutex<Option<(Vec<ModelOption>, Instant)>>` with a ~10 minute TTL:
opening the picker must not cost an HTTP round trip every time, and the catalogue
changes on the order of weeks. When the fetch fails, the catalogue degrades to
**mock + the configured default** with `credential`/`detail` explaining why — the
dock stays usable and says what broke, rather than presenting an empty list.

### 3. `copilot.rs` — sending the choice

`CopilotModel::new(token, api_name, effort)`. `reasoning_effort` is serialised
**only when an explicit level was chosen and that model declares it**. Sending an
unsupported effort earns an opaque 400 from the gateway, and omitting the field
is the model's native default — genuinely different requests, so the type keeps
them different (`Option<ModelEffort>` with `skip_serializing_if`).

### 4. `api.rs` — server functions

- `list_models() -> ModelCatalogue` — new.
- `open_plan(prompt, city, choice: Option<ModelChoice>)` — resolves the choice
  against the catalogue, records the resolved pair on the `Plan`, drafts with it.
- `continue_plan(plan, prompt, choice: Option<ModelChoice>)` — `None` keeps the
  plan's existing pair, so a plain follow-up never silently switches model;
  `Some` updates the plan and applies from this turn on.

`crate::llm::configured()` becomes `configured(&ModelChoice)`, reading the
provider off the id rather than the environment. `KINGDOM_MODEL_PROVIDER` /
`KINGDOM_MODEL` survive as the *default the picker opens on*, so a fresh clone
still drafts offline with no setup.

### 5. UI — `components/chat.rs` + `KingdomState`

Two new signals on `KingdomState` (`chosen_model`, `chosen_effort`), restored
from `localStorage` in a hydrate-only `Effect` and written on change — exactly
the pattern `sidebar.rs` already uses for its dragged width, including the reason
(reading storage during render would make SSR markup disagree with hydration).
Keys `kingdom.model` and `kingdom.effort`. The stored value goes through
`ModelCatalogue::resolve`, so a model withdrawn from the catalogue yesterday
quietly becomes the default today.

Styling goes in `style/components/_chat-dock.scss` beside the existing
`.model-badge` / `.model-setup` rules: `.model-chip`, `.model-picker`,
`.model-group`, `.effort-row`. Quiet by default, consistent with the badge's
"loud only when broken" stance.

---

## Tests

Three, each pinning something a user would notice breaking:

1. **Catalogue parsing** (`llm::catalogue`) — a fixture of the real `/models`
   response shape yields the expected options: a non-chat entry is dropped, a
   model declaring no `reasoning_effort` reports no efforts, and one missing a
   context window is dropped rather than defaulted. This is a wire shape we do
   not control and cannot catch at compile time.
2. **Effort is never fabricated** (`llm::copilot`) — building a request for a
   level the chosen model does not declare omits `reasoning_effort` entirely;
   an explicit supported level is sent verbatim. Pins the difference between
   "native default" and an explicit level, otherwise invisible until a 400.
3. **A stale remembered choice degrades** (`kingdom-core`, `resolve`) — an id no
   longer in the catalogue falls back to the default, and an unsupported effort
   falls back to the model's default, rather than either erroring.

No test for "the picker lists models" or for the new accessors — those restate
the implementation.

---

## Out of scope

- **The Responses API.** Several newer models are Responses-only; they are
  filtered out of the catalogue until that route exists, rather than listed and
  broken.
- **Per-city or per-kingdom defaults.** The plan is the session; one remembered
  starting default is enough until it demonstrably is not.
- **Streaming**, and any change to *when* drafting happens. Still gated on the
  WebSocket work, which remains the next most valuable thing after this.

## Docs to update

- `.kingdom.env.example` — `KINGDOM_MODEL_PROVIDER` / `KINGDOM_MODEL` are now
  the *opening default*, not the only lever.
- `AGENTS.md` §5 — model choice and effort join the "real today" list.
