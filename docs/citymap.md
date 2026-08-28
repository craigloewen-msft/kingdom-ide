# The map

How a project becomes a town, how a file becomes a building, and how the map
reports its own loading.

## How things are laid out

Every file is a building, measured from the file itself. The rules live in
`crates/kingdom-citymap/src/build/layout.rs`.

| What you see | What it is measured from | In code |
|---|---|---|
| House footprint | The file's share of its folder's ground, **linear in lines** | `weight`, `Building::footprint` |
| House height | Branches per line of code, clamped 11–54 | `Building::height`, `DENSITY_HEIGHT` |
| Height ceiling | ~1.9× the footprint's shorter side, so nothing hides its neighbours under the fixed camera | `Building::height_ceiling` |
| Town size | File count × `LAND_PER_FILE` (37,300 units each) | `settlement_extent` |
| Driveway width | Inbound references, **linear** to a knee of 16 | `build/streets.rs` `drive_width` |
| House shape and colour | What the file is *for* — test → watchtower, docs → scriptorium, config → council hall | `Category` → `BuildingKind` |
| Chimneys, crates, forge stacks | Bucketed branch density | `meshes::complexity_step` |

**The footprint is linear, and deliberately so:** twice the lines is twice the
area, a hundred times is a hundred times. It was `20 + √bytes`, and the square
root was the fault — it spent most of its range before the files that matter
began, so 64 KB bought about five times the lot of 1 KB rather than sixty-four.
Measured over this repository, only **33%** of holdings stood within ±43% of
their proportional share, and a file with exactly twice a neighbour's content
drew a median **0.66×** the area — less than the smaller file's own share, let
alone twice it. Linearly that figure is **87%**. A folder weighs the *sum of its
children*, which is what lets the proportion survive the recursion down to a
lot, and the house covers a constant share of that lot (`LOT_COVERAGE`) so it
reaches the eye intact.

Two floors keep the rule drawable rather than absolute. `MIN_HOLDING_LINES` (60)
stops a three-line `mod.rs` earning a house under a world unit across, with a
height ceiling below the 11-unit floor every archetype is modelled against.
`UNREAD_HOLDING_LINES` (120) gives a nominal lot to files the scanner never
opened — binaries, and anything over 2 MB — whose line count is zero because
nobody counted, not because they are empty. Sizing those by bytes would hand one
3.5 MB bundled `.min.js` a quarter of the town: **it is a rule about text.**

Two consequences worth knowing. A plot is a *share of its folder*, so the same
file looks bigger among small siblings than among large ones. And the average
plot per file is fixed, so a 2,000-file repository grows a larger island rather
than cramming the same ground into smaller houses.

Size and height are deliberately different facts: measured across this
repository, lines and total branch count correlate at 0.94, while branch
*density* is independent of size (0.06). So the base says how much code there
is and the height says how tangled it is — a long plain file is a broad low
hall, a short knotty one stands up.

### What "complexity" actually means

A **crude branch count**, not a claim about code quality: occurrences of ten
tokens — `if`, `for`, `while`, `match`, `switch`, `catch`, `case`, `else`, `&&`,
`||` — divided by lines of code.

- **Substring matching, not parsing.** A `&&` inside a string literal counts.
- **Comments and blank lines are excluded** from both count and divisor. They
  have to be, or the most heavily documented file scores as the most tangled.
- **Only code is scored.** Docs, config, data and assets score zero and sit at
  the minimum height by construction.
- Files over 2 MB and non-text files are never opened, so they too sit at the
  floor.

Read a tall building as "a lot of decisions per line, take a look", not as a
metric to optimise.

## How a change is sized

A holding is sized from the file; the **works** — what a plan is proposing — are
sized from the change, on a separate ruler: a column of the agent's colour above
the roof for lines added, a shroud over the house for lines removed.
`engine/works.rs` draws them.

**One rule spans all of it: twice the count, twice the mark.** The footprint,
the column and the driveway are all linear now, which is what lets the map be
read without undoing a curve in your head. `crate::scale::linear_then_tail` is
the shared shape and `map/works.rs` computes the shroud's share.

`magnitude(churn)` turns a count of lines into `0.0..=1.0`, and both the
column's height and its width read it. It is **linear up to `LINEAR_CHURN`
(300 lines), then a saturating tail**:

- under the knee it is exactly proportional — a +100 is exactly twice a +50;
- it never plateaus, so 935 lines and 3,872 stay different marks;
- the tail joins the line at matching slope, so there is no visible kink;
- 300 is fitted to this repository (per-file added lines: p25 = 6, median = 26,
  p75 = 117, p90 = 263, p95 = 425, p99 = 935, max 2,137), so everything up to
  p95 is drawn to scale and only the rewrites are compressed.

