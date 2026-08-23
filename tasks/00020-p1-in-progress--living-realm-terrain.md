# The Living Realm: one continuous land instead of cities in a void

Replace the phyllotactic spiral and the flat black backdrop with a **single
continuous isometric landmass** — a coastline, provinces, roads, and settlements
that grew where they grew. The kingdom should read as a place you could walk
across, not as a scatter plot of dioramas.

The per-city skyline stays. It is the part that works, and it is well tested.
This task fixes the world *around* and *between* the cities.

---

## Root cause: why it feels disjointed

This is not a taste problem. There are five concrete causes, and two of them are
outright bugs.

### 1. The kingdom and the cities are drawn in two different projections

This is the big one.

`CityGlyph` renders its skyline through `iso(x, y, z)` — a true 30° isometric.
But its *position* comes from `spiral_layout`, which is a flat top-down
scatter, applied as a plain `translate(place.x, place.y)`.

So every city is a little 3D diorama pinned to a 2D wall. The cities recede in
depth internally; the world holding them does not recede at all. The eye reads
that mismatch instantly and calls it "floating" or "disjointed", even when it
cannot say why.

**Fix:** put the kingdom itself on the isometric ground plane. Layout produces
ground coordinates; the map projects them with the *same* `iso()` the buildings
use. One world, one projection, one horizon.

### 2. The background is not part of the world — bug

`map/mod.rs`:

```rust
<rect x="-6000" ... fill="url(#terrain)"/>   // outside the transform

<g transform=transform>                       // everything real is inside
```

The grid pattern sits **outside** the pan/zoom group. Drag the map and the
cities slide across a stationary grid. Nothing destroys the illusion of a place
faster than ground that does not move with the things standing on it. Right now
the backdrop reads as a UI panel, not as terrain.

### 3. Adding a project relocates the entire kingdom — bug

`scan.rs` sorts cities by name; `spiral_layout` places them by **list index**:

```rust
.enumerate().map(|(i, city)| { let index = i as f64 + 0.5; ... })
```

So creating a project called `aardvark` shifts every alphabetically-later city
into a different spiral slot. The whole map rearranges.

This directly violates the reason layout lives in `kingdom-core` at all. The
module's own doc comment says placement must be stable *"or the King loses the
spatial memory that makes a map worth having"* — and `spiral_layout_is_
deterministic` does not catch it, because it only ever compares the same list to
itself. Determinism was tested; **stability under insertion** was not.

### 4. The spiral looks like a spiral

Golden-angle phyllotaxis is designed to distribute points *uniformly*. Uniform
is the opposite of natural. Every city ends up the same distance from its
neighbours, arranged in faintly visible rings, with no clusters, no valleys, no
empty quarter. Real settlement is lumpy: towns crowd a river and leave the
highlands bare.

Worse, the spiral is **semantically empty**. Position encodes nothing but
alphabetical order. A map where location means nothing is a map that cannot be
read, only looked at.

### 5. Nothing connects anything

No roads, no rivers, no shared ground. Each city is a diamond plot on black.
"Disjointed" is the literal, accurate description: the cities are, in fact,
disjoint.

---

## What replaces it

A continent, settled.

```text
            ~ ~ ~ ~ ~ ~ ~ ~ ~ ~ ~ ~ ~ ~ ~ ~ ~ ~ ~ ~
         ~ ~ . : ' ' : . _ _ . : ' ' : . ~ ~ ~ ~ ~
       ~ ~ :   ▲▲  RUST HIGHLANDS   ' : . ~ ~ ~
     ~ ~ '   ▲███▲ ─────────── road ──── ▲██▲  : ~ ~
    ~ ~ :    kingdom-ide            grepwarden   ' ~ ~
     ~ ~ ' .        ╲                    ╱     . ' ~ ~
      ~ ~ ~ ' : . ▲███▲ ── road ── ▲██▲ . ' ~ ~ ~
        ~ ~ ~ ~   NODE COAST            ~ ~ ~ ~
            ~ ~ ~ ~ ~ ~ ~ ~ ~ ~ ~ ~ ~ ~ ~
