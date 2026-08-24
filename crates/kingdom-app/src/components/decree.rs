//! The decree bar: where the King starts a new task.
//!
//! Deliberately *only* the composer. A plan's conversation lives in its own
//! chamber at `/plan/:id`, so this bar has one job -- turn a sentence and a
//! chosen city into a plan -- and then gets out of the way by navigating there.

use crate::api::{begin_plan, model_status};
use crate::app::KingdomState;
use kingdom_core::{City, CredentialState, ModelStatus};
use leptos::prelude::*;
use leptos_router::hooks::use_navigate;

#[component]
pub fn DecreeBar() -> impl IntoView {
    let state = expect_context::<KingdomState>();
    let navigate = use_navigate();

    let (draft, set_draft) = signal(String::new());
    let (showing_setup, set_showing_setup) = signal(false);

    let status = Resource::new(|| (), |_| model_status());

    // The decree targets whichever city is selected, so choosing on the map and
    // typing here are one continuous gesture.
    let target_name = Memo::new(move |_| {
        state
            .selected
            .get()
            .and_then(|id| state.kingdom.get().city(&id).map(|c: &City| c.name.clone()))
    });

    let start = Action::new(move |prompt: &String| {
        let prompt = prompt.clone();
        let city = state.selected.get_untracked().map(|c| c.to_string());
        let navigate = navigate.clone();

        async move {
            match begin_plan(prompt, city).await {
                // Opening is instant -- no lease, no model call -- so the King
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

                // The badge answers "will a decree actually reach a model?"
                // before the King spends a decree finding out.
                <button
                    class="model-badge"
                    class:broken={move || {
                        matches!(status.get(), Some(Ok(ref s)) if !s.is_ready())
                    }}
                    title="How plans get drafted"
                    on:click=move |_| set_showing_setup.update(|s| *s = !*s)
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

            <Show when={move || showing_setup.get()}>
                <ModelSetup status=status/>
            </Show>

            <Show when={move || state.error.get().is_some()}>
                <p class="decree-error">{move || state.error.get().unwrap_or_default()}</p>
            </Show>
        </section>
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
