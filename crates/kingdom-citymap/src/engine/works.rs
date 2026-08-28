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
//! # How big a change is drawn, and why the ruler was replaced three times
//!
//! A column's height and girth both come from [`magnitude`], which turns a
//! count of lines into `0.0..=1.0`. It has been wrong three times, and
//! [`LINEAR_CHURN`] holds the full record:
//!
//! - it was **relative** -- a share of the busiest file in the same plan -- so
//!   two agents were measured with two rulers and a one-file plan drew at full
//!   height. That fix stands.
//! - it was then **logarithmic**, which spent a third of the range on changes
//!   below ten lines and left almost none for the range real work lives in: a
//!   `+8` and a `+100` came out 1.9x apart, and anything past 600 lines drew
//!   identically.
//! - it was then a **saturating ratio**, which fixed both of those and was
//!   still nowhere proportional: it bends from the origin, so a `+27` and a
//!   `+115` were 4.3x apart in work and 2.6x apart on screen.
//!
//! It is now **linear** up to a knee and compressed only above it, which is the
//! rule a holding's own footprint already follows -- twice the lines, twice the
//! mark. [`crate::scale`] holds the shape and why a column, unlike a footprint,
//! cannot be proportional all the way up.
//!
//! The lesson worth carrying: the curve is not a matter of taste, it is a claim
//! about the distribution of the thing being drawn, and the distribution is
//! measurable. `LINEAR_CHURN` names the one this is fitted to.
//!
//! # Why these do not come through the manifest
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
//! [`super::activity::WORKING_COLOR`] records three failed attempts at drawing
//! a status colour as a lit surface: emissive scaled for the sun's lux clipped
//! to white, a value near 1.0 was washed out by the tonemapper, and a lit
//! material added the sun's white specular on top and came out mint. The
//! conclusion there is the conclusion here -- a status colour is a piece of
//! interface that happens to be drawn in world space -- so everything in this
//! module is `unlit`.
//!
//! The colours are **not** this module's to choose any more. Each band arrives
//! carrying the two colours of the agent that made it --
//! `kingdom_core::palette`, one hue per plan, a light value for lines added and
//! a dark one for lines removed -- because with several agents on one map the
//! question stopped being "was this growth or cutting?" and became "whose hands
//! are on this file?". A single green and a single red could answer the first
//! and structurally could not answer the second.
//!
//! # Nothing here moves, and nothing here is dimmed
//!
//! A band is its agent's colour exactly, at the alpha its kind earns, from the
//! moment it is raised until it is replaced. Two things used to vary it and
//! both are gone at the King's word: a 2.4-second breath shared with the town
//! ring, and a *strength* ramp that drew a small change at 55% of its agent's
//! colour and a large one at full.
//!
//! Size is the only channel magnitude has now -- [`band_height`],
//! [`band_girth`] and [`shroud_height`] -- which is the
//! channel it was always read from anyway. A colour that moves is a colour
//! being asked to say two things at once: whose work this is, and how much of
//! it there is. It now says only the first, and says it steadily.
//!
//! What this module still decides is *alpha*, which is about the drawing rather
//! than the domain: a proposal is translucent, and a ghost is fainter still.

use bevy::prelude::*;

use crate::map::{Work, WorkSite};

use super::materials::to_color;
use super::meshes::BuildingShape;
use super::spawn::MeshCache;

