//! Docker: the one runtime a shared resource can run on today.
//!
//! # What lives here and what does not
//!
//! Everything that knows a container exists -- the `docker` subprocess, the
//! network per scope, the `/24` a city's addresses come out of, the labels, the
//! wait for a port to answer. Nothing here counts users, decides which scopes a
//! plan draws from, or knows what a plan is. That half is
//! [`super`], and the split is the point: the reference-counting invariant is
//! about *sharing*, and was previously interleaved with a conversation about
//! `docker run`.
//!
//! [`super`] matches on [`kingdom_core::services::ResourceKind`] and calls in
//! here. There is deliberately no trait: with one runtime a trait would be one
//! implementation behind a `dyn`, and an exhaustive `match` is the stronger
//! guarantee anyway -- a second kind makes the compiler name every place that
//! has to decide something, where a driver shaped wrongly for it would compile
//! and be wrong at runtime.
//!
//! # The measurement this rests on
//!
//! A plan's namespace can already reach a container by its bridge address.
//! `slirp4netns --disable-host-loopback` blocks `127.0.0.1` and nothing else,
//! so a published port is provably unreachable from inside a plan and a bridge
//! address is not. Measured before any of this was written:
//!
//! ```text
//!   namespace -> 172.17.0.2:5432    (default bridge)     reachable
//!   namespace -> 172.31.77.10:27017 (custom subnet)      reachable
//!   namespace -> 127.0.0.1:47777    (a published port)   REFUSED
//! ```
//!
//! So nothing is ever published to the King's loopback: a service takes no port
//! from him, and `docker network create --subnet` installs a host route that
//! lets him open the address directly anyway.

use super::{path_hash, which, RunningService, Scope, ServiceError};
use kingdom_core::services::{data_dir_for, known_image, ResourceKind, ServiceSpec};
use std::path::Path;

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
/// of `namespaces::net::add_forward`'s port draw.
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

/// Brings a scope's containers up, or adopts them if they are already up.
///
/// # Why a batch, and why it does not stop at the first failure
///
/// The network and the `/24` are one thought for the whole scope, so they are
/// settled once here rather than per resource. And a scope's containers are
/// raised as far as they will go: a resource that came up before its neighbour
/// failed **is standing**, and the caller must be told about it so that it is
/// recorded and can later be swept. Returning `Err` and dropping the list --
/// which the older shape did with `?` -- left a container running that nothing
/// in the process knew it had raised, and so nothing would ever stop.
///
/// Each spec carries its index in the manifest, which is where its address
/// comes from: see [`service_address`].
pub(super) async fn raise(scope: &Scope, key: &str, specs: &[(usize, &ServiceSpec)]) -> Raised {
    if which("docker").is_none() {
        return Raised::failed(unavailable_missing());
    }

    let network = network_name(key);
    let subnet = match ensure_network(&network).await {
        Ok(subnet) => subnet,
        // Nothing is up, and nothing can be: no container was started, so there
        // is nothing for the caller to record.
        Err(e) => return Raised::failed(e),
    };

    let mut up = Vec::new();
    for (index, spec) in specs {
        match ensure_one(scope, key, &network, subnet, *index, spec).await {
            Ok(service) => up.push(service),
            // Stop asking for more -- a daemon that failed one will likely fail
            // the next -- but hand back what did come up.
            Err(e) => {
                return Raised {
                    up,
                    failure: Some(e),
                }
            }
        }
    }

    Raised { up, failure: None }
}

/// What a raise achieved, and what went wrong -- which are not exclusive.
///
/// A `Result` cannot say "three of these five are standing and here is why the
/// fourth is not", and that is exactly the state a half-finished raise leaves
/// behind. See [`raise`].
pub(super) struct Raised {
    /// Everything that is now running, in manifest order.
    pub up: Vec<RunningService>,
    /// Why the rest are not, if anything went wrong.
    pub failure: Option<ServiceError>,
}

impl Raised {
    /// Nothing came up, for this reason.
    fn failed(e: ServiceError) -> Self {
        Raised {
            up: Vec::new(),
            failure: Some(e),
        }
    }
}

/// Stops one container, keeping it and its volume.
///
/// `stop`, never `rm`: the King's data is the whole reason the resource
/// existed, and a stopped container starts again with everything in it.
pub(super) async fn stop(service: &RunningService) {
    let _ = docker(&["stop", &service.handle]).await;
}

/// Why nothing can be running, or `None` if the daemon answered.
pub(super) async fn trouble() -> Option<String> {
    if which("docker").is_none() {
        return Some(unavailable_missing().to_string());
    }
    match docker(&["version", "--format", "{{.Server.Version}}"]).await {
        Ok(_) => None,
        Err(e) => Some(unavailable_unreachable(&e).to_string()),
    }
}

