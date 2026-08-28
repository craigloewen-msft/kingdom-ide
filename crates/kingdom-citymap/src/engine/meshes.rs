//! Procedural geometry for every building archetype.
//!
//! The old renderer drew each archetype as a stack of hand-ordered 2D polygons
//! with hand-darkened faces. Here an archetype is real geometry: the engine
//! projects it, sorts it by depth, and lights it.
//!
//! Meshes are built in a unit-ish local space — one unit wide, one unit deep,
//! and `height` tall, standing on the ground plane at `y = 0` — so a single
//! mesh can be shared by every holding with the same shape and stretched to fit
//! its lot. Bevy's coordinate space is right-handed with `y` up, so the world's
//! ground `y` becomes `z` here.

use crate::map::{BuildingKind, MapUnderside};
use bevy::asset::RenderAssetUsages;
use bevy::mesh::{Indices, PrimitiveTopology};
use bevy::prelude::*;

/// A mesh under construction.
///
/// Faces are pushed as flat-shaded quads and triangles, each with its own
/// vertices, so every face gets a true normal instead of an averaged one. Sharp
/// creases are what make a roofline read as a roofline.
#[derive(Default)]
pub struct MeshBuilder {
    positions: Vec<[f32; 3]>,
    normals: Vec<[f32; 3]>,
    uvs: Vec<[f32; 2]>,
    indices: Vec<u32>,
}

impl MeshBuilder {
    /// An empty builder, holding no geometry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a triangle with a flat normal derived from its winding.
    pub fn triangle(&mut self, a: Vec3, b: Vec3, c: Vec3) {
        let normal = (b - a).cross(c - a).normalize_or_zero();
        let base = self.positions.len() as u32;
        for (vertex, uv) in [(a, [0.0, 1.0]), (b, [1.0, 1.0]), (c, [0.5, 0.0])] {
            self.positions.push(vertex.to_array());
            self.normals.push(normal.to_array());
            self.uvs.push(uv);
        }
        self.indices.extend([base, base + 1, base + 2]);
    }

    /// Adds a quad wound `a → b → c → d`.
    pub fn quad(&mut self, a: Vec3, b: Vec3, c: Vec3, d: Vec3) {
        let normal = (b - a).cross(c - a).normalize_or_zero();
        let base = self.positions.len() as u32;
        for (vertex, uv) in [
            (a, [0.0, 1.0]),
            (b, [1.0, 1.0]),
            (c, [1.0, 0.0]),
            (d, [0.0, 0.0]),
        ] {
            self.positions.push(vertex.to_array());
            self.normals.push(normal.to_array());
            self.uvs.push(uv);
        }
        self.indices
            .extend([base, base + 1, base + 2, base, base + 2, base + 3]);
    }

    /// Adds an axis-aligned box, omitting the underside nobody can see.
    pub fn box_from_to(&mut self, min: Vec3, max: Vec3) {
        let (x0, y0, z0) = (min.x, min.y, min.z);
        let (x1, y1, z1) = (max.x, max.y, max.z);

        // Top.
        self.quad(
            Vec3::new(x0, y1, z1),
            Vec3::new(x1, y1, z1),
            Vec3::new(x1, y1, z0),
            Vec3::new(x0, y1, z0),
        );
        // Front (+z) and back (-z).
        self.quad(
            Vec3::new(x0, y0, z1),
            Vec3::new(x1, y0, z1),
            Vec3::new(x1, y1, z1),
            Vec3::new(x0, y1, z1),
        );
        self.quad(
            Vec3::new(x1, y0, z0),
            Vec3::new(x0, y0, z0),
            Vec3::new(x0, y1, z0),
            Vec3::new(x1, y1, z0),
        );
        // Right (+x) and left (-x).
        self.quad(
            Vec3::new(x1, y0, z1),
            Vec3::new(x1, y0, z0),
            Vec3::new(x1, y1, z0),
            Vec3::new(x1, y1, z1),
        );
        self.quad(
            Vec3::new(x0, y0, z0),
            Vec3::new(x0, y0, z1),
            Vec3::new(x0, y1, z1),
            Vec3::new(x0, y1, z0),
        );
    }

    /// Adds a vertical prism from a closed ground polygon.
    pub fn prism(&mut self, polygon: &[Vec2], base_y: f32, top_y: f32) {
        if polygon.len() < 3 {
            return;
        }
        let ring = upward_ring(polygon);
        self.ground_polygon(&ring, top_y);
        self.wall_ring(&ring, base_y, top_y);
    }

    /// Adds the walls of a prism without capping it.
    ///
    /// A skirt hanging below a surface that is already drawn wants exactly
    /// this: capping it would put a second horizontal face in the same plane as
    /// the ground above and the two would fight for the depth buffer.
    pub fn wall_ring(&mut self, polygon: &[Vec2], base_y: f32, top_y: f32) {
        if polygon.len() < 3 || top_y <= base_y {
            return;
        }
        let ring = upward_ring(polygon);
        for index in 0..ring.len() {
            let current = ring[index];
            let next = ring[(index + 1) % ring.len()];
            self.quad(
                Vec3::new(current.x, base_y, current.y),
                Vec3::new(next.x, base_y, next.y),
                Vec3::new(next.x, top_y, next.y),
                Vec3::new(current.x, top_y, current.y),
            );
        }
    }

    /// Adds a band between two closed polygons at different heights.
    ///
    /// The two must have the same number of points, in the same order, which is
    /// what lets a frustum be built from a rim and a scaled copy of it without
    /// anything having to match vertices up.
    pub fn skirt(&mut self, top: &[Vec2], top_y: f32, bottom: &[Vec2], bottom_y: f32) {
        if top.len() < 3 || top.len() != bottom.len() {
            return;
        }
        // Both rings are reordered by the same rule, so they stay in step.
        let top_ring = upward_ring(top);
        let bottom_ring = upward_ring(bottom);
        for index in 0..top_ring.len() {
            let next = (index + 1) % top_ring.len();
            self.quad(
                Vec3::new(bottom_ring[index].x, bottom_y, bottom_ring[index].y),
                Vec3::new(bottom_ring[next].x, bottom_y, bottom_ring[next].y),
                Vec3::new(top_ring[next].x, top_y, top_ring[next].y),
                Vec3::new(top_ring[index].x, top_y, top_ring[index].y),
            );
        }
    }

    /// Adds a flat horizontal polygon, used for ground surfaces.
    ///
    /// Handles concave outlines: wards and outlines from the layout are
    /// deliberately irregular, and a triangle fan would fold them back over
    /// themselves.
    pub fn ground_polygon(&mut self, polygon: &[Vec2], y: f32) {
        if polygon.len() < 3 {
            return;
        }
        let base = self.positions.len() as u32;
        for point in polygon {
            self.positions.push([point.x, y, point.y]);
            self.normals.push([0.0, 1.0, 0.0]);
            self.uvs.push([point.x, point.y]);
        }
        for [a, b, c] in triangulate(polygon) {
            // Triangulation works counter-clockwise in the ground plane's own
            // coordinates, which faces down once it is laid into a y-up world.
            self.indices
                .extend([base + c as u32, base + b as u32, base + a as u32]);
        }
    }

