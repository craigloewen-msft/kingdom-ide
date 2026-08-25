//! Materialising a proving ground on disk.
//!
//! The mirror image of [`crate::scan`]: that turns a folder into a `Kingdom`,
//! this turns a [`FixtureSpec`] into a folder. Server-only, and deliberately
//! the *only* place in the codebase that creates or deletes files.
//!
//! # Why write real files at all
//!
//! It would be less work to fabricate a `Vec<City>` in memory. That would test
//! a code path that never runs: `scan.rs`'s depth cap, its per-folder pruning
//! into `extra_files`, its `SKIP_DIRS` and its marker sniffing are exactly the
//! behaviour worth rehearsing, and faking above them means they are first
//! exercised by a real monorepo on the user's machine. Writing real bytes also
//! gives a future agent with hands somewhere to actually act.
//!
//! # The safety rule
//!
//! This module can delete directories, so it has exactly one inviolable rule:
//! **nothing is cleared or written into unless it is empty or carries the
//! [`MARKER`] file.** No flag overrides it. See [`ensure_seedable`].

use kingdom_core::mockdata::{FileContent, GitSpec, PlannedFile, FixtureSpec};
use std::io::Write;
use std::path::{Path, PathBuf};

/// Marks a directory as a proving ground, and therefore safe to destroy.
pub const MARKER: &str = ".kingdom-mock";

/// Where fixtures are seeded unless told otherwise.
///
/// In the King's profile ([`crate::profile`]), so a realm belongs to the person
/// rather than to the directory the server happened to be launched from -- the
/// old default was relative, which meant `cargo leptos serve` from one folder
/// and from another disagreed about which proving grounds existed. Not under
/// `/tmp`, so a fixture survives a reboot and can be poked at with ordinary
/// tools; not under `target/`, so `cargo clean` cannot delete it mid-
/// investigation.
pub fn sandbox_root() -> PathBuf {
    match std::env::var("KINGDOM_SANDBOX_ROOT") {
        Ok(p) if !p.trim().is_empty() => PathBuf::from(p.trim()),
        _ => crate::profile::home().join("realms"),
    }
}

/// Where a named fixture is seeded by default.
pub fn fixture_path(name: &str) -> PathBuf {
    sandbox_root().join(name)
}

/// Files above this size are created sparsely rather than written.
///
/// A sparse file reports its full length to `metadata().len()` -- which is what
/// the scanner reads and the skyline draws -- while costing almost nothing on
/// disk. That is what lets a fixture hold a 40 MB asset, the exact case behind
/// the tested "assets never outweigh code" invariant, for kilobytes.
const SPARSE_ABOVE: u64 = 64 * 1024;

#[derive(Debug)]
pub enum SeedError {
    /// The fixture itself is malformed. Author error, caught before any write.
    Spec(Vec<kingdom_core::mockdata::SpecError>),
    /// The target exists and is not a proving ground. **Never overridable.**
    NotAProvingGround(PathBuf),
    Io(std::io::Error),
}

impl std::fmt::Display for SeedError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SeedError::Spec(errors) => {
                write!(f, "realm is not seedable: ")?;
                let joined: Vec<String> = errors.iter().map(|e| e.to_string()).collect();
                f.write_str(&joined.join("; "))
            }
            SeedError::NotAProvingGround(path) => write!(
                f,
                "{} exists and is not a proving ground (no {MARKER} file). \
                 Refusing to touch it -- delete it yourself if you meant to.",
                path.display()
            ),
            SeedError::Io(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for SeedError {}

impl From<std::io::Error> for SeedError {
    fn from(e: std::io::Error) -> Self {
        SeedError::Io(e)
    }
}

/// What a seed produced, for the CLI to print.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeedReport {
    pub fixture: String,
    pub root: PathBuf,
    pub cities: usize,
    pub files: usize,
    /// Total *declared* size. Sparse files mean far less is really occupied.
    pub declared_bytes: u64,
    /// Cities that were put under git, if git was available.
    pub repos: usize,
}

impl std::fmt::Display for SeedReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Seeded '{}' at {}\n  {} cities, {} files, {} declared ({} git repos)",
            self.fixture,
            self.root.display(),
            self.cities,
            self.files,
            human_bytes(self.declared_bytes),
            self.repos,
        )
    }
}

/// Whether a directory is a proving ground we are allowed to destroy.
pub fn is_proving_ground(path: &Path) -> bool {
    path.join(MARKER).is_file()
}

/// The gate that stands between this module and someone's real work.
///
/// A directory may be written into only if it does not exist, is empty, or
/// carries [`MARKER`]. Deliberately has no `force` parameter: the caller's
/// `--force` means "re-seed this proving ground", and there is intentionally no
/// way to express "overwrite whatever is there" -- the escape hatch is to delete
/// the directory by hand, which cannot happen by accident from inside the app.
fn ensure_seedable(path: &Path) -> Result<(), SeedError> {
    if !path.exists() {
        return Ok(());
    }

    if is_proving_ground(path) {
        return Ok(());
    }

    // An empty directory is harmless to adopt, and is what a user who made the
    // folder themselves before seeding into it will have.
    let empty = std::fs::read_dir(path)
        .map(|mut entries| entries.next().is_none())
        .unwrap_or(false);
    if empty {
        return Ok(());
    }

    Err(SeedError::NotAProvingGround(path.to_path_buf()))
}

