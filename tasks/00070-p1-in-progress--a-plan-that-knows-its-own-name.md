# A plan that knows its own name

Every plan should get a **short, human name** the moment it opens — shown as its
row in the rail and its chamber title, and used as the git branch it works on.

## Why

Today a plan's identity is two unrelated accidents:

- **Title**: `Plan::opened` takes the King's decree verbatim, then `api::settle`
  overwrites it with whatever markdown heading the drafting model happened to
  lead with (`llm/copilot.rs::headline`). So the rail label changes under him
  after the first reply, and is at the mercy of the model's formatting.
- **Branch**: `worktree.rs::prepare` cuts `kingdom/<uuid>`. Unreadable in
  `git branch`, unreadable in `git log`, and tells the King nothing about which
  of his eight in-flight plans he is looking at.

Both are the same missing thing: a plan has no name of its own. Naming is
cheap to do well — a two-line prompt to a small model — and it pays off in the
exact place this product claims to be about: knowing at a glance what many
agents are doing.

## Shape

### 1. A plan carries its name

`kingdom-core/src/model.rs`:

```rust
pub struct Plan {
    /// A short, stable name: the rail label and the chamber title.
    pub title: String,
    /// The same name, as a git-safe slug. The branch is cut from this.
    #[serde(default)]
    pub slug: String,
    ...
}
```

`#[serde(default)]` so plan records already on disk still load.

And a pure, wasm-safe `kingdom_core::naming` module:

- `slugify(&str) -> String` — lowercase, ASCII words joined by `-`, capped at
  ~32 chars on a word boundary, and hardened against git's ref rules (no
  leading/trailing `-` or `.`, no `..`, no `@{`, never empty).
- `fallback_name(prompt: &str) -> String` — first clause of the decree,
  trimmed to a handful of words. What we use when no model can be reached.

Core, not app, because both the wasm UI and the server want to reason about it,
and it is pure maths over a string — the same category as `layout.rs`.

### 2. A cheap model names it

New `crates/kingdom-app/src/llm/naming.rs`:

```rust
pub struct PlanName { pub title: String, pub slug: String }

/// Names a plan from its decree. Never fails: any refusal, timeout or
/// nonsense reply falls back to `naming::fallback_name`.
pub async fn name_plan(prompt: &str, city: &str) -> PlanName;
```

- Prompt: *"Give this task a name of at most five words. Reply with the name
  alone — no punctuation, no quotes, no explanation."* plus the decree and the
  city name. Reuses the existing `Model` trait via a new
  `Model::name(&self, ...)` default method, or — cheaper to read — a plain
  `oneshot(&self, system, user)` on the trait that naming and drafting both use.
  Decide during implementation; the trait already has exactly one method, so
  adding a second general one is the smaller change.
- **Which model**: a `naming_model_id()` in `llm/catalogue.rs` — `KINGDOM_NAMING_MODEL`
  wins if the catalogue serves it, else the first id in a small preference list
  of cheap models (`copilot/gpt-5.4-mini`, `copilot/gpt-4o-mini`,
  `copilot/claude-haiku-4.5`, `copilot/gemini-3.6-flash`) that the catalogue
  actually offers, else the mock. Explicitly *not* the drafting model: naming
  with Opus is a waste and the King already chose that model for the thinking,
  not the labelling.
- Effort: `None` — never asked to think hard.
- Budget: a hard ~4s timeout. A slow naming call must not delay the King
  landing in the chamber; it degrades to the fallback name.
- The **mock provider** implements naming deterministically (keywords from the
  prompt), so offline and the proving grounds still produce sensible names and
  the tests need no network.

### 3. The name is used

- `api::begin_plan` calls `name_plan` *before* `worktree::prepare`, and passes
  the slug in so the branch is `kingdom/<slug>` (falling back to
  `kingdom/<slug>-<n>` on collision — git tells us, and there is already an
  error path to hang that off).
- `worktree::prepare` takes the slug alongside the `WorkspaceMode`. The
  worktree *directory* stays the uuid — it is disposable and never read by a
  human; the branch is the part the King sees. `BRANCH_PREFIX` still applies,
  so `archive`'s "only prune branches we made" guard is untouched.
- `api::settle` **stops overwriting `plan.title`**. It keeps setting `summary`
  from the draft. This is the behaviour change the King will feel: the rail
  label he sees at second one is the label at minute ten, and it matches the
  branch name. `llm/copilot.rs::headline` loses its caller and goes with it;
  `Draft.title` likewise (`Draft` becomes `{ summary, body }`).
- The chamber header shows the branch next to the title where the workspace
  path already is, so "chat name" and "branch name" are visibly the same thing.

## What this deliberately does not do

- No renaming UI. The King cannot yet edit a plan's name — a rename would have
  to decide whether the branch follows, and that is a real question worth its
  own decree rather than a guess.
- Existing plans on disk keep their current title and get an empty slug; the
  rail is unchanged for them.

## Tests

Minimal, three:

1. `kingdom-core`: `slugify` produces a valid git ref from hostile input
   (unicode, punctuation runs, leading dashes, a 400-char decree, an
   all-emoji decree) — this is the one that turns into a failed `git worktree
   add` in front of a user if it is wrong.
2. `kingdom-app`: `name_plan` against the mock yields a stable name, and
   against a model that errors yields the fallback rather than an error.
3. `kingdom-app`: `settle` leaves `plan.title` alone — the regression this
   whole task exists to pin.

## Docs

`AGENTS.md` §4 gains a line: plans are named at open by a cheap model, and the
branch is cut from that name.