    /// Adds a cone with its apex at `apex_y`, used for spires and foliage.
    pub fn cone(&mut self, center: Vec2, radius: f32, base_y: f32, apex_y: f32, segments: usize) {
        let segments = segments.max(3);
        let apex = Vec3::new(center.x, apex_y, center.y);
        for index in 0..segments {
            let (from, to) = (
                ring_point(center, radius, index, segments, base_y),
                ring_point(center, radius, index + 1, segments, base_y),
            );
            self.triangle(from, to, apex);
        }
    }

    /// Adds a cone hanging downward, with its apex *below* the ring.
    ///
    /// Not [`Self::cone`] with the arguments swapped: that winds each face
    /// `from -> to -> apex` against a ring [`ring_point`] deliberately builds
    /// clockwise so an upright cone's walls face outward. Move the apex
    /// underneath and the same winding faces inward, and back-face culling eats
    /// the whole spire. This walks the ring the other way instead.
    pub fn spire(&mut self, center: Vec2, radius: f32, base_y: f32, tip_y: f32, segments: usize) {
        let segments = segments.max(3);
        let tip = Vec3::new(center.x, tip_y, center.y);
        for index in 0..segments {
            let (from, to) = (
                ring_point(center, radius, index, segments, base_y),
                ring_point(center, radius, index + 1, segments, base_y),
            );
            self.triangle(to, from, tip);
        }
    }

    /// Adds the walls of a prism facing *inward*, for a shaft seen from above.
    ///
    /// Not [`Self::wall_ring`] with the points reversed: that normalises its
    /// winding through [`upward_ring`] before it builds anything, so reversing
    /// them changes nothing at all and back-face culling eats the whole shaft.
    /// The same trap [`Self::spire`] documents against [`Self::cone`], one
    /// level down -- here the fix is to wind each quad the other way rather
    /// than to walk the ring the other way.
    pub fn inward_wall_ring(&mut self, polygon: &[Vec2], base_y: f32, top_y: f32) {
        if polygon.len() < 3 || top_y <= base_y {
            return;
        }
        let ring = upward_ring(polygon);
        for index in 0..ring.len() {
            let current = ring[index];
            let next = ring[(index + 1) % ring.len()];
            self.quad(
                Vec3::new(current.x, top_y, current.y),
                Vec3::new(next.x, top_y, next.y),
                Vec3::new(next.x, base_y, next.y),
                Vec3::new(current.x, base_y, current.y),
            );
        }
    }

    /// Adds a flat horizontal ring: a disc with a hole in the middle.
    ///
    /// What caps a hollow drum. [`Self::ground_polygon`] can only fill a solid
    /// outline, and filling this one would put a lid over the very hole the
    /// shaft is drawn to show.
    pub fn annulus(&mut self, center: Vec2, inner: f32, outer: f32, y: f32, segments: usize) {
        let segments = segments.max(3);
        if outer <= inner {
            return;
        }
        let at = |radius: f32, index: usize| {
            let angle = index as f32 / segments as f32 * std::f32::consts::TAU;
            Vec3::new(
                center.x + angle.cos() * radius,
                y,
                center.y + angle.sin() * radius,
            )
        };
        for index in 0..segments {
            let next = index + 1;
            // Inner edge first and outer second, which is the winding that
            // faces upward once the ring is laid into a y-up world -- the
            // opposite order builds a ring visible only from underneath.
            self.quad(
                at(inner, index),
                at(inner, next),
                at(outer, next),
                at(outer, index),
            );
        }
    }

    /// Adds a closed cylinder, used for towers and tree trunks.
    pub fn cylinder(
        &mut self,
        center: Vec2,
        radius: f32,
        base_y: f32,
        top_y: f32,
        segments: usize,
    ) {
        let segments = segments.max(3);
        let ring: Vec<Vec2> = (0..segments)
            .map(|index| {
                let angle = index as f32 / segments as f32 * std::f32::consts::TAU;
                center + Vec2::new(angle.cos(), angle.sin()) * radius
            })
            .collect();
        self.prism(&ring, base_y, top_y);
    }

    /// Whether no triangles have been added, so the mesh can be skipped.
    pub fn is_empty(&self) -> bool {
        self.indices.is_empty()
    }

    /// Consumes the builder and produces the finished mesh.
    pub fn build(self) -> Mesh {
        Mesh::new(
            PrimitiveTopology::TriangleList,
            RenderAssetUsages::MAIN_WORLD | RenderAssetUsages::RENDER_WORLD,
        )
        .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, self.positions)
        .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, self.normals)
        .with_inserted_attribute(Mesh::ATTRIBUTE_UV_0, self.uvs)
        .with_inserted_indices(Indices::U32(self.indices))
    }
}

fn ring_point(center: Vec2, radius: f32, index: usize, segments: usize, y: f32) -> Vec3 {
    let angle = index as f32 / segments as f32 * std::f32::consts::TAU;
    Vec3::new(
        center.x + angle.cos() * radius,
        y,
        // Negated so the ring runs the way a ground polygon has to run to face
        // upward, which is what keeps cone and cylinder walls facing outward.
        center.y - angle.sin() * radius,
    )
}

/// Twice the signed area of a polygon in the ground plane.
///
/// Positive means counter-clockwise in the polygon's own `(x, y)` coordinates.
fn signed_area(polygon: &[Vec2]) -> f32 {
    let mut total = 0.0;
    for index in 0..polygon.len() {
        let current = polygon[index];
        let next = polygon[(index + 1) % polygon.len()];
        total += current.x * next.y - next.x * current.y;
    }
    total
}

/// Reorders a polygon so a surface built from it faces upward.
///
/// Bevy is right-handed with `y` up, so laying a ground polygon into the world
/// flips its handedness: a polygon that reads counter-clockwise on paper faces
/// *down* once it is on the ground and gets culled. Scene outlines arrive in
/// whichever order the layout produced them, so the two conventions are
/// reconciled here, once, rather than at every call site.
fn upward_ring(polygon: &[Vec2]) -> Vec<Vec2> {
    let mut ring = polygon.to_vec();
    if signed_area(polygon) > 0.0 {
        ring.reverse();
    }
    ring
}

