//! Where each city stands, and the roads between them.
//!
//! Layout lives in `kingdom-core`, not in the UI, for one reason: it must be
//! **stable**. A city has to land in the same spot on every render and every
//! reload, or the King loses the spatial memory that makes a map worth having
//! at all. Keeping it a pure function makes that testable.
//!
//! ## What replaced the spiral, and why
//!
//! The previous layout was a phyllotactic spiral indexed by position in the
//! city list. It had two problems, one cosmetic and one serious.
//!
//! The serious one: **cities were placed by list index, and the list is sorted
//! by name.** Creating a project called `aardvark` shifted every
//! alphabetically-later city into a different spiral slot, so the entire map
//! rearranged itself. That defeats the exact property this module exists to
//! protect. The old `spiral_layout_is_deterministic` test did not catch it,
//! because it only ever compared the same list against itself: determinism was
//! tested, *stability under insertion* was not.
//!
//! The cosmetic one: a golden-angle spiral is engineered to distribute points
//! **uniformly**, which is the opposite of how land gets settled. Every city sat
//! the same distance from its neighbours in faint concentric rings, and position
//! encoded nothing but alphabetical order.
//!
//! ## How this version works
//!
//! The continent is covered in a fixed lattice of **settlement slots**, jittered
//! and hex-offset so no grid is visible, and filtered down to those sitting
//! comfortably inland. Slots are a property of the *terrain*, not of the city
//! list.
//!
//! Each city then hashes **its own id** to a preferred spot and claims the
//! nearest free slot, in a priority order that is also a hash of its id. Two
//! consequences, both of which the old layout got wrong:
//!
//! - A city's position depends only on its own identity and on the cities that
//!   outrank it. Adding a project leaves every higher-ranked city bit-identical
//!   and displaces at most a neighbour or two. Pinned by
//!   [`tests::adding_a_city_does_not_move_the_others`].
//! - Position means something: cities are drawn toward the province of their own
//!   stack, so a kingdom grows a Rust highland and a Node coast.

use crate::model::{City, CityKind};
use crate::terrain::{Terrain, SEA_LEVEL};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// The largest footprint any city can have.
///
/// Must match the ceiling in [`city_radius`]. Slot spacing is derived from this
/// rather than from the kingdom's actual contents, which is what lets the
/// lattice stay fixed while projects come and go -- if spacing tracked the
/// largest city present, scanning one big repo would move every other city.
const MAX_CITY_RADIUS: f64 = 96.0;

/// Distance between neighbouring settlement slots.
///
/// The invariant that makes non-overlap free: `SLOT_PITCH - 2 * SLOT_JITTER`
/// must exceed `2 * MAX_CITY_RADIUS`, so even two maximum-size cities in the two
/// closest, most adversely jittered slots cannot touch. Pinned by
/// [`tests::cities_never_overlap`].
const SLOT_PITCH: f64 = 330.0;

/// How far a slot may wander from its lattice point.
///
/// Large enough to break up the grid, small enough to preserve the clearance
/// above: `330 - 2*55 = 220 > 192`.
const SLOT_JITTER: f64 = 55.0;

/// Vertical row spacing, as a fraction of the pitch.
///
/// Hexagonal packing (`sqrt(3)/2`) with alternate rows offset by half a pitch.
/// A square lattice betrays itself immediately as rows and columns; a triangular
/// one reads as scattered.
const ROW_RATIO: f64 = 0.866_025_403_784_438_6;

/// Granularity by which the island's radius grows.
///
/// The continent must not resize every time a project appears, or the whole map
/// rescales and spatial memory is lost again through a different door.
/// Quantising growth means the island stays put for most additions and
/// occasionally makes one visible jump -- a legible "the realm expanded" event
/// rather than constant churn.
const EXTENT_STEP: f64 = SLOT_PITCH * 2.0;

/// Smallest island, so a one-city kingdom still looks like a place.
const MIN_EXTENT: f64 = SLOT_PITCH * 3.0;

/// How many settlement slots the island must offer per city.
///
/// Sizing the land to *exactly* fit its cities is wrong twice over. It looks
/// wrong -- a fully tiled board reads as a game grid, and it is the empty
/// countryside between towns that makes a realm feel settled rather than
/// assembled. And it behaves wrong: on a saturated island the nearest free slot
/// can be right across the map, so one new project sends a displaced neighbour
/// hundreds of units away. Headroom keeps displacement local, which is what
/// [`tests::adding_a_city_does_not_move_the_others`] measures.
const SLOT_HEADROOM: f64 = 2.2;

