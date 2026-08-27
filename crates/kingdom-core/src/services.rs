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
use std::collections::BTreeMap;
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
}

/// One container the city shares.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServiceSpec {
    /// What the King calls it, and half of the container's identity. Unique
    /// within a manifest.
    pub name: String,
    /// The image to run, tag included.
    pub image: String,
    /// The port the service listens on *inside* the container.
    pub port: u16,
    /// Variables handed to every plan's tools, with `{host}` and `{port}`
    /// replaced by the running container's address.
    ///
    /// This is how a plan finds the well. A plan's namespace cannot resolve
    /// Docker's service names -- Docker's DNS answers only between containers
    /// on the same network -- so an address is the only thing that works, and
    /// the address is not known until the container exists.
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// A named Docker volume for the service's data, kept when the container is
    /// stopped.
    ///
    /// `None` means the data goes with the container. That is right for a cache
    /// and wrong for a database, so it is stated per service rather than
    /// assumed either way.
    #[serde(default)]
    pub volume: Option<String>,
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
    /// An env value interpolated something that is not `{host}` or `{port}`.
    UnknownPlaceholder { service: String, key: String },
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
            ManifestError::UnknownPlaceholder { service, key } => write!(
                f,
                "service `{service}` sets `{key}` with a placeholder that is \
                 not `{{host}}` or `{{port}}`"
            ),
        }
    }
}

impl std::error::Error for ManifestError {}

impl ServiceSpec {
    /// The env vars for this service once its address is known.
    ///
    /// Substitution rather than a format string, so a value with no placeholder
    /// passes through untouched -- a service may want a plain
    /// `MONGO_DB=shopfront` beside its URI.
    pub fn environment(&self, host: &str, port: u16) -> Vec<(String, String)> {
        self.env
            .iter()
            .map(|(key, value)| {
                let filled = value
                    .replace("{host}", host)
                    .replace("{port}", &port.to_string());
                (key.clone(), filled)
            })
            .collect()
    }

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
        if !self.env.is_empty() {
            let pairs: Vec<String> = self
                .env
                .iter()
                .map(|(key, value)| format!("{key} = {}", toml_string(value)))
                .collect();
            let _ = writeln!(out, "env   = {{ {} }}", pairs.join(", "));
        }
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
        self.services.is_empty()
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

