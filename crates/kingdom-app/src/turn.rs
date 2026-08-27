//! Taking turns with the model: the loop that turns a decree into work.
//!
//! This is the agent loop, and everything it needs to answer for itself. It was
//! carved out of [`crate::api`], which had grown to 4,162 lines by holding two
//! unrelated jobs: the `#[server]` functions that *are* the browser/server wire
//! -- short, each one a request -- and this, which is not a request at all but a
//! conversation that outlives the one that started it.
//!
//! The seam between them is narrow and worth stating, because it is what made
//! the split possible:
//!
//! - [`api`](crate::api) calls **into** here at exactly two points:
//!   `draft_plan` starts [`converse`] for a plan the King opened, and
//!   `tools::spawn_agents` calls [`spawn_subagents`] for the errands the court
//!   sends. Both hand over a plan id and get a settled [`Plan`] back.
//! - Here calls **back into** [`api`](crate::api) only for the records:
//!   `lock`, `update`, `snapshot`, `remember`, `next_plan_number`.
//!   Those are the kingdom's shared state, and they stay where the state is.
//!
//! Nothing else crosses. In particular the *stopping* of a turn is shared with
//! `api::stop_plan` through [`crate::turns`] -- the registry of what is running
//! -- rather than by either module reaching into the other.
//!
//! ## Why the loop is here rather than behind the model
//!
//! A provider running its own tools would do so with nothing recording them:
//! the conversation would show a long silence and then an answer, and a crash
//! mid-way would leave no trace of what had already been done to the workspace.
//! Every step this loop takes is published to the conversation as it happens,
//! which is what makes a five-minute turn watchable instead of a spinner.

#![cfg(feature = "ssr")]

use crate::api::{city_root_of, lock, next_plan_number, remember, snapshot, update};
use kingdom_core::{Plan, PlanId};
use leptos::prelude::ServerFnError;

/// How many times the model may act before it must speak.
///
/// A loop that never ends is the failure mode worth being loud about: an agent
/// retrying a broken command against a paid model spends real money producing
/// nothing, and it does so quietly. Reaching this is recorded as a note, so the
/// user sees the plan stopped rather than finding a plausible answer that was
/// actually a truncation.
pub(crate) const MOST_ROUNDS: usize = 500;

/// How many times one round may ask the model before the turn gives up.
///
/// Three: the first ask and two more. Unrelated to [`MOST_ROUNDS`], which
/// bounds how many times the court may *act*; this bounds how many times a
/// single round may be *asked for*, and only when the failure is one that
/// asking again could fix.
///
/// Small on purpose. A retry is a bet that the failure was a hiccup, and the
/// bet is paid for in the user's time and quota. Two extra attempts clear the
/// empty reply that killed a real plan three times in ninety seconds; a
/// dozen would turn a genuinely broken conversation into a long silence with a
/// large bill at the end of it.
const MOST_ATTEMPTS: usize = 3;

/// What the model is told when the turn before this one came back empty.
///
/// Sent as [`crate::llm::Brief::aside`] -- on the wire only, never as a `Turn`
/// and never in the transcript. It exists because the King's own retry would
/// otherwise resend the exact request that produced the silence: the note the
/// failure left is deliberately not part of what a model is handed, so without
/// this the second attempt is indistinguishable from the first.
///
/// Written as a fact rather than an instruction. "Your last reply was empty" is
/// something the model can act on; "do not send an empty reply" is a rule it
/// cannot check it is following, and prompts that scold tend to be argued with
/// rather than obeyed -- the mermaid hint in `system_prompt` is Kingdom's
/// standing lesson on that.
const AFTER_SILENCE: &str = "Your previous reply arrived with no content and no tool calls, so \
     the turn could not continue. Nothing you intended to say or do was recorded. Pick up where \
     the conversation above leaves off: if you were mid-investigation, carry on with the next \
     tool call; if you had reached an answer, give it.";

/// Whether a failed attempt is worth making again.
///
/// Split from [`converse`] so the judgement can be tested without a kingdom, a
/// credential or a running turn -- the same reason [`halted`] is split from
/// [`stopped`], and the same reason it matters: this decides whether a plan
/// recovers from a hiccup or dies on it, and that is not something a reader can
/// check by eye from inside a `tokio::select!`.
fn worth_asking_again(error: &crate::llm::ModelError, attempt: usize) -> bool {
    error.is_transient() && attempt < MOST_ATTEMPTS - 1
}

/// Whether the last thing the *court* did was return an empty reply.
///
/// Not simply the last entry, and that distinction is the whole correctness of
/// this function. The sequence a real plan produces is `Note(EmptyReply)` and
/// then `Message(User, "Keep going")` -- [`receive`] appends the King's words
/// after the note -- so testing `transcript.last()` would answer `false` on
/// exactly the turn this exists to catch, and the whole fix would silently
/// never fire.
///
/// So the walk skips what the King said, because prodding a silent court does
/// not change what it last did, and stops at the first thing anything *else*
/// put in the log. A deed or a reply means the model has answered since, and
/// the silence is history rather than the current state -- telling it about
/// that now would report old news as current.
///
/// This is what makes the King's own retry differ from the request that failed.
/// Everything else about the two is identical -- the note the failure left is
/// deliberately not a [`kingdom_core::Turn`] -- so without this, "keep going"
/// resends the exact bytes that came back empty and receives the same silence.
fn follows_silence(plan: &Plan) -> bool {
    use kingdom_core::{Entry, NoteKind, Speaker};

    plan.transcript.iter().rev().find_map(|entry| match entry {
        // The King prodding a court that said nothing. Skipped: it is what
        // *caused* this turn, not evidence the silence was answered.
        Entry::Message(m) if m.speaker == Speaker::User => None,
        Entry::Note(n) if n.kind == NoteKind::EmptyReply => Some(true),
        // Anything else -- a deed, a reply, another sort of notice -- means the
        // conversation moved on.
        _ => Some(false),
    }) == Some(true)
}

