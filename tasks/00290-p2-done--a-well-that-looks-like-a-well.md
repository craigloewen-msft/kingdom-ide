# A well that looks like a well, standing on the square

**Status:** done · **Priority:** p2

The King, looking at the map: *"The shared resource (The well) looks strange...
its colour looks off it looks like it's glowing. Also make it on the main town
square instead of awkwardly sitting in the middle of the town."*

Both complaints were one line of code each. Both were already contradicted by
the documentation, which is the interesting part.

## The placement bug that read as correct

`map/network.rs` said, in its own module doc and in `docs/citymap.md`:

> a **wellhead**, standing on a town's square, one per service that city has
> running … A town with no square gets no wellhead. The square is the well's
> place.

Neither sentence was true. `resolve` never looked at `world.plazas` at all.
`well_stand` took `town.center` — the centre of the town's *rectangle* — and a
square is nowhere near there: `streets::square_site` walks a square outward from
the settlement's middle until it finds ground no ward has claimed, because the
middle is the one place the largest folder has already taken.

Measured against the live manifest the King's own server was serving:

| town | square's distance from the town centre |
|---|---|
| copilot | 94 |
| agora | 218 |
| repo-city-visualizer | 281 |
| mommys-heart | 499 |
| kingdom-ide | 624 |
| autotaskcalendar | 749 |
| phoenix-ide | 1,622 |

A square is 52 units across. Every well on that map was standing among the
houses, which is exactly what "awkwardly sitting in the middle of the town"
describes.

**This is `tasks/00280-p2`'s third lesson repeating.** That task fixed the agent
marks for standing among the buildings and wrote down why. The well had the
identical fault three lines away and was missed, because twelve passing tests
all measured the well against *the placement rule* rather than against the town.

## "Glowing" was literal

`engine/network.rs` drew the well `Surface::Unlit` — `unlit: true`, so no sun,
no shade, no shadow — in `#cfd8dd`, a near-white, 350 from the paving on the
palette's own ruler. Every other object in a settlement is lit. An unlit
near-white disc among lit earth tones *is* a light source, and the eye reported
it accurately.

The unlit rule was right for what it was written for. `activity::WORKING_COLOR`
records three measurements proving a status colour cannot be lit: emissive
scaled for the sun's lux clips to white, a value near 1.0 is washed out by the
tonemapper, and a lit surface adds the sun's specular — measured `(168, 231,
167)` for a green meant to be `(34, 197, 94)`. That reasoning covers the agent
marks, the host ring, the moats and the channels, all of which carry an identity
that has to arrive exactly as sent.

It does not cover a well, which carries no identity at all. A well is a
*building*.

## What it is made of now

Four parts, all `Surface::Matte`, all lit by the settlement's own sun:

| part | colour | why |
|---|---|---|
| drum wall + rim | stone `#9a9187` | the silhouette, at every zoom |
| water | teal `#24424a`, recessed | says *well* rather than *barrel* |
| posts + beam | timber `#6b4a32` | `Architecture` tier only |

Heights are multiples of the well's own radius rather than fixed, because the
radius now varies with how crowded the square is.

Two new mesh primitives, both pinned without a browser:

- `annulus` — a flat ring. `ground_polygon` can only fill a solid outline, and
  filling this one would cap the very hole the shaft exists to show.
- `inward_wall_ring` — the shaft's inside face. **Not `wall_ring` with the
  points reversed:** that normalises its winding through `upward_ring` before
  building anything, so reversing them changes nothing and back-face culling
  eats the whole shaft. Exactly the trap `spire()` already documents against
  `cone()`, one level down.

## Making the square findable

`world.plazas` carried no town, so nothing could ask where a given town's square
was. `MapPlaza` now has `town`, tagged in `build::scene` (the only place that
knows both the square and whose it is), and `MapManifest::square_of` reads it —
matched on the **name**, never the `town-N` id, for the reason `town_named`
documents at length.

Tagging rather than guessing geometrically. "Which town rectangle does this
square fall inside?" happens to work on today's map, but `square_site` is free
to walk a square anywhere it finds clear ground, so it is a rule that holds
until it silently doesn't.

## Placement, and the 0.8 units that mattered

Wells lie along the square's **rear** edge. The rear is not arbitrary:
`wayfinding::square_label` paints the town's name across the middle band, and
the camera looks down `(-1, -1, -1)` so low `y` projects up-screen — a well at
the back stands above the lettering rather than on it.

`WELL_RADIUS` drops 13 → 8.5. Thirteen was chosen when a well floated on open
ground; on a 52-unit square it was a quarter of the whole paving. It is a
*ceiling* now: each well takes an equal slot of the usable span, so three
services give three smaller wells on the stone rather than a row spilling onto
the grass.

The first version of the lettering test failed by **0.8 units** — the drum
reached 160.0 where the name began at 159.2. The fix was not to nudge a
constant: the radius is now also capped by the depth of the rear strip, derived
from `SQUARE_LABEL_SHARE`. That constant is necessarily duplicated (`build` is
server-only; the placement compiles to both targets), so a test in
`build::wayfinding` pins the copy — raise the size a name is painted at and it
fails there rather than a name quietly appearing underneath a well.

## The colour, measured on the palette's own ruler

Nearest banner pair: 126.1. That is the bar.

| colour | nearest banner | vs paving `#816941` |
|---|---|---|
| `#cfd8dd` (old) | 193.3 | 349.9 |
| `#9a9187` (stone) | 165.5 | 141.3 |

The existing test asked only the first question. A well stands on paving now, so
a colour close to `PLAZA` would be camouflage — the mark exactly where the King
was told to look and invisible when he looked. The second column is now pinned
too, in `build::streets`, the only module that can see both constants.

## Tests

247 in `kingdom-citymap`, up from 240. The seven that are new:

- a wellhead's footprint lies inside its town's square;
- it clears the band the town's name is painted in;
- three wells share a square without overlapping or leaving it;
- a town with no square gets no wellhead — the documented rule, now real;
- `square_of` matches on name, not position;
- an annulus faces up and keeps its hole;
- an inward wall ring faces the middle.

And the fixture change that gives them teeth: `a_map` now builds each town a
square **offset** from its centre. A fixture with the square at the centre would
have let the original bug pass every one of these.

## Rehearsed against a real container

`shopfront` seeded into `/tmp` under its own `KINGDOM_HOME`, served on `:3117`,
a plan opened, a real `mongo:7` started. The well stood on the paving at the
rear of the square, lit, shadowed, clear of the lettering, with the channel from
the isolated agent running to it. The King's own server on `:3000` and his
`kingdom-agora-…-db` container were untouched; the rehearsal's container and
profile were removed afterwards. The King took over final visual verification.

## Where the code lives

| File | Target | What |
|---|---|---|
| `map/mod.rs` | both | `MapPlaza::town`, `MapManifest::square_of` |
| `map/network.rs` | both | placement, sizing, the three colours |
| `build/scene.rs` | ssr | tagging each square with its town |
| `build/streets.rs` | ssr | the paving-contrast test |
| `build/wayfinding.rs` | ssr | pinning the duplicated label share |
| `engine/meshes.rs` | hydrate | `annulus`, `inward_wall_ring` |
| `engine/network.rs` | hydrate | the wellhead, lit |

`MapPlaza` gaining a field changes the wire format, which is safe here because
no manifest is ever stored: `kingdom_app::citymap` memoises the JSON in memory,
keyed on the kingdom root, and rebuilds when that changes.
