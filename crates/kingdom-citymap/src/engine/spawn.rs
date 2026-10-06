//! Builds the Bevy world from a manifest.
//!
//! Every holding used to arrive as a list of pre-projected polygons. It now
//! arrives as a footprint, a height, and an archetype, and the geometry is
//! generated here — once per distinct shape, then shared.

use crate::map::{MapBuilding, MapManifest, MapScenery, MapWard, MapWorld};
use bevy::light::{CascadeShadowConfigBuilder, NotShadowCaster, NotShadowReceiver};
use bevy::platform::collections::HashMap;
use bevy::prelude::*;
use core::ops::Range;

use super::activity;
use super::bridge::{Bridge, LodLevel};
use super::camera;
use super::materials::{self, MaterialCache, MaterialKey, Surface, to_color};
use super::meshes::{self, BuildingShape};
use super::raise::Step;
use super::text;
use super::wards;

/// Everything spawned from a manifest carries this, so loading a new world is
/// a single despawn pass.
#[derive(Component)]
pub struct SceneRoot;

/// Line weight of a top-level folder's boundary, in world units.
const WARD_EDGE_WIDTH: f32 = 2.6;

/// Line weight of a working town's ring at the tiers where its own detail is
/// legible, in world units.
///
/// Much heavier than the heaviest ward kerb, which is what keeps it from being
/// read as one more folder boundary. Measured on screen -- 4.2 was a hairline
/// indistinguishable from a kerb.
///
/// This is the *close* weight. Pulled back to the [`LodLevel::Districts`] tier
/// it collapses to a couple of pixels, which is what
/// [`BOLD_TOWN_RING_WIDTH`] exists to answer.
const TOWN_RING_WIDTH: f32 = 9.0;

/// The lane the realm's packing leaves between neighbouring settlements.
///
/// `build::layout::pack_towns`'s own `TOWN_GAP`, restated rather than imported:
/// `build` compiles only for `ssr` and `engine` only for `hydrate`, so the two
/// genuinely cannot see each other -- see the crate docs on that split. It is
/// here so the ring widths below can be checked against it.
const TOWN_GAP: f32 = 24.0;

/// Line weight of a working town's ring at the [`LodLevel::Districts`] tier.
///
/// A width in world units is not a width on screen. The camera is fixed at the
/// isometric angle, so a ground span is foreshortened by 0.8165 whichever way
/// it runs (`camera::screen_axes`), and the tiers are cut by how many pixels a
/// ~30-unit holding covers (`LodLevel::for_holding_pixels`). Putting those
/// together, [`TOWN_RING_WIDTH`] comes out around 20 px when the map is pointed
/// at one file, ~7 px at the Districts boundary, and **~2.4 px** at the
/// zoom-out limit -- a hairline, at exactly the tier where every building has
/// lost its trim and the ring is the only thing left that can say an agent is
/// working here.
///
/// So this is what a working town is traced with once the camera is that far
/// back: ~5.9 px at the limit and ~18 px at the boundary, a band rather than a
/// line.
///
/// # Why it stops here
///
/// [`meshes::ribbon`] centres a band on its path, so half of this -- 11 units
/// -- hangs *outside* the town it traces, into [`TOWN_GAP`]. Two working
/// neighbours each spend half of that lane and keep a seam between them instead
/// of merging into one green mass. That is the ceiling on this number, and the
/// `const` assertion below is what holds it.
pub const BOLD_TOWN_RING_WIDTH: f32 = 22.0;

// Checked when this compiles rather than when the suite runs, the same way
// `works::SCAFFOLD_FLOOR`'s roofline clearance is: both of these are arithmetic
// on literals rather than facts about behaviour, so a `#[test]` asserting them
// could only ever fail on a build that had already been made. Widening a ring
// into its neighbour's half of the lane is now a compile error.
const _: () = assert!(
    BOLD_TOWN_RING_WIDTH * 0.5 < TOWN_GAP * 0.5,
    "a bold ring this wide meets its neighbour's across the lane between two towns"
);
const _: () = assert!(
    BOLD_TOWN_RING_WIDTH > TOWN_RING_WIDTH,
    "the bold ring is no heavier than the fine one, so the pair says nothing"
);

/// Each weight a town is traced at, and the zoom that weight is for.
///
/// A table rather than two spawn calls so the pair can be read -- and checked
/// for covering every tier exactly once -- without standing up a Bevy `World`.
const TOWN_RINGS: [(f32, activity::RingTier); 2] = [
    (TOWN_RING_WIDTH, activity::RingTier::Fine),
    (BOLD_TOWN_RING_WIDTH, activity::RingTier::Bold),
];

/// A holding the pointer can interact with.
#[derive(Component, Clone)]
pub struct Holding {
    /// The manifest feature this building was built from.
    pub feature_id: String,
    /// Where a label for this holding should sit, in world space.
    pub label_anchor: Vec3,
}

/// The lowest zoom tier at which an entity is drawn.
#[derive(Component, Clone, Copy, PartialEq, Eq)]
pub struct VisibleFrom(pub LodLevel);

/// The loaded manifest, kept for lookups by the interface bridge.
#[derive(Resource, Default)]
pub struct LoadedMap(pub Option<Box<MapManifest>>);

/// Mesh handles shared across the world, and geometry still being merged.
///
/// Two jobs, held together because both are per-world caches cleared by the
/// same [`clear_world`] pass. The handles are what lets five thousand files
/// share a few thousand building meshes; [`Merge`] is what lets fifty thousand
/// roads and trees become a few hundred *entities*.
#[derive(Resource, Default)]
pub struct MeshCache {
    buildings: HashMap<BuildingShape, BuildingHandles>,
    /// Roads, accumulated across the slices of [`Step::Roads`].
    roads: Merge,
    /// Trees and posts, accumulated across the slices of [`Step::Groves`].
    groves: Merge,
}

