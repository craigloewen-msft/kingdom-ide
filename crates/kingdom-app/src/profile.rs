//! The King's own profile: everything Kingdom records about itself, kept once
//! per user rather than once per checkout.
//!
//! Server-only.
//!
//! ```text
//! ~/.kingdom/
//!   settings.json               durable IDE settings, e.g. the last kingdom opened
//!   kingdoms/<key>/
//!     kingdom.json              which root this folder is for
//!     plans/<id>.json           (see `store.rs`)
//!     archive/<id>.patch
//!     approved/<id>.md
//!   realms/<name>/              proving grounds (see `mock.rs`)
//! ```
//!
//! **Why out of the kingdom root.** Two reasons, and they pull the same way.
//! Which folder the King last opened is the one fact that cannot be read from
//! inside a kingdom he has not opened yet, so it has nowhere else to live. And
//! a plan record is not the project's business: writing it into the user's dev
//! folder makes his repositories carry our bookkeeping, and made *where the
//! server was launched from* decide which proving grounds existed.
//!
//! **What stays behind.** A plan's worktree, at `<city>/.kingdom/<uuid>`, and
//! the draft inside it. Those are a checkout of *that* repository, derived and
//! disposable; the paths are named in the system prompt and resolved by each
//! plan's `Sandbox`. The two `.kingdom` directories used to be a collision worth
//! warning about. Now they are a division: derived state in the repo, durable
//! state here.
//!
//! Every write is failure-tolerant, in the style `store.rs` set. A profile that
//! cannot be written costs persistence, never the open.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Overrides the profile location. Set in a rehearsal session, and by tests.
pub const HOME_VAR: &str = "KINGDOM_HOME";

/// The profile root: `$KINGDOM_HOME`, else `~/.kingdom`.
pub fn home() -> PathBuf {
    #[cfg(test)]
    if let Some(p) = test_home() {
        return p;
    }

    if let Ok(p) = std::env::var(HOME_VAR) {
        if !p.trim().is_empty() {
            return PathBuf::from(p.trim());
        }
    }

    let home = std::env::var("HOME").unwrap_or_default();
    PathBuf::from(home).join(".kingdom")
}

/// Where this kingdom's records are kept.
///
/// The folder is named for a person reading `ls`, but identified by the path:
/// the readable part is the directory's own name, and the suffix is a hash of
/// the fully-resolved root, so two folders both called `dev` do not share a
/// drawer. The key is derived and never authoritative -- `kingdom.json` inside
/// holds the real root, which is what tells you whose records these are when
/// the name alone is ambiguous.
pub fn kingdom_dir(root: &Path) -> PathBuf {
    let dir = home().join("kingdoms").join(key_for(root));

    // Written on first use only. A failure here costs the label, not the
    // records, so it is deliberately not reported.
    let marker = dir.join("kingdom.json");
    if !marker.exists() {
        let body = serde_json::json!({ "root": resolved(root).to_string_lossy() });
        if std::fs::create_dir_all(&dir).is_ok() {
            let _ = std::fs::write(&marker, format!("{body:#}\n"));
        }
    }

    dir
}

/// A readable, collision-resistant folder name for a kingdom root.
fn key_for(root: &Path) -> String {
    let resolved = resolved(root);

    let name = resolved
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("kingdom");
    let name = kingdom_core::naming::slugify(name);
    let name = if name.is_empty() {
        "kingdom".to_string()
    } else {
        name
    };

    format!("{name}-{:08x}", hash(&resolved.to_string_lossy()))
}

/// The root as the filesystem sees it, so `~/dev` and `~/dev/` and a path
/// through a symlink all land in one drawer. A root that cannot be resolved --
/// it may not exist yet -- is used as given rather than refused.
fn resolved(root: &Path) -> PathBuf {
    root.canonicalize().unwrap_or_else(|_| root.to_path_buf())
}

/// FNV-1a. Not cryptographic and does not need to be: it separates a handful of
/// folder names on one machine, and a dependency for that would be silly.
fn hash(text: &str) -> u32 {
    let mut h: u32 = 0x811c_9dc5;
    for b in text.as_bytes() {
        h ^= u32::from(*b);
        h = h.wrapping_mul(0x0100_0193);
    }
    h
}

/// Durable settings: what the King chose, remembered between runs.
///
/// One document rather than a file per setting, so the next thing worth keeping
/// is a field here instead of another path to invent. Unknown fields are
/// tolerated by serde's default, and every field is optional, so an older build
/// reading a newer file loses the setting rather than the file.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct Settings {
    /// The kingdom root last opened, reopened at boot.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_kingdom: Option<String>,
}

fn settings_path() -> PathBuf {
    home().join("settings.json")
}

/// Reads the settings. A missing or unreadable file reads as the defaults.
pub fn settings() -> Settings {
    std::fs::read_to_string(settings_path())
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default()
}

