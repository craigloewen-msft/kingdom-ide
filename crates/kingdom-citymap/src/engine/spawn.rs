//! Builds the Bevy world from a manifest.
//!
//! Every holding used to arrive as a list of pre-projected polygons. It now
//! arrives as a footprint, a height, and an archetype, and the geometry is
//! generated here — once per distinct shape, then shared.

use bevy::light::{CascadeShadowConfigBuilder, NotShadowCaster, NotShadowReceiver};
use bevy::platform::collections::HashMap;
use bevy::prelude::*;
use crate::map::{MapBuilding, MapManifest, MapScenery, MapWard, MapWorld};

use super::activity;
use super::bridge::{Bridge, LodLevel};
use super::camera;
use super::materials::{MaterialCache, Surface, to_color};
use super::meshes::{self, BuildingShape};
use super::text;
use super::wards;

/// Everything spawned from a manifest carries this, so loading a new world is
/// a single despawn pass.
#[derive(Component)]
pub struct SceneRoot;

/// Line weight of a top-level folder's boundary, in world units.
const WARD_EDGE_WIDTH: f32 = 2.6;

/// Line weight of a working town's ring, in world units.
///
/// Much heavier than the heaviest ward kerb, and that is the point: at the
/// Districts tier a whole town is a couple of hundred pixels across and every
/// folder boundary inside it has collapsed into noise. Measured on screen at
/// the fitted view -- 4.2 was a hairline indistinguishable from a ward kerb.
const TOWN_RING_WIDTH: f32 = 9.0;

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

/// Mesh handles shared across the world.
#[derive(Resource, Default)]
pub struct MeshCache {
    buildings: HashMap<BuildingShape, BuildingHandles>,
    foliage: Option<Handle<Mesh>>,
    trunk: Option<Handle<Mesh>>,
    post: Option<Handle<Mesh>>,
}

#[derive(Clone)]
struct BuildingHandles {
    walls: Handle<Mesh>,
    roof: Handle<Mesh>,
    details: Option<Handle<Mesh>>,
}

