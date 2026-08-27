//! The channel between the Leptos interface and the rendering engine.
//!
//! Leptos owns the DOM chrome — search, the inspector, the minimap, the
//! toolbar — while the engine owns the map. Neither can borrow the other's
//! state directly, so they meet here: Leptos pushes commands, the engine
//! publishes what it is currently showing, and Leptos polls that back into
//! signals.

use std::sync::{Arc, Mutex};

use crate::map::{MapManifest, MapPresence, Work};
use bevy::prelude::*;

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
    /// Centre on a world point and zoom in far enough to read what stands
    /// there.
    ///
    /// This is what opening a file in a chamber sends. It was a bare
    /// `LookAt` -- centre, keep the zoom -- which slid a town-wide frame
    /// sideways and left the King looking at the coarsest detail tier, where a
    /// house is twenty pixels and nothing is labelled. Pointing the map at one
    /// file now means arriving at it, not merely aiming at it.
    ///
    /// Whether to glide there or cut is the sender's call, and it is a
    /// question about *distance travelled* rather than about taste. Moving
    /// between two files of one project is a short hop the eye can follow, and
    /// a glide is what keeps the King oriented -- he sees which way the camera
    /// went, so the new building arrives somewhere rather than merely
    /// replacing what was there. Arriving in a different city is not a
    /// journey worth animating: the whole frame changes, so a tween across it
    /// is a smear rather than a movement.
    ///
    /// The engine cannot answer that itself -- it does not know what a city
    /// is, deliberately, which is why the flag arrives already decided. See
    /// the pointer effect in `view.rs`.
    ///
    /// This used to be a cut in every case, on the grounds that the rail's map
    /// ticks at `engine::RAIL_WAKE` and a tween would be animated at eight
    /// frames a second. That reasoning was sound and is now answered rather
    /// than ignored: a glide forces the engine to a continuous pace for the
    /// quarter second it lasts, exactly as `raise::raise_world` does for the
    /// length of a raise.
    Inspect {
        /// The world point to centre on.
        point: [f32; 2],
        /// Whether to travel there over [`super::camera::GLIDE_SECONDS`]
        /// rather than arriving at once.
        glide: bool,
    },
    /// Hand the camera back to whatever the interface wants it pointed at.
    ///
    /// The counterpart to a takeover: dragging or scrolling the map suspends
    /// the following (see [`super::input::Steering`]), and this ends that
    /// suspension. Sent by the King pressing the free-look chip, and by the
    /// map changing home -- a camera framed for the whole region is simply
    /// wrong in a pane at the foot of the rail, so a re-fit there is fitting
    /// rather than following.
    ReleaseCamera,
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
    /// What the open plan is proposing, as ground to raise works on.
    ///
    /// Replaces rather than amends, for [`Self::SetActivity`]'s reason and one
    /// more: a file the court has since reverted must *stop* being drawn, and
    /// an amending command has no way to say that. An empty list is the ordinary
    /// way to say "nothing is under construction" -- it is what leaving a
    /// chamber sends.
    ///
    /// Everything here is already world-space geometry. The resolving from a
    /// changed file to a rectangle happens in `view.rs`, which is the boundary
    /// the engine's ignorance of Kingdom's domain is kept at -- exactly as
    /// [`TownActivity`] is a bare name and a count rather than a `CityId`.
    SetWorks(Vec<Work>),
    /// Where the map is standing, and therefore how hard it should work.
    ///
    /// The map is mounted once for the life of the page -- see
    /// `kingdom_app::app::ThroneRoom` for why it may never unmount -- and moves
    /// between rectangles rather than between screens. But a rectangle it is
    /// not being looked at through still costs everything to draw:
    /// `visibility: hidden` stops the pixels reaching the screen, not the work
    /// of producing them, and the engine would go on running its render graph
    /// over every building on the island behind a conversation for as long as
    /// that conversation lasted.
    ///
    /// Three states rather than two, because there are now genuinely three.
    /// See [`MapPresence`], and the arm in `engine::apply_commands` for what
    /// each one costs.
    Show(MapPresence),
}

