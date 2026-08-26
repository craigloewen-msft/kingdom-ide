//! The camera: orthographic, and locked to the isometric angle.
//!
//! The old renderer baked one fixed isometric projection into the manifest and
//! panned a flat picture around. This is a real camera over real geometry — it
//! is simply held at the classic angle so the settlement keeps its silhouette.
//!
//! Zoom is `OrthographicProjection::scale`, measured in world units per pixel,
//! so a smaller scale means a closer view.

use std::f32::consts::FRAC_PI_4;

use bevy::camera::{Exposure, Projection, ScalingMode};
use bevy::core_pipeline::tonemapping::Tonemapping;
use bevy::prelude::*;

use super::bridge::{Bridge, LodLevel};

/// Yaw about the vertical axis. Together with [`ISOMETRIC_PITCH`] this points
/// the camera down the `(-1, -1, -1)` diagonal — true isometric.
const ISOMETRIC_YAW: f32 = FRAC_PI_4;
/// `-atan(1 / sqrt(2))`, about -35.264 degrees.
const ISOMETRIC_PITCH: f32 = -0.615_479_7;

/// How far back the camera stands from a world of the given span.
///
/// Orthographic size does not depend on this; it only has to be far enough
/// that nothing crosses the near plane. An island is no longer a fixed size —
/// it grows with the repository it stands for — so a fixed distance would put
/// half of a large realm behind the camera and clip it away.
pub fn distance_for(span: f32) -> f32 {
    span.max(0.0) * 1.8 + 500.0
}

/// Zoom limits, as how many pixels wide a typical holding may become.
///
/// These are deliberately absolute rather than multiples of the fitted view.
/// A holding is the same size in world units whatever it stands in — the
/// ground grows with the file count precisely so that stays true — so tying
/// the limits to it means a house is the same size on screen at the same zoom
/// in a twelve-file repository and in a realm of three thousand.
const MAX_HOLDING_PIXELS: f32 = 420.0;
const MIN_HOLDING_PIXELS: f32 = 8.0;

/// How far past the fitted view the camera may pull back.
///
/// This is the one place the world's own size still gets a say, and only as a
/// floor: a world too large to fit inside [`MIN_HOLDING_PIXELS`] must still be
/// framable, or it could never be seen whole. The margin is kept small so the
/// absolute limit governs wherever it possibly can.
const FIT_HEADROOM: f32 = 1.15;

/// How large a holding is drawn when the map is pointed at one file.
///
/// Comfortably past [`LodLevel`]'s 64 px `FileDetail` threshold rather than on
/// it, so the tier cannot flicker as the pane is resized, and far short of
/// [`MAX_HOLDING_PIXELS`] so neighbours stay in frame and the building has a
/// street to stand on. In the rail's ~290 px pane that is three and a half
/// holdings across: the file's own building, the ones either side of it, and
/// the per-file labels the tier turns on.
pub const INSPECT_HOLDING_PIXELS: f32 = 84.0;

/// The footprint span assumed for a holding before a world is loaded.
const DEFAULT_HOLDING: f32 = 30.0;

/// Padding in pixels left around the world when fitting.
const FIT_PADDING: f32 = 56.0;

/// Marks the one camera looking at the map.
#[derive(Component)]
pub struct MapCamera;

/// Where the camera is looking and how far in.
#[derive(Resource, Debug, Clone, Copy)]
pub struct CameraRig {
    /// The world point held at the centre of the viewport.
    pub focus: Vec3,
    /// World units per pixel.
    pub scale: f32,
    /// The scale at which the whole world fits. Only the zoom-out limit
    /// consults it, so that a world too big to fit can still be pulled back
    /// far enough to see.
    pub fit_scale: f32,
    /// The footprint span of a typical holding, in world units. Zoom limits
    /// and the detail tier are measured against this rather than against the
    /// world, which is what keeps them steady across repositories.
    pub holding: f32,
    /// How wide the world being viewed is, which sets how far back the camera
    /// stands and how deep its clip range has to be.
    pub span: f32,
}

impl Default for CameraRig {
    fn default() -> Self {
        Self {
            focus: Vec3::ZERO,
            scale: 1.0,
            fit_scale: 1.0,
            holding: DEFAULT_HOLDING,
            span: 1_000.0,
        }
    }
}

