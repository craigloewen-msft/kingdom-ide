//! The channel between the Leptos interface and the rendering engine.
//!
//! Leptos owns the DOM chrome — search, the inspector, the minimap, the
//! toolbar — while the engine owns the map. Neither can borrow the other's
//! state directly, so they meet here: Leptos pushes commands, the engine
//! publishes what it is currently showing, and Leptos polls that back into
//! signals.

use std::sync::{Arc, Mutex};

use bevy::prelude::*;
use crate::map::MapManifest;

/// How far the camera is zoomed in, and therefore how much detail is drawn.
///
/// The three tiers are exclusive: exactly one is active at any moment.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum LodLevel {
    /// Wards and towns collapse into labelled plaques.
    #[default]
    Districts,
    /// Full architecture and scenery.
    Architecture,
    /// Architecture plus per-file labels on nearby holdings.
    FileDetail,
}

impl LodLevel {
    /// The tier's name, in the interface's upper case.
    pub fn label(self) -> &'static str {
        match self {
            Self::Districts => "DISTRICTS",
            Self::Architecture => "ARCHITECTURE",
            Self::FileDetail => "FILE DETAIL",
        }
    }

    /// Chooses a tier from how many pixels wide a typical holding is.
    ///
    /// Detail is a question about what can actually be made out on screen, so
    /// it is decided by apparent house size rather than by zoom relative to
    /// the fitted view. The latter meant a big repository stayed in
    /// `Districts` until its houses were a tenth the size a small one showed
    /// architecture at.
    pub fn for_holding_pixels(pixels: f32) -> Self {
        if pixels < 24.0 {
            Self::Districts
        } else if pixels < 64.0 {
            Self::Architecture
        } else {
            Self::FileDetail
        }
    }
}

/// Something the interface asks the engine to do.
#[derive(Clone, Debug)]
pub enum ViewerCommand {
    /// Hand the engine a freshly loaded manifest to build.
    Load(Box<MapManifest>),
    /// Frame the whole world.
    Fit,
    /// Multiply the zoom about the centre of the viewport.
    ZoomBy(f32),
    /// Return to one world unit per pixel.
    ActualSize,
    /// Frame a named place.
    Focus {
        /// Where to centre the camera.
        center: [f32; 2],
        /// The world-space width and depth to fit.
        extent: [f32; 2],
    },
    /// Centre on a world point without changing the zoom.
    LookAt {
        /// The world point to centre on.
        point: [f32; 2],
    },
    /// Pin or unpin a ward from outside the map, which is what a breadcrumb
    /// step does.
    ///
    /// There is no equivalent for a holding, and no map click pins anything:
    /// the map answers questions while the pointer is over it and forgets as
    /// soon as it leaves.
    SelectWard(Option<String>),
    /// Which towns have work under way in them, and how much.
    ///
    /// Replaces whatever was set before rather than amending it: the interface
    /// polls for the whole picture, so a town missing from the list is a town
    /// with nothing running rather than a town nobody mentioned.
    SetActivity(Vec<TownActivity>),
    /// Whether the King is actually looking at the map.
    ///
    /// The map is mounted once for the life of the page and hidden with CSS
    /// while he is in a plan's chamber -- see `kingdom_app::app::ThroneRoom`
    /// for why it may never unmount. But `visibility: hidden` stops the pixels
    /// reaching the screen, not the work of producing them: the engine would
    /// go on running its render graph over every building on the island,
    /// behind a conversation, for as long as that conversation lasted. Since
    /// most of the King's time is spent in a chamber rather than on the map,
    /// that is most of the time.
    Show(bool),
}

/// One town with agents working in it.
///
/// Deliberately a plain string and a count rather than a `kingdom-core` type:
/// the engine knows nothing about cities or plans, and this is the seam that
/// keeps it that way. The interface translates on its side of the bridge.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TownActivity {
    /// The town's name, as [`crate::map::MapTown::name`] gives it.
    ///
    /// Matched by **name** rather than by id, because a manifest's `town-N`
    /// identifiers are numbered from two different orderings -- `scene::towns`
    /// enumerates the packing order, `manifest::build_world_manifest` sorts by
    /// file count first -- so `town-0` need not mean the same settlement in
    /// both halves of one manifest. The name is what both agree on, and it is
    /// the same string `kingdom_app::scan` builds a `CityId` from.
    pub town: String,
    /// How many plans are working there. Never zero.
    pub working: usize,
}

/// What the engine is currently showing.
#[derive(Clone, Debug, Default)]
pub struct ViewerStatus {
    /// Whether a world has been built and drawn.
    ///
    /// False from boot until the first [`ViewerCommand::Load`] has been
    /// spawned and framed, which is what the loading card watches: the
    /// manifest arriving is not the same moment as the settlement standing up,
    /// and building one of a few thousand holdings blocks a frame.
    pub built: bool,
    /// The holding under the pointer.
    pub hovered: Option<String>,
    /// The innermost ward under the pointer, whether that came from the ground
    /// itself or from a holding standing on it.
    pub hovered_ward: Option<String>,
    /// The ward pinned from the breadcrumb, which is what the interface
    /// highlights. Nothing on the map itself sets it.
    pub selected_ward: Option<String>,
    /// Zoom relative to the fitted view, which is what the toolbar shows.
    pub zoom: f32,
    /// The detail tier in force, which the toolbar shows.
    pub lod: LodLevel,
    /// The world-space rect the camera covers, for the minimap indicator.
    pub camera_rect: [f32; 4],
    /// Set when the engine could not build the world it was given.
    pub error: Option<String>,
}

