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

pub mod network;
pub mod works;

pub use network::{AgentMark, HostRing, Link, LinkKind, NetworkPicture, Wellhead};
pub use works::{Work, WorkBand, WorkSite};

/// An sRGB colour with alpha.
pub type MapColor = [u8; 4];

/// Where the map is being shown, and therefore how hard it should work.
///
/// The map is mounted exactly once for the life of the page -- see
/// [`crate::engine`] and `kingdom_app::app::ThroneRoom` for why it may never
/// unmount -- so it does not move between screens, it moves between
/// *rectangles*. This says which one it is currently standing in.
///
/// # Why this is in `map` and not in `engine::bridge`
///
/// It is the odd thing here: this module is otherwise world-space geometry,
/// and nothing about a viewport belongs in a wire format. It lives here
/// because it is a **prop type of `CityMap`**, and `lib.rs` carries an `ssr`
/// stub of that component whose signature must match the browser's
/// prop-for-prop -- its own doc notes that a prop on one target and not the
/// other is a build failure on whichever target is not being looked at. `map`
/// is the only module compiled to both, so it is the only place a shared prop
/// type can stand.
///
/// Deliberately three named states rather than two booleans. "Is it visible"
/// and "is it the whole screen" are not independent -- there is no fourth
/// combination -- and a pair of flags encoding three states is the shape that
/// eventually disagrees with itself.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum MapPresence {
    /// Not on screen at all: the King is in a chamber with the rail folded
    /// away. The engine stops.
    #[default]
    Hidden,
    /// A pane at the foot of the cities rail, beside a conversation. Small,
    /// glanced at rather than flown through, and drawn slowly.
    Rail,
    /// The whole main region, which is the map's own screen. Drawn fully.
    Full,
}

impl MapPresence {
    /// Whether the map is on screen at all, in either home.
    pub fn showing(self) -> bool {
        !matches!(self, Self::Hidden)
    }

    /// Whether this is the small pane in the rail.
    ///
    /// The rail's map is scoped to the city a plan works in, and the full one
    /// is not -- on his own map the King drives the camera and nothing should
    /// take it from him. So this is the question the focus effects ask.
    pub fn in_rail(self) -> bool {
        matches!(self, Self::Rail)
    }
}

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

impl MapManifest {
    /// The town a project's directory name stands for, if this map has one.
    ///
    /// Matched on [`MapLocation::label`] rather than on its id, and that is
    /// load-bearing rather than incidental: a manifest's `town-N` identifiers
    /// are numbered from two different orderings -- `scene::towns` enumerates
    /// the packing order while `manifest::build_world_manifest` sorts by file
    /// count -- so `town-0` need not mean the same settlement in both halves of
    /// one manifest. [`crate::engine::bridge::TownActivity`] records the same
    /// trap for the same reason, and a test below pins it.
    ///
    /// The **name** is what both halves agree on, and it is the same string a
    /// `CityId` is built from: `kingdom_app::scan` takes a project's
    /// `path.file_name()` and `build::scan_repository` takes its
    /// `root.file_name()`.
    ///
    /// Takes a `&str` and not a `CityId`, because this module is deliberately
    /// ignorant of Kingdom's domain -- it is the wire format, and `kingdom-core`
    /// is a dependency only of `build`.
    pub fn town_named(&self, name: &str) -> Option<&MapLocation> {
        self.locations.iter().find(|place| place.label == name)
    }

    /// Where one file's holding stands, if this map drew one for it.
    ///
    /// Both halves of the identity are checked. `path` alone would be ambiguous
    /// the moment two projects in one kingdom both have a `src/main.rs`, which
    /// on a kingdom of Rust projects is most of them.
    pub fn holding_at(&self, repository: &str, path: &str) -> Option<&MapFeature> {
        self.features
            .iter()
            .find(|feature| feature.repository == repository && feature.path == path)
    }

