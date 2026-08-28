//! Drawing the wells, the host network, and who is plugged into what.
//!
//! The rendering half of [`crate::map::network`], which is where the geometry
//! and all the judgements behind it live. This module only turns rectangles and
//! colours into meshes -- it knows nothing about a plan, a city or a container,
//! exactly as [`super::works`] knows nothing about a changed file.
//!
//! # Everything here is unlit, except the well
//!
//! Every *mark* this draws is interface that happens to be in world space: a
//! colour that means "this agent", "this is your machine", "this is shared".
//! [`super::activity::WORKING_COLOR`] records the three attempts that
//! established why such a colour cannot be a lit material -- emissive scaled
//! for the sun's lux clips to white, a value near 1.0 is washed out by the
//! tonemapper, and a lit surface adds the sun's white specular on top, which
//! measured `(168, 231, 167)` for a green that was supposed to be `(34, 197,
//! 94)`. So the agent marks, the host ring, the moats and the channels are all
//! [`Surface::Unlit`] and their colours are exactly the colours asked for.
//!
//! **A wellhead is the exception, deliberately.** It is not a colour that means
//! something -- it is a *building*, standing on a town's square among lit
//! houses. Drawn unlit it was the only thing in the settlement with no shading
//! and no shadow, which is the definition of a light source, and it read as one:
//! a pale disc that looked switched on. So the well is [`Surface::Matte`] like
//! the houses around it, takes the sun, and casts the shadow that is most of
//! what makes a thing look like it is standing on the ground. Nothing is lost by
//! it, because no part of a well carries an identity that has to survive the
//! trip exactly.
//!
//! # What is animated: nothing
//!
//! For the reason the working ring no longer breathes. These are facts, not
//! events: an agent is on its own network for as long as it lives, and a
//! pulsing mark would draw the eye to something that is not changing. It also
//! means every material here can come from the shared
//! [`MaterialCache`](super::materials::MaterialCache), which quantises by
//! colour and hands one handle to many meshes -- something an animated material
//! could not do without pulsing whatever else landed in the same bucket.

use bevy::prelude::*;

use super::materials::{MaterialCache, Surface};
use super::meshes;
use super::spawn::{VisibleFrom, layer};
use crate::map::MapColor;
use crate::map::network::{NetworkPicture, WELL_TIMBER_COLOR, WELL_WATER_COLOR, Wellhead};

/// How tall an agent's marker stands, in world units.
///
/// Tall enough to read as a standing thing rather than a disc painted on the
/// ground, and well below a house so it never competes with the settlement's
/// own skyline.
pub(crate) const AGENT_HEIGHT: f32 = 16.0;

/// How tall a wellhead's drum stands, as a share of its own radius.
///
/// A proportion rather than the fixed height it once was, because the radius
/// now varies: a square with three wells on it shrinks each one
/// (`map::network::well_stand`), and a fixed height would turn those into
/// chimneys. Just under one radius reads as a low wall a person could lean on
/// rather than as a barrel.
const WELL_DRUM: f32 = 0.85;

/// How thick the stone rim around the mouth is, as a share of the radius.
///
/// The wall has to be visibly *thick* for the mark to read as masonry rather
/// than as a hoop, and this is what leaves a mouth two thirds the width of the
/// drum -- enough dark water to see at a glance.
const WELL_WALL: f32 = 0.3;

/// How far the water sits below the rim, as a share of the radius.
///
/// Deep enough that the shaft's inside face is visible from the map's fixed
/// isometric angle, which is what says "there is a hole here"; shallow enough
/// that the water is never hidden behind the near wall.
const WELL_DEPTH: f32 = 0.42;

/// How tall the canopy's posts stand, as a share of the radius.
const WELL_POST: f32 = 1.5;

/// How thick a canopy post is, as a share of the radius.
const WELL_POST_WIDTH: f32 = 0.14;

