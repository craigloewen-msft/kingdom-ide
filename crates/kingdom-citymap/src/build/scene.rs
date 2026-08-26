//! Builds the world-space scene the renderer turns into geometry.
//!
//! Nothing in this module projects, shades, or rasterises anything. It decides
//! *where things stand and what colour they are*; the rendering engine owns the
//! camera, depth sorting, lighting, and culling.


use crate::map::{
    MapBuilding, MapColor, MapPalette, MapPlaza, MapRect, MapSun, MapTown, MapUnderside, MapWard,
    MapWorld,
};

use crate::build::layout::{
    Building, CityLayout, District, Rect, TownLayout, WorldLayout, stable_hash,
};
use crate::build::model::Category;
use crate::build::scenery::{SceneryInput, scenery};
use crate::build::streets;
use crate::build::wayfinding::{ground_labels, square_label, ward_palettes};

const SPACE: MapColor = [7, 11, 20, 255];
const GROUND: MapColor = [73, 87, 52, 255];
/// The rock below the ground, drawn unlit -- see `spawn_terrain` for why
/// nothing under the disk can be lit. So these are not base colours a light
/// will shade: they are the final pixels, and the falloff from one to the next
/// is the whole of the shading the underside gets.
const CLIFF: MapColor = [124, 108, 86, 255];
const ROCK: MapColor = [92, 80, 68, 255];
const DEEP: MapColor = [66, 58, 52, 255];

/// How deep the world is, as multiples of the disk's radius.
///
/// At the isometric angle a ground radius `r` falls `0.577r` down the screen
/// and a depth `d` falls `0.816d`, so nothing below the ground shows past the
/// near rim until `d > 0.707r`. The cliff and the shelf sit inside that and
/// only widen the disk's edge; the spire is the part that hangs out where it
/// can be seen, and it is kept just past the threshold rather than well past
/// it -- the chamber's composer overlays the bottom of the canvas, so a longer
/// spire buys depth that is drawn underneath the prompt bar.
const CLIFF_DEPTH: f32 = 0.045;
const SHELF_DEPTH: f32 = 0.13;
/// How far the frustum has pulled in by the shelf. A narrow waist is what
/// makes the underside read as a world balanced on a point rather than as a
/// plate with a lampshade under it.
const SHELF_TAPER: f32 = 0.42;
const SPIRE_DEPTH: f32 = 0.92;

/// The size of the square at the heart of a settlement. Constant, like the
/// wards around it.
const PLAZA_SIZE: f32 = 52.0;

/// How many segments the rim is drawn with.
///
/// The rim is a real circle rather than an authored blob, so the only question
/// is how finely it is sampled. At 96 the straight edges are under four degrees
/// apart, which reads as a curve at every zoom the camera allows, and the whole
/// outline is still cheaper than the 16-point coastline it replaced once the
/// shallows ring beside it is counted.
const RIM_SEGMENTS: usize = 96;

/// How much wider than the built ground the disk is drawn.
///
/// The rim *circumscribes* what was actually built: a settlement is a rectangle
/// with towns packed into its corners, and any circle tighter than its
/// half-diagonal would drop a corner town over the edge. This is the open
/// country past that -- enough for woodland and a coast road, not so much that
/// the town is marooned in the middle of a bare green plate.
const RIM_MARGIN: f32 = 1.12;

/// Identifier linking a single repository's holding to its manifest feature.
pub fn city_feature_id(building_index: usize) -> String {
    format!("file-{building_index}")
}

/// Identifier linking a realm holding to its manifest feature.
pub fn realm_feature_id(repository_index: usize, building_index: usize) -> String {
    format!("{repository_index}-{building_index}")
}

/// Ink for the name painted on a settlement's square.
///
/// The paving is a light tan, so unlike a ward name — which is lightened to
/// read against grass — this one has to be dark to be read at all.
const SQUARE_INK: MapColor = [54, 40, 24, 255];

