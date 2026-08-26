//! The event bus: publishing a plan's changes to whoever is watching it.
//!
//! Server-only. This is the push half of the conversation -- the browser no
//! longer asks "has anything happened?" on a timer, it is told.
//!
//! # Why the wire carries whole plans, not deltas
//!
//! The obvious design is to push events: "a line was added", "the status
//! changed". It is also the wrong one here, and the reason is worth stating
//! because it is what makes this module small.
//!
//! An event stream is an *ordered* stream. Order means sequence numbers, and
//! sequence numbers mean a client that reconnects must say where it got to and
//! a server must be able to replay from there -- a backlog, a retention policy,
//! and a whole class of bug where the conversation quietly renders a transcript
//! missing the three entries that arrived while the socket was down. A
//! conversation that has silently lost a line is worse than one that polls,
//! because it looks correct.
//!
//! Publishing the whole plan dissolves all of that. There is no sequence, so
//! nothing can be missed; a reconnecting client gets current truth as its first
//! message and is immediately correct again. Replay is not implemented because
//! there is nothing to replay.
//!
//! What that costs is bandwidth: a few kilobytes of JSON per change, over
//! loopback, for one user. That is not a real cost. Should a plan's transcript
//! ever grow large enough that it is, the fix is to trim what a plan carries on
//! the wire -- not to reintroduce ordering.
//!
//! # Why this hangs off `api::update`
//!
//! [`crate::api::update`] is already the single funnel for plan mutations,
//! which is why persistence hangs off it: a caller cannot change a plan and
//! forget to write it. Publishing belongs there for exactly the same reason. An
//! event bus that each caller has to remember to call is one that will be
//! forgotten, and the symptom -- a conversation frozen mid-turn -- would be
//! blamed on the socket rather than on the caller that stayed silent.

//! # Why a subagent reaches two channels
//!
//! A subagent is published on its own channel *and* on the channel of the plan
//! that sent it. That is the whole of the live-subagent-status feature, and it
//! is four lines, because the decision above pays off a second time: the wire
//! carries whole plans, and [`kingdom_core::Kingdom::insert`] files a plan it
//! has not seen before rather than dropping it. So a conversation watching a
//! parent accumulates that parent's subagents as they work, with no new message
//! type, no second socket, and nothing to keep in step.
//!
//! An event stream would have needed a new event, a client that knew how to
//! apply it, and a decision about ordering between the two channels.

use kingdom_core::{Plan, PlanId};
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use tokio::sync::broadcast;

/// How many proclamations a slow listener may fall behind before it is dropped
/// from the channel.
///
/// Falling behind is survivable precisely because each message is a whole plan:
/// a listener that misses messages has missed nothing but intermediate states,
/// and the next one it receives is complete. The watcher treats a lag as a
/// non-event for that reason.
const BACKLOG: usize = 16;

/// One broadcast channel per plan being watched.
///
/// Keyed by plan because that is the unit the conversation subscribes to. A
/// single kingdom-wide channel would wake every open tab for every keystroke of
/// every plan, which is the sort of thing that is free with one plan and
/// embarrassing with thirty.
static CHANNELS: OnceLock<Mutex<HashMap<PlanId, broadcast::Sender<Plan>>>> = OnceLock::new();

fn registry() -> &'static Mutex<HashMap<PlanId, broadcast::Sender<Plan>>> {
    CHANNELS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Announces a plan's current state to everyone watching it.
