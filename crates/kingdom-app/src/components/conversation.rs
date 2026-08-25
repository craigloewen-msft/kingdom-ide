//! The plan's conversation: one plan's whole life, at its own URL.
//!
//! A `Plan` is both the unit of work and the unit of review, so it gets a place
//! to be reviewed in. Everything here reads from server state rather than local
//! signals, which is what lets a reload -- or a link shared between tabs --
//! rebuild the conversation exactly.

use crate::api::{approve_plan, draft_plan, finish_plan, get_kingdom, say, set_aside_plan};
use crate::app::KingdomState;
use crate::components::prompt_bar::autogrow;
use crate::components::resizer::{restore_width, Bounds, Grows, Resizer};
use crate::components::BrowserView;
use crate::components::Prose;
use kingdom_core::{
    Disposition, Entry, Permissions, Plan, PlanId, PlanStatus, Proposal, Speaker, Timestamp,
    ToolCall,
};
use leptos::prelude::*;
use leptos_router::hooks::use_params_map;

/// How wide the spyglass may be dragged. Narrower than the minimum and the
/// court's 1024-wide page is scaled past reading; wider and the transcript
/// stops being the thing under review.
const SPYGLASS_BOUNDS: Bounds = Bounds {
    min: 320.0,
    max: 900.0,
    default: 480.0,
};

const SPYGLASS_WIDTH_KEY: &str = "kingdom.spyglass_width";

