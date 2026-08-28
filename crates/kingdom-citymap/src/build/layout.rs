//! Giving the scanned tree somewhere to stand.
//!
//! The important property here is that size follows content. Every file is
//! given the same land budget, the settlement's area is derived from how many
//! files it holds, and when wards will not fit the island grows rather than
//! the wards shrinking. A realm works the same way one level up: each town is
//! laid out at its natural size first, and the island is drawn around them
//! afterwards.

use std::cmp::Ordering;

use crate::map::BuildingKind;

use crate::build::model::{Category, Metrics, Node, NodeKind, Repository};

/// The island ground one file is given, in world units².
///
/// This single number is what makes a settlement grow with its contents
/// instead of squeezing them into a fixed square. Everything else in the world
/// — road widths, tree spacing, the clearance around a holding, the size a
/// folder's name is painted at — is an absolute measurement tuned against a
/// holding of roughly this much land, so holding the ratio fixed is what keeps
/// a thousand-file realm looking like the same place as a thirty-file
/// repository, only larger.
///
/// It is the *whole* square per file, not the lot: most of it is the water
/// margin, the streets, the padding around each ward, and the open ground the
/// woodland grows on.
pub(crate) const LAND_PER_FILE: f32 = 37_300.0;

/// The smallest settlement, expressed in files' worth of land.
///
/// A one-file repository still has to be big enough to hold a road and a tree,
/// both of which are measured in absolute world units.
const MIN_SETTLEMENT_FILES: f32 = 8.0;

/// The share of its square a settlement's wards may cover.
///
/// Rectangles dropped one at a time onto open ground jam well below the
/// theoretical limit, so this is set to a density the packer can actually
/// reach. Asking for more only makes it fail and grow the island, which wastes
/// the very ground the higher figure was meant to save.
const SETTLEMENT_FILL: f32 = 0.28;

/// The share of its island a realm's towns may cover, on the same reasoning.
const REALM_FILL: f32 = 0.32;

/// The size the absolute constants scattered through the scene were tuned at.
///
/// Nothing is laid out in a square this size, and since the coastline became a
/// circle derived from the extent nothing outside the tests reads it either --
/// it survives as the yardstick [`reference_extent`] hands them.
#[cfg(test)]
pub(crate) const REFERENCE_WORLD: f32 = 1000.0;

/// The deepest a lot ever stands back from the street running past its cell.
///
/// Roads are planned against lots, so this is shared rather than inlined: a
/// driveway is allowed to leave its lot by exactly this much to reach the
/// street, and no further. Every split carves a corridor, so two lots are
/// always separated by both their setbacks plus that corridor — which is what
/// makes reaching one setback safe from ever touching the lot next door.
pub(crate) const LOT_SETBACK: f32 = 12.0;

/// The fewest lines a holding is ever sized as having.
///
/// [`weight`] is linear in lines, and taken literally a three-line `mod.rs`
/// would get a three-line lot: a house under a world unit across, with a height
/// ceiling — `plan * 1.9` — falling straight through the 11-unit floor that
/// every archetype is modelled against. So the proportion is honoured down to
/// about a screenful of code and no further.
///
/// It costs about a tenth of the files in this repository their exact share,
/// and every one of them is a file whose entire content fits on one screen.
/// That is the right place to stop: below it the difference being drawn is
/// smaller than the ink used to draw it.
const MIN_HOLDING_LINES: f32 = 60.0;

/// The lot given to a file the scanner never opened.
///
/// Binary assets, and anything over `scan::MAX_ANALYZED_BYTES`, are never read,
/// so [`Metrics::lines`](crate::build::model::Metrics::lines) is zero for them
/// — not because they are empty but because nobody counted. Sizing them from
/// `bytes` instead would hand a 3.5 MB bundled `.min.js` a quarter of the town
/// on the strength of one line, which is exactly the claim the linear rule is
/// not making: it is a rule about *text*. A fixed nominal lot says "something
/// stands here, and it is not code the map can measure".
const UNREAD_HOLDING_LINES: f32 = 120.0;

/// The share of its lot's area a house covers.
///
/// Constant, so that a lot twice the size carries a house twice the size. The
/// figure is the middle of the range the old per-house hash used to roll, so
/// the density of a street is unchanged -- only its evenness.
const LOT_COVERAGE: f32 = 0.26;

/// The most of one side of its lot a house may take.
///
/// [`LOT_COVERAGE`] fixes the area, and `skew` trades one side against the
/// other to vary the shape; on a lot that is already long and thin that trade
/// can push a side out to the boundary. This keeps the front garden on every
/// lot, so a house never touches the street planned along its edge.
const MAX_LOT_SIDE: f32 = 0.8;

/// The narrowest a cell may be cut, in world units.
///
/// A split's ratio follows the weights either side of it, and with weights now
/// linear that ratio can be genuinely tiny — a 60-line file beside a 5,000-line
/// one is a hair over 1% of their cell. Some floor is needed or the thin side
/// becomes a sliver no house can stand on, but the floor this replaced was a
/// *share* — a flat `0.08` — which charged a large cell 8% of its ground for
/// its smallest file however small that file was, and so over-served the
/// tiniest holdings by up to 29×.
///
/// Expressed as land instead, it costs whatever it has to and no more: in a
/// small cell it binds about as often as the old clamp did, and in a large one
/// it is a rounding error. It is measured against the side being cut, which is
/// [`Rect::span`], because that is the direction the sliver would be thin in,
/// and the street about to be carved from that cell is charged for on top of
/// it — half of an avenue can be fifteen units on its own.
const MIN_LOT_THICKNESS: f32 = 22.0;

/// World units of height per branch-per-line.
///
/// Tuned against the density this repository actually shows: a median file
/// runs about 0.08 branches per line and the densest around 0.22. At this
/// figure that spread lands between roughly 23 and 50 units, which uses most
/// of the 11–54 range without piling files up against either end.
const DENSITY_HEIGHT: f32 = 190.0;

/// An axis-aligned rectangle on the ground plane, in world units.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    /// Left edge.
    pub x: f32,
    /// Top edge, running along the ground `y` axis rather than up.
    pub y: f32,
    /// Extent along `x`.
    pub width: f32,
    /// Extent along `y`.
    pub height: f32,
}

impl Rect {
    /// Shrinks the rectangle on every side, never past a third of a side so a
    /// small rectangle cannot invert.
    pub fn inset(self, amount: f32) -> Self {
        let horizontal = amount.min(self.width * 0.32);
        let vertical = amount.min(self.height * 0.32);
        Self {
            x: self.x + horizontal,
            y: self.y + vertical,
            width: (self.width - horizontal * 2.0).max(1.0),
            height: (self.height - vertical * 2.0).max(1.0),
        }
    }

    /// Inset by `amount`, but never by more than `fraction` of the shorter side.
    ///
    /// A fixed inset consumes almost the entire cell once a repository packs
    /// thousands of files into small lots, which is what makes dense wards
    /// collapse into a solid mass of touching buildings. Scaling the road down
    /// with the cell keeps a proportional amount of open ground at every
    /// density.
    pub fn inset_relative(self, amount: f32, fraction: f32) -> Self {
        let shortest = self.width.min(self.height);
        self.inset(amount.min(shortest * fraction))
    }

    /// Ground covered, in world units².
    pub fn area(self) -> f32 {
        self.width * self.height
    }

    /// The longer side, used wherever a single number has to stand for the
    /// size of a piece of ground.
    pub fn span(self) -> f32 {
        self.width.max(self.height)
    }

    /// The middle of the rectangle, as `(x, y)`.
    pub fn center(self) -> (f32, f32) {
        (self.x + self.width * 0.5, self.y + self.height * 0.5)
    }

    /// The same rectangle moved by `(dx, dy)`.
    pub fn translated(self, dx: f32, dy: f32) -> Self {
        Self {
            x: self.x + dx,
            y: self.y + dy,
            ..self
        }
    }

