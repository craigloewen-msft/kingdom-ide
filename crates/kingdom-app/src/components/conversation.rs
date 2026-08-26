//! The plan's conversation: one plan's whole life, at its own URL.
//!
//! A `Plan` is both the unit of work and the unit of review, so it gets a place
//! to be reviewed in. Everything here reads from server state rather than local
//! signals, which is what lets a reload -- or a link shared between tabs --
//! rebuild the conversation exactly.

use crate::api::{
    annotate_file, annotate_proposal, approve_plan, draft_plan, finish_plan, get_kingdom,
    plan_briefing, say, send_file_notes, send_notes, set_aside_plan, stop_plan, unqueue,
    withdraw_file_note, withdraw_note,
};
use crate::app::KingdomState;
use crate::components::prompt_bar::autogrow;
use crate::components::resizer::{restore_width, Bounds, Grows, Resizer};
use crate::components::BrowserView;
use crate::components::CityRail;
use crate::components::DiffView;
use crate::components::Prose;
use crate::components::ReviewMargin;
use crate::components::SourceView;
// `FileTree` is not named here: the files rail is `CityRail`, which stacks the
// tree over the review drawer and mounts both itself.
use crate::components::ProposalCard;
use kingdom_core::{
    Disposition, Entry, Permissions, Plan, PlanId, PlanStatus, Speaker, Timestamp, ToolCall,
};
use leptos::prelude::*;
use leptos_router::hooks::use_params_map;

/// What is showing in the panel beside the transcript.
///
/// **One focused panel at a time**, and this type is why. The spyglass, the
/// diff and the source view all want the full-height column right of the
/// conversation, and all three are things the King reads *against* the
/// transcript rather than instead of it. Two of them side by side would leave
/// three columns fighting over a screen the transcript is supposed to be the
/// middle of, so opening any one closes the others -- not by three booleans
/// watching each other, which is how they end up all true, but because there is
/// only one signal and it holds one value.
///
/// The transcript is deliberately *outside* this decision. It is not one of the
/// alternatives; it is the thing they are alternatives beside.
#[derive(Clone, PartialEq, Eq)]
enum Aside {
    Hidden,
    Browser,
    /// A file under review, named relative to the plan's workspace: what this
    /// plan changed about it.
    Diff(String),
    /// A file being read whole, named the same way. Opened from the files tree,
    /// where most files have no diff to show.
    Source(String),
}

impl Aside {
    fn is_browser(&self) -> bool {
        matches!(self, Aside::Browser)
    }

    /// The file being compared, if a diff is what is showing.
    fn diff_file(&self) -> Option<String> {
        match self {
            Aside::Diff(path) => Some(path.clone()),
            _ => None,
        }
    }

    /// The file being read whole, if that is what is showing.
    fn source_file(&self) -> Option<String> {
        match self {
            Aside::Source(path) => Some(path.clone()),
            _ => None,
        }
    }

    /// The file being shown either way.
    ///
    /// What the rail highlights against: the tree and the drawer may hold the
    /// same path, and the King should see the row he pressed marked whichever
    /// pane he pressed it in.
    fn file(&self) -> Option<String> {
        match self {
            Aside::Diff(path) | Aside::Source(path) => Some(path.clone()),
            _ => None,
        }
    }
}

/// How wide the spyglass may be dragged. Narrower than the minimum and the
/// court's 1024-wide page is scaled past reading; wider and the transcript
/// stops being the thing under review.
const SPYGLASS_BOUNDS: Bounds = Bounds {
    min: 320.0,
    max: 900.0,
    default: 480.0,
};

const SPYGLASS_WIDTH_KEY: &str = "kingdom.spyglass_width";

/// How wide the diff may be dragged.
///
/// Its own bounds and its own key, deliberately not shared with the spyglass's.
/// Two columns of code need more room than a screencast of a 1024-wide page,
/// and the width the King drags for one should still be there when he comes
/// back to it rather than having been overwritten by the other.
const DIFF_BOUNDS: Bounds = Bounds {
    min: 380.0,
    max: 1200.0,
    default: 640.0,
};

const DIFF_WIDTH_KEY: &str = "kingdom.diff_width";

/// How wide the source view may be dragged.
///
/// Its own bounds and its own key, deliberately not shared with the diff's --
/// the precedent the spyglass and the diff set, and for its reason. One column
/// of code needs less room than two side by side, so the default is narrower;
/// the width the King drags for one should still be there when he comes back to
/// it rather than having been overwritten by another panel.
const SOURCE_BOUNDS: Bounds = Bounds {
    min: 320.0,
    max: 1200.0,
    default: 520.0,
};

