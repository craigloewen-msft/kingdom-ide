//! The wire format: world geometry plus everything the interface needs to
//! answer "what am I looking at?".
//!
//! The manifest is the boundary between this crate and any renderer.

use std::collections::HashMap;

pub use crate::map::{MapCrumb, MapDistrict, MapFeature, MapLocation, MapManifest, MapWorld};

use crate::build::layout::{Building, CityLayout, District, Rect, WorldLayout};
use crate::build::model::{Category, Repository};
use crate::build::scene::{city_feature_id, map_rect, realm_feature_id};

/// Looks a folder path up to the ward that stands for it.
///
/// A breadcrumb has to name every folder between the root and the file, but
/// only some of those folders became wards, and in a realm a ward's identifier
/// carries a town prefix its path does not. Resolving through the layout is
/// what keeps the two in step.
fn wards_by_path(districts: &[District]) -> HashMap<&str, &str> {
    districts
        .iter()
        .map(|district| (district.path.as_str(), district.id.as_str()))
        .collect()
}

/// The folder holding a file, relative to the repository root.
fn folder_of(path: &str) -> &str {
    match path.rfind('/') {
        Some(index) => &path[..index],
        None => "",
    }
}

/// The trail of folders leading to a file, outermost first.
fn breadcrumb(folder: &str, wards: &HashMap<&str, &str>) -> Vec<MapCrumb> {
    if folder.is_empty() {
        return Vec::new();
    }
    let mut trail = Vec::new();
    let mut prefix = String::new();
    for segment in folder.split('/') {
        if !prefix.is_empty() {
            prefix.push('/');
        }
        prefix.push_str(segment);
        trail.push(MapCrumb {
            ward_id: wards.get(prefix.as_str()).map(|id| (*id).to_owned()),
            name: segment.to_owned(),
        });
    }
    trail
}

/// Builds the manifest for a single settlement.
///
/// `world` comes from [`build_city_world`](crate::build::scene::build_city_world) on
/// the same layout; passing a world built from a different one produces a map
/// whose geometry and interaction data disagree.
pub fn build_city_manifest(
    repository: &Repository,
    layout: &CityLayout,
    world: MapWorld,
) -> MapManifest {
    let mut major_districts: Vec<_> = layout
        .districts
        .iter()
        .filter(|district| district.depth == 0)
        .collect();
    major_districts.sort_by_key(|district| std::cmp::Reverse(district.files));
    let locations = major_districts
        .iter()
        .enumerate()
        .map(|(index, district)| {
            location_from_rect(
                format!("ward-{index}"),
                district.name.clone(),
                format!(
                    "{} files · {}",
                    format_number(district.files),
                    district.path
                ),
                district.rect,
            )
        })
        .collect();
    let districts = major_districts
        .iter()
        .enumerate()
        .map(|(index, district)| {
            district_from_rect(
                format!("ward-{index}"),
                district.name.clone(),
                format!("{} files", format_number(district.files)),
                district.rect,
            )
        })
        .collect();
    let mut buildings: Vec<_> = layout.buildings.iter().enumerate().collect();
    buildings
        .sort_by(|(_, left), (_, right)| building_depth(left).total_cmp(&building_depth(right)));
    let wards = wards_by_path(&layout.districts);
    let features = buildings
        .into_iter()
        .map(|(index, building)| {
            feature_from_building(
                city_feature_id(index),
                building,
                &repository.root.name,
                &wards,
            )
        })
        .collect();

    MapManifest {
        title: format!("{} · Repository City", repository.root.name),
        subtitle: format!(
            "{} holdings across {} lines",
            format_number(repository.root.metrics.file_count),
            format_number(repository.root.metrics.lines)
        ),
        world,
        districts,
        locations,
        features,
    }
}

