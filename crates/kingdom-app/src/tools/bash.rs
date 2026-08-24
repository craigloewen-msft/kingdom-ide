//! Running a command, and living with the ones that do not finish.
//!
//! The court's hands. Everything a plan actually *does* to a machine -- build,
//! test, `git`, a script -- arrives here.
//!
//! # Why a command outlives its call
//!
//! The obvious shape is "run it, wait for it, return the output", with a
//! timeout that kills anything slow. That shape breaks on the one command the
//! court runs most: a build. A cold `cargo build` takes longer than any timeout
//! anybody is willing to sit through, and killing it at the deadline throws
//! away minutes of work and leaves a half-written target directory behind --
//! then the model retries and pays the same cost again.
//!
//! So `wait_seconds` here is *not* a kill timeout. It is how long this one call
//! blocks. When it elapses the process is left running and the caller is handed
//! a handle to come back to. Nothing in this module ever terminates a process
//! because time passed; the only thing that kills is [`Op::Kill`], asked for
//! explicitly.
//!
//! # Why the boundary is thinner here than anywhere else
//!
//! The command's working directory is [`Sandbox::root`], and that is the whole
//! of the containment. A shell can `cd /`, name an absolute path, or `ssh`
//! somewhere else entirely, and nothing here stops it. That hole is stated
//! plainly rather than papered over, because a guarantee people believe in and
//! that does not hold is worse than a limit they can see. Closing it means an
//! OS-level sandbox -- a deliberate later decision, not an oversight.
//!
//! # Why the output is capped
//!
//! A watched test runner or a chatty build can emit gigabytes. Retaining it all
//! would let one runaway command take the whole server's memory with it, so a
//! ring buffer keeps the tail and *says* what it dropped. Silent truncation
//! would be worse than the crash: the model would reason from output it thinks
//! is complete.

use super::{Refusal, Tool, Sandbox};
use kingdom_core::ToolOutcome;
use serde_json::{json, Value};
use std::collections::{HashMap, VecDeque};
use std::fmt::Write as _;
use std::io::Read;
use std::os::unix::process::{CommandExt as _, ExitStatusExt as _};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};
use tokio::sync::watch;

/// How much of a command's output is kept.
///
/// The *tail*, not the head: when a build fails, the error is at the end. A cap
/// on the head would faithfully retain a megabyte of "Compiling ..." and drop
/// the one line the court needed.
const RING_BYTES: usize = 256 * 1024;

/// Lines returned by a peek when the caller does not say.
///
/// Enough to see how a run is going without spending the model's context on a
/// build log it will scroll past.
const DEFAULT_PEEK_LINES: usize = 200;

/// How long a finished handle is remembered.
///
/// A tombstone has to outlive the call that started it -- that is the whole
/// point -- but not the session. Without an upper bound the registry grows by
/// one entry per command ever run, for the life of the server.
const TOMBSTONE_LIFETIME: Duration = Duration::from_secs(30 * 60);

/// Every command this process has started and not yet forgotten.
///
/// Process-global rather than per-[`Sandbox`] because a handle must survive
/// the call that minted it: the whole contract is that the court comes back for
/// it in a *later* deed, with a fresh workshop. Follows the registry pattern in
/// `herald.rs`.
static JOBS: OnceLock<Mutex<HashMap<String, Arc<Job>>>> = OnceLock::new();

fn jobs() -> &'static Mutex<HashMap<String, Arc<Job>>> {
    JOBS.get_or_init(|| Mutex::new(HashMap::new()))
}

pub struct Bash;

