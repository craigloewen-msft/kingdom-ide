//! The Docker kind: what a container needs, and what Kingdom knows about one
//! without being told.
//!
//! The first -- today the only -- kind of shared resource. Everything specific
//! to running one as a container lives here: the two fields a declaration
//! carries beyond a name and a port, and the table of well-known images that
//! saves the King from having to know Postgres's port or where Mongo keeps its
//! files.
//!
//! Pure and wasm-safe, like the rest of `kingdom-core`: this says what a
//! container *is*, and `kingdom_app::services::docker` is what talks to a
//! daemon about it.

use serde::{Deserialize, Serialize};

/// What a shared resource run as a Docker container needs.
///
/// The payload of [`super::ResourceKind::Docker`]. It holds exactly the fields
/// that only a container has -- a name and a port are asked of every kind and
/// live on [`super::ServiceSpec`] instead.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DockerSpec {
    /// The image to run, tag included.
    pub image: String,
    /// A named Docker volume for the service's data, kept when the container is
    /// stopped.
    ///
    /// `None` means the data goes with the container. That is right for a cache
    /// and wrong for a database, so it is stated per service rather than
    /// assumed either way.
    #[serde(default)]
    pub volume: Option<String>,
}

impl DockerSpec {
    /// The one line saying what this resource is, for a row the King scans.
    pub fn what(&self) -> String {
        self.image.clone()
    }

    /// The rows the detail pane prints, in order.
    ///
    /// Returned as a list rather than drawn by the screen, because "Image" and
    /// "Data" are facts about a *container*: a kind with neither would leave
    /// the browser rendering two empty rows it had hard-coded. See
    /// [`super::ResourceKind::facts`].
    pub fn facts(&self) -> Vec<(&'static str, String)> {
        vec![
            ("Image", self.image.clone()),
            (
                "Data",
                match &self.volume {
                    Some(volume) => format!(
                        "Kept in the named volume `{volume}`, which outlives the container."
                    ),
                    // Stated rather than omitted: "no volume" is a decision
                    // with a consequence, and the consequence is that the data
                    // goes when the container does.
                    None => {
                        "No volume \u{2014} data goes when the container is removed.".to_string()
                    }
                },
            ),
        ]
    }

    /// The manifest lines a container declaration carries beyond name and port.
    ///
    /// `volume` is omitted rather than written empty, because `volume = ""` is
    /// a *different* declaration from "no volume" -- and one the parser refuses.
    pub(super) fn render_fields(&self) -> String {
        use std::fmt::Write as _;

        let mut out = String::new();
        let _ = writeln!(out, "image = {}", super::toml_string(&self.image));
        if let Some(volume) = &self.volume {
            let _ = writeln!(out, "volume = {}", super::toml_string(volume));
        }
        out
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
