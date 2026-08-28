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
//!
//! # Why the kingdom-wide channel does NOT carry whole plans
//!
//! Everything above justifies whole plans on a channel keyed *per plan*, where
//! there is one watcher looking at exactly the plan being sent. The rail asks a
//! different question -- "which of my thirty plans needs me?" -- and the same
//! answer would be wrong for it: every open tab woken with every plan's entire
//! transcript, on every round of every turn, to repaint a badge.
//!
//! So that channel carries [`kingdom_core::PlanPulse`], and it is **deduped**:
//! a pulse goes out only when it differs from the last one sent for that plan.
//! Both halves are load-bearing. The digest is what makes a message cheap; the
//! dedupe is what makes most rounds send nothing at all, because a sixty-deed
//! turn changes `working_on` a handful of times and what the King is wanted for
//! twice.
//!
//! The dedupe is also what keeps this honest against the design above. A plan
//! channel is complete on every message and so may be missed freely; a pulse
//! channel is *also* complete on every message -- a pulse is a whole digest, not
//! a delta -- so a listener that falls behind has still missed nothing but
//! intermediate states. Dedupe narrows what is sent, never what a message says.

use kingdom_core::{Plan, PlanId, PlanPulse};
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

/// How far behind a rail may fall on the kingdom-wide channel.
///
/// Larger than [`BACKLOG`] because one channel carries every plan in the
/// kingdom: thirty plans working at once share this where each has its own
/// per-plan channel. Falling behind is survivable on the same terms -- a pulse
/// is a whole digest, so the next one is complete on its own.
const PULSE_BACKLOG: usize = 64;

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

/// The one channel every open browser listens to, whatever it is looking at.
///
/// Unlike [`CHANNELS`] this is not keyed by plan: its whole job is to tell a
/// rail about plans whose chambers are *not* open, which is the case the
/// per-plan channel structurally cannot cover. What it costs to have one shared
/// channel -- waking every tab for every plan -- is what the digest and the
/// dedupe below pay for.
static PULSES: OnceLock<broadcast::Sender<PlanPulse>> = OnceLock::new();

fn pulses_channel() -> &'static broadcast::Sender<PlanPulse> {
    PULSES.get_or_init(|| broadcast::channel(PULSE_BACKLOG).0)
}

/// The last pulse sent for each plan, so an unchanged one is not sent again.
///
/// This is the dedupe, and it is what makes a shared channel affordable: a turn
/// publishes on every deed, and the rail's view of a plan changes a handful of
/// times across the whole turn. Without it, every tab would parse thirty
/// identical messages saying the plan is still drafting.
///
/// Never grows without bound in practice -- one entry per plan in the kingdom --
/// and is cleared with the channel it belongs to when a kingdom is closed.
static LAST_PULSE: OnceLock<Mutex<HashMap<PlanId, PlanPulse>>> = OnceLock::new();

fn last_pulse() -> &'static Mutex<HashMap<PlanId, PlanPulse>> {
    LAST_PULSE.get_or_init(|| Mutex::new(HashMap::new()))
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
///
/// The rail is told separately, and *unconditionally*: see [`pulse`]. The two
/// halves are independent on purpose, so a poisoned per-plan registry silences
/// one chamber rather than every badge in the app.
pub fn publish(plan: &Plan) {
    // Resolved here, where no lock is held, rather than deep inside
    // `on_the_wire`: see [`publish_within`] for why that distinction is
    // load-bearing.
    let city_root = crate::api::city_root_of(&plan.id);
    publish_within(plan, city_root.as_deref());
}

