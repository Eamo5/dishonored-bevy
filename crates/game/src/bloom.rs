//! Bloom parts as the original renders them (its "BloomParts" pass, 0x009251A0: "Draw blooming
//! primitives", "Bloom reduction and blur", "Bloom compose"; decoded for this rewrite from the
//! user's own Dishonored.exe and its shader caches, after roshan's r614/blink research first
//! described it).
//!
//! The original blooms no bright scene: only surfaces whose material blooms
//! (`MaterialInterface.bHasBloomPart`, cooked as `Ue3Material::bloom`) are drawn a second time
//! with their shader map's `TBloomPartMesh` shaders. Those give what of them blooms (Arkane's
//! `BloomColor` material input: for the generic materials `E_Emissive x E_Emissive_Color x
//! B_Bloom_Intensity`), LogLuv-encoded, into a target of the scene's size, depth-tested
//! against the scene. That is reduced to a quarter of the size (`FBloomDownSamplePixelShader`:
//! four bilinear taps a source texel off the middle, each decoded, averaged, times the level's
//! `m_Tint x m_Scale`, less `m_Threshold x m_Scale`), blurred along x then y
//! (`TBloomBlurPixelShader<5>`: taps at 0, +-1.3973 and +-3.3973 texels), and added to the
//! scene after its fog (the fog pass's `sBloomParts`, or `FBloomComposePixelShader`), before
//! the colour grading. The material shaders run translated (`Policy::BloomPart`) in a phase of
//! their own: Bevy's mesh pipeline with the material's bind group. The reduction, blur and
//! compose are the original's shaders by hand (`bloom.wgsl`). `DH_NO_BLOOM=1` turns it off;
//! `DH_BLOOM_DEBUG=GAIN` shows the bloom alone (times GAIN), `DH_BLOOM_LOG` what it draws.

use crate::postfx::ArkGrade;
use crate::ue3mat::{Ue3Material, Ue3Programs};
use bevy::asset::embedded_asset;
use bevy::camera::Viewport;
use bevy::core_pipeline::core_3d::{TransparentSortingInfo3d, CORE_3D_DEPTH_FORMAT};
use bevy::core_pipeline::{schedule::Core3d, Core3dSystems, FullscreenShader};
use bevy::ecs::entity::EntityHash;
use bevy::ecs::system::{lifetimeless::SRes, SystemParamItem};
use bevy::math::FloatOrd;
use bevy::mesh::MeshVertexBufferLayoutRef;
use bevy::pbr::{
    DrawMesh, MeshInputUniform, MeshPipeline, MeshPipelineKey, MeshPipelineSystems, MeshUniform, RenderMeshInstances, SetMaterialBindGroup,
    SetMeshBindGroup, SetMeshViewBindGroup, SetMeshViewBindingArrayBindGroup, ViewKeyCache,
};
use bevy::platform::collections::{HashMap, HashSet};
use bevy::prelude::*;
use bevy::render::batching::gpu_preprocessing::{batch_and_prepare_sorted_render_phase, BatchedInstanceBuffers, IndirectParametersCpuMetadata, UntypedPhaseIndirectParametersBuffers};
use bevy::render::batching::{GetBatchData, GetFullBatchData};
use bevy::render::camera::ExtractedCamera;
use bevy::render::extract_component::{ExtractComponent, ExtractComponentPlugin};
use bevy::render::extract_resource::{ExtractResource, ExtractResourcePlugin};
use bevy::render::mesh::{allocator::MeshAllocator, RenderMesh};
use bevy::render::render_asset::RenderAssets;
use bevy::render::render_phase::{
    sort_phase_system, AddRenderCommand, CachedRenderPipelinePhaseItem, DrawFunctionId, DrawFunctions, PhaseItem, PhaseItemExtraIndex, SetItemPipeline,
    SortedPhaseItem, SortedRenderPhasePlugin, ViewSortedRenderPhases,
};
use bevy::render::render_resource::binding_types::{sampler, texture_2d, uniform_buffer};
use bevy::render::render_resource::{
    AsBindGroup, BindGroupEntries, BindGroupLayoutDescriptor, BindGroupLayoutEntries, BlendState, Buffer, CachedRenderPipelineId,
    ColorTargetState, ColorWrites, CompareFunction, Extent3d, Face, FilterMode, FragmentState, LoadOp, Operations, PipelineCache,
    RenderPassColorAttachment, RenderPassDescriptor, RenderPipelineDescriptor, Sampler, SamplerBindingType, SamplerDescriptor, ShaderStages, ShaderType,
    SpecializedMeshPipeline, SpecializedMeshPipelineError, SpecializedMeshPipelines, StoreOp, TextureDescriptor, TextureDimension, TextureFormat,
    TextureSampleType, TextureUsages, UniformBuffer,
};
use bevy::render::renderer::{RenderContext, RenderDevice, RenderQueue, ViewQuery};
use bevy::render::sync_world::MainEntity;
use bevy::render::texture::{CachedTexture, TextureCache};
use bevy::render::view::{ExtractedView, Msaa, RetainedViewEntity, ViewDepthTexture, ViewTarget};
use bevy::render::{Extract, Render, RenderApp, RenderDebugFlags, RenderStartup, RenderSystems};
use bevy::shader::ShaderDefVal;
use dhcook::ue3prog::Policy;
use indexmap::IndexMap;
use nonmax::NonMaxU32;
use std::ops::Range;

