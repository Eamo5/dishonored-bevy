//! Materials rendered with their original shaders.
//!
//! Each material's compiled UE3 base pass shaders (D3D9 shader model 3 bytecode from the
//! game's shader cache, cooked into `shaders.bin`) are translated to WGSL and assembled into
//! a program (`dhcook::ue3prog`) when a level spawns. A single material type renders them
//! all: its pipeline specialization swaps in the program of the material's key. The bind
//! group carries the material's parameter values and textures, the per-instance data
//! buffer (light-map placement and scales) and the light-map page arrays.
//! `DH_UE3=0` falls back to the approximated standard materials.

use bevy::mesh::MeshVertexBufferLayoutRef;
use bevy::pbr::{MaterialPipeline, MaterialPipelineKey};
use bevy::prelude::*;
use bevy::reflect::TypePath;
use bevy::render::render_resource::{
    AsBindGroup, BlendComponent, BlendFactor, BlendOperation, BlendState, Face, RenderPipelineDescriptor, ShaderType,
    SpecializedMeshPipelineError,
};
use bevy::render::storage::ShaderBuffer;
use dhcook::format::Blend;
use dhcook::shadercache::CookedLibrary;
use dhcook::ue3prog::{self, Policy, MAX_PARAMS};
use std::collections::HashMap;
use std::sync::{Arc, OnceLock, RwLock};

pub struct Ue3Plugin;

impl Plugin for Ue3Plugin {
    fn build(&self, app: &mut App) {
        bevy::asset::embedded_asset!(app, "ue3_prepass.wgsl");
        app.add_plugins(MaterialPlugin::<Ue3Material>::default()).init_resource::<Ue3Programs>();
    }
}

pub fn enabled() -> bool {
    std::env::var("DH_UE3").map(|v| v != "0").unwrap_or(true)
}

static LIBRARY: OnceLock<Option<Arc<CookedLibrary>>> = OnceLock::new();

/// The cooked shader library (`shaders.bin`), read on first use.
pub fn library() -> Option<Arc<CookedLibrary>> {
    LIBRARY
        .get_or_init(|| {
            let path = dhcook::shader_library_path(&crate::loading::cache_dir());
            match CookedLibrary::read(&path) {
                Ok(l) => {
                    info!("shader library: {} maps, {} shaders", l.maps.len(), l.code.len());
                    Some(Arc::new(l))
                }
                Err(e) => {
                    warn!("shader library {}: {e:#}", path.display());
                    None
                }
            }
        })
        .clone()
}

/// Program shaders, indexed by the program id in a material's key.
static PROGRAMS: RwLock<Vec<Handle<Shader>>> = RwLock::new(Vec::new());

/// Programs built so far, by (shader map, light-map policy).
#[derive(Resource, Default)]
pub struct Ue3Programs {
    by_key: HashMap<(u32, Policy, bool), Option<u32>>,
}

impl Ue3Programs {
    /// The program of a shader map for a light-map policy, with or without the dominant
    /// light's pass (built on first request).
    pub fn get(&mut self, map: u32, policy: Policy, with_light: bool, shaders: &mut Assets<Shader>) -> Option<u32> {
        if let Some(p) = self.by_key.get(&(map, policy, with_light)) {
            return *p;
        }
        let r = build_program(map, policy, with_light, shaders);
        self.by_key.insert((map, policy, with_light), r);
        r
    }
}

