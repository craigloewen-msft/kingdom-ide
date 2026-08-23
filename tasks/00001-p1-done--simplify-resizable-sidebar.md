# Simplify the sidebar into a resizable city -> plans tree

The left rail currently stacks four tabbed panels (Cities / Architects / Plans /
Crown Resources) at a fixed 290px. Three of those panels list pure placeholder
data, and the tab strip costs a click before the King sees anything. Replace it
with one thing: a drag-resizable rail listing cities, each holding its own plans.

## Goals

1. **Drag-resizable width.** Grab the rail's right edge, drag left/right, like
   any IDE. Width persists across reloads.
2. **One list, no tabs.** Cities at the top level; a city's plans nested
   underneath it.
3. **Active / All toggle.** Default shows only live plans (Draft +
   Awaiting review). Flip to All to see approved and rejected history too.

## Scope note - what "rip it out" means here

Architects and Crown Resources leave **the sidebar only**. They stay in
`kingdom-core` and stay on the map (architect pips, red contention threads,
legend), because per AGENTS.md resource arbitration is the reason the product
exists - it just doesn't need a list panel while the data is fabricated. No
changes to `kingdom-core`, `sample.rs`, `map.rs`, or `chat.rs`.

## Plan

### 1. `crates/kingdom-app/src/app.rs`

- Delete the `Panel` enum and `KingdomState::panel` (nothing outside the sidebar
  reads either).
- Add to `KingdomState`:
  - `sidebar_width: RwSignal<f64>`, default `290.0`
  - `show_all_plans: RwSignal<bool>`, default `false`
- Drive the grid from the signal so the resize is live:
  `<div class="throne-room" style:grid-template-columns=move || format!("{}px 1fr", ...)>`,
  and drop the hard-coded column from the SCSS rule.

### 2. `crates/kingdom-app/src/components/sidebar.rs` - rewrite

Structure:

```
header   K  <kingdom name> / <root path>            (unchanged)
toolbar  Cities 7                     [ Active | All ]
body     v kingdom-ide            Rust
           The Great Refactoring            Draft
           The Aqueduct           Awaiting review
         > some-other-project     Node
resizer  (grab strip on the right edge)
```

- `CityRow`: clicking the row body selects the city (existing `state.selected`
  behaviour, so the map highlight and chat-dock target keep working); clicking
  the chevron toggles expansion. Expansion state is local to the sidebar,
  `RwSignal<HashSet<CityId>>`, expanded by default so a city collapses only when
  the King collapses it.
- A city's plans come from `kingdom.plans.iter().filter(|p| p.city == id)`, then
  filtered by `show_all_plans`. Active = `Draft | AwaitingReview`.
- A city with no matching plans shows no children and a dimmed chevron. No
  empty-state row - that is noise once there are twenty cities.
- Plan status renders as a small right-aligned label coloured by status (review
  yellow, approved green, rejected red, draft faint). Drop `plan.summary` and
  the file count from the rail; they belong in the plan review view when that
  gets built.
- Delete `PanelTab`, `ArchitectList`, `PlanList`, `ResourceList`.

Resizer:

- `<div class="sidebar-resizer">` absolutely positioned on the rail's right edge.
- `on:mousedown` records the start x and start width, then attaches window-level
  `mousemove` / `mouseup` listeners via `window_event_listener` (already in the
  leptos prelude), removing them on mouseup and in `on_cleanup`. Window-level
  rather than element-level so the drag survives the pointer crossing onto the
  SVG map, whose own `mousemove` pan handler would otherwise swallow it.
- Clamp width to `200.0..=560.0`.
- Double-click resets to the 290px default.

Persistence:

- Read `localStorage["kingdom.sidebar_width"]` inside an `Effect` (client-only,
  so the SSR markup and hydration agree on the default and there is no flash of
  mismatched layout); write once on mouseup, not on every mousemove.
- Add `"Storage"` and `"Window"` to the `web-sys` features in
  `crates/kingdom-app/Cargo.toml`; guard the calls so a browser with storage
  disabled degrades to the default instead of panicking.

### 3. `style/main.scss`

- Remove `.panel-tabs`, `.panel-tab`, `.tab-glyph`, `.tab-label`, `.tab-count`,
  `.resource-item`, `.res-glyph`, `.contention-line`, `.lease-line`,
  `.plan-foot`, `.plan-status`, `.agent-pip`.
- Keep `.status-working/review/blocked/idle` - the map legend and pips use them.
- Add `.sidebar-toolbar`, `.filter-toggle` (two flat text buttons, active one
  gold), `.city-row`, `.city-chevron`, `.plan-row`, `.plan-badge`, and
  `.sidebar-resizer` (transparent, gold-dim on hover/drag, `cursor: col-resize`).
- `.sidebar` gets `position: relative` for the resizer and keeps its scroll body.
- `body.resizing { user-select: none; }` during a drag, so dragging does not
  smear a text selection across the map.

## Verification

- `cargo leptos serve`, then by hand: drag the edge both ways, release, reload
  and confirm the width is restored; double-click to reset; collapse a city;
  toggle Active/All and confirm approved/rejected plans appear only under All;
  select a city and confirm the map highlight and chat-dock target still follow.
- `cargo fmt`, `cargo clippy`, `cargo test -p kingdom-core`.

## Tests

No new tests. This change is entirely presentation: drag arithmetic, DOM
listener wiring, and a boolean filter over an existing `Vec<Plan>`. There is no
new `kingdom-core` invariant to pin, and the project's policy is to test
behaviour a caller depends on rather than UI plumbing. The existing core tests
(layout non-overlap, lease compatibility matrix) are untouched and must stay
green.
