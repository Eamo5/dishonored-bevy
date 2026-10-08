//! The original post-process graph's screen-warping branch (`AltScreen_Effects.PostProcessChain.
//! Test_PPG`): the vector-field materials (`PPG_*Vectors`, their `TPpMaterialPixelShader`
//! translated) drawn in turn into a quarter-resolution float target, each reading the one
//! before (the graph's switches: under water, Bend Time, Blink...), then the motion blur node
//! (`ArkPpNodeBlur`, `EPpBt_Motion` with `m_FullOffsetInVectorField`: the original
//! `FArkPpMotionBlur2PixelShader`) shifting and smearing the image along it. Each effect's
//! node also grades the image its own way while it runs (`m_bOverrideUberPp`, a layer of
//! `postfx::PowerPost`). The effects' timelines follow their tweaks (`Twk_BendTime`'s post
//! effect warm-up and cool-down, `Twk_Blink`'s warm-up wobble, move blur and cool-down wobbles).

use crate::gameplay::{PlayerStats, TimeControl};
use crate::gamedata::Data;
use crate::level::GameAssets;
use crate::powers::{Power, Powers};
use bevy::core_pipeline::{schedule::Core3d, Core3dSystems};
use bevy::platform::collections::HashMap;
use bevy::prelude::*;
use bevy::render::camera::ExtractedCamera;
use bevy::render::extract_resource::{ExtractResource, ExtractResourcePlugin};
use bevy::render::render_asset::RenderAssets;
use bevy::render::render_resource::binding_types::{sampler, texture_2d, uniform_buffer};
use bevy::render::render_resource::{
    AddressMode, BindGroup, BindGroupEntry, BindGroupLayout, BindGroupLayoutDescriptor, BindGroupLayoutEntry, BindingResource, CachedRenderPipelineId, ColorTargetState,
    ColorWrites, Extent3d, FilterMode, FragmentState, LoadOp, MipmapFilterMode, Operations, PipelineCache, RenderPassColorAttachment,
    RenderPassDescriptor, RenderPipeline, RenderPipelineDescriptor, Sampler, SamplerBindingType, SamplerDescriptor, ShaderStages, ShaderType, StoreOp,
    TextureDataOrder, TextureDescriptor, TextureDimension, TextureFormat, TextureSampleType, TextureUsages, TextureView, TextureViewDescriptor,
    UniformBuffer, VertexState,
};
use bevy::render::renderer::{RenderContext, RenderDevice, RenderQueue, ViewQuery};
use bevy::render::texture::{CachedTexture, FallbackImage, GpuImage, TextureCache};
use bevy::render::view::{ExtractedView, ViewTarget};
use bevy::render::{Render, RenderApp, RenderStartup, RenderSystems};

pub struct PostGraphPlugin;

impl Plugin for PostGraphPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<PostGraph>()
            .init_resource::<GraphShaders>()
            .add_plugins(ExtractResourcePlugin::<PostGraph>::default())
            .add_systems(Update, update_graph.run_if(in_state(crate::GameState::InGame)))
            .add_systems(OnExit(crate::GameState::InGame), clear_graph);
        let Some(render_app) = app.get_sub_app_mut(RenderApp) else { return };
        render_app
            .add_systems(RenderStartup, init_pipelines)
            .add_systems(Render, (prepare_pipelines, prepare_textures).in_set(RenderSystems::PrepareResources))
            // after Dark Vision's node, as the graph's motion blur follows its antialiasing and
            // Outsider nodes
            .add_systems(
                Core3d,
                draw_graph
                    .in_set(Core3dSystems::PostProcess)
                    .in_set(GraphPass)
                    .after(bevy::core_pipeline::fullscreen_material::fullscreen_material_system::<crate::postfx::ArkGrade>)
                    .before(bevy::anti_alias::fxaa::fxaa).before(bevy::anti_alias::smaa::smaa),
            );
    }
}

/// The graph's passes (Dark Vision's node runs before them).
#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub struct GraphPass;

/// `Test_PPG_RT2D`'s reference size: the vector fields count their blur in its pixels
/// (`vNoiseParams`' 1280 x 720), whatever the screen's.
const REFERENCE: Vec2 = Vec2::new(1280.0, 720.0);
/// `ArkPpNodeBlur_0`'s `m_MotionBlurConfig.m_LengthStrength`
const LENGTH_STRENGTH: f32 = 1.0;
/// The vector fields' target (`"1/4RES 64bits"`: the scene colour's float format)
const VECTOR_FORMAT: TextureFormat = TextureFormat::Rgba16Float;
/// `ue3prog::build_post`'s parameter slots, textures and node inputs
const MAX_PARAMS: usize = 64;
const TEXTURES: u32 = 4;
const INPUTS: u32 = 4;