/// Takes turns with the model until it speaks, proposes, runs out of rope, or
/// fails.
///
/// The loop lives here rather than behind the [`crate::llm::Model`] trait
/// because this is where the plan is. A provider running its own tools would do
/// so with nothing recording them: the conversation would show a long silence
/// and then an answer, and a crash mid-way would leave no trace of what had
/// already been done to the workspace.
///
/// Two callers: [`crate::api::draft_plan`], for a plan the user opened, and
/// [`spawn_subagents`], for each subagent the model sends. They differ only in
/// the `cap` handed in -- a subagent gets fewer rounds. The *permissions* are
/// no longer a parameter: they are read back off the plan each pass, because
/// they can change mid-conversation when the user accepts a proposal. Sharing
/// the loop is the point: a subagent that drafted through a second, simpler
/// path would be a second place for the busy mark, the tool call recording and
/// the push to drift.
pub(crate) async fn converse(
    plan_id: PlanId,
    city: crate::llm::CityBrief,
    workspace: kingdom_core::Workspace,
    city_name: String,
    choice: kingdom_core::ModelChoice,
    cap: usize,
) -> Result<Plan, ServerFnError> {
    use crate::llm::{Brief, Reply, SystemPrompt, ToolSpec};
    use crate::tools::Sandbox;
    use kingdom_core::{NoteKind, ToolCall, ToolOutcome};

    // Registers this turn as genuinely running, and yields the signal a Stop
    // travels down. Taken before the first model call and held to the last
    // line, because both of its readers depend on the bracket being exact:
    // `say` queues only while this exists, and `stop_plan` reads its absence as
    // a stale busy mark to repair.
    let mut halt = crate::turns::begin(&plan_id);

    // Read once, outside the loop: the kingdom's root cannot move while a turn
    // runs, and it bounds the guidance walk in every round's prompt. See
    // `SystemPrompt::assemble`.
    let kingdom_root = {
        let kingdom = lock()?;
        std::path::PathBuf::from(&kingdom.root)
    };

    let model = match crate::llm::open(&choice).await {
        Ok(model) => model,
        // A missing credential surfaces as a failed plan the user can see and
        // retry, rather than an error attached to nothing.
        Err(e) => return settle(plan_id, Err(e)),
    };

    // A network of this plan's own, if it was opened with one. Raised here --
    // once per turn, before the first round -- rather than inside each tool,
    // because all of them must land in the *same* namespace and a per-call
    // check would be three chances to forget.
    //
    // A failure here is fatal to the turn on purpose. The alternative is
    // running the agent on the shared network after the King asked for
    // isolation, which is the one outcome this feature must never produce
    // silently: he would find out when it took the port he was using.
    if snapshot(&plan_id).is_some_and(|p| p.network.is_isolated()) {
        if let Err(e) = crate::netns::ensure(&plan_id).await {
            // `Refused` rather than `Transport`: a missing slirp4netns or a
            // kernel that forbids namespaces is a settled answer, and
            // `is_transient` must not send the turn round again to be told the
            // same thing. The message names the package to install.
            return settle(plan_id, Err(crate::llm::ModelError::Refused(e.to_string())));
        }
        crate::netns::watch(&plan_id);
    }

    // The shared services this city declares. Raised here for the same reason
    // the namespace is -- once per turn, before the first round -- and after
    // it, because a service's address is handed to the plan's tools and those
    // tools must already be in the namespace that can reach it.
    //
    // A failure is fatal to the turn on the same reasoning: a plan whose
    // project declares a database, running with no database and no word said,
    // fails later in a way that reads as a bug in its own code.
    //
    // A city with no manifest costs nothing here -- `ensure` reads one absent
    // file and returns.
    if let Some(city_root) = city_root_of(&plan_id) {
        if let Err(e) = crate::services::ensure(&plan_id, &city_root).await {
            // `Refused` for the same reason as above: a missing Docker daemon
            // is a settled answer, and retrying the turn would only be told it
            // again.
            return settle(plan_id, Err(crate::llm::ModelError::Refused(e.to_string())));
        }
    }

    // Distinguishes this turn's rounds from every other turn's on the same
    // plan. See `batch_id`.
    let turn = uuid::Uuid::new_v4().to_string();

    for round in 0..cap {
        // A halt that landed between the two long awaits below is caught here
        // rather than being held until the next one. Without this, stopping a
        // turn during the brief window where it is neither calling the model
        // nor running a tool would appear to do nothing until the *next* model
        // call had already been paid for.
        if halt.was_halted() {
            return stopped(plan_id, None);
        }

        // The conversation is rebuilt from the plan each pass rather than
        // accumulated in a local. The tool calls recorded below are already in
        // it, and reading them back is what makes this loop's state the plan's
        // state -- so a reader of the transcript sees exactly what the model
        // saw.
        //
        // The *remit* is read back for the same reason, and it is the reason
        // this read now yields more than turns. A plan can gain its hands
        // mid-conversation: the King accepts a proposal, `approve_plan` widens
        // the remit, and the very next pass must offer the tools that grant
        // implies. Resolving the tools once before the loop -- as this used to
        // -- would have left an approved plan holding a counsellor's toolbox.
        let (turns, permissions, approved, after_silence) = {
            let mut kingdom = lock()?;

            // The user spoke while the court was working. Their words join the
            // log *before* it is read back, so this round's brief carries them.
            //
            // Here rather than anywhere else because this is the one moment in
            // a turn where nothing is half-done: the last deed is settled and
            // the next has not been asked for. Splicing them in mid-deed would
            // hand the model a conversation in which a tool call and its result
            // are separated by something nobody said at the time.
            //
            // Guarded rather than called unconditionally: `update` saves and
            // publishes, and a write per round with nothing in it would push a
            // whole plan over every watch socket for no news.
            if kingdom.plan(&plan_id).is_some_and(|p| !p.queued.is_empty()) {
                update(&mut kingdom, &plan_id, |p| {
                    p.hear_queued();
                });
            }

            let Some(plan) = kingdom.plan(&plan_id) else {
                return Err(ServerFnError::new("That plan vanished mid-decree."));
            };
            (
                plan.turns().collect::<Vec<_>>(),
                plan.permissions,
                plan.approved_proposal().is_some(),
                follows_silence(plan),
            )
        };

        // Whether the court has actually done anything yet is no longer asked:
        // a reply with prose and no tool call ends the turn, full stop.
        // A model that narrates instead of acting is answering the user early,
        // and the user can say so.

        // A model that cannot call tools still drafts perfectly good prose, so
        // it gets a prose-only turn rather than an error; one that cannot see is
        // not offered the tool that hands back a picture; and a plan under a
        // narrower remit is not offered the tools it may not run. All three
        // narrowings live in `ToolSpec::for_model`, so the reasoning is in one
        // place and no caller has to remember any of them.
        let tools = ToolSpec::for_model(model.as_ref(), permissions);
        let shop = Sandbox::new(workspace.clone())
            .for_plan(plan_id.clone())
            .under(permissions)
            // The fourth narrowing, and the one that could not live in
            // `for_model`: `browser_take_screenshot` is offered to every model
            // -- the King sees the picture either way -- but only a sighted one
            // is handed the base64 with it. See `Sandbox::sighted`.
            .seen_by_a_sighted_model(model.can_see());

        let brief = Brief {
            system_prompt: SystemPrompt::assemble(
                &city,
                &workspace,
                permissions,
                approved,
                &kingdom_root,
            ),
            turns,
            // Set only on the first round of a turn that follows a silent one.
            // Later rounds have a reply behind them and nothing to explain.
            aside: (after_silence && round == 0).then(|| AFTER_SILENCE.to_string()),
            tools: tools.clone(),
            // Every turn starts willing to carry the lot. A turn that is refused
            // for size lowers this and asks again; it deliberately does not
            // persist, because the next turn may be shorter than this one and
            // starting it pre-shrunk would shed a picture nobody needed to lose.
            budget: crate::llm::Budget::FULL,
        };

        // Raced against the halt so a Stop lands while the model is still
        // thinking, rather than after a reply nobody wants has been paid for.
        // Dropping the future drops the HTTP request, which is what Phoenix
        // achieves by aborting the task -- the same effect, with no task to
        // abort and with every careful clearing below still on the return path.
        //
        // `biased` so a halt already signalled wins deterministically instead
        // of by coin-flip against a reply that happened to arrive at once.
        //
        // Retried, but only for the failures a retry can actually fix. A reply
        // that came back empty is the absence of an answer rather than an
        // answer, and the same request resampled usually produces one -- yet
        // this loop used to return `settle(Err)` on the first of them, killing
        // a plan mid-investigation over a hiccup. A refusal or a missing
        // credential still fails at once: see `ModelError::is_transient`.
        //
        // A request the gateway would not read is the third case, and it is
        // neither of those. Resending it unchanged is pointless, so it is not
        // transient; but the request is ours, so `brief` is rebuilt with a
        // tighter budget and asked again. That is why `brief` is `mut` here.
        let mut brief = brief;
        let mut attempt = 0;
        let answer = loop {
            let outcome = tokio::select! {
                biased;
                _ = halt.halted() => return stopped(plan_id, None),
                answer = model.take_turn(&brief) => answer,
            };

            match outcome {
                Ok(answer) => break answer,
                // Too large, and there is still something left to shed. No
                // backoff: nothing is unwell and there is nothing to wait for --
                // the next request is simply a smaller one, and pausing before
                // sending it would only make a recoverable turn feel broken.
                Err(e) if e.is_shrinkable() => {
                    let Some(tighter) = brief.budget.tighter() else {
                        // Everything sheddable is already shed. Saying so beats
                        // reporting the gateway's bare refusal, which names no
                        // number and suggests no remedy.
                        return settle(plan_id, Err(e));
                    };
                    brief.budget = tighter;

                    let mut kingdom = lock()?;
                    update(&mut kingdom, &plan_id, |p| {
                        p.working_on = Some("Trimming the request and asking again".into());
                    });
                }
                Err(e) if worth_asking_again(&e, attempt) => {
                    attempt += 1;
                    // Told while it is happening rather than afterwards: a turn
                    // that goes quiet for a few seconds should say why, and this
                    // is the same channel `working_on` uses for everything else.
                    {
                        let mut kingdom = lock()?;
                        update(&mut kingdom, &plan_id, |p| {
                            p.working_on =
                                Some(format!("Asking again ({attempt} of {})", MOST_ATTEMPTS - 1));
                        });
                    }
                    // Raced against the halt for the same reason as the call
                    // itself: a Stop during the pause must not wait it out.
                    tokio::select! {
                        biased;
                        _ = halt.halted() => return stopped(plan_id, None),
                        _ = tokio::time::sleep(std::time::Duration::from_millis(
                            500 * (1 << (attempt - 1)),
                        )) => {}
                    }
                }
                // Out of attempts, or never worth one.
                Err(e) => return settle(plan_id, Err(e)),
            }
        };

        // Recorded before the reply is acted on, and in a write of its own.
        // Folding it into the tool-call recording below would tie it to *acts*
        // rather than to rounds: a round with three calls would write the same
        // reading three times, and a round that only speaks would never write
        // it at all.
        //
        // Being its own `update` also means `events::publish` pushes it, so the
        // header's bar climbs while a long tool loop is still running -- which
        // is exactly when the user wants to see it moving.
        //
        // Both numbers or neither: a count with no window is a percentage of
        // nothing. See `kingdom_core::ContextUsage`.
        //
        // The byte weight rides along rather than being written separately,
        // for the same reason and one more: it is the *other* limit a gateway
        // enforces, and the two are only comparable when they describe the same
        // request. Written here, they always do.
        if let Some(tokens) = answer.tokens {
            let window = model.context_window();
            let bytes = answer.bytes;
            let mut kingdom = lock()?;
            update(&mut kingdom, &plan_id, |p| {
                p.context = Some(kingdom_core::ContextUsage {
                    tokens,
                    window,
                    bytes,
                });
            });
        }

        match answer.reply {
            Reply::Spoke(draft) => {
                // Prose with no tool call ends the turn, as it does in Phoenix.
                // Kingdom used to intercept a narration-only first reply and
                // send it back round with an instruction appended; that was a
                // Kingdom invention and it is gone.
                // `settle` still records the reply and parks the plan; its
                // return value is deliberately discarded, because the decision
                // below must be made against a fresher read than it can give.
                settle(plan_id.clone(), Ok(draft))?;

                // ...unless the King got a word in while the court was
                // working. `settle` has already recorded the reply and parked
                // the plan, so nothing is lost if this is the last pass; but
                // words left queued here would be waited on by nobody, because
                // the turn that was going to hear them is this one.
                //
                // The queue is re-read under the lock rather than taken from
                // `answered`, and the turn deregisters in the same critical
                // section. That pairing is the whole correctness argument, and
                // the snapshot alone was not enough: `settle` releases the lock
                // before returning, so `say` could see a turn still running,
                // queue against it, and have this branch then consult a
                // snapshot taken before those words existed -- stranding them
                // behind a turn already on its way out.
                //
                // `say` reads the registry under this same lock, so after this
                // block it either queued (and we see it) or found no turn (and
                // takes the direct path, starting a fresh one). There is no
                // third case.
                let mut kingdom = lock()?;
                let waiting = kingdom.plan(&plan_id).is_some_and(|p| !p.queued.is_empty());

                // Going round again costs a round, so a drain on the last one
                // would fall through to the out-of-rope branch below and report
                // a turn that actually answered as having run out. The words
                // stay queued instead, exactly as they do when a turn fails
                // with some waiting: the next thing the King says flushes them.
                if !waiting || round + 1 >= cap {
                    halt.stand_down();
                    return kingdom
                        .plan(&plan_id)
                        .cloned()
                        .ok_or_else(|| ServerFnError::new("That plan vanished mid-decree."));
                }

                update(&mut kingdom, &plan_id, |p| {
                    p.status = kingdom_core::PlanStatus::Drafting;
                    p.working_on = Some("Hearing what the King added".to_string());
                });
                drop(kingdom);
                continue;
            }

            Reply::Acts(acts) => {
                // One id for everything this reply asked for, so the calls can
                // be replayed as the single decision they were rather than as a
                // sequence the model deliberated through. See
                // `kingdom_core::ToolCall::batch` and `batch_id`.
                let batch = batch_id(&plan_id, &turn, round);
                let mut first = true;

                for act in acts.calls {
                    // Recorded *before* it runs, and published by `update`, so
                    // the conversation shows the command while it is still
                    // going. This is the answer to "what is this agent doing
                    // right now", and it only works because the tool call is
                    // written down first.
                    {
                        let mut kingdom = lock()?;
                        // The thinking rides on the first call of the batch and
                        // nowhere else: one reply produced one piece of
                        // reasoning, however many things it asked for, and
                        // copying it onto each would replay it several times
                        // over as though the model had thought it repeatedly.
                        let reasoning = first.then(|| acts.reasoning.clone()).flatten();
                        let narration = first.then(|| acts.narration.clone()).flatten();
                        first = false;

                        update(&mut kingdom, &plan_id, |p| {
                            p.working_on = Some(describe(&act.tool, &act.input));
                            p.begin_tool_call(
                                ToolCall::started(
                                    act.id.clone(),
                                    act.tool.clone(),
                                    act.input.clone(),
                                )
                                .in_reply(batch.clone(), reasoning.clone(), narration.clone())
                                // Asked of the tool itself, from the same
                                // arguments the deed is being recorded with, so
                                // what the chamber tells the King it is waiting
                                // for and what the tool actually waits for
                                // cannot disagree. Read here rather than when
                                // the line is drawn, because a budget that
                                // arrives with the result would appear only
                                // once it no longer mattered.
                                .waiting(crate::tools::waits_for(&act.tool, &act.input, &shop)),
                            );
                        });
                    }

                    // Read off the arguments *before* they are handed to the
                    // tool, and from the same value the deed was recorded with
                    // a moment ago -- so what the transcript shows and what the
                    // chamber offers cannot disagree.
                    //
                    // Reads the draft the call names, which is why it takes the
                    // sandbox: the plan's body lives in a file now, not in the
                    // arguments. See `tools::propose_plan`.
                    let put = (act.tool == "propose_plan")
                        .then(|| crate::tools::propose_plan::proposed(&act.input, &shop))
                        .flatten();

                    // The lock is deliberately not held across this. A tool can
                    // take minutes -- and `ask_user_question` waits on a person
                    // -- so a held lock would freeze every other plan and every
                    // conversation in the kingdom behind it.
                    //
                    // Bound to this tool call, so a tool that has to reach back
                    // out to the user has something the browser can name when
                    // it answers.
                    // Raced against the halt for the same reason as the model
                    // call, and with one deliberate consequence: a stopped
                    // `bash` keeps its process. Phoenix documents the same
                    // choice, and here the `JOBS` registry means the handle
                    // survives, so a later turn can still peek at it or kill
                    // it. Killing on stop is a separate decision about what a
                    // halt means -- see the task's out-of-scope note.
                    // Bound before the race rather than inside it: a temporary
                    // built in a `select!` arm is dropped at the end of the
                    // statement, and `invoke` borrows it for the length of the
                    // call.
                    let bench = shop.for_tool_call(&act.id);
                    let outcome = tokio::select! {
                        biased;
                        _ = halt.halted() => return stopped(plan_id, Some(&act.id)),
                        outcome = crate::tools::invoke(&act.tool, act.input, &bench) => outcome,
                    };

                    let mut kingdom = lock()?;
                    let mut proposed = None;
                    update(&mut kingdom, &plan_id, |p| {
                        if !p.settle_tool_call(&act.id, outcome.clone()) {
                            // Cannot happen -- the tool call was recorded a
                            // moment ago under the same id -- but a silently
                            // dropped result would leave the model waiting
                            // forever for a call the transcript says it never
                            // made.
                            p.note(
                                NoteKind::Failed,
                                format!(
                                    "A result arrived for a call ({}) that is not in this \
                                     plan's log. This is a bug in Kingdom.",
                                    act.id
                                ),
                            );
                        }

                        // A plan put to the King ends the turn.
                        //
                        // The outcome is checked as well as the arguments: a
                        // refused call -- bad arguments, or the wrong remit --
                        // is one the model should be told about and allowed to
                        // correct on the next round, not one that parks the
                        // plan in front of the King with nothing to read.
                        let accepted = matches!(&outcome, ToolOutcome::Done { output, .. }
                            if output == crate::tools::propose_plan::PROPOSED);
                        if let (Some((title, body)), true) = (put.clone(), accepted) {
                            p.propose(title, body);
                            p.working_on = None;
                            p.status = kingdom_core::PlanStatus::AwaitingReview;
                            proposed = Some(p.clone());
                        }
                    });

                    // Nothing is parked and no request stays open: the plan is
                    // on disk with its proposal, and the chamber has been told.
                    // A server restart mid-review therefore loses nothing --
                    // see the module docs on `tools::propose_plan` for why that
                    // is worth more than resuming in place would have been.
                    if let Some(plan) = proposed {
                        // Unless the King was already speaking. A proposal
                        // parks the turn *for review*, and words queued during
                        // it are that review arriving early -- the same path
                        // `say` + `draft_plan` take for notes sent back on a
                        // proposal, without the round trip.
                        //
                        // Safe to read off `plan` here, unlike the prose path
                        // above: `proposed` was cloned inside the `update`
                        // that ran under the lock this scope still holds, so
                        // no `say` can have run since. `stand_down` happens
                        // under that same lock for the reason given there.
                        //
                        // The round check matches the prose path: draining
                        // costs a round, and spending the last one would report
                        // a plan that did propose as having run out of rope.
                        if plan.queued.is_empty() || round + 1 >= cap {
                            halt.stand_down();
                            return Ok(plan);
                        }
                        update(&mut kingdom, &plan_id, |p| {
                            p.status = kingdom_core::PlanStatus::Drafting;
                            p.working_on = Some("Hearing what the King added".to_string());
                        });
                        break;
                    }
                }

                // Back round to ask again, now with the results in the log.
            }
        }
    }

    // Out of rope. Recorded as a note rather than a reply: nothing was said,
    // and dressing a truncation as counsel would hand the user an answer that
    // is really an unfinished job.
    let mut kingdom = lock()?;
    let updated = update(&mut kingdom, &plan_id, |p| {
        p.working_on = None;
        p.status = kingdom_core::PlanStatus::Failed;
        p.summary = format!("Stopped after {cap} rounds without an answer.");
        p.note(
            NoteKind::Failed,
            format!(
                "The court acted {cap} times in {city_name} without reaching an \
                 answer, and was stopped. Its work so far is still in the workspace; \
                 say something to send it round again."
            ),
        );
    });

    updated.ok_or_else(|| ServerFnError::new("That plan vanished mid-decree."))
}

