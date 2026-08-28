//! Shared services: one container, shared by every plan in a city.
//!
//! Shown to the King as "the well"; called a shared service everywhere the
//! compiler reads, per AGENTS.md.
//!
//! # The problem
//!
//! [`crate::namespaces`] answers *"stop these agents colliding on a port"*. This
//! answers the question immediately behind it: **some resources are meant to be
//! shared.** Five plans on a project that needs MongoDB should reach one
//! MongoDB -- started once when the first plan wants it, stopped once when the
//! last plan is done -- not five, and not one by accident.
//!
//! # The invariant, and the one door to it
//!
//! > A well stands exactly while at least one **live, non-subagent plan that
//! > can reach it** exists.
//!
//! [`reconcile`] is the only thing in this module that starts or stops
//! anything. It is handed the whole live population and makes that sentence
//! true: every well those agents can reach is up, raised **once per scope**,
//! and every well nobody is left drawing from is stopped. Everything else here
//! reports.
//!
//! It is called at the four moments the population changes -- a kingdom opens,
//! a plan opens, a plan is finished, a kingdom closes. Raising and stopping
//! being one pass over one input is what stops them drifting into disagreeing:
//! the older shape, an `ensure` per plan on one side and a `release` per plan
//! on the other, had no single place where the invariant was stated.
//!
//! Taking a turn and opening a shell deliberately do **not** raise anything.
//! They call [`require`], which waits for a pass already in flight and then
//! refuses if a promised well is missing. Opening a terminal is not a reason to
//! start a database.
//!
//! # The measurement the whole design rests on
//!
//! A plan's namespace **can already reach a container by its bridge address**.
//! `slirp4netns` runs with `--disable-host-loopback`, which blocks `127.0.0.1`
//! and nothing else; every other address routes out through the host's stack,
//! and a Docker bridge is just another host route. Measured from inside a real
//! namespace before this module was written:
//!
//! ```text
//!   namespace -> 172.17.0.2:5432    (default bridge)     reachable
//!   namespace -> 172.31.77.10:27017 (custom subnet)      reachable
//!   namespace -> 127.0.0.1:47777    (a published port)   REFUSED
//! ```
//!
//! So the obvious design is the one that cannot work. Publishing the container
//! to the host and pointing plans at `127.0.0.1` is exactly the third line.
//! Instead every service gets a **fixed address on a Kingdom-owned network**,
//! and that address is handed to plans as an environment variable.
//!
//! # The host needs nothing built
//!
//! `docker network create --subnet ...` installs a host route for the subnet
//! via its own `br-*` interface, so the King's own machine can open the
//! service's address directly. Nothing is published on his loopback, so the
//! service takes no port from him -- but it is routable, and [`address_of`] is how
//! the UI tells him where. An in-process TCP proxy was drafted for this and
//! deleted: it would have re-solved a problem the kernel had already solved.
//!
//! # Why it differs from `namespaces/` in one important way
//!
//! `namespaces::net::reclaim_previous` **kills** what a previous server left behind,
//! because a namespace with no server attached is worthless. A database is not:
//! it holds state. So on a restart this module **adopts** the containers it
//! finds still carrying its labels rather than killing them, and a plan that
//! comes back finds its data where it left it.
//!
//! The same reasoning bounds [`reconcile`]'s sweep: it never stops a container
//! this process did not raise. At boot the registry is empty, so a container
//! left standing by a previous server is adopted if an agent needs it and left
//! alone otherwise -- stopping it would be killing a database on the strength
//! of a label, having never spoken to whoever started it.
//!
//! # Two levels, one mechanism
//!
//! A well is declared either by a **project**, in its own committed manifest,
//! or by the **King**, in his profile -- and the second is offered to every
//! project he opens. See [`Scope`]. Nothing below distinguishes them beyond the
//! key: same network-per-key, same derived address, same reference count, same
//! adopt-on-restart. That is deliberate. A host well that behaved differently
//! from a city well would be a second feature wearing the first one's clothes.
//!
//! # What this is not
//!
//! **Not a sandbox.** A container Kingdom starts is an ordinary container,
//! visible to the whole machine and to `docker ps`, and a plan can still run
//! `docker` itself and do as it likes. Like [`crate::namespaces`], this is
//! coordination, not containment, and saying so plainly is worth more than a
//! guarantee that does not hold.
//!
//! # Where the runtime lives
//!
//! Not here. This module holds what is true of *sharing* -- the reference
//! count, the two scopes, [`reconcile`]'s invariant, the ledger, the writer --
//! and [`docker`] holds the conversation with a daemon. A resource declares its
//! [`kingdom_core::services::ResourceKind`] and this module matches on it,
//! which is how [`raise`], [`stop`] and [`trouble_with_runtimes`] each reach a
//! runtime without knowing what a container is.

pub mod docker;

use kingdom_core::services::{
    ManifestTrouble, ResourceInventory, ResourceKind, ServiceManifest, ServiceScope, ServiceSpec,
    ServiceState, SharedResource,
};
use kingdom_core::{Kingdom, PlanId};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

/// The name a plan reaches its own wells by, once they are relayed onto its
/// loopback.
///
/// Spelled `localhost` rather than `127.0.0.1` because every consumer of it is
/// now **prose**: a system prompt, the ports badge, the ledger. It used to be
/// substituted into a connection string in a manifest, where a name would have
/// put the plan at the mercy of its resolver -- that substitution is gone, and
/// with it the reason. `localhost` is the word every model and every person
/// reaches for, which is the whole point of putting the service there.
const LOOPBACK: &str = "localhost";

/// One resource that is up, and where to reach it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunningService {
    /// The service's name from the manifest.
    pub name: String,
    /// The one line saying what is running -- an image, for a container.
    ///
    /// Taken from the kind at raise time rather than looked up again, so every
    /// surface that prints it -- the badge, the prompt, the map -- says the
    /// same thing without knowing what kind of thing this is.
    pub what: String,
    /// Its address on the runtime's network -- an IP, because neither the host
    /// nor a plan's namespace can resolve Docker's service names.
    pub host: String,
    /// The port it listens on.
    pub port: u16,
    /// What the runtime calls it, which is also how it is found again.
    ///
    /// A container's name today. Named for what it *does* -- the string the
    /// runtime is handed to identify this thing -- rather than for the one kind
    /// there is.
    pub handle: String,
    /// Which kind of resource this is, as its wire name.
    ///
    /// Carried so that a resource can be stopped, or asked about, without
    /// re-reading the manifest that declared it: [`stop`] matches on this. A
    /// `&'static str` rather than a `ResourceKind` because the payload is the
    /// *declaration*, and what is running has already consumed it.
    pub kind: &'static str,
    /// Which level it was declared at.
    ///
    /// Carried on the running service rather than looked up again, because the
    /// two places that show a well to the King -- the badge and the ledger --
    /// both have to say whether it is this project's or the machine's, and
    /// re-deriving it from the handle would be reading a string back out of a
    /// string.
    pub scope: ServiceScope,
    /// The registry key it is filed under: `host`, or the city's key.
    pub key: String,
}

impl RunningService {
    /// What the King copies to reach it himself: `172.31.4.10:27017`.
    pub fn address(&self) -> String {
        format!("{}:{}", self.host, self.port)
    }
}

/// Every service this server has standing, and which plans are using it.
///
/// Process-global for the reason `namespaces::NAMESPACES` is: a container must
/// outlive the tool call that started it, because the point is that the *next*
/// plan finds it already up.
static SERVICES: OnceLock<Mutex<Registry>> = OnceLock::new();

/// The registry key every host service is filed under.
///
/// A reserved word in the same namespace city keys live in, and safe there
/// because a city key always carries a `-<8 hex digits>` suffix -- so no
/// project, however it is named, can produce the string `host`.
const HOST_KEY: &str = "host";

/// Which level a well is declared and shared at, with the paths that follow
/// from it.
///
/// The one seam between the two kinds. Everything downstream -- the network,
/// the container name, the subnet, the reference count -- is a function of
/// [`Scope::key`], so adding the host level meant naming this rather than
/// branching in six places.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Scope {
    /// The King's machine, declared in his profile.
    Host,
    /// One project, declared in its own repository. Carries the city's root.
    City(PathBuf),
}

impl Scope {
    /// The key this scope's services are filed under.
    pub fn key(&self) -> String {
        match self {
            Scope::Host => HOST_KEY.to_string(),
            Scope::City(root) => city_key(root),
        }
    }

    /// The manifest this scope reads and the form writes.
    ///
    /// The host's is `$KINGDOM_HOME/services.toml`, which is what makes a
    /// rehearsal honest: `tools::child_environment` points a plan working on
    /// Kingdom at a `KINGDOM_HOME` inside its own workspace, so it declares and
    /// sees its own host wells rather than the King's.
    pub fn manifest_path(&self) -> PathBuf {
        match self {
            Scope::Host => crate::profile::home().join(kingdom_core::services::HOST_MANIFEST_FILE),
            Scope::City(root) => root.join(kingdom_core::services::MANIFEST_PATH),
        }
    }

    /// Which of the two kinds this is, for the wire.
    pub fn kind(&self) -> ServiceScope {
        match self {
            Scope::Host => ServiceScope::Host,
            Scope::City(_) => ServiceScope::City,
        }
    }
}

#[derive(Default)]
pub(super) struct Registry {
    /// `(scope key, service name)` -> the running service.
    pub(super) running: HashMap<(String, String), RunningService>,
    /// `(scope key, service name)` -> the plans using it.
    ///
    /// This is the reference count, kept as the set of plan ids rather than an
    /// integer so that a plan closed twice cannot decrement it twice -- which
    /// would stop a database five other plans were still using.
    ///
    /// A host service's set spans **every** city, which is the whole difference
    /// the scope makes: it is stopped when the last plan anywhere lets go, not
    /// when the last plan in one project does.
    pub(super) users: HashMap<(String, String), HashSet<PlanId>>,
}

pub(super) fn registry() -> std::sync::MutexGuard<'static, Registry> {
    let cell = SERVICES.get_or_init(|| Mutex::new(Registry::default()));
    match cell.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

/// Held for the length of one raise-or-stop pass, so the conversation with
/// Docker happens **once**.
///
/// Distinct from [`SERVICES`] and deliberately a different kind of lock. The
/// registry is a `std::sync::Mutex` because every read of it is synchronous and
/// instant; this is a `tokio::sync::Mutex` because the section it guards is
/// full of awaits -- `docker run`, and a wait for a port that is allowed to take
/// three minutes.
///
/// What it prevents is concrete: a kingdom opening, a turn beginning and a shell
/// being opened can all ask for the same well within a second of each other. Two
/// of them inside `ensure_one` at once means two `docker run`s for one container
/// name, and the loser gets a bare "name already in use" that reads as a
/// Kingdom bug. Under this, the second caller waits and then finds the container
/// standing, which is the answer it wanted anyway.
///
/// **The registry guard is never held across an await inside this section.**
/// Desired state is read, the guard dropped, Docker asked, and the guard
/// retaken to record -- the discipline the rest of this module already keeps.
static RAISING: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();

async fn raising() -> tokio::sync::MutexGuard<'static, ()> {
    RAISING
        .get_or_init(|| tokio::sync::Mutex::new(()))
        .lock()
        .await
}

/// Why a city's services could not be raised.
///
/// Every variant is written for the King, because he is the one who acts on it.
///
/// The two that used to name Docker no longer do: which runtime is missing, and
/// what to do about it, is a sentence the runtime writes -- see
/// [`docker::unavailable`]. The variants here carry it rather than composing
/// it, so a second kind cannot produce an error advising the King to start a
/// daemon it does not use.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ServiceError {
    /// The runtime a declared resource needs is missing, or not answering.
    #[error("{0}")]
    Unavailable(String),

    #[error("{0}")]
    Manifest(String),

    #[error("the shared service `{name}` could not be started: {detail}")]
    Failed { name: String, detail: String },

    /// Started, but never answered on its port.
    ///
    /// `hint` is how the King looks at it himself -- `docker logs kingdom-...`
    /// for a container -- and comes from the runtime for the same reason the
    /// sentence above does.
    #[error(
        "the shared service `{name}` started but never answered on port \
         {port}. Its log may say why: `{hint}`."
    )]
    NeverReady {
        name: String,
        port: u16,
        hint: String,
    },
}

