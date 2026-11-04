//! `/pulls` -- the open pull requests on a project, and asking for a review of
//! one.
//!
//! Shown to the King as "Pull requests". A screen of its own rather than a
//! panel in a chamber, for the reason `/resources` is: a pull request is not
//! yet anybody's plan, and choosing which one to have reviewed is a decision
//! made before any conversation exists.
//!
//! # What it does not do
//!
//! It never writes to GitHub. The list is read through `gh` on the server, the
//! review is a plan in Kingdom, and the findings stay here for the King to act
//! on himself. See `kingdom_app::github` for why that is the whole surface.
//!
//! # Which model, which fence
//!
//! The review composer carries the decree bar's own model and isolation chips,
//! opening the very same pickers onto the very same settings. So the King can
//! choose what drafts a review and how far it is walled off without leaving
//! this screen -- and there is still only **one** setting, changeable from two
//! places, rather than a second set of pickers that could disagree with the
//! first. A choice made here is what the next decree opens with too, exactly as
//! a choice made in the decree bar is.
//!
//! There is no workspace picker: a review always works in the pull request's
//! own checkout, so there is nothing to choose.

use super::prompt_bar::{resolved_choice, IsolationPicker, ModelPicker};
use crate::api::{begin_review, list_models, list_pull_requests};
use crate::app::KingdomState;
use kingdom_core::{City, CityId, ModelCatalogue, PullRequest, PullRequestList};
use leptos::prelude::*;
use leptos_router::hooks::use_navigate;

#[component]
pub fn PullRequestsView() -> impl IntoView {
    let state = expect_context::<KingdomState>();

    // Only projects under git can have pull requests, so only those are
    // offered. The rest would each answer "not a repository".
    let cities = Memo::new(move |_| {
        state.kingdom.with(|k| {
            k.cities
                .iter()
                .filter(|c| c.has_git)
                .map(|c: &City| (c.id.clone(), c.name.clone()))
                .collect::<Vec<_>>()
        })
    });

    // Which project is being listed: the one selected on the map if it can
    // have pull requests, otherwise the first that can. Following the map's
    // selection is what lets "choose a city, then look at its PRs" be one
    // gesture, the way it is for a decree.
    let chosen = RwSignal::new(Option::<CityId>::None);
    let city = Memo::new(move |_| {
        let offered = cities.get();
        let wanted = chosen.get().or_else(|| state.selected.get());
        wanted
            .filter(|w| offered.iter().any(|(id, _)| id == w))
            .or_else(|| offered.first().map(|(id, _)| id.clone()))
    });

    // Bumped by the refresh button; part of the source so it re-runs.
    let revision = RwSignal::new(0_u32);

    // Asked once for the whole screen rather than once per row: every review
    // composer offers the same models.
    let catalogue = Resource::new(|| (), |_| list_models());
    let listing = Resource::new(
        move || (city.get(), revision.get()),
        |(city, _)| async move {
            match city {
                None => None,
                Some(city) => Some(
                    list_pull_requests(city.to_string())
                        .await
                        .unwrap_or_else(|e| PullRequestList::Unavailable(e.to_string())),
                ),
            }
        },
    );

    view! {
        <div class="pulls-view">
            <header class="wells-head">
                <div class="wells-heading">
                    <h1>"Pull requests"</h1>
                    <p class="wells-sub">
                        "Choose one to have the court check it out, try it in a browser, \
                         read the code, and report back to you. Nothing is posted to \
                         GitHub \u{2014} the findings are yours to act on."
                    </p>
                </div>
                <div class="wells-actions">
                    <a class="wells-back" href="/" title="Back to the realm">
                        "\u{2190} Realm"
                    </a>
                    <button
                        class="wells-new"
                        title="Ask GitHub again"
                        on:click=move |_| revision.update(|r| *r += 1)
                    >
                        "Refresh"
                    </button>
                </div>
            </header>

            <Show
                when=move || !cities.get().is_empty()
                fallback=|| view! {
                    <p class="wells-nothing">
                        "No project in this kingdom is a git repository, so there are no \
                         pull requests to review."
                    </p>
                }
            >
                <nav class="pulls-cities">
                    <For
                        each=move || cities.get()
                        key=|(id, _)| id.clone()
                        let:entry
                    >
                        {
                            let (id, name) = entry;
                            let mine = id.clone();
                            view! {
                                <button
                                    class="pulls-city"
                                    class:current=move || city.get().as_ref() == Some(&mine)
                                    on:click=move |_| chosen.set(Some(id.clone()))
                                >
                                    {name}
                                </button>
                            }
                        }
                    </For>
                </nav>

                <Suspense fallback=|| view! {
                    <p class="wells-loading">"Asking GitHub\u{2026}"</p>
                }>
                    {move || listing.get().flatten().map(|answer| match answer {
                        PullRequestList::Unavailable(why) => view! {
                            <p class="pulls-unavailable">{why}</p>
                        }.into_any(),
                        PullRequestList::Listed { repository, pulls } if pulls.is_empty() => view! {
                            <p class="wells-nothing">
                                {format!("{repository} has no open pull requests.")}
                            </p>
                        }.into_any(),
                        PullRequestList::Listed { repository, pulls } => {
                            let count = pulls.len();
                            let city = city.get_untracked();
                            view! {
                                <p class="pulls-count">
                                    {format!(
                                        "{count} open on {repository}",
                                    )}
                                </p>
                                <ul class="pulls-list">
                                    {pulls
                                        .into_iter()
                                        .map(|pull| view! {
                                            <PullRow pull=pull city=city.clone() catalogue=catalogue/>
                                        })
                                        .collect_view()}
                                </ul>
                            }.into_any()
                        }
                    })}
                </Suspense>
            </Show>
        </div>
    }
}

