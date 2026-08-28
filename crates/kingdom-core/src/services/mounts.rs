//! Folders a sealed plan may see: what is declared, and what is offered.
//!
//! The other half of a manifest, and deliberately not a resource. A
//! [`MountSpec`] reaches **no runtime at all**: nothing raises it, nothing can
//! fail to be up, and there is no reference count and no container behind it.
//! It is read once, when a sealed plan's namespace is built, and is inert
//! thereafter -- which is why it goes nowhere near [`super::ResourceKind`] or a
//! driver.
//!
//! It shares the file, and now the module tree, with the services for the one
//! reason that matters to the King: both answer *"what does this project need
//! in order to run?"*, and he already knows where to look.

use super::{toml_string, ServiceScope};
use serde::{Deserialize, Serialize};
use std::fmt::Write as _;

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

#[cfg(test)]
mod tests {
    use super::*;
    // The parser is the parent's: a mount is a block in the same file, and
    // these tests read one the way `services::parse` will.
    use crate::services::parse;

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