/// Reads a scope's manifest, if it has one.
///
/// A missing file is `Ok(empty)` rather than an error: almost every project has
/// no manifest and most machines have no host one, and neither is a fault to
/// report.
pub fn manifest_in(scope: &Scope) -> Result<ServiceManifest, ServiceError> {
    let path = scope.manifest_path();
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Ok(ServiceManifest::default());
    };
    // The path is attached **here** rather than inside `ManifestError`, because
    // this is the only layer that knows which of the two manifests was being
    // read. `kingdom-core` does no I/O and cannot say, and a message that
    // guessed named a project's file for a fault in the King's profile.
    //
    // Joined with a colon rather than "is": the errors now begin with a subject
    // of their own -- "service `db` sets `env`..." -- and "<path> is service
    // `db` sets `env`" is not a sentence. A colon reads correctly before both
    // that and the older "not valid TOML: ...".
    kingdom_core::services::parse(&text)
        .map_err(|e| ServiceError::Manifest(format!("{}: {e}", path.display())))
}

/// Reads a city's manifest, if it has one.
///
/// Kept as its own name because it reads at every call site as the question
/// being asked -- "what does this project declare?" -- and because the city is
/// the common case by a wide margin.
pub fn manifest_of(city_root: &Path) -> Result<ServiceManifest, ServiceError> {
    manifest_in(&Scope::City(city_root.to_path_buf()))
}

/// Every folder a sealed plan working in this city may see, from both scopes.
///
/// # Why this reads both manifests and starts nothing
///
/// A mount is not a resource in the sense the rest of this module means: there
/// is no container, no daemon, no reference count and nothing to raise or
/// release. It is read once, when the plan's namespace is built, and is inert
/// thereafter -- which is why it does not go anywhere near `reconcile`.
///
/// Host first and then the city, the order [`scopes_for`] fixes, so a project's
/// own declaration comes after the machine's. Duplicates are dropped keeping
/// the **more permissive** mode: a folder the machine shares read-only and the
/// project needs to write is one folder, and mounting it twice at the same path
/// would leave whichever landed second silently on top.
///
/// A manifest that cannot be read yields nothing rather than failing. The King
/// is already told about a broken manifest on the resources screen and by
/// `require`, and refusing to open a namespace over it would take a working
/// plan away for a fault it has already been told about twice.
pub fn mounts_for(city_root: Option<&Path>) -> Vec<kingdom_core::services::MountSpec> {
    use kingdom_core::services::MountMode;

    let scopes: Vec<Scope> = match city_root {
        Some(city) => scopes_for(city).to_vec(),
        // A plan with no resolvable city still gets the machine's own folders:
        // they are a fact about the King's toolchain, not about his project.
        None => vec![Scope::Host],
    };

    let mut out: Vec<kingdom_core::services::MountSpec> = Vec::new();
    for scope in scopes {
        let Ok(manifest) = manifest_in(&scope) else {
            continue;
        };
        for mount in manifest.mounts {
            match out.iter_mut().find(|held| held.path == mount.path) {
                // Already named. Keep the more permissive of the two, because
                // the stricter one would break whatever needed to write.
                Some(held) => {
                    if mount.mode.is_writable() {
                        held.mode = MountMode::Rw;
                    }
                }
                None => out.push(mount),
            }
        }
    }
    out
}

/// The two scopes a plan working in this city draws from, host first.
///
/// Host first is load-bearing in exactly one place: [`environment`] lets a
/// later scope overwrite an earlier one's variable, so this order is what makes
/// a project's own declaration win over the machine's. The more specific
/// statement is the one the project meant.
fn scopes_for(city_root: &Path) -> [Scope; 2] {
    [Scope::Host, Scope::City(city_root.to_path_buf())]
}

/// Brings a city's wells up and records the plans drawing from them.
///
/// Private, and per **scope** rather than per plan: raising is now driven by
/// [`reconcile`], which has already grouped the live agents by the key they
/// share. Called once for a project five agents are working in, not five times.
///
/// Idempotent, which is what makes adopt-on-restart work: a service already
/// running is adopted rather than restarted, a stopped one is started with its
/// volume intact, and a plan already registered as a drawer stays exactly one
/// user.
///
/// # It does not put anything on a loopback
///
/// Relaying a well onto a plan's own `127.0.0.1` is per **plan** -- it needs
/// that plan's namespace, which does not exist until the plan takes a turn or
/// the King opens a shell in it. This runs when a *kingdom* opens, where no
/// namespace has been raised yet. [`require`] is the per-plan path and is
/// where [`crate::namespaces::net::open_wells`] is called from.
async fn raise(
    scope: &Scope,
    drawers: &HashSet<PlanId>,
) -> Result<Vec<RunningService>, ServiceError> {
    let manifest = manifest_in(scope)?;
    // `has_services`, not `is_empty`: a manifest that declares only folders for
    // a sealed plan needs no runtime at all, and asking for one here would
    // refuse to raise a project whose only declaration is a mount.
    if !manifest.has_services() {
        return Ok(Vec::new());
    }

    let key = scope.key();

    // Grouped by kind, each keeping its **position in the manifest**. The
    // position is load-bearing: an address is assigned from it, so a service
    // that moved group would silently change address and a plan would be handed
    // a database that is not its own. Grouping rather than looping one at a
    // time because a runtime's setup -- for Docker, creating the network and
    // choosing the `/24` -- is one thought for the whole scope, and doing it
    // per service would put that arithmetic in this function.
    let mut containers: Vec<(usize, &ServiceSpec)> = Vec::new();
    for (index, spec) in manifest.services.iter().enumerate() {
        match spec.kind {
            ResourceKind::Docker(_) => containers.push((index, spec)),
        }
    }

    let mut up = Vec::new();
    let mut failure = None;
    if !containers.is_empty() {
        let raised = docker::raise(scope, &key, &containers).await;
        up.extend(raised.up);
        failure = raised.failure;
    }

    // Recorded **before** any failure is returned. A resource that came up
    // before its neighbour failed is standing whether or not this call
    // succeeded, and one the registry never heard of would never be swept: the
    // sweep only stops what this process knows it raised.
    for service in &up {
        // Taken and dropped around each record, never held across an await.
        // See `RAISING`.
        let mut registry = registry();
        let id = (key.clone(), service.name.clone());
        registry.running.insert(id.clone(), service.clone());
        // **Replaced, not extended.** `reconcile` hands down the whole live
        // population for this scope, so that set is the answer rather than
        // an addition to it. Extending was a real bug while this was being
        // written: a plan that finished stayed in the count forever, and
        // `users_of` reported a database as busy that nobody was in.
        registry.users.insert(id, drawers.clone());
    }

    match failure {
        Some(e) => Err(e),
        None => Ok(up),
    }
}

/// Brings the kingdom's wells into line with the agents that are actually
/// alive.
///
/// **The one entry point that starts or stops anything.** Everything else in
/// this module reports. It is given the whole live population -- every
/// non-subagent plan still in play, with the root of the city it works in --
/// and makes exactly two things true:
///
/// - every well those agents can reach is **up**, raised once per scope;
/// - every well **nobody** is left drawing from is stopped.
///
/// # Why the whole population rather than one plan
///
/// Because the invariant is about the population, and a per-plan call cannot
/// state it. The four moments it changes -- a kingdom opens, a plan opens, a
/// plan is finished, a kingdom closes -- all call this with the current list,
/// so raising and stopping are computed by the same pass from the same input
/// and cannot drift into disagreeing.
///
/// It also makes "raise once" structural rather than incidental: five agents on
/// one project are grouped into one scope before Docker is asked anything.
///
/// # What it will not do
///
/// It never stops a container this process did not raise. At boot the registry
/// is empty, so a container left standing by a previous server is invisible
/// here and is left alone -- it is adopted if an agent needs it, and otherwise
/// untouched. Stopping it would mean killing a database on the strength of a
/// label, having never spoken to whoever started it.
///
/// # Failures
///
/// Reported to the log and skipped, per scope. This is not `turn.rs`'s
/// judgement and deliberately so: there, a missing daemon must fail the turn
/// rather than run an agent with no database. Here, refusing to open the
/// kingdom because Docker is down would take the King's whole map away over a
/// project he may not be working in. [`require`] is what still refuses, at the
/// moment it matters.
pub async fn reconcile(agents: Vec<(PlanId, PathBuf)>) {
    // One pass, one Docker conversation. Held for the whole of it so a turn
    // beginning underneath waits for this rather than racing it.
    let _guard = raising().await;

    // Who wants what, grouped by the key everything downstream is a function
    // of. A `BTreeMap` so the order is stable: two scopes raised in a
    // different order on two runs would make the logs unreadable for no gain.
    let mut wanted: BTreeMap<String, (Scope, HashSet<PlanId>)> = BTreeMap::new();
    for (plan, city_root) in agents {
        for scope in scopes_for(&city_root) {
            wanted
                .entry(scope.key())
                .or_insert_with(|| (scope, HashSet::new()))
                .1
                .insert(plan.clone());
        }
    }

    // Raised first, then the sweep. This order matters when a plan finishes in
    // a city another plan is still working in: the surviving agent's claim is
    // recorded before anything is considered orphaned, so a well is never
    // stopped and immediately started again.
    for (key, (scope, drawers)) in &wanted {
        if let Err(e) = raise(scope, drawers).await {
            leptos::logging::warn!("could not raise the shared resources for {key}: {e}");
        }
    }

    // Everything this process has standing that nobody in the population above
    // is drawing from. Collected under the guard and stopped outside it.
    let orphaned: Vec<RunningService> = {
        let mut registry = registry();
        let claimed: HashSet<&String> = wanted.keys().collect();

        let ids: Vec<(String, String)> = registry.running.keys().cloned().collect();
        let mut orphaned = Vec::new();
        for id in ids {
            if claimed.contains(&id.0) {
                continue;
            }
            registry.users.remove(&id);
            if let Some(service) = registry.running.remove(&id) {
                orphaned.push(service);
            }
        }
        orphaned
    };

    for service in orphaned {
        stop(&service).await;
    }
}

/// Stops one resource nobody is drawing from any more.
///
/// Stopped, never removed, and any stored data left exactly where it is: the
/// King's data is the whole reason the resource existed. The runtime is chosen
/// by the kind the resource was raised as, so this cannot stop a container
/// belonging to a resource that is not one.
///
/// Failures are ignored on purpose, as they were before this was its own
/// function: a resource that is already gone, or a daemon that has since
/// stopped answering, are both "it is not running", which is what was wanted.
async fn stop(service: &RunningService) {
    match service.kind {
        "docker" => docker::stop(service).await,
        // Not reachable while there is one kind, and deliberately not a panic:
        // a resource nobody can stop is a container left standing, which is far
        // better than a server that falls over holding five agents' databases.
        other => leptos::logging::warn!(
            "the shared resource `{}` is of kind `{other}`, which nothing here can stop",
            service.name
        ),
    }
}

