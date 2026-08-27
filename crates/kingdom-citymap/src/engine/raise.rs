//! Raising a world a slice at a time, so the King can watch it happen.
//!
//! A manifest for a real dev folder is a few thousand holdings, twelve thousand
//! trees and four thousand roads. Spawned in one call that is several seconds
//! of unbroken main-thread work, measured in blocks of well over a second at a
//! time -- and during it the browser cannot paint, cannot run a timer, and
//! cannot advance an animation that is not already on the compositor.
//!
//! That is the whole reason this module exists. A loading bar drawn over a
//! build like that would freeze at whatever fraction it last managed to show,
//! which reads as a hung application -- the exact impression the loading card
//! was added to remove. So the build is cut into slices with a deadline, the
//! engine yields between them, and the fraction is published to the bridge on
//! the way past.
//!
//! # What is counted
//!
//! [`Step`] is the private unit of work and [`RaiseStage`] is what the King is
//! told; several steps can share a stage, because "the ground" is one thing to
//! read and four lists to build.
//!
//! The bar is weighted by [`Step::weight`] rather than by a plain count of
//! items, because the items are not alike: a road generates a ribbon mesh and a
//! tree reuses one that already exists. The weights are **estimates**, and the
//! design assumes so -- being wrong makes the bar advance unevenly, which is
//! all it makes wrong. It cannot stall the build, and [`RaisePlan::fraction`]
//! is pinned to reach exactly 1.0 whatever they say.

use bevy::platform::time::Instant;
use bevy::prelude::*;
use core::ops::Range;
use core::time::Duration;

use crate::map::{MapManifest, MapWorld};

use super::activity::Activity;
use super::bridge::{Bridge, RaiseStage, Raising};
use super::camera::{CameraRig, MapCamera};
use super::lod::ActiveLod;
use super::materials::MaterialCache;
use super::spawn::{self, LoadedMap, MeshCache};

/// How long one frame may spend *issuing* the world's building work.
///
/// The budget is checked *between* slices, not inside one, so a slice may
/// overrun it -- this bounds when the engine stops starting new work rather
/// than when it stops working.
///
/// # Why this alone was not enough
///
/// It bounds the wrong half of the cost, and measured in a browser the
/// difference is enormous: with only this in force, a frame that spent its
/// eight milliseconds issuing work then ran for **2,694 ms**.
///
/// Building a holding queues a `Commands` spawn and adds a mesh to `Assets`.
/// Neither is the expensive part. The expensive part is what the rest of the
/// frame then has to do with them -- applying the commands, and preparing every
/// new mesh for the GPU -- and all of it happens *after* this deadline has been
/// checked for the last time. Eight milliseconds of issuing can buy seconds of
/// consequence, and a bar cannot be painted inside a frame that is still
/// running.
///
/// So this stays as the cap on a single burst, and [`Job::allowance`] is what
/// actually holds a frame to a length the browser can draw in.
const FRAME_BUDGET: Duration = Duration::from_millis(8);

/// How long a whole frame should take while a world is going up.
///
/// Not a frame rate: it is the length at which the browser still gets to run
/// the interface's poll and paint the loading bar between one frame and the
/// next. Three ordinary 16 ms frames' worth, which is slow enough that the
/// per-frame overhead of raising in pieces stays a small share of the work, and
/// quick enough that a bar moving at this cadence reads as continuous.
const TARGET_FRAME: Duration = Duration::from_millis(48);

/// Least a frame may spend building.
///
/// In [`Step::weight`] units, like everything else about the allowance: enough
/// for four or five holdings, or three folder names. The floor exists so the
/// controller below cannot throttle a slow machine down to a standstill --
/// whatever the frames cost, the world still goes up.
const MIN_ALLOWANCE: f32 = 24.0;

/// Most a frame may spend building.
///
/// The ceiling is what keeps one frame from swallowing a whole cheap step:
/// scenery weighs one apiece and a kingdom holds twelve thousand pieces of it,
/// so without a cap the controller would climb until a single frame planted
/// every grove -- which is the multi-second frame all of this exists to
/// prevent.
const MAX_ALLOWANCE: f32 = 2_048.0;

