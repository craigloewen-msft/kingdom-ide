//! The kingdom map: a zoomable, pannable isometric view of the whole realm.
//!
//! This module owns the land, the roads, the camera, and everything drawn
//! *between* cities; [`city`] owns what a single city looks like. The geometry
//! lives in `kingdom_core::{layout, terrain, skyline}` so it stays pure and
//! testable.
//!
//! ## One projection, from the horizon down to a single roof
//!
//! Everything here goes through the same `iso()` the buildings use. That is the
//! whole point of this module's design. Previously each city was drawn in
//! isometric but *positioned* on a flat scatter, so the cities receded in depth
//! internally while the world holding them did not recede at all -- every
//! project read as a diorama pinned to a wall. Sharing the projection is what
//! makes the kingdom one continuous place.

mod city;

use crate::app::KingdomState;
use city::{CityGlyph, Detail};
use kingdom_core::layout::{realm_bounds, settle_kingdom, Realm};
use kingdom_core::skyline::iso;
use kingdom_core::terrain::{contours, Band, BANDS};
use kingdom_core::{City, CityId, Language, PlanStatus};
use leptos::ev;
use leptos::prelude::*;

/// Viewport transform: world coordinates -> screen.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Viewport {
    x: f64,
    y: f64,
    zoom: f64,
}

impl Default for Viewport {
    fn default() -> Self {
        Self {
            x: 0.0,
            y: 0.0,
            zoom: 1.0,
        }
    }
}

/// How the viewBox is mapped onto the rendered element.
///
/// Exists so pointer positions can be turned into world coordinates exactly.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Projection {
    /// CSS pixels per world unit at zoom 1.
    scale: f64,
    /// Letterbox margins left by `preserveAspectRatio="xMidYMid meet"`.
    off_x: f64,
    off_y: f64,
    min_x: f64,
    min_y: f64,
}

impl Projection {
    /// Converts a position in CSS pixels within the element to world units.
    fn to_world(self, css_x: f64, css_y: f64) -> (f64, f64) {
        let s = self.scale.max(f64::MIN_POSITIVE);
        (
            self.min_x + (css_x - self.off_x) / s,
            self.min_y + (css_y - self.off_y) / s,
        )
    }
}

const MIN_ZOOM: f64 = 0.15;
const MAX_ZOOM: f64 = 6.0;

/// Zoom the camera settles at when the King travels to a city.
///
/// Chosen to land inside [`Detail::Streets`] for a typical kingdom, so clicking
/// a city actually arrives at its skyline rather than merely near it.
const VISIT_ZOOM: f64 = 3.2;

/// How many animation steps a camera move takes.
///
/// The camera eases rather than cutting because a cut destroys the one thing
/// the map is for: knowing *where* you are. Watching the land slide under you
/// preserves the spatial relationship between where you were and where you now
/// are, which is the difference between travelling and teleporting.
const TRAVEL_STEPS: u32 = 22;

/// Resolution of the terrain sample grid.
///
/// Measured rather than guessed: 128 costs ~0.7-1.5 ms natively for all five
/// bands, computed once per kingdom in a `Memo`, and yields a coastline whose
/// detail comfortably exceeds what any zoom level here can resolve.
const TERRAIN_RES: usize = 128;

