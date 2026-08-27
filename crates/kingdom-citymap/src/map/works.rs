//! What a plan proposes, as ground on the map.
//!
//! The King opens a chamber and the map shows him what his agent is building:
//! a house that gained lines wears a scaffold above its roof, one that lost them
//! is covered by a shroud rising from the ground over as much of the house as
//! the file is losing, and a file that did not exist an hour ago stands as a
//! ghost on free land inside the folder it belongs to.
//!
//! # Why this is in `map` and not in `engine`
//!
//! For the reason the whole module is on both targets: this is the *wire shape*
//! between the interface and the renderer, and none of it is drawing. The
//! placement below is the fiddliest arithmetic in the feature and it is
//! deliberately pure -- `cargo test` builds this crate with no features at all,
//! so a ghost house that lands on top of a real one is a test failure on a bare
//! machine rather than something spotted by eye in a browser, once, if the right
//! folder happened to be looked at.
//!
//! # Why the manifest does not carry this
//!
//! `kingdom_app::citymap` memoises the map JSON -- seconds of filesystem work,
//! megabytes of geometry -- keyed on the kingdom root and its city names, and
//! deliberately not on anything that moves. What a plan has changed moves every
//! few seconds. So the works travel the way activity does, as a
//! [`ViewerCommand::SetWorks`](crate::engine::bridge::ViewerCommand::SetWorks),
//! and the manifest is untouched. [`crate::engine::activity`] records the same
//! reasoning for the working ring.

use super::{MapRect, MapWard};
use serde::{Deserialize, Serialize};

/// One file a plan has changed, placed on the map.
///
/// A file rather than a file-and-a-plan: several agents may be in the same file
/// at once, and drawing that as several works would put two columns on one
/// house and say the file was two files. So one work carries one
/// [`WorkBand`] per agent -- see [`resolve`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Work {
    /// The ground this work stands on.
    pub site: WorkSite,
    /// Who is in this file, and how much each of them moved.
    ///
    /// Never empty -- a work with no bands would be a house marked as worked on
    /// by nobody -- and in a stable order, so the stack does not reshuffle
    /// between refetches.
    pub bands: Vec<WorkBand>,
}

impl Work {
    /// How much moved here in total, across every agent.
    ///
    /// What the column as a whole is sized from, so that one agent changing
    /// forty lines and two agents changing twenty each stand equally tall.
    pub fn churn(&self) -> f32 {
        self.bands.iter().map(WorkBand::churn).sum()
    }

    /// Whether more than one agent has hands on this file.
    ///
    /// The contention question from `AGENTS.md` -- question two -- answered at
    /// the only place that knows it.
    pub fn is_contended(&self) -> bool {
        self.bands.len() > 1
    }
}

/// One agent's share of one file.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkBand {
    /// This agent's colour for lines added.
    pub growth: super::MapColor,
    /// This agent's colour for lines removed: the same hue, deepened.
    pub cutting: super::MapColor,
    /// Lines added, as an absolute count.
    ///
    /// **Absolute, not a share.** An earlier version carried `scale`, a
    /// fraction of the busiest file in the same plan, and it made two agents
    /// incomparable: each was measured against its own plan's ruler, so the
    /// same forty-line edit stood at full height in one agent's stack and at a
    /// tenth in another's. It also made a lone change unreadable -- see
    /// `engine::works::band_height`, which records what that cost.
    pub added: f32,
    /// Lines removed, as an absolute count. Absolute for [`Self::added`]'s
    /// reason.
    pub removed: f32,
    /// How much of the house this agent's cutting covers, as `0.0..=1.0`.
    ///
    /// **A share, unlike every other number here, and deliberately so.** The
    /// counts above are absolute because two agents' columns have to be
    /// comparable across the whole map; this one answers a different question --
    /// *how much of this file is going away* -- and that is a ratio or it is
    /// nothing. Three hundred lines cut is most of a four-hundred-line file and
    /// a rounding error in a twenty-thousand-line one, and the King asked for
    /// exactly that distinction: half the file removed covers half the house.
    ///
    /// Computed in [`resolve`] rather than by the renderer, because the
    /// denominator is [`MapFeature::lines`](super::MapFeature::lines) and the
    /// engine is deliberately ignorant of anything but rectangles and heights.
    /// See the module docs on where that seam is.
    pub cover: f32,
    /// Whether this agent deleted the file outright.
    ///
    /// Drawn as a shroud over the whole house rather than as a column: a
    /// deletion is a house being covered over entirely, and anything rising
    /// above the roofline would say the opposite. It is simply [`Self::cover`]
    /// at `1.0` -- kept as its own flag because "the file is gone" is a fact
    /// about the change rather than a measurement of it. See `engine::works`.
    pub razing: bool,
}

impl WorkBand {
    /// Everything that moved in this band.
    pub fn churn(&self) -> f32 {
        self.added + self.removed
    }
}

/// Where a piece of work stands.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum WorkSite {
    /// A house that is already on the map, being worked on.
    Standing {
        /// The ground the existing building covers.
        footprint: MapRect,
        /// How tall it already stands, so the scaffold starts at its roof.
        height: f32,
    },
    /// A file that does not exist in the city's checkout yet, given ground of
    /// its own by [`place_fresh`].
    Fresh {
        /// The ground the ghost house covers.
        footprint: MapRect,
    },
}

impl WorkSite {
    /// The ground covered, whichever kind of site this is.
    pub fn footprint(self) -> MapRect {
        match self {
            Self::Standing { footprint, .. } | Self::Fresh { footprint } => footprint,
        }
    }

    /// How far off the ground the work starts.
    pub fn base(self) -> f32 {
        match self {
            Self::Standing { height, .. } => height,
            // A ghost house *is* the building, so it rises from the ground.
            Self::Fresh { .. } => 0.0,
        }
    }
}

/// The angle the search turns by at each step, in radians.
///
/// The golden angle, which is what `build::layout` already spaces a sunflower
/// spiral of towns by and what `build::scenery` scatters trees with. Reused
/// deliberately rather than reinvented: it is the arrangement that does not
/// fall into rings or spokes however many points are taken, which is exactly
/// what a search for a free spot wants.
const GOLDEN_ANGLE: f32 = 2.399_963_2;

/// How many spots are tried before a folder is called full.
///
/// Generous, because each attempt is a handful of rectangle overlaps against a
/// ward's own holdings -- tens of them, not the world's thousands -- and this
/// runs once per new file when a chamber is opened. A dense ward may genuinely
/// need most of these.
const ATTEMPTS: usize = 240;

/// How much a ghost house shrinks each time the search fails a full sweep.
///
/// A new file in a packed folder should still appear, even if it has to be
/// small to fit. Three sweeps at decreasing sizes, and only then is the ward
/// genuinely full.
const SHRINK: f32 = 0.62;

