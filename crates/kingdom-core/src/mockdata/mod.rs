//! The Proving Grounds: synthetic dev folders, defined in Rust.
//!
//! # Why this exists
//!
//! Kingdom IDE is meant to be pointed at itself -- the user issues prompts and
//! the model builds the next feature. Doing that against a *real* dev folder
//! means every rehearsal touches real projects, real git repos and real files.
//! This module is the alternative: fixtures that look and behave like real dev
//! folders and are provably not one.
//!
//! # How to change the fake data
//!
//! Everything is plain Rust. Open [`fixtures`] in `mockdata/fixtures.rs`, edit
//! a fixture or add a function returning a [`FixtureSpec`], and list it in
//! [`fixtures()`]. There is no config file and no parser: the types *are* the
//! schema, so a mistyped fixture fails to compile rather than failing at seed
//! time, and `CityKind`/`Language` are the real enums rather than strings that
//! have to be matched back to them.
//!
//! ```no_run
//! # use kingdom_core::mockdata::{FixtureSpec, build::*, starter_plans};
//! # use kingdom_core::Language;
//! fn my_fixture() -> FixtureSpec {
//!     FixtureSpec::new("my-fixture", "What it is for.", 0x5EED)
//!         .starter_plans(starter_plans::default_plans)
//!         .city(
//!             rust_city("orchard")
//!                 .dir("src", [
//!                     file("main.rs", 4_200),
//!                     fill("module_{i}.rs", 24, 1_500..12_000, Language::Rust),
//!                 ])
//!                 .dirty(3),
//!         )
//! }
//! ```
//!
//! # The shape of the pipeline
//!
//! This module is pure: it computes *what* a fixture contains, never writing a
//! byte. [`FixtureSpec::expand`] turns a fixture into a flat list of
//! [`PlannedFile`]s and `kingdom-app`'s seeder does nothing but write them down,
//! after which the **ordinary scanner** reads the result. That last part is the
//! point: the fixture exercises `scan.rs` for real rather than faking a
//! `Vec<City>` above it.

pub mod build;
pub mod fixtures;
pub mod starter_plans;

use crate::model::{City, CityKind, Language, Plan};
use std::ops::Range;

pub use build::{docs_city, file, fill, node_city, python_city, rust_city, text};
pub use fixtures::{fixture, fixture_names, fixtures, DEFAULT_FIXTURE};

/// How a fixture's opening model is fabricated.
///
/// A plain `fn` pointer over the signature `sample::starter_plans` already
/// has, rather than a new data format: a model is a handful of plans, and
/// expressing that as data would need a validator to say what the type system
/// already says.
pub type StarterPlansFn = fn(&[City]) -> Vec<Plan>;

/// A whole synthetic dev folder.
#[derive(Debug, Clone)]
pub struct FixtureSpec {
    /// Folder name, and the kingdom's display name.
    pub name: &'static str,
    /// One line shown by the seeder and the picker.
    pub blurb: &'static str,
    /// Everything generated is a pure function of this plus the spec, so two
    /// machines seeding the same fixture produce byte-identical folders.
    pub seed: u64,
    pub cities: Vec<CitySpec>,
    /// How this fixture's opening model is fabricated.
    pub starter_plans: StarterPlansFn,
}

impl FixtureSpec {
    pub fn new(name: &'static str, blurb: &'static str, seed: u64) -> Self {
        Self {
            name,
            blurb,
            seed,
            cities: Vec::new(),
            starter_plans: starter_plans::default_plans,
        }
    }

    pub fn starter_plans(mut self, starter_plans: StarterPlansFn) -> Self {
        self.starter_plans = starter_plans;
        self
    }

    pub fn city(mut self, city: CitySpec) -> Self {
        self.cities.push(city);
        self
    }

    pub fn cities(mut self, cities: impl IntoIterator<Item = CitySpec>) -> Self {
        self.cities.extend(cities);
        self
    }

    /// Every file this fixture will contain, in a stable order.
    ///
    /// Pure and total: two calls on the same spec return identical paths *and*
    /// sizes, on any machine. See [`FileContent::Bulk`] for why sizes are drawn
    /// per-file rather than from a rolling stream.
    pub fn expand(&self) -> Vec<PlannedFile> {
        let mut out = Vec::new();
        for city in &self.cities {
            city.expand_into(self.seed, &mut out);
        }
        out
    }

    /// Total declared size of the fixture, before sparse files save the disk.
    pub fn total_bytes(&self) -> u64 {
        self.expand().iter().map(|f| f.bytes).sum()
    }

