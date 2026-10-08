//! Dishonored's fog (`DisFog` actors' `DisFogComponent` layers), drawn the way the original's
//! full-screen pass (`FDisFogPixelShader<N><M>Layer`, up to four layers) does it, over the
//! opaque scene and under the translucent one (`fog.wgsl`). Exterior layers fog the outside;
//! inside (a roof overhead) the interior layers join them; levels streamed out take theirs
//! with them, and the level scripts can switch layers. Under water, the water's own fog layer
//! (`DishonoredWaterVolumeInfo.m_FogComponent`) is all there is.

use crate::level::{LevelInfo, GROUP_WORLD};
use crate::player::PlayerCamera;
use crate::GameState;
use bevy::asset::{embedded_asset, RenderAssetUsages};
use bevy::camera::visibility::NoFrustumCulling;
use bevy::light::{NotShadowCaster, NotShadowReceiver};
use bevy::mesh::{MeshVertexBufferLayoutRef, PrimitiveTopology};
use bevy::pbr::{MaterialPipeline, MaterialPipelineKey};
use bevy::prelude::*;
use bevy::reflect::TypePath;
use bevy::render::render_resource::{AsBindGroup, CompareFunction, RenderPipelineDescriptor, ShaderType, SpecializedMeshPipelineError};
use bevy::shader::ShaderRef;
use bevy_rapier3d::prelude::*;
use dhcook::format::{FogLayer, LightKind};

pub struct FogPlugin;

impl Plugin for FogPlugin {
    fn build(&self, app: &mut App) {
        embedded_asset!(app, "fog.wgsl");
        app.add_plugins(MaterialPlugin::<FogMaterial>::default())
            .init_resource::<FogState>()
            .add_systems(OnEnter(GameState::InGame), spawn_fog.after(crate::player::spawn_player))
            .add_systems(Update, update_fog.after(crate::player::PlayerMoveSet).run_if(in_state(GameState::InGame)));
    }
}

const MAX_LAYERS: usize = 4;
const LUT_SIZE: usize = 128;

#[derive(ShaderType, Clone, Copy, Default, PartialEq)]
struct GpuLayer {
    color: Vec4,
    band: Vec4,
    planes: Vec4,
}

#[derive(ShaderType, Clone, Copy, PartialEq)]
pub struct FogParams {
    layers: [GpuLayer; MAX_LAYERS],
    sun: Vec4,
    lut: [Vec4; LUT_SIZE],
}

impl Default for FogParams {
    fn default() -> Self {
        FogParams { layers: [GpuLayer::default(); MAX_LAYERS], sun: Vec4::ZERO, lut: [Vec4::ZERO; LUT_SIZE] }
    }
}

#[derive(Asset, TypePath, AsBindGroup, Clone)]
pub struct FogMaterial {
    #[uniform(0)]
    params: FogParams,
}

impl Material for FogMaterial {
    fn vertex_shader() -> ShaderRef {
        "embedded://dishonored/fog.wgsl".into()
    }

    fn fragment_shader() -> ShaderRef {
        "embedded://dishonored/fog.wgsl".into()
    }

    fn alpha_mode(&self) -> AlphaMode {
        AlphaMode::Premultiplied
    }

    fn enable_shadows() -> bool {
        false
    }

    fn enable_prepass() -> bool {
        false
    }

    fn specialize(_pipeline: &MaterialPipeline, descriptor: &mut RenderPipelineDescriptor, layout: &MeshVertexBufferLayoutRef, _key: MaterialPipelineKey<Self>) -> Result<(), SpecializedMeshPipelineError> {
        descriptor.vertex.buffers = vec![layout.0.get_layout(&[Mesh::ATTRIBUTE_POSITION.at_shader_location(0)])?];
        descriptor.primitive.cull_mode = None;
        // the whole screen, whatever is in front
        if let Some(ds) = descriptor.depth_stencil.as_mut() {
            ds.depth_compare = Some(CompareFunction::Always);
            ds.depth_write_enabled = Some(false);
        }
        Ok(())
    }
}

/// The fog layers switched on (level scripts toggle `DisFog` actors), and the water's.
#[derive(Resource, Default)]
pub struct FogState {
    pub enabled: Vec<bool>,
    /// layers' properties the scenes animate: (layer, `Opacity` / `NearPlane` / `FarPlane` /
    /// `Height`), value (UE units)
    pub overrides: std::collections::HashMap<(usize, String), f32>,
    indoors: bool,
    probe_t: f32,
}

#[derive(Component)]
struct FogPass(Handle<FogMaterial>);

fn spawn_fog(mut commands: Commands, cams: Query<Entity, With<PlayerCamera>>, mut meshes: ResMut<Assets<Mesh>>, mut mats: ResMut<Assets<FogMaterial>>, mut state: ResMut<FogState>, level: Option<Res<LevelInfo>>) {
    let Ok(cam) = cams.single() else { return };
    *state = FogState { enabled: level.map(|l| l.scene.fog_layers.iter().map(|f| f.enabled).collect()).unwrap_or_default(), ..default() };
    if std::env::var("DH_NO_FOG").is_ok() {
        return;
    }
    let mesh = Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::RENDER_WORLD).with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, vec![[-1.0f32, -1.0, 0.0], [3.0, -1.0, 0.0], [-1.0, 3.0, 0.0]]);
    let mat = mats.add(FogMaterial { params: FogParams::default() });
    // far down the view: sorted behind every translucent surface, so drawn first
    let pass = commands
        .spawn((
            FogPass(mat.clone()),
            Mesh3d(meshes.add(mesh)),
            MeshMaterial3d(mat),
            Transform::from_xyz(0.0, 0.0, -50000.0),
            Visibility::default(),
            NoFrustumCulling,
            NotShadowCaster,
            NotShadowReceiver,
        ))
        .id();
    commands.entity(cam).add_child(pass);
}

