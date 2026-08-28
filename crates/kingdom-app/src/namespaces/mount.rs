//! A filesystem of a plan's own: the mount namespace, and what is let into it.
//!
//! # The problem
//!
//! [`super::net`] stops two agents colliding on a port. It does nothing at all
//! about the other half of the collision this product exists to surface: an
//! agent that rewrites a file another is halfway through reading, or that
//! `rm -rf`s something outside the work it was given. A plan with a network of
//! its own still has the King's whole disk and his own uid -- `netns.rs` said
//! so plainly, and this module is the answer to it.
//!
//! A [`kingdom_core::Isolation::Sealed`] plan gets a mount namespace and a PID
//! namespace as well as a network one. Inside, the filesystem is **only** what
//! was deliberately let in: the plan's workspace, the toolchain it needs, and a
//! read-only system. The King's home directory is not there to be deleted.
//!
//! # How the root is built
//!
//! One shell script, run by the holder before it becomes `sleep infinity`, in
//! the namespace it has just made. It assembles a new root under a scratch
//! directory, then `pivot_root`s into it and drops the old one:
//!
//! ```text
//!   mount --rbind /usr    -> ro   the system, and every tool in it
//!   mount --rbind /etc    -> ro   so name resolution and users work
//!   mount --rbind /dev            the King's real devices; a plan needs
//!                                 /dev/null and /dev/urandom to do anything
//!   mount -t tmpfs /tmp           private and empty, so scratch files here
//!                                 cannot collide with another plan's
//!   mount --bind <workspace>      read-write: this is the work
//!   mount --bind <city>/.git      read-write, and NOT optional -- see below
//!   mount -t proc /proc           fresh, so `ps` shows this plan's 4 processes
//!                                 rather than the host's several hundred
//!   pivot_root . oldroot; umount -l oldroot
//! ```
//!
//! # Five things measured rather than assumed
//!
//! **The git directory has to be mounted, or `git` does not work at all.** A
//! plan's workspace is a *worktree*, and a worktree's `.git` is a file pointing
//! at `<city>/.git/worktrees/<id>` -- which is outside the workspace. Mount the
//! workspace alone and `git status` fails in a way that reads as a broken
//! repository. Measured in a real Kingdom worktree: with `<city>/.git` bound as
//! well, `git status` and `git log` are correct.
//!
//! **`/bin` is a symlink on every current distribution, and must not be
//! mounted.** Debian, Ubuntu, Fedora and Arch are all "merged-usr":
//! `/bin -> usr/bin`, `/sbin -> usr/bin`, `/lib -> usr/lib`. Bind-mounting
//! those would mount `/usr/bin` a second time under another name. What is
//! needed is that the *symlinks exist*, which is a `ln -s` and not a mount --
//! so [`Layout::of_host`] reads the host's own arrangement and reproduces it,
//! falling back to a bind for a machine that really does keep them apart.
//!
//! **`/etc/resolv.conf` is a symlink into somewhere that is not mounted.** On
//! this machine it points at `/mnt/wsl/resolv.conf`; on a systemd-resolved box
//! it points into `/run`. Either way, binding `/etc` gets a dangling link and
//! DNS fails while the network is perfectly up -- which reads as "the internet
//! is broken" rather than "one file is missing". A resolv.conf naming slirp's
//! own resolver is written and bound over the top.
//!
//! **A private `/tmp` hides the tmux socket, and tmux is then unusable.**
//! `tools::tmux` keeps its socket under the temp directory and talks to the
//! daemon from the **host** side, so a socket created inside a private `/tmp`
//! simply is not there for the 14 call sites that need it. Measured both ways.
//! The directories Kingdom itself owns under `/tmp` are therefore bound
//! through, while everything else in there stays private.
//!
//! **`--wdns`, not `--wd`, and not `current_dir`.** See
//! [`super::enter_prefix`], where that one is paid for.

use std::path::{Path, PathBuf};

/// Where the new root is assembled before `pivot_root` moves into it.
///
/// Under the runtime directory rather than the workspace, for the reason
/// `net::api_socket_path` gives: it belongs to a running process rather than to
/// the work, and a scratch directory inside a worktree would show up in
/// `git status` and in the review drawer.
fn scratch_root(plan: &str) -> PathBuf {
    let base = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    base.join("kingdom-sealed").join(plan)
}

/// How this host arranges `/bin`, `/sbin`, `/lib` and `/lib64`.
///
/// Read rather than assumed, because both arrangements are in the wild and
/// getting it wrong produces a namespace where nothing executes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Layout {
    /// Top-level names that are symlinks on this host, and what they point at.
    /// Reproduced with `ln -s`, because a bind mount of a symlink's target is
    /// the same filesystem twice under two names.
    pub symlinks: Vec<(String, String)>,
    /// Top-level names that are real directories on this host, and so have to
    /// be mounted like any other.
    pub directories: Vec<String>,
}

impl Layout {
    /// The arrangement of the machine Kingdom is running on.
    pub fn of_host() -> Self {
        Self::of(Path::new("/"))
    }

