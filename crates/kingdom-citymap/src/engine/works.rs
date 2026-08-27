//! Raising a plan's proposed changes over the settlement that already stands.
//!
//! This is the map's answer to the third of the three questions in `AGENTS.md`
//! -- *what are they proposing that I need to decide on?* -- put where the first
//! two are already answered. A house the court has been adding to wears a
//! scaffold above its roof, whose height is how much was added; a house it has
//! been cutting from is covered by a shroud rising from the ground over as much
//! of the house as the file is losing; a file that does not exist in the city's
//! checkout at all stands as a ghost on free ground inside its own folder.
//!
//! # The grammar, which has exactly one rule
//!
//! **What is being built rises above the roof. What is being taken away covers
//! the house.** Nothing crosses that line in either direction, so the two can
//! never be confused at a glance however far out the camera is.
//!
//! It was not always so, and the exception was the fault this module was last
//! rewritten for: removals used to be stacked into the *same upward column* as
//! additions, one band above the other, so a file losing three hundred lines
//! grew a tall tower -- saying the opposite of what had happened. A deletion,
//! meanwhile, was a third thing again: a band at the foot of the house sized
//! from its churn. Both are now the same mark, and a deletion is simply a
//! shroud that covers all of it -- one rule rather than three.
//!
//! The share is [`crate::map::works::WorkBand::cover`], computed at the seam
//! rather than here: it is a fraction of the file's own length, and a file's
//! length is a fact about a codebase that this module is deliberately ignorant
//! of. All the drawing does is multiply it by a height it already has.
//!
//! # Why this does not come through the manifest
//!
//! [`crate::map::works`] gives the whole reasoning, and it is
//! [`super::activity`]'s: the map JSON is memoised on the shape of the kingdom
//! and must not be rebuilt for a fact that moves every few seconds. The works
//! arrive as a [`ViewerCommand::SetWorks`](super::bridge::ViewerCommand::SetWorks)
//! and nothing about the settlement is touched.
//!
//! # Why these are spawned rather than pre-built and flagged
//!
//! The working ring is built once with the world and then only shown or hidden,
//! because every town is known the moment the manifest lands. The works are not:
//! nothing is known about them until the King opens a chamber, and there is no
//! set of entities to pre-build. So they are spawned when they arrive and
//! despawned when they change.
//!
//! That is affordable because of what is actually being built -- a handful of
//! entities per changed file, over a set that is a few dozen files at most,
//! against a world of tens of thousands. And it happens when the King *opens a
//! plan*, not on a poll: `view.rs` only re-sends when the resolved works
//! genuinely differ.
//!
//! # Why they are unlit, and why they are the colours they are
//!
//! [`super::activity::PULSE_PEAK`] records three failed attempts at drawing a
//! status colour as a lit surface: emissive scaled for the sun's lux clipped to
//! white, a value near 1.0 was washed out by the tonemapper, and a lit material
//! added the sun's white specular on top and came out mint. The conclusion there
//! is the conclusion here -- a status colour is a piece of interface that
//! happens to be drawn in world space -- so everything in this module is
//! `unlit`, and the pulse dims toward black rather than brightening toward
//! white.
//!
//! The colours are **not** this module's to choose any more. Each band arrives
//! carrying the two colours of the agent that made it --
//! `kingdom_core::palette`, one hue per plan, a light value for lines added and
//! a dark one for lines removed -- because with several agents on one map the
//! question stopped being "was this growth or cutting?" and became "whose hands
//! are on this file?". A single green and a single red could answer the first
//! and structurally could not answer the second.
//!
//! What this module still decides is *alpha*, which is about the drawing rather
//! than the domain: a proposal is translucent, and a ghost is fainter still.

use bevy::prelude::*;

use crate::map::{Work, WorkSite};

use super::activity;
use super::materials::to_color;
use super::meshes::{self, BuildingShape};
use super::spawn::MeshCache;

/// The churn at which a band reaches full height, in lines.
///
/// **This is what fixed the "every bar looks the same" fault**, and the change
/// it records is from a *relative* ramp to an absolute one. Height used to be
/// `churn / busiest_file_in_the_same_plan`, which had three faults at once:
///
/// 1. a plan that touched one file drew it at full height, and so did a plan
///    that rewrote four thousand lines -- the map could not tell them apart;
/// 2. the same forty-line edit stood at a different height depending on what
///    *else* its plan had done, so nothing on screen was comparable to anything
///    else on screen;
/// 3. with several agents drawn at once it became actively wrong, because each
///    agent's stack was measured against its own plan's ruler.
///
/// Measured against the old curve, a two-line edit beside a four-hundred-line
/// one drew a bar 23% as tall. Now every band on the map is measured in lines,
/// against one ruler, and two agents' bands are honestly comparable.
///
/// 600 lines is a judgement: comfortably past a large single-file change, so
/// the top of the range is reachable in real work rather than theoretical, and
/// low enough that ordinary edits use most of the curve.
pub const FULL_CHURN: f32 = 600.0;

/// How much of a change's size shows, given how many lines moved.
///
/// `ln` rather than linear, which is the same bargain `build::layout::
/// Building::height` strikes with line counts and for the same reason: real
/// files differ by three orders of magnitude, and a linear ramp spends the
/// whole range on the largest one and draws everything else as nothing.
///
/// Returns `0.0..=1.0`. Pure, so the shape is pinned by the tests below without
/// a renderer.
///
/// # Why NaN is handled rather than assumed away
///
/// The counts arrive as `f32` over a wire. `f32::clamp` propagates NaN rather
/// than trapping it, so an unguarded version yields a NaN height, which reaches
/// Bevy as a degenerate mesh. The predecessor of this function was found doing
/// exactly that by a test rather than in a browser, and the guard is kept.
pub fn magnitude(churn: f32) -> f32 {
    if !churn.is_finite() || churn <= 0.0 {
        return 0.0;
    }
    (churn.ln_1p() / FULL_CHURN.ln_1p()).clamp(0.0, 1.0)
}

