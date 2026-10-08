// Dishonored's fog (`FDisFogPixelShader<N><M>Layer`), drawn as a full-screen triangle first in
// the translucent phase: from the scene depth (the depth prepass), each layer fogs the part of
// the view ray inside its height band beyond its near plane, ramping (linearly, or through its
// LUT) to its opacity at its far plane; nothing beyond the layer's no-fog plane (the sky), and
// a sun layer only towards the sun. Layers combine as the original does:
//   scene * prod(1 - f_i) + sum(f_i * colour_i)
// which is premultiplied blending of (sum(f_i * colour_i), 1 - prod(1 - f_i)).
#import bevy_pbr::mesh_view_bindings::view
#import bevy_pbr::prepass_utils
#import bevy_pbr::view_transformations

struct FogLayer {
    // linear colour, opacity
    color: vec4<f32>,
    // min height, max height, height density factor, sun power (0: not a sun layer)
    band: vec4<f32>,
    // near, 1 / (far - near), no-fog plane, LUT entries (0: linear)
    planes: vec4<f32>,
};

struct FogParams {
    layers: array<FogLayer, 4>,
    // direction towards the sun, layer count
    sun: vec4<f32>,
    // 4 x 128 LUT entries
    lut: array<vec4<f32>, 128>,
};

@group(#{MATERIAL_BIND_GROUP}) @binding(0) var<uniform> fog: FogParams;

struct VertexOut {
    @builtin(position) pos: vec4<f32>,
};

@vertex
fn vertex(@location(0) position: vec3<f32>) -> VertexOut {
    var o: VertexOut;
    o.pos = vec4<f32>(position.xy, 0.0, 1.0);
    return o;
}

fn lut_entry(layer: u32, i: u32) -> f32 {
    let k = layer * 128u + i;
    return fog.lut[k / 4u][k % 4u];
}

fn lut(layer: u32, t: f32, n: f32) -> f32 {
    let x = clamp(t, 0.0, 1.0) * n - 0.5;
    let i0 = clamp(floor(x), 0.0, n - 1.0);
    let i1 = min(i0 + 1.0, n - 1.0);
    let k = clamp(x - i0, 0.0, 1.0);
    return mix(lut_entry(layer, u32(i0)), lut_entry(layer, u32(i1)), k);
}

@fragment
fn fragment(in: VertexOut) -> @location(0) vec4<f32> {
#ifdef DEPTH_PREPASS
    let d = max(prepass_utils::prepass_depth(in.pos, 0u), 1e-9);
#else
    let d = 1e-9;
#endif
    let world = view_transformations::position_ndc_to_world(vec3<f32>(view_transformations::frag_coord_to_ndc(in.pos).xy, d));
    let cam = view.world_position;
    let ray = world - cam;
    let dist = length(ray);
    let view_depth = -view_transformations::depth_ndc_to_view_z(d);
    var dy = ray.y;
    if (abs(dy) < 1e-6) {
        dy = select(-1e-6, 1e-6, dy >= 0.0);
    }
    let inv_dy = 1.0 / dy;
    var keep = 1.0;
    var add = vec3<f32>(0.0);
    let count = u32(fog.sun.w);
    for (var i = 0u; i < count; i++) {
        let l = fog.layers[i];
        let hi = clamp((l.band.y - cam.y) * inv_dy * l.band.z, 0.0, 1.0);
        let lo = clamp((l.band.x - cam.y) * inv_dy * l.band.z, 0.0, 1.0);
        let t = (abs(hi - lo) * dist - l.planes.x) * l.planes.y;
        var f = select(clamp(t, 0.0, 1.0), lut(i, t, l.planes.w), l.planes.w > 0.5) * l.color.a;
        f = select(0.0, f, l.planes.z - view_depth >= 0.0);
        if (l.band.w > 0.1) {
            let s = clamp(dot(ray, fog.sun.xyz) / max(dist, 1e-6), 0.0, 1.0);
            f = select(0.0, f * pow(s, l.band.w), s - 1e-6 >= 0.0);
        }
        add += f * l.color.rgb;
        keep *= 1.0 - f;
    }
    return vec4<f32>(add, 1.0 - keep);
}