/// How high a well's name plaque is anchored, for a well of this radius.
///
/// A **function of the well** rather than the constant it replaced, and that is
/// forced rather than tidy: a wellhead is no longer one fixed size. Several
/// services on one square each shrink to a share of the paving
/// (`map::network::well_stand`), so the single `WELL_HEIGHT` a plaque used to
/// hang from would float over a small well and sink into a large one.
///
/// Anchored just over the canopy, which is the top of the object at the tier
/// the names are drawn at -- `labels.rs` only asks at the closest tier, which
/// is exactly where the canopy is shown.
pub(crate) fn well_label_height(radius: f32) -> f32 {
    radius * (WELL_POST + WELL_POST_WIDTH * 1.6)
}

/// How many segments a round mark is built from.
///
/// Twelve is enough that a marker reads as round at the zoom it is legible at,
/// and few enough that a kingdom with a dozen agents costs a trivial number of
/// triangles.
const SEGMENTS: usize = 12;

/// How much wider the moat is than the marker it rings.
const MOAT_SPREAD: f32 = 1.9;

/// How wide the moat's band is stroked.
const MOAT_WIDTH: f32 = 2.6;

/// What the interface last said about wells and networks.
///
/// Empty is the common answer -- no project declares a service and no plan is
/// open -- and the system below leans on that: a quiet map costs one resource
/// read per frame.
#[derive(Resource, Default, Debug, Clone, PartialEq)]
pub struct Network(pub NetworkPicture);

/// Everything raised for the current picture, so replacing it is one despawn.
///
/// Its own root rather than a child of `spawn::SceneRoot`, for the reason
/// [`super::works::WorksRoot`] is: this is replaced far more often than a world
/// is, and a separate root means swapping it never walks the settlement's
/// entities.
#[derive(Component)]
pub struct NetworkRoot;

/// Rebuilds the network picture whenever the interface replaces it.
///
/// Despawn-and-rebuild rather than diffing, exactly as `works::apply_works`
/// does. The picture is a few dozen small meshes and it changes every few
/// seconds at most, so the bookkeeping a diff would need costs more than the
/// rebuild it saves.
pub fn apply_network(
    mut commands: Commands,
    network: Res<Network>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut cache: ResMut<MaterialCache>,
    existing: Query<Entity, With<NetworkRoot>>,
) {
    if !network.is_changed() {
        return;
    }

    for entity in existing.iter() {
        commands.entity(entity).despawn();
    }
    if network.0.is_quiet() {
        return;
    }

    let root = commands
        .spawn((NetworkRoot, Transform::default(), Visibility::default()))
        .id();

    // The host band first, so it is under everything that crosses it.
    if let Some(ring) = network.0.host.as_ref() {
        let mut path = to_points(&ring.path);
        // A closed loop, so the band comes back to where it started.
        if let Some(first) = path.first().copied() {
            path.push(first);
        }
        spawn_ribbon(
            &mut commands,
            &mut meshes,
            &mut materials,
            &mut cache,
            root,
            &path,
            ring.width,
            ring.color,
            layer::HOST_RING,
            // Shown at every tier: at the furthest zoom it is the only thing
            // that says where the King's own machine is, which is exactly when
            // the rest of the map has stopped being legible.
            None,
        );
    }

    // Then the lines, which lie on the ground between the marks they join.
    for link in &network.0.links {
        let points = to_points(&link.points);
        if points.len() < 2 {
            continue;
        }
        spawn_ribbon(
            &mut commands,
            &mut meshes,
            &mut materials,
            &mut cache,
            root,
            &points,
            link.width,
            link.color,
            layer::NETWORK_LINK,
            // Hidden when pulled right back: a thicket of hairlines between
            // towns would read as noise at the tier where a house is twenty
            // pixels, and the wellheads and the host ring carry the summary.
            Some(super::bridge::LodLevel::Architecture),
        );
    }

    // The wellheads, standing on their squares.
    for well in &network.0.wells {
        spawn_wellhead(
            &mut commands,
            &mut meshes,
            &mut materials,
            &mut cache,
            root,
            well,
        );
    }

    // The agents, and the moat around any that has a network of its own.
    for agent in &network.0.agents {
        let mesh = round_mark(agent.center, agent.radius, AGENT_HEIGHT);
        commands.spawn((
            ChildOf(root),
            Mesh3d(meshes.add(mesh)),
            MeshMaterial3d(cache.get(&mut materials, agent.color, Surface::Unlit)),
            // Lifted onto the paving it stands on. The marker is built at
            // ground level and an agent now stands on a town's square, which is
            // itself raised to `layer::PLAZA` -- left at zero the disc would
            // z-fight with the stone under it, exactly as a wellhead would.
            Transform::from_xyz(0.0, layer::PLAZA, 0.0),
            VisibleFrom(super::bridge::LodLevel::Architecture),
            Pickable::IGNORE,
        ));

        if agent.isolated {
            // A closed ring around the marker and no conduit to the rim: the
            // picture of a plan that cannot reach the King's own loopback. See
            // `map::network` for why that is the fact worth drawing.
            let moat = ring_path(agent.center, agent.radius * MOAT_SPREAD);
            spawn_ribbon(
                &mut commands,
                &mut meshes,
                &mut materials,
                &mut cache,
                root,
                &moat,
                MOAT_WIDTH,
                agent.color,
                layer::NETWORK_LINK,
                Some(super::bridge::LodLevel::Architecture),
            );
        }
    }
}

