# A building twice the size for twice the content

The King reported it plainly: building sizes "all fall off with a saturation or
a scaling value". He was right, and he named the fix — make it linear, for text
files, scaled to **area**, so a building with twice as much content covers twice
the ground.

## What was actually wrong

Three separate things each flattened the relationship, and all three had to go.

**1. The weight was a square root.** `build::layout::weight` was
`20.0 + √bytes` for a file. A square root *is* the fall-off: 64 KB earned about
five times the lot of 1 KB rather than sixty-four.

**2. A folder weighed something else entirely.** The directory arm was
`file_count * 16 + √bytes` — a different formula from the file arm, so what a
folder was *given* and what it had to *hand out* were unrelated numbers, and the
proportion leaked away at every level of the recursion. This is the one that
mattered most: without it no amount of fixing the file arm survives the descent.

**3. The house was jittered on top.** `Building::footprint` rolled its occupancy
from a hash, `0.46..0.58` on each side. That is up to **1.55× of area**, noise
laid directly over the signal.

### Measured, on this repository's own files

The scanner was run over this checkout and every holding's house area compared
against its line count.

| | before | after |
|---|---|---|
| holdings within ±43% of proportional-to-lines | **33%** | **87%** |
| … within ±25% | 23% | 69% |
| spread, p10 → p90 | 0.20 → 2.59 | 0.70 → 1.15 |
| worst over-served | 15.2× | 8.8× |

The King's own case, stated as he stated it: a file with exactly twice a
neighbour's content drew a median **0.66×** the area. Less than the smaller
file's own share — not two thirds of *twice*, but two thirds of *one*.

## What changed

All three, in `build/layout.rs`:

- `weight` is linear in `metrics.lines`, and a folder is the **sum of its
  children** rather than a formula of its own.
- Two floors, because the rule has to stay drawable. `MIN_HOLDING_LINES` (60)
  stops a three-line `mod.rs` earning a house under a world unit across;
  `UNREAD_HOLDING_LINES` (120) gives a nominal lot to files the scanner never
  opened, whose line count is zero because nobody counted rather than because
  they are empty. Sizing those by bytes would hand one 3.5 MB `.min.js` a
  quarter of the town — and the King asked for linear *for text files*.
- `MIN_LOT_THICKNESS` (22 units) replaces the old `clamp(0.08, 0.92)` on the
  split ratio. The clamp's job was to prevent slivers, but as a *share* it
  charged a large cell 8% of its ground for its smallest file however small
  that file was. As land it costs what it has to and no more.
- `footprint` covers a constant **area** share (`LOT_COVERAGE`), with the hash
  varying only aspect and position. The street looks as varied as it did; two
  houses on equal lots now cover equal ground.

## Two real bugs this uncovered

Both were latent, both were **masked by fat lots**, and both are the reason this
took longer than the layout change itself. `√bytes` left even the smallest lot
big enough that nothing ever came close enough to trigger them. Sizing lots by
content puts genuinely small houses next to avenues, and they fired at once.

**A road widened by the diagonal instead of the worse axis.** `route_clearance`
asked `box_gap` for the distance to the nearest house and got
`horizontal.hypot(vertical)`. But a road widens along *both* axes at once — half
its width is added to every side of its run — so a house sitting diagonally off
the end of a segment only permits the **larger** of the two gaps. The diagonal is
up to √2 too generous. In the busy fixture: gaps of 8.9 and 13.7, a diagonal of
16.3, a road granted 31.7 wide, and 2.15 units of paving through the wall of
`pkg_8/file_35.rs`. Now `widening_room`, taking the max, with the overlap case
returning zero rather than reading an overlap as room.

**The router steered through the middle of the house it was arriving at.** Both
end cells are deliberately allowed to be blocked ground — a gate lies on its own
ward's boundary. But the path reconstruction then pushed that blocked cell's
*centre*, which is inside the obstacle by construction, and `from`/`to` are
inserted either side of it. The result was a dog-leg into the holding: a 2.4-wide
avenue crossing `note_5.md` for 4.6 units. The centre is now skipped for a
blocked end cell; `from` and `to` already say exactly where the road meets each
end. `route_exhaustive` mirrors the change, so
`the_heuristic_never_changes_a_route` keeps testing the *search* rather than the
walk back from it.

## A trap worth naming

**`cargo test -p kingdom-citymap` does not compile `build/`.** The module is
`#[cfg(feature = "ssr")]`, so the command AGENTS.md gives runs 252 tests and
none of them is a layout or streets test. Every change here was green under that
command while three tests were in fact failing. Use:

```bash
cargo test -p kingdom-citymap --features ssr
```

There is also **one pre-existing failure** on this branch's parent,
`a_well_is_legible_against_the_paving_it_stands_on` (74.9 against 126.1), which
is a colour judgement and unrelated to any of this.

## The cost, stated plainly

Small files now get genuinely small houses. The smallest footprint's short side
falls from 6.7 to 5.4 units, the median from 24.1 to 18.4. That is the trade the
King asked for: the map can no longer pretend a 60-line file is nearly as
substantial as a 600-line one. `MIN_HOLDING_LINES` and `LOT_COVERAGE` are the
dials if the small end wants lifting.

One thing the floors mean in practice: a settlement of only two files is barely
a hundred units across, and `MIN_LOT_THICKNESS` will rightly not cut a 117-unit
cell a hundred ways. A hundredfold difference needs a town big enough to draw
it — which any real repository holding a 10,000-line file is. The test says so.
