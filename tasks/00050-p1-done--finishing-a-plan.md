# Finishing a plan: merge, archive, and a kingdom that remembers

Today a plan can be started but never *finished*. `PlanStatus` has `Approved`
and `Rejected`, and nothing in the running product can produce either — they are
reachable only from `sample.rs`. That is the same disease `Blocked` had before it
was deleted: an unreachable state is a trap for whoever matches on it next.

This task gives the King the two endings that actually exist, makes the worktree
disposal decision `worktree.rs` deliberately deferred, and — because none of it
is worth anything if a restart forgets it — gives Kingdom a place to keep state.

Three parts, in dependency order:

1. **Storage.** JSON documents under the kingdom root. Everything else needs it.
2. **Merge to main.** Land the work, dispose of the worktree, settle the plan.
   Any refusal from git stops the whole thing and is shown verbatim.
3. **Archive.** "This didn't work out." Preserve the work so it can come back,
   then reclaim the checkout.

---

## Part 1 — Where state lives

### The recommendation: JSON documents under the kingdom root

```
<kingdom_root>/.kingdom/
  kingdom.json              format version, when it was last opened
  plans/<plan-id>.json      one document per plan — the whole Plan, serde-derived
  archive/<plan-id>.patch   git format-patch output for archived plans
```

Not in memory, not SQLite, not inside each project repo. The reasoning matters
more than the choice, so:

**Why persist at all, and why now.** In-memory was defensible while a plan was
just a conversation — losing chat costs a re-ask. It stops being defensible the
moment a plan owns a *worktree with commits in it*. Restart the server today and
every `.kingdom/<uuid>/` checkout in every city is orphaned: real work on disk
with nothing left that knows what it was for or which plan to merge it from.
That is data loss, not inconvenience. Merge and archive both consume state that
must have survived the restart (`workspace.base`, the branch name), so they
cannot land on top of an in-memory store.

**Why the kingdom root and not the project repos.** Two reasons, one practical
and one about whose repository it is. Practically, the rail and the map read
*all* plans at once; sharding them across N repos means walking N repos on every
load, and a plan is lost entirely when its city is renamed. More importantly,
the King's repository is not ours to write to — `worktree.rs` already made
exactly this call, choosing `.git/info/exclude` over `.gitignore` so Kingdom's
scratch folder never shows up in his diffs. Putting our bookkeeping into his
commits would reverse that decision for no gain.

(The per-city `.kingdom/` stays what it is: worktrees. A *working directory* is
not state — it is derived, disposable, and this task is about learning to
dispose of it. The name is shared; the meaning is not.)

**Why files and not SQLite.** SQLite buys transactions, indexed queries, and
concurrent writers. There is one writer — this process — the whole dataset is a
few hundred small documents that already live in memory, and the most complex
query in the codebase is `filter(|p| p.city == id)`. Against that it costs a
native dependency, a schema kept in sync by hand with types that today
`#[derive(Serialize)]` for free, and migrations for a single-user local tool.
It also cannot live in `kingdom-core` (wasm), so it would sit in `kingdom-app`
behind the same seam a file store sits behind.

The seam is the point. `api.rs` already promises this in a comment — *"It sits
behind these server functions, so swapping in SQLite later touches only this
module."* Keep that promise and take the cheap option now. When there are
genuinely concurrent writers, or a hundred thousand plans, or a query worth an
index, SQLite is a change to one module.

**Why one file per plan.** A write touches only what changed. An unreadable file
loses one plan rather than the kingdom. The directory listing *is* the index. And
it is greppable — not a small thing for a product whose entire premise is that
the King can see what his agents did.

### Shape

New module `crates/kingdom-app/src/store.rs`, `#[cfg(feature = "ssr")]`:

```rust
pub fn load(root: &Path) -> Vec<Plan>;          // empty when absent or unreadable
pub fn save(root: &Path, plan: &Plan) -> io::Result<()>;
pub fn next_number(plans: &[Plan]) -> u64;      // max `plan-<n>` seen, plus one
pub fn archive_patch(root: &Path, id: &PlanId) -> PathBuf;
```

- **Write-through cache.** The `Mutex<Kingdom>` stays as the read path; every
  mutation also writes the one plan it touched. `api.rs::update` is already the
  single funnel for plan mutations, so it is the hook — plus `begin_plan`, which
  pushes directly.
- **Atomic writes.** Serialise to `<file>.tmp`, then `rename`. A half-written
  plan file is worse than a missing one.
- **A failed write is a visible note, not a refused decree.** `save` returns
  `Result`; the caller turns a failure into a `NoteKind::Failed` on the plan in
  memory. Refusing the King's work because the disk was full would be a worse
  outcome than an unsaved plan he can see is unsaved.