/// Materialises a fixture at `into`, clearing any previous seeding of it.
pub fn seed(spec: &FixtureSpec, into: &Path) -> Result<SeedReport, SeedError> {
    // Validate before touching anything: a fixture that fails halfway through
    // leaves a folder that looks plausible and is not, which is worse than no
    // folder at all.
    spec.validate().map_err(SeedError::Spec)?;
    ensure_seedable(into)?;

    if into.exists() {
        std::fs::remove_dir_all(into)?;
    }
    std::fs::create_dir_all(into)?;

    // The marker goes down before any content, so an interrupted seed still
    // leaves a directory that is safe to clear on the next attempt.
    write_marker(spec, into)?;

    let planned = spec.expand();
    for file in &planned {
        materialise(into, file)?;
    }

    let mut repos = 0;
    for city in &spec.cities {
        if let GitSpec::Repo { dirty } = city.git {
            if init_repo(&into.join(&city.name), dirty).is_ok() {
                repos += 1;
            }
        }
    }

    Ok(SeedReport {
        fixture: spec.name.to_string(),
        root: into.to_path_buf(),
        cities: spec.cities.len(),
        files: planned.len(),
        declared_bytes: planned.iter().map(|f| f.bytes).sum(),
        repos,
    })
}

fn write_marker(spec: &FixtureSpec, root: &Path) -> Result<(), SeedError> {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    std::fs::write(
        root.join(MARKER),
        format!(
            "# Kingdom IDE proving ground -- generated, safe to delete.\n\
             fixture = {}\nseed = {:#x}\ngenerated_at = {stamp}\n\n\
             Every file under this directory is fake. Edit\n\
             crates/kingdom-core/src/mockdata/fixtures.rs and re-seed to change it.\n",
            spec.name, spec.seed
        ),
    )?;
    Ok(())
}

fn materialise(root: &Path, file: &PlannedFile) -> Result<(), SeedError> {
    let path = root.join(&file.city).join(&file.path);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    match &file.content {
        FileContent::Literal(text) => std::fs::write(&path, text)?,
        FileContent::Bulk(n) if *n > SPARSE_ABOVE => {
            // Sparse: the scanner sees the full length, the disk sees ~nothing.
            let handle = std::fs::File::create(&path)?;
            handle.set_len(*n)?;
        }
        FileContent::Bulk(n) => {
            let mut handle = std::fs::File::create(&path)?;
            handle.write_all(&filler(&file.path, *n))?;
        }
    }

    Ok(())
}

/// Plausible-looking content of an exact length.
///
/// Not random bytes: a fake project someone opens in an editor while debugging
/// the map should read as code rather than as line noise, and a model handed one
/// of these paths should not receive garbage.
fn filler(path: &str, len: u64) -> Vec<u8> {
    let name = path.rsplit('/').next().unwrap_or(path);
    let comment = match path.rsplit('.').next().unwrap_or("") {
        "py" | "sh" | "rb" | "toml" | "yaml" | "yml" => "#",
        "md" => ">",
        _ => "//",
    };

    let header = format!("{comment} {name} -- generated fixture, not real code.\n");
    let mut out = header.into_bytes();
    out.truncate(len as usize);

    let line = format!("{comment} ").into_bytes();
    let mut n = 0usize;
    while (out.len() as u64) < len {
        let remaining = len as usize - out.len();
        if remaining <= line.len() + 1 {
            out.extend(std::iter::repeat_n(b'\n', remaining));
            break;
        }
        out.extend_from_slice(&line);
        let body = format!("line {n}");
        let take = body.len().min(remaining - line.len() - 1);
        out.extend_from_slice(&body.as_bytes()[..take]);
        out.push(b'\n');
        n += 1;
    }

    out
}

/// Puts a city under git, then leaves `dirty` files modified.
///
/// Best-effort: a machine without git degrades to a city that simply is not a
/// repository, which the model already represents. Failing the whole seed over
/// it would make the proving grounds unusable in a container for no gain.
///
/// `dirty_files` is not read by the scanner yet (it is hardcoded to 0 pending
/// real git status), so this exists so that when it *is*, the fixture already
/// has honest data to report rather than needing to be revisited.
fn init_repo(city: &Path, dirty: usize) -> std::io::Result<()> {
    let git = |args: &[&str]| -> std::io::Result<bool> {
        let out = std::process::Command::new("git")
            .args(args)
            .current_dir(city)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()?;
        Ok(out.success())
    };

    git(&["init", "--quiet"])?;
    // Local identity, so seeding works where no global git identity is set.
    git(&["config", "user.email", "court@kingdom.invalid"])?;
    git(&["config", "user.name", "The Court"])?;
    git(&["add", "-A"])?;
    git(&["commit", "--quiet", "-m", "Found the city"])?;

    for path in dirtiable(city).into_iter().take(dirty) {
        let mut handle = std::fs::OpenOptions::new().append(true).open(path)?;
        writeln!(handle, "\n// uncommitted change")?;
    }

    Ok(())
}

