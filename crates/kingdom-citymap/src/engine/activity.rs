//! Showing which towns have agents working in them.
//!
//! This is the map's answer to the first of the three questions in `AGENTS.md`
//! -- *what is every agent doing right now?* -- at the tier where nothing else
//! about a town is legible. A town with a turn in flight is traced with a slow
//! green pulse; a quiet one is drawn exactly as it always was.
//!
//! # Why this does not come through the manifest
//!
//! `kingdom_app::citymap` memoises the whole map JSON -- seconds of filesystem
//! work and megabytes of geometry -- keyed on the kingdom root and its city
//! names, and deliberately not on anything that changes often. A fact that
//! moves every few seconds either defeats that cache or goes quietly stale
//! inside it. So activity arrives on its own, as a
//! [`ViewerCommand::SetActivity`](super::bridge::ViewerCommand::SetActivity),
//! and the manifest is untouched.
//!
//! # What is animated, and what is not
//!
//! The ring geometry is built once when a world loads and never rebuilt: a
//! change in activity costs a visibility flag and a colour, not a respawn. The
//! pulse itself writes `base_color` on each lit ring's **own** material, which
//! is why [`super::spawn`] does not take those handles from the
//! [`MaterialCache`](super::materials::MaterialCache) -- that cache quantises
//! by colour and shares one handle across hundreds of meshes, so animating a
//! cached material would pulse whatever else happened to land in its bucket.
//!
//! # Why the ring is unlit
//!
//! Everything else on this map is a *surface*: it takes the manifest's sun, it
//! has roughness, and looking right means looking lit. The ring is not. It is a
//! piece of interface that happens to be drawn in world space, and its colour
//! carries the whole of its meaning -- it is the same green the rail badge uses
//! for the same plan. Rendered as a lit material it picked up the sun's white
//! specular and came out mint; see [`PULSE_PEAK`] for the three attempts that
//! established this.

use bevy::prelude::*;

use super::bridge::TownActivity;

/// The colour a working town is traced in.
///
/// The same green `PlanStatus::Drafting` reports and `$working` paints the rail
/// badge with. A test below pins it against `kingdom-core` so the two cannot
/// drift silently -- the rail and the map saying different things about one
/// plan is exactly the confusion this feature exists to remove.
pub const WORKING_COLOR: [u8; 4] = [0x22, 0xc5, 0x5e, 255];

/// How long one breath of the pulse takes, in seconds.
///
/// Slow on purpose. This is ambient status the King reads at a glance while
/// looking at something else, not an alert demanding his eye.
const PULSE_SECONDS: f32 = 2.4;

/// How far the glow dips at the bottom of a breath, as a fraction of full.
const PULSE_FLOOR: f32 = 0.45;

/// How bright the ring is at the top of a breath, as a fraction of full colour.
///
/// The ring is drawn **unlit**, and that is the whole of what makes it read as
/// green. Three attempts got here. Emissive is in linear-RGB units and
/// `StandardMaterial::emissive_exposure_weight` defaults to `0.0`, so a value
/// scaled for the sun's lux (`REFERENCE_ILLUMINANCE`) clipped every channel and
/// the ring rendered pure white; pulling it just over 1.0 landed where the
/// tonemapper desaturates highlights and it rendered pale mint; and even at
/// 0.8 the measured pixels came back `(168, 231, 167)` -- red and blue almost
/// equal, because a *lit* surface adds the sun's white specular on top of
/// whatever the emissive contributes.
///
/// A status colour is not a material in a scene. It is a piece of interface
/// that happens to be drawn in world space, so it is `unlit` and its colour is
/// exactly the colour asked for, and the pulse dims that colour rather than
/// adding light to it.
const PULSE_PEAK: f32 = 1.0;

