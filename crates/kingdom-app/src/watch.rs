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

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::Path;
use axum::response::Response;
use kingdom_core::PlanId;
use tokio::sync::broadcast::error::RecvError;

/// The path the conversation connects to. One plan per socket.
pub const ROUTE: &str = "/watch/plan/{id}";

pub async fn upgrade(ws: WebSocketUpgrade, Path(id): Path<String>) -> Response {
    ws.on_upgrade(move |socket| watch(socket, PlanId::new(id)))
}

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
    if let Some(plan) = crate::api::snapshot(&id) {
        if send(&mut socket, &plan).await.is_err() {
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

async fn send(socket: &mut WebSocket, plan: &kingdom_core::Plan) -> Result<(), ()> {
    // A plan that will not serialise is a bug in the domain type, not something
    // the user can act on, so it closes this socket rather than poisoning the
    // stream with a message the conversation cannot parse.
    let Ok(json) = serde_json::to_string(plan) else {
        return Err(());
    };
    socket.send(Message::Text(json.into())).await.map_err(|_| ())
}