/// Sends a set of subagents and waits for them all to report.
///
/// Called by the `spawn_agents` tool, which is itself running inside its
/// parent's [`converse`] loop -- so this is the turn loop calling itself, one
/// level down, with narrower permissions. That shape is why `converse` is
/// `pub(crate)` rather than private.
///
/// Each subagent is a real plan: recorded, watched and pushed like any other,
/// which is what lets the user open one and read it while it works. They run
/// concurrently, which is only safe because a subagent is born under
/// [`kingdom_core::Permissions::ReadOnly`] and so cannot write -- see
/// [`Plan::spawned`], which is where that is now settled, and
/// `tools::spawn_agents`.
///
/// Returns the reports as one block of text for the model, or `Err` with a
/// reason to report to it when no subagent could be sent at all.
pub(crate) async fn spawn_subagents(
    parent_id: &PlanId,
    tool_call: &str,
    tasks: Vec<crate::tools::spawn_agents::Errand>,
    patience: std::time::Duration,
) -> Result<String, String> {
    // Everything the subagents need is read once, under one lock: the parent
    // they are cut from, and the city they work in. Holding it across the model
    // calls below would freeze every other plan in the kingdom.
    let (subagents, city_brief, city_name) = {
        let mut kingdom = lock().map_err(|e| e.to_string())?;

        let Some(parent) = kingdom.plan(parent_id).cloned() else {
            return Err(
                "The plan that sent these errands is no longer in the records.".to_string(),
            );
        };
        let Some(city) = kingdom.city(&parent.city).cloned() else {
            return Err("That plan's city is gone.".to_string());
        };

        // A subagent is briefed on the *workspace*, exactly as its parent is:
        // it is sent to look at the work in progress, not at a pristine copy.
        let city_brief = crate::llm::CityBrief::from_city(&city, &parent.workspace);

        let mut subagents = Vec::new();
        for errand in tasks {
            let id = PlanId::new(format!("plan-{}", next_plan_number()));
            let mut subagent = Plan::spawned(id.clone(), &parent, tool_call, errand.task.clone());
            let root = std::path::PathBuf::from(&kingdom.root);
            remember(&root, &mut subagent);
            // Pushed as well as recorded, so the parent's conversation can draw
            // the subagent the instant it exists rather than when it first
            // speaks.
            crate::events::publish(&subagent);
            kingdom.plans.push(subagent);
            subagents.push((id, errand));
        }

        (subagents, city_brief, city.name)
    };

    // All at once. The permissions are what make this safe: they share one
    // worktree and none of them can write to it. That is settled on the
    // subagent itself, by `Plan::spawned`, and read back by the loop -- so the
    // invariant travels with the plan rather than with this call site.
    let running: Vec<_> = subagents
        .iter()
        .map(|(id, errand)| {
            let id = id.clone();
            let city_brief = city_brief.clone();
            let city_name = city_name.clone();
            // How many rounds this one gets: what the model asked for, or the
            // default. Per subagent rather than one figure for the batch, so a
            // cheap lookup can be capped short without shortening the survey
            // running beside it.
            let cap = errand.max_turns;
            // Read back rather than carried down, so a subagent is drafted by
            // what its own record says -- the same rule `draft_plan` follows.
            let found = snapshot(&id).map(|p| (p.workspace.clone(), p.choice()));
            tokio::spawn(async move {
                let Some((workspace, choice)) = found else {
                    return Err(ServerFnError::new("That errand vanished before it began."));
                };
                converse(id, city_brief, workspace, city_name, choice, cap).await
            })
        })
        .collect();

    // One deadline across the whole call rather than one per subagent: they run
    // concurrently, so the bound that matters is how long the *parent* waits.
    //
    // Collected one at a time against that shared deadline, which is what keeps
    // a partial answer possible -- a subagent that has already reported is
    // collected even if a later one has to be given up on. Timing the whole
    // batch out as a unit would throw away work that was finished and paid for.
    let deadline = tokio::time::Instant::now() + patience;
    let mut outcomes: Vec<Option<Plan>> = Vec::with_capacity(running.len());
    for handle in running {
        let settled = tokio::time::timeout_at(deadline, handle)
            .await
            .ok()
            .and_then(|joined| joined.ok())
            .and_then(|result| result.ok());
        outcomes.push(settled);
    }

    Ok(report(&subagents, &outcomes))
}