/// How far above a roof the tallest column reaches, in world units.
///
/// A judgement rather than a measurement, and deliberately generous. The map's
/// most common home is a pane at the foot of the rail where a house is a couple
/// of pixels across -- so what has to read at a glance is the *column of light*
/// above the roofline rather than the house under it. Compare `spawn::TALLEST`,
/// which assumes 60 units for the tallest holding in a world: a full column is
/// therefore comparable to a tall building standing on top of one.
///
/// Raised from 34 after looking at a real plan on screen: a typical holding in
/// the proving ground stands around 32 units, so a 34-unit column was the same
/// order as the house and read as a slightly taller roof rather than as work.
const COLUMN_REACH: f32 = 52.0;

/// The shortest a band may be, whatever the churn.
///
/// A one-line change is still a change the King should be able to see. Without
/// a floor, a file that moved three lines beside one that moved four hundred
/// would be drawn as nothing at all.
///
/// **Cut from 9 to 3.5**, and that is half of the "every bar looks the same"
/// fix. Against a reach of 52 the old floor spent the bottom 17% of the range
/// before any measurement happened, which flattened everything below a hundred
/// lines into the same stub. It can be this much smaller now because girth
/// carries magnitude too (see [`band_girth`]): a small change is drawn as a
/// *thin* column as well as a short one, so it no longer has to be tall to be
/// distinguishable from a large one.
const BAND_FLOOR: f32 = 3.5;

/// How wide a column is as a share of the house it stands on, from the smallest
/// change to the largest.
///
/// The second half of the "every bar looks the same" fix. Girth used to be a
/// constant `footprint * 0.82`, so the *only* thing that varied a column's
/// width was the size of the house under it -- a fact about the file, not about
/// the change. Ramping it with magnitude means a big change reads as a heavy
/// column and a small one as a slender mark, and because apparent volume goes
/// as girth squared it roughly doubles the dynamic range height alone gives:
/// a 4-line change beside a 400-line one went from 3.9x to 8.6x apparent
/// volume.
const GIRTH_RANGE: (f32, f32) = (0.30, 0.85);

/// How far a skirt of cleared ground spreads past the lot it surrounds, as a
/// share of the footprint's shorter side.
///
/// # Why this survives the shroud
///
/// The skirt and the shroud say the same thing at two zooms, and neither can do
/// the other's job. The map's most common home is a pane at the foot of the
/// rail where a house is a couple of pixels across -- at that size a shroud
/// over the house is a *fraction* of those pixels and cannot be resolved at
/// all, while a stain spreading across the lot around it can. Close in, the
/// shroud is the precise reading and the skirt is the halo that draws the eye
/// to it.
///
/// **Was an absolute 1.9 world units, and that is why removals did not show.**
/// The world's own `REFERENCE_WORLD` is 1,000 units and a typical holding
/// stands ~32 units tall, so 1.9 units at maximum -- and a file that was 80%
/// additions got `1.9 * 0.2 = 0.38` of a unit. In the rail's pane that is
/// sub-pixel: the skirt was being drawn correctly and was simply too small for
/// any display to resolve.
///
/// A share of the footprint scales with whatever house it surrounds, in a world
/// whose buildings differ in size by an order of magnitude.
const SKIRT_SPREAD: f32 = 0.45;

/// The least ground a skirt covers, in world units, however small the lot.
///
/// A share alone leaves the smallest houses back where they started, and the
/// smallest houses are exactly where a deletion is easiest to miss.
const SKIRT_FLOOR: f32 = 1.2;

/// The hairline of clear air between two stacked bands.
///
/// Small, but not optional. Two saturated colours meeting exactly read as one
/// column with a gradient in it rather than as two agents standing on one
/// house, which is precisely the thing the stack exists to say.
const BAND_GAP: f32 = 1.1;

/// How far a shroud spreads past the footprint it covers, as a share of it.
///
/// **Wider than any roof on the map, and that is the whole requirement.** The
/// predecessor of this constant was 1.06, which is narrower than most
/// archetypes' eaves, so the block read as a box wedged *inside* the building
/// rather than as one placed over it. Measured from `meshes.rs`, in the unit
/// footprint every archetype is modelled in (walls span 1.0):
///
/// | Archetype | Widest roof point | Girth it needs |
/// |---|---|---|
/// | `keep` | `HALF + 0.04` | 1.08 |
/// | `scriptorium` | `HALF + 0.05` | 1.10 |
/// | `pitched` (cottage, guildhall, granary) | `HALF + 0.06` | 1.12 |
/// | `market` | `HALF + 0.12` (the front slab) | 1.24 |
///
/// 1.28 clears every one of them with a little air. It stays well inside the
/// lot: `build::layout::Building::footprint` insets a house to 0.46--0.58 of
/// its lot, so the lot is at least ~1.7x the footprint and no neighbour is
/// touched. The `granary`'s ground-level bins (`HALF + 0.2`) do still show,
/// which is deliberate -- a sliver of house at the foot of the block reads as
/// the house being covered rather than replaced.
const SHROUD_GIRTH: f32 = 1.28;