    /// The arrangement of a given root, so the decision can be tested against a
    /// fixture instead of against whatever the test machine happens to be.
    pub fn of(root: &Path) -> Self {
        let mut symlinks = Vec::new();
        let mut directories = Vec::new();
        for name in ["bin", "sbin", "lib", "lib64"] {
            let path = root.join(name);
            match std::fs::symlink_metadata(&path) {
                Ok(meta) if meta.file_type().is_symlink() => {
                    if let Ok(target) = std::fs::read_link(&path) {
                        symlinks.push((name.to_string(), target.to_string_lossy().to_string()));
                    }
                }
                // A real directory: it is genuinely separate from /usr on this
                // host and has to be mounted.
                Ok(meta) if meta.is_dir() => directories.push(name.to_string()),
                // Absent -- `/lib64` on most arm64 machines. Nothing to do, and
                // inventing one would be worse than leaving it out.
                _ => {}
            }
        }
        Self {
            symlinks,
            directories,
        }
    }
}

/// One folder let into a sealed plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bind {
    /// Where it is on the King's machine.
    pub source: PathBuf,
    /// Whether the plan may write to it.
    ///
    /// Read-only is the default everywhere it can be: a toolchain a plan can
    /// rewrite is a toolchain every *later* plan inherits the damage from.
    pub writable: bool,
    /// Whether the source must be created before it is mounted.
    ///
    /// For the scratch directories Kingdom owns under `/tmp` and talks through
    /// from outside. The browser profile is the case that forced this: it is
    /// created lazily when a browser first launches, which is *after* the
    /// holder has built its root -- so at bind time it usually does not exist
    /// yet, and `mount --rbind` of a missing source fails under `set -e` and
    /// takes the whole holder with it.
    pub ensure_source: bool,
}

impl Bind {
    pub fn read_only(source: impl Into<PathBuf>) -> Self {
        Self {
            source: source.into(),
            ensure_source: false,
            writable: false,
        }
    }

    pub fn writable(source: impl Into<PathBuf>) -> Self {
        Self {
            source: source.into(),
            ensure_source: false,
            writable: true,
        }
    }

    /// A directory Kingdom owns on the host and both sides must see, created
    /// if it is not there yet. See [`ensure_source`](Bind::ensure_source).
    pub fn scratch(source: impl Into<PathBuf>) -> Self {
        Self {
            source: source.into(),
            ensure_source: true,
            writable: true,
        }
    }
}

/// Everything a sealed plan's root is built from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MountPlan {
    /// Where the root is assembled.
    pub root: PathBuf,
    /// The host's `/bin`-and-friends arrangement, reproduced inside.
    pub layout: Layout,
    /// Everything bound in, in the order it is mounted. Order matters: a bind
    /// under another bind's path must come second or it is covered up.
    pub binds: Vec<Bind>,
    /// Where the plan's commands start, which must be a path that exists
    /// *inside*. See [`super::enter_prefix`].
    pub workdir: PathBuf,
}

impl MountPlan {
    /// What a sealed plan is given, before anything the King has declared.
    ///
    /// `city_root` is the project, which is not the workspace: an isolated
    /// plan's workspace is a worktree under `<city>/.kingdom/`, and its `.git`
    /// lives back in the city. Both are needed, and the city's git directory is
    /// the one that is easy to forget and fatal to omit.
    pub fn built_in(plan: &str, workspace: &Path, city_root: Option<&Path>) -> Self {
        Self::with_allowed(plan, workspace, city_root, &[])
    }

    /// The same, plus the folders the King has declared.
    ///
    /// Declared folders are added **last**, so one naming a path under
    /// something already mounted lands on top of it rather than under it -- the
    /// same ordering rule the private `/tmp` obeys, for the same reason.
    pub fn with_allowed(
        plan: &str,
        workspace: &Path,
        city_root: Option<&Path>,
        allowed: &[kingdom_core::services::MountSpec],
    ) -> Self {
        let mut binds = vec![
            Bind::read_only("/usr"),
            Bind::read_only("/etc"),
            // Writable because it is the King's real `/dev`: a plan needs
            // `/dev/null` and `/dev/urandom`, and both are written to.
            Bind::writable("/dev"),
        ];

        // The real directories of a split-usr host, if this is one. Nothing on
        // a merged-usr machine, which is almost every machine.
        let layout = Layout::of_host();
        for name in &layout.directories {
            binds.push(Bind::read_only(PathBuf::from("/").join(name)));
        }

        // The work itself.
        binds.push(Bind::writable(workspace));

        // The city's git directory, without which `git` does not work in a
        // worktree at all. Skipped when the workspace *is* the city -- an
        // in-place plan already has its `.git` inside the workspace bind.
        if let Some(city) = city_root {
            let git = city.join(".git");
            if git.exists() && !workspace.starts_with(city.join(".git")) && city != workspace {
                binds.push(Bind::writable(git));
            }
        }

        // The directories Kingdom itself keeps under /tmp and talks to from
        // *outside* the namespace. See the module docs: a tmux socket the host
        // cannot see is a tmux that does not work, and a browser profile the
        // namespace cannot see is a browser that will not launch.
        for shared in host_shared_temp_dirs(plan) {
            binds.push(Bind::scratch(shared));
        }

        // What the King allows in, expanded against his own home. A folder
        // that is not there is dropped rather than mounted: `mount --rbind` of
        // a missing source fails, and `set -e` would take the whole holder down
        // over one stale line in a manifest.
        let home = home_directory();
        for mount in allowed {
            let source = PathBuf::from(mount.expanded(&home));
            if !source.exists() {
                continue;
            }
            if binds.iter().any(|held| held.source == source) {
                continue;
            }
            binds.push(Bind {
                source,
                writable: mount.mode.is_writable(),
                // The King's own folder, which must be there already: creating
                // it would turn a typo in a manifest into an empty directory
                // silently mounted over nothing.
                ensure_source: false,
            });
        }

        Self {
            root: scratch_root(plan),
            layout,
            binds,
            workdir: workspace.to_path_buf(),
        }
    }
}

