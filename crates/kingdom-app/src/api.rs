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

/// Applies a change to one plan, records it, proclaims it, and returns the
/// result, so callers hand the browser the same value that was just stored.
///
/// The single funnel for plan mutations, which is why both persistence and push
/// hang off it: a caller cannot change a plan and forget to write it, and
/// equally cannot change a plan and forget to tell the chamber watching it.
/// See [`crate::herald`] for why that second one had to live here rather than
/// at each call site.
#[cfg(feature = "ssr")]
fn update(kingdom: &mut Kingdom, id: &PlanId, change: impl FnOnce(&mut Plan)) -> Option<Plan> {
    let root = std::path::PathBuf::from(&kingdom.root);
    let plan = kingdom.plans.iter_mut().find(|p| &p.id == id)?;
    change(plan);
    remember(&root, plan);
    // After `remember`, not before: a failed write appends a note to the plan,
    // and the chamber should be told the thing that was actually stored rather
    // than an optimistic version of it.
    crate::herald::proclaim(plan);
    Some(plan.clone())
}

/// One plan as the server currently has it.
///
/// Exists for the watch socket's opening message, which needs a plan without
/// going through a `#[server]` function -- it is already inside the server.
/// Returns `None` rather than erroring for an unknown id: a chamber may connect
/// to a plan that has since been forgotten, and an empty stream is the honest
/// answer.
#[cfg(feature = "ssr")]
pub fn snapshot(id: &PlanId) -> Option<Plan> {
    lock().ok()?.plan(id).cloned()
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

        // An errand answers to the court that sent it, not to the King. Its
        // chamber renders no composer, so this is only reachable from a stale
        // tab or a hand-made request -- but the damage is real: the parent is
        // blocked on a tool call whose conversation would now have two hands on
        // it, and would be handed a report on a conversation that changed
        // underneath it.
        if existing.is_subagent() {
            return Err(ServerFnError::new(
                "This is an errand the court sent, not a plan you decreed. \
                 Say what you want in the plan that sent it.",
            ));
        }
    }

    update(&mut kingdom, &plan_id, |p| {
        p.status = PlanStatus::Drafting;
        p.say(Speaker::User, prompt);
    })
    .ok_or_else(|| ServerFnError::new("That plan is no longer in the records."))
}

/// How many times the court may act before it must speak.
///
/// A loop that never ends is the failure mode worth being loud about: an agent
/// retrying a broken command against a paid model spends real money producing
/// nothing, and it does so quietly. Reaching this is recorded as a note, so the
/// King sees the plan stopped rather than finding a plausible answer that was
/// actually a truncation.
#[cfg(feature = "ssr")]
const MOST_ROUNDS: usize = 24;

/// Draws up the plan: marks it busy, then takes turns with the model until it
/// has something to say.
///
/// A turn is no longer one call. The court may act -- read a file, run a
/// command -- and each act is recorded, run, and answered before the model is
/// asked again. The request stays open for the whole conversation, but nothing
/// waits on it: every step is proclaimed to the chamber as it happens, which is
/// what makes a five-minute turn watchable instead of a spinner.
#[server(DraftPlan, "/api")]
pub async fn draft_plan(plan: String) -> Result<Plan, ServerFnError> {
    use kingdom_core::PlanStatus;

    let plan_id = PlanId::new(plan);

    let (city_brief, workspace, city_name, choice) = {
        let mut kingdom = lock()?;

        let Some(existing) = kingdom.plan(&plan_id).cloned() else {
            return Err(ServerFnError::new("That plan is no longer in the records."));
        };

        // A plan already busy means a turn is in flight: the conversation view
        // kicks drafting off on mount, so a reload or a second tab would
        // otherwise start a second one -- a duplicate model call, and a
        // duplicate bill. More serious now than it was: a second loop would
        // also run the same commands twice.
        if existing.is_busy() {
            return Ok(existing);
        }

        // Likewise a settled plan: the chamber mounts the same way for history
        // as for live work, and drafting against a workspace that has been
        // cleared from disk would brief the model on nothing.
        if existing.status.is_settled() {
            return Ok(existing);
        }

        // An errand is driven by the call that sent it, which is already
        // running one of these loops over it. The chamber mounts identically
        // for an errand, so without this, a King who opens one while it works
        // would start a second loop over the same plan.
        if existing.is_subagent() {
            return Ok(existing);
        }

        let Some(city) = kingdom.city(&existing.city).cloned() else {
            return Err(ServerFnError::new("That plan's city is gone."));
        };
        // Briefed on the workspace it actually holds, not the project it was cut
        // from: an isolated plan naming files in somebody else's checkout would
        // be worse than useless. The same workspace bounds every tool it runs.
        let city_brief = crate::llm::CityBrief::from_city(&city, &existing.workspace);

        update(&mut kingdom, &plan_id, |p| {
            p.status = PlanStatus::Drafting;
            p.working_on = Some(format!("Reading {} to draft a plan", city.name));
        });

        // Drafting keeps whatever the plan is already being drawn by: switching
        // model silently mid-conversation would make the transcript a record of
        // nothing in particular. The choice was settled when the plan opened.
        (
            city_brief,
            existing.workspace.clone(),
            city.name,
            existing.choice(),
        )
    };

    // Spawned rather than awaited inline, because the plan is already marked
    // busy. If the browser navigates away mid-turn, Axum drops this request's
    // future -- which would cancel the conversation *after* the mark and
    // *before* it is cleared, leaving a plan permanently Drafting that no later
    // decree could restart. A detached task loses only this caller's view of
    // the result, never the clearing; and since every step is pushed to the
    // chamber, that view was never the only way to see it.
    let handle = tokio::spawn(async move {
        converse(
            plan_id,
            city_brief,
            workspace,
            city_name,
            choice,
            crate::tools::Remit::Full,
            MOST_ROUNDS,
        )
        .await
    });

    handle
        .await
        .map_err(|e| ServerFnError::new(format!("drafting task failed: {e}")))?
}