/// Static geometry being gathered into one mesh per material.
///
/// # Why this exists
///
/// A road used to be an entity with a mesh of its own. Measured against a real
/// dev folder -- fifteen projects -- that was 13,547 roads and 11,898 trees, and
/// because every road's ribbon was a fresh `Assets<Mesh>` entry there was
/// nothing for Bevy's batcher to merge them with. The frame came out at
/// **62,178 draw calls and 1.8 seconds**, of which 120,086 out of 124,356 drew
/// exactly *one* instance.
///
/// Trees were the same shape of problem arriving the other way round: they do
/// share three meshes, but opaque geometry is depth-sorted before it is
/// batched, so eleven thousand of them scattered across the map interleave with
/// everything else and collapse to one instance apiece regardless.
///
/// So the geometry is merged here instead of being left to the renderer. The
/// grouping key is [`materials::material_key`] -- deliberately the cache's own
/// key rather than a second one -- because a merged mesh carries a single
/// material, and two surfaces may only share a mesh if they would have shared
/// that. The result is the same triangles at the same heights with the same
/// materials, in a few hundred entities rather than fifty thousand.
///
/// # Why it is a buffer rather than a function
///
/// A world is raised in slices so the loading bar can move between them (see
/// [`super::raise`]), and a merged stage therefore cannot emit at the end of
/// one call: the roads arrive forty-eight at a time across many frames. This
/// accumulates across those slices and [`Merge::flush`] empties it when the
/// stage's last slice lands, which leaves the slicing, the bar and the
/// `Step::weight` pacing exactly as they were.
#[derive(Default)]
struct Merge {
    groups: HashMap<MaterialKey, MergeGroup>,
}

/// One material's worth of merged geometry, and what to paint it with.
struct MergeGroup {
    builder: meshes::MeshBuilder,
    /// The colour this group was opened with. Every colour in the group falls
    /// in the same bucket, so any of them names the material; the first is
    /// kept because it is the one that is certainly present.
    color: crate::map::MapColor,
    surface: Surface,
}

impl Merge {
    /// The builder for a colour and surface, opened on first use.
    ///
    /// Heights are **not** part of the key: a merged mesh cannot be moved by a
    /// `Transform` afterwards, so every caller bakes its own `layer::` height
    /// into the vertices it adds. That is what lets a road and its kerb -- two
    /// hundredths of a unit apart, and the same colour only by accident -- land
    /// in one mesh when they do share one.
    fn group(&mut self, color: crate::map::MapColor, surface: Surface) -> &mut meshes::MeshBuilder {
        &mut self
            .groups
            .entry(materials::material_key(color, surface))
            .or_insert_with(|| MergeGroup {
                builder: meshes::MeshBuilder::new(),
                color,
                surface,
            })
            .builder
    }

    /// Spawns one entity per group and empties the buffer.
    ///
    /// `decorate` adds whatever components the stage needs beyond a mesh and a
    /// material -- `Pickable::IGNORE` for every caller here, plus a
    /// [`VisibleFrom`] for the ones that drop out with zoom.
    fn flush(
        &mut self,
        commands: &mut Commands,
        meshes: &mut Assets<Mesh>,
        materials: &mut Assets<StandardMaterial>,
        cache: &mut MaterialCache,
        root: Entity,
        decorate: impl Fn(&mut bevy::ecs::system::EntityCommands),
    ) {
        for (_, group) in self.groups.drain() {
            // An empty group is a colour that was opened and never drawn into,
            // which a degenerate road or a zero-radius tree can do.
            if group.builder.is_empty() {
                continue;
            }
            let material = cache.get(materials, group.color, group.surface);
            let mut entity = commands.spawn((
                ChildOf(root),
                Mesh3d(meshes.add(group.builder.build())),
                MeshMaterial3d(material),
                // The geometry carries its own world position, so the entity
                // sits at the origin rather than being placed.
                Transform::default(),
            ));
            decorate(&mut entity);
        }
    }

    /// How many entities a flush would spawn. The figure the merge exists to
    /// keep small.
    #[cfg(test)]
    fn len(&self) -> usize {
        self.groups
            .values()
            .filter(|group| !group.builder.is_empty())
            .count()
    }

    fn clear(&mut self) {
        self.groups.clear();
    }
}

#[derive(Clone)]
pub(super) struct BuildingHandles {
    pub(super) walls: Handle<Mesh>,
    pub(super) roof: Handle<Mesh>,
    pub(super) details: Option<Handle<Mesh>>,
}

impl MeshCache {
    /// The meshes for a holding's shape, built on first use and shared after.
    ///
    /// `pub(super)` rather than private: `works` builds a ghost house for a file
    /// that does not exist yet, and it must come out of the same cache and the
    /// same generator as a real one -- a new file should look like a *building*,
    /// which is the whole point of drawing it on a map of buildings.
    pub(super) fn building(
        &mut self,
        meshes: &mut Assets<Mesh>,
        shape: BuildingShape,
    ) -> BuildingHandles {
        self.buildings
            .entry(shape)
            .or_insert_with(|| {
                let built = meshes::build_building(shape);
                BuildingHandles {
                    walls: meshes.add(built.walls),
                    roof: meshes.add(built.roof),
                    details: built.details.map(|mesh| meshes.add(mesh)),
                }
            })
            .clone()
    }

    /// The number of distinct building meshes in play, which is the number the
    /// sharing scheme is meant to keep small.
    pub fn building_shapes(&self) -> usize {
        self.buildings.len()
    }

    /// Drops every cached mesh and any half-merged geometry, for when a new
    /// world is loaded.
    pub fn clear(&mut self) {
        self.buildings.clear();
        // A world abandoned mid-raise leaves partly-filled buffers behind, and
        // carrying them into the next one would draw the old kingdom's roads
        // across the new one's ground.
        self.roads.clear();
        self.groves.clear();
    }
}

