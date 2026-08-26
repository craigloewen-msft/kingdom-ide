//! Level of detail.
//!
//! The three tiers are the ones the old viewer used, at the same thresholds.
//! What changed is what they cost: culling and depth sorting now belong to the
//! renderer, so a tier only decides which decorations are worth drawing.

use bevy::prelude::*;

use super::bridge::LodLevel;
use super::camera::CameraRig;
use super::spawn::VisibleFrom;

/// The tier currently in force, so systems can react to a change rather than
/// re-deriving it.
#[derive(Resource, Default, Debug, Clone, Copy, PartialEq, Eq)]
pub struct ActiveLod(pub LodLevel);

pub fn track_lod(rig: Res<CameraRig>, mut active: ResMut<ActiveLod>) {
    let level = rig.lod();
    if active.0 != level {
        active.0 = level;
    }
}

/// Shows or hides decoration as the tier changes.
///
/// Only runs on a change, because walking every entity in a five thousand file
/// world each frame would undo the point of the tiers.
pub fn apply_lod(active: Res<ActiveLod>, mut targets: Query<(&VisibleFrom, &mut Visibility)>) {
    if !active.is_changed() {
        return;
    }
    for (threshold, mut visibility) in targets.iter_mut() {
        let wanted = if reaches(active.0, threshold.0) {
            Visibility::Inherited
        } else {
            Visibility::Hidden
        };
        if *visibility != wanted {
            *visibility = wanted;
        }
    }
}

/// Whether `current` is at or beyond `threshold`.
fn reaches(current: LodLevel, threshold: LodLevel) -> bool {
    rank(current) >= rank(threshold)
}

fn rank(level: LodLevel) -> u8 {
    match level {
        LodLevel::Districts => 0,
        LodLevel::Architecture => 1,
        LodLevel::FileDetail => 2,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tier_shows_everything_at_or_below_it() {
        assert!(reaches(LodLevel::Districts, LodLevel::Districts));
        assert!(!reaches(LodLevel::Districts, LodLevel::Architecture));
        assert!(reaches(LodLevel::Architecture, LodLevel::Districts));
        assert!(reaches(LodLevel::Architecture, LodLevel::Architecture));
        assert!(!reaches(LodLevel::Architecture, LodLevel::FileDetail));
        assert!(reaches(LodLevel::FileDetail, LodLevel::FileDetail));
    }

    #[test]
    fn buildings_stay_visible_at_every_tier() {
        // Holdings are tagged at the lowest tier deliberately: the settlement
        // should never vanish, only lose its trim.
        for tier in [
            LodLevel::Districts,
            LodLevel::Architecture,
            LodLevel::FileDetail,
        ] {
            assert!(reaches(tier, LodLevel::Districts));
        }
    }
}
