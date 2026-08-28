//! The well: a container a whole city shares, declared in its own manifest.
//!
//! # The problem this answers
//!
//! Network isolation stops five agents fighting over `:3000`. But a project's
//! database is the opposite kind of resource: not a collision to prevent, but a
//! common good every agent must reach, started once and torn down once. Without
//! it, five plans on one project either start five databases or share one by
//! accident.
//!
//! # Why the manifest lives in the project
//!
//! At `<city>/.kingdom/services.toml`, committed. It describes what *that
//! project* needs in order to run, which is a fact about the project rather
//! than about the King's machine -- so it travels with the repository.
//!
//! # And why there is a second one that does not
//!
//! Some wells are not any project's business: one Redis the King keeps for
//! whatever he is poking at this week, shared by every city he opens. That is a
//! fact about *his machine*, so it lives in his profile at
//! `$KINGDOM_HOME/services.toml` and is never committed anywhere. Same file
//! format, same parser, same everything below -- see [`ServiceScope`], which is
//! the only thing that distinguishes them.
//!
//! # Why parsing lives here
//!
//! `kingdom-core` compiles to wasm and does no I/O, so this module takes a
//! `&str` and never opens a file. That is what lets the whole parse be tested
//! without a disk, a Docker daemon or a running server; `kingdom-app` reads the
//! bytes and calls [`parse`].

use crate::ids::CityId;
use serde::{Deserialize, Serialize};

use std::fmt::Write as _;

/// Where a city's manifest sits, relative to its root.
///
/// Named rather than inlined because three places must agree on it: the reader,
/// the git exclude rule that keeps it visible (a bare `.kingdom/` exclude would
/// otherwise hide it), and the fixture that writes one.
pub const MANIFEST_PATH: &str = ".kingdom/services.toml";

/// Where the King's own manifest sits, relative to his profile root.
///
/// `$KINGDOM_HOME/services.toml`, beside `settings.json`. Not inside a
/// `kingdoms/<key>/` folder, because a host well is offered to every kingdom he
/// opens rather than to one of them -- that is the whole difference between the
/// two scopes.
pub const HOST_MANIFEST_FILE: &str = "services.toml";

/// Which level a shared resource runs at.
///
/// The King picks this when he declares one, and it decides exactly two things:
/// which file the declaration is written to, and how far it is shared. Nothing
/// else about a well differs between the two -- same image, same network, same
/// reference count, same volume.
///
/// Deliberately carries **no** city. A scope is a kind, and the city a resource
/// belongs to is a separate fact that only exists for one of the two kinds; see
/// [`SharedResource::city`], which holds it where the answer is knowable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum ServiceScope {
    /// The King's machine. Declared in his profile, reachable from every
    /// project in every kingdom he opens.
    Host,
    /// One project. Declared in that project's repository and shared only by
    /// the plans working in it.
    City,
}

impl ServiceScope {
    /// What the King is shown as the heading a resource is filed under.
    pub fn label(&self) -> &'static str {
        match self {
            ServiceScope::Host => "The whole machine",
            ServiceScope::City => "This project",
        }
    }

    /// The stable string this scope is written as on a form or in a URL.
    pub fn wire_name(&self) -> &'static str {
        match self {
            ServiceScope::Host => "host",
            ServiceScope::City => "city",
        }
    }

    /// The inverse of [`Self::wire_name`], for a value coming back from a form.
    pub fn from_wire(name: &str) -> Option<Self> {
        match name {
            "host" => Some(ServiceScope::Host),
            "city" => Some(ServiceScope::City),
            _ => None,
        }
    }
}

/// Everything one city declares it needs standing.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServiceManifest {
    /// The services, in the order they were declared.
    #[serde(default, rename = "service")]
    pub services: Vec<ServiceSpec>,
    /// The folders a sealed plan is allowed to see, in the order they were
    /// declared.
    ///
    /// A **second kind** in the same file rather than a second file, because it
    /// answers the same question -- "what does this project need in order to
    /// run?" -- and the King already knows where to look. It is deliberately
    /// not a [`ServiceSpec`]: a folder has no image, no port and no container,
    /// and nothing downstream of a mount reaches Docker at all.
    ///
    /// Empty for every manifest written before this existed, and ignored
    /// entirely by a plan that is not sealed.
    #[serde(default, rename = "mount")]
    pub mounts: Vec<MountSpec>,
}

/// One folder a sealed plan may see.
///
/// # Why this is declared and not inferred
///
/// A sealed plan starts with a read-only system, its workspace and its git
/// directory -- enough to read and build most things, and not enough for a
/// toolchain the King keeps in his home directory. `~/.cargo` is the ordinary
/// case: without it `cargo` re-downloads its whole registry, and without
/// `~/.rustup` it re-downloads the toolchain itself. Measured, not guessed.
///
/// Kingdom will not go looking through his home directory and mount what it
/// finds. What a plan can see is his decision, written down, in a file he can
/// read back later -- the same judgement `ServiceSpec` makes about containers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MountSpec {
    /// The folder on the King's machine, absolute or beginning with `~`.
    ///
    /// It appears at the **same path** inside the plan, which is what makes a
    /// mounted toolchain work at all: `~/.cargo/bin/cargo` looks for its
    /// registry at `~/.cargo/registry`, and a folder mounted somewhere else
    /// would be a folder it cannot find.
    pub path: String,
    /// Whether the plan may write there. Read-only unless it says otherwise.
    ///
    /// Defaulting to read-only is the whole point of declaring these: a
    /// toolchain a plan can rewrite is one every *later* plan inherits the
    /// damage from. `rw` is nonetheless right for a cache -- see
    /// [`known_path`], where each well-known folder carries the mode it
    /// actually needs.
    #[serde(default)]
    pub mode: MountMode,
}

/// Whether a mounted folder is writable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MountMode {
    /// The plan may read it and not change it.
    #[default]
    Ro,
    /// The plan may write to it -- a cache, or a registry that fills itself in.
    Rw,
}

impl MountMode {
    pub fn is_writable(&self) -> bool {
        matches!(self, MountMode::Rw)
    }

    /// How this is written in a manifest.
    pub fn wire_name(&self) -> &'static str {
        match self {
            MountMode::Ro => "ro",
            MountMode::Rw => "rw",
        }
    }
}

