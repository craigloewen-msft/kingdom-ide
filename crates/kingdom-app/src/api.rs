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
pub(crate) fn lock() -> Result<std::sync::MutexGuard<'static, Kingdom>, ServerFnError> {
    state::get()
        .lock()
        .map_err(|e| ServerFnError::new(format!("kingdom state poisoned: {e}")))
}

#[cfg(feature = "ssr")]
pub(crate) fn next_plan_number() -> u64 {
    state::PLAN_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

/// Applies a change to one plan, records it, publishes it, and returns the
/// result, so callers hand the browser the same value that was just stored.
///
/// The single funnel for plan mutations, which is why both persistence and push
/// hang off it: a caller cannot change a plan and forget to write it, and
/// equally cannot change a plan and forget to tell the conversation watching
/// it. See [`crate::events`] for why that second one had to live here rather
/// than at each call site.
#[cfg(feature = "ssr")]
pub(crate) fn update(
    kingdom: &mut Kingdom,
    id: &PlanId,
    change: impl FnOnce(&mut Plan),
) -> Option<Plan> {
    let root = std::path::PathBuf::from(&kingdom.root);
    // Resolved before the plan is borrowed mutably, and passed down rather than
    // looked up again: publishing attaches the city's shared services, and the
    // kingdom lock this caller is holding cannot be taken a second time.
    let city_root = city_root_in(kingdom, id);
    let plan = kingdom.plans.iter_mut().find(|p| &p.id == id)?;
    change(plan);
    remember(&root, plan);
    // After `remember`, not before: a failed write appends a note to the plan,
    // and the conversation should be told the thing that was actually stored
    // rather than an optimistic version of it.
    crate::events::publish_within(plan, city_root.as_deref());
    Some(plan.clone())
}

/// One plan as the server currently has it.
///
/// Exists for the watch socket's opening message, which needs a plan without
/// going through a `#[server]` function -- it is already inside the server.
/// Returns `None` rather than erroring for an unknown id: a conversation may
/// connect to a plan that has since been forgotten, and an empty stream is the
/// honest answer.
#[cfg(feature = "ssr")]
pub fn snapshot(id: &PlanId) -> Option<Plan> {
    lock().ok()?.plan(id).cloned()
}

/// The whole kingdom as the server currently has it.
///
/// The counterpart to [`snapshot`], and there for the same reason: the map's
/// route is a plain Axum handler already inside the server, so it needs the
/// state without going out through a `#[server]` function.
#[cfg(feature = "ssr")]
pub fn kingdom_snapshot() -> Option<Kingdom> {
    lock().ok().map(|kingdom| kingdom.clone())
}

/// Where a plan's *project* lives, as opposed to where the plan works.
///
/// The distinction is the whole point. A plan works in its own worktree under
/// `<city>/.kingdom/`, but its shared services belong to the **city** -- so
/// five plans on one project must resolve to one path here, or they would each
/// raise a well of their own and share nothing.
///
/// It is also why the services manifest is read from here rather than from the
/// worktree: a plan that edits `services.toml` does not thereby get a private
/// database mid-flight. Its change takes effect when the work is merged.
#[cfg(feature = "ssr")]
pub fn city_root_of(id: &PlanId) -> Option<std::path::PathBuf> {
    let kingdom = lock().ok()?;
    city_root_in(&kingdom, id)
}

/// [`city_root_of`] for a caller that is already holding the kingdom.
///
/// The kingdom's mutex is a plain [`std::sync::Mutex`], which is **not**
/// reentrant: a thread that asks for it twice deadlocks itself and leaves the
/// lock held forever, so every later request hangs too. [`update`] runs with the
/// guard in hand and publishes from inside it, so anything on that path -- which
/// now includes resolving a plan's city for its shared services -- must take the
/// kingdom as an argument rather than reaching for the lock again.
#[cfg(feature = "ssr")]
pub fn city_root_in(kingdom: &Kingdom, id: &PlanId) -> Option<std::path::PathBuf> {
    let plan = kingdom.plan(id)?;
    let city = kingdom.city(&plan.city)?;
    Some(std::path::Path::new(&kingdom.root).join(&city.path))
}

/// A plan on its way to a browser and nowhere else.
///
/// Every `#[server]` function below hands back the plan it just changed, and
/// the browser does exactly what the watch socket's receiver does with it:
/// `Kingdom::insert`. So they are the same wire, and they carry the same
/// needless weight -- [`Plan::for_wire`] has the measurements.
///
/// It matters most on [`say`], which is the King *typing*: without this, every
/// message he sends is answered with a copy of the entire transcript including
/// megabytes of provider signatures, parsed and cloned in the same frame as his
/// next keystroke.
///
/// This is also where the plan's runtime facts -- ports, shared services --
/// are fitted, exactly as the watch socket's push path does it (see
/// `crate::events::on_the_wire`). A reply that skipped this used to be how a
/// browser's own request for a fresh copy of a plan overwrote a good,
/// ports-carrying copy the watch socket had just pushed: this function and
/// [`to_browser_in`] are now the one seam every route to a browser must cross,
/// and `Plan::for_wire` alone is never enough.
///
/// Named rather than inlined so that the rule is visible at each call site, and
/// so a function that is ever consumed server-side is an obvious exception
/// rather than a silent one. Nothing here changes what is stored.
#[cfg(feature = "ssr")]
fn to_browser(plan: Plan) -> Plan {
    let city_root = city_root_of(&plan.id);
    crate::events::on_the_wire(&plan, city_root.as_deref())
}

/// [`to_browser`] for a caller that is already holding the kingdom.
///
/// The kingdom's mutex is a plain [`std::sync::Mutex`], not reentrant -- the
/// same reason [`city_root_in`] exists next to [`city_root_of`]. A caller with
/// the guard in hand resolves the city through that guard rather than through
/// `to_browser`, which would deadlock the server while holding the lock.
#[cfg(feature = "ssr")]
fn to_browser_in(kingdom: &Kingdom, plan: Plan) -> Plan {
    let city_root = city_root_in(kingdom, &plan.id);
    crate::events::on_the_wire(&plan, city_root.as_deref())
}

/// Writes a plan to the records, turning a failed write into something the user
/// can see rather than something that fails his prompt.
///
/// Refusing the work because the disk was full would be a worse outcome than an
/// unsaved plan he can see is unsaved -- the work itself is on a branch either
/// way, and it is only the bookkeeping that was lost.
#[cfg(feature = "ssr")]
pub(crate) fn remember(root: &std::path::Path, plan: &mut Plan) {
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

/// [`to_browser_in`] for a caller that has an entire [`Kingdom`], not one
/// plan -- `get_kingdom`, `open_kingdom`, `enter_proving_grounds`. Fits every
/// plan's runtime facts the same way, resolving each plan's city through the
/// kingdom it was already given rather than re-locking, so it is safe to call
/// whether or not the caller holds the global guard.
///
/// Without this, a whole-kingdom reply overwrote a plan's ports and shared
/// services with nothing the moment it crossed: the browser's own refetches
/// after `speak`, `finish`, `send_review` and `draft` were exactly how the
/// ports badge went blank the instant a turn ended, because each of those
/// asks for the whole kingdom again right after the good, ports-carrying copy
/// the watch socket had just pushed.
#[cfg(feature = "ssr")]
fn kingdom_to_browser(kingdom: &Kingdom) -> Kingdom {
    let mut wire = kingdom.for_wire();
    for plan in wire.plans.iter_mut() {
        let city_root = city_root_in(kingdom, &plan.id);
        *plan = crate::events::on_the_wire(plan, city_root.as_deref());
    }
    wire
}

/// Returns the currently open kingdom, or an empty one if none is open.
///
/// [`kingdom_to_browser`], because this is the app's opening fetch and the
/// largest single transfer it makes: every plan at once, where a push carries
/// one. Stripping the model's opaque thinking here is what takes a real
/// kingdom's page from 13.9 MB down. Server state is untouched.
#[server(GetKingdom, "/api")]
pub async fn get_kingdom() -> Result<Kingdom, ServerFnError> {
    let kingdom = lock()?;
    Ok(kingdom_to_browser(&kingdom))
}

/// Opens a dev folder as the kingdom: scans it for cities and seats a model.
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
    // `kingdom_to_browser` on the way out only. `assemble` has already stored
    // the whole kingdom, opaque thinking and all; what is narrowed and fitted
    // is the copy this browser is handed.
    assemble(&root, None).map(|k| kingdom_to_browser(&k))
}

/// Seeds a proving ground if needed and opens it.
///
/// The one-click path from a cold clone to a populated map. It exists so the
/// *safe* option is also the *easy* one: the opening screen otherwise demands an
/// absolute path to real work before showing anything at all, which makes
/// pointing the tool at real files the default first act.
#[server(EnterProvingGrounds, "/api")]
pub async fn enter_proving_grounds(fixture: Option<String>) -> Result<Kingdom, ServerFnError> {
    let name = fixture.unwrap_or_else(|| kingdom_core::mockdata::DEFAULT_FIXTURE.to_string());
    // `kingdom_to_browser` here and not inside `open_fixture`, which the boot
    // path also calls with no browser in sight. See `get_kingdom`.
    open_fixture(&name)
        .map(|k| kingdom_to_browser(&k))
        .map_err(ServerFnError::new)
}

/// Seeds a named proving ground if it is not already standing, and opens it.
///
/// Not `async`, and not a `#[server]` function, because the server itself needs
/// this too: `KINGDOM_REALM` opens a realm at boot, where there is no browser to
/// call anything. Both paths go through here rather than each doing its own
/// seed-then-scan, for the same reason [`assemble`] is shared -- two ways of
/// opening a realm would differ in some detail nobody notices until one of them
/// is wrong.
///
/// Errors are plain `String`s so the boot path can print one without dragging
/// `ServerFnError` into a context that has no server function in it.
#[cfg(feature = "ssr")]
pub fn open_fixture(name: &str) -> Result<Kingdom, String> {
    use kingdom_core::mockdata;

    let spec = mockdata::fixture(name).ok_or_else(|| {
        format!(
            "No such realm: {name}. Known realms: {}.",
            mockdata::fixture_names().join(", ")
        )
    })?;

    let root = crate::mock::fixture_path(name);

    // Only seed when it is not already there, so entering twice is instant and
    // does not silently discard a fixture the user has been poking at.
    if !crate::mock::is_proving_ground(&root) {
        crate::mock::seed(&spec, &root)
            .map_err(|e| format!("Could not raise the proving grounds: {e}"))?;
    }

    let root = root
        .canonicalize()
        .map_err(|e| format!("Could not resolve {}: {e}", root.display()))?;

    assemble(&root, Some(spec.starter_plans)).map_err(|e| e.to_string())
}

/// Opens the kingdom the King last chose, if there is one on record.
///
/// Not a `#[server]` function, for the same reason [`open_fixture`] is not: the
/// caller is the boot path, where there is no browser to call anything.
///
/// `Ok(None)` means nothing was recorded -- the ordinary first run, and not a
/// problem. An `Err` means something *was* recorded and could not be honoured,
/// which is worth saying out loud before falling back to the picker.
///
/// The sandbox check is the part that matters: without it, `KINGDOM_SANDBOX=1`
/// would be quietly defeated by a root remembered from a session that ran
/// without it. It goes through the same canonicalising [`enforce_sandbox`] the
/// browser path uses rather than a second, looser rule.
#[cfg(feature = "ssr")]
pub fn open_last_kingdom() -> Result<Option<Kingdom>, String> {
    let Some(root) = crate::profile::last_kingdom() else {
        return Ok(None);
    };

    if !root.is_dir() {
        return Err(format!(
            "{} is no longer a folder. Choose a kingdom again.",
            root.display()
        ));
    }

    enforce_sandbox(&root).map_err(|e| e.to_string())?;
    assemble(&root, None).map(Some).map_err(|e| e.to_string())
}

/// Closes the kingdom, returning the King to the opening screen.
///
/// The door out of an auto-opened kingdom. Without it, remembering the folder
/// would make the picker unreachable -- the app only shows it when no kingdom
/// is open, and one always would be. Forgetting is deliberate rather than
/// incidental: the next start must ask again, not reopen the folder just left.
///
/// Nothing recorded is deleted. The plans stay in the profile and come back
/// with the kingdom.
#[server(LeaveKingdom, "/api")]
pub async fn leave_kingdom() -> Result<(), ServerFnError> {
    *lock()? = Kingdom::unopened();
    crate::profile::forget_kingdom();
    // The dedupe is keyed by plan id and would otherwise carry a closed
    // kingdom's answers into the next one, so plans that had not changed since
    // would open silently and their badges never arrive.
    crate::events::forget_pulses();
    // Every agent in that kingdom has just gone away, so its wells are stopped.
    // Without this they stayed up for the life of the server, claimed by plans
    // nobody had open -- the one gap where "stopped when the last plan is done"
    // was not true.
    //
    // Reconciled against an explicitly empty population rather than against the
    // unopened kingdom above. The two are the same answer, and this one says so
    // without taking the lock a second time.
    reconcile_wells(&Kingdom::unopened());
    Ok(())
}

/// Every fixture the user can enter, for the opening screen.
#[server(ListRealms, "/api")]
pub async fn list_fixtures() -> Result<Vec<(String, String)>, ServerFnError> {
    Ok(kingdom_core::mockdata::fixtures()
        .into_iter()
        .map(|r| (r.name.to_string(), r.blurb.to_string()))
        .collect())
}

/// Scans a folder and seats a model over it, then stores it as the kingdom.
///
/// Shared by the real-folder and proving-ground paths so the two cannot drift:
/// a proving ground goes through exactly the same scanner, and the only
/// difference is which model is seated and the `sandbox` flag.
#[cfg(feature = "ssr")]
pub(crate) fn assemble(
    root: &std::path::Path,
    starter_plans: Option<kingdom_core::mockdata::StarterPlansFn>,
) -> Result<Kingdom, ServerFnError> {
    use crate::scan::scan_kingdom;

    let cities = scan_kingdom(root)
        .map_err(|e| ServerFnError::new(format!("Could not read {}: {e}", root.display())))?;

    // A kingdom recorded under the old layout -- inside its own root -- has its
    // records copied into the profile before anything reads them. Once only,
    // and the originals are left where they are; `profile::migrate` says why.
    if let Some(line) = crate::profile::migrate(root) {
        println!("  {line}");
    }

    // Cities are rescanned every time -- disk is their source of truth. Plans
    // are not: they are the one thing here that disk cannot tell us again.
    let recorded = crate::store::load(root);
    let seeding_starter_plans = recorded.is_empty();
    let starter_plans = starter_plans.unwrap_or(kingdom_core::sample::starter_plans);
    let plans = seed_starter_plans(recorded, &cities, starter_plans);

    // A fabricated model is fabricated exactly once per kingdom. Written
    // immediately so the next open reads it back as ordinary history rather
    // than seating a second one over the top of the first.
    if seeding_starter_plans && !plans.is_empty() {
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

    // Recorded here rather than in each caller, so the real-folder and
    // proving-ground paths cannot drift -- and only ever *after* a folder has
    // actually opened, so a typo is never remembered and reopened at boot.
    crate::profile::remember_kingdom(root);

    // And the kingdom's wells are brought into line with the agents that are
    // actually alive in it, which is what makes a restart invisible: a
    // namespace lives in a process and a container does not, so five agents
    // that had a database before the server stopped must find it again without
    // any of them having to take a turn first.
    //
    // Here rather than in each caller for exactly the reason above -- every
    // path into a kingdom crosses this function.
    reconcile_wells(&kingdom);

    Ok(kingdom)
}

/// Brings the shared resources into line with this kingdom's live agents.
///
/// The one call every moment that changes the population makes: a kingdom
/// opened, a plan opened, a plan finished, a kingdom closed. It both raises and
/// stops, because `services::reconcile` computes the two from one input -- see
/// the invariant in that module.
///
/// # Why this is spawned rather than awaited
///
/// Raising a well can legitimately take minutes -- `services::READY_TIMEOUT` is
/// three of them, because the first run of an image includes pulling it. The
/// King must not sit on a folder picker for that, nor wait on `docker stop`
/// after pressing Merge: the screen moves at once and the wells follow, which
/// is also why `/resources` polls.
///
/// # Why the agents are collected here and not inside `services`
///
/// Because resolving a plan's city means reading the kingdom, and callers of
/// [`assemble`] hold its **non-reentrant** [`std::sync::Mutex`]. Going through
/// `city_root_of` would take that lock a second time on the same thread, which
/// is the deadlock [`city_root_in`] and [`kingdom_to_browser`] already exist to
/// avoid. So the population is read from the `Kingdom` value already in hand,
/// and only plain paths cross into the spawned task.
#[cfg(feature = "ssr")]
pub(crate) fn reconcile_wells(kingdom: &Kingdom) {
    let agents = agents_drawing(kingdom);

    // A test assembling a kingdom has no runtime, and panicking there would
    // make an unrelated test fail for a reason that has nothing to do with it.
    if let Ok(handle) = tokio::runtime::Handle::try_current() {
        handle.spawn(crate::services::reconcile(agents));
    }
}

/// Every agent that should be holding a well, with the city it works in.
///
/// Pure, and separate from [`reconcile_wells`], so the rule can be tested
/// without a Docker daemon, a tokio runtime or the process-global kingdom.
///
/// Two filters, both load-bearing:
///
/// - **`is_live`** -- a settled plan's worktree is gone and it is history. A
///   merged plan left holding a database would pin it open for the life of the
///   process. `Failed` still counts, agreeing with [`kingdom_network`] and
///   [`kingdom_changes`]: it can be retried and its workspace is still there.
/// - **`!is_subagent`** -- this one is not tidiness. A subagent works in its
///   parent's worktree and reaches the well through the parent's claim, and
///   [`finish_plan`] *refuses* to finish one. So a subagent recorded as a drawer
///   could never be released, and would hold its well open forever.
#[cfg(feature = "ssr")]
pub(crate) fn agents_drawing(kingdom: &Kingdom) -> Vec<(PlanId, std::path::PathBuf)> {
    kingdom
        .plans
        .iter()
        .filter(|plan| plan.is_live() && !plan.is_subagent())
        .filter_map(|plan| Some((plan.id.clone(), city_root_in(kingdom, &plan.id)?)))
        .collect()
}

/// The plans a freshly opened kingdom starts with.
///
/// A model is seated **only over an empty store**. Fabricating one every time
/// would duplicate the whole opening model on the second open -- the user's
/// real replies to a sample plan sitting beside a pristine copy of the same
/// plan. Split out from [`assemble`] so the rule is testable without the
/// process-global kingdom.
#[cfg(feature = "ssr")]
fn seed_starter_plans(
    recorded: Vec<Plan>,
    cities: &[kingdom_core::City],
    starter_plans: kingdom_core::mockdata::StarterPlansFn,
) -> Vec<Plan> {
    if recorded.is_empty() {
        starter_plans(cities)
    } else {
        recorded
    }
}

/// Refuses any folder outside the sandbox when `KINGDOM_SANDBOX` is set.
///
/// This is the setting for a session where Kingdom IDE is working on Kingdom
/// IDE. It turns "I meant to open the fake one" from something the user must
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
/// The split between opening and drafting is what lets the user land in the
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
    network: Option<kingdom_core::NetworkMode>,
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
    // user's browser remembers but Copilot no longer serves degrades to the
    // default rather than failing the prompt outright.
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
    // the workspace is cut. `slug_for_prompt` is the same derivation
    // `Plan::opened` uses below, so the plan's `slug` and its branch agree.
    let workspace =
        crate::worktree::prepare(&city_root, &mode, &kingdom_core::slug_for_prompt(&prompt))
            .await
            .map_err(|e| ServerFnError::new(e.to_string()))?;

    let network = network.unwrap_or_default();

    // Refused *before* a plan exists, not when its first command runs. A plan
    // that took the setting and then quietly ran on the shared network would be
    // exactly the invisible isolation this feature exists to end -- and the
    // King would only find out when two agents collided on 3000 anyway.
    if network.is_isolated() {
        crate::netns::availability().map_err(|e| ServerFnError::new(e.to_string()))?;
    }

    let mut plan = Plan::opened(
        plan_id,
        city_id,
        &prompt,
        &choice,
        workspace.clone(),
        network,
    );
    // Where an agent is fenced in is not something it said, it is something that
    // happened -- and isolation the user cannot see is isolation he cannot
    // trust, so it is recorded in the log rather than only in the header.
    plan.note(
        NoteKind::Workspace,
        match &workspace.branch {
            Some(branch) => format!("Working in {} on {branch}.", workspace.path),
            None => format!("Working directly in {}, with no isolation.", workspace.path),
        },
    );
    // The same argument, for the other axis. A plan with its own network can
    // bind whatever port it likes, and the King needs to know that the `:3000`
    // this plan talks about is not the `:3000` in his own browser.
    if network.is_isolated() {
        plan.note(
            NoteKind::Workspace,
            "On a network of its own: ports it opens belong to this plan alone, \
             and are forwarded to the host on ports shown in the chamber."
                .to_string(),
        );
    }

    let mut kingdom = lock()?;
    let root = std::path::PathBuf::from(&kingdom.root);
    remember(&root, &mut plan);
    kingdom.plans.push(plan.clone());
    // Opening does not go through `update`, so nothing else would announce it.
    // Without this a plan opened in one tab is invisible in another's rail
    // until something refetches the kingdom.
    crate::events::pulse(&plan);
    // A plan opened after the kingdom was has never crossed `assemble`, so this
    // is what enrols it as a drawer -- and raises the city's wells if it is the
    // first agent there. Taking its first turn no longer does this.
    reconcile_wells(&kingdom);

    Ok(plan)
}

/// Whether this machine can give a plan a network of its own.
///
/// `None` when it can; the string is what to tell the King when it cannot, and
/// it names the package to install. Asked by the prompt bar so the option can
/// be offered as *disabled with a reason* rather than offered and then refused
/// after he has typed his prompt.
#[server(NetworkAvailable, "/api")]
pub async fn network_available() -> Result<Option<String>, ServerFnError> {
    Ok(crate::netns::availability().err().map(|e| e.to_string()))
}

/// Local branches in a city's repository, for the workspace picker.
///
/// Offered as a list so the user picks a branch that exists rather than typing
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

/// One directory of a plan's workspace, listed on demand for the files rail.
///
/// `path` is relative to the workspace root; empty lists the root itself.
/// Returns directories first, then files, each case-insensitively by name -- the
/// order a person expects to read a tree in, which the map's own tree
/// deliberately does not use (it sorts by size, because a skyline is drawn
/// largest-first).
///
/// # Why the plan's workspace and not the city's checkout
///
/// This panel lives inside a plan's chamber, and an isolated plan works in a
/// **worktree**. Keyed on the city, the rail listed one copy of the project
/// while the court edited another -- tolerable while the tree was read-only
/// decoration, and not tolerable now that the King can write "line 34 is wrong"
/// against it. Line 34 of the city's checkout and line 34 of the plan's
/// worktree are different lines, so the model would be sent an objection about
/// code it cannot see. That is the same class of silent wrongness the merge-base
/// decision in [`crate::review`] exists to prevent, and it is answered the same
/// way: read the workspace the work is actually happening in.
///
/// The workspace goes through [`grounded`] first, because the *placeholder*
/// plans a kingdom opens with carry a path relative to the kingdom root.
///
/// # Why this exists rather than reading [`kingdom_core::City::structure`]
///
/// The city already carries a [`kingdom_core::Folder`] tree, and reusing it
/// would cost nothing -- but it is the *map's* tree. [`crate::scan`] keeps only
/// the largest `FILES_PER_DISTRICT` files per folder, sorted by byte size, to a
/// bounded depth, and drops empty folders. That is right for a skyline, where
/// the remainder is still weighed in `extra_files`, and wrong for a panel whose
/// whole promise is "these are the files": a tree silently missing `main.rs`
/// because it is small is worse than no tree. Listing one directory at a time
/// also means nothing walks a monorepo until the King opens the folder.
///
/// # The boundary
///
/// This is the second place in Kingdom where an outsider names a path and the
/// server opens it, so it is held to the same rule as the first: the path is
/// resolved through a [`crate::tools::Sandbox`] rooted at the workspace, exactly
/// as [`crate::artifact`] does, so `a/../../etc` is refused lexically before the
/// filesystem sees it. It lists **names only** and reads no file contents; what
/// reads one is [`plan_source`], which has its own guards.
#[server(ListDirectory, "/api")]
pub async fn list_directory(
    plan: String,
    path: String,
) -> Result<Vec<kingdom_core::DirEntry>, ServerFnError> {
    let plan_id = PlanId::new(plan);
    let root = {
        let kingdom = lock()?;
        let Some(plan) = kingdom.plan(&plan_id) else {
            // A plan Kingdom does not know has no directory to read. An empty
            // listing rather than an error: the rail asks about whatever
            // chamber is open, and a plan that has gone from the records while
            // a tab sat on it is ordinary.
            return Ok(Vec::new());
        };
        std::path::PathBuf::from(grounded(&kingdom.root, &plan.workspace).path)
    };

    read_directory(&root, &path).map_err(ServerFnError::new)
}

/// Everything [`list_directory`] decides once the workspace root is known.
///
/// Split out so the boundary and the ordering can be tested against a real
/// directory without a kingdom in global state -- the same split, for the same
/// reason, as `artifact::from_workspace`.
#[cfg(feature = "ssr")]
fn read_directory(
    workspace_root: &std::path::Path,
    path: &str,
) -> Result<Vec<kingdom_core::DirEntry>, String> {
    use kingdom_core::{DirEntry, Language, Workspace};

    let shop = crate::tools::Sandbox::new(Workspace::in_place(workspace_root.to_string_lossy()));
    let dir = shop
        .resolve(path)
        .map_err(|_| format!("{path} is outside this plan's workspace."))?;

    let Ok(entries) = std::fs::read_dir(&dir) else {
        // A folder that cannot be read is an empty one as far as the rail is
        // concerned: a permission-denied subdirectory should not fail the whole
        // listing the King asked for.
        return Ok(Vec::new());
    };

    let mut listed = Vec::new();

    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();

        // `file_type` avoids the second stat `is_dir()` would cost. An entry
        // whose type cannot be read is skipped rather than guessed at.
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        let is_dir = file_type.is_dir();

        if !is_dir && !file_type.is_file() {
            continue;
        }

        // Build detritus only. Dotfiles are otherwise *shown*: `scan.rs` hides
        // them because a skyline of `.venv` is noise, but a source tree that
        // hides `.github` and `.gitignore` is not the repository the King is
        // looking at.
        if is_dir && crate::scan::SKIP_DIRS.contains(&name.as_str()) {
            continue;
        }

        let child = if path.is_empty() {
            name.clone()
        } else {
            format!("{}/{}", path.trim_end_matches('/'), name)
        };

        listed.push(DirEntry {
            language: if is_dir {
                Language::Other
            } else {
                Language::from_path(&child)
            },
            name,
            path: child,
            is_dir,
        });
    }

    // Directories first, then by name ignoring case. `read_dir` yields in
    // whatever order the filesystem holds, which differs between machines, so
    // sorting here is what makes the rail the same tree everywhere.
    listed.sort_by(|a, b| {
        b.is_dir
            .cmp(&a.is_dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
            .then_with(|| a.name.cmp(&b.name))
    });

    Ok(listed)
}

/// What one plan has changed against its city's default branch.
///
/// Keyed on the **plan** rather than the city, because the comparison is of the
/// plan's own workspace: an isolated plan works in a worktree, and the city's
/// checkout would show the King somebody else's files.
///
/// Never an error for an ordinary absence -- a disposed worktree, a project
/// without git, a repository with no default branch all come back as an empty
/// summary carrying a note that says which. The drawer exists to say what is
/// true, and "this is not a repository" is an answer rather than a failure. A
/// plan the records no longer hold is the one real error, because there is then
/// no workspace to name.
#[server(PlanChanges, "/api")]
pub async fn plan_changes(plan: String) -> Result<kingdom_core::ChangeSummary, ServerFnError> {
    let plan_id = PlanId::new(plan);
    let workspace = {
        let kingdom = lock()?;
        let Some(plan) = kingdom.plan(&plan_id) else {
            return Err(ServerFnError::new("That plan is no longer in the records."));
        };
        grounded(&kingdom.root, &plan.workspace)
    };

    Ok(crate::review::changes(&workspace).await)
}

/// What **every live agent in the kingdom** has changed, as one answer.
///
/// The plural of [`plan_changes`], and it exists because the map's question
/// changed twice. "What did my agent do?" is answered by one plan's summary;
/// "who is touching this file?" cannot be, however many times it is asked. And
/// "what is every agent doing right now?" -- the first of the three questions in
/// `AGENTS.md` -- cannot be answered by *one city's* agents either, which is
/// what this used to take.
///
/// # Why it is no longer given a city
///
/// It was, and a city nobody had selected therefore drew nothing: the browser
/// blanked the works whenever the selection was empty, so the map answered
/// "what is every agent doing" with a picture of at most one project. The
/// selection is a statement about *where the King is looking*, and what his
/// agents are doing is true whether he is looking or not.
///
/// # Which plans
///
/// Live, non-subagent plans, anywhere in the kingdom. The subagent exclusion is
/// [`Kingdom::plans_in`]'s and is kept for its reason: a subagent works inside
/// the worktree of the plan that sent it, so counting it would draw one piece of
/// work several times over. Settled plans are excluded because their worktrees
/// are gone -- there is nothing on disk to diff.
///
/// # Why the reads are concurrent
///
/// Each summary is a few `git` invocations against a different worktree, and
/// they share nothing. Run in sequence, a kingdom with six agents would cost six
/// times one plan's latency on every refetch; spawned together, it costs the
/// slowest one. That bargain is what makes the wider question affordable at all.
/// The kingdom lock is released before any of them start, which matters more
/// than the concurrency: `review::changes` shells out, and holding the mutex
/// across that would park every other request behind the slowest git in the
/// kingdom.
///
/// # Why one bad plan does not fail the request
///
/// [`crate::review::changes`] never errors for an ordinary absence -- a
/// disposed worktree, a project without git -- it answers with an empty summary
/// carrying a note. A plan whose read panics its task is dropped from the
/// answer rather than blanking the map for every other agent, which is the same
/// judgement `plan_changes` makes when it refuses to blank a list the King is
/// reading.
///
/// Sorted by plan id, and that is load-bearing rather than tidiness:
/// `kingdom_core::palette::assign_banners` resolves a colour collision by
/// position, so an unstable order here would let two agents swap colours
/// between refetches. It matters more now than it did: the banners are assigned
/// across the whole kingdom, so the set being ordered is every live plan rather
/// than one city's.
#[server(KingdomChanges, "/api")]
pub async fn kingdom_changes() -> Result<Vec<kingdom_core::PlanChanges>, ServerFnError> {
    // The lock is taken only to decide *what* to read, never across the reads
    // themselves.
    let workspaces: Vec<(
        kingdom_core::PlanId,
        kingdom_core::CityId,
        kingdom_core::Workspace,
    )> = {
        let kingdom = lock()?;
        let root = &kingdom.root;
        let mut found: Vec<_> = kingdom
            .plans
            .iter()
            .filter(|plan| plan.is_live() && !plan.is_subagent())
            .map(|plan| {
                (
                    plan.id.clone(),
                    plan.city.clone(),
                    grounded(root, &plan.workspace),
                )
            })
            .collect();
        found.sort_by(|a, b| a.0.cmp(&b.0));
        found
    };

    let reads: Vec<_> = workspaces
        .into_iter()
        .map(|(plan, city, workspace)| {
            tokio::spawn(async move {
                kingdom_core::PlanChanges {
                    plan,
                    city,
                    changes: crate::review::changes(&workspace).await,
                }
            })
        })
        .collect();

    let mut answered = Vec::with_capacity(reads.len());
    for read in reads {
        // A task that failed is dropped rather than failing the whole answer:
        // one unreadable worktree must not blank the map for every other agent.
        if let Ok(one) = read.await {
            answered.push(one);
        }
    }

    Ok(answered)
}

/// One live agent, as the network feed collects it under the kingdom lock.
///
/// A named type because the tuple it replaces was complex enough for clippy to
/// object, and because naming it is what makes the collecting pass in
/// [`kingdom_network`] readable: it is deliberately *everything from the
/// kingdom* that the readers outside the lock will need.
#[cfg(feature = "ssr")]
type LiveAgent = (
    kingdom_core::PlanId,
    // The plan's title: the map paints it on a plaque over the agent's marker,
    // and it is read here because only the kingdom knows it.
    String,
    kingdom_core::CityId,
    kingdom_core::NetworkMode,
);

/// A city with live agents in it, and where its project sits on disk.
#[cfg(feature = "ssr")]
type CityRoot = (kingdom_core::CityId, std::path::PathBuf);

/// What every agent in the kingdom is plugged into, and what its cities share.
///
/// The sibling of [`kingdom_changes`], and the map draws its wells, its host
/// ring and its agent markers from this. Where that one answers *what is each
/// agent changing*, this answers *what is each agent connected to* -- the
/// second of the three questions in `AGENTS.md`.
///
/// # The lock discipline, which is the whole of the danger here
///
/// The kingdom's mutex is a plain [`std::sync::Mutex`] and is **not**
/// reentrant. Everything below reads runtime state that lives *outside* the
/// kingdom -- `services::` for the wells, `netns::` for the forwards -- and the
/// mapping from a plan to its city root lives *inside* it. So the guard is
/// taken **once**, everything that needs it is collected in one pass, and it is
/// dropped before a single service or namespace is asked anything.
///
/// That is not a preference. `api::update` publishes with the guard in hand,
/// and a lookup from inside that path once deadlocked the whole server --
/// holding the lock, so every later request hung behind it. `city_root_in`
/// exists precisely so a caller that already holds the kingdom can resolve a
/// city without asking for it again, and this uses it for that reason.
///
/// # Why a city with nothing standing is left out
///
/// Because that is nearly every project. A well is a container a *declared*
/// service is running in, so a city with no `.kingdom/services.toml` has none
/// and is simply absent -- the same judgement `Kingdom::activity` makes about a
/// town with nothing running, and for the same reason: the common answer on a
/// real dev folder is an empty list.
#[server(KingdomNetworkFeed, "/api")]
pub async fn kingdom_network() -> Result<kingdom_core::KingdomNetwork, ServerFnError> {
    // One pass, one guard. Everything the readers below need is taken here and
    // the lock is dropped at the end of the block -- see the note above on why
    // that boundary is load-bearing rather than tidy.
    let (agents, city_roots): (Vec<LiveAgent>, Vec<CityRoot>) = {
        let kingdom = lock()?;

        // Subagents are excluded to agree with `kingdom_changes` and with
        // `Kingdom::plans_in`: a subagent works in its parent's worktree and on
        // its parent's network, so a marker of its own would draw one agent
        // twice.
        let mut agents: Vec<_> = kingdom
            .plans
            .iter()
            .filter(|plan| plan.is_live() && !plan.is_subagent())
            .map(|plan| {
                (
                    plan.id.clone(),
                    plan.title.clone(),
                    plan.city.clone(),
                    plan.network,
                )
            })
            .collect();
        // Sorted for `assign_banners`, which resolves a colour collision by
        // position: an unstable order would swap two agents' colours between
        // refetches. The same reason `kingdom_changes` sorts.
        agents.sort_by(|a, b| a.0.cmp(&b.0));

        // Every city that has at least one live agent in it. Keyed by city so a
        // project with five plans is resolved once rather than five times, and
        // resolved through `city_root_in` because the guard is already held.
        let mut roots: Vec<CityRoot> = Vec::new();
        for (plan, _, city, _) in &agents {
            if roots.iter().any(|(known, _)| known == city) {
                continue;
            }
            if let Some(root) = city_root_in(&kingdom, plan) {
                roots.push((city.clone(), root));
            }
        }
        (agents, roots)
    };

    // From here on nothing touches the kingdom.
    let wells: Vec<kingdom_core::CityWells> = city_roots
        .iter()
        .filter_map(|(city, root)| {
            let standing: Vec<kingdom_core::SharedService> = crate::services::running_in(root)
                .into_iter()
                // City scope only. `running_in` also answers with the King's
                // own wells, because a plan working here can reach those too --
                // but a `CityWells` puts a wellhead on a *town's square*, and a
                // host well belongs to no town. Passed through, one Redis would
                // be drawn once in every city with an agent in it: the same
                // container claimed by three projects that do not own it.
                //
                // So the map does not show host wells yet. Its rim already
                // draws the King's machine as a ring, which is where one
                // belongs; putting it there is its own piece of work, noted in
                // docs/roadmap.md rather than approximated here.
                .filter(|service| service.scope == kingdom_core::ServiceScope::City)
                .map(|service| kingdom_core::SharedService {
                    address: service.address(),
                    // By the service's own registry key rather than by city
                    // root. Identical for a city well, and the form that stays
                    // correct if a host well is ever drawn here.
                    users: crate::services::users_of_key(&service.key, &service.name),
                    manifest_path: crate::services::Scope::City(root.clone())
                        .manifest_path()
                        .to_string_lossy()
                        .to_string(),
                    scope: service.scope,
                    name: service.name,
                    image: service.image,
                })
                .collect();
            // Absent rather than empty: see the doc above.
            (!standing.is_empty()).then(|| kingdom_core::CityWells {
                city: city.clone(),
                wells: standing,
            })
        })
        .collect();

    let agents = agents
        .into_iter()
        .map(|(plan, title, city, network)| {
            // Only an isolated plan has forwards, and asking about a plan that
            // has none is answered with an empty list anyway -- the guard is
            // here to say so rather than to avoid a fault.
            let ports = if network.is_isolated() {
                crate::netns::forwards_of(&plan)
                    .into_iter()
                    .map(|(guest, host)| kingdom_core::PortForward { guest, host })
                    .collect()
            } else {
                Vec::new()
            };

            // Which of its city's wells this plan is actually registered as
            // drawing from -- not merely which its city has. See
            // `AgentNetwork::drawing_from`.
            //
            // Filtered to city scope to match `wells` above: these names are
            // looked up against the wellheads drawn on that town's square, and
            // a channel to a well the map does not draw would join an agent to
            // nothing.
            let drawing_from = city_roots
                .iter()
                .find(|(known, _)| known == &city)
                .map(|(_, root)| {
                    crate::services::running_in(root)
                        .into_iter()
                        .filter(|service| service.scope == kingdom_core::ServiceScope::City)
                        // By the service's own key: a well is filed under the
                        // scope that declared it, and asking by city root would
                        // answer "nobody" for anything that is not a city's.
                        .filter(|service| {
                            crate::services::draws_from(&service.key, &service.name, &plan)
                        })
                        .map(|service| service.name)
                        .collect()
                })
                .unwrap_or_default();

            kingdom_core::AgentNetwork {
                plan,
                title,
                city,
                network,
                ports,
                drawing_from,
            }
        })
        .collect();

    Ok(kingdom_core::KingdomNetwork { wells, agents })
}

/// One changed file, as a side-by-side diff against the same base.
///
/// # The boundary
///
/// This is the third place in Kingdom where an outsider names a path and the
/// server opens it, and it is held to the rule the first two set: the path is
/// resolved through a [`crate::tools::Sandbox`] rooted at the plan's workspace
/// -- exactly as [`list_directory`] and [`crate::artifact`] do -- so
/// `a/../../etc/passwd` is refused lexically, before git or the filesystem sees
/// it. What goes on to git is the *workspace-relative* form the sandbox agreed
/// to, never the string as it arrived.
#[server(PlanDiff, "/api")]
pub async fn plan_diff(
    plan: String,
    path: String,
) -> Result<kingdom_core::FileDiff, ServerFnError> {
    let plan_id = PlanId::new(plan);
    let workspace = {
        let kingdom = lock()?;
        let Some(plan) = kingdom.plan(&plan_id) else {
            return Err(ServerFnError::new("That plan is no longer in the records."));
        };
        grounded(&kingdom.root, &plan.workspace)
    };

    let inside = within_workspace(&workspace, &path).map_err(ServerFnError::new)?;

    Ok(crate::review::diff(&workspace, &inside).await)
}

/// The lines a diff left out, when the King asks to see further.
///
/// The panel keeps a change and three lines either side of it, which is often
/// not enough to say *where* a change is -- the function it sits inside starts
/// above the first row shown. This is how the strip between two hunks fills
/// itself in.
///
/// # Why this is a request at all
///
/// Sending the whole file with the diff and folding it in the browser would be
/// simpler and would undo the row cap the panel is built around: a 40,000-line
/// file with one changed line is cheap today precisely because the unchanged
/// 39,990 never leave the server. So the King pays for what he asks to see,
/// when he asks -- and [`crate::review::context`] caps one answer in turn.
///
/// # The boundary
///
/// The sixth place in Kingdom where an outsider names a path and the server
/// opens it, held to the rule the other five keep: resolved through
/// [`within_workspace`] before git or the filesystem sees it.
#[server(PlanDiffContext, "/api")]
pub async fn plan_diff_context(
    plan: String,
    path: String,
    /// Where the run begins in each version, 1-based, and how long it is. Three
    /// numbers rather than a `Gap` because a server function's arguments are a
    /// wire format, and the browser holds the `Gap` they were taken from either
    /// way -- see [`kingdom_core::Gap`] for why both files are named.
    old_from: u32,
    new_from: u32,
    count: u32,
) -> Result<Vec<kingdom_core::DiffRow>, ServerFnError> {
    let plan_id = PlanId::new(plan);
    let workspace = {
        let kingdom = lock()?;
        let Some(plan) = kingdom.plan(&plan_id) else {
            return Err(ServerFnError::new("That plan is no longer in the records."));
        };
        grounded(&kingdom.root, &plan.workspace)
    };

    let inside = within_workspace(&workspace, &path).map_err(ServerFnError::new)?;

    let gap = kingdom_core::Gap {
        old_from,
        new_from,
        count,
    };

    crate::review::context(&workspace, &inside, gap)
        .await
        .map_err(ServerFnError::new)
}

/// One file of a plan's workspace, as it stands, for the King to read and write
/// against.
///
/// The sibling of [`plan_diff`] and the answer to a different question: that one
/// shows what *changed*, and this shows what *is*. Most files in a project have
/// no diff at all, and the files tree offers all of them.
///
/// # The boundary
///
/// The fourth place in Kingdom where an outsider names a path and the server
/// opens it, held to the same rule as the other three: resolved through
/// [`within_workspace`], so `a/../../etc/passwd` is refused lexically before the
/// filesystem sees it, and what reaches [`crate::review::source`] is the
/// workspace-relative form the sandbox agreed to rather than the string as it
/// arrived.
///
/// Unlike [`crate::artifact`] it does **not** narrow by media type, and the
/// difference is deliberate: this is a source view for the King's own eyes, and
/// a project's files are exactly what it is for. What keeps it from being a
/// general file server is the sandbox and the size and binary guards in
/// `review::source`, not a list of extensions.
#[server(PlanSource, "/api")]
pub async fn plan_source(
    plan: String,
    path: String,
) -> Result<kingdom_core::SourceText, ServerFnError> {
    let plan_id = PlanId::new(plan);
    let workspace = {
        let kingdom = lock()?;
        let Some(plan) = kingdom.plan(&plan_id) else {
            return Err(ServerFnError::new("That plan is no longer in the records."));
        };
        grounded(&kingdom.root, &plan.workspace)
    };

    let inside = within_workspace(&workspace, &path).map_err(ServerFnError::new)?;

    Ok(crate::review::source(&workspace, &inside).await)
}

// -- The King's own edits -----------------------------------------------------
//
// Three functions rather than one, because reading, writing and deleting fail
// differently and a caller that cannot tell them apart cannot report them. The
// reasoning for all three -- why the text is fetched whole rather than rebuilt
// from the rendered lines, and what the stamp is defending against -- is in
// [`crate::edit`], which is where it stays true if these move.

/// One file of a plan's workspace, whole and byte-exact, for the King to edit.
///
/// The sibling of [`plan_source`], and deliberately not the same call: that one
/// answers "what does this file look like?" with numbered, truncatable lines,
/// and this answers "what is in this file?" with the bytes. Saving a buffer
/// rebuilt from the first would rewrite every CRLF file as LF -- see
/// [`kingdom_core::FileText`].
///
/// Held to the same wall as its four siblings: the path goes through
/// [`within_workspace`] before anything opens it.
#[server(PlanFileText, "/api")]
pub async fn plan_file_text(
    plan: String,
    path: String,
) -> Result<kingdom_core::FileText, ServerFnError> {
    let plan_id = PlanId::new(plan);
    let workspace = {
        let kingdom = lock()?;
        let Some(plan) = kingdom.plan(&plan_id) else {
            return Err(ServerFnError::new("That plan is no longer in the records."));
        };
        grounded(&kingdom.root, &plan.workspace)
    };

    let inside = within_workspace(&workspace, &path).map_err(ServerFnError::new)?;

    Ok(crate::edit::text(&workspace, &inside).await)
}

/// The King saves an edit of his own.
///
/// # Why this does not consult [`kingdom_core::Permissions`]
///
/// Permissions bound what the **court** may do -- `Propose` withholds an
/// unrestricted `patch` precisely so a model cannot change the project before
/// its plan is accepted. None of that is about the King, who owns the workspace
/// and may edit a file in it whenever he likes. Gating this on the plan's
/// permissions would mean the man reviewing a proposal cannot fix the typo he
/// just found in it.
///
/// What it *does* refuse is a **settled** plan, exactly as [`annotate_file`]
/// does and for its reason: the worktree has been disposed of, so there is no
/// file there to write.
///
/// A successful save appends a [`kingdom_core::NoteKind::Workspace`] note. Two
/// things follow from that, both wanted: the King can see in the transcript that
/// he edited a file himself, in order among the court's deeds, and the note
/// lengthens the transcript -- which is the change signal the review drawer and
/// the source panel already refetch on, so his own edit refreshes the drawer's
/// counts by the same route the court's edits do.
///
/// The **model** is not told. Notes are excluded from `Plan::turns` by design,
/// and the court finds out the honest way: `tools/patch.rs` reads a file fresh
/// on every call and refuses an anchor that is no longer there, which is a
/// louder signal than a sentence it might have skimmed.
#[server(PlanWriteFile, "/api")]
pub async fn plan_write_file(
    plan: String,
    path: String,
    content: String,
    stamp: kingdom_core::FileStamp,
) -> Result<kingdom_core::FileText, ServerFnError> {
    let plan_id = PlanId::new(plan);
    let workspace = writable_workspace(&plan_id)?;
    let inside = within_workspace(&workspace, &path).map_err(ServerFnError::new)?;

    let written = crate::edit::write(&workspace, &inside, &content, stamp)
        .await
        .map_err(ServerFnError::new)?;

    // After the write, not before: a note saying a file was saved must not
    // stand in front of a save that failed.
    let mut kingdom = lock()?;
    update(&mut kingdom, &plan_id, |p| {
        p.note(
            kingdom_core::NoteKind::Workspace,
            format!("You edited {inside} yourself."),
        );
    });

    Ok(written)
}

/// The King deletes a file himself.
///
/// The sibling of [`plan_write_file`] in every respect -- the same settled-plan
/// guard, the same stamp, the same note -- and it is separate because it fails
/// differently and because the panel does a different thing afterwards: there is
/// no file left to show, so it closes.
#[server(PlanDeleteFile, "/api")]
pub async fn plan_delete_file(
    plan: String,
    path: String,
    stamp: kingdom_core::FileStamp,
) -> Result<Plan, ServerFnError> {
    let plan_id = PlanId::new(plan);
    let workspace = writable_workspace(&plan_id)?;
    let inside = within_workspace(&workspace, &path).map_err(ServerFnError::new)?;

    crate::edit::remove(&workspace, &inside, stamp)
        .await
        .map_err(ServerFnError::new)?;

    let mut kingdom = lock()?;
    update(&mut kingdom, &plan_id, |p| {
        p.note(
            kingdom_core::NoteKind::Workspace,
            format!("You deleted {inside} yourself."),
        );
    })
    .ok_or_else(|| ServerFnError::new("That plan is no longer in the records."))
}

/// The workspace of a plan the King may still change files in.
///
/// Shared by the write and the delete so the refusal is worded once. A settled
/// plan's worktree has been removed from disk, so the honest answer is not "that
/// failed" but "there is nothing there any more" -- the same distinction
/// [`send_file_notes`] draws.
#[cfg(feature = "ssr")]
fn writable_workspace(plan_id: &PlanId) -> Result<kingdom_core::Workspace, ServerFnError> {
    let kingdom = lock()?;
    let Some(plan) = kingdom.plan(plan_id) else {
        return Err(ServerFnError::new("That plan is no longer in the records."));
    };

    if plan.status.is_settled() {
        return Err(ServerFnError::new(format!(
            "That plan is {} and its workspace has been cleared, so there is no \
             longer a file here to change.",
            plan.status.label().to_lowercase()
        )));
    }

    Ok(grounded(&kingdom.root, &plan.workspace))
}

/// Puts a workspace on the actual disk, when its path does not already say
/// where it is.
///
/// [`kingdom_core::Workspace::path`] is documented as absolute, and every
/// workspace [`crate::worktree::prepare`] builds is -- but the *placeholder*
/// plans a kingdom opens with are made by `sample::starter_plans`, which has
/// only `City::path`, and that is relative to the kingdom root. So a starter
/// plan claims to work in `almanac` rather than in `/…/realms/…/almanac`, and
/// anything that opens the directory finds nothing there.
///
/// Resolved here rather than in `review.rs` because the kingdom root is what
/// closes the gap and this is the layer that holds it. It is a no-op for every
/// real plan, since an absolute path is already grounded.
#[cfg(feature = "ssr")]
fn grounded(root: &str, workspace: &kingdom_core::Workspace) -> kingdom_core::Workspace {
    if std::path::Path::new(&workspace.path).is_absolute() {
        return workspace.clone();
    }

    let mut grounded = workspace.clone();
    grounded.path = std::path::Path::new(root)
        .join(&workspace.path)
        .to_string_lossy()
        .into_owned();
    grounded
}

/// Turns a path from the browser into one relative to the plan's workspace, or
/// refuses it.
///
/// Split out from [`plan_diff`] so the refusal can be tested without a kingdom
/// in global state -- the same split, for the same reason, as [`read_directory`]
/// and `artifact::from_workspace`.
#[cfg(feature = "ssr")]
fn within_workspace(
    workspace: &kingdom_core::Workspace,
    requested: &str,
) -> Result<String, String> {
    let shop = crate::tools::Sandbox::new(workspace.clone());
    let resolved = shop
        .resolve(requested)
        .map_err(|_| format!("{requested} is outside this plan's workspace."))?;

    shop.relative(&resolved)
        .filter(|inside| !inside.is_empty())
        .ok_or_else(|| format!("{requested} is outside this plan's workspace."))
}

/// Records another prompt on an existing plan, without drafting a reply.
///
/// Paired with [`draft_plan`] for the same reason as [`begin_plan`]: the user's
/// own words appear in the conversation the instant he sends them, rather than
/// only once the model has finished thinking.
#[server(Say, "/api")]
pub async fn say(plan: String, prompt: String) -> Result<Plan, ServerFnError> {
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

        // A subagent answers to the model that sent it, not to the user. Its
        // conversation renders no composer, so this is only reachable from a
        // stale tab or a hand-made request -- but the damage is real: the
        // parent is blocked on a tool call whose conversation would now have
        // two hands on it, and would be handed a report on a conversation that
        // changed underneath it.
        if existing.is_subagent() {
            return Err(ServerFnError::new(
                "This is an errand the court sent, not a plan you decreed. \
                 Say what you want in the plan that sent it.",
            ));
        }
    }

    update(&mut kingdom, &plan_id, |p| {
        // The one question that matters here is whether a turn is *actually*
        // running -- not whether the plan says it is busy. See `turns` for why
        // those differ, and why answering with `is_busy()` would turn today's
        // recoverable wedge into a permanent one.
        //
        // Asked *inside* the closure, so the registry is read under the same
        // kingdom lock that `converse` deregisters under. Sampling it at the
        // call site would leave a window in which a turn ends between the
        // answer and the write, and the words would be queued for a turn that
        // has already gone.
        let running = crate::turns::is_running(&plan_id);
        receive(p, prompt, running);
    })
    .map(|p| to_browser_in(&kingdom, p))
    .ok_or_else(|| ServerFnError::new("That plan is no longer in the records."))
}

