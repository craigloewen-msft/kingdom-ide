//! Where the kingdom's own records live.
//!
//! Server-only. Plans are kept as one JSON document each under the kingdom root:
//!
//! ```text
//! <kingdom_root>/.kingdom/
//!   kingdom.json              format version, and when it was last opened
//!   plans/<plan-id>.json      one document per plan
//!   archive/<plan-id>.patch   the work an archived plan set aside
//! ```
//!
//! **Why here and not in each project.** The rail and the map read every plan at
//! once, so sharding them across cities would mean walking every repository on
//! each load -- and losing a plan outright when its city is renamed. More to the
//! point, the King's repository is not ours to write to; `worktree.rs` already
//! made that call when it chose `.git/info/exclude` over `.gitignore`.
//!
//! **Why files and not a database.** There is exactly one writer, the whole
//! dataset already lives in memory, and the most demanding query in the codebase
//! is a filter by city. A database would buy transactions and indexes nothing
//! here needs, and cost a schema kept in sync by hand against types that derive
//! their own serialisation today. This module is the seam: when there really are
//! concurrent writers, swapping it for SQLite touches nothing outside it.
//!
//! Every read is failure-tolerant. A kingdom whose records cannot be parsed
//! opens empty rather than refusing to open, and one unreadable plan costs that
//! plan rather than the whole court.

use kingdom_core::{Plan, PlanId};
use std::path::{Path, PathBuf};

/// Folder under the kingdom root holding everything Kingdom records.
const STATE_DIR: &str = ".kingdom";

/// Bumped when the on-disk shape changes in a way a reader must know about.
///
/// Additive changes do not need it -- new fields carry `#[serde(default)]`, the
/// same way `sandbox` and `working_on` were added to types already on disk.
const FORMAT_VERSION: u32 = 1;

/// The kingdom-level record. Deliberately thin: everything about a *plan* lives
/// in that plan's own file, so this exists to mark the format and nothing else.
#[derive(serde::Serialize, serde::Deserialize)]
struct KingdomRecord {
    version: u32,
    /// When the kingdom was last opened, as milliseconds since the epoch.
    opened_at: u128,
}

fn state_dir(root: &Path) -> PathBuf {
    root.join(STATE_DIR)
}

fn plans_dir(root: &Path) -> PathBuf {
    state_dir(root).join("plans")
}

/// Where an archived plan's patch is kept.
///
/// Public because archiving writes it and the plan's outcome records the path,
/// so the King can find it later without knowing this module's layout.
pub fn archive_patch(root: &Path, id: &PlanId) -> PathBuf {
    state_dir(root).join("archive").join(format!("{id}.patch"))
}

/// Every plan recorded for this kingdom, oldest id first.
///
/// Returns empty for a kingdom that has never been opened, which is the same
/// answer as "no plans yet" and needs no distinguishing: both mean the opening
/// court should be seated.
pub fn load(root: &Path) -> Vec<Plan> {
    let Ok(entries) = std::fs::read_dir(plans_dir(root)) else {
        return Vec::new();
    };

    let mut plans: Vec<Plan> = entries
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        // A document that will not parse is skipped rather than fatal: one bad
        // file must not cost the King his whole court, and he can still see the
        // rest of it while he works out what happened to that one.
        .filter_map(|p| std::fs::read_to_string(&p).ok())
        .filter_map(|raw| serde_json::from_str::<Plan>(&raw).ok())
        .collect();

    // Numeric order, so the rail lists a kingdom's plans in the order they were
    // opened rather than in whatever order the directory happened to yield.
    plans.sort_by_key(|p| (plan_number(&p.id).unwrap_or(u64::MAX), p.id.to_string()));
    plans
}

