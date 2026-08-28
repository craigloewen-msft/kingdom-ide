//! Long-lived work: dev servers, watchers and REPLs, on a tmux server of the
//! plan's own.
//!
//! `bash` already answers "run this and tell me what happened". What it cannot
//! answer is "start this and let me come back to it in an hour, and let the
//! user look at it too": its handles live in this process's memory, its output
//! is a captured pipe, and nothing the user can attach to exists. tmux gives
//! all three for free -- a real terminal, real scrollback, and a surface a
//! human can attach to from his own shell.
//!
//! # Why a socket per plan
//!
//! This is the whole reason this module is worth its length. The default tmux
//! server is *shared*: a plan that runs `tmux kill-server`, or names a window
//! `dev` that already exists, or lists windows to decide what to stop, is
//! reaching straight into the user's own session and into every other plan's.
//! Two plans each starting a dev server is precisely the collision this product
//! exists to make visible, and it would be absurd to introduce a fresh one
//! here.
//!
//! So every plan gets its own tmux server, on a socket derived from its plan
//! id, and the pass-through **refuses** `-L` and `-S` in the leading server
//! flags. Refusing rather than silently dropping them: a model that asked for
//! another socket wanted something, and told plainly that the socket is fixed
//! it adapts, where a quietly ignored flag would have it believe it had
//! reached a server it never touched.
//!
//! # Why the boundary is thin here, like `bash`
//!
//! A window starts in [`Sandbox::root`] and that is the whole of the
//! containment. The pane holds an interactive shell: it can `cd /`, name an
//! absolute path, or `ssh` away entirely, and nothing here stops it. Stated
//! plainly rather than implied away -- a guarantee people believe in and that
//! does not hold is worse than a limit they can see. Closing it means an
//! OS-level sandbox, a deliberate later decision.

use super::{Refusal, Sandbox, Tool};
use kingdom_core::{ToolOutcome, WaitBudget};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::Output;
use std::sync::OnceLock;
use std::time::Duration;

/// How long any single tmux CLI call is given.
///
/// tmux subcommands return at once -- except the ones that are *meant* to
/// block: `attach-session` waits for a terminal that will never exist here, and
/// `wait-for` waits for a signal nobody sends. Without this a model could hang
/// the turn forever with one plausible-looking call.
const CLI_TIMEOUT: Duration = Duration::from_secs(10);

/// How far back a pane is read when reporting on it.
///
/// The tail of the scrollback, not the visible screen: a dev server that
/// printed its port and then scrolled it away is the common case, and a report
/// showing only the last 24 rows would miss exactly the line the model wants.
const CAPTURE_START: &str = "-500";

/// How often a readiness wait re-reads the pane.
///
/// Polling rather than tmux's own `wait-for`: readiness here is *text on a
/// screen*, which the command being waited on knows nothing about and cannot
/// signal. 100ms is below human notice and costs nothing next to a process
/// that is starting up.
const POLL: Duration = Duration::from_millis(100);

/// The longest a readiness wait may be asked to block.
///
/// A ceiling rather than trust, because the cost of a bad value is paid by the
/// user waiting on a turn that looks hung.
const MAX_READINESS_SECONDS: u64 = 300;

/// The session every window is created under.
///
/// One session rather than one per window, so `list-windows` without arguments
/// shows the model everything it has started.
const SESSION: &str = "main";

/// The argv prefix that puts a plan's tmux server in its own namespace --
/// refusing rather than silently falling through to the host network.
///
/// Adopts the same guard `terminal.rs` uses for the King's own shell: an
/// isolated plan whose prefix comes back empty must not get a tmux server on
/// the host network with a straight face about being isolated. That silent
/// fallback is the one outcome worse than a refusal -- a dev server started in
/// such a pane would bind the King's own `:3000` and answer for a project it
/// was never given.
fn isolated_enter_prefix(shop: &Sandbox) -> Result<Vec<String>, Refusal> {
    let enter = crate::namespaces::enter_prefix(shop.plan());
    let isolated = crate::api::snapshot(shop.plan()).is_some_and(|p| p.isolation.is_isolated());
    if isolated && enter.is_empty() {
        return Err(Refusal::Refused(
            "This plan's network could not be entered, so no tmux server was \
             started here. A server on the machine's own network would be \
             the wrong answer rather than a lesser one."
                .to_string(),
        ));
    }
    Ok(enter)
}

pub struct TmuxRun;
pub struct Tmux;

