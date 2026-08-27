# A column you can read the size of the change from

The King reported it plainly: a `+8` line change looked about the same size and
height as a `+100`. He was right, and he was right about where to look — the
height calculation for a plan's pending changes was wrong.

The columns a plan raises over the houses it is changing are sized by
`engine::works`, and everything the King reads about a change's size comes from
one function:

```rust
pub const FULL_CHURN: f32 = 600.0;

pub fn magnitude(churn: f32) -> f32 {
    (churn.ln_1p() / FULL_CHURN.ln_1p()).clamp(0.0, 1.0)
}
```

Height, girth, the pulse's brightness and the removal skirt's spread all read it,
so whatever it did badly, it did five times over.

## Four faults in one curve

**1. It spent the range before any real change started.** `ln1p` rises fastest
near zero, so a *one-line* edit already took 11% of the range — 8.8 units of 52 —
before the floor was considered, and eight lines took 34%. The bottom third of
the scale went to changes that had barely happened.

**2. It had no resolution where changes actually live.** Measured over the last
400 commits of this repository, per-file added lines run:

| p25 | median | p75 | p90 | p99 | max |
|---|---|---|---|---|---|
| 6 | 27 | 115 | 246 | 935 | 3,872 |

Across the middle of that — 27 against 115, 4.3× the work — the old curve moved
**1.37×**. The King's own case, +8 against +100, is 12.5× the work and drew
**1.91×**, on a column standing on a roofline that itself varies by more than
that, in a pane where a house is a couple of pixels across.

**3. The clamp was a plateau on real work.** Everything past 600 lines drew
identically — and p99 here is 935. The constant's own doc claimed 600 was
"comfortably past a large single-file change"; the data says it is p95.

**4. The second channel was not one.** `band_girth` was added specifically to
widen the dynamic range and multiplied *the same compressed number*. At +8 a
column was already 58% of full width, for 1.3% of `FULL_CHURN`. Two squashed
channels are still squashed.

## Why the previous fix did not catch this

`tasks/00250` fixed a real and different fault: height had been *relative*, a
share of the busiest file in the same plan, which made two agents incomparable
and drew a one-file plan at full height. That fix was right and is untouched —
the ruler is still absolute lines, and nothing local to a plan is consulted.

What it did not do was ask whether the *shape* it reached for suited the
quantity. It took the logarithm from `build::layout::Building::height`, which was
sound reasoning about a different distribution — file lengths, spanning three
orders of magnitude — and applied it to change sizes, which do not. The lesson
worth carrying: **the curve is a claim about a distribution, and the distribution
is measurable.**

## The fix

A saturating ratio, with its knee where the data is:

```rust
pub const HALF_CHURN: f32 = 110.0;

pub fn magnitude(churn: f32) -> f32 {
    if !churn.is_finite() || churn <= 0.0 {
        return 0.0;
    }
    (churn / (churn + HALF_CHURN)).clamp(0.0, 1.0)
}
```

Near zero it is very nearly linear, so small changes differ in proportion to
their size instead of all being lifted to one stub. It never plateaus, so fault 3
cannot return by mis-tuning a constant. And 110 is close to this repository's p75
of 115, so the steep part covers p25 to p90 — the band the map has to resolve —
and the flattening happens out among the rewrites.

Alongside it:

- **`COLUMN_REACH` 52 → 58.** The curve saturates rather than clamping, so the
  reach is an asymptote nothing stands at; without the extra room the large end
  would have come down. 58 puts a 600-line change back at roughly the 52 it drew
  before. A `const` assertion holds it under the 60 units `engine::mod::TALLEST`
  reserves for a roofline, so a column cannot be framed out.
- **Girth gets its own curve**, `presence = magnitude.sqrt()`. This is fault 4
  answered: a second channel has to be a different shape, or it only squares what
  the first did. Height carries the proportionality; girth keeps a small change
  wide enough to resolve in the rail's pane.
- **The removal skirt uses `presence` too**, so making the low end honest did not
  quietly undo `tasks/00260`'s fix for invisible removals — a 20-line cut would
  otherwise have spread a third of what it did.