    /// Grows the rectangle about its centre by `factor`.
    fn scaled_about_center(self, factor: f32) -> Self {
        let (cx, cy) = self.center();
        let width = self.width * factor;
        let height = self.height * factor;
        Self {
            x: cx - width * 0.5,
            y: cy - height * 0.5,
            width,
            height,
        }
    }
}

/// Pulls a piece of ground in until it hugs what was actually built on it.
///
/// Packing has to start from an estimate of how much room the buildings will
/// need, and an estimate that came out generous leaves a broad ring of empty
/// country round the outside — the settlement marooned in the middle of its
/// own island. Drawing the coastline round the built area instead, plus a
/// fringe of open ground for woodland, is what makes the island look like it
/// grew out of its contents rather than having been drawn first and filled in
/// afterwards.
///
/// Nothing that was placed moves, and no holding changes size: this only
/// decides where the water starts.
fn hug(placements: &[Rect], fallback: Rect) -> Rect {
    /// How much open ground to leave outside the built area, as a share of its
    /// span. Enough for woodland and a coast road, not enough to lose the town
    /// in.
    const FRINGE: f32 = 0.115;
    const MIN_FRINGE: f32 = 60.0;

    let Some(first) = placements.first() else {
        return fallback;
    };
    let mut min_x = first.x;
    let mut min_y = first.y;
    let mut max_x = first.x + first.width;
    let mut max_y = first.y + first.height;
    for rect in placements.iter().skip(1) {
        min_x = min_x.min(rect.x);
        min_y = min_y.min(rect.y);
        max_x = max_x.max(rect.x + rect.width);
        max_y = max_y.max(rect.y + rect.height);
    }

    let built = Rect {
        x: min_x,
        y: min_y,
        width: max_x - min_x,
        height: max_y - min_y,
    };
    let fringe = (built.span() * FRINGE).max(MIN_FRINGE);
    Rect {
        x: built.x - fringe,
        y: built.y - fringe,
        width: built.width + fringe * 2.0,
        height: built.height + fringe * 2.0,
    }
}

/// A reference-sized piece of ground, for tests that need an island but do not
/// care how big it is.
#[cfg(test)]
pub(crate) fn reference_extent() -> Rect {
    Rect {
        x: 0.0,
        y: 0.0,
        width: REFERENCE_WORLD,
        height: REFERENCE_WORLD,
    }
}

/// The ground a settlement of `file_count` files covers, before it is packed.
///
/// The aspect comes from the repository's name so two towns side by side are
/// not identical squares, but the area only ever follows the file count: that
/// is the whole point of sizing the island from its contents.
fn settlement_extent(name: &str, file_count: usize) -> Rect {
    let files = (file_count as f32).max(MIN_SETTLEMENT_FILES);
    let area = files * LAND_PER_FILE;
    let aspect = 0.84 + (stable_hash(name) % 33) as f32 / 100.0;
    let width = (area * aspect).sqrt();
    Rect {
        x: 0.0,
        y: 0.0,
        width,
        height: area / width,
    }
}

/// A strip of ground the layout kept clear so a street could run along it.
///
/// This is the whole reason roads can be drawn at all. The subdivision below
/// splits a folder's ground in two over and over, and every one of those split
/// lines is recorded here as reserved land rather than being handed to a
/// building. Because a cell's edges are always either an ancestor's split line
/// or the ward boundary, the corridors meet end to end without anything having
/// to search for a route: the network is connected by construction.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Corridor {
    /// One end of the centre line.
    pub start: (f32, f32),
    /// The other end.
    pub end: (f32, f32),
    /// How wide the reserved strip is.
    pub width: f32,
    /// Files standing on the ground this corridor divides, which is what its
    /// width was derived from.
    pub traffic: usize,
}

/// How wide a way carrying `traffic` files should be drawn.
///
/// Sub-linear, because a road serving a thousand files is not a thousand times
/// more important than one serving a single file — it is the difference
/// between a high street and a garden path. The exponent is what keeps a
/// deeply nested lane visible while still letting the trunk route into a large
/// folder read as the spine of the settlement.
///
/// At 0.36 the whole range was too flat to read: a hundredfold difference in
/// traffic came out barely five times wider, so every road looked much like
/// every other. 0.48 roughly doubles the spread across the range that matters.
/// How many journeys a subtree generates.
///
/// A file is not one journey but many: it has to be reached once for its own
/// sake, and once more by every file that imports it. Counting only the files
/// left a much-depended-upon holding on a lane no wider than its quietest
/// neighbour's, because a hub is still just one file. Counting arrivals is
/// what makes the way to a hub thicken all the way back to the square.
pub(crate) fn arrivals(metrics: &Metrics) -> usize {
    metrics.file_count.max(1) + metrics.references
}

pub(crate) fn corridor_width(traffic: usize) -> f32 {
    (1.7 * (traffic.max(1) as f32).powf(0.48)).clamp(2.0, 34.0)
}

/// A file, and the lot it stands on.
#[derive(Clone, Debug)]
pub struct Building {
    /// The file name, which is what a tooltip shows.
    pub name: String,
    /// The path relative to the repository root.
    pub path: String,
    /// The innermost ward holding this file, if it is not loose at the root.
    pub ward_id: Option<String>,
    /// What the file is for, which fixes its colour.
    pub category: Category,
    /// The archetype drawn on the lot, which fixes its silhouette.
    pub kind: BuildingKind,
    /// The ground the holding was given.
    pub lot: Rect,
    /// Size on disk.
    pub bytes: u64,
    /// Total lines.
    pub lines: usize,
    /// Lines that are neither blank nor an obvious comment.
    ///
    /// This rather than [`lines`](Self::lines) is what [`height`](Self::height)
    /// divides by, so a heavily commented file does not read as a simple one.
    pub code_lines: usize,
    /// A rough branch count.
    pub complexity: usize,
    /// How many other files import or include this one.
    pub references: usize,
    /// A last-resort squeeze, kept for layouts that must fit a fixed space.
    ///
    /// Nothing in this crate sets it to anything but `1.0` any more — the
    /// island grows instead — but it remains the honest way to express "this
    /// holding had to be shrunk" should a caller ever need to.
    pub scale: f32,
}

impl Building {
    /// How tall the building stands: how *branchy* the file is, not how big.
    ///
    /// Size is already the footprint — a file's lot is its share of its
    /// folder's ground, in proportion to its lines. Height used to follow
    /// `ln(lines)` as well, which spent a second dimension restating the
    /// first: measured across this repository, lines and total complexity
    /// correlate at 0.94, so the two axes rose and fell together and the
    /// skyline carried one fact drawn twice.
    ///
    /// Worse, it ran out of room saying it. With `ln(lines)` driving the
    /// number, the five largest files here all pinned flat against the 54
    /// clamp — the holdings most worth picking out were the ones height could
    /// no longer tell apart.
    ///
    /// Branches *per line* is the signal that is actually independent of size
    /// (correlation 0.06). So the base answers "how much code is here?" and
    /// the height answers "how tangled is it?", and a long plain file is now a
    /// broad low hall rather than another spike.
    pub fn height(&self) -> f32 {
        let intrinsic = (8.0 + self.complexity_density() * DENSITY_HEIGHT).clamp(11.0, 54.0)
            * self.scale.clamp(0.24, 1.0);
        intrinsic.min(self.height_ceiling())
    }

    /// Branches per line of code, the measure height is drawn from.
    ///
    /// Zero for every file the scanner does not score — see
    /// [`Category`] — which is why documentation and assets sit low.
    pub fn complexity_density(&self) -> f32 {
        self.complexity as f32 / self.code_lines.max(1) as f32
    }

    /// The tallest a holding may stand before it starts hiding its neighbours.
    ///
    /// The camera is locked to an isometric angle, so rows behind a building
    /// are only a fraction of their world depth apart on screen while the
    /// silhouette rises by its full height. Tying the ceiling to the ground the
    /// building actually owns keeps sparse repositories dramatic and turns
    /// dense ones into readable villages instead of a thicket of spikes.
    fn height_ceiling(&self) -> f32 {
        let footprint = self.footprint();
        let plan = footprint.width.min(footprint.height);
        (plan * 1.9).min(self.lot.height * 1.15).max(3.0)
    }