#[async_trait::async_trait]
impl Tool for TmuxRun {
    fn name(&self) -> &'static str {
        "tmux_run"
    }

    fn description(&self) -> String {
        "Start a command in a named tmux window and return immediately with its \
         window id. For dev servers, watchers, REPLs -- anything that is meant \
         to keep running and be looked at later.\n\n\
         The window starts in the workspace, on a tmux server private to this \
         plan: no other plan and no session of the King's can see or kill it. \
         The pane stays inspectable after the command exits, so you can still \
         read why it died.\n\n\
         Give `readiness` to block until the command prints something -- a \
         port, a `ready`, a prompt -- rather than guessing with sleeps.\n\n\
         Come back to the window with the `tmux` tool: capture-pane to read it, \
         send-keys to type at it, kill-window to stop it."
            .to_string()
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["cmd"],
            "properties": {
                "cmd": {
                    "type": "string",
                    // Worded to match `bash`, but deliberately *without* its
                    // "you never need to `cd` there" sentence. A pane holds a
                    // live shell whose working directory persists across
                    // send-keys, so the claim bash can make truthfully would be
                    // false here after the first `cd`.
                    "description": "The command, run via `bash -lc` in your workspace root."
                },
                "name": {
                    "type": "string",
                    "description": "Window name. Defaults to a short name taken from the command. Use it to find the window again."
                },
                "keep_open_on_exit": {
                    "type": "boolean",
                    "description": "Keep the pane readable after the command exits. Default true; set false only for windows you do not intend to inspect."
                },
                "readiness": {
                    "type": "object",
                    "description": "Block until this text appears in the pane. Omit to return as soon as the window exists.",
                    "required": ["text"],
                    "properties": {
                        "text": {
                            "type": "string",
                            "description": "Substring to wait for, e.g. `Listening on`."
                        },
                        "timeout_seconds": {
                            "type": "integer",
                            "description": "How long to wait for it. Default 30, at most 300. Timing out is reported, not an error -- the window keeps running."
                        }
                    }
                }
            }
        })
    }

    async fn run(&self, input: Value, shop: &Sandbox) -> ToolOutcome {
        if let Err(refusal) = tmux_is_installed() {
            return refusal.into();
        }

        let Some(cmd) = input
            .get("cmd")
            .and_then(Value::as_str)
            .filter(|c| !c.trim().is_empty())
        else {
            return Refusal::BadArguments {
                tool: "tmux_run".to_string(),
                detail: "a non-empty `cmd` is required".to_string(),
            }
            .into();
        };

        let readiness = match readiness(&input) {
            Ok(r) => r,
            Err(refusal) => return refusal.into(),
        };
        let name = input
            .get("name")
            .and_then(Value::as_str)
            .map_or_else(|| name_from(cmd), sanitise_name);
        let keep_open = input
            .get("keep_open_on_exit")
            .and_then(Value::as_bool)
            .unwrap_or(true);

        let socket = socket_for(shop);
        let enter = match isolated_enter_prefix(shop) {
            Ok(enter) => enter,
            Err(refusal) => return refusal.into(),
        };
        if let Err(reason) = ensure_server(&socket, shop.root(), shop.plan(), &enter).await {
            return Refusal::Refused(reason).into();
        }

        // `bash -lc` rather than bare exec: the model writes commands with
        // pipes and `&&` in them, and a login shell is also what puts the
        // user's toolchain managers on PATH -- a `cargo` that works in his
        // terminal and not in the pane is a bug report nobody can reproduce.
        let shell_command = format!("bash -lc {}", quote(cmd));
        // What this plan's children get beyond the server's own environment --
        // see [`super::child_environment`]. `-e` per variable rather than an
        // `export` prefixed onto the command, so nothing needs quoting twice
        // and the pane's own shell still sees them.
        let environment: Vec<String> = super::child_environment(shop)
            .into_iter()
            .map(|(key, value)| format!("{key}={value}"))
            .collect();
        let root = shop.root().to_string_lossy();
        let mut arguments = vec![
            "new-window",
            "-d",
            "-P",
            "-F",
            "#{window_id}",
            "-t",
            SESSION,
            "-n",
            &name,
            "-c",
            &root,
        ];
        for variable in &environment {
            arguments.push("-e");
            arguments.push(variable);
        }
        arguments.push(&shell_command);
        let started = cli(&socket, &arguments).await;

        let started = match started {
            Ok(out) => out,
            Err(reason) => return Refusal::Refused(reason).into(),
        };
        if !started.status.success() {
            return ToolOutcome::done(format!(
                "tmux would not open a window for `{cmd}`:\n{}",
                text(&started.stderr)
            ));
        }

        let window = text(&started.stdout).trim().to_string();

        if !keep_open {
            // Per-window, after the fact, because `new-window` has no flag for
            // it. A command that exits within the millisecond this takes will
            // still leave a dead pane behind -- harmless, and much cheaper than
            // flipping the server-wide default and racing every other window.
            let _ = cli(
                &socket,
                &["set-option", "-t", &window, "-w", "remain-on-exit", "off"],
            )
            .await;
        }

        let seen = match &readiness {
            Some((wanted, limit)) => await_text(&socket, &window, wanted, *limit).await,
            None => None,
        };

        ToolOutcome::done(report(&socket, &window, &name, cmd, readiness.as_ref(), seen).await)
    }

    /// Only when `readiness` was asked for: without it this returns as soon as
    /// the window is open, and a figure on the line would describe a wait that
    /// never happens.
    ///
    /// [`WaitBudget::Patience`], because the readiness wait is a wait on
    /// *watching*, not on the work. When it runs out the command is still
    /// running in its window and the model is told the text was not seen -- the
    /// same shape as a `bash` handle, and not a failure to flag.
    fn waits_for(&self, input: &Value) -> Option<WaitBudget> {
        let (_, limit) = readiness(input).ok().flatten()?;
        Some(WaitBudget::Patience {
            seconds: limit.as_secs(),
        })
    }
}

