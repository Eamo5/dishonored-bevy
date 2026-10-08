//! Assembly of a renderable program from a material's translated D3D9 shaders.
//!
//! A program pairs a base pass vertex shader and pixel shader of one material shader map
//! (for a vertex factory and light-map policy) with:
//!  - the engine constants UE3 set for them (view / object transforms in UE space, light-map
//!    scale and coordinates, overrides), computed from Bevy's view and mesh data,
//!  - the material's uniform expressions (parameters, time) compiled to WGSL,
//!  - vertex inputs rebuilt in UE3's layout from the Bevy mesh attributes.
//!
//! The module targets Bevy's material pipeline (`#import`s, `MATERIAL_BIND_GROUP`), or with
//! `Platform::Standalone` stub bindings so it can be validated with naga alone.

use crate::shadercache::{param_nodes, CookedMap, Expr};
use crate::sm3::{self, Interface, Shader, Stage};
use anyhow::{bail, Result};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Policy {
    NoLightMap,
    DirectionalLightMap,
    /// a dynamic light's additive pass (`TLight*ShaderF{Point,Spot}LightPolicy`): the light's
    /// parameters are the material's last uniform slots (see `DYN_LIGHT_SLOT`)
    PointLight,
    SpotLight,
    /// a dominant light's pass through its static distance-field shadow map (coordinates and
    /// page layer in `DYN_SHADOW_SLOT`)
    PointLightShadowed,
    SpotLightShadowed,
}

/// Two material uniform slots of a shadowed dynamic light pass: the shadow map's coordinate
/// scale / bias (as `LightmapCoordinateScaleBias`) and its shadow page layer.
pub const DYN_SHADOW_SLOT: usize = 58;

/// Distance-field shadows: the shaders take `DistanceFieldParameters` as (bias, scale,
/// exponent) for `pow(saturate((distance + bias) * scale), exponent)`. The engine's values
/// are native; this keeps the 0.5 contour (the shadow edge) at half light.
const DISTANCE_FIELD_SHARPNESS: f32 = 8.0;

/// First of the four material uniform slots a dynamic light pass reads: position and
/// inverse radius, colour and falloff exponent, spot angles, spot direction (UE space).
pub const DYN_LIGHT_SLOT: usize = 60;

impl Policy {
    pub fn shader_types(self) -> (&'static str, &'static str) {
        match self {
            Policy::NoLightMap => ("TBasePassVertexShaderFNoLightMapPolicy", "TBasePassPixelShaderFNoLightMapPolicyNoSkyLightFALSEFALSE"),
            Policy::PointLight => ("TLightVertexShaderFPointLightPolicyFNoStaticShadowingPolicy", "TLightPixelShaderFPointLightPolicyFNoStaticShadowingPolicy"),
            Policy::SpotLight => ("TLightVertexShaderFSpotLightPolicyFNoStaticShadowingPolicy", "TLightPixelShaderFSpotLightPolicyFNoStaticShadowingPolicy"),
            Policy::PointLightShadowed => (
                "TLightVertexShaderFPointLightPolicyFSignedDistanceFieldShadowTexturePolicy",
                "TLightPixelShaderFPointLightPolicyFSignedDistanceFieldShadowTexturePolicy",
            ),
            Policy::SpotLightShadowed => (
                "TLightVertexShaderFSpotLightPolicyFSignedDistanceFieldShadowTexturePolicy",
                "TLightPixelShaderFSpotLightPolicyFSignedDistanceFieldShadowTexturePolicy",
            ),
            Policy::DirectionalLightMap => (
                "TBasePassVertexShaderFDirectionalLightMapTexturePolicy",
                "TBasePassPixelShaderFDirectionalLightMapTexturePolicyNoSkyLightFALSEFALSE",
            ),
        }
    }
}