pub struct BloomPlugin;

impl Plugin for BloomPlugin {
    fn build(&self, app: &mut App) {
        embedded_asset!(app, "bloom.wgsl");
        app.init_resource::<BloomSettings>()
            .add_plugins((
                ExtractComponentPlugin::<BloomPart>::default(),
                ExtractResourcePlugin::<BloomSettings>::default(),
                SortedRenderPhasePlugin::<BloomPart3d, MeshPipeline>::new(RenderDebugFlags::default()),
            ))
            .add_systems(PostUpdate, (mark_bloom_parts, bloom_settings));
        let Some(render_app) = app.get_sub_app_mut(RenderApp) else { return };
        render_app
            .init_resource::<SpecializedMeshPipelines<BloomPipeline>>()
            .init_resource::<DrawFunctions<BloomPart3d>>()
            .add_render_command::<BloomPart3d, DrawBloomPart>()
            .init_resource::<ViewSortedRenderPhases<BloomPart3d>>()
            .add_systems(RenderStartup, init_pipelines.after(MeshPipelineSystems))
            .add_systems(ExtractSchedule, extract_bloom_phases)
            .add_systems(
                Render,
                (
                    (prepare_post_pipelines, prepare_bloom_textures).in_set(RenderSystems::PrepareResources),
                    queue_bloom_parts.in_set(RenderSystems::QueueMeshes),
                    sort_phase_system::<BloomPart3d>.in_set(RenderSystems::PhaseSort),
                    batch_and_prepare_sorted_render_phase::<BloomPart3d, BloomPipeline>.in_set(RenderSystems::PrepareResources),
                ),
            )
            // in the original's order: after the fog (drawn with the scene) and the light shafts,
            // before the depth of field and the colour grading (`postfx::ArkGrade`)
            .add_systems(
                Core3d,
                draw_bloom
                    .in_set(Core3dSystems::PostProcess)
                    .after(bevy::core_pipeline::tonemapping::tonemapping)
                    .after(crate::lightshafts::draw_shafts)
                    .before(crate::dof::draw_dof)
                    .before(bevy::core_pipeline::fullscreen_material::fullscreen_material_system::<ArkGrade>),
            );
    }
}

/// A surface whose material blooms: its bloom program (`Policy::BloomPart`) and how its
/// material draws it.
#[derive(Component, Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct BloomPart {
    program: u32,
    two_sided: bool,
    foreground: bool,
    foreground_world: bool,
}

impl bevy::render::sync_component::SyncComponent for BloomPart {
    type Target = BloomPart;
}

/// (in the render world while the surface is in view: what the views' culling left visible)
impl ExtractComponent for BloomPart {
    type QueryData = (&'static BloomPart, &'static ViewVisibility);
    type QueryFilter = ();
    type Out = BloomPart;

    fn extract_component((part, seen): bevy::ecs::query::QueryItem<'_, '_, Self::QueryData>) -> Option<BloomPart> {
        seen.get().then_some(*part)
    }
}

/// The level's bloom (`ArkPpBloomParameters`): on, and its tint (`m_Tint x m_Scale`,
/// `-m_Threshold x m_Scale`).
#[derive(Resource, ExtractResource, Clone, Copy, Default)]
pub struct BloomSettings {
    on: bool,
    tint: Vec4,
}

