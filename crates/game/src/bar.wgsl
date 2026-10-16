// A meter's fill (the HUD's health and mana shards, their `mask_mc` scaled to the amount): the
// texture shows from `level` (uv, top 0) down to the bottom, the line slanting by `slope` across
// (a piece turned in the mask's frame), its colour times `bright` (the regeneration's half).
#import bevy_ui::ui_vertex_output::UiVertexOutput
#ifdef SRGB_OUTPUT
#import bevy_render::color_operations::linear_to_srgb
#endif

// (level, alpha, slope, bright); a colour added (a drink's flash: rgb, -)
@group(1) @binding(0) var<uniform> params: vec4<f32>;
@group(1) @binding(1) var tex: texture_2d<f32>;
@group(1) @binding(2) var samp: sampler;
@group(1) @binding(3) var<uniform> add: vec4<f32>;

@fragment
fn fragment(in: UiVertexOutput) -> @location(0) vec4<f32> {
    let c = textureSample(tex, samp, in.uv);
    let shown = select(0.0, 1.0, in.uv.y >= params.x + params.z * in.uv.x);
    let rgb = c.rgb * params.w + add.rgb;
    // (gamma-encoded for a camera compositing in sRGB)
#ifdef SRGB_OUTPUT
    return vec4<f32>(linear_to_srgb(max(rgb, vec3<f32>(0.0))), c.a * params.y * shown);
#else
    return vec4<f32>(rgb, c.a * params.y * shown);
#endif
}
