// A lens flare's element (`LensFlare_PMAT`, its glowing permutation, through
// `FLensFlareVertexFactory`): a round glow, max(0, 1 - 2|uv - 1/2|)^power, dimmed away from the
// screen's middle (1 - radial distance x factor), in the element's colour times the material's,
// pulsing |range sin(2 pi speed t) + base| when it glows; clamped to the opacity range, times
// the element's opacity, added to the scene (alpha 0).
#import bevy_pbr::forward_io::VertexOutput
#import bevy_pbr::mesh_view_bindings::globals

struct Flare {
    color: vec4<f32>,
    // power, radial dimming, minimum and maximum opacity
    shape: vec4<f32>,
    // speed, range, base, on
    glow: vec4<f32>,
};

@group(#{MATERIAL_BIND_GROUP}) @binding(0) var<uniform> flare: Flare;

@fragment
fn fragment(in: VertexOutput) -> @location(0) vec4<f32> {
    let g = max(1.0 - 2.0 * length(in.uv - vec2<f32>(0.5)), 0.0);
    if (g - 1e-6 < 0.0) {
        return vec4<f32>(0.0);
    }
    var k = pow(g, flare.shape.x) * flare.shape.y;
    if (flare.glow.w > 0.0) {
        k = k * abs(flare.glow.y * sin(globals.time * flare.glow.x * 6.2831855) + flare.glow.z);
    }
    let c = clamp(k * flare.color.rgb, vec3<f32>(flare.shape.z), vec3<f32>(flare.shape.w));
    return vec4<f32>(c * flare.color.a, 0.0);
}
