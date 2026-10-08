//! The post-process graph's painterly branch (`Test_PPG`: `KOKuwa` -> `KuwaHalfRes` ->
//! `KuwaHalfResSwitch` -> `KuwaFullRes` -> `KuwaSwitch`), which the original shows only by its
//! viewport client's debug exec `KUWA` (`KuwaSwitch` defaults off; no level or power turns it
//! on). The same here, on F4 (or `DH_KUWA`, or the scripts' `kuwa`): the scene after its
//! anti-aliasing drawn at half size, through the 5 x 5 Kuwahara filter (`ArkPpNodeKuwa_2`:
//! `m_Type` 2, `m_Strength` 0.9), then back to full size through the 3 x 3 one
//! (`ArkPpNodeKuwa_1`: `m_Strength` 2), the original's shaders as disassembled
//! (`kuwahara.wgsl`).

use bevy::asset::embedded_asset;
use bevy::core_pipeline::{schedule::Core3d, Core3dSystems, FullscreenShader};
use bevy::platform::collections::HashMap;
use bevy::prelude::*;
use bevy::render::camera::ExtractedCamera;
use bevy::render::extract_component::{ExtractComponent, ExtractComponentPlugin};
use bevy::render::render_resource::binding_types::{sampler, texture_2d, uniform_buffer};
use bevy::render::render_resource::{
    BindGroupEntries, BindGroupLayoutDescriptor, BindGroupLayoutEntries, CachedRenderPipelineId, ColorTargetState, ColorWrites, Extent3d, FilterMode,
    FragmentState, Operations, PipelineCache, RenderPassColorAttachment, RenderPassDescriptor, RenderPipelineDescriptor, Sampler, SamplerBindingType,
    SamplerDescriptor, ShaderStages, ShaderType, TextureDescriptor, TextureDimension, TextureFormat, TextureSampleType, TextureUsages, UniformBuffer,
};
use bevy::render::renderer::{RenderContext, RenderDevice, RenderQueue, ViewQuery};
use bevy::render::texture::{CachedTexture, TextureCache};
use bevy::render::view::{ExtractedView, ViewTarget};
use bevy::render::{Render, RenderApp, RenderStartup, RenderSystems};

pub struct KuwaharaPlugin;

impl Plugin for KuwaharaPlugin {
    fn build(&self, app: &mut App) {
        embedded_asset!(app, "kuwahara.wgsl");
        app.add_plugins(ExtractComponentPlugin::<ArkKuwa>::default()).init_resource::<KuwaSwitch>().add_systems(Update, kuwa_switch.run_if(in_state(crate::GameState::InGame)));
        let Some(render_app) = app.get_sub_app_mut(RenderApp) else { return };
        render_app
            .add_systems(RenderStartup, init_pipelines)
            .add_systems(Render, (prepare_pipelines, prepare_textures).in_set(RenderSystems::PrepareResources))
            .add_systems(
                Core3d,
                draw_kuwa
                    .in_set(Core3dSystems::PostProcess)
                    .after(bevy::anti_alias::fxaa::fxaa)
                    .after(bevy::anti_alias::smaa::smaa)
                    .after(bevy::core_pipeline::fullscreen_material::fullscreen_material_system::<crate::postfx::ArkGrade>),
            );
    }
}

/// `KuwaSwitch`: the painterly branch on.
#[derive(Resource, Default)]
pub struct KuwaSwitch(pub bool);

/// A camera drawn through the Kuwahara filters.
#[derive(Component, ExtractComponent, Clone, Copy, Default)]
pub struct ArkKuwa;

/// The nodes' strengths (`m_Strength`): the half-size 5 x 5 pass's, the full-size 3 x 3's.
const HALF_STRENGTH: f32 = 0.9;
const FULL_STRENGTH: f32 = 2.0;

fn kuwa_switch(
    mut commands: Commands,
    keys: Res<ButtonInput<KeyCode>>,
    mut switch: ResMut<KuwaSwitch>,
    mut init: Local<bool>,
    cams: Query<(Entity, Has<ArkKuwa>), With<crate::player::PlayerCamera>>,
) {
    if !*init {
        *init = true;
        switch.0 |= std::env::var("DH_KUWA").is_ok();
    }
    if keys.just_pressed(KeyCode::F4) {
        switch.0 = !switch.0;
        info!("KUWA: {}", if switch.0 { "on" } else { "off" });
    }
    for (e, has) in &cams {
        if switch.0 && !has {
            commands.entity(e).insert(ArkKuwa);
        } else if !switch.0 && has {
            commands.entity(e).remove::<ArkKuwa>();
        }
    }
}

/// The uniform: 1 / the render target's size, the strength.
#[derive(ShaderType, Clone, Copy)]
struct KuwaParams {
    texel: Vec4,
}

#[derive(Resource)]
struct KuwaPipelines {
    layout: BindGroupLayoutDescriptor,
    sampler: Sampler,
    shader: Handle<Shader>,
    fullscreen: FullscreenShader,
    /// (target format, pass) -> pipeline
    ids: HashMap<(TextureFormat, u8), CachedRenderPipelineId>,
}

/// The half-size buffers' format.
const LOW: TextureFormat = TextureFormat::Rgba16Float;
const PASSES: [&str; 3] = ["downsample", "kuwa5", "kuwa3"];

