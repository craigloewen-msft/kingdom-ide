//! The rendering engine.
//!
//! Repo City used to draw itself twice: once into a baked isometric display
//! list at generation time, and once again by hand onto a 2D canvas. Both are
//! gone. The manifest now describes the settlement in world space and this
//! module hands it to Bevy, which owns projection, depth, lighting, shadows,
//! culling, and hit testing.
//!
//! The interface around the map is still Leptos. The two halves talk through
//! [`bridge::Bridge`].

pub mod activity;
pub mod bridge;
pub mod camera;
pub mod input;
pub mod labels;
pub mod materials;
pub mod meshes;
pub mod spawn;
pub mod text;
pub mod wards;

mod lod;

use bevy::app::PluginGroup;
use bevy::asset::AssetPlugin;
use bevy::camera::Exposure;
use bevy::picking::mesh_picking::MeshPickingPlugin;
use bevy::prelude::*;
use bevy::render::RenderPlugin;
use bevy::render::settings::{Backends, RenderCreation, WgpuSettings};
use bevy::window::{PresentMode, WindowPlugin, WindowResolution};

use activity::Activity;
use bridge::{Bridge, ViewerCommand};
use camera::{CameraRig, MapCamera};
use lod::ActiveLod;
use materials::MaterialCache;
use spawn::{LoadedMap, MeshCache, SceneRoot};

/// The CSS selector of the canvas the engine draws into.
///
/// The canvas is created by Leptos and handed over, rather than injected by
/// the engine, so the surrounding interface keeps control of the layout.
pub const CANVAS_SELECTOR: &str = "#repo-city-canvas";

/// Boots the engine into the page.
///
/// On the web `App::run` never returns — it hands control to the browser's
/// animation loop — so this must be the last thing the caller does.
pub fn run(bridge: Bridge) {
    let mut app = App::new();

    app.add_plugins(
        DefaultPlugins
            .set(WindowPlugin {
                primary_window: Some(Window {
                    canvas: Some(CANVAS_SELECTOR.to_owned()),
                    fit_canvas_to_parent: true,
                    // The page around the canvas still needs its own scrolling
                    // and context menus.
                    prevent_default_event_handling: false,
                    present_mode: PresentMode::AutoVsync,
                    resolution: WindowResolution::default(),
                    ..default()
                }),
                ..default()
            })
            .set(RenderPlugin {
                render_creation: RenderCreation::Automatic(
                    WgpuSettings {
                        // Prefer WebGPU, fall back to WebGL2 where it is missing.
                        backends: Some(Backends::BROWSER_WEBGPU | Backends::GL),
                        ..default()
                    }
                    .into(),
                ),
                ..default()
            })
            .set(AssetPlugin {
                file_path: "assets".to_owned(),
                ..default()
            }),
    )
    .add_plugins(MeshPickingPlugin)
    .add_plugins(RepoCityPlugin { bridge })
    .run();
}

/// The plugin that installs the whole map renderer into a Bevy app.
pub struct RepoCityPlugin {
    /// The channel the interface talks to the engine through.
    pub bridge: Bridge,
}

impl Plugin for RepoCityPlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(self.bridge.clone())
            .init_resource::<CameraRig>()
            .init_resource::<MeshCache>()
            .init_resource::<MaterialCache>()
            .init_resource::<LoadedMap>()
            .init_resource::<ActiveLod>()
            .init_resource::<Activity>()
            .init_resource::<wards::ActiveWard>()
            .init_resource::<input::PointerState>()
            .init_resource::<labels::LabelPool>()
            .add_systems(Startup, (setup, labels::spawn_label_pool))
            .add_systems(
                Update,
                (
                    apply_commands,
                    input::track_pointer,
                    input::handle_scroll,
                    lod::track_lod,
                    lod::apply_lod,
                    // After `apply_lod`, which walks every entity with a
                    // `VisibleFrom` and would otherwise be free to hide a ring
                    // in the same frame this has just shown it. A ring carries
                    // no `VisibleFrom` precisely so that cannot happen, and the
                    // ordering is the second half of that guarantee.
                    activity::apply_activity,
                    activity::pulse_rings,
                    camera::sync_camera,
                    wards::apply_label_legibility,
                    wards::track_active_ward,
                    wards::apply_ward_highlight,
                    labels::update_labels,
                )
                    .chain(),
            );
    }
}

/// Spawns the camera before any world arrives, so the first frame is the sky
/// rather than a black screen.
fn setup(mut commands: Commands) {
    camera::spawn_camera(
        &mut commands,
        Color::srgb(0.55, 0.68, 0.78),
        Color::srgb(0.72, 0.78, 0.90),
        320.0,
    );
}

