//! Wells, networks, and who is plugged into what -- as ground on the map.
//!
//! The map already answers *what is every agent doing right now?* This module
//! answers the question behind it, the second of the three in `AGENTS.md`:
//! **what shared resources are they holding, and what is each of them
//! connected to?**
//!
//! Three marks, and each is a fact the King cannot get anywhere else on screen:
//!
//! - **the host ring**, a band just inside the realm's rim. It is the King's
//!   own machine -- his `localhost` -- drawn as the edge of his world.
//! - **a wellhead**, standing on a town's square, one per service that city has
//!   running. The well is the *city's*, not any plan's, which is why it stands
//!   on the square rather than beside an agent.
//! - **an agent marker**, in that agent's own banner colour, joined by a
//!   conduit to the host ring if it is on the shared network, ringed by a moat
//!   if it has a network of its own, and joined to a wellhead by a channel if
//!   it is actually drawing from that well.
//!
//! # The one thing this exists to show
//!
//! An **isolated agent still reaches its city's well**. That is the fact that
//! surprised the author of `kingdom_app::services` and it is invisible
//! everywhere else in the interface: `slirp4netns` runs with
//! `--disable-host-loopback`, which blocks `127.0.0.1` and nothing else, so a
//! Docker bridge address is just another host route. A moat with no conduit to
//! the rim *and* a channel to the wellhead is a picture that says exactly that,
//! and no badge or list in the chamber says it at all.
//!
//! # Why this is in `map` and not in `engine`
//!
//! For the reason [`super::works`] is: this is the *wire shape* between the
//! interface and the renderer, and none of it is drawing. `cargo test` builds
//! this crate with no features at all, so the placement arithmetic below is
//! pinned on a bare machine rather than checked by eye in a browser once.
//!
//! # Why the manifest does not carry it
//!
//! `kingdom_app::citymap` memoises the map JSON -- seconds of filesystem work,
//! megabytes of geometry -- keyed on the kingdom root and its city names, and
//! deliberately not on anything that moves. Which agent is on which network
//! moves every few seconds. So this travels as a
//! [`ViewerCommand::SetNetwork`](crate::engine::bridge::ViewerCommand::SetNetwork),
//! exactly as the works and the working rings do.

use super::{MapColor, MapRect};
use serde::{Deserialize, Serialize};

/// Everything the map draws about wells and networks, in world space.
///
/// Replaces whatever was drawn before rather than amending it, for
/// [`super::works`]'s reason: an agent that has finished must *stop* being
/// drawn, and an amending command has no way to say that. An empty picture is
/// the ordinary way to say "nothing is connected to anything".
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NetworkPicture {
    /// The band standing for the King's own machine, if this map has a rim to
    /// draw it on.
    pub host: Option<HostRing>,
    /// Every well standing in every town.
    pub wells: Vec<Wellhead>,
    /// Every live agent's marker.
    pub agents: Vec<AgentMark>,
    /// The lines joining agents to what they reach.
    pub links: Vec<Link>,
}

impl NetworkPicture {
    /// Whether there is nothing at all to draw.
    ///
    /// The common answer on a real dev folder: no project declares a service
    /// and no plan is open. What lets the renderer skip its whole pass.
    pub fn is_quiet(&self) -> bool {
        self.host.is_none() && self.wells.is_empty() && self.agents.is_empty()
    }
}

/// The King's own machine, drawn as a band just inside the realm's rim.
///
/// # Why the rim, and why this costs no relayout
///
/// `build::scene::disk` draws the world's edge around the ground the towns
/// actually cover and then adds a margin (`RIM_MARGIN`), so there is already
/// empty ground between the outermost town and the rim. The band sits in that
/// fringe: nothing moves to make room for it, and it cannot collide with a
/// settlement.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HostRing {
    /// The closed path the band follows, inset from the rim.
    pub path: Vec<[f32; 2]>,
    /// How wide to stroke it, in world units.
    pub width: f32,
    /// Its colour.
    pub color: MapColor,
}

/// One well: a container a whole city shares, standing on that city's square.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Wellhead {
    /// The town whose square this stands on, by name. The engine knows towns by
    /// name and never by `CityId` -- see the module docs on that boundary.
    pub town: String,
    /// The service's name, e.g. `db`.
    pub name: String,
    /// Where it stands.
    pub center: [f32; 2],
    /// How far across the wellhead is.
    pub radius: f32,
    /// How many plans are drawing from it right now.
    ///
    /// Carried so the renderer can say *shared by five* without asking anyone:
    /// the answer to "who else is in here?", which is the question the King
    /// actually has before he changes something in a shared database.
    pub users: usize,
    /// Its colour.
    pub color: MapColor,
}

/// One agent, standing in the town it works in.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentMark {
    /// The town it stands in, by name.
    pub town: String,
    /// Where it stands.
    pub center: [f32; 2],
    /// How far across the marker is.
    pub radius: f32,
    /// What the King calls this agent -- its plan's title.
    ///
    /// Painted on a plaque over the marker at the closest tier, exactly as a
    /// house is named. Carried here rather than looked up because the engine
    /// knows nothing of plans: see the module docs on that seam.
    pub label: String,
    /// This agent's banner colour -- the same colour as the columns it is
    /// raising and the chip in the rail. See [`resolve`].
    pub color: MapColor,
    /// Whether it has a network of its own, which is drawn as a closed moat.
    ///
    /// The absence of a [`LinkKind::ToHost`] link says the same thing, but this
    /// says it positively: isolation is a state of the agent, not merely a
    /// missing line, and the renderer should not have to search the link list
    /// to find out.
    pub isolated: bool,
}

/// What one line on the map joins, and therefore what it means.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum LinkKind {
    /// An agent that reaches the King's own machine directly: a conduit from
    /// its marker out to the host ring.
    ToHost,
    /// An agent drawing from one of its city's wells.
    ToWell,
}

