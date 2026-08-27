//! Showing which towns have agents working in them.
//!
//! This is the map's answer to the first of the three questions in `AGENTS.md`
//! -- *what is every agent doing right now?* -- at the tier where nothing else
//! about a town is legible. A town with a turn in flight is traced in green; a
//! quiet one is drawn exactly as it always was.
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
//! # What is animated: nothing
//!
//! The ring used to breathe on a 2.4-second cycle, and the works standing
//! inside it breathed on the same clock. Both are still at the King's word, so
//! a working town is simply **traced or not traced** -- one visibility flag and
//! a colour set once, which is all this ever needed to say. The ring geometry
//! was already built once with the world and never rebuilt; now its material is
//! never written either, so it can come from the shared
//! [`MaterialCache`](super::materials::MaterialCache) like every other unlit
//! surface on the map. It could not before: the cache quantises by colour and
//! hands one handle to hundreds of meshes, so animating a cached material would
//! have pulsed whatever else landed in the same bucket.
//!
//! # Why a town is traced twice
//!
//! A ring is a fixed width in world units and the map is drawn at wildly
//! different zooms, so one width cannot serve both ends of it: the weight that
//! is a bold band over a single street is a two-pixel hairline once the whole
//! realm is in frame. Since that far end is precisely where nothing else about
//! a town is legible, each town gets both weights and the detail tier chooses
//! between them. See [`RingTier`] and [`shows`].
//!
//! # Why the ring is unlit
//!
//! Everything else on this map is a *surface*: it takes the manifest's sun, it
//! has roughness, and looking right means looking lit. The ring is not. It is a
//! piece of interface that happens to be drawn in world space, and its colour
//! carries the whole of its meaning -- it is the same green the rail badge uses
//! for the same plan. Rendered as a lit material it picked up the sun's white
//! specular and came out mint; see [`WORKING_COLOR`] for the three attempts
//! that established this.

use bevy::prelude::*;

use super::bridge::{LodLevel, TownActivity};
use super::lod::ActiveLod;

/// The colour a working town is traced in.
///
/// The same green `PlanStatus::Drafting` reports and `$working` paints the rail
/// badge with. A test below pins it against `kingdom-core` so the two cannot
/// drift silently -- the rail and the map saying different things about one
/// plan is exactly the confusion this feature exists to remove.
///
/// # Why it is drawn unlit, and exactly this colour
///
/// Three attempts got here, and all three failed in the bright direction.
/// Emissive is in linear-RGB units and `StandardMaterial::
/// emissive_exposure_weight` defaults to `0.0`, so a value scaled for the sun's
/// lux (`REFERENCE_ILLUMINANCE`) clipped every channel and the ring rendered
/// pure white; pulling it just over 1.0 landed where the tonemapper desaturates
/// highlights and it rendered pale mint; and even at 0.8 the measured pixels
/// came back `(168, 231, 167)` -- red and blue almost equal, because a *lit*
/// surface adds the sun's white specular on top of whatever the emissive
/// contributes.
///
/// A status colour is not a material in a scene. It is a piece of interface
/// that happens to be drawn in world space, so it is `unlit` and its colour is
/// exactly the colour asked for. `engine::works` reaches the same conclusion
/// for the bands, and cites this.
pub const WORKING_COLOR: [u8; 4] = [0x22, 0xc5, 0x5e, 255];

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

/// Which of a town's two rings this is.
///
/// A ring is a fixed width in *world* units, and the map is drawn at wildly
/// different zooms, so one width cannot serve both ends of it. At the
/// [`LodLevel::FileDetail`] end nine units is a bold band; pulled back to the
/// zoom-out limit the same nine units come out around two and a half pixels --
/// a hairline barely heavier than a ward kerb, at exactly the tier where the
/// ring is the *only* thing left that can say an agent is here.
///
/// So each town is traced twice, and the tier picks which trace is shown. See
/// [`shows`], and `spawn::BOLD_TOWN_RING_WIDTH` for the widths and the
/// arithmetic behind them.
#[derive(Component, Clone, Copy, Debug, PartialEq, Eq)]
pub enum RingTier {
    /// The hairline, for the tiers where a town's own detail is legible.
    Fine,
    /// The heavy band, for the tier where nothing else about a town reads.
    Bold,
}

/// Whether a ring of this weight is the one to show at this zoom.
///
/// Pure, so the whole matrix can be pinned without a camera or a render device
/// -- the same reason `lod::reaches` is a free function. The two arms are
/// exhaustive and disjoint on purpose: every tier shows exactly one of a town's
/// rings, so a working town is never drawn with two bands stacked and never
/// drawn with none.
pub fn shows(tier: RingTier, lod: LodLevel) -> bool {
    match tier {
        RingTier::Bold => lod == LodLevel::Districts,
        RingTier::Fine => lod != LodLevel::Districts,
    }
}