/// How many times it may shrink.
const SHRINKS: usize = 3;

/// Finds free ground inside a ward for a house that is not on the map yet.
///
/// `taken` is every plot already spoken for -- both the ward's own holdings
/// ([`MapManifest::lots_in`](super::MapManifest::lots_in)) and the ground its
/// nested folders stand on
/// ([`MapManifest::wards_inside`](super::MapManifest::wards_inside)) -- because
/// a ghost dropped on a sub-folder's ground would look like it belonged to that
/// folder rather than to this one.
///
/// `seed` fixes the answer: the same file lands in the same place every time the
/// review is refetched, which happens on every transcript entry. Without it a
/// new house would hop around the folder while the King watched, which reads as
/// a bug rather than as a plan. This is the guarantee `MapBuilding::seed` gives
/// a real holding, for the same reason.
///
/// Returns `None` only when the folder is genuinely full at every size tried.
/// The caller decides what to do about it; there is no good silent answer, and
/// stacking two houses on one lot is not one.
pub fn place_fresh(
    ward: &MapWard,
    taken: &[MapRect],
    seed: u32,
    want: [f32; 2],
) -> Option<MapRect> {
    // The ward's own edge is kept clear so a ghost never straddles the kerb its
    // folder is drawn with.
    let margin = (ward.rect.width.min(ward.rect.depth) * 0.06).clamp(0.5, 6.0);
    let inner = MapRect {
        x: ward.rect.x + margin,
        y: ward.rect.y + margin,
        width: ward.rect.width - margin * 2.0,
        depth: ward.rect.depth - margin * 2.0,
    };
    if inner.width <= 0.0 || inner.depth <= 0.0 {
        return None;
    }

    let center = inner.center();
    // The spiral is walked out to the corner of the ward, so a sweep covers all
    // of it rather than an inscribed circle that misses four corners.
    let reach = (inner.width.max(inner.depth)) * 0.5;

    let mut size = [
        want[0].max(0.01).min(inner.width),
        want[1].max(0.01).min(inner.depth),
    ];

    for _ in 0..=SHRINKS {
        // The first point tried is `radius == 0`, the middle of the ward: an
        // empty folder puts its one new house in the centre, which is where a
        // person would put it.
        for attempt in 0..ATTEMPTS {
            let step = attempt as f32;
            let angle = step * GOLDEN_ANGLE + (seed % 360) as f32 * 0.017_453_3;
            // `sqrt` spacing, which is what keeps a sunflower's points evenly
            // *dense* rather than crowding the middle.
            let radius = reach * (step / ATTEMPTS as f32).sqrt();
            let candidate = MapRect {
                x: center[0] + angle.cos() * radius - size[0] * 0.5,
                y: center[1] + angle.sin() * radius - size[1] * 0.5,
                width: size[0],
                depth: size[1],
            };

            if !contains_rect(&inner, &candidate) {
                continue;
            }
            if taken.iter().any(|plot| overlaps(plot, &candidate)) {
                continue;
            }
            return Some(candidate);
        }
        size = [size[0] * SHRINK, size[1] * SHRINK];
    }

    None
}

/// Whether `inner` lies wholly within `outer`, edges included.
fn contains_rect(outer: &MapRect, inner: &MapRect) -> bool {
    inner.x >= outer.x
        && inner.y >= outer.y
        && inner.max_x() <= outer.max_x()
        && inner.max_y() <= outer.max_y()
}

/// Whether two plots share any ground. Touching edges do not count as overlap,
/// which is what lets a house sit flush against its neighbour's lot line.
fn overlaps(a: &MapRect, b: &MapRect) -> bool {
    a.x < b.max_x() && b.x < a.max_x() && a.y < b.max_y() && b.y < a.max_y()
}

/// Turning what a plan changed into ground on the map.
///
/// Gated exactly as [`crate::engine`] is, and for the same two reasons: it is
/// only ever called from the browser, and `kingdom-core` is a *dev*-dependency
/// of this crate on native -- so this compiles under `cargo test`, where the
/// judgements below can be pinned without a browser, and under `hydrate`, where
/// it actually runs. It must never reach the Axum binary; the server has no
/// plan open and nothing to resolve.
#[cfg(any(feature = "hydrate", test))]
mod resolve {
    use super::{Work, WorkBand, WorkSite, place_fresh};
    use crate::map::{MapManifest, MapRect, MapWard};
    use kingdom_core::{ChangeKind, ChangeSummary, PlanId};
    use std::collections::{BTreeMap, HashMap};

    /// A banner colour, opaque, as the map's own colour type.
    ///
    /// The translucency the works are drawn with is `engine::works`'s business
    /// -- it varies with what is being drawn, and a ghost is fainter than a
    /// standing house -- so the wire carries the colour at full strength and
    /// the renderer decides how solid a proposal looks.
    fn with_alpha(rgb: kingdom_core::palette::Rgb) -> crate::map::MapColor {
        [rgb[0], rgb[1], rgb[2], 255]
    }