/// A line joining an agent to something it reaches.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Link {
    /// What this line means.
    pub kind: LinkKind,
    /// The path to stroke, in world coordinates.
    pub points: Vec<[f32; 2]>,
    /// How wide to stroke it.
    pub width: f32,
    /// Its colour -- the agent's own, so a line can be traced back to whose it
    /// is at a glance.
    pub color: MapColor,
}

// The measurements `resolve` places marks by. Gated exactly as `resolve` is:
// only the browser lays anything out, and under a plain `ssr` build these would
// be six constants nothing reads. The types above are *not* gated -- the wire
// shape is compiled on both targets, as the whole `map` module is.
#[cfg(any(feature = "hydrate", test))]
mod measures {
    /// How far inside the rim the host band sits, as a share of the disk's radius.
    ///
    /// Small: the band belongs to the *edge* of the world, and pulling it further
    /// in would read as a road between towns rather than as the boundary.
    pub(super) const HOST_INSET: f32 = 0.045;

    /// How wide the host band is stroked, in world units.
    ///
    /// Heavier than any road on the map. It is the one mark that stands for
    /// something outside the kingdom entirely, and at the `Districts` tier -- where
    /// a house is twenty pixels -- it is the only thing at the rim that has to
    /// still read.
    pub(super) const HOST_WIDTH: f32 = 14.0;

    /// How wide a conduit or channel is stroked.
    ///
    /// Deliberately lighter than it first was (3.4). Seen on screen, conduits at
    /// that weight were heavier than the roads they cross and the eye read them
    /// as the settlement's own streets. A connection is a *thread* between two
    /// marks: the marks carry the meaning and the line only says which two.
    pub(super) const LINK_WIDTH: f32 = 1.8;

    /// How far across an agent's marker is, in world units.
    pub(super) const AGENT_RADIUS: f32 = 9.0;

    /// How far across a wellhead is, at most.
    ///
    /// Down from 13, which was chosen while a well floated on open ground at a
    /// town's centre. It stands on the square now, and a square is 52 units
    /// across: at 13 the drum was a quarter of the whole paving. A well no
    /// longer has to be the biggest disc on the map to read as a destination --
    /// it is built, lit, and standing on stone, which the marks around it are
    /// not.
    ///
    /// A ceiling rather than a fixed size: see
    /// [`well_stand`](super::resolve::well_stand), which shrinks it when
    /// several wells share one square.
    pub(super) const WELL_RADIUS: f32 = 8.5;

    /// How far a wellhead keeps off the kerb, in world units.
    ///
    /// Small enough that two wells still fit a 52-unit square side by side,
    /// large enough that a drum reads as standing *on* the paving rather than
    /// straddling its edge.
    pub(super) const KERB_INSET: f32 = 3.0;

    /// How far beyond a town's own edge its agents stand, in world units.
    ///
    /// A margin *outside* the settlement rather than a radius within it -- see
    /// [`agent_stand`](super::resolve::agent_stand). Large enough to clear the
    /// outermost holdings and the kerb around them, small enough that a marker
    /// still plainly belongs to the town it rings rather than floating in the
    /// space between two.
    pub(super) const AGENT_ORBIT: f32 = 34.0;
}

#[cfg(any(feature = "hydrate", test))]
use measures::*;

#[cfg(any(feature = "hydrate", test))]
pub use resolve::resolve;

#[cfg(any(feature = "hydrate", test))]
mod resolve {
    use super::*;
    use crate::map::MapManifest;
    use kingdom_core::{AgentNetwork, CityWells, KingdomNetwork};

    /// Turns what the server knows into ground on the map.
    ///
    /// Gated exactly as [`crate::map::works::resolve`] is, and for the same two
    /// reasons: only the browser ever calls it, and `kingdom-core` is a *dev*
    /// dependency of this crate on native -- so this compiles under `cargo
    /// test`, where the judgements below are pinned without a browser, and
    /// under `hydrate`, where it runs.
    ///
    /// # What it refuses to draw
    ///
    /// Every omission is a judgement about what is *honest*:
    ///
    /// - **A city the manifest never drew is skipped.** An empty project is
    ///   dropped from the manifest entirely (`build::manifest_for`), so there
    ///   is no town to stand in. Drawing the agent anyway would put it on some
    ///   other city's ground.
    /// - **A town with no square gets no wellhead.** The square is the well's
    ///   place; a settlement too small to have been given one
    ///   (`streets::settlement_roads` returns `None`) has nowhere for it to
    ///   stand that would not be somebody's front garden.
    /// - **A channel is drawn only to a well that is actually standing.** The
    ///   feed reports what a plan is registered as drawing from, and a name
    ///   that resolves to no wellhead is dropped rather than drawn to nowhere.
    ///
    /// # Why the colours are assigned over the whole kingdom
    ///
    /// `palette::assign_banners` is given **every** agent at once, exactly as
    /// `works::resolve` does it, because that is what guarantees an agent is
    /// the same colour here as the columns it is raising and the chip in the
    /// rail. Assigning per town would let two agents in different projects take
    /// the same hue and make the map disagree with itself.
    pub fn resolve(map: &MapManifest, network: &KingdomNetwork) -> NetworkPicture {
        let banners = kingdom_core::palette::assign_banners(
            &network
                .agents
                .iter()
                .map(|agent| agent.plan.clone())
                .collect::<Vec<_>>(),
        );

        let host = host_ring(map);
        let wells = wellheads(map, &network.wells);

        let mut agents = Vec::new();
        let mut links = Vec::new();

        // Grouped by town so several agents in one city can be spaced around
        // its square without standing in each other. Walked in the order the
        // feed gave, which is sorted by plan id -- so a marker keeps its place
        // between refetches rather than hopping, the guarantee
        // `works::place_fresh` gives a ghost house.
        for (agent, (_, banner)) in network.agents.iter().zip(banners.iter()) {
            let town_name = agent.city.as_str();
            let Some(town) = map.town_named(town_name) else {
                // A city the map never drew. See the doc above.
                continue;
            };

            let placed_here = agents
                .iter()
                .filter(|mark: &&AgentMark| mark.town == town_name)
                .count();
            let center = agent_stand(town.center, town.extent, placed_here);
            let color = growth_of(banner);

            if agent.on_host_network() {
                if let Some(ring) = host.as_ref() {
                    links.push(Link {
                        kind: LinkKind::ToHost,
                        points: vec![center, nearest_on(&ring.path, center)],
                        width: LINK_WIDTH,
                        color,
                    });
                }
            }

            for well in wells_reached(&wells, town_name, agent) {
                links.push(Link {
                    kind: LinkKind::ToWell,
                    points: vec![center, well],
                    width: LINK_WIDTH,
                    color,
                });
            }

            agents.push(AgentMark {
                town: town_name.to_owned(),
                center,
                radius: AGENT_RADIUS,
                label: agent.title.clone(),
                color,
                isolated: !agent.on_host_network(),
            });
        }

        NetworkPicture {
            host,
            wells,
            agents,
            links,
        }
    }