- **Versioning.** `kingdom.json` carries `version: u32`. New fields land as
  `#[serde(default)]` — already how `sandbox` and `working_on` were added. A
  plan file that fails to parse is skipped, not fatal: one bad document must not
  cost the King his whole court.
- **Ids survive.** `PLAN_SEQ` is seeded from `next_number(&loaded)` rather than
  from a stored counter. A counter can drift from what is actually on disk;
  `max + 1` cannot. Non-numeric ids (`plan-ramparts`, from the sample court) are
  simply ignored by the parse.

### The trap: the opening court must not be re-seated

`assemble()` currently always calls `court(&cities)`. With storage that becomes
a duplication bug — reply to a sample plan, it gets saved, restart, and now the
kingdom holds both the stored copy and a freshly fabricated one.

So: `assemble` loads first, and seats a court **only when the store is empty**.
When it does seat one, it persists it immediately, so the fabricated court is
fabricated exactly once per kingdom and every later load treats it as ordinary
history. Extract that decision into a small function so it is testable without
touching the process-global mutex.

---

## Part 2 — The two endings

### Domain (`kingdom-core`)

`Approved` and `Rejected` are **replaced**, not joined. They describe a judgement
nobody can currently pass; `Merged` and `Archived` describe things that actually
happen to a branch. Keeping four settled states, two of them unreachable, is how
the `Blocked` mess happened.

```rust
pub enum PlanStatus {
    Drafting,
    AwaitingReview,
    Failed,
    /// Its work landed on the city's branch. The worktree is gone.
    Merged,
    /// Set aside, work preserved. The worktree is gone.
    Archived,
}

impl PlanStatus {
    /// Settled history, as opposed to still in play.
    pub fn is_settled(&self) -> bool { matches!(self, Merged | Archived) }
}

/// How a plan ended, and what it left behind.
///
/// Separate from the status because a status is a `Copy` label the map and the
/// rail paint, while this is the evidence — the sha to `git show`, the branch to
/// restore from. Folding the detail into the enum would make every match on
/// state carry a payload it does not want.
pub enum Outcome {
    Merged { commit: String, into: String },
    Archived { branch: String, tip: String, base: String, patch: Option<String> },
}
```

- `Plan` gains `#[serde(default)] pub outcome: Option<Outcome>`.
- `Plan::is_live()` and `sidebar::is_active` both collapse onto
  `status.is_settled()`. They are the same predicate written twice today.
- `NoteKind` gains `Merge` — a merge refusal is Kingdom reporting what git said,
  which is exactly what a note is for, and it must never reach a model.
- `Workspace` gains `#[serde(default)] pub base: Option<String>`: **the branch
  the worktree was cut from**, recorded by `prepare` at the moment it is true.
  This is what makes "merge to main" honest — reading the city's current HEAD at
  merge time would silently land a plan wherever the King has wandered since.

### Merge to main

One server function, `finish_plan(plan, Disposition::Merge)`. The git half lives
in `worktree.rs`, because it is git and its refusals are the contract:

1. **Refuse if the plan is busy.** A draft in flight is mid-write.
2. **`InPlace` has nothing to merge.** Settle as `Merged` with a note saying so
   plainly. The work is already in the folder; pretending to merge it would be a
   lie, and refusing would leave the King with a plan he cannot close.
3. **Commit anything uncommitted in the worktree**, with a message naming the
   plan, and note it in the transcript. The alternative — refusing until the King
   tidies up — strands work behind a UI that offers no way to tidy. A commit on a
   throwaway branch is fully reversible; a discarded edit is not.
4. **Check the city is on `workspace.base`.** If not, stop and name both
   branches. Checking out the base branch behind the King's back would move his
   working copy out from under him — precisely the collision this product exists
   to prevent.
5. **`git merge --no-ff <branch>`** in the city root. `--no-ff` because a plan is
   the unit of review, and one merge commit per plan is what makes "what did that
   plan actually land?" answerable afterwards with `git log --merges`.
   - **Any failure stops everything.** Capture git's stderr verbatim, add the
     conflicted paths from `git diff --name-only --diff-filter=U`, run
     `git merge --abort`, and record it as a `NoteKind::Merge` note. Status is
     left untouched — the plan is still awaiting review, because it is.
   - It returns `Ok(plan)`, not `Err`. A conflict is not a plumbing failure; it
     is a real event in the plan's life and belongs in the plan's log, where the
     King is already looking. `Err` stays for "the server could not do the work".
   - A dirty city working tree needs no check of ours — `git merge` refuses on
     its own, more specifically than we could.
6. **On success, dispose of the worktree.** `git worktree remove --force`, then
   `git branch -d` — the *safe* delete, which succeeds only because the branch is
   now merged. This is the answer to the question `worktree.rs` deferred: a
   worktree is disposable exactly when its work has landed.