/// The King's home directory, for expanding a `~` in a declared mount.
///
/// `$HOME`, falling back to the passwd entry's own idea. Both can be wrong in
/// odd setups, and neither being right costs only that a `~`-rooted mount is
/// not found and so is skipped -- which is the same outcome as naming a folder
/// that is not there.
fn home_directory() -> String {
    std::env::var("HOME").unwrap_or_else(|_| {
        std::env::var("USER")
            .map(|user| format!("/home/{user}"))
            .unwrap_or_default()
    })
}

/// Directories under the temp directory that both sides must see.
///
/// `/tmp` inside a sealed plan is a private tmpfs, which is what stops two
/// plans colliding over a scratch file. These are the deliberate exceptions:
/// Kingdom creates them on the host and then talks through them to a process
/// inside, so a private copy would silently break the thing they exist for.
///
/// Keyed by the plan, because the browser profile is -- unlike the tmux socket
/// directory, which is one per uid.
fn host_shared_temp_dirs(plan: &str) -> Vec<PathBuf> {
    let temp = std::env::temp_dir();
    let uid = unsafe { libc::getuid() };
    vec![
        // `tools::tmux::socket_for` -- the daemon is inside, every one of the
        // 14 `cli()` calls is outside.
        temp.join(format!("kingdom-tmux-{uid}")),
        // The browser profile, which has exactly the same shape of problem and
        // was missed. `cpu_shim` writes `chrome-confined.sh` in here from the
        // host; the `nsenter` wrapper then executes it *inside*. With a private
        // /tmp the two are different filesystems, and every `browser_*` tool
        // fails with `nsenter: failed to execute ...: No such file or
        // directory`. Measured from inside a sealed plan.
        //
        // Asked of `kingdom-browser` rather than rebuilt here: the path is a
        // hash of the plan id, and two copies of a hash drift apart.
        kingdom_browser::profile_dir(plan),
    ]
}