    /// The agent's colour for lines added -- the brighter of its two.
    ///
    /// The same half of the palette `works::resolve` paints a growth band with,
    /// so a marker and the column beside it are the same hue rather than two
    /// shades of one agent.
    fn growth_of(banner: &kingdom_core::AgentPalette) -> MapColor {
        let [r, g, b] = banner.growth_rgb;
        [r, g, b, 255]
    }

    /// Where every wellhead in the kingdom stands.
    fn wellheads(map: &MapManifest, wells: &[CityWells]) -> Vec<Wellhead> {
        let mut out = Vec::new();
        for city in wells {
            let town_name = city.city.as_str();
            if map.town_named(town_name).is_none() {
                // A city the map never drew. See the doc above.
                continue;
            }
            let Some(square) = map.square_of(town_name) else {
                // A town too small to have been given a square. The rule this
                // module has always documented, and until now did not keep:
                // there is nowhere for a well to stand that would not be
                // somebody's front garden.
                continue;
            };
            // Several services in one city are laid out along the square rather
            // than stacked, so a city with a database and a cache shows two
            // wellheads and not one.
            for (index, service) in city.wells.iter().enumerate() {
                let (center, radius) = well_stand(square, index, city.wells.len());
                out.push(Wellhead {
                    town: town_name.to_owned(),
                    name: service.name.clone(),
                    center,
                    radius,
                    users: service.users,
                    color: WELL_COLOR,
                });
            }
        }
        out
    }

    /// The centres of the wells one agent is actually drawing from.
    fn wells_reached(wells: &[Wellhead], town: &str, agent: &AgentNetwork) -> Vec<[f32; 2]> {
        wells
            .iter()
            .filter(|well| {
                well.town == town && agent.drawing_from.iter().any(|name| name == &well.name)
            })
            .map(|well| well.center)
            .collect()
    }

    /// The band standing for the King's own machine.
    ///
    /// `None` when the map has no rim to inset from, which is a world with
    /// nothing on it.
    fn host_ring(map: &MapManifest) -> Option<HostRing> {
        let rim = &map.world.rim;
        if rim.len() < 3 {
            return None;
        }

        let center = map.world.bounds.center();
        let path = rim
            .iter()
            .map(|point| {
                // Pulled towards the middle by a share of its own distance, so
                // the band follows whatever shape the disk actually has rather
                // than assuming a circle.
                [
                    center[0] + (point[0] - center[0]) * (1.0 - HOST_INSET),
                    center[1] + (point[1] - center[1]) * (1.0 - HOST_INSET),
                ]
            })
            .collect();

        Some(HostRing {
            path,
            width: HOST_WIDTH,
            color: HOST_COLOR,
        })
    }

    /// The point on a closed path nearest a given place.
    ///
    /// What a conduit runs to: an agent joins the host ring at the closest
    /// point on it, so the line is the shortest honest one rather than aimed at
    /// an arbitrary spot on the rim.
    fn nearest_on(path: &[[f32; 2]], from: [f32; 2]) -> [f32; 2] {
        let mut best = path[0];
        let mut best_distance = f32::MAX;
        for point in path {
            let dx = point[0] - from[0];
            let dy = point[1] - from[1];
            let distance = dx * dx + dy * dy;
            if distance < best_distance {
                best_distance = distance;
                best = *point;
            }
        }
        best
    }

    /// Where the `index`-th agent in a town stands.
    ///
    /// **Outside the settlement, not inside it.** The first version orbited a
    /// fixed 34 units from the town's centre, and on screen that put the markers
    /// down among the houses -- one stood on the keep in the middle of the
    /// square. An agent is not a building and must not look like it has been
    /// built there.
    ///
    /// So the orbit is derived from the town's own [`MapLocation::extent`]: the
    /// marks ring the settlement, clear of its outermost holdings, where the
    /// realm's packing already leaves a lane. They are what stands *around* a
    /// town, in the same fringe the roads between towns run through.
    ///
    /// Spaced by the golden angle rather than evenly, so a town with two agents
    /// and a town with seven both read without the ring falling into spokes --
    /// the arrangement `works::place_fresh` and `build::layout` both use.
    fn agent_stand(town_center: [f32; 2], extent: [f32; 2], index: usize) -> [f32; 2] {
        const GOLDEN_ANGLE: f32 = 2.399_963_2;
        // Half the town's larger span puts the ring at its corner; the margin
        // carries it just past, into the lane outside.
        let reach = extent[0].max(extent[1]) * 0.5 + AGENT_ORBIT;
        let angle = index as f32 * GOLDEN_ANGLE;
        [
            town_center[0] + angle.cos() * reach,
            town_center[1] + angle.sin() * reach,
        ]
    }

