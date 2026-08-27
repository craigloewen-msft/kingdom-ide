//! The application shell and root component.

use crate::api::{get_kingdom, open_kingdom};
use crate::components::{Conversation, PromptBar, Sidebar};
use kingdom_citymap::map::MapPresence;
use kingdom_citymap::CityMap;
use kingdom_core::{
    Attention, CityActivity, CityId, Kingdom, ModelChoice, NetworkMode, Plan, WorkspaceMode,
};
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

/// How tall the map pane at the foot of the cities rail opens.
///
/// Enough to read a town's shape and see which of them are alight, without
/// taking the rail over from the list of plans it exists for. The registry
/// above takes whatever is left, so this is the only number behind the split.
pub const DEFAULT_MAP_HEIGHT: f64 = 260.0;

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
/// Where the last-used network mode is remembered.
///
/// Its own key rather than a field of the workspace's, because it is its own
/// axis -- see [`kingdom_core::NetworkMode`].
#[cfg(feature = "hydrate")]
const NETWORK_KEY: &str = "kingdom.network";

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
    /// How tall the map pane at the foot of the cities rail is, in pixels.
    ///
    /// Lives here rather than in `sidebar.rs` for the reason `tree_width` does
    /// -- a height the King dragged must survive moving between plans -- and
    /// for a second one: [`ThroneRoom`] reads it too. The map's canvas is not
    /// *inside* the rail, it is an overlay positioned over it, and this is the
    /// one number both the rail's empty slot and that overlay are cut from.
    /// Two numbers would be two rectangles free to disagree.
    pub map_height: RwSignal<f64>,
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
    /// Whether the next plan gets a network of its own. A separate axis from
    /// `workspace`; see [`kingdom_core::NetworkMode`].
    pub network: RwSignal<NetworkMode>,
    /// The file the chamber's panel is showing, relative to the plan's city.
    ///
    /// Written by the chamber and read by the map, which is why it is here: the
    /// conversation renders inside the router's outlet and the map is mounted
    /// outside it, so a shared signal is the only seam between them. Exactly
    /// what `selected` already is, and it sits beside it for that reason.
    ///
    /// `None` when no file is open. The rail's map deliberately does not pull
    /// back when that happens -- see `CityMap`.
    pub focus_file: RwSignal<Option<String>>,
    /// The file the King picked by pressing its building on the rail's map.
    ///
    /// [`Self::focus_file`]'s return leg, and it sits beside it for that
    /// reason: that one carries what the chamber has open *out* to the map, and
    /// this carries what he pressed on the map *back*. Same seam, same reason
    /// for being here -- the map is mounted outside the router's outlet (see
    /// [`ThroneRoom`]) and cannot be handed a chamber's callback.
    ///
    /// Written by the map only while it stands in the rail, and only for a file
    /// of the city the open plan works in. Read by the chamber, which opens the
    /// file and then **clears this**: it is a message rather than a state, and
    /// leaving it set would mean the same building could not be pressed twice.
    pub picked_file: RwSignal<Option<String>>,
    /// What every live agent in the selected city is changing, published where
    /// the map can draw it.
    ///
    /// Here for exactly [`Self::focus_file`]'s reason, and it sits beside it for
    /// that reason: the chamber renders inside the router's outlet, the map is
    /// mounted outside it (see [`ThroneRoom`]), and a shared signal is the only
    /// seam between them.
    ///
    /// **Several plans rather than one, which is what makes the map answer
    /// "who".** It used to be the open plan's summary alone, so a city with
    /// three agents in it drew one agent's work and silently omitted the rest --
    /// and with one green for every addition there was nothing to tell them
    /// apart even in principle. Each entry carries its plan, and
    /// `kingdom_core::palette` turns that into the colours the works are drawn
    /// in.
    ///
    /// Ordered by plan id, which is load-bearing: a banner collision is
    /// resolved by position, so an unstable order would let two agents swap
    /// colours between refetches. `api::city_changes` sorts.
    ///
    /// Empty when nothing is live, which is what tears the works down.
    pub works: RwSignal<Vec<(kingdom_core::PlanId, kingdom_core::ChangeSummary)>>,
    /// What each plan wants of the King, as the server last said.
    ///
    /// A cache beside the kingdom rather than a field on `Plan`, and the reason
    /// is which channel carries it. A plan's own transcript answers this too --
    /// [`kingdom_core::Plan::wants_attention`] reads it -- but the transcript
    /// only arrives on the *chamber's* socket, so a browser sitting on the map
    /// would never learn that a plan had stopped to ask something. The rail's
    /// socket carries the answer directly.
    ///
    /// Written by both sockets, so the two cannot disagree: the pulse writes
    /// what the server computed, and the chamber writes what it computes from
    /// the plan it was just handed. Read through
    /// [`KingdomState::attention_of`], which falls back to the plan itself for
    /// a plan neither socket has spoken about yet -- the opening fetch, most
    /// often, which is a plain HTTP response carrying no pulse at all.
    ///
    /// The value is an `Option` inside the map, and that is load-bearing rather
    /// than sloppy. "The server says this plan wants nothing" and "nothing has
    /// been said about this plan" have to stay different answers: a plan whose
    /// question was answered in another tab pulses `None`, and if that were
    /// stored as an *absent* entry the badge would fall back to a transcript
    /// fetched before the answer and go on showing a question nobody is asking.
    pub attention: RwSignal<std::collections::HashMap<kingdom_core::PlanId, Option<Attention>>>,
}