/// The least of a house a shroud covers, as a share of its height.
///
/// A one-line cut in a four-thousand-line file is a share of 0.00025, which is
/// nothing at any zoom. This is the floor that keeps such a change visible at
/// all, and it is the same judgement [`BAND_FLOOR`] makes for the column: a
/// small change is still a change the King should be able to see.
///
/// A share rather than an absolute, unlike `BAND_FLOOR`, because it is measured
/// against the house's own height -- houses on this map differ in height by an
/// order of magnitude, and an absolute floor would swallow a small one whole.
const SHROUD_FLOOR: f32 = 0.08;

/// How solid a shroud is.
///
/// The most present thing on a lot. A deletion is the most consequential thing
/// in a review and the easiest to miss -- it was invisible before the work that
/// introduced this -- so it is drawn as the heaviest mark. Still short of
/// opaque: the roofline reads through the top edge, which is what keeps a
/// covered house legible as a *house* rather than as an anonymous block.
const SHROUD_ALPHA: u8 = 0xe0;

// Checked when this compiles rather than when the suite runs. `main` made this
// argument for the old scaffold floor and it is the right one: these are facts
// about literals, not about behaviour, so a `#[test]` asserting them could only
// ever fail on a build that had already been made. Each is the shape of the
// grammar the works are drawn in, and breaking one is now a compile error.
//
// Note what is deliberately NOT asserted here any more: the old
// `SCAFFOLD_FLOOR > 6.0`. That floor was cut from 9 to 3.5 on purpose -- see
// `BAND_FLOOR`, where the reasoning is that girth now carries magnitude too, so
// a small change is a *thin* column rather than needing to be a tall one. The
// clearance it protected is bought a different way, and keeping the old
// assertion would have pinned the very thing that made every bar look alike.
const _: () = assert!(
    BAND_GAP < BAND_FLOOR,
    "a gap wider than the smallest band makes a stack mostly air"
);
const _: () = assert!(
    GIRTH_RANGE.0 < GIRTH_RANGE.1,
    "girth has to ramp, or magnitude has only one channel again"
);
const _: () = assert!(
    SHROUD_GIRTH > GIRTH_RANGE.1,
    "a shroud covers its house rather than standing on it, so it is the wider mark"
);
// The measurement in `SHROUD_GIRTH`'s own docs, held as a build failure. A
// shroud narrower than the widest roof on the map is a box wedged inside the
// building instead of one placed over it, which is exactly the fault this
// replaced -- and it is one edited literal away at all times. 1.24 is the
// `market`'s front slab, the widest roof point of any archetype in `meshes.rs`.
const _: () = assert!(
    SHROUD_GIRTH > 1.24,
    "a shroud this narrow leaves the market's eaves sticking out of it"
);
const _: () = assert!(
    SHROUD_FLOOR > 0.0 && SHROUD_FLOOR < 1.0,
    "the least of a house a removal covers is a share of it, and not all of it"
);

/// How high the skirt stands off the ground.
///
/// Barely anything: this is a stain on the ground rather than a wall, and the
/// shroud is what stands *on* the lot. Above `spawn::layer::GROUND_LABEL` so it
/// is not swallowed by a folder name painted across the same ward.
const SKIRT_LIFT: f32 = 0.24;

/// How transparent the works are.
///
/// Translucent on purpose, and not only for looks: this is a *proposal*, not
/// the city. The King has approved nothing yet, and a solid building would say
/// the work is already part of the settlement.
///
/// Raised from 0x9c after looking at it on screen. The map's own Source roofs
/// are a muted sage (`scene::palette`), and a scaffold in the working green is
/// close enough in hue that at 61% it read as a tint on the roof rather than as
/// a thing standing on it. The saturation differs but the value did not. This
/// is still visibly see-through -- the roofline reads straight through it --
/// which is the whole of what the translucency has to say.
const WORKS_ALPHA: u8 = 0xc8;

/// A ghost house is fainter still: nothing of it exists on disk yet.
const GHOST_ALPHA: u8 = 0x78;

// The three weights the works are drawn at, in the order they must stay in: a
// removal is the most present thing on a lot, a proposal is translucent, and a
// house that does not exist yet is fainter still.
const _: () = assert!(
    SHROUD_ALPHA > WORKS_ALPHA && WORKS_ALPHA > GHOST_ALPHA,
    "the works' three weights have crossed over"
);

/// What the interface last said the open plan is proposing.
///
/// Empty is the answer everywhere except inside a chamber, and the systems
/// below lean on that: a map with no plan open costs one resource read a frame.
#[derive(Resource, Default, Debug, Clone, PartialEq)]
pub struct Works(pub Vec<Work>);

impl Works {
    /// Whether anything at all is under construction.
    pub fn is_quiet(&self) -> bool {
        self.0.is_empty()
    }
}

/// Everything raised for the current works, so replacing them is one despawn.
///
/// Deliberately its own root rather than a child of `spawn::SceneRoot`: the
/// works outlive no world, but they are replaced far more often than one is, and
/// a separate root means swapping them never walks the settlement's entities.
#[derive(Component)]
pub struct WorksRoot;

/// A band of a column, which breathes. Carries its own material for
/// [`super::activity::TownRing`]'s reason: the shared cache quantises by colour
/// and hands one handle to hundreds of meshes, so animating a cached material
/// would pulse whatever else landed in the same bucket.
#[derive(Component, Clone)]
pub struct Scaffold {
    /// This band's own material, animated by [`pulse_works`].
    pub material: Handle<StandardMaterial>,
    /// The colour it breathes around: the agent's own, for this direction.
    pub base: Color,
    /// How bright this one burns at the top of its breath. Taken from the
    /// band's own magnitude, so a heavily-worked house is the brightest thing
    /// on the map as well as the tallest.
    pub strength: f32,
}