impl ViewerCommand {
    /// Whether obeying this would move the camera.
    ///
    /// Asked by `engine::apply_commands` so that a camera glide in flight is
    /// abandoned by anything that re-aims the camera itself: a tween has a
    /// quarter second of writing left in it, and without this it would spend
    /// that undoing whatever was just asked for. [`Self::Load`] counts --
    /// a world going up ends with its own `fit()`, and a glide over the world
    /// that was torn down means nothing against the one replacing it.
    ///
    /// [`Self::Inspect`] is deliberately absent: it is the one command that
    /// *starts* a journey, and its own arm decides what to do with the one
    /// already running.
    pub fn moves_the_camera(&self) -> bool {
        matches!(
            self,
            Self::Load(_) | Self::Fit | Self::ZoomBy(_) | Self::ActualSize | Self::Focus { .. }
        )
    }
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

/// One stage of raising a world, in the order they are built.
///
/// This is the unit the loading bar counts in, which is why it is a public
/// enum rather than an implementation detail of the spawner: the interface
/// names the stage it is watching, so the King reads "Laying the roads" rather
/// than a bare percentage.
///
/// The order here is the order [`super::raise`] walks them, and [`Self::ALL`]
/// is what pins that -- a stage added to one and not the other would silently
/// never be built.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum RaiseStage {
    /// The disk, its underside, the towns, the folders and the plazas.
    Ground,
    /// Every path and road.
    Roads,
    /// Every file, as a building.
    Holdings,
    /// Trees and rim posts.
    Groves,
    /// Folder names painted onto the ground.
    Names,
}

impl RaiseStage {
    /// Every stage, in build order.
    pub const ALL: [Self; 5] = [
        Self::Ground,
        Self::Roads,
        Self::Holdings,
        Self::Groves,
        Self::Names,
    ];

    /// What the loading card calls this stage.
    ///
    /// The King's vocabulary rather than the code's -- and on this map the
    /// metaphor is also the literal subject matter, so no translation is being
    /// invented here.
    pub fn label(self) -> &'static str {
        match self {
            Self::Ground => "Laying the ground",
            Self::Roads => "Laying the roads",
            Self::Holdings => "Raising the holdings",
            Self::Groves => "Planting the groves",
            Self::Names => "Painting the names",
        }
    }
}

/// How far through building a world the engine is.
///
/// Published on every slice so the loading card can draw a bar that actually
/// moves. See [`super::raise`] for why a world is built in slices at all:
/// built in one go it blocks the main thread for seconds, and a bar that
/// cannot be repainted is worse than no bar.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Raising {
    /// Which stage is going up right now.
    pub stage: RaiseStage,
    /// How much of the whole world is standing, from 0.0 to 1.0.
    pub fraction: f32,
}

/// What the engine is currently showing.
#[derive(Clone, Debug, Default)]
pub struct ViewerStatus {
    /// Whether the engine has started at all.
    ///
    /// False from the moment the interface is created until the engine's
    /// `Startup` runs, which on the web is a genuine wait rather than a
    /// formality: booting Bevy means asking for a GPU adapter and a device and
    /// compiling the first pipelines, and a `Load` sent before that lands sits
    /// in the command queue until it is over.
    ///
    /// The loading card is what needs the distinction. Without it "the engine
    /// has not woken yet" and "the engine has the manifest and the first slice
    /// is pending" are the same absence of a [`Self::raising`], and the card
    /// announced work that had not started -- measured at up to 1.4 seconds of
    /// "Raising the cities" while nothing was being raised.
    pub awake: bool,
    /// Whether a world has been built and drawn.
    ///
    /// False from boot until the first [`ViewerCommand::Load`] has been
    /// spawned and framed, which is what the loading card watches: the
    /// manifest arriving is not the same moment as the settlement standing up,
    /// and building one of a few thousand holdings blocks a frame.
    pub built: bool,
    /// How far through raising a world, while one is going up.
    ///
    /// `None` before the first [`ViewerCommand::Load`] and again once the world
    /// stands, so "nothing is being built" and "a build has just started" stay
    /// different answers -- the card shows an indeterminate bar for the first
    /// and a bar at its start for the second.
    pub raising: Option<Raising>,
    /// The holding under the pointer.
    pub hovered: Option<String>,
    /// The holding the pointer last *clicked*, and which click that was.
    ///
    /// A click is reported by the engine rather than reconstructed outside it,
    /// and that is the whole point. Selection used to be a DOM `click` handler
    /// on the canvas paired with whatever [`Self::hovered`] happened to say --
    /// but that hover reaches the interface through a 50 ms poll, so a click
    /// arriving sooner than one poll after the pointer moved selected the wrong
    /// thing or nothing at all. A person clicking quickly hit it; a synthetic
    /// click, which moves and presses in the same instant, hit it every time.
    ///
    /// The serial is what makes clicking the *same* holding twice two events.
    /// Without it the second click leaves the status identical, `status_matches`
    /// reports no change, and the revision never moves -- so the interface
    /// never hears about it.
    pub clicked: Option<(String, u64)>,
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
    /// Whether the King has taken the camera by hand.
    ///
    /// Set the moment a drag pans or a wheel zooms, and cleared by
    /// [`ViewerCommand::ReleaseCamera`] or by `input::RELEASE_AFTER` of
    /// stillness. While it is set the interface stops re-framing the map on
    /// the selected city and the open file -- see the two focus effects in
    /// `view.rs` -- and draws the chip that says so.
    pub manual: bool,
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
        && left.awake == right.awake
        && raising_matches(left.raising, right.raising)
        && left.hovered == right.hovered
        && left.clicked == right.clicked
        && left.hovered_ward == right.hovered_ward
        && left.selected_ward == right.selected_ward
        && left.lod == right.lod
        && left.manual == right.manual
        && left.error == right.error
        && (left.zoom - right.zoom).abs() < 0.005
        && left
            .camera_rect
            .iter()
            .zip(right.camera_rect.iter())
            .all(|(a, b)| (a - b).abs() < 0.5)
}

