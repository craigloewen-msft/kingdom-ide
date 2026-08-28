//! Planning the road network, from a graph of what the settlement contains.
//!
//! Roads used to be decoration. A line was drawn from the middle of one
//! top-level ward to the middle of another, with a random kink in it, and
//! painted over whatever happened to be standing in between — so it crossed
//! the very holdings it was meant to serve, and nothing below the top level
//! was joined to anything at all.
//!
//! The network is built in two halves, and neither of them searches for a
//! route:
//!
//! * **Inside a ward**, the streets are the [`Corridor`]s the layout reserved
//!   as it subdivided the ground. That subdivision is a k-d tree, so every
//!   cell edge is either an ancestor's split line or the ward boundary — which
//!   means each corridor already ends on another corridor. The streets meet
//!   because of how the land was divided, not because anything joined them up
//!   afterwards.
//! * **Between wards**, a graph is built over the wards, the loose holdings at
//!   the repository root, and the central square, and its minimum spanning
//!   tree becomes the avenues. Each avenue is routed in right angles, to match
//!   the streets it feeds, and stops at a *gate*: the point where the ward's
//!   own widest street reaches its boundary.
//!
//! Width everywhere comes from traffic — how many files must travel a road to
//! reach the central square. That is the one measure of "connectedness" the
//! scan actually supports: nothing reads imports or references, and a measure
//! built on them would leave assets, documentation, and configuration more
//! stranded than they already were.

use std::cmp::{Ordering, Reverse};
use std::collections::{BinaryHeap, HashMap};

use crate::map::{MapColor, MapPlaza, MapRect, MapRoad, RoadKind};

use crate::build::layout::{
    AVENUE_WIDENING, Building, Corridor, District, LOT_SETBACK, Rect, WARD_GAP, corridor_width,
};

/// Somewhere near the middle of the settlement with room for the square.
///
/// The square used to be dropped on the settlement's exact centre, which is
/// the one place the largest ward has almost certainly already claimed — so
/// the square sat on top of a district, and every avenue heading for it had to
/// cross that district's holdings to arrive. Stepping outward until the ground
/// is clear costs a little symmetry and buys a square that roads can actually
/// reach.
///
/// How far out that walk has to go depends on the ward it is escaping, so the
/// search runs until it has cleared everything rather than for a fixed number
/// of rings. A repository with one dominant top-level folder — `src` holding
/// everything — is the case that breaks a fixed bound: that ward covers most
/// of the map, the walk gives up inside it, and because a ward is solid ground
/// to the router every avenue then fails to find a way to the square and falls
/// back to a straight line through the holdings.
fn square_site(center: (f32, f32), size: f32, obstacles: &[Rect], lane: f32) -> Rect {
    let square = |at: (f32, f32)| Rect {
        x: at.0 - size * 0.5,
        y: at.1 - size * 0.5,
        width: size,
        height: size,
    };
    // Wide enough that a whole grid cell fits between the square and whatever
    // it is standing clear of, or the router cannot see the gap it is left.
    let margin = (size * 0.15).max(lane);
    let clear = |rect: Rect| {
        !obstacles
            .iter()
            .any(|other| rects_overlap(rect, *other, margin))
    };

    if clear(square(center)) {
        return square(center);
    }

    // Far enough to step outside the furthest corner of everything there is,
    // whatever shape the settlement turned out to be.
    let reach = obstacles
        .iter()
        .map(|rect| {
            let far_x = (rect.x - center.0)
                .abs()
                .max(rect.x + rect.width - center.0);
            let far_y = (rect.y - center.1)
                .abs()
                .max(rect.y + rect.height - center.1);
            far_x.hypot(far_y)
        })
        .fold(0.0f32, f32::max)
        + size
        + margin;

    let step = size * 0.6;
    let rings = ((reach / step).ceil() as usize).clamp(1, 4_000);
    for ring in 1..=rings {
        let radius = step * ring as f32;
        let points = (ring * 8).max(8);
        for index in 0..points {
            let angle = index as f32 / points as f32 * std::f32::consts::TAU;
            let candidate = square((
                center.0 + angle.cos() * radius,
                center.1 + angle.sin() * radius,
            ));
            if clear(candidate) {
                return candidate;
            }
        }
    }
    square(center)
}

fn rects_overlap(left: Rect, right: Rect, gap: f32) -> bool {
    left.x - gap < right.x + right.width
        && left.x + left.width + gap > right.x
        && left.y - gap < right.y + right.height
        && left.y + left.height + gap > right.y
}

/// The ground the road network has to be planned across.
///
/// Grown from the places themselves rather than taken from the island, because
/// the island includes a wide fringe of open country the network never needs
/// to reach — and every cell of it would be searched.
fn settlement_extent(obstacles: &[Rect], center: (f32, f32)) -> Rect {
    let mut min = (center.0, center.1);
    let mut max = (center.0, center.1);
    for rect in obstacles {
        min.0 = min.0.min(rect.x);
        min.1 = min.1.min(rect.y);
        max.0 = max.0.max(rect.x + rect.width);
        max.1 = max.1.max(rect.y + rect.height);
    }
    let margin = ((max.0 - min.0).max(max.1 - min.1) * 0.06).max(30.0);
    Rect {
        x: min.0 - margin,
        y: min.1 - margin,
        width: (max.0 - min.0) + margin * 2.0,
        height: (max.1 - min.1) + margin * 2.0,
    }
}

/// Paving for a road carrying almost nothing.
///
/// A quiet lane is dirt: dark, dull, and close enough to the ground it crosses
/// that it reads as a track rather than a route.
const LANE: MapColor = [99, 89, 67, 255];
/// Paving for the busiest road in the settlement.
///
/// The busiest road is pale dressed stone. The gap between this and [`LANE`]
/// is deliberately large — around a hundred levels of brightness against a
/// ground of seventy — because it is the only thing that says which way the
/// weight of the repository actually flows. The two used to sit twenty-five
/// levels apart, which on grass is no difference at all.
const TRUNK: MapColor = [216, 190, 134, 255];
const LANE_EDGE: MapColor = [66, 58, 42, 255];
const TRUNK_EDGE: MapColor = [128, 108, 68, 255];
const PLAZA: MapColor = [129, 105, 65, 255];
/// Paving for a highway between towns, which keeps its own darker colour so
/// the roads of the realm read apart from the roads inside a town.
const REALM_ROAD: MapColor = [126, 101, 61, 255];
const REALM_ROAD_EDGE: MapColor = [64, 54, 42, 255];

/// How wide a driveway is drawn for a file nothing refers to.
///
/// A genuine hairline, and that is the whole point of the number. Measured
/// across a real dev folder — five repositories, 2,174 files — **74% of every
/// drive on the map sits at exactly this floor**, because most files are
/// imported by nothing. The floor is therefore what the rest of the range is
/// read *against*, so every unit given to it is a unit of contrast taken from
/// the files that earned one. At 1.6 the mark for an unreferenced holding was
/// heavy enough that a file with a single reference looked no different; at 1.0
/// it still marks a front door and lets one arrival show.
const DRIVE_WIDTH: f32 = 1.0;

/// The widest a drive is drawn, before the wall it leaves has its own say.
///
/// Reference counts have a long tail — one or two files in a repository are
/// leaned on by dozens — so the curve is capped rather than left to run away
/// with the most depended-upon file in the tree.
///
/// It was 13, which the top of the range actually *reached*: the busiest doors
/// in a repository were flattened into each other at the one end where telling
/// them apart matters most. The cap is meant to catch a runaway, not to be
/// where the curve normally lands.
const DRIVE_MAX_WIDTH: f32 = 20.0;

/// How much of a drive's own frontage it may cover.
///
/// The wall a drive leaves is what bounds how wide it can be: a drive broader
/// than the house it serves reads as a blot against the wall rather than as a
/// path to a door.
const DRIVE_FRONTAGE_SHARE: f32 = 0.8;

/// How steeply a drive widens with each reference arriving at its door.
///
/// **Linear, at the King's word, and this is the slope of that line**: one world
/// unit of paving per file that imports this one, up to
/// [`DRIVE_LINEAR_REFERENCES`]. So a file imported four times draws a door four
/// units wider than one imported none, and one imported eight, eight — the same
/// rule a holding's own footprint follows.
///
/// It replaced `DRIVE_CURVE = 0.80`, an exponent, which was sub-linear
/// everywhere: a file imported sixteen times drew about nine times the paving of
/// a lone one rather than sixteen. That curve was itself a fix — it replaced the
/// street's own harder compression, which spent nearly all its slope on a range
/// references never enter — and the lesson survives it: what a mark is fitted to
/// is the distribution of the thing being drawn.
///
/// **One unit is what the geometry will actually carry, and that was measured
/// rather than assumed.** A drive is capped by the wall it leaves
/// ([`DRIVE_FRONTAGE_SHARE`]) and by the street it joins, and a slope the
/// geometry immediately overrides is not linear on screen — it is a linear rule
/// with a flat top. Scanning this repository, 155 holdings: at this slope the
/// frontage binds on 2% of them, and the median holding has an 18-unit wall in
/// front of a 2-unit drive. The caps stay as the backstop they are.
const DRIVE_SLOPE: f32 = 1.0;

/// How many references a drive is drawn exactly to scale for.
///
/// The knee: proportional up to here, compressed above it, out to
/// [`DRIVE_MAX_WIDTH`]. Fitted to the distribution rather than chosen — across
/// this repository the median holding has 1 inbound reference, p90 has 5, p99
/// has 10 and the busiest has 51; across a wider dev folder the busiest file in
/// five repositories had 44. A knee here leaves everything but a handful of hubs
/// in the strictly proportional part, and a hub is precisely where "very heavily
/// used" is a good enough answer.
const DRIVE_LINEAR_REFERENCES: f32 = 16.0;

/// How wide a drive serving a file with `references` inbound imports is,
/// given the wall it comes out of is `frontage` across.
///
/// **Linear** to [`DRIVE_LINEAR_REFERENCES`] — twice the references, twice the
/// paving above the floor — then a tail, so a runaway hub cannot swallow its own
/// house. [`crate::scale`] holds the shape and why the top of the range is
/// compressed at all.
///
/// The floor is exact: a file nothing imports gets [`DRIVE_WIDTH`] precisely,
/// which matters because most files are imported by nothing and that mark is
/// what every other drive is read against. The wall is a hard limit on top: a
/// drive broader than the house it serves stops looking like a drive.
fn drive_width(references: usize, frontage: f32) -> f32 {
    let span = DRIVE_MAX_WIDTH - DRIVE_WIDTH;
    // What the linear part spends, derived so its slope is exactly
    // `DRIVE_SLOPE` of paving per reference: the line climbs
    // `span * share / knee` per reference.
    let share = (DRIVE_SLOPE * DRIVE_LINEAR_REFERENCES / span).clamp(0.0, 1.0);
    let earned = crate::scale::linear_then_tail(references as f32, DRIVE_LINEAR_REFERENCES, share);
    let width = DRIVE_WIDTH + span * earned;
    let widest = DRIVE_MAX_WIDTH
        .min(frontage * DRIVE_FRONTAGE_SHARE)
        .max(DRIVE_WIDTH);
    width.clamp(DRIVE_WIDTH, widest)
}

/// The narrowest a street may be drawn, whatever the layout reserved.
///
/// A corridor in a deeply nested folder can be reserved at well under a world
/// unit across. Drawn honestly it would vanish, and the ward would look
/// unserved when it is not, so the thinnest lanes are drawn slightly wider
/// than their reservation.
const MIN_DRAWN_WIDTH: f32 = 2.4;

/// How much bare ground a road leaves between its edge and a holding.
const KERB: f32 = 0.5;

/// How much wider again a highway is drawn than an avenue carrying the same
/// traffic.
///
/// A highway crosses open country between whole towns, so it is read at a
/// distance and against nothing else; at that scale an avenue's width all but
/// disappears.
const HIGHWAY_WIDENING: f32 = 1.6;

/// A town as the realm's road network sees it.
pub struct Town {
    /// The ground the town stands on. A highway must stay off it, because
    /// everything built is inside it.
    pub rect: Rect,
    /// Journeys in the town, which is the traffic any highway serving it
    /// carries: every file, plus every reference reaching one.
    pub files: usize,
}

