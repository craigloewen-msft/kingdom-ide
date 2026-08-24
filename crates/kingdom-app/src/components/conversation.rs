//! The plan's chamber: one plan's whole life, at its own URL.
//!
//! A `Plan` is both the unit of work and the unit of review, so it gets a place
//! to be reviewed in. Everything here reads from server state rather than local
//! signals, which is what lets a reload -- or a link shared between tabs --
//! rebuild the conversation exactly.

use crate::api::{draft_plan, finish_plan, get_kingdom, say};
use crate::app::KingdomState;
use kingdom_core::{Disposition, Entry, Plan, PlanId, PlanStatus, Speaker, Timestamp};
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

    // `state.error` is shared with the opening screen and the decree bar, so
    // whatever sits in it on arrival belongs to a screen the King has already
    // left. Clearing it on entry stops another view's complaint from being
    // shown against this plan.
    Effect::new(move |_| {
        let _ = plan_id.get();
        state.error.set(None);
    });

    let drafting = Memo::new(move |_| {
        plan.get()
            .map(|p| p.status == PlanStatus::Drafting)
            .unwrap_or(false)
    });

    // Drafting is kicked off here rather than by whoever opened the plan, so
    // that landing on a freshly opened plan and reloading one mid-draft take
    // the same path. `draft_plan` is idempotent while its busy mark is set, so
    // this cannot start a second draft over a running one.
    let draft = Action::new(move |id: &PlanId| {
        let id = id.to_string();
        async move {
            match draft_plan(id).await {
                Ok(_) => state.error.set(None),
                Err(e) => state.error.set(Some(e.to_string())),
            }
            // Refetch rather than patching the local copy: drafting also moved
            // the plan's status and busy mark, which the rail and map render.
            if let Ok(k) = get_kingdom().await {
                state.kingdom.set(k);
            }
        }
    });

    // A plan that is Drafting, busy with nothing and has heard nothing back is
    // one nobody has started yet: exactly the state `begin_plan` leaves behind.
    Effect::new(move |_| {
        let Some(p) = plan.get() else { return };
        let unstarted = p.status == PlanStatus::Drafting
            && !p.is_busy()
            && !p.said().any(|u| u.speaker == Speaker::Court);
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

    // Finishing touches the workspace on disk and may settle the plan, so the
    // whole kingdom is refetched rather than patched: the rail and the map both
    // render what just changed.
    let finish = Action::new(move |(id, how): &(PlanId, Disposition)| {
        let id = id.to_string();
        let how = *how;
        async move {
            match finish_plan(id, how).await {
                Ok(_) => state.error.set(None),
                Err(e) => state.error.set(Some(e.to_string())),
            }
            if let Ok(k) = get_kingdom().await {
                state.kingdom.set(k);
            }
        }
    });

    view! {
        <div class="chamber">
            {move || match plan.get() {
                Some(p) => view! {
                    <ChamberBody
                        plan=p
                        city=city_name
                        drafting=drafting
                        on_say=speak
                        finish=finish
                    />
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
    finish: Action<(PlanId, Disposition), ()>,
) -> impl IntoView {
    let state = expect_context::<KingdomState>();
    let (reply, set_reply) = signal(String::new());
    let (showing_done, set_showing_done) = signal(false);

    let id = StoredValue::new(plan.id.clone());
    let status = plan.status;
    let workspace_label = plan.workspace.mode.label();
    let workspace_path = plan.workspace.path.clone();
    let workspace_isolated = plan.workspace.is_isolated();
    // Named so the menu offers the branch this plan will really land on, rather
    // than a hopeful "main" it might not have been cut from. `StoredValue`
    // because the label sits inside a reactive closure, which must be `Fn`.
    let base = StoredValue::new(
        plan.workspace
            .base
            .clone()
            .unwrap_or_else(|| "the project".to_string()),
    );
    let settled = status.is_settled();
    let outcome = plan.outcome.clone();

    // Keeps the newest line in view. A conversation longer than the viewport
    // otherwise leaves the reply the King is waiting for below the fold.
    let log_ref = NodeRef::<leptos::html::Div>::new();
    let entry_count = plan.transcript.len();
    stick_to_bottom(
        log_ref,
        Signal::derive(move || (entry_count, drafting.get())),
    );

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

    let close_with = move |how: Disposition| {
        set_showing_done.set(false);
        finish.dispatch((id.get_value(), how));
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
                    // Isolation the King cannot see is isolation he cannot
                    // trust, so where this plan works is stated next to what is
                    // drafting it, with the full path on hover.
                    <span
                        class="chamber-workspace"
                        class:isolated=workspace_isolated
                        title=workspace_path
                    >
                        {workspace_label}
                    </span>
                </div>
            </div>
            <span class=format!("plan-badge plan-{}", status.css_suffix())>
                {status.label()}
            </span>
        </header>

        // Everything the King and the court have exchanged, oldest first, and
        // nothing else. Plan *state* -- the summary, the status -- lives in the
        // header or the rail; mixing it into this column put blocks derived
        // from the newest reply above the decree that opened the plan.
        <div class="chamber-log" node_ref=log_ref>
            <Transcript plan=plan.clone()/>

            <Show when={move || drafting.get()}>
                <div class="chat-msg drafting">
                    <span class="msg-at"></span>
                    <span class="msg-who">"Court"</span>
                    <span class="msg-body">"Drawing up the plan\u{2026}"</span>
                </div>
            </Show>
        </div>

        // Outside the log, because an error is not something anybody said. A
        // drafting failure is already recorded in the transcript as a note, in
        // its proper place in time; this strip is for what just went wrong.
        <Show when={move || state.error.get().is_some()}>
            <div class="chamber-error">
                {move || state.error.get().unwrap_or_default()}
            </div>
        </Show>

        // A settled plan is a record, not a place to type. The composer goes
        // and the outcome takes its place, so the chamber says what became of
        // the work rather than inviting more of it.
        <Show
            when={move || !settled}
            fallback={
                let stated = outcome
                    .as_ref()
                    .map(|o| o.summary())
                    .unwrap_or_else(|| "This plan is closed.".to_string());
                move || view! {
                    <div class="chamber-outcome">
                        <span class="outcome-mark">"\u{2713}"</span>
                        <span class="outcome-text">{stated.clone()}</span>
                    </div>
                }
            }
        >
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

                // Closing the plan sits beside sending to it, because they are
                // the two things the King does from here.
                <button
                    class="done-btn"
                    title="Finish with this plan"
                    disabled={move || drafting.get() || finish.pending().get()}
                    on:click=move |_| set_showing_done.update(|s| *s = !*s)
                >
                    {move || if finish.pending().get() { "Closing\u{2026}" } else { "Done" }}
                    <span class="chip-chevron">"\u{2304}"</span>
                </button>
            </div>

            // Two rows and no confirmation dialog: both endings are recoverable
            // -- one makes a revertable merge commit, the other keeps a patch of
            // the work -- and a modal would spend the King's attention to
            // prevent nothing.
            <Show when={move || showing_done.get()}>
                <div class="done-picker">
                    <div class="picker-head">
                        <span class="picker-title">"Finish with this plan"</span>
                        <button
                            class="picker-close"
                            on:click=move |_| set_showing_done.set(false)
                        >"\u{2715}"</button>
                    </div>

                    <ul class="done-list">
                        <li>
                            <button
                                class="done-row"
                                on:click=move |_| close_with(Disposition::Merge)
                            >
                                <span class="done-name">
                                    {move || format!("Merge into {}", base.get_value())}
                                </span>
                                <span class="done-detail">
                                    "Lands this work in the project and clears the \
                                     worktree. Stops and explains if git refuses."
                                </span>
                            </button>
                        </li>
                        <li>
                            <button
                                class="done-row"
                                on:click=move |_| close_with(Disposition::Archive)
                            >
                                <span class="done-name">"Archive"</span>
                                <span class="done-detail">
                                    "Sets this aside. The work is kept as a \
                                     patch and the branch cleared away."
                                </span>
                            </button>
                        </li>
                    </ul>
                </div>
            </Show>
        </Show>
    }
}

/// One plan's log, oldest first: what was said, and what happened.
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
                match line {
                    Entry::Said(u) => {
                        let royal = u.speaker == Speaker::King;
                        view! {
                            <div class="chat-msg" class:royal=royal>
                                <span class="msg-at">{clock(u.at)}</span>
                                <span class="msg-who">
                                    {if royal { "You" } else { "Court" }}
                                </span>
                                <span class="msg-body">{u.body.clone()}</span>
                            </div>
                        }
                        .into_any()
                    }
                    // Not a bubble: nothing said this, and dressing an app
                    // notice as counsel would misrepresent who is speaking.
                    Entry::Note(n) => view! {
                        <div class=format!("chat-note note-{}", n.kind.css_suffix())>
                            <span class="note-at">{clock(n.at)}</span>
                            {n.body.clone()}
                        </div>
                    }
                    .into_any(),
                }
            }
        </For>
    }
}

/// A log entry's time as a bare `HH:MM` in the King's own timezone.
///
/// Browser-only, and that is not a limitation: the stamp is UTC milliseconds and
/// only the browser knows what the King's clock reads. Under SSR this is the
/// empty string, which never reaches him -- the whole app is gated behind a
/// kingdom being open, and that only becomes true on the client.
fn clock(at: Option<Timestamp>) -> String {
    #[cfg(feature = "hydrate")]
    {
        let Some(Timestamp(ms)) = at else {
            return String::new();
        };
        let date = js_sys::Date::new(&wasm_bindgen::JsValue::from_f64(ms as f64));
        format!("{:02}:{:02}", date.get_hours(), date.get_minutes())
    }

    #[cfg(not(feature = "hydrate"))]
    {
        let _ = at;
        String::new()
    }
}

/// Scrolls the log to the bottom whenever `watch` changes.
///
/// Browser-only: under SSR there is nothing laid out to scroll.
fn stick_to_bottom(element: NodeRef<leptos::html::Div>, watch: Signal<(usize, bool)>) {
    #[cfg(feature = "hydrate")]
    Effect::new(move |_| {
        // Tracked so a new line or a draft starting re-runs this.
        let _ = watch.get();
        if let Some(el) = element.get() {
            el.set_scroll_top(el.scroll_height());
        }
    });

    #[cfg(not(feature = "hydrate"))]
    {
        let _ = (element, watch);
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