```

- **Coastline.** The kingdom is an island with a real, irregular shore. Land
  ends; sea begins. That single boundary is what turns a void into a world.
- **Elevation.** Low coastal ground, inland hills, a high seat for the throne.
  Cities sit *on* the terrain at its height, not on a separate plane.
- **Provinces.** Cities of the same stack settle near each other, so the kingdom
  develops a Rust highland and a Node coast. Position starts meaning something.
- **Roads.** Every city is joined to its neighbours by a road network. No city
  is an island — provably, by test.
- **One projection.** Terrain, roads and skylines all go through `iso()`.

### On keeping isometric

You said not to feel stuck with it. I want to keep it, and the reason is
that the current problem is *too little* isometric, not too much: the cities are
iso and the world is flat, and abandoning iso would make the map **more**
abstract, not less. Sim City and Age of Empires are isometric for exactly this
reason. The fix is to commit to it all the way up to the horizon.

It also preserves the tested skyline work — `buildings_are_painted_back_to_
front`, the non-overlap invariant, the treemap — instead of discarding 1,200
lines of working, verified geometry.

If after seeing it the projection still feels wrong, swapping `iso()` for
another projection becomes a one-function change once everything routes through
it. That is an argument for unifying now, whichever projection wins.

---

## Design decisions

**1. Placement is keyed on city identity, not list index.**

Each city hashes its own `CityId` to a candidate spot; overlaps are then
resolved by local relaxation. Adding a project perturbs its immediate
neighbours at most, never the whole map. This is the fix for defect #3 and it is
the single most valuable part of this task — it is what makes spatial memory
real rather than aspirational.

**2. Terrain is seeded by the kingdom, never by its contents.**

The continent's shape comes from a hash of the kingdom path alone. Scan a folder
today and in six months and the coastline is identical, however many projects
came and went. A world that reshapes itself when you `cargo new` is not a world.

**3. Terrain is deliberately decorative, and must stay quiet.**

`AGENTS.md` is strict that colour on screen should mean something. Elevation
noise means nothing, and I am not going to pretend otherwise. It earns its place
as *substrate*: it exists so the meaningful things read as objects in a place.

The discipline that keeps that honest: terrain uses only desaturated blues and
slates from the existing palette, no terrain colour may approach a `Ward::tint()`
or a status colour, and land contrast stays below the faintest city element. If
the terrain ever competes with a gilded roof for attention, the terrain is wrong.

Province clustering, by contrast, **is** meaningful — it shows the kingdom's
composition at a glance, which is a question the King actually has.

**4. Cities must be painted back to front — new requirement.**

Once cities sit on an iso plane at varying elevation, a near city must overdraw a
far one. `Cities` currently renders in list (alphabetical) order, which was
harmless on a flat plane and will look broken on a landscape. Same painter's
reasoning as `order_for_painting`, one level up. Sorting the entries before the
`<For>` keeps keyed reactivity intact.

**5. Noise is hand-rolled, integer-hashed, dependency-free.**

`kingdom-core` must compile to wasm and carries no deps but `serde`. A ~40-line
value-noise fBm over wrapping integer hashes is deterministic on every platform
and keeps it that way.

---

## Implementation plan

Five stages. Each is independently shippable and independently reviewable; if
the budget runs out after stage 2 the map is still better than it is today.

### Stage 1 — One world, one projection *(and the feel bugs)*

Small, high payoff, no new maths.

- Project city positions through `iso()` so the kingdom shares the ground plane.
- Move the backdrop **inside** the transform group (defect #2).
- Sort cities back-to-front by ground depth before rendering (decision 4).
- **Cursor-anchored zoom.** `on_wheel` currently scales about the world origin,
  so content slides out from under the pointer. Anchor to the cursor.
- **1:1 pan.** Dragging adds screen pixels straight into a `viewBox`-unit
  translate, so the map moves at `viewport_px / bounds.width()` of cursor speed —
  roughly 0.7× in a mid-size kingdom, and different in every kingdom. Convert
  through the viewBox scale so the land sticks to the pointer.

Those last two are why panning currently feels like dragging a picture of a map
rather than a map.

### Stage 2 — `kingdom-core/src/terrain.rs` (new, pure, wasm-safe)

```rust
pub struct Terrain { seed: u64, /* … */ }

impl Terrain {
    pub fn for_kingdom(path: &str, extent: f64) -> Terrain;
    /// Ground height at a point; negative is sea.
    pub fn elevation(&self, x: f64, y: f64) -> f64;
    pub fn is_land(&self, x: f64, y: f64) -> bool;
}