/// Drains and applies everything the interface has asked for.
#[allow(clippy::too_many_arguments)]
fn apply_commands(
    mut commands: Commands,
    bridge: Res<Bridge>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut mesh_cache: ResMut<MeshCache>,
    mut material_cache: ResMut<MaterialCache>,
    mut loaded: ResMut<LoadedMap>,
    mut rig: ResMut<CameraRig>,
    mut working: ResMut<Activity>,
    existing: Query<Entity, With<SceneRoot>>,
    windows: Query<&Window>,
    mut cameras: Query<(&mut Camera, &mut Exposure, &mut AmbientLight), With<MapCamera>>,
) {
    let queued = bridge.drain_commands();
    if queued.is_empty() {
        return;
    }
    let viewport = windows
        .single()
        .map(|window| Vec2::new(window.width(), window.height()))
        .unwrap_or(Vec2::splat(1.0));

    for command in queued {
        match command {
            ViewerCommand::Load(manifest) => {
                spawn::clear_world(
                    &mut commands,
                    &existing,
                    &mut mesh_cache,
                    &mut material_cache,
                );
                spawn::spawn_world(
                    &mut commands,
                    &mut meshes,
                    &mut materials,
                    &mut mesh_cache,
                    &mut material_cache,
                    &manifest.world,
                );

                if let Ok((mut camera, mut exposure, mut ambient)) = cameras.single_mut() {
                    camera.clear_color =
                        ClearColorConfig::Custom(materials::to_color(manifest.world.sky));
                    // The manifest carries its own sun, so the camera is
                    // exposed for that light rather than for a default one.
                    *exposure = camera::exposure_for(manifest.world.sun.illuminance);
                    ambient.color = materials::to_color(manifest.world.sun.ambient);
                    ambient.brightness = manifest.world.sun.ambient_brightness;
                }

                fit(&mut rig, &manifest.world.bounds, viewport);
                // Zoom limits and detail tiers are measured against a house,
                // so the reference house is taken once per world. The fitted
                // scale is kept only as the floor on how far back the camera
                // may pull, so that a large world can still be framed whole.
                rig.holding = typical_holding(&manifest);
                rig.fit_scale = rig.scale;
                bridge.update_status(|status| {
                    status.error = None;
                    status.hovered = None;
                    status.selected_ward = None;
                    status.hovered_ward = None;
                });
                loaded.0 = Some(manifest);
            }
            ViewerCommand::Fit => {
                if let Some(manifest) = loaded.0.as_ref() {
                    fit(&mut rig, &manifest.world.bounds, viewport);
                }
            }
            ViewerCommand::ZoomBy(factor) => rig.zoom_by(factor, Vec2::ZERO),
            ViewerCommand::ActualSize => {
                // One world unit per pixel, held on the current centre.
                let factor = rig.scale;
                rig.zoom_by(factor, Vec2::ZERO);
            }
            ViewerCommand::Focus { center, extent } => {
                let height = loaded
                    .0
                    .as_ref()
                    .map(|manifest| tallest(manifest))
                    .unwrap_or(40.0);
                rig.frame(
                    Vec2::from_array(center),
                    Vec2::from_array(extent).max(Vec2::splat(24.0)),
                    height,
                    viewport,
                );
            }
            ViewerCommand::LookAt { point } => rig.look_at(Vec2::from_array(point)),
            ViewerCommand::SelectWard(id) => {
                bridge.update_status(|status| status.selected_ward = id);
            }
            ViewerCommand::SetActivity(towns) => {
                // Assigned through `Res`'s change detection rather than
                // compared first: `apply_activity` runs only on a change, and
                // an equal assignment still marks the resource changed, which
                // costs one pass over a handful of rings. Guarding it here
                // would trade that for a comparison on every poll.
                *working = Activity(towns);
            }
        }
    }
}

fn fit(rig: &mut CameraRig, bounds: &crate::map::MapRect, viewport: Vec2) {
    let center = bounds.center();
    rig.span = bounds.width.max(bounds.depth);
    rig.frame(
        Vec2::from_array(center),
        Vec2::new(bounds.width, bounds.depth),
        60.0,
        viewport,
    );
}

/// The footprint span of a typical holding in this world.
///
/// Zoom limits and detail tiers are measured against a house, so they need to
/// know how big one is. The median is used rather than the mean because a
/// handful of enormous files would otherwise drag the reference off the size
/// nearly every building actually is.
fn typical_holding(manifest: &crate::map::MapManifest) -> f32 {
    let mut spans = manifest
        .world
        .buildings
        .iter()
        .map(|building| building.footprint.width.max(building.footprint.depth))
        .filter(|span| span.is_finite() && *span > 0.0)
        .collect::<Vec<_>>();
    if spans.is_empty() {
        return CameraRig::default().holding;
    }
    spans.sort_by(f32::total_cmp);
    spans[spans.len() / 2]
}

fn tallest(manifest: &crate::map::MapManifest) -> f32 {
    manifest
        .world
        .buildings
        .iter()
        .fold(24.0f32, |top, building| top.max(building.height))
}
