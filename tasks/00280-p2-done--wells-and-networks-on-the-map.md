# Wells and networks, drawn on the map

**Status:** done · **Priority:** p2

The map answered the first of the product's three questions and half of the
second. It said nothing about the two things Kingdom actually coordinates: the
**well** a city shares, and which **network** each agent is on. Both existed and
both were invisible unless you had a particular chamber open.

Shown as the well, the host ring, an agent's moat; called shared services and
network modes in code.

## What was already true and unseen

| Fact | Where it lived | Who could see it |
|---|---|---|
| a city's wells | `services::running_in` | only the open chamber's badge |
| a plan's network mode | `Plan::network` | the chamber |
| a plan's forwarded ports | `netns::forwards_of` | the chamber |

All three reached the browser through `events::on_the_wire`, **one plan at a
time**. The map needs the whole kingdom at once, so the first piece of work was
a feed, not a mesh: `api::kingdom_network`, the sibling of `kingdom_changes`.

## The deadlock this was nearly built on top of

While reading the publish path before writing anything, I traced a re-entrant
deadlock: `api::update` runs holding the kingdom's `std::sync::Mutex`, calls
`events::publish` → `on_the_wire` → `api::city_root_of` → `lock()` **again**. A
non-reentrant mutex taken twice by one thread deadlocks *while holding the
lock*, so every later request in the process hangs behind it.

It was already fixed on `main` (`bcd915b`), which the merge brought in. The fix
is `city_root_in(&kingdom, id)` and `publish_within(plan, city_root)` — resolved
by whoever already holds the lock, passed down.

**That set the shape of this feature.** Everything here attaches *more* runtime
truth to things bound for a browser, which is exactly the path that deadlocked.
So `kingdom_network` takes the guard **once**, collects every plan's city root
through `city_root_in`, drops it, and only then asks `services::` and `netns::`
anything. A test holds the lock while doing the collecting half, on its own
thread with a deadline, so a reintroduction fails rather than hanging the suite.

## What is drawn

| Mark | Meaning |
|---|---|
| slate band inside the rim | the King's own machine |
| wellhead on a town's square | one container that city shares |
| agent mark, in its banner colour | one live plan |
| conduit to the ring | this agent binds your ports |
| moat, and no conduit | this agent has a network of its own |
| channel to a wellhead | this agent is *drawing from* that well |

The last three together are the point. **An isolated agent still reaches its
city's well** — `slirp4netns` blocks host loopback and nothing else, so a Docker
bridge address is just another route out. Nothing else in the interface says
that, and it reads like a contradiction until you see it.

`drawing_from` is what a plan is *registered* as using, not what its city has
standing: every plan could reach the database, and drawing a channel from all of
them would claim five connections where there is one.

## Three faults only the render found

The geometry had twelve passing tests before it was ever drawn. All twelve still
passed while the picture was wrong in three ways:

1. **The well was the same colour as an agent.** `#38bdf8` sits 110.5 from the
   `azure` banner on the palette's own weighted-RGB ruler — closer than the two
   nearest *banners* are to each other (126.1). My test asserted the well was
   not *equal* to any banner, which is a bar that cannot catch this. It now
   asserts a margin against the palette's own worst pair, and fails on the old
   colour with the measured numbers in the message.
2. **The conduits were roads.** At 3.4 units they outweighed the streets they
   crossed. Now 1.8: a connection is a thread between two marks.
3. **The agents stood among the buildings** — one on the keep in the middle of
   the square. They now ring the settlement, placed from the town's own
   `extent`, so a big project and a tiny one both push their agents clear.

Worth stating plainly: a test suite that passes is not a picture that reads.

## Rehearsed against a real database

Not a fixture of the shape of one. `KINGDOM_REALM=shopfront`, a real `mongo:7`
container, a real isolated plan:

```
wells:  [{city: shopfront, name: db, address: 172.31.206.10:27017, users: 1}]
agents: plan-1          Isolated  drawing_from: [db]   ← moat, no conduit, channel to well
        plan-aqueduct   Shared    drawing_from: []
        plan-foundations Shared   drawing_from: []
        plan-proposing  Shared    drawing_from: []
        plan-ramparts   Shared    drawing_from: []
```

The address was reachable from the host (`172.31.206.10:27017`), and resolving
that exact payload against the live manifest produced one wellhead, five
non-overlapping agent marks, four host conduits and one well channel. The
isolated plan's colour — orchid `#f55ced` — matches its pip in the rail, checked
by calling `assign_banners` rather than by eye.

## Where the code lives

| File | Target | What |
|---|---|---|
| `core/review.rs` | both | `KingdomNetwork`, `CityWells`, `AgentNetwork` |
| `app/api.rs` | ssr | the feed, and the lock discipline |
| `app/services.rs` | ssr | `draws_from` |
| `citymap/map/network.rs` | both | geometry + every judgement, 15 tests |
| `citymap/engine/network.rs` | hydrate | the meshes |
| `app/app.rs` | hydrate | `watch_kingdom_network` |

The engine never learns what a plan, a city or a container is: `SetNetwork`
carries town names and colours, translated in `view.rs` — the boundary
`TownActivity` and `SetWorks` are already held to. The picture travels as a
command, not in the manifest, because `citymap.rs` memoises that on kingdom root
and city names and deliberately not on anything that moves.

Two incidental things: `apply_commands` had reached Bevy's sixteen-parameter
ceiling, so the three live overlays are now one `Overlays` system param; and
`watch_kingdom_network`'s digest excludes `working_on` on purpose, unlike the
works watcher's — what an agent is plugged into does not change when it edits a
file.

## What this is not

- **Not arbitration.** It reports what is connected to what. Nothing detects a
  genuine clash; a shared `target/` still blocks with nothing said.
- **Not one mark per port.** A conduit says the agent *has* a network. The
  chamber's badge reports numbers, and it is fed by the per-plan socket.
- **No new bookkeeping.** Everything drawn already existed server-side.
- **No relayout.** The rim fringe and the square were already empty ground.
