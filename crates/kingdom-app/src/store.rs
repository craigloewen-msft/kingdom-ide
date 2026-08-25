//! Where the kingdom's own records live.
//!
//! Server-only. Plans are kept as one JSON document each under the kingdom root:
//!
//! ```text
//! <kingdom_root>/.kingdom/
//!   plans/<plan-id>.json      one document per plan
//!   archive/<plan-id>.patch   the work an archived plan set aside
//! ```
//!
//! **Why here and not in each project.** The rail and the map read every plan at
//! once, so sharding them across cities would mean walking every repository on
//! each load -- and losing a plan outright when its city is renamed. More to the
//! point, the user's repository is not ours to write to; `worktree.rs` already
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
//! plan rather than the whole model.

use kingdom_core::{Plan, PlanId};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

/// Folder under the kingdom root holding everything Kingdom records.
const STATE_DIR: &str = ".kingdom";

fn state_dir(root: &Path) -> PathBuf {
    root.join(STATE_DIR)
}

fn plans_dir(root: &Path) -> PathBuf {
    state_dir(root).join("plans")
}

/// Where an archived plan's patch is kept.
///
/// Public because archiving writes it and the plan's outcome records the path,
/// so the user can find it later without knowing this module's layout.
pub fn archive_patch(root: &Path, id: &PlanId) -> PathBuf {
    state_dir(root).join("archive").join(format!("{id}.patch"))
}

/// Where the record of an approved plan is kept.
///
/// Markdown rather than JSON because this one is written for a person. Its
/// neighbour `plans/<id>.json` is the machine's copy and the read path; this is
/// the King's ledger of what he agreed to.
pub fn approved_plan(root: &Path, id: &PlanId) -> PathBuf {
    state_dir(root).join("approved").join(format!("{id}.md"))
}

/// Records a plan at the moment the user approved it.
///
/// # Why this is not already covered by `plans/<id>.json`
///
/// That document is the plan as it is *now*, rewritten on every update -- and a
/// plan keeps moving after approval. Revisions replace the standing proposal,
/// the transcript grows, the status settles, and archiving may eventually take
/// the whole thing away. None of that is wrong, but it means the JSON cannot
/// answer "what exactly did I agree to, and when?" once the plan has moved on.
///
/// This can, because it is written once and never touched again. Approval is
/// the one moment in a plan's life where the user commits to something, so it
/// is the one worth freezing: the proposal as it read when he pressed the
/// button, the decree that led to it, and the branch the work will land on.
///
/// Write-once is enforced rather than assumed. A second approval of the same
/// plan -- a stale tab, a revised proposal accepted twice -- must not overwrite
/// the first record, or the ledger silently loses the entry it exists to keep.
pub fn record_approval(root: &Path, plan: &Plan) -> std::io::Result<PathBuf> {
    let Some(proposal) = plan.proposal.as_ref() else {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "a plan with no proposal cannot have been approved",
        ));
    };

    let path = approved_plan(root, &plan.id);
    if path.exists() {
        return Ok(path);
    }

    let dir = path.parent().unwrap_or(root);
    std::fs::create_dir_all(dir)?;

    let mut out = String::new();
    let _ = writeln!(out, "# {}", proposal.title);
    let _ = writeln!(out);
    let _ = writeln!(out, "- **Plan**: `{}`", plan.id);
    let _ = writeln!(out, "- **City**: {}", plan.city);
    let _ = writeln!(out, "- **Approved**: {}", stamp(proposal.at));
    let _ = writeln!(out, "- **Model**: {}{}", plan.model, effort(plan));
    let _ = writeln!(out, "- **Workspace**: `{}`", plan.workspace.path);
    if let Some(branch) = &plan.workspace.branch {
        let _ = writeln!(out, "- **Branch**: `{branch}`");
    }
    let _ = writeln!(out);
    let _ = writeln!(out, "## The decree");
    let _ = writeln!(out);
    for line in plan.prompt.trim().lines() {
        let _ = writeln!(out, "> {line}");
    }
    let _ = writeln!(out);
    let _ = writeln!(out, "## The plan, as approved");
    let _ = writeln!(out);
    out.push_str(proposal.body.trim());
    out.push('\n');

    // Same temp-then-rename as `save`: a reader must never catch this file
    // half-written, and the ledger is exactly the thing nobody re-derives.
    let tmp = dir.join(format!(".{}.tmp", plan.id));
    std::fs::write(&tmp, out.as_bytes())?;
    std::fs::rename(&tmp, &path)?;

    Ok(path)
}