/// Puts the user's words either in the queue or straight into the log.
///
/// Split out from [`say`] so the branch can be tested without a live turn and
/// without the kingdom singleton: `turn_running` is the only thing the two
/// paths differ on, and it is the one fact the caller must establish.
#[cfg(feature = "ssr")]
pub(crate) fn receive(plan: &mut Plan, prompt: String, turn_running: bool) {
    use kingdom_core::{PlanStatus, Speaker};

    if turn_running {
        // Heard at the top of the court's next round, by `converse`.
        //
        // Deliberately not appended to the transcript here. The turn in flight
        // rebuilds its brief from the transcript on every pass, so writing
        // straight into it would splice the user's words between a tool call
        // and its result -- handing the model a conversation that never
        // happened, and doing it mid-deed rather than at a boundary.
        plan.queue(prompt);
        return;
    }

    // Anything a turn left queued when it ended goes in first, so the log can
    // never carry the user's words out of the order they said them. This is the
    // path words take when a turn failed while they were waiting: `converse`
    // deliberately does not drain on its failure exits, because looping a
    // queued message back into a provider that just errored would burn the
    // round budget against a broken model.
    plan.hear_queued();

    plan.status = PlanStatus::Drafting;
    // The busy mark is cleared, not merely overwritten by the status.
    //
    // `draft_plan` refuses to start a turn over a plan that `is_busy()`, and
    // `is_busy()` is exactly `working_on.is_some()`. So a mark left behind by a
    // turn that died without clearing it -- a panic, a dropped future -- makes
    // the plan unstartable, and before this line the sole cure was restarting
    // the server. `store::reconcile` repairs it too, but only on load.
    //
    // Safe because this branch is reached only when no turn is running: there
    // is nothing in flight for the clearing to interrupt, which is a stronger
    // guarantee than this line used to rest on.
    plan.working_on = None;
    plan.say(Speaker::User, prompt);
}

