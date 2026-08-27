//! Places natural scenery in the world-space manifest.
//!
//! The settlement layout owns buildings, roads, plazas, and wards. This module
//! fills only the land that remains: trees use a deterministic Poisson sampler
//! clipped to the real rim, while rim posts are laid out by arc length so
//! uneven outline vertices do not create uneven markers.

use crate::map::{MapColor, MapPlaza, MapRect, MapRoad, MapScenery, MapWard};

use crate::build::layout::stable_hash;

const TREE_TRUNK: MapColor = [82, 59, 37, 255];
const POST: MapColor = [111, 103, 79, 255];

const RIM_INSET: f32 = 10.0;
/// How far a canopy must stay clear of the nearest building lot.
///
/// A tree touching a wall reads as an accident rather than as planting, and at
/// the isometric angle a canopy overhanging a roof hides the holding behind it.
/// Woodland therefore keeps a full building's width away, which also means a
/// park only appears where a ward has genuine slack instead of trees threading
/// between the lots.
const BUILDING_CLEARANCE: f32 = 16.0;
/// Paving is public ground, so a tree only has to stay off it rather than well
/// back from it: a path lined with trees is what a park looks like.
const PAVING_CLEARANCE: f32 = 2.5;
const BASE_TREE_SPACING: f32 = 24.0;
const MIN_SPACING_FACTOR: f32 = 0.65;
const MAX_SPACING_FACTOR: f32 = 1.8;
const MIN_TREE_SPACING: f32 = BASE_TREE_SPACING * MIN_SPACING_FACTOR;
const MAX_TREE_SPACING: f32 = BASE_TREE_SPACING * MAX_SPACING_FACTOR * 1.8;
const NOISE_CELL: f32 = 125.0;
const BRIDSON_ATTEMPTS: usize = 30;
/// Darts thrown looking for somewhere the sampler can start growing again.
const RESEED_ATTEMPTS: usize = 600;
const TREE_AREA: f32 = 1_150.0;
/// The most trees that will ever be planted.
///
/// An island now grows with the repository it stands for, and woodland at a
/// fixed spacing would grow with its area — a realm of a dozen repositories
/// would ask for a hundred thousand trees and take the renderer down with it.
/// Past this point the whole island is thinned instead: the trees spread
/// further apart everywhere rather than the sampler stopping partway and
/// leaving half the land bare.
const MAX_TREES: usize = 14_000;
const POST_SPACING: f32 = 110.0;

/// The land, with everything already standing on it indexed for lookup.
///
/// A candidate tree has to be checked against every holding, ward, road, and
/// plaza near it. On a realm-sized island there are thousands of each, and
/// walking all of them per candidate is what turns scattering woodland into
/// the slowest part of building a world — so the fixed geometry is bucketed
/// once, up front.
struct Land<'a> {
    input: &'a SceneryInput<'a>,
    lots: RectIndex,
    wards: RectIndex,
    roads: SegmentIndex,
    /// How much further apart than natural the trees must stand.
    ///
    /// One on an island small enough to plant at full density, and above one
    /// once the tree budget is the binding constraint.
    thinning: f32,
}

impl Land<'_> {
    fn min_spacing(&self) -> f32 {
        MIN_TREE_SPACING * self.thinning
    }

    fn max_spacing(&self) -> f32 {
        MAX_TREE_SPACING * self.thinning
    }
}

/// Buckets rectangles into a uniform grid so a point only tests its neighbours.
struct RectIndex {
    origin: [f32; 2],
    cell: f32,
    columns: usize,
    rows: usize,
    cells: Vec<Vec<u32>>,
    rects: Vec<MapRect>,
}

impl RectIndex {
    /// `reach` is the furthest a query point may sit outside a rectangle and
    /// still be considered to touch it.
    fn new(rects: &[MapRect], bounds: Bounds, reach: f32) -> Self {
        let span = (bounds.max[0] - bounds.min[0]).max(bounds.max[1] - bounds.min[1]);
        // Enough cells to be selective, few enough that the grid itself stays
        // cheap on an island that may be tens of thousands of units across.
        let cell = (span / 220.0).max(reach * 2.0).max(1.0);
        let columns = (((bounds.max[0] - bounds.min[0]) / cell).ceil() as usize + 1).max(1);
        let rows = (((bounds.max[1] - bounds.min[1]) / cell).ceil() as usize + 1).max(1);
        let mut index = Self {
            origin: bounds.min,
            cell,
            columns,
            rows,
            cells: vec![Vec::new(); columns * rows],
            rects: rects.to_vec(),
        };
        for (position, rect) in rects.iter().enumerate() {
            let (min_column, min_row) = index.cell_of([rect.x - reach, rect.y - reach]);
            let (max_column, max_row) = index.cell_of([rect.max_x() + reach, rect.max_y() + reach]);
            for row in min_row..=max_row {
                for column in min_column..=max_column {
                    index.cells[row * columns + column].push(position as u32);
                }
            }
        }
        index
    }