///
/// Silent when nobody is listening, which is the common case: a plan drafted
/// with no conversation open should cost nothing. Deliberately infallible -- a
/// failure to publish must never fail the user's prompt, because the work
/// itself succeeded and the records on disk are already correct. The worst case
/// is a conversation that is briefly stale, and the next proclamation repairs
/// it.
///
/// A subagent is announced twice: once to its own conversation, and once to the
/// conversation of the plan that sent it, which is where the user watches it
/// work. See the module docs for why that costs nothing.
///
/// What goes out is [`Plan::for_wire`] rather than the plan itself: this
/// channel's only subscribers are watch sockets, and a browser has no use for
/// the provider-opaque half of the model's thinking. The plan the *server*
/// holds is untouched.
pub fn publish(plan: &Plan) {
    let Ok(channels) = registry().lock() else {
        return;
    };

    // Everything on this channel is bound for a browser, so the opaque half of
    // the model's thinking is dropped once here rather than at each `send`.
    // See `Plan::for_wire`: it is never drawn, and it was the largest thing on
    // the wire by a wide margin.
    //
    // Built lazily, because the common case is that nobody is listening and the
    // whole point of that case is that it costs nothing.
    let mut for_wire: Option<Plan> = None;

    if let Some(tx) = channels.get(&plan.id) {
        let _ = tx.send(for_wire.get_or_insert_with(|| plan.for_wire()).clone());
    }

    // The second channel is the *sender's*, not a broadcast: only the plan that
    // sent this subagent hears about it.
    if let Some(subagent) = &plan.spawned_by {
        if let Some(tx) = channels.get(&subagent.parent) {
            let _ = tx.send(for_wire.get_or_insert_with(|| plan.for_wire()).clone());
        }
    }
}

/// Starts watching one plan.
///
/// The channel is created on first listen rather than when a plan is opened, so
/// plans nobody is watching hold no machinery at all.
pub fn subscribe(id: &PlanId) -> broadcast::Receiver<Plan> {
    let mut channels = match registry().lock() {
        Ok(c) => c,
        // A poisoned registry means a previous holder panicked mid-update. The
        // user's conversation should not go dark over it: hand back a lone
        // receiver that will simply never fire, and let the socket's opening
        // snapshot still render.
        Err(poisoned) => poisoned.into_inner(),
    };

    match channels.get(id) {
        Some(tx) => tx.subscribe(),
        None => {
            let (tx, rx) = broadcast::channel(BACKLOG);
            channels.insert(id.clone(), tx);
            rx
        }
    }
}

