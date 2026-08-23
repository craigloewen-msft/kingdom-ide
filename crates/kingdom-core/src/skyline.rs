//! Turning a project's folder tree into a city skyline.
//!
//! This lives in `kingdom-core`, beside [`crate::layout`], for the same reason
//! that city placement does: a building must land on the **same spot on every
//! render and every reload**. The King navigates by spatial memory — "the tall
//! amber tower on the left of the auth district" — and that memory is worthless
//! if the skyline reshuffles. Being a pure function of the file tree makes the
//! stability testable rather than hoped for.
//!
//! ## What the geometry means
//!
//! - **Area is proportional to code mass.** Placement is a squarified treemap, so
//!   a folder holding half the project's code covers half the city. This is the
//!   feature's headline promise: the map shows where the code actually lives.
//!   Note *code*, not bytes -- see [`mass`], without which a single 40 MB video
//!   would outweigh an entire `src/` tree and the map would answer the wrong
//!   question.
//! - **Height also rises with mass**, compressed by a power curve. Area and
//!   height reinforce each other so the dominant module reads instantly, from
//!   any zoom, without the King decoding a legend.
//! - **Nothing is silently dropped.** Files pruned by the caps below are
//!   aggregated into a *commons* lot that still carries their count and their
//!   weight, so a huge folder always looks huge. A map that quietly under-reports
//!   mass would be worse than no map.

use crate::model::{Building, District, Ward};
use std::collections::BTreeSet;

// ---------------------------------------------------------------------------
// Caps
// ---------------------------------------------------------------------------

/// How many folder levels get their own plate before the rest is flattened.
///
/// Beyond three the plates are smaller than their own border and read as noise.
const MAX_DEPTH: usize = 3;

/// Ceiling on individually drawn buildings per city.
///
/// A 40-city kingdom at 160 each is already ~6,400 SVG nodes; without this cap
/// a single monorepo could stall the whole map.
const MAX_BUILDINGS: usize = 160;

/// Fraction of the city's layout radius used for building.
///
/// Comfortably below `1/sqrt(2)` (~0.707), the largest square that fits inside
/// the circle. Because [`crate::layout::spiral_layout`] guarantees city circles
/// never overlap, keeping every footprint inside the circle extends that
/// guarantee to buildings for free — no building can ever stray into a
/// neighbouring city. Pinned by `buildings_stay_inside_their_city`.
const BUILD_EXTENT: f64 = 0.68;

/// Bytes a file is assumed to be worth when weighting by count instead.
///
/// Guarantees a non-zero weight for empty files, so they still get a lot rather
/// than vanishing from the map.
const EMPTY_FILE_WEIGHT: f64 = 64.0;

/// Ceiling on how much a non-code file may weigh.
///
/// Without this the map answers the wrong question. A single 40 MB demo video or
/// a 2 MB `Cargo.lock` outweighs every source file in the project put together,
/// so the city becomes a monument to its assets and the King cannot see the code
/// at all. Capping keeps such files visible -- they are really there, and a
/// folder full of them still reads as populated -- while stopping them from
/// dominating area, height, or the choice of landmark.
const NON_CODE_CEILING: u64 = 8_192;

/// True for wards that represent hand-written program logic.
fn is_code(ward: Ward) -> bool {
    matches!(
        ward,
        Ward::Rust | Ward::Web | Ward::Python | Ward::Go | Ward::Systems | Ward::Shell
    )
}

/// The weight a file carries on the map: its size, capped unless it is code.
///
/// This is the difference between "where are my bytes?" and "where is my code?",
/// and only the second is worth a map.
fn mass(ward: Ward, bulk: u64) -> u64 {
    if is_code(ward) {
        bulk
    } else {
        bulk.min(NON_CODE_CEILING)
    }
}

// ---------------------------------------------------------------------------
// Output
// ---------------------------------------------------------------------------

/// What a lot stands for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LotKind {
    /// A single file.
    Tower,
    /// Many files, aggregated because of the caps above.
    Commons,
}

/// A placed building, in city-local coordinates (origin = city centre).
#[derive(Debug, Clone, PartialEq)]
pub struct Lot {
    /// Path relative to the city root; the join key for [`crate::Plan`] touches.
    pub path: String,
    pub name: String,
    /// Centre of the footprint.
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub depth: f64,
    pub height: f64,
    pub ward: Ward,
    pub kind: LotKind,
    /// How many real files this lot represents: 1 for a tower, N for a commons.
    pub files: usize,
}

impl Lot {
    /// Distance from the viewer, used to break ties in the occlusion sort.
    ///
    /// This is only a heuristic on its own: buildings differ wildly in size, so
    /// comparing centres gets dense clusters wrong. [`order_for_painting`] uses
    /// it purely as a tie-break behind the exact rule.
    pub fn depth_key(&self) -> f64 {
        (self.x + self.width / 2.0) + (self.y + self.depth / 2.0)
    }