/// One pull request, and the composer that asks for its review.
#[component]
fn PullRow(
    pull: PullRequest,
    city: Option<CityId>,
    catalogue: Resource<Result<ModelCatalogue, ServerFnError>>,
) -> impl IntoView {
    let state = expect_context::<KingdomState>();
    let navigate = use_navigate();

    let composing = RwSignal::new(false);
    let notes = RwSignal::new(String::new());
    let number = pull.number;

    // Which of the two pickers is open under the composer, if either. One at a
    // time, as in the decree bar.
    let (showing_models, set_showing_models) = signal(false);
    let (showing_isolation, set_showing_isolation) = signal(false);

    // What the chip shows and what Start sends: the decree bar's own memo, so
    // the two cannot disagree about which model a remembered choice resolves to.
    let choice = resolved_choice(state, catalogue);

    // The project being reviewed, for the isolation panel's Files tab -- which
    // need not be the one selected on the map.
    let reviewed = Signal::stored(city.clone());

    let start = Action::new(move |notes: &String| {
        let notes = notes.clone();
        let city = city.clone();
        let choice = choice.get_untracked();
        let isolation = state.isolation.get_untracked();
        let mounts = state.mounts.get_untracked();
        let navigate = navigate.clone();
        async move {
            let Some(city) = city else {
                state.error.set(Some("Choose a project first.".to_string()));
                return;
            };
            match begin_review(
                city.to_string(),
                number,
                notes,
                choice,
                Some(isolation),
                mounts,
            )
            .await
            {
                Ok(plan) => {
                    state.error.set(None);
                    let href = format!("/plan/{}", plan.id);
                    // Inserted rather than refetched, for `PromptBar`'s reason:
                    // the chamber must find the plan in its own copy of the
                    // kingdom the moment it lands.
                    state.kingdom.update(|k| k.plans.push(plan));
                    navigate(&href, Default::default());
                }
                Err(e) => state.error.set(Some(e.to_string())),
            }
        }
    });

    let updated = pull
        .updated_at
        .split('T')
        .next()
        .unwrap_or_default()
        .to_string();
    let decision = pull.decision;

    view! {
        <li class="pull-row" class:open=move || composing.get()>
            <div class="pull-main">
                <span class="pull-number">{format!("#{}", pull.number)}</span>
                <a
                    class="pull-title"
                    href=pull.url.clone()
                    target="_blank"
                    rel="noopener noreferrer"
                    title="Open on GitHub"
                >
                    {pull.title.clone()}
                </a>
                <Show when=move || pull.draft>
                    <span class="pull-tag pull-draft">"Draft"</span>
                </Show>
                {decision.map(|d| view! {
                    <span class=format!("pull-tag decision-{}", d.css_suffix())>{d.label()}</span>
                })}
                <button
                    class="pull-review"
                    class:open=move || composing.get()
                    on:click=move |_| composing.update(|c| *c = !*c)
                >
                    {move || if composing.get() { "Cancel" } else { "Review" }}
                </button>
            </div>
            <div class="pull-meta">
                <span>{format!("@{}", pull.author)}</span>
                <span class="pull-branches">
                    <code>{pull.head.clone()}</code>
                    " \u{2192} "
                    <code>{pull.base.clone()}</code>
                </span>
                <Show when=move || pull.from_fork>
                    <span class="pull-fork" title="This change comes from a fork">"fork"</span>
                </Show>
                <span class="pull-size">
                    <span class="count-added">{format!("+{}", pull.additions)}</span>
                    " "
                    <span class="count-removed">{format!("\u{2212}{}", pull.deletions)}</span>
                    {format!(" in {} file(s)", pull.changed_files)}
                </span>
                <span class="pull-updated">{format!("updated {updated}")}</span>
            </div>

            <Show when=move || composing.get()>
                <div class="pull-compose">
                    <textarea
                        class="pull-notes"
                        rows="3"
                        placeholder="What should the court look at? (optional) \u{2014} e.g. \
                                     check the login flow at phone width, and whether the \
                                     migration is reversible"
                        prop:value=move || notes.get()
                        on:input=move |ev| notes.set(event_target_value(&ev))
                    />
                    <div class="pull-compose-foot">
                        // The decree bar's two chips, opening its two pickers:
                        // what drafts this review, and how far it is walled off.
                        <button
                            class="model-chip"
                            class:open=move || showing_models.get()
                            title="Choose the model that reviews this, and how hard it thinks"
                            on:click=move |_| {
                                set_showing_isolation.set(false);
                                set_showing_models.update(|s| *s = !*s);
                            }
                        >
                            {move || match choice.get() {
                                Some(c) => c.label(),
                                None => "\u{2026}".to_string(),
                            }}
                            <span class="chip-chevron">"\u{2304}"</span>
                        </button>
                        <button
                            class="isolation-chip"
                            class:isolated=move || state.isolation.get().is_isolated()
                            title="Choose how far this review is walled off"
                            on:click=move |_| {
                                set_showing_models.set(false);
                                set_showing_isolation.update(|s| *s = !*s);
                            }
                        >
                            "Isolation"
                            <Show when=move || state.isolation.get().is_isolated()>
                                <span class="chip-mark">"\u{2022}"</span>
                            </Show>
                            <span class="chip-chevron">"\u{2304}"</span>
                        </button>
                        <button
                            class="start-btn"
                            disabled=move || start.pending().get()
                            on:click=move |_| { start.dispatch(notes.get_untracked()); }
                        >
                            {move || if start.pending().get() {
                                "Checking out\u{2026}"
                            } else {
                                "Start review"
                            }}
                        </button>
                    </div>
                    <Show when=move || showing_models.get()>
                        <ModelPicker
                            catalogue=catalogue
                            chosen=Signal::derive(move || choice.get())
                            on_close=move || set_showing_models.set(false)
                        />
                    </Show>
                    <Show when=move || showing_isolation.get()>
                        <IsolationPicker
                            city=reviewed
                            on_close=move || set_showing_isolation.set(false)
                        />
                    </Show>
                    <Show when=move || state.error.get().is_some()>
                        <p class="decree-error">{move || state.error.get().unwrap_or_default()}</p>
                    </Show>
                </div>
            </Show>
        </li>
    }
}
