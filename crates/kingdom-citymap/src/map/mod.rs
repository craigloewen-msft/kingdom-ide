//! The interaction manifest: the wire format between the builder and the
//! renderer.
//!
//! These are the types [`crate::build`] writes and [`crate::engine`] reads. The
//! module deliberately holds nothing else: no scanner, no renderer, no camera.
//! That is why it is the one part of this crate compiled on **both** targets --
//! the server builds a manifest and the browser draws one.
//!
//! Everything is in world space. Nothing is projected, because the renderer
//! owns the camera — the manifest only ever says where things stand.
//!
//! ```
//! use kingdom_citymap::map::MapManifest;
//!
//! # fn load(json: &str) -> Result<(), Box<dyn std::error::Error>> {
//! let manifest: MapManifest = serde_json::from_str(json)?;
//! # Ok(())
//! # }
//! ```

use serde::{Deserialize, Serialize};

/// An sRGB colour with alpha.
pub type MapColor = [u8; 4];

/// A world-space rectangle on the ground plane.
///
/// `x`/`y` are ground coordinates and `depth` runs along the ground `y` axis.
/// Nothing here is projected: the renderer owns the camera, so the manifest
/// only ever describes where things stand in the world.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct MapRect {
    /// Left edge.
    pub x: f32,
    /// Near edge, along the ground `y` axis.
    pub y: f32,
    /// Extent along `x`.
    pub width: f32,
    /// Extent along `y`.
    pub depth: f32,
}

impl MapRect {
    /// The middle of the rectangle.
    pub fn center(&self) -> [f32; 2] {
        [self.x + self.width * 0.5, self.y + self.depth * 0.5]
    }

    /// The right edge.
    pub fn max_x(&self) -> f32 {
        self.x + self.width
    }

    /// The far edge.
    pub fn max_y(&self) -> f32 {
        self.y + self.depth
    }

