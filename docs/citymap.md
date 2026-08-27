# The map

How a project becomes a town, how a file becomes a building, and how the map
reports its own loading.

## How a holding is sized

Every file on the map is a building, and its dimensions are measured from the
file itself. The two axes carry two different facts.

**The base is how much code there is.** A folder's ground is divided among its
contents by weight — `20 + √bytes` for a file — so a big file gets a visibly
bigger plot than its neighbours: 64 KB buys about five times the lot area of
1 KB. What stays fixed is the *average* plot per file (`LAND_PER_FILE`), which
is why a 2,000-file repository grows a larger island rather than cramming the
same ground into smaller houses. One consequence worth knowing: a plot is a
*share of its folder*, so the same file looks bigger among small siblings than
among large ones.

**The height is how tangled it is** — branches per line of code, not length.
Length is already the base, and measured across this repository lines and total
branch count correlate at 0.94, so putting both on the map would draw one fact
twice; branch *density* is independent of size (0.06). A long plain file is
therefore a broad low hall, and a short knotty one stands up. The result is
clamped to 11–54 units, then capped at ~1.9× the footprint's shorter side so
nothing towers over the neighbours it would hide under the fixed camera angle.

**Everything else is category.** What a file is *for* fixes its archetype and
colour, identically in every repository — a test is a watchtower, docs are a
scriptorium, config is a council hall. Branch density also adds chimneys,
crates and forge stacks, so a complex file looks busy as well as tall.

### What "complexity" actually means

It is a **crude branch count**, not a claim about code quality. The scanner
counts occurrences of ten tokens — `if`, `for`, `while`, `match`, `switch`,
`catch`, `case`, `else`, `&&`, `||` — and divides by the lines of code. Its
limits are worth stating plainly:

- It is **substring matching, not parsing**. A `&&` inside a string literal
  counts. Tokens are space-padded, so `else` will not match inside an
  identifier, but that is the extent of the cleverness.
- **Comments and blank lines are excluded** from both the count and the
  divisor. They have to be: this crate's own `lib.rs` is 155 lines of which 98
  are prose, and phrases like "for exactly this reason" would otherwise score
  as branches and make the most heavily documented file look like the most
  tangled one.
- **Only code is scored.** Documentation, configuration, data and assets always
  score zero and sit at the minimum height by construction.
- Files over 2 MB and non-text files are never opened, so they also come out at
  the floor.

Treat a tall building as "this file has a lot of decisions per line, take a
look", not as a metric to optimise. The real rules live in
`crates/kingdom-citymap/src/build/layout.rs`.

## How a change is sized

A holding is sized from the file. The **works** — what a plan is proposing — are
sized from the change, and they are a separate ruler: a column of the agent's
colour above the roof for lines added, a shroud over the house for lines removed.
`crates/kingdom-citymap/src/engine/works.rs` draws them.

One number does most of the work. `magnitude(churn)` turns a count of lines into
`0.0..=1.0`, and height, girth, the pulse's brightness and the removal skirt all
read it. **It has been wrong twice, in opposite directions**, and both are worth
knowing before touching it.

It was first **relative** — a share of the busiest file in the same plan. That
made two agents incomparable, since each was measured against its own plan's
ruler, and drew a plan that touched one file at exactly the height of a plan that
rewrote four thousand lines. `tasks/00250` replaced it with an absolute count,
and that part stands: the input is lines, and nothing local to a plan is
consulted.

It was then **logarithmic**, `ln1p(churn) / ln1p(600)`, and the King reported the
result: a `+8` looked about the same size as a `+100`. Four faults in one curve.
`ln1p` rises fastest near zero, so a one-line edit already took 11% of the range
and an eight-line one took 34% — the bottom third of the scale went to changes
that had barely happened. What was left had no resolution where changes actually
live: measured over 400 commits of this repository, per-file added lines run p25
= 6, median = 27, p75 = 115, p90 = 246, p99 = 935, and across the middle of that
the curve moved 1.37×. The clamp at 600 was a plateau on real work, drawing p99
and a 3,872-line rewrite identically. And girth, added specifically to widen the
range, multiplied *the same compressed number*, so it was the first channel
restated rather than a second one.

What is drawn now is a **saturating ratio**, `churn / (churn + HALF_CHURN)`:

- near zero it is very nearly linear, so small changes differ in proportion to
  their size instead of all being lifted to one stub;
- it never plateaus, so 935 lines and 3,872 stay different marks;
- its knee can be put where the data is. `HALF_CHURN` is 110 — close to this
  repository's p75 — so the steep part covers p25 to p90 and the flattening
  happens out among the rewrites, where *very large* is a good enough answer.

