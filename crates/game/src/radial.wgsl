// A ring filling round (the HUD's skip gauge: its `_mask_mc` wipe): the texture shows from
// `start` (turns clockwise from the top) over `share` of the circle.
#import bevy_ui::ui_vertex_output::UiVertexOutput
#ifdef SRGB_OUTPUT
#import bevy_render::color_operations::linear_to_srgb
#endif

// (share, start, alpha, -)
@group(1) @binding(0) var<uniform> params: vec4<f32>;
@group(1) @binding(1) var tex: texture_2d<f32>;
@group(1) @binding(2) var samp: sampler;

@fragment
fn fragment(in: UiVertexOutput) -> @location(0) vec4<f32> {
    let c = textureSample(tex, samp, in.uv);
    let d = in.uv - vec2<f32>(0.5);
    var a = atan2(d.x, -d.y) / 6.2831853;
    if (a < 0.0) {
        a = a + 1.0;
    }
    let rel = fract(a - params.y + 1.0);
    let shown = select(0.0, 1.0, rel <= params.x);
    // (gamma-encoded for a camera compositing in sRGB)
#ifdef SRGB_OUTPUT
    return vec4<f32>(linear_to_srgb(c.rgb), c.a * params.z * shown);
#else
    return vec4<f32>(c.rgb, c.a * params.z * shown);
#endif
}
