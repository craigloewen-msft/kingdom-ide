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
//! # What this is not
//!
//! **Not a sandbox.** A container Kingdom starts is an ordinary container,
//! visible to the whole machine and to `docker ps`, and a plan can still run
//! `docker` itself and do as it likes. Like [`crate::netns`], this is
//! coordination, not containment, and saying so plainly is worth more than a
//! guarantee that does not hold.

use kingdom_core::services::{ServiceManifest, ServiceSpec};
use kingdom_core::PlanId;
use std::collections::{HashMap, HashSet};
use std::path::Path;
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

/// The label carrying the city a container belongs to.
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

#[derive(Default)]
struct Registry {
    /// `(city key, service name)` -> the running service.
    running: HashMap<(String, String), RunningService>,
    /// `(city key, service name)` -> the plans using it.
    ///
    /// This is the reference count, kept as the set of plan ids rather than an
    /// integer so that a plan closed twice cannot decrement it twice -- which
    /// would stop a database five other plans were still using.
    users: HashMap<(String, String), HashSet<PlanId>>,
}

fn registry() -> std::sync::MutexGuard<'static, Registry> {
    let cell = SERVICES.get_or_init(|| Mutex::new(Registry::default()));
    match cell.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
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

/// Reads a city's manifest, if it has one.
///
/// A missing file is `Ok(empty)` rather than an error: almost every project has
/// no manifest, and that is not a fault to report.
pub fn manifest_of(city_root: &Path) -> Result<ServiceManifest, ServiceError> {
    let path = city_root.join(kingdom_core::services::MANIFEST_PATH);
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Ok(ServiceManifest::default());
    };
    kingdom_core::services::parse(&text).map_err(|e| ServiceError::Manifest(e.to_string()))
}

/// Makes sure every service this city declares is up, and records that this
/// plan is drawing from it.
///
/// Idempotent in both halves: a service already running is adopted rather than
/// restarted, and a plan already registered as a drawer stays exactly one
/// user. Plans two through five therefore find the service standing and pay only
/// for a `docker inspect`.
pub async fn ensure(plan: &PlanId, city_root: &Path) -> Result<Vec<RunningService>, ServiceError> {
    let manifest = manifest_of(city_root)?;
    if manifest.is_empty() {
        return Ok(Vec::new());
    }

    if which("docker").is_none() {
        return Err(ServiceError::DockerMissing);
    }

    let key = city_key(city_root);
    let network = network_name(&key);
    let subnet = ensure_network(&network).await?;

    let mut up = Vec::new();
    for (index, spec) in manifest.services.iter().enumerate() {
        let service = ensure_one(&key, &network, subnet, index, spec).await?;
        {
            let mut registry = registry();
            let id = (key.clone(), spec.name.clone());
            registry.running.insert(id.clone(), service.clone());
            registry.users.entry(id).or_default().insert(plan.clone());
        }
        up.push(service);
    }
    Ok(up)
}

/// The environment a plan's tools get for this city's services.
///
/// Read from the registry rather than started here, so this is cheap enough to
/// call on every command. A city with nothing running yields nothing, which is
/// what makes it safe to apply unconditionally in `tools::child_environment`.
pub fn environment(city_root: &Path) -> Vec<(String, String)> {
    let Ok(manifest) = manifest_of(city_root) else {
        return Vec::new();
    };
    if manifest.is_empty() {
        return Vec::new();
    }

    let key = city_key(city_root);
    let registry = registry();
    let mut out = Vec::new();
    for spec in &manifest.services {
        if let Some(running) = registry.running.get(&(key.clone(), spec.name.clone())) {
            out.extend(spec.environment(&running.host, running.port));
        }
    }
    out
}

/// What this city has standing, for the badge and the system prompt.
pub fn running_in(city_root: &Path) -> Vec<RunningService> {
    let key = city_key(city_root);
    let registry = registry();
    let mut out: Vec<RunningService> = registry
        .running
        .iter()
        .filter(|((city, _), _)| city == &key)
        .map(|(_, service)| service.clone())
        .collect();
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// How many plans are using a given service right now.
pub fn users_of(city_root: &Path, service: &str) -> usize {
    let key = (city_key(city_root), service.to_string());
    registry().users.get(&key).map_or(0, HashSet::len)
}

/// The address of one named service, or `None` if it is not up.
pub fn address_of(city_root: &Path, service: &str) -> Option<String> {
    let key = (city_key(city_root), service.to_string());
    registry().running.get(&key).map(RunningService::address)
}

/// Notes that a plan is finished with this city's services, stopping any that
/// nobody is left drawing from.
///
/// Called when a plan is merged or archived, beside `netns::shutdown`. The
/// container is **stopped, not removed**, and its named volume is left alone:
/// the King's data is the whole reason the service existed, and losing it
/// because five agents finished their work would be the worst possible
/// interpretation of "tear down".
pub async fn release(plan: &PlanId, city_root: &Path) {
    let key = city_key(city_root);

    let orphaned: Vec<RunningService> = {
        let mut registry = registry();
        let mut orphaned = Vec::new();
        let ids: Vec<(String, String)> = registry
            .users
            .keys()
            .filter(|(city, _)| city == &key)
            .cloned()
            .collect();

        for id in ids {
            let Some(users) = registry.users.get_mut(&id) else {
                continue;
            };
            users.remove(plan);
            if users.is_empty() {
                registry.users.remove(&id);
                if let Some(service) = registry.running.remove(&id) {
                    orphaned.push(service);
                }
            }
        }
        orphaned
    };

    for service in orphaned {
        let _ = docker(&["stop", &service.container]).await;
    }
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
    city_key: &str,
    network: &str,
    subnet: u8,
    index: usize,
    spec: &ServiceSpec,
) -> Result<RunningService, ServiceError> {
    let container = container_name(city_key, &spec.name);
    let host = service_address(subnet, index);

    let service = RunningService {
        name: spec.name.clone(),
        image: spec.image.clone(),
        host: host.clone(),
        port: spec.port,
        container: container.clone(),
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
        format!("{LABEL_CITY}={city_key}"),
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
        let up = ensure(&one, &root).await.expect("the service must come up");
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
        let again = ensure(&two, &root)
            .await
            .expect("the second plan draws too");
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
        release(&one, &root).await;
        assert_eq!(users_of(&root, "cache"), 1);
        assert_eq!(
            container_state(&service.container).await,
            ContainerState::Running
        );

        // The last plan leaving does.
        release(&two, &root).await;
        assert_eq!(users_of(&root, "cache"), 0);
        assert_eq!(
            container_state(&service.container).await,
            ContainerState::Stopped
        );

        // A later plan gets it back, with its data.
        let three = PlanId::new("real-plan-3");
        let restarted = ensure(&three, &root).await.expect("the service comes back");
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
        release(&three, &root).await;
        let _ = docker(&["rm", "-f", &service.container]).await;
        let _ = docker(&["volume", "rm", "kingdom-services-real-test-data"]).await;
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

        // Five plans, opened one after another as the King would.
        let plans: Vec<PlanId> = (1..=5).map(|n| PlanId::new(format!("agent-{n}"))).collect();

        let mut addresses = Vec::new();
        for plan in &plans {
            let up = ensure(plan, &root).await.expect("the service must come up");
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
        for plan in &plans[..4] {
            release(plan, &root).await;
        }
        assert_eq!(users_of(&root, "db"), 1);
        assert_eq!(
            container_state(&running[0].container).await,
            ContainerState::Running,
            "the last agent is still working -- the database must still be up"
        );

        // The last one out stops it.
        release(&plans[4], &root).await;
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