/// Docker is not installed, said in the words the King acts on.
///
/// Composed here rather than in `ServiceError` so that a runtime owns its own
/// diagnosis: a second kind must not be able to produce a refusal telling the
/// King to install something it does not use.
fn unavailable_missing() -> ServiceError {
    ServiceError::Unavailable(format!(
        "this project declares shared services in `{path}`, but Docker is not \
         installed -- so there is nothing to run them. Install Docker, or \
         remove that file if the project no longer needs it.",
        path = kingdom_core::services::MANIFEST_PATH
    ))
}

/// Docker is installed and not answering.
fn unavailable_unreachable(detail: &str) -> ServiceError {
    ServiceError::Unavailable(format!(
        "Docker is installed but not answering ({detail}). It is usually the \
         daemon not running: try `sudo systemctl start docker`."
    ))
}

/// How the King reads a container's log himself.
///
/// Built here rather than in the browser, which is where it used to be
/// formatted: a Docker command composed by a component that cannot run one is a
/// command that would be wrong for any other kind of resource.
pub(super) fn log_hint(container: &str) -> String {
    format!("docker logs {container}")
}

/// The Docker network for a city.
fn network_name(city_key: &str) -> String {
    format!("kingdom-{city_key}")
}

/// The container name for one service.
pub(super) fn container_name(city_key: &str, service: &str) -> String {
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
/// Shelling out rather than taking on `bollard`, for the reason `namespaces/`
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
        .map_err(|e| unavailable_unreachable(&e))?;

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
    // The container half of the declaration. `raise` only ever hands this
    // function a docker resource -- the caller matched on the kind to group
    // them -- so anything else is a bug in that grouping rather than a
    // manifest the King wrote.
    let ResourceKind::Docker(docker_spec) = &spec.kind;

    let container = container_name(key, &spec.name);
    let host = service_address(subnet, index);

    let service = RunningService {
        name: spec.name.clone(),
        what: docker_spec.what(),
        host: host.clone(),
        port: spec.port,
        handle: container.clone(),
        kind: spec.kind.wire_name(),
        scope: scope.kind(),
        key: key.to_string(),
    };

    match container_state(&container).await {
        // Up already: this is a plan two-through-five, or a server restart
        // finding what the last one left. Adopted, deliberately -- see the
        // module docs on why this differs from `namespaces::net::reclaim_previous`.
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
    if let Some(volume) = &docker_spec.volume {
        args.push("--volume".into());
        args.push(format!("{volume}:{}", data_dir_for(&docker_spec.image)));
    }
    // What the image needs in its **own** environment simply to start. Nothing
    // here is ever shown to an agent -- it is the opposite direction of travel
    // from the manifest's retired `env`. Without it `postgres:16` exits 1 on
    // first boot complaining about POSTGRES_PASSWORD, and all the King sees is
    // "never answered on port 5432".
    if let Some(known) = known_image(&docker_spec.image) {
        for (key, value) in known.boot {
            args.push("--env".into());
            args.push(format!("{key}={value}"));
        }
    }
    args.push(docker_spec.image.clone());

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
        if container_state(&service.handle).await != ContainerState::Running {
            return Err(ServiceError::Failed {
                name: service.name.clone(),
                detail: format!(
                    "the container stopped while starting; `{}` may say why",
                    log_hint(&service.handle)
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
        hint: log_hint(&service.handle),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::city_key;

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

    /// A container is never published to the host.
    ///
    /// Pinned as a test because the temptation to add `-p` is exactly what the
    /// measurement in the module docs rules out: a published port is on the
    /// King's loopback, which is the one address a plan's namespace provably
    /// cannot reach.
    #[test]
    fn nothing_is_published_to_the_kings_loopback() {
        let source = include_str!("docker.rs");
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
    // The sharing half: these drive the real lifecycle end to end, so they
    // go in through the same entry points the server does.
    use crate::services::{address_for, city_key, reconcile, registry, running_in, users_of};
    use kingdom_core::PlanId;

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
            volume = "kingdom-services-real-test-data"

            # Declared exactly as the form now writes one: an image, a name and
            # nothing else. Postgres exits 1 on first boot without a password,
            # so this service comes up only if `known_image`'s boot environment
            # is actually passed to `docker run` -- which is the whole point of
            # the table. Its port is left out too, because the form fills that
            # in from the image.
            [[service]]
            name  = "pg"
            image = "postgres:16"
            port  = 5432
            "#,
        )
        .expect("write the manifest");

        let one = PlanId::new("real-plan-1");
        let two = PlanId::new("real-plan-2");

        // First plan starts the services.
        reconcile(vec![(one.clone(), root.clone())]).await;
        let up = running_in(&root);
        assert_eq!(up.len(), 2, "the manifest declares a cache and a postgres");
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

        // And so does the Postgres, which is the case that could not start
        // before: nothing in the manifest gives it a password, so it comes up
        // only because `known_image` hands one to `docker run`.
        let postgres = up[1].clone();
        assert_eq!(postgres.name, "pg");
        tokio::net::TcpStream::connect(postgres.address())
            .await
            .expect("postgres must boot from the image's own defaults");

        // And that address is what a plan is told. `one` has no namespace here,
        // so it is told the container's address -- the shared-network answer,
        // and the fallback an isolated plan gets if its loopback relay could
        // not be raised.
        assert_eq!(address_for(&one, &service), service.address());

        // Second plan finds it standing rather than starting another.
        reconcile(vec![
            (one.clone(), root.clone()),
            (two.clone(), root.clone()),
        ])
        .await;
        let again = running_in(&root);
        assert_eq!(again[0].handle, service.handle, "adopted, not restarted");
        assert_eq!(users_of(&root, "cache"), 2);

        // Leave something behind, to prove the volume outlives the container.
        let written = docker(&[
            "exec",
            &service.handle,
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
        let _ = docker(&["exec", &service.handle, "redis-cli", "save"]).await;

        // One plan leaving does not stop it.
        reconcile(vec![(two.clone(), root.clone())]).await;
        assert_eq!(users_of(&root, "cache"), 1);
        assert_eq!(
            container_state(&service.handle).await,
            ContainerState::Running
        );

        // The last plan leaving does.
        reconcile(Vec::new()).await;
        assert_eq!(users_of(&root, "cache"), 0);
        assert_eq!(
            container_state(&service.handle).await,
            ContainerState::Stopped
        );

        // A later plan gets it back, with its data.
        let three = PlanId::new("real-plan-3");
        reconcile(vec![(three.clone(), root.clone())]).await;
        let restarted = running_in(&root);
        assert_eq!(restarted[0].handle, service.handle);
        let read = docker(&["exec", &service.handle, "redis-cli", "get", "kingdom-probe"])
            .await
            .expect("read back");
        assert_eq!(
            read.trim(),
            "survived",
            "the volume must outlive the container -- that is the King's data"
        );

        // Tidy up: this test owns every name it used.
        reconcile(Vec::new()).await;
        let _ = docker(&["rm", "-f", &service.handle]).await;
        let _ = docker(&["rm", "-f", &postgres.handle]).await;
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
            &service.handle,
            "redis-cli",
            "set",
            "kingdom-restart-probe",
            "survived",
        ])
        .await;
        assert!(written.is_ok(), "could not write: {written:?}");
        let _ = docker(&["exec", &service.handle, "redis-cli", "save"]).await;

        // The server stops. The registry goes with the process; the container
        // is left stopped, as the last release would have left it.
        let _ = docker(&["stop", &service.handle]).await;
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
        assert_eq!(after[0].handle, service.handle, "adopted, not recreated");
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
            &service.handle,
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
        let _ = docker(&["rm", "-f", &service.handle]).await;
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
    use crate::services::{city_key, reconcile, running_in, users_of};
    use kingdom_core::PlanId;
    use std::path::PathBuf;

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
            running[0].what,
            running[0].address()
        );

        // Four leaving does not take the database away from the fifth.
        reconcile(vec![(plans[4].clone(), root.clone())]).await;
        assert_eq!(users_of(&root, "db"), 1);
        assert_eq!(
            container_state(&running[0].handle).await,
            ContainerState::Running,
            "the last agent is still working -- the database must still be up"
        );

        // The last one out stops it.
        reconcile(Vec::new()).await;
        assert_eq!(users_of(&root, "db"), 0);
        assert_eq!(
            container_state(&running[0].handle).await,
            ContainerState::Stopped
        );

        let _ = docker(&["rm", "-f", &running[0].handle]).await;
        let _ = docker(&["volume", "rm", "shopfront-db"]).await;
        let _ = docker(&["network", "rm", &network_name(&city_key(&root))]).await;
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
    use crate::services::{address_for, declare, reconcile, running_in, users_of_key};
    use kingdom_core::services::ServiceScope;
    use kingdom_core::PlanId;

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
                port: 6379,
                kind: ResourceKind::Docker(kingdom_core::services::DockerSpec {
                    image: "redis:7-alpine".to_string(),
                    volume: None,
                }),
            },
        )
        .expect("the King's own manifest must be writable");

        // Two projects that declare nothing of their own. Whatever they reach
        // is the machine's, which is the point.
        let one = tempfile::tempdir().expect("project one");
        let two = tempfile::tempdir().expect("project two");
        let alice = PlanId::new("host-scope-alice");
        let bob = PlanId::new("host-scope-bob");

        // The *sharing* raise -- the parent's, which records drawers -- not this
        // module's, which only talks to Docker. Named in full because the two
        // are one word apart and mean different things.
        let raised = crate::services::raise(&Scope::Host, &[alice.clone()].into_iter().collect())
            .await
            .expect("the first plan raises it");
        assert_eq!(raised.len(), 1);
        assert_eq!(raised[0].scope, ServiceScope::Host);
        let container = raised[0].handle.clone();
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

        // And both are told the same address. Neither plan has a namespace
        // here, so both get the container's own address; an isolated plan would
        // be told its own loopback instead.
        for plan in [&alice, &bob] {
            assert_eq!(
                address_for(plan, &raised[0]),
                raised[0].address(),
                "a plan on the machine's network reaches the container directly"
            );
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