#[async_trait::async_trait]
impl Tool for Tmux {
    fn name(&self) -> &'static str {
        "tmux"
    }

    fn description(&self) -> String {
        "Run any tmux command against this plan's own tmux server.\n\n\
         Pass the subcommand and its flags as `args`, e.g.\n\
         [\"list-windows\"]\n\
         [\"capture-pane\", \"-p\", \"-t\", \"@3\", \"-S\", \"-200\"]\n\
         [\"send-keys\", \"-t\", \"@3\", \"q\", \"Enter\"]\n\
         [\"kill-window\", \"-t\", \"@3\"]\n\n\
         The server is chosen for you and is private to this plan; do not pass \
         `-L` or `-S` before the subcommand, they are refused. Nothing here \
         can see the King's tmux or another plan's.\n\n\
         Commands that wait for a terminal (`attach-session`) or for a signal \
         (`wait-for`) will simply time out -- there is no client attached."
            .to_string()
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["args"],
            "properties": {
                "args": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "The tmux command line, one argument per element. No shell quoting -- pass arguments already split."
                }
            }
        })
    }

    async fn run(&self, input: Value, shop: &Sandbox) -> ToolOutcome {
        if let Err(refusal) = tmux_is_installed() {
            return refusal.into();
        }

        let args = match args(&input) {
            Ok(args) => args,
            Err(refusal) => return refusal.into(),
        };

        let borrowed: Vec<&str> = args.iter().map(String::as_str).collect();
        match cli(&socket_for(shop), &borrowed).await {
            Err(reason) => Refusal::Refused(reason).into(),
            // A tmux that ran and complained has told the model something it
            // needs -- "can't find window @7" is how it learns the window is
            // gone. Only a call that never ran is a refusal.
            Ok(out) => ToolOutcome::done(joined(&out)),
        }
    }
}

/// Reads and vets the pass-through's arguments.
///
/// `-L`/`-S` are rejected only in the *leading* flags -- the ones tmux reads as
/// server selection, before the subcommand. Rejecting them everywhere would
/// break `capture-pane -S -200`, where `-S` is the scrollback start and has
/// nothing to do with sockets: the model would be told off for the single most
/// common call it makes.
fn args(input: &Value) -> Result<Vec<String>, Refusal> {
    let Some(items) = input.get("args").and_then(Value::as_array) else {
        return Err(Refusal::BadArguments {
            tool: "tmux".to_string(),
            detail: "`args` must be an array of strings, e.g. [\"list-windows\"]".to_string(),
        });
    };

    let mut args = Vec::with_capacity(items.len());
    for item in items {
        match item.as_str() {
            Some(s) => args.push(s.to_string()),
            None => {
                return Err(Refusal::BadArguments {
                    tool: "tmux".to_string(),
                    detail: format!("every element of `args` must be a string; {item} is not"),
                })
            }
        }
    }

    if args.is_empty() {
        return Err(Refusal::BadArguments {
            tool: "tmux".to_string(),
            detail: "`args` was empty; name a tmux subcommand, e.g. [\"list-windows\"]".to_string(),
        });
    }

    for arg in args.iter().take_while(|a| a.starts_with('-')) {
        if arg == "-L" || arg == "-S" {
            return Err(Refusal::Refused(format!(
                "{arg} would point tmux at a different server. This plan has a \
                 tmux server of its own and it is not negotiable -- it is what \
                 keeps its windows out of the King's session and out of other \
                 plans'. Drop {arg} and the rest of the command runs against \
                 the right server."
            )));
        }
    }

    Ok(args)
}

