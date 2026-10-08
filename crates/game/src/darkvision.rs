//! Dark Vision as the original draws it (`Twk_DarkVision` and the `AltScreen_Effects`
//! post-process graph's Dark Vision node).
//!
//! Living beings within the power's reach are drawn as "souls" (`DarkVision_PMAT` with
//! `DarkVision_Souls_INST`) into a mask (`DisDarkVisionMeshRenderPpController`'s target): full
//! colour where they are in view, the behind colour stirred by drifting noise where something
//! hides them, more opaque at the silhouette, fading out towards the power's distance. The
//! power's material (`PPG_DarkVisionFinal_Mat`) then mixes the mask over the graded image,
//! darkens the corners and opens the eye from the middle (`fxt_eyelid_gradient`); the eyelid
//! material (`PPG_EyeLidFinal_Mat`) closes it, top and bottom, as the power starts and ends.
//! Both run after the colour grading, in display space, as the original's node follows its
//! DOF + colour balance + HDR node. The soul pass reuses Bevy's mesh pipeline (skinning,
//! vertex shader) with the soul fragment shader (`darkvision.wgsl`); the composite is
//! `darkvision_post.wgsl`. Both are the original pixel shaders by hand.

use bevy::asset::embedded_asset;
use bevy::camera::Viewport;
use bevy::core_pipeline::core_3d::TransparentSortingInfo3d;
use bevy::core_pipeline::{schedule::Core3d, Core3dSystems, FullscreenShader};
use bevy::ecs::entity::EntityHash;
use bevy::ecs::query::ROQueryItem;
use bevy::ecs::system::{lifetimeless::SRes, SystemParamItem};
use bevy::math::FloatOrd;
use bevy::mesh::MeshVertexBufferLayoutRef;
use bevy::pbr::{
    DrawMesh, MeshInputUniform, MeshPipeline, MeshPipelineKey, MeshPipelineSystems, MeshUniform, RenderMeshInstances, SetMeshBindGroup,
    SetMeshViewBindGroup, SetMeshViewBindingArrayBindGroup, ViewKeyCache,
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
    sort_phase_system, AddRenderCommand, CachedRenderPipelinePhaseItem, DrawFunctionId, DrawFunctions, PhaseItem, PhaseItemExtraIndex, RenderCommand,
    RenderCommandResult, SetItemPipeline, SortedPhaseItem, SortedRenderPhasePlugin, TrackedRenderPass, ViewSortedRenderPhases,
};
use bevy::render::render_resource::binding_types::{sampler, texture_2d, uniform_buffer};
use bevy::render::render_resource::{
    AddressMode, BindGroup, BindGroupEntries, BindGroupLayoutDescriptor, BindGroupLayoutEntries, CachedRenderPipelineId, ColorTargetState, ColorWrites,
    CompareFunction, DepthStencilState, Extent3d, FilterMode, FragmentState, LoadOp, MipmapFilterMode, Operations, PipelineCache,
    RenderPassColorAttachment, RenderPassDepthStencilAttachment, RenderPassDescriptor, RenderPipelineDescriptor, Sampler, SamplerBindingType,
    SamplerDescriptor, ShaderStages, ShaderType, SpecializedMeshPipeline, SpecializedMeshPipelineError, SpecializedMeshPipelines, StoreOp,
    TextureDescriptor, TextureDimension, TextureFormat, TextureSampleType, TextureUsages, UniformBuffer,
};
use bevy::render::renderer::{RenderContext, RenderDevice, RenderQueue, ViewQuery};
use bevy::render::sync_world::MainEntity;
use bevy::render::texture::{CachedTexture, GpuImage, TextureCache};
use bevy::render::view::{ExtractedView, RetainedViewEntity, ViewTarget};
use bevy::render::{Extract, Render, RenderApp, RenderDebugFlags, RenderStartup, RenderSystems};
use indexmap::IndexMap;
use nonmax::NonMaxU32;
use std::ops::Range;

pub struct DarkVisionPlugin;

