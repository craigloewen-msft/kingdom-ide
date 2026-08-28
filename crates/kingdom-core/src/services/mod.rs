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
//!
//! # What a resource *is*, and what runs it
//!
//! Every shared resource is a Docker container today, and for a long time the
//! code said so in its field names. It no longer does: a declaration carries a
//! [`ResourceKind`], which holds what that kind of thing needs -- for the one
//! kind there is, a [`docker::DockerSpec`] with an image and a volume.
//!
//! The kind is an **enum carrying its payload**, not a tag beside a bag of
//! optional fields and not a trait object. A second kind is then a variant, and
//! every place that has to decide something is named by the compiler rather
//! than found by reading -- which a trait with one implementor would not do,
//! since a driver shaped wrongly for a new kind still compiles.
//!
//! This module knows what the kinds *are* and nothing about running them. The
//! runtime half lives in `kingdom_app::services`, which matches on the kind and
//! hands off to `kingdom_app::services::docker`.

pub mod docker;
pub mod mounts;

pub use docker::{data_dir_for, known_image, DockerSpec, KnownImage};
pub use mounts::{known_extras, known_path, KnownPath, MountCandidate, MountMode, MountSpec};

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

/// A whole manifest exactly as it appears in the file.
///
/// The counterpart of [`RawService`], and there for the same reason: the file's
/// services are flat blocks, and the typed [`ServiceSpec`] they become is not.
/// Only [`parse`] uses it -- [`ServiceManifest`] keeps its own derived serde
/// for the wire, where the typed shape is the one both sides want.
#[derive(Debug, Default, Deserialize)]
struct RawManifest {
    #[serde(default, rename = "service")]
    services: Vec<RawService>,
    #[serde(default, rename = "mount")]
    mounts: Vec<MountSpec>,
}

/// One resource the city shares.
///
/// Split into the facts every kind has -- a name and a port -- and a
/// [`ResourceKind`] carrying what only that kind needs. That division is the
/// whole change: `image` and `volume` used to sit here, on a type that four
/// other modules read without caring what a container was.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServiceSpec {
    /// What the King calls it, and half of the resource's identity. Unique
    /// within a manifest.
    pub name: String,
    /// The port the service listens on -- and the port an agent reaches it on,
    /// because that is the whole promise: a relay puts the resource on the
    /// plan's own loopback at this same number.
    ///
    /// On [`ServiceSpec`] rather than inside the kind because it is the one
    /// fact every kind must have: it is what an agent is told, and a kind with
    /// no port would be a resource nobody could reach.
    pub port: u16,
    /// What kind of thing this is, and what that kind needs.
    pub kind: ResourceKind,
    /// `env`, kept only so that a manifest still carrying it can be **refused**
    /// by name rather than silently ignored.
    ///
    /// Serde drops an unknown key without a word. Kingdom used to hand these
    /// variables to every command a plan ran, so a project that still declares
    /// them would otherwise believe its agents get `$DATABASE_URL` while
    /// nothing sets it. Never read for its contents -- only for whether it is
    /// there. See [`ManifestError::Retired`].
    pub retired_env: Option<RetiredField>,
}

/// What kind of thing a shared resource is, with what that kind needs.
///
/// # Why the payload is inside the variant
///
/// The alternative -- a bare tag beside `image: Option<String>` and
/// `volume: Option<String>` -- makes the kind a comment. Nothing would stop a
/// declaration naming a kind that has no image while carrying one, and every
/// reader would have to know which fields its kind actually uses. Here, a
/// resource that is a container **has** a [`DockerSpec`] and one that is not
/// cannot be given an image at all.
///
/// Adding a kind is: add a variant, and follow the compiler to the handful of
/// places that must decide something.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum ResourceKind {
    /// A Docker container, started and stopped by Kingdom. The only kind today.
    Docker(DockerSpec),
}

