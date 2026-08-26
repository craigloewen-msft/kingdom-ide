//! The application shell and root component.

use crate::api::{get_kingdom, open_kingdom};
use crate::components::{Conversation, PromptBar, Sidebar};
use kingdom_citymap::CityMap;
use kingdom_core::{CityActivity, CityId, Kingdom, ModelChoice, WorkspaceMode};
use leptos::prelude::*;
use leptos_meta::{provide_meta_context, MetaTags, Stylesheet, Title};
use leptos_router::components::{Outlet, ParentRoute, Route, Router, Routes};
use leptos_router::hooks::use_location;
use leptos_router::path;

/// The HTML document shell wrapped around the app during SSR.
pub fn shell(options: LeptosOptions) -> impl IntoView {
    view! {
        <!DOCTYPE html>
        <html lang="en">
            <head>
                <meta charset="utf-8"/>
                <meta name="viewport" content="width=device-width, initial-scale=1"/>
                <AutoReload options=options.clone()/>
                <HydrationScripts options/>
                <MetaTags/>
            </head>
            <body>
                <App/>
            </body>
        </html>
    }
}

/// Width the rail opens at, and the width a double-click on the resizer
/// returns it to.
pub const DEFAULT_SIDEBAR_WIDTH: f64 = 290.0;

/// Width the files rail opens at. Narrower than the cities rail by default and
/// capped lower (see `city_rail.rs`): a tree of names needs less room than a
/// rail of titles and badges, and neither column may become the widest thing on
/// screen.
///
/// The rail itself lives inside the chamber rather than in this grid -- it
/// belongs to a conversation -- but the width is remembered here so it survives
/// moving between plans. See [`KingdomState::tree_width`].
pub const DEFAULT_TREE_WIDTH: f64 = 240.0;

/// What the rail collapses *to*. Never zero: the rail is this app's entire
/// navigation, so a collapsed one that could not be reopened would be a dead
/// end. The strip is wide enough to hold the button that brings it back.
pub const COLLAPSED_RAIL_WIDTH: f64 = 34.0;

/// Below this window width the cities rail folds itself away.
///
/// A plan's chamber can be asking for four columns at once -- the cities rail,
/// the files rail, the transcript and a focused panel -- and the first of those
/// is the one the King is *least* likely to be reading while he reviews a diff.
/// So it is the one that yields. 1250 is where the transcript stops having room
/// to be a conversation once a panel is open beside it.
#[cfg(feature = "hydrate")]
const RAIL_FOLDS_BELOW: f64 = 1250.0;

/// The window's current width, or a width nothing folds at if it cannot be read.
#[cfg(feature = "hydrate")]
fn window_width() -> f64 {
    web_sys::window()
        .and_then(|w| w.inner_width().ok())
        .and_then(|w| w.as_f64())
        .unwrap_or(f64::MAX)
}

/// Where the rail's collapsed state is remembered between visits.
#[cfg(feature = "hydrate")]
const RAIL_COLLAPSED_KEY: &str = "kingdom.rail_collapsed";

/// Where the last-used model and effort are remembered between visits.
#[cfg(feature = "hydrate")]
const MODEL_KEY: &str = "kingdom.model";
#[cfg(feature = "hydrate")]
const EFFORT_KEY: &str = "kingdom.effort";
/// Where the last-used workspace mode is remembered. Two keys, because a mode
/// carrying a branch name has to store it and most modes have none.
#[cfg(feature = "hydrate")]
const WORKSPACE_KEY: &str = "kingdom.workspace";
#[cfg(feature = "hydrate")]
const BRANCH_KEY: &str = "kingdom.branch";