impl MountSpec {
    /// This mount as the `[[mount]]` block a person would have typed.
    ///
    /// Rendered and appended for the same reason [`ServiceSpec::render`] is:
    /// the manifest is a file people comment, and re-serialising the document
    /// would eat every comment in it.
    pub fn render(&self) -> String {
        let mut out = String::from("[[mount]]\n");
        let _ = writeln!(out, "path = {}", toml_string(&self.path));
        let _ = writeln!(out, "mode = {}", toml_string(self.mode.wire_name()));
        out
    }

    /// The path with a leading `~` replaced by the given home directory.
    ///
    /// Expansion happens at *use* rather than at parse, so what the King wrote
    /// is what he reads back -- and a manifest that travels between machines,
    /// as a committed project manifest does, still means "this user's home"
    /// wherever it lands.
    pub fn expanded(&self, home: &str) -> String {
        match self.path.strip_prefix('~') {
            Some(rest) => format!("{}{}", home.trim_end_matches('/'), rest),
            None => self.path.clone(),
        }
    }
}

/// One container the city shares.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServiceSpec {
    /// What the King calls it, and half of the container's identity. Unique
    /// within a manifest.
    pub name: String,
    /// The image to run, tag included.
    pub image: String,
    /// The port the service listens on *inside* the container -- and the port
    /// an agent reaches it on, because that is the whole promise: a relay puts
    /// the container on the plan's own loopback at this same number.
    pub port: u16,
    /// A named Docker volume for the service's data, kept when the container is
    /// stopped.
    ///
    /// `None` means the data goes with the container. That is right for a cache
    /// and wrong for a database, so it is stated per service rather than
    /// assumed either way.
    #[serde(default)]
    pub volume: Option<String>,
    /// `env`, kept only so that a manifest still carrying it can be **refused**
    /// by name rather than silently ignored.
    ///
    /// Serde drops an unknown key without a word. Kingdom used to hand these
    /// variables to every command a plan ran, so a project that still declares
    /// them would otherwise believe its agents get `$DATABASE_URL` while
    /// nothing sets it. Never read for its contents -- only for whether it is
    /// there. See [`ManifestError::Retired`].
    ///
    /// Skipped when serialising, so nothing Kingdom writes can ever contain it.
    #[serde(default, rename = "env", skip_serializing)]
    pub retired_env: Option<RetiredField>,
}

/// A field that is accepted by the parser only so it can be rejected by name.
///
/// Deserialises from **any** TOML value and keeps none of it: the only question
/// asked of it is whether the key was present. A marker rather than a
/// `toml::Value` for two reasons -- a `Value` holds floats and so is not `Eq`,
/// which every type in this module is, and keeping the contents would invite
/// somebody to start honouring them again.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
pub struct RetiredField;

impl<'de> Deserialize<'de> for RetiredField {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        serde::de::IgnoredAny::deserialize(deserializer)?;
        Ok(RetiredField)
    }
}

/// What Kingdom knows about a well-known image without being told.
///
/// # Why a table rather than more fields on the form
///
/// The King should not have to know Postgres's port, where Mongo keeps its
/// files, or that `postgres:16` refuses to boot without a password, in order to
/// share a database. Every one of those is a fact about the image rather than a
/// decision about his project, and a form that asks for facts it could look up
/// is a form that can be got wrong.
///
/// Deliberately small and deliberately here: `kingdom-core` does no I/O, so the
/// whole table is tested without a daemon, and one table means the port the
/// form fills in and the data directory the volume is mounted at cannot
/// disagree about what `mongo` is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KnownImage {
    /// The port it listens on, which the form offers and an agent reaches it
    /// on.
    pub port: u16,
    /// Where it keeps its data, so a named volume is mounted somewhere useful.
    pub data_dir: &'static str,
    /// What it needs in its **own** environment in order to start at all.
    ///
    /// Container-facing, and the opposite direction of travel from the `env`
    /// this change deletes: nothing here is ever shown to an agent. `postgres`
    /// exits 1 without a password -- measured, not assumed -- so a King who
    /// typed `postgres:16` and nothing else would otherwise get a resource that
    /// never comes up.
    pub boot: &'static [(&'static str, &'static str)],
}

/// What is known about an image, by its name with any tag and registry
/// stripped.
///
/// `None` for anything unrecognised, which is not a refusal: such an image runs
/// perfectly well, it just has to be told its port, and gets `/data` if it is
/// given a volume.
pub fn known_image(image: &str) -> Option<KnownImage> {
    let name = image.split(':').next().unwrap_or(image);
    let name = name.rsplit('/').next().unwrap_or(name);
    let known = |port, data_dir, boot| {
        Some(KnownImage {
            port,
            data_dir,
            boot,
        })
    };
    match name {
        "mongo" => known(27017, "/data/db", &[]),
        // Without this it exits 1 on first boot with a message about
        // POSTGRES_PASSWORD, and the King sees only "never answered on port
        // 5432". A fixed password is right here: nothing is published on his
        // loopback, and the container is reachable only from his own machine
        // and the plans he opens.
        "postgres" => known(
            5432,
            "/var/lib/postgresql/data",
            &[("POSTGRES_PASSWORD", "postgres")],
        ),
        "mysql" | "mariadb" => known(3306, "/var/lib/mysql", &[("MYSQL_ROOT_PASSWORD", "root")]),
        "redis" => known(6379, "/data", &[]),
        _ => None,
    }
}

/// Where an image keeps its data, for a named volume to be mounted at.
///
/// `/data` for anything unrecognised -- a guess, but a harmless one: a volume
/// mounted at the wrong path costs an empty directory, where no volume at all
/// costs the King his data.
pub fn data_dir_for(image: &str) -> &'static str {
    known_image(image).map_or("/data", |known| known.data_dir)
}

/// What Kingdom knows about a folder a toolchain keeps, without being told.
///
/// The counterpart to [`known_image`], and shaped like it deliberately: a small
/// table, here in `kingdom-core` so the whole of it is tested without a disk,
/// naming facts about a *tool* rather than decisions about the King's project.
///
/// # Why a `PATH` entry maps to a set
///
/// The obvious rule -- "share the directory the binary is in" -- is wrong for
/// most real toolchains, and quietly so. `~/.cargo/bin/cargo` on its own runs,
/// and then re-downloads the entire crate registry because `~/.cargo/registry`
/// is not there; without `~/.rustup` it re-downloads the toolchain itself.
/// Measured, from inside a real sealed namespace: it began syncing 1.97.1.
///
/// So each entry names every folder that tool needs, and the mode each one
/// needs it in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KnownPath {
    /// What the King is told this is for, in one phrase.
    pub why: &'static str,
    /// The folders, with the mode each needs.
    pub folders: &'static [(&'static str, MountMode)],
}

