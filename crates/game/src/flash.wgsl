// A bitmap of the original interface movies with its Flash colour transform: multiplied, then
// added to, in the movie's (gamma) colour space.
#import bevy_ui::ui_vertex_output::UiVertexOutput

// multiply terms (rgba), add terms (rgba, 0..1), the part of the bitmap shown (u0, v0, u1, v1),
// options (x: it tiles; y: its colours are premultiplied by its alpha, a picture drawn by a
// camera)
@group(1) @binding(0) var<uniform> mult: vec4<f32>;
@group(1) @binding(1) var<uniform> add: vec4<f32>;
@group(1) @binding(2) var<uniform> uvr: vec4<f32>;
@group(1) @binding(3) var<uniform> opts: vec4<f32>;
@group(1) @binding(4) var tex: texture_2d<f32>;
@group(1) @binding(5) var samp: sampler;
// a mask it is cut to as drawn (`setMask`, a mask layer turned to it): the quad (0..1) into the
// mask's frame (its columns; its offset, z: there is one) and the mask's rectangle there
@group(1) @binding(6) var<uniform> clip_a: vec4<f32>;
@group(1) @binding(7) var<uniform> clip_b: vec4<f32>;
@group(1) @binding(8) var<uniform> clip_r: vec4<f32>;

fn to_srgb(c: vec3<f32>) -> vec3<f32> {
    let lo = c * 12.92;
    let hi = 1.055 * pow(max(c, vec3<f32>(0.0)), vec3<f32>(1.0 / 2.4)) - 0.055;
    return select(hi, lo, c <= vec3<f32>(0.0031308));
}

fn to_linear(c: vec3<f32>) -> vec3<f32> {
    let lo = c / 12.92;
    let hi = pow((max(c, vec3<f32>(0.0)) + 0.055) / 1.055, vec3<f32>(2.4));
    return select(hi, lo, c <= vec3<f32>(0.04045));
}

@fragment
fn fragment(in: UiVertexOutput) -> @location(0) vec4<f32> {
    if (clip_b.z > 0.5) {
        let q = vec2<f32>(clip_a.x * in.uv.x + clip_a.z * in.uv.y + clip_b.x, clip_a.y * in.uv.x + clip_a.w * in.uv.y + clip_b.y);
        if (q.x < clip_r.x || q.y < clip_r.y || q.x > clip_r.z || q.y > clip_r.w) {
            discard;
        }
    }
    var uv = mix(uvr.xy, uvr.zw, in.uv);
    if (opts.x > 0.5) {
        uv = fract(uv);
    }
    var c = textureSample(tex, samp, uv);
    if (opts.y > 0.5 && c.a > 0.0001) {
        c = vec4<f32>(c.rgb / c.a, c.a);
    }
    let rgb = clamp(to_srgb(c.rgb) * mult.rgb + add.rgb, vec3<f32>(0.0), vec3<f32>(1.0));
    let a = clamp(c.a * mult.a + add.a, 0.0, 1.0);
    return vec4<f32>(to_linear(rgb), a);
}
