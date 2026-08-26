//! Making the folder tree legible on the ground.
//!
//! Wards already preserved the repository's folder hierarchy geometrically,
//! but nothing said so out loud: a nested ward was tinted from a hash of its
//! own path, so a child looked unrelated to its parent, and only top-level
//! folders were ever named. This module fixes both halves. Colour now descends
//! from the top-level ancestor so a subtree reads as one family, and every
//! ward that has room for it gets its name painted onto its own ground.
//!
//! Nothing here draws anything. It decides which ground a name may occupy and
//! how large it may be; the renderer owns glyphs, meshes, and when a label is
//! close enough to be worth showing.

use std::collections::HashMap;

use crate::map::{MapColor, MapGroundLabel};

use crate::build::layout::{Building, District, Rect};

/// The shortest cap height worth painting. Below this a name is a smudge, and
/// the ward is better served by the ward plaque the interface already shows.
const MIN_CAP_HEIGHT: f32 = 2.6;

/// The tallest cap height on a reference-sized island, so a sprawling
/// top-level ward does not end up with its name written across the whole
/// world.
const MAX_CAP_HEIGHT: f32 = 24.0;

/// A conservative per-character advance, in cap heights.
///
/// The renderer owns the real glyph metrics, so this only has to be close
/// enough to choose a sensible size; [`MapGroundLabel::max_width`] is what
/// actually guarantees the text fits.
const ADVANCE: f32 = 0.74;

/// How much of a margin band the text may fill across its thickness.
const BAND_FILL: f32 = 0.58;

/// Ground labels are cheap individually but a deep realm has thousands of
/// wards, and each one is a mesh. The deepest folders are also the ones whose
/// names are least useful from any distance, so the budget is spent on the
/// shallow ones first.
const MAX_LABELS: usize = 400;

/// The deepest folder that gets its name painted.
const MAX_LABELLED_DEPTH: usize = 4;

/// Resolves each ward to its top-level ancestor's seed.
///
/// A ward's colour comes from the ancestor rather than from itself, which is
/// what makes a whole subtree read as one holding rather than as a pile of
/// unrelated tints.
fn ancestor_seeds(districts: &[District]) -> HashMap<&str, u32> {
    let by_id: HashMap<&str, &District> = districts
        .iter()
        .map(|district| (district.id.as_str(), district))
        .collect();

    districts
        .iter()
        .map(|district| {
            let mut current = district;
            // A malformed parent chain would otherwise spin forever; the depth
            // is the bound the layout already guarantees.
            for _ in 0..=current.depth {
                let Some(parent) = current.parent.as_deref().and_then(|id| by_id.get(id)) else {
                    break;
                };
                current = parent;
            }
            (district.id.as_str(), current.seed)
        })
        .collect()
}

/// The ground and kerb colours for every ward, keyed by ward id.
pub fn ward_palettes(districts: &[District]) -> HashMap<String, (MapColor, MapColor)> {
    let ancestors = ancestor_seeds(districts);
    districts
        .iter()
        .map(|district| {
            let seed = ancestors
                .get(district.id.as_str())
                .copied()
                .unwrap_or(district.seed);
            (district.id.clone(), ward_palette(seed, district.depth))
        })
        .collect()
}

/// One ward's ground and kerb, from its family's seed and its own depth.
///
/// Depth lightens and warms the ground in even steps. Stacking wards this way
/// turns nesting into something the eye reads directly — a pale patch inside a
/// darker one is a folder inside a folder — where before every level was the
/// same weight and the hierarchy was invisible.
fn ward_palette(ancestor_seed: u32, depth: usize) -> (MapColor, MapColor) {
    let hue = (ancestor_seed % 23) as f32 / 23.0;
    let base = [66.0 + hue * 22.0, 84.0 + hue * 14.0, 50.0 + hue * 20.0];
    // Each level is a fixed step toward the same pale, sunlit tone, so the
    // number of steps between two shades is the number of folders between them.
    let step = (depth.min(5) as f32) * 0.15;
    let ground = [
        channel(base[0] + (188.0 - base[0]) * step),
        channel(base[1] + (192.0 - base[1]) * step),
        channel(base[2] + (150.0 - base[2]) * step),
        255,
    ];
    // The kerb is the ward's own ground in shadow, so it reads as the edge of
    // that ward rather than as a separate thing lying on top of it. It is
    // darkened by a fixed proportion at every depth, which keeps the border
    // visible whether the ground it surrounds is the darkest or the palest.
    let edge = [
        channel(ground[0] as f32 * 0.52 + 10.0),
        channel(ground[1] as f32 * 0.52 + 10.0),
        channel(ground[2] as f32 * 0.52 + 10.0),
        255,
    ];
    (ground, edge)
}

