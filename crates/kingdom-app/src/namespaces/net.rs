//! A network of a plan's own: namespaces, and the ports they hold.
//!
//! # The problem
//!
//! Two agents both run `cargo leptos serve`; both want `:3000`; the second one
//! dies. That is the collision AGENTS.md names as the reason this product
//! exists, and until now Kingdom could only *watch* it happen.
//!
//! A plan opened with [`Isolation::Isolated`] gets a Linux network namespace
//! of its own. Inside it, `127.0.0.1:3000` is a different socket from the one
//! on the King's machine and from every other plan's, so nothing collides and
//! no agent has to be told to pick another port.
//!
//! # How it is put together
//!
//! Three processes per isolated plan, none of them privileged:
//!
//! ```text
//!   holder     unshare --user --net --fork sleep infinity
//!              Owns the namespace. Exists only so the namespace outlives the
//!              tool call that made it -- a namespace with no process in it is
//!              collected by the kernel.
//!
//!   slirp4netns  Gives the namespace a way OUT. A fresh namespace has only
//!              `lo`: no DNS, no crates.io, no git. slirp4netns puts a `tap0`
//!              at 10.0.2.100/24 in it and NATs from the host side, in
//!              userspace, with no root anywhere.
//!
//!   nsenter    How everything else gets IN. Prefixed to bash, tmux and
//!              Chrome so they start inside the namespace.
//! ```
//!
//! # Two things measured rather than assumed
//!
//! **`nsenter` needs `--preserve-credentials`.** Re-entering a namespace you
//! made yourself otherwise fails with `setgroups failed: Operation not
//! permitted`, because entering a user namespace ordinarily drops supplementary
//! groups and that is itself a privileged act. This one flag is the difference
//! between the feature working and not, and it is in none of the obvious
//! documentation.
//!
//! **Port discovery needs no entry at all.** `/proc/<holder>/net/tcp`, read from
//! the host, is the *namespace's* table, because `/proc/<pid>/net` is per
//! network namespace. So finding out what a plan is listening on costs one file
//! read and no subprocess. See [`listeners`] for the trap in that.
//!
//! # The namespace's loopback is a place to *put* things, not only to avoid
//!
//! `--disable-host-loopback` makes `127.0.0.1` inside a plan a dead end, and
//! [`crate::services`] is built around that: a shared database is reached at
//! its container address because a published port provably cannot be reached
//! from in here.
//!
//! But that same dead end is *empty and the plan's own*, which makes it the
//! best possible place to put the database. [`open_wells`] stands a relay on
//! `127.0.0.1:<the service's own port>` inside the namespace and splices it to
//! the container -- so `mongodb://localhost:27017` is true in this plan, false
//! on the King's machine, and true again with a different database in the plan
//! next door. Measured before it was written: a MongoDB handshake completed
//! through exactly this hop from inside a real plan.
//!
//! It is the same [`spawn_relay`] the forwarding path uses, pointed the other
//! way -- inwards from the namespace rather than outwards to `tap0`.
//!
//! **A forward needs a relay, because it can only ever land on `tap0`.**
//! `add_hostfwd` NATs to the namespace's `tap0` address, but almost everything
//! a plan runs binds `127.0.0.1` instead -- a different socket even inside the
//! same namespace. Measured directly: slirp accepts a forward to a
//! loopback-bound server and it then answers nothing, a silent wrong answer
//! rather than a refusal. [`spawn_relay`] puts this same binary back into the
//! namespace, in a hidden `--relay` mode (see [`run_relay`]), hopping `tap0:P`
//! to `127.0.0.1:P` -- on the same port number both sides, which is also what
//! lets Chrome's own CDP port cross the boundary with no rewriting anywhere.
//! Skipped when the relay cannot itself bind `tap0:P`: that failure means the
//! server already answers there directly, and one relay too many would be
//! wrong rather than merely redundant.
//!
//! **The namespace lives in a process, not a name.** A daemon found again by a
//! stable name -- tmux's socket, most notably -- says nothing about which
//! *generation* of namespace it was last talking to. See
//! `tools::tmux::ensure_server` for the mismatch this causes and the fix.
//!
//! # What this is not
//!
//! **Not a security boundary.** A process in here still has the whole
//! filesystem and the King's own uid. It cannot take a port from another plan;
//! it can still delete his home directory. This is collision avoidance, and
//! saying so plainly is worth more than a guarantee that does not hold --
//! `tools::Sandbox::root` makes the same admission about paths.

use super::{kill, lock, sanitise, wait_for_child, Namespace, NetworkError, Request};
use kingdom_core::PlanId;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

/// Where forwarded ports are drawn from.
///
/// High enough to sit above anything a project picks for itself, and above the
/// usual ephemeral range's busiest part, so a forward is unlikely to collide
/// with a socket the King's own machine opened a moment ago. Collisions are
/// retried rather than prevented -- see [`Namespace::forward`].
const HOST_PORT_LOW: u16 = 40000;
const HOST_PORT_HIGH: u16 = 49999;

/// How many times a port draw is retried before giving up.
const PORT_ATTEMPTS: usize = 40;

/// Ports inside the namespace that are never forwarded to the King's badge.
///
/// Not a fixed list: the one port that must stay off the badge is Chrome's own
/// CDP port, and that number is chosen per plan at launch time rather than
/// known up front. It is tracked on [`Namespace::cdp_port`] instead and
/// excluded there. This constant now names the *idea*, not any content -- kept
/// so a future private port has somewhere obvious to be added.
const NEVER_FORWARD: &[u16] = &[];

/// The address slirp4netns gives the namespace on `tap0`.
///
/// Fixed by slirp's own defaults rather than chosen here, and named so the
/// forwarding call and any future diagnostic cannot drift apart.
const GUEST_ADDR: &str = "10.0.2.100";

/// One shared service standing on a plan's own loopback.
///
/// # Why the target is kept and not just the pid
///
/// A plan's loopback has one socket per port, so only the **first** service on
/// a given port can be relayed -- and the King's own Redis and a project's are
/// both `:6379` by default, which makes that an ordinary case rather than a
/// corner. Recording only the port, as this once did, meant a second Redis was
/// told `127.0.0.1:6379` as well, and every read and write it made landed
/// silently in the *first* one's data.
///
/// So a well remembers which container it actually reaches, and
/// [`crate::services::address_for`] asks about that container rather than about
/// the number.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Well {
    /// The relay's pid inside the namespace, so it can be killed with the rest.
    pub(super) pid: u32,
    /// The container address it forwards to, as `host:port`.
    target: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Forward {
    pub(super) host_port: u16,
    /// slirp's own handle for this rule, needed to remove it again.
    id: i64,
    /// The relay's pid, inside the namespace, hopping `tap0:guest_port` to
    /// `127.0.0.1:guest_port` -- present only when one was needed. See
    /// [`relay_needed`] for when it is not: a server that already bound
    /// `0.0.0.0` or `tap0` itself needs no help reaching the forward.
    relay: Option<u32>,
}