const SOURCE_WIDTH_KEY: &str = "kingdom.source_width";

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
        // `with` rather than `get`: `get` on this signal *clones the entire
        // kingdom* -- every plan, every transcript -- and then throws all but
        // one away. On a real kingdom that is 14 MB copied to read one plan,
        // and it happens on every watch-socket push. `with` borrows instead,
        // so only the plan that is wanted is cloned.
        state.kingdom.with(|k| k.plan(&id).cloned())
    });

    // Which plan is open, by identity rather than by value.
    //
    // This is what the chamber's body is built from, and the distinction is
    // load-bearing. Branching on `plan` itself rebuilds `ConversationBody` on
    // every watch-socket push, because a growing transcript makes the memo's
    // value differ every round. Leptos reuses the DOM nodes, so it never looked
    // like a remount -- but each rebuild constructs the component afresh, and
    // every signal declared in its body is born empty again: the files rail's
    // cache of listings, the folders the King had opened, whether the spyglass
    // is watching, and the half-written decree in the composer. A turn running
    // anywhere -- including one the King is not watching -- collapsed his tree
    // and wiped his textarea twice per exchange.
    //
    // `PlanId` is `PartialEq`, so this fires only when the conversation
    // genuinely becomes a different one: navigating between plans, or a plan
    // leaving the kingdom. Those *should* rebuild, because the snapshot the
    // body takes describes that plan and not this one. Everything that moves
    // during a turn is read through `live` by the body itself.
    let open_plan = Memo::new(move |_| plan.get().map(|p| p.id));

    let city_name = Memo::new(move |_| {
        let plan = plan.get()?;
        // `with`, for the reason given on `plan` above: this reads one `String`
        // and would otherwise clone the kingdom to get it.
        state
            .kingdom
            .with(|k| k.city(&plan.city).map(|c| c.name.clone()))
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
        let unstarted = p.status == PlanStatus::Drafting
            && !p.is_busy()
            && !p.is_subagent()
            && !model_has_moved;
        if unstarted && !draft.pending().get_untracked() {
            draft.dispatch(p.id.clone());
        }
    });

    // Whatever the server says about this plan, as it says it. A draft started
    // by another page -- a reload mid-flight, a second tab -- lands here just
    // the same, because the conversation is watching the plan rather than
    // awaiting its own request.
    //
    // The badge cache is written from here as well as from the rail's own
    // socket. Both sockets carry the same fact and neither is authoritative
    // over the other: this one has the whole plan and can compute it, the
    // rail's is told it. Writing from both is what stops the rail lagging
    // behind the chamber the King is looking at -- and they cannot disagree,
    // because `wants_attention` is the single definition on both ends.
    watch_plan(plan_id, move |updated| {
        state.note_attention(&updated.id, updated.wants_attention());
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
            {move || match open_plan.get() {
                Some(_) => {
                    // Read untracked: this closure must depend on the plan's
                    // identity alone. A tracked `get()` here would subscribe it
                    // to every field of the plan again, reinstating the
                    // rebuild-per-push that `open_plan` exists to prevent.
                    let Some(snapshot) = plan.get_untracked() else {
                        // `open_plan` was `Some`, so the plan was there when
                        // the memo last ran. Unreachable in practice, and
                        // rendering nothing beats an `expect` that would take
                        // the chamber down over a race.
                        return ().into_any();
                    };
                    view! {
                        <ConversationBody
                            plan=snapshot
                            live=plan
                            city=city_name
                            drafting=drafting
                            on_say=speak
                            on_draft=redraft
                            finish=finish
                        />
                    }
                    .into_any()
                }
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

    // `with` throughout rather than `get`. Every one of the memos below re-runs
    // on each watch-socket push, and `live.get()` deep-copies the *whole plan*
    // -- its entire transcript -- to read a single field from it. Reading
    // `status` that way cost a 2.3 MB clone per push on a real plan, and there
    // are a dozen such memos. `with` borrows the plan and clones only what is
    // actually kept.
    let status = Memo::new(move |_| {
        live.with(|p| p.as_ref().map(|p| p.status))
            .unwrap_or(PlanStatus::Drafting)
    });
    let settled = Memo::new(move |_| status.get().is_settled());
    let title = Memo::new(move |_| {
        live.with(|p| p.as_ref().map(|p| p.title.clone()))
            .unwrap_or_default()
    });

    // Whether a turn is actually in flight, as against merely `Drafting` --
    // which is also true of a plan nobody has started yet. This is what gates
    // Stop, so that the button appears only when there is something to stop.
    let busy = Memo::new(move |_| live.with(|p| p.as_ref().is_some_and(|p| p.is_busy())));

    // What the King has said that the court has not heard yet. Read live: it
    // grows as he types into a working chamber, and empties the moment the turn
    // reaches a round boundary.
    let queued = Memo::new(move |_| {
        live.with(|p| p.as_ref().map(|p| p.queued.clone()))
            .unwrap_or_default()
    });

    // Calling a halt. Its own action so the button can read its pending state,
    // and so a second click while the first is in flight does nothing.
    let stop = Action::new(move |id: &PlanId| {
        let id = id.to_string();
        async move {
            if let Err(e) = stop_plan(id).await {
                state.error.set(Some(e.to_string()));
            }
        }
    });

    // Taking words back before the court hears them. Losing the race is
    // reported rather than swallowed -- see `api::unqueue`.
    let withdraw = Callback::new(move |(plan, queued_id): (PlanId, String)| {
        leptos::task::spawn_local(async move {
            match unqueue(plan.to_string(), queued_id).await {
                Ok(_) => state.error.set(None),
                Err(e) => state.error.set(Some(e.to_string())),
            }
        });
    });

    // The plan the model has put to the user, if it is their move. Read live,
    // because it arrives mid-conversation over the watch socket -- and read
    // through `standing_proposal` rather than by testing the fields here, so
    // the browser and the server agree on what "awaiting their word" means.
    let proposal =
        Memo::new(move |_| live.with(|p| p.as_ref().and_then(|p| p.standing_proposal().cloned())));
    // What this plan wants of the King, read off the plan itself rather than
    // from `state.attention`. The chamber holds the whole plan, so it has the
    // better source; the cache exists for the rail, which does not.
    let wants = Memo::new(move |_| live.with(|p| p.as_ref().and_then(|p| p.wants_attention())));
    // What the model may touch right now. Widens exactly once, on approval.
    let permissions = Memo::new(move |_| {
        live.with(|p| p.as_ref().map(|p| p.permissions))
            .unwrap_or(Permissions::Full)
    });

    // How full the model's window is, read live because it moves every round --
    // including mid tool loop, which is when it is worth watching. `None` until
    // the court has answered once, and always `None` for a provider that
    // reports no usage or declares no window; the header simply says nothing
    // rather than drawing a bar over a number nobody measured.
    let context = Memo::new(move |_| {
        let usage = live.with(|p| p.as_ref()?.context)?;
        let percent = usage.percent()?;
        Some((
            percent,
            kingdom_core::window_label(usage.window),
            usage.tokens,
            usage.weight(),
        ))
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
        state.kingdom.with(|k| {
            let parent = k.plan(&sent_by.parent)?;
            Some((parent.id.clone(), parent.title.clone()))
        })
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
        live.with(|p| {
            p.as_ref()
                .and_then(|p| p.outcome.as_ref())
                .map(|o| o.summary())
        })
        .unwrap_or_else(|| "This plan is closed.".to_string())
    });

    // Keeps the newest line in view. A conversation longer than the viewport
    // otherwise leaves the reply the user is waiting for below the fold.
    let log_ref = NodeRef::<leptos::html::Div>::new();
    stick_to_bottom(
        log_ref,
        Signal::derive(move || {
            (
                live.with(|p| p.as_ref().map(|p| p.transcript.len()).unwrap_or(0)),
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
    //
    // No longer refuses while the court is working. Sending mid-turn queues the
    // words instead of dropping them -- the server decides which, on the one
    // question the browser cannot answer: whether a turn is genuinely running.
    // See `api::say`.
    let submit = move || {
        let text = reply.get().trim().to_string();
        if text.is_empty() {
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

    // What the King has written in the margin and not yet sent. Read live for
    // the same reason the proposal itself is: a note written in another tab
    // arrives over the watch socket, and the margin must empty here the moment
    // the notes are sent from anywhere.
    let notes = Memo::new(move |_| live.get().map(|p| p.notes().to_vec()).unwrap_or_default());

    // Writing one. The whole card is presentational -- every call in this view
    // is owned here, as it was before annotation existed -- so the components
    // hand the note up and this decides what happens to it.
    let annotate = Callback::new(move |(line, quote, note): (usize, String, String)| {
        let plan_id = id.get_value();
        leptos::task::spawn_local(async move {
            match annotate_proposal(plan_id.to_string(), line, quote, note).await {
                Ok(updated) => {
                    state.error.set(None);
                    state.kingdom.update(|k| k.insert(updated));
                }
                Err(e) => state.error.set(Some(e.to_string())),
            }
        });
    });

    // Taking one back. Losing the race is reported rather than swallowed, for
    // the same reason withdrawing queued words is -- see `api::withdraw_note`.
    let withdraw_a_note = Callback::new(move |note_id: String| {
        let plan_id = id.get_value();
        leptos::task::spawn_local(async move {
            match withdraw_note(plan_id.to_string(), note_id).await {
                Ok(updated) => {
                    state.error.set(None);
                    state.kingdom.update(|k| k.insert(updated));
                }
                Err(e) => state.error.set(Some(e.to_string())),
            }
        });
    });

    // Putting them to the court. An `Action` so the button can read its own
    // pending state and a second click while the first is in flight does
    // nothing -- a double send would drain the margin once and then report a
    // confusing failure for notes that in fact landed.
    //
    // Sending and drafting are split for exactly the reason accepting and
    // drafting are: the King's words land in the chamber immediately rather
    // than behind the first round of the court's reply.
    let send = Action::new(move |_: &()| {
        let plan_id = id.get_value();
        async move {
            match send_notes(plan_id.to_string()).await {
                Ok(_) => {
                    state.error.set(None);
                    if let Ok(k) = get_kingdom().await {
                        state.kingdom.set(k);
                    }
                    on_draft.run(plan_id);
                }
                Err(e) => state.error.set(Some(e.to_string())),
            }
        }
    });
    let send_the_notes = Callback::new(move |_: ()| {
        send.dispatch(());
    });
    let sending = Signal::derive(move || send.pending().get());

    // What is showing beside the transcript. One signal holding one value, so
    // the browser and the diff cannot both be open -- see [`Aside`].
    //
    // Hidden by default. For the browser that is load-bearing rather than
    // tidy: opening it is what attaches a viewer and starts the screencast, and
    // closing it is what stops one, so this is the King's direct control over
    // whether Chrome is painting for an audience.
    let aside = RwSignal::new(Aside::Hidden);

    // The system prompt this plan's model is given, once it has been asked for.
    // `None` means it has never been fetched: the panel opening is what fetches
    // it, so a user who never asks pays nothing for the guidance walk that
    // assembling one costs on the server.
    let (briefing, set_briefing) = signal(None::<Result<String, String>>);
    let (reading_orders, set_reading_orders) = signal(false);

    let fetch_briefing = move || {
        set_briefing.set(None);
        let plan_id = id.get_value();
        leptos::task::spawn_local(async move {
            let fetched = plan_briefing(plan_id.to_string())
                .await
                .map_err(|e| e.to_string());
            set_briefing.set(Some(fetched));
        });
    };

    // Escape closes it, as it closes every overlay. Registered once and gated
    // inside rather than attached while the panel is open: a listener whose
    // lifetime is tied to a `Show` has to be torn down from a branch that is no
    // longer rendering, which is how a stray handler outlives its view.
    let escape = window_event_listener(leptos::ev::keydown, move |ev| {
        if ev.key() == "Escape" && reading_orders.get_untracked() {
            set_reading_orders.set(false);
        }
    });
    on_cleanup(move || escape.remove());

    // How wide each panel is, in pixels. Plain local signals rather than
    // something on `KingdomState`: like the rail's collapse set, they are view
    // preferences and nothing outside this view reads them. One each, because
    // a screencast and a two-column diff do not want the same room.
    let spyglass_width = RwSignal::new(SPYGLASS_BOUNDS.default);
    restore_width(spyglass_width, SPYGLASS_WIDTH_KEY, SPYGLASS_BOUNDS);
    let diff_width = RwSignal::new(DIFF_BOUNDS.default);
    restore_width(diff_width, DIFF_WIDTH_KEY, DIFF_BOUNDS);
    let source_width = RwSignal::new(SOURCE_BOUNDS.default);
    restore_width(source_width, SOURCE_WIDTH_KEY, SOURCE_BOUNDS);

    // The file the rail should mark as open, whichever way it is showing.
    let open_file = Memo::new(move |_| aside.get().file());
    // And the two the panels fetch by. Split, because a diff and a whole file
    // are different requests and the wrong panel must not fire one.
    let diff_file = Memo::new(move |_| aside.get().diff_file());
    let source_file = Memo::new(move |_| aside.get().source_file());

    // A change signal for the review drawer, free from the watch socket: every
    // transcript entry is a moment the court may have touched a file.
    let activity =
        Memo::new(move |_| live.with(|p| p.as_ref().map(|p| p.transcript.len()).unwrap_or(0)));

    // Opening a file is the one thing that can *replace* what is in the panel
    // rather than merely closing it, so the rail hands the path here and this
    // is where the browser gives way. Two callbacks, because the two panes of
    // the rail mean different things by "show me this": the drawer asks what
    // changed, and the tree asks what is there.
    let review_file = Callback::new(move |path: String| aside.set(Aside::Diff(path)));
    let read_file = Callback::new(move |path: String| aside.set(Aside::Source(path)));

    // Opening a file from the review margin. Deliberately the *source* view:
    // the margin lists notes from both panels, and a note written on a file
    // with no diff could not be reopened at all if this asked for one.
    let open_noted_file = read_file;

    // What the plan has changed. Held here rather than in the rail because two
    // views read it: the drawer's rows and its badge, and the diff panel, which
    // uses a file's line counts as a version stamp to know when to refetch.
    let summary = RwSignal::new(None::<kingdom_core::ChangeSummary>);
    let looking = RwSignal::new(false);

    // The open file's counts, as a stamp. When the court edits the file the
    // King is reading, these move and the panel fetches again -- at no cost,
    // because the rail has already asked the question.
    let diff_version = Memo::new(move |_| {
        let Some(path) = diff_file.get() else {
            return (0, 0);
        };
        summary
            .get()
            .and_then(|s| s.files.into_iter().find(|f| f.path == path))
            .map(|f| (f.added, f.removed))
            .unwrap_or((0, 0))
    });

    // The source view's own stamp. The summary cannot serve here: a file the
    // plan has not changed is absent from it, so its counts never move however
    // much the court edits the file. The transcript's length does move, and it
    // is the same free signal the review drawer already refetches on.
    let source_version = activity;

    // What the King has written against lines of code, read live off the plan
    // so a note written in another tab appears here, and so the margin empties
    // the moment the review is sent.
    let review_notes = Memo::new(move |_| {
        live.get()
            .map(|p| p.review_notes.clone())
            .unwrap_or_default()
    });
    // Narrowed to the file each panel is showing, so a line can say whether it
    // already carries a note.
    let notes_on_diff = Memo::new(move |_| {
        let Some(path) = diff_file.get() else {
            return Vec::new();
        };
        review_notes
            .get()
            .into_iter()
            .filter(|n| n.path == path)
            .collect()
    });
    let notes_on_source = Memo::new(move |_| {
        let Some(path) = source_file.get() else {
            return Vec::new();
        };
        review_notes
            .get()
            .into_iter()
            .filter(|n| n.path == path)
            .collect()
    });

    // Writing one. The panels are presentational -- every call in this view is
    // owned here, as the proposal card's already were -- so they hand the note
    // up and this decides what happens to it.
    let annotate_line = Callback::new(
        move |(line, side, quote, note): (u32, kingdom_core::NoteSide, String, String)| {
            // The path is read here rather than passed up: the panel knows the
            // line, and the chamber knows which file is open. Asking the panel
            // to carry a path it was handed would be a second copy to keep in
            // step with `Aside`.
            let Some(path) = open_file.get_untracked() else {
                return;
            };
            let plan_id = id.get_value();
            leptos::task::spawn_local(async move {
                match annotate_file(plan_id.to_string(), path, line, side, quote, note).await {
                    Ok(updated) => {
                        state.error.set(None);
                        state.kingdom.update(|k| k.insert(updated));
                    }
                    Err(e) => state.error.set(Some(e.to_string())),
                }
            });
        },
    );

    // Taking one back. Losing the race is reported rather than swallowed, for
    // the reason `api::withdraw_file_note` gives.
    let withdraw_line_note = Callback::new(move |note_id: String| {
        let plan_id = id.get_value();
        leptos::task::spawn_local(async move {
            match withdraw_file_note(plan_id.to_string(), note_id).await {
                Ok(updated) => {
                    state.error.set(None);
                    state.kingdom.update(|k| k.insert(updated));
                }
                Err(e) => state.error.set(Some(e.to_string())),
            }
        });
    });

    // Putting the review to the court. An `Action` for the reason sending the
    // proposal's notes is one: the button reads its own pending state, and a
    // second click while the first is in flight does nothing -- a double send
    // would drain the review once and then report a confusing failure for notes
    // that in fact landed.
    let send_review = Action::new(move |_: &()| {
        let plan_id = id.get_value();
        async move {
            match send_file_notes(plan_id.to_string()).await {
                Ok(_) => {
                    state.error.set(None);
                    if let Ok(k) = get_kingdom().await {
                        state.kingdom.set(k);
                    }
                    on_draft.run(plan_id);
                }
                Err(e) => state.error.set(Some(e.to_string())),
            }
        }
    });
    let send_the_review = Callback::new(move |_: ()| {
        send_review.dispatch(());
    });
    let sending_review = Signal::derive(move || send_review.pending().get());

    // What the court is doing to the page right now, for the panel's caption.
    // Read off the live plan, so it moves during a turn like everything else
    // the watch socket carries.
    let browser_deed =
        Memo::new(move |_| live.with(|p| p.as_ref().and_then(|p| browsing(&p.transcript))));

    view! {
        // A flex row: the city's files, then everything that is read against
        // them. The rail is a sibling of that whole group rather than of the
        // transcript alone, so it stays a full-height column down the left
        // whatever the group beside it is doing.
        <div class="chamber-frame">
            // The files of the city this plan works in. Part of the chamber and
            // not of the throne room: it describes the ground a *conversation*
            // stands on, so on the map it was an orphan telling the King to go
            // and choose a city, beside the screen whose whole job is choosing
            // one.
            <CityRail
                plan=id.get_value()
                summary=summary
                looking=looking
                activity=activity
                open_file=open_file
                on_read=read_file
                on_diff=review_file
            />
            // The transcript and the panel beside it: side by side rather than
            // stacked, because the transcript and the thing it describes are
            // read together -- stacking them made each one shorter to make room
            // for the other.
            <div class="chamber-body">
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
                                    {move || context.get().map(|(percent, window, tokens, weight)| view! {
                                        <span
                                            class="chamber-context"
                                            title=format!(
                                                "{tokens} tokens of this model's {window} context \
                                                 window, as the provider counted them on the last turn.{}",
                                                // The other limit, and the one that has
                                                // actually refused a request here. A gateway
                                                // enforces a token window *and* a body size,
                                                // and a plan once died three times on the
                                                // second while this header reported comfort
                                                // about the first. Stated in the tooltip
                                                // rather than beside the bar: it is the
                                                // number to reach for when a turn fails for
                                                // no visible reason, not one to watch.
                                                weight.as_ref().map_or_else(
                                                    String::new,
                                                    |w| format!(" The last request weighed {w} on the wire."),
                                                ),
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
                        // Colour follows what the plan *wants* where it wants something,
                        // and its status otherwise -- the same rule and the same helper
                        // shape the rail's `badge_for` uses, so the two surfaces cannot
                        // disagree about a plan the King is looking at from both.
                        <span class=move || {
                            let tint = match (is_subagent, wants.get()) {
                                // A subagent asks nobody for anything, so its badge is
                                // purely a status. See `subagent_status`.
                                (true, _) => status.get().css_suffix(),
                                (false, Some(needs)) => needs.css_suffix(),
                                (false, None) => status.get().css_suffix(),
                            };
                            format!("plan-badge plan-{tint}")
                        }>
                            {move || {
                                // A subagent is never reviewed and never merged: it reports
                                // to the model that sent it. Same states, honest words.
                                //
                                // A plan with something in front of the user is the other
                                // case where "Awaiting review" is too vague to be useful --
                                // it does not say that the wait is on *them*, or that there
                                // is a button. A label, deliberately, and not a sixth
                                // `PlanStatus`: nothing about the state machine changed.
                                //
                                // "Question" arrives through the same door and is the one
                                // that could not be said any other way: a plan parked on a
                                // question is `Drafting` throughout, so the status alone
                                // reports it as working when it is in fact blocked on him.
                                match (is_subagent, wants.get(), status.get()) {
                                    (true, _, PlanStatus::AwaitingReview) => "Reported",
                                    (true, _, PlanStatus::Drafting) => "Working",
                                    (false, Some(needs), _) => needs.label(),
                                    (_, _, s) => s.label(),
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
                            class:open=move || aside.get().is_browser()
                            title="Watch this plan's browser"
                            on:click=move |_| aside.update(|a| {
                                // Toggles, and in doing so closes a diff: there
                                // is one panel, and this is asking for it.
                                *a = if a.is_browser() { Aside::Hidden } else { Aside::Browser };
                            })
                        >
                            "\u{1F50D}"
                        </button>
                        // What the court was told before it was asked anything. The
                        // transcript carries every word since; this is the one text
                        // that shaped all of them and is otherwise invisible.
                        <button
                            class="orders-toggle"
                            class:open=move || reading_orders.get()
                            title="Read the standing orders this plan was given"
                            on:click=move |_| {
                                let opening = !reading_orders.get_untracked();
                                set_reading_orders.set(opening);
                                // Refetched on every open rather than kept: the
                                // permissions widen on approval and an AGENTS.md can
                                // be edited under a running plan, so a cached copy
                                // would answer a diagnostic question with stale text.
                                if opening {
                                    fetch_briefing();
                                }
                            }
                        >
                            "\u{1F4DC}"
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

                        // The King's words, waiting their turn. Below the drafting
                        // line because that is where they belong in time: the court
                        // started, and then he spoke. Drawn as ghosts rather than as
                        // messages because nobody has heard them yet -- putting them
                        // in the log proper would claim they were part of a
                        // conversation the model has not been shown.
                        <For
                            each=move || queued.get()
                            key=|word| word.id.clone()
                            let:word
                        >
                            {
                                let queued_id = word.id.clone();
                                view! {
                                    <div class="chat-msg is_user queued-word">
                                        <span class="msg-at">{clock(word.at)}</span>
                                        <span class="msg-who">"You"</span>
                                        <span class="msg-body">{word.body.clone()}</span>
                                        <span class="queued-mark">"waiting to be heard"</span>
                                        <button
                                            class="queued-drop"
                                            title="Take this back before the court hears it"
                                            on:click=move |_| withdraw.run((
                                                id.get_value(),
                                                queued_id.clone(),
                                            ))
                                        >
                                            "\u{00d7}"
                                        </button>
                                    </div>
                                }
                            }
                        </For>
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
                                notes=notes
                                sending=sending
                                on_accept=accept
                                on_set_aside=set_aside
                                on_note=annotate
                                on_withdraw_note=withdraw_a_note
                                on_send_notes=send_the_notes
                            />
                        })}
                    </Show>

                    // The King's own review of the code, gathered from both
                    // panels. Above the composer, beside the proposal card and
                    // for its reason: this is a decision still to make, and it
                    // sits where his attention already is.
                    //
                    // Deliberately *outside* the settled/subagent `Show` below,
                    // which swaps the composer for a record: this draws only
                    // when there is something in it, and a settled plan cannot
                    // have anything in it -- `annotate_file` refuses one.
                    <ReviewMargin
                        notes=review_notes
                        sending=sending_review
                        on_open=open_noted_file
                        on_withdraw=withdraw_line_note
                        on_send=send_the_review
                    />

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
                            //
                            // Never disabled. The court being busy is exactly when the
                            // King most wants to say something -- a twenty-minute turn
                            // going the wrong way used to be twenty minutes he could
                            // not speak into. What he sends now is queued and heard at
                            // the next round boundary.
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
                                    if drafting.get() {
                                        "The court is working \u{2014} say something for it to hear next\u{2026}"
                                    } else if permissions.get().is_full() {
                                        "Say more, or ask for a change\u{2026}"
                                    } else if proposal.get().is_some() {
                                        "Say what you would change about this plan\u{2026}"
                                    } else {
                                        "Say more about what you want\u{2026}"
                                    }
                                }
                                prop:value=move || reply.get()
                                on:input=move |ev| set_reply.set(event_target_value(&ev))
                                on:keydown=move |ev| {
                                    if ev.key() == "Enter" && !ev.shift_key() {
                                        ev.prevent_default();
                                        submit();
                                    }
                                }
                            />
                            // Still "Send" while the court works, because it still does
                            // something. What it does is queue, and the chip that appears
                            // in the log says so better than a disabled button did.
                            <button
                                class="start-btn"
                                on:click=move |_| submit()
                            >
                                "Send"
                            </button>

                            // Only while a turn is genuinely in flight. `drafting` alone
                            // is also true of a plan nobody has started yet, and offering
                            // to stop something that has not begun is a button that does
                            // nothing the first time it is pressed.
                            <Show when={move || drafting.get() && busy.get()}>
                                <button
                                    class="stop-btn"
                                    title="Stop the court where it stands"
                                    disabled={move || stop.pending().get()}
                                    on:click=move |_| { stop.dispatch(id.get_value()); }
                                >
                                    {move || if stop.pending().get() {
                                        "Stopping\u{2026}"
                                    } else {
                                        "Stop"
                                    }}
                                </button>
                            </Show>

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
                    // An overlay rather than a third column: the spyglass already
                    // owns the space beside the transcript, and this is read once
                    // and dismissed rather than watched alongside the work.
                    <Show when=move || reading_orders.get()>
                        <div
                            class="orders-backdrop"
                            on:click=move |_| set_reading_orders.set(false)
                        >
                            // Stopped here so a click *inside* the panel -- selecting
                            // the text, most likely -- does not dismiss it.
                            <div class="orders-panel" on:click=|ev| ev.stop_propagation()>
                                <header class="orders-head">
                                    <h2>"The standing orders"</h2>
                                    <p class="orders-note">
                                        "What the court is told before it is asked anything, \
                                         as it would be assembled now."
                                    </p>
                                    <button
                                        class="orders-close"
                                        title="Close"
                                        on:click=move |_| set_reading_orders.set(false)
                                    >
                                        "\u{00d7}"
                                    </button>
                                </header>
                                {move || match briefing.get() {
                                    // Deliberately a `<pre>` and not `Prose`: this is
                                    // the literal text sent to the model, and
                                    // rendering its markdown would hide the very
                                    // structure -- the tags, the block order -- that
                                    // a reader opens this to check.
                                    Some(Ok(text)) => view! {
                                        <pre class="orders-text">{text}</pre>
                                    }
                                    .into_any(),
                                    // Reported in the panel rather than through
                                    // `state.error`, which belongs to the composer.
                                    Some(Err(e)) => view! {
                                        <p class="orders-failed">
                                            "The orders could not be read: " {e}
                                        </p>
                                    }
                                    .into_any(),
                                    None => view! {
                                        <p class="orders-waiting">"Fetching the orders\u{2026}"</p>
                                    }
                                    .into_any(),
                                }}
                            </div>
                        </div>
                    </Show>
                </div>

                // The panel beside the transcript, and the handle that sizes
                // it. The handle exists only while a panel does: a divider with
                // nothing on one side of it is a control that does nothing.
                //
                // Two `Show`s over one `Aside` rather than two independent
                // flags, which is what makes "only one focused panel" true by
                // construction rather than by two handlers remembering to close
                // each other.
                <Show when=move || aside.get().is_browser()>
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

                <Show when=move || diff_file.get().is_some()>
                    <Resizer
                        width=diff_width
                        grows=Grows::Leftwards
                        bounds=DIFF_BOUNDS
                        storage_key=DIFF_WIDTH_KEY
                        class="spyglass-resizer"
                    />
                    <DiffView
                        plan=id.get_value()
                        path=diff_file
                        version=diff_version
                        notes=notes_on_diff
                        width=diff_width
                        on_note=annotate_line
                        on_close=Callback::new(move |_: ()| aside.set(Aside::Hidden))
                    />
                </Show>

                <Show when=move || source_file.get().is_some()>
                    <Resizer
                        width=source_width
                        grows=Grows::Leftwards
                        bounds=SOURCE_BOUNDS
                        storage_key=SOURCE_WIDTH_KEY
                        class="spyglass-resizer"
                    />
                    <SourceView
                        plan=id.get_value()
                        path=source_file
                        version=source_version
                        notes=notes_on_source
                        width=source_width
                        on_note=annotate_line
                        on_close=Callback::new(move |_: ()| aside.set(Aside::Hidden))
                    />
                </Show>
            </div>
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
    let plan_id = Memo::new(move |_| live.with(|p| p.as_ref().map(|p| p.id.clone())));

    // The chamber's one clock. It runs only while some deed is actually in
    // flight, and every running deed on the line reads this same signal -- see
    // `ticking_clock`.
    let anything_running = Memo::new(move |_| {
        live.with(|p| {
            p.as_ref().is_some_and(|p| {
                p.transcript
                    .iter()
                    .any(|e| matches!(e, Entry::Tool(d) if d.in_flight()))
            })
        })
    });
    let now = ticking_clock(anything_running);

    view! {
        <For
            each={move || {
                // `with`: this clones the transcript it is about to iterate,
                // which is unavoidable, but `get` would clone the whole plan
                // *and then* the transcript out of it.
                live.with(|p| {
                    p.as_ref()
                        .map(|p| p.transcript.clone())
                        .unwrap_or_default()
                        .into_iter()
                        .enumerate()
                        .collect::<Vec<_>>()
                })
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
                    // What the court *said* while asking rides above whichever
                    // of the three shapes below the call takes. Drawn here and
                    // not inside `ToolCallLine` because two of those shapes are
                    // not that component, and a batch's first call -- the one
                    // carrying the remark -- can be any of them. See `remark`.
                    //
                    // A question still waiting on him is the exception: that is
                    // not something to watch, it is something to do, so it is
                    // rendered as the thing to do.
                    Entry::Tool(d) => {
                        let said = remark(&d);
                        let thought = thinking(&d);
                        let deed = if is_open_question(&d) {
                            view! { <Question tool_call=d plan=plan_id/> }.into_any()
                        } else if d.tool == "spawn_agents" {
                            // Subagents are not a line of output either: the
                            // call's own text result is a summary, and the thing
                            // the user wants is the list of agents it sent and a
                            // way into each one.
                            view! { <Subagents tool_call=d plan=plan_id/> }.into_any()
                        } else {
                            view! { <ToolCallLine tool_call=d plan=plan_id now=now/> }.into_any()
                        };
                        view! { <Thinking thought=thought/> <Remark said=said/> {deed} }.into_any()
                    }
                }
            }
        </For>
    }
}

/// What the court said in the reply that asked for this deed.
///
/// Carried on the first call of a batch and `None` on the rest, so a reply that
/// asked for six things draws one remark and not six. That grouping is not a
/// decision being made here -- `api.rs` records it that way, for the same reason
/// the model is replayed it that way.
///
/// Blank is nothing. [`kingdom_core::ToolCall::in_reply`] already filters an
/// empty narration out before it is stored, so this is the second lock on the
/// same door: a record written by an older build can still hold `"  "`, and an
/// empty remark is a stripe of padding above a deed with no words in it.
fn remark(tool_call: &ToolCall) -> Option<String> {
    tool_call
        .narration
        .as_deref()
        .map(str::trim)
        .filter(|said| !said.is_empty())
        .map(str::to_string)
}

/// The court's own words about the deed beneath it.
///
/// Deliberately not a `chat-msg`. A bubble carries a speaker column and a clock,
/// which would present the sentence as an utterance of its own -- something the
/// court said, followed by some unrelated commands. It was not: it is the
/// preamble *of* those commands, and the log should say so without the King
/// having to work it out. So it is drawn as the header of the block it belongs
/// to, with the deed's own timestamp doing for both.
///
/// Rendered as markdown for the same reason every other piece of the court's
/// writing is: it is model prose, with backticked paths and the occasional list
/// in it. [`Prose`] already settles the escaping.
#[component]
fn Remark(said: Option<String>) -> impl IntoView {
    said.map(|said| {
        view! {
            <div class="chat-remark">
                <Prose text=said class="remark-body"/>
            </div>
        }
    })
}

/// The court's own thinking, as it arrived with this deed.
///
/// Grouped exactly as [`remark`] is, and for the same reason: one reply produced
/// one piece of reasoning however many things it asked for.
///
/// Only the prose half. [`kingdom_core::Reasoning::opaque`] is a signature or an
/// encrypted trace -- carried for the provider, meaningless to a reader, and
/// several kilobytes of base64 if it were ever drawn.
fn thinking(tool_call: &ToolCall) -> Option<String> {
    tool_call
        .reasoning
        .as_ref()?
        .text
        .as_deref()
        .map(str::trim)
        .filter(|thought| !thought.is_empty())
        .map(str::to_string)
}

/// What the court was thinking, folded away until asked for.
///
/// Kept apart from [`Remark`] because they are not the same thing. A remark is
/// what the court chose to say; reasoning is what it happened to think on the
/// way there -- longer, unaddressed, and sometimes a provider's own summary of
/// itself. Drawing them alike would tell the King that a model's musing carries
/// the weight of its stated intent.
///
/// Collapsed by default, and deliberately *unlike* [`ToolCallLine`], which is
/// now open. A deed is what the court chose to do and the King is watching for
/// it; reasoning is what it happened to think on the way, and a chamber that
/// renders every block of it in full buries the deeds under the musing.
///
/// Deliberately *not* markdown. Reasoning arrives as a stream of thought with
/// stray `#` and `*` in it that was never meant as formatting, and rendering it
/// as prose would turn a half-finished sentence into a heading.
#[component]
fn Thinking(thought: Option<String>) -> impl IntoView {
    let (open, set_open) = signal(false);

    thought.map(|thought| {
        // Lines rather than characters: it is the depth of the fold the King is
        // judging, and it is the figure Phoenix's own aside reports.
        let lines = thought.lines().count();
        let label = format!(
            "thinking ({lines} {})",
            if lines == 1 { "line" } else { "lines" }
        );
        let thought = StoredValue::new(thought);

        view! {
            <div class="chat-thought" class:is-open=move || open.get()>
                <button
                    class="thought-line"
                    on:click=move |_| set_open.update(|o| *o = !*o)
                >
                    <span class="thought-chevron">
                        {move || if open.get() { "\u{2303}" } else { "\u{2304}" }}
                    </span>
                    <span class="thought-label">{label}</span>
                </button>
                <Show when=move || open.get()>
                    <div class="thought-body">{move || thought.get_value()}</div>
                </Show>
            </div>
        }
    })
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
        tool_call
            .input
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

/// A question the model has stopped to ask, rendered where it was asked.
///
/// Inline in the transcript rather than as a modal. A modal would be the
/// obvious choice and is the wrong one: it puts the question in front of the
/// user with the work that prompted it hidden behind it, so he answers without
/// the context he needs. Here the reasoning and the commands that led to the
/// question are right above it, and he can scroll.
///
/// See [`is_open_question`] for why only an unanswered one gets this treatment.
///
/// # One question at a time
///
/// The court may ask up to four things at once, and they are put to the King
/// **one at a time**, with Back and Next between them and Submit on the last.
/// Every question ends in Submit -- even a lone one, which costs a second click
/// and buys three things worth more than it: an option and a sentence of his own
/// can stand together, `multi_select` becomes answerable at all, and the whole
/// set is answered as one act rather than four.
///
/// That is a correction rather than a preference. Rendering every question at
/// once made the *first* click send its own label and settle the call, so a
/// court that asked four things was told the answer to one of them and never
/// learned the others had been asked.
///
/// # Where the King's place is kept
///
/// In this component, which survives the push socket for a reason worth stating
/// because it is not obvious. `Transcript`'s `<For>` is keyed by
/// `(index, entry_version)`, and `entry_version` is 1 for the whole time a call
/// is in flight -- so deeds landing elsewhere in the chamber re-render the list
/// without rebuilding this row. A future change to that key would silently send
/// him back to question one every time the court did anything.
#[component]
fn Question(tool_call: kingdom_core::ToolCall, plan: Memo<Option<PlanId>>) -> impl IntoView {
    let state = expect_context::<KingdomState>();
    let tool_call_id = StoredValue::new(tool_call.id.clone());
    let questions = StoredValue::new(parse_questions(&tool_call.input));
    let count = questions.with_value(Vec::len);

    // One entry per question, in the order asked. Pre-sized so a step can be
    // written by index without the wizard having to grow it as it goes.
    let answers = RwSignal::new(vec![Answering::default(); count]);
    let (step, set_step) = signal(0usize);
    let (sent, set_sent) = signal(false);

    // Whether the question on screen has been answered. Advancing is gated on
    // it: the court asked because it could not guess, and the free-text box is
    // the escape hatch for "none of these" -- so there is always a way forward
    // that is not silence.
    let answered = Memo::new(move |_| {
        answers.with(|all| all.get(step.get()).is_some_and(Answering::is_answered))
    });
    let last = Memo::new(move |_| step.get() + 1 >= count);
    // Whether Back has anywhere to go, and whether the counter is worth drawing.
    // Memos rather than inline comparisons because `>` inside a `view!`
    // attribute closes the tag, and parenthesising it to say otherwise draws a
    // lint. Naming them reads better than either.
    let first = Memo::new(move |_| step.get() == 0);
    let several = count > 1;

    let submit = move || {
        let Some(plan_id) = plan.get_untracked() else {
            return;
        };
        if sent.get_untracked() {
            return;
        }
        let answer = questions.with_value(|asked| compose_answer(asked, &answers.get_untracked()));
        if answer.is_empty() {
            return;
        }
        // Locked as soon as he answers, so a double-click cannot send twice --
        // the second would find nothing waiting and report a confusing failure
        // for an answer that in fact landed.
        set_sent.set(true);
        let tool_call = tool_call_id.get_value();
        leptos::task::spawn_local(async move {
            if let Err(e) =
                crate::api::answer_question(plan_id.to_string(), tool_call, answer).await
            {
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
                // Only when there is more than one. With a single question the
                // counter is noise, and "1 of 1" reads like a wizard that lost
                // its other pages.
                <Show when=move || several>
                    <span class="question-step">
                        {move || format!("{} of {}", step.get() + 1, count)}
                    </span>
                </Show>
            </div>

            {move || {
                let at = step.get();
                let Some(q) = questions.with_value(|all| all.get(at).cloned()) else {
                    // No questions parsed at all. The court is parked on a call
                    // this browser cannot render, so say so rather than showing
                    // an empty card with a dead Submit under it -- he still has
                    // the composer, and the turn can be stopped.
                    return view! {
                        <p class="question-text">
                            "The court asked something this chamber could not read. \
                             Stop the turn, or say what you want in the composer."
                        </p>
                    }
                    .into_any();
                };

                let multi = q.multi_select;
                let options = q
                    .options
                    .into_iter()
                    .map(|opt| {
                        let chosen = opt.label.clone();
                        let detail = opt.description.clone();
                        let has_detail = !detail.is_empty();
                        let is_chosen = {
                            let label = opt.label.clone();
                            Memo::new(move |_| {
                                answers.with(|all| {
                                    all.get(at).is_some_and(|a| a.chose(&label))
                                })
                            })
                        };
                        view! {
                            <li>
                                <button
                                    class="question-option"
                                    class:chosen=move || is_chosen.get()
                                    disabled=move || sent.get()
                                    on:click=move |_| {
                                        answers.update(|all| {
                                            if let Some(a) = all.get_mut(at) {
                                                a.choose(&chosen, multi);
                                            }
                                        });
                                    }
                                >
                                    <span class="option-label">{opt.label}</span>
                                    <Show when=move || has_detail>
                                        <span class="option-detail">{detail.clone()}</span>
                                    </Show>
                                </button>
                            </li>
                        }
                    })
                    .collect_view();

                view! {
                    <div class="question-block">
                        <p class="question-text">{q.question}</p>
                        <Show when=move || multi>
                            <p class="question-hint">"Choose as many as apply."</p>
                        </Show>
                        <ul class="question-options">{options}</ul>

                        // The listed options are the court's guesses at what the
                        // King might want. He is the one deciding, so he must be
                        // able to say something that was not on the list -- and,
                        // now that answering is separate from sending, to say it
                        // *alongside* an option rather than instead of one.
                        <div class="question-own-words">
                            <input
                                class="decree-input"
                                r#type="text"
                                placeholder="Or say what you want in your own words\u{2026}"
                                prop:value=move || {
                                    answers.with(|all| {
                                        all.get(at).map(|a| a.words.clone()).unwrap_or_default()
                                    })
                                }
                                disabled=move || sent.get()
                                on:input=move |ev| {
                                    let words = event_target_value(&ev);
                                    answers.update(|all| {
                                        if let Some(a) = all.get_mut(at) {
                                            a.words = words;
                                        }
                                    });
                                }
                            />
                        </div>
                    </div>
                }
                .into_any()
            }}

            <div class="question-nav">
                // Present from the second question on rather than disabled on
                // the first: a control that can never do anything is one he has
                // to learn to ignore.
                <Show when=move || !first.get()>
                    <button
                        class="question-back"
                        disabled=move || sent.get()
                        on:click=move |_| set_step.update(|s| *s = s.saturating_sub(1))
                    >
                        "\u{2190} Back"
                    </button>
                </Show>
                <span class="nav-spacer"></span>
                <Show
                    when=move || last.get()
                    fallback=move || view! {
                        <button
                            class="question-next"
                            disabled=move || sent.get() || !answered.get()
                            title=move || {
                                if answered.get() { String::new() }
                                else { "Choose an option, or say what you want".to_string() }
                            }
                            on:click=move |_| set_step.update(|s| *s += 1)
                        >
                            "Next \u{2192}"
                        </button>
                    }
                >
                    <button
                        class="question-submit"
                        disabled=move || sent.get() || !answered.get()
                        title=move || {
                            if answered.get() { String::new() }
                            else { "Choose an option, or say what you want".to_string() }
                        }
                        on:click=move |_| submit()
                    >
                        {move || if sent.get() { "Sent" } else { "Submit" }}
                    </button>
                </Show>
            </div>
        </div>
    }
}

/// What the King has said about one question so far.
///
/// Chosen options and his own words are kept apart rather than flattened into
/// one string, because they are answers to different things: the options are the
/// court's guesses, and the words are what it failed to guess. Composing them
/// happens once, at the end, in [`compose_answer`].
#[derive(Clone, Debug, Default, PartialEq)]
struct Answering {
    /// Labels he has chosen, in the order he chose them. A `Vec` rather than a
    /// set because order is meaning here -- "Postgres, then SQLite" is a
    /// preference -- and because a question offers at most four options, so
    /// there is nothing to index.
    chosen: Vec<String>,
    /// Anything he typed instead of, or beside, the options.
    words: String,
}

impl Answering {
    /// True once there is something to send. Either half will do: the options
    /// are guesses, and being able to reject all of them in his own words is
    /// the point of the free-text box.
    fn is_answered(&self) -> bool {
        !self.chosen.is_empty() || !self.words.trim().is_empty()
    }

    fn chose(&self, label: &str) -> bool {
        self.chosen.iter().any(|c| c == label)
    }

    /// Records a click on one option.
    ///
    /// On a `multi_select` question this toggles, so a second click takes a
    /// choice back -- without it there would be no way to undo a mis-click, and
    /// the King would have to reload the chamber to unsay something. On an
    /// ordinary one it replaces, which is what makes the options behave like
    /// the radio buttons they are.
    fn choose(&mut self, label: &str, multi: bool) {
        if !multi {
            self.chosen = vec![label.to_string()];
            return;
        }
        match self.chosen.iter().position(|c| c == label) {
            Some(at) => {
                self.chosen.remove(at);
            }
            None => self.chosen.push(label.to_string()),
        }
    }

    /// This one answer as a line of prose, or nothing if he said nothing.
    fn say(&self) -> Option<String> {
        let words = self.words.trim();
        match (self.chosen.is_empty(), words.is_empty()) {
            (true, true) => None,
            (false, true) => Some(self.chosen.join(", ")),
            (true, false) => Some(words.to_string()),
            // Both. He picked something *and* qualified it, which the old
            // one-click card could not express at all.
            (false, false) => Some(format!("{} \u{2014} {words}", self.chosen.join(", "))),
        }
    }
}

/// Everything the King said, as the one string the parked call is waiting for.
///
/// `ask_user_question` resolves a oneshot carrying a `String`, so however many
/// questions were asked, exactly one answer goes back. That constraint is what
/// shapes this.
///
/// **A single question sends its bare answer**, with no scaffolding at all, so
/// the common case reads on the far side exactly as it always did -- the mock's
/// "You chose X" path and the tool's own test both depend on that, and neither
/// needed changing.
///
/// **Several are labelled and kept in the order they were asked.** Prose rather
/// than JSON because a model reads this out of a tool result: a labelled block
/// is what it parses best, and it is also what the King sees quoted back. The
/// ordering is the same courtesy `file_notes_as_decree` extends -- an answer
/// shuffled out of the order the questions were put in makes the reader sort it
/// before it can be used.
///
/// A question he answered nothing for is **named as unanswered** rather than
/// dropped. Silence and omission look identical to a model otherwise, and it
/// would fill the gap with a guess -- which is the very thing it stopped to ask
/// in order to avoid.
fn compose_answer(asked: &[Asked], answers: &[Answering]) -> String {
    if asked.len() == 1 {
        return answers.first().and_then(Answering::say).unwrap_or_default();
    }

    asked
        .iter()
        .enumerate()
        .map(|(i, q)| {
            let said = answers
                .get(i)
                .and_then(Answering::say)
                .unwrap_or_else(|| "(no answer)".to_string());
            format!("{}: {}\n{}", i + 1, q.question, said)
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// One thing the model wants decided.
#[derive(Clone)]
struct Asked {
    question: String,
    options: Vec<AskedOption>,
    /// Whether several options may be chosen at once.
    ///
    /// Declared in the tool's schema since the tool existed and never read
    /// until now: the old card sent whichever option was clicked first, so a
    /// question asking for several could only ever be answered with one.
    multi_select: bool,
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
            Some(Asked {
                question,
                options,
                // Absent means one answer. A model that wants several says so;
                // guessing otherwise would let the King pick two things for a
                // question with one slot.
                multi_select: q
                    .get("multi_select")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false),
            })
        })
        .collect()
}

/// True for a question the user has not answered yet.
///
/// Once answered it is ordinary history and renders as any other tool call,
/// which is what stops him answering the same question twice.
///
/// The same test [`kingdom_core::Plan::open_question`] applies, asked of one
/// entry rather than of a whole plan: the transcript is walked here anyway, so
/// this is the cheaper half of the same definition. Both read
/// [`kingdom_core::ASK_USER_QUESTION`], so a rename cannot leave one behind.
fn is_open_question(entry: &kingdom_core::ToolCall) -> bool {
    entry.tool == kingdom_core::ASK_USER_QUESTION && entry.in_flight()
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

/// One tool call: a summary line the user can skim, and the detail beneath it.
///
/// **Open by default.** The King is here to watch the court work, and the
/// output *is* the work -- a chamber of closed lines makes him click through
/// every one of them to learn what happened, which is the opposite of the
/// question this product exists to answer. What kept them shut was the fear of
/// a thousand-line build log burying the conversation, and that is answered by
/// capping the panes' height (`.deed-input`, `.deed-output` in
/// `_conversation.scss`) rather than by hiding them: a deed is a small scroll
/// box, not a truncation. The chevron still folds one away for when a
/// particular deed is not worth the room.
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

    let (open, set_open) = signal(true);

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
    let overrun = elapsed as u64 / 1_000 >= budget.seconds() && budget.overrunning_is_a_problem();

    Some((
        format!(
            "{} / {}",
            span(elapsed),
            span(budget.seconds() as i64 * 1_000)
        ),
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
///
/// Shared with the proposal card rather than spelled twice: a proposal's clock
/// and a message's clock must read identically, and two implementations is how
/// they come to differ by an hour on one side of a daylight-saving change.
pub(crate) fn clock(at: Option<Timestamp>) -> String {
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
        let location = web_sys::window()
            .expect("a browser has a window")
            .location();
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

    /// What the chamber rebuilds its body on.
    ///
    /// Mirrors the `open_plan` memo in [`Conversation`]. The memo itself needs
    /// a reactive runtime and a DOM to observe, but the decision it encodes is
    /// this pure one -- which part of a plan is allowed to trigger a rebuild --
    /// and that is the part a regression would get wrong.
    fn rebuild_key(plan: Option<&Plan>) -> Option<PlanId> {
        plan.map(|p| p.id.clone())
    }

    fn a_plan(id: &str) -> Plan {
        Plan::opened(
            PlanId::new(id),
            kingdom_core::CityId::new("forge"),
            "Do the thing",
            &kingdom_core::ModelChoice::new("mock", None),
            kingdom_core::Workspace::in_place("forge"),
        )
    }

    /// A turn moving must not rebuild the chamber.
    ///
    /// `ConversationBody` is constructed from this key, and constructing it
    /// again makes every signal in its body afresh: the files rail's cache of
    /// listings and the folders the King had opened, whether the spyglass is
    /// watching, and whatever he has half-typed into the composer. Keying on
    /// the plan's *value* rebuilt on every watch-socket push -- so a turn
    /// running anywhere collapsed his file tree to "Surveying..." and erased
    /// his textarea, twice per exchange, without him touching the tab.
    ///
    /// Everything that moves during a turn is read through the `live` memo
    /// instead, which is why the body does not need rebuilding to stay current.
    #[test]
    fn a_plan_that_merely_moved_does_not_rebuild_the_chamber() {
        let before = a_plan("plan-foundations");

        // The three ways a plan changes mid-turn: it speaks, it acts, and its
        // status moves. None of them is a different conversation.
        let mut after = before.clone();
        after.transcript.push(call("read_file", true));
        let spoke = kingdom_core::Message::new(Speaker::Assistant, "Here is what I found.");
        after.transcript.push(Entry::Message(spoke));
        after.status = PlanStatus::AwaitingReview;
        after.title = "A better title".to_string();

        assert_ne!(
            before, after,
            "the guard is worthless if the plan is unchanged"
        );
        assert_eq!(
            rebuild_key(Some(&before)),
            rebuild_key(Some(&after)),
            "a plan whose transcript, status or title moved is the same conversation",
        );
    }

    /// The other half: a rebuild is *required* when the conversation genuinely
    /// changes, because the body snapshots the fields that cannot change while
    /// one plan is open -- its id, workspace, prompt and errand parentage. Left
    /// standing, those would describe the plan the King navigated away from.
    #[test]
    fn a_different_plan_does_rebuild_the_chamber() {
        let one = a_plan("plan-foundations");
        let two = a_plan("plan-aqueduct");

        assert_ne!(rebuild_key(Some(&one)), rebuild_key(Some(&two)));
        // And a plan leaving the kingdom must fall back to "no such plan"
        // rather than leave the last one on screen.
        assert_ne!(rebuild_key(Some(&one)), rebuild_key(None));
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
        assert_eq!(
            span(9_990),
            "9.9s",
            "the decimal band never rounds up into the next one"
        );
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

    fn asked(question: &str, multi: bool) -> Asked {
        Asked {
            question: question.to_string(),
            options: vec![
                AskedOption {
                    label: "Left".into(),
                    description: String::new(),
                },
                AskedOption {
                    label: "Right".into(),
                    description: String::new(),
                },
            ],
            multi_select: multi,
        }
    }

    fn chose(labels: &[&str]) -> Answering {
        Answering {
            chosen: labels.iter().map(|l| l.to_string()).collect(),
            words: String::new(),
        }
    }

    /// A lone question answers exactly as it always did: the bare label, with no
    /// scaffolding around it.
    ///
    /// This is what makes the wizard a change to the *asking* and not to the
    /// answering. The mock's "You chose X" reply and `ask_user_question`'s own
    /// test both read this string, and neither needed touching.
    #[test]
    fn one_question_still_sends_the_bare_answer() {
        assert_eq!(
            compose_answer(&[asked("Which way?", false)], &[chose(&["Left"])]),
            "Left"
        );
    }

    /// The fault this replaces. Four questions asked, and the old card sent
    /// whichever option was clicked first -- so three answers were discarded and
    /// the court never learned it had asked for them.
    ///
    /// Every answer must be present, each against the question it answers, in
    /// the order they were put. The order is the same courtesy the King's other
    /// margin extends: an answer shuffled out of sequence has to be sorted by
    /// the reader before it can be used.
    #[test]
    fn several_questions_are_all_answered_in_the_order_they_were_asked() {
        let questions = [
            asked("Which database?", false),
            asked("Which cache?", false),
            asked("Deploy where?", false),
        ];
        let answers = [chose(&["Postgres"]), chose(&["Redis"]), chose(&["Fly"])];

        let composed = compose_answer(&questions, &answers);

        assert!(composed.contains("1: Which database?\nPostgres"));
        assert!(composed.contains("2: Which cache?\nRedis"));
        assert!(composed.contains("3: Deploy where?\nFly"));
        assert!(
            composed.find("Postgres") < composed.find("Redis")
                && composed.find("Redis") < composed.find("Fly"),
            "answers must arrive in the order the questions were put"
        );
    }

    /// Silence must be named rather than dropped. A model handed two answers to
    /// three questions cannot tell an unanswered one from a question it never
    /// asked, so it fills the gap with the guess it stopped to avoid making.
    #[test]
    fn a_question_left_unanswered_says_so() {
        let questions = [
            asked("Which database?", false),
            asked("Which cache?", false),
        ];
        let answers = [chose(&["Postgres"]), Answering::default()];

        let composed = compose_answer(&questions, &answers);
        assert!(composed.contains("2: Which cache?\n(no answer)"));
    }

    /// What the always-Submit shape buys: an option and a qualification of it
    /// can stand together. The old card could express only one or the other,
    /// because clicking an option *was* the send.
    #[test]
    fn an_option_and_the_kings_own_words_can_stand_together() {
        let both = Answering {
            chosen: vec!["The careful way".into()],
            words: "  but skip the migration  ".into(),
        };
        assert_eq!(
            both.say().as_deref(),
            Some("The careful way \u{2014} but skip the migration"),
            "the words are trimmed and joined to the choice rather than replacing it"
        );

        let only_words = Answering {
            chosen: Vec::new(),
            words: "neither, do it in place".into(),
        };
        assert_eq!(only_words.say().as_deref(), Some("neither, do it in place"));
        assert_eq!(Answering::default().say(), None);
    }

    /// `multi_select` toggles and an ordinary question replaces.
    ///
    /// The toggle is the half worth pinning: without it there is no way to take
    /// back a mis-click, and the King would have to reload the chamber to unsay
    /// something -- on a card whose whole purpose is that he is the one
    /// deciding.
    #[test]
    fn choosing_toggles_when_several_are_allowed_and_replaces_when_not() {
        let mut multi = Answering::default();
        multi.choose("Left", true);
        multi.choose("Right", true);
        assert_eq!(multi.chosen, vec!["Left", "Right"]);

        multi.choose("Left", true);
        assert_eq!(multi.chosen, vec!["Right"], "a second click takes it back");

        let mut single = Answering::default();
        single.choose("Left", false);
        single.choose("Right", false);
        assert_eq!(
            single.chosen,
            vec!["Right"],
            "one slot means the second choice replaces the first"
        );
    }

    /// Either half is enough to move on. The options are the court's guesses,
    /// so being able to reject all of them in his own words is the point of the
    /// free-text box -- gating Next on an *option* would trap him on a question
    /// none of whose answers are right.
    #[test]
    fn a_question_is_answered_by_a_choice_or_by_words() {
        assert!(!Answering::default().is_answered());
        assert!(chose(&["Left"]).is_answered());
        assert!(Answering {
            chosen: Vec::new(),
            words: "something else".into(),
        }
        .is_answered());
        assert!(
            !Answering {
                chosen: Vec::new(),
                words: "   ".into(),
            }
            .is_answered(),
            "whitespace is not an answer"
        );
    }

    /// `multi_select` reaches the view. It has been in the tool's schema since
    /// the tool existed and was read by nothing, which is why a question asking
    /// for several answers could only ever be given one.
    #[test]
    fn a_multi_select_question_is_parsed_as_one() {
        let parsed = parse_questions(&json!({
            "questions": [
                {
                    "question": "Which of these?",
                    "header": "Which",
                    "multi_select": true,
                    "options": [{ "label": "A" }, { "label": "B" }]
                },
                {
                    "question": "And this one?",
                    "header": "This",
                    "options": [{ "label": "Yes" }, { "label": "No" }]
                }
            ]
        }));

        assert_eq!(parsed.len(), 2);
        assert!(parsed[0].multi_select);
        assert!(
            !parsed[1].multi_select,
            "a question that does not ask for several must not be given several"
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
        assert_eq!(
            timing(&shell, within),
            Some(("29s / 30s".to_string(), false))
        );
        assert_eq!(
            timing(&browser, within),
            Some(("29s / 30s".to_string(), false))
        );

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

    /// The remark is a property of the *reply*, not of the call, and `api.rs`
    /// records it on the first call of a batch and nowhere else. This is what
    /// fails if that is ever "tidied" into copying the narration onto every
    /// call: the King would read the same sentence three times and take it for
    /// three separate decisions.
    #[test]
    fn one_reply_draws_one_remark_however_many_deeds_it_asked_for() {
        let said = "I'll read all three of these before I change anything.";
        let batch: Vec<ToolCall> = ["read_file", "read_file", "search"]
            .iter()
            .enumerate()
            .map(|(i, tool)| {
                // Exactly as `api.rs` writes it: the words ride on the first.
                ToolCall::started(format!("call-{i}"), *tool, json!({})).in_reply(
                    "reply-1",
                    None,
                    (i == 0).then(|| said.to_string()),
                )
            })
            .collect();

        let drawn: Vec<String> = batch.iter().filter_map(remark).collect();

        assert_eq!(
            drawn,
            vec![said.to_string()],
            "three deeds from one reply must carry one remark between them"
        );
    }

    /// `in_reply` already refuses to store a blank narration, so this is the
    /// second lock on the same door -- a record written by an older build can
    /// still hold whitespace, and an empty remark is a bordered stripe of
    /// padding above a deed with no words in it.
    #[test]
    fn a_deed_the_court_said_nothing_about_draws_nothing() {
        let silent = ToolCall::started("call-1", "bash", json!({}));
        assert_eq!(
            remark(&silent),
            None,
            "a call with no narration says nothing"
        );

        let mut blank = ToolCall::started("call-2", "bash", json!({}));
        blank.narration = Some("   \n  ".to_string());
        assert_eq!(
            remark(&blank),
            None,
            "whitespace is not something the court said"
        );

        let mut padded = ToolCall::started("call-3", "bash", json!({}));
        padded.narration = Some("  Checking the tests first.  ".to_string());
        assert_eq!(
            remark(&padded).as_deref(),
            Some("Checking the tests first."),
            "the words survive; the padding around them does not"
        );
    }

    /// The thinking is grouped exactly as the remark is, and only its prose half
    /// is ever drawn: the opaque half is a provider's signature, meaningless to
    /// a reader and kilobytes of base64 on the page.
    #[test]
    fn only_the_readable_half_of_the_thinking_is_drawn() {
        let signed = kingdom_core::Reasoning {
            text: None,
            opaque: [("signature".to_string(), json!("c2lnbmF0dXJl"))]
                .into_iter()
                .collect(),
        };
        let carried =
            ToolCall::started("call-1", "bash", json!({})).in_reply("reply-1", Some(signed), None);
        assert_eq!(
            thinking(&carried),
            None,
            "a signature is carried for the provider, not shown to the King"
        );

        let thought = kingdom_core::Reasoning {
            text: Some("  Two ways at this.  ".to_string()),
            opaque: Default::default(),
        };
        let mused =
            ToolCall::started("call-2", "bash", json!({})).in_reply("reply-2", Some(thought), None);
        assert_eq!(thinking(&mused).as_deref(), Some("Two ways at this."));
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