/// Builds the manifest for a realm of towns.
///
/// `world` comes from [`build_realm_world`](crate::build::scene::build_realm_world)
/// on the same layout.
pub fn build_world_manifest(
    world_name: &str,
    repositories: &[Repository],
    layout: &WorldLayout,
    world: MapWorld,
) -> MapManifest {
    let mut towns: Vec<_> = layout.towns.iter().collect();
    towns.sort_by_key(|town| {
        let repository = &repositories[town.repository_index];
        std::cmp::Reverse(repository.root.metrics.file_count + repository.omitted_files)
    });
    let locations = towns
        .iter()
        .enumerate()
        .map(|(index, town)| {
            let repository = &repositories[town.repository_index];
            let files = repository.root.metrics.file_count + repository.omitted_files;
            location_from_rect(
                format!("town-{index}"),
                town.name.clone(),
                format!(
                    "{} files · {} lines",
                    format_number(files),
                    format_number(repository.root.metrics.lines)
                ),
                town.rect,
            )
        })
        .collect();
    let districts = towns
        .iter()
        .enumerate()
        .map(|(index, town)| {
            let repository = &repositories[town.repository_index];
            district_from_rect(
                format!("town-{index}"),
                town.name.clone(),
                format!(
                    "{} files · {} lines",
                    format_number(repository.root.metrics.file_count + repository.omitted_files),
                    format_number(repository.root.metrics.lines)
                ),
                town.rect,
            )
        })
        .collect();

    let mut buildings: Vec<_> = layout
        .towns
        .iter()
        .flat_map(|town| {
            let repository = &repositories[town.repository_index];
            town.city
                .buildings
                .iter()
                .enumerate()
                .map(move |(building_index, building)| {
                    (
                        town.repository_index,
                        building_index,
                        building,
                        repository.root.name.as_str(),
                    )
                })
        })
        .collect();
    buildings.sort_by(|left, right| building_depth(left.2).total_cmp(&building_depth(right.2)));
    // Two repositories both having a `src` would collapse into one entry in a
    // realm-wide lookup, so each town resolves its own folders.
    let wards: HashMap<usize, HashMap<&str, &str>> = layout
        .towns
        .iter()
        .map(|town| (town.repository_index, wards_by_path(&town.city.districts)))
        .collect();
    let empty = HashMap::new();
    let features = buildings
        .into_iter()
        .map(
            |(repository_index, building_index, building, repository_name)| {
                feature_from_building(
                    realm_feature_id(repository_index, building_index),
                    building,
                    repository_name,
                    wards.get(&repository_index).unwrap_or(&empty),
                )
            },
        )
        .collect();
    let files: usize = repositories
        .iter()
        .map(|repository| repository.root.metrics.file_count + repository.omitted_files)
        .sum();

    MapManifest {
        title: format!("{world_name} · Code Realm"),
        subtitle: format!(
            "{} towns · {} holdings",
            repositories.len(),
            format_number(files)
        ),
        world,
        districts,
        locations,
        features,
    }
}

/// A named place the camera can frame, in world units.
///
/// The padding keeps a little ground visible around the ward instead of
/// clipping the camera to its exact edge.
fn location_from_rect(id: String, label: String, detail: String, rect: Rect) -> MapLocation {
    MapLocation {
        id,
        label,
        detail,
        center: [rect.x + rect.width * 0.5, rect.y + rect.height * 0.5],
        extent: [
            (rect.width * 1.25).max(90.0),
            (rect.height * 1.25).max(90.0),
        ],
    }
}

fn district_from_rect(id: String, label: String, detail: String, rect: Rect) -> MapDistrict {
    MapDistrict {
        id,
        label,
        detail,
        polygon: vec![
            [rect.x, rect.y],
            [rect.x + rect.width, rect.y],
            [rect.x + rect.width, rect.y + rect.height],
            [rect.x, rect.y + rect.height],
        ],
        center: [rect.x + rect.width * 0.5, rect.y + rect.height * 0.5],
    }
}

/// The file behind a holding, plus the world-space ground it stands on.
///
/// Hit testing used to need a pre-projected silhouette polygon. The renderer
/// now picks the real mesh, so the feature only carries the footprint and
/// height the details panel and minimap need.
fn feature_from_building(
    id: String,
    building: &Building,
    repository: &str,
    wards: &HashMap<&str, &str>,
) -> MapFeature {
    let footprint = building.footprint();
    let folder = folder_of(&building.path);
    MapFeature {
        id,
        name: building.name.clone(),
        path: building.path.clone(),
        repository: repository.to_owned(),
        folder: folder.to_owned(),
        breadcrumb: breadcrumb(folder, wards),
        building_kind: building.kind.label().to_owned(),
        meaning: building.kind.meaning().to_owned(),
        category: category_label(building.category).to_owned(),
        bytes: building.bytes,
        lines: building.lines,
        complexity: building.complexity,
        references: building.references,
        footprint: map_rect(footprint),
        height: building.height(),
        center: [
            footprint.x + footprint.width * 0.5,
            footprint.y + footprint.height * 0.5,
        ],
    }
}

