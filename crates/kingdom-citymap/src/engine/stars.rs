//! The stars the kingdom hangs among.
//!
//! A decoration, and the only part of the scene that is not in the world. It is
//! drawn in *screen* space -- a field of small quads parented to the camera --
//! for a reason the rest of the map does not have to think about: the
//! projection is orthographic, so a star placed out in the world would have no
//! parallax whatsoever. It would slide with the kingdom when panned, and worse,
//! it would *zoom* with it. Stars that grow as you lean in are not stars.
//!
//! Parenting to the camera answers both at once. The field never moves relative
//! to the viewer, and holding it at a constant pixel size is one multiplication
//! in [`crate::engine::camera::sync_camera`], which already knows how many world
//! units a pixel is worth.

use bevy::prelude::*;

use super::materials::{MaterialCache, Surface};
use super::meshes::MeshBuilder;

/// How many stars are scattered.
///
/// Enough to read as a sky and few enough to stay one small mesh. They are all
/// drawn whatever the detail tier is: a star costs two triangles, and a void
/// that empties as the camera pulls back would be the opposite of what the
/// zoom is doing.
const COUNT: usize = 900;

/// How far the field reaches, in pixels from the centre of the viewport.
///
/// The field has to cover the window at any size, since the viewport is not
/// known when it is built -- but only just: every pixel of reach past the edge
/// of the screen is stars scattered where nobody can see them, thinning the
/// sky that is actually on screen. This covers a 3000x3000 window.
const REACH: f32 = 1_500.0;

/// How large a star is, in pixels.
const SMALLEST: f32 = 1.0;
const LARGEST: f32 = 2.6;

/// The colour of a star, before its own brightness is applied.
const STAR: [u8; 4] = [226, 232, 246, 255];

/// The seed the field is scattered from.
///
/// Fixed, so the sky is the same sky every time the map is opened. There is
/// nothing for it to vary with -- unlike woodland, which is seeded from the
/// repository it grows on.
///
/// The grouping spells the word rather than dividing the number evenly, which
/// is what clippy objects to. Regrouping it to `0x005e_ed0f_57a5` would say the
/// same thing about a constant nobody reads for its magnitude, so the lint is
/// waived here rather than obeyed.
#[allow(clippy::unusual_byte_groupings)]
const SEED: u64 = 0x5eed_0f_57a5;

/// Marks the star field, which rides on the camera.
#[derive(Component)]
pub struct StarField;

/// One star: where it sits on screen, and how big it is.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Star {
    /// Offset from the centre of the viewport, in pixels.
    pub position: Vec2,
    /// Half-width in pixels.
    pub size: f32,
}

/// Scatters the field.
///
/// Pure, and separate from the spawning, so the scatter can be tested without a
/// renderer -- the same division every other piece of geometry in this crate
/// keeps.
pub fn scatter(count: usize, reach: f32, seed: u64) -> Vec<Star> {
    let mut state = seed.max(1);
    let mut next = move || {
        // Xorshift64, as `build::scenery` uses: cheap, and identical on every
        // machine, which is what makes the sky reproducible.
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        (state >> 11) as f32 / (1u64 << 53) as f32
    };
    (0..count)
        .map(|_| {
            let position = Vec2::new(next() * 2.0 - 1.0, next() * 2.0 - 1.0) * reach;
            // Squared, so most stars are faint specks and a few stand out.
            let weight = next() * next();
            Star {
                position,
                size: SMALLEST + weight * (LARGEST - SMALLEST),
            }
        })
        .collect()
}

/// Builds the whole field as one mesh, in the camera's local space.
///
/// Flat at local `z = 0`, because the entity carries how far back the field
/// sits -- and it has to, since the far plane moves with the zoom.
fn field_mesh(stars: &[Star]) -> Mesh {
    let mut builder = MeshBuilder::new();
    for star in stars {
        let (x, y, half) = (star.position.x, star.position.y, star.size);
        // Wound counter-clockwise as seen from the camera, which is looking
        // down local -z, so the quad faces the viewer.
        builder.quad(
            Vec3::new(x - half, y - half, 0.0),
            Vec3::new(x + half, y - half, 0.0),
            Vec3::new(x + half, y + half, 0.0),
            Vec3::new(x - half, y + half, 0.0),
        );
    }
    builder.build()
}

/// Spawns the field onto the camera.
///
/// Deferred through `Commands` like everything else in `setup`, which is why it
/// looks the camera up by marker rather than being handed its entity.
pub fn spawn_stars(commands: &mut Commands) {
    commands.queue(|world: &mut World| {
        let Some(camera) = world
            .query_filtered::<Entity, With<super::camera::MapCamera>>()
            .iter(world)
            .next()
        else {
            return;
        };
        let stars = scatter(COUNT, REACH, SEED);
        let mesh = world.resource_mut::<Assets<Mesh>>().add(field_mesh(&stars));
        let material = world.resource_scope(|world, mut cache: Mut<MaterialCache>| {
            let mut materials = world.resource_mut::<Assets<StandardMaterial>>();
            cache.get(&mut materials, STAR, Surface::Unlit)
        });
        world.spawn((
            ChildOf(camera),
            StarField,
            Mesh3d(mesh),
            MeshMaterial3d(material),
            Transform::default(),
            // A star is scenery at infinity; it must never take a click meant
            // for the kingdom, or for the empty space a click clears the
            // selection in.
            Pickable::IGNORE,
        ));
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The sky has to be the same sky every time the map is opened.
    #[test]
    fn the_field_is_the_same_every_time() {
        assert_eq!(scatter(64, REACH, SEED), scatter(64, REACH, SEED));
    }

    #[test]
    fn every_star_lands_inside_the_field() {
        for star in scatter(COUNT, REACH, SEED) {
            assert!(
                star.position.x.abs() <= REACH && star.position.y.abs() <= REACH,
                "a star escaped to {:?}",
                star.position
            );
            assert!((SMALLEST..=LARGEST).contains(&star.size), "{}", star.size);
        }
    }

    /// Every star must face the camera, or back-face culling takes the whole
    /// sky -- the trap `ground_polygons_face_upwards` exists for, one axis over.
    #[test]
    fn the_field_faces_the_camera() {
        let mesh = field_mesh(&scatter(8, REACH, SEED));
        let normals = mesh
            .attribute(Mesh::ATTRIBUTE_NORMAL)
            .and_then(|values| values.as_float3())
            .expect("normals");
        assert!(!normals.is_empty());
        for normal in normals {
            assert!(normal[2] > 0.99, "a star faced {normal:?}");
        }
    }

    /// A field of nothing must still be a mesh rather than a panic.
    #[test]
    fn an_empty_field_builds() {
        assert_eq!(field_mesh(&[]).count_vertices(), 0);
    }
}