/// Answers whether this plan's wells are up, waiting for a raise already in
/// flight.
///
/// What `turn.rs` and `terminal.rs` call. Neither of them raises anything any
/// more -- opening a shell is not a reason to start a database -- but both must
/// still **refuse** when a well the plan was promised is missing, which is the
/// promise `docs/shared-resources.md` makes: a project whose manifest is broken
/// refuses to start an agent rather than running one with no database and
/// saying nothing.
///
/// # Why it awaits rather than merely checking
///
/// Because a kingdom opens by *spawning* its reconcile, so a turn beginning a
/// second later would otherwise look at a half-raised city and refuse over a
/// well that was seconds from being up. Taking the same guard means this waits
/// for that pass and then reads a settled answer.
pub async fn require(plan: &PlanId, city_root: &Path) -> Result<(), ServiceError> {
    // Reads the manifests before waiting on anything: a city that declares
    // nothing -- the overwhelming majority -- is answered here, without
    // touching the guard or the daemon.
    let mut declared = Vec::new();
    for scope in scopes_for(city_root) {
        let manifest = manifest_in(&scope)?;
        // Only containers can be missing in a way this must refuse over; a
        // mount is not raised and cannot fail to be up.
        if manifest.has_services() {
            declared.push((scope, manifest));
        }
    }
    if declared.is_empty() {
        return Ok(());
    }

    // Whatever pass is in flight finishes first. Dropped immediately: this
    // reads state, it does not change it.
    drop(raising().await);

    let mut standing: Vec<(String, u16)> = Vec::new();
    for (scope, manifest) in declared {
        let key = scope.key();
        for spec in &manifest.services {
            let id = (key.clone(), spec.name.clone());
            let mut registry = registry();
            let Some(service) = registry.running.get(&id) else {
                return Err(ServiceError::Failed {
                    name: spec.name.clone(),
                    detail: "it is declared by this project but is not running. \
                             Kingdom raises it when an agent that needs it is \
                             open; the shared resources screen says why it is \
                             not up."
                        .to_string(),
                });
            };
            standing.push((service.host.clone(), service.port));
            // This plan is one of its drawers from here on. The well is
            // already up, so this is bookkeeping rather than a start -- it is
            // what stops the well being swept out from under a plan that
            // reached it between two reconciles.
            registry.users.entry(id).or_default().insert(plan.clone());
        }
    }

    // And onto this plan's own loopback, so the agent reaches its database at
    // `localhost:27017` rather than at an address it has to be taught.
    //
    // **Here rather than in `raise`**, which is where it lived when raising was
    // per-plan. A relay lives inside one plan's namespace, and `raise` now runs
    // when a *kingdom* opens -- before any namespace exists, and for a whole
    // city's worth of agents at once. This is the per-plan path, it runs after
    // the caller has raised the namespace, and both callers that need it
    // (`turn` and `terminal`) already route through here: the same "one
    // function nobody has to remember" argument that put it in `ensure`.
    //
    // Idempotent and a no-op for a shared-network plan, which has no loopback
    // of its own to put anything on -- see `namespaces::net::open_wells`.
    crate::namespaces::net::open_wells(plan, &standing).await;

    Ok(())
}

/// The address a plan reaches a shared service at.
///
/// # The whole promise, in one function
///
/// `localhost:<the service's own port>` when [`crate::namespaces::net::open_wells`] has a
/// relay standing for **that container** inside this plan's namespace, and the
/// container's own address otherwise. Every surface that tells anyone where a
/// shared resource is -- the system prompt, the ports badge, the ledger -- goes
/// through here, so none of them can promise something a different one denies.
///
/// # Why the plan decides and not the city
///
/// Five plans on one project can have four isolated and one on the machine's
/// network, and each must be told the address that is true where it is
/// standing. A plan with no namespace of its own gets the container address
/// because a relay for it would bind the King's *real* `127.0.0.1:5432` -- the
/// port collision this product exists to prevent, committed by the product.
///
/// # Why the container is matched and not the port
///
/// A loopback has one socket per port, and two services can want the same one:
/// the King's own Redis and a project's are both `:6379` by default. Only the
/// first gets the relay. Matching on the port alone -- which this once did --
/// told the second one `localhost:6379` as well, and sent its every read and
/// write into the first one's data. See [`crate::namespaces::net::Well`].
pub fn address_for(plan: &PlanId, service: &RunningService) -> String {
    let container = service.address();
    if crate::namespaces::net::wells_of(plan).contains(&container) {
        format!("{LOOPBACK}:{}", service.port)
    } else {
        container
    }
}

/// What a plan in this city has standing, for the badge and the system prompt.
///
/// Both scopes, because both are addresses the plan can actually use, and an
/// address nobody is shown is an address nobody can use. Ordered host first
/// then by name, so the list does not reshuffle between pushes.
pub fn running_in(city_root: &Path) -> Vec<RunningService> {
    let keys: Vec<String> = scopes_for(city_root).iter().map(Scope::key).collect();
    let registry = registry();
    let mut out: Vec<RunningService> = registry
        .running
        .iter()
        .filter(|((key, _), _)| keys.contains(key))
        .map(|(_, service)| service.clone())
        .collect();
    out.sort_by(|a, b| a.scope.cmp(&b.scope).then_with(|| a.name.cmp(&b.name)));
    out
}

/// How many plans are using a given service right now.
pub fn users_of(city_root: &Path, service: &str) -> usize {
    let key = (city_key(city_root), service.to_string());
    registry().users.get(&key).map_or(0, HashSet::len)
}

/// [`users_of`] for a service already resolved to its registry key.
///
/// The form the two callers that hold a [`RunningService`] want: a host well is
/// not filed under any city, so asking for it by city root would count zero and
/// report a shared database as used by nobody.
pub fn users_of_key(key: &str, service: &str) -> usize {
    let id = (key.to_string(), service.to_string());
    registry().users.get(&id).map_or(0, HashSet::len)
}

/// Whether one particular plan is drawing from a given service.
///
/// [`users_of_key`] counts the drawers; this asks whether a named one is among
/// them. The map needs the distinction: a channel is drawn from an agent to a
/// well it has actually reached for, and every plan in the city *could* reach
/// the database without any of them having done so. Drawing from the count
/// alone would claim five connections where there is one.
///
/// By registry **key** rather than by city root, which matters now that a well
/// can belong to the King's machine rather than to a project: a host well is
/// filed under `host`, so asking for it by city would answer "nobody is drawing
/// from this" for every agent actually connected to it. The caller holds a
/// [`RunningService`], which carries its own key.
///
/// Reads the same reference set [`reconcile`] maintains, so it is true at the
/// moment it is asked and makes no record of its own.
pub fn draws_from(key: &str, service: &str, plan: &PlanId) -> bool {
    let id = (key.to_string(), service.to_string());
    registry()
        .users
        .get(&id)
        .is_some_and(|users| users.contains(plan))
}

/// The address of one named service in this city's scope, or `None` if it is
/// not up.
pub fn address_of(city_root: &Path, service: &str) -> Option<String> {
    let key = (city_key(city_root), service.to_string());
    registry().running.get(&key).map(RunningService::address)
}

// ---------------------------------------------------------------------------
// The ledger: what is declared anywhere, and how a new one is declared
// ---------------------------------------------------------------------------

/// Every shared resource the King has declared, with what it is doing.
///
/// The whole screen in one call. Deliberately **not** per city: the question
/// the ledger answers is "what does this machine share, and with whom", and
/// answering it one project at a time would put the joining back on the
/// browser.
///
/// Cheap. It reads at most one small file per city plus one for the host, and
/// asks each runtime **at most one** question for the whole screen -- see
/// [`trouble_with_runtimes`]. No `docker inspect` per row: the registry already
/// knows what this server started, and a container some *other* server started
/// is not this ledger's business.
pub async fn inventory(kingdom: &Kingdom) -> ResourceInventory {
    let mut out = ResourceInventory::default();

    // Host first, so the machine's own wells head the list -- they are the ones
    // shared furthest, and so the ones a change to is most consequential.
    let mut scopes: Vec<(Scope, Option<&kingdom_core::City>)> = vec![(Scope::Host, None)];
    let root = Path::new(&kingdom.root);
    for city in &kingdom.cities {
        scopes.push((Scope::City(root.join(&city.path)), Some(city)));
    }

    // The manifests are read **before** any runtime is asked anything, so that
    // the question can be asked only of the runtimes something actually
    // declares. A machine that shares nothing now costs no subprocess at all --
    // it used to shell out to `docker version` to draw an empty screen.
    let mut declared: Vec<(Scope, Option<&kingdom_core::City>, String, ServiceManifest)> =
        Vec::new();
    let mut kinds: Vec<&'static str> = Vec::new();

    for (scope, city) in scopes {
        let path = scope.manifest_path();
        let shown_path = path.to_string_lossy().to_string();
        let manifest = match manifest_in(&scope) {
            Ok(manifest) => manifest,
            // A manifest that does not parse is the failure this ledger exists
            // to surface. Today it is silent until an agent's first turn is
            // refused, minutes in, with a message about the model.
            Err(e) => {
                out.troubles.push(ManifestTrouble {
                    scope: scope.kind(),
                    city_name: city.map(|c| c.name.clone()),
                    manifest_path: shown_path,
                    detail: e.to_string(),
                });
                continue;
            }
        };
        if !manifest.has_services() {
            continue;
        }
        for spec in &manifest.services {
            let kind = spec.kind.wire_name();
            if !kinds.contains(&kind) {
                kinds.push(kind);
            }
        }
        declared.push((scope, city, shown_path, manifest));
    }

    out.runtime_trouble = trouble_with_runtimes(&kinds).await;

    for (scope, city, shown_path, manifest) in &declared {
        let key = scope.key();
        for spec in &manifest.services {
            out.resources.push(describe(
                kingdom,
                scope,
                &key,
                *city,
                shown_path,
                spec,
                out.runtime_trouble.is_some(),
            ));
        }
    }

    out
}

/// Why nothing of the declared kinds could be running, or `None` if every
/// runtime that matters answered.
///
/// Asked once for the whole ledger rather than once per row: the answer is the
/// same for every one of them, and it is the difference between a screen of
/// confusing "not started"s and one banner saying the daemon is down.
///
/// Asked only of the kinds something actually declares, which is the whole
/// reason it takes a list. A King whose projects share nothing but a Postgres
/// should not be told that some other runtime he has never used is missing.
async fn trouble_with_runtimes(kinds: &[&'static str]) -> Option<String> {
    for kind in kinds {
        let trouble = match *kind {
            "docker" => docker::trouble().await,
            // A kind with no runtime behind it cannot be running, and saying so
            // is better than a row that reads "not started" forever.
            other => Some(format!(
                "a shared resource is declared as `{other}`, which Kingdom does \
                 not know how to run."
            )),
        };
        if trouble.is_some() {
            return trouble;
        }
    }
    None
}

/// One declared service, joined against what is actually running.
#[allow(clippy::too_many_arguments)]
fn describe(
    kingdom: &Kingdom,
    scope: &Scope,
    key: &str,
    city: Option<&kingdom_core::City>,
    manifest_path: &str,
    spec: &ServiceSpec,
    runtime_is_out: bool,
) -> SharedResource {
    let id = (key.to_string(), spec.name.clone());
    let (running, drawing) = {
        let registry = registry();
        (
            registry.running.get(&id).cloned(),
            registry.users.get(&id).cloned().unwrap_or_default(),
        )
    };

    // Titles rather than ids: "who else is in here?" is a question about
    // people's work, and a UUID does not answer it. Sorted, so the same three
    // plans do not reorder themselves between two loads of the screen.
    let mut users: Vec<String> = drawing
        .iter()
        .map(|plan| {
            kingdom
                .plan(plan)
                .map(|p| p.title.clone())
                .unwrap_or_else(|| plan.to_string())
        })
        .collect();
    users.sort();

    let state = match (&running, runtime_is_out) {
        (Some(_), _) => ServiceState::Running,
        // Not "idle": with no runtime answering, Kingdom genuinely does not know
        // whether this is up, and saying "not started" would be a guess dressed
        // as a fact.
        (None, true) => ServiceState::Unknown,
        (None, false) => ServiceState::Idle,
    };

    // What the runtime calls this thing, and how the King looks at it himself.
    // Derived rather than allocated, so both are known even for a resource that
    // has never run -- which is what makes `docker logs <name>` printable on a
    // row that is not up.
    let (handle, hint) = match &spec.kind {
        ResourceKind::Docker(_) => {
            let handle = docker::container_name(key, &spec.name);
            let hint = docker::log_hint(&handle);
            (handle, hint)
        }
    };

    // Filled in when the address is known. The resource's own address, which is
    // what the King reaches it at from his own machine -- a plan is told
    // `localhost` instead, per `address_for`, and the screen says both.
    SharedResource {
        spec: spec.clone(),
        scope: scope.kind(),
        city: city.map(|c| c.id.clone()),
        city_name: city.map(|c| c.name.clone()),
        manifest_path: manifest_path.to_string(),
        state,
        address: running.as_ref().map(RunningService::address),
        handle,
        hint,
        users,
    }
}

/// Why a resource could not be declared.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DeclareError {
    #[error("{0}")]
    Invalid(String),

    #[error(
        "`{name}` is already declared in {path}. Pick another name, or edit \
         that file to change the one that is there."
    )]
    Duplicate { name: String, path: String },

    #[error("{path} could not be written: {detail}")]
    Unwritable { path: String, detail: String },
}

