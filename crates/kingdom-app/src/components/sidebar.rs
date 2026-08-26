//! The left rail: cities, and the plans drawn up inside each of them.
//!
//! Deliberately one list rather than a set of tabbed panels. The user's scarce
//! resource is attention, so the rail answers exactly one question — what is
//! being proposed, and where — and leaves agent status and resource contention
//! to the map, which shows them spatially.
//!
//! It is also the app's primary navigator: every row goes somewhere. A city
//! goes to the fixture with that city selected; a plan goes to its
//! conversation. A row that silently changed state on a screen that cannot show
//! the result would be a dead end.

use crate::app::{KingdomState, DEFAULT_SIDEBAR_WIDTH};
use crate::components::resizer::{restore_width, Bounds, Grows, Resizer};
use kingdom_core::{City, CityId, Plan};
use leptos::ev;
use leptos::prelude::*;
use leptos_router::components::A;
use leptos_router::hooks::{use_location, use_navigate};
use std::collections::HashSet;

/// How far the rail may be dragged. Narrower than the minimum and city names
/// are unreadable; wider than the maximum and the map stops being the hero.
const BOUNDS: Bounds = Bounds {
    min: 200.0,
    max: 560.0,
    default: DEFAULT_SIDEBAR_WIDTH,
};

const WIDTH_KEY: &str = "kingdom.sidebar_width";

#[component]
pub fn Sidebar() -> impl IntoView {
    let state = expect_context::<KingdomState>();

    // Collapse is opt-in and lives only here: it is a view preference, not
    // kingdom state, and nothing outside the rail cares about it.
    let collapsed = RwSignal::new(HashSet::<CityId>::new());

    let cities = move || state.kingdom.with(|k| k.cities.clone());
    let city_count = move || state.kingdom.with(|k| k.cities.len());

    restore_width(state.sidebar_width, WIDTH_KEY, BOUNDS);

    let collapsed_rail = move || state.rail_collapsed.get();

    // Clearing the kingdom locally is what returns the app to the opening
    // screen; the server call is what stops the next start reopening it.
    let leave = Action::new(move |(): &()| async move {
        if let Err(e) = crate::api::leave_kingdom().await {
            leptos::logging::warn!("could not leave the kingdom: {e}");
        }
        state.kingdom.set(kingdom_core::Kingdom::unopened());
    });

    view! {
        <aside class="sidebar" class:collapsed=collapsed_rail>
            // Folded away, the rail keeps exactly two things: the crown, so the
            // strip still reads as the kingdom's, and the control that brings it
            // back. It is never zero-width -- every route in the app is reached
            // from this rail, so a rail with no way back would be a dead end.
            <Show when=collapsed_rail>
                <div class="rail-strip">
                    <button
                        class="rail-toggle"
                        title="Show cities and plans"
                        on:click=move |_| state.toggle_rail()
                    >"\u{bb}"</button>
                    <div class="crown-small strip-crown">"\u{265a}"</div>
                    <div class="strip-legend">"CITIES"</div>
                </div>
            </Show>

            <Show when=move || !collapsed_rail()>
            <header class="kingdom-header">
                <div class="crown-small">"♚"</div>
                <div class="kingdom-id">
                    <div class="kingdom-name">{move || state.kingdom.with(|k| k.name.clone())}</div>
                    // A proving ground is *designed* to be indistinguishable
                    // from a real kingdom on the map, which makes an unlabelled
                    // one a trap -- for the user glancing at it, and for anyone
                    // reading a screenshot of it later. So the label sits with
                    // the kingdom's identity, not somewhere it scrolls away.
                    <Show when=move || state.kingdom.with(|k| k.sandbox)>
                        <div
                            class="sandbox-tag"
                            title="Synthetic data. Nothing in this kingdom is real work."
                        >
                            "PROVING GROUNDS"
                        </div>
                    </Show>
                    <div class="kingdom-path" title=move || state.kingdom.with(|k| k.root.clone())>
                        {move || state.kingdom.with(|k| k.root.clone())}
                    </div>
                </div>
                // The way out. A kingdom now reopens itself on every start, so
                // without this the opening screen is unreachable once one has
                // been chosen -- it shows only when nothing is open.
                <button
                    class="leave-kingdom"
                    title="Close this kingdom and choose another"
                    on:click=move |_| { leave.dispatch(()); }
                >
                    "Change"
                </button>
            </header>

            <div class="sidebar-toolbar">
                <span class="toolbar-label">
                    {move || format!("Cities {}", city_count())}
                </span>
                <div class="filter-toggle">
                    <button
                        class="filter-btn"
                        class:active=move || !state.show_all_plans.get()
                        title="Only plans still in play"
                        on:click=move |_| state.show_all_plans.set(false)
                    >"Active"</button>
                    <button
                        class="filter-btn"
                        class:active=move || state.show_all_plans.get()
                        title="Include merged and archived plans"
                        on:click=move |_| state.show_all_plans.set(true)
                    >"All"</button>
                </div>
                <button
                    class="rail-toggle"
                    title="Fold the rail away"
                    on:click=move |_| state.toggle_rail()
                >"\u{ab}"</button>
            </div>

            <div class="sidebar-body">
                <ul class="registry">
                    <For each=cities key=|c: &City| c.id.clone() let:city>
                        <CityBranch city=city collapsed=collapsed/>
                    </For>
                </ul>
            </div>

            // The handle exists only while there is a panel to size: a divider
            // on a folded rail is a control that does nothing.
            <Resizer
                width=state.sidebar_width
                grows=Grows::Rightwards
                bounds=BOUNDS
                storage_key=WIDTH_KEY
                class="sidebar-resizer"
            />
            </Show>
        </aside>
    }
}