impl Policy {
    /// Shader types of the dominant directional light pass (static shadow texture).
    pub const SUN: (&'static str, &'static str) = (
        "TLightVertexShaderFDirectionalLightPolicyFSignedDistanceFieldShadowTexturePolicy",
        "TLightPixelShaderFDirectionalLightPolicyFSignedDistanceFieldShadowTexturePolicy",
    );
    /// The plain shadow-factor variant (materials compiled without the distance-field one).
    pub const SUN_FACTOR: (&'static str, &'static str) = (
        "TLightVertexShaderFDirectionalLightPolicyFShadowTexturePolicy",
        "TLightPixelShaderFDirectionalLightPolicyFShadowTexturePolicy",
    );
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Platform {
    Bevy,
    Standalone,
}

/// Material bind group layout shared by every program.
pub const MAX_PARAMS: usize = 64;
pub const MAX_TEXTURES: usize = 12;
/// vec4s per instance in the instance buffer:
/// [0] light-map coordinate scale.xy, bias.xy  [1] LightMapScale[0]  [2] LightMapScale[1]
/// [3] bitcast u32: light-map layer, shadow layer, flags, sun mode (0 none, 1 shadow
/// mapped, 2 unshadowed)  [4] shadow coordinate scale.xy, bias.xy  [5] ambient colour and
/// sky factor  [6..7] reserved.
/// Entry 0 holds the scene: [0] direction towards the dominant light (UE space)
/// [1] its colour times brightness.
pub const INSTANCE_STRIDE: u32 = 8;

pub struct Program {
    pub wgsl: String,
    /// engine constants the shaders read that the program does not supply
    pub unknown: BTreeSet<String>,
}

struct Ctx {
    unknown: BTreeSet<String>,
    slots: usize,
    /// filling the light pass's constants
    light: bool,
    /// the program is a dynamic light's pass (its constants come from the material)
    dyn_light: bool,
}

/// One translated stage.
struct StageT {
    sh: Shader,
    body: String,
    iface: Interface,
}

fn stage(code: &[u8], prefix: &str) -> Result<StageT> {
    let sh = sm3::parse(code)?;
    let (body, iface) = sm3::translate(&sh, prefix)?;
    Ok(StageT { sh, body, iface })
}

fn vec4(v: [f32; 4]) -> String {
    format!("vec4<f32>({:?}, {:?}, {:?}, {:?})", v[0], v[1], v[2], v[3])
}

/// WGSL for a uniform expression; parameters read `ue_mat.p[slot]` in `param_nodes` order.
fn expr(e: &Expr, slot: &mut usize) -> String {
    let mut b = |x: &Expr| expr(x, slot);
    match e {
        Expr::Constant(v) => vec4(*v),
        Expr::VectorParameter(..) | Expr::ScalarParameter(..) => {
            let s = *slot;
            *slot += 1;
            if s < MAX_PARAMS {
                format!("ue_mat.p[{s}]")
            } else {
                "vec4<f32>(0.0)".into()
            }
        }
        Expr::Time | Expr::RealTime => "vec4<f32>(ue_time())".into(),
        Expr::Texture(_) | Expr::TextureParameter(..) | Expr::FlipBook(_) => "vec4<f32>(0.0)".into(),
        Expr::Sine(x, cos) => format!("vec4<f32>({}({}.x))", if *cos { "cos" } else { "sin" }, b(x)),
        Expr::Periodic(x) | Expr::Frac(x) => format!("fract({})", b(x)),
        Expr::Floor(x) => format!("floor({})", b(x)),
        Expr::Ceil(x) => format!("ceil({})", b(x)),
        Expr::Abs(x) => format!("abs({})", b(x)),
        Expr::Square(x) => {
            let v = b(x);
            format!("({v} * {v})")
        }
        Expr::Clamp(x, lo, hi) => {
            let (x, lo, hi) = (b(x), b(lo), b(hi));
            format!("clamp({x}, {lo}, {hi})")
        }
        Expr::Min(x, y) => {
            let (x, y) = (b(x), b(y));
            format!("min({x}, {y})")
        }
        Expr::Max(x, y) => {
            let (x, y) = (b(x), b(y));
            format!("max({x}, {y})")
        }
        Expr::Fmod(x, y) => {
            let (x, y) = (b(x), b(y));
            format!("ue_fmod({x}, {y})")
        }
        Expr::Math(x, y, op) => {
            let (x, y) = (b(x), b(y));
            match op {
                0 => format!("({x} + {y})"),
                1 => format!("({x} - {y})"),
                2 => format!("({x} * {y})"),
                3 => format!("ue_div({x}, {y})"),
                _ => format!("vec4<f32>(dot({x}, {y}))"),
            }
        }
        Expr::Append(x, y, n) => {
            let (x, y) = (b(x), b(y));
            format!("ue_append({x}, {y}, {}u)", (*n).clamp(1, 3))
        }
    }
}

/// Uniform expression functions: `ue_pv{i}`, `ue_ps{i}`, `ue_vv{i}`, `ue_vs{i}`.
fn expression_functions(map: &CookedMap, ctx: &mut Ctx) -> String {
    let mut m = String::new();
    let mut slot = 0usize;
    for (prefix, list) in [("pv", &map.pixel.vectors), ("ps", &map.pixel.scalars), ("vv", &map.vertex.vectors), ("vs", &map.vertex.scalars)] {
        for (i, e) in list.iter().enumerate() {
            let _ = writeln!(m, "fn ue_{prefix}{i}() -> vec4<f32> {{ return {}; }}", expr(e, &mut slot));
        }
    }
    ctx.slots = slot;
    m
}

/// The register an engine / material constant of the constant table is read from.
fn constant_value(name: &str, i: u32, stage: Stage, map: &CookedMap, ctx: &mut Ctx) -> String {
    let scalars = |list_len: usize, f: &str, idx: u32| -> String {
        let c: Vec<String> = (0..4)
            .map(|k| {
                let j = idx as usize * 4 + k;
                if j < list_len {
                    format!("ue_{f}{j}().x")
                } else {
                    "0.0".into()
                }
            })
            .collect();
        format!("vec4<f32>({})", c.join(", "))
    };
    let indexed = |n: &str, p: &str| -> Option<u32> { n.strip_prefix(p).and_then(|s| s.parse().ok()) };
    if let Some(k) = indexed(name, "UniformPixelVector_") {
        return if (k as usize) < map.pixel.vectors.len() { format!("ue_pv{k}()") } else { "vec4<f32>(0.0)".into() };
    }
    if let Some(k) = indexed(name, "UniformPixelScalars_") {
        return scalars(map.pixel.scalars.len(), "ps", k + i);
    }
    if let Some(k) = indexed(name, "UniformVertexVector_") {
        return if (k as usize) < map.vertex.vectors.len() { format!("ue_vv{k}()") } else { "vec4<f32>(0.0)".into() };
    }
    if let Some(k) = indexed(name, "UniformVertexScalars_") {
        return scalars(map.vertex.scalars.len(), "vs", k + i);
    }
    match (name, stage) {
        ("ViewProjectionMatrix", _) => format!("ue_vp[{}]", i.min(3)),
        ("LocalToWorld", _) | ("LocalToWorldMatrix", _) => format!("ue_l2w[{}]", i.min(3)),
        ("WorldToLocal", _) => format!("vec4<f32>(ue_w2l[{}], 0.0)", i.min(2)),
        ("CameraPosition", _) | ("CameraWorldPos", _) | ("CameraWorldPosition", _) => "vec4<f32>(ue_cam, 1.0)".into(),
        ("PreViewTranslation", _) => "vec4<f32>(0.0)".into(),
        ("LocalToWorldRotDeterminantFlip", _) => "vec4<f32>(ue_det_sign)".into(),
        // a dominant light's pass: its shadow map's coordinates
        ("LightmapCoordinateScaleBias", _) if ctx.dyn_light => format!("ue_mat.p[{}]", DYN_SHADOW_SLOT),
        ("DistanceFieldParameters", _) => format!("vec4<f32>({:?}, {DISTANCE_FIELD_SHARPNESS:?}, 1.0, 0.0)", 0.5 / DISTANCE_FIELD_SHARPNESS - 0.5),
        // the light pass maps the light-map coordinates to the light's static shadow map
        ("LightmapCoordinateScaleBias", _) if ctx.light => "vec4<f32>(ue_inst[ue_base + 4u].xy, ue_inst[ue_base + 4u].wz)".into(),
        ("LightmapCoordinateScaleBias", _) => "vec4<f32>(ue_inst[ue_base].xy, ue_inst[ue_base].wz)".into(),
        // the scene's dominant directional light, or a character's dominant local light
        // (its light environment: instance entries 6-7)
        ("LightPositionAndInvRadius", _) => format!("ue_mat.p[{}]", DYN_LIGHT_SLOT),
        ("LightColorAndFalloffExponent", _) if ctx.dyn_light => format!("ue_mat.p[{}]", DYN_LIGHT_SLOT + 1),
        ("SpotAngles", _) => format!("ue_mat.p[{}]", DYN_LIGHT_SLOT + 2),
        ("SpotDirection", _) => format!("ue_mat.p[{}]", DYN_LIGHT_SLOT + 3),
        ("LightDirection", _) if ctx.light => "vec4<f32>(select(ue_inst[0].xyz, ue_inst[ue_base + 6u].xyz, ue_inst[ue_base + 6u].w > 0.5), 0.0)".into(),
        ("LightColorAndFalloffExponent", _) if ctx.light => {
            "vec4<f32>(select(ue_inst[1].rgb, ue_inst[ue_base + 7u].rgb, ue_inst[ue_base + 6u].w > 0.5) * ue_sun, 0.0)".into()
        }
        ("DistanceFadeParameters", _) => "vec4<f32>(0.0)".into(),
        ("LightMapScale", _) => format!("ue_inst[ue_base + {}u]", 1 + i.min(1)),
        ("AmbientColorAndSkyFactor", _) => "ue_inst[ue_base + 5u]".into(),
        ("DiffuseOverrideParameter", _) | ("SpecularOverrideParameter", _) => "vec4<f32>(0.0, 0.0, 0.0, 1.0)".into(),
        ("ScreenPositionScaleBias", _) => "vec4<f32>(0.5, -0.5, 0.5, 0.5)".into(),
        // (post-process materials: the target's size and its inverse)
        ("GScreenResolution", _) | ("GResolution", _) => "ue_post_res()".into(),
        ("TwoSidedSign", _) => "vec4<f32>(1.0)".into(),
        ("ObjectWorldPositionAndRadius", _) => "vec4<f32>(ue_l2w[3].xyz, 100.0)".into(),
        ("UpperSkyColor", _) | ("LowerSkyColor", _) | ("WorldIncidentLighting", _) | ("LightColorAndFalloffExponent", _) => {
            "vec4<f32>(0.0)".into()
        }
        _ => {
            ctx.unknown.insert(name.to_string());
            "vec4<f32>(0.0)".into()
        }
    }
}

/// Code filling a stage's constant array from its constant table.
fn constant_fill(st: &StageT, prefix: &str, map: &CookedMap, ctx: &mut Ctx) -> String {
    let mut m = String::new();
    let n = if st.iface.uses_rel_const { 256 } else { st.iface.max_const + 1 };
    let stage = st.sh.stage.unwrap_or(Stage::Pixel);
    for c in &st.sh.ctab {
        match c.set {
            2 => {
                for i in 0..c.count as u32 {
                    let r = c.reg as u32 + i;
                    if r >= n || !(st.iface.uses_rel_const || st.iface.consts.contains(&r)) {
                        continue;
                    }
                    let v = constant_value(&c.name, i, stage, map, ctx);
                    let _ = writeln!(m, "    {prefix}kc[{r}] = {v};");
                }
            }
            0 => {
                // bool constants: the dominant directional light's pass takes the dynamic
                // objects' shadows (`ue_light_attenuation`); no distance fading
                if c.name != "bReceiveDynamicShadows" && c.name != "bEnableDistanceShadowFading" {
                    ctx.unknown.insert(c.name.clone());
                }
                let on = c.name == "bReceiveDynamicShadows" && ctx.light && !ctx.dyn_light;
                for i in 0..c.count.max(1) as u32 {
                    let r = c.reg as u32 + i;
                    if r <= st.iface.max_bool {
                        let _ = writeln!(m, "    {prefix}kb[{r}] = {}u;", on as u32);
                    }
                }
            }
            _ => {}
        }
    }
    m
}

/// Texture behind each sampler register: (texture, sampler, extra sample arguments, result
/// wrapper).
fn sampler_bindings(st: &StageT) -> BTreeMap<u32, (String, String, String, String)> {
    let mut out = BTreeMap::new();
    for c in st.sh.ctab.iter().filter(|c| c.set == 3) {
        for i in 0..c.count.max(1) as u32 {
            let r = c.reg as u32 + i;
            let dim = *st.iface.samplers.get(&r).unwrap_or(&2);
            let idx = |p: &str| -> Option<usize> { c.name.strip_prefix(p).and_then(|s| s.parse().ok()) };
            let n = String::new;
            let b = match (c.name.as_str(), dim) {
                (_, 2) if idx("Texture2D_").is_some_and(|k| k < MAX_TEXTURES) => {
                    let k = idx("Texture2D_").unwrap();
                    (format!("ue_t{k}"), format!("ue_s{k}"), n(), n())
                }
                ("LightMapTextures", 2) => (format!("ue_lm{}", i.min(1)), "ue_lm_s".into(), ", ue_lm_layer".into(), n()),
                // surfaces the sun reaches without a shadow map read as fully lit
                ("ShadowTexture", 2) => ("ue_shadow".into(), "ue_shadow_s".into(), ", ue_shadow_layer".into(), "max({}, vec4<f32>(ue_shadow_full))".into()),
                // UE3's light attenuation buffer: the dynamic objects' shadows
                ("LightAttenuationTexture", 2) => ("ue_white".into(), "ue_white_s".into(), n(), "ue_light_attenuation({})".into()),
                ("SceneColorTexture", 2) => ("SCENECOLOR".into(), n(), n(), n()),
                // a post-process node's inputs (`ArkPpNodeMaterial::m_Node`)
                (_, 2) if idx("GArkPpSampler_").is_some_and(|k| k < POST_INPUTS) => {
                    let k = idx("GArkPpSampler_").unwrap();
                    (format!("ue_pp{k}"), format!("ue_pps{k}"), n(), n())
                }
                (_, 2) => ("ue_black".into(), "ue_white_s".into(), n(), n()),
                (_, 3) if idx("TextureCube_") == Some(0) => ("ue_cube0".into(), "ue_cube_s0".into(), n(), n()),
                // the level's reflection cube (`WorldInfo.mSceneReflection`)
                ("SceneReflectionTexture", 3) => ("ue_refl".into(), "ue_cube_s0".into(), n(), n()),
                (_, 3) => ("ue_black_cube".into(), "ue_cube_s0".into(), n(), n()),
                _ => (n(), n(), n(), n()),
            };
            out.insert(r, b);
        }
    }
    out
}

fn helpers(st: &StageT, prefix: &str) -> String {
    let binds = sampler_bindings(st);
    let mut iface = st.iface.clone();
    // samplers without a texture (volume textures, unnamed) return zero
    let mut zero = String::new();
    for (&s, &kinds) in &st.iface.sample_kinds {
        if binds.get(&s).is_some_and(|b| !b.0.is_empty()) {
            continue;
        }
        iface.sample_kinds.remove(&s);
        let dim = *st.iface.samplers.get(&s).unwrap_or(&2);
        let uvt = if dim == 2 { "vec2<f32>" } else { "vec3<f32>" };
        let f = format!("{prefix}smp{s}");
        for (bit, suffix, extra) in [(1, "", ""), (2, "_l", ", lod: f32"), (4, "_b", ", bias: f32")] {
            if kinds & bit != 0 {
                let _ = writeln!(zero, "fn {f}{suffix}(uv: {uvt}{extra}) -> vec4<f32> {{ return vec4<f32>(0.0); }}");
            }
        }
        if kinds & 8 != 0 {
            let _ = writeln!(zero, "fn {f}_g(uv: {uvt}, gx: {uvt}, gy: {uvt}) -> vec4<f32> {{ return vec4<f32>(0.0); }}");
        }
    }
    // scene colour reads: the scene depth from the depth prepass
    let mut scene = String::new();
    for (&s, &kinds) in &st.iface.sample_kinds {
        if binds.get(&s).map(|b| b.0.as_str()) != Some("SCENECOLOR") {
            continue;
        }
        iface.sample_kinds.remove(&s);
        let f = format!("{prefix}smp{s}");
        for (bit, suffix, extra) in [(1, "", ""), (2, "_l", ", lod: f32"), (4, "_b", ", bias: f32")] {
            if kinds & bit != 0 {
                let _ = writeln!(scene, "fn {f}{suffix}(uv: vec2<f32>{extra}) -> vec4<f32> {{ return ue_scene_color(uv); }}");
            }
        }
        if kinds & 8 != 0 {
            let _ = writeln!(scene, "fn {f}_g(uv: vec2<f32>, gx: vec2<f32>, gy: vec2<f32>) -> vec4<f32> {{ return ue_scene_color(uv); }}");
        }
    }
    let vertex = st.sh.stage == Some(Stage::Vertex);
    let mut m = sm3::sampling_helpers(&iface, prefix, vertex, &|s| binds.get(&s).cloned().unwrap_or_default());
    m.push_str(&zero);
    m.push_str(&scene);
    m
}

/// Vertex input value (UE3 local vertex factory layout) for a declared input semantic.
fn local_vf_input(usage: u32, index: u32) -> &'static str {
    match (usage, index) {
        (0, _) => "vec4<f32>(ue_p(v.position), 1.0)",
        // packed UBYTE4 tangent basis (decoded in the shader as x * 2/255 - 1)
        (6, _) => "(vec4<f32>(ue_d(v.tangent.xyz), 1.0) + 1.0) * 127.5",
        (3, _) => "(vec4<f32>(ue_d(v.normal), -v.tangent.w) + 1.0) * 127.5",
        (10, 1) => "v.color",
        (10, 0) => "vec4<f32>(v.uv1, 0.0, 1.0)",
        (5, 0) => "vec4<f32>(v.uv0, 0.0, 1.0)",
        (5, 1) => "vec4<f32>(v.uv1, 0.0, 1.0)",
        (5, _) => "vec4<f32>(v.uv0, 0.0, 1.0)",
        _ => "vec4<f32>(0.0)",
    }
}