#[async_trait::async_trait]
impl Tool for Bash {
    fn name(&self) -> &'static str {
        "bash"
    }

    fn description(&self) -> String {
        "Run a shell command in the workspace and return its combined output \
         and exit code.\n\n\
         `wait_seconds` is how long this call blocks -- it is NOT a kill \
         timeout. If the command finishes first you get everything. If it does \
         not, the command keeps running and you get a handle to come back to \
         with op=peek, op=wait or op=kill. Nothing is ever killed because time \
         passed.\n\n\
         op=run    start a command (needs `cmd`)\n\
         op=peek   read a handle's output so far (needs `handle`)\n\
         op=wait   block up to `wait_seconds` for a handle to finish\n\
         op=kill   signal a handle and everything it started (TERM, or KILL)"
            .to_string()
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["op"],
            "properties": {
                "op": {
                    "type": "string",
                    "enum": ["run", "peek", "wait", "kill"],
                    "description": "What to do."
                },
                "cmd": {
                    "type": "string",
                    "description": "The shell command, for op=run. Runs under `bash -c` with the workspace as its working directory."
                },
                "handle": {
                    "type": "string",
                    "description": "The handle from an earlier op=run, for op=peek, op=wait and op=kill."
                },
                "wait_seconds": {
                    "type": "integer",
                    "description": "How long this call blocks before handing back a handle (op=run, op=wait). Default 30. The process is never killed when it elapses."
                },
                "lines": {
                    "type": "integer",
                    "description": "For op=peek: return only the last N lines. Default 200."
                },
                "signal": {
                    "type": "string",
                    "enum": ["TERM", "KILL"],
                    "description": "For op=kill. Default TERM. Sent exactly once -- there is no automatic escalation to KILL, so ask again with KILL if TERM is ignored."
                }
            }
        })
    }

    async fn run(&self, input: Value, shop: &Sandbox) -> ToolOutcome {
        match op(&input) {
            Err(refusal) => refusal.into(),
            Ok(Op::Run) => start(&input, shop).await,
            Ok(Op::Peek) => peek(&input),
            Ok(Op::Wait) => match job(&input) {
                Ok(job) => {
                    let waited = job.settle(wait_seconds(&input)).await;
                    ToolOutcome::done(job.report(waited, DEFAULT_PEEK_LINES))
                }
                Err(refusal) => refusal.into(),
            },
            Ok(Op::Kill) => kill(&input),
        }
    }
}

enum Op {
    Run,
    Peek,
    Wait,
    Kill,
}

fn op(input: &Value) -> Result<Op, Refusal> {
    match input.get("op").and_then(Value::as_str) {
        Some("run") => Ok(Op::Run),
        Some("peek") => Ok(Op::Peek),
        Some("wait") => Ok(Op::Wait),
        Some("kill") => Ok(Op::Kill),
        other => Err(Refusal::BadArguments {
            tool: "bash".to_string(),
            detail: match other {
                Some(op) => format!("`{op}` is not an op; use run, peek, wait or kill"),
                None => "no `op` was given; use run, peek, wait or kill".to_string(),
            },
        }),
    }
}

fn wait_seconds(input: &Value) -> Duration {
    Duration::from_secs(input.get("wait_seconds").and_then(Value::as_u64).unwrap_or(30))
}

/// Looks a handle up, refusing when it is not there.
///
/// An unknown handle is a [`Refusal`] and not an empty result: the model asked
/// about something that does not exist, and told so it can start again rather
/// than concluding the command produced nothing.
fn job(input: &Value) -> Result<Arc<Job>, Refusal> {
    let Some(id) = input.get("handle").and_then(Value::as_str) else {
        return Err(Refusal::BadArguments {
            tool: "bash".to_string(),
            detail: "no `handle` was given".to_string(),
        });
    };
    let found = jobs().lock().ok().and_then(|j| j.get(id).cloned());
    found.ok_or_else(|| {
        Refusal::Refused(format!(
            "There is no handle {id} here. It was never started, or it \
             finished long enough ago to have been forgotten."
        ))
    })
}

async fn start(input: &Value, shop: &Sandbox) -> ToolOutcome {
    let Some(cmd) = input.get("cmd").and_then(Value::as_str) else {
        return Refusal::BadArguments {
            tool: "bash".to_string(),
            detail: "op=run needs a `cmd`".to_string(),
        }
        .into();
    };

    let job = match Job::spawn(cmd, shop.root().to_path_buf()) {
        Ok(job) => job,
        // A shell that would not start is not a command that failed -- there is
        // no exit code to report, so this is the one place a run refuses.
        Err(e) => return Refusal::Refused(format!("the command could not be started: {e}")).into(),
    };

    {
        let mut registry = match jobs().lock() {
            Ok(r) => r,
            Err(poisoned) => poisoned.into_inner(),
        };
        forget_the_long_dead(&mut registry);
        registry.insert(job.id.clone(), job.clone());
    }

    let settled = job.settle(wait_seconds(input)).await;
    ToolOutcome::done(job.report(settled, usize::MAX))
}

