//! Turning a kingdom on disk into a map manifest.
//!
//! Server-only: this walks the filesystem, so it must never reach the wasm
//! bundle.
//!
//! # Where this came from
//!
//! Everything below this module is Repo City
//! (<https://github.com/craigloewen-msft/repo-city-visualizer>, MIT), copied in
//! at `449f090` and kept as close to verbatim as the move allowed so its own
//! tests still pin it. The changes were mechanical: crate paths, and `anyhow`
//! traded for [`std::io`] with the CLI that brought it.
//!
//! # Why Kingdom drives the stages itself
//!
//! Repo City's own entry point (`Survey`) discovers projects by looking for
//! `.git` and stops there. Kingdom's [`scan`](crate::build::scan) counterpart --
//! `kingdom_app::scan` -- treats *every* immediate subdirectory as a city, and
//! the two disagree: the `kingdom-mirror` proving ground has a `forge` with no
//! repository in it, which `Survey` drops silently. A map that quietly omits a
//! city the rail beside it lists is worse than no map, so `Survey` was left
//! behind and this walks [`Kingdom::cities`] instead -- the same list every
//! other part of Kingdom reads.

pub mod layout;
pub mod manifest;
pub mod model;
pub mod scan;
pub mod scene;

mod scenery;
mod streets;
mod wayfinding;

use std::path::Path;

use kingdom_core::Kingdom;

use crate::map::MapManifest;
use layout::WorldLayout;
use manifest::build_world_manifest;
use model::Repository;
use scan::{ScanOptions, scan_repository};
use scene::build_realm_world;

/// Buildings in a whole kingdom, before the rest are aggregated rather than
/// drawn.
///
/// # Why this is a kingdom's budget and not a city's
///
/// It used to be 1,500 *per city* -- Repo City's own default, which was right
/// for Repo City because it drew one repository at a time. Kingdom draws all of
/// them at once, so the same number meant the cost of opening a folder grew
/// without limit in the number of projects in it: fifteen cities is a ceiling
/// of 22,500 holdings, forty is 60,000, and the King who opens a big `~/dev`
/// pays for every one of them on every frame.
///
/// So the ceiling now belongs to the thing actually being drawn. A kingdom of
/// forty projects costs about what one of fifteen costs, with each town
/// correspondingly less detailed, rather than being three times slower.
///
/// The figure is roughly what a real fifteen-project `~/dev` already produced
/// (6,832 holdings), so this is a ceiling on growth rather than a cut to what
/// is drawn today.
const MAX_FILES: usize = 7_500;

/// Fewest buildings any city may be reduced to.
///
/// A town below this stops reading as a settlement and starts reading as a
/// bug -- and a project's presence on the map is worth more than its detail.
/// With the budget above, this is the share a city gets once a kingdom passes
/// about forty projects, at which point every town is drawn at its floor and
/// the map is a map of *which* projects exist rather than of what is in them.
/// That is the right thing for it to become at that size.
const MIN_FILES_PER_CITY: usize = 180;

/// How many buildings each city may draw, in the order the cities are given.
///
/// Apportioned by how much code each city actually holds, so a monorepo beside
/// a scratch project still looks like the larger of the two, with
/// [`MIN_FILES_PER_CITY`] as a floor so the scratch project is still a town.
///
/// A free function taking plain numbers because it is the whole of the policy
/// and is worth testing without a filesystem: every interesting case here is
/// arithmetic about proportion and rounding, not about disk.
///
/// # Why a share is never clamped to the city's own count
///
/// `file_counts` are `City::file_count`, and those are **estimates**: `scan.rs`
/// stops at `COUNT_CAP` files and `SCAN_DEPTH` folders deep, so a large or
/// deeply nested project is routinely undercounted -- this very checkout holds
/// 33,588 files and reports at most 5,000. Capping a budget at that number
/// would silently starve exactly the projects most worth drawing. A share that
/// turns out larger than a city can use costs nothing: `max_files` is a
/// truncation cap, and a city with less than its share simply draws all of it.
fn apportion(file_counts: &[usize], budget: usize) -> Vec<usize> {
    if file_counts.is_empty() {
        return Vec::new();
    }

    // Every city is a town before it is a big town. If even the floors do not
    // fit, the floor wins and the budget is overspent: a map missing a project
    // entirely is worse than a map that costs a little more than intended.
    let floors = MIN_FILES_PER_CITY * file_counts.len();
    if budget <= floors {
        return vec![MIN_FILES_PER_CITY; file_counts.len()];
    }

    // What is left once every city has its floor, shared out by size.
    let spare = budget - floors;
    let total: usize = file_counts.iter().sum();
    if total == 0 {
        return vec![MIN_FILES_PER_CITY; file_counts.len()];
    }

    file_counts
        .iter()
        .map(|&count| {
            // In u128 because `count * spare` overflows a 32-bit `usize` long
            // before either factor is implausible on its own.
            MIN_FILES_PER_CITY + (count as u128 * spare as u128 / total as u128) as usize
        })
        .collect()
}