/// How far from the island's centre a province's heartland sits.
const PROVINCE_RING: f64 = 0.52;

/// How far a city may scatter from its province anchor, as a fraction of extent.
///
/// Loose on purpose: provinces should read as tendencies, not as bins with
/// walls. A kingdom of one stack must still fill its island naturally.
const PROVINCE_SPREAD: f64 = 0.30;

// ---------------------------------------------------------------------------
// Output
// ---------------------------------------------------------------------------

/// A city's computed position, in ground coordinates.
///
/// These are coordinates on the isometric **ground plane**, not screen
/// coordinates: the renderer passes them through the same `iso()` the buildings
/// use. That shared projection is what makes the kingdom one continuous place
/// rather than a set of dioramas pinned to a flat backdrop.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CityPlacement {
    pub x: f64,
    pub y: f64,
    /// Radius of the city's footprint, scaled by project size.
    pub radius: f64,
    /// Ground height, in world units, from the terrain beneath it.
    pub elevation: f64,
}

/// A road joining two cities.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Road {
    /// Indices into [`Realm::placements`].
    pub from: usize,
    pub to: usize,
    /// Perpendicular offset of the road's midpoint, in world units.
    ///
    /// Roads follow the land; a dead-straight line between two cities reads as a
    /// surveyor's diagram. The renderer draws a quadratic curve through it.
    pub bend: f64,
}

/// A laid-out kingdom: the land, the cities on it, and the roads between them.
#[derive(Debug, Clone, PartialEq)]
pub struct Realm {
    pub terrain: Terrain,
    pub placements: Vec<CityPlacement>,
    pub roads: Vec<Road>,
}

// ---------------------------------------------------------------------------
// Settling
// ---------------------------------------------------------------------------

/// Settles a kingdom's cities on its land.
///
/// `root` seeds the terrain and nothing else; the cities decide only how large
/// the island needs to be.
pub fn settle_kingdom(root: &str, cities: &[City]) -> Realm {
    let terrain = fit_terrain(root, cities.len());
    let slots = settlement_slots(&terrain);

    let mut placements = vec![
        CityPlacement {
            x: 0.0,
            y: 0.0,
            radius: 0.0,
            elevation: 0.0,
        };
        cities.len()
    ];

    // Claim order is a hash of city identity, never list position. This is what
    // confines the blast radius of adding a project: every city that outranks
    // the newcomer is placed against exactly the same set of taken slots as
    // before, so its position cannot change at all.
    let mut order: Vec<usize> = (0..cities.len()).collect();
    order.sort_by(|&a, &b| {
        let ka = hash_str(cities[a].id.as_str());
        let kb = hash_str(cities[b].id.as_str());
        // The id tie-break keeps two cities that hash alike deterministic.
        ka.cmp(&kb)
            .then_with(|| cities[a].id.as_str().cmp(cities[b].id.as_str()))
    });

    let mut taken = vec![false; slots.len()];

    for &i in &order {
        let city = &cities[i];
        let want = preferred_spot(city, &terrain);

        // Nearest free slot to the city's own preference. Provinces emerge from
        // the preference; the lattice guarantees the spacing.
        let chosen = slots
            .iter()
            .enumerate()
            .filter(|(s, _)| !taken[*s])
            .min_by(|(_, a), (_, b)| {
                let da = (a.0 - want.0).powi(2) + (a.1 - want.1).powi(2);
                let db = (b.0 - want.0).powi(2) + (b.1 - want.1).powi(2);
                da.partial_cmp(&db).unwrap_or(std::cmp::Ordering::Equal)
            })
            .map(|(s, _)| s);

        // `fit_terrain` sizes the island so this cannot happen; falling back to
        // the preferred point keeps a city on the map rather than dropping it,
        // because a missing city is a far worse lie than a crowded one.
        let (x, y) = match chosen {
            Some(s) => {
                taken[s] = true;
                slots[s]
            }
            None => want,
        };

        placements[i] = CityPlacement {
            x,
            y,
            radius: city_radius(city),
            elevation: terrain.height(x, y),
        };
    }

    let roads = road_network(&placements, &terrain);

    Realm {
        terrain,
        placements,
        roads,
    }
}

