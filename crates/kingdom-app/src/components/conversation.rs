//! The plan's chamber: one plan's whole life, at its own URL.
//!
//! A `Plan` is both the unit of work and the unit of review, so it gets a place
//! to be reviewed in. Everything here reads from server state rather than local
//! signals, which is what lets a reload -- or a link shared between tabs --
//! rebuild the conversation exactly.

use crate::api::{draft_plan, get_kingdom, say};
use crate::app::KingdomState;
use kingdom_core::{Plan, PlanId, PlanStatus, Speaker};
use leptos::prelude::*;
use leptos_router::hooks::use_params_map;

/// How often to ask the server whether a draft has landed.
///
/// A deliberate stopgap for WebSocket push, which is the next thing on
/// `AGENTS.md`'s list. Delete this and `poll_while` when push lands -- do not
/// grow it into a general polling layer.
#[cfg(feature = "hydrate")]
const POLL_MS: u64 = 1000;

#[component]
pub fn Conversation() -> impl IntoView {
    let state = expect_context::<KingdomState>();
    let params = use_params_map();

    let plan_id = Memo::new(move |_| params.get().get("id").map(PlanId::new));

    let plan = Memo::new(move |_| {
        let id = plan_id.get()?;
        state.kingdom.get().plan(&id).cloned()
    });

    let city_name = Memo::new(move |_| {
        let plan = plan.get()?;
        state.kingdom.get().city(&plan.city).map(|c| c.name.clone())
    });

    // The rail and the map should agree with the URL about where the King is.
    Effect::new(move |_| {
        if let Some(p) = plan.get() {
            state.selected.set(Some(p.city));
        }
    });

    let drafting = Memo::new(move |_| {
        plan.get()
            .map(|p| p.status == PlanStatus::Drafting)
            .unwrap_or(false)
    });

    // Drafting is kicked off here rather than by whoever opened the plan, so
    // that landing on a freshly opened plan and reloading one mid-draft take
    // the same path. `draft_plan` is idempotent while a lease is held, so this
    // cannot start a second draft over a running one.
    let draft = Action::new(move |id: &PlanId| {
        let id = id.to_string();
        async move {
            match draft_plan(id).await {
                Ok(_) => state.error.set(None),
                Err(e) => state.error.set(Some(e.to_string())),
            }
            // Refetch rather than patching the local copy: drafting also moved
            // leases and resources, which the rail and map render.
            if let Ok(k) = get_kingdom().await {
                state.kingdom.set(k);
            }
        }
    });

    // A plan that is Drafting, holds nothing and has heard nothing back is one
    // nobody has started yet: exactly the state `begin_plan` leaves behind.
    Effect::new(move |_| {
        let Some(p) = plan.get() else { return };
        let unstarted = p.status == PlanStatus::Drafting
            && p.leases.is_empty()
            && !p.transcript.iter().any(|u| u.speaker == Speaker::Court);
        if unstarted && !draft.pending().get_untracked() {
            draft.dispatch(p.id.clone());
        }
    });

    // A draft started by another page (a reload mid-flight, most likely) has
    // nobody here awaiting it, so poll until it settles.
    poll_while(drafting, move || {
        leptos::task::spawn_local(async move {
            if let Ok(k) = get_kingdom().await {
                state.kingdom.set(k);
            }
        });
    });

    // The King's words land first, then the court is asked -- so his half of
    // the exchange never waits on the model.
    let speak = Callback::new(move |(id, text): (PlanId, String)| {
        leptos::task::spawn_local(async move {
            match say(id.to_string(), text).await {
                Ok(_) => {
                    if let Ok(k) = get_kingdom().await {
                        state.kingdom.set(k);
                    }
                    draft.dispatch(id);
                }
                Err(e) => state.error.set(Some(e.to_string())),
            }
        });
    });

    view! {
        <div class="chamber">
            {move || match plan.get() {
                Some(p) => view! {
                    <ChamberBody plan=p city=city_name drafting=drafting on_say=speak/>
                }
                .into_any(),
                None => view! {
                    <div class="empty-chamber">
                        <p>"No such plan in the records."</p>
                        <a class="back-link" href="/">"\u{2190} Back to the realm"</a>
                    </div>
                }
                .into_any(),
            }}
        </div>
    }
}