/// Shared UI state, provided via context so the three regions stay in sync
/// without threading props through every layer.
#[derive(Clone, Copy)]
pub struct KingdomState {
    /// The kingdom as last loaded from the server.
    pub kingdom: RwSignal<Kingdom>,
    /// The city the user has selected, if any. Highlighted on the map and
    /// used as the target for a new prompt.
    pub selected: RwSignal<Option<CityId>>,
    /// Current width of the left rail, in pixels. Driven by the resizer.
    pub sidebar_width: RwSignal<f64>,
    /// Current width of the files rail, in pixels. Driven by its own resizer.
    ///
    /// The rail is part of a plan's chamber, not of the throne room, so nothing
    /// outside a conversation reads this. It lives here anyway rather than as a
    /// local signal in the chamber, because a width the King dragged should not
    /// reset every time he opens a different plan.
    pub tree_width: RwSignal<f64>,
    /// Whether the cities rail is folded away to a strip.
    ///
    /// A view preference and nothing more, but it lives here rather than in
    /// `sidebar.rs` because `ThroneRoom` is what writes the grid track and so
    /// has to read it.
    pub rail_collapsed: RwSignal<bool>,
    /// What the King last *asked* for, as distinct from what the window can
    /// currently afford.
    ///
    /// The two differ because a narrow window folds the rail on its own (see
    /// `fold_rail_when_cramped`). Keeping the wish apart from the state is what
    /// lets widening the window restore what he chose, rather than whatever the
    /// last resize happened to leave behind -- and it is why the automatic fold
    /// never writes to storage while [`KingdomState::toggle_rail`] always does.
    pub rail_preference: RwSignal<bool>,
    /// How wide the window was when he last worked the rail himself, if he has.
    ///
    /// The automatic fold defers to a decision made at the width it is looking
    /// at, so an explicit choice is not undone by the next incidental resize.
    /// Crossing the threshold is what makes that decision stale.
    pub rail_decided_at: RwSignal<Option<f64>>,
    /// False shows only live plans; true also shows settled history.
    pub show_all_plans: RwSignal<bool>,
    /// Set while a folder scan is in flight.
    pub loading: RwSignal<bool>,
    /// Most recent error, shown in the chat dock.
    pub error: RwSignal<Option<String>>,
    /// What the next new plan will be drafted with. `None` until the user has
    /// chosen or a remembered choice has been restored, at which point the
    /// server's catalogue default applies.
    pub choice: RwSignal<Option<ModelChoice>>,
    /// How the next new plan will be isolated on disk.
    pub workspace: RwSignal<WorkspaceMode>,
}

impl KingdomState {
    fn new() -> Self {
        Self {
            kingdom: RwSignal::new(Kingdom::unopened()),
            selected: RwSignal::new(None),
            sidebar_width: RwSignal::new(DEFAULT_SIDEBAR_WIDTH),
            tree_width: RwSignal::new(DEFAULT_TREE_WIDTH),
            rail_collapsed: RwSignal::new(false),
            rail_preference: RwSignal::new(false),
            rail_decided_at: RwSignal::new(None),
            show_all_plans: RwSignal::new(false),
            loading: RwSignal::new(false),
            error: RwSignal::new(None),
            choice: RwSignal::new(None),
            workspace: RwSignal::new(WorkspaceMode::default()),
        }
    }

    /// Records the user's choice and remembers it for next time.
    pub fn choose_model(&self, choice: ModelChoice) {
        store_choice(&choice);
        self.choice.set(Some(choice));
    }

    /// Records how the next plan should be isolated, and remembers it.
    pub fn choose_workspace(&self, mode: WorkspaceMode) {
        store_workspace(&mode);
        self.workspace.set(mode);
    }

    /// Folds the cities rail away, or brings it back, remembering which.
    ///
    /// Records the wish, and the width he was at when he made it. Both matter:
    /// the wish is what a widened window gives back, and the width is what
    /// stops the automatic fold undoing a rail he deliberately opened on a
    /// screen too narrow to hold one.
    pub fn toggle_rail(&self) {
        let next = !self.rail_collapsed.get_untracked();
        self.rail_collapsed.set(next);
        self.rail_preference.set(next);
        #[cfg(feature = "hydrate")]
        self.rail_decided_at.set(Some(window_width()));
        store_rail_collapsed(next);
    }
}