/// What the graph draws this frame: the vector field's stages in order (each reading the one
/// before as its first input), and the motion blur's `gVectorFieldScales`. No stages: the
/// branch is off.
#[derive(Resource, Clone, Default, ExtractResource)]
pub struct PostGraph {
    pub stages: Vec<GraphStage>,
    /// knocked out (`Epp_Knocked`): the last frame blended in (`KO_Combine`) and the frame
    /// kept for the next (`TEST_PPG_BackupRed`)
    pub ko: Option<(GraphStage, GraphStage)>,
    /// full-resolution material passes over the image after the blur (the mask's lens:
    /// `PPG_LensCompose`)
    pub post: Vec<GraphStage>,
    pub scales: Vec4,
    pub time: f32,
    pub blur: Option<Handle<Shader>>,
}

#[derive(Clone)]
pub struct GraphStage {
    pub shader: Handle<Shader>,
    pub params: Vec<Vec4>,
    pub textures: Vec<Option<Handle<Image>>>,
}

/// The programs built so far: the materials' by shader map, the motion blur's.
#[derive(Resource, Default)]
struct GraphShaders {
    maps: HashMap<u32, Option<Handle<Shader>>>,
    blur: Option<Option<Handle<Shader>>>,
}

impl GraphShaders {
    fn map(&mut self, map: u32, shaders: &mut Assets<Shader>) -> Option<Handle<Shader>> {
        self.maps
            .entry(map)
            .or_insert_with(|| {
                let lib = crate::ue3mat::library()?;
                let m = lib.maps.get(map as usize)?;
                let code = lib.code.get(&m.shader("FLocalVertexFactory", "TPpMaterialPixelShader<FPpMaterialLinearSpaceMeshPolicy>")?)?;
                match dhcook::ue3prog::build_post(m, code) {
                    Ok(p) => Some(shaders.add(Shader::from_wgsl(p.wgsl, format!("ue3/post_{map}.wgsl")))),
                    Err(e) => {
                        warn!("post-process material {}: {e:#}", m.name);
                        None
                    }
                }
            })
            .clone()
    }

    fn blur(&mut self, shaders: &mut Assets<Shader>) -> Option<Handle<Shader>> {
        self.blur
            .get_or_insert_with(|| {
                let lib = crate::ue3mat::library()?;
                let code = lib.globals.get("FArkPpMotionBlur2PixelShader")?;
                match dhcook::ue3prog::build_global(code) {
                    Ok((wgsl, _, _)) => Some(shaders.add(Shader::from_wgsl(wgsl, "ue3/post_motion_blur.wgsl"))),
                    Err(e) => {
                        warn!("motion blur shader: {e:#}");
                        None
                    }
                }
            })
            .clone()
    }
}

/// The effects' timelines.
#[derive(Default)]
struct Timelines {
    /// Bend Time's post effect, 0..1 (its warm-up and cool-down)
    bend: f32,
    /// seconds Blink has been aimed
    aim: f32,
    /// the move's blur and the distortion when it started
    moving: Option<f32>,
    /// seconds into the cool-down, from the distortion it started at
    cool: Option<(f32, f32)>,
    /// Blink's distortion and blur (`vOpacity`)
    blink: Vec2,
    last: Option<(bool, bool, usize)>,
    /// the scripts' knock-out and weepers' effects, 0..1
    ko: f32,
    weepers: f32,
    /// the view's turning on screen (`vCamDir`), and where it looked last frame
    cam_dir: Vec2,
    last_fwd: Option<Vec3>,
    /// possessing: seconds into going in (`POSSESSION_IN`) or coming out (`POSSESSION_OUT`),
    /// and whether Corvo was inside a host last frame
    poss_in: Option<f32>,
    poss_out: Option<f32>,
    possessed: bool,
}

/// A time-varying material's parameters at `t` seconds, and whether it is still playing.
fn curves_at(assets: &GameAssets, name: &str, t: f32) -> Option<Vec<(String, Vec4)>> {
    let m = assets.post.get(name)?;
    let end = m.curves.iter().map(|c| c.end()).fold(0.0, f32::max);
    (t <= end).then(|| m.curves.iter().map(|c| (c.param.clone(), Vec4::splat(c.at(t)))).collect())
}

/// Seconds a scripted screen effect fades in and out over.
const SCRIPTED_FADE: f32 = 0.5;

fn debug_once(last: &mut Option<(bool, bool, usize)>, now: (bool, bool, usize)) {
    if *last != Some(now) {
        info!("post-process graph: bending {} blinking {} materials {}", now.0, now.1, now.2);
        *last = Some(now);
    }
}

fn clear_graph(mut graph: ResMut<PostGraph>, mut post: ResMut<crate::postfx::PowerPost>) {
    graph.stages.clear();
    graph.ko = None;
    graph.post.clear();
    post.set(crate::postfx::PowerPost::GRAPH, None);
}

