//! UE3's light shafts (`LightComponent.bRenderLightShafts`: a few maps' sun, otherwise unlit,
//! sends them): a half-size pass picks out the occlusion (near things against the far) and the
//! bloom (the bright and far near the light's place on the screen), two radial blurs draw both
//! out towards the light, and the scene is darkened where the shafts are occluded and lit with
//! the tinted bloom (`lightshafts.wgsl`, the original's shaders as disassembled). It runs on the
//! scene colour before the depth of field and the colour grading, as the original renders its
//! shafts ahead of its post-processing, and the options turn it off
//! (`PSI_GraphicsPC_LightShaftEnable`).

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

pub struct LightShaftsPlugin;

impl Plugin for LightShaftsPlugin {
    fn build(&self, app: &mut App) {
        embedded_asset!(app, "lightshafts.wgsl");
        app.add_plugins(ExtractComponentPlugin::<ArkShafts>::default())
            .add_systems(PostUpdate, update_shafts.after(bevy::transform::TransformSystems::Propagate).run_if(in_state(crate::GameState::InGame)));
        let Some(render_app) = app.get_sub_app_mut(RenderApp) else { return };
        render_app
            .add_systems(RenderStartup, init_pipelines)
            .add_systems(Render, (prepare_pipelines, prepare_textures).in_set(RenderSystems::PrepareResources))
            .add_systems(
                Core3d,
                draw_shafts
                    .in_set(Core3dSystems::PostProcess)
                    .after(bevy::core_pipeline::tonemapping::tonemapping)
                    .before(crate::dof::draw_dof)
                    .before(bevy::core_pipeline::fullscreen_material::fullscreen_material_system::<crate::postfx::ArkGrade>)
                    .before(bevy::anti_alias::fxaa::fxaa)
                    .before(bevy::anti_alias::smaa::smaa),
            );
    }
}

/// A camera's light shafts, as the shaders take them (`lightshafts.wgsl`'s `Shafts`).
#[derive(Component, ExtractComponent, Clone, Copy, Default, ShaderType, PartialEq)]
pub struct ArkShafts {
    pub origin: Vec4,
    pub aspect: Vec4,
    pub params: Vec4,
    pub tint: Vec4,
    pub extra: Vec4,
}

/// The level's shaft-casting light onto the player's camera: where it stands on the screen, and
/// its settings; none while the option is off, or the light lies behind.
fn update_shafts(
    mut commands: Commands,
    level: Option<Res<crate::level::LevelInfo>>,
    settings: Res<crate::settings::Settings>,
    window: Query<&Window, With<bevy::window::PrimaryWindow>>,
    cams: Query<(Entity, &GlobalTransform, &Projection, Option<&ArkShafts>), With<crate::player::PlayerCamera>>,
) {
    // (the sun's first; else a point light's, from where it stands)
    let light = level.as_ref().and_then(|l| {
        let shafts = || l.scene.lights.iter().filter(|l| l.shafts.is_some() && l.enabled);
        shafts().find(|l| l.kind == dhcook::format::LightKind::Directional).or_else(|| shafts().next())
    });
    for (e, g, proj, cur) in &cams {
        let (Some(light), Some(sh), true, Ok(w), Projection::Perspective(pp)) = (light, light.and_then(|l| l.shafts.as_ref()), settings.light_shafts, window.single(), proj) else {
            if cur.is_some() {
                commands.entity(e).remove::<ArkShafts>();
            }
            continue;
        };
        // the light: far off along the way the sun's light comes from, or where it stands
        let at = if light.kind == dhcook::format::LightKind::Directional {
            g.translation() - Vec3::from(light.direction).normalize_or(Vec3::NEG_Y) * 1000.0
        } else {
            Vec3::from(light.position)
        };
        let to_light = (at - g.translation()).normalize_or(Vec3::Y);
        let fwd = g.forward().as_vec3();
        let facing = fwd.dot(to_light);
        let local = g.affine().inverse().transform_point3(at);
        let (sw, shh) = (w.physical_width().max(1) as f32, w.physical_height().max(1) as f32);
        let aspect = sw / shh;
        let t = (pp.fov * 0.5).tan();
        let depth = (-local.z).max(1e-3);
        let ndc = Vec2::new(local.x / depth / (t * aspect), local.y / depth / t);
        let uv = Vec2::new(ndc.x * 0.5 + 0.5, 0.5 - ndc.y * 0.5);
        // (gone as it comes round behind)
        let fade = 1.0 - ((facing - 0.0) / 0.15).clamp(0.0, 1.0);
        if fade >= 1.0 {
            if cur.is_some() {
                commands.entity(e).remove::<ArkShafts>();
            }
            continue;
        }
        let inv = Vec2::new(1.0, shh / sw);
        let s = ArkShafts {
            origin: Vec4::new(uv.x * inv.x, uv.y * inv.y, fade, 0.0),
            aspect: Vec4::new(1.0, sw / shh, 1.0, shh / sw),
            params: Vec4::new(1.0 / sh.occlusion_range.max(1.0), sh.bloom_scale, 1.0, sh.darkness),
            tint: Vec4::new(sh.tint[0], sh.tint[1], sh.tint[2], sh.bloom_threshold),
            extra: Vec4::new(sh.screen_blend_threshold, pp.near, sh.radial_blur.clamp(0.0, 1.0), 0.0),
        };
        if cur != Some(&s) {
            commands.entity(e).insert(s);
        }
    }
}

#[derive(Resource)]
struct ShaftPipelines {
    layout: [BindGroupLayoutDescriptor; 2],
    sampler: Sampler,
    shader: Handle<Shader>,
    fullscreen: FullscreenShader,
    /// (target format, multisampled depth, pass) -> pipeline
    ids: HashMap<(TextureFormat, bool, u8), CachedRenderPipelineId>,
}