/// Writes one plan, replacing whatever was recorded for it before.
///
/// Atomic: serialised beside the target and renamed over it, because a
/// half-written document is worse than a missing one -- it is the one state
/// [`load`] cannot tell from corruption.
pub fn save(root: &Path, plan: &Plan) -> std::io::Result<()> {
    let dir = plans_dir(root);
    std::fs::create_dir_all(&dir)?;

    let body = serde_json::to_vec_pretty(plan)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;

    let final_path = dir.join(format!("{}.json", plan.id));
    let tmp = dir.join(format!(".{}.tmp", plan.id));
    std::fs::write(&tmp, &body)?;
    std::fs::rename(&tmp, &final_path)?;

    Ok(())
}

/// Writes every plan, and stamps the kingdom record.
///
/// Used when a kingdom is first opened and a court is seated over it. Returns
/// the first failure, so the caller can report it once rather than per plan.
pub fn save_all(root: &Path, plans: &[Plan]) -> std::io::Result<()> {
    std::fs::create_dir_all(state_dir(root))?;

    let record = KingdomRecord {
        version: FORMAT_VERSION,
        opened_at: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0),
    };
    let body = serde_json::to_vec_pretty(&record)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    std::fs::write(state_dir(root).join("kingdom.json"), body)?;

    for plan in plans {
        save(root, plan)?;
    }

    Ok(())
}

/// The number the next plan should take: one above the highest already recorded.
///
/// Derived from the plans themselves rather than from a stored counter, because
/// a counter can drift from what is actually on disk and `max + 1` cannot. Ids
/// that are not `plan-<number>` -- the sample court's `plan-ramparts`, say --
/// simply do not participate.
pub fn next_number(plans: &[Plan]) -> u64 {
    plans
        .iter()
        .filter_map(|p| plan_number(&p.id))
        .max()
        .map_or(1, |n| n + 1)
}

fn plan_number(id: &PlanId) -> Option<u64> {
    id.as_str().strip_prefix("plan-")?.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use kingdom_core::{CityId, ModelChoice, Outcome, Speaker, Workspace};

    fn plan(id: &str) -> Plan {
        Plan::opened(
            PlanId::new(id),
            CityId::new("testburg"),
            "Do the thing",
            &ModelChoice::new("mock", None),
            Workspace::in_place("/dev/testburg"),
        )
    }

    /// A plan owns a worktree with commits in it, so forgetting a plan orphans
    /// real work on disk -- there would be nothing left that knew what that
    /// checkout was for or which branch to merge it from. This pins the whole
    /// reason the store exists: what goes in comes back out intact, and the id
    /// sequence resumes above what is already recorded rather than colliding
    /// with it.
    #[test]
    fn plans_survive_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();

        assert!(
            load(root).is_empty(),
            "a kingdom never opened before has no records, and must not error"
        );
        assert_eq!(next_number(&[]), 1, "the first plan of a kingdom is plan-1");

        let mut first = plan("plan-1");
        first.say(Speaker::Court, "Here is what I propose.");
        let mut ninth = plan("plan-9");
        ninth.settle(Outcome::Archived {
            branch: "kingdom/abc".into(),
            tip: "deadbeef".into(),
            base: "main".into(),
            base_commit: "cafebabe".into(),
            patch: Some("/dev/testburg/.kingdom/archive/plan-9.patch".into()),
            pruned: true,
        });
        let scripted = plan("plan-ramparts");

        save_all(root, &[first.clone(), ninth.clone(), scripted.clone()]).unwrap();

        let loaded = load(root);
        assert_eq!(
            loaded,
            vec![first, ninth.clone(), scripted],
            "every plan comes back exactly as it went in, transcript and outcome included"
        );

        assert_eq!(
            next_number(&loaded),
            10,
            "ids resume above the highest recorded, so a restart cannot reissue one"
        );

        // Re-saving one plan must replace it rather than accumulate.
        let mut edited = ninth;
        edited.title = "A better title".into();
        save(root, &edited).unwrap();
        let reloaded = load(root);
        assert_eq!(reloaded.len(), 3, "saving a plan again replaces its record");
        assert_eq!(reloaded[1].title, "A better title");
    }
}