/// What is known about a `PATH` entry, by the folder it is.
///
/// `None` for anything unrecognised, which is not a refusal: such a folder is
/// still offered, read-only and unadorned. A tool Kingdom has never heard of is
/// still a tool the King has.
pub fn known_path(entry: &str) -> Option<KnownPath> {
    let entry = entry.trim_end_matches('/');
    // Matched on the tail rather than the whole path, because everything here
    // lives under a home directory whose name differs per machine.
    let known = |why, folders| Some(KnownPath { why, folders });
    match tail(entry).as_str() {
        ".cargo/bin" | ".cargo" => known(
            "Rust: cargo, rustc and the crate registry",
            // Both writable: the registry cache and the toolchain are filled in
            // as a build runs, and a read-only pair means every build
            // re-downloads everything it needs.
            &[("~/.cargo", MountMode::Rw), ("~/.rustup", MountMode::Rw)],
        ),
        ".local/bin" => known(
            "Locally installed tools -- uv, pipx and the like",
            &[("~/.local/bin", MountMode::Ro)],
        ),
        ".local/share/mise/shims" | ".local/share/mise" => known(
            "mise: the shims, and the versions they point at",
            &[("~/.local/share/mise", MountMode::Ro)],
        ),
        ".npm-global/bin" | ".npm-global" => known(
            "Globally installed npm packages",
            &[
                ("~/.npm-global", MountMode::Ro),
                // npm writes here whenever it resolves anything.
                ("~/.npm", MountMode::Rw),
            ],
        ),
        ".local/share/pnpm" => known(
            "pnpm, and its content-addressed store",
            &[("~/.local/share/pnpm", MountMode::Rw)],
        ),
        ".bun/bin" | ".bun" => known("Bun, and its cache", &[("~/.bun", MountMode::Rw)]),
        ".deno/bin" | ".deno" => known("Deno, and its cache", &[("~/.deno", MountMode::Rw)]),
        ".nvm/versions/node" | ".nvm" => known(
            "nvm, and every Node it manages",
            &[("~/.nvm", MountMode::Ro)],
        ),
        "go/bin" => known("Go, and its module cache", &[("~/go", MountMode::Rw)]),
        ".pyenv/bin" | ".pyenv/shims" | ".pyenv" => known(
            "pyenv, and every Python it manages",
            &[("~/.pyenv", MountMode::Ro)],
        ),
        _ => None,
    }
}

/// Folders worth offering that are not on `PATH` at all.
///
/// A tool's *configuration* is not somewhere a binary lives, so no amount of
/// reading `PATH` will find it -- and a plan that can run `git` but has no
/// `~/.gitconfig` commits as "unknown", which looks like a bug in the project
/// rather than a folder nobody shared.
///
/// Offered, never assumed: `~/.ssh` in particular is the King's private key,
/// and Kingdom will not hand that to an agent because it seemed convenient.
pub fn known_extras() -> &'static [(&'static str, &'static str, MountMode)] {
    &[
        (
            "~/.gitconfig",
            "Your git identity, so a plan's commits are yours",
            MountMode::Ro,
        ),
        (
            "~/.config/git",
            "Your git configuration, where it lives in a folder",
            MountMode::Ro,
        ),
        (
            "~/.ssh",
            "Your SSH keys -- only if a plan must push or pull over SSH",
            MountMode::Ro,
        ),
        ("~/.config/uv", "uv's configuration", MountMode::Ro),
        ("~/.cache/uv", "uv's package cache", MountMode::Rw),
    ]
}

/// The last one or two components of a path, which is what a known folder is
/// recognised by.
///
/// Two components rather than one because `bin` alone says nothing, and the
/// folder above it is the tool: `.cargo/bin`, `.npm-global/bin`. Three where
/// the tool nests deeper, which `mise` and `nvm` do.
fn tail(path: &str) -> String {
    let parts: Vec<&str> = path.split('/').filter(|p| !p.is_empty()).collect();
    for take in [4, 3, 2, 1] {
        if parts.len() >= take {
            let candidate = parts[parts.len() - take..].join("/");
            if candidate.starts_with('.') || candidate.starts_with("go/") {
                return candidate;
            }
        }
    }
    parts.last().map(|p| p.to_string()).unwrap_or_default()
}

/// One folder Kingdom offers to share with sealed plans.
///
/// Runtime truth, like [`SharedResource`]: it is answered by looking at the
/// King's actual machine and is never persisted. What he *chooses* becomes a
/// [`MountSpec`] in a manifest; this is only the offer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MountCandidate {
    /// The folders this offer would add, with the mode each needs. Several,
    /// because a toolchain is usually more than one -- see [`KnownPath`].
    pub folders: Vec<MountSpec>,
    /// What the King is told this is for.
    pub why: String,
    /// Where this offer is already declared, if it is: `None` for one not
    /// shared at all.
    ///
    /// # Why the scope and not a bare `bool`
    ///
    /// The panel this feeds is a set of **checkboxes**, and a box that can be
    /// ticked must be able to be unticked. Untick writes to the King's own
    /// profile, which is the only manifest that panel may edit -- a folder a
    /// *project* declared lives in a committed file belonging to whoever else
    /// works on it, and silently editing that from a picker would be Kingdom
    /// changing somebody's repository because a box was clicked.
    ///
    /// So "shared" and "shared somewhere I may unshare it" are two different
    /// facts, and a bool could carry only the first.
    #[serde(default)]
    pub declared: Option<ServiceScope>,
}

impl MountCandidate {
    /// Whether this offer is already shared, wherever it was declared.
    ///
    /// Kept as a method so the sites that only ask "is it taken?" read exactly
    /// as they did when this was a field.
    pub fn already(&self) -> bool {
        self.declared.is_some()
    }

