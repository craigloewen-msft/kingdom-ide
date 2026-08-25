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

/// Applies a change to one plan, records it, publishes it, and returns the
/// result, so callers hand the browser the same value that was just stored.
///
/// The single funnel for plan mutations, which is why both persistence and push
/// hang off it: a caller cannot change a plan and forget to write it, and
/// equally cannot change a plan and forget to tell the conversation watching
/// it. See [`crate::events`] for why that second one had to live here rather
/// than at each call site.
#[cfg(feature = "ssr")]
fn update(kingdom: &mut Kingdom, id: &PlanId, change: impl FnOnce(&mut Plan)) -> Option<Plan> {
    let root = std::path::PathBuf::from(&kingdom.root);
    let plan = kingdom.plans.iter_mut().find(|p| &p.id == id)?;
    change(plan);
    remember(&root, plan);
    // After `remember`, not before: a failed write appends a note to the plan,
    // and the conversation should be told the thing that was actually stored
    // rather than an optimistic version of it.
    crate::events::publish(plan);
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

/// Writes a plan to the records, turning a failed write into something the user
/// can see rather than something that fails his prompt.
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
    assemble(&root, None)
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
    open_fixture(&name).map_err(ServerFnError::new)
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
    assemble(&root, None)
        .map(Some)
        .map_err(|e| e.to_string())
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
fn assemble(
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

    Ok(kingdom)
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

    let mut plan = Plan::opened(plan_id, city_id, &prompt, &choice, workspace.clone());
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

    let mut kingdom = lock()?;
    let root = std::path::PathBuf::from(&kingdom.root);
    remember(&root, &mut plan);
    kingdom.plans.push(plan.clone());

    Ok(plan)
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

/// One directory of a city, listed on demand for the files rail.
///
/// `path` is relative to the city root; empty lists the root itself. Returns
/// directories first, then files, each case-insensitively by name -- the order
/// a person expects to read a tree in, which the map's own tree deliberately
/// does not use (it sorts by size, because a skyline is drawn largest-first).
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
/// resolved through a [`crate::tools::Sandbox`] rooted at the city, exactly as
/// [`crate::artifact`] does, so `a/../../etc` is refused lexically before the
/// filesystem sees it. It lists **names only** and reads no file contents,
/// which is the property that keeps it from becoming a file server.
#[server(ListDirectory, "/api")]
pub async fn list_directory(
    city: String,
    path: String,
) -> Result<Vec<kingdom_core::DirEntry>, ServerFnError> {
    use kingdom_core::CityId;

    let city_id = CityId::new(city);
    let city_root = {
        let kingdom = lock()?;
        let Some(city) = kingdom.city(&city_id) else {
            // A city Kingdom does not know has no directory to read. An empty
            // listing rather than an error: the rail asks about whatever is
            // selected, and a selection that has gone stale is ordinary.
            return Ok(Vec::new());
        };
        std::path::Path::new(&kingdom.root).join(&city.path)
    };

    read_directory(&city_root, &path).map_err(ServerFnError::new)
}

/// Everything [`list_directory`] decides once the city's root is known.
///
/// Split out so the boundary and the ordering can be tested against a real
/// directory without a kingdom in global state -- the same split, for the same
/// reason, as `artifact::from_workspace`.
#[cfg(feature = "ssr")]
fn read_directory(
    city_root: &std::path::Path,
    path: &str,
) -> Result<Vec<kingdom_core::DirEntry>, String> {
    use kingdom_core::{DirEntry, Language, Workspace};

    let shop = crate::tools::Sandbox::new(Workspace::in_place(city_root.to_string_lossy()));
    let dir = shop
        .resolve(path)
        .map_err(|_| format!("{path} is outside this city."))?;

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
    .ok_or_else(|| ServerFnError::new("That plan is no longer in the records."))
}

/// Puts the user's words either in the queue or straight into the log.
///
/// Split out from [`say`] so the branch can be tested without a live turn and
/// without the kingdom singleton: `turn_running` is the only thing the two
/// paths differ on, and it is the one fact the caller must establish.
#[cfg(feature = "ssr")]
fn receive(plan: &mut Plan, prompt: String, turn_running: bool) {
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

    Ok(updated)
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
    .ok_or_else(|| ServerFnError::new("That plan is no longer in the records."))
}

/// How many times the model may act before it must speak.
///
/// A loop that never ends is the failure mode worth being loud about: an agent
/// retrying a broken command against a paid model spends real money producing
/// nothing, and it does so quietly. Reaching this is recorded as a note, so the
/// user sees the plan stopped rather than finding a plausible answer that was
/// actually a truncation.
#[cfg(feature = "ssr")]
const MOST_ROUNDS: usize = 500;

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
            converse(
                plan_id,
                city_brief,
                workspace,
                city_name,
                choice,
                MOST_ROUNDS,
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
        Ok(result) => result,
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
            repaired.ok_or_else(|| ServerFnError::new(format!("drafting task failed: {e}")))
        }
    }
}

