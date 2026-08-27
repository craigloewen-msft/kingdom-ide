//! A network of a plan's own: namespaces, and the ports they hold.
//!
//! # The problem
//!
//! Two agents both run `cargo leptos serve`; both want `:3000`; the second one
//! dies. That is the collision AGENTS.md names as the reason this product
//! exists, and until now Kingdom could only *watch* it happen.
//!
//! A plan opened with [`NetworkMode::Isolated`] gets a Linux network namespace
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
//! # What this is not
//!
//! **Not a security boundary.** A process in here still has the whole
//! filesystem and the King's own uid. It cannot take a port from another plan;
//! it can still delete his home directory. This is collision avoidance, and
//! saying so plainly is worth more than a guarantee that does not hold --
//! `tools::Sandbox::root` makes the same admission about paths.

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

/// Ports inside the namespace that are never forwarded.
///
/// Empty today, and named rather than inlined because the *question* recurs:
/// every port a plan opens becomes reachable from the King's loopback, so
/// anything that should stay private belongs here. Chrome's debugging port was
/// the obvious candidate and is deliberately absent -- chromiumoxide lets the
/// kernel choose it, so there is no fixed number to list, and the spyglass
/// reaches Chrome from inside the namespace regardless.
const NEVER_FORWARD: &[u16] = &[];

/// The address slirp4netns gives the namespace on `tap0`.
///
/// Fixed by slirp's own defaults rather than chosen here, and named so the
/// forwarding call and any future diagnostic cannot drift apart.
const GUEST_ADDR: &str = "10.0.2.100";

/// One isolated plan's network.
pub struct Namespace {
    /// The holder process. Killing it collects the namespace.
    holder: u32,
    /// The `slirp4netns` process giving that namespace a way out.
    slirp: u32,
    /// Its JSON control socket, which is how ports get forwarded.
    api_socket: PathBuf,
    /// Guest port -> (host port, slirp's id for the forward).
    forwards: HashMap<u16, Forward>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Forward {
    host_port: u16,
    /// slirp's own handle for this rule, needed to remove it again.
    id: i64,
}

/// Every namespace this server is holding.
///
/// Process-global for the reason `tools::bash`'s `JOBS` is: a namespace must
/// outlive the tool call that created it, because the whole point is that the
/// *next* call lands in the same one.
static NAMESPACES: OnceLock<Mutex<HashMap<PlanId, Namespace>>> = OnceLock::new();

fn namespaces() -> &'static Mutex<HashMap<PlanId, Namespace>> {
    NAMESPACES.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Why an isolated plan could not be started.
///
/// Written for the King, because he is the one who has to act on it: every
/// variant either names a package to install or a kernel setting to change.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum NetworkError {
    #[error(
        "slirp4netns is not installed. A plan with its own network needs it to \
         reach anything outside -- without it there would be no DNS, no \
         crates.io and no git. Install it with `sudo pacman -S slirp4netns` \
         (Arch), `sudo apt install slirp4netns` (Debian/Ubuntu) or `sudo dnf \
         install slirp4netns` (Fedora)."
    )]
    SlirpMissing,

    #[error(
        "this kernel will not let an ordinary user create a network namespace \
         ({0}). On Debian and its derivatives this is usually \
         `kernel.unprivileged_userns_clone=0`; enable it with `sudo sysctl -w \
         kernel.unprivileged_userns_clone=1`."
    )]
    Unprivileged(String),

    #[error("the network namespace could not be created: {0}")]
    Failed(String),
}

/// Whether this machine can give a plan a network of its own, and why not.
///
/// Checked before a plan is opened rather than when its first command runs, so
/// the King is told at the moment he chooses -- a plan that accepted the
/// setting and then quietly ran shared would be exactly the invisible isolation
/// this feature exists to end.
pub fn availability() -> Result<(), NetworkError> {
    if !cfg!(target_os = "linux") {
        return Err(NetworkError::Unprivileged(
            "network namespaces are a Linux feature".to_string(),
        ));
    }
    if which("slirp4netns").is_none() {
        return Err(NetworkError::SlirpMissing);
    }
    if which("unshare").is_none() || which("nsenter").is_none() {
        return Err(NetworkError::Failed(
            "`unshare` and `nsenter` are needed; install util-linux".to_string(),
        ));
    }
    Ok(())
}

/// Finds an executable on `PATH`.
///
/// Hand-rolled rather than shelling out to `which`, which is itself a program
/// that may not be installed -- and spawning a process to ask whether we can
/// spawn a process is a strange thing to do.
fn which(program: &str) -> Option<PathBuf> {
    std::env::var_os("PATH").and_then(|path| {
        std::env::split_paths(&path)
            .map(|dir| dir.join(program))
            .find(|candidate| candidate.is_file())
    })
}

