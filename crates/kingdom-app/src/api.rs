//! Server functions: the typed bridge between browser and server.
//!
//! Leptos `#[server]` functions compile to a real HTTP call on the client and
//! a direct invocation on the server, sharing one signature. That is the main
//! reason this project is Rust on both ends — there is no hand-written client,
//! no schema to keep in sync, and a domain type change breaks the build rather
//! than failing at runtime.

use kingdom_core::{Disposition, Kingdom, ModelCatalogue, ModelChoice, Plan, WorkspaceMode};
#[cfg(feature = "ssr")]
use kingdom_core::{NoteKind, PlanId};
use leptos::prelude::*;

/// In-memory kingdom state, backed by the records on disk.
///
/// A process-global `Mutex` is the right amount of machinery for a single-user
/// local tool: it is the read path, and [`crate::store`] is the write-through
/// behind it. Both sit behind these server functions, so swapping in SQLite
/// later touches only that module.
#[cfg(feature = "ssr")]
mod state {
    use kingdom_core::Kingdom;
    use std::sync::atomic::AtomicU64;
    use std::sync::{Mutex, OnceLock};

    static KINGDOM: OnceLock<Mutex<Kingdom>> = OnceLock::new();

    pub fn get() -> &'static Mutex<Kingdom> {
        KINGDOM.get_or_init(|| Mutex::new(Kingdom::unopened()))
    }

    /// Monotonic counter behind plan ids, seeded from the records on disk when a
    /// kingdom is opened so a restart cannot reissue an id already in use.
    pub static PLAN_SEQ: AtomicU64 = AtomicU64::new(1);
}

/// Locks the kingdom, turning a poisoned mutex into a server error.
#[cfg(feature = "ssr")]
fn lock() -> Result<std::sync::MutexGuard<'static, Kingdom>, ServerFnError> {
    state::get()
        .lock()
        .map_err(|e| ServerFnError::new(format!("kingdom state poisoned: {e}")))
}