7. Record `Outcome::Merged { commit, into }`, set status `Merged`, note the sha.

### Archive

`finish_plan(plan, Disposition::Archive)`. The promise is: **the checkout goes,
the work does not.**

1. Commit anything uncommitted, as above — otherwise `worktree remove` throws it
   away, which would make archiving a destructive act wearing a gentle name.
2. **Write a patch**: `git format-patch --stdout <base>..<branch>` into
   `<kingdom_root>/.kingdom/archive/<plan-id>.patch`. `format-patch` rather than
   `diff` because it keeps each commit's message and author and replays with
   `git am` — the difference between recovering a change and recovering the work.
3. **Keep the branch.** Branches are nearly free and are a more faithful record
   than any file. The patch exists for the day the branch is pruned or the repo
   re-cloned; belt and braces, cheaply.
4. `git worktree remove --force` — reclaiming the checkout is the entire point.
5. Record `Outcome::Archived { branch, tip, base, patch }`, status `Archived`.

**Restoring is explicitly not built.** The recorded outcome carries everything a
later "Restore" would need. Guessing at that UI now, with nobody asking for it,
is how the lease machinery happened.

### Guard: a settled plan is history

`say` and `draft_plan` refuse when `status.is_settled()`. Without it a stale tab
reopens a conversation whose workspace no longer exists on disk.

---

## Part 3 — The chamber

**A `Done ▾` button** beside the composer's `Decree`, opening a two-row menu in
the established `WorkspacePicker` shape:

- **`Merge into main`** — naming the real base branch, since `workspace.base`
  knows it. Detail: *"Lands this work in the project and clears the worktree."*
- **`Archive`** — detail: *"Sets this aside. The branch and a patch are kept, so
  it can come back."*

Two rows and no confirmation dialog. Both are recoverable (one makes a revertable
merge commit, the other preserves everything), and the King's scarce resource is
attention — a modal spends it to prevent nothing.

**A conflict** renders where it already would: `Transcript` draws
`NoteKind::Merge` as `.chat-note.note-merge`, no new component. It gets a colour
distinct from `note-failed`, because a conflict is the King's problem to resolve
and a failed model call is not.

**A settled plan** loses the composer and the Done button; a footer states the
outcome — *"Merged into main as `a1b2c3d`."* or *"Archived on `kingdom/<id>`.
Patch kept at …"*. The chamber stops being a place to type and becomes a record.

### Everything the rename touches

- `style/abstracts/_tokens.scss`: `$palette` key `"approved"` → `"merged"`;
  `$statuses` keys → `"merged"` / `"archived"`. Colours are unchanged — blue for
  landed, idle grey for set aside — and must stay in step with
  `PlanStatus::color`, as the comment there already insists.
- `map/city.rs`: `p.status != PlanStatus::Rejected` → `!= Archived`, a mechanical
  rename. (Whether a *merged* plan should still gild the roofs it touched is a
  real question and is deliberately not answered here.)
- `sample.rs`: the settled pair become merged and archived. Its existing test —
  trouble and history must be visible on first run — still holds and still pins.
- The map legend is driven from `PlanStatus::ALL`, so it follows for free.

---

## Tests

Four, each pinning something a caller depends on, and none restating the
implementation.

1. **`a_conflicted_merge_leaves_the_city_exactly_as_it_was`** — the safety
   invariant the whole feature rests on. Real repo, divergent edits to one file,
   merge attempted: city HEAD unmoved, `git status --porcelain` empty, no
   `MERGE_HEAD` left behind, worktree still present, plan still `AwaitingReview`
   and carrying a note naming the conflicted path.
2. **`a_clean_merge_lands_the_work_and_disposes_of_the_worktree`** — the change
   is on the base branch, the worktree directory is gone, the branch is gone.
3. **`archiving_keeps_the_work_recoverable`** — worktree gone, branch still
   resolves to the recorded tip sha, patch file exists and contains the change.
   This is the promise archiving makes; if it can regress, it will.
4. **`plans_survive_a_restart`** — save, load, compare; the next plan number is
   above the highest stored id; and a store with plans in it does not get a
   fabricated court seated over the top of it.

The existing `each_mode_prepares_the_checkout_it_promises` extends by one
assertion — `workspace.base` is the branch actually cut from — rather than
growing a test of its own.

---

## Not in this task

- Restoring an archived plan (the outcome records enough; nobody has asked).
- Merging to any branch other than the one the worktree was cut from.
- Deleting a plan, or garbage-collecting old archives.
- Live updates. The chamber still polls.

## `AGENTS.md`

§4 loses "Removing worktrees", "Persistence (state is in memory…)" and "Plan
approval/rejection actually doing anything" from *not built at all*, and gains a
line on where state now lives.