    /// Whether this panel may withdraw it again.
    ///
    /// True only for the King's own profile. See [`Self::declared`].
    pub fn removable(&self) -> bool {
        matches!(self.declared, Some(ServiceScope::Host))
    }
}

/// Why a manifest could not be used.
///
/// Written for the King, who is the one who has to edit the file: every variant
/// names the service at fault where it can, because "invalid manifest" in a
/// file with four services is a search rather than a fix.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ManifestError {
    /// The TOML itself did not parse.
    Syntax(String),
    /// A field was empty that cannot be.
    Empty {
        service: String,
        field: &'static str,
    },
    /// Two services claimed the same name.
    DuplicateName(String),
    /// A name that cannot safely be part of a container name.
    BadName(String),
    /// A mount whose path cannot be used.
    ///
    /// Refused at parse time rather than at mount time, for the reason every
    /// other variant here is: a bad path reported when the file is read is one
    /// line to fix, where the same fault three minutes into a plan is a sealed
    /// namespace that half-built and a `pivot_root` failure naming nothing the
    /// King ever wrote.
    BadMount { path: String, why: &'static str },
    /// A field Kingdom used to honour and no longer does.
    ///
    /// Refused rather than ignored, which is the whole reason this variant
    /// exists. Serde drops an unknown key without a word, so a manifest still
    /// carrying `env` would leave a project believing it sets `$DATABASE_URL`
    /// for its agents while nothing does -- a silence that reads as a broken
    /// database an hour later. One line to delete, said once, is cheaper.
    Retired {
        service: String,
        field: &'static str,
    },
}

impl std::fmt::Display for ManifestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Deliberately **no path**. There are two manifests now -- a project's
        // and the King's -- so a message naming one of them was wrong half the
        // time; it said `.kingdom/services.toml` for a fault in his profile.
        // The reader supplies the real path, which it is the only one that
        // knows: see `services::ServiceError::Manifest`.
        match self {
            ManifestError::Syntax(detail) => {
                write!(f, "not valid TOML: {detail}")
            }
            ManifestError::Empty { service, field } => {
                write!(f, "service `{service}` has an empty `{field}`")
            }
            ManifestError::DuplicateName(name) => write!(
                f,
                "two services are called `{name}`; names must be unique because \
                 the name identifies the container"
            ),
            ManifestError::BadName(name) => write!(
                f,
                "`{name}` cannot be a service name: use letters, digits, `-` \
                 and `_` only"
            ),
            ManifestError::BadMount { path, why } => {
                write!(f, "`{path}` cannot be shared with a plan: {why}")
            }
            ManifestError::Retired { service, field } => write!(
                f,
                "service `{service}` sets `{field}`, which Kingdom no longer \
                 uses -- an agent reaches a shared resource at `localhost` on \
                 the service's own port, with nothing to configure. Remove \
                 that line."
            ),
        }
    }
}

impl std::error::Error for ManifestError {}

impl ServiceSpec {
    /// This service as the `[[service]]` block a person would have typed.
    ///
    /// # Why the UI writes text rather than a document
    ///
    /// The manifest is hand-written and full of comments -- the `shopfront`
    /// fixture's says what Kingdom does with it, in prose, above the block. A
    /// form that parsed the file into a `ServiceManifest`, pushed one entry and
    /// re-serialised would eat every one of those comments, silently, as the
    /// price of adding a service. So the form renders *one block* and the
    /// writer appends it, leaving everything already in the file untouched.
    ///
    /// The output is round-tripped through [`parse`] before it is written, so
    /// "the form produced something the parser refuses" is caught here rather
    /// than at the next plan's first turn.
    pub fn render(&self) -> String {
        let mut out = String::from("[[service]]\n");
        let _ = writeln!(out, "name  = {}", toml_string(&self.name));
        let _ = writeln!(out, "image = {}", toml_string(&self.image));
        let _ = writeln!(out, "port  = {}", self.port);
        if let Some(volume) = &self.volume {
            let _ = writeln!(out, "volume = {}", toml_string(volume));
        }
        out
    }
}

