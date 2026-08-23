//! Filesystem scanning: turning a real dev folder into a Kingdom.
//!
//! Server-only. This is the one place where the metaphor touches the disk.

use kingdom_core::{City, CityId, CityKind};
use std::path::Path;

/// Directories that are never projects and are expensive to walk.
const SKIP_DIRS: &[&str] = &[
    "node_modules",
    "target",
    ".git",
    ".svn",
    "dist",
    "build",
    "venv",
    ".venv",
    "__pycache__",
    ".next",
    ".cache",
    "vendor",
];

/// How deep to descend when counting files. Enough to gauge project size
/// without walking an entire monorepo on every page load.
const COUNT_DEPTH: usize = 3;

/// Cap on files counted per city, so one enormous project cannot stall a scan.
const COUNT_CAP: usize = 5_000;

/// Scans a dev folder, treating each immediate subdirectory as a city.
///
/// Only one level down: the kingdom is a flat collection of projects. Nested
/// structures (`work/`, `personal/`) would need districts, which is a
/// deliberate later decision rather than something to guess at now.
pub fn scan_kingdom(root: &Path) -> std::io::Result<Vec<City>> {
    let mut cities = Vec::new();

    for entry in std::fs::read_dir(root)? {
        let Ok(entry) = entry else { continue };
        let path = entry.path();

        if !path.is_dir() {
            continue;
        }

        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };

        // Hidden folders and known build detritus are not projects.
        if name.starts_with('.') || SKIP_DIRS.contains(&name) {
            continue;
        }

        cities.push(City {
            id: CityId::new(name),
            name: name.to_string(),
            path: name.to_string(),
            kind: detect_kind(&path),
            file_count: count_files(&path, COUNT_DEPTH),
            has_git: path.join(".git").exists(),
            // Real git status needs a git library or subprocess; deferred until
            // the agent layer is real, since that is what will make it matter.
            dirty_files: 0,
        });
    }

    // Alphabetical, so map positions stay stable across scans.
    cities.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    Ok(cities)
}

/// Infers a project's stack from marker files in its root.
fn detect_kind(path: &Path) -> CityKind {
    let has = |f: &str| path.join(f).exists();

    let mut kinds = Vec::new();
    if has("Cargo.toml") {
        kinds.push(CityKind::Rust);
    }
    if has("package.json") {
        kinds.push(CityKind::Node);
    }
    if has("pyproject.toml") || has("requirements.txt") || has("setup.py") {
        kinds.push(CityKind::Python);
    }
    if has("go.mod") {
        kinds.push(CityKind::Go);
    }

    match kinds.len() {
        0 => CityKind::Unknown,
        1 => kinds[0],
        _ => CityKind::Mixed,
    }
}

/// Counts files up to a bounded depth, skipping build directories.
fn count_files(path: &Path, depth: usize) -> usize {
    fn walk(path: &Path, depth: usize, count: &mut usize) {
        if depth == 0 || *count >= COUNT_CAP {
            return;
        }

        let Ok(entries) = std::fs::read_dir(path) else {
            return;
        };

        for entry in entries.flatten() {
            if *count >= COUNT_CAP {
                return;
            }

            let p = entry.path();
            let name = entry.file_name();
            let name = name.to_string_lossy();

            if p.is_dir() {
                if !name.starts_with('.') && !SKIP_DIRS.contains(&name.as_ref()) {
                    walk(&p, depth - 1, count);
                }
            } else {
                *count += 1;
            }
        }
    }

    let mut count = 0;
    walk(path, depth, &mut count);
    count
}
