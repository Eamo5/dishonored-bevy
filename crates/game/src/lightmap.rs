//! Baked lighting support via a StandardMaterial extension.
//!
//! Bevy's built-in `Lightmap` component allocates one lightmap slot per entity, which
//! breaks batching for thousands of level instances. Instead every world material shares
//! one storage buffer of per-instance entries (indexed through `MeshTag`) plus texture
//! arrays holding the packed lightmap pages and the dominant light's shadow-map pages.

use bevy::asset::{embedded_asset, RenderAssetUsages};
use bevy::image::{ImageAddressMode, ImageFilterMode, ImageSampler, ImageSamplerDescriptor};
use bevy::pbr::{ExtendedMaterial, MaterialExtension};
use bevy::prelude::*;
use bevy::reflect::TypePath;
use bevy::render::render_resource::{
    AsBindGroup, Extent3d, ShaderType, TextureDimension, TextureFormat, TextureViewDescriptor, TextureViewDimension,
};
use bevy::render::storage::ShaderBuffer;
use bevy::shader::ShaderRef;
use dhcook::format::{TexFile, TexFormat};

pub type WorldMaterial = ExtendedMaterial<StandardMaterial, LightmapExt>;

pub struct LightmapPlugin;

impl Plugin for LightmapPlugin {
    fn build(&self, app: &mut App) {
        embedded_asset!(app, "lightmapped.wgsl");
        app.add_plugins(MaterialPlugin::<WorldMaterial>::default());
    }
}

#[derive(ShaderType, Debug, Clone, Copy, Default)]
pub struct LmParams {
    /// x = lightmap exposure, y = ambient floor, z = sun enabled (0/1)
    pub misc: Vec4,
    pub sun_dir: Vec4,
    pub sun_color: Vec4,
}

/// Per-material diffuse layering (Dishonored's generic_PMAT): the diffuse is re-sampled at
/// its own tiling, then optionally blended with a variation layer through a mask channel.
#[derive(ShaderType, Debug, Clone, Copy, Default)]
pub struct LayerParams {
    /// x = diffuse tiling u, y = diffuse tiling v, z = layer enabled (0/1), w = multiply mode (0/1)
    pub diffuse: Vec4,
    /// variation colour (rgb)
    pub color: Vec4,
    /// x = variation tiling, y = mask tiling, z = mask power, w = has variation texture (0/1)
    pub tiling: Vec4,
    /// one-hot mask channel selector
    pub channel: Vec4,
    /// diffuse tint (the StandardMaterial base colour already folds in the texture)
    pub tint: Vec4,
}

#[derive(Asset, AsBindGroup, TypePath, Debug, Clone)]
pub struct LightmapExt {
    #[storage(100, read_only)]
    pub entries: Handle<ShaderBuffer>,
    #[texture(101, dimension = "2d_array")]
    #[sampler(102)]
    pub pages: Handle<Image>,
    #[uniform(103)]
    pub params: LmParams,
    #[texture(104, dimension = "2d_array")]
    #[sampler(105)]
    pub shadow_pages: Handle<Image>,
    #[uniform(106)]
    pub layer: LayerParams,
    /// Same image as the base colour texture (or white), sampled with the layer tiling.
    #[texture(107)]
    #[sampler(108)]
    pub diffuse: Handle<Image>,
    #[texture(109)]
    pub variation: Handle<Image>,
    #[texture(110)]
    pub layer_mask: Handle<Image>,
}

impl MaterialExtension for LightmapExt {
    fn fragment_shader() -> ShaderRef {
        "embedded://dishonored/lightmapped.wgsl".into()
    }
}

/// One storage-buffer entry (3 x vec4) per lightmapped instance.
pub fn entry(lm_rect: [f32; 4], sh_rect: [f32; 4], lm_layer: u32, sh_layer: u32, has_lm: bool, sun: u8) -> [[f32; 4]; 3] {
    let b = |v: u32| f32::from_bits(v);
    [lm_rect, sh_rect, [b(lm_layer), b(sh_layer), b(has_lm as u32), b(sun as u32)]]
}

/// Build a 2D array texture from equally sized pages.
pub fn make_page_array(pages: &[&TexFile]) -> Image {
    let (w, h, mips, format, srgb) = match pages.first() {
        Some(p) => (p.width, p.height, p.mips.len() as u32, p.format, p.srgb),
        None => (4, 4, 1, TexFormat::Rgba8, false),
    };
    let fmt = match (format, srgb) {
        (TexFormat::Bc1, true) => TextureFormat::Bc1RgbaUnormSrgb,
        (TexFormat::Bc1, false) => TextureFormat::Bc1RgbaUnorm,
        (TexFormat::R8, _) => TextureFormat::R8Unorm,
        (TexFormat::Bc4, _) => TextureFormat::Bc4RUnorm,
        (_, true) => TextureFormat::Rgba8UnormSrgb,
        (_, false) => TextureFormat::Rgba8Unorm,
    };
    let layers = pages.len().max(1) as u32;
    let mut data = Vec::new();
    if pages.is_empty() {
        data = vec![0u8; 4 * 4 * 4];
    }
    for p in pages {
        // layer-major: every mip of layer 0, then layer 1, ...
        for m in p.mips.iter().take(mips as usize) {
            data.extend_from_slice(m);
        }
    }
    let mut img = Image::new_uninit(
        Extent3d { width: w, height: h, depth_or_array_layers: layers },
        TextureDimension::D2,
        fmt,
        RenderAssetUsages::RENDER_WORLD,
    );
    img.texture_descriptor.mip_level_count = mips;
    img.data = Some(data);
    img.texture_view_descriptor = Some(TextureViewDescriptor {
        dimension: Some(TextureViewDimension::D2Array),
        ..default()
    });
    img.sampler = ImageSampler::Descriptor(ImageSamplerDescriptor {
        address_mode_u: ImageAddressMode::ClampToEdge,
        address_mode_v: ImageAddressMode::ClampToEdge,
        address_mode_w: ImageAddressMode::ClampToEdge,
        mag_filter: ImageFilterMode::Linear,
        min_filter: ImageFilterMode::Linear,
        mipmap_filter: ImageFilterMode::Linear,
        ..default()
    });
    img
}