/// Files worth leaving modified, in a stable order.
///
/// Prefers files inside subfolders over those at the city root: the root holds
/// the manifests (`Cargo.toml`, `package.json`) that `detect_kind` sniffs and a
/// model might read, and appending a comment to those would leave the fixture
/// holding syntactically broken manifests for no benefit.
fn dirtiable(city: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    collect_files(city, &mut found, 3);
    found.sort_by_key(|p| {
        let depth = p
            .strip_prefix(city)
            .map(|r| r.components().count())
            .unwrap_or(1);
        (depth < 2, p.clone())
    });
    found
}

fn collect_files(dir: &Path, out: &mut Vec<PathBuf>, depth: usize) {
    if depth == 0 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with('.') {
            continue;
        }
        match entry.file_type() {
            Ok(t) if t.is_dir() => collect_files(&path, out, depth - 1),
            Ok(t) if t.is_file() => out.push(path),
            _ => {}
        }
    }
}

fn human_bytes(n: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut value = n as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{n} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scratch directory that cleans itself up.
    struct Temp(PathBuf);

    impl Temp {
        fn new(tag: &str) -> Self {
            let unique = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let path = std::env::temp_dir().join(format!("kingdom-seed-{tag}-{unique}"));
            std::fs::create_dir_all(&path).unwrap();
            Temp(path)
        }
    }

    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// **The test that stands between this feature and destroying real work.**
    ///
    /// The seeder deletes directories. Everything else here is a convenience;
    /// this is the safety property. A directory holding anything the seeder did
    /// not write must be refused outright, and refused *before* a single byte
    /// moves -- a check that runs after a partial clear would be no check at
    /// all.
    #[test]
    fn refuses_to_seed_over_a_directory_it_did_not_create() {
        let temp = Temp::new("guard");
        let precious = temp.0.join("NOT_A_FIXTURE.txt");
        std::fs::write(&precious, "a user's real work").unwrap();

        // Any bundled fixture will do; which one is not what is under test
        // here.
        let fixture = kingdom_core::mockdata::fixture("kingdom-mirror").expect("bundled realm");

        let refused = seed(&fixture, &temp.0);
        assert!(
            matches!(refused, Err(SeedError::NotAProvingGround(_))),
            "an unmarked directory must be refused, got {refused:?}"
        );
        assert_eq!(
            std::fs::read_to_string(&precious).unwrap(),
            "a user's real work",
            "the refused seed must not have touched anything"
        );

        // Marked, the same directory is fair game -- which is what makes
        // re-seeding safe rather than special-cased.
        std::fs::write(temp.0.join(MARKER), "realm = contended\n").unwrap();
        seed(&fixture, &temp.0).expect("a marked directory may be re-seeded");
        assert!(!precious.exists(), "re-seeding a proving ground clears it");
    }

    /// Proves the whole chain is honest, end to end.
    ///
    /// A fixture declaring `CityKind::Rust` means nothing unless the *real*
    /// scanner independently infers Rust from the `Cargo.toml` that was
    /// actually written. Same for file counts: this is what catches the seeder
    /// silently dropping files, or `Fill` expanding to fewer than it claims.
    #[test]
    fn a_seeded_fixture_scans_back_as_it_was_declared() {
        let temp = Temp::new("roundtrip");
        let root = temp.0.join("kingdom-mirror");
        let fixture = kingdom_core::mockdata::fixture("kingdom-mirror").expect("bundled realm");

        let report = seed(&fixture, &root).expect("seeding a fresh directory");
        let scanned = crate::scan::scan_kingdom(&root).expect("scanning the seeded realm");

        assert_eq!(
            scanned.len(),
            fixture.cities.len(),
            "every declared city must scan back as a city"
        );

        for declared in &fixture.cities {
            let found = scanned
                .iter()
                .find(|c| c.name == declared.name)
                .unwrap_or_else(|| panic!("city {} is missing after scanning", declared.name));

            assert_eq!(
                found.kind, declared.stack,
                "city {} was written with markers for {:?} but scanned as {:?}",
                declared.name, declared.stack, found.kind
            );

            let expected = fixture
                .expand()
                .iter()
                .filter(|f| f.city == declared.name)
                .count();
            assert_eq!(
                found.file_count, expected,
                "city {} declared {expected} files but scanned {}",
                declared.name, found.file_count
            );

            assert_eq!(
                found.has_git,
                declared.git != GitSpec::None && report.repos > 0,
                "git presence for {} disagrees with the spec",
                declared.name
            );
        }
    }
}
