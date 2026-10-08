// Port of Dishonored's procedural sky material (Skies_PMAT): vertical gradient,
// horizon glow, two scrolling cloud layers and a storm layer.
#import bevy_pbr::forward_io::VertexOutput
#import bevy_pbr::mesh_view_bindings::{view, globals}

struct SkyParams {
    top: vec4<f32>,
    bottom: vec4<f32>,
    horizon: vec4<f32>,         // rgb, w = intensity
    clouds_color: vec4<f32>,
    clouds2_color: vec4<f32>,
    storm_color: vec4<f32>,     // rgb, w = intensity
    // x = horizon exponent, y = gradient power, z = clouds visibility, w = clouds2 visibility
    p0: vec4<f32>,
    // x = clouds speed, y = clouds2 speed, z = has storm, w = brightness
    p1: vec4<f32>,
};

@group(#{MATERIAL_BIND_GROUP}) @binding(0) var<uniform> sky: SkyParams;
@group(#{MATERIAL_BIND_GROUP}) @binding(1) var clouds_tex: texture_2d<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(2) var clouds_samp: sampler;
@group(#{MATERIAL_BIND_GROUP}) @binding(3) var clouds2_tex: texture_2d<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(4) var clouds2_samp: sampler;
@group(#{MATERIAL_BIND_GROUP}) @binding(5) var storm_tex: texture_2d<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(6) var storm_samp: sampler;

@fragment
fn fragment(in: VertexOutput) -> @location(0) vec4<f32> {
    let dir = normalize(in.world_position.xyz - view.world_position);
    let elev = clamp(dir.y, -1.0, 1.0);
    let t = pow(clamp(elev * 0.5 + 0.5, 0.0, 1.0), max(sky.p0.y, 0.05));
    var col = mix(sky.bottom.rgb, sky.top.rgb, t);

    // horizon glow
    let h = pow(clamp(1.0 - abs(elev), 0.0, 1.0), max(sky.p0.x, 0.1));
    col = col + sky.horizon.rgb * sky.horizon.w * h * 0.35;

    // clouds: planar projection of the view direction gives stable UVs on any dome
    let proj = dir.xz / max(elev + 0.35, 0.08);
    let time = globals.time;
    let uv1 = proj * 0.18 + vec2<f32>(time * sky.p1.x * 0.004, 0.0);
    let uv2 = proj * 0.11 + vec2<f32>(time * sky.p1.y * 0.004, time * 0.0015);
    let c1 = textureSample(clouds_tex, clouds_samp, uv1).r;
    let c2 = textureSample(clouds2_tex, clouds2_samp, uv2).r;
    let above = smoothstep(-0.05, 0.12, elev);
    col = mix(col, sky.clouds_color.rgb, clamp(c1 * sky.p0.z * 2.0, 0.0, 1.0) * above);
    col = mix(col, sky.clouds2_color.rgb, clamp(c2 * sky.p0.w * sky.clouds2_color.a * 1.5, 0.0, 1.0) * above);

    if (sky.p1.z > 0.5) {
        let s = textureSample(storm_tex, storm_samp, in.uv).rgb;
        col = col + s * sky.storm_color.rgb * sky.storm_color.w * 0.25 * above;
    }
    return vec4<f32>(col * sky.p1.w, 1.0);
}