/// Builds the scene for a single repository.
///
/// `name` is the repository's, and is painted on the square at its centre.
pub fn build_city_world(layout: &CityLayout, name: &str) -> MapWorld {
    let (roads, plaza) = streets::settlement_roads(
        &layout.districts,
        &layout.buildings,
        &layout.corridors,
        layout.extent.center(),
        PLAZA_SIZE,
        // A settlement on its own has no highways arriving.
        &[],
        layout.ward_gap,
    );
    let plazas: Vec<MapPlaza> = plaza.into_iter().collect();
    let wards = wards(&layout.districts);
    let buildings: Vec<MapBuilding> = layout
        .buildings
        .iter()
        .enumerate()
        .map(|(index, building)| map_building(city_feature_id(index), building))
        .collect();
    let lots: Vec<MapRect> = buildings.iter().map(|building| building.lot).collect();
    // Round the wards rather than the holdings: a ward is the ground a folder
    // claims, so an empty corner of one is still part of the settlement and
    // should stand on the disk rather than off the edge of it.
    let claimed: Vec<MapRect> = wards.iter().map(|ward| ward.rect).collect();
    let disk = disk(&claimed, layout.extent);
    let rim = disk.outline();

    MapWorld {
        bounds: disk.bounds(),
        space: SPACE,
        ground: GROUND,
        underside: disk.underside(),
        sun: sun(),
        towns: Vec::new(),
        scenery: scenery(SceneryInput {
            rim: &rim,
            building_lots: &lots,
            roads: &roads,
            plazas: &plazas,
            wards: &wards,
            seed_key: &settlement_key(&layout.districts),
        }),
        ground_labels: {
            let mut labels = ground_labels(&layout.districts, &layout.buildings);
            labels.extend(
                plazas
                    .first()
                    .and_then(|plaza| square_label(plaza_ground(plaza), name, SQUARE_INK)),
            );
            labels
        },
        wards,
        plazas,
        roads,
        buildings,
        rim,
    }
}

/// The square's ground as a layout rect, for anything that has to measure it.
fn plaza_ground(plaza: &MapPlaza) -> Rect {
    Rect {
        x: plaza.rect.x,
        y: plaza.rect.y,
        width: plaza.rect.width,
        height: plaza.rect.depth,
    }
}

/// Builds the scene for a realm of several repositories.
pub fn build_realm_world(layout: &WorldLayout) -> MapWorld {
    let districts: Vec<District> = layout
        .towns
        .iter()
        .flat_map(|town| town.city.districts.iter().cloned())
        .collect();
    let all_buildings: Vec<Building> = layout
        .towns
        .iter()
        .flat_map(|town| town.city.buildings.iter().cloned())
        .collect();

    // A realm's disk is drawn round the towns standing on it, not round the
    // ground the packing claimed: `pack_towns` grows its island until every
    // town fits and then hugs them, so the claimed extent already carries a
    // fringe that circumscribing would square up into a bare plate.
    let disk = disk(
        &layout
            .towns
            .iter()
            .map(|town| map_rect(town.rect))
            .collect::<Vec<_>>(),
        layout.extent,
    );
    let rim = disk.outline();

    let towns_for_roads: Vec<streets::Town> = layout
        .towns
        .iter()
        .map(|town| streets::Town {
            rect: town.rect,
            // Journeys, not files: a highway into a town has to carry every
            // reference reaching into it as well as the town's own traffic.
            files: town
                .city
                .buildings
                .iter()
                .map(|building| 1 + building.references)
                .sum(),
        })
        .collect();
    let (mut roads, arrivals) = streets::highways(&towns_for_roads, layout.extent.center());
    let mut plazas = Vec::new();
    // Each town's square carries that town's name, which in a realm is the
    // repository's — so the square says which settlement you are standing in.
    let mut square_labels = Vec::new();
    for (index, town) in layout.towns.iter().enumerate() {
        let (town_roads, plaza) = streets::settlement_roads(
            &town.city.districts,
            &town.city.buildings,
            &town.city.corridors,
            town.center(),
            PLAZA_SIZE,
            &arrivals[index],
            town.city.ward_gap,
        );
        roads.extend(town_roads);
        if let Some(plaza) = plaza.as_ref() {
            square_labels.extend(square_label(plaza_ground(plaza), &town.name, SQUARE_INK));
        }
        plazas.extend(plaza);
    }

    let buildings: Vec<MapBuilding> = layout
        .towns
        .iter()
        .flat_map(|town| {
            town.city
                .buildings
                .iter()
                .enumerate()
                .map(move |(index, building)| {
                    map_building(realm_feature_id(town.repository_index, index), building)
                })
        })
        .collect();
    let lots: Vec<MapRect> = buildings.iter().map(|building| building.lot).collect();
    let wards = wards(&districts);

    MapWorld {
        bounds: disk.bounds(),
        space: SPACE,
        ground: GROUND,
        underside: disk.underside(),
        sun: sun(),
        towns: towns(&layout.towns),
        scenery: scenery(SceneryInput {
            rim: &rim,
            building_lots: &lots,
            roads: &roads,
            plazas: &plazas,
            wards: &wards,
            seed_key: &settlement_key(&districts),
        }),
        ground_labels: {
            let mut labels = ground_labels(&districts, &all_buildings);
            labels.extend(square_labels);
            labels
        },
        wards,
        plazas,
        roads,
        buildings,
        rim,
    }
}