impl KingdomState {
    fn new() -> Self {
        Self {
            kingdom: RwSignal::new(Kingdom::unopened()),
            selected: RwSignal::new(None),
            sidebar_width: RwSignal::new(DEFAULT_SIDEBAR_WIDTH),
            tree_width: RwSignal::new(DEFAULT_TREE_WIDTH),
            map_height: RwSignal::new(DEFAULT_MAP_HEIGHT),
            rail_collapsed: RwSignal::new(false),
            rail_preference: RwSignal::new(false),
            rail_decided_at: RwSignal::new(None),
            show_all_plans: RwSignal::new(false),
            loading: RwSignal::new(false),
            error: RwSignal::new(None),
            choice: RwSignal::new(None),
            workspace: RwSignal::new(WorkspaceMode::default()),
            network: RwSignal::new(NetworkMode::default()),
            focus_file: RwSignal::new(None),
            picked_file: RwSignal::new(None),
            works: RwSignal::new(Vec::new()),
            attention: RwSignal::new(std::collections::HashMap::new()),
        }
    }

    /// What a plan wants of the King, however this browser came to know it.
    ///
    /// The cache first, because it is the only half a browser away from the
    /// chamber ever hears; the plan's own transcript second, so a kingdom just
    /// fetched over HTTP still badges correctly before any socket has spoken.
    ///
    /// Note the double `Option`: an entry holding `None` is the server saying
    /// this plan wants nothing, and it wins over the plan's own -- possibly
    /// stale -- transcript. Only a genuinely *missing* entry falls back.
    pub fn attention_of(&self, plan: &Plan) -> Option<Attention> {
        match self.attention.with(|known| known.get(&plan.id).copied()) {
            Some(said) => said,
            None => plan.wants_attention(),
        }
    }