/// Reads the optional readiness wait.
fn readiness(input: &Value) -> Result<Option<(String, Duration)>, Refusal> {
    let Some(readiness) = input.get("readiness") else {
        return Ok(None);
    };
    if readiness.is_null() {
        return Ok(None);
    }

    let Some(text) = readiness
        .get("text")
        .and_then(Value::as_str)
        .filter(|t| !t.is_empty())
    else {
        return Err(Refusal::BadArguments {
            tool: "tmux_run".to_string(),
            detail: "`readiness` needs a non-empty `text` to wait for; omit \
                     `readiness` entirely to return straight away"
                .to_string(),
        });
    };

    let seconds = readiness
        .get("timeout_seconds")
        .and_then(Value::as_u64)
        .unwrap_or(30);
    if seconds == 0 || seconds > MAX_READINESS_SECONDS {
        return Err(Refusal::BadArguments {
            tool: "tmux_run".to_string(),
            detail: format!(
                "`readiness.timeout_seconds` must be between 1 and \
                 {MAX_READINESS_SECONDS}; {seconds} is not"
            ),
        });
    }

    Ok(Some((text.to_string(), Duration::from_secs(seconds))))
}

/// Whether tmux is on this machine at all.
///
/// Answered once and remembered: the answer cannot change under a running
/// server, and a `which` per call is a fork per call. The refusal names the
/// package because the model cannot install it and the user can.
fn tmux_is_installed() -> Result<(), Refusal> {
    static FOUND: OnceLock<bool> = OnceLock::new();
    let found = *FOUND.get_or_init(|| {
        std::process::Command::new("tmux")
            .arg("-V")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok()
    });

    if found {
        Ok(())
    } else {
        Err(Refusal::Refused(
            "tmux is not installed on this machine, so there is nowhere to run \
             a window. Use the `bash` tool instead -- it can start a long \
             command and hand you a handle -- and tell the King that installing \
             tmux would give him panes he can attach to himself."
                .to_string(),
        ))
    }
}

/// Kills the tmux server a plan was using, if it ever started one.
///
/// Called when a plan settles. Without this a dev server the model started
/// outlives the plan that owned it: still bound to port 3000, still holding a
/// target directory, and with nothing left in the records that knows what it
/// was for. That is precisely the orphaned-work collision this whole product
/// exists to prevent, so leaving it to the user to notice would be the product
/// committing its own founding mistake.
///
/// Deliberately infallible and quiet. It runs on the *success* path of merging
/// or archiving, where the work has already landed -- failing a completed merge
/// because a socket would not close would be a worse outcome than a stray tmux
/// server, which the user can kill himself and which dies with his next reboot.
pub async fn dismiss(plan: &kingdom_core::PlanId) {
    // The workspace is irrelevant here -- only the plan decides which socket to
    // knock on -- so a placeholder is honest rather than lazy.
    let shop =
        Sandbox::new(kingdom_core::Workspace::in_place(String::new())).for_plan(plan.clone());
    let socket = socket_for(&shop);

    // Nothing was ever started, so there is nothing to kill and no reason to
    // fork a process to find that out.
    if !socket.exists() {
        return;
    }
    if tmux_is_installed().is_err() {
        return;
    }

    let _ = cli(&socket, &["kill-server"]).await;
    let _ = std::fs::remove_file(&socket);
}

/// Where this plan's tmux server listens.
///
/// Derived from the plan id rather than handed out and remembered, so the
/// answer survives a restart of this process: a server started yesterday is
/// found again today, and the windows in it are not orphaned.
///
/// The id is not trusted to be a filename -- it is sanitised for legibility and
/// a hash of the whole id is appended so that two ids that sanitise alike still
/// get separate servers. A collision here would be exactly the leak the whole
/// module is built to prevent, so it is closed by construction rather than by
/// assuming ids are tame.
fn socket_for(shop: &Sandbox) -> PathBuf {
    let plan = shop.plan().as_str();
    let readable: String = plan
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .take(24)
        .collect();

    // Under the user's own directory: /tmp is shared, and a socket another
    // account could connect to is not isolation.
    let dir = std::env::temp_dir().join(format!("kingdom-tmux-{}", unsafe { libc::getuid() }));
    let _ = std::fs::create_dir_all(&dir);

    dir.join(format!("{readable}-{:016x}.sock", fingerprint(plan)))
}

/// FNV-1a over the plan id.
///
/// Not a cryptographic hash and does not need to be: nothing here defends
/// against a chosen collision, it only has to keep two honest ids apart. A
/// crypto hash would mean a dependency for eight lines of arithmetic.
fn fingerprint(s: &str) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in s.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100_0000_01b3);
    }
    hash
}