/// Renders what the subagents found, for the model that sent them.
///
/// Each block names the subagent's plan id, which is what lets the parent refer
/// to one in its own reply -- and what lets the user find the same conversation
/// the parent read.
fn report(
    subagents: &[(PlanId, crate::tools::spawn_agents::Errand)],
    outcomes: &[Option<Plan>],
) -> String {
    use kingdom_core::Speaker;

    let mut out = String::new();
    for (i, (id, errand)) in subagents.iter().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        out.push_str(&format!(
            "--- errand {} ({id}) ---\nTask: {}\n\n",
            i + 1,
            errand.task
        ));

        // Read from the record rather than from the returned plan: a subagent
        // that timed out here may still have said something, and its record is
        // where that would be.
        let plan = outcomes
            .get(i)
            .and_then(|p| p.clone())
            .or_else(|| snapshot(id));

        // The last thing the *model* said is the subagent's answer. Read with a
        // fold rather than `next_back` because `messages()` yields a plain
        // forward iterator; taking the last match is the whole intent.
        let messages = plan.as_ref().and_then(|p| {
            p.messages()
                .filter(|u| u.speaker == Speaker::Assistant)
                .last()
                .map(|u| u.body.clone())
        });
        match messages {
            Some(body) => out.push_str(&body),
            None => out.push_str(
                "This errand did not report back. It may have run out of rope or \
                 taken too long; carry on without it, or ask something narrower.",
            ),
        }
        out.push('\n');
    }
    out
}