/// A stage of a material by its cooked name, with its parameters overridden by name.
fn stage(assets: &GameAssets, shaders: &mut GraphShaders, shader_assets: &mut Assets<Shader>, name: &str, set: &[(&str, Vec4)]) -> Option<GraphStage> {
    let m = assets.post.get(name)?;
    let shader = shaders.map(m.map, shader_assets)?;
    let mut params: Vec<Vec4> = m.params.iter().map(|p| Vec4::from_array(*p)).collect();
    for (n, v) in set {
        for (i, pn) in m.param_names.iter().enumerate() {
            if pn == n {
                params[i] = *v;
            }
        }
    }
    Some(GraphStage { shader, params, textures: m.textures.clone() })
}

#[allow(clippy::too_many_arguments)]
fn update_graph(
    time: Res<Time<bevy::time::Real>>,
    powers: Res<Powers>,
    tc: Res<TimeControl>,
    water: Res<crate::postfx::PostOverride>,
    stats: Res<PlayerStats>,
    data: Res<Data>,
    assets: Option<Res<GameAssets>>,
    (mut graph, mut shaders, mut shader_assets, mut post): (ResMut<PostGraph>, ResMut<GraphShaders>, ResMut<Assets<Shader>>, ResMut<crate::postfx::PowerPost>),
    (vm, zoom, possession, blinding): (Option<Res<crate::kismet::Vm>>, Res<crate::zoom::Zoom>, Res<crate::possession::Possession>, Res<crate::watchtower::Blinding>),
    cam: Query<&GlobalTransform, With<crate::player::PlayerCamera>>,
    mut tl: Local<Timelines>,
) {
    let Some(assets) = assets else { return };
    let tl = &mut *tl;
    let dt = time.delta_secs().min(0.1);
    let off = std::env::var("DH_NO_PPG").is_ok();
    // Bend Time: the post effect warms up and cools down (`m_PpPerLevelParameters`)
    let bend_level = stats.power("BendTime").max(1);
    let bending = tc.bend_remaining > 0.0;
    let bend_in = data.power_f("BendTime", bend_level, "m_fPostEffectWarmupTime", 0.5).max(0.01);
    let bend_out = data.power_f("BendTime", bend_level, "m_fPostEffectCooldownTime", 0.5).max(0.01);
    tl.bend = if bending { (tl.bend + dt / bend_in).min(1.0) } else { (tl.bend - dt / bend_out).max(0.0) };
    // Blink: aimed, the distortion warms up and wobbles; the move blurs; the arrival's
    // distortion wobbles away (`m_CoolDownWobbleCount` times over `m_fCooldownTime`)
    let bl = stats.power("Blink").max(1);
    let f = |k: &str, d: f32| data.power_f("Blink", bl, k, d);
    let aiming = powers.aiming && powers.selected == Power::Blink && powers.blink.is_none();
    if aiming {
        tl.aim += dt;
        tl.cool = None;
        let warm = (tl.aim / f("m_fWarmupTime", 1.0).max(0.01)).min(1.0);
        let wobble = f("m_fWarmupWobbleMaxStrength", 0.2) * (tl.aim * f("m_fWarmupWobblePerSecond", 0.4) * std::f32::consts::TAU).sin();
        tl.blink = Vec2::new(warm * (f("m_fWarmupDistorionMinStrength", 0.2) + wobble), 0.0);
    } else if let Some((_, _, t)) = powers.blink {
        // the move: blurred most at its middle
        let x = tl.blink.x;
        let start = *tl.moving.get_or_insert(x);
        tl.aim = 0.0;
        tl.blink = Vec2::new(start.max(f("m_fWarmupDistorionMinStrength", 0.2)), f("m_fMoveBlurMaxStrength", 1.2) * (t * std::f32::consts::PI).sin().max(0.35));
    } else {
        if tl.moving.take().is_some() || (tl.aim > 0.0 && tl.cool.is_none()) {
            tl.cool = Some((0.0, tl.blink.x));
        }
        tl.aim = 0.0;
        if let Some((c, from)) = tl.cool.as_mut() {
            *c += dt;
            let len = f("m_fCooldownTime", 1.0).max(0.01);
            let k = (*c / len).min(1.0);
            let wobbles = f("m_CoolDownWobbleCount", 10.0);
            tl.blink = Vec2::new(*from * (1.0 - k) * (k * wobbles * std::f32::consts::TAU).cos(), tl.blink.y * (1.0 - dt * 12.0).max(0.0));
            if k >= 1.0 {
                tl.cool = None;
                tl.blink = Vec2::ZERO;
            }
        } else {
            tl.blink = Vec2::ZERO;
        }
    }
    let blinking = aiming || powers.blink.is_some() || tl.cool.is_some();
    if std::env::var("DH_PPG_DEBUG").is_ok() {
        debug_once(&mut tl.last, (bending, blinking, assets.post.len()));
    }
    // the scripts' screen effects (`DisSeqAct_PostProcess`)
    let scripted = |e: &str| vm.as_ref().is_some_and(|v| v.post_effects.iter().any(|x| x == e));
    let step = |v: f32, on: bool| if on { (v + dt / SCRIPTED_FADE).min(1.0) } else { (v - dt / SCRIPTED_FADE).max(0.0) };
    tl.ko = step(tl.ko, scripted("Epp_Knocked"));
    tl.weepers = step(tl.weepers, scripted("Epp_Weepers"));
    // the view's turning, on screen (right, down)
    if let Ok(cg) = cam.single() {
        let fwd = cg.forward().as_vec3();
        if let Some(prev) = tl.last_fwd {
            let d = Vec2::new(-prev.dot(cg.right().as_vec3()), prev.dot(cg.up().as_vec3()));
            if d.length() > 1e-4 {
                tl.cam_dir = tl.cam_dir.lerp(d.normalize(), 0.2);
            }
        }
        tl.last_fwd = Some(fwd);
    }
    if tl.cam_dir.length() < 0.01 {
        tl.cam_dir = Vec2::X;
    }
    // possessing (`m_WhileMaxDistort`, and `m_WarnMaxDistort` over `m_WarnFadeDuration` once
    // `m_WarnApparitionInSecondsBeforeForceExit` are left): the eye's opening and closing
    let possessed = possession.host.is_some();
    if possessed != tl.possessed {
        tl.possessed = possessed;
        if possessed {
            tl.poss_in = Some(0.0);
            tl.poss_out = None;
        } else {
            tl.poss_out = Some(0.0);
            tl.poss_in = None;
        }
    }
    for t in [&mut tl.poss_in, &mut tl.poss_out].into_iter().flatten() {
        *t += dt;
    }
    let pf = |k: &str, d: f32| data.active("Possess").and_then(|a| a.params.get(k).copied()).unwrap_or(d);
    let under = water.0.is_some();
    // the graph's switches
    let mut stages = Vec::new();
    let mut nodes: Vec<&str> = Vec::new();
    let mut s = |name: &str, set: &[(&str, Vec4)]| stage(&assets, &mut shaders, &mut shader_assets, name, set);
    if under {
        stages.extend(s("underwater_vectors", &[]));
    }
    if tl.bend > 0.0 {
        let k = tl.bend * tl.bend * (3.0 - 2.0 * tl.bend);
        if under {
            stages.extend(s("bendtime_vectors_water", &[("vBendTimeOpacity", Vec4::splat(k))]));
            nodes.push("BendTimeUnderWaterVectors");
        } else {
            stages.extend(s("bendtime_vectors", &[("vBendTimeOpacity", Vec4::splat(k))]));
            nodes.push("BendTimeVectors");
        }
    }
    if blinking {
        let v = Vec4::new(tl.blink.x, tl.blink.y, 0.0, 1.0);
        if stages.is_empty() {
            stages.extend(s("blink_vectors", &[("vOpacity", v)]));
            nodes.push("BlinkVectors2");
        } else {
            stages.extend(s("blink_vectors_previous", &[("vOpacity", v)]));
            nodes.push("BlinkVectors");
        }
    }
    if possessed {
        let warn = pf("m_WarnApparitionInSecondsBeforeForceExit", 4.0);
        let k = ((warn - possession.left) / pf("m_WarnFadeDuration", 1.8).max(0.01)).clamp(0.0, 1.0);
        let distort = pf("m_WhileMaxDistort", 0.2) + (pf("m_WarnMaxDistort", 1.2) - pf("m_WhileMaxDistort", 0.2)) * k;
        let blur = pf("m_WhileMaxBlur", 0.0) + (pf("m_WarnMaxBlur", 0.0) - pf("m_WhileMaxBlur", 0.0)) * k;
        stages.extend(s("possession_vectors", &[("vParams", Vec4::new(distort, blur, 100.0, 1.0))]));
        nodes.push("PossessionVectors");
    }
    if tl.weepers > 0.0 {
        let k = tl.weepers * tl.weepers * (3.0 - 2.0 * tl.weepers);
        stages.extend(s("adrenaline_vectors", &[("vBendTimeOpacity", Vec4::splat(k))]));
        nodes.push("PlagueVectors");
    }
    // the mask's optics (`ZoomLensSwitch`): the lens's ring blurred and bent, then its frame
    // (`ZoomLensCompose`), as the zoom comes in
    let lens = ((zoom.factor - 1.0) / 1.0).clamp(0.0, 1.0);
    let mut post_stages = Vec::new();
    // a searchlight in the eyes (`TEST_PPG_Blinded_INST`, its node's white-out grading)
    if blinding.0 > 0.01 {
        post_stages.extend(s("blinded", &[("vOpacity", Vec4::splat(blinding.0))]));
        nodes.push("Blinded");
    }
    // going in and coming out (`POSSESSION_IN_INST` / `POSSESSION_OUT_INST` curves), before
    // the lens
    for (t, name) in [(&mut tl.poss_in, "possession_in"), (&mut tl.poss_out, "possession_out")] {
        let Some(at) = *t else { continue };
        match curves_at(&assets, name, at) {
            Some(set) => {
                let set: Vec<(&str, Vec4)> = set.iter().map(|(n, v)| (n.as_str(), *v)).collect();
                post_stages.extend(s(name, &set));
            }
            None => *t = None,
        }
    }
    if lens > 0.01 {
        stages.extend(s("lens_vectors", &[("EffectStrength", Vec4::new(130.0 * lens, 1.0, 0.38, 0.55))]));
        nodes.push("LensVectors");
        post_stages.extend(s("lens_compose", &[("MaskOpacity", Vec4::splat(0.5 * lens))]));
    }
    let mut ko = None;
    if tl.ko > 0.0 {
        // the knock-out's field takes over (`KOSwitch`): a blur along the view's turning
        let k = tl.ko * tl.ko * (3.0 - 2.0 * tl.ko);
        stages.clear();
        nodes.clear();
        stages.extend(s("ko_vectors", &[("vCamDir", tl.cam_dir.extend(0.0).extend(1.0)), ("vParams", Vec4::new(k, 0.0, 0.0, 1.0))]));
        if let (Some(c), Some(b)) = (s("ko_combine", &[("vParams", Vec4::new(0.5 * k, 0.0, 0.0, 1.0))]), s("ko_backup", &[])) {
            ko = Some((c, b));
        }
    }
    if off {
        stages.clear();
        nodes.clear();
        ko = None;
        post_stages.clear();
    }
    graph.post = post_stages;
    graph.blur = if stages.is_empty() { None } else { shaders.blur(&mut shader_assets) };
    if graph.blur.is_none() {
        stages.clear();
        ko = None;
    }
    graph.ko = ko;
    if stages.len() != graph.stages.len() {
        debug!("post-process graph: {} vector stages {nodes:?} (bend {:.2}, blink {:.2?})", stages.len(), tl.bend, tl.blink);
    }
    graph.stages = stages;
    graph.scales = Vec4::new(LENGTH_STRENGTH / REFERENCE.x, LENGTH_STRENGTH / REFERENCE.y, 1.0, 1.0);
    graph.time = time.elapsed_secs_wrapped() % 3600.0;
    // the nodes' own grading while they run
    let layer = (!nodes.is_empty()).then(|| {
        let mut fields = Vec::new();
        for n in &nodes {
            if let Some(p) = data.0.post_nodes.get(*n) {
                fields.extend(crate::postfx::uber_fields(p, ""));
            }
        }
        let (fade_in, fade_out) = if tl.bend > 0.0 { (bend_in, bend_out) } else { (f("m_fWarmupTime", 1.0) * 0.5, f("m_fCooldownTime", 1.0) * 0.5) };
        crate::postfx::PostLayer { fields, fade_in, fade_out }
    });
    post.set(crate::postfx::PowerPost::GRAPH, layer);
}