/// Declares a new shared resource, by appending it to its scope's manifest.
///
/// # Why this appends text
///
/// The manifest is a file a person writes and comments. Parsing it into a
/// `ServiceManifest`, pushing an entry and re-serialising would silently eat
/// every comment in it as the price of adding one service -- including the
/// paragraph the `shopfront` fixture puts at the top explaining what Kingdom
/// does with the file. So the spec is rendered as one block and appended, and
/// everything already in the file is left exactly as the King typed it.
///
/// # What is checked before anything is written
///
/// The whole file **after** the addition is parsed. That is stronger than
/// validating the spec alone and it is the check that matters: it proves the
/// King is left with a manifest that still works, rather than one that parses
/// in isolation and collides with what was already there.
///
/// Returns the path written to, which is what the UI shows him next.
pub fn declare(scope: &Scope, spec: &ServiceSpec) -> Result<PathBuf, DeclareError> {
    let path = scope.manifest_path();
    let shown = path.to_string_lossy().to_string();

    let existing = std::fs::read_to_string(&path).unwrap_or_default();

    // Named first, because "that name is taken" is a better message than
    // whatever the parser says about a duplicate three lines later.
    if let Ok(current) = kingdom_core::services::parse(&existing) {
        if current.services.iter().any(|s| s.name == spec.name) {
            return Err(DeclareError::Duplicate {
                name: spec.name.clone(),
                path: shown,
            });
        }
    }

    let mut next = existing.clone();
    if !next.is_empty() && !next.ends_with('\n') {
        next.push('\n');
    }
    if !next.is_empty() {
        next.push('\n');
    }
    next.push_str(&spec.render());

    // The whole file, not just the new block. See the doc comment.
    kingdom_core::services::parse(&next).map_err(|e| DeclareError::Invalid(e.to_string()))?;

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| DeclareError::Unwritable {
            path: shown.clone(),
            detail: e.to_string(),
        })?;
    }
    std::fs::write(&path, next).map_err(|e| DeclareError::Unwritable {
        path: shown,
        detail: e.to_string(),
    })?;

    Ok(path)
}

/// What Kingdom offers to share with a sealed plan on this machine.
///
/// # Why this reads `PATH` rather than a list somebody wrote
///
/// A sealed plan gets a read-only system, and everything under `/usr` comes
/// with it -- which on an ordinary machine is `git`, `node`, `python3`, `rg`
/// and `docker`. What is *missing* is whatever the King installed under his own
/// home, and `PATH` is the only honest answer to "which tools do I have": it is
/// the list his own shell uses.
///
/// So `PATH` is split into entries already covered by a built-in mount and
/// entries that are not, and the remainder is offered. A recognised entry
/// brings the folders its tool actually needs (see
/// [`kingdom_core::services::known_path`]); an unrecognised one is still
/// offered, read-only -- a tool Kingdom has never heard of is still a tool he
/// has.
///
/// Folders that do not exist are never offered, so the list is what is really
/// there rather than a catalogue of what he might have.
pub fn mount_candidates(city_root: Option<&Path>) -> Vec<kingdom_core::services::MountCandidate> {
    let path = std::env::var_os("PATH").unwrap_or_default();
    let home = std::env::var("HOME").unwrap_or_default();
    candidates_from(
        &std::env::split_paths(&path).collect::<Vec<_>>(),
        &home,
        city_root,
    )
}

/// The same decision, with the machine's answers passed in.
///
/// Split out so it can be tested **without touching the process environment**.
/// AGENTS.md gives the rule and `llm::catalogue::default_id` sets the pattern;
/// the cost of ignoring it was measured here rather than argued about. An
/// earlier version of this set `PATH` for the duration of its own test, and
/// because `PATH` is process-global and the suite runs in parallel, an
/// unrelated test invoking `git` two modules away failed while it did -- a
/// failure that appeared in the full run, never on its own, and named a file
/// this change had not touched.
fn candidates_from(
    path_entries: &[PathBuf],
    home: &str,
    city_root: Option<&Path>,
) -> Vec<kingdom_core::services::MountCandidate> {
    let city = city_root
        .map(|root| declared_in(&Scope::City(root.to_path_buf())))
        .unwrap_or_default();
    candidates_with(path_entries, home, &declared_in(&Scope::Host), &city)
}

/// The folders one scope's manifest declares, or none if it cannot be read.
///
/// The per-scope half of [`mounts_for`], which merges the two. Kept apart
/// because the offer needs to know *which* file a folder came from -- see
/// [`kingdom_core::services::MountCandidate::declared`].
fn declared_in(scope: &Scope) -> Vec<kingdom_core::services::MountSpec> {
    manifest_in(scope).map(|m| m.mounts).unwrap_or_default()
}

/// Where a row of the offer came from.
///
/// The two differ in exactly two ways, and both are about honesty rather than
/// taste: a folder Kingdom *offers* must exist, because an offer it would
/// silently skip is worse than none; a folder already *declared* is shown
/// whether it exists or not, because a stale line the King wants to clear is
/// the one he most needs to see.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Origin {
    /// Found on `PATH`, or one of the well-known extras.
    Offered,
    /// Read out of a manifest, and so already shared.
    Declared,
}

/// The same decision again, with the manifests passed in as well.
///
/// Split one layer further than [`candidates_from`] for the reason that one was
/// split from [`mount_candidates`]: what a folder's checkbox *does* depends on
/// which manifest declared it, and a test of that must not depend on which
/// manifests the machine running it happens to have.
fn candidates_with(
    path_entries: &[PathBuf],
    home: &str,
    host_mounts: &[kingdom_core::services::MountSpec],
    city_mounts: &[kingdom_core::services::MountSpec],
) -> Vec<kingdom_core::services::MountCandidate> {
    use kingdom_core::services::{
        known_extras, known_path, MountCandidate, MountMode, MountSpec, ServiceScope,
    };

    let home = home.to_string();
    let expand = |path: &str| -> String {
        match path.strip_prefix('~') {
            Some(rest) => format!("{}{}", home.trim_end_matches('/'), rest),
            None => path.to_string(),
        }
    };
    // Where a folder is declared, by the path it actually resolves to -- so
    // `~/.cargo` in his profile and `/home/him/.cargo` in a project count as
    // the same folder rather than being offered twice.
    //
    // A project's declaration wins when both name it, because that is the
    // answer the checkbox has to act on: such a folder stays shared however the
    // King's own profile is edited, so offering to withdraw it here would be
    // offering something that would not happen.
    let declared_at = |path: &str| -> Option<ServiceScope> {
        let wanted = expand(path);
        if city_mounts.iter().any(|m| m.expanded(&home) == wanted) {
            return Some(ServiceScope::City);
        }
        if host_mounts.iter().any(|m| m.expanded(&home) == wanted) {
            return Some(ServiceScope::Host);
        }
        None
    };

    let mut out: Vec<MountCandidate> = Vec::new();
    let mut seen: Vec<String> = Vec::new();
    // Every folder any emitted row already names, expanded. What keeps a
    // declared-by-hand row from repeating a folder the offer above it covers.
    let mut covered: Vec<String> = Vec::new();

    let mut offer = |folders: Vec<MountSpec>, why: String, origin: Origin| {
        // Every folder must exist, or the offer is one that would be silently
        // skipped at mount time and is better not made.
        //
        // Not so for a folder already declared: a stale line in a manifest is
        // exactly the one the King most wants to see and clear, and dropping it
        // because the folder has since gone would leave it unremovable here.
        let live: Vec<MountSpec> = folders
            .into_iter()
            .filter(|m| origin == Origin::Declared || Path::new(&expand(&m.path)).exists())
            .collect();
        if live.is_empty() {
            return;
        }
        // A folder some offer above already names is not shown again: the row
        // that names the whole toolchain is the useful one, and a second row
        // for `~/.cargo` alone beneath it is the same decision twice.
        if origin == Origin::Declared && live.iter().all(|m| covered.contains(&expand(&m.path))) {
            return;
        }
        let key = live
            .iter()
            .map(|m| m.path.clone())
            .collect::<Vec<_>>()
            .join(",");
        if seen.contains(&key) {
            return;
        }
        seen.push(key);
        covered.extend(live.iter().map(|m| expand(&m.path)));
        // Shared only when *every* folder of the offer is: half a toolchain is
        // not a toolchain, and a box shown ticked for one would promise a
        // `cargo` that has no `~/.rustup`. Ticking such a row adds the missing
        // half, which `declare_mount` already allows by being idempotent.
        //
        // Of the scopes found, the *project* is the answer: one folder from a
        // committed manifest makes the whole offer one this panel may not undo.
        let scopes: Vec<Option<ServiceScope>> = live.iter().map(|m| declared_at(&m.path)).collect();
        let declared = match scopes.iter().all(|s| s.is_some()) {
            true if scopes.iter().any(|s| *s == Some(ServiceScope::City)) => {
                Some(ServiceScope::City)
            }
            true => Some(ServiceScope::Host),
            false => None,
        };
        out.push(MountCandidate {
            declared,
            folders: live,
            why,
        });
    };

    for entry in path_entries {
        let shown = entry.display().to_string();
        if shown.is_empty() || !entry.is_dir() {
            continue;
        }
        // Already inside something every sealed plan gets. `canonicalize` on
        // purpose: `/bin` is a symlink into `/usr` on every merged-usr machine,
        // and comparing the names alone would offer it as though it were
        // separate.
        let real = entry.canonicalize().unwrap_or_else(|_| entry.to_path_buf());
        if built_in_covers(&real) || not_worth_offering(&real) {
            continue;
        }

        match known_path(&shown) {
            Some(known) => offer(
                known
                    .folders
                    .iter()
                    .map(|(path, mode)| MountSpec {
                        path: (*path).to_string(),
                        mode: *mode,
                    })
                    .collect(),
                known.why.to_string(),
                Origin::Offered,
            ),
            // Unrecognised, and so offered exactly as found: read-only, and
            // named for itself because nothing more is known about it.
            None => offer(
                vec![MountSpec {
                    path: shown.clone(),
                    mode: MountMode::Ro,
                }],
                format!("On your PATH: {shown}"),
                Origin::Offered,
            ),
        }
    }

    // Configuration a binary's own folder never reveals -- a git identity is
    // not on `PATH` and no amount of reading it will find one.
    for (path, why, mode) in known_extras() {
        offer(
            vec![MountSpec {
                path: (*path).to_string(),
                mode: *mode,
            }],
            (*why).to_string(),
            Origin::Offered,
        );
    }

    // Anything already shared that none of the above would have named: a folder
    // written into a manifest by hand, or one whose tool has since left `PATH`.
    //
    // Without this the list is a set of checkboxes that cannot show, let alone
    // clear, part of what a sealed plan will actually see -- and this panel is
    // meant to be the truth about exactly that. Last, because these are the
    // unusual ones and the offers above are what he came to read.
    for mount in city_mounts.iter().chain(host_mounts.iter()) {
        offer(
            vec![mount.clone()],
            format!("Shared by hand: {}", mount.path),
            Origin::Declared,
        );
    }

    out
}

/// Whether a path is already inside what every sealed plan is given.
///
/// `/usr` and `/etc` are mounted for every sealed plan, so offering anything
/// beneath them would be offering a folder the plan already has.
fn built_in_covers(path: &Path) -> bool {
    ["/usr", "/etc", "/dev", "/proc"]
        .iter()
        .any(|root| path.starts_with(root))
}

/// Whether a `PATH` entry is not worth offering at all.
///
/// # Why this exists, measured rather than imagined
///
/// Running the offer on a real machine produced **twenty-five** entries under
/// `/mnt/c` -- `C:\Windows\system32`, PowerShell, NVIDIA's control panel,
/// VS Code, two copies of Node -- because WSL appends the whole Windows `PATH`
/// to the Linux one. Every one was a real directory and technically shareable;
/// together they buried `~/.cargo` and `~/.local/bin` under a page of noise,
/// and a list nobody will read is a feature nobody will use.
///
/// None of them can help a Linux plan build anything: they hold `.exe` files
/// that a sealed namespace cannot execute. Dropped rather than sorted lower,
/// because "lower down a list of thirty" is still unusable.
///
/// A folder the King genuinely wants and Kingdom skipped is still available to
/// him: `/resources` declares any path he names. This decides what to *offer*,
/// not what is allowed.
fn not_worth_offering(path: &Path) -> bool {
    let shown = path.to_string_lossy();
    // Windows, seen through WSL's drive mounts.
    shown.starts_with("/mnt/c/") || shown.starts_with("/mnt/") && shown.contains("/Windows")
}