/// Writes the settings, atomically.
pub fn save_settings(settings: &Settings) -> std::io::Result<()> {
    let dir = home();
    std::fs::create_dir_all(&dir)?;

    let body = serde_json::to_vec_pretty(settings)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;

    let tmp = dir.join(".settings.json.tmp");
    std::fs::write(&tmp, &body)?;
    std::fs::rename(&tmp, settings_path())
}

/// Records the kingdom to reopen next time.
///
/// Called only after a root has actually opened, so a typo is never remembered.
pub fn remember_kingdom(root: &Path) {
    let mut s = settings();
    s.last_kingdom = Some(resolved(root).to_string_lossy().to_string());
    if let Err(e) = save_settings(&s) {
        leptos::logging::warn!("could not remember the kingdom folder: {e}");
    }
}

/// The kingdom to reopen, if one was recorded and the value is not blank.
pub fn last_kingdom() -> Option<PathBuf> {
    settings()
        .last_kingdom
        .map(|p| p.trim().to_string())
        .filter(|p| !p.is_empty())
        .map(PathBuf::from)
}

/// Forgets the last kingdom, so the next start asks again.
pub fn forget_kingdom() {
    let mut s = settings();
    s.last_kingdom = None;
    if let Err(e) = save_settings(&s) {
        leptos::logging::warn!("could not forget the kingdom folder: {e}");
    }
}

/// Folders that used to live under `<kingdom_root>/.kingdom/`.
const MIGRATED: [&str; 3] = ["plans", "archive", "approved"];

/// Brings an older kingdom's records into the profile, once.
///
/// **Copies, and does not delete.** A plan record is the one thing disk cannot
/// tell us again, so a bug here must be survivable: the originals stay exactly
/// where they were, which also means an older build run by accident still finds
/// its state. The cost is a stale copy nobody reads, which is the cheap side of
/// that trade.
///
/// Guarded on the destination being absent, so it runs at most once per kingdom
/// and can never overwrite records written since.
///
/// Returns a line to print when something was actually moved.
pub fn migrate(root: &Path) -> Option<String> {
    let old = root.join(".kingdom");
    let new = kingdom_dir(root);

    let mut copied = Vec::new();
    for folder in MIGRATED {
        let from = old.join(folder);
        let to = new.join(folder);
        if !from.is_dir() || to.exists() {
            continue;
        }
        match copy_dir(&from, &to) {
            Ok(n) if n > 0 => copied.push(format!("{n} {folder}")),
            Ok(_) => {}
            Err(e) => leptos::logging::warn!("could not migrate {}: {e}", from.display()),
        }
    }

    if copied.is_empty() {
        return None;
    }

    Some(format!(
        "Copied {} from {} into {}. The originals were left in place.",
        copied.join(", "),
        old.display(),
        new.display()
    ))
}

/// Copies a flat directory of files, returning how many were copied.
///
/// Flat on purpose: all three migrated folders hold files and nothing else, and
/// a general recursive copy would be more machinery than this one-time path
/// deserves.
fn copy_dir(from: &Path, to: &Path) -> std::io::Result<usize> {
    std::fs::create_dir_all(to)?;

    let mut n = 0;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        if entry.file_type()?.is_file() {
            std::fs::copy(entry.path(), to.join(entry.file_name()))?;
            n += 1;
        }
    }
    Ok(n)
}

/// Pointing the profile somewhere disposable, for tests.
///
/// [`home`] is read from process-global state, so tests that move it must not
/// run beside each other. This hands out a lock as well as the location, which
/// is what makes that a rule rather than a hope -- the same reason
/// `api::within_sandbox` was split out of its environment-reading caller.
#[cfg(test)]
pub(crate) mod testing {
    use std::path::{Path, PathBuf};
    use std::sync::{Mutex, MutexGuard, OnceLock};

    static OVERRIDE: Mutex<Option<PathBuf>> = Mutex::new(None);

    fn gate() -> &'static Mutex<()> {
        static GATE: OnceLock<Mutex<()>> = OnceLock::new();
        GATE.get_or_init(|| Mutex::new(()))
    }

    pub(super) fn current() -> Option<PathBuf> {
        OVERRIDE.lock().ok().and_then(|g| g.clone())
    }

    /// Holds the profile at `dir` until it is dropped.
    pub(crate) struct Profile {
        // Held for its lifetime; poisoning is irrelevant here, so a poisoned
        // gate is taken anyway rather than failing an unrelated test.
        _gate: MutexGuard<'static, ()>,
    }

    impl Profile {
        pub(crate) fn at(dir: &Path) -> Self {
            let gate = gate().lock().unwrap_or_else(|e| e.into_inner());
            *OVERRIDE.lock().unwrap_or_else(|e| e.into_inner()) = Some(dir.to_path_buf());
            Self { _gate: gate }
        }
    }

    impl Drop for Profile {
        fn drop(&mut self) {
            *OVERRIDE.lock().unwrap_or_else(|e| e.into_inner()) = None;
        }
    }
}

