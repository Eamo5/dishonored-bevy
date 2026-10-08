// A meter's fill (the HUD's health and mana shards, their `mask_mc` scaled to the amount): the
// texture shows from `level` (uv, top 0) down to the bottom.
#import bevy_ui::ui_vertex_output::UiVertexOutput

// (level, alpha, -, -)
@group(1) @binding(0) var<uniform> params: vec4<f32>;
@group(1) @binding(1) var tex: texture_2d<f32>;
@group(1) @binding(2) var samp: sampler;

@fragment
fn fragment(in: UiVertexOutput) -> @location(0) vec4<f32> {
    let c = textureSample(tex, samp, in.uv);
    let shown = select(0.0, 1.0, in.uv.y >= params.x);
    return vec4<f32>(c.rgb, c.a * params.y * shown);
}