/// The half-size buffers' format.
const LOW: TextureFormat = TextureFormat::Rgba16Float;
const PASSES: [&str; 3] = ["downsample", "blur", "apply"];

fn init_pipelines(mut commands: Commands, asset_server: Res<AssetServer>, device: Res<RenderDevice>, fullscreen: Res<FullscreenShader>) {
    let layout = |ms: bool| {
        let tex = || texture_2d(TextureSampleType::Float { filterable: true });
        if ms {
            BindGroupLayoutDescriptor::new(
                "shafts_layout_ms",
                &BindGroupLayoutEntries::sequential(ShaderStages::FRAGMENT, (tex(), sampler(SamplerBindingType::Filtering), uniform_buffer::<ArkShafts>(false), tex(), texture_depth_2d_multisampled())),
            )
        } else {
            BindGroupLayoutDescriptor::new(
                "shafts_layout",
                &BindGroupLayoutEntries::sequential(ShaderStages::FRAGMENT, (tex(), sampler(SamplerBindingType::Filtering), uniform_buffer::<ArkShafts>(false), tex(), texture_depth_2d())),
            )
        }
    };
    let sampler = device.create_sampler(&SamplerDescriptor { mag_filter: FilterMode::Linear, min_filter: FilterMode::Linear, ..default() });
    commands.insert_resource(ShaftPipelines { layout: [layout(false), layout(true)], sampler, shader: asset_server.load("embedded://dishonored/lightshafts.wgsl"), fullscreen: fullscreen.clone(), ids: HashMap::default() });
}

fn prepare_pipelines(mut pipelines: ResMut<ShaftPipelines>, pipeline_cache: Res<PipelineCache>, views: Query<(&ExtractedView, &Msaa), With<ArkShafts>>) {
    for (view, msaa) in &views {
        let ms = msaa.samples() > 1;
        for pass in 0..3u8 {
            let format = if pass == 2 { view.target_format } else { LOW };
            let key = (format, ms, pass);
            if pipelines.ids.contains_key(&key) {
                continue;
            }
            let mut defs = Vec::new();
            if ms {
                defs.push("MULTISAMPLED".into());
            }
            let desc = RenderPipelineDescriptor {
                label: Some(format!("shafts_{}", PASSES[pass as usize]).into()),
                layout: vec![pipelines.layout[ms as usize].clone()],
                vertex: pipelines.fullscreen.to_vertex_state(),
                fragment: Some(FragmentState {
                    shader: pipelines.shader.clone(),
                    shader_defs: defs,
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

/// A view's two half-size buffers (the blur goes back and forth between them).
#[derive(Component)]
struct ShaftTextures {
    a: CachedTexture,
    b: CachedTexture,
}

fn prepare_textures(mut commands: Commands, mut cache: ResMut<TextureCache>, device: Res<RenderDevice>, views: Query<(Entity, &ExtractedCamera), With<ArkShafts>>) {
    for (e, camera) in &views {
        let Some(size) = camera.physical_target_size else { continue };
        let mut tex = |label: &'static str| {
            cache.get(
                &device,
                TextureDescriptor {
                    label: Some(label),
                    size: Extent3d { width: (size.x / 2).max(1), height: (size.y / 2).max(1), depth_or_array_layers: 1 },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: TextureDimension::D2,
                    format: LOW,
                    usage: TextureUsages::RENDER_ATTACHMENT | TextureUsages::TEXTURE_BINDING,
                    view_formats: &[],
                },
            )
        };
        let (a, b) = (tex("shafts_a"), tex("shafts_b"));
        commands.entity(e).insert(ShaftTextures { a, b });
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_shafts(
    view: ViewQuery<(&ExtractedView, &ViewTarget, &ViewPrepassTextures, &ArkShafts, &Msaa, Option<&ShaftTextures>)>,
    pipelines: Res<ShaftPipelines>,
    pipeline_cache: Res<PipelineCache>,
    device: Res<RenderDevice>,
    queue: Res<RenderQueue>,
    mut ctx: RenderContext,
) {
    let (extracted, target, prepass, shafts, msaa, textures) = view.into_inner();
    let Some(textures) = textures else { return };
    let Some(depth) = prepass.depth_view() else { return };
    let ms = msaa.samples() > 1;
    let get = |pass: u8| pipelines.ids.get(&(if pass == 2 { extracted.target_format } else { LOW }, ms, pass)).and_then(|id| pipeline_cache.get_render_pipeline(*id));
    let (Some(down), Some(blur), Some(apply)) = (get(0), get(1), get(2)) else { return };
    let mut uniform = UniformBuffer::from(*shafts);
    uniform.write_buffer(&device, &queue);
    let Some(binding) = uniform.binding() else { return };
    let layout = pipeline_cache.get_bind_group_layout(&pipelines.layout[ms as usize]);
    let (a, b) = (&textures.a.default_view, &textures.b.default_view);
    let mut pass = |label: &'static str, pipeline, source, extra, dest| {
        let bg = device.create_bind_group(label, &layout, &BindGroupEntries::sequential((source, &pipelines.sampler, binding.clone(), extra, depth)));
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
    // the occlusion and bloom, half-size; drawn out twice towards the light
    pass("shafts downsample", down, target.main_texture_view(), b, a);
    pass("shafts blur 1", blur, a, a, b);
    pass("shafts blur 2", blur, b, b, a);
    // onto the scene
    let post = target.post_process_write();
    pass("shafts apply", apply, post.source, a, post.destination);
}
