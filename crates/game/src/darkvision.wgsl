// Dark Vision's souls: the original `DarkVision_PMAT` pixel shader (`TPpMaterialPixelShader`,
// as `dhtool mapshader` translates it) in Bevy's mesh pipeline:
//   - two panners (+-0.05 / s) over the texture coordinates: `Cloud_64_d` distorts where
//     `Smoke_01_d` is read, its green stirring the behind colour (+ 0.3 x);
//   - shown where the scene's depth is not more than 5 units in front of the surface
//     (`Full_Color`), else hidden (`Behind_color` + noise);
//   - opacity 0.7 x ((n.z + 1)^2 / 2 + 1/2) from the view-space normal (UE view space, z
//     forward: more opaque at the silhouette), 0.8 when hidden;
//   - all fading out (smoothstep) between the power's fade distance and its reach.
// The original writes (colour x fade, opacity x fade) into a mask the power's material mixes
// over the graded scene; alpha blending over the graded image is the same mix.
#import bevy_pbr::forward_io::VertexOutput
#import bevy_pbr::mesh_view_bindings::view
#import bevy_pbr::prepass_utils
#import bevy_pbr::view_transformations

struct Soul {
    full: vec4<f32>,
    behind: vec4<f32>,
    // fade start, fade end (metres), time (s), overall opacity
    misc: vec4<f32>,
};
@group(3) @binding(0) var<uniform> soul: Soul;
@group(3) @binding(1) var cloud: texture_2d<f32>;
@group(3) @binding(2) var smoke: texture_2d<f32>;
@group(3) @binding(3) var samp: sampler;

// one UE unit in metres (as cooked)
const UU: f32 = 0.01;

@fragment
fn fragment(in: VertexOutput) -> @location(0) vec4<f32> {
    let t = soul.misc.z;
#ifdef VERTEX_UVS_A
    let uv = in.uv;
#else
    let uv = vec2<f32>(0.0);
#endif
    // `Periodic(Time x 0.05)` on both axes, and its opposite
    let pan_a = vec2<f32>(fract(t * 0.05));
    let pan_b = vec2<f32>(fract(t * -0.05));
    let n = textureSample(cloud, samp, uv + pan_a).xy;
    let s = textureSample(smoke, samp, n * 0.4 + uv + pan_b).y;
    let behind = soul.behind.rgb + vec3<f32>(s * 0.3);

    // this surface's depth against the scene's (both view distances)
    let z = -view_transformations::position_world_to_view(in.world_position.xyz).z;
    let d = prepass_utils::prepass_depth(in.position, 0u);
    let scene_z = -view_transformations::depth_ndc_to_view_z(max(d, 1e-9));
    let shown = scene_z >= z - 5.0 * UU;

    // fading out over the power's last stretch
    let k = clamp((z - soul.misc.x) / max(soul.misc.y - soul.misc.x, 1e-3), 0.0, 1.0);
    let fade = 1.0 - k * k * (3.0 - 2.0 * k);

    // the normal's view-space depth (UE: z forward; Bevy's view space looks down -z)
    let nz = -normalize((view.view_from_world * vec4<f32>(normalize(in.world_normal), 0.0)).xyz).z;
    let rim = ((nz + 1.0) * (nz + 1.0) * 0.5 + 0.5) * 0.7;

    let colour = select(behind, soul.full.rgb, shown);
    let a = select(0.8, rim, shown) * fade;
    return vec4<f32>(colour * fade, a);
}