    fn cell_of(&self, point: [f32; 2]) -> (usize, usize) {
        let column = ((point[0] - self.origin[0]) / self.cell).floor();
        let row = ((point[1] - self.origin[1]) / self.cell).floor();
        (
            (column.max(0.0) as usize).min(self.columns - 1),
            (row.max(0.0) as usize).min(self.rows - 1),
        )
    }

    fn near(&self, point: [f32; 2]) -> impl Iterator<Item = MapRect> + '_ {
        let (column, row) = self.cell_of(point);
        self.cells[row * self.columns + column]
            .iter()
            .map(|position| self.rects[*position as usize])
    }
}

/// Buckets road segments into the same kind of grid, for the same reason.
///
/// The network used to be a handful of lines between top-level wards, so
/// walking all of them per tree candidate cost nothing. It is now one segment
/// per split in every ward — thousands of them — and a saturated island throws
/// tens of thousands of candidates, so the linear scan would dominate the
/// whole build.
struct SegmentIndex {
    origin: [f32; 2],
    cell: f32,
    columns: usize,
    rows: usize,
    cells: Vec<Vec<u32>>,
    segments: Vec<Segment>,
}

#[derive(Clone, Copy, Debug)]
struct Segment {
    start: [f32; 2],
    end: [f32; 2],
    half_width: f32,
}

impl SegmentIndex {
    fn new(roads: &[MapRoad], bounds: Bounds, reach: f32) -> Self {
        let mut segments = Vec::new();
        for road in roads {
            let half_width = road.width.max(0.0) * 0.5;
            for pair in road.points.windows(2) {
                segments.push(Segment {
                    start: pair[0],
                    end: pair[1],
                    half_width,
                });
            }
        }

        let span = (bounds.max[0] - bounds.min[0]).max(bounds.max[1] - bounds.min[1]);
        let cell = (span / 220.0).max(reach * 2.0).max(1.0);
        let columns = (((bounds.max[0] - bounds.min[0]) / cell).ceil() as usize + 1).max(1);
        let rows = (((bounds.max[1] - bounds.min[1]) / cell).ceil() as usize + 1).max(1);
        let mut index = Self {
            origin: bounds.min,
            cell,
            columns,
            rows,
            cells: vec![Vec::new(); columns * rows],
            segments,
        };

        for position in 0..index.segments.len() {
            let segment = index.segments[position];
            // A segment's own width counts towards how far it reaches, so a
            // trunk road is found from further away than a garden lane.
            let padding = reach + segment.half_width;
            let min = [
                segment.start[0].min(segment.end[0]) - padding,
                segment.start[1].min(segment.end[1]) - padding,
            ];
            let max = [
                segment.start[0].max(segment.end[0]) + padding,
                segment.start[1].max(segment.end[1]) + padding,
            ];
            let (min_column, min_row) = index.cell_of(min);
            let (max_column, max_row) = index.cell_of(max);
            for row in min_row..=max_row {
                for column in min_column..=max_column {
                    index.cells[row * columns + column].push(position as u32);
                }
            }
        }
        index
    }

    fn cell_of(&self, point: [f32; 2]) -> (usize, usize) {
        let column = ((point[0] - self.origin[0]) / self.cell).floor();
        let row = ((point[1] - self.origin[1]) / self.cell).floor();
        (
            (column.max(0.0) as usize).min(self.columns - 1),
            (row.max(0.0) as usize).min(self.rows - 1),
        )
    }

    fn near(&self, point: [f32; 2]) -> impl Iterator<Item = Segment> + '_ {
        let (column, row) = self.cell_of(point);
        self.cells[row * self.columns + column]
            .iter()
            .map(|position| self.segments[*position as usize])
    }
}

/// Everything scenery placement needs from the scene builder.
///
/// Wards are passed only to classify park trees. They are deliberately not an
/// exclusion field, because genuinely unused ward interiors make the small
/// green pockets that dense repositories otherwise lack.
pub struct SceneryInput<'a> {
    /// The edge of the world, in world coordinates.
    pub rim: &'a [[f32; 2]],
    /// Every building lot already placed.
    pub building_lots: &'a [MapRect],
    pub roads: &'a [MapRoad],
    pub plazas: &'a [MapPlaza],
    /// Every ward at every depth.
    pub wards: &'a [MapWard],
    /// Mixed into the sampler seed so two different repositories do not get
    /// identical woodland.
    pub seed_key: &'a str,
}

#[derive(Clone, Copy, Debug)]
struct TreeCandidate {
    position: [f32; 2],
    height: f32,
    radius: f32,
    spacing: f32,
    foliage: MapColor,
}

#[derive(Clone, Copy, Debug)]
struct Bounds {
    min: [f32; 2],
    max: [f32; 2],
}

#[derive(Clone, Debug)]
struct SamplerGrid {
    origin: [f32; 2],
    cell_size: f32,
    width: usize,
    height: usize,
    cells: Vec<Option<usize>>,
}