    /// Turns what every agent in a city is changing into ground on the map.
    ///
    /// The whole of the domain-to-geometry translation. Everything above this
    /// is Kingdom's domain -- paths, line counts, a `ChangeSummary`, a plan's
    /// banner -- and everything below it is world-space geometry, which is what
    /// keeps the engine ignorant of what a plan is.
    ///
    /// # Why it takes many plans rather than one
    ///
    /// Because the question is "who is touching this file", and one plan's
    /// summary structurally cannot answer it. Taking the whole city's work at
    /// once is also what lets a file two agents share be drawn as **one** house
    /// with two bands rather than as two competing columns -- the grouping
    /// below is by path, and the plan is what a band is, not what a work is.
    ///
    /// Every omission it makes is a judgement about what is *honest* to draw:
    ///
    /// - **Binary files are left out.** `ChangedFile::binary` exists because
    ///   `+0 -0` reads as "unchanged" for a file that certainly did change. Its
    ///   numbers are not line counts, and a column built from them would be an
    ///   invented figure standing over a real house.
    /// - **A file with no house and no ward is skipped.** An empty project is
    ///   dropped from the manifest entirely (`build::manifest_for`), and a
    ///   folder the layout merged away has no ward to stand a ghost in. Both are
    ///   the ordinary staleness `kingdom_app::citymap` documents as a deliberate
    ///   trade -- the map follows the shape of a codebase, not its contents.
    ///
    /// **Deletions are no longer left out**, which they were until this took
    /// several plans. The old reasoning was sound as far as it went -- the map
    /// is drawn from the city's checkout, where a deleted file's house is still
    /// standing, so a scaffold on it would say the opposite of what happened --
    /// but the conclusion was wrong: the answer is to draw the *opposite of a
    /// scaffold*, not nothing at all. A razing band is carried here and
    /// `engine::works` draws it as a house covered over entirely, which is
    /// [`WorkBand::cover`] at 1.0 rather than a mark of its own. A
    /// deletion-only plan used to resolve to an empty list and leave the map
    /// blank while an agent was working hard.
    ///
    /// Ghost houses are placed once per **path**, not once per plan, so two
    /// agents creating the same file get one house with two bands rather than
    /// two houses in different corners of the folder. Each is added to the
    /// ground the next must avoid, so two genuinely different new files never
    /// stand in each other. Placement follows the sorted path order, so it is
    /// stable across refetches and the houses do not shuffle.
    pub fn resolve(
        map: &MapManifest,
        city: &str,
        working: &[(PlanId, ChangeSummary)],
    ) -> Vec<Work> {
        // Everything worth drawing, gathered by path so that a file several
        // agents are in becomes one entry with several bands. `BTreeMap` rather
        // than `HashMap` because the iteration order *is* the placement order
        // for ghost houses, and a hash order would move a new house between
        // refetches -- which reads as a bug rather than as a plan.
        let mut by_path: BTreeMap<&str, Vec<WorkBand>> = BTreeMap::new();

        // Banners are assigned over the **whole set** rather than taken per
        // plan, and that is what guarantees two agents on one house are two
        // colours. `palette::preferred` alone is stable but may collide, and a
        // collision here would draw exactly the picture this feature exists to
        // prevent. See `kingdom_core::palette::assign_banners`.
        let banners = kingdom_core::palette::assign_banners(
            &working
                .iter()
                .map(|(plan, _)| plan.clone())
                .collect::<Vec<_>>(),
        );

        for ((plan, summary), (banner_plan, banner)) in working.iter().zip(banners.iter()) {
            // `assign_banners` preserves order, so the two lists are in step.
            // Cheap to state, and it is the sort of coupling that breaks
            // silently by painting one agent in another's colour.
            debug_assert_eq!(plan, banner_plan);
            for file in &summary.files {
                // A binary file's counts are not line counts. Everything else
                // with something to say is drawn, deletions included.
                if file.binary {
                    continue;
                }
                let razing = file.kind == ChangeKind::Deleted;
                if file.churn() == 0 && !razing {
                    // A rename with no content change: nothing moved, and a
                    // column over it would be an invented figure. A deletion
                    // git reports as empty is still a deletion, so it stays.
                    continue;
                }

                by_path
                    .entry(file.path.as_str())
                    .or_default()
                    .push(WorkBand {
                        growth: with_alpha(banner.growth_rgb),
                        cutting: with_alpha(banner.cutting_rgb),
                        added: file.added as f32,
                        removed: file.removed as f32,
                        // Filled in below, once the site is known: the
                        // denominator is the file's own length, and that
                        // arrives with the holding rather than with the change.
                        cover: 0.0,
                        razing,
                    });
            }
        }

        let mut raised = Vec::new();
        // Ground claimed by ghost houses as this pass places them.
        let mut claimed: HashMap<String, Vec<MapRect>> = HashMap::new();

        for (path, mut bands) in by_path {
            // The house's own length as the map last scanned it, which is the
            // denominator `WorkBand::cover` is a share of. Zero stands for "not
            // known" -- a `Fresh` site has no prior file at all, and the scanner
            // leaves `lines` at zero for anything it could not read.
            let mut lines = 0;
            let site = match map.holding_at(city, path) {
                // The house is on the map: the work happens on top of it.
                Some(feature) => {
                    lines = feature.lines;
                    WorkSite::Standing {
                        footprint: feature.footprint,
                        height: feature.height,
                    }
                }
                // No house. Either an agent created this file or the map is
                // stale about it, and the two are indistinguishable from here --
                // which is fine, because both mean "there is no building for
                // this, so give it ground of its own".
                //
                // A file that only ever existed to be deleted is the one case
                // where that is wrong: raising a ghost for it would build a
                // house to announce that a house is gone. Skipped instead.
                None if bands.iter().all(|band| band.razing) => continue,
                None => {
                    let folder = match path.rfind('/') {
                        Some(at) => &path[..at],
                        None => "",
                    };
                    let Some(ward) = map.ward_at(city, folder) else {
                        continue;
                    };

                    // Everything a newcomer must keep off: the ward's own
                    // holdings, the ground its nested folders stand on, and the
                    // ghosts already placed on this pass.
                    let lots: Vec<MapRect> = map.lots_in(&ward.id).collect();
                    let mut taken = lots.clone();
                    taken.extend(map.wards_inside(&ward.id).map(|inner| inner.rect));
                    if let Some(already) = claimed.get(&ward.id) {
                        taken.extend(already.iter().copied());
                    }

                    // Sized from the *lots* rather than from everything in
                    // `taken`: a sub-folder's ground is many times a house and
                    // would drag the median up until a new file was drawn as a
                    // mansion.
                    let want = typical_lot(&lots, ward);
                    let Some(footprint) = place_fresh(ward, &taken, seed(path), want) else {
                        continue;
                    };
                    claimed.entry(ward.id.clone()).or_default().push(footprint);
                    WorkSite::Fresh { footprint }
                }
            };

            // How much of the house each agent's cutting covers. Done here
            // rather than where the band was built because the denominator is
            // the file's own length, which only the holding knows.
            for band in &mut bands {
                band.cover = cover_of(band, site, lines);
            }

            raised.push(Work { site, bands });
        }

        raised
    }

    /// How much of a house one agent's cutting covers, as `0.0..=1.0`.
    ///
    /// `lines` is the file's own length as the map last scanned it, or zero for
    /// "not known". The whole point of the number is stated on
    /// [`WorkBand::cover`]: half the file removed covers half the house.
    ///
    /// # The three cases, and why each is what it is
    ///
    /// - **A deletion covers everything.** Not a matter of degree, and not a
    ///   matter of what git managed to count: a file reported as `-0` because it
    ///   was empty is still entirely gone.
    /// - **A known length is the honest denominator.** Clamped, because `removed`
    ///   can genuinely exceed it -- the manifest is memoised on the shape of the
    ///   kingdom and is allowed to be stale about a file's contents, and a house
    ///   cannot be more than covered.
    /// - **An unknown length falls back to an absolute ramp.** A holding the
    ///   scanner never read (too large, or not text) has no share to be taken
    ///   of, and drawing nothing would be the invisible-removal fault all over
    ///   again. [`FALLBACK_LINES`] is the length this pretends such a file has:
    ///   a few hundred lines, so a substantial cut still covers a substantial
    ///   part of the house without ever reaching the top -- the shroud stays
    ///   visibly short of a real deletion's.
    /// - **A house with no prior file is a share of its own churn.** A
    ///   [`WorkSite::Fresh`] is either a file an agent created or one the
    ///   manifest is stale about, and neither has a scanned length to divide
    ///   by. What both do have is how much of the work on them is cutting, and
    ///   that is the only honest denominator available. It matters most in the
    ///   stale case, which is a real house being genuinely gutted: against a
    ///   nominal length that would draw as a sliver, and against its own churn
    ///   it draws as most of the house, which is what happened.
    ///
    /// Guarded against a zero denominator throughout, because a NaN here becomes
    /// a degenerate mesh in Bevy -- the trap this module already records twice.
    fn cover_of(band: &WorkBand, site: WorkSite, lines: usize) -> f32 {
        if band.razing {
            return 1.0;
        }
        if band.removed <= 0.0 || !band.removed.is_finite() {
            return 0.0;
        }
        let whole = match site {
            // A house that stands: a share of the file it is, or of a nominal
            // file if the scanner never managed to count it.
            WorkSite::Standing { .. } if lines > 0 => lines as f32,
            WorkSite::Standing { .. } => FALLBACK_LINES,
            // Nothing on disk to be a share of. `churn` is never zero here --
            // `removed` alone is already positive.
            WorkSite::Fresh { .. } => band.churn(),
        };
        (band.removed / whole.max(1.0)).clamp(0.0, 1.0)
    }

