//! Shared services: one container, shared by every plan in a city.
//!
//! Shown to the King as "the well"; called a shared service everywhere the
//! compiler reads, per AGENTS.md.
//!
//! # The problem
//!
//! [`crate::netns`] answers *"stop these agents colliding on a port"*. This
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
//! # Why it differs from `netns.rs` in one important way
//!
//! `netns::reclaim_previous` **kills** what a previous server left behind,
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
//! `docker` itself and do as it likes. Like [`crate::netns`], this is
//! coordination, not containment, and saying so plainly is worth more than a
//! guarantee that does not hold.

use kingdom_core::services::{
    ManifestTrouble, ResourceInventory, ServiceManifest, ServiceScope, ServiceSpec, ServiceState,
    SharedResource,
};
use kingdom_core::{Kingdom, PlanId};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

/// The private range Kingdom draws city subnets from.
///
/// `172.31.0.0/16`, carved into `/24`s. Docker's own default pool starts at
/// `172.17` and grows upwards, so starting at the top of the same block keeps
/// Kingdom's networks clear of it for a long time while staying inside the
/// address space Docker users already expect to be busy.
const SUBNET_PREFIX: (u8, u8) = (172, 31);

/// How many `/24`s are tried before giving up on a free one.
///
/// A collision is possible -- another Docker network, a VPN, a route the King
/// added himself -- so the subnet is *tried* rather than assumed, in the manner
/// of `netns::add_forward`'s port draw.
const SUBNET_ATTEMPTS: u8 = 32;

/// The last octet of the first service's address.
///
/// Services are numbered upwards from here. Above Docker's own gateway at `.1`,
/// and clear of the low addresses with enough room that a person reading
/// `172.31.4.10` can tell it was assigned rather than allocated.
const FIRST_HOST_OCTET: u8 = 10;

/// How long a service is given to start answering on its port.
///
/// Generous, because the first run of an image includes pulling it. A plan
/// handed an address for a container that is not listening yet fails in a way
/// that reads as a bug in the plan's own code, so waiting is worth the delay.
const READY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(180);

/// How often the port is tried while waiting.
const READY_POLL: std::time::Duration = std::time::Duration::from_millis(250);

/// The label carrying the scope a container belongs to: a city's key, or
/// `host`.
const LABEL_CITY: &str = "kingdom.city";
/// The label carrying the service's name within that city.
const LABEL_SERVICE: &str = "kingdom.service";

/// One service that is up, and where to reach it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunningService {
    /// The service's name from the manifest.
    pub name: String,
    /// The image it is running.
    pub image: String,
    /// Its address on the city's network -- an IP, because neither the host nor
    /// a plan's namespace can resolve Docker's service names.
    pub host: String,
    /// The port it listens on.
    pub port: u16,
    /// The container's name, which is also how it is found again.
    pub container: String,
    /// Which level it was declared at.
    ///
    /// Carried on the running service rather than looked up again, because the
    /// two places that show a well to the King -- the badge and the ledger --
    /// both have to say whether it is this project's or the machine's, and
    /// re-deriving it from the container name would be reading a string back
    /// out of a string.
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
/// Process-global for the reason `netns::NAMESPACES` is: a container must
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
struct Registry {
    /// `(scope key, service name)` -> the running service.
    running: HashMap<(String, String), RunningService>,
    /// `(scope key, service name)` -> the plans using it.
    ///
    /// This is the reference count, kept as the set of plan ids rather than an
    /// integer so that a plan closed twice cannot decrement it twice -- which
    /// would stop a database five other plans were still using.
    ///
    /// A host service's set spans **every** city, which is the whole difference
    /// the scope makes: it is stopped when the last plan anywhere lets go, not
    /// when the last plan in one project does.
    users: HashMap<(String, String), HashSet<PlanId>>,
}

fn registry() -> std::sync::MutexGuard<'static, Registry> {
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
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ServiceError {
    #[error(
        "this project declares shared services in `{path}`, but Docker is not \
         installed -- so there is nothing to run them. Install Docker, or \
         remove that file if the project no longer needs it.",
        path = kingdom_core::services::MANIFEST_PATH
    )]
    DockerMissing,

    #[error(
        "Docker is installed but not answering ({0}). It is usually the daemon \
         not running: try `sudo systemctl start docker`."
    )]
    DockerUnreachable(String),

    #[error("{0}")]
    Manifest(String),

    #[error("the shared service `{name}` could not be started: {detail}")]
    Failed { name: String, detail: String },

    #[error(
        "the shared service `{name}` started but never answered on port \
         {port}. Its log may say why: `docker logs {container}`."
    )]
    NeverReady {
        name: String,
        port: u16,
        container: String,
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
    kingdom_core::services::parse(&text)
        .map_err(|e| ServiceError::Manifest(format!("{} is {e}", path.display())))
}

