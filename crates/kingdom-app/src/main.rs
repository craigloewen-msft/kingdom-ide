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

    // Model configuration lives in an optional, gitignored `.kingdom.env` so a
    // credential or provider choice survives restarts without being committed.
    // Real environment variables win, which keeps one-off overrides easy.
    match dotenvy::from_filename(".kingdom.env") {
        Ok(_) => println!("  Read model configuration from .kingdom.env"),
        Err(e) if e.not_found() => {}
        Err(e) => eprintln!("  Could not read .kingdom.env: {e}"),
    }

    // Done before anything slow, so a misspelt realm is reported while the
    // reader is still looking at the startup lines. The line it produces is
    // held back to keep the banner in one block below.
    let realm = opening_realm();

    let conf = get_configuration(None).expect("failed to read Leptos configuration");
    let leptos_options = conf.leptos_options;
    let addr = leptos_options.site_addr;
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

    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .expect("failed to bind");
    axum::serve(listener, app.into_make_service())
        .await
        .expect("server error");
}

/// Opens whatever the server should come up on: the named proving ground, or
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