/// One city and, beneath it, the plans drawn up there.
#[component]
fn CityBranch(city: City, collapsed: RwSignal<HashSet<CityId>>) -> impl IntoView {
    let state = expect_context::<KingdomState>();
    let id = city.id.clone();

    // Memos throughout: a plain closure capturing `id` is `FnOnce` and cannot
    // be read from two places in the view.
    let selected = {
        let id = id.clone();
        Memo::new(move |_| state.selected.get().as_ref() == Some(&id))
    };

    let plans = {
        let id = id.clone();
        Memo::new(move |_| {
            let show_all = state.show_all_plans.get();
            // `with` rather than `get`, and it matters more here than anywhere:
            // this runs once *per city*, so `get` cloned the entire kingdom --
            // every plan and every transcript in it -- once for each row in the
            // rail, on every watch-socket push. Six cities meant six full
            // copies to build six short lists.
            state.kingdom.with(|k| {
                k.plans
                    .iter()
                    // Subagents are excluded here as they are on the map: the rail
                    // is the list of what the *user* asked for, and filling it with
                    // work the model sent itself makes it worse at that job. A
                    // subagent is reached from the conversation of the plan that
                    // sent it.
                    .filter(|p| p.city == id && !p.is_subagent() && (show_all || p.is_live()))
                    .cloned()
                    .collect::<Vec<_>>()
            })
        })
    };

    let is_open = {
        let id = id.clone();
        Memo::new(move |_| !collapsed.get().contains(&id))
    };

    let toggle = {
        let id = id.clone();
        move |ev: ev::MouseEvent| {
            // The chevron sits inside the row, whose click selects the city.
            ev.stop_propagation();
            collapsed.update(|set| {
                if !set.remove(&id) {
                    set.insert(id.clone());
                }
            });
        }
    };

    let select = {
        let id = id.clone();
        let navigate = use_navigate();
        move |_| {
            state.selected.set(Some(id.clone()));
            // A city is a place on the map, so selecting one goes to the map.
            // Otherwise clicking a city from inside a conversation would change
            // a selection the user cannot see the effect of.
            navigate("/", Default::default());
        }
    };

    let has_plans = Memo::new(move |_| !plans.get().is_empty());

    // Prominence follows *live* work, not whatever the filter happens to show.
    // A city whose plans are all approved or rejected has nothing awaiting the
    // user, so it recedes even in "All" -- otherwise switching filters would
    // re-clutter the rail with settled history. The selected city never
    // recedes: the user is looking at it deliberately.
    let dormant = {
        let id = id.clone();
        Memo::new(move |_| {
            !selected.get()
                && !state
                    .kingdom
                    .get()
                    .plans
                    .iter()
                    .any(|p| p.city == id && p.is_live() && !p.is_subagent())
        })
    };

    view! {
        <li class="city-branch" class:dormant=dormant>
            <div class="city-row" class:selected=selected on:click=select>
                <button
                    class="city-chevron"
                    class:empty={move || !has_plans.get()}
                    on:click=toggle
                >
                    {move || if is_open.get() { "▾" } else { "▸" }}
                </button>
                <span class="kind-dot" style:background=city.kind.banner_color()></span>
                <span class="city-name-text" title=city.path.clone()>{city.name.clone()}</span>
                <Show when={move || has_plans.get()}>
                    <span class="plan-count">{move || plans.get().len()}</span>
                </Show>
            </div>

            <Show when={move || is_open.get() && has_plans.get()}>
                <ul class="plan-list">
                    // Keyed on what the row actually draws, not just the id: a
                    // `For` reuses a row whose key is unchanged, so keying on
                    // the id alone would leave a plan reading "Drafting" in the
                    // rail long after its conversation showed the finished
                    // draft.
                    //
                    // The proposal is in the key for the same reason and is
                    // easier to miss: a plan can go from speaking to proposing
                    // without its status moving at all -- both are
                    // `AwaitingReview` -- so without it the badge would never
                    // change to "Proposal".
                    <For
                        each={move || plans.get()}
                        key=|p: &Plan| {
                            (
                                p.id.clone(),
                                p.status,
                                p.title.clone(),
                                p.choice().label(),
                                p.standing_proposal().is_some(),
                            )
                        }
                        let:plan
                    >
                        {
                            let href = format!("/plan/{}", plan.id);
                            let current = {
                                let href = href.clone();
                                let location = use_location();
                                Memo::new(move |_| location.pathname.get() == href)
                            };
                            // Read off the plan before the view, which moves it.
                            let title = plan.title.clone();
                            let summary = plan.summary.clone();
                            let model = plan.choice().label();
                            let status = plan.status;
                            // "Awaiting review" is true both of a plan the
                            // model has merely finished speaking on and of one
                            // that has put work to the user -- but only the
                            // second is something they have to *act* on. The
                            // rail is where they scan for that, so it says
                            // which is which.
                            //
                            // A label, not a sixth `PlanStatus`: nothing about
                            // the state machine changed, and the badge keeps its
                            // colour.
                            let badge = match plan.standing_proposal() {
                                Some(_) => "Proposal",
                                None => status.label(),
                            };
                            view! {
                                <li>
                                    <A href=href attr:class="plan-row" attr:title=summary>
                                        <span class="plan-row-inner" class:current=current>
                                            <span class="plan-title">{title}</span>
                                            <span class="plan-model">{model}</span>
                                            <span class=format!(
                                                "plan-badge plan-{}",
                                                status.css_suffix(),
                                            )>
                                                {badge}
                                            </span>
                                        </span>
                                    </A>
                                </li>
                            }
                        }
                    </For>
                </ul>
            </Show>
        </li>
    }
}
