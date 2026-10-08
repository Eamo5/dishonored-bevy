// The original's Kuwahara filters (`FKuwaPixelShader3` / `FKuwaPixelShader5`, as disassembled:
// the means of the neighbourhood's overlapping quadrants, the one of least variance kept), and
// the `KOKuwa` downsample ahead of them. The vertex shaders' tap coordinates
// (`FKuwaVertexShader3` / `5`: the render target's texel times the node's strength) are worked
// out here per pixel, as they're linear across the screen.

#import bevy_core_pipeline::fullscreen_vertex_shader::FullscreenVertexOutput

@group(0) @binding(0) var src: texture_2d<f32>;
@group(0) @binding(1) var smp: sampler;
struct Kuwa {
    // 1 / the render target's size, the strength
    texel: vec4<f32>,
};
@group(0) @binding(2) var<uniform> kuwa: Kuwa;

fn tap(uv: vec2<f32>) -> vec4<f32> {
    return textureSampleLevel(src, smp, uv, 0.0);
}

@fragment
fn downsample(in: FullscreenVertexOutput) -> @location(0) vec4<f32> {
    return vec4<f32>(tap(in.uv).rgb, 1.0);
}

// 3 x 3: four 2 x 2 quadrants
@fragment
fn kuwa3(in: FullscreenVertexOutput) -> @location(0) vec4<f32> {
    let u = in.uv.x;
    let v = in.uv.y;
    let s = kuwa.texel.z * kuwa.texel.xy;
    let v0 = vec4<f32>(u - s.x, u, u + s.x, v - s.y);
    let v1 = vec4<f32>(u - s.x, u, u + s.x, v);
    let v2 = vec4<f32>(u - s.x, u, u + s.x, v + s.y);
    var r0: vec4<f32> = vec4<f32>(0.0);
    var r1: vec4<f32> = vec4<f32>(0.0);
    var r2: vec4<f32> = vec4<f32>(0.0);
    var r3: vec4<f32> = vec4<f32>(0.0);
    var r4: vec4<f32> = vec4<f32>(0.0);
    var r5: vec4<f32> = vec4<f32>(0.0);
    var r6: vec4<f32> = vec4<f32>(0.0);
    var r7: vec4<f32> = vec4<f32>(0.0);
    var r8: vec4<f32> = vec4<f32>(0.0);
    let c0 = vec4<f32>(4.0, 1.0, 0.25, 0.0);
    var oc0: vec4<f32> = vec4<f32>(0.0);
    r0 = tap(v0.xwzw.xy);
    r1 = tap(v1.xwzw.xy);
    r2 = select(r2, (r1 * r1), vec4<bool>(true, true, true, false));
    r3 = select(r3, fma(r0, r0, r2), vec4<bool>(true, true, true, false));
    r0 = select(r0, (r0 + r1), vec4<bool>(true, true, true, false));
    r4 = tap(v0.ywzw.xy);
    r3 = select(r3, fma(r4, r4, r3), vec4<bool>(true, true, true, false));
    r5 = tap(v1.ywzw.xy);
    r3 = select(r3, fma(r5, r5, r3), vec4<bool>(true, true, true, false));
    r0 = select(r0, (r0 + r4), vec4<bool>(true, true, true, false));
    r0 = select(r0, (r5 + r0), vec4<bool>(true, true, true, false));
    r6 = select(r6, (r0 * r0), vec4<bool>(true, true, true, false));
    r3 = select(r3, fma(r3, c0.xxxx, (-r6)), vec4<bool>(true, true, true, false));
    r0 = select(r0, vec4<f32>(dot(abs(r3).xyz, c0.yyyy.xyz)), vec4<bool>(false, false, false, true));
    r3 = tap(v0.zwzw.xy);
    r6 = tap(v1.zwzw.xy);
    r7 = select(r7, (r6 * r6), vec4<bool>(true, true, true, false));
    r8 = select(r8, fma(r3, r3, r7), vec4<bool>(true, true, true, false));
    r3 = select(r3, (r3 + r6), vec4<bool>(true, true, true, false));
    r3 = select(r3, (r4 + r3), vec4<bool>(true, true, true, false));
    r4 = select(r4, fma(r4, r4, r8), vec4<bool>(true, true, true, false));
    r4 = select(r4, fma(r5, r5, r4), vec4<bool>(true, true, true, false));
    r3 = select(r3, (r5 + r3), vec4<bool>(true, true, true, false));
    r8 = select(r8, (r3 * r3), vec4<bool>(true, true, true, false));
    r4 = select(r4, fma(r4, c0.xxxx, (-r8)), vec4<bool>(true, true, true, false));
    r3 = select(r3, vec4<f32>(dot(abs(r4).xyz, c0.yyyy.xyz)), vec4<bool>(false, false, false, true));
    r1 = select(r1, ((-r0.wwww) + r3.wwww), vec4<bool>(false, false, false, true));
    r0 = select(r3, r0, r1.wwww >= vec4<f32>(0.0));
    r3 = tap(v2.xwzw.xy);
    r2 = select(r2, fma(r3, r3, r2), vec4<bool>(true, true, true, false));
    r1 = select(r1, (r1 + r3), vec4<bool>(true, true, true, false));
    r1 = select(r1, (r5 + r1), vec4<bool>(true, true, true, false));
    r2 = select(r2, fma(r5, r5, r2), vec4<bool>(true, true, true, false));
    r3 = tap(v2.ywzw.xy);
    r2 = select(r2, fma(r3, r3, r2), vec4<bool>(true, true, true, false));
    r1 = select(r1, (r1 + r3), vec4<bool>(true, true, true, false));
    r4 = select(r4, (r1 * r1), vec4<bool>(true, true, true, false));
    r2 = select(r2, fma(r2, c0.xxxx, (-r4)), vec4<bool>(true, true, true, false));
    r1 = select(r1, vec4<f32>(dot(abs(r2).xyz, c0.yyyy.xyz)), vec4<bool>(false, false, false, true));
    r2 = select(r2, ((-r0.wwww) + r1.wwww), vec4<bool>(true, false, false, false));
    r0 = select(r1, r0, r2.xxxx >= vec4<f32>(0.0));
    r1 = tap(v2.zwzw.xy);
    r2 = select(r2, fma(r1, r1, r7), vec4<bool>(true, true, true, false));
    r1 = select(r1, (r1 + r6), vec4<bool>(true, true, true, false));
    r1 = select(r1, (r5 + r1), vec4<bool>(true, true, true, false));
    r2 = select(r2, fma(r5, r5, r2), vec4<bool>(true, true, true, false));
    r2 = select(r2, fma(r3, r3, r2), vec4<bool>(true, true, true, false));
    r1 = select(r1, (r3 + r1), vec4<bool>(true, true, true, false));
    r3 = select(r3, (r1 * r1), vec4<bool>(true, true, true, false));
    r2 = select(r2, fma(r2, c0.xxxx, (-r3)), vec4<bool>(true, true, true, false));
    r1 = select(r1, vec4<f32>(dot(abs(r2).xyz, c0.yyyy.xyz)), vec4<bool>(false, false, false, true));
    r0 = select(r0, ((-r0.wwww) + r1.wwww), vec4<bool>(false, false, false, true));
    r0 = select(r0, select(r1, r0, r0.wwww >= vec4<f32>(0.0)), vec4<bool>(true, true, true, false));
    oc0 = select(oc0, (r0 * c0.zzzz), vec4<bool>(true, true, true, false));
    oc0 = select(oc0, c0.yyyy, vec4<bool>(false, false, false, true));
    return oc0;
}