/// Reads a city's manifest, if it has one.
///
/// Kept as its own name because it reads at every call site as the question
/// being asked -- "what does this project declare?" -- and because the city is
/// the common case by a wide margin.
pub fn manifest_of(city_root: &Path) -> Result<ServiceManifest, ServiceError> {
    manifest_in(&Scope::City(city_root.to_path_buf()))
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
async fn raise(
    scope: &Scope,
    drawers: &HashSet<PlanId>,
) -> Result<Vec<RunningService>, ServiceError> {
    let manifest = manifest_in(scope)?;
    if manifest.is_empty() {
        return Ok(Vec::new());
    }

    if which("docker").is_none() {
        return Err(ServiceError::DockerMissing);
    }

    let key = scope.key();
    let network = network_name(&key);
    let subnet = ensure_network(&network).await?;

    let mut up = Vec::new();
    for (index, spec) in manifest.services.iter().enumerate() {
        let service = ensure_one(scope, &key, &network, subnet, index, spec).await?;
        {
            // Taken and dropped around the record, never held across the await
            // above. See `RAISING`.
            let mut registry = registry();
            let id = (key.clone(), spec.name.clone());
            registry.running.insert(id.clone(), service.clone());
            // **Replaced, not extended.** `reconcile` hands down the whole live
            // population for this scope, so that set is the answer rather than
            // an addition to it. Extending was a real bug while this was being
            // written: a plan that finished stayed in the count forever, and
            // `users_of` reported a database as busy that nobody was in.
            registry.users.insert(id, drawers.clone());
        }
        up.push(service);
    }
    Ok(up)
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
        // Stopped, never removed, and the named volume left alone: the King's
        // data is the whole reason the service existed.
        let _ = docker(&["stop", &service.container]).await;
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
        if !manifest.is_empty() {
            declared.push((scope, manifest));
        }
    }
    if declared.is_empty() {
        return Ok(());
    }

    // Whatever pass is in flight finishes first. Dropped immediately: this
    // reads state, it does not change it.
    drop(raising().await);

    for (scope, manifest) in declared {
        let key = scope.key();
        for spec in &manifest.services {
            let id = (key.clone(), spec.name.clone());
            let mut registry = registry();
            if !registry.running.contains_key(&id) {
                return Err(ServiceError::Failed {
                    name: spec.name.clone(),
                    detail: "it is declared by this project but is not running. \
                             Kingdom raises it when an agent that needs it is \
                             open; the shared resources screen says why it is \
                             not up."
                        .to_string(),
                });
            }
            // This plan is one of its drawers from here on. The well is
            // already up, so this is bookkeeping rather than a start -- it is
            // what stops the well being swept out from under a plan that
            // reached it between two reconciles.
            registry.users.entry(id).or_default().insert(plan.clone());
        }
    }

    Ok(())
}

/// The environment a plan's tools get for the services it can reach.
///
/// Read from the registry rather than started here, so this is cheap enough to
/// call on every command. A city with nothing running yields nothing, which is
/// what makes it safe to apply unconditionally in `tools::child_environment`.
///
/// Host first and the city second, so a project that sets `DATABASE_URL` for
/// its own database wins over a machine-wide one of the same name. Collected
/// through a map rather than a `Vec` for that reason -- the last writer has to
/// be the only one left.
pub fn environment(city_root: &Path) -> Vec<(String, String)> {
    let registry = registry();
    let mut merged: BTreeMap<String, String> = BTreeMap::new();

    for scope in scopes_for(city_root) {
        let Ok(manifest) = manifest_in(&scope) else {
            continue;
        };
        if manifest.is_empty() {
            continue;
        }
        let key = scope.key();
        for spec in &manifest.services {
            if let Some(running) = registry.running.get(&(key.clone(), spec.name.clone())) {
                merged.extend(spec.environment(&running.host, running.port));
            }
        }
    }

    merged.into_iter().collect()
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
/// asks Docker exactly one question for the whole screen -- see
/// [`docker_trouble`]. No `docker inspect` per row: the registry already knows
/// what this server started, and a container some *other* server started is not
/// this ledger's business.
pub async fn inventory(kingdom: &Kingdom) -> ResourceInventory {
    let mut out = ResourceInventory {
        docker_trouble: docker_trouble().await,
        ..ResourceInventory::default()
    };

    // Host first, so the machine's own wells head the list -- they are the ones
    // shared furthest, and so the ones a change to is most consequential.
    let mut scopes: Vec<(Scope, Option<&kingdom_core::City>)> = vec![(Scope::Host, None)];
    let root = Path::new(&kingdom.root);
    for city in &kingdom.cities {
        scopes.push((Scope::City(root.join(&city.path)), Some(city)));
    }

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
        if manifest.is_empty() {
            continue;
        }

        let key = scope.key();
        for spec in &manifest.services {
            out.resources.push(describe(
                kingdom,
                &scope,
                &key,
                city,
                &shown_path,
                spec,
                out.docker_trouble.is_some(),
            ));
        }
    }

    out
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
    docker_is_out: bool,
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

    let state = match (&running, docker_is_out) {
        (Some(_), _) => ServiceState::Running,
        // Not "idle": with no daemon answering, Kingdom genuinely does not know
        // whether this is up, and saying "not started" would be a guess dressed
        // as a fact.
        (None, true) => ServiceState::Unknown,
        (None, false) => ServiceState::Idle,
    };

    // Filled in when the address is known, and left as written when it is not.
    // A `{host}` the King can see is the honest answer for a service that has
    // never run -- the address does not exist yet.
    let environment = match &running {
        Some(running) => spec.environment(&running.host, running.port),
        None => spec
            .env
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect(),
    };

    SharedResource {
        spec: spec.clone(),
        scope: scope.kind(),
        city: city.map(|c| c.id.clone()),
        city_name: city.map(|c| c.name.clone()),
        manifest_path: manifest_path.to_string(),
        state,
        address: running.as_ref().map(RunningService::address),
        // Derived rather than allocated, so it is known even for a service that
        // has never run -- which is what makes `docker logs <name>` printable
        // on a row that is not up.
        container: container_name(key, &spec.name),
        users,
        environment,
    }
}