    /// The ward standing for a folder of one repository, if the map drew one.
    ///
    /// The folder is named the way [`MapWard::path`] and [`MapFeature::folder`]
    /// both are -- relative to the repository root, no leading slash -- so the
    /// folder of a file the King's agent has just created can be looked up
    /// directly from its path.
    ///
    /// # Why the repository is needed as well
    ///
    /// [`Self::holding_at`]'s reason, and it bites harder here: a `MapWard`'s
    /// `path` is repository-relative by deliberate decision (`layout::move_city`
    /// says so), so in a kingdom of Rust projects every town contributes a ward
    /// whose path is exactly `src`. Matching on the path alone would put a new
    /// file in whichever city's `src` happened to be built first.
    ///
    /// The identity that disambiguates them is which repository's *buildings*
    /// stand on the ward -- see [`Self::ward_belongs_to`]. Ward ids do encode
    /// the town (`layout::move_city` prefixes them), but that is an
    /// implementation detail of the layout rather than a promise of the wire
    /// format, so it is not parsed.
    ///
    /// Returns `None` for a folder the layout merged away, which is ordinary --
    /// not every folder becomes a ward, exactly as [`MapCrumb::ward_id`] is
    /// `None` for the steps that did not.
    pub fn ward_at(&self, repository: &str, folder: &str) -> Option<&MapWard> {
        self.world
            .wards
            .iter()
            .filter(|ward| ward.path == folder)
            .find(|ward| self.ward_belongs_to(&ward.id, repository))
    }

    /// Whether a named repository's files stand on a ward, or anywhere below it.
    ///
    /// Settled through the **buildings**, which are the only structural link
    /// between a ward and a repository: a [`MapBuilding`] names both the ward it
    /// stands on and the feature it was built from, and the feature names its
    /// repository. A feature's `folder` cannot serve here -- it is
    /// repository-relative, so every town's `src` features report the same
    /// folder and two towns' `src` wards would be indistinguishable, which is
    /// exactly the confusion this function exists to resolve.
    ///
    /// Descendants are searched because a building records only the *innermost*
    /// ward it stands in, so a folder holding nothing but sub-folders has no
    /// building of its own -- and answering `false` for one would leave a new
    /// file in `crates/` with nowhere to stand.
    ///
    /// The manifest arrives over the network, so the walk keeps a visited set:
    /// a `parent` chain that loops terminates by construction rather than by a
    /// bound that has to be chosen. `engine::wards::lineage` guards the same
    /// links walked the other way.
    fn ward_belongs_to(&self, ward_id: &str, repository: &str) -> bool {
        let owns = |id: &str| {
            self.world.buildings.iter().any(|building| {
                building.ward_id.as_deref() == Some(id)
                    && self.features.iter().any(|feature| {
                        feature.id == building.feature_id && feature.repository == repository
                    })
            })
        };

        let mut seen: Vec<&str> = vec![ward_id];
        let mut frontier: Vec<&str> = vec![ward_id];
        while let Some(current) = frontier.pop() {
            if owns(current) {
                return true;
            }
            for child in self.wards_inside(current) {
                if !seen.contains(&child.id.as_str()) {
                    seen.push(&child.id);
                    frontier.push(&child.id);
                }
            }
        }
        false
    }

    /// Every plot already spoken for on a ward, as ground a newcomer must avoid.
    ///
    /// The **lot** rather than the footprint, which is the whole point: a lot is
    /// the plot including the open ground around the house, so a ghost house
    /// placed clear of every lot lands on genuinely free land rather than in
    /// somebody's garden.
    ///
    /// Only the wards' *own* holdings are returned -- a building records the
    /// innermost ward it stands in ([`MapBuilding::ward_id`]) -- so a placer
    /// working on a folder that has sub-folders must ask about those too. The
    /// caller does exactly that, because it also has to keep out of the
    /// sub-wards' ground itself.
    pub fn lots_in<'a>(&'a self, ward_id: &'a str) -> impl Iterator<Item = MapRect> + 'a {
        self.world
            .buildings
            .iter()
            .filter(move |building| building.ward_id.as_deref() == Some(ward_id))
            .map(|building| building.lot)
    }

    /// The wards nested directly inside one, whose ground is not free either.
    pub fn wards_inside<'a>(&'a self, ward_id: &'a str) -> impl Iterator<Item = &'a MapWard> + 'a {
        self.world
            .wards
            .iter()
            .filter(move |ward| ward.parent.as_deref() == Some(ward_id))
    }
}

/// Everything the renderer needs to build the scene, in world space.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct MapWorld {
    /// Everything the world covers, which is what the camera frames.
    pub bounds: MapRect,
    /// The empty space the disk hangs in, and what the camera clears to.
    pub space: MapColor,
    /// The land the settlement stands on.
    pub ground: MapColor,
    /// The edge of the world: a closed outline, filled with `ground`.
    pub rim: Vec<[f32; 2]>,
    /// What hangs below the ground, which is what makes the world an object
    /// rather than a flat cut-out.
    pub underside: MapUnderside,
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
    /// Trees and rim posts.
    pub scenery: Vec<MapScenery>,
    /// Folder names painted flat onto the ground they belong to.
    pub ground_labels: Vec<MapGroundLabel>,
}