fn bloom_settings(level: Option<Res<crate::level::LevelInfo>>, mut settings: ResMut<BloomSettings>) {
    let off = std::env::var("DH_NO_BLOOM").is_ok_and(|v| v != "0");
    let want = match level.as_ref() {
        Some(l) if !off && l.scene.post.bloom => {
            let p = &l.scene.post;
            BloomSettings { on: true, tint: (Vec3::from(p.bloom_tint) * p.bloom_scale).extend(-p.bloom_threshold * p.bloom_scale) }
        }
        _ => BloomSettings::default(),
    };
    if settings.on != want.on || settings.tint != want.tint {
        *settings = want;
    }
}

/// The surfaces drawn with a blooming material take their bloom part (and lose it with
/// another material); a light pass's copy of the material doesn't.
#[allow(clippy::type_complexity)]
fn mark_bloom_parts(
    mut commands: Commands,
    level: Option<Res<crate::level::LevelInfo>>,
    materials: Res<Assets<Ue3Material>>,
    mut events: MessageReader<AssetEvent<Ue3Material>>,
    (mut programs, mut shaders): (ResMut<Ue3Programs>, ResMut<Assets<Shader>>),
    changed: Query<(Entity, &MeshMaterial3d<Ue3Material>, Option<&BloomPart>), Changed<MeshMaterial3d<Ue3Material>>>,
    surfaces: Query<(Entity, &MeshMaterial3d<Ue3Material>, Option<&BloomPart>)>,
) {
    let Some(level) = level else { return };
    // (a material changed in place: its key may have)
    let modified: HashSet<AssetId<Ue3Material>> = events.read().filter_map(|e| if let AssetEvent::Modified { id } = e { Some(*id) } else { None }).collect();
    let again = surfaces.iter().filter(|(_, m, _)| !modified.is_empty() && modified.contains(&m.0.id()));
    for (e, mat, had) in changed.iter().chain(again) {
        let want = materials.get(&mat.0).filter(|m| !m.key.light_pass).and_then(|m| {
            let ue = level.scene.materials.get(m.mat_id as usize)?.ue3.as_ref()?;
            if !ue.bloom || ue.map != m.map {
                return None;
            }
            let program = programs.get(m.map, Policy::BloomPart, false, &mut shaders)?;
            Some(BloomPart { program, two_sided: m.key.two_sided, foreground: m.key.foreground, foreground_world: m.key.foreground_world })
        });
        match (want, had) {
            (Some(b), h) if h != Some(&b) => {
                if std::env::var("DH_BLOOM_LOG").is_ok() {
                    info!("bloom: part {e} program {}", b.program);
                }
                commands.entity(e).try_insert(b);
            }
            (None, Some(_)) => {
                commands.entity(e).try_remove::<BloomPart>();
            }
            _ => {}
        }
    }
}

/// The bloom parts' scene-size target (multisampled as the scene is, and resolved), and the
/// two quarter-size ones the reduction and the blur take turns on.
const PARTS_FORMAT: TextureFormat = TextureFormat::Rgba8Unorm;
const QUARTER_FORMAT: TextureFormat = TextureFormat::Rgba16Float;

#[derive(Resource)]
struct BloomPipeline {
    mesh_pipeline: MeshPipeline,
    material_layout: BindGroupLayoutDescriptor,
}

#[derive(Resource)]
struct PostPipelines {
    layout: BindGroupLayoutDescriptor,
    sampler: Sampler,
    shader: Handle<Shader>,
    fullscreen: FullscreenShader,
    /// the reduction and the blur (quarter size), and the compose by the view's format
    downsample: Option<CachedRenderPipelineId>,
    blur: Option<CachedRenderPipelineId>,
    compose: HashMap<TextureFormat, CachedRenderPipelineId>,
}

#[derive(Clone, Copy, ShaderType, Default)]
struct BloomUniform {
    /// the level's tint (rgb) and threshold (w), as the reduction takes them
    tint: Vec4,
    /// the source's texel (xy) and the blur's direction (zw)
    texel: Vec4,
    /// (debugging, `DH_BLOOM_DEBUG=GAIN`: the bloom alone, times GAIN)
    debug: Vec4,
}