    /// Rejects a fixture that cannot be seeded, before a byte is written.
    ///
    /// These are all author mistakes rather than user input, but they are
    /// miserable to diagnose from a half-materialised folder -- an escaping
    /// path in particular would write *outside* the sandbox, which is the one
    /// thing this whole feature exists to prevent.
    pub fn validate(&self) -> Result<(), Vec<SpecError>> {
        let mut errors = Vec::new();

        if self.name.trim().is_empty() || !is_safe_segment(self.name) {
            errors.push(SpecError {
                fixture: self.name,
                city: None,
                detail: format!("realm name {:?} is not a safe folder name", self.name),
            });
        }

        if self.cities.is_empty() {
            errors.push(SpecError {
                fixture: self.name,
                city: None,
                detail: "a realm with no cities would open as an empty map".into(),
            });
        }

        let mut seen_cities = std::collections::HashSet::new();
        for city in &self.cities {
            if !seen_cities.insert(city.name.as_str()) {
                errors.push(SpecError {
                    fixture: self.name,
                    city: Some(city.name.clone()),
                    detail: "two cities share a name, so one would overwrite the other".into(),
                });
            }
            city.validate(self.name, &mut errors);
        }

        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }
}

/// One project directory inside a fixture.
#[derive(Debug, Clone)]
pub struct CitySpec {
    pub name: String,
    /// What the stack *should* come out as.
    ///
    /// Never handed to the scanner: the builders write the corresponding marker
    /// files (`Cargo.toml`, `package.json`, ...) and `scan::detect_kind` infers
    /// the stack from those by the same rules it uses on a real project. This
    /// field is what the round-trip test asserts the scanner arrived at.
    pub stack: CityKind,
    pub git: GitSpec,
    pub tree: Vec<TreeSpec>,
}

impl CitySpec {
    pub fn new(name: impl Into<String>, stack: CityKind) -> Self {
        Self {
            name: name.into(),
            stack,
            git: GitSpec::Repo { dirty: 0 },
            tree: Vec::new(),
        }
    }

    /// Adds entries at the city root.
    pub fn files(mut self, entries: impl IntoIterator<Item = TreeSpec>) -> Self {
        self.tree.extend(entries);
        self
    }

    /// Adds a folder holding the given entries.
    pub fn dir(
        mut self,
        name: impl Into<String>,
        entries: impl IntoIterator<Item = TreeSpec>,
    ) -> Self {
        self.tree.push(TreeSpec::Dir {
            name: name.into(),
            children: entries.into_iter().collect(),
        });
        self
    }

    /// Leaves `n` files uncommitted, so `dirty_files` is honestly derived.
    pub fn dirty(mut self, n: usize) -> Self {
        self.git = GitSpec::Repo { dirty: n };
        self
    }

    /// Not a git repository. Worth having in a fixture: `has_git: false`
    /// changes what the map draws, so it must be reachable.
    pub fn no_git(mut self) -> Self {
        self.git = GitSpec::None;
        self
    }

    fn expand_into(&self, fixture_seed: u64, out: &mut Vec<PlannedFile>) {
        for entry in &self.tree {
            entry.expand_into(fixture_seed, &self.name, "", out);
        }
    }

    fn validate(&self, fixture: &'static str, errors: &mut Vec<SpecError>) {
        let mut push = |detail: String| {
            errors.push(SpecError {
                fixture,
                city: Some(self.name.clone()),
                detail,
            })
        };

        if !is_safe_segment(&self.name) {
            push(format!(
                "city name {:?} is not a safe folder name",
                self.name
            ));
        }

        let mut planned = Vec::new();
        for entry in &self.tree {
            entry.expand_into(0, &self.name, "", &mut planned);
        }

        let mut seen = std::collections::HashSet::new();
        for f in &planned {
            if !seen.insert(f.path.clone()) {
                push(format!("duplicate path {:?}", f.path));
            }
            if let Err(why) = check_relative(&f.path) {
                push(format!("path {:?}: {why}", f.path));
            }
        }

        // A `Fill` without `{i}` collapses N files into one, which looks like a
        // scanner bug rather than a typo when the map comes out wrong.
        for entry in &self.tree {
            entry.check_fills(&mut push);
        }
    }
}

/// Whether a city is a git repository, and how dirty.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GitSpec {
    None,
    Repo {
        /// Files left modified after the initial commit.
        dirty: usize,
    },
}

/// One entry in a city's tree.
#[derive(Debug, Clone)]
pub enum TreeSpec {
    File {
        path: String,
        content: FileContent,
    },
    Dir {
        name: String,
        children: Vec<TreeSpec>,
    },
    /// `count` generated files from a pattern containing `{i}`, each sized from
    /// `bytes`. This is what makes a 5,000-file city one line.
    Fill {
        pattern: String,
        count: usize,
        bytes: Range<u64>,
        language: Language,
    },
}

