//! Map labels.
//!
//! Labels are the one thing that genuinely wants to be flat: a ward name
//! painted onto the ground would skew with the camera and be unreadable. So
//! they live in the interface layer, positioned by projecting a world anchor
//! into the viewport each frame.
//!
//! The placement rules — four candidate anchors, keep clear of the viewport
//! edge, reject overlaps, cap the count — are the ones the old renderer used.

use bevy::camera::Camera;
use bevy::prelude::*;

use super::activity::Activity;
use super::bridge::LodLevel;
use super::camera::MapCamera;
use super::lod::ActiveLod;
use super::spawn::{Holding, LoadedMap};

/// Labels beyond this many are dropped, newest first. A dense ward can put
/// hundreds of names on screen, and past this point they are unreadable
/// anyway.
const MAX_LABELS: usize = 100;

/// The gap two labels must leave between them.
const LABEL_GAP: f32 = 5.0;

/// How far a label must stay inside the viewport.
const EDGE_MARGIN: f32 = 4.0;

const FILE_FONT_SIZE: f32 = 11.0;
const DISTRICT_FONT_SIZE: f32 = 13.0;
const DETAIL_FONT_SIZE: f32 = 9.0;

/// Marks the container every label lives under.
#[derive(Component)]
pub struct LabelLayer;

/// One label slot. Slots are reused frame to frame rather than respawned.
#[derive(Component)]
pub struct LabelSlot;

/// The bold first line of a label slot.
#[derive(Component)]
pub struct LabelTitle;

/// The smaller second line of a label slot.
#[derive(Component)]
pub struct LabelDetail;

/// The reusable label slots, grown on demand and never shrunk.
#[derive(Resource, Default)]
pub struct LabelPool {
    slots: Vec<Entity>,
}

/// A label asking to be placed.
#[derive(Clone, Debug)]
pub struct LabelRequest {
    /// The bold first line.
    pub title: String,
    /// The smaller second line.
    pub detail: String,
    /// The projected screen point the label hangs off.
    pub anchor: Vec2,
    /// Title size in pixels.
    pub font_size: f32,
    /// Detail size in pixels.
    pub detail_size: f32,
    /// Higher wins when two labels collide.
    pub weight: f32,
}

/// A label that survived placement.
#[derive(Clone, Debug)]
pub struct PlacedLabel {
    /// What was asked for.
    pub request: LabelRequest,
    /// Top-left corner in viewport pixels.
    pub position: Vec2,
    /// Measured width and height in viewport pixels.
    pub size: Vec2,
}

