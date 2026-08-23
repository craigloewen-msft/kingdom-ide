//! Rendering one city as an isometric skyline built from its own file tree.
//!
//! The geometry is decided in [`kingdom_core::skyline`]; this module only turns
//! it into SVG. That split is deliberate: placement must stay pure and testable
//! (a building that moves between reloads destroys the King's spatial memory),
//! while everything here is presentation and may change freely.
//!
//! ## Why the skyline earns its place on the map
//!
//! Cities used to be one generic keep, identical for a 30-file library and a
//! 5,000-file monorepo, so "Vitruvius is refactoring the auth module" had
//! nowhere on screen to *be*. Giving every file a stable location turns agent
//! activity from prose into geography: the district glows, the exact building a
//! plan would touch is gilded, and contention threads land on the district in
//! dispute rather than on a vague circle.

use crate::app::KingdomState;
use kingdom_core::layout::CityPlacement;
use kingdom_core::skyline::{iso, Lot, LotKind, Plate, Skyline};
use kingdom_core::{ArchitectStatus, City, CityId, PlanStatus};
use leptos::prelude::*;
use std::collections::HashSet;

/// How much of a building's side face is darkened, per side.
///
/// The gap between the three faces is what sells the extrusion; too little and
/// dense clusters flatten into a wall of colour.
const LEFT_SHADE: f64 = 0.55;
const RIGHT_SHADE: f64 = 0.30;

/// Detail tiers. A 40-city kingdom cannot render thousands of buildings at
/// once, and at low zoom they would be sub-pixel noise anyway.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Detail {
    /// The realm seen whole: a city is a mass and a banner.
    Realm,
    /// District plates and landmarks, but no individual buildings.
    Province,
    /// The full skyline, street by street.
    Streets,
}

impl Detail {
    /// Chooses a tier from how large a city actually appears on screen.
    ///
    /// Keying off raw zoom would be wrong: the map auto-fits the realm, so
    /// zoom 1.0 means "six cities, each huge" in one kingdom and "sixty cities,
    /// each tiny" in another. Using apparent size instead means a small kingdom
    /// shows its skyline immediately -- the whole point of the view -- while a
    /// large one still degrades to silhouettes before it can bury the browser.
    pub fn for_city_size(city_px: f64) -> Detail {
        if city_px < 26.0 {
            Detail::Realm
        } else if city_px < 68.0 {
            Detail::Province
        } else {
            Detail::Streets
        }
    }
}

/// A single city, drawn as a living metropolis standing on the realm's ground.
#[component]
pub fn CityGlyph(
    city: City,
    place: CityPlacement,
    detail: Memo<Detail>,
    visit: Callback<CityId>,
) -> impl IntoView {
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

    // Paths a pending plan proposes to touch: these buildings get gilded, which
    // is what makes "what am I being asked to approve?" a place on the map.
    let under_plan = {
        let id = id.clone();
        Memo::new(move |_| {
            state
                .kingdom
                .get()
                .plans
                .iter()
                .filter(|p| p.city == id && p.status != PlanStatus::Rejected)
                .flat_map(|p| p.touches.iter().cloned())
                .collect::<HashSet<String>>()
        })
    };

    let r = place.radius;
    let skyline = StoredValue::new(
        city.structure
            .as_ref()
            .map(|s| kingdom_core::skyline::build_skyline(s, r))
            .unwrap_or_default(),
    );
    let has_skyline = skyline.with_value(|s| !s.lots.is_empty());

    let select = {
        let id = id.clone();
        move |ev: leptos::ev::MouseEvent| {
            // The map's own click handler pulls the camera back out to survey
            // the realm; without this a click on a city would immediately be
            // undone by the click on the land behind it.
            ev.stop_propagation();
            visit.run(id.clone());
        }
    };

    let city_name = city.name.clone();
    let banner = city.kind.banner_color();
    let has_git = city.has_git;
    let file_count = city.file_count;

    view! {
        <g
            class="city"
            class:selected=is_selected
            class:astir=astir
            class:troubled=troubled
            // The city stands on the ground plane at its own elevation, in the
            // same isometric space as the terrain and every building inside it.
            transform={
                let (sx, sy) = iso(place.x, place.y, place.elevation);
                format!("translate({sx:.2} {sy:.2})")
            }
            on:click=select
        >
            <Show when=astir>
                <circle class="city-halo" r=r + 20.0/>
            </Show>

            // The ground the city stands on, as an isometric diamond.
            <polygon class="city-plot" points=diamond(r)/>

            <Show when={move || has_git}>
                <polygon class="city-walls" points=diamond(r * 0.94)/>
            </Show>

            {move || {
                let d = detail.get();
                if !has_skyline {
                    // Nothing scanned: fall back to a keep so the city is still
                    // legible rather than an empty patch of ground.
                    return view! { <BareKeep radius=r/> }.into_any();
                }
                match d {
                    Detail::Realm => view! {
                        <Silhouette skyline=skyline/>
                    }.into_any(),
                    Detail::Province => view! {
                        <Districts skyline=skyline show_labels=false/>
                        <Landmark skyline=skyline/>
                    }.into_any(),
                    Detail::Streets => view! {
                        <Districts skyline=skyline show_labels=true/>
                        <Buildings skyline=skyline under_plan=under_plan/>
                    }.into_any(),
                }
            }}

            // Cranes mark cities where an architect is actively building.
            <Show when=astir>
                <Crane radius=r/>
            </Show>

            <Banner radius=r color=banner/>

            <text class="city-name" text-anchor="middle" y=r * 0.62 + 26.0>
                {city_name.clone()}
            </text>
            <Show when={move || detail.get() != Detail::Realm}>
                <text class="city-meta" text-anchor="middle" y=r * 0.62 + 40.0>
                    {format!("{file_count} files")}
                </text>
            </Show>

            <ArchitectPips architects=architects radius=r/>
        </g>
    }
}

