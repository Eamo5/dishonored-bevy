//! Arkane's depth of field as the original's post-processing applies it (`FArkPpDofDownsample`
//! then `FArkPpDofUber`): the scene colour is mixed with a quarter-resolution copy of itself by
//! how far beyond the focus each pixel lies, `saturate((depth - FocusDistance) / InFocusRadius)
//! x FarBlurAmount` (`gBlurParams`): the far blur of the level's settings (or the scripts'),
//! softening distant views and the sky. It runs on the linear scene colour just before the
//! colour grading (`postfx::ArkGrade`), as the original's uber shader blends before its LUT.

use bevy::asset::embedded_asset;
use bevy::core_pipeline::prepass::ViewPrepassTextures;
use bevy::core_pipeline::{schedule::Core3d, Core3dSystems, FullscreenShader};
use bevy::platform::collections::HashMap;
use bevy::prelude::*;
use bevy::render::camera::ExtractedCamera;
use bevy::render::extract_component::{ExtractComponent, ExtractComponentPlugin};
use bevy::render::render_resource::binding_types::{sampler, texture_2d, texture_depth_2d, texture_depth_2d_multisampled, uniform_buffer};
use bevy::render::render_resource::{
    BindGroupEntries, BindGroupLayoutDescriptor, BindGroupLayoutEntries, CachedRenderPipelineId, ColorTargetState, ColorWrites, Extent3d, FilterMode,
    FragmentState, Operations, PipelineCache, RenderPassColorAttachment, RenderPassDescriptor, RenderPipelineDescriptor, Sampler, SamplerBindingType,
    SamplerDescriptor, ShaderStages, ShaderType, TextureDescriptor, TextureDimension, TextureFormat, TextureSampleType, TextureUsages, UniformBuffer,
};
use bevy::render::renderer::{RenderContext, RenderDevice, RenderQueue, ViewQuery};
use bevy::render::texture::{CachedTexture, TextureCache};
use bevy::render::view::{ExtractedView, Msaa, ViewTarget};
use bevy::render::{Render, RenderApp, RenderStartup, RenderSystems};

pub struct DofPlugin;

impl Plugin for DofPlugin {
    fn build(&self, app: &mut App) {
        embedded_asset!(app, "dof.wgsl");
        app.add_plugins(ExtractComponentPlugin::<ArkDof>::default())
            .add_systems(Update, update_dof.run_if(in_state(crate::GameState::InGame)));
        let Some(render_app) = app.get_sub_app_mut(RenderApp) else { return };
        render_app
            .add_systems(RenderStartup, init_pipelines)
            .add_systems(Render, (prepare_pipelines, prepare_textures).in_set(RenderSystems::PrepareResources))
            .add_systems(
                Core3d,
                draw_dof
                    .in_set(Core3dSystems::PostProcess)
                    .after(bevy::core_pipeline::tonemapping::tonemapping)
                    .before(bevy::core_pipeline::fullscreen_material::fullscreen_material_system::<crate::postfx::ArkGrade>)
                    .before(bevy::anti_alias::fxaa::fxaa).before(bevy::anti_alias::smaa::smaa),
            );
    }
}

/// A camera's depth of field: focus distance, 1 / in-focus radius (metres), far blur amount,
/// the camera's near plane.
#[derive(Component, ExtractComponent, Clone, Copy, Default, ShaderType)]
pub struct ArkDof {
    pub params: Vec4,
}