const PLATFORM_BEVY: &str = r#"#import bevy_pbr::mesh_functions
#import bevy_pbr::mesh_view_bindings::view
#ifndef PREPASS_PIPELINE
#import bevy_pbr::mesh_view_bindings::{lights, globals}
#import bevy_pbr::shadows::fetch_directional_shadow
#endif
#import bevy_pbr::skinning
#import bevy_pbr::prepass_utils
#import bevy_pbr::view_transformations
#import bevy_render::globals::Globals

fn ue_world_from_local(i: u32) -> mat4x4<f32> { return mesh_functions::get_world_from_local(i); }
fn ue_tag(i: u32) -> u32 { return mesh_functions::get_tag(i); }
#ifdef UE_FOREGROUND
// first-person view model (UE3's foreground depth priority group): its own field of view
// (62 degrees), and depths squeezed into the nearest slice so it never clips into the world
fn ue_clip_from_world() -> mat4x4<f32> {
    var p = view.clip_from_view;
#ifdef UE_FOREGROUND_WORLD
    // (a character drawn in the foreground group, `DisSeqAct_OutsiderConfig`: the world's view)
    let k = 1.0;
#else
    let k = 1.6642795 / p[1][1];
#endif
    p[0][0] *= k;
    p[1][1] *= k;
    for (var c = 0; c < 4; c++) {
        p[c].z = 0.001 * p[c].z + 0.999 * p[c].w;
    }
    return p * view.view_from_world;
}
#else
fn ue_clip_from_world() -> mat4x4<f32> { return view.clip_from_world; }
#endif
fn ue_view_position() -> vec3<f32> { return view.world_position; }
// the globals uniform sits at a different binding in the prepass view layout
#ifdef PREPASS_PIPELINE
@group(0) @binding(1) var<uniform> ue_globals: Globals;
fn ue_time() -> f32 { return ue_globals.time; }
#else
fn ue_time() -> f32 { return globals.time; }
#endif