/// Vertical offsets that keep coplanar surfaces from fighting for the same
/// depth. The camera looks down at a shallow angle, so a few hundredths of a
/// world unit is enough to settle the order without being visible.
pub mod layer {
    /// The rim's own ground. Everything else stacks on it.
    pub const LAND: f32 = 0.0;
    /// A town's ground, over the land it stands on.
    pub const TOWN: f32 = 0.02;
    /// A folder's ground, over the town it belongs to.
    pub const WARD: f32 = 0.06;
    /// The paved square at a settlement's heart.
    pub const PLAZA: f32 = 0.12;
    /// Roads and paths, over every ground surface they cross.
    pub const ROAD: f32 = 0.16;
    /// Folder names sit above every ground surface, including the roads that
    /// cross their ward, so a name is never half-swallowed by a path.
    /// A folder's outline sits above every other ground surface but below the
    /// names, so a kerb still reads where a road runs along a ward's boundary.
    pub const WARD_EDGE: f32 = 0.19;
    /// A working town's ring sits above every kerb inside it, so the fact that
    /// an agent is here is never half-hidden by the folder tree it is working
    /// in. Below the ground labels, which are what a name is for.
    pub const TOWN_GLOW: f32 = 0.205;
    /// The band standing for the King's own machine, at the realm's rim.
    ///
    /// Above the working ring so a conduit crossing a town's edge is not
    /// swallowed by it, and below the ground labels for [`GROUND_LABEL`]'s
    /// reason: a folder's name must never be half-covered by a line drawn over
    /// it.
    pub const HOST_RING: f32 = 0.207;
    /// Conduits, channels and moats -- the lines joining agents to what they
    /// reach.
    ///
    /// Just above the host band they run to, so a conduit reads as arriving at
    /// the ring rather than disappearing under it.
    pub const NETWORK_LINK: f32 = 0.209;
    /// A folder's name, painted flat on its ground and above everything that
    /// crosses it -- a name half-swallowed by a road is worse than no name.
    pub const GROUND_LABEL: f32 = 0.22;
}

/// Removes the previous world, if any.
pub fn clear_world(
    commands: &mut Commands,
    existing: &Query<Entity, With<SceneRoot>>,
    mesh_cache: &mut MeshCache,
    material_cache: &mut MaterialCache,
) {
    for entity in existing.iter() {
        commands.entity(entity).despawn();
    }
    mesh_cache.clear();
    material_cache.clear();
}

/// Spawns the empty root a world is built under.
///
/// Taken apart from [`spawn_world`] because a world is normally raised over
/// several frames -- see [`super::raise`] -- and the root has to exist, hidden,
/// before the first slice of it does.
pub fn spawn_root(commands: &mut Commands, visibility: Visibility) -> Entity {
    commands
        .spawn((SceneRoot, Transform::default(), visibility))
        .id()
}

/// Shows a world that was raised under a hidden root.
pub fn reveal(commands: &mut Commands, root: Entity) {
    commands.entity(root).insert(Visibility::Inherited);
}

/// Builds one slice of one step of a world.
///
/// The single door the sliced path goes through, so that "what does this step
/// build" is answered in one place rather than once here and once in
/// [`spawn_world`].
#[allow(clippy::too_many_arguments)]
pub fn spawn_step(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    mesh_cache: &mut MeshCache,
    material_cache: &mut MaterialCache,
    root: Entity,
    world: &MapWorld,
    step: Step,
    slice: Range<usize>,
) {
    match step {
        Step::Base => spawn_base(commands, meshes, materials, material_cache, root, world),
        Step::Towns => spawn_towns(
            commands,
            meshes,
            materials,
            material_cache,
            root,
            world,
            slice,
        ),
        Step::Wards => spawn_wards(
            commands,
            meshes,
            materials,
            material_cache,
            root,
            world,
            slice,
        ),
        Step::Plazas => spawn_plazas(
            commands,
            meshes,
            materials,
            material_cache,
            root,
            world,
            slice,
        ),
        Step::Roads => spawn_roads(
            commands,
            meshes,
            materials,
            mesh_cache,
            material_cache,
            root,
            world,
            slice,
        ),
        Step::Holdings => spawn_buildings(
            commands,
            meshes,
            materials,
            mesh_cache,
            material_cache,
            root,
            world,
            slice,
        ),
        Step::Groves => spawn_scenery(
            commands,
            meshes,
            materials,
            mesh_cache,
            material_cache,
            root,
            world,
            slice,
        ),
        Step::Names => spawn_ground_labels(
            commands,
            meshes,
            materials,
            material_cache,
            root,
            world,
            slice,
        ),
    }
}

/// Spawns a manifest's world in one go. Returns the root entity.
///
/// The engine does not take this path -- it raises a world in slices so the
/// browser can paint a loading bar between them. It is kept because it is the
/// definition of "the whole world", and because every stage below is reached
/// through exactly the same functions the sliced path uses, so the two cannot
/// build different settlements.
pub fn spawn_world(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    mesh_cache: &mut MeshCache,
    material_cache: &mut MaterialCache,
    world: &MapWorld,
) -> Entity {
    let root = spawn_root(commands, Visibility::default());

    spawn_base(commands, meshes, materials, material_cache, root, world);
    spawn_towns(
        commands,
        meshes,
        materials,
        material_cache,
        root,
        world,
        0..world.towns.len(),
    );
    spawn_wards(
        commands,
        meshes,
        materials,
        material_cache,
        root,
        world,
        0..world.wards.len(),
    );
    spawn_plazas(
        commands,
        meshes,
        materials,
        material_cache,
        root,
        world,
        0..world.plazas.len(),
    );
    spawn_roads(
        commands,
        meshes,
        materials,
        mesh_cache,
        material_cache,
        root,
        world,
        0..world.roads.len(),
    );
    spawn_buildings(
        commands,
        meshes,
        materials,
        mesh_cache,
        material_cache,
        root,
        world,
        0..world.buildings.len(),
    );
    spawn_scenery(
        commands,
        meshes,
        materials,
        mesh_cache,
        material_cache,
        root,
        world,
        0..world.scenery.len(),
    );
    spawn_ground_labels(
        commands,
        meshes,
        materials,
        material_cache,
        root,
        world,
        0..world.ground_labels.len(),
    );

    root
}

