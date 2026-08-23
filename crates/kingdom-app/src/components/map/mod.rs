//! The kingdom map: a zoomable, pannable SVG view of every city.
//!
//! This module owns the viewport and the signals drawn *between* cities;
//! [`city`] owns what a single city looks like. The layout maths lives in
//! `kingdom_core::{layout, skyline}` so it stays pure and testable.

mod city;

use crate::app::KingdomState;
use city::{CityGlyph, Detail};
use kingdom_core::layout::{bounds_of, spiral_layout, CityPlacement};
use kingdom_core::{City, CityId, Ward};
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

const MIN_ZOOM: f64 = 0.15;
const MAX_ZOOM: f64 = 4.0;

#[component]
pub fn KingdomMap() -> impl IntoView {
    let state = expect_context::<KingdomState>();

    let viewport = RwSignal::new(Viewport::default());
    // Drag origin: pointer position and viewport offset when the drag began.
    let dragging = RwSignal::new(Option::<(f64, f64, f64, f64)>::None);
    // Rendered width of the map in CSS pixels, needed to turn world units into
    // apparent size. Seeded with a sane default for the server-rendered pass.
    let viewport_px = RwSignal::new(1200.0_f64);

    let cities = Memo::new(move |_| state.kingdom.get().cities);
    let placements = Memo::new(move |_| spiral_layout(&cities.get()));

    // Level of detail follows how big a city actually looks on screen, not the
    // raw zoom. The viewBox auto-fits the kingdom, so the same zoom value means
    // very different apparent sizes in a 6-city and a 60-city kingdom; keying
    // off zoom alone would hide the skyline exactly when there is room for it.
    let detail = Memo::new(move |_| {
        let places = placements.get();
        if places.is_empty() {
            return Detail::Full;
        }

        let b = bounds_of(&places);
        // The viewBox is fitted to the kingdom's width, so world units map to
        // screen pixels by roughly (viewport width / kingdom width).
        let world_to_px = if b.width() > 0.0 {
            viewport_px.get() / b.width()
        } else {
            1.0
        };

        let median_radius = {
            let mut radii: Vec<f64> = places.iter().map(|p| p.radius).collect();
            radii.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            radii[radii.len() / 2]
        };

        Detail::for_city_size(median_radius * 2.0 * world_to_px * viewport.get().zoom)
    });

    // The SVG viewBox is sized to the kingdom's natural extent; zoom and pan
    // are then applied as a transform on the contents. This keeps the initial
    // framing automatic regardless of how many cities there are.
    let view_box = Memo::new(move |_| {
        let b = bounds_of(&placements.get());
        format!("{} {} {} {}", b.min_x, b.min_y, b.width(), b.height())
    });

    let transform = move || {
        let v = viewport.get();
        format!("translate({} {}) scale({})", v.x, v.y, v.zoom)
    };

    let on_wheel = move |ev: ev::WheelEvent| {
        ev.prevent_default();
        viewport.update(|v| {
            let factor = if ev.delta_y() < 0.0 { 1.12 } else { 1.0 / 1.12 };
            v.zoom = (v.zoom * factor).clamp(MIN_ZOOM, MAX_ZOOM);
        });
    };

    let on_mouse_down = move |ev: ev::MouseEvent| {
        let v = viewport.get();
        dragging.set(Some((ev.client_x() as f64, ev.client_y() as f64, v.x, v.y)));
    };

    let on_mouse_move = move |ev: ev::MouseEvent| {
        if let Some((sx, sy, ox, oy)) = dragging.get() {
            let dx = ev.client_x() as f64 - sx;
            let dy = ev.client_y() as f64 - sy;
            viewport.update(|v| {
                v.x = ox + dx;
                v.y = oy + dy;
            });
        }
    };

    let end_drag = move |_| dragging.set(None);

    let zoom_by = move |factor: f64| {
        viewport.update(|v| v.zoom = (v.zoom * factor).clamp(MIN_ZOOM, MAX_ZOOM));
    };

    // Track the SVG's real width so the detail tier reflects what the King can
    // actually see rather than an assumed viewport.
    let svg_ref = NodeRef::<leptos::svg::Svg>::new();
    Effect::new(move |_| {
        if let Some(el) = svg_ref.get() {
            let width = el.get_bounding_client_rect().width();
            if width > 0.0 {
                viewport_px.set(width);
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
                class:grabbing=move || dragging.get().is_some()
            >
                <defs>
                    <radialGradient id="realm-glow" cx="50%" cy="50%" r="50%">
                        <stop offset="0%" stop-color="#1e293b" stop-opacity="0.9"/>
                        <stop offset="100%" stop-color="#0b1120" stop-opacity="0"/>
                    </radialGradient>
                    <pattern
                        id="terrain"
                        width="48"
                        height="48"
                        patternUnits="userSpaceOnUse"
                    >
                        <path
                            d="M 48 0 L 0 0 0 48"
                            fill="none"
                            stroke="#1e293b"
                            stroke-width="1"
                            opacity="0.5"
                        />
                    </pattern>
                </defs>

                <rect
                    x="-6000" y="-6000" width="12000" height="12000"
                    fill="url(#terrain)"
                />

                <g transform=transform>
                    <ContentionThreads placements=placements/>
                    <Throne/>
                    <Cities placements=placements detail=detail/>
                </g>
            </svg>

            <div class="map-controls">
                <button on:click=move |_| zoom_by(1.25) title="Zoom in">"+"</button>
                <button on:click=move |_| zoom_by(0.8) title="Zoom out">"−"</button>
                <button
                    on:click=move |_| viewport.set(Viewport::default())
                    title="Reset view"
                >"⌂"</button>
            </div>

            <div class="map-legend">
                <div class="legend-row">
                    <span><i class="dot status-working"></i>"Working"</span>
                    <span><i class="dot status-review"></i>"Awaiting review"</span>
                    <span><i class="dot status-blocked"></i>"Blocked"</span>
                    <span><i class="dot status-idle"></i>"Idle"</span>
                </div>
                // Ward colours: what the code *is*, as opposed to what an agent
                // is doing to it.
                <div class="legend-row wards">
                    {Ward::ALL.iter().map(|w| view! {
                        <span>
                            <i class="dot" style:background=w.tint()></i>
                            {w.label()}
                        </span>
                    }).collect_view()}
                </div>
            </div>

            <div class="map-zoomhint">
                {move || match detail.get() {
                    Detail::Distant => "Zoom in to see districts",
                    Detail::Districts => "Zoom in to see buildings",
                    Detail::Full => "Full detail",
                }}
            </div>
        </div>
    }
}

/// The throne at the centre of the realm: the King's own seat.
#[component]
fn Throne() -> impl IntoView {
    view! {
        <g class="throne">
            <circle r="120" fill="url(#realm-glow)"/>
            <circle r="34" class="throne-ring"/>
            <text class="throne-glyph" text-anchor="middle" dy="12">"♚"</text>
            <text class="throne-label" text-anchor="middle" dy="58">"THRONE"</text>
        </g>
    }
}

/// Red threads between cities whose architects are contending for the same
/// crown resource. This is the map's most important signal.
#[component]
fn ContentionThreads(placements: Memo<Vec<CityPlacement>>) -> impl IntoView {
    let state = expect_context::<KingdomState>();

    let threads = Memo::new(move |_| {
        let kingdom = state.kingdom.get();
        let places = placements.get();
        let mut lines: Vec<(f64, f64, f64, f64)> = Vec::new();

        // Index of city -> placement, by architect.
        let city_index = |id: &CityId| kingdom.cities.iter().position(|c| &c.id == id);
        let architect_city = |aid: &kingdom_core::ArchitectId| {
            kingdom
                .architects
                .iter()
                .find(|a| &a.id == aid)
                .and_then(|a| city_index(&a.city))
        };

        for resource in kingdom.contended_resources() {
            for holder in &resource.holders {
                for waiter in &resource.waiting {
                    if let (Some(h), Some(w)) =
                        (architect_city(&holder.holder), architect_city(waiter))
                    {
                        if h != w {
                            if let (Some(a), Some(b)) = (places.get(h), places.get(w)) {
                                lines.push((a.x, a.y, b.x, b.y));
                            }
                        }
                    }
                }
            }
        }

        lines
    });

    view! {
        <g class="contention-threads">
            <For
                each={move || threads.get().into_iter().enumerate().collect::<Vec<_>>()}
                key=|(i, _)| *i
                let:entry
            >
                {
                    let (_, (x1, y1, x2, y2)) = entry;
                    view! {
                        <line
                            x1=x1 y1=y1 x2=x2 y2=y2
                            class="contention-thread"
                        />
                    }
                }
            </For>
        </g>
    }
}

/// All the cities of the realm.
#[component]
fn Cities(placements: Memo<Vec<CityPlacement>>, detail: Memo<Detail>) -> impl IntoView {
    let state = expect_context::<KingdomState>();

    let entries = Memo::new(move |_| {
        state
            .kingdom
            .get()
            .cities
            .into_iter()
            .zip(placements.get())
            .collect::<Vec<(City, CityPlacement)>>()
    });

    view! {
        <For
            each=move || entries.get()
            key=|(c, _): &(City, CityPlacement)| c.id.clone()
            let:entry
        >
            {
                let (city, place) = entry;
                view! { <CityGlyph city=city place=place detail=detail/> }
            }
        </For>
    }
}