/// A TOML basic string, escaped.
///
/// Small and written out rather than reached for, because the only values that
/// pass through here are a name, an image tag and a connection URI -- and a
/// serializer would bring back the whole-document round trip that
/// [`ServiceSpec::render`] exists to avoid.
fn toml_string(value: &str) -> String {
    let mut out = String::from("\"");
    for c in value.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

impl ServiceManifest {
    /// Whether this city declares anything at all.
    ///
    /// The common case is a city with no manifest, and every caller checks this
    /// before spending a subprocess on Docker.
    pub fn is_empty(&self) -> bool {
        self.services.is_empty() && self.mounts.is_empty()
    }

    /// Whether this city declares any **container**.
    ///
    /// Distinct from [`Self::is_empty`] and the distinction matters: every
    /// caller that reaches for Docker wants this one. A manifest holding only
    /// mounts needs no daemon, and treating it as non-empty would refuse to
    /// open a project whose only declaration is a folder.
    pub fn has_services(&self) -> bool {
        !self.services.is_empty()
    }

    fn validate(&self) -> Result<(), ManifestError> {
        let mut seen: Vec<&str> = Vec::new();
        for service in &self.services {
            if service.name.trim().is_empty() {
                return Err(ManifestError::Empty {
                    service: "<unnamed>".to_string(),
                    field: "name",
                });
            }
            if !is_safe_name(&service.name) {
                return Err(ManifestError::BadName(service.name.clone()));
            }
            if service.image.trim().is_empty() {
                return Err(ManifestError::Empty {
                    service: service.name.clone(),
                    field: "image",
                });
            }
            if seen.contains(&service.name.as_str()) {
                return Err(ManifestError::DuplicateName(service.name.clone()));
            }
            seen.push(&service.name);

            // Retired fields, refused by name. See [`ManifestError::Retired`]:
            // ignoring one would be a silent promise nothing keeps.
            if service.retired_env.is_some() {
                return Err(ManifestError::Retired {
                    service: service.name.clone(),
                    field: "env",
                });
            }
        }

        for mount in &self.mounts {
            let path = mount.path.trim();
            if path.is_empty() {
                return Err(ManifestError::BadMount {
                    path: mount.path.clone(),
                    why: "it is empty",
                });
            }
            // Absolute or `~`-rooted only. A relative path has no meaning here:
            // there is no working directory a mount is resolved against, and
            // guessing one would silently share the wrong folder.
            if !path.starts_with('/') && !path.starts_with('~') {
                return Err(ManifestError::BadMount {
                    path: mount.path.clone(),
                    why: "a shared folder must be an absolute path, or start \
                          with `~` for your home directory",
                });
            }
            // `~user` is not expanded, and quietly mounting the wrong home
            // would be worse than saying so.
            if path.starts_with('~') && !(path == "~" || path.starts_with("~/")) {
                return Err(ManifestError::BadMount {
                    path: mount.path.clone(),
                    why: "only `~/` is understood, not `~someone-else`",
                });
            }
            // `..` would let a manifest reach outside what it appears to name.
            if path.split('/').any(|part| part == "..") {
                return Err(ManifestError::BadMount {
                    path: mount.path.clone(),
                    why: "`..` is not allowed: name the folder you mean",
                });
            }
            // The root would put the King's whole disk back inside a plan whose
            // entire purpose is not to have it -- isolation that silently
            // isolates nothing.
            if path == "/" {
                return Err(ManifestError::BadMount {
                    path: mount.path.clone(),
                    why: "sharing `/` would undo the sealing entirely",
                });
            }
        }

        Ok(())
    }
}

/// Reads a manifest, and refuses one that would fail later.
///
/// Validation happens here rather than at container-start time on purpose: a
/// bad name is worth reporting when the file is read, not three minutes into a
/// plan when a `docker run` fails with a message about something else.
pub fn parse(text: &str) -> Result<ServiceManifest, ManifestError> {
    let manifest: ServiceManifest =
        toml::from_str(text).map_err(|e| ManifestError::Syntax(e.to_string()))?;
    manifest.validate()?;
    Ok(manifest)
}

/// Whether a name is usable as part of a container name.
///
/// Docker allows a wider set than this. The narrower rule is deliberate: the
/// name is also concatenated into a container name and read back out of a
/// label, and a name containing a slash or a colon would make both ambiguous.
fn is_safe_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// Whether a name may be used for a new service, as the form asks before it
/// writes.
///
/// The same rule [`parse`] enforces, made public so the form can refuse a bad
/// name while the King is still typing it rather than after the file is
/// written. One rule, one function -- two would be free to drift, and the drift
/// would show up as a form that accepts what the parser then rejects.
pub fn is_usable_name(name: &str) -> bool {
    is_safe_name(name)
}

/// What a declared service is actually doing.
///
/// Distinct from the declaration for the reason the whole ledger exists: a
/// manifest says what a project *needs*, and the answer to "is it up?" comes
/// from a Docker daemon that may not even be running.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ServiceState {
    /// Up, with plans drawing from it.
    Running,
    /// Declared, but nothing has asked for it yet. The ordinary state of every
    /// well on a project with no plan open, and not a fault.
    Idle,
    /// Declared, but Kingdom cannot tell: no Docker daemon is answering.
    Unknown,
}

impl ServiceState {
    /// What the King reads on the row.
    pub fn label(&self) -> &'static str {
        match self {
            ServiceState::Running => "running",
            ServiceState::Idle => "not started",
            ServiceState::Unknown => "unknown",
        }
    }
}

/// One shared resource as the King is shown it: what it is, where it is
/// declared, and what it is doing.
///
/// The join between the two halves that exist already -- a [`ServiceSpec`] read
/// from a file and a `services::RunningService` held by the daemon -- plus the
/// one thing neither of them carries and the King specifically needs: **the
/// path of the file to go and edit**.
///
/// In `kingdom-core` rather than beside the reader because both sides render
/// it: the server builds it and the browser draws it, and one definition is
/// what stops those two disagreeing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SharedResource {
    /// What was declared.
    pub spec: ServiceSpec,
    /// Which level it runs at.
    pub scope: ServiceScope,
    /// The city it belongs to, for [`ServiceScope::City`]. `None` for a host
    /// resource, which belongs to no project.
    pub city: Option<CityId>,
    /// That city's name, so a row can say who owns it without the browser
    /// having to look the city up.
    pub city_name: Option<String>,
    /// The absolute path of the manifest this was declared in.
    ///
    /// Absolute rather than relative, and that is the point of it: the King's
    /// next move after reading this screen is to open the file in his own
    /// editor, and a path relative to something he has to work out is a path he
    /// cannot paste.
    pub manifest_path: String,
    /// What it is doing right now.
    pub state: ServiceState,
    /// Where to reach it, as `host:port`, once it is up.
    pub address: Option<String>,
    /// The container's name, for `docker logs`. Known even when it is not
    /// running, because the name is derived rather than allocated.
    pub container: String,
    /// The titles of the plans drawing from it right now.
    ///
    /// Titles rather than ids: "who else is in here?" is a question about
    /// people's work, and a UUID does not answer it.
    pub users: Vec<String>,
}

impl SharedResource {
    /// Who this belongs to, in one phrase, as the ledger groups by.
    pub fn owner(&self) -> String {
        match (&self.scope, &self.city_name) {
            (ServiceScope::Host, _) => "The whole machine".to_string(),
            (ServiceScope::City, Some(name)) => name.clone(),
            (ServiceScope::City, None) => "A project".to_string(),
        }
    }
}

/// A manifest that could not be read, kept rather than dropped.
///
/// Today a broken manifest is silent until an agent's turn is refused three
/// minutes in. Carrying the failure into the ledger is most of why the ledger
/// is worth building.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManifestTrouble {
    /// Whose manifest it is.
    pub scope: ServiceScope,
    /// The city, when it is a city's.
    pub city_name: Option<String>,
    /// The absolute path of the file at fault.
    pub manifest_path: String,
    /// What was wrong with it, in [`ManifestError`]'s words.
    pub detail: String,
}

/// Everything the shared-resources screen draws.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResourceInventory {
    /// Every resource declared anywhere, host first and then by city.
    pub resources: Vec<SharedResource>,
    /// Every manifest that could not be read.
    pub troubles: Vec<ManifestTrouble>,
    /// Why nothing can be running, when that is the case: no Docker on `PATH`,
    /// or a daemon that is not answering.
    ///
    /// Asked once for the whole screen rather than once per row. `None` means
    /// Docker answered, so an idle resource is genuinely just idle.
    pub docker_trouble: Option<String>,
}

