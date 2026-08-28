# Wells that stand exactly while agents need them

**Status:** done · **Priority:** p1

Shared resources were raised by the wrong things and released by too few. A
well came up when a plan *took a turn* or when the King *opened a shell*, and
went down only when a plan was merged or archived. Two consequences, both
visible to the King:

- **A restart left five live agents with no database.** `cargo leptos watch`
  restarts on every save. The registry is process-global and starts empty, so
  `running_in` answered nothing: the ports badge went blank, the map drew no
  wellhead, and `/resources` reported `not started` for a project with five
  agents open. The first agent to take a turn repaired it silently.
- **Closing a kingdom leaked every well it held.** `leave_kingdom` dropped the
  kingdom and forgot its pulses but never touched services, so containers stayed
  up for the life of the server, claimed by plans nobody had open.

## What was built

One reconciler, and the invariant it holds:

> A well stands exactly while at least one **live, non-subagent plan that can
> reach it** exists.

`services::reconcile(agents)` is now the only thing in the module that starts or
stops anything. It is handed the whole live population, groups it **by scope
key**, raises each distinct scope once, and stops whatever nobody is left
drawing from. `api::reconcile_wells` calls it at the four moments that
population changes: a kingdom opened (`assemble`), a plan opened (`begin_plan`),
a plan finished (`finish_plan`), a kingdom closed (`leave_kingdom`).

`ensure` and `release` are gone as public entry points. `turn.rs` and
`terminal.rs` now call `services::require`, which **waits for a raise in flight
and then verifies**, refusing exactly as before but raising nothing: opening a
terminal is not a reason to start a database.

## Four things worth remembering

**Declarative beat incremental, and the first draft got it wrong.** `raise`
initially *extended* the drawer set. That is the natural shape when you are
thinking per-plan, and it is a leak: a finished plan stayed in the count
forever, so `users_of` reported a database as busy that nobody was in. Because
`reconcile` is handed the whole population, the set it computes **is** the
answer — `insert`, not `extend`. The bug is now a comment on the line.

**Two locks, deliberately different kinds.** The registry stays a
`std::sync::Mutex` because every read of it is synchronous and instant. Raising
needed a second, *async* guard (`RAISING`, a `tokio::sync::Mutex`) because the
section it protects is full of awaits — `docker run`, and a wait for a port
allowed to take three minutes. Without it, a kingdom opening and a turn
beginning within a second reach `docker run` for the same container name and the
loser takes a bare "name already in use", which reads as a Kingdom bug rather
than a race. `concurrent_reconciles_raise_one_container` fires four at once
against a real daemon and pins one container.

**Ordering in `finish_plan` is load-bearing.** The old `release` ran *before*
the worktree was torn down. A reconcile cannot: the plan is still live at that
point, so it would immediately re-enrol itself. It moved to after `update`,
which is what makes the plan absent from the population — and it means a merge
git *refuses* correctly leaves the plan live with its database standing, because
he is going to try again.

**The sweep will not stop what it did not raise.** At boot the registry is
empty, so a container left standing by a previous server is invisible to the
sweep and is left alone — adopted if an agent needs it, untouched otherwise.
Stopping it would mean killing a database on the strength of a label, having
never spoken to whoever started it. This is the same judgement that already made
this module *adopt* on restart where `netns` *kills*: a namespace with no server
is worthless, a database is not.

## Decisions worth keeping

**Raising is spawned, not awaited.** `READY_TIMEOUT` is three minutes because a
first run pulls the image. The King must not sit on a folder picker for that,
nor wait on `docker stop` after pressing Merge. The consequence is that
`/resources` had to learn to refresh itself — a five-second poll owned by the
screen and cleared on cleanup, the pattern `ticking_clock` and `rail_clock`
already set — or the King would land on a screen reading `not started` for a
well that came up moments later.

**The population is read from the `Kingdom` value already in hand.** Callers of
`assemble` hold the kingdom's non-reentrant mutex; going through `city_root_of`
would take it a second time on the same thread. That is the deadlock
`city_root_in` and `kingdom_to_browser` already exist to avoid, and
`agents_drawing` is pure so the rule is testable without a daemon, a runtime or
the process-global kingdom.

**Subagents are excluded, and it is not tidiness.** `finish_plan` *refuses* to
finish a subagent, so one recorded as a drawer could never be released and would
hold its well open for the life of the process. It works in its parent's
worktree and reaches the well through the parent's claim.

## Where the loopback relay ended up

Merging main brought in wells-on-a-plan's-`localhost`
(`netns::open_wells`), which had been hung off the end of `ensure` — the
function this work deleted. The two features meet cleanly once you notice they
are at **different grains**: raising a container is per *scope* and now happens
when a kingdom opens, while a relay lives inside one plan's network namespace
and cannot exist before that plan does.

So `open_wells` moved to `require`, the per-plan path, which both `turn` and
`terminal` already route through *after* raising the namespace — preserving the
original "one function nobody has to remember" argument. Putting it in `raise`
would have been wrong twice: no namespace exists at kingdom-open, and `raise`
handles a whole city's agents at once rather than the one plan a relay is for.

## Proven against a real daemon

`a_restart_brings_the_well_back_to_the_agents_that_had_it` simulates the restart
honestly — registry cleared *and* container stopped, which is what the previous
server's last release leaves behind — and asserts the same container, the same
address, **both** drawers counted again, and data written before the restart
still readable. Plus the three that already existed, all rewritten against
`reconcile` and passing: the lifecycle, the host scope across two projects, and
the five-agent rehearsal.