#[cfg(feature = "hydrate")]
fn local_storage() -> Option<web_sys::Storage> {
    web_sys::window()?.local_storage().ok().flatten()
}

/// Restores the remembered choice inside an effect, so it runs only on the
/// client -- reading storage during rendering would make the server emit
/// different markup than hydration expects. Whatever comes back is still passed
/// through the server's catalogue before it is used, so a model withdrawn since
/// it was stored degrades rather than failing a prompt.
fn restore_choice(choice: RwSignal<Option<ModelChoice>>) {
    #[cfg(feature = "hydrate")]
    Effect::new(move |_| {
        let Some(storage) = local_storage() else {
            return;
        };
        let Some(model) = storage
            .get_item(MODEL_KEY)
            .ok()
            .flatten()
            .filter(|m| !m.trim().is_empty())
        else {
            return;
        };
        let effort = storage
            .get_item(EFFORT_KEY)
            .ok()
            .flatten()
            .and_then(|e| kingdom_core::ModelEffort::from_wire(&e));
        choice.set(Some(ModelChoice::new(model, effort)));
    });

    #[cfg(not(feature = "hydrate"))]
    let _ = choice;
}

fn store_choice(choice: &ModelChoice) {
    #[cfg(feature = "hydrate")]
    if let Some(storage) = local_storage() {
        let _ = storage.set_item(MODEL_KEY, &choice.model);
        match choice.effort {
            Some(effort) => {
                let _ = storage.set_item(EFFORT_KEY, effort.wire_name());
            }
            // "The model's own default" must be remembered as an absence, not
            // as the `none` level, which is a different request.
            //
            // This arm is reached only when the user presses `default` on the
            // effort row -- never from a change of model, which carries the
            // standing wish across untouched (`ModelChoice::with_model`). A
            // remembered level is therefore forgotten when he says so and at no
            // other time.
            None => {
                let _ = storage.remove_item(EFFORT_KEY);
            }
        }
    }

    #[cfg(not(feature = "hydrate"))]
    let _ = choice;
}

/// Restores the remembered workspace mode, for the same reason and in the same
/// place as [`restore_choice`].
fn restore_workspace(mode: RwSignal<WorkspaceMode>) {
    #[cfg(feature = "hydrate")]
    Effect::new(move |_| {
        let Some(storage) = local_storage() else {
            return;
        };
        let Some(stored) = storage.get_item(WORKSPACE_KEY).ok().flatten() else {
            return;
        };
        let restored = match stored.as_str() {
            "in-place" => WorkspaceMode::InPlace,
            "branch" => storage
                .get_item(BRANCH_KEY)
                .ok()
                .flatten()
                .filter(|b| !b.trim().is_empty())
                .map(WorkspaceMode::Branch)
                // A remembered branch that has since been deleted or renamed
                // degrades to a fresh worktree rather than failing the prompt,
                // exactly as a withdrawn model degrades to the default. The
                // fallback is deliberately the isolated one.
                .unwrap_or(WorkspaceMode::Fresh),
            _ => WorkspaceMode::Fresh,
        };
        mode.set(restored);
    });

    #[cfg(not(feature = "hydrate"))]
    let _ = mode;
}

fn store_workspace(mode: &WorkspaceMode) {
    #[cfg(feature = "hydrate")]
    if let Some(storage) = local_storage() {
        match mode {
            WorkspaceMode::Fresh => {
                let _ = storage.set_item(WORKSPACE_KEY, "fresh");
                let _ = storage.remove_item(BRANCH_KEY);
            }
            WorkspaceMode::InPlace => {
                let _ = storage.set_item(WORKSPACE_KEY, "in-place");
                let _ = storage.remove_item(BRANCH_KEY);
            }
            WorkspaceMode::Branch(b) => {
                let _ = storage.set_item(WORKSPACE_KEY, "branch");
                let _ = storage.set_item(BRANCH_KEY, b);
            }
        }
    }

    #[cfg(not(feature = "hydrate"))]
    let _ = mode;
}