/// Plans the highways between towns, and where each one arrives.
///
/// This is the settlement plan one level up: towns are the places, a spanning
/// tree picks which pairs are worth a road, and the width follows the traffic
/// it carries — so the trunk roads of the realm read the same way the avenues
/// inside a town do.
///
/// Highways stop at the edge of a town rather than driving to its middle. The
/// old realm roads ran centre to centre with a random kink in them, which is
/// what put two of them straight through buildings: a town's centre is built
/// on. Stopping at the boundary and letting the town's own network come out to
/// meet the arrival point keeps a highway on open ground the whole way.
///
/// Returns the roads, and for each town the points where highways arrive
/// carrying how much traffic, to be handed to that town's
/// [`settlement_roads`].
pub fn highways(towns: &[Town], realm_center: (f32, f32)) -> (Vec<MapRoad>, Vec<Vec<Gateway>>) {
    let mut arrivals = vec![Vec::new(); towns.len()];
    if towns.len() < 2 {
        return (Vec::new(), arrivals);
    }

    let places: Vec<Place> = towns
        .iter()
        .map(|town| Place {
            center: town.rect.center(),
            files: town.files.max(1),
            gates: vec![front_door(town.rect, realm_center)],
            rect: Some(town.rect),
        })
        .collect();

    let edges = road_tree(&places);
    let traffic = edge_traffic(&places, &edges);

    let obstacles: Vec<Rect> = towns.iter().map(|town| town.rect).collect();
    let ground = Ground::new(
        settlement_extent(&obstacles, realm_center),
        &obstacles,
        WARD_GAP,
    );

    let mut roads = Vec::with_capacity(edges.len());
    for (index, &(parent, child)) in edges.iter().enumerate() {
        let carried = traffic[index];
        let target = nearest_gate(&places[parent], places[child].center);
        let from = nearest_gate(&places[child], target);
        let points = route(&ground, from, target);

        arrivals[child].push(Gateway {
            point: from,
            traffic: carried,
        });
        arrivals[parent].push(Gateway {
            point: target,
            traffic: carried,
        });

        roads.push(MapRoad {
            kind: RoadKind::Realm,
            points,
            width: (corridor_width(carried) * HIGHWAY_WIDENING).max(MIN_DRAWN_WIDTH),
            traffic: carried.min(u32::MAX as usize) as u32,
            color: REALM_ROAD,
            edge: REALM_ROAD_EDGE,
        });
    }

    (roads, arrivals)
}

/// Where a highway meets a town, and how much traffic it brings.
#[derive(Clone, Copy)]
pub struct Gateway {
    pub point: (f32, f32),
    /// Files on the far side of the highway. The town's own network carries
    /// them on to the square, so the spur out to the gateway is drawn as wide
    /// as the road it continues.
    pub traffic: usize,
}

/// One place in the settlement graph that roads can run between.
struct Place {
    center: (f32, f32),
    /// Journeys starting or ending here, which becomes this place's
    /// contribution to the traffic on every road between it and the central
    /// square. A file counts once for itself and once for every file that
    /// refers to it, so the way to a much-used holding is wide the whole way.
    files: usize,
    /// Where a road should actually stop, rather than barging on to the
    /// centre. Empty for the square itself.
    gates: Vec<(f32, f32)>,
    /// The ground this place occupies, which roads to elsewhere must avoid.
    rect: Option<Rect>,
}

/// Plans every road inside one settlement, and the square they meet at.
///
/// `corridors`, `districts`, and `buildings` must all already be in world
/// coordinates — for a realm that means after the town has been moved onto its
/// plot, so the network is planned where it will actually stand.
///
/// `gateways` are the points where highways from other towns arrive. They join
/// the graph as places in their own right, so the town's own spanning tree and
/// pathfinder bring a road out to meet each one, on open ground, the same way
/// they reach a ward. A settlement standing on its own has none.
pub fn settlement_roads(
    districts: &[District],
    buildings: &[Building],
    corridors: &[Corridor],
    settlement_center: (f32, f32),
    square_size: f32,
    gateways: &[Gateway],
    lane: f32,
) -> (Vec<MapRoad>, Option<MapPlaza>) {
    let wards: Vec<&District> = districts
        .iter()
        .filter(|district| district.depth == 0)
        .collect();
    let loose: Vec<&Building> = buildings
        .iter()
        .filter(|building| building.ward_id.is_none())
        .collect();
    if wards.is_empty() && loose.is_empty() {
        return (Vec::new(), None);
    }

    // Everything the square and the avenues have to keep out of.
    let occupied: Vec<Rect> = wards
        .iter()
        .map(|ward| ward.rect)
        .chain(loose.iter().map(|building| building.lot))
        .collect();
    let square = square_site(settlement_center, square_size, &occupied, lane);
    let plaza = MapPlaza {
        // Tagged by whoever built it: `settlement_roads` lays out one
        // settlement and is not told whose. `build::scene` knows the name at
        // both call sites and fills it in.
        town: String::new(),
        rect: MapRect {
            x: square.x,
            y: square.y,
            width: square.width,
            depth: square.height,
        },
        color: PLAZA,
    };

    // The square is place zero, which is what roots the spanning tree at it:
    // traffic on an edge is then simply everything hanging off its far end.
    let mut places = vec![Place {
        center: square.center(),
        files: 0,
        // The square is paving, so a road stops at its kerb rather than
        // driving into the middle of it.
        gates: edge_gates(square),
        rect: None,
    }];
    for ward in &wards {
        places.push(Place {
            center: ward.rect.center(),
            files: ward.arrivals.max(1),
            gates: ward_gates(ward.rect, square.center(), corridors),
            rect: Some(ward.rect),
        });
    }
    for building in &loose {
        places.push(Place {
            center: building.lot.center(),
            files: 1 + building.references,
            // A holding standing loose at the root has no streets of its own,
            // so avenues meet it at its front door: one fixed point on the lot
            // edge, which keeps the road off the building and gives every
            // avenue serving it the same place to join.
            gates: vec![front_door(building.lot, square.center())],
            rect: Some(building.lot),
        });
    }
    for gateway in gateways {
        places.push(Place {
            center: gateway.point,
            files: gateway.traffic,
            // A gateway is a single point on open ground, so it is its own
            // gate and blocks nothing.
            gates: vec![gateway.point],
            rect: None,
        });
    }

    let edges = road_tree(&places);
    let traffic = edge_traffic(&places, &edges);
    let busiest = traffic.iter().copied().max().unwrap_or(0);
    let crossing = ward_crossings(&places, &edges, &traffic);

    let obstacles: Vec<Rect> = places.iter().filter_map(|place| place.rect).collect();
    // A gateway sits out on the town boundary, well clear of the wards, so the
    // grid has to be grown to reach it or there is no route out to meet it.
    let mut reach = obstacles.clone();
    reach.extend(gateways.iter().map(|gateway| Rect {
        x: gateway.point.0,
        y: gateway.point.1,
        width: 0.0,
        height: 0.0,
    }));
    let ground = Ground::new(settlement_extent(&reach, square.center()), &obstacles, lane);
    let houses: Vec<Rect> = buildings
        .iter()
        .map(|building| building.footprint())
        .collect();
    let mut roads = Vec::with_capacity(edges.len() + corridors.len());

    for (index, &(parent, child)) in edges.iter().enumerate() {
        let carried = traffic[index];
        // Only ever a gate when the place has any. Offering the centre as a
        // competing candidate was enough to break the whole scheme: a ward's
        // middle is usually closer to its neighbour than either of its gates,
        // so the centre kept winning and the avenue drove into the ward.
        let target = nearest_gate(&places[parent], places[child].center);
        let from = nearest_gate(&places[child], target);
        let points = route(&ground, from, target);
        let room = (route_clearance(&points, &houses) - KERB) * 2.0;
        let (color, edge) = paving(carried, busiest);
        roads.push(MapRoad {
            kind: RoadKind::Ward,
            points,
            // Never wider than the lane left for it, nor than the route it
            // was actually given has room for. The lane is sized for the
            // busiest avenue the settlement can call for, so the first only
            // binds on a squeeze and the second only where the planner's grid
            // put the route closer to a holding than the middle of the lane.
            width: (corridor_width(carried) * AVENUE_WIDENING)
                .min(lane - WARD_GAP * 0.3)
                .min(room)
                .max(MIN_DRAWN_WIDTH),
            traffic: carried.min(u32::MAX as usize) as u32,
            color,
            edge,
        });
    }

    // A ward that traffic crosses is a waypoint, not just a destination, and
    // the street it crosses by has to be drawn for everything using it.
    let mut street_traffic: Vec<usize> =
        corridors.iter().map(|corridor| corridor.traffic).collect();
    for (index, ward) in wards.iter().enumerate() {
        // Place zero is the square, so ward `index` is place `index + 1`.
        let through = crossing[index + 1];
        if through == 0 {
            continue;
        }
        if let Some(spine) = ward_spine(ward.rect, corridors) {
            street_traffic[spine] += through;
        }
    }

    for (index, corridor) in corridors.iter().enumerate() {
        let carried = street_traffic[index];
        let points = vec![
            [corridor.start.0, corridor.start.1],
            [corridor.end.0, corridor.end.1],
        ];
        // Only a street carrying traffic across its ward is redrawn, and only
        // ever wider: the layout already fitted every other street to the
        // ground its cell could spare. Even then the holdings either side set
        // the limit, because the gap between them was left for the narrower
        // street the layout planned.
        let width = if carried > corridor.traffic {
            let room = (route_clearance(&points, &houses) - KERB) * 2.0;
            corridor_width(carried).min(room).max(corridor.width)
        } else {
            corridor.width
        };
        let (color, edge) = paving(carried, busiest.max(carried));
        roads.push(MapRoad {
            kind: RoadKind::Street,
            points,
            width: width.max(MIN_DRAWN_WIDTH),
            traffic: carried.min(u32::MAX as usize) as u32,
            color,
            edge,
        });
    }

    for building in buildings {
        if let Some(drive) = driveway(building, &roads) {
            roads.push(drive);
        }
    }

    (roads, Some(plaza))
}

/// Whether a drive from `door` out to `kerb` leaves by a wall the camera can
/// see.
///
/// The map is drawn from a camera looking down the `(-1, -1, -1)` diagonal, so
/// growing x and growing y both project *downwards* on screen — by exactly the
/// same amount, x towards the bottom-right and y towards the bottom-left. Those
/// two walls are the front of the house. The other two face away and are hidden
/// by the roof standing on them, so a drive leaving either one is drawn under
/// the building and reads as no drive at all.
fn faces_the_viewer(door: (f32, f32), kerb: (f32, f32)) -> bool {
    if (kerb.1 - door.1).abs() <= f32::EPSILON {
        kerb.0 > door.0
    } else {
        kerb.1 > door.1
    }
}