/// A timestamp as a person reads it, or a plain marker when there is none.
///
/// `Timestamp` is milliseconds since the epoch and `kingdom-core` deliberately
/// carries no date formatting, so this does the arithmetic rather than pulling
/// in a calendar crate for one line in one file.
fn stamp(at: Option<kingdom_core::Timestamp>) -> String {
    let Some(kingdom_core::Timestamp(ms)) = at else {
        return "time unrecorded".to_string();
    };

    let secs = ms / 1000;
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);

    // Civil-from-days (Howard Hinnant's algorithm), shifted to a 0000-03-01 era.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = era * 400 + yoe + i64::from(m <= 2);

    format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02} UTC",
        rem / 3_600,
        (rem % 3_600) / 60
    )
}

/// The effort suffix, where the user asked for one.
fn effort(plan: &Plan) -> String {
    plan.effort
        .map(|e| format!(" · {}", e.wire_name()))
        .unwrap_or_default()
}

/// Every plan recorded for this kingdom, oldest id first.
///
/// Returns empty for a kingdom that has never been opened, which is the same
/// answer as "no plans yet" and needs no distinguishing: both mean the opening
/// model should be seated.
pub fn load(root: &Path) -> Vec<Plan> {
    let Ok(entries) = std::fs::read_dir(plans_dir(root)) else {
        return Vec::new();
    };

    let mut plans: Vec<Plan> = entries
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        // A document that will not parse is skipped rather than fatal: one bad
        // file must not cost the user his whole model, and he can still see the
        // rest of it while he works out what happened to that one.
        .filter_map(|p| std::fs::read_to_string(&p).ok())
        .filter_map(|raw| serde_json::from_str::<Plan>(&raw).ok())
        .map(reconcile)
        .collect();

    // Numeric order, so the rail lists a kingdom's plans in the order they were
    // opened rather than in whatever order the directory happened to yield.
    plans.sort_by_key(|p| (plan_number(&p.id).unwrap_or(u64::MAX), p.id.to_string()));
    plans
}

/// Repairs a plan whose turn died with the process that was taking it.
///
/// A plan is marked `Drafting` for as long as the model is working, and the
/// mark is cleared when the turn ends. If the server stops in between -- a
/// crash, a rebuild, Ctrl-C -- the record on disk keeps a status that says
/// "working" with nothing working on it, and the plan is *stuck*: the
/// conversation disables its composer while a plan is drafting, so the user
/// cannot even say something to nudge it. Nothing on the running server can fix
/// it, because the task that would have is gone.
///
/// Reconciling on load is the only place with enough information to know: a
/// process that has just started cannot have a turn already in flight, so any
/// plan claiming one was interrupted. The work it did is still in its
/// workspace; only the conversation was cut off, which is why this leaves a
/// note rather than discarding anything.
///
/// This matters far more than it used to. A turn was one HTTP call and the
/// window was a second or two; a turn is now a loop that can run for minutes.
fn reconcile(mut plan: Plan) -> Plan {
    use kingdom_core::{ToolOutcome, Entry, NoteKind, PlanStatus, Speaker};

    if plan.status != PlanStatus::Drafting {
        return plan;
    }

    // A plan the user opened but whose first turn never began looks exactly the
    // same on disk: `Drafting`, busy with nothing. It is *not* damaged -- the
    // conversation starts it on mount -- so touching it here would mark a
    // perfectly healthy new plan as failed before it had a chance. The
    // difference is whether anything but the user has been in the log.
    let had_begun = plan
        .transcript
        .iter()
        .any(|e| !matches!(e, Entry::Message(u) if u.speaker == Speaker::User));

    if !had_begun {
        return plan;
    }

    // A call recorded as begun but never settled is one the process died
    // during. Left in flight it would be replayed to the model forever as a
    // command still running, so it is closed with the truth.
    let orphans: Vec<String> = plan
        .transcript
        .iter()
        .filter_map(|e| match e {
            Entry::Tool(d) if d.in_flight() => Some(d.id.clone()),
            _ => None,
        })
        .collect();
    for id in orphans {
        plan.settle_tool_call_at_an_unknown_time(
            &id,
            ToolOutcome::Refused {
                reason: "The server stopped while this was running. Whether it \
                         finished is unknown."
                    .to_string(),
            },
        );
    }

    plan.working_on = None;
    plan.status = PlanStatus::Failed;
    plan.summary = "Interrupted when the server stopped.".to_string();
    plan.note(
        NoteKind::Failed,
        "This plan was mid-turn when the server stopped, so the court never \
         finished. Anything it had already done is still in its workspace. Say \
         something to set it going again.",
    );

    plan
}