/// Builds all manifest scenery in deterministic order.
///
/// Tree density is estimated from free land: the island polygon area minus the
/// sum of building lots, road ribbons, and plazas. That intentionally ignores
/// overlaps between those shapes, trading exact computational geometry for a
/// stable, cheap signal that still makes crowded repositories grow fewer trees.
pub fn scenery(input: SceneryInput<'_>) -> Vec<MapScenery> {
    let seed = seed_from_key(input.seed_key);
    let natural = natural_tree_count(&input);
    let target = natural.clamp(40, MAX_TREES);
    // Spacing is what the budget is spent through, not a hard stop on the
    // sampler. Thinning the whole island keeps the woodland even; truncating a
    // saturated scatter would leave the far side of a large realm bare.
    let thinning = (natural as f32 / target.max(1) as f32).sqrt().max(1.0);
    let Some(bounds) = bounds(input.rim) else {
        return Vec::new();
    };
    let reach = BUILDING_CLEARANCE + MAX_TREE_RADIUS;
    let land = Land {
        input: &input,
        lots: RectIndex::new(input.building_lots, bounds, reach),
        wards: RectIndex::new(
            &input.wards.iter().map(|ward| ward.rect).collect::<Vec<_>>(),
            bounds,
            0.0,
        ),
        roads: SegmentIndex::new(input.roads, bounds, MAX_TREE_RADIUS + PAVING_CLEARANCE),
        thinning,
    };
    let mut trees = poisson_trees(&land, bounds, seed);
    if trees.len() > target {
        trees.sort_by(|left, right| {
            tree_priority(*left, seed)
                .cmp(&tree_priority(*right, seed))
                .then_with(|| left.position[0].total_cmp(&right.position[0]))
                .then_with(|| left.position[1].total_cmp(&right.position[1]))
        });
        trees.truncate(target);
    }
    trees.sort_by(|left, right| {
        left.position[0]
            .total_cmp(&right.position[0])
            .then_with(|| left.position[1].total_cmp(&right.position[1]))
    });

    let mut output = Vec::with_capacity(trees.len() + input.rim.len());
    output.extend(trees.into_iter().map(|tree| MapScenery::Tree {
        position: tree.position,
        height: tree.height,
        radius: tree.radius,
        foliage: tree.foliage,
        trunk: TREE_TRUNK,
    }));
    output.extend(rim_posts(input.rim));
    output
}

/// Scatters trees by Bridson's Poisson-disk sampling, clipped to the island.
///
/// The sampler grows outward from a seed point, and a settlement is full of
/// barriers it cannot grow across: a row of holdings with their clearance
/// around them can wall off a whole side of the island. So whenever the growing
/// front dies out, a fresh seed is dropped somewhere still open and the sampler
/// carries on from there. Without that, one wall of buildings leaves everything
/// behind it bare — which is the arbitrary-looking scatter this replaced.
fn poisson_trees(land: &Land<'_>, bounds: Bounds, seed: u64) -> Vec<TreeCandidate> {
    let mut rng = Rng::new(seed ^ 0x6d2b_79f5_aa51_87d5);
    let mut grid = SamplerGrid::new(bounds, land.min_spacing() / 2.0_f32.sqrt());
    let mut trees = Vec::new();
    let mut active = Vec::new();

    // The scatter ends when the land is full, not when a counter runs out. The
    // ceiling is only a guard against a degenerate world, and sits well above
    // the tree budget the spacing was chosen to land on.
    let ceiling = MAX_TREES * 3;
    while trees.len() < ceiling {
        if active.is_empty() {
            let Some(tree) = open_ground(land, bounds, &trees, &grid, seed, &mut rng) else {
                break;
            };
            grid.insert(tree.position, trees.len());
            trees.push(tree);
            active.push(trees.len() - 1);
        }

        let active_index = rng.usize(active.len());
        let tree_index = active[active_index];
        let origin = trees[tree_index];
        let mut placed = false;

        for _ in 0..BRIDSON_ATTEMPTS {
            let angle = rng.f32() * std::f32::consts::TAU;
            let distance = origin.spacing * (1.0 + rng.f32());
            let point = [
                origin.position[0] + angle.cos() * distance,
                origin.position[1] + angle.sin() * distance,
            ];
            if point[0] < bounds.min[0]
                || point[0] > bounds.max[0]
                || point[1] < bounds.min[1]
                || point[1] > bounds.max[1]
            {
                continue;
            }
            let Some(tree) = candidate_at(point, land, seed) else {
                continue;
            };
            if clear_of_trees(&tree, &trees, &grid, land.max_spacing()) {
                grid.insert(tree.position, trees.len());
                trees.push(tree);
                active.push(trees.len() - 1);
                placed = true;
                break;
            }
        }

        if !placed {
            active.swap_remove(active_index);
        }
    }

    trees
}

/// Looks for somewhere the sampler could start again.
///
/// Dart throwing is a poor way to fill a region and a perfectly good way to
/// find one point in it, which is all this has to do. The attempt budget is
/// what ends the scatter: once the open ground left is small enough that this
/// many darts all miss, there is nothing worth planting.
fn open_ground(
    land: &Land<'_>,
    bounds: Bounds,
    trees: &[TreeCandidate],
    grid: &SamplerGrid,
    seed: u64,
    rng: &mut Rng,
) -> Option<TreeCandidate> {
    for _ in 0..RESEED_ATTEMPTS {
        let point = random_point(bounds, rng);
        let Some(tree) = candidate_at(point, land, seed) else {
            continue;
        };
        if clear_of_trees(&tree, trees, grid, land.max_spacing()) {
            return Some(tree);
        }
    }
    None
}