/// Declares a folder a sealed plan may see, by appending it to a manifest.
///
/// The mount counterpart of [`declare`], and it appends text for exactly the
/// same reason: the manifest is a file people comment, and re-serialising the
/// document would eat every comment in it.
///
/// Idempotent by path. Quick-add offers several folders at once and a King who
/// presses twice, or who adds `~/.cargo` from one project having already added
/// it to his profile, should get one line and not two -- a duplicate is not an
/// error worth stopping him for, it is simply nothing to do.
pub fn declare_mount(
    scope: &Scope,
    spec: &kingdom_core::services::MountSpec,
) -> Result<PathBuf, DeclareError> {
    let path = scope.manifest_path();
    let shown = path.to_string_lossy().to_string();

    let existing = std::fs::read_to_string(&path).unwrap_or_default();

    if let Ok(current) = kingdom_core::services::parse(&existing) {
        if let Some(held) = current.mounts.iter().find(|m| m.path == spec.path) {
            // Already there in a mode at least as permissive: nothing to do.
            if held.mode == spec.mode || held.mode.is_writable() {
                return Ok(path);
            }
            // Declared read-only and now wanted writable. Refused rather than
            // silently appended, because two blocks naming one path is a file
            // whose meaning depends on which Kingdom read it first.
            return Err(DeclareError::Duplicate {
                name: spec.path.clone(),
                path: shown,
            });
        }
    }

    let mut next = existing.clone();
    if !next.is_empty() && !next.ends_with('\n') {
        next.push('\n');
    }
    if !next.is_empty() {
        next.push('\n');
    }
    next.push_str(&spec.render());

    // The whole file, not just the new block: the King must be left with a
    // manifest that still works, rather than one whose new line parses alone.
    kingdom_core::services::parse(&next).map_err(|e| DeclareError::Invalid(e.to_string()))?;

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| DeclareError::Unwritable {
            path: shown.clone(),
            detail: e.to_string(),
        })?;
    }
    std::fs::write(&path, next).map_err(|e| DeclareError::Unwritable {
        path: shown,
        detail: e.to_string(),
    })?;

    Ok(path)
}

/// Stops sharing a folder, by removing its block from a manifest.
///
/// The inverse of [`declare_mount`], and what lets the Files tab offer a
/// checkbox rather than a one-way button. Until this existed the only way to
/// stop sharing `~/.ssh` with sealed plans was to find the file and edit TOML
/// by hand -- a thing nobody does, and so a decision nobody revisits.
///
/// # Why it edits the text rather than re-serialising
///
/// The same reason [`declare_mount`] appends text: the manifest is a file
/// people comment, and turning it into a `ServiceManifest` and back would eat
/// every comment in it as the price of removing one block. So the block's own
/// lines are cut out and everything else is left exactly as it was written.
///
/// A comment sitting *above* the block is deliberately left behind. It reads as
/// an orphan, which is untidy -- and the alternative is a rule that cannot tell
/// "the note explaining this mount" from "the paragraph at the top of the file
/// explaining what Kingdom does with it", and would delete the second.
///
/// Idempotent: a path that is not there is nothing to do rather than an error,
/// which is what makes a double press on a checkbox harmless.
pub fn withdraw_mount(
    scope: &Scope,
    spec: &kingdom_core::services::MountSpec,
) -> Result<PathBuf, DeclareError> {
    let path = scope.manifest_path();
    let shown = path.to_string_lossy().to_string();

    let Ok(existing) = std::fs::read_to_string(&path) else {
        // No manifest at all: the folder is already not shared here.
        return Ok(path);
    };

    let Some(next) = without_mount(&existing, &spec.path) else {
        return Ok(path);
    };

    // The whole file, exactly as [`declare_mount`] checks it: the King must be
    // left with a manifest that still works. Finding out here costs a message;
    // finding out later costs a plan that will not open.
    kingdom_core::services::parse(&next).map_err(|e| DeclareError::Invalid(e.to_string()))?;

    std::fs::write(&path, next).map_err(|e| DeclareError::Unwritable {
        path: shown,
        detail: e.to_string(),
    })?;

    Ok(path)
}

/// A manifest's text with one `[[mount]]` block removed, or `None` if it names
/// no such folder.
///
/// Split out from [`withdraw_mount`] so the text surgery is testable without a
/// disk, and because it is the only part of the removal that can be subtly
/// wrong.
///
/// A block runs from its own header line to the next header line, so the
/// removal takes that block's keys with it and nothing else. *Which* block is
/// decided by *parsing the candidate*, not by matching the path as a string:
/// `path = "~/.cargo"` and `path = '~/.cargo'` are one folder written two ways,
/// and a string match would leave one of them behind.
fn without_mount(text: &str, wanted: &str) -> Option<String> {
    let lines: Vec<&str> = text.lines().collect();
    // Where each block begins. A table header is the only thing in a file this
    // shape that starts a line with `[`.
    let heads: Vec<usize> = lines
        .iter()
        .enumerate()
        .filter(|(_, line)| line.starts_with('['))
        .map(|(i, _)| i)
        .collect();

    for (nth, start) in heads.iter().copied().enumerate() {
        if lines[start].trim() != "[[mount]]" {
            continue;
        }
        // The block runs to the next header -- but not quite. The blank lines
        // and comments immediately before that header belong to *it*, not to
        // this block, and taking them would delete the note the King wrote
        // above a folder he is keeping.
        let next = heads.get(nth + 1).copied().unwrap_or(lines.len());
        let mut end = next;
        while end > start + 1 {
            let line = lines[end - 1].trim();
            if line.is_empty() || line.starts_with('#') {
                end -= 1;
            } else {
                break;
            }
        }
        let block = lines[start..end].join("\n");
        // A block that does not parse on its own is not one we can be sure
        // about, so it is left alone rather than guessed at.
        let Ok(parsed) = kingdom_core::services::parse(&block) else {
            continue;
        };
        if parsed.mounts.first().map(|m| m.path.as_str()) != Some(wanted) {
            continue;
        }

        let mut kept: Vec<&str> = Vec::with_capacity(lines.len());
        kept.extend_from_slice(&lines[..start]);
        kept.extend_from_slice(&lines[end..]);
        let mut out = kept.join("\n");
        // The blank lines that separated the block from its neighbours are now
        // doubled up. Left as one gap, so clearing three folders one at a time
        // does not leave a file that is mostly whitespace.
        while out.contains("\n\n\n") {
            out = out.replace("\n\n\n", "\n\n");
        }
        // And a file must not start with the gap that used to sit under the
        // block that has gone.
        let out = out.trim_start_matches('\n').trim_end().to_string();
        return Some(match out.is_empty() {
            true => String::new(),
            false => format!("{out}\n"),
        });
    }

    None
}

/// Finds an executable on `PATH`.
///
/// A copy of `namespaces::which` rather than a shared helper: it is four lines, and
/// the alternative is a utility module that exists to hold four lines.
fn which(program: &str) -> Option<std::path::PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(program))
        .find(|candidate| candidate.is_file())
}

/// A stable, filesystem-safe key for a city, derived from its root path.
///
/// Derived from the *path* rather than the city's name, because two projects
/// can share a name and must not share a database. The readable prefix is for
/// `docker ps`, where a person needs to know what a container belongs to; the
/// hash is what makes it unique.
pub fn city_key(city_root: &Path) -> String {
    let name = city_root
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "city".to_string());
    let readable: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .take(24)
        .collect();
    format!("{readable}-{:08x}", path_hash(city_root))
}

/// A stable hash of a path.
///
/// FNV-1a, written out rather than pulled in: `DefaultHasher` is explicitly not
/// guaranteed stable across Rust releases, and this value names containers that
/// must be findable again after an upgrade.
pub(super) fn path_hash(path: &Path) -> u32 {
    let mut hash: u32 = 0x811c_9dc5;
    for byte in path.to_string_lossy().as_bytes() {
        hash ^= *byte as u32;
        hash = hash.wrapping_mul(0x0100_0193);
    }
    hash
}

/// Records one running service, and writes the manifest that declares it.
///
/// Test-only, and `pub(crate)` because two modules need to set up the same
/// state: this module tests which address a plan is *handed*, and
/// `llm::system_prompt` tests which address a plan is *told*. Those two must
/// agree, and they cannot agree if each builds its own idea of a standing well.
///
/// Nothing here touches Docker: the container named does not exist, which is
/// exactly right for testing the half that decides what an address says.
///
/// Returns the running service, so a caller can ask [`address_for`] about the
/// very thing it just stood up rather than rebuilding it.
#[cfg(test)]
pub(crate) fn pretend_a_well_is_running(city_root: &Path, port: u16) -> RunningService {
    pretend_a_named_well_is_running(city_root, "db", "172.31.4.10", port)
}

/// [`pretend_a_well_is_running`] with the name and container address chosen.
///
/// Two services can want one port -- the King's Redis and a project's are both
/// `:6379` -- and telling them apart is the whole reason a relay records what
/// it reaches. That case needs two wells on one port at two addresses.
#[cfg(test)]
pub(crate) fn pretend_a_named_well_is_running(
    city_root: &Path,
    name: &str,
    host: &str,
    port: u16,
) -> RunningService {
    std::fs::create_dir_all(city_root.join(".kingdom")).expect("the city's folder");
    let existing = std::fs::read_to_string(city_root.join(kingdom_core::services::MANIFEST_PATH))
        .unwrap_or_default();
    std::fs::write(
        city_root.join(kingdom_core::services::MANIFEST_PATH),
        format!("{existing}[[service]]\nname = \"{name}\"\nimage = \"mongo:7\"\nport = {port}\n\n"),
    )
    .expect("the manifest");

    let key = city_key(city_root);
    let service = RunningService {
        name: name.to_string(),
        what: "mongo:7".to_string(),
        host: host.to_string(),
        port,
        handle: format!("kingdom-{key}-{name}"),
        kind: "docker",
        scope: ServiceScope::City,
        key: key.clone(),
    };
    registry()
        .running
        .insert((key, name.to_string()), service.clone());
    service
}

#[cfg(test)]
mod mount_offer_tests {
    use super::*;

    /// Nothing under `/usr` is offered, because every sealed plan already has
    /// it.
    ///
    /// This is most of `PATH` on an ordinary machine -- `git`, `node`,
    /// `python3`, `rg`, `docker` -- and offering it would bury the handful of
    /// folders that actually matter under a list of ones that do not.
    #[test]
    fn the_built_in_system_is_not_offered_again() {
        assert!(built_in_covers(Path::new("/usr/bin")));
        assert!(built_in_covers(Path::new("/usr/local/bin")));
        assert!(built_in_covers(Path::new("/etc/alternatives")));
        assert!(!built_in_covers(Path::new("/home/anyone/.cargo/bin")));
        assert!(!built_in_covers(Path::new("/opt/vendor/bin")));
    }

    /// A folder that is not there is not offered.
    ///
    /// An offer Kingdom cannot honour is worse than no offer: the mount would
    /// be skipped when the namespace is built and the King would be left
    /// believing a toolchain was shared.
    ///
    /// Takes its `PATH` as an argument rather than setting the real one. That
    /// is not fussiness -- the first version of this test *did* set it, and
    /// because `PATH` is process-global and the suite runs in parallel, a test
    /// two modules away that shells out to `git` failed while it did.
    #[test]
    fn only_folders_that_exist_are_offered() {
        let temp = std::env::temp_dir().join("kingdom-offer-test");
        let real = temp.join("real-bin");
        std::fs::create_dir_all(&real).unwrap();
        let absent = temp.join("absent-bin");
        let _ = std::fs::remove_dir_all(&absent);

        let offered = candidates_from(&[real.clone(), absent.clone()], "/home/nobody", None);
        let paths: Vec<String> = offered
            .iter()
            .flat_map(|c| c.folders.iter().map(|f| f.path.clone()))
            .collect();

        assert!(paths.contains(&real.display().to_string()));
        assert!(
            !paths.contains(&absent.display().to_string()),
            "an offer Kingdom would silently skip is worse than no offer"
        );

        let _ = std::fs::remove_dir_all(&temp);
    }

