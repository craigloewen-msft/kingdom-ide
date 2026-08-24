//! Filesystem scanning: turning a real dev folder into a Kingdom.
//!
//! Server-only. This is the one place where the metaphor touches the disk.

use kingdom_core::{Building, City, CityId, CityKind, District, Ward};
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

/// How deep to descend when scanning a project. Enough to gauge size and shape
/// without walking an entire monorepo on every page load.
const SCAN_DEPTH: usize = 5;

/// Cap on files visited per city, so one enormous project cannot stall a scan.
const COUNT_CAP: usize = 5_000;

/// Cap on files listed individually **per folder**.
///
/// The skyline can only draw so many buildings, and it aggregates the rest
/// anyway; keeping the largest files from each folder is what makes that
/// aggregation faithful. The remainder is still counted and weighed, never
/// dropped -- see `District::extra_files`.
const FILES_PER_DISTRICT: usize = 64;

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

        // One walk yields both the size signal and the shape the map draws, so
        // adding the skyline costs no extra traversal.
        let mut budget = COUNT_CAP;
        let structure = survey(&path, name, "", SCAN_DEPTH, &mut budget);
        let file_count = structure.total_files();

        cities.push(City {
            id: CityId::new(name),
            name: name.to_string(),
            path: name.to_string(),
            kind: detect_kind(&path),
            file_count,
            has_git: path.join(".git").exists(),
            // Real git status needs a git library or subprocess; deferred until
            // the agent layer is real, since that is what will make it matter.
            dirty_files: 0,
            structure: Some(structure),
        });
    }

    // Alphabetical, so map positions stay stable across scans.
    cities.sort_by_key(|c| c.name.to_lowercase());
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

/// Walks one project into a district tree, bounded by depth and `budget`.
///
/// `rel` is the path relative to the city root, which is what the map uses to
/// identify a building.
fn survey(dir: &Path, name: &str, rel: &str, depth: usize, budget: &mut usize) -> District {
    let mut district = District::new(name, rel);

    let Ok(entries) = std::fs::read_dir(dir) else {
        return district;
    };

    let mut files: Vec<Building> = Vec::new();
    let mut subdirs: Vec<(std::path::PathBuf, String)> = Vec::new();

    for entry in entries.flatten() {
        if *budget == 0 {
            break;
        }

        let entry_name = entry.file_name();
        let entry_name = entry_name.to_string_lossy().to_string();
        let path = entry.path();

        // `file_type` avoids a second stat that `is_dir()` would cost.
        let Ok(file_type) = entry.file_type() else {
            continue;
        };

        if file_type.is_dir() {
            if entry_name.starts_with('.') || SKIP_DIRS.contains(&entry_name.as_str()) {
                continue;
            }
            if depth > 1 {
                subdirs.push((path, entry_name));
            }
            continue;
        }

        if !file_type.is_file() {
            continue;
        }

        *budget -= 1;

        let child_rel = join_rel(rel, &entry_name);
        files.push(Building {
            ward: Ward::from_path(&child_rel),
            // Size drives building height. `metadata` is the one extra syscall
            // the skyline costs, and only for files already being visited.
            bulk: entry.metadata().map(|m| m.len()).unwrap_or(0),
            name: entry_name,
            path: child_rel,
        });
    }

    // Keep the largest files: they are the ones worth a tower of their own, and
    // the rest is preserved in aggregate rather than discarded.
    files.sort_by(|a, b| b.bulk.cmp(&a.bulk).then_with(|| a.path.cmp(&b.path)));
    if files.len() > FILES_PER_DISTRICT {
        let pruned = files.split_off(FILES_PER_DISTRICT);
        district.extra_files = pruned.len();
        district.extra_bulk = pruned.iter().map(|b| b.bulk).sum();
    }
    district.buildings = files;

    // Deterministic recursion order, so two machines scanning the same project
    // produce the same tree and therefore the same skyline.
    subdirs.sort_by(|a, b| a.1.cmp(&b.1));

    for (path, child_name) in subdirs {
        if *budget == 0 {
            break;
        }
        let child_rel = join_rel(rel, &child_name);
        let child = survey(&path, &child_name, &child_rel, depth - 1, budget);
        if !child.is_empty() {
            district.children.push(child);
        }
    }

    district
}

fn join_rel(rel: &str, name: &str) -> String {
    if rel.is_empty() {
        name.to_string()
    } else {
        format!("{rel}/{name}")
    }
}