fn init_pipelines(mut commands: Commands, mesh_pipeline: Res<MeshPipeline>, asset_server: Res<AssetServer>, device: Res<RenderDevice>, fullscreen: Res<FullscreenShader>) {
    commands.insert_resource(BloomPipeline { mesh_pipeline: mesh_pipeline.clone(), material_layout: Ue3Material::bind_group_layout_descriptor(&device) });
    let layout = BindGroupLayoutDescriptor::new(
        "bloom_post_layout",
        &BindGroupLayoutEntries::sequential(
            ShaderStages::FRAGMENT,
            (
                texture_2d(TextureSampleType::Float { filterable: true }),
                texture_2d(TextureSampleType::Float { filterable: true }),
                sampler(SamplerBindingType::Filtering),
                uniform_buffer::<BloomUniform>(false),
            ),
        ),
    );
    let linear = device.create_sampler(&SamplerDescriptor { mag_filter: FilterMode::Linear, min_filter: FilterMode::Linear, ..default() });
    commands.insert_resource(PostPipelines {
        layout,
        sampler: linear,
        shader: asset_server.load("embedded://dishonored/bloom.wgsl"),
        fullscreen: fullscreen.clone(),
        downsample: None,
        blur: None,
        compose: HashMap::default(),
    });
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct BloomKey {
    mesh: MeshPipelineKey,
    part: BloomPart,
}

impl SpecializedMeshPipeline for BloomPipeline {
    type Key = BloomKey;

    fn specialize(&self, key: Self::Key, layout: &MeshVertexBufferLayoutRef) -> Result<RenderPipelineDescriptor, SpecializedMeshPipelineError> {
        // Bevy's mesh pipeline (views, the mesh, skinning), the material's bind group, and the
        // material's bloom program as its material pipeline would set it up
        let mut desc = self.mesh_pipeline.specialize(key.mesh, layout)?;
        desc.label = Some("bloom_part_pipeline".into());
        desc.layout.push(self.material_layout.clone());
        let shader = crate::ue3mat::program_shader(key.part.program).unwrap_or_default();
        let mut defs = vec![ShaderDefVal::UInt("MATERIAL_BIND_GROUP".into(), 3)];
        if key.part.foreground || key.part.foreground_world {
            defs.push("UE_FOREGROUND".into());
        }
        if key.part.foreground_world {
            defs.push("UE_FOREGROUND_WORLD".into());
        }
        desc.vertex.shader_defs.extend(defs.iter().cloned());
        desc.vertex.shader = shader.clone();
        desc.vertex.entry_point = Some("vertex".into());
        desc.vertex.buffers = vec![layout.0.get_layout(&crate::ue3mat::vertex_attributes(layout))?];
        desc.primitive.cull_mode = if key.part.two_sided { None } else { Some(Face::Back) };
        if let Some(f) = desc.fragment.as_mut() {
            f.shader_defs.extend(defs);
            f.shader = shader;
            f.entry_point = Some("fragment".into());
            f.targets = vec![Some(ColorTargetState { format: PARTS_FORMAT, blend: None, write_mask: ColorWrites::ALL })];
        }
        // against the scene's depth, as its base pass left it (its own vertex shader places the
        // surface there up to rounding: the light passes' nudge towards the camera)
        if let Some(ds) = desc.depth_stencil.as_mut() {
            ds.format = CORE_3D_DEPTH_FORMAT;
            ds.depth_write_enabled = Some(false);
            ds.depth_compare = Some(CompareFunction::GreaterEqual);
            ds.bias.constant = 4;
            ds.bias.slope_scale = 1.0;
        }
        Ok(desc)
    }
}

type DrawBloomPart = (SetItemPipeline, SetMeshViewBindGroup<0>, SetMeshViewBindingArrayBindGroup<1>, SetMeshBindGroup<2>, SetMaterialBindGroup<3>, DrawMesh);

struct BloomPart3d {
    sorting_info: TransparentSortingInfo3d,
    distance: FloatOrd,
    entity: (Entity, MainEntity),
    pipeline: CachedRenderPipelineId,
    draw_function: DrawFunctionId,
    batch_range: Range<u32>,
    extra_index: PhaseItemExtraIndex,
    indexed: bool,
}

impl PhaseItem for BloomPart3d {
    fn entity(&self) -> Entity {
        self.entity.0
    }
    fn main_entity(&self) -> MainEntity {
        self.entity.1
    }
    fn draw_function(&self) -> DrawFunctionId {
        self.draw_function
    }
    fn batch_range(&self) -> &Range<u32> {
        &self.batch_range
    }
    fn batch_range_mut(&mut self) -> &mut Range<u32> {
        &mut self.batch_range
    }
    fn extra_index(&self) -> PhaseItemExtraIndex {
        self.extra_index.clone()
    }
    fn batch_range_and_extra_index_mut(&mut self) -> (&mut Range<u32>, &mut PhaseItemExtraIndex) {
        (&mut self.batch_range, &mut self.extra_index)
    }
}

impl SortedPhaseItem for BloomPart3d {
    type SortKey = FloatOrd;
    fn sort_key(&self) -> Self::SortKey {
        self.distance
    }
    fn sort(items: &mut IndexMap<(Entity, MainEntity), BloomPart3d, EntityHash>) {
        items.sort_by_key(|_, i| i.distance);
    }
    fn recalculate_sort_keys(items: &mut IndexMap<(Entity, MainEntity), Self, EntityHash>, view: &ExtractedView) {
        let rangefinder = view.rangefinder3d();
        for item in items.values_mut() {
            item.distance = FloatOrd(item.sorting_info.sort_distance(&rangefinder));
        }
    }
    fn indexed(&self) -> bool {
        self.indexed
    }
}

impl CachedRenderPipelinePhaseItem for BloomPart3d {
    fn cached_pipeline(&self) -> CachedRenderPipelineId {
        self.pipeline
    }
}

impl GetBatchData for BloomPipeline {
    type Param = (SRes<RenderMeshInstances>, SRes<RenderAssets<RenderMesh>>, SRes<MeshAllocator>);
    type BatchSetCompareData = AssetId<Mesh>;
    type BatchCompareData = ();
    type BufferData = MeshUniform;

    fn get_batch_data((mesh_instances, _, mesh_allocator): &SystemParamItem<Self::Param>, (_, main_entity): (Entity, MainEntity)) -> Option<(Self::BufferData, Option<(Self::BatchSetCompareData, Self::BatchCompareData)>)> {
        let RenderMeshInstances::CpuBuilding(ref mesh_instances) = **mesh_instances else { return None };
        let mi = mesh_instances.get(&main_entity)?;
        let first_vertex_index = mesh_allocator.mesh_vertex_slice(&mi.mesh_asset_id()).map(|s| s.range.start).unwrap_or(0);
        Some((MeshUniform::new(&mi.transforms, first_vertex_index, mi.material_bindings_index().slot, None, None, None, None), None))
    }
}

impl GetFullBatchData for BloomPipeline {
    type BufferInputData = MeshInputUniform;

    fn get_index_and_compare_data((mesh_instances, _, _): &SystemParamItem<Self::Param>, main_entity: MainEntity) -> Option<(NonMaxU32, Option<(Self::BatchSetCompareData, Self::BatchCompareData)>)> {
        let RenderMeshInstances::GpuBuilding(ref mesh_instances) = **mesh_instances else { return None };
        let mi = mesh_instances.get(&main_entity)?;
        Some((NonMaxU32::new(mi.gpu_specific.current_uniform_index())?, mi.should_batch().then_some((mi.mesh_asset_id(), ()))))
    }

    fn get_binned_batch_data((mesh_instances, _, mesh_allocator): &SystemParamItem<Self::Param>, main_entity: MainEntity) -> Option<Self::BufferData> {
        let RenderMeshInstances::CpuBuilding(ref mesh_instances) = **mesh_instances else { return None };
        let mi = mesh_instances.get(&main_entity)?;
        let first_vertex_index = mesh_allocator.mesh_vertex_slice(&mi.mesh_asset_id()).map(|s| s.range.start).unwrap_or(0);
        Some(MeshUniform::new(&mi.transforms, first_vertex_index, mi.material_bindings_index().slot, None, None, None, None))
    }

    fn write_batch_indirect_parameters_metadata(indexed: bool, base_output_index: u32, batch_set_index: Option<NonMaxU32>, buffers: &mut UntypedPhaseIndirectParametersBuffers, offset: u32) {
        let meta = IndirectParametersCpuMetadata { base_output_index, batch_set_index: batch_set_index.map(u32::from).unwrap_or(!0) };
        if indexed {
            buffers.indexed.set(offset, meta);
        } else {
            buffers.non_indexed.set(offset, meta);
        }
    }

    fn get_binned_index(_: &SystemParamItem<Self::Param>, _: MainEntity) -> Option<NonMaxU32> {
        None
    }
}

/// The player's view (the one graded: not the reflections' captures, which the original draws
/// without bloom parts).
fn extract_bloom_phases(
    mut phases: ResMut<ViewSortedRenderPhases<BloomPart3d>>,
    cameras: Extract<Query<(Entity, &Camera), (With<Camera3d>, With<ArkGrade>)>>,
    settings: Extract<Res<BloomSettings>>,
    mut live: Local<HashSet<RetainedViewEntity>>,
) {
    live.clear();
    for (e, camera) in &cameras {
        if !camera.is_active || !settings.on {
            continue;
        }
        let v = RetainedViewEntity::new(e.into(), None, 0);
        phases.prepare_for_new_frame(v);
        live.insert(v);
    }
    phases.retain(|v, _| live.contains(v));
}

#[allow(clippy::too_many_arguments)]
fn queue_bloom_parts(
    draw_functions: Res<DrawFunctions<BloomPart3d>>,
    mut pipelines: ResMut<SpecializedMeshPipelines<BloomPipeline>>,
    pipeline_cache: Res<PipelineCache>,
    pipeline: Option<Res<BloomPipeline>>,
    render_meshes: Res<RenderAssets<RenderMesh>>,
    render_mesh_instances: Res<RenderMeshInstances>,
    mut phases: ResMut<ViewSortedRenderPhases<BloomPart3d>>,
    views: Query<&ExtractedView>,
    view_key_cache: Res<ViewKeyCache>,
    parts: Query<(Entity, &MainEntity, &BloomPart)>,
    batched: Option<Res<BatchedInstanceBuffers<MeshUniform, MeshInputUniform>>>,
) {
    let Some(pipeline) = pipeline else { return };
    if parts.is_empty() {
        return;
    }
    let draw = draw_functions.read().id::<DrawBloomPart>();
    let log = std::env::var("DH_BLOOM_LOG").is_ok();
    let mut counts = [0u32; 4];
    for view in &views {
        let Some(phase) = phases.get_mut(&view.retained_view_entity) else { continue };
        let Some(&view_key) = view_key_cache.get(&view.retained_view_entity) else { continue };
        counts[0] += 1;
        for (render_entity, main_entity, part) in &parts {
            counts[1] += 1;
            if crate::ue3mat::program_shader(part.program).is_none() {
                continue;
            }
            counts[2] += 1;
            let Some(mi) = render_mesh_instances.render_mesh_queue_data(*main_entity) else { continue };
            let Some(mesh) = render_meshes.get(mi.mesh_asset_id()) else { continue };
            counts[3] += 1;
            let key = BloomKey { mesh: view_key | MeshPipelineKey::from_bits_retain(mesh.key_bits.bits()), part: *part };
            let id = match pipelines.specialize(&pipeline_cache, &pipeline, key, &mesh.layout) {
                Ok(id) => id,
                Err(e) => {
                    error!("bloom parts: {e}");
                    continue;
                }
            };
            let center = bevy::pbr::get_mesh_instance_world_from_local(*main_entity, mi.current_uniform_index, &render_mesh_instances, batched.as_deref()).transform_point3(mesh.aabb_center);
            phase.add_transient(BloomPart3d {
                sorting_info: TransparentSortingInfo3d::Sorted { mesh_center: center, depth_bias: 0.0 },
                distance: FloatOrd(0.0),
                entity: (render_entity, *main_entity),
                pipeline: id,
                draw_function: draw,
                batch_range: 0..1,
                extra_index: PhaseItemExtraIndex::None,
                indexed: mesh.indexed(),
            });
        }
    }
    if log && rand::random::<f32>() < 0.02 {
        info!("bloom: views {} parts {} visible {} queued {}", counts[0], counts[1], counts[2], counts[3]);
    }
}

fn prepare_post_pipelines(mut pipelines: ResMut<PostPipelines>, pipeline_cache: Res<PipelineCache>, views: Query<&ExtractedView, With<ExtractedCamera>>) {
    let desc = |p: &PostPipelines, label: &'static str, entry: &'static str, format: TextureFormat| RenderPipelineDescriptor {
        label: Some(label.into()),
        layout: vec![p.layout.clone()],
        vertex: p.fullscreen.to_vertex_state(),
        fragment: Some(FragmentState {
            shader: p.shader.clone(),
            entry_point: Some(entry.into()),
            targets: vec![Some(ColorTargetState { format, blend: None::<BlendState>, write_mask: ColorWrites::ALL })],
            ..default()
        }),
        ..default()
    };
    if pipelines.downsample.is_none() {
        let d = desc(&pipelines, "bloom_downsample", "downsample", QUARTER_FORMAT);
        pipelines.downsample = Some(pipeline_cache.queue_render_pipeline(d));
        let b = desc(&pipelines, "bloom_blur", "blur", QUARTER_FORMAT);
        pipelines.blur = Some(pipeline_cache.queue_render_pipeline(b));
    }
    for view in &views {
        let format = view.target_format;
        if pipelines.compose.contains_key(&format) {
            continue;
        }
        let c = desc(&pipelines, "bloom_compose", "compose", format);
        let id = pipeline_cache.queue_render_pipeline(c);
        pipelines.compose.insert(format, id);
    }
}

/// A view's bloom targets and the passes' uniforms (the reduction, the blur along x, along y).
#[derive(Component)]
struct BloomTextures {
    parts: CachedTexture,
    resolved: Option<CachedTexture>,
    quarter: [CachedTexture; 2],
    uniforms: [Buffer; 3],
}

#[allow(clippy::too_many_arguments)]
fn prepare_bloom_textures(
    mut commands: Commands,
    settings: Res<BloomSettings>,
    phases: Res<ViewSortedRenderPhases<BloomPart3d>>,
    mut cache: ResMut<TextureCache>,
    device: Res<RenderDevice>,
    queue: Res<RenderQueue>,
    views: Query<(Entity, &ExtractedView, &ExtractedCamera, &Msaa)>,
    mut buffers: Local<HashMap<Entity, [UniformBuffer<BloomUniform>; 3]>>,
) {
    if !settings.on {
        return;
    }
    buffers.retain(|e, _| views.contains(*e));
    for (e, view, camera, msaa) in &views {
        if phases.get(&view.retained_view_entity).is_none() {
            continue;
        }
        let Some(size) = camera.physical_target_size else { continue };
        let tex = |size: UVec2, format: TextureFormat, samples: u32, label: &'static str| TextureDescriptor {
            label: Some(label),
            size: Extent3d { width: size.x.max(1), height: size.y.max(1), depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: samples,
            dimension: TextureDimension::D2,
            format,
            usage: TextureUsages::RENDER_ATTACHMENT | TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        };
        let samples = msaa.samples();
        let parts = cache.get(&device, tex(size, PARTS_FORMAT, samples, "bloom_parts"));
        let resolved = (samples > 1).then(|| cache.get(&device, tex(size, PARTS_FORMAT, 1, "bloom_parts_resolved")));
        // (the original's filter buffers: a quarter of the scene's size each way)
        let q = (size + 3) / 4;
        let quarter = [cache.get(&device, tex(q, QUARTER_FORMAT, 1, "bloom_quarter_a")), cache.get(&device, tex(q, QUARTER_FORMAT, 1, "bloom_quarter_b"))];
        let texel = |s: UVec2| Vec2::ONE / s.max(UVec2::ONE).as_vec2();
        let debug = Vec4::new(std::env::var("DH_BLOOM_DEBUG").ok().and_then(|v| v.parse().ok()).unwrap_or(0.0), 0.0, 0.0, 0.0);
        let values = [
            BloomUniform { tint: settings.tint, texel: texel(size).extend(0.0).extend(0.0), debug },
            BloomUniform { tint: settings.tint, texel: texel(q).extend(1.0).extend(0.0), debug },
            BloomUniform { tint: settings.tint, texel: texel(q).extend(0.0).extend(1.0), debug },
        ];
        let bufs = buffers.entry(e).or_default();
        for (b, v) in bufs.iter_mut().zip(values) {
            b.set(v);
            b.write_buffer(&device, &queue);
        }
        let Some(uniforms) = (|| Some([bufs[0].buffer()?.clone(), bufs[1].buffer()?.clone(), bufs[2].buffer()?.clone()]))() else { continue };
        commands.entity(e).insert(BloomTextures { parts, resolved, quarter, uniforms });
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_bloom(
    world: &World,
    view: ViewQuery<(&ExtractedCamera, &ExtractedView, &ViewTarget, &ViewDepthTexture, Option<&BloomTextures>)>,
    phases: Res<ViewSortedRenderPhases<BloomPart3d>>,
    settings: Res<BloomSettings>,
    post: Res<PostPipelines>,
    pipeline_cache: Res<PipelineCache>,
    mut ctx: RenderContext,
) {
    if !settings.on {
        return;
    }
    let view_entity = view.entity();
    let (camera, extracted, target, depth, textures) = view.into_inner();
    let Some(textures) = textures else { return };
    let Some(phase) = phases.get(&extracted.retained_view_entity) else { return };
    if phase.items.is_empty() {
        return;
    }
    if std::env::var("DH_BLOOM_LOG").is_ok() && rand::random::<f32>() < 0.02 {
        info!("bloom: drawing {} parts", phase.items.len());
    }
    let (Some(down), Some(blur), Some(compose)) = (
        post.downsample.and_then(|id| pipeline_cache.get_render_pipeline(id)),
        post.blur.and_then(|id| pipeline_cache.get_render_pipeline(id)),
        post.compose.get(&extracted.target_format).and_then(|id| pipeline_cache.get_render_pipeline(*id)),
    ) else {
        if std::env::var("DH_BLOOM_LOG").is_ok() && rand::random::<f32>() < 0.02 {
            let ready = |id: Option<CachedRenderPipelineId>| id.map(|id| format!("{:?}", pipeline_cache.get_render_pipeline_state(id)));
            info!(
                "bloom: pipelines not ready: down {:?} blur {:?} compose {:?} ({:?})",
                ready(post.downsample),
                ready(post.blur),
                ready(post.compose.get(&extracted.target_format).copied()),
                extracted.target_format
            );
        }
        return;
    };
    // the blooming surfaces, against the scene's depth
    {
        let (view, resolve) = match &textures.resolved {
            Some(r) => (&textures.parts.default_view, Some(&*r.default_view)),
            None => (&textures.parts.default_view, None),
        };
        let mut pass = ctx.begin_tracked_render_pass(RenderPassDescriptor {
            label: Some("bloom parts"),
            color_attachments: &[Some(RenderPassColorAttachment {
                view,
                depth_slice: None,
                resolve_target: resolve,
                ops: Operations { load: LoadOp::Clear(Default::default()), store: StoreOp::Store },
            })],
            depth_stencil_attachment: Some(depth.get_attachment(StoreOp::Store)),
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        if let Some(viewport) = camera.viewport.as_ref() {
            pass.set_camera_viewport(&Viewport { physical_position: viewport.physical_position, physical_size: viewport.physical_size, depth: viewport.depth.clone() });
        }
        if let Err(e) = phase.render(&mut pass, world, view_entity) {
            error!("bloom parts: {e:?}");
        }
    }
    let parts = textures.resolved.as_ref().unwrap_or(&textures.parts);
    let layout = pipeline_cache.get_bind_group_layout(&post.layout);
    let device = ctx.render_device().clone();
    let pass = |ctx: &mut RenderContext, label: &'static str, pipeline: &bevy::render::render_resource::RenderPipeline, src: &CachedTexture, dst: &CachedTexture, uniform: &Buffer| {
        let bind_group = device.create_bind_group(label, &layout, &BindGroupEntries::sequential((&src.default_view, &src.default_view, &post.sampler, uniform.as_entire_binding())));
        let mut pass = ctx.begin_tracked_render_pass(RenderPassDescriptor {
            label: Some(label),
            color_attachments: &[Some(RenderPassColorAttachment { view: &dst.default_view, depth_slice: None, resolve_target: None, ops: Operations::default() })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_render_pipeline(pipeline);
        pass.set_bind_group(0, &bind_group, &[]);
        pass.draw(0..3, 0..1);
    };
    // reduced to a quarter, tinted; blurred along x, then y
    pass(&mut ctx, "bloom reduction", down, parts, &textures.quarter[0], &textures.uniforms[0]);
    pass(&mut ctx, "bloom blur x", blur, &textures.quarter[0], &textures.quarter[1], &textures.uniforms[1]);
    pass(&mut ctx, "bloom blur y", blur, &textures.quarter[1], &textures.quarter[0], &textures.uniforms[2]);
    // added to the scene
    let out = target.post_process_write();
    let bind_group = device.create_bind_group(
        "bloom_compose",
        &layout,
        &BindGroupEntries::sequential((out.source, &textures.quarter[0].default_view, &post.sampler, textures.uniforms[0].as_entire_binding())),
    );
    let mut pass = ctx.begin_tracked_render_pass(RenderPassDescriptor {
        label: Some("bloom compose"),
        color_attachments: &[Some(RenderPassColorAttachment { view: out.destination, depth_slice: None, resolve_target: None, ops: Operations::default() })],
        depth_stencil_attachment: None,
        timestamp_writes: None,
        occlusion_query_set: None,
        multiview_mask: None,
    });
    pass.set_render_pipeline(compose);
    pass.set_bind_group(0, &bind_group, &[]);
    pass.draw(0..3, 0..1);
    if std::env::var("DH_BLOOM_LOG").is_ok() && rand::random::<f32>() < 0.02 {
        info!("bloom: composed into {:?} ({:?})", out.destination.id(), extracted.target_format);
    }
}