// the fragment's world position and facing (for the dynamic shadows)
var<private> ue_frag_coord: vec4<f32>;
var<private> ue_frag_world: vec3<f32>;
var<private> ue_frag_normal: vec3<f32>;
fn ue_frag_setup(pos: vec4<f32>) {
    ue_frag_coord = pos;
    ue_frag_world = view_transformations::position_ndc_to_world(view_transformations::frag_coord_to_ndc(pos));
    var n = normalize(cross(dpdy(ue_frag_world), dpdx(ue_frag_world)));
    if (dot(n, view.world_position - ue_frag_world) < 0.0) {
        n = -n;
    }
    ue_frag_normal = n;
}

// UE3's light attenuation buffer: the dynamic objects (characters, props) shadowing the
// dominant directional light, from Bevy's shadow map of them
fn ue_light_attenuation(x: vec4<f32>) -> vec4<f32> {
#ifdef UE_FOREGROUND
    return x;
#else
#ifdef PREPASS_PIPELINE
    return x;
#else
    if (lights.n_directional_lights == 0u) {
        return x;
    }
    let view_z = (view.view_from_world * vec4<f32>(ue_frag_world, 1.0)).z;
    return vec4<f32>(fetch_directional_shadow(0u, vec4<f32>(ue_frag_world, 1.0), ue_frag_normal, view_z, ue_frag_coord.xy));
#endif
#endif
}

// UE3 on D3D9 keeps the scene depth (UE units / 256) in the scene colour's alpha; translucent
// materials read it back for depth fading. Here it comes from the depth prepass.
fn ue_scene_color(uv: vec2<f32>) -> vec4<f32> {
#ifdef UE_FOREGROUND
    // the view model draws over the cleared foreground depth
    return vec4<f32>(0.0, 0.0, 0.0, 4096.0);
#endif
#ifdef DEPTH_PREPASS
#ifndef PREPASS_PIPELINE
    let d = prepass_utils::prepass_depth(vec4<f32>(uv * view.viewport.zw, 0.0, 0.0), 0u);
    let z = -view_transformations::depth_ndc_to_view_z(max(d, 1e-7)) * 100.0;
    return vec4<f32>(0.0, 0.0, 0.0, z / 256.0);
#else
    return vec4<f32>(0.0, 0.0, 0.0, 4096.0);
#endif
#else
    return vec4<f32>(0.0, 0.0, 0.0, 4096.0);
#endif
}
"#;

const PLATFORM_STANDALONE: &str = r#"struct UeStub { clip_from_world: mat4x4<f32>, world_position: vec3<f32>, time: f32 };
@group(0) @binding(0) var<uniform> ue_stub: UeStub;
@group(1) @binding(0) var<storage, read> ue_stub_models: array<mat4x4<f32>>;
fn ue_world_from_local(i: u32) -> mat4x4<f32> { return ue_stub_models[i]; }
fn ue_tag(i: u32) -> u32 { return i; }
fn ue_clip_from_world() -> mat4x4<f32> { return ue_stub.clip_from_world; }
fn ue_view_position() -> vec3<f32> { return ue_stub.world_position; }
fn ue_time() -> f32 { return ue_stub.time; }
fn ue_scene_color(uv: vec2<f32>) -> vec4<f32> { return vec4<f32>(0.0, 0.0, 0.0, 4096.0); }
fn ue_frag_setup(pos: vec4<f32>) {}
fn ue_light_attenuation(x: vec4<f32>) -> vec4<f32> { return x; }
"#;

