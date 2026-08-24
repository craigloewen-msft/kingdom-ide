//! The spyglass's source: a CDP screencast, relayed to whoever is watching.
//!
//! A plan's Chrome is headless, so the only way the King can see what his court
//! is doing to a page is for us to ask Chrome for pictures of it.
//! `Page.startScreencast` does exactly that, and this module fans the frames out
//! to any number of viewers.
//!
//! # Why the lifetime is structural
//!
//! A screencast is not free -- it forces Chrome to paint a frame and encode a
//! JPEG continuously, whether or not anybody is looking. So it starts on the
//! first viewer and stops on the last, and that is enforced by ownership rather
//! than by a counter someone has to remember to decrement:
//!
//! - each viewer holds an `Arc<ScreencastBroker>`,
//! - the session holds only a `Weak`,
//! - the broker's `Drop` aborts its listener and fires `Page.stopScreencast`.
//!
//! A counter would leak the screencast the first time a viewer's task panicked
//! between increment and decrement. `Arc` cannot forget.
//!
//! # View-only
//!
//! Nothing here accepts input, and that is deliberate rather than unfinished --
//! see the note on the same subject in `kingdom-app`'s `spyglass` module. This
//! is a one-way relay from Chrome to a socket.

use base64::Engine;
use chromiumoxide::cdp::browser_protocol::page::{
    EventFrameNavigated, EventScreencastFrame, ScreencastFrameAckParams, StartScreencastFormat,
    StartScreencastParams, StopScreencastParams,
};
use chromiumoxide::Page;
use futures::StreamExt;
use std::sync::Arc;
use tokio::sync::{broadcast, Mutex};
use tokio::task::JoinHandle;

use crate::session::BrowserError;

/// JPEG quality, 0-100. The value Chrome's own DevTools screencast uses:
/// sub-100KB frames for a typical page, without visible artefacts on text.
const QUALITY: i64 = 70;

/// How many frames a slow viewer may fall behind before it starts skipping.
///
/// Small on purpose. This is a *live* view, so a viewer that cannot keep up
/// should jump to the present rather than work through a backlog -- stale frames
/// are worse than missing ones, and blocking the source to preserve them would
/// be worse still.
const BACKLOG: usize = 16;

/// One thing a viewer is told.
#[derive(Debug, Clone)]
pub enum ScreencastEvent {
    /// A frame, already decoded from the base64 CDP delivers it in.
    ///
    /// `Arc` because every attached viewer receives the same bytes; cloning the
    /// image per socket would multiply a 100KB frame by the number of tabs the
    /// King has open.
    Frame { jpeg: Arc<[u8]> },
    /// The page navigated. The chamber shows this above the picture, so the
    /// King can see *where* the court is, not merely that something changed.
    Url(String),
}

/// One live screencast, and its subscribers.
pub struct ScreencastBroker {
    tx: broadcast::Sender<ScreencastEvent>,
    /// The last URL seen, so a viewer attaching mid-flow can paint its header
    /// immediately rather than waiting for the next navigation.
    last_url: Arc<Mutex<Option<String>>>,
    /// Held so `Drop` can stop the screencast without needing the session back.
    page: Page,
    listener: JoinHandle<()>,
}

impl ScreencastBroker {
    /// Starts a screencast on this page and returns a broker ready for viewers.
    ///
    /// On failure the screencast is *not* running, so a caller may retry without
    /// leaving a half-started capture behind.
    ///
    /// # Errors
    /// If the CDP event subscriptions or `Page.startScreencast` fail.
    pub async fn start(page: Page) -> Result<Arc<Self>, BrowserError> {
        // Subscribe before starting, or the first frame can arrive between the
        // two and be lost -- which shows up as a viewer staring at nothing until
        // the page next repaints.
        let frames = page.event_listener::<EventScreencastFrame>().await?;
        let navigations = page.event_listener::<EventFrameNavigated>().await?;

        let params = StartScreencastParams {
            format: Some(StartScreencastFormat::Jpeg),
            quality: Some(QUALITY),
            max_width: None,
            max_height: None,
            every_nth_frame: Some(1),
        };
        page.execute(params).await?;

        let (tx, _) = broadcast::channel(BACKLOG);
        let last_url = Arc::new(Mutex::new(None));

        // Seed from wherever the page already is, so the first viewer gets a URL
        // even if the court never navigates again.
        if let Ok(Some(url)) = page.url().await {
            *last_url.lock().await = Some(url);
        }

        let listener = listen(page.clone(), tx.clone(), Arc::clone(&last_url), frames, navigations);

        Ok(Arc::new(Self {
            tx,
            last_url,
            page,
            listener,
        }))
    }

    /// Adds a viewer, and tells it where the page currently is.
    pub async fn subscribe(&self) -> (broadcast::Receiver<ScreencastEvent>, Option<String>) {
        let url = self.last_url.lock().await.clone();
        (self.tx.subscribe(), url)
    }

    /// How many viewers are attached. For diagnostics only -- the lifecycle is
    /// governed by `Arc` strong counts, not by this.
    #[must_use]
    pub fn viewers(&self) -> usize {
        self.tx.receiver_count()
    }
}

impl Drop for ScreencastBroker {
    fn drop(&mut self) {
        self.listener.abort();
        // Best effort: `Drop` cannot await, and if the page or the browser is
        // already gone then the screencast died with it anyway.
        let page = self.page.clone();
        tokio::spawn(async move {
            let _ = page.execute(StopScreencastParams::default()).await;
        });
    }
}

/// Pumps CDP events into the broadcast channel until the broker is dropped.
fn listen(
    page: Page,
    tx: broadcast::Sender<ScreencastEvent>,
    last_url: Arc<Mutex<Option<String>>>,
    mut frames: chromiumoxide::listeners::EventStream<EventScreencastFrame>,
    mut navigations: chromiumoxide::listeners::EventStream<EventFrameNavigated>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            tokio::select! {
                frame = frames.next() => {
                    let Some(frame) = frame else { break };

                    // Decode once here rather than per viewer, and acknowledge
                    // even when decoding fails: Chrome will not send another
                    // frame until the last one is acked, so a skipped ack stalls
                    // the whole screencast rather than losing one image.
                    let data: &str = frame.data.as_ref();
                    if let Ok(jpeg) = base64::engine::general_purpose::STANDARD.decode(data) {
                        let _ = tx.send(ScreencastEvent::Frame {
                            jpeg: Arc::from(jpeg.into_boxed_slice()),
                        });
                    }
                    let _ = page
                        .execute(ScreencastFrameAckParams::new(frame.session_id))
                        .await;
                }
                navigation = navigations.next() => {
                    // Frames are the authoritative stream: if navigation events
                    // stop but pictures keep arriving, keep serving pictures.
                    let Some(navigation) = navigation else { continue };

                    // Only the main frame. An iframe navigating is noise in a
                    // header that claims to say where the page is.
                    if navigation.frame.parent_id.is_some() {
                        continue;
                    }
                    let url = navigation.frame.url.clone();
                    *last_url.lock().await = Some(url.clone());
                    let _ = tx.send(ScreencastEvent::Url(url));
                }
            }
        }
    })
}