    /// The file length a holding of unknown size is measured against.
    ///
    /// Only reached for a house the scanner never counted -- too large for
    /// `MAX_ANALYZED_BYTES`, or not text. A few hundred lines is a large-ish
    /// source file, which keeps such a shroud honestly short of the full-height
    /// one a real deletion earns.
    const FALLBACK_LINES: f32 = 400.0;

    /// How big a new house should be, so it looks like it belongs.
    ///
    /// The median of the houses already on the ward rather than the mean, for
    /// `engine::typical_holding`'s reason: a handful of enormous files would
    /// drag the reference off the size nearly every building actually is.
    fn typical_lot(lots: &[MapRect], ward: &MapWard) -> [f32; 2] {
        if lots.is_empty() {
            // A folder with nothing in it yet: a modest share of its own
            // ground, so the first house is neither a speck nor the whole ward.
            return [ward.rect.width * 0.18, ward.rect.depth * 0.18];
        }
        let mut widths: Vec<f32> = lots.iter().map(|lot| lot.width).collect();
        let mut depths: Vec<f32> = lots.iter().map(|lot| lot.depth).collect();
        widths.sort_by(f32::total_cmp);
        depths.sort_by(f32::total_cmp);
        [widths[widths.len() / 2], depths[depths.len() / 2]]
    }

    /// A stable number for a path, so a ghost house does not move between
    /// refetches -- and the review is refetched on every transcript entry.
    ///
    /// FNV-1a, which is what `build::layout::stable_hash` gives a real holding
    /// its variation from. Copied rather than shared because that function is
    /// server-only and this runs in the browser.
    fn seed(path: &str) -> u32 {
        path.bytes().fold(2_166_136_261u32, |hash, byte| {
            hash.wrapping_mul(16_777_619) ^ byte as u32
        })
    }
}

#[cfg(any(feature = "hydrate", test))]
pub use resolve::resolve;

#[cfg(test)]
mod tests {
    use super::*;

    fn ward(x: f32, y: f32, width: f32, depth: f32) -> MapWard {
        MapWard {
            id: "ward-0".to_owned(),
            name: "src".to_owned(),
            path: "src".to_owned(),
            parent: None,
            files: 3,
            rect: MapRect { x, y, width, depth },
            polygon: Vec::new(),
            depth: 0,
            ground: [0, 0, 0, 255],
            edge: [0, 0, 0, 255],
        }
    }

    pub(super) fn rect(x: f32, y: f32, width: f32, depth: f32) -> MapRect {
        MapRect { x, y, width, depth }
    }

    /// The guarantee the whole placer exists for. A ghost house dropped on top
    /// of a real one would read as the map being wrong about the city, which is
    /// far worse than a new file not being drawn at all.
    #[test]
    fn a_new_house_never_lands_on_an_existing_lot() {
        let ward = ward(0.0, 0.0, 100.0, 100.0);
        // A dense-ish folder: a grid of lots with gaps between them.
        let taken: Vec<MapRect> = (0..5)
            .flat_map(|row| {
                (0..5).map(move |col| rect(col as f32 * 20.0, row as f32 * 20.0, 14.0, 14.0))
            })
            .collect();

        let placed = place_fresh(&ward, &taken, 7, [4.0, 4.0]).expect("a gap this wide exists");
        for plot in &taken {
            assert!(
                !overlaps(plot, &placed),
                "ghost at {placed:?} overlaps {plot:?}"
            );
        }
    }

    /// A house outside its own folder is a house in the wrong folder, which is
    /// the one thing the placement is meant to be saying.
    #[test]
    fn a_new_house_never_leaves_its_ward() {
        let ward = ward(30.0, 40.0, 60.0, 25.0);
        for seed in 0..40 {
            let placed = place_fresh(&ward, &[], seed, [5.0, 5.0]).expect("an empty ward has room");
            assert!(
                placed.x >= ward.rect.x
                    && placed.y >= ward.rect.y
                    && placed.max_x() <= ward.rect.max_x()
                    && placed.max_y() <= ward.rect.max_y(),
                "seed {seed} placed {placed:?} outside {:?}",
                ward.rect
            );
        }
    }

    /// The review is refetched on every transcript entry. A ghost that moved
    /// each time would read as a bug rather than as a plan.
    #[test]
    fn the_same_file_lands_in_the_same_place_every_time() {
        let ward = ward(0.0, 0.0, 80.0, 80.0);
        let taken = vec![rect(10.0, 10.0, 30.0, 30.0)];
        let first = place_fresh(&ward, &taken, 4_242, [6.0, 6.0]);
        let again = place_fresh(&ward, &taken, 4_242, [6.0, 6.0]);
        assert_eq!(first, again);
        assert!(first.is_some());
    }

    /// Two different files should not be drawn standing in each other, so the
    /// seed has to actually separate them.
    #[test]
    fn different_files_are_offered_different_ground() {
        let ward = ward(0.0, 0.0, 80.0, 80.0);
        let first = place_fresh(&ward, &[], 1, [6.0, 6.0]).expect("room");
        let second = place_fresh(&ward, &[first], 2, [6.0, 6.0]).expect("room");
        assert!(!overlaps(&first, &second));
    }

    /// A folder with no room left says so, rather than stacking two houses on
    /// one lot. The caller can then decide, which is better than a silent lie.
    #[test]
    fn a_full_ward_admits_it_has_no_room() {
        let ward = ward(0.0, 0.0, 20.0, 20.0);
        // The whole ward, covered.
        let taken = vec![rect(-5.0, -5.0, 30.0, 30.0)];
        assert_eq!(place_fresh(&ward, &taken, 1, [4.0, 4.0]), None);
    }