/// Splits a simple polygon into triangles by ear clipping.
///
/// Returns indices into `polygon`, wound counter-clockwise in the polygon's own
/// coordinates. Wards and the outlines the layout produces are frequently
/// concave, so a fan from one vertex would produce triangles that spill outside
/// the outline.
fn triangulate(polygon: &[Vec2]) -> Vec<[usize; 3]> {
    let count = polygon.len();
    if count < 3 {
        return Vec::new();
    }
    // Work counter-clockwise so the ear test only has one orientation to
    // consider.
    let mut remaining: Vec<usize> = if signed_area(polygon) > 0.0 {
        (0..count).collect()
    } else {
        (0..count).rev().collect()
    };

    let mut triangles = Vec::with_capacity(count - 2);
    while remaining.len() > 3 {
        let mut clipped = None;
        for position in 0..remaining.len() {
            let previous = remaining[(position + remaining.len() - 1) % remaining.len()];
            let current = remaining[position];
            let next = remaining[(position + 1) % remaining.len()];
            if is_ear(polygon, &remaining, previous, current, next) {
                triangles.push([previous, current, next]);
                clipped = Some(position);
                break;
            }
        }
        match clipped {
            Some(position) => {
                remaining.remove(position);
            }
            // Self-intersecting or degenerate input. Fanning the rest keeps the
            // surface present rather than dropping it entirely.
            None => break,
        }
    }
    for index in 1..remaining.len().saturating_sub(1) {
        triangles.push([remaining[0], remaining[index], remaining[index + 1]]);
    }
    triangles
}

/// Whether the corner at `current` can be clipped off as a triangle.
fn is_ear(
    polygon: &[Vec2],
    remaining: &[usize],
    previous: usize,
    current: usize,
    next: usize,
) -> bool {
    let (a, b, c) = (polygon[previous], polygon[current], polygon[next]);
    let cross = (b - a).perp_dot(c - a);
    if cross <= f32::EPSILON {
        // Reflex or collinear, so the corner is not an ear.
        return false;
    }
    !remaining
        .iter()
        .filter(|index| **index != previous && **index != current && **index != next)
        .any(|index| point_in_triangle(polygon[*index], a, b, c))
}

fn point_in_triangle(point: Vec2, a: Vec2, b: Vec2, c: Vec2) -> bool {
    let first = (b - a).perp_dot(point - a);
    let second = (c - b).perp_dot(point - b);
    let third = (a - c).perp_dot(point - c);
    first >= 0.0 && second >= 0.0 && third >= 0.0
}

/// The parts a holding is assembled from.
///
/// Splitting a building into three meshes lets each one carry its own material
/// — walls, roof, and trim — without a texture, and lets the fine details drop
/// out at distance without rebuilding anything.
pub struct BuildingMeshes {
    /// The body of the building.
    pub walls: Mesh,
    /// The roof, which carries its own colour.
    pub roof: Mesh,
    /// Windows, chimneys and trim, absent on distant or plain buildings.
    pub details: Option<Mesh>,
}

/// The shape of a holding, quantised so thousands of files share a handful of
/// meshes.
///
/// Height and squareness are bucketed rather than exact: a repository with five
/// thousand files would otherwise allocate five thousand meshes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct BuildingShape {
    /// Which silhouette to build.
    pub kind: BuildingKind,
    /// Height as a multiple of the footprint's shorter side, in tenths.
    pub height_ratio: u16,
    /// Whether the holding is detailed enough to earn windows and chimneys.
    pub detailed: bool,
    /// Bucketed complexity, driving crate counts and forge stacks.
    pub complexity_step: u8,
}

impl BuildingShape {
    /// Quantises a holding's true measurements into a shared shape.
    pub fn new(kind: BuildingKind, footprint: Vec2, height: f32, complexity: u32) -> Self {
        let plan = footprint.x.min(footprint.y).max(0.001);
        let ratio = (height / plan * 10.0).round().clamp(2.0, 240.0) as u16;
        Self {
            kind,
            height_ratio: ratio,
            detailed: footprint.x > 1.5 && footprint.y > 1.2,
            complexity_step: (complexity / 4).min(6) as u8,
        }
    }

    /// Height in local units, where the footprint is one unit square.
    pub fn height(&self) -> f32 {
        self.height_ratio as f32 / 10.0
    }
}

/// Builds the geometry for one archetype.
pub fn build_building(shape: BuildingShape) -> BuildingMeshes {
    let height = shape.height();
    match shape.kind {
        BuildingKind::Keep => keep(shape, height),
        BuildingKind::Watchtower => watchtower(shape, height),
        BuildingKind::Market => market(shape, height),
        BuildingKind::Scriptorium => scriptorium(shape, height),
        BuildingKind::CouncilHall => council_hall(shape, height),
        BuildingKind::Granary => granary(shape, height),
        BuildingKind::Forge => forge(shape, height),
        BuildingKind::Stockpile => stockpile(shape),
        BuildingKind::Guildhall => pitched(shape, height, true),
        BuildingKind::Cottage => pitched(shape, height, false),
    }
}

/// The unit footprint every archetype is built inside.
const HALF: f32 = 0.5;

/// A hall or cottage: four walls under a pitched roof running along `x`.
fn pitched(shape: BuildingShape, height: f32, is_hall: bool) -> BuildingMeshes {
    let wall_height = height * if is_hall { 0.72 } else { 0.62 };
    let roof_height = (height - wall_height)
        .min(if is_hall { 0.75 } else { 0.6 })
        .max(0.08);
    let ridge_y = wall_height + roof_height;
    let overhang = 0.06;

    let mut walls = MeshBuilder::new();
    walls.box_from_to(
        Vec3::new(-HALF, 0.0, -HALF),
        Vec3::new(HALF, wall_height, HALF),
    );
    // Gable triangles filling the wall up to the ridge.
    walls.triangle(
        Vec3::new(HALF, wall_height, HALF),
        Vec3::new(HALF, wall_height, -HALF),
        Vec3::new(HALF, ridge_y, 0.0),
    );
    walls.triangle(
        Vec3::new(-HALF, wall_height, -HALF),
        Vec3::new(-HALF, wall_height, HALF),
        Vec3::new(-HALF, ridge_y, 0.0),
    );

    let mut roof = MeshBuilder::new();
    let eave = HALF + overhang;
    let ridge_left = Vec3::new(-eave, ridge_y, 0.0);
    let ridge_right = Vec3::new(eave, ridge_y, 0.0);
    // Two slopes.
    roof.quad(
        Vec3::new(-eave, wall_height, eave),
        Vec3::new(eave, wall_height, eave),
        ridge_right,
        ridge_left,
    );
    roof.quad(
        Vec3::new(eave, wall_height, -eave),
        Vec3::new(-eave, wall_height, -eave),
        ridge_left,
        ridge_right,
    );
    // Close the gable ends of the roof itself.
    roof.triangle(
        Vec3::new(eave, wall_height, eave),
        Vec3::new(eave, wall_height, -eave),
        ridge_right,
    );
    roof.triangle(
        Vec3::new(-eave, wall_height, -eave),
        Vec3::new(-eave, wall_height, eave),
        ridge_left,
    );

    let mut details = MeshBuilder::new();
    if shape.detailed {
        // A door on the +z face and a window either side of it.
        let door_height = wall_height * 0.55;
        details.box_from_to(
            Vec3::new(-0.09, 0.0, HALF - 0.02),
            Vec3::new(0.09, door_height, HALF + 0.03),
        );
        for offset in [-0.28, 0.28] {
            details.box_from_to(
                Vec3::new(offset - 0.07, wall_height * 0.45, HALF - 0.02),
                Vec3::new(offset + 0.07, wall_height * 0.72, HALF + 0.03),
            );
        }
    }
    if shape.complexity_step >= 1 {
        // A chimney, taller the more complex the file.
        let stack = ridge_y + 0.12 + shape.complexity_step as f32 * 0.06;
        details.box_from_to(
            Vec3::new(0.14, wall_height, -0.16),
            Vec3::new(0.28, stack, -0.02),
        );
    }

    BuildingMeshes {
        walls: walls.build(),
        roof: roof.build(),
        details: (!details.is_empty()).then(|| details.build()),
    }
}