const COMMON: &str = r#"
struct UeMaterial { p: array<vec4<f32>, 64> };
@group(MATGROUP) @binding(0) var<uniform> ue_mat: UeMaterial;
@group(MATGROUP) @binding(1) var ue_t0: texture_2d<f32>;
@group(MATGROUP) @binding(2) var ue_s0: sampler;
@group(MATGROUP) @binding(3) var ue_t1: texture_2d<f32>;
@group(MATGROUP) @binding(4) var ue_s1: sampler;
@group(MATGROUP) @binding(5) var ue_t2: texture_2d<f32>;
@group(MATGROUP) @binding(6) var ue_s2: sampler;
@group(MATGROUP) @binding(7) var ue_t3: texture_2d<f32>;
@group(MATGROUP) @binding(8) var ue_s3: sampler;
@group(MATGROUP) @binding(9) var ue_t4: texture_2d<f32>;
@group(MATGROUP) @binding(10) var ue_s4: sampler;
@group(MATGROUP) @binding(11) var ue_t5: texture_2d<f32>;
@group(MATGROUP) @binding(12) var ue_s5: sampler;
@group(MATGROUP) @binding(13) var ue_t6: texture_2d<f32>;
@group(MATGROUP) @binding(14) var ue_s6: sampler;
@group(MATGROUP) @binding(15) var ue_t7: texture_2d<f32>;
@group(MATGROUP) @binding(16) var ue_s7: sampler;
@group(MATGROUP) @binding(17) var ue_t8: texture_2d<f32>;
@group(MATGROUP) @binding(18) var ue_s8: sampler;
@group(MATGROUP) @binding(19) var ue_t9: texture_2d<f32>;
@group(MATGROUP) @binding(20) var ue_s9: sampler;
@group(MATGROUP) @binding(21) var ue_t10: texture_2d<f32>;
@group(MATGROUP) @binding(22) var ue_s10: sampler;
@group(MATGROUP) @binding(23) var ue_t11: texture_2d<f32>;
@group(MATGROUP) @binding(24) var ue_s11: sampler;
@group(MATGROUP) @binding(25) var ue_cube0: texture_cube<f32>;
@group(MATGROUP) @binding(26) var ue_cube_s0: sampler;
@group(MATGROUP) @binding(27) var<storage, read> ue_inst: array<vec4<f32>>;
@group(MATGROUP) @binding(28) var ue_lm0: texture_2d_array<f32>;
@group(MATGROUP) @binding(29) var ue_lm1: texture_2d_array<f32>;
@group(MATGROUP) @binding(30) var ue_lm_s: sampler;
@group(MATGROUP) @binding(31) var ue_shadow: texture_2d_array<f32>;
@group(MATGROUP) @binding(32) var ue_shadow_s: sampler;
@group(MATGROUP) @binding(33) var ue_white: texture_2d<f32>;
@group(MATGROUP) @binding(34) var ue_white_s: sampler;
@group(MATGROUP) @binding(35) var ue_black: texture_2d<f32>;
@group(MATGROUP) @binding(36) var ue_black_cube: texture_cube<f32>;
@group(MATGROUP) @binding(37) var ue_refl: texture_cube<f32>;

// per-draw state
var<private> ue_base: u32;
// dominant light: enabled (0 / 1), and no shadow map (fully lit, 0 / 1)
var<private> ue_sun: f32;
var<private> ue_shadow_full: f32;
var<private> ue_lm_layer: i32;
var<private> ue_shadow_layer: i32;
var<private> ue_vp: mat4x4<f32>;
var<private> ue_l2w: mat4x4<f32>;
var<private> ue_w2l: mat3x3<f32>;
var<private> ue_cam: vec3<f32>;
var<private> ue_det_sign: f32;

// Bevy (right-handed, Y up, metres) <-> UE3 (left-handed, Z up, centimetres)
fn ue_p(v: vec3<f32>) -> vec3<f32> { return vec3<f32>(v.x, v.z, v.y) * 100.0; }
fn ue_d(v: vec3<f32>) -> vec3<f32> { return vec3<f32>(v.x, v.z, v.y); }
const UE_SWAP = mat4x4<f32>(vec4<f32>(1.0, 0.0, 0.0, 0.0), vec4<f32>(0.0, 0.0, 1.0, 0.0), vec4<f32>(0.0, 1.0, 0.0, 0.0), vec4<f32>(0.0, 0.0, 0.0, 1.0));
fn ue_scale(s: f32) -> mat4x4<f32> {
    return mat4x4<f32>(vec4<f32>(s, 0.0, 0.0, 0.0), vec4<f32>(0.0, s, 0.0, 0.0), vec4<f32>(0.0, 0.0, s, 0.0), vec4<f32>(0.0, 0.0, 0.0, 1.0));
}
fn ue_fmod(a: vec4<f32>, b: vec4<f32>) -> vec4<f32> { return a - b * trunc(a / b); }
fn ue_div(a: vec4<f32>, b: vec4<f32>) -> vec4<f32> { return select(a / b, vec4<f32>(0.0), b == vec4<f32>(0.0)); }
fn ue_append(a: vec4<f32>, b: vec4<f32>, n: u32) -> vec4<f32> {
    if n == 1u { return vec4<f32>(a.x, b.x, b.y, b.z); }
    if n == 2u { return vec4<f32>(a.x, a.y, b.x, b.y); }
    return vec4<f32>(a.x, a.y, a.z, b.x);
}

fn ue_setup(instance: u32, tag: u32) {
    ue_setup_model(ue_world_from_local(instance), tag);
}

// `model`: Bevy world-from-local (skinned per vertex for skeletal meshes)
fn ue_setup_model(model: mat4x4<f32>, tag: u32) {
    ue_base = tag * 8u;
    let info = bitcast<vec4<u32>>(ue_inst[ue_base + 3u]);
    ue_lm_layer = i32(info.x);
    ue_shadow_layer = i32(info.y);
    ue_sun = select(0.0, 1.0, info.w != 0u);
    ue_shadow_full = select(0.0, 1.0, info.w == 2u);
    // UE world = C^-1 * Bevy world with C = scale(0.01) * swap
    let to_ue = UE_SWAP * ue_scale(100.0);
    let from_ue = ue_scale(0.01) * UE_SWAP;
    ue_l2w = to_ue * model * from_ue;
    let m3 = mat3x3<f32>(ue_l2w[0].xyz, ue_l2w[1].xyz, ue_l2w[2].xyz);
    let det = determinant(m3);
    ue_det_sign = select(1.0, -1.0, det < 0.0);
    // inverse of the 3x3 part (adjugate / determinant)
    let c0 = cross(m3[1], m3[2]);
    let c1 = cross(m3[2], m3[0]);
    let c2 = cross(m3[0], m3[1]);
    ue_w2l = transpose(mat3x3<f32>(c0, c1, c2)) * (1.0 / det);
    // clip = 100 * clip_from_world(Bevy) * C: same NDC, w in UE units
    let vp = ue_clip_from_world() * from_ue;
    ue_vp = mat4x4<f32>(vp[0] * 100.0, vp[1] * 100.0, vp[2] * 100.0, vp[3] * 100.0);
    ue_cam = ue_p(ue_view_position());
}
"#;