#[cfg(feature = "ssr")]
fn next_plan_number() -> u64 {
    state::PLAN_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

/// Applies a change to one plan, records it, and returns the result, so callers
/// hand the browser the same value that was just stored.
///
/// The single funnel for plan mutations, which is why persistence hangs off it:
/// a caller cannot change a plan and forget to write it.
#[cfg(feature = "ssr")]
fn update(kingdom: &mut Kingdom, id: &PlanId, change: impl FnOnce(&mut Plan)) -> Option<Plan> {
    let root = std::path::PathBuf::from(&kingdom.root);
    let plan = kingdom.plans.iter_mut().find(|p| &p.id == id)?;
    change(plan);
    remember(&root, plan);
    Some(plan.clone())
}

/// Writes a plan to the records, turning a failed write into something the King
/// can see rather than something that fails his decree.
///
/// Refusing the work because the disk was full would be a worse outcome than an
/// unsaved plan he can see is unsaved -- the work itself is on a branch either
/// way, and it is only the bookkeeping that was lost.
#[cfg(feature = "ssr")]
fn remember(root: &std::path::Path, plan: &mut Plan) {
    if let Err(e) = crate::store::save(root, plan) {
        plan.note(
            NoteKind::Failed,
            format!(
                "Could not record this plan under {}: {e}. \
                 It will be forgotten when the server restarts.",
                root.display()
            ),
        );
    }
}

/// Returns the currently open kingdom, or an empty one if none is open.
#[server(GetKingdom, "/api")]
pub async fn get_kingdom() -> Result<Kingdom, ServerFnError> {
    Ok(lock()?.clone())
}

/// Opens a dev folder as the kingdom: scans it for cities and seats a court.
#[server(OpenKingdom, "/api")]
pub async fn open_kingdom(path: String) -> Result<Kingdom, ServerFnError> {
    use std::path::PathBuf;

    let expanded = expand_home(&path);
    let root = PathBuf::from(&expanded);

    if !root.is_dir() {
        return Err(ServerFnError::new(format!(
            "No such folder: {expanded}. Give an absolute path to your dev folder."
        )));
    }

    enforce_sandbox(&root)?;
    assemble(&root, None)
}

/// Seeds a proving ground if needed and opens it.
///
/// The one-click path from a cold clone to a populated map. It exists so the
/// *safe* option is also the *easy* one: the opening screen otherwise demands an
/// absolute path to real work before showing anything at all, which makes
/// pointing the tool at real files the default first act.
#[server(EnterProvingGrounds, "/api")]
pub async fn enter_proving_grounds(realm: Option<String>) -> Result<Kingdom, ServerFnError> {
    use kingdom_core::mockdata;

    let name = realm.unwrap_or_else(|| mockdata::DEFAULT_REALM.to_string());
    let spec = mockdata::realm(&name).ok_or_else(|| {
        ServerFnError::new(format!(
            "No such realm: {name}. Known realms: {}.",
            mockdata::realm_names().join(", ")
        ))
    })?;

    let root = crate::mock::realm_path(&name);

    // Only seed when it is not already there, so entering twice is instant and
    // does not silently discard a realm the King has been poking at.
    if !crate::mock::is_proving_ground(&root) {
        crate::mock::seed(&spec, &root)
            .map_err(|e| ServerFnError::new(format!("Could not raise the proving grounds: {e}")))?;
    }

    let root = root
        .canonicalize()
        .map_err(|e| ServerFnError::new(format!("Could not resolve {}: {e}", root.display())))?;

    assemble(&root, Some(spec.court))
}

/// Every realm the King can enter, for the opening screen.
#[server(ListRealms, "/api")]
pub async fn list_realms() -> Result<Vec<(String, String)>, ServerFnError> {
    Ok(kingdom_core::mockdata::realms()
        .into_iter()
        .map(|r| (r.name.to_string(), r.blurb.to_string()))
        .collect())
}

/// Scans a folder and seats a court over it, then stores it as the kingdom.
///
/// Shared by the real-folder and proving-ground paths so the two cannot drift:
/// a proving ground goes through exactly the same scanner, and the only
/// difference is which court is seated and the `sandbox` flag.
#[cfg(feature = "ssr")]
fn assemble(
    root: &std::path::Path,
    court: Option<kingdom_core::mockdata::CourtFn>,
) -> Result<Kingdom, ServerFnError> {
    use crate::scan::scan_kingdom;

    let cities = scan_kingdom(root)
        .map_err(|e| ServerFnError::new(format!("Could not read {}: {e}", root.display())))?;

    // Cities are rescanned every time -- disk is their source of truth. Plans
    // are not: they are the one thing here that disk cannot tell us again.
    let recorded = crate::store::load(root);
    let seating_court = recorded.is_empty();
    let court = court.unwrap_or(kingdom_core::sample::populate_court);
    let plans = open_court(recorded, &cities, court);

    // A fabricated court is fabricated exactly once per kingdom. Written
    // immediately so the next open reads it back as ordinary history rather
    // than seating a second one over the top of the first.
    if seating_court && !plans.is_empty() {
        if let Err(e) = crate::store::save_all(root, &plans) {
            leptos::logging::warn!("could not record the opening court: {e}");
        }
    }

    // Ids resume above whatever is already recorded, so a restart cannot reissue
    // an id that a plan on disk is already using.
    state::PLAN_SEQ.store(
        crate::store::next_number(&plans),
        std::sync::atomic::Ordering::Relaxed,
    );

    let kingdom = Kingdom {
        name: root
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("Kingdom")
            .to_string(),
        root: root.to_string_lossy().to_string(),
        cities,
        plans,
        sandbox: crate::mock::is_proving_ground(root),
    };

    *lock()? = kingdom.clone();

    Ok(kingdom)
}

/// The plans a freshly opened kingdom starts with.
///
/// A court is seated **only over an empty store**. Fabricating one every time
/// would duplicate the whole opening court on the second open -- the King's
/// real replies to a sample plan sitting beside a pristine copy of the same
/// plan. Split out from [`assemble`] so the rule is testable without the
/// process-global kingdom.
#[cfg(feature = "ssr")]
fn open_court(
    recorded: Vec<Plan>,
    cities: &[kingdom_core::City],
    court: kingdom_core::mockdata::CourtFn,
) -> Vec<Plan> {
    if recorded.is_empty() {
        court(cities)
    } else {
        recorded
    }
}

/// Refuses any folder outside the sandbox when `KINGDOM_SANDBOX` is set.
///
/// This is the setting for a session where Kingdom IDE is working on Kingdom
/// IDE. It turns "I meant to open the fake one" from something the King must
/// remember into something the server enforces -- and when plans get hands, it
/// is the wall that keeps an agent's first destructive command inside the
/// proving grounds.
///
/// Both sides are `canonicalize`d before comparing. Comparing the strings as
/// typed would let `sandbox/../../home/you/dev` through, which is precisely the
/// case that matters.
#[cfg(feature = "ssr")]
fn enforce_sandbox(root: &std::path::Path) -> Result<(), ServerFnError> {
    if !sandbox_enforced() {
        return Ok(());
    }
    within_sandbox(&crate::mock::sandbox_root(), root).map_err(ServerFnError::new)
}

/// Whether `requested` lies inside `sandbox`, both fully resolved.
///
/// Split out from [`enforce_sandbox`] so the containment rule can be tested
/// directly: the environment variable it keys off is process-global, and a test
/// that mutated it would race every other test in the binary.
///
/// Both sides are `canonicalize`d before comparing, which is the whole substance
/// of the check. Comparing the strings as typed would admit
/// `sandbox/../../home/you/dev` -- a path that *starts with* the sandbox root
/// and resolves nowhere near it.
#[cfg(feature = "ssr")]
fn within_sandbox(sandbox: &std::path::Path, requested: &std::path::Path) -> Result<(), String> {
    // A sandbox root that cannot be resolved contains nothing, so nothing may be
    // opened. Failing *open* here would quietly disable the whole protection at
    // exactly the moment it is misconfigured.
    let allowed = sandbox.canonicalize().map_err(|e| {
        format!(
            "KINGDOM_SANDBOX is set but the sandbox root {} is unusable: {e}. \
             Seed a realm first.",
            sandbox.display()
        )
    })?;

    let requested = requested
        .canonicalize()
        .map_err(|e| format!("Could not resolve {}: {e}", requested.display()))?;

    if !requested.starts_with(&allowed) {
        return Err(format!(
            "KINGDOM_SANDBOX is set: only folders inside {} may be opened. {} is outside it.",
            allowed.display(),
            requested.display()
        ));
    }

    Ok(())
}

#[cfg(feature = "ssr")]
fn sandbox_enforced() -> bool {
    matches!(
        std::env::var("KINGDOM_SANDBOX")
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase()
            .as_str(),
        "1" | "true" | "yes" | "on"
    )
}

/// Opens a plan and returns immediately, before any drafting happens.
///
/// The split between opening and drafting is what lets the King land in the
/// plan's conversation the moment he presses Start: the plan has an id, a
/// title and his own words in its transcript before the model has been called
/// at all. [`draft_plan`] then does the slow half.
///
/// Makes **no model call**. It does, however, prepare the workspace. A workspace
/// that cannot be cut must fail loudly *before* a plan exists, rather than
/// leaving a plan pointing nowhere.
#[server(BeginPlan, "/api")]
pub async fn begin_plan(
    prompt: String,
    city: Option<String>,
    choice: Option<ModelChoice>,
    workspace: Option<WorkspaceMode>,
) -> Result<Plan, ServerFnError> {
    use kingdom_core::{CityId, NoteKind};

    let prompt = prompt.trim().to_string();
    if prompt.is_empty() {
        return Err(ServerFnError::new("A decree cannot be empty."));
    }

    let city_id = CityId::new(
        city.ok_or_else(|| ServerFnError::new("Choose a city before issuing a decree."))?,
    );
    let mode = workspace.unwrap_or_default();

    // Resolved here rather than at draft time so the plan records what it will
    // actually be drawn by from the instant it appears in the rail. A model the
    // King's browser remembers but Copilot no longer serves degrades to the
    // default rather than failing the decree outright.
    let choice = crate::llm::catalogue::catalogue()
        .await
        .resolve(choice.as_ref());

    let plan_id = PlanId::new(format!("plan-{}", next_plan_number()));

    let city_root = {
        let kingdom = lock()?;
        let Some(city) = kingdom.city(&city_id) else {
            return Err(ServerFnError::new("No such city in this kingdom."));
        };
        std::path::Path::new(&kingdom.root).join(&city.path)
    };

    // The branch is named after the plan, so the name has to be settled before
    // the workspace is cut. `slug_for_decree` is the same derivation
    // `Plan::opened` uses below, so the plan's `slug` and its branch agree.
    let workspace =
        crate::worktree::prepare(&city_root, &mode, &kingdom_core::slug_for_decree(&prompt))
            .await
            .map_err(|e| ServerFnError::new(e.to_string()))?;

    let mut plan = Plan::opened(plan_id, city_id, &prompt, &choice, workspace.clone());
    // Where an agent is fenced in is not something it said, it is something that
    // happened -- and isolation the King cannot see is isolation he cannot
    // trust, so it is recorded in the log rather than only in the header.
    plan.note(
        NoteKind::Workspace,
        match &workspace.branch {
            Some(branch) => format!("Working in {} on {branch}.", workspace.path),
            None => format!("Working directly in {}, with no isolation.", workspace.path),
        },
    );

    let mut kingdom = lock()?;
    let root = std::path::PathBuf::from(&kingdom.root);
    remember(&root, &mut plan);
    kingdom.plans.push(plan.clone());

    Ok(plan)
}

/// Local branches in a city's repository, for the workspace picker.
///
/// Offered as a list so the King picks a branch that exists rather than typing
/// one that does not. A city with no git yields an empty list, which the picker
/// reads as "nothing to offer here".
#[server(ListBranches, "/api")]
pub async fn list_branches(city: String) -> Result<Vec<String>, ServerFnError> {
    use kingdom_core::CityId;

    let city_id = CityId::new(city);
    let root = {
        let kingdom = lock()?;
        let Some(city) = kingdom.city(&city_id) else {
            return Ok(Vec::new());
        };
        std::path::Path::new(&kingdom.root).join(&city.path)
    };

    Ok(crate::worktree::branches(&root).await)
}

/// Records another decree on an existing plan, without drafting a reply.
///
/// Paired with [`draft_plan`] for the same reason as [`begin_plan`]: the King's
/// own words appear in the conversation the instant he sends them, rather than
/// only once the court has finished thinking.
#[server(Say, "/api")]
pub async fn say(plan: String, prompt: String) -> Result<Plan, ServerFnError> {
    use kingdom_core::{PlanStatus, Speaker};

    let prompt = prompt.trim().to_string();
    if prompt.is_empty() {
        return Err(ServerFnError::new("A decree cannot be empty."));
    }

    let plan_id = PlanId::new(plan);
    let mut kingdom = lock()?;

    // A settled plan is history, and its workspace is gone from disk. A stale
    // tab must not be able to reopen a conversation whose checkout no longer
    // exists -- the reply would be drafted against nothing.
    if let Some(existing) = kingdom.plan(&plan_id) {
        if existing.status.is_settled() {
            return Err(ServerFnError::new(format!(
                "That plan is {} and its workspace has been cleared. \
                 Start a new decree to carry the work on.",
                existing.status.label().to_lowercase()
            )));
        }
    }

    update(&mut kingdom, &plan_id, |p| {
        p.status = PlanStatus::Drafting;
        p.say(Speaker::King, prompt);
    })
    .ok_or_else(|| ServerFnError::new("That plan is no longer in the records."))
}

/// Draws up the plan: marks it busy, calls the model, settles.
///
/// The whole turn happens inside the request because there is no push channel
/// yet -- the browser has nowhere to receive progress. When the WebSocket layer
/// lands this becomes spawn-and-notify.
#[server(DraftPlan, "/api")]
pub async fn draft_plan(plan: String) -> Result<Plan, ServerFnError> {
    use crate::llm::{Brief, CityBrief};
    use kingdom_core::{PlanStatus, Speaker};

    let plan_id = PlanId::new(plan);

    let (brief, transcript, prompt, choice) = {
        let mut kingdom = lock()?;

        let Some(existing) = kingdom.plan(&plan_id).cloned() else {
            return Err(ServerFnError::new("That plan is no longer in the records."));
        };

        // A plan already busy means a draft is in flight: the conversation view
        // kicks drafting off on mount, so a reload or a second tab would
        // otherwise start a second one -- a duplicate model call, and a
        // duplicate bill.
        if existing.is_busy() {
            return Ok(existing);
        }

        // Likewise a settled plan: the chamber mounts the same way for history
        // as for live work, and drafting against a workspace that has been
        // cleared from disk would brief the model on nothing.
        if existing.status.is_settled() {
            return Ok(existing);
        }

        let Some(city) = kingdom.city(&existing.city).cloned() else {
            return Err(ServerFnError::new("That plan's city is gone."));
        };
        // Briefed on the workspace it actually holds, not the project it was cut
        // from: an isolated plan naming files in somebody else's checkout would
        // be worse than useless.
        let brief = CityBrief::from_city(&city, &existing.workspace);

        // The message being answered is the last thing the King *said*;
        // everything said before it is the context leading up to it. Kingdom's
        // own notices are not in this sequence at all -- `said()` is the only
        // door between a plan's log and a model, and it does not open for them.
        let said: Vec<_> = existing.said().cloned().collect();
        let (transcript, prompt) = match said.iter().rposition(|u| u.speaker == Speaker::King) {
            Some(i) => (said[..i].to_vec(), said[i].body.clone()),
            None => (Vec::new(), existing.prompt.clone()),
        };

        update(&mut kingdom, &plan_id, |p| {
            p.status = PlanStatus::Drafting;
            p.working_on = Some(format!("Reading {} to draft a plan", city.name));
        });

        // Drafting keeps whatever the plan is already being drawn by: switching
        // model silently mid-conversation would make the transcript a record of
        // nothing in particular. The choice was settled when the plan opened.
        (brief, transcript, prompt, existing.choice())
    };

    // Spawned rather than awaited inline, because the plan is already marked
    // busy. If the browser navigates away mid-draft, Axum drops this request's
    // future -- which would cancel the model call *after* the mark and *before*
    // it is cleared, leaving a plan permanently Drafting that no later decree
    // could restart. A detached task loses only the reply, never the clearing.
    let handle = tokio::spawn(async move {
        let outcome = match crate::llm::open(&choice).await {
            Ok(model) => {
                model
                    .draft(&Brief {
                        city: brief,
                        transcript,
                        prompt,
                    })
                    .await
            }
            // A missing credential surfaces as a failed plan the King can see
            // and retry, rather than an error attached to nothing.
            Err(e) => Err(e),
        };

        settle(plan_id, outcome)
    });

    handle
        .await
        .map_err(|e| ServerFnError::new(format!("drafting task failed: {e}")))?
}

/// Records a drafting outcome on the plan and marks it no longer busy.
///
/// The clearing must happen on the failure path too: a plan left marked busy
/// could never be retried, because `draft_plan` would keep short-circuiting.
#[cfg(feature = "ssr")]
fn settle(
    plan_id: PlanId,
    outcome: Result<crate::llm::Draft, crate::llm::ModelError>,
) -> Result<Plan, ServerFnError> {
    use kingdom_core::{NoteKind, PlanStatus, Speaker};

    let mut kingdom = lock()?;

    let updated = update(&mut kingdom, &plan_id, |plan| {
        plan.working_on = None;
        match &outcome {
            Ok(draft) => {
                plan.title = draft.title.clone();
                plan.summary = draft.summary.clone();
                plan.status = PlanStatus::AwaitingReview;
                plan.say(Speaker::Court, draft.body.clone());
            }
            Err(e) => {
                // A failure is Kingdom reporting, not the model speaking -- so
                // the next turn must not replay it as prior counsel.
                let message = e.to_string();
                plan.status = PlanStatus::Failed;
                plan.summary = message.clone();
                plan.note(NoteKind::Failed, message);
            }
        }
    });

    updated.ok_or_else(|| ServerFnError::new("That plan vanished mid-decree."))
}

/// Closes a plan: lands its work, or sets it aside.
///
/// The two endings share this one function because they share their whole
/// shape -- check the plan can be closed, do the git work, settle or report --
/// and differ only in which disposal runs. Splitting them would duplicate the
/// guards, and a guard that exists in one copy is a guard that will be missed
/// in the other.
///
/// A refusal from git comes back as `Ok`, with the reason recorded in the
/// plan's log. `Err` means the server could not do the work at all.
#[server(FinishPlan, "/api")]
pub async fn finish_plan(plan: String, how: Disposition) -> Result<Plan, ServerFnError> {
    use crate::worktree::Finish;

    let plan_id = PlanId::new(plan);

    let (workspace, city_root, root) = {
        let kingdom = lock()?;
        let Some(existing) = kingdom.plan(&plan_id) else {
            return Err(ServerFnError::new("That plan is no longer in the records."));
        };

        // A draft in flight is mid-write: merging under it would land half a
        // thought, and removing its worktree would pull the floor out.
        if existing.is_busy() {
            return Err(ServerFnError::new(
                "This plan is still being drafted. Wait for it to finish.",
            ));
        }

        // Already settled: its worktree is gone, so there is nothing left to do
        // and repeating the disposal would only produce confusing git errors.
        if existing.status.is_settled() {
            return Ok(existing.clone());
        }

        let Some(city) = kingdom.city(&existing.city) else {
            return Err(ServerFnError::new("That plan's city is gone."));
        };

        (
            existing.workspace.clone(),
            std::path::Path::new(&kingdom.root).join(&city.path),
            std::path::PathBuf::from(&kingdom.root),
        )
    };

    let finish = match how {
        Disposition::Merge => crate::worktree::merge(&city_root, &workspace).await,
        Disposition::Archive => {
            let patch = crate::store::archive_patch(&root, &plan_id);
            crate::worktree::archive(&city_root, &workspace, &patch).await
        }
    }
    .map_err(|e| ServerFnError::new(e.to_string()))?;

    let mut kingdom = lock()?;
    update(&mut kingdom, &plan_id, |p| match finish {
        Finish::Settled(outcome) => {
            p.note(NoteKind::Merge, outcome.summary());
            p.settle(outcome);
        }
        // Nothing about the plan has changed, so nothing about its status
        // does either: it is still awaiting review, because it is.
        Finish::Refused(why) => p.note(NoteKind::Merge, why),
    })
    .ok_or_else(|| ServerFnError::new("That plan vanished mid-decree."))
}

/// Every model the King can choose between, and what each will accept.
///
/// Read live from each provider rather than hard-coded, so the picker cannot
/// offer a model that has been withdrawn or hide one that has just landed. It
/// also carries the credential state, which is why there is no separate status
/// call: "what can draft this?" and "will it work?" are one question.
#[server(ListModels, "/api")]
pub async fn list_models() -> Result<ModelCatalogue, ServerFnError> {
    Ok(crate::llm::catalogue::catalogue().await)
}

/// A suggested starting folder, so the King is not typing a path from scratch.
#[server(SuggestRoot, "/api")]
pub async fn suggest_root() -> Result<String, ServerFnError> {
    let home = std::env::var("HOME").unwrap_or_default();
    for candidate in ["dev", "Development", "projects", "code", "src", "repos"] {
        let p = std::path::Path::new(&home).join(candidate);
        if p.is_dir() {
            return Ok(p.to_string_lossy().to_string());
        }
    }
    Ok(home)
}

/// Expands a leading `~` to the user's home directory.
#[cfg(feature = "ssr")]
fn expand_home(path: &str) -> String {
    let trimmed = path.trim();
    if let Some(rest) = trimmed.strip_prefix("~") {
        if let Ok(home) = std::env::var("HOME") {
            return format!("{home}{rest}");
        }
    }
    trimmed.to_string()
}

#[cfg(all(test, feature = "ssr"))]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// Containment must survive traversal, not merely prefix-matching.
    ///
    /// This is the wall that keeps a session working on Kingdom IDE from
    /// opening the King's real projects -- and, once plans have hands, the wall
    /// that keeps an agent's first destructive command inside the proving
    /// grounds. The `..` case is the one that matters: a check written as a
    /// `starts_with` on the strings as typed would happily admit
    /// `<sandbox>/../../dev`, which is both a prefix match and completely
    /// outside the sandbox.
    #[test]
    fn the_sandbox_cannot_be_walked_out_of() {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let base = std::env::temp_dir().join(format!("kingdom-sandbox-{unique}"));
        let sandbox = base.join("realms");
        let realm = sandbox.join("kingdom-mirror");
        let outside = base.join("real-work");
        std::fs::create_dir_all(&realm).unwrap();
        std::fs::create_dir_all(&outside).unwrap();

        assert!(
            within_sandbox(&sandbox, &realm).is_ok(),
            "a realm inside the sandbox must be openable"
        );

        assert!(
            within_sandbox(&sandbox, &outside).is_err(),
            "a folder outside the sandbox must be refused"
        );

        // Lexically inside, actually outside.
        let traversal: PathBuf = sandbox.join("..").join("real-work");
        assert!(
            within_sandbox(&sandbox, &traversal).is_err(),
            "a path that walks out with .. must be refused, not prefix-matched"
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    /// A kingdom is opened many times; its court is fabricated once.
    ///
    /// Without this the second open would seat a fresh court *over* the stored
    /// one -- the King's real replies to a sample plan sitting beside a pristine
    /// copy of the same plan, multiplying on every restart. The opening court
    /// exists to give a new kingdom something to show, and a kingdom with
    /// records is not new.
    #[test]
    fn a_court_is_seated_only_over_an_empty_store() {
        use kingdom_core::{CityId, ModelChoice, PlanId, Workspace};

        fn court(_: &[kingdom_core::City]) -> Vec<Plan> {
            vec![Plan::opened(
                PlanId::new("plan-fabricated"),
                CityId::new("c1"),
                "A fabricated decree",
                &ModelChoice::new("mock", None),
                Workspace::in_place("/dev/testburg"),
            )]
        }

        let seated = open_court(Vec::new(), &[], court);
        assert_eq!(
            seated.len(),
            1,
            "a kingdom with no records gets an opening court"
        );

        let recorded = vec![Plan::opened(
            PlanId::new("plan-1"),
            CityId::new("c1"),
            "The King's own decree",
            &ModelChoice::new("mock", None),
            Workspace::in_place("/dev/testburg"),
        )];
        assert_eq!(
            open_court(recorded.clone(), &[], court),
            recorded,
            "a kingdom with records keeps them, and gets no second court"
        );
    }
}
