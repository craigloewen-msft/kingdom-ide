//! Server functions: the typed bridge between browser and server.
//!
//! Leptos `#[server]` functions compile to a real HTTP call on the client and
//! a direct invocation on the server, sharing one signature. That is the main
//! reason this project is Rust on both ends — there is no hand-written client,
//! no schema to keep in sync, and a domain type change breaks the build rather
//! than failing at runtime.

#[cfg(feature = "ssr")]
use kingdom_core::PlanId;
use kingdom_core::{Kingdom, ModelStatus, Plan};
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

/// Issues a decree: the King opens a new plan in a city, and a model drafts it.
///
/// The whole turn happens here rather than in a background task, because there
/// is no push channel yet -- the browser has nowhere to receive progress. When
/// the WebSocket layer lands this becomes spawn-and-notify; until then, awaiting
/// the reply is honest about what the King is actually waiting for.
#[server(OpenPlan, "/api")]
pub async fn open_plan(prompt: String, city: Option<String>) -> Result<Plan, ServerFnError> {
    use crate::llm::{broker, Brief, CityBrief};
    use kingdom_core::{CityId, PlanStatus, Speaker};

    let prompt = prompt.trim().to_string();
    if prompt.is_empty() {
        return Err(ServerFnError::new("A decree cannot be empty."));
    }

    let city_id = CityId::new(
        city.ok_or_else(|| ServerFnError::new("Choose a city before issuing a decree."))?,
    );

    // Build the model first: with no credential there is nothing to draft with,
    // and taking a lease we cannot use would leave the city looking busy.
    let model = crate::llm::configured()
        .await
        .map_err(|e| ServerFnError::new(e.to_string()))?;

    let plan_id = PlanId::new(format!("plan-{}", next_plan_number()));
    let mut plan = Plan::opened(plan_id.clone(), city_id.clone(), &prompt, model.name());

    // Register the plan and claim the city's files in one critical section, so
    // two decrees racing for the same city cannot both see it free.
    let brief = {
        let mut kingdom = lock()?;

        let Some(city) = kingdom.city(&city_id).cloned() else {
            return Err(ServerFnError::new("No such city in this kingdom."));
        };
        let brief = CityBrief::from_city(&city, &kingdom.root);

        match broker::acquire_city_read(&mut kingdom, &plan_id, &city_id) {
            Ok(lease) => plan.leases.push(lease),
            Err(refusal) => {
                // Refused work is not silently dropped: it is parked where the
                // King can see it, on the map and in the rail.
                plan.status = PlanStatus::Blocked;
                plan.summary = refusal.reason.clone();
                plan.say(Speaker::Court, refusal.reason);
                kingdom.plans.push(plan.clone());
                return Ok(plan);
            }
        }

        kingdom.plans.push(plan.clone());
        brief
    };

    let outcome = model
        .draft(&Brief {
            city: brief,
            transcript: Vec::new(),
            prompt,
        })
        .await;

    settle(plan_id, outcome)
}

/// Another turn on an existing plan, so the dock is a conversation rather than
/// a series of unrelated one-shots.
#[server(ContinuePlan, "/api")]
pub async fn continue_plan(plan: String, prompt: String) -> Result<Plan, ServerFnError> {
    use crate::llm::{broker, Brief, CityBrief};
    use kingdom_core::{PlanStatus, Speaker};

    let prompt = prompt.trim().to_string();
    if prompt.is_empty() {
        return Err(ServerFnError::new("A decree cannot be empty."));
    }
    let plan_id = PlanId::new(plan);

    let model = crate::llm::configured()
        .await
        .map_err(|e| ServerFnError::new(e.to_string()))?;

    let (brief, transcript) = {
        let mut kingdom = lock()?;

        let Some(existing) = kingdom.plan(&plan_id).cloned() else {
            return Err(ServerFnError::new("That plan is no longer in the records."));
        };
        let Some(city) = kingdom.city(&existing.city).cloned() else {
            return Err(ServerFnError::new("That plan's city is gone."));
        };
        let brief = CityBrief::from_city(&city, &kingdom.root);

        let lease = match broker::acquire_city_read(&mut kingdom, &plan_id, &existing.city) {
            Ok(lease) => lease,
            Err(refusal) => {
                let plan = update(&mut kingdom, &plan_id, |p| {
                    p.status = PlanStatus::Blocked;
                    p.summary = refusal.reason.clone();
                    p.say(Speaker::Court, refusal.reason.clone());
                });
                return plan.ok_or_else(|| ServerFnError::new("That plan vanished mid-decree."));
            }
        };

        update(&mut kingdom, &plan_id, |p| {
            p.status = PlanStatus::Drafting;
            p.model = model.name().to_string();
            p.leases = vec![lease.clone()];
            p.say(Speaker::King, prompt.clone());
        });

        (brief, existing.transcript)
    };

    let outcome = model
        .draft(&Brief {
            city: brief,
            transcript,
            prompt,
        })
        .await;

    settle(plan_id, outcome)
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
    use kingdom_core::{PlanStatus, Speaker};

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
            let message = e.to_string();
            plan.status = PlanStatus::Failed;
            plan.summary = message.clone();
            plan.say(Speaker::Court, message);
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