/// `UePost` (`ue3prog::build_post`)
#[derive(Clone, Copy, ShaderType)]
struct PostUniform {
    p: [Vec4; MAX_PARAMS],
    time: Vec4,
    res: Vec4,
}

/// `UeGlobal` (`ue3prog::build_global`)
#[derive(Clone, Copy, ShaderType)]
struct GlobalUniform {
    c: [Vec4; 16],
}

#[derive(Resource)]
struct GraphPipelines {
    /// what an unconnected node input reads: zero vectors (a cleared target; the fallback
    /// image is white, which reads as a full-screen offset)
    zero: TextureView,
    stage_layout: BindGroupLayoutDescriptor,
    blur_layout: BindGroupLayoutDescriptor,
    clamp: Sampler,
    repeat: Sampler,
    /// (program, target format) -> pipeline
    stages: HashMap<(AssetId<Shader>, TextureFormat), CachedRenderPipelineId>,
    blur: HashMap<(AssetId<Shader>, TextureFormat), CachedRenderPipelineId>,
}

fn init_pipelines(mut commands: Commands, device: Res<RenderDevice>, queue: Res<RenderQueue>) {
    let zero = device
        .create_texture_with_data(
            &queue,
            &TextureDescriptor {
                label: Some("ppg_zero"),
                size: Extent3d { width: 1, height: 1, depth_or_array_layers: 1 },
                mip_level_count: 1,
                sample_count: 1,
                dimension: TextureDimension::D2,
                format: VECTOR_FORMAT,
                usage: TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            },
            TextureDataOrder::default(),
            &[0u8; 8],
        )
        .create_view(&TextureViewDescriptor::default());
    let f = ShaderStages::FRAGMENT;
    let tex = || texture_2d(TextureSampleType::Float { filterable: true });
    let mut entries: Vec<BindGroupLayoutEntry> = vec![uniform_buffer::<PostUniform>(false).build(0, f)];
    for k in 0..TEXTURES {
        entries.push(sampler(SamplerBindingType::Filtering).build(1 + k, f));
        entries.push(tex().build(1 + TEXTURES + k, f));
    }
    for k in 0..INPUTS {
        entries.push(sampler(SamplerBindingType::Filtering).build(1 + 2 * TEXTURES + k, f));
        entries.push(tex().build(1 + 2 * TEXTURES + INPUTS + k, f));
    }
    let stage_layout = BindGroupLayoutDescriptor::new("ppg_stage_layout", &entries);
    let blur_entries = vec![
        uniform_buffer::<GlobalUniform>(false).build(0, f),
        sampler(SamplerBindingType::Filtering).build(1, f),
        tex().build(2, f),
        sampler(SamplerBindingType::Filtering).build(3, f),
        tex().build(4, f),
    ];
    let blur_layout = BindGroupLayoutDescriptor::new("ppg_blur_layout", &blur_entries);
    let clamp = device.create_sampler(&SamplerDescriptor { mag_filter: FilterMode::Linear, min_filter: FilterMode::Linear, ..default() });
    let repeat = device.create_sampler(&SamplerDescriptor {
        address_mode_u: AddressMode::Repeat,
        address_mode_v: AddressMode::Repeat,
        mag_filter: FilterMode::Linear,
        min_filter: FilterMode::Linear,
        mipmap_filter: MipmapFilterMode::Linear,
        ..default()
    });
    commands.insert_resource(GraphPipelines { zero, stage_layout, blur_layout, clamp, repeat, stages: HashMap::default(), blur: HashMap::default() });
    commands.insert_resource(KoHistory::default());
}