/// Takes turns with the model until it speaks, runs out of rope, or fails.
///
/// The loop lives here rather than behind the [`crate::llm::Model`] trait
/// because this is where the plan is. A provider running its own tools would do
/// so with nothing recording them: the chamber would show a long silence and
/// then an answer, and a crash mid-way would leave no trace of what had already
/// been done to the workspace.
///
/// Two callers: [`draft_plan`], for a plan the King decreed, and
/// [`spawn_subagents`], for each errand the court sends. They differ only in the
/// `remit` and `cap` handed in -- an errand reads and reports, and gets fewer
/// rounds to do it in. Sharing the loop is the point: an errand that drafted
/// through a second, simpler path would be a second place for the busy mark,
/// the deed recording and the push to drift.
#[cfg(feature = "ssr")]
pub(crate) async fn converse(
    plan_id: PlanId,
    city: crate::llm::CityBrief,
    workspace: kingdom_core::Workspace,
    city_name: String,
    choice: kingdom_core::ModelChoice,
    remit: crate::tools::Remit,
    cap: usize,
) -> Result<Plan, ServerFnError> {
    use crate::llm::{Brief, Reply, ToolSpec};
    use crate::tools::Workshop;
    use kingdom_core::{ToolCall, NoteKind};

    let model = match crate::llm::open(&choice).await {
        Ok(model) => model,
        // A missing credential surfaces as a failed plan the King can see and
        // retry, rather than an error attached to nothing.
        Err(e) => return settle(plan_id, Err(e)),
    };

    // A model that cannot call tools still drafts perfectly good prose, so it
    // gets a prose-only turn rather than an error; one that cannot see is not
    // offered the tool that hands back a picture; and a plan under a survey
    // remit is not offered the tools that would let it write. All three
    // narrowings live in `ToolSpec::for_model`, so the reasoning is in one
    // place and no caller has to remember any of them.
    let tools = ToolSpec::for_model(model.as_ref(), remit);
    let shop = Workshop::new(workspace)
        .for_plan(plan_id.clone())
        .under(remit);

    for round in 0..cap {
        // The conversation is rebuilt from the plan each pass rather than
        // accumulated in a local. The deeds recorded below are already in it,
        // and reading them back is what makes this loop's state the plan's
        // state -- so a reader of the transcript sees exactly what the model
        // saw.
        let turns = {
            let kingdom = lock()?;
            let Some(plan) = kingdom.plan(&plan_id) else {
                return Err(ServerFnError::new("That plan vanished mid-decree."));
            };
            plan.turns().collect::<Vec<_>>()
        };

        let brief = Brief {
            city: city.clone(),
            turns,
            tools: tools.clone(),
        };

        match model.take_turn(&brief).await {
            Ok(Reply::Spoke(draft)) => return settle(plan_id, Ok(draft)),
            Err(e) => return settle(plan_id, Err(e)),

            Ok(Reply::Acts(acts)) => {
                for act in acts {
                    // Recorded *before* it runs, and proclaimed by `update`, so
                    // the chamber shows the command while it is still going.
                    // This is the answer to "what is this agent doing right
                    // now", and it only works because the deed is written down
                    // first.
                    {
                        let mut kingdom = lock()?;
                        update(&mut kingdom, &plan_id, |p| {
                            p.working_on = Some(describe(&act.tool, &act.input));
                            p.begin_tool_call(ToolCall::started(
                                act.id.clone(),
                                act.tool.clone(),
                                act.input.clone(),
                            ));
                        });
                    }

                    // The lock is deliberately not held across this. A tool can
                    // take minutes -- and `ask_user_question` waits on a person
                    // -- so a held lock would freeze every other plan and every
                    // chamber in the kingdom behind it.
                    //
                    // Bound to this deed, so a tool that has to reach back out
                    // to the King has something the browser can name when it
                    // answers.
                    let outcome =
                        crate::tools::invoke(&act.tool, act.input, &shop.for_tool_call(&act.id)).await;

                    let mut kingdom = lock()?;
                    update(&mut kingdom, &plan_id, |p| {
                        if !p.settle_tool_call(&act.id, outcome.clone()) {
                            // Cannot happen -- the deed was recorded a moment
                            // ago under the same id -- but a silently dropped
                            // result would leave the model waiting forever for
                            // a call the transcript says it never made.
                            p.note(
                                NoteKind::Failed,
                                format!(
                                    "A result arrived for a call ({}) that is not in this \
                                     plan's log. This is a bug in Kingdom.",
                                    act.id
                                ),
                            );
                        }
                    });
                }

                // Back round to ask again, now with the results in the log.
                let _ = round;
            }
        }
    }

    // Out of rope. Recorded as a note rather than a reply: nothing was said,
    // and dressing a truncation as counsel would hand the King an answer that
    // is really an unfinished job.
    let mut kingdom = lock()?;
    let updated = update(&mut kingdom, &plan_id, |p| {
        p.working_on = None;
        p.status = kingdom_core::PlanStatus::Failed;
        p.summary = format!("Stopped after {cap} rounds without an answer.");
        p.note(
            NoteKind::Failed,
            format!(
                "The court acted {cap} times in {city_name} without reaching an \
                 answer, and was stopped. Its work so far is still in the workspace; \
                 say something to send it round again."
            ),
        );
    });

    updated.ok_or_else(|| ServerFnError::new("That plan vanished mid-decree."))
}

