//! The spyglass panel: watching a plan's browser from its chamber.
//!
//! The client half of `crate::spyglass`. Reads the binary frames that module
//! documents and paints them onto a canvas.
//!
//! **View-only.** The canvas is `pointer-events: none`, so a King who clicks it
//! gets nothing rather than an ambiguous non-response. The reasoning is in the
//! server module and it is a permanent decision, not a missing feature.
//!
//! # Non-goal, recorded rather than guessed at
//!
//! A city lighting up on the map because a plan holds a live browser is the
//! obvious next thought, and it is deliberately not built. It needs two things
//! Kingdom does not have: a plan that *knows* it owns a session (a field, set
//! when a browser deed first launches one, proclaimed by the herald like any
//! other change), and live updates reaching the map at all -- which `AGENTS.md`
//! lists as unbuilt. Both are real; neither is this. Guessing at UI nobody has
//! asked for is how the lease machinery happened.

use leptos::prelude::*;

/// What the socket has last told us.
///
/// `NoSession` and `Ended` are only ever set by the socket handler, which is
/// `hydrate`-only -- so on the server build they are legitimately unreachable
/// rather than forgotten.
#[cfg_attr(not(feature = "hydrate"), allow(dead_code))]
#[derive(Clone, Copy, PartialEq, Eq)]
enum Sight {
    Opening,
    /// The plan has no browser. Not an error -- the usual case, for a plan that
    /// has never been asked to look at a page.
    NoSession,
    Live,
    Ended,
}

impl Sight {
    fn tell(self) -> &'static str {
        match self {
            Sight::Opening => "Raising the spyglass\u{2026}",
            Sight::NoSession => "This plan has not opened a browser.",
            Sight::Live => "",
            Sight::Ended => "The court's browser has closed.",
        }
    }
}

/// A live view of one plan's browser.
#[component]
pub fn Spyglass(plan: kingdom_core::PlanId) -> impl IntoView {
    let canvas = NodeRef::<leptos::html::Canvas>::new();
    let (sight, set_sight) = signal(Sight::Opening);
    let (url, set_url) = signal(String::new());

    watch_browser(plan, canvas, set_sight, set_url);

    view! {
        <div class="spyglass">
            <div class="spyglass-bar">
                <span class="spyglass-url">{move || url.get()}</span>
                // Said plainly rather than shown as a badge: the King is being
                // told he is watching and cannot touch, which is a sentence,
                // not an icon.
                <span class="spyglass-note">"watching"</span>
            </div>
            <div class="spyglass-stage">
                <canvas class="spyglass-canvas" node_ref=canvas></canvas>
                <Show when=move || sight.get() != Sight::Live>
                    <p class="spyglass-empty">{move || sight.get().tell()}</p>
                </Show>
            </div>
        </div>
    }
}

/// Opens the socket for as long as this panel is mounted.
///
/// Split out so the wasm-only machinery does not clutter the view, and so the
/// server build has one obvious no-op instead of a `cfg` in the middle of the
/// markup.
#[cfg(feature = "hydrate")]
fn watch_browser(
    plan: kingdom_core::PlanId,
    canvas: NodeRef<leptos::html::Canvas>,
    set_sight: WriteSignal<Sight>,
    set_url: WriteSignal<String>,
) {
    // Owned by the effect, so leaving the chamber closes the socket -- which is
    // what stops the screencast, because the last viewer detaching is what
    // drops the broker. A leaked socket here is a Chrome painting forever.
    let _watch = LocalResource::new(move || {
        let plan = plan.clone();
        async move { Watch::open(&plan, canvas, set_sight, set_url) }
    });
}

#[cfg(not(feature = "hydrate"))]
fn watch_browser(
    _plan: kingdom_core::PlanId,
    _canvas: NodeRef<leptos::html::Canvas>,
    _set_sight: WriteSignal<Sight>,
    _set_url: WriteSignal<String>,
) {
}

/// An open spyglass, which closes itself when dropped.
#[cfg(feature = "hydrate")]
struct Watch {
    socket: web_sys::WebSocket,
    /// Kept alive for the socket's lifetime: a closure handed to JS and then
    /// dropped on the Rust side would be called after being freed. Same
    /// reasoning as `PlanWatch` in `conversation.rs`.
    _on_message: wasm_bindgen::closure::Closure<dyn FnMut(web_sys::MessageEvent)>,
}

