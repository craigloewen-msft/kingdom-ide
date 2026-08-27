# Every agent's work, always drawn — and perfectly still

Two faults in the same feature: the agent diffs raised over the map. They were
scoped to whichever city the King had selected, so a project he had not clicked
drew **nothing** — and on the map's own screen, where there is usually no
selection at all, no agent's work was drawn anywhere. And everything that *was*
drawn breathed on a 2.4-second cycle.

Both are gone. Every live agent in the kingdom stands on the map at all times,
in its own colour, and nothing on the map animates itself.

## The scoping was in four places, not one

It was not a renderer bug. The whole chain assumed exactly one city, and each
link had to widen:

| Where | Was | Now |
|---|---|---|
| `api.rs` | `city_changes(city)` → `plans_in(city)` | `kingdom_changes()` → every live plan |
| `app.rs` | memo keyed on `state.selected`; **cleared the works** when `None` | keyed on every live plan's id and `working_on` |
| `view.rs` | resolved only when `focus_city` was `Some` | resolves whenever the world stands |
| `map/works.rs` | `resolve(map, city, ..)` | `resolve(map, ..)`, city per entry |

The `app.rs` line is the one that actually produced the reported symptom: with
no selection the digest returned `None` and the effect **set the works to
empty**, which is how the map is told to tear them down.

`PlanChanges { plan, city, changes }` replaces the `(PlanId, ChangeSummary)`
pair, because the answer is no longer about one city and so has to say which one
each entry belongs to.

## The trap that came with it

Drawing every city at once breaks an assumption that was safe while this took
one: **a path does not identify a file in a kingdom.** Every Rust project on the
map has a `src/main.rs`. The grouping key was the path alone, so two projects'
files would have fused into one house wearing two bands — inventing contention
between agents who had never touched the same file, and drawing only one of the
two real files.

The key is `(city, path)` now, which is what `MapManifest::holding_at` has
required all along for precisely this reason. Ghost placement is seeded from
both halves too. Three tests pin it, including one for new files.

Two consumers of `state.works` also had to narrow, since the signal widened
under them:

- **`review_drawer`'s contention pips** filter to the plan's own city first.
  Without that, an agent in another repository would have been reported as
  sharing your file.
- **the rail map's agent chips** filter to the focused city: that pane is framed
  on one town, and a key naming agents working elsewhere is a key to nothing.

Banners are now assigned across the whole kingdom, which they must be for the
map to be honest with several projects drawn at once.

## Nothing pulses, and nothing is dimmed by size

`pulse_works` and `pulse_rings` are gone, along with `glow`, `PULSE_SECONDS`,
`PULSE_FLOOR` and `PULSE_PEAK`.

The King also asked that bands not *change colour*, and the pulse was only half
of what did: `band_color` took a `strength` of `0.55 + magnitude(churn) * 0.45`,
so a 4-line edit was drawn at 55% of its agent's hue and a 400-line one at full.
The removal skirt faded the same way. A colour that varies is being asked to say
two things at once — *whose* work this is, and *how much* — and it now says only
the first. **Size is the only channel magnitude has**: height, girth, cover and
the skirt's spread are all untouched.

Two things fell out for free once nothing was animated:

- The `Scaffold` component is deleted. It existed solely to carry a per-band
  material handle for the pulse to write through.
- `TownRing` loses its material handle and takes the **shared** `MaterialCache`
  like every other unlit surface. It could not before: the cache quantises by
  colour and hands one handle to hundreds of meshes, so writing to it each frame
  would have pulsed whatever else landed in the same bucket.

`PULSE_PEAK`'s doc comment survives on `WORKING_COLOR` — it records three failed
attempts at drawing a status colour as a *lit* surface (white, then mint, then
near-white), which is why everything here is `unlit`. That reasoning is still
load-bearing even though the constant is not.

`RAIL_WAKE`'s justification was rewritten in all three places that cite it: the
interval is unchanged, but "the only thing that moves is the ring's breath" had
become false. It now cites the camera's glides, which is what still moves.

## How it was checked

The suite, and then the thing itself. Tests alone could not prove the server
function was wired up, so a server was run against the `kingdom-mirror` realm
with real edits made in two of its repositories — 221 lines added in one file,
267 removed from another, plus a new file — and the map read with **no city
selected**. Both towns drew their agents' work. Before this change that screen
was empty.

Stillness was measured rather than eyeballed: `readPixels` over the band of
canvas where the works stand, sampled across 3.4 seconds — longer than a full
old pulse — gave **13 frames and exactly one distinct signature**. The same
probe was then re-run against a moving camera and returned two, which is what
makes the first result mean something rather than being a broken instrument.

One fixture quirk worth knowing: `forge` in `kingdom-mirror` has no `.git` of
its own and inherits the enclosing worktree's, so it reports the *plan's* diff
rather than a fixture one. Nothing to do with this change, but it will mislead
anyone rehearsing there.