impl Plugin for DarkVisionPlugin {
    fn build(&self, app: &mut App) {
        embedded_asset!(app, "darkvision.wgsl");
        embedded_asset!(app, "darkvision_post.wgsl");
        app.init_resource::<DvSouls>().add_plugins((
            ExtractComponentPlugin::<DvSoul>::default(),
            ExtractResourcePlugin::<DvSouls>::default(),
            SortedRenderPhasePlugin::<Soul3d, MeshPipeline>::new(RenderDebugFlags::default()),
        ));
        let Some(render_app) = app.get_sub_app_mut(RenderApp) else { return };
        render_app
            .init_resource::<SpecializedMeshPipelines<SoulPipeline>>()
            .init_resource::<DrawFunctions<Soul3d>>()
            .init_resource::<SoulBindGroup>()
            .add_render_command::<Soul3d, DrawSoul<0>>()
            .add_render_command::<Soul3d, DrawSoul<1>>()
            .add_render_command::<Soul3d, DrawSoul<2>>()
            .init_resource::<ViewSortedRenderPhases<Soul3d>>()
            .add_systems(RenderStartup, init_pipelines.after(MeshPipelineSystems))
            .add_systems(ExtractSchedule, extract_soul_phases)
            .add_systems(
                Render,
                (
                    prepare_soul_bind_group.in_set(RenderSystems::PrepareBindGroups),
                    (prepare_composite_pipelines, prepare_mask_textures).in_set(RenderSystems::PrepareResources),
                    queue_souls.in_set(RenderSystems::QueueMeshes),
                    sort_phase_system::<Soul3d>.in_set(RenderSystems::PhaseSort),
                    batch_and_prepare_sorted_render_phase::<Soul3d, SoulPipeline>.in_set(RenderSystems::PrepareResources),
                ),
            )
            // after the colour grading (`postfx::ArkGrade`), as the original's node follows its
            // DOF + colour balance + HDR node
            .add_systems(
                Core3d,
                draw_dark_vision
                    .in_set(Core3dSystems::PostProcess)
                    .after(bevy::core_pipeline::fullscreen_material::fullscreen_material_system::<crate::postfx::ArkGrade>)
                    .before(bevy::anti_alias::fxaa::fxaa).before(bevy::anti_alias::smaa::smaa)
                    .before(crate::ppgraph::GraphPass),
            );
    }
}

/// Something drawn into Dark Vision's mask while the power shows it: 0 a living being's part
/// (`DarkVision_Souls_INST`), 1 an item (`DarkVision_Items_INST`), 2 a mechanism
/// (`DarkVision_Mechanics_INST`).
#[derive(Component, ExtractComponent, Clone, Copy, Default, PartialEq)]
pub struct DvSoul(pub u8);

/// Dark Vision's look: the souls' colours (`DarkVision_Souls_INST`), their noise textures and
/// the distances they fade out between (metres); the eye (`EyeLidColor`, the gradient), which
/// material is on (1 the eyelid's, 2 the power's) and its `Alpha`.
#[derive(Resource, Clone, ExtractResource, Default)]
pub struct DvSouls {
    pub full: [Vec4; 3],
    pub behind: [Vec4; 3],
    pub near_far: Vec2,
    pub cloud: Handle<Image>,
    pub smoke: Handle<Image>,
    pub gradient: Handle<Image>,
    pub eyelid: Vec4,
    pub mode: u32,
    pub alpha: f32,
}

#[derive(ShaderType, Clone, Copy, Default)]
struct SoulUniform {
    full: Vec4,
    behind: Vec4,
    /// fade start, fade end (metres), time (seconds), -
    misc: Vec4,
}

#[derive(ShaderType, Clone, Copy, Default)]
struct EyeUniform {
    eyelid: Vec4,
    /// material (1 eyelid, 2 power), its alpha
    mode: Vec4,
}

/// The souls' mask (and the depth they sort by among themselves).
const MASK_FORMAT: TextureFormat = TextureFormat::Rgba8Unorm;
const MASK_DEPTH: TextureFormat = TextureFormat::Depth32Float;

#[derive(Resource)]
struct SoulPipeline {
    mesh_pipeline: MeshPipeline,
    shader: Handle<Shader>,
    layout: BindGroupLayoutDescriptor,
    sampler: Sampler,
}

#[derive(Resource)]
struct CompositePipelines {
    layout: BindGroupLayoutDescriptor,
    sampler: Sampler,
    shader: Handle<Shader>,
    fullscreen: FullscreenShader,
    ids: HashMap<TextureFormat, CachedRenderPipelineId>,
}