    /// A tight folder should still show a new file, small, rather than dropping
    /// it -- which is what the shrink sweeps are for.
    #[test]
    fn a_tight_ward_still_finds_room_by_building_smaller() {
        let ward = ward(0.0, 0.0, 40.0, 40.0);
        // A ring of lots leaving only a small pocket free in the middle.
        let taken = vec![
            rect(0.0, 0.0, 40.0, 16.0),
            rect(0.0, 24.0, 40.0, 16.0),
            rect(0.0, 16.0, 16.0, 8.0),
            rect(26.0, 16.0, 14.0, 8.0),
        ];
        // Asking for something far too big: it has to shrink to fit the pocket.
        let placed = place_fresh(&ward, &taken, 3, [12.0, 12.0]).expect("the pocket is usable");
        for plot in &taken {
            assert!(!overlaps(plot, &placed), "{placed:?} hits {plot:?}");
        }
        assert!(placed.width < 12.0, "it should have built smaller");
    }

    /// An empty folder puts its new house where a person would: in the middle.
    #[test]
    fn the_first_house_in_an_empty_folder_stands_at_its_heart() {
        let ward = ward(0.0, 0.0, 100.0, 100.0);
        let placed = place_fresh(&ward, &[], 0, [10.0, 10.0]).expect("room");
        let center = placed.center();
        assert!((center[0] - 50.0).abs() < 0.001, "{center:?}");
        assert!((center[1] - 50.0).abs() < 0.001, "{center:?}");
    }

    /// A ward smaller than nothing cannot be built in, and must not panic or
    /// return a rectangle with negative extent.
    #[test]
    fn a_ward_with_no_usable_ground_is_refused() {
        assert_eq!(
            place_fresh(&ward(0.0, 0.0, 0.0, 0.0), &[], 1, [1.0, 1.0]),
            None
        );
    }

    /// A site reports its ground and its base whichever kind it is, which is
    /// what the renderer builds from.
    #[test]
    fn a_site_reports_its_ground_and_where_the_work_starts() {
        let standing = WorkSite::Standing {
            footprint: rect(1.0, 2.0, 3.0, 4.0),
            height: 12.0,
        };
        assert_eq!(standing.footprint(), rect(1.0, 2.0, 3.0, 4.0));
        assert_eq!(standing.base(), 12.0);

        let fresh = WorkSite::Fresh {
            footprint: rect(5.0, 6.0, 7.0, 8.0),
        };
        assert_eq!(fresh.footprint(), rect(5.0, 6.0, 7.0, 8.0));
        // A ghost house *is* the building, so it rises from the ground rather
        // than from a roof that does not exist.
        assert_eq!(fresh.base(), 0.0);
    }
}

/// The judgements [`resolve`] makes about what is honest to draw.
#[cfg(test)]
mod resolve_tests {
    use super::tests::rect;
    use super::*;
    use crate::map::{
        BuildingKind, MapBuilding, MapFeature, MapManifest, MapPalette, MapSun, MapTown,
        MapUnderside, MapWorld,
    };
    use kingdom_core::{ChangeKind, ChangeSummary, ChangedFile, Language, PlanId};

    fn summary(files: Vec<ChangedFile>) -> ChangeSummary {
        ChangeSummary {
            base: "main".to_owned(),
            files,
            note: None,
        }
    }

    fn file(path: &str, kind: ChangeKind, added: u32, removed: u32) -> ChangedFile {
        ChangedFile {
            path: path.to_owned(),
            old_path: None,
            kind,
            added,
            removed,
            binary: false,
            language: Language::Rust,
        }
    }

    /// A map of one town with one `src` ward, and whatever houses are asked for.
    ///
    /// The houses have no scanned length, which is the ordinary state for a
    /// file the scanner could not read. Use [`world_of`] where the length
    /// matters -- it is the denominator of `WorkBand::cover`.
    fn world(city: &str, houses: &[(&str, MapRect)]) -> MapManifest {
        let sized: Vec<_> = houses.iter().map(|(path, lot)| (*path, *lot, 0)).collect();
        world_of(city, &sized)
    }

    /// The same, with each house's file length -- what a removal is a share of.
    fn world_of(city: &str, houses: &[(&str, MapRect, usize)]) -> MapManifest {
        let ward = MapWard {
            id: "ward-0".to_owned(),
            name: "src".to_owned(),
            path: "src".to_owned(),
            parent: None,
            files: houses.len() as u32,
            rect: MapRect {
                x: 0.0,
                y: 0.0,
                width: 120.0,
                depth: 120.0,
            },
            polygon: Vec::new(),
            depth: 0,
            ground: [0, 0, 0, 255],
            edge: [0, 0, 0, 255],
        };

        let features = houses
            .iter()
            .map(|(path, lot, lines)| MapFeature {
                id: (*path).to_owned(),
                name: path.rsplit('/').next().unwrap_or(path).to_owned(),
                path: (*path).to_owned(),
                repository: city.to_owned(),
                folder: "src".to_owned(),
                breadcrumb: Vec::new(),
                building_kind: String::new(),
                meaning: String::new(),
                category: String::new(),
                bytes: 0,
                lines: *lines,
                complexity: 0,
                references: 0,
                footprint: *lot,
                height: 9.0,
                center: lot.center(),
            })
            .collect();
        let buildings = houses
            .iter()
            .map(|(path, lot, _)| MapBuilding {
                feature_id: (*path).to_owned(),
                ward_id: Some("ward-0".to_owned()),
                kind: BuildingKind::Guildhall,
                footprint: *lot,
                lot: *lot,
                height: 9.0,
                palette: MapPalette::default(),
                complexity: 0,
                seed: 0,
            })
            .collect();

        MapManifest {
            title: String::new(),
            subtitle: String::new(),
            world: MapWorld {
                bounds: MapRect::default(),
                space: [0, 0, 0, 255],
                ground: [0, 0, 0, 255],
                rim: Vec::new(),
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
                towns: vec![MapTown {
                    id: "town-0".to_owned(),
                    name: city.to_owned(),
                    rect: MapRect::default(),
                    polygon: Vec::new(),
                    ground: [0, 0, 0, 255],
                    edge: [0, 0, 0, 255],
                }],
                wards: vec![ward],
                plazas: Vec::new(),
                roads: Vec::new(),
                buildings,
                scenery: Vec::new(),
                ground_labels: Vec::new(),
            },
            districts: Vec::new(),
            locations: Vec::new(),
            features,
        }
    }

    /// One plan's work, as `resolve` now takes it.
    fn alone(files: Vec<ChangedFile>) -> Vec<(PlanId, ChangeSummary)> {
        vec![(PlanId::new("plan-1"), summary(files))]
    }