// 5 x 5 (bilinear taps between texels): four 3 x 3 quadrants
@fragment
fn kuwa5(in: FullscreenVertexOutput) -> @location(0) vec4<f32> {
    let u = in.uv.x;
    let v = in.uv.y;
    let s = kuwa.texel.z * kuwa.texel.xy;
    let v0 = vec4<f32>(u - 1.5 * s.x, v - 2.0 * s.y, u + 1.5 * s.x, v - 2.0 * s.y);
    let v1 = vec4<f32>(u - 1.5 * s.x, v - s.y, u + 1.5 * s.x, v - s.y);
    let v2 = vec4<f32>(u - 1.5 * s.x, v, u + 1.5 * s.x, v);
    let v3 = vec4<f32>(u - 1.5 * s.x, v + s.y, u + 1.5 * s.x, v + s.y);
    let v4 = vec4<f32>(u - 1.5 * s.x, v + 2.0 * s.y, u + 1.5 * s.x, v + 2.0 * s.y);
    let v5 = vec4<f32>(u, v - 1.5 * s.y, v, v + 1.5 * s.y);
    var r0: vec4<f32> = vec4<f32>(0.0);
    var r1: vec4<f32> = vec4<f32>(0.0);
    var r2: vec4<f32> = vec4<f32>(0.0);
    var r3: vec4<f32> = vec4<f32>(0.0);
    var r4: vec4<f32> = vec4<f32>(0.0);
    var r5: vec4<f32> = vec4<f32>(0.0);
    var r6: vec4<f32> = vec4<f32>(0.0);
    var r7: vec4<f32> = vec4<f32>(0.0);
    var r8: vec4<f32> = vec4<f32>(0.0);
    let c0 = vec4<f32>(0.5, 0.25, 9.0, 1.0);
    let c1 = vec4<f32>(0.22222222, 0.0, 0.0, 0.0);
    var oc0: vec4<f32> = vec4<f32>(0.0);
    r0 = tap(v0.xy);
    r1 = tap(v1.xy);
    r2 = select(r2, (r0 + r1), vec4<bool>(true, true, true, false));
    r1 = select(r1, (r1 * r1), vec4<bool>(true, true, true, false));
    r0 = select(r0, fma(r0, r0, r1), vec4<bool>(true, true, true, false));
    r1 = tap(v2.xy);
    r2 = select(r2, (r1 + r2), vec4<bool>(true, true, true, false));
    r3 = tap(v5.xy);
    r2 = select(r2, (r2 + r3), vec4<bool>(true, true, true, false));
    r4 = tap(v5.xzzw.xy);
    r2 = select(r2, fma(r4, c0.xxxx, r2), vec4<bool>(true, true, true, false));
    r5 = select(r5, (r2 * r2), vec4<bool>(true, true, true, false));
    r0 = select(r0, fma(r1, r1, r0), vec4<bool>(true, true, true, false));
    r0 = select(r0, fma(r3, r3, r0), vec4<bool>(true, true, true, false));
    r6 = select(r6, (r4 * r4), vec4<bool>(true, true, true, false));
    r0 = select(r0, fma(r6, c0.yyyy, r0), vec4<bool>(true, true, true, false));
    r0 = select(r0, fma(r0, c0.zzzz, (-r5)), vec4<bool>(true, true, true, false));
    r2 = select(r2, vec4<f32>(dot(abs(r0).xyz, c0.wwww.xyz)), vec4<bool>(false, false, false, true));
    r0 = tap(v0.zwzw.xy);
    r5 = tap(v1.zwzw.xy);
    r7 = select(r7, (r0 + r5), vec4<bool>(true, true, true, false));
    r5 = select(r5, (r5 * r5), vec4<bool>(true, true, true, false));
    r0 = select(r0, fma(r0, r0, r5), vec4<bool>(true, true, true, false));
    r5 = tap(v2.zwzw.xy);
    r7 = select(r7, (r5 + r7), vec4<bool>(true, true, true, false));
    r7 = select(r7, (r3 + r7), vec4<bool>(true, true, true, false));
    r7 = select(r7, fma(r4, c0.xxxx, r7), vec4<bool>(true, true, true, false));
    r8 = select(r8, (r7 * r7), vec4<bool>(true, true, true, false));
    r0 = select(r0, fma(r5, r5, r0), vec4<bool>(true, true, true, false));
    r0 = select(r0, fma(r3, r3, r0), vec4<bool>(true, true, true, false));
    r0 = select(r0, fma(r6, c0.yyyy, r0), vec4<bool>(true, true, true, false));
    r0 = select(r0, fma(r0, c0.zzzz, (-r8)), vec4<bool>(true, true, true, false));
    r7 = select(r7, vec4<f32>(dot(abs(r0).xyz, c0.wwww.xyz)), vec4<bool>(false, false, false, true));
    r0 = select(r0, ((-r2.wwww) + r7.wwww), vec4<bool>(true, false, false, false));
    r0 = select(r7, r2, r0.xxxx >= vec4<f32>(0.0));
    r2 = tap(v3.xy);
    r3 = select(r3, (r1 + r2), vec4<bool>(true, true, true, false));
    r1 = select(r1, (r1 * r1), vec4<bool>(true, true, true, false));
    r1 = select(r1, fma(r2, r2, r1), vec4<bool>(true, true, true, false));
    r2 = tap(v4.xy);
    r3 = select(r3, (r2 + r3), vec4<bool>(true, true, true, false));
    r1 = select(r1, fma(r2, r2, r1), vec4<bool>(true, true, true, false));
    r2 = tap(v5.xwzw.xy);
    r3 = select(r3, (r2 + r3), vec4<bool>(true, true, true, false));
    r3 = select(r3, fma(r4, c0.xxxx, r3), vec4<bool>(true, true, true, false));
    r7 = select(r7, (r3 * r3), vec4<bool>(true, true, true, false));
    r1 = select(r1, fma(r2, r2, r1), vec4<bool>(true, true, true, false));
    r1 = select(r1, fma(r6, c0.yyyy, r1), vec4<bool>(true, true, true, false));
    r1 = select(r1, fma(r1, c0.zzzz, (-r7)), vec4<bool>(true, true, true, false));
    r3 = select(r3, vec4<f32>(dot(abs(r1).xyz, c0.wwww.xyz)), vec4<bool>(false, false, false, true));
    r1 = select(r1, ((-r0.wwww) + r3.wwww), vec4<bool>(true, false, false, false));
    r0 = select(r3, r0, r1.xxxx >= vec4<f32>(0.0));
    r1 = select(r1, (r5 * r5), vec4<bool>(true, true, true, false));
    r3 = tap(v3.zwzw.xy);
    r1 = select(r1, fma(r3, r3, r1), vec4<bool>(true, true, true, false));
    r3 = select(r3, (r3 + r5), vec4<bool>(true, true, true, false));
    r5 = tap(v4.zwzw.xy);
    r1 = select(r1, fma(r5, r5, r1), vec4<bool>(true, true, true, false));
    r3 = select(r3, (r3 + r5), vec4<bool>(true, true, true, false));
    r3 = select(r3, (r2 + r3), vec4<bool>(true, true, true, false));
    r1 = select(r1, fma(r2, r2, r1), vec4<bool>(true, true, true, false));
    r1 = select(r1, fma(r6, c0.yyyy, r1), vec4<bool>(true, true, true, false));
    r2 = select(r2, fma(r4, c0.xxxx, r3), vec4<bool>(true, true, true, false));
    r3 = select(r3, (r2 * r2), vec4<bool>(true, true, true, false));
    r1 = select(r1, fma(r1, c0.zzzz, (-r3)), vec4<bool>(true, true, true, false));
    r1 = select(r1, vec4<f32>(dot(abs(r1).xyz, c0.wwww.xyz)), vec4<bool>(true, false, false, false));
    r0 = select(r0, ((-r0.wwww) + r1.xxxx), vec4<bool>(false, false, false, true));
    r0 = select(r0, select(r2, r0, r0.wwww >= vec4<f32>(0.0)), vec4<bool>(true, true, true, false));
    oc0 = select(oc0, (r0 * c1.xxxx), vec4<bool>(true, true, true, false));
    oc0 = select(oc0, c0.wwww, vec4<bool>(false, false, false, true));
    return oc0;
}