/// The churn up to which a column is drawn exactly to scale, in lines.
///
/// **This is the knee of the curve, and it is the third ruler this mark has
/// had.** The record matters, because each replacement was made for a fault the
/// King reported by eye and each one is a way of getting this wrong:
///
/// 1. **Relative.** Height was a share of the busiest file in the *same plan*,
///    so two agents were measured with two rulers and a one-file plan drew at
///    full height. Replaced, and the fix stands: the ruler is absolute, churn in
///    lines goes in, and nothing local to a plan is consulted.
/// 2. **Logarithmic.** `FULL_CHURN = 600.0` with `magnitude = ln_1p(churn) /
///    ln_1p(FULL_CHURN)`, and the King reported it exactly: a `+8` looked about
///    the same size as a `+100`. `ln1p` rises fastest near zero, so a *one-line*
///    edit already took 11% of the range; there was no resolution left where
///    changes actually live; and the clamp drew everything past 600 lines
///    identically.
/// 3. **A saturating ratio,** `churn / (churn + 110)`. It fixed the plateau and
///    the wasted bottom end, and it is what this replaces -- because it is
///    *nowhere* proportional. It bends immediately: at its knee it has already
///    spent half its range, so a +27 and a +115 are 4.3x apart in work and were
///    2.6x apart on screen.
///
/// **Linear, now, and for the reason the footprint is.** The King asked for one
/// rule across the map: twice the lines, twice the mark. Under this knee that is
/// exact -- see [`crate::scale::linear_then_tail`], which also explains why a
/// column cannot be linear all the way up when a footprint can.
///
/// 300 lines is that knee, fitted to this repository's own distribution: over
/// 400 commits, per-file added lines run p25 = 6, median = 26, p75 = 117,
/// p90 = 263, p95 = 425, p99 = 935, with the largest single file at 2,137. A
/// knee here puts everything up to p95 in the strictly proportional part and
/// leaves the tail to the rewrites, where "very large" is a good enough answer.
pub const LINEAR_CHURN: f32 = 300.0;

/// How much of a column's range the proportional part spends.
///
/// The other half of [`LINEAR_CHURN`]'s decision, and a direct trade: three
/// quarters of the height is given to changes up to the knee, and the last
/// quarter carries everything above it out to infinity. More would draw the
/// common range more faithfully and leave a 935-line change and a 4,000-line one
/// harder to tell apart; less would buy resolution among rewrites that nobody
/// needs at the cost of the range the map is actually read in.
const TAIL_SHARE: f32 = 0.75;

/// How much of a change's size shows, given how many lines moved.
///
/// Linear to [`LINEAR_CHURN`], then a tail -- that constant records the two
/// rulers this replaced and why the shape is what it is.
///
/// Returns `0.0..=1.0`, approaching 1.0 without ever reaching it. Pure, so the
/// shape is pinned by the tests below without a renderer, and NaN-guarded in
/// [`crate::scale::linear_then_tail`] for the reason recorded there.
pub fn magnitude(churn: f32) -> f32 {
    crate::scale::linear_then_tail(churn, LINEAR_CHURN, TAIL_SHARE)
}

/// How far above a roof a column of the largest changes reaches, in world
/// units.
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
///
/// **An asymptote now rather than a value that is reached**, which is why it
/// went from 52 to 58: [`magnitude`] saturates instead of clamping, so nothing
/// stands at exactly this height and the large end would otherwise have come
/// down. 58 puts a 600-line change back at roughly the 52 it drew before, and a
/// four-thousand-line one a little above it -- which is the plateau the change
/// was made to remove. It stays under `super::TALLEST`, the 60 units the camera
/// fit already assumes for a roofline, so a column cannot be framed out.
const COLUMN_REACH: f32 = 58.0;

/// The shortest a band may be, whatever the churn.
///
/// A one-line change is still a change the King should be able to see. Without
/// a floor, a file that moved three lines beside one that moved four hundred
/// would be drawn as nothing at all.
///
/// **Cut twice: 9, then 3.5, now 2.5.** A floor is a flat tax on the bottom of
/// the range, and the more honest the curve above it becomes the more that tax
/// is the only thing left distorting a small change. At 9 it flattened
/// everything under a hundred lines into one stub. At 3.5 it was affordable
/// because a saturating curve was itself lifting the low end.
///
/// With [`magnitude`] strictly proportional under its knee, nothing lifts the
/// low end any more, and 3.5 was measurably too much: it left the step from this
/// repository's p25 to its median at 1.66x, under the 1.82x
/// `every_step_through_a_real_distribution_is_visible` requires. At 2.5 the same
/// step draws 1.87x. Lower still would be honester arithmetic and a worse map --
/// this is the point where a one-line change stops being a mark at all.
const BAND_FLOOR: f32 = 2.5;