    /// The ground the building actually covers, inset from its lot so
    /// neighbours do not touch.
    ///
    /// The house covers a **constant share of its lot's area**, and the hash
    /// varies only its proportions and where it sits. That division is the
    /// point: the lot is now linear in the file's length, so anything that
    /// scales the house by an amount the file did not earn is noise laid
    /// directly over the signal. The old occupancy ran `0.46..0.58` on each
    /// side from the hash, which is up to 1.55x of area -- enough on its own to
    /// make a file look half again as big as an equal neighbour.
    ///
    /// Nothing is lost visually. `skew` still makes one house oblong and the
    /// next square, and the offsets still stand them off-centre on their plots,
    /// so a street reads as varied as it did; two houses on equal lots simply
    /// now cover equal ground.
    pub fn footprint(&self) -> Rect {
        let hash = stable_hash(&self.name);
        // 0.88..=1.12: what one side gains in proportion the other gives back,
        // so the product -- the area -- is `LOT_COVERAGE` either way.
        let skew = 0.88 + (hash % 25) as f32 / 100.0;
        let side = LOT_COVERAGE.sqrt();
        let width = (self.lot.width * side * skew).min(self.lot.width * MAX_LOT_SIDE);
        let height = (self.lot.height * side / skew).min(self.lot.height * MAX_LOT_SIDE);
        let free_x = self.lot.width - width;
        let free_y = self.lot.height - height;
        Rect {
            x: self.lot.x + free_x * (0.34 + (hash % 23) as f32 / 70.0),
            y: self.lot.y + free_y * (0.28 + ((hash / 23) % 29) as f32 / 75.0),
            width,
            height,
        }
    }
}

/// A folder, and the ward or neighbourhood it became.
#[derive(Clone, Debug)]
pub struct District {
    /// Stable within a settlement, and unique across a realm once the town
    /// prefix is applied. Buildings, ground labels, and the manifest all refer
    /// to a ward by this.
    pub id: String,
    /// The folder name, which is what the ground label paints.
    pub name: String,
    /// The path relative to the repository root.
    pub path: String,
    /// The ground the ward covers.
    pub rect: Rect,
    /// Nesting depth, zero for a top-level folder.
    pub depth: usize,
    /// Files at or below this folder.
    pub files: usize,
    /// Journeys at or below this folder: every file, plus every reference to
    /// one. This is what a road serving the ward has to carry.
    pub arrivals: usize,
    /// The ward this one sits inside, if it is not a top-level folder.
    pub parent: Option<String>,
    /// Drives every look-of-the-place decision that must stay put across
    /// rebuilds. Kept apart from `path` so a realm can vary a ward by town
    /// without corrupting the folder path the interface shows.
    pub seed: u32,
}

/// The identifier a ward carries, from its position in the layout.
fn ward_id(index: usize) -> String {
    format!("ward-{index}")
}

/// One settlement, laid out.
#[derive(Debug)]
pub struct CityLayout {
    /// Every file, with the lot it stands on.
    pub buildings: Vec<Building>,
    /// Every folder, at every depth.
    pub districts: Vec<District>,
    /// The ground reserved for streets, carved as the wards were subdivided.
    ///
    /// These are strips of land, not drawn roads: [`streets`](crate::build::streets)
    /// is what turns them into a network. Keeping them here is what lets the
    /// roads be planned before the layout is frozen instead of being painted
    /// over the top of finished buildings.
    pub corridors: Vec<Corridor>,
    /// The ground this settlement covers, in world coordinates.
    ///
    /// It follows the number of files rather than being fixed, so a holding is
    /// the same size whether it stands in a repository of thirty files or one
    /// of three thousand. Everything that has to know how big the world is —
    /// the shoreline, the camera, the minimap — reads it from here.
    pub extent: Rect,
    /// The open lane left between wards, which is the widest an avenue running
    /// between them may be drawn.
    pub ward_gap: f32,
}

impl CityLayout {
    /// Lays a scanned tree out on ground sized to hold it.
    pub fn build(root: &Node) -> Self {
        Self::build_in(root, settlement_extent(&root.name, root.metrics.file_count))
    }

    /// Lays a settlement out on a given piece of ground, growing it if the
    /// wards will not pack.
    ///
    /// Growing the ground rather than shrinking the wards is the whole
    /// inversion: the island answers to the contents, never the other way
    /// round.
    fn build_in(root: &Node, extent: Rect) -> Self {
        let mut wards: Vec<&Node> = root.children.iter().collect();
        wards.sort_by(|left, right| {
            weight(right)
                .partial_cmp(&weight(left))
                .unwrap_or(Ordering::Equal)
                .then_with(|| left.name.cmp(&right.name))
        });

        // How much ground the wards may cover, from the file count alone. It
        // is deliberately not a share of `extent`: growing the island to make
        // room must add open country, never enlarge the holdings.
        let buildable = root.metrics.file_count as f32 * LAND_PER_FILE * SETTLEMENT_FILL;

        // The avenue nearest the square carries the whole settlement, so that
        // is what the lanes between wards have to be able to hold.
        let gap = ward_gap(arrivals(&root.metrics));

        let mut extent = extent;
        let natural_area = extent.area();
        let placements = loop {
            let packed = pack_wards(&wards, extent, buildable, gap);
            if packed.complete || extent.area() > natural_area * 4.0 {
                break packed.placements;
            }
            extent = extent.scaled_about_center(1.06);
        };

        let ward_rects: Vec<Rect> = placements.iter().map(|(_, rect)| *rect).collect();
        let mut layout = Self {
            buildings: Vec::with_capacity(root.metrics.file_count),
            districts: Vec::new(),
            corridors: Vec::new(),
            extent: hug(&ward_rects, extent),
            ward_gap: gap,
        };

        for ((child, _), ward_rect) in placements {
            match &child.kind {
                NodeKind::Directory => {
                    let path = child.relative_path.to_string_lossy().into_owned();
                    let id = ward_id(layout.districts.len());
                    layout.districts.push(District {
                        id: id.clone(),
                        name: child.name.clone(),
                        path: path.clone(),
                        rect: ward_rect,
                        depth: 0,
                        files: child.metrics.file_count,
                        arrivals: arrivals(&child.metrics),
                        parent: None,
                        seed: stable_hash(&path),
                    });
                    layout.layout_children(child, ward_rect, 1, Some(&id));
                }
                NodeKind::File { category } => {
                    layout.buildings.push(Building {
                        name: child.name.clone(),
                        path: child.relative_path.to_string_lossy().into_owned(),
                        ward_id: None,
                        category: *category,
                        kind: building_kind(child),
                        lot: ward_rect.inset_relative(10.0, 0.16),
                        bytes: child.metrics.bytes,
                        lines: child.metrics.lines,
                        code_lines: child.metrics.code_lines,
                        complexity: child.metrics.complexity,
                        references: child.metrics.references,
                        scale: 1.0,
                    });
                }
            }
        }
        layout
    }