fn channel(value: f32) -> u8 {
    value.clamp(0.0, 255.0) as u8
}

/// A band of open ground inside a ward that a name could be painted on.
#[derive(Clone, Copy, Debug)]
struct Band {
    rect: Rect,
    vertical: bool,
}

/// Places every ward's name on its own ground.
pub fn ground_labels(districts: &[District], buildings: &[Building]) -> Vec<MapGroundLabel> {
    let palettes = ward_palettes(districts);
    let mut occupants: HashMap<&str, Vec<Rect>> = HashMap::new();
    for district in districts {
        if let Some(parent) = district.parent.as_deref() {
            occupants.entry(parent).or_default().push(district.rect);
        }
    }
    for building in buildings {
        if let Some(ward) = building.ward_id.as_deref() {
            occupants.entry(ward).or_default().push(building.lot);
        }
    }

    // Shallow folders first, and larger ones ahead of their siblings, so the
    // budget buys the names that orient someone rather than the ones that only
    // matter once they have already arrived.
    let mut ordered: Vec<&District> = districts
        .iter()
        .filter(|district| district.depth <= MAX_LABELLED_DEPTH)
        .collect();
    ordered.sort_by(|left, right| {
        left.depth
            .cmp(&right.depth)
            .then_with(|| right.files.cmp(&left.files))
            .then_with(|| left.id.cmp(&right.id))
    });

    ordered
        .into_iter()
        .filter_map(|district| {
            let color = palettes
                .get(district.id.as_str())
                .map(|(_, edge)| label_ink(*edge))
                .unwrap_or([236, 228, 204, 255]);
            ground_label(
                district,
                occupants.get(district.id.as_str()).map(Vec::as_slice),
                color,
            )
        })
        .take(MAX_LABELS)
        .collect()
}

/// A name painted onto a ward, or nothing when the ward has no room for one.
fn ground_label(
    district: &District,
    occupants: Option<&[Rect]>,
    color: MapColor,
) -> Option<MapGroundLabel> {
    let band = choose_band(district.rect, occupants)?;
    let text = shout(&district.name);
    if text.is_empty() {
        return None;
    }

    let thickness = if band.vertical {
        band.rect.width
    } else {
        band.rect.height
    };
    let along = if band.vertical {
        band.rect.height
    } else {
        band.rect.width
    };
    let shortest = district.rect.width.min(district.rect.height);
    let size = (thickness * BAND_FILL)
        .min(shortest * 0.22)
        .min(MAX_CAP_HEIGHT);
    if size < MIN_CAP_HEIGHT {
        return None;
    }

    // The band's length is the budget; a name too long for it is shortened
    // rather than allowed to run out of its own ward.
    let budget = along * 0.94;
    let text = fit(&text, size, budget);
    if text.is_empty() {
        return None;
    }

    // The width actually reserved for the name. The viewer condenses the text
    // into exactly this, so it is both the footprint the label is centred on
    // and the promise that it cannot spill past the band.
    let max_width = (text.chars().count() as f32 * ADVANCE * size).min(budget);
    let origin = if band.vertical {
        [
            band.rect.x + (band.rect.width - size) * 0.5,
            band.rect.y + (along - max_width) * 0.5,
        ]
    } else {
        [
            band.rect.x + (along - max_width) * 0.5,
            band.rect.y + (band.rect.height + size) * 0.5,
        ]
    };

    Some(MapGroundLabel {
        ward_id: district.id.clone(),
        text,
        origin,
        size,
        max_width,
        // Thin enough to stay legible at a glance, heavy enough to survive the
        // camera's shallow angle.
        stroke: (size * 0.15).clamp(0.35, 2.6),
        vertical: band.vertical,
        color,
        depth: district.depth as u32,
        // A cap shorter than this on screen is noise, so the renderer holds the
        // name back until the camera is close enough for it to be read. A
        // top-level ward is readable at roughly the fitted view, and deeper
        // folders ask for a little more, which staggers the levels as you zoom
        // instead of revealing them all at once.
        min_pixel_height: 6.0 + district.depth as f32 * 1.5,
    })
}