impl MeshCache {
    fn building(&mut self, meshes: &mut Assets<Mesh>, shape: BuildingShape) -> BuildingHandles {
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

    /// Drops every cached mesh, for when a new world is loaded.
    pub fn clear(&mut self) {
        self.buildings.clear();
        self.foliage = None;
        self.trunk = None;
        self.post = None;
    }
}

/// Vertical offsets that keep coplanar surfaces from fighting for the same
/// depth. The camera looks down at a shallow angle, so a few hundredths of a
/// world unit is enough to settle the order without being visible.
mod layer {
    /// The rim's own ground. Everything else stacks on it.
    pub const LAND: f32 = 0.0;
    pub const TOWN: f32 = 0.02;
    pub const WARD: f32 = 0.06;
    pub const PLAZA: f32 = 0.12;
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

/// Spawns a manifest's world. Returns the root entity.
pub fn spawn_world(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    mesh_cache: &mut MeshCache,
    material_cache: &mut MaterialCache,
    world: &MapWorld,
) -> Entity {
    let root = commands
        .spawn((SceneRoot, Transform::default(), Visibility::default()))
        .id();

    spawn_sun(commands, root, world);
    spawn_terrain(commands, meshes, materials, material_cache, root, world);
    spawn_roads(commands, meshes, materials, material_cache, root, world);
    spawn_buildings(
        commands,
        meshes,
        materials,
        mesh_cache,
        material_cache,
        root,
        world,
    );
    spawn_scenery(
        commands,
        meshes,
        materials,
        mesh_cache,
        material_cache,
        root,
        world,
    );
    spawn_ground_labels(commands, meshes, materials, material_cache, root, world);

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
) {
    for label in &world.ground_labels {
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

fn spawn_terrain(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    cache: &mut MaterialCache,
    root: Entity,
    world: &MapWorld,
) {
    // The world is a disk hanging in space: the ground it stands on, and the
    // rock below holding it up. There is nothing beyond the rim -- no plane
    // running to the horizon -- so the silhouette of the disk is the edge of
    // everything there is.
    let rim = to_points(&world.rim);
    if rim.len() >= 3 {
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

    for town in &world.towns {
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

        spawn_town_ring(commands, meshes, materials, root, town);
    }

    for ward in &world.wards {
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

    for plaza in &world.plazas {
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

/// Traces a town with the ring that lights up while an agent works there.
///
/// Spawned hidden with the world and never rebuilt: activity changes every few
/// seconds and a respawn per change would rebuild geometry for a fact that is
/// only a visibility flag. [`activity::apply_activity`] is what shows it.
///
/// Two departures from the ward kerb this is otherwise modelled on, both
/// deliberate. The material is **its own** rather than the shared cache's,
/// because the pulse writes to it and the cache hands one handle to every mesh
/// of a similar colour. And there is **no** [`VisibleFrom`], so `apply_lod`
/// leaves it alone: the ring answers "who is working here" at every zoom, not
/// only at the tier it was drawn to be legible from.
fn spawn_town_ring(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    root: Entity,
    town: &crate::map::MapTown,
) {
    let mut points = to_points(&town.polygon);
    // A ring is a loop, so the ribbon has to come back to where it started.
    points.push(points[0]);

    let material = materials.add(StandardMaterial {
        base_color: activity::ring_color(1.0),
        // Unlit: this is interface drawn in world space, not a surface in the
        // scene, and its colour is the whole of its meaning. Lit, it took the
        // sun's white specular and came out mint -- see `activity::PULSE_PEAK`.
        unlit: true,
        ..default()
    });

    commands.spawn((
        ChildOf(root),
        Mesh3d(meshes.add(meshes::ribbon(&points, TOWN_RING_WIDTH))),
        MeshMaterial3d(material.clone()),
        Transform::from_xyz(0.0, layer::TOWN_GLOW, 0.0),
        activity::TownRing {
            town: town.name.clone(),
            material,
        },
        // Nothing is running when a world loads, and a ring shown around a
        // quiet town would be a lie for as long as it took the first poll to
        // land.
        Visibility::Hidden,
        // The ring is a hairline drawn over its own town; a click on it is meant
        // for whatever it is drawn around.
        Pickable::IGNORE,
    ));
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

fn spawn_roads(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    cache: &mut MaterialCache,
    root: Entity,
    world: &MapWorld,
) {
    for road in &world.roads {
        if road.points.len() < 2 {
            continue;
        }
        let points = to_points(&road.points);
        // The edge is a slightly wider ribbon slipped underneath, which reads
        // as a kerb without needing a second material pass.
        let edge = meshes::ribbon(&points, road.width + 1.6);
        commands.spawn((
            ChildOf(root),
            Mesh3d(meshes.add(edge)),
            MeshMaterial3d(cache.get(materials, road.edge, Surface::Matte)),
            Transform::from_xyz(0.0, layer::ROAD, 0.0),
            Pickable::IGNORE,
        ));

        let surface = meshes::ribbon(&points, road.width);
        commands.spawn((
            ChildOf(root),
            Mesh3d(meshes.add(surface)),
            MeshMaterial3d(cache.get(materials, road.color, Surface::Matte)),
            Transform::from_xyz(0.0, layer::ROAD + 0.02, 0.0),
            Pickable::IGNORE,
        ));
    }
}

fn spawn_buildings(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    mesh_cache: &mut MeshCache,
    material_cache: &mut MaterialCache,
    root: Entity,
    world: &MapWorld,
) {
    for building in &world.buildings {
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

fn spawn_scenery(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    mesh_cache: &mut MeshCache,
    material_cache: &mut MaterialCache,
    root: Entity,
    world: &MapWorld,
) {
    // Scenery is modelled once at unit size and scaled into place.
    let foliage = mesh_cache
        .foliage
        .get_or_insert_with(|| meshes.add(meshes::tree_foliage(1.0, 1.0)))
        .clone();
    let trunk = mesh_cache
        .trunk
        .get_or_insert_with(|| meshes.add(meshes::tree_trunk(1.0, 1.0)))
        .clone();
    let post = mesh_cache
        .post
        .get_or_insert_with(|| meshes.add(meshes::tree_trunk(1.0, 1.0)))
        .clone();

    for item in &world.scenery {
        match *item {
            MapScenery::Tree {
                position,
                height,
                radius,
                foliage: foliage_color,
                trunk: trunk_color,
            } => {
                // Foliage and trunk hang off the world root directly rather
                // than off a shared parent transform. An island now grows with
                // the codebase, so a realm carries tens of thousands of trees,
                // and the parent bought nothing but a third entity and an extra
                // level for transform propagation to walk.
                commands.spawn((
                    ChildOf(root),
                    Mesh3d(foliage.clone()),
                    MeshMaterial3d(material_cache.get(materials, foliage_color, Surface::Matte)),
                    Transform::from_xyz(position[0], 0.0, position[1])
                        .with_scale(Vec3::new(radius, height, radius)),
                    VisibleFrom(LodLevel::Architecture),
                    Pickable::IGNORE,
                ));
                commands.spawn((
                    ChildOf(root),
                    Mesh3d(trunk.clone()),
                    MeshMaterial3d(material_cache.get(materials, trunk_color, Surface::Matte)),
                    Transform::from_xyz(position[0], 0.0, position[1]).with_scale(Vec3::new(
                        radius * 0.16,
                        height * 0.42,
                        radius * 0.16,
                    )),
                    VisibleFrom(LodLevel::Architecture),
                    Pickable::IGNORE,
                ));
            }
            MapScenery::Post {
                position,
                height,
                color,
            } => {
                commands.spawn((
                    ChildOf(root),
                    Mesh3d(post.clone()),
                    MeshMaterial3d(material_cache.get(materials, color, Surface::Matte)),
                    Transform::from_xyz(position[0], 0.0, position[1])
                        .with_scale(Vec3::new(0.5, height, 0.5)),
                    VisibleFrom(LodLevel::Architecture),
                    Pickable::IGNORE,
                ));
            }
        }
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
}
