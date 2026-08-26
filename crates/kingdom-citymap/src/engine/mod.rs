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
pub mod raise;
pub mod spawn;
pub mod stars;
pub mod text;
pub mod wards;
pub mod works;

mod lod;

use bevy::app::PluginGroup;
use bevy::asset::AssetPlugin;
use bevy::camera::Exposure;
use bevy::picking::mesh_picking::MeshPickingPlugin;
use bevy::prelude::*;
use bevy::render::RenderPlugin;
use bevy::render::settings::{Backends, RenderCreation, WgpuSettings};
use bevy::window::{PresentMode, WindowPlugin, WindowResolution};
use bevy::winit::{UpdateMode, WinitSettings};

use std::time::Duration;

use activity::Activity;
use bridge::{Bridge, RaiseStage, Raising, ViewerCommand};
use camera::{CameraGlide, CameraRig, MapCamera};
use lod::ActiveLod;
use materials::MaterialCache;
use raise::Raise;
use spawn::{LoadedMap, MeshCache, SceneRoot};

use crate::map::MapPresence;

/// The CSS selector of the canvas the engine draws into.
///
/// The canvas is created by Leptos and handed over, rather than injected by
/// the engine, so the surrounding interface keeps control of the layout.
pub const CANVAS_SELECTOR: &str = "#repo-city-canvas";

/// How long the engine may sleep when the King is not watching the map.
///
/// This is a wake guarantee rather than a frame rate. It bounds how long
/// [`ViewerCommand::Show`] can sit unread in the bridge, because only a running
/// update drains it -- so it is also the worst-case delay before the map comes
/// back when he returns to it. Short enough not to be seen, long enough that an
/// idle map is doing nothing worth measuring.
const IDLE_WAKE: Duration = Duration::from_millis(250);

/// How often the engine redraws while the map stands in the rail.
///
/// The middle gear between `Continuous` and [`IDLE_WAKE`]. A rail map is a
/// small pane the King glances at beside a conversation, not one he flies
/// through, and the only thing on it that moves on its own is the activity
/// ring -- whose breath takes `activity::PULSE_SECONDS`, several hundred times
/// this interval. So this is far more than enough to render it smoothly while
/// costing a fraction of a full frame rate for the length of a conversation.
///
/// A guess in the same spirit as `IDLE_WAKE`, and a one-constant change if it
/// ever reads as janky or still costs too much.
const RAIL_WAKE: Duration = Duration::from_millis(125);

