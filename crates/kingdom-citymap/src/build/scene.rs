//! Builds the world-space scene the renderer turns into geometry.
//!
//! Nothing in this module projects, shades, or rasterises anything. It decides
//! *where things stand and what colour they are*; the rendering engine owns the
//! camera, depth sorting, lighting, and culling.


use crate::map::{
    MapBuilding, MapColor, MapPalette, MapPlaza, MapRect, MapSun, MapTown, MapWard, MapWorld,
};

use crate::build::layout::{
    Building, CityLayout, District, REFERENCE_WORLD, Rect, TownLayout, WorldLayout, stable_hash,
};
use crate::build::model::Category;
use crate::build::scenery::{SceneryInput, scenery};
use crate::build::streets;
use crate::build::wayfinding::{ground_labels, square_label, ward_palettes};

const SKY: MapColor = [18, 24, 36, 255];
const GROUND: MapColor = [73, 87, 52, 255];
/// Open sea, deeper and colder than the water near the shore.
const WATER: MapColor = [24, 48, 58, 255];
/// The shallows around the island, warmed by the sand under them.
const SHALLOWS: MapColor = [46, 84, 85, 255];

/// The size of the square at the heart of a settlement. Constant, like the
/// wards around it.
const PLAZA_SIZE: f32 = 52.0;

/// The island outline, given for a [`REFERENCE_WORLD`]-sized square and mapped
/// onto whatever ground the layout actually asked for.
const SHORELINE: [(f32, f32); 16] = [
    (47.0, 292.0),
    (127.0, 145.0),
    (299.0, 61.0),
    (472.0, 91.0),
    (635.0, 47.0),
    (832.0, 120.0),
    (946.0, 262.0),
    (909.0, 447.0),
    (963.0, 638.0),
    (869.0, 827.0),
    (703.0, 943.0),
    (509.0, 910.0),
    (322.0, 955.0),
    (134.0, 862.0),
    (65.0, 690.0),
    (103.0, 500.0),
];

/// The water ring just outside the shoreline, in the same reference square.
const MOAT: [(f32, f32); 16] = [
    (28.0, 285.0),
    (112.0, 126.0),
    (292.0, 38.0),
    (470.0, 72.0),
    (638.0, 25.0),
    (846.0, 103.0),
    (969.0, 252.0),
    (930.0, 445.0),
    (986.0, 641.0),
    (887.0, 845.0),
    (713.0, 966.0),
    (508.0, 932.0),
    (318.0, 979.0),
    (116.0, 881.0),
    (43.0, 697.0),
    (82.0, 497.0),
];

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
    let island = layout.extent;
    let shoreline = outline(&SHORELINE, island);
    let moat = outline(&MOAT, island);
    let (roads, plaza) = streets::settlement_roads(
        &layout.districts,
        &layout.buildings,
        &layout.corridors,
        island.center(),
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

    MapWorld {
        bounds: island_bounds(&moat),
        sky: SKY,
        ground: GROUND,
        water: WATER,
        shallows: SHALLOWS,
        sun: sun(),
        towns: Vec::new(),
        scenery: scenery(SceneryInput {
            shoreline: &shoreline,
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
        shoreline,
        moat,
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

    let island = layout.extent;
    let shoreline = outline(&SHORELINE, island);
    let moat = outline(&MOAT, island);

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
    let (mut roads, arrivals) = streets::highways(&towns_for_roads, island.center());
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
        bounds: island_bounds(&moat),
        sky: SKY,
        ground: GROUND,
        water: WATER,
        shallows: SHALLOWS,
        sun: sun(),
        towns: towns(&layout.towns),
        scenery: scenery(SceneryInput {
            shoreline: &shoreline,
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
        shoreline,
        moat,
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

/// Maps a reference-square outline onto the ground the layout asked for.
///
/// The island shape is authored once, in a 1000-unit square, and stretched to
/// whatever the settlement grew to. That is what lets the coastline stay the
/// same recognisable shape whether it rings thirty holdings or three thousand.
fn outline(source: &[(f32, f32)], island: Rect) -> Vec<[f32; 2]> {
    let scale_x = island.width / REFERENCE_WORLD;
    let scale_y = island.height / REFERENCE_WORLD;
    source
        .iter()
        .map(|(x, y)| [island.x + x * scale_x, island.y + y * scale_y])
        .collect()
}

fn island_bounds(moat: &[[f32; 2]]) -> MapRect {
    let mut min = [f32::MAX; 2];
    let mut max = [f32::MIN; 2];
    for [x, y] in moat {
        min[0] = min[0].min(*x);
        min[1] = min[1].min(*y);
        max[0] = max[0].max(*x);
        max[1] = max[1].max(*y);
    }
    if min[0] > max[0] {
        return MapRect {
            x: 0.0,
            y: 0.0,
            width: 1.0,
            depth: 1.0,
        };
    }
    MapRect {
        x: min[0],
        y: min[1],
        width: max[0] - min[0],
        depth: max[1] - min[1],
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
        assert_eq!(world.shoreline.len(), SHORELINE.len());
        assert!(world.bounds.width > 0.0 && world.bounds.depth > 0.0);
    }
}
