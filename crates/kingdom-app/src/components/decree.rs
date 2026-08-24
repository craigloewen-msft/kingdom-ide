//! The decree bar: where the King starts a new task.
//!
//! Deliberately *only* the composer, plus the two controls that answer "what
//! will draft this, and will it work?" before a decree is spent. A plan's
//! conversation lives in its own chamber at `/plan/:id`, so this bar turns a
//! sentence and a chosen city into a plan and then gets out of the way by
//! navigating there.

use crate::api::{begin_plan, list_models, model_status};
use crate::app::KingdomState;
use kingdom_core::{
    City, CredentialState, ModelCatalogue, ModelChoice, ModelEffort, ModelOption, ModelStatus,
};
use leptos::prelude::*;
use leptos_router::hooks::use_navigate;

#[component]
pub fn DecreeBar() -> impl IntoView {
    let state = expect_context::<KingdomState>();
    let navigate = use_navigate();

    let (draft, set_draft) = signal(String::new());
    let (showing_setup, set_showing_setup) = signal(false);
    let (showing_models, set_showing_models) = signal(false);

    let status = Resource::new(|| (), |_| model_status());
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
        let navigate = navigate.clone();

        async move {
            match begin_plan(prompt, city, chosen).await {
                // Opening is instant -- no model call -- so the King
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
                <button
                    class="model-chip"
                    title="Choose the model and how hard it thinks"
                    on:click=move |_| {
                        set_showing_setup.set(false);
                        set_showing_models.update(|s| *s = !*s);
                    }
                >
                    {move || match choice.get() {
                        Some(c) => c.label(),
                        None => "\u{2026}".to_string(),
                    }}
                    <span class="chip-chevron">"\u{2304}"</span>
                </button>

                // The badge answers "will a decree actually reach a model?"
                // before the King spends a decree finding out.
                <button
                    class="model-badge"
                    class:broken={move || {
                        matches!(status.get(), Some(Ok(ref s)) if !s.is_ready())
                    }}
                    title="How plans get drafted"
                    on:click=move |_| {
                        set_showing_models.set(false);
                        set_showing_setup.update(|s| *s = !*s);
                    }
                >
                    {move || match status.get() {
                        Some(Ok(s)) => format!(
                            "{} {}",
                            s.provider.label(),
                            if s.is_ready() { "\u{2713}" } else { "\u{2717}" },
                        ),
                        Some(Err(_)) => "model ?".to_string(),
                        None => "\u{2026}".to_string(),
                    }}
                </button>
            </div>

            <Show when={move || showing_models.get()}>
                <ModelPicker
                    catalogue=catalogue
                    chosen={Signal::derive(move || choice.get())}
                    on_close=move || set_showing_models.set(false)
                />
            </Show>

            <Show when={move || showing_setup.get()}>
                <ModelSetup status=status/>
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
#[component]
fn ModelPicker(
    catalogue: Resource<Result<ModelCatalogue, ServerFnError>>,
    chosen: Signal<Option<ModelChoice>>,
    on_close: impl Fn() + Copy + Send + Sync + 'static,
) -> impl IntoView {
    let state = expect_context::<KingdomState>();
    let (show_all, set_show_all) = signal(false);

    let options = Memo::new(move |_| match catalogue.get() {
        Some(Ok(c)) => c.options,
        _ => Vec::new(),
    });

    let visible = Memo::new(move |_| {
        let all = show_all.get();
        options
            .get()
            .into_iter()
            .filter(|o| all || o.recommended)
            .collect::<Vec<_>>()
    });

    let hidden_count = Memo::new(move |_| options.get().iter().filter(|o| !o.recommended).count());

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

/// The setup panel: names the exact variable to set, rather than leaving the
/// King to read the source to find out why nothing is drafting.
#[component]
fn ModelSetup(status: Resource<Result<ModelStatus, ServerFnError>>) -> impl IntoView {
    const EXAMPLE: &str = "# .kingdom.env \u{2014} either path works
KINGDOM_MODEL_PROVIDER=copilot

# 1. a token you already hold
KINGDOM_API_KEY=gho_\u{2026}

# 2. or a command that prints one (the default)
KINGDOM_API_KEY_HELPER=agency auth github";

    view! {
        <div class="model-setup">
            {move || match status.get() {
                Some(Ok(s)) => {
                    let needs_help = s.credential != CredentialState::Ready;
                    view! {
                        <div>
                            <p class="setup-line">
                                <strong>{s.provider.label()}</strong>
                                " \u{2014} "
                                {s.model.clone()}
                            </p>
                            <p class="setup-detail">{s.detail.clone()}</p>
                            <Show when={move || needs_help}>
                                <pre class="setup-code">{EXAMPLE}</pre>
                            </Show>
                        </div>
                    }
                    .into_any()
                }
                _ => view! { <p class="setup-detail">"Asking the court\u{2026}"</p> }.into_any(),
            }}
        </div>
    }
}