/// The rock under the ground: a sheer cliff, a frustum, and a spire.
///
/// The disk is drawn from its `rim` and these measurements rather than from a
/// polygon soup, for the reason every other part of the manifest is: the
/// builder says where things stand and how deep they go, and the renderer turns
/// that into triangles. All the depths are world units below the ground plane,
/// so they are read against the same `y = 0` everything else stands on.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct MapUnderside {
    /// How far the sheer band drops straight down from the rim before the rock
    /// starts pulling in. This is the part that reads as a cliff face.
    pub cliff: f32,
    /// How far below the ground the frustum under the cliff ends.
    pub shelf: f32,
    /// What fraction of the rim's radius the frustum has pulled in to by the
    /// time it reaches `shelf`. A smaller number is a steeper taper.
    pub taper: f32,
    /// How far below the ground the spire's tip hangs. The camera's fit is
    /// widened to hold this, so it is also how much of the view the underside
    /// is allowed to claim.
    pub depth: f32,
    /// The cliff band, which stands vertically and so still catches the sun.
    pub cliff_color: MapColor,
    /// The rock under the cliff. Drawn unlit -- see `rock_is_drawn_unlit` in
    /// `engine::spawn` for why nothing down there can be lit.
    pub rock: MapColor,
    /// The spire, darker still, so the underside reads as falling away.
    pub deep: MapColor,
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

/// The narrowest kerb drawn either side of a road.
///
/// A hairline drive still has to read as a path rather than as a scratch, so
/// the verge never disappears entirely however thin the paving gets.
pub const MIN_KERB: f32 = 0.45;

/// The widest kerb drawn either side of a road.
///
/// Every road on the map used to get exactly this much, and no road gets more
/// now — the ceiling is what guarantees the change to a proportional kerb
/// cannot widen a street that was already drawn.
pub const MAX_KERB: f32 = 1.6;

/// What share of its own paving a road's kerb is.
const KERB_SHARE: f32 = 0.35;

impl MapRoad {
    /// How wide the whole ribbon is drawn, paving plus the verge under it.
    ///
    /// **This, not [`width`](Self::width), is what the King actually sees**, and
    /// keeping the two apart is what this function exists to prevent forgetting.
    /// The verge used to be a flat `width + 1.6` for every road on the map, and
    /// adding a constant to both ends of a range compresses it — hardest at the
    /// quiet end, where the constant is most of the mark. Measured across a real
    /// dev folder, driveway paving spanned 6.8x from an unreferenced file to the
    /// busiest one while the ribbon drawn for it spanned only 3.9x, and the step
    /// from *nothing imports this* to *one file does* came out at 1.27x — a third
    /// of a world unit, at an isometric angle, which is no difference at all.
    ///
    /// A proportional verge keeps the ratio the width earned. The clamp is what
    /// keeps it honest in both directions: [`MIN_KERB`] so a hairline still has
    /// a verge, [`MAX_KERB`] so no road is drawn wider than it was before.
    pub fn ribbon_width(&self) -> f32 {
        self.width + (self.width * KERB_SHARE).clamp(MIN_KERB, MAX_KERB)
    }
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

    /// A location standing for one town.
    fn place(id: &str, label: &str, center: [f32; 2]) -> MapLocation {
        MapLocation {
            id: id.to_owned(),
            label: label.to_owned(),
            detail: String::new(),
            center,
            extent: [100.0, 100.0],
        }
    }

    /// A feature standing for one file.
    ///
    /// `folder` is derived from the path exactly as `manifest::feature_from_building`
    /// derives it, because `ward_at` reads it -- a helper that left it empty
    /// would be testing against a manifest the builder never produces.
    fn holding(repository: &str, path: &str, center: [f32; 2]) -> MapFeature {
        MapFeature {
            id: format!("{repository}/{path}"),
            name: path.rsplit('/').next().unwrap_or(path).to_owned(),
            path: path.to_owned(),
            repository: repository.to_owned(),
            folder: match path.rfind('/') {
                Some(at) => path[..at].to_owned(),
                None => String::new(),
            },
            breadcrumb: Vec::new(),
            building_kind: String::new(),
            meaning: String::new(),
            category: String::new(),
            bytes: 0,
            lines: 0,
            complexity: 0,
            references: 0,
            footprint: MapRect::default(),
            height: 1.0,
            center,
        }
    }