impl CameraRig {
    /// How far back the camera stands from the world it is looking at.
    pub fn distance(&self) -> f32 {
        distance_for(self.span)
    }

    /// Zoom as a fraction of actual size — one world unit per pixel — which is
    /// what the toolbar reports.
    ///
    /// This used to be measured against the fitted view, which made the same
    /// reading mean a different thing in every repository. Actual size is a
    /// fixed reference, so 100% shows a house at the same size everywhere.
    pub fn zoom(&self) -> f32 {
        if self.scale <= 0.0 {
            return 1.0;
        }
        1.0 / self.scale
    }

    /// How many pixels wide a typical holding currently comes out.
    pub fn holding_pixels(&self) -> f32 {
        if self.scale <= 0.0 {
            return 0.0;
        }
        self.ground_pixels(Vec2::X, self.holding.max(1.0))
    }

    /// The scale at which a typical holding would be `pixels` wide.
    fn scale_for_holding_pixels(&self, pixels: f32) -> f32 {
        let at_unit_scale = Self {
            scale: 1.0,
            ..*self
        }
        .holding_pixels();
        at_unit_scale / pixels.max(1.0)
    }

    /// The detail tier the current view calls for.
    pub fn lod(&self) -> LodLevel {
        LodLevel::for_holding_pixels(self.holding_pixels())
    }

    /// How many pixels tall a ground-plane span of `length` world units comes
    /// out, measured along the ground axis the given direction points down.
    ///
    /// The camera is fixed at the isometric angle, so a span lying on the
    /// ground is foreshortened by an amount that depends only on which way it
    /// runs — never on where it is. That makes this a pure function of the
    /// zoom, which is what lets a ground label decide whether it is legible
    /// without being projected.
    pub fn ground_pixels(&self, direction: Vec2, length: f32) -> f32 {
        if self.scale <= 0.0 {
            return 0.0;
        }
        let (right, up) = screen_axes();
        let span = Vec3::new(direction.x, 0.0, direction.y).normalize_or_zero() * length;
        Vec2::new(span.dot(right), span.dot(up)).length() / self.scale
    }

    fn clamp_scale(&self, scale: f32) -> f32 {
        // Closest approach is absolute: a holding never grows past
        // MAX_HOLDING_PIXELS, however small the world around it is.
        let closest = self.scale_for_holding_pixels(MAX_HOLDING_PIXELS);
        // Furthest back is absolute too, except that a world too large to fit
        // within that limit is allowed to pull back until it does — otherwise
        // a big realm could never be framed whole.
        let furthest = self
            .scale_for_holding_pixels(MIN_HOLDING_PIXELS)
            .max(self.fit_scale * FIT_HEADROOM);
        // A world small enough to fit closer than the near limit keeps its
        // fitted view reachable rather than being pushed away from it.
        let closest = closest.min(furthest);
        scale.clamp(closest, furthest)
    }

    /// Pans by a screen-space delta in pixels.
    pub fn pan(&mut self, delta: Vec2) {
        let (right, up) = screen_axes();
        self.focus += (-right * delta.x + up * delta.y) * self.scale;
    }

    /// Zooms so a typical holding comes out `pixels` wide, holding the centre
    /// of the viewport over the same world point.
    ///
    /// This is the currency detail is already measured in — see
    /// [`Self::holding_pixels`] and [`LodLevel::for_holding_pixels`] — so a
    /// caller can ask for a tier rather than for a scale, and get the same
    /// apparent house size in a twelve-file repository and in a realm of three
    /// thousand. It goes through [`Self::clamp_scale`] like every other zoom,
    /// so nothing can ask the camera past where the wheel could take it.
    pub fn zoom_to_holding_pixels(&mut self, pixels: f32) {
        self.scale = self.clamp_scale(self.scale_for_holding_pixels(pixels));
    }

    /// Zooms by `factor` while holding `anchor` — an offset in pixels from the
    /// centre of the viewport — over the same world point.
    pub fn zoom_by(&mut self, factor: f32, anchor: Vec2) {
        let next = self.clamp_scale(self.scale / factor.max(0.001));
        let (right, up) = screen_axes();
        self.focus += (right * anchor.x - up * anchor.y) * (self.scale - next);
        self.scale = next;
    }