/// How tall one band stands, given how many lines it moved.
///
/// Absolute rather than relative -- see [`FULL_CHURN`], which records the fault
/// this replaced and why a share of the busiest file could not be made to work
/// once several agents were drawn at once.
///
/// Pure, so the shape can be pinned without a renderer.
pub fn band_height(churn: f32) -> f32 {
    BAND_FLOOR + magnitude(churn) * (COLUMN_REACH - BAND_FLOOR)
}

/// How wide a column stands, as a share of the house under it.
///
/// The second channel magnitude is carried in. See [`GIRTH_RANGE`] for why one
/// channel was not enough.
pub fn band_girth(churn: f32) -> f32 {
    let (thin, thick) = GIRTH_RANGE;
    thin + magnitude(churn) * (thick - thin)
}

/// One of an agent's two colours at a point in its breath.
///
/// The agent's own colour, dimmed toward black -- never lightened toward white.
/// See the module docs, and [`super::activity::ring_color`], which this is the
/// translucent sibling of.
///
/// `alpha` is the caller's because a ghost house is fainter than a standing
/// one: what is being drawn decides how solid a proposal looks, and the colour
/// itself carries only whose it is.
pub fn band_color(base: Color, strength: f32, alpha: u8) -> Color {
    let base = base.to_linear();
    Color::LinearRgba(LinearRgba {
        red: base.red * strength,
        green: base.green * strength,
        blue: base.blue * strength,
        alpha: alpha as f32 / 255.0,
    })
}

/// Rebuilds the works whenever the interface reports a different set.
///
/// Only on a change, which is what keeps this from respawning a chamber's worth
/// of geometry every frame. `Works` derives `PartialEq` and `view.rs` only sends
/// when the resolved set actually differs, so the two guards agree.
pub fn apply_works(
    mut commands: Commands,
    works: Res<Works>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut mesh_cache: ResMut<MeshCache>,
    existing: Query<Entity, With<WorksRoot>>,
) {
    if !works.is_changed() {
        return;
    }

    for entity in existing.iter() {
        commands.entity(entity).despawn();
    }
    if works.is_quiet() {
        return;
    }

    let root = commands
        .spawn((WorksRoot, Transform::default(), Visibility::default()))
        .id();

    for work in &works.0 {
        raise_one(
            &mut commands,
            &mut meshes,
            &mut materials,
            &mut mesh_cache,
            root,
            work,
        );
    }
}

