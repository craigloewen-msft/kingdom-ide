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
//! # Why parsing lives here
//!
//! `kingdom-core` compiles to wasm and does no I/O, so this module takes a
//! `&str` and never opens a file. That is what lets the whole parse be tested
//! without a disk, a Docker daemon or a running server; `kingdom-app` reads the
//! bytes and calls [`parse`].

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Where a city's manifest sits, relative to its root.
///
/// Named rather than inlined because three places must agree on it: the reader,
/// the git exclude rule that keeps it visible (a bare `.kingdom/` exclude would
/// otherwise hide it), and the fixture that writes one.
pub const MANIFEST_PATH: &str = ".kingdom/services.toml";

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
        match self {
            ManifestError::Syntax(detail) => {
                write!(f, "{MANIFEST_PATH} is not valid TOML: {detail}")
            }
            ManifestError::Empty { service, field } => write!(
                f,
                "service `{service}` in {MANIFEST_PATH} has an empty `{field}`"
            ),
            ManifestError::DuplicateName(name) => write!(
                f,
                "{MANIFEST_PATH} declares two services called `{name}`; names \
                 must be unique because the name identifies the container"
            ),
            ManifestError::BadName(name) => write!(
                f,
                "`{name}` cannot be a service name in {MANIFEST_PATH}: use \
                 letters, digits, `-` and `_` only"
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

    /// Broken TOML says so, and says where, rather than reporting as an empty
    /// manifest -- which would look exactly like "this project has no
    /// services" and hide the typo.
    #[test]
    fn broken_toml_is_a_syntax_error_not_an_empty_manifest() {
        let error = parse("[[service]\nname = \"db\"\n").expect_err("must not parse");
        assert!(matches!(error, ManifestError::Syntax(_)));
        assert!(error.to_string().contains(MANIFEST_PATH));
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
}