    /// Frames a set of world points, choosing the scale that just contains
    /// them all.
    ///
    /// The box-shaped [`Self::frame_range`] cannot describe the disk: its
    /// underside is a *point* at the centre, and reserving a full world-width
    /// box down at the spire's tip leaves a third of the viewport empty. Points
    /// say what is actually there.
    pub fn frame_points(&mut self, points: &[Vec3], viewport: Vec2) {
        if points.is_empty() {
            return;
        }
        let (right, up) = screen_axes();
        let forward = isometric_rotation() * Vec3::NEG_Z;

        // The projection is orthographic, so a point's screen position is just
        // its coordinates in the camera's own basis and the depth is free.
        let mut min = Vec2::splat(f32::MAX);
        let mut max = Vec2::splat(f32::MIN);
        let mut depth = 0.0;
        for point in points {
            let screen = Vec2::new(point.dot(right), point.dot(up));
            min = min.min(screen);
            max = max.max(screen);
            depth += point.dot(forward);
        }
        depth /= points.len() as f32;

        let size = max - min;
        let usable = (viewport - Vec2::splat(FIT_PADDING)).max(Vec2::splat(1.0));
        self.scale = (size.x / usable.x).max(size.y / usable.y).max(f32::EPSILON);

        // The basis is orthonormal, so the world point that projects to the
        // middle of that box is simply its coordinates read back out.
        let middle = (min + max) * 0.5;
        self.focus = right * middle.x + up * middle.y + forward * depth;
    }

    /// Frames a world-space box, choosing the scale that just contains it.
    pub fn frame(&mut self, center: Vec2, extent: Vec2, top: f32, viewport: Vec2) {
        self.frame_range(center, extent, 0.0, top, viewport);
    }

    /// Frames a world-space box that may hang below the ground plane.
    ///
    /// The world used to start at `y = 0` and rise, so a frame could assume its
    /// bottom. The disk hangs a spire under the ground, and a fit that assumed
    /// nothing was down there would crop it off the bottom of the screen on
    /// load -- which is exactly the part of the world that shows it is an
    /// object floating in space rather than a picture of one.
    pub fn frame_range(
        &mut self,
        center: Vec2,
        extent: Vec2,
        bottom: f32,
        top: f32,
        viewport: Vec2,
    ) {
        let middle = (top + bottom) * 0.5;
        let half_height = (top - bottom).abs() * 0.5;
        let focus = Vec3::new(center.x, middle, center.y);
        let half = Vec3::new(extent.x * 0.5, half_height, extent.y * 0.5);
        let (right, up) = screen_axes();

        // The box is symmetric about its centre and the projection is affine,
        // so the projected extent is symmetric too: measuring the corners on
        // one side is enough.
        let mut half_width = 0.0f32;
        let mut half_screen_height = 0.0f32;
        for corner in [
            Vec3::new(half.x, half.y, half.z),
            Vec3::new(half.x, half.y, -half.z),
            Vec3::new(half.x, -half.y, half.z),
            Vec3::new(half.x, -half.y, -half.z),
        ] {
            half_width = half_width.max(corner.dot(right).abs());
            half_screen_height = half_screen_height.max(corner.dot(up).abs());
        }

        let usable = (viewport - Vec2::splat(FIT_PADDING)).max(Vec2::splat(1.0));
        let scale = (half_width * 2.0 / usable.x)
            .max(half_screen_height * 2.0 / usable.y)
            .max(f32::EPSILON);

        self.focus = focus;
        self.scale = scale;
    }

    /// Centres on a world point without changing the zoom.
    pub fn look_at(&mut self, point: Vec2) {
        self.focus = Vec3::new(point.x, self.focus.y, point.y);
    }