/// How wide a column is as a share of the house it stands on, from the smallest
/// change to the largest.
///
/// Girth used to be a constant `footprint * 0.82`, so the *only* thing that
/// varied a column's width was the size of the house under it -- a fact about
/// the file, not about the change. Ramping it with the change's size means a big
/// change reads as a heavy column and a small one as a slender mark.
///
/// **It ramps with [`magnitude`] itself, linearly, at the King's word.** It used
/// to ramp with the square root of it, deliberately: a gentler second channel
/// that kept a small change wide enough to resolve in the rail's small pane
/// while height carried the proportion. That is a real property and it is what
/// was given up here -- the King asked for every size on the map to be linear,
/// and a width that is the square root of the size is not.
///
/// The floor is what keeps the loss bearable: at 0.30 the narrowest column is
/// still nearly a third of its house, so a small change is slender rather than
/// invisible, and the two channels now agree instead of disagreeing by design.
const GIRTH_RANGE: (f32, f32) = (0.30, 0.85);

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
// The knee of the curve has to be a real number of lines. At zero every change
// would be the largest change there is, which is the flat map this replaced
// wearing the opposite sign.
const _: () = assert!(
    LINEAR_CHURN > 1.0,
    "a knee at or below one line draws every change as a full column"
);
// The linear part must leave the tail something to work with. At a share of 1.0
// the curve is a clamp again -- every change past the knee drawn identically --
// which is the plateau `LINEAR_CHURN` records as the second fault.
const _: () = assert!(
    TAIL_SHARE > 0.0 && TAIL_SHARE < 1.0,
    "a linear part that spends the whole range is a clamp, which is a plateau"
);
// `super::TALLEST` is the roof height the camera's fit assumes, and it is
// private to that module -- so the number is repeated here with the reason
// rather than left as a coincidence. A column that reaches past what the fit
// reserves is one the King can zoom until it is cut off.
const _: () = assert!(
    COLUMN_REACH < 60.0,
    "a column taller than the fit's assumed roofline can be framed out"
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

/// The shortest a ghost house stands, in world units.
///
/// A new file's house is half the column its churn would earn, and with an
/// honest curve under that (see [`LINEAR_CHURN`]) a small new file would be a
/// slab a few units high -- which reads as a mark on the ground rather than as
/// a building, and a ghost exists precisely so that a new file looks like a
/// house.
///
/// 11.0 is not a taste: it is `build::layout::Building::height`'s own lower
/// clamp, the height of the shortest holding the map ever draws. So a ghost is
/// never shorter than the smallest real house standing beside it.
const GHOST_FLOOR: f32 = 11.0;

/// The tallest a ghost house stands, as a multiple of its footprint's shorter
/// side.
///
/// [`GHOST_FLOOR`]'s necessary other half. A ward with no room left shrinks a
/// new house to fit (`map::works::SHRINK`, up to three times), so a ghost's
/// footprint can be a fraction of the usual -- and an absolute floor standing
/// over one would draw a spike, which is a silhouette no holding on this map is
/// allowed to have.
///
/// 1.9 is `build::layout::Building::height_ceiling`'s own ratio, repeated here
/// because that function takes a `Building` and a ghost is not one. Sharing the
/// number is the point: a ghost is bound by the same proportion as every real
/// house, so a small lot gets a small house rather than a mast.
const GHOST_ASPECT: f32 = 1.9;

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
///
/// It is also the *only* handle anything needs on the works now. There used to
/// be a `Scaffold` component on every band as well, carrying that band's own
/// material handle and base colour so the pulse could write a new colour through
/// it each frame -- and each band therefore needed a material of its own rather
/// than a shared one. With nothing animating them, a band is raised in its
/// colour and left alone, so there is nothing for such a component to be read
/// by.
#[derive(Component)]
pub struct WorksRoot;

/// How tall one band stands, given how many lines it moved.
///
/// Absolute rather than relative -- see [`LINEAR_CHURN`], which records both the
/// fault this replaced and the curve fault that came after it.
///
/// Pure, so the shape can be pinned without a renderer.
pub fn band_height(churn: f32) -> f32 {
    BAND_FLOOR + magnitude(churn) * (COLUMN_REACH - BAND_FLOOR)
}

/// How wide a column stands, as a share of the house under it.
///
/// Linear in the change's size, like its height -- see [`GIRTH_RANGE`] for what
/// that gave up and why. The two channels now say the same thing at the same
/// rate, so a column's whole silhouette is proportional to the work in it.
pub fn band_girth(churn: f32) -> f32 {
    let (thin, thick) = GIRTH_RANGE;
    thin + magnitude(churn) * (thick - thin)
}

/// One of an agent's two colours, at the weight the drawing calls for.
///
/// The agent's own colour, exactly, with `alpha` applied. Nothing scales the
/// channels any more: the King asked for bands that neither pulse nor change
/// colour, and a hue that is only sometimes itself is a hue that cannot be used
/// to recognise an agent at a glance.
///
/// It used to take a `strength` and multiply every channel by it, for two
/// reasons that both went: a breath shared with the town ring, and a ramp with
/// the size of the change. See the module docs -- magnitude is size now, and
/// only size.
///
/// `alpha` is still the caller's, because a ghost house is fainter than a
/// standing one: what is being drawn decides how solid a proposal looks, and the
/// colour itself carries only whose it is.
pub fn band_color(base: Color, alpha: u8) -> Color {
    let base = base.to_linear();
    Color::LinearRgba(LinearRgba {
        red: base.red,
        green: base.green,
        blue: base.blue,
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
        // The archetypes are modelled inside a unit footprint with height as a
        // multiple of the shorter side, so a placed one stands `height() * plan`
        // tall. The same arithmetic `spawn::spawn_building` does.
        let plan = plan_size.x.min(plan_size.y);
        // Half the column its churn would earn, held between the same two
        // bounds a real house obeys.
        //
        // The floor is new and it is needed: with the logarithm gone from
        // `magnitude` an honest low end means a brand-new file of a dozen lines
        // would be a slab a few units high, which reads as a stain on the
        // ground rather than as a building -- and a ghost's whole point is that
        // a new file looks like a house.
        //
        // The cap is what keeps the floor from being the opposite fault. A
        // ghost on a shrunken lot in a packed ward (`map::works::SHRINK`, three
        // times over) can have a footprint a quarter of the usual, and an
        // absolute floor over it would draw a thin spike -- a shape no holding
        // on the map is allowed. `GHOST_ASPECT` is
        // `build::layout::Building::height_ceiling`'s own ratio, so a small lot
        // gets a small house exactly as a real one does.
        let height = (band_height(work.churn()) * 0.5)
            .max(GHOST_FLOOR)
            .min(plan * GHOST_ASPECT);
        let shape = BuildingShape::new(crate::map::BuildingKind::Cottage, plan_size, height, 0);
        let handles = mesh_cache.building(meshes, shape);
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
        let ghost = materials.add(unlit(band_color(ghost_color, GHOST_ALPHA), GHOST_ALPHA));

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
        let base_color = to_color(band.growth);
        let material = materials.add(unlit(band_color(base_color, WORKS_ALPHA), WORKS_ALPHA));

        commands.spawn((
            ChildOf(root),
            Mesh3d(meshes.add(Cuboid::new(
                footprint.width * girth,
                height,
                footprint.depth * girth,
            ))),
            MeshMaterial3d(material),
            // Cuboids are built about their own centre, so the box is lifted by
            // half its height to stand *on* what is below it rather than
            // through it.
            Transform::from_xyz(center[0], standing + height * 0.5, center[1]),
            Pickable::IGNORE,
        ));

        // The next segment starts above this one, with a hairline of clear air
        // between them: two saturated colours meeting exactly would read as one
        // column with a gradient rather than as two agents.
        standing += height + BAND_GAP;
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
        let material = materials.add(unlit(band_color(base_color, SHROUD_ALPHA), SHROUD_ALPHA));

        commands.spawn((
            ChildOf(root),
            Mesh3d(meshes.add(Cuboid::new(
                footprint.width * SHROUD_GIRTH,
                height,
                footprint.depth * SHROUD_GIRTH,
            ))),
            MeshMaterial3d(material),
            // From the ground up, each agent's share stacked on the last. No
            // gap between them, unlike the column: these are one house being
            // covered rather than separate things standing on each other, and
            // a stripe of bare wall between two blocks would read as the
            // house showing through.
            Transform::from_xyz(center[0], covered + height * 0.5, center[1]),
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

    /// **The King's own comparison, as arithmetic.** He reported that a `+8`
    /// looked about the same size and height as a `+100`, and he was right: the
    /// logarithm drew them 1.9x apart, on a column standing on a roofline that
    /// itself varies by more than that.
    ///
    /// Height alone is asserted first, because height is the word he used and
    /// the thing the eye compares between two columns of different widths.
    #[test]
    fn a_hundred_line_change_towers_over_an_eight_line_one() {
        let small = band_height(8.0);
        let large = band_height(100.0);
        assert!(
            large / small >= 3.0,
            "a +100 stands only {:.2}x a +8 ({large} against {small}); \
             the logarithm managed 1.91x and that was the reported fault",
            large / small
        );
        // And the face each presents -- height by girth -- spreads further
        // still, because girth is a genuinely different curve now.
        let face = |churn: f32| band_height(churn) * band_girth(churn);
        assert!(
            face(100.0) / face(8.0) > 5.0,
            "the two channels together spread only {:.2}x",
            face(100.0) / face(8.0)
        );
    }

    /// The distribution the curve is fitted to, walked one quartile at a time.
    ///
    /// These are this repository's own per-file added-line counts over 400
    /// commits -- p25, median, p75, p90 -- and they are the range the map has to
    /// resolve. Each neighbouring pair must be *visibly* apart, which is the
    /// whole of what the King asked for.
    ///
    /// Stated as consecutive steps rather than end to end, because a curve can
    /// spread its ends handsomely and still be flat through the middle -- which
    /// is exactly what the log did: 27 to 115 is 4.3x the work and it drew
    /// 1.37x.
    ///
    /// # Why the last step is allowed to be smaller
    ///
    /// Because it *is* smaller: p75 to p90 is 2.1x the lines where the two
    /// steps below it are 4.4x. A curve that drew all three the same distance
    /// apart would be misreporting the distribution rather than resolving it.
    /// So the floor is what a step must clear to be seen at all, and the second
    /// figure -- what the logarithm drew for the same pair -- is what makes
    /// each one an improvement rather than merely adequate.
    #[test]
    fn every_step_through_a_real_distribution_is_visible() {
        // from, to, and the height ratio the logarithm managed for that pair.
        const STEPS: [(f32, f32, f32); 3] =
            [(6.0, 27.0, 1.58), (27.0, 115.0, 1.37), (115.0, 246.0, 1.14)];
        for (small, large, was) in STEPS {
            let step = band_height(large) / band_height(small);
            assert!(
                step > 1.25,
                "+{large} stands only {step:.2}x a +{small}, which reads as the same column"
            );
            assert!(
                step > was * 1.15,
                "+{small} to +{large} moved {step:.2}x against the logarithm's {was:.2}x, \
                 which is not the fix the King asked for"
            );
        }
    }

    /// **The plateau, which must not come back.** The old `FULL_CHURN = 600`
    /// clamped, so every change past it drew identically -- and p99 in this
    /// repository is 935 lines, with single files reaching 3,872. "Large" and
    /// "enormous" were one mark.
    ///
    /// `magnitude` saturates instead, so this holds at any size.
    #[test]
    fn a_very_large_change_still_grows() {
        assert!(
            band_height(4_000.0) > band_height(600.0),
            "600 and 4,000 lines draw the same column, which is the old clamp"
        );
        assert!(band_height(935.0) > band_height(600.0));
        assert!(band_height(20_000.0) > band_height(4_000.0));
        // And nothing a real repository can produce reaches the reach, which is
        // what lets the curve keep growing without a ceiling to hit. Only
        // `f32::MAX` saturates it, and that is arithmetic rather than a change.
        assert!(band_height(1_000_000.0) < COLUMN_REACH);
        assert!(band_height(f32::MAX) <= COLUMN_REACH);
    }

    /// **The King's own rule, and the reason this ruler was replaced a third
    /// time.** Twice the lines is twice the column -- exactly, not nearly.
    ///
    /// Measured above the floor, because the floor is a fixed offset that every
    /// column carries: what has to be proportional is the part that says how
    /// much moved. `BAND_FLOOR`'s own docs record how far it can be cut before
    /// a one-line change stops being a mark at all.
    ///
    /// The tolerance is float slop, not slack in the rule. The saturating curve
    /// this replaced managed 1.24x here, and the logarithm before it 1.24x from
    /// a base already a fifth of the way up.
    #[test]
    fn doubling_a_change_doubles_the_column() {
        let above_floor = |churn: f32| band_height(churn) - BAND_FLOOR;
        for (small, large) in [(4.0, 8.0), (8.0, 16.0), (50.0, 100.0), (75.0, 150.0)] {
            let step = above_floor(large) / above_floor(small);
            assert!(
                (step - 2.0).abs() < 0.01,
                "doubling {small} lines to {large} moved the column {step:.3}x, \
                 which is not the proportional rule the footprint already follows"
            );
        }
    }

    /// The same rule at any ratio, not only at doubling, and through the
    /// quartiles of a real distribution: the column is the work, to scale.
    #[test]
    fn a_column_is_proportional_to_the_lines_it_stands_for() {
        let above_floor = |churn: f32| band_height(churn) - BAND_FLOOR;
        for (small, large) in [(6.0, 27.0), (27.0, 115.0), (115.0, 246.0), (10.0, 250.0)] {
            let drawn = above_floor(large) / above_floor(small);
            let want = large / small;
            assert!(
                (drawn / want - 1.0).abs() < 0.01,
                "+{large} against +{small} is {want:.2}x the work and drew {drawn:.2}x"
            );
        }
    }

    /// Girth is linear too, at the King's word -- so the whole silhouette of a
    /// column, not merely its height, is proportional to the change in it.
    ///
    /// Measured above `GIRTH_RANGE.0` for `band_height`'s reason: the floor is
    /// the part that exists to keep a small mark visible, and the ramp above it
    /// is the part that carries the size.
    #[test]
    fn doubling_a_change_doubles_the_columns_width_too() {
        let above_floor = |churn: f32| band_girth(churn) - GIRTH_RANGE.0;
        for (small, large) in [(8.0, 16.0), (50.0, 100.0), (100.0, 200.0)] {
            let step = above_floor(large) / above_floor(small);
            assert!(
                (step - 2.0).abs() < 0.01,
                "doubling {small} lines to {large} widened the column {step:.3}x"
            );
        }
    }

    /// A ghost house stands like a house: never a slab, never a spike.
    ///
    /// The floor and the cap are one decision and have to be tested together.
    /// The floor exists because an honest low end (see [`LINEAR_CHURN`]) would
    /// otherwise draw a small new file a few units high; the cap exists because
    /// a ward with no room shrinks a ghost's footprint
    /// (`map::works::SHRINK`, three times over) and an absolute floor over a
    /// quarter-sized lot is a mast.
    ///
    /// The arithmetic is lifted from `raise_one` rather than called, because it
    /// is three lines inside a function that needs a Bevy world. What is pinned
    /// is the *bound*, which is the part that was reasoned about.
    #[test]
    fn a_ghost_house_is_shaped_like_a_house_on_any_lot() {
        let ghost = |churn: f32, plan: f32| {
            (band_height(churn) * 0.5)
                .max(GHOST_FLOOR)
                .min(plan * GHOST_ASPECT)
        };
        // An ordinary lot: a tiny new file still gets a building rather than a
        // slab lying on the ground.
        assert_eq!(ghost(3.0, 20.0), GHOST_FLOOR);
        // A lot shrunk three times over in a packed ward. The floor gives way
        // to the proportion, so the house is small rather than a spike.
        let cramped = 4.0;
        assert!(ghost(3.0, cramped) < GHOST_FLOOR);
        // And on every lot a ghost keeps a holding's proportions, which is the
        // silhouette rule the whole cap exists for.
        for plan in [2.0, 4.0, 12.0, 20.0, 60.0] {
            for churn in [1.0, 40.0, 400.0, 4_000.0] {
                let height = ghost(churn, plan);
                assert!(
                    height <= plan * GHOST_ASPECT,
                    "a {churn}-line ghost on a {plan}-wide lot stood {height}, which is a spike"
                );
                assert!(height > 0.0);
            }
        }
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

    /// **The second reported bug, and the mark that answers it now.** A removal
    /// used to be drawn twice -- this shroud, and a stain spreading across the
    /// ground around the house. The King reported seeing only the shroud, so the
    /// stain is gone and this is the one mark a removal makes.
    ///
    /// What the stain was for is not forgotten: it was the mark that survived at
    /// the zoom where a house is two pixels across. The shroud has to carry that
    /// now, which is what `SHROUD_FLOOR` and `SHROUD_GIRTH` are between them
    /// for -- a cut always covers a visible share of a house, and the block is
    /// wider than any roof on the map.
    #[test]
    fn a_removal_is_drawn_at_a_size_that_can_be_seen() {
        // A typical holding, and the least a cut of any size covers of it.
        let house = 32.0_f32;
        let smallest = shroud_height(1.0 / 4_000.0, house);
        assert!(
            smallest > 1.0,
            "the smallest cut covers only {smallest} units of a {house} house, \
             which is what being invisible looked like"
        );
        // And a big house gets proportionally more, which an absolute could not.
        let big = shroud_height(0.5, 80.0);
        assert!(
            big > shroud_height(0.5, house) * 2.0,
            "the shroud must follow the house it covers"
        );
        // That the block is also wider than every roof on the map is a fact
        // about literals, so it is a `const` assertion at `SHROUD_GIRTH`
        // itself rather than a check that could only fail on a build already
        // made.
    }

    /// **The King's instruction, stated as arithmetic.** A band is exactly its
    /// agent's colour -- not a dimmed version of it, and not one that moves.
    ///
    /// This replaces `a_band_keeps_its_agents_hue_throughout_its_breath`, which
    /// sampled the colour across a 2.4-second cycle and asserted only that the
    /// hue survived it. There is no cycle now, so the far stronger property is
    /// available: the colour *is* the banner's, channel for channel.
    ///
    /// The reasoning the old test guarded is not lost -- see
    /// `activity::WORKING_COLOR` for the three attempts at a lit status colour
    /// that ended in white, mint and near-white, which is why everything here
    /// is unlit.
    #[test]
    fn a_band_is_exactly_its_agents_colour() {
        for banner in kingdom_core::palette::BANNERS {
            for rgb in [banner.growth_rgb, banner.cutting_rgb] {
                let base = to_color([rgb[0], rgb[1], rgb[2], 255]);
                // `to_color` hands back sRGB and `band_color` answers in
                // linear, so the comparison is made in the one they can share.
                let want = base.to_linear();
                let Color::LinearRgba(got) = band_color(base, WORKS_ALPHA) else {
                    panic!("the works should be linear rgba");
                };
                assert_eq!(
                    (got.red, got.green, got.blue),
                    (want.red, want.green, want.blue),
                    "{} was not drawn in its own colour",
                    banner.name
                );
            }
        }
    }

    /// A proposal is not the city. Solid works would say the King had already
    /// approved them.
    #[test]
    fn the_works_are_translucent() {
        let Color::LinearRgba(c) = band_color(Color::WHITE, WORKS_ALPHA) else {
            panic!("linear rgba");
        };
        assert!(c.alpha < 1.0, "a proposal must not look built");
        // A ghost is fainter still: nothing of it exists on disk yet.
        const { assert!(GHOST_ALPHA < WORKS_ALPHA) };
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