/// Plain-language "what is happening right now", for the rail and the map.
///
/// The tool's name alone is close to useless to the user -- "bash" tells him
/// nothing that "an agent is working" did not. The argument is the information,
/// so it is what gets shown.
fn describe(tool: &str, input: &serde_json::Value) -> String {
    // Waiting on a person is not the same kind of busy as running a command,
    // and "who is blocked behind whom" is one of the three questions this
    // product exists to answer. It gets said in those words rather than being
    // rendered as another tool name.
    if tool == kingdom_core::ASK_USER_QUESTION {
        return "Waiting on the King".to_string();
    }

    // Likewise a plan put to him. Not a command running and not a question
    // either: the turn is over and the next move is his, which is a different
    // sort of "busy" again and worth its own words.
    if tool == "propose_plan" {
        return "Awaiting the King's word".to_string();
    }

    let subject = ["cmd", "path", "pattern", "url", "selector", "query"]
        .iter()
        .find_map(|k| input.get(*k).and_then(|v| v.as_str()))
        .unwrap_or_default()
        .trim();

    if subject.is_empty() {
        return format!("Running {tool}");
    }

    let short: String = subject.chars().take(60).collect();
    format!("{tool}: {short}")
}

/// Records a drafting outcome on the plan and marks it no longer busy.
///
/// The clearing must happen on the failure path too: a plan left marked busy
/// could never be retried, because `draft_plan` would keep short-circuiting.
fn settle(
    plan_id: PlanId,
    outcome: Result<crate::llm::Draft, crate::llm::ModelError>,
) -> Result<Plan, ServerFnError> {
    use kingdom_core::{NoteKind, PlanStatus, Speaker};

    let mut kingdom = lock()?;

    let updated = update(&mut kingdom, &plan_id, |plan| {
        plan.working_on = None;
        match &outcome {
            Ok(draft) => {
                // The title is deliberately *not* touched here. It was written
                // once from the decree and is rewritten only when the court
                // proposes -- see `Plan::propose`. Setting it from whatever
                // heading the model happened to lead this reply with made the
                // rail label change under the user on every turn.
                plan.summary = draft.summary.clone();
                plan.status = PlanStatus::AwaitingReview;
                plan.say(Speaker::Assistant, draft.body.clone());
                // The model has spoken, so any proposal the user had not
                // accepted is no longer the question on the table. Without
                // this, a plan that proposed, was asked something, and answered
                // in prose would still be offering to start work the
                // conversation has moved past. An accepted proposal is left
                // alone -- it is not a question, it is the job in hand.
                plan.set_aside_proposal();
            }
            Err(e) => {
                // A failure is Kingdom reporting, not the model speaking -- so
                // the next turn must not replay it as prior counsel.
                let message = e.to_string();
                plan.status = PlanStatus::Failed;
                plan.summary = message.clone();
                // An empty reply is noted as *itself*, so the next turn can
                // find it and tell the model what happened. Every other failure
                // stays a plain `Failed`: a refusal or a missing credential is
                // not something to explain to a model, it is something for the
                // King to fix. See `AFTER_SILENCE` and `converse`.
                let kind = match &e {
                    crate::llm::ModelError::Empty(_) => NoteKind::EmptyReply,
                    _ => NoteKind::Failed,
                };
                plan.note(kind, message);
            }
        }
    });

    updated.ok_or_else(|| ServerFnError::new("That plan vanished mid-decree."))
}