/// Paints each folder's name onto its own ground.
///
/// The generator reserved the space and chose the size; the only thing left
/// here is turning the name into strokes and laying it flat. Text is condensed
/// into the width the generator reserved rather than trusted to fit, because
/// the generator has no glyph metrics to work from — that keeps a long folder
/// name inside its ward without the two halves needing to agree on a font.
fn spawn_ground_labels(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    cache: &mut MaterialCache,
    root: Entity,
    world: &MapWorld,
    slice: Range<usize>,
) {
    for label in world.ground_labels.get(slice).unwrap_or_default() {
        if label.text.trim().is_empty() || label.size <= 0.0 {
            continue;
        }
        let natural = text::text_width(&label.text) * label.size;
        if natural <= 0.0 {
            continue;
        }
        let condense = if label.max_width > 0.0 {
            (label.max_width / natural).min(1.0)
        } else {
            1.0
        };

        let mesh = text::text_mesh(&label.text, label.size, label.stroke);
        // The mesh is built running along +x with its caps rising toward -z.
        // A vertical label is the same mesh turned a quarter turn about the
        // vertical axis, which is why only the rotation differs.
        let rotation = if label.vertical {
            Quat::from_rotation_y(-std::f32::consts::FRAC_PI_2)
        } else {
            Quat::IDENTITY
        };
        let cap_direction = if label.vertical {
            Vec2::new(1.0, 0.0)
        } else {
            Vec2::new(0.0, -1.0)
        };

        commands.spawn((
            ChildOf(root),
            Mesh3d(meshes.add(mesh)),
            MeshMaterial3d(cache.get(materials, label.color, Surface::Matte)),
            Transform {
                translation: Vec3::new(
                    label.origin[0],
                    layer::GROUND_LABEL + label.depth as f32 * 0.015,
                    label.origin[1],
                ),
                rotation,
                scale: Vec3::new(condense, 1.0, 1.0),
            },
            wards::WardLabel {
                size: label.size,
                cap_direction,
                min_pixel_height: label.min_pixel_height,
            },
            // A name is painted on the ground, so it must never intercept a
            // click meant for the ward underneath it.
            Pickable::IGNORE,
            Visibility::Hidden,
        ));
    }
}

fn spawn_sun(commands: &mut Commands, root: Entity, world: &MapWorld) {
    let direction = Vec3::from_array(world.sun.direction).normalize_or(Vec3::NEG_Y);
    let span = world.bounds.width.max(world.bounds.depth);
    let distance = camera::distance_for(span);
    commands.spawn((
        ChildOf(root),
        DirectionalLight {
            color: to_color(world.sun.color),
            illuminance: world.sun.illuminance,
            shadow_maps_enabled: true,
            ..default()
        },
        // A directional light only casts shadows inside its cascades, and
        // WebGL2 allows exactly one of them. A single cascade spanning the
        // whole town is also all this scene needs: the camera is orthographic,
        // so everything visible sits at much the same depth. The default
        // cascade distances are measured from the camera's near plane and
        // would stop short of the ground long before reaching it.
        CascadeShadowConfigBuilder {
            num_cascades: 1,
            minimum_distance: (distance - span).max(1.0),
            maximum_distance: distance + span,
            first_cascade_far_bound: distance + span,
            overlap_proportion: 0.0,
        }
        .build(),
        Transform::from_translation(-direction * span).looking_to(direction, Vec3::Y),
    ));
}

/// The disk itself: the sun above it and the rock under it.
///
/// Its own stage rather than part of the town loop because it is a fixed,
/// small number of meshes -- there is nothing here to count, so nothing to
/// slice.
fn spawn_base(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    cache: &mut MaterialCache,
    root: Entity,
    world: &MapWorld,
) {
    spawn_sun(commands, root, world);

    // The world is a disk hanging in space: the ground it stands on, and the
    // rock below holding it up. There is nothing beyond the rim -- no plane
    // running to the horizon -- so the silhouette of the disk is the edge of
    // everything there is.
    let rim = to_points(&world.rim);
    if rim.len() < 3 {
        return;
    }

    commands.spawn((
        ChildOf(root),
        Mesh3d(meshes.add(meshes::ground_polygon(&rim))),
        MeshMaterial3d(cache.get(materials, world.ground, Surface::Matte)),
        Transform::from_xyz(0.0, layer::LAND, 0.0),
        Pickable::IGNORE,
    ));

    // The cliff, the shelf and the spire are three meshes so each takes
    // its own colour, and all three are drawn **unlit**.
    //
    // That is not a shortcut. The sun points almost straight down, so no
    // surface under the disk receives any of it, and the scene is exposed
    // for a 9,000-lux sun against a 420-lux ambient fill -- which renders
    // the whole underside as a black silhouette whatever colour it is
    // given. Lighting it properly would mean a second light aimed up at
    // the rock, and that light would also fall on the town. Unlit rock,
    // shaded by hand from the manifest's three colours, keeps the sun
    // calibrated for the kingdom above and still reads as depth.
    for (mesh, color) in [
        (
            meshes::rim_cliff(&rim, world.underside.cliff),
            world.underside.cliff_color,
        ),
        (
            meshes::disk_shelf(&rim, &world.underside),
            world.underside.rock,
        ),
        (
            meshes::disk_spire(&rim, &world.underside),
            world.underside.deep,
        ),
    ] {
        commands.spawn((
            ChildOf(root),
            Mesh3d(meshes.add(mesh)),
            MeshMaterial3d(cache.get(materials, color, Surface::Unlit)),
            Transform::from_xyz(0.0, layer::LAND, 0.0),
            // The spire would otherwise cast a long shadow into empty
            // space, at the cost of the one cascade WebGL2 allows.
            NotShadowCaster,
            NotShadowReceiver,
            Pickable::IGNORE,
        ));
    }
}

/// One town's ground, and the ring that lights up while an agent works there.
///
/// `slice` is which towns to build, so a world can go up over several frames.
/// Every stage below takes one for the same reason, and an out-of-range slice
/// yields nothing rather than panicking: the range is arithmetic done
/// elsewhere, and a loading bar is not worth a crash.
fn spawn_towns(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    cache: &mut MaterialCache,
    root: Entity,
    world: &MapWorld,
    slice: Range<usize>,
) {
    for town in world.towns.get(slice).unwrap_or_default() {
        if town.polygon.len() < 3 {
            continue;
        }
        let mesh = meshes::ground_polygon(&to_points(&town.polygon));
        commands.spawn((
            ChildOf(root),
            Mesh3d(meshes.add(mesh)),
            MeshMaterial3d(cache.get(materials, town.ground, Surface::Matte)),
            Transform::from_xyz(0.0, layer::TOWN, 0.0),
            Pickable::IGNORE,
        ));

        spawn_town_ring(commands, meshes, materials, cache, root, town);
    }
}