/// Grows the island in quantised steps until it holds every city.
///
/// Looping until the slots fit -- rather than computing an area formula -- keeps
/// this honest about the fact that usable land is an irregular shape, not a
/// disc: a ragged coastline can easily cost a third of the naive area.
fn fit_terrain(root: &str, cities: usize) -> Terrain {
    let mut extent = MIN_EXTENT;
    let wanted = ((cities as f64) * SLOT_HEADROOM).ceil() as usize;

    loop {
        let terrain = Terrain::for_kingdom(root, extent);
        if settlement_slots(&terrain).len() >= wanted {
            return terrain;
        }

        // Guard against a pathological seed producing an unusable island.
        if extent > MIN_EXTENT + EXTENT_STEP * 400.0 {
            return terrain;
        }
        extent += EXTENT_STEP;
    }
}

/// Every spot on the island where a city could stand.
///
/// A slot qualifies only if the city's whole footprint would be inland, tested
/// around its rim rather than at its centre. Checking the centre alone lets a
/// coastal city hang half its buildings over the water, which looks like a bug
/// even though the placement is technically "on land".
fn settlement_slots(terrain: &Terrain) -> Vec<(f64, f64)> {
    let span = terrain.extent();
    let rows = ((span * 2.0) / (SLOT_PITCH * ROW_RATIO)).ceil() as i64 + 1;
    let cols = ((span * 2.0) / SLOT_PITCH).ceil() as i64 + 1;

    let mut out = Vec::new();

    for r in -rows..=rows {
        for c in -cols..=cols {
            // Offset alternate rows: a triangular lattice, not a square one.
            let offset = if r.rem_euclid(2) == 0 { 0.0 } else { 0.5 };
            let base_x = (c as f64 + offset) * SLOT_PITCH;
            let base_y = r as f64 * SLOT_PITCH * ROW_RATIO;

            // Jitter is a property of the lattice point, seeded by the terrain,
            // so it never depends on which city ends up here.
            let jx = (terrain.jitter(c * 2, r * 2) - 0.5) * 2.0 * SLOT_JITTER;
            let jy = (terrain.jitter(c * 2 + 1, r * 2 + 1) - 0.5) * 2.0 * SLOT_JITTER;

            let (x, y) = (base_x + jx, base_y + jy);

            if footprint_is_inland(terrain, x, y) {
                out.push((x, y));
            }
        }
    }

    out
}

/// True when a maximum-size city centred here would sit entirely on land.
fn footprint_is_inland(terrain: &Terrain, x: f64, y: f64) -> bool {
    if !terrain.is_land(x, y) {
        return false;
    }

    // Eight points around the rim: enough to reject a slot straddling a shore,
    // cheap enough to run over every lattice point.
    const SAMPLES: usize = 8;
    for i in 0..SAMPLES {
        let angle = (i as f64 / SAMPLES as f64) * std::f64::consts::TAU;
        let px = x + angle.cos() * MAX_CITY_RADIUS;
        let py = y + angle.sin() * MAX_CITY_RADIUS;
        if terrain.elevation(px, py) <= SEA_LEVEL {
            return false;
        }
    }

    true
}

/// Where a city would most like to stand: inside its province, scattered by its
/// own identity.
fn preferred_spot(city: &City, terrain: &Terrain) -> (f64, f64) {
    let extent = terrain.extent();
    let h = hash_str(city.id.as_str());

    // The province anchor is fixed per stack, not per kingdom, so a kingdom's
    // changing composition never relocates an existing province.
    let kinds = 6.0;
    let kind_index = province_index(city.kind) as f64;
    let angle = (kind_index / kinds) * std::f64::consts::TAU + terrain.rotation();
    let anchor_x = angle.cos() * extent * PROVINCE_RING;
    let anchor_y = angle.sin() * extent * PROVINCE_RING;

    // Scatter within the province, from two independent slices of the hash.
    let a = unit(h) * std::f64::consts::TAU;
    // sqrt keeps the scatter even across the disc instead of bunched at the
    // centre, which would undo the point of scattering at all.
    let radius = unit(h.rotate_left(29)).sqrt() * extent * PROVINCE_SPREAD;

    (anchor_x + a.cos() * radius, anchor_y + a.sin() * radius)
}

/// Stable province index per stack.
fn province_index(kind: CityKind) -> usize {
    match kind {
        CityKind::Rust => 0,
        CityKind::Node => 1,
        CityKind::Python => 2,
        CityKind::Go => 3,
        CityKind::Mixed => 4,
        CityKind::Unknown => 5,
    }
}

