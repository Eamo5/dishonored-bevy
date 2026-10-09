//! Lens flares (`LensFlareSource`: the candles' glow in the bridge's guard rooms): the flare's
//! source element facing the view at the source, drawn over the scene as the original draws it
//! (`SDPG_Foreground`), its size and opacity following the view's distance (`DistMap_Scale`,
//! `DistMap_Alpha`), shaded as `LensFlare_PMAT` shades it (`flare.wgsl`); hidden behind a wall
//! it fades (the original's occlusion query, here a ray).

use crate::level::{LevelInfo, GROUP_WORLD};
use crate::player::PlayerCamera;
use crate::GameState;
use bevy::asset::embedded_asset;
use bevy::camera::visibility::NoFrustumCulling;
use bevy::light::{NotShadowCaster, NotShadowReceiver};
use bevy::mesh::MeshVertexBufferLayoutRef;
use bevy::pbr::{MaterialPipeline, MaterialPipelineKey};
use bevy::prelude::*;
use bevy::reflect::TypePath;
use bevy::render::render_resource::{AsBindGroup, CompareFunction, RenderPipelineDescriptor, ShaderType, SpecializedMeshPipelineError};
use bevy::shader::ShaderRef;
use bevy_rapier3d::prelude::*;
use dhcook::format::LensFlareDef;

pub struct FlaresPlugin;

impl Plugin for FlaresPlugin {
    fn build(&self, app: &mut App) {
        embedded_asset!(app, "flare.wgsl");
        app.add_plugins(MaterialPlugin::<FlareMaterial>::default())
            .add_systems(OnEnter(GameState::InGame), spawn_flares.after(crate::level::LevelSpawnSet))
            .add_systems(PostUpdate, update_flares.after(bevy::transform::TransformSystems::Propagate).run_if(in_state(GameState::InGame)));
    }
}

/// How fast a flare fades in and out as it is hidden and seen (per second).
const FADE: f32 = 6.0;

#[derive(ShaderType, Clone, Copy, Default, PartialEq)]
struct FlareParams {
    /// the element's colour times the material's, its opacity (faded)
    color: Vec4,
    /// power, the radial dimming (1 - radial distance x factor), opacity range
    shape: Vec4,
    /// the pulse: speed, range, base, on
    glow: Vec4,
}

#[derive(Asset, TypePath, AsBindGroup, Clone)]
pub struct FlareMaterial {
    #[uniform(0)]
    params: FlareParams,
}

impl Material for FlareMaterial {
    fn fragment_shader() -> ShaderRef {
        "embedded://dishonored/flare.wgsl".into()
    }

    fn alpha_mode(&self) -> AlphaMode {
        AlphaMode::Add
    }

    fn enable_shadows() -> bool {
        false
    }

    fn enable_prepass() -> bool {
        false
    }

    fn specialize(_pipeline: &MaterialPipeline, descriptor: &mut RenderPipelineDescriptor, _layout: &MeshVertexBufferLayoutRef, _key: MaterialPipelineKey<Self>) -> Result<(), SpecializedMeshPipelineError> {
        descriptor.primitive.cull_mode = None;
        // (in front of everything: the foreground)
        if let Some(ds) = descriptor.depth_stencil.as_mut() {
            ds.depth_compare = Some(CompareFunction::Always);
            ds.depth_write_enabled = Some(false);
        }
        Ok(())
    }
}

#[derive(Component)]
struct Flare {
    def: LensFlareDef,
    material: Handle<FlareMaterial>,
    /// how much of it shows (eased as it is hidden and seen)
    seen: f32,
}

