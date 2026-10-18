// The bloom parts' reduction, blur and compose, as the original's global shaders compute them
// (FBloomDownSamplePixelShader, TBloomBlurPixelShader<5>, FBloomComposePixelShader).
#import bevy_core_pipeline::fullscreen_vertex_shader::FullscreenVertexOutput

@group(0) @binding(0) var src: texture_2d<f32>;
@group(0) @binding(1) var bloom: texture_2d<f32>;
@group(0) @binding(2) var samp: sampler;

struct Bloom {
    // the level's m_Tint x m_Scale (rgb), -m_Threshold x m_Scale (w)
    tint: vec4<f32>,
    // the source's texel (xy), the blur's direction (zw)
    texel: vec4<f32>,
    // (debugging: x > 0, the bloom alone times x)
    debug: vec4<f32>,
};
@group(0) @binding(3) var<uniform> b: Bloom;

// a bloom part's LogLuv texel: (u', v', log-luminance high byte, low byte)
fn decode(t: vec4<f32>) -> vec3<f32> {
    if t.y <= 0.0 {
        return vec3<f32>(0.0);
    }
    let y = exp2((t.z * 255.0 + t.w - 127.0) * 0.5);
    let z = y / t.y;
    let xyz = vec3<f32>(t.x * z, y, z);
    let rgb = vec3<f32>(
        dot(xyz, vec3<f32>(6.0014, -1.332, 0.3008)),
        dot(xyz, vec3<f32>(-2.7008, 3.1029, -1.0882)),
        dot(xyz, vec3<f32>(-1.7996, -5.7721, 5.6268)),
    );
    return max(rgb, vec3<f32>(0.0));
}

// a quarter of the size: four bilinear taps a source texel off the middle (each the mean of
// a 2x2 block), decoded, averaged, tinted, less the threshold
@fragment
fn downsample(in: FullscreenVertexOutput) -> @location(0) vec4<f32> {
    let d = b.texel.xy;
    let c = decode(textureSample(src, samp, in.uv + vec2<f32>(-d.x, -d.y)))
        + decode(textureSample(src, samp, in.uv + vec2<f32>(d.x, -d.y)))
        + decode(textureSample(src, samp, in.uv + vec2<f32>(-d.x, d.y)))
        + decode(textureSample(src, samp, in.uv + vec2<f32>(d.x, d.y)));
    return vec4<f32>(max(c * b.tint.rgb * 0.25 + b.tint.w, vec3<f32>(0.0)), 1.0);
}

// five taps along the direction: 0, +-1.3973, +-3.3973 texels
@fragment
fn blur(in: FullscreenVertexOutput) -> @location(0) vec4<f32> {
    let d = b.texel.xy * b.texel.zw;
    let c = textureSample(src, samp, in.uv - 3.3973148 * d) * 0.11486951
        + textureSample(src, samp, in.uv - 1.3973147 * d) * 0.26431197
        + textureSample(src, samp, in.uv) * 0.24163704
        + textureSample(src, samp, in.uv + 1.3973147 * d) * 0.26431197
        + textureSample(src, samp, in.uv + 3.3973148 * d) * 0.11486951;
    return vec4<f32>(c.rgb, 1.0);
}

// the scene and its bloom
@fragment
fn compose(in: FullscreenVertexOutput) -> @location(0) vec4<f32> {
    let s = textureSample(src, samp, in.uv);
    let glow = textureSample(bloom, samp, in.uv).rgb;
    if b.debug.x > 0.0 {
        if b.debug.x > 999.0 {
            return vec4<f32>(1.0, 0.0, 1.0, 1.0);
        }
        return vec4<f32>(glow * b.debug.x, s.a);
    }
    return vec4<f32>(s.rgb + glow, s.a);
}