/// Scales a city's footprint by file count, compressed logarithmically so a
/// monorepo does not dwarf a small library into invisibility.
fn city_radius(city: &City) -> f64 {
    let base = 38.0;
    let growth = ((city.file_count as f64).max(1.0)).ln() * 7.0;
    (base + growth).clamp(38.0, MAX_CITY_RADIUS)
}

// ---------------------------------------------------------------------------
// Roads
// ---------------------------------------------------------------------------

/// How much longer a road feels when it crosses water.
///
/// A penalty rather than a prohibition: the network must stay connected (see
/// [`tests::every_city_is_reachable_by_road`]), so a road may cross a bay when
/// there is no land route -- it simply reads as a bridge. Forbidding water
/// outright would strand cities on peninsulas.
const WATER_PENALTY: f64 = 6.0;

/// Builds the road network: a minimum spanning tree, plus a few short extra
/// links so it reads as a web rather than a tree.
///
/// The spanning tree is the point. "Every city is joined to the rest" is the
/// literal answer to a kingdom that looked like disconnected islands, and being
/// a spanning tree makes it true by construction rather than by hope.
fn road_network(placements: &[CityPlacement], terrain: &Terrain) -> Vec<Road> {
    let n = placements.len();
    if n < 2 {
        return Vec::new();
    }

    let cost = |a: usize, b: usize| {
        let (p, q) = (&placements[a], &placements[b]);
        let distance = ((p.x - q.x).powi(2) + (p.y - q.y).powi(2)).sqrt();
        distance * (1.0 + WATER_PENALTY * water_fraction(terrain, p, q))
    };

    // Prim's algorithm. O(n^2) is the right call here: n is the number of
    // projects in a dev folder, and a dense loop beats a heap at this size.
    let mut in_tree = vec![false; n];
    let mut best = vec![f64::MAX; n];
    let mut parent = vec![usize::MAX; n];
    let mut roads = Vec::with_capacity(n - 1);

    best[0] = 0.0;

    for _ in 0..n {
        let next = (0..n)
            .filter(|&i| !in_tree[i])
            .min_by(|&a, &b| {
                best[a]
                    .partial_cmp(&best[b])
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then(a.cmp(&b))
            })
            .expect("some vertex is always outside the tree");

        in_tree[next] = true;
        if parent[next] != usize::MAX {
            roads.push(make_road(parent[next], next, placements));
        }

        for other in 0..n {
            if in_tree[other] {
                continue;
            }
            let c = cost(next, other);
            if c < best[other] {
                best[other] = c;
                parent[other] = next;
            }
        }
    }

    // A pure tree looks skeletal; real road networks have loops. Add the
    // shortest few edges that are not already roads and do not wander.
    let existing: std::collections::BTreeSet<(usize, usize)> = roads
        .iter()
        .map(|r| (r.from.min(r.to), r.from.max(r.to)))
        .collect();

    let mut extras: Vec<(f64, usize, usize)> = Vec::new();
    for a in 0..n {
        for b in (a + 1)..n {
            if existing.contains(&(a, b)) {
                continue;
            }
            let c = cost(a, b);
            // Only genuinely neighbourly links; long-distance shortcuts across
            // the whole island read as noise, not as roads.
            if c < SLOT_PITCH * 1.9 {
                extras.push((c, a, b));
            }
        }
    }

    extras.sort_by(|x, y| {
        x.0.partial_cmp(&y.0)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(x.1.cmp(&y.1))
            .then(x.2.cmp(&y.2))
    });

    for (_, a, b) in extras.into_iter().take(n / 3) {
        roads.push(make_road(a, b, placements));
    }

    roads
}

fn make_road(from: usize, to: usize, placements: &[CityPlacement]) -> Road {
    // The bend is derived from the endpoints, so a road curves the same way on
    // every render -- the same stability requirement as everything else here.
    let (p, q) = (&placements[from], &placements[to]);
    let length = ((p.x - q.x).powi(2) + (p.y - q.y).powi(2)).sqrt();
    let h = hash_str(&format!("{from}:{to}"));
    let bend = (unit(h) - 0.5) * length * 0.18;

    Road { from, to, bend }
}