fn init_pipelines(mut commands: Commands, asset_server: Res<AssetServer>, device: Res<RenderDevice>, fullscreen: Res<FullscreenShader>) {
    let layout = BindGroupLayoutDescriptor::new(
        "kuwa_layout",
        &BindGroupLayoutEntries::sequential(ShaderStages::FRAGMENT, (texture_2d(TextureSampleType::Float { filterable: true }), sampler(SamplerBindingType::Filtering), uniform_buffer::<KuwaParams>(false))),
    );
    let sampler = device.create_sampler(&SamplerDescriptor { mag_filter: FilterMode::Linear, min_filter: FilterMode::Linear, ..default() });
    commands.insert_resource(KuwaPipelines { layout, sampler, shader: asset_server.load("embedded://dishonored/kuwahara.wgsl"), fullscreen: fullscreen.clone(), ids: HashMap::default() });
}

fn prepare_pipelines(mut pipelines: ResMut<KuwaPipelines>, pipeline_cache: Res<PipelineCache>, views: Query<&ExtractedView, With<ArkKuwa>>) {
    for view in &views {
        for pass in 0..3u8 {
            let format = if pass == 2 { view.target_format } else { LOW };
            let key = (format, pass);
            if pipelines.ids.contains_key(&key) {
                continue;
            }
            let desc = RenderPipelineDescriptor {
                label: Some(format!("kuwa_{}", PASSES[pass as usize]).into()),
                layout: vec![pipelines.layout.clone()],
                vertex: pipelines.fullscreen.to_vertex_state(),
                fragment: Some(FragmentState {
                    shader: pipelines.shader.clone(),
                    shader_defs: Vec::new(),
                    entry_point: Some(PASSES[pass as usize].into()),
                    targets: vec![Some(ColorTargetState { format, blend: None, write_mask: ColorWrites::ALL })],
                }),
                ..default()
            };
            let id = pipeline_cache.queue_render_pipeline(desc);
            pipelines.ids.insert(key, id);
        }
    }
}

/// A view's two half-size buffers.
#[derive(Component)]
struct KuwaTextures {
    a: CachedTexture,
    b: CachedTexture,
    half: UVec2,
    full: UVec2,
}

fn prepare_textures(mut commands: Commands, mut cache: ResMut<TextureCache>, device: Res<RenderDevice>, views: Query<(Entity, &ExtractedCamera), With<ArkKuwa>>) {
    for (e, camera) in &views {
        let Some(size) = camera.physical_target_size else { continue };
        let half = UVec2::new((size.x / 2).max(1), (size.y / 2).max(1));
        let mut tex = |label: &'static str| {
            cache.get(
                &device,
                TextureDescriptor {
                    label: Some(label),
                    size: Extent3d { width: half.x, height: half.y, depth_or_array_layers: 1 },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: TextureDimension::D2,
                    format: LOW,
                    usage: TextureUsages::RENDER_ATTACHMENT | TextureUsages::TEXTURE_BINDING,
                    view_formats: &[],
                },
            )
        };
        let (a, b) = (tex("kuwa_a"), tex("kuwa_b"));
        commands.entity(e).insert(KuwaTextures { a, b, half, full: size });
    }
}

fn draw_kuwa(
    view: ViewQuery<(&ExtractedView, &ViewTarget, &ArkKuwa, Option<&KuwaTextures>)>,
    pipelines: Res<KuwaPipelines>,
    pipeline_cache: Res<PipelineCache>,
    device: Res<RenderDevice>,
    queue: Res<RenderQueue>,
    mut ctx: RenderContext,
) {
    let (extracted, target, _, textures) = view.into_inner();
    let Some(textures) = textures else { return };
    let get = |pass: u8| pipelines.ids.get(&(if pass == 2 { extracted.target_format } else { LOW }, pass)).and_then(|id| pipeline_cache.get_render_pipeline(*id));
    let (Some(down), Some(k5), Some(k3)) = (get(0), get(1), get(2)) else { return };
    let params = |size: UVec2, strength: f32| {
        let mut u = UniformBuffer::from(KuwaParams { texel: Vec4::new(1.0 / size.x as f32, 1.0 / size.y as f32, strength, 0.0) });
        u.write_buffer(&device, &queue);
        u
    };
    let (half_p, full_p) = (params(textures.half, HALF_STRENGTH), params(textures.full, FULL_STRENGTH));
    let (Some(half_b), Some(full_b)) = (half_p.binding(), full_p.binding()) else { return };
    let layout = pipeline_cache.get_bind_group_layout(&pipelines.layout);
    let (a, b) = (&textures.a.default_view, &textures.b.default_view);
    let mut pass = |label: &'static str, pipeline, source, binding, dest| {
        let bg = device.create_bind_group(label, &layout, &BindGroupEntries::sequential((source, &pipelines.sampler, binding)));
        let mut p = ctx.begin_tracked_render_pass(RenderPassDescriptor {
            label: Some(label),
            color_attachments: &[Some(RenderPassColorAttachment { view: dest, depth_slice: None, resolve_target: None, ops: Operations::default() })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        p.set_render_pipeline(pipeline);
        p.set_bind_group(0, &bg, &[]);
        p.draw(0..3, 0..1);
    };
    // `KOKuwa`: half size; `KuwaHalfRes`; `KuwaFullRes` back onto the scene
    let post = target.post_process_write();
    pass("kuwa downsample", down, post.source, half_b.clone(), a);
    pass("kuwa half", k5, a, half_b, b);
    pass("kuwa full", k3, b, full_b, post.destination);
}