impl ResourceKind {
    /// The stable string this kind is written as in a manifest and on a form.
    ///
    /// The same word `type` takes in the file, so what the King types and what
    /// the parser matches cannot drift apart.
    pub fn wire_name(&self) -> &'static str {
        match self {
            ResourceKind::Docker(_) => "docker",
        }
    }

    /// What the King reads on a row.
    pub fn label(&self) -> &'static str {
        match self {
            ResourceKind::Docker(_) => "Docker container",
        }
    }

    /// The one line saying what this resource is -- an image, for a container.
    ///
    /// What the ledger row, the ports badge and the system prompt print beside
    /// the name. Asked of the kind so that none of those three has to know what
    /// a Docker image is.
    pub fn what(&self) -> String {
        match self {
            ResourceKind::Docker(docker) => docker.what(),
        }
    }

    /// The rows the detail pane prints, in order, as label and value.
    ///
    /// A list rather than fields the screen reaches into, because "Image" and
    /// "Data" are facts about a *container*: a kind with neither would leave
    /// the browser drawing two empty rows it had hard-coded.
    pub fn facts(&self) -> Vec<(&'static str, String)> {
        match self {
            ResourceKind::Docker(docker) => docker.facts(),
        }
    }

    /// Every kind there is, for a form to offer and a test to iterate.
    ///
    /// Returns the wire names rather than values, because a value needs a
    /// payload and "what kinds exist" is a question asked before there is one.
    pub fn all() -> &'static [&'static str] {
        &["docker"]
    }

    /// The manifest lines only this kind has, for [`ServiceSpec::render`].
    ///
    /// Written by the kind rather than by the renderer above it, which is the
    /// same argument [`Self::facts`] makes for the screen: `image` and `volume`
    /// mean nothing to a kind that has neither.
    fn render_fields(&self) -> String {
        match self {
            ResourceKind::Docker(docker) => docker.render_fields(),
        }
    }

    /// The name of a field this kind requires and does not have, if any.
    ///
    /// Asked by [`ServiceManifest::validate`], which is deliberately still the
    /// single place a field is judged -- so faults are reported in the order
    /// they appear in the file, whatever kind each service is. A kind that
    /// checked its own fields at parse time would report the *second* service's
    /// missing image before the first service's missing name.
    fn missing_field(&self) -> Option<&'static str> {
        match self {
            // An image is what a container is. Without one there is nothing to
            // run, and `docker run` would fail three minutes later saying so
            // about a name the King never typed.
            ResourceKind::Docker(docker) if docker.image.trim().is_empty() => Some("image"),
            ResourceKind::Docker(_) => None,
        }
    }
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

/// One `[[service]]` block exactly as it appears in the file.
///
/// # Why parsing goes through a flat shape and then converts
///
/// The file's shape is flat -- `type`, `name`, `image`, `port`, `volume` all at
/// one level -- and it must stay that way: every manifest already written, the
/// `shopfront` fixture and both documents are flat, and a nested
/// `[service.docker]` table would make the tag honest and every existing file
/// wrong.
///
/// Serde *can* read a flat internally-tagged enum, but what it says when it
/// cannot is the problem. An unknown `type` becomes "unknown variant `podman`,
/// expected `docker`" and a missing `image` becomes "missing field `image`",
/// neither naming the service at fault -- in a file with four services that is
/// a search rather than a fix, which is precisely what [`ManifestError`] exists
/// to avoid. So the raw form takes everything as optional and
/// [`RawService::into_spec`] is where a fault becomes a sentence with a name in
/// it.
#[derive(Debug, Clone, Deserialize)]
struct RawService {
    /// Which kind this is. Absent means `docker`: every manifest written before
    /// there were kinds is a container, and re-typing them is work the King
    /// gets nothing for.
    #[serde(default, rename = "type")]
    kind: Option<String>,
    #[serde(default)]
    name: String,
    #[serde(default)]
    port: u16,
    /// Docker's, and read only for that kind.
    #[serde(default)]
    image: Option<String>,
    /// Docker's, and read only for that kind.
    #[serde(default)]
    volume: Option<String>,
    #[serde(default, rename = "env")]
    retired_env: Option<RetiredField>,
}

