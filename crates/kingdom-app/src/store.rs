//! Where the kingdom's own records live.
//!
//! Server-only. Plans are kept as one JSON document each, in the King's profile
//! ([`crate::profile`]) under a folder of that kingdom's own:
//!
//! ```text
//! ~/.kingdom/kingdoms/<key>/
//!   plans/<plan-id>.json             one document per plan, the read path
//!   plans/<plan-id>--<slug>.md       the plan itself, as the court wrote it
//!   archive/<plan-id>.patch          the work an archived plan set aside
//! ```
//!
//! **Why the plan is a file beside its JSON.** The court drafts its plan to
//! `.kingdom/draft.md` in its own worktree, and that checkout is deleted when
//! the plan is merged or archived -- taking the draft with it, since
//! `.kingdom/` is excluded from the repository and so never committed. So the
//! draft is copied out here, once, at approval or at the end. See
//! [`file_plan`]. It replaces an earlier `approved/<id>.md` ledger that held a
//! second rendering of the same prose; one document is the point.
//!
//! **Why not in each project.** The rail and the map read every plan at once, so
//! sharding them across cities would mean walking every repository on each load
//! -- and losing a plan outright when its city is renamed. More to the point,
//! the user's repository is not ours to write to; `worktree.rs` already made
//! that call when it chose `.git/info/exclude` over `.gitignore`.
//!
//! **Why not in the kingdom root either.** These are records of Kingdom's own
//! work, not the dev folder's, and they should outlive any one checkout of it.
//! [`crate::profile`] has the rest of that reasoning, and the migration for
//! kingdoms written under the old layout.
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

/// Where everything recorded for `root` is kept.
///
/// The single seam between this module and the profile's layout: `plans_dir`
/// and [`archive_patch`] both hang off it, so moving the records again is this
/// one function.
fn state_dir(root: &Path) -> PathBuf {
    crate::profile::kingdom_dir(root)
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

/// Where this plan's own document is filed.
///
/// `plans/<plan-id>--<slug>.md`, beside the JSON of the same plan. Markdown
/// because this one is written for a person: its neighbour `plans/<id>.json` is
/// the machine's copy and the read path, and this is the plan itself, in the
/// words the King read.
///
/// The id leads and the slug follows. Slugs are not unique -- two plans opened
/// from similar decrees genuinely share one -- so the id is what makes the name
/// unambiguous, and the slug is what makes it readable in `ls`. It is the same
/// slug the plan's branch was cut from, so `plan-12--tidy-the-sidebar.md` sits
/// beside `kingdom/tidy-the-sidebar`.
///
/// A plan recorded before slugs existed has an empty one (`#[serde(default)]`),
/// and falls back to plain `<plan-id>.md` rather than growing a trailing `--`.
pub fn filed_plan(root: &Path, plan: &Plan) -> PathBuf {
    let name = match plan.slug.trim() {
        "" => format!("{}.md", plan.id),
        slug => format!("{}--{slug}.md", plan.id),
    };
    plans_dir(root).join(name)
}

/// Files a plan's own document, from the draft the court wrote.
///
/// # Why the plan is filed at all
///
/// The court writes its plan to `.kingdom/draft.md` inside its worktree and
/// revises it there as it works -- that file is the plan, and
/// [`crate::tools::propose_plan`] explains why it has to be a file rather than
/// an argument. But `.kingdom/` is excluded from the repository and
/// `git worktree remove` deletes it, so without this the one document the court
/// actually wrote is the one thing that does not survive.
///
/// So it is copied out, once, into the kingdom's records. `body` is the draft's
/// own bytes rather than `proposal.body` re-rendered: they are the same words,
/// and reading the file keeps this honest about which copy is the original.
///
/// # Why here rather than in the user's repository
///
/// Phoenix keeps its task files in the project and commits them. That is the
/// part deliberately not copied: a plan is Kingdom's bookkeeping about the
/// user's project, not the project's own content, and
/// [`crate::profile`] made that call for every other record already.
///
/// # Write-once
///
/// Enforced rather than assumed. A plan is filed at approval and *again* when
/// it is merged or archived, because a plan can end without ever having been
/// approved -- so the second call is the ordinary case, not the exceptional
/// one, and it must not overwrite what the King actually agreed to with
/// whatever the draft says by the end. After approval the court holds an
/// unrestricted `patch` and could rewrite the draft freely; filing at the
/// moment of the grant is what puts the agreed text safely on disk first.
pub fn file_plan(root: &Path, plan: &Plan, body: &str) -> std::io::Result<PathBuf> {
    if body.trim().is_empty() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "a plan with an empty draft has nothing to file",
        ));
    }

    let path = filed_plan(root, plan);
    if path.exists() {
        return Ok(path);
    }

    let dir = path.parent().unwrap_or(root);
    std::fs::create_dir_all(dir)?;

    let mut out = String::new();
    let _ = writeln!(out, "- **Plan**: `{}`", plan.id);
    let _ = writeln!(out, "- **City**: {}", plan.city);
    let _ = writeln!(out, "- **Filed**: {}", stamp(filed_at(plan)));
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

    // The draft last and whole, including its own `# H1` -- which `propose_plan`
    // has already refused a draft for lacking, so the document is titled by the
    // court's own headline rather than by anything invented here.
    out.push_str(body.trim());
    out.push('\n');

    // Same temp-then-rename as `save`: a reader must never catch this file
    // half-written, and this is exactly the thing nobody re-derives.
    let tmp = dir.join(format!(".{}.md.tmp", plan.id));
    std::fs::write(&tmp, out.as_bytes())?;
    std::fs::rename(&tmp, &path)?;

    Ok(path)
}