/// Build a program for a material shader map: its base pass shaders, plus optionally the
/// dominant directional light's pass (added to the base pass result).
pub fn build(map: &CookedMap, vs_code: &[u8], ps_code: &[u8], light: Option<(&[u8], &[u8])>, platform: Platform) -> Result<Program> {
    let vs = stage(vs_code, "vs_")?;
    let ps = stage(ps_code, "ps_")?;
    if vs.sh.stage != Some(Stage::Vertex) || ps.sh.stage != Some(Stage::Pixel) {
        bail!("shader stages mismatch");
    }
    let light = match light {
        Some((v, p)) => {
            let (lvs, lps) = (stage(v, "lvs_")?, stage(p, "lps_")?);
            if lvs.sh.stage != Some(Stage::Vertex) || lps.sh.stage != Some(Stage::Pixel) {
                bail!("light shader stages mismatch");
            }
            Some((lvs, lps))
        }
        None => None,
    };
    // a dynamic light's pass: its pixel shader takes the light's colour
    let dyn_light = light.is_none() && ps.sh.ctab.iter().any(|c| c.name == "LightColorAndFalloffExponent");
    let mut ctx = Ctx { unknown: BTreeSet::new(), slots: 0, light: false, dyn_light };
    let mut m = String::new();
    m.push_str(match platform {
        Platform::Bevy => PLATFORM_BEVY,
        Platform::Standalone => PLATFORM_STANDALONE,
    });
    let group = match platform {
        Platform::Bevy => "#{MATERIAL_BIND_GROUP}",
        Platform::Standalone => "3",
    };
    m.push_str(&COMMON.replace("MATGROUP", group));
    m.push_str(&expression_functions(map, &mut ctx));
    let mut stages: Vec<(&StageT, &str)> = vec![(&vs, "vs_"), (&ps, "ps_")];
    if let Some((lvs, lps)) = &light {
        stages.push((lvs, "lvs_"));
        stages.push((lps, "lps_"));
    }
    for (st, prefix) in &stages {
        m.push_str(&helpers(st, prefix));
        m.push_str(&sm3::stage_decls(&st.sh, &st.iface, prefix));
        m.push_str(&st.body);
    }
    let vs_fill = constant_fill(&vs, "vs_", map, &mut ctx);
    let ps_fill = constant_fill(&ps, "ps_", map, &mut ctx);
    ctx.light = true;
    let (lvs_fill, lps_fill) = match &light {
        Some((lvs, lps)) => (constant_fill(lvs, "lvs_", map, &mut ctx), constant_fill(lps, "lps_", map, &mut ctx)),
        None => (String::new(), String::new()),
    };

    // varyings: every pixel shader input semantic of each pass gets a dense location
    let semantics = |st: &StageT| -> Vec<(u32, u32)> {
        let mut v: Vec<(u32, u32)> = st.iface.inputs.iter().map(|&(_, _, _, u, i)| (u, i)).collect();
        v.sort();
        v.dedup();
        v
    };
    let base_sem = semantics(&ps);
    let light_sem = light.as_ref().map(|(_, lps)| semantics(lps)).unwrap_or_default();
    let mut loc = 0;
    m.push_str("struct UeVertex {\n    @builtin(instance_index) instance_index: u32,\n");
    m.push_str("    @location(0) position: vec3<f32>,\n    @location(1) normal: vec3<f32>,\n    @location(2) uv0: vec2<f32>,\n");
    m.push_str("    @location(3) uv1: vec2<f32>,\n    @location(4) tangent: vec4<f32>,\n    @location(5) color: vec4<f32>,\n");
    if platform == Platform::Bevy {
        m.push_str("#ifdef SKINNED\n    @location(6) joint_indices: vec4<u32>,\n    @location(7) joint_weights: vec4<f32>,\n#endif\n");
    }
    m.push_str("};\n");
    // invariant: the depth prepass and the main pass must agree exactly
    m.push_str("struct UeVaryings {\n    @invariant @builtin(position) position: vec4<f32>,\n");
    for (u, i) in &base_sem {
        let _ = writeln!(m, "    @location({loc}) s{u}_{i}: vec4<f32>,");
        loc += 1;
    }
    for (u, i) in &light_sem {
        let _ = writeln!(m, "    @location({loc}) l{u}_{i}: vec4<f32>,");
        loc += 1;
    }
    let _ = writeln!(m, "    @location({loc}) @interpolate(flat) ids: vec2<u32>,");
    // debugging: the instance's light-map coordinate
    let _ = writeln!(m, "    @location({}) lmuv: vec2<f32>,\n}};", loc + 1);

    // vertex entry: each pass's vertex shader on the same inputs
    m.push_str("fn ue_vertex(v: UeVertex) -> UeVaryings {\n");
    m.push_str("    let tag = ue_tag(v.instance_index);\n");
    if platform == Platform::Bevy {
        m.push_str("#ifdef SKINNED\n    ue_setup_model(skinning::skin_model(v.joint_indices, v.joint_weights, v.instance_index), tag);\n#else\n    ue_setup(v.instance_index, tag);\n#endif\n");
    } else {
        m.push_str("    ue_setup(v.instance_index, tag);\n");
    }
    m.push_str("    var out: UeVaryings;\n");
    let vertex_pass = |m: &mut String, st: &StageT, prefix: &str, fill: &str, sem: &[(u32, u32)], var: &str, position: bool| -> Result<()> {
        m.push_str(fill);
        let _ = writeln!(m, "    {{\n    var io: {prefix}Io;");
        for &(_, ty, reg, usage, index) in &st.iface.inputs {
            if ty == sm3::INPUT {
                let _ = writeln!(m, "    io.v{reg} = {};", local_vf_input(usage, index));
            }
        }
        let _ = writeln!(m, "    {prefix}main(&io);");
        let mut wrote_pos = false;
        for &(_, _, reg, usage, index) in &st.iface.outputs {
            if usage == 0 && index == 0 {
                if position {
                    let _ = writeln!(m, "    out.position = io.o{reg};");
                }
                wrote_pos = true;
            } else if sem.contains(&(usage, index)) {
                let _ = writeln!(m, "    out.{var}{usage}_{index} = io.o{reg};");
            }
        }
        m.push_str("    }\n");
        if !wrote_pos {
            bail!("vertex shader writes no position");
        }
        Ok(())
    };
    vertex_pass(&mut m, &vs, "vs_", &vs_fill, &base_sem, "s", true)?;
    if let Some((lvs, _)) = &light {
        vertex_pass(&mut m, lvs, "lvs_", &lvs_fill, &light_sem, "l", false)?;
    }
    m.push_str("    out.ids = vec2<u32>(v.instance_index, tag);\n    out.lmuv = v.uv1 * ue_inst[ue_base].xy + ue_inst[ue_base].zw;\n    return out;\n}\n");
    m.push_str("@vertex\nfn vertex(v: UeVertex) -> UeVaryings {\n    return ue_vertex(v);\n}\n");
    m.push_str("@vertex\nfn prepass_vertex(v: UeVertex) -> UeVaryings {\n    return ue_vertex(v);\n}\n");
    // depth prepass of masked materials: the base pass pixel shader only for its clipping
    m.push_str("@fragment\nfn prepass_fragment(in: UeVaryings, @builtin(front_facing) face: bool) {\n    ue_setup(in.ids.x, in.ids.y);\n");
    m.push_str(&ps_fill);
    m.push_str("    var io: ps_Io;\n");
    for &(_, ty, reg, usage, index) in &ps.iface.inputs {
        let n = if ty == sm3::INPUT { format!("v{reg}") } else { format!("t{reg}") };
        let _ = writeln!(m, "    io.{n} = in.s{usage}_{index};");
    }
    m.push_str("    io.vpos = vec4<f32>(floor(in.position.xy), 0.0, 1.0);\n    io.vface = vec4<f32>(select(-1.0, 1.0, face));\n    ps_main(&io);\n}\n");

    // fragment entry: base pass plus the light pass
    m.push_str("@fragment\nfn fragment(in: UeVaryings, @builtin(front_facing) face: bool) -> @location(0) vec4<f32> {\n");
    m.push_str("    ue_setup(in.ids.x, in.ids.y);\n");
    if light.is_some() {
        m.push_str("    ue_frag_setup(in.position);\n");
    }
    if ctx.dyn_light {
        // a dominant light's own shadow map page
        let _ = writeln!(m, "    ue_shadow_layer = i32(ue_mat.p[{}].x);\n    ue_shadow_full = 0.0;", DYN_SHADOW_SLOT + 1);
    }
    let pixel_pass = |m: &mut String, st: &StageT, prefix: &str, fill: &str, var: &str, result: &str| {
        m.push_str(fill);
        let _ = writeln!(m, "    var {prefix}io: {prefix}Io;");
        for &(_, ty, reg, usage, index) in &st.iface.inputs {
            let n = if ty == sm3::INPUT { format!("v{reg}") } else { format!("t{reg}") };
            let _ = writeln!(m, "    {prefix}io.{n} = in.{var}{usage}_{index};");
        }
        let _ = writeln!(m, "    {prefix}io.vpos = vec4<f32>(floor(in.position.xy), 0.0, 1.0);\n    {prefix}io.vface = vec4<f32>(select(-1.0, 1.0, face));");
        let _ = writeln!(m, "    {prefix}main(&{prefix}io);");
        if sm3::output_regs(&st.sh, sm3::COLOROUT).contains(&0) {
            let _ = writeln!(m, "    {result}{prefix}io.oc0;");
        }
    };
    m.push_str("    var color = vec4<f32>(0.0, 0.0, 0.0, 1.0);\n");
    pixel_pass(&mut m, &ps, "ps_", &ps_fill, "s", "color = ");
    if let Some((_, lps)) = &light {
        pixel_pass(&mut m, lps, "lps_", &lps_fill, "l", "color = color + vec4<f32>(1.0, 1.0, 1.0, 0.0) * ");
    }
    // DH_UE3_DEBUG: base | light | nac | dmc | ambient | atten
    match std::env::var("DH_UE3_DEBUG").as_deref() {
        Ok("base") => {
            m.push_str("    color = ps_io.oc0;\n");
        }
        Ok("light") if light.is_some() => {
            m.push_str("    color = vec4<f32>(lps_io.oc0.rgb, 1.0);\n");
        }
        Ok("nac") => m.push_str("    color = textureSample(ue_lm0, ue_lm_s, in.lmuv, ue_lm_layer);\n"),
        Ok("dmc") => m.push_str("    color = textureSample(ue_lm1, ue_lm_s, in.lmuv, ue_lm_layer);\n"),
        Ok("ambient") => m.push_str("    color = ue_inst[ue_base + 5u];\n"),
        Ok("atten") if light.is_some() => m.push_str("    color = vec4<f32>(ue_light_attenuation(vec4<f32>(1.0)).rgb, 1.0);\n"),
        Ok("attenlights") if light.is_some() && platform == Platform::Bevy => m.push_str(
            "#ifndef PREPASS_PIPELINE\n    color = vec4<f32>(f32(lights.n_directional_lights) * 0.5, f32(lights.directional_lights[0].num_cascades) * 0.25, f32(lights.directional_lights[0].flags & 1u), 1.0);\n#endif\n",
        ),
        Ok("depth") if platform == Platform::Bevy => m.push_str(
            "#ifdef DEPTH_PREPASS\n    color = vec4<f32>(vec3<f32>(fract(ue_scene_color(in.position.xy / view.viewport.zw).a * 256.0 / 500.0)), 1.0);\n#endif\n",
        ),
        _ => {}
    }
    m.push_str("    return color;\n}\n");
    let _ = ctx.slots;
    Ok(Program { wgsl: m, unknown: ctx.unknown })
}