fn candidate_at(point: [f32; 2], land: &Land<'_>, seed: u64) -> Option<TreeCandidate> {
    let input = land.input;
    if !point_in_polygon(point, input.rim) {
        return None;
    }
    let rim_distance = distance_to_polyline(point, input.rim, true);
    if rim_distance < RIM_INSET {
        return None;
    }

    let in_ward = land.wards.near(point).any(|rect| rect.contains(point));
    let radius = tree_radius(point, in_ward, seed);
    if !clear_of_fixed_geometry(point, radius, land) {
        return None;
    }

    let mut spacing = BASE_TREE_SPACING
        * (MIN_SPACING_FACTOR
            + value_noise(point, seed) * (MAX_SPACING_FACTOR - MIN_SPACING_FACTOR));
    let rim_clearance = ((rim_distance - RIM_INSET) / 80.0).clamp(0.0, 1.0);
    spacing *= 1.0 + (1.0 - rim_clearance) * 0.8;
    spacing = (spacing * land.thinning)
        .max(radius * 2.05)
        .max(land.min_spacing());

    Some(TreeCandidate {
        position: point,
        height: tree_height(point, in_ward, seed),
        radius,
        spacing,
        foliage: foliage(point, seed),
    })
}

fn clear_of_fixed_geometry(point: [f32; 2], radius: f32, land: &Land<'_>) -> bool {
    let input = land.input;
    !land
        .lots
        .near(point)
        .any(|lot| rect_contains_padded(lot, point, radius + BUILDING_CLEARANCE))
        && !input
            .plazas
            .iter()
            .any(|plaza| rect_contains_padded(plaza.rect, point, radius + PAVING_CLEARANCE))
        && !land.roads.near(point).any(|segment| {
            distance_to_segment(point, segment.start, segment.end)
                <= segment.half_width + radius + PAVING_CLEARANCE
        })
}

fn clear_of_trees(
    tree: &TreeCandidate,
    trees: &[TreeCandidate],
    grid: &SamplerGrid,
    max_spacing: f32,
) -> bool {
    for index in grid.neighbours(tree.position, max_spacing) {
        let other = trees[index];
        let required = tree
            .spacing
            .max(other.spacing)
            .max(tree.radius + other.radius);
        if distance_squared(tree.position, other.position) < required * required {
            return false;
        }
    }
    true
}

/// How many trees the free land would hold at full density.
fn natural_tree_count(input: &SceneryInput<'_>) -> usize {
    let land = polygon_area(input.rim);
    if land <= 0.0 {
        return 0;
    }
    let building_area: f32 = input
        .building_lots
        .iter()
        // A lot takes its clearance out of the plantable land too, or the
        // target would ask for far more trees than the free ground can hold.
        .map(|rect| {
            (rect.width.max(0.0) + BUILDING_CLEARANCE * 2.0)
                * (rect.depth.max(0.0) + BUILDING_CLEARANCE * 2.0)
        })
        .sum();
    let road_area: f32 = input
        .roads
        .iter()
        .map(|road| road_length(road) * road.width.max(0.0))
        .sum();
    let plaza_area: f32 = input
        .plazas
        .iter()
        .map(|plaza| plaza.rect.width.max(0.0) * plaza.rect.depth.max(0.0))
        .sum();
    let free = (land - building_area - road_area - plaza_area).max(0.0);
    (free / TREE_AREA).round().max(0.0) as usize
}

fn rim_posts(rim: &[[f32; 2]]) -> Vec<MapScenery> {
    let perimeter = polygon_perimeter(rim);
    if perimeter == 0.0 {
        return Vec::new();
    }
    let count = (perimeter / POST_SPACING).round().max(1.0) as usize;
    let spacing = perimeter / count as f32;
    (0..count)
        .map(|index| MapScenery::Post {
            position: point_at_distance(rim, spacing * index as f32),
            height: 16.0,
            color: POST,
        })
        .collect()
}

fn seed_from_key(seed_key: &str) -> u64 {
    let high = stable_hash("repo-city-scenery") as u64;
    let low = stable_hash(seed_key) as u64;
    let seed = (high << 32) | low;
    if seed == 0 { 1 } else { seed }
}

/// The widest canopy [`tree_radius`] can return, which is how far outside a lot
/// the sampler has to look when it indexes the ground.
const MAX_TREE_RADIUS: f32 = 7.2;

fn tree_radius(point: [f32; 2], in_ward: bool, seed: u64) -> f32 {
    let value = visual_value(point, seed, 0x7a37_1d49);
    if in_ward {
        3.4 + value * 1.1
    } else {
        5.8 + value * 1.4
    }
}

fn tree_height(point: [f32; 2], in_ward: bool, seed: u64) -> f32 {
    let value = visual_value(point, seed, 0x91e1_c4af);
    if in_ward {
        13.0 + value * 6.0
    } else {
        20.0 + value * 8.0
    }
}