/// A fortified block with a crenellated parapet.
fn keep(shape: BuildingShape, height: f32) -> BuildingMeshes {
    let body = height * 0.94;
    let mut walls = MeshBuilder::new();
    walls.box_from_to(Vec3::new(-HALF, 0.0, -HALF), Vec3::new(HALF, body, HALF));

    let mut roof = MeshBuilder::new();
    roof.box_from_to(
        Vec3::new(-HALF - 0.04, body, -HALF - 0.04),
        Vec3::new(HALF + 0.04, body + 0.05, HALF + 0.04),
    );

    let mut details = MeshBuilder::new();
    let merlon = body + 0.05;
    let step = 0.25;
    let mut offset = -HALF;
    while offset < HALF - 0.01 {
        for edge in [-HALF - 0.04, HALF - 0.08] {
            details.box_from_to(
                Vec3::new(offset, merlon, edge),
                Vec3::new(offset + 0.12, merlon + 0.14, edge + 0.12),
            );
            details.box_from_to(
                Vec3::new(edge, merlon, offset),
                Vec3::new(edge + 0.12, merlon + 0.14, offset + 0.12),
            );
        }
        offset += step;
    }
    if shape.detailed {
        details.box_from_to(
            Vec3::new(-0.1, 0.0, HALF - 0.02),
            Vec3::new(0.1, body * 0.4, HALF + 0.03),
        );
    }

    BuildingMeshes {
        walls: walls.build(),
        roof: roof.build(),
        details: Some(details.build()),
    }
}

/// A round tower under a tall conical spire.
fn watchtower(shape: BuildingShape, height: f32) -> BuildingMeshes {
    let shaft = height * 0.74;
    let radius = 0.42;

    let mut walls = MeshBuilder::new();
    walls.cylinder(Vec2::ZERO, radius, 0.0, shaft, 12);

    let mut roof = MeshBuilder::new();
    roof.cone(Vec2::ZERO, radius + 0.07, shaft, height, 12);

    let mut details = MeshBuilder::new();
    // The gallery ring below the spire.
    details.cylinder(Vec2::ZERO, radius + 0.05, shaft - 0.09, shaft, 12);
    if shape.detailed {
        for step in 0..3 {
            let y = shaft * (0.3 + step as f32 * 0.2);
            details.box_from_to(
                Vec3::new(-0.05, y, radius - 0.02),
                Vec3::new(0.05, y + 0.12, radius + 0.04),
            );
        }
    }

    BuildingMeshes {
        walls: walls.build(),
        roof: roof.build(),
        details: Some(details.build()),
    }
}

/// A low stall under a wide sloping awning.
fn market(shape: BuildingShape, height: f32) -> BuildingMeshes {
    let wall_height = height * 0.6;
    let mut walls = MeshBuilder::new();
    walls.box_from_to(
        Vec3::new(-HALF, 0.0, -HALF),
        Vec3::new(HALF, wall_height, HALF * 0.55),
    );

    // A single slab sloping down towards the front of the lot.
    let mut roof = MeshBuilder::new();
    let back = wall_height + (height - wall_height).max(0.1);
    let front = wall_height * 0.82;
    roof.quad(
        Vec3::new(-HALF - 0.08, back, -HALF - 0.05),
        Vec3::new(HALF + 0.08, back, -HALF - 0.05),
        Vec3::new(HALF + 0.08, front, HALF + 0.12),
        Vec3::new(-HALF - 0.08, front, HALF + 0.12),
    );

    let mut details = MeshBuilder::new();
    // Posts holding the awning up over the open front.
    for x in [-HALF + 0.06, HALF - 0.12] {
        details.box_from_to(
            Vec3::new(x, 0.0, HALF + 0.02),
            Vec3::new(x + 0.06, front, HALF + 0.08),
        );
    }
    if shape.detailed {
        details.box_from_to(
            Vec3::new(-HALF + 0.1, wall_height * 0.45, HALF * 0.5),
            Vec3::new(HALF - 0.1, wall_height * 0.62, HALF * 0.58),
        );
    }

    BuildingMeshes {
        walls: walls.build(),
        roof: roof.build(),
        details: Some(details.build()),
    }
}

/// A hall crowned by a slender spire.
fn scriptorium(shape: BuildingShape, height: f32) -> BuildingMeshes {
    let wall_height = height * 0.58;
    let roof_height = (height * 0.22).max(0.1);
    let ridge_y = wall_height + roof_height;

    let mut walls = MeshBuilder::new();
    walls.box_from_to(
        Vec3::new(-HALF, 0.0, -HALF),
        Vec3::new(HALF, wall_height, HALF),
    );
    walls.triangle(
        Vec3::new(HALF, wall_height, HALF),
        Vec3::new(HALF, wall_height, -HALF),
        Vec3::new(HALF, ridge_y, 0.0),
    );
    walls.triangle(
        Vec3::new(-HALF, wall_height, -HALF),
        Vec3::new(-HALF, wall_height, HALF),
        Vec3::new(-HALF, ridge_y, 0.0),
    );

    let mut roof = MeshBuilder::new();
    let eave = HALF + 0.05;
    roof.quad(
        Vec3::new(-eave, wall_height, eave),
        Vec3::new(eave, wall_height, eave),
        Vec3::new(eave, ridge_y, 0.0),
        Vec3::new(-eave, ridge_y, 0.0),
    );
    roof.quad(
        Vec3::new(eave, wall_height, -eave),
        Vec3::new(-eave, wall_height, -eave),
        Vec3::new(-eave, ridge_y, 0.0),
        Vec3::new(eave, ridge_y, 0.0),
    );
    roof.cone(Vec2::new(-0.22, 0.0), 0.14, ridge_y, height, 8);

    let mut details = MeshBuilder::new();
    if shape.detailed {
        // Tall lancet windows along the front.
        for offset in [-0.26, 0.0, 0.26] {
            details.box_from_to(
                Vec3::new(offset - 0.05, wall_height * 0.3, HALF - 0.02),
                Vec3::new(offset + 0.05, wall_height * 0.78, HALF + 0.03),
            );
        }
    }

    BuildingMeshes {
        walls: walls.build(),
        roof: roof.build(),
        details: (!details.is_empty()).then(|| details.build()),
    }
}