    fn x0(&self) -> f64 {
        self.x - self.width / 2.0
    }

    fn x1(&self) -> f64 {
        self.x + self.width / 2.0
    }

    fn y0(&self) -> f64 {
        self.y - self.depth / 2.0
    }

    fn y1(&self) -> f64 {
        self.y + self.depth / 2.0
    }
}

/// Orders lots back to front so that nearer buildings overdraw farther ones.
///
/// SVG has no depth buffer, so draw order *is* the depth cue; get it wrong and
/// a far tower slices through a near one, which reads instantly as broken.
///
/// For axis-aligned boxes on an isometric grid the order is exact rather than
/// approximate. `a` is strictly behind `b` in three cases:
///
/// ```text
///   share y-span and a.x1 <= b.x0              =>  a is behind b
///   share x-span and a.y1 <= b.y0              =>  a is behind b
///   a.x1 <= b.x0 and a.y1 <= b.y0  (diagonal)  =>  a is behind b
/// ```
///
/// The third case is easy to miss and was: two boxes offset on *both* axes share
/// no span, yet a tall one still rises into the other's screen area, so they do
/// overlap and do need ordering.
///
/// The remaining arrangement -- `a` lower on one axis but higher on the other --
/// needs no constraint. Isometric screen x is `(x - y)`, so such boxes sit on
/// opposite sides of the screen and cannot overlap.
///
/// The constraints form a DAG, which a topological sort resolves; ties are
/// broken by distance and then path so the result stays deterministic.
fn order_for_painting(lots: &mut Vec<Lot>) {
    let n = lots.len();
    if n < 2 {
        return;
    }

    // `behind[a]` lists the lots that must be drawn after `a`.
    let mut behind: Vec<Vec<usize>> = vec![Vec::new(); n];
    let mut incoming = vec![0usize; n];

    // Footprints never overlap (see `buildings_never_overlap_and_stay_inside_
    // their_city`), so at most one of the two directions can hold per pair.
    for i in 0..n {
        for j in (i + 1)..n {
            let (a, b) = (&lots[i], &lots[j]);
            let shares_y = a.y0() < b.y1() && b.y0() < a.y1();
            let shares_x = a.x0() < b.x1() && b.x0() < a.x1();
            let a_lower_x = a.x1() <= b.x0();
            let b_lower_x = b.x1() <= a.x0();
            let a_lower_y = a.y1() <= b.y0();
            let b_lower_y = b.y1() <= a.y0();

            let a_behind =
                (shares_y && a_lower_x) || (shares_x && a_lower_y) || (a_lower_x && a_lower_y);
            let b_behind =
                (shares_y && b_lower_x) || (shares_x && b_lower_y) || (b_lower_x && b_lower_y);

            let order = match (a_behind, b_behind) {
                (true, false) => Some((i, j)),
                (false, true) => Some((j, i)),
                _ => None,
            };

            if let Some((back, front)) = order {
                behind[back].push(front);
                incoming[front] += 1;
            }
        }
    }

    // Kahn's algorithm. Among the lots that are currently free of constraints,
    // always take the farthest, so unconstrained pairs still read naturally.
    let mut ready: Vec<usize> = (0..n).filter(|i| incoming[*i] == 0).collect();
    let mut out: Vec<usize> = Vec::with_capacity(n);
    let mut placed = vec![false; n];

    while !ready.is_empty() {
        let pick = ready
            .iter()
            .enumerate()
            .min_by(|(_, a), (_, b)| {
                lots[**a]
                    .depth_key()
                    .partial_cmp(&lots[**b].depth_key())
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then_with(|| lots[**a].path.cmp(&lots[**b].path))
            })
            .map(|(idx, _)| idx)
            .expect("ready is non-empty");

        let node = ready.swap_remove(pick);
        out.push(node);
        placed[node] = true;

        for &next in &behind[node] {
            incoming[next] -= 1;
            if incoming[next] == 0 {
                ready.push(next);
            }
        }
    }

    // A cycle would mean the non-overlap invariant was violated. Rather than
    // drop buildings -- which would make the map lie about the code -- append
    // whatever is left in distance order.
    if out.len() < n {
        let mut rest: Vec<usize> = (0..n).filter(|i| !placed[*i]).collect();
        rest.sort_by(|a, b| {
            lots[*a]
                .depth_key()
                .partial_cmp(&lots[*b].depth_key())
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| lots[*a].path.cmp(&lots[*b].path))
        });
        out.extend(rest);
    }

    let mut reordered: Vec<Option<Lot>> = lots.drain(..).map(Some).collect();
    for i in out {
        lots.push(reordered[i].take().expect("each lot is emitted once"));
    }
}