fn build_program(map_id: u32, policy: Policy, with_light: bool, shaders: &mut Assets<Shader>) -> Option<u32> {
    let lib = library()?;
    let map = lib.maps.get(map_id as usize)?;
    let (vst, pst) = policy.shader_types();
    let vs = lib.code.get(&map.shader("FLocalVertexFactory", vst)?)?;
    let ps = lib.code.get(&map.shader("FLocalVertexFactory", pst)?)?;
    // opaque surfaces also take the dominant light's pass
    // the dominant light through its distance-field shadow maps (else the plain factor variant)
    // (`DH_SUN_FACTOR` forces the plain variant, for comparison)
    let order = if std::env::var("DH_SUN_FACTOR").is_ok() { [Policy::SUN_FACTOR, Policy::SUN] } else { [Policy::SUN, Policy::SUN_FACTOR] };
    let sun = order
        .into_iter()
        .find_map(|(lv, lp)| Some((lib.code.get(&map.shader("FLocalVertexFactory", lv)?)?.as_slice(), lib.code.get(&map.shader("FLocalVertexFactory", lp)?)?.as_slice())))
        .filter(|_| with_light);
    let prog = match ue3prog::build(map, vs, ps, sun, ue3prog::Platform::Bevy) {
        Ok(p) => p,
        Err(e) => {
            warn!("program {} {policy:?}: {e:#}", map.name);
            return None;
        }
    };
    if !prog.unknown.is_empty() {
        debug!("program {} {policy:?}: unsupported constants {:?}", map.name, prog.unknown);
    }
    if std::env::var("DH_UE3_DUMP").is_ok() {
        let dir = crate::loading::cache_dir().join("debug").join("programs");
        let _ = std::fs::create_dir_all(&dir);
        let _ = std::fs::write(dir.join(format!("{map_id}_{policy:?}.wgsl")), &prog.wgsl);
    }
    // debugging: hand-edited programs override the generated ones; `DH_UE3_DEBUG=id`
    // tints each program by its shader map (logged with the colour)
    let mut wgsl = prog.wgsl;
    if std::env::var("DH_UE3_DEBUG").as_deref() == Ok("id") {
        // map id in base 16 per channel (decodable from an untonemapped screenshot)
        let c = [(map_id % 16) as f32 / 15.0, ((map_id / 16) % 16) as f32 / 15.0, ((map_id / 256) % 16) as f32 / 15.0];
        info!("program {map_id} {} {policy:?}: colour {:.2?}", map.name, c);
        wgsl = wgsl.replace("    return color;
}", &format!("    return vec4<f32>({:?}, {:?}, {:?}, 1.0);
}}", c[0], c[1], c[2]));
    }
    if let Ok(dir) = std::env::var("DH_UE3_PATCH_DIR") {
        if let Ok(p) = std::fs::read_to_string(std::path::Path::new(&dir).join(format!("{map_id}_{policy:?}.wgsl"))) {
            info!("program {map_id} {policy:?}: patched from {dir}");
            wgsl = p;
        }
    }
    let handle = shaders.add(Shader::from_wgsl(wgsl, format!("ue3/{map_id}_{policy:?}.wgsl")));
    let mut programs = PROGRAMS.write().unwrap();
    programs.push(handle);
    Some(programs.len() as u32 - 1)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Ue3Key {
    pub program: u32,
    /// 0 opaque, 1 masked, 2 translucent, 3 additive, 4 modulate
    pub blend: u8,
    pub two_sided: bool,
    /// first-person view model (UE3's foreground depth priority group)
    pub foreground: bool,
    /// a character in the foreground group at the world's field of view (the Outsider:
    /// `DisSeqAct_OutsiderConfig` `SDPG_Foreground`)
    pub foreground_world: bool,
    /// a dynamic light's additive pass over an opaque surface (drawn just in front of it)
    pub light_pass: bool,
}

impl Ue3Key {
    pub fn new(program: u32, blend: Blend, two_sided: bool) -> Self {
        let blend = match blend {
            Blend::Opaque => 0,
            Blend::Masked => 1,
            Blend::Translucent => 2,
            Blend::Additive => 3,
            Blend::Modulate => 4,
        };
        Ue3Key { program, blend, two_sided, foreground: false, foreground_world: false, light_pass: false }
    }
}

#[derive(ShaderType, Debug, Clone, Copy)]
pub struct Ue3Params {
    pub p: [Vec4; MAX_PARAMS],
}

impl Default for Ue3Params {
    fn default() -> Self {
        Ue3Params { p: [Vec4::ZERO; MAX_PARAMS] }
    }
}

