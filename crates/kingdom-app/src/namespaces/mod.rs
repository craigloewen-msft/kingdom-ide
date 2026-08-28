//! A plan's namespaces: the holder that owns them, and the way in.
//!
//! # The problem
//!
//! Two agents both run `cargo leptos serve`; both want `:3000`; the second one
//! dies. Two agents both `rm -rf` a directory the other is reading. That is the
//! collision AGENTS.md names as the reason this product exists, and until
//! recently Kingdom could only *watch* it happen.
//!
//! A plan opened with an isolated [`kingdom_core::Isolation`] gets namespaces
//! of its own. This module owns what is true of all of them -- the holder
//! process, the registry, and the way back in -- and delegates the rest to one
//! submodule per kind:
//!
//! - [`net`] gives a plan its own loopback and its own ports, via
//!   `slirp4netns`, forwards and relays.
//! - [`mount`] gives a plan its own filesystem, via a mount namespace and a
//!   `pivot_root` over a set of allowed folders.
//!
//! # One holder, several namespaces
//!
//! There is exactly **one** holder process per plan, holding whichever
//! namespaces that plan asked for. Two holders -- one for the network, one for
//! the mounts -- was tried and rejected on a measurement: two separately
//! created user namespaces are *siblings*, and an unprivileged process may not
//! enter a sibling user namespace. It works only when the server happens to be
//! running as root, and where it fails it fails in the worst possible way, by
//! attaching the network and silently not the mounts -- a plan that believes it
//! is sealed and is not. Kingdom must not be quietly more capable as root.
//!
//! # How it is put together
//!
//! ```text
//!   holder     unshare --user --net [--mount --pid] --fork sleep infinity
//!              Owns the namespaces. Exists only so they outlive the tool call
//!              that made them -- a namespace with no process in it is
//!              collected by the kernel.
//!
//!   slirp4netns  Gives the network namespace a way OUT. See [`net`].
//!
//!   nsenter    How everything else gets IN. Prefixed to bash, tmux and
//!              Chrome so they start inside the namespaces.
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
//! **The namespace lives in a process, not a name.** A daemon found again by a
//! stable name -- tmux's socket, most notably -- says nothing about which
//! *generation* of namespace it was last talking to. See
//! `tools::tmux::ensure_server` for the mismatch this causes and the fix.
//!
//! # What this is not
//!
//! **Not a security boundary, and how far short it falls depends on the mode.**
//! A plan with only a network of its own still has the whole filesystem and the
//! King's own uid: it cannot take a port from another plan, but it can still
//! delete his home directory. A sealed plan cannot, because it cannot see it --
//! but a mount namespace is still not a jail, and saying so plainly is worth
//! more than a guarantee that does not hold. `tools::Sandbox::root` makes the
//! same admission about paths.

pub mod mount;
pub mod net;

use kingdom_core::PlanId;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

use net::{Forward, Well};