Girth ramps with the **square root** of that, which is what makes it a genuine
second channel: height carries the proportionality and girth keeps a small change
wide enough to resolve in the rail's pane. The removal skirt uses the same
gentler curve, so making the low end honest did not undo `tasks/00260`'s fix for
invisible removals.

| | +100 vs +8 | p75 vs median | +4000 vs +600 |
|---|---|---|---|
| height, before | 1.91× | 1.37× | 1.00× |
| height, now | 4.09× | 2.20× | 1.14× |
| height × girth, now | 6.28× | 2.80× | 1.19× |

The trade made deliberately: the very top of the range is compressed, so a
1,000-line change and a 4,000-line one come out nearly alike. Both mean *very
large*, and telling `+8` from `+100` does not.

The shroud is a different question and has its own scale — *how much of this file
is going away* is a ratio of the file's own length, not of churn. See
`tasks/00260`.

## Wells and networks: what each agent is plugged into

A holding is sized from the file and the works from the change. These are a
third ruler again, and they answer the **second** of the three questions in
`AGENTS.md` — *what shared resources are they holding?* Three marks, drawn by
`crates/kingdom-citymap/src/map/network.rs` (the geometry, and every judgement
behind it) and `engine/network.rs` (the meshes).

| Mark | What it is | Where it stands |
|---|---|---|
| the **host ring** | the King's own machine | a slate band just inside the realm's rim |
| a **wellhead** | one container a whole city shares | built on that town's square |
| an **agent mark** | one live plan, in its own banner colour | ringing the town it works in |

And the lines between them: a **conduit** from an agent out to the host ring
means it binds the King's own ports; a closed **moat** around an agent and *no*
conduit means it has a network of its own; a **channel** to a wellhead means it
is actually drawing from that well.

### A well is built, not marked

Everything else in that table is *interface drawn in world space*: a colour that
means "this agent", "this is your machine". All of it is unlit, and
`engine::activity::WORKING_COLOR` records the three measurements that established
why such a colour cannot be a lit material.

A wellhead is the one exception. It is not a colour that means something — it is
a thing standing on a town's square among lit houses — so it is a stone drum
with water in it and a timber canopy over it, `Surface::Matte` like the
buildings, taking the sun and casting a shadow. The canopy shows from the
`Architecture` tier only; the drum carries "a shared thing stands here" at every
zoom.

Drawn unlit it had been the only object in the settlement with no shading and no
shadow, which is the definition of a light source, and it read as exactly that:
a pale disc that looked switched on.

### Only a *project's* wells are drawn

A shared resource can also be declared at the King's own machine, in
`~/.kingdom/services.toml`, and reached by every project he opens — see
[`shared-resources.md`](shared-resources.md). Those are **not** on the map.

A wellhead stands on a town's square, and a machine-wide well belongs to no
town: drawn from `services::running_in` unfiltered, one Redis would appear once
in every city that had an agent in it, the same container claimed by three
projects that do not own it. So `api::kingdom_network` filters on scope, and a
test in `services.rs` pins the fact that makes the filter necessary.

The honest home for one is the **host ring** — it already draws the King's
machine, which is precisely what such a well belongs to. Putting it there is its
own piece of work rather than a line in this one, because it needs a mark that
reads as "shared by everything" without looking like a city's, and channels from
agents in several different towns converging on the rim.

### The one fact this exists to draw

**An isolated agent still reaches its city's well.** That is the fact nothing
else in the interface says, and it is not obvious — it looks like a
contradiction. `slirp4netns` runs with `--disable-host-loopback`, which blocks
`127.0.0.1` and *nothing else*, so a Docker bridge address is simply another
host route. The picture that says it is a moat with no conduit to the rim and a
channel to the wellhead anyway. `services.rs` records the measurements.

### Why it is not one mark per port

A conduit says the agent *has* a network, not what it is listening on. Ports
move constantly; what an agent is plugged into does not. The chamber's ports
badge reports the numbers, fed by the per-plan socket — see
`watch_kingdom_network`, whose digest deliberately excludes them.

### What is drawn from what an agent *did*, not what it could

`drawing_from` names the wells a plan is registered as using, from the same
reference set that decides when a container is stopped. Every plan in a city
*could* reach its database; drawing a channel from all of them would claim five
connections where there is one.

### Four things settled by looking at it

The render of this was wrong in four ways that no test caught, and each is now
pinned by one. The fourth came a version later than the others, from the King:

- **the well was the same colour as an agent.** `#38bdf8` sits 110.5 from the
  `azure` banner on the palette's own ruler, where the two closest banners are
  126.1 apart — so an azure agent's conduit and the well it ran to were one
  colour. The test now asserts a *margin*, not inequality, which is what let the
  first attempt pass.
- **the conduits were roads.** At 3.4 units they were heavier than the streets
  they crossed. A connection is a thread; the marks carry the meaning.
- **the agents stood among the buildings** — one on the keep in the middle of
  the square. They are now placed from the town's own `extent`, so they ring the
  settlement rather than standing in it.
- **and so did the well.** The same fault as the one above it, unnoticed because
  the code *said* it stood on the square: `well_stand` placed it at the town's
  centre **point**, which is not the square. `streets::square_site` walks a
  square outward from the settlement's middle until it finds ground no ward has
  claimed — the middle being the one place the largest folder has already taken
  — and on a real kingdom of seven towns that walk landed between 94 and 1,622
  units out, against a square 52 across. So every well on the map stood among
  the houses. `MapPlaza` now carries its town's name and `MapManifest::square_of`
  finds it, which is also what finally makes the documented rule *a town with no
  square gets no wellhead* true rather than merely written down.

Worth stating plainly, since it is now the fourth: a test suite that passes is
not a picture that reads. Twelve tests were green while the well was standing in
somebody's garden, because every one of them measured the well against the
placement rule instead of against the town.

## Reporting progress while the map loads

**The map says how far along it is, and had to be rebuilt to be able to.** The
loading card named its two phases and animated three towers; it reported no
progress, because there was none to be had. Both phases now carry a real bar,
and the second one is the whole of the work.

The fetch was the easy half: `view::read_whole` reads the body a chunk at a time
instead of calling `Response::json`, and counts bytes against `content-length`.
`progress::Transfer` turns that into a fraction and a line — *2.1 MB of 4.2 MB*.
It refuses to answer rather than guessing when the two numbers cannot be
compared: no declared length, or a count that has passed it. That second case is
not hypothetical — putting compression in front of `/kingdom/map.json` would
make `content-length` the compressed size while the reader counts decompressed
bytes, and a bar reading 640% is worse than one that admits it does not know.

The raise is the half that mattered. **A bar over `spawn_world` could not have
moved at all**: measured on a real dev folder, building ~20k entities held the
main thread in blocks of 1.3–1.7 seconds, and a sampling loop started at
navigation did not get its first turn until t≈10.9 s. Nothing repaints in there
— which is exactly why the card's SCSS already insisted every animation on it be
`transform`/`opacity`, since those composite off the main thread. A *number* has
no such escape. So `engine::raise` cuts the build into slices against an 8 ms
deadline, publishes the fraction to the bridge between them, and lets the frame
go. The bar moves because the browser is free to draw it.

Five things there are load-bearing. `spawn.rs`'s stages take a `Range` and
`spawn_step` is the one door both paths go through, so the sliced build and
`spawn_world` cannot raise different settlements. The root is spawned
**hidden** and revealed on the last slice: the card is a translucent gradient
rather than an opaque screen, and scenery spawns visible and is only culled when
`apply_lod` next runs — so the King would otherwise watch trees appear and then
vanish. `ActiveLod` and `Activity` are **marked changed** at completion, because
both systems early-return unless their resource moved and every entity of the
new world was spawned after the last change. Winit is held at the watching pace
while a raise is in flight, since walking into a chamber mid-raise would
otherwise drop the engine to four ticks a second and turn three seconds into a
minute — `winit_for` is one definition so the two systems cannot disagree. And
`built` still means *standing*, so the card's dismissal and the bridge's test
for it are untouched.

The weights in `Step::weight` are **estimates** and the design assumes so:
being wrong makes the bar advance unevenly and nothing else, and `fraction`
reaches exactly 1.0 whatever they say. `status_matches` compares the fraction
within 0.5% for the reason it already tolerates sub-pixel camera drift — tens of
thousands of entities against a bar a few hundred pixels wide — but starting and
finishing are always heard, because `None` means *nothing is going up* and
rounding that together with a build at 0% would leave the bar indeterminate for
the whole first slice.

What is deliberately **not** done here is compressing that route. It would cut
4.35 MB to ~694 KB and help the fetch more than any bar does, but it is a server
change with its own trade-offs and it is what `Transfer`'s clamp is written to
survive.

**And then the King reported it as an indeterminate bar that goes forever — and
he was right about what reached the screen.** Measured in a browser against a
running server, the card painted **three** distinct fractions in five seconds,
with an indeterminate sweep at each end of them. Two faults, both invisible from
the code that computes the numbers.