fn peek(input: &Value) -> ToolOutcome {
    let job = match job(input) {
        Ok(job) => job,
        Err(refusal) => return refusal.into(),
    };
    let lines = input
        .get("lines")
        .and_then(Value::as_u64)
        .map_or(DEFAULT_PEEK_LINES, |n| (n as usize).max(1));

    ToolOutcome::done(job.report(job.finished(), lines))
}

fn kill(input: &Value) -> ToolOutcome {
    let job = match job(input) {
        Ok(job) => job,
        Err(refusal) => return refusal.into(),
    };

    let signal = match input.get("signal").and_then(Value::as_str) {
        None | Some("TERM") => libc::SIGTERM,
        Some("KILL") => libc::SIGKILL,
        Some(other) => {
            return Refusal::BadArguments {
                tool: "bash".to_string(),
                detail: format!("`{other}` is not a signal here; use TERM or KILL"),
            }
            .into()
        }
    };

    ToolOutcome::done(job.signal(signal))
}

/// Drops handles that finished long ago.
///
/// Done on insert rather than by a background sweeper, because a sweeper is a
/// task that has to be started, owned and shut down to reclaim a few kilobytes.
/// A registry only grows when something is added to it, so that is the one
/// moment it needs tidying.
fn forget_the_long_dead(registry: &mut HashMap<String, Arc<Job>>) {
    registry.retain(|_, job| match job.finished() {
        Some(end) => end.at.elapsed() < TOMBSTONE_LIFETIME,
        None => true,
    });
}

/// One command, and everything anybody will later want to know about it.
struct Job {
    id: String,
    cmd: String,
    /// The process group, not the pid. Signalling the group is what makes
    /// killing a `cargo test` take the test binaries with it; signalling the
    /// pid alone reparents the children to init, where they hold the target
    /// directory lock and nothing left knows they exist.
    pgid: i32,
    started: Instant,
    output: Mutex<Ring>,
    /// Set once, by the reaper. A watch channel rather than a `Notify` because
    /// `wait` must work for a caller that arrives *after* the process is
    /// already dead -- a notification has no such memory, and that caller would
    /// block until its deadline over a process that ended a minute ago.
    end: watch::Sender<Option<Ending>>,
    /// The signal already sent, if any. Recorded so a second `kill` says so
    /// instead of quietly sending TERM again: signals do not queue, and a model
    /// told "sent" twice believes it tried twice.
    signalled: Mutex<Option<&'static str>>,
}

#[derive(Clone, Copy)]
struct Ending {
    code: Option<i32>,
    signal: Option<i32>,
    at: Instant,
    ran_for: Duration,
}

impl Job {
    fn spawn(cmd: &str, cwd: PathBuf) -> std::io::Result<Arc<Self>> {
        let mut command = std::process::Command::new("bash");
        command
            .arg("-c")
            .arg(cmd)
            .current_dir(&cwd)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());

