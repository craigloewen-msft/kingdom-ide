# Archiving prunes the plan's branch

## Why

Finishing a plan has two endings, and they leave the city in inconsistent
states:

- **Merge** removes the worktree *and* deletes `kingdom/<uuid>` — the branch
  goes with its worktree once the work has landed.
- **Archive** removes the worktree but leaves `kingdom/<uuid>` behind forever.

A King who archives a handful of plans accumulates a `git branch` list full of
`kingdom/3f2a…` noise in his own repository. Kingdom's promise is to reduce the
mess many agents make, not to add a new kind of it. Archiving should reclaim the
branch the same way it reclaims the checkout.

## Is the patch really enough?

Mostly — with one condition that has to be enforced in code, not assumed.

`archive` already writes `git format-patch --stdout base..branch` to
`.kingdom/archive/<plan-id>.patch`. That carries every commit's message, author
and diff, and replays with `git am`. It is a *better* record than the branch for
recovery-after-reclone, which is exactly why it was written in the first place.

But today the patch is explicitly the belt to the branch's braces:
`patch: Option<String>` is `None` when `format-patch` produced nothing or the
file could not be written, and the code comments say so. Deleting the branch
unconditionally would turn that tolerated `None` into silent data loss.

Two gaps to close before the branch may go:

1. **Binary changes.** `format-patch` without `--binary` emits
   `Binary files differ` — unapplicable. Today the branch covers that; without
   it, binary work is lost. Add `--binary`.
2. **The base is a name, not a sha.** `Outcome::Archived.base` records a branch
   name (`main`), which will have moved by the time anyone restores. Record the
   base *commit* alongside it so a restore knows where the patch was cut from.

## What to do

**`crates/kingdom-app/src/worktree.rs` — `archive`**

- Add `--binary` to the `format-patch` invocation.
- After `worktree remove --force` succeeds, delete the branch — **only if** a
  patch was actually written *and* the branch is one Kingdom created
  (`kingdom/` prefix). A `WorkspaceMode::Branch` plan is checked out on the
  King's own branch; deleting that would be destroying his work, not tidying up.
- `git branch -D <branch>` (force: the branch was never merged, which is the
  whole point). If the delete itself fails, that is not a reason to fail the
  archive — the work is preserved either way. Keep the branch name in the
  outcome regardless, as the record of what it was called.

**`crates/kingdom-core/src/model.rs` — `Outcome::Archived`**

- Record whether the branch survived, so the chamber does not tell the King to
  `git checkout` something that is gone. Smallest honest shape: keep the
  existing fields, add `base_commit: String` and make the summary read from what
  actually exists — e.g. `"Archived at 3f2a1c9, kept as a patch."` when the
  branch was pruned, and the current `"Archived on <branch>, at <sha>."` when it
  was not (in-place plans, the King's own branch, or a failed patch write).
- Update the sample/mock `Outcome::Archived` in `store.rs` and `mockdata` to the
  new shape.

**`crates/kingdom-app/src/components/conversation.rs`**

- The Archive row's copy currently promises "The branch and a patch are kept".
  It should promise what will now be true: the work is kept as a patch, the
  branch is cleaned up.

## Tests

Amend the existing `archiving_keeps_the_work_recoverable` rather than adding a
second archive test — its assertion that the branch survives is the behaviour
being deliberately changed. It should now pin:

- the worktree is gone, the branch is gone, and the patch on disk still carries
  `folly.rs` / `fn doomed()`;
- the recorded `tip` and `base_commit` are real shas.

Add one new test that earns its place: **archiving a `WorkspaceMode::Branch`
plan must not delete the King's branch.** That is the destructive edge this
change introduces, and nothing else pins it.

## Out of scope

The restore button. The outcome now carries everything a restore needs
(`git am` the patch onto `base_commit`), but guessing at that UI is what §4 of
AGENTS.md warns against.