    /// Two agents' work, with ids chosen so their banners differ.
    fn between(first: Vec<ChangedFile>, second: Vec<ChangedFile>) -> Vec<(PlanId, ChangeSummary)> {
        vec![
            (PlanId::new("plan-1"), summary(first)),
            (PlanId::new("plan-2"), summary(second)),
        ]
    }

    /// A file that already has a house is worked on top of, not given new
    /// ground -- and the column has to start at the existing roof.
    #[test]
    fn a_changed_file_raises_works_on_the_house_it_already_has() {
        let lot = rect(10.0, 10.0, 8.0, 8.0);
        let map = world("alpha", &[("src/main.rs", lot)]);
        let raised = resolve(
            &map,
            "alpha",
            &alone(vec![file("src/main.rs", ChangeKind::Modified, 30, 10)]),
        );

        assert_eq!(raised.len(), 1);
        assert_eq!(
            raised[0].site,
            WorkSite::Standing {
                footprint: lot,
                height: 9.0
            }
        );
        assert_eq!(raised[0].bands.len(), 1, "one agent, one band");
        assert_eq!(raised[0].bands[0].added, 30.0);
        assert_eq!(raised[0].bands[0].removed, 10.0);
        assert!(!raised[0].bands[0].razing);
    }

    /// The counts are carried **absolutely**, not as a share of the plan's
    /// busiest file. That is what makes two agents comparable, and it is the
    /// heart of the "every bar looks the same" fix -- a 40-line edit says 40
    /// whatever else its plan did.
    #[test]
    fn what_a_band_carries_does_not_depend_on_the_rest_of_the_plan() {
        let map = world(
            "alpha",
            &[
                ("src/big.rs", rect(10.0, 10.0, 8.0, 8.0)),
                ("src/small.rs", rect(30.0, 30.0, 8.0, 8.0)),
            ],
        );
        let with_a_giant = resolve(
            &map,
            "alpha",
            &alone(vec![
                file("src/big.rs", ChangeKind::Modified, 800, 200),
                file("src/small.rs", ChangeKind::Modified, 10, 0),
            ]),
        );
        let by_itself = resolve(
            &map,
            "alpha",
            &alone(vec![file("src/small.rs", ChangeKind::Modified, 10, 0)]),
        );

        let small_of = |raised: &[Work]| {
            raised
                .iter()
                .find(|w| w.churn() == 10.0)
                .expect("the small file")
                .bands[0]
                .added
        };
        assert_eq!(
            small_of(&with_a_giant),
            small_of(&by_itself),
            "a file's own size must not depend on what else its plan touched"
        );
    }

    /// **The feature this was all for.** One file, two agents, one house -- and
    /// the bands say which agents, in their own colours.
    #[test]
    fn a_file_two_agents_share_is_one_house_with_two_bands() {
        let lot = rect(10.0, 10.0, 8.0, 8.0);
        let map = world("alpha", &[("src/main.rs", lot)]);
        let raised = resolve(
            &map,
            "alpha",
            &between(
                vec![file("src/main.rs", ChangeKind::Modified, 30, 10)],
                vec![file("src/main.rs", ChangeKind::Modified, 5, 40)],
            ),
        );

        assert_eq!(raised.len(), 1, "one file is one house, not two");
        assert_eq!(raised[0].bands.len(), 2);
        assert!(raised[0].is_contended());
        assert_eq!(raised[0].churn(), 85.0, "everyone's work, added up");
        assert_ne!(
            raised[0].bands[0].growth, raised[0].bands[1].growth,
            "two agents on one house must be different colours"
        );
    }

    /// Each agent's band carries that agent's own banner, and the map's colour
    /// is the palette's colour -- two spellings of one fact is how a rail and a
    /// map come to disagree.
    #[test]
    fn a_band_wears_its_own_agents_colours() {
        let map = world("alpha", &[("src/main.rs", rect(10.0, 10.0, 8.0, 8.0))]);
        let plan = PlanId::new("plan-7");
        let raised = resolve(
            &map,
            "alpha",
            &[(
                plan.clone(),
                summary(vec![file("src/main.rs", ChangeKind::Modified, 3, 1)]),
            )],
        );

        let banner = kingdom_core::palette::preferred(&plan);
        let band = raised[0].bands[0];
        assert_eq!(&band.growth[..3], &banner.growth_rgb[..]);
        assert_eq!(&band.cutting[..3], &banner.cutting_rgb[..]);
        assert_eq!(band.growth[3], 255, "the wire carries full strength");
    }

    /// The feature the King asked for: a file an agent created has no house,
    /// so it is given free ground inside its own folder.
    #[test]
    fn a_created_file_is_given_ground_inside_its_folder() {
        let taken = rect(10.0, 10.0, 8.0, 8.0);
        let map = world("alpha", &[("src/main.rs", taken)]);
        let raised = resolve(
            &map,
            "alpha",
            &alone(vec![file("src/new.rs", ChangeKind::Untracked, 40, 0)]),
        );

        assert_eq!(raised.len(), 1);
        let WorkSite::Fresh { footprint } = raised[0].site else {
            panic!(
                "a file with no house should be given ground, got {:?}",
                raised[0].site
            );
        };
        assert!(
            !overlaps(&taken, &footprint),
            "it landed on an existing lot"
        );
        let ward = &map.world.wards[0];
        assert!(
            footprint.x >= ward.rect.x && footprint.max_x() <= ward.rect.max_x(),
            "it left its folder"
        );
    }

    /// Two new files in one folder must not be drawn standing in each other.
    #[test]
    fn two_created_files_in_one_folder_are_given_separate_ground() {
        let map = world("alpha", &[("src/main.rs", rect(10.0, 10.0, 8.0, 8.0))]);
        let raised = resolve(
            &map,
            "alpha",
            &alone(vec![
                file("src/one.rs", ChangeKind::Added, 20, 0),
                file("src/two.rs", ChangeKind::Untracked, 25, 0),
            ]),
        );

        assert_eq!(raised.len(), 2);
        let first = raised[0].site.footprint();
        let second = raised[1].site.footprint();
        assert!(
            !overlaps(&first, &second),
            "{first:?} and {second:?} collide"
        );
    }

    /// The same trap across two agents: both creating the same file must give
    /// **one** ghost with two bands, not two houses in different corners of the
    /// folder claiming to be the same file.
    #[test]
    fn two_agents_creating_one_file_raise_one_ghost() {
        let map = world("alpha", &[("src/main.rs", rect(10.0, 10.0, 8.0, 8.0))]);
        let raised = resolve(
            &map,
            "alpha",
            &between(
                vec![file("src/new.rs", ChangeKind::Untracked, 20, 0)],
                vec![file("src/new.rs", ChangeKind::Untracked, 9, 2)],
            ),
        );

        assert_eq!(raised.len(), 1, "one path is one house");
        assert_eq!(raised[0].bands.len(), 2);
        assert!(matches!(raised[0].site, WorkSite::Fresh { .. }));
    }