fn foliage(point: [f32; 2], seed: u64) -> MapColor {
    let tint = (visual_value(point, seed, 0xc6a4_a793) * 22.0).round() as u8;
    [55 + tint / 3, 91 + tint, 48 + tint / 4, 255]
}

fn visual_value(point: [f32; 2], seed: u64, salt: u64) -> f32 {
    unit_from_u64(mix64(
        seed ^ salt ^ ((point[0].to_bits() as u64) << 32) ^ point[1].to_bits() as u64,
    ))
}

fn value_noise(point: [f32; 2], seed: u64) -> f32 {
    let x = point[0] / NOISE_CELL;
    let y = point[1] / NOISE_CELL;
    let x0 = x.floor() as i32;
    let y0 = y.floor() as i32;
    let tx = smoothstep(x - x0 as f32);
    let ty = smoothstep(y - y0 as f32);

    let a = lattice_value(x0, y0, seed);
    let b = lattice_value(x0 + 1, y0, seed);
    let c = lattice_value(x0, y0 + 1, seed);
    let d = lattice_value(x0 + 1, y0 + 1, seed);
    lerp(lerp(a, b, tx), lerp(c, d, tx), ty)
}

fn lattice_value(x: i32, y: i32, seed: u64) -> f32 {
    unit_from_u64(mix64(
        seed ^ 0x4f1b_bcdc_b5aa_765d ^ ((x as u32 as u64) << 32) ^ y as u32 as u64,
    ))
}

fn tree_priority(tree: TreeCandidate, seed: u64) -> u64 {
    mix64(
        seed ^ 0xb1ae_35d5_115f_a139
            ^ tree.position[0].to_bits() as u64
            ^ ((tree.position[1].to_bits() as u64) << 32),
    )
}

fn point_in_polygon(point: [f32; 2], polygon: &[[f32; 2]]) -> bool {
    if polygon.len() < 3 {
        return false;
    }
    let mut inside = false;
    let mut previous = polygon[polygon.len() - 1];
    for &current in polygon {
        let crosses = (current[1] > point[1]) != (previous[1] > point[1]);
        if crosses {
            let x = (previous[0] - current[0]) * (point[1] - current[1])
                / (previous[1] - current[1])
                + current[0];
            if point[0] < x {
                inside = !inside;
            }
        }
        previous = current;
    }
    inside
}

fn polygon_area(polygon: &[[f32; 2]]) -> f32 {
    if polygon.len() < 3 {
        return 0.0;
    }
    let mut sum = 0.0;
    for index in 0..polygon.len() {
        let current = polygon[index];
        let next = polygon[(index + 1) % polygon.len()];
        sum += current[0] * next[1] - next[0] * current[1];
    }
    sum.abs() * 0.5
}

fn polygon_perimeter(polygon: &[[f32; 2]]) -> f32 {
    if polygon.len() < 2 {
        return 0.0;
    }
    (0..polygon.len())
        .map(|index| distance(polygon[index], polygon[(index + 1) % polygon.len()]))
        .sum()
}

fn point_at_distance(polygon: &[[f32; 2]], mut target: f32) -> [f32; 2] {
    for index in 0..polygon.len() {
        let start = polygon[index];
        let end = polygon[(index + 1) % polygon.len()];
        let length = distance(start, end);
        if target <= length || index == polygon.len() - 1 {
            let ratio = if length == 0.0 { 0.0 } else { target / length };
            return [lerp(start[0], end[0], ratio), lerp(start[1], end[1], ratio)];
        }
        target -= length;
    }
    polygon[0]
}

fn distance_to_polyline(point: [f32; 2], polygon: &[[f32; 2]], closed: bool) -> f32 {
    if polygon.len() < 2 {
        return f32::MAX;
    }
    let edge_count = if closed {
        polygon.len()
    } else {
        polygon.len() - 1
    };
    (0..edge_count)
        .map(|index| {
            distance_to_segment(point, polygon[index], polygon[(index + 1) % polygon.len()])
        })
        .fold(f32::MAX, f32::min)
}

fn distance_to_segment(point: [f32; 2], start: [f32; 2], end: [f32; 2]) -> f32 {
    let dx = end[0] - start[0];
    let dy = end[1] - start[1];
    let length_squared = dx * dx + dy * dy;
    if length_squared == 0.0 {
        return distance(point, start);
    }
    let t = (((point[0] - start[0]) * dx + (point[1] - start[1]) * dy) / length_squared)
        .clamp(0.0, 1.0);
    distance(point, [start[0] + dx * t, start[1] + dy * t])
}

fn road_length(road: &MapRoad) -> f32 {
    road.points
        .windows(2)
        .map(|segment| distance(segment[0], segment[1]))
        .sum()
}

fn rect_contains_padded(rect: MapRect, point: [f32; 2], padding: f32) -> bool {
    point[0] >= rect.x - padding
        && point[0] <= rect.max_x() + padding
        && point[1] >= rect.y - padding
        && point[1] <= rect.max_y() + padding
}

fn bounds(points: &[[f32; 2]]) -> Option<Bounds> {
    if points.is_empty() {
        return None;
    }
    let mut min = [f32::MAX; 2];
    let mut max = [f32::MIN; 2];
    for point in points {
        min[0] = min[0].min(point[0]);
        min[1] = min[1].min(point[1]);
        max[0] = max[0].max(point[0]);
        max[1] = max[1].max(point[1]);
    }
    Some(Bounds { min, max })
}