/// The isometric diamond of a city's ground plot.
fn diamond(r: f64) -> String {
    let (a, b) = iso(-r * 0.72, -r * 0.72, 0.0);
    let (c, d) = iso(r * 0.72, -r * 0.72, 0.0);
    let (e, f) = iso(r * 0.72, r * 0.72, 0.0);
    let (g, h) = iso(-r * 0.72, r * 0.72, 0.0);
    format!("{a:.1},{b:.1} {c:.1},{d:.1} {e:.1},{f:.1} {g:.1},{h:.1}")
}

/// District plates: the folders, drawn as tinted ground.
#[component]
fn Districts(skyline: StoredValue<Skyline>, show_labels: bool) -> impl IntoView {
    let plates = skyline.with_value(|s| s.plates.clone());

    view! {
        <g class="districts">
            {plates.into_iter().map(|plate| {
                let points = plate_points(&plate);
                let tint = plate.ward.tint();
                // Deeper plates sit brighter so nesting reads as depth. The
                // folder-as-district metaphor is the core semantic content of
                // the map, so plates have to be legible, not merely present.
                let opacity = 0.20 + (plate.level as f64) * 0.10;
                let label = (show_labels && plate.level > 0 && plate.width > 16.0)
                    .then(|| {
                        let (lx, ly) = iso(
                            plate.x - plate.width / 2.0 + 2.0,
                            plate.y - plate.depth / 2.0 + 2.0,
                            0.0,
                        );
                        view! {
                            <text class="district-label" x=lx y=ly>
                                {plate.name.clone()}
                            </text>
                        }
                    });

                view! {
                    <g class="district">
                        <polygon
                            class="district-plate"
                            points=points
                            fill=tint
                            fill-opacity=opacity
                            stroke=tint
                        >
                            <title>{format!("{} \u{2014} {} files", plate.path, plate.files)}</title>
                        </polygon>
                        {label}
                    </g>
                }
            }).collect_view()}
        </g>
    }
}

