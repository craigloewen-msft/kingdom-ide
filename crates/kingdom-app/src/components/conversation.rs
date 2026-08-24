//! The plan's chamber: one plan's whole life, at its own URL.
//!
//! A `Plan` is both the unit of work and the unit of review, so it gets a place
//! to be reviewed in. Everything here reads from server state rather than local
//! signals, which is what lets a reload -- or a link shared between tabs --
//! rebuild the conversation exactly.

use crate::api::{draft_plan, finish_plan, get_kingdom, say};
use crate::app::KingdomState;
use crate::components::Spyglass;
use kingdom_core::{Disposition, Entry, Plan, PlanId, PlanStatus, Speaker, Timestamp};
use leptos::prelude::*;
use leptos_router::hooks::use_params_map;

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
    //
    // Never an errand: an errand is driven by the call that sent it, which is
    // already looping over it. A King who opens one in the window before its
    // first turn must not start a second loop over the same plan. The server
    // refuses this too -- this is the half that avoids the pointless request.
    Effect::new(move |_| {
        let Some(p) = plan.get() else { return };
        let unstarted = p.status == PlanStatus::Drafting
            && !p.is_busy()
            && !p.is_errand()
            && !p.said().any(|u| u.speaker == Speaker::Court);
        if unstarted && !draft.pending().get_untracked() {
            draft.dispatch(p.id.clone());
        }
    });

    // Whatever the server says about this plan, as it says it. A draft started
    // by another page -- a reload mid-flight, a second tab -- lands here just
    // the same, because the chamber is watching the plan rather than awaiting
    // its own request.
    watch_plan(plan_id, move |updated| {
        state.kingdom.update(|k| k.absorb(updated));
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
                        live=plan
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
    /// The plan as it stood when this chamber was built.
    ///
    /// Read only for the parts of a plan that *cannot* change while it is open:
    /// its id, its city, the model drawing it, and the workspace it was cut in.
    /// Those are settled when the plan opens and never touched again.
    plan: Plan,
    /// The same plan, as the server currently has it.
    ///
    /// Everything that moves during a turn -- status, title, outcome -- must be
    /// read through here rather than off `plan`. A snapshot taken at
    /// construction renders once and then lies: the badge sat on "Drafting"
    /// after the court had already answered, because the status it was built
    /// from was a value and not a signal.
    live: Memo<Option<Plan>>,
    city: Memo<Option<String>>,
    drafting: Memo<bool>,
    on_say: Callback<(PlanId, String)>,
    finish: Action<(PlanId, Disposition), ()>,
) -> impl IntoView {
    let state = expect_context::<KingdomState>();
    let (reply, set_reply) = signal(String::new());
    let (showing_done, set_showing_done) = signal(false);

    let id = StoredValue::new(plan.id.clone());
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

    let status = Memo::new(move |_| live.get().map(|p| p.status).unwrap_or(PlanStatus::Drafting));
    let settled = Memo::new(move |_| status.get().is_settled());
    let title = Memo::new(move |_| live.get().map(|p| p.title).unwrap_or_default());

    // An errand is settled once and never changes, so it is read off the
    // snapshot rather than the live signal.
    let errand = StoredValue::new(plan.errand_for.clone());
    let is_errand = errand.get_value().is_some();
    // The plan that sent it, for the banner's link. Read live because the
    // parent's *title* is still being drafted while its errands run -- a
    // snapshot here would leave the banner naming a title that has moved on.
    let parent = Memo::new(move |_| {
        let sent_by = errand.get_value()?;
        let kingdom = state.kingdom.get();
        let parent = kingdom.plan(&sent_by.parent)?;
        Some((parent.id.clone(), parent.title.clone()))
    });
    let task = StoredValue::new(plan.prompt.clone());

    // Where the arrow goes. For an errand that is the plan that sent it; for
    // anything else, the realm. A parent this browser has not loaded falls back
    // to the realm rather than to a link that would lead nowhere.
    let back = Memo::new(move |_| match parent.get() {
        Some((id, _)) => (
            format!("/plan/{id}"),
            "Back to the plan that sent this errand".to_string(),
        ),
        None => ("/".to_string(), "Back to the realm".to_string()),
    });
    let outcome = Memo::new(move |_| {
        live.get()
            .and_then(|p| p.outcome)
            .map(|o| o.summary())
            .unwrap_or_else(|| "This plan is closed.".to_string())
    });

    // Keeps the newest line in view. A conversation longer than the viewport
    // otherwise leaves the reply the King is waiting for below the fold.
    let log_ref = NodeRef::<leptos::html::Div>::new();
    stick_to_bottom(
        log_ref,
        Signal::derive(move || {
            (
                live.get().map(|p| p.transcript.len()).unwrap_or(0),
                drafting.get(),
            )
        }),
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

    // Closed by default. Opening it is what attaches a viewer and starts the
    // screencast, and closing it is what stops one -- so this signal is the
    // King's direct control over whether Chrome is painting for an audience.
    let (watching, set_watching) = signal(false);

    view! {
        <header class="chamber-header" class:errand=is_errand>
            // For an errand, "back" is the plan that sent it rather than the
            // realm: that is where the King came from, and where he has to go
            // to steer the work. Retargeting the arrow he already knows beats
            // adding a second one beside it.
            <a
                class="back-link"
                href=move || back.get().0
                title=move || back.get().1
            >"\u{2190}"</a>
            <div class="chamber-id">
                // Where he is, above what he is reading -- the ordinary
                // breadcrumb shape. Present only for an errand, because for a
                // plan the King decreed there is no "above" to name.
                <Show when=move || is_errand>
                    <div class="chamber-crumb">
                        <span class="crumb-mark">"\u{26b1}"</span>
                        "Errand of "
                        {move || match parent.get() {
                            Some((id, title)) => view! {
                                <a class="crumb-parent" href=format!("/plan/{id}")>{title}</a>
                            }
                            .into_any(),
                            // The parent is not in the kingdom this browser has
                            // loaded -- a deep link, most likely. Still say what
                            // this is; the alternative reads as an ordinary plan.
                            None => view! {
                                <span class="crumb-parent">"another plan"</span>
                            }
                            .into_any(),
                        }}
                    </div>
                </Show>
                // An errand's title is cut from its task, so the full wording
                // goes on hover rather than onto a line of its own.
                <h1
                    class="chamber-title"
                    title=move || if is_errand { task.get_value() } else { String::new() }
                >{move || title.get()}</h1>
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
            <span class=move || format!("plan-badge plan-{}", status.get().css_suffix())>
                {move || {
                    // An errand is never reviewed and never merged: it reports
                    // to the court that sent it. Same states, honest words.
                    match (is_errand, status.get()) {
                        (true, PlanStatus::AwaitingReview) => "Reported",
                        (true, PlanStatus::Drafting) => "Working",
                        (_, s) => s.label(),
                    }
                }}
            </span>
            // The court's browser is headless, so without this the King's only
            // evidence of a browser flow is a list of tool names. Offered on
            // every plan rather than only those known to hold a session:
            // Kingdom has no field saying which do, and the panel's own
            // "no browser" state answers the question honestly for the rest.
            <button
                class="spyglass-toggle"
                class:open=move || watching.get()
                title="Watch this plan's browser"
                on:click=move |_| set_watching.update(|w| *w = !*w)
            >
                "\u{1F50D}"
            </button>
        </header>

        <Show when=move || watching.get()>
            <Spyglass plan=id.get_value()/>
        </Show>

        // Everything the King and the court have exchanged, oldest first, and
        // nothing else. Plan *state* -- the summary, the status -- lives in the
        // header or the rail; mixing it into this column put blocks derived
        // from the newest reply above the decree that opened the plan.
        <div class="chamber-log" node_ref=log_ref>
            <Transcript live=live/>

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
        //
        // An errand is a record for a different reason: it answers to the court
        // that sent it, and a second person steering it would leave the parent
        // reading a report on a conversation that changed under it.
        <Show
            when={move || !settled.get() && !is_errand}
            fallback={move || view! {
                <Show
                    when=move || !is_errand
                    fallback=move || view! {
                        <div class="chamber-outcome errand-outcome">
                            <span class="outcome-text">
                                "This errand reports to the plan that sent it. \
                                 To steer the work, speak there."
                            </span>
                        </div>
                    }
                >
                    <div class="chamber-outcome">
                        <span class="outcome-mark">"\u{2713}"</span>
                        <span class="outcome-text">{move || outcome.get()}</span>
                    </div>
                </Show>
            }}
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
                    {move || if drafting.get() { "Drafting\u{2026}" } else { "Send" }}
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

/// One plan's log, oldest first: what was said, what was done, and what
/// happened.
///
/// Reads the plan live rather than taking a copy, because the whole point of
/// the push socket is that lines arrive *during* a turn. A snapshot would show
/// the transcript as it was when the King walked in.
#[component]
fn Transcript(live: Memo<Option<Plan>>) -> impl IntoView {
    let plan_id = Memo::new(move |_| live.get().map(|p| p.id));
    view! {
        <For
            each={move || {
                live.get()
                    .map(|p| p.transcript)
                    .unwrap_or_default()
                    .into_iter()
                    .enumerate()
                    .collect::<Vec<_>>()
            }}
            // Keyed by position *and* by what the entry is, so a deed settling
            // in place re-renders. Keying on the index alone would leave a
            // running command showing "working..." forever -- the list is the
            // same length when a result arrives as it was when the call was
            // recorded.
            key=|(i, entry)| (*i, entry_version(entry))
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
                    // Also not a bubble, and for the first half of the same
                    // reason -- but a deed is the court *working*, which is the
                    // thing the King most wants to watch, so it gets its own
                    // shape rather than a note's.
                    //
                    // A question still waiting on him is the exception: that is
                    // not something to watch, it is something to do, so it is
                    // rendered as the thing to do.
                    Entry::Did(d) if is_open_question(&d) => {
                        view! { <Question deed=d plan=plan_id/> }.into_any()
                    }
                    // Errands are not a line of output either: the call's own
                    // text result is a summary, and the thing the King wants is
                    // the list of agents it sent and a way into each one.
                    Entry::Did(d) if d.tool == "spawn_agents" => {
                        view! { <Errands deed=d plan=plan_id/> }.into_any()
                    }
                    Entry::Did(d) => view! { <DeedLine deed=d/> }.into_any(),
                }
            }
        </For>
    }
}

/// The errands one call sent, each a way into its own conversation.
///
/// Rendered from the *plans*, not from the deed's text output. The output is a
/// summary written for the model; the King wants to know who is out there, what
/// each was asked, and how each is getting on -- and to be able to go and read
/// one. None of that survives being flattened into a paragraph.
///
/// The rows are live for free: `herald::proclaim` announces an errand on its
/// parent's channel as well as its own, and `Kingdom::absorb` files a plan the
/// chamber has not seen before. So these come from the same signal everything
/// else reads, with no separate subscription.
#[component]
fn Errands(deed: kingdom_core::Deed, plan: Memo<Option<PlanId>>) -> impl IntoView {
    let state = expect_context::<KingdomState>();
    let deed_id = StoredValue::new(deed.id.clone());
    let running = deed.in_flight();

    let errands = Memo::new(move |_| {
        let Some(parent) = plan.get() else {
            return Vec::new();
        };
        state
            .kingdom
            .get()
            .errands_of(&parent, &deed_id.get_value())
            .cloned()
            .collect::<Vec<_>>()
    });

    // The tasks as the model asked for them. Shown only until the errands
    // themselves exist: between the call being recorded and the plans being
    // created there is a moment with nothing to list, and an empty box there
    // would read as a call that sent nobody.
    let asked = StoredValue::new(
        deed.input
            .get("tasks")
            .and_then(|t| t.as_array())
            .map(Vec::as_slice)
            .unwrap_or_default()
            .iter()
            .filter_map(|t| t.get("task").and_then(|t| t.as_str()))
            .map(|t| t.to_string())
            .collect::<Vec<_>>(),
    );

    view! {
        <div class="chat-errands" class:running=running>
            <div class="errands-head">
                <span class="errands-mark">"\u{26b1}"</span>
                <span class="errands-who">
                    {move || {
                        let sent = errands.get().len().max(asked.get_value().len());
                        match sent {
                            1 => "The court sent an errand".to_string(),
                            n => format!("The court sent {n} errands"),
                        }
                    }}
                </span>
            </div>

            <ul class="errand-list">
                {move || {
                    let sent = errands.get();
                    if sent.is_empty() {
                        // Not yet created, or a record from before this chamber
                        // knew them: show what was asked, without a link that
                        // would lead nowhere.
                        return asked
                            .get_value()
                            .into_iter()
                            .map(|task| view! {
                                <li class="errand-row pending">
                                    <span class="errand-dot"></span>
                                    <span class="errand-task">{task}</span>
                                    <span class="errand-state">"sending\u{2026}"</span>
                                </li>
                            })
                            .collect_view()
                            .into_any();
                    }

                    sent.into_iter()
                        .map(|errand| {
                            let href = format!("/plan/{}", errand.id);
                            let status = errand.status;
                            let working = errand.working_on.clone();
                            view! {
                                <li class="errand-row">
                                    <a class="errand-link" href=href>
                                        <span
                                            class=format!(
                                                "errand-dot status-{}",
                                                status.css_suffix(),
                                            )
                                            style:background=status.color()
                                        ></span>
                                        <span class="errand-task">{errand.prompt.clone()}</span>
                                        <span class="errand-state">
                                            {match (status, working) {
                                                // What it is doing beats what it
                                                // is: "Drafting" is true of every
                                                // working errand and tells the
                                                // King nothing.
                                                (_, Some(doing)) => doing,
                                                (s, None) => errand_status(s).to_string(),
                                            }}
                                        </span>
                                    </a>
                                </li>
                            }
                        })
                        .collect_view()
                        .into_any()
                }}
            </ul>
        </div>
    }
}

/// How an errand's state reads, which is not how a plan's does.
///
/// An errand is never reviewed and never merged -- it reports to the court that
/// sent it. Relabelled here rather than given its own `PlanStatus` variant: a
/// sixth state would ripple through the map legend, `ALL` and every match on
/// plan state to buy one word.
fn errand_status(status: PlanStatus) -> &'static str {
    match status {
        PlanStatus::Drafting => "working\u{2026}",
        PlanStatus::AwaitingReview => "reported",
        PlanStatus::Failed => "could not finish",
        PlanStatus::Merged | PlanStatus::Archived => status.label(),
    }
}

/// A question the court has stopped to ask, rendered where it was asked./// A question the court has stopped to ask, rendered where it was asked.
///
/// Inline in the transcript rather than as a modal. A modal would be the
/// obvious choice and is the wrong one: it puts the question in front of the
/// King with the work that prompted it hidden behind it, so he answers without
/// the context he needs. Here the reasoning and the commands that led to the
/// question are right above it, and he can scroll.
#[component]
fn Question(deed: kingdom_core::Deed, plan: Memo<Option<PlanId>>) -> impl IntoView {
    let state = expect_context::<KingdomState>();
    let deed_id = StoredValue::new(deed.id.clone());
    let questions = StoredValue::new(parse_questions(&deed.input));
    let (sent, set_sent) = signal(false);

    let reply = move |answer: String| {
        let Some(plan_id) = plan.get_untracked() else {
            return;
        };
        // Locked as soon as he answers, so a double-click cannot send twice --
        // the second would find nothing waiting and report a confusing failure
        // for an answer that in fact landed.
        set_sent.set(true);
        let deed = deed_id.get_value();
        leptos::task::spawn_local(async move {
            if let Err(e) = crate::api::answer_question(plan_id.to_string(), deed, answer).await {
                state.error.set(Some(e.to_string()));
                set_sent.set(false);
            }
        });
    };

    view! {
        <div class="chat-question" class:answered=move || sent.get()>
            <div class="question-head">
                <span class="question-mark">"\u{2637}"</span>
                <span class="question-who">"The court asks"</span>
            </div>

            {move || {
                questions
                    .get_value()
                    .into_iter()
                    .map(|q| {
                        let options = q
                            .options
                            .into_iter()
                            .map(|opt| {
                                let chosen = opt.label.clone();
                                let detail = opt.description.clone();
                                let has_detail = !detail.is_empty();
                                view! {
                                    <li>
                                        <button
                                            class="question-option"
                                            disabled=move || sent.get()
                                            on:click=move |_| reply(chosen.clone())
                                        >
                                            <span class="option-label">{opt.label}</span>
                                            <Show when=move || has_detail>
                                                <span class="option-detail">
                                                    {detail.clone()}
                                                </span>
                                            </Show>
                                        </button>
                                    </li>
                                }
                            })
                            .collect_view();

                        view! {
                            <div class="question-block">
                                <p class="question-text">{q.question}</p>
                                <ul class="question-options">{options}</ul>
                            </div>
                        }
                    })
                    .collect_view()
            }}

            // The listed options are the court's guesses at what the King might
            // want. He is the one deciding, so he must be able to say something
            // that was not on the list.
            <QuestionFreeText sent=sent on_answer=Callback::new(reply)/>
        </div>
    }
}

/// The "something else" line under a question's options.
#[component]
fn QuestionFreeText(sent: ReadSignal<bool>, on_answer: Callback<String>) -> impl IntoView {
    let (text, set_text) = signal(String::new());

    let send = move || {
        let words = text.get_untracked().trim().to_string();
        if words.is_empty() || sent.get_untracked() {
            return;
        }
        set_text.set(String::new());
        on_answer.run(words);
    };

    view! {
        <div class="question-own-words">
            <input
                class="decree-input"
                r#type="text"
                placeholder="Or say what you want in your own words\u{2026}"
                prop:value=move || text.get()
                disabled=move || sent.get()
                on:input=move |ev| set_text.set(event_target_value(&ev))
                on:keydown=move |ev| { if ev.key() == "Enter" { send(); } }
            />
        </div>
    }
}

/// One thing the court wants decided.
#[derive(Clone)]
struct Asked {
    question: String,
    options: Vec<AskedOption>,
}

#[derive(Clone)]
struct AskedOption {
    label: String,
    description: String,
}

/// Reads the questions out of a call's arguments.
///
/// Tolerant on purpose: these are a model's JSON, not a shape we declared, and
/// a missing `description` or a malformed option must not cost the King the
/// whole question. A question that renders with one option missing is still
/// answerable; one that fails to render at all leaves the court parked with
/// nothing on screen to unpark it.
fn parse_questions(input: &serde_json::Value) -> Vec<Asked> {
    input
        .get("questions")
        .and_then(|q| q.as_array())
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .filter_map(|q| {
            let question = q.get("question")?.as_str()?.to_string();
            let options = q
                .get("options")
                .and_then(|o| o.as_array())
                .map(Vec::as_slice)
                .unwrap_or_default()
                .iter()
                .filter_map(|o| {
                    Some(AskedOption {
                        label: o.get("label")?.as_str()?.to_string(),
                        description: o
                            .get("description")
                            .and_then(|d| d.as_str())
                            .unwrap_or_default()
                            .to_string(),
                    })
                })
                .collect();
            Some(Asked { question, options })
        })
        .collect()
}

/// True for a question the King has not answered yet.
///
/// Once answered it is ordinary history and renders as any other deed, which is
/// what stops him answering the same question twice.
fn is_open_question(entry: &kingdom_core::Deed) -> bool {
    entry.tool == "ask_user_question" && entry.in_flight()
}

/// Distinguishes an entry from a later version of *itself*.
///
/// Only deeds have a later version: an utterance and a note are written once
/// and never touched, but a deed is recorded in flight and settled afterwards.
/// This is what tells the keyed list those are two different things to render.
fn entry_version(entry: &Entry) -> u8 {
    match entry {
        Entry::Did(d) if d.in_flight() => 1,
        Entry::Did(_) => 2,
        _ => 0,
    }
}

/// One tool call, collapsed to a line the King can skim and expand when it
/// matters.
///
/// Collapsed by default because a transcript that renders every command's full
/// output inline is unreadable at exactly the moment it becomes interesting:
/// the King is watching for *what the court is doing*, and a thousand lines of
/// build log buries that. The summary line is the answer; the detail is one
/// click away for when it is not.
#[component]
fn DeedLine(deed: kingdom_core::Deed) -> impl IntoView {
    use kingdom_core::DeedOutcome;

    let (open, set_open) = signal(false);

    let running = deed.in_flight();
    let state = match &deed.outcome {
        Some(o) => o.css_suffix(),
        None => "running",
    };
    let mark = match state {
        "done" => "\u{2713}",
        "refused" => "\u{2715}",
        _ => "\u{25cf}",
    };
    let at = clock(deed.at);
    let tool = deed.tool.clone();

    // The arguments matter more than the tool's name -- "bash" tells the King
    // nothing, `cargo test` tells him everything -- so the most telling
    // argument is promoted onto the collapsed line.
    let gist = telling_argument(&deed.input);
    // `StoredValue` because these sit inside `Show` bodies, which must be `Fn`:
    // a closure that moves an owned String is `FnOnce` and can only render once.
    let input = StoredValue::new(pretty(&deed.input));
    let output = StoredValue::new(match &deed.outcome {
        Some(DeedOutcome::Done { output, .. }) => output.clone(),
        Some(DeedOutcome::Refused { reason }) => reason.clone(),
        None => String::new(),
    });
    let has_output = !output.read_value().is_empty();

    view! {
        <div class=format!("chat-deed deed-{state}")>
            <button class="deed-line" on:click=move |_| set_open.update(|o| *o = !*o)>
                <span class="deed-at">{at}</span>
                <span class="deed-mark">{mark}</span>
                <span class="deed-tool">{tool}</span>
                <span class="deed-gist">{gist}</span>
                <Show when=move || running>
                    <span class="deed-running">"working\u{2026}"</span>
                </Show>
                <span class="deed-chevron">
                    {move || if open.get() { "\u{2303}" } else { "\u{2304}" }}
                </span>
            </button>

            <Show when=move || open.get()>
                <div class="deed-detail">
                    <pre class="deed-input">{move || input.get_value()}</pre>
                    <Show when=move || has_output>
                        <pre class="deed-output">{move || output.get_value()}</pre>
                    </Show>
                </div>
            </Show>
        </div>
    }
}

/// The one argument worth showing on a collapsed deed line.
///
/// Every tool names its subject differently, and there is no shared field to
/// rely on. Rather than teach this component about each tool's schema -- which
/// would have to be updated every time a tool is added, and would be silently
/// wrong when it was not -- it prefers a short list of conventional names and
/// otherwise shows the first string it finds. Being approximate is fine here:
/// the exact arguments are one click away.
fn telling_argument(input: &serde_json::Value) -> String {
    const PREFERRED: &[&str] = &["cmd", "path", "pattern", "url", "selector", "query"];

    let Some(fields) = input.as_object() else {
        return String::new();
    };

    let found = PREFERRED
        .iter()
        .find_map(|k| fields.get(*k).and_then(|v| v.as_str()))
        .or_else(|| fields.values().find_map(|v| v.as_str()));

    match found {
        Some(text) => ellipsise(text, 80),
        None => String::new(),
    }
}

fn pretty(value: &serde_json::Value) -> String {
    serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string())
}