/// What the next frame may spend, given what the last one cost.
///
/// A plain feedback loop, and deliberately the simplest one that works: too
/// slow a frame halves the allowance, a comfortably quick one doubles it, and
/// anything in between is left alone so the raise settles rather than
/// oscillating.
///
/// It is a free function so that the arithmetic is testable without an `App`,
/// for the same reason [`RaisePlan`] is free of Bevy.
fn next_allowance(allowance: f32, last_frame: Duration) -> f32 {
    if last_frame > TARGET_FRAME {
        (allowance / 2.0).max(MIN_ALLOWANCE)
    } else if last_frame < TARGET_FRAME / 2 {
        (allowance * 2.0).min(MAX_ALLOWANCE)
    } else {
        allowance
    }
}

/// How long the full bar is left up before the world is revealed.
///
/// Yielding one frame is not enough on its own. The engine runs continuously
/// while a world goes up, so its next frame can begin before the interface's
/// poll has read the full bar, and then the King never sees it: measured, that
/// came out as a coin flip between a bar that finished and one that stopped at
/// 98%. This is a few polls' worth of head start, spent once, at the end of a
/// wait of several seconds.
///
/// It reports nothing and measures nothing -- it is a pause to let something
/// already true be drawn, which is the same trade `view::PAINT_PAUSE_MS` makes
/// at the other end of the wait.
const REVEAL_PAUSE: Duration = Duration::from_millis(64);

/// How many items one slice covers.
///
/// Small enough that a slice cannot blow far past [`FRAME_BUDGET`] on its own,
/// large enough that the per-slice bookkeeping is noise beside the spawning.
const SLICE: usize = 48;

/// One list of the manifest, built in order.
///
/// Finer than [`RaiseStage`] on purpose: the interface wants a phrase the King
/// can read and this wants a list it can index into, and forcing those to be
/// the same enum would mean either four unreadable stage names or one stage
/// that cannot be sliced.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    /// The sun, the disk and the rock beneath it. Not a list -- one unit.
    Base,
    /// Each town's ground and its activity ring.
    Towns,
    /// Each folder's ground and kerb.
    Wards,
    /// The paved squares.
    Plazas,
    /// Every road and path.
    Roads,
    /// Every file, as a building.
    Holdings,
    /// Trees and rim posts.
    Groves,
    /// Folder names painted on the ground.
    Names,
}

impl Step {
    /// Every step, in build order. This *is* the build order.
    pub const ALL: [Self; 8] = [
        Self::Base,
        Self::Towns,
        Self::Wards,
        Self::Plazas,
        Self::Roads,
        Self::Holdings,
        Self::Groves,
        Self::Names,
    ];

    /// What the King is told this step is.
    pub fn stage(self) -> RaiseStage {
        match self {
            Self::Base | Self::Towns | Self::Wards | Self::Plazas => RaiseStage::Ground,
            Self::Roads => RaiseStage::Roads,
            Self::Holdings => RaiseStage::Holdings,
            Self::Groves => RaiseStage::Groves,
            Self::Names => RaiseStage::Names,
        }
    }

    /// How many items of this step there are in a world.
    pub fn count(self, world: &MapWorld) -> usize {
        match self {
            // The disk is a fixed handful of meshes rather than a list, so it
            // is one indivisible item.
            Self::Base => 1,
            Self::Towns => world.towns.len(),
            Self::Wards => world.wards.len(),
            Self::Plazas => world.plazas.len(),
            Self::Roads => world.roads.len(),
            Self::Holdings => world.buildings.len(),
            Self::Groves => world.scenery.len(),
            Self::Names => world.ground_labels.len(),
        }
    }

    /// Roughly what one item of this step costs, relative to a tree.
    ///
    /// Estimates, and read as such -- see the module docs. What they are
    /// ordered by is how much mesh generation an item causes: a tree and a
    /// plaza reuse geometry, a road and a kerb generate a ribbon, a folder name
    /// is turned into strokes, and the disk is the rim, the cliff, the shelf
    /// and the spire together.
    pub fn weight(self) -> f32 {
        match self {
            Self::Base => 40.0,
            Self::Towns => 12.0,
            Self::Wards => 8.0,
            Self::Plazas => 2.0,
            Self::Roads => 6.0,
            Self::Holdings => 5.0,
            Self::Groves => 1.0,
            Self::Names => 8.0,
        }
    }
}