/// Makes sure the plan's server and its session exist, and that they belong
/// to the namespace this plan is in *right now*.
///
/// Done before every start rather than once, because the server is a process
/// like any other: the user may have killed it, or the machine may have
/// rebooted, and a cached "it exists" would turn that into a confusing failure
/// on the next window instead of a silent recovery.
///
/// **The mismatch this exists to catch.** A namespace lives in a process, not
/// on disk (see `namespaces::net::reclaim_previous`), so a server restart gives the plan
/// a *fresh* namespace while this daemon -- named from the plan id alone, and
/// found again on disk regardless of restarts -- is still the one from before.
/// `has-session` succeeding said nothing about which namespace answered.
/// Measured directly: kill the holder a restart would kill, and the daemon
/// answers `has-session` for a full minute afterwards, its every future window
/// landing in the orphaned namespace -- no slirp, no DNS, unreachable from
/// anything in the new one. So once the daemon is confirmed alive, its own
/// network namespace is compared against the one this plan should be in, and a
/// mismatch means starting over.
async fn ensure_server(
    socket: &Path,
    root: &Path,
    plan: &kingdom_core::PlanId,
    enter: &[String],
) -> Result<(), String> {
    let alive = cli(socket, &["has-session", "-t", SESSION]).await?;
    if alive.status.success() {
        if daemon_belongs_here(socket, plan).await {
            return Ok(());
        }
        // Wrong generation: this daemon's panes cannot reach anywhere the
        // *current* namespace can, and nothing later in this plan will notice
        // until a dev server it starts answers nothing. Starting over is the
        // only correct move -- there is no way to move a live pane to a
        // different namespace.
        let _ = cli(socket, &["kill-server"]).await;
        let _ = std::fs::remove_file(socket);
    }

    // The *server* is what has to be inside the namespace, not each window: a
    // tmux server is a daemon, and every pane it later opens is its child and
    // inherits its network. Entering per-window would be both redundant and
    // wrong -- the first `new-session` already forked the daemon outside.
    let created = enter_cli(
        enter,
        socket,
        &[
            "new-session",
            "-d",
            "-s",
            SESSION,
            "-c",
            &root.to_string_lossy(),
        ],
    )
    .await?;
    if !created.status.success() {
        return Err(format!(
            "this plan's tmux server would not start: {}",
            text(&created.stderr).trim()
        ));
    }

    // Stamped with the namespace this server was started in, so a later call
    // can tell it from one a previous Kingdom left behind. Written before the
    // options below because it is the thing that makes the daemon
    // *identifiable*: a server that came up but was never stamped is treated as
    // stale and restarted, which is correct but wasteful.
    //
    // Skipped for a plan with no namespace of its own, which has nothing to be
    // stale against -- `daemon_belongs_here` returns early for those.
    if let Some(holder) = crate::namespaces::holder_ns(plan) {
        let _ = cli(
            socket,
            &[
                "set-environment",
                "-g",
                HOLDER_STAMP,
                &holder.to_string_lossy(),
            ],
        )
        .await;
    }

    // Set on the server's window defaults, once, so a command that exits
    // instantly still leaves its output readable. Without it the window
    // vanishes with the process and the model is left with nothing to explain
    // why its dev server is not there.
    let _ = cli(socket, &["set-option", "-g", "-w", "remain-on-exit", "on"]).await;
    // Deep scrollback: a build that scrolls its error off the top is a build
    // whose failure the model cannot read.
    let _ = cli(socket, &["set-option", "-g", "history-limit", "20000"]).await;
    Ok(())
}

/// The tmux environment variable carrying the holder this server was started
/// under.
///
/// Written once at creation and read back to tell a live daemon from a stale
/// one. See [`daemon_belongs_here`] for why `#{pid}` cannot do this job.
const HOLDER_STAMP: &str = "KINGDOM_HOLDER_NS";

/// Whether a live tmux daemon is in the same namespaces this plan is meant to
/// be in.
///
/// A **shared plan is never mismatched**: it has no namespace of its own to
/// compare against, so any live daemon belongs to it by definition, and this
/// returns `true` without asking tmux anything more. Only an isolated plan can
/// have a daemon that has fallen behind a server restart.
///
/// # Why the daemon is stamped rather than asked for its pid
///
/// This used to read `#{pid}` and `readlink /proc/<pid>/ns/net`. That works for
/// a plan with only a network of its own and is **silently wrong for a sealed
/// one**: a sealed plan's tmux runs in a PID namespace, so `#{pid}` is a number
/// in *its* numbering, not the host's. Measured -- tmux reported `21`, and
/// `readlink /proc/21/ns/net` on the host fails or, far worse, names an
/// unrelated process that happens to be pid 21.
///
/// Every failure path here returns `true`, so the mismatch would be read as
/// "this daemon is fine" and the exact stale-daemon trap `docs/architecture.md`
/// documents would come back wearing a different hat: every window opened
/// afterwards lands in an orphaned namespace.
///
/// So the *holder* is stamped into the server's own environment when it is
/// created, and compared as a string. It crosses both namespaces because it is
/// the King's own number for the holder, recorded from out here where that
/// number means something.
async fn daemon_belongs_here(socket: &Path, plan: &kingdom_core::PlanId) -> bool {
    let Some(current) = crate::namespaces::holder_ns(plan) else {
        // Not isolated, or isolation not yet (re-)established for this call --
        // either way there is no namespace of the plan's own to disagree with.
        return true;
    };

    let Ok(out) = cli(socket, &["show-environment", "-g", HOLDER_STAMP]).await else {
        return true;
    };
    let stamped = text(&out.stdout);
    let Some(stamped) = stamped
        .trim()
        .strip_prefix(&format!("{HOLDER_STAMP}="))
        .map(str::to_string)
    else {
        // No stamp at all: a daemon from before this was introduced, or one
        // whose creation did not get this far. Either way it cannot be shown
        // to belong here, and restarting it is the cheap, safe answer.
        return false;
    };

    stamped == current.to_string_lossy()
}