/// When to say the plan was filed.
///
/// The proposal's own timestamp when there is one, because that is the moment
/// the plan was put to the King and the moment the words were fixed. `None`
/// falls through to [`stamp`]'s marker for a plan filed having never proposed.
fn filed_at(plan: &Plan) -> Option<kingdom_core::Timestamp> {
    plan.proposal.as_ref().and_then(|p| p.at)
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
    use crate::profile::testing::Profile;
    use kingdom_core::{CityId, ModelChoice, Outcome, Speaker, Workspace};

    /// A kingdom root, and a profile to record it in.
    ///
    /// The records no longer live under the root, so a test needs both: a
    /// folder to *be* the kingdom, and somewhere disposable for the profile.
    /// The guard also serialises these tests against each other, because the
    /// profile location is process-global.
    ///
    /// That every assertion below still passes with the profile pointed at a
    /// directory of its own is itself the check that nothing writes into the
    /// kingdom root any more.
    fn kingdom() -> (tempfile::TempDir, Profile, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let guard = Profile::at(&dir.path().join("profile"));
        let root = dir.path().join("dev");
        std::fs::create_dir_all(&root).unwrap();
        (dir, guard, root)
    }

    fn plan(id: &str) -> Plan {
        Plan::opened(
            PlanId::new(id),
            CityId::new("testburg"),
            "Do the thing",
            &ModelChoice::new("mock", None),
            Workspace::in_place("/dev/testburg"),
        )
    }

    /// The filed plan carries the court's own draft, not a re-rendering of it.
    ///
    /// This is the document that outlives the worktree: the draft file is
    /// deleted with the checkout, so if it is not copied out here the one thing
    /// the court actually wrote is gone. Pins that the draft's own words are
    /// what lands, along with the decree and where the work was done.
    #[test]
    fn a_plan_is_filed_from_the_draft_the_court_wrote() {
        let (_dir, _profile, root) = kingdom();
        let root = root.as_path();

        let mut p = plan("plan-1");
        p.propose("Remember the folder", "# Remember the folder\n\nStore the root.");
        assert!(p.approve());

        let path = file_plan(root, &p, "# Remember the folder\n\nStore the root.\n")
            .expect("the plan is filed");
        let body = std::fs::read_to_string(&path).unwrap();

        // Beside the JSON, named so a person can read it in `ls`.
        assert!(path.starts_with(plans_dir(root)), "{path:?}");
        assert_eq!(
            path.file_name().unwrap(),
            format!("plan-1--{}.md", p.slug).as_str(),
            "the id makes it unique and the slug makes it readable"
        );

        assert!(body.contains("Remember the folder"), "{body}");
        assert!(body.contains("Store the root."), "the plan itself: {body}");
        assert!(body.contains("Do the thing"), "the decree that led to it: {body}");
        assert!(body.contains("plan-1") && body.contains("testburg"), "{body}");
    }

    /// A plan whose record predates slugs is still filed, under a plain name.
    ///
    /// `slug` is `#[serde(default)]`, so an older record genuinely has an empty
    /// one. Without this the filename would grow a trailing `--`.
    #[test]
    fn a_plan_without_a_slug_is_filed_under_its_id_alone() {
        let (_dir, _profile, root) = kingdom();
        let root = root.as_path();

        let mut p = plan("plan-4");
        p.slug = String::new();

        let path = file_plan(root, &p, "# Untitled\n\nWords.\n").unwrap();
        assert_eq!(path.file_name().unwrap(), "plan-4.md");
    }

    /// Filing twice must not rewrite the first document.
    ///
    /// The ordinary path, not an exotic one: a plan is filed when the King
    /// approves it and *again* when it is merged or archived. Between those two
    /// moments the court holds an unrestricted `patch` and can rewrite its own
    /// draft freely -- so without write-once, finishing a plan would replace
    /// what the King agreed to with whatever the draft happened to say at the
    /// end.
    #[test]
    fn the_first_filing_is_the_one_that_is_kept() {
        let (_dir, _profile, root) = kingdom();
        let root = root.as_path();

        let mut p = plan("plan-2");
        p.propose("First terms", "# First terms\n\nAs originally agreed.");
        assert!(p.approve());
        file_plan(root, &p, "# First terms\n\nAs originally agreed.\n").unwrap();

        // The court rewrites its draft, and the plan is finished.
        let path = file_plan(root, &p, "# Second terms\n\nSomething else entirely.\n").unwrap();

        let body = std::fs::read_to_string(&path).unwrap();
        assert!(body.contains("As originally agreed."), "{body}");
        assert!(
            !body.contains("Something else entirely."),
            "a later filing must not rewrite the first: {body}"
        );
    }

    /// A plan that never drafted has nothing to file, and saying so beats
    /// writing a document with nothing in it.
    #[test]
    fn an_empty_draft_files_nothing() {
        let (_dir, _profile, root) = kingdom();
        assert!(file_plan(&root, &plan("plan-3"), "   \n").is_err());
        assert!(!filed_plan(&root, &plan("plan-3")).exists());
    }

    /// A filed plan sits in `plans/` beside the JSON, and must not be mistaken
    /// for one.
    ///
    /// `load` filters on the `json` extension, which is what makes it safe to
    /// put the markdown in the same directory rather than inventing another.
    /// If that filter ever loosened, every filed plan would come back as an
    /// unparseable record.
    #[test]
    fn a_filed_plan_is_not_loaded_as_a_record() {
        let (_dir, _profile, root) = kingdom();
        let root = root.as_path();

        let p = plan("plan-1");
        save(root, &p).unwrap();
        file_plan(root, &p, "# The plan\n\nWhat I would do.\n").unwrap();

        assert_eq!(
            load(root),
            vec![p],
            "the markdown beside the JSON must be ignored by the loader"
        );
    }

    /// A plan owns a worktree with commits in it, so forgetting a plan orphans
    /// real work on disk -- there would be nothing left that knew what that
    /// checkout was for or which branch to merge it from. This pins the whole
    /// reason the store exists: what goes in comes back out intact, and the id
    /// sequence resumes above what is already recorded rather than colliding
    /// with it.
    #[test]
    fn plans_survive_a_restart() {
        let (_dir, _profile, root) = kingdom();
        let root = root.as_path();

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

        let (_dir, _profile, root) = kingdom();
        let root = root.as_path();

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

    /// Words the King spoke while the court was working must survive a
    /// restart, and for a reason the rest of the queue's design depends on:
    /// the queue is the *only* place they exist. They are deliberately kept out
    /// of the transcript until they are heard, so a crash between speaking and
    /// hearing would otherwise lose them outright -- with the user believing
    /// he had already given the instruction.
    ///
    /// Note the contrast with `reconcile` above, which repairs a wedged plan:
    /// queued words are *not* something to repair. They are still waiting, and
    /// the next turn drains them.
    #[test]
    fn words_waiting_to_be_heard_survive_a_restart() {
        let (_dir, _profile, root) = kingdom();
        let root = root.as_path();

        let mut waiting = plan("plan-1");
        waiting.status = kingdom_core::PlanStatus::AwaitingReview;
        waiting.queue("first");
        waiting.queue("second");

        save(root, &waiting).unwrap();
        let loaded = load(root);

        let bodies: Vec<&str> = loaded[0].queued.iter().map(|w| w.body.as_str()).collect();
        assert_eq!(
            bodies,
            vec!["first", "second"],
            "queued words and their order must both come back"
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
        let (_dir, _profile, root) = kingdom();
        let root = root.as_path();

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

        let raw = std::fs::read_to_string(plans_dir(root).join("plan-1.json")).unwrap();
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

    /// What the court said as it worked survives disk.
    ///
    /// The failure this guards is worse than the feature being absent: a chamber
    /// that draws the remark live and loses it on reload teaches the King not to
    /// trust the record. Narration is also the one thing on a deed that is
    /// *deliberately* not stripped -- `without_images` reaches into the same
    /// tool call to strip the picture, and a stray broadening of it here would
    /// silently take the words with it.
    #[test]
    fn the_words_the_court_said_while_working_survive_the_round_trip() {
        let (_dir, _profile, root) = kingdom();
        let root = root.as_path();

        let said = "I'll read the two callers before I change the signature.";
        let mut spoke = plan("plan-1");
        spoke.begin_tool_call(
            kingdom_core::ToolCall::started(
                "call-1",
                "read_file",
                serde_json::json!({ "path": "src/lib.rs" }),
            )
            .in_reply(
                "reply-1",
                Some(kingdom_core::Reasoning {
                    text: Some("Two callers, so the signature is the risk.".to_string()),
                    opaque: Default::default(),
                }),
                Some(said.to_string()),
            ),
        );
        spoke.settle_tool_call(
            "call-1",
            kingdom_core::ToolOutcome::done("pub fn open() {}"),
        );

        save(root, &spoke).unwrap();

        let reloaded = load(root);
        let tool_call = reloaded[0]
            .turns()
            .find_map(|t| match t {
                kingdom_core::Turn::Tool(d) => Some(d.clone()),
                _ => None,
            })
            .expect("the deed itself is still recorded");

        assert_eq!(
            tool_call.narration.as_deref(),
            Some(said),
            "the court's own words are the reason for the deed and must outlive a restart"
        );
        assert_eq!(
            tool_call.reasoning.and_then(|r| r.text).as_deref(),
            Some("Two callers, so the signature is the risk."),
            "and so must the thinking, which the chamber folds away rather than drops"
        );
    }

    /// A plan document written before tool calls could carry images -- no
    /// `images` key anywhere -- must still load. Written as literal JSON rather
    /// than by round-tripping today's types, because a round trip would
    /// serialise the *current* shape and prove nothing about the old one.
    #[test]
    fn a_plan_recorded_before_images_existed_still_loads() {
        let (_dir, _profile, root) = kingdom();
        let root = root.as_path();
        std::fs::create_dir_all(plans_dir(root)).unwrap();

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
        std::fs::write(plans_dir(root).join("plan-1.json"), old).unwrap();

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