    fn manifest(locations: Vec<MapLocation>, features: Vec<MapFeature>) -> MapManifest {
        MapManifest {
            title: String::new(),
            subtitle: String::new(),
            world: MapWorld {
                bounds: MapRect::default(),
                space: [0, 0, 0, 255],
                ground: [0, 0, 0, 255],
                rim: Vec::new(),
                underside: MapUnderside {
                    cliff: 0.0,
                    shelf: 0.0,
                    taper: 0.0,
                    depth: 0.0,
                    cliff_color: [0, 0, 0, 255],
                    rock: [0, 0, 0, 255],
                    deep: [0, 0, 0, 255],
                },
                sun: MapSun {
                    direction: [0.0, -1.0, 0.0],
                    color: [255, 255, 255, 255],
                    illuminance: 1.0,
                    ambient: [255, 255, 255, 255],
                    ambient_brightness: 1.0,
                },
                towns: Vec::new(),
                wards: Vec::new(),
                plazas: Vec::new(),
                roads: Vec::new(),
                buildings: Vec::new(),
                scenery: Vec::new(),
                ground_labels: Vec::new(),
            },
            districts: Vec::new(),
            locations,
            features,
        }
    }

    /// The rail's map is framed on the city a conversation is about, and this
    /// is the lookup that finds it.
    #[test]
    fn a_town_is_found_by_the_project_directory_name() {
        let map = manifest(
            vec![
                place("town-0", "kingdom-ide", [10.0, 20.0]),
                place("town-1", "mommys-heart", [90.0, 80.0]),
            ],
            Vec::new(),
        );

        assert_eq!(
            map.town_named("mommys-heart").map(|town| town.center),
            Some([90.0, 80.0]),
        );
        // A folder the map never drew -- an empty project is left out of the
        // manifest entirely (`manifest_for`) -- must be an absence, not a
        // wrong town.
        assert!(map.town_named("not-a-city").is_none());
    }

    /// The trap `TownActivity` already documents, pinned on this side too.
    ///
    /// `scene::towns` numbers its settlements in packing order while
    /// `manifest::build_world_manifest` numbers its locations by file count, so
    /// `town-0` need not mean the same place in both halves of one manifest.
    /// Matching on the id would therefore frame the wrong city -- silently, and
    /// only on kingdoms whose two orderings happen to disagree. A future
    /// refactor to id-matching fails here rather than in front of the King.
    #[test]
    fn a_town_is_not_found_by_its_position_in_the_list() {
        // The orderings disagree: the largest project is listed first, and it
        // is not the one whose id says zero.
        let map = manifest(
            vec![
                place("town-3", "kingdom-ide", [10.0, 20.0]),
                place("town-0", "scratch", [90.0, 80.0]),
            ],
            Vec::new(),
        );

        assert_eq!(
            map.town_named("kingdom-ide").map(|town| town.center),
            Some([10.0, 20.0]),
            "the name has to win over the position in the list"
        );
    }

    /// Opening a file in the chamber points the rail's map at its building.
    ///
    /// Both halves of the identity are checked, and the test is worth having
    /// for the second: a kingdom of Rust projects has a `src/main.rs` in nearly
    /// every city, so matching on the path alone would point the map at
    /// whichever one happened to be built first.
    #[test]
    fn a_holding_needs_both_its_city_and_its_path() {
        let map = manifest(
            Vec::new(),
            vec![
                holding("kingdom-ide", "src/main.rs", [1.0, 2.0]),
                holding("scratch", "src/main.rs", [300.0, 400.0]),
            ],
        );

        assert_eq!(
            map.holding_at("scratch", "src/main.rs")
                .map(|feature| feature.center),
            Some([300.0, 400.0]),
        );
        // The right city, a file it does not have. Nothing, rather than the
        // other city's building.
        assert!(map.holding_at("scratch", "src/lib.rs").is_none());
    }

    fn test_ward(id: &str, path: &str, parent: Option<&str>) -> MapWard {
        MapWard {
            id: id.to_owned(),
            name: path.rsplit('/').next().unwrap_or(path).to_owned(),
            path: path.to_owned(),
            parent: parent.map(str::to_owned),
            files: 1,
            rect: MapRect {
                x: 0.0,
                y: 0.0,
                width: 50.0,
                depth: 50.0,
            },
            polygon: Vec::new(),
            depth: path.matches('/').count() as u32,
            ground: [0, 0, 0, 255],
            edge: [0, 0, 0, 255],
        }
    }