fn pipeline(label: &'static str, layout: BindGroupLayoutDescriptor, shader: &Handle<Shader>, format: TextureFormat) -> RenderPipelineDescriptor {
    RenderPipelineDescriptor {
        label: Some(label.into()),
        layout: vec![layout],
        vertex: VertexState { shader: shader.clone(), entry_point: Some("post_vertex".into()), ..default() },
        fragment: Some(FragmentState {
            shader: shader.clone(),
            entry_point: Some("post_fragment".into()),
            targets: vec![Some(ColorTargetState { format, blend: None, write_mask: ColorWrites::ALL })],
            ..default()
        }),
        ..default()
    }
}

fn prepare_pipelines(mut pipelines: ResMut<GraphPipelines>, graph: Res<PostGraph>, pipeline_cache: Res<PipelineCache>, views: Query<&ExtractedView, With<ExtractedCamera>>) {
    let mut want: Vec<(&Handle<Shader>, TextureFormat)> = graph.stages.iter().map(|s| (&s.shader, VECTOR_FORMAT)).collect();
    if let Some((_, backup)) = graph.ko.as_ref() {
        want.push((&backup.shader, VECTOR_FORMAT));
    }
    for view in &views {
        if let Some((combine, _)) = graph.ko.as_ref() {
            want.push((&combine.shader, view.target_format));
        }
        want.extend(graph.post.iter().map(|s| (&s.shader, view.target_format)));
        let Some(blur) = graph.blur.as_ref() else { continue };
        let key = (blur.id(), view.target_format);
        if !pipelines.blur.contains_key(&key) {
            let id = pipeline_cache.queue_render_pipeline(pipeline("ppg_motion_blur", pipelines.blur_layout.clone(), blur, view.target_format));
            pipelines.blur.insert(key, id);
        }
    }
    for (shader, format) in want {
        if !pipelines.stages.contains_key(&(shader.id(), format)) {
            let id = pipeline_cache.queue_render_pipeline(pipeline("ppg_stage", pipelines.stage_layout.clone(), shader, format));
            pipelines.stages.insert((shader.id(), format), id);
        }
    }
}