impl TreeSpec {
    fn expand_into(&self, fixture_seed: u64, city: &str, prefix: &str, out: &mut Vec<PlannedFile>) {
        match self {
            TreeSpec::File { path, content } => {
                let full = join(prefix, path);
                let bytes = match content {
                    FileContent::Literal(text) => text.len() as u64,
                    FileContent::Bulk(n) => *n,
                };
                out.push(PlannedFile {
                    city: city.to_string(),
                    path: full,
                    bytes,
                    content: content.clone(),
                });
            }
            TreeSpec::Dir { name, children } => {
                let child_prefix = join(prefix, name);
                for child in children {
                    child.expand_into(fixture_seed, city, &child_prefix, out);
                }
            }
            TreeSpec::Fill {
                pattern,
                count,
                bytes,
                language: _,
            } => {
                for i in 0..*count {
                    let path = join(prefix, &pattern.replace("{i}", &i.to_string()));
                    // Seeded per file, from the path itself -- see the note on
                    // `FileContent::Bulk`.
                    let size = size_in(fixture_seed, city, &path, bytes);
                    out.push(PlannedFile {
                        city: city.to_string(),
                        path,
                        bytes: size,
                        content: FileContent::Bulk(size),
                    });
                }
            }
        }
    }

    fn check_fills(&self, push: &mut impl FnMut(String)) {
        match self {
            TreeSpec::Fill {
                pattern,
                count,
                bytes,
                ..
            } => {
                if !pattern.contains("{i}") {
                    push(format!(
                        "fill pattern {pattern:?} has no {{i}}, so its {count} files would collide"
                    ));
                }
                if bytes.is_empty() {
                    push(format!("fill pattern {pattern:?} has an empty size range"));
                }
            }
            TreeSpec::Dir { children, .. } => {
                for c in children {
                    c.check_fills(push);
                }
            }
            TreeSpec::File { .. } => {}
        }
    }
}

/// What a planned file is made of.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileContent {
    /// Exact bytes, for files whose contents matter -- a `Cargo.toml` the
    /// scanner sniffs, a README a model might read.
    Literal(String),
    /// A declared size, filled with generated filler.
    ///
    /// Sizes are drawn from a splitmix64 PRNG seeded with the fixture seed, the
    /// city name and the file's own path -- deliberately **per file**, never a
    /// rolling stream. A rolling stream would make each file's size depend on
    /// how many files came before it, so inserting one entry at the top of a
    /// city would silently resize the whole fixture and move every tower on the
    /// map. Per-file seeding means an edit changes exactly what it names.
    Bulk(u64),
}

/// One file the seeder will write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedFile {
    pub city: String,
    /// Path relative to the city root.
    pub path: String,
    pub bytes: u64,
    pub content: FileContent,
}

/// An author mistake in a fixture, caught before anything is written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpecError {
    pub fixture: &'static str,
    pub city: Option<String>,
    pub detail: String,
}

impl std::fmt::Display for SpecError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.city {
            Some(city) => write!(f, "{}/{}: {}", self.fixture, city, self.detail),
            None => write!(f, "{}: {}", self.fixture, self.detail),
        }
    }
}

// ---------------------------------------------------------------------------
// Deterministic generation
// ---------------------------------------------------------------------------

