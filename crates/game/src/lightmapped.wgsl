// StandardMaterial extension applying Dishonored's baked lighting:
//  - directional lightmaps (flattened to irradiance pages) for static lights
//  - the dominant directional light (sun) with its precomputed static shadow maps
// Per-instance data lives in a storage buffer indexed by MeshTag: 3 vec4 per entry
//   [0] lightmap rect (bias.xy, scale.zw)
//   [1] sun shadow rect (bias.xy, scale.zw)
//   [2] bitcast u32: x = lightmap layer, y = shadow layer,
//       z = lighting source (0 = none, 1 = lightmap, 2 = environment colour stored in [1].rgb),
//       w = sun mode (0 = no sun, 1 = shadow-mapped, 2 = fully lit)
#import bevy_pbr::{
    pbr_fragment::pbr_input_from_standard_material,
    pbr_functions::alpha_discard,
    mesh_functions,
    mesh_view_bindings::view,
    pbr_types,
}

#ifdef PREPASS_PIPELINE
#import bevy_pbr::{
    prepass_io::{VertexOutput, FragmentOutput},
    pbr_deferred_functions::deferred_output,
}
#else
#import bevy_pbr::{
    forward_io::{VertexOutput, FragmentOutput},
    pbr_functions::{apply_pbr_lighting, main_pass_post_lighting_processing},
}
#endif

struct LmParams {
    // x = lightmap exposure, y = ambient floor, z = sun enabled
    misc: vec4<f32>,
    // direction the sun light travels (world space)
    sun_dir: vec4<f32>,
    // linear rgb * illuminance (lux)
    sun_color: vec4<f32>,
};

@group(#{MATERIAL_BIND_GROUP}) @binding(100) var<storage, read> lm_entries: array<vec4<f32>>;
@group(#{MATERIAL_BIND_GROUP}) @binding(101) var lm_pages: texture_2d_array<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(102) var lm_sampler: sampler;
@group(#{MATERIAL_BIND_GROUP}) @binding(103) var<uniform> lm_params: LmParams;
@group(#{MATERIAL_BIND_GROUP}) @binding(104) var sh_pages: texture_2d_array<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(105) var sh_sampler: sampler;

struct LayerParams {
    diffuse: vec4<f32>,
    color: vec4<f32>,
    tiling: vec4<f32>,
    channel: vec4<f32>,
    tint: vec4<f32>,
};
@group(#{MATERIAL_BIND_GROUP}) @binding(106) var<uniform> layer: LayerParams;
@group(#{MATERIAL_BIND_GROUP}) @binding(107) var layer_diffuse: texture_2d<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(108) var layer_sampler: sampler;
@group(#{MATERIAL_BIND_GROUP}) @binding(109) var layer_variation: texture_2d<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(110) var layer_mask: texture_2d<f32>;

const PI: f32 = 3.141592653589793;

@fragment
fn fragment(
    in: VertexOutput,
    @builtin(front_facing) is_front: bool,
) -> FragmentOutput {
    var pbr_input = pbr_input_from_standard_material(in, is_front);
#ifdef VERTEX_UVS_A
    // Opaque layered materials: diffuse at its own tiling (the standard path used the
    // normal-map tiling), then the masked variation layer.
    if layer.diffuse.z > 0.5 {
        let tint = layer.tint;
        var rgb = textureSample(layer_diffuse, layer_sampler, in.uv * layer.diffuse.xy).rgb;
        var v = layer.color.rgb;
        if layer.tiling.w > 0.5 {
            v = v * textureSample(layer_variation, layer_sampler, in.uv * layer.tiling.x).rgb;
        }
        let m = pow(clamp(dot(textureSample(layer_mask, layer_sampler, in.uv * layer.tiling.y), layer.channel), 0.0, 1.0), layer.tiling.z);
        if layer.diffuse.w > 0.5 {
            rgb = rgb * tint.rgb * mix(vec3<f32>(1.0), v, m);
        } else {
            rgb = mix(rgb * tint.rgb, v, m);
        }
        pbr_input.material.base_color = vec4<f32>(rgb, pbr_input.material.base_color.a);
    }
#endif
    pbr_input.material.base_color = alpha_discard(pbr_input.material, pbr_input.material.base_color);

#ifdef PREPASS_PIPELINE
    let out = deferred_output(in, pbr_input);
#else
    var out: FragmentOutput;
    let tag = mesh_functions::get_tag(in.instance_index);
    let info = bitcast<vec4<u32>>(lm_entries[tag * 3u + 2u]);
    var baked = vec3<f32>(0.0);
    var lm_weight = 0.0;
    var sun_vis = select(0.0, 1.0, info.w != 0u);
#ifdef VERTEX_UVS_B
    let lr = lm_entries[tag * 3u];
    let sr = lm_entries[tag * 3u + 1u];
    // sampled unconditionally to stay in uniform control flow
    baked = textureSample(lm_pages, lm_sampler, in.uv_b * lr.zw + lr.xy, i32(info.x)).rgb;
    let shadow = textureSample(sh_pages, sh_sampler, in.uv_b * sr.zw + sr.xy, i32(info.y)).r;
    lm_weight = select(0.0, 1.0, info.z == 1u);
    sun_vis = select(sun_vis, shadow, info.w == 1u);
    // baked surfaces get their indirect light from the lightmap, not the ambient term
    pbr_input.diffuse_occlusion = pbr_input.diffuse_occlusion * (1.0 - lm_weight);
#endif
    // dynamic actors (characters, held items): CPU-estimated light environment
    let env_weight = select(0.0, 1.0, info.z == 2u);
    baked = mix(baked, lm_entries[tag * 3u + 1u].rgb, env_weight);
    lm_weight = max(lm_weight, env_weight);
    pbr_input.diffuse_occlusion = pbr_input.diffuse_occlusion * (1.0 - env_weight);
    let unlit = (pbr_input.material.flags & pbr_types::STANDARD_MATERIAL_FLAGS_UNLIT_BIT) != 0u;
    out.color = apply_pbr_lighting(pbr_input);
    let albedo = pbr_input.material.base_color.rgb * (1.0 - pbr_input.material.metallic);
    let irradiance = max(baked, vec3<f32>(lm_params.misc.y)) * lm_params.misc.x;
    let n_dot_l = max(dot(pbr_input.N, -lm_params.sun_dir.xyz), 0.0);
    let sun = lm_params.sun_color.rgb * (n_dot_l * sun_vis * lm_params.misc.z / PI);
    out.color = vec4<f32>(out.color.rgb + albedo * (irradiance * lm_weight + sun) * view.exposure, out.color.a);
    if unlit {
        // emissive-only materials (VFX, sky fillers): no lighting at all
        out.color = pbr_input.material.base_color;
    }
    out.color = main_pass_post_lighting_processing(pbr_input, out.color);
#endif
    return out;
}
