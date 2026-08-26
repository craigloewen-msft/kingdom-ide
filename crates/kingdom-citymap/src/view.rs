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
//!
//! # Or it never boots at all
//!
//! The other case is [`crate::mode`]: under an automated browser the engine is
//! stood down and a notice is drawn in its place. Nothing here is created and
//! then disabled -- no canvas, no manifest fetch, no timers, no Bevy -- because
//! the cost this avoids is paid the moment a WebGL context is asked for, and no
//! later instruction takes it back. See the module doc there.

use gloo_net::http::Request;
use gloo_timers::callback::{Interval, Timeout};
use kingdom_core::{ChangeSummary, CityActivity, CityId};
use leptos::prelude::*;
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::JsFuture;
use wasm_bindgen_futures::spawn_local;

use crate::engine;
use crate::engine::bridge::{Bridge, TownActivity, ViewerCommand, ViewerStatus};
use crate::map::{MapManifest, MapPresence};
use crate::mode::{decide, MapMode};
use crate::progress::{Transfer, Wait};

/// How often the component picks up what the engine is showing.
///
/// The engine renders on its own clock. This has to be often enough that a
/// click lands on whatever the pointer was over, and -- the reason it is not
/// slower -- often enough to catch the loading bar moving. A world goes up in
/// frames of about `raise::TARGET_FRAME`, so a poll of the same order would
/// alias against them and throw away most of the readings the engine
/// published: measured at 50 ms, the bar painted a third of what it was
/// told. The bridge's revision counter is what makes asking this often cheap,
/// and an idle map still costs nothing.
const POLL_INTERVAL_MS: u32 = 16;

/// How long the loading card is given to paint its second phase before the
/// engine is handed the world.
///
/// Long enough for a frame at any refresh rate, short enough to be invisible
/// inside a wait measured in seconds. See the note where it is used: without
/// it the phase is never drawn at all, because the work it announces begins on
/// the same frame that would have announced it.
const PAINT_PAUSE_MS: u32 = 50;

/// How long to wait after the map changes home before re-framing the camera.
///
/// The map moves between two rectangles of wildly different shape -- the whole
/// main region, and a pane at the foot of the rail -- and [`CameraRig`] is only
/// ever re-fitted on `Load` and `Fit`, so a camera framed for one is wrong in
/// the other.
///
/// The pause is the same class of problem as [`PAINT_PAUSE_MS`] above, in the
/// other direction. Bevy resizes its canvas from a `ResizeObserver` on the
/// parent element, which fires asynchronously, so a `Fit` or a `Focus` sent in
/// the same tick as the class change would frame against the **old** viewport
/// and leave the map cropped in its new home. Waiting a beat lets the observer
/// land first.
const RESIZE_SETTLE_MS: u32 = 140;

/// What the map is doing on this page load, decided once and remembered.
///
/// # Why it is cached
///
/// Both facts it is decided from can change under the app's feet, and neither
/// change may be allowed to alter the answer. `location.search` is gone the
/// moment the router pushes `/plan/:id`, so a King who arrived on `?map=on`
/// would find the override silently withdrawn by his first navigation. One read
/// at boot is what keeps the answer the same for the life of the page -- and
/// `CityMap` is mounted exactly once, so it decides exactly once.
pub(crate) fn map_mode() -> MapMode {
    static DECIDED: std::sync::OnceLock<MapMode> = std::sync::OnceLock::new();
    *DECIDED.get_or_init(|| decide(automated(), forced().as_deref()))
}

/// Whether this browser is being driven by automation.
///
/// `navigator.webdriver`, which CDP sets -- so Kingdom's own `browser_*` tools,
/// Playwright and Puppeteer all report it without any of them being named here.
///
/// The cast is unchecked because it has to be: `webdriver` lives on the
/// `NavigatorAutomationInformation` mixin, which `web-sys` declares with
/// `is_type_of = |_| false`, so `dyn_into` can never succeed. Reading a missing
/// property off a real `Navigator` yields `undefined`, which arrives here as
/// `false` -- the answer wanted for any browser that does not have it.
fn automated() -> bool {
    let Some(window) = web_sys::window() else {
        return false;
    };
    let navigator = window.navigator();
    let automation: &web_sys::NavigatorAutomationInformation = navigator.unchecked_ref();
    automation.webdriver()
}