/// The parameter slots of a shader map (for sizing / checking a material's values).
pub fn param_count(map: &CookedMap) -> usize {
    param_nodes(map).len()
}

/// Inputs a post-process material reads (`GArkPpSampler_N`) and its own textures.
pub const POST_INPUTS: usize = 4;
pub const POST_TEXTURES: usize = 4;

/// A post-process material (`bUsedWithArkPostProcess`: `PPG_*`, Dark Vision's, the eyelid's)
/// as a full-screen pass: its `TPpMaterialPixelShader` translated, behind a full-screen
/// triangle giving it the screen coordinates (TEXCOORD0) its quad would have. Bindings
/// (group 0): 0 the uniform (`p`: parameter slots in `param_nodes` order, `time`, `res`:
/// width, height, 1 / width, 1 / height), 1..4 the material's texture samplers, 5..8 its
/// textures (`Texture2D_N`), 9..12 the node inputs' samplers, 13..16 the node inputs
/// (`GArkPpSampler_N`; `SceneColorTexture` reads the first). Entry points `post_vertex` and
/// `post_fragment`.
pub fn build_post(map: &CookedMap, ps_code: &[u8]) -> Result<Program> {
    let ps = stage(ps_code, "ps_")?;
    if ps.sh.stage != Some(Stage::Pixel) {
        bail!("not a pixel shader");
    }
    let mut ctx = Ctx { unknown: BTreeSet::new(), slots: 0, light: false, dyn_light: false };
    let mut m = String::new();
    let _ = writeln!(m, "struct UePost {{ p: array<vec4<f32>, {MAX_PARAMS}>, time: vec4<f32>, res: vec4<f32> }};");
    m.push_str("@group(0) @binding(0) var<uniform> ue_mat: UePost;\n");
    for k in 0..POST_TEXTURES {
        let _ = writeln!(m, "@group(0) @binding({}) var ue_s{k}: sampler;", 1 + k);
        let _ = writeln!(m, "@group(0) @binding({}) var ue_t{k}: texture_2d<f32>;", 1 + POST_TEXTURES + k);
    }
    for k in 0..POST_INPUTS {
        let _ = writeln!(m, "@group(0) @binding({}) var ue_pps{k}: sampler;", 1 + 2 * POST_TEXTURES + k);
        let _ = writeln!(m, "@group(0) @binding({}) var ue_pp{k}: texture_2d<f32>;", 1 + 2 * POST_TEXTURES + POST_INPUTS + k);
    }
    m.push_str("fn ue_time() -> f32 { return ue_mat.time.x; }\n");
    m.push_str("fn ue_post_res() -> vec4<f32> { return ue_mat.res; }\n");
    m.push_str("fn ue_scene_color(uv: vec2<f32>) -> vec4<f32> { return textureSample(ue_pp0, ue_pps0, uv); }\n");
    m.push_str("fn ue_fmod(a: vec4<f32>, b: vec4<f32>) -> vec4<f32> { return a - b * trunc(a / b); }\n");
    m.push_str("fn ue_div(a: vec4<f32>, b: vec4<f32>) -> vec4<f32> { return select(a / b, vec4<f32>(0.0), b == vec4<f32>(0.0)); }\n");
    m.push_str(
        "fn ue_append(a: vec4<f32>, b: vec4<f32>, n: u32) -> vec4<f32> {\n    if n == 1u { return vec4<f32>(a.x, b.x, b.y, b.z); }\n    if n == 2u { return vec4<f32>(a.x, a.y, b.x, b.y); }\n    return vec4<f32>(a.x, a.y, a.z, b.x);\n}\n",
    );
    m.push_str(&expression_functions(map, &mut ctx));
    m.push_str(&helpers(&ps, "ps_"));
    m.push_str(&sm3::stage_decls(&ps.sh, &ps.iface, "ps_"));
    m.push_str(&ps.body);
    let fill = constant_fill(&ps, "ps_", map, &mut ctx);
    m.push_str("struct UePostOut {\n    @builtin(position) position: vec4<f32>,\n    @location(0) uv: vec2<f32>,\n};\n");
    m.push_str(
        "@vertex\nfn post_vertex(@builtin(vertex_index) i: u32) -> UePostOut {\n    let uv = vec2<f32>(f32(i >> 1u), f32(i & 1u)) * 2.0;\n    var out: UePostOut;\n    out.position = vec4<f32>(uv * vec2<f32>(2.0, -2.0) + vec2<f32>(-1.0, 1.0), 0.0, 1.0);\n    out.uv = uv;\n    return out;\n}\n",
    );
    m.push_str("@fragment\nfn post_fragment(in: UePostOut) -> @location(0) vec4<f32> {\n    var ps_io: ps_Io;\n");
    for &(_, _, reg, usage, index) in &ps.iface.inputs {
        // TEXCOORD0: the quad's coordinates; the rest unused by the original's quad
        let v = if usage == 5 && index == 0 { "vec4<f32>(in.uv, 0.0, 1.0)" } else { "vec4<f32>(0.0)" };
        let _ = writeln!(m, "    ps_io.v{reg} = {v};");
    }
    m.push_str("    ps_io.vpos = vec4<f32>(floor(in.position.xy), 0.0, 1.0);\n    ps_io.vface = vec4<f32>(1.0);\n");
    m.push_str(&fill);
    m.push_str("    ps_main(&ps_io);\n");
    if sm3::output_regs(&ps.sh, sm3::COLOROUT).contains(&0) {
        m.push_str("    return ps_io.oc0;\n}\n");
    } else {
        m.push_str("    return vec4<f32>(0.0);\n}\n");
    }
    Ok(Program { wgsl: m, unknown: ctx.unknown })
}