/// What is left to build, and how far through it the engine is.
///
/// Deliberately free of Bevy: it is a cursor and some arithmetic, which is what
/// lets the ordering and the fraction be tested without standing up an `App`.
#[derive(Clone, Debug)]
pub struct RaisePlan {
    /// Each step and how many items it has, in build order.
    steps: Vec<(Step, usize)>,
    /// Which step is being built.
    at: usize,
    /// How far into that step's list the cursor has reached.
    offset: usize,
    /// Weighted work already handed out.
    done: f32,
    /// Weighted work in the whole world. Never negative; may be zero.
    total: f32,
}

impl RaisePlan {
    /// Reads a world and works out what raising it involves.
    pub fn for_world(world: &MapWorld) -> Self {
        let steps: Vec<(Step, usize)> = Step::ALL
            .into_iter()
            .map(|step| (step, step.count(world)))
            .collect();
        let total = steps
            .iter()
            .map(|(step, count)| step.weight() * *count as f32)
            .sum();
        Self {
            steps,
            at: 0,
            offset: 0,
            done: 0.0,
            total,
        }
    }

    /// The next slice of work, or `None` when the world is complete.
    ///
    /// Walks past empty steps rather than returning an empty range for each,
    /// so a world with no scenery does not cost a frame per absent list.
    pub fn take(&mut self) -> Option<(Step, Range<usize>)> {
        self.take_worth(SLICE as f32 * Step::Groves.weight())
    }

    /// The next slice, costing at most `budget` in [`Step::weight`].
    ///
    /// A budget rather than a count of items, because the items are not alike:
    /// a tree reuses a mesh that already exists and a folder name is turned
    /// into strokes. Counted plainly, a frame allowed two thousand items would
    /// plant two thousand trees in 40 ms or paint two thousand names in several
    /// seconds -- and the second is exactly the multi-second frame this budget
    /// exists to prevent. The weights are the estimates the bar already trusts,
    /// used here to spend a frame rather than to fill a bar.
    ///
    /// Always at least one item, whatever the budget: a caller with nothing
    /// left to spend should stop calling, and a plan that could return
    /// `Some(0..0)` would be a way to hang the raise forever.
    pub fn take_worth(&mut self, budget: f32) -> Option<(Step, Range<usize>)> {
        while let Some(&(step, count)) = self.steps.get(self.at) {
            if self.offset >= count {
                self.at += 1;
                self.offset = 0;
                continue;
            }
            let each = step.weight().max(f32::EPSILON);
            let affordable = (budget / each) as usize;
            let start = self.offset;
            let end = (start + affordable.clamp(1, SLICE)).min(count);
            self.offset = end;
            self.done += each * (end - start) as f32;
            return Some((step, start..end));
        }
        None
    }

    /// Whether everything has been handed out.
    pub fn finished(&self) -> bool {
        self.at >= self.steps.len()
    }

    /// Which stage the King should be told about right now.
    ///
    /// The step the cursor is *on*, which after the last slice of a step is
    /// the next one -- the bar and its caption then move together rather than
    /// the caption lagging a frame behind.
    pub fn stage(&self) -> RaiseStage {
        self.steps
            .get(self.at)
            .map(|(step, _)| step.stage())
            // Past the end there is nothing left to name, and the last thing
            // built is the truest thing to say.
            .unwrap_or(RaiseStage::Names)
    }

    /// How much of the world is standing, from 0.0 to 1.0.
    ///
    /// Exactly 1.0 once finished, whatever the weights said: they are
    /// estimates, and a bar that stops at 0.98 because of one is a bug the King
    /// can see.
    pub fn fraction(&self) -> f32 {
        if self.finished() || self.total <= 0.0 {
            return 1.0;
        }
        (self.done / self.total).clamp(0.0, 1.0)
    }
}

/// A world going up: what it is being built from, where, and how far along.
#[derive(Resource, Default)]
pub struct Raise(Option<Job>);

struct Job {
    manifest: Box<MapManifest>,
    root: Entity,
    plan: RaisePlan,
    /// What this frame may spend building, in [`Step::weight`], adapted from
    /// what the last frame cost. See [`next_allowance`].
    ///
    /// Starts at the floor rather than the ceiling on purpose: the first frames
    /// of a raise are the ones the King is most likely to be looking at, and it
    /// takes only a handful of cheap frames to climb from here to whatever the
    /// machine can actually carry.
    allowance: f32,
    /// Whether everything is built and only the reveal is left, and when that
    /// became true.
    ///
    /// The reveal gets a frame of its own -- see the end of [`raise_world`] --
    /// and the instant is how long it has been waiting to take it.
    revealing: Option<Instant>,
    /// When this system last began, so the length of the whole frame -- render
    /// included, not just the part spent here -- can be measured.
    ///
    /// `None` on the first frame of a raise, where there is no previous frame
    /// to have measured.
    started: Option<Instant>,
}

