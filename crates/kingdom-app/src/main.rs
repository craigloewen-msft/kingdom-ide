//! The Axum server binary.

// Leptos builds one deeply-nested generic type per `view!` tree, and the
// fixture view has grown nested enough to exceed rustc's default query depth
// while laying out the SSR future. Raising the limit is the compiler's own
// suggested fix and costs nothing at runtime.
#![recursion_limit = "512"]

#[cfg(feature = "ssr")]
#[tokio::main]
async fn main() {
    use axum::Router;
    use kingdom_app::app::{shell, App};
    use leptos::prelude::*;
    use leptos_axum::{generate_route_list, LeptosRoutes};

    // A hidden, early mode: relay traffic between a bind address and a target
    // address, then exit. Never reached by the King -- it is how this same
    // binary is re-spawned, via `nsenter`, *inside* a plan's namespace, to hop
    // a forwarded port from `tap0` to the loopback address the real server
    // actually bound. See `kingdom_app::netns` for why this hop exists at all.
    // Short-circuited ahead of everything else in `main` because none of
    // Axum, Leptos or the model catalogue has any business running in a
    // process whose entire job is one TCP splice.
    {
        let args: Vec<String> = std::env::args().collect();
        if args.get(1).map(String::as_str) == Some("--relay") {
            let (Some(bind), Some(target)) = (args.get(2), args.get(3)) else {
                eprintln!("--relay needs <bind> <target>");
                std::process::exit(2);
            };
            kingdom_app::namespaces::net::run_relay(bind, target).await;
            return;
        }

        // The second hidden mode, and the same idea: this binary re-entered
        // inside a sealed plan -- this time through its *mount* namespace -- to
        // run one tool call on the plan's own filesystem and print the outcome
        // as JSON. It is what makes `read_file` and friends confined by the
        // kernel rather than by a path comparison; see `tools::inside`.
        //
        // Short-circuited here for the same reason as `--relay`: none of Axum,
        // Leptos or the model catalogue belongs in a process whose whole job is
        // to read one file.
        if args.get(1).map(String::as_str) == Some(kingdom_app::tools::inside::FLAG) {
            let Some(request) = args.get(2) else {
                eprintln!(
                    "{} needs one JSON request",
                    kingdom_app::tools::inside::FLAG
                );
                std::process::exit(2);
            };
            // Printed, not logged: stdout *is* the return channel, and the
            // server reads exactly this line back.
            println!("{}", kingdom_app::tools::inside::serve_one(request).await);
            return;
        }
    }

    // Below the hidden modes, so a relay or a confined tool call keeps the
    // lifetime its spawner gave it. From here down this process is the server,
    // and the server must not outlive whoever started it.
    die_when_the_parent_does();

    // Model configuration lives in an optional, gitignored `.kingdom.env` so a
    // credential or provider choice survives restarts without being committed.
    // Real environment variables win, which keeps one-off overrides easy.
    match dotenvy::from_filename(".kingdom.env") {
        Ok(_) => println!("  Read model configuration from .kingdom.env"),
        Err(e) if e.not_found() => {}
        Err(e) => eprintln!("  Could not read .kingdom.env: {e}"),
    }

    let conf = get_configuration(None).expect("failed to read Leptos configuration");
    let leptos_options = conf.leptos_options;
    let addr = leptos_options.site_addr;

    // The first thing done after the address is known, because everything
    // below either costs time, prints, or takes action: `opening_realm` reads
    // a kingdom off disk and announces it, the model catalogue is a network
    // round trip, and `start_housekeeping` *reclaims* the browser profiles it
    // judges abandoned. A second server started against a port the first one
    // already holds would do all of that -- including clearing the running
    // server's browsers out from under it, and telling the reader it had
    // reopened their work -- and only then discover it had nowhere to listen.
    // Holding the socket first means the worst a doomed start can do is
    // explain itself.
    let listener = match tokio::net::TcpListener::bind(&addr).await {
        Ok(listener) => listener,
        Err(e) => {
            eprintln!("{}", bind_failure(&addr, &e));
            std::process::exit(1);
        }
    };

    // Done before anything slow, so a misspelt realm is reported while the
    // reader is still looking at the startup lines. The line it produces is
    // held back to keep the banner in one block below.
    let realm = opening_realm();

    let routes = generate_route_list(App);

    let app = Router::new()
        // Before the Leptos routes, because this is not one: the conversation's
        // push channel is a plain Axum handler and must not be swallowed by
        // the SSR fallback.
        .route(
            kingdom_app::watch::ROUTE,
            axum::routing::get(kingdom_app::watch::upgrade),
        )
        // The rail's channel, on the same terms: one socket per browser rather
        // than one per plan, carrying only what a badge needs. It is what lets
        // a plan waiting on the King say so from a chamber nobody has open.
        .route(
            kingdom_app::watch::KINGDOM_ROUTE,
            axum::routing::get(kingdom_app::watch::upgrade_kingdom),
        )
        // The screencast, for the same reason and on the same terms: pixels
        // rather than plans, but equally not a Leptos route.
        .route(
            kingdom_app::screencast::ROUTE,
            axum::routing::get(kingdom_app::screencast::upgrade),
        )
        // The King's own shell, in the plan's workspace and its network. Ahead
        // of the Leptos routes for the same reason as the sockets above.
        .route(
            kingdom_app::terminal::ROUTE,
            axum::routing::get(kingdom_app::terminal::upgrade),
        )
        // Files a plan's work left behind -- a screenshot the chamber renders
        // inline. Ahead of the Leptos routes because its path lives under
        // `/plan/`, which the SSR fallback would otherwise claim and answer
        // with the app shell instead of the picture.
        .route(
            kingdom_app::artifact::ROUTE,
            axum::routing::get(kingdom_app::artifact::serve),
        )
        // The map's manifest. Not a Leptos route either, and ahead of them for
        // the same reason as the rest: the SSR fallback would answer it with
        // the app shell instead of the geometry.
        .route(
            kingdom_citymap::ROUTE,
            axum::routing::get(kingdom_app::citymap::serve),
        )
        .leptos_routes(&leptos_options, routes, {
            let opts = leptos_options.clone();
            move || shell(opts.clone())
        })
        .fallback(leptos_axum::file_and_error_handler(shell))
        .with_state(leptos_options);

    println!("\n  \u{265a}  Kingdom IDE \u{2014} the throne room awaits at http://{addr}");

    let catalogue = kingdom_app::llm::catalogue::catalogue().await;
    println!(
        "     {} model(s) available, opening on {} \u{2014} {}",
        catalogue.options.len(),
        catalogue.default_id,
        catalogue.detail
    );

    // Said out loud, because the failure this setting invites is doing real
    // work against fake cities without noticing. This is where you find out.
    match realm {
        Some(line) => println!("{line}\n"),
        None => println!(),
    }

    // Clears the browsers a previous server died without closing, and starts
    // the reaper that stops this one accumulating its own. Reported only when
    // it found something: on a clean machine there is nothing to say, and a
    // line saying "reclaimed 0" every boot is noise.
    let reclaimed = kingdom_app::tools::browser::start_housekeeping();
    if reclaimed > 0 {
        println!("     Reclaimed {reclaimed} abandoned browser profile(s) from a previous run\n");
    }

    // Deliberately *not* `with_graceful_shutdown`: that waits for every open
    // connection to close, and the chamber, the rail, the spyglass and the
    // King's shell are all long-lived websockets that never will. It would
    // turn every restart under `cargo leptos watch` into the full ten seconds
    // cargo-leptos waits before it reaches for SIGKILL. Dropping the serve
    // future instead stops accepting at once, which is what a development
    // server wants.
    tokio::select! {
        served = axum::serve(listener, app.into_make_service()) => {
            served.expect("server error");
        }
        _ = told_to_stand_down() => {
            println!("\n  Kingdom IDE is standing down.");
        }
    }

    stop_the_household();
}