/// The clearest strip of ground inside a ward.
///
/// A ward's children partition its whole rect, so the open ground is the
/// margin the layout's road insets leave around them. Horizontal bands are
/// preferred because the camera's fixed isometric angle foreshortens the other
/// axis much harder, and text laid across it is the first thing to become
/// unreadable.
fn choose_band(rect: Rect, occupants: Option<&[Rect]>) -> Option<Band> {
    let Some(occupants) = occupants.filter(|rects| !rects.is_empty()) else {
        // Nothing stands here, so the ward is its own band.
        return Some(Band {
            rect,
            vertical: rect.height > rect.width * 1.6,
        });
    };

    let mut min = [f32::MAX; 2];
    let mut max = [f32::MIN; 2];
    for occupant in occupants {
        min[0] = min[0].min(occupant.x);
        min[1] = min[1].min(occupant.y);
        max[0] = max[0].max(occupant.x + occupant.width);
        max[1] = max[1].max(occupant.y + occupant.height);
    }

    let candidates = [
        Band {
            rect: Rect {
                x: rect.x,
                y: rect.y,
                width: rect.width,
                height: (min[1] - rect.y).max(0.0),
            },
            vertical: false,
        },
        Band {
            rect: Rect {
                x: rect.x,
                y: max[1],
                width: rect.width,
                height: (rect.y + rect.height - max[1]).max(0.0),
            },
            vertical: false,
        },
        Band {
            rect: Rect {
                x: rect.x,
                y: rect.y,
                width: (min[0] - rect.x).max(0.0),
                height: rect.height,
            },
            vertical: true,
        },
        Band {
            rect: Rect {
                x: max[0],
                y: rect.y,
                width: (rect.x + rect.width - max[0]).max(0.0),
                height: rect.height,
            },
            vertical: true,
        },
    ];

    candidates
        .into_iter()
        .filter(|band| band.rect.width > 0.0 && band.rect.height > 0.0)
        .max_by(|left, right| {
            band_score(*left)
                .partial_cmp(&band_score(*right))
                .unwrap_or(std::cmp::Ordering::Equal)
        })
}

/// How good a band is to write in: thickness sets the cap height, so it counts
/// for most, and the horizontal preference is a flat bonus.
fn band_score(band: Band) -> f32 {
    let thickness = if band.vertical {
        band.rect.width
    } else {
        band.rect.height
    };
    let along = if band.vertical {
        band.rect.height
    } else {
        band.rect.width
    };
    thickness * along.sqrt() * if band.vertical { 0.72 } else { 1.0 }
}

/// Folder names are painted in capitals, matching every other plaque on the map.
fn shout(name: &str) -> String {
    name.trim().to_uppercase()
}

/// Shortens a name that cannot fit the ground it has been given.
fn fit(text: &str, size: f32, max_width: f32) -> String {
    let per_character = ADVANCE * size;
    if per_character <= 0.0 {
        return String::new();
    }
    // The renderer condenses text into `max_width`, so a little overrun is
    // absorbed rather than clipped. Past that point condensing turns the name
    // into a picket fence and truncating reads better.
    let room = ((max_width * 1.25) / per_character).floor() as usize;
    if room == 0 {
        return String::new();
    }
    let length = text.chars().count();
    if length <= room {
        return text.to_owned();
    }
    if room == 1 {
        return "…".to_owned();
    }
    let mut shortened: String = text.chars().take(room - 1).collect();
    shortened.push('…');
    shortened
}