    fn layout_children(&mut self, node: &Node, rect: Rect, depth: usize, parent: Option<&str>) {
        if node.children.is_empty() {
            return;
        }

        let mut children: Vec<&Node> = node.children.iter().collect();
        children.sort_by(|left, right| {
            weight(right)
                .partial_cmp(&weight(left))
                .unwrap_or(Ordering::Equal)
                .then_with(|| left.name.cmp(&right.name))
        });
        let placements = partition(&children, rect, &mut self.corridors);

        for (child, child_rect) in placements {
            match &child.kind {
                NodeKind::Directory => {
                    // No inset. The street carved either side of this cell is
                    // what separates it from its neighbours now, and pulling
                    // the ward in from its cell would open a gap between its
                    // own streets and the one they are supposed to join.
                    let district_rect = child_rect;
                    let path = child.relative_path.to_string_lossy().into_owned();
                    let id = ward_id(self.districts.len());
                    self.districts.push(District {
                        id: id.clone(),
                        name: child.name.clone(),
                        path: path.clone(),
                        rect: district_rect,
                        depth,
                        files: child.metrics.file_count,
                        arrivals: arrivals(&child.metrics),
                        parent: parent.map(str::to_owned),
                        seed: stable_hash(&path),
                    });
                    self.layout_children(child, district_rect, depth + 1, Some(&id));
                }
                NodeKind::File { category } => {
                    // A front garden, not a substitute for a road: the street
                    // running past the cell is now real, so the lot only has
                    // to stand back from it rather than fake the gap itself.
                    let gap = if depth <= 1 { LOT_SETBACK } else { 8.0 };
                    self.buildings.push(Building {
                        name: child.name.clone(),
                        path: child.relative_path.to_string_lossy().into_owned(),
                        ward_id: parent.map(str::to_owned),
                        category: *category,
                        kind: building_kind(child),
                        lot: child_rect.inset_relative(gap, 0.14),
                        bytes: child.metrics.bytes,
                        lines: child.metrics.lines,
                        code_lines: child.metrics.code_lines,
                        complexity: child.metrics.complexity,
                        references: child.metrics.references,
                        scale: 1.0,
                    });
                }
            }
        }
    }
}

/// One repository's settlement, placed on a realm island.
#[derive(Debug)]
pub struct TownLayout {
    /// Which entry of the surveyed slice this town came from.
    pub repository_index: usize,
    /// The repository name, which is what the town label shows.
    pub name: String,
    /// The ground the town covers. Identical to the island the same
    /// repository would have been given on its own.
    pub rect: Rect,
    /// The settlement itself, already translated into place.
    pub city: CityLayout,
}

impl TownLayout {
    /// The middle of the town, as `(x, y)`.
    pub fn center(&self) -> (f32, f32) {
        (
            self.rect.x + self.rect.width * 0.5,
            self.rect.y + self.rect.height * 0.5,
        )
    }
}

/// A realm of towns, laid out.
#[derive(Debug)]
pub struct WorldLayout {
    /// Every repository, placed.
    pub towns: Vec<TownLayout>,
    /// The ground the whole realm covers, grown to hold every town at its
    /// natural size.
    pub extent: Rect,
}

impl WorldLayout {
    /// Lays every repository out at its natural size, then draws an island
    /// around them.
    pub fn build(repositories: &[Repository]) -> Self {
        // Each settlement is laid out first, at whatever size its own contents
        // call for. Only then is a realm big enough to hold them all worked
        // out. Doing it the other way round — dividing a fixed island between
        // repositories — is what used to shrink a large codebase into a smear
        // of specks.
        let cities: Vec<CityLayout> = repositories
            .iter()
            .map(|repository| CityLayout::build(&repository.root))
            .collect();

        let (placements, extent) = pack_towns(repositories, &cities);
        let mut cities: Vec<Option<CityLayout>> = cities.into_iter().map(Some).collect();
        let towns = placements
            .into_iter()
            .map(|(repository_index, rect)| {
                let repository = &repositories[repository_index];
                let city = cities[repository_index]
                    .take()
                    .expect("each town is placed exactly once");
                let city = move_city(city, rect, &repository.root.name);
                TownLayout {
                    repository_index,
                    name: repository.root.name.clone(),
                    rect,
                    city,
                }
            })
            .collect();
        Self { towns, extent }
    }

    /// How many holdings stand in the whole realm.
    pub fn building_count(&self) -> usize {
        self.towns
            .iter()
            .map(|town| town.city.buildings.len())
            .sum()
    }
}

/// Moves a settlement onto its plot in a realm.
///
/// This is a translation and nothing more. A town's plot is exactly the ground
/// its own layout asked for, so no holding is ever resized by being placed next
/// to a larger neighbour — which is what keeps a cottage in a small repository
/// the same size as a cottage in a large one.
///
/// Ward identifiers are prefixed with the town so two repositories never claim
/// the same `ward-3`, and the ward's variation seed picks up the town name so
/// neighbouring towns do not come out identically tinted. The folder `path`
/// deliberately stays repository-relative: it is what the interface shows.
fn move_city(mut city: CityLayout, rect: Rect, town_name: &str) -> CityLayout {
    let dx = rect.x - city.extent.x;
    let dy = rect.y - city.extent.y;
    let prefix = |id: &str| format!("{town_name}/{id}");
    for building in &mut city.buildings {
        building.lot = building.lot.translated(dx, dy);
        building.ward_id = building.ward_id.as_deref().map(prefix);
    }
    for district in &mut city.districts {
        district.rect = district.rect.translated(dx, dy);
        district.id = prefix(&district.id);
        district.parent = district.parent.as_deref().map(prefix);
        district.seed = stable_hash(&format!("{town_name}/{}", district.path));
    }
    for corridor in &mut city.corridors {
        corridor.start = (corridor.start.0 + dx, corridor.start.1 + dy);
        corridor.end = (corridor.end.0 + dx, corridor.end.1 + dy);
    }
    city.extent = city.extent.translated(dx, dy);
    city
}

/// The angle a sunflower spiral turns by at each step, in radians.
const GOLDEN_ANGLE: f32 = 2.399_963_2;

/// The outcome of packing a set of rectangles onto a piece of ground.
struct Packing<T> {
    placements: Vec<(T, Rect)>,
    /// Whether every rectangle found a clear spot inside the ground. A `false`
    /// here is the signal to grow the ground and try again.
    complete: bool,
}

/// Places already-sized towns on an island, growing the island until they fit.
///
/// Returns the placements and the ground the realm ended up covering.
fn pack_towns(repositories: &[Repository], cities: &[CityLayout]) -> (Vec<(usize, Rect)>, Rect) {
    const TOWN_GAP: f32 = 24.0;

    let mut indices: Vec<usize> = (0..repositories.len()).collect();
    indices.sort_by(|left, right| {
        cities[*right]
            .extent
            .area()
            .partial_cmp(&cities[*left].extent.area())
            .unwrap_or(Ordering::Equal)
            .then_with(|| {
                repositories[*left]
                    .root
                    .name
                    .cmp(&repositories[*right].root.name)
            })
    });

    // The island starts at the size the towns would need if they tiled it at
    // the usual density, and the largest town alone has to fit inside the
    // margins whatever happens.
    let occupied: f32 = cities.iter().map(|city| city.extent.area()).sum();
    let widest = cities
        .iter()
        .map(|city| city.extent.span())
        .fold(0.0f32, f32::max);
    let mut side = (occupied / REALM_FILL).sqrt().max(widest / 0.86);

    for _ in 0..14 {
        let extent = Rect {
            x: 0.0,
            y: 0.0,
            width: side,
            height: side,
        };
        let rects: Vec<(usize, Rect)> = indices
            .iter()
            .map(|index| (*index, cities[*index].extent))
            .collect();
        let packed = pack_onto(&rects, extent, TOWN_GAP, |(index, _)| {
            stable_hash(&repositories[*index].root.name)
        });
        if packed.complete {
            let placements: Vec<(usize, Rect)> = packed
                .placements
                .into_iter()
                .map(|((index, _), rect)| (index, rect))
                .collect();
            let rects: Vec<Rect> = placements.iter().map(|(_, rect)| *rect).collect();
            let island = hug(&rects, extent);
            return (placements, island);
        }
        side *= 1.07;
    }

    let extent = Rect {
        x: 0.0,
        y: 0.0,
        width: side,
        height: side,
    };
    let rects: Vec<(usize, Rect)> = indices
        .iter()
        .map(|index| (*index, cities[*index].extent))
        .collect();
    let packed = pack_onto(&rects, extent, TOWN_GAP, |(index, _)| {
        stable_hash(&repositories[*index].root.name)
    });
    let placements: Vec<(usize, Rect)> = packed
        .placements
        .into_iter()
        .map(|((index, _), rect)| (index, rect))
        .collect();
    let rects: Vec<Rect> = placements.iter().map(|(_, rect)| *rect).collect();
    let island = hug(&rects, extent);
    (placements, island)
}