/// A civic hall flying a banner from its roof.
fn council_hall(shape: BuildingShape, height: f32) -> BuildingMeshes {
    let mut meshes = pitched(shape, height * 0.88, true);

    let mut details = MeshBuilder::new();
    let mast = height * 1.06;
    details.box_from_to(
        Vec3::new(-0.02, height * 0.6, -0.02),
        Vec3::new(0.02, mast, 0.02),
    );
    // The banner itself, hanging off the mast.
    details.quad(
        Vec3::new(0.02, mast, -0.01),
        Vec3::new(0.3, mast - 0.04, -0.01),
        Vec3::new(0.3, mast - 0.24, -0.01),
        Vec3::new(0.02, mast - 0.2, -0.01),
    );
    details.quad(
        Vec3::new(0.02, mast - 0.2, 0.01),
        Vec3::new(0.3, mast - 0.24, 0.01),
        Vec3::new(0.3, mast - 0.04, 0.01),
        Vec3::new(0.02, mast, 0.01),
    );
    if let Some(existing) = meshes.details.take() {
        meshes.details = Some(merge(existing, details.build()));
    } else {
        meshes.details = Some(details.build());
    }
    meshes
}

/// A store house fronted by a row of bins.
fn granary(shape: BuildingShape, height: f32) -> BuildingMeshes {
    let mut meshes = pitched(shape, height, false);

    let mut details = MeshBuilder::new();
    let bins = 2 + shape.complexity_step.min(3) as usize;
    let span = 1.0 / bins as f32;
    for index in 0..bins {
        let x = -HALF + span * index as f32 + span * 0.15;
        let bin = 0.12 + (index % 2) as f32 * 0.05;
        details.box_from_to(
            Vec3::new(x, 0.0, HALF + 0.02),
            Vec3::new(x + span * 0.7, bin, HALF + 0.2),
        );
    }
    if let Some(existing) = meshes.details.take() {
        meshes.details = Some(merge(existing, details.build()));
    } else {
        meshes.details = Some(details.build());
    }
    meshes
}

/// A workshop with a heavy chimney stack.
fn forge(shape: BuildingShape, height: f32) -> BuildingMeshes {
    let mut meshes = pitched(shape, height * 0.9, false);

    let mut details = MeshBuilder::new();
    let stack = height * (1.05 + shape.complexity_step as f32 * 0.04);
    details.box_from_to(
        Vec3::new(0.1, height * 0.3, -0.28),
        Vec3::new(0.32, stack, -0.06),
    );
    // A wider cap so the stack reads as a chimney and not a post.
    details.box_from_to(
        Vec3::new(0.06, stack, -0.32),
        Vec3::new(0.36, stack + 0.06, -0.02),
    );
    if let Some(existing) = meshes.details.take() {
        meshes.details = Some(merge(existing, details.build()));
    } else {
        meshes.details = Some(details.build());
    }
    meshes
}

/// An open yard of stacked crates rather than a building.
fn stockpile(shape: BuildingShape) -> BuildingMeshes {
    let height = shape.height();
    let mut walls = MeshBuilder::new();
    let mut roof = MeshBuilder::new();

    let stacks = 2 + shape.complexity_step.min(4) as usize;
    let crate_size = (0.9 / stacks as f32).clamp(0.14, 0.34);
    for index in 0..stacks {
        let column = index % 3;
        let row = index / 3;
        let x = -HALF + 0.08 + column as f32 * (crate_size + 0.06);
        let z = -HALF + 0.1 + row as f32 * (crate_size + 0.08);
        let stacked = 1 + (index % 3);
        for level in 0..stacked {
            let base = level as f32 * crate_size;
            let top = (base + crate_size).min(height.max(crate_size));
            let inset = level as f32 * 0.012;
            let target = if level % 2 == 0 {
                &mut walls
            } else {
                &mut roof
            };
            target.box_from_to(
                Vec3::new(x + inset, base, z + inset),
                Vec3::new(x + crate_size - inset, top, z + crate_size - inset),
            );
        }
    }

    BuildingMeshes {
        walls: walls.build(),
        roof: roof.build(),
        details: None,
    }
}

/// Appends `extra` onto `base`, offsetting its indices.
fn merge(base: Mesh, extra: Mesh) -> Mesh {
    let mut builder = MeshBuilder::new();
    for mesh in [&base, &extra] {
        let offset = builder.positions.len() as u32;
        let (Some(positions), Some(normals)) = (
            mesh.attribute(Mesh::ATTRIBUTE_POSITION)
                .and_then(|values| values.as_float3()),
            mesh.attribute(Mesh::ATTRIBUTE_NORMAL)
                .and_then(|values| values.as_float3()),
        ) else {
            continue;
        };
        builder.positions.extend_from_slice(positions);
        builder.normals.extend_from_slice(normals);
        builder.uvs.resize(builder.positions.len(), [0.0, 0.0]);
        if let Some(Indices::U32(indices)) = mesh.indices() {
            builder
                .indices
                .extend(indices.iter().map(|index| index + offset));
        }
    }
    builder.build()
}

/// A conical tree, sized by the manifest.
pub fn tree_foliage(radius: f32, height: f32) -> Mesh {
    let mut builder = MeshBuilder::new();
    // Two stacked cones read as a fuller crown than one.
    builder.cone(Vec2::ZERO, radius, height * 0.32, height, 9);
    builder.cone(Vec2::ZERO, radius * 0.74, height * 0.58, height * 1.16, 9);
    builder.build()
}

/// A tree's trunk, a plain six-sided cylinder.
pub fn tree_trunk(radius: f32, height: f32) -> Mesh {
    let mut builder = MeshBuilder::new();
    builder.cylinder(Vec2::ZERO, radius, 0.0, height, 6);
    builder.build()
}

/// A flat ground surface from a closed polygon.
pub fn ground_polygon(polygon: &[Vec2]) -> Mesh {
    let mut builder = MeshBuilder::new();
    builder.ground_polygon(polygon, 0.0);
    builder.build()
}

/// The lit band straight down from the rim: the edge you could fall off.
///
/// Separate from [`disk_underside`] so it can take its own colour. The two
/// meet exactly at `-depth` and never overlap, so neither has to be lifted off
/// the other to keep the depth buffer honest.
pub fn rim_cliff(rim: &[Vec2], depth: f32) -> Mesh {
    let mut builder = MeshBuilder::new();
    builder.wall_ring(rim, -depth.max(0.0), 0.0);
    builder.build()
}

/// The frustum under the cliff, pulling in toward the spire.
///
/// Separate from the spire so the two can take different colours: both are
/// drawn unlit, so a flat band and a darker point below it is the only way the
/// underside reads as falling away rather than as one silhouette.
pub fn disk_shelf(rim: &[Vec2], underside: &MapUnderside) -> Mesh {
    let mut builder = MeshBuilder::new();
    if rim.len() < 3 {
        return builder.build();
    }
    let center = centroid(rim);
    builder.skirt(
        rim,
        cliff_bottom(underside),
        &shelf_ring(rim, center, underside),
        shelf_bottom(underside),
    );
    builder.build()
}

