# The rail shows a plan's real name

The left rail labels a plan with something stale or wrong. Make it show the
title the court actually settled on — the one on the proposal the King read —
as soon as there is one.

## Why it is wrong today

A plan's `title` is written by two different things, neither of which is the
plan's name:

1. `Plan::opened` (`kingdom-core/src/model.rs`) sets it to the decree's first 60
   characters. Correct as a placeholder, wrong once the work has a name.
2. `api::settle` (`crates/kingdom-app/src/api.rs:982`) then overwrites it on
   **every** drafting turn with `draft.title` — whatever markdown heading the
   model happened to lead its reply with (`llm/copilot.rs::headline`). So the
   rail label mutates behind the King's back and reflects the last thing the
   model typed, not the work.

Meanwhile the one title that *is* deliberate — `Proposal::title`, the headline
the model wrote when it called `propose_plan`, the one the King read on the card
before pressing "Start with this" — is never applied. The doc comment on
`Proposal::title` says so outright: *"Not applied to `Plan::title` today."*

Net effect: the rail shows an accident, and the accurate name sits unused one
field away.

## Shape

Two edits, both small.

### 1. A proposal names the plan

`Plan::propose` (`kingdom-core/src/model.rs`) sets `self.title` to the
proposal's title alongside storing the proposal. Doing it in `propose` rather
than at the `api.rs` call site keeps the plan's name and the plan's proposal
impossible to desynchronise, and a revised proposal renames the plan for free.

`slug` is deliberately **not** touched: it is the git branch, already cut on
disk, and renaming a branch mid-flight is task 00070's business, not this one's.
The branch/label divergence this introduces is the existing behaviour for every
plan already on disk; 00070 is where it is closed properly.

Update the `Proposal::title` doc comment, which currently documents the opposite.

### 2. `settle` stops overwriting the title

`api::settle` keeps setting `summary` from the draft and drops the
`plan.title = draft.title` line. This is the half the King feels immediately:
the label stops changing under him every turn.

`llm::Draft::title` then has no consumer — check `llm/copilot.rs::headline` and
`llm/mock.rs`; if nothing else reads it, remove the field and the `headline`
helper with it rather than leaving a field that lies about being used. (00070
plans the same removal; whichever lands first does it.)

Nothing in the rail itself changes — `sidebar.rs` already renders `plan.title`
and already keys its `For` on it, so the row re-renders when the name changes.

## Tests

Two, both regressions this task exists to pin:

1. `kingdom-core`: `Plan::propose` sets `title` to the proposal's title and
   leaves `slug` alone.
2. `kingdom-app`: `settle` with a successful draft leaves `plan.title`
   unchanged.

## Not in scope

- Renaming the branch to follow the new title (task 00070).
- Naming a plan with a cheap model at open (task 00070).
- Any rename UI for the King.