#[component]
pub fn KingdomMap() -> impl IntoView {
    let state = expect_context::<KingdomState>();

    // Declared up front because the wheel handler needs it to measure the
    // element that pointer coordinates are relative to.
    let svg_ref = NodeRef::<leptos::svg::Svg>::new();

    let viewport = RwSignal::new(Viewport::default());
    // Drag origin: pointer position and viewport offset when the drag began.
    let dragging = RwSignal::new(Option::<(f64, f64, f64, f64)>::None);
    // Distinguishes a click from the end of a drag, so releasing the mouse after
    // panning across the sea does not also fling the camera back out.
    let dragged_far = RwSignal::new(false);
    // Rendered size of the map in CSS pixels. Both axes are needed: the viewBox
    // is fitted with `meet`, so which axis constrains the fit decides the scale
    // and how much letterboxing there is.
    let viewport_px = RwSignal::new((1200.0_f64, 800.0_f64));

    let kingdom = Memo::new(move |_| state.kingdom.get());

    // The realm is the expensive part -- terrain fitting, settling, roads -- so
    // it is derived once per kingdom rather than per render.
    let realm = Memo::new(move |_| {
        let k = kingdom.get();
        settle_kingdom(&k.root, &k.cities)
    });

    let bands = Memo::new(move |_| contours(&realm.get().terrain, &BANDS, TERRAIN_RES));

    let bounds = Memo::new(move |_| realm_bounds(&realm.get()));

    // How the browser actually maps the viewBox onto the element.
    //
    // `preserveAspectRatio="xMidYMid meet"` scales by whichever axis is the
    // tighter fit and centres the result, leaving letterbox margins on the
    // other axis. Reproducing that exactly here is what lets pointer positions
    // become world coordinates without drift -- assuming width always
    // constrains, or ignoring the margins, leaves cursor anchoring subtly and
    // permanently off.
    let projection = Memo::new(move |_| {
        let b = bounds.get();
        let (w, h) = viewport_px.get();
        let scale = (w / b.width().max(1.0)).min(h / b.height().max(1.0));
        Projection {
            scale,
            off_x: (w - b.width() * scale) / 2.0,
            off_y: (h - b.height() * scale) / 2.0,
            min_x: b.min_x,
            min_y: b.min_y,
        }
    });

    // World units per CSS pixel at zoom 1.
    let world_per_px = Memo::new(move |_| 1.0 / projection.get().scale.max(f64::MIN_POSITIVE));

    // Level of detail follows how big a city actually looks on screen, not the
    // raw zoom. The viewBox auto-fits the realm, so the same zoom value means
    // very different apparent sizes in a 6-city and a 60-city kingdom; keying
    // off zoom alone would hide the skyline exactly when there is room for it.
    let detail = Memo::new(move |_| {
        let r = realm.get();
        if r.placements.is_empty() {
            return Detail::Streets;
        }

        let median_radius = {
            let mut radii: Vec<f64> = r.placements.iter().map(|p| p.radius).collect();
            radii.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            radii[radii.len() / 2]
        };

        let px_per_world = 1.0 / world_per_px.get().max(f64::MIN_POSITIVE);
        Detail::for_city_size(median_radius * 2.0 * px_per_world * viewport.get().zoom)
    });

    let view_box = Memo::new(move |_| {
        let b = bounds.get();
        format!("{} {} {} {}", b.min_x, b.min_y, b.width(), b.height())
    });

    let transform = move || {
        let v = viewport.get();
        format!("translate({} {}) scale({})", v.x, v.y, v.zoom)
    };

    // Zoom about the pointer rather than the world origin. Anchoring to the
    // origin makes the land slide out from under the cursor, which is the
    // single clearest tell that a view is a picture rather than a map.
    let on_wheel = move |ev: ev::WheelEvent| {
        ev.prevent_default();

        // Deliberately *not* `offset_x`: that is measured from whatever element
        // the event happened to land on -- usually the ocean polygon -- so it
        // carries that element's own origin and leaves a constant drift.
        let Some(el) = svg_ref.get_untracked() else {
            return;
        };
        let rect = el.get_bounding_client_rect();
        let p = projection.get();
        let (px, py) = p.to_world(
            ev.client_x() as f64 - rect.left(),
            ev.client_y() as f64 - rect.top(),
        );

        viewport.update(|v| {
            let factor = if ev.delta_y() < 0.0 { 1.12 } else { 1.0 / 1.12 };
            let next = (v.zoom * factor).clamp(MIN_ZOOM, MAX_ZOOM);
            // Keep the world point under the cursor fixed: solve
            // (p - x)/zoom == (p - x')/zoom' for the new offset.
            v.x = px - (px - v.x) * (next / v.zoom);
            v.y = py - (py - v.y) * (next / v.zoom);
            v.zoom = next;
        });
    };

    let on_mouse_down = move |ev: ev::MouseEvent| {
        let v = viewport.get();
        dragged_far.set(false);
        dragging.set(Some((ev.client_x() as f64, ev.client_y() as f64, v.x, v.y)));
    };

    // Pan 1:1 with the cursor. The transform is applied in viewBox units, so
    // screen pixels must be converted or the land lags the pointer by the
    // viewBox-to-pixel ratio -- a different amount in every kingdom.
    let on_mouse_move = move |ev: ev::MouseEvent| {
        if let Some((sx, sy, ox, oy)) = dragging.get() {
            let scale = world_per_px.get();
            let dx = (ev.client_x() as f64 - sx) * scale;
            let dy = (ev.client_y() as f64 - sy) * scale;

            if dx.abs() + dy.abs() > scale * 4.0 {
                dragged_far.set(true);
            }

            viewport.update(|v| {
                v.x = ox + dx;
                v.y = oy + dy;
            });
        }
    };

    let end_drag = move |_| dragging.set(None);

    // --- Camera -------------------------------------------------------------
    //
    // Travel is animated by stepping a signal on a timer rather than by CSS
    // transition, because the viewport also drives the level-of-detail memo:
    // the tier has to switch *during* the flight, so the skyline is already
    // there when the camera arrives.
    let travel = move |target: Option<(f64, f64)>| {
        let from = viewport.get_untracked();
        let b = bounds.get_untracked();

        let to = match target {
            Some((gx, gy)) => {
                // Centre the city: the viewBox centre is where the camera puts
                // whatever it is looking at.
                let (cx, cy) = b.center();
                let (sx, sy) = iso(gx, gy, 0.0);
                Viewport {
                    x: cx - sx * VISIT_ZOOM,
                    y: cy - sy * VISIT_ZOOM,
                    zoom: VISIT_ZOOM,
                }
            }
            None => Viewport::default(),
        };

        let step = RwSignal::new(0u32);
        let tick = move || {
            let i = step.get_untracked() + 1;
            step.set(i);
            let t = (i as f64 / TRAVEL_STEPS as f64).min(1.0);
            // Cubic ease-in-out: accelerate away, settle gently on arrival.
            let e = if t < 0.5 {
                4.0 * t * t * t
            } else {
                1.0 - (-2.0 * t + 2.0).powi(3) / 2.0
            };
            viewport.set(Viewport {
                x: from.x + (to.x - from.x) * e,
                y: from.y + (to.y - from.y) * e,
                zoom: from.zoom + (to.zoom - from.zoom) * e,
            });
            i < TRAVEL_STEPS
        };

        set_interval_stepper(tick);
    };

    // A `Callback` rather than a bare closure: components must be `Send` for
    // server-side rendering, and `Callback` provides that by storing the
    // function in the reactive arena instead of capturing it directly.
    //
    // Clicking a city travels to it; clicking open country pulls back out. That
    // pairing is what makes zoom feel like moving through a world rather than
    // scaling a picture.
    let visit: Callback<CityId> = Callback::new(move |id: CityId| {
        let k = kingdom.get_untracked();
        if let Some(i) = k.cities.iter().position(|c| c.id == id) {
            if let Some(p) = realm.get_untracked().placements.get(i) {
                travel(Some((p.x, p.y)));
            }
        }
        state.selected.set(Some(id));
    });

    let leave = move |_| {
        if dragging.get_untracked().is_some() || dragged_far.get_untracked() {
            return;
        }
        state.selected.set(None);
        travel(None);
    };

    let zoom_by = move |factor: f64| {
        viewport.update(|v| v.zoom = (v.zoom * factor).clamp(MIN_ZOOM, MAX_ZOOM));
    };

    // Track the SVG's real size so the detail tier and the pointer maths both
    // reflect what the King can actually see rather than an assumed viewport.
    Effect::new(move |_| {
        if let Some(el) = svg_ref.get() {
            let rect = el.get_bounding_client_rect();
            if rect.width() > 0.0 && rect.height() > 0.0 {
                viewport_px.set((rect.width(), rect.height()));
            }
        }
    });

    view! {
        <div class="map-wrap">
            <svg
                class="kingdom-svg"
                node_ref=svg_ref
                viewBox=move || view_box.get()
                preserveAspectRatio="xMidYMid meet"
                on:wheel=on_wheel
                on:mousedown=on_mouse_down
                on:mousemove=on_mouse_move
                on:mouseup=end_drag
                on:mouseleave=end_drag
                on:click=leave
                class:grabbing=move || dragging.get().is_some()
            >
                <defs>
                    <radialGradient id="realm-glow" cx="50%" cy="50%" r="50%">
                        <stop offset="0%" stop-color="#1e293b" stop-opacity="0.9"/>
                        <stop offset="100%" stop-color="#0b1120" stop-opacity="0"/>
                    </radialGradient>
                    // The sea's own shimmer, so open water is not a flat void.
                    <radialGradient id="sea-sheen" cx="50%" cy="45%" r="62%">
                        <stop offset="0%" stop-color="#12233f" stop-opacity="1"/>
                        <stop offset="100%" stop-color="#070d18" stop-opacity="1"/>
                    </radialGradient>
                </defs>

                // Everything lives inside the camera transform. The backdrop
                // used to sit outside it, so the cities slid across a fixed
                // grid when panning -- the fastest possible way to make ground
                // read as a UI panel rather than as land.
                <g transform=transform>
                    <Ocean realm=realm/>
                    <Land bands=bands/>
                    <Roads realm=realm/>
                    <Throne realm=realm/>
                    <Cities realm=realm detail=detail visit=visit/>
                </g>
            </svg>

            <div class="map-controls">
                <button on:click=move |_| zoom_by(1.25) title="Zoom in">"+"</button>
                <button on:click=move |_| zoom_by(0.8) title="Zoom out">"−"</button>
                <button
                    on:click=move |_| { state.selected.set(None); travel(None) }
                    title="Survey the whole realm"
                >"⌂"</button>
            </div>

            <div class="map-legend">
                // Driven from the enum rather than hand-listed, so a new plan
                // state cannot appear on the map without appearing here too.
                <div class="legend-row">
                    {PlanStatus::ALL.iter().map(|s| view! {
                        <span>
                            <i class=format!("dot status-{}", s.css_suffix())></i>
                            {s.label()}
                        </span>
                    }).collect_view()}
                </div>
                // Ward colours: what the code *is*, as opposed to what is being
                // proposed for it.
                <div class="legend-row wards">
                    {Language::ALL.iter().map(|w| view! {
                        <span>
                            <i class="dot" style:background=w.tint()></i>
                            {w.label()}
                        </span>
                    }).collect_view()}
                </div>
            </div>

            <div class="map-zoomhint">
                {move || match detail.get() {
                    Detail::Realm => "The realm — click a city to travel there",
                    Detail::Province => "Districts — zoom in for streets",
                    Detail::Streets => "Streets — click open country to pull back",
                }}
            </div>
        </div>
    }
}