/// Withdraws words the user queued, before the court has heard them.
///
/// Racing the drain is expected rather than exceptional: the turn may reach a
/// round boundary while the request is in flight. Losing that race is reported
/// rather than swallowed -- the words are in the transcript by then, and
/// quietly doing nothing would leave the user believing they had been taken
/// back.
#[server(Unqueue, "/api")]
pub async fn unqueue(plan: String, queued_id: String) -> Result<Plan, ServerFnError> {
    let plan_id = PlanId::new(plan);
    let mut kingdom = lock()?;

    let mut withdrawn = false;
    let updated = update(&mut kingdom, &plan_id, |p| {
        withdrawn = p.unqueue(&queued_id);
    })
    .ok_or_else(|| ServerFnError::new("That plan is no longer in the records."))?;

    if !withdrawn {
        return Err(ServerFnError::new(
            "The court has already heard that. It is in the chamber's log now.",
        ));
    }

    Ok(to_browser_in(&kingdom, updated))
}

/// The user calls a halt on a turn that is running.
///
/// Two outcomes, and the difference is diagnosis rather than failure:
///
/// - A turn is genuinely running: it is signalled, and it cleans up after
///   itself. Deliberately *not* cleaned up from here -- the turn owns its own
///   writes to the plan, and a second writer racing it is how the in-flight
///   deed would end up settled twice, once with a real outcome and once as
///   stopped.
/// - Nothing is running, but the plan says it is busy: the mark has outlived
///   its turn, and this repairs it. So Stop is also the cure for a wedged plan,
///   which until now needed a server restart to reach `store::reconcile`.
#[server(StopPlan, "/api")]
pub async fn stop_plan(plan: String) -> Result<Plan, ServerFnError> {
    use kingdom_core::{NoteKind, PlanStatus};

    let plan_id = PlanId::new(plan);

    // The errands the court sent are stopped with it. Without this, "Stop"
    // leaves subagents running against a turn nobody is waiting for any more --
    // and the spend they carry on making is a large part of why the button
    // exists at all. Read before the parent is halted, because a subagent's own
    // turn is what keeps it in the registry.
    let subagents: Vec<PlanId> = {
        let kingdom = lock()?;
        kingdom
            .plans
            .iter()
            .filter(|p| {
                p.spawned_by
                    .as_ref()
                    .is_some_and(|sent| sent.parent == plan_id)
            })
            .map(|p| p.id.clone())
            .collect()
    };

    // A subagent with no turn behind it is wedged in exactly the way the
    // parent is repaired for below, and for the same reason -- most often the
    // server stopped while it was mid-round. Repairing it here too is what
    // stops "Stop" reporting success on a parent while leaving a child stuck
    // `Drafting` forever, which nothing else would ever come back to clear.
    let mut stale = Vec::new();
    for subagent in &subagents {
        if !crate::turns::halt(subagent) {
            stale.push(subagent.clone());
        }
    }
    if !stale.is_empty() {
        let mut kingdom = lock()?;
        for subagent in &stale {
            update(&mut kingdom, subagent, |p| {
                if p.is_busy() {
                    p.working_on = None;
                    p.status = kingdom_core::PlanStatus::AwaitingReview;
                }
            });
        }
    }

    if crate::turns::halt(&plan_id) {
        // The turn will publish its own stopped state in a moment. Returning
        // the plan as it stands keeps this caller honest about what it knows.
        return snapshot(&plan_id)
            .map(to_browser)
            .ok_or_else(|| ServerFnError::new("That plan is no longer in the records."));
    }

    let mut kingdom = lock()?;
    update(&mut kingdom, &plan_id, |p| {
        // Only worth saying anything if the plan claimed to be working. A Stop
        // that lands just after a turn finished on its own is a no-op, and
        // should not write a note about a halt that halted nothing.
        if !p.is_busy() {
            return;
        }
        p.working_on = None;
        p.status = PlanStatus::AwaitingReview;
        p.note(
            NoteKind::Stopped,
            "This plan was marked as working, but no turn was running -- most \
             likely the server stopped while it was mid-decree. It has been \
             set right. Say something to send the court round again.",
        );
    })
    .map(|p| to_browser_in(&kingdom, p))
    .ok_or_else(|| ServerFnError::new("That plan is no longer in the records."))
}