/// The `map` query parameter, if the page was opened with one.
fn forced() -> Option<String> {
    let search = web_sys::window()?.location().search().ok()?;
    let params = web_sys::UrlSearchParams::new_with_str(&search).ok()?;
    params.get(crate::mode::OVERRIDE)
}

/// The map: every city in the kingdom, as a disk of towns hanging in space.
///
/// Fetches its manifest from [`crate::ROUTE`] on mount and hands it to the
/// engine. `selected` is the King's chosen city, shared with the rest of the
/// app — clicking a building sets it, clicking empty space clears it.
///
/// `working` is which cities have agents in them right now, refreshed by
/// whoever owns this component rather than here: the map draws what it is told
/// and does not decide how often to ask.
///
/// # Two homes
///
/// This component is mounted exactly once (see the module note) but is shown in
/// two places: its own screen, and a pane at the foot of the cities rail while
/// the King is in a chamber. `presence` is which, and it is what the two focus
/// props below are gated on — in the rail the map is *scoped* to the work in
/// front of him, and on his own map he drives the camera himself.
#[component]
pub fn CityMap(
    /// The city the King has selected, if any.
    selected: RwSignal<Option<CityId>>,
    /// Which cities have a turn in flight, and how many.
    #[prop(into)]
    working: Signal<Vec<CityActivity>>,
    /// Where the map is standing, and therefore how hard the engine should
    /// work. The engine cannot see a CSS class, so it has to be told.
    #[prop(into)]
    presence: Signal<MapPresence>,
    /// The city the rail's map should frame, if any.
    ///
    /// Read only in [`MapPresence::Rail`]. This is the plan's own city, so the
    /// pane beside a conversation shows the place that conversation is about
    /// rather than the whole kingdom at a size nothing is legible at.
    #[prop(into)]
    focus_city: Signal<Option<CityId>>,
    /// The file open in the chamber's panel, relative to the city's root.
    ///
    /// Read only in [`MapPresence::Rail`], and narrows the frame further: from
    /// the town to the one holding that file's building stands on.
    #[prop(into)]
    focus_file: Signal<Option<String>>,
    /// What the open plan is proposing, if the King is in a chamber.
    ///
    /// Resolved against the manifest here and handed to the engine as plain
    /// geometry -- see the effect below, which is the boundary the engine's
    /// ignorance of Kingdom's domain is kept at. `None` outside a chamber, which
    /// tears the works down.
    #[prop(into)]
    works: Signal<Option<ChangeSummary>>,
) -> impl IntoView {
    // First, and before anything is created: an engine that is not to run must
    // not be half-started and then told to stop. See the module doc.
    if map_mode().stood_down() {
        return view! { <StoodDown/> }.into_any();
    }

    let manifest = RwSignal::new(None::<MapManifest>);
    let load_error = RwSignal::new(None::<String>);
    let status = RwSignal::new(ViewerStatus::default());
    // How much of the manifest has arrived. Written from inside the fetch, so
    // the card can report the first of the two waits as it happens rather than
    // only when it ends.
    let transfer = RwSignal::new(Transfer::default());

    let bridge = Bridge::new();

    let loader = bridge.clone();
    Effect::new(move |_| {
        let loader = loader.clone();
        spawn_local(async move {
            let outcome = load_manifest(transfer).await;
            // The signal first, the engine second, and a beat between them --
            // which is not fussiness, it is the only way the phase after this
            // one is ever seen.
            //
            // Booting Bevy is not a request either. It asks for a GPU adapter
            // and a device and compiles the first pipelines, and every bit of
            // that lands on the thread the card is drawn from. Started on
            // mount, as it used to be, all of it landed *on top of the fetch*:
            // measured against a running server, a 3.3-second download painted
            // its bar exactly twice, and the card then sat on an indeterminate
            // sweep for up to 1.4 seconds while the engine finished waking.
            //
            // Nothing can be drawn before the manifest exists, so waiting for
            // it costs no pixel and buys the whole first phase a free thread.
            match outcome {
                Ok(map) => {
                    manifest.set(Some(map.clone()));
                    Timeout::new(PAINT_PAUSE_MS, move || {
                        // Queued before the engine is started, because
                        // `engine::run` may never return -- see below. The
                        // bridge holds it until the first update drains it,
                        // which is exactly what the queue is for.
                        loader.send(ViewerCommand::Load(Box::new(map)));
                        boot(loader);
                    })
                    .forget();
                }
                Err(error) => {
                    // Still booted, so a kingdom whose manifest could not be
                    // read shows the same empty space and stars it always did
                    // behind the error, rather than a black rectangle.
                    load_error.set(Some(error));
                    Timeout::new(PAINT_PAUSE_MS, move || boot(loader)).forget();
                }
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

    // Which towns are alight. Sent on every change rather than polled by the
    // engine, and translated from `CityId` to a town name here because this is
    // the boundary the engine's ignorance of Kingdom's domain is kept at.
    //
    // The name is the identifier on purpose. A `CityId` is built from a
    // project's directory name (`kingdom_app::scan`), and a `MapTown`'s name is
    // that same directory name (`build::scan` reads it from `file_name`) — the
    // very identity the click handler below already relies on in the other
    // direction. The manifest's own `town-N` ids would be the obvious key and
    // are the wrong one: `scene::towns` numbers them in packing order while
    // `manifest::build_world_manifest` numbers its districts by file count, so
    // the two need not agree within one manifest.
    let reporter = bridge.clone();
    Effect::new(move |_| {
        let towns = working
            .get()
            .into_iter()
            .map(|city| TownActivity {
                town: city.city.to_string(),
                working: city.working,
            })
            .collect();
        reporter.send(ViewerCommand::SetActivity(towns));
    });

    // The engine draws whether or not anything is on screen, so it is told
    // where it stands. This is an ordinary effect rather than part of the boot
    // one: it has to run again every time the King moves between the map and a
    // chamber, which is the whole point of it.
    //
    // Kept apart from the activity effect above rather than merged into one:
    // each tracks a single signal, so moving between the map and a chamber does
    // not also re-send the town list, and a poll landing does not re-send the
    // presence.
    let watching = bridge.clone();
    Effect::new(move |_| {
        watching.send(ViewerCommand::Show(presence.get()));
    });

    // Two reads of the engine's status, as memos rather than as bare reads
    // inside the effects below.
    //
    // This is not tidiness. `status` is re-set wholesale on every poll the
    // bridge's revision moved for -- and the revision moves for a camera rect
    // that shifted half a world unit, so *panning the map* re-set it. The
    // focus effects below read `built` from it, so a pan re-ran them and
    // re-sent the `Focus` that dragged the camera straight back: the map
    // fighting the King's own hand. A memo only notifies when its own value
    // changes, so a pan no longer wakes anything that would undo it.
    let built = Memo::new(move |_| status.with(|state| state.built));
    // Whether the King has taken the camera. The two focus effects are gated
    // on it, and both track it -- so when the engine hands the camera back
    // after `input::RELEASE_AFTER` they re-run on their own and the map
    // returns to the city and the file that are open *now*, rather than
    // waiting for the next time one of them changes.
    let manual = Memo::new(move |_| status.with(|state| state.manual));

    // What the open plan is proposing, resolved into ground and handed over.
    //
    // **This is the boundary.** Everything above it is Kingdom's domain -- a
    // `ChangeSummary` of `ChangedFile`s with paths and line counts -- and
    // everything below it is world-space geometry. The engine never learns what
    // a plan or a changed file is, exactly as it never learns what a `CityId`
    // is: `SetActivity` above translates for the same reason.
    //
    // Tracks the manifest as well as the works, and must: a chamber opened from
    // a cold page has its summary in hand long before the map has arrived, and
    // without the dependency those changes would never be drawn at all. The
    // same trap the scoping effect below documents.
    //
    // And it tracks `built` for a second, sharper reason. Raising a world
    // clears the works (`apply_commands`, the `Load` arm) -- scaffolding left
    // hanging over a settlement being torn down would end up above whatever
    // replaced it. On a cold page the summary is usually resolved *before* that
    // `Load` lands, so without this the first send is thrown away and nothing
    // ever asks again: the signal has not changed, so the effect does not
    // re-run. Tracking `built` is what re-sends them once the city stands.
    // Measured, not guessed -- the works were silently absent on first open.
    let builder = bridge.clone();
    Effect::new(move |_| {
        let summary = works.get();
        let city = focus_city.get();
        let standing = built.get();
        let raised = manifest.with(|map| match (map, &summary, &city) {
            (Some(map), Some(summary), Some(city)) if standing => {
                crate::map::works::resolve(map, city.as_str(), summary)
            }
            // No plan open, or nothing to draw against yet. An empty list is
            // how the works are torn down, so this is sent rather than skipped.
            _ => Vec::new(),
        });
        builder.send(ViewerCommand::SetWorks(raised));
    });

    // Re-framing when the map changes home.
    //
    // The two homes are wildly different shapes, and the rig is re-fitted only
    // on `Load` and `Fit` -- so without this the camera framed for one is
    // cropped in the other. The rail's frame is the scoped one when there is a
    // city to scope to, because `Focus` re-frames against the current viewport
    // and therefore does the fitting as well as the scoping; otherwise, and on
    // the King's own map, the whole world.
    //
    // Deliberately keyed on the home *changing* rather than on every render:
    // `Effect` re-runs only when `presence` moves, so panning the rail's map
    // and then opening a file does not snap the camera back.
    //
    // The pause is `RESIZE_SETTLE_MS`, and its doc says why it cannot be zero.
    //
    // Not suppressed while the King holds the camera, and instead it *ends*
    // that hold. A camera framed for the whole main region is simply wrong in
    // a 290px pane at the foot of the rail, so this is fitting rather than
    // following -- and making a change of home hand the camera back gives the
    // rule a shape a person can hold: free look lasts as long as the map stays
    // where it is.
    let reframer = bridge.clone();
    Effect::new(move |_| {
        let presence = presence.get();
        if !presence.showing() {
            return;
        }

        let reframer = reframer.clone();
        reframer.send(ViewerCommand::ReleaseCamera);
        Timeout::new(RESIZE_SETTLE_MS, move || {
            // Read when it *fires*, not when it was scheduled, and that is the
            // whole reason this is not captured above. Arriving in a chamber
            // changes the route before the conversation has mounted and told us
            // its city, so a scope read at schedule time is empty -- and this
            // would then land a `Fit` on top of the `Focus` the effect below
            // had already sent, leaving the rail showing the whole kingdom.
            //
            // Reading late is also simply more correct: the point of the delay
            // is to act once the viewport has settled, so the scope should be
            // whatever is true by then.
            let scoped = presence
                .in_rail()
                .then(|| focus_city.get_untracked())
                .flatten()
                .and_then(|city| {
                    manifest.with_untracked(|map| {
                        let town = map.as_ref()?.town_named(city.as_str())?;
                        Some((town.center, town.extent))
                    })
                });

            reframer.send(match scoped {
                Some((center, extent)) => ViewerCommand::Focus { center, extent },
                None => ViewerCommand::Fit,
            });
        })
        .forget();
    });

    // Scoping the rail's map to the city the conversation is about.
    //
    // Only in the rail: on his own map the King drives the camera, and a view
    // that jumped every time the selection changed would take it from him.
    //
    // `built` is tracked as well as the city, and that is load-bearing rather
    // than tidy. A world is raised a slice at a time, and the frame it
    // finishes on calls `fit()` (see `raise::raise_world`) -- so on a page
    // opened straight into a chamber this effect would send its `Focus` while
    // the cities were still going up, and the raise would overwrite it with
    // the whole kingdom a moment later. Re-running once the world is standing
    // is what puts the scope back.
    //
    // And `manual` is tracked for the opposite reason: while the King has the
    // camera this must not move it, and the moment the engine hands it back
    // this must re-run and put the map where it now belongs.
    let scoper = bridge.clone();
    Effect::new(move |_| {
        if manual.get() {
            return;
        }
        let Some(city) = focus_city.get() else {
            return;
        };
        if !presence.get().in_rail() || !built.get() {
            return;
        }
        // Tracked, not `_untracked`: the manifest arrives after the first run
        // of this effect, and without the dependency a chamber opened from a
        // cold page would never frame its city at all.
        let Some((center, extent)) = manifest.with(|map| {
            let town = map.as_ref()?.town_named(city.as_str())?;
            Some((town.center, town.extent))
        }) else {
            return;
        };
        scoper.send(ViewerCommand::Focus { center, extent });
    });

    // And narrowing it to the building of the file he is reading.
    //
    // `Inspect` rather than `LookAt`: this centres on the holding *and* zooms
    // until a house is `camera::INSPECT_HOLDING_PIXELS` wide, which is the
    // tier that draws per-file labels. A bare `LookAt` kept the town's zoom,
    // which in a rail pane is a twenty-pixel house -- the coarsest tier there
    // is -- so the map aimed at the file without ever arriving at it.
    //
    // Closing the panel pulls back to the town, which reverses what this did
    // before. That older behaviour was right when the difference was a pan of
    // a town-wide frame and re-framing would have been motion for nothing; it
    // is wrong now that opening a file fills the pane with one building,
    // because a closed panel would leave the map staring at a file the King is
    // no longer reading. Which city he is in is still true, so the town is
    // what it falls back to.
    //
    // `built` for the same reason as the effect above: a camera pointed at a
    // holding before the world it stands in has finished going up is undone by
    // the raise's closing `fit()`. And `manual` for the same reason too.
    let pointer = bridge.clone();
    Effect::new(move |_| {
        if manual.get() {
            return;
        }
        let open = focus_file.get();
        if !presence.get().in_rail() || !built.get() {
            return;
        }
        let Some(city) = focus_city.get() else {
            return;
        };
        let framed = manifest.with(|map| {
            let map = map.as_ref()?;
            match open.as_deref() {
                // A file to point at: its building, close enough to read.
                Some(path) => {
                    map.holding_at(city.as_str(), path)
                        .map(|holding| ViewerCommand::Inspect {
                            point: holding.center,
                        })
                }
                // None open: back out to the town the chamber is about.
                None => map
                    .town_named(city.as_str())
                    .map(|town| ViewerCommand::Focus {
                        center: town.center,
                        extent: town.extent,
                    }),
            }
        });
        let Some(command) = framed else {
            return;
        };
        pointer.send(command);
    });

    // Clicking a building selects its city; clicking empty space clears it.
    //
    // The engine reports the click itself (`ViewerStatus::clicked`), and the
    // DOM handler now only handles the *absence* of one. It used to pair its
    // own click with whatever hover the 50 ms poll had last delivered, which
    // lost every click that arrived less than one poll after the pointer
    // moved -- a fast human click, and a synthetic one every single time,
    // since a driven pointer moves and presses in the same instant.
    //
    // Reading the click out of an effect rather than out of the handler is
    // what makes that work: the engine may publish it *after* the DOM event
    // has already been and gone.
    //
    // Both halves are suppressed while the map stands in the rail, and that is
    // not an oversight. In a chamber `conversation.rs` force-sets `selected`
    // from the open plan on every render, so a click that changed it would be
    // overwritten a frame later -- and a control that visibly does nothing is
    // worse than no control. The rail's map is a view; its head carries the way
    // to the real one. Panning and zooming stay live either way.
    let last_click = RwSignal::new(None::<(String, u64)>);
    Effect::new(move |_| {
        let Some(click) = status.with(|state| state.clicked.clone()) else {
            return;
        };
        // The same click, seen again on a later poll, is not a new one.
        if last_click.get_untracked().as_ref() == Some(&click) {
            return;
        }
        last_click.set(Some(click.clone()));
        if presence.get_untracked().in_rail() {
            return;
        }
        // A feature's `repository` is the project's directory name, which is
        // exactly what `CityId::new` is built from in `kingdom_app::scan`.
        let city = manifest.with_untracked(|map| {
            map.as_ref().and_then(|map| {
                map.features
                    .iter()
                    .find(|feature| feature.id == click.0)
                    .map(|feature| CityId::new(feature.repository.clone()))
            })
        });
        if city.is_some() {
            selected.set(city);
        }
    });

    // Empty space clears the selection. Nothing in the engine reports a click
    // on *nothing*, so this is still the DOM's job -- and `hovered` is a sound
    // reading here in a way it was not for selection: it asks whether the
    // pointer is over a holding at all, which a stale poll answers correctly
    // for a pointer that has been resting.
    let clear = move |_| {
        if presence.get_untracked().in_rail() {
            return;
        }
        if status.with(|state| state.hovered.is_none()) {
            selected.set(None);
        }
    };

    // Handing the camera back. The chip does not need to know what to re-frame:
    // the two focus effects above track `manual`, so clearing it is enough to
    // make them re-run against whatever is open now.
    //
    // A `Bridge` rather than a ready-made handler, because `Show` may build its
    // children more than once and a closure that moved the handler out of its
    // environment would only be callable the first time.
    let resumer = bridge.clone();

    view! {
        <div class="city-map" class:over-holding=move || status.with(|s| s.hovered.is_some())>
            <canvas
                id="repo-city-canvas"
                class="city-map-canvas"
                on:click=clear
                aria-label="The kingdom: every project, drawn as a disk of towns in space"
            ></canvas>
            // Shown only in the rail, because only there does anything follow
            // him: on his own map the camera was always his, so a chip saying
            // he has taken it would announce a state that is simply the normal
            // one. A real button, so it is reachable by keyboard and announced
            // as the control it is.
            <Show when=move || manual.get() && presence.get().in_rail()>
                {
                    let resumer = resumer.clone();
                    view! {
                        <button
                            class="map-free-look"
                            on:click=move |_| resumer.send(ViewerCommand::ReleaseCamera)
                            title="The map is where you left it. Press to follow the plan again \
                                   -- which also happens on its own after ten minutes."
                        >
                            <span class="map-free-look-mark"></span>
                            <span class="map-free-look-name">"Free look"</span>
                            <span class="map-free-look-resume">"Follow"</span>
                        </button>
                    }
                }
            </Show>
            <Survey manifest=manifest status=status failed=load_error transfer=transfer/>
            {move || load_error.get().map(|error| view! {
                <p class="city-map-error">{error}</p>
            })}
        </div>
    }
    .into_any()
}

/// What stands where the map would be, when the engine has been stood down.
///
/// Deliberately plain rather than in the kingdom's voice. This is a diagnostic
/// addressed to whoever is driving the browser -- most often a model reading
/// the page back through a tool -- and phrased in the metaphor it would read as
/// a feature of the product rather than a fact about this session.
///
/// The second line is the way out. A notice that says only "no" leaves a plan
/// asked to look at the map with nothing to try.
#[component]
fn StoodDown() -> impl IntoView {
    view! {
        <div class="city-map">
            <div class="city-map-stood-down" role="status">
                <p class="stood-down-notice">
                    "Not running full rendering engine in headless test mode"
                </p>
                <p class="stood-down-way-out">"add ?map=on to draw it"</p>
            </div>
        </div>
    }
}

/// The loading card: what the map is doing, how far along it is, while it is
/// doing it.
///
/// The map region is painted `$void` and nothing else, so without this the
/// King watches a black rectangle for the several seconds the two waits take —
/// fetching a manifest the server walks every project to build, and then
/// raising a few thousand holdings into the scene.
///
/// # Why two phases and not one
///
/// They are genuinely two waits, and the second is the one that looks broken.
/// A card dismissed when the fetch resolved would vanish immediately *before*
/// the longest run of main-thread work in the app, so the King would watch the
/// loading state disappear and then watch the page freeze — worse than never
/// having shown one. So it stands until the engine reports
/// [`ViewerStatus::built`], which is published after the last slice of the
/// world has gone up.
///
/// # Why the bar can be trusted
///
/// Both phases report *measured* work rather than a timer pretending to be
/// progress: bytes off the wire against `content-length` for the fetch, and
/// weighted items built against the manifest's own totals for the raise. They
/// are two segments of one scale rather than two bars taking turns -- see
/// [`Wait`] -- so the bar only ever moves forwards, and the moment between them
/// holds where the fetch left it instead of forgetting what it knew.
///
/// One case still declines to answer, and it is the case the sweep was written
/// for: a fetch whose length the server never declared. Then the first segment
/// is genuinely unmeasurable, and an honest sweep beats an invented number.
///
/// And it moves during the raise for two reasons, both of which had to be true.
/// [`crate::engine::raise`] hands the frame back between slices -- built in one
/// call, as it used to be, the browser could not have repainted a bar at all --
/// and it stops the engine rendering a world nobody can see while it does, so
/// the frame it hands back is one the card can actually be drawn in.
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
    /// What the engine is showing; `built` is what dismisses this, and
    /// `raising` is what the bar reads during the second phase.
    status: RwSignal<ViewerStatus>,
    /// Set when the manifest could not be fetched or read.
    failed: RwSignal<Option<String>>,
    /// How much of the manifest has come off the wire, during the first phase.
    transfer: RwSignal<Transfer>,
) -> impl IntoView {
    let done = move || status.with(|s| s.built) || failed.with(Option::is_some);
    let arrived = move || manifest.with(Option::is_some);
    let built = move || status.with(|s| s.built);

    // The whole wait as one number. See [`Wait`]: the two phases are segments
    // of a single scale rather than two bars taking turns, so the moment
    // between them holds the bar at the boundary instead of dropping it back to
    // a sweep -- a moment measured at up to 1.4 seconds, not the single frame
    // this once assumed.
    let wait = move || Wait {
        transfer: transfer.get(),
        arrived: arrived(),
        raising: status.with(|s| s.raising.map(|raising| raising.fraction)),
        built: built(),
    };
    let fraction = move || wait().fraction();

    // What is happening, in the King's words. During the raise the engine says
    // which part of the settlement is going up, so the caption moves with the
    // bar instead of standing still for the whole of the longer wait.
    let phase = move || {
        if !arrived() {
            return "Surveying the realm".to_owned();
        }
        if built() {
            return "The kingdom stands".to_owned();
        }
        status.with(|s| match s.raising {
            // The world is built and the engine is about to show it. A full bar
            // under the caption of a building stage would claim the masons are
            // still at the names while they are in fact standing back -- and
            // this is the longest single frame of the whole wait, so it is the
            // caption the King reads for the longest.
            Some(raising) if raising.fraction >= 1.0 => "Opening the gates".to_owned(),
            Some(raising) => raising.stage.label().to_owned(),
            // The manifest is in hand and nothing is going up yet, which is two
            // different waits and used to be reported as one. Before the engine
            // has woken there is nobody to build; after it has, the first slice
            // is a frame away. Saying "Raising the cities" for both announced
            // work that had not started, for as long as a second and a half.
            None if !s.awake => "Summoning the masons".to_owned(),
            None => "Raising the cities".to_owned(),
        })
    };

    // The line under it. Bytes while they are arriving; afterwards the
    // manifest's own one-line summary -- "6 towns · 3,014 holdings" -- which
    // names what is being built and is earned rather than invented.
    let detail = move || {
        manifest.with(|map| match map {
            Some(map) => map.subtitle.clone(),
            None => transfer.with(Transfer::detail),
        })
    };

    // A percentage for anyone listening rather than looking. Announced only
    // when there is a real fraction: `aria-valuenow` on an indeterminate bar
    // would have a screen reader read out a number nobody measured.
    let value_now = move || fraction().map(|f| format!("{:.0}", f * 100.0));

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
                // One bar for both waits, and the sweep it falls back to is the
                // rule this card has always drawn. `scaleX` rather than
                // `width`: a transform composites off the main thread, and this
                // bar's whole job is to keep moving across work that is on it.
                <div
                    class="survey-rule"
                    class:measured=move || fraction().is_some()
                    role="progressbar"
                    aria-valuemin="0"
                    aria-valuemax="100"
                    aria-valuenow=value_now
                >
                    <span style:transform=move || {
                        fraction().map(|f| format!("scaleX({f})"))
                    }></span>
                </div>
            </div>
        </div>
    }
}

/// Starts the engine into the canvas that is already in the document.
///
/// `engine::run` hands control to the browser's animation loop and may never
/// return, so this must be the last thing its caller does -- which is why the
/// `Load` command is queued on the bridge *before* it, rather than after.
///
/// # Why it is not started on mount
///
/// It was, and that is what made the first phase's bar useless. See the effect
/// in [`CityMap`]: booting costs seconds of main thread, the fetch's bar is
/// drawn from that same thread, and there is nothing to draw until the manifest
/// has arrived anyway.
fn boot(bridge: Bridge) {
    engine::run(bridge);
}

/// Fetches the manifest the server built for this kingdom, reporting progress.
///
/// Read a chunk at a time rather than with `Response::json`, which resolves
/// only once the whole 4 MB body is in hand and so can say nothing while the
/// longest single request in the app is in flight. gloo's own `json()` is
/// `from_str(&text().await?)`, so parsing the assembled bytes here costs no
/// more than it did.
///
/// A body that cannot be streamed is not an error: [`read_whole`] falls back to
/// the old path, and the bar stays indeterminate exactly as it would for a
/// server that declared no length.
async fn load_manifest(transfer: RwSignal<Transfer>) -> Result<MapManifest, String> {
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

    let total = response
        .headers()
        .get("content-length")
        .and_then(|value| value.parse::<u64>().ok());
    transfer.set(Transfer { read: 0, total });

    let body = read_whole(&response, transfer, total).await?;
    serde_json::from_slice::<MapManifest>(&body)
        .map_err(|error| format!("the map could not be read: {error}"))
}

/// Reads a response body, publishing how much has arrived as it goes.
async fn read_whole(
    response: &gloo_net::http::Response,
    transfer: RwSignal<Transfer>,
    total: Option<u64>,
) -> Result<Vec<u8>, String> {
    let Some(stream) = response.body() else {
        // No stream to read: an empty body, or a browser that does not offer
        // one. Either way the bytes are still there to be had, and a bar that
        // cannot move is a smaller loss than a map that does not load.
        return response
            .binary()
            .await
            .map_err(|error| format!("the map could not be read: {error}"));
    };

    let reader: web_sys::ReadableStreamDefaultReader = stream.get_reader().unchecked_into();
    let mut body: Vec<u8> = Vec::with_capacity(total.unwrap_or(0) as usize);

    loop {
        let chunk = JsFuture::from(reader.read())
            .await
            .map_err(|_| "the map arrived only in part".to_owned())?;
        let chunk: web_sys::ReadableStreamReadResult = chunk.unchecked_into();
        if chunk.get_done().unwrap_or(false) {
            break;
        }
        let bytes = js_sys::Uint8Array::new(&chunk.get_value());
        let at = body.len();
        body.resize(at + bytes.length() as usize, 0);
        bytes.copy_to(&mut body[at..]);
        transfer.set(Transfer {
            read: body.len() as u64,
            total,
        });
    }

    Ok(body)
}
