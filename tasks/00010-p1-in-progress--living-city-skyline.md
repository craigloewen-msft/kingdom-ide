# The Living Skyline: render every city as a real, built-up metropolis

Turn each city on the kingdom map from a single generic keep into an isometric
skyline **generated from the project's actual folder and file structure** — so
the King can see at a glance where the bulk of the code lives, and watch the
exact spot light up when an architect changes it.

---

## Why this is worth building (the guiding test)

`AGENTS.md` warns: *"A beautiful file tree fails that test."* That warning is
right, and this is deliberately **not** a file tree. The distinction matters:

> Today there is **nowhere on screen for agent activity to happen.**

Right now "Vitruvius is refactoring the auth module" is a *string in a sidebar*.
`Plan::touches` already carries `src/lib.rs`, and the map cannot point at it.
The map has one keep glyph per city, identical for a 30-file library and a
5,000-file monorepo, so the answer to question 1 — *what is every agent doing
right now?* — is prose, not geography.

Giving every folder and file a **stable location in space** is what makes
activity showable rather than describable. Once a building exists at a fixed
coordinate, all three of the product's questions get a physical home:

| Product question | What the skyline gives it |
|---|---|
| What is every agent doing? | The district it is working in glows; the changed building pulses |
| Who is blocked behind whom? | Contention threads land on **the district in contention**, not a vague circle |
| What am I being asked to approve? | `Plan::touches` highlights exactly the buildings a plan would alter |

The skyline is the **substrate** those signals get drawn onto. That is the
justification; the fact that it looks good is a welcome side effect, not the
argument.

### Scope discipline

This task builds the substrate **and wires it to the signals that already
exist** (architect status, `Plan::touches`, contention). It deliberately does
not invent new domain concepts. No file opening, no editing, no file tree.
Clicking a building selects its city and shows the path — nothing more.

---

## The visual concept

An **isometric city** per project, built from its own directory structure.

```
                    ▲  tall tower  = large file / dense module
                  ▲ █  clustered   = one folder (a district)
                ▲ █ █  gold roof   = a plan proposes to touch this
              ░░░░░░░  district plate, tinted by dominant language
            ╱ walls ╲  = the project is a git repo
```

- **District** = a folder. Nested folders nest as sub-plates.
- **Building** = a file. Height scales with size, footprint with kind.
- **Ward tint** = language of the file (Rust amber, TS green, Python blue…),
  so the King reads composition without a legend.
- **Walls** encircle the city when `has_git` — the repo boundary made literal.
- **Cathedral**: the largest district gets a landmark silhouette. This is the
  "where does the bulk of my code live" answer, visible at any zoom.
- **Construction cranes** hover over districts an architect is working in.
- **Scaffolding + gold roofs** on buildings a pending `Plan` would touch.

All of it rendered in SVG, consistent with the existing map. No canvas/WebGL —
the current pan/zoom, hit-testing and CSS status classes keep working.

---

## Design decisions worth stating up front

**1. Layout stays pure, in `kingdom-core`.**
Same reasoning that already put `spiral_layout` there: a building must not move
between reloads or the King's spatial memory — the entire value of a map — is
destroyed. Pure function of the file list, therefore testable.

**2. Non-overlap comes for free, and must stay that way.**
`spiral_layout` already guarantees city circles never overlap (pinned by
`spiral_layout_never_overlaps_cities`). If each city's buildable area is a
square **inscribed within its existing layout radius**, buildings can never
escape their city, so the cross-city guarantee extends transitively. The new
test only has to cover packing *within* a city.

**3. Isometric projection preserves that guarantee.**
The iso transform is affine, so disjoint footprints stay disjoint. Only
*height* can occlude, which painter's-algorithm depth sorting handles.

**4. Hard caps, with overflow made honest.**
50 cities × unbounded files would melt the DOM. Caps: nesting depth 3,
≤160 buildings per city. Everything beyond the cap is aggregated into a
**"commons" block sized by the summed bytes it represents** — so the map never
silently under-reports mass. A big folder always looks big.

**5. Level of detail is mandatory, not a nicety.**
A 40-city kingdom cannot render 6,000 buildings at once.