/// Restores whether the rail was left folded away, in an effect for the same
/// reason and in the same place as [`restore_choice`]: reading storage during
/// rendering would make the server emit markup hydration then disagrees with.
///
/// Sets the *preference* as well as the state, because what was stored is what
/// the King asked for -- and the automatic fold below reads the preference to
/// know what to give back when the room returns.
fn restore_rail_collapsed(collapsed: RwSignal<bool>, preference: RwSignal<bool>) {
    #[cfg(feature = "hydrate")]
    Effect::new(move |_| {
        if let Some(stored) =
            local_storage().and_then(|s| s.get_item(RAIL_COLLAPSED_KEY).ok().flatten())
        {
            let wanted = stored == "1";
            preference.set(wanted);
            // Only ever folds. A window too narrow to afford the rail has
            // already folded it by now, and a stored "open" must not override
            // that -- the fold is about room, and the room has not changed.
            if wanted {
                collapsed.set(true);
            }
        }
    });

    #[cfg(not(feature = "hydrate"))]
    let _ = (collapsed, preference);
}

fn store_rail_collapsed(collapsed: bool) {
    #[cfg(feature = "hydrate")]
    if let Some(storage) = local_storage() {
        let _ = storage.set_item(RAIL_COLLAPSED_KEY, if collapsed { "1" } else { "0" });
    }

    #[cfg(not(feature = "hydrate"))]
    let _ = collapsed;
}

/// Folds the cities rail away when the window is too narrow to afford it, and
/// gives it back when the room returns.
///
/// A chamber can want four columns at once, and on a laptop there is not room
/// for all of them. Something has to yield, and the cities rail is the right
/// one: it is navigation the King has already finished using by the time he is
/// reading a diff, and it is the only one of the four that can fold to a strip
/// and still be reopened.
///
/// **It never writes to storage.** That is the whole design. The stored flag is
/// the King's *preference*, and this is a response to the window -- so widening
/// the window restores whatever he chose rather than whatever the last resize
/// happened to leave behind. `toggle_rail` still writes, because that is him
/// deciding, and it updates the preference too -- so a rail he opens on a narrow
/// screen stays open until the width crosses the threshold again, which is the
/// honest reading of "he asked for it *here*".
fn fold_rail_when_cramped(
    collapsed: RwSignal<bool>,
    preference: RwSignal<bool>,
    decided_at: RwSignal<Option<f64>>,
) {
    #[cfg(feature = "hydrate")]
    {
        use wasm_bindgen::JsCast;

        let apply = move || {
            let width = window_width();
            let cramped = width < RAIL_FOLDS_BELOW;

            // A choice the King made on *this* side of the threshold stands.
            // Without this, opening the rail on a laptop is undone by the very
            // next resize -- and every window manager sends a flurry of them.
            // Crossing the threshold is what makes his decision stale, because
            // that is when the question he answered has genuinely changed.
            if let Some(at) = decided_at.get_untracked() {
                if (at < RAIL_FOLDS_BELOW) == cramped {
                    collapsed.set(preference.get_untracked());
                    return;
                }
                decided_at.set(None);
            }

            if cramped {
                collapsed.set(true);
            } else {
                // Back to whatever he asked for -- not merely "open", which
                // would undo a fold he chose himself before the window ever
                // narrowed.
                collapsed.set(preference.get_untracked());
            }
        };

        // Once on arrival, so a chamber opened on a laptop starts folded rather
        // than folding itself a moment later.
        Effect::new(move |_| apply());

        let on_resize = wasm_bindgen::closure::Closure::<dyn FnMut()>::new(apply);
        if let Some(window) = web_sys::window() {
            let _ = window
                .add_event_listener_with_callback("resize", on_resize.as_ref().unchecked_ref());
        }
        // Leaked deliberately: the listener lives as long as the app does, and
        // `App` is never unmounted. Dropping the closure while the listener is
        // still registered would call freed memory on the next resize.
        on_resize.forget();
    }

    #[cfg(not(feature = "hydrate"))]
    let _ = (collapsed, preference, decided_at);
}