impl Namespace {
    /// Ports something inside the namespace is listening on.
    ///
    /// Read from the host, with no subprocess: `/proc/<pid>/net` is per network
    /// namespace, so this file *is* the namespace's table.
    ///
    /// **A well's port is not one of these, and the distinction is the whole
    /// reason `wells` is a separate field.** A well relay listens inside the
    /// namespace like anything else, so `/proc` reports it -- but it is the
    /// King's own database seen sideways, not a server this plan wrote.
    /// Forwarded, it would put a host port on the ports badge that opens a
    /// MongoDB wire protocol socket in a browser tab. Dropped here rather than
    /// hidden in [`forwards_of`], because unlike Chrome's CDP port there is no
    /// reason to carry it out of the namespace at all.
    fn listeners(&self) -> Vec<u16> {
        let mut ports = Vec::new();
        for family in ["tcp", "tcp6"] {
            let path = format!("/proc/{}/net/{family}", self.holder);
            if let Ok(text) = std::fs::read_to_string(&path) {
                ports.extend(parse_listeners(&text));
            }
        }
        ports.sort_unstable();
        ports.dedup();
        forwardable(ports, &self.wells)
    }
}

/// Drops the ports that are listening but are not the plan's own server.
///
/// Pure, and separate from the `/proc` read for the reason [`parse_listeners`]
/// is: the suite runs on a bare machine, and this is the half worth pinning.
/// A well port that slipped through here would be forwarded to the King's
/// loopback and offered on his ports badge as something to open in a browser --
/// where it would answer his click in the MongoDB wire protocol.
fn forwardable(mut ports: Vec<u16>, wells: &HashMap<u16, Well>) -> Vec<u16> {
    ports.retain(|p| !NEVER_FORWARD.contains(p) && !wells.contains_key(p));
    ports
}

/// Pulls the listening ports out of a `/proc/net/tcp` table.
///
/// Split from the reading so it can be tested without a kernel -- the suite has
/// to run on a bare machine, per AGENTS.md.
///
/// Only state `0A` (`TCP_LISTEN`) counts. Every other line is an established
/// connection or a socket on its way down, and forwarding to those would
/// produce host ports that answer nothing.
fn parse_listeners(table: &str) -> Vec<u16> {
    table
        .lines()
        .skip(1) // the header
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let local = fields.nth(1)?; // `local_address`, as ADDR:PORT in hex
            let state = fields.nth(1)?; // `st`, two hex digits
            if state != "0A" {
                return None;
            }
            u16::from_str_radix(local.rsplit(':').next()?, 16).ok()
        })
        .filter(|port| *port != 0)
        .collect()
}

/// What a plan currently has forwarded, for the ports badge.
///
/// Chrome's CDP port is excluded, deliberately: it is forwarded like any other
/// listener so the relay can reach it, but it is not a port the King ever
/// wants to click -- it speaks CDP, not HTTP, and showing it would be a badge
/// entry that always fails to open in a browser.
pub fn forwards_of(plan: &PlanId) -> Vec<(u16, u16)> {
    let registry = lock();
    let Some(namespace) = registry.get(plan) else {
        return Vec::new();
    };
    let mut out: Vec<(u16, u16)> = namespace
        .forwards
        .iter()
        .filter(|(guest, _)| Some(**guest) != namespace.cdp_port)
        .map(|(guest, forward)| (*guest, forward.host_port))
        .collect();
    out.sort_unstable();
    out
}

/// Brings the forward table into line with what the plan is actually listening
/// on: adds what is new, removes what has gone.
///
/// Returns true when anything changed, so the caller can avoid publishing an
/// event for a poll that found nothing.
pub async fn reconcile(plan: &PlanId) -> bool {
    let (listening, known, api_socket, holder) = {
        let registry = lock();
        let Some(namespace) = registry.get(plan) else {
            return false;
        };
        (
            namespace.listeners(),
            namespace.forwards.clone(),
            namespace.api_socket.clone(),
            namespace.holder,
        )
    };

    let mut changed = false;

    // Gone: the agent stopped its server, so the host port must stop answering.
    for (guest, forward) in &known {
        if !listening.contains(guest) && remove_hostfwd(&api_socket, forward.id).await.is_ok() {
            if let Some(pid) = forward.relay {
                kill(pid);
            }
            if let Some(ns) = lock().get_mut(plan) {
                ns.forwards.remove(guest);
            }
            changed = true;
        }
    }

    // New: something started listening, so give it a door on the host.
    for guest in listening {
        if known.contains_key(&guest) {
            continue;
        }
        if let Some(forward) = add_forward(&api_socket, holder, guest).await {
            if let Some(ns) = lock().get_mut(plan) {
                ns.forwards.insert(guest, forward);
                changed = true;
            }
        }
    }

    changed
}

/// How often a plan's namespace is asked what it is listening on.
///
/// A poll rather than a subscription because the kernel offers no "a socket
/// started listening" notification for another namespace. One `read` of a small
/// `/proc` file per second per isolated plan is cheap enough that a smarter
/// scheme would be optimising the wrong thing.
const POLL: std::time::Duration = std::time::Duration::from_secs(1);

/// Starts watching a plan's namespace for ports, if nothing is watching yet.
///
/// Idempotent, and that matters: this is called at the top of every turn, and a
/// second watcher would mean two tasks racing to forward the same port.
pub fn watch(plan: &PlanId) {
    {
        let mut watched = match watchers().lock() {
            Ok(w) => w,
            Err(poisoned) => poisoned.into_inner(),
        };
        if !watched.insert(plan.clone()) {
            return;
        }
    }

    let plan = plan.clone();
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(POLL).await;

            // The namespace is gone -- the plan was merged or archived. Stop,
            // and let a future turn start a fresh watcher.
            if !lock().contains_key(&plan) {
                break;
            }

            // Only publish when something actually changed: this runs once a
            // second forever, and pushing an unchanged plan down every open
            // socket would repaint the chamber for nothing.
            if reconcile(&plan).await {
                if let Some(current) = crate::api::snapshot(&plan) {
                    crate::events::publish(&current);
                }
            }
        }

        if let Ok(mut watched) = watchers().lock() {
            watched.remove(&plan);
        }
    });
}