/// Draws up the plan: marks it busy, then takes turns with the model until it
/// has something to say.
///
/// A turn is no longer one call. The model may act -- read a file, run a
/// command -- and each act is recorded, run, and answered before the model is
/// asked again. The request stays open for the whole conversation, but nothing
/// waits on it: every step is published to the conversation as it happens,
/// which is what makes a five-minute turn watchable instead of a spinner.
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

        // Likewise a settled plan: the conversation mounts the same way for
        // history as for live work, and drafting against a workspace that has
        // been cleared from disk would brief the model on nothing.
        if existing.status.is_settled() {
            return Ok(existing);
        }

        // A subagent is driven by the call that sent it, which is already
        // running one of these loops over it. The conversation mounts
        // identically for a subagent, so without this, a user who opens one
        // while it works would start a second loop over the same plan.
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
    // prompt could restart. A detached task loses only this caller's view of
    // the result, never the clearing; and since every step is pushed to the
    // conversation, that view was never the only way to see it.
    let handle = tokio::spawn({
        let plan_id = plan_id.clone();
        async move {
            crate::turn::converse(
                plan_id,
                city_brief,
                workspace,
                city_name,
                choice,
                crate::turn::MOST_ROUNDS,
            )
            .await
        }
    });

    // A panic inside the turn is the other way the busy mark outlives the task
    // that set it. `converse` clears it on every path it controls, but it
    // cannot clear it on a path that unwound -- and the plan left behind is the
    // wedged one described on `say`. Clearing it here means the failure is
    // visible and the plan is restartable, rather than needing a server restart
    // to reach `store::reconcile`.
    match handle.await {
        Ok(result) => result.map(to_browser),
        Err(e) => {
            let mut kingdom = lock()?;
            let repaired = update(&mut kingdom, &plan_id, |p| {
                p.working_on = None;
                p.status = kingdom_core::PlanStatus::Failed;
                p.summary = "The turn stopped unexpectedly.".to_string();
                p.note(
                    NoteKind::Failed,
                    "This plan's turn stopped unexpectedly. Anything it had already done is \
                     still in its workspace. Say something to set it going again.",
                );
            });
            repaired
                .map(|p| to_browser_in(&kingdom, p))
                .ok_or_else(|| ServerFnError::new(format!("drafting task failed: {e}")))
        }
    }
}

/// Carries the user's answer to a question the model is waiting on.
///
/// Deliberately a `#[server]` function rather than a message on the watch
/// socket. The socket exists for what HTTP cannot do -- let the server speak
/// first -- and this direction is an ordinary request the browser initiates.
/// Sending it over the socket would mean hand-rolling a request/response
/// protocol with no type checking across it, throwing away the main reason this
/// project is Rust on both ends.
///
/// Returns the plan, so the caller sees the same state everything else does.
/// The tool call is settled by the turn loop when the parked call resumes, not
/// here: this only unblocks it. Recording the outcome in two places is how a
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

    snapshot(&plan_id)
        .map(to_browser)
        .ok_or_else(|| ServerFnError::new("That plan is no longer in the records."))
}

/// The King accepts the standing proposal, and the court gains its hands.
///
/// The one place a plan's authority widens, and the only thing in Kingdom the
/// King changes on a plan directly. Everything else about a plan is written by
/// the court or by Kingdom itself.
///
/// Makes **no model call**, exactly as [`begin_plan`] makes none: it grants and
/// returns, and the chamber then dispatches [`draft_plan`] the same way it does
/// after [`say`]. Splitting them is what lets the King see the grant land
/// immediately rather than watching a spinner for the first round of work.
#[server(ApprovePlan, "/api")]
pub async fn approve_plan(plan: String) -> Result<Plan, ServerFnError> {
    use kingdom_core::{NoteKind, PlanStatus, Speaker};

    let plan_id = PlanId::new(plan);
    let mut kingdom = lock()?;
    let root = std::path::PathBuf::from(&kingdom.root);

    // Checked before the grant rather than inside it, so the refusal can say
    // *why*. A stale tab is the ordinary case here: the King left a proposal
    // open, revised it in another tab, and came back to press a button that no
    // longer refers to anything.
    match kingdom.plan(&plan_id) {
        None => return Err(ServerFnError::new("That plan is no longer in the records.")),
        Some(existing) if existing.standing_proposal().is_none() => {
            return Err(ServerFnError::new(
                "There is no plan awaiting your word here. It may have been accepted in \
                 another tab, or the court may have revised it since.",
            ))
        }
        Some(_) => {}
    }

    update(&mut kingdom, &plan_id, |p| {
        if !p.approve() {
            return;
        }

        // Authority changing is something that *happened*, not something anyone
        // said, so it is a note. The King must be able to see the moment his
        // agent gained the ability to change his files -- and see it in the log
        // rather than only in a header that reflects the present.
        //
        // The plan is filed in the same breath, at the one moment it is
        // unambiguously true. Doing it inside this closure means there is no
        // path that widens the permissions without also filing the document.
        //
        // The timing matters beyond tidiness: from the next line onward the
        // court holds an unrestricted `patch` and could rewrite its own draft.
        // Filing here is what puts the text the King actually read safely on
        // disk before that becomes possible -- and `file_plan` is write-once,
        // so the copy made now is the copy that survives.
        match crate::tools::propose_plan::draft_body(&p.workspace)
            .ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    "this plan's draft is missing or empty",
                )
            })
            .and_then(|body| crate::store::file_plan(&root, p, &body))
        {
            Ok(path) => {
                p.note(
                    NoteKind::Workspace,
                    format!(
                        "Approved. The court may now change the project. \
                         Filed at {}.",
                        path.display()
                    ),
                );
            }
            // The approval stands: the King said yes and the permissions
            // widened. Refusing that over a bookkeeping failure would be the
            // worse outcome, so this reports the loss the way `remember` does.
            // Finishing the plan tries again, so a draft that is merely
            // unreadable right now is not necessarily lost for good.
            Err(e) => {
                p.note(
                    NoteKind::Workspace,
                    "Approved. The court may now change the project.",
                );
                p.note(
                    NoteKind::Failed,
                    format!(
                        "Could not file this plan's document: {e}. The plan is \
                         approved regardless; only the filed copy was lost."
                    ),
                );
            }
        }

        // The grant reaches the model as an ordinary King turn. It could have
        // been a new kind of message with its own handling on the wire; making
        // it something the King said needs no provider to know anything, and is
        // also simply true.
        p.say(Speaker::User, kingdom_core::APPROVAL);
        p.status = PlanStatus::Drafting;
    })
    .map(|p| to_browser_in(&kingdom, p))
    .ok_or_else(|| ServerFnError::new("That plan vanished as it was being approved."))
}