/// Builds the map for a whole kingdom, one town per city.
///
/// A city whose folder cannot be read is skipped rather than failing the map:
/// the kingdom is rescanned on every open and a folder may have been moved
/// since, and one unreadable project should not cost the King the other eleven.
///
/// Returns `None` when nothing at all could be read, because a manifest with no
/// towns has no disk, no bounds and nothing for the camera to frame.
pub fn manifest_for(kingdom: &Kingdom) -> Option<MapManifest> {
    let root = Path::new(&kingdom.root);

    // Apportioned from the counts the kingdom scan already took, so deciding
    // how much of each city to draw costs no extra walk of the disk.
    let counts: Vec<usize> = kingdom.cities.iter().map(|city| city.file_count).collect();
    let budgets = apportion(&counts, MAX_FILES);

    let mut repositories: Vec<Repository> = Vec::with_capacity(kingdom.cities.len());
    for (city, &max_files) in kingdom.cities.iter().zip(budgets.iter()) {
        let options = ScanOptions {
            max_files,
            include_ignored: false,
        };
        // `City::path` is relative to the kingdom root; the scanner needs a real
        // path on disk.
        let path = root.join(&city.path);
        match scan_repository(&path, &options) {
            // An empty project would still claim a town and a name, and a town
            // with nothing in it reads as a bug rather than as a fact about the
            // folder. Repo City's own rule, kept.
            Ok(repository) if repository.root.metrics.file_count > 0 => {
                repositories.push(repository)
            }
            Ok(_) => {}
            Err(error) => {
                eprintln!("citymap: skipping {}: {error}", path.display());
            }
        }
    }

    if repositories.is_empty() {
        return None;
    }

    let layout = WorldLayout::build(&repositories);
    let world = build_realm_world(&layout);
    let name = root
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("kingdom");
    Some(build_world_manifest(name, &repositories, &layout, world))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_kingdoms_budget_is_shared_out_by_size() {
        let shares = apportion(&[8_000, 4_000, 2_000, 2_000], MAX_FILES);
        assert!(shares[0] > shares[1], "{shares:?}");
        assert!(shares[1] > shares[2], "{shares:?}");
        assert_eq!(shares[2], shares[3], "equal cities get equal shares");
        assert!(shares.iter().sum::<usize>() <= MAX_FILES, "{shares:?}");
    }

    /// The whole point of the change: cost stops growing with the number of
    /// projects. This is what a regression to a per-city cap would break.
    #[test]
    fn more_cities_does_not_mean_a_bigger_world() {
        let fifteen: usize = apportion(&[5_000; 15], MAX_FILES).iter().sum();
        let forty: usize = apportion(&[5_000; 40], MAX_FILES).iter().sum();
        assert!(fifteen <= MAX_FILES, "fifteen cities spent {fifteen}");
        assert!(forty <= MAX_FILES, "forty cities spent {forty}");
    }

    /// One enormous project beside three small ones must not reduce the small
    /// ones to nothing: a project on the map at all should read as a town.
    #[test]
    fn every_city_is_still_a_town() {
        let shares = apportion(&[500_000, 30, 40, 50], MAX_FILES);
        assert!(shares[0] > MIN_FILES_PER_CITY, "{shares:?}");
        for share in &shares[1..] {
            assert!(*share >= MIN_FILES_PER_CITY, "{shares:?}");
        }
    }

    /// Past the point where the floors fill the budget, the floor wins. A map
    /// missing a project entirely is worse than one that costs a little more.
    #[test]
    fn a_kingdom_too_large_to_share_still_draws_every_city() {
        let many = apportion(&[1_000; 400], MAX_FILES);
        assert_eq!(many.len(), 400);
        assert!(many.iter().all(|share| *share == MIN_FILES_PER_CITY));
    }

    /// A share larger than the city can use is harmless, and deliberately not
    /// clamped -- `City::file_count` is a capped estimate, so clamping to it
    /// would starve the deep projects most worth drawing. See `apportion`.
    #[test]
    fn a_share_may_exceed_what_a_city_reports() {
        let shares = apportion(&[20, 20], MAX_FILES);
        assert!(shares.iter().all(|share| *share > 20), "{shares:?}");
        assert!(shares.iter().sum::<usize>() <= MAX_FILES);
    }

    #[test]
    fn an_empty_kingdom_apportions_nothing() {
        assert!(apportion(&[], MAX_FILES).is_empty());
    }

    /// A kingdom of empty folders must not divide by zero.
    #[test]
    fn cities_with_no_files_still_get_a_share() {
        assert_eq!(
            apportion(&[0, 0, 0], MAX_FILES),
            vec![MIN_FILES_PER_CITY; 3]
        );
    }
}