    /// Windows folders are not offered, because a Linux plan cannot use them.
    ///
    /// Found by running the offer on this machine rather than by reasoning:
    /// WSL appends the whole Windows `PATH`, which produced twenty-five entries
    /// -- system32, PowerShell, NVIDIA's control panel, two copies of Node --
    /// and buried `~/.cargo` under a page of `.exe` directories a sealed
    /// namespace cannot execute anyway. A list nobody will read is a feature
    /// nobody will use.
    #[test]
    fn windows_folders_are_not_offered() {
        assert!(not_worth_offering(Path::new("/mnt/c/Windows/system32")));
        assert!(not_worth_offering(Path::new("/mnt/c/Program Files/nodejs")));
        assert!(not_worth_offering(Path::new(
            "/mnt/d/Windows/System32/OpenSSH"
        )));

        // And nothing of the King's own Linux toolchain is caught by it.
        assert!(!not_worth_offering(Path::new("/home/anyone/.cargo/bin")));
        assert!(!not_worth_offering(Path::new("/opt/vendor/bin")));
        assert!(!not_worth_offering(Path::new("/mnt/data/projects/bin")));
    }

    /// A recognised entry brings the folders its tool needs, and an
    /// unrecognised one is still offered.
    #[test]
    fn a_known_toolchain_brings_its_companions() {
        let home = std::env::temp_dir().join("kingdom-offer-home");
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(home.join(".cargo/bin")).unwrap();
        std::fs::create_dir_all(home.join(".rustup")).unwrap();
        let odd = home.join("vendor/bin");
        std::fs::create_dir_all(&odd).unwrap();

        let offered = candidates_from(
            &[home.join(".cargo/bin"), odd.clone()],
            &home.display().to_string(),
            None,
        );

        let rust = offered
            .iter()
            .find(|c| c.why.contains("Rust"))
            .expect("cargo on PATH is recognised");
        let paths: Vec<&str> = rust.folders.iter().map(|f| f.path.as_str()).collect();
        assert!(paths.contains(&"~/.cargo"));
        assert!(
            paths.contains(&"~/.rustup"),
            "without it every build re-downloads the toolchain"
        );

        assert!(
            offered.iter().any(|c| c.why.contains("vendor/bin")),
            "a tool Kingdom has never heard of is still a tool the King has"
        );

        let _ = std::fs::remove_dir_all(&home);
    }

    /// An offer says *where* it is declared, not merely that it is.
    ///
    /// The distinction the Files tab's checkboxes are built on: a box may be
    /// unticked only when unticking it would do something, and this panel
    /// writes to the King's own profile alone. A folder a project declared
    /// lives in a committed file belonging to whoever else works on that
    /// repository -- shown, because the plan will see it, and not offered for
    /// removal.
    #[test]
    fn an_offer_says_which_manifest_declared_it() {
        use kingdom_core::services::{MountMode, MountSpec, ServiceScope};

        let home = std::env::temp_dir().join("kingdom-offer-scope-home");
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(home.join(".cargo/bin")).unwrap();
        std::fs::create_dir_all(home.join(".rustup")).unwrap();
        let shown = home.display().to_string();

        let mine = |path: &str, mode| MountSpec {
            path: path.to_string(),
            mode,
        };

        // Nothing declared anywhere: nothing to untick.
        let none = candidates_with(&[home.join(".cargo/bin")], &shown, &[], &[]);
        let rust = none.iter().find(|c| c.why.contains("Rust")).unwrap();
        assert_eq!(rust.declared, None);
        assert!(!rust.already());

        // Half of it declared is not declared: a `cargo` with no `~/.rustup`
        // re-downloads the toolchain, so a box ticked for it would be a
        // promise the mount cannot keep.
        let half = candidates_with(
            &[home.join(".cargo/bin")],
            &shown,
            &[mine("~/.cargo", MountMode::Rw)],
            &[],
        );
        let rust = half.iter().find(|c| c.why.contains("Rust")).unwrap();
        assert_eq!(rust.declared, None, "half a toolchain is not a toolchain");

        // All of it, in his profile: ticked, and his to untick.
        let host = candidates_with(
            &[home.join(".cargo/bin")],
            &shown,
            &[
                mine("~/.cargo", MountMode::Rw),
                mine("~/.rustup", MountMode::Rw),
            ],
            &[],
        );
        let rust = host.iter().find(|c| c.why.contains("Rust")).unwrap();
        assert_eq!(rust.declared, Some(ServiceScope::Host));
        assert!(rust.removable());

        // One folder from the project's own manifest makes the whole offer one
        // this panel may not undo.
        let city = candidates_with(
            &[home.join(".cargo/bin")],
            &shown,
            &[mine("~/.cargo", MountMode::Rw)],
            &[mine("~/.rustup", MountMode::Rw)],
        );
        let rust = city.iter().find(|c| c.why.contains("Rust")).unwrap();
        assert_eq!(rust.declared, Some(ServiceScope::City));
        assert!(rust.already(), "it is shared");
        assert!(!rust.removable(), "but not from here");

        let _ = std::fs::remove_dir_all(&home);
    }

    /// A folder shared by hand is listed even though nothing offers it.
    ///
    /// Otherwise the checkboxes are not the truth about what a sealed plan can
    /// see: a line typed into the manifest, or one whose tool has since left
    /// `PATH`, would be mounted and invisible here -- and so impossible to
    /// clear from the one screen that claims to say what a plan may see.
    ///
    /// Such a row is shown even when the folder is **gone**, which is the
    /// opposite of the rule for an offer, and deliberately: a stale line is
    /// exactly the one worth clearing.
    #[test]
    fn a_folder_shared_by_hand_is_still_listed() {
        use kingdom_core::services::{MountMode, MountSpec, ServiceScope};

        let home = std::env::temp_dir().join("kingdom-offer-hand-home");
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).unwrap();
        let shown = home.display().to_string();

        let offered = candidates_with(
            &[],
            &shown,
            &[MountSpec {
                path: "/opt/nowhere".to_string(),
                mode: MountMode::Ro,
            }],
            &[],
        );

        let hand = offered
            .iter()
            .find(|c| c.folders.iter().any(|f| f.path == "/opt/nowhere"))
            .expect("a folder the King shared must be listed, so he can unshare it");
        assert_eq!(hand.declared, Some(ServiceScope::Host));
        assert!(hand.removable());

        let _ = std::fs::remove_dir_all(&home);
    }

    /// A declared folder an offer already names is not listed twice.
    ///
    /// `~/.cargo` is both what the King declared and part of the Rust offer.
    /// Two rows for it would be the same decision asked twice, with the second
    /// one able to half-undo the first.
    #[test]
    fn a_declared_folder_does_not_repeat_an_offer() {
        use kingdom_core::services::{MountMode, MountSpec};

        let home = std::env::temp_dir().join("kingdom-offer-dup-home");
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(home.join(".cargo/bin")).unwrap();
        std::fs::create_dir_all(home.join(".rustup")).unwrap();
        let shown = home.display().to_string();

        let declared = vec![
            MountSpec {
                path: "~/.cargo".to_string(),
                mode: MountMode::Rw,
            },
            MountSpec {
                path: "~/.rustup".to_string(),
                mode: MountMode::Rw,
            },
        ];
        let offered = candidates_with(&[home.join(".cargo/bin")], &shown, &declared, &[]);

        let naming_cargo = offered
            .iter()
            .filter(|c| c.folders.iter().any(|f| f.path == "~/.cargo"))
            .count();
        assert_eq!(naming_cargo, 1, "one folder, one box");

        let _ = std::fs::remove_dir_all(&home);
    }
}

#[cfg(test)]
mod mount_removal_tests {
    use super::*;

    /// A withdrawal takes the block and leaves everything else, comments
    /// included.
    ///
    /// The whole reason this is text surgery rather than a serde round trip:
    /// the manifest is a file people write and comment, and re-serialising it
    /// would silently eat the paragraph at the top explaining what Kingdom does
    /// with the file.
    #[test]
    fn one_block_goes_and_the_rest_of_the_file_stays() {
        let before = r#"# What this machine lends to sealed plans.

[[service]]
name  = "db"
image = "mongo:7"
port  = 27017

[[mount]]
path = "~/.cargo"
mode = "rw"

[[mount]]
path = "~/.ssh"
mode = "ro"
"#;

        let after = without_mount(before, "~/.ssh").expect("it is in there");

        assert!(
            after.contains("# What this machine lends to sealed plans."),
            "a removal must not eat his comments"
        );
        assert!(after.contains("~/.cargo"), "the other folder stays");
        assert!(after.contains("mongo:7"), "and so does the service");
        assert!(!after.contains("~/.ssh"), "the withdrawn one is gone");

        // And what is left is still a manifest.
        let parsed = kingdom_core::services::parse(&after).expect("still valid");
        assert_eq!(parsed.mounts.len(), 1);
        assert_eq!(parsed.services.len(), 1);
    }

    /// A comment above the *next* block is not taken with this one.
    ///
    /// The block-to-next-header rule reads a trailing comment as part of the
    /// block above it, where a person reading the file sees a note about the
    /// folder below. Getting this wrong deletes the King's own words while
    /// removing a folder he never commented.
    #[test]
    fn a_note_above_the_next_folder_stays_with_it() {
        let before = r#"[[mount]]
path = "~/.cargo"
mode = "rw"

# Only because the deploy script needs it.
[[mount]]
path = "~/.ssh"
mode = "ro"
"#;

        let after = without_mount(before, "~/.cargo").expect("it is in there");

        assert!(
            after.contains("# Only because the deploy script needs it."),
            "the note belongs to the folder it sits above: {after:?}"
        );
        assert!(after.contains("~/.ssh"));
        assert!(!after.contains("~/.cargo"));
    }

    /// Withdrawing a folder that is not shared is nothing to do.
    ///
    /// What makes a double press on a checkbox harmless: `None` here becomes a
    /// successful no-op in [`withdraw_mount`], rather than an error about a
    /// state the King already wanted.
    #[test]
    fn a_folder_that_is_not_there_is_left_alone() {
        let text = "[[mount]]\npath = \"~/.cargo\"\nmode = \"rw\"\n";

        assert_eq!(without_mount(text, "~/.npm"), None);
        assert_eq!(without_mount("", "~/.cargo"), None);
    }

    /// The path is matched by *parsing* the block, not by finding the string.
    ///
    /// TOML has more than one way to write one string, and a King who typed
    /// single quotes -- or who put the two keys the other way round -- must not
    /// end up with a checkbox that unticks and then re-ticks itself because the
    /// block was never actually removed.
    #[test]
    fn a_block_is_matched_however_it_was_written() {
        let text = "[[mount]]\nmode = 'ro'\npath = '~/.ssh'\n";

        let after = without_mount(text, "~/.ssh").expect("single quotes are still that folder");
        assert!(after.trim().is_empty());
    }

    /// The last folder out leaves an empty file rather than a ragged one.
    #[test]
    fn removing_the_only_block_empties_the_file() {
        let text = "[[mount]]\npath = \"~/.cargo\"\nmode = \"rw\"\n";

        let after = without_mount(text, "~/.cargo").expect("it is in there");
        assert_eq!(after, "");
        assert!(kingdom_core::services::parse(&after).is_ok());
    }

    /// Withdrawing writes the file, and the next read no longer has it.
    ///
    /// The round trip, because the pieces can each be right and the whole still
    /// wrong: `declare_mount` appends and `withdraw_mount` cuts, and the proof
    /// that they agree is a folder declared and then withdrawn leaving what was
    /// there before.
    #[test]
    fn declaring_and_withdrawing_leave_the_manifest_as_it_was() {
        use kingdom_core::services::{MountMode, MountSpec};

        let root = std::env::temp_dir().join("kingdom-withdraw-city");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join(".kingdom")).unwrap();
        let scope = Scope::City(root.clone());
        let manifest = scope.manifest_path();
        std::fs::write(&manifest, "# kept\n").unwrap();

        let spec = MountSpec {
            path: "~/.cargo".to_string(),
            mode: MountMode::Rw,
        };

        declare_mount(&scope, &spec).expect("declaring");
        assert!(declared_in(&scope).iter().any(|m| m.path == "~/.cargo"));

        withdraw_mount(&scope, &spec).expect("withdrawing");
        assert!(
            !declared_in(&scope).iter().any(|m| m.path == "~/.cargo"),
            "a folder withdrawn is one a sealed plan no longer sees"
        );
        assert!(
            std::fs::read_to_string(&manifest)
                .unwrap()
                .contains("# kept"),
            "and his own words survive the round trip"
        );

        // Again, on a folder that is no longer there: harmless.
        withdraw_mount(&scope, &spec).expect("a second press does nothing");