/// Plans that already have a watcher running.
static WATCHERS: OnceLock<Mutex<std::collections::HashSet<PlanId>>> = OnceLock::new();

fn watchers() -> &'static Mutex<std::collections::HashSet<PlanId>> {
    WATCHERS.get_or_init(|| Mutex::new(std::collections::HashSet::new()))
}

/// Picks a free host port, asks slirp to forward it, and puts the relay in
/// place that lets the forward actually answer.
///
/// The retry is the point. A host port is chosen at random rather than
/// allocated, so two plans can pick the same number; rather than coordinate,
/// this asks and moves on when refused, which is both simpler and correct under
/// a race with a process that is not ours at all.
async fn add_forward(api_socket: &std::path::Path, holder: u32, guest: u16) -> Option<Forward> {
    for _ in 0..PORT_ATTEMPTS {
        let host_port = random_port();
        // Bind it ourselves first. slirp would tell us too, but this also
        // catches a port some *other* program on the machine holds, which is
        // the collision that matters to the King. Dropped immediately: this is
        // a test for "is anyone there", and slirp needs the port free to take.
        if std::net::TcpListener::bind(("127.0.0.1", host_port)).is_err() {
            continue;
        }
        if let Ok(id) = add_hostfwd(api_socket, host_port, guest).await {
            let relay = spawn_relay(holder, guest).await;
            return Some(Forward {
                host_port,
                id,
                relay,
            });
        }
    }
    None
}

/// Puts a relay in the namespace that hops `tap0:guest` to `127.0.0.1:guest`,
/// unless the server is already reachable on `tap0` without one.
///
/// **Why this exists at all.** `add_hostfwd` can only ever land traffic on
/// `tap0` -- that is the interface slirp4netns gave the namespace, and the
/// only address it knows how to NAT to. Almost everything a plan runs binds
/// `127.0.0.1` instead, because that is the ordinary default for a dev server,
/// and `tap0` and `127.0.0.1` are different sockets even inside the same
/// namespace. Measured directly: a forward to a loopback-bound server accepts
/// the rule from slirp and then answers nothing. The relay is the missing hop.
///
/// **Why the bind conflict is the detector, not a special case.** If the
/// relay's own attempt to bind `tap0:guest` fails, something is *already*
/// listening there -- the plan's own server bound `0.0.0.0` or `tap0` itself --
/// and forwarding straight to it is both correct and simpler than asking first.
/// One test ("can I bind this address") distinguishes the two cases without
/// guessing from a project's own configuration.
///
/// Spawned as this same binary, re-entered into the namespace by `nsenter`,
/// rather than `socat`: `socat` is not a listed prerequisite in AGENTS.md, and
/// this keeps that list from growing by one for a job the binary can do
/// itself. See `main.rs`'s `--relay` mode.
async fn spawn_relay(holder: u32, guest: u16) -> Option<u32> {
    spawn_relay_between(
        holder,
        &format!("{GUEST_ADDR}:{guest}"),
        &format!("127.0.0.1:{guest}"),
    )
    .await
}

/// The argv that starts one relay inside a plan's namespace.
///
/// Split out from the spawning so the *direction of travel* can be tested
/// without a kernel, which is worth a test of its own: a relay with its bind
/// and target the wrong way round starts happily, listens on an address
/// nothing uses, and does nothing at all. Both callers pass addresses that
/// look alike, so the mistake would be invisible in review.
fn relay_argv(holder: u32, exe: &str, bind: &str, target: &str) -> Vec<String> {
    vec![
        "nsenter".to_string(),
        "--preserve-credentials".to_string(),
        format!("--user=/proc/{holder}/ns/user"),
        format!("--net=/proc/{holder}/ns/net"),
        "--".to_string(),
        exe.to_string(),
        "--relay".to_string(),
        bind.to_string(),
        target.to_string(),
    ]
}

/// Spawns one relay inside a plan's namespace, from any address to any other.
///
/// The general form of [`spawn_relay`], which travels outwards to `tap0`, and
/// of [`open_wells`], which travels inwards from the loopback. Returns the pid
/// only when the process is still alive after its bind: an immediate exit means
/// the bind was refused, and what that *means* differs between the two callers,
/// so each interprets `None` for itself.
async fn spawn_relay_between(holder: u32, bind: &str, target: &str) -> Option<u32> {
    let exe = std::env::current_exe().ok()?;
    let argv = relay_argv(holder, &exe.to_string_lossy(), bind, target);

    let mut child = tokio::process::Command::new(&argv[0])
        .args(&argv[1..])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(false)
        .spawn()
        .ok()?;
    let pid = child.id()?;

    // Give the relay a moment to either settle into `accept()` or fail its
    // bind. Short: this only has to outlast the bind syscall, not the
    // process's whole life, and every forward already waits on this before it
    // is usable.
    tokio::time::sleep(std::time::Duration::from_millis(150)).await;

    match child.try_wait() {
        // Exited already -- the bind was refused, because something is already
        // listening there. For a forward that is the server itself on `tap0`
        // and no relay is wanted; for a well it means the address is taken and
        // the caller must not claim it works.
        Ok(Some(_)) => None,
        // Still running: the bind succeeded and it is now relaying.
        Ok(None) => Some(pid),
        // Could not even ask -- treat as "no relay", the safer default: a
        // forward with no relay either works already or answers nothing, and
        // the latter is diagnosable, where a relay pid we cannot account for
        // is a leak.
        Err(_) => {
            kill(pid);
            None
        }
    }
}

/// A port in the forwarding range.
///
/// Seeded from the clock rather than pulled from a crate: this needs to be
/// unpredictable enough to avoid a collision, which is a much weaker
/// requirement than randomness, and it saves a dependency. Collisions are
/// caught and retried by the caller regardless.
fn random_port() -> u16 {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as u64)
        .unwrap_or(0);
    let mixed = nanos.wrapping_mul(6364136223846793005).rotate_left(17);
    HOST_PORT_LOW + (mixed % port_span()) as u16
}

/// How many ports the forwarding range holds.
fn port_span() -> u64 {
    (HOST_PORT_HIGH - HOST_PORT_LOW) as u64 + 1
}