        // The child leads its own process group, so a later kill reaches the
        // whole tree. Set in the child between fork and exec because doing it
        // from the parent races: a command that spawns immediately could have
        // grandchildren in the old group before the parent got there.
        unsafe {
            command.pre_exec(|| {
                if libc::setpgid(0, 0) == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }

        let mut child = command.spawn()?;
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();

        let (end, _) = watch::channel(None);
        let job = Arc::new(Job {
            id: format!("b-{}", &uuid::Uuid::new_v4().simple().to_string()[..8]),
            cmd: cmd.to_string(),
            pgid: child.id() as i32,
            started: Instant::now(),
            output: Mutex::new(Ring::new(RING_BYTES)),
            end,
            signalled: Mutex::new(None),
        });

        // Plain threads rather than async reads: the crate's tokio has no
        // `io-util`, so a child's pipes cannot be read asynchronously here. Two
        // blocking readers per command is the honest cost of that, and it keeps
        // stdout and stderr interleaved into one ring the way a terminal shows
        // them.
        let readers: Vec<_> = [stdout.map(Pipe::Out), stderr.map(Pipe::Err)]
            .into_iter()
            .flatten()
            .map(|pipe| {
                let job = job.clone();
                std::thread::spawn(move || pipe.drain_into(&job))
            })
            .collect();

        let reaper = job.clone();
        std::thread::spawn(move || {
            // Joined before waiting so the ring holds everything the process
            // wrote by the time the ending is published. A caller that sees
            // "exited" and then reads a truncated log has been lied to.
            for reader in readers {
                let _ = reader.join();
            }
            let status = child.wait();
            let _ = reaper.end.send(Some(Ending {
                code: status.as_ref().ok().and_then(|s| s.code()),
                signal: status.as_ref().ok().and_then(|s| s.signal()),
                at: Instant::now(),
                ran_for: reaper.started.elapsed(),
            }));
        });

        Ok(job)
    }

    fn finished(&self) -> Option<Ending> {
        *self.end.borrow()
    }

    /// Blocks up to `limit` for the process to end.
    ///
    /// Returning `None` on expiry is not a failure and never touches the
    /// process: the deadline governs this call, not the command.
    async fn settle(&self, limit: Duration) -> Option<Ending> {
        let mut rx = self.end.subscribe();
        let _ = tokio::time::timeout(limit, async {
            while rx.borrow_and_update().is_none() {
                if rx.changed().await.is_err() {
                    break;
                }
            }
        })
        .await;
        self.finished()
    }

    fn signal(&self, signal: i32) -> String {
        let name = if signal == libc::SIGKILL { "KILL" } else { "TERM" };

        if let Some(end) = self.finished() {
            return format!(
                "{} already finished; nothing was signalled.\n\n{}",
                self.id,
                self.verdict(Some(end))
            );
        }

        // Negated: the target is the group, which is what reaches the
        // grandchildren a build spawns.
        let sent = unsafe { libc::kill(-self.pgid, signal) };
        if sent == -1 {
            return format!(
                "{}: {name} could not be delivered: {}",
                self.id,
                std::io::Error::last_os_error()
            );
        }

        let previous = match self.signalled.lock() {
            Ok(mut held) => held.replace(name),
            Err(poisoned) => poisoned.into_inner().replace(name),
        };

        let mut out = format!("{name} sent to {} and everything it started.", self.id);
        if let Some(previous) = previous {
            let _ = write!(
                out,
                " ({previous} was already sent and the process is still alive; \
                 signals do not queue, so if this one is ignored too the \
                 process is in uninterruptible sleep.)"
            );
        }
        let _ = write!(
            out,
            " It is not waited for -- peek or wait on the handle to see it go."
        );
        out
    }

    /// The answer the model sees, for every op that reports on a command.
    ///
    /// One function rather than one per op so a running command and a finished
    /// one are never described in two different vocabularies -- the model has
    /// to recognise "this is still going" from any of them.
    fn report(&self, end: Option<Ending>, lines: usize) -> String {
        let (body, dropped) = match self.output.lock() {
            Ok(ring) => ring.tail(lines),
            Err(poisoned) => poisoned.into_inner().tail(lines),
        };

        let mut out = self.verdict(end);
        if dropped > 0 {
            let _ = write!(
                out,
                "\n[{dropped} earlier lines dropped: output passed {} KiB and \
                 only the tail is kept.]",
                RING_BYTES / 1024
            );
        }
        if body.is_empty() {
            out.push_str("\n(no output)");
        } else {
            out.push('\n');
            out.push_str(&body);
        }
        out
    }

    /// The first line: what happened, in the terms the model must act on.
    fn verdict(&self, end: Option<Ending>) -> String {
        match end {
            Some(Ending {
                signal: Some(signal),
                ran_for,
                ..
            }) => format!(
                "{} was killed by signal {signal} after {:.1}s.",
                self.id,
                ran_for.as_secs_f32()
            ),
            Some(Ending { code, ran_for, .. }) => format!(
                "exit code: {} (after {:.1}s)",
                code.map_or_else(|| "unknown".to_string(), |c| c.to_string()),
                ran_for.as_secs_f32()
            ),
            None => format!(
                "Still running after {:.1}s, as handle {} -- `{}`. It was NOT \
                 killed. Come back with op=peek or op=wait, or op=kill to stop \
                 it.",
                self.started.elapsed().as_secs_f32(),
                self.id,
                self.cmd
            ),
        }
    }
}

/// Which of a child's two pipes a reader thread is draining.
///
/// Both end up in the same ring, interleaved as a terminal would show them. The
/// alternative -- keeping them apart -- reads worse for the one case that
/// matters: a compiler's error on stderr belongs next to the file it was
/// compiling on stdout, not in a separate section.
enum Pipe {
    Out(std::process::ChildStdout),
    Err(std::process::ChildStderr),
}

impl Pipe {
    fn drain_into(self, job: &Job) {
        let mut source: Box<dyn Read> = match self {
            Pipe::Out(out) => Box::new(out),
            Pipe::Err(err) => Box::new(err),
        };

        // Read raw rather than by `lines()`: a command that writes a prompt or
        // a progress bar without a trailing newline would otherwise have its
        // last, most interesting bytes invisible to every peek until it exits.
        let mut chunk = [0u8; 8192];
        let mut pending: Vec<u8> = Vec::new();
        loop {
            match source.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    pending.extend_from_slice(&chunk[..n]);
                    while let Some(at) = pending.iter().position(|b| *b == b'\n') {
                        let line: Vec<u8> = pending.drain(..=at).collect();
                        job.push(&line[..line.len() - 1]);
                    }
                    // A line longer than the whole ring can never be committed
                    // by a newline, so it is committed by length instead.
                    if pending.len() > RING_BYTES {
                        let overlong = std::mem::take(&mut pending);
                        job.push(&overlong);
                    }
                }
            }
        }
        if !pending.is_empty() {
            job.push(&pending);
        }
    }
}