/// One tmux invocation, always against this plan's socket.
///
/// The socket flag is prepended here, in the single place every call passes
/// through, so no caller -- and no model -- can arrange for a tmux that talks
/// to somebody else's server.
async fn cli(socket: &Path, args: &[&str]) -> Result<Output, String> {
    enter_cli(&[], socket, args).await
}

/// One tmux invocation, optionally inside a plan's network namespace.
///
/// Only the call that *starts* the server passes a prefix; every later call
/// talks to that daemon over its socket, and a UNIX socket crosses a network
/// namespace freely -- so the rest of this module needs no knowledge of any of
/// this.
async fn enter_cli(enter: &[String], socket: &Path, args: &[&str]) -> Result<Output, String> {
    let mut command = match enter.split_first() {
        Some((program, rest)) => {
            let mut command = std::process::Command::new(program);
            command.args(rest).arg("tmux");
            command
        }
        None => std::process::Command::new("tmux"),
    };
    command
        .arg("-S")
        .arg(socket)
        .args(args)
        .stdin(std::process::Stdio::null());

    // `spawn_blocking` rather than an async child: this crate's tokio has no
    // `io-util`, so a child's pipes cannot be read asynchronously -- the same
    // constraint `bash.rs` meets with reader threads. tmux calls return at
    // once, so one blocking thread each is a cheaper answer than a pipe pump.
    let run = tokio::task::spawn_blocking(move || command.output());

    match tokio::time::timeout(CLI_TIMEOUT, run).await {
        Ok(Ok(Ok(output))) => Ok(output),
        Ok(Ok(Err(e))) => Err(format!("tmux could not be run: {e}")),
        Ok(Err(e)) => Err(format!("tmux could not be run: {e}")),
        // The blocking call is left to finish on its own thread; there is
        // nothing to kill and nothing waiting on it.
        Err(_) => Err(format!(
            "tmux did not return within {}s. Commands that wait for a terminal \
             (attach-session) or a signal (wait-for) never will here -- there \
             is no client attached. Read the pane with capture-pane instead.",
            CLI_TIMEOUT.as_secs()
        )),
    }
}

/// Polls the pane until the awaited text shows up.
///
/// Returns whether it was seen. A timeout is deliberately not an error: the
/// command is still running, and the model's next move -- read more of the
/// pane, wait again, give up -- depends on what the pane says, which it is
/// about to be shown either way.
async fn await_text(socket: &Path, window: &str, wanted: &str, limit: Duration) -> Option<bool> {
    let deadline = std::time::Instant::now() + limit;
    loop {
        if capture(socket, window).await.contains(wanted) {
            return Some(true);
        }
        if std::time::Instant::now() >= deadline {
            return Some(false);
        }
        tokio::time::sleep(POLL).await;
    }
}

async fn capture(socket: &Path, window: &str) -> String {
    match cli(
        socket,
        &["capture-pane", "-p", "-t", window, "-S", CAPTURE_START],
    )
    .await
    {
        Ok(out) if out.status.success() => text(&out.stdout),
        Ok(out) => text(&out.stderr),
        Err(e) => e,
    }
}