impl Namespace {
    pub(super) async fn create(plan: &PlanId, request: &Request) -> Result<Self, NetworkError> {
        use tokio::process::Command;

        // Whatever a *previous* server left behind for this plan. A namespace
        // lives in a process, not on disk, so a server that restarts has no
        // registry -- but the holder and slirp it started are still running,
        // still holding ports, and now unreachable. Reclaimed here, keyed by
        // the plan, in the spirit of `kingdom_browser::sweep_orphans`.
        reclaim_previous(plan);

        // What the holder will be, which is the whole of the difference
        // between the two isolated kinds. A sealed plan takes two more
        // namespaces and a script that builds it a filesystem; everything
        // below this point -- slirp, the api socket, the pidfile, the registry
        // entry -- is identical for both, which is the point of building it
        // this way rather than as two `create`s.
        let sealed = request.isolation.is_sealed();
        let mount_plan = sealed.then(|| {
            super::mount::MountPlan::with_allowed(
                plan.as_str(),
                &request.workspace,
                request.city_root.as_deref(),
                &request.allowed,
            )
        });

        let mut unshare_args = vec!["--user", "--map-root-user", "--net"];
        if sealed {
            // The filesystem and the process table. `--pid` is what makes `ps`
            // inside show this plan's own handful of processes rather than the
            // King's several hundred, and it is why the holder must be the
            // thing that `exec`s `sleep` -- pid 1 of that namespace.
            unshare_args.push("--mount");
            unshare_args.push("--pid");
        }
        unshare_args.push("--fork");
        unshare_args.push("--");

        // `lo` up: a plan's own server on 127.0.0.1 is the entire point, and a
        // fresh namespace's loopback is DOWN. A sealed plan's script does the
        // same thing at its end, after it has a filesystem to do it from.
        let script = match &mount_plan {
            Some(mount_plan) => super::mount::holder_script(mount_plan),
            None => "ip link set lo up; exec sleep infinity".to_string(),
        };

        // The holder. `--map-root-user` so the agent is root *inside* its own
        // namespace, which is what lets it bring up `lo` -- and, for a sealed
        // plan, mount and pivot_root at all. It is still the King's uid
        // everywhere that matters, because a user namespace maps rather than
        // grants: measured, a write to `/usr/bin` from inside is refused.
        let mut holder = Command::new("unshare")
            .args(&unshare_args)
            .arg("sh")
            .arg("-c")
            .arg(&script)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(false)
            .spawn()
            .map_err(|e| NetworkError::Failed(format!("could not run unshare: {e}")))?;

        let holder_pid = holder
            .id()
            .ok_or_else(|| NetworkError::Failed("unshare exited immediately".to_string()))?;

        // `--fork` means the namespace belongs to a *child* of the process just
        // spawned, so wait for that child to exist before naming it.
        let inner = match wait_for_child(holder_pid).await {
            Some(pid) => pid,
            None => {
                kill(holder_pid);
                return Err(NetworkError::Unprivileged(
                    "unshare could not create the namespace".to_string(),
                ));
            }
        };

        let api_socket = api_socket_path(plan);
        if let Some(parent) = api_socket.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::remove_file(&api_socket);

        let slirp = Command::new("slirp4netns")
            .args([
                "--configure",
                "--mtu=65520",
                // The namespace must not reach back into the host's own
                // loopback: that is where the King's services live, and
                // reaching them is exactly the collision being prevented.
                "--disable-host-loopback",
                "--api-socket",
            ])
            .arg(&api_socket)
            .arg(inner.to_string())
            .arg("tap0")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(false)
            .spawn()
            .map_err(|e| NetworkError::Failed(format!("could not run slirp4netns: {e}")))?;

        let slirp_pid = slirp
            .id()
            .ok_or_else(|| NetworkError::Failed("slirp4netns exited immediately".to_string()))?;

        // slirp writes the socket once it is ready to be asked things. Waiting
        // for the file is what stops the first forward racing the daemon.
        if !wait_for_socket(&api_socket).await {
            kill(slirp_pid);
            kill(holder_pid);
            let _ = holder.start_kill();
            return Err(NetworkError::Failed(
                "slirp4netns did not come up; its API socket never appeared".to_string(),
            ));
        }

        // Recorded so a future server can find these two again. Written only
        // once both are up: a pidfile naming a process that never started is
        // worse than none.
        remember_pids(&api_socket, inner, slirp_pid);

        // A sealed plan is not ready when its holder *exists* -- it is ready
        // when the holder has finished building its root. Between those two
        // moments the script is still mounting, and `pivot_root` has not
        // happened, so anything entering sees a half-built namespace: the old
        // root, no private `/tmp`, no resolver. Measured as an intermittent
        // failure of the live test (`/proc/1/comm` answering `sh` rather than
        // `sleep`), which is exactly that window seen from outside.
        //
        // The holder `exec`s `sleep` as the very last thing it does, so pid 1
        // of the namespace turning into `sleep` is the script's own report
        // that it finished. Waited for here rather than in each of the ~100
        // callers of `enter_prefix`.
        if sealed && !wait_for_sealed_root(inner).await {
            kill(slirp_pid);
            kill(holder_pid);
            let _ = holder.start_kill();
            return Err(NetworkError::Failed(
                "the sealed plan's filesystem was never finished".to_string(),
            ));
        }

        Ok(Self {
            holder: inner,
            slirp: slirp_pid,
            api_socket,
            forwards: HashMap::new(),
            cdp_port: None,
            wells: HashMap::new(),
            // Recorded only for a sealed plan, and it is what every later
            // `enter_prefix` reads to know this namespace has a filesystem of
            // its own -- and where to stand in it.
            scratch: mount_plan.as_ref().map(|plan| plan.root.clone()),
            workdir: mount_plan.map(|plan| plan.workdir),
        })
    }