/// Spirals fixed-size rectangles outward from the middle of `extent`.
///
/// Nothing is ever resized to make it fit. If a rectangle cannot find clear
/// ground the packing reports itself incomplete and the caller grows the
/// ground, which is what turns a crowded settlement into a larger island rather
/// than a squashed one.
fn pack_onto<T: Copy + HasExtent>(
    items: &[T],
    extent: Rect,
    gap: f32,
    seed: impl Fn(&T) -> u32,
) -> Packing<T> {
    let edge = extent.span() * 0.045;
    let (center_x, center_y) = extent.center();
    // The spiral has to be able to reach the far corner of whatever ground it
    // is given, so its step is measured in that ground rather than in absolute
    // units.
    let reach = extent.span() * 0.75;
    let mut placed: Vec<(T, Rect)> = Vec::with_capacity(items.len());
    let mut complete = true;

    for item in items {
        let size = item.extent();
        let hash = seed(item);
        let mut chosen = None;

        // A sunflower spiral: `radius` grows with the square root of the step
        // and the angle turns by the golden angle, which spreads candidates
        // evenly over the ground instead of leaving the wide arcs a constant
        // angular step opens up far from the middle. The pitch is set by the
        // rectangle being placed, so the search is as fine as it needs to be
        // and no finer — that is what lets it succeed at the natural size
        // rather than reporting defeat and making the caller grow the island.
        let pitch = (size.width.min(size.height) * 0.11).max(reach * 0.0015);
        let steps = ((reach / pitch).powi(2).ceil() as usize).clamp(64, 400_000);
        let start = (hash % 17) as f32 * 0.37;
        for step in 0..steps {
            let angle = start + step as f32 * GOLDEN_ANGLE;
            let radius = pitch * (step as f32).sqrt();
            let candidate = Rect {
                x: center_x + angle.cos() * radius * 1.12 - size.width * 0.5,
                y: center_y + angle.sin() * radius * 0.88 - size.height * 0.5,
                width: size.width,
                height: size.height,
            };
            let inside = candidate.x >= extent.x + edge
                && candidate.y >= extent.y + edge
                && candidate.x + candidate.width <= extent.x + extent.width - edge
                && candidate.y + candidate.height <= extent.y + extent.height - edge;
            if inside
                && placed
                    .iter()
                    .all(|(_, other)| !rects_overlap(candidate, *other, gap))
            {
                chosen = Some(candidate);
                break;
            }
        }

        let rect = chosen.unwrap_or_else(|| {
            complete = false;
            // Somewhere legal, so a failed packing still produces a usable
            // world while the caller grows the ground and tries again.
            let index = placed.len() as f32;
            let angle = index * 2.399_963;
            let radius = extent.span() * 0.06 + index.sqrt() * extent.span() * 0.08;
            Rect {
                x: (center_x + angle.cos() * radius - size.width * 0.5)
                    .clamp(extent.x, extent.x + (extent.width - size.width).max(0.0)),
                y: (center_y + angle.sin() * radius - size.height * 0.5)
                    .clamp(extent.y, extent.y + (extent.height - size.height).max(0.0)),
                width: size.width,
                height: size.height,
            }
        });
        placed.push((*item, rect));
    }

    Packing {
        placements: placed,
        complete,
    }
}

/// The ground an item being packed needs.
trait HasExtent {
    fn extent(&self) -> Rect;
}

impl HasExtent for (usize, Rect) {
    fn extent(&self) -> Rect {
        self.1
    }
}

impl HasExtent for (&Node, Rect) {
    fn extent(&self) -> Rect {
        self.1
    }
}

fn building_kind(node: &Node) -> BuildingKind {
    let category = match node.kind {
        NodeKind::File { category } => category,
        NodeKind::Directory => return BuildingKind::Cottage,
    };
    let stem = node
        .relative_path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let path_depth = node.relative_path.components().count();
    let is_entrypoint = path_depth <= 2
        && matches!(
            stem.as_str(),
            "main" | "app" | "application" | "server" | "lib" | "mod" | "index"
        )
        && matches!(
            category,
            Category::Source | Category::Web | Category::Script
        );
    if is_entrypoint {
        return BuildingKind::Keep;
    }

    match category {
        Category::Source => BuildingKind::Guildhall,
        Category::Web => BuildingKind::Market,
        Category::Test => BuildingKind::Watchtower,
        Category::Docs => BuildingKind::Scriptorium,
        Category::Config => BuildingKind::CouncilHall,
        Category::Data => BuildingKind::Granary,
        Category::Asset => BuildingKind::Stockpile,
        Category::Script => BuildingKind::Forge,
        Category::Other => BuildingKind::Cottage,
    }
}

/// Places a settlement's top-level wards on its ground.
///
/// A ward's area is its share of the buildable land, exactly as before — but
/// the land itself now came from the file count, so the share works out to the
/// same number of square units per file in every repository. Wards are never
/// shrunk to make them fit; an incomplete packing tells the caller to hand this
/// the same wards on a larger piece of ground.
/// Open ground left between neighbouring wards.
///
/// This is the lane every avenue runs in, so the road planner's grid has to
/// stay fine enough to see it — see `streets::Ground`.
pub(crate) const WARD_GAP: f32 = 15.0;

/// How much wider an avenue is drawn than a street carrying the same traffic.
///
/// An avenue crosses open country between wards rather than threading between
/// holdings, and at that length it needs the extra width to read as the route
/// into a ward rather than as a crack in the ground.
pub(crate) const AVENUE_WIDENING: f32 = 1.2;

/// The lane to leave between wards, given what will have to run down it.
///
/// A fixed lane was a promise the layout could not keep. An avenue's width
/// follows its traffic, and the avenue nearest the square carries the whole
/// settlement, so on a busy map the widest avenues were drawn near three times
/// wider than the fifteen units they ran in and spilled over the holdings on
/// either side. Sizing the lane for the widest avenue it may have to carry is
/// what makes the road fit the ground reserved for it.
pub(crate) fn ward_gap(traffic: usize) -> f32 {
    (corridor_width(traffic) * AVENUE_WIDENING + WARD_GAP * 0.35).max(WARD_GAP)
}

fn pack_wards<'a>(
    nodes: &[&'a Node],
    extent: Rect,
    buildable: f32,
    gap: f32,
) -> Packing<(&'a Node, Rect)> {
    // The longest a single ward may be, measured against the settlement the
    // file count called for rather than against the ground it was finally
    // given: an island grown to make room must not stretch its wards with it.
    let longest = (buildable / SETTLEMENT_FILL).sqrt() * 0.43;
    let total_weight: f32 = nodes.iter().map(|node| weight(node)).sum();
    let sized: Vec<(&Node, Rect)> = nodes
        .iter()
        .map(|node| {
            let hash = stable_hash(&node.name);
            let aspect = 0.72 + (hash % 91) as f32 / 100.0;
            let target_area = if total_weight > 0.0 {
                (buildable * weight(node) / total_weight).max(620.0)
            } else {
                620.0
            };
            let width = (target_area * aspect).sqrt().clamp(22.0, longest.max(22.0));
            let height = (target_area / width).clamp(22.0, longest.max(22.0));
            (
                *node,
                Rect {
                    x: 0.0,
                    y: 0.0,
                    width,
                    height,
                },
            )
        })
        .collect();

    pack_onto(&sized, extent, gap, |(node, _)| stable_hash(&node.name))
}

fn rects_overlap(left: Rect, right: Rect, gap: f32) -> bool {
    left.x - gap < right.x + right.width
        && left.x + left.width + gap > right.x
        && left.y - gap < right.y + right.height
        && left.y + left.height + gap > right.y
}

pub(crate) fn stable_hash(value: &str) -> u32 {
    value.bytes().fold(2_166_136_261, |hash, byte| {
        hash.wrapping_mul(16_777_619) ^ byte as u32
    })
}

