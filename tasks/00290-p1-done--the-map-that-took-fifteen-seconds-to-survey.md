# The map that took fifteen seconds to survey

The King asked why the map had become slow to load, and the answer was not
subtle once anything was measured. Against his own dev folder
(`/home/omarchy/dev` -- 7 cities, 3,080 holdings), on a server built the way
`cargo leptos serve` builds it:

```
first  GET /kingdom/map.json   15.19 s
second GET /kingdom/map.json    0.008 s   (memoised)
```

Fifteen seconds before a single byte reached the browser, and then another seven
while the world went up. `citymap.rs` said the build took "about 1.7 seconds"
and meant it -- that figure was taken on a release build, and nobody runs one.

## Where it went

Two causes, and they multiply.

**`kingdom-citymap` was compiled at `opt-level = 0`.** The workspace already
knows this trap: `Cargo.toml` carries an entry for `syntect` and the note that
tokenising one file "took 5.4s at opt-level 0 and 350ms released... long enough
to make opening a file feel broken". The map builder is the same shape of code
-- a gitignore walk, a grid pathfinder, a Poisson sampler -- and nobody had
spotted that it was in the same position. Building the **unmodified** builder
both ways over the same folder:

|  | opt-level 0 | released |
|---|---|---|
| scan | 2,575 ms | 301 ms |
| scene | 13,114 ms | 1,136 ms |
| **total** | **15.9 s** | **1.5 s** |

15.9 s reproduces the 15.19 s the live server took, which is what confirmed
there was nothing else hiding behind it.

**The road planner was an exhaustive Dijkstra over a 2.8-million-cell grid.**
`Ground::route` searched `(cell, heading)` with no heuristic, so it fanned out
in every direction until it happened to meet the goal -- and re-allocated two
`Vec`s of `cells * 4` entries, 11 million elements, for *every road it planned*.
Six highways on this kingdom; three of them explored a quarter to a half of
every state in the grid:

```
highway grid: 1,815 x 1,525 = 2,767,875 cells
  route 0 -> 6 : 373 ms   (4,665,548 pops -- 42% of all states)
  route 0 -> 5 : 201 ms   (2,647,870 pops)
  route 0 -> 2 : 189 ms   (2,480,354 pops)
```

The goal is a known point on a uniform grid. The search was declining a
heuristic it was entitled to.

## What was built

**One line of `Cargo.toml`.** `[profile.dev.package.kingdom-citymap]` at
`opt-level = 3`, beside the `syntect` group and for the same reason, with the
measurement in the comment as that block does. `ignore`, `globset` and `memchr`
were tried alongside it and made no difference -- this crate's own code was the
whole cost. Measured by touching `lib.rs` and rebuilding, the entry costs
nothing in compile time either way: 2 s at opt-level 0, 2 s at 3.

**A\* in `Ground::route`.** The frontier is ordered by `cost + estimate` rather
than by distance travelled, and the visited sets are maps rather than dense
arrays -- A\* reaches a few hundred thousand states out of eleven million, so
the array was tens of megabytes cleared per road to hold a frontier that never
filled it.

`Ground::estimate` is Manhattan distance plus the turns still owed: one when the
goal is off both axes, one when the heading points away from it or runs along an
axis already satisfied, each charged at `TURN_COST`, and zero at the goal. Both
terms are deliberately the cheapest thing that could still be true, because A\*
returns the same answer as an exhaustive search only while the estimate never
overstates what remains.

## What it costs now

| | before | after |
|---|---|---|
| scan | 2,575 ms | 335 ms |
| scene | 13,114 ms | 438 ms |
| -- of which highways | ~11 s | 163 ms |
| **first `map.json`, real server** | **15.19 s** | **1.48 s** |

The browser's raise is unchanged at ~6 s and is now the honest majority of the
wait. Nothing was done to it: there is no *before* to compare it against, so
there is no evidence it regressed, and guessing at it would have muddied a
change that is otherwise provably invisible.

## Why the test is a comparison and not an assertion

This is a speed change that must not be a map change, and that is a hard thing
to check by looking. **An inadmissible heuristic still returns a route** -- just
a worse one, a few turns longer -- and every road test in this file asks only
whether the network is one piece, reaches every ward and crosses no holding. All
of them pass just as happily on a road that did not need to bend.

So the old search is kept as `Ground::route_exhaustive` under `#[cfg(test)]`,
and `the_heuristic_never_changes_a_route` compares the two over 3,000 randomly
generated grids -- random size, random obstacles, random endpoints. Random
rather than the fixture repository because the cases that break an A\* are
awkward geometry, and three thousand random boards find those far more reliably
than a scene somebody chose.

It was checked against being vacuous the only way that means anything: making
the heuristic inadmissible on purpose (multiplying it by three, the classic A\*
bug) fails it on the first case.

Belt and braces, the same property was confirmed end to end before the test was
written -- every road dumped from the old and new builders across three
kingdoms, compared byte for byte:

```
/home/omarchy/dev                  4,342 roads   IDENTICAL
~/.kingdom/realms/crowded          2,657 roads   IDENTICAL
~/.kingdom/realms/kingdom-mirror                 IDENTICAL
```

## Left alone, deliberately

Three things were found along the way and are each their own decision:

- `Ground::new` clamps its cell to `lane * 0.5`, and `highways` passes
  `WARD_GAP` (15) although towns are packed `TOWN_GAP` (24) apart. The real gap
  would quarter the grid -- but it moves roads, so it wants the King's eye
  rather than a speed argument.
- Scenery is 11,702 trees: 1.5 MB of the 4.4 MB manifest, and the largest item
  count in the browser's raise. Thinning it is a decision about how the map
  looks.
- Building the manifest when a kingdom is opened, so the first map view finds it
  warm. Worth doing -- but the two fixes above make the thing it would hide
  cheap enough that it may no longer be wanted.
