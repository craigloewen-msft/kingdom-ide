//! Server functions: the typed bridge between browser and server.
//!
//! Leptos `#[server]` functions compile to a real HTTP call on the client and
//! a direct invocation on the server, sharing one signature. That is the main
//! reason this project is Rust on both ends — there is no hand-written client,
//! no schema to keep in sync, and a domain type change breaks the build rather
//! than failing at runtime.

#[cfg(feature = "ssr")]
use kingdom_core::PlanId;
use kingdom_core::{Kingdom, ModelCatalogue, ModelChoice, ModelStatus, Plan, WorkspaceMode};
use leptos::prelude::*;

/// In-memory kingdom state.
///
/// A process-global `Mutex` is the right amount of machinery for a
/// single-user local tool at this stage. It sits behind these server
/// functions, so swapping in SQLite later touches only this module.
#[cfg(feature = "ssr")]
mod store {
    use kingdom_core::Kingdom;
    use std::sync::atomic::AtomicU64;
    use std::sync::{Mutex, OnceLock};

    static KINGDOM: OnceLock<Mutex<Kingdom>> = OnceLock::new();

    pub fn get() -> &'static Mutex<Kingdom> {
        KINGDOM.get_or_init(|| Mutex::new(Kingdom::unopened()))
    }

    /// Monotonic counter behind plan ids. Restarting empties the kingdom too,
    /// so it does not need to survive the process.
    pub static PLAN_SEQ: AtomicU64 = AtomicU64::new(1);
}

/// Locks the kingdom, turning a poisoned mutex into a server error.
#[cfg(feature = "ssr")]
fn lock() -> Result<std::sync::MutexGuard<'static, Kingdom>, ServerFnError> {
    store::get()
        .lock()
        .map_err(|e| ServerFnError::new(format!("kingdom state poisoned: {e}")))
}