/// Sends a set of errands and waits for them all to report.
///
/// Called by the `spawn_agents` tool, which is itself running inside its
/// parent's [`converse`] loop -- so this is the turn loop calling itself, one
/// level down, with a narrower remit. That shape is why `converse` is
/// `pub(crate)` rather than private.
///
/// Each errand is a real plan: recorded, watched and pushed like any other,
/// which is what lets the King open one and read it while it works. They run
/// concurrently, which is only safe because [`crate::tools::spawn_agents::ERRAND_REMIT`]
/// forbids them from writing -- see that module.
///
/// Returns the reports as one block of text for the model, or `Err` with a
/// reason to report to it when no errand could be sent at all.
#[cfg(feature = "ssr")]
pub(crate) async fn spawn_subagents(
    parent_id: &PlanId,
    tool_call: &str,
    tasks: Vec<String>,
    patience: std::time::Duration,
) -> Result<String, String> {
    use crate::tools::spawn_agents::{ERRAND_REMIT, MOST_ERRAND_ROUNDS};

    // Everything the errands need is read once, under one lock: the parent they
    // are cut from, and the city they work in. Holding it across the model
    // calls below would freeze every other plan in the kingdom.
    let (subagents, city_brief, city_name) = {
        let mut kingdom = lock().map_err(|e| e.to_string())?;

        let Some(parent) = kingdom.plan(parent_id).cloned() else {
            return Err("The plan that sent these errands is no longer in the records.".to_string());
        };
        let Some(city) = kingdom.city(&parent.city).cloned() else {
            return Err("That plan's city is gone.".to_string());
        };

        // An errand is briefed on the *workspace*, exactly as its parent is:
        // it is sent to look at the work in progress, not at a pristine copy.
        let city_brief = crate::llm::CityBrief::from_city(&city, &parent.workspace);

        let mut subagents = Vec::new();
        for task in tasks {
            let id = PlanId::new(format!("plan-{}", next_plan_number()));
            let mut subagent = Plan::spawned(id.clone(), &parent, tool_call, task.clone());
            let root = std::path::PathBuf::from(&kingdom.root);
            remember(&root, &mut subagent);
            // Pushed as well as recorded, so the parent's chamber can draw the
            // errand the instant it exists rather than when it first speaks.
            crate::herald::proclaim(&subagent);
            kingdom.plans.push(subagent);
            subagents.push((id, task));
        }

        (subagents, city_brief, city.name)
    };

    // All at once. The remit is what makes this safe: they share one worktree
    // and none of them can write to it.
    let running: Vec<_> = subagents
        .iter()
        .map(|(id, _)| {
            let id = id.clone();
            let city_brief = city_brief.clone();
            let city_name = city_name.clone();
            // Read back rather than carried down, so an errand is drafted by
            // what its own record says -- the same rule `draft_plan` follows.
            let found = snapshot(&id).map(|p| (p.workspace.clone(), p.choice()));
            tokio::spawn(async move {
                let Some((workspace, choice)) = found else {
                    return Err(ServerFnError::new("That errand vanished before it began."));
                };
                converse(
                    id,
                    city_brief,
                    workspace,
                    city_name,
                    choice,
                    ERRAND_REMIT,
                    MOST_ERRAND_ROUNDS,
                )
                .await
            })
        })
        .collect();

    // One deadline across the whole call rather than one per errand: they run
    // concurrently, so the bound that matters is how long the *parent* waits.
    //
    // Collected one at a time against that shared deadline, which is what keeps
    // a partial answer possible -- an errand that has already reported is
    // collected even if a later one has to be given up on. Timing the whole
    // batch out as a unit would throw away work that was finished and paid for.
    let deadline = tokio::time::Instant::now() + patience;
    let mut outcomes: Vec<Option<Plan>> = Vec::with_capacity(running.len());
    for handle in running {
        let settled = tokio::time::timeout_at(deadline, handle)
            .await
            .ok()
            .and_then(|joined| joined.ok())
            .and_then(|result| result.ok());
        outcomes.push(settled);
    }

    Ok(report(&subagents, &outcomes))
}