    /// Where the `index`-th of `total` wellheads on a square stands, and how
    /// far across it is.
    ///
    /// **On the paving, at the back of it.** The first version stood a well at
    /// the town's centre point, which is not the square at all: a square is
    /// walked *outward* from the settlement's middle until it finds ground no
    /// ward has claimed (`build::streets::square_site`), because the middle is
    /// the one place the largest folder has already taken. Measured across a
    /// real kingdom of seven towns, that walk landed between 94 and 1,622 units
    /// out -- against a square 52 units across. So every well on the map stood
    /// among the houses, the same fault the agent marks were fixed for.
    ///
    /// The **rear** edge, and that is not arbitrary. The square already carries
    /// the town's name painted across its middle (`wayfinding::square_label`,
    /// a cap height of up to 0.26 of the square), and the camera looks down
    /// `(-1, -1, -1)`, so low `y` projects up-screen. A well at the back stands
    /// above the lettering rather than on it.
    ///
    /// The radius shrinks when several wells share one square: each takes an
    /// equal slot of the usable span, so a city with three services shows three
    /// smaller wells on the paving rather than a row spilling onto the grass.
    /// It is capped by the depth of that rear strip too, which is what keeps a
    /// lone well off the lettering on a square of any size.
    fn well_stand(square: MapRect, index: usize, total: usize) -> ([f32; 2], f32) {
        let total = total.max(1);
        // Room enough that a drum never overhangs the kerb, taken off both
        // ends before the span is shared out.
        let usable = (square.width - KERB_INSET * 2.0).max(1.0);
        let slot = usable / total as f32;

        // The strip between the rear kerb and the lettering, which is all the
        // depth there is: a kerb's width is kept clear at both ends of it, so a
        // drum neither hangs off the paving nor touches the name.
        let band = square.depth * (1.0 - SQUARE_LABEL_SHARE) * 0.5;
        let depth_allows = (band - KERB_INSET * 2.0) * 0.5;

        let radius = WELL_RADIUS.min(slot * 0.42).min(depth_allows).max(1.0);

        let left = square.x + KERB_INSET;
        let x = left + slot * (index as f32 + 0.5);
        // A well's own radius back from the rear kerb, so the drum sits fully
        // on the paving whatever size it ended up.
        let y = square.y + KERB_INSET + radius;
        ([x, y], radius)
    }
}

/// The colour of the host ring.
///
/// A cool slate, deliberately not any agent's banner and not the working green:
/// it stands for the King's own machine rather than for anything an agent is
/// doing, and a status colour here would say something untrue.
///
/// Drawn unlit, like every other piece of interface on this map --
/// `engine::activity::WORKING_COLOR` records the three attempts that
/// established why a lit status colour comes out wrong.
pub const HOST_COLOR: MapColor = [0x7d, 0x9c, 0xc4, 255];

/// The colour of a wellhead's stonework.
///
/// A weathered grey stone. It replaced a pale near-white (`#cfd8dd`) that was
/// drawn **unlit** among lit earth tones and read, correctly, as a light
/// source: nothing else in a settlement glows, so the one thing that did looked
/// like it was switched on. The well is lit now (`engine::network`), and this is
/// a base colour the sun shades rather than a final pixel.
///
/// Two distances are asked of it, on the weighted-RGB ruler `palette`'s own hue
/// search used, where the two nearest *banners* are 126.1 apart:
///
/// - **165.5 from its nearest banner.** Further than any two agents ever are, so
///   a well cannot be mistaken for one. The test below pins that margin rather
///   than mere inequality, which is what let an earlier `#38bdf8` pass while
///   being visibly the same cyan as the `azure` agent beside it.
/// - **141.3 from the paving it stands on** (`streets::PLAZA`, `#816941`). A
///   test in `build::streets` pins that one, because a well the colour of its
///   own square is a well nobody can see. The old near-white was 349.9 from the
///   paving -- legible, and that is the only thing it had going for it.
pub const WELL_COLOR: MapColor = [0x9a, 0x91, 0x87, 255];

/// The water at the bottom of a well's shaft.
///
/// A deep, dark teal. It is what makes the mark read as a *well* rather than as
/// a drum or a tower: a dark disc recessed inside the stone says there is a
/// hole here, and a hole with water in it is the whole picture. Kept far from
/// every banner for [`WELL_COLOR`]'s reason -- 306.9 from its nearest, the
/// furthest of anything drawn here.
pub const WELL_WATER_COLOR: MapColor = [0x24, 0x42, 0x4a, 255];

/// The timber of a well's canopy: two posts and the beam across them.
///
/// Dark enough to read against the stone below it at a glance, and the same
/// family of browns the settlement's own trim and tree trunks are painted in --
/// this is a thing that was *built*, by the same hands as the houses around it.
pub const WELL_TIMBER_COLOR: MapColor = [0x6b, 0x4a, 0x32, 255];

/// The share of a square's depth the town's name is painted across.
///
/// `build::wayfinding::square_label` sizes the lettering at up to this fraction
/// of the square, centred on it. A wellhead is placed clear of that band, so the
/// number has to be known on both sides -- and `build` is server-only while this
/// module compiles to both targets, so it cannot simply be imported.
///
/// The duplication is pinned rather than trusted: a test in `build::wayfinding`
/// asserts that a real label fits inside this share, so raising it there fails
/// here instead of quietly painting a name under a well.
pub const SQUARE_LABEL_SHARE: f32 = 0.26;