    /// The world-space ground rect the viewport currently covers.
    ///
    /// The projected viewport is a rotated rectangle on the ground, so this
    /// reports its axis-aligned bounds — which is exactly what the minimap
    /// indicator wants.
    pub fn ground_rect(&self, viewport: Vec2) -> [f32; 4] {
        let (right, up) = screen_axes();
        let half = viewport * 0.5 * self.scale;
        let mut min = Vec2::splat(f32::MAX);
        let mut max = Vec2::splat(f32::MIN);
        for (x, y) in [
            (-half.x, -half.y),
            (half.x, -half.y),
            (half.x, half.y),
            (-half.x, half.y),
        ] {
            let corner = self.focus + right * x + up * y;
            min = min.min(Vec2::new(corner.x, corner.z));
            max = max.max(Vec2::new(corner.x, corner.z));
        }
        [min.x, min.y, max.x - min.x, max.y - min.y]
    }
}

/// The fixed rotation the camera is held at.
pub fn isometric_rotation() -> Quat {
    Quat::from_euler(EulerRot::YXZ, ISOMETRIC_YAW, ISOMETRIC_PITCH, 0.0)
}

/// The camera's right and up vectors in world space.
fn screen_axes() -> (Vec3, Vec3) {
    let rotation = isometric_rotation();
    (rotation * Vec3::X, rotation * Vec3::Y)
}

/// The exposure the camera is set to, from the sun the manifest carries.
///
/// The manifest describes its light physically, in lux. A camera has to be
/// exposed for the light it is pointed at or the image blows out — which is
/// exactly what a hand-shaded renderer never had to think about, and exactly
/// what makes a real one look right.
///
/// This is the standard incident-light meter relation at ISO 100 with the usual
/// calibration constant of 250.
/// The sun a manifest is authored against. The palettes were tuned under this
/// light, so it is the exposure the scene is calibrated to.
const REFERENCE_ILLUMINANCE: f32 = 9_000.0;

/// Chooses an exposure for a given sun.
///
/// A manifest picks its own sun, so the camera cannot assume a fixed one: a
/// brighter light with a fixed exposure blows the roofs out, and a dimmer one
/// leaves the town in the dark. Exposing relative to the reference sun keeps
/// the calibrated look while letting a manifest change the light by a stop or
/// two and still be legible.
pub fn exposure_for(illuminance: f32) -> Exposure {
    let stops = (illuminance.max(1.0) / REFERENCE_ILLUMINANCE).log2();
    Exposure {
        ev100: (Exposure::EV100_BLENDER + stops).clamp(4.0, 20.0),
    }
}

/// Spawns the isometric camera and the ambient light it sees by.
pub fn spawn_camera(commands: &mut Commands, sky: Color, ambient: Color, brightness: f32) {
    let rotation = isometric_rotation();
    commands.spawn((
        Camera3d::default(),
        Camera {
            clear_color: ClearColorConfig::Custom(sky),
            ..default()
        },
        // The default tonemapper reads from a lookup table that only ships
        // with the `tonemapping_luts` feature; without it every surface comes
        // out magenta. This filmic curve needs no table, which keeps a few
        // hundred kilobytes out of the bundle.
        Tonemapping::AcesFitted,
        Exposure::default(),
        Projection::Orthographic(OrthographicProjection {
            scaling_mode: ScalingMode::WindowSize,
            scale: 1.0,
            near: 0.1,
            far: distance_for(1_000.0) * 2.5,
            ..OrthographicProjection::default_3d()
        }),
        AmbientLight {
            color: ambient,
            brightness,
            affects_lightmapped_meshes: true,
        },
        Transform::from_translation(rotation * Vec3::new(0.0, 0.0, distance_for(1_000.0)))
            .with_rotation(rotation),
        MapCamera,
    ));
}