/// The King sets aside a proposal without accepting it.
///
/// Deliberately **not** a terminal state. The plan stays where it was, with its
/// composer live, so he can say what he actually wants instead. Archiving is
/// how a plan ends, and it is reached the way it always is -- conflating "not
/// this plan" with "not this work" would make the dismissive click the
/// destructive one.
#[server(SetAsidePlan, "/api")]
pub async fn set_aside_plan(plan: String) -> Result<Plan, ServerFnError> {
    let plan_id = PlanId::new(plan);
    let mut kingdom = lock()?;

    update(&mut kingdom, &plan_id, |p| p.set_aside_proposal())
        .map(|p| to_browser_in(&kingdom, p))
        .ok_or_else(|| ServerFnError::new("That plan is no longer in the records."))
}

/// The King writes a note against one part of a standing proposal.
///
/// Recorded on the plan rather than held in the browser, for the same reason
/// queued words are: a note typed and not sent must survive a reload, a second
/// tab and a server restart. Going through [`update`] means it is stored and
/// pushed to every watcher like everything else, so a note written in one tab
/// appears in the other.
///
/// `line` and `quote` both travel because they answer different questions.
/// The line puts the note beside the right block while the card is open; the
/// quote is what is actually put to the model, and is the half that cannot go
/// stale -- see [`kingdom_core::ProposalNote::quote`].
#[server(AnnotateProposal, "/api")]
pub async fn annotate_proposal(
    plan: String,
    line: usize,
    quote: String,
    note: String,
) -> Result<Plan, ServerFnError> {
    let note = note.trim().to_string();
    if note.is_empty() {
        return Err(ServerFnError::new("An empty note says nothing."));
    }

    let plan_id = PlanId::new(plan);
    let mut kingdom = lock()?;

    let mut written = false;
    let updated = update(&mut kingdom, &plan_id, |p| {
        written = p.annotate(line, quote, note).is_some();
    })
    .ok_or_else(|| ServerFnError::new("That plan is no longer in the records."))?;

    // The ordinary case here is a stale tab: the King left a card open and the
    // court revised it, or he accepted it elsewhere. Reported rather than
    // swallowed, because a note silently written onto nothing is one he
    // believes he has made.
    if !written {
        return Err(ServerFnError::new(
            "There is no plan awaiting your word here. The court may have revised it \
             since, or it may have been accepted in another tab.",
        ));
    }

    Ok(to_browser_in(&kingdom, updated))
}

/// The King removes one entry from a plan's transcript -- a message, or a
/// settled tool call -- rather than living with something in the record he
/// does not want read back to the model.
///
/// `index` is a position in [`Plan::transcript`] as the browser last drew it.
/// That is honest rather than fragile: the transcript only ever grows or has
/// entries removed from it by this very call, so a stale index either still
/// names the entry the King meant, or [`kingdom_core::Plan::delete_entry`]
/// refuses it outright -- there is no third case where it silently names
/// something else. See that method for what else it refuses and why.
#[server(DeleteEntry, "/api")]
pub async fn delete_entry(plan: String, index: usize) -> Result<Plan, ServerFnError> {
    let plan_id = PlanId::new(plan);
    let mut kingdom = lock()?;

    // A settled plan is history and its workspace is gone; there is no reason
    // to prune a conversation nothing will ever read again, and refusing here
    // matches every other edit a settled plan already turns away.
    if let Some(existing) = kingdom.plan(&plan_id) {
        if existing.status.is_settled() {
            return Err(ServerFnError::new(format!(
                "That plan is {} and its record is closed.",
                existing.status.label().to_lowercase()
            )));
        }
    }

    let mut outcome = Ok(());
    let updated = update(&mut kingdom, &plan_id, |p| {
        outcome = p.delete_entry(index);
    })
    .ok_or_else(|| ServerFnError::new("That plan is no longer in the records."))?;

    outcome.map_err(ServerFnError::new)?;
    Ok(to_browser_in(&kingdom, updated))
}

/// The King takes a note back before the court has been told of it.
///
/// The sibling of [`unqueue`], and it loses its race the same way: the notes may
/// have been sent while this request was in flight. Saying so is the point --
/// quietly doing nothing would leave him believing he had withdrawn something
/// the model is already reading.
#[server(WithdrawNote, "/api")]
pub async fn withdraw_note(plan: String, note_id: String) -> Result<Plan, ServerFnError> {
    let plan_id = PlanId::new(plan);
    let mut kingdom = lock()?;

    let mut withdrawn = false;
    let updated = update(&mut kingdom, &plan_id, |p| {
        withdrawn = p.unannotate(&note_id);
    })
    .ok_or_else(|| ServerFnError::new("That plan is no longer in the records."))?;

    if !withdrawn {
        return Err(ServerFnError::new(
            "That note is no longer in the margin. It may already have gone to the court.",
        ));
    }

    Ok(to_browser_in(&kingdom, updated))
}

/// The King sends his notes back for the court to answer.
///
/// The margin is drained and becomes **one** [`kingdom_core::Speaker::User`]
/// turn, composed by [`notes_as_decree`]. One turn rather than one per note
/// because they are one review: a model handed five separate messages would
/// answer the last one and treat the rest as history.
///
/// Deliberately reuses [`receive`], which [`say`] already splits out for exactly
/// this decision -- whether a turn is genuinely running, and therefore whether
/// the words go into the queue or straight into the log. Notes sent into a
/// working chamber are heard at the next round boundary with no second branch to
/// get wrong, and a plan wedged by a stale busy mark is un-wedged by the same
/// line that un-wedges it for `say`.
///
/// Makes no model call. The chamber dispatches `draft_plan` afterwards, exactly
/// as it does after speaking -- which is what lets the King watch his notes land
/// rather than watching a spinner for the first round of work.
#[server(SendNotes, "/api")]
pub async fn send_notes(plan: String) -> Result<Plan, ServerFnError> {
    let plan_id = PlanId::new(plan);
    let mut kingdom = lock()?;

    // Checked before the drain rather than inside it, so a refusal can say why
    // and the notes are still there to try again with.
    match kingdom.plan(&plan_id) {
        None => return Err(ServerFnError::new("That plan is no longer in the records.")),
        Some(existing) if existing.notes().is_empty() => {
            return Err(ServerFnError::new(
                "There are no notes to send. Write against a part of the plan first.",
            ))
        }
        Some(_) => {}
    }

    update(&mut kingdom, &plan_id, |p| {
        let notes = p.take_notes();
        if notes.is_empty() {
            return;
        }
        // Asked inside the closure, under the same lock `converse` deregisters
        // under -- see `say` for why sampling it at the call site would leave a
        // window where words are queued for a turn that has already gone.
        let running = crate::turns::is_running(&plan_id);
        receive(p, notes_as_decree(&notes), running);
    })
    .map(|p| to_browser_in(&kingdom, p))
    .ok_or_else(|| ServerFnError::new("That plan vanished as its notes were sent."))
}

/// The King's marginal notes as one thing he said.
///
/// Ordinary prose in his own voice, blockquoting the part each note is against.
/// No new message kind and nothing for a provider to learn -- the same call
/// [`kingdom_core::APPROVAL`] makes, and true in the same way: he did write
/// these.
///
/// The quote is what makes a note answerable. "This is wrong" against nothing is
/// an objection the model has to guess the target of; against the paragraph it
/// is about, it is an instruction.
///
/// Split out and tested because the shape is the whole payload: a decree that
/// separated a note from its quote would be read by the model as two unrelated
/// remarks.
#[cfg(feature = "ssr")]
fn notes_as_decree(notes: &[kingdom_core::ProposalNote]) -> String {
    let mut decree = String::new();
    decree.push_str(match notes.len() {
        1 => {
            "I have read the plan and written a note against part of it. \
             Revise the draft to answer it, then propose again.\n"
        }
        _ => {
            "I have read the plan and written notes against parts of it. \
             Revise the draft to answer them, then propose again.\n"
        }
    });

    for note in notes {
        decree.push('\n');
        // Every line of the quote is prefixed, not just the first: a wrapped
        // paragraph quoted with one `>` reads as a quote that ends after its
        // first line, with the rest apparently the King speaking.
        for line in note.quote.lines() {
            decree.push_str("> ");
            decree.push_str(line);
            decree.push('\n');
        }
        decree.push('\n');
        decree.push_str(note.body.trim());
        decree.push('\n');
    }

    decree
}

/// The King writes a note against one line of one file.
///
/// The sibling of [`annotate_proposal`], and it takes the same path for the same
/// reasons: through [`update`], so the note is stored and pushed to every
/// watcher like everything else, which is what makes a note written in one tab
/// appear in the other.
///
/// `line`, `side` and `quote` all travel because they answer different
/// questions. The line and the side put the note beside the right row while the
/// panel is open; the quote is what is actually put to the model, and is the
/// half that cannot go stale -- see [`kingdom_core::ReviewNote::quote`], which
/// matters more here than it does for a proposal because the court may be
/// rewriting the file while the note is being typed.
#[server(AnnotateFile, "/api")]
pub async fn annotate_file(
    plan: String,
    path: String,
    line: u32,
    side: kingdom_core::NoteSide,
    quote: String,
    note: String,
) -> Result<Plan, ServerFnError> {
    let note = note.trim().to_string();
    if note.is_empty() {
        return Err(ServerFnError::new("An empty note says nothing."));
    }

    let plan_id = PlanId::new(plan);
    let mut kingdom = lock()?;

    let mut written = false;
    let updated = update(&mut kingdom, &plan_id, |p| {
        written = p.annotate_file(path, line, side, quote, note).is_some();
    })
    .ok_or_else(|| ServerFnError::new("That plan is no longer in the records."))?;

    // The only way to fail here is a plan that has been settled since the panel
    // was opened -- its workspace is gone, so the file the note names is gone
    // too. Reported rather than swallowed, because a note silently written onto
    // nothing is one the King believes he has made.
    if !written {
        return Err(ServerFnError::new(
            "This plan has been closed and its workspace cleared, so there is no \
             longer a file here to write against.",
        ));
    }

    Ok(updated)
}

/// The King takes a line note back before the court has been told of it.
///
/// The sibling of [`withdraw_note`], and it loses its race the same way: the
/// review may have been sent while this request was in flight. Saying so is the
/// point -- quietly doing nothing would leave him believing he had withdrawn
/// something the model is already reading.
#[server(WithdrawFileNote, "/api")]
pub async fn withdraw_file_note(plan: String, note_id: String) -> Result<Plan, ServerFnError> {
    let plan_id = PlanId::new(plan);
    let mut kingdom = lock()?;

    let mut withdrawn = false;
    let updated = update(&mut kingdom, &plan_id, |p| {
        withdrawn = p.unannotate_file(&note_id);
    })
    .ok_or_else(|| ServerFnError::new("That plan is no longer in the records."))?;

    if !withdrawn {
        return Err(ServerFnError::new(
            "That note is no longer in the review. It may already have gone to the court.",
        ));
    }

    Ok(updated)
}

/// The King sends his line notes to the court as one review.
///
/// The sibling of [`send_notes`] and identical in shape, which is the point: the
/// review is drained and becomes **one** [`kingdom_core::Speaker::User`] turn,
/// composed by [`file_notes_as_decree`]. One turn rather than one per note
/// because they are one review -- a model handed nine separate messages would
/// answer the last and treat the rest as history.
///
/// Deliberately reuses [`receive`], the branch [`say`] already splits out.
/// Notes sent into a working chamber queue and are heard at the next round
/// boundary with no second code path to get wrong, and a plan wedged by a stale
/// busy mark is un-wedged by the same line that un-wedges it for `say`.
///
/// Makes no model call. The chamber dispatches `draft_plan` afterwards, exactly
/// as it does after speaking -- which is what lets the King watch his review
/// land rather than watching a spinner for the first round of work.
#[server(SendFileNotes, "/api")]
pub async fn send_file_notes(plan: String) -> Result<Plan, ServerFnError> {
    let plan_id = PlanId::new(plan);
    let mut kingdom = lock()?;

    // Checked before the drain rather than inside it, so a refusal can say why
    // and the notes are still there to try again with.
    match kingdom.plan(&plan_id) {
        None => return Err(ServerFnError::new("That plan is no longer in the records.")),
        Some(existing) if existing.review_notes().is_empty() => {
            return Err(ServerFnError::new(
                "There is nothing to send. Write against a line of a file first.",
            ))
        }
        // A settled plan's workspace is gone, so there is nothing left to change
        // in answer to the review -- the same guard `say` keeps, for the same
        // reason. The notes are deliberately left standing rather than drained
        // into a refusal.
        Some(existing) if existing.status.is_settled() => {
            return Err(ServerFnError::new(format!(
                "That plan is {} and its workspace has been cleared, so there is \
                 nothing left to change. Start a new decree to carry the work on.",
                existing.status.label().to_lowercase()
            )))
        }
        Some(_) => {}
    }

    update(&mut kingdom, &plan_id, |p| {
        let notes = p.take_review_notes();
        if notes.is_empty() {
            return;
        }
        // Asked inside the closure, under the same lock `converse` deregisters
        // under -- see `say` for why sampling it at the call site would leave a
        // window where words are queued for a turn that has already gone.
        let running = crate::turns::is_running(&plan_id);
        receive(p, file_notes_as_decree(&notes), running);
    })
    .ok_or_else(|| ServerFnError::new("That plan vanished as its review was sent."))
}