/// The browser deed the panel should caption itself with, if any.
///
/// The one in flight, or failing that the last one that finished -- so a panel
/// opened between calls says what was just done rather than nothing at all.
///
/// Scanned from the back because this runs on every plan update, and a busy
/// turn's transcript is long: the answer is nearly always within a few entries
/// of the end.
fn browsing(transcript: &[Entry]) -> Option<ToolCall> {
    let mut latest = None;
    for entry in transcript.iter().rev() {
        let Entry::Tool(call) = entry else { continue };
        if !call.tool.starts_with("browser_") {
            continue;
        }
        if call.in_flight() {
            return Some(call.clone());
        }
        // Keep the first settled one seen -- it is the most recent -- but keep
        // looking, because an in-flight call behind it is the better answer. A
        // turn can issue two browser calls at once, and then either is honest.
        if latest.is_none() {
            latest = Some(call.clone());
        }
    }
    latest
}

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

    // The rail and the map should agree with the URL about where the user is.
    Effect::new(move |_| {
        if let Some(p) = plan.get() {
            state.selected.set(Some(p.city));
        }
    });

    // `state.error` is shared with the opening screen and the prompt bar, so
    // whatever sits in it on arrival belongs to a screen the user has already
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

    // A plan that is Drafting, busy with nothing and whose court has done
    // nothing is one nobody has started: exactly the state `begin_plan` leaves
    // behind.
    //
    // "Done nothing" means the court has neither spoken *nor acted*, and the
    // second half is load-bearing. This used to ask only whether the court had
    // spoken, which was a safe proxy while every turn ended in speech -- but a
    // counselling plan ends its turn on a `propose_plan` call, having said
    // nothing at all. Under the old test such a plan read as unstarted on every
    // mount, so approving one dispatched a second turn loop racing the first,
    // and the court's reply landed in the transcript twice.
    //
    // Kingdom's own notes are deliberately not evidence either way: a plan is
    // born with a workspace note already in its log, so counting notes here
    // would mean no plan ever started at all.
    //
    // Never a subagent: a subagent is driven by the call that sent it, which is
    // already looping over it. A user who opens one in the window before its
    // first turn must not start a second loop over the same plan. The server
    // refuses this too -- this is the half that avoids the pointless request.
    Effect::new(move |_| {
        let Some(p) = plan.get() else { return };
        let model_has_moved = p.transcript.iter().any(|e| match e {
            Entry::Message(u) => u.speaker == Speaker::Assistant,
            Entry::Tool(_) => true,
            Entry::Note(_) => false,
        });
        let unstarted =
            p.status == PlanStatus::Drafting && !p.is_busy() && !p.is_subagent() && !model_has_moved;
        if unstarted && !draft.pending().get_untracked() {
            draft.dispatch(p.id.clone());
        }
    });

    // Whatever the server says about this plan, as it says it. A draft started
    // by another page -- a reload mid-flight, a second tab -- lands here just
    // the same, because the conversation is watching the plan rather than
    // awaiting its own request.
    watch_plan(plan_id, move |updated| {
        state.kingdom.update(|k| k.insert(updated));
    });

    // The user's words land first, then the model is asked -- so his half of
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

    // Sending the court round again, for the chamber's other reason to: the
    // King accepting a plan. Wrapping the action rather than handing it down
    // keeps `draft`'s pending state -- which the composer reads -- owned here.
    let redraft = Callback::new(move |id: PlanId| {
        draft.dispatch(id);
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
                    <ConversationBody
                        plan=p
                        live=plan
                        city=city_name
                        drafting=drafting
                        on_say=speak
                        on_draft=redraft
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
fn ConversationBody(
    /// The plan as it stood when this conversation was built.
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
    /// after the model had already answered, because the status it was built
    /// from was a value and not a signal.
    live: Memo<Option<Plan>>,
    city: Memo<Option<String>>,
    drafting: Memo<bool>,
    on_say: Callback<(PlanId, String)>,
    /// Sends the court round again. Used after the King accepts a plan, which
    /// grants authority but deliberately makes no model call of its own.
    on_draft: Callback<PlanId>,
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

    // The plan the model has put to the user, if it is their move. Read live,
    // because it arrives mid-conversation over the watch socket -- and read
    // through `standing_proposal` rather than by testing the fields here, so
    // the browser and the server agree on what "awaiting their word" means.
    let proposal = Memo::new(move |_| live.get().and_then(|p| p.standing_proposal().cloned()));
    // What the model may touch right now. Widens exactly once, on approval.
    let permissions =
        Memo::new(move |_| live.get().map(|p| p.permissions).unwrap_or(Permissions::Full));

    // How full the model's window is, read live because it moves every round --
    // including mid tool loop, which is when it is worth watching. `None` until
    // the court has answered once, and always `None` for a provider that
    // reports no usage or declares no window; the header simply says nothing
    // rather than drawing a bar over a number nobody measured.
    let context = Memo::new(move |_| {
        let usage = live.get()?.context?;
        let percent = usage.percent()?;
        Some((percent, kingdom_core::window_label(usage.window), usage.tokens))
    });

    // A subagent is settled once and never changes, so it is read off the
    // snapshot rather than the live signal.
    let subagent = StoredValue::new(plan.spawned_by.clone());
    let is_subagent = subagent.get_value().is_some();
    // The plan that sent it, for the banner's link. Read live because the
    // parent's *title* is still being drafted while its subagents run -- a
    // snapshot here would leave the banner naming a title that has moved on.
    let parent = Memo::new(move |_| {
        let sent_by = subagent.get_value()?;
        let kingdom = state.kingdom.get();
        let parent = kingdom.plan(&sent_by.parent)?;
        Some((parent.id.clone(), parent.title.clone()))
    });
    let task = StoredValue::new(plan.prompt.clone());

    // Where the arrow goes. For a subagent that is the plan that sent it; for
    // anything else, the fixture. A parent this browser has not loaded falls
    // back to the fixture rather than to a link that would lead nowhere.
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
    // otherwise leaves the reply the user is waiting for below the fold.
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

    // Same composer behaviour as the decree bar: it grows with the reply and
    // shrinks back once it is sent.
    let composer = NodeRef::<leptos::html::Textarea>::new();
    Effect::new(move |_| {
        reply.track();
        if let Some(el) = composer.get() {
            autogrow(&el);
        }
    });

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

    // Accepting a plan makes no model call of its own -- it grants, and then
    // the court is asked again through exactly the path `say` uses. Splitting
    // them is what lets the grant land in the chamber immediately rather than
    // behind the first round of real work.
    let accept = Callback::new(move |_: ()| {
        let plan_id = id.get_value();
        leptos::task::spawn_local(async move {
            match approve_plan(plan_id.to_string()).await {
                Ok(_) => {
                    state.error.set(None);
                    if let Ok(k) = get_kingdom().await {
                        state.kingdom.set(k);
                    }
                    // Straight on to the work. The court has hands now.
                    on_draft.run(plan_id);
                }
                Err(e) => state.error.set(Some(e.to_string())),
            }
        });
    });

    // Setting a proposal aside is deliberately *not* an ending: the plan stays
    // exactly where it was, with its composer live, so the King can say what he
    // actually wants instead. Archiving is how a plan ends, and it is still
    // reached the way it always was.
    let set_aside = Callback::new(move |_: ()| {
        let plan_id = id.get_value();
        leptos::task::spawn_local(async move {
            match set_aside_plan(plan_id.to_string()).await {
                Ok(updated) => {
                    state.error.set(None);
                    state.kingdom.update(|k| k.insert(updated));
                }
                Err(e) => state.error.set(Some(e.to_string())),
            }
        });
    });

    // Closed by default. Opening it is what attaches a viewer and starts the
    // screencast, and closing it is what stops one -- so this signal is the
    // user's direct control over whether Chrome is painting for an audience.
    let (watching, set_watching) = signal(false);

    // How wide the panel is, in pixels. A plain local signal rather than
    // something on `KingdomState`: like the rail's collapse set, it is a view
    // preference and nothing outside this view reads it.
    let spyglass_width = RwSignal::new(SPYGLASS_BOUNDS.default);
    restore_width(spyglass_width, SPYGLASS_WIDTH_KEY, SPYGLASS_BOUNDS);

    // What the court is doing to the page right now, for the panel's caption.
    // Read off the live plan, so it moves during a turn like everything else
    // the watch socket carries.
    let browser_deed = Memo::new(move |_| live.get().and_then(|p| browsing(&p.transcript)));

    view! {
        // A flex row: the chamber's own column, and the spyglass beside it.
        // Side by side rather than stacked, because the transcript and the
        // page it describes are read together -- stacking them made each one
        // shorter to make room for the other.
        <div class="chamber-frame">
            <div class="chamber-column">
                <header class="chamber-header" class:subagent=is_subagent>
                    // For a subagent, "back" is the plan that sent it rather than the
                    // fixture: that is where the user came from, and where he has to go
                    // to steer the work. Retargeting the arrow he already knows beats
                    // adding a second one beside it.
                    <a
                        class="back-link"
                        href=move || back.get().0
                        title=move || back.get().1
                    >"\u{2190}"</a>
                    <div class="chamber-id">
                        // Where he is, above what he is reading -- the ordinary
                        // breadcrumb shape. Present only for a subagent, because for a
                        // plan the user opened there is no "above" to name.
                        <Show when=move || is_subagent>
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
                        // A subagent's title is cut from its task, so the full wording
                        // goes on hover rather than onto a line of its own.
                        <h1
                            class="chamber-title"
                            title=move || if is_subagent { task.get_value() } else { String::new() }
                        >{move || title.get()}</h1>
                        <div class="chamber-meta">
                            <span class="chamber-city">
                                {move || city.get().unwrap_or_else(|| "unknown city".into())}
                            </span>
                            <span class="chamber-model">{plan.choice().label()}</span>
                            // Isolation the user cannot see is isolation he cannot
                            // trust, so where this plan works is stated next to what is
                            // drafting it, with the full path on hover.
                            <span
                                class="chamber-workspace"
                                class:isolated=workspace_isolated
                                title=workspace_path
                            >
                                {workspace_label}
                            </span>
                            // How much of the model's window this conversation
                            // is filling. A long chamber creeps toward the
                            // limit with no other warning than the refusal at
                            // the end of it, so this sits with the other
                            // provenance facts rather than announcing itself.
                            <Show when=move || context.get().is_some()>
                                {move || context.get().map(|(percent, window, tokens)| view! {
                                    <span
                                        class="chamber-context"
                                        title=format!(
                                            "{tokens} tokens of this model's {window} context \
                                             window, as the provider counted them on the last turn",
                                        )
                                    >
                                        <span class="context-track">
                                            <span
                                                class="context-fill"
                                                style=format!("width: {percent}%")
                                            ></span>
                                        </span>
                                        <span class="context-label">
                                            {format!("{percent}% of {window}")}
                                        </span>
                                    </span>
                                })}
                            </Show>
                        </div>
                    </div>
                    <span class=move || format!("plan-badge plan-{}", status.get().css_suffix())>
                        {move || {
                            // A subagent is never reviewed and never merged: it reports
                            // to the model that sent it. Same states, honest words.
                            //
                            // A plan with something in front of the user is the other
                            // case where "Awaiting review" is too vague to be useful --
                            // it does not say that the wait is on *them*, or that there
                            // is a button. A label, deliberately, and not a sixth
                            // `PlanStatus`: nothing about the state machine changed.
                            match (is_subagent, status.get()) {
                                (true, PlanStatus::AwaitingReview) => "Reported",
                                (true, PlanStatus::Drafting) => "Working",
                                (false, PlanStatus::AwaitingReview) if proposal.get().is_some() => {
                                    "Proposal"
                                }
                                (_, s) => s.label(),
                            }
                        }}
                    </span>
                    // The model's browser is headless, so without this the user's only
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

                // Everything the user and the model have exchanged, oldest first, and
                // nothing else. Plan *state* -- the summary, the status -- lives in the
                // header or the rail; mixing it into this column put blocks derived
                // from the newest reply above the prompt that opened the plan.
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

                // The King's move, when the court has put something to him. Above the
                // composer rather than inside the log for the same reason the error
                // strip is: the log is what was said and done, and this is a decision
                // still to make. It sits where his attention already is.
                <Show when=move || proposal.get().is_some()>
                    {move || proposal.get().map(|put| view! {
                        <ProposalCard
                            proposal=put
                            busy=drafting
                            on_accept=accept
                            on_set_aside=set_aside
                        />
                    })}
                </Show>

                // A settled plan is a record, not a place to type. The composer goes
                // and the outcome takes its place, so the conversation says what became
                // of the work rather than inviting more of it.
                //
                // A subagent is a record for a different reason: it answers to the
                // model that sent it, and a second person steering it would leave the
                // parent reading a report on a conversation that changed under it.
                <Show
                    when={move || !settled.get() && !is_subagent}
                    fallback={move || view! {
                        <Show
                            when=move || !is_subagent
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
                        // Enter sends; Shift+Enter makes a line, so a long reply
                        // does not have to be one paragraph.
                        <textarea
                            class="decree-input"
                            node_ref=composer
                            rows="1"
                            placeholder=move || {
                                // The composer says which conversation this is. While
                                // the court is drawing something up, "ask for a change"
                                // would be inviting him to steer work that has not
                                // started -- and once a plan is in front of him, the
                                // useful thing to type is what he would change *about
                                // the plan*.
                                if permissions.get().is_full() {
                                    "Say more, or ask for a change\u{2026}"
                                } else if proposal.get().is_some() {
                                    "Say what you would change about this plan\u{2026}"
                                } else {
                                    "Say more about what you want\u{2026}"
                                }
                            }
                            prop:value=move || reply.get()
                            disabled={move || drafting.get()}
                            on:input=move |ev| set_reply.set(event_target_value(&ev))
                            on:keydown=move |ev| {
                                if ev.key() == "Enter" && !ev.shift_key() {
                                    ev.prevent_default();
                                    submit();
                                }
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
                        // the two things the user does from here.
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
                    // the work -- and a modal would spend the user's attention to
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
            </div>

            // The handle exists only while the panel does: a divider with
            // nothing on one side of it is a control that does nothing.
            <Show when=move || watching.get()>
                <Resizer
                    width=spyglass_width
                    grows=Grows::Leftwards
                    bounds=SPYGLASS_BOUNDS
                    storage_key=SPYGLASS_WIDTH_KEY
                    class="spyglass-resizer"
                />
                <BrowserView
                    plan=id.get_value()
                    deed=browser_deed
                    width=spyglass_width
                />
            </Show>
        </div>
    }
}

/// One plan's log, oldest first: what was said, what was done, and what
/// happened.
///
/// Reads the plan live rather than taking a copy, because the whole point of
/// the push socket is that lines arrive *during* a turn. A snapshot would show
/// the transcript as it was when the user walked in.
#[component]
fn Transcript(live: Memo<Option<Plan>>) -> impl IntoView {
    let plan_id = Memo::new(move |_| live.get().map(|p| p.id));

    // The chamber's one clock. It runs only while some deed is actually in
    // flight, and every running deed on the line reads this same signal -- see
    // `ticking_clock`.
    let anything_running = Memo::new(move |_| {
        live.get().is_some_and(|p| {
            p.transcript
                .iter()
                .any(|e| matches!(e, Entry::Tool(d) if d.in_flight()))
        })
    });
    let now = ticking_clock(anything_running);

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
            // Keyed by position *and* by what the entry is, so a tool call
            // settling in place re-renders. Keying on the index alone would
            // leave a running command showing "working..." forever -- the list
            // is the same length when a result arrives as it was when the call
            // was recorded.
            key=|(i, entry)| (*i, entry_version(entry))
            let:entry
        >
            {
                let (_, line) = entry;
                match line {
                    Entry::Message(u) => {
                        let is_user = u.speaker == Speaker::User;
                        view! {
                            <div class="chat-msg" class:is_user=is_user>
                                <span class="msg-at">{clock(u.at)}</span>
                                <span class="msg-who">
                                    {if is_user { "You" } else { "Court" }}
                                </span>
                                // The court's prose is markdown and is rendered
                                // as such. The King's is not: he typed it, and
                                // re-rendering his `#` as a heading would be a
                                // small lie about what he said.
                                {if is_user {
                                    view! {
                                        <span class="msg-body">{u.body.clone()}</span>
                                    }
                                    .into_any()
                                } else {
                                    view! {
                                        <Prose text=u.body.clone() class="msg-body"/>
                                    }
                                    .into_any()
                                }}
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
                    // reason -- but a tool call is the model *working*, which
                    // is the thing the user most wants to watch, so it gets its
                    // own shape rather than a note's.
                    //
                    // A question still waiting on him is the exception: that is
                    // not something to watch, it is something to do, so it is
                    // rendered as the thing to do.
                    Entry::Tool(d) if is_open_question(&d) => {
                        view! { <Question tool_call=d plan=plan_id/> }.into_any()
                    }
                    // Subagents are not a line of output either: the call's own
                    // text result is a summary, and the thing the user wants is
                    // the list of agents it sent and a way into each one.
                    Entry::Tool(d) if d.tool == "spawn_agents" => {
                        view! { <Subagents tool_call=d plan=plan_id/> }.into_any()
                    }
                    Entry::Tool(d) => {
                        view! { <ToolCallLine tool_call=d plan=plan_id now=now/> }.into_any()
                    }
                }
            }
        </For>
    }
}

/// The subagents one call sent, each a way into its own conversation.
///
/// Rendered from the *plans*, not from the tool call's text output. The output
/// is a summary written for the model; the user wants to know who is out there,
/// what each was asked, and how each is getting on -- and to be able to go and
/// read one. None of that survives being flattened into a paragraph.
///
/// The rows are live for free: `events::publish` announces a subagent on its
/// parent's channel as well as its own, and `Kingdom::insert` files a plan the
/// conversation has not seen before. So these come from the same signal
/// everything else reads, with no separate subscription.
#[component]
fn Subagents(tool_call: kingdom_core::ToolCall, plan: Memo<Option<PlanId>>) -> impl IntoView {
    let state = expect_context::<KingdomState>();
    let tool_call_id = StoredValue::new(tool_call.id.clone());
    let running = tool_call.in_flight();

    let subagents = Memo::new(move |_| {
        let Some(parent) = plan.get() else {
            return Vec::new();
        };
        state
            .kingdom
            .get()
            .subagents_of(&parent, &tool_call_id.get_value())
            .cloned()
            .collect::<Vec<_>>()
    });

    // The tasks as the model asked for them. Shown only until the subagents
    // themselves exist: between the call being recorded and the plans being
    // created there is a moment with nothing to list, and an empty box there
    // would read as a call that sent nobody.
    let asked = StoredValue::new(
        tool_call.input
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
                        let sent = subagents.get().len().max(asked.get_value().len());
                        match sent {
                            1 => "The court sent an errand".to_string(),
                            n => format!("The court sent {n} errands"),
                        }
                    }}
                </span>
            </div>

            <ul class="errand-list">
                {move || {
                    let sent = subagents.get();
                    if sent.is_empty() {
                        // Not yet created, or a record from before this
                        // conversation knew them: show what was asked, without
                        // a link that would lead nowhere.
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
                        .map(|subagent| {
                            let href = format!("/plan/{}", subagent.id);
                            let status = subagent.status;
                            let working = subagent.working_on.clone();
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
                                        <span class="errand-task">{subagent.prompt.clone()}</span>
                                        <span class="errand-state">
                                            {match (status, working) {
                                                // What it is doing beats what
                                                // it is: "Drafting" is true of
                                                // every working subagent and
                                                // tells the user nothing.
                                                (_, Some(doing)) => doing,
                                                (s, None) => subagent_status(s).to_string(),
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

/// How a subagent's state reads, which is not how a plan's does.
///
/// A subagent is never reviewed and never merged -- it reports to the model
/// that sent it. Relabelled here rather than given its own `PlanStatus`
/// variant: a sixth state would ripple through the map legend, `ALL` and every
/// match on plan state to buy one word.
fn subagent_status(status: PlanStatus) -> &'static str {
    match status {
        PlanStatus::Drafting => "working\u{2026}",
        PlanStatus::AwaitingReview => "reported",
        PlanStatus::Failed => "could not finish",
        PlanStatus::Merged | PlanStatus::Archived => status.label(),
    }
}

/// A plan the model has put to the user, and the two things they can do with it.
///
/// Follows the `.chat-question` idiom deliberately: that card already means
/// "this is not something to watch, it is something to do", and a proposal is
/// the same kind of thing at a larger scale. What differs is the stakes, so the
/// accepting button is the loud one and the setting-aside is quiet.
///
/// The body is **rendered markdown** -- headings, lists, tables, code fences and
/// mermaid diagrams -- through [`crate::components::Prose`]. A proposal is the
/// one artefact the King is asked to read and judge in full, so it is the place
/// where structure earns its keep most; the renderer was built for it.
#[component]
fn ProposalCard(
    proposal: Proposal,
    /// True while a turn is in flight. The buttons go dead rather than
    /// disappearing, so the card does not jump under the user's cursor.
    busy: Memo<bool>,
    on_accept: Callback<()>,
    on_set_aside: Callback<()>,
) -> impl IntoView {
    // Locked the instant they decide, so a double-click cannot grant twice or
    // race a set-aside against an acceptance. The same guard `Question` uses,
    // and for the same reason -- except that here the thing being handed over
    // is the ability to change their files.
    let (decided, set_decided) = signal(false);
    let deciding = move || decided.get() || busy.get();

    let accept = move |_| {
        if deciding() {
            return;
        }
        set_decided.set(true);
        on_accept.run(());
    };
    let set_aside = move |_| {
        if deciding() {
            return;
        }
        set_decided.set(true);
        on_set_aside.run(());
    };

    view! {
        <div class="chat-proposal" class:decided=move || decided.get()>
            <div class="proposal-head">
                <span class="proposal-mark">"\u{1F4DC}"</span>
                <span class="proposal-who">"The court proposes"</span>
                <span class="proposal-at">{clock(proposal.at)}</span>
            </div>

            <p class="proposal-title">{proposal.title}</p>
            <Prose text=proposal.body class="proposal-body"/>

            <div class="proposal-actions">
                <button
                    class="proposal-accept"
                    title="Let the court carry out this plan"
                    disabled=deciding
                    on:click=accept
                >
                    {move || if decided.get() { "Starting\u{2026}" } else { "Start with this" }}
                </button>
                <button
                    class="proposal-aside"
                    title="Put this plan aside and say what you want instead"
                    disabled=deciding
                    on:click=set_aside
                >
                    "Set aside"
                </button>
                <span class="proposal-hint">
                    "Or say what you would change below."
                </span>
            </div>
        </div>
    }
}

/// A question the model has stopped to ask, rendered where it was asked.
///
/// Inline in the transcript rather than as a modal. A modal would be the
/// obvious choice and is the wrong one: it puts the question in front of the
/// user with the work that prompted it hidden behind it, so he answers without
/// the context he needs. Here the reasoning and the commands that led to the
/// question are right above it, and he can scroll.
///
/// See [`is_open_question`] for why only an unanswered one gets this treatment.
#[component]
fn Question(tool_call: kingdom_core::ToolCall, plan: Memo<Option<PlanId>>) -> impl IntoView {
    let state = expect_context::<KingdomState>();
    let tool_call_id = StoredValue::new(tool_call.id.clone());
    let questions = StoredValue::new(parse_questions(&tool_call.input));
    let (sent, set_sent) = signal(false);

    let reply = move |answer: String| {
        let Some(plan_id) = plan.get_untracked() else {
            return;
        };
        // Locked as soon as he answers, so a double-click cannot send twice --
        // the second would find nothing waiting and report a confusing failure
        // for an answer that in fact landed.
        set_sent.set(true);
        let tool_call = tool_call_id.get_value();
        leptos::task::spawn_local(async move {
            if let Err(e) = crate::api::answer_question(plan_id.to_string(), tool_call, answer).await {
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

            // The listed options are the model's guesses at what the user might
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

/// One thing the model wants decided.
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
/// a missing `description` or a malformed option must not cost the user the
/// whole question. A question that renders with one option missing is still
/// answerable; one that fails to render at all leaves the model parked with
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

/// True for a question the user has not answered yet.
///
/// Once answered it is ordinary history and renders as any other tool call,
/// which is what stops him answering the same question twice.
fn is_open_question(entry: &kingdom_core::ToolCall) -> bool {
    entry.tool == "ask_user_question" && entry.in_flight()
}

/// Distinguishes an entry from a later version of *itself*.
///
/// Only tool calls have a later version: a message and a note are written once
/// and never touched, but a tool call is recorded in flight and settled
/// afterwards. This is what tells the keyed list those are two different things
/// to render.
fn entry_version(entry: &Entry) -> u8 {
    match entry {
        Entry::Tool(d) if d.in_flight() => 1,
        Entry::Tool(_) => 2,
        _ => 0,
    }
}

/// One tool call, collapsed to a line the user can skim and expand when it
/// matters.
///
/// Collapsed by default because a transcript that renders every command's full
/// output inline is unreadable at exactly the moment it becomes interesting:
/// the user is watching for *what the model is doing*, and a thousand lines of
/// build log buries that. The summary line is the answer; the detail is one
/// click away for when it is not.
#[component]
fn ToolCallLine(
    tool_call: kingdom_core::ToolCall,
    /// The plan this deed belongs to, so a file it left behind can be fetched
    /// from that plan's workspace. See `artifact.rs`.
    plan: Memo<Option<PlanId>>,
    /// The chamber's clock, so a running deed can say how long it has been
    /// going. Ticks only while something is in flight; see `ticking_clock`.
    now: Memo<Option<Timestamp>>,
) -> impl IntoView {
    use kingdom_core::ToolOutcome;

    let (open, set_open) = signal(false);

    let running = tool_call.in_flight();
    let state = match &tool_call.outcome {
        Some(o) => o.css_suffix(),
        None => "running",
    };
    let mark = match state {
        "done" => "\u{2713}",
        "refused" => "\u{2715}",
        _ => "\u{25cf}",
    };
    let at = clock(tool_call.at);
    let tool = tool_call.tool.clone();

    // The arguments matter more than the tool's name -- "bash" tells the user
    // nothing, `cargo test` tells him everything -- so the most telling
    // argument is promoted onto the collapsed line.
    let gist = telling_argument(&tool_call.input);
    // `StoredValue` because these sit inside `Show` bodies, which must be `Fn`:
    // a closure that moves an owned String is `FnOnce` and can only render once.
    let input = StoredValue::new(pretty(&tool_call.input));
    let output = StoredValue::new(match &tool_call.outcome {
        Some(ToolOutcome::Done { output, .. }) => output.clone(),
        Some(ToolOutcome::Refused { reason }) => reason.clone(),
        None => String::new(),
    });
    let has_output = !output.read_value().is_empty();

    // What this call left behind that can be looked at. Read once here rather
    // than in the view, because the paths are fixed the moment the call settles
    // and only the plan id is reactive.
    let pictures = StoredValue::new(
        tool_call
            .artifacts()
            .iter()
            .filter(|a| a.is_image())
            .map(|a| a.path.clone())
            .collect::<Vec<_>>(),
    );
    let has_pictures = !pictures.read_value().is_empty();

    // Recomputed on each tick while this deed runs, and constant once it has
    // settled -- at which point it no longer reads the clock at all, so a
    // transcript full of finished deeds costs nothing per second.
    let timing = StoredValue::new(tool_call.clone());
    let timing = Memo::new(move |_| timing.with_value(|d| self::timing(d, now.get())));

    view! {
        <div class=format!("chat-deed deed-{state}")>
            <button class="deed-line" on:click=move |_| set_open.update(|o| *o = !*o)>
                <span class="deed-at">{at}</span>
                <span class="deed-mark">{mark}</span>
                <span class="deed-tool">{tool}</span>
                <span class="deed-gist">{gist}</span>
                // The figure replaces "working..." rather than joining it: a
                // ticking clock already says the deed is running, and says how
                // long it has been at it as well. The word comes back only when
                // there is no figure to show -- a plan mid-turn when the server
                // restarted has a running deed and no clock to read it against.
                <Show
                    when=move || timing.get().is_some()
                    fallback=move || view! {
                        <Show when=move || running>
                            <span class="deed-running">"working\u{2026}"</span>
                        </Show>
                    }
                >
                    <span
                        class="deed-took"
                        class:is-running=move || running
                        class:is-overrun=move || timing.get().is_some_and(|(_, over)| over)
                    >
                        {move || timing.get().map(|(text, _)| text)}
                    </span>
                </Show>
                <span class="deed-chevron">
                    {move || if open.get() { "\u{2303}" } else { "\u{2304}" }}
                </span>
            </button>

            // Outside the `Show` above, and that is the decision worth stating:
            // the chevron governs the *text*, which is usually a wall of output
            // worth hiding. A picture is the opposite -- the highest-signal
            // thing a transcript can hold, and one nobody would think to click
            // for. So it is simply shown.
            <Show when=move || has_pictures>
                {move || pictures.get_value().into_iter().map(|path| {
                    let src = plan
                        .get()
                        .map(|id| crate::artifact::url(&id, &path))
                        .unwrap_or_default();
                    view! { <Sight src=src/> }
                }).collect_view()}
            </Show>

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

/// A picture the court left behind, as the King sees it.
///
/// The image is fetched from the plan's workspace rather than carried on the
/// plan -- see `artifact.rs` for why -- which means the file can legitimately
/// be gone: merging or archiving a plan clears its worktree away. That is the
/// *expected* end state of any finished plan, so it is answered with a sentence
/// rather than with a browser's broken-image glyph, which would read as a fault
/// in Kingdom rather than as the ordinary passage of time.
#[component]
fn Sight(src: String) -> impl IntoView {
    let (gone, set_gone) = signal(false);
    let href = src.clone();

    view! {
        <Show
            when=move || !gone.get()
            fallback=move || view! {
                <div class="deed-sight sight-gone">
                    "The workshop this was taken in has been cleared away."
                </div>
            }
        >
            // A link, not a lightbox: full size in a new tab is the whole
            // feature, and the route already serves it.
            <a
                class="deed-sight"
                href=href.clone()
                target="_blank"
                rel="noreferrer"
                title="Open this at full size"
            >
                <img
                    class="sight-frame"
                    src=src.clone()
                    alt="What the court saw"
                    loading="lazy"
                    on:error=move |_| set_gone.set(true)
                />
            </a>
        </Show>
    }
}

/// The one argument worth showing on a collapsed tool call line.
///
/// Shared with the spyglass, which captions its picture with the same deed the
/// transcript is showing. Two different answers to "which argument matters"
/// would have the two disagree about the same call.
///
/// Every tool names its subject differently, and there is no shared field to
/// rely on. Rather than teach this component about each tool's schema -- which
/// would have to be updated every time a tool is added, and would be silently
/// wrong when it was not -- it prefers a short list of conventional names and
/// otherwise shows the first string it finds. Being approximate is fine here:
/// the exact arguments are one click away.
pub(super) fn telling_argument(input: &serde_json::Value) -> String {
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

/// How long a deed took, or has been going, written for a glance.
///
/// Four bands, because one format cannot serve a `read_file` and a
/// `cargo build` at once:
///
/// | Under | Reads as | Why |
/// |---|---|---|
/// | 1s | `743ms` | the range where tools differ by a factor of ten |
/// | 10s | `3.2s` | a decimal still means something while a person waits |
/// | 1m | `42s` | tenths would be noise at this length |
/// | -- | `4m 7s` | minutes, said in words |
///
/// The last band is `4m 7s` rather than `4:07` deliberately. A colon reads as a
/// clock time, and a long deed would render as `61:11` -- which is not a time
/// of day and not obviously sixty-one minutes either. Naming the unit costs one
/// character and cannot be misread. This follows Phoenix's tool strip, which
/// arrived at the same four bands.
fn span(ms: i64) -> String {
    let ms = ms.max(0);
    if ms < 1_000 {
        return format!("{ms}ms");
    }
    let seconds = ms / 1_000;
    if seconds < 10 {
        // One decimal, so `1.0s` and `9.4s` are distinguishable in the range a
        // person actually sits watching.
        //
        // Truncated with integer maths rather than rounded through a float:
        // `{:.1}` would render 9.99s as `10.0s`, which contradicts the band it
        // was chosen by -- the very next millisecond renders as `10s`. Anything
        // shown with a decimal here is genuinely under ten seconds.
        let tenths = ms / 100;
        return format!("{}.{}s", tenths / 10, tenths % 10);
    }
    if seconds < 60 {
        return format!("{seconds}s");
    }
    match (seconds / 60, seconds % 60) {
        (minutes, 0) => format!("{minutes}m"),
        (minutes, rest) => format!("{minutes}m {rest}s"),
    }
}

/// What the deed line says about time, and whether it is cause for concern.
///
/// Returns nothing at all when there is nothing honest to say -- a settled call
/// from a record written before deeds were timed, or one the server died
/// during. Silence is the right rendering of "not known": a `0ms` would be a
/// claim, and the same claim a genuinely instant call makes.
///
/// The budget is shown only while the call is still running, which is the only
/// time it answers a question. Once a deed is settled, what it *would* have
/// waited for is trivia; how long it actually took is the fact worth keeping.
///
/// **The budget shown is the effective one, defaults included.** Phoenix shows
/// a wait only where the model named one, deliberately keeping the tools'
/// defaults out of its frontend so there is no second copy to drift. The same
/// concern is answered differently here: `Tool::waits_for` asks the tool, so
/// the resolved figure is the tool's own and there is still only one copy. That
/// is worth the difference, because the King's question is "is this about to
/// time out" -- and a `browser_click` that will give up in thirty seconds is
/// about to do so whether or not the model typed the number.
fn timing(tool_call: &ToolCall, now: Option<Timestamp>) -> Option<(String, bool)> {
    if !tool_call.in_flight() {
        return tool_call.elapsed_ms().map(|ms| (span(ms), false));
    }

    let (Some(Timestamp(started)), Some(Timestamp(now))) = (tool_call.at, now) else {
        return None;
    };
    let elapsed = (now - started).max(0);

    let Some(budget) = tool_call.waits else {
        return Some((span(elapsed), false));
    };

    // Past its budget, and the budget was one that mattered. This is the line
    // the King should look at: a browser call that has outlived its own timeout
    // is wedged, while a shell command past `wait_seconds` is simply still
    // going -- which is why the type is asked rather than the number compared.
    let overrun =
        elapsed as u64 / 1_000 >= budget.seconds() && budget.overrunning_is_a_problem();

    Some((
        format!("{} / {}", span(elapsed), span(budget.seconds() as i64 * 1_000)),
        overrun,
    ))
}

/// A clock the chamber can read, ticking once a second while `while_busy`.
///
/// One timer for the whole conversation rather than one per deed. A busy turn's
/// transcript holds dozens of settled calls, and a timer each would have every
/// one of them waking to re-render a string that cannot change -- a cost that
/// grows with the log for no gain, since only the deeds in flight move.
///
/// It stops when nothing is in flight, so a chamber left open overnight is not
/// waking the browser once a second until morning. `while_busy` is a signal
/// rather than a value for exactly that: the turn ends without anybody
/// navigating away.
///
/// **Why the handle is held rather than left to `on_cleanup`.** This effect
/// re-runs every time a turn starts or ends, which on a working plan is often.
/// Relying on cleanup alone to cancel the previous interval is relying on that
/// cleanup being scoped to the effect *run* rather than to the component -- and
/// if it is the latter, every turn leaves another timer ticking until the user
/// navigates away. That failure is invisible: the clock still reads correctly,
/// it is merely being driven by five timers instead of one. Owning the handle
/// makes the cancellation ours, and true under either scoping.
fn ticking_clock(while_busy: Memo<bool>) -> Memo<Option<Timestamp>> {
    let (now, set_now) = signal(Timestamp::now());

    #[cfg(feature = "hydrate")]
    {
        let running: StoredValue<Option<leptos::leptos_dom::helpers::IntervalHandle>> =
            StoredValue::new(None);

        // Cancels whatever is ticking, if anything. Idempotent, so it is safe on
        // the path where there was never a timer to begin with.
        let stop = move || {
            if let Some(handle) = running.try_get_value().flatten() {
                handle.clear();
                running.try_set_value(None);
            }
        };

        Effect::new(move |_| {
            // Unconditionally first: this run supersedes the last, whether it is
            // about to start a new timer or to stop entirely.
            stop();

            if !while_busy.get() {
                return;
            }

            // Read straight away, so the first figure appears without waiting a
            // second for the first tick.
            set_now.set(browser_now());

            if let Ok(handle) = leptos::leptos_dom::helpers::set_interval_with_handle(
                move || set_now.set(browser_now()),
                std::time::Duration::from_secs(1),
            ) {
                running.try_set_value(Some(handle));
            }
        });

        // And the last one stops when the chamber itself goes away.
        on_cleanup(stop);
    }

    #[cfg(not(feature = "hydrate"))]
    let _ = (while_busy, set_now);

    Memo::new(move |_| now.get())
}

/// The wall clock, as the browser reads it.
#[cfg(feature = "hydrate")]
fn browser_now() -> Option<Timestamp> {
    Some(Timestamp(js_sys::Date::now() as i64))
}

/// A log entry's time as a bare `HH:MM` in the user's own timezone.
///
/// Browser-only, and that is not a limitation: the stamp is UTC milliseconds and
/// only the browser knows what the user's clock reads. Under SSR this is the
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

/// Watches one plan over the push socket, handing each proclamation to
/// `insert`.
///
/// Browser-only: under SSR there is no socket and the first render is served
/// from server state directly.
///
/// Reconnection is a plain fixed-delay retry with no backoff ladder and no
/// give-up: the server is on loopback, so a dropped socket means it is
/// restarting, and the honest response is to keep trying until it is back. The
/// reconnect costs nothing to get right because the socket's opening message is
/// the whole plan -- there is no cursor to resume from and nothing that can be
/// missed while it was down. See `events.rs`.
fn watch_plan(plan_id: Memo<Option<PlanId>>, insert: impl Fn(Plan) + Clone + 'static) {
    #[cfg(feature = "hydrate")]
    Effect::new(move |previous: Option<Option<PlanWatch>>| {
        // Close the previous socket before opening another, so moving between
        // conversation views cannot leave a socket behind feeding a plan nobody
        // is looking at.
        drop(previous);

        let id = plan_id.get()?;
        Some(PlanWatch::open(&id, insert.clone()))
    });

    #[cfg(not(feature = "hydrate"))]
    {
        let _ = (plan_id, insert);
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
    /// a conversation the user has already left.
    retry: std::rc::Rc<std::cell::Cell<Option<i32>>>,
}

#[cfg(feature = "hydrate")]
impl PlanWatch {
    /// How long to wait before reopening a dropped socket.
    const RETRY_MS: i32 = 1000;

    fn open(id: &PlanId, insert: impl Fn(Plan) + Clone + 'static) -> Self {
        use wasm_bindgen::closure::Closure;
        use wasm_bindgen::JsCast;

        let socket = web_sys::WebSocket::new(&Self::url(id))
            .expect("the chamber's watch socket should be constructible");

        let on_message = Closure::<dyn FnMut(web_sys::MessageEvent)>::new({
            let insert = insert.clone();
            move |event: web_sys::MessageEvent| {
                let Some(text) = event.data().as_string() else {
                    return;
                };
                // A message that will not parse means the server sent a shape
                // this bundle does not know -- a stale tab after a rebuild,
                // most likely. Dropping it leaves the conversation showing the
                // last good state, which is better than tearing it down.
                if let Ok(plan) = serde_json::from_str::<Plan>(&text) {
                    insert(plan);
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
                    let insert = insert.clone();
                    let retry = retry.clone();
                    move || {
                        retry.set(None);
                        // Reopening replaces this watch's socket in place. The
                        // effect that owns it is not re-run, because nothing it
                        // tracked changed -- the user is still in the same
                        // conversation.
                        //
                        // Deliberately leaked: the reopened watch outlives this
                        // callback and has no owner to hand it back to. Bounded
                        // by the number of disconnects in one conversation
                        // visit, and the socket it holds is closed by the
                        // browser when the page goes.
                        std::mem::forget(PlanWatch::open(&id, insert));
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

#[cfg(test)]
mod tests {
    use super::*;
    use kingdom_core::{ToolOutcome, WaitBudget};
    use serde_json::json;

    fn call(tool: &str, settled: bool) -> Entry {
        let mut c = ToolCall::started(tool, tool, json!({ "selector": ".btn" }));
        if settled {
            c.outcome = Some(ToolOutcome::done("ok"));
        }
        Entry::Tool(c)
    }

    /// The formatter has to serve a `read_file` and a `cargo build` on the same
    /// line, and the band edges are where a single format breaks.
    ///
    /// The last band is the one worth pinning: `4m 7s` rather than `4:07`,
    /// because a colon reads as a clock time and an hour-long deed would render
    /// as `61:11` -- neither a time of day nor obviously sixty-one minutes.
    #[test]
    fn a_span_reads_the_same_whether_it_is_instant_or_an_hour() {
        assert_eq!(span(0), "0ms");
        assert_eq!(span(400), "400ms");
        assert_eq!(span(999), "999ms");
        assert_eq!(span(1_000), "1.0s");
        assert_eq!(span(3_240), "3.2s");
        assert_eq!(span(9_990), "9.9s", "the decimal band never rounds up into the next one");
        assert_eq!(span(10_000), "10s");
        assert_eq!(span(59_999), "59s");
        assert_eq!(span(60_000), "1m");
        assert_eq!(span(247_000), "4m 7s");
        assert_eq!(span(3_671_000), "61m 11s");
        // Impossible, but a clock that stepped backwards must not produce a
        // deed that took less than no time.
        assert_eq!(span(-5), "0ms");
    }

    /// A question waiting on the King must never be drawn as a deed.
    ///
    /// `ask_user_question` reports a half-hour `Deadline`, and `timing` turns a
    /// passed deadline red. That is right for a wedged browser call and quite
    /// wrong here: the King reading a question would get a countdown pressuring
    /// him through the one decision this product exists to let him take slowly.
    ///
    /// What actually prevents it is `is_open_question` diverting the entry to
    /// `Question` before `ToolCallLine` ever sees it. That is one match arm's
    /// worth of protection, so it is pinned here rather than trusted -- and the
    /// second half of the test is the reason the first is not enough on its
    /// own: once *answered*, the same call is ordinary history, and history
    /// shows what it took rather than what it was waiting for.
    #[test]
    fn a_question_waiting_on_the_king_never_shows_him_a_countdown() {
        let mut asked = ToolCall::started("q", "ask_user_question", json!({}));
        asked.at = Some(Timestamp(0));
        asked.waits = Some(WaitBudget::Deadline { seconds: 30 * 60 });

        assert!(
            is_open_question(&asked),
            "an unanswered question is diverted to `Question`, so it is never a deed line"
        );

        // Answered an hour later -- twice its own budget. Were this ever drawn
        // as a deed, it must read as history and not as an alarm.
        asked.outcome = Some(ToolOutcome::done("The careful way"));
        asked.settled_at = Some(Timestamp(3_600_000));

        assert!(!is_open_question(&asked));
        assert_eq!(
            timing(&asked, Some(Timestamp(3_600_000))),
            Some(("60m".to_string(), false)),
            "a settled call shows what it took, with no budget and no alarm"
        );
    }

    /// What the line says about time, in the four states it can be in.
    ///
    /// The one that matters most is the silence. A deed with no end recorded --
    /// an old record, or a call the server died during -- must show nothing at
    /// all: `0.0s` there is not a smaller mistake than a wrong number, it *is*
    /// a wrong number, and indistinguishable from a genuinely instant call.
    #[test]
    fn the_line_says_nothing_about_a_deed_nobody_timed() {
        let now = Some(Timestamp(60_000));

        let mut settled = ToolCall::started("c", "read_file", json!({}));
        settled.at = Some(Timestamp(10_000));
        settled.outcome = Some(ToolOutcome::done("ok"));
        settled.settled_at = Some(Timestamp(10_400));
        assert_eq!(
            timing(&settled, now),
            Some(("400ms".to_string(), false)),
            "a settled deed reports what it took, and a budget it no longer has \
             any use for is not shown"
        );

        let mut unknown = settled.clone();
        unknown.settled_at = None;
        assert_eq!(
            timing(&unknown, now),
            None,
            "a settled deed with no end recorded says nothing rather than zero"
        );

        let mut running = ToolCall::started("c", "bash", json!({}));
        running.at = Some(Timestamp(48_000));
        assert_eq!(
            timing(&running, now),
            Some(("12s".to_string(), false)),
            "a running deed with no budget simply counts up"
        );

        assert_eq!(
            timing(&running, None),
            None,
            "and with no clock to read -- under SSR -- it says nothing"
        );
    }

    /// Overrunning is drawn in alarm for a deadline and left alone for
    /// patience, which is the whole reason the budget is a type.
    ///
    /// Both deeds below are past their number by the same margin. Flagging the
    /// shell one would put a red figure on every cold `cargo build` -- the most
    /// common long deed there is -- and the King would learn within a day that
    /// the colour means nothing.
    #[test]
    fn only_a_deed_past_a_real_deadline_is_drawn_as_trouble() {
        let mut shell = ToolCall::started("c", "bash", json!({}));
        shell.at = Some(Timestamp(0));
        shell.waits = Some(WaitBudget::Patience { seconds: 30 });

        let mut browser = ToolCall::started("c", "browser_click", json!({}));
        browser.at = Some(Timestamp(0));
        browser.waits = Some(WaitBudget::Deadline { seconds: 30 });

        let within = Some(Timestamp(29_000));
        assert_eq!(timing(&shell, within), Some(("29s / 30s".to_string(), false)));
        assert_eq!(timing(&browser, within), Some(("29s / 30s".to_string(), false)));

        let past = Some(Timestamp(45_000));
        assert_eq!(
            timing(&shell, past),
            Some(("45s / 30s".to_string(), false)),
            "a command outliving its wait is the design working: the work goes on"
        );
        assert_eq!(
            timing(&browser, past),
            Some(("45s / 30s".to_string(), true)),
            "a browser call outliving its timeout is wedged, and worth looking at"
        );
    }

    /// The caption must name what the court is doing *now* when there is such a
    /// thing, and what it last did when there is not. Getting this backwards
    /// leaves the panel confidently captioning a live page with a stale call --
    /// wrong without looking wrong, which is why it is the one thing tested.
    #[test]
    fn the_caption_prefers_the_call_still_running() {
        let in_flight = browsing(&[
            call("browser_navigate", true),
            call("browser_click", false),
            call("bash", false),
        ]);
        assert_eq!(in_flight.unwrap().tool, "browser_click");

        let settled = browsing(&[call("browser_navigate", true), call("browser_click", true)]);
        assert_eq!(settled.unwrap().tool, "browser_click");

        // A plan that has never browsed has nothing to caption, and a running
        // non-browser deed is not evidence that it has.
        assert!(browsing(&[call("bash", false)]).is_none());
    }

    /// A deed that left a picture behind offers one to render, and an ordinary
    /// deed offers none.
    ///
    /// The view itself needs a browser to assert against, so what is pinned is
    /// the decision the view reads: *which* deeds have something to show, and
    /// the URL it would point at. That URL is the part worth pinning -- it has
    /// to match `artifact::ROUTE`, and the failure if it does not is a broken
    /// image rather than anything that fails to compile.
    #[test]
    fn only_a_deed_that_left_a_picture_has_one_to_show() {
        let mut took = ToolCall::started("call-1", "browser_take_screenshot", json!({}));
        took.outcome = Some(ToolOutcome::produced(
            "Screenshot saved to shot.png.",
            vec![kingdom_core::ToolArtifact {
                path: "shot.png".into(),
                media_type: "image/png".into(),
            }],
        ));

        let pictures: Vec<_> = took
            .artifacts()
            .iter()
            .filter(|a| a.is_image())
            .map(|a| crate::artifact::url(&PlanId::new("plan-1"), &a.path))
            .collect();

        assert_eq!(pictures, vec!["/plan/plan-1/artifact/shot.png"]);

        let Entry::Tool(ran) = call("bash", true) else {
            panic!("a deed");
        };
        assert!(
            ran.artifacts().is_empty(),
            "an ordinary command must not sprout a frame in the chamber"
        );
    }
}