    /// A binary file's counts are not line counts, so it is still left out.
    #[test]
    fn binary_files_are_left_out() {
        let map = world(
            "alpha",
            &[
                ("src/main.rs", rect(10.0, 10.0, 8.0, 8.0)),
                ("src/logo.png", rect(50.0, 50.0, 8.0, 8.0)),
            ],
        );
        let mut binary = file("src/logo.png", ChangeKind::Modified, 900, 0);
        binary.binary = true;

        let raised = resolve(
            &map,
            "alpha",
            &alone(vec![
                file("src/main.rs", ChangeKind::Modified, 10, 0),
                binary,
            ]),
        );

        assert_eq!(raised.len(), 1, "only the honest one is drawn");
    }

    /// **The third reported fault.** A deleted file used to be filtered out
    /// entirely and drew nothing at all, so a deletion-only plan left the map
    /// blank while an agent was working hard. It is now carried as a razing.
    #[test]
    fn a_deleted_file_is_drawn_rather_than_dropped() {
        let lot = rect(30.0, 30.0, 8.0, 8.0);
        let map = world("alpha", &[("src/gone.rs", lot)]);
        let raised = resolve(
            &map,
            "alpha",
            &alone(vec![file("src/gone.rs", ChangeKind::Deleted, 0, 200)]),
        );

        assert_eq!(raised.len(), 1, "a deletion is a thing that happened");
        assert!(raised[0].bands[0].razing);
        assert_eq!(raised[0].bands[0].removed, 200.0);
        // On the house that is still standing in the checkout the map was
        // drawn from -- which is exactly why it is drawn as a razing rather
        // than as a column.
        assert_eq!(
            raised[0].site,
            WorkSite::Standing {
                footprint: lot,
                height: 9.0
            }
        );
    }

    /// A deletion git reports with no line counts at all is still a deletion.
    /// The zero-churn guard must not swallow it.
    #[test]
    fn a_deletion_with_no_counts_is_still_a_deletion() {
        let map = world("alpha", &[("src/gone.rs", rect(30.0, 30.0, 8.0, 8.0))]);
        let raised = resolve(
            &map,
            "alpha",
            &alone(vec![file("src/gone.rs", ChangeKind::Deleted, 0, 0)]),
        );
        assert_eq!(raised.len(), 1);
        assert!(raised[0].bands[0].razing);
    }

    /// But a file that has no house *and* only ever existed to be deleted must
    /// not raise a ghost: building a house to announce that a house is gone is
    /// the one thing worse than drawing nothing.
    #[test]
    fn a_deleted_file_with_no_house_raises_nothing() {
        let map = world("alpha", &[("src/main.rs", rect(10.0, 10.0, 8.0, 8.0))]);
        let raised = resolve(
            &map,
            "alpha",
            &alone(vec![file("src/vanished.rs", ChangeKind::Deleted, 0, 40)]),
        );
        assert!(raised.is_empty());
    }

    /// The map is drawn from the city's checkout and follows its *shape*, not
    /// its contents -- so a file in a project the map never drew is an ordinary
    /// absence rather than something to invent ground for.
    #[test]
    fn a_file_in_a_town_that_is_not_on_the_map_is_skipped() {
        let map = world("alpha", &[("src/main.rs", rect(10.0, 10.0, 8.0, 8.0))]);
        let raised = resolve(
            &map,
            "nowhere",
            &alone(vec![file("src/main.rs", ChangeKind::Modified, 10, 2)]),
        );
        assert!(raised.is_empty());
    }

    /// A folder the layout merged away has no ward, so there is no ground to
    /// place a ghost on. Skipped rather than dropped somewhere arbitrary.
    #[test]
    fn a_created_file_in_an_undrawn_folder_is_skipped() {
        let map = world("alpha", &[("src/main.rs", rect(10.0, 10.0, 8.0, 8.0))]);
        let raised = resolve(
            &map,
            "alpha",
            &alone(vec![file("vendor/deep/new.rs", ChangeKind::Added, 40, 0)]),
        );
        assert!(raised.is_empty());
    }

    /// A rename with no content change has nothing to say, and must produce
    /// nothing rather than a zero-sized column.
    #[test]
    fn a_summary_with_nothing_to_measure_draws_nothing() {
        let map = world("alpha", &[("src/main.rs", rect(10.0, 10.0, 8.0, 8.0))]);
        let raised = resolve(
            &map,
            "alpha",
            &alone(vec![file("src/main.rs", ChangeKind::Renamed, 0, 0)]),
        );
        assert!(raised.is_empty());
        assert!(resolve(&map, "alpha", &alone(Vec::new())).is_empty());
        assert!(resolve(&map, "alpha", &[]).is_empty(), "no agents at all");
    }

    /// **The King's own rule, at the seam that computes it.** Two hundred lines
    /// cut from a four-hundred-line file is half the file, so half the house is
    /// covered.
    ///
    /// This is the number the whole feature turns on, and it is computed here
    /// rather than in the renderer because the denominator is the file's
    /// scanned length -- a fact about a codebase, which the engine is
    /// deliberately ignorant of.
    #[test]
    fn half_a_file_removed_covers_half_the_house() {
        let map = world_of("alpha", &[("src/main.rs", rect(10.0, 10.0, 8.0, 8.0), 400)]);
        let raised = resolve(
            &map,
            "alpha",
            &alone(vec![file("src/main.rs", ChangeKind::Modified, 0, 200)]),
        );

        assert_eq!(raised.len(), 1);
        assert_eq!(raised[0].bands[0].cover, 0.5);
    }

    /// The same cut in a file five times the size covers a fifth as much. That
    /// difference is the entire reason `cover` is a share rather than reusing
    /// the column's absolute churn ramp.
    #[test]
    fn the_same_cut_covers_less_of_a_bigger_file() {
        let cut = |lines: usize| {
            let map = world_of(
                "alpha",
                &[("src/main.rs", rect(10.0, 10.0, 8.0, 8.0), lines)],
            );
            resolve(
                &map,
                "alpha",
                &alone(vec![file("src/main.rs", ChangeKind::Modified, 0, 100)]),
            )[0]
            .bands[0]
                .cover
        };
        assert_eq!(cut(200), 0.5);
        assert_eq!(cut(1_000), 0.1);
        assert!(cut(200) > cut(1_000));
    }

