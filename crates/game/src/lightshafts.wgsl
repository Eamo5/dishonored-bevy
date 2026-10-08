// UE3's light shafts (`TDownsampleLightShaftsPixelShader<FALSE>`, `FBlurLightShaftsPixelShader`,
// `FApplyLightShaftsPixelShader`, as disassembled):
//   downsample: half-size; rgb the bloom (the bright parts near the light, beyond half the
//     occlusion range), a the occlusion (near things dark), both faded at the screen's edges;
//   blur: 64 taps along the way to the light (two interleaved chains, weights falling towards
//     it), out x 1/64; run twice;
//   apply: the scene darkened where the shafts are occluded (to `OcclusionMaskDarkness`, less
//     so away from the light), the tinted bloom added where the scene is dark.
#import bevy_core_pipeline::fullscreen_vertex_shader::FullscreenVertexOutput

struct Shafts {
    // the light's place on the screen (uv x aspect: xy), its fade (z: 0 shown, 1 gone)
    origin: vec4<f32>,
    // (1, w/h, 1, h/w): `AspectRatioAndInvAspectRatio`
    aspect: vec4<f32>,
    // 1 / occlusion range (m), bloom scale, 1, occlusion mask darkness: `LightShaftParameters`
    params: vec4<f32>,
    // bloom tint (rgb), bloom threshold (a)
    tint: vec4<f32>,
    // bloom screen blend threshold, camera near plane, radial blur (0-1), -
    extra: vec4<f32>,
};

@group(0) @binding(0) var src: texture_2d<f32>;
@group(0) @binding(1) var samp: sampler;
@group(0) @binding(2) var<uniform> s: Shafts;
@group(0) @binding(3) var shafts: texture_2d<f32>;
#ifdef MULTISAMPLED
@group(0) @binding(4) var depth: texture_depth_multisampled_2d;
#else
@group(0) @binding(4) var depth: texture_depth_2d;
#endif

// a scene texel's distance (m): reverse infinite depth, near / depth (the sky as far as it gets)
fn distance_at(p: vec2<i32>) -> f32 {
    let dims = vec2<i32>(textureDimensions(depth));
    let d = textureLoad(depth, clamp(p, vec2<i32>(0), dims - vec2<i32>(1)), 0);
    return s.extra.y / max(d, 1e-9);
}

@fragment
fn downsample(in: FullscreenVertexOutput) -> @location(0) vec4<f32> {
    let uv = in.uv;
    // the screen's edges: 1 - (1 - 8 x(1-x) y(1-y))^4
    let p = uv.x * (1.0 - uv.x) * uv.y * (1.0 - uv.y);
    let edge_off = pow(1.0 - 8.0 * p, 4.0);
    let edge = 1.0 - edge_off;
    // four scene texels: their colour and distance
    let full = vec2<f32>(textureDimensions(src));
    let base = vec2<i32>(floor(uv * full - vec2<f32>(0.5)));
    var c = vec3<f32>(0.0);
    var occl = 0.0;
    var far = 0.0;
    let range = 1.0 / max(s.params.x, 1e-6);
    for (var y = 0; y < 2; y++) {
        for (var x = 0; x < 2; x++) {
            let t = base + vec2<i32>(x, y);
            c += textureLoad(src, clamp(t, vec2<i32>(0), vec2<i32>(full) - vec2<i32>(1)), 0).rgb;
            let z = distance_at(t);
            occl += clamp(z * s.params.x, 0.0, 1.0);
            far += clamp((z - 0.5 * range) * s.params.x, 0.0, 1.0);
        }
    }
    c *= 0.25;
    occl *= 0.25;
    far *= 0.25;
    // the bloom: the brightness over the threshold, beyond half the range, near the light
    let lum = max(dot(c, vec3<f32>(0.3, 0.59, 0.11)), 6.10352e-5);
    let over = lum - s.tint.a;
    var bloom = vec3<f32>(0.0);
    if over >= 0.0 {
        bloom = c * s.params.y / lum * over * 2.0 * far;
    }
    let to = s.origin.xy - uv * s.aspect.zw;
    let near_light = 1.0 - clamp(length(to) * 5.0, 0.0, 1.0);
    return vec4<f32>(bloom * edge * near_light * near_light * 0.25, max(occl, edge_off));
}

@fragment
fn blur(in: FullscreenVertexOutput) -> @location(0) vec4<f32> {
    let uv = in.uv;
    // the way to the light (aspect-corrected), as far as min(d, sqrt(d / 2)), back in uv
    var v = s.origin.xy - uv * s.aspect.zw;
    let d = length(v);
    let half_d = 0.5 * d;
    v = v / max(half_d, 1e-6);
    v = v * min(half_d, 0.5 * sqrt(half_d)) * s.extra.z;
    let step = v * s.aspect.xy;
    var a = uv;
    var b = uv + step / 64.0;
    var acc = vec4<f32>(0.0);
    var t0 = 2.0;
    var t1 = 1.96875;
    for (var i = 0; i < 32; i++) {
        // rgb weighted t (4t^2 under 1/4), a by t
        let w0 = select(4.0 * t0 * t0, t0, 4.0 * t0 * t0 - t0 >= 0.0);
        let w1 = select(4.0 * t1 * t1, t1, 4.0 * t1 * t1 - t1 >= 0.0);
        let s0 = textureSampleLevel(src, samp, clamp(a, vec2<f32>(0.0), vec2<f32>(1.0)), 0.0);
        let s1 = textureSampleLevel(src, samp, clamp(b, vec2<f32>(0.0), vec2<f32>(1.0)), 0.0);
        acc += s0 * vec4<f32>(w0, w0, w0, t0) + s1 * vec4<f32>(w1, w1, w1, t1);
        t0 -= 0.0625;
        t1 -= 0.0625;
        a += step / 32.0;
        b += step / 32.0;
    }
    return acc / 64.0;
}

@fragment
fn apply(in: FullscreenVertexOutput) -> @location(0) vec4<f32> {
    let uv = in.uv;
    let scene = textureSample(src, samp, uv);
    let sh = textureSample(shafts, samp, uv);
    let away = clamp(length(s.origin.xy - uv * s.aspect.zw) * 0.5, 0.0, 1.0);
    // the occluded parts darkened (less so away from the light, none as the light fades)
    let mask = mix(s.params.w, s.params.z, sh.a * sh.a);
    var k = mix(mask, 1.0, away);
    k = k + s.origin.z * (1.0 - k);
    // the bloom, tinted, where the scene is dark
    let lum = dot(scene.rgb, vec3<f32>(0.3, 0.59, 0.11));
    let blend = clamp(exp2(-3.0 * lum) * s.extra.x, 0.0, 1.0) * (1.0 - s.origin.z);
    return vec4<f32>(scene.rgb * k + sh.rgb * s.tint.rgb * blend * 4.0, scene.a);
}