#[derive(Default)]
struct BridgeState {
    commands: Vec<ViewerCommand>,
    status: ViewerStatus,
    /// Bumped whenever the status changes, so Leptos can skip untouched polls.
    revision: u64,
}

/// A shared handle held by both the interface and the engine.
///
/// `Arc<Mutex<_>>` rather than `Rc<RefCell<_>>` because Bevy resources must be
/// `Send + Sync`, even in a single-threaded wasm build.
#[derive(Resource, Clone, Default)]
pub struct Bridge(Arc<Mutex<BridgeState>>);

impl Bridge {
    /// An empty bridge, with nothing queued and nothing shown.
    pub fn new() -> Self {
        Self::default()
    }

    /// Queues a command for the engine to pick up on its next frame.
    pub fn send(&self, command: ViewerCommand) {
        if let Ok(mut state) = self.0.lock() {
            state.commands.push(command);
        }
    }

    /// Takes every queued command, leaving the queue empty.
    pub fn drain_commands(&self) -> Vec<ViewerCommand> {
        self.0
            .lock()
            .map(|mut state| std::mem::take(&mut state.commands))
            .unwrap_or_default()
    }

    /// A snapshot of what the engine is showing.
    pub fn status(&self) -> ViewerStatus {
        self.0
            .lock()
            .map(|state| state.status.clone())
            .unwrap_or_default()
    }

    /// Bumped whenever the status changes, so the interface can poll cheaply
    /// and only re-render when something actually moved.
    pub fn revision(&self) -> u64 {
        self.0.lock().map(|state| state.revision).unwrap_or(0)
    }

    /// Applies a change to the status, bumping [`Bridge::revision`] only if it
    /// made a difference.
    pub fn update_status(&self, update: impl FnOnce(&mut ViewerStatus)) {
        if let Ok(mut state) = self.0.lock() {
            let before = state.status.clone();
            update(&mut state.status);
            if !status_matches(&before, &state.status) {
                state.revision = state.revision.wrapping_add(1);
            }
        }
    }
}

/// Compares two statuses, ignoring sub-pixel camera drift.
///
/// The camera rect changes by a fraction of a unit on almost every frame while
/// a drag is settling. Treating those as changes would wake the interface up
/// sixty times a second for nothing.
fn status_matches(left: &ViewerStatus, right: &ViewerStatus) -> bool {
    // `built` is compared for a reason worth stating: a field left out here is
    // a field the interface never hears about, because this is the only thing
    // that moves `Bridge::revision` and the poll skips an unmoved revision.
    left.built == right.built
        && left.hovered == right.hovered
        && left.hovered_ward == right.hovered_ward
        && left.selected_ward == right.selected_ward
        && left.lod == right.lod
        && left.error == right.error
        && (left.zoom - right.zoom).abs() < 0.005
        && left
            .camera_rect
            .iter()
            .zip(right.camera_rect.iter())
            .all(|(a, b)| (a - b).abs() < 0.5)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commands_are_drained_once() {
        let bridge = Bridge::new();
        bridge.send(ViewerCommand::Fit);
        bridge.send(ViewerCommand::ZoomBy(1.4));

        assert_eq!(bridge.drain_commands().len(), 2);
        assert!(bridge.drain_commands().is_empty());
    }

    #[test]
    fn the_revision_only_moves_on_a_real_change() {
        let bridge = Bridge::new();
        let start = bridge.revision();

        bridge.update_status(|status| status.hovered = Some("file-3".to_owned()));
        let after_hover = bridge.revision();
        assert_ne!(start, after_hover);

        // Sub-pixel camera drift is not a change worth waking the interface for.
        bridge.update_status(|status| status.camera_rect[0] += 0.05);
        assert_eq!(bridge.revision(), after_hover);

        bridge.update_status(|status| status.camera_rect[0] += 12.0);
        assert_ne!(bridge.revision(), after_hover);
    }

    #[test]
    fn the_world_standing_up_wakes_the_interface() {
        // The loading card hangs off this one field, and the poll only reads a
        // status whose revision moved -- so a `built` that did not bump the
        // revision would leave the card up over a finished map forever.
        let bridge = Bridge::new();
        assert!(!bridge.status().built, "nothing is drawn before a Load");

        let start = bridge.revision();
        bridge.update_status(|status| status.built = true);
        assert_ne!(start, bridge.revision());
        assert!(bridge.status().built);
    }

    #[test]
    fn a_ward_changing_wakes_the_interface() {
        let bridge = Bridge::new();
        let start = bridge.revision();
        bridge.update_status(|status| status.hovered_ward = Some("ward-2".to_owned()));
        let after_ward = bridge.revision();
        assert_ne!(start, after_ward);

        bridge.update_status(|status| status.selected_ward = Some("ward-2".to_owned()));
        assert_ne!(after_ward, bridge.revision());
    }

    #[test]
    fn apparent_house_size_picks_exactly_one_detail_tier() {
        assert_eq!(LodLevel::for_holding_pixels(0.0), LodLevel::Districts);
        assert_eq!(LodLevel::for_holding_pixels(23.9), LodLevel::Districts);
        assert_eq!(LodLevel::for_holding_pixels(24.0), LodLevel::Architecture);
        assert_eq!(LodLevel::for_holding_pixels(63.9), LodLevel::Architecture);
        assert_eq!(LodLevel::for_holding_pixels(64.0), LodLevel::FileDetail);
        assert_eq!(LodLevel::for_holding_pixels(900.0), LodLevel::FileDetail);
    }
}