impl Job {
    fn push(&self, line: &[u8]) {
        // Lossy: a command that writes binary to stdout is a mistake, but one
        // that should show up as replacement characters in the log rather than
        // as a silently discarded line.
        let line = String::from_utf8_lossy(line).into_owned();
        match self.output.lock() {
            Ok(mut ring) => ring.push(line),
            Err(poisoned) => poisoned.into_inner().push(line),
        }
    }
}

/// The retained tail of a command's output.
struct Ring {
    lines: VecDeque<String>,
    bytes: usize,
    cap: usize,
    dropped: usize,
}

impl Ring {
    fn new(cap: usize) -> Self {
        Self {
            lines: VecDeque::new(),
            bytes: 0,
            cap,
            dropped: 0,
        }
    }

    fn push(&mut self, line: String) {
        self.bytes += line.len();
        self.lines.push_back(line);
        while self.bytes > self.cap {
            match self.lines.pop_front() {
                Some(gone) => {
                    self.bytes -= gone.len();
                    self.dropped += 1;
                }
                None => break,
            }
        }
    }

    /// The last `lines` lines, and how many were lost before them.
    ///
    /// The count covers both losses -- eviction and this tail -- because from
    /// the model's side they are the same fact: there is output it has not
    /// seen, and it may need to ask differently.
    fn tail(&self, lines: usize) -> (String, usize) {
        let shown = lines.min(self.lines.len());
        let skipped = self.dropped + (self.lines.len() - shown);
        let body = self
            .lines
            .iter()
            .skip(self.lines.len() - shown)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n");
        (body, skipped)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kingdom_core::Workspace;

    async fn bash(root: &std::path::Path, input: Value) -> String {
        let shop = Sandbox::new(Workspace::in_place(root.to_str().unwrap()));
        match Bash.run(input, &shop).await {
            ToolOutcome::Done { output, .. } => output,
            ToolOutcome::Refused { reason } => panic!("refused: {reason}"),
        }
    }

    fn handle_in(output: &str) -> String {
        output
            .split_whitespace()
            .find(|word| word.starts_with("b-"))
            .expect("a still-running command must name its handle")
            .to_string()
    }

    /// The distinction the whole outcome type turns on. A failing test suite is
    /// a successful tool call carrying bad news; reporting it as a refusal
    /// would have the chamber cry error over exactly the result the King asked
    /// for, and send the model off to fix a call that was right.
    #[tokio::test]
    async fn a_command_that_fails_still_ran() {
        let dir = tempfile::tempdir().unwrap();
        let shop = Sandbox::new(Workspace::in_place(dir.path().to_str().unwrap()));

        let outcome = Bash
            .run(json!({"op": "run", "cmd": "echo nope >&2; exit 3"}), &shop)
            .await;

        match outcome {
            ToolOutcome::Done { output, .. } => {
                assert!(output.contains("exit code: 3"), "{output}");
                assert!(output.contains("nope"), "stderr belongs in the output: {output}");
            }
            ToolOutcome::Refused { reason } => panic!("a non-zero exit is not a refusal: {reason}"),
        }
    }

    /// The reason this tool is not a one-shot: the deadline governs the *call*,
    /// and the command survives it to be found again through the registry in a
    /// later deed. Killing at the deadline is what would throw away a build.
    #[tokio::test]
    async fn a_slow_command_survives_the_deadline_and_can_be_killed() {
        let dir = tempfile::tempdir().unwrap();

        let started = bash(
            dir.path(),
            json!({"op": "run", "cmd": "echo working; sleep 30", "wait_seconds": 0}),
        )
        .await;
        assert!(started.contains("Still running"), "{started}");
        let handle = handle_in(&started);

        // Output written before the deadline is readable while it runs -- a
        // handle whose log only appears at exit would be useless for watching.
        let seen = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let peeked = bash(dir.path(), json!({"op": "peek", "handle": handle})).await;
                if peeked.contains("working") {
                    return peeked;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("a running command's output must be visible before it exits");
        assert!(seen.contains("Still running"), "{seen}");

        bash(dir.path(), json!({"op": "kill", "handle": handle})).await;

        let ended = bash(
            dir.path(),
            json!({"op": "wait", "handle": handle, "wait_seconds": 5}),
        )
        .await;
        assert!(
            ended.contains("killed by signal"),
            "the kill must actually reach it: {ended}"
        );
    }

    /// Truncation has to be *said*. A model handed a tail in silence reasons
    /// from a log it believes is whole -- and the line it is missing is the one
    /// that explains the failure.
    #[test]
    fn a_flooded_ring_keeps_the_tail_and_admits_what_it_dropped() {
        let mut ring = Ring::new(32);
        for n in 0..20 {
            ring.push(format!("line {n:02}"));
        }

        let (body, dropped) = ring.tail(usize::MAX);
        assert!(body.contains("line 19"), "the tail is what is kept: {body}");
        assert!(!body.contains("line 00"));
        assert!(dropped > 0, "the loss must be countable, not silent");
    }

    /// A handle nobody minted is a refusal, not an empty log: told the handle
    /// does not exist, the model starts the command again; handed "(no
    /// output)", it concludes the command produced nothing and moves on.
    #[tokio::test]
    async fn an_unknown_handle_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let shop = Sandbox::new(Workspace::in_place(dir.path().to_str().unwrap()));

        let outcome = Bash
            .run(json!({"op": "peek", "handle": "b-nosuch"}), &shop)
            .await;

        assert!(matches!(outcome, ToolOutcome::Refused { .. }), "{outcome:?}");
    }

    /// Process groups earn their keep here: a command's children must die with
    /// it. Signalling the pid alone reparents them to init, still holding
    /// whatever lock they held, with nothing left that knows they exist.
    #[tokio::test]
    async fn killing_a_handle_takes_its_children_with_it() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("child.pid");

        let started = bash(
            dir.path(),
            json!({
                "op": "run",
                "cmd": "sleep 30 & echo $! > child.pid; wait",
                "wait_seconds": 0
            }),
        )
        .await;
        let handle = handle_in(&started);

        let child_pid = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Ok(text) = std::fs::read_to_string(&marker) {
                    if let Ok(pid) = text.trim().parse::<i32>() {
                        return pid;
                    }
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("the grandchild should have recorded its pid");

        bash(dir.path(), json!({"op": "kill", "handle": handle, "signal": "KILL"})).await;
        bash(
            dir.path(),
            json!({"op": "wait", "handle": handle, "wait_seconds": 5}),
        )
        .await;

        let gone = tokio::time::timeout(Duration::from_secs(5), async {
            while still_running(child_pid) {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await;
        assert!(gone.is_ok(), "the grandchild outlived its group's kill");
    }

    /// Liveness by process state rather than `kill(pid, 0)`.
    ///
    /// A killed grandchild whose parent died in the same instant sits as a
    /// zombie until something reaps it, and `kill(pid, 0)` succeeds against a
    /// zombie -- which would make the test above fail over a process that is
    /// very much dead.
    fn still_running(pid: i32) -> bool {
        match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
            // The state letter is the field after the parenthesised comm, which
            // may itself contain spaces -- hence splitting on the last `)`.
            Ok(stat) => stat
                .rsplit_once(')')
                .and_then(|(_, rest)| rest.split_whitespace().next())
                .is_some_and(|state| state != "Z"),
            Err(_) => false,
        }
    }
}
