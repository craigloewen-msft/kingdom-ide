//! The left rail: the King's registry of cities, architects, plans and
//! crown resources.

use crate::app::{KingdomState, Panel};
use kingdom_core::{Architect, City, Plan, Resource as CrownResource};
use leptos::prelude::*;

#[component]
pub fn Sidebar() -> impl IntoView {
    let state = expect_context::<KingdomState>();

    // Counts double as an at-a-glance health readout, so the King can see
    // pending work without opening each section.
    let pending_plans = move || state.kingdom.get().pending_plans().count();
    let contended = move || state.kingdom.get().contended_resources().count();

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

            <nav class="panel-tabs">
                <PanelTab
                    panel=Panel::Cities
                    label="Cities"
                    glyph="⌂"
                    count=Signal::derive(move || state.kingdom.get().cities.len())
                />
                <PanelTab
                    panel=Panel::Architects
                    label="Architects"
                    glyph="⚒"
                    count=Signal::derive(move || state.kingdom.get().architects.len())
                />
                <PanelTab
                    panel=Panel::Plans
                    label="Plans"
                    glyph="☷"
                    count=Signal::derive(pending_plans)
                />
                <PanelTab
                    panel=Panel::Resources
                    label="Crown Resources"
                    glyph="⚿"
                    count=Signal::derive(contended)
                />
            </nav>

            <div class="panel-body">
                {move || match state.panel.get() {
                    Panel::Cities => view! { <CityList/> }.into_any(),
                    Panel::Architects => view! { <ArchitectList/> }.into_any(),
                    Panel::Plans => view! { <PlanList/> }.into_any(),
                    Panel::Resources => view! { <ResourceList/> }.into_any(),
                }}
            </div>
        </aside>
    }
}

#[component]
fn PanelTab(
    panel: Panel,
    label: &'static str,
    glyph: &'static str,
    count: Signal<usize>,
) -> impl IntoView {
    let state = expect_context::<KingdomState>();
    let active = move || state.panel.get() == panel;

    view! {
        <button
            class="panel-tab"
            class:active=active
            on:click=move |_| state.panel.set(panel)
        >
            <span class="tab-glyph">{glyph}</span>
            <span class="tab-label">{label}</span>
            <Show when={move || count.get() > 0}>
                <span class="tab-count">{move || count.get()}</span>
            </Show>
        </button>
    }
}

#[component]
fn CityList() -> impl IntoView {
    let state = expect_context::<KingdomState>();
    let cities = move || state.kingdom.get().cities;

    view! {
        <ul class="registry">
            <For each=cities key=|c: &City| c.id.clone() let:city>
                {
                    let id = city.id.clone();
                    let selected = {
                        let id = id.clone();
                        move || state.selected.get().as_ref() == Some(&id)
                    };
                    let agents = {
                        let id = id.clone();
                        // A Memo is Copy, so it can be read in several places
                        // in the view without being moved.
                        Memo::new(move |_| state.kingdom.get().architects_in(&id).count())
                    };

                    view! {
                        <li
                            class="registry-item"
                            class:selected=selected
                            on:click=move |_| state.selected.set(Some(id.clone()))
                        >
                            <span
                                class="kind-dot"
                                style:background=city.kind.banner_color()
                            ></span>
                            <div class="item-main">
                                <div class="item-title">{city.name.clone()}</div>
                                <div class="item-sub">
                                    {city.kind.label()}
                                    <Show when=move || city.has_git>
                                        <span class="git-mark" title="git repository">" ⎇"</span>
                                    </Show>
                                </div>
                            </div>
                            <Show when={move || agents.get() > 0}>
                                <span class="agent-pip" title="architects at work">
                                    {move || agents.get()}
                                </span>
                            </Show>
                        </li>
                    }
                }
            </For>
        </ul>
    }
}

#[component]
fn ArchitectList() -> impl IntoView {
    let state = expect_context::<KingdomState>();
    let architects = move || state.kingdom.get().architects;

    view! {
        <ul class="registry">
            <For each=architects key=|a: &Architect| a.id.clone() let:architect>
                <li class="registry-item">
                    <span
                        class=format!("status-dot status-{}", architect.status.css_suffix())
                        title=architect.status.label()
                    ></span>
                    <div class="item-main">
                        <div class="item-title">{architect.name.clone()}</div>
                        <div class="item-sub">{architect.activity.clone()}</div>
                        <Show when={
                            let n = architect.leases.len();
                            move || n > 0
                        }>
                            <div class="lease-line">
                                {format!("holds {} resource(s)", architect.leases.len())}
                            </div>
                        </Show>
                    </div>
                </li>
            </For>
        </ul>
    }
}

#[component]
fn PlanList() -> impl IntoView {
    let state = expect_context::<KingdomState>();
    let plans = move || state.kingdom.get().plans;

    view! {
        <ul class="registry">
            <For each=plans key=|p: &Plan| p.id.clone() let:plan>
                <li class="registry-item plan-item">
                    <div class="item-main">
                        <div class="item-title">{plan.title.clone()}</div>
                        <div class="item-sub">{plan.summary.clone()}</div>
                        <div class="plan-foot">
                            <span class="plan-status">{plan.status.label()}</span>
                            <span class="plan-touches">
                                {format!("{} files", plan.touches.len())}
                            </span>
                        </div>
                    </div>
                </li>
            </For>
        </ul>
    }
}

/// Crown resources, with contention called out.
///
/// This panel is the reason the IDE exists: it answers "who is holding what,
/// and who is stuck waiting behind them?"
#[component]
fn ResourceList() -> impl IntoView {
    let state = expect_context::<KingdomState>();
    let resources = move || state.kingdom.get().resources;

    view! {
        <ul class="registry">
            <For each=resources key=|r: &CrownResource| r.id.clone() let:resource>
                {
                    let contended = resource.is_contended();
                    let held = resource.is_held();

                    view! {
                        <li class="registry-item resource-item" class:contended=move || contended>
                            <span class="res-glyph">{resource.kind.glyph()}</span>
                            <div class="item-main">
                                <div class="item-title">{resource.name.clone()}</div>
                                <div class="item-sub">
                                    {
                                        if !held {
                                            "Unclaimed".to_string()
                                        } else {
                                            resource
                                                .holders
                                                .iter()
                                                .map(|l| format!("{} ({})", l.holder, l.mode.label()))
                                                .collect::<Vec<_>>()
                                                .join(", ")
                                        }
                                    }
                                </div>
                                <Show when=move || contended>
                                    <div class="contention-line">
                                        {format!("{} waiting", resource.waiting.len())}
                                    </div>
                                </Show>
                            </div>
                        </li>
                    }
                }
            </For>
        </ul>
    }
}