fn random_point(bounds: Bounds, rng: &mut Rng) -> [f32; 2] {
    [
        lerp(bounds.min[0], bounds.max[0], rng.f32()),
        lerp(bounds.min[1], bounds.max[1], rng.f32()),
    ]
}

fn distance(left: [f32; 2], right: [f32; 2]) -> f32 {
    distance_squared(left, right).sqrt()
}

fn distance_squared(left: [f32; 2], right: [f32; 2]) -> f32 {
    (left[0] - right[0]).powi(2) + (left[1] - right[1]).powi(2)
}

fn lerp(start: f32, end: f32, amount: f32) -> f32 {
    start + (end - start) * amount
}

fn smoothstep(value: f32) -> f32 {
    value * value * (3.0 - 2.0 * value)
}

fn unit_from_u64(value: u64) -> f32 {
    ((value >> 40) as u32) as f32 / (1u32 << 24) as f32
}

fn mix64(mut value: u64) -> u64 {
    value ^= value >> 30;
    value = value.wrapping_mul(0xbf58_476d_1ce4_e5b9);
    value ^= value >> 27;
    value = value.wrapping_mul(0x94d0_49bb_1331_11eb);
    value ^ (value >> 31)
}

impl SamplerGrid {
    fn new(bounds: Bounds, cell_size: f32) -> Self {
        let width = ((bounds.max[0] - bounds.min[0]) / cell_size)
            .ceil()
            .max(1.0) as usize;
        let height = ((bounds.max[1] - bounds.min[1]) / cell_size)
            .ceil()
            .max(1.0) as usize;
        Self {
            origin: bounds.min,
            cell_size,
            width,
            height,
            cells: vec![None; width * height],
        }
    }

    fn insert(&mut self, point: [f32; 2], index: usize) {
        if let Some(cell) = self.cell_index(point) {
            self.cells[cell] = Some(index);
        }
    }

    fn neighbours(&self, point: [f32; 2], radius: f32) -> Vec<usize> {
        let Some((cell_x, cell_y)) = self.cell(point) else {
            return Vec::new();
        };
        let reach = (radius / self.cell_size).ceil() as isize;
        let mut output = Vec::new();
        for y in cell_y - reach..=cell_y + reach {
            for x in cell_x - reach..=cell_x + reach {
                if x < 0 || y < 0 || x >= self.width as isize || y >= self.height as isize {
                    continue;
                }
                if let Some(index) = self.cells[y as usize * self.width + x as usize] {
                    output.push(index);
                }
            }
        }
        output
    }

    fn cell_index(&self, point: [f32; 2]) -> Option<usize> {
        let (x, y) = self.cell(point)?;
        if x < 0 || y < 0 || x >= self.width as isize || y >= self.height as isize {
            return None;
        }
        Some(y as usize * self.width + x as usize)
    }

    fn cell(&self, point: [f32; 2]) -> Option<(isize, isize)> {
        if !point[0].is_finite() || !point[1].is_finite() {
            return None;
        }
        Some((
            ((point[0] - self.origin[0]) / self.cell_size).floor() as isize,
            ((point[1] - self.origin[1]) / self.cell_size).floor() as isize,
        ))
    }
}

struct Rng {
    state: u64,
}

impl Rng {
    fn new(seed: u64) -> Self {
        Self { state: seed.max(1) }
    }

    fn next(&mut self) -> u64 {
        let mut value = self.state;
        value ^= value << 13;
        value ^= value >> 7;
        value ^= value << 17;
        self.state = value;
        value
    }

    fn f32(&mut self) -> f32 {
        unit_from_u64(self.next())
    }