        let _ = std::fs::remove_dir_all(&root);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One container declaration, as the tests below want it.
    ///
    /// A helper rather than a literal in each test: a spec is two nested types
    /// now, and what these tests are about is the scope and the reference
    /// count, never the nesting.
    fn container(name: &str, image: &str, port: u16, volume: Option<&str>) -> ServiceSpec {
        ServiceSpec {
            name: name.to_string(),
            port,
            kind: ResourceKind::Docker(kingdom_core::services::DockerSpec {
                image: image.to_string(),
                volume: volume.map(str::to_string),
            }),
        }
    }

    /// Two projects with the same folder name must not share a database.
    ///
    /// The realistic case is `~/work/api` and `~/scratch/api`, and quietly
    /// pointing both at one MongoDB would be this product causing exactly the
    /// collision it exists to prevent.
    #[test]
    fn two_cities_with_one_name_get_different_keys() {
        let a = city_key(Path::new("/home/king/work/api"));
        let b = city_key(Path::new("/home/king/scratch/api"));
        assert_ne!(a, b);
        // Both still readable in `docker ps`, which is half the point of the
        // prefix.
        assert!(a.starts_with("api-"), "{a}");
        assert!(b.starts_with("api-"), "{b}");
    }

    /// The same city keeps its key across restarts, or an adopted container
    /// could never be found again.
    #[test]
    fn a_citys_key_is_stable() {
        let path = Path::new("/home/king/work/shopfront");
        assert_eq!(city_key(path), city_key(path));
    }

    /// A plan closed twice must not decrement the count twice.
    ///
    /// The failure this prevents is the bad one: a double release stopping a
    /// database that four other plans are still using. A set rather than an
    /// integer makes it impossible rather than unlikely.
    #[tokio::test]
    async fn releasing_a_plan_twice_does_not_strand_the_others() {
        let city = "double-release-test".to_string();
        let id = (city.clone(), "db".to_string());
        let one = PlanId::new("plan-1");
        let two = PlanId::new("plan-2");

        {
            let mut registry = registry();
            registry.running.insert(
                id.clone(),
                RunningService {
                    name: "db".to_string(),
                    what: "mongo:7".to_string(),
                    host: "172.31.9.10".to_string(),
                    port: 27017,
                    handle: "kingdom-double-release-test-db".to_string(),
                    kind: "docker",
                    scope: ServiceScope::City,
                    key: city.clone(),
                },
            );
            registry
                .users
                .insert(id.clone(), [one.clone(), two.clone()].into_iter().collect());
        }

        // Plan one leaves, twice.
        for _ in 0..2 {
            let mut registry = registry();
            if let Some(users) = registry.users.get_mut(&id) {
                users.remove(&one);
            }
        }

        let registry = registry();
        assert_eq!(
            registry.users.get(&id).map(HashSet::len),
            Some(1),
            "plan two must still be counted as drawing"
        );
        assert!(
            registry.running.contains_key(&id),
            "the service must still be up while plan two is using it"
        );
    }

    /// A city with no manifest is not an error, and costs no subprocess.
    #[test]
    fn a_city_without_a_manifest_declares_nothing() {
        let empty = std::env::temp_dir().join("kingdom-no-manifest-test");
        let _ = std::fs::create_dir_all(&empty);
        let manifest = manifest_of(&empty).expect("a missing manifest is not a failure");
        assert!(manifest.is_empty());
        assert!(running_in(&empty).is_empty());
    }

    // -----------------------------------------------------------------------
    // Which address a plan is told. The container is one address and the
    // plan's own loopback is another, and which one a given plan gets is the
    // difference between `npm start` working and an agent hunting for a
    // database that is running perfectly well.
    // -----------------------------------------------------------------------

    /// Puts one running service in the registry, as `ensure` would have.
    ///
    /// Returns the city root it is filed under. Named for the city so two
    /// tests cannot collide in the process-global registry.
    fn a_running_well(city: &str, port: u16) -> (PathBuf, RunningService) {
        let root = std::env::temp_dir().join(city);
        let _ = std::fs::remove_dir_all(&root);
        let service = pretend_a_well_is_running(&root, port);
        (root, service)
    }

    /// A plan with the well on its own loopback is told to use it.
    ///
    /// This is the whole feature seen from the one place that decides it. The
    /// agent never reads this code -- it reads the address in its prompt, and
    /// what that says is what it will type.
    #[test]
    fn a_plan_with_the_well_on_its_loopback_is_given_localhost() {
        let (_root, service) = a_running_well("kingdom-loopback-well-test", 27017);
        let plan = PlanId::new("plan-with-its-own-network");
        crate::namespaces::net::pretend_wells_are_open(&plan, &["172.31.4.10:27017"]);

        assert_eq!(address_for(&plan, &service), "localhost:27017");

        crate::namespaces::net::forget_namespace(&plan);
    }

    /// A plan on the machine's network is told the container's address, exactly
    /// as before.
    ///
    /// Not a leftover: such a plan's `127.0.0.1` **is** the King's, so a relay
    /// there would take his real port. The awkward address is the correct
    /// answer for this plan, and the fallback if an isolated plan's relay fails.
    #[test]
    fn a_plan_on_the_machines_network_keeps_the_containers_address() {
        let (_root, service) = a_running_well("kingdom-shared-network-well-test", 27017);
        let plan = PlanId::new("plan-on-the-shared-network");
        crate::namespaces::net::forget_namespace(&plan);

        assert_eq!(address_for(&plan, &service), "172.31.4.10:27017");
    }

    /// A relay that did not come up must not be described as if it had.
    ///
    /// The failure this prevents is the worst one available here: a `localhost`
    /// address that refuses the connection reads as a broken database, and the
    /// agent goes looking for a fault in the project's own code. A working
    /// address that needed reading is a far smaller cost.
    #[test]
    fn a_well_whose_relay_never_bound_is_still_given_its_real_address() {
        let (_root, service) = a_running_well("kingdom-half-open-well-test", 27017);
        let plan = PlanId::new("plan-whose-relay-failed");
        // A namespace, and some *other* service relayed -- but not this well.
        crate::namespaces::net::pretend_wells_are_open(&plan, &["172.31.4.11:6379"]);

        assert_eq!(
            address_for(&plan, &service),
            "172.31.4.10:27017",
            "promising localhost where nothing is listening is worse than the IP"
        );

        crate::namespaces::net::forget_namespace(&plan);
    }

    /// Two services on one port: only the one actually relayed gets
    /// `localhost`.
    ///
    /// The ordinary shape of it is the King's own Redis and a project's, both
    /// `:6379` by default. A plan's loopback has one socket per port, so only
    /// the first can be relayed -- and this used to be decided by comparing
    /// *port numbers*, which told the second one `localhost:6379` as well. Its
    /// every read and write then landed silently in the first one's data, which
    /// is the worst failure this module can produce: not an error, a wrong
    /// database.
    #[test]
    fn a_second_service_on_the_same_port_is_not_told_it_is_local() {
        let root = std::env::temp_dir().join("kingdom-two-on-one-port-test");
        let _ = std::fs::remove_dir_all(&root);
        let relayed = pretend_a_named_well_is_running(&root, "cache", "172.31.4.10", 6379);
        let crowded_out = pretend_a_named_well_is_running(&root, "other", "172.31.9.10", 6379);

        let plan = PlanId::new("plan-with-two-caches");
        // Exactly what `open_wells` records: one relay, for one container.
        crate::namespaces::net::pretend_wells_are_open(&plan, &["172.31.4.10:6379"]);

        assert_eq!(
            address_for(&plan, &relayed),
            "localhost:6379",
            "the relayed service is on the loopback and must say so"
        );
        assert_eq!(
            address_for(&plan, &crowded_out),
            "172.31.9.10:6379",
            "the other service is a different database -- localhost would reach \
             the first one's data"
        );

        crate::namespaces::net::forget_namespace(&plan);
    }

    /// Reconciling a city that declares nothing touches nothing.
    ///
    /// The overwhelmingly common case, and now on the path of *every kingdom
    /// open*: if this cost a subprocess, opening a folder of twenty ordinary
    /// projects would shell out twenty times before the map appeared. It
    /// returns before `which("docker")` is ever reached.
    #[tokio::test]
    async fn reconciling_a_city_without_a_manifest_costs_nothing() {
        let home = tempfile::tempdir().expect("a temporary profile");
        let _profile = crate::profile::testing::Profile::at(home.path());
        let city = tempfile::tempdir().expect("a city that declares nothing");

        reconcile(vec![(PlanId::new("quiet-plan"), city.path().to_path_buf())]).await;

        assert!(
            running_in(city.path()).is_empty(),
            "a city with no manifest must have nothing standing"
        );
    }

    /// Closing a kingdom lets go of every well it was holding.
    ///
    /// An empty population is what `leave_kingdom` hands down, and it must mean
    /// "nobody is drawing from anything" rather than "nothing changed". Before
    /// this, wells stayed claimed for the life of the server by plans the King
    /// had closed.
    ///
    /// Registered directly rather than started, so no daemon is needed: the
    /// claim under test is the bookkeeping. `docker stop` on the way out is a
    /// no-op against a container that was never created.
    #[tokio::test]
    async fn closing_a_kingdom_lets_go_of_every_well() {
        let home = tempfile::tempdir().expect("a temporary profile");
        let _profile = crate::profile::testing::Profile::at(home.path());
        let city = tempfile::tempdir().expect("a temporary city");
        let key = city_key(city.path());
        let plan = PlanId::new("leaving-plan");

        {
            let mut registry = registry();
            let id = (key.clone(), "db".to_string());
            registry.running.insert(
                id.clone(),
                RunningService {
                    name: "db".to_string(),
                    what: "mongo:7".to_string(),
                    host: "172.31.9.10".to_string(),
                    port: 27017,
                    handle: "kingdom-leaving-test-db".to_string(),
                    kind: "docker",
                    scope: ServiceScope::City,
                    key: key.clone(),
                },
            );
            registry
                .users
                .insert(id, [plan.clone()].into_iter().collect());
        }

        reconcile(Vec::new()).await;

        assert_eq!(
            users_of(city.path(), "db"),
            0,
            "a closed kingdom's plans are drawing from nothing"
        );
        assert!(
            running_in(city.path()).is_empty(),
            "and the well is no longer held by this server"
        );
    }

    // -----------------------------------------------------------------------
    // The two levels, and the ledger over them. No daemon is needed for any of
    // these: they are about which file is read and written, which is the half
    // that actually decides how far a resource is shared.
    // -----------------------------------------------------------------------

    /// A host resource and a city one land in two different files.
    ///
    /// The single most consequential fact about the scope, and the one mistake
    /// worth making impossible: a project's database written into the King's
    /// profile would be offered to every other project he opens.
    #[test]
    fn the_two_scopes_write_to_two_different_files() {
        let dir = tempfile::tempdir().expect("a temporary profile");
        let _profile = crate::profile::testing::Profile::at(dir.path());

        let city = Path::new("/home/king/dev/shopfront");
        let host_path = Scope::Host.manifest_path();
        let city_path = Scope::City(city.to_path_buf()).manifest_path();

        assert_eq!(host_path, dir.path().join("services.toml"));
        assert_eq!(
            city_path,
            city.join(kingdom_core::services::MANIFEST_PATH),
            "a project's manifest belongs in the project"
        );
        assert_ne!(host_path, city_path);
    }

    /// A host well is filed under its own key, so it is not confusable with a
    /// project's -- and gets a network and containers of its own.
    #[test]
    fn a_host_well_has_a_key_no_project_can_take() {
        assert_eq!(Scope::Host.key(), "host");
        assert_eq!(
            docker::container_name(&Scope::Host.key(), "cache"),
            "kingdom-host-cache"
        );

        // A city key always carries a `-<8 hex>` suffix, so no project -- even
        // one whose folder is literally called `host` -- can collide with it.
        let awkward = city_key(Path::new("/home/king/dev/host"));
        assert_ne!(awkward, "host");
        assert!(awkward.starts_with("host-"), "{awkward}");
    }