/// How much ground a node has earned, in lines of content.
///
/// **Linear, deliberately.** A file with twice as much in it gets twice the
/// land, so the map answers "how much is in here?" by area and not by a curve
/// the reader has to undo in their head. This was `20 + √bytes`, and the square
/// root *was* the fault: 64 KB earned about five times the lot of 1 KB rather
/// than sixty-four. Measured across this repository, only 32% of holdings
/// stood within ±43% of their proportional share, and a file with exactly twice
/// its neighbour's content drew a median **0.66×** the area — less than the
/// smaller one's own share, let alone twice it.
///
/// **A folder weighs exactly the sum of its children.** The old directory arm
/// was a different formula from the file arm, so what a folder was given and
/// what it had to hand out were two unrelated numbers and the proportion leaked
/// away at every level of the recursion. Summing is what lets the ratio survive
/// [`partition_recursive`] all the way down to a lot.
fn weight(node: &Node) -> f32 {
    match node.kind {
        NodeKind::Directory => node.children.iter().map(weight).sum(),
        NodeKind::File { .. } if node.metrics.lines > 0 => {
            (node.metrics.lines as f32).max(MIN_HOLDING_LINES)
        }
        NodeKind::File { .. } => UNREAD_HOLDING_LINES,
    }
}

fn partition<'a>(
    nodes: &[&'a Node],
    rect: Rect,
    corridors: &mut Vec<Corridor>,
) -> Vec<(&'a Node, Rect)> {
    let mut output = Vec::with_capacity(nodes.len());
    partition_recursive(nodes, rect, corridors, &mut output);
    output
}