    /// Whether a ground point lies inside, edges included.
    pub fn contains(&self, point: [f32; 2]) -> bool {
        point[0] >= self.x
            && point[0] <= self.max_x()
            && point[1] >= self.y
            && point[1] <= self.max_y()
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
/// A whole map: the geometry to draw, plus what the interface needs to answer
/// "what am I looking at?".
pub struct MapManifest {
    /// The repository or realm name.
    pub title: String,
    /// A one-line summary, such as the file and line totals.
    pub subtitle: String,
    /// Everything to build the scene from.
    pub world: MapWorld,
    /// Plaques shown when the camera is too far out for architecture.
    pub districts: Vec<MapDistrict>,
    /// Named places the camera can jump to.
    pub locations: Vec<MapLocation>,
    /// One entry per file, holding the detail a selection panel shows.
    pub features: Vec<MapFeature>,
}

/// Everything the renderer needs to build the scene, in world space.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct MapWorld {
    /// Everything the world covers, which is what the camera frames.
    pub bounds: MapRect,
    /// The colour beyond the water.
    pub sky: MapColor,
    /// The land the settlement stands on.
    pub ground: MapColor,
    /// Open water, which runs to the horizon in every direction.
    pub water: MapColor,
    /// The shallower water lapping at the shore, between `shoreline` and
    /// `moat`. Keeping it separate is what stops the island from looking like
    /// it was cut out and pasted onto a flat sea.
    pub shallows: MapColor,
    /// The outline of the island, filled with `ground`.
    pub shoreline: Vec<[f32; 2]>,
    /// The outer edge of the shallows, just beyond `shoreline`.
    pub moat: Vec<[f32; 2]>,
    /// The light the renderer shades everything with.
    pub sun: MapSun,
    /// One per repository. Empty for a single settlement.
    pub towns: Vec<MapTown>,
    /// Every folder, at every nesting depth.
    pub wards: Vec<MapWard>,
    /// Paved squares at the heart of each settlement.
    pub plazas: Vec<MapPlaza>,
    /// Paths between wards and roads between towns.
    pub roads: Vec<MapRoad>,
    /// Every file.
    pub buildings: Vec<MapBuilding>,
    /// Trees and shoreline posts.
    pub scenery: Vec<MapScenery>,
    /// Folder names painted flat onto the ground they belong to.
    pub ground_labels: Vec<MapGroundLabel>,
}

/// The single directional light plus ambient fill.
///
/// Face shading used to be baked per polygon; the engine now derives it from
/// this light, so the manifest carries the light instead of the shading.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct MapSun {
    /// Direction the light travels, in world space.
    pub direction: [f32; 3],
    /// The light's colour.
    pub color: MapColor,
    /// Brightness in lux.
    pub illuminance: f32,
    /// The fill colour lighting surfaces the sun does not reach.
    pub ambient: MapColor,
    /// How strong that fill is.
    pub ambient_brightness: f32,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
/// One repository's settlement within a realm.
pub struct MapTown {
    /// Unique within the manifest.
    pub id: String,
    /// The repository name.
    pub name: String,
    /// The ground the town covers.
    pub rect: MapRect,
    /// The drawn outline, with corners eased off.
    pub polygon: Vec<[f32; 2]>,
    /// Fill colour for the town's ground.
    pub ground: MapColor,
    /// Colour of its border.
    pub edge: MapColor,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
/// One folder, drawn as a ward or a neighbourhood inside one.
pub struct MapWard {
    /// Unique within the manifest. Buildings and ground labels refer to it.
    pub id: String,
    /// The folder's own name, without its parents.
    pub name: String,
    /// The folder's path relative to the repository root.
    pub path: String,
    /// The ward this one sits inside, if it is not a top-level folder.
    pub parent: Option<String>,
    /// Files at or below this folder.
    pub files: u32,
    /// The ground the ward covers.
    pub rect: MapRect,
    /// The drawn outline, with corners eased off.
    pub polygon: Vec<[f32; 2]>,
    /// Nesting depth, zero for a top-level folder.
    pub depth: u32,
    /// Fill colour for the ward's ground.
    pub ground: MapColor,
    /// Colour of its border.
    pub edge: MapColor,
}

/// A folder name painted onto its ward's ground.
///
/// The generator decides where the name fits and how big it may be; turning it
/// into geometry is the renderer's job, the same division every other part of
/// the manifest follows.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct MapGroundLabel {
    /// The [`MapWard`] this names.
    pub ward_id: String,
    /// Already shortened to something that fits the ward.
    pub text: String,
    /// The left end of the text baseline, in world coordinates.
    pub origin: [f32; 2],
    /// Cap height in world units.
    pub size: f32,
    /// The widest the text may be. The renderer knows its own glyph metrics
    /// and the generator does not, so the generator reserves the space and the
    /// renderer condenses the text into it. That keeps a name inside its ward
    /// without the two halves having to agree on a font.
    pub max_width: f32,
    /// Stroke width in world units.
    pub stroke: f32,
    /// `false` lays the text along the ground `x` axis, `true` along `y`.
    pub vertical: bool,
    /// Text colour.
    pub color: MapColor,
    /// The ward's nesting depth, so a renderer can order or filter by it.
    pub depth: u32,
    /// The renderer hides the label until a cap is at least this many pixels
    /// tall, which is what keeps a nested folder's name out of the way until
    /// the camera is close enough for it to mean anything.
    pub min_pixel_height: f32,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
/// The paved square at the heart of a settlement.
pub struct MapPlaza {
    /// The ground it covers.
    pub rect: MapRect,
    /// Its paving colour.
    pub color: MapColor,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
/// What a road connects.
///
/// The three kinds form one continuous network rather than three unrelated
/// sets of lines: a [`Street`](RoadKind::Street) is carved between the folders
/// inside a ward, a [`Ward`](RoadKind::Ward) avenue carries a ward's traffic
/// out to the settlement's central square, and a [`Realm`](RoadKind::Realm)
/// road joins whole towns.
pub enum RoadKind {
    /// A road between towns in a multi-repository realm.
    Realm,
    /// An avenue from a ward out to the settlement's central square.
    Ward,
    /// A street carved between the folders and files inside a ward.
    Street,
    /// The short path from a holding's door out to the street it fronts onto.
    Drive,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
/// A path or road, as a polyline to be stroked on the ground.
pub struct MapRoad {
    /// Whether this runs between towns, out of a ward, or inside one.
    pub kind: RoadKind,
    /// The centre line, in world coordinates.
    pub points: Vec<[f32; 2]>,
    /// How wide to stroke it, in world units.
    pub width: f32,
    /// How many files this road carries, counting everything that would have
    /// to travel along it to reach the settlement's central square.
    ///
    /// This is what `width` is derived from, and it is what makes the network
    /// say something: a road is thick because a great deal of the repository
    /// depends on reaching what lies at the end of it.
    pub traffic: u32,
    /// Paving colour.
    pub color: MapColor,
    /// Colour of the verge either side.
    pub edge: MapColor,
}

/// A single holding, described as geometry to build rather than pixels to draw.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct MapBuilding {
    /// Links back to the [`MapFeature`] that describes the file.
    pub feature_id: String,
    /// The innermost ward holding this file, if it is not loose at the root.
    pub ward_id: Option<String>,
    /// The archetype to build, which fixes the silhouette.
    pub kind: BuildingKind,
    /// The ground the building itself covers.
    pub footprint: MapRect,
    /// The whole plot, including the open ground around the building.
    pub lot: MapRect,
    /// How tall it stands, in world units.
    pub height: f32,
    /// The colours to build it from.
    pub palette: MapPalette,
    /// A rough branch count, which drives how ornate it looks.
    pub complexity: u32,
    /// Stable per-file variation, so a rebuild never reshuffles the town.
    pub seed: u32,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
/// The colours one building is built from.
pub struct MapPalette {
    /// Wall colour.
    pub wall: MapColor,
    /// Roof colour.
    pub roof: MapColor,
    /// Beams, banners, and other detail.
    pub trim: MapColor,
    /// Window colour.
    pub window: MapColor,
    /// The lot the building stands on.
    pub ground: MapColor,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "camelCase")]
/// Something small standing on open ground.
pub enum MapScenery {
    /// A tree.
    Tree {
        /// Where it stands.
        position: [f32; 2],
        /// Overall height in world units.
        height: f32,
        /// Canopy radius in world units.
        radius: f32,
        /// Canopy colour.
        foliage: MapColor,
        /// Trunk colour.
        trunk: MapColor,
    },
    /// A shoreline marker post.
    Post {
        /// Where it stands.
        position: [f32; 2],
        /// Height in world units.
        height: f32,
        /// Its colour.
        color: MapColor,
    },
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "camelCase")]
/// The archetype a file is drawn as.
///
/// The mapping from file role to archetype is fixed across every repository,
/// so a watchtower means the same thing wherever it stands.
pub enum BuildingKind {
    /// A top-level entrypoint or architectural hub.
    Keep,
    /// Source code and business logic.
    Guildhall,
    /// Web and user interface code.
    Market,
    /// Tests and verification.
    Watchtower,
    /// Documentation and project knowledge.
    Scriptorium,
    /// Configuration, manifests, and project rules.
    CouncilHall,
    /// Data, schemas, queries, and protocols.
    Granary,
    /// Images, fonts, audio, and other assets, drawn as an open yard of crates.
    Stockpile,
    /// Scripts, automation, and developer tooling.
    Forge,
    /// Anything that fits nowhere else.
    Cottage,
}

impl BuildingKind {
    /// Every archetype, in legend order.
    pub const ALL: [Self; 10] = [
        Self::Keep,
        Self::Guildhall,
        Self::Market,
        Self::Watchtower,
        Self::Scriptorium,
        Self::CouncilHall,
        Self::Granary,
        Self::Stockpile,
        Self::Forge,
        Self::Cottage,
    ];

