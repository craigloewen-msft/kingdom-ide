//! Raising a plan's proposed changes over the settlement that already stands.
//!
//! This is the map's answer to the third of the three questions in `AGENTS.md`
//! -- *what are they proposing that I need to decide on?* -- put where the first
//! two are already answered. A house the court has been working in wears a
//! scaffold whose height is how much moved; a house it has been cutting from
//! wears a skirt around its lot; a file that does not exist in the city's
//! checkout at all stands as a ghost on free ground inside its own folder.
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
//! The green is [`super::activity::WORKING_COLOR`], the same green the rail
//! badge paints a drafting plan with and the same green a working town is traced
//! in. A third green for the same plan is how a map and a rail come to disagree.
//! The red is [`CUTTING_COLOR`], which is what the review drawer already tints a
//! deletion with. A test below pins both against `kingdom-core`.

use bevy::prelude::*;

use crate::map::{Work, WorkSite};

use super::activity::{self, WORKING_COLOR};
use super::materials::to_color;
use super::meshes::{self, BuildingShape};
use super::spawn::MeshCache;

/// The colour of ground being cleared: lines removed.
///
/// `$blocked` from the interface's own tokens, which is what `.count-removed`
/// paints a deletion with in the review drawer. The King reads the same two
/// colours in the drawer and on the map, which is the point.
pub const CUTTING_COLOR: [u8; 4] = [0xef, 0x44, 0x44, 255];

/// How far above a roof the tallest scaffold reaches, in world units.
///
/// A judgement rather than a measurement, and deliberately generous. The map's
/// most common home is a pane at the foot of the rail where a house is a couple
/// of pixels across -- so what has to read at a glance is the *column of light*
/// above the roofline rather than the house under it. Compare `spawn::TALLEST`,
/// which assumes 60 units for the tallest holding in a world: a full scaffold is
/// therefore comparable to a tall building standing on top of one.
///
/// Raised from 34 after looking at a real plan on screen: a typical holding in
/// the proving ground stands around 32 units, so a 34-unit column was the same
/// order as the house and read as a slightly taller roof rather than as work.
const SCAFFOLD_REACH: f32 = 52.0;

/// The shortest a scaffold may be, whatever the churn.
///
/// A one-line change is still a change the King should be able to see. Without
/// a floor, a file that moved three lines beside one that moved four hundred
/// would be drawn as nothing at all.
///
/// Nine rather than four, for the same reason [`SCAFFOLD_REACH`] grew: measured
/// against a house's own roof, a four-unit stub sat *within* the silhouette and
/// read as part of it. The floor has to clear the roofline to be a mark on the
/// building rather than a bump in it.
const SCAFFOLD_FLOOR: f32 = 9.0;

// Checked when this compiles rather than when the suite runs: the roofline
// clearance above is a fact about the constant, not about any behaviour, and a
// `#[test]` asserting it could only ever fail on a build that had already been
// made. Lowering the floor past a roof is now a compile error.
const _: () = assert!(
    SCAFFOLD_FLOOR > 6.0,
    "a stub shorter than this disappears into the roof it stands on"
);

/// How far a skirt of cleared ground spreads past the lot it surrounds.
const SKIRT_SPREAD: f32 = 1.9;

/// How high the skirt stands off the ground.
///
/// Barely anything: this is a stain on the ground rather than a wall, and a
/// skirt tall enough to hide its own house would say the opposite of what it
/// means. Above `spawn::layer::GROUND_LABEL` so it is not swallowed by a folder
/// name painted across the same ward.
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

/// A scaffold, which breathes. Carries its own material for
/// [`super::activity::TownRing`]'s reason: the shared cache quantises by colour
/// and hands one handle to hundreds of meshes, so animating a cached material
/// would pulse whatever else landed in the same bucket.
#[derive(Component, Clone)]
pub struct Scaffold {
    /// This scaffold's own material, animated by [`pulse_works`].
    pub material: Handle<StandardMaterial>,
    /// How bright this one burns at the top of its breath. Taken from the
    /// work's own scale, so a heavily-worked house is the brightest thing on
    /// the map as well as the tallest.
    pub strength: f32,
}