/// One of Arkane's global post-processing pixel shaders (`FArkPpMotionBlurPixelShader`...) as
/// a full-screen pass: its samplers bound in constant-table order (binding 1 + 2i the
/// sampler, 2 + 2i the texture), its constants from a uniform of 16 vectors in constant-table
/// order (binding 0). Returns the module and the samplers' and constants' names.
pub fn build_global(ps_code: &[u8]) -> Result<(String, Vec<String>, Vec<String>)> {
    let ps = stage(ps_code, "ps_")?;
    if ps.sh.stage != Some(Stage::Pixel) {
        bail!("not a pixel shader");
    }
    let samplers: Vec<&sm3::CtabEntry> = ps.sh.ctab.iter().filter(|c| c.set == 3).collect();
    let consts: Vec<&sm3::CtabEntry> = ps.sh.ctab.iter().filter(|c| c.set == 2).collect();
    let mut m = String::new();
    m.push_str("struct UeGlobal { c: array<vec4<f32>, 16> };\n@group(0) @binding(0) var<uniform> ue_g: UeGlobal;\n");
    for (i, c) in samplers.iter().enumerate() {
        let _ = writeln!(m, "@group(0) @binding({}) var ue_gs{}: sampler;", 1 + 2 * i, c.reg);
        let _ = writeln!(m, "@group(0) @binding({}) var ue_gt{}: texture_2d<f32>;", 2 + 2 * i, c.reg);
    }
    let binds = |s: u32| (format!("ue_gt{s}"), format!("ue_gs{s}"), String::new(), String::new());
    m.push_str(&sm3::sampling_helpers(&ps.iface, "ps_", false, &binds));
    m.push_str(&sm3::stage_decls(&ps.sh, &ps.iface, "ps_"));
    m.push_str(&ps.body);
    m.push_str("struct UeGlobalOut {\n    @builtin(position) position: vec4<f32>,\n    @location(0) uv: vec2<f32>,\n};\n");
    m.push_str(
        "@vertex\nfn post_vertex(@builtin(vertex_index) i: u32) -> UeGlobalOut {\n    let uv = vec2<f32>(f32(i >> 1u), f32(i & 1u)) * 2.0;\n    var out: UeGlobalOut;\n    out.position = vec4<f32>(uv * vec2<f32>(2.0, -2.0) + vec2<f32>(-1.0, 1.0), 0.0, 1.0);\n    out.uv = uv;\n    return out;\n}\n",
    );
    m.push_str("@fragment\nfn post_fragment(in: UeGlobalOut) -> @location(0) vec4<f32> {\n    var ps_io: ps_Io;\n");
    for &(_, _, reg, usage, index) in &ps.iface.inputs {
        let v = if usage == 5 && index == 0 { "vec4<f32>(in.uv, 0.0, 1.0)" } else { "vec4<f32>(0.0)" };
        let _ = writeln!(m, "    ps_io.v{reg} = {v};");
    }
    m.push_str("    ps_io.vpos = vec4<f32>(floor(in.position.xy), 0.0, 1.0);\n    ps_io.vface = vec4<f32>(1.0);\n");
    for (k, c) in consts.iter().enumerate() {
        for i in 0..c.count.max(1) as u32 {
            let r = c.reg as u32 + i;
            if r <= ps.iface.max_const && (ps.iface.uses_rel_const || ps.iface.consts.contains(&r)) {
                let _ = writeln!(m, "    ps_kc[{r}] = ue_g.c[{}];", (k as u32 + i).min(15));
            }
        }
    }
    m.push_str("    ps_main(&ps_io);\n    return ps_io.oc0;\n}\n");
    Ok((m, samplers.iter().map(|c| c.name.clone()).collect(), consts.iter().map(|c| c.name.clone()).collect()))
}