**Why a column is not linear all the way up, when a footprint is.** A lot is a
share of its folder's ground, so it can simply grow; a column is one axis with a
hard ceiling — 58 units, under the 60 the camera's fit reserves for a roofline —
so something has to give at the top or the tallest column can be framed out.

The input is **absolute** lines — never a share of the busiest file in the same
plan, which made two agents incomparable. `BAND_FLOOR` (2.5 units) is the only
thing that is not proportional, and it is deliberately as small as it can be: a
floor is a flat tax on the bottom of the range, and it was cut from 3.5 when the
curve above it became honest.

The ruler here has been replaced three times, and `LINEAR_CHURN`'s own docs hold
the record: relative (two agents, two rulers), then logarithmic (a +8 and a +100
looked alike, and everything past 600 lines drew identically), then a saturating
ratio (never proportional anywhere — a +27 and a +115 were 4.3× apart in work
and 2.6× apart on screen).

### A removal is one mark, not two

**The shroud, and nothing else.** A block rises from the ground and covers as
much of the house as the file is losing — half the file cut, half the house
covered. That share is a ratio of the file's own length, computed in
`map/works.rs::cover_of`, so it is linear by construction. A deletion is simply
a cover of 1.0.

There used to be a second mark as well: a stain spreading across the ground
around the house, sized on a gentler curve so it stayed visible when the map sits
in the rail's pane at a couple of pixels per house. The King reported reading
only the shroud, so the stain is gone — one fact, one mark. What it was for is
now carried by `SHROUD_FLOOR` (a cut always covers at least 8% of its house) and
`SHROUD_GIRTH` (the block is wider than any roof on the map).

The grammar has exactly one rule and no exceptions: **what is being built rises
above the roof; what is being taken away covers the house.**

## Wells and networks: what each agent is plugged into

A third ruler again, answering the **second** of the three questions in
`AGENTS.md` — *what shared resources are they holding?* Drawn by
`map/network.rs` (the geometry) and `engine/network.rs` (the meshes).

| Mark | What it is | Where it stands |
|---|---|---|
| the **host ring** | the King's own machine | a slate band just inside the realm's rim |
| a **wellhead** | one container a whole city shares | built on that town's square |
| an **agent mark** | one live plan, in its own banner colour | ringing the town it works in |

And the lines between them: a **conduit** from an agent out to the host ring
means it binds the King's own ports; a closed **moat** around an agent and *no*
conduit means it has a network of its own; a **channel** to a wellhead means it
is actually drawing from that well.

Five rules hold this together:

- **A well is built, not marked.** Everything else in the table is interface
  drawn in world space and is unlit; a wellhead is a thing standing among lit
  houses, so it is a stone drum with a timber canopy, `Surface::Matte` like the
  buildings. Drawn unlit it was the only object with no shading and no shadow —
  which is the definition of a light source, and it read as one.
- **Only a *project's* wells are drawn.** A machine-wide well belongs to no
  town, so unfiltered it would appear once in every city with an agent in it.
  `api::kingdom_network` filters on scope. The honest home for one is the host
  ring; putting it there is its own piece of work.
- **A conduit says the agent *has* a network, not what it listens on.** Ports
  move constantly; what an agent is plugged into does not. The chamber's ports
  badge reports the numbers.
- **A channel is drawn from what an agent *did*.** `drawing_from` names the
  wells a plan is registered as using — the same reference set that decides when
  a container is stopped. Every plan in a city *could* reach its database.
- **Agents and wells are placed from the town's `extent` and its square**, not
  its centre point. `streets::square_site` walks outward from the middle until
  it finds unclaimed ground, and on a real kingdom that walk landed 94 to 1,622
  units out — so a well placed at the centre stood among the houses.

The one fact this exists to draw: **an isolated agent still reaches its city's
well.** `slirp4netns --disable-host-loopback` blocks `127.0.0.1` and *nothing
else*, so a Docker bridge address is simply another host route. The picture that
says it is a moat with no conduit to the rim and a channel to the wellhead
anyway.

A well's colour must keep a **margin** from every agent banner, not merely
differ from it: `#38bdf8` sat 110.5 from `azure` on the palette's own ruler,
where the two closest banners are 126.1 apart. A test asserts the margin.

## Reporting progress while the map loads

The map says how far along it is, in two phases.

The fetch: `view::read_whole` reads the body a chunk at a time and counts bytes
against `content-length`. `progress::Transfer` refuses to answer rather than
guessing when the two cannot be compared — no declared length, or a count that
has passed it, which is what compression in front of `/kingdom/map.json` would
cause.