impl RawService {
    /// The typed spec, or the reason this block cannot be one.
    ///
    /// The name is resolved first and used in every message, because a fault
    /// that does not say which service it is about is a fault the King has to
    /// go looking for.
    fn into_spec(self) -> Result<ServiceSpec, ManifestError> {
        let named = || {
            let name = self.name.trim();
            if name.is_empty() {
                "<unnamed>".to_string()
            } else {
                name.to_string()
            }
        };

        let kind = match self.kind.as_deref().unwrap_or("docker") {
            // An empty or absent image is **not** refused here: it is a field
            // fault, and every other field fault is reported by
            // `ServiceManifest::validate` so that the first one in the file
            // wins. See `ResourceKind::missing_field`.
            "docker" => ResourceKind::Docker(DockerSpec {
                image: self.image.clone().unwrap_or_default(),
                volume: self.volume.clone(),
            }),
            // Refused by name rather than ignored, for the reason
            // `ManifestError::Retired` exists: serde would drop an unknown
            // `type` without a word, and a project that asked for a kind
            // Kingdom does not have would get a container instead and never be
            // told.
            other => {
                return Err(ManifestError::UnknownKind {
                    service: named(),
                    kind: other.to_string(),
                })
            }
        };

        Ok(ServiceSpec {
            name: self.name,
            port: self.port,
            kind,
            retired_env: self.retired_env,
        })
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
    /// A `type` naming a kind of resource Kingdom does not have.
    ///
    /// Refused rather than defaulted to `docker`, for the reason [`Self::Retired`]
    /// exists: a project that asked for something Kingdom cannot run and was
    /// quietly given a container instead would find out from the container's
    /// behaviour, which reads as a bug in the project.
    UnknownKind { service: String, kind: String },
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
            // Names the kinds there are, because the King's next move is to
            // pick one of them -- and today the list is one word long.
            ManifestError::UnknownKind { service, kind } => write!(
                f,
                "service `{service}` is of type `{kind}`, which is not a kind of \
                 shared resource Kingdom knows. Use one of: {}.",
                ResourceKind::all().join(", ")
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
    ///
    /// # Why the kind is written even though it is the default
    ///
    /// What Kingdom *writes* says what it means. A file that omitted `type`
    /// would still parse as a container -- that is the compatibility rule for
    /// manifests written before there were kinds -- but a King reading back
    /// what the form produced deserves to see the decision it made on his
    /// behalf, and it is the line he edits when there is a second kind.
    pub fn render(&self) -> String {
        let mut out = String::from("[[service]]\n");
        let _ = writeln!(out, "type  = {}", toml_string(self.kind.wire_name()));
        let _ = writeln!(out, "name  = {}", toml_string(&self.name));
        let _ = writeln!(out, "port  = {}", self.port);
        // Whatever only this kind has, asked of the kind: a renderer here that
        // reached for an image would be the exact coupling this change removes.
        out.push_str(&self.kind.render_fields());
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
            // Whatever this kind of resource must have and does not. Asked of
            // the kind, so a kind with no image is not judged against one.
            if let Some(field) = service.kind.missing_field() {
                return Err(ManifestError::Empty {
                    service: service.name.clone(),
                    field,
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
///
/// Three steps, in this order and for a reason each: the TOML is read into the
/// flat [`RawManifest`] the file actually is, each block is **converted** into
/// a typed [`ServiceSpec`] -- where an unrecognised `type` is refused by name --
/// and only then is the whole thing validated, so that field faults are
/// reported in the order they appear in the file.
pub fn parse(text: &str) -> Result<ServiceManifest, ManifestError> {
    let raw: RawManifest =
        toml::from_str(text).map_err(|e| ManifestError::Syntax(e.to_string()))?;

    let manifest = ServiceManifest {
        services: raw
            .services
            .into_iter()
            .map(RawService::into_spec)
            .collect::<Result<Vec<_>, _>>()?,
        mounts: raw.mounts,
    };
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
    /// What the runtime calls this resource -- a container's name, today.
    ///
    /// Known even when it is not running, because it is derived rather than
    /// allocated. Named `handle` rather than `container` because the browser
    /// draws this field and the browser has no business knowing what a
    /// container is; what it *is* for a given kind is the runtime's word, and
    /// [`Self::hint`] is the sentence that makes it useful.
    pub handle: String,
    /// How the King looks at this resource himself: `docker logs kingdom-...`.
    ///
    /// Built by the runtime that owns the resource rather than formatted by the
    /// screen. The screen used to write `format!("docker logs {container}")` in
    /// the browser, which is a Docker command composed by a component that
    /// cannot run one and would be wrong for any other kind.
    pub hint: String,
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
    /// Why nothing can be running, when that is the case -- no runtime
    /// installed, or one that is not answering.
    ///
    /// Asked once for the whole screen rather than once per row, and only of
    /// the runtimes some manifest actually declares: a machine that shares
    /// nothing does not shell out to `docker` to draw an empty screen. `None`
    /// means every runtime that matters answered, so an idle resource is
    /// genuinely just idle.
    pub runtime_trouble: Option<String>,
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

    /// One container declaration, as the tests below all want it.
    ///
    /// A helper rather than a literal in each test, because a spec is now two
    /// nested types and what these tests are *about* is almost never the
    /// nesting.
    fn container(name: &str, image: &str, port: u16, volume: Option<&str>) -> ServiceSpec {
        ServiceSpec {
            name: name.to_string(),
            port,
            kind: ResourceKind::Docker(DockerSpec {
                image: image.to_string(),
                volume: volume.map(str::to_string),
            }),
            retired_env: None,
        }
    }

    /// The manifest the `shopfront` fixture ships, parsed as written.
    ///
    /// Pinned as a test because it is the one manifest a person will copy, and
    /// a change to the field names that silently made it parse to *nothing*
    /// would leave five agents quietly sharing no database at all.
    ///
    /// It names **no `type`**, which is the compatibility rule this change
    /// rests on: every manifest written before there were kinds is a container,
    /// and one that quietly parsed to something else would take five agents'
    /// database away.
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
        assert_eq!(db.port, 27017);
        assert_eq!(
            db.kind,
            ResourceKind::Docker(DockerSpec {
                image: "mongo:7".to_string(),
                volume: Some("shopfront-db".to_string()),
            }),
            "a manifest with no `type` is a Docker container"
        );
    }

    /// Saying `type = "docker"` out loud means exactly what leaving it out
    /// means.
    ///
    /// The two must not drift: the form now writes the line, and every manifest
    /// already on disk does not. If these ever parsed differently, half the
    /// King's declarations would behave unlike the other half for no reason he
    /// could see.
    #[test]
    fn naming_the_kind_is_the_same_as_leaving_it_out() {
        let with = parse(
            r#"
            [[service]]
            type  = "docker"
            name  = "db"
            image = "mongo:7"
            port  = 27017
            "#,
        )
        .expect("an explicit kind must parse");
        let without = parse(
            r#"
            [[service]]
            name  = "db"
            image = "mongo:7"
            port  = 27017
            "#,
        )
        .expect("an implicit kind must parse");

        assert_eq!(with.services, without.services);
    }

    /// A kind Kingdom does not have is refused **by name**, not defaulted.
    ///
    /// The same judgement `env` gets, and for the same reason: a project that
    /// asked for a runtime Kingdom cannot drive and was quietly given a
    /// container instead would discover it from the container's behaviour, an
    /// hour later, and read it as a bug in its own code.
    #[test]
    fn a_kind_kingdom_does_not_have_is_refused() {
        let error = parse(
            r#"
            [[service]]
            type = "podman"
            name = "db"
            port = 27017
            "#,
        )
        .expect_err("an unknown kind must be refused rather than defaulted");

        assert_eq!(
            error,
            ManifestError::UnknownKind {
                service: "db".to_string(),
                kind: "podman".to_string(),
            }
        );
        // He has to find the block and fix the line, so the message names the
        // service, what he asked for, and what he could have asked for.
        let said = error.to_string();
        assert!(said.contains("db"), "{said}");
        assert!(said.contains("podman"), "{said}");
        assert!(said.contains("docker"), "{said}");
    }

    /// What a kind is called in the file is what it is called on a form.
    ///
    /// One list, so a kind cannot be offered by a name the parser will not
    /// take. `all()` is what a test iterates and what the form would render.
    #[test]
    fn every_kind_is_named_the_same_everywhere() {
        let docker = ResourceKind::Docker(DockerSpec {
            image: "mongo:7".to_string(),
            volume: None,
        });
        assert_eq!(docker.wire_name(), "docker");
        assert!(ResourceKind::all().contains(&docker.wire_name()));

        // Every name offered must be one the parser accepts. Today that is one
        // word; the point is that adding a second cannot skip this.
        for kind in ResourceKind::all() {
            let text = format!(
                "[[service]]\ntype = \"{kind}\"\nname = \"x\"\nimage = \"mongo:7\"\nport = 1\n"
            );
            assert!(parse(&text).is_ok(), "`{kind}` is offered but not accepted");
        }
    }

    /// The ledger draws a resource from what its kind says, not from fields it
    /// reached into itself.
    ///
    /// `what` is the row and `facts` is the detail pane. Pinned because the
    /// screen no longer hard-codes "Image" and "Data", and a kind that returned
    /// nothing would render a blank card rather than fail.
    #[test]
    fn a_kind_says_what_it_is_and_what_to_show() {
        let kept = ResourceKind::Docker(DockerSpec {
            image: "mongo:7".to_string(),
            volume: Some("shopfront-db".to_string()),
        });
        assert_eq!(kept.what(), "mongo:7");
        assert_eq!(kept.label(), "Docker container");

        let facts = kept.facts();
        assert_eq!(facts[0], ("Image", "mongo:7".to_string()));
        assert!(facts[1].1.contains("shopfront-db"), "{:?}", facts[1]);

        // No volume is a decision with a consequence, and it is stated rather
        // than left off the card.
        let ephemeral = ResourceKind::Docker(DockerSpec {
            image: "redis:7".to_string(),
            volume: None,
        });
        assert!(ephemeral.facts()[1].1.contains("goes when"));
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
        let spec = container("db", "mongo:7", 27017, Some("shopfront-db"));

        let rendered = spec.render();
        // What Kingdom writes says what it means, even where the parser would
        // have assumed it.
        assert!(rendered.contains("type"), "rendered: {rendered:?}");
        let manifest = parse(&rendered).expect("what the form renders must parse");
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
        let spec = container("cache", "redis:7", 6379, None);

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
        let spec = container("db", "postgres:16", 5432, Some("a\"b\\c"));

        let manifest = parse(&spec.render()).expect("an escaped value still parses");
        assert_eq!(manifest.services, vec![spec]);
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
        let addition = container("cache", "redis:7", 6379, None);

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
            let spec = container(name, "mongo:7", 1, None);
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
        let spec = container("db", "mongo:7", 27017, None);
        let resource = |scope, city_name: Option<&str>| SharedResource {
            spec: spec.clone(),
            scope,
            city: city_name.map(CityId::from),
            city_name: city_name.map(str::to_string),
            manifest_path: "/tmp/services.toml".to_string(),
            state: ServiceState::Idle,
            address: None,
            handle: "kingdom-host-db".to_string(),
            hint: "docker logs kingdom-host-db".to_string(),
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