/// [`publish`] for a caller that is already holding the kingdom.
///
/// # Why the city is passed in
///
/// Attaching a plan's shared services means knowing which city it belongs to,
/// and that answer lives in the kingdom -- behind a plain, **non-reentrant**
/// [`std::sync::Mutex`]. [`crate::api::update`] publishes with that guard in
/// hand, so a lookup from in here would be the same thread asking for the same
/// lock twice: it deadlocks, and because it deadlocks *holding* the lock, every
/// later request in the process hangs behind it. The server answers once and
/// then spins forever.
///
/// So the city is resolved by whoever already has the kingdom open and handed
/// down. `None` is a plan whose city could not be resolved -- it simply carries
/// no services, the same as a project that declares none.
pub fn publish_within(plan: &Plan, city_root: Option<&std::path::Path>) {
    if let Ok(channels) = registry().lock() {
        // Everything on this channel is bound for a browser, so the opaque half
        // of the model's thinking is dropped once here rather than at each
        // `send`. See `Plan::for_wire`: it is never drawn, and it was the
        // largest thing on the wire by a wide margin.
        //
        // Built lazily, because the common case is that nobody is listening and
        // the whole point of that case is that it costs nothing.
        let mut for_wire: Option<Plan> = None;

        if let Some(tx) = channels.get(&plan.id) {
            let _ = tx.send(
                for_wire
                    .get_or_insert_with(|| on_the_wire(plan, city_root))
                    .clone(),
            );
        }

        // The second channel is the *sender's*, not a broadcast: only the plan
        // that sent this subagent hears about it.
        if let Some(subagent) = &plan.spawned_by {
            if let Some(tx) = channels.get(&subagent.parent) {
                let _ = tx.send(
                    for_wire
                        .get_or_insert_with(|| on_the_wire(plan, city_root))
                        .clone(),
                );
            }
        }
    }

    pulse(plan);
}

/// A plan as a browser should receive it, with the given runtime facts
/// attached.
///
/// [`Plan::for_wire`] trims what the browser does not need; this adds the two
/// things it needs that the *record* does not have -- ports and shared-service
/// addresses. Both belong to a running process (a slirp4netns, a Docker
/// daemon) rather than to the plan, so neither is stored: a forward or an
/// address written to disk would name something that stopped answering when
/// the server did.
///
/// Pure and cheap on purpose: this is the seam every route to a browser must
/// cross. `Plan::for_wire` alone is never enough -- a plan handed to a browser
/// without going through here is a plan that will render an empty ports badge
/// the instant nothing else happens to publish it. [`on_the_wire`] is the
/// version that looks the facts up; this is the version a test can call
/// without a namespace or a Docker daemon.
pub(crate) fn fitted(
    plan: &Plan,
    ports: Vec<kingdom_core::PortForward>,
    services: Vec<kingdom_core::SharedService>,
) -> Plan {
    let mut wire = plan.for_wire();
    if plan.network.is_isolated() {
        wire.ports = ports;
    }
    // Unconditional, unlike ports: a shared service is used by every plan in
    // the city whether or not this one has a network of its own, and the King
    // wants the address either way. Empty for the overwhelming majority of
    // projects, which declare no services at all.
    wire.shared_services = services;
    wire
}

/// [`fitted`], with the facts looked up from the namespace and the daemon.
///
/// The city is given rather than looked up: see [`publish_within`].
pub(crate) fn on_the_wire(plan: &Plan, city_root: Option<&std::path::Path>) -> Plan {
    let ports = if plan.network.is_isolated() {
        crate::netns::forwards_of(&plan.id)
            .into_iter()
            .map(|(guest, host)| kingdom_core::PortForward { guest, host })
            .collect()
    } else {
        Vec::new()
    };
    let services = city_root
        .map(|city_root| {
            crate::services::running_in(city_root)
                .into_iter()
                .map(|service| {
                    // Which file this was declared in, which is what the badge
                    // links the King to. Derived from the scope the service
                    // carries: a project's sits in that project, the King's own
                    // in his profile.
                    let scope = match service.scope {
                        kingdom_core::ServiceScope::Host => crate::services::Scope::Host,
                        kingdom_core::ServiceScope::City => {
                            crate::services::Scope::City(city_root.to_path_buf())
                        }
                    };
                    kingdom_core::SharedService {
                        // The address **this plan** reaches it at, not the
                        // container's: an isolated plan has it on its own
                        // loopback, and the badge must say what the agent
                        // beside it is actually typing. See
                        // `services::address_for`.
                        address: crate::services::address_for(&plan.id, &service),
                        // By the service's own registry key, not by city root:
                        // a host well is filed under `host`, so asking for it
                        // by city would report a database five agents share as
                        // used by nobody.
                        users: crate::services::users_of_key(&service.key, &service.name),
                        manifest_path: scope.manifest_path().to_string_lossy().to_string(),
                        scope: service.scope,
                        name: service.name,
                        image: service.image,
                    }
                })
                .collect()
        })
        .unwrap_or_default();
    fitted(plan, ports, services)
}

