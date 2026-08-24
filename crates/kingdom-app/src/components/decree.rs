//! The decree bar: where the King starts a new task.
//!
//! Deliberately *only* the composer, plus the two controls that answer "what
//! will draft this, and will it work?" before a decree is spent. A plan's
//! conversation lives in its own chamber at `/plan/:id`, so this bar turns a
//! sentence and a chosen city into a plan and then gets out of the way by
//! navigating there.

use crate::api::{begin_plan, list_branches, list_models};
use crate::app::KingdomState;
use kingdom_core::{
    City, CredentialState, ModelCatalogue, ModelChoice, ModelEffort, ModelOption, WorkspaceMode,
};
use leptos::prelude::*;
use leptos_router::hooks::use_navigate;

#[component]
pub fn DecreeBar() -> impl IntoView {
    let state = expect_context::<KingdomState>();
    let navigate = use_navigate();

    let (draft, set_draft) = signal(String::new());
    let (showing_models, set_showing_models) = signal(false);
    let (showing_workspace, set_showing_workspace) = signal(false);

    let catalogue = Resource::new(|| (), |_| list_models());

    // The decree targets whichever city is selected, so choosing on the map and
    // typing here are one continuous gesture.
    let target_name = Memo::new(move |_| {
        state
            .selected
            .get()
            .and_then(|id| state.kingdom.get().city(&id).map(|c: &City| c.name.clone()))
    });

    // What the chip shows, and what the next decree will carry: the King's own
    // choice if he has made one, otherwise the catalogue's default.
    //
    // Passed through the catalogue before it is shown, because the server
    // resolves the same way before drafting -- a chip advertising a model that
    // has left the catalogue would be a promise the decree cannot keep.
    let choice = Memo::new(move |_| {
        let wanted = state.choice.get();
        match catalogue.get() {
            Some(Ok(c)) => Some(c.resolve(wanted.as_ref())),
            // Before the catalogue lands there is nothing to check against, so
            // show the King's own choice rather than a placeholder.
            _ => wanted,
        }
    });

    let start = Action::new(move |prompt: &String| {
        let prompt = prompt.clone();
        let city = state.selected.get_untracked().map(|c| c.to_string());
        // Send what the chip promised, not the raw stored value: they differ
        // exactly when a remembered model has left the catalogue.
        let chosen = choice.get_untracked();
        let workspace = state.workspace.get_untracked();
        let navigate = navigate.clone();

        async move {
            match begin_plan(prompt, city, chosen, Some(workspace)).await {
                // Opening makes no model call, so the King
                // lands in the conversation while the court is still thinking.
                // The chamber itself kicks off the drafting.
                Ok(plan) => {
                    state.error.set(None);
                    let href = format!("/plan/{}", plan.id);
                    // Insert rather than refetch: opening claimed nothing, so
                    // the new plan is the only thing that changed. Navigating
                    // without it would land the chamber on a plan its own copy
                    // of the kingdom does not yet know about.
                    state.kingdom.update(|k| k.plans.push(plan));
                    navigate(&href, Default::default());
                }
                Err(e) => state.error.set(Some(e.to_string())),
            }
        }
    });

    let ready = Memo::new(move |_| target_name.get().is_some() && !start.pending().get());

    let submit = move || {
        let text = draft.get().trim().to_string();
        if text.is_empty() || !ready.get_untracked() {
            return;
        }
        set_draft.set(String::new());
        start.dispatch(text);
    };

    view! {
        <section class="decree-bar">
            <div class="decree-row">
                <span class="decree-target" class:none={move || target_name.get().is_none()}>
                    {move || match target_name.get() {
                        Some(name) => format!("\u{2192} {name}"),
                        None => "\u{2192} choose a city".to_string(),
                    }}
                </span>

                <input
                    class="decree-input"
                    r#type="text"
                    placeholder=move || match target_name.get() {
                        Some(name) => format!("Describe the work for {name}\u{2026}"),
                        None => "Choose a city on the map first\u{2026}".to_string(),
                    }
                    prop:value=move || draft.get()
                    disabled={move || !ready.get()}
                    on:input=move |ev| set_draft.set(event_target_value(&ev))
                    on:keydown=move |ev| {
                        if ev.key() == "Enter" { submit(); }
                    }
                />

                <button
                    class="start-btn"
                    disabled={move || !ready.get()}
                    on:click=move |_| submit()
                >
                    {move || if start.pending().get() { "Opening\u{2026}" } else { "Start" }}
                </button>

                // Which model the next plan opens with. The choice is recorded
                // on the plan, so it is settled here rather than mid-draft.
                // This chip is the only answer to "what will draft this?" --
                // there is no separate provider badge, because the mock is a
                // model in the list like any other.
                <button
                    class="model-chip"
                    title="Choose the model and how hard it thinks"
                    on:click=move |_| {
                        set_showing_workspace.set(false);
                        set_showing_models.update(|s| *s = !*s);
                    }
                >
                    {move || match choice.get() {
                        Some(c) => c.label(),
                        None => "\u{2026}".to_string(),
                    }}
                    <span class="chip-chevron">"\u{2304}"</span>
                </button>

                // Where the next plan will work. Beside the model chip because
                // they are the same kind of decision: both are settled before a
                // decree is spent, and both are recorded on the plan.
                <button
                    class="workspace-chip"
                    class:isolated={move || state.workspace.get() != WorkspaceMode::InPlace}
                    title="Choose where this work happens"
                    on:click=move |_| {
                        set_showing_models.set(false);
                        set_showing_workspace.update(|s| *s = !*s);
                    }
                >
                    {move || state.workspace.get().label()}
                    <span class="chip-chevron">"\u{2304}"</span>
                </button>
            </div>

            <Show when={move || showing_models.get()}>
                <ModelPicker
                    catalogue=catalogue
                    chosen={Signal::derive(move || choice.get())}
                    on_close=move || set_showing_models.set(false)
                />
            </Show>

            <Show when={move || showing_workspace.get()}>
                <WorkspacePicker on_close=move || set_showing_workspace.set(false)/>
            </Show>

            <Show when={move || state.error.get().is_some()}>
                <p class="decree-error">{move || state.error.get().unwrap_or_default()}</p>
            </Show>
        </section>
    }
}