/// A seed for everything that must vary between repositories but never between
/// two builds of the same one.
fn settlement_key(districts: &[District]) -> String {
    let mut key = String::from("repo-city");
    for district in districts.iter().filter(|district| district.depth == 0) {
        key.push('/');
        key.push_str(&district.path);
    }
    key
}

/// The single light every surface is shaded by.
///
/// Walls and roofs used to carry hand-darkened colours per face. The engine now
/// derives all of that from this direction, so a building only needs one wall
/// colour and one roof colour.
fn sun() -> MapSun {
    MapSun {
        direction: normalize([-0.348, -0.895, -0.278]),
        color: [255, 246, 224, 255],
        illuminance: 9_000.0,
        ambient: [176, 192, 214, 255],
        ambient_brightness: 420.0,
    }
}

/// The disk the world stands on: where its centre is, and how far the ground
/// reaches from there.
///
/// Every part of the world's shape is derived from these two numbers -- the
/// outline, the bounds the camera frames, and how deep the rock below it goes.
/// Deriving them once, here, is what keeps the rim the renderer draws and the
/// land the trees are scattered on the same circle.
#[derive(Clone, Copy, Debug)]
struct Disk {
    center: [f32; 2],
    radius: f32,
}

/// The disk that holds a piece of ground, with room to spare at the edges.
///
/// `built` is what actually stands on the world -- the lots, or the towns in a
/// realm -- and `extent` is the ground the layout claimed, which is used only
/// when nothing was built at all. Measuring the disk against the built area
/// rather than the extent matters: `layout::hug` already leaves a fringe of
/// open country round the outside, and circumscribing *that* squares the fringe
/// up into a plate half again as wide as the kingdom standing on it.
fn disk(built: &[MapRect], extent: Rect) -> Disk {
    let bounds = built_bounds(built).unwrap_or(extent);
    let (x, y) = bounds.center();
    let half_diagonal = (bounds.width * bounds.width + bounds.height * bounds.height).sqrt() * 0.5;
    Disk {
        center: [x, y],
        // A degenerate world would otherwise produce a disk with no area, and
        // every downstream measurement divides by the radius somewhere.
        radius: (half_diagonal * RIM_MARGIN).max(1.0),
    }
}

