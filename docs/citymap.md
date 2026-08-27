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