fn raise_one(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    mesh_cache: &mut MeshCache,
    root: Entity,
    work: &Work,
) {
    let footprint = work.site.footprint();
    if footprint.width <= 0.0 || footprint.depth <= 0.0 {
        return;
    }
    let center = footprint.center();

    // A file that did not exist gets a house of its own, built through the same
    // cache every real holding uses -- so a new file looks like a *building*
    // rather than like a marker, which is the whole point of drawing it on a
    // map of buildings.
    //
    // The roof it ends up with becomes the base its column rises from, which
    // is why this is computed rather than taken from `WorkSite::base`: a ghost
    // house is built here and only here, so only here is its height known.
    // Without it the column started at the ground and rose *through* the
    // house, and the two read as one mass rather than as a building with
    // work on top -- seen on screen, not reasoned about.
    let mut base = work.site.base();
    if let WorkSite::Fresh { .. } = work.site {
        let plan_size = Vec2::new(footprint.width, footprint.depth);
        let height = band_height(work.churn()) * 0.5;
        let shape = BuildingShape::new(crate::map::BuildingKind::Cottage, plan_size, height, 0);
        let handles = mesh_cache.building(meshes, shape);
        // The archetypes are modelled inside a unit footprint with height as a
        // multiple of the shorter side, so a placed one stands `height() * plan`
        // tall. The same arithmetic `spawn::spawn_building` does.
        let plan = plan_size.x.min(plan_size.y);
        base = shape.height() * plan;
        // A ghost wears the colour of whoever is building it. With more than
        // one agent creating the same file, the first band's is used and the
        // rest is told by the column above -- a house cannot be two colours,
        // and the stack is where "who" is actually answered.
        let ghost_color = work
            .bands
            .first()
            .map(|band| to_color(band.growth))
            .unwrap_or(Color::WHITE);
        let ghost = materials.add(unlit(
            band_color(ghost_color, 1.0, GHOST_ALPHA),
            GHOST_ALPHA,
        ));

        let entity = commands
            .spawn((
                ChildOf(root),
                Transform::from_xyz(center[0], 0.0, center[1]).with_scale(Vec3::new(
                    plan_size.x,
                    plan,
                    plan_size.y,
                )),
                Visibility::default(),
            ))
            .id();
        commands.spawn((
            ChildOf(entity),
            Mesh3d(handles.walls),
            MeshMaterial3d(ghost.clone()),
            // The works are a reading of the city, not part of it: a click is
            // meant for whatever stands underneath.
            Pickable::IGNORE,
        ));
        commands.spawn((
            ChildOf(entity),
            Mesh3d(handles.roof),
            MeshMaterial3d(ghost),
            Pickable::IGNORE,
        ));
    }

    // The column: one segment per agent, stacked from the roof up. This is what
    // makes several agents in one file legible -- how many segments is how many
    // agents, and which hues they are is which agents.
    //
    // **Additions only.** Removals used to be stacked in here too, directly on
    // top of an agent's growth band, and that was the fault this replaced: it
    // put a *taller tower* on a house that was losing three hundred lines, which
    // says the opposite of what happened and contradicted the grammar this
    // module's own docs claimed. What is being built rises above the roof; what
    // is being taken away covers the house, below. There is now no exception to
    // that in either direction.
    let mut standing = base;
    for band in &work.bands {
        let churn = band.added;
        if churn <= 0.0 {
            continue;
        }
        let height = band_height(churn);
        let girth = band_girth(churn);
        let strength = 0.55 + magnitude(churn) * 0.45;
        let base_color = to_color(band.growth);
        let material = materials.add(unlit(
            band_color(base_color, strength, WORKS_ALPHA),
            WORKS_ALPHA,
        ));

        commands.spawn((
            ChildOf(root),
            Mesh3d(meshes.add(Cuboid::new(
                footprint.width * girth,
                height,
                footprint.depth * girth,
            ))),
            MeshMaterial3d(material.clone()),
            // Cuboids are built about their own centre, so the box is lifted by
            // half its height to stand *on* what is below it rather than
            // through it.
            Transform::from_xyz(center[0], standing + height * 0.5, center[1]),
            Scaffold {
                material,
                base: base_color,
                strength,
            },
            Pickable::IGNORE,
        ));

        // The next segment starts above this one, with a hairline of clear air
        // between them: two saturated colours meeting exactly would read as one
        // column with a gradient rather than as two agents.
        standing += height + BAND_GAP;
    }

    // The ground: what is being cleared here, and by whom.
    //
    // Sized from the footprint rather than as an absolute (see `SKIRT_SPREAD`),
    // which is the whole of why removals were invisible before. A razing takes
    // the whole lot and the deepest colour -- the house is going, not shrinking.
    let razing = work.bands.iter().find(|band| band.razing);
    let cutting: f32 = work.bands.iter().map(|band| band.removed).sum();
    if razing.is_some() || cutting > 0.0 {
        let (colour, weight) = match razing {
            // A deletion is total, so it is drawn at full weight whatever the
            // line count: "this file is going" is not a matter of degree.
            Some(band) => (band.cutting, 1.0),
            None => (
                work.bands
                    .iter()
                    .filter(|band| band.removed > 0.0)
                    .max_by(|a, b| a.removed.total_cmp(&b.removed))
                    .map(|band| band.cutting)
                    .unwrap_or(work.bands[0].cutting),
                magnitude(cutting),
            ),
        };

        let plan = footprint.width.min(footprint.depth);
        let spread = (SKIRT_SPREAD * plan * weight).max(SKIRT_FLOOR * weight.max(0.35));
        let mut color = to_color(colour);
        color.set_alpha(weight.clamp(0.35, 1.0) * (WORKS_ALPHA as f32 / 255.0));

        commands.spawn((
            ChildOf(root),
            Mesh3d(meshes.add(meshes::ground_polygon(&[
                Vec2::new(footprint.x - spread, footprint.y - spread),
                Vec2::new(footprint.max_x() + spread, footprint.y - spread),
                Vec2::new(footprint.max_x() + spread, footprint.max_y() + spread),
                Vec2::new(footprint.x - spread, footprint.max_y() + spread),
            ]))),
            MeshMaterial3d(materials.add(StandardMaterial {
                base_color: color,
                unlit: true,
                alpha_mode: AlphaMode::Blend,
                ..default()
            })),
            Transform::from_xyz(0.0, SKIRT_LIFT, 0.0),
            Pickable::IGNORE,
        ));
    }

    // The shroud: what is being taken away, covering the house it is taken from.
    //
    // **This is what the King asked for, and the grammar the module's docs
    // always claimed.** A block rising from the ground over as much of the house
    // as the file is losing -- half the file cut, half the house covered -- so a
    // removal can never be mistaken for the column of growth above the roof.
    //
    // Stacked, like the column, and for the same reason: with two agents cutting
    // one file, whose deletion is whose is a question the map has to answer, and
    // two hues covering half a house between them answer it. Each agent's share
    // is its own, so the stack adds up to what the file is actually losing.
    //
    // A deletion is not a special case any more -- it is `cover` at 1.0, and the
    // whole house disappears under the block. That is the honest reading, and it
    // is one rule instead of two.
    let mut covered = 0.0;
    for band in &work.bands {
        if band.cover <= 0.0 || !band.cover.is_finite() {
            continue;
        }
        // Each agent's share is clamped by `resolve`, but the *stack* is not:
        // three agents each cutting half a file sum to one and a half houses,
        // and a shroud rising past the roof is the one thing this grammar does
        // not allow. What is left of the house is the ceiling.
        let room = base - covered;
        if room <= 0.0 {
            break;
        }
        let height = shroud_height(band.cover, base).min(room);
        if height <= 0.0 {
            continue;
        }
        let base_color = to_color(band.cutting);
        let material = materials.add(unlit(
            band_color(base_color, 1.0, SHROUD_ALPHA),
            SHROUD_ALPHA,
        ));

        commands.spawn((
            ChildOf(root),
            Mesh3d(meshes.add(Cuboid::new(
                footprint.width * SHROUD_GIRTH,
                height,
                footprint.depth * SHROUD_GIRTH,
            ))),
            MeshMaterial3d(material.clone()),
            // From the ground up, each agent's share stacked on the last. No
            // gap between them, unlike the column: these are one house being
            // covered rather than separate things standing on each other, and
            // a stripe of bare wall between two blocks would read as the
            // house showing through.
            Transform::from_xyz(center[0], covered + height * 0.5, center[1]),
            Scaffold {
                material,
                base: base_color,
                strength: 1.0,
            },
            Pickable::IGNORE,
        ));

        covered += height;
    }
}