/// Paints the settlement's name across its square.
///
/// The square is the one piece of the map that stands for nothing in the
/// repository: it is where the roads meet, and how busy a road is means "how
/// many files travel it to reach here". Unnamed it reads as an unexplained
/// slab of paving — the first thing people ask about. Named, it reads as the
/// town centre, and in a realm each town's square says which town it is.
///
/// Returns nothing when the name cannot be painted large enough to read,
/// rather than laying down a smear.
pub fn square_label(square: Rect, name: &str, ink: MapColor) -> Option<MapGroundLabel> {
    let text = shout(name);
    if text.is_empty() {
        return None;
    }

    // The paving is a fixed size but names are not, so it is the lettering
    // that gives way: a short name is painted large, a long one smaller, and
    // only a name that will not fit even at the smallest readable size is
    // shortened. Fixing the cap height instead cut `repo-city-visualizer`
    // down to `REPO…`, which names nothing.
    let budget = square.width * 0.88;
    let size = (square.height * 0.26)
        .min(budget / (text.chars().count() as f32 * ADVANCE))
        .min(MAX_CAP_HEIGHT);
    if size < MIN_CAP_HEIGHT {
        return None;
    }
    let text = fit(&text, size, budget);
    if text.is_empty() {
        return None;
    }

    let max_width = (text.chars().count() as f32 * ADVANCE * size).min(budget);
    Some(MapGroundLabel {
        // The square belongs to no ward. Nothing reads this for a label, but
        // it has to say something, and naming it after the square it marks
        // keeps it from colliding with a real ward's id.
        ward_id: "square".to_owned(),
        text,
        origin: [
            square.x + (square.width - max_width) * 0.5,
            square.y + (square.height + size) * 0.5,
        ],
        size,
        max_width,
        stroke: (size * 0.15).clamp(0.35, 2.6),
        vertical: false,
        color: ink,
        depth: 0,
        // The square is the centre of the settlement, so its name is one of
        // the first that should appear rather than one of the last.
        min_pixel_height: 5.0,
    })
}