#[cfg(feature = "hydrate")]
impl Watch {
    fn open(
        plan: &kingdom_core::PlanId,
        canvas: NodeRef<leptos::html::Canvas>,
        set_sight: WriteSignal<Sight>,
        set_url: WriteSignal<String>,
    ) -> Self {
        use wasm_bindgen::closure::Closure;
        use wasm_bindgen::JsCast;

        let socket = web_sys::WebSocket::new(&Self::url(plan))
            .expect("the spyglass socket should be constructible");
        // Frames are binary. Without this the browser hands us Blobs, which are
        // read asynchronously and would arrive out of order under load -- on a
        // live view that shows up as the picture flickering backwards.
        socket.set_binary_type(web_sys::BinaryType::Arraybuffer);

        let on_message = Closure::<dyn FnMut(web_sys::MessageEvent)>::new(
            move |event: web_sys::MessageEvent| {
                let Ok(buffer) = event.data().dyn_into::<js_sys::ArrayBuffer>() else {
                    return;
                };
                let bytes = js_sys::Uint8Array::new(&buffer).to_vec();
                let Some((&tag, rest)) = bytes.split_first() else {
                    return;
                };

                match tag {
                    TAG_FRAME => {
                        // Skip the length prefix: the socket already framed the
                        // message for us, so it is the server's own check rather
                        // than something this end needs to parse.
                        if rest.len() > 4 {
                            set_sight.set(Sight::Live);
                            paint(canvas, &rest[4..]);
                        }
                    }
                    TAG_URL => set_url.set(String::from_utf8_lossy(rest).into_owned()),
                    TAG_STATUS => match String::from_utf8_lossy(rest).as_ref() {
                        "no-session" => set_sight.set(Sight::NoSession),
                        "ended" => set_sight.set(Sight::Ended),
                        // "started" is not yet "live": the screencast is running
                        // but nothing has been painted, and claiming otherwise
                        // would show the King an empty canvas labelled as a
                        // working browser.
                        _ => {}
                    },
                    _ => {}
                }
            },
        );
        socket.set_onmessage(Some(on_message.as_ref().unchecked_ref()));

        Self {
            socket,
            _on_message: on_message,
        }
    }

    /// Derived from the page's own origin, so it follows the server wherever it
    /// is served from and upgrades to `wss` when the page is secure.
    fn url(plan: &kingdom_core::PlanId) -> String {
        let location = web_sys::window().expect("a browser has a window").location();
        let secure = location.protocol().map(|p| p == "https:").unwrap_or(false);
        let host = location.host().unwrap_or_default();
        let scheme = if secure { "wss" } else { "ws" };
        format!("{scheme}://{host}/watch/plan/{plan}/browser")
    }
}

#[cfg(feature = "hydrate")]
impl Drop for Watch {
    fn drop(&mut self) {
        self.socket.set_onmessage(None);
        let _ = self.socket.close();
    }
}

/// Draws one JPEG frame onto the canvas.
///
/// Via a blob URL and an `<img>` because a canvas cannot be handed compressed
/// bytes directly. The URL is revoked in the load handler rather than straight
/// after `set_src`: revoking immediately races the decode, and the frame that
/// loses simply never appears.
#[cfg(feature = "hydrate")]
fn paint(canvas: NodeRef<leptos::html::Canvas>, jpeg: &[u8]) {
    use wasm_bindgen::closure::Closure;
    use wasm_bindgen::JsCast;

    let Some(canvas) = canvas.get_untracked() else {
        return;
    };
    let canvas: web_sys::HtmlCanvasElement = canvas.into();

    let array = js_sys::Uint8Array::from(jpeg);
    let parts = js_sys::Array::new();
    parts.push(&array.buffer());

    let options = web_sys::BlobPropertyBag::new();
    options.set_type("image/jpeg");
    let Ok(blob) = web_sys::Blob::new_with_u8_array_sequence_and_options(&parts, &options) else {
        return;
    };
    let Ok(src) = web_sys::Url::create_object_url_with_blob(&blob) else {
        return;
    };

    let Ok(image) = web_sys::HtmlImageElement::new() else {
        let _ = web_sys::Url::revoke_object_url(&src);
        return;
    };

    let on_load = {
        let image = image.clone();
        let src = src.clone();
        Closure::once_into_js(move || {
            // Match the backing store to the frame, so a page at one size is not
            // resampled into a canvas at another and shown blurred.
            canvas.set_width(image.natural_width());
            canvas.set_height(image.natural_height());
            if let Ok(Some(context)) = canvas.get_context("2d") {
                if let Ok(context) = context.dyn_into::<web_sys::CanvasRenderingContext2d>() {
                    let _ = context.draw_image_with_html_image_element(&image, 0.0, 0.0);
                }
            }
            let _ = web_sys::Url::revoke_object_url(&src);
        })
    };
    image.set_onload(Some(on_load.unchecked_ref()));
    image.set_src(&src);
}

#[cfg(feature = "hydrate")]
const TAG_FRAME: u8 = 0x00;
#[cfg(feature = "hydrate")]
const TAG_URL: u8 = 0x01;
#[cfg(feature = "hydrate")]
const TAG_STATUS: u8 = 0x02;