/// What the model is told about a window it just started.
async fn report(
    socket: &Path,
    window: &str,
    name: &str,
    cmd: &str,
    readiness: Option<&(String, Duration)>,
    seen: Option<bool>,
) -> String {
    // `pane_dead_status` is why the exit code needs no marker printed into the
    // pane: tmux keeps it for a pane held open by remain-on-exit, so the model
    // reads the truth rather than something the command could have faked.
    let dead = cli(
        socket,
        &[
            "display-message",
            "-p",
            "-t",
            window,
            "#{pane_dead}|#{pane_dead_status}",
        ],
    )
    .await
    .map(|out| text(&out.stdout).trim().to_string())
    .unwrap_or_default();
    let (alive, code) = match dead.split_once('|') {
        Some(("1", status)) => (false, status.to_string()),
        _ => (true, String::new()),
    };

    let mut out = String::new();
    if alive {
        out.push_str(&format!("{window} ({name}) is running `{cmd}`."));
    } else {
        out.push_str(&format!(
            "{window} ({name}) ran `{cmd}` and it has already exited{}. The \
             pane is kept so you can still read it.",
            if code.is_empty() {
                String::new()
            } else {
                format!(" with code {code}")
            }
        ));
    }

    if let Some((wanted, limit)) = readiness {
        out.push_str(&match seen {
            Some(true) => format!(" `{wanted}` appeared, so it is ready."),
            _ => format!(
                " `{wanted}` did not appear within {}s. It was NOT stopped -- \
                 read the pane below and decide.",
                limit.as_secs()
            ),
        });
    }

    out.push_str(&format!(
        "\nRead it again with tmux [\"capture-pane\", \"-p\", \"-t\", \"{window}\"], \
         stop it with tmux [\"kill-window\", \"-t\", \"{window}\"].\n\n"
    ));

    let pane = capture(socket, window).await;
    let pane = pane.trim_end();
    if pane.is_empty() {
        out.push_str("(the pane is empty so far)");
    } else {
        out.push_str(pane);
    }
    out
}

/// A window name taken from the command.
///
/// The first word, because that is what a human scanning `list-windows` for
/// "the one running the server" is looking for.
fn name_from(cmd: &str) -> String {
    let word = cmd.split_whitespace().next().unwrap_or("cmd");
    let word = word.rsplit('/').next().unwrap_or(word);
    sanitise_name(word)
}

/// Keeps a name usable as a tmux target.
///
/// `.` and `:` are tmux's own window/pane separators, so a name containing them
/// produces a window the model can create and then never address again.
fn sanitise_name(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .take(32)
        .collect();
    if cleaned.trim_matches('-').is_empty() {
        "cmd".to_string()
    } else {
        cleaned
    }
}