/// How often the map asks which cities have agents working in them.
///
/// A compromise, and worth naming as one. The chamber is *pushed* to over a
/// socket; the map is not, because `events.rs` is keyed per plan by design --
/// its own doc says a kingdom-wide channel "would wake every open tab for every
/// keystroke of every plan". Two seconds is fast enough that a town lighting up
/// feels like a response to starting work, and the request is a few dozen bytes
/// asked only while the King is actually looking at the map.
#[cfg(feature = "hydrate")]
const ACTIVITY_POLL_MS: u64 = 2_000;

/// Keeps `working` current while the map is on screen, and stops the moment it
/// is not.
///
/// Stopping matters more than starting. The King spends most of his time in a
/// chamber, where this answers a question nobody is asking -- and a timer left
/// running there would poll the server for the life of the session. The
/// start/stop/`on_cleanup` shape is the one `conversation.rs::elapsed_clock`
/// already uses for its own interval, for the same reason.
fn poll_activity(working: RwSignal<Vec<CityActivity>>, showing: Memo<bool>) {
    #[cfg(feature = "hydrate")]
    {
        let running = StoredValue::new(None::<leptos::leptos_dom::helpers::IntervalHandle>);

        let refresh = move || {
            leptos::task::spawn_local(async move {
                // A failure is silence, not an error banner. This is ambient
                // decoration on a map; a server restart mid-poll must not put a
                // message in front of the King about something he did not ask
                // for. The next tick asks again.
                if let Ok(seen) = crate::api::kingdom_activity().await {
                    working.set(seen);
                }
            });
        };

        let stop = move || {
            if let Some(handle) = running.try_get_value().flatten() {
                handle.clear();
                running.try_set_value(None);
            }
        };

        Effect::new(move |_| {
            // Unconditionally first: this run supersedes the last, whether it is
            // about to start a new timer or to stop entirely.
            stop();

            if !showing.get() {
                // Leaving the map clears what was last seen, so returning to it
                // cannot show a ring around a town that stopped working while
                // the King was elsewhere. The first poll is a moment away.
                working.set(Vec::new());
                return;
            }

            // Asked straight away, so the map does not stand quiet for two
            // seconds after arriving on it.
            refresh();

            if let Ok(handle) = leptos::leptos_dom::helpers::set_interval_with_handle(
                refresh,
                std::time::Duration::from_millis(ACTIVITY_POLL_MS),
            ) {
                running.try_set_value(Some(handle));
            }
        });

        on_cleanup(stop);
    }

    #[cfg(not(feature = "hydrate"))]
    let _ = (working, showing);
}

/// Root component: the throne room.
#[component]
pub fn App() -> impl IntoView {
    provide_meta_context();

    let state = KingdomState::new();
    provide_context(state);
    restore_choice(state.choice);
    restore_workspace(state.workspace);
    restore_rail_collapsed(state.rail_collapsed, state.rail_preference);
    // After the restore, so "enough room again" gives back the King's own
    // preference rather than the default standing in for it.
    fold_rail_when_cramped(
        state.rail_collapsed,
        state.rail_preference,
        state.rail_decided_at,
    );

    // Load any kingdom the server already has open, so a refresh does not
    // send the user back to the folder picker.
    let initial = Resource::new(|| (), |_| get_kingdom());
    Effect::new(move |_| {
        if let Some(Ok(k)) = initial.get() {
            if k.is_open() {
                state.kingdom.set(k);
            }
        }
    });

    view! {
        <Stylesheet id="leptos" href="/pkg/kingdom-ide.css"/>
        <Title text="Kingdom IDE"/>

        <Show
            // `with`, not `get`: this is the app's outermost gate and re-runs on
            // every push, and `is_open` only asks whether the root is a
            // non-empty string -- `get` cloned every city and every plan to
            // answer it.
            when=move || state.kingdom.with(|k| k.is_open())
            fallback=move || view! { <ChooseKingdom/> }
        >
            // The rail lives on the parent route so it never unmounts: moving
            // between the fixture and a plan's conversation is then instant,
            // and the rail keeps its scroll position and collapse state across
            // the move.
            <Router>
                <Routes fallback=|| view! { <NoSuchPlace/> }>
                    <ParentRoute path=path!("") view=ThroneRoom>
                        <Route path=path!("") view=Realm/>
                        <Route path=path!("plan/:id") view=Conversation/>
                    </ParentRoute>
                </Routes>
            </Router>
        </Show>
    }
}

