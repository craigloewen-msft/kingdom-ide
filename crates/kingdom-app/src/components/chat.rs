//! The bottom dock: where the King issues decrees, and where a plan's
//! conversation is read back.
//!
//! The transcript lives on the server rather than in a signal here, so a
//! refresh does not lose it and the rail, the map and the dock all read the
//! same plan.

use crate::api::{continue_plan, get_kingdom, model_status, open_plan};
use crate::app::KingdomState;
use kingdom_core::{City, CredentialState, ModelStatus, Plan, PlanStatus, Speaker};
use leptos::prelude::*;

#[component]
pub fn ChatDock() -> impl IntoView {
    let state = expect_context::<KingdomState>();

    let (draft, set_draft) = signal(String::new());
    let (expanded, set_expanded) = signal(false);
    let (showing_setup, set_showing_setup) = signal(false);

    let status = Resource::new(|| (), |_| model_status());

    // The decree targets whichever city is selected, so choosing on the map
    // and typing here are one continuous gesture.
    let target_name = move || {
        state
            .selected
            .get()
            .and_then(|id| state.kingdom.get().city(&id).map(|c: &City| c.name.clone()))
    };

    // The plan under discussion: the most recent live one in the selected city.
    // Switching cities therefore switches conversation.
    let current = Memo::new(move |_| {
        let id = state.selected.get()?;
        state
            .kingdom
            .get()
            .plans_in(&id)
            .filter(|p| p.is_live())
            .last()
            .cloned()
    });

    let send = Action::new(move |prompt: &String| {
        let prompt = prompt.clone();
        let city = state.selected.get_untracked().map(|c| c.to_string());
        // Carry on the plan already in play; otherwise open a new one.
        let existing = current
            .get_untracked()
            .filter(|p| matches!(p.status, PlanStatus::AwaitingReview | PlanStatus::Drafting));

        async move {
            let outcome = match existing {
                Some(plan) => continue_plan(plan.id.to_string(), prompt).await,
                None => open_plan(prompt, city).await,
            };

            match outcome {
                // Refetch rather than patching the local copy: drafting also
                // moved leases and resources, which the rail and map render.
                Ok(_) => {
                    if let Ok(k) = get_kingdom().await {
                        state.kingdom.set(k);
                    }
                    state.error.set(None);
                }
                Err(e) => state.error.set(Some(e.to_string())),
            }
        }
    });

    let submit = move || {
        let text = draft.get().trim().to_string();
        if text.is_empty() {
            return;
        }
        set_expanded.set(true);
        set_draft.set(String::new());
        send.dispatch(text);
    };

    let drafting = Memo::new(move |_| send.pending().get());

    view! {
        <section class="chat-dock" class:expanded=move || expanded.get()>
            <div class="dock-handle-row">
                <button
                    class="dock-handle"
                    on:click=move |_| set_expanded.update(|e| *e = !*e)
                >
                    <span class="handle-title">
                        {move || match current.get() {
                            Some(p) => p.title,
                            None => "Start a new task".to_string(),
                        }}
                    </span>
                    <span class="handle-target">
                        {move || match target_name() {
                            Some(name) => format!("\u{2192} {name}"),
                            None => "\u{2192} no city chosen".to_string(),
                        }}
                    </span>
                    <span class="handle-chevron">
                        {move || if expanded.get() { "\u{2304}" } else { "\u{2303}" }}
                    </span>
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

            <Show when={move || expanded.get()}>
                <div class="chat-log">
                    <Show when={move || current.get().is_none() && !drafting.get()}>
                        <p class="chat-empty">
                            "Choose a city on the map, then describe the work. \
                             A plan will be drawn up for your review."
                        </p>
                    </Show>

                    {move || current.get().map(|plan| view! { <Transcript plan=plan/> })}

                    <Show when={move || drafting.get()}>
                        <div class="chat-msg drafting">
                            <span class="msg-who">"Court"</span>
                            <span class="msg-body">"Drawing up the plan\u{2026}"</span>
                        </div>
                    </Show>

                    <Show when={move || state.error.get().is_some()}>
                        <div class="chat-msg failed">
                            <span class="msg-who">"Court"</span>
                            <span class="msg-body">
                                {move || state.error.get().unwrap_or_default()}
                            </span>
                        </div>
                    </Show>
                </div>
            </Show>

            <div class="chat-input-row">
                <input
                    class="chat-input"
                    r#type="text"
                    placeholder=move || match target_name() {
                        Some(name) => format!("Describe the work for {name}\u{2026}"),
                        None => "Choose a city first\u{2026}".to_string(),
                    }
                    prop:value=move || draft.get()
                    disabled={move || drafting.get()}
                    on:focus=move |_| set_expanded.set(true)
                    on:input=move |ev| set_draft.set(event_target_value(&ev))
                    on:keydown=move |ev| {
                        if ev.key() == "Enter" { submit(); }
                    }
                />
                <button
                    class="send-btn"
                    disabled={move || drafting.get()}
                    on:click=move |_| submit()
                >
                    {move || if drafting.get() { "Drafting\u{2026}" } else { "Decree" }}
                </button>
            </div>
        </section>
    }
}

/// One plan's conversation, oldest first.
#[component]
fn Transcript(plan: Plan) -> impl IntoView {
    let lines = plan.transcript.clone();
    view! {
        <For
            each={move || lines.clone().into_iter().enumerate().collect::<Vec<_>>()}
            key=|(i, _)| *i
            let:entry
        >
            {
                let (_, line) = entry;
                let royal = line.speaker == Speaker::King;
                view! {
                    <div class="chat-msg" class:royal=royal>
                        <span class="msg-who">
                            {if royal { "You" } else { "Court" }}
                        </span>
                        <span class="msg-body">{line.body.clone()}</span>
                    </div>
                }
            }
        </For>
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