    /// Records what the server says a plan wants, from either socket.
    pub fn note_attention(&self, plan: &kingdom_core::PlanId, needs: Option<Attention>) {
        // Written even when nothing is wanted -- see the field's docs. Cheap:
        // one entry per plan, and the rail is the only reader.
        self.attention.update(|known| {
            known.insert(plan.clone(), needs);
        });
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

    /// Records whether the next plan gets a network of its own.
    pub fn choose_network(&self, mode: NetworkMode) {
        store_network(&mode);
        self.network.set(mode);
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

/// Restores the remembered network mode, in the same place and for the same
/// reason as [`restore_workspace`].
fn restore_network(mode: RwSignal<NetworkMode>) {
    #[cfg(feature = "hydrate")]
    Effect::new(move |_| {
        let Some(storage) = local_storage() else {
            return;
        };
        let Some(stored) = storage.get_item(NETWORK_KEY).ok().flatten() else {
            return;
        };
        // Anything unrecognised means shared. The default is deliberately the
        // *un*isolated one here, the opposite of the workspace's: a network
        // namespace needs slirp4netns, and a machine that has since lost it
        // should open on the mode that always works rather than on one the
        // server would refuse.
        mode.set(match stored.as_str() {
            "isolated" => NetworkMode::Isolated,
            _ => NetworkMode::Shared,
        });
    });

    #[cfg(not(feature = "hydrate"))]
    let _ = mode;
}

fn store_network(mode: &NetworkMode) {
    #[cfg(feature = "hydrate")]
    if let Some(storage) = local_storage() {
        let _ = storage.set_item(
            NETWORK_KEY,
            match mode {
                NetworkMode::Shared => "shared",
                NetworkMode::Isolated => "isolated",
            },
        );
    }

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

/// Which cities have agents working in them, derived from the kingdom this
/// browser already holds.
///
/// This replaced a two-second poll of `api::kingdom_activity`, and the change
/// was forced rather than opportunistic: the poll deliberately stopped whenever
/// the map was off screen, so the map now standing in the rail beside every
/// conversation would have shown rings frozen at the moment the King left `/`.
///
/// It turns out nothing had to be plumbed. [`Kingdom::activity`] is a pure
/// method that compiles to wasm, and the browser is already told everything it
/// needs: the rail's socket carries a [`kingdom_core::PlanPulse`] per plan,
/// `absorb` writes its `working_on` onto the kingdom, and `Plan::is_busy` is
/// `working_on.is_some()`. So this is push rather than poll, with no lag, no
/// request while idle, and one answer on every screen instead of one screen's
/// answer.
///
/// A `Memo` and not a closure, and that is what makes it cheap: it re-runs on
/// every push, but `CityActivity` derives `PartialEq`, so a run that finds the
/// same set notifies nobody -- and a transcript entry that changes no city's
/// busy count therefore does not re-send `SetActivity` to the engine.
fn kingdom_activity(state: KingdomState) -> Memo<Vec<CityActivity>> {
    // `with`, not `get`: answering this should not clone every city and every
    // plan in the kingdom.
    Memo::new(move |_| state.kingdom.with(Kingdom::activity))
}

/// Keeps [`KingdomState::works`] current for whichever city is selected.
///
/// # Why this is push-driven rather than polled
///
/// The rail's socket already carries a `PlanPulse` for every plan whenever it
/// moves, and `absorb` writes that onto the kingdom this browser holds. So
/// "has any agent in this city done anything?" is a question already answered
/// locally, for free, at exactly the moment it changes -- a timer would be both
/// slower and more expensive. This is the same argument `kingdom_activity`
/// above records for the working rings, applied to the works.
///
/// # What it is keyed on
///
/// A digest -- the city, and each live plan's id and `working_on` -- rather than
/// the kingdom itself. The kingdom signal is re-set wholesale on every push,
/// including pushes about plans in other cities and about transcript entries
/// that touched no file, and refetching a city's git state for those would be a
/// request per round of every turn in the kingdom. A `Memo` over a small
/// `PartialEq` value means the fetch runs when an agent here actually did
/// something.
///
/// The guard is `fetching`, the same shape `review_drawer::fetch_changes` uses
/// and for its reason: an answer that arrives while one is in flight is dropped
/// rather than queued, because the next pulse will ask again in a moment and
/// two overlapping answers could land out of order.
fn watch_city_works(state: KingdomState) {
    // What would make the answer different. `working_on` is in here because it
    // moves on every deed, which is the cheapest honest proxy for "this agent
    // may have just touched a file"; the id list covers a plan opening,
    // settling or being deleted.
    let stirring = Memo::new(move |_| {
        let Some(city) = state.selected.get() else {
            return None;
        };
        let plans = state.kingdom.with(|k| {
            k.plans_in(&city)
                .filter(|plan| plan.is_live())
                .map(|plan| (plan.id.clone(), plan.working_on.clone()))
                .collect::<Vec<_>>()
        });
        Some((city, plans))
    });

    let fetching = RwSignal::new(false);

    Effect::new(move |_| {
        let Some((city, _)) = stirring.get() else {
            // No city selected: nothing to draw, and an empty list is how the
            // works are torn down.
            state.works.set(Vec::new());
            return;
        };
        if fetching.get_untracked() {
            return;
        }
        fetching.set(true);

        leptos::task::spawn_local(async move {
            if let Ok(found) = crate::api::city_changes(city.to_string()).await {
                // A failed fetch deliberately leaves the last good answer
                // standing, exactly as the review drawer's does: blanking the
                // map because one request was dropped is a worse answer than a
                // slightly stale one.
                state.works.set(found);
            }
            fetching.set(false);
        });
    });
}

/// One agent's line in the rail map's header: who, how much, and its banner.
///
/// A named alias because it is the key of a `<For>` as well as its item, and
/// spelling a five-tuple twice in a view is how the two come to disagree.
type AgentTally = (
    kingdom_core::PlanId,
    u32,
    u32,
    usize,
    &'static kingdom_core::AgentPalette,
);

/// Root component: the throne room.
#[component]
pub fn App() -> impl IntoView {
    provide_meta_context();

    let state = KingdomState::new();
    provide_context(state);
    restore_choice(state.choice);
    restore_workspace(state.workspace);
    restore_network(state.network);
    restore_rail_collapsed(state.rail_collapsed, state.rail_preference);
    // After the restore, so "enough room again" gives back the King's own
    // preference rather than the default standing in for it.
    fold_rail_when_cramped(
        state.rail_collapsed,
        state.rail_preference,
        state.rail_decided_at,
    );

    // The rail's own channel, opened once for the life of the app. It is what
    // lets a plan that has stopped to ask something say so while the King is
    // looking at the map or at another chamber entirely.
    watch_kingdom(state);

    // What every agent in the selected city is changing, kept current off that
    // same socket. Mounted here rather than in the chamber because the map
    // outlives any one conversation -- and because the works are now about a
    // city rather than about whichever plan happens to be open.
    watch_city_works(state);

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
/// # Why the map is here, and why it is a sibling of both
///
/// [`CityMap`] hands its canvas to Bevy, and on the web `App::run()` never
/// returns -- it gives control to `requestAnimationFrame` and keeps the element
/// it resolved at startup. If the router unmounted that canvas on the way to a
/// plan and mounted a fresh one on the way back, the engine would go on drawing
/// into the detached one and the King would return to a blank map. Booting a
/// second engine is not the fix either: that would want a second winit event
/// loop inside one wasm instance.
///
/// So the map is mounted **once**, here, and it is a direct child of the grid
/// rather than of either track. That is what lets it be shown in two places
/// without being rendered twice: it does not move through the DOM, it is
/// positioned, and only the four numbers describing its rectangle change.
///
/// - On `/` it covers the main region, exactly as it always did.
/// - In a chamber it stands at the foot of the cities rail, scoped to the city
///   that conversation is about.
/// - With the rail folded away in a chamber it has no home, and goes.
///
/// Every number in both rectangles is arithmetic over signals the grid track
/// itself is written from, so the overlay and the column agree by construction
/// rather than by measurement.
///
/// A home is not the same as a frame rate, though, so [`MapPresence`] is handed
/// to the map as well as to the class: CSS decides which pixels the King sees,
/// and the prop is what decides how much work his machine does producing them.
#[component]
fn ThroneRoom() -> impl IntoView {
    let state = expect_context::<KingdomState>();
    let location = use_location();
    let on_the_map = Memo::new(move |_| location.pathname.get() == "/");

    // Which cities have agents in them. Derived from the kingdom the rail's
    // socket already keeps current -- see `kingdom_activity`, which replaced a
    // poll that stopped whenever the map was off screen.
    let working = kingdom_activity(state);

    // Where the map stands. The single answer both the class and the engine are
    // driven from, so the rectangle it is drawn in and the effort it spends
    // drawing cannot disagree.
    let presence = Memo::new(move |_| {
        if on_the_map.get() {
            MapPresence::Full
        } else if state.rail_collapsed.get() {
            // A chamber on a narrow window folds the rail on its own
            // (`fold_rail_when_cramped`), so this is the ordinary laptop case
            // rather than a corner: no room for a map, and the engine stops
            // exactly as it did before there was a second home.
            MapPresence::Hidden
        } else {
            MapPresence::Rail
        }
    });

    // How wide the rail's track currently is. Read by the grid and by the
    // overlay's rectangle both, which is what keeps the two in step.
    let rail_width = move || {
        if state.rail_collapsed.get() {
            COLLAPSED_RAIL_WIDTH
        } else {
            state.sidebar_width.get()
        }
    };

    view! {
        <div
            class="throne-room"
            style:grid-template-columns=move || format!("{}px 1fr", rail_width())
        >
            // First among the grid's children, and positioned rather than laid
            // out. Paint order is settled by `z-index` in `_city-map.scss`
            // rather than by this position: in the rail it lifts above the
            // sidebar, and on its own screen it stays under the main region so
            // the decree bar still stands over it.
            <div
                class="map-region"
                class:at-rail=move || presence.get() == MapPresence::Rail
                class:at-large=move || presence.get() == MapPresence::Full
                class:gone=move || presence.get() == MapPresence::Hidden
                style:left=move || {
                    match presence.get() {
                        MapPresence::Full => format!("{}px", rail_width()),
                        _ => "0".to_owned(),
                    }
                }
                style:width=move || {
                    match presence.get() {
                        MapPresence::Full => "auto".to_owned(),
                        _ => format!("{}px", rail_width()),
                    }
                }
                style:height=move || {
                    match presence.get() {
                        MapPresence::Full => "auto".to_owned(),
                        // The same signal the rail's own empty slot is cut
                        // from, so the canvas lands exactly where the registry
                        // stops. See `KingdomState::map_height`.
                        _ => format!("{}px", state.map_height.get()),
                    }
                }
            >
                <CityMap
                    selected=state.selected
                    working=working
                    presence=presence
                    focus_city=state.selected
                    focus_file=state.focus_file
                    picked_file=state.picked_file
                    works=state.works
                />
                // The rail's map is a view rather than a control -- clicking it
                // cannot select, because the chamber force-sets the selection
                // from the open plan -- so the way to the real map has to be
                // drawn. Rendered inside the region so the chrome travels with
                // the rectangle instead of being a second thing to align.
                <Show when=move || presence.get() == MapPresence::Rail>
                    <div class="map-rail-head">
                        <span class="rail-pane-label">"Kingdom"</span>
                        <span class="map-rail-city">
                            {move || {
                                state
                                    .selected
                                    .get()
                                    .map(|id| id.to_string())
                                    .unwrap_or_default()
                            }}
                        </span>
                        // What the works on the map add up to, per agent.
                        //
                        // The map says *where* each agent is working and how
                        // much; this says *who* they are and totals it, which
                        // is the one thing geometry is bad at -- a column is a
                        // proportion, not a number. One chip per agent, in that
                        // agent's own two colours, so the hues standing on the
                        // map have a key sitting beside them.
                        <Show when=move || {
                            state.works.with(|w| w.iter().any(|(_, s)| !s.files.is_empty()))
                        }>
                            <span class="map-rail-works">
                                <For
                                    each=move || {
                                        state.works.with(|w| {
                                            let plans: Vec<_> =
                                                w.iter().map(|(id, _)| id.clone()).collect();
                                            let banners =
                                                kingdom_core::palette::assign_banners(&plans);
                                            w.iter()
                                                .zip(banners)
                                                .filter(|((_, s), _)| !s.files.is_empty())
                                                .map(|((id, s), (_, banner))| {
                                                    (
                                                        id.clone(),
                                                        s.added(),
                                                        s.removed(),
                                                        s.files.len(),
                                                        banner,
                                                    )
                                                })
                                                .collect::<Vec<_>>()
                                        })
                                    }
                                    key=|entry: &AgentTally| {
                                        (entry.0.clone(), entry.1, entry.2, entry.3)
                                    }
                                    let:entry
                                >
                                    {
                                        let (id, added, removed, files, banner) = entry;
                                        let title = state.kingdom.with(|k| {
                                            let name = k
                                                .plan(&id)
                                                .map(|p| p.title.clone())
                                                .unwrap_or_else(|| id.to_string());
                                            format!(
                                                "{name} \u{2014} {files} files, +{added} \
                                                 \u{2212}{removed} ({})",
                                                banner.name,
                                            )
                                        });
                                        view! {
                                            <span class="map-rail-agent" title=title>
                                                <span
                                                    class="agent-pip"
                                                    style:background=banner.growth
                                                ></span>
                                                <span
                                                    class="count-added"
                                                    style:color=banner.growth
                                                >
                                                    "+"{added}
                                                </span>
                                                <span
                                                    class="count-removed"
                                                    style:color=banner.cutting
                                                >
                                                    "\u{2212}"{removed}
                                                </span>
                                            </span>
                                        }
                                    }
                                </For>
                            </span>
                        </Show>
                        <a class="map-rail-open" href="/" title="Open the whole kingdom">
                            "\u{2197}"
                        </a>
                    </div>
                </Show>
            </div>

            <Sidebar/>
            <main class="main-region">
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

/// Keeps the rail in step with every plan in the kingdom, not just the one on
/// screen.
///
/// The counterpart to `conversation.rs::watch_plan`, and deliberately the same
/// shape: a socket owned by an effect, a fixed-delay reconnect with no backoff
/// ladder and no give-up, and a drop that cancels the retry. What differs is
/// what it carries -- a list of [`kingdom_core::PlanPulse`] rather than a plan
/// -- and why it exists at all: the chamber's socket structurally cannot report
/// a plan whose chamber is closed, and "which of my agents needs me?" is a
/// question about exactly those.
///
/// Mounted once, by [`App`], and never unmounted. Browser-only: under SSR there
/// is no socket, and the first render is served from server state.
fn watch_kingdom(state: KingdomState) {
    #[cfg(feature = "hydrate")]
    Effect::new(move |previous: Option<KingdomWatch>| KingdomWatch::open(state, previous));

    #[cfg(not(feature = "hydrate"))]
    let _ = state;
}

/// Applies one pulse: the badge cache first, then whatever else moved.
///
/// Returns false for a plan this browser does not hold, which the caller reads
/// as "refetch". A pulse is deliberately too small to invent a plan from -- see
/// [`kingdom_core::PlanPulse`] -- and half a plan in the rail would be a row
/// leading to an empty chamber.
#[cfg(feature = "hydrate")]
fn absorb(state: KingdomState, pulse: &kingdom_core::PlanPulse) -> bool {
    // The attention is recorded whether or not the plan is known. It costs one
    // map entry and it means the badge is already right at the instant the
    // refetch below lands, rather than one push later.
    state.note_attention(&pulse.id, pulse.needs);
    state
        .kingdom
        .try_update(|k| k.apply(pulse))
        .unwrap_or(false)
}

/// An open watch on the whole kingdom, which closes itself when dropped.
#[cfg(feature = "hydrate")]
struct KingdomWatch {
    socket: web_sys::WebSocket,
    /// Kept alive for the socket's lifetime: a closure handed to JS and then
    /// dropped on the Rust side would be called after being freed. Same
    /// reasoning as `PlanWatch` in `conversation.rs`.
    _on_message: wasm_bindgen::closure::Closure<dyn FnMut(web_sys::MessageEvent)>,
    _on_close: wasm_bindgen::closure::Closure<dyn FnMut()>,
    /// Cleared on drop, so a retry queued by a closing socket does not reopen
    /// a watch nothing owns.
    retry: std::rc::Rc<std::cell::Cell<Option<i32>>>,
}

#[cfg(feature = "hydrate")]
impl KingdomWatch {
    /// How long to wait before reopening a dropped socket. The server is on
    /// loopback, so a dropped socket means it is restarting.
    const RETRY_MS: i32 = 1000;

    fn open(state: KingdomState, previous: Option<Self>) -> Self {
        use wasm_bindgen::closure::Closure;
        use wasm_bindgen::JsCast;

        // Before opening another, so a re-run cannot leave a socket behind.
        drop(previous);

        let socket = web_sys::WebSocket::new(&Self::url())
            .expect("the rail's watch socket should be constructible");

        let on_message = Closure::<dyn FnMut(web_sys::MessageEvent)>::new(
            move |event: web_sys::MessageEvent| {
                let Some(text) = event.data().as_string() else {
                    return;
                };
                // A message that will not parse means the server sent a shape
                // this bundle does not know -- a stale tab after a rebuild.
                // Dropping it leaves the rail showing the last good state.
                let Ok(pulses) = serde_json::from_str::<Vec<kingdom_core::PlanPulse>>(&text) else {
                    return;
                };

                // Every message is a list, including a single-plan update, so
                // there is one code path here rather than two. See `watch.rs`.
                let unknown = pulses.iter().filter(|p| !absorb(state, p)).count();

                // A plan this browser has never seen -- opened in another tab.
                // One refetch teaches it about all of them at once, and it
                // cannot loop: after it lands, the ids are known.
                if unknown > 0 {
                    leptos::task::spawn_local(async move {
                        if let Ok(k) = crate::api::get_kingdom().await {
                            state.kingdom.set(k);
                        }
                    });
                }
            },
        );
        socket.set_onmessage(Some(on_message.as_ref().unchecked_ref()));

        let retry = std::rc::Rc::new(std::cell::Cell::new(None));
        let on_close = Closure::<dyn FnMut()>::new({
            let retry = retry.clone();
            move || {
                let Some(window) = web_sys::window() else {
                    return;
                };
                let reopen = Closure::once_into_js({
                    let retry = retry.clone();
                    move || {
                        retry.set(None);
                        // Deliberately leaked, exactly as `PlanWatch`'s retry
                        // is: the reopened watch outlives this callback and has
                        // no owner to hand it back to. Bounded by the number of
                        // disconnects in one visit, and the socket it holds is
                        // closed by the browser when the page goes.
                        std::mem::forget(KingdomWatch::open(state, None));
                    }
                });
                if let Ok(handle) = window.set_timeout_with_callback_and_timeout_and_arguments_0(
                    reopen.unchecked_ref(),
                    Self::RETRY_MS,
                ) {
                    retry.set(Some(handle));
                }
            }
        });
        socket.set_onclose(Some(on_close.as_ref().unchecked_ref()));

        Self {
            socket,
            _on_message: on_message,
            _on_close: on_close,
            retry,
        }
    }

    /// The socket's address, derived from the page's own origin so it follows
    /// the server wherever it is served from.
    fn url() -> String {
        let location = web_sys::window()
            .expect("a browser has a window")
            .location();
        let secure = location.protocol().map(|p| p == "https:").unwrap_or(false);
        let host = location.host().unwrap_or_default();
        let scheme = if secure { "wss" } else { "ws" };
        format!("{scheme}://{host}{}", crate::watch::KINGDOM_ROUTE)
    }
}

#[cfg(feature = "hydrate")]
impl Drop for KingdomWatch {
    fn drop(&mut self) {
        // Order matters: clear the close handler before closing, or closing
        // deliberately would schedule the reconnect this drop exists to stop.
        self.socket.set_onclose(None);
        self.socket.set_onmessage(None);
        let _ = self.socket.close();

        if let (Some(handle), Some(window)) = (self.retry.take(), web_sys::window()) {
            window.clear_timeout_with_handle(handle);
        }
    }
}