fn spawn_flares(mut commands: Commands, level: Option<Res<LevelInfo>>, mut meshes: ResMut<Assets<Mesh>>, mut materials: ResMut<Assets<FlareMaterial>>) {
    let Some(level) = level else { return };
    if level.scene.lens_flares.is_empty() {
        return;
    }
    let quad = meshes.add(Rectangle::new(1.0, 1.0));
    for f in &level.scene.lens_flares {
        let material = materials.add(FlareMaterial { params: FlareParams::default() });
        commands.spawn((
            Flare { def: f.clone(), material: material.clone(), seen: 0.0 },
            Mesh3d(quad.clone()),
            MeshMaterial3d(material),
            Transform::from_translation(Vec3::from(f.position)),
            Visibility::Hidden,
            NoFrustumCulling,
            NotShadowCaster,
            NotShadowReceiver,
            DespawnOnExit(GameState::InGame),
        ));
    }
    info!("{} lens flares", level.scene.lens_flares.len());
}

/// Each flare faces the view, sized and faded for its distance and its place on the screen.
fn update_flares(
    time: Res<Time>,
    cam: Query<(&GlobalTransform, &Camera), With<PlayerCamera>>,
    mut flares: Query<(&mut Flare, &mut Transform, &mut Visibility)>,
    mut materials: ResMut<Assets<FlareMaterial>>,
    rapier: ReadRapierContext,
) {
    let Ok((ct, camera)) = cam.single() else { return };
    let eye = ct.translation();
    let ctx = rapier.single().ok();
    let walls = QueryFilter::default().groups(CollisionGroups::new(Group::ALL, GROUP_WORLD));
    let dt = time.delta_secs();
    for (mut f, mut t, mut vis) in &mut flares {
        let at = Vec3::from(f.def.position);
        let to = at - eye;
        let dist = to.length();
        // seen: in front, nothing solid between (up to the flame)
        let ndc = camera.world_to_ndc(ct, at).filter(|n| n.z > 0.0 && n.x.abs() < 1.2 && n.y.abs() < 1.2);
        let clear = ndc.is_some() && (std::env::var("DH_FLARE_NOOCC").is_ok() || ctx.as_ref().is_none_or(|c| dist < 0.3 || c.cast_ray(eye, to / dist.max(1e-3), dist - 0.25, true, walls).is_none()));
        let want = if clear { 1.0 } else { 0.0 };
        if std::env::var("DH_FLARE_LOG").is_ok() {
            let hit = ctx.as_ref().and_then(|c| c.cast_ray(eye, to / dist.max(1e-3), dist - 0.25, true, walls)).map(|(e, toi)| (e, toi));
            info!("flare at {at:.2}: {dist:.1} m, ndc {ndc:?}, hit {hit:?}, seen {:.2}", f.seen);
        }
        f.seen += (want - f.seen).clamp(-FADE * dt, FADE * dt);
        let show = f.seen > 0.001;
        let v = if show { Visibility::Inherited } else { Visibility::Hidden };
        if *vis != v {
            *vis = v;
        }
        if !show {
            continue;
        }
        let uu = dist * 100.0;
        let mut o = [1.0; 3];
        let scale = match &f.def.dist_scale {
            Some(d) if crate::particles::sample(d, uu, [0.5; 3], &mut o) > 0 => o[0],
            _ => 1.0,
        };
        let mut o = [1.0; 3];
        let alpha = match &f.def.dist_alpha {
            Some(d) if crate::particles::sample(d, uu, [0.5; 3], &mut o) > 0 => o[0],
            _ => 1.0,
        } * f.def.alpha;
        // the source's distance from the screen's middle (to its corner, normalised)
        let r = ndc.map(|n| n.truncate().length() / if f.def.normalize { std::f32::consts::SQRT_2 } else { 1.0 }).unwrap_or(1.0);
        *t = Transform { translation: at, rotation: ct.to_scale_rotation_translation().1, scale: Vec3::splat(f.def.size * scale) };
        let c = Vec3::from(f.def.color) * Vec3::from(f.def.tint);
        let params = FlareParams {
            color: c.extend(alpha * f.seen),
            shape: Vec4::new(f.def.power, 1.0 - r * f.def.radial, f.def.opacity[0], f.def.opacity[1]),
            glow: f.def.glow.map(|g| Vec4::new(g[0], g[1], g[2], 1.0)).unwrap_or(Vec4::ZERO),
        };
        if let Some(mut m) = materials.get_mut(&f.material) {
            if m.params != params {
                m.params = params;
            }
        }
    }
}