/// A district's ground plate: the folder rendered as a plot of land.
#[derive(Debug, Clone, PartialEq)]
pub struct Plate {
    pub path: String,
    pub name: String,
    /// Centre of the plate.
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub depth: f64,
    /// Nesting level; 0 is the city's own root plate.
    pub level: usize,
    pub files: usize,
    /// The ward holding the most bytes here, used to tint the plate.
    pub ward: Ward,
}

/// A city's fully laid-out skyline.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Skyline {
    /// Buildings, pre-sorted back-to-front for painter's-algorithm rendering.
    pub lots: Vec<Lot>,
    /// District plates, outermost first.
    pub plates: Vec<Plate>,
    /// Index into `lots` of the landmark building: the single largest file.
    /// Rendered as the city's cathedral so "where is the bulk?" is answerable
    /// at a glance.
    pub cathedral: Option<usize>,
    /// Half-width of the buildable square, in city-local units.
    pub half_extent: f64,
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

/// Lays out a project's folder tree as a city of the given radius.
///
/// `radius` is the city's footprint from [`crate::layout::CityPlacement`].
pub fn build_skyline(root: &District, radius: f64) -> Skyline {
    let half = radius * BUILD_EXTENT;
    let mut skyline = Skyline {
        half_extent: half,
        ..Default::default()
    };

    let trimmed = trim(root);
    if trimmed.total_files() == 0 {
        return skyline;
    }

    // Height is normalised against the city's own largest file, so a small
    // library still gets a legible skyline instead of a uniform pancake.
    let tallest = trimmed.max_bulk().max(1) as f64;

    let ground = Rect {
        x: -half,
        y: -half,
        w: half * 2.0,
        h: half * 2.0,
    };

    lay_district(&trimmed, ground, 0, radius, tallest, &mut skyline);

    // SVG has no depth buffer, so draw order carries the whole 3D illusion.
    order_for_painting(&mut skyline.lots);

    // The landmark answers "where is the bulk of my code?", so it must be code:
    // a project's largest file is very often a lockfile or a demo video, and
    // crowning one of those would point the King at the least interesting thing
    // in the city.
    skyline.cathedral = skyline
        .lots
        .iter()
        .enumerate()
        .filter(|(_, l)| l.kind == LotKind::Tower)
        .max_by(|(_, a), (_, b)| {
            is_code(a.ward)
                .cmp(&is_code(b.ward))
                .then_with(|| {
                    a.height
                        .partial_cmp(&b.height)
                        .unwrap_or(std::cmp::Ordering::Equal)
                })
                .then_with(|| b.path.cmp(&a.path))
        })
        .map(|(i, _)| i);

    skyline
}

/// The isometric projection used by the map.
///
/// A true 30-degree isometric. The transform is affine, so footprints that do
/// not overlap on the ground plane cannot overlap once projected — the
/// non-overlap guarantee survives the change of view, and only `z` can occlude,
/// which the back-to-front sort of [`Skyline::lots`] handles.
pub fn iso(x: f64, y: f64, z: f64) -> (f64, f64) {
    const COS30: f64 = 0.866_025_403_784_438_6;
    ((x - y) * COS30, (x + y) * 0.5 - z)
}

// ---------------------------------------------------------------------------
// Placement
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq)]
struct Rect {
    x: f64,
    y: f64,
    w: f64,
    h: f64,
}

impl Rect {
    fn cx(&self) -> f64 {
        self.x + self.w / 2.0
    }

    fn cy(&self) -> f64 {
        self.y + self.h / 2.0
    }

    /// Shrinks the rect on all sides, never past collapsing.
    fn inset(&self, pad: f64) -> Rect {
        let pad = pad.min(self.w / 2.5).min(self.h / 2.5).max(0.0);
        Rect {
            x: self.x + pad,
            y: self.y + pad,
            w: self.w - pad * 2.0,
            h: self.h - pad * 2.0,
        }
    }
}

fn lay_district(
    district: &Trimmed,
    cell: Rect,
    level: usize,
    radius: f64,
    tallest: f64,
    out: &mut Skyline,
) {
    let plate = cell.inset(plate_pad(cell, level));

    out.plates.push(Plate {
        path: district.path.clone(),
        name: district.name.clone(),
        x: plate.cx(),
        y: plate.cy(),
        width: plate.w,
        depth: plate.h,
        level,
        files: district.total_files(),
        ward: district.dominant_ward(),
    });

    // Children compete for area with this district's own files, which are
    // treated as one more claimant so a folder that is mostly loose files still
    // reads as heavy against its sub-folders.
    let own_weight = district.own_weight();
    let mut weights: Vec<f64> = district.children.iter().map(Trimmed::weight).collect();
    if own_weight > 0.0 {
        weights.push(own_weight);
    }

    let cells = squarify(&weights, plate);

    for (child, cell) in district.children.iter().zip(&cells) {
        lay_district(child, *cell, level + 1, radius, tallest, out);
    }

    if own_weight > 0.0 {
        if let Some(cell) = cells.get(district.children.len()) {
            lay_buildings(district, *cell, radius, tallest, out);
        }
    }
}