            for (key, value) in &service.env {
                if unknown_placeholder(value).is_some() {
                    return Err(ManifestError::UnknownPlaceholder {
                        service: service.name.clone(),
                        key: key.clone(),
                    });
                }
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

/// `KEY=value` lines as the map a manifest holds.
///
/// # Why the form sends text rather than pairs
///
/// It is the shape the file itself has, so what the King types is what he sees
/// in the preview and what lands in the manifest -- one representation instead
/// of three. It also closes a real hole: a `Vec<(String, String)>` that happens
/// to be **empty** does not survive a server function's argument encoding at
/// all, and a service with no environment is the ordinary case rather than a
/// corner. Measured, not guessed: the form failed with "missing field `env`"
/// for exactly that input.
///
/// Forgiving in the two ways a person is: blank lines are skipped, and a line
/// with no `=` is dropped rather than becoming a variable with an empty name --
/// which [`parse`] would then refuse, blaming the whole file for one unfinished
/// line.
pub fn parse_env(text: &str) -> BTreeMap<String, String> {
    text.lines()
        .filter_map(|line| line.split_once('='))
        .map(|(key, value)| (key.trim().to_string(), value.trim().to_string()))
        .filter(|(key, _)| !key.is_empty())
        .collect()
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
    /// The environment a plan actually receives, with `{host}` and `{port}`
    /// filled in when the address is known and left as written when it is not.
    pub environment: Vec<(String, String)>,
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

/// The first `{...}` in a value that is not one we substitute.
///
/// Catches `{hosts}` and `{HOST}`, which would otherwise reach a plan verbatim
/// and fail as a connection to a host literally called `{hosts}`.
fn unknown_placeholder(value: &str) -> Option<String> {
    let mut rest = value;
    while let Some(open) = rest.find('{') {
        let after = &rest[open + 1..];
        let Some(close) = after.find('}') else {
            return Some("{".to_string());
        };
        let name = &after[..close];
        if name != "host" && name != "port" {
            return Some(name.to_string());
        }
        rest = &after[close + 1..];
    }
    None
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
            env   = { MONGODB_URI = "mongodb://{host}:{port}/shopfront" }
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

    /// The address is substituted, and everything else is left alone.
    ///
    /// The second half matters as much as the first: a manifest may set a plain
    /// value beside its URI, and rewriting that would be a surprise.
    #[test]
    fn the_address_is_filled_in_and_nothing_else_is() {
        let manifest = parse(
            r#"
            [[service]]
            name = "db"
            image = "mongo:7"
            port = 27017
            env = { MONGODB_URI = "mongodb://{host}:{port}/shop", MONGO_DB = "shop" }
            "#,
        )
        .expect("valid");

        let environment = manifest.services[0].environment("172.31.4.10", 27017);
        let lookup: std::collections::BTreeMap<_, _> = environment.into_iter().collect();

        assert_eq!(
            lookup.get("MONGODB_URI").map(String::as_str),
            Some("mongodb://172.31.4.10:27017/shop")
        );
        assert_eq!(lookup.get("MONGO_DB").map(String::as_str), Some("shop"));
    }

    /// `{port}` uses the container's real port, which need not be the declared
    /// one forever -- so it is passed in rather than read off the spec.
    #[test]
    fn the_port_placeholder_takes_the_port_it_is_given() {
        let spec = ServiceSpec {
            name: "cache".to_string(),
            image: "redis:7".to_string(),
            port: 6379,
            env: [("URL".to_string(), "redis://{host}:{port}".to_string())]
                .into_iter()
                .collect(),
            volume: None,
        };
        assert_eq!(
            spec.environment("10.0.0.5", 6380),
            vec![("URL".to_string(), "redis://10.0.0.5:6380".to_string())]
        );
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

    /// A misspelled placeholder reaches the plan verbatim and fails as a
    /// connection to a host called `{hosts}` -- which is a genuinely baffling
    /// error an hour later. Caught here instead.
    #[test]
    fn a_misspelled_placeholder_is_caught_at_parse_time() {
        let error = parse(
            r#"
            [[service]]
            name = "db"
            image = "mongo:7"
            port = 27017
            env = { URI = "mongodb://{hosts}:{port}" }
            "#,
        )
        .expect_err("an unknown placeholder must be refused");

        assert_eq!(
            error,
            ManifestError::UnknownPlaceholder {
                service: "db".to_string(),
                key: "URI".to_string(),
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
    /// Every field at once, including the two that are optional and the one
    /// that carries placeholders -- a `render` that dropped `volume` would be
    /// invisible until somebody's database lost its data.
    #[test]
    fn a_rendered_service_parses_back_to_itself() {
        let spec = ServiceSpec {
            name: "db".to_string(),
            image: "mongo:7".to_string(),
            port: 27017,
            env: BTreeMap::from([
                (
                    "MONGODB_URI".to_string(),
                    "mongodb://{host}:{port}/shopfront".to_string(),
                ),
                ("MONGO_DB".to_string(), "shopfront".to_string()),
            ]),
            volume: Some("shopfront-db".to_string()),
        };

        let manifest = parse(&spec.render()).expect("what the form renders must parse");
        assert_eq!(manifest.services, vec![spec]);
    }

    /// A service with nothing optional set renders the minimum, and that
    /// minimum still parses.
    ///
    /// The `env` and `volume` lines are omitted rather than written empty:
    /// `env = {  }` is noise in a file a person reads, and `volume = ""` would
    /// be a *different* declaration from "no volume".
    #[test]
    fn a_bare_service_renders_without_empty_lines() {
        let spec = ServiceSpec {
            name: "cache".to_string(),
            image: "redis:7".to_string(),
            port: 6379,
            env: BTreeMap::new(),
            volume: None,
        };

        let rendered = spec.render();
        assert!(!rendered.contains("env"), "rendered: {rendered:?}");
        assert!(!rendered.contains("volume"), "rendered: {rendered:?}");
        assert_eq!(
            parse(&rendered).expect("a bare service parses").services,
            vec![spec]
        );
    }

    /// A value with a quote in it must not break out of its string.
    ///
    /// A URI with a password in it is the realistic way this arrives, and an
    /// unescaped quote would turn one bad password into a manifest that no
    /// longer parses at all -- taking the *other* services in the file with it.
    #[test]
    fn a_quote_in_a_value_does_not_break_the_file() {
        let spec = ServiceSpec {
            name: "db".to_string(),
            image: "postgres:16".to_string(),
            port: 5432,
            env: BTreeMap::from([("PASSWORD".to_string(), "a\"b\\c".to_string())]),
            volume: None,
        };

        let manifest = parse(&spec.render()).expect("an escaped value still parses");
        assert_eq!(manifest.services[0].env["PASSWORD"], "a\"b\\c");
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
            env: BTreeMap::new(),
            volume: None,
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
                env: BTreeMap::new(),
                volume: None,
            };
            assert!(parse(&spec.render()).is_ok(), "`{name}` should be usable");
        }
        for name in ["a/b", "with space", "colon:name", ""] {
            assert!(!is_usable_name(name), "`{name}` should be refused");
        }
    }

    /// The form's text box and the manifest's map are the same thing, so the
    /// trip between them has to close.
    #[test]
    fn env_lines_become_the_map_a_manifest_holds() {
        let parsed = parse_env("URI = mongodb://{host}:{port}/app\nMONGO_DB=app");
        assert_eq!(parsed["URI"], "mongodb://{host}:{port}/app");
        assert_eq!(parsed["MONGO_DB"], "app");
    }

    /// A half-typed line must not become a variable with no name.
    ///
    /// It would be refused by [`parse`] at write time with an error about the
    /// whole file, which reads as "your manifest is broken" rather than "line
    /// three is not finished".
    #[test]
    fn a_line_without_an_equals_is_dropped_rather_than_half_kept() {
        let parsed = parse_env("GOOD=1\n\njust typing\n  =2\n");
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed["GOOD"], "1");
    }

    /// A value may contain `=` -- a Postgres DSN routinely does -- so only the
    /// first one separates.
    #[test]
    fn only_the_first_equals_separates() {
        let parsed = parse_env("DSN=host=db user=app");
        assert_eq!(parsed["DSN"], "host=db user=app");
    }

    /// An empty box is an empty map, and the resulting service still renders
    /// and parses.
    ///
    /// The case that actually broke: a `Vec` of pairs that happened to be empty
    /// did not survive a server function's argument encoding at all, and
    /// "declare a Redis with no environment" is the ordinary case rather than a
    /// corner. Sending the text instead is what fixed it, so the empty text is
    /// worth a test of its own.
    #[test]
    fn no_environment_at_all_is_a_service_that_still_works() {
        assert!(parse_env("").is_empty());
        assert!(parse_env("\n  \n").is_empty());

        let spec = ServiceSpec {
            name: "cache".to_string(),
            image: "redis:7".to_string(),
            port: 6379,
            env: parse_env(""),
            volume: None,
        };
        assert_eq!(
            parse(&spec.render())
                .expect("a service with no environment must parse")
                .services,
            vec![spec]
        );
    }

    /// A host resource is owned by the machine and a city's by its city, and
    /// the ledger groups on exactly that string.
    #[test]
    fn a_resource_says_who_it_belongs_to() {
        let spec = ServiceSpec {
            name: "db".to_string(),
            image: "mongo:7".to_string(),
            port: 27017,
            env: BTreeMap::new(),
            volume: None,
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
            environment: Vec::new(),
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