/// The shell the holder runs to build its root, as one script.
///
/// Pure, and separate from spawning it, because that is what lets the whole
/// arrangement be tested on a machine with no namespaces in it -- the rule
/// AGENTS.md sets for the suite. Every trap in the module docs is visible in
/// this one string.
pub fn holder_script(plan: &MountPlan) -> String {
    let root = shell_quote(&plan.root.to_string_lossy());
    let mut out = String::new();

    // Nothing this script mounts may propagate back to the King's own mount
    // table. Without this the binds below are *shared* and a plan's private
    // arrangement leaks onto the host.
    out.push_str("set -e\n");
    out.push_str("mount --make-rprivate /\n");
    out.push_str(&format!("mkdir -p {root}\n"));
    // A bind of the root onto itself: `pivot_root` refuses a directory that is
    // not itself a mount point.
    out.push_str(&format!("mount --bind {root} {root}\n"));

    // Private and empty: scratch files here belong to this plan alone.
    //
    // **Before** the binds, not after, and that ordering is load-bearing: a
    // workspace under `/tmp` -- which is where a rehearsal or a test one lives
    // -- would be mounted first and then covered by this tmpfs, leaving the
    // plan with an empty directory where its work should be. Mounting the
    // tmpfs first means the binds land *on top of* it, which is what makes
    // both the private scratch space and the shared exceptions work.
    out.push_str(&format!("mkdir -p {root}/tmp\n"));
    out.push_str(&format!("mount -t tmpfs none {root}/tmp\n"));

    // Every directory the new root needs in its own right. `/proc` is the one
    // that bites: it is mounted *after* the pivot and so is never created by
    // the bind loop, and without it `pivot_root` succeeds and the very next
    // command fails with "mount point does not exist" -- taking the holder with
    // it, which surfaces much later as `nsenter: cannot open /proc/<pid>/ns/user`.
    // Measured exactly that way.
    out.push_str(&format!("mkdir -p {root}/proc {root}/run {root}/oldroot\n"));

    for bind in &plan.binds {
        let source = shell_quote(&bind.source.to_string_lossy());
        let target = shell_quote(&format!(
            "{}{}",
            plan.root.to_string_lossy(),
            bind.source.to_string_lossy()
        ));
        // A scratch directory Kingdom owns may legitimately not exist yet --
        // the browser profile is made when a browser first launches, long
        // after this script has run. Created here so the bind has something to
        // work with, because `mount --rbind` of a missing source is fatal
        // under `set -e`.
        if bind.ensure_source {
            out.push_str(&format!("mkdir -p {source}\n"));
        }
        out.push_str(&format!("mkdir -p {target}\n"));
        // `--rbind` rather than `--bind`: /usr and /dev have submounts, and a
        // plain bind silently leaves them out.
        out.push_str(&format!("mount --rbind {source} {target}\n"));
        if !bind.writable {
            // Read-only is a *second* operation on Linux; a bind cannot be
            // made read-only as it is created. Tolerated if it fails, because
            // a writable /usr is worse than a refusal only in theory, while a
            // plan that will not start is a plan that does nothing at all.
            out.push_str(&format!(
                "mount --bind -o remount,ro,rbind {target} || true\n"
            ));
        }
    }

    // The host's own /bin arrangement, reproduced. See the module docs.
    for (name, target) in &plan.layout.symlinks {
        out.push_str(&format!(
            "ln -sfn {} {}/{}\n",
            shell_quote(target),
            root,
            name
        ));
    }

    // Into the new root, and drop the old one so there is no way back to it.
    out.push_str(&format!("cd {root}\n"));
    out.push_str("pivot_root . oldroot\n");
    out.push_str("cd /\n");
    // A fresh /proc, which is what makes `ps` show this plan rather than the
    // machine. Mounted after the pivot because it must be the new root's.
    out.push_str("mount -t proc proc /proc\n");
    out.push_str("umount -l /oldroot\n");
    out.push_str("rmdir /oldroot || true\n");

    // DNS, and it has to be **here** -- after the pivot, not before it.
    //
    // slirp4netns answers on 10.0.2.3, but naming the resolver was never the
    // hard part. `/etc/resolv.conf` is an *absolute* symlink on both the hosts
    // that matter: `/mnt/wsl/resolv.conf` under WSL, `/run/systemd/resolve/...`
    // under systemd-resolved. Done before `pivot_root`, the kernel resolves
    // that symlink against the **host's** root, so a bind aimed at
    // `{root}/etc/resolv.conf` lands outside the new root entirely -- and the
    // old `|| true` then swallowed the failure, leaving a plan that routes
    // packets perfectly and cannot resolve a single name. Measured from inside
    // a sealed plan: `curl https://1.1.1.1` returned 301 while `getent hosts
    // crates.io` returned nothing at all.
    //
    // After the pivot the symlink means what it says inside the new root, so
    // there are two cases and both are handled rather than guessed at:
    // materialise what a symlink points at, and bind over a real file.
    out.push_str(&format!(
        "printf 'nameserver %s\\n' {SLIRP_RESOLVER} > /run/resolv.conf\n"
    ));
    out.push_str("if [ -L /etc/resolv.conf ]; then\n");
    // `-m` rather than `-f`: the target does not exist yet, which is the whole
    // problem, and `-f` fails on a dangling link while `-m` reports where it
    // would be.
    out.push_str("    target=$(readlink -m /etc/resolv.conf)\n");
    out.push_str("    mkdir -p \"$(dirname \"$target\")\"\n");
    out.push_str("    cp /run/resolv.conf \"$target\"\n");
    out.push_str("else\n");
    out.push_str("    mount --bind /run/resolv.conf /etc/resolv.conf\n");
    out.push_str("fi\n");
    // Checked rather than assumed. A plan that cannot resolve a hostname can
    // reach neither crates.io nor git, and finding that out ten minutes into a
    // build is far worse than being told here. `-s` follows the symlink, so
    // this is exactly the question "can something read a resolver from it".
    out.push_str("if [ ! -s /etc/resolv.conf ]; then\n");
    out.push_str("    echo 'kingdom: no resolver could be installed in the namespace' >&2\n");
    out.push_str("    exit 1\n");
    out.push_str("fi\n");

    // `lo` up, for the same reason the network-only holder does it: a plan's
    // own server on 127.0.0.1 is the entire point, and a fresh namespace's
    // loopback is DOWN.
    out.push_str("ip link set lo up\n");
    out.push_str("exec sleep infinity\n");
    out
}

/// The address slirp4netns answers DNS on.
///
/// Fixed by slirp's own defaults rather than chosen here, and named for the
/// same reason `net::GUEST_ADDR` is: two places must not drift apart about it.
const SLIRP_RESOLVER: &str = "10.0.2.3";