/// A view's vector fields: the stages draw into them in turn.
#[derive(Component)]
struct VectorTextures {
    a: CachedTexture,
    b: CachedTexture,
    size: UVec2,
}

/// The knock-out's last frame (`Test_PPG_RT2D_2`, a quarter the size), kept from one frame to
/// the next, by view.
#[derive(Resource, Default)]
struct KoHistory(HashMap<Entity, (UVec2, TextureView)>);

fn prepare_textures(
    mut commands: Commands,
    graph: Res<PostGraph>,
    mut cache: ResMut<TextureCache>,
    mut history: ResMut<KoHistory>,
    device: Res<RenderDevice>,
    views: Query<(Entity, &ExtractedCamera), With<crate::postfx::ArkGrade>>,
) {
    if graph.ko.is_none() {
        history.0.clear();
    }
    if graph.stages.is_empty() {
        return;
    }
    for (e, camera) in &views {
        let Some(size) = camera.physical_target_size else { continue };
        let size = UVec2::new((size.x / 4).max(1), (size.y / 4).max(1));
        let desc = |label: &'static str| TextureDescriptor {
            label: Some(label),
            size: Extent3d { width: size.x, height: size.y, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: TextureDimension::D2,
            format: VECTOR_FORMAT,
            usage: TextureUsages::RENDER_ATTACHMENT | TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        };
        let a = cache.get(&device, desc("ppg_vectors_a"));
        let b = cache.get(&device, desc("ppg_vectors_b"));
        commands.entity(e).insert(VectorTextures { a, b, size });
        if graph.ko.is_some() && history.0.get(&e).is_none_or(|h| h.0 != size) {
            let view = device.create_texture(&desc("ppg_ko_history")).create_view(&TextureViewDescriptor::default());
            history.0.insert(e, (size, view));
        }
    }
}

