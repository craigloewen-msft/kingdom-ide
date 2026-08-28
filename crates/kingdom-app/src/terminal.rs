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
//!   to the browser    raw pty output, as binary frames
//!   from the browser  [0x00][utf-8 keystrokes]      what was typed
//!                     [0x01][u16 cols][u16 rows]    the window was resized
//! ```
//!
//! # Lifetime
//!
//! One shell per socket, and it dies with the socket. Closing the panel closes
//! the shell -- there is no reattach, because a terminal the King cannot see is
//! a process nobody is supervising, which is the thing this product exists to
//! prevent rather than to add more of.

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::Path;
use axum::response::Response;
use std::io::{Read as _, Write as _};

/// The path the terminal connects to. One shell per socket.
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

pub async fn upgrade(ws: WebSocketUpgrade, Path(id): Path<String>) -> Response {
    ws.on_upgrade(move |socket| run(socket, id))
}

async fn run(mut socket: WebSocket, plan: String) {
    let plan_id = kingdom_core::PlanId::new(plan);

    let Some(plan) = crate::api::snapshot(&plan_id) else {
        let _ = socket
            .send(Message::Text("That plan is not here.".into()))
            .await;
        return;
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
        if let Err(e) = crate::netns::ensure(&plan_id).await {
            let _ = socket
                .send(Message::Text(
                    format!(
                        "This plan has a network of its own, but it could not be \
                         opened -- so no shell was started. A shell on the \
                         machine's network would be the wrong answer rather than \
                         a lesser one.\r\n\r\n{e}\r\n"
                    )
                    .into(),
                ))
                .await;
            return;
        }
        crate::netns::watch(&plan_id);
    }

    // The well, for the same reason and on the same terms as the namespace
    // above: the King's shell must be able to reach this city's database, and a
    // shell that silently could not would send him hunting for a fault in the
    // project. **Opening a shell does not raise one** -- that is not a reason to
    // start a database -- so this waits for any pass in flight and then checks.
    // Nothing happens for a city that declares no services.
    if let Some(city_root) = crate::api::city_root_of(&plan_id) {
        if let Err(e) = crate::services::require(&plan_id, &city_root).await {
            let _ = socket
                .send(Message::Text(
                    format!(
                        "This project declares shared services, and they could \
                         not be raised -- so no shell was started. A shell that \
                         cannot reach the database would send you looking for \
                         the wrong fault.\r\n\r\n{e}\r\n"
                    )
                    .into(),
                ))
                .await;
            return;
        }
    }

    // The same prefix every tool gets, and empty for a shared-network plan --
    // so this is the King's ordinary shell unless he asked for isolation.
    let enter = crate::netns::enter_prefix(&plan_id);

    // Belt and braces on the guarantee above. An isolated plan whose prefix came
    // back empty would silently be a host shell, and that is the one outcome
    // worth refusing outright rather than degrading to.
    if plan.network.is_isolated() && enter.is_empty() {
        let _ = socket
            .send(Message::Text(
                "This plan's network could not be entered, so no shell was \
                 started.\r\n"
                    .into(),
            ))
            .await;
        return;
    }

    let pty = match portable_pty::native_pty_system().openpty(portable_pty::PtySize {
        rows: INITIAL_ROWS,
        cols: INITIAL_COLS,
        pixel_width: 0,
        pixel_height: 0,
    }) {
        Ok(pair) => pair,
        Err(e) => {
            let _ = socket
                .send(Message::Text(
                    format!("No terminal could be opened: {e}").into(),
                ))
                .await;
            return;
        }
    };

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

    let mut child = match pty.slave.spawn_command(command) {
        Ok(child) => child,
        Err(e) => {
            let _ = socket
                .send(Message::Text(
                    format!("No shell could be started: {e}").into(),
                ))
                .await;
            return;
        }
    };

    // The slave is the child's end. Dropping it here is what makes the reader
    // below see EOF when the shell exits -- held open, the read would block
    // forever on a terminal nobody is attached to.
    drop(pty.slave);

    let mut reader = match pty.master.try_clone_reader() {
        Ok(reader) => reader,
        Err(e) => {
            let _ = socket
                .send(Message::Text(
                    format!("The terminal could not be read: {e}").into(),
                ))
                .await;
            let _ = child.kill();
            return;
        }
    };
    let mut writer = match pty.master.take_writer() {
        Ok(writer) => writer,
        Err(e) => {
            let _ = socket
                .send(Message::Text(
                    format!("The terminal could not be written: {e}").into(),
                ))
                .await;
            let _ = child.kill();
            return;
        }
    };

    // A pty read blocks, and this crate's tokio has no async file I/O for one,
    // so the reader lives on a blocking thread and hands bytes over a channel
    // -- the same shape `bash.rs` uses for its output pumps.
    let (output, mut from_shell) = tokio::sync::mpsc::channel::<Vec<u8>>(64);
    std::thread::spawn(move || {
        let mut buffer = [0u8; 8192];
        loop {
            match reader.read(&mut buffer) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if output.blocking_send(buffer[..n].to_vec()).is_err() {
                        break;
                    }
                }
            }
        }
    });

    loop {
        tokio::select! {
            chunk = from_shell.recv() => match chunk {
                Some(bytes) => {
                    if socket.send(Message::Binary(bytes.into())).await.is_err() {
                        break;
                    }
                }
                // The shell exited. Say so in words rather than closing
                // silently, so the panel can tell "you typed exit" from "the
                // connection dropped".
                None => {
                    let _ = socket.send(Message::Text("\r\n[the shell has exited]\r\n".into())).await;
                    break;
                }
            },

            from_browser = socket.recv() => match from_browser {
                Some(Ok(Message::Binary(bytes))) => {
                    match bytes.split_first() {
                        Some((&TAG_INPUT, typed)) => {
                            if writer.write_all(typed).is_err() || writer.flush().is_err() {
                                break;
                            }
                        }
                        Some((&TAG_RESIZE, dimensions)) if dimensions.len() >= 4 => {
                            let cols = u16::from_be_bytes([dimensions[0], dimensions[1]]);
                            let rows = u16::from_be_bytes([dimensions[2], dimensions[3]]);
                            // Best effort: a refused resize leaves the shell at
                            // its old size, which is untidy but not broken.
                            let _ = pty.master.resize(portable_pty::PtySize {
                                rows: rows.max(1),
                                cols: cols.max(1),
                                pixel_width: 0,
                                pixel_height: 0,
                            });
                        }
                        _ => {}
                    }
                }
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                _ => continue,
            },
        }
    }

    // The panel is gone, so the shell goes. See the module docs: an unwatched
    // shell is an unsupervised process, which is the thing being prevented.
    let _ = child.kill();
    let _ = child.wait();
}
