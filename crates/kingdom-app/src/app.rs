//! The application shell and root component.

use crate::api::{get_kingdom, open_kingdom};
use crate::components::{Conversation, PromptBar, KingdomMap, Sidebar, WardTree};
use kingdom_core::{CityId, Kingdom, ModelChoice, WorkspaceMode};
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

/// Width the files rail opens at. Narrower than the rail by default and capped
/// lower (see `ward_tree.rs`): a tree of names needs less room than a rail of
/// titles and badges, and neither rail may become the widest thing on screen.
pub const DEFAULT_TREE_WIDTH: f64 = 240.0;

/// What the rail collapses *to*. Never zero: the rail is this app's entire
/// navigation, so a collapsed one that could not be reopened would be a dead
/// end. The strip is wide enough to hold the button that brings it back.
pub const COLLAPSED_RAIL_WIDTH: f64 = 34.0;

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
    pub tree_width: RwSignal<f64>,
    /// Whether the cities rail is folded away to a strip.
    ///
    /// A view preference and nothing more, but it lives here rather than in
    /// `sidebar.rs` because `ThroneRoom` is what writes the grid track and so
    /// has to read it.
    pub rail_collapsed: RwSignal<bool>,
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
    pub fn toggle_rail(&self) {
        let next = !self.rail_collapsed.get_untracked();
        self.rail_collapsed.set(next);
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
fn restore_rail_collapsed(collapsed: RwSignal<bool>) {
    #[cfg(feature = "hydrate")]
    Effect::new(move |_| {
        if let Some(stored) =
            local_storage().and_then(|s| s.get_item(RAIL_COLLAPSED_KEY).ok().flatten())
        {
            collapsed.set(stored == "1");
        }
    });

    #[cfg(not(feature = "hydrate"))]
    let _ = collapsed;
}

fn store_rail_collapsed(collapsed: bool) {
    #[cfg(feature = "hydrate")]
    if let Some(storage) = local_storage() {
        let _ = storage.set_item(RAIL_COLLAPSED_KEY, if collapsed { "1" } else { "0" });
    }

    #[cfg(not(feature = "hydrate"))]
    let _ = collapsed;
}

/// Root component: the throne room.
#[component]
pub fn App() -> impl IntoView {
    provide_meta_context();

    let state = KingdomState::new();
    provide_context(state);
    restore_choice(state.choice);
    restore_workspace(state.workspace);
    restore_rail_collapsed(state.rail_collapsed);

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
            when=move || state.kingdom.get().is_open()
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

/// The frame both screens hang in: the two rails, and whichever view is routed.
///
/// Three tracks, and the proportion between them is the point: the rails are
/// **fixed** widths and the main region takes `1fr`. The map and the chamber are
/// what the King came to look at; the rails support them and must never grow
/// into equal columns, which is what their own resizers' bounds enforce.
#[component]
fn ThroneRoom() -> impl IntoView {
    let state = expect_context::<KingdomState>();

    view! {
        <div
            class="throne-room"
            style:grid-template-columns=move || {
                let rail = if state.rail_collapsed.get() {
                    COLLAPSED_RAIL_WIDTH
                } else {
                    state.sidebar_width.get()
                };
                format!("{}px {}px 1fr", rail, state.tree_width.get())
            }
        >
            <Sidebar/>
            <WardTree/>
            <main class="main-region">
                <Outlet/>
            </main>
        </div>
    }
}

/// `/` -- the whole fixture, with the prompt bar beneath it.
#[component]
fn Realm() -> impl IntoView {
    view! {
        <div class="realm-view">
            <div class="map-region">
                <KingdomMap/>
            </div>
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