#[derive(Asset, AsBindGroup, TypePath, Debug, Clone)]
#[bind_group_data(Ue3Key)]
pub struct Ue3Material {
    #[uniform(0)]
    pub params: Ue3Params,
    #[texture(1)]
    #[sampler(2)]
    pub t0: Handle<Image>,
    #[texture(3)]
    #[sampler(4)]
    pub t1: Handle<Image>,
    #[texture(5)]
    #[sampler(6)]
    pub t2: Handle<Image>,
    #[texture(7)]
    #[sampler(8)]
    pub t3: Handle<Image>,
    #[texture(9)]
    #[sampler(10)]
    pub t4: Handle<Image>,
    #[texture(11)]
    #[sampler(12)]
    pub t5: Handle<Image>,
    #[texture(13)]
    #[sampler(14)]
    pub t6: Handle<Image>,
    #[texture(15)]
    #[sampler(16)]
    pub t7: Handle<Image>,
    #[texture(17)]
    #[sampler(18)]
    pub t8: Handle<Image>,
    #[texture(19)]
    #[sampler(20)]
    pub t9: Handle<Image>,
    #[texture(21)]
    #[sampler(22)]
    pub t10: Handle<Image>,
    #[texture(23)]
    #[sampler(24)]
    pub t11: Handle<Image>,
    #[texture(25, dimension = "cube")]
    #[sampler(26)]
    pub cube0: Handle<Image>,
    #[storage(27, read_only)]
    pub instances: Handle<ShaderBuffer>,
    #[texture(28, dimension = "2d_array")]
    pub lm0: Handle<Image>,
    #[texture(29, dimension = "2d_array")]
    #[sampler(30)]
    pub lm1: Handle<Image>,
    #[texture(31, dimension = "2d_array")]
    #[sampler(32)]
    pub shadow: Handle<Image>,
    #[texture(33)]
    #[sampler(34)]
    pub white: Handle<Image>,
    #[texture(35)]
    pub black: Handle<Image>,
    #[texture(36, dimension = "cube")]
    pub black_cube: Handle<Image>,
    /// the level's reflection cube (`SceneReflectionTexture`)
    #[texture(37, dimension = "cube")]
    pub refl: Handle<Image>,
    pub key: Ue3Key,
    /// its shader map, and whether it takes dynamic light passes (lit, opaque or masked)
    pub map: u32,
    /// the scene material it was made from (`u32::MAX`: none), for parameters set at runtime
    pub mat_id: u32,
    pub light_ok: bool,
}

impl From<&Ue3Material> for Ue3Key {
    fn from(m: &Ue3Material) -> Self {
        m.key
    }
}

impl Ue3Material {
    pub fn set_texture(&mut self, i: usize, h: Handle<Image>) {
        let slot = match i {
            0 => &mut self.t0,
            1 => &mut self.t1,
            2 => &mut self.t2,
            3 => &mut self.t3,
            4 => &mut self.t4,
            5 => &mut self.t5,
            6 => &mut self.t6,
            7 => &mut self.t7,
            8 => &mut self.t8,
            9 => &mut self.t9,
            10 => &mut self.t10,
            11 => &mut self.t11,
            _ => return,
        };
        *slot = h;
    }
}

impl Material for Ue3Material {
    fn alpha_mode(&self) -> AlphaMode {
        match self.key.blend {
            0 => AlphaMode::Opaque,
            1 => AlphaMode::Mask(0.5),
            2 => AlphaMode::Blend,
            3 => AlphaMode::Add,
            _ => AlphaMode::Multiply,
        }
    }

    // The depth prepass runs the programs' prepass entry points (selected in `specialize`;
    // this placeholder only makes Bevy create a fragment stage for masked materials).
    fn prepass_fragment_shader() -> bevy::shader::ShaderRef {
        "embedded://dishonored/ue3_prepass.wgsl".into()
    }

    // dynamic objects (characters, props) cast into the dominant directional light's shadow
    // map (the world is `NotShadowCaster`: its shadows are baked)
    fn enable_shadows() -> bool {
        true
    }