/// Each folder's ground, and the kerb around it.
fn spawn_wards(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    cache: &mut MaterialCache,
    root: Entity,
    world: &MapWorld,
    slice: Range<usize>,
) {
    for ward in world.wards.get(slice).unwrap_or_default() {
        if ward.polygon.len() < 3 {
            continue;
        }
        let mesh = meshes::ground_polygon(&to_points(&ward.polygon));
        // Nested wards stack, so each level of nesting gets its own sliver of
        // height rather than fighting its parent for the same plane.
        let height = layer::WARD + ward.depth as f32 * 0.015;
        commands
            .spawn((
                ChildOf(root),
                Mesh3d(meshes.add(mesh)),
                MeshMaterial3d(cache.get(materials, ward.ground, Surface::Matte)),
                Transform::from_xyz(0.0, height, 0.0),
                wards::WardGround {
                    id: ward.id.clone(),
                    depth: ward.depth,
                },
            ))
            .observe(wards::on_ward_hover)
            .observe(wards::on_ward_unhover);

        spawn_ward_outline(commands, meshes, materials, cache, root, ward);
    }
}

/// The paved squares at the heart of each settlement.
fn spawn_plazas(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    cache: &mut MaterialCache,
    root: Entity,
    world: &MapWorld,
    slice: Range<usize>,
) {
    for plaza in world.plazas.get(slice).unwrap_or_default() {
        let rect = plaza.rect;
        let mesh = meshes::ground_polygon(&[
            Vec2::new(rect.x, rect.y),
            Vec2::new(rect.max_x(), rect.y),
            Vec2::new(rect.max_x(), rect.max_y()),
            Vec2::new(rect.x, rect.max_y()),
        ]);
        commands.spawn((
            ChildOf(root),
            Mesh3d(meshes.add(mesh)),
            MeshMaterial3d(cache.get(materials, plaza.color, Surface::Matte)),
            Transform::from_xyz(0.0, layer::PLAZA, 0.0),
            Pickable::IGNORE,
        ));
    }
}

/// Traces a town with the rings that light up while an agent works there.
///
/// Spawned hidden with the world and never rebuilt: activity changes every few
/// seconds and a respawn per change would rebuild geometry for a fact that is
/// only a visibility flag. [`activity::apply_activity`] is what shows them.
///
/// **Two** rings, one per [`activity::RingTier`], because a fixed width in world
/// units is a different width on screen at every zoom -- see
/// [`BOLD_TOWN_RING_WIDTH`] for the measurements.
///
/// One departure from the ward kerb this is otherwise modelled on, and it is
/// deliberate: there is **no** [`VisibleFrom`], so `apply_lod` leaves both rings
/// alone. Which of them shows is decided by [`activity::shows`] against the same
/// tier, so that a ring is hidden for want of *activity* rather than for want of
/// zoom, and the town's answer to "who is working here" survives at every zoom
/// rather than only at the tier one of them was drawn to be legible from.
///
/// The material comes from the shared cache, like every other unlit surface
/// here. It could not while the ring breathed: the cache quantises by colour and
/// hands one handle to hundreds of meshes, so writing to it each frame would
/// have pulsed whatever else landed in the same bucket. Nothing writes to it now.
fn spawn_town_ring(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    cache: &mut MaterialCache,
    root: Entity,
    town: &crate::map::MapTown,
) {
    let mut points = to_points(&town.polygon);
    // A ring is a loop, so the ribbon has to come back to where it started.
    points.push(points[0]);

    // Unlit: this is interface drawn in world space, not a surface in the
    // scene, and its colour is the whole of its meaning. Lit, it took the
    // sun's white specular and came out mint -- see `activity::WORKING_COLOR`.
    let material = cache.get(materials, activity::WORKING_COLOR, Surface::Unlit);

    for (width, tier) in TOWN_RINGS {
        commands.spawn((
            ChildOf(root),
            Mesh3d(meshes.add(meshes::ribbon(&points, width))),
            MeshMaterial3d(material.clone()),
            Transform::from_xyz(0.0, layer::TOWN_GLOW, 0.0),
            activity::TownRing {
                town: town.name.clone(),
                tier,
            },
            // Nothing is running when a world loads, and a ring shown around a
            // quiet town would be a lie for as long as it took the first poll
            // to land.
            Visibility::Hidden,
            // A ring is drawn over its own town; a click on it is meant for
            // whatever it is drawn around.
            Pickable::IGNORE,
        ));
    }
}

/// Draws a folder's boundary as a kerb around its ground.
///
/// The outline is what a ward highlight moves, rather than the ground or the
/// buildings: flooding a whole ward with colour drowns out the wards nested
/// inside it, while a lit boundary reads as a border at every depth and leaves
/// the holdings looking like themselves. It is always drawn, so the folder tree
/// is legible before anything is hovered at all.
fn spawn_ward_outline(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    cache: &mut MaterialCache,
    root: Entity,
    ward: &MapWard,
) {
    let mut points = to_points(&ward.polygon);
    // A boundary is a loop, so the ribbon has to come back to where it started.
    points.push(points[0]);
    // A top-level folder gets the heaviest line, and each level inside it a
    // finer one, so nesting is legible from the weight of the border alone.
    let width = (WARD_EDGE_WIDTH - ward.depth as f32 * 0.55).max(0.9);
    let base = cache.get(materials, ward.edge, Surface::Matte);
    let highlight = cache.get(materials, wards::lift(ward.edge, 0.62), Surface::Matte);

    commands.spawn((
        ChildOf(root),
        Mesh3d(meshes.add(meshes::ribbon(&points, width))),
        MeshMaterial3d(base.clone()),
        Transform::from_xyz(0.0, layer::WARD_EDGE + ward.depth as f32 * 0.004, 0.0),
        wards::InWard(ward.id.clone()),
        wards::Tint { base, highlight },
        // The kerb is a hairline drawn over its own ward; a click on it is
        // meant for the ground underneath.
        Pickable::IGNORE,
    ));
}