/// Roughly how much of a straight route between two cities lies under water.
fn water_fraction(terrain: &Terrain, a: &CityPlacement, b: &CityPlacement) -> f64 {
    const STEPS: usize = 12;
    let mut wet = 0;

    for i in 1..STEPS {
        let t = i as f64 / STEPS as f64;
        let x = a.x + (b.x - a.x) * t;
        let y = a.y + (b.y - a.y) * t;
        if !terrain.is_land(x, y) {
            wet += 1;
        }
    }

    wet as f64 / (STEPS - 1) as f64
}

// ---------------------------------------------------------------------------
// Hashing
// ---------------------------------------------------------------------------

/// FNV-1a, matching [`crate::terrain`]: identical on every target, which is what
/// makes an identity-derived position reproducible rather than merely stable
/// within one process.
fn hash_str(s: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// A hash's top 53 bits as a value in `0.0..1.0`.
fn unit(h: u64) -> f64 {
    (h >> 11) as f64 / ((1u64 << 53) as f64)
}

// ---------------------------------------------------------------------------
// Framing
// ---------------------------------------------------------------------------

/// The bounding box of a laid-out kingdom, used to frame the initial view.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Bounds {
    pub min_x: f64,
    pub min_y: f64,
    pub max_x: f64,
    pub max_y: f64,
}

impl Bounds {
    pub fn width(&self) -> f64 {
        self.max_x - self.min_x
    }

    pub fn height(&self) -> f64 {
        self.max_y - self.min_y
    }

    pub fn center(&self) -> (f64, f64) {
        (
            (self.min_x + self.max_x) / 2.0,
            (self.min_y + self.max_y) / 2.0,
        )
    }
}

/// How much sea to keep around the island when framing the view.
///
/// Deliberately tight, for the reason the old `bounds_of` gave and which still
/// holds: any more and the realm shrinks into the middle of the screen
/// surrounded by dead water, wasting the one view the King spends most of their
/// time looking at.
const SEA_MARGIN: f64 = 1.14;