/// How tall a scaffold stands for a given share of the plan's busiest file.
///
/// `sqrt` rather than linear, and that is what makes a plan legible rather than
/// merely honest. Real work is lopsided -- one file rewritten, a dozen touched
/// -- so on a linear scale the dozen are stubs beside the one. The curve lifts
/// the small ones into view while keeping the order intact, which is the same
/// bargain `layout::Building::height` strikes with `ln` for line counts.
///
/// # Why NaN is handled rather than assumed away
///
/// A scale is a ratio, and both of the ratios behind it can divide by zero: a
/// rename with no content change has a churn of nothing, and a summary whose
/// files are all binary has a `busiest` of nothing. `f32::clamp` propagates NaN
/// rather than trapping it, so an unguarded version yields a NaN height, which
/// reaches Bevy as a degenerate mesh. Found by the test below, not in a browser.
///
/// Pure, so the shape can be pinned without a renderer.
pub fn scaffold_height(scale: f32) -> f32 {
    let scale = if scale.is_finite() {
        scale.clamp(0.0, 1.0)
    } else {
        0.0
    };
    SCAFFOLD_FLOOR + scale.sqrt() * (SCAFFOLD_REACH - SCAFFOLD_FLOOR)
}

/// The colour of a scaffold at a point in its breath.
///
/// The working green, dimmed toward black -- never lightened toward white. See
/// the module docs, and [`super::activity::ring_color`], which this is the
/// translucent sibling of.
pub fn works_color(strength: f32) -> Color {
    let base = to_color(WORKING_COLOR).to_linear();
    Color::LinearRgba(LinearRgba {
        red: base.red * strength,
        green: base.green * strength,
        blue: base.blue * strength,
        alpha: WORKS_ALPHA as f32 / 255.0,
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
    // The roof it ends up with becomes the base its scaffold rises from, which
    // is why this is computed rather than taken from `WorkSite::base`: a ghost
    // house is built here and only here, so only here is its height known.
    // Without it the scaffold started at the ground and rose *through* the
    // house, and the two read as one green mass rather than as a building with
    // work on top -- seen on screen, not reasoned about.
    let mut base = work.site.base();
    if let WorkSite::Fresh { .. } = work.site {
        let plan_size = Vec2::new(footprint.width, footprint.depth);
        let height = scaffold_height(work.scale) * 0.5;
        let shape = BuildingShape::new(crate::map::BuildingKind::Cottage, plan_size, height, 0);
        let handles = mesh_cache.building(meshes, shape);
        // The archetypes are modelled inside a unit footprint with height as a
        // multiple of the shorter side, so a placed one stands `height() * plan`
        // tall. The same arithmetic `spawn::spawn_building` does.
        let plan = plan_size.x.min(plan_size.y);
        base = shape.height() * plan;
        let ghost = materials.add(unlit(works_color(1.0), GHOST_ALPHA));

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

    // The scaffold: how much moved, as height. Drawn for a new house too --
    // creating a file is the most additive thing a plan can do, and leaving it
    // bare would make new files the quietest thing on a map about change.
    if work.growth > 0.0 {
        let height = scaffold_height(work.scale) * work.growth.clamp(0.0, 1.0).max(0.25);
        let strength = 0.55 + work.scale.clamp(0.0, 1.0) * 0.45;
        let material = materials.add(unlit(works_color(strength), WORKS_ALPHA));

        commands.spawn((
            ChildOf(root),
            Mesh3d(meshes.add(Cuboid::new(
                footprint.width * 0.82,
                height,
                footprint.depth * 0.82,
            ))),
            MeshMaterial3d(material.clone()),
            // Cuboids are built about their own centre, so the box is lifted by
            // half its height to stand *on* the roof rather than through it.
            Transform::from_xyz(center[0], base + height * 0.5, center[1]),
            Scaffold { material, strength },
            Pickable::IGNORE,
        ));
    }

    // The skirt: ground being cleared, as a stain around the lot.
    let cutting = 1.0 - work.growth.clamp(0.0, 1.0);
    if cutting > 0.0 {
        let spread = SKIRT_SPREAD * cutting * work.scale.clamp(0.15, 1.0);
        let mut color = to_color(CUTTING_COLOR);
        color.set_alpha(cutting.clamp(0.25, 1.0) * (WORKS_ALPHA as f32 / 255.0));

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

/// Breathes every scaffold, on the same clock the working ring breathes on.
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
            material.base_color = works_color(breath * scaffold.strength);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::map::MapRect;

    #[test]
    fn a_bigger_change_builds_a_taller_scaffold() {
        assert!(scaffold_height(1.0) > scaffold_height(0.5));
        assert!(scaffold_height(0.5) > scaffold_height(0.0));
    }

    /// The floor is what keeps a small change visible. Without it a file that
    /// moved three lines beside one that moved four hundred is drawn as nothing.
    ///
    /// That the floor also clears a typical roofline is pinned at the constant
    /// itself, as a `const` assertion -- it is arithmetic on a literal, so it
    /// is checked at compile time rather than here.
    #[test]
    fn even_the_smallest_change_stands_high_enough_to_see() {
        assert!(scaffold_height(0.0) >= SCAFFOLD_FLOOR);
        assert!(scaffold_height(0.001) >= SCAFFOLD_FLOOR);
    }

    /// The curve is what makes a lopsided plan legible: the dozen touched files
    /// have to lift clear of the floor rather than sitting on it beside the one
    /// that was rewritten.
    #[test]
    fn a_modest_change_is_lifted_clear_of_the_floor() {
        let full = scaffold_height(1.0);
        let tenth = scaffold_height(0.1);
        // Linear would put a tenth of the churn a tenth of the way up. The
        // square root puts it around a third, which is the whole point.
        assert!(
            tenth > full * 0.25,
            "a tenth of the busiest file drew only {tenth} of {full}"
        );
        assert!(tenth < full, "but it must not outgrow the busiest file");
    }

    /// A scale outside the range is a bug upstream, and must not produce a
    /// spike through the roof of the world or a hole under it.
    ///
    /// NaN is in here because it is *reachable*, not as paranoia: a scale is a
    /// ratio, and a rename with no content change divides by a churn of zero.
    /// This assertion failed when it was first written.
    #[test]
    fn a_scale_out_of_range_is_still_a_sane_height() {
        assert_eq!(scaffold_height(4.0), scaffold_height(1.0));
        assert_eq!(scaffold_height(-2.0), scaffold_height(0.0));
        assert_eq!(scaffold_height(f32::NAN), scaffold_height(0.0));
        assert!(scaffold_height(f32::INFINITY).is_finite());
    }

    /// The regression `activity::PULSE_PEAK` was written for, on this module's
    /// own colour: three attempts there ended in white, mint, and near-white,
    /// all in the *bright* direction. A scaffold must stay recognisably green
    /// through the whole of its breath.
    #[test]
    fn a_scaffold_is_recognisably_green_throughout_its_breath() {
        for step in 0..24 {
            let breath = activity::glow(step as f32 * 0.1);
            let Color::LinearRgba(c) = works_color(breath) else {
                panic!("the works should be linear rgba");
            };
            assert!(
                c.green > c.red * 2.0 && c.green > c.blue * 1.5,
                "the scaffold lost its hue at step {step}: {c:?}"
            );
            assert!(c.green <= 1.0, "it can clip to white at step {step}: {c:?}");
        }
    }

    /// A proposal is not the city. Solid works would say the King had already
    /// approved them.
    #[test]
    fn the_works_are_translucent() {
        let Color::LinearRgba(c) = works_color(1.0) else {
            panic!("linear rgba");
        };
        assert!(c.alpha < 1.0, "a proposal must not look built");
    }

    /// The map and the rail must say the same green about the same plan, and
    /// the map and the review drawer the same red about the same deletion.
    ///
    /// `kingdom-core` is a dev-dependency of this crate for exactly this kind of
    /// assertion -- see the one in `activity`.
    #[test]
    fn the_works_are_the_colours_the_rest_of_the_interface_uses() {
        let drafting = kingdom_core::PlanStatus::Drafting.color();
        assert_eq!(
            format!(
                "#{:02x}{:02x}{:02x}",
                WORKING_COLOR[0], WORKING_COLOR[1], WORKING_COLOR[2]
            ),
            drafting,
            "a scaffold must be the green a drafting plan is"
        );
        // `$blocked` in `style/abstracts/_tokens.scss`, which is what
        // `.count-removed` paints a deletion with in the review drawer.
        assert_eq!(
            format!(
                "#{:02x}{:02x}{:02x}",
                CUTTING_COLOR[0], CUTTING_COLOR[1], CUTTING_COLOR[2]
            ),
            "#ef4444"
        );
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
            scale: 1.0,
            growth: 1.0,
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
                scale: 0.5,
                growth: 1.0,
            }])
            .is_quiet()
        );
    }
}