/// The short path from a holding's door out to the street it fronts onto.
///
/// Every lot already borders a street, so a driveway carries no traffic that
/// the street does not. What it carries is the *answer to a question*: which
/// of the four sides of this house is the front, and which of the roads around
/// it is the one that serves it. Without it a holding is a box sitting in a
/// block, and the road network around it is anonymous.
///
/// The drive runs from the middle of one wall out to the centre line of the
/// serving street, along a single axis. It may leave its own lot only by the
/// lot's setback plus half that street — never far enough to reach the lot
/// next door, because every split puts a corridor and two setbacks between
/// them. That bound is what keeps a drive off its neighbours.
///
/// Of the walls that can reach a street square-on it takes one the camera can
/// see, and only then the nearest — see [`faces_the_viewer`]. A holding with
/// no such wall gets no drive at all, which is why coverage is high rather
/// than total.
fn driveway(building: &Building, roads: &[MapRoad]) -> Option<MapRoad> {
    let lot = building.lot;
    let footprint = building.footprint();
    let door = footprint.center();

    // Only a square-on approach counts. Aiming at the nearest point on the
    // nearest kerb sounds equivalent and is not: when that point is the *end*
    // of a segment the nearest kerb is diagonal, and a drive along one axis
    // stops beside the street instead of on it.
    let mut best: Option<(f32, &MapRoad, (f32, f32))> = None;
    for road in roads {
        if road.kind == RoadKind::Drive {
            continue;
        }
        for segment in road.points.windows(2) {
            let (start, end) = (
                (segment[0][0], segment[0][1]),
                (segment[1][0], segment[1][1]),
            );
            let meeting = if (start.1 - end.1).abs() <= f32::EPSILON {
                let (low, high) = (start.0.min(end.0), start.0.max(end.0));
                (door.0 >= low && door.0 <= high).then_some((door.0, start.1))
            } else if (start.0 - end.0).abs() <= f32::EPSILON {
                let (low, high) = (start.1.min(end.1), start.1.max(end.1));
                (door.1 >= low && door.1 <= high).then_some((start.0, door.1))
            } else {
                None
            };
            let Some(meeting) = meeting else { continue };
            // A back wall is not a worse frontage than a front one, it is no
            // frontage at all: the drive is drawn under the house and cannot
            // be seen. A holding with no street it can front onto in view is
            // better left without a drive than given one nobody will read.
            if !faces_the_viewer(door, meeting) {
                continue;
            }
            // The drive may leave its own lot only far enough to reach the
            // centre line of the street just outside it. A holding whose own
            // kerb cannot be met square-on would otherwise reach across the
            // lot next door for one that can — straight through the neighbour.
            if !within(lot.inset(-(road.width * 0.5 + LOT_SETBACK)), meeting) {
                continue;
            }
            let span = distance_squared(door, meeting);
            if best.is_none_or(|(found, _, _)| span < found) {
                best = Some((span, road, meeting));
            }
        }
    }
    let (_, street, kerb) = best?;

    // Out of the wall facing the street, and on to its centre line. Stopping
    // at the lot edge instead leaves the drive a stride short of the kerb,
    // which looks joined but is not.
    // The drive leaves whichever wall faces the street, so that wall is what
    // bounds how wide it can be.
    let (start, end, frontage) = if (kerb.1 - door.1).abs() <= f32::EPSILON {
        let from = if kerb.0 >= door.0 {
            footprint.x + footprint.width
        } else {
            footprint.x
        };
        ((from, door.1), kerb, footprint.height)
    } else {
        let from = if kerb.1 >= door.1 {
            footprint.y + footprint.height
        } else {
            footprint.y
        };
        ((door.0, from), kerb, footprint.width)
    };
    // A drive shorter than its own width is a smudge, not a path. The base
    // width is what that is measured against: a much-referenced file must
    // never lose its drive for the very reason it earned a wide one.
    let reach = distance_squared(start, end);
    if reach < (DRIVE_WIDTH * DRIVE_WIDTH) {
        return None;
    }
    // Never broader than it is long, so a wide drive on a short run still
    // reads as a path rather than as a blot against the wall. Nor broader
    // than the street it joins: traffic now carries references all the way
    // back to the square, so the street outside a much-used holding is wide
    // already — and where it still is not, a drive spilling out on to a lane
    // a fifth its width reads as a mistake rather than as importance.
    let width = drive_width(building.references, frontage)
        .min(reach.sqrt())
        .min(street.width);

    Some(MapRoad {
        kind: RoadKind::Drive,
        points: vec![[start.0, start.1], [end.0, end.1]],
        width,
        // Every file that imports this one arrives by its door, and so does
        // the holding itself. Width and traffic stay the one number, so
        // "wider means busier" holds for a drive as it does for a street.
        traffic: (building.references + 1).min(u32::MAX as usize) as u32,
        // Borrowed from the street it meets, so a door on a trunk road reads
        // as belonging to that road rather than as an unrelated mark.
        color: street.color,
        edge: street.edge,
    })
}
///
/// A ward's first split spans the whole ward, so both ends of it sit exactly
/// on the ward's edge — which makes them the honest places for an avenue to
/// arrive. Anything narrower belongs to a cell deeper inside and would strand
/// the avenue in the middle of the holdings.
///
/// A ward holding a single file is never split at all, so it has no street for
/// an avenue to meet and falls back to a front door like a loose holding.
fn ward_gates(ward: Rect, square: (f32, f32), corridors: &[Corridor]) -> Vec<(f32, f32)> {
    match ward_spine(ward, corridors) {
        Some(index) => vec![corridors[index].start, corridors[index].end],
        None => vec![front_door(ward, square)],
    }
}

/// The street a ward's avenues arrive at, if it has one.
///
/// The widest street lying wholly inside the ward is its first split, which is
/// the only one spanning the ward end to end. Both of its ends are therefore
/// on the ward's edge, which is what makes them gates — and what makes this
/// street the one an avenue continues along when the traffic it brings is
/// bound for somewhere on the far side.
fn ward_spine(ward: Rect, corridors: &[Corridor]) -> Option<usize> {
    let mut best: Option<usize> = None;
    for (index, corridor) in corridors.iter().enumerate() {
        if !within(ward, corridor.start) || !within(ward, corridor.end) {
            continue;
        }
        if best.is_none_or(|found| corridor.traffic > corridors[found].traffic) {
            best = Some(index);
        }
    }
    best
}

/// The single point every avenue serving a street-less place must meet at.
///
/// A place with no streets of its own — a loose holding, or a ward small
/// enough never to have been split — has nothing inside it for a road to join,
/// so the meeting point has to be on its edge. It has to be *one* point: given
/// a choice of four edge middles, a road arriving from the north and a road
/// leaving to the south each pick the edge nearest them, and the two never
/// touch. That is how a chain of small holdings ends up as a string of roads
/// that connect to nothing.
///
/// The edge facing the square wins, because that is the way the traffic goes.
fn front_door(lot: Rect, square: (f32, f32)) -> (f32, f32) {
    edge_gates(lot)
        .into_iter()
        .min_by(|a, b| {
            distance_squared(*a, square)
                .partial_cmp(&distance_squared(*b, square))
                .unwrap_or(Ordering::Equal)
        })
        .unwrap_or(lot.center())
}

fn within(rect: Rect, point: (f32, f32)) -> bool {
    const EDGE: f32 = 1.0;
    point.0 >= rect.x - EDGE
        && point.0 <= rect.x + rect.width + EDGE
        && point.1 >= rect.y - EDGE
        && point.1 <= rect.y + rect.height + EDGE
}

/// The gate on `place` that a road from `target` should arrive at.
fn nearest_gate(place: &Place, target: (f32, f32)) -> (f32, f32) {
    match nearest_gate_index(place, target) {
        Some(index) => place.gates[index],
        None => place.center,
    }
}

/// Which of `place`'s gates a road from `target` should arrive at.
///
/// Roads need the point, but a ward needs to know whether two of its roads
/// picked the *same* gate: two roads meeting at one gate pass each other
/// outside the ward, while roads at opposite gates send their traffic straight
/// through the middle of it.
fn nearest_gate_index(place: &Place, target: (f32, f32)) -> Option<usize> {
    place
        .gates
        .iter()
        .enumerate()
        .min_by(|left, right| {
            distance_squared(*left.1, target)
                .partial_cmp(&distance_squared(*right.1, target))
                .unwrap_or(Ordering::Equal)
        })
        .map(|(index, _)| index)
}

/// How much a road is charged for how far its junction already is from the
/// square, as a fraction of the road's own length.
///
/// At zero this is Prim's algorithm, which builds the shortest network that
/// reaches everywhere — and which, on places spread at all evenly, is a single
/// winding lane: the nearest unjoined place is almost always next to the one
/// just added, so the network keeps growing from its own far end and arrives
/// everywhere by way of everywhere else. At one it is Dijkstra's, and every
/// place is joined straight back towards the square, which is a wheel of long
/// spokes that ignores how much paving it costs.
///
/// In between, a junction that is already a long way out has to be that much
/// closer to be worth joining to, so a place near the square wins the ones
/// behind it and the network forks. At a half, the deepest chain across the
/// repositories this was measured on fell from twenty-six roads to ten while
/// the paving grew by an eighth — pushing it higher went on straightening the
/// tree, but bought less and less of it for steadily more road.
const BRANCHING: f32 = 0.5;

/// Joins every place to the network, branching out from the central square.
///
/// Prim's, grown from place zero — the central square — but charging each
/// candidate road [`BRANCHING`] of the distance its junction already stands
/// from the square, so the network spreads outwards in branches rather than
/// wandering. Growing it from the square is what gives the edges their
/// direction for free: a place is always added after the one that reached it,
/// so the list is already a tree rooted at the square, which is exactly what
/// [`edge_traffic`] and [`ward_crossings`] need.
fn road_tree(places: &[Place]) -> Vec<(usize, usize)> {
    let count = places.len();
    if count < 2 {
        return Vec::new();
    }
    let mut joined = vec![false; count];
    let mut best = vec![f32::MAX; count];
    let mut parent = vec![0usize; count];
    // How far each joined place stands from the square along the roads, which
    // is what the next road out of it is charged for.
    let mut reach = vec![0.0f32; count];
    joined[0] = true;
    for index in 1..count {
        best[index] = distance(places[0].center, places[index].center);
    }

    let mut edges = Vec::with_capacity(count - 1);
    for _ in 1..count {
        let mut pick = None;
        for index in 0..count {
            if joined[index] {
                continue;
            }
            if pick.is_none_or(|current: usize| best[index] < best[current]) {
                pick = Some(index);
            }
        }
        let Some(pick) = pick else { break };
        joined[pick] = true;
        reach[pick] =
            reach[parent[pick]] + distance(places[parent[pick]].center, places[pick].center);
        edges.push((parent[pick], pick));
        for index in 0..count {
            if joined[index] {
                continue;
            }
            let cost =
                BRANCHING * reach[pick] + distance(places[pick].center, places[index].center);
            if cost < best[index] {
                best[index] = cost;
                parent[index] = pick;
            }
        }
    }
    edges
}

/// How many files travel each edge to reach the central square.
///
/// Everything beyond an edge has to cross it, so an edge carries its whole
/// subtree. Walking the edges backwards is enough to total them up: a place is
/// always listed after whichever place reached it, so by the time an edge is
/// reached going backwards everything below it has already been counted.
fn edge_traffic(places: &[Place], edges: &[(usize, usize)]) -> Vec<usize> {
    let mut subtree: Vec<usize> = places.iter().map(|place| place.files).collect();
    for &(parent, child) in edges.iter().rev() {
        subtree[parent] += subtree[child];
    }
    edges.iter().map(|&(_, child)| subtree[child]).collect()
}

/// How much traffic drives straight through each place instead of stopping.
///
/// A place with a street of its own offers a gate at either end of it, and an
/// avenue arrives at whichever is nearer. When the avenue a place leaves by
/// and the avenue something beyond it arrives by pick *different* gates, the
/// two are joined only by the street between them: everything on the far side
/// drives in one gate and out the other. That street is then an avenue in all
/// but name, and sizing it for the place's own files alone is what left the
/// sole road to a whole branch of the city thinner than the roads either side
/// of it. Roads that pick the same gate meet outside and cross nothing.
fn ward_crossings(places: &[Place], edges: &[(usize, usize)], traffic: &[usize]) -> Vec<usize> {
    // Which gate each avenue arrives at on its parent, and which gate each
    // place in turn leaves by on its own way to the square.
    let mut arrival = Vec::with_capacity(edges.len());
    let mut uphill: Vec<Option<usize>> = vec![None; places.len()];
    for &(parent, child) in edges {
        let gate = nearest_gate_index(&places[parent], places[child].center);
        let target = gate.map_or(places[parent].center, |index| places[parent].gates[index]);
        uphill[child] = nearest_gate_index(&places[child], target);
        arrival.push(gate);
    }

    let mut crossing = vec![0usize; places.len()];
    for (index, &(parent, _)) in edges.iter().enumerate() {
        // The square is paving, not a place with an inside, so roads meeting
        // it cross open ground rather than anything that has to be widened.
        if parent == 0 {
            continue;
        }
        if uphill[parent].is_some() && arrival[index] != uphill[parent] {
            crossing[parent] += traffic[index];
        }
    }
    crossing
}

/// How far a road along `run` may be widened before it touches `house`.
///
/// **Not the distance between the two boxes, and the difference is the whole
/// point.** A road is widened along both axes at once — half its width is
/// added to every side of its run — so what a house sitting diagonally off the
/// end of a segment permits is the *larger* of the two gaps, not the diagonal
/// between them. Straight-line distance, which is what this returned, is up to
/// √2 too generous, and the road spends the difference growing sideways into
/// the house.
///
/// It survived for as long as it did because it only bites when a holding
/// stands within half a road's width of a route in *both* axes at once. Lots
/// used to be sized by `√bytes`, which left even the smallest of them fat
/// enough that nothing came that close; sizing them by content puts genuinely
/// small houses beside the avenues, and the old sum stopped being safe.
fn widening_room(run: Rect, house: Rect) -> f32 {
    let horizontal = (house.x - (run.x + run.width))
        .max(run.x - (house.x + house.width))
        .max(0.0);
    let vertical = (house.y - (run.y + run.height))
        .max(run.y - (house.y + house.height))
        .max(0.0);
    // Zero when they overlap in both axes -- a house standing *on* the route.
    // Taking the larger of the two would otherwise read an overlap as room.
    if horizontal <= 0.0 && vertical <= 0.0 {
        return 0.0;
    }
    horizontal.max(vertical)
}