| Zoom | Renders |
|---|---|
| `< 0.5` | Silhouette + banner + status pips (roughly today's glyph) |
| `0.5–1.5` | District plates, cathedral, walls, cranes |
| `> 1.5` | Full buildings, plan highlights, labels on hover |

---

## Implementation plan

### 1. `kingdom-core/src/skyline.rs` — new, pure, wasm-safe

```rust
pub struct Building {  // one file
    pub name: String,
    pub path: String,      // relative to city root — the join key for plans
    pub ward: Ward,        // language, drives colour
    pub bulk: u64,         // bytes, drives height
}

pub struct District {     // one folder
    pub name: String,
    pub path: String,
    pub buildings: Vec<Building>,
    pub children: Vec<District>,
}

pub struct Lot {          // a placed building, in city-local coords
    pub path: String,
    pub x: f64, pub y: f64,
    pub width: f64, pub depth: f64, pub height: f64,
    pub ward: Ward,
}

pub struct Skyline {
    pub lots: Vec<Lot>,
    pub plates: Vec<Plate>,   // district plates, for the mid LOD
    pub cathedral: Option<usize>,  // index of the landmark lot
}

pub fn build_skyline(district: &District, radius: f64) -> Skyline;
pub fn iso(x: f64, y: f64, z: f64) -> (f64, f64);
```

**Packing algorithm:** squarified treemap over district `bulk`, recursing into
sub-districts, then a grid of building footprints within each leaf plate.
Treemap is the right pick because area stays proportional to bulk — the King
sees a *true* picture of where the code mass is. Deterministic: sort by
(bulk desc, path asc), never by hash-map order.

### 2. `kingdom-core/src/model.rs` — extend `City`

Add `pub structure: Option<District>`. `Option` keeps it cheap: existing
callers and the layout tests construct `None` and still compile.

Add `Ward` (language) with `tint()`, mirroring the existing `CityKind::banner_color`.

### 3. `kingdom-app/src/scan.rs` — collect structure

Extend the existing walk (do not add a second pass) to build the `District`
tree, capturing `metadata.len()` for source-ish extensions. Reuses the current
`SKIP_DIRS` / depth / cap machinery.

> **Risk to measure:** this adds a `stat` per file. The existing walk already
> visits every entry, so the marginal cost should be small — but I will time a
> scan of a real dev folder and, if it regresses noticeably, move structure
> collection into a background refresh after the first paint.

### 4. `kingdom-app/src/components/map.rs` — render it

Split the growing map module rather than letting it sprawl:
- `map.rs` — viewport, pan/zoom, LOD threshold, contention threads
- `map/city.rs` — the skyline: plates, buildings, walls, cathedral, cranes

Wire the existing signals in:
- architect `Working` in a city → crane over its district, existing `astir` halo retained
- `Plan::touches` (pending plans only) → gold roof + scaffolding on matching `Lot.path`
- contention threads → terminate on district centroids instead of city centres

### 5. `style/main.scss` — skyline styling

Extend the existing palette; keep colour meaningful. Reuse `status-*` classes
so sidebar and map stay consistent. Roof/facade/shadow tones derived from
`Ward.tint()` so language reads instantly.

### 6. `kingdom-core/src/sample.rs` — point plans at real files

`populate_court` currently hardcodes `touches: ["src/lib.rs", "src/main.rs"]`,
which will match no scanned building in most cities, leaving the highlight path
dead on arrival. Change it to pick real paths from the scanned `District`.

In the same spirit as the existing note about the deliberately blocked
architect: the sample must exercise the states the UI exists to show.

---

## Tests

Three, all in `kingdom-core`, each pinning something the King would notice
breaking — matching the standard set by the existing layout/lease tests.

1. **`buildings_stay_within_their_city_and_never_overlap`** — the load-bearing
   one. Every `Lot` lies inside the inscribed square of the city radius and no
   two footprints intersect. This is what lets the existing cross-city
   non-overlap guarantee extend to buildings.
2. **`skyline_layout_is_deterministic`** — same city structure ⇒ byte-identical
   placement. Directly protects the King's spatial memory, exactly as
   `spiral_layout_is_deterministic` does for cities.
3. **`every_file_is_placed_exactly_once`** — total lot count plus commons-block
   aggregation accounts for every file in the district tree. Pins the promise
   that **the map never silently loses code**, which is what makes "where does
   my code live" trustworthy.

No tests for tint lookups, glyph strings, or the iso transform's arithmetic —
those restate the implementation.

---

## Verification

- `cargo test -p kingdom-core`, `cargo fmt`, `cargo clippy` clean
- `cargo leptos serve` against a real dev folder; confirm at 40+ cities that
  pan/zoom stays smooth and each LOD tier switches cleanly
- Confirm a city with a dominant folder is *visibly* dominated by it — the
  headline promise of the feature

## Out of scope

Opening or editing files; a file tree panel; real git dirty state; live updates
(WebSocket remains the separately-tracked next step); persistence.