impl ResourceInventory {
    /// Whether the King has declared anything at all.
    pub fn is_empty(&self) -> bool {
        self.resources.is_empty() && self.troubles.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The manifest the `shopfront` fixture ships, parsed as written.
    ///
    /// Pinned as a test because it is the one manifest a person will copy, and
    /// a change to the field names that silently made it parse to *nothing*
    /// would leave five agents quietly sharing no database at all.
    #[test]
    fn the_sample_manifest_parses() {
        let manifest = parse(
            r#"
            [[service]]
            name  = "db"
            image = "mongo:7"
            port  = 27017
            volume = "shopfront-db"
            "#,
        )
        .expect("the sample manifest must parse");

        assert_eq!(manifest.services.len(), 1);
        let db = &manifest.services[0];
        assert_eq!(db.name, "db");
        assert_eq!(db.image, "mongo:7");
        assert_eq!(db.port, 27017);
        assert_eq!(db.volume.as_deref(), Some("shopfront-db"));
    }

    /// A city with no services is a valid city, not an error.
    ///
    /// The overwhelmingly common case: every existing project has no manifest,
    /// and an empty one must behave the same as an absent one.
    #[test]
    fn an_empty_manifest_is_not_an_error() {
        assert!(parse("").expect("empty is valid").is_empty());
        assert!(parse("# nothing here\n").expect("comments only").is_empty());
    }

    /// A manifest still setting `env` is refused **by name**, not ignored.
    ///
    /// The failure this prevents is the quiet one. Serde drops an unknown key
    /// without a word, so a project that still declares `MONGODB_URI` would go
    /// on believing its agents are handed it while nothing sets it -- and the
    /// agent's connection failure would read as a bug in the project's own
    /// code. The message names the service and says what to do instead.
    #[test]
    fn a_manifest_still_setting_env_is_refused_by_name() {
        let error = parse(
            r#"
            [[service]]
            name = "db"
            image = "mongo:7"
            port = 27017
            env = { MONGODB_URI = "mongodb://{host}:{port}/shop" }
            "#,
        )
        .expect_err("a retired field must be refused rather than dropped");

        assert_eq!(
            error,
            ManifestError::Retired {
                service: "db".to_string(),
                field: "env",
            }
        );
        // The King has to find the line and delete it, so both the service and
        // the field are named, and the replacement is stated.
        let said = error.to_string();
        assert!(said.contains("db"), "{said}");
        assert!(said.contains("env"), "{said}");
        assert!(said.contains("localhost"), "{said}");
    }

    /// What Kingdom knows about an image so the King does not have to.
    ///
    /// The port is what the form fills in, the data directory is where a volume
    /// is mounted, and `boot` is the difference between a Postgres that starts
    /// and one that exits 1 saying `POSTGRES_PASSWORD` -- measured against a
    /// real daemon, which is why it is a table rather than a hope.
    #[test]
    fn a_well_known_image_brings_its_own_port_and_boot_environment() {
        let mongo = known_image("mongo:7").expect("mongo is known");
        assert_eq!(mongo.port, 27017);
        assert_eq!(mongo.data_dir, "/data/db");
        assert!(mongo.boot.is_empty(), "mongo starts with nothing set");

        let postgres = known_image("postgres:16").expect("postgres is known");
        assert_eq!(postgres.port, 5432);
        assert_eq!(postgres.boot, &[("POSTGRES_PASSWORD", "postgres")]);

        assert_eq!(known_image("redis:7-alpine").map(|k| k.port), Some(6379));
        assert_eq!(known_image("mysql:8").map(|k| k.port), Some(3306));
    }

    /// A tag and a registry are not part of the image's identity here.
    ///
    /// `docker.io/library/postgres:16-alpine` is still Postgres, and a King who
    /// pastes a fully qualified name should not lose the port and the password
    /// for it.
    #[test]
    fn an_image_is_recognised_through_its_registry_and_tag() {
        for image in [
            "postgres",
            "postgres:16",
            "postgres:16-alpine",
            "library/postgres:16",
            "docker.io/library/postgres:16",
        ] {
            assert_eq!(
                known_image(image).map(|k| k.port),
                Some(5432),
                "{image} should be recognised as postgres"
            );
        }
    }

    /// An unknown image is not a refusal: it runs, it just has to be told its
    /// port, and a volume on it lands somewhere harmless.
    #[test]
    fn an_unknown_image_still_gets_a_data_directory() {
        assert!(known_image("ghcr.io/acme/thing:1").is_none());
        assert_eq!(data_dir_for("ghcr.io/acme/thing:1"), "/data");
        assert_eq!(data_dir_for("mongo:7"), "/data/db");
    }

    /// Two services with one name would mean two containers with one name, and
    /// the second `docker run` failing for a reason that reads as unrelated.
    #[test]
    fn two_services_may_not_share_a_name() {
        let error = parse(
            r#"
            [[service]]
            name = "db"
            image = "mongo:7"
            port = 27017

            [[service]]
            name = "db"
            image = "postgres:16"
            port = 5432
            "#,
        )
        .expect_err("a duplicate name must be refused");

        assert_eq!(error, ManifestError::DuplicateName("db".to_string()));
        // The King has to find the offending line, so the name is in the text.
        assert!(error.to_string().contains("db"));
    }

    /// A name that cannot be part of a container name is refused at parse time
    /// rather than by Docker later, where the message is about something else.
    #[test]
    fn a_name_that_docker_could_not_take_is_refused() {
        for bad in ["a/b", "with space", "colon:name", ""] {
            let text = format!("[[service]]\nname = \"{bad}\"\nimage = \"mongo:7\"\nport = 1\n");
            assert!(
                parse(&text).is_err(),
                "`{bad}` should not be an acceptable service name"
            );
        }
        assert!(parse("[[service]]\nname = \"my_db-2\"\nimage = \"mongo:7\"\nport = 1\n").is_ok());
    }

    /// An image is what gets run; an empty one is a manifest that cannot work.
    #[test]
    fn a_service_without_an_image_is_refused() {
        let error = parse("[[service]]\nname = \"db\"\nimage = \"\"\nport = 1\n")
            .expect_err("an empty image must be refused");
        assert_eq!(
            error,
            ManifestError::Empty {
                service: "db".to_string(),
                field: "image",
            }
        );
    }

    /// Broken TOML says so, and says what was wrong, rather than reporting as
    /// an empty manifest -- which would look exactly like "this project has no
    /// services" and hide the typo.
    ///
    /// It deliberately does **not** name a file. There are two manifests now,
    /// this crate does no I/O and cannot tell which one it was handed, and a
    /// message that guessed said `.kingdom/services.toml` for a fault in the
    /// King's own profile. `services::manifest_in` attaches the real path,
    /// being the only layer that knows it.
    #[test]
    fn broken_toml_is_a_syntax_error_not_an_empty_manifest() {
        let error = parse("[[service]\nname = \"db\"\n").expect_err("must not parse");
        assert!(matches!(error, ManifestError::Syntax(_)));
        // What went wrong, in words.
        assert!(error.to_string().contains("not valid TOML"), "{error}");
        // And no path, because this crate cannot know which file it was.
        assert!(!error.to_string().contains(MANIFEST_PATH), "{error}");
    }

    /// The manifest path is inside `.kingdom/`, which is exactly why
    /// `worktree.rs` has to re-include it in the repository's excludes.
    ///
    /// Pinned so that moving the file forces a look at that rule, rather than
    /// silently making the manifest invisible to git.
    #[test]
    fn the_manifest_lives_in_the_citys_kingdom_folder() {
        assert_eq!(MANIFEST_PATH, ".kingdom/services.toml");
    }

    /// The form writes text and the parser reads it, so the only thing that
    /// makes the pair trustworthy is that the trip closes.
    ///
    /// Every field at once, including the optional one -- a `render` that
    /// dropped `volume` would be invisible until somebody's database lost its
    /// data.
    #[test]
    fn a_rendered_service_parses_back_to_itself() {
        let spec = ServiceSpec {
            name: "db".to_string(),
            image: "mongo:7".to_string(),
            port: 27017,
            volume: Some("shopfront-db".to_string()),
            retired_env: None,
        };

        let manifest = parse(&spec.render()).expect("what the form renders must parse");
        assert_eq!(manifest.services, vec![spec]);
    }

    /// A service with nothing optional set renders the minimum, and that
    /// minimum still parses.
    ///
    /// The `volume` line is omitted rather than written empty: `volume = ""`
    /// would be a *different* declaration from "no volume". `env` must never
    /// appear at all -- a rendered block that carried one would be refused by
    /// the parser it was written for.
    #[test]
    fn a_bare_service_renders_without_empty_lines() {
        let spec = ServiceSpec {
            name: "cache".to_string(),
            image: "redis:7".to_string(),
            port: 6379,
            volume: None,
            retired_env: None,
        };

        let rendered = spec.render();
        assert!(!rendered.contains("env"), "rendered: {rendered:?}");
        assert!(!rendered.contains("volume"), "rendered: {rendered:?}");
        assert_eq!(
            parse(&rendered).expect("a bare service parses").services,
            vec![spec]
        );
    }

    /// A quote in a value must not break out of its string.
    ///
    /// An image or a volume is where one can now arrive -- a private registry
    /// path, most plausibly -- and an unescaped quote would turn one odd name
    /// into a manifest that no longer parses at all, taking the *other*
    /// services in the file with it.
    #[test]
    fn a_quote_in_a_value_does_not_break_the_file() {
        let spec = ServiceSpec {
            name: "db".to_string(),
            image: "postgres:16".to_string(),
            port: 5432,
            volume: Some("a\"b\\c".to_string()),
            retired_env: None,
        };

        let manifest = parse(&spec.render()).expect("an escaped value still parses");
        assert_eq!(manifest.services[0].volume.as_deref(), Some("a\"b\\c"));
    }

    /// Appending a rendered block to a manifest that already has one leaves
    /// both readable -- which is the whole reason the form appends text instead
    /// of re-serialising the document.
    #[test]
    fn a_rendered_block_appends_to_an_existing_manifest() {
        let existing = "# What this project needs standing.\n\
                        [[service]]\n\
                        name = \"db\"\n\
                        image = \"mongo:7\"\n\
                        port = 27017\n";
        let addition = ServiceSpec {
            name: "cache".to_string(),
            image: "redis:7".to_string(),
            port: 6379,
            volume: None,
            retired_env: None,
        };

        let combined = format!("{existing}\n{}", addition.render());
        let manifest = parse(&combined).expect("both blocks must parse");

        let names: Vec<&str> = manifest.services.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["db", "cache"]);
        // And the comment the King wrote is still there.
        assert!(combined.contains("# What this project needs standing."));
    }