/// The spire falling from the shelf to a point.
///
/// Nothing caps the bottom. The camera is held at a fixed downward pitch, so
/// the underside of the spire can never come into view -- the same reason
/// [`MeshBuilder::box_from_to`] omits the bottom face of every building.
pub fn disk_spire(rim: &[Vec2], underside: &MapUnderside) -> Mesh {
    let mut builder = MeshBuilder::new();
    if rim.len() < 3 {
        return builder.build();
    }
    let center = centroid(rim);
    let shelf = shelf_ring(rim, center, underside);
    let radius = shelf
        .iter()
        .map(|point| point.distance(center))
        .fold(0.0f32, f32::max);
    let bottom = shelf_bottom(underside);
    builder.spire(
        center,
        radius,
        bottom,
        bottom.min(-underside.depth.max(0.0)),
        SPIRE_SEGMENTS,
    );
    builder.build()
}

fn cliff_bottom(underside: &MapUnderside) -> f32 {
    -underside.cliff.max(0.0)
}

fn shelf_bottom(underside: &MapUnderside) -> f32 {
    cliff_bottom(underside).min(-underside.shelf.max(0.0))
}

/// The rim pulled in to where the spire hangs from.
fn shelf_ring(rim: &[Vec2], center: Vec2, underside: &MapUnderside) -> Vec<Vec2> {
    let taper = underside.taper.clamp(0.05, 1.0);
    rim.iter()
        .map(|point| center + (*point - center) * taper)
        .collect()
}

/// How many sides the spire is built with.
///
/// Fewer than the rim above it on purpose: the spire is seen edge-on at a
/// distance where a faceted silhouette reads as rock, and matching the rim's
/// segment count would spend triangles on a curve nobody can resolve.
const SPIRE_SEGMENTS: usize = 32;

/// The average of a polygon's points, which the underside tapers toward.
fn centroid(polygon: &[Vec2]) -> Vec2 {
    if polygon.is_empty() {
        return Vec2::ZERO;
    }
    polygon.iter().copied().sum::<Vec2>() / polygon.len() as f32
}