/// The picker: which model, and how hard it thinks.
///
/// Recommended models first, the rest behind a toggle -- the full Copilot
/// catalogue runs to dozens of entries, most of which the King will never pick,
/// and a wall of them costs more attention than it saves.
///
/// This is also where a broken credential surfaces. There is no separate status
/// badge: a thin list and the reason it is thin belong in the same place, at the
/// moment the King notices the models he expected are missing.
#[component]
fn ModelPicker(
    catalogue: Resource<Result<ModelCatalogue, ServerFnError>>,
    chosen: Signal<Option<ModelChoice>>,
    on_close: impl Fn() + Copy + Send + Sync + 'static,
) -> impl IntoView {
    let state = expect_context::<KingdomState>();
    let (show_all, set_show_all) = signal(false);

    /// Named so a King who cannot see the model he wants knows exactly what to
    /// set, rather than reading the source to find out.
    const EXAMPLE: &str = "# .kingdom.env \u{2014} either credential path works

# 1. a token you already hold
KINGDOM_API_KEY=gho_\u{2026}

# 2. or a command that prints one (the default)
KINGDOM_API_KEY_HELPER=agency auth github

# optional: which model the picker opens on
KINGDOM_MODEL=copilot/claude-opus-5";

    // Shown only when something is actually wrong, so a healthy court does not
    // spend the King's attention on setup instructions he does not need.
    let needs_help = Memo::new(
        move |_| matches!(catalogue.get(), Some(Ok(c)) if c.credential != CredentialState::Ready),
    );

    let options = Memo::new(move |_| match catalogue.get() {
        Some(Ok(c)) => c.options,
        _ => Vec::new(),
    });

    // Recommended only, until the King asks for everything -- except when
    // nothing is recommended at all. That happens exactly when no credential
    // works and the offline mock is the only model left: filtering it out would
    // leave the King staring at an empty picker with no way to draft.
    let visible = Memo::new(move |_| {
        let all = show_all.get();
        let options = options.get();
        let any_recommended = options.iter().any(|o| o.recommended);
        options
            .into_iter()
            .filter(|o| all || !any_recommended || o.recommended)
            .collect::<Vec<_>>()
    });

    let hidden_count = Memo::new(move |_| {
        let options = options.get();
        match options.iter().any(|o| o.recommended) {
            true => options.iter().filter(|o| !o.recommended).count(),
            false => 0,
        }
    });

    // The effort row belongs to the chosen model: offering a level it does not
    // declare would earn an opaque 400 rather than a harder answer.
    let efforts = Memo::new(move |_| {
        let id = chosen.get()?.model;
        options
            .get()
            .into_iter()
            .find(|o| o.id == id)
            .map(|o| o.efforts)
    });

    let pick_model = move |option: &ModelOption| {
        let keep = chosen
            .get_untracked()
            .and_then(|c| c.effort)
            .filter(|e| option.efforts.contains(e));
        state.choose_model(ModelChoice::new(option.id.clone(), keep));
    };

    let pick_effort = move |effort: Option<ModelEffort>| {
        if let Some(current) = chosen.get_untracked() {
            state.choose_model(ModelChoice::new(current.model, effort));
        }
    };

    view! {
        <div class="model-picker">
            <div class="picker-head">
                <span class="picker-title">"Draft with"</span>
                <button class="picker-close" on:click=move |_| on_close()>"\u{2715}"</button>
            </div>

            <p class="setup-detail">
                {move || match catalogue.get() {
                    Some(Ok(c)) => c.detail,
                    Some(Err(e)) => e.to_string(),
                    None => "Asking the court what it can think with\u{2026}".to_string(),
                }}
            </p>

            <Show when={move || needs_help.get()}>
                <pre class="setup-code">{EXAMPLE}</pre>
            </Show>

            <ul class="model-list">
                <For each={move || visible.get()} key=|o: &ModelOption| o.id.clone() let:option>
                    {
                        let id = option.id.clone();
                        let is_chosen = Memo::new(move |_| {
                            chosen.get().is_some_and(|c| c.model == id)
                        });
                        let picked = option.clone();
                        view! {
                            <li>
                                <button
                                    class="model-row"
                                    class:chosen={move || is_chosen.get()}
                                    on:click=move |_| pick_model(&picked)
                                >
                                    <span class="model-name">{option.label.clone()}</span>
                                    // Copilot ships dated aliases that share a
                                    // display name (three distinct "GPT-4o"s),
                                    // so the api name -- which is what the chip
                                    // and the plan record -- is what tells them
                                    // apart.
                                    <span class="model-api-name">
                                        {option.id.rsplit('/').next().unwrap_or(&option.id).to_string()}
                                    </span>
                                    <span class="model-vendor">{option.vendor.clone()}</span>
                                    <span class="model-window">
                                        {match option.context_window {
                                            0 => String::new(),
                                            w => format!("{}K", w / 1000),
                                        }}
                                    </span>
                                </button>
                            </li>
                        }
                    }
                </For>
            </ul>

            <Show when={move || hidden_count.get() > 0 && !show_all.get()}>
                <button class="picker-more" on:click=move |_| set_show_all.set(true)>
                    {move || format!("Show all {} models", options.get().len())}
                </button>
            </Show>

            <Show when={move || efforts.get().is_some_and(|e| !e.is_empty())}>
                <div class="effort-row">
                    <span class="effort-label">"Effort"</span>
                    // "Default" is not a level: it sends no field at all, which
                    // is a different request from any named effort.
                    <button
                        class="effort-btn"
                        class:chosen={move || chosen.get().is_some_and(|c| c.effort.is_none())}
                        on:click=move |_| pick_effort(None)
                    >
                        "default"
                    </button>
                    <For
                        each={move || efforts.get().unwrap_or_default()}
                        key=|e: &ModelEffort| *e
                        let:effort
                    >
                        <button
                            class="effort-btn"
                            class:chosen={move || {
                                chosen.get().is_some_and(|c| c.effort == Some(effort))
                            }}
                            on:click=move |_| pick_effort(Some(effort))
                        >
                            {effort.wire_name()}
                        </button>
                    </For>
                </div>
            </Show>
        </div>
    }
}