The first was composition. Each phase was asked for its own fraction and the bar
swept whenever the one asked had none — and the gap between the two phases,
commented as lasting "a frame", measured up to **1,430 ms**, because the engine
was still booting and the `Load` sat in the bridge queue. `progress::Wait` now
composes them: the fetch fills the first `FETCH_SHARE`, the raise fills the
rest, a phase that has not begun holds the bar where the last one left it, and
the sweep is kept only for a fetch whose length the server never declared.

The second was that the readings were **published and not painted**. Recording
frame timestamps inside the map page found single frames of **2,694 ms** while
`FRAME_BUDGET` was 8 ms — because that budget bounds the wrong half. Issuing a
spawn and adding a mesh is cheap; applying the commands and preparing the meshes
for the GPU is not, and all of it happens after the deadline is checked for the
last time. So `raise` now adapts: `Job::allowance` is what a frame may spend, in
`Step::weight` units, halved or doubled from what the *whole previous frame*
cost. Weighed rather than counted, because two thousand trees and two thousand
folder names are not the same frame. Alongside it, the engine no longer renders
a world nobody can see — the camera is off for the length of the raise — the
engine boots when the manifest lands rather than on mount (which alone took the
fetch from 3.3 s to ~60 ms), and the reveal, the single most expensive frame in
the app at 2,835 ms, waits behind a bar the King has already seen reach the end.

25 painted readings, against three. `tasks/00250` has the tables.

## Looking at the map from a plan's browser

A plan's browser stands the map down by default (`mode.rs`) — most plans never
want it, and booting Bevy is not free. Ask for it with `?map=on`:

```
browser_navigate  http://127.0.0.1:3000/?map=on
```

That is now sufficient on its own; there is no environment variable to set
first. The world takes a few seconds to stand, so **wait on a value rather than
sleeping** — and assert on values afterwards too, rather than on pixels:

```js
__kingdom_map.built             // false until the world is up
__kingdom_map.hovered           // "src/main.rs", after a mouse move
__kingdom_map.clicked.holding   // what the last click actually hit
```

`window.__kingdom_map` is defined only under automation (`view::publish_status`)
and carries the whole of `ViewerStatus`. Prefer it to a screenshot: it is stable
against every change to how the map is *drawn*, and it names what was hit rather
than leaving a reader to recognise it in an image.

### A plan with a network of its own cannot reach *your* Kingdom

The URL above is the one served by the Kingdom you are reading this in — on the
host's `127.0.0.1:3000`. A plan opened with a **network of its own** has a
different `127.0.0.1` and slirp4netns runs with `--disable-host-loopback`, so
that address answers nothing there. Measured: `000` from inside the namespace
against `200` from the host.

That is isolation working, not a bug — reaching back into the host's loopback is
the collision the namespace exists to prevent. But it means the recipe above is
for a **shared-network** plan, which is the default. A plan with its own network
should start its own server and browse to *that* `:3000`, which is the ordinary
case and works normally.

WebGL itself is unaffected by the namespace: SwiftShader is pure CPU rendering
and needs no network. Verified with a real Chrome inside one — `ANGLE (Google,
Vulkan 1.3.0 (SwiftShader Device ...))`, with every child process including the
GPU process confined to the same four CPUs.

One caveat before you aim: at the default zoom the whole kingdom is in frame, so
a single holding is a few pixels. Move the pointer and read `hovered` back
rather than trusting a coordinate to have landed.

### Why it costs what it does

A headless browser has no GPU — on WSL2, and on most CI, every hardware path
(`--use-angle=vulkan`, `=gl`, `--use-gl=egl`) yields **no WebGL context at
all**. SwiftShader on the CPU is the only renderer there is, and `--disable-gpu`
never had anything to do with that: it turns off *hardware* acceleration, which
such a machine did not have to begin with.

Measured on this map, world standing, nothing happening:

| | Cost |
|---|---|
| uncapped, unconfined | 9.50 cores |
| one frame a second, unconfined | 4.09 cores |
| capped and confined to four CPUs | 2.03 cores |

The middle row is the one that matters, and it is why pacing alone was not the
answer: SwiftShader sizes its thread pool from the machine and spends most of
what it spends whether or not a frame was asked for. So the engine cuts the
frames (`engine::AUTOMATED_WAKE`) and the browser cuts the floor beneath them
(`session::CPUS_VAR`, default 4). Both are needed.

Bounded work is exempt from the frame cap, and that exception is load-bearing:
capping a world going up turned a three-second raise into **157 seconds**. The
machine did no less work — it simply spread it over fifty times the wall clock,
with something waiting on it. See `engine::Pace::set_for_work`.