fn init_pipelines(mut commands: Commands, mesh_pipeline: Res<MeshPipeline>, asset_server: Res<AssetServer>, device: Res<RenderDevice>, fullscreen: Res<FullscreenShader>) {
    let layout = BindGroupLayoutDescriptor::new(
        "dv_soul_layout",
        &BindGroupLayoutEntries::sequential(
            ShaderStages::FRAGMENT,
            (
                uniform_buffer::<SoulUniform>(false),
                texture_2d(TextureSampleType::Float { filterable: true }),
                texture_2d(TextureSampleType::Float { filterable: true }),
                sampler(SamplerBindingType::Filtering),
            ),
        ),
    );
    let repeat = device.create_sampler(&SamplerDescriptor {
        address_mode_u: AddressMode::Repeat,
        address_mode_v: AddressMode::Repeat,
        mag_filter: FilterMode::Linear,
        min_filter: FilterMode::Linear,
        mipmap_filter: MipmapFilterMode::Linear,
        ..default()
    });
    commands.insert_resource(SoulPipeline {
        mesh_pipeline: mesh_pipeline.clone(),
        shader: asset_server.load("embedded://dishonored/darkvision.wgsl"),
        layout,
        sampler: repeat,
    });
    let composite = BindGroupLayoutDescriptor::new(
        "dv_composite_layout",
        &BindGroupLayoutEntries::sequential(
            ShaderStages::FRAGMENT,
            (
                texture_2d(TextureSampleType::Float { filterable: true }),
                texture_2d(TextureSampleType::Float { filterable: true }),
                texture_2d(TextureSampleType::Float { filterable: true }),
                sampler(SamplerBindingType::Filtering),
                uniform_buffer::<EyeUniform>(false),
            ),
        ),
    );
    let clamp = device.create_sampler(&SamplerDescriptor { mag_filter: FilterMode::Linear, min_filter: FilterMode::Linear, ..default() });
    commands.insert_resource(CompositePipelines {
        layout: composite,
        sampler: clamp,
        shader: asset_server.load("embedded://dishonored/darkvision_post.wgsl"),
        fullscreen: fullscreen.clone(),
        ids: HashMap::default(),
    });
}

impl SpecializedMeshPipeline for SoulPipeline {
    type Key = MeshPipelineKey;

    fn specialize(&self, key: Self::Key, layout: &MeshVertexBufferLayoutRef) -> Result<RenderPipelineDescriptor, SpecializedMeshPipelineError> {
        // Bevy's own mesh pipeline (skinning and all), the soul shader's fragment stage, into
        // the mask (the nearest soul over the others)
        let mut desc = self.mesh_pipeline.specialize(key, layout)?;
        desc.label = Some("dv_soul_pipeline".into());
        desc.layout.push(self.layout.clone());
        if let Some(f) = desc.fragment.as_mut() {
            f.shader = self.shader.clone();
            f.entry_point = Some("fragment".into());
            f.targets = vec![Some(ColorTargetState { format: MASK_FORMAT, blend: None, write_mask: ColorWrites::ALL })];
        }
        desc.depth_stencil = Some(DepthStencilState {
            format: MASK_DEPTH,
            depth_write_enabled: Some(true),
            depth_compare: Some(CompareFunction::GreaterEqual),
            stencil: default(),
            bias: default(),
        });
        desc.multisample = default();
        Ok(desc)
    }
}

/// The soul bind groups (group 3, by kind), the composite's uniform and gradient, and whether
/// the node runs at all.
#[derive(Resource, Default)]
struct SoulBindGroup {
    souls: [Option<BindGroup>; 3],
    eye: Option<(UniformBuffer<EyeUniform>, Handle<Image>)>,
    on: bool,
}

