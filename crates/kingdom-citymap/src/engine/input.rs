//! Pointer control of the camera.
//!
//! Dragging pans, the wheel zooms about the cursor. Both are read from the
//! window rather than from picking, so the empty sky drags just as well as a
//! rooftop does.
//!
//! # And what happens when they are used
//!
//! The interface points the map at things on the King's behalf: the city a
//! conversation is about, the file open in front of him. That is welcome right
//! up until he takes hold of the map himself, at which point it is the map
//! arguing with him. So a pan or a zoom by hand *takes the camera*
//! ([`Steering`]), the following stops, and it resumes when he hands it back or
//! when the map has been left alone for [`RELEASE_AFTER`].
//!
//! The decision lives here rather than in `view.rs` for two reasons. It is
//! written by the very systems that move the camera, so it cannot disagree with
//! what actually happened; and it is then plain arithmetic the native test
//! suite can pin, which nothing in `view.rs` can be -- that file is
//! `hydrate`-only and there is no DOM under `cargo test`.

use std::time::Duration;

use bevy::input::mouse::{MouseScrollUnit, MouseWheel};
use bevy::prelude::*;
use bevy::window::CursorMoved;

use super::bridge::Bridge;
use super::camera::CameraRig;

/// A line of wheel scroll is worth this many pixels of zoom travel.
const LINE_TO_PIXELS: f32 = 18.0;

/// How much of a zoom one pixel of scroll is worth.
const ZOOM_SENSITIVITY: f32 = 0.0022;

/// How long the map must be left alone before it follows the King again.
///
/// Long enough that reading a file with the map parked where he put it is
/// never interrupted, short enough that a map abandoned mid-afternoon is
/// pointed at the work in front of him by the time he looks back at it. A
/// judgement rather than a measurement, and a one-constant change.
pub const RELEASE_AFTER: Duration = Duration::from_secs(600);

/// Where the pointer is, in pixels from the centre of the viewport.
#[derive(Resource, Default, Debug, Clone, Copy)]
pub struct PointerState {
    /// Offset from the viewport centre, which is what zooming needs.
    pub anchor: Vec2,
    /// Whether a drag is under way.
    pub dragging: bool,
}

/// Whether the camera is being steered by hand, and when it last was.
///
/// One `Option` rather than a flag and a timestamp: "the map is following" and
/// "the map was last touched at T" are the same fact asked two ways, and
/// keeping them as two fields is how they come to disagree.
#[derive(Resource, Default, Debug, Clone, Copy, PartialEq, Eq)]
pub struct Steering {
    /// When the King last moved the camera himself. `None` means the map is
    /// following the interface.
    last_input: Option<Duration>,
}

impl Steering {
    /// Whether the King has the camera.
    pub fn taken(&self) -> bool {
        self.last_input.is_some()
    }

    /// Records that the King moved the camera at `now`.
    ///
    /// Called only where a pan or a zoom actually moves something. A click
    /// that selects a city moves nothing and must not take the camera, or
    /// every selection would silently stop the map following.
    pub fn touched(&mut self, now: Duration) {
        self.last_input = Some(now);
    }

    /// Hands the camera back to the interface.
    pub fn release(&mut self) {
        self.last_input = None;
    }

    /// Whether the camera is held and has been still for at least `after`.
    ///
    /// Pure, and the only place the waiting actually lives, so the rule can be
    /// tested without a clock. A map that is already following is not "still"
    /// in this sense -- there is nothing to give back -- so this answers false
    /// for it and the caller does no work.
    pub fn still_for(&self, now: Duration, after: Duration) -> bool {
        self.last_input
            .is_some_and(|at| now.saturating_sub(at) >= after)
    }
}

/// Records where the pointer is, in viewport pixels.
pub fn track_pointer(
    mut moved: MessageReader<CursorMoved>,
    buttons: Res<ButtonInput<MouseButton>>,
    windows: Query<&Window>,
    time: Res<Time>,
    mut pointer: ResMut<PointerState>,
    mut steering: ResMut<Steering>,
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
                // Inside the branch that actually moved the camera, and not a
                // line higher: a press that selects a building is not a
                // takeover, and a pointer merely crossing the map is not one
                // either.
                steering.touched(time.elapsed());
            }
        }
    }
}

/// Turns wheel movement into a zoom about the pointer.
pub fn handle_scroll(
    mut wheel: MessageReader<MouseWheel>,
    pointer: Res<PointerState>,
    time: Res<Time>,
    mut steering: ResMut<Steering>,
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
    steering.touched(time.elapsed());
}

/// Hands the camera back once the map has been left alone long enough, and
/// publishes who currently holds it.
///
/// Runs on the engine's own clock, which keeps ticking at `engine::IDLE_WAKE`
/// even when nothing is happening, so the release lands within a quarter
/// second of its due time however long the map has been sitting there.
///
/// The publish is unconditional rather than gated on a change: `update_status`
/// already compares before it moves the revision, so an unchanged flag costs
/// one comparison and wakes nobody.
pub fn release_when_still(time: Res<Time>, bridge: Res<Bridge>, mut steering: ResMut<Steering>) {
    if steering.still_for(time.elapsed(), RELEASE_AFTER) {
        steering.release();
    }
    let taken = steering.taken();
    bridge.update_status(|status| status.manual = taken);
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

    #[test]
    fn the_map_follows_until_it_is_touched() {
        let mut steering = Steering::default();
        assert!(!steering.taken(), "a fresh map follows");

        steering.touched(Duration::from_secs(5));
        assert!(steering.taken());
    }

    #[test]
    fn stillness_hands_the_camera_back() {
        let mut steering = Steering::default();
        steering.touched(Duration::from_secs(100));

        let due = Duration::from_secs(100) + RELEASE_AFTER;
        assert!(!steering.still_for(due - Duration::from_secs(1), RELEASE_AFTER));
        assert!(steering.still_for(due, RELEASE_AFTER));

        steering.release();
        assert!(!steering.taken());
    }

    /// The wait is from the *last* touch, not the first. Otherwise a King who
    /// kept panning would have the map pulled out from under him ten minutes
    /// after he started rather than ten minutes after he stopped.
    #[test]
    fn touching_it_again_restarts_the_wait() {
        let mut steering = Steering::default();
        steering.touched(Duration::from_secs(0));
        steering.touched(RELEASE_AFTER - Duration::from_secs(1));

        assert!(!steering.still_for(RELEASE_AFTER, RELEASE_AFTER));
        assert!(steering.still_for(RELEASE_AFTER * 2 - Duration::from_secs(1), RELEASE_AFTER));
    }

    /// A map that is already following has nothing to give back, and must not
    /// report itself as due a release -- the caller would publish the same
    /// flag forever.
    #[test]
    fn a_following_map_is_never_due_a_release() {
        let steering = Steering::default();
        assert!(!steering.still_for(RELEASE_AFTER * 10, RELEASE_AFTER));
    }
}