/// Which towns are working, as last reported by the interface.
///
/// Empty is the common answer on a real dev folder, and the systems below lean
/// on that: an idle map costs one resource read per frame.
#[derive(Resource, Default, Debug, Clone, PartialEq, Eq)]
pub struct Activity(pub Vec<TownActivity>);

impl Activity {
    /// How many plans are working in a named town. Zero when none are.
    pub fn working_in(&self, town: &str) -> usize {
        self.0
            .iter()
            .find(|entry| entry.town == town)
            .map(|entry| entry.working)
            .unwrap_or(0)
    }

    /// Whether anything at all is running, which is what lets the pulse skip
    /// its work on a quiet map.
    pub fn is_quiet(&self) -> bool {
        self.0.is_empty()
    }
}

/// A ring traced around one town, shown only while that town is working.
///
/// Carries its own material handle rather than reading it back off the entity,
/// so the pulse never has to ask which of a shared cache's handles is safe to
/// write to. See the module docs.
#[derive(Component, Clone)]
pub struct TownRing {
    /// The town this ring belongs to, by name.
    pub town: String,
    /// This ring's own material, animated by [`pulse_rings`].
    pub material: Handle<StandardMaterial>,
}

/// Shows or hides each ring as the reported activity changes.
///
/// Only runs on a change. A ring going dark is faded to its resting colour as
/// well as hidden, so that a town coming back to life starts its breath from
/// rest rather than resuming mid-glow.
pub fn apply_activity(
    activity: Res<Activity>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut rings: Query<(&TownRing, &mut Visibility)>,
) {
    if !activity.is_changed() {
        return;
    }
    for (ring, mut visibility) in rings.iter_mut() {
        let working = activity.working_in(&ring.town) > 0;
        let wanted = if working {
            Visibility::Inherited
        } else {
            Visibility::Hidden
        };
        if *visibility != wanted {
            *visibility = wanted;
        }
        if !working && let Some(mut material) = materials.get_mut(&ring.material) {
            material.base_color = ring_color(PULSE_FLOOR);
        }
    }
}

/// Breathes the colour of every lit ring.
///
/// Every lit ring breathes in step. That is deliberate: staggering them by town
/// would read as several unrelated things blinking, where the point is one
/// state shared by several places.
pub fn pulse_rings(
    time: Res<Time>,
    activity: Res<Activity>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    rings: Query<(&TownRing, &Visibility)>,
) {
    if activity.is_quiet() {
        return;
    }
    let color = ring_color(glow(time.elapsed_secs()));
    for (ring, visibility) in rings.iter() {
        if *visibility == Visibility::Hidden {
            continue;
        }
        if let Some(mut material) = materials.get_mut(&ring.material) {
            material.base_color = color;
        }
    }
}

/// The ring's colour at a given point in its breath.
///
/// Dims [`WORKING_COLOR`] toward black rather than lightening it toward white:
/// the hue is the message, and the brightness is only what draws the eye.
pub fn ring_color(strength: f32) -> Color {
    let base = super::materials::to_color(WORKING_COLOR).to_linear();
    Color::LinearRgba(LinearRgba {
        red: base.red * strength,
        green: base.green * strength,
        blue: base.blue * strength,
        alpha: 1.0,
    })
}

