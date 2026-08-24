//! Kingdom IDE — an IDE for coordinating many agents across many projects.
//!
//! One crate, two targets. Built natively with the `ssr` feature it is an Axum
//! server; built for `wasm32` with the `hydrate` feature it is the browser
//! bundle. The `#[server]` functions in [`api`] span both.

pub mod api;
pub mod app;
pub mod components;

#[cfg(feature = "ssr")]
pub mod llm;
#[cfg(feature = "ssr")]
pub mod mock;
#[cfg(feature = "ssr")]
pub mod scan;
#[cfg(feature = "ssr")]
pub mod store;
#[cfg(feature = "ssr")]
pub mod worktree;

/// Entry point for the wasm bundle. Called by the browser once the script loads.
#[cfg(feature = "hydrate")]
#[wasm_bindgen::prelude::wasm_bindgen]
pub fn hydrate() {
    console_error_panic_hook::set_once();
    leptos::mount::hydrate_body(app::App);
}