#[allow(clippy::too_many_arguments)]
fn prepare_soul_bind_group(
    souls: Option<Res<DvSouls>>,
    pipeline: Option<Res<SoulPipeline>>,
    pipeline_cache: Res<PipelineCache>,
    images: Res<RenderAssets<GpuImage>>,
    device: Res<RenderDevice>,
    queue: Res<RenderQueue>,
    time: Res<Time>,
    mut bind_group: ResMut<SoulBindGroup>,
    mut buffers: Local<[UniformBuffer<SoulUniform>; 3]>,
) {
    let (Some(souls), Some(pipeline)) = (souls, pipeline) else { return };
    bind_group.on = souls.mode != 0;
    if !bind_group.on {
        return;
    }
    let mut eye = UniformBuffer::from(EyeUniform { eyelid: souls.eyelid, mode: Vec4::new(souls.mode as f32, souls.alpha, 0.0, 0.0) });
    eye.write_buffer(&device, &queue);
    bind_group.eye = Some((eye, souls.gradient.clone()));
    let (Some(cloud), Some(smoke)) = (images.get(&souls.cloud), images.get(&souls.smoke)) else {
        bind_group.souls = Default::default();
        return;
    };
    for (k, buffer) in buffers.iter_mut().enumerate() {
        buffer.set(SoulUniform { full: souls.full[k], behind: souls.behind[k], misc: Vec4::new(souls.near_far.x, souls.near_far.y, time.elapsed_secs_wrapped(), 0.0) });
        buffer.write_buffer(&device, &queue);
        let Some(binding) = buffer.binding() else { continue };
        bind_group.souls[k] = Some(device.create_bind_group(
            "dv_soul_bind_group",
            &pipeline_cache.get_bind_group_layout(&pipeline.layout),
            &BindGroupEntries::sequential((binding, &cloud.texture_view, &smoke.texture_view, &pipeline.sampler)),
        ));
    }
}

fn prepare_composite_pipelines(mut pipelines: ResMut<CompositePipelines>, pipeline_cache: Res<PipelineCache>, views: Query<&ExtractedView, With<ExtractedCamera>>) {
    for view in &views {
        let format = view.target_format;
        if pipelines.ids.contains_key(&format) {
            continue;
        }
        let desc = RenderPipelineDescriptor {
            label: Some("dv_composite".into()),
            layout: vec![pipelines.layout.clone()],
            vertex: pipelines.fullscreen.to_vertex_state(),
            fragment: Some(FragmentState {
                shader: pipelines.shader.clone(),
                targets: vec![Some(ColorTargetState { format, blend: None, write_mask: ColorWrites::ALL })],
                ..default()
            }),
            ..default()
        };
        let id = pipeline_cache.queue_render_pipeline(desc);
        pipelines.ids.insert(format, id);
    }
}

/// A view's soul mask and its depth, while Dark Vision is on.
#[derive(Component)]
struct MaskTextures {
    mask: CachedTexture,
    depth: CachedTexture,
}

fn prepare_mask_textures(mut commands: Commands, state: Res<SoulBindGroup>, mut cache: ResMut<TextureCache>, device: Res<RenderDevice>, views: Query<(Entity, &ExtractedCamera)>) {
    if !state.on {
        return;
    }
    for (e, camera) in &views {
        let Some(size) = camera.physical_target_size else { continue };
        let size = Extent3d { width: size.x.max(1), height: size.y.max(1), depth_or_array_layers: 1 };
        let tex = |format: TextureFormat, label: &'static str| TextureDescriptor {
            label: Some(label),
            size,
            mip_level_count: 1,
            sample_count: 1,
            dimension: TextureDimension::D2,
            format,
            usage: TextureUsages::RENDER_ATTACHMENT | TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        };
        let mask = cache.get(&device, tex(MASK_FORMAT, "dv_mask"));
        let depth = cache.get(&device, tex(MASK_DEPTH, "dv_mask_depth"));
        commands.entity(e).insert(MaskTextures { mask, depth });
    }
}

struct SetSoulBindGroup<const I: usize, const K: usize>;

impl<P: PhaseItem, const I: usize, const K: usize> RenderCommand<P> for SetSoulBindGroup<I, K> {
    type Param = SRes<SoulBindGroup>;
    type ViewQuery = ();
    type ItemQuery = ();

    fn render<'w>(_: &P, _: ROQueryItem<'w, '_, ()>, _: Option<ROQueryItem<'w, '_, ()>>, bg: SystemParamItem<'w, '_, Self::Param>, pass: &mut TrackedRenderPass<'w>) -> RenderCommandResult {
        match &bg.into_inner().souls[K] {
            Some(b) => {
                pass.set_bind_group(I, b, &[]);
                RenderCommandResult::Success
            }
            None => RenderCommandResult::Skip,
        }
    }
}

