//! The map panel: one canvas, and the engine drawing into it.
//!
//! This is the half of the old `MapViewer` that Kingdom keeps. Repo City's own
//! component wrapped the canvas in a sidebar, an inspector, a search box, a
//! toolbar and a minimap; Kingdom has its own rails for all of that, so only
//! the map itself was taken and everything around it was left behind.
//!
//! # The engine boots once and never stops
//!
//! On the web `App::run()` hands control to `requestAnimationFrame` and does
//! not return, holding the canvas element it resolved at startup. So this
//! component may be mounted exactly once for the life of the page — see
//! `kingdom_app::app::ThroneRoom`, which mounts it beside the router's outlet
//! and hides it with CSS rather than letting a route unmount it. Mounting a
//! second one would need a second winit event loop in one wasm instance, which
//! is not a thing.

use gloo_net::http::Request;
use gloo_timers::callback::{Interval, Timeout};
use kingdom_core::CityId;
use leptos::prelude::*;
use wasm_bindgen_futures::spawn_local;

use crate::engine;
use crate::engine::bridge::{Bridge, ViewerCommand, ViewerStatus};
use crate::map::MapManifest;

/// How often the component picks up what the engine is showing.
///
/// The engine renders on its own clock. This only has to be often enough that
/// a click lands on whatever the pointer was over, and the bridge's revision
/// counter means an idle map costs nothing.
const POLL_INTERVAL_MS: u32 = 50;

/// How long the loading card is given to paint its second phase before the
/// engine is handed the world.
///
/// Long enough for a frame at any refresh rate, short enough to be invisible
/// inside a wait measured in seconds. See the note where it is used: without
/// it the phase is never drawn at all, because the work it announces begins on
/// the same frame that would have announced it.
const PAINT_PAUSE_MS: u32 = 50;

/// The map: every city in the kingdom, as an island of towns.
///
/// Fetches its manifest from [`crate::ROUTE`] on mount and hands it to the
/// engine. `selected` is the King's chosen city, shared with the rest of the
/// app — clicking a building sets it, clicking open sea clears it.
#[component]
pub fn CityMap(
    /// The city the King has selected, if any.
    selected: RwSignal<Option<CityId>>,
) -> impl IntoView {
    let manifest = RwSignal::new(None::<MapManifest>);
    let load_error = RwSignal::new(None::<String>);
    let status = RwSignal::new(ViewerStatus::default());

    let bridge = Bridge::new();

    // The engine takes over the canvas, so it can only start once the canvas is
    // in the document. `engine::run` never returns on the web, so it is
    // deferred into its own task rather than blocking the effect.
    let boot = bridge.clone();
    Effect::new(move |started: Option<bool>| {
        if started == Some(true) {
            return true;
        }
        let boot = boot.clone();
        Timeout::new(0, move || engine::run(boot)).forget();
        true
    });

    let loader = bridge.clone();
    Effect::new(move |_| {
        let loader = loader.clone();
        spawn_local(async move {
            match load_manifest().await {
                Ok(map) => {
                    // The signal first, the command second, and a beat between
                    // them -- which is not fussiness, it is the only way the
                    // second phase is ever seen.
                    //
                    // Handing the engine a world is not a request: Bevy's loop
                    // runs on `requestAnimationFrame`, which fires *before* the
                    // browser paints, so `spawn_world` blocks the very frame
                    // that would have drawn "Raising the cities". Sent inline,
                    // the card went from "Surveying the realm" straight to gone
                    // -- measured, not guessed -- and the King saw a card
                    // freeze and vanish rather than a phase change.
                    //
                    // So the phase is published, the task yields, and the paint
                    // lands before the block begins. The delay is spent inside
                    // a wait of several seconds and costs nothing anyone can
                    // perceive.
                    manifest.set(Some(map.clone()));
                    Timeout::new(PAINT_PAUSE_MS, move || {
                        loader.send(ViewerCommand::Load(Box::new(map)));
                    })
                    .forget();
                }
                Err(error) => load_error.set(Some(error)),
            }
        });
    });

    // The engine publishes what it is showing; this mirrors it into a signal so
    // the click handler can read it. The revision check keeps an idle map from
    // waking the interface sixty times a second.
    let watcher = bridge.clone();
    Effect::new(move |_| {
        let watcher = watcher.clone();
        let mut seen = u64::MAX;
        Interval::new(POLL_INTERVAL_MS, move || {
            let revision = watcher.revision();
            if revision == seen {
                return;
            }
            seen = revision;
            status.set(watcher.status());
        })
        .forget();
    });

    // Clicking a building selects its city; clicking open sea clears it.
    //
    // The engine observes `Pointer<Over>`/`Pointer<Out>` but has no click
    // handler of its own, so the DOM's click is paired with whatever the engine
    // last reported as hovered. Now that the engine is Kingdom's own source a
    // real `Pointer<Click>` observer in `engine::spawn` is available and is the
    // tidier answer; this stays as the smaller change until something needs the
    // difference.
    let select = move |_| {
        let hovered = status.with(|state| state.hovered.clone());
        let Some(id) = hovered else {
            selected.set(None);
            return;
        };
        // A feature's `repository` is the project's directory name, which is
        // exactly what `CityId::new` is built from in `kingdom_app::scan`.
        let city = manifest.with(|map| {
            map.as_ref().and_then(|map| {
                map.features
                    .iter()
                    .find(|feature| feature.id == id)
                    .map(|feature| CityId::new(feature.repository.clone()))
            })
        });
        selected.set(city);
    };

    view! {
        <div class="city-map" class:over-holding=move || status.with(|s| s.hovered.is_some())>
            <canvas
                id="repo-city-canvas"
                class="city-map-canvas"
                on:click=select
                aria-label="The kingdom: every project, drawn as an island of towns"
            ></canvas>
            <Survey manifest=manifest status=status failed=load_error/>
            {move || load_error.get().map(|error| view! {
                <p class="city-map-error">{error}</p>
            })}
        </div>
    }
}