/// Writes one plan, replacing whatever was recorded for it before.
///
/// Atomic: serialised beside the target and renamed over it, because a
/// half-written document is worse than a missing one -- it is the one state
/// [`load`] cannot tell from corruption.
///
/// **Pictures are not written.** A tool call that looked at an image carries
/// the bytes in memory so the model can be shown them for the rest of the turn,
/// but this file is rewritten on *every* update to the plan -- so persisting
/// them would mean rewriting a megabyte per screenshot for the life of the
/// plan, to store something nothing ever reads back. The words survive (`"Image
/// loaded: <path> (N bytes)"`) and so does the file in the workspace, so a
/// reloaded plan can still say what was looked at.
///
/// **The paths to them are.** A tool call's artifacts are names, not payloads,
/// and they are exactly what lets a reloaded conversation show the picture
/// again -- the file is still in the workspace and [`crate::artifact`] serves
/// it. The two look alike and only one is dropped; that asymmetry is the whole
/// design, and [`kingdom_core::ToolArtifact`] has the rest of the reasoning.
pub fn save(root: &Path, plan: &Plan) -> std::io::Result<()> {
    let dir = plans_dir(root);
    std::fs::create_dir_all(&dir)?;

    let body = serde_json::to_vec_pretty(&without_images(plan))
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;

    let final_path = dir.join(format!("{}.json", plan.id));
    let tmp = dir.join(format!(".{}.tmp", plan.id));
    std::fs::write(&tmp, &body)?;
    std::fs::rename(&tmp, &final_path)?;

    Ok(())
}

/// A copy of this plan with every tool call's images stripped, ready for disk.
///
/// Cloned rather than stripping in place because the caller's plan is the live
/// one, still being shown to a model this turn: blinding it as a side effect of
/// saving would be a memory bug disguised as a storage decision.
///
/// Artifacts are deliberately left alone -- see [`save`].
fn without_images(plan: &Plan) -> Plan {
    use kingdom_core::Entry;

    let mut plan = plan.clone();
    for entry in &mut plan.transcript {
        if let Entry::Tool(tool_call) = entry {
            tool_call.outcome = tool_call.outcome.take().map(kingdom_core::ToolOutcome::without_images);
        }
    }
    plan
}

/// Writes every plan.
///
/// Used when a kingdom is first opened and its starter plans are seeded over
/// it. Returns the first failure, so the caller can report it once rather than
/// per plan.
pub fn save_all(root: &Path, plans: &[Plan]) -> std::io::Result<()> {
    std::fs::create_dir_all(state_dir(root))?;

    for plan in plans {
        save(root, plan)?;
    }

    Ok(())
}