/// How much room a route has before it reaches the nearest holding.
///
/// The planner works on a grid whose cells are half a lane across, so a route
/// it reports as clear may still run right along a ward's edge — clear for a
/// line, but not for a road forty units wide. Rather than refine the grid
/// until the width fits, which costs a finer search over the whole map, the
/// width is fitted to the route that was found.
fn route_clearance(points: &[[f32; 2]], houses: &[Rect]) -> f32 {
    let mut clearance = f32::MAX;
    for segment in points.windows(2) {
        let (start, end) = (segment[0], segment[1]);
        let run = Rect {
            x: start[0].min(end[0]),
            y: start[1].min(end[1]),
            width: (start[0] - end[0]).abs(),
            height: (start[1] - end[1]).abs(),
        };
        for house in houses {
            clearance = clearance.min(widening_room(run, *house));
        }
    }
    clearance
}

/// The ground avenues must find their way across.
///
/// Trying both elbows of a right-angled route and keeping the better one is
/// not obstacle avoidance — when both cross a ward, one of them still gets
/// drawn, which is how avenues ended up running through the holdings they were
/// meant to serve. A coarse grid with the wards stamped out of it turns the
/// problem into a search that either finds a clear route or honestly reports
/// that there is none.
struct Ground {
    origin: (f32, f32),
    cell: f32,
    columns: usize,
    rows: usize,
    blocked: Vec<bool>,
}

/// How much a route pays to change direction, in cells.
///
/// Without it the search returns staircases: a diagonal drawn as a hundred
/// tiny steps, all of them equally short. Making a turn cost several straight
/// moves buys long runs and few corners, which is what a street looks like.
const TURN_COST: u32 = 4;

impl Ground {
    fn new(extent: Rect, obstacles: &[Rect], lane: f32) -> Self {
        // A cell may never be wider than the lane between two wards, or the
        // lane falls between the sampling points and the planner cannot see
        // the only open ground there is. On a large map `span / 150` is wider
        // than the gap, which is what used to force avenues to squeeze through
        // the edge of a ward — and that is where the holdings stand.
        let cell = (extent.span() / 150.0).clamp(4.0, lane * 0.5);
        let columns = ((extent.width / cell).ceil() as usize + 2).max(1);
        let rows = ((extent.height / cell).ceil() as usize + 2).max(1);
        let origin = (extent.x - cell, extent.y - cell);
        let mut blocked = vec![false; columns * rows];
        for rect in obstacles {
            // Blocked on the true rect: with the cell now smaller than the
            // lane, nothing has to be given away to keep a route open, so a
            // road is held off built ground to within one cell.
            let first_column =
                (((rect.x - origin.0) / cell).floor().max(0.0) as usize).min(columns);
            let first_row = (((rect.y - origin.1) / cell).floor().max(0.0) as usize).min(rows);
            let last_column =
                ((((rect.x + rect.width) - origin.0) / cell).ceil().max(0.0) as usize + 1)
                    .min(columns);
            let last_row = ((((rect.y + rect.height) - origin.1) / cell).ceil().max(0.0) as usize
                + 1)
            .min(rows);
            for row in first_row..last_row {
                for column in first_column..last_column {
                    let point = (
                        origin.0 + (column as f32 + 0.5) * cell,
                        origin.1 + (row as f32 + 0.5) * cell,
                    );
                    if point.0 >= rect.x
                        && point.0 <= rect.x + rect.width
                        && point.1 >= rect.y
                        && point.1 <= rect.y + rect.height
                    {
                        blocked[row * columns + column] = true;
                    }
                }
            }
        }
        Self {
            origin,
            cell,
            columns,
            rows,
            blocked,
        }
    }

    fn cell_of(&self, point: (f32, f32)) -> (usize, usize) {
        let column = ((point.0 - self.origin.0) / self.cell).floor().max(0.0) as usize;
        let row = ((point.1 - self.origin.1) / self.cell).floor().max(0.0) as usize;
        (column.min(self.columns - 1), row.min(self.rows - 1))
    }

    fn center_of(&self, column: usize, row: usize) -> [f32; 2] {
        [
            self.origin.0 + (column as f32 + 0.5) * self.cell,
            self.origin.1 + (row as f32 + 0.5) * self.cell,
        ]
    }

    /// The cheapest clear right-angled route between two points, if one exists.
    ///
    /// A* over `(cell, heading)` rather than over cells alone, because the cost
    /// of a step depends on the direction the route arrived in — that is what
    /// [`TURN_COST`] is charged against.
    ///
    /// # Why it is not a plain Dijkstra
    ///
    /// It was, and on a realm-sized grid that made this the slowest thing in
    /// the whole build. Dijkstra expands outward in every direction until it
    /// happens to meet the goal, and a realm's grid is not small: measured over
    /// a real dev folder of seven cities the highway grid is 1,815 × 1,525 —
    /// 2.8 million cells, 11 million states — and three of the six highways
    /// each explored a quarter to a half of every state in it. One route cost
    /// 373 ms and popped 4.7 million states.
    ///
    /// The goal is a known point on a uniform grid, so the search is entitled
    /// to a heuristic and was simply declining one. With it, those six highways
    /// cost 833 ms → 163 ms, and the worst single route 373 ms → 128 ms.
    ///
    /// **It returns exactly the routes the exhaustive search returned**, which
    /// is what makes this a speed change rather than a change to the map: see
    /// [`Ground::estimate`] for why the heuristic is admissible, and
    /// `the_heuristic_never_changes_a_route`, which pins it against the old
    /// search over three thousand random grids.
    fn route(&self, from: (f32, f32), to: (f32, f32)) -> Option<Vec<[f32; 2]>> {
        const HEADINGS: [(i32, i32); 4] = [(1, 0), (-1, 0), (0, 1), (0, -1)];

        let (start_column, start_row) = self.cell_of(from);
        let (goal_column, goal_row) = self.cell_of(to);
        let start = start_row * self.columns + start_column;
        let goal = goal_row * self.columns + goal_column;

        // Sparse rather than `vec![_; cells * 4]`, and that is a good share of
        // the saving. A* reaches a few hundred thousand states out of eleven
        // million, so a dense array is tens of megabytes cleared per road to
        // hold a frontier that never fills it — and `highways` plans one road
        // at a time against a single `Ground`, so that cost was paid again for
        // every one of them.
        let mut best: HashMap<usize, u32> = HashMap::new();
        let mut came_from: HashMap<usize, usize> = HashMap::new();

        let mut queue = BinaryHeap::new();
        for heading in 0..4 {
            let state = start * 4 + heading;
            best.insert(state, 0);
            queue.push(Reverse((
                self.estimate(state, goal_column, goal_row),
                0u32,
                state,
            )));
        }

        let mut arrived = None;
        // Ordered by estimated total rather than by distance travelled, which
        // is the whole difference from the Dijkstra this replaced. The cost so
        // far is carried alongside because that, and not the estimate, is what
        // the neighbours are relaxed against.
        while let Some(Reverse((_, cost, state))) = queue.pop() {
            if cost > best.get(&state).copied().unwrap_or(u32::MAX) {
                continue;
            }
            let cell = state / 4;
            let heading = state % 4;
            if cell == goal {
                arrived = Some(state);
                break;
            }
            let column = cell % self.columns;
            let row = cell / self.columns;
            for (next_heading, (dx, dy)) in HEADINGS.iter().enumerate() {
                let next_column = column as i32 + dx;
                let next_row = row as i32 + dy;
                if next_column < 0
                    || next_row < 0
                    || next_column >= self.columns as i32
                    || next_row >= self.rows as i32
                {
                    continue;
                }
                let next = next_row as usize * self.columns + next_column as usize;
                // The two ends are always reachable even when they sit on
                // blocked ground: a gate lies on its ward's own boundary.
                if self.blocked[next] && next != goal && next != start {
                    continue;
                }
                let turn = if next_heading == heading {
                    0
                } else {
                    TURN_COST
                };
                let next_cost = cost + 1 + turn;
                let next_state = next * 4 + next_heading;
                if next_cost < best.get(&next_state).copied().unwrap_or(u32::MAX) {
                    best.insert(next_state, next_cost);
                    came_from.insert(next_state, state);
                    queue.push(Reverse((
                        next_cost + self.estimate(next_state, goal_column, goal_row),
                        next_cost,
                        next_state,
                    )));
                }
            }
        }

        let mut state = arrived?;
        let mut points = Vec::new();
        loop {
            let cell = state / 4;
            // The two end cells are allowed to be blocked ground -- a gate
            // lies on its own ward's boundary -- but the *centre* of a blocked
            // cell is then inside that ground, and steering the route through
            // it dog-legs the road into the very holding it is arriving at.
            // `from` and `to` are inserted below and say exactly where the road
            // meets each end, so the cell centre adds nothing but the detour.
            let end = cell == start || cell == goal;
            if !(end && self.blocked[cell]) {
                points.push(self.center_of(cell % self.columns, cell / self.columns));
            }
            if cell == start {
                break;
            }
            state = *came_from.get(&state)?;
        }
        points.reverse();

        points.insert(0, [from.0, from.1]);
        points.push([to.0, to.1]);
        Some(simplify(&squared_off(&points)))
    }

    /// The least a route from `state` to the goal cell could possibly cost.
    ///
    /// [`Ground::route`] returns the same road as an exhaustive search only
    /// while this never *over*-states what remains, so both terms are
    /// deliberately the cheapest thing that could still be true:
    ///
    /// - **The distance.** Every step moves one cell along an axis and costs at
    ///   least one, so a right-angled route can never be shorter than the
    ///   Manhattan distance. Blocked ground only ever makes it longer.
    /// - **The turns.** A goal off both axes cannot be reached without at least
    ///   one turn, and a heading that points away from it — or runs along an
    ///   axis already satisfied — owes at least one more. Each is charged at
    ///   [`TURN_COST`], which is what a turn genuinely costs.
    ///
    /// Zero at the goal itself, as it must be: a state that has arrived has
    /// nothing left to pay.
    ///
    /// Getting this wrong would not fail loudly. An over-estimate simply
    /// returns a slightly worse road, which nobody would catch by looking at
    /// the map — which is why `the_heuristic_never_changes_a_route` compares
    /// against the old search rather than checking a road looks reasonable.
    fn estimate(&self, state: usize, goal_column: usize, goal_row: usize) -> u32 {
        let cell = state / 4;
        let heading = state % 4;
        let column = (cell % self.columns) as i64;
        let row = (cell / self.columns) as i64;
        let dx = (column - goal_column as i64).abs();
        let dy = (row - goal_row as i64).abs();
        if dx == 0 && dy == 0 {
            return 0;
        }

        // One turn is owed whenever the goal is off both axes: no single
        // straight run can reach it.
        let mut turns = u32::from(dx > 0 && dy > 0);
        // And one whenever this heading is not already making progress --
        // either it points away from the goal, or it runs along an axis that is
        // already satisfied. `HEADINGS` in `route` is `[+x, -x, +y, -y]`.
        let away = match heading {
            0 => column > goal_column as i64,
            1 => column < goal_column as i64,
            2 => row > goal_row as i64,
            _ => row < goal_row as i64,
        };
        let on_axis = match heading {
            0 | 1 => dx > 0,
            _ => dy > 0,
        };
        if away || !on_axis {
            turns = turns.max(1);
        }

        (dx + dy) as u32 + turns * TURN_COST
    }