    /// The archetype's display name, in the interface's upper case.
    pub fn label(self) -> &'static str {
        match self {
            Self::Keep => "KEEP",
            Self::Guildhall => "GUILDHALL",
            Self::Market => "MARKET",
            Self::Watchtower => "WATCHTOWER",
            Self::Scriptorium => "SCRIPTORIUM",
            Self::CouncilHall => "COUNCIL HALL",
            Self::Granary => "GRANARY",
            Self::Stockpile => "STOCKPILE",
            Self::Forge => "FORGE",
            Self::Cottage => "COTTAGE",
        }
    }

    /// What the archetype says about the file, for a legend.
    pub fn meaning(self) -> &'static str {
        match self {
            Self::Keep => "ENTRYPOINT / HUB",
            Self::Guildhall => "SOURCE / LOGIC",
            Self::Market => "WEB / UI",
            Self::Watchtower => "TEST / VERIFY",
            Self::Scriptorium => "DOCS / KNOWLEDGE",
            Self::CouncilHall => "CONFIG / RULES",
            Self::Granary => "DATA / SCHEMA",
            Self::Stockpile => "ASSET / MEDIA",
            Self::Forge => "SCRIPT / TOOLING",
            Self::Cottage => "MISCELLANEOUS",
        }
    }
}

/// A ward or town plaque shown when the camera is too far out for architecture.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct MapDistrict {
    /// The [`MapWard`] or [`MapTown`] this stands for.
    pub id: String,
    /// The name to show.
    pub label: String,
    /// A secondary line, such as the file count.
    pub detail: String,
    /// The area the plaque covers.
    pub polygon: Vec<[f32; 2]>,
    /// Where to anchor it.
    pub center: [f32; 2],
}