/// Takes turns with the model until it speaks, proposes, runs out of rope, or
/// fails.
///
/// The loop lives here rather than behind the [`crate::llm::Model`] trait
/// because this is where the plan is. A provider running its own tools would do
/// so with nothing recording them: the conversation would show a long silence
/// and then an answer, and a crash mid-way would leave no trace of what had
/// already been done to the workspace.
///
/// Two callers: [`draft_plan`], for a plan the user opened, and
/// [`spawn_subagents`], for each subagent the model sends. They differ only in
/// the `cap` handed in -- a subagent gets fewer rounds. The *permissions* are
/// no longer a parameter: they are read back off the plan each pass, because
/// they can change mid-conversation when the user accepts a proposal. Sharing
/// the loop is the point: a subagent that drafted through a second, simpler
/// path would be a second place for the busy mark, the tool call recording and
/// the push to drift.
#[cfg(feature = "ssr")]
pub(crate) async fn converse(
    plan_id: PlanId,
    city: crate::llm::CityBrief,
    workspace: kingdom_core::Workspace,
    city_name: String,
    choice: kingdom_core::ModelChoice,
    cap: usize,
) -> Result<Plan, ServerFnError> {
    use crate::llm::{Brief, Reply, SystemPrompt, ToolSpec};
    use crate::tools::Sandbox;
    use kingdom_core::{NoteKind, ToolCall, ToolOutcome};

    // Registers this turn as genuinely running, and yields the signal a Stop
    // travels down. Taken before the first model call and held to the last
    // line, because both of its readers depend on the bracket being exact:
    // `say` queues only while this exists, and `stop_plan` reads its absence as
    // a stale busy mark to repair.
    let mut halt = crate::turns::begin(&plan_id);

    // Read once, outside the loop: the kingdom's root cannot move while a turn
    // runs, and it bounds the guidance walk in every round's prompt. See
    // `SystemPrompt::assemble`.
    let kingdom_root = {
        let kingdom = lock()?;
        std::path::PathBuf::from(&kingdom.root)
    };

    let model = match crate::llm::open(&choice).await {
        Ok(model) => model,
        // A missing credential surfaces as a failed plan the user can see and
        // retry, rather than an error attached to nothing.
        Err(e) => return settle(plan_id, Err(e)),
    };

    // Distinguishes this turn's rounds from every other turn's on the same
    // plan. See `batch_id`.
    let turn = uuid::Uuid::new_v4().to_string();

    for round in 0..cap {
        // A halt that landed between the two long awaits below is caught here
        // rather than being held until the next one. Without this, stopping a
        // turn during the brief window where it is neither calling the model
        // nor running a tool would appear to do nothing until the *next* model
        // call had already been paid for.
        if halt.was_halted() {
            return stopped(plan_id, None);
        }

        // The conversation is rebuilt from the plan each pass rather than
        // accumulated in a local. The tool calls recorded below are already in
        // it, and reading them back is what makes this loop's state the plan's
        // state -- so a reader of the transcript sees exactly what the model
        // saw.
        //
        // The *remit* is read back for the same reason, and it is the reason
        // this read now yields more than turns. A plan can gain its hands
        // mid-conversation: the King accepts a proposal, `approve_plan` widens
        // the remit, and the very next pass must offer the tools that grant
        // implies. Resolving the tools once before the loop -- as this used to
        // -- would have left an approved plan holding a counsellor's toolbox.
        let (turns, permissions, approved) = {
            let mut kingdom = lock()?;

            // The user spoke while the court was working. Their words join the
            // log *before* it is read back, so this round's brief carries them.
            //
            // Here rather than anywhere else because this is the one moment in
            // a turn where nothing is half-done: the last deed is settled and
            // the next has not been asked for. Splicing them in mid-deed would
            // hand the model a conversation in which a tool call and its result
            // are separated by something nobody said at the time.
            //
            // Guarded rather than called unconditionally: `update` saves and
            // publishes, and a write per round with nothing in it would push a
            // whole plan over every watch socket for no news.
            if kingdom.plan(&plan_id).is_some_and(|p| !p.queued.is_empty()) {
                update(&mut kingdom, &plan_id, |p| {
                    p.hear_queued();
                });
            }

            let Some(plan) = kingdom.plan(&plan_id) else {
                return Err(ServerFnError::new("That plan vanished mid-decree."));
            };
            (
                plan.turns().collect::<Vec<_>>(),
                plan.permissions,
                plan.approved_proposal().is_some(),
            )
        };

        // Whether the court has actually done anything yet is no longer asked:
        // a reply with prose and no tool call ends the turn, full stop.
        // A model that narrates instead of acting is answering the user early,
        // and the user can say so.

        // A model that cannot call tools still drafts perfectly good prose, so
        // it gets a prose-only turn rather than an error; one that cannot see is
        // not offered the tool that hands back a picture; and a plan under a
        // narrower remit is not offered the tools it may not run. All three
        // narrowings live in `ToolSpec::for_model`, so the reasoning is in one
        // place and no caller has to remember any of them.
        let tools = ToolSpec::for_model(model.as_ref(), permissions);
        let shop = Sandbox::new(workspace.clone())
            .for_plan(plan_id.clone())
            .under(permissions);

        let brief = Brief {
            system_prompt: SystemPrompt::assemble(
                &city,
                &workspace,
                permissions,
                approved,
                &kingdom_root,
            ),
            turns,
            tools: tools.clone(),
        };

        // Raced against the halt so a Stop lands while the model is still
        // thinking, rather than after a reply nobody wants has been paid for.
        // Dropping the future drops the HTTP request, which is what Phoenix
        // achieves by aborting the task -- the same effect, with no task to
        // abort and with every careful clearing below still on the return path.
        //
        // `biased` so a halt already signalled wins deterministically instead
        // of by coin-flip against a reply that happened to arrive at once.
        let answer = tokio::select! {
            biased;
            _ = halt.halted() => return stopped(plan_id, None),
            answer = model.take_turn(&brief) => match answer {
                Ok(answer) => answer,
                Err(e) => return settle(plan_id, Err(e)),
            },
        };

        // Recorded before the reply is acted on, and in a write of its own.
        // Folding it into the tool-call recording below would tie it to *acts*
        // rather than to rounds: a round with three calls would write the same
        // reading three times, and a round that only speaks would never write
        // it at all.
        //
        // Being its own `update` also means `events::publish` pushes it, so the
        // header's bar climbs while a long tool loop is still running -- which
        // is exactly when the user wants to see it moving.
        //
        // Both numbers or neither: a count with no window is a percentage of
        // nothing. See `kingdom_core::ContextUsage`.
        if let Some(tokens) = answer.tokens {
            let window = model.context_window();
            let mut kingdom = lock()?;
            update(&mut kingdom, &plan_id, |p| {
                p.context = Some(kingdom_core::ContextUsage { tokens, window });
            });
        }

        match answer.reply {
            Reply::Spoke(draft) => {
                // Prose with no tool call ends the turn, as it does in Phoenix.
                // Kingdom used to intercept a narration-only first reply and
                // send it back round with an instruction appended; that was a
                // Kingdom invention and it is gone.
                // `settle` still records the reply and parks the plan; its
                // return value is deliberately discarded, because the decision
                // below must be made against a fresher read than it can give.
                settle(plan_id.clone(), Ok(draft))?;

                // ...unless the King got a word in while the court was
                // working. `settle` has already recorded the reply and parked
                // the plan, so nothing is lost if this is the last pass; but
                // words left queued here would be waited on by nobody, because
                // the turn that was going to hear them is this one.
                //
                // The queue is re-read under the lock rather than taken from
                // `answered`, and the turn deregisters in the same critical
                // section. That pairing is the whole correctness argument, and
                // the snapshot alone was not enough: `settle` releases the lock
                // before returning, so `say` could see a turn still running,
                // queue against it, and have this branch then consult a
                // snapshot taken before those words existed -- stranding them
                // behind a turn already on its way out.
                //
                // `say` reads the registry under this same lock, so after this
                // block it either queued (and we see it) or found no turn (and
                // takes the direct path, starting a fresh one). There is no
                // third case.
                let mut kingdom = lock()?;
                let waiting = kingdom
                    .plan(&plan_id)
                    .is_some_and(|p| !p.queued.is_empty());

                // Going round again costs a round, so a drain on the last one
                // would fall through to the out-of-rope branch below and report
                // a turn that actually answered as having run out. The words
                // stay queued instead, exactly as they do when a turn fails
                // with some waiting: the next thing the King says flushes them.
                if !waiting || round + 1 >= cap {
                    halt.stand_down();
                    return kingdom
                        .plan(&plan_id)
                        .cloned()
                        .ok_or_else(|| ServerFnError::new("That plan vanished mid-decree."));
                }

                update(&mut kingdom, &plan_id, |p| {
                    p.status = kingdom_core::PlanStatus::Drafting;
                    p.working_on = Some("Hearing what the King added".to_string());
                });
                drop(kingdom);
                continue;
            }

            Reply::Acts(acts) => {
                // One id for everything this reply asked for, so the calls can
                // be replayed as the single decision they were rather than as a
                // sequence the model deliberated through. See
                // `kingdom_core::ToolCall::batch` and `batch_id`.
                let batch = batch_id(&plan_id, &turn, round);
                let mut first = true;

                for act in acts.calls {
                    // Recorded *before* it runs, and published by `update`, so
                    // the conversation shows the command while it is still
                    // going. This is the answer to "what is this agent doing
                    // right now", and it only works because the tool call is
                    // written down first.
                    {
                        let mut kingdom = lock()?;
                        // The thinking rides on the first call of the batch and
                        // nowhere else: one reply produced one piece of
                        // reasoning, however many things it asked for, and
                        // copying it onto each would replay it several times
                        // over as though the model had thought it repeatedly.
                        let reasoning = first.then(|| acts.reasoning.clone()).flatten();
                        let narration = first.then(|| acts.narration.clone()).flatten();
                        first = false;

                        update(&mut kingdom, &plan_id, |p| {
                            p.working_on = Some(describe(&act.tool, &act.input));
                            p.begin_tool_call(
                                ToolCall::started(
                                    act.id.clone(),
                                    act.tool.clone(),
                                    act.input.clone(),
                                )
                                .in_reply(batch.clone(), reasoning.clone(), narration.clone())
                                // Asked of the tool itself, from the same
                                // arguments the deed is being recorded with, so
                                // what the chamber tells the King it is waiting
                                // for and what the tool actually waits for
                                // cannot disagree. Read here rather than when
                                // the line is drawn, because a budget that
                                // arrives with the result would appear only
                                // once it no longer mattered.
                                .waiting(crate::tools::waits_for(&act.tool, &act.input, &shop)),
                            );
                        });
                    }

                    // Read off the arguments *before* they are handed to the
                    // tool, and from the same value the deed was recorded with
                    // a moment ago -- so what the transcript shows and what the
                    // chamber offers cannot disagree.
                    //
                    // Reads the draft the call names, which is why it takes the
                    // sandbox: the plan's body lives in a file now, not in the
                    // arguments. See `tools::propose_plan`.
                    let put = (act.tool == "propose_plan")
                        .then(|| crate::tools::propose_plan::proposed(&act.input, &shop))
                        .flatten();

                    // The lock is deliberately not held across this. A tool can
                    // take minutes -- and `ask_user_question` waits on a person
                    // -- so a held lock would freeze every other plan and every
                    // conversation in the kingdom behind it.
                    //
                    // Bound to this tool call, so a tool that has to reach back
                    // out to the user has something the browser can name when
                    // it answers.
                    // Raced against the halt for the same reason as the model
                    // call, and with one deliberate consequence: a stopped
                    // `bash` keeps its process. Phoenix documents the same
                    // choice, and here the `JOBS` registry means the handle
                    // survives, so a later turn can still peek at it or kill
                    // it. Killing on stop is a separate decision about what a
                    // halt means -- see the task's out-of-scope note.
                    // Bound before the race rather than inside it: a temporary
                    // built in a `select!` arm is dropped at the end of the
                    // statement, and `invoke` borrows it for the length of the
                    // call.
                    let bench = shop.for_tool_call(&act.id);
                    let outcome = tokio::select! {
                        biased;
                        _ = halt.halted() => return stopped(plan_id, Some(&act.id)),
                        outcome = crate::tools::invoke(&act.tool, act.input, &bench) => outcome,
                    };

                    let mut kingdom = lock()?;
                    let mut proposed = None;
                    update(&mut kingdom, &plan_id, |p| {
                        if !p.settle_tool_call(&act.id, outcome.clone()) {
                            // Cannot happen -- the tool call was recorded a
                            // moment ago under the same id -- but a silently
                            // dropped result would leave the model waiting
                            // forever for a call the transcript says it never
                            // made.
                            p.note(
                                NoteKind::Failed,
                                format!(
                                    "A result arrived for a call ({}) that is not in this \
                                     plan's log. This is a bug in Kingdom.",
                                    act.id
                                ),
                            );
                        }

                        // A plan put to the King ends the turn.
                        //
                        // The outcome is checked as well as the arguments: a
                        // refused call -- bad arguments, or the wrong remit --
                        // is one the model should be told about and allowed to
                        // correct on the next round, not one that parks the
                        // plan in front of the King with nothing to read.
                        let accepted = matches!(&outcome, ToolOutcome::Done { output, .. }
                            if output == crate::tools::propose_plan::PROPOSED);
                        if let (Some((title, body)), true) = (put.clone(), accepted) {
                            p.propose(title, body);
                            p.working_on = None;
                            p.status = kingdom_core::PlanStatus::AwaitingReview;
                            proposed = Some(p.clone());
                        }
                    });

                    // Nothing is parked and no request stays open: the plan is
                    // on disk with its proposal, and the chamber has been told.
                    // A server restart mid-review therefore loses nothing --
                    // see the module docs on `tools::propose_plan` for why that
                    // is worth more than resuming in place would have been.
                    if let Some(plan) = proposed {
                        // Unless the King was already speaking. A proposal
                        // parks the turn *for review*, and words queued during
                        // it are that review arriving early -- the same path
                        // `say` + `draft_plan` take for notes sent back on a
                        // proposal, without the round trip.
                        //
                        // Safe to read off `plan` here, unlike the prose path
                        // above: `proposed` was cloned inside the `update`
                        // that ran under the lock this scope still holds, so
                        // no `say` can have run since. `stand_down` happens
                        // under that same lock for the reason given there.
                        //
                        // The round check matches the prose path: draining
                        // costs a round, and spending the last one would report
                        // a plan that did propose as having run out of rope.
                        if plan.queued.is_empty() || round + 1 >= cap {
                            halt.stand_down();
                            return Ok(plan);
                        }
                        update(&mut kingdom, &plan_id, |p| {
                            p.status = kingdom_core::PlanStatus::Drafting;
                            p.working_on = Some("Hearing what the King added".to_string());
                        });
                        break;
                    }
                }

                // Back round to ask again, now with the results in the log.
            }
        }
    }

    // Out of rope. Recorded as a note rather than a reply: nothing was said,
    // and dressing a truncation as counsel would hand the user an answer that
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

/// Sends a set of subagents and waits for them all to report.
///
/// Called by the `spawn_agents` tool, which is itself running inside its
/// parent's [`converse`] loop -- so this is the turn loop calling itself, one
/// level down, with narrower permissions. That shape is why `converse` is
/// `pub(crate)` rather than private.
///
/// Each subagent is a real plan: recorded, watched and pushed like any other,
/// which is what lets the user open one and read it while it works. They run
/// concurrently, which is only safe because a subagent is born under
/// [`kingdom_core::Permissions::ReadOnly`] and so cannot write -- see
/// [`Plan::spawned`], which is where that is now settled, and
/// `tools::spawn_agents`.
///
/// Returns the reports as one block of text for the model, or `Err` with a
/// reason to report to it when no subagent could be sent at all.
#[cfg(feature = "ssr")]
pub(crate) async fn spawn_subagents(
    parent_id: &PlanId,
    tool_call: &str,
    tasks: Vec<crate::tools::spawn_agents::Errand>,
    patience: std::time::Duration,
) -> Result<String, String> {
    // Everything the subagents need is read once, under one lock: the parent
    // they are cut from, and the city they work in. Holding it across the model
    // calls below would freeze every other plan in the kingdom.
    let (subagents, city_brief, city_name) = {
        let mut kingdom = lock().map_err(|e| e.to_string())?;

        let Some(parent) = kingdom.plan(parent_id).cloned() else {
            return Err(
                "The plan that sent these errands is no longer in the records.".to_string(),
            );
        };
        let Some(city) = kingdom.city(&parent.city).cloned() else {
            return Err("That plan's city is gone.".to_string());
        };

        // A subagent is briefed on the *workspace*, exactly as its parent is:
        // it is sent to look at the work in progress, not at a pristine copy.
        let city_brief = crate::llm::CityBrief::from_city(&city, &parent.workspace);

        let mut subagents = Vec::new();
        for errand in tasks {
            let id = PlanId::new(format!("plan-{}", next_plan_number()));
            let mut subagent =
                Plan::spawned(id.clone(), &parent, tool_call, errand.task.clone());
            let root = std::path::PathBuf::from(&kingdom.root);
            remember(&root, &mut subagent);
            // Pushed as well as recorded, so the parent's conversation can draw
            // the subagent the instant it exists rather than when it first
            // speaks.
            crate::events::publish(&subagent);
            kingdom.plans.push(subagent);
            subagents.push((id, errand));
        }

        (
            subagents,
            city_brief,
            city.name,
        )
    };

    // All at once. The permissions are what make this safe: they share one
    // worktree and none of them can write to it. That is settled on the
    // subagent itself, by `Plan::spawned`, and read back by the loop -- so the
    // invariant travels with the plan rather than with this call site.
    let running: Vec<_> = subagents
        .iter()
        .map(|(id, errand)| {
            let id = id.clone();
            let city_brief = city_brief.clone();
            let city_name = city_name.clone();
            // How many rounds this one gets: what the model asked for, or the
            // default. Per subagent rather than one figure for the batch, so a
            // cheap lookup can be capped short without shortening the survey
            // running beside it.
            let cap = errand.max_turns;
            // Read back rather than carried down, so a subagent is drafted by
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
                    cap,
                )
                .await
            })
        })
        .collect();

    // One deadline across the whole call rather than one per subagent: they run
    // concurrently, so the bound that matters is how long the *parent* waits.
    //
    // Collected one at a time against that shared deadline, which is what keeps
    // a partial answer possible -- a subagent that has already reported is
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