/// Renders what the errands found, for the model that sent them.
///
/// Each block names the errand's plan id, which is what lets the parent refer
/// to one in its own reply -- and what lets the King find the same conversation
/// the parent read.
#[cfg(feature = "ssr")]
fn report(subagents: &[(PlanId, String)], outcomes: &[Option<Plan>]) -> String {
    use kingdom_core::Speaker;

    let mut out = String::new();
    for (i, (id, task)) in subagents.iter().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        out.push_str(&format!("--- errand {} ({id}) ---\nTask: {task}\n\n", i + 1));

        // Read from the record rather than from the returned plan: an errand
        // that timed out here may still have said something, and its record is
        // where that would be.
        let plan = outcomes
            .get(i)
            .and_then(|p| p.clone())
            .or_else(|| snapshot(id));

        // The last thing the *court* said is the errand's answer. Read with a
        // fold rather than `next_back` because `messages()` yields a plain forward
        // iterator; taking the last match is the whole intent.
        let messages = plan.as_ref().and_then(|p| {
            p.messages()
                .filter(|u| u.speaker == Speaker::Assistant)
                .last()
                .map(|u| u.body.clone())
        });
        match messages {
            Some(body) => out.push_str(&body),
            None => out.push_str(
                "This errand did not report back. It may have run out of rope or \
                 taken too long; carry on without it, or ask something narrower.",
            ),
        }
        out.push('\n');
    }
    out
}