/// Chooses where labels go, dropping any that cannot be placed cleanly.
///
/// Pure so the rules can be tested without a camera or a window.
pub fn place_labels(mut requests: Vec<LabelRequest>, viewport: Vec2) -> Vec<PlacedLabel> {
    // Strongest first, so the important names claim their spot and the filler
    // is what gets dropped.
    requests.sort_by(|left, right| {
        right
            .weight
            .partial_cmp(&left.weight)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    let mut placed: Vec<PlacedLabel> = Vec::new();
    for request in requests {
        if placed.len() >= MAX_LABELS {
            break;
        }
        let size = measure(&request);
        let half = size * 0.5;

        // Above, below, right, then left — the same order the old renderer
        // tried, which keeps names off the roofs they belong to.
        let candidates = [
            Vec2::new(request.anchor.x - half.x, request.anchor.y - size.y - 10.0),
            Vec2::new(request.anchor.x - half.x, request.anchor.y + 10.0),
            Vec2::new(request.anchor.x + 12.0, request.anchor.y - half.y),
            Vec2::new(request.anchor.x - size.x - 12.0, request.anchor.y - half.y),
        ];

        if let Some(position) = candidates.into_iter().find(|position| {
            inside(*position, size, viewport)
                && !placed
                    .iter()
                    .any(|other| overlaps(*position, size, other.position, other.size))
        }) {
            placed.push(PlacedLabel {
                request,
                position,
                size,
            });
        }
    }
    placed
}

/// Estimates a label's box.
///
/// Exact glyph metrics would need the font atlas, and the only thing this
/// feeds is overlap rejection, where a slight overestimate is the safe error.
fn measure(request: &LabelRequest) -> Vec2 {
    const ADVANCE: f32 = 0.58;
    const PADDING: Vec2 = Vec2::new(12.0, 8.0);

    let title = request.title.chars().count() as f32 * request.font_size * ADVANCE;
    let detail = request.detail.chars().count() as f32 * request.detail_size * ADVANCE;
    let width = title.max(detail);
    let mut height = request.font_size * 1.25;
    if !request.detail.is_empty() {
        height += request.detail_size * 1.25;
    }
    Vec2::new(width, height) + PADDING
}

fn inside(position: Vec2, size: Vec2, viewport: Vec2) -> bool {
    position.x >= EDGE_MARGIN
        && position.y >= EDGE_MARGIN
        && position.x + size.x <= viewport.x - EDGE_MARGIN
        && position.y + size.y <= viewport.y - EDGE_MARGIN
}

fn overlaps(position: Vec2, size: Vec2, other: Vec2, other_size: Vec2) -> bool {
    position.x < other.x + other_size.x + LABEL_GAP
        && position.x + size.x + LABEL_GAP > other.x
        && position.y < other.y + other_size.y + LABEL_GAP
        && position.y + size.y + LABEL_GAP > other.y
}

/// Creates the container and a fixed pool of reusable slots.
pub fn spawn_label_pool(mut commands: Commands, mut pool: ResMut<LabelPool>) {
    let layer = commands
        .spawn((
            LabelLayer,
            Node {
                position_type: PositionType::Absolute,
                width: Val::Percent(100.0),
                height: Val::Percent(100.0),
                ..default()
            },
            // The map beneath must stay draggable through the label layer.
            Pickable::IGNORE,
        ))
        .id();

    pool.slots = (0..MAX_LABELS)
        .map(|_| {
            let slot = commands
                .spawn((
                    ChildOf(layer),
                    LabelSlot,
                    Node {
                        position_type: PositionType::Absolute,
                        padding: UiRect::axes(Val::Px(6.0), Val::Px(4.0)),
                        flex_direction: FlexDirection::Column,
                        border_radius: BorderRadius::all(Val::Px(3.0)),
                        ..default()
                    },
                    BackgroundColor(Color::srgba(0.05, 0.06, 0.09, 0.72)),
                    Visibility::Hidden,
                    Pickable::IGNORE,
                ))
                .id();
            commands.spawn((
                ChildOf(slot),
                LabelTitle,
                Text::new(""),
                TextFont::from_font_size(FILE_FONT_SIZE),
                TextColor(Color::srgb(0.93, 0.92, 0.87)),
                Pickable::IGNORE,
            ));
            commands.spawn((
                ChildOf(slot),
                LabelDetail,
                Text::new(""),
                TextFont::from_font_size(DETAIL_FONT_SIZE),
                TextColor(Color::srgb(0.68, 0.70, 0.66)),
                Pickable::IGNORE,
            ));
            slot
        })
        .collect();
}

/// A plaque's second line, with what is working there added to it.
///
/// Appends to the existing detail rather than claiming a third line: a label
/// slot holds exactly a [`LabelTitle`] and a [`LabelDetail`], and growing the
/// pool is a larger change than one clause earns.
///
/// **ASCII only, deliberately.** Bevy's bundled default font is a *subset* of
/// FiraMono carrying 95 codepoints, so anything outside ASCII renders as a
/// tofu box. A middot and a bullet were both tried on screen and both came out
/// as squares. (The manifest's own `"N files \u{b7} N lines"` has the same bug
/// and predates this; it is left alone here rather than fixed in passing.)
///
/// Pure, so the wording can be pinned without a camera or a window.
pub fn with_activity(detail: &str, working: usize) -> String {
    if working == 0 {
        return detail.to_owned();
    }
    // "1 working" rather than "1 plan working": the plaque is already crowded
    // and the map's own vocabulary for a piece of work is the town lighting up.
    let note = format!("[{working} working]");
    if detail.is_empty() {
        note
    } else {
        format!("{detail}  {note}")
    }
}

/// Projects anchors, places labels, and drives the pool.
#[allow(clippy::too_many_arguments)]
pub fn update_labels(
    active: Res<ActiveLod>,
    working: Res<Activity>,
    map: Res<LoadedMap>,
    camera: Query<(&Camera, &GlobalTransform), With<MapCamera>>,
    holdings: Query<&Holding>,
    pool: Res<LabelPool>,
    mut slots: Query<(&mut Node, &mut Visibility), With<LabelSlot>>,
    children: Query<&Children>,
    mut titles: Query<&mut Text, (With<LabelTitle>, Without<LabelDetail>)>,
    mut details: Query<&mut Text, (With<LabelDetail>, Without<LabelTitle>)>,
) {
    let Ok((camera_component, camera_transform)) = camera.single() else {
        return;
    };
    let Some(viewport) = camera_component.logical_viewport_size() else {
        return;
    };
    let Some(manifest) = map.0.as_ref() else {
        return;
    };

    let project = |point: Vec3| {
        camera_component
            .world_to_viewport(camera_transform, point)
            .ok()
    };

    let requests = match active.0 {
        LodLevel::Districts => manifest
            .districts
            .iter()
            .filter_map(|district| {
                let anchor = project(Vec3::new(district.center[0], 0.0, district.center[1]))?;
                Some(LabelRequest {
                    title: district.label.clone(),
                    // A district plaque at this tier stands for a whole town,
                    // and its `label` is that town's name -- which is what the
                    // activity is keyed on. In a single-settlement map the
                    // plaques are wards instead, and no ward shares a name with
                    // a town, so the lookup simply finds nothing.
                    detail: with_activity(&district.detail, working.working_in(&district.label)),
                    anchor,
                    font_size: DISTRICT_FONT_SIZE,
                    detail_size: DETAIL_FONT_SIZE,
                    // Bigger wards read as more important, so they win ties.
                    weight: 1_000.0,
                })
            })
            .collect::<Vec<_>>(),
        LodLevel::Architecture => Vec::new(),
        LodLevel::FileDetail => {
            let mut requests = Vec::new();
            for holding in holdings.iter() {
                let Some(anchor) = project(holding.label_anchor) else {
                    continue;
                };
                if anchor.x < 0.0
                    || anchor.y < 0.0
                    || anchor.x > viewport.x
                    || anchor.y > viewport.y
                {
                    continue;
                }
                let Some(feature) = manifest
                    .features
                    .iter()
                    .find(|feature| feature.id == holding.feature_id)
                else {
                    continue;
                };
                requests.push(LabelRequest {
                    title: feature.name.clone(),
                    // Just the name at this tier -- no "GUILDHALL · SOURCE"
                    // style subtitle under each building.
                    detail: String::new(),
                    anchor,
                    font_size: FILE_FONT_SIZE,
                    detail_size: DETAIL_FONT_SIZE,
                    // Taller holdings matter more, and stay put as the camera
                    // moves because the weight comes from the world, not the
                    // screen.
                    weight: feature.height,
                });
            }
            requests
        }
    };

    let placed = place_labels(requests, viewport);

    for (index, slot) in pool.slots.iter().enumerate() {
        let Ok((mut node, mut visibility)) = slots.get_mut(*slot) else {
            continue;
        };
        match placed.get(index) {
            Some(label) => {
                node.left = Val::Px(label.position.x);
                node.top = Val::Px(label.position.y);
                *visibility = Visibility::Inherited;
                if let Ok(slot_children) = children.get(*slot) {
                    for child in slot_children.iter() {
                        if let Ok(mut text) = titles.get_mut(child) {
                            if text.0 != label.request.title {
                                text.0 = label.request.title.clone();
                            }
                        } else if let Ok(mut text) = details.get_mut(child)
                            && text.0 != label.request.detail
                        {
                            text.0 = label.request.detail.clone();
                        }
                    }
                }
            }
            None => {
                if *visibility != Visibility::Hidden {
                    *visibility = Visibility::Hidden;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(title: &str, anchor: Vec2, weight: f32) -> LabelRequest {
        LabelRequest {
            title: title.to_owned(),
            detail: "COTTAGE".to_owned(),
            anchor,
            font_size: FILE_FONT_SIZE,
            detail_size: DETAIL_FONT_SIZE,
            weight,
        }
    }

    const VIEWPORT: Vec2 = Vec2::new(1280.0, 800.0);

    #[test]
    fn a_lone_label_sits_above_its_anchor() {
        let placed = place_labels(
            vec![request("main.rs", Vec2::new(640.0, 400.0), 1.0)],
            VIEWPORT,
        );
        assert_eq!(placed.len(), 1);
        assert!(
            placed[0].position.y + placed[0].size.y < 400.0,
            "the label should clear its anchor"
        );
    }

    #[test]
    fn labels_never_overlap() {
        let requests = (0..40)
            .map(|index| {
                let x = 200.0 + (index % 8) as f32 * 12.0;
                let y = 200.0 + (index / 8) as f32 * 14.0;
                request(&format!("file{index}.rs"), Vec2::new(x, y), index as f32)
            })
            .collect();

        let placed = place_labels(requests, VIEWPORT);
        for (index, label) in placed.iter().enumerate() {
            for other in placed.iter().skip(index + 1) {
                assert!(
                    !overlaps(label.position, label.size, other.position, other.size),
                    "{} overlapped {}",
                    label.request.title,
                    other.request.title
                );
            }
        }
    }

    #[test]
    fn labels_stay_inside_the_viewport() {
        let requests = vec![
            request("top-left.rs", Vec2::new(2.0, 2.0), 1.0),
            request("bottom-right.rs", Vec2::new(1278.0, 798.0), 1.0),
            request("middle.rs", Vec2::new(600.0, 400.0), 1.0),
        ];
        for label in place_labels(requests, VIEWPORT) {
            assert!(
                inside(label.position, label.size, VIEWPORT),
                "{} escaped the viewport at {:?}",
                label.request.title,
                label.position
            );
        }
    }

    #[test]
    fn the_label_count_is_capped() {
        let requests = (0..600)
            .map(|index| {
                let x = 20.0 + (index % 30) as f32 * 40.0;
                let y = 20.0 + (index / 30) as f32 * 36.0;
                request(&format!("f{index}.rs"), Vec2::new(x, y), index as f32)
            })
            .collect();
        assert!(place_labels(requests, VIEWPORT).len() <= MAX_LABELS);
    }

    #[test]
    fn the_heaviest_label_wins_a_contested_spot() {
        let requests = vec![
            request("minor.rs", Vec2::new(400.0, 300.0), 1.0),
            request("major.rs", Vec2::new(402.0, 301.0), 99.0),
        ];
        let placed = place_labels(requests, VIEWPORT);
        assert_eq!(placed[0].request.title, "major.rs");
    }

    /// A quiet town's plaque must read exactly as it always did: this runs on
    /// every district at the Districts tier, so a stray separator would appear
    /// across the whole map the moment nothing at all was running.
    #[test]
    fn a_quiet_town_says_nothing_new() {
        assert_eq!(with_activity("1,204 files", 0), "1,204 files");
        assert_eq!(with_activity("", 0), "");
    }

    #[test]
    fn a_working_town_says_how_much_is_running() {
        assert_eq!(with_activity("1,204 files", 2), "1,204 files  [2 working]");
        // Nothing to separate from, so no separator.
        assert_eq!(with_activity("", 1), "[1 working]");
    }

    /// Bevy's bundled font is a 95-codepoint subset of FiraMono, so a plaque
    /// written with a middot or a bullet renders a tofu box -- observed on
    /// screen before this test was written. Anything this module composes must
    /// stay inside ASCII.
    #[test]
    fn the_plaque_stays_within_the_font_it_is_drawn_with() {
        for count in [1, 2, 17] {
            let line = with_activity("1,204 files", count);
            assert!(line.is_ascii(), "the default font cannot draw {line:?}");
        }
    }
}
