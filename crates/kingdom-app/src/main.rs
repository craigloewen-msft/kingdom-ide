//! The Axum server binary.

// Leptos builds one deeply-nested generic type per `view!` tree, and the realm
// view has grown nested enough to exceed rustc's default query depth while
// laying out the SSR future. Raising the limit is the compiler's own suggested
// fix and costs nothing at runtime.
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

    let conf = get_configuration(None).expect("failed to read Leptos configuration");
    let leptos_options = conf.leptos_options;
    let addr = leptos_options.site_addr;
    let routes = generate_route_list(App);

    let app = Router::new()
        .leptos_routes(&leptos_options, routes, {
            let opts = leptos_options.clone();
            move || shell(opts.clone())
        })
        .fallback(leptos_axum::file_and_error_handler(shell))
        .with_state(leptos_options);

    println!("\n  \u{265a}  Kingdom IDE \u{2014} the throne room awaits at http://{addr}");

    let catalogue = kingdom_app::llm::catalogue::catalogue().await;
    println!(
        "     {} model(s) available, opening on {} \u{2014} {}\n",
        catalogue.options.len(),
        catalogue.default_id,
        catalogue.detail
    );

    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .expect("failed to bind");
    axum::serve(listener, app.into_make_service())
        .await
        .expect("server error");
}

#[cfg(not(feature = "ssr"))]
fn main() {
    // The wasm target builds the library, not this binary.
}