impl Raise {
    /// Whether a world is going up right now.
    pub fn in_flight(&self) -> bool {
        self.0.is_some()
    }

    /// Starts raising a manifest under an already-spawned root.
    pub fn begin(&mut self, manifest: Box<MapManifest>, root: Entity) {
        let plan = RaisePlan::for_world(&manifest.world);
        self.0 = Some(Job {
            manifest,
            root,
            plan,
            allowance: MIN_ALLOWANCE,
            revealing: None,
            started: None,
        });
    }

    /// Abandons whatever was going up, for when a new world replaces it.
    ///
    /// The entities are not despawned here: `spawn::clear_world` takes every
    /// `SceneRoot`, including a half-built one, and having one place that
    /// removes a world is worth more than the symmetry.
    pub fn abandon(&mut self) {
        self.0 = None;
    }
}

/// Builds a slice of the world each frame, and finishes the job on the last.
///
/// The two halves of this are the point: everything before the deadline is
/// spawning, and everything after it is the *reporting* that makes the wait
/// legible. A frame that builds and says nothing is indistinguishable from a
/// frozen one.
#[allow(clippy::too_many_arguments)]
pub fn raise_world(
    mut commands: Commands,
    bridge: Res<Bridge>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut mesh_cache: ResMut<MeshCache>,
    mut material_cache: ResMut<MaterialCache>,
    mut raise: ResMut<Raise>,
    mut loaded: ResMut<LoadedMap>,
    mut rig: ResMut<CameraRig>,
    mut lod: ResMut<ActiveLod>,
    mut working: ResMut<Activity>,
    mut pace: super::Pace,
    standing: Res<super::Standing>,
    mut cameras: Query<&mut Camera, With<MapCamera>>,
    windows: Query<&Window>,
) {
    let Some(job) = raise.0.as_mut() else {
        return;
    };

    // Nothing that is going up can be seen, so nothing that is going up is
    // worth drawing.
    //
    // This is the second half of what makes the bar move, and the half the
    // original slicing was missing. Cutting the build into 8 ms pieces hands
    // the frame back, but the frame it hands back was still spending itself
    // rendering a world behind an opaque-enough card -- the root is spawned
    // `Visibility::Hidden` and the card is drawn over the region -- and a
    // browser busy rendering does not run the interface's poll or paint its
    // bar. Measured before this, the card painted **one** raise reading in a
    // 1.4-second raise, and often none at all.
    //
    // `is_active` is the same switch `ViewerCommand::Show` throws to stand the
    // map down behind a conversation, and its meaning is unchanged: the render
    // graph skips this camera entirely. It is put back at the bottom of this
    // function, on the frame the world is revealed.
    if let Ok(mut camera) = cameras.single_mut() {
        camera.is_active = false;
    }

    // A world under construction is worth every frame the machine will give:
    // moving to a chamber mid-raise drops the engine to a few ticks a second
    // and would turn three seconds of building into a minute of it.
    //
    // The same reasoning is why an automated browser's frame cap does not
    // reach this line -- see `Pace::set_for_work`, where the minute in question
    // was measured at two and a half.
    pace.set_for_work();

    let deadline = Instant::now() + FRAME_BUDGET;

    // What the last frame of this raise cost, all in -- this system, the
    // command queue it filled, the meshes the renderer then had to prepare, and
    // the paint at the end. Measured start-of-system to start-of-system,
    // because the part that hurt was never the part spent in here.
    let now = Instant::now();
    if let Some(started) = job.started.replace(now) {
        job.allowance = next_allowance(job.allowance, now - started);
    }

    let mut left = job.allowance;
    let world = &job.manifest.world;
    while let Some((step, slice)) = job.plan.take_worth(left) {
        left -= step.weight() * slice.len() as f32;
        spawn::spawn_step(
            &mut commands,
            &mut meshes,
            &mut materials,
            &mut mesh_cache,
            &mut material_cache,
            job.root,
            world,
            step,
            slice,
        );
        if left <= 0.0 || Instant::now() >= deadline {
            break;
        }
    }

    if !job.plan.finished() {
        let raising = Raising {
            stage: job.plan.stage(),
            fraction: job.plan.fraction(),
        };
        bridge.update_status(|status| status.raising = Some(raising));
        return;
    }

    // Everything is built. The reveal waits for the bar to be seen full.
    //
    // Not ceremony -- it is what lets the bar finish. Revealing means showing
    // every entity of the new world to a camera that has been switched off for
    // the whole raise, and that frame is the most expensive one there is:
    // measured, **2,835 ms**. Whatever the card is showing when it begins is
    // what the King looks at until it ends, and doing this inline meant that
    // was a bar stopped short of the end -- the work finished, and the last
    // sight of it unfinished.
    //
    // So the last slice publishes a full bar, and the reveal holds off for
    // [`REVEAL_PAUSE`] to let it be drawn. See that constant for why yielding a
    // single frame was not enough.
    let waited = match job.revealing {
        Some(since) => now.saturating_duration_since(since),
        None => {
            job.revealing = Some(now);
            let raising = Raising {
                stage: job.plan.stage(),
                fraction: 1.0,
            };
            bridge.update_status(|status| status.raising = Some(raising));
            Duration::ZERO
        }
    };
    if waited < REVEAL_PAUSE {
        return;
    }

    // The reveal. Everything from here is what `ViewerCommand::Load` used to do
    // inline once the whole world had been spawned.
    let viewport = windows
        .single()
        .map(|window| Vec2::new(window.width(), window.height()))
        .unwrap_or(Vec2::splat(1.0));
    super::fit(&mut rig, world, viewport);
    // Zoom limits and detail tiers are measured against a house, so the
    // reference house is taken once per world. The fitted scale is kept only as
    // the floor on how far back the camera may pull, so that a large world can
    // still be framed whole.
    rig.holding = super::typical_holding(&job.manifest);
    rig.fit_scale = rig.scale;

    // Nothing was shown while it went up, so the King sees a finished kingdom
    // appear rather than trees and roads arriving under a loading card he can
    // see through.
    spawn::reveal(&mut commands, job.root);

    // And the camera comes back to whatever the map's own place justifies --
    // which is not necessarily on. A world raised while the King is in a
    // chamber must finish stood down, exactly as `ViewerCommand::Show` left it.
    if let Ok(mut camera) = cameras.single_mut() {
        camera.is_active = standing.0.showing();
    }

    // Both of these run only when their resource changed, and every entity of
    // this world was spawned after the last change -- so without this the new
    // scenery keeps a detail tier nobody chose and a town reported as working
    // stays dark until the next poll happens to differ.
    lod.set_changed();
    working.set_changed();

    let job = raise.0.take().expect("a job was in flight a moment ago");
    loaded.0 = Some(job.manifest);
    bridge.update_status(|status| {
        status.built = true;
        status.raising = None;
        status.error = None;
        status.hovered = None;
        status.selected_ward = None;
        status.hovered_ward = None;
    });

    // And the engine goes back to whatever pace the King's attention justifies
    // -- which is not necessarily the one this system forced above, because he
    // may have walked into a chamber, or out to the map, while the cities were
    // going up.
    //
    // Read from `Standing` rather than from the camera. `is_active` is one bit
    // and there are three places the map can be, so inferring from it would
    // bring a map that now lives in the rail back running continuously behind
    // a conversation.
    pace.set(standing.0);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::map::{
        MapColor, MapGroundLabel, MapPlaza, MapRect, MapScenery, MapSun, MapUnderside,
    };

    fn plaza() -> MapPlaza {
        MapPlaza {
            rect: MapRect {
                x: 0.0,
                y: 0.0,
                width: 4.0,
                depth: 4.0,
            },
            color: MapColor::default(),
        }
    }

    fn scenery() -> MapScenery {
        MapScenery::Post {
            position: [0.0, 0.0],
            height: 1.0,
            color: MapColor::default(),
        }
    }

    fn label() -> MapGroundLabel {
        MapGroundLabel {
            ward_id: "ward-0".to_owned(),
            text: "src".to_owned(),
            origin: [0.0, 0.0],
            size: 2.0,
            max_width: 10.0,
            stroke: 0.2,
            vertical: false,
            color: MapColor::default(),
            depth: 0,
            min_pixel_height: 4.0,
        }
    }

    fn world(plazas: usize) -> MapWorld {
        MapWorld {
            bounds: MapRect {
                x: 0.0,
                y: 0.0,
                width: 100.0,
                depth: 100.0,
            },
            space: MapColor::default(),
            ground: MapColor::default(),
            rim: Vec::new(),
            underside: MapUnderside {
                cliff: 4.0,
                shelf: 8.0,
                taper: 0.5,
                depth: 40.0,
                cliff_color: MapColor::default(),
                rock: MapColor::default(),
                deep: MapColor::default(),
            },
            sun: MapSun {
                direction: [0.0, -1.0, 0.0],
                color: MapColor::default(),
                illuminance: 9000.0,
                ambient: MapColor::default(),
                ambient_brightness: 420.0,
            },
            towns: Vec::new(),
            wards: Vec::new(),
            plazas: (0..plazas).map(|_| plaza()).collect(),
            roads: Vec::new(),
            buildings: Vec::new(),
            scenery: Vec::new(),
            ground_labels: Vec::new(),
        }
    }

    /// Every item is built exactly once, in order, whatever the slice size.
    /// A gap here is a hole in the map that nothing else would report.
    #[test]
    fn every_item_is_built_once_and_in_order() {
        let world = world(SLICE * 2 + 7);
        let mut plan = RaisePlan::for_world(&world);

        let mut seen: Vec<(Step, usize)> = Vec::new();
        while let Some((step, slice)) = plan.take() {
            assert!(
                !slice.is_empty(),
                "an empty slice costs a frame for nothing"
            );
            for index in slice {
                seen.push((step, index));
            }
        }

        for step in Step::ALL {
            let built: Vec<usize> = seen
                .iter()
                .filter(|(each, _)| *each == step)
                .map(|(_, index)| *index)
                .collect();
            let wanted: Vec<usize> = (0..step.count(&world)).collect();
            assert_eq!(
                built, wanted,
                "{step:?} was not built exactly once, in order"
            );
        }

        // And the steps themselves came in build order.
        let order: Vec<Step> = Step::ALL
            .into_iter()
            .filter(|step| step.count(&world) > 0)
            .collect();
        let mut walked: Vec<Step> = Vec::new();
        for (step, _) in &seen {
            if walked.last() != Some(step) {
                walked.push(*step);
            }
        }
        assert_eq!(walked, order);
    }

    #[test]
    fn the_bar_only_ever_moves_forwards_and_reaches_the_end() {
        let world = world(200);
        let mut plan = RaisePlan::for_world(&world);

        let mut last = plan.fraction();
        assert!(last < 1.0, "nothing is standing yet");
        while plan.take().is_some() {
            let now = plan.fraction();
            assert!(now >= last, "the bar went backwards: {last} then {now}");
            assert!((0.0..=1.0).contains(&now), "the bar left its track: {now}");
            last = now;
        }
        assert!(plan.finished());
        assert_eq!(
            plan.fraction(),
            1.0,
            "a finished world must read as finished"
        );
    }

    /// A kingdom with nothing in it still has a disk, and must still finish --
    /// the card hangs off this and a stalled plan would never take it down.
    #[test]
    fn an_all_but_empty_world_finishes_without_dividing_by_zero() {
        let mut plan = RaisePlan::for_world(&world(0));
        assert_eq!(plan.take(), Some((Step::Base, 0..1)));
        assert_eq!(plan.take(), None);
        assert!(plan.finished());
        assert_eq!(plan.fraction(), 1.0);
    }

    #[test]
    fn the_caption_keeps_up_with_the_bar() {
        let world = world(SLICE + 1);
        let mut plan = RaisePlan::for_world(&world);

        assert_eq!(plan.stage(), RaiseStage::Ground, "the disk comes first");
        while plan.take().is_some() {
            // Nothing after the ground exists in this world, so every stage
            // named while it is going up must be the ground's.
            if !plan.finished() {
                assert_eq!(plan.stage(), RaiseStage::Ground);
            }
        }
    }

    /// The two lists are walked in lockstep by `Step::stage`, and a stage no
    /// step reaches would be a phrase the King is never shown.
    #[test]
    fn every_stage_is_reached_by_some_step() {
        for stage in RaiseStage::ALL {
            assert!(
                Step::ALL.into_iter().any(|step| step.stage() == stage),
                "nothing builds {stage:?}"
            );
        }
    }

    /// A frame may stop part-way through a slice, and doing so must not skip,
    /// repeat or reorder anything.
    ///
    /// This is what lets a frame's allowance be spent in whatever the next step
    /// happens to cost, which matters because the cheap steps have thousands of
    /// items each: a budget that could only land on slice boundaries would be
    /// no budget at all for a grove.
    #[test]
    fn a_frame_takes_only_what_its_budget_affords() {
        let world = world(100);
        let mut plan = RaisePlan::for_world(&world);

        // The disk is one indivisible item, whatever it weighs.
        assert_eq!(plan.take_worth(1.0), Some((Step::Base, 0..1)));

        // A plaza weighs 2, so 14 buys seven of them.
        assert_eq!(Step::Plazas.weight(), 2.0);
        assert_eq!(plan.take_worth(14.0), Some((Step::Plazas, 0..7)));
        assert_eq!(plan.take_worth(14.0), Some((Step::Plazas, 7..14)));

        // A budget beyond what is left is capped by what is left -- and by
        // `SLICE`, so no single call can run away with a whole step.
        assert_eq!(plan.take_worth(100_000.0), Some((Step::Plazas, 14..62)));
    }

    /// The same budget buys fewer of an expensive thing than of a cheap one.
    ///
    /// This is the whole reason the allowance is weighed rather than counted.
    /// A frame allowed two thousand *items* would plant two thousand trees in
    /// no time or paint two thousand names in several seconds, and the second
    /// is the multi-second frame that froze the bar.
    #[test]
    fn an_expensive_step_costs_a_frame_more_of_its_budget() {
        let cheap = {
            let mut world = world(0);
            world.scenery = vec![scenery(); SLICE];
            let mut plan = RaisePlan::for_world(&world);
            plan.take_worth(48.0); // the disk
            plan.take_worth(48.0)
        };
        let dear = {
            let mut world = world(0);
            world.ground_labels = vec![label(); SLICE];
            let mut plan = RaisePlan::for_world(&world);
            plan.take_worth(48.0); // the disk
            plan.take_worth(48.0)
        };

        let (_, cheap) = cheap.expect("there is scenery to plant");
        let (_, dear) = dear.expect("there are names to paint");
        assert_eq!(cheap.len(), 48, "a tree weighs one");
        assert_eq!(dear.len(), 6, "a name weighs eight");
    }

    /// Asking for nothing must still make progress.
    ///
    /// A budget of zero that returned an empty range would be a raise that
    /// never ends and a loading card that never comes down -- the worst failure
    /// this module has, and one arithmetic slip away.
    #[test]
    fn a_frame_with_nothing_left_to_spend_still_cannot_stall_the_raise() {
        let mut plan = RaisePlan::for_world(&world(3));
        let mut taken = 0;
        while let Some((_, slice)) = plan.take_worth(0.0) {
            assert!(!slice.is_empty(), "an empty slice would loop forever");
            taken += slice.len();
            assert!(taken <= 4, "more was built than the world holds");
        }
        assert!(plan.finished());
    }

    /// The controller reacts to what a frame actually cost, in the direction
    /// that makes the bar paintable, and settles rather than oscillating.
    #[test]
    fn the_allowance_follows_what_the_frames_cost() {
        // A frame that overran is halved.
        assert_eq!(next_allowance(1_000.0, TARGET_FRAME * 4), 500.0);
        // A comfortably quick one is doubled.
        assert_eq!(next_allowance(100.0, TARGET_FRAME / 4), 200.0);
        // And one that landed near the target is left alone, so a raise that
        // has found its pace keeps it.
        assert_eq!(next_allowance(100.0, TARGET_FRAME), 100.0);
        assert_eq!(
            next_allowance(100.0, TARGET_FRAME / 2 + Duration::from_millis(1)),
            100.0
        );
    }

    /// Neither end of the controller may run away.
    ///
    /// The floor is what stops a slow machine being throttled to a standstill;
    /// the ceiling is what stops a cheap step -- twelve thousand trees, at a
    /// weight of one each -- being planted in a single multi-second frame.
    #[test]
    fn the_allowance_stays_between_its_two_bounds() {
        let mut starved = MIN_ALLOWANCE;
        for _ in 0..20 {
            starved = next_allowance(starved, TARGET_FRAME * 10);
        }
        assert_eq!(starved, MIN_ALLOWANCE);

        let mut greedy = MIN_ALLOWANCE;
        for _ in 0..40 {
            greedy = next_allowance(greedy, Duration::ZERO);
        }
        assert_eq!(greedy, MAX_ALLOWANCE);
    }
}