/// Every road and path, merged into one mesh per colour.
///
/// The ribbons are accumulated rather than spawned (see [`Merge`]), and the
/// last slice of the stage is what empties the buffer -- `slice.end` reaching
/// the end of the list is the only signal available here that the stage is
/// done, and it is exact because `RaisePlan` walks each list once, in order.
#[allow(clippy::too_many_arguments)]
fn spawn_roads(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    mesh_cache: &mut MeshCache,
    cache: &mut MaterialCache,
    root: Entity,
    world: &MapWorld,
    slice: Range<usize>,
) {
    let last = slice.end >= world.roads.len();

    for road in world.roads.get(slice).unwrap_or_default() {
        if road.points.len() < 2 {
            continue;
        }
        let points = to_points(&road.points);
        // The edge is a slightly wider ribbon slipped underneath, which reads
        // as a kerb without needing a second material pass. Its width is
        // `MapRoad`'s own business rather than a constant here: the verge is
        // part of how wide a road *looks*, so it belongs beside the width it is
        // measured from. See `MapRoad::ribbon_width`.
        mesh_cache.roads.group(road.edge, Surface::Matte).ribbon(
            &points,
            road.ribbon_width(),
            layer::ROAD,
        );

        mesh_cache.roads.group(road.color, Surface::Matte).ribbon(
            &points,
            road.width,
            layer::ROAD + 0.02,
        );
    }

    if last {
        mesh_cache
            .roads
            .flush(commands, meshes, materials, cache, root, |entity| {
                entity.insert(Pickable::IGNORE);
            });
    }
}

// Eight, because a stage takes the slice of itself to build on top of the
// caches and the world it reads. See `spawn_step`.
#[allow(clippy::too_many_arguments)]
fn spawn_buildings(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    mesh_cache: &mut MeshCache,
    material_cache: &mut MaterialCache,
    root: Entity,
    world: &MapWorld,
    slice: Range<usize>,
) {
    for building in world.buildings.get(slice).unwrap_or_default() {
        spawn_building(
            commands,
            meshes,
            materials,
            mesh_cache,
            material_cache,
            root,
            building,
        );
    }
}

fn spawn_building(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    mesh_cache: &mut MeshCache,
    material_cache: &mut MaterialCache,
    root: Entity,
    building: &MapBuilding,
) {
    let footprint = Vec2::new(building.footprint.width, building.footprint.depth);
    if footprint.x <= 0.0 || footprint.y <= 0.0 {
        return;
    }
    let shape = BuildingShape::new(
        building.kind,
        footprint,
        building.height,
        building.complexity,
    );
    let handles = mesh_cache.building(meshes, shape);

    // Archetypes are modelled inside a unit footprint with height expressed as
    // a multiple of the shorter side, so placing one is a scale and a move.
    let plan = footprint.x.min(footprint.y);
    let center = building.footprint.center();
    let scale = Vec3::new(footprint.x, plan, footprint.y);
    let top = shape.height() * plan;

    let entity = commands
        .spawn((
            ChildOf(root),
            Holding {
                feature_id: building.feature_id.clone(),
                label_anchor: Vec3::new(center[0], top, center[1]),
            },
            Transform::from_xyz(center[0], 0.0, center[1]).with_scale(scale),
            Visibility::default(),
        ))
        // Picking events travel up the hierarchy, so observing the holding
        // catches hits on its walls, roof, and trim alike.
        .observe(on_hover)
        .observe(on_unhover)
        .observe(on_click)
        .id();

    let trim_material = material_cache.get(materials, building.palette.trim, Surface::Polished);

    commands.spawn((
        ChildOf(entity),
        Mesh3d(handles.walls),
        MeshMaterial3d(material_cache.get(materials, building.palette.wall, Surface::Matte)),
        VisibleFrom(LodLevel::Districts),
    ));
    commands.spawn((
        ChildOf(entity),
        Mesh3d(handles.roof),
        MeshMaterial3d(material_cache.get(materials, building.palette.roof, Surface::Matte)),
        VisibleFrom(LodLevel::Districts),
    ));
    if let Some(details) = handles.details {
        commands.spawn((
            ChildOf(entity),
            Mesh3d(details),
            MeshMaterial3d(trim_material),
            VisibleFrom(LodLevel::Architecture),
            // Trim is decoration; the holding underneath is the pick target.
            Pickable::IGNORE,
        ));
    }
}

/// Trees and rim posts, merged into one mesh per colour.
///
/// Scenery already shared three meshes and was still the single largest source
/// of draw calls, because sharing a mesh is not enough: opaque geometry is
/// depth-sorted before it is batched, and eleven thousand trees scattered
/// across a kingdom interleave with everything else. So each one's geometry is
/// generated at its own position and size and folded into its colour's mesh.
///
/// The shapes are the same ones [`meshes::tree_foliage`] and
/// [`meshes::tree_trunk`] build at unit size: those were modelled at one unit
/// and scaled into place by a `Transform`, and the calls below are that scale
/// applied to the generator's arguments instead. Two cones for a crown, a
/// six-sided cylinder for a trunk.
#[allow(clippy::too_many_arguments)]
fn spawn_scenery(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    mesh_cache: &mut MeshCache,
    material_cache: &mut MaterialCache,
    root: Entity,
    world: &MapWorld,
    slice: Range<usize>,
) {
    let last = slice.end >= world.scenery.len();

    for item in world.scenery.get(slice).unwrap_or_default() {
        match *item {
            MapScenery::Tree {
                position,
                height,
                radius,
                foliage: foliage_color,
                trunk: trunk_color,
            } => {
                let at = Vec2::new(position[0], position[1]);

                // Two stacked cones read as a fuller crown than one --
                // `meshes::tree_foliage`'s own comment, and its own proportions.
                let crown = mesh_cache.groves.group(foliage_color, Surface::Matte);
                crown.cone(at, radius, height * 0.32, height, 9);
                crown.cone(at, radius * 0.74, height * 0.58, height * 1.16, 9);

                mesh_cache
                    .groves
                    .group(trunk_color, Surface::Matte)
                    .cylinder(at, radius * 0.16, 0.0, height * 0.42, 6);
            }
            MapScenery::Post {
                position,
                height,
                color,
            } => {
                mesh_cache.groves.group(color, Surface::Matte).cylinder(
                    Vec2::new(position[0], position[1]),
                    0.5,
                    0.0,
                    height,
                    6,
                );
            }
        }
    }

    if last {
        mesh_cache.groves.flush(
            commands,
            meshes,
            materials,
            material_cache,
            root,
            |entity| {
                entity.insert((VisibleFrom(LodLevel::Architecture), Pickable::IGNORE));
            },
        );
    }
}

fn to_points(points: &[[f32; 2]]) -> Vec<Vec2> {
    points
        .iter()
        .map(|point| Vec2::from_array(*point))
        .collect()
}

fn on_hover(event: On<Pointer<Over>>, holdings: Query<&Holding>, bridge: Res<Bridge>) {
    if let Ok(holding) = holdings.get(event.entity) {
        let id = holding.feature_id.clone();
        bridge.update_status(|status| status.hovered = Some(id));
    }
}