/// Records that the user called a halt, and marks the plan no longer busy.
///
/// The sibling of [`settle`], and it differs in exactly one judgement: the plan
/// is left `AwaitingReview`, not `Failed`. Nothing failed. The user chose this,
/// and painting a deliberate act in the colour of a breakage misreports who did
/// what -- besides putting the plan into the status the conversation offers a
/// retry against, which is not what he asked for.
///
/// `in_flight` names the deed the halt interrupted, when it interrupted one. It
/// is settled as [`ToolOutcome::Refused`] -- the same variant `store::reconcile`
/// uses for a call the server died during, for the same reason: a call left
/// unsettled is replayed to the model on every later turn as though it were
/// still running, and the model waits forever for a result nobody will send.
fn stopped(plan_id: PlanId, in_flight: Option<&str>) -> Result<Plan, ServerFnError> {
    // A question parked in front of the user belongs to a turn that has now
    // stopped. Clearing it keeps `PENDING` from holding a oneshot that nothing
    // will ever answer, and stops a stale question in an open tab from
    // resolving a call that is already settled below.
    if let Some(id) = in_flight {
        crate::tools::ask_user_question::abandon(&plan_id, id);
    }

    let mut kingdom = lock()?;

    let updated = update(&mut kingdom, &plan_id, |plan| halted(plan, in_flight));

    updated.ok_or_else(|| ServerFnError::new("That plan vanished mid-decree."))
}

