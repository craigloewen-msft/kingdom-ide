#![warn(missing_docs)]
//! The kingdom map: every project on the machine, drawn as an isometric disk
//! of towns floating in space.
//!
//! This crate replaced a hand-written SVG map (`kingdom_core::{layout, terrain,
//! skyline}` and `kingdom_app::components::map`, all deleted). It is **Repo
//! City** — <https://github.com/craigloewen-msft/repo-city-visualizer> by Craig
//! Loewen, MIT — copied in wholesale at commit `449f090` rather than depended
//! on, so that there is one project to maintain rather than two. See the
//! `LICENSE` at this crate's root.
//!
//! # The shape of it
//!
//! Three modules, and which target each compiles to is the whole design:
//!
//! | Module | Target | What it is |
//! |---|---|---|
//! | [`map`] | both | the manifest: plain serialisable world-space geometry |
//! | [`follow`] | `hydrate` | when the rail's map may move its camera, and where |
//! | [`progress`] | both | how much of that manifest has arrived, as a bar |
//! | [`build`] | `ssr` | scanning a kingdom on disk and laying it out |
//! | [`engine`] | `hydrate` | drawing a manifest with Bevy |
//!
//! The split is load-bearing in both directions. [`build`] uses `std::fs` and
//! `ignore`, neither of which belongs in a wasm bundle; [`engine`] pulls in
//! Bevy, which must never reach the Axum binary. [`map`] is the only *seam* on
//! both targets, which is exactly why the manifest exists: it is where the two
//! halves meet, and `kingdom-app` is what carries it between them over
//! [`crate::ROUTE`]. [`progress`] is on both targets for a smaller reason --
//! only the browser reads it, but `cargo test` builds this crate with no
//! features at all, and arithmetic nothing compiles is arithmetic nothing
//! tests.
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
//! `kingdom_app::components::file_tree` is the file tree. That collision is
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
// The same gate as `engine`, and for the same reason: when the map may move its
// camera is a decision about a browser, but it is made out of six plain values
// and is exactly the kind of rule that is worth pinning without one. `view.rs`
// is `hydrate`-only and there is no DOM under `cargo test`, so a rule left in
// an effect is a rule nothing can test.
#[cfg(any(feature = "hydrate", test))]
pub mod follow;
pub mod map;
// The same gate as `engine`, for a related reason: deciding whether to draw at
// all is a decision about a browser, but it is made out of two plain values and
// is worth pinning without one.
#[cfg(any(feature = "hydrate", test))]
pub mod mode;
pub mod progress;

#[cfg(feature = "hydrate")]
mod view;

#[cfg(feature = "hydrate")]
pub use view::CityMap;

/// The map, as the server renders it: the element, and nothing in it.
///
/// `app.rs` is compiled on both targets and names [`CityMap`] in its view tree,
/// so the name has to exist on the server too.
///
/// # Why it need not match the browser's markup
///
/// It does not, and cannot: [`view::CityMap`] also renders a loading card and
/// an error line, both of which are driven by client state. That is safe
/// because **the map is never server-rendered at all**. `App` gates the whole
/// interface behind `kingdom.is_open()`, the server answers that with
/// `ChooseKingdom`, and the only thing that opens a kingdom is an `Effect` --
/// which does not run during SSR. The delivered document holds the folder
/// picker and no `repo-city-canvas`.
///
/// So this exists to satisfy the compiler rather than the hydrator, and adding
/// the client-only nodes here would be the riskier move: a static element on
/// one side and a dynamic one on the other is how a mismatch is actually made.
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
    /// Which cities have work under way. Unused here for the same reason: the
    /// signature must match the browser's, because `app.rs` names this
    /// component on both targets.
    #[allow(unused_variables)]
    #[prop(into)]
    working: leptos::prelude::Signal<Vec<kingdom_core::CityActivity>>,
    /// Where the map is standing. Unused here for the same reason: only the
    /// browser has an engine to slow down or stop. It is taken anyway so that
    /// the two `CityMap` signatures stay identical -- `app.rs` compiles against
    /// both, and a prop on one and not the other is a build failure on
    /// whichever target is not being looked at. That is also why
    /// [`map::MapPresence`] lives in `map`, the one module on both targets.
    #[allow(unused_variables)]
    #[prop(into)]
    presence: leptos::prelude::Signal<map::MapPresence>,
    /// The city the rail's map frames. Unused here, for the reason above.
    #[allow(unused_variables)]
    #[prop(into)]
    focus_city: leptos::prelude::Signal<Option<kingdom_core::CityId>>,
    /// The file the rail's map points at. Unused here, for the reason above.
    #[allow(unused_variables)]
    #[prop(into)]
    focus_file: leptos::prelude::Signal<Option<String>>,
    /// The file the King picked off the map. Unused here, for the reason
    /// above -- and doubly so: the server never renders a map to press.
    #[allow(unused_variables)]
    #[prop(into)]
    picked_file: leptos::prelude::RwSignal<Option<String>>,
    /// What every live agent in the focused city is changing. Unused here, for
    /// the reason above -- only the browser has a manifest to resolve it
    /// against and an engine to raise it with.
    #[allow(unused_variables)]
    #[prop(into)]
    works: leptos::prelude::Signal<Vec<(kingdom_core::PlanId, kingdom_core::ChangeSummary)>>,
) -> impl leptos::IntoView {
    use leptos::prelude::*;
    view! {
        <div class="city-map">
            <canvas
                id="repo-city-canvas"
                class="city-map-canvas"
                aria-label="The kingdom: every project, drawn as a disk of towns in space"
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