/// A material stage's bind group: its parameters and textures, and its node inputs (the rest
/// read zero).
#[allow(clippy::too_many_arguments)]
fn stage_bind_group(
    device: &RenderDevice,
    queue: &RenderQueue,
    layout: &BindGroupLayout,
    pipelines: &GraphPipelines,
    images: &RenderAssets<GpuImage>,
    fallback: &TextureView,
    s: &GraphStage,
    inputs: &[&TextureView],
    time: f32,
    res: Vec4,
) -> Option<BindGroup> {
    let mut p = [Vec4::ZERO; MAX_PARAMS];
    for (d, v) in p.iter_mut().zip(&s.params) {
        *d = *v;
    }
    let mut uniform = UniformBuffer::from(PostUniform { p, time: Vec4::splat(time), res });
    uniform.write_buffer(device, queue);
    let binding = uniform.binding()?;
    let tex_views: Vec<_> =
        (0..TEXTURES as usize).map(|k| s.textures.get(k).and_then(|t| t.as_ref()).and_then(|h| images.get(h)).map(|g| &g.texture_view).unwrap_or(fallback)).collect();
    let mut entries = vec![BindGroupEntry { binding: 0, resource: binding }];
    for k in 0..TEXTURES {
        entries.push(BindGroupEntry { binding: 1 + k, resource: BindingResource::Sampler(&pipelines.repeat) });
        entries.push(BindGroupEntry { binding: 1 + TEXTURES + k, resource: BindingResource::TextureView(tex_views[k as usize]) });
    }
    for k in 0..INPUTS {
        let input = inputs.get(k as usize).copied().unwrap_or(&pipelines.zero);
        entries.push(BindGroupEntry { binding: 1 + 2 * TEXTURES + k, resource: BindingResource::Sampler(&pipelines.clamp) });
        entries.push(BindGroupEntry { binding: 1 + 2 * TEXTURES + INPUTS + k, resource: BindingResource::TextureView(input) });
    }
    Some(device.create_bind_group("ppg_stage", layout, &entries))
}

/// A full-screen pass into `target`.
fn full_screen(ctx: &mut RenderContext<'_, '_>, label: &'static str, target: &TextureView, pipeline: &RenderPipeline, bind_group: &BindGroup) {
    let mut pass = ctx.begin_tracked_render_pass(RenderPassDescriptor {
        label: Some(label),
        color_attachments: &[Some(RenderPassColorAttachment {
            view: target,
            depth_slice: None,
            resolve_target: None,
            ops: Operations { load: LoadOp::Clear(Default::default()), store: StoreOp::Store },
        })],
        depth_stencil_attachment: None,
        timestamp_writes: None,
        occlusion_query_set: None,
        multiview_mask: None,
    });
    pass.set_render_pipeline(pipeline);
    pass.set_bind_group(0, bind_group, &[]);
    pass.draw(0..3, 0..1);
}

#[allow(clippy::too_many_arguments)]
fn draw_graph(
    view: ViewQuery<(&ExtractedView, &ViewTarget, Option<&VectorTextures>)>,
    graph: Res<PostGraph>,
    pipelines: Res<GraphPipelines>,
    history: Res<KoHistory>,
    pipeline_cache: Res<PipelineCache>,
    images: Res<RenderAssets<GpuImage>>,
    fallback: Res<FallbackImage>,
    (device, queue): (Res<RenderDevice>, Res<RenderQueue>),
    mut ctx: RenderContext,
) {
    let view_entity = view.entity();
    let (extracted, target, textures) = view.into_inner();
    let format = extracted.target_format;
    let get = |shader: &Handle<Shader>, format: TextureFormat| pipelines.stages.get(&(shader.id(), format)).and_then(|id| pipeline_cache.get_render_pipeline(*id));
    let layout = pipeline_cache.get_bind_group_layout(&pipelines.stage_layout);
    let fallback_view = &fallback.d2.texture_view;
    if let (Some(textures), Some(blur)) = (textures.filter(|_| !graph.stages.is_empty()), graph.blur.as_ref()) {
        draw_field(&mut ctx, &graph, &pipelines, &history, &pipeline_cache, &images, fallback_view, (&device, &queue), (view_entity, format, target, &layout), (textures, blur));
    }
    // the image stages, at full resolution
    let size = extracted.viewport.zw().max(UVec2::ONE);
    let res = Vec4::new(size.x as f32, size.y as f32, 1.0 / size.x as f32, 1.0 / size.y as f32);
    for s in &graph.post {
        let Some(pipeline) = get(&s.shader, format) else { return };
        let post = target.post_process_write();
        let Some(bg) = stage_bind_group(&device, &queue, &layout, &pipelines, &images, fallback_view, s, &[post.source], graph.time, res) else { return };
        full_screen(&mut ctx, "ppg image", post.destination, pipeline, &bg);
    }
}

