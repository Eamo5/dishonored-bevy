// Arkane's level post-processing (WorldInfo.m_ArkDefaultPpSettings) as the original
// FArkPpDofUber pixel shader applies it, after a linear scene colour (no tonemapper):
//   bloom (added in linear), then the colour LUT: scene colour clamped to [0, 1], its square
//   root addresses a 16x16x16 table (256x16 texels, blue slices side by side) baked by the
//   original FArkPpDofLutBlender shader (exposure, colour balance, gamma, contrast), giving
//   the display colour; film grain is added on top.
#import bevy_core_pipeline::fullscreen_vertex_shader::FullscreenVertexOutput

@group(0) @binding(0) var screen: texture_2d<f32>;
@group(0) @binding(1) var samp: sampler;

struct ArkGrade {
    // bloom tint (rgb), bloom scale
    bloom: vec4<f32>,
    // x = time, y = bloom threshold, z = enabled, w = film grain amplitude (display units)
    misc: vec4<f32>,
    // 256x16 RGBA8 texels, four per entry
    lut: array<vec4<u32>, 1024>,
};
@group(0) @binding(2) var<uniform> g: ArkGrade;

fn to_linear(c: vec3<f32>) -> vec3<f32> {
    let lo = c / 12.92;
    let hi = pow((c + 0.055) / 1.055, vec3<f32>(2.4));
    return select(hi, lo, c <= vec3<f32>(0.04045));
}

fn luma(c: vec3<f32>) -> f32 {
    return dot(c, vec3<f32>(0.299, 0.587, 0.114));
}

fn texel(x: i32, y: i32) -> vec4<f32> {
    let i = u32(clamp(y, 0, 15) * 256 + clamp(x, 0, 255));
    return unpack4x8unorm(g.lut[i / 4u][i % 4u]);
}

// bilinear filtering of the LUT texture at texel-space coordinates (centres at i + 0.5)
fn lut_bilinear(x: f32, y: f32) -> vec4<f32> {
    let x0 = floor(x);
    let y0 = floor(y);
    let fx = x - x0;
    let fy = y - y0;
    let a = mix(texel(i32(x0), i32(y0)), texel(i32(x0) + 1, i32(y0)), fx);
    let b = mix(texel(i32(x0), i32(y0) + 1), texel(i32(x0) + 1, i32(y0) + 1), fx);
    return mix(a, b, fy);
}

fn hash(p: vec2<f32>) -> f32 {
    let q = fract(p * vec2<f32>(443.8975, 397.2973));
    let d = dot(q, q.yx + 19.19);
    return fract((q.x + q.y) * d);
}

@fragment
fn fragment(in: FullscreenVertexOutput) -> @location(0) vec4<f32> {
    let src = textureSample(screen, samp, in.uv);
    if g.misc.z < 0.5 {
        return src;
    }
    var lin = max(src.rgb, vec3<f32>(0.0));

    // bloom: thresholded glow from two rings of taps
    if g.bloom.w > 0.0 {
        let dims = vec2<f32>(textureDimensions(screen));
        let px = 1.0 / dims;
        var glow = vec3<f32>(0.0);
        var wsum = 0.0;
        for (var ring = 1; ring <= 2; ring++) {
            let r = f32(ring) * 0.0045 * dims.y;
            for (var i = 0; i < 8; i++) {
                let a = (f32(i) + f32(ring) * 0.5) * 0.7853982;
                let uv = in.uv + vec2<f32>(cos(a), sin(a)) * r * px;
                let s = textureSample(screen, samp, uv).rgb;
                let bright = max(luma(s) - g.misc.y, 0.0) / max(luma(s), 1e-4);
                let w = 1.0 / f32(ring);
                glow += s * bright * w;
                wsum += w;
            }
        }
        lin += glow / wsum * g.bloom.rgb * g.bloom.w;
    }

    // LUT addressing of FArkPpDofUber: sqrt-encoded, red within a slice clamped to
    // [0.1, 14.9] texels, linear blend between the two blue slices
    let s = clamp(sqrt(lin), vec3<f32>(0.0), vec3<f32>(1.0));
    let b = s.b * 14.9999;
    let slice = floor(b);
    let bf = b - slice;
    let r = min(max(s.r * 15.0, 0.1), 14.9);
    let x = slice * 16.0 + r;
    let y = s.g * 15.0;
    let c = mix(lut_bilinear(x, y), lut_bilinear(x + 16.0, y), bf);
    var display = c.rgb;
    // film grain
    if g.misc.w > 0.0 {
        let n = hash(in.position.xy + fract(g.misc.x * 13.37) * 1000.0);
        display += vec3<f32>(n * 2.0 - 1.0) * g.misc.w;
    }
    return vec4<f32>(to_linear(clamp(display, vec3<f32>(0.0), vec3<f32>(1.0))), src.a);
}