/// The loading card: what the map is doing, while it is doing it.
///
/// The map region is painted `$void` and nothing else, so without this the
/// King watches a black rectangle for the several seconds the two waits take —
/// fetching a manifest the server walks every project to build, and then
/// spawning a few thousand holdings into the scene.
///
/// # Why two phases and not one
///
/// They are genuinely two waits, and the second is the one that looks broken.
/// A card dismissed when the fetch resolved would vanish immediately *before*
/// the longest main-thread block in the app, so the King would watch the
/// loading state disappear and then watch the page freeze — worse than never
/// having shown one. So it stands until the engine reports
/// [`ViewerStatus::built`], which is published after `spawn_world` has run.
///
/// # Why it goes on failure
///
/// A fetch that failed is not still working, and `city-map-error` is what has
/// something true to say at that point. Leaving the card up would put a
/// cheerful animation over an error message.
#[component]
fn Survey(
    /// The manifest, once it has arrived. Its absence *is* the first phase.
    manifest: RwSignal<Option<MapManifest>>,
    /// What the engine is showing; `built` is what dismisses this.
    status: RwSignal<ViewerStatus>,
    /// Set when the manifest could not be fetched or read.
    failed: RwSignal<Option<String>>,
) -> impl IntoView {
    let done = move || status.with(|s| s.built) || failed.with(Option::is_some);

    // The manifest's own one-line summary — "6 towns · 3,014 holdings". It is
    // the nearest thing to honest progress available without streaming the
    // fetch: it names what is about to be drawn, and it is earned rather than
    // invented.
    let detail = move || {
        manifest.with(|map| match map {
            Some(map) => map.subtitle.clone(),
            None => "Reading every city in the kingdom".to_owned(),
        })
    };
    let phase = move || {
        if manifest.with(Option::is_none) {
            "Surveying the realm"
        } else {
            "Raising the cities"
        }
    };

    view! {
        // `role`/`aria-live`, because a wait this long should be announced and
        // not only drawn. `aria-hidden` on the drawing: it is decoration, and
        // the two lines below it already say everything it says.
        <div class="city-survey" class:gone=done role="status" aria-live="polite">
            <div class="survey-card">
                <div class="survey-plot" aria-hidden="true">
                    <span class="survey-tower"></span>
                    <span class="survey-tower"></span>
                    <span class="survey-tower"></span>
                </div>
                <p class="survey-phase">{phase}</p>
                <p class="survey-detail">{detail}</p>
                <div class="survey-rule" aria-hidden="true"><span></span></div>
            </div>
        </div>
    }
}

/// Fetches the manifest the server built for this kingdom.
async fn load_manifest() -> Result<MapManifest, String> {
    let response = Request::get(crate::ROUTE)
        .send()
        .await
        .map_err(|error| format!("could not reach the map: {error}"))?;
    if !response.ok() {
        return Err(format!(
            "the map could not be drawn ({})",
            response.status()
        ));
    }
    response
        .json::<MapManifest>()
        .await
        .map_err(|error| format!("the map could not be read: {error}"))
}