/// Forgets the channel for a plan once nothing is watching it.
///
/// Without this the map grows by one entry per plan ever opened in a
/// conversation, for the life of the process. Called when a watcher
/// disconnects; the check is racy by nature, so it only removes a channel that
/// currently has no receivers, and a listener arriving in that instant simply
/// makes a new one.
pub fn forget_if_unwatched(id: &PlanId) {
    let Ok(mut channels) = registry().lock() else {
        return;
    };
    if let Some(tx) = channels.get(id) {
        if tx.receiver_count() == 0 {
            channels.remove(id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kingdom_core::{ModelChoice, Workspace};

    fn a_plan(id: &str) -> Plan {
        Plan::opened(
            PlanId::new(id),
            kingdom_core::CityId::new("city"),
            "do the thing",
            &ModelChoice::new("mock", None),
            Workspace::in_place("/tmp/city"),
        )
    }

    /// The whole point of the module: a change made through the funnel reaches
    /// a conversation that is watching, without the conversation having asked.
    #[tokio::test]
    async fn a_watcher_hears_what_it_is_watching_and_nothing_else() {
        let watched = PlanId::new("watched");
        let mut rx = subscribe(&watched);

        publish(&a_plan("elsewhere"));
        publish(&a_plan("watched"));

        let heard = rx.recv().await.expect("the watched plan should arrive");
        assert_eq!(
            heard.id, watched,
            "a watcher must not be woken by another plan's changes"
        );
    }

    /// What crosses the socket is stripped of what a browser cannot draw.
    ///
    /// The size half of the push design, pinned at the place that performs it.
    /// `events.rs` deliberately publishes *whole plans* -- that is what makes
    /// reconnection free -- and the cost of that decision is paid on every
    /// round of every turn. Most of those bytes were the model's opaque
    /// thinking, which nothing in the chamber has ever rendered.
    ///
    /// Worth a test rather than a comment because the receiving side cannot
    /// tell: a conversation absorbs whatever arrives, so a regression here
    /// would show up only as the King's browser going slow again, with nothing
    /// in the UI's own code to blame.
    #[tokio::test]
    async fn a_watcher_is_not_sent_the_thinking_it_cannot_read() {
        // Its own id, deliberately. The registry is process-global and every
        // test in this module shares it, so a plan id used by two tests lets
        // one test's `publish` land in the other's receiver -- which is exactly
        // the flake that reuse of "watched" produced here.
        let id = PlanId::new("watched-thinking");
        let mut rx = subscribe(&id);

        let mut thinking = kingdom_core::Reasoning {
            text: Some("checking how the rail reads a title".to_string()),
            ..Default::default()
        };
        thinking.opaque.insert(
            "signature".to_string(),
            serde_json::json!("c2lnbmVkLXRoaW5raW5n"),
        );

        let mut plan = a_plan("watched-thinking");
        plan.begin_tool_call(
            kingdom_core::ToolCall::started("call-1", "bash", serde_json::json!({})).in_reply(
                "reply-1",
                Some(thinking),
                None,
            ),
        );

        publish(&plan);

        let heard = rx.recv().await.expect("the watched plan should arrive");
        let Some(kingdom_core::Entry::Tool(deed)) = heard.transcript.last() else {
            panic!("the deed must survive the crossing");
        };
        let carried = deed.reasoning.as_ref().expect("the thinking still rides");

        assert_eq!(
            carried.text.as_deref(),
            Some("checking how the rail reads a title"),
            "the chamber folds the prose away behind a chevron, so it must cross"
        );
        assert!(
            carried.opaque.is_empty(),
            "the provider's signature is never drawn and must not be sent"
        );

        // And publishing did not reach back and blind the caller's own plan,
        // which is the copy the model is replayed from.
        let Some(kingdom_core::Entry::Tool(original)) = plan.transcript.last() else {
            panic!("the caller still holds its deed");
        };
        assert!(
            original
                .reasoning
                .as_ref()
                .is_some_and(|r| r.opaque.contains_key("signature")),
            "publishing must not strip the plan the server keeps -- see Reasoning's docs"
        );
    }

    /// A plan nobody watches must not accumulate machinery, or the registry
    /// grows by one entry per plan ever opened for the life of the process.
    #[tokio::test]
    async fn a_plan_nobody_watches_costs_nothing() {
        let id = PlanId::new("transient");
        let rx = subscribe(&id);
        drop(rx);
        forget_if_unwatched(&id);

        assert!(
            !registry().lock().unwrap().contains_key(&id),
            "the last watcher leaving must take the channel with it"
        );
    }

    /// The whole of live subagent status in the parent's conversation: a
    /// conversation watching a plan is told about the subagents that plan sent,
    /// without subscribing to each one.
    ///
    /// Worth pinning because the *receiving* side has no idea this is
    /// happening -- it just absorbs a plan -- so a regression here would show
    /// up as subagent rows that never leave "working", with nothing in the
    /// conversation's own code to blame.
    #[tokio::test]
    async fn a_parent_hears_its_subagents_and_no_others() {
        let parent = a_plan("parent");
        let mut watching_parent = subscribe(&parent.id);

        // Somebody else's subagent first: if the second channel were a
        // broadcast rather than the sender's own parent, this is what would
        // leak.
        publish(&Plan::spawned(
            PlanId::new("stranger"),
            &a_plan("elsewhere"),
            "call-1",
            "Not ours",
        ));
        publish(&Plan::spawned(
            PlanId::new("errand"),
            &parent,
            "call-1",
            "Go and look",
        ));

        let heard = watching_parent
            .recv()
            .await
            .expect("the parent's chamber should hear about its own errand");
        assert_eq!(
            heard.id,
            PlanId::new("errand"),
            "a chamber must hear the errands its own plan sent, and only those"
        );
    }
}