    pub(super) fn tear_down(&self) {
        // Relays first: each is a child of nothing but its own bind, and
        // killing it before the holder avoids a moment where the relay is
        // still accepting into a namespace whose holder is already gone.
        for forward in self.forwards.values() {
            if let Some(pid) = forward.relay {
                kill(pid);
            }
        }
        // The well relays are relays too, and leak in exactly the same way if
        // they are missed -- one process per shared service per plan, holding
        // an open socket to a database, for the life of the server.
        for well in self.wells.values() {
            kill(well.pid);
        }
        // slirp next: it holds a descriptor on the namespace, and killing the
        // holder while it watches produces a noisy log for no benefit.
        kill(self.slirp);
        kill(self.holder);
        let _ = std::fs::remove_file(&self.api_socket);
        let _ = std::fs::remove_file(pid_path(&self.api_socket));

        // The scratch root a sealed plan was assembled in. Empty by now: every
        // mount in it belonged to the holder's own mount namespace and went
        // with the holder, and `pivot_root` left the directories themselves
        // behind. Found by looking after a live test rather than by reasoning
        // -- four skeleton roots were sitting under `$XDG_RUNTIME_DIR`, one per
        // plan ever sealed, for the life of the machine.
        //
        // `remove_dir_all` is safe here precisely because it is empty: if a
        // mount somehow survived, the directory would be non-empty and busy,
        // and the removal fails rather than reaching through a live bind into
        // the King's own files. Best effort, like everything else here.
        if let Some(root) = &self.scratch {
            let _ = std::fs::remove_dir_all(root);
        }
    }
}

/// Where the pids of a plan's two processes are recorded.
fn pid_path(api_socket: &std::path::Path) -> PathBuf {
    api_socket.with_extension("pid")
}

fn remember_pids(api_socket: &std::path::Path, holder: u32, slirp: u32) {
    let _ = std::fs::write(pid_path(api_socket), format!("{holder}\n{slirp}\n"));
}

/// Kills the holder, slirp and any relays a previous server left for this
/// plan.
///
/// Best effort throughout, and **paranoid about pid reuse**: a pid is a
/// recycled number, so a stale file naming 4242 must not kill whatever happens
/// to be 4242 today. Each process is therefore identified before it is
/// signalled, and by something specific to *this* plan:
///
/// - slirp by its command line, which carries this plan's api-socket path.
/// - the holder by its network namespace differing from our own. It cannot be
///   matched on its command line: `unshare --fork` runs it through a shell that
///   `exec`s `sleep`, which replaces the argv, so the word "unshare" is on the
///   *parent* and never on the process actually holding the namespace.
/// - a relay, found by scanning for our own binary's `--relay` argv whose net
///   namespace matches the holder's. Like the holder, a relay left running
///   keeps its namespace alive on its own -- the same trap this whole design
///   exists to close for tmux, reproduced here if it were left unhandled.
fn reclaim_previous(plan: &PlanId) {
    let api_socket = api_socket_path(plan);
    let Ok(recorded) = std::fs::read_to_string(pid_path(&api_socket)) else {
        return;
    };

    let mut pids = recorded
        .lines()
        .filter_map(|line| line.trim().parse::<u32>().ok());
    let holder = pids.next();
    let slirp = pids.next();

    // slirp first, as in `tear_down`: it watches the namespace, and killing
    // what it watches out from under it logs noise for no benefit.
    if let Some(pid) = slirp {
        let ours = api_socket.to_string_lossy();
        if let Ok(cmdline) = std::fs::read(format!("/proc/{pid}/cmdline")) {
            if String::from_utf8_lossy(&cmdline).contains(ours.as_ref()) {
                kill(pid);
            }
        }
    }

    // Relays, before the holder: found by their own `--relay` argv and a net
    // namespace matching the holder's -- the same identification the holder
    // itself gets below, applied to every pid rather than one recorded one,
    // because a relay's pid was never written to the pidfile.
    if let Some(holder_pid) = holder {
        if let Ok(theirs) = std::fs::read_link(format!("/proc/{holder_pid}/ns/net")) {
            if let Ok(entries) = std::fs::read_dir("/proc") {
                for entry in entries.flatten() {
                    let Some(pid) = entry
                        .file_name()
                        .to_str()
                        .and_then(|n| n.parse::<u32>().ok())
                    else {
                        continue;
                    };
                    if pid == holder_pid {
                        continue;
                    }
                    let Ok(cmdline) = std::fs::read(format!("/proc/{pid}/cmdline")) else {
                        continue;
                    };
                    if !String::from_utf8_lossy(&cmdline).contains("--relay") {
                        continue;
                    }
                    if std::fs::read_link(format!("/proc/{pid}/ns/net")).ok()
                        == Some(theirs.clone())
                    {
                        kill(pid);
                    }
                }
            }
        }
    }

    if let Some(pid) = holder {
        let theirs = std::fs::read_link(format!("/proc/{pid}/ns/net")).ok();
        let ours = std::fs::read_link("/proc/self/ns/net").ok();
        // A process on our own network is not a holder of anything, whatever
        // the file says. This is the check that makes a stale pid harmless.
        if theirs.is_some() && theirs != ours {
            kill(pid);
        }
    }

    let _ = std::fs::remove_file(&api_socket);
    let _ = std::fs::remove_file(pid_path(&api_socket));
}

/// Where a plan's slirp control socket lives.
///
/// Under the runtime directory rather than the plan's worktree: it is a socket
/// belonging to a running process, not part of the work, and a stray socket
/// file in a worktree would show up in `git status` and in the review drawer.
fn api_socket_path(plan: &PlanId) -> PathBuf {
    let base = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    base.join("kingdom-netns")
        .join(format!("{}.sock", sanitise(plan.as_str())))
}

