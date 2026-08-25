# Give the court somewhere to write the plan down

Kingdom's agents investigate forever and never propose. The last real decree
(`plan-8`) spent **21 rounds, 33 tool calls and 81,854 tokens** and ended in
`Drafting` with `proposal: None` — it never called `propose_plan` at all.

The cause is structural, and Phoenix already solves it. This ports Phoenix's
mechanism rather than describing it in prose, then verifies the result in a
browser.

## The evidence

From `~/dev/.kingdom/plans/plan-8.json`. The model's own reasoning shows it had a
**complete, decided plan by round ~11** — component file, server function,
resizer direction, localStorage keys, sort order, entry cap, empty state. Rounds
12–21 then re-derive the same decisions with cosmetic variations, while
re-reading files it had already read:

| file | times opened |
|---|---|
| `components/conversation.rs` | 7 |
| `api.rs` | 5 |
| `kingdom-core/src/model.rs` | 4 |

It re-decides the same names repeatedly — `.survey` vs `.districts` vs "Files";
`DirEntry` vs `FileEntry` vs `WorkspaceEntry`. The last sixteen calls return
481–4,939 bytes each: micro-verification, not investigation.

Truncation is **not** the cause — only 5 of 33 results exceeded `MOST_REPLAYED`,
none in the stalling tail. Across `plan-3/5/6/7/8` the `think` tool was called
**zero times**.

## Why it breaks, and what Phoenix does instead

**The model has nowhere to put the plan.** Kingdom's `propose_plan` carries
`title` and `body` inline, so the model must hold the entire plan in its head
and emit it in one blob. Nothing it has decided is ever banked. So each round it
faces the same choice — keep looking, or produce everything at once from memory
— and keeps choosing to look. The repeated re-naming in the reasoning is exactly
what that looks like: decisions made and lost because they were never written
down.

**Phoenix externalises the plan as it goes**, in three parts that work together:

1. Explore mode gets a **scoped `patch`** —
   `PatchTool::for_task_proposal_drafts(tasks_dir_name)`
   (`phoenix-tools/src/lib.rs`). It can write, but only to the tasks directory.
   The plan goes to disk incrementally and can be revised by further patches.
2. Every successful write in that scope appends a mechanical cue to the tool
   result (`phoenix-tools/src/patch.rs:187`):
   `<next_step>Call propose_task with task_file="…" if this is the task you want the user to approve.</next_step>`
   The exit is pointed at, in-band, attached to the act of drafting.
3. `propose_task` takes a **path**, not content — so proposing is cheap once the
   file exists, and revising is a patch rather than a re-emission.

Kingdom has none of these. `propose_plan.rs`'s module doc argues the inline
form "needs none of it" — that reasoning is what this task reverses, and it must
be rewritten rather than left contradicting the code.

## Changes

### 1. Scope the patch tool — `tools/patch.rs`

Port Phoenix's `PatchScope`:

```rust
enum PatchScope { Unrestricted, Draft { dir: String } }
```

- `Patch::unrestricted()` — today's behaviour, for `Full`.
- `Patch::for_draft(dir)` — refuses any path outside `dir`, refuses `..`
  components, refuses non-`.md` files. Mirror Phoenix's `enforce_scope`,
  including its refusal wording, which is written for the model to recover from.

### 2. Emit the next-step cue — `tools/patch.rs`

Kingdom's `proposal_next_step`. On a **successful** write under `Draft`, append
to the tool output:

```
<next_step>Call propose_plan with draft="<path>" if this is the plan you want the user to approve.</next_step>
```

Never under `Unrestricted` — Phoenix returns `None` there, and a working plan
must not be told to propose. Escape the path as Phoenix does.

### 3. Where the draft lives

**Not the user's project.** Kingdom's rule that it does not write files into the
user's repo stays; Phoenix's `tasks/` has no Kingdom counterpart.

Use **`.kingdom/draft.md` inside the plan's own workspace**. It resolves through
`Sandbox::resolve` (so `patch` and `read_file` reach it with no new plumbing),
and `worktree.rs::exclude_worktree_dir` already adds `.kingdom/` to the repo's
`info/exclude` — which git shares across worktrees — so it never shows as
untracked work. Confirm that exclude actually covers the worktree case before
relying on it; if it does not, extend it rather than moving the draft.

### 4. Offer it while proposing — `tools/mod.rs::all`

`Permissions::Propose` gains `Patch::for_draft(...)`, exactly as Phoenix's
`explore_*` registries push the scoped patch. `Full` keeps `Patch::unrestricted()`.

