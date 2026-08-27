//! Knowing which folder you are standing in.
//!
//! The map has always carried the folder tree in its geometry — a ward is a
//! folder, and a ward inside a ward is a folder inside a folder — but nothing
//! said which was which. This module is the half of the answer that lives in
//! the renderer: it makes ward ground pickable so a folder can be pointed at,
//! lights up a folder's boundary together with every boundary it sits inside,
//! and holds each folder's painted name back until the camera is close enough
//! to read it.
//!
//! The other half is in the generator, which decides where a name fits and
//! what colour a ward is.

use crate::map::MapWard;
use bevy::prelude::*;

use super::bridge::Bridge;
use super::camera::CameraRig;
use super::spawn::LoadedMap;

/// A ward's ground. Picking one is how a folder gets pointed at.
#[derive(Component, Clone)]
pub struct WardGround {
    /// The manifest feature this ward was built from.
    pub id: String,
    /// Deeper wards win the pointer, because the innermost folder under a
    /// point is the one that answers "where am I".
    pub depth: u32,
}

/// Something that belongs to a ward and should light up with it.
///
/// In practice this is a ward's own boundary: a folder is shown by lighting its
/// border, not by washing its ground or its buildings with colour, which would
/// bury the folders nested inside it.
#[derive(Component, Clone)]
pub struct InWard(pub String);

/// A surface with a second, brighter material to swap to while its ward is
/// active.
///
/// Swapping a handle is far cheaper than editing a material, and because the
/// material cache quantizes by colour the highlight variants collapse into a
/// handful of extra materials rather than one per ward.
#[derive(Component, Clone)]
pub struct Tint {
    /// The material shown at rest.
    pub base: Handle<StandardMaterial>,
    /// The brighter material shown while the ward is active.
    pub highlight: Handle<StandardMaterial>,
}

/// A folder name painted on the ground.
#[derive(Component, Clone, Copy)]
pub struct WardLabel {
    /// Cap height in world units.
    pub size: f32,
    /// The direction the caps rise along, on the ground plane.
    pub cap_direction: Vec2,
    /// Below this many pixels of cap height the name is a smudge, so it is not
    /// drawn at all.
    pub min_pixel_height: f32,
}

impl WardLabel {
    /// Whether the name is worth drawing at the current zoom.
    pub fn legible(&self, rig: &CameraRig) -> bool {
        rig.ground_pixels(self.cap_direction, self.size) >= self.min_pixel_height
    }
}

/// The ward the map is currently lighting up, so the highlight only has to be
/// rebuilt when it actually changes.
#[derive(Resource, Default, Debug, Clone, PartialEq, Eq)]
pub struct ActiveWard(pub Option<String>);

/// Shows a folder's name once its caps are tall enough on screen to read.
///
/// This is what staggers the folder tree as you zoom: a top-level name is
/// legible from across the island, and a name four levels down only appears
/// once you are close enough for that folder to be what you are looking at.
pub fn apply_label_legibility(
    rig: Res<CameraRig>,
    mut labels: Query<(&WardLabel, &mut Visibility)>,
) {
    if !rig.is_changed() {
        return;
    }
    for (label, mut visibility) in labels.iter_mut() {
        let wanted = if label.legible(&rig) {
            Visibility::Inherited
        } else {
            Visibility::Hidden
        };
        if *visibility != wanted {
            *visibility = wanted;
        }
    }
}

/// Works out which ward the interface is pointing at.
///
/// A holding takes priority over bare ground, because someone hovering a
/// building is asking about that building's folder, and the building may well
/// be standing on a ward its own ground never gets hit through.
pub fn track_active_ward(
    bridge: Res<Bridge>,
    map: Res<LoadedMap>,
    mut seen: Local<u64>,
    mut active: ResMut<ActiveWard>,
) {
    // Resolving a holding to its ward is a scan of the manifest, so it only
    // happens when the interface has actually reported something new rather
    // than on every frame the map sits still.
    let revision = bridge.revision();
    if revision == *seen && !map.is_changed() {
        return;
    }
    *seen = revision;

    let status = bridge.status();
    let from_holding = status
        .hovered
        .as_deref()
        .and_then(|feature| ward_of_holding(&map, feature));
    let wanted = from_holding
        .or_else(|| status.hovered_ward.clone())
        .or_else(|| status.selected_ward.clone());

    if active.0 != wanted {
        active.0 = wanted;
    }
}