#[cfg(feature = "ssr")]
fn next_plan_number() -> u64 {
    store::PLAN_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

/// Applies a change to one plan and returns the result, so callers hand the
/// browser the same value that was just stored.
#[cfg(feature = "ssr")]
fn update(kingdom: &mut Kingdom, id: &PlanId, change: impl FnOnce(&mut Plan)) -> Option<Plan> {
    let plan = kingdom.plans.iter_mut().find(|p| &p.id == id)?;
    change(plan);
    Some(plan.clone())
}

/// Returns the currently open kingdom, or an empty one if none is open.
#[server(GetKingdom, "/api")]
pub async fn get_kingdom() -> Result<Kingdom, ServerFnError> {
    Ok(lock()?.clone())
}

/// Opens a dev folder as the kingdom: scans it for cities and seats a court.
#[server(OpenKingdom, "/api")]
pub async fn open_kingdom(path: String) -> Result<Kingdom, ServerFnError> {
    use crate::scan::scan_kingdom;
    use std::path::PathBuf;

    let expanded = expand_home(&path);
    let root = PathBuf::from(&expanded);

    if !root.is_dir() {
        return Err(ServerFnError::new(format!(
            "No such folder: {expanded}. Give an absolute path to your dev folder."
        )));
    }

    let cities = scan_kingdom(&root)
        .map_err(|e| ServerFnError::new(format!("Could not read {expanded}: {e}")))?;

    // Cities are real; the starting court is still fabricated.
    // See `kingdom_core::sample`.
    let (plans, resources) = kingdom_core::sample::populate_court(&cities);

    let kingdom = Kingdom {
        name: root
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("Kingdom")
            .to_string(),
        root: root.to_string_lossy().to_string(),
        cities,
        plans,
        resources,
    };

    *lock()? = kingdom.clone();

    Ok(kingdom)
}

/// Opens a plan and returns immediately, before any drafting happens.
///
/// The split between opening and drafting is what lets the King land in the
/// plan's conversation the moment he presses Start: the plan has an id, a
/// title and his own words in its transcript before the model has been called
/// at all. [`draft_plan`] then does the slow half.
///
/// Makes **no model call**. It does, however, prepare the workspace, and for an
/// isolated mode that means cutting a git worktree -- which touches something
/// shared, so it takes the city's repo lock across the one git command and hands
/// it straight back. A deliberate exception to "opening claims nothing": a
/// workspace that cannot be cut must fail loudly *before* a plan exists, rather
/// than leaving a plan pointing nowhere.
#[server(BeginPlan, "/api")]
pub async fn begin_plan(
    prompt: String,
    city: Option<String>,
    choice: Option<ModelChoice>,
    workspace: Option<WorkspaceMode>,
) -> Result<Plan, ServerFnError> {
    use crate::llm::broker;
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

    // Held across the git command only. Taken under the id the plan is about to
    // have, so a King watching Crown Resources sees the same name that lands in
    // the rail a moment later.
    let workspace = if mode.needs_git() {
        let lease = {
            let mut kingdom = lock()?;
            broker::acquire_repo_lock(&mut kingdom, &plan_id, &city_id)
                .map_err(|r| ServerFnError::new(r.reason))?
        };

        let prepared = crate::worktree::prepare(&city_root, &mode).await;

        // Released on the failure path too: a repo left locked by a plan that
        // never existed would block every later decree for that city.
        {
            let mut kingdom = lock()?;
            broker::release(&mut kingdom, &lease);
        }

        prepared.map_err(|e| ServerFnError::new(e.to_string()))?
    } else {
        crate::worktree::prepare(&city_root, &mode)
            .await
            .map_err(|e| ServerFnError::new(e.to_string()))?
    };

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

    lock()?.plans.push(plan.clone());

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

    update(&mut kingdom, &plan_id, |p| {
        p.status = PlanStatus::Drafting;
        p.say(Speaker::King, prompt);
    })
    .ok_or_else(|| ServerFnError::new("That plan is no longer in the records."))
}

/// Draws up the plan: claims the city's files, calls the model, settles.
///
/// This is the half that costs something, so it is the half that takes a lease.
/// The whole turn still happens inside the request because there is no push
/// channel yet -- the browser has nowhere to receive progress. When the
/// WebSocket layer lands this becomes spawn-and-notify.
#[server(DraftPlan, "/api")]
pub async fn draft_plan(plan: String) -> Result<Plan, ServerFnError> {
    use crate::llm::{broker, Brief, CityBrief};
    use kingdom_core::{NoteKind, PlanStatus, Speaker};

    let plan_id = PlanId::new(plan);

    let (brief, transcript, prompt, choice) = {
        let mut kingdom = lock()?;

        let Some(existing) = kingdom.plan(&plan_id).cloned() else {
            return Err(ServerFnError::new("That plan is no longer in the records."));
        };

        // A lease already held by this plan means a draft is in flight: the
        // conversation view kicks drafting off on mount, so a reload or a second
        // tab would otherwise start a second one. The lease is already the
        // answer to "is someone working on this right now?", so it is used here
        // rather than inventing a flag that could disagree with it.
        if !existing.leases.is_empty() {
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

        match broker::acquire_workspace_read(
            &mut kingdom,
            &plan_id,
            &existing.city,
            &existing.workspace,
        ) {
            Ok(lease) => {
                update(&mut kingdom, &plan_id, |p| {
                    p.status = PlanStatus::Drafting;
                    p.leases = vec![lease];
                });
            }
            Err(refusal) => {
                // Refused work is not silently dropped: it is parked where the
                // King can see it, on the map and in the rail.
                let plan = update(&mut kingdom, &plan_id, |p| {
                    p.status = PlanStatus::Blocked;
                    p.summary = refusal.reason.clone();
                    p.note(NoteKind::Blocked, refusal.reason.clone());
                });
                return plan.ok_or_else(|| ServerFnError::new("That plan vanished mid-decree."));
            }
        }

        // Drafting keeps whatever the plan is already being drawn by: switching
        // model silently mid-conversation would make the transcript a record of
        // nothing in particular. The choice was settled when the plan opened.
        (brief, transcript, prompt, existing.choice())
    };

    // Spawned rather than awaited inline, because the lease is already held.
    // If the browser navigates away mid-draft, Axum drops this request's
    // future -- which would cancel the model call *after* the claim and
    // *before* the release, leaving the city held by a plan that is
    // permanently Drafting and blocking every later decree with no way to
    // clear it. A detached task loses only the reply, never the release.
    let handle = tokio::spawn(async move {
        let outcome = match crate::llm::configured(&choice).await {
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

/// Records a drafting outcome on the plan and hands back every lease it held.
///
/// Shared by both entry points so the release path cannot drift between them --
/// a plan left holding a city's files would block every later decree for it.
#[cfg(feature = "ssr")]
fn settle(
    plan_id: PlanId,
    outcome: Result<crate::llm::Draft, crate::llm::ModelError>,
) -> Result<Plan, ServerFnError> {
    use crate::llm::broker;
    use kingdom_core::{NoteKind, PlanStatus, Speaker};

    let mut kingdom = lock()?;
    broker::release_all(&mut kingdom, &plan_id);

    let updated = update(&mut kingdom, &plan_id, |plan| match &outcome {
        Ok(draft) => {
            plan.title = draft.title.clone();
            plan.summary = draft.summary.clone();
            plan.touches = draft.touches.clone();
            plan.status = PlanStatus::AwaitingReview;
            plan.say(Speaker::Court, draft.body.clone());
        }
        Err(e) => {
            // A failure is Kingdom reporting, not the model speaking -- so it is
            // a note, and the next turn will not replay it as prior counsel.
            let message = e.to_string();
            plan.status = PlanStatus::Failed;
            plan.summary = message.clone();
            plan.note(NoteKind::Failed, message);
        }
    });

    updated.ok_or_else(|| ServerFnError::new("That plan vanished mid-decree."))
}

/// How plans will be drafted: provider, model, and whether a credential works.
///
/// Resolves the credential rather than merely checking it is configured, since
/// "set" and "works" are different questions and only the second one matters.
#[server(GetModelStatus, "/api")]
pub async fn model_status() -> Result<ModelStatus, ServerFnError> {
    Ok(crate::llm::status().await)
}

/// Every model the King can choose between, and what each will accept.
///
/// Read live from the provider rather than hard-coded, so the picker cannot
/// offer a model that has been withdrawn or hide one that has just landed.
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