/// What a halt leaves on the plan. Split from [`stopped`] so the judgement can
/// be tested without a kingdom or a running turn.
fn halted(plan: &mut Plan, in_flight: Option<&str>) {
    use kingdom_core::{NoteKind, PlanStatus, ToolOutcome};

    plan.working_on = None;
    plan.status = PlanStatus::AwaitingReview;

    if let Some(id) = in_flight {
        plan.settle_tool_call(
            id,
            ToolOutcome::Refused {
                reason: "The King called a halt while this was running. Whether it \
                         finished is unknown."
                    .to_string(),
            },
        );
    }

    // A note rather than a reply: the court did not say this, Kingdom did.
    // Recording it as counsel would replay it to the model next turn as
    // something it had told the user itself.
    plan.note(
        NoteKind::Stopped,
        "The King called a halt. The court stopped where it stood; anything it had \
         already done is still in its workspace. Say something to set it going again.",
    );
}

/// The id shared by every call in one reply.
///
/// Scoped to the *turn* as well as the round, and that is the whole point of it
/// being a function. `round` restarts at zero each time the user sets a plan
/// going again, so an id built from the round alone repeats within one plan --
/// and [`crate::llm`]'s replay groups *consecutive* calls that share one.
///
/// [`kingdom_core::Plan::turns`] filters notes out, so a turn that ended on a
/// note rather than a message leaves its last call directly adjacent to the
/// next turn's first. The `Failed` note left behind when the server stops
/// mid-turn is exactly that shape, and it is common: the user says "keep
/// going", the new turn opens at round 0, and it collides with whatever the
/// dead turn ended on. Two separate decisions then replay as one assistant
/// message -- and the second's thinking is dropped, because only the first call
/// of a batch carries any.
fn batch_id(plan: &PlanId, turn: &str, round: usize) -> String {
    format!("{}-{turn}-{round}", plan.as_str())
}
#[cfg(all(test, feature = "ssr"))]
mod tests {
    use super::*;
    // The two fixtures are shared with `api`'s own tests, which still need them
    // for the paths that stayed there. Imported rather than copied: a second
    // `a_plan()` drifting from the first is how two suites end up disagreeing
    // about what an opened plan looks like.
    use crate::api::receive;
    use crate::api::tests::{a_plan, said};

    /// Two turns must not be able to claim the same batch id.
    ///
    /// The replay groups consecutive calls that share one, and `round` starts
    /// again at zero on every turn -- so without the turn in the id, the first
    /// call after "keep going" collides with the last call of the turn that
    /// died. The two decisions merge into one assistant message and the later
    /// one's thinking is discarded. See [`batch_id`].
    #[test]
    fn a_batch_id_is_not_reused_by_the_next_turn() {
        let plan = PlanId::new("plan-7");

        // The shape that bit: a turn that ended at round 0, then a new turn
        // opening at round 0 after the user said "keep going".
        assert_ne!(
            batch_id(&plan, "turn-a", 0),
            batch_id(&plan, "turn-b", 0),
            "round 0 of one turn must not be round 0 of the next"
        );

        // Within one turn the round still separates replies, or every call a
        // turn ever made would replay as a single vast decision.
        assert_ne!(
            batch_id(&plan, "turn-a", 0),
            batch_id(&plan, "turn-a", 1),
            "two replies in one turn are still two decisions"
        );

        // And calls from one reply still share an id, which is the grouping the
        // whole mechanism exists for.
        assert_eq!(batch_id(&plan, "turn-a", 3), batch_id(&plan, "turn-a", 3));
    }

    /// The regression this whole task exists for: a plan must not die on a
    /// reply that simply never arrived.
    ///
    /// A real plan was killed three times in ninety seconds by
    /// `Copilot returned an empty reply`, and every retry failed identically
    /// because the loop returned `settle(Err)` on the first failure of any
    /// kind. An empty reply is the absence of an answer, and the same request
    /// resampled usually produces one.
    ///
    /// The bound matters as much as the retry: this must give up, or a
    /// genuinely broken conversation becomes a long silence with a large bill.
    #[test]
    fn silence_is_asked_again_and_then_given_up_on() {
        let empty = crate::llm::ModelError::Empty("Copilot returned an empty reply.".into());

        assert!(
            worth_asking_again(&empty, 0),
            "the first empty reply must not kill the plan"
        );
        assert!(worth_asking_again(&empty, MOST_ATTEMPTS - 2));
        assert!(
            !worth_asking_again(&empty, MOST_ATTEMPTS - 1),
            "the retry must be bounded, or a broken conversation never stops costing"
        );
    }

    /// A failure that asking again cannot fix must fail at once.
    ///
    /// The other half of the judgement, and the one that keeps the retry
    /// honest. A missing credential stays missing and a refusal is a considered
    /// answer; retrying either spends the user's time and quota three times to
    /// be told the same thing, and delays the message he actually needs to act
    /// on.
    #[test]
    fn a_failure_that_will_not_change_is_not_asked_again() {
        let refused = crate::llm::ModelError::Refused("This decree cannot be drafted.".into());
        assert!(
            !worth_asking_again(&refused, 0),
            "a refusal is an answer, not a hiccup"
        );

        let no_credential = crate::llm::ModelError::Credential(
            crate::llm::credential::CredentialError::NotConfigured,
        );
        assert!(
            !worth_asking_again(&no_credential, 0),
            "a credential that is missing stays missing"
        );
    }

    /// The King's own retry has to reach the model as a different request.
    ///
    /// This is the half that made the bug feel unfixable. `settle` records the
    /// failure as a note, notes are deliberately excluded from `Plan::turns`,
    /// and so "keep going" rebuilt a byte-identical payload and received a
    /// byte-identical silence. Finding the note is what lets the next turn say
    /// something the failed one did not.
    #[test]
    fn a_turn_after_silence_knows_it_is_following_silence() {
        use kingdom_core::NoteKind;

        let mut plan = a_plan();
        assert!(!follows_silence(&plan), "a fresh plan follows nothing");

        plan.note(NoteKind::EmptyReply, "Copilot returned an empty reply.");
        assert!(follows_silence(&plan));

        // The sequence a real plan actually produces, and the one that nearly
        // shipped broken: `receive` appends the King's words *after* the note,
        // so the note is never the last entry by the time the next turn starts.
        // Reading `transcript.last()` answers `false` here -- on precisely the
        // turn this exists to catch.
        plan.say(kingdom_core::Speaker::User, "Keep going");
        assert!(
            follows_silence(&plan),
            "prodding a silent court does not change what it last did"
        );

        // And again, exactly as plan-15 recorded it.
        plan.note(NoteKind::EmptyReply, "Copilot returned an empty reply.");
        plan.say(kingdom_core::Speaker::User, "Keep going!");
        assert!(follows_silence(&plan));

        // Answered since: the silence is history, not the current state, and
        // telling the model about it now would report old news as current.
        plan.say(kingdom_core::Speaker::Assistant, "Here is what I found.");
        assert!(!follows_silence(&plan));
    }