/// The King's line notes as one thing he said.
///
/// Ordinary prose in his own voice, grouped by file and ordered by line, each
/// note quoting the line it answers. No new message kind and nothing for a
/// provider to learn -- the same call [`notes_as_decree`] makes, and true in the
/// same way: he did write these.
///
/// **Grouped by file rather than left in the order they were written.** A review
/// is read as a set of changes to make, and a model given nine notes shuffled
/// across four files has to sort them before it can start. Ordering within a
/// file is by line, so the model reads a file the way it will edit it.
///
/// A [`kingdom_core::NoteSide::Base`] note says in words which version it is
/// about. A note on a deleted line is an ordinary review comment -- "why did this
/// go?" -- and a bare line number would point the court at whatever now occupies
/// that position.
///
/// Split out and tested because the shape is the whole payload: a decree that
/// separated a note from its line would be read as an objection with no target.
#[cfg(feature = "ssr")]
fn file_notes_as_decree(notes: &[kingdom_core::ReviewNote]) -> String {
    use kingdom_core::NoteSide;

    let mut decree = String::new();
    decree.push_str(match notes.len() {
        1 => {
            "I have read the code and written a note against one line. \
             Make the change it asks for.\n"
        }
        _ => {
            "I have read the code and written notes against some lines. \
             Make the changes they ask for.\n"
        }
    });

    // Grouped without sorting the caller's slice: files appear in the order the
    // King first wrote against them, which is the order he was reading in, and
    // within a file the notes are ordered by line.
    let mut files: Vec<&str> = Vec::new();
    for note in notes {
        if !files.contains(&note.path.as_str()) {
            files.push(&note.path);
        }
    }

    for path in files {
        decree.push_str("\n## ");
        decree.push_str(path);
        decree.push('\n');

        let mut on_this_file: Vec<&kingdom_core::ReviewNote> =
            notes.iter().filter(|n| n.path == path).collect();
        on_this_file.sort_by_key(|n| n.line);

        for note in on_this_file {
            decree.push('\n');
            decree.push_str(&match note.side {
                NoteSide::Working => format!("Line {}:\n", note.line),
                NoteSide::Base => {
                    format!("Line {}, in the version before your changes:\n", note.line)
                }
            });
            // Every line of the quote is prefixed, not just the first: a quote
            // whose second line is unprefixed reads as the quote ending and the
            // King speaking, which is the model's cue to act on it. The same
            // rule `notes_as_decree` is tested for.
            for line in note.quote.lines() {
                decree.push_str("> ");
                decree.push_str(line);
                decree.push('\n');
            }
            // A line that is empty, or all whitespace, has no lines to iterate,
            // so the quote would vanish and the note would read as being about
            // nothing. Said instead.
            if note.quote.lines().next().is_none() {
                decree.push_str("> (blank line)\n");
            }
            decree.push('\n');
            decree.push_str(note.body.trim());
            decree.push('\n');
        }
    }

    decree
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

        // A subagent's workspace is a *clone of its parent's* -- same path,
        // same branch, same id -- because a subagent works alongside the plan
        // that sent it rather than in a checkout of its own.
        //
        // So finishing one would merge the parent's half-finished work and then
        // delete the worktree out from under a plan still running in it. The
        // conversation never offers the button, but the blast radius here is
        // the user's actual work, so the guard is here rather than in the UI.
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

    // Read before the disposal, because the disposal is what destroys it:
    // `worktree remove --force` takes `.kingdom/draft.md` with the checkout, and
    // the draft was never committed to be recoverable from the branch. This is
    // the last moment the plan's own document exists.
    //
    // Read unconditionally, even for a merge that git may refuse. Holding a few
    // kilobytes that turn out not to be needed costs nothing; discovering after
    // a successful teardown that the file should have been read costs the
    // document.
    let draft = crate::tools::propose_plan::draft_body(&workspace);

    // The King's own shell in this plan goes first of all. It has the worktree
    // as its working directory, so `git worktree remove` below would be
    // fighting a live process; and a shell in a namespace about to be torn down
    // is a shell into nowhere. Nothing happens for a plan he never opened one
    // in.
    crate::terminal::shutdown(&plan_id);

    // The plan's network goes before its worktree does. Ordered deliberately:
    // the namespace holds whatever the agent left running -- a dev server, a
    // watcher -- and those processes have the worktree as their working
    // directory. Killing them first means `git worktree remove` is not fighting
    // a process still writing into the directory it is trying to delete.
    //
    // Unconditional, like the draft read above: a plan on the shared network
    // has no namespace and this does nothing at all.
    crate::netns::shutdown(&plan_id);

    // The city's shared services are **not** touched here. They are reconciled
    // once the plan has actually been settled below -- see the call after
    // `update`. Letting go here would be wrong twice over: the plan is still
    // live at this point, so a reconcile would immediately re-enrol it; and a
    // merge git refuses leaves the plan in play, still needing its database.

    let finish = match how {
        Disposition::Merge => crate::worktree::merge(&city_root, &workspace).await,
        Disposition::Archive => {
            let patch = crate::store::archive_patch(&root, &plan_id);
            crate::worktree::archive(&city_root, &workspace, &patch).await
        }
    }
    .map_err(|e| ServerFnError::new(e.to_string()))?;

    // Only once the work has actually landed. A refused merge leaves the plan
    // in play, and killing the model's dev server under a plan the user is
    // about to retry would take away the thing he needs to see to fix it.
    //
    // The browser goes with it, for the same reason and on the same terms: a
    // settled plan's Chrome is holding nine processes and most of a gigabyte on
    // behalf of work that is over.
    if matches!(finish, Finish::Settled(_)) {
        crate::tools::tmux::dismiss(&plan_id).await;
        crate::tools::browser::dismiss(&plan_id).await;
    }

    let mut kingdom = lock()?;
    let mut filed = false;
    let plan = update(&mut kingdom, &plan_id, |p| match finish {
        Finish::Settled(outcome) => {
            // The plan's document outlives the checkout it was written in.
            // Usually this is a no-op -- `file_plan` is write-once and an
            // approved plan was filed at the moment of the grant -- but a plan
            // archived while still awaiting review, or set aside and then
            // archived, was never approved and is filed here for the first and
            // only time. That is the case this exists for.
            //
            // A plan that never drafted files nothing, and that is not a
            // failure: it is what an abandoned plan looks like.
            if let Some(body) = &draft {
                match crate::store::file_plan(&root, p, body) {
                    Ok(path) => {
                        filed = true;
                        p.note(
                            NoteKind::Merge,
                            format!("Plan filed at {}.", path.display()),
                        );
                    }
                    // Reported rather than fatal, exactly as at approval: the
                    // work has already landed and cannot be un-landed over a
                    // bookkeeping failure.
                    Err(e) => p.note(
                        NoteKind::Failed,
                        format!("Could not file this plan's document: {e}."),
                    ),
                }
            }

            p.note(NoteKind::Merge, outcome.summary());
            p.settle(outcome);
        }
        // Nothing about the plan has changed, so nothing about its status
        // does either: it is still awaiting review, because it is.
        Finish::Refused(why) => p.note(NoteKind::Merge, why),
        // The same on disk, and a different kind on purpose. This is the one
        // refusal an agent can clear, and the chamber finds it by matching on
        // the kind -- see `NoteKind::MergeConflict`.
        Finish::Diverged(why) => p.note(NoteKind::MergeConflict, why),
    })
    .ok_or_else(|| ServerFnError::new("That plan vanished mid-decree."))?;

    // A plan working *in place* has no worktree to tear down, so nothing has
    // removed its draft -- it would be left sitting in the user's own project
    // folder. `discard_draft` is a no-op for an isolated plan, whose whole
    // checkout is already gone.
    //
    // Guarded on the filing having actually succeeded, because this deletes the
    // only remaining copy. If filing failed, the draft stays exactly where it
    // is and the note above says why -- a file the King has to tidy up himself
    // is a far better outcome than a plan deleted twice over.
    if filed {
        crate::tools::propose_plan::discard_draft(&workspace);
    }

    // Now that the plan is settled -- and only now -- the wells are reconciled
    // against who is left. A container is stopped only if *nobody* is drawing
    // from it: five plans on one project share one database, and four of them
    // finishing must not take it away from the fifth. Its named volume is kept
    // regardless, because the King's data is the whole reason it existed.
    //
    // After `update` rather than before, because that is what makes this plan
    // absent from the population. A merge git *refused* leaves the plan live
    // and its database standing, which is right: he is going to try again.
    reconcile_wells(&kingdom);

    Ok(to_browser_in(&kingdom, plan))
}

/// Every shared resource this kingdom knows about, and what each is doing.
///
/// The whole ledger in one call, host scope and every city at once. See
/// [`crate::services::inventory`] for why it is not per city and why it costs
/// one Docker question rather than one per row.
///
/// The kingdom's lock is taken and **released** before the await: `inventory`
/// shells out to `docker`, and holding a `std::sync::Mutex` across an await
/// point is the deadlock `city_root_in` already warns about, with a subprocess
/// on top of it.
#[server(SharedResources, "/api")]
pub async fn shared_resources() -> Result<kingdom_core::ResourceInventory, ServerFnError> {
    let kingdom = { lock()?.clone() };
    Ok(crate::services::inventory(&kingdom).await)
}

/// Declares a new shared resource, writing it to the manifest its scope keeps.
///
/// Returns the path written to, which is the thing the King needs next: the
/// file is the source of truth and editing it by hand is the supported way to
/// change or remove what is there.
///
/// # Why the city is a `CityId` and not a path
///
/// The browser cannot be trusted with a filesystem path -- that is the whole
/// reason the opening screen asks the King to type one rather than accepting
/// one from a page. A city id is resolved against the open kingdom here, so the
/// only paths this can write to are a city of the kingdom he opened, or his own
/// profile.
///
/// # Why `env` is a string
///
/// It arrives as the `KEY=value` text the King typed, and is parsed by
/// [`kingdom_core::services::parse_env`] here. Not cosmetic: a `Vec` of pairs
/// that happens to be **empty** does not survive a server function's argument
/// encoding at all, and a service with no environment is the ordinary case.
/// Measured against a running server, where the form failed with "missing
/// field `env`" for exactly that input.
#[server(DeclareSharedResource, "/api")]
pub async fn declare_shared_resource(
    scope: String,
    city: Option<String>,
    name: String,
    image: String,
    port: u16,
    env: String,
    volume: String,
) -> Result<String, ServerFnError> {
    use kingdom_core::services::ServiceScope;

    let Some(kind) = ServiceScope::from_wire(&scope) else {
        return Err(ServerFnError::new(format!(
            "`{scope}` is not a level a shared resource can run at."
        )));
    };

    let scope = match kind {
        ServiceScope::Host => crate::services::Scope::Host,
        ServiceScope::City => {
            let kingdom = lock()?;
            let Some(city) = city
                .map(kingdom_core::CityId::new)
                .and_then(|id| kingdom.city(&id).cloned())
            else {
                return Err(ServerFnError::new(
                    "A resource that belongs to one project needs a project. \
                     Pick one, or share it with the whole machine instead.",
                ));
            };
            crate::services::Scope::City(std::path::Path::new(&kingdom.root).join(&city.path))
        }
    };

    let spec = kingdom_core::ServiceSpec {
        name: name.trim().to_string(),
        image: image.trim().to_string(),
        port,
        env: kingdom_core::services::parse_env(&env),
        // An empty box is "no volume", which is a different declaration from a
        // volume named "" -- and the one the parser would refuse.
        volume: Some(volume.trim().to_string()).filter(|v| !v.is_empty()),
    };

    let path =
        crate::services::declare(&scope, &spec).map_err(|e| ServerFnError::new(e.to_string()))?;
    Ok(path.to_string_lossy().to_string())
}

/// Every model the user can choose between, and what each will accept.
///
/// Read live from each provider rather than hard-coded, so the picker cannot
/// offer a model that has been withdrawn or hide one that has just landed. It
/// also carries the credential state, which is why there is no separate status
/// call: "what can draft this?" and "will it work?" are one question.
#[server(ListModels, "/api")]
pub async fn list_models() -> Result<ModelCatalogue, ServerFnError> {
    Ok(crate::llm::catalogue::catalogue().await)
}

/// Ends the King's shell in a plan, deliberately.
///
/// The panel's close button, and nothing else. A shell now outlives its socket
/// (see [`crate::terminal`]), so the browser can no longer end one by
/// disconnecting -- which is the whole point, since navigating to a diff
/// disconnects too. Closing has to be said out loud.
///
/// Takes no lock and answers `()`: it neither reads nor changes the records,
/// and a plan that has no shell is not an error, it is the ordinary case.
#[server(EndTerminal, "/api")]
pub async fn end_terminal(plan: String) -> Result<(), ServerFnError> {
    crate::terminal::shutdown(&PlanId::new(plan));
    Ok(())
}

/// The system prompt a plan's model is given, rendered exactly as a turn would
/// send it.
///
/// Goes through the same [`crate::llm::SystemPrompt::assemble`] and `render`
/// that [`crate::turn::converse`] uses, with the plan's own workspace, permissions and
/// approval. A second rendering path would drift from the real one the first
/// time either was touched, and showing the user a prompt the model never
/// received is worse than showing nothing.
///
/// Derived on demand rather than stored on the plan: the text is a function of
/// the plan, the city on disk and every `AGENTS.md` found on the way up, so a
/// copy frozen into `plans/<id>.json` would grow every write and go stale.
///
/// **This is the prompt as it would be assembled *now*.** A plan approved since
/// its last turn shows the widened permissions -- the honest answer to what the
/// next round will be told, rather than what the first round read.
#[server(PlanBriefing, "/api")]
pub async fn plan_briefing(plan: String) -> Result<String, ServerFnError> {
    use crate::llm::{CityBrief, SystemPrompt};

    let plan_id = PlanId::new(plan);
    let kingdom = lock()?;

    let Some(plan) = kingdom.plan(&plan_id) else {
        return Err(ServerFnError::new("That plan is no longer in the records."));
    };
    let Some(city) = kingdom.city(&plan.city) else {
        return Err(ServerFnError::new("That plan's city is gone."));
    };

    // Briefed on the workspace the plan actually holds, exactly as `draft_plan`
    // does -- an isolated plan's prompt names files in its worktree, and a
    // viewer showing the city's checkout instead would be quietly wrong.
    let brief = CityBrief::from_city(city, &plan.workspace);
    let root = std::path::PathBuf::from(&kingdom.root);

    Ok(SystemPrompt::assemble(
        &plan_id,
        &brief,
        &plan.workspace,
        plan.permissions,
        plan.approved_proposal().is_some(),
        &root,
    )
    .render())
}

