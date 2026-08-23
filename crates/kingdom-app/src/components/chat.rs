//! The bottom dock: where the King issues decrees.

use crate::api::start_task;
use crate::app::KingdomState;
use kingdom_core::City;
use leptos::prelude::*;

/// One line in the decree log.
#[derive(Clone, PartialEq)]
struct Message {
    /// True when the King spoke; false for the court's reply.
    royal: bool,
    body: String,
}

#[component]
pub fn ChatDock() -> impl IntoView {
    let state = expect_context::<KingdomState>();

    let (draft, set_draft) = signal(String::new());
    let (log, set_log) = signal(Vec::<Message>::new());
    let (expanded, set_expanded) = signal(false);

    // The decree targets whichever city is selected, so choosing on the map
    // and typing here are one continuous gesture.
    let target_name = move || {
        state
            .selected
            .get()
            .and_then(|id| state.kingdom.get().city(&id).map(|c: &City| c.name.clone()))
    };

    let send = Action::new(move |prompt: &String| {
        let prompt = prompt.clone();
        let city = state.selected.get_untracked().map(|c| c.to_string());
        async move {
            set_log.update(|l| {
                l.push(Message {
                    royal: true,
                    body: prompt.clone(),
                })
            });

            let reply = match start_task(prompt, city).await {
                Ok(r) => r,
                Err(e) => format!("The court could not hear you: {e}"),
            };

            set_log.update(|l| {
                l.push(Message {
                    royal: false,
                    body: reply,
                })
            });
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

    view! {
        <section class="chat-dock" class:expanded=move || expanded.get()>
            <button
                class="dock-handle"
                on:click=move |_| set_expanded.update(|e| *e = !*e)
            >
                <span class="handle-title">"Start a new task"</span>
                <span class="handle-target">
                    {move || match target_name() {
                        Some(name) => format!("→ {name}"),
                        None => "→ no city chosen".to_string(),
                    }}
                </span>
                <span class="handle-chevron">
                    {move || if expanded.get() { "⌄" } else { "⌃" }}
                </span>
            </button>

            <Show when=move || expanded.get()>
                <div class="chat-log">
                    <Show when=move || log.get().is_empty()>
                        <p class="chat-empty">
                            "Choose a city on the map, then describe the work. \
                             An architect will be dispatched to draw up plans."
                        </p>
                    </Show>
                    <For
                        each={move || log.get().into_iter().enumerate().collect::<Vec<_>>()}
                        key=|(i, _)| *i
                        let:entry
                    >
                        {
                            let (_, msg) = entry;
                            view! {
                                <div class="chat-msg" class:royal=msg.royal>
                                    <span class="msg-who">
                                        {if msg.royal { "You" } else { "Court" }}
                                    </span>
                                    <span class="msg-body">{msg.body.clone()}</span>
                                </div>
                            }
                        }
                    </For>
                </div>
            </Show>

            <div class="chat-input-row">
                <input
                    class="chat-input"
                    r#type="text"
                    placeholder=move || match target_name() {
                        Some(name) => format!("Describe the work for {name}…"),
                        None => "Describe the work…".to_string(),
                    }
                    prop:value=move || draft.get()
                    on:focus=move |_| set_expanded.set(true)
                    on:input=move |ev| set_draft.set(event_target_value(&ev))
                    on:keydown=move |ev| {
                        if ev.key() == "Enter" { submit(); }
                    }
                />
                <button class="send-btn" on:click=move |_| submit()>
                    "Decree"
                </button>
            </div>
        </section>
    }
}