The raise is the half that matters. A bar over `spawn_world` could not have
moved at all: building ~20k entities held the main thread in blocks of 1.3–1.7
seconds. So `engine::raise` cuts the build into slices, publishes the fraction
between them, and lets the frame go. `Job::allowance` adapts what a frame may
spend, halved or doubled from what the *whole previous frame* cost — weighed
rather than counted, because two thousand trees and two thousand folder names
are not the same frame. A fixed 8 ms budget bounded the wrong half: issuing a
spawn is cheap, preparing meshes for the GPU is not, and that happens after the
deadline is last checked. Measured single frames of 2,694 ms proved it.

Five things are load-bearing:

- `spawn.rs`'s stages take a `Range` and `spawn_step` is the one door both paths
  go through, so the sliced build and `spawn_world` cannot raise different
  settlements.
- The root is spawned **hidden** and revealed on the last slice — the card is
  translucent, and scenery spawns visible and is culled only when `apply_lod`
  next runs, so the King would otherwise watch trees appear and vanish.
- `ActiveLod` and `Activity` are **marked changed** at completion, or both
  systems early-return forever.
- Winit is held at the watching pace while a raise is in flight (`winit_for` is
  one definition), or walking into a chamber mid-raise drops the engine to four
  ticks a second.
- `progress::Wait` **composes** the two phases: the fetch fills `FETCH_SHARE`,
  the raise fills the rest, and a phase that has not begun holds the bar where
  the last left it. Asking each phase for its own fraction swept the bar at
  every gap — and the gap between them, commented as "a frame", measured up to
  1,430 ms.

The weights in `Step::weight` are **estimates** and the design assumes so:
being wrong makes the bar advance unevenly and nothing else, and `fraction`
reaches exactly 1.0 whatever they say. The camera is off for the length of the
raise, the engine boots when the manifest lands rather than on mount (which
alone took the fetch from 3.3 s to ~60 ms), and the reveal — the single most
expensive frame in the app at 2,835 ms — waits behind a bar already seen to
reach the end. Result: 25 painted readings, against three.

Deliberately **not** done: compressing that route. It would cut 4.35 MB to
~694 KB and help more than any bar does, but it is a server change with its own
trade-offs and it is what `Transfer`'s clamp is written to survive.

## Looking at the map from a plan's browser

A plan's browser stands the map down by default (`mode.rs`). Ask for it with
`?map=on`:

```
browser_navigate  http://127.0.0.1:3000/?map=on
```

The world takes a few seconds to stand, so **wait on a value rather than
sleeping** — and assert on values afterwards too, rather than on pixels:

```js
__kingdom_map.built             // false until the world is up
__kingdom_map.hovered           // "src/main.rs", after a mouse move
__kingdom_map.clicked.holding   // what the last click actually hit
```

`window.__kingdom_map` is defined only under automation
(`view::publish_status`). Prefer it to a screenshot: it is stable against every
change to how the map is *drawn*, and it names what was hit. At the default zoom
the whole kingdom is in frame, so a single holding is a few pixels — move the
pointer and read `hovered` back rather than trusting a coordinate.

**A plan with a network of its own cannot reach *your* Kingdom.** The URL above
is the host's `127.0.0.1:3000`; an isolated plan has a different `127.0.0.1` and
slirp4netns runs with `--disable-host-loopback`, so that address answers nothing
there (measured: `000` against `200` from the host). That is isolation working.
Such a plan should start its own server and browse to *that* `:3000`. WebGL is
unaffected — SwiftShader is pure CPU rendering and needs no network.

### Why it costs what it does

A headless browser has no GPU: on WSL2 and most CI, every hardware path yields
**no WebGL context at all**, so SwiftShader on the CPU is the only renderer
there is. Measured on this map, world standing, nothing happening:

| | Cost |
|---|---|
| uncapped, unconfined | 9.50 cores |
| one frame a second, unconfined | 4.09 cores |
| capped and confined to four CPUs | 2.03 cores |

The middle row is why pacing alone was not the answer: SwiftShader sizes its
thread pool from the machine and spends most of what it spends whether or not a
frame was asked for. So the engine cuts the frames (`engine::AUTOMATED_WAKE`)
and the browser cuts the floor beneath them (`session::CPUS_VAR`, default 4).
Both are needed.

Bounded work is exempt from the frame cap, and that exception is load-bearing:
capping a world going up turned a three-second raise into **157 seconds** — the
same work spread over fifty times the wall clock, with something waiting on it.
See `engine::Pace::set_for_work`.