    fn specialize(
        _pipeline: &MaterialPipeline,
        descriptor: &mut RenderPipelineDescriptor,
        layout: &MeshVertexBufferLayoutRef,
        key: MaterialPipelineKey<Self>,
    ) -> Result<(), SpecializedMeshPipelineError> {
        let k = key.bind_group_data;
        let shader = PROGRAMS.read().unwrap().get(k.program as usize).cloned();
        // materials are only created once their program is registered
        let Some(shader) = shader else { return Ok(()) };
        let prepass = descriptor.vertex.shader_defs.iter().any(|d| matches!(d, bevy::shader::ShaderDefVal::Bool(n, true) if n == "PREPASS_PIPELINE"));
        if k.foreground || k.foreground_world {
            descriptor.vertex.shader_defs.push("UE_FOREGROUND".into());
        }
        if k.foreground_world {
            descriptor.vertex.shader_defs.push("UE_FOREGROUND_WORLD".into());
        }
        descriptor.vertex.shader = shader.clone();
        descriptor.vertex.entry_point = Some(if prepass { "prepass_vertex" } else { "vertex" }.into());
        let mut attributes = vec![
            Mesh::ATTRIBUTE_POSITION.at_shader_location(0),
            Mesh::ATTRIBUTE_NORMAL.at_shader_location(1),
            Mesh::ATTRIBUTE_UV_0.at_shader_location(2),
            Mesh::ATTRIBUTE_UV_1.at_shader_location(3),
            Mesh::ATTRIBUTE_TANGENT.at_shader_location(4),
            Mesh::ATTRIBUTE_COLOR.at_shader_location(5),
        ];
        if layout.0.contains(Mesh::ATTRIBUTE_JOINT_INDEX) && layout.0.contains(Mesh::ATTRIBUTE_JOINT_WEIGHT) {
            attributes.push(Mesh::ATTRIBUTE_JOINT_INDEX.at_shader_location(6));
            attributes.push(Mesh::ATTRIBUTE_JOINT_WEIGHT.at_shader_location(7));
        }
        descriptor.vertex.buffers = vec![layout.0.get_layout(&attributes)?];
        descriptor.primitive.cull_mode = if k.two_sided { None } else { Some(Face::Back) };
        if k.light_pass {
            // the light pass's vertex shader places the surface where the base pass did, up
            // to rounding: a nudge towards the camera (reverse Z) keeps it on top
            if let Some(ds) = descriptor.depth_stencil.as_mut() {
                ds.bias.constant = 4;
                ds.bias.slope_scale = 1.0;
                ds.depth_write_enabled = Some(false);
            }
        }
        if k.foreground || k.foreground_world {
            if let Some(fragment) = descriptor.fragment.as_mut() {
                fragment.shader_defs.push("UE_FOREGROUND".into());
                if k.foreground_world {
                    fragment.shader_defs.push("UE_FOREGROUND_WORLD".into());
                }
            }
        }
        if prepass {
            if let Some(fragment) = descriptor.fragment.as_mut() {
                fragment.shader = shader;
                fragment.entry_point = Some("prepass_fragment".into());
            }
            return Ok(());
        }
        if let Some(fragment) = descriptor.fragment.as_mut() {
            fragment.shader = shader;
            fragment.entry_point = Some("fragment".into());
            let add = |src: BlendFactor, dst: BlendFactor| BlendComponent { src_factor: src, dst_factor: dst, operation: BlendOperation::Add };
            let blend = match k.blend {
                2 => Some(BlendState {
                    color: add(BlendFactor::SrcAlpha, BlendFactor::OneMinusSrcAlpha),
                    alpha: add(BlendFactor::One, BlendFactor::OneMinusSrcAlpha),
                }),
                3 => Some(BlendState { color: add(BlendFactor::One, BlendFactor::One), alpha: add(BlendFactor::Zero, BlendFactor::One) }),
                4 => Some(BlendState { color: add(BlendFactor::Dst, BlendFactor::Zero), alpha: add(BlendFactor::Zero, BlendFactor::One) }),
                _ => None,
            };
            for t in fragment.targets.iter_mut().flatten() {
                t.blend = blend;
            }
        }
        Ok(())
    }
}
