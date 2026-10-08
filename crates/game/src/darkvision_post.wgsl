// Dark Vision's post-process node, the original materials' pixel shaders by hand, over the
// graded image in display space:
//   the power's (`PPG_DarkVisionFinal_Mat`): the souls' mask mixed over the scene, darkened
//     towards the corners by the gradient's green, the eye opening from the middle as Alpha
//     goes to 1 (eyelid colour where saturate(red - 1 + 2 Alpha) is 0);
//   the eyelid's (`PPG_EyeLidFinal_Mat`): the eyelid colour closing in, top and bottom, as
//     Alpha goes to 1 (saturate(alpha - 1 + 2 Alpha)).
#import bevy_core_pipeline::fullscreen_vertex_shader::FullscreenVertexOutput

@group(0) @binding(0) var scene: texture_2d<f32>;
@group(0) @binding(1) var mask: texture_2d<f32>;
@group(0) @binding(2) var gradient: texture_2d<f32>;
@group(0) @binding(3) var samp: sampler;

struct Eye {
    eyelid: vec4<f32>,
    // material (1 eyelid, 2 power), its alpha
    mode: vec4<f32>,
};
@group(0) @binding(4) var<uniform> eye: Eye;

fn to_display(c: vec3<f32>) -> vec3<f32> {
    let lo = c * 12.92;
    let hi = 1.055 * pow(c, vec3<f32>(1.0 / 2.4)) - 0.055;
    return select(hi, lo, c <= vec3<f32>(0.0031308));
}

fn to_linear(c: vec3<f32>) -> vec3<f32> {
    let lo = c / 12.92;
    let hi = pow((c + 0.055) / 1.055, vec3<f32>(2.4));
    return select(hi, lo, c <= vec3<f32>(0.04045));
}

@fragment
fn fragment(in: FullscreenVertexOutput) -> @location(0) vec4<f32> {
    let src = textureSample(scene, samp, in.uv);
    let s = to_display(clamp(src.rgb, vec3<f32>(0.0), vec3<f32>(1.0)));
    let g = textureSample(gradient, samp, in.uv);
    let alpha = eye.mode.y;
    var c = s;
    if eye.mode.x > 1.5 {
        let m = textureSample(mask, samp, in.uv);
        let souls = mix(s, m.rgb, m.a);
        let k = clamp(g.r - (1.0 - 2.0 * alpha), 0.0, 1.0);
        c = mix(eye.eyelid.rgb, g.g * souls, k);
    } else if eye.mode.x > 0.5 {
        let k = clamp(g.a - (1.0 - 2.0 * alpha), 0.0, 1.0);
        c = mix(s, eye.eyelid.rgb, k);
    }
    return vec4<f32>(to_linear(c), src.a);
}