/// Places one district's own files inside its cell, again by treemap so that
/// footprint area stays proportional to file size.
fn lay_buildings(district: &Trimmed, cell: Rect, radius: f64, tallest: f64, out: &mut Skyline) {
    let mut weights: Vec<f64> = district
        .buildings
        .iter()
        .map(|b| (mass(b.ward, b.bulk) as f64).max(EMPTY_FILE_WEIGHT))
        .collect();

    let has_commons = district.extra_files > 0;
    if has_commons {
        weights.push(district.extra_weight());
    }

    let cells = squarify(&weights, cell);

    for (building, cell) in district.buildings.iter().zip(&cells) {
        let foot = footprint(*cell);
        out.lots.push(Lot {
            path: building.path.clone(),
            name: building.name.clone(),
            x: foot.cx(),
            y: foot.cy(),
            width: foot.w,
            depth: foot.h,
            height: height_for(mass(building.ward, building.bulk), tallest, radius),
            ward: building.ward,
            kind: LotKind::Tower,
            files: 1,
        });
    }

    if has_commons {
        if let Some(cell) = cells.get(district.buildings.len()) {
            let foot = footprint(*cell);
            // The commons stands for many files at once, so it is sized by their
            // mean rather than their sum: a low, broad block, not a false spire.
            let mean = district.extra_bulk / district.extra_files.max(1) as u64;
            out.lots.push(Lot {
                path: format!("{}\u{1f}commons", district.path),
                name: format!("{} more files", district.extra_files),
                x: foot.cx(),
                y: foot.cy(),
                width: foot.w,
                depth: foot.h,
                height: height_for(mean, tallest, radius) * 0.6,
                ward: district.extra_ward,
                kind: LotKind::Commons,
                files: district.extra_files,
            });
        }
    }
}

/// The building itself: inset within its treemap cell and squared up a little,
/// because a skyline of thin slivers reads as noise rather than as a city.
///
/// Staying inside the cell is what keeps buildings from overlapping.
fn footprint(cell: Rect) -> Rect {
    let gap = (cell.w.min(cell.h) * 0.24).clamp(0.25, 2.6);

    let mut w = (cell.w - gap).max(cell.w * 0.4);
    let mut d = (cell.h - gap).max(cell.h * 0.4);

    const MAX_ASPECT: f64 = 2.4;
    if w > d * MAX_ASPECT {
        w = d * MAX_ASPECT;
    }
    if d > w * MAX_ASPECT {
        d = w * MAX_ASPECT;
    }

    Rect {
        x: cell.cx() - w / 2.0,
        y: cell.cy() - d / 2.0,
        w,
        h: d,
    }
}

/// Padding around a district plate, tightening as nesting deepens.
fn plate_pad(cell: Rect, level: usize) -> f64 {
    let scale = match level {
        0 => 0.02,
        1 => 0.05,
        _ => 0.07,
    };
    (cell.w.min(cell.h) * scale).clamp(0.4, 5.0)
}

/// Maps a file's size to a tower height.
///
/// The power curve compresses the long tail: real projects have one enormous
/// lockfile and hundreds of small modules, and a linear scale would render the
/// latter as a flat plain. The ceiling is generous because vertical presence is
/// what makes a city read as a city rather than as a patterned floor.
fn height_for(bulk: u64, tallest: f64, radius: f64) -> f64 {
    let norm = (bulk as f64 / tallest).clamp(0.0, 1.0);
    let min = radius * 0.09;
    let max = radius * 0.95;
    min + norm.powf(0.42) * (max - min)
}

// ---------------------------------------------------------------------------
// Squarified treemap
// ---------------------------------------------------------------------------