/// The frame both screens hang in: the cities rail, and whichever view is
/// routed beside it.
///
/// Two tracks, and the proportion between them is the point: the rail is a
/// **fixed** width and the main region takes `1fr`. The map and the chamber are
/// what the King came to look at; the rail supports them and must never grow
/// into an equal column, which is what its resizer's bounds enforce.
///
/// The files rail is deliberately *not* here. It describes the city a plan is
/// working in, so it belongs to the chamber and is rendered there -- on the map
/// it had no conversation to belong to and nothing to say but an instruction to
/// go and pick a city, next to the very screen for picking one.
///
/// # Why the map is here and not on its route
///
/// [`CityMap`] hands its canvas to Bevy, and on the web `App::run()` never
/// returns -- it gives control to `requestAnimationFrame` and keeps the element
/// it resolved at startup. If the router unmounted that canvas on the way to a
/// plan and mounted a fresh one on the way back, the engine would go on drawing
/// into the detached one and the King would return to a blank map. Booting a
/// second engine is not the fix either: that would want a second winit event
/// loop inside one wasm instance.
///
/// So the map is mounted **once**, here, as a sibling of the outlet, and is
/// hidden with CSS when the route is not `/`. It costs one canvas standing idle
/// behind the chamber and buys a map that is still there when you come back.
#[component]
fn ThroneRoom() -> impl IntoView {
    let state = expect_context::<KingdomState>();
    let location = use_location();
    let on_the_map = Memo::new(move |_| location.pathname.get() == "/");

    // Which cities have agents in them. Owned here rather than in
    // `KingdomState` because nothing outside the map reads it, and polled only
    // while the map is what the King is looking at.
    let working = RwSignal::new(Vec::<CityActivity>::new());
    poll_activity(working, on_the_map);

    view! {
        <div
            class="throne-room"
            style:grid-template-columns=move || {
                let rail = if state.rail_collapsed.get() {
                    COLLAPSED_RAIL_WIDTH
                } else {
                    state.sidebar_width.get()
                };
                format!("{}px 1fr", rail)
            }
        >
            <Sidebar/>
            <main class="main-region">
                <div class="map-region" class:hidden=move || !on_the_map.get()>
                    <CityMap selected=state.selected working=working.into()/>
                </div>
                <Outlet/>
            </main>
        </div>
    }
}

/// `/` -- the decree bar, over the map standing behind it.
///
/// The map itself is mounted by [`ThroneRoom`] rather than here; see the note
/// there. What this route contributes is the bar beneath it, which is the only
/// part of the realm view that may come and go.
#[component]
fn Realm() -> impl IntoView {
    view! {
        <div class="realm-view">
            <PromptBar/>
        </div>
    }
}

/// A URL that matches nothing. Rare, but a blank screen would leave the user
/// with no way back.
#[component]
fn NoSuchPlace() -> impl IntoView {
    view! {
        <div class="empty-chamber">
            <p>"No such place in this kingdom."</p>
            <a class="back-link" href="/">"\u{2190} Back to the realm"</a>
        </div>
    }
}