/// One isolated plan's network.
pub struct Namespace {
    /// The holder process. Killing it collects the namespace.
    holder: u32,
    /// The `slirp4netns` process giving that namespace a way out.
    slirp: u32,
    /// Its JSON control socket, which is how ports get forwarded.
    api_socket: PathBuf,
    /// Guest port -> (host port, slirp's id for the forward, and the relay
    /// standing between them, if one was needed).
    forwards: HashMap<u16, Forward>,
    /// Chrome's own CDP port inside this namespace, if a browser has been
    /// launched for this plan. Excluded from [`forwards_of`] -- see its own
    /// docs for why the King's badge must never show it.
    cdp_port: Option<u16>,
    /// Service port -> the relay putting a shared service on this namespace's
    /// own loopback. See [`open_wells`].
    ///
    /// Kept apart from `forwards` because it is the opposite direction of
    /// travel and has to be treated as such everywhere: a forward carries the
    /// plan's own server *out* to the King, where a well carries a container
    /// *in* to the plan. [`Namespace::listeners`] is where that distinction
    /// earns its keep.
    wells: HashMap<u16, Well>,
    /// Where commands start inside this plan's own filesystem -- `Some` only
    /// for a sealed plan, and the flag that tells the two isolated kinds apart
    /// throughout this module.
    ///
    /// It holds the *working directory* rather than a bare `is_sealed` bool
    /// because that is what is actually needed at the moment it matters: the
    /// path has to be handed to `nsenter --wdns`, and a namespace that recorded
    /// only "sealed" would have to go and ask somebody else where to stand.
    /// See [`Namespace::enter_prefix`].
    workdir: Option<PathBuf>,
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

/// This plan's own network namespace, for comparing against something that
/// claims to be in it -- the check `tools::tmux::daemon_belongs_here` needs to
/// tell a live daemon from a stale one.
///
/// `None` for a shared-network plan, the same as `enter_prefix`'s empty
/// vector, and for the same reason: there is nothing of the plan's own to
/// compare against.
pub fn holder_ns(plan: &PlanId) -> Option<std::path::PathBuf> {
    let registry = lock();
    let namespace = registry.get(plan)?;
    std::fs::read_link(format!("/proc/{}/ns/net", namespace.holder)).ok()
}

impl Namespace {
    fn enter_prefix(&self) -> Vec<String> {
        let mut argv = vec![
            "nsenter".to_string(),
            // Load-bearing. See the module docs: without it, re-entering our
            // own user namespace fails on setgroups.
            "--preserve-credentials".to_string(),
            format!("--user=/proc/{}/ns/user", self.holder),
            format!("--net=/proc/{}/ns/net", self.holder),
        ];

        // A sealed plan's filesystem and process table as well.
        //
        // `--wdns` is the load-bearing one here, and it took a measurement to
        // find. Every caller -- `bash`, `tmux`, the King's terminal -- sets the
        // working directory on the **host** side, with `Command::current_dir`
        // or tmux's own `-c`. That path is resolved before `nsenter` enters the
        // mount namespace, so inside a sealed plan it is silently ignored and
        // the command runs in `/`. Measured: with the workspace bound at its
        // own path inside, `pwd` still answered `/`. `--wd` does not help --
        // it resolves on the host too and fails outright. `--wdns` resolves
        // *in the namespace*, which is the only one of the three that is
        // correct here.
        //
        // An agent quietly running every build in `/` is the failure this
        // prevents, and nothing about it would look like an error.
        if let Some(workdir) = &self.workdir {
            argv.push(format!("--mount=/proc/{}/ns/mnt", self.holder));
            argv.push(format!("--pid=/proc/{}/ns/pid", self.holder));
            argv.push(format!("--wdns={}", workdir.display()));
        }

        argv.push("--".to_string());
        argv
    }
}

/// What a plan is asking for, which is everything its namespaces are built
/// from.
///
/// A small struct rather than three arguments because two of the three matter
/// only for one mode, and a call site passing `(&plan, &workspace, None)` for
/// an ordinary isolated plan reads as though it forgot something.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    /// How far this plan is walled off.
    pub isolation: kingdom_core::Isolation,
    /// Where the plan works, which is both what gets mounted and where
    /// commands start.
    pub workspace: PathBuf,
    /// The project the workspace belongs to, when there is one.
    ///
    /// Not the same as the workspace, and the difference is load-bearing: an
    /// isolated plan's workspace is a worktree under `<city>/.kingdom/`, whose
    /// `.git` lives back in the city. See [`mount::MountPlan::built_in`].
    pub city_root: Option<PathBuf>,
}

impl Request {
    /// What an ordinary isolated plan asks for: a network, and nothing else.
    pub fn network_only() -> Self {
        Self {
            isolation: kingdom_core::Isolation::Isolated,
            workspace: PathBuf::from("/"),
            city_root: None,
        }
    }
}

/// Gives a plan the namespaces it asked for.
///
/// Idempotent: a plan that already has them keeps them, because the processes
/// it has already started are in there and there is no way to move them.
///
/// Called when a plan's first tool runs rather than when it is opened, so a
/// plan that never executes anything never pays for the processes.
///
/// # Why it takes the whole shape of the request
///
/// A sealed plan needs to know *what to mount* before its holder exists, and
/// the answer -- the workspace, the city, the folders the King allows -- is not
/// derivable from a [`PlanId`]. Passing it in keeps this module free of any
/// lookup into the plan registry, which is what lets the whole of it be reasoned
/// about from its arguments.
pub async fn ensure(plan: &PlanId, request: &Request) -> Result<(), NetworkError> {
    if lock().contains_key(plan) {
        return Ok(());
    }

    availability()?;

    let namespace = Namespace::create(plan, request).await?;

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

fn kill(pid: u32) {
    // SAFETY: a pid this module spawned. A stale pid at worst signals nothing;
    // it cannot reach another user's process, since Kingdom is unprivileged.
    unsafe {
        libc::kill(pid as i32, libc::SIGKILL);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

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
            cdp_port: None,
            wells: HashMap::new(),
            workdir: None,
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
}
