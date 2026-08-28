//! Kingdom IDE — an IDE for coordinating many agents across many projects.
//!
//! One crate, two targets. Built natively with the `ssr` feature it is an Axum
//! server; built for `wasm32` with the `hydrate` feature it is the browser
//! bundle. The `#[server]` functions in [`api`] span both.

pub mod api;
pub mod app;
pub mod components;

// Taking turns with the model. Carved out of `api`, which was holding both the
// `#[server]` wire and the agent loop; see the module's own note on the seam
// between the two.
#[cfg(feature = "ssr")]
pub mod turn;

// Both targets: the browser builds the link, the server answers it. See the
// module's own note on what inside it is server-only.
pub mod artifact;

#[cfg(feature = "ssr")]
pub mod citymap;
#[cfg(feature = "ssr")]
pub mod edit;
#[cfg(feature = "ssr")]
pub mod events;
// Syntax colour for the source panel. Server-only on purpose: tokenising before
// the lines go over the wire is what keeps a regex engine and 213 syntax
// definitions out of the wasm bundle. See the module's own note.
#[cfg(feature = "ssr")]
pub mod highlight;
#[cfg(feature = "ssr")]
pub mod llm;
#[cfg(feature = "ssr")]
pub mod mock;
// A network of a plan's own. Server-only and Linux-only: it spawns `unshare`
// and `slirp4netns`, neither of which has any meaning in a browser.
#[cfg(feature = "ssr")]
pub mod namespaces;
#[cfg(feature = "ssr")]
pub mod profile;
#[cfg(feature = "ssr")]
pub mod review;
#[cfg(feature = "ssr")]
pub mod scan;
#[cfg(feature = "ssr")]
pub mod screencast;
// The well: containers a whole city shares. Server-only, for the same reason
// `namespaces` is -- it spawns `docker`, which has no meaning in a browser.
#[cfg(feature = "ssr")]
pub mod services;
#[cfg(feature = "ssr")]
pub mod skills;
#[cfg(feature = "ssr")]
pub mod store;
#[cfg(feature = "ssr")]
pub mod tools;
#[cfg(feature = "ssr")]
pub mod turns;
// Both targets: the browser builds the socket's address from `terminal::ROUTE`,
// the server answers it -- the same split as `watch`, for the same reason.
pub mod terminal_route {
    /// The path the terminal socket lives at. One shell per socket.
    ///
    /// Here rather than in `terminal` because that module is server-only (it
    /// forks a shell) while the browser still has to know where to connect.
    pub const ROUTE: &str = "/watch/plan/{id}/terminal";
}
#[cfg(feature = "ssr")]
pub mod terminal;
// Both targets: the browser builds the socket's address from `watch::ROUTE`,
// the server answers it. See the module's own note on why the constants cross
// and the handlers do not.
pub mod watch;
#[cfg(feature = "ssr")]
pub mod worktree;

/// Entry point for the wasm bundle. Called by the browser once the script loads.
#[cfg(feature = "hydrate")]
#[wasm_bindgen::prelude::wasm_bindgen]
pub fn hydrate() {
    console_error_panic_hook::set_once();
    leptos::mount::hydrate_body(app::App);
}
