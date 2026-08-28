//! The King's own terminal, in a plan's workspace and a plan's network.
//!
//! `/watch/plan/{id}/terminal`. A sibling of [`crate::screencast`] -- that one
//! carries pixels one way, this one carries bytes both ways.
//!
//! # Why this is interactive when the spyglass is not
//!
//! `screencast.rs` refuses input permanently, and says why: a page driven by
//! both the model and the user, with nothing arbitrating, is the collision this
//! product exists to surface. That argument does not apply here. This is not a
//! shared surface being fought over -- it is the King's *own* shell, which he
//! opened, in a workspace he owns. Nothing else is typing into it.
//!
//! What it is *for* is the other half of network isolation. Once a plan has its
//! own namespace, the King can no longer `cd` to the worktree in his own
//! terminal and reach the agent's server -- his shell is in a different
//! network. This is the door into that namespace.
//!
//! # The wire
//!
//! Deliberately trivial, so this file and `components/terminal_view.rs` can be
//! checked against each other by eye:
//!
//! ```text
//!   to the browser    raw pty output, as binary frames -- and a text frame
//!                     for a final word in the King's own English ("the shell
//!                     has exited", or why one could not be started). Text is
//!                     always final, which is how the panel knows not to add
//!                     "disconnected" underneath it.
//!   from the browser  [0x00][utf-8 keystrokes]      what was typed
//!                     [0x01][u16 cols][u16 rows]    the window was resized
//! ```
//!
//! # Lifetime
//!
//! One shell per **plan**, not one per socket. The socket attaches to it and
//! detaches from it; the shell outlives both.
//!
//! This used to be the other way round, and the argument for that was
//! supervision: a terminal the King cannot see is a process nobody is watching,
//! which is the collision this product exists to surface. The argument was
//! sound and the behaviour was still wrong, because the panel is destroyed by
//! far more than the close button. It lives inside a `Show` on one aside slot,
//! so opening the spyglass, a diff or a source file disposes it -- and a
//! half-finished `cargo test` died because the King looked at a diff. Killing
//! his build for glancing away is a worse failure than an unattended shell.
//!
//! The supervision concern is answered rather than dropped: this is the King's
//! *own* process, started by hand, and it still ends with the plan --
//! [`shutdown`] is called when a plan is merged or archived, beside
//! [`crate::netns::shutdown`], and the panel's close button ends it deliberately
//! through [`crate::api::end_terminal`]. Looking away is not an act; closing is.
//!
//! Output produced while nobody is attached is not lost: it accumulates in a
//! bounded scrollback which is replayed to the next socket that attaches.

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::Path;
use axum::response::Response;
use kingdom_core::PlanId;
use std::collections::{HashMap, VecDeque};
use std::io::{Read as _, Write as _};
use std::sync::{Arc, Mutex, OnceLock};
use tokio::sync::broadcast;

/// The path the terminal connects to. One shell per plan; a socket attaches.
///
/// Re-exported from [`crate::terminal_route`] rather than declared here,
/// because the browser needs the same string and this module is server-only. A
/// second copy would be free to disagree, and the symptom would be a panel that
/// connects to nothing with no error to explain it.
pub use crate::terminal_route::ROUTE;

/// Sent by the browser: these bytes were typed.
const TAG_INPUT: u8 = 0x00;
/// Sent by the browser: the window is now this many columns and rows.
const TAG_RESIZE: u8 = 0x01;

/// The size a shell opens at, before the browser has measured itself.
///
/// Replaced within a frame or two by a real measurement; it exists so the shell
/// never starts at 0x0, which makes `less` and `vim` behave strangely.
const INITIAL_COLS: u16 = 80;
const INITIAL_ROWS: u16 = 24;

/// How many chunks a slow panel may fall behind before it is told it missed
/// some. Generous, because the cost of a lag notice is a line of noise in the
/// King's terminal.
const BROADCAST_BACKLOG: usize = 1024;

pub async fn upgrade(ws: WebSocketUpgrade, Path(id): Path<String>) -> Response {
    ws.on_upgrade(move |socket| run(socket, id))
}

/// How much output is kept for a panel that is not looking.
///
/// Enough that a `cargo build` the King glanced away from is still readable
/// when he comes back, and small enough that a runaway `yes` costs a quarter of
/// a megabyte rather than the machine.
const SCROLLBACK_BYTES: usize = 256 * 1024;

/// What an attached socket hears from the shell.
#[derive(Clone)]
enum Frame {
    /// Raw pty bytes.
    Output(Vec<u8>),
    /// The shell exited. Sent once, by the reader thread, so a panel can tell
    /// "you typed exit" from "the connection dropped".
    Exited,
}

