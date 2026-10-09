//! Planar reflections (`SceneCaptureReflectActor`): the levels' water shows the world mirrored
//! in its plane - the meshes in the capture's channels (`ReflectionChannels`: mostly `Group_1`,
//! the big buildings put there) - drawn into the render target its materials sample at the
//! screen's place (`TextureRenderTarget2D`, half the screen: `TRT_HALFSIZE`). A camera mirrored
//! in the plane draws them (upright, so its image is the mirror's upside down), and a flat pass
//! turns the image over into the target, as UE3's mirrored view would have drawn it.

use crate::player::PlayerCamera;
use crate::GameState;
use bevy::camera::visibility::RenderLayers;
use bevy::camera::RenderTarget;
use bevy::prelude::*;
use bevy::render::render_resource::TextureFormat;
use dhcook::format::Scene;
use std::collections::HashMap;

pub struct ReflectionsPlugin;

impl Plugin for ReflectionsPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<ReflectionTargets>()
            .add_systems(OnEnter(GameState::InGame), spawn_cameras.after(crate::level::LevelSpawnSet))
            .add_systems(Update, (activate, resize_targets).run_if(in_state(GameState::InGame)))
            .add_systems(PostUpdate, follow.after(bevy::transform::TransformSystems::Propagate).before(bevy::camera::CameraUpdateSystems).before(bevy::camera::visibility::VisibilitySystems::UpdateFrusta).run_if(in_state(GameState::InGame)));
    }
}

/// The render layer of the meshes the reflections show (besides the main one).
pub const REFLECT_LAYER: usize = 25;
/// The flat passes' layers (one a capture, from here up).
const FLIP_LAYER: usize = 26;
/// The channels that are whole kinds of things (BSP, static meshes) rather than groups.
const KIND_CHANNELS: u16 = 0b11;
/// A reflection is drawn while Corvo is this near its plane's origin (m).
const REACH: f32 = 120.0;

/// The level's render-target textures (by texture id) and the images drawn into them.
#[derive(Resource, Default)]
pub struct ReflectionTargets(pub HashMap<u32, Handle<Image>>);

/// Images for a level's render-target textures: their share of a 1920x1080 screen (or their
/// own size), drawn at run time.
pub fn make_targets(scene: &Scene, images: &mut Assets<Image>) -> HashMap<u32, Handle<Image>> {
    let mut out = HashMap::new();
    for (i, t) in scene.textures.iter().enumerate() {
        let Some([w, h, share]) = t.render_target else { continue };
        let (w, h) = if share > 0.0 { ((1920.0 * share) as u32, (1080.0 * share) as u32) } else { ((w as u32).max(1), (h as u32).max(1)) };
        out.insert(i as u32, images.add(Image::new_target_texture(w.max(16), h.max(16), TextureFormat::Rgba8UnormSrgb, None)));
    }
    out
}

/// The groups the level's reflections show (`REFLECT_CHANNELS` bits past the kinds), for the
/// meshes to put on `REFLECT_LAYER`.
pub fn shown_groups(scene: &Scene) -> u16 {
    scene.reflections.iter().fold(0, |a, r| a | (r.channels & 0xFF00))
}

/// A reflection camera's projection: a perspective whose near clip lies along the mirror's plane
/// (Lengyel's oblique near plane, for the reversed depth): what is below the water is cut away,
/// as UE3's capture clips it.
#[derive(Debug, Clone)]
struct Oblique {
    persp: PerspectiveProjection,
    /// the plane in view space (normal, distance): the side to keep positive; zero: none
    plane: Vec4,
}

impl bevy::camera::CameraProjection for Oblique {
    fn get_clip_from_view(&self) -> Mat4 {
        oblique_clip(self.persp.get_clip_from_view(), self.plane)
    }
    fn get_clip_from_view_for_sub(&self, sub_view: &bevy::camera::SubCameraView) -> Mat4 {
        oblique_clip(self.persp.get_clip_from_view_for_sub(sub_view), self.plane)
    }
    fn update(&mut self, width: f32, height: f32) {
        self.persp.update(width, height);
    }
    fn far(&self) -> f32 {
        self.persp.far()
    }
    fn get_frustum_corners(&self, z_near: f32, z_far: f32) -> [bevy::math::Vec3A; 8] {
        self.persp.get_frustum_corners(z_near, z_far)
    }
}

