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

/// Buildings per city, before the rest are aggregated rather than drawn.
///
/// Repo City's own default. A town keeps its shape well past this, and the
/// files beyond it are still counted -- see `Repository::omitted_files`.
const MAX_FILES: usize = 1_500;

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
    let options = ScanOptions {
        max_files: MAX_FILES,
        include_ignored: false,
    };

    let mut repositories: Vec<Repository> = Vec::with_capacity(kingdom.cities.len());
    for city in &kingdom.cities {
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
