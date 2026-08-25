//! A drag handle that sets the size of the panel beside it.
//!
//! Three panels want this: the left rail, which grows as the pointer moves
//! right; the chamber's focused panel, which grows as it moves left; and the
//! files rail's split, where the tree above grows as the pointer moves *down*.
//! They are one component rather than three because the fiddly parts are not the
//! arithmetic -- they are the three details below, and each one is a bug that
//! would otherwise have to be found and fixed three times.
//!
//! - **The move and release listeners are on the window, not the handle.** The
//!   handle is a few pixels wide and the pointer leaves it immediately. Worse,
//!   what it leaves it *for* has handlers of its own -- the map pans on
//!   `mousemove` -- so an element-level listener does not merely stop tracking,
//!   it hands the drag to something else.
//! - **`body.resizing` for the duration.** Without it the drag smears a text
//!   selection across whatever the pointer crosses.
//! - **The size is stored once, on release.** Writing every `mousemove` would
//!   hammer `localStorage` for a value only the next visit reads.

use leptos::ev;
use leptos::prelude::*;

/// Which way the panel grows as the pointer moves.
///
/// The whole difference between the callers, and the one thing a caller can get
/// backwards -- so it is named rather than passed as a sign. It also carries
/// which *axis* is being dragged, because a handle that read the wrong
/// coordinate would still move, just never in step with the pointer.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Grows {
    /// The panel is left of the handle: dragging right widens it. The rail.
    Rightwards,
    /// The panel is right of the handle: dragging right narrows it. The
    /// chamber's focused panel.
    Leftwards,
    /// The panel is above the handle: dragging down makes it taller. The files
    /// rail's split, where the tree sits over the review drawer.
    Downwards,
}

impl Grows {
    /// Whether this handle follows the pointer's vertical travel.
    fn vertical(self) -> bool {
        matches!(self, Grows::Downwards)
    }

    /// How much a pointer that has travelled `delta` along this handle's axis
    /// (right, or down) should add.
    fn apply(self, delta: f64) -> f64 {
        match self {
            Grows::Rightwards | Grows::Downwards => delta,
            Grows::Leftwards => -delta,
        }
    }
}

/// How far a panel may be dragged, and where a double-click returns it to.
#[derive(Clone, Copy)]
pub struct Bounds {
    pub min: f64,
    pub max: f64,
    /// The size the panel opens at, and the one a double-click restores.
    pub default: f64,
}

/// The drag handle on a panel's edge.
///
/// `class` is the caller's, because the handle's position -- which edge, which
/// offset, what it looks like on hover -- belongs to the panel it borders and
/// not to this component.
#[component]
pub fn Resizer(
    /// The panel's size in pixels -- width, or height for a vertical handle.
    /// Driven live during the drag.
    width: RwSignal<f64>,
    grows: Grows,
    bounds: Bounds,
    /// Where the size is remembered between visits.
    storage_key: &'static str,
    class: &'static str,
) -> impl IntoView {
    // Drag origin: pointer position along the dragged axis, and panel size, both
    // at mousedown. Both are needed -- accumulating per-move deltas drifts,
    // because the size is clamped and a clamped step silently loses the
    // remainder.
    let drag = RwSignal::new(Option::<(f64, f64)>::None);

    // One accessor for both axes, so nothing below has to branch on direction.
    let along = move |ev: &ev::MouseEvent| {
        if grows.vertical() {
            ev.client_y() as f64
        } else {
            ev.client_x() as f64
        }
    };

    let on_mouse_down = move |ev: ev::MouseEvent| {
        ev.prevent_default();
        drag.set(Some((along(&ev), width.get_untracked())));
        set_resizing_class(true);
    };

    let move_handle = window_event_listener(ev::mousemove, move |ev: ev::MouseEvent| {
        if let Some((start, start_size)) = drag.get_untracked() {
            let next = start_size + grows.apply(along(&ev) - start);
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