/// A named place the viewer can frame the camera on.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct MapLocation {
    /// The ward or town this frames.
    pub id: String,
    /// The name to show in a jump list.
    pub label: String,
    /// A secondary line, such as the file count.
    pub detail: String,
    /// Where to centre the camera.
    pub center: [f32; 2],
    /// World-space width and depth to frame.
    pub extent: [f32; 2],
}

/// One step of a file's folder trail, carrying enough to navigate to it.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct MapCrumb {
    /// The [`MapWard`] this step names, when the folder became a ward.
    pub ward_id: Option<String>,
    /// The folder's own name.
    pub name: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
/// Everything the interface shows about one file.
pub struct MapFeature {
    /// Unique within the manifest. [`MapBuilding::feature_id`] points here.
    pub id: String,
    /// The file name.
    pub name: String,
    /// The path relative to the repository root.
    pub path: String,
    /// Which repository the file came from.
    pub repository: String,
    /// The folder holding this file, relative to the repository root. Empty
    /// when the file sits loose at the root.
    pub folder: String,
    /// `folder` split into the ward at each level, outermost first, so the
    /// interface can offer the trail as navigation rather than as text.
    pub breadcrumb: Vec<MapCrumb>,
    /// The archetype's label, ready to show.
    pub building_kind: String,
    /// What that archetype means, ready to show.
    pub meaning: String,
    /// The file's role.
    pub category: String,
    /// Size on disk.
    pub bytes: u64,
    /// Total lines.
    pub lines: usize,
    /// A rough branch count.
    pub complexity: usize,
    /// How many other files import or include this one.
    pub references: usize,
    /// The ground the building covers.
    pub footprint: MapRect,
    /// How tall it stands.
    pub height: f32,
    /// Where to centre the camera when framing this file.
    pub center: [f32; 2],
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rect_reports_center_and_containment() {
        let rect = MapRect {
            x: 10.0,
            y: 20.0,
            width: 40.0,
            depth: 60.0,
        };
        assert_eq!(rect.center(), [30.0, 50.0]);
        assert!(rect.contains([30.0, 50.0]));
        assert!(!rect.contains([9.0, 50.0]));
        assert!(!rect.contains([30.0, 81.0]));
    }

    #[test]
    fn building_kinds_round_trip_as_camel_case() {
        let json = serde_json::to_string(&BuildingKind::CouncilHall).expect("serialize");
        assert_eq!(json, "\"councilHall\"");
        let parsed: BuildingKind = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(parsed, BuildingKind::CouncilHall);
    }

    #[test]
    fn every_building_kind_has_a_label_and_meaning() {
        for kind in BuildingKind::ALL {
            assert!(!kind.label().is_empty());
            assert!(!kind.meaning().is_empty());
        }
    }

    #[test]
    fn ground_labels_round_trip_as_camel_case() {
        let label = MapGroundLabel {
            ward_id: "ward-3".to_owned(),
            text: "SRC".to_owned(),
            origin: [12.0, 34.0],
            size: 9.0,
            max_width: 80.0,
            stroke: 1.4,
            vertical: false,
            color: [200, 190, 160, 255],
            depth: 1,
            min_pixel_height: 7.0,
        };
        let json = serde_json::to_string(&label).expect("serialize");
        assert!(json.contains("\"wardId\""), "{json}");
        assert!(json.contains("\"minPixelHeight\""), "{json}");
        let parsed: MapGroundLabel = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(parsed, label);
    }

    #[test]
    fn a_ward_carries_its_folder_identity_and_parent() {
        let ward = MapWard {
            id: "ward-4".to_owned(),
            name: "engine".to_owned(),
            path: "viewer/src/engine".to_owned(),
            parent: Some("ward-2".to_owned()),
            files: 9,
            rect: MapRect::default(),
            polygon: Vec::new(),
            depth: 2,
            ground: [0, 0, 0, 255],
            edge: [0, 0, 0, 255],
        };
        let json = serde_json::to_string(&ward).expect("serialize");
        let parsed: MapWard = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(parsed, ward);
        assert_eq!(parsed.parent.as_deref(), Some("ward-2"));
    }

    #[test]
    fn a_breadcrumb_round_trips_with_its_ward_links() {
        let crumbs = vec![
            MapCrumb {
                ward_id: Some("ward-0".to_owned()),
                name: "viewer".to_owned(),
            },
            MapCrumb {
                ward_id: None,
                name: "src".to_owned(),
            },
        ];
        let json = serde_json::to_string(&crumbs).expect("serialize");
        let parsed: Vec<MapCrumb> = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(parsed, crumbs);
    }
}