/// How far the raise fraction must move before the interface is woken.
///
/// A world is tens of thousands of entities and the bar is at most a few
/// hundred pixels wide, so a slice that advanced it by a thousandth would
/// repaint nothing. This is the same bargain the camera rect's tolerance
/// strikes above, in the units this field is measured in.
const RAISE_STEP: f32 = 0.005;

/// Whether two raise readings say the same thing.
///
/// Starting and finishing are always a change, whatever the fractions were:
/// `None` is what the card reads as "nothing is going up", and rounding that
/// together with a build at 0% would leave the bar indeterminate for the whole
/// of the first slice.
fn raising_matches(left: Option<Raising>, right: Option<Raising>) -> bool {
    match (left, right) {
        (None, None) => true,
        (Some(left), Some(right)) => {
            left.stage == right.stage && (left.fraction - right.fraction).abs() < RAISE_STEP
        }
        _ => false,
    }
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

    /// Clicking the same holding twice must read as two clicks.
    ///
    /// The interface only sees a status whose revision moved, so without the
    /// serial the second click leaves `clicked` byte-identical, the revision
    /// stands still, and the click is never delivered. That is not a corner
    /// case: re-selecting the city you just cleared is the ordinary way to use
    /// the map.
    #[test]
    fn clicking_the_same_holding_again_is_a_new_click() {
        let bridge = Bridge::new();

        let click = |id: &str| {
            let id = id.to_owned();
            bridge.update_status(|status| {
                let serial = status.clicked.as_ref().map_or(0, |(_, n)| n + 1);
                status.clicked = Some((id, serial));
            });
            bridge.revision()
        };

        let first = click("file-3");
        let again = click("file-3");

        assert_ne!(first, again, "the second click was never delivered");
        assert_eq!(bridge.status().clicked, Some(("file-3".to_owned(), 1)));
    }

    #[test]
    fn a_raise_wakes_the_interface_when_it_actually_moves() {
        let bridge = Bridge::new();
        assert!(bridge.status().raising.is_none(), "nothing goes up at boot");

        // Starting is always a change: `None` reads as "nothing is being
        // built", and a build at 0% is a different thing to say.
        let start = bridge.revision();
        bridge.update_status(|status| {
            status.raising = Some(Raising {
                stage: RaiseStage::Ground,
                fraction: 0.0,
            });
        });
        let begun = bridge.revision();
        assert_ne!(start, begun);

        // One entity out of twenty thousand moves nothing on screen.
        bridge.update_status(|status| {
            status.raising = Some(Raising {
                stage: RaiseStage::Ground,
                fraction: 0.0005,
            });
        });
        assert_eq!(bridge.revision(), begun);

        bridge.update_status(|status| {
            status.raising = Some(Raising {
                stage: RaiseStage::Ground,
                fraction: 0.08,
            });
        });
        let moved = bridge.revision();
        assert_ne!(begun, moved);

        // The stage names what the King is reading, so a new one is a change
        // even if the bar has barely advanced.
        bridge.update_status(|status| {
            status.raising = Some(Raising {
                stage: RaiseStage::Roads,
                fraction: 0.0801,
            });
        });
        let staged = bridge.revision();
        assert_ne!(moved, staged);

        // And finishing must be heard, or the bar would stop at whatever
        // fraction the last slice left it showing.
        bridge.update_status(|status| status.raising = None);
        assert_ne!(staged, bridge.revision());
    }

    #[test]
    fn every_stage_has_something_to_call_itself() {
        // The card renders this string, so an unnamed stage would be a blank
        // line where the King is told what is happening.
        for stage in RaiseStage::ALL {
            assert!(!stage.label().trim().is_empty(), "{stage:?} has no name");
        }
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

    /// The chip is drawn from this flag, so a takeover the revision never
    /// moves for is a takeover the King is never told about -- the trap
    /// `status_matches` is annotated for, on the newest field.
    #[test]
    fn taking_the_camera_wakes_the_interface() {
        let bridge = Bridge::new();
        assert!(!bridge.status().manual, "the map follows at boot");

        let start = bridge.revision();
        bridge.update_status(|status| status.manual = true);
        let taken = bridge.revision();
        assert_ne!(start, taken);

        // And handing it back, or the chip would stay on screen for the life
        // of the page.
        bridge.update_status(|status| status.manual = false);
        assert_ne!(taken, bridge.revision());
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