/// Splits `rect` among `weights`, keeping cells as close to square as possible.
///
/// Bruls, Huizing & van Wijk's squarified treemap. Area stays proportional to
/// weight — the property the whole visualisation rests on — while avoiding the
/// long slivers a naive slice-and-dice produces. Returns one rect per weight,
/// in input order.
fn squarify(weights: &[f64], rect: Rect) -> Vec<Rect> {
    let mut out = vec![
        Rect {
            x: rect.x,
            y: rect.y,
            w: 0.0,
            h: 0.0
        };
        weights.len()
    ];

    let total: f64 = weights.iter().sum();
    if total <= 0.0 || rect.w <= 0.0 || rect.h <= 0.0 {
        return out;
    }

    // Work in areas rather than weights so row thickness falls out directly.
    let scale = (rect.w * rect.h) / total;
    let areas: Vec<f64> = weights.iter().map(|w| w * scale).collect();

    // Descending order is what makes the squarified heuristic work; the
    // permutation is undone when writing results back.
    let mut order: Vec<usize> = (0..areas.len()).collect();
    order.sort_by(|&a, &b| {
        areas[b]
            .partial_cmp(&areas[a])
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.cmp(&b))
    });

    let mut free = rect;
    let mut i = 0;

    while i < order.len() {
        let side = free.w.min(free.h);
        if side <= 0.0 {
            break;
        }

        // Grow the row while doing so improves its worst aspect ratio.
        let mut end = i + 1;
        let mut sum = areas[order[i]];
        let mut worst_now = worst_aspect(sum, areas[order[i]], areas[order[i]], side);

        while end < order.len() {
            let next = sum + areas[order[end]];
            // Areas are sorted descending, so the row's max is its first entry
            // and its min is whatever we are about to append.
            let candidate = worst_aspect(next, areas[order[i]], areas[order[end]], side);
            if candidate > worst_now {
                break;
            }
            worst_now = candidate;
            sum = next;
            end += 1;
        }

        let thickness = if side > 0.0 { sum / side } else { 0.0 };
        let horizontal = free.w >= free.h;
        let mut offset = 0.0;

        for &idx in &order[i..end] {
            let extent = if sum > 0.0 {
                areas[idx] / thickness
            } else {
                0.0
            };
            out[idx] = if horizontal {
                Rect {
                    x: free.x,
                    y: free.y + offset,
                    w: thickness,
                    h: extent,
                }
            } else {
                Rect {
                    x: free.x + offset,
                    y: free.y,
                    w: extent,
                    h: thickness,
                }
            };
            offset += extent;
        }

        if horizontal {
            free.x += thickness;
            free.w -= thickness;
        } else {
            free.y += thickness;
            free.h -= thickness;
        }

        i = end;
    }

    out
}

/// Worst aspect ratio in a row of the given total area, bounded by `side`.
fn worst_aspect(sum: f64, max: f64, min: f64, side: f64) -> f64 {
    if sum <= 0.0 || min <= 0.0 || side <= 0.0 {
        return f64::MAX;
    }
    let s2 = side * side;
    let sum2 = sum * sum;
    ((s2 * max) / sum2).max(sum2 / (s2 * min))
}

// ---------------------------------------------------------------------------
// Trimming: applying the caps without losing mass
// ---------------------------------------------------------------------------

/// A district reduced to what the map will actually draw.
#[derive(Debug, Clone)]
struct Trimmed {
    name: String,
    path: String,
    buildings: Vec<Building>,
    children: Vec<Trimmed>,
    /// Files folded away by the caps, still counted and still weighed.
    extra_files: usize,
    extra_bulk: u64,
    extra_ward: Ward,
}

impl Trimmed {
    fn total_files(&self) -> usize {
        self.buildings.len()
            + self.extra_files
            + self
                .children
                .iter()
                .map(Trimmed::total_files)
                .sum::<usize>()
    }

    fn own_bulk(&self) -> u64 {
        self.buildings
            .iter()
            .map(|b| mass(b.ward, b.bulk))
            .sum::<u64>()
            + self.extra_bulk
    }

    fn total_bulk(&self) -> u64 {
        self.own_bulk() + self.children.iter().map(Trimmed::total_bulk).sum::<u64>()
    }

    fn max_bulk(&self) -> u64 {
        self.buildings
            .iter()
            .map(|b| mass(b.ward, b.bulk))
            .chain(self.children.iter().map(Trimmed::max_bulk))
            .max()
            .unwrap_or(0)
    }

    /// Weight of this district's own files, floored by count so that even a
    /// folder of empty files still claims ground.
    fn own_weight(&self) -> f64 {
        let files = self.buildings.len() + self.extra_files;
        if files == 0 {
            return 0.0;
        }
        (self.own_bulk() as f64).max(files as f64 * EMPTY_FILE_WEIGHT)
    }

    fn extra_weight(&self) -> f64 {
        if self.extra_files == 0 {
            return 0.0;
        }
        (self.extra_bulk as f64).max(self.extra_files as f64 * EMPTY_FILE_WEIGHT)
    }

    fn weight(&self) -> f64 {
        let files = self.total_files();
        if files == 0 {
            return 0.0;
        }
        (self.total_bulk() as f64).max(files as f64 * EMPTY_FILE_WEIGHT)
    }

    /// The ward holding the most bytes anywhere beneath this district.
    fn dominant_ward(&self) -> Ward {
        let mut totals = [0u64; Ward::ALL.len()];
        self.tally(&mut totals);
        Ward::ALL
            .iter()
            .enumerate()
            .max_by_key(|(i, _)| totals[*i])
            .filter(|(i, _)| totals[*i] > 0)
            .map(|(_, w)| *w)
            .unwrap_or(Ward::Other)
    }