/// Why nothing could be running, or `None` if Docker answered.
///
/// Asked once for the whole ledger rather than once per row: the answer is the
/// same for every one of them, and it is the difference between a screen of
/// confusing "not started"s and one banner saying the daemon is down.
async fn docker_trouble() -> Option<String> {
    if which("docker").is_none() {
        return Some(ServiceError::DockerMissing.to_string());
    }
    match docker(&["version", "--format", "{{.Server.Version}}"]).await {
        Ok(_) => None,
        Err(e) => Some(ServiceError::DockerUnreachable(e).to_string()),
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

/// Finds an executable on `PATH`.
///
/// A copy of `netns::which` rather than a shared helper: it is four lines, and
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
fn path_hash(path: &Path) -> u32 {
    let mut hash: u32 = 0x811c_9dc5;
    for byte in path.to_string_lossy().as_bytes() {
        hash ^= *byte as u32;
        hash = hash.wrapping_mul(0x0100_0193);
    }
    hash
}

/// The Docker network for a city.
fn network_name(city_key: &str) -> String {
    format!("kingdom-{city_key}")
}

/// The container name for one service.
fn container_name(city_key: &str, service: &str) -> String {
    format!("kingdom-{city_key}-{service}")
}

/// The address a service gets on its city's network.
///
/// Assigned from its position in the manifest rather than allocated by Docker,
/// which is what makes the address knowable before the container exists -- and
/// therefore printable, and substitutable into an environment variable.
fn service_address(subnet: u8, index: usize) -> String {
    let (a, b) = SUBNET_PREFIX;
    format!("{a}.{b}.{subnet}.{}", FIRST_HOST_OCTET as usize + index)
}

/// The third octet a city's `/24` starts from.
///
/// Hashed rather than counted so that a city keeps the same subnet across
/// restarts and across the order plans happen to be opened in.
fn preferred_subnet(city_key: &str) -> u8 {
    (path_hash(Path::new(city_key)) % 256) as u8
}

/// Runs a `docker` command, returning its stdout.
///
/// Shelling out rather than taking on `bollard`, for the reason `netns.rs`
/// shells out to `unshare` and `slirp4netns`: the surface used here is a
/// handful of subcommands, and the CLI is the interface Docker documents and
/// the King can reproduce by hand when something goes wrong.
async fn docker(args: &[&str]) -> Result<String, String> {
    let output = tokio::process::Command::new("docker")
        .args(args)
        .output()
        .await
        .map_err(|e| e.to_string())?;

    if !output.status.success() {
        let message = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(if message.is_empty() {
            format!("`docker {}` failed", args.join(" "))
        } else {
            message
        });
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Makes sure the city's network exists, and returns the `/24` it is on.
///
/// Adopts an existing network rather than recreating it -- containers are
/// attached to it, and recreating would strand them.
async fn ensure_network(network: &str) -> Result<u8, ServiceError> {
    // Already there from an earlier plan, or an earlier run of the server.
    if let Ok(existing) = docker(&[
        "network",
        "inspect",
        network,
        "--format",
        "{{range .IPAM.Config}}{{.Subnet}}{{end}}",
    ])
    .await
    {
        if let Some(subnet) = third_octet(&existing) {
            return Ok(subnet);
        }
    }

    // Not there. `docker network inspect` failing could also mean the daemon is
    // down, which is a different problem with a different fix -- so ask.
    docker(&["version", "--format", "{{.Server.Version}}"])
        .await
        .map_err(ServiceError::DockerUnreachable)?;

    let preferred = preferred_subnet(network);
    let mut last = String::new();
    for attempt in 0..SUBNET_ATTEMPTS {
        let third = preferred.wrapping_add(attempt);
        let (a, b) = SUBNET_PREFIX;
        let subnet = format!("{a}.{b}.{third}.0/24");
        match docker(&["network", "create", "--subnet", &subnet, network]).await {
            Ok(_) => return Ok(third),
            Err(e) => {
                // Another plan created it while we were asking. Not a failure:
                // re-inspect and take whatever it settled on.
                if let Ok(existing) = docker(&[
                    "network",
                    "inspect",
                    network,
                    "--format",
                    "{{range .IPAM.Config}}{{.Subnet}}{{end}}",
                ])
                .await
                {
                    if let Some(third) = third_octet(&existing) {
                        return Ok(third);
                    }
                }
                last = e;
            }
        }
    }

    Err(ServiceError::Failed {
        name: network.to_string(),
        detail: format!(
            "no free subnet in {}.{}.0.0/16: {last}",
            SUBNET_PREFIX.0, SUBNET_PREFIX.1
        ),
    })
}

/// The third octet of a `172.31.N.0/24`, or `None` if it is not one of ours.
fn third_octet(subnet: &str) -> Option<u8> {
    let address = subnet.split('/').next()?;
    let mut parts = address.split('.');
    let a: u8 = parts.next()?.parse().ok()?;
    let b: u8 = parts.next()?.parse().ok()?;
    let c: u8 = parts.next()?.parse().ok()?;
    if (a, b) != SUBNET_PREFIX {
        return None;
    }
    Some(c)
}

/// Brings one service up, or adopts it if it is already up.
async fn ensure_one(
    scope: &Scope,
    key: &str,
    network: &str,
    subnet: u8,
    index: usize,
    spec: &ServiceSpec,
) -> Result<RunningService, ServiceError> {
    let container = container_name(key, &spec.name);
    let host = service_address(subnet, index);

    let service = RunningService {
        name: spec.name.clone(),
        image: spec.image.clone(),
        host: host.clone(),
        port: spec.port,
        container: container.clone(),
        scope: scope.kind(),
        key: key.to_string(),
    };

    match container_state(&container).await {
        // Up already: this is a plan two-through-five, or a server restart
        // finding what the last one left. Adopted, deliberately -- see the
        // module docs on why this differs from `netns::reclaim_previous`.
        ContainerState::Running => {
            wait_until_ready(&service).await?;
            return Ok(service);
        }
        // There but stopped: the last plan released it, and a new one wants it
        // again. Starting it keeps the volume and everything in it.
        ContainerState::Stopped => {
            docker(&["start", &container])
                .await
                .map_err(|detail| ServiceError::Failed {
                    name: spec.name.clone(),
                    detail,
                })?;
            wait_until_ready(&service).await?;
            return Ok(service);
        }
        ContainerState::Absent => {}
    }

    let mut args: Vec<String> = vec![
        "run".into(),
        "--detach".into(),
        "--name".into(),
        container.clone(),
        "--network".into(),
        network.into(),
        "--ip".into(),
        host.clone(),
        "--label".into(),
        format!("{LABEL_CITY}={key}"),
        "--label".into(),
        format!("{LABEL_SERVICE}={}", spec.name),
        // Deliberately no `-p`. Nothing is published on the King's loopback, so
        // the service cannot take a port from him; he reaches it at the address
        // above, which Docker's own bridge route makes reachable.
    ];
    if let Some(volume) = &spec.volume {
        args.push("--volume".into());
        args.push(format!("{volume}:{}", data_dir_for(&spec.image)));
    }
    args.push(spec.image.clone());

    let argv: Vec<&str> = args.iter().map(String::as_str).collect();
    docker(&argv).await.map_err(|detail| ServiceError::Failed {
        name: spec.name.clone(),
        detail,
    })?;

    wait_until_ready(&service).await?;
    Ok(service)
}

/// Whether a container exists, and whether it is running.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ContainerState {
    Running,
    Stopped,
    Absent,
}

async fn container_state(container: &str) -> ContainerState {
    match docker(&["inspect", "-f", "{{.State.Running}}", container]).await {
        Ok(answer) if answer.trim() == "true" => ContainerState::Running,
        Ok(_) => ContainerState::Stopped,
        Err(_) => ContainerState::Absent,
    }
}

/// Where a well-known image keeps its data.
///
/// A small table rather than a manifest field, because the King should not have
/// to know MongoDB's data path to share a database. An image not in the table
/// gets `/data`, and a project that needs otherwise can set the volume to a
/// path itself -- which is the escape hatch, not the common case.
fn data_dir_for(image: &str) -> &'static str {
    let name = image.split(':').next().unwrap_or(image);
    match name.rsplit('/').next().unwrap_or(name) {
        "mongo" => "/data/db",
        "postgres" => "/var/lib/postgresql/data",
        "mysql" | "mariadb" => "/var/lib/mysql",
        "redis" => "/data",
        _ => "/data",
    }
}

/// Waits until the service answers on its port.
///
/// A TCP connect rather than a health check: it is the same question every
/// client asks, needs nothing installed in the image, and is true exactly when
/// a plan handed the address would succeed. `docker run` returns as soon as the
/// container is *created*, which for a database is well before it can be used.
async fn wait_until_ready(service: &RunningService) -> Result<(), ServiceError> {
    let deadline = std::time::Instant::now() + READY_TIMEOUT;
    let address = service.address();

    while std::time::Instant::now() < deadline {
        // The container dying is a settled answer -- keep waiting and the King
        // gets a timeout describing the wrong problem.
        if container_state(&service.container).await != ContainerState::Running {
            return Err(ServiceError::Failed {
                name: service.name.clone(),
                detail: format!(
                    "the container stopped while starting; `docker logs {}` may say why",
                    service.container
                ),
            });
        }

        let connected = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            tokio::net::TcpStream::connect(&address),
        )
        .await;
        if matches!(connected, Ok(Ok(_))) {
            return Ok(());
        }

        tokio::time::sleep(READY_POLL).await;
    }

    Err(ServiceError::NeverReady {
        name: service.name.clone(),
        port: service.port,
        container: service.container.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

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

    /// A path with spaces or dots still yields a name Docker will take.
    #[test]
    fn an_awkward_path_still_makes_a_usable_name() {
        let key = city_key(Path::new("/home/king/my project (v2)"));
        assert!(
            key.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'),
            "{key} is not a usable container name fragment"
        );
        // And it is still a legal container name once wrapped.
        let container = container_name(&key, "db");
        assert!(container.starts_with("kingdom-"), "{container}");
    }

    /// Addresses are assigned from manifest order, which is what makes them
    /// knowable *before* the container exists -- and therefore substitutable
    /// into an environment variable.
    #[test]
    fn addresses_come_from_manifest_order() {
        assert_eq!(service_address(77, 0), "172.31.77.10");
        assert_eq!(service_address(77, 1), "172.31.77.11");
        // Clear of Docker's own gateway at .1, which it takes for itself on
        // every network it creates -- asked of a generated address rather than
        // of the constant, so this still holds if the numbering changes.
        let first = service_address(77, 0);
        let last_octet: u8 = first.rsplit('.').next().unwrap().parse().unwrap();
        assert!(last_octet > 1, "{first} collides with the bridge gateway");
    }

    /// A city's subnet is derived, not counted, so it survives a restart and
    /// does not depend on the order plans were opened in.
    #[test]
    fn a_citys_subnet_is_stable() {
        let key = city_key(Path::new("/home/king/work/shopfront"));
        assert_eq!(preferred_subnet(&key), preferred_subnet(&key));
    }

    /// Only Kingdom's own subnets are recognised.
    ///
    /// A city whose network someone recreated on `172.17.x` must not be read as
    /// ours, or services would be assigned addresses on a subnet the network
    /// does not actually cover.
    #[test]
    fn only_our_own_subnets_are_recognised() {
        assert_eq!(third_octet("172.31.77.0/24"), Some(77));
        assert_eq!(third_octet("172.31.0.0/24"), Some(0));
        assert_eq!(third_octet("172.17.0.0/16"), None);
        assert_eq!(third_octet("10.0.0.0/8"), None);
        assert_eq!(third_octet("nonsense"), None);
    }

    /// The data directory is looked up per image, so a manifest does not have
    /// to know where MongoDB keeps its files.
    #[test]
    fn a_volume_lands_where_the_image_keeps_its_data() {
        assert_eq!(data_dir_for("mongo:7"), "/data/db");
        assert_eq!(data_dir_for("postgres:16"), "/var/lib/postgresql/data");
        // Registry-qualified names resolve to the same answer.
        assert_eq!(data_dir_for("docker.io/library/mongo:7"), "/data/db");
        // An unknown image still gets something rather than failing.
        assert_eq!(data_dir_for("ghcr.io/someone/thing:1"), "/data");
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
                    image: "mongo:7".to_string(),
                    host: "172.31.9.10".to_string(),
                    port: 27017,
                    container: "kingdom-double-release-test-db".to_string(),
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
        assert!(environment(&empty).is_empty());
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
        assert!(environment(city.path()).is_empty());
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
                    image: "mongo:7".to_string(),
                    host: "172.31.9.10".to_string(),
                    port: 27017,
                    container: "kingdom-leaving-test-db".to_string(),
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

    /// A container is never published to the host.
    ///
    /// Pinned as a test because the temptation to add `-p` is exactly what the
    /// measurement in the module docs rules out: a published port is on the
    /// King's loopback, which is the one address a plan's namespace provably
    /// cannot reach.
    #[test]
    fn nothing_is_published_to_the_kings_loopback() {
        let source = include_str!("services.rs");
        // The run arguments are built in `ensure_one`; no `-p`/`--publish`
        // anywhere in this module.
        assert!(
            !source.contains("\"--publish\""),
            "a published port cannot be reached from a plan's namespace"
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
            container_name(&Scope::Host.key(), "cache"),
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
        let spec = ServiceSpec {
            name: "db".to_string(),
            image: "postgres:16".to_string(),
            port: 5432,
            env: [(
                "DATABASE_URL".to_string(),
                "postgres://postgres@{host}:{port}/app".to_string(),
            )]
            .into_iter()
            .collect(),
            volume: Some("app-db".to_string()),
        };

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

        declare(
            &scope,
            &ServiceSpec {
                name: "cache".to_string(),
                image: "redis:7".to_string(),
                port: 6379,
                env: std::collections::BTreeMap::new(),
                volume: None,
            },
        )
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
        let spec = ServiceSpec {
            name: "db".to_string(),
            image: "mongo:7".to_string(),
            port: 27017,
            env: std::collections::BTreeMap::new(),
            volume: None,
        };

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
        // Known even though nothing is running, because the name is derived
        // rather than allocated -- which is what makes `docker logs` printable.
        assert_eq!(cache.container, "kingdom-host-cache");

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
                        image: "mongo:7".to_string(),
                        host: "172.31.9.10".to_string(),
                        port: 27017,
                        container: container_name(&filed_under, name),
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

/// Against a real Docker daemon. `cargo test -p kingdom-app --features ssr \
/// --no-default-features -- --ignored a_host_well_serves_two_projects`
///
/// The claim the host scope makes, and the one that cannot be checked without
/// a daemon: **one** container, reached by plans working in two different
/// projects, released only when the last of them is done. Every other test of
/// the scope is about which file is read; this is about what actually runs.
#[cfg(test)]
mod host_scope {
    use super::*;

    #[tokio::test]
    #[ignore = "needs a running Docker daemon"]
    async fn a_host_well_serves_two_projects() {
        let home = tempfile::tempdir().expect("a temporary profile");
        let _profile = crate::profile::testing::Profile::at(home.path());

        // Declared through the same path the form uses, so this proves the
        // write and the read together rather than one against a hand-made file.
        declare(
            &Scope::Host,
            &ServiceSpec {
                name: "scopecache".to_string(),
                image: "redis:7-alpine".to_string(),
                port: 6379,
                env: [("REDIS_URL".to_string(), "redis://{host}:{port}".to_string())]
                    .into_iter()
                    .collect(),
                volume: None,
            },
        )
        .expect("the King's own manifest must be writable");

        // Two projects that declare nothing of their own. Whatever they reach
        // is the machine's, which is the point.
        let one = tempfile::tempdir().expect("project one");
        let two = tempfile::tempdir().expect("project two");
        let alice = PlanId::new("host-scope-alice");
        let bob = PlanId::new("host-scope-bob");

        let raised = raise(&Scope::Host, &[alice.clone()].into_iter().collect())
            .await
            .expect("the first plan raises it");
        assert_eq!(raised.len(), 1);
        assert_eq!(raised[0].scope, ServiceScope::Host);
        let container = raised[0].container.clone();
        assert_eq!(container, "kingdom-host-scopecache");

        // A plan in a *different* project finds the same container standing.
        // Through `reconcile`, because that is what a second plan opening
        // actually does, and it must hand down *both* drawers -- the whole
        // population, not an increment.
        reconcile(vec![
            (alice.clone(), one.path().to_path_buf()),
            (bob.clone(), two.path().to_path_buf()),
        ])
        .await;
        let adopted = running_in(two.path());
        assert_eq!(
            adopted[0].address(),
            raised[0].address(),
            "two projects must reach one address, or the scope means nothing"
        );
        assert_eq!(users_of_key("host", "scopecache"), 2);

        // And both are handed it as an environment variable, with the real
        // address substituted -- the only way an agent finds it.
        for root in [one.path(), two.path()] {
            let env = environment(root);
            let url = env
                .iter()
                .find(|(k, _)| k == "REDIS_URL")
                .map(|(_, v)| v.clone())
                .expect("every project gets the machine's wells");
            assert_eq!(url, format!("redis://{}", raised[0].address()));
        }

        // One project finishing does not take it from the other. This is the
        // reference count spanning cities, which is the whole difference
        // between a host well and a city one. Alice finishing means she is no
        // longer in the population; Bob still is.
        reconcile(vec![(bob.clone(), two.path().to_path_buf())]).await;
        assert_eq!(users_of_key("host", "scopecache"), 1);
        assert_eq!(
            container_state(&container).await,
            ContainerState::Running,
            "another project is still using it"
        );

        // And the last one out stops it: an empty population, which is also
        // what closing the kingdom hands down.
        reconcile(Vec::new()).await;
        assert_eq!(users_of_key("host", "scopecache"), 0);
        assert_eq!(container_state(&container).await, ContainerState::Stopped);

        let _ = docker(&["rm", "-f", &container]).await;
        let _ = docker(&["network", "rm", &network_name("host")]).await;
    }
}

/// Against a real Docker daemon. `cargo test -p kingdom-app --features ssr \
/// --no-default-features -- --ignored services_against_real_docker`
///
/// Ignored by default, the rule `kingdom-browser` follows for Chrome: the suite
/// has to run on a bare machine, and nothing in CI has a daemon. But the
/// interesting half of this module *is* the conversation with Docker, so it is
/// worth being able to run for real.
#[cfg(test)]
mod real_docker {
    use super::*;

    /// The whole lifecycle: start, adopt, share, release, and keep the data.
    ///
    /// One test rather than five because each step needs the previous one's
    /// container, and five tests would either serialise by luck or fight over
    /// the same name.
    #[tokio::test]
    #[ignore = "needs a running Docker daemon"]
    async fn services_against_real_docker() {
        let root = std::env::temp_dir().join("kingdom-services-real-test");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join(".kingdom")).expect("make the city");
        std::fs::write(
            root.join(kingdom_core::services::MANIFEST_PATH),
            r#"
            [[service]]
            name  = "cache"
            image = "redis:7-alpine"
            port  = 6379
            env   = { REDIS_URL = "redis://{host}:{port}" }
            volume = "kingdom-services-real-test-data"
            "#,
        )
        .expect("write the manifest");

        let one = PlanId::new("real-plan-1");
        let two = PlanId::new("real-plan-2");

        // First plan starts the service.
        reconcile(vec![(one.clone(), root.clone())]).await;
        let up = running_in(&root);
        assert_eq!(up.len(), 1);
        let service = up[0].clone();
        assert!(
            service.host.starts_with("172.31."),
            "expected a Kingdom subnet, got {}",
            service.host
        );

        // The address actually answers -- the whole claim, tested rather than
        // trusted.
        tokio::net::TcpStream::connect(service.address())
            .await
            .expect("the service must answer at the address we hand to plans");

        // And that address is what a plan's tools would be given.
        let environment = environment(&root);
        assert_eq!(
            environment,
            vec![(
                "REDIS_URL".to_string(),
                format!("redis://{}", service.address())
            )]
        );

        // Second plan finds it standing rather than starting another.
        reconcile(vec![
            (one.clone(), root.clone()),
            (two.clone(), root.clone()),
        ])
        .await;
        let again = running_in(&root);
        assert_eq!(
            again[0].container, service.container,
            "adopted, not restarted"
        );
        assert_eq!(users_of(&root, "cache"), 2);

        // Leave something behind, to prove the volume outlives the container.
        let written = docker(&[
            "exec",
            &service.container,
            "redis-cli",
            "set",
            "kingdom-probe",
            "survived",
        ])
        .await;
        assert!(
            written.is_ok(),
            "could not write to the service: {written:?}"
        );
        let _ = docker(&["exec", &service.container, "redis-cli", "save"]).await;

        // One plan leaving does not stop it.
        reconcile(vec![(two.clone(), root.clone())]).await;
        assert_eq!(users_of(&root, "cache"), 1);
        assert_eq!(
            container_state(&service.container).await,
            ContainerState::Running
        );

        // The last plan leaving does.
        reconcile(Vec::new()).await;
        assert_eq!(users_of(&root, "cache"), 0);
        assert_eq!(
            container_state(&service.container).await,
            ContainerState::Stopped
        );

        // A later plan gets it back, with its data.
        let three = PlanId::new("real-plan-3");
        reconcile(vec![(three.clone(), root.clone())]).await;
        let restarted = running_in(&root);
        assert_eq!(restarted[0].container, service.container);
        let read = docker(&[
            "exec",
            &service.container,
            "redis-cli",
            "get",
            "kingdom-probe",
        ])
        .await
        .expect("read back");
        assert_eq!(
            read.trim(),
            "survived",
            "the volume must outlive the container -- that is the King's data"
        );

        // Tidy up: this test owns every name it used.
        reconcile(Vec::new()).await;
        let _ = docker(&["rm", "-f", &service.container]).await;
        let _ = docker(&["volume", "rm", "kingdom-services-real-test-data"]).await;
        let _ = docker(&["network", "rm", &network_name(&city_key(&root))]).await;
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A restart brings the well back to the agents that had it.
    ///
    /// The failure this whole change exists to fix. A container does not live
    /// in the server process but the registry does, so a restart -- which
    /// `cargo leptos watch` performs on every save -- used to leave five live
    /// agents with no database and every surface reporting "not started", until
    /// one of them happened to take a turn.
    ///
    /// Simulates the restart honestly: the registry is emptied **and** the
    /// container stopped, which is the state the previous server's last release
    /// leaves behind. What must come back is the *same* container with the
    /// *same* data, because adopt-rather-than-recreate is what the King's data
    /// depends on.
    #[tokio::test]
    #[ignore = "needs a running Docker daemon"]
    async fn a_restart_brings_the_well_back_to_the_agents_that_had_it() {
        let root = std::env::temp_dir().join("kingdom-services-restart-test");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join(".kingdom")).expect("make the city");
        std::fs::write(
            root.join(kingdom_core::services::MANIFEST_PATH),
            r#"
            [[service]]
            name  = "cache"
            image = "redis:7-alpine"
            port  = 6379
            env   = { REDIS_URL = "redis://{host}:{port}" }
            volume = "kingdom-services-restart-test-data"
            "#,
        )
        .expect("write the manifest");

        let alice = PlanId::new("restart-alice");
        let bob = PlanId::new("restart-bob");
        let population = vec![(alice.clone(), root.clone()), (bob.clone(), root.clone())];

        // A session in which two agents are working with a database up.
        reconcile(population.clone()).await;
        let before = running_in(&root);
        assert_eq!(before.len(), 1);
        let service = before[0].clone();
        assert_eq!(users_of(&root, "cache"), 2);

        let written = docker(&[
            "exec",
            &service.container,
            "redis-cli",
            "set",
            "kingdom-restart-probe",
            "survived",
        ])
        .await;
        assert!(written.is_ok(), "could not write: {written:?}");
        let _ = docker(&["exec", &service.container, "redis-cli", "save"]).await;

        // The server stops. The registry goes with the process; the container
        // is left stopped, as the last release would have left it.
        let _ = docker(&["stop", &service.container]).await;
        {
            let mut registry = registry();
            registry.running.clear();
            registry.users.clear();
        }
        assert!(
            running_in(&root).is_empty(),
            "the fixture must start from a server that knows nothing"
        );

        // The server starts again and opens the kingdom, which is the one call
        // this change adds.
        reconcile(population).await;

        let after = running_in(&root);
        assert_eq!(after.len(), 1, "the well is standing again");
        assert_eq!(
            after[0].container, service.container,
            "adopted, not recreated"
        );
        assert_eq!(
            after[0].address(),
            service.address(),
            "the address must survive a restart, or every agent's env is stale"
        );
        assert_eq!(
            users_of(&root, "cache"),
            2,
            "both agents that had it must be counted again, not one and not none"
        );

        // It answers, and the King's data is where he left it.
        tokio::net::TcpStream::connect(after[0].address())
            .await
            .expect("the service must answer at the address handed to plans");
        let read = docker(&[
            "exec",
            &service.container,
            "redis-cli",
            "get",
            "kingdom-restart-probe",
        ])
        .await
        .expect("read back");
        assert_eq!(
            read.trim(),
            "survived",
            "a restart must not cost the King his data"
        );

        reconcile(Vec::new()).await;
        let _ = docker(&["rm", "-f", &service.container]).await;
        let _ = docker(&["volume", "rm", "kingdom-services-restart-test-data"]).await;
        let _ = docker(&["network", "rm", &network_name(&city_key(&root))]).await;
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Two reconciles at once raise one container, not two.
    ///
    /// A kingdom opening, a plan opening and a turn beginning can all land
    /// within a second of each other. Without the `RAISING` guard two of them
    /// reach `docker run` for the same container name and the loser takes a
    /// bare "name already in use", which reads as a Kingdom bug rather than a
    /// race. Fired concurrently rather than in sequence, because in sequence
    /// this passes with no guard at all.
    #[tokio::test]
    #[ignore = "needs a running Docker daemon"]
    async fn concurrent_reconciles_raise_one_container() {
        let root = std::env::temp_dir().join("kingdom-services-race-test");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join(".kingdom")).expect("make the city");
        std::fs::write(
            root.join(kingdom_core::services::MANIFEST_PATH),
            r#"
            [[service]]
            name  = "cache"
            image = "redis:7-alpine"
            port  = 6379
            "#,
        )
        .expect("write the manifest");

        let container = container_name(&city_key(&root), "cache");
        let _ = docker(&["rm", "-f", &container]).await;

        let running: Vec<_> = (1..=4)
            .map(|n| {
                let plan = PlanId::new(format!("race-plan-{n}"));
                let root = root.clone();
                tokio::spawn(async move { reconcile(vec![(plan, root)]).await })
            })
            .collect();
        for handle in running {
            handle.await.expect("no reconcile may panic");
        }

        // One container by that name, and it is up.
        let found = docker(&[
            "ps",
            "--all",
            "--filter",
            &format!("name=^{container}$"),
            "--format",
            "{{.Names}}",
        ])
        .await
        .expect("docker must answer");
        assert_eq!(
            found.lines().filter(|l| !l.trim().is_empty()).count(),
            1,
            "four concurrent reconciles must leave exactly one container: {found}"
        );
        assert_eq!(container_state(&container).await, ContainerState::Running);

        reconcile(Vec::new()).await;
        let _ = docker(&["rm", "-f", &container]).await;
        let _ = docker(&["network", "rm", &network_name(&city_key(&root))]).await;
        let _ = std::fs::remove_dir_all(&root);
    }
}

/// The five-agent rehearsal, driven end to end against real Docker.
///
/// `cargo test -p kingdom-app --features ssr --no-default-features -- \
///  --ignored five_agents_share_one_database`
///
/// Separate from the lifecycle test above because it proves a different claim:
/// not that one service starts and stops correctly, but that *five plans at
/// once* get one database and can see each other's writes. That is the whole
/// feature, and it is the one thing a unit test cannot reach.
#[cfg(test)]
mod five_agents {
    use super::*;

    #[tokio::test]
    #[ignore = "needs a running Docker daemon and the seeded `shopfront` realm"]
    async fn five_agents_share_one_database() {
        let Ok(city) = std::env::var("SHOPFRONT_CITY") else {
            eprintln!("set SHOPFRONT_CITY to the seeded shopfront city directory");
            return;
        };
        let root = std::path::PathBuf::from(city);

        // Five plans, opened one after another as the King would -- each open
        // reconciling against the population so far, which is exactly what
        // `begin_plan` does.
        let plans: Vec<PlanId> = (1..=5).map(|n| PlanId::new(format!("agent-{n}"))).collect();

        let mut addresses = Vec::new();
        for open_so_far in 1..=plans.len() {
            let population: Vec<(PlanId, PathBuf)> = plans[..open_so_far]
                .iter()
                .map(|plan| (plan.clone(), root.clone()))
                .collect();
            reconcile(population).await;
            let up = running_in(&root);
            assert_eq!(up.len(), 1, "the manifest declares exactly one service");
            addresses.push(up[0].address());
        }

        // One address, five plans. If this fails, each agent got its own
        // database and the entire feature is decorative.
        assert!(
            addresses.windows(2).all(|w| w[0] == w[1]),
            "all five agents must be handed the SAME address, got {addresses:?}"
        );
        assert_eq!(users_of(&root, "db"), 5, "five plans must be counted");

        // And they are all counted against one container.
        let running = running_in(&root);
        assert_eq!(running.len(), 1);
        println!(
            "five agents sharing {} at {}",
            running[0].image,
            running[0].address()
        );

        // Four leaving does not take the database away from the fifth.
        reconcile(vec![(plans[4].clone(), root.clone())]).await;
        assert_eq!(users_of(&root, "db"), 1);
        assert_eq!(
            container_state(&running[0].container).await,
            ContainerState::Running,
            "the last agent is still working -- the database must still be up"
        );

        // The last one out stops it.
        reconcile(Vec::new()).await;
        assert_eq!(users_of(&root, "db"), 0);
        assert_eq!(
            container_state(&running[0].container).await,
            ContainerState::Stopped
        );

        let _ = docker(&["rm", "-f", &running[0].container]).await;
        let _ = docker(&["volume", "rm", "shopfront-db"]).await;
        let _ = docker(&["network", "rm", &network_name(&city_key(&root))]).await;
    }
}