/// The level's (or the scripts') settings onto the player's camera.
fn update_dof(
    mut commands: Commands,
    level: Option<Res<crate::level::LevelInfo>>,
    script: Res<crate::postfx::ScriptPost>,
    (data, blur): (Res<crate::gamedata::Data>, Res<crate::postfx::MenuBlur>),
    cams: Query<(Entity, &Projection, Option<&ArkDof>), With<crate::player::PlayerCamera>>,
) {
    let Some(level) = level else { return };
    if std::env::var("DH_NO_DOF").is_ok() {
        return;
    }
    let mut p = script.0.as_ref().map(|s| s.1).unwrap_or(level.scene.post);
    // the menus' blur over it, as far as it has faded in (or out)
    if blur.0 > 0.0 {
        let mut q = p;
        let fields = crate::postfx::uber_fields(&data.0.ui_blur, "m_Parameters.");
        crate::postfx::apply_fields(&mut q, fields.iter().map(|(n, v)| (n.as_str(), v.as_slice())));
        let w = blur.0;
        p.focus_distance += (q.focus_distance - p.focus_distance) * w;
        p.in_focus_radius += (q.in_focus_radius - p.in_focus_radius) * w;
        p.far_blur += (q.far_blur - p.far_blur) * w;
    }
    for (e, proj, dof) in &cams {
        let near = match proj {
            Projection::Perspective(p) => p.near,
            _ => 0.1,
        };
        let params = Vec4::new(p.focus_distance, 1.0 / p.in_focus_radius.max(0.01), p.far_blur, near);
        if p.far_blur <= 0.0 {
            if dof.is_some() {
                commands.entity(e).remove::<ArkDof>();
            }
            continue;
        }
        if dof.map(|d| d.params) != Some(params) {
            commands.entity(e).insert(ArkDof { params });
        }
    }
}

#[derive(Resource)]
pub(crate) struct DofPipelines {
    down_layout: BindGroupLayoutDescriptor,
    dof_layout: [BindGroupLayoutDescriptor; 2],
    sampler: Sampler,
    shader: Handle<Shader>,
    fullscreen: FullscreenShader,
    /// (target format, multisampled depth, downsample) -> pipeline
    ids: HashMap<(TextureFormat, bool, bool), CachedRenderPipelineId>,
}

fn init_pipelines(mut commands: Commands, asset_server: Res<AssetServer>, device: Res<RenderDevice>, fullscreen: Res<FullscreenShader>) {
    let down_layout = BindGroupLayoutDescriptor::new(
        "dof_down_layout",
        &BindGroupLayoutEntries::with_indices(ShaderStages::FRAGMENT, ((0, texture_2d(TextureSampleType::Float { filterable: true })), (2, sampler(SamplerBindingType::Filtering)))),
    );
    let dof = |ms: bool| {
        if ms {
            BindGroupLayoutDescriptor::new(
                "dof_layout_ms",
                &BindGroupLayoutEntries::sequential(
                    ShaderStages::FRAGMENT,
                    (
                        texture_2d(TextureSampleType::Float { filterable: true }),
                        texture_2d(TextureSampleType::Float { filterable: true }),
                        sampler(SamplerBindingType::Filtering),
                        texture_depth_2d_multisampled(),
                        uniform_buffer::<ArkDof>(false),
                    ),
                ),
            )
        } else {
            BindGroupLayoutDescriptor::new(
                "dof_layout",
                &BindGroupLayoutEntries::sequential(
                    ShaderStages::FRAGMENT,
                    (
                        texture_2d(TextureSampleType::Float { filterable: true }),
                        texture_2d(TextureSampleType::Float { filterable: true }),
                        sampler(SamplerBindingType::Filtering),
                        texture_depth_2d(),
                        uniform_buffer::<ArkDof>(false),
                    ),
                ),
            )
        }
    };
    let sampler = device.create_sampler(&SamplerDescriptor { mag_filter: FilterMode::Linear, min_filter: FilterMode::Linear, ..default() });
    commands.insert_resource(DofPipelines {
        down_layout,
        dof_layout: [dof(false), dof(true)],
        sampler,
        shader: asset_server.load("embedded://dishonored/dof.wgsl"),
        fullscreen: fullscreen.clone(),
        ids: HashMap::default(),
    });
}

fn prepare_pipelines(mut pipelines: ResMut<DofPipelines>, pipeline_cache: Res<PipelineCache>, views: Query<(&ExtractedView, &Msaa), With<ArkDof>>) {
    for (view, msaa) in &views {
        let ms = msaa.samples() > 1;
        for down in [true, false] {
            let format = view.target_format;
            let key = (format, ms, down);
            if pipelines.ids.contains_key(&key) {
                continue;
            }
            let layout = if down { pipelines.down_layout.clone() } else { pipelines.dof_layout[ms as usize].clone() };
            let mut defs = Vec::new();
            if ms {
                defs.push("MULTISAMPLED".into());
            }
            let desc = RenderPipelineDescriptor {
                label: Some(if down { "dof_downsample" } else { "dof" }.into()),
                layout: vec![layout],
                vertex: pipelines.fullscreen.to_vertex_state(),
                fragment: Some(FragmentState {
                    shader: pipelines.shader.clone(),
                    shader_defs: defs,
                    entry_point: Some(if down { "downsample" } else { "dof" }.into()),
                    targets: vec![Some(ColorTargetState { format, blend: None, write_mask: ColorWrites::ALL })],
                }),
                ..default()
            };
            let id = pipeline_cache.queue_render_pipeline(desc);
            pipelines.ids.insert(key, id);
        }
    }
}