/// Where the map is currently standing, as the interface last said.
///
/// Remembered rather than inferred, because the only thing the engine could
/// otherwise infer it from is `Camera::is_active` -- one bit, against three
/// answers. [`raise::raise_world`] needs the distinction: it forces a
/// continuous pace while a world goes up and has to put back the *right* one
/// when it finishes, and reading the camera would bring a rail map back
/// running flat out behind a conversation.
#[derive(Resource, Default, Debug, Clone, Copy)]
pub struct Standing(pub MapPresence);

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
            .init_resource::<CameraGlide>()
            .init_resource::<MeshCache>()
            .init_resource::<MaterialCache>()
            .init_resource::<LoadedMap>()
            .init_resource::<ActiveLod>()
            .init_resource::<Activity>()
            .init_resource::<Raise>()
            .init_resource::<Standing>()
            .init_resource::<wards::ActiveWard>()
            .init_resource::<works::Works>()
            .init_resource::<input::PointerState>()
            .init_resource::<input::Steering>()
            .init_resource::<labels::LabelPool>()
            .add_systems(Startup, (setup, labels::spawn_label_pool))
            .add_systems(
                Update,
                (
                    apply_commands,
                    // Straight after the commands that start it, so a world
                    // begins going up on the same frame it was handed over
                    // rather than one later.
                    raise::raise_world,
                    input::track_pointer,
                    input::handle_scroll,
                    // After the two systems that can take the camera, so a
                    // takeover is published on the frame it happens rather
                    // than one later -- and so the release cannot undo a pan
                    // that arrived in the same frame it fell due.
                    input::release_when_still,
                    // After all three of those, which is what lets the King
                    // interrupt a glide: a drag or a scroll in this frame has
                    // already taken the camera by the time the tween looks,
                    // so it stands down rather than pulling the map back out
                    // of his hands. Before `track_lod`, so a frame is drawn at
                    // the detail tier the camera actually reached in it.
                    camera::advance_glide,
                    // Straight after it, so a journey that started or ended in
                    // this frame is paid for -- or stopped being paid for --
                    // in the same one.
                    pace_for_glide,
                    lod::track_lod,
                    lod::apply_lod,
                    // After `apply_lod`, which walks every entity with a
                    // `VisibleFrom` and would otherwise be free to hide a ring
                    // in the same frame this has just shown it. A ring carries
                    // no `VisibleFrom` precisely so that cannot happen, and the
                    // ordering is the second half of that guarantee.
                    activity::apply_activity,
                    activity::pulse_rings,
                    // Beside the activity systems and after `apply_lod` for the
                    // same reason: the works carry no `VisibleFrom`, so nothing
                    // may hide them in the frame this has just raised them in.
                    works::apply_works,
                    works::pulse_works,
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

/// Spawns the camera before any world arrives, so the first frame is empty
/// space rather than a black screen or, worse, a flash of daylight sky the
/// manifest is about to replace.
fn setup(mut commands: Commands) {
    camera::spawn_camera(
        &mut commands,
        // `build::scene::SPACE`, which the manifest carries and overrides this
        // with the moment one arrives.
        Color::srgb(0.027, 0.043, 0.078),
        Color::srgb(0.72, 0.78, 0.90),
        320.0,
    );
    stars::spawn_stars(&mut commands);
}

/// Drains and applies everything the interface has asked for.
#[allow(clippy::too_many_arguments)]
fn apply_commands(
    mut commands: Commands,
    bridge: Res<Bridge>,
    mut mesh_cache: ResMut<MeshCache>,
    mut material_cache: ResMut<MaterialCache>,
    mut loaded: ResMut<LoadedMap>,
    mut raise: ResMut<Raise>,
    mut rig: ResMut<CameraRig>,
    mut glide: ResMut<CameraGlide>,
    mut working: ResMut<Activity>,
    mut works: ResMut<works::Works>,
    existing: Query<Entity, With<SceneRoot>>,
    windows: Query<&Window>,
    mut cameras: Query<(&mut Camera, &mut Exposure, &mut AmbientLight), With<MapCamera>>,
    mut winit: ResMut<WinitSettings>,
    mut standing: ResMut<Standing>,
    mut steering: ResMut<input::Steering>,
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
        // Every arm below that moves the camera ends whatever journey was under
        // way, and it is done once here rather than remembered in five places.
        // A glide left running would spend the rest of its quarter second
        // writing its own destination over a fit that has just been asked for,
        // so the last instruction has to win.
        if command.moves_the_camera() {
            glide.cancel();
        }
        match command {
            ViewerCommand::Load(manifest) => {
                // The world is no longer built here. `raise::raise_world`
                // builds it a slice at a time over the frames that follow, so
                // the browser can paint the loading bar between them -- see
                // that module for why one call cannot.
                //
                // What is still done inline is everything that costs nothing:
                // clearing the old world, lighting the scene, and spawning the
                // root the new one goes up under.
                raise.abandon();
                // A camera the King took hold of over a world that no longer
                // exists is meaningless, so a new world arrives to a map that
                // is following again.
                steering.release();
                // And works raised over a settlement that is being torn down
                // would hang in the air above whatever replaces it. The
                // interface re-sends them if the plan is still open.
                if !works.is_quiet() {
                    *works = works::Works::default();
                }
                spawn::clear_world(
                    &mut commands,
                    &existing,
                    &mut mesh_cache,
                    &mut material_cache,
                );
                // The manifest the map is *about to be* is not the map, so
                // anything reading `LoadedMap` mid-raise must find the absence
                // rather than a world whose entities do not exist yet.
                loaded.0 = None;

                if let Ok((mut camera, mut exposure, mut ambient)) = cameras.single_mut() {
                    camera.clear_color =
                        ClearColorConfig::Custom(materials::to_color(manifest.world.space));
                    // The manifest carries its own sun, so the camera is
                    // exposed for that light rather than for a default one.
                    *exposure = camera::exposure_for(manifest.world.sun.illuminance);
                    ambient.color = materials::to_color(manifest.world.sun.ambient);
                    ambient.brightness = manifest.world.sun.ambient_brightness;
                }

                // Hidden until it stands. The loading card is a translucent
                // gradient rather than an opaque screen, so a half-built
                // kingdom would show through it -- and scenery spawns visible
                // and is only culled the next time `apply_lod` runs, so the
                // King would watch trees appear and then vanish.
                let root = spawn::spawn_root(&mut commands, Visibility::Hidden);
                bridge.update_status(|status| {
                    // Not built, and no longer showing whatever stood before: a
                    // stale map under a loading card is worse than an empty
                    // one, because it looks finished.
                    status.built = false;
                    status.raising = Some(Raising {
                        stage: RaiseStage::Ground,
                        fraction: 0.0,
                    });
                    status.error = None;
                    status.hovered = None;
                    // A click on a world that no longer exists must not be
                    // replayed against the one that replaced it.
                    status.clicked = None;
                    status.selected_ward = None;
                    status.hovered_ward = None;
                });
                raise.begin(manifest, root);
            }
            ViewerCommand::Fit => {
                if let Some(manifest) = loaded.0.as_ref() {
                    fit(&mut rig, &manifest.world, viewport);
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
            ViewerCommand::Inspect { point, glide: fly } => {
                // Both ways of getting there agree on the destination, because
                // both ask the rig for it -- see `inspect_target`. The zoom is
                // the whole point of this command: centring alone slid a
                // town-wide frame sideways and left a house twenty pixels wide,
                // which is the coarsest detail tier. See the variant's own doc.
                let (focus, scale) =
                    rig.inspect_target(Vec2::from_array(point), camera::INSPECT_HOLDING_PIXELS);
                if fly {
                    glide.begin(&rig, focus, scale);
                } else {
                    rig.focus = focus;
                    rig.scale = scale;
                }
            }
            ViewerCommand::ReleaseCamera => steering.release(),
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
            ViewerCommand::SetWorks(raised) => {
                // Guarded, unlike `SetActivity` above, and the asymmetry is
                // deliberate. An equal assignment there costs one pass over a
                // handful of rings; here `apply_works` despawns and rebuilds
                // every scaffold on the map, so a redundant set would tear down
                // and re-raise the whole construction site. The comparison is
                // over a few dozen plain structs -- far cheaper than the work it
                // avoids.
                if works.0 != raised {
                    *works = works::Works(raised);
                }
            }
            ViewerCommand::Show(presence) => {
                // Two separate costs, and only stopping both is worth
                // anything. An inactive camera is skipped by the render graph,
                // which is the GPU half; the update mode is the CPU half, and
                // without it the whole schedule would still run sixty times a
                // second to draw nothing.
                //
                // The middle gear is what lets the map stand in the rail
                // beside a conversation. It keeps the camera -- the pane is
                // genuinely on screen and must genuinely be drawn -- and pays
                // for it by ticking at `RAIL_WAKE` instead of continuously.
                // That is enough for what a rail map has to show: the activity
                // ring's own breath takes `activity::PULSE_SECONDS`, so it
                // reads perfectly well at this cadence, and the King is
                // glancing at this map rather than flying through it. Running
                // `Continuous` behind every chamber is exactly the cost this
                // arm exists to avoid.
                if let Ok((mut camera, _, _)) = cameras.single_mut() {
                    camera.is_active = presence.showing();
                }
                // Remembered, so that `raise_world` can put back the pace the
                // King's attention actually justifies when a world finishes
                // going up. It cannot read that off the camera: `is_active` is
                // one bit and there are three answers, so a rail map would come
                // back from a raise running continuously.
                *standing = Standing(presence);
                // Not while a world is going up. `raise_world` overrides this
                // to continuous every frame it runs anyway -- see the note
                // there -- and setting it here as well would only mean the
                // pace flickering between the two systems for the length of a
                // raise.
                if !raise.in_flight() {
                    *winit = winit_for(presence);
                }
            }
        }
    }
}

/// Runs the engine flat out for as long as a camera glide lasts.
///
/// This is what answers the objection the cut was originally chosen over: the
/// rail's map ticks at [`RAIL_WAKE`], and a quarter-second tween drawn at eight
/// frames a second would read worse than the jump it replaced. So a glide buys
/// the frames it needs and gives them straight back.
///
/// The same bargain [`raise::raise_world`] strikes for the length of a world
/// going up, and it is bounded far more tightly than that one: a glide lasts
/// [`camera::GLIDE_SECONDS`], and only ever happens in the rail, because
/// `Inspect` is only ever sent there.
///
/// `Local<bool>` rather than a resource because nothing else has any business
/// asking: the question is "did *this system* raise the pace", and the answer
/// is only ever read on the next run of it. Without it the restore would fire
/// on every idle frame and would fight `Show` for the setting.
fn pace_for_glide(
    glide: Res<CameraGlide>,
    raise: Res<Raise>,
    standing: Res<Standing>,
    mut winit: ResMut<WinitSettings>,
    mut forcing: Local<bool>,
) {
    if glide.in_flight() {
        if !*forcing {
            *winit = winit_for(MapPresence::Full);
            *forcing = true;
        }
        return;
    }
    if !*forcing {
        return;
    }
    *forcing = false;
    // Not while a world is going up. `raise_world` forces the watching pace
    // every frame it runs and restores the right one when it finishes, so
    // handing the pace back here as well would only mean the two systems
    // flickering between settings for the length of the raise -- the same
    // guard, and the same reason, as the `Show` arm above.
    if !raise.in_flight() {
        *winit = winit_for(standing.0);
    }
}

/// How hard the engine works, given where the map is standing.
///
/// Shared with [`raise::raise_world`], which forces the watching pace while a
/// world goes up and restores this on the frame it finishes -- one definition,
/// so the two cannot disagree about what "idle" means.
fn winit_for(presence: MapPresence) -> WinitSettings {
    match presence {
        MapPresence::Full => WinitSettings {
            focused_mode: UpdateMode::Continuous,
            unfocused_mode: UpdateMode::reactive_low_power(IDLE_WAKE),
        },
        // The middle gear, which is what lets the map stand in the rail beside
        // a conversation: the pane is genuinely on screen and must genuinely be
        // drawn, so it keeps the camera and pays for it by ticking at
        // `RAIL_WAKE` rather than continuously. Enough for what a rail map has
        // to show -- the activity ring's breath takes `PULSE_SECONDS` -- and a
        // fraction of the cost of running `Continuous` behind every chamber.
        MapPresence::Rail => WinitSettings {
            focused_mode: UpdateMode::reactive_low_power(RAIL_WAKE),
            unfocused_mode: UpdateMode::reactive_low_power(IDLE_WAKE),
        },
        // Deliberately a short wait rather than a long one. The engine is told
        // to come back *through the bridge*, which only `apply_commands`
        // drains and which therefore only runs on an update -- so this interval
        // is also the delay before a return to the map is noticed. Ticking four
        // times a second costs almost nothing, because every system in the
        // schedule early-returns when nothing has changed, while the expensive
        // half stays off with the camera.
        MapPresence::Hidden => WinitSettings {
            focused_mode: UpdateMode::reactive_low_power(IDLE_WAKE),
            unfocused_mode: UpdateMode::reactive_low_power(IDLE_WAKE),
        },
    }
}

/// Frames the whole world: the disk, and the spire hanging under it.
///
/// The rim is sampled rather than taken as a box, and the underside enters as
/// the single point it actually is. A box would reserve a full world-width
/// slab down at the spire's tip -- most of it empty -- and push the kingdom
/// into the top third of the screen.
fn fit(rig: &mut CameraRig, world: &crate::map::MapWorld, viewport: Vec2) {
    let bounds = world.bounds;
    let center = bounds.center();
    rig.span = bounds.width.max(bounds.depth);

    let mut points: Vec<Vec3> = world
        .rim
        .iter()
        // The tallest holdings stand well inside the rim, so the rim is taken
        // at the height of a roof rather than at the ground.
        .map(|[x, y]| Vec3::new(*x, TALLEST, *y))
        .collect();
    if points.is_empty() {
        points.push(Vec3::new(center[0], TALLEST, center[1]));
    }
    points.push(Vec3::new(center[0], -world.underside.depth, center[1]));

    rig.frame_points(&points, viewport);
}

/// The height a fit assumes the tallest holding reaches.
///
/// The same 60 units the box-shaped fit used before the world grew an
/// underside.
const TALLEST: f32 = 60.0;

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
