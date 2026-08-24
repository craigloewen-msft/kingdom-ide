# Branches named, not numbered

A plan's branch should read as what the plan *is* — `kingdom/tidy-the-sidebar` —
rather than `kingdom/9f0dcf47-6d65-4dc7-9dcf-e25b28295c42`.

## Why this is still broken

Task 00070 ("A plan that knows its own name") was approved and committed, but
the commit `9f0dcf4` changed **one file**: the task document itself. No code
landed. There is no `Plan::slug`, no `kingdom_core::naming`, no
`llm/naming.rs`, and `worktree.rs::prepare` still does:

```rust
let id = uuid::Uuid::new_v4().to_string();
let branch = format!("{BRANCH_PREFIX}{id}");
```

So the branch is a GUID because nothing has ever made it anything else.

## Scope: the branch half only

00070 bundles two things: *deriving a name* (a cheap-model call, a preference
list of models, a timeout, a mock implementation) and *using that name for the
branch*. Only the second is what the King is looking at in `git branch`.

And the first is not actually required to fix it. `Plan::opened` already sets a
readable title via `model.rs::title_from_prompt` — the first clause of the
decree. Slugified, that is already a far better branch name than a UUID, with
no network call, no timeout budget and no new provider surface.

So: do the slug now, from the title we already have. Leave the model-naming
half of 00070 open — a better *title* later automatically becomes a better
*branch*, because the seam is the slug.

## Shape

### 1. `kingdom-core/src/naming.rs` — new, pure, wasm-safe

```rust
/// A git-safe ref component derived from human text.
pub fn slugify(text: &str) -> String;
```

Lowercase; ASCII alphanumeric runs joined by `-`; non-ASCII dropped; capped at
~32 chars on a word boundary. Hardened against git's ref rules (`git
check-ref-format`): never empty, no leading/trailing `-` or `.`, no `..`, no
`@{`, no `.lock` suffix. Empty-after-cleaning falls back to `"plan"`.

Core rather than app because it is pure maths over a string — the same category
as `layout.rs` — and the wasm UI will want it once there is a rename affordance.

### 2. `Plan` carries the slug

`model.rs`:

```rust
pub struct Plan {
    pub title: String,
    /// The title as a git-safe slug. The branch is cut from this.
    #[serde(default)]
    pub slug: String,
    ...
}
```

`#[serde(default)]` so the plan records already under `.kingdom/plans/` still
load. `Plan::opened` sets `slug: naming::slugify(&title)` right where it already
computes the title, so the two cannot drift.

### 3. `prepare` takes the slug

```rust
pub async fn prepare(
    city_root: &Path,
    mode: &WorkspaceMode,
    slug: &str,
) -> Result<Workspace, WorktreeError>
```

`WorkspaceMode::Fresh` cuts `kingdom/<slug>`. On collision — two plans from
similar decrees is the *common* case, not the exotic one — retry
`kingdom/<slug>-2`, `-3`, up to a small bound, then fall back to the uuid so a
decree can never be refused merely for being named like an earlier one.

The worktree **directory** stays the uuid: it is disposable, nested under
`<city>/.kingdom/`, and never read by a human. The branch is the part the King
sees. `BRANCH_PREFIX` still applies, so `archive`'s "only prune branches we
made" guard is untouched.

`api::begin_plan` computes the plan's title/slug before calling `prepare` and
passes it through. That is a small reordering: today it builds `Plan::opened`
*after* `prepare`.

## Tests

Two, both pinning things that become a user-visible failure if wrong:

1. `kingdom-core`: `slugify` yields a valid git ref from hostile input — a
   400-char decree, punctuation runs, leading/trailing dashes, an all-emoji
   decree. This is the one that otherwise surfaces as a failed `git worktree
   add` in front of the King.
2. `kingdom-app`: two plans with the same decree in one city both get a
   workspace, on distinct branches, neither of them a bare uuid. The collision
   path is the part most likely to be quietly wrong.

The existing `prepare` tests gain a slug argument; no new assertions needed
there.

## Docs

`AGENTS.md` §4: note that a plan's branch is cut from its title, and that
naming it with a model is still outstanding as 00070.
