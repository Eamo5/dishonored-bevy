// The finished frame put gamma-encoded for the interface to blend over (`gammaout.rs`): as
// Scaleform draws over UE3's finished, gamma-encoded frame, the menus' veils and the HUD's shades
// blend in gamma space; the blit to the window decodes it.
#import bevy_core_pipeline::fullscreen_vertex_shader::FullscreenVertexOutput
#import bevy_render::color_operations::linear_to_srgb

@group(0) @binding(0) var screen: texture_2d<f32>;

@fragment
fn fragment(in: FullscreenVertexOutput) -> @location(0) vec4<f32> {
    let c = textureLoad(screen, vec2<i32>(in.position.xy), 0);
    return vec4<f32>(linear_to_srgb(clamp(c.rgb, vec3<f32>(0.0), vec3<f32>(1.0))), c.a);
}