/// Publishes the holding that was clicked.
///
/// The engine is the only thing that knows what is under the pointer at the
/// instant of a press, so it is the only thing that can answer this without a
/// race -- see [`crate::engine::bridge::ViewerStatus::clicked`] for the one it
/// replaces. The serial makes a second click on the same holding a second
/// event rather than an unchanged status nobody hears about.
fn on_click(event: On<Pointer<Click>>, holdings: Query<&Holding>, bridge: Res<Bridge>) {
    if let Ok(holding) = holdings.get(event.entity) {
        let id = holding.feature_id.clone();
        bridge.update_status(|status| {
            let serial = status.clicked.as_ref().map_or(0, |(_, n)| n + 1);
            status.clicked = Some((id, serial));
        });
    }
}

fn on_unhover(event: On<Pointer<Out>>, holdings: Query<&Holding>, bridge: Res<Bridge>) {
    if let Ok(holding) = holdings.get(event.entity) {
        bridge.update_status(|status| {
            if status.hovered.as_deref() == Some(holding.feature_id.as_str()) {
                status.hovered = None;
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::map::{BuildingKind, MapPalette, MapRect};

    fn building(kind: BuildingKind, width: f32, depth: f32, height: f32) -> MapBuilding {
        MapBuilding {
            feature_id: "file-0".to_owned(),
            ward_id: Some("ward-0".to_owned()),
            kind,
            footprint: MapRect {
                x: 0.0,
                y: 0.0,
                width,
                depth,
            },
            lot: MapRect {
                x: 0.0,
                y: 0.0,
                width,
                depth,
            },
            height,
            palette: MapPalette::default(),
            complexity: 4,
            seed: 7,
        }
    }

    #[test]
    fn buildings_of_a_similar_shape_share_one_mesh() {
        let mut meshes = Assets::default();
        let mut cache = MeshCache::default();

        for index in 0..64 {
            let jitter = index as f32 * 0.001;
            let source = building(BuildingKind::Cottage, 4.0 + jitter, 3.0, 6.0);
            let footprint = Vec2::new(source.footprint.width, source.footprint.depth);
            let shape =
                BuildingShape::new(source.kind, footprint, source.height, source.complexity);
            cache.building(&mut meshes, shape);
        }

        assert_eq!(
            cache.building_shapes(),
            1,
            "near-identical holdings should share a mesh"
        );
    }

    #[test]
    fn different_archetypes_do_not_share_meshes() {
        let mut meshes = Assets::default();
        let mut cache = MeshCache::default();
        for kind in BuildingKind::ALL {
            let shape = BuildingShape::new(kind, Vec2::new(4.0, 3.0), 6.0, 4);
            cache.building(&mut meshes, shape);
        }
        assert_eq!(cache.building_shapes(), BuildingKind::ALL.len());
    }

    #[test]
    fn clearing_the_cache_releases_every_shape() {
        let mut meshes = Assets::default();
        let mut cache = MeshCache::default();
        cache.building(
            &mut meshes,
            BuildingShape::new(BuildingKind::Keep, Vec2::new(6.0, 6.0), 12.0, 9),
        );
        assert_eq!(cache.building_shapes(), 1);
        cache.clear();
        assert_eq!(cache.building_shapes(), 0);
    }

    /// Counts the triangles a builder holds, by reading back the mesh it
    /// produces. The merge must not lose or invent geometry -- only entities.
    fn triangle_count(mesh: &Mesh) -> usize {
        mesh.indices().map(|indices| indices.len() / 3).unwrap_or(0)
    }

    /// The whole point of `Merge`. Thirteen thousand roads in three colours
    /// must come out as three entities, not thirteen thousand.
    #[test]
    fn roads_sharing_a_colour_are_merged_into_one_mesh() {
        let mut merge = Merge::default();
        let colors = [[120, 90, 60, 255], [40, 40, 40, 255], [200, 200, 180, 255]];

        for index in 0..13_000usize {
            let color = colors[index % colors.len()];
            let at = index as f32;
            merge.group(color, Surface::Matte).ribbon(
                &[Vec2::new(at, 0.0), Vec2::new(at, 10.0)],
                3.0,
                layer::ROAD,
            );
        }

        assert_eq!(
            merge.len(),
            colors.len(),
            "thirteen thousand roads should draw as one mesh per colour"
        );
    }

    /// Colours a single step apart already share a *material*, so they must
    /// also share a mesh: a merged mesh carries one material, and grouping more
    /// finely than the material cache does would spawn entities that could have
    /// been one. This is why `Merge` keys on `materials::material_key`.
    #[test]
    fn near_identical_colours_merge_together() {
        let mut merge = Merge::default();
        let line = [Vec2::new(0.0, 0.0), Vec2::new(1.0, 0.0)];
        merge
            .group([120, 90, 60, 255], Surface::Matte)
            .ribbon(&line, 1.0, 0.0);
        merge
            .group([122, 91, 61, 255], Surface::Matte)
            .ribbon(&line, 1.0, 0.0);
        assert_eq!(merge.len(), 1);
    }

    /// The same colour on a different surface is a different material, so it
    /// cannot share a mesh. The mirror of the test above, and the bound on it.
    #[test]
    fn a_different_surface_is_not_merged_in() {
        let mut merge = Merge::default();
        let line = [Vec2::new(0.0, 0.0), Vec2::new(1.0, 0.0)];
        merge
            .group([120, 90, 60, 255], Surface::Matte)
            .ribbon(&line, 1.0, 0.0);
        merge
            .group([120, 90, 60, 255], Surface::Unlit)
            .ribbon(&line, 1.0, 0.0);
        assert_eq!(merge.len(), 2);
    }

    /// Merging must be geometry-neutral: the triangles of N roads gathered into
    /// one mesh are the triangles those N roads had separately. This is what
    /// stops the optimisation quietly becoming a visual change.
    #[test]
    fn merging_keeps_every_triangle() {
        let roads: Vec<[Vec2; 2]> = (0..50)
            .map(|index| {
                let at = index as f32;
                [Vec2::new(at, 0.0), Vec2::new(at + 1.0, 4.0)]
            })
            .collect();

        let separate: usize = roads
            .iter()
            .map(|points| triangle_count(&meshes::ribbon(points, 2.0)))
            .sum();

        let mut merge = Merge::default();
        for points in &roads {
            merge
                .group([10, 20, 30, 255], Surface::Matte)
                .ribbon(points, 2.0, 0.0);
        }
        let merged: usize = merge
            .groups
            .values()
            .map(|group| triangle_count(&group.builder.clone_mesh()))
            .sum();

        assert_eq!(merged, separate, "the merge lost or invented triangles");
    }

    /// A road's ribbon has to land on its own `layer::` height, because a
    /// merged mesh cannot be lifted by a `Transform` afterwards. If this
    /// regressed, every road would sink to the ground plane and z-fight with
    /// the wards under it.
    #[test]
    fn a_merged_ribbon_sits_at_the_height_it_was_given() {
        let mut merge = Merge::default();
        merge.group([10, 20, 30, 255], Surface::Matte).ribbon(
            &[Vec2::new(0.0, 0.0), Vec2::new(4.0, 0.0)],
            2.0,
            layer::ROAD,
        );
        let mesh = merge.groups.values().next().unwrap().builder.clone_mesh();
        let positions = mesh
            .attribute(Mesh::ATTRIBUTE_POSITION)
            .and_then(|values| values.as_float3())
            .expect("positions");
        assert!(
            positions.iter().all(|point| point[1] == layer::ROAD),
            "a merged ribbon did not keep its layer"
        );
    }

    /// Abandoning a world mid-raise must not leave its roads in the buffer, or
    /// they would be drawn across the next kingdom's ground.
    #[test]
    fn clearing_the_cache_discards_half_merged_geometry() {
        let mut cache = MeshCache::default();
        cache.roads.group([1, 2, 3, 255], Surface::Matte).ribbon(
            &[Vec2::new(0.0, 0.0), Vec2::new(1.0, 0.0)],
            1.0,
            0.0,
        );
        assert_eq!(cache.roads.len(), 1);
        cache.clear();
        assert_eq!(cache.roads.len(), 0, "a stale road survived a new world");
    }

    /// The merge bakes each tree's position and size into its vertices instead
    /// of scaling a unit mesh by a `Transform`. Those two must produce the
    /// *same* triangles, or the optimisation is a visual change wearing a
    /// performance change's clothes.
    ///
    /// Checked against the unit generators themselves, with the transform
    /// applied by hand, so this fails if either side drifts.
    #[test]
    fn a_merged_tree_is_the_same_tree_the_transform_drew() {
        let (radius, height) = (3.5f32, 9.0f32);
        let at = Vec2::new(120.0, -40.0);

        // The old path: a unit mesh, scaled and moved into place.
        let scale = Vec3::new(radius, height, radius);
        let placed = |mesh: &Mesh| -> Vec<Vec3> {
            let positions = mesh
                .attribute(Mesh::ATTRIBUTE_POSITION)
                .and_then(|values| values.as_float3())
                .expect("positions");
            positions
                .iter()
                .map(|point| Vec3::from_array(*point) * scale + Vec3::new(at.x, 0.0, at.y))
                .collect()
        };
        let was_crown = placed(&meshes::tree_foliage(1.0, 1.0));

        // The new path: generated at its true size and position.
        let mut builder = meshes::MeshBuilder::new();
        builder.cone(at, radius, height * 0.32, height, 9);
        builder.cone(at, radius * 0.74, height * 0.58, height * 1.16, 9);
        let now_crown = placed_identity(&builder.clone_mesh());

        assert_eq!(
            was_crown.len(),
            now_crown.len(),
            "a merged crown has a different vertex count"
        );
        for (was, now) in was_crown.iter().zip(now_crown.iter()) {
            assert!(
                was.distance(*now) < 1e-3,
                "a crown vertex moved: {was:?} became {now:?}"
            );
        }
    }

    /// The trunk half of the check above.
    #[test]
    fn a_merged_trunk_is_the_same_trunk_the_transform_drew() {
        let (radius, height) = (3.5f32, 9.0f32);
        let at = Vec2::new(-12.0, 70.0);

        let scale = Vec3::new(radius * 0.16, height * 0.42, radius * 0.16);
        let unit = meshes::tree_trunk(1.0, 1.0);
        let positions = unit
            .attribute(Mesh::ATTRIBUTE_POSITION)
            .and_then(|values| values.as_float3())
            .expect("positions");
        let was: Vec<Vec3> = positions
            .iter()
            .map(|point| Vec3::from_array(*point) * scale + Vec3::new(at.x, 0.0, at.y))
            .collect();

        let mut builder = meshes::MeshBuilder::new();
        builder.cylinder(at, radius * 0.16, 0.0, height * 0.42, 6);
        let now = placed_identity(&builder.clone_mesh());

        assert_eq!(
            was.len(),
            now.len(),
            "a merged trunk has a different vertex count"
        );
        for (was, now) in was.iter().zip(now.iter()) {
            assert!(
                was.distance(*now) < 1e-3,
                "a trunk vertex moved: {was:?} became {now:?}"
            );
        }
    }

    /// Reads a mesh's vertices as they stand, with no transform applied.
    fn placed_identity(mesh: &Mesh) -> Vec<Vec3> {
        mesh.attribute(Mesh::ATTRIBUTE_POSITION)
            .and_then(|values| values.as_float3())
            .expect("positions")
            .iter()
            .map(|point| Vec3::from_array(*point))
            .collect()
    }

    /// Every zoom has to have a ring drawn for it, and only one. The pair is
    /// what `activity::shows` chooses between, and a table missing a weight --
    /// or carrying the same one twice -- would leave a working town bare at one
    /// zoom or double-banded at another.
    #[test]
    fn a_town_is_ringed_once_for_every_tier() {
        for lod in [
            LodLevel::Districts,
            LodLevel::Architecture,
            LodLevel::FileDetail,
        ] {
            let drawn: Vec<f32> = TOWN_RINGS
                .into_iter()
                .filter(|(_, tier)| activity::shows(*tier, lod))
                .map(|(width, _)| width)
                .collect();
            assert_eq!(drawn.len(), 1, "{lod:?} is traced {} times", drawn.len());
            assert!(
                drawn[0] > WARD_EDGE_WIDTH,
                "{lod:?} is traced no heavier than a ward kerb"
            );
        }
    }
}