/// A ribbon following a polyline, used for roads.
pub fn ribbon(points: &[Vec2], width: f32) -> Mesh {
    let mut builder = MeshBuilder::new();
    let half = width * 0.5;
    for segment in points.windows(2) {
        let (from, to) = (segment[0], segment[1]);
        let direction = (to - from).normalize_or_zero();
        if direction == Vec2::ZERO {
            continue;
        }
        // Extend each segment by half its width so the corners meet without a
        // notch where the road bends.
        let from = from - direction * half;
        let to = to + direction * half;
        let side = Vec2::new(-direction.y, direction.x) * half;
        // Wound so the ribbon faces upward once it is laid into a y-up world.
        builder.quad(
            Vec3::new(from.x - side.x, 0.0, from.y - side.y),
            Vec3::new(from.x + side.x, 0.0, from.y + side.y),
            Vec3::new(to.x + side.x, 0.0, to.y + side.y),
            Vec3::new(to.x - side.x, 0.0, to.y - side.y),
        );
    }
    builder.build()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Reads back a mesh's triangles as world-space positions.
    ///
    /// Face orientation is decided by index winding, not by the normal
    /// attribute, so anything checking whether a surface is visible has to look
    /// at the triangles themselves. Getting this wrong is invisible in a normal
    /// check and fatal on screen: the face is simply culled away.
    fn triangles(mesh: &Mesh) -> Vec<[Vec3; 3]> {
        let positions = mesh
            .attribute(Mesh::ATTRIBUTE_POSITION)
            .and_then(|values| values.as_float3())
            .expect("mesh has positions");
        let Some(Indices::U32(indices)) = mesh.indices() else {
            panic!("mesh has no indices");
        };
        indices
            .chunks_exact(3)
            .map(|face| {
                [
                    Vec3::from_array(positions[face[0] as usize]),
                    Vec3::from_array(positions[face[1] as usize]),
                    Vec3::from_array(positions[face[2] as usize]),
                ]
            })
            .collect()
    }

    /// The direction a triangle actually faces, from its winding.
    fn winding_normal(face: [Vec3; 3]) -> Vec3 {
        (face[1] - face[0])
            .cross(face[2] - face[0])
            .normalize_or_zero()
    }

    fn vertex_count(mesh: &Mesh) -> usize {
        mesh.attribute(Mesh::ATTRIBUTE_POSITION)
            .and_then(|values| values.as_float3())
            .map(<[[f32; 3]]>::len)
            .unwrap_or(0)
    }

    fn highest_point(mesh: &Mesh) -> f32 {
        mesh.attribute(Mesh::ATTRIBUTE_POSITION)
            .and_then(|values| values.as_float3())
            .map(|points| points.iter().fold(f32::MIN, |top, point| top.max(point[1])))
            .unwrap_or(f32::MIN)
    }

    fn assert_normals_are_unit_length(mesh: &Mesh) {
        let normals = mesh
            .attribute(Mesh::ATTRIBUTE_NORMAL)
            .and_then(|values| values.as_float3())
            .expect("mesh has normals");
        for normal in normals {
            let length =
                (normal[0] * normal[0] + normal[1] * normal[1] + normal[2] * normal[2]).sqrt();
            assert!(
                (length - 1.0).abs() < 1e-3,
                "normal {normal:?} had length {length}"
            );
        }
    }

    #[test]
    fn every_archetype_builds_lit_geometry() {
        for kind in BuildingKind::ALL {
            let shape = BuildingShape::new(kind, Vec2::new(4.0, 3.0), 6.0, 12);
            let meshes = build_building(shape);
            assert!(vertex_count(&meshes.walls) > 0, "{kind:?} has no walls");
            assert_normals_are_unit_length(&meshes.walls);
            assert_normals_are_unit_length(&meshes.roof);
            if let Some(details) = &meshes.details {
                assert_normals_are_unit_length(details);
            }
        }
    }

    #[test]
    fn buildings_reach_the_height_they_were_asked_for() {
        // A stockpile is a yard of crates, so it deliberately ignores height.
        for kind in BuildingKind::ALL
            .into_iter()
            .filter(|kind| *kind != BuildingKind::Stockpile)
        {
            let shape = BuildingShape::new(kind, Vec2::splat(2.0), 5.0, 3);
            let meshes = build_building(shape);
            let peak = highest_point(&meshes.walls)
                .max(highest_point(&meshes.roof))
                .max(
                    meshes
                        .details
                        .as_ref()
                        .map(highest_point)
                        .unwrap_or(f32::MIN),
                );
            let expected = shape.height();
            assert!(
                peak >= expected * 0.85,
                "{kind:?} peaked at {peak}, wanted about {expected}"
            );
        }
    }

    #[test]
    fn shapes_quantise_so_meshes_can_be_shared() {
        let first = BuildingShape::new(BuildingKind::Cottage, Vec2::splat(2.0), 4.00, 3);
        let second = BuildingShape::new(BuildingKind::Cottage, Vec2::splat(2.0), 4.02, 3);
        let taller = BuildingShape::new(BuildingKind::Cottage, Vec2::splat(2.0), 9.0, 3);

        assert_eq!(first, second, "near-identical holdings must share a mesh");
        assert_ne!(first, taller);
    }

    #[test]
    fn a_ribbon_covers_every_segment_of_its_polyline() {
        let road = ribbon(
            &[Vec2::ZERO, Vec2::new(10.0, 0.0), Vec2::new(10.0, 8.0)],
            4.0,
        );
        // Two segments, each a quad of four vertices.
        assert_eq!(vertex_count(&road), 8);
        assert_normals_are_unit_length(&road);
    }

    #[test]
    fn a_ribbon_ignores_zero_length_segments() {
        let road = ribbon(&[Vec2::ZERO, Vec2::ZERO, Vec2::new(4.0, 0.0)], 2.0);
        assert_eq!(vertex_count(&road), 4);
    }

    #[test]
    fn ground_polygons_face_upwards() {
        // Both windings must produce an upward-facing surface: scene outlines
        // arrive in whichever order the layout produced them.
        for polygon in [
            vec![
                Vec2::ZERO,
                Vec2::new(10.0, 0.0),
                Vec2::new(10.0, 10.0),
                Vec2::new(0.0, 10.0),
            ],
            vec![
                Vec2::new(0.0, 10.0),
                Vec2::new(10.0, 10.0),
                Vec2::new(10.0, 0.0),
                Vec2::ZERO,
            ],
        ] {
            let mesh = ground_polygon(&polygon);
            for face in triangles(&mesh) {
                assert!(
                    winding_normal(face).y > 0.99,
                    "a ground triangle faced {:?}",
                    winding_normal(face)
                );
            }
        }
    }

    /// A ring of world-space points, standing in for the rim of a disk.
    fn ring(radius: f32, segments: usize) -> Vec<Vec2> {
        (0..segments)
            .map(|index| {
                let angle = index as f32 / segments as f32 * std::f32::consts::TAU;
                Vec2::new(angle.cos(), angle.sin()) * radius
            })
            .collect()
    }

    fn underside() -> MapUnderside {
        MapUnderside {
            cliff: 20.0,
            shelf: 90.0,
            taper: 0.6,
            depth: 400.0,
            cliff_color: [96, 84, 68, 255],
            rock: [48, 42, 38, 255],
            deep: [30, 26, 24, 255],
        }
    }

    /// A skirt hangs below ground that is already drawn, so capping it would
    /// put a second horizontal face in the same plane and the two would fight.
    #[test]
    fn a_wall_ring_has_walls_and_no_cap() {
        let mut builder = MeshBuilder::new();
        builder.wall_ring(&ring(50.0, 8), -10.0, 0.0);
        let mesh = builder.build();

        // Eight walls, four vertices each. A cap would add more.
        assert_eq!(vertex_count(&mesh), 32);
        for face in triangles(&mesh) {
            let normal = winding_normal(face);
            assert!(
                normal.y.abs() < 1e-3,
                "a wall faced {normal:?}, which is not vertical"
            );
        }
        assert_normals_are_unit_length(&mesh);
    }

    /// The whole reason [`MeshBuilder::spire`] exists rather than reusing
    /// `cone`: hang the apex underneath with `cone`'s winding and back-face
    /// culling eats the entire spire.
    #[test]
    fn a_spire_faces_outward() {
        let mut builder = MeshBuilder::new();
        builder.spire(Vec2::ZERO, 100.0, -50.0, -400.0, 16);
        let mesh = builder.build();

        for face in triangles(&mesh) {
            let normal = winding_normal(face);
            let outward = Vec3::new(
                (face[0].x + face[1].x + face[2].x) / 3.0,
                0.0,
                (face[0].z + face[1].z + face[2].z) / 3.0,
            )
            .normalize_or_zero();
            assert!(
                normal.dot(outward) > 0.0,
                "a spire face at {outward:?} pointed inward as {normal:?}"
            );
        }
    }

    /// The rim of a well: a flat ring, facing up, with the hole left open.
    ///
    /// Capping the drum instead would make it a plinth, and the hole is the
    /// whole point of a well.
    #[test]
    fn an_annulus_faces_upward_and_keeps_its_hole() {
        let mut builder = MeshBuilder::new();
        builder.annulus(Vec2::ZERO, 6.0, 10.0, 4.0, 12);
        let mesh = builder.build();

        for face in triangles(&mesh) {
            assert!(
                winding_normal(face).y > 0.99,
                "a rim triangle faced {:?}, so the well is visible only from \
                 underneath",
                winding_normal(face)
            );
            for point in face {
                let across = Vec2::new(point.x, point.z).length();
                assert!(
                    across >= 6.0 - 1e-3,
                    "a rim vertex {across:.2} from the middle is inside the \
                     mouth -- the hole has been covered over"
                );
            }
        }
        assert_normals_are_unit_length(&mesh);
    }

    /// The whole reason [`MeshBuilder::inward_wall_ring`] exists rather than
    /// reusing `wall_ring` with the points reversed.
    ///
    /// `wall_ring` normalises its winding through [`upward_ring`] before it
    /// builds anything, so a caller that hands it a reversed ring gets exactly
    /// the same outward-facing shaft back and back-face culling eats it. The
    /// same trap [`MeshBuilder::spire`] documents against `cone`.
    #[test]
    fn an_inward_wall_ring_faces_the_middle() {
        let mut builder = MeshBuilder::new();
        builder.inward_wall_ring(&ring(10.0, 12), 0.0, 5.0);
        let mesh = builder.build();

        for face in triangles(&mesh) {
            let normal = winding_normal(face);
            let outward = Vec3::new(
                (face[0].x + face[1].x + face[2].x) / 3.0,
                0.0,
                (face[0].z + face[1].z + face[2].z) / 3.0,
            )
            .normalize_or_zero();
            assert!(
                normal.dot(outward) < 0.0,
                "a shaft face at {outward:?} pointed outward as {normal:?}, so \
                 the inside of the well is culled away"
            );
            assert!(
                normal.y.abs() < 1e-3,
                "a shaft face is not vertical: {normal:?}"
            );
        }
        assert_normals_are_unit_length(&mesh);
    }

    /// The underside is what makes the map an object rather than a cut-out, so
    /// it has to actually reach the depth the manifest asked for.
    #[test]
    fn the_underside_hangs_to_the_depth_it_was_given() {
        let underside = underside();
        let mesh = disk_spire(&ring(500.0, 32), &underside);
        let lowest = mesh
            .attribute(Mesh::ATTRIBUTE_POSITION)
            .and_then(|values| values.as_float3())
            .expect("positions")
            .iter()
            .fold(f32::MAX, |low, point| low.min(point[1]));

        assert!(
            (lowest + underside.depth).abs() < 1e-3,
            "the spire reached {lowest}, not {}",
            -underside.depth
        );
        assert_normals_are_unit_length(&mesh);
    }

    /// The cliff meets the rock exactly, so neither has to be lifted off the
    /// other to keep the depth buffer honest.
    #[test]
    fn the_cliff_ends_where_the_rock_begins() {
        let underside = underside();
        let rim = ring(500.0, 32);
        let cliff = rim_cliff(&rim, underside.cliff);

        let (top, bottom) = cliff
            .attribute(Mesh::ATTRIBUTE_POSITION)
            .and_then(|values| values.as_float3())
            .expect("positions")
            .iter()
            .fold((f32::MIN, f32::MAX), |(top, bottom), point| {
                (top.max(point[1]), bottom.min(point[1]))
            });
        assert!((top - 0.0).abs() < 1e-3, "the cliff started at {top}");
        assert!((bottom + underside.cliff).abs() < 1e-3);

        let rock_top = disk_shelf(&rim, &underside)
            .attribute(Mesh::ATTRIBUTE_POSITION)
            .and_then(|values| values.as_float3())
            .expect("positions")
            .iter()
            .fold(f32::MIN, |top, point| top.max(point[1]));
        assert!(
            (rock_top - bottom).abs() < 1e-3,
            "the rock started at {rock_top} while the cliff ended at {bottom}"
        );
    }

    /// A world with no rim at all must be an empty mesh rather than a panic.
    #[test]
    fn an_underside_with_no_rim_builds_nothing() {
        assert_eq!(disk_shelf(&[], &underside()).count_vertices(), 0);
        assert_eq!(disk_spire(&[], &underside()).count_vertices(), 0);
        assert_eq!(rim_cliff(&[], 10.0).count_vertices(), 0);
    }

    /// The three pieces have to meet exactly, since each is a different colour
    /// and any gap between them shows the void straight through the world.
    #[test]
    fn the_shelf_meets_the_spire_where_it_meets_the_cliff() {
        let underside = underside();
        let rim = ring(500.0, 32);
        let (shelf_top, shelf_bottom) = span(&disk_shelf(&rim, &underside));
        let (spire_top, _) = span(&disk_spire(&rim, &underside));

        assert!((shelf_top + underside.cliff).abs() < 1e-3);
        assert!((shelf_bottom + underside.shelf).abs() < 1e-3);
        assert!(
            (spire_top - shelf_bottom).abs() < 1e-3,
            "the spire started at {spire_top} while the shelf ended at {shelf_bottom}"
        );
    }

    /// The highest and lowest points of a mesh.
    fn span(mesh: &Mesh) -> (f32, f32) {
        mesh.attribute(Mesh::ATTRIBUTE_POSITION)
            .and_then(|values| values.as_float3())
            .expect("positions")
            .iter()
            .fold((f32::MIN, f32::MAX), |(top, bottom), point| {
                (top.max(point[1]), bottom.min(point[1]))
            })
    }

    #[test]
    fn concave_outlines_stay_inside_themselves() {
        // An L-shape: a fan from the first vertex would spill across the notch.
        let outline = [
            Vec2::new(0.0, 0.0),
            Vec2::new(10.0, 0.0),
            Vec2::new(10.0, 4.0),
            Vec2::new(4.0, 4.0),
            Vec2::new(4.0, 10.0),
            Vec2::new(0.0, 10.0),
        ];
        let mesh = ground_polygon(&outline);
        let faces = triangles(&mesh);
        assert_eq!(faces.len(), outline.len() - 2);

        let area: f32 = faces
            .iter()
            .map(|face| {
                let first = Vec2::new(face[0].x, face[0].z);
                let second = Vec2::new(face[1].x, face[1].z);
                let third = Vec2::new(face[2].x, face[2].z);
                ((second - first).perp_dot(third - first) / 2.0).abs()
            })
            .sum();
        // The L covers 64 units. A fan over this outline covers far more.
        assert!((area - 64.0).abs() < 0.01, "triangulated area was {area}");
    }

    #[test]
    fn roads_face_upwards_in_every_direction() {
        for end in [
            Vec2::new(40.0, 0.0),
            Vec2::new(-40.0, 0.0),
            Vec2::new(0.0, 40.0),
            Vec2::new(0.0, -40.0),
            Vec2::new(30.0, -30.0),
        ] {
            let mesh = ribbon(&[Vec2::ZERO, end], 6.0);
            for face in triangles(&mesh) {
                assert!(
                    winding_normal(face).y > 0.99,
                    "a road heading {end:?} faced {:?}",
                    winding_normal(face)
                );
            }
        }
    }

    #[test]
    fn closed_shapes_face_outwards() {
        // Every triangle of a solid should point away from its centre. A face
        // wound the wrong way is culled and leaves a hole in the building.
        let mut builder = MeshBuilder::new();
        builder.box_from_to(Vec3::new(-1.0, 0.0, -2.0), Vec3::new(1.0, 3.0, 2.0));
        let box_mesh = builder.build();
        let center = Vec3::new(0.0, 1.5, 0.0);
        for face in triangles(&box_mesh) {
            let middle = (face[0] + face[1] + face[2]) / 3.0;
            assert!(
                winding_normal(face).dot(middle - center) > 0.0,
                "a box face pointed inwards"
            );
        }

        let mut builder = MeshBuilder::new();
        builder.cylinder(Vec2::ZERO, 2.0, 0.0, 5.0, 8);
        let cylinder = builder.build();
        for face in triangles(&cylinder) {
            let middle = (face[0] + face[1] + face[2]) / 3.0;
            assert!(
                winding_normal(face).dot(middle - Vec3::new(0.0, 2.5, 0.0)) > 0.0,
                "a cylinder face pointed inwards"
            );
        }

        let mut builder = MeshBuilder::new();
        builder.cone(Vec2::ZERO, 2.0, 0.0, 6.0, 9);
        let cone = builder.build();
        for face in triangles(&cone) {
            let middle = (face[0] + face[1] + face[2]) / 3.0;
            assert!(
                winding_normal(face).dot(middle - Vec3::new(0.0, 2.0, 0.0)) > 0.0,
                "a cone face pointed inwards"
            );
        }
    }

    #[test]
    fn every_archetype_shows_its_faces_to_the_camera() {
        // The camera is locked to one angle, so a systematically inverted
        // archetype would be invisible from the only side anyone ever sees.
        let view = Vec3::new(-1.0, -1.0, -1.0).normalize();
        for kind in BuildingKind::ALL {
            let shape = BuildingShape::new(kind, Vec2::new(4.0, 3.0), 6.0, 12);
            let meshes = build_building(shape);
            let faces = triangles(&meshes.walls);
            let facing = faces
                .iter()
                .filter(|face| winding_normal(**face).dot(-view) > 0.0)
                .count();
            assert!(
                facing * 3 >= faces.len(),
                "{kind:?} showed only {facing} of {} wall faces to the camera",
                faces.len()
            );
        }
    }
}