    fn test_building(feature_id: &str, ward_id: &str, lot: MapRect) -> MapBuilding {
        MapBuilding {
            feature_id: feature_id.to_owned(),
            ward_id: Some(ward_id.to_owned()),
            kind: BuildingKind::Guildhall,
            footprint: lot,
            lot,
            height: 10.0,
            palette: MapPalette::default(),
            complexity: 1,
            seed: 0,
        }
    }

    fn town(name: &str) -> MapTown {
        MapTown {
            id: format!("town-{name}"),
            name: name.to_owned(),
            rect: MapRect::default(),
            polygon: Vec::new(),
            ground: [0, 0, 0, 255],
            edge: [0, 0, 0, 255],
        }
    }

    /// A new file's folder is looked up by path, which is all the review
    /// summary gives us.
    #[test]
    fn a_folder_resolves_to_the_ward_drawn_for_it() {
        let mut map = manifest(Vec::new(), vec![holding("solo", "src/main.rs", [1.0, 1.0])]);
        map.world.wards = vec![test_ward("ward-0", "src", None)];
        map.world.towns = vec![town("solo")];
        // The building is what ties a ward to a repository -- see
        // `ward_belongs_to`. A feature with no building is a file the layout
        // did not draw.
        map.world.buildings = vec![test_building(
            "solo/src/main.rs",
            "ward-0",
            MapRect::default(),
        )];

        assert_eq!(
            map.ward_at("solo", "src").map(|w| w.id.as_str()),
            Some("ward-0")
        );
        // A folder the layout merged away is an ordinary absence, exactly as a
        // breadcrumb step with no `ward_id` is.
        assert!(map.ward_at("solo", "src/deep/nested").is_none());
        // And a city this map never drew must not be answered with another
        // city's ground -- the bug a single-town fast path introduced here.
        assert!(map.ward_at("nowhere", "src").is_none());
    }

    /// The trap `holding_at` documents, in the form it takes for folders -- and
    /// it bites harder here, because *every* Rust project has a `src`.
    #[test]
    fn two_towns_with_the_same_folder_name_are_told_apart() {
        let mut map = manifest(
            Vec::new(),
            vec![
                holding("alpha", "src/main.rs", [1.0, 1.0]),
                holding("beta", "src/main.rs", [9.0, 9.0]),
            ],
        );
        map.world.towns = vec![town("alpha"), town("beta")];
        // Ward ids carry the town prefix `layout::move_city` applies; the path
        // deliberately does not.
        map.world.wards = vec![
            test_ward("alpha/ward-0", "src", None),
            test_ward("beta/ward-0", "src", None),
        ];
        map.world.buildings = vec![
            test_building("alpha/src/main.rs", "alpha/ward-0", MapRect::default()),
            test_building("beta/src/main.rs", "beta/ward-0", MapRect::default()),
        ];

        assert_eq!(
            map.ward_at("beta", "src").map(|w| w.id.as_str()),
            Some("beta/ward-0"),
            "a new file must not be placed in another city's src"
        );
        assert_eq!(
            map.ward_at("alpha", "src").map(|w| w.id.as_str()),
            Some("alpha/ward-0")
        );
    }

    /// A folder holding nothing but sub-folders has no building of its own, so
    /// the ownership test has to look below it -- or a new file in `crates/`
    /// would have nowhere to stand.
    #[test]
    fn a_folder_of_folders_still_belongs_to_its_repository() {
        let mut map = manifest(
            Vec::new(),
            vec![holding("alpha", "crates/core/lib.rs", [1.0, 1.0])],
        );
        map.world.towns = vec![town("alpha"), town("beta")];
        map.world.wards = vec![
            test_ward("alpha/ward-0", "crates", None),
            test_ward("alpha/ward-1", "crates/core", Some("alpha/ward-0")),
        ];
        // The building sits in the *inner* ward, which is the only one it names.
        map.world.buildings = vec![test_building(
            "alpha/crates/core/lib.rs",
            "alpha/ward-1",
            MapRect::default(),
        )];

        assert_eq!(
            map.ward_at("alpha", "crates").map(|w| w.id.as_str()),
            Some("alpha/ward-0")
        );
    }