    fn usize(&mut self, upper: usize) -> usize {
        (self.next() % upper as u64) as usize
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::map::RoadKind;

    const RIM: [[f32; 2]; 8] = [
        [70.0, 260.0],
        [170.0, 90.0],
        [415.0, 55.0],
        [735.0, 110.0],
        [900.0, 325.0],
        [835.0, 820.0],
        [520.0, 935.0],
        [135.0, 805.0],
    ];

    fn fixture<'a>(
        building_lots: &'a [MapRect],
        roads: &'a [MapRoad],
        plazas: &'a [MapPlaza],
        wards: &'a [MapWard],
        seed_key: &'a str,
    ) -> SceneryInput<'a> {
        SceneryInput {
            rim: &RIM,
            building_lots,
            roads,
            plazas,
            wards,
            seed_key,
        }
    }

    fn tree_positions(scenery: &[MapScenery]) -> Vec<([f32; 2], f32)> {
        scenery
            .iter()
            .filter_map(|item| match item {
                MapScenery::Tree {
                    position, radius, ..
                } => Some((*position, *radius)),
                MapScenery::Post { .. } => None,
            })
            .collect()
    }

    /// The straight-line gap between a point and the nearest edge of a rect,
    /// which is what a canopy actually has to clear.
    fn distance_to_rect(point: [f32; 2], rect: MapRect) -> f32 {
        let dx = (rect.x - point[0]).max(point[0] - rect.max_x()).max(0.0);
        let dy = (rect.y - point[1]).max(point[1] - rect.max_y()).max(0.0);
        (dx * dx + dy * dy).sqrt()
    }

    fn post_positions(scenery: &[MapScenery]) -> Vec<[f32; 2]> {
        scenery
            .iter()
            .filter_map(|item| match item {
                MapScenery::Post { position, .. } => Some(*position),
                MapScenery::Tree { .. } => None,
            })
            .collect()
    }

    #[test]
    fn no_tree_lands_inside_a_building_lot() {
        let lots = vec![
            MapRect {
                x: 260.0,
                y: 230.0,
                width: 170.0,
                depth: 180.0,
            },
            MapRect {
                x: 520.0,
                y: 500.0,
                width: 190.0,
                depth: 150.0,
            },
        ];

        let output = scenery(fixture(&lots, &[], &[], &[], "buildings"));
        let trees = tree_positions(&output);
        assert!(!trees.is_empty(), "the island should still be planted");

        for (position, radius) in trees {
            for lot in &lots {
                let gap = distance_to_rect(position, *lot);
                assert!(
                    gap >= radius + BUILDING_CLEARANCE,
                    "tree at {position:?} came within {gap} of a building lot"
                );
            }
        }
    }

    #[test]
    fn a_tree_keeps_its_distance_from_paving() {
        let roads = vec![MapRoad {
            kind: RoadKind::Ward,
            points: vec![[150.0, 300.0], [700.0, 340.0], [760.0, 700.0]],
            width: 9.0,

            traffic: 40,
            color: [0, 0, 0, 255],
            edge: [0, 0, 0, 255],
        }];
        let plazas = vec![MapPlaza {
            rect: MapRect {
                x: 300.0,
                y: 500.0,
                width: 140.0,
                depth: 120.0,
            },
            color: [0, 0, 0, 255],
        }];

        let output = scenery(fixture(&[], &roads, &plazas, &[], "paving"));

        for (position, radius) in tree_positions(&output) {
            for plaza in &plazas {
                assert!(
                    distance_to_rect(position, plaza.rect) >= radius + PAVING_CLEARANCE,
                    "tree at {position:?} stood on a plaza"
                );
            }
            for road in &roads {
                for segment in road.points.windows(2) {
                    let gap = distance_to_segment(position, segment[0], segment[1]);
                    assert!(
                        gap >= road.width * 0.5 + radius + PAVING_CLEARANCE,
                        "tree at {position:?} stood in a road"
                    );
                }
            }
        }
    }

    #[test]
    fn a_wall_of_holdings_does_not_leave_the_land_behind_it_bare() {
        // Poisson sampling grows outward from a seed, so a row of lots wide
        // enough to span the island can stop the front dead and leave
        // everything past it empty. Trees have to appear on both sides.
        let wall: Vec<MapRect> = (0..9)
            .map(|index| MapRect {
                x: 40.0 + index as f32 * 100.0,
                y: 470.0,
                width: 92.0,
                depth: 70.0,
            })
            .collect();

        let output = scenery(fixture(&wall, &[], &[], &[], "wall"));
        let trees = tree_positions(&output);
        let north = trees.iter().filter(|(p, _)| p[1] < 470.0).count();
        let south = trees.iter().filter(|(p, _)| p[1] > 540.0).count();

        assert!(
            north >= 10 && south >= 10,
            "a wall of holdings split the woodland: {north} north, {south} south"
        );
    }

    #[test]
    fn no_tree_lands_outside_the_rim_polygon() {
        let output = scenery(fixture(&[], &[], &[], &[], "rim"));

        for (position, _) in tree_positions(&output) {
            assert!(
                point_in_polygon(position, &RIM),
                "tree at {position:?} escaped the island"
            );
        }
    }

    #[test]
    fn every_quadrant_gets_a_reasonable_share_of_trees() {
        let output = scenery(fixture(&[], &[], &[], &[], "quadrants"));
        let trees = tree_positions(&output);
        let bounds = bounds(&RIM).unwrap();
        let mid_x = (bounds.min[0] + bounds.max[0]) * 0.5;
        let mid_y = (bounds.min[1] + bounds.max[1]) * 0.5;
        let mut quadrants = [0usize; 4];
        for (position, _) in &trees {
            let index = match (position[0] >= mid_x, position[1] >= mid_y) {
                (false, false) => 0,
                (true, false) => 1,
                (false, true) => 2,
                (true, true) => 3,
            };
            quadrants[index] += 1;
        }
        let minimum = (trees.len() / 10).max(8);

        assert!(
            quadrants.iter().all(|count| *count >= minimum),
            "quadrant distribution {quadrants:?} was too uneven for {} trees",
            trees.len()
        );
    }

    #[test]
    fn a_realm_sized_island_is_planted_all_over() {
        // An island now grows with the repositories on it, and the tree budget
        // does not. The thing that must not happen is the sampler filling one
        // corner at full density and running out before it reaches the rest —
        // so density is traded away evenly, across the whole coastline.
        let huge: Vec<[f32; 2]> = RIM.iter().map(|[x, y]| [x * 18.0, y * 18.0]).collect();
        let input = SceneryInput {
            rim: &huge,
            building_lots: &[],
            roads: &[],
            plazas: &[],
            wards: &[],
            seed_key: "a realm",
        };
        let output = scenery(input);
        let trees = tree_positions(&output);

        assert!(
            trees.len() > MAX_TREES / 2,
            "only {} trees grew on a realm-sized island",
            trees.len()
        );
        assert!(
            trees.len() <= MAX_TREES,
            "{} trees is past the budget",
            trees.len()
        );

        let bounds = bounds(&huge).unwrap();
        let mid_x = (bounds.min[0] + bounds.max[0]) * 0.5;
        let mid_y = (bounds.min[1] + bounds.max[1]) * 0.5;
        let mut quadrants = [0usize; 4];
        for (position, _) in &trees {
            let index = match (position[0] >= mid_x, position[1] >= mid_y) {
                (false, false) => 0,
                (true, false) => 1,
                (false, true) => 2,
                (true, true) => 3,
            };
            quadrants[index] += 1;
        }
        let minimum = (trees.len() / 10).max(8);
        assert!(
            quadrants.iter().all(|count| *count >= minimum),
            "quadrant distribution {quadrants:?} left part of the island bare"
        );
    }

    #[test]
    fn minimum_spacing_is_honoured() {
        let output = scenery(fixture(&[], &[], &[], &[], "spacing"));
        let trees = tree_positions(&output);
        assert!(!trees.is_empty());

        for (index, (left, _)) in trees.iter().enumerate() {
            for (right, _) in trees.iter().skip(index + 1) {
                let gap = distance(*left, *right);
                assert!(
                    gap + 1e-3 >= MIN_TREE_SPACING,
                    "trees at {left:?} and {right:?} were only {gap} apart"
                );
            }
        }
    }

    #[test]
    fn scenery_is_deterministic_for_the_same_input() {
        let roads = vec![MapRoad {
            kind: RoadKind::Ward,
            points: vec![[120.0, 420.0], [820.0, 440.0]],
            width: 8.0,

            traffic: 32,
            color: [1, 2, 3, 255],
            edge: [4, 5, 6, 255],
        }];

        let first = scenery(fixture(&[], &roads, &[], &[], "deterministic"));
        let second = scenery(fixture(&[], &roads, &[], &[], "deterministic"));

        assert_eq!(first, second);
    }

    #[test]
    fn different_seed_keys_produce_different_woodland() {
        let first = tree_positions(&scenery(fixture(&[], &[], &[], &[], "alpha")));
        let second = tree_positions(&scenery(fixture(&[], &[], &[], &[], "beta")));

        assert_ne!(first, second);
    }

    #[test]
    fn density_responds_to_free_land() {
        let mut lots = Vec::new();
        for row in 0..5 {
            for column in 0..5 {
                lots.push(MapRect {
                    x: 170.0 + column as f32 * 120.0,
                    y: 160.0 + row as f32 * 120.0,
                    width: 92.0,
                    depth: 92.0,
                });
            }
        }

        let open = tree_positions(&scenery(fixture(&[], &[], &[], &[], "density"))).len();
        let crowded = tree_positions(&scenery(fixture(&lots, &[], &[], &[], "density"))).len();

        assert!(
            crowded * 5 < open * 4,
            "crowded island kept {crowded} trees versus {open} on open land"
        );
    }

    #[test]
    fn trees_never_intersect_each_others_canopies() {
        let output = scenery(fixture(&[], &[], &[], &[], "canopies"));
        let trees = tree_positions(&output);

        for (index, (left, left_radius)) in trees.iter().enumerate() {
            for (right, right_radius) in trees.iter().skip(index + 1) {
                let gap = distance(*left, *right);
                let required = left_radius + right_radius;
                assert!(
                    gap + 1e-3 >= required,
                    "canopies at {left:?} and {right:?} overlap: {gap} < {required}"
                );
            }
        }
    }

    #[test]
    fn posts_are_evenly_spaced_along_the_rim() {
        let output = scenery(fixture(&[], &[], &[], &[], "posts"));
        let posts = post_positions(&output);
        let perimeter = polygon_perimeter(&RIM);
        let expected = perimeter / posts.len() as f32;

        for index in 0..posts.len() {
            let start = distance_along_polygon(&RIM, posts[index]);
            let end = distance_along_polygon(&RIM, posts[(index + 1) % posts.len()]);
            let interval = if end >= start {
                end - start
            } else {
                perimeter - start + end
            };
            assert!(
                (interval - expected).abs() < 0.75,
                "post interval {interval} differed from expected {expected}"
            );
        }
    }

    fn distance_along_polygon(polygon: &[[f32; 2]], point: [f32; 2]) -> f32 {
        let mut travelled = 0.0;
        for index in 0..polygon.len() {
            let start = polygon[index];
            let end = polygon[(index + 1) % polygon.len()];
            let segment = distance(start, end);
            if distance_to_segment(point, start, end) < 0.01 {
                return travelled + distance(start, point);
            }
            travelled += segment;
        }
        travelled
    }
}