    /// The original exhaustive search, kept only so a test can prove the
    /// heuristic above changed no answer.
    ///
    /// This is [`Ground::route`] as it stood before A*: the same relaxation,
    /// ordered by distance travelled alone, over dense arrays. It is compiled
    /// only under `cfg(test)`, so the server carries none of it.
    ///
    /// Kept rather than deleted because it is the only thing that can settle
    /// the question the change turns on. Every other road test asks whether the
    /// network is connected and misses no holding; none of them can tell an
    /// optimal route from a route four turns longer, and that is exactly what a
    /// bad heuristic produces.
    #[cfg(test)]
    fn route_exhaustive(&self, from: (f32, f32), to: (f32, f32)) -> Option<Vec<[f32; 2]>> {
        const HEADINGS: [(i32, i32); 4] = [(1, 0), (-1, 0), (0, 1), (0, -1)];

        let (start_column, start_row) = self.cell_of(from);
        let (goal_column, goal_row) = self.cell_of(to);
        let start = start_row * self.columns + start_column;
        let goal = goal_row * self.columns + goal_column;

        let cells = self.columns * self.rows;
        let mut best = vec![u32::MAX; cells * 4];
        let mut came_from = vec![usize::MAX; cells * 4];
        let mut queue = BinaryHeap::new();
        for heading in 0..4 {
            best[start * 4 + heading] = 0;
            queue.push(Reverse((0u32, start * 4 + heading)));
        }

        let mut arrived = None;
        while let Some(Reverse((cost, state))) = queue.pop() {
            if cost > best[state] {
                continue;
            }
            let cell = state / 4;
            let heading = state % 4;
            if cell == goal {
                arrived = Some(state);
                break;
            }
            let column = cell % self.columns;
            let row = cell / self.columns;
            for (next_heading, (dx, dy)) in HEADINGS.iter().enumerate() {
                let next_column = column as i32 + dx;
                let next_row = row as i32 + dy;
                if next_column < 0
                    || next_row < 0
                    || next_column >= self.columns as i32
                    || next_row >= self.rows as i32
                {
                    continue;
                }
                let next = next_row as usize * self.columns + next_column as usize;
                if self.blocked[next] && next != goal && next != start {
                    continue;
                }
                let turn = if next_heading == heading {
                    0
                } else {
                    TURN_COST
                };
                let next_cost = cost + 1 + turn;
                let next_state = next * 4 + next_heading;
                if next_cost < best[next_state] {
                    best[next_state] = next_cost;
                    came_from[next_state] = state;
                    queue.push(Reverse((next_cost, next_state)));
                }
            }
        }

        let mut state = arrived?;
        let mut points = Vec::new();
        loop {
            let cell = state / 4;
            // Reconstructed exactly as `Ground::route` does, so what this
            // settles stays a question about the *search* rather than about
            // the walk back from it.
            let end = cell == start || cell == goal;
            if !(end && self.blocked[cell]) {
                points.push(self.center_of(cell % self.columns, cell / self.columns));
            }
            if cell == start {
                break;
            }
            state = came_from[state];
            if state == usize::MAX {
                return None;
            }
        }
        points.reverse();

        points.insert(0, [from.0, from.1]);
        points.push([to.0, to.1]);
        Some(simplify(&squared_off(&points)))
    }
}

/// Replaces any diagonal step with a right-angled pair.
///
/// Only the two ends can be diagonal — the search itself only ever moves along
/// an axis — but those are the ends that meet a gate, which is exactly where a
/// stray diagonal is most visible.
fn squared_off(points: &[[f32; 2]]) -> Vec<[f32; 2]> {
    let mut output: Vec<[f32; 2]> = Vec::with_capacity(points.len() + 2);
    for &point in points {
        if let Some(&last) = output.last() {
            if last == point {
                continue;
            }
            if last[0] != point[0] && last[1] != point[1] {
                output.push([point[0], last[1]]);
            }
        }
        output.push(point);
    }
    output
}

/// Drops points that only continue a straight run.
fn simplify(points: &[[f32; 2]]) -> Vec<[f32; 2]> {
    let mut output: Vec<[f32; 2]> = Vec::with_capacity(points.len());
    for (index, &point) in points.iter().enumerate() {
        if index == 0 || index + 1 == points.len() {
            output.push(point);
            continue;
        }
        let previous = *output.last().expect("a first point");
        let next = points[index + 1];
        let straight = (previous[0] == point[0] && point[0] == next[0])
            || (previous[1] == point[1] && point[1] == next[1]);
        if !straight {
            output.push(point);
        }
    }
    output
}

/// The middle of each edge of a lot, as places a road may arrive at.
fn edge_gates(lot: Rect) -> Vec<(f32, f32)> {
    let (cx, cy) = lot.center();
    vec![
        (cx, lot.y),
        (cx, lot.y + lot.height),
        (lot.x, cy),
        (lot.x + lot.width, cy),
    ]
}

/// Routes a road between two points, going around whatever stands in the way.
///
/// Falls back to a plain elbow only when the ground offers no clear route at
/// all, which happens when a place is walled in by its neighbours.
fn route(ground: &Ground, from: (f32, f32), to: (f32, f32)) -> Vec<[f32; 2]> {
    ground
        .route(from, to)
        .unwrap_or_else(|| simplify(&squared_off(&[[from.0, from.1], [to.0, to.1]])))
}

/// How much of an axis-aligned segment lies inside a rectangle.
///
/// Only ever called with the right-angled segments the router produces, so one
/// axis is always constant and the answer is a length rather than a clipped
/// polyline.
#[cfg(test)]
fn overlap(rect: Rect, start: [f32; 2], end: [f32; 2]) -> f32 {
    let (min_x, max_x) = (start[0].min(end[0]), start[0].max(end[0]));
    let (min_y, max_y) = (start[1].min(end[1]), start[1].max(end[1]));
    let inside_x = (max_x.min(rect.x + rect.width) - min_x.max(rect.x)).max(0.0);
    let inside_y = (max_y.min(rect.y + rect.height) - min_y.max(rect.y)).max(0.0);
    if start[1] == end[1] {
        if min_y >= rect.y && max_y <= rect.y + rect.height {
            inside_x
        } else {
            0.0
        }
    } else if min_x >= rect.x && max_x <= rect.x + rect.width {
        inside_y
    } else {
        0.0
    }
}

/// Paving and verge for a road, paling as it grows busier.
///
/// Width already says how much a road carries, but width alone is hard to
/// compare across a settlement at an isometric angle. Colour makes the trunk
/// routes legible from far enough out that the individual holdings have
/// stopped being readable: a lane is barely lighter than the grass it crosses,
/// a trunk road is pale stone, and the whole range is spread between them so
/// the busiest route reads as the busiest at a glance.
///
/// Traffic is a long tail — one road carries everything and most carry a
/// handful — so the share is taken through a square root. On a linear ramp
/// every road but the trunk would sit at the dark end and the ramp would say
/// nothing.
fn paving(traffic: usize, busiest: usize) -> (MapColor, MapColor) {
    let share = if busiest == 0 {
        0.0
    } else {
        (traffic as f32 / busiest as f32).clamp(0.0, 1.0).sqrt()
    };
    (mix(LANE, TRUNK, share), mix(LANE_EDGE, TRUNK_EDGE, share))
}

fn mix(from: MapColor, to: MapColor, ratio: f32) -> MapColor {
    let channel = |left: u8, right: u8| {
        (left as f32 + (right as f32 - left as f32) * ratio).clamp(0.0, 255.0) as u8
    };
    [
        channel(from[0], to[0]),
        channel(from[1], to[1]),
        channel(from[2], to[2]),
        255,
    ]
}

fn distance_squared(left: (f32, f32), right: (f32, f32)) -> f32 {
    (left.0 - right.0).powi(2) + (left.1 - right.1).powi(2)
}