    fn tally(&self, totals: &mut [u64; Ward::ALL.len()]) {
        for b in &self.buildings {
            totals[ward_index(b.ward)] += mass(b.ward, b.bulk).max(1);
        }
        if self.extra_files > 0 {
            totals[ward_index(self.extra_ward)] += self.extra_bulk.max(1);
        }
        for c in &self.children {
            c.tally(totals);
        }
    }
}

fn ward_index(ward: Ward) -> usize {
    Ward::ALL.iter().position(|w| *w == ward).unwrap_or(0)
}

/// Applies both caps: depth first, then the building budget.
fn trim(root: &District) -> Trimmed {
    let mut trimmed = collapse(root, MAX_DEPTH);
    apply_budget(&mut trimmed);
    sort_tree(&mut trimmed);
    trimmed
}

/// Flattens districts below `depth_left` into their deepest surviving ancestor.
///
/// Their files keep their full paths, so a plan touching a deeply nested file
/// still lights up a real building.
fn collapse(district: &District, depth_left: usize) -> Trimmed {
    let mut out = Trimmed {
        name: district.name.clone(),
        path: district.path.clone(),
        buildings: district.buildings.clone(),
        children: Vec::new(),
        extra_files: district.extra_files,
        extra_bulk: district.extra_bulk,
        extra_ward: Ward::Other,
    };

    if depth_left == 0 {
        for child in &district.children {
            absorb(child, &mut out);
        }
    } else {
        for child in &district.children {
            if child.total_files() == 0 {
                continue;
            }
            out.children.push(collapse(child, depth_left - 1));
        }
    }

    out.extra_ward = dominant_of(district);
    out
}

/// Folds an entire subtree's files into one district's own building list.
fn absorb(district: &District, into: &mut Trimmed) {
    into.buildings.extend(district.buildings.iter().cloned());
    into.extra_files += district.extra_files;
    into.extra_bulk += district.extra_bulk;
    for child in &district.children {
        absorb(child, into);
    }
}

/// Keeps the most significant [`MAX_BUILDINGS`] files and turns the rest into
/// commons.
///
/// Selection is global rather than per-district so the city's genuinely most
/// significant files always earn a tower, wherever they live; the remainder is
/// still accounted for district by district, so no folder shrinks below its true
/// mass. Code outranks assets here for the same reason it does when choosing the
/// cathedral: a city drawn from its images tells the King nothing.
fn apply_budget(root: &mut Trimmed) {
    let mut all: Vec<(bool, u64, String)> = Vec::new();
    gather(root, &mut all);

    if all.len() <= MAX_BUILDINGS {
        return;
    }

    all.sort_by(|a, b| {
        b.0.cmp(&a.0)
            .then_with(|| b.1.cmp(&a.1))
            .then_with(|| a.2.cmp(&b.2))
    });
    let keep: BTreeSet<String> = all
        .into_iter()
        .take(MAX_BUILDINGS)
        .map(|(_, _, p)| p)
        .collect();

    demote(root, &keep);
}

fn gather(district: &Trimmed, out: &mut Vec<(bool, u64, String)>) {
    for b in &district.buildings {
        out.push((is_code(b.ward), mass(b.ward, b.bulk), b.path.clone()));
    }
    for c in &district.children {
        gather(c, out);
    }
}

fn demote(district: &mut Trimmed, keep: &BTreeSet<String>) {
    let mut dropped_bulk = 0u64;
    let mut dropped = 0usize;
    let mut dropped_wards = [0u64; Ward::ALL.len()];

    district.buildings.retain(|b| {
        if keep.contains(&b.path) {
            true
        } else {
            dropped += 1;
            dropped_bulk += mass(b.ward, b.bulk);
            dropped_wards[ward_index(b.ward)] += mass(b.ward, b.bulk).max(1);
            false
        }
    });

    if dropped > 0 {
        // Blend the demoted files' dominant ward with whatever the scanner had
        // already set aside, so the commons block is tinted by what it holds.
        if district.extra_files == 0 {
            if let Some((_, ward)) = Ward::ALL
                .iter()
                .enumerate()
                .filter(|(i, _)| dropped_wards[*i] > 0)
                .max_by_key(|(i, _)| dropped_wards[*i])
            {
                district.extra_ward = *ward;
            }
        }
        district.extra_files += dropped;
        district.extra_bulk += dropped_bulk;
    }

    for child in &mut district.children {
        demote(child, keep);
    }
}

