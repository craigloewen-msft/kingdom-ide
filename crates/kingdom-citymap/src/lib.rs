#![warn(missing_docs)]
//! The kingdom map: every project on the machine, drawn as an isometric island
//! of towns.
//!
//! This crate replaced a hand-written SVG map (`kingdom_core::{layout, terrain,
//! skyline}` and `kingdom_app::components::map`, all deleted). It is **Repo
//! City** — <https://github.com/craigloewen-msft/repo-city-visualizer> by Craig
//! Loewen, MIT — copied in wholesale at commit `449f090` rather than depended
//! on, so that there is one project to maintain rather than two. See `LICENSE`
//! beside this file.
//!
//! # The shape of it
//!
//! Three modules, and which target each compiles to is the whole design:
//!
//! | Module | Target | What it is |
//! |---|---|---|
//! | [`map`] | both | the manifest: plain serialisable world-space geometry |
//! | [`build`] | `ssr` | scanning a kingdom on disk and laying it out |
//! | [`engine`] | `hydrate` | drawing a manifest with Bevy |
//!
//! The split is load-bearing in both directions. [`build`] uses `std::fs` and
//! `ignore`, neither of which belongs in a wasm bundle; [`engine`] pulls in
//! Bevy, which must never reach the Axum binary. [`map`] is the only part on
//! both, which is exactly why the manifest exists: it is the seam the two
//! halves meet at, and `kingdom-app` is what carries it between them over
//! [`crate::ROUTE`].
//!
//! `engine` is also compiled natively for `cargo test` — its camera, mesh,
//! label and bridge logic are plain maths and the copied tests are what pin
//! that the move was faithful.
//!
//! # A word about "ward"
//!
//! In this crate a **`Ward` is a folder** — the ground a directory's files
//! stand on, and its parent-child nesting is the folder tree. Kingdom's own
//! glossary in `AGENTS.md` uses "a ward" for [`kingdom_core::Language`], and
//! `kingdom_app::components::ward_tree` is the file tree. That collision is
//! real and deliberate: renaming ~200 references through code whose tests are
//! the evidence it still works would trade a genuine guarantee for a cosmetic
//! one. The two vocabularies do not meet — nothing outside this crate names a
//! `Ward` — so the rule is simply: inside `kingdom-citymap`, a ward is a folder.

#[cfg(feature = "ssr")]
pub mod build;
// `test` as well as `hydrate`: the engine's camera, mesh, label and bridge
// logic is plain maths, and the tests copied with it are the evidence the move
// was faithful. Bevy is a *dev*-dependency on native for exactly this reason --
// a plain `ssr` build of the server must never compile it, which a
// non-optional native dependency would.
#[cfg(any(feature = "hydrate", test))]
pub mod engine;
pub mod map;

#[cfg(feature = "hydrate")]
mod view;

#[cfg(feature = "hydrate")]
pub use view::CityMap;

/// The map, as the server renders it: the element, and nothing in it.
///
/// `app.rs` is compiled on both targets and names [`CityMap`] in its view tree,
/// so the name has to exist on the server too. What it must *not* do is render
/// anything the browser will then disagree with -- so it emits exactly the
/// markup [`view::CityMap`] does, minus the engine, and hydration matches.
///
/// The canvas is deliberately still here rather than behind a `Show`: the
/// engine resolves it by id at boot, and an element that appears only after
/// hydration would not be there to be found.
#[cfg(all(feature = "ssr", not(feature = "hydrate")))]
#[leptos::component]
pub fn CityMap(
    /// The city the King has selected. Unused here; the browser's copy is what
    /// reads and writes it.
    #[allow(unused_variables)]
    selected: leptos::prelude::RwSignal<Option<kingdom_core::CityId>>,
    /// Whether the map is on screen. Unused here for the same reason: only the
    /// browser has an engine to stop. It is taken anyway so that the two
    /// `CityMap` signatures stay identical -- `app.rs` compiles against both,
    /// and a prop on one and not the other is a build failure on whichever
    /// target is not being looked at.
    #[allow(unused_variables)]
    #[prop(into)]
    visible: leptos::prelude::Signal<bool>,
) -> impl leptos::IntoView {
    use leptos::prelude::*;
    view! {
        <div class="city-map">
            <canvas
                id="repo-city-canvas"
                class="city-map-canvas"
                aria-label="The kingdom: every project, drawn as an island of towns"
            ></canvas>
        </div>
    }
}

#[cfg(feature = "ssr")]
pub use build::manifest_for;

/// Where the browser fetches the manifest from, and where the server answers.
///
/// Compiled into both targets and named by both sides, so the fetch and the
/// route cannot drift apart — the same reason `kingdom_app::artifact` keeps its
/// route beside its URL builder.
pub const ROUTE: &str = "/kingdom/map.json";