/// The ground everything built actually covers.
fn built_bounds(built: &[MapRect]) -> Option<Rect> {
    let first = built.first()?;
    let mut min = [first.x, first.y];
    let mut max = [first.max_x(), first.max_y()];
    for rect in built.iter().skip(1) {
        min[0] = min[0].min(rect.x);
        min[1] = min[1].min(rect.y);
        max[0] = max[0].max(rect.max_x());
        max[1] = max[1].max(rect.max_y());
    }
    Some(Rect {
        x: min[0],
        y: min[1],
        width: max[0] - min[0],
        height: max[1] - min[1],
    })
}

impl Disk {
    /// The rim, as a closed outline running counter-clockwise.
    fn outline(&self) -> Vec<[f32; 2]> {
        (0..RIM_SEGMENTS)
            .map(|index| {
                let angle = index as f32 / RIM_SEGMENTS as f32 * std::f32::consts::TAU;
                [
                    self.center[0] + angle.cos() * self.radius,
                    self.center[1] + angle.sin() * self.radius,
                ]
            })
            .collect()
    }

    /// The ground the world covers, which is what the camera frames.
    fn bounds(&self) -> MapRect {
        MapRect {
            x: self.center[0] - self.radius,
            y: self.center[1] - self.radius,
            width: self.radius * 2.0,
            depth: self.radius * 2.0,
        }
    }

    /// The rock below the ground, measured against this disk's own radius.
    ///
    /// Proportional rather than absolute for the reason the rim is: a kingdom's
    /// ground grows with the number of files in it, and a fixed spire would be
    /// a spike under a small world and a stub under a large one.
    fn underside(&self) -> MapUnderside {
        MapUnderside {
            cliff: self.radius * CLIFF_DEPTH,
            shelf: self.radius * SHELF_DEPTH,
            taper: SHELF_TAPER,
            depth: self.radius * SPIRE_DEPTH,
            cliff_color: CLIFF,
            rock: ROCK,
            deep: DEEP,
        }
    }
}

fn map_building(feature_id: String, building: &Building) -> MapBuilding {
    MapBuilding {
        feature_id,
        ward_id: building.ward_id.clone(),
        kind: building.kind,
        footprint: map_rect(building.footprint()),
        lot: map_rect(building.lot),
        height: building.height(),
        palette: palette(building.category),
        complexity: building.complexity.min(u32::MAX as usize) as u32,
        seed: stable_hash(&building.path),
    }
}

fn towns(towns: &[TownLayout]) -> Vec<MapTown> {
    towns
        .iter()
        .enumerate()
        .map(|(index, town)| {
            let variation = (stable_hash(&town.name) % 17) as u8;
            MapTown {
                id: format!("town-{index}"),
                name: town.name.clone(),
                rect: map_rect(town.rect),
                polygon: organic_points(town.rect, stable_hash(&town.name), 0),
                ground: [65 + variation / 2, 76 + variation, 49, 255],
                edge: [151, 132, 83, 255],
            }
        })
        .collect()
}

/// Every ward, at every depth, carrying the folder it stands for.
///
/// Shallow wards are emitted first so a renderer stacking them by depth never
/// has to sort, and so a hit test walking the list backwards finds the
/// innermost folder under a point first.
fn wards(districts: &[District]) -> Vec<MapWard> {
    let palettes = ward_palettes(districts);
    let mut sorted: Vec<&District> = districts.iter().collect();
    sorted.sort_by(|left, right| {
        left.depth
            .cmp(&right.depth)
            .then_with(|| left.id.cmp(&right.id))
    });
    sorted
        .into_iter()
        .map(|district| {
            let (ground, edge) = palettes
                .get(district.id.as_str())
                .copied()
                .unwrap_or(([73, 91, 56, 255], [99, 94, 66, 170]));
            MapWard {
                id: district.id.clone(),
                name: district.name.clone(),
                path: district.path.clone(),
                parent: district.parent.clone(),
                files: district.files.min(u32::MAX as usize) as u32,
                rect: map_rect(district.rect),
                polygon: organic_points(district.rect, district.seed, district.depth),
                depth: district.depth as u32,
                ground,
                edge,
            }
        })
        .collect()
}

