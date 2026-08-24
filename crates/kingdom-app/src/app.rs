//! The application shell and root component.

use crate::api::{get_kingdom, open_kingdom};
use crate::components::{Conversation, DecreeBar, KingdomMap, Sidebar};
use kingdom_core::{CityId, Kingdom};
use leptos::prelude::*;
use leptos_meta::{provide_meta_context, MetaTags, Stylesheet, Title};
use leptos_router::components::{Outlet, ParentRoute, Route, Router, Routes};
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

/// Shared UI state, provided via context so the three regions stay in sync
/// without threading props through every layer.
#[derive(Clone, Copy)]
pub struct KingdomState {
    /// The kingdom as last loaded from the server.
    pub kingdom: RwSignal<Kingdom>,
    /// The city the King has selected, if any. Highlighted on the map and
    /// used as the target for a new decree.
    pub selected: RwSignal<Option<CityId>>,
    /// Current width of the left rail, in pixels. Driven by the resizer.
    pub sidebar_width: RwSignal<f64>,
    /// False shows only live plans; true also shows settled history.
    pub show_all_plans: RwSignal<bool>,
    /// Set while a folder scan is in flight.
    pub loading: RwSignal<bool>,
    /// Most recent error, shown in the chat dock.
    pub error: RwSignal<Option<String>>,
}

impl KingdomState {
    fn new() -> Self {
        Self {
            kingdom: RwSignal::new(Kingdom::unopened()),
            selected: RwSignal::new(None),
            sidebar_width: RwSignal::new(DEFAULT_SIDEBAR_WIDTH),
            show_all_plans: RwSignal::new(false),
            loading: RwSignal::new(false),
            error: RwSignal::new(None),
        }
    }
}

/// Root component: the throne room.
#[component]
pub fn App() -> impl IntoView {
    provide_meta_context();

    let state = KingdomState::new();
    provide_context(state);

    // Load any kingdom the server already has open, so a refresh does not
    // send the King back to the folder picker.
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
            when=move || state.kingdom.get().is_open()
            fallback=move || view! { <ChooseKingdom/> }
        >
            // The rail lives on the parent route so it never unmounts: moving
            // between the realm and a plan's chamber is then instant, and the
            // rail keeps its scroll position and collapse state across the move.
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

/// The frame both screens hang in: the rail, and whichever view is routed.
#[component]
fn ThroneRoom() -> impl IntoView {
    let state = expect_context::<KingdomState>();

    view! {
        <div
            class="throne-room"
            style:grid-template-columns=move || {
                format!("{}px 1fr", state.sidebar_width.get())
            }
        >
            <Sidebar/>
            <main class="main-region">
                <Outlet/>
            </main>
        </div>
    }
}

/// `/` -- the whole realm, with the decree bar beneath it.
#[component]
fn Realm() -> impl IntoView {
    view! {
        <div class="realm-view">
            <div class="map-region">
                <KingdomMap/>
            </div>
            <DecreeBar/>
        </div>
    }
}

/// A URL that matches nothing. Rare, but a blank screen would leave the King
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

    // Pre-fill with a likely dev folder so the King usually just presses enter.
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
                // the King types one. A native file dialog would need the app
                // to ship as a desktop shell.
                <p class="hint">"The server reads this path directly from disk."</p>

                <Show when=move || state.error.get().is_some()>
                    <p class="error">{move || state.error.get().unwrap_or_default()}</p>
                </Show>
            </div>
        </div>
    }
}