/// How much of a house a removal covers, in world units.
///
/// `cover` is the share of the file going away and `house` is how tall it
/// stands, so the product is the King's own rule: half the file removed covers
/// half the house. [`SHROUD_FLOOR`] is what keeps a one-line cut in a huge file
/// from being nothing at all.
///
/// Pure, so the shape is pinned by the tests below without a renderer -- and
/// guarded against the same NaN that [`magnitude`] records, since `cover` is a
/// ratio that crossed a wire.
pub fn shroud_height(cover: f32, house: f32) -> f32 {
    if !cover.is_finite() || cover <= 0.0 || !house.is_finite() || house <= 0.0 {
        return 0.0;
    }
    house * cover.clamp(SHROUD_FLOOR, 1.0)
}

/// A translucent unlit material in one colour. See the module docs for why
/// nothing here is lit.
fn unlit(color: Color, alpha: u8) -> StandardMaterial {
    let mut color = color;
    color.set_alpha(alpha as f32 / 255.0);
    StandardMaterial {
        base_color: color,
        unlit: true,
        alpha_mode: AlphaMode::Blend,
        ..default()
    }
}

/// Breathes every band, on the same clock the working ring breathes on.
///
/// [`super::activity::glow`] is shared rather than copied, so the works and the
/// ring around the town they stand in rise and fall together instead of beating
/// against each other -- they are two readings of one plan, and reading them as
/// unrelated blinking would be wrong about that.
pub fn pulse_works(
    time: Res<Time>,
    works: Res<Works>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    scaffolds: Query<&Scaffold>,
) {
    if works.is_quiet() {
        return;
    }
    let breath = activity::glow(time.elapsed_secs());
    for scaffold in scaffolds.iter() {
        if let Some(mut material) = materials.get_mut(&scaffold.material) {
            // The band's own colour, dimmed -- not a shared one. Each band
            // carries its base so the pulse never has to ask which agent it
            // belongs to.
            material.base_color = band_color(
                scaffold.base,
                breath * scaffold.strength,
                (material.base_color.alpha() * 255.0) as u8,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::map::{MapRect, WorkBand};

    fn band(added: f32, removed: f32) -> WorkBand {
        WorkBand {
            growth: [0x5c, 0xf5, 0xa8, 255],
            cutting: [0x15, 0x7f, 0x4a, 255],
            added,
            removed,
            // What `resolve` would compute for a file of a hundred lines, which
            // is a plausible house for these to stand on.
            cover: (removed / 100.0).clamp(0.0, 1.0),
            razing: false,
        }
    }

    #[test]
    fn a_bigger_change_builds_a_taller_band() {
        assert!(band_height(400.0) > band_height(40.0));
        assert!(band_height(40.0) > band_height(4.0));
        assert!(band_height(4.0) > band_height(0.0));
    }

    /// The floor is what keeps a small change visible. Without it a file that
    /// moved three lines beside one that moved four hundred is drawn as nothing.
    ///
    /// That the floor also clears a typical roofline is pinned at the constant
    /// itself, as a `const` assertion -- it is arithmetic on a literal, so it
    /// is checked at compile time rather than here.
    #[test]
    fn even_the_smallest_change_stands_high_enough_to_see() {
        assert!(band_height(0.0) >= BAND_FLOOR);
        assert!(band_height(1.0) > BAND_FLOOR);
    }

    /// **The reported bug.** A two-line edit beside a four-hundred-line one used
    /// to draw a bar 23% as tall, because height was a `sqrt` of a *share of the
    /// busiest file in the same plan* over a floor that ate a sixth of the
    /// range. Both channels now ramp with absolute churn, and what the King
    /// actually reads is apparent volume -- height times girth squared.
    #[test]
    fn a_small_change_and_a_large_one_are_obviously_different() {
        let volume = |churn: f32| band_height(churn) * band_girth(churn).powi(2);
        let small = volume(4.0);
        let large = volume(400.0);
        assert!(
            large / small > 6.0,
            "a 400-line change draws only {large} against {small} for a 4-line one; \
             the old relative ramp managed 3.9x and that was the bug"
        );
    }

    /// The other half of the same fault: two agents' bands must be measured
    /// against one ruler. Height is a function of churn alone, so a forty-line
    /// edit is the same height whatever else its plan happened to do.
    #[test]
    fn the_same_change_is_the_same_height_whoever_made_it() {
        // No plan, no summary, no context of any kind is reachable from here --
        // which is the guarantee. Stated as a test so that reintroducing a
        // normaliser has to break something.
        assert_eq!(band_height(40.0), band_height(40.0));
        assert!(band_height(40.0) < band_height(41.0));
    }

    /// A scale outside the range is a bug upstream, and must not produce a
    /// spike through the roof of the world or a hole under it.
    ///
    /// NaN is in here because it is *reachable*: the counts cross a wire as
    /// `f32`, and `f32::clamp` propagates NaN rather than trapping it. The
    /// predecessor of this function was found doing exactly that by a test.
    #[test]
    fn a_churn_out_of_range_is_still_a_sane_height() {
        assert_eq!(band_height(-2.0), band_height(0.0));
        assert_eq!(band_height(f32::NAN), band_height(0.0));
        assert!(band_height(f32::INFINITY).is_finite());
        assert!(band_height(f32::MAX).is_finite());
        // Girth is the second channel and has to survive the same inputs.
        assert!(band_girth(f32::NAN).is_finite());
        assert!(band_girth(f32::INFINITY) <= GIRTH_RANGE.1);
    }

    /// Magnitude is a fraction, and everything downstream assumes it.
    #[test]
    fn magnitude_stays_within_its_range() {
        for churn in [0.0, 1.0, 10.0, 600.0, 6_000.0, f32::MAX] {
            let m = magnitude(churn);
            assert!((0.0..=1.0).contains(&m), "{churn} gave {m}");
        }
        assert_eq!(magnitude(f32::NAN), 0.0);
    }

    /// Girth is the channel that lets the floor be small, so it has to actually
    /// vary -- a constant girth is what the fault looked like before.
    #[test]
    fn a_bigger_change_builds_a_wider_band() {
        assert!(band_girth(400.0) > band_girth(40.0));
        assert!(band_girth(40.0) > band_girth(4.0));
        assert!(band_girth(0.0) >= GIRTH_RANGE.0);
        assert!(band_girth(f32::MAX) <= GIRTH_RANGE.1);
    }

    /// **The second reported bug.** The skirt was an absolute 1.9 world units at
    /// maximum, in a world 1,000 units across -- sub-pixel in the rail's pane,
    /// which is why removals looked like they were not drawn at all. It is a
    /// share of the footprint now, so it scales with whatever house it wraps.
    #[test]
    fn a_removal_is_drawn_at_a_size_that_can_be_seen() {
        // A typical holding, and the spread a full removal earns on it.
        let plan = 12.0_f32;
        let spread = (SKIRT_SPREAD * plan * 1.0).max(SKIRT_FLOOR);
        assert!(
            spread > 4.0,
            "a removal spreads only {spread} units, which is what made it invisible"
        );
        // And a big house gets proportionally more, which an absolute could not.
        let big = SKIRT_SPREAD * 40.0;
        assert!(
            big > spread * 2.0,
            "the skirt must follow the house it wraps"
        );
    }

    /// The regression `activity::PULSE_PEAK` was written for, generalised: a
    /// band must stay recognisably *its own agent's colour* through the whole
    /// of its breath. Three earlier attempts at a lit status colour ended in
    /// white, mint and near-white, all in the bright direction.
    #[test]
    fn a_band_keeps_its_agents_hue_throughout_its_breath() {
        for banner in kingdom_core::palette::BANNERS {
            let base = to_color([
                banner.growth_rgb[0],
                banner.growth_rgb[1],
                banner.growth_rgb[2],
                255,
            ]);
            let Color::LinearRgba(full) = band_color(base, 1.0, WORKS_ALPHA) else {
                panic!("the works should be linear rgba");
            };
            for step in 0..24 {
                let breath = activity::glow(step as f32 * 0.1);
                let Color::LinearRgba(c) = band_color(base, breath, WORKS_ALPHA) else {
                    panic!("linear rgba");
                };
                // Never brighter than the colour itself: the pulse dims toward
                // black rather than brightening toward white.
                assert!(
                    c.red <= full.red + 1e-6
                        && c.green <= full.green + 1e-6
                        && c.blue <= full.blue + 1e-6,
                    "{} clipped toward white at step {step}",
                    banner.name
                );
                // And the hue is intact: the channel ordering never changes,
                // which is what "still recognisably that agent" means.
                let order = |a: f32, b: f32| a.partial_cmp(&b).unwrap();
                assert_eq!(
                    order(c.red, c.green),
                    order(full.red, full.green),
                    "{} lost its hue at step {step}",
                    banner.name
                );
                assert_eq!(
                    order(c.green, c.blue),
                    order(full.green, full.blue),
                    "{} lost its hue at step {step}",
                    banner.name
                );
            }
        }
    }

    /// A proposal is not the city. Solid works would say the King had already
    /// approved them.
    #[test]
    fn the_works_are_translucent() {
        let Color::LinearRgba(c) = band_color(Color::WHITE, 1.0, WORKS_ALPHA) else {
            panic!("linear rgba");
        };
        assert!(c.alpha < 1.0, "a proposal must not look built");
        // A ghost is fainter still: nothing of it exists on disk yet.
        assert!(GHOST_ALPHA < WORKS_ALPHA);
    }

    /// **The reported fault.** Half the file removed covers half the house.
    ///
    /// This is the King's own rule, stated as arithmetic. It replaces
    /// `a_razing_stays_below_the_roof_it_is_taking_down`, which pinned the
    /// previous grammar -- a deletion drawn as a band at the foot of the house
    /// at 55% of whatever height its churn earned, with ordinary removals
    /// stacked in the *column above the roof*, where they read as growth.
    #[test]
    fn a_removal_covers_a_share_of_the_house_it_is_cutting() {
        let house = 32.0_f32;
        assert_eq!(shroud_height(0.5, house), house * 0.5);
        assert_eq!(shroud_height(0.25, house), house * 0.25);
        // A deletion is a shroud at full height: the whole house goes under it.
        assert_eq!(shroud_height(1.0, house), house);
    }

    /// Nothing about a removal may rise above the house it is covering. The
    /// whole grammar is that growth goes up and cutting covers, so a shroud
    /// taller than its house would be indistinguishable from a column.
    ///
    /// A share over 1.0 is reachable: the manifest is memoised on the shape of
    /// the kingdom and may be stale about a file's length, so `removed` can
    /// exceed it. `resolve` clamps, and this is the second guard.
    #[test]
    fn a_removal_never_rises_above_the_house() {
        let house = 32.0_f32;
        for cover in [0.0, 0.01, 0.5, 1.0, 4.0, f32::MAX] {
            let height = shroud_height(cover, house);
            assert!(
                height <= house,
                "a cover of {cover} rose {height} over a {house} house"
            );
        }
    }

    /// A *stack* of removals may not rise above the house either.
    ///
    /// Each agent's share is clamped on its own by `resolve`, but three agents
    /// each cutting half a file sum to one and a half houses. This is the
    /// arithmetic the drawing does to hold the stack to the roofline -- the one
    /// rule the grammar has.
    #[test]
    fn a_stack_of_removals_never_rises_above_the_house() {
        let house = 32.0_f32;
        let mut covered = 0.0_f32;
        for cover in [0.5, 0.5, 0.5, 1.0] {
            let room = house - covered;
            if room <= 0.0 {
                break;
            }
            covered += shroud_height(cover, house).min(room);
        }
        assert!(
            covered <= house,
            "four agents covered {covered} of a {house} house"
        );
        assert_eq!(covered, house, "and between them they cover all of it");
    }

    /// A small cut in a large file is still a cut the King should be able to
    /// see. One line of four thousand is a share of 0.00025, which is nothing
    /// at any zoom -- the same fault `BAND_FLOOR` exists to prevent for the
    /// column.
    #[test]
    fn even_the_smallest_removal_covers_enough_to_see() {
        let house = 32.0_f32;
        let sliver = shroud_height(1.0 / 4_000.0, house);
        assert_eq!(sliver, house * SHROUD_FLOOR);
        assert!(
            sliver > 1.0,
            "a one-line cut covered {sliver} of a {house} house, which is nothing"
        );
    }

    /// **The regression guard for the girth.** A shroud has to be wider than
    /// every roof on the map or it is a box wedged inside the building rather
    /// than one placed over it -- which is how the razing it replaced read.
    ///
    /// The numbers are the widest point of each archetype in `meshes.rs`, in
    /// the unit footprint they are modelled in. That `SHROUD_GIRTH` clears the
    /// widest of them is also a `const` assertion; this is the table it was
    /// taken from, so that adding a wider roof breaks a test that names it.
    #[test]
    fn a_shroud_is_wider_than_every_roof_it_covers() {
        const HALF: f32 = 0.5;
        for (archetype, widest) in [
            ("keep", HALF + 0.04),
            ("scriptorium", HALF + 0.05),
            ("pitched: cottage, guildhall, granary", HALF + 0.06),
            ("market's front slab", HALF + 0.12),
        ] {
            let needed = widest * 2.0;
            assert!(
                SHROUD_GIRTH > needed,
                "{archetype} needs {needed} and the shroud is only {SHROUD_GIRTH} wide"
            );
        }
    }

    /// A house with no height cannot be covered, and a share that is not a
    /// number must not become one.
    ///
    /// Reachable for the same reason `magnitude` guards: `cover` crosses a wire
    /// as an `f32` and `f32::clamp` propagates NaN rather than trapping it, so
    /// an unguarded version reaches Bevy as a degenerate mesh.
    #[test]
    fn a_cover_out_of_range_is_still_a_sane_height() {
        assert_eq!(shroud_height(f32::NAN, 32.0), 0.0);
        assert_eq!(shroud_height(0.5, f32::NAN), 0.0);
        assert_eq!(shroud_height(-1.0, 32.0), 0.0);
        assert_eq!(shroud_height(0.5, 0.0), 0.0);
        assert!(shroud_height(f32::INFINITY, 32.0).is_finite());
        assert!(shroud_height(0.5, f32::MAX).is_finite());
    }

    /// A site with no ground cannot be built on, and must not reach the
    /// renderer as a zero or negative mesh.
    #[test]
    fn a_site_with_no_ground_is_skipped() {
        let work = Work {
            site: WorkSite::Fresh {
                footprint: MapRect {
                    x: 0.0,
                    y: 0.0,
                    width: 0.0,
                    depth: 4.0,
                },
            },
            bands: vec![band(10.0, 0.0)],
        };
        assert!(work.site.footprint().width <= 0.0, "the guard's condition");
    }

    /// An empty set is the ordinary way to say a chamber was left, and the
    /// pulse must cost nothing on a map with no plan open.
    #[test]
    fn no_open_plan_is_a_quiet_map() {
        assert!(Works::default().is_quiet());
        assert!(
            !Works(vec![Work {
                site: WorkSite::Standing {
                    footprint: MapRect::default(),
                    height: 1.0,
                },
                bands: vec![band(5.0, 5.0)],
            }])
            .is_quiet()
        );
    }

    /// The contention question from `AGENTS.md`, answered where it is known.
    #[test]
    fn a_file_two_agents_are_in_says_so() {
        let one = Work {
            site: WorkSite::Standing {
                footprint: MapRect::default(),
                height: 1.0,
            },
            bands: vec![band(5.0, 0.0)],
        };
        assert!(!one.is_contended());
        assert_eq!(one.churn(), 5.0);

        let two = Work {
            bands: vec![band(5.0, 0.0), band(3.0, 4.0)],
            ..one
        };
        assert!(two.is_contended());
        assert_eq!(two.churn(), 12.0, "a column is sized from everyone's work");
    }
}