/// Output kept for whoever attaches next.
///
/// Capped by total bytes, dropping **whole oldest chunks**. It never slices
/// one: half an ANSI escape or half a UTF-8 sequence is worse than a missing
/// line, because it corrupts everything the emulator draws after it.
#[derive(Default)]
struct Scrollback {
    chunks: VecDeque<Vec<u8>>,
    bytes: usize,
}

impl Scrollback {
    fn push(&mut self, chunk: Vec<u8>) {
        self.bytes += chunk.len();
        self.chunks.push_back(chunk);
        // `> 1` so a single chunk larger than the cap is kept whole rather than
        // emptying the buffer it is the only member of.
        while self.bytes > SCROLLBACK_BYTES && self.chunks.len() > 1 {
            match self.chunks.pop_front() {
                Some(oldest) => self.bytes -= oldest.len(),
                None => break,
            }
        }
    }

    /// Everything kept, as one buffer, for replay on attach.
    fn replay(&self) -> Vec<u8> {
        let mut all = Vec::with_capacity(self.bytes);
        for chunk in &self.chunks {
            all.extend_from_slice(chunk);
        }
        all
    }
}

/// One shell, alive between sockets.
struct Session {
    /// The master end, kept for `resize`.
    master: Mutex<Box<dyn portable_pty::MasterPty + Send>>,
    writer: Mutex<Box<dyn std::io::Write + Send>>,
    child: Mutex<Box<dyn portable_pty::Child + Send + Sync>>,
    live: broadcast::Sender<Frame>,
    scrollback: Mutex<Scrollback>,
}