/// Runs `tick` on an animation timer until it returns false.
///
/// `requestAnimationFrame` would be smoother, but it needs a wasm-only closure
/// dance; a short interval is honest, works identically under SSR hydration,
/// and the camera move is 22 steps.
fn set_interval_stepper(tick: impl FnMut() -> bool + Send + Sync + 'static) {
    use std::sync::{Arc, Mutex};

    // `set_interval_with_handle` takes an `Fn`, but stepping an animation is
    // inherently stateful, so the state lives behind a lock rather than in the
    // closure's captures. The handle is stored the same way so the interval can
    // cancel itself from inside.
    let tick = Arc::new(Mutex::new(tick));
    let handle: Arc<Mutex<Option<leptos::leptos_dom::helpers::IntervalHandle>>> =
        Arc::new(Mutex::new(None));
    let inner = handle.clone();

    let result = leptos::leptos_dom::helpers::set_interval_with_handle(
        move || {
            let keep_going = tick.lock().map(|mut t| t()).unwrap_or(false);
            if !keep_going {
                if let Some(h) = inner.lock().ok().and_then(|mut g| g.take()) {
                    h.clear();
                }
            }
        },
        std::time::Duration::from_millis(16),
    );

    if let Ok(h) = result {
        if let Ok(mut g) = handle.lock() {
            *g = Some(h);
        }
    }
}