/// Copies the rig onto the camera entity and publishes the result.
pub fn sync_camera(
    rig: Res<CameraRig>,
    bridge: Res<Bridge>,
    mut camera: Query<(&mut Transform, &mut Projection), With<MapCamera>>,
    mut stars: Query<&mut Transform, (With<super::stars::StarField>, Without<MapCamera>)>,
    windows: Query<&Window>,
) {
    let Ok((mut transform, mut projection)) = camera.single_mut() else {
        return;
    };
    let rotation = isometric_rotation();
    let distance = rig.distance();
    transform.translation = rig.focus + rotation * Vec3::new(0.0, 0.0, distance);
    transform.rotation = rotation;
    let mut far = distance;
    if let Projection::Orthographic(orthographic) = projection.as_mut() {
        orthographic.scale = rig.scale;
        // The clip range hugs the world rather than spanning a fixed depth, so
        // a large island neither falls out of the far plane nor spends its
        // depth buffer on empty space and starts z-fighting on the ground.
        let reach = rig.span * 1.2 + 200.0;
        orthographic.near = (distance - reach).max(0.1);
        orthographic.far = distance + reach;
        far = orthographic.far;
    }

    // The star field is authored in pixels and rides on the camera, so it is
    // scaled by however many world units a pixel is currently worth -- which is
    // what keeps a star the same speck at every zoom. It sits just inside the
    // far plane, which moves with the zoom, so the whole world draws in front
    // of it.
    if let Ok(mut field) = stars.single_mut() {
        field.scale = Vec3::splat(rig.scale.max(f32::EPSILON));
        field.translation = Vec3::new(0.0, 0.0, -far * 0.98);
    }

    let viewport = windows
        .single()
        .map(|window| Vec2::new(window.width(), window.height()))
        .unwrap_or(Vec2::splat(1.0));
    let rect = rig.ground_rect(viewport);
    let zoom = rig.zoom();
    let lod = rig.lod();
    bridge.update_status(|status| {
        status.zoom = zoom;
        status.lod = lod;
        status.camera_rect = rect;
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rig() -> CameraRig {
        CameraRig {
            focus: Vec3::new(500.0, 0.0, 500.0),
            scale: 2.0,
            fit_scale: 2.0,
            span: 1_000.0,
            holding: 30.0,
        }
    }

    #[test]
    fn the_camera_looks_down_the_isometric_diagonal() {
        let forward = isometric_rotation() * Vec3::NEG_Z;
        let expected = Vec3::new(-1.0, -1.0, -1.0).normalize();
        assert!(
            forward.distance(expected) < 1e-4,
            "forward was {forward:?}, wanted {expected:?}"
        );
    }

    #[test]
    fn the_camera_basis_is_orthonormal() {
        let (right, up) = screen_axes();
        let forward = isometric_rotation() * Vec3::NEG_Z;
        for axis in [right, up, forward] {
            assert!((axis.length() - 1.0).abs() < 1e-5);
        }
        assert!(right.dot(up).abs() < 1e-5);
        assert!(right.dot(forward).abs() < 1e-5);
        assert!(up.dot(forward).abs() < 1e-5);
    }

    #[test]
    fn panning_moves_the_world_with_the_pointer() {
        let mut rig = rig();
        let (right, _) = screen_axes();
        rig.pan(Vec2::new(10.0, 0.0));
        // Dragging right pulls the camera left so the world follows the cursor.
        let moved = rig.focus - Vec3::new(500.0, 0.0, 500.0);
        assert!(moved.dot(right) < 0.0);
    }

    #[test]
    fn zooming_holds_the_anchored_point_still() {
        let mut rig = rig();
        let anchor = Vec2::new(120.0, -80.0);
        let (right, up) = screen_axes();
        let before = rig.focus + (right * anchor.x - up * anchor.y) * rig.scale;

        rig.zoom_by(2.0, anchor);

        let after = rig.focus + (right * anchor.x - up * anchor.y) * rig.scale;
        assert!(
            before.distance(after) < 1e-3,
            "the anchored point drifted from {before:?} to {after:?}"
        );
    }

    #[test]
    fn zoom_is_clamped_to_the_supported_range() {
        let mut rig = rig();
        for _ in 0..40 {
            rig.zoom_by(2.0, Vec2::ZERO);
        }
        assert!(
            rig.holding_pixels() <= MAX_HOLDING_PIXELS + 1e-2,
            "{}",
            rig.holding_pixels()
        );

        for _ in 0..80 {
            rig.zoom_by(0.5, Vec2::ZERO);
        }
        assert!(
            rig.holding_pixels() <= MIN_HOLDING_PIXELS + 1e-2,
            "{}",
            rig.holding_pixels()
        );
    }

    /// The point of the whole exercise: the same zoom action has to leave a
    /// house the same size on screen whatever it is standing in.
    #[test]
    fn a_house_is_the_same_size_at_the_limits_however_big_the_world_is() {
        let mut sizes = Vec::new();
        for span in [200.0f32, 2_000.0, 20_000.0, 200_000.0] {
            let mut rig = rig();
            rig.span = span;
            // A fitted view of a world of this span, as a load would leave it.
            rig.frame(
                Vec2::ZERO,
                Vec2::splat(span),
                60.0,
                Vec2::new(1_400.0, 900.0),
            );
            rig.fit_scale = rig.scale;

            for _ in 0..60 {
                rig.zoom_by(2.0, Vec2::ZERO);
            }
            let closest = rig.holding_pixels();
            for _ in 0..120 {
                rig.zoom_by(0.5, Vec2::ZERO);
            }
            sizes.push((span, closest, rig.holding_pixels()));
        }

        for (span, closest, _) in &sizes {
            assert!(
                (closest - MAX_HOLDING_PIXELS).abs() < 1.0,
                "a span of {span} zoomed in to {closest} px"
            );
        }
        // Pulling back is absolute too, until the world is so large that
        // fitting it demands more room than the absolute limit allows.
        let (_, _, small) = sizes[0];
        let (_, _, medium) = sizes[1];
        assert!((small - MIN_HOLDING_PIXELS).abs() < 1.0, "{small}");
        assert!((medium - MIN_HOLDING_PIXELS).abs() < 1.0, "{medium}");
        for (span, _, furthest) in &sizes {
            assert!(
                *furthest <= MIN_HOLDING_PIXELS + 1e-2,
                "a span of {span} only pulled back to {furthest} px"
            );
        }
    }

    /// A world too big to fit inside the absolute pull-back limit must still
    /// be framable, or it could never be seen whole.
    #[test]
    fn a_huge_world_can_still_be_pulled_back_far_enough_to_fit() {
        let mut rig = rig();
        let viewport = Vec2::new(1_400.0, 900.0);
        let extent = Vec2::splat(200_000.0);
        rig.frame(Vec2::ZERO, extent, 60.0, viewport);
        rig.fit_scale = rig.scale;
        let fitted = rig.scale;

        // Zoom right in, then right back out, and the fitted view is reachable.
        for _ in 0..60 {
            rig.zoom_by(2.0, Vec2::ZERO);
        }
        for _ in 0..120 {
            rig.zoom_by(0.5, Vec2::ZERO);
        }
        assert!(
            rig.scale >= fitted,
            "pulled back to {} but fitting needs {fitted}",
            rig.scale
        );
    }

    /// The spire hangs below the ground, and it is the part that shows the
    /// kingdom is an object floating in space. A fit that assumed the world
    /// started at `y = 0` would crop it off the bottom of the screen on load.
    #[test]
    fn a_fitted_view_holds_both_the_rooftops_and_the_spire() {
        let viewport = Vec2::new(1_400.0, 900.0);
        let radius = 600.0f32;
        let extent = Vec2::splat(radius * 2.0);
        let (top, bottom) = (60.0f32, -radius * 1.05);

        let mut rig = rig();
        rig.frame_range(Vec2::ZERO, extent, bottom, top, viewport);

        // Project the extremes of the world the same way the camera does, and
        // check both land inside the viewport it just chose.
        let (right, up) = screen_axes();
        let half = (viewport - Vec2::splat(FIT_PADDING)) * 0.5 * rig.scale;
        for corner in [
            Vec3::new(-radius, top, -radius),
            Vec3::new(radius, top, radius),
            Vec3::new(0.0, bottom, 0.0),
            Vec3::new(radius, bottom, radius),
        ] {
            let offset = corner - rig.focus;
            assert!(
                offset.dot(right).abs() <= half.x + 1e-2
                    && offset.dot(up).abs() <= half.y + 1e-2,
                "{corner:?} fell outside the fitted view"
            );
        }
    }

    /// The old signature has to keep meaning exactly what it meant, since every
    /// other caller still frames a world standing on the ground.
    #[test]
    fn framing_to_the_ground_is_framing_from_zero() {
        let viewport = Vec2::new(1_400.0, 900.0);
        let mut framed = rig();
        let mut ranged = rig();

        framed.frame(Vec2::new(120.0, -40.0), Vec2::splat(900.0), 60.0, viewport);
        ranged.frame_range(
            Vec2::new(120.0, -40.0),
            Vec2::splat(900.0),
            0.0,
            60.0,
            viewport,
        );

        assert_eq!(framed.scale, ranged.scale);
        assert_eq!(framed.focus, ranged.focus);
    }

    /// The disk's underside is a *point*, not a slab. Framing it as a box
    /// reserves a full world-width band down at the spire's tip, most of it
    /// empty, and pushes the kingdom into the top of the screen.
    #[test]
    fn framing_points_is_tighter_than_framing_the_box_around_them() {
        let viewport = Vec2::new(1_400.0, 900.0);
        let radius = 600.0f32;
        let depth = radius * 1.05;

        // A disk seen from above, plus the one point hanging under its centre.
        let mut points: Vec<Vec3> = (0..48)
            .map(|index| {
                let angle = index as f32 / 48.0 * std::f32::consts::TAU;
                Vec3::new(angle.cos() * radius, 60.0, angle.sin() * radius)
            })
            .collect();
        points.push(Vec3::new(0.0, -depth, 0.0));

        let mut pointwise = rig();
        pointwise.frame_points(&points, viewport);

        let mut boxed = rig();
        boxed.frame_range(Vec2::ZERO, Vec2::splat(radius * 2.0), -depth, 60.0, viewport);

        assert!(
            pointwise.scale < boxed.scale,
            "points fitted at {} but a box at {}",
            pointwise.scale,
            boxed.scale
        );

        // And everything still fits.
        let (right, up) = screen_axes();
        let half = (viewport - Vec2::splat(FIT_PADDING)) * 0.5 * pointwise.scale;
        for point in &points {
            let offset = *point - pointwise.focus;
            assert!(
                offset.dot(right).abs() <= half.x + 1e-2
                    && offset.dot(up).abs() <= half.y + 1e-2,
                "{point:?} fell outside the fitted view"
            );
        }
    }

    /// Nothing to frame must leave the camera where it was rather than send it
    /// to an undefined place.
    #[test]
    fn framing_nothing_changes_nothing() {
        let mut rig = rig();
        let before = (rig.focus, rig.scale);
        rig.frame_points(&[], Vec2::new(1_400.0, 900.0));
        assert_eq!((rig.focus, rig.scale), before);
    }

    #[test]
    fn fitting_covers_the_whole_world() {
        let mut rig = rig();
        let viewport = Vec2::new(1280.0, 800.0);
        let center = Vec2::new(500.0, 500.0);
        let extent = Vec2::new(960.0, 960.0);

        rig.frame(center, extent, 60.0, viewport);

        let covered = rig.ground_rect(viewport);
        assert!(
            covered[0] <= center.x - extent.x * 0.5,
            "left edge {} did not reach {}",
            covered[0],
            center.x - extent.x * 0.5
        );
        assert!(covered[1] <= center.y - extent.y * 0.5);
        assert!(covered[0] + covered[2] >= center.x + extent.x * 0.5);
        assert!(covered[1] + covered[3] >= center.y + extent.y * 0.5);
    }

    #[test]
    fn looking_at_a_point_keeps_the_zoom() {
        let mut rig = rig();
        rig.look_at(Vec2::new(120.0, 340.0));
        assert_eq!(rig.scale, 2.0);
        assert_eq!(rig.focus.x, 120.0);
        assert_eq!(rig.focus.z, 340.0);
    }

    /// Asking for a holding size must actually deliver it, and must deliver it
    /// whatever world the camera is standing over -- which is the whole point
    /// of measuring in apparent house size rather than in scale.
    #[test]
    fn zooming_to_a_holding_size_gives_that_size() {
        for span in [200.0f32, 2_000.0, 20_000.0] {
            let mut rig = rig();
            rig.span = span;
            rig.frame(
                Vec2::ZERO,
                Vec2::splat(span),
                60.0,
                Vec2::new(1_400.0, 900.0),
            );
            rig.fit_scale = rig.scale;

            rig.zoom_to_holding_pixels(INSPECT_HOLDING_PIXELS);
            let pixels = rig.holding_pixels();
            assert!(
                (pixels - INSPECT_HOLDING_PIXELS).abs() < 0.5,
                "span {span} gave {pixels} px"
            );
        }
    }

    /// The reason the constant exists: pointing the map at a file has to land
    /// in the tier that draws per-file labels.
    #[test]
    fn inspecting_a_file_reaches_the_file_detail_tier() {
        for span in [200.0f32, 2_000.0, 20_000.0] {
            let mut rig = rig();
            rig.span = span;
            rig.frame(
                Vec2::ZERO,
                Vec2::splat(span),
                60.0,
                Vec2::new(1_400.0, 900.0),
            );
            rig.fit_scale = rig.scale;

            rig.zoom_to_holding_pixels(INSPECT_HOLDING_PIXELS);
            assert_eq!(rig.lod(), LodLevel::FileDetail, "span {span}");
        }
    }

    /// A zoom asked for from outside is still a zoom, so the absolute limits
    /// govern it exactly as they govern the wheel.
    #[test]
    fn zooming_to_a_holding_size_still_obeys_the_limits() {
        let mut close = rig();
        close.zoom_to_holding_pixels(MAX_HOLDING_PIXELS * 10.0);
        assert!(
            close.holding_pixels() <= MAX_HOLDING_PIXELS + 0.5,
            "came out at {} px",
            close.holding_pixels()
        );

        let mut far = rig();
        far.fit_scale = far.scale;
        far.zoom_to_holding_pixels(MIN_HOLDING_PIXELS * 0.1);
        assert!(
            far.holding_pixels() >= MIN_HOLDING_PIXELS - 0.5,
            "came out at {} px",
            far.holding_pixels()
        );
    }

    #[test]
    fn the_camera_is_exposed_for_the_light_it_is_given() {
        // A brighter sun must call for a brighter exposure value, or the image
        // blows out — which is how saturated roofs turned magenta.
        let dim = exposure_for(200.0).ev100;
        let daylight = exposure_for(9_000.0).ev100;
        let glare = exposure_for(100_000.0).ev100;
        assert!(dim < daylight, "{dim} !< {daylight}");
        assert!(daylight < glare, "{daylight} !< {glare}");

        // Doubling the light is one stop.
        let one_stop = exposure_for(18_000.0).ev100 - daylight;
        assert!((one_stop - 1.0).abs() < 1e-4, "one stop was {one_stop}");

        // The reference sun must reproduce the exposure the palettes were
        // tuned under, or every existing manifest shifts brightness.
        assert!((daylight - Exposure::EV100_BLENDER).abs() < 1e-4);
    }

    #[test]
    fn exposure_survives_a_nonsense_light() {
        // A malformed manifest must not produce an infinite or negative
        // exposure and a black screen.
        for illuminance in [0.0, -5.0, f32::MAX] {
            let ev100 = exposure_for(illuminance).ev100;
            assert!(ev100.is_finite(), "{illuminance} gave {ev100}");
            assert!((4.0..=20.0).contains(&ev100), "{illuminance} gave {ev100}");
        }
    }

    #[test]
    fn the_reported_tier_follows_the_apparent_size_of_a_house() {
        let mut rig = rig();
        rig.holding = 30.0;
        rig.scale = 30.0;
        assert_eq!(rig.lod(), LodLevel::Districts);
        rig.scale = 0.6;
        assert_eq!(rig.lod(), LodLevel::Architecture);
        rig.scale = 0.1;
        assert_eq!(rig.lod(), LodLevel::FileDetail);
    }

    /// The tier must not depend on how big the world is, only on how big a
    /// house looks.
    #[test]
    fn the_tier_ignores_the_size_of_the_world() {
        for span in [200.0f32, 2_000.0, 20_000.0] {
            let mut rig = rig();
            rig.span = span;
            rig.frame(
                Vec2::ZERO,
                Vec2::splat(span),
                60.0,
                Vec2::new(1_400.0, 900.0),
            );
            rig.fit_scale = rig.scale;
            rig.scale = rig.scale_for_holding_pixels(40.0);
            assert_eq!(rig.lod(), LodLevel::Architecture, "span {span}");
            rig.scale = rig.scale_for_holding_pixels(120.0);
            assert_eq!(rig.lod(), LodLevel::FileDetail, "span {span}");
        }
    }
}