/// Renders what the subagents found, for the model that sent them.
///
/// Each block names the subagent's plan id, which is what lets the parent refer
/// to one in its own reply -- and what lets the user find the same conversation
/// the parent read.
#[cfg(feature = "ssr")]
fn report(
    subagents: &[(PlanId, crate::tools::spawn_agents::Errand)],
    outcomes: &[Option<Plan>],
) -> String {
    use kingdom_core::Speaker;

    let mut out = String::new();
    for (i, (id, errand)) in subagents.iter().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        out.push_str(&format!(
            "--- errand {} ({id}) ---\nTask: {}\n\n",
            i + 1,
            errand.task
        ));

        // Read from the record rather than from the returned plan: a subagent
        // that timed out here may still have said something, and its record is
        // where that would be.
        let plan = outcomes
            .get(i)
            .and_then(|p| p.clone())
            .or_else(|| snapshot(id));

        // The last thing the *model* said is the subagent's answer. Read with a
        // fold rather than `next_back` because `messages()` yields a plain
        // forward iterator; taking the last match is the whole intent.
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

/// Plain-language "what is happening right now", for the rail and the map.
///
/// The tool's name alone is close to useless to the user -- "bash" tells him
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

    // Likewise a plan put to him. Not a command running and not a question
    // either: the turn is over and the next move is his, which is a different
    // sort of "busy" again and worth its own words.
    if tool == "propose_plan" {
        return "Awaiting the King's word".to_string();
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
                // The title is deliberately *not* touched here. It was written
                // once from the decree and is rewritten only when the court
                // proposes -- see `Plan::propose`. Setting it from whatever
                // heading the model happened to lead this reply with made the
                // rail label change under the user on every turn.
                plan.summary = draft.summary.clone();
                plan.status = PlanStatus::AwaitingReview;
                plan.say(Speaker::Assistant, draft.body.clone());
                // The model has spoken, so any proposal the user had not
                // accepted is no longer the question on the table. Without
                // this, a plan that proposed, was asked something, and answered
                // in prose would still be offering to start work the
                // conversation has moved past. An accepted proposal is left
                // alone -- it is not a question, it is the job in hand.
                plan.set_aside_proposal();
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

/// Records that the user called a halt, and marks the plan no longer busy.
///
/// The sibling of [`settle`], and it differs in exactly one judgement: the plan
/// is left `AwaitingReview`, not `Failed`. Nothing failed. The user chose this,
/// and painting a deliberate act in the colour of a breakage misreports who did
/// what -- besides putting the plan into the status the conversation offers a
/// retry against, which is not what he asked for.
///
/// `in_flight` names the deed the halt interrupted, when it interrupted one. It
/// is settled as [`ToolOutcome::Refused`] -- the same variant `store::reconcile`
/// uses for a call the server died during, for the same reason: a call left
/// unsettled is replayed to the model on every later turn as though it were
/// still running, and the model waits forever for a result nobody will send.
#[cfg(feature = "ssr")]
fn stopped(plan_id: PlanId, in_flight: Option<&str>) -> Result<Plan, ServerFnError> {
    // A question parked in front of the user belongs to a turn that has now
    // stopped. Clearing it keeps `PENDING` from holding a oneshot that nothing
    // will ever answer, and stops a stale question in an open tab from
    // resolving a call that is already settled below.
    if let Some(id) = in_flight {
        crate::tools::ask_user_question::abandon(&plan_id, id);
    }

    let mut kingdom = lock()?;

    let updated = update(&mut kingdom, &plan_id, |plan| halted(plan, in_flight));

    updated.ok_or_else(|| ServerFnError::new("That plan vanished mid-decree."))
}

/// What a halt leaves on the plan. Split from [`stopped`] so the judgement can
/// be tested without a kingdom or a running turn.
#[cfg(feature = "ssr")]
fn halted(plan: &mut Plan, in_flight: Option<&str>) {
    use kingdom_core::{NoteKind, PlanStatus, ToolOutcome};

    plan.working_on = None;
    plan.status = PlanStatus::AwaitingReview;

    if let Some(id) = in_flight {
        plan.settle_tool_call(
            id,
            ToolOutcome::Refused {
                reason: "The King called a halt while this was running. Whether it \
                         finished is unknown."
                    .to_string(),
            },
        );
    }

    // A note rather than a reply: the court did not say this, Kingdom did.
    // Recording it as counsel would replay it to the model next turn as
    // something it had told the user itself.
    plan.note(
        NoteKind::Stopped,
        "The King called a halt. The court stopped where it stood; anything it had \
         already done is still in its workspace. Say something to set it going again.",
    );
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

    snapshot(&plan_id).ok_or_else(|| ServerFnError::new("That plan is no longer in the records."))
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
        // The ledger entry is written in the same breath, at the one moment it
        // is unambiguously true. `plans/<id>.json` keeps changing as the plan
        // works; that record does not -- see `store::record_approval`. Doing it
        // inside this closure means there is no path that widens the
        // permissions without also writing the record.
        match crate::store::record_approval(&root, p) {
            Ok(path) => {
                let kept = path.strip_prefix(&root).unwrap_or(&path).display().to_string();
                p.note(
                    NoteKind::Workspace,
                    format!(
                        "Approved. The court may now change the project. \
                         Recorded at {kept}."
                    ),
                );
            }
            // The approval stands: the King said yes and the permissions
            // widened. Refusing that over a bookkeeping failure would be the
            // worse outcome, so this reports the loss the way `remember` does.
            Err(e) => {
                p.note(
                    NoteKind::Workspace,
                    "Approved. The court may now change the project.",
                );
                p.note(
                    NoteKind::Failed,
                    format!(
                        "Could not record this approval under {}: {e}. The plan is \
                         approved regardless; only the ledger entry was lost.",
                        root.display()
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
        .ok_or_else(|| ServerFnError::new("That plan is no longer in the records."))
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

/// The system prompt a plan's model is given, rendered exactly as a turn would
/// send it.
///
/// Goes through the same [`crate::llm::SystemPrompt::assemble`] and `render`
/// that [`converse`] uses, with the plan's own workspace, permissions and
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

/// The id shared by every call in one reply.
///
/// Scoped to the *turn* as well as the round, and that is the whole point of it
/// being a function. `round` restarts at zero each time the user sets a plan
/// going again, so an id built from the round alone repeats within one plan --
/// and [`crate::llm`]'s replay groups *consecutive* calls that share one.
///
/// [`kingdom_core::Plan::turns`] filters notes out, so a turn that ended on a
/// note rather than a message leaves its last call directly adjacent to the
/// next turn's first. The `Failed` note left behind when the server stops
/// mid-turn is exactly that shape, and it is common: the user says "keep
/// going", the new turn opens at round 0, and it collides with whatever the
/// dead turn ended on. Two separate decisions then replay as one assistant
/// message -- and the second's thinking is dropped, because only the first call
/// of a batch carries any.
#[cfg(feature = "ssr")]
fn batch_id(plan: &PlanId, turn: &str, round: usize) -> String {
    format!("{}-{turn}-{round}", plan.as_str())
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
            !names.contains(&"target") && !names.contains(&"node_modules"),
            "build detritus is hidden"
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

    /// Paths in a listing are relative to the city root, so the entry the King
    /// clicks is the one that can be listed a level down without the browser
    /// ever knowing an absolute path.
    #[test]
    fn a_nested_listing_names_its_entries_from_the_city_root() {
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
    fn a_path_that_leaves_the_city_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();

        for escape in ["../..", "src/../../etc", "/etc"] {
            assert!(
                read_directory(dir.path(), escape).is_err(),
                "{escape} leaves the city and must be refused"
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

    /// Two turns must not be able to claim the same batch id.
    ///
    /// The replay groups consecutive calls that share one, and `round` starts
    /// again at zero on every turn -- so without the turn in the id, the first
    /// call after "keep going" collides with the last call of the turn that
    /// died. The two decisions merge into one assistant message and the later
    /// one's thinking is discarded. See [`batch_id`].
    #[test]
    fn a_batch_id_is_not_reused_by_the_next_turn() {
        let plan = PlanId::new("plan-7");

        // The shape that bit: a turn that ended at round 0, then a new turn
        // opening at round 0 after the user said "keep going".
        assert_ne!(
            batch_id(&plan, "turn-a", 0),
            batch_id(&plan, "turn-b", 0),
            "round 0 of one turn must not be round 0 of the next"
        );

        // Within one turn the round still separates replies, or every call a
        // turn ever made would replay as a single vast decision.
        assert_ne!(
            batch_id(&plan, "turn-a", 0),
            batch_id(&plan, "turn-a", 1),
            "two replies in one turn are still two decisions"
        );

        // And calls from one reply still share an id, which is the grouping the
        // whole mechanism exists for.
        assert_eq!(batch_id(&plan, "turn-a", 3), batch_id(&plan, "turn-a", 3));
    }

    fn a_plan() -> Plan {
        Plan::opened(
            PlanId::new("plan-1"),
            kingdom_core::CityId::new("c1"),
            "Fix the parser",
            &kingdom_core::ModelChoice::new("mock", None),
            kingdom_core::Workspace::in_place("/dev/testburg"),
        )
    }

    fn said(plan: &Plan) -> Vec<&str> {
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

    /// A turn can fail with words still waiting -- `converse` deliberately does
    /// not drain on its failure exits. Those words must not then be overtaken
    /// by whatever the user says next, or the court reads his instructions in
    /// an order he never gave them.
    #[test]
    fn words_left_over_from_a_failed_turn_are_heard_before_newer_ones() {
        let mut plan = a_plan();
        plan.working_on = Some("thinking".into());

        receive(&mut plan, "first, while it worked".into(), true);
        receive(&mut plan, "second, while it worked".into(), true);

        // The turn dies without draining, as a failed one does.
        plan.working_on = None;
        plan.status = kingdom_core::PlanStatus::Failed;

        receive(&mut plan, "third, after it stopped".into(), false);

        let said = said(&plan);
        let ordered: Vec<&str> = said
            .into_iter()
            .filter(|body| body.contains("while it worked") || body.contains("after it stopped"))
            .collect();
        assert_eq!(
            ordered,
            vec![
                "first, while it worked",
                "second, while it worked",
                "third, after it stopped",
            ]
        );
        assert!(plan.queued.is_empty());
    }

    /// What a halt must leave behind, and the one thing it must not.
    ///
    /// The status is the judgement worth pinning: `Failed` would paint a
    /// deliberate act in the colour of a breakage, and it is also the status
    /// the conversation offers a retry against -- so a stopped plan would
    /// invite the user to restart the thing he just stopped.
    ///
    /// The deed must be settled for a harder reason: `Plan::turns` replays
    /// in-flight calls to the model on every later round, so a call left open
    /// is one the court waits on forever, in every turn the plan ever takes
    /// again.
    #[test]
    fn a_halt_closes_the_deed_it_interrupted_without_calling_it_a_failure() {
        use kingdom_core::{Entry, NoteKind, PlanStatus, ToolCall, Turn};

        let mut plan = a_plan();
        plan.working_on = Some("bash: cargo test".into());
        plan.begin_tool_call(ToolCall::started(
            "call-1",
            "bash",
            serde_json::json!({ "cmd": "cargo test" }),
        ));

        halted(&mut plan, Some("call-1"));

        assert_eq!(
            plan.status,
            PlanStatus::AwaitingReview,
            "the King stopped this on purpose -- it did not fail"
        );
        assert!(!plan.is_busy(), "the busy mark must go, or nothing can restart");
        assert!(
            plan.turns()
                .all(|t| !matches!(t, Turn::Tool(d) if d.in_flight())),
            "an unsettled call is replayed to the model as still running, forever"
        );
        assert!(
            plan.transcript
                .iter()
                .any(|e| matches!(e, Entry::Note(n) if n.kind == NoteKind::Stopped)),
            "the King must be able to see why the court stopped"
        );
    }

    /// A halt between deeds has no call to close, and must still park the plan
    /// rather than falling through to a no-op. This is the path taken when Stop
    /// lands while the model is thinking.
    #[test]
    fn a_halt_with_no_deed_in_flight_still_parks_the_plan() {
        let mut plan = a_plan();
        plan.working_on = Some("thinking".into());

        halted(&mut plan, None);

        assert_eq!(plan.status, kingdom_core::PlanStatus::AwaitingReview);
        assert!(!plan.is_busy());
    }

    /// Stopping does not throw away what the King had already queued. The two
    /// are deliberately independent: he may have stopped the court precisely
    /// *because* of what he typed, and discarding it would lose the correction
    /// along with the work.
    #[test]
    fn a_halt_leaves_queued_words_waiting() {
        let mut plan = a_plan();
        plan.working_on = Some("thinking".into());
        plan.queue("stop and do this instead");

        halted(&mut plan, None);

        assert_eq!(plan.queued.len(), 1);

        // And the next thing he says flushes them, in order.
        receive(&mut plan, "here is the rest of it".into(), false);
        let ordered: Vec<&str> = said(&plan)
            .into_iter()
            .filter(|b| b.contains("instead") || b.contains("the rest"))
            .collect();
        assert_eq!(
            ordered,
            vec!["stop and do this instead", "here is the rest of it"]
        );
    }
}