fn gpu_layer(l: &FogLayer) -> GpuLayer {
    GpuLayer {
        color: Vec3::from(l.color).extend(l.opacity),
        band: Vec4::new(l.min_height, l.max_height, l.height_density, l.sun.unwrap_or(0.0)),
        planes: Vec4::new(l.near, 1.0 / (l.far - l.near).abs().max(1e-3) * (l.far - l.near).signum(), l.no_fog, if l.lut.is_empty() { 0.0 } else { LUT_SIZE as f32 }),
    }
}

#[allow(clippy::too_many_arguments)]
fn update_fog(
    time: Res<Time>,
    level: Option<Res<LevelInfo>>,
    vm: Option<Res<crate::kismet::Vm>>,
    mut state: ResMut<FogState>,
    (swim, waters): (Res<crate::swim::Swim>, Option<Res<crate::swim::Waters>>),
    cam: Query<&GlobalTransform, With<PlayerCamera>>,
    rapier: ReadRapierContext,
    pass: Query<&FogPass>,
    mut mats: ResMut<Assets<FogMaterial>>,
) {
    let (Some(level), Ok(pass), Ok(cam)) = (level, pass.single(), cam.single()) else { return };
    let eye = cam.translation();
    // a roof overhead: inside
    state.probe_t -= time.delta_secs();
    if state.probe_t <= 0.0 {
        state.probe_t = 0.25;
        if let Ok(ctx) = rapier.single() {
            state.indoors = ctx.cast_ray(eye, Vec3::Y, 60.0, true, QueryFilter::default().groups(CollisionGroups::new(Group::ALL, GROUP_WORLD))).is_some();
        }
    }
    let scene = &level.scene;
    // the water's fog hangs from its surface (the info is shared by waters at any height)
    let water = swim.under.and_then(|i| waters.as_ref().and_then(|w| w.layer(i))).map(|l| {
        let depth = (l.max_height - l.min_height).abs();
        FogLayer { max_height: swim.surface, min_height: swim.surface - depth, ..l.clone() }
    });
    // the scenes' changes to layers
    let tuned: Vec<FogLayer> = scene
        .fog_layers
        .iter()
        .enumerate()
        .map(|(i, l)| {
            let mut l = l.clone();
            for ((li, field), v) in &state.overrides {
                if *li != i {
                    continue;
                }
                match field.as_str() {
                    "Opacity" => l.opacity = *v,
                    "NearPlane" => l.near = v * 0.01,
                    "FarPlane" => l.far = v * 0.01,
                    "Height" => {
                        let lo = l.min_height;
                        l.max_height = lo + v * 0.01;
                    }
                    _ => {}
                }
            }
            l
        })
        .collect();
    let mut chosen: Vec<&FogLayer> = Vec::new();
    match water.as_ref() {
        Some(w) => chosen.push(w),
        None => {
            let live = |i: usize, l: &FogLayer| {
                state.enabled.get(i).copied().unwrap_or(l.enabled) && !scene.levels.get(l.level as usize).is_some_and(|lr| vm.as_ref().is_some_and(|vm| vm.level_out(&lr.name)))
            };
            // inside, the interior layers first
            if state.indoors {
                chosen.extend(tuned.iter().enumerate().filter(|(i, l)| l.interior && live(*i, l)).map(|(_, l)| l));
            }
            chosen.extend(tuned.iter().enumerate().filter(|(i, l)| !l.interior && live(*i, l)).map(|(_, l)| l));
            chosen.truncate(MAX_LAYERS);
        }
    }
    let sun = scene.lights.iter().find(|l| l.kind == LightKind::Directional && l.enabled).map(|l| -Vec3::from(l.direction).normalize_or(Vec3::NEG_Y)).unwrap_or(Vec3::Y);
    let mut p = FogParams { sun: sun.extend(chosen.len() as f32), ..default() };
    for (i, l) in chosen.iter().enumerate() {
        p.layers[i] = gpu_layer(l);
        if !l.lut.is_empty() {
            for j in 0..LUT_SIZE {
                // resampled to the shader's 128 entries
                let x = (j as f32 + 0.5) / LUT_SIZE as f32 * l.lut.len() as f32 - 0.5;
                let a = x.floor().clamp(0.0, (l.lut.len() - 1) as f32);
                let b = (a + 1.0).min((l.lut.len() - 1) as f32);
                let v = l.lut[a as usize] + (l.lut[b as usize] - l.lut[a as usize]) * (x - a).clamp(0.0, 1.0);
                let k = i * LUT_SIZE + j;
                p.lut[k / 4][k % 4] = v;
            }
        }
    }
    if mats.get(&pass.0).is_some_and(|m| m.params != p) {
        if let Some(mut m) = mats.get_mut(&pass.0) {
            m.params = p;
        }
    }
}