    /// The manifest arrives over the network, and a parent chain that loops
    /// must not hang the browser.
    ///
    /// [`MapManifest::ward_belongs_to`] searches a ward's descendants, so the
    /// links here are genuinely walked -- and the visited set is what makes the
    /// walk terminate. `engine::wards::lineage` pins the same guarantee on the
    /// same links walked upward.
    #[test]
    fn a_looping_folder_tree_cannot_hang_the_lookup() {
        let mut map = manifest(Vec::new(), Vec::new());
        map.world.towns = vec![town("alpha"), town("beta")];
        let mut a = test_ward("a", "knot", Some("b"));
        a.parent = Some("b".to_owned());
        let mut b = test_ward("b", "other", Some("a"));
        b.parent = Some("a".to_owned());
        map.world.wards = vec![a, b];

        // No buildings at all, so the answer is "not this repository's" -- the
        // point of the test is that it *returns*.
        assert!(map.ward_at("alpha", "knot").is_none());
    }

    /// The ground a placer has to keep off: the lots on a ward, and the wards
    /// nested inside it.
    #[test]
    fn a_ward_reports_the_ground_already_spoken_for() {
        let mut map = manifest(Vec::new(), Vec::new());
        let lot = MapRect {
            x: 4.0,
            y: 5.0,
            width: 6.0,
            depth: 7.0,
        };
        map.world.wards = vec![
            test_ward("ward-0", "src", None),
            test_ward("ward-1", "src/engine", Some("ward-0")),
            test_ward("ward-2", "docs", None),
        ];
        map.world.buildings = vec![
            test_building("f0", "ward-0", lot),
            test_building("f1", "ward-1", MapRect::default()),
        ];

        let lots: Vec<MapRect> = map.lots_in("ward-0").collect();
        assert_eq!(lots, vec![lot], "only this ward's own holdings");

        let inside: Vec<&str> = map.wards_inside("ward-0").map(|w| w.id.as_str()).collect();
        assert_eq!(inside, ["ward-1"], "and only its direct children");
    }

    /// The two questions the presence is asked, and the one that is not a
    /// synonym for the other.
    ///
    /// `Rail` is on screen *and* scoped; `Full` is on screen and not. Reading
    /// `showing()` where `in_rail()` was meant would scope the King's own map
    /// and take the camera off him.
    #[test]
    fn presence_separates_being_shown_from_being_in_the_rail() {
        assert!(!MapPresence::Hidden.showing());
        assert!(!MapPresence::Hidden.in_rail());

        assert!(MapPresence::Rail.showing());
        assert!(MapPresence::Rail.in_rail());

        assert!(MapPresence::Full.showing());
        assert!(
            !MapPresence::Full.in_rail(),
            "the King's own map must never be scoped out from under him"
        );
    }

    fn road(width: f32) -> MapRoad {
        MapRoad {
            kind: RoadKind::Drive,
            points: vec![[0.0, 0.0], [10.0, 0.0]],
            width,
            traffic: 1,
            color: [0, 0, 0, 255],
            edge: [0, 0, 0, 255],
        }
    }

    /// The verge is part of how wide a road *looks*, so it must not flatten the
    /// range the width earned. A flat kerb did exactly that: it is most of a
    /// hairline's mark and almost none of a trunk road's, so it compressed the
    /// quiet end into the busy one.
    #[test]
    fn a_kerb_never_swamps_the_paving_it_edges() {
        let hairline = road(1.0).ribbon_width();
        let busy = road(11.0).ribbon_width();
        let paving = 11.0 / 1.0;
        let drawn = busy / hairline;
        assert!(
            drawn > paving * 0.6,
            "paving spans {paving:.1}x but the ribbon drawn for it spans only \
             {drawn:.1}x, so most of what the width says is lost in the verge"
        );
    }

    /// Both ends of the clamp, pinned directly. The ceiling is the load-bearing
    /// one: every road used to get exactly `MAX_KERB`, so this is what promises
    /// a proportional verge cannot widen a road that was already drawn.
    #[test]
    fn a_kerb_stays_between_its_floor_and_the_width_roads_had_before() {
        for width in [0.0, 0.5, 1.0, 2.4, 5.0, 13.0, 34.0, 54.4] {
            // Recovered by subtraction, so it carries the rounding of a sum of
            // two f32s of very different sizes -- a 13.0 road comes back with a
            // 1.6000004 kerb. The tolerance is for that, not for slack in the
            // rule; anything actually out of range is out by far more.
            let kerb = road(width).ribbon_width() - width;
            assert!(
                (MIN_KERB - 1e-3..=MAX_KERB + 1e-3).contains(&kerb),
                "a {width} wide road was given a {kerb} kerb"
            );
        }
    }

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
