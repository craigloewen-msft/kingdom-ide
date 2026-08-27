# What is real, what is faked, what is not built

The honest ledger. [`AGENTS.md`](../AGENTS.md) carries the summary; this is the
detail and the reasoning for each gap.

## Faked

`kingdom_core::sample::starter_plans`:
- The *opening* court: the plans a kingdom starts with, before the King has
  issued any decree. Plans he opens himself are entirely real.

## Not built at all

- **Subagents with tools, and subagents that spawn subagents.** A subagent is
  read-only
  (`Permissions::ReadOnly`), which is what makes running several of them in one
  worktree safe without arbitrating anything. Both extensions need the same
  missing piece: the moment a subagent can write, two of them can collide, and
  that is the resource question the product exists to answer. `Permissions::Full` is the seam either
  would arrive at.
- **Subagents while drawing up a plan.** `spawn_agents` is `Permissions::Full`
  only, so a proposing plan cannot fan out — which is a shame, because exploring
  a codebase to write a proposal is the most fan-out-shaped work there is. It
  was left out rather than guessed at; the case for adding it is that
  `Plan::spawned` already pins a subagent to `ReadOnly` unconditionally, so a
  subagent of a proposing plan would be the same read-only thing a subagent
  always is.
- Restoring an archived plan. Its outcome records the branch, the tip and a
  patch, so everything a restore would need is kept — but nothing has asked for
  the button yet, and guessing at that UI is how the lease machinery happened.
- Live updates on the **map**. The chamber is pushed to over a WebSocket
  (`events.rs`, `watch.rs`), the plan's browser is mirrored over a second one
  (`screencast.rs`), and the **rail** now has one of its own — a kingdom-wide
  socket carrying a `PlanPulse` per plan (`watch.rs::KINGDOM_ROUTE`), which is
  what lets a plan that has stopped to ask something say so from a chamber
  nobody has open.

  The map does not read it yet. Which towns are working is still *polled*, every
  two seconds and only while the map is on screen (`app.rs::poll_activity` over
  `api::kingdom_activity`) — written when `events.rs` was keyed per plan by
  design and a kingdom-wide channel was "a real change rather than a smaller
  one". That change has since landed, so the poll is now a survivor rather than
  a necessity, and folding it onto the pulse is a tidy-up someone should do. The
  rest of the map is a Bevy canvas, and colouring holdings from plan state is
  its own piece of work. The spyglass is still deliberately *not* surfaced
  there: a city lighting up because a plan holds a live browser needs a plan
  that knows it owns a session.

  Both halves of "is the King looking at the map?" hang off the **same**
  `on_the_map` memo in `ThroneRoom`: it stops the activity poll
  (`poll_activity`) and it stops the engine drawing (`ViewerCommand::Show`).
  Keeping them on one signal is what stops the map from polling for a ring that
  nothing is rendering, or rendering a ring nothing is refreshing.
- **Resource arbitration beyond ports** — the second of the product's three
  questions, now half-answered. A plan can be given a network of its own
  (`netns.rs`), so two agents no longer collide on 3000: each has its own, and
  the chamber shows where to reach it. That is *avoidance*, not arbitration —
  nothing detects or reports a genuine clash, and the other shared resources are
  untouched. A shared `target/` directory is the obvious next one: two plans
  building the same crate still block each other on the same lock, and nothing
  says so. Isolation is also still off by default and per plan, so the King has
  to know to ask for it.

  What the map *does* now show is the standing arrangement: the host network as
  a ring at the realm's rim, each city's wells on its square, and every live
  agent joined to what it reaches -- or moated, with nothing joining it to the
  edge. That answers "what is each agent plugged into, and who shares this
  database?" at a glance. It still does not answer "these two are fighting over
  it", because nothing anywhere detects that yet.
- Naming a plan with a model. A plan's branch is cut from its title today —
  `kingdom/<slug>`, via `kingdom_core::naming::slugify`, with `-2`, `-3` walked
  past on collision — but that title is still just the first clause of the
  decree. Having a cheap model propose a real name is task 00070; when it lands
  it changes the title, and the branch follows for free.

## Tools the court does not have

Each is its own decision:
- `keyword_search`. Wants a model call of its own. Genuinely useful, but
  `search` plus `read_file` cover most of the ground, so it earns its place
  only once someone finds the gap.
