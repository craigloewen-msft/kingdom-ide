//! The `/watch/plan/{id}` socket: one conversation, watching one plan.
//!
//! The transport. [`crate::events`] decides *what* is published and why;
//! this decides how it reaches a browser.
//!
//! Deliberately one-way. The user's half of the conversation still goes through
//! `#[server]` functions over ordinary HTTP, because those are typed end to end
//! and a socket message is not -- hand-rolling a request/response protocol over
//! this socket would throw away the main reason this project is Rust on both
//! ends. The socket exists for what HTTP cannot do: let the server speak first.
//!
//! # What is where
//!
//! The two [`ROUTE`] constants compile into **both** targets and the handlers
//! into the server only, for the reason `artifact.rs` does the same: the browser
//! is what builds the address and the server is what answers it, and two copies
//! of a path string is how those come to disagree. The failure mode is a socket
//! that silently never connects.

#[cfg(feature = "ssr")]
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
#[cfg(feature = "ssr")]
use axum::extract::Path;
#[cfg(feature = "ssr")]
use axum::response::Response;
#[cfg(feature = "ssr")]
use kingdom_core::PlanId;
#[cfg(feature = "ssr")]
use tokio::sync::broadcast::error::RecvError;

/// The path the conversation connects to. One plan per socket.
pub const ROUTE: &str = "/watch/plan/{id}";

/// The path the rail connects to. One socket, every plan.
///
/// The counterpart to [`ROUTE`] and deliberately not keyed by anything: its job
/// is to carry news of plans whose chambers are *closed*, which is precisely
/// what a per-plan socket cannot do. See [`crate::events`] for why this one
/// carries a digest rather than a plan.
pub const KINGDOM_ROUTE: &str = "/watch/kingdom";

#[cfg(feature = "ssr")]
pub async fn upgrade(ws: WebSocketUpgrade, Path(id): Path<String>) -> Response {
    ws.on_upgrade(move |socket| watch(socket, PlanId::new(id)))
}

#[cfg(feature = "ssr")]
async fn watch(mut socket: WebSocket, id: PlanId) {
    // Subscribed *before* the opening snapshot is read, so a change landing
    // between the two is queued rather than missed. The other order leaves a
    // window in which the conversation renders a stale plan and is never told
    // otherwise -- rare, silent, and exactly the failure a socket is supposed
    // to remove.
    let mut proclamations = crate::events::subscribe(&id);

    // The opening snapshot is what makes reconnection free: a conversation that
    // has been offline is handed current truth as its first message, with
    // nothing to replay and no sequence to reconcile.
    //
    // `for_wire` for the same reason every proclamation is: this is the largest
    // message the socket ever sends -- a whole plan, at whatever length its
    // transcript has reached -- and the opaque half of the model's thinking is
    // no part of what the chamber draws.
    if let Some(plan) = crate::api::snapshot(&id) {
        if send(&mut socket, &plan.for_wire()).await.is_err() {
            crate::events::forget_if_unwatched(&id);
            return;
        }
    }

    loop {
        tokio::select! {
            heard = proclamations.recv() => match heard {
                Ok(plan) => {
                    if send(&mut socket, &plan).await.is_err() {
                        break;
                    }
                }
                // Fell behind. Harmless here, because every message is a whole
                // plan: what was missed is intermediate states, and the next
                // proclamation is complete on its own. Nothing to recover.
                Err(RecvError::Lagged(_)) => continue,
                Err(RecvError::Closed) => break,
            },

            // Nothing is expected from the browser, but the read half must
            // still be driven: it is what delivers Close and keeps ping/pong
            // alive. Without this arm a conversation the user closed would
            // leave its task parked on `recv()` until the next proclamation
            // happened to fail on write.
            from_browser = socket.recv() => match from_browser {
                Some(Ok(_)) => continue,
                _ => break,
            },
        }
    }

    crate::events::forget_if_unwatched(&id);
}

#[cfg(feature = "ssr")]
async fn send(socket: &mut WebSocket, plan: &kingdom_core::Plan) -> Result<(), ()> {
    // A plan that will not serialise is a bug in the domain type, not something
    // the user can act on, so it closes this socket rather than poisoning the
    // stream with a message the conversation cannot parse.
    let Ok(json) = serde_json::to_string(plan) else {
        return Err(());
    };
    socket
        .send(Message::Text(json.into()))
        .await
        .map_err(|_| ())
}

/// The rail's socket: every plan in the kingdom, as little as will do.
///
/// Shaped exactly like [`watch`] above and for the same reasons -- subscribe
/// before the snapshot, drive the read half, whole messages with nothing to
/// reconcile -- with one difference worth stating. **Every message is a list**,
/// including the single-plan updates. There is therefore no message kind to tag
/// and no branch on the receiving side: the opening snapshot and an update are
/// the same shape, and a rail that reconnects simply receives a longer one.
#[cfg(feature = "ssr")]
pub async fn upgrade_kingdom(ws: WebSocketUpgrade) -> Response {
    ws.on_upgrade(watch_kingdom)
}

#[cfg(feature = "ssr")]
async fn watch_kingdom(mut socket: WebSocket) {
    // Before the snapshot, so a plan that moves between the two is queued
    // rather than missed. Same window, same reasoning as `watch`.
    let mut pulses = crate::events::subscribe_to_pulses();

    // What makes reconnection free here too: the rail is handed every plan's
    // state as its first message and is immediately correct, with nothing to
    // replay.
    let opening: Vec<kingdom_core::PlanPulse> = crate::api::kingdom_snapshot()
        .map(|kingdom| {
            kingdom
                .plans
                .iter()
                .filter(|p| !p.is_subagent())
                .map(kingdom_core::Plan::pulse)
                .collect()
        })
        .unwrap_or_default();

    if send_pulses(&mut socket, &opening).await.is_err() {
        return;
    }

    loop {
        tokio::select! {
            heard = pulses.recv() => match heard {
                Ok(pulse) => {
                    if send_pulses(&mut socket, std::slice::from_ref(&pulse)).await.is_err() {
                        break;
                    }
                }
                // Fell behind. Harmless for the same reason it is on the plan
                // socket: a pulse is a whole digest, so what was missed is
                // intermediate states and the next one is complete on its own.
                Err(RecvError::Lagged(_)) => continue,
                Err(RecvError::Closed) => break,
            },

            // Nothing is expected from the browser, but the read half still has
            // to be driven -- it is what delivers Close. See `watch`.
            from_browser = socket.recv() => match from_browser {
                Some(Ok(_)) => continue,
                _ => break,
            },
        }
    }
}

#[cfg(feature = "ssr")]
async fn send_pulses(socket: &mut WebSocket, pulses: &[kingdom_core::PlanPulse]) -> Result<(), ()> {
    let Ok(json) = serde_json::to_string(pulses) else {
        return Err(());
    };
    socket
        .send(Message::Text(json.into()))
        .await
        .map_err(|_| ())
}
