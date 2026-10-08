// Arkane's depth of field (`FArkPpDofDownsamplePS`, the DOF part of `FArkPpDofUber_11PS`):
//   downsample: the scene a quarter the size (a box of its texels);
//   dof: scene colour mixed towards the low-resolution copy by
//        saturate((depth - focus) / in-focus radius) x far blur amount.
#import bevy_core_pipeline::fullscreen_vertex_shader::FullscreenVertexOutput

@group(0) @binding(0) var src: texture_2d<f32>;

#ifdef MULTISAMPLED
@group(0) @binding(3) var depth: texture_depth_multisampled_2d;
#else
@group(0) @binding(3) var depth: texture_depth_2d;
#endif

struct Dof {
    // focus distance, 1 / in-focus radius (metres), far blur, near plane
    params: vec4<f32>,
};

@group(0) @binding(1) var low: texture_2d<f32>;
@group(0) @binding(2) var samp: sampler;
@group(0) @binding(4) var<uniform> dof_params: Dof;

// (the downsample binds only the scene and the sampler)

@fragment
fn downsample(in: FullscreenVertexOutput) -> @location(0) vec4<f32> {
    // a quarter the size: the four bilinear taps cover its 4 x 4 texels
    let px = 1.0 / vec2<f32>(textureDimensions(src));
    var c = vec4<f32>(0.0);
    for (var y = -1; y <= 1; y += 2) {
        for (var x = -1; x <= 1; x += 2) {
            c += textureSample(src, samp, in.uv + vec2<f32>(f32(x), f32(y)) * px);
        }
    }
    return vec4<f32>(c.rgb * 0.25, 1.0);
}

@fragment
fn dof(in: FullscreenVertexOutput) -> @location(0) vec4<f32> {
    let s = textureSample(src, samp, in.uv);
    let l = textureSample(low, samp, in.uv);
    let coords = vec2<i32>(in.position.xy);
#ifdef MULTISAMPLED
    let d = textureLoad(depth, coords, 0);
#else
    let d = textureLoad(depth, coords, 0);
#endif
    // reverse, infinite: depth = near / distance (the sky at 0: as far as it gets)
    let z = dof_params.params.w / max(d, 1e-9);
    let w = clamp((z - dof_params.params.x) * dof_params.params.y, 0.0, 1.0) * dof_params.params.z;
    return vec4<f32>(mix(s.rgb, l.rgb, w), s.a);
}