/// The ground a mark covers, for a renderer that wants a rectangle.
///
/// Both marks are drawn as round things standing on the ground, and the engine
/// builds cylinders rather than sprites -- but the *placement* checks below are
/// about whether two marks collide, which is a question about area. Kept here
/// so the arithmetic is testable without a renderer.
pub fn footprint(center: [f32; 2], radius: f32) -> MapRect {
    MapRect {
        x: center[0] - radius,
        y: center[1] - radius,
        width: radius * 2.0,
        depth: radius * 2.0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::map::{MapLocation, MapManifest, MapPlaza, MapSun, MapUnderside, MapWorld};
    use kingdom_core::{
        AgentNetwork, CityId, CityWells, Isolation, KingdomNetwork, PlanId, SharedService,
    };

    /// How far across a square is, matching `build::scene::PLAZA_SIZE`.
    ///
    /// The real number rather than a round one, because most of what is checked
    /// below is whether a well fits on the paving -- and a test run against a
    /// roomier square than the map actually builds would pass while the picture
    /// spilled onto the grass.
    const SQUARE: f32 = 52.0;

    /// How far a test square stands from the middle of its town.
    ///
    /// **Deliberately not zero.** A square is walked outward from the
    /// settlement's centre until it finds unclaimed ground
    /// (`build::streets::square_site`), and on a real kingdom of seven towns it
    /// landed between 94 and 1,622 units out. A fixture that put the square at
    /// the town's centre would let the old bug -- a well standing at
    /// `town.center` -- pass every test here.
    const SQUARE_OFFSET: f32 = 140.0;

    /// A disk with a square rim, big enough that the inset band is clear of the
    /// towns standing inside it.
    fn a_map(towns: &[(&str, [f32; 2])]) -> MapManifest {
        MapManifest {
            title: String::new(),
            subtitle: String::new(),
            world: MapWorld {
                bounds: MapRect {
                    x: -500.0,
                    y: -500.0,
                    width: 1000.0,
                    depth: 1000.0,
                },
                space: [0, 0, 0, 255],
                ground: [0, 0, 0, 255],
                rim: vec![
                    [-500.0, -500.0],
                    [500.0, -500.0],
                    [500.0, 500.0],
                    [-500.0, 500.0],
                ],
                underside: MapUnderside {
                    cliff: 0.0,
                    shelf: 0.0,
                    taper: 0.0,
                    depth: 0.0,
                    cliff_color: [0, 0, 0, 255],
                    rock: [0, 0, 0, 255],
                    deep: [0, 0, 0, 255],
                },
                sun: MapSun {
                    direction: [0.0, -1.0, 0.0],
                    color: [255, 255, 255, 255],
                    illuminance: 1.0,
                    ambient: [255, 255, 255, 255],
                    ambient_brightness: 1.0,
                },
                towns: Vec::new(),
                wards: Vec::new(),
                // Every town gets a square, offset from its centre the way a
                // real one is -- see `SQUARE_OFFSET`.
                plazas: towns
                    .iter()
                    .map(|(name, center)| MapPlaza {
                        town: (*name).to_owned(),
                        rect: a_square(*center),
                        color: [0, 0, 0, 255],
                    })
                    .collect(),
                roads: Vec::new(),
                buildings: Vec::new(),
                scenery: Vec::new(),
                ground_labels: Vec::new(),
            },
            districts: Vec::new(),
            locations: towns
                .iter()
                .enumerate()
                .map(|(index, (name, center))| MapLocation {
                    id: format!("town-{index}"),
                    label: (*name).to_owned(),
                    detail: String::new(),
                    center: *center,
                    extent: [200.0, 200.0],
                })
                .collect(),
            features: Vec::new(),
        }
    }

    /// Where a town's square stands, given the town's centre.
    fn a_square(town_center: [f32; 2]) -> MapRect {
        MapRect {
            x: town_center[0] + SQUARE_OFFSET,
            y: town_center[1] + SQUARE_OFFSET,
            width: SQUARE,
            depth: SQUARE,
        }
    }

    fn an_agent(plan: &str, city: &str, network: Isolation, drawing: &[&str]) -> AgentNetwork {
        AgentNetwork {
            plan: PlanId::new(plan),
            title: format!("Plan {plan}"),
            city: CityId::new(city),
            network,
            ports: Vec::new(),
            drawing_from: drawing.iter().map(|s| (*s).to_string()).collect(),
        }
    }

    fn a_well(city: &str, names: &[&str]) -> CityWells {
        CityWells {
            city: CityId::new(city),
            wells: names
                .iter()
                .map(|name| SharedService {
                    name: (*name).to_string(),
                    image: "mongo:7".into(),
                    address: "172.31.4.10:27017".into(),
                    users: 2,
                    // A wellhead stands on a town's square, so everything the
                    // map draws is a city's own. A machine-wide well belongs to
                    // no town and is not fed to the map at all -- see the
                    // filter in `api::kingdom_network`.
                    scope: kingdom_core::ServiceScope::City,
                    manifest_path: format!("/dev/{city}/.kingdom/services.toml"),
                })
                .collect(),
        }
    }

    /// **The fact this whole feature exists to draw.**
    ///
    /// An agent with a network of its own does *not* reach the King's machine
    /// -- no conduit to the rim -- but it *does* still reach its city's well.
    /// `slirp4netns` blocks host loopback and nothing else, so a Docker bridge
    /// address is just another route out. Nothing else in the interface says
    /// this, which is why it is drawn.
    #[test]
    fn an_isolated_agent_reaches_the_well_but_not_the_host() {
        let map = a_map(&[("shopfront", [0.0, 0.0])]);
        let network = KingdomNetwork {
            wells: vec![a_well("shopfront", &["db"])],
            agents: vec![an_agent("p1", "shopfront", Isolation::Isolated, &["db"])],
        };

        let picture = resolve(&map, &network);

        assert!(
            !picture
                .links
                .iter()
                .any(|link| link.kind == LinkKind::ToHost),
            "an isolated agent must not be joined to the host ring -- that is \
             the whole picture of isolation"
        );
        assert_eq!(
            picture
                .links
                .iter()
                .filter(|link| link.kind == LinkKind::ToWell)
                .count(),
            1,
            "but it still draws from the city's well, which is the \
             counter-intuitive fact worth drawing"
        );
        assert!(
            picture.agents[0].isolated,
            "and it is marked isolated, so the moat is drawn"
        );
    }

    /// The marker is named, so the plaque over it at the closest tier reads a
    /// title rather than a random plan id -- the engine has no way to look one
    /// up, so if it is not carried here it is not drawable at all.
    #[test]
    fn an_agent_marker_carries_the_plans_name() {
        let map = a_map(&[("orchard", [0.0, 0.0])]);
        let network = KingdomNetwork {
            wells: Vec::new(),
            agents: vec![an_agent("p1", "orchard", Isolation::Shared, &[])],
        };

        let picture = resolve(&map, &network);

        assert_eq!(picture.agents[0].label, "Plan p1");
    }

    /// The other half: a plan on the shared network is joined to the rim.
    #[test]
    fn an_agent_on_the_shared_network_is_joined_to_the_host_ring() {
        let map = a_map(&[("orchard", [0.0, 0.0])]);
        let network = KingdomNetwork {
            wells: Vec::new(),
            agents: vec![an_agent("p1", "orchard", Isolation::Shared, &[])],
        };

        let picture = resolve(&map, &network);

        assert_eq!(
            picture
                .links
                .iter()
                .filter(|link| link.kind == LinkKind::ToHost)
                .count(),
            1,
            "a shared-network agent binds the King's own ports, so it is joined \
             to the ring that stands for his machine"
        );
        assert!(!picture.agents[0].isolated);
    }

    /// A channel is drawn to a well the agent actually draws from, and to no
    /// other. A city's well being *available* is not the same as a plan having
    /// reached for it -- see `AgentNetwork::drawing_from`.
    #[test]
    fn a_channel_is_drawn_only_to_the_well_actually_drawn_from() {
        let map = a_map(&[("shopfront", [0.0, 0.0])]);
        let network = KingdomNetwork {
            wells: vec![a_well("shopfront", &["db", "cache"])],
            // Registered against the database only, though both are standing.
            agents: vec![an_agent("p1", "shopfront", Isolation::Shared, &["db"])],
        };

        let picture = resolve(&map, &network);

        assert_eq!(picture.wells.len(), 2, "both wells stand in the town");
        assert_eq!(
            picture
                .links
                .iter()
                .filter(|link| link.kind == LinkKind::ToWell)
                .count(),
            1,
            "but only one channel: an available well is not a used one"
        );
    }

    /// A name that resolves to no standing well is dropped rather than drawn to
    /// nowhere.
    #[test]
    fn a_well_that_is_not_standing_gets_no_channel() {
        let map = a_map(&[("shopfront", [0.0, 0.0])]);
        let network = KingdomNetwork {
            wells: Vec::new(),
            agents: vec![an_agent("p1", "shopfront", Isolation::Shared, &["db"])],
        };

        let picture = resolve(&map, &network);
        assert!(picture.wells.is_empty());
        assert!(
            !picture
                .links
                .iter()
                .any(|link| link.kind == LinkKind::ToWell),
            "a channel to a well that is not up would point at bare ground"
        );
    }

    /// **A wellhead stands on its town's square.**
    ///
    /// The fault the King saw and the reason for this work. `well_stand` put
    /// the drum at `town.center`, which is not the square at all: a square is
    /// walked outward until it finds ground no ward has claimed, so on the
    /// live map of seven towns it stood between 94 and 1,622 units from the
    /// centre -- against a square 52 across. Every well was therefore standing
    /// among the houses, hundreds of units from the paving.
    ///
    /// Checked as a *footprint inside a rectangle* rather than as a distance
    /// between two centres, because the whole drum has to be on the stone: a
    /// centre on the paving with the rim overhanging the kerb is the same
    /// picture, only smaller.
    #[test]
    fn a_wellhead_stands_on_the_paving_of_its_towns_square() {
        let map = a_map(&[("shopfront", [0.0, 0.0])]);
        let square = map.square_of("shopfront").expect("the town has a square");
        let picture = resolve(
            &map,
            &KingdomNetwork {
                wells: vec![a_well("shopfront", &["db"])],
                agents: Vec::new(),
            },
        );

        let well = &picture.wells[0];
        let ground = footprint(well.center, well.radius);
        assert!(
            square.contains([ground.x, ground.y])
                && square.contains([ground.max_x(), ground.max_y()]),
            "a well covering {ground:?} hangs off a square of {square:?} -- it is \
             standing in the settlement rather than on its paving"
        );
    }

    /// The well keeps clear of the name painted across the square.
    ///
    /// `wayfinding::square_label` paints the town's name across the middle of
    /// the paving at a cap height of up to 0.26 of the square. A well dropped
    /// on the centre would sit on the lettering, and the map would have two
    /// things fighting for one patch of stone.
    ///
    /// The *rear* edge is where it goes, which is the half this checks: the
    /// camera looks down `(-1, -1, -1)`, so low `y` projects up-screen and a
    /// well at the back stands above the name rather than in front of it.
    #[test]
    fn a_wellhead_keeps_off_the_name_painted_on_the_square() {
        let map = a_map(&[("shopfront", [0.0, 0.0])]);
        let square = map.square_of("shopfront").expect("the town has a square");
        let picture = resolve(
            &map,
            &KingdomNetwork {
                wells: vec![a_well("shopfront", &["db"])],
                agents: Vec::new(),
            },
        );

        // The band the lettering occupies: centred, and as tall as the largest
        // cap `square_label` will paint. `SQUARE_LABEL_SHARE` is the number
        // both sides work from, so this is the placement's own rule read back
        // rather than a second guess at it.
        let band = square.depth * SQUARE_LABEL_SHARE;
        let name_starts = square.y + (square.depth - band) * 0.5;

        let well = &picture.wells[0];
        let ground = footprint(well.center, well.radius);
        assert!(
            ground.max_y() <= name_starts,
            "a well reaching {:.1} runs into the lettering, which starts at \
             {name_starts:.1}",
            ground.max_y()
        );
    }

    /// Three services in one city stay on the paving and out of each other.
    ///
    /// The case that decides whether the radius has to give: a 52-unit square
    /// cannot hold three wells at the full 8.5, so `well_stand` shrinks them to
    /// a share of the span. The alternative -- a fixed size and a row that
    /// spills onto the grass -- is the fault this whole change is fixing,
    /// reintroduced by the back door.
    #[test]
    fn several_wells_share_one_square_without_leaving_it() {
        let map = a_map(&[("shopfront", [0.0, 0.0])]);
        let square = map.square_of("shopfront").expect("the town has a square");
        let picture = resolve(
            &map,
            &KingdomNetwork {
                wells: vec![a_well("shopfront", &["db", "cache", "queue"])],
                agents: Vec::new(),
            },
        );

        assert_eq!(picture.wells.len(), 3);
        for well in &picture.wells {
            let ground = footprint(well.center, well.radius);
            assert!(
                square.contains([ground.x, ground.y])
                    && square.contains([ground.max_x(), ground.max_y()]),
                "`{}` covers {ground:?}, off a square of {square:?}",
                well.name
            );
        }

        for (index, a) in picture.wells.iter().enumerate() {
            for b in picture.wells.iter().skip(index + 1) {
                let dx = a.center[0] - b.center[0];
                let dy = a.center[1] - b.center[1];
                let apart = (dx * dx + dy * dy).sqrt();
                assert!(
                    apart >= a.radius + b.radius,
                    "`{}` and `{}` stand {apart:.1} apart but need {:.1}",
                    a.name,
                    b.name,
                    a.radius + b.radius
                );
            }
        }
    }

    /// A town the map gave no square gets no wellhead.
    ///
    /// The rule this module has documented since it was written and did not
    /// keep: a settlement with no wards and no loose files is given no square
    /// (`build::streets::settlement_roads` returns `None`), and there is
    /// nowhere for a well to stand that would not be somebody's front garden.
    /// The old code never looked at the squares at all, so it drew one anyway.
    #[test]
    fn a_town_with_no_square_gets_no_wellhead() {
        let mut map = a_map(&[("shopfront", [0.0, 0.0])]);
        map.world.plazas.clear();

        let picture = resolve(
            &map,
            &KingdomNetwork {
                wells: vec![a_well("shopfront", &["db"])],
                agents: vec![an_agent("p1", "shopfront", Isolation::Shared, &["db"])],
            },
        );

        assert!(
            picture.wells.is_empty(),
            "no square, no wellhead -- the rule this module states"
        );
        assert!(
            !picture
                .links
                .iter()
                .any(|link| link.kind == LinkKind::ToWell),
            "and no channel to a well that is not drawn"
        );
    }

    /// A city the map never drew -- an empty project is dropped from the
    /// manifest entirely -- must be an absence, not a marker on someone else's
    /// ground.
    #[test]
    fn an_agent_in_a_town_the_map_never_drew_is_skipped() {
        let map = a_map(&[("orchard", [0.0, 0.0])]);
        let network = KingdomNetwork {
            wells: vec![a_well("ghost-town", &["db"])],
            agents: vec![an_agent("p1", "ghost-town", Isolation::Shared, &["db"])],
        };

        let picture = resolve(&map, &network);
        assert!(picture.agents.is_empty(), "no town, no marker");
        assert!(picture.wells.is_empty(), "no town, no wellhead");
    }

    /// Several agents in one town must not stand in each other.
    ///
    /// The same guarantee `works::place_fresh` gives a ghost house, and it
    /// matters for the same reason: two markers on one spot is one agent drawn
    /// twice as far as the King can tell.
    #[test]
    fn agents_in_one_town_do_not_stand_on_each_other() {
        let map = a_map(&[("shopfront", [0.0, 0.0])]);
        let network = KingdomNetwork {
            wells: Vec::new(),
            agents: (1..=5)
                .map(|n| an_agent(&format!("p{n}"), "shopfront", Isolation::Shared, &[]))
                .collect(),
        };

        let picture = resolve(&map, &network);
        assert_eq!(picture.agents.len(), 5);

        for (i, a) in picture.agents.iter().enumerate() {
            for b in picture.agents.iter().skip(i + 1) {
                let dx = a.center[0] - b.center[0];
                let dy = a.center[1] - b.center[1];
                let apart = (dx * dx + dy * dy).sqrt();
                assert!(
                    apart >= a.radius + b.radius,
                    "two agents stand {apart:.1} apart but need {:.1}",
                    a.radius + b.radius
                );
            }
        }
    }

    /// An agent must stand *outside* the settlement it belongs to.
    ///
    /// The regression the first render caught. Markers orbited a fixed radius
    /// from the town's centre, which put them down among the buildings -- one
    /// stood on the keep in the middle of the square. An agent is not a
    /// building, and a mark inside the skyline reads as one more thing that was
    /// built there.
    ///
    /// Checked against the town's own extent rather than a constant, because
    /// that is what the placement is now derived from: a big project and a tiny
    /// one must both push their agents clear.
    #[test]
    fn an_agents_mark_stands_clear_of_the_town_it_belongs_to() {
        let map = a_map(&[("shopfront", [0.0, 0.0])]);
        let town = map.town_named("shopfront").unwrap();
        let half = town.extent[0].max(town.extent[1]) * 0.5;

        let network = KingdomNetwork {
            wells: Vec::new(),
            agents: (1..=6)
                .map(|n| an_agent(&format!("p{n}"), "shopfront", Isolation::Shared, &[]))
                .collect(),
        };

        for mark in resolve(&map, &network).agents {
            let dx = mark.center[0] - town.center[0];
            let dy = mark.center[1] - town.center[1];
            let out = (dx * dx + dy * dy).sqrt();
            assert!(
                out - mark.radius > half,
                "an agent stands {out:.1} from the middle of a town whose own \
                 half-span is {half:.1} -- it is inside the settlement, among \
                 the buildings"
            );
        }
    }

    /// The placement must not reshuffle between refetches.
    ///
    /// This resolves every few seconds while the King watches. A marker that
    /// hopped around its town on each poll would read as a bug rather than as a
    /// fact -- the trap `works::place_fresh` seeds against.
    #[test]
    fn the_same_picture_resolves_the_same_way_twice() {
        let map = a_map(&[("shopfront", [0.0, 0.0]), ("orchard", [300.0, 120.0])]);
        let network = KingdomNetwork {
            wells: vec![a_well("shopfront", &["db"])],
            agents: vec![
                an_agent("p1", "shopfront", Isolation::Isolated, &["db"]),
                an_agent("p2", "shopfront", Isolation::Shared, &[]),
                an_agent("p3", "orchard", Isolation::Shared, &[]),
            ],
        };

        assert_eq!(
            resolve(&map, &network),
            resolve(&map, &network),
            "the picture must be stable, or markers hop while the King watches"
        );
    }

    /// An agent's marker is the same colour as the columns it raises.
    ///
    /// The rail's chip, the works band and this marker all come from
    /// `assign_banners`, and the map disagreeing with itself about whose colour
    /// is whose is precisely the confusion the banners exist to remove.
    #[test]
    fn an_agents_marker_is_its_own_banner_colour() {
        let map = a_map(&[("orchard", [0.0, 0.0])]);
        let agents = vec![an_agent("p1", "orchard", Isolation::Shared, &[])];
        let network = KingdomNetwork {
            wells: Vec::new(),
            agents: agents.clone(),
        };

        let picture = resolve(&map, &network);
        let expected = kingdom_core::palette::assign_banners(&[PlanId::new("p1")]);
        let [r, g, b] = expected[0].1.growth_rgb;

        assert_eq!(
            picture.agents[0].color,
            [r, g, b, 255],
            "the marker must be the colour the rail and the works use"
        );
    }

    /// A well must never be mistaken for an agent.
    ///
    /// **Distance, not inequality.** An earlier version of this test asserted
    /// only that the well's colour was not *equal* to any banner, and it passed
    /// a well that was visibly the same cyan as the `azure` agent standing
    /// beside it -- 110.5 apart, where the two closest banners are 126.1.
    ///
    /// So the bar is the palette's own: a well must be at least as far from
    /// every banner as the nearest two banners are from each other. Anything
    /// less and the map has two things that mean different things and look the
    /// same.
    ///
    /// The ruler is the weighted-RGB approximation `palette`'s own hue search
    /// was run against -- not a colour-science claim, but one consistent ruler,
    /// which is what a regression test needs.
    #[test]
    fn a_well_is_no_closer_to_an_agent_than_agents_are_to_each_other() {
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
            .flat_map(|(i, a)| {
                banners[i + 1..]
                    .iter()
                    .map(move |b| distance(a.growth_rgb, b.growth_rgb))
            })
            .fold(f64::MAX, f64::min);

        let well = [WELL_COLOR[0], WELL_COLOR[1], WELL_COLOR[2]];
        for banner in banners.iter() {
            let apart = distance(well, banner.growth_rgb);
            assert!(
                apart >= closest_pair,
                "a wellhead is {apart:.1} from the `{}` banner, closer than the \
                 two nearest agents are to each other ({closest_pair:.1}) -- a \
                 well and an agent would read as the same thing",
                banner.name
            );
        }
    }

    /// The host ring stands for the King's machine rather than for any agent, so
    /// it must not be confusable with one either.
    #[test]
    fn the_host_ring_is_not_the_colour_of_any_agent() {
        for banner in kingdom_core::palette::BANNERS.iter() {
            let [r, g, b] = banner.growth_rgb;
            assert_ne!(
                HOST_COLOR,
                [r, g, b, 255],
                "the host ring wears an agent's banner"
            );
        }
    }

    /// The host band stands inside the rim, in the fringe the disk already
    /// leaves empty -- so nothing has to move to make room for it.
    #[test]
    fn the_host_ring_stands_inside_the_rim() {
        let map = a_map(&[("orchard", [0.0, 0.0])]);
        let picture = resolve(
            &map,
            &KingdomNetwork {
                wells: Vec::new(),
                agents: Vec::new(),
            },
        );

        let ring = picture.host.expect("a world with a rim has a host ring");
        assert_eq!(ring.path.len(), map.world.rim.len());

        for (drawn, rim) in ring.path.iter().zip(map.world.rim.iter()) {
            let from_centre = (drawn[0] * drawn[0] + drawn[1] * drawn[1]).sqrt();
            let rim_distance = (rim[0] * rim[0] + rim[1] * rim[1]).sqrt();
            assert!(
                from_centre < rim_distance,
                "the band must sit inside the world's edge, not on it"
            );
        }
    }

    /// A world with no rim has no host ring, rather than a degenerate one.
    #[test]
    fn a_world_with_no_rim_draws_no_host_ring() {
        let mut map = a_map(&[("orchard", [0.0, 0.0])]);
        map.world.rim.clear();

        let picture = resolve(
            &map,
            &KingdomNetwork {
                wells: Vec::new(),
                agents: vec![an_agent("p1", "orchard", Isolation::Shared, &[])],
            },
        );

        assert!(picture.host.is_none());
        assert!(
            !picture
                .links
                .iter()
                .any(|link| link.kind == LinkKind::ToHost),
            "with no ring to join, a conduit would run to nowhere"
        );
    }

    /// Nothing open and nothing declared is the ordinary state of a dev folder,
    /// and it is what lets the renderer skip its pass entirely.
    #[test]
    fn an_empty_kingdom_is_quiet_apart_from_the_host() {
        let mut map = a_map(&[]);
        map.world.rim.clear();

        let picture = resolve(&map, &KingdomNetwork::default());
        assert!(picture.is_quiet());
    }
}