This changes what the "no `patch` while proposing" boundary means, and the long
doc comment in `tools/mod.rs` (§"Why `Propose` keeps `bash` but loses `patch`")
now states the opposite of the code. Rewrite it: the boundary is no longer
"cannot write" but "can write only its own draft" — which is Phoenix's boundary,
and is still a clear statement of the job. `AGENTS.md` §4 says the same thing in
two places and needs the same correction.

### 5. Accept a path — `tools/propose_plan.rs`

Add a `draft` argument taking a workspace-relative path, read through the
sandbox: title from the body's first `# H1`, body from the file — Phoenix's
`TaskSource::PlainMarkdown` rule. Keep inline `title`/`body` working so nothing
in flight breaks, but make `draft` what the prompt teaches.

Rewrite the module's §"Why the proposal travels in the arguments". The new
reasoning: the body still lands on the plan and in the transcript exactly as
before — it is read off disk at propose time rather than retyped by the model.

### 6. Port the mode block — `llm/system_prompt.rs`

Rewrite `PROPOSE` to describe the two-step workflow, following
`llm_language.rs::mode_explore`'s shape: draft the plan to `.kingdom/draft.md`
with `patch` (`operation: overwrite`), then call `propose_plan` with that path;
`patch` is restricted to that file in this mode.

**No new Kingdom-invented guidance blocks.** In particular do **not** restore
`ECONOMY` or anything like it — Phoenix does not send one, and the drafting
mechanism above is what does this job. `SHARED_MACHINE` and `MERMAID` stay as
they are.

### 7. Tests

- Scoped patch: accepts `.kingdom/draft.md`; refuses `src/main.rs`, `..`
  traversal, and a non-`.md` path.
- The `<next_step>` cue appears on a successful draft write and **not** on an
  unrestricted one, nor on a failed patch (mirror Phoenix's own three tests at
  `patch.rs:536/582/620`).
- `propose_plan` with `draft` reads title and body off disk; a missing file is a
  refusal the model can act on.
- `Propose` offers `patch`; the offered list and the runnable list still agree
  (the existing `tools::all` invariant).
- The remit still renders last (`the_remit_is_the_last_thing_the_model_reads`).

## Verification

```bash
cargo test -p kingdom-core
cargo test -p kingdom-app --features ssr --no-default-features
```

### Then the real test: a full browser E2E

The suite pins wiring, not whether the model now concludes. So drive Kingdom
against a real decree and a real model.

**Setup.** Serve on an unusual free port, never 3000 — the King's own server is
very likely there:

```bash
LEPTOS_SITE_ADDR=127.0.0.1:3947 cargo leptos serve
```

Chromium is at `/usr/sbin/chromium`; the credential comes from `agency auth
github` via `KINGDOM_API_KEY_HELPER`'s default. Open `~/dev` — a real kingdom,
not a proving ground, since the feature under test is about choosing a real
folder.

**The decree**, typed into the prompt bar in the browser:

> When the user has selected their kingdom folder once, store that value and then
> automatically load up into it in future runs

A fair test: `app.rs` already has `local_storage()`, `restore_choice` and
`restore_workspace` as the pattern, and `ChooseKingdom` is where it lands. The
wrinkle the agent must notice — the kingdom root is a **server-side** path, so
remembering it client-side means re-opening through `open_kingdom` on boot, with
`enforce_sandbox` still honoured for the remembered path.

**Pass conditions.** All of:

1. The plan reaches `AwaitingReview` with a real `Proposal`, **in materially
   fewer rounds than plan-8's 21**, and without re-reading any file more than
   twice. Record the round count — this is the regression under test.
2. The transcript shows the intended shape: `patch` writes the draft, the
   `<next_step>` cue comes back, `propose_plan` follows. If the model proposes
   without ever drafting, say so — that means the cue, not the draft, is doing
   the work.
3. The proposal names real paths it actually opened (`app.rs`, `api.rs`).
4. Approving it (**Start with this**) carries the same conversation into `Full`,
   and the work lands and compiles.
5. Reloading the browser lands straight in the kingdom, with no `ChooseKingdom`.
6. Clearing the stored value returns to `ChooseKingdom`.

**Report back** rounds and tool calls against plan-8's 21/33, the proposal, and a
screenshot of the reload skipping the picker. If it still stalls, say so plainly
rather than tuning until it passes.

Stop the server when done. The feature the agent writes is the *subject* of the
test; the deliverable is §1–§7 plus the evidence.

## Open question for the King

§5 keeps inline `title`/`body` alongside the new `draft` path. Phoenix accepts
**only** a path. Dropping the inline form would be the stricter port and would
remove the "two ways to do it" ambiguity from the model's view; keeping it is
the conservative choice. Say which you want — the task assumes keeping it.
