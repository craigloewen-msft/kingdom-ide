//! What a plan proposes, as ground on the map.
//!
//! The King opens a chamber and the map shows him what his agent is building:
//! a house that gained lines wears a scaffold, one that lost them wears a
//! skirt, and a file that did not exist an hour ago stands as a ghost on free
//! land inside the folder it belongs to.
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
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Work {
    /// The ground this work stands on.
    pub site: WorkSite,
    /// How much of this file moved, as a fraction of the busiest file in the
    /// same plan -- `kingdom_core::ChangeSummary::busiest`.
    ///
    /// Always in `0.0..=1.0`. The normalising is done on the interface's side
    /// of the bridge, so nothing downstream needs the rest of the plan in hand
    /// to know how tall to build.
    pub scale: f32,
    /// What share of the churn was growth, from 0.0 (all deletion) to 1.0 (all
    /// addition).
    ///
    /// Carried rather than the raw counts because it is what the drawing
    /// actually asks: how much scaffold, and how much skirt. The counts
    /// themselves are the review drawer's business.
    pub growth: f32,
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
    use super::{Work, WorkSite, place_fresh};
    use crate::map::{MapManifest, MapRect, MapWard};
    use kingdom_core::{ChangeKind, ChangeSummary};
    use std::collections::HashMap;

    /// Turns what a plan changed into ground on the map.
    ///
    /// The whole of the domain-to-geometry translation. Everything above this
    /// is Kingdom's domain -- paths, line counts, a `ChangeSummary` -- and
    /// everything below it is world-space geometry, which is what keeps the
    /// engine ignorant of what a plan is.
    ///
    /// Every omission it makes is a judgement about what is *honest* to draw:
    ///
    /// - **Binary files are left out.** `ChangedFile::binary` exists because
    ///   `+0 -0` reads as "unchanged" for a file that certainly did change. Its
    ///   numbers are not line counts, and a scaffold built from them would be an
    ///   invented figure standing over a real house.
    /// - **Deletions are left out.** A deleted file is a lot that *empties*, not
    ///   a house that grows -- and the map is drawn from the city's checkout,
    ///   where the house is still standing. A scaffold on it would say the
    ///   opposite of what happened.
    /// - **A file with no house and no ward is skipped.** An empty project is
    ///   dropped from the manifest entirely (`build::manifest_for`), and a
    ///   folder the layout merged away has no ward to stand a ghost in. Both are
    ///   the ordinary staleness `kingdom_app::citymap` documents as a deliberate
    ///   trade -- the map follows the shape of a codebase, not its contents.
    ///
    /// Ghost houses are placed in the summary's order, and each is added to the
    /// ground the next must avoid, so two new files in one folder never stand in
    /// each other. The summary is sorted by path (`review::changes`), so that
    /// order is stable across refetches and the houses do not shuffle.
    pub fn resolve(map: &MapManifest, city: &str, summary: &ChangeSummary) -> Vec<Work> {
        // The scale is taken over the files that will actually be *drawn*, not
        // over the whole summary, and the difference is visible on screen. A
        // plan that deleted a 400-line file and edited a 40-line one draws only
        // the edit -- and normalised against the deletion it would draw it at a
        // tenth height, so the one thing on the map would look like nothing had
        // happened. `ChangeSummary::busiest` skips binaries for the same reason;
        // this goes further because this knows what it is going to omit.
        let drawable: Vec<&kingdom_core::ChangedFile> = summary
            .files
            .iter()
            .filter(|file| !file.binary && file.kind != ChangeKind::Deleted && file.churn() > 0)
            .collect();
        let busiest = drawable.iter().map(|file| file.churn()).max().unwrap_or(0);
        if busiest == 0 {
            // Nothing with an honest line count is being drawn -- a rename-only,
            // deletion-only or binary-only summary. Inventing a denominator
            // here would draw a picture of nothing.
            return Vec::new();
        }

        let mut raised = Vec::new();
        // Ground claimed by ghost houses as this pass places them.
        let mut claimed: HashMap<String, Vec<MapRect>> = HashMap::new();

        for file in drawable {
            let churn = file.churn();
            let scale = (churn as f32 / busiest as f32).clamp(0.0, 1.0);
            let growth = file.added as f32 / churn as f32;

            let site = match map.holding_at(city, &file.path) {
                // The house is on the map: the work happens on top of it.
                Some(feature) => WorkSite::Standing {
                    footprint: feature.footprint,
                    height: feature.height,
                },
                // No house. Either the court created this file or the map is
                // stale about it, and the two are indistinguishable from here --
                // which is fine, because both mean "there is no building for
                // this, so give it ground of its own".
                None => {
                    let folder = match file.path.rfind('/') {
                        Some(at) => &file.path[..at],
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
                    let Some(footprint) = place_fresh(ward, &taken, seed(&file.path), want) else {
                        continue;
                    };
                    claimed.entry(ward.id.clone()).or_default().push(footprint);
                    WorkSite::Fresh { footprint }
                }
            };

            raised.push(Work {
                site,
                scale,
                growth,
            });
        }

        raised
    }

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
    use kingdom_core::{ChangeKind, ChangeSummary, ChangedFile, Language};

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
    fn world(city: &str, houses: &[(&str, MapRect)]) -> MapManifest {
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
            .map(|(path, lot)| MapFeature {
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
                lines: 0,
                complexity: 0,
                references: 0,
                footprint: *lot,
                height: 9.0,
                center: lot.center(),
            })
            .collect();
        let buildings = houses
            .iter()
            .map(|(path, lot)| MapBuilding {
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

    /// A file that already has a house is worked on top of, not given new
    /// ground -- and the scaffold has to start at the existing roof.
    #[test]
    fn a_changed_file_raises_works_on_the_house_it_already_has() {
        let lot = rect(10.0, 10.0, 8.0, 8.0);
        let map = world("alpha", &[("src/main.rs", lot)]);
        let raised = resolve(
            &map,
            "alpha",
            &summary(vec![file("src/main.rs", ChangeKind::Modified, 30, 10)]),
        );

        assert_eq!(raised.len(), 1);
        assert_eq!(
            raised[0].site,
            WorkSite::Standing {
                footprint: lot,
                height: 9.0
            }
        );
        assert_eq!(raised[0].scale, 1.0, "the only file is the busiest");
        assert!(
            (raised[0].growth - 0.75).abs() < 1e-6,
            "30 of 40 was growth"
        );
    }

    /// The feature the King asked for: a file the court created has no house,
    /// so it is given free ground inside its own folder.
    #[test]
    fn a_created_file_is_given_ground_inside_its_folder() {
        let taken = rect(10.0, 10.0, 8.0, 8.0);
        let map = world("alpha", &[("src/main.rs", taken)]);
        let raised = resolve(
            &map,
            "alpha",
            &summary(vec![file("src/new.rs", ChangeKind::Untracked, 40, 0)]),
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
            &summary(vec![
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

    /// A binary file's counts are not line counts, and a deleted file's house
    /// is still standing in the checkout the map was drawn from. Neither is
    /// honest to draw.
    #[test]
    fn binary_and_deleted_files_are_left_out() {
        let map = world(
            "alpha",
            &[
                ("src/main.rs", rect(10.0, 10.0, 8.0, 8.0)),
                ("src/gone.rs", rect(30.0, 30.0, 8.0, 8.0)),
                ("src/logo.png", rect(50.0, 50.0, 8.0, 8.0)),
            ],
        );
        let mut binary = file("src/logo.png", ChangeKind::Modified, 900, 0);
        binary.binary = true;

        let raised = resolve(
            &map,
            "alpha",
            &summary(vec![
                file("src/main.rs", ChangeKind::Modified, 10, 0),
                file("src/gone.rs", ChangeKind::Deleted, 0, 200),
                binary,
            ]),
        );

        assert_eq!(raised.len(), 1, "only the honest one is drawn");
        // And the binary's 900 lines must not have set the scale either.
        assert_eq!(raised[0].scale, 1.0);
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
            &summary(vec![file("src/main.rs", ChangeKind::Modified, 10, 2)]),
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
            &summary(vec![file("vendor/deep/new.rs", ChangeKind::Added, 40, 0)]),
        );
        assert!(raised.is_empty());
    }

    /// The normaliser at work: the busiest file is full height and the others
    /// are a real fraction of it, in the right order.
    #[test]
    fn every_scaffold_is_scaled_against_the_busiest_file() {
        let map = world(
            "alpha",
            &[
                ("src/big.rs", rect(10.0, 10.0, 8.0, 8.0)),
                ("src/small.rs", rect(30.0, 30.0, 8.0, 8.0)),
            ],
        );
        let raised = resolve(
            &map,
            "alpha",
            &summary(vec![
                file("src/big.rs", ChangeKind::Modified, 80, 20),
                file("src/small.rs", ChangeKind::Modified, 10, 0),
            ]),
        );

        assert_eq!(raised.len(), 2);
        assert_eq!(raised[0].scale, 1.0);
        assert!((raised[1].scale - 0.1).abs() < 1e-6);
        // And every scale stays in range, which is what the renderer assumes.
        for work in &raised {
            assert!((0.0..=1.0).contains(&work.scale), "{:?}", work.scale);
            assert!((0.0..=1.0).contains(&work.growth), "{:?}", work.growth);
        }
    }

    /// A rename with no content change divides by zero. It must produce nothing
    /// rather than a NaN that reaches the renderer -- the sibling of the guard
    /// in `engine::works::scaffold_height`.
    #[test]
    fn a_summary_with_nothing_to_measure_draws_nothing() {
        let map = world("alpha", &[("src/main.rs", rect(10.0, 10.0, 8.0, 8.0))]);
        let raised = resolve(
            &map,
            "alpha",
            &summary(vec![file("src/main.rs", ChangeKind::Renamed, 0, 0)]),
        );
        assert!(raised.is_empty());
        assert!(resolve(&map, "alpha", &summary(Vec::new())).is_empty());
    }

    /// The review is refetched on every transcript entry, so resolving the same
    /// summary twice has to give the same map -- or the works would shuffle
    /// while the King watched.
    #[test]
    fn resolving_the_same_changes_twice_gives_the_same_works() {
        let map = world("alpha", &[("src/main.rs", rect(10.0, 10.0, 8.0, 8.0))]);
        let changes = summary(vec![
            file("src/main.rs", ChangeKind::Modified, 12, 3),
            file("src/new.rs", ChangeKind::Untracked, 30, 0),
        ]);
        assert_eq!(
            resolve(&map, "alpha", &changes),
            resolve(&map, "alpha", &changes)
        );
    }
}