    /// A scope survives the round trip through a form value.
    ///
    /// The wire name is what a `<select>` hands back, and a scope that came
    /// back wrong would write a project's database into the King's profile --
    /// where every other project would then find it.
    #[test]
    fn a_scope_survives_the_form() {
        for scope in [ServiceScope::Host, ServiceScope::City] {
            assert_eq!(ServiceScope::from_wire(scope.wire_name()), Some(scope));
        }
        assert_eq!(ServiceScope::from_wire("machine"), None);
    }

    /// The form asks the same question the parser does.
    ///
    /// Two rules would drift, and the drift shows up as a form that cheerfully
    /// accepts a name the parser then refuses -- after the file is written.
    #[test]
    fn the_form_refuses_exactly_what_the_parser_refuses() {
        for name in ["db", "my_db-2"] {
            assert!(is_usable_name(name));
            let spec = ServiceSpec {
                name: name.to_string(),
                image: "mongo:7".to_string(),
                port: 1,
                volume: None,
                retired_env: None,
            };
            assert!(parse(&spec.render()).is_ok(), "`{name}` should be usable");
        }
        for name in ["a/b", "with space", "colon:name", ""] {
            assert!(!is_usable_name(name), "`{name}` should be refused");
        }
    }

    /// A host resource is owned by the machine and a city's by its city, and
    /// the ledger groups on exactly that string.
    #[test]
    fn a_resource_says_who_it_belongs_to() {
        let spec = ServiceSpec {
            name: "db".to_string(),
            image: "mongo:7".to_string(),
            port: 27017,
            volume: None,
            retired_env: None,
        };
        let resource = |scope, city_name: Option<&str>| SharedResource {
            spec: spec.clone(),
            scope,
            city: city_name.map(CityId::from),
            city_name: city_name.map(str::to_string),
            manifest_path: "/tmp/services.toml".to_string(),
            state: ServiceState::Idle,
            address: None,
            container: "kingdom-host-db".to_string(),
            users: Vec::new(),
        };

        assert_eq!(
            resource(ServiceScope::Host, None).owner(),
            "The whole machine"
        );
        assert_eq!(
            resource(ServiceScope::City, Some("shopfront")).owner(),
            "shopfront"
        );
    }
}