/// The opening screen: choose which folder is your kingdom.
#[component]
fn ChooseKingdom() -> impl IntoView {
    let state = expect_context::<KingdomState>();
    let (path, set_path) = signal(String::new());

    // Pre-fill with a likely dev folder so the user usually just presses enter.
    let suggested = Resource::new(|| (), |_| crate::api::suggest_root());
    Effect::new(move |_| {
        if let Some(Ok(s)) = suggested.get() {
            if path.get_untracked().is_empty() {
                set_path.set(s);
            }
        }
    });

    let claim = Action::new(move |p: &String| {
        let p = p.clone();
        async move {
            state.loading.set(true);
            state.error.set(None);
            match open_kingdom(p).await {
                Ok(k) => state.kingdom.set(k),
                Err(e) => state.error.set(Some(e.to_string())),
            }
            state.loading.set(false);
        }
    });

    let submit = move || claim.dispatch(path.get());

    // Raising a proving ground is the safe path, so it gets its own action
    // rather than being something the user must first read about and then type
    // a path to reach.
    let fixtures = Resource::new(|| (), |_| crate::api::list_fixtures());
    let enter = Action::new(move |fixture: &Option<String>| {
        let fixture = fixture.clone();
        async move {
            state.loading.set(true);
            state.error.set(None);
            match crate::api::enter_proving_grounds(fixture).await {
                Ok(k) => state.kingdom.set(k),
                Err(e) => state.error.set(Some(e.to_string())),
            }
            state.loading.set(false);
        }
    });

    view! {
        <div class="choose-kingdom">
            <div class="choose-inner">
                <div class="crown-mark">"♚"</div>
                <h1>"Kingdom IDE"</h1>
                <p class="tagline">
                    "Name the folder that holds your projects. \
                     Each one becomes a city under your rule."
                </p>

                <div class="path-row">
                    <input
                        class="path-input"
                        r#type="text"
                        placeholder="/home/you/dev"
                        prop:value=move || path.get()
                        on:input=move |ev| set_path.set(event_target_value(&ev))
                        on:keydown=move |ev| {
                            if ev.key() == "Enter" { submit(); }
                        }
                    />
                    <button
                        class="claim-btn"
                        on:click=move |_| { submit(); }
                        disabled=move || state.loading.get()
                    >
                        {move || if state.loading.get() { "Surveying…" } else { "Claim Kingdom" }}
                    </button>
                </div>

                // The browser cannot hand a server a real filesystem path, so
                // the user types one. A native file dialog would need the app
                // to ship as a desktop shell.
                <p class="hint">"The server reads this path directly from disk."</p>

                <Show when=move || state.error.get().is_some()>
                    <p class="error">{move || state.error.get().unwrap_or_default()}</p>
                </Show>

                // Deliberately below the real-folder path but unmissable. The
                // screen otherwise demands a path to real work before showing
                // anything at all, which makes pointing the tool at real files
                // the default first act -- exactly what this should not be.
                <div class="proving-grounds">
                    <div class="pg-rule"><span>"or"</span></div>
                    <button
                        class="pg-btn"
                        on:click=move |_| { enter.dispatch(None); }
                        disabled=move || state.loading.get()
                    >
                        "⚔ Enter the Proving Grounds"
                    </button>
                    <p class="hint">
                        "A synthetic dev folder, generated on demand. Nothing real is \
                         touched, and the same realm comes out the same way every time."
                    </p>
                    <div class="pg-realms">
                        <Suspense>
                            {move || {
                                fixtures
                                    .get()
                                    .and_then(|r| r.ok())
                                    .map(|list| {
                                        list.into_iter()
                                            .skip(1)
                                            .map(|(name, blurb)| {
                                                let id = name.clone();
                                                view! {
                                                    <button
                                                        class="pg-realm"
                                                        title=blurb
                                                        disabled=move || state.loading.get()
                                                        on:click=move |_| {
                                                            enter.dispatch(Some(id.clone()));
                                                        }
                                                    >
                                                        {name}
                                                    </button>
                                                }
                                            })
                                            .collect_view()
                                    })
                            }}
                        </Suspense>
                    </div>
                </div>
            </div>
        </div>
    }
}