    /// A deletion covers the whole house, whatever git managed to count. "This
    /// file is going" is not a matter of degree -- and a deletion reported as
    /// `-0` because the file was empty is still total.
    #[test]
    fn a_deletion_covers_the_whole_house() {
        let map = world_of("alpha", &[("src/gone.rs", rect(10.0, 10.0, 8.0, 8.0), 900)]);
        for counted in [0, 3, 900] {
            let raised = resolve(
                &map,
                "alpha",
                &alone(vec![file("src/gone.rs", ChangeKind::Deleted, 0, counted)]),
            );
            assert_eq!(
                raised[0].bands[0].cover, 1.0,
                "a deletion git counted as {counted} covered less than the house"
            );
        }
    }

    /// A file the scanner never counted still shows something. Its `lines` is
    /// zero -- too large to analyse, or not text -- and dividing by it would be
    /// either a NaN or an invisible removal, which is the exact fault the
    /// shroud replaced.
    #[test]
    fn a_file_of_unknown_length_still_covers_something() {
        let map = world("alpha", &[("src/huge.rs", rect(10.0, 10.0, 8.0, 8.0))]);
        let raised = resolve(
            &map,
            "alpha",
            &alone(vec![file("src/huge.rs", ChangeKind::Modified, 0, 100)]),
        );

        let cover = raised[0].bands[0].cover;
        assert!(cover.is_finite(), "an unknown length gave {cover}");
        assert!(
            cover > 0.0 && cover < 1.0,
            "an unmeasurable file covered {cover}, which reads as either nothing or a deletion"
        );
    }

    /// A stale manifest is allowed to report fewer lines than were removed --
    /// it is memoised on the shape of the kingdom, not on any file's contents.
    /// A house cannot be more than covered.
    #[test]
    fn a_cut_bigger_than_the_file_covers_no_more_than_all_of_it() {
        let map = world_of("alpha", &[("src/main.rs", rect(10.0, 10.0, 8.0, 8.0), 10)]);
        let raised = resolve(
            &map,
            "alpha",
            &alone(vec![file("src/main.rs", ChangeKind::Modified, 0, 4_000)]),
        );

        assert_eq!(raised[0].bands[0].cover, 1.0);
    }

    /// Two agents cutting one file each cover their own share, so the stack
    /// adds up to what the file is actually losing rather than to twice it.
    /// Whose deletion is whose is question two of `AGENTS.md`.
    #[test]
    fn two_agents_cutting_one_file_cover_their_own_shares() {
        let map = world_of("alpha", &[("src/main.rs", rect(10.0, 10.0, 8.0, 8.0), 400)]);
        let raised = resolve(
            &map,
            "alpha",
            &between(
                vec![file("src/main.rs", ChangeKind::Modified, 0, 100)],
                vec![file("src/main.rs", ChangeKind::Modified, 0, 100)],
            ),
        );

        assert_eq!(raised.len(), 1, "one file is one house");
        assert_eq!(raised[0].bands.len(), 2, "two agents, two bands");
        let total: f32 = raised[0].bands.iter().map(|band| band.cover).sum();
        assert_eq!(
            total, 0.5,
            "200 of 400 lines is half the house between them"
        );
    }

    /// Growth alone covers nothing. A file that only gained lines wears a
    /// column and no shroud at all -- the two directions are now drawn in two
    /// places, and nothing about an addition may appear below the roof.
    #[test]
    fn a_file_that_only_grew_covers_nothing() {
        let map = world_of("alpha", &[("src/main.rs", rect(10.0, 10.0, 8.0, 8.0), 400)]);
        let raised = resolve(
            &map,
            "alpha",
            &alone(vec![file("src/main.rs", ChangeKind::Modified, 120, 0)]),
        );

        assert_eq!(raised[0].bands[0].cover, 0.0);
        assert_eq!(raised[0].bands[0].added, 120.0);
    }

    /// Every band's counts must be finite and non-negative: they become mesh
    /// dimensions, and a NaN reaches Bevy as a degenerate mesh.
    ///
    /// `cover` is in here for a sharper version of the same reason: it is a
    /// *ratio*, so it has a denominator that can be zero, and `f32::clamp`
    /// propagates NaN rather than trapping it.
    #[test]
    fn every_band_carries_a_drawable_number() {
        let map = world(
            "alpha",
            &[
                ("src/a.rs", rect(10.0, 10.0, 8.0, 8.0)),
                ("src/b.rs", rect(30.0, 30.0, 8.0, 8.0)),
            ],
        );
        let raised = resolve(
            &map,
            "alpha",
            &between(
                vec![file("src/a.rs", ChangeKind::Modified, 4000, 0)],
                vec![file("src/b.rs", ChangeKind::Deleted, 0, 1)],
            ),
        );
        for work in &raised {
            assert!(!work.bands.is_empty(), "a work with no bands is nobody's");
            for band in &work.bands {
                assert!(band.added.is_finite() && band.added >= 0.0);
                assert!(band.removed.is_finite() && band.removed >= 0.0);
                assert!(
                    band.cover.is_finite() && (0.0..=1.0).contains(&band.cover),
                    "a cover of {} is not a share of a house",
                    band.cover
                );
            }
        }
    }

    /// The review is refetched on every transcript entry, so resolving the same
    /// work twice has to give the same map -- or the works would shuffle while
    /// the King watched. The grouping is a `BTreeMap` for exactly this reason.
    #[test]
    fn resolving_the_same_changes_twice_gives_the_same_works() {
        let map = world("alpha", &[("src/main.rs", rect(10.0, 10.0, 8.0, 8.0))]);
        let changes = between(
            vec![
                file("src/main.rs", ChangeKind::Modified, 12, 3),
                file("src/new.rs", ChangeKind::Untracked, 30, 0),
            ],
            vec![file("src/other.rs", ChangeKind::Added, 8, 0)],
        );
        assert_eq!(
            resolve(&map, "alpha", &changes),
            resolve(&map, "alpha", &changes)
        );
    }

    /// And the order the agents are handed in must not move a house either: the
    /// grouping is by path, so who was listed first is not a fact about ground.
    #[test]
    fn the_order_agents_are_listed_in_does_not_move_a_house() {
        let map = world("alpha", &[("src/main.rs", rect(10.0, 10.0, 8.0, 8.0))]);
        let first = vec![file("src/new.rs", ChangeKind::Untracked, 30, 0)];
        let second = vec![file("src/also.rs", ChangeKind::Added, 8, 0)];

        let one_way = resolve(&map, "alpha", &between(first.clone(), second.clone()));
        let other_way = resolve(&map, "alpha", &between(second, first));

        let ground = |raised: &[Work]| {
            let mut lots: Vec<_> = raised.iter().map(|w| w.site.footprint()).collect();
            lots.sort_by(|a, b| a.x.total_cmp(&b.x).then(a.y.total_cmp(&b.y)));
            lots
        };
        assert_eq!(ground(&one_way), ground(&other_way));
    }
}