- **A ghost house gains a floor of 11.0**, `build::layout::Building::height`'s own
  lower clamp. A new file's house is half the column its churn earns, and with an
  honest low end a small new file would have been a slab a few units high, which
  reads as a stain rather than as a building.

  That floor needed a **cap** to go with it, found while re-reading the code
  rather than by a failing test. A ward with no room shrinks a ghost's footprint
  to fit — `map::works::SHRINK`, up to three times, so a quarter of the usual —
  and an absolute floor standing over a lot that small draws a *spike*, a
  silhouette no holding on this map is allowed. The height is now held between
  the floor and `plan * 1.9`, which is `height_ceiling`'s own ratio: on a
  cramped lot the proportion wins and the house is small rather than a mast.
  `a_ghost_house_is_shaped_like_a_house_on_any_lot` pins both bounds.
- `strength`, the pulse's brightness, is unchanged and simply inherits the better
  curve: a heavily-worked house is now visibly brighter as well as taller.

## What it draws

| lines | height before | height now | girth before | girth now |
|---|---|---|---|---|
| 1 | 8.8 | 4.0 | 0.36 | 0.35 |
| 8 | 20.2 | 7.2 | 0.49 | 0.44 |
| 27 | 28.8 | 14.2 | 0.59 | 0.54 |
| 100 | 38.5 | 29.5 | 0.70 | 0.68 |
| 246 | 45.3 | 41.2 | 0.77 | 0.76 |
| 600 | 52.0 | 49.6 | 0.85 | 0.81 |
| 4,000 | 52.0 | 56.5 | 0.85 | 0.84 |

| comparison | height before | height now | face before | face now |
|---|---|---|---|---|
| **+100 vs +8** (reported) | 1.91× | **4.09×** | 2.72× | **6.28×** |
| p75 vs median (115 vs 27) | 1.37× | 2.20× | 1.66× | 2.80× |
| +400 vs +40 | 1.55× | 2.56× | 2.04× | 3.46× |
| +4,000 vs +600 | 1.00× | 1.14× | 1.00× | 1.19× |

The trade is deliberate: the very top of the range is compressed, so a 1,000-line
change and a 4,000-line one come out nearly alike. Both mean *very large*, and
telling +8 from +100 does not.

`BAND_FLOOR` stays at 3.5 and now matters more — it is the only thing holding the
smallest change above nothing, where before a curve lifted everything.

## What is pinned

Six tests, each naming the fault it guards:

- `a_hundred_line_change_towers_over_an_eight_line_one` — the King's own
  comparison as arithmetic, at ≥3× height and >5× face.
- `every_step_through_a_real_distribution_is_visible` — the quartiles above,
  walked consecutively, because a curve can spread its ends and still be flat
  through the middle. Each step carries the ratio the logarithm managed for that
  same pair and must beat it by 15%.
- `a_very_large_change_still_grows` — 935 > 600, 4,000 > 600, 20,000 > 4,000. The
  plateau cannot return.
- `doubling_a_small_change_nearly_doubles_it` — 8 to 16 lines moves the column
  1.6–2.0× above the floor, against the log's 1.24×.
- `a_ghost_house_is_shaped_like_a_house_on_any_lot` — the new floor and its cap,
  on lots from cramped to generous.
- Two new `const` assertions: the knee is a real number of lines, and the reach
  stays under the fit's assumed roofline.

The existing shape, NaN and infinity guards, girth monotonicity and skirt
visibility tests are unchanged and pass. 208 tests in `kingdom-citymap`, up
from 203.

**One correction against the approved plan.** It proposed asserting every
quartile step at >1.4×, and that failed: p75→p90 draws 1.31×. The threshold was
wrong, not the curve — that pair is 2.1× the lines where the two below it are
4.4×, so demanding an equal step of all three would ask the curve to misreport
the distribution. The test now asserts a visibility floor *and* an improvement
over the logarithm's own figure for each pair, which is the honest form of the
same claim.

## What was not changed

Nothing outside `kingdom-citymap`. `map::works::resolve`, the seam that turns
line counts into bands, is untouched; so is the shroud, whose scale answers a
different question — *how much of this file is going away* is a ratio of the
file's own length, and `tasks/00260` is still the record of it.

Not seen in a browser beyond a smoke check on the proving ground: the map runs on
a software rasteriser here, and the works need a plan with changes open to draw
at all. What was verified there is that the world still stands and renders with
these constants — `__kingdom_map.built` true, the map drawn — over CDP against
`kingdom-mirror` on port 39117. Every figure above is measured from the
arithmetic or from `git log --numstat` over 1,209 file-changes, and pinned by a
test.