/// Resolves when the operating system asks this process to stop.
///
/// SIGHUP is here because of the failure that motivated all of this: the
/// terminal closing. cargo-leptos listens for SIGINT and SIGTERM only, so a
/// closed window kills it outright and leaves the server it spawned running.
/// Kingdom hears that one itself rather than relying on being told.
#[cfg(feature = "ssr")]
async fn told_to_stand_down() {
    use tokio::signal::unix::{signal, SignalKind};

    let mut interrupt = signal(SignalKind::interrupt()).expect("SIGINT handler");
    let mut terminate = signal(SignalKind::terminate()).expect("SIGTERM handler");
    let mut hangup = signal(SignalKind::hangup()).expect("SIGHUP handler");

    tokio::select! {
        _ = interrupt.recv() => {}
        _ = terminate.recv() => {}
        _ = hangup.recv() => {}
    }
}

/// Ask the kernel to kill this process when its parent dies.
///
/// cargo-leptos spawns the server with `setpgid(0, 0)` -- through
/// `tokio-process-tools`, so it can signal the whole tree at once -- which also
/// takes the server *out* of the terminal's foreground process group. Ctrl+C
/// therefore never reaches the server directly; it reaches cargo-leptos, which
/// forwards it. That works, right up until cargo-leptos dies without getting
/// the chance: a closed terminal (SIGHUP, which it does not handle), a
/// `kill -9`, a panic. The server is then reparented to init and holds port
/// 3000 for the rest of the session, and the next `cargo leptos serve` meets
/// the `AddrInUse` message below instead of a throne room.
///
/// `PR_SET_PDEATHSIG` closes that hole in the one place that cannot be
/// bypassed: the kernel sends the signal on parent death regardless of *how*
/// the parent died, so there is no exit path left that can orphan a server.
#[cfg(feature = "ssr")]
fn die_when_the_parent_does() {
    // SAFETY: `getppid` and this `prctl` take no pointers and cannot fail in a
    // way that matters -- an older kernel without `PR_SET_PDEATHSIG` returns
    // an error and leaves the process exactly as it was.
    unsafe {
        let parent = libc::getppid();
        libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM);
        // The parent may have died in the gap between those two calls, in
        // which case the signal just asked for will never come and this
        // process is already the orphan the call was meant to prevent.
        if libc::getppid() != parent {
            std::process::exit(0);
        }
    }
}