/// Reverse-Z's near plane is row 4 minus row 3. Scale the replacement plane so
/// the opposite far corner remains at depth zero; a fixed scale clips distant
/// reflected geometry at wide FOVs and steep viewing angles.
fn oblique_clip(mut matrix: Mat4, plane: Vec4) -> Mat4 {
    if plane == Vec4::ZERO {
        return matrix;
    }
    let corner = matrix.inverse() * Vec4::new(plane.x.signum(), plane.y.signum(), 0.0, 1.0);
    let denominator = plane.dot(corner);
    // A plane facing away from the entire frustum cannot define a useful near
    // clip. Keep the ordinary projection rather than generating NaNs or flipping it.
    if !denominator.is_finite() || denominator <= 1e-6 {
        return matrix;
    }
    let row = Vec4::NEG_Z - plane * (-corner.z / denominator);
    matrix.x_axis.z = row.x;
    matrix.y_axis.z = row.y;
    matrix.z_axis.z = row.z;
    matrix.w_axis.z = row.w;
    matrix
}

#[derive(Component)]
struct ReflectSurface {
    target: Handle<Image>,
    /// Zero means an authored fixed-size target.
    share: f32,
}

/// Screen-relative render targets follow the physical viewport, including DPI
/// and resolution changes. Resize both passes together so the flip fills its target.
fn resize_targets(main: Query<&Camera, With<PlayerCamera>>, mut surfaces: Query<(&ReflectSurface, &mut Sprite)>, mut images: ResMut<Assets<Image>>) {
    let Some(viewport) = main.single().ok().and_then(Camera::physical_viewport_size).filter(|s| s.x > 0 && s.y > 0) else { return };
    for (surface, mut sprite) in &mut surfaces {
        if surface.share <= 0.0 {
            continue;
        }
        let size = UVec2::new(((viewport.x as f32 * surface.share) as u32).max(16), ((viewport.y as f32 * surface.share) as u32).max(16));
        for handle in [&surface.target, &sprite.image] {
            if images.get(handle).is_some_and(|image| image.size() != size) {
                if let Some(mut image) = images.get_mut(handle) {
                    image.resize(bevy::render::render_resource::Extent3d { width: size.x, height: size.y, depth_or_array_layers: 1 });
                }
            }
        }
        if sprite.custom_size != Some(size.as_vec2()) {
            sprite.custom_size = Some(size.as_vec2());
        }
    }
}

/// A reflection's camera: its plane, and its flat pass's camera.
#[derive(Component)]
struct ReflectCam {
    point: Vec3,
    normal: Vec3,
    flip: Entity,
}

