// The interactables' golden rim (`golden_focus_PMAT`'s base-pass pixel shader, whose shader
// map has no vertex shader to pair it with): f = (1 - saturate(N.V))^power, emissive
// colour x visibility x f, opacity saturate(f x visibility).
#import bevy_pbr::{forward_io::VertexOutput, mesh_view_bindings::view}

// colour (`F_Fresnel_Color`), (power, visibility, -, -)
@group(#{MATERIAL_BIND_GROUP}) @binding(0) var<uniform> color: vec4<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(1) var<uniform> params: vec4<f32>;

@fragment
fn fragment(in: VertexOutput) -> @location(0) vec4<f32> {
    let n = normalize(in.world_normal);
    let v = normalize(view.world_position.xyz - in.world_position.xyz);
    let f = pow(1.0 - clamp(abs(dot(n, v)), 0.0, 1.0), params.x);
    return vec4<f32>(color.rgb * params.y * f, clamp(f * params.y, 0.0, 1.0));
}