#[cfg(test)]
mod mount_tests {
    use super::*;

    /// A manifest can declare folders as well as containers, and the two do not
    /// interfere.
    #[test]
    fn a_manifest_holds_both_kinds() {
        let manifest = parse(
            r#"
[[service]]
name  = "db"
image = "mongo:7"
port  = 27017

[[mount]]
path = "~/.cargo"
mode = "rw"

[[mount]]
path = "/opt/toolchain"
"#,
        )
        .expect("both kinds must parse together");

        assert_eq!(manifest.services.len(), 1);
        assert_eq!(manifest.mounts.len(), 2);
        assert_eq!(manifest.mounts[0].path, "~/.cargo");
        assert!(manifest.mounts[0].mode.is_writable());
        // Read-only unless it says otherwise: the default that makes declaring
        // a toolchain safe.
        assert!(!manifest.mounts[1].mode.is_writable());
    }

    /// A manifest with only folders needs no Docker daemon.
    ///
    /// The distinction `has_services` exists for. Treating such a manifest as
    /// non-empty would refuse to open a project whose only declaration is a
    /// folder, over a daemon it never needed.
    #[test]
    fn folders_alone_do_not_ask_for_a_daemon() {
        let manifest = parse("[[mount]]\npath = \"~/.cargo\"\n").expect("mounts alone must parse");

        assert!(!manifest.is_empty(), "it declares something");
        assert!(
            !manifest.has_services(),
            "but nothing that needs a container"
        );
    }

    /// Every manifest written before mounts existed still parses, with none.
    #[test]
    fn a_manifest_without_mounts_is_unchanged() {
        let manifest =
            parse("[[service]]\nname = \"db\"\nimage = \"mongo:7\"\nport = 27017\n").unwrap();

        assert!(manifest.mounts.is_empty());
        assert!(manifest.has_services());
    }

    /// The paths that cannot be allowed, each refused with a reason the King
    /// can act on.
    ///
    /// `/` is the one that matters most: sharing it would put his whole disk
    /// back inside a plan whose entire purpose is not to have it -- isolation
    /// that silently isolates nothing.
    #[test]
    fn a_path_that_would_undo_the_sealing_is_refused() {
        for (path, expected) in [
            ("/", "undo the sealing"),
            ("relative/path", "absolute path"),
            ("~someone/else", "not `~someone-else`"),
            ("/etc/../root", "`..` is not allowed"),
            ("", "it is empty"),
        ] {
            let text = format!("[[mount]]\npath = {}\n", toml_string(path));
            let error = parse(&text).expect_err("{path} must be refused");

            let shown = error.to_string();
            assert!(
                shown.contains(expected),
                "`{path}` should say {expected:?}, said {shown:?}"
            );
        }
    }

    /// `~` is expanded where it is used, not where it is parsed.
    ///
    /// What the King wrote is what he reads back, and a project manifest that
    /// travels between machines still means "this user's home" wherever it
    /// lands.
    #[test]
    fn a_home_relative_path_is_expanded_at_use() {
        let mount = MountSpec {
            path: "~/.cargo".to_string(),
            mode: MountMode::Rw,
        };

        assert_eq!(mount.path, "~/.cargo", "the file keeps what he typed");
        assert_eq!(mount.expanded("/home/king"), "/home/king/.cargo");
        // A trailing slash on the home directory must not double up.
        assert_eq!(mount.expanded("/home/king/"), "/home/king/.cargo");

        // An absolute path is untouched.
        let absolute = MountSpec {
            path: "/opt/tools".to_string(),
            mode: MountMode::Ro,
        };
        assert_eq!(absolute.expanded("/home/king"), "/opt/tools");
    }

    /// A rendered mount parses back, which is what the quick-add writer relies
    /// on.
    #[test]
    fn a_rendered_mount_round_trips() {
        for mode in [MountMode::Ro, MountMode::Rw] {
            let mount = MountSpec {
                path: "~/.cargo".to_string(),
                mode,
            };
            let parsed = parse(&mount.render()).expect("what we render, we must parse");
            assert_eq!(parsed.mounts, vec![mount]);
        }
    }

    /// A Rust toolchain is more than the folder `cargo` sits in.
    ///
    /// The measurement this table exists for: `~/.cargo/bin` alone gives a
    /// `cargo` that runs and then re-downloads the toolchain, because
    /// `~/.rustup` is not there. Both are writable, because both are written
    /// to as a build runs.
    #[test]
    fn a_path_entry_brings_the_folders_its_tool_needs() {
        let rust = known_path("/home/anyone/.cargo/bin").expect("cargo is known");
        let folders: Vec<&str> = rust.folders.iter().map(|(p, _)| *p).collect();

        assert!(folders.contains(&"~/.cargo"));
        assert!(
            folders.contains(&"~/.rustup"),
            "without it every build re-downloads the toolchain"
        );
        assert!(
            rust.folders.iter().all(|(_, mode)| mode.is_writable()),
            "a read-only registry means re-downloading it every time"
        );
    }

    /// An unrecognised `PATH` entry is not refused, merely unadorned.
    ///
    /// A tool Kingdom has never heard of is still a tool the King has.
    #[test]
    fn an_unknown_path_entry_is_simply_unknown() {
        assert!(known_path("/home/anyone/.config/some-tool/bin").is_none());
        assert!(known_path("/opt/vendor/bin").is_none());
    }

    /// The table recognises a folder wherever the King's home happens to be.
    #[test]
    fn a_known_folder_is_recognised_under_any_home() {
        for home in ["/home/omarchy", "/Users/king", "/var/home/someone"] {
            assert!(
                known_path(&format!("{home}/.cargo/bin")).is_some(),
                "cargo under {home} must be recognised"
            );
            assert!(known_path(&format!("{home}/.local/share/pnpm")).is_some());
        }
    }
}