/// A view's quarter-resolution copy of the scene.
#[derive(Component)]
pub(crate) struct DofTextures {
    low: CachedTexture,
}

fn prepare_textures(mut commands: Commands, mut cache: ResMut<TextureCache>, device: Res<RenderDevice>, views: Query<(Entity, &ExtractedCamera, &ExtractedView), With<ArkDof>>) {
    for (e, camera, view) in &views {
        let Some(size) = camera.physical_target_size else { continue };
        let low = cache.get(
            &device,
            TextureDescriptor {
                label: Some("dof_low"),
                size: Extent3d { width: (size.x / 4).max(1), height: (size.y / 4).max(1), depth_or_array_layers: 1 },
                mip_level_count: 1,
                sample_count: 1,
                dimension: TextureDimension::D2,
                format: view.target_format,
                usage: TextureUsages::RENDER_ATTACHMENT | TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            },
        );
        commands.entity(e).insert(DofTextures { low });
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn draw_dof(
    view: ViewQuery<(&ExtractedView, &ViewTarget, &ViewPrepassTextures, &ArkDof, &Msaa, Option<&DofTextures>)>,
    pipelines: Res<DofPipelines>,
    pipeline_cache: Res<PipelineCache>,
    device: Res<RenderDevice>,
    queue: Res<RenderQueue>,
    mut ctx: RenderContext,
) {
    let (extracted, target, prepass, dof, msaa, textures) = view.into_inner();
    let Some(textures) = textures else { return };
    let Some(depth) = prepass.depth_view() else { return };
    let ms = msaa.samples() > 1;
    let get = |down: bool| pipelines.ids.get(&(extracted.target_format, ms, down)).and_then(|id| pipeline_cache.get_render_pipeline(*id));
    let (Some(down_pipeline), Some(dof_pipeline)) = (get(true), get(false)) else { return };
    let mut uniform = UniformBuffer::from(*dof);
    uniform.write_buffer(&device, &queue);
    let Some(binding) = uniform.binding() else { return };
    // the scene, a quarter the size
    {
        let bg = device.create_bind_group("dof_down", &pipeline_cache.get_bind_group_layout(&pipelines.down_layout), &BindGroupEntries::with_indices(((0, target.main_texture_view()), (2, &pipelines.sampler))));
        let mut pass = ctx.begin_tracked_render_pass(RenderPassDescriptor {
            label: Some("dof downsample"),
            color_attachments: &[Some(RenderPassColorAttachment { view: &textures.low.default_view, depth_slice: None, resolve_target: None, ops: Operations::default() })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_render_pipeline(down_pipeline);
        pass.set_bind_group(0, &bg, &[]);
        pass.draw(0..3, 0..1);
    }
    // mixed in by distance
    let post = target.post_process_write();
    let bg = device.create_bind_group(
        "dof",
        &pipeline_cache.get_bind_group_layout(&pipelines.dof_layout[ms as usize]),
        &BindGroupEntries::sequential((post.source, &textures.low.default_view, &pipelines.sampler, depth, binding)),
    );
    let mut pass = ctx.begin_tracked_render_pass(RenderPassDescriptor {
        label: Some("dof"),
        color_attachments: &[Some(RenderPassColorAttachment { view: post.destination, depth_slice: None, resolve_target: None, ops: Operations::default() })],
        depth_stencil_attachment: None,
        timestamp_writes: None,
        occlusion_query_set: None,
        multiview_mask: None,
    });
    pass.set_render_pipeline(dof_pipeline);
    pass.set_bind_group(0, &bg, &[]);
    pass.draw(0..3, 0..1);
}