/// The argv prefix that puts a command inside a plan's namespace.
///
/// **Empty for a shared-network plan**, which is what makes every call site a
/// no-op by default: `bash`, `tmux` and the browser all prepend this
/// unconditionally and get exactly their old behaviour when the plan has no
/// namespace. That is deliberate -- a call site that had to *remember* to check
/// is a call site that will forget, and the one that forgets is the one that
/// starts a server on the King's own `:3000`.
pub fn enter_prefix(plan: &PlanId) -> Vec<String> {
    let registry = match namespaces().lock() {
        Ok(r) => r,
        Err(poisoned) => poisoned.into_inner(),
    };
    registry
        .get(plan)
        .map(|ns| ns.enter_prefix())
        .unwrap_or_default()
}

impl Namespace {
    fn enter_prefix(&self) -> Vec<String> {
        vec![
            "nsenter".to_string(),
            // Load-bearing. See the module docs: without it, re-entering our
            // own user namespace fails on setgroups.
            "--preserve-credentials".to_string(),
            format!("--user=/proc/{}/ns/user", self.holder),
            format!("--net=/proc/{}/ns/net", self.holder),
            "--".to_string(),
        ]
    }

    /// Ports something inside the namespace is listening on.
    ///
    /// Read from the host, with no subprocess: `/proc/<pid>/net` is per network
    /// namespace, so this file *is* the namespace's table.
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
        ports.retain(|p| !NEVER_FORWARD.contains(p));
        ports
    }
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

/// Gives a plan a network of its own.
///
/// Idempotent: a plan that already has one keeps it, because the processes it
/// has already started are in that namespace and there is no way to move them.
///
/// Called when a plan's first tool runs rather than when it is opened, so a
/// plan that never executes anything never pays for three processes.
pub async fn ensure(plan: &PlanId) -> Result<(), NetworkError> {
    if lock().contains_key(plan) {
        return Ok(());
    }

    availability()?;

    let namespace = Namespace::create(plan).await?;

    let mut registry = lock();
    // Another turn may have raced us here while we were spawning. Keep the one
    // already registered and tear this one down, rather than overwriting it and
    // leaking a holder nobody can reach.
    if registry.contains_key(plan) {
        namespace.tear_down();
        return Ok(());
    }
    registry.insert(plan.clone(), namespace);
    Ok(())
}

fn lock() -> std::sync::MutexGuard<'static, HashMap<PlanId, Namespace>> {
    match namespaces().lock() {
        Ok(r) => r,
        Err(poisoned) => poisoned.into_inner(),
    }
}

/// Tears down a plan's network and everything still running in it.
///
/// Called when a plan is merged or archived. Without it, a holder and a
/// slirp4netns survive for the life of the server for every isolated plan ever
/// opened -- and, worse, so does whatever the agent left listening.
pub fn shutdown(plan: &PlanId) {
    let namespace = lock().remove(plan);
    if let Some(namespace) = namespace {
        namespace.tear_down();
    }
}