impl Session {
    /// Ends the shell. Idempotent: killing one that has already exited is a
    /// no-op, which is what [`shutdown`] wants.
    fn kill(&self) {
        if let Ok(mut child) = self.child.lock() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// Every plan's shell. The same shape as `netns::NAMESPACES` and
/// `services::SERVICES`, and held for the same reason: it lives in the process
/// rather than on disk, so a restarted server starts a fresh one.
static SESSIONS: OnceLock<Mutex<HashMap<PlanId, Arc<Session>>>> = OnceLock::new();

fn sessions() -> &'static Mutex<HashMap<PlanId, Arc<Session>>> {
    SESSIONS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Ends a plan's shell and forgets it.
///
/// Called when the King closes the panel deliberately, and when a plan is
/// merged or archived -- there beside [`crate::netns::shutdown`] and *before*
/// it, because the shell holds the worktree as its working directory and `git
/// worktree remove` must not be fighting it.
pub fn shutdown(plan: &PlanId) {
    let session = sessions().lock().ok().and_then(|mut s| s.remove(plan));
    if let Some(session) = session {
        session.kill();
    }
}

async fn run(mut socket: WebSocket, plan: String) {
    let plan_id = PlanId::new(plan);

    // The shell this plan already has, if any. Looked for before any of the
    // work in `start`, because all of it -- raising a namespace, raising a
    // well, opening a pty -- was already done when that shell was started.
    let existing = sessions()
        .lock()
        .ok()
        .and_then(|registry| registry.get(&plan_id).cloned());

    let session = match existing {
        Some(session) => session,
        None => match start(&plan_id).await {
            Ok(session) => session,
            // Every refusal is reported in words down the socket, which the
            // panel writes into the terminal. None of them leaves a registry
            // entry behind, so the next open tries again from the start.
            Err(refusal) => {
                let _ = socket.send(Message::Text(refusal.into())).await;
                return;
            }
        },
    };

    attach(socket, session).await;
}

/// Opens a plan's shell, or says in the King's own words why it could not.
async fn start(plan_id: &PlanId) -> Result<Arc<Session>, String> {
    let Some(plan) = crate::api::snapshot(plan_id) else {
        return Err("That plan is not here.".to_string());
    };

    let cwd = std::path::PathBuf::from(&plan.workspace.path);

    // Raise this plan's network if it is meant to have one and does not yet --
    // the ordinary case after a server restart, because a namespace lives in a
    // process rather than on disk.
    //
    // **This must never fall through to the host network.** Without it,
    // `enter_prefix` below finds nothing in the registry, returns empty, and
    // hands the King a shell on his *own* network while this panel's header
    // says "in this plan's network". Measured rather than imagined: that shell
    // tried to bind :3000 and took `Address already in use` from the King's own
    // server. A refusal he can read beats a lie he cannot see.
    if plan.network.is_isolated() {
        if let Err(e) = crate::netns::ensure(plan_id).await {
            return Err(format!(
                "This plan has a network of its own, but it could not be \
                 opened -- so no shell was started. A shell on the \
                 machine's network would be the wrong answer rather than \
                 a lesser one.\r\n\r\n{e}\r\n"
            ));
        }
        crate::netns::watch(plan_id);
    }

    // The well, for the same reason and on the same terms as the namespace
    // above: the King's shell must be able to reach this city's database, and a
    // shell that silently could not would send him hunting for a fault in the
    // project. **Opening a shell does not raise one** -- that is not a reason to
    // start a database -- so this waits for any pass in flight and then checks,
    // and puts the well on this plan's loopback. Nothing happens for a city
    // that declares no services.
    if let Some(city_root) = crate::api::city_root_of(plan_id) {
        if let Err(e) = crate::services::require(plan_id, &city_root).await {
            return Err(format!(
                "This project declares shared services, and they could \
                 not be raised -- so no shell was started. A shell that \
                 cannot reach the database would send you looking for \
                 the wrong fault.\r\n\r\n{e}\r\n"
            ));
        }
    }

    // The same prefix every tool gets, and empty for a shared-network plan --
    // so this is the King's ordinary shell unless he asked for isolation.
    let enter = crate::netns::enter_prefix(plan_id);

    // Belt and braces on the guarantee above. An isolated plan whose prefix came
    // back empty would silently be a host shell, and that is the one outcome
    // worth refusing outright rather than degrading to.
    if plan.network.is_isolated() && enter.is_empty() {
        return Err(
            "This plan's network could not be entered, so no shell was started.\r\n".to_string(),
        );
    }

    let pty = portable_pty::native_pty_system()
        .openpty(portable_pty::PtySize {
            rows: INITIAL_ROWS,
            cols: INITIAL_COLS,
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(|e| format!("No terminal could be opened: {e}"))?;

    // `bash -l`, so the King's own toolchain managers are on PATH -- the same
    // reasoning `tmux.rs` gives for its panes. A shell that cannot find the
    // `cargo` he uses every day is a shell he will not use twice.
    let mut command = match enter.split_first() {
        Some((program, rest)) => {
            let mut command = portable_pty::CommandBuilder::new(program);
            command.args(rest);
            command.arg("bash");
            command.arg("-l");
            command
        }
        None => {
            let mut command = portable_pty::CommandBuilder::new("bash");
            command.arg("-l");
            command
        }
    };
    command.cwd(&cwd);
    // What a terminal emulator this shell can expect. xterm.js is faithful
    // enough for the colour and cursor handling `xterm-256color` implies.
    command.env("TERM", "xterm-256color");
    for (key, value) in
        crate::tools::child_environment(&crate::tools::Sandbox::new(plan.workspace.clone()))
    {
        command.env(key, value);
    }

    let mut child = pty
        .slave
        .spawn_command(command)
        .map_err(|e| format!("No shell could be started: {e}"))?;

    // The slave is the child's end. Dropping it here is what makes the reader
    // below see EOF when the shell exits -- held open, the read would block
    // forever on a terminal nobody is attached to.
    drop(pty.slave);

    let mut reader = match pty.master.try_clone_reader() {
        Ok(reader) => reader,
        Err(e) => {
            let _ = child.kill();
            return Err(format!("The terminal could not be read: {e}"));
        }
    };
    let writer = match pty.master.take_writer() {
        Ok(writer) => writer,
        Err(e) => {
            let _ = child.kill();
            return Err(format!("The terminal could not be written: {e}"));
        }
    };

    let (live, _) = broadcast::channel::<Frame>(BROADCAST_BACKLOG);
    let session = Arc::new(Session {
        master: Mutex::new(pty.master),
        writer: Mutex::new(writer),
        child: Mutex::new(child),
        live,
        scrollback: Mutex::new(Scrollback::default()),
    });

    // A pty read blocks, and this crate's tokio has no async file I/O for one,
    // so the reader lives on a blocking thread -- the same shape `bash.rs` uses
    // for its output pumps. It belongs to the *session* rather than to a
    // socket, which is what keeps output flowing while nobody is attached.
    let pump = Arc::clone(&session);
    let pumped_plan = plan_id.clone();
    std::thread::spawn(move || {
        let mut buffer = [0u8; 8192];
        loop {
            match reader.read(&mut buffer) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    let chunk = buffer[..n].to_vec();
                    // Recorded and broadcast **under the one lock**, which
                    // `attach` also takes to subscribe. That is what makes a
                    // socket arriving at this instant either find the chunk in
                    // its replay or hear it live -- never both, never neither.
                    if let Ok(mut scrollback) = pump.scrollback.lock() {
                        scrollback.push(chunk.clone());
                        let _ = pump.live.send(Frame::Output(chunk));
                    }
                }
            }
        }

        // The shell exited of its own accord -- the King typed `exit`, or it
        // died. Forget it so the next open starts a fresh one rather than
        // attaching to a corpse, but only if it is still *this* session in the
        // registry: a `shutdown` may already have removed it.
        if let Ok(mut registry) = sessions().lock() {
            if registry
                .get(&pumped_plan)
                .is_some_and(|current| Arc::ptr_eq(current, &pump))
            {
                registry.remove(&pumped_plan);
            }
        }
        let _ = pump.live.send(Frame::Exited);
        pump.kill();
    });

    if let Ok(mut registry) = sessions().lock() {
        registry.insert(plan_id.clone(), Arc::clone(&session));
    }

    Ok(session)
}

/// Carries one socket's traffic to and from a shell that outlives it.
async fn attach(mut socket: WebSocket, session: Arc<Session>) {
    // Subscribed while the scrollback is held, for the reason the pump's own
    // comment gives: it is what makes the handover neither drop a chunk nor
    // repeat one.
    let (replay, mut live) = {
        let Ok(scrollback) = session.scrollback.lock() else {
            let _ = socket
                .send(Message::Text("That terminal is no longer readable.".into()))
                .await;
            return;
        };
        (scrollback.replay(), session.live.subscribe())
    };

    // What happened while nobody was looking, in one frame. xterm.js replays
    // the ANSI as faithfully as it drew it the first time, so the panel comes
    // back to the screen it left plus whatever the shell has done since.
    if !replay.is_empty() && socket.send(Message::Binary(replay.into())).await.is_err() {
        return;
    }

    loop {
        tokio::select! {
            frame = live.recv() => match frame {
                Ok(Frame::Output(bytes)) => {
                    if socket.send(Message::Binary(bytes.into())).await.is_err() {
                        break;
                    }
                }
                Ok(Frame::Exited) | Err(broadcast::error::RecvError::Closed) => {
                    let _ = socket.send(Message::Text("\r\n[the shell has exited]\r\n".into())).await;
                    break;
                }
                // This panel fell behind a flood. Sent as bytes rather than
                // text, deliberately: every *text* frame this module sends is
                // a final word, which is what lets the panel know not to add
                // "[disconnected]" underneath one. This is not final.
                Err(broadcast::error::RecvError::Lagged(_)) => {
                    if socket.send(Message::Binary(b"\r\n[output was dropped]\r\n".to_vec().into())).await.is_err() {
                        break;
                    }
                }
            },

            from_browser = socket.recv() => match from_browser {
                Some(Ok(Message::Binary(bytes))) => {
                    match bytes.split_first() {
                        Some((&TAG_INPUT, typed)) => {
                            let Ok(mut writer) = session.writer.lock() else { break };
                            if writer.write_all(typed).is_err() || writer.flush().is_err() {
                                break;
                            }
                        }
                        Some((&TAG_RESIZE, dimensions)) if dimensions.len() >= 4 => {
                            let cols = u16::from_be_bytes([dimensions[0], dimensions[1]]);
                            let rows = u16::from_be_bytes([dimensions[2], dimensions[3]]);
                            // Best effort: a refused resize leaves the shell at
                            // its old size, which is untidy but not broken.
                            if let Ok(master) = session.master.lock() {
                                let _ = master.resize(portable_pty::PtySize {
                                    rows: rows.max(1),
                                    cols: cols.max(1),
                                    pixel_width: 0,
                                    pixel_height: 0,
                                });
                            }
                        }
                        _ => {}
                    }
                }
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                _ => continue,
            },
        }
    }

    // Deliberately nothing here. The socket is gone; the shell is not. See the
    // module docs: looking away is not an act, and `shutdown` is what ends a
    // shell.
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scrollback_keeps_what_fits() {
        let mut scrollback = Scrollback::default();
        scrollback.push(b"one ".to_vec());
        scrollback.push(b"two".to_vec());
        assert_eq!(scrollback.replay(), b"one two");
    }

    #[test]
    fn scrollback_drops_whole_oldest_chunks() {
        let mut scrollback = Scrollback::default();
        // Three chunks of half the cap each: the first must go entirely, and
        // what survives must still begin on a chunk boundary rather than in the
        // middle of one.
        scrollback.push(vec![b'a'; SCROLLBACK_BYTES / 2]);
        scrollback.push(vec![b'b'; SCROLLBACK_BYTES / 2]);
        scrollback.push(vec![b'c'; SCROLLBACK_BYTES / 2]);

        let kept = scrollback.replay();
        assert_eq!(kept.len(), SCROLLBACK_BYTES);
        assert_eq!(kept[0], b'b');
        assert_eq!(kept[kept.len() - 1], b'c');
    }

    #[test]
    fn scrollback_keeps_an_oversized_chunk_whole() {
        let mut scrollback = Scrollback::default();
        // Bigger than the cap on its own. Kept entire rather than sliced: a cut
        // chunk could sever an escape sequence, which corrupts every line the
        // emulator draws afterwards.
        let huge = vec![b'x'; SCROLLBACK_BYTES * 2];
        scrollback.push(huge.clone());
        assert_eq!(scrollback.replay().len(), huge.len());
    }
}