/// splitmix64: a tiny, well-distributed finaliser.
///
/// Written out rather than pulled in as a dependency, and used instead of
/// `DefaultHasher`, because the standard hasher is explicitly *not* stable
/// across Rust releases -- which would break fixture determinism silently, on a
/// toolchain upgrade, months after anyone touched this file.
pub(crate) fn splitmix64(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = x;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// Mixes a fixture seed with a city and path into one per-file seed.
pub(crate) fn file_seed(fixture_seed: u64, city: &str, path: &str) -> u64 {
    let mut h = splitmix64(fixture_seed);
    for byte in city
        .bytes()
        .chain(b"\0".iter().copied())
        .chain(path.bytes())
    {
        h = splitmix64(h ^ u64::from(byte));
    }
    h
}

fn size_in(fixture_seed: u64, city: &str, path: &str, range: &Range<u64>) -> u64 {
    if range.is_empty() {
        return range.start;
    }
    let span = range.end - range.start;
    range.start + (file_seed(fixture_seed, city, path) % span)
}

// ---------------------------------------------------------------------------
// Path safety
// ---------------------------------------------------------------------------

fn join(prefix: &str, name: &str) -> String {
    if prefix.is_empty() {
        name.to_string()
    } else {
        format!("{prefix}/{name}")
    }
}

/// Whether a single path segment is safe to create as a folder.
fn is_safe_segment(s: &str) -> bool {
    !s.is_empty()
        && s != "."
        && s != ".."
        && s != ".git"
        && !s.contains('/')
        && !s.contains('\\')
        && !s.contains('\0')
}

/// Whether a relative path stays inside its city.
///
/// The seeder joins these onto the sandbox root, so an absolute or `..`-walking
/// path would write outside the proving grounds entirely.
fn check_relative(path: &str) -> Result<(), &'static str> {
    if path.is_empty() {
        return Err("empty path");
    }
    if path.starts_with('/') || path.contains(':') {
        return Err("must be relative to the city root");
    }
    for segment in path.split('/') {
        if segment == ".." {
            return Err("escapes the city with ..");
        }
        if segment.is_empty() || segment == "." {
            return Err("has an empty or '.' segment");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec_with(extra: Option<TreeSpec>) -> FixtureSpec {
        let mut orchard = CitySpec::new("orchard", CityKind::Rust).dir(
            "src",
            [TreeSpec::Fill {
                pattern: "module_{i}.rs".into(),
                count: 12,
                bytes: 1_000..9_000,
                language: Language::Rust,
            }],
        );
        if let Some(entry) = extra {
            orchard.tree.insert(0, entry);
        }

        FixtureSpec::new("t", "test realm", 0xABCD).cities([
            orchard,
            CitySpec::new("lantern", CityKind::Node).dir(
                "src",
                [TreeSpec::Fill {
                    pattern: "view_{i}.ts".into(),
                    count: 9,
                    bytes: 500..4_000,
                    language: Language::Web,
                }],
            ),
        ])
    }

    /// Determinism, and the *scope* of a change, are what make the proving
    /// grounds usable as a fixture at all.
    ///
    /// `AGENTS.md` demands deterministic layout so the user's spatial memory
    /// works; that guarantee is worthless if the data underneath the map
    /// reshuffles. The second half is the subtler half: sizes are drawn
    /// per-file so that inserting one entry changes *only* that city. If
    /// someone "simplifies" the PRNG into a rolling stream, every edit to a
    /// fixture silently resizes every file after it and moves every tower on
    /// the map -- a diff of one line producing a completely different picture.
    #[test]
    fn expansion_is_deterministic_and_edits_stay_local() {
        let before = spec_with(None);
        assert_eq!(
            before.expand(),
            before.expand(),
            "expanding the same realm twice must give identical paths and sizes"
        );

        let after = spec_with(Some(TreeSpec::File {
            path: "NOTES.md".into(),
            content: FileContent::Literal("new".into()),
        }));

        let untouched = |spec: &FixtureSpec| -> Vec<PlannedFile> {
            spec.expand()
                .into_iter()
                .filter(|f| f.city == "lantern")
                .collect()
        };
        assert_eq!(
            untouched(&before),
            untouched(&after),
            "editing one city must not resize files in another"
        );

        // And within the edited city, the pre-existing files keep their sizes.
        let sized = |spec: &FixtureSpec| -> Vec<(String, u64)> {
            spec.expand()
                .into_iter()
                .filter(|f| f.city == "orchard" && f.path != "NOTES.md")
                .map(|f| (f.path, f.bytes))
                .collect()
        };
        assert_eq!(
            sized(&before),
            sized(&after),
            "inserting a file must not resize its siblings"
        );
    }

    /// Validation is the last thing standing between a typo and a write outside
    /// the sandbox, so the escaping cases specifically must be caught.
    #[test]
    fn validate_rejects_paths_that_would_escape_the_city() {
        let fixture =
            FixtureSpec::new("t", "", 1).city(CitySpec::new("c", CityKind::Unknown).files([
                TreeSpec::File {
                    path: "../../etc/passwd".into(),
                    content: FileContent::Bulk(1),
                },
                TreeSpec::File {
                    path: "/etc/hosts".into(),
                    content: FileContent::Bulk(1),
                },
            ]));

        let errors = fixture
            .validate()
            .expect_err("escaping paths must be refused");
        assert_eq!(errors.len(), 2, "both escaping paths should be reported");
    }

    /// Every bundled fixture must actually be seedable. This is the cheap guard
    /// that stops a broken fixture reaching the seeder, where the failure would
    /// look like an I/O bug rather than a typo.
    #[test]
    fn every_bundled_fixture_is_valid() {
        for fixture in fixtures() {
            if let Err(errors) = fixture.validate() {
                panic!(
                    "realm {} is not seedable: {}",
                    fixture.name,
                    errors
                        .iter()
                        .map(|e| e.to_string())
                        .collect::<Vec<_>>()
                        .join("; ")
                );
            }
        }
    }
}