/// The number the next plan should take: one above the highest already recorded.
///
/// Derived from the plans themselves rather than from a stored counter, because
/// a counter can drift from what is actually on disk and `max + 1` cannot. Ids
/// that are not `plan-<number>` -- the sample model's `plan-ramparts`, say --
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

    /// The ledger keeps what the King agreed to, in the words he read.
    ///
    /// The plan's own JSON cannot answer this later: it is rewritten on every
    /// update, so a revision after approval replaces the proposal and the
    /// original terms are gone. This pins that the record carries the decree,
    /// the approved body, and where the work will land.
    #[test]
    fn an_approved_plan_is_written_down_for_the_king() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();

        let mut p = plan("plan-1");
        p.propose("Remember the folder", "# Remember the folder\n\nStore the root.");
        assert!(p.approve());

        let path = record_approval(root, &p).expect("the ledger entry is written");
        let body = std::fs::read_to_string(&path).unwrap();

        assert!(path.starts_with(root.join(".kingdom").join("approved")), "{path:?}");
        assert!(body.contains("Remember the folder"), "{body}");
        assert!(body.contains("Store the root."), "the approved plan itself: {body}");
        assert!(body.contains("Do the thing"), "the decree that led to it: {body}");
        assert!(body.contains("plan-1") && body.contains("testburg"), "{body}");
    }

    /// Approving twice must not rewrite the first entry.
    ///
    /// Reachable without doing anything strange: a stale tab, or a proposal
    /// revised and accepted again. The ledger's whole value is that an entry,
    /// once written, still says what it said -- so the second approval is a
    /// no-op rather than an overwrite.
    #[test]
    fn the_first_approval_is_the_one_that_is_kept() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();

        let mut p = plan("plan-2");
        p.propose("First terms", "# First terms\n\nAs originally agreed.");
        assert!(p.approve());
        record_approval(root, &p).unwrap();

        // The court revises, and the King accepts again.
        p.propose("Second terms", "# Second terms\n\nSomething else entirely.");
        assert!(p.approve());
        let path = record_approval(root, &p).unwrap();

        let body = std::fs::read_to_string(&path).unwrap();
        assert!(body.contains("As originally agreed."), "{body}");
        assert!(
            !body.contains("Something else entirely."),
            "a later approval must not rewrite the record of the first: {body}"
        );
    }

    /// A plan with nothing standing cannot have been approved, and saying so
    /// beats writing a record with an empty body in it.
    #[test]
    fn a_plan_with_no_proposal_records_nothing() {
        let dir = tempfile::tempdir().unwrap();
        assert!(record_approval(dir.path(), &plan("plan-3")).is_err());
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
        first.say(Speaker::Assistant, "Here is what I propose.");
        // Settled out of Drafting, as it would be once the model had replied.
        // Left mid-turn it would be *repaired* on load rather than returned
        // verbatim, which is the neighbouring test's business, not this one's.
        first.status = kingdom_core::PlanStatus::AwaitingReview;
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

    /// A turn that died with the process leaves a record claiming to be working
    /// with nothing working on it, and that plan is *unusable*: the
    /// conversation disables its composer while a plan drafts, so the user
    /// cannot even nudge it, and nothing on the fresh server knows to. Load is
    /// the only place with the standing to fix it -- a process that has just
    /// started cannot have a turn in flight.
    ///
    /// The two halves must both hold. A freshly opened plan looks identical on
    /// disk (Drafting, not busy) and is perfectly healthy, so failing that one
    /// would break every plan the user opens just before a restart.
    #[test]
    fn an_interrupted_turn_is_repaired_but_an_unstarted_one_is_left_alone() {
        use kingdom_core::{ToolCall, NoteKind, PlanStatus};

        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();

        // Opened a moment before the server stopped: nothing has happened yet.
        let unstarted = plan("plan-1");

        // Mid-turn: the model had spoken and a tool call was still running.
        let mut interrupted = plan("plan-2");
        interrupted.say(Speaker::Assistant, "I will look into it.");
        interrupted.begin_tool_call(ToolCall::started("call-1", "bash", serde_json::json!({})));
        interrupted.working_on = Some("bash: cargo test".into());

        save_all(root, &[unstarted, interrupted]).unwrap();
        let loaded = load(root);

        assert_eq!(
            loaded[0].status,
            PlanStatus::Drafting,
            "a plan whose first turn had not begun is healthy and must be left to start"
        );

        let repaired = &loaded[1];
        assert_eq!(repaired.status, PlanStatus::Failed);
        assert!(
            !repaired.is_busy(),
            "the busy mark must be cleared, or the plan stays stuck forever"
        );
        assert!(
            repaired.transcript.iter().any(
                |e| matches!(e, kingdom_core::Entry::Note(n) if n.kind == NoteKind::Failed)
            ),
            "the King must be told why, not just find a plan that failed silently"
        );
        assert!(
            repaired.turns().all(|t| !matches!(t, kingdom_core::Turn::Tool(d) if d.in_flight())),
            "a call left in flight would be replayed to the model as still running, forever"
        );
    }

    /// Three things disk must get right about a picture, pinned together
    /// because they are the same decision seen from different sides.
    ///
    /// A screenshot must not be *written*: this file is rewritten on every
    /// update to the plan, so persisting image payloads would cost a megabyte
    /// per screenshot per save, forever, to store something nothing reads back.
    /// The *path* to it must be, because that is what lets a reloaded
    /// conversation show the picture again. And a document written before
    /// images existed must still *load*, because the alternative is a user
    /// whose model vanishes after an upgrade.
    #[test]
    fn a_picture_is_shown_but_never_filed() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();

        let mut seen = plan("plan-1");
        seen.status = kingdom_core::PlanStatus::AwaitingReview;
        seen.begin_tool_call(kingdom_core::ToolCall::started(
            "call-1",
            "read_image",
            serde_json::json!({ "path": "shot.png" }),
        ));
        seen.settle_tool_call(
            "call-1",
            kingdom_core::ToolOutcome::seen(
                "Looked at shot.png (3 bytes).",
                vec![kingdom_core::ToolImage {
                    media_type: "image/png".into(),
                    data: "QUJD".repeat(1000),
                }],
            )
            .leaving(vec![kingdom_core::ToolArtifact {
                path: "shot.png".into(),
                media_type: "image/png".into(),
            }]),
        );

        save(root, &seen).unwrap();

        // The live plan keeps its picture: saving must not blind the model
        // mid-turn.
        assert_eq!(
            seen.turns()
                .filter_map(|t| match t {
                    kingdom_core::Turn::Tool(d) => Some(d.shown().len()),
                    _ => None,
                })
                .sum::<usize>(),
            1,
            "saving must not strip the plan the caller still holds"
        );

        let raw = std::fs::read_to_string(root.join(".kingdom/plans/plan-1.json")).unwrap();
        assert!(
            !raw.contains("QUJD"),
            "image payloads must never reach disk"
        );
        assert!(
            raw.contains("Looked at shot.png"),
            "the words must survive, so a reloaded plan can say what was looked at"
        );

        let reloaded = load(root);
        let tool_call = reloaded[0]
            .turns()
            .find_map(|t| match t {
                kingdom_core::Turn::Tool(d) => Some(d.clone()),
                _ => None,
            })
            .expect("the deed itself is still recorded");
        assert!(tool_call.shown().is_empty());
        assert_eq!(tool_call.report(), "Looked at shot.png (3 bytes).");
        assert_eq!(
            tool_call.artifacts().iter().map(|a| a.path.as_str()).collect::<Vec<_>>(),
            vec!["shot.png"],
            "the path must survive the round trip, or a reloaded chamber has \
             nothing to point an <img> at"
        );
    }

    /// A plan document written before tool calls could carry images -- no
    /// `images` key anywhere -- must still load. Written as literal JSON rather
    /// than by round-tripping today's types, because a round trip would
    /// serialise the *current* shape and prove nothing about the old one.
    #[test]
    fn a_plan_recorded_before_images_existed_still_loads() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join(".kingdom/plans")).unwrap();

        let old = r#"{
            "id": "plan-1",
            "city": "testburg",
            "title": "Do the thing",
            "slug": "do-the-thing",
            "summary": "",
            "prompt": "Do the thing",
            "model": "mock",
            "effort": null,
            "transcript": [
                { "Tool": {
                    "id": "call-1",
                    "tool": "bash",
                    "input": { "cmd": "cargo test" },
                    "outcome": { "Done": { "output": "ok" } },
                    "at": 1787000000000
                } }
            ],
            "status": "AwaitingReview",
            "outcome": null,
            "workspace": {
                "mode": "InPlace",
                "path": "/dev/testburg",
                "branch": null,
                "id": null,
                "base": null
            },
            "working_on": null
        }"#;
        std::fs::write(root.join(".kingdom/plans/plan-1.json"), old).unwrap();

        let loaded = load(root);
        assert_eq!(loaded.len(), 1, "an older document must not be skipped");
        let tool_call = loaded[0]
            .turns()
            .find_map(|t| match t {
                kingdom_core::Turn::Tool(d) => Some(d.clone()),
                _ => None,
            })
            .expect("its deed must survive the upgrade");
        assert_eq!(tool_call.report(), "ok");
        assert!(tool_call.shown().is_empty(), "absent means no pictures, not a parse failure");
    }
}

