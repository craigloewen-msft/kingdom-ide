//! Drawing the wells, the host network, and who is plugged into what.
//!
//! The rendering half of [`crate::map::network`], which is where the geometry
//! and all the judgements behind it live. This module only turns rectangles and
//! colours into meshes -- it knows nothing about a plan, a city or a container,
//! exactly as [`super::works`] knows nothing about a changed file.
//!
//! # Everything here is unlit
//!
//! Every mark this draws is *interface that happens to be in world space*: a
//! colour that means "this agent", "this is your machine", "this is shared".
//! [`super::activity::WORKING_COLOR`] records the three attempts that
//! established why such a colour cannot be a lit material -- emissive scaled
//! for the sun's lux clips to white, a value near 1.0 is washed out by the
//! tonemapper, and a lit surface adds the sun's white specular on top, which
//! measured `(168, 231, 167)` for a green that was supposed to be `(34, 197,
//! 94)`. So all of this is [`Surface::Unlit`] and its colours are exactly the
//! colours asked for.
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
use crate::map::network::NetworkPicture;

/// How tall an agent's marker stands, in world units.
///
/// Tall enough to read as a standing thing rather than a disc painted on the
/// ground, and well below a house so it never competes with the settlement's
/// own skyline.
const AGENT_HEIGHT: f32 = 16.0;

/// How tall a wellhead stands.
///
/// Lower than an agent and wider (see `map::network::WELL_RADIUS`): a well is a
/// thing in the ground that agents gather at, so it reads as a basin rather
/// than as another figure standing about.
const WELL_HEIGHT: f32 = 9.0;

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
        let mesh = round_mark(well.center, well.radius, WELL_HEIGHT);
        commands.spawn((
            ChildOf(root),
            Mesh3d(meshes.add(mesh)),
            MeshMaterial3d(cache.get(&mut materials, well.color, Surface::Unlit)),
            Transform::default(),
            // Kept at every tier, deliberately. "This project has a database
            // five agents share" is exactly the kind of fact worth seeing when
            // the whole realm is in frame.
            Pickable::IGNORE,
        ));
    }

    // The agents, and the moat around any that has a network of its own.
    for agent in &network.0.agents {
        let mesh = round_mark(agent.center, agent.radius, AGENT_HEIGHT);
        commands.spawn((
            ChildOf(root),
            Mesh3d(meshes.add(mesh)),
            MeshMaterial3d(cache.get(&mut materials, agent.color, Surface::Unlit)),
            Transform::default(),
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