/// One well: a stone drum with water in it, and a timber canopy over it.
///
/// Built rather than marked. A well is the only thing this module draws that is
/// a *thing in the town* rather than a fact about an agent, so it is made of the
/// same stuff the town is -- masonry, water and timber, all lit by the same sun
/// -- and the module doc says why that is worth the exception.
///
/// Four parts, and each earns its triangles:
///
/// - the **drum**, whose outer wall is the whole silhouette when the camera is
///   pulled back;
/// - the **rim**, an annulus rather than a disc, because a capped drum is a
///   plinth and the hole is the point;
/// - the **shaft and water**, recessed, which is what makes it read as a well
///   and not a barrel;
/// - the **canopy**, two posts and a beam, shown only from the `Architecture`
///   tier.
fn spawn_wellhead(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    cache: &mut MaterialCache,
    root: Entity,
    well: &Wellhead,
) {
    let center = Vec2::from_array(well.center);
    let radius = well.radius;
    let rim_y = radius * WELL_DRUM;
    let inner = (radius * (1.0 - WELL_WALL)).max(0.1);
    let water_y = rim_y - radius * WELL_DEPTH;

    let stone = cache.get(materials, well.color, Surface::Matte);
    let timber = cache.get(materials, WELL_TIMBER_COLOR, Surface::Matte);
    let water = cache.get(materials, WELL_WATER_COLOR, Surface::Matte);

    // The masonry: the outer wall, the flat rim, and the inside of the shaft.
    // One mesh, because all three are the same stone and a single draw is
    // cheaper than three.
    let mut drum = meshes::MeshBuilder::new();
    let outer_ring = circle(center, radius);
    let inner_ring = circle(center, inner);
    drum.wall_ring(&outer_ring, 0.0, rim_y);
    drum.annulus(center, inner, radius, rim_y, SEGMENTS);
    // Down to the water rather than to the ground: below the surface there is
    // nothing to see, and a wall drawn there would z-fight with the water.
    drum.inward_wall_ring(&inner_ring, water_y, rim_y);

    commands.spawn((
        ChildOf(root),
        Mesh3d(meshes.add(drum.build())),
        MeshMaterial3d(stone),
        Transform::from_xyz(0.0, layer::PLAZA, 0.0),
        // Kept at every tier, deliberately. "This project has a database five
        // agents share" is exactly the kind of fact worth seeing when the whole
        // realm is in frame.
        Pickable::IGNORE,
    ));

    let mut pool = meshes::MeshBuilder::new();
    pool.ground_polygon(&inner_ring, water_y);
    commands.spawn((
        ChildOf(root),
        Mesh3d(meshes.add(pool.build())),
        MeshMaterial3d(water),
        Transform::from_xyz(0.0, layer::PLAZA, 0.0),
        Pickable::IGNORE,
    ));

    // The canopy, from the tier where a house shows its architecture. At the
    // furthest tier it would be a few dark pixels floating over the drum, which
    // is noise -- the drum alone says a shared thing stands here, and that is
    // the whole of what is legible from that far out.
    let post = radius * WELL_POST_WIDTH;
    let top = radius * WELL_POST;
    let mut frame = meshes::MeshBuilder::new();
    for side in [-1.0f32, 1.0] {
        let x = center.x + side * (radius - post);
        frame.box_from_to(
            Vec3::new(x - post, 0.0, center.y - post),
            Vec3::new(x + post, top, center.y + post),
        );
    }
    frame.box_from_to(
        Vec3::new(center.x - radius, top, center.y - post),
        Vec3::new(center.x + radius, top + post * 1.6, center.y + post),
    );
    commands.spawn((
        ChildOf(root),
        Mesh3d(meshes.add(frame.build())),
        MeshMaterial3d(timber),
        Transform::from_xyz(0.0, layer::PLAZA, 0.0),
        VisibleFrom(super::bridge::LodLevel::Architecture),
        Pickable::IGNORE,
    ));
}