/// The sea: everything outside the coastline.
///
/// Drawn as one big quad under the land rather than as its own contour, because
/// the sea is simply "not land" and giving it a shape of its own would mean two
/// sources of truth for one boundary.
#[component]
fn Ocean(realm: Memo<Realm>) -> impl IntoView {
    let points = move || {
        // Generously beyond the sampled span, so panning never reaches an edge.
        let s = realm.get().terrain.span() * 2.4;
        [(-s, -s), (s, -s), (s, s), (-s, s)]
            .iter()
            .map(|(x, y)| {
                let (sx, sy) = iso(*x, *y, 0.0);
                format!("{sx:.1},{sy:.1}")
            })
            .collect::<Vec<_>>()
            .join(" ")
    };

    view! { <polygon class="ocean" points=points fill="url(#sea-sheen)"/> }
}

/// The land: nested elevation bands, shore first.
#[component]
fn Land(bands: Memo<Vec<Band>>) -> impl IntoView {
    view! {
        <g class="land">
            {move || bands.get().into_iter().enumerate().map(|(i, band)| {
                // All of a band's rings go in one path with evenodd, so nested
                // rings punch out as lakes and inlets without any winding
                // bookkeeping here.
                let d = band
                    .rings
                    .iter()
                    .map(|r| ring_path(r))
                    .collect::<Vec<_>>()
                    .join(" ");
                view! {
                    <path
                        class=format!("land-band band-{i}")
                        d=d
                        fill-rule="evenodd"
                    />
                }
            }).collect_view()}
        </g>
    }
}