/// The workspace picker: where the next plan's work actually happens.
///
/// Three options rather than a toggle, because they are three genuinely
/// different bargains -- isolation with a new branch, isolation continuing
/// existing work, and no isolation at all -- and collapsing any two of them
/// would hide the one the King most needs to make on purpose.
#[component]
fn WorkspacePicker(on_close: impl Fn() + Copy + Send + Sync + 'static) -> impl IntoView {
    let state = expect_context::<KingdomState>();
    let (showing_branches, set_showing_branches) = signal(false);

    let current = Memo::new(move |_| state.workspace.get());

    // Reloaded whenever the selected city changes: branches are per repository,
    // and offering another project's would be worse than offering none.
    let branches = Resource::new(
        move || state.selected.get().map(|c| c.to_string()),
        |city| async move {
            match city {
                Some(c) => list_branches(c).await.unwrap_or_default(),
                None => Vec::new(),
            }
        },
    );

    let choose = move |mode: WorkspaceMode| {
        state.choose_workspace(mode);
        on_close();
    };

    let is_fresh = Memo::new(move |_| current.get() == WorkspaceMode::Fresh);
    let is_in_place = Memo::new(move |_| current.get() == WorkspaceMode::InPlace);
    let is_branch = Memo::new(move |_| matches!(current.get(), WorkspaceMode::Branch(_)));

    view! {
        <div class="workspace-picker">
            <div class="picker-head">
                <span class="picker-title">"Work in"</span>
                <button class="picker-close" on:click=move |_| on_close()>"\u{2715}"</button>
            </div>

            <ul class="workspace-list">
                <li>
                    <button
                        class="workspace-row"
                        class:chosen={move || is_fresh.get()}
                        on:click=move |_| choose(WorkspaceMode::Fresh)
                    >
                        <span class="workspace-name">"A fresh worktree"</span>
                        <span class="workspace-detail">
                            "A new checkout under .kingdom/, on a branch of its own."
                        </span>
                    </button>
                </li>

                <li>
                    <button
                        class="workspace-row"
                        class:chosen={move || is_branch.get()}
                        on:click=move |_| set_showing_branches.update(|s| *s = !*s)
                    >
                        <span class="workspace-name">
                            {move || match current.get() {
                                WorkspaceMode::Branch(b) => format!("A branch \u{2014} {b}"),
                                _ => "A specific branch\u{2026}".to_string(),
                            }}
                        </span>
                        <span class="workspace-detail">
                            "Checked out into its own worktree, same as above."
                        </span>
                    </button>

                    <Show when={move || showing_branches.get()}>
                        <ul class="branch-list">
                            {move || match branches.get() {
                                Some(list) if !list.is_empty() => list
                                    .into_iter()
                                    .map(|b| {
                                        let picked = b.clone();
                                        let name = b.clone();
                                        view! {
                                            <li>
                                                <button
                                                    class="branch-row"
                                                    class:chosen={move || {
                                                        current.get()
                                                            == WorkspaceMode::Branch(name.clone())
                                                    }}
                                                    on:click=move |_| {
                                                        choose(WorkspaceMode::Branch(picked.clone()))
                                                    }
                                                >
                                                    {b.clone()}
                                                </button>
                                            </li>
                                        }
                                    })
                                    .collect_view()
                                    .into_any(),
                                // Distinguishing "no branches" from "still
                                // looking" matters: the first means this city is
                                // not a git repository at all.
                                Some(_) => view! {
                                    <li class="branch-empty">
                                        "No branches here \u{2014} this project is not under git."
                                    </li>
                                }
                                .into_any(),
                                None => view! {
                                    <li class="branch-empty">"Reading the branches\u{2026}"</li>
                                }
                                .into_any(),
                            }}
                        </ul>
                    </Show>
                </li>

                <li>
                    <button
                        class="workspace-row"
                        class:chosen={move || is_in_place.get()}
                        on:click=move |_| choose(WorkspaceMode::InPlace)
                    >
                        <span class="workspace-name">"This folder"</span>
                        <span class="workspace-detail">
                            "The project directory itself. No isolation."
                        </span>
                    </button>
                </li>
            </ul>
        </div>
    }
}
