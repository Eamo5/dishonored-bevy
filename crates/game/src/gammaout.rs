//! The finished frame put gamma-encoded, last of all (after the grade, its effects, the
//! anti-aliasing, the painterly branch): the player camera composites in sRGB
//! (`CompositingSpace::Srgb`, its frame HDR), so the interface blends over it in gamma space, as
//! Scaleform's does over UE3's finished, gamma-encoded frame (a 90% veil leaves a tenth of the
//! picture, not a third); the blit to the window decodes it (`gamma_out.wgsl`).

use bevy::asset::embedded_asset;
use bevy::core_pipeline::{schedule::Core3d, Core3dSystems, FullscreenShader};
use bevy::platform::collections::HashMap;
use bevy::prelude::*;
use bevy::render::extract_component::{ExtractComponent, ExtractComponentPlugin};
use bevy::render::render_resource::binding_types::texture_2d;
use bevy::render::render_resource::{
    BindGroupEntries, BindGroupLayoutDescriptor, BindGroupLayoutEntries, CachedRenderPipelineId, ColorTargetState, ColorWrites, FragmentState, Operations, PipelineCache,
    RenderPassColorAttachment, RenderPassDescriptor, RenderPipelineDescriptor, ShaderStages, TextureFormat, TextureSampleType,
};
use bevy::render::renderer::{RenderContext, RenderDevice, ViewQuery};
use bevy::render::view::{ExtractedView, ViewTarget};
use bevy::render::{Render, RenderApp, RenderStartup, RenderSystems};

pub struct GammaOutPlugin;

impl Plugin for GammaOutPlugin {
    fn build(&self, app: &mut App) {
        embedded_asset!(app, "gamma_out.wgsl");
        app.add_plugins(ExtractComponentPlugin::<GammaOut>::default());
        let Some(render_app) = app.get_sub_app_mut(RenderApp) else { return };
        render_app.add_systems(RenderStartup, init_pipelines).add_systems(Render, prepare_pipelines.in_set(RenderSystems::PrepareResources)).add_systems(
            Core3d,
            draw_gamma_out
                .in_set(Core3dSystems::PostProcess)
                .after(bevy::core_pipeline::fullscreen_material::fullscreen_material_system::<crate::postfx::ArkGrade>)
                .after(bevy::anti_alias::fxaa::fxaa)
                .after(bevy::anti_alias::smaa::smaa)
                .after(crate::kuwahara::KuwaPass),
        );
    }
}

/// A camera whose frame is put gamma-encoded at its end.
#[derive(Component, ExtractComponent, Clone, Copy, Default)]
pub struct GammaOut;

#[derive(Resource)]
struct GammaPipelines {
    layout: BindGroupLayoutDescriptor,
    shader: Handle<Shader>,
    fullscreen: FullscreenShader,
    ids: HashMap<TextureFormat, CachedRenderPipelineId>,
}

fn init_pipelines(mut commands: Commands, asset_server: Res<AssetServer>, fullscreen: Res<FullscreenShader>) {
    let layout = BindGroupLayoutDescriptor::new("gamma_out_layout", &BindGroupLayoutEntries::single(ShaderStages::FRAGMENT, texture_2d(TextureSampleType::Float { filterable: false })));
    commands.insert_resource(GammaPipelines { layout, shader: asset_server.load("embedded://dishonored/gamma_out.wgsl"), fullscreen: fullscreen.clone(), ids: HashMap::default() });
}

fn prepare_pipelines(mut pipelines: ResMut<GammaPipelines>, pipeline_cache: Res<PipelineCache>, views: Query<&ExtractedView, With<GammaOut>>) {
    for view in &views {
        let format = view.target_format;
        if pipelines.ids.contains_key(&format) {
            continue;
        }
        let desc = RenderPipelineDescriptor {
            label: Some("gamma_out".into()),
            layout: vec![pipelines.layout.clone()],
            vertex: pipelines.fullscreen.to_vertex_state(),
            fragment: Some(FragmentState {
                shader: pipelines.shader.clone(),
                shader_defs: Vec::new(),
                entry_point: Some("fragment".into()),
                targets: vec![Some(ColorTargetState { format, blend: None, write_mask: ColorWrites::ALL })],
            }),
            ..default()
        };
        let id = pipeline_cache.queue_render_pipeline(desc);
        pipelines.ids.insert(format, id);
    }
}

fn draw_gamma_out(view: ViewQuery<(&ExtractedView, &ViewTarget, &GammaOut)>, pipelines: Res<GammaPipelines>, pipeline_cache: Res<PipelineCache>, device: Res<RenderDevice>, mut ctx: RenderContext) {
    let (extracted, target, _) = view.into_inner();
    let Some(pipeline) = pipelines.ids.get(&extracted.target_format).and_then(|id| pipeline_cache.get_render_pipeline(*id)) else { return };
    let layout = pipeline_cache.get_bind_group_layout(&pipelines.layout);
    let post = target.post_process_write();
    let bg = device.create_bind_group("gamma_out", &layout, &BindGroupEntries::single(post.source));
    let mut p = ctx.begin_tracked_render_pass(RenderPassDescriptor {
        label: Some("gamma_out"),
        color_attachments: &[Some(RenderPassColorAttachment { view: post.destination, depth_slice: None, resolve_target: None, ops: Operations::default() })],
        depth_stencil_attachment: None,
        timestamp_writes: None,
        occlusion_query_set: None,
        multiview_mask: None,
    });
    p.set_render_pipeline(pipeline);
    p.set_bind_group(0, &bg, &[]);
    p.draw(0..3, 0..1);
}
