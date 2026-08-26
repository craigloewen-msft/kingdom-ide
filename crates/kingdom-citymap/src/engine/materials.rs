//! A cache of surface materials, keyed by colour.
//!
//! Shading used to be faked by lightening and darkening each polygon by hand.
//! It is now the lighting model's job, so all the scene needs to supply is the
//! base colour of a surface — and since a whole town is painted from a handful
//! of palettes, those materials are shared rather than duplicated per building.

use bevy::platform::collections::HashMap;
use bevy::prelude::*;
use crate::map::MapColor;

/// Colours are bucketed before lookup so two shades a single step apart share
/// one material instead of allocating two.
type MaterialKey = (u8, u8, u8, u8, u8);

/// One material per bucketed colour and surface, shared by every mesh using
/// it, so a five thousand file world draws from a few dozen materials.
#[derive(Resource, Default)]
pub struct MaterialCache {
    materials: HashMap<MaterialKey, Handle<StandardMaterial>>,
}

/// How rough a surface is, which is the only lighting knob the scene varies.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Surface {
    /// Walls, roofs, and ground: matte, no highlight to speak of.
    Matte,
    /// Trim and windows: enough sheen to catch the sun and read as glass.
    Polished,
    /// Water: smooth and a little reflective.
    Water,
}

impl Surface {
    fn tag(self) -> u8 {
        match self {
            Self::Matte => 0,
            Self::Polished => 1,
            Self::Water => 2,
        }
    }

    fn perceptual_roughness(self) -> f32 {
        match self {
            Self::Matte => 0.94,
            Self::Polished => 0.42,
            Self::Water => 0.16,
        }
    }
}

impl MaterialCache {
    /// Returns the material for a colour, creating it on first use.
    pub fn get(
        &mut self,
        materials: &mut Assets<StandardMaterial>,
        color: MapColor,
        surface: Surface,
    ) -> Handle<StandardMaterial> {
        let key = (
            quantise(color[0]),
            quantise(color[1]),
            quantise(color[2]),
            quantise(color[3]),
            surface.tag(),
        );
        self.materials
            .entry(key)
            .or_insert_with(|| {
                let base = to_color(color);
                let translucent = color[3] < 250;
                materials.add(StandardMaterial {
                    base_color: base,
                    perceptual_roughness: surface.perceptual_roughness(),
                    metallic: 0.0,
                    reflectance: match surface {
                        Surface::Matte => 0.06,
                        Surface::Polished => 0.35,
                        Surface::Water => 0.55,
                    },
                    alpha_mode: if translucent {
                        AlphaMode::Blend
                    } else {
                        AlphaMode::Opaque
                    },
                    // Ground and water are flat planes viewed from one side
                    // only, but ward polygons stack on the terrain and would
                    // otherwise z-fight; the spawner lifts them instead.
                    double_sided: false,
                    ..default()
                })
            })
            .clone()
    }

    /// How many distinct materials have been handed out.
    pub fn len(&self) -> usize {
        self.materials.len()
    }

    /// Whether nothing has been cached yet.
    pub fn is_empty(&self) -> bool {
        self.materials.is_empty()
    }

    /// Drops every cached material, for when a new world is loaded.
    pub fn clear(&mut self) {
        self.materials.clear();
    }
}

/// Rounds a channel to the nearest step of four.
fn quantise(channel: u8) -> u8 {
    (channel / 4).saturating_mul(4)
}

/// Converts a manifest colour into a rendering colour.
///
/// Manifest colours are sRGB, which is what the palettes were authored in.
pub fn to_color(color: MapColor) -> Color {
    Color::srgba_u8(color[0], color[1], color[2], color[3])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cache() -> (MaterialCache, Assets<StandardMaterial>) {
        (MaterialCache::default(), Assets::default())
    }

    #[test]
    fn near_identical_colors_share_one_material() {
        let (mut cache, mut assets) = cache();
        let first = cache.get(&mut assets, [120, 90, 60, 255], Surface::Matte);
        let second = cache.get(&mut assets, [122, 91, 61, 255], Surface::Matte);
        assert_eq!(first, second);
        assert_eq!(cache.len(), 1);
    }

    #[test]
    fn distinct_colors_get_distinct_materials() {
        let (mut cache, mut assets) = cache();
        cache.get(&mut assets, [120, 90, 60, 255], Surface::Matte);
        cache.get(&mut assets, [12, 200, 240, 255], Surface::Matte);
        assert_eq!(cache.len(), 2);
    }

    #[test]
    fn the_same_color_on_a_different_surface_is_a_different_material() {
        let (mut cache, mut assets) = cache();
        let matte = cache.get(&mut assets, [120, 90, 60, 255], Surface::Matte);
        let polished = cache.get(&mut assets, [120, 90, 60, 255], Surface::Polished);
        assert_ne!(matte, polished);
    }

    #[test]
    fn translucent_colors_blend() {
        let (mut cache, mut assets) = cache();
        let handle = cache.get(&mut assets, [10, 40, 80, 140], Surface::Water);
        let material = assets.get(&handle).expect("material");
        assert!(matches!(material.alpha_mode, AlphaMode::Blend));
    }
}