fn ward_of_holding(map: &LoadedMap, feature_id: &str) -> Option<String> {
    map.0.as_ref().and_then(|manifest| {
        manifest
            .world
            .buildings
            .iter()
            .find(|building| building.feature_id == feature_id)
            .and_then(|building| building.ward_id.clone())
    })
}

/// Lights the active folder's boundary, and the boundary of every folder it
/// sits inside.
///
/// Only the borders move. Tinting the ground or the holdings would say "this
/// folder" loudly but hide the structure underneath it — and the structure is
/// the whole question a nested folder raises. Lighting the lineage instead
/// draws a set of nested outlines that reads as a path from the repository root
/// down to whatever the pointer is on.
///
/// Only runs on a change: walking every surface in a five thousand file world
/// each frame would cost more than the highlight is worth.
pub fn apply_ward_highlight(
    active: Res<ActiveWard>,
    map: Res<LoadedMap>,
    mut surfaces: Query<(&InWard, &Tint, &mut MeshMaterial3d<StandardMaterial>)>,
) {
    if !active.is_changed() {
        return;
    }
    let lit = active.0.as_deref().map(|id| {
        let wards = map
            .0
            .as_ref()
            .map(|manifest| manifest.world.wards.as_slice())
            .unwrap_or_default();
        lineage(wards, id)
    });
    for (ward, tint, mut material) in surfaces.iter_mut() {
        let wanted = match &lit {
            // A parent lights up with its children, so hovering a deep folder
            // still shows which part of the map it belongs to.
            Some(ancestors) if ancestors.iter().any(|id| id == &ward.0) => &tint.highlight,
            _ => &tint.base,
        };
        if material.0 != *wanted {
            material.0 = wanted.clone();
        }
    }
}

/// A ward and every ward it sits inside, innermost first.
fn lineage(wards: &[MapWard], ward_id: &str) -> Vec<String> {
    let mut trail = vec![ward_id.to_owned()];
    let mut current = ward_id.to_owned();
    // The manifest is data from the network, so a broken parent chain must not
    // be able to hang the renderer.
    for _ in 0..wards.len() {
        let Some(parent) = wards
            .iter()
            .find(|ward| ward.id == current)
            .and_then(|ward| ward.parent.clone())
        else {
            break;
        };
        if trail.iter().any(|id| id == &parent) {
            break;
        }
        trail.push(parent.clone());
        current = parent;
    }
    trail
}

/// Publishes the ward under the pointer.
pub fn on_ward_hover(event: On<Pointer<Over>>, wards: Query<&WardGround>, bridge: Res<Bridge>) {
    if let Ok(ward) = wards.get(event.entity) {
        let id = ward.id.clone();
        bridge.update_status(|status| status.hovered_ward = Some(id));
    }
}

/// Clears the hovered ward when the pointer leaves it.
pub fn on_ward_unhover(event: On<Pointer<Out>>, wards: Query<&WardGround>, bridge: Res<Bridge>) {
    if let Ok(ward) = wards.get(event.entity) {
        bridge.update_status(|status| {
            if status.hovered_ward.as_deref() == Some(ward.id.as_str()) {
                status.hovered_ward = None;
            }
        });
    }
}

