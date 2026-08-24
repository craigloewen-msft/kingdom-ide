//! The King's spyglass: `/watch/plan/{id}/browser`, a live view of a court's page.
//!
//! The chamber's [`crate::watch`] socket carries *plans*; this one carries
//! pixels. They are siblings rather than one socket with a mode, because a
//! transcript entry and a video frame have nothing in common but a direction.
//!
//! # The wire
//!
//! Binary frames, tagged by their first byte. Deliberately trivial, so this
//! module and `components/spyglass.rs` can be read side by side and checked
//! against each other by eye:
//!
//! ```text
//!   0x00  frame   [0x00][u32 big-endian jpeg length][jpeg bytes]
//!   0x01  url     [0x01][utf-8 url]
//!   0x02  status  [0x02][utf-8 "no-session" | "started" | "ended" | "error: ..."]
//! ```
//!
//! # View-only, permanently
//!
//! There is no tag for input and there will not be one. A page driven by both
//! the court and the King, with nothing arbitrating between them, is precisely
//! the collision this product exists to *surface* -- building one into the
//! instrument meant to reveal such collisions would be self-refuting. The
//! canvas is `pointer-events: none` at the other end so the King never has to
//! wonder whether a click landed.
//!
//! Nothing here creates a browser either. A viewer attaches to whatever session
//! the plan already has, or is told `no-session`; see
//! [`kingdom_browser::BrowserSessionManager::watch`].

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::Path;
use axum::response::Response;
use kingdom_browser::ScreencastEvent;
use tokio::sync::broadcast::error::RecvError;

/// The path the spyglass connects to. One plan per socket.
pub const ROUTE: &str = "/watch/plan/{id}/browser";

const TAG_FRAME: u8 = 0x00;
const TAG_URL: u8 = 0x01;
const TAG_STATUS: u8 = 0x02;

pub async fn upgrade(ws: WebSocketUpgrade, Path(id): Path<String>) -> Response {
    ws.on_upgrade(move |socket| watch(socket, id))
}

async fn watch(mut socket: WebSocket, plan: String) {
    // Attach to a browser that already exists, or say so and close. The King
    // opening a panel must never be what starts a Chrome.
    let attached = crate::tools::browser::browsers().watch(&plan).await;

    let (broker, mut frames, opening_url) = match attached {
        Ok(Some(attached)) => attached,
        Ok(None) => {
            let _ = send(&mut socket, status("no-session")).await;
            return;
        }
        Err(error) => {
            let _ = send(&mut socket, status(&format!("error: {error}"))).await;
            return;
        }
    };

    if send(&mut socket, status("started")).await.is_err() {
        return;
    }

    // Catch the viewer up on where the page is, so its header is right before
    // the first frame arrives rather than after the next navigation.
    if let Some(url) = opening_url {
        if send(&mut socket, url_frame(&url)).await.is_err() {
            return;
        }
    }

    loop {
        tokio::select! {
            event = frames.recv() => match event {
                Ok(ScreencastEvent::Frame { jpeg }) => {
                    if send(&mut socket, frame(&jpeg)).await.is_err() {
                        break;
                    }
                }
                Ok(ScreencastEvent::Url(url)) => {
                    if send(&mut socket, url_frame(&url)).await.is_err() {
                        break;
                    }
                }
                // This viewer fell behind. Skip to the present rather than
                // work through a backlog: on a live view the frames that were
                // missed are already wrong, and the next one is complete on its
                // own. The same reasoning as the chamber's lagged plans.
                Err(RecvError::Lagged(_)) => continue,
                Err(RecvError::Closed) => {
                    let _ = send(&mut socket, status("ended")).await;
                    break;
                }
            },

            // Nothing is expected from the browser -- this socket is one-way --
            // but the read half still has to be driven, because it is what
            // delivers Close and keeps ping/pong alive. Without this arm a
            // closed panel would leave its screencast running until the next
            // frame happened to fail on write, which is the exact cost the
            // lazy-stop lifetime exists to avoid.
            from_browser = socket.recv() => match from_browser {
                Some(Ok(_)) => continue,
                _ => break,
            },
        }
    }

    // Explicit, because the ordering is the whole design: when this drops, if
    // no other viewer holds one, the broker's `Drop` stops the screencast and
    // Chrome goes back to not painting for an audience of nobody.
    drop(broker);
}

async fn send(socket: &mut WebSocket, payload: Vec<u8>) -> Result<(), ()> {
    socket
        .send(Message::Binary(payload.into()))
        .await
        .map_err(|_| ())
}

fn frame(jpeg: &[u8]) -> Vec<u8> {
    let length: u32 = jpeg.len().try_into().unwrap_or(u32::MAX);
    let mut out = Vec::with_capacity(1 + 4 + jpeg.len());
    out.push(TAG_FRAME);
    out.extend_from_slice(&length.to_be_bytes());
    out.extend_from_slice(jpeg);
    out
}

fn url_frame(url: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(1 + url.len());
    out.push(TAG_URL);
    out.extend_from_slice(url.as_bytes());
    out
}

fn status(status: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(1 + status.len());
    out.push(TAG_STATUS);
    out.extend_from_slice(status.as_bytes());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The frame layout, pinned because both ends hand-roll it and a
    /// disagreement is not a compile error -- it is a canvas that stays blank
    /// or draws garbage, with nothing to say why.
    ///
    /// The length prefix is the part worth the test: a JPEG can contain any
    /// byte, so a reader that scanned for a delimiter instead would truncate at
    /// the first one that happened to look like a tag.
    #[test]
    fn a_frame_is_its_tag_then_its_length_then_exactly_its_bytes() {
        let jpeg = [0xFF, 0xD8, 0xFF, 0xE0];
        let encoded = frame(&jpeg);

        assert_eq!(encoded[0], TAG_FRAME);
        assert_eq!(&encoded[1..5], &4u32.to_be_bytes());
        assert_eq!(&encoded[5..], &jpeg);

        assert_eq!(url_frame("http://localhost:3000")[0], TAG_URL);
        assert_eq!(status("no-session")[0], TAG_STATUS);
    }
}