fn partition_recursive<'a>(
    nodes: &[&'a Node],
    rect: Rect,
    corridors: &mut Vec<Corridor>,
    output: &mut Vec<(&'a Node, Rect)>,
) {
    match nodes.len() {
        0 => return,
        1 => {
            output.push((nodes[0], rect));
            return;
        }
        _ => {}
    }

    let total: f32 = nodes.iter().map(|node| weight(node)).sum();
    let mut running = 0.0;
    let mut split = 1;
    let mut best_difference = f32::MAX;
    for (index, node) in nodes.iter().enumerate().take(nodes.len() - 1) {
        running += weight(node);
        let difference = (total * 0.5 - running).abs();
        if difference < best_difference {
            best_difference = difference;
            split = index + 1;
        }
    }
    let first_weight: f32 = nodes[..split].iter().map(|node| weight(node)).sum();
    // A minimum *lot*, not a minimum share: see `MIN_LOT_THICKNESS`.
    //
    // The floor has to leave room for the street this split is about to carve
    // as well as for the lot, because half that street comes out of each side
    // *after* the ratio is applied. Charging only for the lot is what let a
    // 31-unit avenue eat a cell down to a house it then ran through.
    let span = rect.span();
    let street = corridor_width(nodes.iter().map(|node| arrivals(&node.metrics)).sum());
    let floor = ((MIN_LOT_THICKNESS + street * 0.5) / span).min(0.4);
    let ratio = (first_weight / total).clamp(floor, 1.0 - floor);

    // Everything standing either side of this split has to cross it to leave,
    // so the whole subtree is what the street carries.
    let traffic: usize = nodes.iter().map(|node| arrivals(&node.metrics)).sum();

    let (first_rect, second_rect) = if rect.width >= rect.height {
        let width = rect.width * ratio;
        let street = street_width(traffic, rect.width, width);
        let half = street * 0.5;
        corridors.push(Corridor {
            start: (rect.x + width, rect.y),
            end: (rect.x + width, rect.y + rect.height),
            width: street,
            traffic,
        });
        (
            Rect {
                width: width - half,
                ..rect
            },
            Rect {
                x: rect.x + width + half,
                width: rect.width - width - half,
                ..rect
            },
        )
    } else {
        let height = rect.height * ratio;
        let street = street_width(traffic, rect.height, height);
        let half = street * 0.5;
        corridors.push(Corridor {
            start: (rect.x, rect.y + height),
            end: (rect.x + rect.width, rect.y + height),
            width: street,
            traffic,
        });
        (
            Rect {
                height: height - half,
                ..rect
            },
            Rect {
                y: rect.y + height + half,
                height: rect.height - height - half,
                ..rect
            },
        )
    };

    partition_recursive(&nodes[..split], first_rect, corridors, output);
    partition_recursive(&nodes[split..], second_rect, corridors, output);
}

/// How much of a cell a split may reserve for its street.
///
/// The width a road's traffic earns it is only ever a request. A cell that has
/// already been subdivided a dozen times has no room to grant it, and paving
/// over the last of the ground would leave the holdings either side with
/// nothing to stand on — so the request is capped against both the cell and
/// the smaller of the two halves it is being cut from.
fn street_width(traffic: usize, span: f32, offset: f32) -> f32 {
    let smaller = offset.min(span - offset);
    corridor_width(traffic)
        .min(span * 0.2)
        .min(smaller * 0.8)
        .max(0.0)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::build::model::NodeKind;

    fn file(name: &str, bytes: u64) -> Node {
        Node {
            name: name.to_owned(),
            relative_path: PathBuf::from(name),
            kind: NodeKind::File {
                category: Category::Source,
            },
            metrics: Metrics {
                bytes,
                lines: bytes as usize / 10,
                file_count: 1,
                ..Metrics::default()
            },
            children: Vec::new(),
        }
    }

    /// A holding standing on a lot big enough not to cap its height, so a test
    /// measures the formula rather than the ceiling.
    fn holding(lines: usize, code_lines: usize, complexity: usize) -> Building {
        Building {
            name: "module.rs".to_owned(),
            path: "src/module.rs".to_owned(),
            ward_id: None,
            category: Category::Source,
            kind: BuildingKind::Guildhall,
            lot: Rect {
                x: 0.0,
                y: 0.0,
                width: 400.0,
                height: 400.0,
            },
            bytes: 4_096,
            lines,
            code_lines,
            complexity,
            references: 0,
            scale: 1.0,
        }
    }

    #[test]
    fn height_follows_tangle_rather_than_length() {
        // The point of the axis. A long plain file has plenty of code and
        // little branching, so it should read as a broad low hall; a short
        // knotty one should stand up. Length is the footprint's job.
        let long_and_plain = holding(4_000, 3_600, 180);
        let short_and_dense = holding(300, 260, 60);

        assert!(
            short_and_dense.height() > long_and_plain.height(),
            "a {:.0}-line plain file stood {:.1} tall against {:.1} for a dense {:.0}-line one",
            long_and_plain.code_lines,
            long_and_plain.height(),
            short_and_dense.height(),
            short_and_dense.code_lines,
        );
    }

    #[test]
    fn a_file_the_scanner_does_not_score_sits_at_the_floor() {
        // Documentation, config and assets never get a complexity score, so
        // they must land on the minimum rather than on a divide-by-zero.
        let unscored = holding(500, 0, 0);
        assert_eq!(unscored.height(), 11.0);
    }

    #[test]
    fn length_alone_no_longer_buys_height() {
        // The regression the change was made for: with ln(lines) driving the
        // number, every large file pinned against the 54 clamp and the map
        // could not tell them apart. Same density, ten times the code.
        let modest = holding(400, 360, 36);
        let enormous = holding(4_000, 3_600, 360);

        assert!(
            (modest.height() - enormous.height()).abs() < 1e-3,
            "length shifted height from {:.1} to {:.1}",
            modest.height(),
            enormous.height(),
        );
    }

    /// A file of a given length, which is what a lot is now measured from.
    fn text_file(name: &str, path: &str, lines: usize) -> Node {
        Node {
            name: name.to_owned(),
            relative_path: PathBuf::from(path),
            kind: NodeKind::File {
                category: Category::Source,
            },
            metrics: Metrics {
                // Deliberately unrelated to `lines`: nothing about a lot may
                // read the byte count any more.
                bytes: 1_000,
                lines,
                code_lines: lines,
                file_count: 1,
                ..Metrics::default()
            },
            children: Vec::new(),
        }
    }

    /// A settlement of one ward holding exactly these files, by name and length.
    fn ward_of(files: &[(&str, usize)]) -> CityLayout {
        let mut root = Node::directory("project".to_owned(), PathBuf::new());
        let mut ward = Node::directory("src".to_owned(), PathBuf::from("src"));
        ward.children = files
            .iter()
            .map(|(name, lines)| text_file(name, &format!("src/{name}"), *lines))
            .collect();
        for child in &ward.children {
            ward.metrics.add(child.metrics);
        }
        root.metrics = ward.metrics;
        root.children = vec![ward];
        CityLayout::build(&root)
    }

    fn lot_area_of(city: &CityLayout, name: &str) -> f32 {
        city.buildings
            .iter()
            .find(|building| building.name == name)
            .unwrap_or_else(|| panic!("{name} stands somewhere"))
            .lot
            .area()
    }

    /// **The King's own rule.** Twice the content, twice the ground.
    #[test]
    fn twice_the_lines_buys_about_twice_the_lot() {
        // Four files so the recursive split has real work to do, and lengths
        // well clear of `MIN_HOLDING_LINES` so the floor is not what is being
        // measured. Each is twice the one before it.
        let city = ward_of(&[
            ("one.rs", 200),
            ("two.rs", 400),
            ("four.rs", 800),
            ("eight.rs", 1_600),
        ]);

        for (smaller, larger) in [
            ("one.rs", "two.rs"),
            ("two.rs", "four.rs"),
            ("four.rs", "eight.rs"),
        ] {
            let ratio = lot_area_of(&city, larger) / lot_area_of(&city, smaller);
            assert!(
                (1.7..=2.3).contains(&ratio),
                "{larger} has twice the lines of {smaller} but {ratio:.2}x the ground"
            );
        }
    }

    /// The fall-off the King reported, stated as he stated it.
    ///
    /// Under `20 + √bytes` this pair came out about 11x apart rather than a
    /// hundred, because a square root spends most of its range before the
    /// files that matter even start. Nothing may compress the ratio again.
    ///
    /// The two are given a street of ordinary neighbours to stand among,
    /// because a settlement of *only* these two is barely a hundred units
    /// across and `MIN_LOT_THICKNESS` — rightly — will not cut a 117-unit cell
    /// a hundred ways. A hundredfold difference needs a town big enough to
    /// draw it, which any real repository holding a 10,000-line file is.
    #[test]
    fn a_hundred_times_the_lines_buys_about_a_hundred_times_the_ground() {
        let mut files: Vec<(String, usize)> = (0..20).map(|i| (format!("f{i}.rs"), 300)).collect();
        files.push(("small.rs".to_owned(), 100));
        files.push(("large.rs".to_owned(), 10_000));
        let named: Vec<(&str, usize)> = files.iter().map(|(n, l)| (n.as_str(), *l)).collect();

        let city = ward_of(&named);

        let ratio = lot_area_of(&city, "large.rs") / lot_area_of(&city, "small.rs");
        assert!(
            (70.0..=140.0).contains(&ratio),
            "a 100x longer file got {ratio:.1}x the ground"
        );
    }

    /// The house has to carry the proportion too, or the lot's honesty never
    /// reaches the eye: this is what a constant `LOT_COVERAGE` is for.
    #[test]
    fn the_house_grows_with_its_lot_and_not_with_its_name() {
        let city = ward_of(&[("one.rs", 300), ("two.rs", 600)]);
        let house = |name: &str| {
            let footprint = city
                .buildings
                .iter()
                .find(|building| building.name == name)
                .expect("the file stands somewhere")
                .footprint();
            footprint.area()
        };

        let ratio = house("two.rs") / house("one.rs");
        assert!(
            (1.7..=2.3).contains(&ratio),
            "twice the lines covered {ratio:.2}x the ground"
        );
    }

    /// A file nobody could read is not a file with nothing in it.
    #[test]
    fn a_file_the_scanner_never_opened_gets_a_nominal_lot() {
        // `scan` leaves `lines` at zero for a binary or an over-large file
        // while still recording its size, so a rule that read `bytes` would
        // hand this bundle most of the ward on the strength of no content at
        // all.
        let mut bundle = text_file("bundle.min.js", "src/bundle.min.js", 0);
        bundle.metrics.bytes = 3_500_000;
        bundle.metrics.code_lines = 0;

        let mut root = Node::directory("project".to_owned(), PathBuf::new());
        let mut ward = Node::directory("src".to_owned(), PathBuf::from("src"));
        ward.children = vec![bundle, text_file("module.rs", "src/module.rs", 120)];
        for child in &ward.children {
            ward.metrics.add(child.metrics);
        }
        root.metrics = ward.metrics;
        root.children = vec![ward];

        let city = CityLayout::build(&root);
        let ratio = lot_area_of(&city, "bundle.min.js") / lot_area_of(&city, "module.rs");
        assert!(
            (0.7..=1.4).contains(&ratio),
            "an unread 3.5 MB bundle took {ratio:.1}x the ground of a 120-line file"
        );
    }

    /// What a folder is given and what it hands out have to be the same number,
    /// or the proportion leaks away at every level of the recursion.
    #[test]
    fn a_folder_weighs_exactly_what_its_contents_weigh() {
        let mut inner = Node::directory("inner".to_owned(), PathBuf::from("src/inner"));
        inner.children = vec![
            text_file("a.rs", "src/inner/a.rs", 300),
            text_file("b.rs", "src/inner/b.rs", 700),
        ];
        let mut ward = Node::directory("src".to_owned(), PathBuf::from("src"));
        ward.children = vec![inner, text_file("c.rs", "src/c.rs", 1_000)];

        assert_eq!(weight(&ward), 2_000.0);
        assert_eq!(weight(&ward.children[0]), 1_000.0);
    }

    /// The floor, and the fact that it is a floor rather than a curve.
    #[test]
    fn a_file_too_short_to_draw_lands_on_the_floor_and_no_lower() {
        let stub = text_file("mod.rs", "src/mod.rs", 3);
        let short = text_file("tiny.rs", "src/tiny.rs", 30);
        let above = text_file("real.rs", "src/real.rs", 600);

        // Everything below the floor weighs the same -- that is the price of
        // keeping a house big enough to stand up.
        assert_eq!(weight(&stub), MIN_HOLDING_LINES);
        assert_eq!(weight(&short), MIN_HOLDING_LINES);
        // And everything above it is its own length exactly, with no curve.
        assert_eq!(weight(&above), 600.0);
    }

    #[test]
    fn gives_each_file_a_non_overlapping_lot() {
        let mut root = Node::directory("project".to_owned(), PathBuf::new());
        root.children = vec![
            file("main.rs", 1000),
            file("lib.rs", 3000),
            file("cli.rs", 500),
        ];
        root.metrics = Metrics {
            bytes: 4500,
            lines: 450,
            file_count: 3,
            ..Metrics::default()
        };

        let city = CityLayout::build(&root);
        assert_eq!(city.buildings.len(), 3);
        assert!(
            city.buildings
                .iter()
                .all(|building| building.lot.area() > 0.0)
        );
        for (index, first) in city.buildings.iter().enumerate() {
            for second in city.buildings.iter().skip(index + 1) {
                let overlaps = first.lot.x < second.lot.x + second.lot.width
                    && first.lot.x + first.lot.width > second.lot.x
                    && first.lot.y < second.lot.y + second.lot.height
                    && first.lot.y + first.lot.height > second.lot.y;
                assert!(!overlaps);
            }
        }
    }

    #[test]
    fn keeps_dense_wards_uncongested() {
        // A ward holding hundreds of files is where lots used to collapse:
        // fixed road insets ate the cell, footprints overflowed their lot and
        // heights ignored the ground, so every holding buried its neighbours.
        let mut root = Node::directory("project".to_owned(), PathBuf::new());
        let mut ward = Node::directory("src".to_owned(), PathBuf::from("src"));
        ward.children = (0..400)
            .map(|index| {
                let mut node = file(&format!("module_{index}.rs"), 400 + index as u64 * 25);
                node.relative_path = PathBuf::from(format!("src/module_{index}.rs"));
                node.metrics.lines = 40 + index * 3;
                node.metrics.code_lines = 30 + index * 2;
                node.metrics.complexity = index % 40;
                node
            })
            .collect();
        ward.metrics = Metrics {
            bytes: 4_000_000,
            lines: 400_000,
            file_count: 400,
            ..Metrics::default()
        };
        root.metrics = ward.metrics;
        root.children = vec![ward];

        let city = CityLayout::build(&root);
        assert_eq!(city.buildings.len(), 400);

        for building in &city.buildings {
            let footprint = building.footprint();
            let lot = building.lot;
            assert!(
                footprint.x >= lot.x
                    && footprint.y >= lot.y
                    && footprint.x + footprint.width <= lot.x + lot.width + 1e-3
                    && footprint.y + footprint.height <= lot.y + lot.height + 1e-3,
                "{} overflows its lot",
                building.path
            );
            // A holding may not tower over the ground it owns, or it hides
            // every neighbour standing behind it in the isometric view.
            let plan = footprint.width.min(footprint.height);
            assert!(
                building.height() <= (plan * 1.9).max(3.0) + 1e-3,
                "{} is {} tall on a {} plan",
                building.path,
                building.height(),
                plan
            );
        }

        let footprints: Vec<Rect> = city
            .buildings
            .iter()
            .map(|building| building.footprint())
            .collect();
        for (index, first) in footprints.iter().enumerate() {
            for second in footprints.iter().skip(index + 1) {
                let overlaps = first.x < second.x + second.width
                    && first.x + first.width > second.x
                    && first.y < second.y + second.height
                    && first.y + first.height > second.y;
                assert!(!overlaps, "buildings overlap in a dense ward");
            }
        }
    }

    #[test]
    fn assigns_architecture_from_file_role() {
        let mut entrypoint = file("main.rs", 1000);
        entrypoint.relative_path = PathBuf::from("src/main.rs");
        let mut test = file("widget.spec.rs", 500);
        test.kind = NodeKind::File {
            category: Category::Test,
        };
        let mut docs = file("guide.md", 500);
        docs.kind = NodeKind::File {
            category: Category::Docs,
        };

        assert_eq!(building_kind(&entrypoint), BuildingKind::Keep);
        assert_eq!(building_kind(&test), BuildingKind::Watchtower);
        assert_eq!(building_kind(&docs), BuildingKind::Scriptorium);
    }

    /// A repository of `wards` folders holding `per_ward` files each.
    fn repository(name: &str, wards: usize, per_ward: usize) -> Node {
        let mut root = Node::directory(name.to_owned(), PathBuf::new());
        for ward_index in 0..wards {
            let ward_name = format!("ward_{ward_index}");
            let mut ward = Node::directory(ward_name.clone(), PathBuf::from(ward_name.clone()));
            ward.children = (0..per_ward)
                .map(|index| {
                    let mut node = file(&format!("module_{index}.rs"), 1_200 + index as u64 * 40);
                    node.relative_path = PathBuf::from(format!("{ward_name}/module_{index}.rs"));
                    node.metrics.lines = 60 + index * 2;
                    node
                })
                .collect();
            ward.metrics = Metrics {
                bytes: ward.children.iter().map(|node| node.metrics.bytes).sum(),
                lines: ward.children.iter().map(|node| node.metrics.lines).sum(),
                file_count: per_ward,
                ..Metrics::default()
            };
            root.metrics.bytes += ward.metrics.bytes;
            root.metrics.lines += ward.metrics.lines;
            root.metrics.file_count += per_ward;
            root.children.push(ward);
        }
        root
    }

    fn median_lot_area(city: &CityLayout) -> f32 {
        let mut areas: Vec<f32> = city
            .buildings
            .iter()
            .map(|building| building.lot.area())
            .collect();
        areas.sort_by(f32::total_cmp);
        areas[areas.len() / 2]
    }

    #[test]
    fn a_holding_is_the_same_size_however_large_the_repository() {
        // The whole point of sizing the island from its contents. A hundred
        // times the files must buy a hundred times the ground, not the same
        // ground divided a hundred ways.
        let small = CityLayout::build(&repository("small", 4, 5));
        let large = CityLayout::build(&repository("large", 4, 500));

        let ratio = median_lot_area(&large) / median_lot_area(&small);
        assert!(
            (0.5..2.0).contains(&ratio),
            "a holding in a 2000 file repository is {ratio}x one in a 20 file repository"
        );
        assert!(
            large.extent.area() > small.extent.area() * 20.0,
            "the island did not grow with the repository"
        );
    }

    #[test]
    fn a_town_is_the_size_it_would_be_on_its_own() {
        // A repository must not be shrunk by the company it keeps: the town it
        // occupies in a realm is exactly the island it would have had alone.
        let alone = CityLayout::build(&repository("modest", 3, 12));
        let repositories: Vec<Repository> =
            [repository("modest", 3, 12), repository("enormous", 6, 400)]
                .into_iter()
                .map(|root| Repository {
                    root,
                    source_path: PathBuf::new(),
                    omitted_files: 0,
                })
                .collect();

        let world = WorldLayout::build(&repositories);
        let town = world
            .towns
            .iter()
            .find(|town| town.name == "modest")
            .expect("the modest repository has a town");

        assert!((town.rect.width - alone.extent.width).abs() < 1e-3);
        assert!((town.rect.height - alone.extent.height).abs() < 1e-3);

        let ratio = median_lot_area(&town.city) / median_lot_area(&alone);
        assert!(
            (ratio - 1.0).abs() < 1e-3,
            "holdings changed size by {ratio}x when the repository joined a realm"
        );
        for building in &town.city.buildings {
            assert_eq!(building.scale, 1.0, "a holding was scaled to fit its town");
        }
    }

    #[test]
    fn the_island_grows_to_hold_every_town() {
        let repositories: Vec<Repository> = (0..6)
            .map(|index| Repository {
                root: repository(&format!("repo_{index}"), 3, 40 + index * 30),
                source_path: PathBuf::new(),
                omitted_files: 0,
            })
            .collect();

        let world = WorldLayout::build(&repositories);
        let occupied: f32 = world.towns.iter().map(|town| town.rect.area()).sum();
        assert!(
            world.extent.area() > occupied,
            "the island is smaller than the towns standing on it"
        );
        for town in &world.towns {
            assert!(town.rect.x >= world.extent.x);
            assert!(town.rect.y >= world.extent.y);
            assert!(town.rect.x + town.rect.width <= world.extent.x + world.extent.width);
            assert!(town.rect.y + town.rect.height <= world.extent.y + world.extent.height);
        }
    }

    #[test]
    fn packs_repository_towns_without_overlap() {
        let repositories: Vec<Repository> = [
            ("large", 1400),
            ("medium", 500),
            ("small", 80),
            ("tiny", 10),
        ]
        .into_iter()
        .map(|(name, files)| {
            let mut root = Node::directory(name.to_owned(), PathBuf::new());
            root.metrics = Metrics {
                bytes: files as u64 * 1000,
                lines: files * 20,
                file_count: files,
                ..Metrics::default()
            };
            Repository {
                root,
                source_path: PathBuf::from(name),
                omitted_files: 0,
            }
        })
        .collect();

        let world = WorldLayout::build(&repositories);

        assert_eq!(world.towns.len(), repositories.len());
        for (index, first) in world.towns.iter().enumerate() {
            assert!(first.rect.x >= world.extent.x && first.rect.y >= world.extent.y);
            assert!(first.rect.x + first.rect.width <= world.extent.x + world.extent.width);
            assert!(first.rect.y + first.rect.height <= world.extent.y + world.extent.height);
            for second in world.towns.iter().skip(index + 1) {
                assert!(!rects_overlap(first.rect, second.rect, 0.0));
            }
        }
    }
}