async fn wait_for_socket(path: &std::path::Path) -> bool {
    for _ in 0..100 {
        if path.exists() {
            return true;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    false
}

/// Waits for a sealed plan's holder to finish building its root.
///
/// The holder script ends in `exec sleep infinity`, which replaces the shell
/// **in place** -- so the moment `/proc/<holder>/comm` reads `sleep`, every
/// mount is made and the `pivot_root` is done. Before that it reads `sh`, and
/// anything entering lands in a namespace that is still being assembled.
///
/// Read from the host's `/proc`, which is the only side that can see the
/// holder by its real pid; inside its own PID namespace it is pid 1.
async fn wait_for_sealed_root(holder: u32) -> bool {
    for _ in 0..250 {
        match std::fs::read_to_string(format!("/proc/{holder}/comm")) {
            Ok(comm) if comm.trim() == "sleep" => return true,
            // Gone entirely: the script failed -- a refused resolver, most
            // likely. Nothing to wait for, and the caller reports it.
            Err(_) => return false,
            Ok(_) => {}
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    false
}

/// Asks slirp4netns to open a door from the host into the namespace.
///
/// The wire is one JSON object in, one JSON object out, over a UNIX socket --
/// exercised by hand against slirp 1.3.4 before this was written.
async fn add_hostfwd(
    api_socket: &std::path::Path,
    host_port: u16,
    guest_port: u16,
) -> Result<i64, String> {
    let request = serde_json::json!({
        "execute": "add_hostfwd",
        "arguments": {
            "proto": "tcp",
            "host_addr": "127.0.0.1",
            "host_port": host_port,
            "guest_addr": GUEST_ADDR,
            "guest_port": guest_port,
        }
    });
    let reply = ask(api_socket, &request).await?;
    reply
        .get("return")
        .and_then(|r| r.get("id"))
        .and_then(serde_json::Value::as_i64)
        .ok_or_else(|| format!("slirp refused the forward: {reply}"))
}

async fn remove_hostfwd(api_socket: &std::path::Path, id: i64) -> Result<(), String> {
    let request = serde_json::json!({
        "execute": "remove_hostfwd",
        "arguments": { "id": id }
    });
    ask(api_socket, &request).await.map(|_| ())
}

async fn ask(
    api_socket: &std::path::Path,
    request: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let mut stream = tokio::net::UnixStream::connect(api_socket)
        .await
        .map_err(|e| format!("slirp's control socket would not open: {e}"))?;
    stream
        .write_all(request.to_string().as_bytes())
        .await
        .map_err(|e| format!("slirp would not take the request: {e}"))?;
    // slirp reads until the write half closes, then answers.
    stream
        .shutdown()
        .await
        .map_err(|e| format!("slirp's socket would not flush: {e}"))?;

    let mut reply = Vec::new();
    stream
        .read_to_end(&mut reply)
        .await
        .map_err(|e| format!("slirp gave no answer: {e}"))?;

    let reply: serde_json::Value =
        serde_json::from_slice(&reply).map_err(|e| format!("slirp's answer was not JSON: {e}"))?;
    if let Some(error) = reply.get("error") {
        return Err(error.to_string());
    }
    Ok(reply)
}

/// Runs this process as a relay: one TCP splice from `bind` to `target`, and
/// nothing else.
///
/// This is the whole reason `main.rs` has a `--relay` mode instead of shipping
/// a second binary or reaching for `socat`: entered via `nsenter` into a plan's
/// namespace, this binds `tap0` at the forwarded port and pipes every
/// connection straight through to the loopback address the plan's own server
/// actually bound. See [`spawn_relay`] for why the hop exists at all -- in
/// short, `add_hostfwd` can only ever land on `tap0`, and almost nothing binds
/// that address.
///
/// Never returns under ordinary operation: it serves connections until killed,
/// which is what [`Namespace::tear_down`], `reconcile`'s removal arm and
/// [`reclaim_previous`] all do to end its life.
pub async fn run_relay(bind: &str, target: &str) {
    let listener = match tokio::net::TcpListener::bind(bind).await {
        Ok(listener) => listener,
        Err(e) => {
            eprintln!("relay could not bind {bind}: {e}");
            std::process::exit(1);
        }
    };

    let target = target.to_string();
    loop {
        let Ok((mut inbound, _)) = listener.accept().await else {
            continue;
        };
        let target = target.clone();
        tokio::spawn(async move {
            let Ok(mut outbound) = tokio::net::TcpStream::connect(&target).await else {
                return;
            };
            let _ = tokio::io::copy_bidirectional(&mut inbound, &mut outbound).await;
        });
    }
}

/// Puts a city's shared services on this plan's own loopback, so an agent can
/// reach its database at the address it would reach it at anywhere else.
///
/// # Why this is the right place for a database
///
/// [`crate::services`] hands a plan its well as a container address --
/// `172.31.4.10:27017` -- because a container published to the King's loopback
/// is provably unreachable through `--disable-host-loopback`. That is true and
/// stays true. But it means every project with a well must teach every agent a
/// rule that contradicts what every agent already knows, and the failure when
/// it is forgotten reads as a broken database rather than a wrong address.
///
/// A plan's loopback is nobody else's. A relay standing on it makes
/// `mongodb://localhost:27017` *true here* -- while the same address on the
/// King's machine is still his own, and in the plan next door is that plan's.
/// Isolation is what makes the ordinary address safe to hand out, rather than
/// the reason it cannot be.
///
/// # Isolated plans only, and not by accident
///
/// A plan with no namespace gets nothing from this: there is no loopback of its
/// own to bind, so the relay would take the King's real `127.0.0.1:27017` --
/// the precise port collision this product exists to prevent, committed by the
/// product. Such a plan keeps the container address, which works and always
/// has.
///
/// # Idempotent, because it runs at the top of every turn
///
/// A port already relayed is left alone. Re-spawning would be a bind conflict
/// against our own relay, and the second process would exit having achieved
/// nothing while looking like a failure.
///
/// Returns the container addresses that are now answering on the loopback,
/// which is what [`crate::services::address_for`] needs in order to promise
/// `localhost` only where it is true.
pub async fn open_wells(plan: &PlanId, services: &[(String, u16)]) -> Vec<String> {
    let (holder, mut standing) = {
        let registry = lock();
        let Some(namespace) = registry.get(plan) else {
            // No namespace: a shared-network plan, deliberately left with the
            // container address. See the note above.
            return Vec::new();
        };
        (
            namespace.holder,
            namespace
                .wells
                .iter()
                .map(|(port, well)| (*port, well.target.clone()))
                .collect::<Vec<_>>(),
        )
    };

    for (host, port) in services {
        let target = format!("{host}:{port}");
        // Already relayed -- either this very container, in which case there is
        // nothing to do, or a *different* one holding the port. Both are the
        // same decision here: do not spawn a second relay onto a socket that is
        // taken. The second service keeps the container address, which is the
        // honest answer rather than a `localhost` pointing into the first
        // service's data.
        if standing.iter().any(|(taken, _)| taken == port) {
            continue;
        }
        let bind = format!("127.0.0.1:{port}");
        let Some(pid) = spawn_relay_between(holder, &bind, &target).await else {
            // The bind was refused, so this port is *not* answering on the
            // loopback and must not be recorded as if it were. The caller
            // falls back to the container address, which is what every plan
            // had before this existed -- a worse address, but a working one.
            continue;
        };
        let mut registry = lock();
        let Some(namespace) = registry.get_mut(plan) else {
            // The plan was closed while we were spawning. Nothing will ever
            // tear this down, so do it here rather than leak it.
            kill(pid);
            continue;
        };
        namespace.wells.insert(
            *port,
            Well {
                pid,
                target: target.clone(),
            },
        );
        standing.push((*port, target));
    }

    let mut open: Vec<String> = standing.into_iter().map(|(_, target)| target).collect();
    open.sort();
    open.dedup();
    open
}

/// Which shared services are answering on this plan's own loopback, by the
/// container address each one reaches.
///
/// Asked by [`crate::services::address_for`] on every command a plan runs, so
/// it is a map lookup rather than anything that touches the network: the
/// question is "did the relay come up", and the registry is where that was
/// recorded.
///
/// Answers with **targets** rather than ports because a port is not unique --
/// see [`Well`]. Two services on `:6379` are two different databases, and only
/// one of them is on the loopback.
pub fn wells_of(plan: &PlanId) -> Vec<String> {
    let registry = lock();
    let Some(namespace) = registry.get(plan) else {
        return Vec::new();
    };
    let mut targets: Vec<String> = namespace
        .wells
        .values()
        .map(|well| well.target.clone())
        .collect();
    targets.sort();
    targets
}

/// Records a plan as having wells on its loopback, without a kernel.
///
/// Test-only, and deliberately in this module rather than reached for through a
/// public field: what [`crate::services::address_for`] and the system prompt
/// need to be told is exactly what [`open_wells`] records, and a test that
/// built its own idea of that state would pass while the real pair drifted.
/// The pids are fictional and nothing ever signals them -- `tear_down` is not
/// reached, because no namespace is really created here.
///
/// Takes the **targets** the relays reach, in `host:port` form, for the reason
/// [`Well`] gives: a port alone cannot distinguish two services that share one.
#[cfg(test)]
pub(crate) fn pretend_wells_are_open(plan: &PlanId, targets: &[&str]) {
    let mut registry = lock();
    let namespace = registry.entry(plan.clone()).or_insert_with(|| Namespace {
        holder: 0,
        slirp: 0,
        api_socket: PathBuf::from("/run/nowhere.sock"),
        forwards: HashMap::new(),
        cdp_port: None,
        wells: HashMap::new(),
        workdir: None,
        scratch: None,
    });
    for (index, target) in targets.iter().enumerate() {
        let port = target
            .rsplit(':')
            .next()
            .and_then(|p| p.parse().ok())
            .expect("a well target is `host:port`");
        namespace.wells.insert(
            port,
            Well {
                pid: 900_000 + index as u32,
                target: (*target).to_string(),
            },
        );
    }
}

/// Undoes [`pretend_wells_are_open`], so one test cannot leak into another
/// through this process-global registry.
#[cfg(test)]
pub(crate) fn forget_namespace(plan: &PlanId) {
    lock().remove(plan);
}

/// Reserves a fixed port for Chrome's CDP inside a plan's namespace, and
/// records it so the King's ports badge can exclude it.
///
/// Called instead of leaving the port at 0 (kernel-chosen) only when the plan
/// has a namespace: chromiumoxide reads the DevTools URL Chrome prints to its
/// own stderr and connects to whatever address it says, which is
/// `127.0.0.1:<port>` -- true inside the namespace and false for the host
/// client that actually issues CDP calls. A *known* port lets the relay (see
/// [`spawn_relay`]) put the same number on `tap0`, so the URL Chrome printed is
/// already correct from the host and chromiumoxide's ordinary `connect()` path
/// needs no rewriting.
///
/// Idempotent for a given plan: a session relaunching keeps its previous port
/// rather than drawing a new one and leaving the old forward stranded.
pub async fn reserve_cdp_port(plan: &PlanId) -> Option<u16> {
    let holder = {
        let registry = lock();
        let namespace = registry.get(plan)?;
        if let Some(port) = namespace.cdp_port {
            return Some(port);
        }
        namespace.holder
    };

    let api_socket = api_socket_path(plan);
    for _ in 0..PORT_ATTEMPTS {
        let port = random_port();
        // Bound directly rather than through the relay's own retry: the port
        // has to be free on *both* the namespace's tap0 test the relay will
        // make and the loopback address Chrome will actually bind, and
        // drawing it here keeps that single choice in one place.
        if add_hostfwd(&api_socket, port, port).await.is_ok() {
            let relay = spawn_relay(holder, port).await;
            let mut registry = lock();
            if let Some(namespace) = registry.get_mut(plan) {
                namespace.cdp_port = Some(port);
                namespace.forwards.insert(
                    port,
                    Forward {
                        host_port: port,
                        id: 0,
                        relay,
                    },
                );
            }
            return Some(port);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::super::namespaces;
    use super::*;

    /// The `/proc/net/tcp` shape, pinned against a real capture.
    ///
    /// This is the exact text the kernel produced for a namespace holding one
    /// listener on 3000 plus an established connection, and it is a fixture
    /// rather than a live read because the suite must run on a machine with no
    /// namespace in it.
    #[test]
    fn only_listening_sockets_are_forwarded() {
        let table = "\
  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode
   0: 00000000:0BB8 00000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 53694 1 0000000000000000 100 0 0 10 0
   1: 0100007F:1F90 0100007F:C350 01 00000000:00000000 00:00000000 00000000  1000        0 53695 1 0000000000000000 100 0 0 10 0
";
        // 0x0BB8 is 3000. The second row is state 01 -- ESTABLISHED -- and a
        // forward to it would answer nothing.
        assert_eq!(parse_listeners(table), vec![3000]);
    }

    /// A header alone yields nothing, rather than panicking on a missing field.
    #[test]
    fn an_empty_table_holds_no_ports() {
        let header = "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode\n";
        assert!(parse_listeners(header).is_empty());
        assert!(parse_listeners("").is_empty());
    }

    /// Ragged input is skipped, not panicked on.
    ///
    /// This parser reads a kernel file that has changed format before, and a
    /// short line must not take the server down with an index-out-of-bounds.
    #[test]
    fn a_malformed_row_is_skipped() {
        let table = "header\n   0: garbage\n   1: \n";
        assert!(parse_listeners(table).is_empty());
    }

    /// Every drawn port is inside the forwarding range.
    ///
    /// The range matters twice: below it are ports projects choose for
    /// themselves, and a forward landing on one would collide with the very
    /// thing this feature exists to prevent.
    #[test]
    fn a_drawn_port_is_always_in_the_forwarding_range() {
        for _ in 0..500 {
            let port = random_port();
            assert!(
                (HOST_PORT_LOW..=HOST_PORT_HIGH).contains(&port),
                "{port} is outside {HOST_PORT_LOW}..={HOST_PORT_HIGH}"
            );
        }
    }

    /// The span is the count of ports, not the distance between the ends.
    ///
    /// An off-by-one here would make the top port unreachable, which is the
    /// kind of thing nobody notices and nobody can debug later.
    #[test]
    fn the_range_counts_both_of_its_ends() {
        assert_eq!(port_span(), 10_000);
    }

    /// A plan id becomes a filename without ever escaping its directory.
    ///
    /// The id reaches this from a URL, so a `../` in one must not put a socket
    /// somewhere else on the disk.
    #[test]
    fn a_plan_id_cannot_escape_the_socket_directory() {
        assert_eq!(sanitise("plan-1"), "plan-1");
        assert_eq!(sanitise("../../etc/passwd"), "------etc-passwd");
        assert_eq!(sanitise("a/b"), "a-b");

        let path = api_socket_path(&PlanId::new("../escape"));
        assert!(
            path.parent().is_some_and(|p| p.ends_with("kingdom-netns")),
            "{} left the runtime directory",
            path.display()
        );
    }

    /// The pidfile sits beside the socket rather than replacing it.
    #[test]
    fn the_pidfile_is_named_from_the_socket() {
        let socket = PathBuf::from("/run/user/1000/kingdom-netns/plan-7.sock");
        assert_eq!(
            pid_path(&socket),
            PathBuf::from("/run/user/1000/kingdom-netns/plan-7.pid")
        );
    }

    /// Chrome's CDP port is forwarded like any other listener -- the relay
    /// needs the forward to reach it -- but never shown on the King's badge.
    /// A badge entry that speaks CDP, not HTTP, would be one the King clicks
    /// and gets nothing useful from.
    #[test]
    fn the_cdp_port_never_reaches_the_badge() {
        let mut registry = HashMap::new();
        let plan = PlanId::new("plan-with-a-browser");
        let mut forwards = HashMap::new();
        forwards.insert(
            3000,
            Forward {
                host_port: 40001,
                id: 1,
                relay: None,
            },
        );
        forwards.insert(
            9222,
            Forward {
                host_port: 40002,
                id: 2,
                relay: Some(9999),
            },
        );
        registry.insert(
            plan.clone(),
            Namespace {
                holder: 1,
                slirp: 2,
                api_socket: PathBuf::from("/run/nowhere.sock"),
                forwards,
                cdp_port: Some(9222),
                wells: HashMap::new(),
                workdir: None,
                scratch: None,
            },
        );
        *namespaces().lock().unwrap() = registry;

        assert_eq!(forwards_of(&plan), vec![(3000, 40001)]);
    }

    /// A well relay listens inside the namespace, and must not be mistaken for
    /// the plan's own server.
    ///
    /// This is the trap in putting a database on the plan's loopback:
    /// `/proc/<holder>/net/tcp` cannot tell a relay from a dev server, so
    /// without this filter `reconcile` forwards 27017 to a host port and the
    /// ports badge invites the King to open his own database in a browser tab.
    #[test]
    fn a_wells_port_is_never_forwarded_as_a_server_of_the_plans_own() {
        let mut wells = HashMap::new();
        wells.insert(
            27017,
            Well {
                pid: 5555,
                target: "172.31.4.10:27017".to_string(),
            },
        );

        // What `/proc` would report inside a plan running a server with a well
        // open: its own :3000, and the relay carrying the database.
        assert_eq!(forwardable(vec![3000, 27017], &wells), vec![3000]);

        // And with no wells open, nothing is dropped -- the filter must not
        // cost a shared-network plan anything.
        assert_eq!(
            forwardable(vec![3000, 27017], &HashMap::new()),
            vec![3000, 27017]
        );
    }

    /// A well relay binds the plan's loopback and reaches out to the container,
    /// never the reverse.
    ///
    /// Worth pinning because both addresses are `host:port` strings of the same
    /// shape, so swapping them is a one-line mistake that compiles, starts a
    /// process, and silently relays nothing: the relay would listen on the
    /// container's address -- which it cannot bind -- or, worse, bind the
    /// container's port and forward the loopback to itself.
    #[test]
    fn a_well_relay_listens_on_the_loopback_and_reaches_the_container() {
        let argv = relay_argv(
            4242,
            "/usr/bin/kingdom",
            "127.0.0.1:27017",
            "172.31.4.10:27017",
        );

        // Entered into the plan's namespace, or it would bind the King's own
        // loopback -- the collision this whole product exists to prevent.
        assert_eq!(argv.first().map(String::as_str), Some("nsenter"));
        assert!(argv.iter().any(|p| p == "--net=/proc/4242/ns/net"));
        assert!(argv.iter().any(|p| p == "--preserve-credentials"));

        // The order is the direction of travel: bind first, target second.
        let relay = argv.iter().position(|p| p == "--relay").expect("--relay");
        assert_eq!(argv[relay + 1], "127.0.0.1:27017", "binds the loopback");
        assert_eq!(argv[relay + 2], "172.31.4.10:27017", "reaches the well");
    }

    /// The forwarding relay travels the other way, and still does.
    ///
    /// The generalisation that let a well relay exist could have changed this
    /// one's addresses without any caller noticing, because a forward with a
    /// backwards relay fails the same way it failed before relays existed: the
    /// host port accepts and then answers nothing.
    #[test]
    fn a_forwarding_relay_still_travels_out_to_tap0() {
        let argv = relay_argv(
            7,
            "/usr/bin/kingdom",
            &format!("{GUEST_ADDR}:3000"),
            "127.0.0.1:3000",
        );
        let relay = argv.iter().position(|p| p == "--relay").expect("--relay");
        assert_eq!(argv[relay + 1], format!("{GUEST_ADDR}:3000"));
        assert_eq!(argv[relay + 2], "127.0.0.1:3000");
    }

    /// A plan with no namespace gets no well relay at all.
    ///
    /// The refusal is the feature. Such a plan's `127.0.0.1` *is* the King's,
    /// so a relay there would take his real port 27017 -- Kingdom committing
    /// the exact collision it exists to surface.
    #[tokio::test]
    async fn a_shared_network_plan_is_given_no_loopback_well() {
        let plan = PlanId::new("plan-on-the-machines-network");
        namespaces().lock().unwrap().remove(&plan);

        let opened = open_wells(&plan, &[("172.31.4.10".to_string(), 27017)]).await;

        assert!(
            opened.is_empty(),
            "binding the King's own loopback is the one outcome worth refusing"
        );
        assert!(wells_of(&plan).is_empty());
    }
}
