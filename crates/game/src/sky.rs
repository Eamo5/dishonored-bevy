//! Custom material reproducing Dishonored's procedural sky domes.

use bevy::asset::embedded_asset;
use bevy::prelude::*;
use bevy::reflect::TypePath;
use bevy::mesh::MeshVertexBufferLayoutRef;
use bevy::pbr::{MaterialPipeline, MaterialPipelineKey};
use bevy::render::render_resource::{AsBindGroup, RenderPipelineDescriptor, ShaderType, SpecializedMeshPipelineError};
use bevy::shader::ShaderRef;

pub struct SkyPlugin;

impl Plugin for SkyPlugin {
    fn build(&self, app: &mut App) {
        embedded_asset!(app, "sky.wgsl");
        app.add_plugins(MaterialPlugin::<SkyMaterial>::default());
    }
}

#[derive(ShaderType, Debug, Clone, Default)]
pub struct SkyParams {
    pub top: Vec4,
    pub bottom: Vec4,
    pub horizon: Vec4,
    pub clouds_color: Vec4,
    pub clouds2_color: Vec4,
    pub storm_color: Vec4,
    pub p0: Vec4,
    pub p1: Vec4,
}

#[derive(Asset, TypePath, AsBindGroup, Debug, Clone)]
pub struct SkyMaterial {
    #[uniform(0)]
    pub params: SkyParams,
    #[texture(1)]
    #[sampler(2)]
    pub clouds: Handle<Image>,
    #[texture(3)]
    #[sampler(4)]
    pub clouds2: Handle<Image>,
    #[texture(5)]
    #[sampler(6)]
    pub storm: Handle<Image>,
}

impl Material for SkyMaterial {
    fn fragment_shader() -> ShaderRef {
        "embedded://dishonored/sky.wgsl".into()
    }

    fn specialize(
        _pipeline: &MaterialPipeline,
        descriptor: &mut RenderPipelineDescriptor,
        _layout: &MeshVertexBufferLayoutRef,
        _key: MaterialPipelineKey<Self>,
    ) -> Result<(), SpecializedMeshPipelineError> {
        // sky domes are viewed from inside; their winding is not reliable
        descriptor.primitive.cull_mode = None;
        Ok(())
    }
}
