//! Pointer control of the camera.
//!
//! Dragging pans, the wheel zooms about the cursor. Both are read from the
//! window rather than from picking, so the empty sky drags just as well as a
//! rooftop does.

use bevy::input::mouse::{MouseScrollUnit, MouseWheel};
use bevy::prelude::*;
use bevy::window::CursorMoved;

use super::camera::CameraRig;

/// A line of wheel scroll is worth this many pixels of zoom travel.
const LINE_TO_PIXELS: f32 = 18.0;

/// How much of a zoom one pixel of scroll is worth.
const ZOOM_SENSITIVITY: f32 = 0.0022;

/// Where the pointer is, in pixels from the centre of the viewport.
#[derive(Resource, Default, Debug, Clone, Copy)]
pub struct PointerState {
    /// Offset from the viewport centre, which is what zooming needs.
    pub anchor: Vec2,
    /// Whether a drag is under way.
    pub dragging: bool,
}

/// Records where the pointer is, in viewport pixels.
pub fn track_pointer(
    mut moved: MessageReader<CursorMoved>,
    buttons: Res<ButtonInput<MouseButton>>,
    windows: Query<&Window>,
    mut pointer: ResMut<PointerState>,
    mut rig: ResMut<CameraRig>,
) {
    let Ok(window) = windows.single() else {
        return;
    };
    let center = Vec2::new(window.width(), window.height()) * 0.5;

    if buttons.just_pressed(MouseButton::Left) {
        pointer.dragging = true;
    }
    if buttons.just_released(MouseButton::Left) {
        pointer.dragging = false;
    }

    for event in moved.read() {
        pointer.anchor = event.position - center;
        if pointer.dragging {
            if let Some(delta) = event.delta {
                rig.pan(delta);
            }
        }
    }
}

/// Turns wheel movement into a zoom about the pointer.
pub fn handle_scroll(
    mut wheel: MessageReader<MouseWheel>,
    pointer: Res<PointerState>,
    mut rig: ResMut<CameraRig>,
) {
    let mut scrolled = 0.0;
    for event in wheel.read() {
        scrolled += match event.unit {
            MouseScrollUnit::Line => event.y * LINE_TO_PIXELS,
            MouseScrollUnit::Pixel => event.y,
        };
    }
    if scrolled.abs() < f32::EPSILON {
        return;
    }
    // Exponential so a scroll feels the same at every zoom, and so the wheel
    // never crosses zero into an inverted scale.
    rig.zoom_by((scrolled * ZOOM_SENSITIVITY).exp(), pointer.anchor);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scrolling_up_zooms_in() {
        let mut rig = CameraRig {
            focus: Vec3::ZERO,
            scale: 2.0,
            fit_scale: 2.0,
            span: 1_000.0,
            holding: 30.0,
        };
        let before = rig.zoom();
        rig.zoom_by((120.0 * ZOOM_SENSITIVITY).exp(), Vec2::ZERO);
        assert!(rig.zoom() > before, "zoom was {}", rig.zoom());
    }

    #[test]
    fn scrolling_down_zooms_out() {
        let mut rig = CameraRig {
            focus: Vec3::ZERO,
            scale: 0.5,
            fit_scale: 2.0,
            span: 1_000.0,
            holding: 30.0,
        };
        let before = rig.zoom();
        rig.zoom_by((-120.0 * ZOOM_SENSITIVITY).exp(), Vec2::ZERO);
        assert!(rig.zoom() < before);
    }
}