/// The ink a name is painted in: its own ward's kerb, lightened until it reads
/// against the ground it sits on.
fn label_ink(edge: MapColor) -> MapColor {
    [
        channel(edge[0] as f32 * 0.35 + 168.0),
        channel(edge[1] as f32 * 0.35 + 166.0),
        channel(edge[2] as f32 * 0.35 + 140.0),
        255,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::build::layout::{CityLayout, stable_hash};
    use crate::build::model::{Category, Metrics, Node, NodeKind};
    use std::path::PathBuf;

    fn district(id: &str, name: &str, parent: Option<&str>, depth: usize, rect: Rect) -> District {
        District {
            id: id.to_owned(),
            name: name.to_owned(),
            path: name.to_owned(),
            rect,
            depth,
            files: 4,
            arrivals: 7,
            parent: parent.map(str::to_owned),
            seed: stable_hash(name),
        }
    }

    fn rect(x: f32, y: f32, width: f32, height: f32) -> Rect {
        Rect {
            x,
            y,
            width,
            height,
        }
    }

    fn file(name: &str, path: &str) -> Node {
        Node {
            name: name.to_owned(),
            relative_path: PathBuf::from(path),
            kind: NodeKind::File {
                category: Category::Source,
            },
            metrics: Metrics {
                bytes: 2_048,
                lines: 90,
                code_lines: 80,
                complexity: 6,
                references: 2,
                file_count: 1,
            },
            children: Vec::new(),
        }
    }

    fn directory(name: &str, path: &str, children: Vec<Node>) -> Node {
        let mut node = Node::directory(name.to_owned(), PathBuf::from(path));
        for child in &children {
            node.metrics.add(child.metrics);
        }
        node.children = children;
        node
    }

    /// A repository with three levels of nesting, which is what the ground
    /// labels have to survive.
    fn nested_repository() -> Node {
        directory(
            "demo",
            "",
            vec![
                directory(
                    "src",
                    "src",
                    vec![
                        file("main.rs", "src/main.rs"),
                        file("lib.rs", "src/lib.rs"),
                        directory(
                            "engine",
                            "src/engine",
                            vec![
                                file("mod.rs", "src/engine/mod.rs"),
                                file("camera.rs", "src/engine/camera.rs"),
                                directory(
                                    "meshes",
                                    "src/engine/meshes",
                                    vec![
                                        file("build.rs", "src/engine/meshes/build.rs"),
                                        file("share.rs", "src/engine/meshes/share.rs"),
                                    ],
                                ),
                            ],
                        ),
                    ],
                ),
                directory(
                    "docs",
                    "docs",
                    vec![
                        file("guide.md", "docs/guide.md"),
                        file("api.md", "docs/api.md"),
                    ],
                ),
            ],
        )
    }

    #[test]
    fn a_subtree_shares_one_colour_family() {
        let districts = vec![
            district("ward-0", "src", None, 0, rect(0.0, 0.0, 200.0, 200.0)),
            district(
                "ward-1",
                "engine",
                Some("ward-0"),
                1,
                rect(10.0, 10.0, 90.0, 90.0),
            ),
            district("ward-2", "docs", None, 0, rect(400.0, 0.0, 200.0, 200.0)),
        ];
        let palettes = ward_palettes(&districts);

        let parent = palettes["ward-0"].0;
        let child = palettes["ward-1"].0;
        let stranger = palettes["ward-2"].0;

        // The child is the parent walked toward the light, so every channel
        // moves the same way and none of them moves far.
        for channel in 0..3 {
            assert!(
                child[channel] >= parent[channel],
                "channel {channel} darkened instead of lightening"
            );
        }
        assert_ne!(
            (parent[0], parent[1], parent[2]),
            (stranger[0], stranger[1], stranger[2]),
            "unrelated wards should not share a tint"
        );
    }

    #[test]
    fn a_kerb_is_always_darker_than_the_ground_it_borders() {
        // The border is what the viewer lights up when a folder is pointed at,
        // so it has to be visible as a line at every depth — including the
        // palest, most deeply nested wards.
        let seed = stable_hash("viewer");
        for depth in 0..6 {
            let (ground, edge) = ward_palette(seed, depth);
            for channel in 0..3 {
                assert!(
                    edge[channel] < ground[channel],
                    "depth {depth} channel {channel}: kerb {edge:?} did not read against {ground:?}"
                );
            }
            assert_eq!(edge[3], 255, "a kerb is opaque at every depth");
        }
    }

    #[test]
    fn nesting_lightens_the_ground_at_every_step() {
        let seed = stable_hash("src");
        let shades: Vec<u16> = (0..5)
            .map(|depth| ward_palette(seed, depth).0[1] as u16)
            .collect();
        for pair in shades.windows(2) {
            assert!(
                pair[1] > pair[0],
                "depth should keep lightening, got {shades:?}"
            );
        }
    }

    #[test]
    fn every_label_stays_inside_its_own_ward() {
        let layout = CityLayout::build(&nested_repository());
        let labels = ground_labels(&layout.districts, &layout.buildings);
        assert!(!labels.is_empty(), "a nested repository should be labelled");

        for label in &labels {
            let ward = layout
                .districts
                .iter()
                .find(|district| district.id == label.ward_id)
                .expect("every label belongs to a ward");
            // The viewer draws a horizontal name from the origin along +x with
            // its caps rising toward -y, and a vertical one a quarter turn from
            // that, so the ground it covers is the origin plus the reserved
            // width along one axis and the cap height along the other.
            let (min, max) = if label.vertical {
                (
                    [label.origin[0], label.origin[1]],
                    [
                        label.origin[0] + label.size,
                        label.origin[1] + label.max_width,
                    ],
                )
            } else {
                (
                    [label.origin[0], label.origin[1] - label.size],
                    [label.origin[0] + label.max_width, label.origin[1]],
                )
            };
            assert!(
                min[0] >= ward.rect.x - 0.5
                    && min[1] >= ward.rect.y - 0.5
                    && max[0] <= ward.rect.x + ward.rect.width + 0.5
                    && max[1] <= ward.rect.y + ward.rect.height + 0.5,
                "{} escaped {} at {min:?}..{max:?}",
                label.text,
                ward.path,
            );
        }
    }

    #[test]
    fn nested_folders_are_labelled_too() {
        let layout = CityLayout::build(&nested_repository());
        let labels = ground_labels(&layout.districts, &layout.buildings);
        let depths: Vec<u32> = labels.iter().map(|label| label.depth).collect();

        assert!(depths.contains(&0), "top-level folders must be named");
        assert!(
            depths.iter().any(|depth| *depth > 0),
            "nested folders must be named too, got {depths:?}"
        );
    }

    #[test]
    fn a_deeper_folder_waits_longer_before_it_is_shown() {
        let shallow = district("ward-0", "src", None, 0, rect(0.0, 0.0, 300.0, 300.0));
        let deep = district(
            "ward-1",
            "engine",
            Some("ward-0"),
            2,
            rect(0.0, 0.0, 300.0, 300.0),
        );
        let shallow = ground_label(&shallow, None, [255; 4]).expect("labelled");
        let deep = ground_label(&deep, None, [255; 4]).expect("labelled");
        assert!(deep.min_pixel_height > shallow.min_pixel_height);
    }

    #[test]
    fn a_ward_too_small_to_read_is_not_labelled() {
        let cramped = district("ward-0", "src", None, 0, rect(0.0, 0.0, 6.0, 5.0));
        assert!(ground_label(&cramped, None, [255; 4]).is_none());
    }

    #[test]
    fn a_long_name_is_shortened_rather_than_run_out_of_its_ward() {
        let ward = district(
            "ward-0",
            "a-very-long-folder-name-indeed",
            None,
            0,
            rect(0.0, 0.0, 40.0, 40.0),
        );
        let label = ground_label(&ward, None, [255; 4]).expect("labelled");
        assert!(label.text.ends_with('…'), "got {}", label.text);
        assert!(label.text.chars().count() < "A-VERY-LONG-FOLDER-NAME-INDEED".len());
    }

    #[test]
    fn names_are_painted_in_capitals() {
        let ward = district("ward-0", "src", None, 0, rect(0.0, 0.0, 300.0, 300.0));
        assert_eq!(ground_label(&ward, None, [255; 4]).unwrap().text, "SRC");
    }

    /// The square is the one thing on the map that stands for no file or
    /// folder, so it has to say what it is.
    #[test]
    fn the_square_is_named_and_the_name_stays_on_the_paving() {
        let square = rect(100.0, 200.0, 52.0, 52.0);
        let label = square_label(square, "repo-city", [54, 40, 24, 255])
            .expect("a 52-unit square has room for a name");

        assert!(!label.text.trim().is_empty(), "the square was left unnamed");
        assert!(
            label.origin[0] >= square.x && label.origin[0] <= square.x + square.width,
            "the name starts off the paving at {:?}",
            label.origin
        );
        assert!(
            label.origin[0] + label.max_width <= square.x + square.width + 0.01,
            "the name runs {} units past the right kerb",
            label.origin[0] + label.max_width - (square.x + square.width)
        );
        // The baseline sits below the origin's y by the cap height, so the
        // text body has to clear the top kerb as well as the bottom one.
        assert!(
            label.origin[1] - label.size >= square.y - 0.01
                && label.origin[1] <= square.y + square.height + 0.01,
            "the name at {:?} with cap {} does not sit on the paving",
            label.origin,
            label.size
        );
    }

    /// A name too long for the paving is painted smaller rather than cut, so
    /// that it still names the settlement.
    ///
    /// Fixing the cap height and shortening the text instead turned this
    /// repository's own square into `REPO…`, which says nothing at all.
    #[test]
    fn a_long_settlement_name_is_shrunk_onto_its_square_not_cut() {
        let square = rect(0.0, 0.0, 52.0, 52.0);
        let label = square_label(square, "repo-city-visualizer", [54, 40, 24, 255])
            .expect("a twenty character name still fits at a smaller size");
        assert_eq!(
            label.text, "REPO-CITY-VISUALIZER",
            "the name was cut instead of being painted smaller"
        );
        assert!(
            label.max_width <= square.width,
            "the name is {} wide on a {} square",
            label.max_width,
            square.width
        );

        // A short name still gets the full-size lettering.
        let short = square_label(square, "cli", [54, 40, 24, 255]).expect("a short name fits");
        assert!(
            short.size > label.size,
            "a three letter name ({}) should be painted larger than a twenty \
             letter one ({})",
            short.size,
            label.size
        );
    }

    #[test]
    fn labels_are_stable_across_builds() {
        let repository = nested_repository();
        let city = CityLayout::build(&repository);
        let first = ground_labels(&city.districts, &city.buildings);
        let second = ground_labels(&city.districts, &city.buildings);
        assert_eq!(first, second);
    }

    #[test]
    fn a_label_prefers_the_open_margin_around_its_children() {
        // The children leave a clear strip along the top of the ward, which is
        // exactly where the name belongs.
        let ward = rect(0.0, 0.0, 200.0, 200.0);
        let children = [rect(10.0, 40.0, 180.0, 150.0)];
        let band = choose_band(ward, Some(&children)).expect("a band");
        assert!(!band.vertical);
        assert!(band.rect.height <= 40.0);
        assert!(band.rect.y < 40.0);
    }
}