/// Iso-ready contour rings for each elevation band.
pub fn contours(t: &Terrain, bands: &[f64], res: usize) -> Vec<Band>;
```

fBm value noise, multiplied by a radial falloff so the landmass closes into an
island instead of running off the viewport. Coastline and elevation bands come
out of **marching squares** at 4–5 thresholds, which yields a handful of `<path>`
elements rather than thousands of tiles — organic shape, trivial DOM cost.

> **Risk to measure:** a 128×128 sample grid is ~16k noise evaluations per
> kingdom. Computed once in a `Memo`, that should be microseconds, but I will
> time it in wasm and drop the resolution if it shows up on first paint.

### Stage 3 — `kingdom-core/src/layout.rs` (rewritten)

`spiral_layout` → `settle_kingdom`:

1. **Province seeds.** Group cities by `CityKind`; give each group a stable
   region of the continent, ordered so the kingdom's dominant stack takes the
   heartland.
2. **Identity-hashed candidates.** Each city hashes `CityId` → a jittered
   position within its province.
3. **Constrain to land.** A candidate in the sea walks uphill to the nearest
   coast. No city ever sits in water.
4. **Local relaxation.** A few fixed iterations pushing overlapping pairs apart,
   preserving the non-overlap guarantee the skyline depends on.
5. Attach `elevation` to `CityPlacement`.

Step 4 is capped at a fixed iteration count, so it stays pure and terminating.

### Stage 4 — Roads

A Euclidean minimum spanning tree over city positions, plus a few extra short
edges so the network reads as a web rather than a tree. Drawn on the ground
plane in `iso()`, beneath the cities.

Roads are also where contention threads belong: a red pulse should travel *along
the road* between two cities fighting over a port, instead of cutting across
country. That makes the map's most important signal read as traffic on a real
network — and it is the direct answer to "disjointed".

### Stage 5 — Camera and level of detail

LOD stays exactly as designed — three tiers keyed on *apparent* city size, which
is already the right call — renamed to match the new world:

| Tier | Was | Shows |
|---|---|---|
| `Realm` | `Distant` | Continent, provinces, roads, city silhouettes |
| `Province` | `Districts` | District plates, landmarks, city names |
| `Streets` | `Full` | The full skyline |

Plus the thing that makes zoom feel like travel rather than scaling:

- **Click a city → the camera eases to it** and settles at `Streets` detail.
  Clicking empty land eases back out to `Realm`. A short cubic ease on the
  viewport signal; no new dependency.
- Terrain detail follows the tier too: contour bands at `Realm`, plus shoreline
  texture close in.

`bounds_of` must grow to include the continent, or the sea gets clipped at the
viewport edge and the island looks cropped.

---

## Tests

Four, in `kingdom-core`. Each pins something the King would notice breaking, in
the spirit of the existing set. Stated deltas from today's tests:

1. **`adding_a_city_does_not_move_the_others`** — *new, and the important one.*
   Lay out N cities, insert one that sorts first, assert every original city is
   within a small epsilon of where it was. This is the regression test for
   defect #3, and it covers the gap that `spiral_layout_is_deterministic` left
   open by only ever comparing a list to itself.

2. **`cities_never_overlap`** — *carried over*, adapted to `settle_kingdom`. Still
   load-bearing: it is what extends transitively to `buildings_stay_inside_their_
   city`, so the whole skyline invariant rests on it.

3. **`every_city_stands_on_land`** — *new.* No placement may sit below sea level.
   A city floating in the ocean reads as broken software, and step 3 of the
   settle algorithm is the only thing preventing it.

4. **`every_city_is_reachable_by_road`** — *new.* Union-find over the road
   network: one connected component. This is the literal, checkable form of the
   complaint that started this task.

Deliberately **not** tested: noise values, contour vertex counts, iso arithmetic,
province colours, easing curves. Those restate the implementation. Clustering
quality is also left untested — any threshold I picked would be arbitrary and
flaky; it is a judgement to make by looking.

---

## Verification

- `cargo test -p kingdom-core`, `cargo fmt`, `cargo clippy` clean.
- `cargo leptos serve` against a real dev folder.
- **Measure the DOM, do not eyeball it** — the habit that paid off on the
  skyline. Read back rendered geometry and check that no city polygon intersects
  a sea band, and that city draw order matches ground depth for every pair.
- Check the insertion invariant *in the browser*, not just in a unit test: scan a
  folder, add a project that sorts first, rescan, confirm the map does not
  rearrange.
- Confirm at 40+ cities that pan/zoom stays smooth and each tier switches cleanly.
- Take screenshots at all three tiers and actually look at them.

## Out of scope

Rivers and forests (revisit once the coastline is in — they may be gilding).
WebSocket live updates, the real lease broker, persistence, file editing. No
change to scanning, to the domain model beyond `CityPlacement::elevation`, or to
the skyline geometry itself.