/// Wall and roof colours carrying the file's role.
///
/// Only one wall colour is needed now: the difference between a lit and a
/// shadowed face comes from the sun, not from a hand-darkened copy.
fn palette(category: Category) -> MapPalette {
    let (roof, wall) = match category {
        Category::Source => ([91, 121, 91], [151, 134, 102]),
        Category::Web => ([89, 113, 139], [145, 126, 101]),
        Category::Test => ([151, 76, 59], [141, 122, 92]),
        Category::Docs => ([178, 137, 63], [158, 140, 105]),
        Category::Config => ([117, 87, 126], [135, 124, 109]),
        Category::Data => ([99, 128, 74], [146, 128, 89]),
        Category::Asset => ([143, 105, 93], [102, 120, 72]),
        Category::Script => ([165, 101, 50], [146, 125, 96]),
        Category::Other => ([111, 112, 104], [137, 129, 110]),
    };
    MapPalette {
        wall: [wall[0], wall[1], wall[2], 255],
        roof: [roof[0], roof[1], roof[2], 255],
        trim: [79, 66, 51, 255],
        window: [239, 190, 88, 255],
        ground: [
            wall[0].saturating_sub(20),
            wall[1].saturating_sub(17),
            wall[2].saturating_sub(10),
            255,
        ],
    }
}

/// Clips the corners off a rectangle so wards never read as a perfect grid.
fn organic_points(rect: Rect, seed: u32, depth: usize) -> Vec<[f32; 2]> {
    let hash = seed;
    let cut = rect.width.min(rect.height) * if depth == 0 { 0.11 } else { 0.05 };
    let offsets = [
        0.55 + (hash % 23) as f32 / 100.0,
        0.48 + ((hash / 23) % 27) as f32 / 100.0,
        0.52 + ((hash / 53) % 25) as f32 / 100.0,
        0.46 + ((hash / 97) % 29) as f32 / 100.0,
    ];
    vec![
        [rect.x + cut * offsets[0], rect.y],
        [rect.x + rect.width - cut * offsets[1], rect.y],
        [rect.x + rect.width, rect.y + cut * offsets[2]],
        [rect.x + rect.width, rect.y + rect.height - cut * offsets[3]],
        [rect.x + rect.width - cut * offsets[0], rect.y + rect.height],
        [rect.x + cut * offsets[1], rect.y + rect.height],
        [rect.x, rect.y + rect.height - cut * offsets[2]],
        [rect.x, rect.y + cut * offsets[3]],
    ]
}

pub(crate) fn map_rect(rect: Rect) -> MapRect {
    MapRect {
        x: rect.x,
        y: rect.y,
        width: rect.width,
        depth: rect.height,
    }
}