/// The extent of the realm **on screen**, after isometric projection.
///
/// This frames the island plus a rim of sea, not merely the cities: the
/// coastline is what makes the map read as a place, and cropping it would leave
/// the island looking cut off.
///
/// Two things this gets right that the obvious implementation does not:
///
/// - It frames on the island's own radius, **not** on [`Terrain::span`]. The
///   sampled square is deliberately oversized so contour rings close inside it;
///   framing on it leaves the island filling barely half the frame.
/// - It treats the island as a **disc**, because that is what it is. Projecting
///   a bounding square's corners overestimates the width by `sqrt(2)`, since the
///   iso transform sends the square's diagonal onto the horizontal axis.
pub fn realm_bounds(realm: &Realm) -> Bounds {
    let r = realm.terrain.extent() * SEA_MARGIN;

    // A disc of radius `r` under `iso`: screen x spans +/- r*sqrt(2)*cos(30),
    // screen y spans +/- r*sqrt(2)/2.
    let half_w = r * std::f64::consts::SQRT_2 * 0.866_025_403_784_438_6;
    let half_h = r * std::f64::consts::SQRT_2 / 2.0;

    let mut b = Bounds {
        min_x: -half_w,
        min_y: -half_h,
        max_x: half_w,
        max_y: half_h,
    };

    // Cities stand above the ground plane and carry names below them, so the
    // island's own outline is not quite the whole picture.
    for p in &realm.placements {
        let (sx, sy) = crate::skyline::iso(p.x, p.y, p.elevation);
        b.min_x = b.min_x.min(sx - p.radius);
        b.min_y = b.min_y.min(sy - p.radius * 2.0);
        b.max_x = b.max_x.max(sx + p.radius);
        b.max_y = b.max_y.max(sy + p.radius);
    }

    b
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::CityId;

    fn city(name: &str, files: usize) -> City {
        City {
            id: CityId::new(name),
            name: name.into(),
            path: name.into(),
            kind: CityKind::Unknown,
            file_count: files,
            has_git: false,
            dirty_files: 0,
            structure: None,
        }
    }

    /// A kingdom shaped like a real dev folder: mixed stacks, mixed sizes.
    fn kingdom(count: usize) -> Vec<City> {
        (0..count)
            .map(|i| {
                let mut c = city(&format!("project-{i:02}"), (i * 137) % 4_000 + 1);
                c.kind = match i % 5 {
                    0 => CityKind::Rust,
                    1 => CityKind::Node,
                    2 => CityKind::Python,
                    3 => CityKind::Go,
                    _ => CityKind::Mixed,
                };
                c
            })
            .collect()
    }

    const ROOT: &str = "/home/king/dev";

    /// The regression test for this module's rewrite.
    ///
    /// The old spiral placed cities by list index over a name-sorted list, so
    /// adding a project that sorted first shifted *every* city into a different
    /// slot and the whole map rearranged. `spiral_layout_is_deterministic` never
    /// caught it, because it only ever compared a list against itself.
    ///
    /// Perfect immobility is not achievable -- slots are finite, so a newcomer
    /// can displace a neighbour -- but the disruption must be *local*, which is
    /// the difference between a kingdom that stays recognisable and one that
    /// does not.
    #[test]
    fn adding_a_city_does_not_move_the_others() {
        let before = kingdom(40);
        let mut after = before.clone();
        // Sorts first by name, which is precisely what broke the spiral.
        after.insert(0, city("aardvark", 500));

        let old = settle_kingdom(ROOT, &before);
        let new = settle_kingdom(ROOT, &after);

        let mut moved = 0;
        let mut worst: f64 = 0.0;

        for (i, was) in old.placements.iter().enumerate() {
            // The newcomer is at index 0, so everything shifts by one.
            let now = &new.placements[i + 1];
            let d = ((was.x - now.x).powi(2) + (was.y - now.y).powi(2)).sqrt();
            if d > 1e-9 {
                moved += 1;
                worst = worst.max(d);
            }
        }

        assert!(
            moved <= 3,
            "{moved} of 40 cities moved when one project was added"
        );
        assert!(
            worst <= SLOT_PITCH * 2.0,
            "a city was displaced {worst:.0} units, more than two slots away"
        );
    }

    /// Cities must never overlap, or the map becomes unreadable at exactly the
    /// moment it matters most: when the kingdom is large.
    ///
    /// This is load-bearing beyond itself. `build_skyline` keeps every building
    /// inside its city's radius, so this guarantee is what extends transitively
    /// to buildings never straying into a neighbouring city.
    #[test]
    fn cities_never_overlap() {
        for count in [1, 7, 40, 120] {
            let realm = settle_kingdom(ROOT, &kingdom(count));

            for (i, a) in realm.placements.iter().enumerate() {
                for (j, b) in realm.placements.iter().enumerate().skip(i + 1) {
                    let distance = ((a.x - b.x).powi(2) + (a.y - b.y).powi(2)).sqrt();
                    assert!(
                        distance > a.radius + b.radius,
                        "{count} cities: {i} and {j} overlap: gap {distance:.1} <= radii {:.1}",
                        a.radius + b.radius
                    );
                }
            }
        }
    }

    /// A city standing in the sea reads as broken software, and the inland test
    /// in `settlement_slots` is the only thing preventing it.
    #[test]
    fn every_city_stands_on_land() {
        for root in ["/home/king/dev", "/var/projects", ""] {
            for count in [1, 12, 60] {
                let realm = settle_kingdom(root, &kingdom(count));

                for (i, p) in realm.placements.iter().enumerate() {
                    assert!(
                        realm.terrain.is_land(p.x, p.y),
                        "{root:?} with {count} cities: city {i} is in the sea"
                    );
                }
            }
        }
    }

    /// The literal, checkable form of the complaint that prompted this rewrite:
    /// the kingdom looked like disconnected islands. If the road network ever
    /// splits into components, some city really is cut off from the rest.
    #[test]
    fn every_city_is_reachable_by_road() {
        for count in [2, 9, 40] {
            let realm = settle_kingdom(ROOT, &kingdom(count));

            // Union-find over the roads; one component means one joined realm.
            let mut parent: Vec<usize> = (0..count).collect();
            fn find(parent: &mut Vec<usize>, x: usize) -> usize {
                if parent[x] != x {
                    let root = find(parent, parent[x]);
                    parent[x] = root;
                }
                parent[x]
            }

            for road in &realm.roads {
                let (a, b) = (find(&mut parent, road.from), find(&mut parent, road.to));
                parent[a] = b;
            }

            let roots: std::collections::BTreeSet<usize> =
                (0..count).map(|i| find(&mut parent, i)).collect();
            assert_eq!(roots.len(), 1, "{count} cities split into several networks");
        }
    }

    /// The King navigates by spatial memory, which depends on a city not moving
    /// between reloads.
    #[test]
    fn settling_is_deterministic() {
        let cities = kingdom(30);
        assert_eq!(settle_kingdom(ROOT, &cities), settle_kingdom(ROOT, &cities));
    }
}