    /// The form writes a file the parser can read back, and creates the
    /// directory it needs on the way.
    #[tokio::test]
    async fn declaring_a_resource_writes_a_manifest_that_parses() {
        let dir = tempfile::tempdir().expect("a temporary city");
        let scope = Scope::City(dir.path().to_path_buf());
        let spec = container("db", "postgres:16", 5432, Some("app-db"));

        let path = declare(&scope, &spec).expect("a first declaration must land");
        assert_eq!(path, dir.path().join(kingdom_core::services::MANIFEST_PATH));

        let manifest = manifest_in(&scope).expect("and must read back");
        assert_eq!(manifest.services, vec![spec]);
    }

    /// A second declaration is appended, and the comments already in the file
    /// survive it.
    ///
    /// This is the whole reason `declare` writes text rather than
    /// re-serialising a parsed document: the `shopfront` fixture's manifest
    /// opens with a paragraph explaining what Kingdom does with the file, and a
    /// round trip would eat it as the price of adding one service.
    #[tokio::test]
    async fn a_second_declaration_keeps_the_first_and_its_comments() {
        let dir = tempfile::tempdir().expect("a temporary city");
        let scope = Scope::City(dir.path().to_path_buf());
        let manifest_path = dir.path().join(kingdom_core::services::MANIFEST_PATH);
        std::fs::create_dir_all(manifest_path.parent().unwrap()).unwrap();
        std::fs::write(
            &manifest_path,
            "# What this project needs standing in order to run.\n\
             [[service]]\n\
             name = \"db\"\n\
             image = \"mongo:7\"\n\
             port = 27017\n",
        )
        .unwrap();

        declare(&scope, &container("cache", "redis:7", 6379, None))
            .expect("the second must land beside the first");

        let text = std::fs::read_to_string(&manifest_path).unwrap();
        assert!(
            text.contains("# What this project needs standing in order to run."),
            "the King's own comment must survive: {text:?}"
        );

        let names: Vec<String> = manifest_in(&scope)
            .expect("both must parse")
            .services
            .into_iter()
            .map(|s| s.name)
            .collect();
        assert_eq!(names, vec!["db".to_string(), "cache".to_string()]);
    }

    /// A name already in the file is refused, and nothing is written.
    ///
    /// The parser would refuse a duplicate too, but only after the file was on
    /// disk -- leaving the King with a manifest that no longer loads and a
    /// message about the whole file rather than about the name he just typed.
    #[tokio::test]
    async fn a_duplicate_name_is_refused_before_anything_is_written() {
        let dir = tempfile::tempdir().expect("a temporary city");
        let scope = Scope::City(dir.path().to_path_buf());
        let spec = container("db", "mongo:7", 27017, None);

        declare(&scope, &spec).expect("the first lands");
        let before = std::fs::read_to_string(scope.manifest_path()).unwrap();

        let error = declare(&scope, &spec).expect_err("the second must be refused");
        assert!(matches!(error, DeclareError::Duplicate { .. }), "{error:?}");
        // And the King is told which name and which file.
        assert!(error.to_string().contains("db"), "{error}");

        assert_eq!(
            std::fs::read_to_string(scope.manifest_path()).unwrap(),
            before,
            "a refused declaration must not have touched the file"
        );
    }

    /// The ledger reports both scopes, and reports a manifest that does not
    /// parse instead of dropping it.
    ///
    /// That last part is most of why the screen exists: today a broken manifest
    /// is silent until an agent's first turn is refused, minutes in, with a
    /// message about the model rather than about the file.
    #[tokio::test]
    async fn the_ledger_reports_both_scopes_and_a_broken_manifest() {
        let home = tempfile::tempdir().expect("a temporary profile");
        let _profile = crate::profile::testing::Profile::at(home.path());
        let root = tempfile::tempdir().expect("a temporary kingdom");

        // The King's own well.
        std::fs::write(
            home.path().join("services.toml"),
            "[[service]]\nname = \"cache\"\nimage = \"redis:7\"\nport = 6379\n",
        )
        .unwrap();

        // One good project manifest, and one that does not parse.
        for (name, body) in [
            (
                "shopfront",
                "[[service]]\nname = \"db\"\nimage = \"mongo:7\"\nport = 27017\n",
            ),
            ("ledger", "[[service]\nname = \"db\"\n"),
        ] {
            let dir = root.path().join(name).join(".kingdom");
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("services.toml"), body).unwrap();
        }

        let mut kingdom = Kingdom::unopened();
        kingdom.root = root.path().to_string_lossy().to_string();
        kingdom.cities = vec![
            a_city("shopfront"),
            a_city("ledger"),
            // A project with no manifest at all: the overwhelmingly common
            // case, and it must contribute nothing rather than an empty group.
            a_city("quiet"),
        ];

        let inventory = inventory(&kingdom).await;

        let named: Vec<(&str, &str)> = inventory
            .resources
            .iter()
            .map(|r| (r.scope.wire_name(), r.spec.name.as_str()))
            .collect();
        assert_eq!(
            named,
            vec![("host", "cache"), ("city", "db")],
            "the machine's own wells come first, then each project's"
        );

        let cache = &inventory.resources[0];
        assert_eq!(cache.city_name, None);
        assert_eq!(cache.owner(), "The whole machine");
        assert_eq!(
            cache.manifest_path,
            home.path().join("services.toml").to_string_lossy()
        );
        // Known even though nothing is running, because both are derived
        // rather than allocated -- which is what makes `docker logs` printable
        // on a row that is not up.
        assert_eq!(cache.handle, "kingdom-host-cache");
        assert_eq!(cache.hint, "docker logs kingdom-host-cache");
        // And the row says what kind of thing it is, which is what the screen
        // labels it with instead of assuming "container".
        assert_eq!(cache.spec.kind.wire_name(), "docker");

        let db = &inventory.resources[1];
        assert_eq!(db.city_name.as_deref(), Some("shopfront"));
        assert_eq!(db.owner(), "shopfront");
        assert!(
            db.manifest_path.contains("shopfront"),
            "the King has to be able to find the file: {}",
            db.manifest_path
        );

        assert_eq!(inventory.troubles.len(), 1, "{:?}", inventory.troubles);
        let trouble = &inventory.troubles[0];
        assert_eq!(trouble.city_name.as_deref(), Some("ledger"));
        assert!(trouble.manifest_path.contains("ledger"));
        assert!(!trouble.detail.is_empty());
        // The message names the file that is actually at fault. It used to name
        // a hardcoded `.kingdom/services.toml` from `kingdom-core`, which was
        // wrong for the King's own profile -- the message sent him to a file in
        // a project that had nothing wrong with it.
        assert!(
            trouble.detail.contains(&trouble.manifest_path),
            "the King must be told which file to open: {}",
            trouble.detail
        );
    }

    /// A city that declares nothing costs nothing and appears nowhere.
    #[tokio::test]
    async fn a_kingdom_that_shares_nothing_has_an_empty_ledger() {
        let home = tempfile::tempdir().expect("a temporary profile");
        let _profile = crate::profile::testing::Profile::at(home.path());
        let root = tempfile::tempdir().expect("a temporary kingdom");

        let mut kingdom = Kingdom::unopened();
        kingdom.root = root.path().to_string_lossy().to_string();
        kingdom.cities = vec![a_city("quiet")];

        assert!(inventory(&kingdom).await.is_empty());
    }

    /// A kingdom that shares nothing asks no runtime whether it is healthy.
    ///
    /// The manifests are read first, and the question is put only to the kinds
    /// something actually declares -- so the overwhelmingly common case, a
    /// machine with no manifest anywhere, costs no subprocess at all. It used
    /// to shell out to `docker version` to draw an empty screen, on a timer,
    /// while the screen was open.
    ///
    /// Asserted through `runtime_trouble` because that is the field the answer
    /// lands in: with nothing declared there is nothing to be troubled about,
    /// **whether or not** this machine has Docker. That is what makes this test
    /// true on a developer's laptop and in CI alike.
    #[tokio::test]
    async fn a_kingdom_that_shares_nothing_asks_no_runtime_anything() {
        let home = tempfile::tempdir().expect("a temporary profile");
        let _profile = crate::profile::testing::Profile::at(home.path());
        let root = tempfile::tempdir().expect("a temporary kingdom");

        let mut kingdom = Kingdom::unopened();
        kingdom.root = root.path().to_string_lossy().to_string();
        kingdom.cities = vec![a_city("quiet")];

        assert_eq!(
            inventory(&kingdom).await.runtime_trouble,
            None,
            "nothing is declared, so no runtime is asked and none can be at fault"
        );
    }

    /// Every kind Kingdom offers can actually be run.
    ///
    /// The seam this change is for, pinned from the runtime side.
    /// `ResourceKind::all` is what a manifest may name and what a form would
    /// offer; this asserts that each of those words reaches something that
    /// knows how to raise, stop and diagnose it.
    ///
    /// Without it, adding a variant to `ResourceKind` and forgetting the match
    /// arms here would compile -- `stop` and `trouble_with_runtimes` both have
    /// a catch-all, deliberately, because falling over in front of five working
    /// agents is worse than logging -- and fail at the moment a King first
    /// declared one.
    #[tokio::test]
    async fn every_kind_offered_has_a_runtime_behind_it() {
        for kind in kingdom_core::ResourceKind::all() {
            let unknown = trouble_with_runtimes(&[kind]).await;
            // The catch-all in `trouble_with_runtimes` says this exact thing,
            // and no kind that is genuinely wired up may produce it.
            if let Some(said) = &unknown {
                assert!(
                    !said.contains("does not know how to run"),
                    "`{kind}` is offered but nothing runs it: {said}"
                );
            }
        }
    }

    /// `running_in` answers with both scopes, and every caller that places a
    /// well *somewhere* has to know which is which.
    ///
    /// The map is the one that bites. `api::kingdom_network` puts a wellhead on
    /// a **town's square**, and a host well belongs to no town -- so passed
    /// through unfiltered, one Redis is drawn once in every city that has an
    /// agent in it: the same container claimed by three projects that do not
    /// own it. The feed filters on `scope`, and this pins the fact that makes
    /// the filter necessary rather than decorative.
    #[tokio::test]
    async fn running_in_answers_with_both_scopes_and_says_which_is_which() {
        let home = tempfile::tempdir().expect("a temporary profile");
        let _profile = crate::profile::testing::Profile::at(home.path());
        let city = tempfile::tempdir().expect("a temporary city");
        let key = city_key(city.path());

        // Two wells standing: one the King's, one the project's. Registered
        // directly rather than started, so this needs no daemon -- the claim
        // under test is about which scope each is filed under.
        {
            let mut registry = registry();
            for (scope, filed_under, name) in [
                (ServiceScope::Host, HOST_KEY.to_string(), "cache"),
                (ServiceScope::City, key.clone(), "db"),
            ] {
                registry.running.insert(
                    (filed_under.clone(), name.to_string()),
                    RunningService {
                        name: name.to_string(),
                        what: "mongo:7".to_string(),
                        host: "172.31.9.10".to_string(),
                        port: 27017,
                        handle: docker::container_name(&filed_under, name),
                        kind: "docker",
                        scope,
                        key: filed_under,
                    },
                );
            }
        }

        let standing = running_in(city.path());
        let named: Vec<(&str, ServiceScope)> = standing
            .iter()
            .map(|s| (s.name.as_str(), s.scope))
            .collect();
        assert_eq!(
            named,
            vec![("cache", ServiceScope::Host), ("db", ServiceScope::City)],
            "a plan here can reach both, and the machine's comes first"
        );

        // What the map keeps: the city's own, and only that one.
        let on_the_square: Vec<&str> = standing
            .iter()
            .filter(|s| s.scope == ServiceScope::City)
            .map(|s| s.name.as_str())
            .collect();
        assert_eq!(on_the_square, vec!["db"]);

        // And each carries the key it is filed under, which is the only way to
        // count its drawers: asking for the host well by city root finds
        // nothing at all.
        let cache = &standing[0];
        assert_eq!(cache.key, HOST_KEY);
        assert_ne!(cache.key, key);

        // Left as found, since the registry is process-global.
        let mut registry = registry();
        registry
            .running
            .remove(&(HOST_KEY.to_string(), "cache".to_string()));
        registry.running.remove(&(key, "db".to_string()));
    }

    fn a_city(name: &str) -> kingdom_core::City {
        kingdom_core::City {
            id: kingdom_core::CityId::from(name),
            name: name.to_string(),
            path: name.to_string(),
            kind: kingdom_core::CityKind::Unknown,
            file_count: 0,
            has_git: true,
            dirty_files: 0,
            structure: None,
        }
    }
}