/// Wraps a command so tmux hands it to the shell whole.
///
/// tmux re-splits the command string it is given, so a command with spaces or
/// quotes in it would arrive at `bash -lc` in pieces. Single quotes with the
/// `'\''` escape are the one form no shell reinterprets.
fn quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// stdout and stderr as the model sees them.
///
/// Interleaved into one answer, and stderr is *not* dropped on success: tmux
/// reports "no server running" and "can't find window" there, which is the
/// information the model most needs.
fn joined(out: &Output) -> String {
    let mut body = text(&out.stdout);
    let err = text(&out.stderr);
    if !err.trim().is_empty() {
        if !body.is_empty() && !body.ends_with('\n') {
            body.push('\n');
        }
        body.push_str(&err);
    }
    if body.trim().is_empty() {
        "(tmux said nothing)".to_string()
    } else {
        body
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kingdom_core::{PlanId, Workspace};

    /// Every test here needs a real tmux. Skipping rather than failing when
    /// there is none: a machine without tmux is a machine where these tools
    /// refuse politely, which is correct behaviour and not a broken build.
    fn have_tmux() -> bool {
        tmux_is_installed().is_ok()
    }

    fn shop_for(plan: &str, root: &Path) -> Sandbox {
        Sandbox::new(Workspace::in_place(root.to_str().unwrap()))
            .for_plan(PlanId::new(plan.to_string()))
    }

    fn done(outcome: ToolOutcome) -> String {
        match outcome {
            ToolOutcome::Done { output, .. } => output,
            ToolOutcome::Refused { reason } => panic!("refused: {reason}"),
        }
    }

    /// The point of the whole module. Two plans running dev servers that can
    /// see each other's windows -- and so kill them, or collide on a name --
    /// is the exact failure this product exists to prevent; introducing it in
    /// the tool that runs dev servers would be the worst possible place.
    #[tokio::test]
    async fn one_plans_window_is_invisible_to_another() {
        if !have_tmux() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let mine = shop_for("plan-alpha", dir.path());
        let theirs = shop_for("plan-beta", dir.path());

        let started = done(
            TmuxRun
                .run(json!({"cmd": "sleep 30", "name": "secret"}), &mine)
                .await,
        );
        assert!(started.contains("secret"), "{started}");

        let seen_by_owner = done(Tmux.run(json!({"args": ["list-windows"]}), &mine).await);
        assert!(seen_by_owner.contains("secret"), "{seen_by_owner}");

        let seen_by_other = done(Tmux.run(json!({"args": ["list-windows"]}), &theirs).await);
        assert!(
            !seen_by_other.contains("secret"),
            "another plan must not see this window: {seen_by_other}"
        );

        let _ = cli(&socket_for(&mine), &["kill-server"]).await;
        let _ = cli(&socket_for(&theirs), &["kill-server"]).await;
    }

    /// The isolation is only as good as the pass-through's refusal to be
    /// steered elsewhere. A `-L` that were merely ignored would leave the model
    /// believing it had reached another server.
    #[tokio::test]
    async fn a_server_flag_in_the_pass_through_is_refused() {
        let shop = shop_for("plan-alpha", Path::new("/tmp"));
        for escape in [
            json!({"args": ["-L", "default", "list-sessions"]}),
            json!({"args": ["-S", "/tmp/other.sock", "kill-server"]}),
        ] {
            assert!(
                matches!(
                    Tmux.run(escape.clone(), &shop).await,
                    ToolOutcome::Refused { .. }
                ),
                "{escape} must not be allowed to choose a server"
            );
        }
    }

    /// The other half: `-S` after the subcommand is scrollback depth, not a
    /// socket. Refusing it would break the single most common call the model
    /// makes against a window it started.
    #[test]
    fn scrollback_depth_is_not_a_server_flag() {
        assert!(args(&json!({"args": ["capture-pane", "-p", "-t", "@1", "-S", "-200"]})).is_ok());
    }

    /// Readiness is the reason this tool is not just `bash` with a pane: the
    /// model needs to know a server is *up*, not merely started, and the
    /// alternative it reaches for otherwise is a sleep long enough to be wrong
    /// in both directions.
    #[tokio::test]
    async fn a_wait_for_text_returns_when_the_text_appears() {
        if !have_tmux() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let shop = shop_for("plan-ready", dir.path());

        let output = done(
            TmuxRun
                .run(
                    json!({
                        "cmd": "echo listening on 3000; sleep 30",
                        "name": "server",
                        "readiness": {"text": "listening on", "timeout_seconds": 10}
                    }),
                    &shop,
                )
                .await,
        );

        assert!(output.contains("it is ready"), "{output}");
        let _ = cli(&socket_for(&shop), &["kill-server"]).await;
    }

    /// A window starts where the plan works. Anywhere else and the model's
    /// first `cargo build` runs against somebody else's checkout.
    #[tokio::test]
    async fn a_window_starts_in_the_workspace() {
        if !have_tmux() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let marker = "kingdom-was-here";
        let shop = shop_for("plan-cwd", dir.path());

        done(
            TmuxRun
                .run(
                    json!({
                        "cmd": format!("touch {marker}; sleep 30"),
                        "readiness": {"text": "$", "timeout_seconds": 1}
                    }),
                    &shop,
                )
                .await,
        );

        let landed = tokio::time::timeout(Duration::from_secs(5), async {
            while !dir.path().join(marker).exists() {
                tokio::time::sleep(POLL).await;
            }
        })
        .await;
        assert!(landed.is_ok(), "the window did not start in the workspace");
        let _ = cli(&socket_for(&shop), &["kill-server"]).await;
    }
}

#[cfg(test)]
mod environment_tests {
    use super::*;
    use kingdom_core::Workspace;
    use serde_json::json;

    /// A rehearsal server started in a pane must inherit the mock, and keep its
    /// records inside the workspace.
    ///
    /// Against a real tmux, because the mechanism *is* the `-e` flag: a unit
    /// test over `child_environment` alone would still pass if these arguments
    /// were assembled wrongly, and assembling them wrongly is the whole risk.
    /// Verified to fail when the environment is dropped from the call.
    #[tokio::test]
    async fn a_pane_in_a_kingdom_checkout_is_pointed_at_the_mock() {
        let dir = tempfile::tempdir().expect("a temporary workspace");
        let marker = dir.path().join("crates").join("kingdom-app");
        std::fs::create_dir_all(&marker).expect("the marker's directory");
        std::fs::write(marker.join("Cargo.toml"), "[package]\n").expect("the marker");

        let shop = Sandbox::new(Workspace::in_place(dir.path().display().to_string()))
            .for_plan(kingdom_core::PlanId::new("plan-env".to_string()));

        let outcome = TmuxRun
            .run(
                json!({
                    "cmd": "echo model=$KINGDOM_MODEL home=$KINGDOM_HOME",
                    "name": "env-check",
                    "readiness": {"text": "model=", "timeout_seconds": 10}
                }),
                &shop,
            )
            .await;

        let said = format!("{outcome:?}");
        assert!(said.contains("model=mock"), "{said}");
        assert!(
            said.contains(&format!("home={}", dir.path().display())),
            "the pane kept its records outside the workspace: {said}"
        );

        let _ = cli(&socket_for(&shop), &["kill-server"]).await;
    }
}