#[cfg(test)]
fn test_home() -> Option<PathBuf> {
    testing::current()
}

#[cfg(test)]
mod tests {
    use super::testing::Profile;
    use super::*;

    /// One folder always lands in one drawer, however it is spelled.
    #[test]
    fn a_kingdom_keys_to_the_same_folder_every_time() {
        let dir = tempfile::tempdir().unwrap();
        let _p = Profile::at(dir.path());

        let root = dir.path().join("dev");
        std::fs::create_dir_all(&root).unwrap();

        let a = kingdom_dir(&root);
        let b = kingdom_dir(&root.join("."));
        assert_eq!(a, b, "the same folder, spelled two ways");
        assert!(a.join("kingdom.json").is_file(), "the drawer says whose it is");
    }

    /// Two projects called `dev` are two kingdoms, not one.
    ///
    /// The readable half of the key is the folder's name, so without the hash
    /// these would share a drawer and silently merge their plans.
    #[test]
    fn two_folders_of_the_same_name_do_not_share_a_drawer() {
        let dir = tempfile::tempdir().unwrap();
        let _p = Profile::at(dir.path());

        let one = dir.path().join("a/dev");
        let two = dir.path().join("b/dev");
        std::fs::create_dir_all(&one).unwrap();
        std::fs::create_dir_all(&two).unwrap();

        assert_ne!(kingdom_dir(&one), kingdom_dir(&two));
    }

    #[test]
    fn the_last_kingdom_survives_a_write_and_a_read() {
        let dir = tempfile::tempdir().unwrap();
        let _p = Profile::at(dir.path());

        assert_eq!(last_kingdom(), None, "nothing recorded yet");

        let root = dir.path().join("dev");
        std::fs::create_dir_all(&root).unwrap();
        remember_kingdom(&root);
        assert_eq!(last_kingdom(), Some(root.canonicalize().unwrap()));

        forget_kingdom();
        assert_eq!(last_kingdom(), None, "and can be taken back");
    }

    /// A blank value must read as "none recorded", not as the empty path --
    /// which would otherwise be opened, fail, and look like a broken profile.
    #[test]
    fn a_blank_setting_is_no_setting() {
        let dir = tempfile::tempdir().unwrap();
        let _p = Profile::at(dir.path());

        std::fs::create_dir_all(dir.path()).unwrap();
        std::fs::write(
            dir.path().join("settings.json"),
            r#"{"last_kingdom":"   "}"#,
        )
        .unwrap();

        assert_eq!(last_kingdom(), None);
    }

    /// Unparseable settings must not take the profile down with them.
    #[test]
    fn a_corrupt_settings_file_reads_as_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let _p = Profile::at(dir.path());

        std::fs::create_dir_all(dir.path()).unwrap();
        std::fs::write(dir.path().join("settings.json"), "{ not json").unwrap();

        assert_eq!(last_kingdom(), None);
    }

    #[test]
    fn an_older_kingdoms_records_are_copied_across_once() {
        let dir = tempfile::tempdir().unwrap();
        let _p = Profile::at(&dir.path().join("profile"));

        let root = dir.path().join("dev");
        let old = root.join(".kingdom");
        std::fs::create_dir_all(old.join("plans")).unwrap();
        std::fs::create_dir_all(old.join("approved")).unwrap();
        std::fs::write(old.join("plans/plan-1.json"), "{}").unwrap();
        std::fs::write(old.join("approved/plan-1.md"), "# agreed").unwrap();

        let line = migrate(&root).expect("something was migrated");
        assert!(line.contains("plans"), "{line}");

        let new = kingdom_dir(&root);
        assert_eq!(
            std::fs::read_to_string(new.join("plans/plan-1.json")).unwrap(),
            "{}"
        );
        assert!(new.join("approved/plan-1.md").is_file());

        // The originals are left alone: a bug here must not cost a plan.
        assert!(old.join("plans/plan-1.json").is_file());

        // And it happens once. A record written since must survive a second
        // open, so the guard is on the destination existing at all.
        std::fs::write(new.join("plans/plan-1.json"), "newer").unwrap();
        assert_eq!(migrate(&root), None, "nothing left to migrate");
        assert_eq!(
            std::fs::read_to_string(new.join("plans/plan-1.json")).unwrap(),
            "newer"
        );
    }

    /// A kingdom that never used the old layout has nothing to say about it.
    #[test]
    fn a_fresh_kingdom_migrates_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let _p = Profile::at(&dir.path().join("profile"));

        let root = dir.path().join("dev");
        std::fs::create_dir_all(&root).unwrap();

        assert_eq!(migrate(&root), None);
    }
}
