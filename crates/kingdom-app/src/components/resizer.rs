//! A drag handle that sets the width of the panel beside it.
//!
//! Two panels want this: the left rail, which grows as the pointer moves right,
//! and the chamber's spyglass, which grows as it moves left. They are one
//! component rather than two because the fiddly parts are not the arithmetic --
//! they are the three details below, and each one is a bug that would otherwise
//! have to be found and fixed twice.
//!
//! - **The move and release listeners are on the window, not the handle.** The
//!   handle is a few pixels wide and the pointer leaves it immediately. Worse,
//!   what it leaves it *for* has handlers of its own -- the map pans on
//!   `mousemove` -- so an element-level listener does not merely stop tracking,
//!   it hands the drag to something else.
//! - **`body.resizing` for the duration.** Without it the drag smears a text
//!   selection across whatever the pointer crosses.
//! - **The width is stored once, on release.** Writing every `mousemove` would
//!   hammer `localStorage` for a value only the next visit reads.

use leptos::ev;
use leptos::prelude::*;

/// Which way the panel grows when the pointer moves right.
///
/// The whole difference between the two callers, and the one thing a caller can
/// get backwards -- so it is named rather than passed as a sign.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Grows {
    /// The panel is left of the handle: dragging right widens it. The rail.
    Rightwards,
    /// The panel is right of the handle: dragging right narrows it. The
    /// spyglass.
    Leftwards,
}

impl Grows {
    /// How much a pointer that has travelled `delta` pixels right should add.
    fn apply(self, delta: f64) -> f64 {
        match self {
            Grows::Rightwards => delta,
            Grows::Leftwards => -delta,
        }
    }
}

/// How wide a panel may be dragged, and where a double-click returns it to.
#[derive(Clone, Copy)]
pub struct Bounds {
    pub min: f64,
    pub max: f64,
    /// The width the panel opens at, and the one a double-click restores.
    pub default: f64,
}

/// The drag handle on a panel's edge.
///
/// `class` is the caller's, because the handle's position -- which edge, which
/// offset, what it looks like on hover -- belongs to the panel it borders and
/// not to this component.
#[component]
pub fn Resizer(
    /// The panel's width in pixels. Driven live during the drag.
    width: RwSignal<f64>,
    grows: Grows,
    bounds: Bounds,
    /// Where the width is remembered between visits.
    storage_key: &'static str,
    class: &'static str,
) -> impl IntoView {
    // Drag origin: pointer x and panel width at mousedown. Both are needed --
    // accumulating per-move deltas drifts, because the width is clamped and a
    // clamped step silently loses the remainder.
    let drag = RwSignal::new(Option::<(f64, f64)>::None);

    let on_mouse_down = move |ev: ev::MouseEvent| {
        ev.prevent_default();
        drag.set(Some((ev.client_x() as f64, width.get_untracked())));
        set_resizing_class(true);
    };

    let move_handle = window_event_listener(ev::mousemove, move |ev: ev::MouseEvent| {
        if let Some((start_x, start_w)) = drag.get_untracked() {
            let next = start_w + grows.apply(ev.client_x() as f64 - start_x);
            width.set(next.clamp(bounds.min, bounds.max));
        }
    });

    let up_handle = window_event_listener(ev::mouseup, move |_| {
        if drag.get_untracked().is_some() {
            drag.set(None);
            set_resizing_class(false);
            store_width(storage_key, width.get_untracked());
        }
    });

    on_cleanup(move || {
        move_handle.remove();
        up_handle.remove();
    });

    view! {
        <div
            class=class
            class:dragging=move || drag.get().is_some()
            title="Drag to resize \u{b7} double-click to reset"
            on:mousedown=on_mouse_down
            on:dblclick=move |_| {
                width.set(bounds.default);
                store_width(storage_key, bounds.default);
            }
        ></div>
    }
}

// --- Width persistence -----------------------------------------------------
//
// Browser-only, and every call is failure-tolerant: storage can be disabled
// entirely, in which case the panel simply opens at its default width.

#[cfg(feature = "hydrate")]
fn local_storage() -> Option<web_sys::Storage> {
    web_sys::window()?.local_storage().ok().flatten()
}

/// Restores a stored width inside an effect, so it runs only on the client.
/// Reading it during rendering would make the server emit different markup than
/// hydration expects.
///
/// Called by the panel rather than by [`Resizer`], because the panel is what
/// needs the width before the handle is necessarily mounted -- the spyglass's
/// resizer only exists while the panel is open.
pub fn restore_width(width: RwSignal<f64>, storage_key: &'static str, bounds: Bounds) {
    #[cfg(feature = "hydrate")]
    Effect::new(move |_| {
        if let Some(stored) = local_storage()
            .and_then(|s| s.get_item(storage_key).ok().flatten())
            .and_then(|raw| raw.parse::<f64>().ok())
        {
            width.set(stored.clamp(bounds.min, bounds.max));
        }
    });

    #[cfg(not(feature = "hydrate"))]
    let _ = (width, storage_key, bounds);
}

fn store_width(storage_key: &'static str, width: f64) {
    #[cfg(feature = "hydrate")]
    if let Some(storage) = local_storage() {
        let _ = storage.set_item(storage_key, &width.to_string());
    }

    #[cfg(not(feature = "hydrate"))]
    let _ = (storage_key, width);
}

/// Suppresses text selection for the duration of a drag, so the pointer
/// crossing the map does not smear a highlight across it.
fn set_resizing_class(on: bool) {
    #[cfg(feature = "hydrate")]
    if let Some(body) = web_sys::window()
        .and_then(|w| w.document())
        .and_then(|d| d.body())
    {
        let list = body.class_list();
        let _ = if on {
            list.add_1("resizing")
        } else {
            list.remove_1("resizing")
        };
    }

    #[cfg(not(feature = "hydrate"))]
    let _ = on;
}