/// The vector field, the motion blur along it, the knock-out's last frame.
#[allow(clippy::too_many_arguments)]
fn draw_field(
    ctx: &mut RenderContext<'_, '_>,
    graph: &PostGraph,
    pipelines: &GraphPipelines,
    history: &KoHistory,
    pipeline_cache: &PipelineCache,
    images: &RenderAssets<GpuImage>,
    fallback_view: &TextureView,
    (device, queue): (&RenderDevice, &RenderQueue),
    (view_entity, format, target, layout): (Entity, TextureFormat, &ViewTarget, &BindGroupLayout),
    (textures, blur): (&VectorTextures, &Handle<Shader>),
) {
    let get = |shader: &Handle<Shader>, format: TextureFormat| pipelines.stages.get(&(shader.id(), format)).and_then(|id| pipeline_cache.get_render_pipeline(*id));
    let Some(blur_pipeline) = pipelines.blur.get(&(blur.id(), format)).and_then(|id| pipeline_cache.get_render_pipeline(*id)) else { return };
    let Some(stage_pipelines) = graph.stages.iter().map(|s| get(&s.shader, VECTOR_FORMAT)).collect::<Option<Vec<_>>>() else { return };
    let res = Vec4::new(textures.size.x as f32, textures.size.y as f32, 1.0 / textures.size.x as f32, 1.0 / textures.size.y as f32);
    let targets = [&textures.a, &textures.b];
    // the vector field, stage by stage
    for (i, (s, pipeline)) in graph.stages.iter().zip(stage_pipelines).enumerate() {
        let input = if i > 0 { &targets[(i - 1) % 2].default_view } else { &pipelines.zero };
        let Some(bg) = stage_bind_group(device, queue, layout, pipelines, images, fallback_view, s, &[input], graph.time, res) else { return };
        full_screen(ctx, "ppg vectors", &targets[i % 2].default_view, pipeline, &bg);
    }
    // the image along it
    let field = &targets[(graph.stages.len() - 1) % 2].default_view;
    let mut c = [Vec4::ZERO; 16];
    c[0] = graph.scales;
    let mut uniform = UniformBuffer::from(GlobalUniform { c });
    uniform.write_buffer(device, queue);
    let Some(binding) = uniform.binding() else { return };
    let post = target.post_process_write();
    let entries = [
        BindGroupEntry { binding: 0, resource: binding },
        BindGroupEntry { binding: 1, resource: BindingResource::Sampler(&pipelines.clamp) },
        BindGroupEntry { binding: 2, resource: BindingResource::TextureView(post.source) },
        BindGroupEntry { binding: 3, resource: BindingResource::Sampler(&pipelines.clamp) },
        BindGroupEntry { binding: 4, resource: BindingResource::TextureView(field) },
    ];
    let bg = device.create_bind_group("ppg_motion_blur", &pipeline_cache.get_bind_group_layout(&pipelines.blur_layout), &entries);
    full_screen(ctx, "ppg motion blur", post.destination, blur_pipeline, &bg);
    // knocked out: this frame with the last one (`BlendLastFrame`), then this one kept
    // (`BackupLastFrame`)
    let Some((combine, backup)) = graph.ko.as_ref() else { return };
    let Some((_, last)) = history.0.get(&view_entity) else { return };
    let (Some(combine_pipeline), Some(backup_pipeline)) = (get(&combine.shader, format), get(&backup.shader, VECTOR_FORMAT)) else { return };
    let post = target.post_process_write();
    let Some(bg) = stage_bind_group(device, queue, layout, pipelines, images, fallback_view, combine, &[post.source, last], graph.time, res) else { return };
    full_screen(ctx, "ppg ko blend", post.destination, combine_pipeline, &bg);
    let Some(bg) = stage_bind_group(device, queue, layout, pipelines, images, fallback_view, backup, &[post.source], graph.time, res) else { return };
    full_screen(ctx, "ppg ko backup", last, backup_pipeline, &bg);
}
