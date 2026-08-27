# Removals that cover the house, instead of standing on it

A file's removed lines were drawn as a band in the column **above the roof**,
stacked directly on top of that agent's added band. So a file losing three
hundred lines grew a *taller tower* — the same shape growth makes, and the
opposite of what had happened.

Now what is being taken away covers the house: a block rising from the ground
over as much of it as the file is losing. Half the file cut, half the house
covered.

## The grammar has one rule now, where it had three

`engine::works`'s own module docs already claimed the rule — *"what is being
built rises above a roof, what is being taken away stays at the foot"* — and the
code contradicted it in the very next function. Removals were drawn three ways
at once:

| What | How it was drawn |
|---|---|
| lines removed | a band in the upward column, above the roof |
| lines removed | a skirt of stained ground around the lot |
| file deleted | a band at the foot of the house, at 55% of whatever height its churn earned |

The first was the reported fault. The third was a separate mark with separate
constants (`RAZING_RISE`, `RAZING_GIRTH`) saying a *related* thing in an
unrelated way. Both are now one shroud, and a deletion is not a special case at
all — it is a cover of 1.0, which is the honest reading of "the whole house is
going".

## Why the height is a share of the file, not of churn

`tasks/00250` moved column height from a *relative* ramp to an absolute one, and
that was right: two agents' columns have to be comparable across the whole map,
so "how much did this agent move" must not be measured against anything local.

The shroud asks a different question — *how much of this file is going away* —
and that is a ratio or it is nothing. Three hundred lines is most of a
four-hundred-line file and a rounding error in a twenty-thousand-line one, and
the King asked for exactly that distinction. So the shroud gets its own scale and
the column's is untouched.

The denominator was already on the wire: `MapFeature::lines`, populated by the
scanner and carried into the manifest, unused by the renderer until now.

## Where the ratio is computed, and why not in the renderer

`map::works` documents its seam as *"above it are paths and line counts; below it
are rectangles and heights"*. Handing the engine a `lines` field would have
breached that for one multiplication. So `resolve` computes `WorkBand::cover` and
the engine only ever multiplies a fraction by a height it already has — the
engine still knows nothing about codebases.

Four cases, each a judgement rather than a fallback:

- **a deletion covers everything**, whatever git counted — a file reported `-0`
  because it was empty is still entirely gone;
- **a known length is the denominator**, clamped, because the manifest is
  memoised on the *shape* of the kingdom and is allowed to be stale about a
  file's contents — `removed` can genuinely exceed it;
- **an unknown length** (too large to analyse, or not text) falls back to a
  nominal 400 lines, so an unmeasurable file still shows something rather than
  nothing — the invisible-removal fault all over again;
- **a house with no prior file** is a share of its own churn. This is the one
  that matters more than it looks: a `Fresh` site is *either* a created file *or*
  one the manifest is stale about, and the second is a real house being gutted.
  Against a nominal length it would draw as a sliver.

## Two things measured rather than judged

**The girth.** The old `RAZING_GIRTH` was 1.06 — *narrower than most roofs on the
map*, which is a fair part of why a razing read as a box wedged inside the
building rather than one placed over it. Every archetype in `meshes.rs` was read
for its widest point, in the unit footprint they are modelled in:

| Archetype | Widest roof point | Girth needed |
|---|---|---|
| `keep` | `HALF + 0.04` | 1.08 |
| `scriptorium` | `HALF + 0.05` | 1.10 |
| `pitched` (cottage, guildhall, granary) | `HALF + 0.06` | 1.12 |
| `market` | `HALF + 0.12` (front slab) | **1.24** |

`SHROUD_GIRTH` is 1.28. It stays well inside the lot — `layout::Building::
footprint` insets a house to 0.46–0.58 of its lot — so no neighbour is touched.
The table is *both* a `const` assertion and a test that names each archetype, so
adding a wider roof breaks a build rather than a rendering.

**The floor.** One line cut from a four-thousand-line file is a share of
0.00025, which is nothing at any zoom. `SHROUD_FLOOR` is 8% of the house — a
share rather than an absolute, unlike the column's `BAND_FLOOR`, because houses
here differ in height by an order of magnitude.

## What was deliberately left alone

- **The ground skirt stays.** It is not the thing complained about, and it is not
  redundant: the map's most common home is a 290 px pane where a house is a
  couple of pixels across, and there a shroud *over* the house is a fraction of
  those pixels while a stain spreading across the lot can still be seen. Two
  zooms, two marks; the constant now says so.
- **The additions column** — `FULL_CHURN`, `GIRTH_RANGE`, `BAND_GAP`, the pulse.
  `tasks/00250` fought for that ramp and nothing here changes its reasoning.
- **The colours.** Still one hue per agent from `kingdom_core::palette`, the
  deepened value for cutting. Two agents cutting one file stack two shrouds, so
  *whose* deletion is whose survives the change.

## One thing the tests caught after the fact

`resolve` clamps each agent's share to 1.0, which is correct per agent and not
enough: with several agents on one file the *stack* is the sum, and three agents
each cutting half a file come to one and a half houses — a shroud rising past the
roofline, breaking the one rule the whole grammar has. The drawing now holds each
block to whatever is left of the house, and a test walks four agents up a single
roof to pin it.

## Checked

`kingdom-citymap` 200 tests (up from 188), `kingdom-core` 70, `kingdom-app
--features ssr`, both target builds, `fmt` and `clippy` clean.

**Not seen on screen.** The browser was unavailable for this work, so every
number above is either measured from the mesh source or pinned by a test rather
than eyeballed — which is why the eave table exists as a build failure and why
`shroud_height` is a pure function with its own tests. What tests cannot prove is
whether it *looks* right at the two zooms it is drawn for; that wants the King's
eye.