/// One closed ring as an SVG path, projected onto the ground plane.
fn ring_path(ring: &[(f64, f64)]) -> String {
    let mut d = String::with_capacity(ring.len() * 12);
    for (i, (x, y)) in ring.iter().enumerate() {
        let (sx, sy) = iso(*x, *y, 0.0);
        if i == 0 {
            d.push_str(&format!("M{sx:.1} {sy:.1}"));
        } else {
            d.push_str(&format!("L{sx:.1} {sy:.1}"));
        }
    }
    d.push('Z');
    d
}

/// The roads between cities.
///
/// These are the connective tissue the map was missing. A kingdom whose cities
/// are visibly *joined* stops reading as scattered islands, which was the whole
/// complaint; the spanning tree in `layout` guarantees there is always a route.
#[component]
fn Roads(realm: Memo<Realm>) -> impl IntoView {
    let paths = Memo::new(move |_| {
        let r = realm.get();
        r.roads
            .iter()
            .filter_map(|road| {
                let a = r.placements.get(road.from)?;
                let b = r.placements.get(road.to)?;

                // Bend the control point perpendicular to the route, so roads
                // curve across country instead of ruling straight lines.
                let (dx, dy) = (b.x - a.x, b.y - a.y);
                let len = (dx * dx + dy * dy).sqrt().max(1.0);
                let (nx, ny) = (-dy / len, dx / len);
                let mx = (a.x + b.x) / 2.0 + nx * road.bend;
                let my = (a.y + b.y) / 2.0 + ny * road.bend;

                let (ax, ay) = iso(a.x, a.y, a.elevation);
                let (bx, by) = iso(b.x, b.y, b.elevation);
                let (cx, cy) = iso(mx, my, (a.elevation + b.elevation) / 2.0);

                Some(format!("M{ax:.1} {ay:.1} Q{cx:.1} {cy:.1} {bx:.1} {by:.1}"))
            })
            .collect::<Vec<_>>()
    });

    view! {
        <g class="roads">
            // Two passes, not two children per road: every casing must be under
            // every surface, or a road crossing another shows its dark casing
            // cutting through the neighbour's metalled top.
            {move || paths.get().into_iter().map(|d| view! {
                <path class="road-casing" d=d/>
            }).collect_view()}
            {move || paths.get().into_iter().map(|d| view! {
                <path class="road-surface" d=d/>
            }).collect_view()}
        </g>
    }
}

/// The throne at the centre of the realm: the King's own seat, on high ground.
#[component]
fn Throne(realm: Memo<Realm>) -> impl IntoView {
    let pos = move || {
        let t = realm.get().terrain;
        iso(0.0, 0.0, t.height(0.0, 0.0))
    };

    view! {
        <g
            class="throne"
            transform=move || {
                let (x, y) = pos();
                format!("translate({x:.1} {y:.1})")
            }
        >
            <circle r="120" fill="url(#realm-glow)"/>
            <circle r="34" class="throne-ring"/>
            <text class="throne-glyph" text-anchor="middle" dy="12">"♚"</text>
            <text class="throne-label" text-anchor="middle" dy="58">"THRONE"</text>
        </g>
    }
}

/// All the cities of the realm, painted back to front.
#[component]
fn Cities(realm: Memo<Realm>, detail: Memo<Detail>, visit: Callback<CityId>) -> impl IntoView {
    let state = expect_context::<KingdomState>();

    let entries = Memo::new(move |_| {
        let r = realm.get();
        let mut list: Vec<(usize, City, kingdom_core::layout::CityPlacement)> = state
            .kingdom
            .get()
            .cities
            .into_iter()
            .zip(r.placements.iter().copied())
            .enumerate()
            .map(|(i, (c, p))| (i, c, p))
            .collect();

        // SVG has no depth buffer, so draw order is the depth cue -- the same
        // reason `skyline::order_for_painting` exists, one level up. On a flat
        // scatter this did not matter and the cities were drawn alphabetically;
        // on a landscape with elevation, a far city would visibly punch through
        // a near one.
        list.sort_by(|(ia, _, a), (ib, _, b)| {
            (a.x + a.y)
                .partial_cmp(&(b.x + b.y))
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(ia.cmp(ib))
        });

        list
    });

    view! {
        <For
            each=move || entries.get()
            key=|(_, c, _): &(usize, City, kingdom_core::layout::CityPlacement)| c.id.clone()
            let:entry
        >
            {
                let (_, city, place) = entry;
                view! { <CityGlyph city=city place=place detail=detail visit=visit/> }
            }
        </For>
    }
}