fn normalize(vector: [f32; 3]) -> [f32; 3] {
    let length = (vector[0] * vector[0] + vector[1] * vector[1] + vector[2] * vector[2]).sqrt();
    if length == 0.0 {
        return vector;
    }
    [vector[0] / length, vector[1] / length, vector[2] / length]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::build::layout::{Building, WARD_GAP, reference_extent};
    use crate::map::BuildingKind;

    fn building(name: &str) -> Building {
        Building {
            name: name.to_owned(),
            path: format!("src/{name}"),
            ward_id: Some("ward-0".to_owned()),
            category: Category::Source,
            kind: BuildingKind::Guildhall,
            lot: Rect {
                x: 120.0,
                y: 200.0,
                width: 24.0,
                height: 20.0,
            },
            bytes: 4_096,
            lines: 220,
            complexity: 9,
            references: 4,
            scale: 1.0,
        }
    }

    #[test]
    fn buildings_keep_their_world_footprint_and_height() {
        let source = building("lib.rs");
        let mapped = map_building("file-0".to_owned(), &source);
        let footprint = source.footprint();

        assert_eq!(mapped.footprint.x, footprint.x);
        assert_eq!(mapped.footprint.y, footprint.y);
        assert_eq!(mapped.footprint.width, footprint.width);
        assert_eq!(mapped.footprint.depth, footprint.height);
        assert_eq!(mapped.height, source.height());
        assert_eq!(mapped.kind, BuildingKind::Guildhall);
    }

    #[test]
    fn building_seeds_are_stable_across_builds() {
        let source = building("lib.rs");
        let first = map_building("file-0".to_owned(), &source);
        let second = map_building("file-0".to_owned(), &source);
        assert_eq!(first.seed, second.seed);
        assert_ne!(
            first.seed,
            map_building("file-1".to_owned(), &building("main.rs")).seed
        );
    }

    #[test]
    fn ward_polygons_stay_inside_their_rect() {
        let district = District {
            id: "ward-0".to_owned(),
            name: "src".to_owned(),
            path: "src".to_owned(),
            rect: Rect {
                x: 100.0,
                y: 150.0,
                width: 200.0,
                height: 180.0,
            },
            depth: 0,
            files: 12,
            arrivals: 18,
            parent: None,
            seed: stable_hash("src"),
        };
        let polygon = organic_points(district.rect, district.seed, district.depth);
        assert_eq!(polygon.len(), 8);
        for [x, y] in polygon {
            assert!((100.0..=300.0).contains(&x), "x {x} escaped the ward");
            assert!((150.0..=330.0).contains(&y), "y {y} escaped the ward");
        }
    }

    #[test]
    fn a_holding_names_the_ward_it_stands_in() {
        let mapped = map_building("file-0".to_owned(), &building("lib.rs"));
        assert_eq!(mapped.ward_id.as_deref(), Some("ward-0"));
    }

    #[test]
    fn the_sun_direction_is_a_unit_vector_pointing_down() {
        let sun = sun();
        let length = (sun.direction[0] * sun.direction[0]
            + sun.direction[1] * sun.direction[1]
            + sun.direction[2] * sun.direction[2])
            .sqrt();
        assert!((length - 1.0).abs() < 1e-5, "length was {length}");
        assert!(sun.direction[1] < 0.0, "the sun must shine downwards");
    }

    #[test]
    fn a_city_without_wards_still_produces_ground_and_light() {
        let layout = CityLayout {
            ward_gap: WARD_GAP,
            buildings: Vec::new(),
            districts: Vec::new(),
            corridors: Vec::new(),
            extent: reference_extent(),
        };
        let world = build_city_world(&layout, "project");
        assert!(world.roads.is_empty());
        assert!(world.plazas.is_empty());
        assert_eq!(world.rim.len(), RIM_SEGMENTS);
        assert!(world.bounds.width > 0.0 && world.bounds.depth > 0.0);
    }

    /// One rectangle of built ground, as the disk is measured against.
    fn built(rect: Rect) -> Vec<MapRect> {
        vec![map_rect(rect)]
    }

    /// The rim is a circle, and the whole look rests on it being one.
    #[test]
    fn the_rim_is_a_circle_about_the_ground_it_holds() {
        let ground = Rect {
            x: -300.0,
            y: 120.0,
            width: 900.0,
            height: 400.0,
        };
        let disk = disk(&built(ground), ground);
        let rim = disk.outline();

        assert_eq!(rim.len(), RIM_SEGMENTS);
        let (cx, cy) = ground.center();
        for [x, y] in &rim {
            let radius = ((x - cx).powi(2) + (y - cy).powi(2)).sqrt();
            assert!(
                (radius - disk.radius).abs() < 0.01,
                "a rim point sat {radius} from the centre, not {}",
                disk.radius
            );
        }
    }

    /// The reason the rim circumscribes rather than fits: towns are packed into
    /// the corners of the ground, and a tighter circle would drop one into the
    /// void.
    #[test]
    fn every_corner_of_the_ground_stands_on_the_disk() {
        for ground in [
            Rect {
                x: 0.0,
                y: 0.0,
                width: 1_000.0,
                height: 1_000.0,
            },
            // A long thin realm, which is the shape that strains a circle most.
            Rect {
                x: 40.0,
                y: -900.0,
                width: 4_000.0,
                height: 600.0,
            },
        ] {
            let disk = disk(&built(ground), ground);
            let (cx, cy) = ground.center();
            for (x, y) in [
                (ground.x, ground.y),
                (ground.x + ground.width, ground.y),
                (ground.x, ground.y + ground.height),
                (ground.x + ground.width, ground.y + ground.height),
            ] {
                let radius = ((x - cx).powi(2) + (y - cy).powi(2)).sqrt();
                assert!(
                    radius < disk.radius,
                    "the corner at ({x}, {y}) sat {radius} out, past a rim at {}",
                    disk.radius
                );
            }
        }
    }

    /// A disk drawn round the ground the layout *claimed* rather than round
    /// what was built on it is how the kingdom ends up marooned in the middle
    /// of a bare green plate: `hug` already leaves a fringe, and circumscribing
    /// that fringe squares it up.
    #[test]
    fn the_disk_is_measured_against_what_was_built() {
        let claimed = Rect {
            x: 0.0,
            y: 0.0,
            width: 2_000.0,
            height: 2_000.0,
        };
        let settled = Rect {
            x: 700.0,
            y: 700.0,
            width: 600.0,
            height: 600.0,
        };
        let tight = disk(&built(settled), claimed);
        let loose = disk(&[], claimed);

        assert!(
            tight.radius < loose.radius * 0.6,
            "a disk round the built ground came out {} against {}",
            tight.radius,
            loose.radius
        );
        // And it is centred on the settlement rather than on the claim.
        let (cx, cy) = settled.center();
        assert!((tight.center[0] - cx).abs() < 1e-3 && (tight.center[1] - cy).abs() < 1e-3);
    }

    /// The bounds are what the camera frames, so they have to hold the rim --
    /// and only the rim, since there is nothing outside it any more.
    #[test]
    fn the_bounds_are_the_disk_itself() {
        let disk = disk(&built(reference_extent()), reference_extent());
        let bounds = disk.bounds();
        for [x, y] in disk.outline() {
            assert!(
                bounds.contains([x, y]),
                "a rim point at ({x}, {y}) fell outside the bounds"
            );
        }
        assert!((bounds.width - disk.radius * 2.0).abs() < 1e-3);
        assert!((bounds.depth - disk.radius * 2.0).abs() < 1e-3);
    }

    /// The underside is measured against the disk's own radius, so a large
    /// kingdom and a small one hang the same way rather than one growing a
    /// spike and the other a stub.
    #[test]
    fn the_underside_grows_with_the_disk() {
        let ground = reference_extent();
        let wide = Rect {
            x: 0.0,
            y: 0.0,
            width: ground.width * 4.0,
            height: ground.height * 4.0,
        };
        let small = disk(&built(ground), ground).underside();
        let large = disk(&built(wide), wide).underside();

        assert!((large.depth / small.depth - 4.0).abs() < 1e-3);
        assert!((large.cliff / small.cliff - 4.0).abs() < 1e-3);
        assert_eq!(large.taper, small.taper);

        // And the spire has to clear the near rim, or none of it is ever seen:
        // at the isometric angle a radius falls 0.577 down the screen and a
        // depth falls 0.816, so `depth > 0.707 * radius`.
        let radius = disk(&built(ground), ground).radius;
        assert!(
            small.depth > radius * 0.707,
            "a spire {} deep hides behind a disk of radius {radius}",
            small.depth
        );
    }

    /// A world with no ground at all must still produce a disk with area, since
    /// everything downstream measures against its radius.
    #[test]
    fn a_degenerate_extent_still_has_a_disk() {
        let disk = disk(
            &[],
            Rect {
                x: 5.0,
                y: 5.0,
                width: 0.0,
                height: 0.0,
            },
        );
        assert!(disk.radius > 0.0);
        assert_eq!(disk.outline().len(), RIM_SEGMENTS);
        assert!(disk.bounds().width > 0.0);
    }
}