    /// Only silence earns the explanation. Every other failure is something for
    /// the King to fix, not something to explain to a model -- and a plan that
    /// failed on a bad credential must not open its next turn apologising for a
    /// reply that was never empty.
    #[test]
    fn an_ordinary_failure_is_not_mistaken_for_silence() {
        use kingdom_core::NoteKind;

        let mut plan = a_plan();
        plan.note(NoteKind::Failed, "no credential: none configured");
        assert!(!follows_silence(&plan));

        plan.say(kingdom_core::Speaker::User, "try again");
        assert!(
            !follows_silence(&plan),
            "the walk past the King's words must not reach past an ordinary failure"
        );
    }

    /// A deed between the silence and now means the court has since acted, so
    /// there is nothing to explain. Pins the other end of the walk: stopping
    /// only at a user message is what keeps this from reporting an empty reply
    /// from ten rounds ago as though it had just happened.
    #[test]
    fn silence_the_court_has_already_moved_past_is_not_reported() {
        use kingdom_core::{NoteKind, ToolCall, ToolOutcome};

        let mut plan = a_plan();
        plan.note(NoteKind::EmptyReply, "Copilot returned an empty reply.");
        plan.say(kingdom_core::Speaker::User, "Keep going");

        // The retry worked: the court acted.
        plan.begin_tool_call(ToolCall::started("call-1", "bash", serde_json::json!({})));
        plan.settle_tool_call("call-1", ToolOutcome::done("ok"));

        assert!(!follows_silence(&plan));
    }

    /// A turn can fail with words still waiting -- `converse` deliberately does
    /// not drain on its failure exits. Those words must not then be overtaken
    /// by whatever the user says next, or the court reads his instructions in
    /// an order he never gave them.
    #[test]
    fn words_left_over_from_a_failed_turn_are_heard_before_newer_ones() {
        let mut plan = a_plan();
        plan.working_on = Some("thinking".into());

        receive(&mut plan, "first, while it worked".into(), true);
        receive(&mut plan, "second, while it worked".into(), true);

        // The turn dies without draining, as a failed one does.
        plan.working_on = None;
        plan.status = kingdom_core::PlanStatus::Failed;

        receive(&mut plan, "third, after it stopped".into(), false);

        let said = said(&plan);
        let ordered: Vec<&str> = said
            .into_iter()
            .filter(|body| body.contains("while it worked") || body.contains("after it stopped"))
            .collect();
        assert_eq!(
            ordered,
            vec![
                "first, while it worked",
                "second, while it worked",
                "third, after it stopped",
            ]
        );
        assert!(plan.queued.is_empty());
    }

    /// What a halt must leave behind, and the one thing it must not.
    ///
    /// The status is the judgement worth pinning: `Failed` would paint a
    /// deliberate act in the colour of a breakage, and it is also the status
    /// the conversation offers a retry against -- so a stopped plan would
    /// invite the user to restart the thing he just stopped.
    ///
    /// The deed must be settled for a harder reason: `Plan::turns` replays
    /// in-flight calls to the model on every later round, so a call left open
    /// is one the court waits on forever, in every turn the plan ever takes
    /// again.
    #[test]
    fn a_halt_closes_the_deed_it_interrupted_without_calling_it_a_failure() {
        use kingdom_core::{Entry, NoteKind, PlanStatus, ToolCall, Turn};

        let mut plan = a_plan();
        plan.working_on = Some("bash: cargo test".into());
        plan.begin_tool_call(ToolCall::started(
            "call-1",
            "bash",
            serde_json::json!({ "cmd": "cargo test" }),
        ));

        halted(&mut plan, Some("call-1"));

        assert_eq!(
            plan.status,
            PlanStatus::AwaitingReview,
            "the King stopped this on purpose -- it did not fail"
        );
        assert!(
            !plan.is_busy(),
            "the busy mark must go, or nothing can restart"
        );
        assert!(
            plan.turns()
                .all(|t| !matches!(t, Turn::Tool(d) if d.in_flight())),
            "an unsettled call is replayed to the model as still running, forever"
        );
        assert!(
            plan.transcript
                .iter()
                .any(|e| matches!(e, Entry::Note(n) if n.kind == NoteKind::Stopped)),
            "the King must be able to see why the court stopped"
        );
    }

    /// A halt between deeds has no call to close, and must still park the plan
    /// rather than falling through to a no-op. This is the path taken when Stop
    /// lands while the model is thinking.
    #[test]
    fn a_halt_with_no_deed_in_flight_still_parks_the_plan() {
        let mut plan = a_plan();
        plan.working_on = Some("thinking".into());

        halted(&mut plan, None);

        assert_eq!(plan.status, kingdom_core::PlanStatus::AwaitingReview);
        assert!(!plan.is_busy());
    }

    /// Stopping does not throw away what the King had already queued. The two
    /// are deliberately independent: he may have stopped the court precisely
    /// *because* of what he typed, and discarding it would lose the correction
    /// along with the work.
    #[test]
    fn a_halt_leaves_queued_words_waiting() {
        let mut plan = a_plan();
        plan.working_on = Some("thinking".into());
        plan.queue("stop and do this instead");

        halted(&mut plan, None);

        assert_eq!(plan.queued.len(), 1);

        // And the next thing he says flushes them, in order.
        receive(&mut plan, "here is the rest of it".into(), false);
        let ordered: Vec<&str> = said(&plan)
            .into_iter()
            .filter(|b| b.contains("instead") || b.contains("the rest"))
            .collect();
        assert_eq!(
            ordered,
            vec!["stop and do this instead", "here is the rest of it"]
        );
    }
}