/// A brighter version of a colour, for the highlight material.
///
/// Lifting toward white rather than shifting the hue keeps a highlighted
/// boundary recognisably its own ward's, which matters when the whole point of
/// the colour is to say which family the folder belongs to.
pub fn lift(color: [u8; 4], amount: f32) -> [u8; 4] {
    let lift = |channel: u8| {
        let value = channel as f32;
        (value + (255.0 - value) * amount).clamp(0.0, 255.0) as u8
    };
    [lift(color[0]), lift(color[1]), lift(color[2]), color[3]]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rig(scale: f32) -> CameraRig {
        CameraRig {
            focus: Vec3::ZERO,
            scale,
            fit_scale: 1.0,
            span: 1_000.0,
            holding: 30.0,
        }
    }

    fn label(size: f32, min_pixel_height: f32) -> WardLabel {
        WardLabel {
            size,
            cap_direction: Vec2::new(0.0, -1.0),
            min_pixel_height,
        }
    }

    #[test]
    fn a_name_appears_only_once_its_caps_are_tall_enough() {
        let label = label(8.0, 7.0);
        // One world unit per pixel foreshortens an 8 unit cap to well under
        // the threshold; ten times closer clears it comfortably.
        assert!(!label.legible(&rig(4.0)));
        assert!(label.legible(&rig(0.4)));
    }

    #[test]
    fn a_larger_name_survives_a_wider_view() {
        let small = label(4.0, 7.0);
        let large = label(20.0, 7.0);
        let rig = rig(1.2);
        assert!(!small.legible(&rig));
        assert!(large.legible(&rig));
    }

    #[test]
    fn ground_spans_are_foreshortened_but_never_to_nothing() {
        let rig = rig(1.0);
        for direction in [
            Vec2::new(1.0, 0.0),
            Vec2::new(0.0, 1.0),
            Vec2::new(1.0, 1.0),
            Vec2::new(1.0, -1.0),
        ] {
            let pixels = rig.ground_pixels(direction, 10.0);
            assert!(
                pixels > 4.0 && pixels <= 10.001,
                "{direction:?} projected to {pixels}"
            );
        }
    }

    #[test]
    fn zooming_in_makes_a_span_larger_in_proportion() {
        let far = rig(2.0).ground_pixels(Vec2::Y, 10.0);
        let near = rig(0.5).ground_pixels(Vec2::Y, 10.0);
        assert!((near / far - 4.0).abs() < 1e-3, "{near} vs {far}");
    }

    #[test]
    fn lifting_a_colour_brightens_it_without_touching_alpha() {
        let lifted = lift([60, 90, 40, 200], 0.4);
        assert!(lifted[0] > 60 && lifted[1] > 90 && lifted[2] > 40);
        assert_eq!(lifted[3], 200);
    }

    #[test]
    fn lifting_white_cannot_overflow() {
        assert_eq!(lift([255, 255, 255, 255], 1.0), [255, 255, 255, 255]);
    }

    fn ward(id: &str, parent: Option<&str>, depth: u32) -> MapWard {
        MapWard {
            id: id.to_owned(),
            name: id.to_owned(),
            path: id.to_owned(),
            parent: parent.map(str::to_owned),
            files: 1,
            rect: crate::map::MapRect::default(),
            polygon: Vec::new(),
            depth,
            ground: [0, 0, 0, 255],
            edge: [0, 0, 0, 255],
        }
    }

    #[test]
    fn a_folder_lights_up_together_with_every_folder_it_sits_in() {
        let wards = [
            ward("viewer", None, 0),
            ward("src", Some("viewer"), 1),
            ward("engine", Some("src"), 2),
        ];
        assert_eq!(lineage(&wards, "engine"), ["engine", "src", "viewer"]);
        assert_eq!(lineage(&wards, "viewer"), ["viewer"]);
    }

    #[test]
    fn a_broken_parent_chain_cannot_hang_the_renderer() {
        // The manifest arrives over the network, so a ward claiming its own
        // descendant as its parent has to terminate rather than loop.
        let wards = [
            ward("a", Some("b"), 0),
            ward("b", Some("a"), 1),
            ward("orphan", Some("missing"), 0),
        ];
        assert_eq!(lineage(&wards, "a"), ["a", "b"]);
        assert_eq!(lineage(&wards, "orphan"), ["orphan", "missing"]);
        assert_eq!(lineage(&[], "alone"), ["alone"]);
    }
}
