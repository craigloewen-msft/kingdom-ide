# Building works: a plan's changes, raised on the map

The map answered two of the three questions in `AGENTS.md` -- who is working
(the pulsing town ring) and where things are. It said nothing about the third,
**what are they proposing that I need to decide on?**, which is the one the King
opens a chamber to ask.

Now, with a plan open, the settlement shows the proposal as construction:

- a house whose file gained lines wears a **scaffold**, a translucent green
  column standing on its roof, as tall as the change was large;
- a house whose file lost lines wears a **skirt** of cleared red ground;
- a file the court *created* stands as a **ghost house** on free land inside the
  folder it belongs to.

## The two constraints that shaped it

**The manifest may not be rebuilt.** `citymap.rs` memoises the map JSON on the
kingdom's *shape* -- root plus city names -- and its own doc explains why a file
changing must not invalidate it: seconds of filesystem work and ~4 MB, to move a
rooftop. So the works travel the way activity does, as a `ViewerCommand` over the
bridge, and the settlement is untouched. `engine::activity` made this exact
argument for the ring; this is the second customer of it.

**There is one map.** It is mounted once for the life of the page and moved
between two rectangles. Nothing here adds a second one, a second canvas, or a
branch on `MapPresence` -- the works are drawn wherever the map is standing.

## Where the domain stops

```mermaid
flowchart LR
  A["conversation.rs<br/>ChangeSummary"] --> B["state.works"]
  B --> C["view.rs effect"]
  C --> D["works::resolve<br/>(the boundary)"]
  D --> E["SetWorks(Vec&lt;Work&gt;)"]
  E --> F["engine/works.rs"]
```

`resolve` is the seam. Above it are paths and line counts; below it are
rectangles and heights. The engine never learns what a plan is, exactly as it
never learns what a `CityId` is.

It costs **no new request**: the review drawer already fetches the summary on
every transcript entry, and this is that same value read once more.

## What is deliberately not drawn

Each omission is a judgement about what would be *dishonest*, not a shortcut:

- **Binary files.** `ChangedFile::binary` exists because `+0 -0` reads as
  "unchanged" for a file that certainly changed. Its numbers are not line counts,
  and a scaffold built from them would be an invented figure over a real house.
- **Deleted files.** A deletion is a lot that *empties*. The map is drawn from
  the city's checkout, where the house still stands -- so a scaffold on it would
  say the opposite of what happened.
- **Files whose town or folder the map never drew.** The ordinary staleness
  `citymap.rs` documents as a deliberate trade.

## The normaliser, and where it belongs

A scaffold's height is a fraction of the busiest file *in the same plan*, on a
`sqrt` curve. Real work is lopsided -- one file rewritten, a dozen touched -- and
linearly the dozen are stubs beside the one.

The plan put `busiest()` on `ChangeSummary` in `kingdom-core`. **That was wrong,
and a test caught it.** The scale has to be taken over the files actually
*drawn*: a plan that deleted a 400-line file and edited a 40-line one draws only
the edit, and normalised against the deletion it would draw it at a tenth height
-- the one thing on the map looking like nothing had happened. Only the map knows
what it is going to omit, so the normaliser moved into `works::resolve` and
`kingdom-core` kept just `ChangedFile::churn()`, which is a genuine domain fact.

## Placing a house that does not exist

The fiddliest part, and the reason it is pure. `place_fresh` walks a golden-angle
spiral from the middle of the ward -- the arrangement `build::layout` already
spaces towns by -- rejecting any spot that overlaps a lot, a nested folder's
ground, or a ghost already placed on this pass. It shrinks and re-sweeps three
times before calling a folder full, so a new file in a packed folder still
appears, small, rather than vanishing.

Seeded on the path, because the review is refetched on every transcript entry and
a house that hopped around the folder while the King watched would read as a bug.

Living in `map/` rather than `engine/` is what lets `cargo test` pin it on a bare
machine: a ghost landing on a real house is a test failure, not something spotted
by eye once, if the right folder happened to be looked at.

## Four things found by building it

1. **NaN reached the renderer.** A scale is a ratio, and a rename with no content
   change divides by a churn of zero. `f32::clamp` propagates NaN rather than
   trapping it, so the height arrived at Bevy as a degenerate mesh. Guarded in
   `scaffold_height`, and guarded again upstream by skipping zero-churn files.
2. **The works were silently absent on first open.** Raising a world clears them
   -- scaffolding over a settlement being torn down would hang above whatever
   replaced it -- and on a cold page the summary resolves *before* that `Load`
   lands, so the first send was thrown away and nothing asked again. The effect
   now tracks `built`, which is the same trap the two camera effects beside it
   already document.
3. **A ghost house and its scaffold merged into one green mass.** The scaffold
   started at ground level and rose *through* the house. A ghost's roof is now
   its scaffold's base, as a standing house's already was.
4. **A single-town fast path answered with the wrong city's ground.** Caught by
   the test asserting a city the map never drew resolves to nothing.

## Two judgements, both one constant

`SCAFFOLD_REACH` and `WORKS_ALPHA` were both raised after looking at a real plan
on screen, and both are named constants with the measurement beside them. A
typical holding in the proving ground stands ~32 units, so the original 34-unit
column was the same order as the house and read as a slightly taller roof; and at
61% opacity the working green over a sage Source roof read as a tint on it rather
than a thing standing on it.

The colours themselves were *not* touched. The green is the one
`PlanStatus::Drafting` reports and the ring already uses -- a third green for one
plan is how a map and a rail come to disagree -- and the red is what
`.count-removed` paints a deletion with in the drawer. Both are pinned against
`kingdom-core` by test, as `activity`'s already was.

## Checked

`kingdom-core` (70), `kingdom-citymap` (152), `kingdom-app --features ssr` (259,
with one pre-existing failure that needs a live Copilot credential and fails on
`main` too), and both target builds.

Then driven in a real browser on the Proving Grounds against a plan with a
deliberate mix -- a 121-line edit, a 3-line edit, a 277-line deletion, two files
created after the manifest was cached, and a whole file deleted -- confirming
each of the five is drawn, or not drawn, as intended.
