//! The kingdom map: a zoomable, pannable SVG view of every city.
//!
//! Deliberately simple for now — flat SVG, no canvas, no WebGL. The layout
//! maths lives in `kingdom_core::layout` so it stays pure and testable; this
//! module is only concerned with turning placements into shapes and wiring up
//! the viewport transform.

use crate::app::KingdomState;
use kingdom_core::layout::{bounds_of, spiral_layout, CityPlacement};
use kingdom_core::{ArchitectStatus, City, CityId};
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

    let cities = Memo::new(move |_| state.kingdom.get().cities);
    let placements = Memo::new(move |_| spiral_layout(&cities.get()));

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

    view! {
        <div class="map-wrap">
            <svg
                class="kingdom-svg"
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
                    <Cities placements=placements/>
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
                <span><i class="dot status-working"></i>"Working"</span>
                <span><i class="dot status-review"></i>"Awaiting review"</span>
                <span><i class="dot status-blocked"></i>"Blocked"</span>
                <span><i class="dot status-idle"></i>"Idle"</span>
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
fn Cities(placements: Memo<Vec<CityPlacement>>) -> impl IntoView {
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
                view! { <CityGlyph city=city place=place/> }
            }
        </For>
    }
}

/// A single city: a keep with towers, banners for its stack, and a ring of
/// architect pips showing who is working there and in what state.
#[component]
fn CityGlyph(city: City, place: CityPlacement) -> impl IntoView {
    let state = expect_context::<KingdomState>();

    let id = city.id.clone();
    let is_selected = {
        let id = id.clone();
        move || state.selected.get().as_ref() == Some(&id)
    };

    let architects = {
        let id = id.clone();
        Memo::new(move |_| {
            state
                .kingdom
                .get()
                .architects_in(&id)
                .cloned()
                .collect::<Vec<_>>()
        })
    };

    // A city is "astir" when anyone is actively working there; it gets a halo.
    let astir = move || {
        architects
            .get()
            .iter()
            .any(|a| a.status == ArchitectStatus::Working)
    };
    let troubled = move || {
        architects
            .get()
            .iter()
            .any(|a| a.status == ArchitectStatus::Blocked)
    };

    let r = place.radius;
    let select = move |_| state.selected.set(Some(id.clone()));

    view! {
        <g
            class="city"
            class:selected=is_selected
            class:astir=astir
            class:troubled=troubled
            transform=format!("translate({} {})", place.x, place.y)
            on:click=select
        >
            // Halo for active cities.
            <Show when=astir>
                <circle class="city-halo" r=r + 18.0/>
            </Show>

            <circle class="city-plot" r=r/>

            // The keep: a simple blocky silhouette, scaled to the city.
            <g class="keep" transform=format!("scale({})", r / 48.0)>
                <rect x="-22" y="-6" width="44" height="30" rx="2" class="keep-body"/>
                <rect x="-30" y="-18" width="13" height="42" rx="2" class="keep-tower"/>
                <rect x="17" y="-18" width="13" height="42" rx="2" class="keep-tower"/>
                <polygon points="-30,-18 -23.5,-30 -17,-18" class="keep-roof"/>
                <polygon points="17,-18 23.5,-30 30,-18" class="keep-roof"/>
                <rect x="-5" y="8" width="10" height="16" rx="1" class="keep-gate"/>

                // Banner in the city's stack colour.
                <line x1="0" y1="-6" x2="0" y2="-40" class="banner-pole"/>
                <polygon
                    points="0,-40 20,-34 0,-28"
                    fill=city.kind.banner_color()
                    class="banner-flag"
                />
            </g>

            <text class="city-name" text-anchor="middle" y=r + 26.0>
                {city.name.clone()}
            </text>

            // Architect pips, arranged along the top arc of the plot.
            <g class="architect-ring">
                <For
                    each={move || architects.get().into_iter().enumerate().collect::<Vec<_>>()}
                    key=|(_, a)| a.id.clone()
                    let:entry
                >
                    {
                        let (i, architect) = entry;
                        let total = architects.get().len().max(1);
                        // Spread pips across a 120-degree arc above the keep.
                        let spread = 2.094_395_1_f64;
                        let start = -std::f64::consts::FRAC_PI_2 - spread / 2.0;
                        let step = if total > 1 {
                            spread / (total - 1) as f64
                        } else {
                            0.0
                        };
                        let angle = start + step * i as f64;
                        let ring = r + 14.0;

                        view! {
                            <circle
                                class=format!("pip status-{}", architect.status.css_suffix())
                                cx=ring * angle.cos()
                                cy=ring * angle.sin()
                                r="6"
                            >
                                <title>
                                    {format!(
                                        "{} \u{2014} {}: {}",
                                        architect.name,
                                        architect.status.label(),
                                        architect.activity,
                                    )}
                                </title>
                            </circle>
                        }
                    }
                </For>
            </g>
        </g>
    }
}