/// A suggested starting folder, so the user is not typing a path from scratch.
#[server(SuggestRoot, "/api")]
pub async fn suggest_root() -> Result<String, ServerFnError> {
    // The folder last opened, above any guess: if the King has told us once,
    // that answer beats probing for a folder called `dev`. Reached only when
    // the picker is showing at all -- boot reopens it without asking.
    if let Some(last) = crate::profile::last_kingdom() {
        if last.is_dir() {
            return Ok(last.to_string_lossy().to_string());
        }
    }

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
pub(crate) mod tests {
    use super::*;
    use kingdom_core::NetworkMode;
    use std::path::PathBuf;

    /// Containment must survive traversal, not merely prefix-matching.
    ///
    /// This is the wall that keeps a session working on Kingdom IDE from
    /// opening the user's real projects -- and, once plans have hands, the wall
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
        let fixture = sandbox.join("kingdom-mirror");
        let outside = base.join("real-work");
        std::fs::create_dir_all(&fixture).unwrap();
        std::fs::create_dir_all(&outside).unwrap();

        assert!(
            within_sandbox(&sandbox, &fixture).is_ok(),
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

    /// Only live, non-subagent plans are counted as holding a well.
    ///
    /// Both filters guard a real leak. A **settled** plan's worktree is gone,
    /// so a merged plan left in the count would pin a database open forever. A
    /// **subagent** is worse: `finish_plan` refuses to finish one, so nothing
    /// would ever take it back out of the count -- and it needs no claim of its
    /// own, because it works in its parent's worktree and reaches the well
    /// through the parent's.
    ///
    /// `Failed` deliberately counts as live, agreeing with `kingdom_network`
    /// and `kingdom_changes`: it can be retried and its workspace is still
    /// there.
    #[test]
    fn only_live_agents_are_counted_as_drawing_from_a_well() {
        use kingdom_core::{
            City, CityId, CityKind, ModelChoice, Outcome, PlanId, PlanStatus, SpawnedBy, Workspace,
        };

        let mut kingdom = Kingdom::unopened();
        kingdom.root = "/dev".to_string();
        kingdom.cities = vec![City {
            id: CityId::new("c1"),
            name: "testburg".into(),
            path: "testburg".into(),
            kind: CityKind::Rust,
            file_count: 1,
            has_git: true,
            dirty_files: 0,
            structure: None,
        }];

        let of = |id: &str| {
            Plan::opened(
                PlanId::new(id),
                CityId::new("c1"),
                "A decree",
                &ModelChoice::new("mock", None),
                Workspace::in_place("/dev/testburg"),
                NetworkMode::Shared,
            )
        };

        let working = of("plan-working");

        let mut failed = of("plan-failed");
        failed.status = PlanStatus::Failed;

        let mut merged = of("plan-merged");
        merged.settle(Outcome::Merged {
            commit: "0000000".into(),
            into: "main".into(),
        });

        let mut errand = of("plan-errand");
        errand.spawned_by = Some(SpawnedBy {
            parent: working.id.clone(),
            tool_call: "call-1".to_string(),
        });

        kingdom.plans = vec![working, failed, merged, errand];

        let drawing: Vec<String> = agents_drawing(&kingdom)
            .into_iter()
            .map(|(plan, _)| plan.to_string())
            .collect();

        assert_eq!(
            drawing,
            vec!["plan-working".to_string(), "plan-failed".to_string()],
            "a settled plan and a subagent must not hold a well open"
        );
    }

    /// Two agents in one city are two drawers of one well.
    ///
    /// The reference count has to be restored at its true depth, or the first
    /// plan to finish would stop a database the other four are still using.
    /// Both entries name the same city root, which is what `services::reconcile`
    /// groups on to raise the well exactly **once**.
    #[test]
    fn two_agents_in_one_city_both_hold_its_well() {
        use kingdom_core::{City, CityId, CityKind, ModelChoice, PlanId, Workspace};

        let mut kingdom = Kingdom::unopened();
        kingdom.root = "/dev".to_string();
        kingdom.cities = vec![City {
            id: CityId::new("c1"),
            name: "shopfront".into(),
            path: "shopfront".into(),
            kind: CityKind::Node,
            file_count: 1,
            has_git: true,
            dirty_files: 0,
            structure: None,
        }];
        kingdom.plans = ["plan-1", "plan-2"]
            .into_iter()
            .map(|id| {
                Plan::opened(
                    PlanId::new(id),
                    CityId::new("c1"),
                    "A decree",
                    &ModelChoice::new("mock", None),
                    Workspace::in_place("/dev/shopfront"),
                    NetworkMode::Shared,
                )
            })
            .collect();

        let drawing = agents_drawing(&kingdom);
        assert_eq!(drawing.len(), 2, "both agents are counted");

        let roots: std::collections::HashSet<_> =
            drawing.iter().map(|(_, root)| root.clone()).collect();
        assert_eq!(
            roots.len(),
            1,
            "one city, so one scope to raise however many agents are in it"
        );
        assert_eq!(
            roots.into_iter().next().unwrap(),
            PathBuf::from("/dev/shopfront")
        );
    }

    /// A starter plan's workspace is relative to the kingdom root, and
    /// everything that opens a workspace assumes it is absolute.
    ///
    /// The gap is `sample::starter_plans`, which builds a `Workspace` out of
    /// `City::path` -- documented as relative -- and hands it to a field
    /// documented as absolute. The review drawer was the first thing to open
    /// that directory and find nothing there. Pinned because the fix is one
    /// call at one boundary, and losing it makes every placeholder plan report
    /// its workspace as missing.
    #[test]
    fn a_relative_workspace_is_put_back_on_the_disk() {
        use kingdom_core::Workspace;

        let relative = Workspace::in_place("almanac");
        assert_eq!(
            grounded("/realms/kingdom-mirror", &relative).path,
            "/realms/kingdom-mirror/almanac"
        );

        // A real plan's workspace already says where it is, and must be left
        // exactly as it stands -- joining a root onto it would corrupt it.
        let absolute = Workspace::in_place("/dev/testburg/.kingdom/abc");
        assert_eq!(grounded("/dev", &absolute), absolute);
    }

    /// `kingdom_to_browser` fits every plan while already holding the guard
    /// its own caller (`get_kingdom`) took -- it must resolve each plan's city
    /// through the kingdom it was handed, not by taking the lock a second
    /// time. A caller that got this wrong would deadlock the server while
    /// still holding the lock, exactly as `events::publishing_while_the_
    /// kingdom_is_held_does_not_deadlock` exists to catch for the per-plan
    /// path.
    ///
    /// Run on its own thread with a deadline, so a reintroduced deadlock is a
    /// failing test rather than a suite that never finishes.
    #[test]
    fn fitting_a_whole_kingdom_while_it_is_held_does_not_deadlock() {
        let (done, finished) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let kingdom = lock().expect("the kingdom locks");
            let _ = kingdom_to_browser(&kingdom);
            let _ = done.send(());
        });

        assert!(
            finished
                .recv_timeout(std::time::Duration::from_secs(10))
                .is_ok(),
            "fitting a whole kingdom while its own lock is held must return -- a \
             second lock from inside it deadlocks the whole server"
        );
    }

    /// The review drawer's diff and the source view are the third and fourth
    /// places an outsider names a path and the server opens it, so both are held
    /// to the same wall as the first two.
    ///
    /// The King's own editing added three more -- `plan_file_text`,
    /// `plan_write_file` and `plan_delete_file` -- and they raise the stakes:
    /// the earlier four only *read* a file the King should not see, and these
    /// would overwrite or delete one. Every one of the seven calls
    /// `within_workspace` and none has a resolver of its own, which is the
    /// property this pins.
    ///
    /// The refusal is what matters: a path that walks out of the workspace must
    /// be turned away *before* git or the filesystem is reached, because
    /// `git show`, `std::fs::read` and `std::fs::remove_file` will each happily
    /// take a file the King never meant to name. Tested through the predicate
    /// rather than through the server functions, which need a scanned kingdom
    /// and the process-global lock to reach -- the same split the sandbox test
    /// above uses.
    #[test]
    fn a_file_cannot_be_asked_for_outside_the_workspace() {
        use kingdom_core::Workspace;

        let workspace = Workspace::in_place("/dev/testburg");

        assert_eq!(
            within_workspace(&workspace, "src/main.rs").unwrap(),
            "src/main.rs",
            "an ordinary path passes through as the workspace-relative form"
        );
        assert_eq!(
            within_workspace(&workspace, "/dev/testburg/src/main.rs").unwrap(),
            "src/main.rs",
            "an absolute path already inside is relativised rather than refused"
        );

        assert!(
            within_workspace(&workspace, "../../etc/passwd").is_err(),
            "a path that walks out with .. must be refused"
        );
        assert!(
            within_workspace(&workspace, "/etc/passwd").is_err(),
            "an absolute path outside must be refused"
        );
        assert!(
            within_workspace(&workspace, "").is_err(),
            "the workspace root is not a file to read or diff"
        );
        // Lexically inside, actually outside -- the case a `starts_with` on the
        // strings as typed would admit.
        assert!(
            within_workspace(&workspace, "src/../../etc/passwd").is_err(),
            "a path that walks out mid-way must be refused, not prefix-matched"
        );
    }

    /// Every path-taking route goes through the one predicate.
    ///
    /// The test above pins what the predicate *decides*; this pins that nothing
    /// decides it privately. It is a source check rather than a behavioural one
    /// because the failure it guards against is a future route resolving a path
    /// itself -- which no amount of exercising the existing routes would catch.
    /// The three writing routes are the ones worth being loud about: a read
    /// outside the workspace exposes a file, and a write outside it destroys
    /// one.
    #[test]
    fn every_route_that_opens_a_named_file_resolves_it_through_one_predicate() {
        let source = include_str!("api.rs");

        for route in [
            "pub async fn plan_diff(",
            "pub async fn plan_diff_context(",
            "pub async fn plan_source(",
            "pub async fn plan_file_text(",
            "pub async fn plan_write_file(",
            "pub async fn plan_delete_file(",
        ] {
            let start = source
                .find(route)
                .unwrap_or_else(|| panic!("{route} has been renamed; this test must follow it"));
            // To the end of that function: the next item at column zero.
            let body = &source[start..];
            let end = body.find("\n}\n").unwrap_or(body.len());
            assert!(
                body[..end].contains("within_workspace("),
                "{route} must resolve its path through within_workspace, not privately"
            );
        }
    }

    /// The prompt viewer must show what the model is actually given.
    ///
    /// Its whole value is diagnostic: a viewer rendering a second, similar
    /// prompt would answer "why did it do that?" with a text nothing was ever
    /// told. This pins the two facts that make it the real one -- it is built
    /// from the plan's own workspace, and it moves with the plan's permissions.
    ///
    /// One test rather than three because they share the process-global
    /// kingdom, which the server function reaches through `lock()`: separate
    /// tests would race each other for it.
    #[tokio::test]
    async fn the_prompt_shown_is_the_prompt_sent() {
        use kingdom_core::{City, CityId, CityKind, ModelChoice, PlanId, Workspace};

        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("kingdom-briefing-{unique}"));
        let city_path = root.join("testburg");
        std::fs::create_dir_all(&city_path).unwrap();

        let city = City {
            id: CityId::new("c1"),
            name: "testburg".into(),
            path: "testburg".into(),
            kind: CityKind::Rust,
            file_count: 1,
            has_git: false,
            dirty_files: 0,
            structure: None,
        };
        let mut plan = Plan::opened(
            PlanId::new("plan-briefing"),
            CityId::new("c1"),
            "Read the tests",
            &ModelChoice::new("mock", None),
            Workspace::in_place(city_path.to_string_lossy()),
            NetworkMode::Shared,
        );
        plan.permissions = kingdom_core::Permissions::Propose;

        {
            let mut kingdom = lock().unwrap();
            kingdom.name = "proving".into();
            kingdom.root = root.to_string_lossy().to_string();
            kingdom.cities = vec![city];
            kingdom.plans = vec![plan.clone()];
        }

        let proposing = plan_briefing("plan-briefing".into()).await.unwrap();
        assert!(
            proposing.contains(&city_path.to_string_lossy().to_string()),
            "the prompt must name the workspace the plan actually holds"
        );

        {
            let mut kingdom = lock().unwrap();
            plan.permissions = kingdom_core::Permissions::Full;
            kingdom.insert(plan);
        }
        let working = plan_briefing("plan-briefing".into()).await.unwrap();

        assert_ne!(
            proposing, working,
            "a plan that has gained its hands must not be shown a counsellor's \
             remit -- the widened permissions are the thing a reader opens this \
             to check"
        );

        assert!(
            plan_briefing("plan-nowhere".into()).await.is_err(),
            "an unknown plan is an error, not an empty prompt"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    /// The order the files rail draws, and the two things it hides.
    ///
    /// Directories before files and case-insensitive by name is what makes the
    /// rail readable; it is deliberately *not* the map's order, which is by
    /// byte size. `read_dir` yields in whatever order the filesystem holds, so
    /// without the sort here the rail would differ between machines.
    #[test]
    fn a_listing_is_ordered_for_reading_and_hides_only_detritus() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();

        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::create_dir_all(root.join(".github")).unwrap();
        std::fs::create_dir_all(root.join("target")).unwrap();
        std::fs::create_dir_all(root.join("node_modules")).unwrap();
        // A city's own worktree folder: entire further copies of the project,
        // which is noise on the map and actively confusing in a rail whose
        // whole job is showing the King the workspace he is working in.
        std::fs::create_dir_all(root.join(".kingdom/worktrees")).unwrap();
        std::fs::write(root.join("Cargo.toml"), "").unwrap();
        std::fs::write(root.join("README.md"), "").unwrap();
        std::fs::write(root.join(".gitignore"), "").unwrap();

        let listed = read_directory(root, "").unwrap();
        let names: Vec<&str> = listed.iter().map(|e| e.name.as_str()).collect();

        assert_eq!(
            names,
            vec![".github", "src", ".gitignore", "Cargo.toml", "README.md"],
            "directories first, then files, each case-insensitively by name"
        );

        // The whole reason this is not the map's tree: build output is noise in
        // both, but a dotfile is noise only on a skyline. A source tree that
        // hides `.github` and `.gitignore` is not the repository the King is
        // looking at.
        assert!(
            !names.contains(&"target")
                && !names.contains(&"node_modules")
                && !names.contains(&".kingdom"),
            "build detritus and Kingdom's own worktrees are hidden: {names:?}"
        );

        let github = listed.iter().find(|e| e.name == ".github").unwrap();
        assert!(github.is_dir, "a directory must be marked as one");

        let readme = listed.iter().find(|e| e.name == "README.md").unwrap();
        assert!(!readme.is_dir);
        assert_eq!(
            readme.language,
            kingdom_core::Language::Docs,
            "a file carries the same language the map would tint it with"
        );
    }

    /// Paths in a listing are relative to the workspace root, so the entry the
    /// King clicks is the one that can be listed a level down -- or read whole
    /// by `plan_source` -- without the browser ever knowing an absolute path.
    #[test]
    fn a_nested_listing_names_its_entries_from_the_workspace_root() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src/inner")).unwrap();
        std::fs::write(dir.path().join("src/main.rs"), "").unwrap();

        let listed = read_directory(dir.path(), "src").unwrap();
        let paths: Vec<&str> = listed.iter().map(|e| e.path.as_str()).collect();

        assert_eq!(paths, vec!["src/inner", "src/main.rs"]);
    }

    /// The second place in Kingdom where an outsider names a path and the server
    /// opens it, so it is pinned here as it is in `artifact.rs`: `..` must be
    /// refused lexically, not prefix-matched and then handed to the filesystem.
    #[test]
    fn a_path_that_leaves_the_workspace_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();

        for escape in ["../..", "src/../../etc", "/etc"] {
            assert!(
                read_directory(dir.path(), escape).is_err(),
                "{escape} leaves the workspace and must be refused"
            );
        }
    }

    /// A kingdom is opened many times; its model is fabricated once.
    ///
    /// Without this the second open would seat a fresh model *over* the stored
    /// one -- the user's real replies to a sample plan sitting beside a
    /// pristine copy of the same plan, multiplying on every restart. The
    /// opening model exists to give a new kingdom something to show, and a
    /// kingdom with records is not new.
    #[test]
    fn starter_plans_are_seeded_only_over_an_empty_store() {
        use kingdom_core::{CityId, ModelChoice, PlanId, Workspace};

        fn starter_plans(_: &[kingdom_core::City]) -> Vec<Plan> {
            vec![Plan::opened(
                PlanId::new("plan-fabricated"),
                CityId::new("c1"),
                "A fabricated decree",
                &ModelChoice::new("mock", None),
                Workspace::in_place("/dev/testburg"),
                NetworkMode::Shared,
            )]
        }

        let seated = seed_starter_plans(Vec::new(), &[], starter_plans);
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
            NetworkMode::Shared,
        )];
        assert_eq!(
            seed_starter_plans(recorded.clone(), &[], starter_plans),
            recorded,
            "a kingdom with records keeps them, and gets no second court"
        );
    }

    /// Speaking to a plan must be able to rescue a wedged one.
    ///
    /// `draft_plan` refuses to start a turn while `is_busy()`, and the
    /// conversation disables the composer while the plan is `Drafting` -- so a
    /// `working_on` left behind by a turn that died mid-flight locks the plan
    /// out of both doors at once. This is the state three real plans were found
    /// in on disk, and before the clear in `say` the only cure was restarting
    /// the server so `store::reconcile` could run.
    ///
    /// Pinned on `Plan` rather than through the `say` server function, which
    /// needs the process-global kingdom lock to reach: what matters is that the
    /// mark is gone and the plan is startable again.
    #[test]
    fn speaking_to_a_wedged_plan_makes_it_startable_again() {
        use kingdom_core::{CityId, ModelChoice, PlanId, Speaker, Workspace};

        let mut plan = Plan::opened(
            PlanId::new("plan-wedged"),
            CityId::new("c1"),
            "A decree whose turn died",
            &ModelChoice::new("mock", None),
            Workspace::in_place("/dev/testburg"),
            NetworkMode::Shared,
        );

        // Exactly what a turn killed mid-flight leaves behind.
        plan.status = kingdom_core::PlanStatus::Drafting;
        plan.working_on = Some("bash: cargo build".to_string());
        assert!(plan.is_busy(), "the fixture must start out wedged");

        // What `say` does to it.
        plan.status = kingdom_core::PlanStatus::Drafting;
        plan.working_on = None;
        plan.say(Speaker::User, "try again");

        assert!(
            !plan.is_busy(),
            "a plan the user has just spoken to must not still look busy, or \
             `draft_plan` will refuse to start it"
        );
    }

    /// The King's notes reach the court as one review, each against its own quote.
    ///
    /// The pairing is the whole payload. A decree that let a note drift from the
    /// text it answers gives the model an objection with no target -- and the
    /// failure would be silent, because a plausible revision of the wrong
    /// paragraph looks exactly like work.
    #[test]
    fn notes_reach_the_court_as_one_review_beside_what_they_answer() {
        use kingdom_core::ProposalNote;

        let note = |line: usize, quote: &str, body: &str| ProposalNote {
            id: format!("n{line}"),
            line,
            quote: quote.to_string(),
            body: body.to_string(),
            at: None,
        };

        let decree = notes_as_decree(&[
            note(1, "## The domain", "This should not touch `Plan::approve`."),
            note(9, "- `annotate(line, quote, body)`", "Why an Option?"),
        ]);

        assert!(
            decree.contains("> ## The domain\n\nThis should not touch `Plan::approve`."),
            "each note follows the text it is against: {decree}"
        );
        assert!(
            decree.contains("> - `annotate(line, quote, body)`\n\nWhy an Option?"),
            "{decree}"
        );
        assert!(
            decree.starts_with("I have read the plan and written notes"),
            "the notes arrive as one review, not as five separate remarks: {decree}"
        );

        // A wrapped quote is prefixed on every line. With only the first
        // prefixed, the quote reads as ending after one line and the rest reads
        // as the King speaking -- which is the model's cue to act on it.
        let wrapped = notes_as_decree(&[note(3, "One line.\nAnd its second.", "Shorten this.")]);
        assert!(
            wrapped.contains("> One line.\n> And its second.\n"),
            "{wrapped}"
        );
        assert!(
            wrapped.starts_with("I have read the plan and written a note"),
            "one note is not addressed in the plural: {wrapped}"
        );
    }

    /// A note is not something the King has said until he sends it.
    ///
    /// The same exclusion `queued` has, and for the same reason: `Plan::turns`
    /// is the one door between a plan's log and a model, so a half-written note
    /// reaching it would put the King's private second thoughts to the court
    /// while he was still deciding whether to make them.
    #[test]
    fn a_note_is_not_a_turn_until_it_is_sent() {
        let mut plan = a_plan();
        plan.status = kingdom_core::PlanStatus::AwaitingReview;
        plan.permissions = kingdom_core::Permissions::Propose;
        plan.propose("A plan", "# A plan\n\nDo the thing.");

        let before = plan.turns().count();
        plan.annotate(3, "Do the thing.", "Which thing?")
            .expect("a standing proposal can be annotated");

        assert_eq!(
            plan.turns().count(),
            before,
            "a note in the margin is not yet addressed to anyone"
        );
        assert!(
            !plan
                .turns()
                .any(|t| matches!(&t, kingdom_core::Turn::Message(m)
                if m.body.contains("Which thing?"))),
            "the note's text must not reach a model before it is sent"
        );

        // Sent, by the path `send_notes` takes: drained, composed, received.
        let notes = plan.take_notes();
        receive(&mut plan, notes_as_decree(&notes), false);

        assert!(
            plan.turns()
                .any(|t| matches!(&t, kingdom_core::Turn::Message(m)
                if m.body.contains("Which thing?"))),
            "once sent, it is an ordinary thing the King said"
        );
    }

    /// A review of code reaches the court as one decree, grouped by file, each
    /// note standing beside the line it answers.
    ///
    /// The pairing is the whole payload, exactly as it is for marginal notes:
    /// an objection separated from its line is one the model has to guess the
    /// target of, and a plausible edit to the wrong line looks exactly like
    /// work.
    #[test]
    fn a_code_review_reaches_the_court_grouped_by_file_and_ordered_by_line() {
        use kingdom_core::{NoteSide, ReviewNote};

        let note = |path: &str, line: u32, side, quote: &str, body: &str| ReviewNote {
            id: format!("{path}-{line}"),
            path: path.to_string(),
            line,
            side,
            quote: quote.to_string(),
            body: body.to_string(),
            at: None,
        };

        let decree = file_notes_as_decree(&[
            note(
                "src/lex.rs",
                42,
                NoteSide::Working,
                "let n = i + 1;",
                "This is the off-by-one.",
            ),
            note(
                "src/main.rs",
                7,
                NoteSide::Working,
                "run();",
                "Handle the error.",
            ),
            // Deliberately out of order and on an already-seen file: the
            // grouping has to gather it and the sort has to place it.
            note(
                "src/lex.rs",
                9,
                NoteSide::Working,
                "use std::io;",
                "Unused.",
            ),
        ]);

        assert!(
            decree.starts_with("I have read the code and written notes"),
            "a review arrives as one review, not as three separate remarks: {decree}"
        );

        // Grouped: one heading per file, in the order he first wrote against
        // them.
        let lex = decree.find("## src/lex.rs").expect("a heading per file");
        let main = decree.find("## src/main.rs").expect("a heading per file");
        assert!(lex < main, "files keep the order he read them in: {decree}");
        assert_eq!(
            decree.matches("## src/lex.rs").count(),
            1,
            "two notes on one file share one heading: {decree}"
        );

        // Ordered within a file, so the model reads it the way it will edit it.
        let nine = decree.find("Line 9:").expect("line 9 is reported");
        let forty_two = decree.find("Line 42:").expect("line 42 is reported");
        assert!(nine < forty_two, "lines ascend within a file: {decree}");
        assert!(
            nine > lex && forty_two < main,
            "both sit under their own file"
        );

        // Each note stands beside the line it answers.
        assert!(
            decree.contains("> let n = i + 1;\n\nThis is the off-by-one."),
            "{decree}"
        );
    }

    /// The two shapes that would otherwise be read as something the King said.
    ///
    /// A wrapped quote with only its first line prefixed reads as the quote
    /// ending and the King speaking -- which is the model's cue to *act* on the
    /// code rather than to read it. And a note on a blank line has no lines to
    /// iterate at all, so the quote would vanish and the note would arrive about
    /// nothing.
    #[test]
    fn a_quoted_line_cannot_be_mistaken_for_the_kings_own_words() {
        use kingdom_core::{NoteSide, ReviewNote};

        let note = |line: u32, side, quote: &str| ReviewNote {
            id: "n".into(),
            path: "src/lex.rs".into(),
            line,
            side,
            quote: quote.to_string(),
            body: "Fix this.".into(),
            at: None,
        };

        let wrapped = file_notes_as_decree(&[note(
            3,
            NoteSide::Working,
            "fn long_signature(\n    a: usize,",
        )]);
        assert!(
            wrapped.contains("> fn long_signature(\n>     a: usize,\n"),
            "{wrapped}"
        );
        assert!(
            wrapped.starts_with("I have read the code and written a note"),
            "one note is not addressed in the plural: {wrapped}"
        );

        let blank = file_notes_as_decree(&[note(3, NoteSide::Working, "")]);
        assert!(
            blank.contains("> (blank line)"),
            "a note on a blank line still says what it is against: {blank}"
        );

        // A note on the old side of a diff says which version it is about. A
        // bare line number would point the court at whatever now occupies that
        // position in the file.
        let deleted = file_notes_as_decree(&[note(88, NoteSide::Base, "self.cleanup();")]);
        assert!(
            deleted.contains("Line 88, in the version before your changes:"),
            "{deleted}"
        );
    }

    /// A review is not something the King has said until he sends it, and
    /// sending it takes the path every other message takes.
    #[test]
    fn a_code_review_is_not_a_turn_until_it_is_sent() {
        use kingdom_core::NoteSide;

        let mut plan = a_plan();
        plan.annotate_file(
            "src/lex.rs",
            42,
            NoteSide::Working,
            "let n = i + 1;",
            "This is the off-by-one.",
        )
        .expect("a plan in play can be annotated");

        assert!(
            !plan
                .turns()
                .any(|t| matches!(&t, kingdom_core::Turn::Message(m)
                if m.body.contains("off-by-one"))),
            "the note's text must not reach a model before it is sent"
        );

        // Sent, by the path `send_file_notes` takes: drained, composed,
        // received.
        let notes = plan.take_review_notes();
        receive(&mut plan, file_notes_as_decree(&notes), false);

        assert!(
            plan.turns()
                .any(|t| matches!(&t, kingdom_core::Turn::Message(m)
                if m.body.contains("off-by-one"))),
            "once sent, it is an ordinary thing the King said"
        );
    }

    /// A plan is filed once, whichever way it ends.
    ///
    /// The two moments a plan can be filed -- approval, and merge or archive --
    /// meeting in the one outcome that matters: exactly one document, holding
    /// what the King agreed to. Approving and then merging must not file twice,
    /// and a plan that ends without ever being approved must still be filed.
    ///
    /// Tested through `store::file_plan` in the order `api` calls it, rather
    /// than through `finish_plan` itself, which needs a git repository, a
    /// scanned kingdom and a process-global lock to reach -- the same reason
    /// the subagent guard above is tested through its predicate. What the git
    /// work does to the draft is pinned in `worktree.rs`.
    #[test]
    fn a_plan_is_filed_once_however_it_ends() {
        use crate::profile::testing::Profile;
        use kingdom_core::PlanId;

        let dir = tempfile::tempdir().unwrap();
        let _profile = Profile::at(&dir.path().join("profile"));
        let root = dir.path().join("dev");
        std::fs::create_dir_all(&root).unwrap();

        // Approved, then merged: the King's terms are filed at the grant, and
        // finishing finds the document already there. The draft has moved on in
        // between, because after approval the court may rewrite it freely.
        let mut approved = a_plan();
        approved.propose("The terms", "# The terms\n\nAs agreed.");
        assert!(approved.approve());

        crate::store::file_plan(&root, &approved, "# The terms\n\nAs agreed.\n").unwrap();
        let path =
            crate::store::file_plan(&root, &approved, "# Drifted\n\nSomething else.\n").unwrap();

        let body = std::fs::read_to_string(&path).unwrap();
        assert!(body.contains("As agreed."), "{body}");
        assert!(
            !body.contains("Something else."),
            "finishing must not overwrite what the King approved: {body}"
        );

        // Archived having never been approved: nothing filed it earlier, so
        // this is its first and only filing. Without it the plan's document
        // would be lost entirely.
        let mut abandoned = a_plan();
        abandoned.id = PlanId::new("plan-2");
        abandoned.propose("Never accepted", "# Never accepted\n\nSet aside.");

        assert!(!crate::store::filed_plan(&root, &abandoned).exists());
        let path =
            crate::store::file_plan(&root, &abandoned, "# Never accepted\n\nSet aside.\n").unwrap();
        assert!(
            std::fs::read_to_string(&path)
                .unwrap()
                .contains("Set aside."),
            "a plan that ended without approval is still worth keeping"
        );

        // And a plan that never drafted files nothing, rather than an empty
        // document -- what an abandoned plan legitimately looks like.
        let mut silent = a_plan();
        silent.id = PlanId::new("plan-3");
        assert!(crate::store::file_plan(&root, &silent, "").is_err());
        assert!(!crate::store::filed_plan(&root, &silent).exists());
    }

    /// The guard with the largest blast radius in the codebase.
    ///
    /// A subagent's workspace is a clone of its parent's, so finishing one
    /// would merge the parent's half-finished work and then delete the worktree
    /// from under a plan still running in it. The conversation never offers the
    /// button -- but "the UI does not offer it" is not a guarantee, and the
    /// thing being protected is the user's real work.
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
            NetworkMode::Shared,
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

    /// The network feed must never reach for the kingdom lock twice.
    ///
    /// The fault this guards is the one `events::publish_within` was split out
    /// to fix, and it is worth restating because this feed is *new code on the
    /// same path*: the kingdom's mutex is a plain [`std::sync::Mutex`], so a
    /// thread that asks for it while already holding it deadlocks -- and it
    /// deadlocks **holding the lock**, so every later request in the process
    /// hangs behind it. The server answers once and then spins forever.
    ///
    /// `kingdom_network` reads two things that live outside the kingdom (the
    /// wells, and each namespace's forwards) and one that lives inside it (a
    /// plan's city root). The rule it follows is: take the guard once, collect
    /// everything, drop it, *then* ask the outside world. This holds the lock
    /// while calling the feed to prove the feed does not want it again.
    ///
    /// Run on its own thread with a deadline, so a reintroduced deadlock is a
    /// failing test rather than a suite that never finishes.
    #[test]
    fn the_network_feed_does_not_reach_for_the_kingdom_twice() {
        let (done, finished) = std::sync::mpsc::channel();

        std::thread::spawn(move || {
            // Held for the whole call, which is the shape `api::update` is in
            // when it publishes -- the situation that actually deadlocked.
            let kingdom = lock().expect("the kingdom locks");

            // The collecting half of the feed, run under the guard. If any of
            // it asked for the lock again this would never return.
            let agents: Vec<_> = kingdom
                .plans
                .iter()
                .filter(|plan| plan.is_live() && !plan.is_subagent())
                .map(|plan| {
                    (
                        plan.id.clone(),
                        plan.title.clone(),
                        plan.city.clone(),
                        plan.network,
                    )
                })
                .collect();
            for (plan, _, _, _) in &agents {
                let _ = city_root_in(&kingdom, plan);
            }

            drop(kingdom);
            let _ = done.send(());
        });

        assert!(
            finished
                .recv_timeout(std::time::Duration::from_secs(10))
                .is_ok(),
            "resolving every agent's city under the kingdom lock must return -- \
             a second lock from inside this path deadlocks the whole server"
        );
    }

    pub(crate) fn a_plan() -> Plan {
        Plan::opened(
            PlanId::new("plan-1"),
            kingdom_core::CityId::new("c1"),
            "Fix the parser",
            &kingdom_core::ModelChoice::new("mock", None),
            kingdom_core::Workspace::in_place("/dev/testburg"),
            NetworkMode::Shared,
        )
    }

    pub(crate) fn said(plan: &Plan) -> Vec<&str> {
        plan.transcript
            .iter()
            .filter_map(|e| match e {
                kingdom_core::Entry::Message(m) => Some(m.body.as_str()),
                _ => None,
            })
            .collect()
    }

    /// While a turn is running, the user's words wait rather than landing in a
    /// conversation the model is already halfway through reading.
    #[test]
    fn words_spoken_over_a_running_turn_are_queued() {
        let mut plan = a_plan();
        plan.working_on = Some("bash: cargo test".into());
        let before = plan.transcript.len();

        receive(&mut plan, "also check the tests".into(), true);

        assert_eq!(
            plan.transcript.len(),
            before,
            "the log must not gain an entry the running turn did not put there"
        );
        assert_eq!(plan.queued.len(), 1);
        assert!(
            plan.is_busy(),
            "queuing must leave the busy mark alone -- the turn holding it is \
             still running, and clearing it would let a second turn start"
        );
    }

    /// The regression the `is_running`/`is_busy` split exists to prevent.
    ///
    /// A plan whose busy mark outlived its turn -- a panic, a dropped future,
    /// a server killed mid-round -- *looks* busy and is not. If `say` decided
    /// by `is_busy()`, every message the user sent such a plan would be queued
    /// behind a turn that will never drain it, and the plan would be
    /// permanently mute. Deciding by whether a turn is genuinely running keeps
    /// speaking to it the cure it has always been.
    #[test]
    fn a_plan_wedged_by_a_dead_turn_is_still_rescued_by_speaking_to_it() {
        let mut plan = a_plan();
        plan.working_on = Some("bash: cargo test".into());

        // No turn is registered: the mark is stale.
        receive(&mut plan, "are you still there?".into(), false);

        assert!(
            !plan.is_busy(),
            "the stale mark must be cleared, or `draft_plan` keeps refusing"
        );
        assert!(
            plan.queued.is_empty(),
            "words to a wedged plan must not be queued -- nothing would drain them"
        );
        assert_eq!(plan.status, kingdom_core::PlanStatus::Drafting);
        assert!(said(&plan).contains(&"are you still there?"));
    }
}