/// How brightly a working ring burns at a given moment, as a fraction of full
/// colour.
///
/// Pure, and the only place the animation actually lives, so its shape can be
/// pinned without a window or a render device -- the way the rest of this
/// engine's maths is tested.
pub fn glow(seconds: f32) -> f32 {
    let phase = seconds / PULSE_SECONDS * std::f32::consts::TAU;
    // sin maps to 0..1 first, so the floor is a floor rather than a midpoint.
    let wave = (phase.sin() + 1.0) * 0.5;
    PULSE_PEAK * (PULSE_FLOOR + (1.0 - PULSE_FLOOR) * wave)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn working(town: &str, count: usize) -> TownActivity {
        TownActivity {
            town: town.to_owned(),
            working: count,
        }
    }

    #[test]
    fn a_town_nobody_reported_is_a_town_with_nothing_running() {
        let activity = Activity(vec![working("forge", 2)]);
        assert_eq!(activity.working_in("forge"), 2);
        assert_eq!(activity.working_in("archive"), 0);
        assert!(!activity.is_quiet());
        assert!(Activity::default().is_quiet());
    }

    /// The glow must never reach zero while a town is working: a ring that goes
    /// fully dark between breaths reads as the work having stopped.
    #[test]
    fn the_pulse_dips_but_never_goes_out() {
        let samples: Vec<f32> = (0..240)
            .map(|step| glow(step as f32 * PULSE_SECONDS / 60.0))
            .collect();
        let lowest = samples.iter().copied().fold(f32::MAX, f32::min);
        let highest = samples.iter().copied().fold(f32::MIN, f32::max);

        assert!(lowest > 0.0, "the ring went out at {lowest}");
        // The dip stays well clear of black: the pulse should read as breathing
        // rather than blinking, and a ring that nearly vanishes between breaths
        // reads as the work having stopped.
        assert!(
            lowest > PULSE_PEAK * 0.3,
            "the ring dips too far toward black: {lowest}"
        );
        assert!(
            (highest - PULSE_PEAK).abs() < PULSE_PEAK * 0.02,
            "the peak should reach full, got {highest}"
        );
    }

    /// One breath, and the same brightness one period later. A pulse that drifts
    /// would beat against nothing in particular and look like a bug.
    #[test]
    fn the_pulse_repeats_on_its_period() {
        for step in 0..8 {
            let at = step as f32 * PULSE_SECONDS / 8.0;
            let later = glow(at + PULSE_SECONDS);
            assert!(
                (glow(at) - later).abs() < PULSE_PEAK * 0.001,
                "drifted at {at}: {} vs {later}",
                glow(at)
            );
        }
    }

    /// The regression that got past three attempts at this. The ring must stay
    /// **recognisably green** at every point in its breath, and the failures
    /// were all in the bright direction: emissive scaled for the sun's lux
    /// clipped to white, a value near 1.0 was washed out by the tonemapper, and
    /// a lit material added white specular on top. Hence unlit, and hence a
    /// pulse that dims toward black rather than brightening toward white.
    ///
    /// Sampled across a whole breath, because "green at the peak" was true of
    /// two of the versions that looked wrong on screen.
    #[test]
    fn the_ring_is_recognisably_green_throughout_its_breath() {
        for step in 0..24 {
            let at = step as f32 * PULSE_SECONDS / 24.0;
            let Color::LinearRgba(c) = ring_color(glow(at)) else {
                panic!("the ring's colour should be linear rgba");
            };

            assert!(
                c.green > c.red * 2.0 && c.green > c.blue * 1.5,
                "the ring lost its hue at {at}s: {c:?}"
            );
            // Never brighter than the colour itself, which is what keeps the
            // display from clamping the channels together into white.
            assert!(c.green <= 1.0, "the ring can clip to white at {at}s: {c:?}");
            assert!(c.alpha == 1.0, "the ring is drawn opaque");
        }
    }

    /// The rail badge and the map must say the same green about the same plan.
    ///
    /// `kingdom-core` is a dev-dependency of this crate for exactly this
    /// assertion: the engine cannot depend on the domain model, but a test can
    /// check that the constant copied out of it is still what it says.
    #[test]
    fn a_working_town_is_the_colour_a_working_plan_is() {
        let expected = kingdom_core::PlanStatus::Drafting.color();
        let actual = format!(
            "#{:02x}{:02x}{:02x}",
            WORKING_COLOR[0], WORKING_COLOR[1], WORKING_COLOR[2]
        );
        assert_eq!(
            actual, expected,
            "the ring must be the green the rail badge uses"
        );
        assert_eq!(WORKING_COLOR[3], 255, "the ring is drawn opaque");
    }
}