/// Plain-language "what is happening right now", for the rail and the map./// Plain-language "what is happening right now", for the rail and the map.
///
/// The tool's name alone is close to useless to the King -- "bash" tells him
/// nothing that "an agent is working" did not. The argument is the information,
/// so it is what gets shown.
#[cfg(feature = "ssr")]
fn describe(tool: &str, input: &serde_json::Value) -> String {
    // Waiting on a person is not the same kind of busy as running a command,
    // and "who is blocked behind whom" is one of the three questions this
    // product exists to answer. It gets said in those words rather than being
    // rendered as another tool name.
    if tool == "ask_user_question" {
        return "Waiting on the King".to_string();
    }

    let subject = ["cmd", "path", "pattern", "url", "selector", "query"]
        .iter()
        .find_map(|k| input.get(*k).and_then(|v| v.as_str()))
        .unwrap_or_default()
        .trim();

    if subject.is_empty() {
        return format!("Running {tool}");
    }

    let short: String = subject.chars().take(60).collect();
    format!("{tool}: {short}")
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
                plan.say(Speaker::Assistant, draft.body.clone());
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

/// Carries the King's answer to a question the court is waiting on.
///
/// Deliberately a `#[server]` function rather than a message on the watch
/// socket. The socket exists for what HTTP cannot do -- let the server speak
/// first -- and this direction is an ordinary request the browser initiates.
/// Sending it over the socket would mean hand-rolling a request/response
/// protocol with no type checking across it, throwing away the main reason this
/// project is Rust on both ends.
///
/// Returns the plan, so the caller sees the same state everything else does.
/// The deed is settled by the turn loop when the parked call resumes, not here:
/// this only unblocks it. Recording the outcome in two places is how a
/// transcript ends up disagreeing with itself.
#[server(AnswerQuestion, "/api")]
pub async fn answer_question(
    plan: String,
    tool_call: String,
    answer: String,
) -> Result<Plan, ServerFnError> {
    let plan_id = PlanId::new(plan);

    if !crate::tools::ask_user_question::answer(&plan_id, &tool_call, answer) {
        return Err(ServerFnError::new(
            "Nothing is waiting on that answer. It may have been answered in \
             another tab, or the server may have restarted since it was asked.",
        ));
    }

    snapshot(&plan_id).ok_or_else(|| ServerFnError::new("That plan is no longer in the records."))
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

        // An errand's workspace is a *clone of its parent's* -- same path, same
        // branch, same id -- because an errand works alongside the plan that
        // sent it rather than in a checkout of its own.
        //
        // So finishing one would merge the parent's half-finished work and then
        // delete the worktree out from under a plan still running in it. The
        // chamber never offers the button, but the blast radius here is the
        // King's actual work, so the guard is here rather than in the UI.
        if existing.is_subagent() {
            return Err(ServerFnError::new(
                "This is an errand, and it works in the worktree of the plan that \
                 sent it -- it has nothing of its own to merge or archive. \
                 Finish the plan that sent it instead.",
            ));
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

    // Only once the work has actually landed. A refused merge leaves the plan
    // in play, and killing the court's dev server under a plan the King is
    // about to retry would take away the thing he needs to see to fix it.
    if matches!(finish, Finish::Settled(_)) {
        crate::tools::tmux::dismiss(&plan_id).await;
    }

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

    /// The guard with the largest blast radius in the codebase.
    ///
    /// An errand's workspace is a clone of its parent's, so finishing one would
    /// merge the parent's half-finished work and then delete the worktree from
    /// under a plan still running in it. The chamber never offers the button --
    /// but "the UI does not offer it" is not a guarantee, and the thing being
    /// protected is the King's real work.
    ///
    /// Tested through the guard's own predicate rather than through
    /// `finish_plan`, which needs a git repository, a scanned kingdom and a
    /// process-global lock to reach: this pins the decision, and the git work
    /// below it is `worktree.rs`'s business and already has its own tests.
    #[test]
    fn a_subagent_is_never_finished_on_its_own() {
        use kingdom_core::{CityId, ModelChoice, PlanId, Workspace};

        let parent = Plan::opened(
            PlanId::new("plan-1"),
            CityId::new("c1"),
            "Make the tests pass",
            &ModelChoice::new("mock", None),
            Workspace {
                mode: kingdom_core::WorkspaceMode::Fresh,
                path: "/dev/testburg/.kingdom/abc".into(),
                branch: Some("kingdom/make-the-tests-pass".into()),
                id: Some("abc".into()),
                base: Some("main".into()),
            },
        );
        let subagent = Plan::spawned(PlanId::new("plan-2"), &parent, "call-1", "Read the tests");

        assert_eq!(
            subagent.workspace, parent.workspace,
            "an errand works in its parent's checkout -- which is precisely why \
             finishing it would destroy work that is not its own"
        );
        assert!(
            subagent.is_subagent(),
            "the predicate `finish_plan` refuses on must hold for a sent plan"
        );
        assert!(
            !parent.is_subagent(),
            "and must not hold for the plan that sent it, or the King could \
             never finish anything"
        );
    }
}