/// Take the plans' own processes down on the way out.
///
/// A plan leaves real processes behind it -- `slirp4netns`, the `unshare` that
/// holds its network, a relay, a browser -- and they are all in this process's
/// group, because they inherited it. Signalling the group is what reaches them
/// all without keeping a register of who is who.
///
/// Guarded on actually *leading* that group, which is true exactly when
/// something spawned this server the way cargo-leptos does. Run straight from
/// a shell, the server shares the shell's process group, and signalling that
/// would take down the King's own terminal job along with it.
#[cfg(feature = "ssr")]
fn stop_the_household() {
    // SAFETY: plain process-group calls, no pointers, and nothing here can
    // touch a process outside this server's own group.
    unsafe {
        if libc::getpgrp() != libc::getpid() {
            return;
        }
        // Deafen this process first: the group about to be signalled includes
        // it, and the point is to leave on our own terms rather than be killed
        // partway through doing so.
        libc::signal(libc::SIGTERM, libc::SIG_IGN);
        libc::killpg(0, libc::SIGTERM);
    }
}

/// What to say when the server cannot take the address it was asked for.
///
/// `AddrInUse` earns its own wording because it is the one failure here that a
/// reader causes by hand and can undo by hand: a Kingdom is already running,
/// usually forgotten in another terminal. The `expect` this replaced rendered
/// that as `Os { code: 98, kind: AddrInUse }` under a panic backtrace, which
/// reads as a bug in the server rather than as a second copy of it -- the same
/// confusion `terminal.rs` records from the other side, where a shell that
/// fell through to the host network took `Address already in use` from the
/// King's own server and left the King diagnosing the wrong machine. Naming
/// the likely cause, and the command that confirms it, turns a crash report
/// back into an instruction.
///
/// Every other kind is left to the operating system's own words. They are
/// rare, they are various, and a guess dressed up as an explanation would be
/// worse than the real message: a wrong `LEPTOS_SITE_ADDR` and a privileged
/// port fail differently, and only the error itself knows which happened.
#[cfg(feature = "ssr")]
fn bind_failure(addr: &std::net::SocketAddr, error: &std::io::Error) -> String {
    if error.kind() == std::io::ErrorKind::AddrInUse {
        return format!(
            "\n  Kingdom IDE could not start: something already holds {addr}.\n\n  \
             Almost always that is another Kingdom server, still running in a \
             terminal you have lost track of. Ask who has it:\n\n      \
             ss -tlnp 'sport = :{port}'\n\n  \
             Stop that one and start this again, or point LEPTOS_SITE_ADDR at a \
             free port to run both side by side.\n",
            addr = addr,
            port = addr.port(),
        );
    }

    format!("\n  Kingdom IDE could not listen on {addr}: {error}\n")
}

