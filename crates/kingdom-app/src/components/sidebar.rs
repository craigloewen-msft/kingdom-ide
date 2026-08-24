//! The left rail: cities, and the plans drawn up inside each of them.
//!
//! Deliberately one list rather than a set of tabbed panels. The King's scarce
//! resource is attention, so the rail answers exactly one question — what is
//! being proposed, and where — and leaves agent status and resource contention
//! to the map, which shows them spatially.

use crate::app::{KingdomState, DEFAULT_SIDEBAR_WIDTH};
use kingdom_core::{City, CityId, Plan, PlanStatus};
use leptos::ev;
use leptos::prelude::*;
use std::collections::HashSet;

/// How far the rail may be dragged. Narrower than the minimum and city names
/// are unreadable; wider than the maximum and the map stops being the hero.
const MIN_WIDTH: f64 = 200.0;
const MAX_WIDTH: f64 = 560.0;

#[cfg(feature = "hydrate")]
const WIDTH_KEY: &str = "kingdom.sidebar_width";

#[component]
pub fn Sidebar() -> impl IntoView {
    let state = expect_context::<KingdomState>();

    // Collapse is opt-in and lives only here: it is a view preference, not
    // kingdom state, and nothing outside the rail cares about it.
    let collapsed = RwSignal::new(HashSet::<CityId>::new());

    let cities = move || state.kingdom.get().cities;
    let city_count = move || state.kingdom.get().cities.len();

    restore_width(state.sidebar_width);

    view! {
        <aside class="sidebar">
            <header class="kingdom-header">
                <div class="crown-small">"♚"</div>
                <div class="kingdom-id">
                    <div class="kingdom-name">{move || state.kingdom.get().name}</div>
                    <div class="kingdom-path" title=move || state.kingdom.get().root>
                        {move || state.kingdom.get().root}
                    </div>
                </div>
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
                        title="Include approved and rejected plans"
                        on:click=move |_| state.show_all_plans.set(true)
                    >"All"</button>
                </div>
            </div>

            <div class="sidebar-body">
                <ul class="registry">
                    <For each=cities key=|c: &City| c.id.clone() let:city>
                        <CityBranch city=city collapsed=collapsed/>
                    </For>
                </ul>
            </div>

            <Resizer width=state.sidebar_width/>
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
            state
                .kingdom
                .get()
                .plans
                .into_iter()
                .filter(|p| p.city == id && (show_all || is_active(p.status)))
                .collect::<Vec<_>>()
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
        move |_| state.selected.set(Some(id.clone()))
    };

    let has_plans = Memo::new(move |_| !plans.get().is_empty());

    view! {
        <li class="city-branch">
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
                    <For each={move || plans.get()} key=|p: &Plan| p.id.clone() let:plan>
                        <li class="plan-row" title=plan.summary.clone()>
                            <span class="plan-title">{plan.title.clone()}</span>
                            <span class="plan-model">{plan.choice().label()}</span>
                            <span class=format!("plan-badge plan-{}", plan.status.css_suffix())>
                                {plan.status.label()}
                            </span>
                        </li>
                    </For>
                </ul>
            </Show>
        </li>
    }
}

/// A plan is active while it is still in play. Approved and rejected plans are
/// history, hidden unless the King asks.
fn is_active(status: PlanStatus) -> bool {
    !matches!(status, PlanStatus::Approved | PlanStatus::Rejected)
}

/// The drag handle on the rail's right edge.
#[component]
fn Resizer(width: RwSignal<f64>) -> impl IntoView {
    // Drag origin: pointer x and rail width at mousedown.
    let drag = RwSignal::new(Option::<(f64, f64)>::None);

    let on_mouse_down = move |ev: ev::MouseEvent| {
        ev.prevent_default();
        drag.set(Some((ev.client_x() as f64, width.get_untracked())));
        set_resizing_class(true);
    };

    // Window-level rather than element-level: once the pointer leaves the thin
    // handle it is over the SVG map, whose own mousemove pans the viewport and
    // would otherwise steal the drag.
    let move_handle = window_event_listener(ev::mousemove, move |ev: ev::MouseEvent| {
        if let Some((start_x, start_w)) = drag.get_untracked() {
            let next = start_w + (ev.client_x() as f64 - start_x);
            width.set(next.clamp(MIN_WIDTH, MAX_WIDTH));
        }
    });

    let up_handle = window_event_listener(ev::mouseup, move |_| {
        if drag.get_untracked().is_some() {
            drag.set(None);
            set_resizing_class(false);
            // Persist once, on release: writing every mousemove would hammer
            // localStorage for no benefit.
            store_width(width.get_untracked());
        }
    });

    on_cleanup(move || {
        move_handle.remove();
        up_handle.remove();
    });

    view! {
        <div
            class="sidebar-resizer"
            class:dragging=move || drag.get().is_some()
            title="Drag to resize · double-click to reset"
            on:mousedown=on_mouse_down
            on:dblclick=move |_| {
                width.set(DEFAULT_SIDEBAR_WIDTH);
                store_width(DEFAULT_SIDEBAR_WIDTH);
            }
        ></div>
    }
}

// --- Width persistence -----------------------------------------------------
//
// Browser-only, and every call is failure-tolerant: storage can be disabled
// entirely, in which case the rail simply opens at its default width.

#[cfg(feature = "hydrate")]
fn local_storage() -> Option<web_sys::Storage> {
    web_sys::window()?.local_storage().ok().flatten()
}

/// Restores the stored width inside an effect, so it runs only on the client.
/// Reading it during rendering would make the server emit different markup
/// than hydration expects.
fn restore_width(width: RwSignal<f64>) {
    #[cfg(feature = "hydrate")]
    Effect::new(move |_| {
        if let Some(stored) = local_storage()
            .and_then(|s| s.get_item(WIDTH_KEY).ok().flatten())
            .and_then(|raw| raw.parse::<f64>().ok())
        {
            width.set(stored.clamp(MIN_WIDTH, MAX_WIDTH));
        }
    });

    #[cfg(not(feature = "hydrate"))]
    let _ = width;
}

fn store_width(width: f64) {
    #[cfg(feature = "hydrate")]
    if let Some(storage) = local_storage() {
        let _ = storage.set_item(WIDTH_KEY, &width.to_string());
    }

    #[cfg(not(feature = "hydrate"))]
    let _ = width;
}

/// Suppresses text selection for the duration of a drag, so the pointer
/// crossing the map does not smear a highlight across it.
fn set_resizing_class(on: bool) {
    #[cfg(feature = "hydrate")]
    if let Some(body) = web_sys::window()
        .and_then(|w| w.document())
        .and_then(|d| d.body())
    {
        let list = body.class_list();
        let _ = if on {
            list.add_1("resizing")
        } else {
            list.remove_1("resizing")
        };
    }

    #[cfg(not(feature = "hydrate"))]
    let _ = on;
}