/// What a plan currently has forwarded, for the ports badge.
pub fn forwards_of(plan: &PlanId) -> Vec<(u16, u16)> {
    let registry = lock();
    let Some(namespace) = registry.get(plan) else {
        return Vec::new();
    };
    let mut out: Vec<(u16, u16)> = namespace
        .forwards
        .iter()
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
    let (listening, known, api_socket) = {
        let registry = lock();
        let Some(namespace) = registry.get(plan) else {
            return false;
        };
        (
            namespace.listeners(),
            namespace.forwards.clone(),
            namespace.api_socket.clone(),
        )
    };

    let mut changed = false;

    // Gone: the agent stopped its server, so the host port must stop answering.
    for (guest, forward) in &known {
        if !listening.contains(guest) && remove_hostfwd(&api_socket, forward.id).await.is_ok() {
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
        if let Some(forward) = add_forward(&api_socket, guest).await {
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

/// Picks a free host port and asks slirp to forward it.
///
/// The retry is the point. A host port is chosen at random rather than
/// allocated, so two plans can pick the same number; rather than coordinate,
/// this asks and moves on when refused, which is both simpler and correct under
/// a race with a process that is not ours at all.
async fn add_forward(api_socket: &std::path::Path, guest: u16) -> Option<Forward> {
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
            return Some(Forward { host_port, id });
        }
    }
    None
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
    async fn create(plan: &PlanId) -> Result<Self, NetworkError> {
        use tokio::process::Command;

        // Whatever a *previous* server left behind for this plan. A namespace
        // lives in a process, not on disk, so a server that restarts has no
        // registry -- but the holder and slirp it started are still running,
        // still holding ports, and now unreachable. Reclaimed here, keyed by
        // the plan, in the spirit of `kingdom_browser::sweep_orphans`.
        reclaim_previous(plan);

        // The holder. `--map-root-user` so the agent is root *inside* its own
        // namespace, which is what lets it bring up `lo`; it is still the
        // King's uid everywhere that matters, because a user namespace maps
        // rather than grants.
        let mut holder = Command::new("unshare")
            .args([
                "--user",
                "--map-root-user",
                "--net",
                "--fork",
                "--",
                "sh",
                "-c",
                // `lo` up: a plan's own server on 127.0.0.1 is the entire point,
                // and a fresh namespace's loopback is DOWN.
                "ip link set lo up; exec sleep infinity",
            ])
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

        Ok(Self {
            holder: inner,
            slirp: slirp_pid,
            api_socket,
            forwards: HashMap::new(),
        })
    }

    fn tear_down(&self) {
        // slirp first: it holds a descriptor on the namespace, and killing the
        // holder while it watches produces a noisy log for no benefit.
        kill(self.slirp);
        kill(self.holder);
        let _ = std::fs::remove_file(&self.api_socket);
        let _ = std::fs::remove_file(pid_path(&self.api_socket));
    }
}

/// Where the pids of a plan's two processes are recorded.
fn pid_path(api_socket: &std::path::Path) -> PathBuf {
    api_socket.with_extension("pid")
}

fn remember_pids(api_socket: &std::path::Path, holder: u32, slirp: u32) {
    let _ = std::fs::write(pid_path(api_socket), format!("{holder}\n{slirp}\n"));
}

/// Kills the holder and slirp a previous server left for this plan.
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

/// Keeps a plan id to characters that are safe in a filename.
fn sanitise(id: &str) -> String {
    id.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect()
}

/// The pid of the namespace-owning child `unshare --fork` made.
async fn wait_for_child(parent: u32) -> Option<u32> {
    for _ in 0..50 {
        let path = format!("/proc/{parent}/task/{parent}/children");
        if let Ok(children) = std::fs::read_to_string(&path) {
            if let Some(pid) = children
                .split_whitespace()
                .next()
                .and_then(|p| p.parse().ok())
            {
                return Some(pid);
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    None
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

fn kill(pid: u32) {
    // SAFETY: a pid this module spawned. A stale pid at worst signals nothing;
    // it cannot reach another user's process, since Kingdom is unprivileged.
    unsafe {
        libc::kill(pid as i32, libc::SIGKILL);
    }
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

#[cfg(test)]
mod tests {
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

    /// A plan with no namespace gets an empty prefix.
    ///
    /// The invariant the whole design rests on: every call site prepends this
    /// unconditionally, so "empty" is what makes a shared-network plan behave
    /// exactly as it did before this module existed.
    #[test]
    fn a_shared_network_plan_is_never_wrapped() {
        let nobody = PlanId::new("plan-that-was-never-isolated");
        assert!(enter_prefix(&nobody).is_empty());
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

    /// The `--preserve-credentials` flag is not optional.
    ///
    /// Pinned as a *test* rather than left to a comment because its absence
    /// does not fail to compile and does not fail loudly: `nsenter` refuses
    /// with `setgroups failed: Operation not permitted`, which surfaces as a
    /// tool that mysteriously will not run. Measured on a real kernel before
    /// this module was written.
    #[test]
    fn entering_a_namespace_preserves_credentials() {
        let namespace = Namespace {
            holder: 4242,
            slirp: 4243,
            api_socket: PathBuf::from("/run/nowhere.sock"),
            forwards: HashMap::new(),
        };
        let prefix = namespace.enter_prefix();

        assert_eq!(prefix.first().map(String::as_str), Some("nsenter"));
        assert!(
            prefix.iter().any(|part| part == "--preserve-credentials"),
            "without this flag nsenter cannot re-enter our own user namespace"
        );
        // Both namespaces, named from the holder: the user namespace is what
        // makes the net namespace enterable unprivileged.
        assert!(prefix.iter().any(|p| p == "--user=/proc/4242/ns/user"));
        assert!(prefix.iter().any(|p| p == "--net=/proc/4242/ns/net"));
        // The terminator, so a command starting with a dash is not read as a
        // flag to nsenter itself.
        assert_eq!(prefix.last().map(String::as_str), Some("--"));
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
}