fn ellipsise(text: &str, max: usize) -> String {
    let text = text.trim();
    if text.chars().count() <= max {
        return text.to_string();
    }
    format!("{}\u{2026}", text.chars().take(max).collect::<String>())
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

/// Watches one plan over the push socket, handing each proclamation to `absorb`.
///
/// Browser-only: under SSR there is no socket and the first render is served
/// from server state directly.
///
/// Reconnection is a plain fixed-delay retry with no backoff ladder and no
/// give-up: the server is on loopback, so a dropped socket means it is
/// restarting, and the honest response is to keep trying until it is back. The
/// reconnect costs nothing to get right because the socket's opening message is
/// the whole plan -- there is no cursor to resume from and nothing that can be
/// missed while it was down. See `herald.rs`.
fn watch_plan(plan_id: Memo<Option<PlanId>>, absorb: impl Fn(Plan) + Clone + 'static) {
    #[cfg(feature = "hydrate")]
    Effect::new(move |previous: Option<Option<PlanWatch>>| {
        // Close the previous socket before opening another, so moving between
        // chambers cannot leave a socket behind feeding a plan nobody is
        // looking at.
        drop(previous);

        let id = plan_id.get()?;
        Some(PlanWatch::open(&id, absorb.clone()))
    });

    #[cfg(not(feature = "hydrate"))]
    {
        let _ = (plan_id, absorb);
    }
}

/// An open watch on one plan, which closes itself when dropped.
#[cfg(feature = "hydrate")]
struct PlanWatch {
    socket: web_sys::WebSocket,
    /// Kept alive for the socket's lifetime: a closure passed to JS and then
    /// dropped on the Rust side would be called after being freed.
    _on_message: wasm_bindgen::closure::Closure<dyn FnMut(web_sys::MessageEvent)>,
    _on_close: wasm_bindgen::closure::Closure<dyn FnMut()>,
    /// Cleared on drop, so a retry queued by a closing socket does not reopen
    /// a chamber the King has already left.
    retry: std::rc::Rc<std::cell::Cell<Option<i32>>>,
}

#[cfg(feature = "hydrate")]
impl PlanWatch {
    /// How long to wait before reopening a dropped socket.
    const RETRY_MS: i32 = 1000;

    fn open(id: &PlanId, absorb: impl Fn(Plan) + Clone + 'static) -> Self {
        use wasm_bindgen::closure::Closure;
        use wasm_bindgen::JsCast;

        let socket = web_sys::WebSocket::new(&Self::url(id))
            .expect("the chamber's watch socket should be constructible");

        let on_message = Closure::<dyn FnMut(web_sys::MessageEvent)>::new({
            let absorb = absorb.clone();
            move |event: web_sys::MessageEvent| {
                let Some(text) = event.data().as_string() else {
                    return;
                };
                // A message that will not parse means the server sent a shape
                // this bundle does not know -- a stale tab after a rebuild,
                // most likely. Dropping it leaves the chamber showing the last
                // good state, which is better than tearing it down.
                if let Ok(plan) = serde_json::from_str::<Plan>(&text) {
                    absorb(plan);
                }
            }
        });
        socket.set_onmessage(Some(on_message.as_ref().unchecked_ref()));

        let retry = std::rc::Rc::new(std::cell::Cell::new(None));
        let on_close = Closure::<dyn FnMut()>::new({
            let id = id.clone();
            let retry = retry.clone();
            move || {
                let Some(window) = web_sys::window() else {
                    return;
                };
                let reopen = Closure::once_into_js({
                    let id = id.clone();
                    let absorb = absorb.clone();
                    let retry = retry.clone();
                    move || {
                        retry.set(None);
                        // Reopening replaces this watch's socket in place. The
                        // effect that owns it is not re-run, because nothing it
                        // tracked changed -- the King is still in the same
                        // chamber.
                        //
                        // Deliberately leaked: the reopened watch outlives this
                        // callback and has no owner to hand it back to. Bounded
                        // by the number of disconnects in one chamber visit,
                        // and the socket it holds is closed by the browser when
                        // the page goes.
                        std::mem::forget(PlanWatch::open(&id, absorb));
                    }
                });
                if let Ok(handle) = window.set_timeout_with_callback_and_timeout_and_arguments_0(
                    reopen.unchecked_ref(),
                    Self::RETRY_MS,
                ) {
                    retry.set(Some(handle));
                }
            }
        });
        socket.set_onclose(Some(on_close.as_ref().unchecked_ref()));

        Self {
            socket,
            _on_message: on_message,
            _on_close: on_close,
            retry,
        }
    }

    /// The socket's address, derived from the page's own origin so it follows
    /// the server wherever it is served from, and upgrades to `wss` when the
    /// page itself is secure.
    fn url(id: &PlanId) -> String {
        let location = web_sys::window().expect("a browser has a window").location();
        let secure = location.protocol().map(|p| p == "https:").unwrap_or(false);
        let host = location.host().unwrap_or_default();
        let scheme = if secure { "wss" } else { "ws" };
        format!("{scheme}://{host}/watch/plan/{id}")
    }
}

#[cfg(feature = "hydrate")]
impl Drop for PlanWatch {
    fn drop(&mut self) {
        // Order matters: clear the close handler before closing, or closing
        // deliberately would schedule the reconnect this drop exists to stop.
        self.socket.set_onclose(None);
        self.socket.set_onmessage(None);
        let _ = self.socket.close();

        if let (Some(handle), Some(window)) = (self.retry.take(), web_sys::window()) {
            window.clear_timeout_with_handle(handle);
        }
    }
}