type DrawSoul<const K: usize> = (SetItemPipeline, SetMeshViewBindGroup<0>, SetMeshViewBindingArrayBindGroup<1>, SetMeshBindGroup<2>, SetSoulBindGroup<3, K>, DrawMesh);

struct Soul3d {
    sorting_info: TransparentSortingInfo3d,
    distance: FloatOrd,
    entity: (Entity, MainEntity),
    pipeline: CachedRenderPipelineId,
    draw_function: DrawFunctionId,
    batch_range: Range<u32>,
    extra_index: PhaseItemExtraIndex,
    indexed: bool,
}

impl PhaseItem for Soul3d {
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

impl SortedPhaseItem for Soul3d {
    type SortKey = FloatOrd;
    fn sort_key(&self) -> Self::SortKey {
        self.distance
    }
    fn sort(items: &mut IndexMap<(Entity, MainEntity), Soul3d, EntityHash>) {
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

impl CachedRenderPipelinePhaseItem for Soul3d {
    fn cached_pipeline(&self) -> CachedRenderPipelineId {
        self.pipeline
    }
}

impl GetBatchData for SoulPipeline {
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

impl GetFullBatchData for SoulPipeline {
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

fn extract_soul_phases(mut phases: ResMut<ViewSortedRenderPhases<Soul3d>>, cameras: Extract<Query<(Entity, &Camera), With<Camera3d>>>, mut live: Local<HashSet<RetainedViewEntity>>) {
    live.clear();
    for (e, camera) in &cameras {
        if !camera.is_active {
            continue;
        }
        let v = RetainedViewEntity::new(e.into(), None, 0);
        phases.prepare_for_new_frame(v);
        live.insert(v);
    }
    phases.retain(|v, _| live.contains(v));
}

#[allow(clippy::too_many_arguments)]
fn queue_souls(
    draw_functions: Res<DrawFunctions<Soul3d>>,
    mut pipelines: ResMut<SpecializedMeshPipelines<SoulPipeline>>,
    pipeline_cache: Res<PipelineCache>,
    pipeline: Option<Res<SoulPipeline>>,
    render_meshes: Res<RenderAssets<RenderMesh>>,
    render_mesh_instances: Res<RenderMeshInstances>,
    mut phases: ResMut<ViewSortedRenderPhases<Soul3d>>,
    views: Query<&ExtractedView>,
    view_key_cache: Res<ViewKeyCache>,
    souls: Query<(Entity, &MainEntity, &DvSoul)>,
    batched: Option<Res<BatchedInstanceBuffers<MeshUniform, MeshInputUniform>>>,
) {
    let Some(pipeline) = pipeline else { return };
    if souls.is_empty() {
        return;
    }
    let draws = {
        let f = draw_functions.read();
        [f.id::<DrawSoul<0>>(), f.id::<DrawSoul<1>>(), f.id::<DrawSoul<2>>()]
    };
    for view in &views {
        let Some(phase) = phases.get_mut(&view.retained_view_entity) else { continue };
        let Some(&view_key) = view_key_cache.get(&view.retained_view_entity) else { continue };
        // (the view's key as it is: its bind group's layout, a multisampled depth prepass
        // and all; the mask itself is single-sampled, see `specialize`)
        for (render_entity, main_entity, kind) in &souls {
            let Some(mi) = render_mesh_instances.render_mesh_queue_data(*main_entity) else { continue };
            let Some(mesh) = render_meshes.get(mi.mesh_asset_id()) else { continue };
            let key = view_key | MeshPipelineKey::from_primitive_topology_and_strip_index(mesh.primitive_topology(), mesh.index_format());
            let id = match pipelines.specialize(&pipeline_cache, &pipeline, key, &mesh.layout) {
                Ok(id) => id,
                Err(e) => {
                    error!("dark vision: {e}");
                    continue;
                }
            };
            let center = bevy::pbr::get_mesh_instance_world_from_local(*main_entity, mi.current_uniform_index, &render_mesh_instances, batched.as_deref()).transform_point3(mesh.aabb_center);
            phase.add_transient(Soul3d {
                sorting_info: TransparentSortingInfo3d::Sorted { mesh_center: center, depth_bias: 0.0 },
                distance: FloatOrd(0.0),
                entity: (render_entity, *main_entity),
                pipeline: id,
                draw_function: draws[(kind.0 as usize).min(2)],
                batch_range: 0..1,
                extra_index: PhaseItemExtraIndex::None,
                indexed: mesh.indexed(),
            });
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_dark_vision(
    world: &World,
    view: ViewQuery<(&ExtractedCamera, &ExtractedView, &ViewTarget, Option<&MaskTextures>)>,
    phases: Res<ViewSortedRenderPhases<Soul3d>>,
    state: Res<SoulBindGroup>,
    composite: Res<CompositePipelines>,
    pipeline_cache: Res<PipelineCache>,
    images: Res<RenderAssets<GpuImage>>,
    mut ctx: RenderContext,
) {
    if !state.on {
        return;
    }
    let view_entity = view.entity();
    let (camera, extracted, target, masks) = view.into_inner();
    let Some(masks) = masks else { return };
    // the souls into the mask
    {
        let mut pass = ctx.begin_tracked_render_pass(RenderPassDescriptor {
            label: Some("dark vision souls"),
            color_attachments: &[Some(RenderPassColorAttachment {
                view: &masks.mask.default_view,
                depth_slice: None,
                resolve_target: None,
                ops: Operations { load: LoadOp::Clear(Default::default()), store: StoreOp::Store },
            })],
            depth_stencil_attachment: Some(RenderPassDepthStencilAttachment {
                view: &masks.depth.default_view,
                depth_ops: Some(Operations { load: LoadOp::Clear(0.0), store: StoreOp::Store }),
                stencil_ops: None,
            }),
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        if let Some(viewport) = camera.viewport.as_ref() {
            pass.set_camera_viewport(&Viewport { physical_position: viewport.physical_position, physical_size: viewport.physical_size, depth: viewport.depth.clone() });
        }
        if let Some(phase) = phases.get(&extracted.retained_view_entity) {
            if let Err(e) = phase.render(&mut pass, world, view_entity) {
                error!("dark vision souls: {e:?}");
            }
        }
    }
    // the power's (or the eyelid's) material over the graded image
    let Some(pipeline) = composite.ids.get(&extracted.target_format).and_then(|id| pipeline_cache.get_render_pipeline(*id)) else { return };
    let Some((eye, gradient)) = state.eye.as_ref() else { return };
    let Some(gradient) = images.get(gradient) else { return };
    let Some(eye_binding) = eye.binding() else { return };
    let post = target.post_process_write();
    let bind_group = ctx.render_device().create_bind_group(
        "dv_composite_bind_group",
        &pipeline_cache.get_bind_group_layout(&composite.layout),
        &BindGroupEntries::sequential((post.source, &masks.mask.default_view, &gradient.texture_view, &composite.sampler, eye_binding)),
    );
    let mut pass = ctx.begin_tracked_render_pass(RenderPassDescriptor {
        label: Some("dark vision"),
        color_attachments: &[Some(RenderPassColorAttachment { view: post.destination, depth_slice: None, resolve_target: None, ops: Operations::default() })],
        depth_stencil_attachment: None,
        timestamp_writes: None,
        occlusion_query_set: None,
        multiview_mask: None,
    });
    pass.set_render_pipeline(pipeline);
    pass.set_bind_group(0, &bind_group, &[]);
    pass.draw(0..3, 0..1);
}

/// Where the eye is: shutting before the power's look (the eyelid's material), opening on it
/// (the power's), the power on, then the same backwards as it ends.
#[derive(Default, PartialEq, Clone, Copy, Debug)]
pub enum Eye {
    #[default]
    Open,
    Closing,
    Opening,
    On,
    ClosingOff,
    OpeningOff,
}

/// What Dark Vision shows (living beings within its reach; at level 2 items and mechanisms
/// too), their sight (`Ps_DVision_VCone_Blue_01`, `PS_DVision_VCone_Red_01` once alerted, within
/// `m_fSecondaryEffectDistance`; the combat one in a fight), and the eye's timeline (`m_PpPerLevelParameters`: the
/// eyelid's closing and opening durations).
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
pub fn update_dark_vision(
    mut commands: Commands,
    (time, powers, data, stats, assets, devices): (
        Res<Time>,
        Res<crate::powers::Powers>,
        Res<crate::gamedata::Data>,
        Res<crate::gameplay::PlayerStats>,
        Option<Res<crate::level::GameAssets>>,
        Res<crate::security::Devices>,
    ),
    mut souls: ResMut<DvSouls>,
    mut eye: Local<(Eye, f32)>,
    mut fx: MessageWriter<crate::particles::SpawnEffect>,
    player: Query<&Transform, With<crate::player::Player>>,
    npcs: Query<(Entity, &crate::npc::Npc, &Transform, &Children)>,
    pickups: Query<(&crate::interact::Pickup, &Transform), Without<crate::npc::Npc>>,
    children: Query<&Children>,
    meshes: Query<(Entity, Option<&DvSoul>), With<Mesh3d>>,
    mut cones: Query<(&crate::particles::EffectTag, &mut crate::particles::ParticleEmitter)>,
    mut lit: Local<HashMap<Entity, (bool, f32)>>,
) {
    let on = powers.dark_vision;
    if let Some(a) = assets.as_ref() {
        if souls.gradient == Handle::default() {
            souls.cloud = a.textures.get("dv_cloud").cloned().unwrap_or_default();
            souls.smoke = a.textures.get("dv_smoke").cloned().unwrap_or_default();
            souls.gradient = a.textures.get("eyelid_gradient").cloned().unwrap_or_default();
        }
    }
    // `DarkVision_Souls_INST`, `_Items_INST`, `_Mechanics_INST`; `PPG_DarkVisionFinal_Mat`
    souls.full = [Vec4::new(1.0, 0.947_146_5, 0.0, 1.0), Vec4::new(0.343_842_74, 0.755_434_8, 0.208_412_81, 1.0), Vec4::new(0.228_635_2, 0.302_210_93, 0.75, 1.0)];
    souls.behind = [Vec4::new(0.858_695_6, 0.810_195_9, 0.0, 1.0), Vec4::new(0.1799, 0.521_739_1, 0.070_888_47, 1.0), Vec4::new(0.291_174_38, 0.440_667_36, 0.864_130_44, 1.0)];
    souls.eyelid = Vec4::new(0.392, 0.333, 0.275, 1.0);
    let level = stats.power("DarkVision").max(1);
    let uu = 0.01;
    let reach = data.power_f("DarkVision", level, "m_fDistance", 2000.0) * uu;
    let fade_from = data.power_f("DarkVision", level, "m_fFadeStartAtDistance", 1700.0) * uu;
    let sight = data.power_f("DarkVision", level, "m_fSecondaryEffectDistance", 1200.0) * uu;
    souls.near_far = Vec2::new(fade_from, reach);
    let close = data.power_f("DarkVision", level, "m_PpPerLevelParameters.m_EyeLidClosingDuration", 0.05).max(0.01);
    let open = data.power_f("DarkVision", level, "m_PpPerLevelParameters.m_EyeLidOpeningDuration", 0.1).max(0.01);
    // the eye's timeline
    let (ref mut phase, ref mut t) = *eye;
    *t += time.delta_secs();
    match (*phase, on) {
        (Eye::Open, true) | (Eye::OpeningOff, true) | (Eye::ClosingOff, true) => {
            *phase = Eye::Closing;
            *t = 0.0;
        }
        (Eye::Closing, _) if *t >= close => {
            *phase = Eye::Opening;
            *t = 0.0;
        }
        (Eye::Opening, _) if *t >= open => {
            *phase = Eye::On;
            *t = 0.0;
        }
        (Eye::On, false) | (Eye::Opening, false) => {
            *phase = Eye::ClosingOff;
            *t = 0.0;
        }
        (Eye::ClosingOff, false) if *t >= close => {
            *phase = Eye::OpeningOff;
            *t = 0.0;
        }
        (Eye::OpeningOff, false) if *t >= open => {
            *phase = Eye::Open;
            *t = 0.0;
        }
        _ => {}
    }
    (souls.mode, souls.alpha) = match *phase {
        Eye::Open => (0, 0.0),
        Eye::Closing => (1, (*t / close).min(1.0)),
        Eye::Opening => (2, (*t / open).min(1.0)),
        Eye::On => (2, 1.0),
        Eye::ClosingOff => (2, 1.0 - (*t / close).min(1.0)),
        Eye::OpeningOff => (1, 1.0 - (*t / open).min(1.0)),
    };
    let show = souls.mode == 2;
    let at = player.single().map(|t| t.translation).unwrap_or(Vec3::ZERO);
    // the meshes of each thing shown (a character's are its descendants)
    let mut want: HashMap<Entity, u8> = HashMap::default();
    let collect = |root: Entity, kind: u8, want: &mut HashMap<Entity, u8>| {
        let mut stack = vec![root];
        while let Some(e) = stack.pop() {
            if meshes.get(e).is_ok() {
                want.insert(e, kind);
            }
            if let Ok(c) = children.get(e) {
                stack.extend(c.iter());
            }
        }
    };
    let mut sighted: HashMap<Entity, bool> = HashMap::default();
    if show {
        for (e, npc, t, _) in &npcs {
            let d = t.translation.distance(at);
            if npc.mode == crate::npc::Mode::Dead || d > reach {
                continue;
            }
            collect(e, 0, &mut want);
            if !npc.is_down() && d <= sight {
                // (`m_VisionConePatrolTemplate`, `m_VisionConeCombatTemplate`)
                sighted.insert(e, npc.alert == crate::npc::Alert::Combat);
            }
        }
        if level >= 2 {
            for (p, t) in &pickups {
                if t.translation.distance(at) <= reach {
                    for &m in &p.entities {
                        collect(m, 1, &mut want);
                    }
                }
            }
            for d in &devices.list {
                for &i in d.instances() {
                    collect(i, 2, &mut want);
                }
            }
        }
    }
    for (e, kind) in &meshes {
        match (want.get(&e), kind) {
            (Some(&k), Some(cur)) if cur.0 == k => {}
            (Some(&k), _) => {
                commands.entity(e).try_insert(DvSoul(k));
            }
            (None, Some(_)) => {
                commands.entity(e).try_remove::<DvSoul>();
            }
            (None, None) => {}
        }
    }
    // the characters' sight: a cone from the eyes, turning with them (red eyes in a fight)
    let now = time.elapsed_secs();
    for (tag, mut em) in &mut cones {
        let keep = match (sighted.get(&tag.0), lit.get(&tag.0)) {
            (Some(&red), Some(&(was, _))) => red == was,
            _ => false,
        };
        if !keep {
            em.stop();
        }
    }
    lit.retain(|e, (red, _)| sighted.get(e) == Some(red));
    for (e, red) in sighted {
        if lit.contains_key(&e) {
            continue;
        }
        lit.insert(e, (red, now));
        fx.write(crate::particles::SpawnEffect {
            follow: Some(e),
            turn: true,
            // (the system's forward, UE +X, along the character's facing)
            rot: Quat::from_rotation_y(std::f32::consts::FRAC_PI_2),
            secs: 3600.0,
            tag: Some(e),
            ..crate::particles::SpawnEffect::at(if red { "dv_cone_red" } else { "dv_cone_blue" }, Vec3::Y * 0.7)
        });
    }
}

/// What the guards could hear, seen (`m_AINoiseVisualEffect`: `Ps_DVision_Sounds_Base01` where
/// a noise is made) while the power shows the world.
pub fn dark_vision_sounds(souls: Res<DvSouls>, mut noises: MessageReader<crate::gameplay::Noise>, mut fx: MessageWriter<crate::particles::SpawnEffect>, mut last: Local<Vec<(Vec3, f32)>>, time: Res<Time>) {
    let now = time.elapsed_secs();
    last.retain(|(_, t)| now - *t < 0.5);
    for n in noises.read() {
        if souls.mode != 2 || last.iter().any(|(p, _)| p.distance(n.pos) < 1.0) {
            continue;
        }
        last.push((n.pos, now));
        fx.write(crate::particles::SpawnEffect { secs: 2.0, ..crate::particles::SpawnEffect::at("dv_sound", n.pos) });
    }
}