/// One argument, safe inside single quotes.
///
/// A workspace path comes from the King's own disk and can hold a space or a
/// quote. Hand-rolled because the alternative is a dependency for six lines,
/// and the rule is the whole of POSIX quoting: inside `'...'` everything is
/// literal, so only `'` itself has to be broken out.
fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', r"'\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A worktree plan gets its city's git directory, or `git` does not work.
    ///
    /// The trap this whole module is most likely to fall into again: the
    /// workspace looks self-contained, and it is not. Measured in a real
    /// Kingdom worktree before it was written.
    #[test]
    fn a_worktree_is_given_the_citys_git_directory() {
        let temp = std::env::temp_dir().join("kingdom-mount-test-worktree");
        let city = temp.join("city");
        let workspace = city.join(".kingdom").join("plan-1");
        std::fs::create_dir_all(city.join(".git")).unwrap();
        std::fs::create_dir_all(&workspace).unwrap();

        let plan = MountPlan::built_in("plan-1", &workspace, Some(&city));
        let sources: Vec<_> = plan.binds.iter().map(|b| b.source.clone()).collect();

        assert!(
            sources.contains(&city.join(".git")),
            "without the city's .git, `git status` fails in a worktree"
        );
        // And it must be writable: a plan commits.
        let git = plan
            .binds
            .iter()
            .find(|b| b.source == city.join(".git"))
            .unwrap();
        assert!(git.writable);

        let _ = std::fs::remove_dir_all(&temp);
    }

    /// The system is mounted read-only, the workspace is not.
    ///
    /// The one distinction that makes sealing worth anything: a plan may write
    /// its own work and may not rewrite the toolchain every later plan uses.
    #[test]
    fn the_system_is_read_only_and_the_work_is_not() {
        let workspace = PathBuf::from("/tmp/kingdom-mount-test/workspace");
        let plan = MountPlan::built_in("plan-2", &workspace, None);

        let usr = plan
            .binds
            .iter()
            .find(|b| b.source == Path::new("/usr"))
            .expect("/usr is always mounted");
        assert!(!usr.writable, "a plan must not rewrite the toolchain");

        let work = plan
            .binds
            .iter()
            .find(|b| b.source == workspace)
            .expect("the workspace is always mounted");
        assert!(work.writable, "the workspace is the work");
    }

    /// `/bin` is reproduced as a symlink, never mounted, on a merged-usr host.
    ///
    /// Bind-mounting it would mount `/usr/bin` twice under two names. Tested
    /// against a fixture rather than the real `/`, so the answer does not
    /// depend on which distribution the suite runs on.
    #[test]
    fn a_merged_usr_host_gets_symlinks_rather_than_mounts() {
        let root = std::env::temp_dir().join("kingdom-mount-test-merged");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("usr").join("bin")).unwrap();
        std::os::unix::fs::symlink("usr/bin", root.join("bin")).unwrap();
        std::os::unix::fs::symlink("usr/lib", root.join("lib")).unwrap();

        let layout = Layout::of(&root);

        assert!(layout.directories.is_empty(), "nothing to mount here");
        assert!(layout
            .symlinks
            .contains(&("bin".to_string(), "usr/bin".to_string())));
        assert!(layout
            .symlinks
            .contains(&("lib".to_string(), "usr/lib".to_string())));

        let _ = std::fs::remove_dir_all(&root);
    }

    /// A split-usr host gets mounts instead, because there the directories are
    /// genuinely separate.
    #[test]
    fn a_split_usr_host_gets_mounts_rather_than_symlinks() {
        let root = std::env::temp_dir().join("kingdom-mount-test-split");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("bin")).unwrap();
        std::fs::create_dir_all(root.join("lib")).unwrap();

        let layout = Layout::of(&root);

        assert!(layout.symlinks.is_empty());
        assert!(layout.directories.contains(&"bin".to_string()));
        assert!(layout.directories.contains(&"lib".to_string()));

        let _ = std::fs::remove_dir_all(&root);
    }

    /// The script pivots, drops the old root, and mounts a fresh `/proc`.
    ///
    /// Order is the whole of it: `/proc` before the pivot is the host's, and an
    /// `oldroot` left mounted is a way back out to the King's disk.
    #[test]
    fn the_script_pivots_and_drops_the_old_root() {
        let plan = MountPlan::built_in("plan-3", Path::new("/tmp/work"), None);
        let script = holder_script(&plan);

        let pivot = script.find("pivot_root").expect("it must pivot");
        let proc = script.find("mount -t proc").expect("a fresh /proc");
        let drop = script.find("umount -l /oldroot").expect("and drop the old");

        assert!(pivot < proc, "/proc before the pivot would be the host's");
        assert!(
            pivot < drop,
            "the old root can only be dropped once we are out of it"
        );
        assert!(
            script.contains("mount --make-rprivate /"),
            "without this the plan's mounts leak onto the King's own table"
        );
    }

    /// The private `/tmp` is mounted before anything under it, and every
    /// directory the new root needs exists before it is used.
    ///
    /// Both halves of this were real bugs, found by the live test and not by
    /// any of the pure ones:
    ///
    /// - The tmpfs was mounted **after** the binds, so a workspace under `/tmp`
    ///   -- which is exactly where a test or rehearsal workspace lives -- was
    ///   mounted and then covered up. The plan saw an empty directory where its
    ///   work should have been.
    /// - `/proc` was never created in the new root, because it is mounted after
    ///   the pivot and so is not part of the bind loop. `pivot_root` then
    ///   succeeded and the next line failed, killing the holder -- which
    ///   surfaced a whole second later as
    ///   `nsenter: cannot open /proc/<pid>/ns/user`, naming nothing that was
    ///   actually wrong.
    #[test]
    fn the_private_tmp_is_mounted_before_anything_beneath_it() {
        // A workspace under /tmp, which is the case that broke.
        let plan = MountPlan::built_in("plan-7", Path::new("/tmp/somewhere/work"), None);
        let script = holder_script(&plan);

        let tmpfs = script.find("mount -t tmpfs").expect("a private /tmp");
        let workspace = script
            .find("/tmp/somewhere/work'")
            .expect("the workspace is mounted");
        assert!(
            tmpfs < workspace,
            "a tmpfs mounted after the workspace hides it: {script}"
        );

        // And the pivot's own directories exist before the pivot needs them.
        let made = script.find("mkdir -p").expect("directories are made");
        let pivot = script.find("pivot_root").expect("it pivots");
        assert!(script.contains("/proc"), "the new root needs a /proc");
        assert!(made < pivot);
        let proc_dir = script
            .find("/proc /run/user")
            .or_else(|| script.find("/proc "))
            .expect("/proc is created, not merely mounted");
        assert!(
            proc_dir < script.find("mount -t proc").expect("a fresh /proc"),
            "creating /proc must come before mounting on it: {script}"
        );
    }

    /// A declared folder is mounted, in the mode it asked for.
    #[test]
    fn a_declared_folder_is_let_in() {
        use kingdom_core::services::{MountMode, MountSpec};

        // Real directories, because a mount whose source is absent is dropped
        // -- `mount --rbind` of a missing path fails, and `set -e` would take
        // the whole holder down over one stale line in a manifest.
        let root = std::env::temp_dir().join("kingdom-mount-test-allowed");
        let readable = root.join("toolchain");
        let writable = root.join("cache");
        std::fs::create_dir_all(&readable).unwrap();
        std::fs::create_dir_all(&writable).unwrap();

        let allowed = vec![
            MountSpec {
                path: readable.display().to_string(),
                mode: MountMode::Ro,
            },
            MountSpec {
                path: writable.display().to_string(),
                mode: MountMode::Rw,
            },
            MountSpec {
                path: root.join("not-there").display().to_string(),
                mode: MountMode::Ro,
            },
        ];

        let plan = MountPlan::with_allowed("plan-8", Path::new("/tmp/work"), None, &allowed);

        let bind = |p: &Path| plan.binds.iter().find(|b| b.source == p);
        assert!(
            !bind(&readable)
                .expect("declared folders are mounted")
                .writable
        );
        assert!(bind(&writable).expect("and in the mode asked for").writable);
        assert!(
            bind(&root.join("not-there")).is_none(),
            "a folder that is not there must be skipped, not mounted: \
             `set -e` would otherwise kill the holder over a stale line"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    /// `built_in` is `with_allowed` with nothing allowed.
    ///
    /// Cheap, and it pins the seam a real bug slipped through: `net::create`
    /// called `built_in` and so silently dropped every folder the King had
    /// declared. Nothing failed -- the namespace came up perfectly, and `cargo`
    /// simply was not in it. Only the live test caught it, and only because it
    /// ran a real toolchain.
    #[test]
    fn the_built_in_set_is_the_allowed_set_with_nothing_allowed() {
        let workspace = Path::new("/tmp/work");
        assert_eq!(
            MountPlan::built_in("plan-9", workspace, None).binds,
            MountPlan::with_allowed("plan-9", workspace, None, &[]).binds,
        );
    }

    /// DNS is given a resolver that exists, *after* the pivot.
    ///
    /// `/etc/resolv.conf` is an absolute symlink to somewhere unmounted on both
    /// WSL and systemd-resolved machines. Done before `pivot_root` the kernel
    /// resolves it against the host's root, so the bind lands outside the new
    /// root and the plan gets no resolver at all -- which looks like a plan
    /// with no network rather than one with no resolver.
    ///
    /// The ordering is the whole fix, so the ordering is what is asserted. The
    /// previous version of this test only looked for the two strings anywhere
    /// in the script, and passed throughout the entire time DNS was broken.
    #[test]
    fn the_namespace_is_given_a_resolver() {
        let plan = MountPlan::built_in("plan-4", Path::new("/tmp/work"), None);
        let script = holder_script(&plan);

        assert!(script.contains("10.0.2.3"), "slirp's own resolver");

        let pivot = script.find("pivot_root").expect("the script must pivot");
        let resolver = script
            .find("/etc/resolv.conf")
            .expect("the script must install a resolver");
        assert!(
            resolver > pivot,
            "the resolver must be installed after pivot_root, or an absolute \
             symlink resolves against the host's root and the bind misses"
        );
    }

    /// A dangling resolv.conf symlink is materialised rather than bound over.
    ///
    /// The WSL case, and the one that was actually broken: `/etc` is bound
    /// read-only, so there is no binding *over* the link -- what works is
    /// creating the file it already points at.
    #[test]
    fn a_symlinked_resolv_conf_is_followed() {
        let plan = MountPlan::built_in("plan-4a", Path::new("/tmp/work"), None);
        let script = holder_script(&plan);

        assert!(
            script.contains("readlink -m /etc/resolv.conf"),
            "a symlink's target has to be found before it can be filled in"
        );
        assert!(
            script.contains("[ -L /etc/resolv.conf ]"),
            "the two cases -- symlink and real file -- must be told apart"
        );
    }

    /// A namespace with no resolver refuses to start.
    ///
    /// The old script ended its bind with `|| true`, so the failure that left
    /// this plan unable to reach crates.io was silent. "You have no DNS" is
    /// worth refusing over.
    #[test]
    fn a_missing_resolver_is_not_swallowed() {
        let plan = MountPlan::built_in("plan-4b", Path::new("/tmp/work"), None);
        let script = holder_script(&plan);

        assert!(
            !script.contains("/etc/resolv.conf || true"),
            "the resolver's failure must not be swallowed"
        );
        assert!(
            script.contains("[ ! -s /etc/resolv.conf ]") && script.contains("exit 1"),
            "a namespace with no resolver has to say so"
        );
    }

    /// The tmux socket directory crosses the boundary.
    ///
    /// `/tmp` is private, and tmux's daemon is inside while all 14 of its
    /// callers are outside. Measured both ways: without this the socket is
    /// simply not there for the host.
    #[test]
    fn the_tmux_socket_directory_is_shared_with_the_host() {
        let plan = MountPlan::built_in("plan-5", Path::new("/tmp/work"), None);
        let uid = unsafe { libc::getuid() };
        let expected = std::env::temp_dir().join(format!("kingdom-tmux-{uid}"));

        assert!(
            plan.binds
                .iter()
                .any(|b| b.source == expected && b.writable),
            "a tmux socket the host cannot see is a tmux that does not work"
        );
    }

    /// The browser profile crosses the boundary too.
    ///
    /// The same shape of problem as the tmux socket, and missed when the sealed
    /// path was built. `cpu_shim` writes `chrome-confined.sh` into the profile
    /// from the host and the `nsenter` wrapper executes it inside; with a
    /// private `/tmp` those are two different filesystems and every `browser_*`
    /// tool dies with `nsenter: failed to execute ...: No such file or
    /// directory`. Measured from inside a sealed plan.
    #[test]
    fn the_browser_profile_is_shared_with_the_host() {
        let plan = MountPlan::built_in("plan-5a", Path::new("/tmp/work"), None);
        let expected = kingdom_browser::profile_dir("plan-5a");

        assert!(
            plan.binds
                .iter()
                .any(|b| b.source == expected && b.writable),
            "a browser profile the namespace cannot see is a browser that will \
             not launch"
        );
    }

    /// The profile is created before it is mounted.
    ///
    /// It is made lazily when a browser first launches, which is *after* the
    /// holder builds its root -- so without this the bind source is missing and
    /// `set -e` takes the whole holder down, which surfaces much later and much
    /// more confusingly as a namespace that will not open.
    #[test]
    fn a_scratch_directory_is_created_before_it_is_bound() {
        let plan = MountPlan::built_in("plan-5b", Path::new("/tmp/work"), None);
        let profile = kingdom_browser::profile_dir("plan-5b");
        let script = holder_script(&plan);

        let quoted = format!("'{}'", profile.display());
        let made = script
            .find(&format!("mkdir -p {quoted}\n"))
            .expect("the profile directory must be created");
        let bound = script
            .find(&format!("mount --rbind {quoted}"))
            .expect("the profile directory must be bound");
        assert!(made < bound, "the source must exist before it is bound");
    }

    /// A path with a space or a quote in it does not break the script.
    ///
    /// The King names his own folders, and `~/my code` is not exotic.
    #[test]
    fn an_awkward_path_is_quoted() {
        let plan = MountPlan::built_in("plan-6", Path::new("/tmp/my code/it's here"), None);
        let script = holder_script(&plan);

        assert!(script.contains(r"'/tmp/my code/it'\''s here'"));
    }
}

/// Tests that build a real namespace on a real kernel.
///
/// Opt-in, in the spirit of `kingdom-browser`'s own `--ignored` suite and
/// `services.rs`'s Docker tests: AGENTS.md requires the ordinary suite to run
/// on a bare machine, and these need `unshare`, `nsenter` and a kernel that
/// permits unprivileged user namespaces.
///
/// Run them with:
///
/// ```text
/// cargo test -p kingdom-app --features ssr --no-default-features -- --ignored sealed
/// ```
///
/// Everything they check was measured by hand before any of this was written.
/// They exist so the next person does not have to trust that.
#[cfg(test)]
mod live {
    use kingdom_core::{Isolation, PlanId};

    /// Where a plan's root is assembled, as the module itself decides it.
    fn plan_scratch_root(plan: &str) -> std::path::PathBuf {
        super::MountPlan::built_in(plan, std::path::Path::new("/tmp/unused"), None).root
    }

    /// Builds a sealed namespace and asks it questions from the outside.
    ///
    /// One test rather than six, deliberately: a namespace costs two processes
    /// and a second of setup, and six tests that each raise one would be six
    /// seconds and six chances to leak a holder.
    #[tokio::test]
    #[ignore = "creates a real namespace; needs unshare, nsenter and user namespaces"]
    async fn a_sealed_plan_gets_a_filesystem_of_its_own() {
        let plan = PlanId::new("plan-sealed-live-test");
        let workspace = std::env::temp_dir().join("kingdom-sealed-live/workspace");
        std::fs::create_dir_all(&workspace).expect("a workspace to seal around");
        std::fs::write(workspace.join("marker.txt"), "the work").unwrap();

        let request = crate::namespaces::Request {
            isolation: Isolation::Sealed,
            workspace: workspace.clone(),
            city_root: None,
            allowed: Vec::new(),
        };

        crate::namespaces::ensure(&plan, &request)
            .await
            .expect("a sealed namespace must come up");

        let run = |script: &str| {
            let mut argv = crate::namespaces::enter_prefix(&plan);
            assert!(!argv.is_empty(), "a sealed plan must have a prefix");
            argv.push("sh".to_string());
            argv.push("-c".to_string());
            argv.push(script.to_string());
            let out = std::process::Command::new(&argv[0])
                .args(&argv[1..])
                .output()
                .expect("the namespace must be enterable");
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        };

        // Commands start in the workspace. This is `--wdns` earning its keep:
        // with `current_dir` or `--wd` this answers `/`, and every build an
        // agent runs happens in the wrong place with no error.
        assert_eq!(run("pwd"), workspace.display().to_string());
        assert_eq!(run("cat marker.txt"), "the work");

        // The King's home is not there to be deleted. The whole point.
        assert_eq!(
            run("test -e /home/$(id -un 2>/dev/null || echo nobody)/.ssh && echo THERE || echo gone"),
            "gone"
        );

        // The toolchain is, and is read-only.
        assert_eq!(run("test -x /usr/bin/env && echo yes"), "yes");
        assert_eq!(
            run("touch /usr/bin/kingdom-test-write 2>/dev/null && echo WRITABLE || echo readonly"),
            "readonly",
            "a plan that can rewrite the toolchain damages every later plan"
        );

        // Its own process table: pid 1 is the holder's sleep, not the King's
        // init, and the count is a handful rather than several hundred.
        assert_eq!(run("cat /proc/1/comm"), "sleep");
        let processes: usize = run("ls -d /proc/[0-9]* | wc -l")
            .parse()
            .unwrap_or(usize::MAX);
        assert!(
            processes < 20,
            "a sealed plan should see only its own processes, saw {processes}"
        );

        crate::namespaces::shutdown(&plan);

        // Shutting a sealed plan down takes its scratch root with it. Left
        // behind, one skeleton root accumulates under `$XDG_RUNTIME_DIR` per
        // plan ever sealed -- found by looking after a live run rather than by
        // reasoning about it.
        assert!(
            !plan_scratch_root("plan-sealed-live-test").exists(),
            "a settled plan must leave nothing of its root behind"
        );

        let _ = std::fs::remove_dir_all(std::env::temp_dir().join("kingdom-sealed-live"));
    }

    /// The whole point, end to end: a declared toolchain works inside.
    ///
    /// `cargo --version` from a sealed namespace, using the King's own
    /// `~/.cargo` and `~/.rustup` mounted in by declaration. This is the claim
    /// the entire feature rests on -- that a sealed plan can still build -- and
    /// it is the one that cannot be checked without a real kernel.
    ///
    /// Skipped rather than failed where there is no cargo to find: the point is
    /// that a *declared* toolchain works, not that this machine has Rust.
    #[tokio::test]
    #[ignore = "creates a real namespace; needs unshare, nsenter and user namespaces"]
    async fn a_declared_toolchain_works_inside_a_sealed_plan() {
        use kingdom_core::services::{MountMode, MountSpec};

        let home = std::env::var("HOME").unwrap_or_default();
        if !std::path::Path::new(&home)
            .join(".cargo/bin/cargo")
            .exists()
        {
            eprintln!("no cargo under {home}; nothing to prove here");
            return;
        }

        let plan = PlanId::new("plan-sealed-toolchain-test");
        let workspace = std::env::temp_dir().join("kingdom-sealed-toolchain/workspace");
        std::fs::create_dir_all(&workspace).unwrap();

        let allowed = vec![
            MountSpec {
                path: "~/.cargo".to_string(),
                mode: MountMode::Rw,
            },
            MountSpec {
                path: "~/.rustup".to_string(),
                mode: MountMode::Rw,
            },
        ];

        crate::namespaces::ensure(
            &plan,
            &crate::namespaces::Request {
                isolation: Isolation::Sealed,
                workspace: workspace.clone(),
                city_root: None,
                allowed,
            },
        )
        .await
        .expect("a sealed namespace must come up");

        let mut argv = crate::namespaces::enter_prefix(&plan);
        argv.push("sh".to_string());
        argv.push("-c".to_string());
        argv.push(format!("PATH={home}/.cargo/bin:/usr/bin cargo --version"));
        let out = std::process::Command::new(&argv[0])
            .args(&argv[1..])
            .output()
            .expect("the namespace must be enterable");
        let said = String::from_utf8_lossy(&out.stdout);

        assert!(
            said.starts_with("cargo "),
            "a declared toolchain must actually run: {said:?} / {:?}",
            String::from_utf8_lossy(&out.stderr)
        );

        crate::namespaces::shutdown(&plan);
        let _ = std::fs::remove_dir_all(std::env::temp_dir().join("kingdom-sealed-toolchain"));
    }

    /// A plan with only a network of its own still has the King's filesystem.
    ///
    /// The other half of the ladder, and worth pinning because the two modes
    /// now share one `create`: a mistake there would silently seal every
    /// isolated plan, changing what an existing plan can see.
    #[tokio::test]
    #[ignore = "creates a real namespace; needs unshare, nsenter and user namespaces"]
    async fn a_network_only_plan_still_sees_the_machine() {
        let plan = PlanId::new("plan-network-only-live-test");

        crate::namespaces::ensure(&plan, &crate::namespaces::Request::network_only())
            .await
            .expect("a network namespace must come up");

        let mut argv = crate::namespaces::enter_prefix(&plan);
        argv.push("sh".to_string());
        argv.push("-c".to_string());
        argv.push("test -d /home && echo THERE || echo gone".to_string());
        let out = std::process::Command::new(&argv[0])
            .args(&argv[1..])
            .output()
            .expect("the namespace must be enterable");

        assert_eq!(
            String::from_utf8_lossy(&out.stdout).trim(),
            "THERE",
            "an isolated plan shares the King's filesystem, and must keep doing so"
        );
        // And no mount namespace was taken.
        assert!(
            !crate::namespaces::enter_prefix(&plan)
                .iter()
                .any(|a| a.starts_with("--mount=")),
            "only a sealed plan takes a mount namespace"
        );

        crate::namespaces::shutdown(&plan);
    }
}