/// Sorts every level so placement never depends on directory-read order.
///
/// This is what makes the layout reproducible across machines and reloads.
fn sort_tree(district: &mut Trimmed) {
    district.buildings.sort_by(|a, b| {
        mass(b.ward, b.bulk)
            .cmp(&mass(a.ward, a.bulk))
            .then_with(|| a.path.cmp(&b.path))
            .then_with(|| a.name.cmp(&b.name))
    });
    district.children.sort_by(|a, b| {
        b.total_bulk()
            .cmp(&a.total_bulk())
            .then_with(|| a.path.cmp(&b.path))
    });
    for child in &mut district.children {
        sort_tree(child);
    }
}

/// Dominant ward of a raw district, used to tint aggregated remainders.
fn dominant_of(district: &District) -> Ward {
    let mut totals = [0u64; Ward::ALL.len()];

    fn walk(d: &District, totals: &mut [u64; Ward::ALL.len()]) {
        for b in &d.buildings {
            totals[ward_index(b.ward)] += mass(b.ward, b.bulk).max(1);
        }
        for c in &d.children {
            walk(c, totals);
        }
    }

    walk(district, &mut totals);

    Ward::ALL
        .iter()
        .enumerate()
        .filter(|(i, _)| totals[*i] > 0)
        .max_by_key(|(i, _)| totals[*i])
        .map(|(_, w)| *w)
        .unwrap_or(Ward::Other)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds a district tree from `(path, bytes)` pairs, creating folders as
    /// needed, so tests read as the shapes of real projects.
    fn tree(files: &[(&str, u64)]) -> District {
        let mut root = District::new("root", "");

        for (path, bulk) in files {
            let parts: Vec<&str> = path.split('/').collect();
            let (name, folders) = parts.split_last().expect("path has a file name");

            let mut node = &mut root;
            let mut prefix = String::new();
            for folder in folders {
                if !prefix.is_empty() {
                    prefix.push('/');
                }
                prefix.push_str(folder);

                let existing = node.children.iter().position(|c| c.name == *folder);
                let idx = match existing {
                    Some(i) => i,
                    None => {
                        node.children.push(District::new(*folder, prefix.clone()));
                        node.children.len() - 1
                    }
                };
                node = &mut node.children[idx];
            }

            node.buildings.push(Building {
                name: (*name).to_string(),
                path: (*path).to_string(),
                ward: Ward::from_path(path),
                bulk: *bulk,
            });
        }

        root
    }

    /// A project shaped like a real one: nested, lopsided, mixed languages.
    fn sample_project(files: usize) -> District {
        let owned: Vec<(String, u64)> = (0..files)
            .map(|i| {
                let path = match i % 5 {
                    0 => format!("src/core/mod_{i}.rs"),
                    1 => format!("src/ui/view_{i}.ts"),
                    2 => format!("src/ui/deep/nested/inner_{i}.ts"),
                    3 => format!("docs/page_{i}.md"),
                    _ => format!("config_{i}.toml"),
                };
                // Deliberately lopsided sizes, including a zero-byte file.
                (path, ((i * 977) % 9_000) as u64)
            })
            .collect();

        let refs: Vec<(&str, u64)> = owned.iter().map(|(p, b)| (p.as_str(), *b)).collect();
        tree(&refs)
    }

    fn overlaps(a: &Lot, b: &Lot) -> bool {
        // A shared edge is not an overlap, so compare with a small tolerance.
        const EPS: f64 = 1e-9;
        let dx = (a.x - b.x).abs();
        let dy = (a.y - b.y).abs();
        dx < (a.width + b.width) / 2.0 - EPS && dy < (a.depth + b.depth) / 2.0 - EPS
    }

    /// The load-bearing invariant. Buildings must not overlap each other, or the
    /// skyline stops being readable as structure; and they must stay inside the
    /// city's own circle, which is what extends `spiral_layout`'s guarantee that
    /// cities never collide down to the buildings inside them.
    #[test]
    fn buildings_never_overlap_and_stay_inside_their_city() {
        for count in [1, 7, 60, 400] {
            let radius = 80.0;
            let skyline = build_skyline(&sample_project(count), radius);

            assert!(!skyline.lots.is_empty(), "{count} files produced no lots");

            for lot in &skyline.lots {
                // Corners are what matter: the footprint is axis-aligned, so the
                // farthest point from the centre is a corner of the rectangle.
                let far_x = lot.x.abs() + lot.width / 2.0;
                let far_y = lot.y.abs() + lot.depth / 2.0;
                let corner = (far_x * far_x + far_y * far_y).sqrt();
                assert!(
                    corner <= radius,
                    "{count} files: '{}' reaches {corner:.2} beyond city radius {radius}",
                    lot.path
                );
                assert!(
                    lot.width > 0.0 && lot.depth > 0.0 && lot.height > 0.0,
                    "{count} files: '{}' has no volume",
                    lot.path
                );
            }

            for (i, a) in skyline.lots.iter().enumerate() {
                for b in skyline.lots.iter().skip(i + 1) {
                    assert!(
                        !overlaps(a, b),
                        "{count} files: '{}' overlaps '{}'",
                        a.path,
                        b.path
                    );
                }
            }
        }
    }

    /// The King navigates by spatial memory. If a building moves between
    /// reloads, the map stops being worth reading -- the same reason
    /// `spiral_layout_is_deterministic` exists for cities.
    #[test]
    fn skyline_layout_is_deterministic() {
        let project = sample_project(120);
        assert_eq!(build_skyline(&project, 90.0), build_skyline(&project, 90.0));

        // Folder order on disk varies between machines; placement must not.
        let mut shuffled = project.clone();
        shuffled.children.reverse();
        for child in &mut shuffled.children {
            child.buildings.reverse();
        }
        assert_eq!(
            build_skyline(&project, 90.0),
            build_skyline(&shuffled, 90.0),
            "skyline must not depend on directory read order"
        );
    }

    /// The map must never silently lose code: every file is either its own tower
    /// or counted inside a commons block. Without this, "where does my code
    /// live" would be answerable only for projects under the cap.
    #[test]
    fn every_file_is_accounted_for() {
        for count in [12, MAX_BUILDINGS, MAX_BUILDINGS * 4] {
            let project = sample_project(count);
            let skyline = build_skyline(&project, 100.0);

            let placed: usize = skyline.lots.iter().map(|l| l.files).sum();
            assert_eq!(
                placed,
                project.total_files(),
                "{count} files: skyline accounts for {placed}"
            );

            let towers = skyline
                .lots
                .iter()
                .filter(|l| l.kind == LotKind::Tower)
                .count();
            assert!(
                towers <= MAX_BUILDINGS,
                "{count} files: {towers} towers exceeds the cap"
            );
        }
    }

    /// Regression: a project's biggest *file* is usually a lockfile, a demo
    /// video or a bundled image, and sizing purely by bytes let those bury the
    /// source. The map then answered "where are my bytes?" -- a question nobody
    /// asked -- so the landmark and the tallest towers must be code.
    #[test]
    fn assets_never_outweigh_code() {
        let project = tree(&[
            ("assets/promo.mp4", 40_000_000),
            ("assets/hero.png", 8_000_000),
            ("Cargo.lock", 2_000_000),
            ("src/main.rs", 24_000),
            ("src/engine.rs", 60_000),
        ]);

        let skyline = build_skyline(&project, 100.0);
        let lot = |path: &str| {
            skyline
                .lots
                .iter()
                .find(|l| l.path == path)
                .unwrap_or_else(|| panic!("{path} was not placed"))
        };

        let cathedral = skyline
            .cathedral
            .map(|i| &skyline.lots[i])
            .expect("a city with source files has a landmark");
        assert_eq!(
            cathedral.path, "src/engine.rs",
            "the landmark must be the largest source file, not the largest file"
        );

        let video = lot("assets/promo.mp4");
        let engine = lot("src/engine.rs");
        assert!(
            engine.height > video.height,
            "a 60 KB source file must stand taller than a 40 MB video"
        );
        assert!(
            engine.width * engine.depth > video.width * video.depth,
            "source must claim more ground than bundled assets"
        );
        // The video is still on the map: it exists, and hiding it would be a
        // different kind of lie.
        assert!(video.height > 0.0 && video.width > 0.0);
    }

    /// SVG has no depth buffer, so draw order carries the entire 3D illusion.
    /// When a far tower is painted over a near one the city visibly clips
    /// through itself, which reads as broken software rather than a skyline.
    #[test]
    fn buildings_are_painted_back_to_front() {
        let skyline = build_skyline(&sample_project(150), 90.0);
        let pos: std::collections::HashMap<&str, usize> = skyline
            .lots
            .iter()
            .enumerate()
            .map(|(i, l)| (l.path.as_str(), i))
            .collect();

        for a in &skyline.lots {
            for b in &skyline.lots {
                if a.path == b.path {
                    continue;
                }

                // Boxes occlude when they share a span on one axis and one sits
                // lower on the other, or when one is lower on *both* (diagonal:
                // a tall far box still rises into a near box's screen area).
                let shares_y = a.y0() < b.y1() && b.y0() < a.y1();
                let shares_x = a.x0() < b.x1() && b.x0() < a.x1();
                let lower_x = a.x1() <= b.x0();
                let lower_y = a.y1() <= b.y0();
                let a_behind =
                    (shares_y && lower_x) || (shares_x && lower_y) || (lower_x && lower_y);

                if a_behind {
                    assert!(
                        pos[a.path.as_str()] < pos[b.path.as_str()],
                        "'{}' is behind '{}' but is painted after it",
                        a.path,
                        b.path
                    );
                }
            }
        }
    }
}