fn category_label(category: Category) -> &'static str {
    match category {
        Category::Source => "Source",
        Category::Web => "Web / UI",
        Category::Test => "Test",
        Category::Docs => "Documentation",
        Category::Config => "Configuration",
        Category::Data => "Data",
        Category::Asset => "Asset",
        Category::Script => "Script / Tooling",
        Category::Other => "Other",
    }
}

fn building_depth(building: &Building) -> f32 {
    building.lot.x + building.lot.y + building.lot.width + building.lot.height
}

fn format_number(value: usize) -> String {
    let digits = value.to_string();
    let mut output = String::new();
    for (index, character) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            output.push(',');
        }
        output.push(character);
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::build::layout::{Building, WARD_GAP, reference_extent};
    use crate::build::scene::build_city_world;
    use crate::map::BuildingKind;

    fn building() -> Building {
        Building {
            name: "main.rs".to_owned(),
            path: "src/main.rs".to_owned(),
            ward_id: Some("ward-0".to_owned()),
            category: Category::Source,
            kind: BuildingKind::Keep,
            lot: Rect {
                x: 100.0,
                y: 120.0,
                width: 80.0,
                height: 60.0,
            },
            bytes: 2048,
            lines: 90,
            code_lines: 80,
            complexity: 7,
            references: 3,
            scale: 1.0,
        }
    }

    #[test]
    fn feature_manifest_preserves_file_identity_and_world_footprint() {
        let building = building();
        let feature =
            feature_from_building("file-0".to_owned(), &building, "demo", &HashMap::new());
        let footprint = building.footprint();

        assert_eq!(feature.path, "src/main.rs");
        assert_eq!(feature.repository, "demo");
        assert_eq!(feature.building_kind, "KEEP");
        assert_eq!(feature.footprint.x, footprint.x);
        assert_eq!(feature.footprint.depth, footprint.height);
        assert_eq!(feature.height, building.height());
        assert!(feature.footprint.contains(feature.center));
    }

    #[test]
    fn a_feature_names_the_folder_it_belongs_to() {
        let mut building = building();
        building.path = "viewer/src/engine/mod.rs".to_owned();
        let wards = HashMap::from([("viewer", "ward-0"), ("viewer/src/engine", "ward-4")]);
        let feature = feature_from_building("file-0".to_owned(), &building, "demo", &wards);

        assert_eq!(feature.folder, "viewer/src/engine");
        let names: Vec<&str> = feature
            .breadcrumb
            .iter()
            .map(|crumb| crumb.name.as_str())
            .collect();
        assert_eq!(names, ["viewer", "src", "engine"]);
        // Only the folders that actually became wards can be navigated to; a
        // folder the layout merged away is still named, but is not a link.
        let ids: Vec<Option<&str>> = feature
            .breadcrumb
            .iter()
            .map(|crumb| crumb.ward_id.as_deref())
            .collect();
        assert_eq!(ids, [Some("ward-0"), None, Some("ward-4")]);
    }

    #[test]
    fn a_file_at_the_root_has_no_folder_trail() {
        let mut building = building();
        building.path = "README.md".to_owned();
        let feature =
            feature_from_building("file-0".to_owned(), &building, "demo", &HashMap::new());
        assert_eq!(feature.folder, "");
        assert!(feature.breadcrumb.is_empty());
    }

    #[test]
    fn features_and_scene_buildings_share_one_identifier() {
        let layout = CityLayout {
            ward_gap: WARD_GAP,
            buildings: vec![building()],
            districts: Vec::new(),
            corridors: Vec::new(),
            extent: reference_extent(),
        };
        let world = build_city_world(&layout, "project");
        let feature = feature_from_building(
            city_feature_id(0),
            &layout.buildings[0],
            "demo",
            &HashMap::new(),
        );

        assert_eq!(world.buildings.len(), 1);
        assert_eq!(world.buildings[0].feature_id, feature.id);
        assert_eq!(world.buildings[0].height, feature.height);
    }

    #[test]
    fn locations_frame_more_ground_than_the_ward_covers() {
        let rect = Rect {
            x: 100.0,
            y: 150.0,
            width: 200.0,
            height: 180.0,
        };
        let location = location_from_rect(
            "ward-0".to_owned(),
            "src".to_owned(),
            "12 files".to_owned(),
            rect,
        );

        assert_eq!(location.center, [200.0, 240.0]);
        assert!(location.extent[0] > rect.width);
        assert!(location.extent[1] > rect.height);
    }
}