/// failing that the kingdom the King last chose.
///
/// The server otherwise comes up with no kingdom open, so every restart sends
/// the user back to the folder picker -- and `cargo leptos watch` restarts on
/// every save. `KINGDOM_REALM` makes the rehearsal loop land straight on a
/// populated map; the remembered folder does the same for ordinary use.
///
/// `KINGDOM_REALM` wins outright when it is set. An explicit instruction for
/// *this* run must beat a preference left over from the last one, or a
/// rehearsal session would silently reopen real work.
///
/// A failure is a warning rather than a panic: refusing to boot over a
/// convenience setting would be worse than starting on the picker, which still
/// works and still has the button.
///
/// Returns the banner line to print, so the startup output stays in one block.
#[cfg(feature = "ssr")]
fn opening_realm() -> Option<String> {
    if let Some(name) = std::env::var("KINGDOM_REALM")
        .ok()
        .map(|n| n.trim().to_string())
        .filter(|n| !n.is_empty())
    {
        return match kingdom_app::api::open_fixture(&name) {
            Ok(kingdom) => Some(format!(
                "     Opened the proving ground '{name}' at {}",
                kingdom.root
            )),
            Err(e) => {
                eprintln!("  Could not open the proving ground '{name}': {e}");
                eprintln!("  Starting on the folder picker instead.");
                None
            }
        };
    }

    match kingdom_app::api::open_last_kingdom() {
        Ok(Some(kingdom)) => Some(format!(
            "     Reopened {} at {}",
            kingdom.name, kingdom.root
        )),
        // Nothing recorded: the ordinary first run, and not worth a word.
        Ok(None) => None,
        Err(e) => {
            eprintln!("  Could not reopen the last kingdom: {e}");
            eprintln!("  Starting on the folder picker instead.");
            None
        }
    }
}

#[cfg(not(feature = "ssr"))]
fn main() {
    // The wasm target builds the library, not this binary.
}

#[cfg(all(test, feature = "ssr"))]
mod tests {
    use super::bind_failure;

    /// The failure a reader actually hits, reproduced the way they hit it.
    ///
    /// A real second bind against a really-held port, rather than a synthesised
    /// `ErrorKind`: the point of the test is that the kind the operating system
    /// reports for this situation is the kind the match arm looks for, and a
    /// hand-made error would assert that agreement instead of checking it.
    #[test]
    fn a_port_someone_else_holds_names_the_port_and_what_to_run() {
        let held = std::net::TcpListener::bind("127.0.0.1:0").expect("a free port to hold");
        let addr = held.local_addr().expect("the port it settled on");

        let error = std::net::TcpListener::bind(addr).expect_err("the second bind to be refused");
        let said = bind_failure(&addr, &error);

        assert!(
            said.contains(&addr.to_string()),
            "names the address: {said}"
        );
        assert!(
            said.contains(&format!("sport = :{}", addr.port())),
            "names a command that finds the holder: {said}"
        );
        assert!(
            said.contains("LEPTOS_SITE_ADDR"),
            "offers the other way out: {said}"
        );
        assert!(
            !said.contains("AddrInUse"),
            "and does not fall back to the debug form: {said}"
        );
    }

    /// Anything else keeps the system's own words.
    ///
    /// A privileged port and a misspelt `LEPTOS_SITE_ADDR` both arrive here,
    /// and they need different answers -- so this says what happened and gets
    /// out of the way rather than guessing which one it was.
    #[test]
    fn any_other_failure_is_reported_in_the_systems_own_words() {
        let addr: std::net::SocketAddr = "127.0.0.1:3000".parse().expect("a literal address");
        let error = std::io::Error::new(std::io::ErrorKind::PermissionDenied, "permission denied");

        let said = bind_failure(&addr, &error);

        assert!(
            said.contains("permission denied"),
            "keeps the cause: {said}"
        );
        assert!(
            !said.contains("ss -tlnp"),
            "and does not send the reader hunting a holder that does not exist: {said}"
        );
    }
}