/// [`on_the_wire`] for a caller outside the publish path that holds no lock --
/// a snapshot for an opening watch socket, say.
///
/// Resolves the plan and its city itself, exactly as [`publish`] does, so a
/// caller need only name the plan.
pub(crate) fn for_browser(id: &PlanId) -> Option<Plan> {
    let plan = crate::api::snapshot(id)?;
    let city_root = crate::api::city_root_of(id);
    Some(on_the_wire(&plan, city_root.as_deref()))
}

/// Tells every open rail what this plan is and what it wants, if that changed.
///
/// Split out of [`publish`] so the one caller that does not go through
/// `api::update` -- a plan being opened, which is pushed onto the kingdom
/// directly -- can announce itself with the same words.
///
/// Silent when nothing a rail draws has moved. See the module docs: the dedupe
/// is what makes a channel shared by every tab affordable, and it is safe
/// precisely because a pulse is a whole digest rather than a delta.
pub fn pulse(plan: &Plan) {
    let pulse = plan.pulse();

    // A subagent is never in the rail and never asks the user for anything, so
    // a pulse for one is a message every tab would parse and discard.
    if pulse.is_subagent {
        return;
    }

    // A poisoned dedupe must not silence the rail: send it anyway. A duplicate
    // message costs a repaint; a swallowed one costs the King a badge.
    if let Ok(mut seen) = last_pulse().lock() {
        if seen.get(&pulse.id) == Some(&pulse) {
            return;
        }
        seen.insert(pulse.id.clone(), pulse.clone());
    }

    let _ = pulses_channel().send(pulse);
}

/// Starts listening to every plan in the kingdom.
///
/// The rail's counterpart to [`subscribe`]. There is no id because there is
/// nothing to name: this is the whole kingdom, which is the entire reason it
/// exists.
pub fn subscribe_to_pulses() -> broadcast::Receiver<PlanPulse> {
    pulses_channel().subscribe()
}