/// Nearest-first comparisons can use [`distance_squared`], but a road's length
/// has to be added to the length of the roads before it, so it has to be real.
fn distance(left: (f32, f32), right: (f32, f32)) -> f32 {
    distance_squared(left, right).sqrt()
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::build::layout::CityLayout;
    use crate::build::model::{Category, Metrics, Node, NodeKind};

    fn file(name: &str, path: &str) -> Node {
        Node {
            name: name.to_owned(),
            relative_path: PathBuf::from(path),
            kind: NodeKind::File {
                category: Category::Source,
            },
            metrics: Metrics {
                bytes: 2_000,
                lines: 200,
                file_count: 1,
                ..Metrics::default()
            },
            children: Vec::new(),
        }
    }

    /// A shape close to what real repositories actually look like: a couple of
    /// substantial folders, a folder holding a single file, and a scatter of
    /// loose files at the root.
    ///
    /// The loose files and the one-file folder are not padding. None of them
    /// gets streets of its own, so they are the places the spanning tree has
    /// to link edge-to-edge, and a chain of them is what exposed the roads
    /// that ran close by each other without ever joining. A fixture with only
    /// big folders never exercises that at all.
    fn repository() -> Node {
        let mut root = Node::directory("project".to_owned(), PathBuf::new());
        let mut source = Node::directory("src".to_owned(), PathBuf::from("src"));
        source.children = (0..24)
            .map(|index| {
                file(
                    &format!("module_{index}.rs"),
                    &format!("src/module_{index}.rs"),
                )
            })
            .collect();
        source.metrics = Metrics {
            bytes: 48_000,
            lines: 4_800,
            file_count: 24,
            ..Metrics::default()
        };

        let mut docs = Node::directory("docs".to_owned(), PathBuf::from("docs"));
        docs.children = (0..5)
            .map(|index| {
                file(
                    &format!("guide_{index}.md"),
                    &format!("docs/guide_{index}.md"),
                )
            })
            .collect();
        docs.metrics = Metrics {
            bytes: 10_000,
            lines: 1_000,
            file_count: 5,
            ..Metrics::default()
        };

        let mut scripts = Node::directory("scripts".to_owned(), PathBuf::from("scripts"));
        scripts.children = vec![file("release.sh", "scripts/release.sh")];
        scripts.metrics = Metrics {
            bytes: 2_000,
            lines: 200,
            file_count: 1,
            ..Metrics::default()
        };

        root.children = vec![
            source,
            docs,
            scripts,
            file("README.md", "README.md"),
            file("LICENSE", "LICENSE"),
            file("Cargo.toml", "Cargo.toml"),
            file(".gitignore", ".gitignore"),
        ];
        root.metrics = Metrics {
            bytes: 68_000,
            lines: 6_800,
            file_count: 34,
            ..Metrics::default()
        };
        root
    }

    fn plan(layout: &CityLayout) -> Vec<MapRoad> {
        plan_settlement(layout).0
    }

    fn plan_settlement(layout: &CityLayout) -> (Vec<MapRoad>, Option<MapPlaza>) {
        settlement_roads(
            &layout.districts,
            &layout.buildings,
            &layout.corridors,
            layout.extent.center(),
            52.0,
            &[],
            layout.ward_gap,
        )
    }

    /// The property the whole design rests on: a corridor always ends on
    /// another corridor or on its ward's boundary, so nothing is stranded.
    #[test]
    fn every_holding_has_a_road_along_its_cell() {
        let layout = CityLayout::build(&repository());
        let roads = plan(&layout);
        assert!(!roads.is_empty());

        for building in &layout.buildings {
            let lot = building.lot;
            let reach = lot.width.max(lot.height) + 40.0;
            let near = roads.iter().any(|road| {
                road.points.windows(2).any(|segment| {
                    distance_to_segment(lot.center(), segment[0], segment[1])
                        <= reach + road.width * 0.5
                })
            });
            assert!(near, "{} stands nowhere near a road", building.path);
        }
    }

    #[test]
    fn a_busier_road_is_drawn_wider() {
        let layout = CityLayout::build(&repository());
        let roads = plan(&layout);
        let busiest = roads
            .iter()
            .max_by(|left, right| left.traffic.cmp(&right.traffic))
            .expect("a road");
        let quietest = roads
            .iter()
            .min_by(|left, right| left.traffic.cmp(&right.traffic))
            .expect("a road");
        assert!(busiest.traffic > quietest.traffic, "traffic never varied");
        assert!(
            busiest.width > quietest.width,
            "width {} carried {} but width {} carried {}",
            busiest.width,
            busiest.traffic,
            quietest.width,
            quietest.traffic
        );
    }

    /// The difference between a network that branches and one that winds.
    ///
    /// Every place here stands the same distance from the square, close enough
    /// to its neighbours that the shortest network by total length is a single
    /// lane threaded through all of them — one road out of the square, and
    /// each place reached by way of the last. Charging a road for how far its
    /// junction already stands from the square is what turns that into roads
    /// leaving the square in their own right.
    #[test]
    fn places_the_same_way_out_each_get_their_own_road() {
        let radius = 100.0f32;
        let step = (35.0f32 / radius).asin() * 2.0;
        let mut places = vec![Place {
            center: (0.0, 0.0),
            files: 0,
            gates: vec![(0.0, 0.0)],
            rect: None,
        }];
        for index in 0..5 {
            let angle = step * index as f32;
            places.push(Place {
                center: (radius * angle.cos(), radius * angle.sin()),
                files: 1,
                gates: vec![(radius * angle.cos(), radius * angle.sin())],
                rect: None,
            });
        }

        let edges = road_tree(&places);
        assert_eq!(edges.len(), places.len() - 1, "every place is joined once");
        let from_square = edges.iter().filter(|(parent, _)| *parent == 0).count();
        assert!(
            from_square > 1,
            "the square was left with {from_square} road(s), so the network is a chain"
        );
    }

    /// A ward whose avenues arrive at opposite gates is a waypoint: everything
    /// beyond it drives in one gate and out the other, so the street between
    /// them carries the far side as well as the ward's own holdings. When both
    /// avenues pick the same gate they meet outside and nothing crosses.
    #[test]
    fn a_ward_carries_the_traffic_that_crosses_it() {
        fn crossing_beyond(far: (f32, f32)) -> usize {
            let places = vec![
                Place {
                    center: (0.0, 0.0),
                    files: 0,
                    gates: vec![(10.0, 0.0)],
                    rect: None,
                },
                Place {
                    center: (100.0, 0.0),
                    files: 8,
                    gates: vec![(50.0, 0.0), (150.0, 0.0)],
                    rect: Some(Rect {
                        x: 50.0,
                        y: -50.0,
                        width: 100.0,
                        height: 100.0,
                    }),
                },
                Place {
                    center: far,
                    files: 17,
                    gates: vec![far],
                    rect: None,
                },
            ];
            let edges = vec![(0, 1), (1, 2)];
            let traffic = edge_traffic(&places, &edges);
            assert_eq!(traffic, vec![25, 17]);
            ward_crossings(&places, &edges, &traffic)[1]
        }

        assert_eq!(
            crossing_beyond((200.0, 0.0)),
            17,
            "a place reached through the far gate has to cross the ward"
        );
        assert_eq!(
            crossing_beyond((60.0, -150.0)),
            0,
            "a place reached through the same gate never enters the ward"
        );
    }

    /// Deliberately checks *every* kind of road, not just the streets. An
    /// earlier version of this test looked only at streets and passed happily
    /// while the avenues drove through seventeen holdings.
    #[test]
    fn no_road_runs_through_a_holding() {
        let layout = CityLayout::build(&repository());
        let roads = plan(&layout);
        for road in &roads {
            for building in &layout.buildings {
                let footprint = building.footprint();
                for segment in road.points.windows(2) {
                    let crosses = overlap(footprint, segment[0], segment[1]);
                    assert!(
                        crosses <= 0.0,
                        "a {:?} road ran through {} for {crosses} units",
                        road.kind,
                        building.path
                    );
                }
            }
        }
    }

    /// A driveway stays on the lot it serves, give or take the street it meets.
    ///
    /// The crossing test above only catches a drive that hits a *footprint*,
    /// and a lot is bigger than the house on it: a drive reaching past its
    /// neighbour's garden for a street it can meet square-on would slip
    /// A drive must never be wider than the way it empties on to.
    ///
    /// This was the bug: a hub earned a great wide drive and then met a lane
    /// no wider than a footpath, because a street's traffic counted the files
    /// standing on it and a hub is still only one file. Both halves are pinned
    /// here — the traffic that should have widened the street, and the cap
    /// that catches whatever it does not reach.
    #[test]
    fn no_drive_is_wider_than_the_street_it_joins() {
        let mut tree = repository();
        for child in &mut tree.children {
            if child.name == "src" {
                child.children[0].metrics.references = 40;
                child.children[3].metrics.references = 12;
            }
        }
        // The folder totals a scan would have aggregated.
        for child in &mut tree.children {
            if child.name == "src" {
                child.metrics.references = 52;
            }
        }

        let layout = CityLayout::build(&tree);
        let roads = plan(&layout);
        let ways: Vec<_> = roads
            .iter()
            .filter(|road| road.kind != RoadKind::Drive)
            .collect();

        for drive in roads.iter().filter(|road| road.kind == RoadKind::Drive) {
            let end = (drive.points[1][0], drive.points[1][1]);
            let met = ways
                .iter()
                .filter(|way| {
                    way.points.windows(2).any(|segment| {
                        let (start, finish) = (
                            (segment[0][0], segment[0][1]),
                            (segment[1][0], segment[1][1]),
                        );
                        let reach = way.width * 0.5 + 0.5;
                        end.0 >= start.0.min(finish.0) - reach
                            && end.0 <= start.0.max(finish.0) + reach
                            && end.1 >= start.1.min(finish.1) - reach
                            && end.1 <= start.1.max(finish.1) + reach
                    })
                })
                .fold(0.0f32, |widest, way| widest.max(way.width));
            assert!(
                met > 0.0,
                "a drive ended at {end:?} without meeting any street at all"
            );
            assert!(
                drive.width <= met + 1e-3,
                "a {} wide drive empties on to a {met} wide street, which reads as \
                 a mistake rather than as importance",
                drive.width
            );
        }
    }

    /// The way to a much-referenced file has to be wide the whole way, not
    /// just at its door. Traffic is journeys, so every street between a hub
    /// and the square carries the references reaching it.
    #[test]
    fn references_widen_the_street_outside_the_holding() {
        let quiet = CityLayout::build(&repository());
        let mut busy_tree = repository();
        for child in &mut busy_tree.children {
            if child.name == "src" {
                for file in &mut child.children {
                    file.metrics.references = 20;
                }
                child.metrics.references = 20 * child.children.len();
            }
        }
        let busy = CityLayout::build(&busy_tree);

        let widest = |layout: &CityLayout| {
            plan(layout)
                .iter()
                .filter(|road| road.kind == RoadKind::Street)
                .fold(0.0f32, |widest, road| widest.max(road.width))
        };
        let (before, after) = (widest(&quiet), widest(&busy));
        assert!(
            after > before * 1.5,
            "streets were {before} wide with no references and {after} with twenty \
             each, so the network does not carry the connections at all"
        );
    }

    /// A file the rest of the repository leans on should be visibly easier to
    /// get to than one nothing refers to. Widths are compared on the same
    /// fixture so nothing but the reference count differs.
    #[test]
    fn a_much_referenced_holding_gets_a_wider_drive() {
        let mut tree = repository();
        // One hub everything imports, and one file nothing does.
        for child in &mut tree.children {
            if child.name == "src" {
                child.children[0].metrics.references = 60;
                child.children[1].metrics.references = 0;
            }
        }
        let layout = CityLayout::build(&tree);
        let roads = plan(&layout);

        let drive_for = |name: &str| {
            let building = layout
                .buildings
                .iter()
                .find(|building| building.name == name)
                .expect("the fixture holds this file");
            let door = building.footprint().center();
            roads
                .iter()
                .filter(|road| road.kind == RoadKind::Drive)
                .min_by(|first, second| {
                    let reach = |road: &MapRoad| {
                        distance_squared((road.points[0][0], road.points[0][1]), door)
                    };
                    reach(first).total_cmp(&reach(second))
                })
                .map(|road| road.width)
        };

        let hub = drive_for("module_0.rs").expect("the hub was given a drive");
        let quiet = drive_for("module_1.rs").expect("the quiet file was given a drive");
        assert!(
            hub > quiet * 1.5,
            "a file referenced 60 times got a {hub} wide drive against {quiet} for one \
             referenced not at all, which is not a difference anyone would read"
        );
    }

    /// **The King's own rule, for the second of the two marks.** Twice the
    /// references is twice the driveway.
    ///
    /// Measured above `DRIVE_WIDTH`, because the floor is what a file nothing
    /// imports gets and every other drive is read against it: what has to be
    /// proportional is the paving a file *earned*. The exponent this replaced
    /// drew a sixteen-times-imported file about nine times a lone one.
    ///
    /// A frontage wide enough that the wall is not what is being measured here;
    /// the cap has its own assertion in the test above.
    #[test]
    fn twice_the_references_buys_twice_the_driveway() {
        let frontage = 200.0;
        let earned = |references: usize| drive_width(references, frontage) - DRIVE_WIDTH;
        for (few, many) in [(1, 2), (2, 4), (4, 8), (8, 16)] {
            let step = earned(many) / earned(few);
            assert!(
                (step - 2.0).abs() < 0.01,
                "a file imported {many} times drew {step:.3}x the paving of one \
                 imported {few} times, which is not proportional"
            );
        }
        // And at any ratio, not only at doubling.
        let step = earned(12) / earned(3);
        assert!(
            (step - 4.0).abs() < 0.02,
            "four times the references drew {step:.3}x the paving"
        );
    }

    /// The knee is where proportionality stops, and above it a hub still grows
    /// rather than flattening onto the cap -- the plateau that a ceiling alone
    /// would produce. `DRIVE_MAX_WIDTH` is approached, never reached.
    #[test]
    fn a_much_referenced_hub_keeps_growing_without_reaching_the_cap() {
        let frontage = 200.0;
        let mut last = drive_width(DRIVE_LINEAR_REFERENCES as usize, frontage);
        for references in [20, 44, 51, 64, 256, 4_096] {
            let now = drive_width(references, frontage);
            assert!(
                now > last,
                "{references} references drew no more than the count below it"
            );
            assert!(
                now < DRIVE_MAX_WIDTH,
                "{references} references reached the cap, which is a plateau"
            );
            last = now;
        }
    }

    /// The curve has to stay readable at both ends: flat enough that a hub
    /// does not swallow its own house, steep enough that being referenced at
    /// all shows.
    #[test]
    fn drive_width_rises_with_references_and_stops() {
        let frontage = 40.0;
        let widths: Vec<f32> = [0, 1, 4, 16, 64, 256, 4_096]
            .iter()
            .map(|references| drive_width(*references, frontage))
            .collect();
        for pair in widths.windows(2) {
            assert!(pair[1] >= pair[0], "{widths:?} is not rising");
        }
        assert!((widths[0] - DRIVE_WIDTH).abs() < 1e-5, "{widths:?}");
        assert!(widths[2] > widths[0] * 1.5, "{widths:?}");
        assert!(
            *widths.last().expect("a width") <= DRIVE_MAX_WIDTH,
            "{widths:?}"
        );
        // A narrow house never gets a drive broader than the wall it leaves.
        assert!(drive_width(4_096, 6.0) <= 6.0 * DRIVE_FRONTAGE_SHARE + 1e-5);
    }

    /// Rising is not the same as *readable*, and only the second one is what
    /// the King was complaining about: the curve above has always risen, and a
    /// file with twenty references still arrived at a door barely distinguishable
    /// from a file with none.
    ///
    /// So this pins the property the eye actually judges — the whole ribbon,
    /// verge included, at the reference counts a real repository holds rather
    /// than at 4,096. It reads the drawn width through [`MapRoad::ribbon_width`]
    /// for the same reason: the builder widening a drive that the renderer then
    /// swallows in a flat kerb is precisely the failure this is guarding, and it
    /// cannot be caught by a test that only looks at the paving.
    #[test]
    fn a_quiet_drive_and_a_busy_one_are_told_apart_at_a_glance() {
        // Roomy enough that the wall is not what is being measured here; the
        // frontage cap has its own assertion above.
        let frontage = 80.0;
        let drawn = |references: usize| {
            MapRoad {
                kind: RoadKind::Drive,
                points: vec![[0.0, 0.0], [40.0, 0.0]],
                width: drive_width(references, frontage),
                traffic: references as u32 + 1,
                color: LANE,
                edge: LANE_EDGE,
            }
            .ribbon_width()
        };

        let (none, one, twenty) = (drawn(0), drawn(1), drawn(20));
        assert!(
            one >= none * 1.5,
            "one reference draws {one:.2} against {none:.2} for none — a {:.2}x \
             step, which at an isometric angle is no difference at all",
            one / none
        );
        assert!(
            twenty >= none * 5.0,
            "twenty references draw {twenty:.2} against {none:.2} for none, a \
             {:.2}x step",
            twenty / none
        );
    }

    /// through. This pins the bound directly. Both ends must lie inside the
    /// serving lot grown by its setback plus half the widest street, which is
    /// the furthest a drive can go before it is on someone else's ground.
    #[test]
    fn a_driveway_stays_on_the_lot_it_serves() {
        let layout = CityLayout::build(&repository());
        let roads = plan(&layout);
        let widest = roads
            .iter()
            .filter(|road| road.kind != RoadKind::Drive)
            .fold(0.0f32, |widest, road| widest.max(road.width));

        let drives: Vec<_> = roads
            .iter()
            .filter(|road| road.kind == RoadKind::Drive)
            .collect();
        assert!(
            drives.len() * 2 >= layout.buildings.len(),
            "only {} of {} holdings were given a drive, so the frontage cue is \
             missing from most of the map",
            drives.len(),
            layout.buildings.len()
        );

        for drive in drives {
            let start = (drive.points[0][0], drive.points[0][1]);
            // A drive leaves a wall, so the lot it serves is the one whose
            // house it starts against.
            let served = layout
                .buildings
                .iter()
                .min_by(|first, second| {
                    let reach = |building: &Building| {
                        let footprint = building.footprint();
                        distance_squared(start, footprint.center())
                    };
                    reach(first).total_cmp(&reach(second))
                })
                .expect("the fixture has holdings");

            let allowed = served.lot.inset(-(widest * 0.5 + LOT_SETBACK));
            for point in &drive.points {
                assert!(
                    within(allowed, (point[0], point[1])),
                    "a drive serving {} ran to {point:?}, outside its own lot \
                     {:?} and over a neighbour's ground",
                    served.path,
                    served.lot
                );
            }
        }
    }

    /// A drive comes out of a wall the camera can see.
    ///
    /// The map is isometric, so the two walls with growing x and growing y
    /// face the viewer and the other two are hidden behind the roof. A drive
    /// on a hidden wall is drawn under the building, which is worse than not
    /// drawing it: the frontage cue is gone and the mark is still there.
    #[test]
    fn a_driveway_comes_out_of_a_wall_the_camera_can_see() {
        for fixture in [repository(), single_folder_repository()] {
            let layout = CityLayout::build(&fixture);
            let roads = plan(&layout);
            let drives: Vec<_> = roads
                .iter()
                .filter(|road| road.kind == RoadKind::Drive)
                .collect();
            assert!(!drives.is_empty(), "the fixture produced no drives");

            let mut hidden = Vec::new();
            for drive in &drives {
                let (from, to) = (drive.points[0], drive.points[1]);
                let towards_viewer = if (to[1] - from[1]).abs() <= f32::EPSILON {
                    to[0] > from[0]
                } else {
                    to[1] > from[1]
                };
                if !towards_viewer {
                    hidden.push((from, to));
                }
            }
            assert!(
                hidden.is_empty(),
                "{} of {} drives run away from the camera and are hidden by \
                 their own house, the first from {:?} to {:?}",
                hidden.len(),
                drives.len(),
                hidden[0].0,
                hidden[0].1
            );
        }
    }

    /// The same two guarantees on a settlement big enough to expose the road
    /// planner's resolution.
    ///
    /// On a small map the planner's grid cell is already smaller than the lane
    /// between two wards, so the small fixture cannot catch a cell that has
    /// grown wider than the lane. On a large one it can: the lane then falls
    /// between the sampling points, avenues are squeezed along the inside edge
    /// of a ward, and they clip the holdings standing there. That is invisible
    /// at 34 files and showed up on every run at 1500.
    #[test]
    fn a_large_settlement_is_still_one_network_that_misses_every_holding() {
        let layout = CityLayout::build(&large_repository());
        let (roads, plaza) = plan_settlement(&layout);
        assert!(
            layout.buildings.len() > 500,
            "fixture is too small to test resolution: {} holdings",
            layout.buildings.len()
        );

        for road in &roads {
            for building in &layout.buildings {
                let footprint = building.footprint();
                for segment in road.points.windows(2) {
                    let crosses = overlap(footprint, segment[0], segment[1]);
                    assert!(
                        crosses <= 0.0,
                        "a {:?} road ran through {} for {crosses} units",
                        road.kind,
                        building.path
                    );
                }
            }
        }

        assert_eq!(
            islands(&roads, plaza.as_ref()),
            1,
            "a large settlement fell into several networks"
        );
    }

    /// A settlement with enough wards that the island is large, which is what
    /// drives the planner's grid coarse.
    ///
    /// The sizes vary deliberately. A fixture where every file and folder is
    /// identical packs into a tidy grid with room everywhere, and road faults
    /// that depend on a tight fit simply do not appear in it — the uniform
    /// version of this fixture passed while the real thing was splitting into
    /// two networks.
    /// A road is a rectangle, not a line, and it must fit between the wards.
    ///
    /// The width test above only asks where a road's *centre* runs. An avenue
    /// carrying a whole settlement is drawn forty units across, and for a
    /// long time it was drawn down a lane fixed at fifteen — so the busiest
    /// road on the map lay across the houses either side of it. The lane is
    /// now cut to fit the widest avenue the settlement can call for, and the
    /// avenue is cut again to fit the route it was actually given.
    #[test]
    fn a_road_is_never_laid_across_a_holding() {
        for root in [busy_repository(), large_repository(), repository()] {
            let layout = CityLayout::build(&root);
            for road in plan(&layout) {
                let half = road.width * 0.5;
                let start = road.points[0];
                for building in &layout.buildings {
                    let house = building.footprint();
                    // A drive begins against its own wall, so it is bound to
                    // lie on the house it serves. Every other house is fair
                    // game, and so is every other kind of road.
                    let serves = start[0] >= house.x - 0.5
                        && start[0] <= house.x + house.width + 0.5
                        && start[1] >= house.y - 0.5
                        && start[1] <= house.y + house.height + 0.5;
                    if road.kind == RoadKind::Drive && serves {
                        continue;
                    }
                    for segment in road.points.windows(2) {
                        let run = Rect {
                            x: segment[0][0].min(segment[1][0]) - half,
                            y: segment[0][1].min(segment[1][1]) - half,
                            width: (segment[0][0] - segment[1][0]).abs() + road.width,
                            height: (segment[0][1] - segment[1][1]).abs() + road.width,
                        };
                        let across =
                            (run.x + run.width).min(house.x + house.width) - run.x.max(house.x);
                        let along =
                            (run.y + run.height).min(house.y + house.height) - run.y.max(house.y);
                        assert!(
                            across <= 0.0 || along <= 0.0,
                            "a {:?} road {:.2} wide lay {:.2} units into {}",
                            road.kind,
                            road.width,
                            across.min(along),
                            building.path
                        );
                    }
                }
            }
        }
    }

    /// The way out of a busy quarter stays wide the whole way to the square.
    ///
    /// Squeezing an avenue to fit the ground it was given is only half an
    /// answer: do that alone and the busiest road on the map ends up a
    /// footpath, which is the very break this was meant to close. The lane
    /// between wards is cut to the width of the avenue that will run down it,
    /// so the avenue keeps the width its traffic earned.
    #[test]
    fn the_busiest_avenue_stays_a_wide_road() {
        let layout = CityLayout::build(&busy_repository());
        let roads = plan(&layout);
        let widest = roads
            .iter()
            .filter(|road| road.kind == RoadKind::Ward)
            .map(|road| road.width)
            .fold(0.0_f32, f32::max);
        let drives = roads
            .iter()
            .filter(|road| road.kind == RoadKind::Drive)
            .map(|road| road.width)
            .fold(0.0_f32, f32::max);
        assert!(
            widest >= drives,
            "the widest avenue is {widest:.2} but a drive is {drives:.2}"
        );
        assert!(
            widest > 30.0,
            "a settlement this busy should have a broad avenue, not {widest:.2}"
        );
    }

    /// Like the large fixture, but every file is heavily referenced, which is
    /// what drives the traffic on an avenue up to the top of its range.
    fn busy_repository() -> Node {
        let mut root = large_repository();
        fn cite(node: &mut Node) -> usize {
            if node.children.is_empty() {
                node.metrics.references = 24;
                return 24;
            }
            let total = node.children.iter_mut().map(cite).sum();
            node.metrics.references = total;
            total
        }
        cite(&mut root);
        root
    }

    fn large_repository() -> Node {
        let mut root = Node::directory("project".to_owned(), PathBuf::new());
        let mut noise = 12_345_u32;
        let mut next = move || {
            noise = noise.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            noise >> 16
        };
        for package in 0..60 {
            let name = format!("pkg_{package}");
            let mut folder = Node::directory(name.clone(), PathBuf::from(&name));
            let count = 6 + (next() % 40) as usize;
            let mut lines = 0;
            for index in 0..count {
                let mut child = file(
                    &format!("file_{index}.rs"),
                    &format!("{name}/file_{index}.rs"),
                );
                let size = 40 + (next() % 900) as usize;
                child.metrics = Metrics {
                    bytes: (size * 24) as u64,
                    lines: size,
                    file_count: 1,
                    ..Metrics::default()
                };
                lines += size;
                folder.children.push(child);
            }
            folder.metrics = Metrics {
                bytes: (lines * 24) as u64,
                lines,
                file_count: count,
                ..Metrics::default()
            };
            root.metrics.file_count += count;
            root.metrics.lines += lines;
            root.metrics.bytes += (lines * 24) as u64;
            root.children.push(folder);
        }
        root.children.push(file("README.md", "README.md"));
        root.metrics.file_count += 1;
        root
    }

    /// A road may only widen by what it has room for in the *worse* axis.
    ///
    /// The unit case behind `a_road_is_never_laid_across_a_holding`. A house
    /// sitting diagonally off the end of a run is 8.9 units clear one way and
    /// 13.7 the other; the straight line between them is 16.3, and a road
    /// granted 16.3 of half-width puts 2.1 units of paving through the wall.
    /// Only the larger of the two gaps is ever safe, because a road widens on
    /// both axes at once.
    #[test]
    fn a_road_widens_by_the_worse_gap_and_not_the_diagonal() {
        let run = Rect {
            x: 0.0,
            y: 0.0,
            width: 100.0,
            height: 0.0,
        };
        let house = Rect {
            x: 108.9,
            y: 13.7,
            width: 20.0,
            height: 10.0,
        };

        let room = widening_room(run, house);
        assert!(
            (room - 13.7).abs() < 1e-3,
            "a road was offered {room:.2} of room where only 13.70 is safe"
        );
        assert!(
            room < 8.9_f32.hypot(13.7),
            "the diagonal is still what is being handed out"
        );
    }

    /// A house standing on the route has no room at all, rather than the
    /// overlap being handed back as though it were clearance.
    #[test]
    fn a_house_on_the_route_offers_no_room() {
        let run = Rect {
            x: 0.0,
            y: 0.0,
            width: 100.0,
            height: 0.0,
        };
        let across = Rect {
            x: 40.0,
            y: -5.0,
            width: 10.0,
            height: 10.0,
        };

        assert_eq!(widening_room(run, across), 0.0);
    }

    /// Everything under a single top-level folder, which is the commonest
    /// shape a repository takes and the one that used to strand the square.
    ///
    /// The other large fixture spreads its packages across the root, so there
    /// are sixty top-level wards and the square always finds a gap between
    /// two of them. Here there is exactly one ward, covering nearly the whole
    /// map — and a ward is solid ground to the router.
    fn single_folder_repository() -> Node {
        let mut root = Node::directory("project".to_owned(), PathBuf::new());
        let mut source = Node::directory("src".to_owned(), PathBuf::from("src"));
        let mut noise = 8_191_u32;
        let mut next = move || {
            noise = noise.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            noise >> 16
        };
        for package in 0..40 {
            let name = format!("pkg_{package}");
            let path = format!("src/{name}");
            let mut folder = Node::directory(name, PathBuf::from(&path));
            let count = 6 + (next() % 34) as usize;
            let mut lines = 0;
            for index in 0..count {
                let mut child = file(
                    &format!("file_{index}.rs"),
                    &format!("{path}/file_{index}.rs"),
                );
                let size = 40 + (next() % 900) as usize;
                child.metrics = Metrics {
                    bytes: (size * 24) as u64,
                    lines: size,
                    file_count: 1,
                    ..Metrics::default()
                };
                lines += size;
                folder.children.push(child);
            }
            folder.metrics = Metrics {
                bytes: (lines * 24) as u64,
                lines,
                file_count: count,
                ..Metrics::default()
            };
            source.metrics.file_count += count;
            source.metrics.lines += lines;
            source.metrics.bytes += (lines * 24) as u64;
            source.children.push(folder);
        }
        root.metrics = source.metrics;
        root.children.push(source);
        for index in 0..6 {
            root.children.push(file(
                &format!("note_{index}.md"),
                &format!("note_{index}.md"),
            ));
            root.metrics.file_count += 1;
        }
        root
    }

    /// A settlement whose wards all sit inside one dominant folder still has a
    /// square its roads can reach.
    ///
    /// A ward is solid ground to the router, so a square placed inside one is
    /// walled in: every avenue heading for it fails to find a route and falls
    /// back to a straight line, which runs through whatever holdings lie
    /// between. That is not a near miss — it put a road clean through nine
    /// houses over 1500 units, and it is invisible to any fixture that spreads
    /// its folders across the root.
    #[test]
    fn a_settlement_under_one_folder_still_has_a_reachable_square() {
        let layout = CityLayout::build(&single_folder_repository());
        let (roads, plaza) = plan_settlement(&layout);
        let plaza = plaza.expect("a settlement this size has a square");

        let wards: Vec<_> = layout
            .districts
            .iter()
            .filter(|ward| ward.depth == 0)
            .collect();
        assert_eq!(
            wards.len(),
            1,
            "the fixture is meant to have one dominant ward, not {}",
            wards.len()
        );
        let square = Rect {
            x: plaza.rect.x,
            y: plaza.rect.y,
            width: plaza.rect.width,
            height: plaza.rect.depth,
        };
        assert!(
            !rects_overlap(square, wards[0].rect, 0.0),
            "the square at {square:?} was placed inside ward {} at {:?}, where \
             no road can reach it",
            wards[0].path,
            wards[0].rect
        );

        // The failure this guards is a *fallback* route, so the damage shows
        // up as roads through holdings rather than as a broken network.
        for road in &roads {
            for building in &layout.buildings {
                let footprint = building.footprint();
                for segment in road.points.windows(2) {
                    let crosses = overlap(footprint, segment[0], segment[1]);
                    assert!(
                        crosses <= 0.0,
                        "a {:?} road ran through {} for {crosses} units",
                        road.kind,
                        building.path
                    );
                }
            }
        }
        assert_eq!(islands(&roads, Some(&plaza)), 1, "the network came apart");
    }

    /// The road planner can only see a gap it samples, so its grid cell has to
    /// stay narrower than the lane between two wards no matter how large the
    /// settlement grows. When it does not, the only open ground in the
    /// settlement falls between the sampling points: avenues get squeezed
    /// along the inside edge of a ward, where the holdings are, or fail to
    /// find a route at all.
    #[test]
    fn the_planner_can_always_see_the_lane_between_two_wards() {
        for span in [200.0_f32, 2_000.0, 20_000.0, 200_000.0] {
            let extent = Rect {
                x: 0.0,
                y: 0.0,
                width: span,
                height: span,
            };
            let ground = Ground::new(extent, &[], WARD_GAP);
            assert!(
                ground.cell < WARD_GAP,
                "over {span} units the grid cell is {} but wards are only {WARD_GAP} apart",
                ground.cell
            );
        }
    }

    /// The heuristic must not change a single road.
    ///
    /// [`Ground::route`] was an exhaustive Dijkstra and is now A*, which is a
    /// change made purely for speed -- so the thing worth pinning is that it
    /// bought no difference in the answer. An inadmissible heuristic still
    /// returns *a* route, just a worse one, and every other road test here asks
    /// only whether the network is connected and misses every holding. All of
    /// them pass just as happily on a road four turns longer than it needed to
    /// be, which is why this compares against the old search directly.
    ///
    /// Random grids rather than the fixture repository, because the property is
    /// about the search and not about any one settlement: the cases that break
    /// an A* are awkward geometry -- a goal directly behind the start, a
    /// corridor that forces a detour, a start standing on blocked ground -- and
    /// three thousand random boards find those far more reliably than a scene
    /// somebody chose.
    ///
    /// The generator is a plain xorshift rather than a dependency: it needs to
    /// be repeatable and to spread over the space, and nothing more.
    #[test]
    fn the_heuristic_never_changes_a_route() {
        let mut seed = 0x2545_F491_4F6C_DD1Du64;
        let mut rand = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };

        for case in 0..3_000 {
            let width = 40.0 + (rand() % 400) as f32;
            let height = 40.0 + (rand() % 400) as f32;
            let extent = Rect {
                x: 0.0,
                y: 0.0,
                width,
                height,
            };
            let obstacles: Vec<Rect> = (0..(rand() % 12) as usize)
                .map(|_| Rect {
                    x: (rand() % width as u64) as f32,
                    y: (rand() % height as u64) as f32,
                    width: 5.0 + (rand() % 60) as f32,
                    height: 5.0 + (rand() % 60) as f32,
                })
                .collect();
            let ground = Ground::new(extent, &obstacles, WARD_GAP);

            let from = (
                (rand() % width as u64) as f32,
                (rand() % height as u64) as f32,
            );
            let to = (
                (rand() % width as u64) as f32,
                (rand() % height as u64) as f32,
            );

            assert_eq!(
                ground.route(from, to),
                ground.route_exhaustive(from, to),
                "case {case}: the heuristic changed the road from {from:?} to {to:?} \
                 across {extent:?} with {obstacles:?}"
            );
        }
    }

    #[test]
    fn the_network_reaches_every_ward() {
        let layout = CityLayout::build(&repository());
        let roads = plan(&layout);
        for ward in layout.districts.iter().filter(|ward| ward.depth == 0) {
            let served = roads.iter().any(|road| {
                road.points
                    .iter()
                    .any(|point| within(ward.rect, (point[0], point[1])))
            });
            assert!(served, "ward {} was never reached", ward.path);
        }
    }

    /// The whole point of the exercise: you can drive from any holding to any
    /// other. A network that merely *reaches* every ward can still be several
    /// islands that never meet, which is exactly what it was — chains of small
    /// holdings each arriving at one lot edge and leaving from another, so the
    /// roads passed close by without ever joining. Union-find over the
    /// segments is the only check that catches it.
    #[test]
    fn the_network_is_all_one_piece() {
        let layout = CityLayout::build(&repository());
        let (roads, plaza) = plan_settlement(&layout);
        assert!(roads.len() > 1, "expected a network, not a lone road");
        let islands = islands(&roads, plaza.as_ref());
        assert_eq!(
            islands, 1,
            "the roads fall into {islands} separate networks that never meet"
        );
    }

    /// How many separate networks the roads fall into.
    ///
    /// The square counts as one of them: it is the one thing besides a road
    /// you can cross, so roads stopping at different sides of it really are
    /// joined. Everything else has to touch kerb to kerb.
    fn islands(roads: &[MapRoad], plaza: Option<&MapPlaza>) -> usize {
        let mut segments = Vec::new();
        for road in roads {
            for pair in road.points.windows(2) {
                segments.push((pair[0], pair[1], road.width * 0.5));
            }
        }
        if segments.is_empty() {
            return 0;
        }

        let square = segments.len();
        let mut group: Vec<usize> = (0..=square).collect();
        fn root(group: &mut [usize], mut node: usize) -> usize {
            while group[node] != node {
                group[node] = group[group[node]];
                node = group[node];
            }
            node
        }
        fn join(group: &mut [usize], left: usize, right: usize) {
            let (x, y) = (root(group, left), root(group, right));
            if x != y {
                group[x] = y;
            }
        }
        let on_square = |point: [f32; 2]| match plaza {
            Some(plaza) => {
                point[0] >= plaza.rect.x - 2.0
                    && point[0] <= plaza.rect.x + plaza.rect.width + 2.0
                    && point[1] >= plaza.rect.y - 2.0
                    && point[1] <= plaza.rect.y + plaza.rect.depth + 2.0
            }
            None => false,
        };

        for left in 0..segments.len() {
            let (a, b, half_a) = segments[left];
            if on_square(a) || on_square(b) {
                join(&mut group, left, square);
            }
            for (offset, &(c, d, half_b)) in segments[left + 1..].iter().enumerate() {
                let right = left + 1 + offset;
                let gap = distance_to_segment((a[0], a[1]), c, d)
                    .min(distance_to_segment((b[0], b[1]), c, d))
                    .min(distance_to_segment((c[0], c[1]), a, b))
                    .min(distance_to_segment((d[0], d[1]), a, b));
                // Kerbs that touch are joined; the extra unit covers roads
                // that meet at a corner rather than overlapping squarely.
                if gap <= half_a + half_b + 1.0 {
                    join(&mut group, left, right);
                }
            }
        }

        (0..segments.len())
            .map(|segment| root(&mut group, segment))
            .collect::<std::collections::BTreeSet<usize>>()
            .len()
    }

    /// A well must be legible against the paving it stands on.
    ///
    /// The sibling of `map::network`'s test that a well is not the colour of an
    /// *agent*, and the question that one cannot ask: it compares the well
    /// against the banners, which say nothing about the stone under it. A well
    /// now stands on a town's square rather than on open ground, so a colour
    /// close to [`PLAZA`] would be camouflage -- the mark would be exactly
    /// where the King was told to look and invisible when he looked.
    ///
    /// This test lives here because this module owns the paving colour and
    /// `map::network` owns the well's, and nothing else can see both. The bar
    /// is the palette's own: the two nearest agent banners, 126.1 apart on the
    /// weighted-RGB ruler `palette`'s hue search used. Stone `#9a9187` sits
    /// 141.3 from the paving, and the water 162.5.
    ///
    /// Each part is measured against **what it is actually drawn on**, which is
    /// not the same surface for all three. The stonework and the shaft of water
    /// meet the paving; the timber of the canopy is painted over the stonework
    /// and never touches the square, so holding it to the paving asked it to
    /// contrast with something it is nowhere adjacent to. It is a dark brown on
    /// pale stone -- 209.0 apart -- and reads at a glance.
    #[test]
    fn a_well_is_legible_against_the_paving_it_stands_on() {
        /// The same weighted-RGB approximation `kingdom_core::palette` and
        /// `map::network` measure with. Not a colour-science claim -- one
        /// consistent ruler, which is what a regression test needs.
        fn distance(a: [u8; 3], b: [u8; 3]) -> f64 {
            let mean = (a[0] as f64 + b[0] as f64) / 2.0 / 255.0;
            let (dr, dg, db) = (
                a[0] as f64 - b[0] as f64,
                a[1] as f64 - b[1] as f64,
                a[2] as f64 - b[2] as f64,
            );
            ((2.0 + mean) * dr * dr + 4.0 * dg * dg + (3.0 - mean) * db * db).sqrt()
        }

        let banners = kingdom_core::palette::BANNERS;
        let closest_pair = banners
            .iter()
            .enumerate()
            .flat_map(|(index, a)| {
                banners[index + 1..]
                    .iter()
                    .map(move |b| distance(a.growth_rgb, b.growth_rgb))
            })
            .fold(f64::MAX, f64::min);

        let paving = [PLAZA[0], PLAZA[1], PLAZA[2]];
        let stone = {
            let c = crate::map::network::WELL_COLOR;
            [c[0], c[1], c[2]]
        };
        for (part, color, ground, ground_name) in [
            (
                "stonework",
                crate::map::network::WELL_COLOR,
                paving,
                "the paving it stands on",
            ),
            (
                "water",
                crate::map::network::WELL_WATER_COLOR,
                paving,
                "the paving it stands on",
            ),
            (
                "timber",
                crate::map::network::WELL_TIMBER_COLOR,
                stone,
                "the stonework it is built on",
            ),
        ] {
            let apart = distance([color[0], color[1], color[2]], ground);
            assert!(
                apart >= closest_pair,
                "a well's {part} is {apart:.1} from {ground_name}, closer than \
                 the two nearest agents are to each other ({closest_pair:.1}) \
                 -- the well would be camouflage on its own square"
            );
        }
    }

    fn distance_to_segment(point: (f32, f32), start: [f32; 2], end: [f32; 2]) -> f32 {
        let dx = end[0] - start[0];
        let dy = end[1] - start[1];
        let length_squared = dx * dx + dy * dy;
        if length_squared == 0.0 {
            return distance_squared(point, (start[0], start[1])).sqrt();
        }
        let t = (((point.0 - start[0]) * dx + (point.1 - start[1]) * dy) / length_squared)
            .clamp(0.0, 1.0);
        distance_squared(point, (start[0] + dx * t, start[1] + dy * t)).sqrt()
    }
}