/// A closed ring of points around a centre, as ground coordinates.
fn circle(center: Vec2, radius: f32) -> Vec<Vec2> {
    (0..SEGMENTS)
        .map(|index| {
            let angle = index as f32 / SEGMENTS as f32 * std::f32::consts::TAU;
            center + Vec2::new(angle.cos(), angle.sin()) * radius
        })
        .collect()
}

/// A round mark standing on the ground.
fn round_mark(center: [f32; 2], radius: f32, height: f32) -> Mesh {
    let mut builder = meshes::MeshBuilder::new();
    builder.cylinder(Vec2::from_array(center), radius, 0.0, height, SEGMENTS);
    builder.build()
}

/// A closed ring of points around a centre.
fn ring_path(center: [f32; 2], radius: f32) -> Vec<Vec2> {
    let center = Vec2::from_array(center);
    let mut points: Vec<Vec2> = (0..SEGMENTS)
        .map(|index| {
            let angle = index as f32 / SEGMENTS as f32 * std::f32::consts::TAU;
            center + Vec2::new(angle.cos(), angle.sin()) * radius
        })
        .collect();
    if let Some(first) = points.first().copied() {
        points.push(first);
    }
    points
}

/// Lays a band along a path at a given height.
#[allow(clippy::too_many_arguments)]
fn spawn_ribbon(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    cache: &mut MaterialCache,
    root: Entity,
    points: &[Vec2],
    width: f32,
    color: MapColor,
    height: f32,
    visible_from: Option<super::bridge::LodLevel>,
) {
    if points.len() < 2 {
        return;
    }
    let mesh = meshes::ribbon(points, width);
    let mut entity = commands.spawn((
        ChildOf(root),
        Mesh3d(meshes.add(mesh)),
        MeshMaterial3d(cache.get(materials, color, Surface::Unlit)),
        Transform::from_xyz(0.0, height, 0.0),
        Pickable::IGNORE,
    ));
    if let Some(tier) = visible_from {
        entity.insert(VisibleFrom(tier));
    }
}

fn to_points(points: &[[f32; 2]]) -> Vec<Vec2> {
    points.iter().map(|p| Vec2::from_array(*p)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A ring must come back to where it started, or the moat has a gap in it
    /// -- and a moat with a gap says the opposite of what it is drawn to say.
    #[test]
    fn a_moat_is_a_closed_loop() {
        let path = ring_path([10.0, -4.0], 12.0);
        assert_eq!(
            path.first(),
            path.last(),
            "an unclosed moat leaves a doorway through the isolation"
        );
        assert!(
            path.len() > SEGMENTS,
            "the closing point is extra, not a swap"
        );
    }

    /// The moat stands clear of the marker it rings, or the two would read as
    /// one blob.
    #[test]
    fn a_moat_stands_clear_of_its_agent() {
        let radius = 9.0;
        let path = ring_path([0.0, 0.0], radius * MOAT_SPREAD);
        for point in &path {
            assert!(
                point.length() > radius + MOAT_WIDTH * 0.5,
                "the moat overlaps the marker it is supposed to ring"
            );
        }
    }
}
