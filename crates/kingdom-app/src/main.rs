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
        // The screencast, for the same reason and on the same terms: pixels
        // rather than plans, but equally not a Leptos route.
        .route(
            kingdom_app::screencast::ROUTE,
            axum::routing::get(kingdom_app::screencast::upgrade),
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

    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .expect("failed to bind");
    axum::serve(listener, app.into_make_service())
        .await
        .expect("server error");
}

/// Opens the proving ground named by `KINGDOM_REALM`, if one is named.
///
/// The server otherwise comes up with no kingdom open, so every restart sends
/// the user back to the folder picker -- and `cargo leptos watch` restarts on
/// every save. Setting this makes the rehearsal loop land straight on a
/// populated map.
///
/// A failure is a warning rather than a panic: refusing to boot over a
/// convenience setting would be worse than starting on the picker, which still
/// works and still has the button.
///
/// Returns the banner line to print, so the startup output stays in one block.
#[cfg(feature = "ssr")]
fn opening_realm() -> Option<String> {
    let name = std::env::var("KINGDOM_REALM")
        .ok()
        .map(|n| n.trim().to_string())
        .filter(|n| !n.is_empty())?;

    match kingdom_app::api::open_fixture(&name) {
        Ok(kingdom) => Some(format!(
            "     Opened the proving ground '{name}' at {}",
            kingdom.root
        )),
        Err(e) => {
            eprintln!("  Could not open the proving ground '{name}': {e}");
            eprintln!("  Starting on the folder picker instead.");
            None
        }
    }
}

#[cfg(not(feature = "ssr"))]
fn main() {
    // The wasm target builds the library, not this binary.
}