fn spawn_cameras(mut commands: Commands, level: Option<Res<crate::level::LevelInfo>>, targets: Res<ReflectionTargets>, mut images: ResMut<Assets<Image>>) {
    let Some(level) = level.filter(|_| std::env::var("DH_NO_REFLECT").is_err()) else { return };
    // (captures drawing into the same target are one camera, showing all their channels)
    let mut merged: Vec<dhcook::format::Reflection> = Vec::new();
    for r in &level.scene.reflections {
        match merged.iter_mut().find(|m| m.texture == r.texture) {
            Some(m) => m.channels |= r.channels,
            None => merged.push(r.clone()),
        }
    }
    for (i, r) in merged.iter().enumerate() {
        let Some(target) = targets.0.get(&r.texture) else { continue };
        let Some(size) = images.get(target).map(|im| im.size()) else { continue };
        let drawn = images.add(Image::new_target_texture(size.x, size.y, TextureFormat::Rgba8UnormSrgb, None));
        let layers = if r.channels & KIND_CHANNELS != 0 { RenderLayers::from_layers(&[0, REFLECT_LAYER]) } else { RenderLayers::layer(REFLECT_LAYER) };
        let flip_layer = RenderLayers::layer(FLIP_LAYER + i);
        let flip = commands
            .spawn((
                Camera2d,
                Camera { order: -10 + i as isize, is_active: false, clear_color: ClearColorConfig::Custom(Color::BLACK), ..default() },
                RenderTarget::Image(target.clone().into()),
                Msaa::Off,
                flip_layer.clone(),
                DespawnOnExit(GameState::InGame),
            ))
            .id();
        commands.spawn((
            Sprite { image: drawn.clone(), flip_y: true, custom_size: Some(size.as_vec2()), ..default() },
            ReflectSurface { target: target.clone(), share: level.scene.textures.get(r.texture as usize).and_then(|t| t.render_target).map_or(0.0, |t| t[2]) },
            flip_layer,
            DespawnOnExit(GameState::InGame),
        ));
        commands.spawn((
            Camera3d::default(),
            Camera { order: -40 + i as isize, is_active: false, clear_color: ClearColorConfig::Custom(Color::BLACK), ..default() },
            RenderTarget::Image(drawn.into()),
            Msaa::Off,
            layers,
            Projection::custom(Oblique { persp: PerspectiveProjection::default(), plane: Vec4::ZERO }),
            ReflectCam { point: Vec3::from(r.point), normal: Vec3::from(r.normal).normalize_or(Vec3::Y), flip },
            DespawnOnExit(GameState::InGame),
        ));
    }
    if !level.scene.reflections.is_empty() {
        info!("reflections: {} planes", level.scene.reflections.len());
    }
    // (DH_SHOW_REFLECT: the first reflection's target in a corner of the screen)
    if std::env::var("DH_SHOW_REFLECT").is_ok() {
        if let Some(t) = level.scene.reflections.first().and_then(|r| targets.0.get(&r.texture)) {
            commands.spawn((
                ImageNode::new(t.clone()),
                Node { position_type: PositionType::Absolute, right: Val::Px(10.0), top: Val::Px(10.0), width: Val::Px(640.0), height: Val::Px(360.0), ..default() },
                GlobalZIndex(1000),
                DespawnOnExit(GameState::InGame),
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::camera::CameraProjection;

    #[test]
    fn oblique_plane_clips_only_the_underwater_side() {
        let projection = Oblique {
            persp: PerspectiveProjection { fov: 110.0_f32.to_radians(), aspect_ratio: 32.0 / 9.0, ..default() },
            plane: Vec4::new(0.6, 0.0, -0.8, -2.0),
        };
        let matrix = projection.get_clip_from_view();
        let on_plane = Vec4::new(0.0, 0.0, -2.5, 1.0);
        let clip = matrix * on_plane;
        assert!((clip.z - clip.w).abs() < 1e-5);
        let underwater = matrix * Vec4::new(-0.1, 0.0, -2.5, 1.0);
        assert!(underwater.z > underwater.w);
        // A tilted mirror on an ultrawide viewport: distant scenery near the
        // right edge used to fall beyond depth zero with the fixed coefficient.
        let scenery = matrix * Vec4::new(480.0, 0.0, -100.0, 1.0);
        assert!(scenery.z >= 0.0 && scenery.z <= scenery.w);
        assert!(scenery.x.abs() <= scenery.w);
    }

    #[test]
    fn oblique_far_corner_stays_on_the_far_plane() {
        for fov in [65.0_f32, 90.0, 110.0] {
            for aspect_ratio in [4.0 / 3.0, 16.0 / 9.0, 32.0 / 9.0] {
                let persp = PerspectiveProjection { fov: fov.to_radians(), aspect_ratio, ..default() };
                let plane = Vec4::new(-0.2, 0.6, -0.8, -2.0);
                let corner = persp.get_clip_from_view().inverse() * Vec4::new(-1.0, 1.0, 0.0, 1.0);
                let result = Oblique { persp, plane }.get_clip_from_view() * corner;
                assert!(result.z.abs() < 1e-5, "fov={fov}, aspect={aspect_ratio}: {result:?}");
            }
        }
    }

    #[test]
    fn subviews_keep_the_reflection_clip_plane() {
        let projection = Oblique { persp: PerspectiveProjection::default(), plane: Vec4::new(0.0, 0.6, -0.8, -2.0) };
        let sub_view = bevy::camera::SubCameraView { full_size: UVec2::new(1920, 1080), size: UVec2::new(960, 1080), offset: Vec2::new(960.0, 0.0) };
        let clip = projection.get_clip_from_view_for_sub(&sub_view) * Vec4::new(0.0, 0.0, -2.5, 1.0);
        assert!((clip.z - clip.w).abs() < 1e-5);
    }

    #[test]
    fn degenerate_clip_planes_leave_a_finite_projection() {
        let matrix = PerspectiveProjection::default().get_clip_from_view();
        for plane in [Vec4::ZERO, Vec4::new(0.0, 0.0, 0.0, -2.0), Vec4::new(0.0, 0.0, 1.0, -2.0)] {
            assert_eq!(oblique_clip(matrix, plane), matrix);
        }
    }

    #[test]
    fn resizing_keeps_both_passes_matched_and_preserves_fixed_targets() {
        let mut app = App::new();
        app.init_resource::<Assets<Image>>().add_systems(Update, resize_targets);
        let camera = app.world_mut().spawn((PlayerCamera, Camera::default())).id();
        let mut handles = Vec::new();
        for share in [0.5, 0.0] {
            let mut images = app.world_mut().resource_mut::<Assets<Image>>();
            let target = images.add(Image::new_target_texture(256, 256, TextureFormat::Rgba8UnormSrgb, None));
            let drawn = images.add(Image::new_target_texture(256, 256, TextureFormat::Rgba8UnormSrgb, None));
            let entity = app.world_mut().spawn((
                Sprite { image: drawn.clone(), custom_size: Some(Vec2::splat(256.0)), ..default() },
                ReflectSurface { target: target.clone(), share },
            )).id();
            handles.push((entity, target, drawn));
        }
        for viewport in [UVec2::new(1920, 1080), UVec2::new(1280, 960), UVec2::ZERO] {
            app.world_mut().get_mut::<Camera>(camera).unwrap().computed.target_info = Some(bevy::camera::RenderTargetInfo { physical_size: viewport, scale_factor: 2.0 });
            app.update();
            // A minimized viewport retains the last valid targets. DPI must not
            // halve their physical dimensions a second time.
            let expected = if viewport == UVec2::ZERO { UVec2::new(640, 480) } else { viewport / 2 };
            for (i, (entity, target, drawn)) in handles.iter().enumerate() {
                let size = if i == 0 { expected } else { UVec2::splat(256) };
                let images = app.world().resource::<Assets<Image>>();
                assert_eq!(images.get(target).unwrap().size(), size);
                assert_eq!(images.get(drawn).unwrap().size(), size);
                assert_eq!(app.world().get::<Sprite>(*entity).unwrap().custom_size, Some(size.as_vec2()));
            }
        }
    }
}

/// Which reflections draw: those whose plane the view is above, and near (decided before the
/// frame's lights are prepared: a camera that came on later in the frame would have no shadow
/// cascades of the sun, which the renderer can't do without).
fn activate(main: Query<&GlobalTransform, (With<PlayerCamera>, Without<ReflectCam>)>, mut cams: Query<(&ReflectCam, &mut Camera)>, mut flips: Query<&mut Camera, Without<ReflectCam>>) {
    let Ok(eye) = main.single() else { return };
    let at = eye.translation();
    for (rc, mut cam) in &mut cams {
        let height = (at - rc.point).dot(rc.normal);
        let on = height > 0.0 && at.distance(rc.point) < REACH;
        if cam.is_active != on {
            cam.is_active = on;
            if let Ok(mut f) = flips.get_mut(rc.flip) {
                f.is_active = on;
            }
        }
    }
}

/// Each drawing reflection's camera, mirrored from the view in its plane.
fn follow(main: Query<(&GlobalTransform, &Projection), (With<PlayerCamera>, Without<ReflectCam>)>, mut cams: Query<(&ReflectCam, &Camera, &mut Transform, &mut GlobalTransform, &mut Projection), Without<PlayerCamera>>) {
    let Ok((eye, proj)) = main.single() else { return };
    let (at, fwd, up) = (eye.translation(), eye.forward().as_vec3(), eye.up().as_vec3());
    let mirror = |v: Vec3, n: Vec3| v - 2.0 * v.dot(n) * n;
    for (rc, cam, mut t, mut gt, mut p) in &mut cams {
        if !cam.is_active {
            continue;
        }
        let height = (at - rc.point).dot(rc.normal);
        // (upright below the plane: the mirror's image turned over, which the flat pass undoes)
        let pos = at - 2.0 * height * rc.normal;
        // (after the view's own place is known this frame: set outright, no lag)
        *t = Transform::from_translation(pos).looking_to(mirror(fwd, rc.normal), -mirror(up, rc.normal));
        *gt = GlobalTransform::from(*t);
        // (the water's plane in the camera's view, for its near clip)
        let (rot, n) = (t.rotation, rc.normal);
        let plane = (rot.inverse() * n).extend(n.dot(pos - rc.point));
        if let (Projection::Perspective(src), Projection::Custom(c)) = (proj, p.as_mut()) {
            if let Some(o) = c.get_mut::<Oblique>() {
                o.persp.fov = src.fov;
                o.persp.near = src.near;
                o.persp.far = src.far;
                o.plane = plane;
            }
        }
    }
}