/// Forgets what has been sent about a plan.
///
/// Called when a kingdom is closed, so that reopening one announces its plans
/// afresh rather than deduping them against a previous kingdom's state.
pub fn forget_pulses() {
    if let Ok(mut seen) = last_pulse().lock() {
        seen.clear();
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
            kingdom_core::NetworkMode::Shared,
        )
    }

    /// Publishing must never reach for the kingdom lock.
    ///
    /// [`crate::api::update`] publishes with the kingdom's guard in hand, and
    /// that mutex is a plain [`std::sync::Mutex`] -- not reentrant. A lookup
    /// from inside the publish path is therefore the same thread asking for the
    /// same lock twice, which deadlocks *while holding it*: the server answers
    /// one request and then every later one hangs behind a lock nobody will ever
    /// release. That is exactly what attaching shared services on the wire once
    /// did, by resolving the plan's city through `api::city_root_of`.
    ///
    /// A subscriber is registered first because it is load-bearing: `on_the_wire`
    /// is built lazily and only runs when somebody is actually listening, which
    /// is why the fault appeared only once a chamber was open.
    ///
    /// Run on its own thread with a deadline, so a reintroduced deadlock is a
    /// failing test rather than a suite that never finishes.
    #[test]
    fn publishing_while_the_kingdom_is_held_does_not_deadlock() {
        let plan = a_plan("held");
        let listening = subscribe(&plan.id);

        let (done, finished) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _kingdom = crate::api::lock().expect("the kingdom locks");
            publish_within(&plan, None);
            let _ = done.send(());
        });

        assert!(
            finished
                .recv_timeout(std::time::Duration::from_secs(10))
                .is_ok(),
            "publishing under the kingdom lock must return -- a second lock from \
             inside the publish path deadlocks the whole server"
        );

        drop(listening);
    }

    /// `fitted` attaches the runtime facts to a plan whose record carries
    /// none -- the state every server-held plan is in -- without touching a
    /// namespace or a Docker daemon. This is the whole reason `on_the_wire`
    /// was split: the wiring can be exercised without either running.
    #[test]
    fn fitted_attaches_ports_and_services_to_a_bare_record() {
        let mut plan = a_plan("fitted");
        plan.network = kingdom_core::NetworkMode::Isolated;
        assert!(plan.ports.is_empty(), "the record itself must carry none");

        let ports = vec![kingdom_core::PortForward {
            guest: 3000,
            host: 41000,
        }];
        let services = vec![kingdom_core::SharedService {
            name: "db".into(),
            image: "mongo:7".into(),
            address: "127.0.0.1:27017".into(),
            users: 1,
            scope: kingdom_core::ServiceScope::City,
            manifest_path: "/dev/shopfront/.kingdom/services.toml".into(),
        }];

        let wire = fitted(&plan, ports.clone(), services.clone());
        assert_eq!(wire.ports, ports, "the fitted copy must carry the forward");
        assert_eq!(
            wire.shared_services, services,
            "the fitted copy must carry the service"
        );

        // The caller's own plan is untouched -- `fitted` reads a plan, it does
        // not mutate one.
        assert!(
            plan.ports.is_empty() && plan.shared_services.is_empty(),
            "fitted must not reach back and mutate the caller's plan"
        );
    }

    /// A shared plan (no network of its own) gets no ports even if some are
    /// passed in -- the field is gated on `network.is_isolated()`, matching
    /// `PortsBadge`'s own gate, so a caller cannot accidentally show a shared
    /// plan ports it has no way to have bound.
    #[test]
    fn fitted_never_shows_ports_for_a_shared_plan() {
        let plan = a_plan("shared-fitted");
        assert!(!plan.network.is_isolated());

        let ports = vec![kingdom_core::PortForward {
            guest: 3000,
            host: 41000,
        }];
        let wire = fitted(&plan, ports, vec![]);
        assert!(
            wire.ports.is_empty(),
            "a plan on the shared network must never show ports it cannot have"
        );
    }

    /// `fitted` still strips the opaque half of the model's thinking -- the
    /// size win `for_wire` exists for must survive the split.
    #[test]
    fn fitted_still_strips_opaque_thinking() {
        let mut thinking = kingdom_core::Reasoning {
            text: Some("thinking".to_string()),
            ..Default::default()
        };
        thinking
            .opaque
            .insert("signature".to_string(), serde_json::json!("opaque"));

        let mut plan = a_plan("fitted-thinking");
        plan.begin_tool_call(
            kingdom_core::ToolCall::started("call-1", "bash", serde_json::json!({})).in_reply(
                "reply-1",
                Some(thinking),
                None,
            ),
        );

        let wire = fitted(&plan, vec![], vec![]);
        let Some(kingdom_core::Entry::Tool(deed)) = wire.transcript.last() else {
            panic!("the deed must survive");
        };
        assert!(
            deed.reasoning.as_ref().is_some_and(|r| r.opaque.is_empty()),
            "fitted must still strip what for_wire strips"
        );
    }

    /// A shared service is never persisted, only attached on the way out.
    ///
    /// The same rule `Plan::ports` follows, and it matters for the same reason:
    /// an address belongs to a running Docker daemon, so one restored from a
    /// record would name a container that stopped when the server did. This
    /// pins the *record* side -- a plan as the server holds it carries none --
    /// which is what `on_the_wire` then fills in.
    #[test]
    fn a_plan_record_carries_no_shared_services() {
        let plan = a_plan("kept");
        assert!(
            plan.shared_services.is_empty(),
            "a plan record must not carry an address that outlives the daemon"
        );

        // And it survives a round trip through the store's format without
        // acquiring one, because the field is `#[serde(default)]`.
        let json = serde_json::to_string(&plan).expect("a plan serialises");
        let back: Plan = serde_json::from_str(&json).expect("and comes back");
        assert!(back.shared_services.is_empty());

        // A record written before this field existed still loads.
        let old = json.replace(",\"shared_services\":[]", "");
        let ancient: Plan = serde_json::from_str(&old).expect("an older record still loads");
        assert!(ancient.shared_services.is_empty());
    }

    /// A shared service sent before there were two levels still reads, as a
    /// project's own.
    ///
    /// `scope` and `manifest_path` arrived with the host level. Both default,
    /// and the default has to be `City`: a chamber that fell back to "the whole
    /// machine" would tell the King a project's database was shared with every
    /// other project he has open, which is the one thing about a well he most
    /// needs to be right.
    #[test]
    fn a_shared_service_from_before_the_two_levels_reads_as_a_projects_own() {
        let older = r#"{
            "name": "db",
            "image": "mongo:7",
            "address": "172.31.44.10:27017",
            "users": 3
        }"#;

        let service: kingdom_core::SharedService =
            serde_json::from_str(older).expect("an older wire form still loads");

        assert_eq!(service.scope, kingdom_core::ServiceScope::City);
        assert!(service.manifest_path.is_empty());
        assert_eq!(service.users, 3);
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

    /// Helper: the next pulse about a particular plan, ignoring the rest.
    ///
    /// The pulse channel is process-global and shared by every test in this
    /// module, so a receiver hears other tests' plans too. Filtering by id is
    /// what makes these tests independent of each other -- the same flake the
    /// per-plan tests avoid by never sharing an id.
    async fn pulse_about(rx: &mut broadcast::Receiver<PlanPulse>, id: &PlanId) -> PlanPulse {
        loop {
            let heard = tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv())
                .await
                .expect("a pulse should arrive well within the timeout")
                .expect("the pulse channel should stay open");
            if &heard.id == id {
                return heard;
            }
        }
    }

    /// The fault this channel exists for, end to end: a plan parks on a
    /// question and *every* browser is told, whether or not that plan's chamber
    /// is open. The per-plan channel structurally cannot do this -- there is no
    /// subscriber for a chamber nobody opened -- which is why a second channel
    /// had to exist at all.
    #[tokio::test]
    async fn a_question_reaches_a_rail_that_is_watching_nothing_in_particular() {
        let id = PlanId::new("pulse-asking");
        let mut rail = subscribe_to_pulses();

        let mut plan = a_plan("pulse-asking");
        plan.working_on = Some("Waiting on the King".into());
        plan.begin_tool_call(kingdom_core::ToolCall::started(
            "q1",
            kingdom_core::ASK_USER_QUESTION,
            serde_json::json!({}),
        ));

        // Nobody is subscribed to this plan's own channel: the chamber is shut.
        publish(&plan);

        let heard = pulse_about(&mut rail, &id).await;
        assert_eq!(heard.needs, Some(kingdom_core::Attention::Question));
        assert_eq!(heard.status, kingdom_core::PlanStatus::Drafting);
        assert_eq!(
            heard.working_on.as_deref(),
            Some("Waiting on the King"),
            "the rail is told what the plan is doing, in the server's own words"
        );
    }

    /// The dedupe, which is what makes one channel shared by every tab
    /// affordable. A turn publishes on every deed; the rail's view of the plan
    /// changes a handful of times in the whole turn.
    ///
    /// Worth pinning because the failure is invisible: nothing renders wrong,
    /// the King's browser just parses thirty identical messages per turn and
    /// gets slower with every plan he opens.
    #[tokio::test]
    async fn an_unchanged_plan_is_not_announced_twice() {
        let id = PlanId::new("pulse-repeat");
        let mut rail = subscribe_to_pulses();

        let plan = a_plan("pulse-repeat");
        publish(&plan);
        publish(&plan);
        publish(&plan);

        // Something genuinely different, so there is a second message to find
        // and the test cannot pass by the channel simply being slow.
        let mut moved = plan.clone();
        moved.working_on = Some("bash: cargo test".into());
        publish(&moved);

        let first = pulse_about(&mut rail, &id).await;
        assert_eq!(first.working_on, None);

        let second = pulse_about(&mut rail, &id).await;
        assert_eq!(
            second.working_on.as_deref(),
            Some("bash: cargo test"),
            "the three identical publishes must not have produced three messages"
        );
    }

    /// A subagent never reaches the rail. It is excluded from the rail's list
    /// and never asks the user for anything, so a pulse for one is a message
    /// every open tab would parse and throw away.
    #[tokio::test]
    async fn the_rail_is_never_told_about_an_errand() {
        let parent = a_plan("pulse-parent");
        let mut rail = subscribe_to_pulses();

        publish(&Plan::spawned(
            PlanId::new("pulse-errand"),
            &parent,
            "call-1",
            "Go and look",
        ));
        publish(&parent);

        let heard = pulse_about(&mut rail, &parent.id).await;
        assert_eq!(
            heard.id, parent.id,
            "the errand published first, so hearing the parent first means it was never sent"
        );
    }
}