/// A ring traced around one town, shown only while that town is working.
///
/// Carries no material handle any more. It used to, so the pulse could write a
/// colour through it each frame without asking which of a shared cache's
/// handles was safe to touch; with nothing animating a ring, its colour is set
/// once when it is spawned and this is only a label saying which town it belongs
/// to and which zoom it is for.
#[derive(Component, Clone)]
pub struct TownRing {
    /// The town this ring belongs to, by name.
    pub town: String,
    /// Which weight this one is drawn at, and therefore which zoom it is for.
    pub tier: RingTier,
}

/// Shows or hides each ring as the reported activity, or the zoom, changes.
///
/// A ring is shown when its town is working **and** its weight is the one this
/// zoom calls for, so this watches [`ActiveLod`] as well as [`Activity`]. Still
/// only on a change of one or the other: an idle map held at a steady zoom
/// costs two resource reads a frame.
///
/// Visibility is now the whole of it. A ring going dark also used to be faded
/// back to its resting colour, so that a town coming back to life started its
/// breath from rest rather than resuming mid-glow -- with no breath to resume,
/// a hidden ring and a shown one are the same green and there is nothing to put
/// back.
pub fn apply_activity(
    activity: Res<Activity>,
    active: Res<ActiveLod>,
    mut rings: Query<(&TownRing, &mut Visibility)>,
) {
    if !activity.is_changed() && !active.is_changed() {
        return;
    }
    for (ring, mut visibility) in rings.iter_mut() {
        let working = activity.working_in(&ring.town) > 0;
        let wanted = if working && shows(ring.tier, active.0) {
            Visibility::Inherited
        } else {
            Visibility::Hidden
        };
        if *visibility != wanted {
            *visibility = wanted;
        }
    }
}

/// The colour a working town is traced in, as the renderer wants it.
///
/// One colour, not a curve: see the module docs for what used to vary it and
/// why nothing does now.
pub fn ring_color() -> Color {
    super::materials::to_color(WORKING_COLOR)
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

    /// **What the King asked for.** The ring does not move.
    ///
    /// This replaces `the_pulse_dips_but_never_goes_out` and
    /// `the_pulse_repeats_on_its_period`, which pinned the breath's floor and
    /// its period -- both facts about an animation there no longer is. Stated
    /// as a test rather than left as an absence so that reintroducing a curve
    /// has to break something that names the instruction.
    #[test]
    fn the_ring_does_not_change_colour() {
        assert_eq!(ring_color(), ring_color());
        // And it is the colour asked for, undimmed -- the pulse used to leave
        // it at 45% of full between breaths.
        assert_eq!(
            ring_color(),
            super::super::materials::to_color(WORKING_COLOR)
        );
    }

    /// The whole point of the pair: pulled right back, the band the King sees
    /// is the heavy one.
    #[test]
    fn the_bold_ring_is_the_one_shown_when_the_map_is_furthest_out() {
        assert!(shows(RingTier::Bold, LodLevel::Districts));
        assert!(!shows(RingTier::Bold, LodLevel::Architecture));
        assert!(!shows(RingTier::Bold, LodLevel::FileDetail));

        assert!(!shows(RingTier::Fine, LodLevel::Districts));
        assert!(shows(RingTier::Fine, LodLevel::Architecture));
        assert!(shows(RingTier::Fine, LodLevel::FileDetail));
    }

    /// The property the pair actually has to hold, stated as a property rather
    /// than as six assertions: two bands stacked would read as a thick smear
    /// with a seam down it, and none at all would lose the fact entirely at
    /// whichever zoom the gap fell in.
    #[test]
    fn exactly_one_ring_shows_at_every_tier() {
        for lod in [
            LodLevel::Districts,
            LodLevel::Architecture,
            LodLevel::FileDetail,
        ] {
            let shown = [RingTier::Fine, RingTier::Bold]
                .into_iter()
                .filter(|tier| shows(*tier, lod))
                .count();
            assert_eq!(shown, 1, "{lod:?} shows {shown} rings, not one");
        }
    }

    /// The regression that got past three attempts at this. The ring must be
    /// **recognisably green**, and the failures were all in the bright
    /// direction: emissive scaled for the sun's lux clipped to white, a value
    /// near 1.0 was washed out by the tonemapper, and a lit material added white
    /// specular on top. Hence unlit.
    ///
    /// One sample rather than twenty-four across a breath, which is what this
    /// took while the colour was a function of time. The reasoning it guards is
    /// unchanged and is recorded on [`WORKING_COLOR`].
    #[test]
    fn the_ring_is_recognisably_green() {
        // `to_color` hands back sRGB, which is what the palettes were authored
        // in; the channel comparisons below are about light, so they are made
        // in linear.
        let c = ring_color().to_linear();

        assert!(
            c.green > c.red * 2.0 && c.green > c.blue * 1.5,
            "the ring lost its hue: {c:?}"
        );
        // Never brighter than the colour itself, which is what keeps the
        // display from clamping the channels together into white.
        assert!(c.green <= 1.0, "the ring can clip to white: {c:?}");
        assert!(c.alpha == 1.0, "the ring is drawn opaque");
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
