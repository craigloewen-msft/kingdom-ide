//! The application shell and root component.

use crate::api::{get_kingdom, open_kingdom};
use crate::components::{ChatDock, KingdomMap, Sidebar};
use kingdom_core::{CityId, Kingdom, ModelChoice};
use leptos::prelude::*;
use leptos_meta::{provide_meta_context, MetaTags, Stylesheet, Title};

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

/// Where the last-used model and effort are remembered between visits.
#[cfg(feature = "hydrate")]
const MODEL_KEY: &str = "kingdom.model";
#[cfg(feature = "hydrate")]
const EFFORT_KEY: &str = "kingdom.effort";

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
    /// What the next new plan will be drafted with. `None` until the King has
    /// chosen or a remembered choice has been restored, at which point the
    /// server's catalogue default applies.
    pub choice: RwSignal<Option<ModelChoice>>,
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
            choice: RwSignal::new(None),
        }
    }

    /// Records the King's choice and remembers it for next time.
    pub fn choose_model(&self, choice: ModelChoice) {
        store_choice(&choice);
        self.choice.set(Some(choice));
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
/// it was stored degrades rather than failing a decree.
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
            None => {
                let _ = storage.remove_item(EFFORT_KEY);
            }
        }
    }

    #[cfg(not(feature = "hydrate"))]
    let _ = choice;
}

/// Root component: the throne room.
#[component]
pub fn App() -> impl IntoView {
    provide_meta_context();

    let state = KingdomState::new();
    provide_context(state);
    restore_choice(state.choice);

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
            <div
                class="throne-room"
                style:grid-template-columns=move || {
                    format!("{}px 1fr", state.sidebar_width.get())
                }
            >
                <Sidebar/>
                <main class="map-region">
                    <KingdomMap/>
                </main>
                <ChatDock/>
            </div>
        </Show>
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
