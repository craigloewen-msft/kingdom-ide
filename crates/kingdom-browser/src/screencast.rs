//! The screencast's source: a CDP screencast, relayed to whoever is watching.
//!
//! A plan's Chrome is headless, so the only way the user can see what his model
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
//! see the note on the same subject in `kingdom-app`'s `screencast` module.
//! This is a one-way relay from Chrome to a socket.

use base64::Engine;
use chromiumoxide::cdp::browser_protocol::page::{
    EventFrameNavigated, EventScreencastFrame, ScreencastFrameAckParams, StartScreencastFormat,
    StartScreencastParams, StopScreencastParams,
};
use chromiumoxide::Page;
use futures::StreamExt;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{broadcast, Mutex};
use tokio::task::JoinHandle;
use tokio::time::Instant;

use crate::session::BrowserError;

/// JPEG quality, 0-100.
///
/// Below the 70 Chrome's own DevTools screencast uses. This is a panel beside a
/// conversation, not a recording: at 60 the artefacts are invisible at the size
/// it is drawn, and every frame is cheaper to encode and to push down a socket.
const QUALITY: i64 = 60;

/// The largest frame Chrome is asked to encode, in CSS pixels.
///
/// The browser runs at 1440x900 (see `session::DEFAULT_VIEWPORT`) but the
/// spyglass is a panel, and encoding a full-size JPEG to be drawn at half of it
/// is work thrown away twice -- once in Chrome and once on the wire. Chrome
/// scales to fit inside this while keeping the aspect ratio, so the picture is
/// unchanged in shape and only in resolution.
const MAX_WIDTH: i64 = 960;
const MAX_HEIGHT: i64 = 600;

/// The shortest gap between two frames, enforced by holding the acknowledgement.
///
/// Chrome will not send the next frame until the last one is acked, so the ack
/// *is* the throttle -- there is no CDP setting for "frames per second", and
/// `everyNthFrame` counts frames the page produces rather than seconds.
///
/// Measured on an animating page at 1440x900: acking immediately, as this did,
/// ran the capture at 68 frames a second and cost 134% of a core, against 66%
/// for the same page with nobody watching -- the spyglass was doubling the cost
/// of the browser it was looking at. Held to this interval it costs 87% and
/// delivers about ten frames a second, which no one watching a page being
/// driven can tell from the sixty-eight.
const FRAME_INTERVAL: Duration = Duration::from_millis(100);

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
    /// user has open.
    Frame { jpeg: Arc<[u8]> },
    /// The page navigated. The conversation shows this above the picture, so
    /// the user can see *where* the model is, not merely that something
    /// changed.
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
            max_width: Some(MAX_WIDTH),
            max_height: Some(MAX_HEIGHT),
            // Left at 1: the pacing is done by holding the ack (see
            // [`FRAME_INTERVAL`]), which is in seconds. `everyNthFrame` counts
            // frames the *page* produced, so on a page that repaints rarely it
            // would drop the few frames there are, and on a busy one it would
            // still deliver whatever fraction of sixty happened to remain.
            every_nth_frame: Some(1),
        };
        page.execute(params).await?;

        let (tx, _) = broadcast::channel(BACKLOG);
        let last_url = Arc::new(Mutex::new(None));

        // Seed from wherever the page already is, so the first viewer gets a URL
        // even if the model never navigates again.
        if let Ok(Some(url)) = page.url().await {
            *last_url.lock().await = Some(url);
        }

        let listener = listen(
            page.clone(),
            tx.clone(),
            Arc::clone(&last_url),
            frames,
            navigations,
        );

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
        // When the last frame was let through. `None` until the first arrives,
        // so the opening frame is never delayed -- a viewer attaching should
        // see the page immediately, and only the *rate* is being limited.
        let mut last_frame: Option<Instant> = None;

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

                    // The throttle. Sleeping *before* the ack is what makes it
                    // one: Chrome is waiting on this acknowledgement, so the
                    // pause is time it spends not painting and not encoding,
                    // rather than time we spend discarding work it already did.
                    if let Some(wait) = wait_before_ack(last_frame, Instant::now()) {
                        tokio::time::sleep(wait).await;
                    }
                    last_frame = Some(Instant::now());

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

/// How long to hold an acknowledgement so frames arrive no faster than
/// [`FRAME_INTERVAL`].
///
/// Split out from the loop because it is the whole of the pacing decision and
/// the only part of it worth testing -- the rest is a CDP round trip. `None`
/// means send now: either this is the first frame, or the interval has already
/// passed while the frame was being decoded and broadcast.
fn wait_before_ack(last_frame: Option<Instant>, now: Instant) -> Option<Duration> {
    let elapsed = now.duration_since(last_frame?);
    FRAME_INTERVAL.checked_sub(elapsed).filter(|d| !d.is_zero())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The first frame is never held back.
    ///
    /// A viewer attaching should see the page at once. The throttle is on the
    /// *rate*, and a rate limiter that delayed the opening frame would make
    /// opening the panel feel broken for the tenth of a second that matters
    /// most.
    #[test]
    fn the_first_frame_is_sent_immediately() {
        assert_eq!(wait_before_ack(None, Instant::now()), None);
    }

    /// A frame arriving too soon is held for the remainder of the interval.
    ///
    /// The exact figure matters: holding for the whole interval regardless of
    /// how much of it had passed would halve the frame rate again, and holding
    /// for none of it is the unthrottled behaviour this replaced.
    #[test]
    fn a_frame_that_comes_too_soon_waits_out_the_remainder() {
        let started = Instant::now();
        let quarter = FRAME_INTERVAL / 4;

        let wait = wait_before_ack(Some(started), started + quarter)
            .expect("a frame this early must be held");

        assert_eq!(wait, FRAME_INTERVAL - quarter);
    }

    /// Once the interval has passed, nothing is held.
    ///
    /// This is what keeps a slow page responsive: if decoding and broadcasting
    /// a frame already took longer than the interval, the next one must go
    /// straight out rather than paying the delay twice.
    #[test]
    fn a_frame_that_comes_late_enough_is_not_delayed() {
        let started = Instant::now();

        assert_eq!(
            wait_before_ack(Some(started), started + FRAME_INTERVAL),
            None
        );
        assert_eq!(
            wait_before_ack(Some(started), started + FRAME_INTERVAL * 3),
            None
        );
    }
}