fn plate_points(plate: &Plate) -> String {
    let hw = plate.width / 2.0;
    let hd = plate.depth / 2.0;
    let corners = [
        (plate.x - hw, plate.y - hd),
        (plate.x + hw, plate.y - hd),
        (plate.x + hw, plate.y + hd),
        (plate.x - hw, plate.y + hd),
    ];
    corners
        .iter()
        .map(|(x, y)| {
            let (sx, sy) = iso(*x, *y, 0.0);
            format!("{sx:.1},{sy:.1}")
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Every building in the city, back to front.
#[component]
fn Buildings(skyline: StoredValue<Skyline>, under_plan: Memo<HashSet<String>>) -> impl IntoView {
    let lots = skyline.with_value(|s| s.lots.clone());
    let cathedral = skyline.with_value(|s| s.cathedral);

    view! {
        <g class="buildings">
            {lots.into_iter().enumerate().map(|(i, lot)| {
                let is_cathedral = cathedral == Some(i);
                view! { <BuildingGlyph lot=lot under_plan=under_plan cathedral=is_cathedral/> }
            }).collect_view()}
        </g>
    }
}

/// One building: three faces of an extruded isometric box.
#[component]
fn BuildingGlyph(lot: Lot, under_plan: Memo<HashSet<String>>, cathedral: bool) -> impl IntoView {
    let hw = lot.width / 2.0;
    let hd = lot.depth / 2.0;
    let h = lot.height;

    // Ground corners, then the same corners raised by the building's height.
    let (x0, y0) = (lot.x - hw, lot.y - hd);
    let (x1, y1) = (lot.x + hw, lot.y + hd);

    let top = [
        iso(x0, y0, h),
        iso(x1, y0, h),
        iso(x1, y1, h),
        iso(x0, y1, h),
    ];
    let left = [
        iso(x0, y1, h),
        iso(x1, y1, h),
        iso(x1, y1, 0.0),
        iso(x0, y1, 0.0),
    ];
    let right = [
        iso(x1, y0, h),
        iso(x1, y1, h),
        iso(x1, y1, 0.0),
        iso(x1, y0, 0.0),
    ];

    let tint = lot.ward.tint();
    let path = lot.path.clone();
    // A Memo is Copy, so it can be read in both the class and the Show below.
    let touched = Memo::new(move |_| under_plan.get().contains(&path));

    let title = match lot.kind {
        LotKind::Tower => format!("{} \u{2014} {}", lot.path, lot.ward.label()),
        LotKind::Commons => format!("{} \u{2014} aggregated", lot.name),
    };

    view! {
        <g
            class="building"
            class:commons={lot.kind == LotKind::Commons}
            class:cathedral=cathedral
            class:touched=move || touched.get()
        >
            <polygon class="face-left" points=points(&left) fill=shade(tint, LEFT_SHADE)/>
            <polygon class="face-right" points=points(&right) fill=shade(tint, RIGHT_SHADE)/>
            <polygon class="face-top" points=points(&top) fill=tint>
                <title>{title}</title>
            </polygon>
            // Gilded roof: this is a building a pending plan would alter.
            <Show when=move || touched.get()>
                <polygon class="roof-gilt" points=points(&top)/>
            </Show>
        </g>
    }
}

fn points(pts: &[(f64, f64)]) -> String {
    pts.iter()
        .map(|(x, y)| format!("{x:.1},{y:.1}"))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Darkens a hex colour toward the map's night palette.
///
/// Faces are shaded rather than given fixed colours so a building's ward stays
/// recognisable from any side.
fn shade(hex: &str, factor: f64) -> String {
    let parse = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).unwrap_or(0) as f64;
    if hex.len() != 7 {
        return hex.to_string();
    }
    let (r, g, b) = (parse(1), parse(3), parse(5));
    format!(
        "#{:02x}{:02x}{:02x}",
        (r * factor) as u8,
        (g * factor) as u8,
        (b * factor) as u8
    )
}

/// At distance a city is only a mass: its tallest buildings, merged.
#[component]
fn Silhouette(skyline: StoredValue<Skyline>) -> impl IntoView {
    // Only the notable towers, so the shape still reads at a glance.
    let lots = skyline.with_value(|s| {
        let mut lots: Vec<Lot> = s.lots.clone();
        lots.sort_by(|a, b| {
            b.height
                .partial_cmp(&a.height)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        lots.truncate(12);
        lots.sort_by(|a, b| {
            a.depth_key()
                .partial_cmp(&b.depth_key())
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        lots
    });

    view! {
        <g class="silhouette">
            {lots.into_iter().map(|lot| {
                let hw = lot.width / 2.0;
                let hd = lot.depth / 2.0;
                let top = [
                    iso(lot.x - hw, lot.y - hd, lot.height),
                    iso(lot.x + hw, lot.y - hd, lot.height),
                    iso(lot.x + hw, lot.y + hd, lot.height),
                    iso(lot.x - hw, lot.y + hd, lot.height),
                ];
                let base = [
                    iso(lot.x - hw, lot.y - hd, 0.0),
                    iso(lot.x + hw, lot.y - hd, 0.0),
                    iso(lot.x + hw, lot.y + hd, 0.0),
                    iso(lot.x - hw, lot.y + hd, 0.0),
                ];
                let hull = [top[0], top[1], top[2], base[2], base[3]];
                view! {
                    <polygon class="silhouette-block" points=points(&hull)/>
                }
            }).collect_view()}
        </g>
    }
}

/// The city's landmark: its single largest file, marked with a spire so the
/// answer to "where is the bulk of my code?" survives even at mid zoom.
#[component]
fn Landmark(skyline: StoredValue<Skyline>) -> impl IntoView {
    let lot = skyline.with_value(|s| s.cathedral.and_then(|i| s.lots.get(i).cloned()));

    lot.map(|lot| {
        let hw = lot.width / 2.0;
        let hd = lot.depth / 2.0;
        let top = [
            iso(lot.x - hw, lot.y - hd, lot.height),
            iso(lot.x + hw, lot.y - hd, lot.height),
            iso(lot.x + hw, lot.y + hd, lot.height),
            iso(lot.x - hw, lot.y + hd, lot.height),
        ];
        let (sx, sy) = iso(lot.x, lot.y, lot.height * 1.5);
        let (bx, by) = iso(lot.x, lot.y, lot.height);

        view! {
            <g class="landmark">
                <polygon class="landmark-top" points=points(&top) fill=lot.ward.tint()/>
                <line class="landmark-spire" x1=bx y1=by x2=sx y2=sy/>
                <circle class="landmark-star" cx=sx cy=sy r="2.4"/>
                <title>{format!("{} \u{2014} largest file", lot.path)}</title>
            </g>
        }
    })
}

/// A construction crane, shown while an architect is working here.
#[component]
fn Crane(radius: f64) -> impl IntoView {
    let h = radius * 0.85;
    let (bx, by) = iso(radius * 0.34, -radius * 0.34, 0.0);
    let (tx, ty) = iso(radius * 0.34, -radius * 0.34, h);
    let jib = radius * 0.42;

    view! {
        <g class="crane">
            <line class="crane-mast" x1=bx y1=by x2=tx y2=ty/>
            <line class="crane-jib" x1=tx - jib y1=ty + 4.0 x2=tx + jib * 0.5 y2=ty + 4.0/>
            <line class="crane-cable" x1=tx - jib * 0.72 y1=ty + 4.0 x2=tx - jib * 0.72 y2=ty + 18.0/>
            <rect class="crane-load" x=tx - jib * 0.72 - 3.0 y=ty + 18.0 width="6" height="6"/>
        </g>
    }
}

/// The city's heraldic banner, in its stack colour.
#[component]
fn Banner(radius: f64, color: &'static str) -> impl IntoView {
    let (bx, by) = iso(-radius * 0.4, radius * 0.4, 0.0);
    let top = by - radius * 0.95;

    view! {
        <g class="city-banner">
            <line class="banner-pole" x1=bx y1=by x2=bx y2=top/>
            <polygon
                class="banner-flag"
                points=format!(
                    "{bx:.1},{top:.1} {:.1},{:.1} {bx:.1},{:.1}",
                    bx + radius * 0.34, top + radius * 0.1, top + radius * 0.2,
                )
                fill=color
            />
        </g>
    }
}

/// Status pips for the architects stationed here.
#[component]
fn ArchitectPips(architects: Memo<Vec<kingdom_core::Architect>>, radius: f64) -> impl IntoView {
    view! {
        <g class="architect-ring">
            <For
                each={move || architects.get().into_iter().enumerate().collect::<Vec<_>>()}
                key=|(_, a)| a.id.clone()
                let:entry
            >
                {
                    let (i, architect) = entry;
                    let total = architects.get().len().max(1);
                    let spread = 2.094_395_1_f64;
                    let start = -std::f64::consts::FRAC_PI_2 - spread / 2.0;
                    let step = if total > 1 { spread / (total - 1) as f64 } else { 0.0 };
                    let angle = start + step * i as f64;
                    let ring = radius * 0.78;

                    view! {
                        <circle
                            class=format!("pip status-{}", architect.status.css_suffix())
                            cx=ring * angle.cos()
                            cy=ring * angle.sin() * 0.6 - radius * 0.28
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
    }
}

/// Fallback for a city with no scanned structure.
#[component]
fn BareKeep(radius: f64) -> impl IntoView {
    view! {
        <g class="keep" transform=format!("scale({})", radius / 48.0)>
            <rect x="-22" y="-6" width="44" height="30" rx="2" class="keep-body"/>
            <rect x="-30" y="-18" width="13" height="42" rx="2" class="keep-tower"/>
            <rect x="17" y="-18" width="13" height="42" rx="2" class="keep-tower"/>
            <polygon points="-30,-18 -23.5,-30 -17,-18" class="keep-roof"/>
            <polygon points="17,-18 23.5,-30 30,-18" class="keep-roof"/>
            <rect x="-5" y="8" width="10" height="16" rx="1" class="keep-gate"/>
        </g>
    }
}