#[component]
fn ChamberBody(
    plan: Plan,
    city: Memo<Option<String>>,
    drafting: Memo<bool>,
    on_say: Callback<(PlanId, String)>,
) -> impl IntoView {
    let state = expect_context::<KingdomState>();
    let (reply, set_reply) = signal(String::new());

    let id = StoredValue::new(plan.id.clone());
    let status = plan.status;
    let touches = plan.touches.clone();
    let summary = plan.summary.clone();
    let has_summary = !summary.is_empty();
    let has_touches = !touches.is_empty();

    // `StoredValue` rather than a captured `PlanId`: a closure holding an owned
    // non-Copy value is `FnOnce` and cannot be used by both handlers below.
    let submit = move || {
        let text = reply.get().trim().to_string();
        if text.is_empty() || drafting.get_untracked() {
            return;
        }
        set_reply.set(String::new());
        on_say.run((id.get_value(), text));
    };

    view! {
        <header class="chamber-header">
            <a class="back-link" href="/" title="Back to the realm">"\u{2190}"</a>
            <div class="chamber-id">
                <h1 class="chamber-title">{plan.title.clone()}</h1>
                <div class="chamber-meta">
                    <span class="chamber-city">
                        {move || city.get().unwrap_or_else(|| "unknown city".into())}
                    </span>
                    <span class="chamber-model">{plan.choice().label()}</span>
                </div>
            </div>
            <span class=format!("plan-badge plan-{}", status.css_suffix())>
                {status.label()}
            </span>
        </header>

        <div class="chamber-log">
            <Show when={move || has_summary}>
                <p class="chamber-summary">{summary.clone()}</p>
            </Show>

            // What the plan proposes to touch. On the map these are gilded
            // roofs; here they are the list the King actually reviews.
            <Show when={move || has_touches}>
                <div class="chamber-touches">
                    <span class="touches-label">"Would touch"</span>
                    <ul>
                        {touches.iter().map(|p| view! {
                            <li class="touch-path">{p.clone()}</li>
                        }).collect_view()}
                    </ul>
                </div>
            </Show>

            <Transcript plan=plan.clone()/>

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

        <div class="chamber-composer">
            <input
                class="decree-input"
                r#type="text"
                placeholder="Say more, or ask for a change\u{2026}"
                prop:value=move || reply.get()
                disabled={move || drafting.get()}
                on:input=move |ev| set_reply.set(event_target_value(&ev))
                on:keydown=move |ev| {
                    if ev.key() == "Enter" { submit(); }
                }
            />
            <button
                class="start-btn"
                disabled={move || drafting.get()}
                on:click=move |_| submit()
            >
                {move || if drafting.get() { "Drafting\u{2026}" } else { "Decree" }}
            </button>
        </div>
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

/// Runs `refresh` on a timer for as long as `active` reads true.
///
/// Browser-only: under SSR there is no timer to run and nothing to observe.
fn poll_while(active: Memo<bool>, refresh: impl Fn() + Clone + 'static) {
    #[cfg(feature = "hydrate")]
    Effect::new(
        move |previous: Option<Option<leptos::leptos_dom::helpers::IntervalHandle>>| {
            // Clear whatever the last run started before deciding again, so a
            // timer can never outlive the state that justified it.
            if let Some(Some(h)) = previous {
                h.clear();
            }

            if !active.get() {
                return None;
            }

            let refresh = refresh.clone();
            leptos::leptos_dom::helpers::set_interval_with_handle(
                refresh,
                std::time::Duration::from_millis(POLL_MS),
            )
            .ok()
        },
    );

    #[cfg(not(feature = "hydrate"))]
    {
        let _ = (active, refresh);
    }
}
