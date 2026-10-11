//! Dishonored map cooker: converts UE3 level packages into a Bevy-friendly cache.

pub mod audio;
pub mod format;
pub mod gamedata;
pub mod kismet;
pub mod materials;
pub mod movies;
pub mod resolver;
pub mod shadercache;
pub mod sm3;
pub mod swf;
pub mod swfvec;
pub mod ttf;
pub mod ue3prog;
pub mod edge;
pub mod facefx;
pub mod navmesh;
pub mod skeletal;
pub mod xform;

use anyhow::{bail, Context, Result};
use format::*;
use glam::{Mat4, Vec3};
use materials::{resolve_material, ResolvedMaterial};
use rayon::prelude::*;
use resolver::{Assets, Obj};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use upk::props::{object_array, parse_struct_array};
use upk::texture::PixelFormat;
use upk::{Props, Reader, Value};
use xform::*;

pub const DEFAULT_GAME_DIR: &str = r"S:\Games\SteamLibrary\steamapps\common\Dishonored";

/// Locate the Dishonored CookedPCConsole directory.
pub fn find_cooked_dir() -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Ok(p) = std::env::var("DISHONORED_DIR") {
        candidates.push(PathBuf::from(p));
    }
    candidates.push(PathBuf::from(DEFAULT_GAME_DIR));
    for drive in ["C", "D", "E", "F", "S"] {
        candidates.push(PathBuf::from(format!(r"{drive}:\Program Files (x86)\Steam\steamapps\common\Dishonored")));
        candidates.push(PathBuf::from(format!(r"{drive}:\SteamLibrary\steamapps\common\Dishonored")));
        candidates.push(PathBuf::from(format!(r"{drive}:\Games\SteamLibrary\steamapps\common\Dishonored")));
    }
    for c in candidates {
        let cooked = c.join("DishonoredGame").join("CookedPCConsole");
        if cooked.join("Startup.upk").exists() {
            return Some(cooked);
        }
        if c.join("Startup.upk").exists() {
            return Some(c);
        }
    }
    None
}

#[derive(Clone)]
pub struct CookOptions {
    pub max_texture_size: u32,
    pub force: bool,
    pub include_streaming: bool,
}

impl Default for CookOptions {
    fn default() -> Self {
        Self { max_texture_size: 2048, force: false, include_streaming: true }
    }
}

pub type Progress<'a> = &'a (dyn Fn(f32, &str) + Sync);

#[derive(Clone)]
enum TexJob {
    Plain(Obj),
    /// Diffuse with alpha taken from a separate opacity mask.
    Masked(Obj, Obj),
    /// A single-channel mask (R, or alpha for DXT3/5) expanded to grey RGB + alpha.
    Mask(Obj),
    /// A page of packed lightmaps: each entry is a Dishonored directional lightmap pair
    /// (NormalizedAverageColor, DirectionalMaxComponent) flattened into irradiance and
    /// copied to (x, y) inside a `size`x`size` page.
    LightmapPage(u32, Vec<(LmPair, u32, u32)>),
    /// The same page layout holding one of the pair's textures as stored (0 = normalized
    /// average colour, 1 = directional max components), for the original shaders.
    LightmapRaw(u32, Vec<(LmPair, u32, u32)>, u8),
    /// Packed G8 dominant-light shadow factor textures.
    ShadowPage(u32, Vec<(Obj, u32, u32)>),
}

#[derive(Clone)]
struct LmPair {
    nac: Obj,
    dmc: Obj,
    s_nac: [f32; 3],
    s_dmc: [f32; 3],
    size: (u32, u32),
}

const LM_PAGE: u32 = 4096;

/// Lightmap reference parsed from a StaticMeshComponent's LOD 0 data.
struct ComponentLightmap {
    shadow_maps: Vec<Obj>,
    textures: [Option<Obj>; 3],
    scales: [[f32; 3]; 3],
    coord_scale: [f32; 2],
    coord_bias: [f32; 2],
}

fn read_component_lightmap(assets: &Assets, comp: &Obj) -> Option<ComponentLightmap> {
    let od = upk::read_object(&comp.pkg, comp.idx).ok()?;
    let mut r = od.reader;
    let nlod = r.count(8).ok()?;
    if nlod == 0 {
        return None;
    }
    let n = r.count(4).ok()?; // ShadowMaps
    let mut shadow_maps = Vec::new();
    for _ in 0..n {
        let o = r.i32().ok()?;
        if let Some(sm) = assets.resolve(&comp.pkg, o) {
            shadow_maps.push(sm);
        }
    }
    let n = r.count(4).ok()?; // ShadowVertexBuffers
    r.skip(n * 4).ok()?;
    if r.u32().ok()? != 2 {
        // no texture lightmap, but shadow maps may still matter
        return Some(ComponentLightmap {
            shadow_maps,
            textures: [None, None, None],
            scales: [[1.0; 3]; 3],
            coord_scale: [0.0; 2],
            coord_bias: [0.0; 2],
        });
    }
    let n = r.count(16).ok()?; // LightGuids
    r.skip(n * 16).ok()?;
    let mut textures: [Option<Obj>; 3] = [None, None, None];
    let mut scales = [[1.0f32; 3]; 3];
    for i in 0..3 {
        let t = r.i32().ok()?;
        scales[i] = r.vec3().ok()?;
        if t != 0 {
            textures[i] = assets.resolve(&comp.pkg, t);
        }
    }
    let coord_scale = [r.f32().ok()?, r.f32().ok()?];
    let coord_bias = [r.f32().ok()?, r.f32().ok()?];
    if !(coord_scale[0].is_finite() && coord_scale[1].is_finite() && coord_scale[0] > 0.0) {
        return None;
    }
    Some(ComponentLightmap { shadow_maps, textures, scales, coord_scale, coord_bias })
}

struct Cooker<'a> {
    assets: &'a Assets,
    root: PathBuf,
    opts: CookOptions,
    scene: Scene,
    mesh_ids: HashMap<String, u32>,
    mat_ids: HashMap<String, u32>,
    tex_ids: HashMap<String, u32>,
    tex_jobs: Vec<(u32, TexJob)>,
    props_cache: HashMap<(usize, i32), Arc<Props>>,
    failed_meshes: HashSet<String>,
    mesh_section_mats: HashMap<String, Vec<u32>>,
    /// door meshes' keyhole sockets in mesh space
    keyhole_sockets: HashMap<String, Option<Vec3>>,
    /// usable objects by actor key (`scene.usables`)
    usable_ids: HashMap<String, u32>,
    /// skeletal level instances: their mesh and their component's animation sets (for the
    /// props the matinees animate)
    skel_insts: HashMap<u32, (Obj, Vec<Obj>)>,
    /// the map's faces and lines' facial animations, and the faces cooked (by object key)
    facefx: FaceFx,
    facefx_ids: HashMap<String, u32>,
    /// the contact system's intersections: (striker, struck) contact type names -> its subs
    /// (sound, particle, AI noise objects)
    contacts: Option<HashMap<(String, String), [Option<Obj>; 3]>>,
    lm_pairs: Vec<LmPair>,
    lm_pair_ids: HashMap<String, u32>,
    sun_guids: Vec<[u32; 4]>,
    npc_type_ids: HashMap<String, Option<u32>>,
    ragdoll_ids: HashMap<String, Option<u32>>,
    skeleton_ids: HashMap<String, u32>,
    anim_set_ids: HashMap<String, Option<u32>>,
    bark_ids: HashMap<String, Option<u32>>,
    /// AnimSets to bind once every character mesh is known
    pending_anims: Vec<PendingAnims>,
    shadow_texs: Vec<Obj>,
    shadow_tex_ids: HashMap<String, u32>,
    /// dominant spot / point lights by `LightGuid`
    light_guids: HashMap<[u32; 4], u32>,
    /// per instance, before the lights are known: (instance, light guid, shadow map)
    pending_light_shadows: Vec<(u32, [u32; 4], LightMapRef)>,
    pending_irrelevant: Vec<(u32, Vec<[u32; 4]>)>,
    progress: Progress<'a>,
    warnings: usize,
    /// (package, export) of each entry in `scene.spawners`.
    spawner_keys: Vec<kismet::ActorKey>,
    /// each spawner's stealable pickup actor, resolved once every level is read
    stealable_keys: Vec<Option<kismet::ActorKey>>,
    /// spawners' tether volumes, resolved once the volumes are in
    tether_keys: Vec<Vec<kismet::ActorKey>>,
    /// flee points' next points (actor keys), and each point's key
    flee_keys: Vec<(kismet::ActorKey, Option<kismet::ActorKey>)>,
    /// physics joints: name, the actors they bind
    joint_keys: Vec<(String, Vec<kismet::ActorKey>)>,
    /// the sound propagation's rooms by actor, and each doorway's rooms
    audio_cell_keys: HashMap<kismet::ActorKey, u32>,
    portal_cell_keys: Vec<[Option<kismet::ActorKey>; 2]>,
    /// distractors' scenes, cooked once the level scripts are: (distractor, MatineeData)
    pending_soirees: Vec<(usize, Obj)>,
    /// Height of the fog layer currently in `scene.fog`.
    fog_height: f32,
    /// What each level actor became in the scene, for the level scripts.
    actor_refs: HashMap<kismet::ActorKey, KActor>,
    /// actors attached to another (`Base`): rider -> base
    actor_base: HashMap<kismet::ActorKey, kismet::ActorKey>,
    particle_system_ids: HashMap<String, Option<u32>>,
}

static SHADER_LIBRARY: std::sync::OnceLock<Option<shadercache::ShaderLibrary>> = std::sync::OnceLock::new();

/// The game's reference shader cache (compiled material shaders), loaded once.
pub fn shader_library(assets: &Assets) -> Option<&'static shadercache::ShaderLibrary> {
    SHADER_LIBRARY
        .get_or_init(|| {
            let pkg = assets.package("RefShaderCache-PC-D3D-SM3")?;
            match shadercache::ShaderLibrary::load(&pkg) {
                Ok(l) => {
                    log::info!("shader cache: {} shaders, {} material shader maps", l.records.len(), l.maps.len());
                    Some(l)
                }
                Err(e) => {
                    log::warn!("shader cache unreadable: {e:#}");
                    None
                }
            }
        })
        .as_ref()
}

pub fn shader_library_path(root: &Path) -> PathBuf {
    root.join("shaders.bin")
}

/// Write the cooked shader library (`shaders.bin`) if it is missing.
pub fn cook_shader_library(assets: &Assets, root: &Path, force: bool) -> Result<()> {
    let path = shader_library_path(root);
    if path.exists() && !force {
        return Ok(());
    }
    let Some(lib) = shader_library(assets) else { bail!("no shader cache") };
    let mut cooked = shadercache::CookedLibrary::from_library(lib);
    // Arkane's post-processing shaders from the global shader cache
    match std::fs::read(assets.cooked_dir.join("GlobalShaderCache-PC-D3D-SM3.bin")).map_err(anyhow::Error::from).and_then(|d| shadercache::global_shaders(&d)) {
        Ok(globals) => {
            for g in globals.into_iter().filter(|g| g.name.starts_with("FArkPp") || g.name.starts_with("FKuwa")) {
                cooked.globals.insert(g.name, g.code);
            }
        }
        Err(e) => log::warn!("global shader cache: {e:#}"),
    }
    cooked.write(&path)?;
    log::info!("wrote {} ({} maps, {} shaders)", path.display(), cooked.maps.len(), cooked.code.len());
    Ok(())
}

/// The map's faces and lines' facial animations.
pub fn facefx_path(root: &Path, map: &str) -> PathBuf {
    root.join("facefx").join(format!("{}.json", map.to_ascii_lowercase()))
}

pub fn scene_path(root: &Path, map: &str) -> PathBuf {
    root.join("maps").join(format!("{}.json", map.to_ascii_lowercase()))
}

/// Cook a map (persistent level package name, e.g. "L_Prison_P") into `root`.
pub fn cook_map(assets: &Assets, map: &str, root: &Path, opts: CookOptions, progress: Progress) -> Result<PathBuf> {
    assets.load_globals();
    if let Err(e) = cook_shader_library(assets, root, false) {
        log::warn!("shader library: {e:#}");
    }
    let mut c = Cooker {
        assets,
        root: root.to_path_buf(),
        opts,
        scene: Scene { version: SCENE_VERSION, name: map.to_string(), kill_y: -100.0, ..Default::default() },
        mesh_ids: HashMap::new(),
        mat_ids: HashMap::new(),
        tex_ids: HashMap::new(),
        tex_jobs: Vec::new(),
        props_cache: HashMap::new(),
        failed_meshes: HashSet::new(),
        mesh_section_mats: HashMap::new(),
        keyhole_sockets: HashMap::new(),
        usable_ids: HashMap::new(),
        skel_insts: HashMap::new(),
        facefx: FaceFx::default(),
        facefx_ids: HashMap::new(),
        contacts: None,
        lm_pairs: Vec::new(),
        lm_pair_ids: HashMap::new(),
        sun_guids: Vec::new(),
        npc_type_ids: HashMap::new(),
        ragdoll_ids: HashMap::new(),
        skeleton_ids: HashMap::new(),
        anim_set_ids: HashMap::new(),
        bark_ids: HashMap::new(),
        pending_anims: Vec::new(),
        shadow_texs: Vec::new(),
        shadow_tex_ids: HashMap::new(),
        light_guids: HashMap::new(),
        pending_light_shadows: Vec::new(),
        pending_irrelevant: Vec::new(),
        progress,
        warnings: 0,
        spawner_keys: Vec::new(),
        stealable_keys: Vec::new(),
        tether_keys: Vec::new(),
        flee_keys: Vec::new(),
        joint_keys: Vec::new(),
        audio_cell_keys: HashMap::new(),
        portal_cell_keys: Vec::new(),
        pending_soirees: Vec::new(),
        fog_height: 0.0,
        actor_refs: HashMap::new(),
        actor_base: HashMap::new(),
        particle_system_ids: HashMap::new(),
    };
    // material 0: default
    c.scene.materials.push(MaterialDef {
        name: "default".into(),
        tint: [0.6, 0.6, 0.6, 1.0],
        alpha_cutoff: 0.5,
        ..Default::default()
    });
    let persistent = assets.package(map).with_context(|| format!("map package {map} not found"))?;
    let mut levels = vec![persistent.clone()];
    if c.opts.include_streaming {
        for name in streaming_levels(&persistent) {
            match assets.package(&name) {
                Some(p) => levels.push(p),
                None => log::warn!("streaming level {name} not found"),
            }
        }
    }
    for p in &levels {
        assets.add_search(p.clone());
    }
    c.scene.packages = levels.iter().map(|p| p.name.clone()).collect();
    c.scene.music = levels.iter().find_map(|p| music_events(p));
    c.scene.player_vis = c.player_vis(&levels);
    c.health_effects();
    // Lightmass volume samples (characters' lighting)
    let samples: Vec<VolumeSample> = levels.iter().flat_map(|p| light_volume(p)).collect();
    if !samples.is_empty() {
        let file = format!("maps/{}.lvol", map.to_ascii_lowercase());
        write_light_volume(&root.join(&file), &samples)?;
        log::info!("{map}: {} light volume samples", samples.len());
        c.scene.light_volume = Some(file);
    }
    for p in &levels {
        for i in p.exports_of_class("DominantDirectionalLightComponent").collect::<Vec<_>>() {
            if let Ok(od) = upk::read_object(p, i) {
                if let Some(Value::Guid(g)) = od.props.get("LightGuid") {
                    c.sun_guids.push(*g);
                }
            }
        }
    }
    let n = levels.len();
    let streamed = kismet_streamed_levels(&persistent);
    for (i, p) in levels.iter().enumerate() {
        progress(0.05 + 0.35 * i as f32 / n as f32, &format!("Reading level {}", p.name));
        let before = (c.scene.instances.len() as u32, c.scene.spawners.len() as u32, c.scene.pickups.len() as u32, c.scene.volumes.len() as u32);
        if let Err(e) = c.process_level(p) {
            log::warn!("level {} failed: {e:#}", p.name);
        }
        c.scene.levels.push(LevelRange {
            name: p.name.clone(),
            streamed: streamed.iter().any(|s| s.eq_ignore_ascii_case(&p.name)),
            instances: (before.0, c.scene.instances.len() as u32),
            spawners: (before.1, c.scene.spawners.len() as u32),
            pickups: (before.2, c.scene.pickups.len() as u32),
            ops: (0, 0),
            volumes: (before.3, c.scene.volumes.len() as u32),
        });
    }
    let started = kismet::spawners_started_at_load(&levels);
    for (sp, key) in c.scene.spawners.iter_mut().zip(c.stealable_keys.iter()) {
        sp.stealable = key.as_ref().and_then(|k| c.actor_refs.get(k)).and_then(|a| a.pickup);
    }
    for (sp, keys) in c.scene.spawners.iter_mut().zip(c.tether_keys.iter()) {
        sp.tether = keys.iter().filter_map(|k| c.actor_refs.get(k)).filter_map(|a| a.volume).collect();
    }
    // flee points' chains
    let flee_index: HashMap<kismet::ActorKey, u32> = c.flee_keys.iter().enumerate().map(|(i, (k, _))| (k.clone(), i as u32)).collect();
    for (i, (_, next)) in c.flee_keys.iter().enumerate() {
        if let Some(n) = next.as_ref().and_then(|k| flee_index.get(k)) {
            c.scene.ai_markers.flee[i].next = Some(*n);
        }
    }
    for (sp, key) in c.scene.spawners.iter_mut().zip(c.spawner_keys.iter()) {
        sp.script_start = started.contains(key);
    }
    // the doorways' rooms
    for (p, keys) in c.portal_cell_keys.iter().enumerate() {
        for (k, key) in keys.iter().enumerate() {
            c.scene.audio_portals[p].cells[k] = key.as_ref().and_then(|key| c.audio_cell_keys.get(key)).copied();
        }
    }
    // the props the joints hold
    for (name, keys) in &c.joint_keys {
        let insts: Vec<u32> = keys.iter().filter_map(|k| c.actor_refs.get(k)).flat_map(|a| a.instances.iter().copied()).collect();
        for m in c.scene.movables.iter_mut().filter(|m| insts.contains(&m.instance)) {
            m.joints.push(name.clone());
        }
    }
    log::info!(
        "{map}: {} spawners, {} begin-play, {} started by level scripts",
        c.scene.spawners.len(),
        c.scene.spawners.iter().filter(|s| s.spawn_on_begin_play).count(),
        c.scene.spawners.iter().filter(|s| s.script_start).count()
    );
    // what rides on another actor moves with it
    for (rider, base) in std::mem::take(&mut c.actor_base) {
        let Some(insts) = c.actor_refs.get(&rider).map(|r| r.instances.clone()).filter(|i| !i.is_empty()) else { continue };
        if let Some(b) = c.actor_refs.get_mut(&base) {
            b.riders.extend(insts);
        }
    }
    c.scene.kismet = kismet::cook(&levels, &c.actor_refs);
    // the challenge's scoring rule sets the scripts name (Dunwall City Trials)
    let mut sets: Vec<String> = Vec::new();
    for op in &c.scene.kismet.ops {
        let mut names: Vec<String> = Vec::new();
        if let Some(KVal::Str(s)) = op.props.get("m_pRuleSetTweak") {
            names.push(s.clone());
        }
        if op.class == "DisSeqAct_DLC05_TriggerCustomScoringRule" {
            if let Some(KVal::List(l)) = op.props.get("Targets") {
                names.extend(l.iter().filter_map(|v| if let KVal::Str(s) = v { Some(s.clone()) } else { None }));
            }
        }
        for n in names {
            if !sets.contains(&n) {
                sets.push(n);
            }
        }
    }
    for n in sets {
        match c.ruleset(&n) {
            Some(r) => c.scene.challenge_rules.push(r),
            None => log::warn!("{map}: scoring rule set {n} not found"),
        }
    }
    // the distractions' scenes join the matinees
    let soirees = std::mem::take(&mut c.pending_soirees);
    let mut soiree_ids: HashMap<(String, i32), Option<u32>> = HashMap::new();
    for (d, obj) in soirees {
        let key = (obj.pkg.name.clone(), obj.idx);
        let id = *soiree_ids.entry(key).or_insert_with(|| {
            kismet::cook_soiree(&obj.pkg, obj.idx).map(|m| {
                c.scene.kismet.matinees.push(m);
                c.scene.kismet.matinees.len() as u32 - 1
            })
        });
        c.scene.distractors[d].matinee = id;
    }
    if !c.scene.distractors.is_empty() {
        log::info!("{map}: {} distractors, {} with scenes", c.scene.distractors.len(), c.scene.distractors.iter().filter(|d| d.matinee.is_some()).count());
    }
    c.scene.navmesh = navmesh::merge(&levels);
    log::info!("{map}: navmesh of {} polygons", c.scene.navmesh.polys.len());
    c.given_pickups();
    c.swap_materials();
    c.lens_effects();
    c.possess_overrides();
    c.vis_settings_ops();
    c.target_notification_ops();
    c.event_defaults();
    c.matinee_props();
    c.scripted_blasts();
    c.npc_set_materials();
    c.movie_lengths();
    c.factory_pickups();
    c.factory_tanks();
    for (r, ops) in c.scene.levels.iter_mut().zip(c.scene.kismet.level_ops.iter()) {
        r.ops = *ops;
    }
    c.scene.loadouts = levels.iter().flat_map(|p| gamedata::loadouts_in(p)).collect();
    log::info!("{map}: {} script ops, {} variables, {} actors", c.scene.kismet.ops.len(), c.scene.kismet.vars.len(), c.scene.kismet.actors.len());
    c.resolve_light_shadows();
    c.assign_routes();
    c.cook_props();
    // cook_props creates player_arms; scene animation sets must be bound only
    // after that rig exists, or every player cinematic clip is silently omitted.
    c.matinee_pawn_anims();
    c.cook_anims();
    c.pack_lightmaps();
    c.cook_textures()?;
    log::info!(
        "{map}: {}/{} materials linked to their compiled shaders",
        c.scene.materials.iter().filter(|m| m.ue3.is_some()).count(),
        c.scene.materials.len()
    );
    // the lines' facial animations (the dialogue data's FaceFX animation sets)
    for pkg in &levels {
        for i in 1..=pkg.exports.len() as i32 {
            if pkg.class_name(i) != "FaceFXAnimSet" {
                continue;
            }
            let Ok(od) = upk::read_object(pkg, i) else { continue };
            match facefx::read_anim_set(&od.reader.data[od.reader.pos..]) {
                Ok(list) => c.facefx.anims.extend(list),
                Err(e) => log::warn!("{}: {e:#}", pkg.obj_path(i)),
            }
        }
    }
    if !c.facefx.anims.is_empty() || !c.facefx.actors.is_empty() {
        let fx = facefx_path(root, map);
        std::fs::create_dir_all(fx.parent().unwrap())?;
        write_atomic(&fx, &serde_json::to_vec(&c.facefx)?)?;
        log::info!("{map}: {} facial animations, {} faces", c.facefx.anims.len(), c.facefx.actors.len());
    }
    progress(0.98, "Writing scene");
    let path = scene_path(root, map);
    let json = serde_json::to_vec(&c.scene)?;
    write_atomic(&path, &json)?;
    log::info!(
        "cooked {map}: {} meshes, {} materials, {} textures, {} instances, {} lights, {} warnings",
        c.scene.meshes.len(),
        c.scene.materials.len(),
        c.scene.textures.len(),
        c.scene.instances.len(),
        c.scene.lights.len(),
        c.warnings
    );
    progress(1.0, "Done");
    Ok(path)
}

/// All playable persistent maps (`L_*_P` packages) in the cooked directory.
pub fn list_maps(cooked: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(cooked)
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .filter_map(|e| e.file_name().into_string().ok())
                .filter(|n| n.starts_with("L_") && n.to_ascii_lowercase().ends_with("_p.upk") || n == "Dishonored_MainMenu.upk")
                .map(|n| n[..n.len() - 4].to_string())
                .collect()
        })
        .unwrap_or_default();
    v.sort();
    v
}

/// The sublevels the level scripts stream in and out (`LevelStreamingKismet`).
pub fn kismet_streamed_levels(pkg: &upk::Package) -> Vec<String> {
    let mut out = Vec::new();
    for i in 1..=pkg.exports.len() as i32 {
        if pkg.class_name(i) == "LevelStreamingKismet" {
            if let Ok(od) = upk::read_object(pkg, i) {
                if let Some(n) = od.props.name("PackageName") {
                    out.push(n.to_string());
                }
            }
        }
    }
    out
}

/// Names of streaming sublevel packages referenced by a persistent level.
pub fn streaming_levels(pkg: &upk::Package) -> Vec<String> {
    let mut out = Vec::new();
    for i in 1..=pkg.exports.len() as i32 {
        let cls = pkg.class_name(i);
        if cls.starts_with("LevelStreaming") {
            if let Ok(od) = upk::read_object(pkg, i) {
                if let Some(n) = od.props.name("PackageName") {
                    if !out.iter().any(|x: &String| x.eq_ignore_ascii_case(n)) {
                        out.push(n.to_string());
                    }
                }
            }
        }
    }
    out
}

/// The precomputed light volume of a level (`ULevel::PrecomputedLightVolume`, near the end of
/// the level's data): bInitialized, bounds (FBox), sample spacing, then 33-byte samples
/// (position, radius, indirect and environment directions as theta/phi bytes, indirect /
/// environment / ambient radiance as RGBE colours, shadowed-from-dominant-lights flag).
fn light_volume(pkg: &upk::Package) -> Vec<VolumeSample> {
    let Some(level) = pkg.find_export("TheWorld.PersistentLevel") else { return Vec::new() };
    let Ok(d) = pkg.export_data(level) else { return Vec::new() };
    let n = d.len();
    let f = |o: usize| f32::from_le_bytes([d[o], d[o + 1], d[o + 2], d[o + 3]]);
    let u = |o: usize| u32::from_le_bytes([d[o], d[o + 1], d[o + 2], d[o + 3]]);
    const STRIDE: usize = 33;
    // the volume sits near the end: search backwards
    let mut p = n.saturating_sub(37);
    while p > 4 {
        p -= 1;
        if u(p - 4) != 1 || d[p + 24] != 1 {
            continue;
        }
        let (mn, mx) = ([f(p), f(p + 4), f(p + 8)], [f(p + 12), f(p + 16), f(p + 20)]);
        if !(0..3).all(|i| mn[i].is_finite() && mx[i].is_finite() && mn[i] < mx[i] && mx[i] - mn[i] < 1e6) {
            continue;
        }
        let cnt = u(p + 29) as usize;
        let q = p + 33;
        if cnt == 0 || q + cnt * STRIDE > n || n - (q + cnt * STRIDE) > 4096 {
            continue;
        }
        let inside = |k: usize| {
            let o = q + k * STRIDE;
            (0..3).all(|i| (mn[i] - 100.0..=mx[i] + 100.0).contains(&f(o + i * 4)))
        };
        if !(inside(0) && inside(cnt - 1) && inside(cnt / 2)) {
            continue;
        }
        let rgbe = |o: usize| -> [f32; 3] {
            // FColor (B, G, R, A) holding RGBE
            let (b, g, r, e) = (d[o] as f32, d[o + 1] as f32, d[o + 2] as f32, d[o + 3]);
            if e == 0 {
                return [0.0; 3];
            }
            let s = (1.0 / 255.0) * 2f32.powi(e as i32 - 128);
            [r * s, g * s, b * s]
        };
        let dir = |th: u8, ph: u8| -> [f32; 3] {
            let t = th as f32 / 255.0 * std::f32::consts::PI;
            let ph = ph as f32 / 255.0 * std::f32::consts::TAU - std::f32::consts::PI;
            // UE (x, y, z) -> Bevy (x, z, y)
            [t.sin() * ph.cos(), t.cos(), t.sin() * ph.sin()]
        };
        return (0..cnt)
            .map(|k| {
                let o = q + k * STRIDE;
                VolumeSample {
                    position: ue_point([f(o), f(o + 4), f(o + 8)]),
                    radius: f(o + 12) * UNIT,
                    indirect_dir: dir(d[o + 16], d[o + 17]),
                    environment_dir: dir(d[o + 18], d[o + 19]),
                    indirect: rgbe(o + 20),
                    environment: rgbe(o + 24),
                    ambient: rgbe(o + 28),
                    shadowed: d[o + 32] as f32,
                    ..Default::default()
                }
            })
            .collect();
    }
    Vec::new()
}

/// The map's music director settings: `DishonoredMapInfo.m_pMapMusicEvents`.
fn music_events(pkg: &upk::Package) -> Option<MusicEvents> {
    let info = pkg.exports_of_class("DishonoredMapInfo").next()?;
    let ip = upk::read_object(pkg, info).ok()?.props;
    let tw = ip.object("m_pMapMusicEvents").filter(|o| *o > 0)?;
    let p = upk::read_object(pkg, tw).ok()?.props;
    let event = |name: &str, i: i32| match p.get_idx(name, i) {
        Some(Value::Object(o)) if *o != 0 => {
            let path = pkg.obj_path(*o);
            Some(path.rsplit('.').next().unwrap_or(&path).to_string())
        }
        _ => None,
    };
    let float = |name: &str, i: i32| match p.get_idx(name, i) {
        Some(Value::Float(f)) => Some(*f),
        _ => None,
    };
    let list = |name: &str| (0..8).map_while(|i| event(name, i)).collect::<Vec<_>>();
    Some(MusicEvents {
        exploration: event("m_pStartExploration", 0).unwrap_or_default(),
        suspense: event("m_pStartSuspense", 0).unwrap_or_default(),
        rat_attack: event("m_pStartRatAttack", 0).unwrap_or_default(),
        combat: list("m_pStartCombat"),
        chase: list("m_pStartChase"),
        combat_intensity: (0..4).map(|i| float("m_fCombatIntensity", i).unwrap_or(f32::MAX)).collect(),
        contribution: (0..8).map(|i| float("m_fCombatIntensityContribution", i).unwrap_or(1.0)).collect(),
        suspense_range: float("m_fEnemyRangeForSuspense", 0).unwrap_or(800.0) * 0.01,
        chase_time: float("m_fChaseTime", 0).unwrap_or(15.0),
        chase_range: (0..2).map(|i| float("m_fEnemyRangeForChase", i).unwrap_or(2000.0 * (i + 1) as f32) * 0.01).collect(),
    })
}

/// `EDisAttentionLevel` (`DAL_Unaware` 0 .. `DAL_Busted` 4).
fn attention_level(e: &str) -> u8 {
    match e {
        "DAL_HeadTrack" => 1,
        "DAL_TurnToFace" => 2,
        "DAL_Investigate" => 3,
        "DAL_Busted" => 4,
        _ => 0,
    }
}

/// `EDisAttentionLevelForIncrease` (`DALFI_None` 0 .. `DALFI_Busted` 4, `DALFI_MaxAttention` 5).
fn attention_increase_level(e: &str) -> u8 {
    match e {
        "DALFI_HeadTrack" => 1,
        "DALFI_TurnToFace" => 2,
        "DALFI_Investigate" => 3,
        "DALFI_Busted" => 4,
        "DALFI_MaxAttention" => 5,
        _ => 0,
    }
}

fn is_actor_like(cls: &str) -> bool {
    !matches!(cls, "Model" | "Polys" | "Sequence" | "Level" | "ShadowMap2D" | "LightMapTexture2D")
}

/// Property lookup across an object's archetype chain. Each entry remembers the package
/// it came from: object references and array offsets are relative to that package.
struct Chain(Vec<(Arc<upk::Package>, Arc<Props>)>);

/// An object's properties with the plain values its archetypes give it besides (down to its
/// class's defaults): its own first. Arrays and structs stay its own, since their bytes are
/// their package's.
fn merged_props(assets: &Assets, obj: &Obj) -> Option<upk::props::Props> {
    let mut out = obj.props().ok()?;
    let mut cur = Some(obj.clone());
    for _ in 0..12 {
        let Some(o) = cur.take() else { break };
        if o.idx <= 0 || o.idx as usize > o.pkg.exports.len() {
            break;
        }
        let arch = o.pkg.exports[o.idx as usize - 1].archetype;
        // (none: its class's defaults, the game's script classes')
        let a = if arch != 0 {
            assets.resolve(&o.pkg, arch)
        } else if !o.name().starts_with("Default__") {
            let class = o.class();
            ["DishonoredGame", "Engine", "GameFramework", "Core"].iter().find_map(|p| assets.find(&format!("{p}.Default__{class}")))
        } else {
            None
        };
        let Some(a) = a else { break };
        if let Ok(p) = a.props() {
            for q in p.0 {
                if matches!(q.value, Value::Array { .. } | Value::Struct(..) | Value::Raw { .. }) {
                    continue;
                }
                if !out.0.iter().any(|x| x.name.eq_ignore_ascii_case(&q.name) && x.index == q.index) {
                    out.0.push(q);
                }
            }
        }
        cur = Some(a);
    }
    Some(out)
}

impl Chain {
    fn get(&self, name: &str) -> Option<&Value> {
        self.0.iter().find_map(|(_, p)| p.get(name))
    }
    fn get_pkg(&self, name: &str) -> Option<(&Arc<upk::Package>, &Value)> {
        self.0.iter().find_map(|(k, p)| p.get(name).map(|v| (k, v)))
    }
    fn get_idx_pkg(&self, name: &str, i: i32) -> Option<(&Arc<upk::Package>, &Value)> {
        self.0.iter().find_map(|(k, p)| p.get_idx(name, i).map(|v| (k, v)))
    }
    /// Resolve an object-valued property in the package that defined it.
    fn obj(&self, assets: &Assets, name: &str) -> Option<Obj> {
        match self.get_pkg(name)? {
            (pkg, Value::Object(o)) if *o != 0 => assets.resolve(pkg, *o),
            _ => None,
        }
    }
    fn obj_path(&self, name: &str) -> Option<String> {
        match self.get_pkg(name)? {
            (pkg, Value::Object(o)) if *o != 0 => Some(pkg.obj_path(*o)),
            _ => None,
        }
    }
    fn bool(&self, name: &str) -> Option<bool> {
        match self.get(name)? {
            Value::Bool(b) => Some(*b),
            _ => None,
        }
    }
    fn float(&self, name: &str) -> Option<f32> {
        match self.get(name)? {
            Value::Float(f) => Some(*f),
            Value::Int(i) => Some(*i as f32),
            _ => None,
        }
    }
    fn vector(&self, name: &str) -> Option<[f32; 3]> {
        match self.get(name)? {
            Value::Vector(v) => Some(*v),
            _ => None,
        }
    }
    fn rotator(&self, name: &str) -> Option<[i32; 3]> {
        match self.get(name)? {
            Value::Rotator(v) => Some(*v),
            _ => None,
        }
    }
    fn color(&self, name: &str) -> Option<[u8; 4]> {
        match self.get(name)? {
            Value::Color(v) => Some(*v),
            _ => None,
        }
    }
    fn name(&self, name: &str) -> Option<&str> {
        match self.get(name)? {
            Value::Name(s) | Value::Enum(s) | Value::Str(s) => Some(s.as_str()),
            _ => None,
        }
    }
}

impl<'a> Cooker<'a> {
    fn chain(&mut self, obj: &Obj) -> Chain {
        let mut v = Vec::new();
        let mut cur = Some(obj.clone());
        let mut depth = 0;
        while let Some(o) = cur.take() {
            depth += 1;
            if depth > 12 || o.idx <= 0 || o.idx as usize > o.pkg.exports.len() {
                break;
            }
            let key = (Arc::as_ptr(&o.pkg) as usize, o.idx);
            let props = match self.props_cache.get(&key) {
                Some(p) => p.clone(),
                None => {
                    let p = Arc::new(o.props().unwrap_or_default());
                    self.props_cache.insert(key, p.clone());
                    p
                }
            };
            v.push((o.pkg.clone(), props));
            let arch = o.pkg.exports[o.idx as usize - 1].archetype;
            if arch != 0 {
                cur = self.assets.resolve(&o.pkg, arch);
            }
        }
        Chain(v)
    }

    /// A challenge's scoring rule set (`DisDLC05Tweaks_ChallengeScoringRuleset`) by its path.
    /// (Its rules' and modifiers' settings with their classes' defaults: Power Combo's gain,
    /// Payback's second.)
    fn ruleset(&mut self, name: &str) -> Option<RulesetDef> {
        let o = self.assets.find(name)?;
        let p = o.props().ok()?;
        let pkg = o.pkg.clone();
        let rules = |field: &str, assets: &Assets| -> Vec<ScoreRuleDef> {
            let Some((cnt, off, sz)) = p.array(field) else { return Vec::new() };
            let mut out = Vec::new();
            for s in upk::props::parse_struct_array(&pkg, off, sz, cnt).unwrap_or_default() {
                let Some(ro) = s.object("m_Rule").filter(|r| *r != 0).and_then(|r| assets.resolve(&pkg, r)) else { continue };
                let Some(rp) = merged_props(assets, &ro) else { continue };
                let mut gains = Vec::new();
                if let Some((gc, goff, gsz)) = rp.array("m_NpcBaseGains") {
                    for g in upk::props::parse_struct_array(&ro.pkg, goff, gsz, gc).unwrap_or_default() {
                        let group = g.object("m_pStoryGroup").filter(|x| *x != 0).map(|x| ro.pkg.obj_path(x)).unwrap_or_default();
                        gains.push((group, g.int("m_iBaseGain").unwrap_or(0), g.name("m_EntryName").unwrap_or_default().to_string()));
                    }
                }
                // (its numbers and its enums)
                let settings = |props: &upk::props::Props, skip: &[&str]| {
                    let mut params = std::collections::BTreeMap::new();
                    let mut kinds = std::collections::BTreeMap::new();
                    for q in &props.0 {
                        if skip.contains(&q.name.as_str()) {
                            continue;
                        }
                        // (a static array's elements other than the first: the lists')
                        if q.index > 0 {
                            continue;
                        }
                        match &q.value {
                            Value::Float(f) => {
                                params.insert(q.name.clone(), *f);
                            }
                            Value::Int(i) => {
                                params.insert(q.name.clone(), *i as f32);
                            }
                            Value::Bool(b) => {
                                params.insert(q.name.clone(), *b as u8 as f32);
                            }
                            Value::Enum(e) => {
                                kinds.insert(q.name.clone(), e.clone());
                            }
                            _ => {}
                        }
                    }
                    (params, kinds)
                };
                let (params, kinds) = settings(&rp, &["m_iBaseGain"]);
                let mut lists: std::collections::BTreeMap<String, Vec<Option<f32>>> = std::collections::BTreeMap::new();
                let indexed: std::collections::BTreeSet<&str> = rp.0.iter().filter(|q| q.index > 0).map(|q| q.name.as_str()).collect();
                for q in rp.0.iter().filter(|q| indexed.contains(q.name.as_str())) {
                    let v = match &q.value {
                        Value::Float(f) => *f,
                        Value::Int(i) => *i as f32,
                        _ => continue,
                    };
                    let l = lists.entry(q.name.clone()).or_default();
                    if l.len() <= q.index as usize {
                        l.resize(q.index as usize + 1, None);
                    }
                    l[q.index as usize] = Some(v);
                }
                let mut modifiers = Vec::new();
                if let Some((mc, moff, msz)) = rp.array("m_Modifiers") {
                    for m in upk::props::parse_struct_array(&ro.pkg, moff, msz, mc).unwrap_or_default() {
                        let Some(mo) = m.object("m_pModifier").filter(|r| *r != 0).and_then(|r| assets.resolve(&ro.pkg, r)) else { continue };
                        let Some(mp) = merged_props(assets, &mo) else { continue };
                        let (params, kinds) = settings(&mp, &["m_iExtraGain", "m_iNoveltyExtraGain"]);
                        modifiers.push(ScoreModifierDef {
                            name: m.name("m_ModifierName").unwrap_or_default().to_string(),
                            class: mo.class(),
                            entry: mp.name("m_EntryName").unwrap_or_default().to_string(),
                            extra: mp.int("m_iExtraGain").unwrap_or(0),
                            novelty: mp.int("m_iNoveltyExtraGain").unwrap_or(0),
                            params,
                            kinds,
                        });
                    }
                }
                out.push(ScoreRuleDef {
                    name: s.name("m_RuleName").unwrap_or_default().to_string(),
                    entry: rp.name("m_EntryName").unwrap_or_default().to_string(),
                    class: ro.class(),
                    base_gain: rp.int("m_iBaseGain").unwrap_or(0),
                    gains,
                    params,
                    modifiers,
                    kinds,
                    lists,
                });
            }
            out
        };
        let mut stats = Vec::new();
        if let Some((cnt, off, sz)) = p.array("m_ResultsMenuStats") {
            for s in upk::props::parse_struct_array(&pkg, off, sz, cnt).unwrap_or_default() {
                stats.push((s.name("m_StatLookup").unwrap_or_default().to_string(), s.name("m_StatName").unwrap_or_default().to_string()));
            }
        }
        Some(RulesetDef { name: name.to_string(), rules: rules("m_Rules", self.assets), multipliers: rules("m_Multipliers", self.assets), stats })
    }

    /// A `LensFlare`'s source element at a place, and what its material makes of it.
    fn lens_flare(&mut self, lf: &Obj, at: [f32; 3]) -> Option<LensFlareDef> {
        let props = lf.props().ok()?;
        let el = props.struct_props("SourceElement")?;
        let dist = |n: &str, vector: bool| el.struct_props(n).and_then(|p| raw_distribution(lf, &p, None, vector));
        // (a constant's value: its first entry)
        let first = |d: Option<Dist>, n: usize, default: f32| -> Vec<f32> {
            let t = d.map(|d| d.table).unwrap_or_default();
            (0..n).map(|k| t.get(k).copied().unwrap_or(default)).collect()
        };
        let size = match el.get("Size") {
            Some(Value::Vector(v)) => v[0],
            _ => 1.0,
        };
        let scaling = first(dist("Scaling", false), 1, 1.0)[0];
        let alpha = first(dist("Alpha", false), 1, 1.0)[0];
        let c = first(dist("Color", true), 3, 1.0);
        let mut f = LensFlareDef {
            position: ue_point(at),
            size: size * scaling * UNIT,
            alpha,
            color: [c[0], c[1], c[2]],
            dist_scale: dist("DistMap_Scale", true),
            dist_alpha: dist("DistMap_Alpha", false),
            normalize: matches!(el.get("bNormalizeRadialDistance"), Some(Value::Bool(true))),
            tint: [1.0; 3],
            power: 5.0,
            radial: 1.0,
            opacity: [0.0, 1.0],
            glow: None,
        };
        let mats = upk::props::object_array(&lf.pkg, &el, "LFMaterials");
        if let Some(m) = mats.first().and_then(|m| self.assets.resolve(&lf.pkg, *m)) {
            let rm = crate::materials::resolve_material(self.assets, &m);
            let s = |n: &str, d: f32| rm.scalars.iter().find(|(k, _)| k == n).map(|(_, v)| *v).unwrap_or(d);
            if let Some((_, c)) = rm.vectors.iter().find(|(k, _)| k == "L_LensFlare_Color") {
                f.tint = [c[0] * c[3], c[1] * c[3], c[2] * c[3]];
            }
            f.power = s("L_LensFlare_Power", 5.0);
            f.radial = s("L_Radial_Distance_Factor", 1.0);
            f.opacity = [s("L_Minimum_Opacity", 0.0), s("L_Maximum_Opacity", 1.0)];
            if rm.switches.iter().any(|(k, on)| k == "Lg_Enable_Glowing_LensFlare" && *on) {
                f.glow = Some([s("L_LensFlare_Glowing_Speed", 0.5), s("Lg_LensFlare_Glowing_Range", 0.1), s("Lg_LensFlare_Glowing_Range_Visibility", 0.5)]);
            }
        }
        Some(f)
    }

    fn actor_matrix(&mut self, actor: &Obj) -> Mat4 {
        let ch = self.chain(actor);
        let loc = ch.vector("Location").unwrap_or([0.0; 3]);
        let rot = ch.rotator("Rotation").unwrap_or([0; 3]);
        let ds = ch.float("DrawScale").unwrap_or(1.0);
        let ds3 = ch.vector("DrawScale3D").unwrap_or([1.0; 3]);
        let pre = ch.vector("PrePivot").unwrap_or([0.0; 3]);
        srt(Vec3::from(ds3) * ds, rot, Vec3::from(loc)) * Mat4::from_translation(-Vec3::from(pre))
    }

    fn component_local(&mut self, comp: &Obj) -> Mat4 {
        let ch = self.chain(comp);
        let t = ch.vector("Translation").unwrap_or([0.0; 3]);
        let r = ch.rotator("Rotation").unwrap_or([0; 3]);
        let s = ch.float("Scale").unwrap_or(1.0);
        let s3 = ch.vector("Scale3D").unwrap_or([1.0; 3]);
        srt(Vec3::from(s3) * s, r, Vec3::from(t))
    }

    /// The script-facing record of a level actor.
    fn actor_ref(&mut self, actor: &Obj) -> &mut KActor {
        let key = (actor.pkg.name.clone(), actor.idx);
        if !self.actor_refs.contains_key(&key) {
            let ch = self.chain(actor);
            let loc = ch.vector("Location").unwrap_or([0.0; 3]);
            let rot = ch.rotator("Rotation").unwrap_or([0; 3]);
            // (a name another sublevel's door has too: `name@package`, `Scene::door`)
            let door_key = if self.scene.doors.contains_key(actor.name()) { format!("{}@{}", actor.name(), actor.pkg.name) } else { actor.name().to_string() };
            if actor.class() == "DisDoor" && !self.scene.doors.contains_key(&door_key) {
                let tweak = ch.obj(self.assets, "m_pDoorTweaks");
                let mut d = tweak.as_ref().and_then(|t| door_sounds(self.assets, t)).unwrap_or_default();
                // breakable: break steps with any authored step list
                d.breakable = tweak
                    .as_ref()
                    .and_then(|t| self.chain(t).obj(self.assets, "m_pBreakSteps"))
                    .map(|b| self.chain(&b))
                    .is_some_and(|bc| bc.0.iter().any(|(_, p)| p.0.iter().any(|x| !matches!(x.name.as_str(), "m_BaseClassVersions" | "m_FallbackChainCooked" | "m_TweakNameCooked" | "m_VersionNum") && matches!(&x.value, Value::Array { count, .. } if *count > 0))));
                if d.breakable {
                    if let Some(t) = tweak.as_ref() {
                        let tc = self.chain(t);
                        d.health = tc.float("m_Health").unwrap_or(15.0);
                        d.threshold = tc.float("m_DamageThreshold").unwrap_or(0.0);
                        if let Some(b) = tc.obj(self.assets, "m_pBreakSteps") {
                            let bc = self.chain(&b);
                            d.breaks = self.last_break_step(&bc, "m_DoorBreakSteps");
                        }
                    }
                }
                if let Some(t) = tweak.as_ref() {
                    let tc = self.chain(t);
                    d.occlusion = [tc.float("m_fPlayerSoundOcclusion").unwrap_or(0.0), tc.float("m_fAISoundOcclusion").unwrap_or(0.0)];
                }
                d.locked_start = ch.bool("m_bLocked").unwrap_or(false);
                d.keys = str_array(&ch, "m_MatchingKeys");
                d.open_start = match ch.name("m_InitialDoorState") {
                    Some(s) if s.ends_with("Opened_CW") => Some(true),
                    Some(s) if s.ends_with("Opened_CCW") => Some(false),
                    _ => None,
                };
                self.scene.doors.insert(door_key, d);
            }
            let r = KActor {
                name: actor.name().to_string(),
                class: actor.class(),
                position: ue_point(loc),
                yaw: ue_yaw_to_bevy(rot[1]),
                rotation: rot,
                ue_location: loc,
                fov: if actor.class().contains("Camera") { ch.float("FOVAngle").unwrap_or(90.0) } else { 0.0 },
                ..Default::default()
            };
            if let Some((pkg, upk::props::Value::Object(o))) = ch.get_pkg("Base") {
                if *o > 0 && Arc::ptr_eq(pkg, &actor.pkg) {
                    self.actor_base.insert(key.clone(), (pkg.name.clone(), *o));
                }
            }
            self.actor_refs.insert(key.clone(), r);
        }
        self.actor_refs.get_mut(&key).unwrap()
    }

    fn process_level(&mut self, pkg: &Arc<upk::Package>) -> Result<()> {
        let Some(level) = pkg.find_export("TheWorld.PersistentLevel") else {
            bail!("no PersistentLevel in {}", pkg.name);
        };
        let n = pkg.exports.len() as i32;
        let actors: Vec<i32> = (1..=n)
            .filter(|&i| pkg.exports[i as usize - 1].outer == level && is_actor_like(&pkg.class_name(i)))
            .collect();
        let actor_set: HashSet<i32> = actors.iter().copied().collect();
        let mut collected: HashSet<i32> = HashSet::new();

        // Pass 1: actors with special handling
        for &a in &actors {
            let cls = pkg.class_name(a);
            let obj = Obj { pkg: pkg.clone(), idx: a };
            let r = match cls.as_str() {
                "StaticMeshCollectionActor" => self.mesh_collection(&obj, &mut collected),
                "StaticLightCollectionActor" => self.light_collection(&obj, &mut collected),
                "PlayerStart" => self.player_start(&obj),
                "DishonoredSpawner" => self.spawner(&obj),
                "DisFog" => self.fog(&obj),
                "DisWallOfLight" | "DisDefenceTower" | "DisWatchTower" | "DisAlarmBell" | "DisWhaleOilReceptacle" | "DisWhaleOilBattery" => self.security(&obj),
                "DisRatSpawner" => self.rat_spawner(&obj),
                "DisRiverKrust" => self.krust(&obj),
                "DisFish" => self.fish(&obj),
                "DisWaterSource" => self.water_source(&obj),
                "DishonoredUsableObject" | "DisAudioLogPlayer" => self.usable_object(&obj),
                "DisProtectionTuneSource" => self.tune_source(&obj),
                "DisTripwire" | "DisProjectileLauncher" => self.trap(&obj),
                "DisNPCDistractor" => self.distractor(&obj),
                "DisForbiddenZone" | "DisTetherVolume" | "DisPossessionVolume" => self.volume(&obj),
                "DisGuardWatchPoint" => {
                    let ch = self.chain(&obj);
                    self.scene.ai_markers.watch.push(WatchPoint {
                        position: ue_point(ch.vector("Location").unwrap_or([0.0; 3])),
                        time: [ch.float("m_fMinWatchTime").unwrap_or(3.0), ch.float("m_fMaxWatchTime").unwrap_or(5.0)],
                    });
                    Ok(())
                }
                "DisFleePointActor" => {
                    let ch = self.chain(&obj);
                    let comp = ch.obj(self.assets, "m_pFleeComponent");
                    let cch = comp.as_ref().map(|c| self.chain(c));
                    let squads: Vec<String> = cch
                        .as_ref()
                        .and_then(|c| c.get_pkg("m_Squads"))
                        .and_then(|(pkg, v)| if let Value::Array { count, offset, size } = v { parse_struct_array(pkg, *offset, *size, *count).ok() } else { None })
                        .unwrap_or_default()
                        .iter()
                        .filter_map(|s| s.name("m_SquadName").filter(|n| *n != "None").map(|n| n.to_string()))
                        .collect();
                    let next = cch.as_ref().and_then(|c| c.get_pkg("m_pNextFleePoint")).and_then(|(pkg, v)| if let Value::Object(o) = v { (*o > 0).then(|| (pkg.name.clone(), *o)) } else { None });
                    let rot = ch.rotator("Rotation").unwrap_or([0; 3]);
                    self.scene.ai_markers.flee.push(FleePoint { name: obj.name().to_string(), position: ue_point(ch.vector("Location").unwrap_or([0.0; 3])), yaw: ue_yaw_to_bevy(rot[1]), squads, next: None });
                    self.flee_keys.push(((obj.pkg.name.clone(), obj.idx), next));
                    Ok(())
                }
                "DisHideoutAccessPoint" => {
                    let ch = self.chain(&obj);
                    let p = ue_point(ch.vector("Location").unwrap_or([0.0; 3]));
                    let rot = ch.rotator("Rotation").unwrap_or([0; 3]);
                    self.scene.ai_markers.hideout_access.push([p[0], p[1], p[2], ue_yaw_to_bevy(rot[1])]);
                    Ok(())
                }
                "DisAmbushPoint" => {
                    let ch = self.chain(&obj);
                    let rot = ch.rotator("Rotation").unwrap_or([0; 3]);
                    let f = |k: &str, d: f32| ch.float(k).unwrap_or(d);
                    // (the area's direction: degrees about the point, UE's yaw)
                    let dir = ue_yaw_to_bevy(rot[1]) - f("m_fAmbushAreaDirection", 0.0).to_radians();
                    self.scene.ai_markers.ambush.push(AmbushPoint {
                        name: obj.name().to_string(),
                        position: ue_point(ch.vector("Location").unwrap_or([0.0; 3])),
                        yaw: ue_yaw_to_bevy(rot[1]),
                        area: [dir, f("m_fAmbushAreaLength", 1000.0) * UNIT, f("m_fAmbushAreaWidth", 200.0) * UNIT, f("m_fAmbushAreaTop", 300.0) * UNIT, f("m_fAmbushAreaBottom", -300.0) * UNIT],
                    });
                    Ok(())
                }
                "RB_RadialImpulseActor" => {
                    // a physics burst the scripts set off (`SeqAct_Toggle`: `FireImpulse`)
                    let ch = self.chain(&obj);
                    if let Some(c) = ch.obj(self.assets, "ImpulseComponent") {
                        let cc = self.chain(&c);
                        let imp = [
                            cc.float("ImpulseRadius").unwrap_or(200.0) * UNIT,
                            cc.float("ImpulseStrength").unwrap_or(900.0),
                            cc.bool("bVelChange").unwrap_or(false) as u8 as f32,
                            (cc.name("ImpulseFalloff").unwrap_or("RIF_Linear") == "RIF_Linear") as u8 as f32,
                        ];
                        self.actor_ref(&obj).impulse = Some(imp);
                    }
                    Ok(())
                }
                "LensFlareSource" => {
                    // a lens flare (a candle's glow): its flare's source element, its material
                    let ch = self.chain(&obj);
                    let at = ch.vector("Location").unwrap_or([0.0; 3]);
                    if let Some(lf) = ch.obj(self.assets, "LensFlareComp").and_then(|c| self.chain(&c).obj(self.assets, "Template")) {
                        if let Some(f) = self.lens_flare(&lf, at) {
                            self.scene.lens_flares.push(f);
                        }
                    }
                    Ok(())
                }
                "SceneCaptureReflectActor" => {
                    // a planar reflection: its target, its plane (the actor's place, facing its
                    // rotation: up by default), what it shows
                    let ch = self.chain(&obj);
                    if let Some(c) = ch.obj(self.assets, "SceneCapture") {
                        let cc = self.chain(&c);
                        if let Some(t) = cc.obj(self.assets, "TextureTarget") {
                            let texture = self.texture(TexJob::Plain(t));
                            let rot = ch.rotator("Rotation").unwrap_or([16384, 0, 0]);
                            let (pitch, yaw) = (rot[0] as f32 * std::f32::consts::TAU / 65536.0, rot[1] as f32 * std::f32::consts::TAU / 65536.0);
                            let dir = [pitch.cos() * yaw.cos(), pitch.cos() * yaw.sin(), pitch.sin()];
                            let p = ue_point(ch.vector("Location").unwrap_or([0.0; 3]));
                            let n = ue_point(dir);
                            self.scene.reflections.push(Reflection { texture, point: p, normal: n, channels: reflect_bits(cc.get("ReflectionChannels"), 0b1010_1111) });
                        }
                    }
                    Ok(())
                }
                "DishonoredAudioVolume" => {
                    let hulls = self.brush_hulls(&obj);
                    if !hulls.is_empty() {
                        let ch = self.chain(&obj);
                        let last = |p: Option<String>| p.map(|p| p.rsplit('.').next().unwrap_or(&p).to_string()).unwrap_or_default();
                        self.audio_cell_keys.insert((obj.pkg.name.clone(), obj.idx), self.scene.audio_cells.len() as u32);
                        self.scene.audio_cells.push(AudioCell {
                            name: obj.name().to_string(),
                            hulls,
                            environment: ch.name("m_Environment").filter(|n| *n != "None").unwrap_or_default().to_string(),
                            state_event: last(ch.obj_path("m_pSoundEvent")),
                            interior: ch.name("m_VolumeKind") == Some("VK_INTERIOR"),
                        });
                    }
                    Ok(())
                }
                "DishonoredAudioPortal" => {
                    let ch = self.chain(&obj);
                    let mut corners = [[0.0; 3]; 4];
                    for (k, c) in corners.iter_mut().enumerate() {
                        if let Some((_, Value::Vector(v))) = ch.get_idx_pkg("m_Corners", k as i32) {
                            *c = ue_point(*v);
                        }
                    }
                    let key = |k: &str| ch.get_pkg(k).and_then(|(pkg, v)| if let Value::Object(o) = v { (*o > 0).then(|| (pkg.name.clone(), *o)) } else { None });
                    self.portal_cell_keys.push([key("m_pCellA"), key("m_pCellB")]);
                    self.scene.audio_portals.push(AudioPortal {
                        name: obj.name().to_string(),
                        corners,
                        cells: [None, None],
                        occlusion: [ch.float("m_fOcclusion_HeardByPlayer").unwrap_or(0.0), ch.float("m_fOcclusion_HeardByAI").unwrap_or(0.0)],
                    });
                    Ok(())
                }
                "RB_BSJointActor" | "RB_HingeActor" | "RB_PrismaticActor" | "RB_ConstraintActor" | "RB_PulleyJointActor" | "RB_ConstraintActorSpawnable" => {
                    // what a joint binds (a PA speaker to its mount)
                    let ch = self.chain(&obj);
                    let keys = ["ConstraintActor1", "ConstraintActor2"]
                        .iter()
                        .filter_map(|k| ch.get_pkg(k).and_then(|(pkg, v)| if let Value::Object(o) = v { (*o > 0).then(|| (pkg.name.clone(), *o)) } else { None }))
                        .collect();
                    self.joint_keys.push((obj.name().to_string(), keys));
                    Ok(())
                }
                "DisRatRepulsor" => {
                    let ch = self.chain(&obj);
                    self.scene.rat_repulsors.push(RatRepulsor {
                        position: ue_point(ch.vector("Location").unwrap_or([0.0; 3])),
                        radius: ch.float("m_fRadius").unwrap_or(500.0) * UNIT,
                        no_summon: ch.bool("m_bPreventDevouringRatSpawn").unwrap_or(false),
                    });
                    Ok(())
                }
                "WorldInfo" => {
                    let ch = self.chain(&obj);
                    if let Some(k) = ch.float("KillZ") {
                        self.scene.kill_y = self.scene.kill_y.min(k * UNIT);
                    }
                    // the persistent level's post-processing
                    if Some(pkg.name.as_str()) == self.scene.packages.first().map(|s| s.as_str()) {
                        if let Some(Value::Struct(_, pp)) = ch.get("m_ArkDefaultPpSettings") {
                            self.scene.post = post_process(&Props(pp.clone()));
                        }
                        // ... and its reflection cube
                        if let Some(cube) = ch.obj(self.assets, "mSceneReflection") {
                            self.scene.scene_reflection = self.cube_map(&cube);
                        }
                    }
                    Ok(())
                }
                c if c.ends_with("Volume") => self.volume(&obj),
                "Trigger" | "DisTrigger" | "Trigger_LOS" | "Trigger_Dynamic" => self.trigger(&obj),
                c if c.starts_with("AkAmbientSound") => self.ambient_sound(&obj),
                c if c.contains("Pickup") || c == "DisElixirHealth" || c.starts_with("DisKey") || c == "DisWhaleBoneCharm" => self.pickup(&obj),
                _ => Ok(()),
            };
            if let Err(e) = r {
                self.warnings += 1;
                log::debug!("actor {} ({cls}) failed: {e:#}", obj.path());
            }
        }

        // Pass 2: components owned by regular actors
        for i in 1..=n {
            if collected.contains(&i) {
                continue;
            }
            let outer = pkg.exports[i as usize - 1].outer;
            if !actor_set.contains(&outer) {
                continue;
            }
            let cls = pkg.class_name(i);
            // the river krusts' shells and the fish are animated by the game, not placed as
            // level meshes
            if cls.ends_with("SkeletalMeshComponent") && matches!(pkg.class_name(outer).as_str(), "DisRiverKrust" | "DisFish" | "DisTripwire" | "DisProjectileLauncher") {
                continue;
            }
            let comp = Obj { pkg: pkg.clone(), idx: i };
            let actor = Obj { pkg: pkg.clone(), idx: outer };
            let r = if cls.ends_with("StaticMeshComponent") || cls == "SplineMeshComponent" {
                let m = self.actor_matrix(&actor) * self.component_local(&comp);
                self.mesh_instance(&comp, &actor, m)
            } else if cls.ends_with("LightComponent") {
                let m = self.actor_matrix(&actor) * self.component_local(&comp);
                self.light(&comp, &actor, m)
            } else if cls.ends_with("SkeletalMeshComponent") {
                let m = self.actor_matrix(&actor) * self.component_local(&comp);
                self.skeletal_instance(&comp, &actor, m)
            } else if cls.ends_with("ParticleSystemComponent") {
                let m = self.actor_matrix(&actor) * self.component_local(&comp);
                self.particle_component(&comp, &actor, m)
            } else {
                Ok(())
            };
            if let Err(e) = r {
                self.warnings += 1;
                log::debug!("component {} failed: {e:#}", comp.path());
            }
        }

        // Pass 3: routes (need resolved actor locations)
        for &a in &actors {
            if pkg.class_name(a) == "DishonoredRoute" {
                let obj = Obj { pkg: pkg.clone(), idx: a };
                if let Err(e) = self.route(&obj) {
                    log::debug!("route failed: {e:#}");
                }
            }
        }
        Ok(())
    }

    fn mesh_collection(&mut self, actor: &Obj, collected: &mut HashSet<i32>) -> Result<()> {
        let od = upk::read_object(&actor.pkg, actor.idx)?;
        let comps = object_array(&actor.pkg, &od.props, "StaticMeshComponents");
        let mut r = od.reader;
        for c in comps {
            let mut m = [0f32; 16];
            for x in m.iter_mut() {
                *x = r.f32()?;
            }
            if c <= 0 || c as usize > actor.pkg.exports.len() {
                continue;
            }
            collected.insert(c);
            let comp = Obj { pkg: actor.pkg.clone(), idx: c };
            // The stored matrix is the component's CachedParentToWorld; the component's own
            // Translation/Rotation/Scale3D still apply on top of it.
            let mat = Mat4::from_cols_array(&m) * self.component_local(&comp);
            if let Err(e) = self.mesh_instance(&comp, actor, mat) {
                self.warnings += 1;
                log::debug!("collection component {} failed: {e:#}", comp.path());
            }
        }
        Ok(())
    }

    fn light_collection(&mut self, actor: &Obj, collected: &mut HashSet<i32>) -> Result<()> {
        let od = upk::read_object(&actor.pkg, actor.idx)?;
        let comps = object_array(&actor.pkg, &od.props, "LightComponents");
        let mut r = od.reader;
        for c in comps {
            let mut m = [0f32; 16];
            for x in m.iter_mut() {
                *x = r.f32()?;
            }
            if c <= 0 || c as usize > actor.pkg.exports.len() {
                continue;
            }
            collected.insert(c);
            let comp = Obj { pkg: actor.pkg.clone(), idx: c };
            let mat = Mat4::from_cols_array(&m) * self.component_local(&comp);
            let _ = self.light(&comp, actor, mat);
        }
        Ok(())
    }

    fn mesh_instance(&mut self, comp: &Obj, actor: &Obj, ue_world: Mat4) -> Result<()> {
        let cch = self.chain(comp);
        let Some(mesh_path) = cch.obj_path("StaticMesh") else { return Ok(()) };
        let Some(mesh_obj) = cch.obj(self.assets, "StaticMesh") else {
            bail!("unresolved mesh {mesh_path}");
        };
        let hidden_comp = cch.bool("HiddenGame").unwrap_or(false);
        let collide = cch.bool("CollideActors").unwrap_or(true) && cch.bool("BlockActors").unwrap_or(true);
        let cast_shadow = cch.bool("CastShadow").unwrap_or(true);
        let reflect = reflect_bits(cch.get("ReflectionChannels"), 0);
        // per-instance material overrides
        let mut overrides: Vec<Option<Obj>> = Vec::new();
        for i in 0..16 {
            match cch.get_idx_pkg("Materials", i) {
                Some((pkg, Value::Array { count, offset, .. })) => {
                    let mut r = Reader::at(&pkg.data, *offset);
                    for _ in 0..*count {
                        let o = r.i32()?;
                        overrides.push(if o != 0 { self.assets.resolve(pkg, o) } else { None });
                    }
                    break;
                }
                _ => break,
            }
        }
        let ach = self.chain(actor);
        let hidden_actor = ach.bool("bHidden").unwrap_or(false);
        let actor_class = actor.class();
        let actor_name = actor.name().to_string();
        let cooked = if comp.class() == "SplineMeshComponent" { self.cook_spline_mesh(&mesh_obj, comp, &cch)? } else { self.cook_mesh(&mesh_obj)? };
        let Some((mesh_id, section_mats)) = cooked else { return Ok(()) };
        let mut materials = section_mats;
        for (i, o) in overrides.into_iter().enumerate() {
            if let (Some(o), true) = (o, i < materials.len()) {
                materials[i] = self.material(&o);
            }
        }
        let dynamic = matches!(
            actor_class.as_str(),
            "InterpActor" | "KActor" | "DishonoredMovable" | "DisDoor" | "KAsset" | "DisCrusher"
        ) || actor_class.contains("Pickup")
            || actor_class == "DisElixirHealth";
        let comp_lm = read_component_lightmap(self.assets, comp);
        let irrelevant: Vec<[u32; 4]> = match cch.get_pkg("IrrelevantLights") {
            Some((pkg, Value::Array { count, offset, .. })) => {
                let mut r = Reader::at(&pkg.data, *offset);
                (0..*count).filter_map(|_| r.guid().ok()).collect()
            }
            _ => Vec::new(),
        };
        let mut sun = if self.sun_guids.iter().any(|g| irrelevant.contains(g)) { 0u8 } else { 2u8 };
        let mut sun_shadow = None;
        if sun != 0 {
            if let Some(lm) = &comp_lm {
                for sm in &lm.shadow_maps {
                    let Ok(sp) = sm.props() else { continue };
                    let Some(Value::Guid(g)) = sp.get("LightGuid") else { continue };
                    if !self.sun_guids.contains(g) {
                        continue;
                    }
                    let Some(tex) = sp.object("Texture").and_then(|t| self.assets.resolve(&sm.pkg, t)) else { continue };
                    let scale = match sp.get("CoordinateScale") {
                        Some(Value::Vec2(v)) => *v,
                        _ => [1.0, 1.0],
                    };
                    let bias = match sp.get("CoordinateBias") {
                        Some(Value::Vec2(v)) => *v,
                        _ => [0.0, 0.0],
                    };
                    let key = tex.key() + "@" + &tex.pkg.name;
                    let id = match self.shadow_tex_ids.get(&key) {
                        Some(&i) => i,
                        None => {
                            let i = self.shadow_texs.len() as u32;
                            self.shadow_texs.push(tex);
                            self.shadow_tex_ids.insert(key, i);
                            i
                        }
                    };
                    sun = 1;
                    sun_shadow = Some(LightMapRef { textures: vec![id], scales: vec![], coord_scale: scale, coord_bias: bias });
                    break;
                }
            }
        }
        // the other lights' static shadow maps (matched to the lights once all are read)
        let inst_index = self.scene.instances.len() as u32;
        if let Some(lm) = &comp_lm {
            for sm in &lm.shadow_maps {
                let Ok(sp) = sm.props() else { continue };
                let Some(Value::Guid(g)) = sp.get("LightGuid") else { continue };
                if self.sun_guids.contains(g) {
                    continue;
                }
                let Some(tex) = sp.object("Texture").and_then(|t| self.assets.resolve(&sm.pkg, t)) else { continue };
                let scale = match sp.get("CoordinateScale") {
                    Some(Value::Vec2(v)) => *v,
                    _ => [1.0, 1.0],
                };
                let bias = match sp.get("CoordinateBias") {
                    Some(Value::Vec2(v)) => *v,
                    _ => [0.0, 0.0],
                };
                let key = tex.key() + "@" + &tex.pkg.name;
                let id = match self.shadow_tex_ids.get(&key) {
                    Some(&i) => i,
                    None => {
                        let i = self.shadow_texs.len() as u32;
                        self.shadow_texs.push(tex);
                        self.shadow_tex_ids.insert(key, i);
                        i
                    }
                };
                self.pending_light_shadows.push((inst_index, *g, LightMapRef { textures: vec![id], scales: vec![], coord_scale: scale, coord_bias: bias }));
            }
        }
        if !irrelevant.is_empty() {
            self.pending_irrelevant.push((inst_index, irrelevant.clone()));
        }
        let lightmap = comp_lm.and_then(|lm| {
            // Identify the pair by name; fall back to slot order.
            let mut nac = None;
            let mut dmc = None;
            for (i, t) in lm.textures.iter().enumerate() {
                if let Some(t) = t {
                    let n = t.name().to_ascii_lowercase();
                    if n.starts_with("normalizedaveragecolor") {
                        nac = Some((t.clone(), lm.scales[i]));
                    } else if n.starts_with("directionalmax") {
                        dmc = Some((t.clone(), lm.scales[i]));
                    }
                }
            }
            if nac.is_none() || dmc.is_none() {
                if let (Some(a), Some(b)) = (&lm.textures[0], &lm.textures[1]) {
                    nac = Some((a.clone(), lm.scales[0]));
                    dmc = Some((b.clone(), lm.scales[1]));
                }
            }
            let ((a, sa), (b, sb)) = (nac?, dmc?);
            let key = format!("{}|{}|{}", a.pkg.name, a.key(), b.key());
            let pair = match self.lm_pair_ids.get(&key) {
                Some(&i) => i,
                None => {
                    let ap = a.props().ok()?;
                    let size = (ap.int("SizeX").unwrap_or(1024).max(4) as u32, ap.int("SizeY").unwrap_or(1024).max(4) as u32);
                    if size.0 > LM_PAGE || size.1 > LM_PAGE {
                        return None;
                    }
                    let i = self.lm_pairs.len() as u32;
                    self.lm_pairs.push(LmPair { nac: a, dmc: b, s_nac: sa, s_dmc: sb, size });
                    self.lm_pair_ids.insert(key, i);
                    i
                }
            };
            // texture id is resolved to a packed page later (see pack_lightmaps)
            Some(LightMapRef { textures: vec![pair], scales: vec![], coord_scale: lm.coord_scale, coord_bias: lm.coord_bias })
        });
        let world = ue_to_bevy(ue_world);
        let index = self.scene.instances.len() as u32;
        self.actor_ref(actor).instances.push(index);
        if matches!(actor_class.as_str(), "DishonoredMovable" | "DishonoredBreakableNavBlock" | "DisWhaleOilBattery" | "DisSpeaker_PA") {
            self.movable(actor, index);
        }

        self.scene.instances.push(Instance {
            mesh: mesh_id,
            materials,
            transform: world.to_cols_array(),
            visible: !hidden_comp && !hidden_actor,
            collide,
            cast_shadow,
            dynamic,
            actor: actor_name,
            class: actor_class,
            lightmap,
            sun,
            sun_shadow,
            light_shadows: Vec::new(),
            irrelevant_lights: Vec::new(),
            reflect,
        });
        Ok(())
    }

    /// Match the instances' shadow maps and irrelevant lights to the dominant lights.
    fn resolve_light_shadows(&mut self) {
        let mut n = 0;
        for (i, g, r) in std::mem::take(&mut self.pending_light_shadows) {
            if let (Some(&li), Some(inst)) = (self.light_guids.get(&g), self.scene.instances.get_mut(i as usize)) {
                inst.light_shadows.push((li, r));
                n += 1;
            }
        }
        for (i, gs) in std::mem::take(&mut self.pending_irrelevant) {
            let Some(inst) = self.scene.instances.get_mut(i as usize) else { continue };
            inst.irrelevant_lights = gs.iter().filter_map(|g| self.light_guids.get(g).copied()).collect();
        }
        log::info!("{} dominant lights, {n} of their shadow maps on instances", self.light_guids.len());
    }

    fn skeletal_instance(&mut self, comp: &Obj, actor: &Obj, ue_world: Mat4) -> Result<()> {
        let cch = self.chain(comp);
        let Some(mesh_obj) = cch.obj(self.assets, "SkeletalMesh") else { return Ok(()) };
        // (a component held in an animation's frame is a mesh of its own)
        let held = cch
            .obj(self.assets, "Animations")
            .filter(|n| n.class() == "AnimNodeSequence")
            .and_then(|n| {
                let nc = self.chain(&n);
                let seq = nc.name("AnimSeqName").filter(|s| *s != "None")?.to_string();
                Some(format!("@{seq}:{}", nc.float("CurrentTime").unwrap_or(0.0)))
            })
            .unwrap_or_default();
        let key = format!("skel:{}{held}", mesh_obj.key());
        let ach = self.chain(actor);
        let hidden = cch.bool("HiddenGame").unwrap_or(false) || ach.bool("bHidden").unwrap_or(false);
        let actor_class = actor.class();
        let (mesh_id, section_mats) = if let Some(&id) = self.mesh_ids.get(&key) {
            (id, self.mesh_materials_cache(&key))
        } else {
            if self.failed_meshes.contains(&key) {
                return Ok(());
            }
            let data = match skeletal::read_skeletal_mesh(&mesh_obj.pkg, mesh_obj.idx) {
                Ok(d) => d,
                Err(e) => {
                    self.failed_meshes.insert(key);
                    bail!("skeletal mesh {}: {e:#}", mesh_obj.path());
                }
            };
            let mats: Vec<u32> = data
                .materials
                .iter()
                .map(|&m| match self.assets.resolve(&mesh_obj.pkg, m) {
                    Some(o) => self.material(&o),
                    None => 0,
                })
                .collect();
            let section_mats: Vec<u32> =
                data.sections.iter().map(|s| *mats.get(s.material as usize).unwrap_or(&0)).collect();
            let file = format!("meshes/{}.mesh", sanitize(&key));
            // (held in a frame of an animation, or the reference pose)
            let pose = self.component_pose(&cch, &data);
            let mf = match &pose {
                Some((_, p)) => skeletal::to_mesh_file_posed(&data, p),
                None => skeletal::to_mesh_file(&data),
            };
            let (min, max) = bounds(&mf.positions);
            if self.opts.force || !self.root.join(&file).exists() {
                mf.write(&self.root.join(&file))?;
            }
            // what it collides with: its physics asset's bodies on the bones (the Tower's boat
            // hull), else the triangles
            let simple = cch.obj(self.assets, "PhysicsAsset").map(|pa| self.physics_asset_hulls(&pa, &data, pose.as_ref().map(|p| p.1.as_slice()))).unwrap_or_default();
            let id = self.scene.meshes.len() as u32;
            self.scene.meshes.push(MeshRef { name: mesh_obj.path(), file, min, max, simple, ..Default::default() });
            self.mesh_ids.insert(key.clone(), id);
            self.mesh_section_mats.insert(key.clone(), section_mats.clone());
            (id, section_mats)
        };
        let world = ue_to_bevy(ue_world);
        let index = self.scene.instances.len() as u32;
        // (its mesh and own animation sets, should a matinee animate it)
        let comp_sets: Vec<Obj> = match cch.0.iter().find(|(_, p)| p.get("AnimSets").is_some()) {
            Some((spkg, sp)) => object_array(spkg, sp, "AnimSets").into_iter().filter_map(|o| self.assets.resolve(spkg, o)).collect(),
            None => Vec::new(),
        };
        self.skel_insts.insert(index, (mesh_obj.clone(), comp_sets));
        // a usable object's moving part
        if actor_class == "DishonoredUsableObject" || actor_class == "DisAudioLogPlayer" {
            if let Some(&u) = self.usable_ids.get(&actor.key()) {
                self.scene.usables[u as usize].instance = Some(index);
            }
        }
        // a door's keyhole
        if actor_class == "DisDoor" {
            let socket = match self.keyhole_sockets.get(&key) {
                Some(s) => *s,
                None => {
                    let name = ach.name("m_KeyHoleSocketName").unwrap_or("KeyHole").to_string();
                    let s = keyhole_socket(&mesh_obj, &name);
                    self.keyhole_sockets.insert(key.clone(), s);
                    s
                }
            };
            if let Some(c) = socket {
                // the door leaf's thickness runs along the mesh's X
                self.scene.keyholes.push(Keyhole {
                    instance: index,
                    actor: actor.name().to_string(),
                    center: world.transform_point3(c).to_array(),
                    normal: world.transform_vector3(Vec3::X).normalize_or_zero().to_array(),
                    fov: ach.float("m_fKeyHoleFOV").unwrap_or(75.0),
                });
            }
        }
        self.actor_ref(actor).instances.push(index);
        self.scene.instances.push(Instance {
            mesh: mesh_id,
            materials: section_mats,
            transform: world.to_cols_array(),
            visible: !hidden,
            collide: actor_class == "DisDoor" || cch.bool("BlockActors").unwrap_or(false),
            cast_shadow: true,
            dynamic: true,
            actor: actor.name().to_string(),
            class: actor_class,
            lightmap: None,
            sun: 2,
            sun_shadow: None,
            light_shadows: Vec::new(),
            irrelevant_lights: Vec::new(),
            reflect: 0,
        });
        Ok(())
    }

    fn mesh_materials_cache(&self, key: &str) -> Vec<u32> {
        self.mesh_section_mats.get(key).cloned().unwrap_or_default()
    }

    /// Returns (mesh id, default per-section material ids).
    fn cook_mesh(&mut self, obj: &Obj) -> Result<Option<(u32, Vec<u32>)>> {
        let key = obj.key();
        if let Some(&id) = self.mesh_ids.get(&key) {
            return Ok(Some((id, self.mesh_materials_cache(&key))));
        }
        if self.failed_meshes.contains(&key) {
            return Ok(None);
        }
        let data = match upk::mesh::read_static_mesh(&obj.pkg, obj.idx) {
            Ok(d) => d,
            Err(e) => {
                self.failed_meshes.insert(key);
                self.warnings += 1;
                log::warn!("mesh {} failed: {e:#}", obj.path());
                return Ok(None);
            }
        };
        let file = format!("meshes/{}.mesh", sanitize(&key));
        let mf = static_to_mesh_file(&data);
        let (min, max) = bounds(&mf.positions);
        if self.opts.force || !self.root.join(&file).exists() {
            mf.write(&self.root.join(&file))?;
        }
        let mats: Vec<u32> = data
            .sections
            .iter()
            .map(|s| match self.assets.resolve(&obj.pkg, s.material) {
                Some(o) => self.material(&o),
                None => 0,
            })
            .collect();
        let simple = self.simple_collision(obj);
        let id = self.scene.meshes.len() as u32;
        self.scene.meshes.push(MeshRef { name: obj.path(), file, min, max, simple, ..Default::default() });
        self.mesh_ids.insert(key.clone(), id);
        self.mesh_section_mats.insert(key, mats.clone());
        Ok(Some((id, mats)))
    }

    /// A spline mesh (`SplineMeshComponent`: cables, pipes, rails): its static mesh bent along
    /// the component's Hermite spline (`SplineParams`) as UE3's spline mesh vertex factory does
    /// it: the mesh's Z (its bounds mapped onto 0..1) runs along the spline, its X and Y across,
    /// in the frame the spline's direction makes with `SplineXDir`, scaled and rolled from start
    /// to end (smoothly with `bSmoothInterpRollScale`). Baked once per component.
    fn cook_spline_mesh(&mut self, obj: &Obj, comp: &Obj, cch: &Chain) -> Result<Option<(u32, Vec<u32>)>> {
        let Some((base, mats)) = self.cook_mesh(obj)? else { return Ok(None) };
        let Some(Value::Struct(_, sp)) = cch.get("SplineParams") else { return Ok(Some((base, mats))) };
        let sp = Props(sp.clone());
        let v3 = |n: &str| sp.vector(n).map(Vec3::from).unwrap_or(Vec3::ZERO);
        let v2 = |n: &str| match sp.get(n) {
            Some(Value::Vec2(v)) => glam::Vec2::from(*v),
            _ => glam::Vec2::ONE,
        };
        let (p0, t0, p1, t1) = (v3("StartPos"), v3("StartTangent"), v3("EndPos"), v3("EndTangent"));
        let (s0, s1) = (v2("StartScale"), v2("EndScale"));
        let (r0, r1) = (sp.float("StartRoll").unwrap_or(0.0), sp.float("EndRoll").unwrap_or(0.0));
        let up = cch.vector("SplineXDir").map(Vec3::from).unwrap_or(Vec3::X);
        let smooth = cch.bool("bSmoothInterpRollScale").unwrap_or(false);
        let key = format!("spline:{}:{}", comp.pkg.name, comp.key());
        let file = format!("meshes/{}.mesh", sanitize(&key));
        let mut data = match upk::mesh::read_static_mesh(&obj.pkg, obj.idx) {
            Ok(d) => d,
            Err(_) => return Ok(Some((base, mats))),
        };
        // along the mesh's Z (`SplineMeshMinZ`, `SplineMeshScaleZ`): its bounds onto 0..1
        let min_z = data.bounds_origin[2] - data.bounds_extent[2];
        let scale_z = 1.0 / (2.0 * data.bounds_extent[2]).max(1e-4);
        let frame = |a: f32| {
            let (a2, a3) = (a * a, a * a * a);
            let pos = (2.0 * a3 - 3.0 * a2 + 1.0) * p0 + (a3 - 2.0 * a2 + a) * t0 + (a3 - a2) * t1 + (-2.0 * a3 + 3.0 * a2) * p1;
            let c = 6.0 * p0 + 3.0 * t0 + 3.0 * t1 - 6.0 * p1;
            let d = -6.0 * p0 - 4.0 * t0 - 2.0 * t1 + 6.0 * p1;
            let dir = (c * a2 + d * a + t0).normalize_or(Vec3::X);
            let h = if smooth { a.clamp(0.0, 1.0) * a.clamp(0.0, 1.0) * (3.0 - 2.0 * a.clamp(0.0, 1.0)) } else { a };
            let scale = s0.lerp(s1, h);
            let roll = r0 + (r1 - r0) * h;
            let bx = up.cross(dir).normalize_or(Vec3::Y);
            let by = dir.cross(bx).normalize_or(Vec3::Z);
            let (sn, cs) = roll.sin_cos();
            let xv = cs * bx - sn * by;
            let yv = cs * by + sn * bx;
            (pos, dir, xv, yv, scale)
        };
        for i in 0..data.positions.len() {
            let p = Vec3::from(data.positions[i]);
            let (pos, dir, xv, yv, scale) = frame((p.z - min_z) * scale_z);
            data.positions[i] = (pos + xv * (p.x * scale.x) + yv * (p.y * scale.y)).to_array();
            if let Some(n) = data.normals.get_mut(i) {
                let v = Vec3::from(*n);
                *n = (xv * v.x + yv * v.y + dir * v.z).normalize_or_zero().to_array();
            }
            if let Some(t) = data.tangents.get_mut(i) {
                let v = Vec3::new(t[0], t[1], t[2]);
                let r = (xv * v.x + yv * v.y + dir * v.z).normalize_or_zero();
                *t = [r.x, r.y, r.z, t[3]];
            }
        }
        let mf = static_to_mesh_file(&data);
        let (min, max) = bounds(&mf.positions);
        if self.opts.force || !self.root.join(&file).exists() {
            mf.write(&self.root.join(&file))?;
        }
        let id = self.scene.meshes.len() as u32;
        self.scene.meshes.push(MeshRef { name: format!("{} (spline {})", obj.path(), comp.name()), file, min, max, simple: Vec::new(), ..Default::default() });
        Ok(Some((id, mats)))
    }

    /// A skeletal mesh's collision from its physics asset (`PhysicsAsset.BodySetup`): each
    /// body's boxes, spheres, capsules and convex pieces on its bone in the reference pose, as
    /// convex hulls in mesh space (Bevy).
    /// A skeletal component's own pose (`Animations`: an AnimNodeSequence holding a sequence of
    /// its AnimSets at a time: the Tower's lowered gangway), as bone world matrices (Bevy space),
    /// keyed by the sequence and time.
    fn component_pose(&mut self, cch: &Chain, data: &skeletal::SkeletalMeshData) -> Option<(String, Vec<Mat4>)> {
        let node = cch.obj(self.assets, "Animations").filter(|n| n.class() == "AnimNodeSequence")?;
        let nc = self.chain(&node);
        let seq = nc.name("AnimSeqName").filter(|s| *s != "None")?.to_string();
        let time = nc.float("CurrentTime").unwrap_or(0.0);
        let (spkg, sprops) = cch.0.iter().find(|(_, p)| p.get("AnimSets").is_some()).map(|(k, p)| (k.clone(), p.clone()))?;
        for set in object_array(&spkg, &sprops, "AnimSets") {
            let Some(set) = self.assets.resolve(&spkg, set) else { continue };
            let Ok(od) = upk::read_object(&set.pkg, set.idx) else { continue };
            for s in object_array(&set.pkg, &od.props, "Sequences") {
                let Ok(so) = upk::read_object(&set.pkg, s) else { continue };
                if !so.props.name("SequenceName").is_some_and(|n| n.eq_ignore_ascii_case(&seq)) {
                    continue;
                }
                let mut r = so.reader;
                let (Ok(_), Ok(len)) = (r.i32(), r.i32()) else { continue };
                let Ok(blob) = r.bytes(len.max(0) as usize) else { continue };
                let Ok(a) = edge::EdgeAnim::decode(blob) else { continue };
                let layout = anim_layout(&set, &PendingAnims::new(0, Vec::new(), data), &[]).ok()?;
                if a.num_joints != layout.names.len() {
                    return None;
                }
                let bones = skeletal::to_skeleton(data);
                let mut local: Vec<(glam::Quat, Vec3)> = bones.iter().map(|b| (glam::Quat::from_array(b.rotation).normalize(), Vec3::from(b.translation))).collect();
                let frame = (time * a.frequency).max(0.0);
                let key = |frames: &[u16]| frames.partition_point(|&x| (x as f32) <= frame).saturating_sub(1);
                let bone_of = |joint: u16| layout.names.get(joint as usize).and_then(|n| bones.iter().position(|b| b.name.eq_ignore_ascii_case(n)));
                let cq = layout.root_rot;
                // (as the animation cook converts them)
                for t in &a.rotations {
                    let (Some(bi), Some(q)) = (bone_of(t.joint), t.values.get(key(&t.frames).min(t.values.len().saturating_sub(1)))) else { continue };
                    let q = glam::Quat::from_array(*q).normalize();
                    let q = if layout.root == Some(t.joint as usize) { cq * q } else { q };
                    local[bi].0 = glam::Quat::from_array([-q.x, -q.z, -q.y, q.w]).normalize();
                }
                for t in &a.translations {
                    let (Some(bi), Some(v)) = (bone_of(t.joint), t.values.get(key(&t.frames).min(t.values.len().saturating_sub(1)))) else { continue };
                    let v = if layout.root == Some(t.joint as usize) { cq * Vec3::from(*v) + layout.root_offset } else { Vec3::from(*v) };
                    local[bi].1 = Vec3::from(ue_point(v.to_array()));
                }
                let mut world: Vec<Mat4> = Vec::with_capacity(bones.len());
                for (b, (q, t)) in bones.iter().zip(local) {
                    let l = Mat4::from_rotation_translation(q, t);
                    world.push(if b.parent >= 0 && (b.parent as usize) < world.len() { world[b.parent as usize] * l } else { l });
                }
                return Some((format!("{seq}@{time}"), world));
            }
        }
        None
    }

    fn physics_asset_hulls(&mut self, pa: &Obj, data: &skeletal::SkeletalMeshData, posed: Option<&[Mat4]>) -> Vec<Vec<[f32; 3]>> {
        let Ok(pp) = pa.props() else { return Vec::new() };
        // the bones' reference pose (Bevy space, the component transform in the root), or the
        // component's own
        let bones = skeletal::to_skeleton(data);
        let world: Vec<Mat4> = match posed {
            Some(p) if p.len() == bones.len() => p.to_vec(),
            _ => skeletal::ref_world(&bones),
        };
        let mut hulls = Vec::new();
        for bi in upk::props::object_array(&pa.pkg, &pp, "BodySetup") {
            let Some(body) = self.assets.resolve(&pa.pkg, bi) else { continue };
            let bch = self.chain(&body);
            let Some(bone) = bch.name("BoneName").and_then(|n| bones.iter().position(|b| b.name.eq_ignore_ascii_case(n))) else { continue };
            let bm = world[bone];
            let Some((gpkg, Value::Struct(_, geom))) = bch.get_pkg("AggGeom") else { continue };
            let (gpkg, geom) = (gpkg.clone(), Props(geom.clone()));
            // an element's frame (UE3 rows: X, Y, Z axes, origin; bone space, UE units)
            let frame = |e: &Props| match e.get("TM") {
                Some(Value::Matrix(m)) => Mat4::from_cols(
                    glam::Vec4::new(m[0], m[1], m[2], 0.0),
                    glam::Vec4::new(m[4], m[5], m[6], 0.0),
                    glam::Vec4::new(m[8], m[9], m[10], 0.0),
                    glam::Vec4::new(m[12], m[13], m[14], 1.0),
                ),
                _ => Mat4::IDENTITY,
            };
            let place = |tm: Mat4, pts: &[Vec3]| -> Vec<[f32; 3]> { pts.iter().map(|p| bm.transform_point3(Vec3::from(ue_point(tm.transform_point3(*p).to_array()))).to_array()).collect() };
            let elems = |name: &str| geom.array(name).and_then(|(c, off, sz)| parse_struct_array(&gpkg, off, sz, c).ok()).unwrap_or_default();
            for e in elems("BoxElems") {
                // (X, Y, Z: the full sizes)
                let h = Vec3::new(e.float("X").unwrap_or(0.0), e.float("Y").unwrap_or(0.0), e.float("Z").unwrap_or(0.0)) * 0.5;
                let corners: Vec<Vec3> = (0..8).map(|i| Vec3::new(if i & 1 == 0 { -h.x } else { h.x }, if i & 2 == 0 { -h.y } else { h.y }, if i & 4 == 0 { -h.z } else { h.z })).collect();
                hulls.push(place(frame(&e), &corners));
            }
            // spheres and capsules (along Z, `Length` between the ends' centres): their
            // points on a few rings
            let round = |r: f32, half: f32| -> Vec<Vec3> {
                let mut pts = Vec::new();
                for end in [-half, half] {
                    for ring in 0..3 {
                        let a = (ring as f32 - 1.0) * std::f32::consts::FRAC_PI_4;
                        let (rz, rr) = (r * a.sin(), r * a.cos());
                        for k in 0..8 {
                            let b = k as f32 * std::f32::consts::FRAC_PI_4;
                            pts.push(Vec3::new(rr * b.cos(), rr * b.sin(), end + rz));
                        }
                    }
                }
                pts.push(Vec3::new(0.0, 0.0, -half - r));
                pts.push(Vec3::new(0.0, 0.0, half + r));
                pts
            };
            for e in elems("SphereElems") {
                hulls.push(place(frame(&e), &round(e.float("Radius").unwrap_or(0.0), 0.0)));
            }
            for e in elems("SphylElems") {
                hulls.push(place(frame(&e), &round(e.float("Radius").unwrap_or(0.0), e.float("Length").unwrap_or(0.0) * 0.5)));
            }
            for e in elems("ConvexElems") {
                if let Some((vc, voff, _)) = e.array("VertexData") {
                    let mut r = Reader::at(&gpkg.data, voff);
                    let pts: Vec<Vec3> = (0..vc).filter_map(|_| r.vec3().ok()).map(Vec3::from).collect();
                    if pts.len() >= 4 {
                        hulls.push(place(Mat4::IDENTITY, &pts));
                    }
                }
            }
        }
        hulls.retain(|h| h.len() >= 4);
        hulls
    }

    /// A static mesh's simplified collision: its `BodySetup.AggGeom` boxes, spheres, capsules
    /// and convex hulls as hulls (mesh space), unless `UseSimpleBoxCollision` is off. (The
    /// Prison cells' floors are boxes.)
    fn simple_collision(&mut self, obj: &Obj) -> Vec<Vec<[f32; 3]>> {
        let ch = self.chain(obj);
        if ch.bool("UseSimpleBoxCollision") == Some(false) {
            return Vec::new();
        }
        let Some(bs) = ch.obj(self.assets, "BodySetup") else { return Vec::new() };
        let bch = self.chain(&bs);
        let mut hulls: Vec<Vec<[f32; 3]>> = Vec::new();
        let Some((gpkg, Value::Struct(_, geom))) = bch.get_pkg("AggGeom") else { return hulls };
        let (gpkg, geom) = (gpkg.clone(), Props(geom.clone()));
        // an element's frame (UE3 rows: X, Y, Z axes, origin; mesh space, UE units)
        let frame = |e: &Props| match e.get("TM") {
            Some(Value::Matrix(m)) => Mat4::from_cols(
                glam::Vec4::new(m[0], m[1], m[2], 0.0),
                glam::Vec4::new(m[4], m[5], m[6], 0.0),
                glam::Vec4::new(m[8], m[9], m[10], 0.0),
                glam::Vec4::new(m[12], m[13], m[14], 1.0),
            ),
            _ => Mat4::IDENTITY,
        };
        let place = |tm: Mat4, pts: &[Vec3]| -> Vec<[f32; 3]> { pts.iter().map(|p| ue_point(tm.transform_point3(*p).to_array())).collect() };
        let elems = |name: &str| geom.array(name).and_then(|(c, off, sz)| parse_struct_array(&gpkg, off, sz, c).ok()).unwrap_or_default();
        for e in elems("BoxElems") {
            // (X, Y, Z: the full sizes)
            let h = Vec3::new(e.float("X").unwrap_or(0.0), e.float("Y").unwrap_or(0.0), e.float("Z").unwrap_or(0.0)) * 0.5;
            if h.min_element() <= 0.0 {
                continue;
            }
            let corners: Vec<Vec3> = (0..8).map(|i| Vec3::new(if i & 1 == 0 { -h.x } else { h.x }, if i & 2 == 0 { -h.y } else { h.y }, if i & 4 == 0 { -h.z } else { h.z })).collect();
            hulls.push(place(frame(&e), &corners));
        }
        // spheres and capsules (along Z, `Length` between the ends' centres): their points on
        // a few rings
        let round = |r: f32, half: f32| -> Vec<Vec3> {
            let mut pts = Vec::new();
            for end in [-half, half] {
                for ring in 0..3 {
                    let a = (ring as f32 - 1.0) * std::f32::consts::FRAC_PI_4;
                    let (rz, rr) = (r * a.sin(), r * a.cos());
                    for k in 0..8 {
                        let b = k as f32 * std::f32::consts::FRAC_PI_4;
                        pts.push(Vec3::new(rr * b.cos(), rr * b.sin(), end + rz));
                    }
                }
            }
            pts.push(Vec3::new(0.0, 0.0, -half - r));
            pts.push(Vec3::new(0.0, 0.0, half + r));
            pts
        };
        for e in elems("SphereElems") {
            let r = e.float("Radius").unwrap_or(0.0);
            if r > 0.0 {
                hulls.push(place(frame(&e), &round(r, 0.0)));
            }
        }
        for e in elems("SphylElems") {
            let r = e.float("Radius").unwrap_or(0.0);
            if r > 0.0 {
                hulls.push(place(frame(&e), &round(r, e.float("Length").unwrap_or(0.0) * 0.5)));
            }
        }
        for e in elems("ConvexElems") {
            if let Some((vc, voff, _)) = e.array("VertexData") {
                let mut r = Reader::at(&gpkg.data, voff);
                let pts: Vec<[f32; 3]> = (0..vc).filter_map(|_| r.vec3().ok()).map(ue_point).collect();
                if pts.len() >= 4 {
                    hulls.push(pts);
                }
            }
        }
        hulls
    }

    fn texture(&mut self, job: TexJob) -> u32 {
        let key = match &job {
            TexJob::Plain(o) => o.key(),
            TexJob::Masked(d, m) => format!("{}+{}", d.key(), m.key()),
            TexJob::Mask(m) => format!("mask.{}", m.key()),
            TexJob::LightmapPage(_, entries) => {
                // content hash of the page layout so cached pages are reused only when identical
                use std::hash::{Hash, Hasher};
                let mut h = std::collections::hash_map::DefaultHasher::new();
                for (p, x, y) in entries {
                    (p.nac.pkg.name.as_str(), p.nac.key(), p.dmc.key(), x, y).hash(&mut h);
                }
                format!("lmpage.{}.{:016x}", self.scene.name.to_ascii_lowercase(), h.finish())
            }
            TexJob::LightmapRaw(_, entries, which) => {
                use std::hash::{Hash, Hasher};
                let mut h = std::collections::hash_map::DefaultHasher::new();
                for (p, x, y) in entries {
                    (p.nac.pkg.name.as_str(), p.nac.key(), p.dmc.key(), x, y).hash(&mut h);
                }
                format!("lmraw{which}.{}.{:016x}", self.scene.name.to_ascii_lowercase(), h.finish())
            }
            TexJob::ShadowPage(_, entries) => {
                use std::hash::{Hash, Hasher};
                let mut h = std::collections::hash_map::DefaultHasher::new();
                for (o, x, y) in entries {
                    (o.pkg.name.as_str(), o.key(), x, y).hash(&mut h);
                }
                format!("shpage.{}.{:016x}", self.scene.name.to_ascii_lowercase(), h.finish())
            }
        };
        if let Some(&id) = self.tex_ids.get(&key) {
            return id;
        }
        let id = self.scene.textures.len() as u32;
        // a render target is drawn at run time (a scene capture's), nothing to cook
        if let TexJob::Plain(o) = &job {
            if o.class().starts_with("TextureRenderTarget") {
                let ch = self.chain(o);
                let share = match ch.name("m_ResolutionType") {
                    Some("TRT_HALFSIZE") => 0.5,
                    Some("TRT_QUARTERSIZE") => 0.25,
                    Some("TRT_FULLSIZE") => 1.0,
                    _ => 0.0,
                };
                let size = [ch.float("SizeX").unwrap_or(256.0), ch.float("SizeY").unwrap_or(256.0), share];
                self.scene.textures.push(TextureRef { name: key.clone(), file: String::new(), render_target: Some(size) });
                self.tex_ids.insert(key, id);
                return id;
            }
        }
        let file = format!("textures/{}.tex", sanitize(&key));
        self.scene.textures.push(TextureRef { name: key.clone(), file, render_target: None });
        self.tex_ids.insert(key, id);
        self.tex_jobs.push((id, job));
        id
    }

    fn material(&mut self, obj: &Obj) -> u32 {
        let key = obj.key();
        if let Some(&id) = self.mat_ids.get(&key) {
            return id;
        }
        let rm = resolve_material(self.assets, obj);
        let def = self.build_material(obj, &rm);
        let id = self.scene.materials.len() as u32;
        self.scene.materials.push(def);
        self.mat_ids.insert(key, id);
        id
    }

    fn build_material(&mut self, obj: &Obj, rm: &ResolvedMaterial) -> MaterialDef {
        let diffuse = rm
            .find_tex(
                &["D_diffuse_texture", "D_Diffuse", "Diffuse", "DiffuseTexture", "Diffuse_Texture"],
                &["diffuse", "albedo", "basecolor", "base_color", "color_tex"],
                &["variation", "mask", "detail"],
            )
            .or_else(|| rm.find_by_suffix(&["_d", "_diff", "_diffuse", "_d_tx", "_col"]))
            .or_else(|| rm.unnamed.iter().find(|o| rm.obj_used(o) && !materials::is_placeholder(o)).cloned());
        let normal = rm
            .find_tex(&["N_NormalMap_texture", "N_Normals", "Normal", "NormalMap"], &["normal"], &["detail", "variation", "blur"])
            .or_else(|| rm.find_by_suffix(&["_n", "_normal", "_nrm"]));
        let specular = rm
            .find_tex(&["S_Specular_texture", "S_Specular", "Specular"], &["specular"], &["variation", "power", "mask"])
            .or_else(|| rm.find_by_suffix(&["_s", "_spec"]));
        let emissive = rm
            .find_tex(&["E_Emissive_texture", "Emissive"], &["emissive", "glow", "illum"], &["mask"])
            .or_else(|| rm.find_by_suffix(&["_e", "_emissive", "_glow"]));
        let opacity = rm.find_tex(&["O_Opacity_mask", "O_Opacity", "Alpha", "OpacityMask", "LightRays_Mask_R"], &["opacity"], &[]);
        if std::env::var("DH_MATDEBUG").is_ok() {
            log::info!(
                "mat {} used={:?} d={:?} n={:?} e={:?} o={:?} vec={:?} sc={:?} tex={:?}",
                obj.path(),
                rm.used.as_ref().map(|(u, l)| (u.len(), *l)),
                diffuse.as_ref().map(|o| o.key()),
                normal.as_ref().map(|o| o.key()),
                emissive.as_ref().map(|o| o.key()),
                opacity.as_ref().map(|o| o.key()),
                rm.vectors,
                rm.scalars,
                rm.textures.iter().map(|(n, o)| format!("{n}={}", o.key())).collect::<Vec<_>>()
            );
        }

        let mut blend = rm.blend;
        let path = obj.path().to_ascii_lowercase();
        let chain_txt = rm.chain.join(" ").to_ascii_lowercase();
        if blend == Blend::Opaque && (chain_txt.contains("masked") || opacity.is_some() && chain_txt.contains("mask")) {
            blend = Blend::Masked;
        }
        if blend == Blend::Opaque && chain_txt.contains("translucent") {
            blend = Blend::Translucent;
        }
        // Blockout materials draw a flat colour with a faint debug grid; keep only the colour.
        let blockout = chain_txt.contains("blockout_unlit");
        let diffuse = if blockout { None } else { diffuse };
        // VFX cards often carry only an intensity/opacity mask (tinted by a colour parameter).
        let mask_only = diffuse.is_none() && opacity.is_some() && matches!(blend, Blend::Additive | Blend::Translucent);
        let diffuse = if mask_only { opacity.clone() } else { diffuse };
        let diffuse_id = match (&diffuse, &opacity, blend) {
            (Some(d), _, _) if mask_only => Some(self.texture(TexJob::Mask(d.clone()))),
            (Some(d), Some(o), Blend::Masked | Blend::Translucent) if d.key() != o.key() => {
                Some(self.texture(TexJob::Masked(d.clone(), o.clone())))
            }
            (Some(d), _, _) => Some(self.texture(TexJob::Plain(d.clone()))),
            _ => None,
        };
        let normal_id = normal.map(|o| self.texture(TexJob::Plain(o)));
        let specular_id = specular.map(|o| self.texture(TexJob::Plain(o)));
        let emissive_id = emissive.map(|o| self.texture(TexJob::Plain(o)));
        let mut tint = [1.0, 1.0, 1.0, 1.0];
        let mut found_tint = false;
        // generic_PMAT only applies the colour/tiling parameters when their static switch is on
        let sw = |n: &str| rm.switch(n).unwrap_or(true);
        for name in ["D_Diffuse_Color", "Diffuse_Color", "DiffuseColor", "D_diffuse_tint", "Tint", "Color", "Basic_Color", "D_DiffuseColorTweak"] {
            if let Some(v) = rm.vector(name) {
                if name == "D_Diffuse_Color" && !sw("D_Enable_Diffuse_color") {
                    found_tint = true;
                    break;
                }
                tint = v;
                found_tint = true;
                break;
            }
        }
        if !found_tint && rm.unlit {
            // VFX parents name their colour after the effect ("Birds_Color", "Smoke_Color", ...)
            let skip = ["fresnel", "fog", "cubemap", "alert", "specular", "emissive", "tilling", "tiling"];
            if let Some((_, v)) = rm.vectors.iter().find(|(n, _)| {
                let n = n.to_ascii_lowercase();
                n.ends_with("color") && !skip.iter().any(|s| n.contains(s))
            }) {
                tint = *v;
            }
        }
        let finite = |v: [f32; 2]| if v.iter().all(|v| v.is_finite() && v.abs() > 1e-3 && v.abs() < 1000.0) { v } else { [1.0, 1.0] };
        let normal_uv_scale = match rm.switch("N_Enable_NormalMap_Tilling") {
            Some(true) => rm.scalar("N_NormalMap_Tilling").map(|t| finite([t, t])).unwrap_or([1.0, 1.0]),
            Some(false) => [1.0, 1.0],
            None => [0.0, 0.0], // unknown: follow the diffuse tiling
        };
        let uv_scale = if !sw("D_Enable_Diffuse_tilling") {
            [1.0, 1.0]
        } else if let Some(t) = ["D_Diffuse_Tilling", "D_Diffuse_Tiling", "Diffuse_Tilling", "D_Tilling"].iter().find_map(|n| rm.scalar(n)) {
            [t, t]
        } else if let Some(v) = ["UV_Tilling", "UV_Tiling", "D_Diffuse_UV_Tilling"].iter().find_map(|n| rm.vector(n)) {
            [v[0], v[1]]
        } else {
            [1.0, 1.0]
        };
        let uv_scale = finite(uv_scale);
        let normal_uv_scale = if normal_uv_scale == [0.0, 0.0] { uv_scale } else { normal_uv_scale };
        let layer = if rm.switch("D2_Enable_Diffuse_Variation") == Some(true) && blend != Blend::Translucent {
            let texture = rm
                .tex("D2_diffuse_variation_texture")
                .filter(|o| rm.param_used("D2_diffuse_variation_texture") && !materials::is_placeholder(o))
                .cloned()
                .map(|o| self.texture(TexJob::Plain(o)));
            let mask_name = if rm.switch("D2_Diffuse_variation_mask_equal_cubemap_mask") == Some(true) {
                "C_Cubemap_Mask"
            } else {
                "D2_Diffuse_variation_mask"
            };
            let mask = rm.tex(mask_name).filter(|o| !materials::is_placeholder(o)).cloned().map(|o| self.texture(TexJob::Plain(o)));
            let color = if rm.switch("D2_Enable_Diffuse_variation_color") == Some(true) {
                rm.vector("D2_Diffuse_variation_color").unwrap_or([1.0; 4])
            } else {
                [1.0; 4]
            };
            (texture.is_some() || color != [1.0; 4]).then(|| LayerDef {
                texture,
                color,
                tiling: rm.scalar("D2_Diffuse_variation_Tilling").filter(|t| t.is_finite() && *t > 1e-3).unwrap_or(1.0),
                mask,
                mask_channel: rm.mask_channel("D2_Diffuse_Variation_mask_channel").unwrap_or(0) as u32,
                mask_tiling: 1.0,
                mask_power: rm.scalar("D2_Variation_Mask_Power").filter(|p| p.is_finite() && *p > 0.0).unwrap_or(1.0),
                multiply: rm.switch("D2_Enable_Multiply_for_variation") == Some(true),
            })
        } else {
            None
        };
        let mut emissive_color = [0.0; 3];
        if emissive_id.is_some() {
            let mut e = [1.0, 1.0, 1.0];
            for name in ["E_Emissive_Color", "Emissive_Color", "EmissiveColor"] {
                if let Some(v) = rm.vector(name) {
                    e = [v[0], v[1], v[2]];
                    break;
                }
            }
            let mut mult = 1.0;
            for name in ["E_Emissive_Intensity", "E_Emissive_Power", "Emissive_Intensity", "EmissiveIntensity", "E_Emissive_multiply"] {
                if let Some(v) = rm.scalar(name) {
                    mult = v;
                    break;
                }
            }
            emissive_color = [e[0] * mult, e[1] * mult, e[2] * mult];
        }
        // Water sheets running down walls/windows: a subtle distortion effect we don't reproduce.
        let water_slide = chain_txt.contains("water_slide_pmat");
        let invisible = water_slide
            // highlight overlays shown only while the player focuses an interactable
            || chain_txt.contains("golden_focus_pmat")
            // distant flocks: a scrolling flipbook ribbon that reads as a grey band without its shader
            || (chain_txt.contains("wind_motion_pmat") && path.contains("bird"))
            || path.contains("invisible")
            || path.contains("coll_model")
            || path.contains(".collision")
            || path.contains("nodraw")
            || chain_txt.contains("invisible_mat")
            || chain_txt.contains("m_collision")
            || chain_txt.contains("collision_mat");
        let is_sky = chain_txt.contains("skies_pmat") || (path.starts_with("skies.") && rm.unlit);
        let sky = if is_sky {
            let v3 = |n: &str, d: [f32; 3]| rm.vector(n).map(|v| [v[0], v[1], v[2]]).unwrap_or(d);
            let v4 = |n: &str, d: [f32; 4]| rm.vector(n).unwrap_or(d);
            let tex = |s: &mut Self, n: &str| rm.tex(n).filter(|o| !materials::is_placeholder(o) && !o.key().ends_with("black_d")).cloned().map(|o| s.texture(TexJob::Plain(o)));
            Some(SkyDef {
                top: v3("G_Gradiant_Top", [0.1, 0.3, 0.8]),
                bottom: v3("G_Gradiant_Bottom", [0.6, 0.4, 0.2]),
                horizon: v3("Horizon_Color", [0.3, 0.4, 0.6]),
                horizon_intensity: rm.scalar("H_HorizonIntensity").unwrap_or(1.0),
                horizon_exponent: rm.scalar("H_HorizonExponent").unwrap_or(2.0),
                gradient_power: rm.scalar("G_Gradiant_Power").unwrap_or(1.0),
                clouds: tex(self, "C_Clouds_Texture"),
                clouds_color: v4("C_Clouds_Color", [1.0, 1.0, 1.0, 1.0]),
                clouds_visibility: rm.scalar("C_Clouds_Visibility").unwrap_or(0.5),
                clouds_speed: rm.scalar("C_Clouds_Speed").unwrap_or(0.1),
                clouds2: tex(self, "C2_Clouds_2_Texture"),
                clouds2_color: v4("C2_Clouds_2_Color", [1.0, 1.0, 1.0, 1.0]),
                clouds2_visibility: rm.scalar("C2_Clouds_2_Visibility").unwrap_or(0.5),
                clouds2_speed: rm.scalar("C2_Clouds_2_Speed").unwrap_or(0.1),
                storm: tex(self, "Storm"),
                storm_color: v3("Storm_Color", [1.0, 1.0, 1.0]),
                storm_intensity: rm.scalar("Storm_Intensity").unwrap_or(0.0),
            })
        } else {
            None
        };
        // Only real water surfaces (Materials_ref.water_materials.* parents, VFX spillways);
        // names alone are misleading ("waterlock" walls, "underwater" ground).
        let water = !water_slide
            && (chain_txt.contains("water_materials.") || chain_txt.contains("water_spillway"))
            && matches!(blend, Blend::Translucent | Blend::Opaque);
        let opacity = match blend {
            Blend::Translucent if opacity.is_none() => Some(rm.scalar("Opacity").or(rm.scalar("O_Opacity")).unwrap_or(tint[3]).clamp(0.05, 0.9)),
            _ => None,
        };
        MaterialDef {
            name: obj.path(),
            sky,
            water,
            opacity,
            diffuse: diffuse_id,
            normal: normal_id,
            specular: specular_id,
            emissive: emissive_id,
            blend,
            two_sided: rm.two_sided,
            unlit: rm.unlit,
            alpha_cutoff: rm.cutoff,
            tint,
            emissive_color,
            invisible,
            uv_scale,
            normal_uv_scale,
            layer,
            ue3: self.ue3_material(&rm),
            surface: rm.phys.as_deref().map(|s| s.trim_start_matches("Phm_").to_string()).unwrap_or_default(),
        }
    }

    /// Link a material to its compiled shader map and resolve the map's parameters and
    /// textures for this material (instance).
    fn ue3_material(&mut self, rm: &ResolvedMaterial) -> Option<Ue3Material> {
        let lib = shader_library(self.assets)?;
        let Some((owner, res)) = rm.resource.as_ref() else {
            log::debug!("no material resource: {}", rm.chain.join(" > "));
            return None;
        };
        let base = rm.base_resource.as_ref().map(|b| b.id).unwrap_or(res.id);
        let Some(mi) = lib.find(&base, &rm.switches, &rm.masks) else {
            log::debug!(
                "no shader map for {} ({} switches, {} masks)",
                rm.chain.join(" > "),
                rm.switches.len(),
                rm.masks.len()
            );
            return None;
        };
        let map = shadercache::CookedMap::from(&lib.maps[mi]);
        let params = shadercache::param_nodes(&map)
            .iter()
            .map(|e| match e {
                shadercache::Expr::VectorParameter(n, d) => rm.vector(n).unwrap_or(*d),
                shadercache::Expr::ScalarParameter(n, d) => [rm.scalar(n).unwrap_or(*d); 4],
                _ => [0.0; 4],
            })
            .collect();
        let param_names = shadercache::param_nodes(&map)
            .iter()
            .map(|e| match e {
                shadercache::Expr::VectorParameter(n, _) | shadercache::Expr::ScalarParameter(n, _) => n.clone(),
                _ => String::new(),
            })
            .collect();
        let res_tex = |i: i32| -> Option<Obj> {
            let r = *res.textures.get(usize::try_from(i).ok()?)?;
            if r == 0 {
                return None;
            }
            self.assets.resolve(&owner.pkg, r)
        };
        let texture = |c: &mut Self, e: &shadercache::Expr, cube: bool| -> Option<u32> {
            let obj = match e {
                shadercache::Expr::TextureParameter(n, i) => rm.tex(n).cloned().or_else(|| res_tex(*i)),
                shadercache::Expr::Texture(i) | shadercache::Expr::FlipBook(i) => res_tex(*i),
                _ => None,
            }?;
            let cls = obj.class();
            // scene capture targets: drawn at run time (`reflections`: the meshes in their
            // channels, mirrored)
            if cls.starts_with("TextureRenderTarget") {
                return Some(c.texture(TexJob::Plain(obj)));
            }
            if cube {
                return c.cube_map(&obj);
            }
            if !cls.starts_with("Texture2D") && cls != "TextureFlipBook" && cls != "LightMapTexture2D" {
                return None;
            }
            Some(c.texture(TexJob::Plain(obj)))
        };
        let textures = map.pixel.textures.iter().map(|e| texture(self, e, false)).collect();
        let cube_textures = map.cube_textures.iter().map(|e| texture(self, e, true)).collect();
        Some(Ue3Material { map: mi as u32, params, param_names, textures, cube_textures, blend: rm.blend, two_sided: rm.two_sided, unlit: rm.unlit })
    }

    /// A `TextureCube`'s six faces, cooked (index into `scene.cubes`).
    fn cube_map(&mut self, obj: &Obj) -> Option<u32> {
        if obj.class() != "TextureCube" {
            return None;
        }
        let ch = self.chain(obj);
        let faces: Vec<Obj> = ["FacePosX", "FaceNegX", "FacePosY", "FaceNegY", "FacePosZ", "FaceNegZ"].iter().filter_map(|f| ch.obj(self.assets, f)).collect();
        if faces.len() != 6 {
            return None;
        }
        let ids: Vec<u32> = faces.into_iter().map(|f| self.texture(TexJob::Plain(f))).collect();
        let ids = [ids[0], ids[1], ids[2], ids[3], ids[4], ids[5]];
        let i = match self.scene.cubes.iter().position(|x| *x == ids) {
            Some(i) => i,
            None => {
                self.scene.cubes.push(ids);
                self.scene.cubes.len() - 1
            }
        };
        Some(i as u32)
    }

    fn light(&mut self, comp: &Obj, actor: &Obj, ue_world: Mat4) -> Result<()> {
        let ch = self.chain(comp);
        let cls = comp.class();
        let kind = if cls.contains("SpotLight") {
            LightKind::Spot
        } else if cls.contains("Directional") {
            LightKind::Directional
        } else if cls.contains("SkyLight") {
            LightKind::Sky
        } else if cls.contains("PointLight") {
            LightKind::Point
        } else {
            return Ok(());
        };
        let world = ue_to_bevy(ue_world);
        let pos = world.w_axis.truncate();
        let dir = world.x_axis.truncate().normalize_or_zero();
        let c = ch.color("LightColor").unwrap_or([255, 255, 255, 255]);
        let color = [srgb_to_linear(c[0]), srgb_to_linear(c[1]), srgb_to_linear(c[2])];
        let actor_class = actor.class();
        let ach = self.chain(actor);
        let enabled = ch.bool("bEnabled").unwrap_or(true) && !ach.bool("bHidden").unwrap_or(false);
        let dynamic = actor_class.contains("Movable")
            || actor_class.contains("Toggleable")
            || ch.bool("bForceDynamicLight").unwrap_or(false)
            || !ch.bool("UseDirectLightMap").unwrap_or(false) && kind != LightKind::Sky;
        let index = self.scene.lights.len() as u32;
        let dominant = cls.starts_with("Dominant") && kind != LightKind::Directional;
        if dominant {
            if let Some(Value::Guid(g)) = ch.get("LightGuid") {
                self.light_guids.insert(*g, index);
            }
        }
        self.actor_ref(actor).lights.push(index);
        self.scene.lights.push(Light {
            dominant,
            kind,
            position: pos.to_array(),
            direction: dir.to_array(),
            color,
            brightness: ch.float("Brightness").unwrap_or(1.0),
            radius: ch.float("Radius").unwrap_or(1024.0) * UNIT,
            falloff: ch.float("FalloffExponent").unwrap_or(2.0),
            inner_cone: ch.float("InnerConeAngle").unwrap_or(0.0),
            outer_cone: ch.float("OuterConeAngle").unwrap_or(44.0),
            shadows: ch.bool("CastShadows").unwrap_or(true) && ch.bool("CastDynamicShadows").unwrap_or(true),
            enabled,
            dynamic,
            actor: actor.name().to_string(),
            shafts: ch.bool("bRenderLightShafts").unwrap_or(false).then(|| {
                let t = ch.color("BloomTint").unwrap_or([255, 255, 255, 255]);
                LightShafts {
                    occlusion_range: ch.float("OcclusionDepthRange").unwrap_or(20000.0) * UNIT,
                    bloom_scale: ch.float("BloomScale").unwrap_or(2.0),
                    bloom_threshold: ch.float("BloomThreshold").unwrap_or(0.0),
                    screen_blend_threshold: ch.float("BloomScreenBlendThreshold").unwrap_or(1.0),
                    tint: [srgb_to_linear(t[0]), srgb_to_linear(t[1]), srgb_to_linear(t[2])],
                    radial_blur: ch.float("RadialBlurPercent").unwrap_or(100.0) / 100.0,
                    darkness: ch.float("OcclusionMaskDarkness").unwrap_or(0.3),
                }
            }),
        });
        let _ = ch.name("LightingChannels");
        Ok(())
    }

    fn player_start(&mut self, actor: &Obj) -> Result<()> {
        let ch = self.chain(actor);
        let loc = ch.vector("Location").unwrap_or([0.0; 3]);
        let rot = ch.rotator("Rotation").unwrap_or([0; 3]);
        let tag = ch.name("Tag").unwrap_or("").to_string();
        let index = self.scene.player_starts.len() as u32;
        self.actor_ref(actor).start = Some(index);
        self.scene.player_starts.push(PlayerStart { position: ue_point(loc), yaw: ue_yaw_to_bevy(rot[1]), tag });
        Ok(())
    }

    fn spawner(&mut self, actor: &Obj) -> Result<()> {
        let ch = self.chain(actor);
        let loc = ch.vector("Location").unwrap_or([0.0; 3]);
        let rot = ch.rotator("Rotation").unwrap_or([0; 3]);
        let pawn = ch.obj_path("m_pPawnTweaks").unwrap_or_default();
        let pawn_obj = ch.obj(self.assets, "m_pPawnTweaks");
        let spawn = ch.bool("m_bSpawnOnBeginPlay").unwrap_or(true);
        let npc_type = pawn_obj.and_then(|o| self.npc_type(&o));
        let story_group = ch.obj(self.assets, "m_pStoryGroupTweakOverride").map(|o| o.name().to_string()).unwrap_or_default();
        // (the Tower's guards are on Corvo's side: `Faction_Corvo_Default`)
        let faction = ch.obj(self.assets, "m_pFactionTweakOverride").map(|f| self.faction(&f));
        let index = self.scene.spawners.len() as u32;
        self.actor_ref(actor).spawner = Some(index);
        self.scene.spawners.push(Spawner {
            name: actor.name().to_string(),
            position: ue_point(loc),
            yaw: ue_yaw_to_bevy(rot[1]),
            pawn,
            route: None,
            spawn_on_begin_play: spawn,
            npc_type,
            script_start: false,
            story_group,
            stealable: None,
            faction,
            aware: ch.bool("m_bAwareOfPlayerUponStartup").unwrap_or(false),
            squad: match ch.get("m_Squad") {
                Some(Value::Name(n)) if n != "None" => n.clone(),
                _ => String::new(),
            },
            // (`Default__DishonoredSpawner`: able to flee; not spawning in Corvo's sight or near him)
            can_flee: ch.bool("m_bCapableOfFleeing").unwrap_or(true),
            spawn_at: ch.get("m_SpawnAtSuspicionLevel").and_then(|v| if let Value::Enum(e) = v { Some(e.clone()) } else { None }).filter(|e| !e.ends_with("None")),
            spawn_on_help: ch.bool("m_bSpawnOnHearHelpRequest").unwrap_or(false),
            spawn_visible: ch.bool("m_bCanSpawnWhenVisible").unwrap_or(false),
            spawn_near: ch.bool("m_bCanSpawnWhenPlayerNear").unwrap_or(false),
            tether: Vec::new(),
        });
        // its tether volumes (resolved once all are in)
        let tether: Vec<kismet::ActorKey> = match ch.get_pkg("m_TetherVolumes") {
            Some((pkg, Value::Array { count, offset, .. })) => {
                let mut r = Reader::at(&pkg.data, *offset);
                (0..*count).filter_map(|_| r.i32().ok()).filter(|o| *o > 0).map(|o| (pkg.name.clone(), o)).collect()
            }
            _ => Vec::new(),
        };
        self.tether_keys.push(tether);
        self.spawner_keys.push((actor.pkg.name.clone(), actor.idx));
        let steal = match ch.get_pkg("m_pStealablePickup") {
            Some((pkg, Value::Object(o))) if *o > 0 => Some((pkg.name.clone(), *o)),
            _ => None,
        };
        self.stealable_keys.push(steal);
        Ok(())
    }

    /// A security device: its power, detection volume, damage and sounds (from its tweak).
    fn rat_spawner(&mut self, actor: &Obj) -> Result<()> {
        let ch = self.chain(actor);
        let loc = ch.vector("Location").unwrap_or([0.0; 3]);
        let (mut count, mut radius) = (1u32, 0.5f32);
        if let Some(Value::Struct(_, s)) = ch.get("m_Settings") {
            let s = Props(s.clone());
            count = s.int("SpawnNum").unwrap_or(1).max(0) as u32;
            radius = s.float("SpawnRadius").unwrap_or(50.0) * UNIT;
        }
        let mut d = RatSpawner { name: actor.name().to_string(), position: ue_point(loc), count, radius, begin_play: ch.bool("m_bSpawnOnBeginPlay").unwrap_or(false), ..Default::default() };
        if let Some(t) = ch.obj(self.assets, "m_pTweaks") {
            d.tweak = t.name().to_string();
            let tch = self.chain(&t);
            if let Some(sw) = tch.obj(self.assets, "m_pSwarmTweaks") {
                let sch = self.chain(&sw);
                for (pkg, p) in sch.0.iter().rev() {
                    let _ = pkg;
                    for prop in &p.0 {
                        match &prop.value {
                            Value::Float(f) => {
                                d.params.insert(prop.name.clone(), *f);
                            }
                            Value::Int(i) => {
                                d.params.insert(prop.name.clone(), *i as f32);
                            }
                            Value::Bool(b) => {
                                d.params.insert(prop.name.clone(), if *b { 1.0 } else { 0.0 });
                            }
                            _ => {}
                        }
                    }
                }
            }
        }
        let index = self.scene.rat_spawners.len() as u32;
        self.actor_ref(actor).rat_spawner = Some(index);
        self.scene.rat_spawners.push(d);
        Ok(())
    }

    fn security(&mut self, actor: &Obj) -> Result<()> {
        let ch = self.chain(actor);
        let loc = ch.vector("Location").unwrap_or([0.0; 3]);
        let rot = ch.rotator("Rotation").unwrap_or([0; 3]);
        let kind = match actor.class().as_str() {
            "DisWallOfLight" => "WallOfLight",
            "DisDefenceTower" => "ArcPylon",
            "DisWatchTower" => "WatchTower",
            "DisAlarmBell" => "AlarmBell",
            "DisWhaleOilReceptacle" => "Receptacle",
            _ => "Battery",
        };
        let mut d = Security {
            kind: kind.into(),
            actor: actor.name().to_string(),
            position: ue_point(loc),
            yaw: ue_yaw_to_bevy(rot[1]),
            receptacle: ch.obj(self.assets, "m_pAttachedReceptacle").map(|o| o.name().to_string()).unwrap_or_default(),
            battery: ch.obj(self.assets, "m_pBattery").map(|o| o.name().to_string()).unwrap_or_default(),
            ..Default::default()
        };
        // the tweak: numbers, sounds, friendly factions
        let tweak = ["m_pWallOfLightTweaks", "m_pDefenceTowerTweaks", "m_pWatchTowerTweaks", "m_pAlarmBellTweaks", "m_pWhaleOilTweaks"].iter().find_map(|k| ch.obj(self.assets, k));
        if let Some(t) = &tweak {
            if let Ok(tp) = t.props() {
                for p in &tp.0 {
                    match &p.value {
                        Value::Float(f) => {
                            d.params.insert(p.name.clone(), *f);
                        }
                        Value::Int(i) => {
                            d.params.insert(p.name.clone(), *i as f32);
                        }
                        Value::Object(o) if *o != 0 => {
                            let path = t.pkg.obj_path(*o);
                            if t.pkg.class_name(*o) == "AkEvent" || path.contains("Snd_") {
                                d.sounds.insert(p.name.clone(), path.rsplit('.').next().unwrap_or(&path).to_string());
                            }
                        }
                        _ => {}
                    }
                }
                d.friendly = upk::props::object_array(&t.pkg, &tp, "m_FriendlyFactions").into_iter().filter(|o| *o != 0).map(|o| {
                    let p = t.pkg.obj_path(o);
                    p.rsplit('.').next().unwrap_or(&p).to_string()
                }).collect();
                d.damage = tp.int("m_FireDamage").map(|v| v as f32).unwrap_or(0.0);
            }
        }
        // a level's own friendly factions replace the tweak's
        if let Some((fpkg, Value::Array { count, offset, size })) = ch.get_pkg("m_FriendlyFactionsOverride") {
            if *count > 0 && *size >= count * 4 {
                let fpkg = fpkg.clone();
                d.friendly = (0..*count)
                    .map(|k| i32::from_le_bytes(fpkg.data[offset + k * 4..offset + k * 4 + 4].try_into().unwrap()))
                    .filter(|o| *o != 0)
                    .map(|o| {
                        let p = fpkg.obj_path(o);
                        p.rsplit('.').next().unwrap_or(&p).to_string()
                    })
                    .collect();
            }
        }
        // detection: the device's cylinder, or its eye's (walls of light)
        let cyl = ch.obj(self.assets, "m_pDetectionCylinder").and_then(|c| c.props().ok());
        if let Some(c) = &cyl {
            d.radius = c.float("CollisionRadius").unwrap_or(0.0) * UNIT;
            d.height = c.float("CollisionHeight").unwrap_or(0.0) * UNIT;
            d.detect_at = d.position;
        }
        if let Some(eye) = ch.obj(self.assets, "m_pEye") {
            let ech = self.chain(&eye);
            let eloc = ech.vector("Location").unwrap_or(loc);
            let et = ech.obj(self.assets, "m_pEyeTweaks");
            let etc = et.as_ref().map(|t| self.chain(t));
            let mut r = etc.as_ref().and_then(|t| t.float("m_fDetectionRadius")).unwrap_or(275.0);
            let mut h = etc.as_ref().and_then(|t| t.float("m_fDetectionHeight")).unwrap_or(190.0);
            let mut off = etc.as_ref().and_then(|t| t.vector("m_DetectionOffset")).unwrap_or([0.0, 0.0, -300.0]);
            // the eye's own cylinder, when it overrides its tweak's
            if ech.bool("m_bOverrideTweakDetectionParams").unwrap_or(false) {
                if let Some(c) = ech.obj(self.assets, "m_pDetectionCylinder").map(|c| self.chain(&c)) {
                    r = c.float("CollisionRadius").unwrap_or(r);
                    h = c.float("CollisionHeight").unwrap_or(h);
                    off = c.vector("Translation").unwrap_or(off);
                }
            }
            d.radius = r * UNIT;
            d.height = h * UNIT;
            d.detect_at = ue_point([eloc[0] + off[0], eloc[1] + off[1], eloc[2] + off[2]]);
            d.eye = eye.name().to_string();
            if let Some(etc) = etc {
                for (i, k) in ["m_NeutralMat", "m_ThreatMat", "m_FriendMat", "m_DisactiveMat"].iter().enumerate() {
                    if let Some(m) = etc.obj(self.assets, k) {
                        d.eye_materials[i] = self.material(&m);
                    }
                }
                for (k, key) in [("m_ThreatDetectionSound", "eye_threat"), ("m_ThreatDetectionStop", "eye_threat_stop")] {
                    if let Some(p) = etc.obj_path(k) {
                        d.sounds.insert(key.to_string(), p.rsplit('.').next().unwrap_or(&p).to_string());
                    }
                }
            }
        }
        // alarm bells: reinforcements
        if let Some((spkg, Value::Array { count, offset, size })) = ch.get_pkg("m_TriggeredSpawners") {
            if *size >= count * 4 {
                let spkg = spkg.clone();
                d.spawners = (0..*count)
                    .map(|k| i32::from_le_bytes(spkg.data[offset + k * 4..offset + k * 4 + 4].try_into().unwrap()))
                    .filter(|o| *o > 0)
                    .map(|o| spkg.obj_path(o).rsplit('.').next().unwrap_or("").to_string())
                    .collect();
            }
        }
        // watch towers: the head's skeleton and placement, its beam, its shot
        if kind == "WatchTower" {
            if let Some(comp) = ch.obj(self.assets, "m_pTowerMeshCpnt") {
                let cch = self.chain(&comp);
                if let Some(mesh) = cch.obj(self.assets, "SkeletalMesh") {
                    if let Ok(bdata) = skeletal::read_skeletal_mesh(&mesh.pkg, mesh.idx) {
                        let key = mesh.key();
                        let skeleton = match self.skeleton_ids.get(&key) {
                            Some(&i) => i,
                            None => {
                                let i = self.scene.skeletons.len() as u32;
                                self.scene.skeletons.push(SkeletonDef { name: mesh.path(), bones: skeletal::to_skeleton(&bdata), sockets: mesh_sockets(&mesh) });
                                self.skeleton_ids.insert(key, i);
                                i
                            }
                        };
                        d.skeleton = Some(skeleton);
                        let m = self.actor_matrix(actor) * self.component_local(&comp);
                        d.rig = Some(xform::ue_to_bevy(m).to_cols_array());
                    }
                }
            }
            if let Some(cone) = self.assets.find("Vfx_GamePlay.WatchTower.Regent_Light_Cone") {
                d.cone = self.cook_mesh(&cone).ok().flatten();
            }
            if let Some(f) = ch.obj(self.assets, "m_pFactionTweakOverride").or_else(|| tweak.as_ref().and_then(|t| self.chain(t).obj(self.assets, "m_pFactionTweak"))) {
                d.friendly.push(f.name().to_string());
            }
            if let Some(p) = tweak.as_ref().and_then(|t| self.chain(t).obj(self.assets, "m_pProjectileTweak")) {
                let pc = self.chain(&p);
                d.blast = pc.obj(self.assets, "m_pExplosionTweak").map(|x| self.blast(&x));
                d.trail = pc.obj(self.assets, "m_pTrailEffect").and_then(|ps| self.particle_system(&ps));
                d.params.insert("gravity".into(), pc.float("m_fGravityMultiplier").unwrap_or(1.0));
                if let Some(s) = pc.obj_path("m_pSoundEvent_InAir") {
                    d.sounds.insert("fly".into(), s.rsplit('.').next().unwrap_or(&s).to_string());
                }
            }
        }
        self.actor_ref(actor);
        self.scene.security.push(d);
        Ok(())
    }

    /// Dishonored's distance/height fog (`DisFog` + `DisFogComponent`). Maps stack several
    /// height-bounded layers; the tallest one is the global haze, which is the one we keep.
    fn fog(&mut self, actor: &Obj) -> Result<()> {
        let ch = self.chain(actor);
        let Some(comp) = ch.obj(self.assets, "Component") else { return Ok(()) };
        if let Some(mut layer) = self.fog_layer(&comp) {
            layer.enabled &= ch.bool("bEnabled").unwrap_or(true);
            let index = self.scene.fog_layers.len() as u32;
            self.scene.fog_layers.push(layer);
            self.actor_ref(actor).fog = Some(index);
        }
        let cc = self.chain(&comp);
        let height = cc.float("Height").unwrap_or(0.0);
        if self.scene.fog.is_some() && height <= self.fog_height {
            return Ok(());
        }
        self.fog_height = height;
        let c = cc.color("LightColor").unwrap_or([200, 200, 200, 255]);
        let srgb = |v: u8| {
            let v = v as f32 / 255.0;
            if v <= 0.04045 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
        };
        let near = cc.float("NearPlane").unwrap_or(4500.0) * UNIT;
        let far = cc.float("FarPlane").unwrap_or(30000.0) * UNIT;
        self.scene.fog = Some(Fog {
            color: [srgb(c[0]), srgb(c[1]), srgb(c[2])],
            start: near.max(0.0),
            end: far.max(near + 1.0),
            density: cc.float("Opacity").unwrap_or(0.5).clamp(0.0, 1.0),
        });
        Ok(())
    }

    /// Cook an NPC archetype (pawn tweak): body + head skinned meshes on a shared skeleton.
    fn npc_type(&mut self, tweak: &Obj) -> Option<u32> {
        let key = tweak.key();
        if let Some(v) = self.npc_type_ids.get(&key) {
            return *v;
        }
        let r = self.cook_npc_type(tweak);
        if let Err(e) = &r {
            log::warn!("npc type {} failed: {e:#}", tweak.path());
        }
        let id = r.ok().flatten();
        self.npc_type_ids.insert(key, id);
        id
    }

    fn cook_npc_type(&mut self, tweak: &Obj) -> Result<Option<u32>> {
        let ch = self.chain(tweak);
        let name = tweak.path();
        let lname = name.to_ascii_lowercase();
        let kind = if lname.contains("guard") || lname.contains("sniper") || lname.contains("officer") || lname.contains("executioner") {
            "guard"
        } else if lname.contains("overseer") || lname.contains("campbell") {
            // High Overseer Campbell is a mission target like any overseer
            "overseer"
        } else if lname.contains("thug") || lname.contains("gang") || lname.contains("bottle") {
            "thug"
        } else if lname.contains("tripwire") || lname.contains("launcher") {
            // a trap's mechanism
            "device"
        } else if lname.contains("rat") || lname.contains("hound") || lname.contains("fish") || lname.contains("krust") {
            "creature"
        } else if lname.contains("emily") || lname.contains("piero") || lname.contains("samuel") {
            // the allies (the Lord Regent is a target, a civilian)
            "story"
        } else {
            "civilian"
        };
        // (fish name theirs `m_pMesh`)
        // (a launcher's is its unbroken mesh's)
        let unbroken = match ch.get_pkg("m_UnbrokenMeshInfo") {
            Some((pkg, Value::Struct(_, s))) => Props(s.clone()).object("m_pMesh").filter(|o| *o != 0).and_then(|o| self.assets.resolve(pkg, o)),
            _ => None,
        };
        let Some(body) = ch
            .obj(self.assets, "m_pSkeletalMesh")
            .or_else(|| ch.obj(self.assets, "m_pMesh").filter(|m| m.class() == "SkeletalMesh"))
            .or(unbroken.filter(|m| m.class() == "SkeletalMesh"))
        else {
            return Ok(None);
        };
        // first resolvable head mesh from m_RandomHeadMeshes
        let mut head = None;
        if let Some((hpkg, Value::Array { count, offset, size })) = ch.get_pkg("m_RandomHeadMeshes") {
            let hpkg = hpkg.clone();
            if let Ok(items) = parse_struct_array(&hpkg, *offset, *size, *count) {
                'outer: for it in items {
                    for p in &it.0 {
                        if let Value::Object(o) = p.value {
                            if let Some(m) = self.assets.resolve(&hpkg, o) {
                                if m.class() == "SkeletalMesh" {
                                    head = Some(m);
                                    break 'outer;
                                }
                            }
                        }
                    }
                }
            }
        }
        // tallboys: no head, but stilts (a second mesh on the same skeleton) in its place
        if head.is_none() {
            head = ch.obj(self.assets, "m_pStiltsSkeletalMesh").filter(|m| m.class() == "SkeletalMesh");
        }
        let bdata = skeletal::read_skeletal_mesh(&body.pkg, body.idx)?;
        let skel_key = body.key();
        let skeleton = match self.skeleton_ids.get(&skel_key) {
            Some(&i) => i,
            None => {
                let i = self.scene.skeletons.len() as u32;
                self.scene.skeletons.push(SkeletonDef { name: body.path(), bones: skeletal::to_skeleton(&bdata), sockets: mesh_sockets(&body) });
                self.skeleton_ids.insert(skel_key, i);
                i
            }
        };
        let (body_id, body_mats) = self.cook_skinned(&body, &bdata, None)?;
        // the face: the head's FaceFX actor (else the body's)
        let facefx = head.iter().chain(std::iter::once(&body)).find_map(|m| self.chain(m).obj(self.assets, "FaceFXAsset")).and_then(|fx| self.facefx_actor(&fx));
        let slots = |c: &mut Self, o: &Obj, d: &skeletal::SkeletalMeshData| -> Vec<u32> {
            d.materials.iter().map(|&m| c.assets.resolve(&o.pkg, m).map(|x| c.material(&x)).unwrap_or(0)).collect()
        };
        let body_slots = slots(self, &body, &bdata);
        let (mut head_id, mut head_mats, mut head_slots) = (None, Vec::new(), Vec::new());
        if let Some(h) = head {
            if let Ok(hd) = skeletal::read_skeletal_mesh(&h.pkg, h.idx) {
                let (id, mats) = self.cook_skinned(&h, &hd, Some(&bdata))?;
                head_id = Some(id);
                head_mats = mats;
                head_slots = slots(self, &h, &hd);
            }
        }
        let mut sets = Vec::new();
        if let Some((apkg, Value::Array { count, offset, size })) = ch.get_pkg("m_AnimSets") {
            let apkg = apkg.clone();
            if *size >= *count * 4 {
                let mut r = Reader::at(&apkg.data, *offset);
                for _ in 0..*count {
                    let Ok(o) = r.i32() else { break };
                    let Some(set) = self.assets.resolve(&apkg, o) else { continue };
                    if set.class() == "AnimSet" {
                        sets.push(set);
                    }
                }
            }
        }
        // (a fish has the one, `m_pAnimSet`)
        if sets.is_empty() {
            if let Some(set) = ch.obj(self.assets, "m_pAnimSet").filter(|s| s.class() == "AnimSet") {
                sets.push(set);
            }
        }
        // the voices it may bark with
        let mut voices = Vec::new();
        if let Some((vpkg, Value::Array { count, offset, size })) = ch.get_pkg("m_GenericDialogData") {
            let vpkg = vpkg.clone();
            if *size >= *count * 4 {
                let mut r = Reader::at(&vpkg.data, *offset);
                for _ in 0..*count {
                    let Ok(o) = r.i32() else { break };
                    let Some(v) = self.assets.resolve(&vpkg, o) else { continue };
                    let key = v.key();
                    let id = match self.bark_ids.get(&key) {
                        Some(id) => *id,
                        None => {
                            let id = kismet::bark_voice(&v.pkg, v.idx).map(|b| {
                                self.scene.barks.push(b);
                                self.scene.barks.len() as u32 - 1
                            });
                            self.bark_ids.insert(key, id);
                            id
                        }
                    };
                    voices.extend(id);
                }
            }
        }
        // its faction: hostile to Corvo when his faction is among its enemies
        let (faction, hostile) = ch.obj(self.assets, "m_pFactionTweak").map(|f| self.faction(&f)).unwrap_or_default();
        let id = self.scene.npc_types.len() as u32;
        self.pending_anims.push(PendingAnims::new(id as usize, sets, &bdata));
        let anim_sets = Vec::new();
        let possess = ch.obj(self.assets, "m_pPossessableTweaks").map(|t| self.possessable(&t));
        let sight = self.sight(tweak);
        let stats = Some(self.npc_stats(tweak));
        let attachments = self.npc_attachments(&ch);
        let ragdoll = ch.obj(self.assets, "m_pPhysicsAsset").filter(|p| p.class() == "PhysicsAsset").and_then(|p| self.ragdoll(&p));
        self.scene.npc_types.push(NpcType {
            name,
            kind: kind.into(),
            skeleton: Some(skeleton),
            body: Some(body_id),
            body_materials: body_mats,
            head: head_id,
            head_materials: head_mats,
            anim_sets,
            voices,
            faction,
            hostile,
            story_group: ch.obj(self.assets, "m_pStoryGroupTweak").map(|o| o.name().to_string()).unwrap_or_default(),
            possess,
            body_slots,
            facefx,
            head_slots,
            sight,
            stats,
            attachments,
            out_of_bend: ch.bool("m_bAlwaysOutOfBendTime").unwrap_or(false),
            ragdoll,
        });
        Ok(Some(id))
    }

    /// A character's ragdoll (its physics asset), once per asset.
    fn ragdoll(&mut self, pa: &Obj) -> Option<u32> {
        let key = pa.key();
        if let Some(v) = self.ragdoll_ids.get(&key) {
            return *v;
        }
        let id = self.cook_ragdoll(pa).filter(|d| !d.bodies.is_empty()).map(|d| {
            self.scene.ragdolls.push(d);
            self.scene.ragdolls.len() as u32 - 1
        });
        self.ragdoll_ids.insert(key, id);
        id
    }

    /// A physics asset's bodies (`BodySetup`: each's boxes, spheres, capsules and convex
    /// pieces in its bone's frame) and joints (`ConstraintSetup`: the two bones' frames, the
    /// swing and twist limits), in Bevy space.
    fn cook_ragdoll(&mut self, pa: &Obj) -> Option<RagdollDef> {
        let pp = pa.props().ok()?;
        let mut def = RagdollDef { name: pa.path(), ..Default::default() };
        // an element's frame (UE3 rows: X, Y, Z axes, origin; bone space, UE units)
        let frame = |e: &Props| match e.get("TM") {
            Some(Value::Matrix(m)) => Mat4::from_cols(
                glam::Vec4::new(m[0], m[1], m[2], 0.0),
                glam::Vec4::new(m[4], m[5], m[6], 0.0),
                glam::Vec4::new(m[8], m[9], m[10], 0.0),
                glam::Vec4::new(m[12], m[13], m[14], 1.0),
            ),
            _ => Mat4::IDENTITY,
        };
        let v3 = |v: glam::Vec4| Vec3::new(v.x, v.y, v.z);
        for bi in object_array(&pa.pkg, &pp, "BodySetup") {
            let Some(body) = self.assets.resolve(&pa.pkg, bi) else { continue };
            let bch = self.chain(&body);
            let Some(bone) = bch.name("BoneName").map(str::to_string) else { continue };
            let Some((gpkg, Value::Struct(_, geom))) = bch.get_pkg("AggGeom") else { continue };
            let (gpkg, geom) = (gpkg.clone(), Props(geom.clone()));
            let elems = |name: &str| geom.array(name).and_then(|(c, off, sz)| parse_struct_array(&gpkg, off, sz, c).ok()).unwrap_or_default();
            let mut shapes = Vec::new();
            for e in elems("SphereElems") {
                let m = frame(&e);
                shapes.push(RagShape::Sphere { at: ue_point(v3(m.w_axis).to_array()), r: e.float("Radius").unwrap_or(0.0) * UNIT });
            }
            for e in elems("SphylElems") {
                // (along its Z, `Length` between the ends' centres)
                let m = frame(&e);
                let c = Vec3::from(ue_point(v3(m.w_axis).to_array()));
                let axis = Vec3::from(ue_dir(v3(m.z_axis).to_array())).normalize_or(Vec3::Y);
                let half = e.float("Length").unwrap_or(0.0) * 0.5 * UNIT;
                shapes.push(RagShape::Capsule { a: (c - axis * half).to_array(), b: (c + axis * half).to_array(), r: e.float("Radius").unwrap_or(0.0) * UNIT });
            }
            for e in elems("BoxElems") {
                // (X, Y, Z: the full sizes; UE's Y and Z axes swap places in Bevy's)
                let m = frame(&e);
                let (x, y, z) = (Vec3::from(ue_dir(v3(m.x_axis).to_array())), Vec3::from(ue_dir(v3(m.y_axis).to_array())), Vec3::from(ue_dir(v3(m.z_axis).to_array())));
                let rot = glam::Quat::from_mat3(&glam::Mat3::from_cols(x.normalize_or(Vec3::X), z.normalize_or(Vec3::Y), y.normalize_or(Vec3::Z))).normalize();
                let half = [e.float("X").unwrap_or(0.0) * 0.5 * UNIT, e.float("Z").unwrap_or(0.0) * 0.5 * UNIT, e.float("Y").unwrap_or(0.0) * 0.5 * UNIT];
                if half.iter().all(|h| *h > 0.0) {
                    shapes.push(RagShape::Box { at: ue_point(v3(m.w_axis).to_array()), rot: rot.to_array(), half });
                }
            }
            for e in elems("ConvexElems") {
                if let Some((vc, voff, _)) = e.array("VertexData") {
                    let mut r = Reader::at(&gpkg.data, voff);
                    let points: Vec<[f32; 3]> = (0..vc).filter_map(|_| r.vec3().ok()).map(ue_point).collect();
                    if points.len() >= 4 {
                        shapes.push(RagShape::Hull { points });
                    }
                }
            }
            if !shapes.is_empty() {
                def.bodies.push(RagBody { bone, shapes, mass_scale: bch.float("MassScale").unwrap_or(1.0) });
            }
        }
        for ci in object_array(&pa.pkg, &pp, "ConstraintSetup") {
            let Some(cs) = self.assets.resolve(&pa.pkg, ci) else { continue };
            let cc = self.chain(&cs);
            let (Some(child), Some(parent)) = (cc.name("ConstraintBone1").map(str::to_string), cc.name("ConstraintBone2").map(str::to_string)) else { continue };
            // a bone's frame: its position (physics scale, 1/50 of UE's units), its X the
            // primary axis, its Y the secondary (Bevy's frame right-handed: Z = X x Y)
            let bone_frame = |n: &str| -> ([f32; 3], [f32; 4]) {
                let p = Vec3::from(cc.vector(&format!("Pos{n}")).unwrap_or_default()) * 50.0;
                let x = Vec3::from(ue_dir(cc.vector(&format!("PriAxis{n}")).unwrap_or([1.0, 0.0, 0.0]))).normalize_or(Vec3::X);
                let y0 = Vec3::from(ue_dir(cc.vector(&format!("SecAxis{n}")).unwrap_or([0.0, 1.0, 0.0])));
                let z = x.cross(y0).normalize_or(x.any_orthonormal_vector());
                let y = z.cross(x);
                (ue_point(p.to_array()), glam::Quat::from_mat3(&glam::Mat3::from_cols(x, y, z)).normalize().to_array())
            };
            let swing = cc.bool("bSwingLimited").unwrap_or(false).then(|| [cc.float("Swing1LimitAngle").unwrap_or(45.0), cc.float("Swing2LimitAngle").unwrap_or(45.0)]);
            let twist = cc.bool("bTwistLimited").unwrap_or(false).then(|| cc.float("TwistLimitAngle").unwrap_or(45.0));
            def.joints.push(RagJoint { child, parent, child_frame: bone_frame("1"), parent_frame: bone_frame("2"), swing, twist });
        }
        Some(def)
    }

    /// What a character carries on its sockets (`m_pAttachmentsTweaks`): each part's mesh (as a
    /// named prop), a breakable one's health, the damage types breaking it outright or not
    /// hurting it, and its last break step (a tallboy's tank: its blast).
    fn npc_attachments(&mut self, ch: &Chain) -> Vec<NpcAttachment> {
        let Some(at) = ch.obj(self.assets, "m_pAttachmentsTweaks") else { return Vec::new() };
        let ac = self.chain(&at);
        let Some((pkg, Value::Array { count, offset, size })) = ac.get_pkg("m_Attachments").map(|(p, v)| (p.clone(), v.clone())) else { return Vec::new() };
        let mut out = Vec::new();
        for s in parse_struct_array(&pkg, offset, size, count).unwrap_or_default() {
            let socket = s.name("m_Socket").unwrap_or_default().to_string();
            let Some(tw) = s.object("m_pAttachmentTweaks").and_then(|o| self.assets.resolve(&pkg, o)) else { continue };
            let tc = self.chain(&tw);
            let Some(mesh_obj) = tc.obj(self.assets, "m_pStaticMesh").filter(|m| m.class() == "StaticMesh") else { continue };
            let prop = format!("att:{}", tw.path());
            if !self.scene.props.iter().any(|p| p.name == prop) {
                let Ok(Some((mesh, materials))) = self.cook_mesh(&mesh_obj) else { continue };
                self.scene.props.push(PropDef { name: prop.clone(), mesh, materials, blade: None });
            }
            // (damage types: class names)
            let types = |c: &Chain, name: &str| -> Vec<String> {
                c.0.iter()
                    .find(|(_, p)| p.get(name).is_some())
                    .map(|(p, props)| upk::props::object_array(p, props, name).into_iter().filter(|&o| o != 0).map(|o| p.obj_path(o).rsplit('.').next().unwrap_or("").to_string()).collect())
                    .unwrap_or_default()
            };
            out.push(NpcAttachment {
                socket,
                prop,
                health: tc.float("m_Health").unwrap_or(0.0),
                instant: types(&tc, "m_pInstantBreakDamageTypes"),
                immune: types(&tc, "m_pImmuneToDamageTypes"),
                breaks: self.last_break_step(&tc, "m_Steps"),
            });
        }
        out
    }

    /// A `DisAttribute` of a tweak (through its fallbacks): easy, normal, hard, very hard.
    fn dis_attribute(&mut self, tw: &Obj, name: &str) -> Option<[f32; 4]> {
        let Some((_, Value::Struct(_, r))) = self.tweak_prop(tw, name, 0, 0) else { return None };
        let p = Props(r);
        let mut v = [0.0; 4];
        for (i, k) in ["m_fBaseValue1_Easy", "m_fBaseValue2_Normal", "m_fBaseValue3_Hard", "m_fBaseValue4_VeryHard"].iter().enumerate() {
            v[i] = p.float(k).unwrap_or(0.0);
        }
        Some(v)
    }

    /// An array of object references of a tweak (through its fallbacks).
    fn tweak_objs(&mut self, tw: &Obj, name: &str) -> Vec<Obj> {
        let mut out = Vec::new();
        if let Some((pkg, Value::Array { count, offset, size })) = self.tweak_prop(tw, name, 0, 0) {
            if size >= count * 4 {
                let mut r = Reader::at(&pkg.data, offset);
                for _ in 0..count {
                    let Ok(o) = r.i32() else { break };
                    if let Some(p) = self.assets.resolve(&pkg, o) {
                        out.push(p);
                    }
                }
            }
        }
        out
    }

    /// A weapon's NPC contexts (`m_pNPCSpecificTweaks`: its `m_ContextSlots_Primary` and the
    /// `m_ContextSlots_Direct` ones: dodges, the kick).
    fn npc_contexts(&mut self, w: &Obj) -> Vec<Obj> {
        let Some(npc) = self.tweak_obj(w, "m_pNPCSpecificTweaks") else { return Vec::new() };
        let mut out = Vec::new();
        for slots in ["m_ContextSlots_Primary", "m_ContextSlots_Direct"] {
            if let Some((pkg, Value::Array { count, offset, size })) = self.tweak_prop(&npc, slots, 0, 0) {
                out.extend(parse_struct_array(&pkg, offset, size, count).unwrap_or_default().into_iter().filter_map(|f| f.object("m_pItemContext").filter(|o| *o != 0).and_then(|o| self.assets.resolve(&pkg, o))));
            }
        }
        out
    }

    /// A blade's moves (its NPC attack contexts, as enabled).
    fn melee_moves(&mut self, w: &Obj) -> Vec<MeleeMove> {
        let mut out = Vec::new();
        for c in self.npc_contexts(w) {
            let kind = match c.class().as_str() {
                "DisTweaks_NPCAttackShort" => "Short",
                "DisTweaks_NPCAttackMedium" => "Medium",
                "DisTweaks_NPCAttackLong" => "Long",
                "DisTweaks_NPCBash" => "Bash",
                "DisTweaks_NPCSideStep" => "SideStep",
                "DisTweaks_NPCBackStep" => "BackStep",
                "DisTweaks_NPCAttackLeft90" => "Left90",
                "DisTweaks_NPCAttackRight90" => "Right90",
                "DisTweaks_NPCAttackLeft180" => "Left180",
                "DisTweaks_NPCAttackRight180" => "Right180",
                "DisTweaks_NPCJumpAttack" => "Jump",
                "DisTweaks_NPCRiposte" => "Riposte",
                "DisTweaks_NPCPush" => "Push",
                "DisTweaks_NPCAmbush" => "Ambush",
                _ => continue,
            };
            if matches!(self.tweak_prop(&c, "m_bDisabled", 0, 0), Some((_, Value::Bool(true)))) {
                continue;
            }
            let f = |s: &mut Self, n: &str, d: f32| s.tweak_float(&c, n, 0).unwrap_or(d);
            let cooldown = match self.tweak_prop(&c, "m_ContextCooldown", 0, 0) {
                Some((_, Value::Struct(_, r))) => {
                    let p = Props(r);
                    [p.float("m_fMinValue").unwrap_or(0.0), p.float("m_fMaxValue").unwrap_or(0.0)]
                }
                _ => [0.0, 0.0],
            };
            out.push(MeleeMove {
                kind: kind.into(),
                range: [f(self, "m_fMinContextRange", 0.0) * UNIT, f(self, "m_fMaxContextRange", 250.0) * UNIT, f(self, "m_fIdealContextRange", 150.0) * UNIT],
                height: f(self, "m_fMaxContextHeightDifference", 300.0) * UNIT,
                angle: f(self, "m_fAllowedAngle", 45.0),
                cooldown,
                lead: f(self, "m_fPredictionTime", 0.0),
                big: f(self, "m_fRandomBigHitChance", 0.0),
                step: f(self, if kind == "SideStep" { "m_fSideStepDistance" } else { "m_fBackStepDistance" }, 0.0) * UNIT,
            });
        }
        out
    }

    /// A music box's tunes (its `DisTweaks_NPCTune_Combat` and `DisTweaks_NPCTune_Protection`
    /// contexts, each with its tune tweak).
    fn music_box_tunes(&mut self, w: &Obj) -> Option<NpcTunes> {
        let mut t = NpcTunes { combat_range: 20.0, combat_angle: 30.0, damage: 1.0, period: 0.5, stop_out_of_range: 1.0, stop_unseen: 5.0, disorient: 6.0, inhibit: 4.0, ..Default::default() };
        let mut found = false;
        let sound = |s: &mut Self, o: &Obj, n: &str| s.tweak_obj(o, n).map(|x| x.name().to_string()).unwrap_or_default();
        for c in self.npc_contexts(w) {
            match c.class().as_str() {
                "DisTweaks_NPCTune_Combat" => {
                    found = true;
                    t.combat_range = self.tweak_float(&c, "m_fMaxContextRange", 0).unwrap_or(2000.0) * UNIT;
                    t.combat_angle = self.tweak_float(&c, "m_fAllowedAngle", 0).unwrap_or(30.0);
                    t.damage = match self.tweak_prop(&c, "m_iMusicEffectDamage", 0, 0) {
                        Some((_, Value::Int(v))) => v as f32,
                        _ => 1.0,
                    };
                    t.repulse = self.tweak_float(&c, "m_fMusicEffectRepulseForce", 0).unwrap_or(0.0) * UNIT;
                    t.stop_out_of_range = self.tweak_float(&c, "m_fStopWhenOutOfRangeTimer", 0).unwrap_or(1.0);
                    t.stop_unseen = self.tweak_float(&c, "m_fStopWhenNotSeeingTimer", 0).unwrap_or(5.0);
                    t.damage_sounds = [sound(self, &c, "m_SFX_Damage_Start"), sound(self, &c, "m_SFX_Damage_Stop")];
                    if let Some(tune) = self.tweak_obj(&c, "m_pTuneTweak") {
                        t.period = self.tweak_float(&tune, "m_fMusicEffectPeriod", 0).unwrap_or(0.5);
                    }
                }
                "DisTweaks_NPCTune_Protection" => {
                    found = true;
                    if let Some(tune) = self.tweak_obj(&c, "m_pTuneTweak") {
                        t.disorient = self.tweak_float(&tune, "m_fMusicEffectRadius_Disorient", 0).unwrap_or(600.0) * UNIT;
                        t.inhibit = self.tweak_float(&tune, "m_fMusicEffectRadius_InhibitPowers", 0).unwrap_or(400.0) * UNIT;
                        t.protect_sounds = [
                            sound(self, &tune, "m_SFX_Disorient_Start"),
                            sound(self, &tune, "m_SFX_Disorient_Stop"),
                            sound(self, &tune, "m_SFX_Inhibit_Start"),
                            sound(self, &tune, "m_SFX_Inhibit_Stop"),
                        ];
                    }
                }
                _ => {}
            }
        }
        log::debug!("music box {}: {t:?}", w.name());
        found.then_some(t)
    }

    /// A grenade weapon's throws (its NPC contexts that throw, as enabled).
    fn grenade_throws(&mut self, w: &Obj) -> Vec<GrenadeThrow> {
        let mut out = Vec::new();
        for c in self.npc_contexts(w) {
            let kind = c.class().to_string();
            if !kind.contains("Grenade") && !kind.contains("JumpAway") || matches!(self.tweak_prop(&c, "m_bDisabled", 0, 0), Some((_, Value::Bool(true)))) {
                continue;
            }
            let f = |s: &mut Self, n: &str, d: f32| s.tweak_float(&c, n, 0).unwrap_or(d);
            let ranged = |s: &mut Self, n: &str, d: [f32; 2]| match s.tweak_prop(&c, n, 0, 0) {
                Some((_, Value::Struct(_, r))) => {
                    let p = Props(r);
                    [p.float("m_fMinValue").unwrap_or(d[0]), p.float("m_fMaxValue").unwrap_or(d[1])]
                }
                _ => d,
            };
            let cooldown = ranged(self, "m_ContextCooldown", [10.0, 15.0]);
            let mut t = GrenadeThrow {
                kind,
                range: [f(self, "m_fMinContextRange", 0.0) * UNIT, f(self, "m_fMaxContextRange", 1000.0) * UNIT, f(self, "m_fIdealContextRange", 500.0) * UNIT],
                range_unreachable: f(self, "m_fMinContextRangeWhenUnreachable", 800.0) * UNIT,
                cooldown,
                cooldown_unreachable: ranged(self, "m_ContextCooldownWhenUnreachable", cooldown),
                speed: f(self, "m_fThrowSpeed", 1500.0) * UNIT,
                jump: f(self, "m_fJumpClearance", 0.0) * UNIT,
                fuse: [3.0, 1.0],
                blast: TrapBlast { radius: 3.5, full: 1.0, player_radius: 6.0, player_full: 1.5, damage: [50.0; 4], effect: None, sound: String::new() },
            };
            // its grenade
            if let Some(proj) = self.tweak_obj(&c, "m_pProjectileType") {
                if let Some(comp) = self.tweak_obj(&proj, "m_pGrenadeComponentTweaks") {
                    t.fuse = [self.tweak_float(&comp, "m_fDetonationDelay", 0).unwrap_or(3.0), self.tweak_float(&comp, "m_fMinDetonationDelayAfterHit", 0).unwrap_or(1.0)];
                    if let Some(x) = self.tweak_obj(&comp, "m_pExplosionTweaks") {
                        t.blast = self.blast(&x);
                    }
                }
            }
            out.push(t);
        }
        out
    }

    /// What a character can take and deal: its attributes (the Normal slot's attribute
    /// tweak), its weapons and ammunition, its combat tweak's hit reactions.
    fn npc_stats(&mut self, pawn: &Obj) -> NpcStats {
        const ATTRIBUTES: [&str; 19] = [
            "HealthMax",
            "HealthRegenRate",
            "HealthRegenAmount",
            "HealthRegenLimit",
            "HealthRegenInitialDelay",
            "MinAccuracy",
            "MaxAccuracy",
            "ParryChanceOfStarting",
            "ParryChanceOfChaining",
            "ParryChainsAllowed",
            "BlockBreakRate_Min",
            "BlockBreakRate_Max",
            "DodgeLevel",
            "GroundSpeed",
            "GroundSpeedSprint",
            "MeleeDamageBonus",
            "fExplosionResilience",
            "FallingDeathHeight",
            "MaxSpeedBeforeFallingDeath",
        ];
        let mut st = NpcStats::default();
        let attr = match self.tweak_prop(pawn, "m_pAttributeTweaks", 1, 0) {
            Some((pkg, Value::Object(o))) if o != 0 => self.assets.resolve(&pkg, o),
            _ => None,
        };
        if let Some(a) = attr {
            for n in ATTRIBUTES {
                if let Some(v) = self.dis_attribute(&a, &format!("m_{n}")) {
                    st.attributes.insert(n.to_string(), v);
                }
            }
        }
        for w in self.tweak_objs(pawn, "m_ContentInventoryLoadout") {
            let mut wep = NpcWeapon { name: w.name().to_string(), class: w.class().to_string(), projectile_mult: 1.0, ..Default::default() };
            let item = match self.tweak_prop(&w, "m_pAttributeTweak", 1, 0) {
                Some((pkg, Value::Object(o))) if o != 0 => self.assets.resolve(&pkg, o),
                _ => None,
            };
            if let Some(a) = item {
                wep.melee = self.dis_attribute(&a, "m_MeleeDamage").unwrap_or_default();
                wep.ranged = self.dis_attribute(&a, "m_RangedDamage").unwrap_or_default();
            }
            let first_ammo = match self.tweak_prop(&w, "m_RangedAmmoTypes", 0, 0) {
                Some((pkg, Value::Array { count, offset, size })) if count > 0 => parse_struct_array(&pkg, offset, size, count).ok().and_then(|a| a.into_iter().next()).map(|f| (pkg, f)),
                _ => None,
            };
            if let Some((pkg, f)) = first_ammo {
                if let Some(proj) = f.object("m_pProjectileType").filter(|o| *o != 0).and_then(|o| self.assets.resolve(&pkg, o)) {
                    wep.projectile = proj.name().to_string();
                    wep.projectile_damage = self.tweak_float(&proj, "m_fDamage", 0).unwrap_or(0.0);
                    wep.projectile_mult = self.tweak_float(&proj, "m_fDamageMultiplier", 0).unwrap_or(1.0);
                }
            }
            if w.class() == "DisTweaks_WepGrenade" {
                st.grenades.extend(self.grenade_throws(&w));
            }
            if w.class() == "DisTweaks_WepMusicBox" {
                st.tunes = self.music_box_tunes(&w);
            }
            if st.melee.is_empty() && matches!(w.class().as_str(), "DisTweaks_WepSword" | "DisTweaks_WepSwordOfAssassin" | "DisTweaks_Item_WolfhoundBite") {
                st.melee = self.melee_moves(&w);
            }
            st.weapons.push(wep);
        }
        if let Some((pkg, Value::Array { count, offset, size })) = self.tweak_prop(pawn, "m_Ammo", 0, 0) {
            for e in parse_struct_array(&pkg, offset, size, count).unwrap_or_default() {
                let kind = e.name("m_AmmoType").unwrap_or_default().trim_start_matches("eDisAmmoType_").to_string();
                st.ammo.push((kind, e.int("m_AmmoCount").unwrap_or(0)));
            }
        }
        if let Some((_, Value::Str(t))) = self.tweak_prop(pawn, "m_AsleepCrosshairText", 0, 0) {
            st.asleep_text = t;
        }
        st.hit_react = [2.0, 10.0];
        if let Some(c) = self.tweak_obj(pawn, "m_pCombatTweaks") {
            for (i, n) in ["m_HitReact_Min_Damage", "m_HitReact_High_Damage"].iter().enumerate() {
                if let Some(v) = self.tweak_float(&c, n, 0) {
                    st.hit_react[i] = v;
                }
            }
            st.rat_dps = self.tweak_float(&c, "m_fRatSwarmDamagePerSecond", 0).unwrap_or(0.0);
        }
        log::debug!(
            "npc stats {}: health {:?}, weapons {:?}, ammo {:?}",
            pawn.name(),
            st.attributes.get("HealthMax"),
            st.weapons.iter().map(|w| format!("{} melee {:?} ranged {:?} {} {}x{}", w.name, w.melee, w.ranged, w.projectile, w.projectile_damage, w.projectile_mult)).collect::<Vec<_>>(),
            st.ammo
        );
        if !st.melee.is_empty() {
            log::debug!("npc melee {}: {:?}", pawn.name(), st.melee.iter().map(|m| (&m.kind, m.range, m.angle, m.cooldown, m.lead, m.big)).collect::<Vec<_>>());
        }
        if !st.grenades.is_empty() {
            log::debug!("npc grenades {}: {:?}", pawn.name(), st.grenades.iter().map(|g| (&g.kind, g.range, g.cooldown, g.speed, g.fuse, g.blast.radius, g.blast.damage[1])).collect::<Vec<_>>());
        }
        st
    }

    /// An object-valued tweak property (through the tweak's fallbacks).
    fn tweak_obj(&mut self, tw: &Obj, name: &str) -> Option<Obj> {
        match self.tweak_prop(tw, name, 0, 0)? {
            (pkg, Value::Object(o)) if o != 0 => self.assets.resolve(&pkg, o),
            _ => None,
        }
    }

    fn tweak_float(&mut self, tw: &Obj, name: &str, idx: i32) -> Option<f32> {
        match self.tweak_prop(tw, name, idx, 0)? {
            (_, Value::Float(f)) => Some(f),
            (_, Value::Int(i)) => Some(i as f32),
            _ => None,
        }
    }

    /// A `DisVisCalcTuningParams` (fields it leaves out are the class default's: multiply by
    /// x).
    fn tuning(&mut self, tw: &Obj, name: &str) -> Tuning {
        let mut t = Tuning { add: false, scalar: 1.0, bias: 0.0, exponent: 1.0, points: Vec::new() };
        let Some((pkg, Value::Struct(_, r))) = self.tweak_prop(tw, name, 0, 0) else { return t };
        let p = Props(r);
        if let Some(op) = p.name("m_ApplyOp") {
            t.add = op == "DVCTPAO_Add";
        }
        t.scalar = p.float("m_fScalar").unwrap_or(1.0);
        t.bias = p.float("m_fBias").unwrap_or(0.0);
        t.exponent = p.float("m_fExponent").unwrap_or(1.0);
        if p.bool("m_bUsePointsInsteadOfFormula") == Some(true) {
            if let Some((c, off, sz)) = p.array("m_Points") {
                for e in parse_struct_array(&pkg, off, sz, c).unwrap_or_default() {
                    t.points.push([e.float("m_fInputValue").unwrap_or(0.0), e.float("m_fOutputValue").unwrap_or(0.0)]);
                }
            }
        }
        t
    }

    /// How a character sees and notices Corvo: its pawn's vision tweak, and its brain's
    /// attention process's attention tweaks.
    fn sight(&mut self, pawn: &Obj) -> Option<Sight> {
        let vision = self.tweak_obj(pawn, "m_pVisionTweak")?;
        let v = |s: &mut Self, n: &str, d: f32| s.tweak_float(&vision, n, 0).unwrap_or(d);
        let inner = [v(self, "m_fVision_InnerFOV_Deg", 45.0), v(self, "m_fVision_InnerMaxRange", 3500.0) * UNIT, v(self, "m_fVision_InnerVerticalFOVScale_Up", 0.25), v(self, "m_fVision_InnerVerticalFOVScale_Down", 0.7)];
        let outer = [v(self, "m_fVision_OuterFOV_Deg", 160.0), v(self, "m_fVision_OuterMaxRange", 1700.0) * UNIT, v(self, "m_fVision_OuterVerticalFOVScale_Up", 0.1), v(self, "m_fVision_OuterVerticalFOVScale_Down", 0.7)];
        let brain = self.tweak_obj(pawn, "m_pBrainTweak")?;
        let aware_level = match self.tweak_prop(&brain, "m_MinAttentionForAwareness", 0, 0) {
            Some((_, Value::Enum(e))) => attention_level(&e),
            _ => 3,
        };
        // the brain's processes: the attention one
        let mut process = None;
        if let Some((pkg, Value::Array { count, offset, size })) = self.tweak_prop(&brain, "m_BrainProcessTweaks", 0, 0) {
            if size >= count * 4 {
                let mut r = Reader::at(&pkg.data, offset);
                for _ in 0..count {
                    let Ok(o) = r.i32() else { break };
                    if let Some(p) = self.assets.resolve(&pkg, o) {
                        if p.class() == "DisTweaks_AIBrainProcess_Attention" {
                            process = Some(p);
                            break;
                        }
                    }
                }
            }
        }
        let process = process?;
        let attention = |s: &mut Self, n: &str| s.tweak_obj(&process, n).map(|t| s.attention(&t)).unwrap_or_default();
        let unsuspecting = attention(self, "m_pAttentionTweaks_Enemy_Unsuspecting");
        let suspecting = attention(self, "m_pAttentionTweaks_Enemy_Suspecting");
        let neutral = attention(self, "m_pAttentionTweaks_Neutral");
        let protect_neutrals = matches!(self.tweak_prop(&brain, "m_bProtectNeutrals", 0, 0), Some((_, Value::Bool(true))));
        Some(Sight { inner, outer, unsuspecting, suspecting, neutral, aware_level, protect_neutrals })
    }

    /// A `DisTweaks_PawnAttention`.
    fn attention(&mut self, tw: &Obj) -> Attention {
        let mut a = Attention { leak: self.tweak_float(tw, "m_fAttentionLeakRate", 0).unwrap_or(0.1), max: self.tweak_float(tw, "m_fMaxAttention", 0).unwrap_or(1.0), ..Default::default() };
        for k in 0..4 {
            if let Some((_, Value::Struct(_, r))) = self.tweak_prop(tw, "m_AttentionThresholds", k as i32, 0) {
                let p = Props(r);
                a.levels[k] = [p.float("m_fAttentionLevelToStart").unwrap_or(0.0), p.float("m_fAttentionLevelToStop").unwrap_or(0.0)];
            }
        }
        a.see = [1.0; 4];
        if let Some((_, Value::Struct(_, r))) = self.tweak_prop(tw, "m_SeeingPawnAttentionIncreaseScalar_ToPlayer", 0, 0) {
            let p = Props(r);
            for (i, n) in ["m_fBaseValue1_Easy", "m_fBaseValue2_Normal", "m_fBaseValue3_Hard", "m_fBaseValue4_VeryHard"].iter().enumerate() {
                a.see[i] = p.float(n).unwrap_or(1.0);
            }
        }
        a.touch = self.tweak_float(tw, "m_fTouchingPawnAttentionIncreaseRate", 0).unwrap_or(0.0);
        a.distance = self.tuning(tw, "m_DistanceValueTuning");
        let increase = |s: &mut Self, n: &str| match s.tweak_prop(tw, n, 0, 0) {
            Some((_, Value::Struct(_, r))) => {
                let p = Props(r);
                (p.float("m_fByAmount").unwrap_or(0.0), p.name("m_ToThreshold").map(attention_increase_level).unwrap_or(0))
            }
            _ => (0.0, 0),
        };
        a.heard = [
            increase(self, "m_IncreaseForAnomalySound"),
            increase(self, "m_IncreaseForThreateningSound"),
            increase(self, "m_IncreaseForDangerSound"),
            increase(self, "m_IncreaseForDeathRattleSound"),
            increase(self, "m_IncreaseForHelpSound"),
            increase(self, "m_IncreaseForAlarmSound"),
        ];
        a.corpse = increase(self, "m_IncreaseForDiscoveredCorpse");
        a.attacked = increase(self, "m_IncreaseForAttackedBy");
        a
    }

    /// How visible Corvo is: `Twk_PlayerVisibility`, and the light range of everywhere and of
    /// this map.
    fn player_vis(&mut self, levels: &[Arc<upk::Package>]) -> Option<PlayerVis> {
        let tw = self.assets.find("Twk_PlayerVisibility.Twk_PlayerVisibility")?;
        let light = self.tuning(&tw, "m_LightValueTuning");
        let speed = self.tuning(&tw, "m_SpeedValueTuning");
        let sneak_still = matches!(self.tweak_prop(&tw, "m_bForceZeroSpeedWhileSneaking", 0, 0), Some((_, Value::Bool(true))));
        let mut bonus = [0.0; 8];
        for (i, n) in ["Standing", "Walking", "Sprinting", "Sneaking", "Sliding", "Autocrouching", "Swimming", "SwimmingOnSurface"].iter().enumerate() {
            bonus[i] = self.tweak_float(&tw, &format!("m_fVisLevelBonus_{n}"), 0).unwrap_or(0.0);
        }
        let global = self.tweak_obj(&tw, "m_pGlobalPlayerVisSettings").map(|g| self.vis_range(&g)).unwrap_or([0.0, 1.0]);
        let map = levels.iter().find_map(|pkg| {
            let info = pkg.exports_of_class("DishonoredMapInfo").next()?;
            let o = upk::read_object(pkg, info).ok()?.props.object("m_pMapPlayerVisSettings").filter(|o| *o != 0)?;
            self.assets.resolve(pkg, o)
        });
        let map = map.map(|m| self.vis_range(&m));
        Some(PlayerVis { light, speed, sneak_still, bonus, global, map })
    }

    /// Corvo's health effects and water-exit lens effect (`Twk_Pawn_Corvo_Release`): the lens
    /// effects' particle systems, the barks by his `m_BarkCues`.
    fn health_effects(&mut self) {
        let Some(pawn) = self.assets.find("Twk_Pawn_Corvo.Twk_Pawn_Corvo_Release") else { return };
        // a lens effect tweak's particle system and life
        let lens = |s: &mut Self, tw: &Obj| -> Option<(u32, f32)> {
            let ps = s.tweak_obj(tw, "m_DefaultParticleSystem")?;
            let life = s.tweak_float(tw, "m_fLifeSpan", 0).unwrap_or(0.0);
            s.particle_system(&ps).map(|p| (p, life))
        };
        if let Some(w) = self.tweak_obj(&pawn, "m_pWaterExitEffectTweaks") {
            self.scene.water_exit_lens = lens(self, &w);
        }
        // the bark cues: by bark type, the sound event
        let mut cues: Vec<String> = Vec::new();
        for k in 0..16 {
            let ev = match self.tweak_prop(&pawn, "m_BarkCues", k, 0) {
                Some((pkg, Value::Struct(_, r))) => Props(r).object("m_pSoundEvent").filter(|o| *o != 0).map(|o| {
                    let p = pkg.obj_path(o);
                    p.rsplit('.').next().unwrap_or(&p).to_string()
                }),
                _ => None,
            };
            cues.push(ev.unwrap_or_default());
        }
        let bark_types = ["EDisPlayerBarkType_None", "EDisPlayerBarkType_WeakHit", "EDisPlayerBarkType_StrongHit", "EDisPlayerBarkType_NearDeath", "EDisPlayerBarkType_NearDeath2", "EDisPlayerBarkType_Death", "EDisPlayerBarkType_NearDeath2_Stinger", "EDisPlayerBarkType_NearDeath2_Regen", "EDisPlayerBarkType_Death_Stinger"];
        let Some(combat) = self.tweak_obj(&pawn, "m_pCombatTweaks") else { return };
        let Some((pkg, Value::Array { count, offset, size })) = self.tweak_prop(&combat, "m_HealthEffects", 0, 0) else { return };
        for e in parse_struct_array(&pkg, offset, size, count).unwrap_or_default() {
            let barks = |name: &str| -> Vec<String> {
                let Some((c, o, sz)) = e.array(name) else { return Vec::new() };
                parse_struct_array(&pkg, o, sz, c)
                    .unwrap_or_default()
                    .iter()
                    .filter_map(|b| b.name("m_BarkType").and_then(|t| bark_types.iter().position(|x| *x == t)))
                    .filter_map(|i| cues.get(i).filter(|s| !s.is_empty()).cloned())
                    .collect()
            };
            let lens_fx = e.object("m_pLensEffect").filter(|o| *o != 0).and_then(|o| self.assets.resolve(&pkg, o)).and_then(|t| lens(self, &t)).map(|l| l.0);
            self.scene.health_fx.push(HealthFx {
                name: e.name("m_Name").unwrap_or_default().to_string(),
                threshold: e.float("m_fHealthThreshold").unwrap_or(0.0),
                blend_in: e.float("m_fBlendInTime").unwrap_or(0.0),
                blend_out: e.float("m_fBlendOutTime").unwrap_or(0.0),
                lens: lens_fx,
                on: barks("m_Bark_OnEnable"),
                off: barks("m_Bark_OnDisable"),
            });
        }
    }

    /// A `DisTweaks_PlayerVisSettings`: the light values counted as darkest and brightest.
    fn vis_range(&mut self, tw: &Obj) -> [f32; 2] {
        [self.tweak_float(tw, "m_fLightValueDarkestVal", 0).unwrap_or(0.0), self.tweak_float(tw, "m_fLightValueBrightestVal", 0).unwrap_or(1.0)]
    }

    /// A distractor (`DisNPCDistractor`): where, its component's range, chances, routes,
    /// cooldown and its scene (cooked once the level scripts are, into their matinees).
    fn distractor(&mut self, actor: &Obj) -> Result<()> {
        let ch = self.chain(actor);
        let Some(comp) = ch.obj(self.assets, "m_pDistraction") else { return Ok(()) };
        let cc = self.chain(&comp);
        if cc.bool("m_bDistractionEnabled") == Some(false) {
            return Ok(());
        }
        let chance = |name: &str| match cc.get(name) {
            Some(Value::Struct(_, p)) => Props(p.clone()).float("m_fChanceToAnimate").unwrap_or(0.0),
            _ => 0.0,
        };
        let mut d = Distractor {
            name: actor.name().to_string(),
            position: ue_point(ch.vector("Location").unwrap_or([0.0; 3])),
            yaw: ue_yaw_to_bevy(ch.rotator("Rotation").unwrap_or([0; 3])[1]),
            range: cc.float("m_fDistractionRange").unwrap_or(1000.0) * UNIT,
            los: cc.bool("m_bRequireLOS").unwrap_or(true),
            cooldown: cc.float("m_fCooldownTime").unwrap_or(45.0),
            chance: chance("m_SuspicionLvl1_Chances"),
            chance_wary: chance("m_SuspicionLvl234_Chances"),
            ..Default::default()
        };
        // the routes it draws from
        if let Some((rpkg, Value::Array { count, offset, .. })) = cc.get_pkg("m_AllowedRoutes") {
            let mut r = Reader::at(&rpkg.data, *offset);
            for _ in 0..*count {
                let Ok(o) = r.i32() else { break };
                if let Some(ro) = self.assets.resolve(rpkg, o) {
                    d.routes.push(ro.name().to_string());
                }
            }
        }
        // its scene's groups placed on the level's actors, and the sections' repeats
        if let Some((spkg, Value::Array { count, offset, size })) = cc.get_pkg("m_lDistractionSoireeActor") {
            let spkg = spkg.clone();
            for e in parse_struct_array(&spkg, *offset, *size, *count).unwrap_or_default() {
                let group = e.name("m_SoireeGroupName").unwrap_or_default().to_string();
                let at = e.object("m_GroupActor").filter(|o| *o != 0).and_then(|o| self.assets.resolve(&spkg, o)).and_then(|o| self.chain(&o).vector("Location"));
                if let Some(at) = at {
                    d.marks.push((group, ue_point(at)));
                }
            }
        }
        if let Some((lpkg, Value::Array { count, offset, size })) = cc.get_pkg("m_lDistractionSoireeLoops") {
            for e in parse_struct_array(lpkg, *offset, *size, *count).unwrap_or_default() {
                let min = e.int("m_iLoopMin").unwrap_or(1).max(0) as u32;
                d.loops.push((e.name("m_SoireeLoopName").unwrap_or("Loop").to_string(), min, (e.int("m_iLoopMax").unwrap_or(min as i32).max(0) as u32).max(min)));
            }
        }
        // without a scene: its animation and time
        d.anim = cc.name("m_DistractionAnimPrefix").unwrap_or("Distraction_").to_string() + cc.name("m_DistractionCategory").filter(|c| *c != "None").unwrap_or("");
        if let Some(Value::Struct(_, t)) = cc.get("m_DistractionTime") {
            let t = Props(t.clone());
            if let Some(dist) = t.object("Distribution").filter(|o| *o != 0).and_then(|o| self.assets.resolve(&comp.pkg, o)) {
                let dc = self.chain(&dist);
                d.time = [dc.float("Min").unwrap_or(0.0), dc.float("Max").unwrap_or(0.0)];
            }
        }
        let index = self.scene.distractors.len();
        if cc.bool("m_bUseSoireeInsteadAnim").unwrap_or(false) {
            if let Some(s) = cc.obj(self.assets, "m_pDistractionMatineeData") {
                self.pending_soirees.push((index, s));
            }
        }
        self.scene.distractors.push(d);
        Ok(())
    }

    /// A `DisTweaks_Faction`: its name, and whether Corvo is among its enemies (the enemies
    /// kept in `scene.factions`, for the feuds between factions).
    fn faction(&mut self, f: &Obj) -> (String, bool) {
        let name = f.name().to_string();
        let Ok(fp) = f.props() else { return (name, false) };
        let enemies: Vec<String> = upk::props::object_array(&f.pkg, &fp, "m_EnemyFactions")
            .into_iter()
            .filter(|o| *o != 0)
            .map(|o| {
                let p = f.pkg.obj_path(o);
                p.rsplit('.').next().unwrap_or(&p).to_string()
            })
            .collect();
        let hostile = enemies.iter().any(|e| e.to_ascii_lowercase().contains("faction_corvo"));
        self.scene.factions.insert(name.clone(), enemies);
        (name, hostile)
    }

    /// A `DisTweaks_Possessable`: the body Corvo takes on.
    fn possessable(&mut self, tweak: &Obj) -> Possessable {
        let c = self.chain(tweak);
        let mut duration = [0.0; 2];
        for (i, d) in duration.iter_mut().enumerate() {
            if let Some((_, Value::Struct(_, s))) = c.get_idx_pkg("m_Levels", i as i32) {
                *d = Props(s.clone()).float("m_fPossessDuration").unwrap_or(0.0);
            }
        }
        let level = match c.get("m_RequiredPossessPowerLevel") {
            Some(Value::Int(v)) => *v as u8,
            _ => 1,
        };
        Possessable {
            height: c.float("m_fCollisionHeight").unwrap_or(0.0) * UNIT,
            radius: c.float("m_fCollisionRadius").unwrap_or(0.0) * UNIT,
            ground_speed: c.float("m_fGroundSpeedWhilePossessed").unwrap_or(300.0) * UNIT,
            water_speed: c.float("m_fWaterSpeedWhilePossessed").unwrap_or(300.0) * UNIT,
            swim: c.bool("m_bCanSwimWhilePossessed").unwrap_or(false),
            duration,
            level,
        }
    }

    /// Bind the characters' AnimSets (`anims/*.anim`), now that every mesh is known.
    fn cook_anims(&mut self) {
        let pending = std::mem::take(&mut self.pending_anims);
        let refs: Vec<&[String]> = pending.iter().map(|p| p.bones.as_slice()).collect();
        for p in &pending {
            let ids: Vec<u32> = p.sets.iter().filter_map(|set| self.anim_set(set, p, &refs)).collect();
            if let Some(t) = self.scene.npc_types.get_mut(p.npc_type) {
                t.anim_sets = ids;
            }
        }
    }

    /// Cook an AnimSet for a character. Characters with the same bone layout share the file.
    fn anim_set(&mut self, set: &Obj, p: &PendingAnims, refs: &[&[String]]) -> Option<u32> {
        let layout = match anim_layout(set, p, refs) {
            Ok(l) => l,
            Err(e) => {
                log::warn!("anim set {}: {e:#}", set.path());
                return None;
            }
        };
        let key = format!("{}@{:08x}", set.key(), layout.hash);
        if let Some(v) = self.anim_set_ids.get(&key) {
            return *v;
        }
        self.npc_severs(set);
        self.clip_marks(set);
        let file = format!("anims/{}.anim", sanitize(&key));
        let have = !self.opts.force && self.root.join(&file).exists();
        let r = if have { Ok(true) } else { cook_anim_set(set, &layout, &self.root.join(&file)) };
        let id = match r {
            Ok(true) => {
                self.scene.anim_sets.push(AnimSetRef { name: set.path(), file });
                Some(self.scene.anim_sets.len() as u32 - 1)
            }
            Ok(false) => None,
            Err(e) => {
                log::warn!("anim set {}: {e:#}", set.path());
                None
            }
        };
        self.anim_set_ids.insert(key, id);
        id
    }

    /// A skinned mesh, on its own skeleton or worn on a body's (a head: `body`).
    fn cook_skinned(&mut self, obj: &Obj, d: &skeletal::SkeletalMeshData, body: Option<&skeletal::SkeletalMeshData>) -> Result<(u32, Vec<u32>)> {
        // (a head is fitted to each body it is worn on)
        let key = match body {
            Some(b) => format!("skin:{}@{:08x}", obj.key(), skeleton_hash(b)),
            None => format!("skin:{}", obj.key()),
        };
        if let Some(&id) = self.mesh_ids.get(&key) {
            return Ok((id, self.mesh_materials_cache(&key)));
        }
        let mats: Vec<u32> = d
            .materials
            .iter()
            .map(|&m| match self.assets.resolve(&obj.pkg, m) {
                Some(o) => self.material(&o),
                None => 0,
            })
            .collect();
        let section_mats: Vec<u32> = d.sections.iter().map(|s| *mats.get(s.material as usize).unwrap_or(&0)).collect();
        let file = format!("meshes/{}.mesh", sanitize(&key));
        let mf = match body {
            Some(b) => skeletal::to_mesh_file_worn(d, b),
            None => skeletal::to_mesh_file_skinned(d, None),
        };
        let (min, max) = bounds(&mf.positions);
        if self.opts.force || !self.root.join(&file).exists() {
            mf.write(&self.root.join(&file))?;
        }
        let id = self.scene.meshes.len() as u32;
        self.scene.meshes.push(MeshRef { name: obj.path(), file, min, max, simple: Vec::new(), radius: d.radius * UNIT, ..Default::default() });
        self.mesh_ids.insert(key.clone(), id);
        self.mesh_section_mats.insert(key.clone(), section_mats.clone());
        // its lesser LODs (each its sections' materials through `LODMaterialMap`), the last of
        // them the dismemberment one where its sections are the mesh's body parts
        for (i, lod) in d.lods.iter().enumerate() {
            let n = i + 1;
            let lmats: Vec<u32> = lod.sections.iter().map(|s| {
                let m = d.lod_material_maps.get(n).and_then(|map| map.get(s.material as usize)).map(|&m| m as usize).unwrap_or(s.material as usize);
                *mats.get(m).unwrap_or(&0)
            }).collect();
            let lfile = format!("meshes/{}_lod{n}.mesh", sanitize(&key));
            let lmf = match body {
                Some(b) => skeletal::to_mesh_file_worn(lod, b),
                None => skeletal::to_mesh_file_skinned(lod, None),
            };
            let (lmin, lmax) = bounds(&lmf.positions);
            if self.opts.force || !self.root.join(&lfile).exists() {
                lmf.write(&self.root.join(&lfile))?;
            }
            let lid = self.scene.meshes.len() as u32;
            self.scene.meshes.push(MeshRef { name: format!("{} LOD{n}", obj.path()), file: lfile, min: lmin, max: lmax, ..Default::default() });
            let gore = n == d.lods.len() && !d.body_parts.is_empty() && d.body_parts.len() == lod.sections.len();
            let m = &mut self.scene.meshes[id as usize];
            if gore {
                m.gore = Some(GoreLod { mesh: lid, materials: lmats, parts: d.body_parts.clone() });
            } else {
                m.lods.push(MeshLod { mesh: lid, materials: lmats, display_factor: d.lod_factors.get(n).copied().unwrap_or(0.0) });
            }
        }
        Ok((id, section_mats))
    }

    fn route(&mut self, actor: &Obj) -> Result<()> {
        let ch = self.chain(actor);
        let kind = ch.name("RouteType").unwrap_or("ERT_Linear").to_string();
        let mut points = Vec::new();
        if let Some((rpkg, Value::Array { count, offset, size })) = ch.get_pkg("RouteList") {
            let rpkg = rpkg.clone();
            let elem = if *count > 0 { size / count } else { 0 };
            for i in 0..*count {
                let mut r = Reader::at(&rpkg.data, offset + i * elem);
                let a = r.i32()?;
                if a > 0 && (a as usize) <= rpkg.exports.len() {
                    let o = Obj { pkg: rpkg.clone(), idx: a };
                    let pch = self.chain(&o);
                    if let Some(l) = pch.vector("Location") {
                        points.push(ue_point(l));
                    }
                }
            }
        }
        if !points.is_empty() {
            self.scene.routes.push(Route { name: actor.name().to_string(), kind, points });
        }
        Ok(())
    }

    /// The pickups the scripts' actor factories make (`SeqAct_ActorFactory` with a
    /// `DisActorFactoryTweakObj`: runes, bone charms, coins, elixirs, keys, whale oil tanks):
    /// each a pickup of the scene with a hidden instance of its mesh, both waiting for the
    /// script; the op names the pickup (`factory_pickup`).
    fn factory_pickups(&mut self) {
        for i in 0..self.scene.kismet.ops.len() {
            // (and what the scripts put in someone's pocket: `DisSeqAct_SpawnStealable`)
            let stealable = self.scene.kismet.ops[i].class == "DisSeqAct_SpawnStealable";
            if !matches!(self.scene.kismet.ops[i].class.as_str(), "SeqAct_ActorFactory" | "SeqAct_ActorFactoryEx") && !stealable {
                continue;
            }
            let tw = if stealable {
                let Some(KVal::Str(path)) = self.scene.kismet.ops[i].props.get("m_pStealableTweaks").cloned() else { continue };
                let Some(tw) = self.assets.find(&path) else { continue };
                tw
            } else {
                let Some(KVal::Str(path)) = self.scene.kismet.ops[i].props.get("Factory").cloned() else { continue };
                // a sound it makes (`ActorFactoryAkAmbientSound`: a PA speaker's hum), its
                // event onto the op
                if let Some(fac) = self.assets.find(&path).filter(|f| f.class().starts_with("ActorFactoryAkAmbientSound")) {
                    if let Some(ev) = self.chain(&fac).obj_path("AmbientEvent") {
                        let ev = ev.rsplit('.').next().unwrap_or(&ev).to_string();
                        self.scene.kismet.ops[i].props.insert("ambient_event".into(), KVal::Str(ev));
                    }
                    continue;
                }
                let Some(fac) = self.assets.find(&path).filter(|f| f.class() == "DisActorFactoryTweakObj") else { continue };
                let Some(tw) = self.chain(&fac).obj(self.assets, "m_pTweakObject") else { continue };
                tw
            };
            let key = match self.scene.kismet.ops[i].props.get("m_KeyName") {
                Some(KVal::Str(k)) => k.clone(),
                _ => String::new(),
            };
            // (things to pick up: not the movables and whale oil tanks, which are carried)
            let cls = tw.class();
            if cls.contains("Movable") || cls.contains("Battery") || tw.path().contains("Battery") || !(cls.contains("Pickup") || cls.contains("Rune") || cls.contains("Key") || cls.contains("Elixir") || cls.contains("Charm") || cls.contains("Item")) {
                continue;
            }
            let tc = self.chain(&tw);
            let Some(mesh_obj) = ["m_pPickupStaticMesh", "m_pStaticMesh", "m_pMesh"].iter().find_map(|n| tc.obj(self.assets, n)).filter(|m| m.class() == "StaticMesh") else { continue };
            let Ok(Some((mesh, materials))) = self.cook_mesh(&mesh_obj) else { continue };
            let mut ammo = Vec::new();
            let mut ammo_min = Vec::new();
            for k in 0..8 {
                if let Some((_, Value::Struct(_, r))) = tc.get_idx_pkg("m_AmmoRanges", k) {
                    let r = Props(r.clone());
                    let n = r.int("m_MaxValue").or(r.int("m_MinValue")).unwrap_or(0);
                    if n > 0 {
                        ammo.push((k as u8, n as u32));
                        let min = r.int("m_MinValue").unwrap_or(n).clamp(0, n) as u32;
                        if min < n as u32 { ammo_min.push((k as u8, min)); }
                    }
                }
            }
            let quantity = match tc.get("m_Quantity") {
                Some(Value::Int(q)) => (*q).max(0) as u32,
                _ => 0,
            };
            let item_path = tc.obj_path("m_pAbstractItem").unwrap_or_default();
            let coins = item_path.ends_with("Coins_AbsItm");
            let item = item_path.split_once('.').map(|x| x.1.to_string()).unwrap_or(item_path);
            let label = tc.obj(self.assets, "m_pInteractableTweaks").map(|it| self.chain(&it).name("m_Name").unwrap_or_default().to_string()).unwrap_or_default();
            let name = format!("Factory_{i}");
            let instance = self.scene.instances.len() as u32;
            self.scene.instances.push(Instance {
                mesh,
                materials,
                transform: Mat4::IDENTITY.to_cols_array(),
                visible: false,
                collide: false,
                cast_shadow: true,
                dynamic: true,
                actor: name.clone(),
                class: "DisFactoryItem".into(),
                lightmap: None,
                sun: 2,
                sun_shadow: None,
                light_shadows: Vec::new(),
                irrelevant_lights: Vec::new(),
                reflect: 0,
            });
            let pickup = self.scene.pickups.len() as u32;
            self.scene.pickups.push(Pickup {
                name,
                class: tw.class(),
                kind: tw.path(),
                position: [0.0; 3],
                instance: Some(instance),
                item,
                ammo,
                ammo_min,
                food_health: if tc.name("m_Type") == Some("eStatPickupType_Food") {
                    match tc.get("m_HealthChange") { Some(Value::Int(n)) => Some((*n).max(0) as u32), _ => None }
                } else { None },
                label,
                quantity,
                coins,
                factory: true,
                key,
                inv_item: tc.obj_path("m_pItem").map(|p| p.rsplit('.').next().unwrap_or(&p).to_string()).unwrap_or_default(),
            });
            self.scene.kismet.ops[i].props.insert("factory_pickup".into(), KVal::Int(pickup as i32));
        }
    }

    /// What the scripts hand Corvo (`DisSeqAct_GivePickup.m_pPickup`): its ammunition, how
    /// many, whether it's money, its name and abstract item, onto the op.
    /// The materials the scripts put on things (`SeqAct_SetMaterial.NewMaterial`), cooked and
    /// noted on the op (`material_id`).
    fn swap_materials(&mut self) {
        for i in 0..self.scene.kismet.ops.len() {
            if self.scene.kismet.ops[i].class != "SeqAct_SetMaterial" {
                continue;
            }
            let Some(KVal::Str(path)) = self.scene.kismet.ops[i].props.get("NewMaterial").cloned() else { continue };
            let Some(obj) = self.assets.find(&path) else {
                log::debug!("SetMaterial: {path} not found");
                continue;
            };
            let id = self.material(&obj);
            self.scene.kismet.ops[i].props.insert("material_id".into(), KVal::Int(id as i32));
        }
    }

    /// `DisSeqAct_SpawnCameraLensEffect`: the effect tweak's particle system (rain on the lens,
    /// the Hound Pits sickness), whether it loops until stopped, and its life span.
    fn lens_effects(&mut self) {
        for i in 0..self.scene.kismet.ops.len() {
            if self.scene.kismet.ops[i].class != "DisSeqAct_SpawnCameraLensEffect" {
                continue;
            }
            let Some(KVal::Str(path)) = self.scene.kismet.ops[i].props.get("m_pDisEffectTweaks").cloned() else { continue };
            let Some(tw) = self.assets.find(&path) else {
                log::debug!("lens effect {path} not found");
                continue;
            };
            let looping = tw.pkg.class_name(tw.idx).contains("Looping");
            let tch = self.chain(&tw);
            let life = tch.float("m_fLifeSpan").unwrap_or(0.0);
            let Some(ps) = tch.obj(self.assets, "m_DefaultParticleSystem") else { continue };
            let Some(id) = self.particle_system(&ps) else { continue };
            let props = &mut self.scene.kismet.ops[i].props;
            props.insert("lens_system".into(), KVal::Int(id as i32));
            props.insert("lens_looping".into(), KVal::Bool(looping));
            props.insert("lens_life".into(), KVal::Float(life));
        }
    }

    /// `SeqAct_ControlGameMovie`: the movie's length, from its Bink header (frames x the
    /// frame rate's divider / dividend), cooked onto the op.
    fn movie_lengths(&mut self) {
        let movies = self.assets.cooked_dir.parent().map(|d| d.join("Movies"));
        for i in 0..self.scene.kismet.ops.len() {
            if self.scene.kismet.ops[i].class != "SeqAct_ControlGameMovie" {
                continue;
            }
            let Some(KVal::Str(name)) = self.scene.kismet.ops[i].props.get("MovieName").cloned() else { continue };
            let Some(path) = movies.as_ref().map(|d| d.join(format!("{name}.bik"))) else { continue };
            let Ok(head) = std::fs::read(&path).map(|d| d.get(..44).map(|h| h.to_vec()).unwrap_or_default()) else { continue };
            if head.len() < 44 || &head[..3] != b"BIK" {
                continue;
            }
            let u = |o: usize| u32::from_le_bytes([head[o], head[o + 1], head[o + 2], head[o + 3]]) as f32;
            let (frames, num, den) = (u(8), u(28), u(32));
            if num > 0.0 {
                self.scene.kismet.ops[i].props.insert("movie_secs".into(), KVal::Float(frames * den / num));
            }
        }
    }

    /// `DisSeqAct_NPCSetMaterials`: the new materials (slot, scene material) for the body and
    /// the head, cooked onto the op (the Boyle sisters' colours).
    fn npc_set_materials(&mut self) {
        for i in 0..self.scene.kismet.ops.len() {
            if self.scene.kismet.ops[i].class != "DisSeqAct_NPCSetMaterials" {
                continue;
            }
            let mut lists = Vec::new();
            for key in ["body_mat_paths", "head_mat_paths"] {
                let mut list = Vec::new();
                if let Some(KVal::List(l)) = self.scene.kismet.ops[i].props.get(key).cloned() {
                    for e in l {
                        let KVal::List(pair) = e else { continue };
                        let (Some(KVal::Int(slot)), Some(KVal::Str(path))) = (pair.first(), pair.get(1)) else { continue };
                        let Some(m) = self.assets.find(path) else { continue };
                        let id = self.material(&m);
                        list.push(KVal::List(vec![KVal::Int(*slot), KVal::Int(id as i32)]));
                    }
                }
                lists.push(list);
            }
            let props = &mut self.scene.kismet.ops[i].props;
            props.insert("body_mats".into(), KVal::List(std::mem::take(&mut lists[0])));
            props.insert("head_mats".into(), KVal::List(std::mem::take(&mut lists[1])));
        }
    }

    /// `DisSeqAct_TriggerExplosion`: its explosion tweak's blast, cooked onto the op (the
    /// Prison's outer door blown open).
    fn scripted_blasts(&mut self) {
        for i in 0..self.scene.kismet.ops.len() {
            if self.scene.kismet.ops[i].class != "DisSeqAct_TriggerExplosion" {
                continue;
            }
            let Some(KVal::Str(path)) = self.scene.kismet.ops[i].props.get("m_pExplosionTweaks").cloned() else { continue };
            let Some(x) = self.assets.find(&path) else { continue };
            let b = self.blast(&x);
            let props = &mut self.scene.kismet.ops[i].props;
            props.insert("blast_radius".into(), KVal::Float(b.radius));
            props.insert("blast_full".into(), KVal::Float(b.full));
            props.insert("blast_damage".into(), KVal::Float(b.damage[1]));
            if let Some(e) = b.effect {
                props.insert("blast_effect".into(), KVal::Int(e as i32));
            }
            props.insert("blast_sound".into(), KVal::Str(b.sound));
        }
    }

    /// Scenes animate characters with their own animation sets (`InterpGroup.GroupAnimSets`:
    /// the Empress dying in Corvo's arms), which UE3 adds to the bound pawn: the characters'
    /// types take them too.
    fn matinee_pawn_anims(&mut self) {
        let k = &self.scene.kismet;
        let mut wanted: Vec<(usize, String)> = Vec::new();
        for op in k.ops.iter().filter(|o| o.class == "SeqAct_Interp") {
            let mats: Vec<u32> = op
                .vars
                .iter()
                .filter(|l| l.desc == "Data")
                .flat_map(|l| l.vars.iter())
                .filter_map(|&v| match k.vars.get(v as usize).and_then(|v| v.props.get("Matinee")) {
                    Some(KVal::Int(m)) => Some(*m as u32),
                    _ => None,
                })
                .collect();
            for mi in mats {
                let Some(m) = k.matinees.get(mi as usize) else { continue };
                for g in m.groups.iter().filter(|g| !g.anim_sets.is_empty() && g.tracks.iter().any(|t| matches!(t, KTrack::Anim(_)))) {
                    // Corvo's own (his arms)
                    if g.player {
                        if let Some(t) = self.scene.npc_types.iter().position(|t| t.name == "player_arms") {
                            for set in &g.anim_sets {
                                wanted.push((t, set.clone()));
                            }
                        }
                        continue;
                    }
                    for l in op.vars.iter().filter(|l| l.desc == g.name) {
                        for &v in &l.vars {
                            let Some(KVal::Actor(a)) = k.vars.get(v as usize).and_then(|v| v.props.get("ObjValue")) else { continue };
                            let Some(s) = k.actors.get(*a as usize).and_then(|a| a.spawner) else { continue };
                            let Some(t) = self.scene.spawners.get(s as usize).and_then(|sp| sp.npc_type) else { continue };
                            for set in &g.anim_sets {
                                wanted.push((t as usize, set.clone()));
                            }
                        }
                    }
                }
            }
        }
        let mut n = 0;
        for (t, path) in wanted {
            let Some(set) = self.assets.find(&path) else { continue };
            let Some(p) = self.pending_anims.iter_mut().find(|p| p.npc_type == t) else { continue };
            if !p.sets.iter().any(|s| s.key() == set.key()) {
                p.sets.push(set);
                n += 1;
            }
        }
        if n > 0 {
            log::info!("{n} scene animation sets added to characters");
        }
    }

    /// The props the matinees animate (a skeletal level actor, not a character, in a group with
    /// animation keys): an animated type from its mesh and the group's animation sets (and
    /// its own), put on the actor.
    fn matinee_props(&mut self) {
        let k = &self.scene.kismet;
        let mut wanted: Vec<(u32, Vec<String>)> = Vec::new();
        for op in k.ops.iter().filter(|o| o.class == "SeqAct_Interp") {
            let data = op.vars.iter().filter(|l| l.desc == "Data").flat_map(|l| l.vars.iter());
            let mats: Vec<u32> = data.filter_map(|&v| match k.vars.get(v as usize).and_then(|v| v.props.get("Matinee")) {
                Some(KVal::Int(m)) => Some(*m as u32),
                _ => None,
            }).collect();
            for mi in mats {
                let Some(m) = k.matinees.get(mi as usize) else { continue };
                for g in &m.groups {
                    if !g.tracks.iter().any(|t| matches!(t, KTrack::Anim(_))) {
                        continue;
                    }
                    for l in op.vars.iter().filter(|l| l.desc == g.name) {
                        for &v in &l.vars {
                            let Some(KVal::Actor(a)) = k.vars.get(v as usize).and_then(|v| v.props.get("ObjValue")) else { continue };
                            let Some(ka) = k.actors.get(*a as usize) else { continue };
                            if ka.spawner.is_none() && !ka.instances.is_empty() {
                                wanted.push((*a, g.anim_sets.clone()));
                            }
                        }
                    }
                }
            }
        }
        let mut made: HashMap<String, Option<u32>> = HashMap::new();
        let mut n = 0;
        for (a, sets) in wanted {
            if self.scene.kismet.actors[a as usize].device.is_some() {
                continue;
            }
            let insts = self.scene.kismet.actors[a as usize].instances.clone();
            let Some((mesh, own)) = insts.iter().find_map(|i| self.skel_insts.get(i)).cloned() else { continue };
            let mut objs: Vec<Obj> = sets.iter().filter_map(|p| self.assets.find(p)).collect();
            for o in own {
                if !objs.iter().any(|x| x.key() == o.key()) {
                    objs.push(o);
                }
            }
            if objs.is_empty() {
                continue;
            }
            let key = format!("{}|{}", mesh.key(), objs.iter().map(|o| o.key()).collect::<Vec<_>>().join(","));
            let id = match made.get(&key) {
                Some(id) => *id,
                None => {
                    let id = self.cook_device(&mesh, objs).ok();
                    made.insert(key, id);
                    id
                }
            };
            if let Some(id) = id {
                self.scene.kismet.actors[a as usize].device = Some(id);
                n += 1;
            }
        }
        if n > 0 {
            log::info!("{} props animated by matinees", n);
        }
    }

    /// Events' trigger limits from their class defaults when the level leaves them unset
    /// (`DisSeqEvent_DialogOutputs` defaults to 0, unlimited: with UE3's base 1 a dialogue's
    /// second output never reached the scripts).
    fn event_defaults(&mut self) {
        let mut cache: HashMap<String, Option<i32>> = HashMap::new();
        for i in 0..self.scene.kismet.ops.len() {
            let class = self.scene.kismet.ops[i].class.clone();
            if !(class.starts_with("SeqEvent") || class.starts_with("DisSeqEvent")) || self.scene.kismet.ops[i].props.contains_key("MaxTriggerCount") {
                continue;
            }
            let max = match cache.get(&class) {
                Some(m) => *m,
                None => {
                    let m = ["DishonoredGame", "Engine", "GameFramework", "UDKBase"].iter().find_map(|p| {
                        let o = self.assets.find(&format!("{p}.Default__{class}"))?;
                        match self.chain(&o).get("MaxTriggerCount") {
                            Some(Value::Int(n)) => Some(*n),
                            _ => None,
                        }
                    });
                    cache.insert(class.clone(), m);
                    m
                }
            };
            if let Some(n) = max {
                self.scene.kismet.ops[i].props.insert("MaxTriggerCount".into(), KVal::Int(n));
            }
        }
    }

    /// A tweak's property as Arkane's tweaks inherit: its own value, else its named fallbacks'
    /// (`m_FallbackChainCooked`) unless its `m_FallbackSkip` lists the property, else the class
    /// default.
    /// A tweak and those it falls back on (`m_FallbackChainCooked`, depth first), as object
    /// paths.
    fn tweak_lineage(&mut self, tw: &Obj) -> Vec<String> {
        let mut out = vec![tw.path()];
        let mut i = 0;
        while i < out.len() && out.len() < 12 {
            let path = out[i].clone();
            i += 1;
            let Some(o) = self.assets.find(&path) else { continue };
            let ch = self.chain(&o);
            let Some((pkg, own)) = ch.0.first().cloned() else { continue };
            let Some(Value::Array { count, offset, size }) = own.get("m_FallbackChainCooked") else { continue };
            for f in parse_struct_array(&pkg, *offset, *size, *count).unwrap_or_default() {
                if let Some(Value::Str(s) | Value::Name(s)) = f.get("m_FallbackName") {
                    // (`Pickups_twk+++Base.InventoryPickupSwordBase DisTweaks_InventoryPickup`;
                    // subobjects are their parts' own)
                    let p = s.split(' ').next().unwrap_or("").replace("+++", ".");
                    if !p.is_empty() && !p.contains(':') && !out.contains(&p) {
                        out.push(p);
                    }
                }
            }
        }
        out
    }

    fn tweak_prop(&mut self, tw: &Obj, name: &str, idx: i32, depth: u32) -> Option<(Arc<upk::Package>, Value)> {
        let ch = self.chain(tw);
        let (own_pkg, own) = ch.0.first()?.clone();
        if let Some(v) = own.get_idx(name, idx) {
            return Some((own_pkg, v.clone()));
        }
        let list = |prop: &str, field: &str| -> Vec<String> {
            match own.get(prop) {
                Some(Value::Array { count, offset, size }) => parse_struct_array(&own_pkg, *offset, *size, *count)
                    .unwrap_or_default()
                    .iter()
                    .filter_map(|f| match f.get(field) {
                        Some(Value::Str(s)) | Some(Value::Name(s)) => Some(s.clone()),
                        _ => None,
                    })
                    .collect(),
                _ => Vec::new(),
            }
        };
        let skipped = list("m_FallbackSkip", "m_PropertyPathName").iter().any(|p| p == name);
        if !skipped && depth < 6 {
            for fb in list("m_FallbackChainCooked", "m_FallbackName") {
                let path = fb.split(' ').next().unwrap_or("").replace("+++", ".");
                if let Some(o) = self.assets.find(&path) {
                    if let Some(r) = self.tweak_prop(&o, name, idx, depth + 1) {
                        return Some(r);
                    }
                }
            }
        }
        ch.0[1..].iter().find_map(|(k, p)| p.get_idx(name, idx).map(|v| (k.clone(), v.clone())))
    }

    /// `DisSeqAct_OverridePossess`: the possession tweak put on a character (cooked onto the
    /// op): possession disallowed, how long it lasts at each level, the level it needs.
    fn possess_overrides(&mut self) {
        for i in 0..self.scene.kismet.ops.len() {
            if self.scene.kismet.ops[i].class != "DisSeqAct_OverridePossess" {
                continue;
            }
            let Some(KVal::Str(path)) = self.scene.kismet.ops[i].props.get("m_pOverrideTweaks").cloned() else { continue };
            let Some(tw) = self.assets.find(&path) else {
                log::debug!("possession tweak {path} not found");
                continue;
            };
            let secs: Vec<KVal> = (0..2)
                .map(|k| match self.tweak_prop(&tw, "m_Levels", k, 0) {
                    Some((_, Value::Struct(_, r))) => KVal::Float(Props(r).float("m_fPossessDuration").unwrap_or(0.0)),
                    _ => KVal::Float(0.0),
                })
                .collect();
            // (not possessable in any state of awareness: as good as disallowed)
            let aware = (0..7).any(|k| matches!(self.tweak_prop(&tw, "m_AwarenessSettings", k, 0), Some((_, Value::Struct(_, ref r))) if Props(r.clone()).bool("m_bCanBePossessed").unwrap_or(false)));
            let disallow = matches!(self.tweak_prop(&tw, "m_bDisallowPossession", 0, 0), Some((_, Value::Bool(true)))) || !aware;
            let level = match self.tweak_prop(&tw, "m_RequiredPossessPowerLevel", 0, 0) {
                Some((_, Value::Int(l))) => l,
                _ => 2,
            };
            let props = &mut self.scene.kismet.ops[i].props;
            props.insert("possess_disallow".into(), KVal::Bool(disallow));
            props.insert("possess_secs".into(), KVal::List(secs));
            props.insert("possess_level".into(), KVal::Int(level));
        }
    }

    /// `DisSeqAct_ShowTargetNotification`: the target's name and portrait (its chapter target
    /// tweak's `m_TargetName`, the op's portrait or the tweak's `m_TargetPortraitPath`), cooked
    /// onto the op as `target_name`, `target_portrait`.
    fn target_notification_ops(&mut self) {
        for i in 0..self.scene.kismet.ops.len() {
            if self.scene.kismet.ops[i].class != "DisSeqAct_ShowTargetNotification" {
                continue;
            }
            let props = self.scene.kismet.ops[i].props.clone();
            let s = |k: &str| match props.get(k) {
                Some(KVal::Str(s)) => Some(s.clone()),
                _ => None,
            };
            let tw = s("m_pChapterTarget").and_then(|p| self.assets.find(&p));
            let (mut name, mut portrait) = (String::new(), None);
            if let Some(tw) = tw {
                if let Some((_, Value::Str(n))) = self.tweak_prop(&tw, "m_TargetName", 0, 0) {
                    name = n;
                }
                if let Some((_, Value::Str(p))) = self.tweak_prop(&tw, "m_TargetPortraitPath", 0, 0) {
                    portrait = Some(p);
                }
            }
            let portrait = s("m_pTargetPortraitOverride").or(s("m_pTargetPortrait")).or(portrait).unwrap_or_default();
            let portrait = portrait.rsplit('.').next().unwrap_or("").to_string();
            let props = &mut self.scene.kismet.ops[i].props;
            props.insert("target_name".into(), KVal::Str(name));
            props.insert("target_portrait".into(), KVal::Str(portrait));
        }
    }

    /// `DisSeqAct_SetPlayerVisSettings`: the light range it sets (cooked onto the op as `pvs`).
    fn vis_settings_ops(&mut self) {
        for i in 0..self.scene.kismet.ops.len() {
            if self.scene.kismet.ops[i].class != "DisSeqAct_SetPlayerVisSettings" {
                continue;
            }
            let Some(KVal::Str(path)) = self.scene.kismet.ops[i].props.get("m_pSettings").cloned() else { continue };
            let Some(tw) = self.assets.find(&path) else {
                log::debug!("vis settings {path} not found");
                continue;
            };
            let r = self.vis_range(&tw);
            self.scene.kismet.ops[i].props.insert("pvs".into(), KVal::List(vec![KVal::Float(r[0]), KVal::Float(r[1])]));
        }
    }

    fn given_pickups(&mut self) {
        for i in 0..self.scene.kismet.ops.len() {
            if self.scene.kismet.ops[i].class != "DisSeqAct_GivePickup" {
                continue;
            }
            let Some(KVal::Str(path)) = self.scene.kismet.ops[i].props.get("m_pPickup").cloned() else { continue };
            let Some(tw) = self.assets.find(&path) else { continue };
            let tch = self.chain(&tw);
            let mut ammo = Vec::new();
            for k in 0..8 {
                if let Some((_, Value::Struct(_, r))) = tch.get_idx_pkg("m_AmmoRanges", k) {
                    let r = Props(r.clone());
                    let n = r.int("m_MaxValue").or(r.int("m_MinValue")).unwrap_or(0);
                    if n > 0 {
                        let min = r.int("m_MinValue").unwrap_or(n).clamp(0, n);
                        ammo.push(KVal::List(vec![KVal::Int(k), KVal::Int(n), KVal::Int(min)]));
                    }
                }
            }
            let quantity = match tch.get("m_Quantity") {
                Some(Value::Int(q)) => *q,
                _ => 0,
            };
            let item_path = tch.obj_path("m_pAbstractItem").unwrap_or_default();
            let coins = item_path.ends_with("Coins_AbsItm");
            let item = item_path.split_once('.').map(|x| x.1.to_string()).unwrap_or(item_path);
            let label = tch.obj(self.assets, "m_pInteractableTweaks").map(|it| self.chain(&it).name("m_Name").unwrap_or_default().to_string()).unwrap_or_default();
            let props = &mut self.scene.kismet.ops[i].props;
            if !ammo.is_empty() {
                props.insert("pickup_ammo".into(), KVal::List(ammo));
            }
            if tch.name("m_Type") == Some("eStatPickupType_Food") {
                if let Some(Value::Int(n)) = tch.get("m_HealthChange") {
                    props.insert("pickup_food_health".into(), KVal::Int((*n).max(0)));
                }
            }
            props.insert("pickup_quantity".into(), KVal::Int(quantity));
            props.insert("pickup_coins".into(), KVal::Bool(coins));
            props.insert("pickup_item".into(), KVal::Str(item));
            props.insert("pickup_label".into(), KVal::Str(label));
        }
    }

    fn pickup(&mut self, actor: &Obj) -> Result<()> {
        let ch = self.chain(actor);
        let loc = ch.vector("Location").unwrap_or([0.0; 3]);
        const TWEAKS: [&str; 6] = ["m_pTweaks", "m_pPickupTweaks", "m_pStatPickupTweaks", "m_pKeyTweaks", "m_pInventoryTweaks", "m_pInvPickupTweaks"];
        let tweak = TWEAKS.iter().find_map(|n| ch.obj_path(n)).unwrap_or_default();
        if let Some(tw) = TWEAKS.iter().find_map(|n| ch.obj(self.assets, n)) {
            let lineage = self.tweak_lineage(&tw);
            self.actor_ref(actor).tweaks = lineage;
        }
        let mut ammo = Vec::new();
        let mut ammo_min = Vec::new();
        let mut food_health = None;
        if let Some(tw) = ["m_pTweaks", "m_pPickupTweaks", "m_pStatPickupTweaks", "m_pInventoryTweaks", "m_pInvPickupTweaks"].iter().find_map(|n| ch.obj(self.assets, n)) {
            let tch = self.chain(&tw);
            if tch.name("m_Type") == Some("eStatPickupType_Food") {
                if let Some(Value::Int(n)) = tch.get("m_HealthChange") { food_health = Some((*n).max(0) as u32); }
            }
            for i in 0..8 {
                if let Some((_, Value::Struct(_, r))) = tch.get_idx_pkg("m_AmmoRanges", i) {
                    let r = Props(r.clone());
                    let n = r.int("m_MaxValue").or(r.int("m_MinValue")).unwrap_or(0);
                    if n > 0 {
                        ammo.push((i as u8, n as u32));
                        let min = r.int("m_MinValue").unwrap_or(n).clamp(0, n) as u32;
                        if min < n as u32 { ammo_min.push((i as u8, min)); }
                    }
                }
            }
        }
        // its abstract item (`WrittenNotes_twk.GlobalBooks.X` -> `GlobalBooks.X`), or the
        // location map it shows (`m_Map`: `map:DUM_Streets`)
        let mut item = ch.obj_path("m_pAbstractItem").map(|p| p.split_once('.').map(|x| x.1.to_string()).unwrap_or(p)).unwrap_or_default();
        if item.is_empty() {
            if let Some(tw) = ch.obj(self.assets, "m_pTweaks") {
                if let Some(m) = self.chain(&tw).name("m_Map").filter(|m| *m != "DUM_None") {
                    item = format!("map:{m}");
                }
            }
        }
        // (else its tweak's: the Prison's bread note, blueprints, the Boyle invitation; not
        // money's)
        if item.is_empty() {
            if let Some(tw) = ["m_pTweaks", "m_pPickupTweaks"].iter().find_map(|n| ch.obj(self.assets, n)) {
                if let Some(p) = self.chain(&tw).obj_path("m_pAbstractItem").filter(|p| !p.ends_with("Coins_AbsItm")) {
                    item = p.split_once('.').map(|x| x.1.to_string()).unwrap_or(p);
                }
            }
        }
        // its name, how many and whether it's money
        let (mut label, mut quantity, mut coins) = (String::new(), 0, false);
        let mut inv_item = String::new();
        if let Some(tw) = ["m_pTweaks", "m_pPickupTweaks", "m_pStatPickupTweaks", "m_pInventoryTweaks", "m_pKeyTweaks", "m_pInvPickupTweaks"].iter().find_map(|n| ch.obj(self.assets, n)) {
            let tch = self.chain(&tw);
            if let Some(p) = tch.obj_path("m_pItem") {
                inv_item = p.rsplit('.').next().unwrap_or(&p).to_string();
            }
            if let Some(Value::Int(q)) = tch.get("m_Quantity") {
                quantity = (*q).max(0) as u32;
            }
            coins = tch.obj_path("m_pAbstractItem").is_some_and(|p| p.ends_with("Coins_AbsItm"));
            if let Some(it) = tch.obj(self.assets, "m_pInteractableTweaks") {
                label = self.chain(&it).name("m_Name").unwrap_or_default().to_string();
            }
        }
        let key = if actor.class().starts_with("DisKey") { ch.name("m_Name").unwrap_or_default().to_string() } else { String::new() };
        let index = self.scene.pickups.len() as u32;
        self.actor_ref(actor).pickup = Some(index);
        self.scene.pickups.push(Pickup {
            key,
            label,
            quantity,
            coins,
            item,
            name: actor.name().to_string(),
            class: actor.class(),
            kind: tweak,
            position: ue_point(loc),
            instance: None,
            ammo,
            ammo_min,
            food_health,
            factory: false,
            inv_item,
        });
        Ok(())
    }

    /// A brush volume's convex hulls, in the world (Bevy space).
    fn brush_hulls(&mut self, actor: &Obj) -> Vec<Vec<[f32; 3]>> {
        let ach = self.chain(actor);
        let Some(comp) = ach.obj(self.assets, "BrushComponent") else { return Vec::new() };
        let m = self.actor_matrix(actor);
        let cch = self.chain(&comp);
        let mut hulls = Vec::new();
        if let Some((gpkg, Value::Struct(_, geom))) = cch.get_pkg("BrushAggGeom") {
            let gpkg = gpkg.clone();
            let geom = Props(geom.clone());
            if let Some((c, off, sz)) = geom.array("ConvexElems") {
                for elem in parse_struct_array(&gpkg, off, sz, c).unwrap_or_default() {
                    if let Some((vc, voff, _)) = elem.array("VertexData") {
                        let mut r = Reader::at(&gpkg.data, voff);
                        let pts: Vec<[f32; 3]> = (0..vc).map_while(|_| r.vec3().ok()).map(|v| ue_point(m.transform_point3(Vec3::from(v)).to_array())).collect();
                        if pts.len() >= 4 {
                            hulls.push(pts);
                        }
                    }
                }
            }
        }
        hulls
    }

    fn volume(&mut self, actor: &Obj) -> Result<()> {
        let cls = actor.class();
        if !matches!(cls.as_str(), "BlockingVolume" | "DishonoredWaterVolume" | "DisHideoutVolume" | "PhysicsVolume" | "KillVolume" | "DisDeathVolume" | "DisStealthVolume" | "DisForbiddenZone" | "DisTetherVolume" | "DisPossessionVolume")
            && !cls.contains("Water")
            && !cls.contains("Blocking")
            && !cls.contains("Trigger")
        {
            return Ok(());
        }
        let ach = self.chain(actor);
        let hulls = self.brush_hulls(actor);
        if !hulls.is_empty() {
            let index = self.scene.volumes.len() as u32;
            self.actor_ref(actor).volume = Some(index);
            let water = (cls == "DishonoredWaterVolume" || ach.bool("bWaterVolume") == Some(true)).then(|| self.water(&ach));
            let stealth = match (cls == "DisStealthVolume").then(|| ach.obj(self.assets, "m_pVisSettings")).flatten() {
                Some(v) => {
                    let r = self.vis_range(&v);
                    let priority = match ach.get("m_StealthVolumePriority") {
                        Some(Value::Int(p)) => *p as f32,
                        _ => 0.0,
                    };
                    Some([r[0], r[1], priority])
                }
                None => None,
            };
            // a forbidden zone: its owners, who mustn't be in it, on from the start or not
            let zone = (cls == "DisForbiddenZone").then(|| {
                let names = |key: &str| -> Vec<String> {
                    match ach.get_pkg(key) {
                        Some((pkg, Value::Array { count, offset, .. })) => {
                            let mut r = Reader::at(&pkg.data, *offset);
                            (0..*count).filter_map(|_| r.i32().ok()).filter(|o| *o != 0).map(|o| pkg.obj_path(o).rsplit('.').next().unwrap_or("").to_string()).collect()
                        }
                        _ => Vec::new(),
                    }
                };
                ForbiddenZone { owners: names("m_OwningFactions"), forbidden: names("m_ForbiddenFactions"), enabled: ach.bool("m_bEnabled").unwrap_or(true) }
            });
            let no_unpossess = cls == "DisPossessionVolume" && ach.bool("m_bDisallowUnpossession").unwrap_or(true);
            self.scene.volumes.push(Volume { kind: cls, name: actor.name().to_string(), hulls, water, stealth, zone, no_unpossess });
        }
        Ok(())
    }

    /// A river krust: placement, look and the tweak's behaviour.
    fn krust(&mut self, actor: &Obj) -> Result<()> {
        let ch = self.chain(actor);
        let Some(tweak) = ch.obj(self.assets, "m_pRiverKrustTweaks") else { return Ok(()) };
        let tc = self.chain(&tweak);
        let ranged = |n: &str| -> [f32; 2] {
            match tc.get(n) {
                Some(Value::Struct(_, s)) => {
                    let p = Props(s.clone());
                    let lo = p.float("m_fMinValue").or(p.int("m_MinValue").map(|v| v as f32)).unwrap_or(0.0);
                    let hi = p.float("m_fMaxValue").or(p.int("m_MaxValue").map(|v| v as f32)).unwrap_or(lo);
                    [lo, hi.max(lo)]
                }
                _ => [0.0, 0.0],
            }
        };
        let f = |n: &str, d: f32| tc.float(n).unwrap_or(d);
        let mut damage = [10.0; 4];
        for (i, d) in damage.iter_mut().enumerate() {
            if let Some((_, Value::Int(v))) = tc.get_idx_pkg("m_ProjectileDamage", i as i32) {
                *d = *v as f32;
            }
        }
        let proj = tc.obj(self.assets, "m_pProjectileTweak").map(|p| self.chain(&p));
        let trail = proj.as_ref().and_then(|p| p.obj(self.assets, "m_pTrailEffect")).and_then(|ps| self.particle_system(&ps));
        let gravity = proj.as_ref().and_then(|p| p.float("m_fGravityMultiplier")).unwrap_or(0.2);
        let loot_coins = tc.obj(self.assets, "m_pLootTweak").map(|l| self.chain(&l)).and_then(|l| match l.get("m_Quantity") {
            Some(Value::Int(q)) => Some(*q as u32),
            _ => None,
        }).unwrap_or(0);
        let npc_type = self.npc_type(&tweak);
        let health = ranged("m_MaximumHealth");
        let speed = ranged("m_ProjectileSpeed");
        let ds = ch.float("DrawScale").unwrap_or(1.0) * ch.vector("DrawScale3D").map(|v| v[0]).unwrap_or(1.0);
        // the sequence of each animation state
        let rng = |p: &Props, n: &str, d: f32| -> [f32; 2] {
            match p.get(n) {
                Some(Value::Struct(_, s)) => {
                    let r = Props(s.clone());
                    let lo = r.float("m_fMinValue").unwrap_or(d);
                    [lo, r.float("m_fMaxValue").unwrap_or(d).max(lo)]
                }
                _ => [d, d],
            }
        };
        let anims = (0..17)
            .map(|i| match tc.get_idx_pkg("m_AnimSequences", i) {
                Some((_, Value::Struct(_, s))) => {
                    let p = Props(s.clone());
                    KrustAnim {
                        name: p.name("m_SequenceName").unwrap_or_default().to_string(),
                        rate: rng(&p, "m_PlaybackRateRange", 1.0),
                        start: rng(&p, "m_StartTimeSecondsRange", 0.0)[1],
                        looping: p.bool("m_bIsLooping").unwrap_or(false),
                        restart: p.bool("m_bShouldRestart").unwrap_or(false),
                        blend: p.float("m_fBlendInTimeSeconds").unwrap_or(0.15),
                    }
                }
                _ => KrustAnim::default(),
            })
            .collect();
        let filters = |n: &str| -> Vec<(String, f32)> {
            let Some((pkg, Value::Array { count, offset, size })) = tc.get_pkg(n) else { return Vec::new() };
            parse_struct_array(pkg, *offset, *size, *count)
                .unwrap_or_default()
                .iter()
                .filter_map(|p| {
                    let o = p.object("m_DamageType").filter(|o| *o != 0)?;
                    let path = pkg.obj_path(o);
                    Some((path.rsplit('.').next().unwrap_or(&path).to_string(), p.float("m_fDamageMultiplier").unwrap_or(1.0)))
                })
                .collect()
        };
        let delay = |n: &str, d: [f32; 2]| {
            let r = ranged(n);
            if r == [0.0, 0.0] { d } else { r }
        };
        // the stalk (its first body part) and the pearl
        let mut stalk = None;
        if let Some((pkg, Value::Array { count, offset, size })) = tc.get_pkg("m_pBodyParts") {
            let parts = parse_struct_array(pkg, *offset, *size, *count).unwrap_or_default();
            let pkg = pkg.clone();
            if let Some(bp) = parts.first().and_then(|p| p.object("m_pBodyPartTweak")).and_then(|o| self.assets.resolve(&pkg, o)) {
                if let Some(m) = self.chain(&bp).obj(self.assets, "m_pStaticMesh") {
                    stalk = self.cook_mesh(&m).ok().flatten();
                }
            }
        }
        let loot = tc.obj(self.assets, "m_pLootTweak").map(|l| self.chain(&l));
        let pearl_mesh = loot.as_ref().and_then(|l| l.obj(self.assets, "m_pPickupStaticMesh"));
        let pearl = pearl_mesh.and_then(|m| self.cook_mesh(&m).ok().flatten());
        let pearl_name = loot
            .as_ref()
            .and_then(|l| l.obj(self.assets, "m_pInteractableTweaks"))
            .and_then(|it| self.chain(&it).name("m_Name").map(str::to_string))
            .unwrap_or_else(|| "River Krust Pearl".into());
        let index = self.scene.krusts.len() as u32;
        self.scene.krusts.push(Krust {
            name: actor.name().to_string(),
            anims,
            defensive_delay: delay("m_DefensiveEnterDelay", [0.25, 0.25]),
            defensive_exit_delay: delay("m_DefensiveExitDelay", [1.5, 2.5]),
            aggressive_exit_delay: delay("m_AggressiveExitDelay", [2.0, 3.0]),
            reaction: delay("m_DamageReactionStateDuration", [3.0, 4.0]),
            fire_angle: f("m_fProjectileFieldOfFireAngle", 90.0),
            speed_distance: { let r = ranged("m_ProjectileTargetDistanceToSpeedMap"); [r[0] * UNIT, r[1] * UNIT] },
            accurate: f("m_fChanceForAccurateProjectileAim", 0.9),
            lead: tc.bool("m_bShouldLeadTargetWhenFiringProjectile").unwrap_or(false),
            filters: filters("m_DamageFilters"),
            protected_filters: filters("m_ProtectedDamageFilters"),
            stalk,
            pearl,
            pearl_name,
            position: ue_point(ch.vector("Location").unwrap_or([0.0; 3])),
            rotation: ch.rotator("Rotation").unwrap_or([0; 3]),
            scale: ds,
            npc_type,
            defensive: [f("m_fDefensiveEnterRange", 500.0) * UNIT, f("m_fDefensiveExitRange", 600.0) * UNIT],
            aggressive: [f("m_fAggressiveEnterRange", 1800.0) * UNIT, f("m_fAggressiveExitRange", 2000.0) * UNIT],
            damage,
            health: health[1].max(1.0),
            first_volley: ranged("m_InitialVolleyDelay"),
            between_volleys: ranged("m_TimeBetweenVolleys"),
            shots: { let s = ranged("m_NumShotsPerVolley"); [s[0].max(1.0) as u32, s[1].max(1.0) as u32] },
            speed: [speed[0] * UNIT, speed[1] * UNIT],
            gravity,
            trail,
            loot_coins,
        });
        self.actor_ref(actor).krust = Some(index);
        Ok(())
    }

    /// A tripwire or a launcher: its mesh and sequences, and a launcher's aim, shot and loot.
    fn trap(&mut self, actor: &Obj) -> Result<()> {
        let ch = self.chain(actor);
        let launcher = actor.class() == "DisProjectileLauncher";
        let Some(tweak) = ch.obj(self.assets, if launcher { "m_pProjectileLauncherTweaks" } else { "m_pTripwireTweaks" }) else { return Ok(()) };
        let tc = self.chain(&tweak);
        let npc_type = self.npc_type(&tweak);
        let anim = |n: &str| tc.name(n).filter(|n| *n != "None").unwrap_or_default().to_string();
        let (fire_anim, disarm_anim) = (anim("m_TrippedAnimName"), anim("m_DeactivatedAnimName"));
        let (mut label, mut verb, mut used_message) = (String::new(), String::new(), String::new());
        if let Some(it) = tc.obj(self.assets, "m_pInteractableTweaks") {
            let ic = self.chain(&it);
            label = ic.name("m_Name").unwrap_or_default().to_string();
            verb = ic.name("m_InteractText").unwrap_or_default().replace("`GBA_Use`", "").trim().to_string();
            used_message = ic.name("m_UseMessage").unwrap_or_default().to_string();
        }
        // the target point it aims at
        let target = ch.obj(self.assets, "m_pTarget").and_then(|t| t.props().ok()).and_then(|p| match p.get("Location") {
            Some(Value::Vector(v)) => Some(ue_point(*v)),
            _ => None,
        });
        // the sequences' notifies: when it shoots, and the effects at its sockets
        let (mut fire_at, mut fire_fx, mut disarm_fx) = (0.1, None, None);
        if let Some(set) = tc.obj(self.assets, "m_pAnimSet") {
            if let Ok(sp) = set.props() {
                for q in object_array(&set.pkg, &sp, "Sequences") {
                    let Some(seq) = self.assets.resolve(&set.pkg, q) else { continue };
                    let Ok(qp) = seq.props() else { continue };
                    let name = qp.name("SequenceName").unwrap_or_default().to_string();
                    let firing = name.eq_ignore_ascii_case(&fire_anim);
                    if !firing && !name.eq_ignore_ascii_case(&disarm_anim) {
                        continue;
                    }
                    let Some((c, o, sz)) = qp.array("Notifies") else { continue };
                    for n in parse_struct_array(&seq.pkg, o, sz, c).unwrap_or_default() {
                        let Some(no) = n.object("Notify").filter(|o| *o > 0) else { continue };
                        match seq.pkg.class_name(no).as_str() {
                            "DishonoredNotify_FireProjectile" if firing => fire_at = n.float("Time").unwrap_or(0.1),
                            "AnimNotify_PlayParticleEffect" => {
                                let Ok(np) = upk::read_object(&seq.pkg, no).map(|o| o.props) else { continue };
                                let Some(ps) = np.object("PSTemplate").filter(|o| *o != 0).and_then(|o| self.assets.resolve(&seq.pkg, o)) else { continue };
                                let Some(id) = self.particle_system(&ps) else { continue };
                                let fx = Some((np.name("SocketName").unwrap_or("None").to_string(), id));
                                if firing {
                                    fire_fx = fx;
                                } else {
                                    disarm_fx = fx;
                                }
                            }
                            _ => {}
                        }
                    }
                }
            }
        }
        // the shot
        let proj = tc.obj(self.assets, "m_pProjectileTweak").map(|p| self.chain(&p));
        let ev_name = |c: &Chain, n: &str| c.obj_path(n).map(|p| p.rsplit('.').next().unwrap_or(&p).to_string()).unwrap_or_default();
        let trail = proj.as_ref().and_then(|p| p.obj(self.assets, "m_pTrailEffect")).and_then(|ps| self.particle_system(&ps));
        let fly_sound = proj.as_ref().map(|p| ev_name(p, "m_pSoundEvent_InAir")).unwrap_or_default();
        let gravity = proj.as_ref().and_then(|p| p.float("m_fGravityMultiplier")).unwrap_or(1.0);
        let blast = proj.as_ref().and_then(|p| p.obj(self.assets, "m_pExplosionTweak")).map(|x| self.blast(&x));
        let harvest = (0..8)
            .filter_map(|i| match tc.get_idx_pkg("m_HarvestedAmmo", i) {
                Some((_, Value::Int(n))) if *n > 0 => Some((i as u8, *n as u32)),
                _ => None,
            })
            .collect();
        let index = self.scene.traps.len() as u32;
        self.scene.traps.push(Trap {
            name: actor.name().to_string(),
            launcher,
            position: ue_point(ch.vector("Location").unwrap_or([0.0; 3])),
            rotation: ch.rotator("Rotation").unwrap_or([0; 3]),
            scale: ch.float("DrawScale").unwrap_or(1.0) * ch.vector("DrawScale3D").map(|v| v[0]).unwrap_or(1.0),
            npc_type,
            fire_anim,
            disarm_anim,
            label,
            verb,
            used_message,
            target,
            socket: tc.name("m_FiringSocketName").filter(|n| *n != "None").unwrap_or_default().to_string(),
            fire_at,
            speed: tc.float("m_fProjectileSpeed").unwrap_or(2000.0) * UNIT,
            damage: tc.float("m_fProjectileDamage").unwrap_or(10.0),
            gravity,
            trail,
            fly_sound,
            blast,
            fire_fx,
            disarm_fx,
            harvest,
        });
        self.actor_ref(actor).trap = Some(index);
        Ok(())
    }

    /// A usable object: its stages, prompt, and its moving part as an animated type.
    fn usable_object(&mut self, actor: &Obj) -> Result<()> {
        let ch = self.chain(actor);
        // (an audiograph player's are `m_pAudioLogPlayerTweaks`)
        let Some(tweak) = ch.obj(self.assets, "m_pUsableObjTweaks").or_else(|| ch.obj(self.assets, "m_pAudioLogPlayerTweaks")) else { return Ok(()) };
        // the tweak, then its named fallbacks (`m_FallbackChainCooked`: "usable_twk+++UsableObject_twk Class")
        let mut chains = vec![self.chain(&tweak)];
        let fallbacks: Vec<String> = match chains[0].get_pkg("m_FallbackChainCooked") {
            Some((fpkg, Value::Array { count, offset, size })) => parse_struct_array(fpkg, *offset, *size, *count)
                .unwrap_or_default()
                .iter()
                .filter_map(|f| f.name("m_FallbackName").map(|n| n.split(' ').next().unwrap_or("").replace("+++", ".")))
                .collect(),
            _ => Vec::new(),
        };
        for path in fallbacks {
            if let Some(o) = self.assets.find(&path) {
                let c = self.chain(&o);
                chains.push(c);
            }
        }
        let mut stages = Vec::new();
        if let Some((pkg, Value::Array { count, offset, size })) = chains.iter().find_map(|c| c.get_pkg("m_Stages")) {
            for st in parse_struct_array(pkg, *offset, *size, *count).unwrap_or_default() {
                let name = |k: &str| st.name(k).filter(|n| *n != "None").unwrap_or_default().to_string();
                stages.push(UsableStage {
                    name: name("m_Name"),
                    anim: name("m_TransitionAnimName"),
                    text: st.name("m_InteractTextOverride").unwrap_or_default().to_string(),
                    once: st.bool("m_bDeactivateUsableWhenDone").unwrap_or(false),
                });
            }
        }
        if stages.iter().all(|s| s.anim.is_empty()) {
            return Ok(());
        }
        // its moving part and the sequences that move it (the tweak's, else the component's)
        let Some(comp) = ch.obj(self.assets, "SkeletalMeshComponent") else { return Ok(()) };
        let cc = self.chain(&comp);
        let Some(mesh) = cc.obj(self.assets, "SkeletalMesh") else { return Ok(()) };
        let mut set_refs: Vec<(std::sync::Arc<upk::Package>, i32)> = Vec::new();
        let sources: Vec<(&Chain, &str)> = chains.iter().map(|c| (c, "m_AnimSets")).chain(std::iter::once((&cc, "AnimSets"))).collect();
        for (c, prop) in sources {
            if let Some((pkg, Value::Array { count, offset, size })) = c.get_pkg(prop) {
                if *size >= *count * 4 {
                    for k in 0..*count {
                        let o = i32::from_le_bytes(pkg.data[offset + k * 4..offset + k * 4 + 4].try_into().unwrap());
                        set_refs.push((pkg.clone(), o));
                    }
                }
            }
            if !set_refs.is_empty() {
                break;
            }
        }
        let sets: Vec<Obj> = set_refs.iter().filter_map(|(p, o)| self.assets.resolve(p, *o)).filter(|s| s.class() == "AnimSet").collect();
        let (mut label, mut text) = (String::new(), String::new());
        if let Some(it) = chains.iter().find_map(|c| c.obj(self.assets, "m_pInteractableTweaks")) {
            let ic = self.chain(&it);
            label = ic.name("m_Name").unwrap_or_default().to_string();
            text = ic.name("m_InteractText").unwrap_or_default().to_string();
        }
        let npc_type = self.device_type(&mesh, sets);
        // an audiograph player: its card, its start and stop sounds, the card's loop
        let audiograph = ch.obj_path("m_pAudioLog").and_then(|p| p.split_once('.').map(|x| x.1.to_string())).unwrap_or_default();
        let mut flavor = Vec::new();
        let mut playing_anim = String::new();
        if !audiograph.is_empty() {
            for k in 0..4 {
                if let Some((pkg, Value::Object(o))) = chains.iter().find_map(|c| c.get_idx_pkg("m_pFlavorSounds", k)) {
                    if *o != 0 {
                        let p = pkg.obj_path(*o);
                        flavor.push(p.rsplit('.').next().unwrap_or(&p).to_string());
                    }
                }
            }
            playing_anim = chains.iter().find_map(|c| c.name("m_PlayingAnimName")).filter(|n| *n != "None").unwrap_or_default().to_string();
        }
        let index = self.scene.usables.len() as u32;
        self.scene.usables.push(UsableObj {
            name: actor.name().to_string(),
            npc_type,
            instance: None,
            stages,
            label,
            text,
            locked: ch.bool("m_bLocked").unwrap_or(false),
            keys: str_array(&ch, "m_MatchingKeys"),
            audiograph,
            flavor,
            playing_anim,
        });
        self.usable_ids.insert(actor.key(), index);
        self.actor_ref(actor).usable = Some(index);
        Ok(())
    }

    /// A moving part (a locker's doors, a lever) as a character type: its skeleton, skin and
    /// animations.
    fn device_type(&mut self, body: &Obj, sets: Vec<Obj>) -> Option<u32> {
        let key = format!("device:{}:{}", body.key(), sets.iter().map(|s| s.key()).collect::<Vec<_>>().join(","));
        if let Some(v) = self.npc_type_ids.get(&key) {
            return *v;
        }
        let r = self.cook_device(body, sets);
        if let Err(e) = &r {
            log::warn!("device {}: {e:#}", body.path());
        }
        let id = r.ok();
        self.npc_type_ids.insert(key, id);
        id
    }

    fn cook_device(&mut self, body: &Obj, sets: Vec<Obj>) -> Result<u32> {
        let bdata = skeletal::read_skeletal_mesh(&body.pkg, body.idx)?;
        let skel_key = body.key();
        let skeleton = match self.skeleton_ids.get(&skel_key) {
            Some(&i) => i,
            None => {
                let i = self.scene.skeletons.len() as u32;
                self.scene.skeletons.push(SkeletonDef { name: body.path(), bones: skeletal::to_skeleton(&bdata), sockets: mesh_sockets(body) });
                self.skeleton_ids.insert(skel_key, i);
                i
            }
        };
        let (body_id, body_mats) = self.cook_skinned(body, &bdata, None)?;
        let id = self.scene.npc_types.len() as u32;
        self.pending_anims.push(PendingAnims::new(id as usize, sets, &bdata));
        self.scene.npc_types.push(NpcType {
            attachments: Vec::new(),
            out_of_bend: false,
            ragdoll: None,
            name: body.path(),
            kind: "device".into(),
            skeleton: Some(skeleton),
            body: Some(body_id),
            body_materials: body_mats,
            head: None,
            head_materials: Vec::new(),
            anim_sets: Vec::new(),
            voices: Vec::new(),
            faction: String::new(),
            hostile: false,
            story_group: String::new(),
            possess: None,
            body_slots: Vec::new(),
            head_slots: Vec::new(),
            facefx: None,
            sight: None,
            stats: None,
        });
        Ok(id)
    }

    /// An explosion (`DisTweaks_Explosion`): its radii, damage by difficulty, effect and sound.
    fn blast(&mut self, x: &Obj) -> TrapBlast {
        let xc = self.chain(x);
        let mut damage = [0.0; 4];
        if let Some(Value::Struct(_, s)) = xc.get("m_RadialDamage") {
            let p = Props(s.clone());
            for (i, k) in ["m_fBaseValue1_Easy", "m_fBaseValue2_Normal", "m_fBaseValue3_Hard", "m_fBaseValue4_VeryHard"].iter().enumerate() {
                damage[i] = p.float(k).unwrap_or(0.0);
            }
        }
        let effect = xc.obj(self.assets, "m_pExplosionEffect").and_then(|ps| self.particle_system(&ps));
        let sound = xc.obj_path("m_pSoundEvent").map(|p| p.rsplit('.').next().unwrap_or(&p).to_string()).unwrap_or_default();
        TrapBlast {
            radius: xc.float("m_fDamageRadius").unwrap_or(600.0) * UNIT,
            full: xc.float("m_fFullDamageRadius").unwrap_or(300.0) * UNIT,
            player_radius: xc.float("m_fPlayerDamageRadius").unwrap_or(600.0) * UNIT,
            player_full: xc.float("m_fPlayerFullDamageRadius").unwrap_or(150.0) * UNIT,
            damage,
            effect,
            sound,
        }
    }

    /// A protective tune played from the level.
    fn tune_source(&mut self, actor: &Obj) -> Result<()> {
        let ch = self.chain(actor);
        let Some(src) = ch.obj(self.assets, "m_pTuneTweak") else { return Ok(()) };
        let Some(tune) = self.chain(&src).obj(self.assets, "m_pTuneTweak") else { return Ok(()) };
        let tc = self.chain(&tune);
        let ev = |n: &str| tc.obj_path(n).map(|p| p.rsplit('.').next().unwrap_or(&p).to_string()).unwrap_or_default();
        self.scene.tune_sources.push(TuneSource {
            position: ue_point(ch.vector("Location").unwrap_or([0.0; 3])),
            disorient: tc.float("m_fMusicEffectRadius_Disorient").unwrap_or(600.0) * UNIT,
            inhibit: tc.float("m_fMusicEffectRadius_InhibitPowers").unwrap_or(400.0) * UNIT,
            sounds: [ev("m_SFX_Disorient_Start"), ev("m_SFX_Disorient_Stop"), ev("m_SFX_Inhibit_Start"), ev("m_SFX_Inhibit_Stop")],
        });
        self.actor_ref(actor);
        Ok(())
    }

    /// A tap or a fountain: its prompt, and its "Use" stage's sounds and water stream.
    fn water_source(&mut self, actor: &Obj) -> Result<()> {
        let ch = self.chain(actor);
        let Some(tweak) = ch.obj(self.assets, "m_pUsableObjTweaks") else { return Ok(()) };
        let tc = self.chain(&tweak);
        let (mut name, mut text) = ("Water".to_string(), "Drink".to_string());
        if let Some(it) = tc.obj(self.assets, "m_pInteractableTweaks") {
            let ic = self.chain(&it);
            if let Some(n) = ic.name("m_Name").filter(|n| !n.is_empty()) {
                name = n.to_string();
            }
            if let Some(t) = ic.name("m_InteractText").filter(|t| !t.is_empty()) {
                text = t.replace("`GBA_Use`", "").trim().to_string();
            }
        }
        // the stage's animation
        let mut anim = "Use".to_string();
        if let Some((pkg, Value::Array { count, offset, size })) = tc.get_pkg("m_Stages") {
            if let Some(st) = parse_struct_array(pkg, *offset, *size, *count).unwrap_or_default().first() {
                if let Some(n) = st.name("m_TransitionAnimName").filter(|n| *n != "None") {
                    anim = n.to_string();
                }
            }
        }
        let world = ue_to_bevy(self.actor_matrix(actor));
        let mesh = match tc.get("m_UnbrokenMeshInfo") {
            Some(Value::Struct(_, s)) => {
                let (pkg, _) = tc.get_pkg("m_UnbrokenMeshInfo").unwrap();
                let pkg = pkg.clone();
                Props(s.clone()).object("m_pMesh").filter(|o| *o != 0).and_then(|o| self.assets.resolve(&pkg, o))
            }
            _ => None,
        };
        let (mut sounds, mut stream, mut duration) = (Vec::new(), None, 2.0);
        if let Some((pkg, Value::Array { count, offset, size })) = tc.get_pkg("m_AnimSets") {
            let pkg = pkg.clone();
            let sets: Vec<i32> = if *size >= *count * 4 {
                (0..*count).map(|k| i32::from_le_bytes(pkg.data[*offset + k * 4..*offset + k * 4 + 4].try_into().unwrap())).collect()
            } else {
                Vec::new()
            };
            'sets: for s in sets {
                let Some(set) = self.assets.resolve(&pkg, s) else { continue };
                let Ok(sp) = set.props() else { continue };
                for q in object_array(&set.pkg, &sp, "Sequences") {
                    let Some(seq) = self.assets.resolve(&set.pkg, q) else { continue };
                    let Ok(qp) = seq.props() else { continue };
                    if !qp.name("SequenceName").is_some_and(|n| n.eq_ignore_ascii_case(&anim)) {
                        continue;
                    }
                    sounds = sound_notifies(&seq.pkg, &qp);
                    duration = qp.float("SequenceLength").unwrap_or(2.0);
                    // the running water, at its socket
                    if let Some((c, o, sz)) = qp.array("Notifies") {
                        for n in parse_struct_array(&seq.pkg, o, sz, c).unwrap_or_default() {
                            let Some(no) = n.object("Notify").filter(|o| *o > 0) else { continue };
                            if seq.pkg.class_name(no) != "AnimNotify_PlayParticleEffect" {
                                continue;
                            }
                            let Ok(np) = upk::read_object(&seq.pkg, no).map(|o| o.props) else { continue };
                            let Some(ps) = np.object("PSTemplate").filter(|o| *o != 0).and_then(|o| self.assets.resolve(&seq.pkg, o)) else { continue };
                            let Some(id) = self.particle_system(&ps) else { continue };
                            let socket = np.name("SocketName").unwrap_or("None").to_string();
                            let local = mesh.as_ref().and_then(|m| keyhole_socket(m, &socket)).unwrap_or(Vec3::new(0.0, 0.9, 0.0));
                            stream = Some((n.float("Time").unwrap_or(0.0), id, world.transform_point3(local).to_array()));
                            break;
                        }
                    }
                    break 'sets;
                }
            }
        }
        let position = mesh.as_ref().and_then(|m| keyhole_socket(m, "None")).map(|p| world.transform_point3(p).to_array()).or(stream.map(|s| s.2)).unwrap_or(world.transform_point3(Vec3::new(0.0, 0.8, 0.0)).to_array());
        self.scene.water_sources.push(WaterSource { name, text, position, sounds, stream, duration });
        self.actor_ref(actor);
        Ok(())
    }

    /// A hagfish: where it swims and how it behaves.
    fn fish(&mut self, actor: &Obj) -> Result<()> {
        let ch = self.chain(actor);
        let Some(tweak) = ch.obj(self.assets, "m_pTweaks") else { return Ok(()) };
        let tc = self.chain(&tweak);
        let f = |n: &str, d: f32| tc.float(n).unwrap_or(d);
        let mut damage = [1.0; 4];
        for (i, d) in damage.iter_mut().enumerate() {
            if let Some((_, Value::Int(v))) = tc.get_idx_pkg("m_DamagePerHit", i as i32) {
                *d = *v as f32;
            }
        }
        let bites = match tc.get_pkg("m_BiteAnimNames") {
            Some((pkg, Value::Array { count, offset, size })) if *size >= *count * 8 => (0..*count)
                .filter_map(|k| {
                    let at = *offset + k * 8;
                    let idx = i32::from_le_bytes(pkg.data[at..at + 4].try_into().ok()?);
                    let num = i32::from_le_bytes(pkg.data[at + 4..at + 8].try_into().ok()?);
                    let base = pkg.names.get(idx as usize)?;
                    Some(if num > 0 { format!("{base}_{}", num - 1) } else { base.clone() })
                })
                .collect(),
            _ => Vec::new(),
        };
        let name = |n: &str, d: &str| tc.name(n).unwrap_or(d).to_string();
        let npc_type = self.npc_type(&tweak);
        self.scene.fish.push(Fish {
            name: actor.name().to_string(),
            position: ue_point(ch.vector("Location").unwrap_or([0.0; 3])),
            yaw: ue_yaw_to_bevy(ch.rotator("Rotation").unwrap_or([0; 3])[1]),
            npc_type,
            roam: f("m_fRoamingRadius", 300.0) * UNIT,
            speed: f("m_fSpeed", 300.0) * UNIT,
            attack_speed: f("m_fAttackSpeed", 600.0) * UNIT,
            damage,
            bite_interval: f("m_fMinTimeBetweenBites", 0.3),
            bite_distance: f("m_fBiteDistanceFromCam", 60.0) * UNIT,
            consume: f("m_fConsumeCorpseTime", 50.0),
            swim: name("m_SwimmingAnimName", "Fish_Swim"),
            bites,
            eat: name("m_EatingAnimName", "Fish_Eating"),
            death: name("m_DeathAnimName", "Fish_Death"),
        });
        self.actor_ref(actor);
        Ok(())
    }

    /// `Dis_ContactSystem`'s sword against a body: the blood (`DisContactSub_Particle`'s
    /// `m_pParticleTemplate`) and the blood on the lens (`DisContactSub_CameraEffectEmitter`:
    /// its effect class's `PS_CameraEffect`, within `m_fAffectingDistance`, by
    /// `m_fAffectingProbability`).
    fn blade_blood(&mut self) -> Option<BladeBlood> {
        let cs = self.assets.find("Phy_ContactSystem.ContactSystem.Dis_ContactSystem")?;
        let props = cs.props().ok()?;
        let (count, offset, size) = props.array("m_Intersections")?;
        let pkg = cs.pkg.clone();
        let name = |o: Option<i32>| o.filter(|o| *o != 0).map(|o| pkg.obj_path(o)).map(|p| p.rsplit('.').next().unwrap_or(&p).to_string()).unwrap_or_default();
        let it = parse_struct_array(&pkg, offset, size, count)
            .unwrap_or_default()
            .into_iter()
            .find(|it| name(it.object("m_pStriker")) == "DisContactType_Sword" && name(it.object("m_pStruck")) == "DisContactType_Body")?;
        let (c, o, s) = it.array("m_SubIntersections")?;
        let mut out = BladeBlood { system: u32::MAX, lens: None };
        for sub in parse_struct_array(&pkg, o, s, c).unwrap_or_default() {
            let Some(obj) = sub.object("m_pSubIntersectObj").filter(|o| *o != 0).and_then(|o| self.assets.resolve(&pkg, o)) else { continue };
            match name(sub.object("m_pClass")).as_str() {
                "DisContactSub_Particle" => {
                    if let Some(id) = self.chain(&obj).obj(self.assets, "m_pParticleTemplate").and_then(|ps| self.particle_system(&ps)) {
                        out.system = id;
                    }
                }
                "DisContactSub_CameraEffectEmitter" => {
                    let Ok(op) = obj.props() else { continue };
                    let Some(Value::Struct(_, info)) = op.get("m_CameraEffectEmitterInfo").cloned() else { continue };
                    let info = Props(info);
                    let Some(cls) = info.object("m_pEffectClass").filter(|o| *o != 0).and_then(|o| self.assets.resolve(&obj.pkg, o)) else { continue };
                    let cname = cls.name().to_string();
                    let def = ["DishonoredGame", "Engine", "GameFramework"].iter().find_map(|p| self.assets.find(&format!("{p}.Default__{cname}")));
                    if let Some(id) = def.and_then(|d| self.chain(&d).obj(self.assets, "PS_CameraEffect")).and_then(|ps| self.particle_system(&ps)) {
                        out.lens = Some((id, info.float("m_fAffectingDistance").unwrap_or(500.0) * UNIT, info.float("m_fAffectingProbability").unwrap_or(0.3)));
                    }
                }
                _ => {}
            }
        }
        (out.system != u32::MAX).then_some(out)
    }

    /// `NPC_Common.SeveredLimbsInfo.SLInfo_Default`: each break bone's blood when the limb is
    /// severed (`m_bUseWhenSevered`), and `m_CameraEffectEmitterInfoWhenSevered`'s lens effect.
    fn severed_limbs(&mut self) -> Option<SeveredLimbs> {
        let info = self.assets.find("NPC_Common.SeveredLimbsInfo.SLInfo_Default")?;
        let props = info.props().ok()?;
        let pkg = info.pkg.clone();
        let mut out = SeveredLimbs::default();
        let (count, offset, size) = props.array("m_SeveredLimbInfo")?;
        for limb in parse_struct_array(&pkg, offset, size, count).unwrap_or_default() {
            let mut sl = SeveredLimb { bone: limb.name("m_BoneBreakName").unwrap_or_default().to_string(), effects: Vec::new() };
            if let Some((c, o, s)) = limb.array("m_ParticleEffects") {
                for fx in parse_struct_array(&pkg, o, s, c).unwrap_or_default() {
                    if !fx.bool("m_bUseWhenSevered").unwrap_or(false) {
                        continue;
                    }
                    let Some(ps) = fx.object("m_pParticleEmitter").filter(|o| *o != 0).and_then(|o| self.assets.resolve(&pkg, o)) else { continue };
                    if let Some(id) = self.particle_system(&ps) {
                        sl.effects.push((fx.name("m_LocationName").unwrap_or_default().to_string(), id, fx.bool("m_bAttach").unwrap_or(false)));
                    }
                }
            }
            out.limbs.push(sl);
        }
        if let Some(Value::Struct(_, lens)) = props.get("m_CameraEffectEmitterInfoWhenSevered").cloned() {
            let lens = Props(lens);
            if let Some(cls) = lens.object("m_pEffectClass").filter(|o| *o != 0).and_then(|o| self.assets.resolve(&pkg, o)) {
                let cname = cls.name().to_string();
                let def = ["DishonoredGame", "Engine", "GameFramework"].iter().find_map(|p| self.assets.find(&format!("{p}.Default__{cname}")));
                if let Some(id) = def.and_then(|d| self.chain(&d).obj(self.assets, "PS_CameraEffect")).and_then(|ps| self.particle_system(&ps)) {
                    out.lens = Some((id, lens.float("m_fAffectingDistance").unwrap_or(450.0) * UNIT, lens.float("m_fAffectingProbability").unwrap_or(1.0)));
                }
            }
        }
        Some(out)
    }

    /// An NPC anim set's severing notifies (`DishonoredNotify_SeverLimb`) into the scene.
    /// What a set's clips' notifies mark: the blade trailing (`DisNotify_Trails`), the blow's
    /// zone (`DishonoredNotify_AttackZone`), when the attack may be broken off
    /// (`DishonoredNotify_AttackInterruptable`).
    fn clip_marks(&mut self, set: &Obj) {
        let Ok(sp) = set.props() else { return };
        let set_name = set.path();
        if self.scene.clip_marks.iter().any(|m| m.set == set_name) {
            return;
        }
        for q in object_array(&set.pkg, &sp, "Sequences") {
            let Some(seq) = self.assets.resolve(&set.pkg, q) else { continue };
            let Ok(qp) = seq.props() else { continue };
            let clip = qp.name("SequenceName").unwrap_or_default().to_string();
            let len = qp.float("SequenceLength").unwrap_or(0.0);
            let mut m = ClipMarks { set: set_name.clone(), clip, ..Default::default() };
            // (its timed notifies, and those it fires as it starts and as it ends:
            // `m_NotifiesAtAnimStart`, `m_NotifiesAtAnimEnd`)
            let mut all = Vec::new();
            for (array, at) in [("Notifies", None), ("m_NotifiesAtAnimStart", Some(0.0)), ("m_NotifiesAtAnimEnd", Some(len))] {
                let Some((c, o, sz)) = qp.array(array) else { continue };
                all.extend(parse_struct_array(&seq.pkg, o, sz, c).unwrap_or_default().into_iter().map(|n| (n, at)));
            }
            for (n, at) in all {
                let Some(no) = n.object("Notify").filter(|o| *o > 0) else { continue };
                let (t, d) = (at.unwrap_or_else(|| n.float("Time").unwrap_or(0.0)), n.float("Duration").unwrap_or(0.0));
                match seq.pkg.class_name(no).as_str() {
                    "DisNotify_Trails" if d > 0.0 => m.trails.push((t, d)),
                    "DishonoredNotify_AttackZone" if m.zone.is_none() => m.zone = Some((t, d)),
                    "DishonoredNotify_AttackInterruptable" if m.interruptible == 0.0 => m.interruptible = t,
                    "DisNotify_DropItem" => m.drops.push(t),
                    "DishonoredNotify_Ragdoll" if m.ragdoll.is_none() => m.ragdoll = Some(t),
                    "DishonoredNotify_FireDialogHook" => {
                        // (`m_DialogHook`, `eDisDialogHookAnimNotify`: the dying's cry when unset)
                        let h = upk::read_object(&seq.pkg, no).ok().and_then(|o| o.props.name("m_DialogHook").map(str::to_string)).unwrap_or_default();
                        let hook = match h.trim_start_matches("eDisDialogHookAnimNotify_") {
                            "" | "CombatDying" => "COMBAT_DYING",
                            "CombatOuch_Small" => "COMBAT_OUCH_SMALL",
                            "CombatOuch_Big" => "COMBAT_OUCH_BIG",
                            "StealthKilled" => "COMBAT_BACKSTABBED",
                            "CombatThreatVersus" => "COMBAT_THREAT_VERSUS",
                            "PossessionConfused" => "COMBAT_CONFUSION",
                            "TauntGesture" => "COMBAT_TAUNT_WITH_GESTURE",
                            "WeeperMoan" => "WEEP_MOAN",
                            _ => "",
                        };
                        if !hook.is_empty() {
                            m.hooks.push((t, hook.to_string()));
                        }
                    }
                    "DisNotify_FootPlacement" => {
                        let on = upk::read_object(&seq.pkg, no).ok().and_then(|o| o.props.bool("m_bEnableFootPlacement")).unwrap_or(false);
                        m.feet.push((t, on));
                    }
                    _ => {}
                }
            }
            m.feet.sort_by(|a, b| a.0.total_cmp(&b.0));
            m.hooks.sort_by(|a, b| a.0.total_cmp(&b.0));
            if !m.trails.is_empty() || m.zone.is_some() || !m.drops.is_empty() || m.ragdoll.is_some() || !m.feet.is_empty() || !m.hooks.is_empty() {
                self.scene.clip_marks.push(m);
            }
        }
    }

    fn npc_severs(&mut self, set: &Obj) {
        let Ok(sp) = set.props() else { return };
        for q in object_array(&set.pkg, &sp, "Sequences") {
            let Some(seq) = self.assets.resolve(&set.pkg, q) else { continue };
            let Ok(qp) = seq.props() else { continue };
            let clip = qp.name("SequenceName").unwrap_or_default().to_string();
            let Some((c, o, sz)) = qp.array("Notifies") else { continue };
            for n in parse_struct_array(&seq.pkg, o, sz, c).unwrap_or_default() {
                let Some(no) = n.object("Notify").filter(|o| *o > 0) else { continue };
                if seq.pkg.class_name(no) != "DishonoredNotify_SeverLimb" || self.scene.npc_severs.iter().any(|s| s.clip == clip) {
                    continue;
                }
                let Ok(np) = upk::read_object(&seq.pkg, no).map(|o| o.props) else { continue };
                self.scene.npc_severs.push(NpcSever {
                    clip: clip.clone(),
                    time: n.float("Time").unwrap_or(0.0),
                    bone: np.name("m_SeverBoneName").unwrap_or("neck_jnt").to_string(),
                    impulse: np.float("m_fImpulseStrength").unwrap_or(0.0),
                    ragdoll: np.bool("m_bGoToRagdoll").unwrap_or(false),
                });
            }
        }
    }

    /// The contact system's intersections of a striking contact type, against the world, a
    /// body and the world's surfaces.
    fn impacts(&mut self, striker: &str) -> Vec<Impact> {
        if self.contacts.is_none() {
            self.contacts = Some(self.contact_table());
        }
        let entries: Vec<(String, [Option<Obj>; 3])> = self
            .contacts
            .as_ref()
            .map(|c| c.iter().filter(|((s, _), _)| s == striker).map(|((_, t), subs)| (t.clone(), subs.clone())).collect())
            .unwrap_or_default();
        let mut out = Vec::new();
        for (struck, [sound, particle, noise]) in entries {
            let mut im = Impact { against: struck.trim_start_matches("DisContactType_").to_string(), min_volume: 0.3, max_speed: 10.0, ..Default::default() };
            if let Some(s) = sound {
                let c = self.chain(&s);
                im.sound = c.obj_path("m_pSoundEvent").map(|p| p.rsplit('.').next().unwrap_or(&p).to_string()).unwrap_or_default();
                if let Some(Value::Struct(_, v)) = c.get("m_VolumeSettings") {
                    let v = Props(v.clone());
                    im.scale_with_speed = v.bool("m_bScaleWithSpeed").unwrap_or(false);
                    im.min_speed = v.float("m_fMinSpeed").unwrap_or(0.0) * UNIT;
                    im.max_speed = v.float("m_fMaxSpeed").unwrap_or(1000.0) * UNIT;
                    im.min_volume = v.float("m_fMinVolumeScale").unwrap_or(0.3);
                }
            }
            if let Some(p) = particle {
                if let Some(ps) = self.chain(&p).obj(self.assets, "m_pParticleTemplate") {
                    im.particle = self.particle_system(&ps);
                }
            }
            if let Some(n) = noise {
                let c = self.chain(&n);
                im.noise = noise_meters(c.name("m_NoiseLoudness").unwrap_or(""));
                im.threatening = c.name("m_NoiseContext").is_some_and(|x| x.contains("Threatening"));
            }
            if !im.sound.is_empty() || im.particle.is_some() || im.noise > 0.0 {
                out.push(im);
            }
        }
        out.sort_by(|a, b| a.against.cmp(&b.against));
        out
    }

    /// `Phy_ContactSystem.ContactSystem.Dis_ContactSystem`: its intersections by contact types.
    fn contact_table(&mut self) -> HashMap<(String, String), [Option<Obj>; 3]> {
        let mut out = HashMap::new();
        let Some(cs) = self.assets.find("Phy_ContactSystem.ContactSystem.Dis_ContactSystem") else {
            log::warn!("contact system not found");
            return out;
        };
        let Ok(props) = cs.props() else { return out };
        let Some((count, offset, size)) = props.array("m_Intersections") else { return out };
        let pkg = cs.pkg.clone();
        let name = |o: Option<i32>| o.filter(|o| *o != 0).map(|o| pkg.obj_path(o)).map(|p| p.rsplit('.').next().unwrap_or(&p).to_string()).unwrap_or_default();
        for it in parse_struct_array(&pkg, offset, size, count).unwrap_or_default() {
            let (striker, struck) = (name(it.object("m_pStriker")), name(it.object("m_pStruck")));
            let mut subs: [Option<Obj>; 3] = [None, None, None];
            if let Some((c, o, s)) = it.array("m_SubIntersections") {
                for sub in parse_struct_array(&pkg, o, s, c).unwrap_or_default() {
                    let class = name(sub.object("m_pClass"));
                    let slot = match class.as_str() {
                        "DisContactSub_SoundCue" => 0,
                        "DisContactSub_Particle" => 1,
                        "DisContactSub_AINoise" => 2,
                        _ => continue,
                    };
                    subs[slot] = sub.object("m_pSubIntersectObj").filter(|o| *o != 0).and_then(|o| self.assets.resolve(&pkg, o));
                }
            }
            out.insert((striker, struck), subs);
        }
        log::debug!("contact system: {} intersections", out.len());
        out
    }

    /// A FaceFX actor (a head's face), cooked once per map.
    fn facefx_actor(&mut self, fx: &Obj) -> Option<u32> {
        if let Some(&i) = self.facefx_ids.get(&fx.key()) {
            return Some(i);
        }
        let od = upk::read_object(&fx.pkg, fx.idx).ok()?;
        match facefx::read_actor(&od.reader.data[od.reader.pos..]) {
            Ok(a) => {
                let i = self.facefx.actors.len() as u32;
                self.facefx.actors.push(a);
                self.facefx_ids.insert(fx.key(), i);
                Some(i)
            }
            Err(e) => {
                log::warn!("{}: {e:#}", fx.path());
                None
            }
        }
    }

    /// The last of a breakable's steps (a prop's `m_Steps`, a door's `m_DoorBreakSteps`): its
    /// sound, AI noise, effect and pieces.
    fn last_break_step(&mut self, tc: &Chain, prop: &str) -> Option<BreakStep> {
        let Some((pkg, Value::Array { count, offset, size })) = tc.get_pkg(prop) else { return None };
        let pkg = pkg.clone();
        let steps = parse_struct_array(&pkg, *offset, *size, *count).unwrap_or_default();
        let st = steps.last()?;
        let res = |o: Option<i32>| o.filter(|o| *o != 0).and_then(|o| self.assets.resolve(&pkg, o));
        let sound = res(st.object("m_pSoundEvent")).map(|o| o.name().to_string()).unwrap_or_default();
        let noise = noise_meters(st.name("m_AINoiseLoudness").unwrap_or(""));
        let threatening = st.name("m_AINoiseContext").is_some_and(|c| c.contains("Threatening") || c.contains("Danger"));
        let particle = res(st.object("m_pParticleSystem")).and_then(|ps| self.particle_system(&ps));
        let particle_offset = ue_dir(st.vector("m_ParticleSystemOffset").unwrap_or([0.0; 3])).map(|v| v * UNIT);
        let mut chunks = Vec::new();
        if let Some((count, offset, size)) = st.array("m_Chunks") {
            for c in parse_struct_array(&pkg, offset, size, count).unwrap_or_default() {
                if let Some(m) = res(c.object("m_pStaticMesh")) {
                    if let Ok(Some(mm)) = self.cook_mesh(&m) {
                        chunks.push(mm);
                    }
                }
            }
        }
        let blast = res(st.object("m_pExplosion")).map(|x| self.blast(&x));
        Some(BreakStep { sound, noise, threatening, particle, particle_offset, chunks, blast })
    }

    /// A physics prop's or breakable's tweak (`m_pMovableTweaks` / `m_pBreakableTweaks`).
    fn movable(&mut self, actor: &Obj, instance: u32) {
        let ch = self.chain(actor);
        let Some(tweak) = ch.obj(self.assets, "m_pMovableTweaks").or_else(|| ch.obj(self.assets, "m_pBreakableTweaks")).or_else(|| ch.obj(self.assets, "m_pWhaleOilTweaks")) else { return };
        let tank = (actor.class() == "DisWhaleOilBattery").then(|| actor.name().to_string());
        let fixed_default = actor.class() == "DishonoredBreakableNavBlock";
        let m = self.movable_of(&tweak, instance, tank, fixed_default);
        self.scene.movables.push(m);
    }

    /// A movable of its tweak (`DisTweaks_Movable`, `_StaticBreakable`, `_WhaleOilBattery`) on
    /// an instance: a whale oil tank's when named one.
    fn movable_of(&mut self, tweak: &Obj, instance: u32, tank: Option<String>, fixed_default: bool) -> Movable {
        let is_tank = tank.is_some();
        let tc = self.chain(tweak);
        let obj_name = |c: &Chain, n: &str| c.obj_path(n).map(|p| p.rsplit('.').next().unwrap_or(&p).to_string()).unwrap_or_default();
        let (weight, damage) = match tc.get("m_MovableParams") {
            Some(Value::Struct(_, s)) => {
                let p = Props(s.clone());
                let w = match p.name("m_WeightClass") {
                    Some("MWC_Small") => 1,
                    Some("MWC_Medium") => 2,
                    Some("MWC_Large") => 3,
                    _ => 0,
                };
                (w, p.int("m_Damage").unwrap_or(5) as f32)
            }
            _ => (0, 5.0),
        };
        let name = tc
            .obj(self.assets, "m_pInteractableTweaks")
            .and_then(|it| self.chain(&it).name("m_Name").map(str::to_string))
            .unwrap_or_default();
        let surface = obj_name(&tc, "m_pContactTypeOverride").trim_start_matches("DisContactType_").trim_start_matches("Env_").to_string();
        // the last step: what's left of it
        let breaks = self.last_break_step(&tc, "m_Steps");
        let striker = tc.obj_path("m_pContactTypeOverride").map(|p| p.rsplit('.').next().unwrap_or(&p).to_string()).unwrap_or_else(|| "DisContactType_Env_Wood".into());
        let impacts = self.impacts(&striker);
        Movable {
            impacts,
            instance,
            tank,
            name,
            // (a whale oil tank is always Corvo's to carry)
            interactable: tc.bool("m_bInteractable").unwrap_or(false) || is_tank,
            fixed: tc.bool("m_bFixed").unwrap_or(fixed_default),
            weight,
            damage,
            health: tc.float("m_Health").unwrap_or(0.0),
            threshold: tc.float("m_DamageThreshold").unwrap_or(1.0),
            break_speed: tc.float("m_fMinSpeedToBeDamaged").unwrap_or(1200.0) * UNIT,
            break_speed_drop: tc.float("m_fMinSpeedToBeDamagedOnDrop").unwrap_or(3000.0) * UNIT,
            grab_sound: obj_name(&tc, "m_pGrabSound"),
            surface,
            breaks,
            joints: Vec::new(),
            charges: is_tank.then(|| {
                let f = |k: &str, d: f32| tc.float(k).unwrap_or(d);
                [f("m_InitialNumberOfCharges", 50.0), f("m_PawnChargeCost", 4.0), f("m_AmbientAnimalChargeCost", 1.0), f("m_WatchtowerChargeCost", 1.0), f("m_fExplosionChainTimer", 0.33)]
            }),
            pool: None,
        }
    }

    /// Dunwall City Trials' falling whale oil tanks (Oil Drop): the scripts' actor factories
    /// that make `DisDLC05WhaleOilBattery` of a tank tweak draw from a pool of each tweak,
    /// hidden instances of its mesh in its colour (`m_MaterialOverrides`), each a movable of
    /// the tweak's and an actor of the scripts' (the damage events they attach go to it); the
    /// op lists its pool's actors (`factory_pool`, `factory_pool_actors`).
    fn factory_tanks(&mut self) {
        const POOL: usize = 8;
        let mut pools: HashMap<String, u32> = HashMap::new();
        for i in 0..self.scene.kismet.ops.len() {
            if !matches!(self.scene.kismet.ops[i].class.as_str(), "SeqAct_ActorFactory" | "SeqAct_ActorFactoryEx") {
                continue;
            }
            let Some(KVal::Str(path)) = self.scene.kismet.ops[i].props.get("Factory").cloned() else { continue };
            let Some(fac) = self.assets.find(&path).filter(|f| f.class() == "DisActorFactoryTweakObj") else { continue };
            let Some(tw) = self.chain(&fac).obj(self.assets, "m_pTweakObject") else { continue };
            if !tw.class().contains("WhaleOilBattery") {
                continue;
            }
            let key = tw.path();
            let pool = match pools.get(&key) {
                Some(&p) => p,
                None => {
                    let Some(made) = self.tank_pool(&tw, POOL) else { continue };
                    let p = self.scene.tank_pools.len() as u32;
                    self.scene.tank_pools.push(made);
                    pools.insert(key, p);
                    p
                }
            };
            let actors = self.scene.tank_pools[pool as usize].actors.iter().map(|a| KVal::Int(*a as i32)).collect();
            let props = &mut self.scene.kismet.ops[i].props;
            props.insert("factory_pool".into(), KVal::Int(pool as i32));
            props.insert("factory_pool_actors".into(), KVal::List(actors));
        }
        if !self.scene.tank_pools.is_empty() {
            log::info!("{} pools of factory tanks", self.scene.tank_pools.len());
        }
    }

    /// A tank tweak's pool: its mesh, coloured, `n` times over.
    fn tank_pool(&mut self, tw: &Obj, n: usize) -> Option<TankPool> {
        let tc = self.chain(tw);
        let mesh_obj = tc.obj(self.assets, "m_pStaticMesh")?;
        let (mesh, mut materials) = self.cook_mesh(&mesh_obj).ok()??;
        // (its colour: the materials it puts on the mesh's, slot by slot)
        if let Some((pkg, Value::Array { count, offset, .. })) = tc.get_pkg("m_MaterialOverrides") {
            let (pkg, count, offset) = (pkg.clone(), *count, *offset);
            for k in 0..count.min(materials.len()) {
                let at = offset + 4 * k;
                let Some(b) = pkg.data.get(at..at + 4) else { break };
                let r = i32::from_le_bytes([b[0], b[1], b[2], b[3]]);
                if let Some(m) = (r != 0).then(|| self.assets.resolve(&pkg, r)).flatten() {
                    materials[k] = self.material(&m);
                }
            }
        }
        let short = tw.path().rsplit('.').next().unwrap_or_default().to_string();
        let kind = short.trim_start_matches("WhaleOilBattery_").trim_end_matches("_twk").to_string();
        let pool_index = self.scene.tank_pools.len() as u32;
        let mut pool = TankPool { tweak: short.clone(), kind, movables: Vec::new(), actors: Vec::new() };
        for k in 0..n {
            let name = format!("{short}_{k}");
            let instance = self.scene.instances.len() as u32;
            self.scene.instances.push(Instance {
                mesh,
                materials: materials.clone(),
                transform: Mat4::IDENTITY.to_cols_array(),
                visible: false,
                collide: false,
                cast_shadow: true,
                dynamic: true,
                actor: name.clone(),
                class: "DisFactoryItem".into(),
                lightmap: None,
                sun: 2,
                sun_shadow: None,
                light_shadows: Vec::new(),
                irrelevant_lights: Vec::new(),
                reflect: 0,
            });
            let mut m = self.movable_of(tw, instance, Some(name.clone()), false);
            m.pool = Some(pool_index);
            // (shot down, never carried)
            m.interactable = false;
            pool.movables.push(self.scene.movables.len() as u32);
            self.scene.movables.push(m);
            pool.actors.push(self.scene.kismet.actors.len() as u32);
            self.scene.kismet.actors.push(KActor { name, class: "DisDLC05WhaleOilBattery".into(), instances: vec![instance], ..Default::default() });
        }
        Some(pool)
    }

    /// A `DisFogComponent` as the fog pass uses it.
    fn fog_layer(&mut self, comp: &Obj) -> Option<FogLayer> {
        let cc = self.chain(comp);
        let c = cc.color("LightColor").unwrap_or([255, 255, 255, 0]);
        let srgb = |v: u8| {
            let v = v as f32 / 255.0;
            if v <= 0.04045 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
        };
        let origin = cc.float("Origin").unwrap_or(0.0);
        let height = cc.float("Height").unwrap_or(20.0);
        let (lo, hi) = if height >= 0.0 { (origin, origin + height) } else { (origin + height, origin) };
        // the curve: the raw bytes the editor stored, else the LUT texture's middle row
        let mut lut: Vec<f32> = match cc.get_pkg("m_RawLut") {
            Some((pkg, Value::Array { .. })) => {
                let pkg = pkg.clone();
                cc.0.iter().find_map(|(_, p)| p.array("m_RawLut")).map(|(n, off, _)| pkg.data[off..off + n].iter().map(|b| *b as f32 / 255.0).collect()).unwrap_or_default()
            }
            _ => Vec::new(),
        };
        if lut.is_empty() {
            if let Some(t) = cc.obj(self.assets, "FogLUT") {
                if let Ok(tex) = upk::texture::read_texture2d(&t.pkg, t.idx, &tfc_for(&t), 512) {
                    let m = &tex.mips[0];
                    if let Ok(rgba) = decode_rgba(tex.format, m.width, m.height, &m.data) {
                        let row = (m.height / 2) as usize * m.width as usize * 4;
                        lut = (0..m.width as usize).map(|x| rgba[row + x * 4] as f32 / 255.0).collect();
                    }
                }
            }
        }
        let near = cc.float("NearPlane").unwrap_or(2000.0);
        let far = cc.float("FarPlane").unwrap_or(10000.0);
        Some(FogLayer {
            color: [srgb(c[0]), srgb(c[1]), srgb(c[2])],
            opacity: cc.float("Opacity").unwrap_or(0.8),
            min_height: lo * UNIT,
            max_height: hi * UNIT,
            near: near * UNIT,
            far: far * UNIT,
            no_fog: cc.float("NoFogPlane").unwrap_or(400000.0) * UNIT,
            height_density: cc.float("HeightDensityFactor").unwrap_or(1.0),
            sun: cc.bool("bIsSun").unwrap_or(false).then(|| cc.float("SunPower").unwrap_or(16.0)),
            lut,
            interior: cc.bool("bInteriorFog").unwrap_or(false),
            enabled: cc.bool("bEnabled").unwrap_or(true),
            level: self.scene.levels.len() as u32,
        })
    }

    /// A water volume's current, splashes, sounds and the look under water.
    fn water(&mut self, ach: &Chain) -> Water {
        let mut w = Water { current: ue_point(ach.vector("ZoneVelocity").unwrap_or([0.0; 3])), friction: ach.float("FluidFriction").unwrap_or(0.3), ..Default::default() };
        let Some(info) = ach.obj(self.assets, "m_pWaterVolumeInfo") else { return w };
        let ic = self.chain(&info);
        let event = |pkg: &upk::Package, p: &Props| match p.get("m_pSoundEvent") {
            Some(Value::Object(o)) if *o != 0 => pkg.obj_path(*o).rsplit('.').next().unwrap_or_default().to_string(),
            _ => String::new(),
        };
        if let Some((pkg, Value::Array { .. })) = ic.get_pkg("m_WaterEntrySettings") {
            let pkg = pkg.clone();
            let arr = ic.0.iter().find_map(|(_, p)| p.array("m_WaterEntrySettings"));
            if let Some((c, off, sz)) = arr {
                for e in parse_struct_array(&pkg, off, sz, c).unwrap_or_default() {
                    let system = match e.get("pParticleSystem") {
                        Some(Value::Object(o)) if *o != 0 => self.assets.resolve(&pkg, *o).and_then(|ps| self.particle_system(&ps)),
                        _ => None,
                    };
                    w.entry.push((e.float("fVelocityThreshold").unwrap_or(0.0) * UNIT, event(&pkg, &e), system));
                }
            }
        }
        if let Some((pkg, Value::Array { .. })) = ic.get_pkg("m_WaterExitSettings") {
            let pkg = pkg.clone();
            if let Some((c, off, sz)) = ic.0.iter().find_map(|(_, p)| p.array("m_WaterExitSettings")) {
                if let Some(e) = parse_struct_array(&pkg, off, sz, c).unwrap_or_default().first() {
                    w.exit = event(&pkg, e);
                }
            }
        }
        let name = |n: &str| ic.obj_path(n).map(|p| p.rsplit('.').next().unwrap_or_default().to_string()).unwrap_or_default();
        w.underwater = (name("m_pUnderwaterPlaySoundEvent"), name("m_pUnderwaterStopSoundEvent"));
        if let Some(Value::Struct(_, pp)) = ic.get("m_PpOverride") {
            w.grade = Some(post_process(&Props(pp.clone())));
        }
        if let Some(fog) = ic.obj(self.assets, "m_FogComponent") {
            w.fog_layer = self.fog_layer(&fog);
            let fc = self.chain(&fog);
            let c = fc.color("LightColor").unwrap_or([40, 50, 50, 255]);
            let srgb = |v: u8| {
                let v = v as f32 / 255.0;
                if v <= 0.04045 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
            };
            let near = fc.float("NearPlane").unwrap_or(0.0) * UNIT;
            let far = fc.float("FarPlane").unwrap_or(1500.0) * UNIT;
            w.fog = Some(Fog { color: [srgb(c[0]), srgb(c[1]), srgb(c[2])], start: near.max(0.0), end: far.max(near + 1.0), density: fc.float("Opacity").unwrap_or(0.9).clamp(0.0, 1.0) });
        }
        w
    }

    /// An `AkAmbientSound(Movable)` actor: its Wwise event at its location.
    fn ambient_sound(&mut self, actor: &Obj) -> Result<()> {
        let ch = self.chain(actor);
        let Some(ev) = ch.obj(self.assets, "PlayEvent") else { return Ok(()) };
        let loc = ch.vector("Location").unwrap_or([0.0; 3]);
        let index = self.scene.ambient_sounds.len() as u32;
        self.actor_ref(actor).sound = Some(index);
        self.scene.ambient_sounds.push(AmbientSound {
            event: ev.name().to_string(),
            position: ue_point(loc),
            auto_play: ch.bool("bAutoPlay").unwrap_or(true),
        });
        Ok(())
    }

    /// A placed particle system (usually an `Emitter` actor's component).
    fn particle_component(&mut self, comp: &Obj, actor: &Obj, ue_world: Mat4) -> Result<()> {
        let cch = self.chain(comp);
        let Some(template) = cch.obj(self.assets, "Template") else { return Ok(()) };
        let Some(system) = self.particle_system(&template) else { return Ok(()) };
        let ach = self.chain(actor);
        let active = cch.bool("bAutoActivate").unwrap_or(true) && !ach.bool("bHidden").unwrap_or(false) && !cch.bool("HiddenGame").unwrap_or(false);
        let max_draw = cch.float("MaxDrawDistance").unwrap_or(0.0) * UNIT;
        let index = self.scene.particles.len() as u32;
        self.actor_ref(actor).particles.push(index);
        self.scene.particles.push(ParticleInstance { system, transform: ue_to_bevy(ue_world).to_cols_array(), active, max_draw });
        Ok(())
    }

    fn particle_system(&mut self, ps: &Obj) -> Option<u32> {
        let key = ps.key();
        if let Some(v) = self.particle_system_ids.get(&key) {
            return *v;
        }
        let r = self.cook_particle_system(ps);
        if let Err(e) = &r {
            log::debug!("particle system {}: {e:#}", ps.path());
        }
        let id = r.ok().flatten();
        self.particle_system_ids.insert(key, id);
        id
    }

    fn cook_particle_system(&mut self, ps: &Obj) -> Result<Option<u32>> {
        let props = ps.props()?;
        let mut def = ParticleSystemDef { name: ps.path(), emitters: Vec::new() };
        for e in object_array(&ps.pkg, &props, "Emitters") {
            let Some(em) = self.assets.resolve(&ps.pkg, e) else { continue };
            if !em.class().contains("Emitter") {
                continue;
            }
            let ep = em.props()?;
            // highest-detail LOD
            let Some(lod) = object_array(&em.pkg, &ep, "LODLevels").first().and_then(|&l| self.assets.resolve(&em.pkg, l)) else { continue };
            let lp = lod.props()?;
            if lp.bool("bEnabled") == Some(false) {
                continue;
            }
            let module = |c: &mut Self, o: Option<Obj>| -> Option<(Obj, ModuleDef)> {
                let o = o?;
                let p = o.props().ok()?;
                let d = c.class_defaults(&o.class());
                Some((o.clone(), particle_module(&o, &p, d.as_deref())))
            };
            let Some((req_obj, required)) = module(self, lp.object("RequiredModule").and_then(|r| self.assets.resolve(&lod.pkg, r))) else { continue };
            let mut emitter = EmitterDef { name: ep.name("EmitterName").unwrap_or("").to_string(), required, ..Default::default() };
            let rp = req_obj.props()?;
            emitter.material = match rp.object("Material").and_then(|m| self.assets.resolve(&req_obj.pkg, m)) {
                Some(m) => self.material(&m),
                None => 0,
            };
            if let Some((sp_obj, spawn)) = module(self, lp.object("SpawnModule").and_then(|r| self.assets.resolve(&lod.pkg, r))) {
                if let Ok(sp) = sp_obj.props() {
                    if let Some((c, off, sz)) = sp.array("BurstList") {
                        for b in parse_struct_array(&sp_obj.pkg, off, sz, c).unwrap_or_default() {
                            let count = match b.get("Count") {
                                Some(Value::Int(i)) => *i,
                                _ => 0,
                            };
                            emitter.bursts.push((b.float("Time").unwrap_or(0.0), count.max(0) as u32));
                        }
                    }
                }
                emitter.spawn = spawn;
            }
            // (the LOD's type data: mesh particles, with the mesh's own turn)
            let type_data = lp.object("TypeDataModule").filter(|t| *t != 0);
            for m in object_array(&lod.pkg, &lp, "Modules").into_iter().chain(type_data) {
                let Some(mo) = self.assets.resolve(&lod.pkg, m) else { continue };
                let Ok(mp) = mo.props() else { continue };
                if mp.bool("bEnabled") == Some(false) {
                    continue;
                }
                // a mesh particle's own material (`ParticleModuleMeshMaterial`, or the default of
                // `ParticleModuleMaterialByParameter`)
                if mo.class() == "ParticleModuleMeshMaterial" || mo.class() == "ParticleModuleMaterialByParameter" {
                    let list = if mo.class() == "ParticleModuleMeshMaterial" { "MeshMaterials" } else { "DefaultMaterials" };
                    if let Some(m) = object_array(&mo.pkg, &mp, list).into_iter().find(|&o| o != 0).and_then(|o| self.assets.resolve(&mo.pkg, o)) {
                        emitter.material = self.material(&m);
                    }
                }
                if mo.class() == "ParticleModuleEventGenerator" {
                    if let Some((c, off, sz)) = mp.array("Events") {
                        for ev in parse_struct_array(&mo.pkg, off, sz, c).unwrap_or_default() {
                            let kind = match ev.name("Type").unwrap_or("") {
                                "EPET_Spawn" => 0,
                                "EPET_Death" => 1,
                                "EPET_Collision" => 2,
                                _ => 3,
                            };
                            if let Some(n) = ev.name("CustomName").filter(|n| !n.is_empty() && *n != "None") {
                                emitter.events.push((kind, n.to_string()));
                            }
                        }
                    }
                    continue;
                }
                if mo.class().contains("TypeDataMesh") {
                    if let Some(mesh) = mp.object("Mesh").and_then(|x| self.assets.resolve(&mo.pkg, x)) {
                        if let Ok(Some((id, _))) = self.cook_mesh(&mesh) {
                            emitter.mesh = Some(id);
                        }
                    }
                    let deg = |n: &str| (mp.float(n).unwrap_or(0.0) * 65536.0 / 360.0) as i32;
                    let r = [deg("Pitch"), deg("Yaw"), deg("Roll")];
                    if r != [0, 0, 0] {
                        let swap = glam::Mat4::from_cols(glam::Vec4::X, glam::Vec4::Z, glam::Vec4::Y, glam::Vec4::W);
                        let m = swap * xform::rot_matrix(r) * swap;
                        emitter.mesh_rotation = Some(glam::Quat::from_mat4(&m).normalize().to_array());
                    }
                    continue;
                }
                let d = self.class_defaults(&mo.class());
                emitter.modules.push(particle_module(&mo, &mp, d.as_deref()));
            }
            def.emitters.push(emitter);
        }
        if def.emitters.is_empty() {
            return Ok(None);
        }
        self.scene.particle_systems.push(def);
        Ok(Some(self.scene.particle_systems.len() as u32 - 1))
    }

    /// Properties of a class's default object (`Default__<Class>`), cached.
    fn class_defaults(&mut self, class: &str) -> Option<Arc<Props>> {
        let path = format!("Default__{class}");
        let obj = self.assets.find(&path)?;
        let key = (Arc::as_ptr(&obj.pkg) as usize, obj.idx);
        if let Some(p) = self.props_cache.get(&key) {
            return Some(p.clone());
        }
        let p = Arc::new(obj.props().ok()?);
        self.props_cache.insert(key, p.clone());
        Some(p)
    }

    /// Trigger actors: their collision cylinder as a volume (an octagonal prism).
    fn trigger(&mut self, actor: &Obj) -> Result<()> {
        let ach = self.chain(actor);
        let loc = Vec3::from(ach.vector("Location").unwrap_or([0.0; 3]));
        let (mut radius, mut height) = (40.0, 40.0);
        if let Some(comp) = ach.obj(self.assets, "CylinderComponent").or_else(|| ach.obj(self.assets, "CollisionComponent")) {
            let cch = self.chain(&comp);
            radius = cch.float("CollisionRadius").unwrap_or(radius);
            height = cch.float("CollisionHeight").unwrap_or(height);
        }
        let mut hull = Vec::with_capacity(16);
        for k in 0..8 {
            let a = k as f32 * std::f32::consts::TAU / 8.0;
            for z in [-height, height] {
                let p = loc + Vec3::new(a.cos() * radius, a.sin() * radius, z);
                hull.push(ue_point(p.to_array()));
            }
        }
        let index = self.scene.volumes.len() as u32;
        self.actor_ref(actor).volume = Some(index);
        self.scene.volumes.push(Volume { kind: actor.class(), name: actor.name().to_string(), hulls: vec![hull], water: None, stealth: None, zone: None, no_unpossess: false });
        Ok(())
    }

    /// Pack all referenced lightmap pairs into LM_PAGE-sized pages (power-of-two buddy
    /// packing, largest first) and rewrite instance lightmap references.
    fn pack_lightmaps(&mut self) {
        if self.lm_pairs.is_empty() {
            self.pack_shadowmaps();
            return;
        }
        let mut order: Vec<u32> = (0..self.lm_pairs.len() as u32).collect();
        order.sort_by_key(|&i| std::cmp::Reverse(self.lm_pairs[i as usize].size.0 * self.lm_pairs[i as usize].size.1));
        // pages: list of free rectangles (x, y, w, h)
        let mut pages: Vec<Vec<(u32, u32, u32, u32)>> = Vec::new();
        let mut entries: Vec<Vec<(LmPair, u32, u32)>> = Vec::new();
        let mut placement: Vec<(u32, u32, u32)> = vec![(0, 0, 0); self.lm_pairs.len()];
        for i in order {
            let pair = self.lm_pairs[i as usize].clone();
            let (w, h) = (pair.size.0.next_power_of_two(), pair.size.1.next_power_of_two());
            let mut placed = None;
            for (pi, free) in pages.iter_mut().enumerate() {
                if let Some(fi) = free.iter().position(|r| r.2 >= w && r.3 >= h) {
                    placed = Some((pi, fi));
                    break;
                }
            }
            let (pi, fi) = match placed {
                Some(p) => p,
                None => {
                    pages.push(vec![(0, 0, LM_PAGE, LM_PAGE)]);
                    entries.push(Vec::new());
                    (pages.len() - 1, 0)
                }
            };
            let r = pages[pi].remove(fi);
            // split the free rect (guillotine): right part and bottom part
            if r.2 > w {
                pages[pi].push((r.0 + w, r.1, r.2 - w, h));
            }
            if r.3 > h {
                pages[pi].push((r.0, r.1 + h, r.2, r.3 - h));
            }
            pages[pi].sort_by_key(|f| (f.2 * f.3, f.1, f.0));
            placement[i as usize] = (pi as u32, r.0, r.1);
            entries[pi].push((pair, r.0, r.1));
        }
        let page_ids: Vec<u32> = entries.iter().map(|e| self.texture(TexJob::LightmapPage(LM_PAGE, e.clone()))).collect();
        let raw_ids: Vec<(u32, u32)> = entries
            .into_iter()
            .map(|e| (self.texture(TexJob::LightmapRaw(LM_PAGE, e.clone(), 0)), self.texture(TexJob::LightmapRaw(LM_PAGE, e, 1))))
            .collect();
        let k = 1.0 / LM_PAGE as f32;
        for inst in self.scene.instances.iter_mut() {
            if let Some(lm) = inst.lightmap.as_mut() {
                let pair = lm.textures[0] as usize;
                let (pi, x, y) = placement[pair];
                let (w, h) = self.lm_pairs[pair].size;
                let (sx, sy) = (w as f32 * k, h as f32 * k);
                let (raw_nac, raw_dmc) = raw_ids[pi as usize];
                lm.textures = vec![page_ids[pi as usize], raw_nac, raw_dmc];
                let p = &self.lm_pairs[pair];
                lm.scales = vec![[p.s_nac[0], p.s_nac[1], p.s_nac[2], 1.0], [p.s_dmc[0], p.s_dmc[1], p.s_dmc[2], 1.0]];
                lm.coord_bias = [lm.coord_bias[0] * sx + x as f32 * k, lm.coord_bias[1] * sy + y as f32 * k];
                lm.coord_scale = [lm.coord_scale[0] * sx, lm.coord_scale[1] * sy];
            }
        }
        let mut hist: std::collections::BTreeMap<(u32, u32), usize> = Default::default();
        for p in &self.lm_pairs {
            *hist.entry(p.size).or_default() += 1;
        }
        log::info!("packed {} lightmaps into {} pages, sizes {:?}", self.lm_pairs.len(), page_ids.len(), hist);
        self.pack_shadowmaps();
    }

    fn pack_shadowmaps(&mut self) {
        if self.shadow_texs.is_empty() {
            return;
        }
        let sizes: Vec<(u32, u32)> = self
            .shadow_texs
            .iter()
            .map(|t| {
                let p = t.props().unwrap_or_default();
                (p.int("SizeX").unwrap_or(64).max(4) as u32, p.int("SizeY").unwrap_or(64).max(4) as u32)
            })
            .collect();
        let mut order: Vec<usize> = (0..self.shadow_texs.len()).collect();
        order.sort_by_key(|&i| std::cmp::Reverse(sizes[i].0 * sizes[i].1));
        let mut pages: Vec<Vec<(u32, u32, u32, u32)>> = Vec::new();
        let mut entries: Vec<Vec<(Obj, u32, u32)>> = Vec::new();
        let mut placement = vec![(0u32, 0u32, 0u32); self.shadow_texs.len()];
        for i in order {
            let (w, h) = (sizes[i].0.next_power_of_two(), sizes[i].1.next_power_of_two());
            if w > LM_PAGE || h > LM_PAGE {
                continue;
            }
            let mut placed = None;
            for (pi, free) in pages.iter().enumerate() {
                if let Some(fi) = free.iter().position(|r| r.2 >= w && r.3 >= h) {
                    placed = Some((pi, fi));
                    break;
                }
            }
            let (pi, fi) = match placed {
                Some(p) => p,
                None => {
                    pages.push(vec![(0, 0, LM_PAGE, LM_PAGE)]);
                    entries.push(Vec::new());
                    (pages.len() - 1, 0)
                }
            };
            let r = pages[pi].remove(fi);
            if r.2 > w {
                pages[pi].push((r.0 + w, r.1, r.2 - w, h));
            }
            if r.3 > h {
                pages[pi].push((r.0, r.1 + h, r.2, r.3 - h));
            }
            pages[pi].sort_by_key(|f| (f.2 * f.3, f.1, f.0));
            placement[i] = (pi as u32, r.0, r.1);
            entries[pi].push((self.shadow_texs[i].clone(), r.0, r.1));
        }
        let page_ids: Vec<u32> = entries.into_iter().map(|e| self.texture(TexJob::ShadowPage(LM_PAGE, e))).collect();
        let k = 1.0 / LM_PAGE as f32;
        for inst in self.scene.instances.iter_mut() {
            let refs = inst.sun_shadow.iter_mut().chain(inst.light_shadows.iter_mut().map(|x| &mut x.1));
            for sh in refs {
                let t = sh.textures[0] as usize;
                let (pi, x, y) = placement[t];
                let (w, h) = sizes[t];
                let (sx, sy) = (w as f32 * k, h as f32 * k);
                sh.textures = vec![page_ids[pi as usize]];
                sh.coord_bias = [sh.coord_bias[0] * sx + x as f32 * k, sh.coord_bias[1] * sy + y as f32 * k];
                sh.coord_scale = [sh.coord_scale[0] * sx, sh.coord_scale[1] * sy];
            }
        }
        log::info!("packed {} static shadow maps into {} pages", self.shadow_texs.len(), page_ids.len());
    }

    fn cook_props(&mut self) {
        // the effects gameplay spawns (the weapons' and powers' tweaks name these)
        const GAME_EFFECTS: &[(&str, &str)] = &[
            ("grenade", "Vfx_Weapon.Effects.Grenade.ps_grenade"),
            ("grenade_water", "Vfx_Weapon.Effects.Grenade.Ps_Grenade_Surfwater_Expl"),
            ("grenade_trail", "Vfx_Weapon.Effects.Grenade.Ps_Grenade_Trail"),
            // (a sword's swing: `DisMeleeExtentTweak.m_ParticleSystemComponent`, an AnimTrail)
            ("sword_trail", "Vfx_Weapon.Effects.Trails.Sword_Trail"),
            ("explosive_bullet", "Vfx_Weapon.Effects.pistol.ps_pistol_explosive"),
            ("pistol_muzzle", "Vfx_Weapon.Effects.pistol.Ps_PlayerPistol_StdMuzzle_01"),
            ("npc_pistol_muzzle", "Vfx_Weapon.Effects.Ps_PistolMuzzle"),
            ("crossbow_muzzle", "Vfx_Weapon.Effects.Ps_CrossBowMuzzle"),
            ("pistol_tracer", "Vfx_Weapon.Effects.Trails.Ps_Pistol_StdTracer_01"),
            ("springrazor", "Vfx_Weapon.Effects.SpringRazor.Ps_SpringRazor"),
            ("springrazor_blood", "Vfx_PhysMat.Blood.SpringRazor.Pfx_BloodSRazorBody"),
            ("flare_trail", "Vfx_Weapon.Effects.FlareBolts.Flare_Bolt_Trail"),
            ("immolation", "Vfx_GamePlay.SpittingFire.ps_immolation"),
            ("shadow_kill", "Vfx_GamePlay.ShadowKill.ps_SK_Leg_Arms"),
            ("electrified", "Vfx_GamePlay.WallLight.ps_electrified"),
            ("rat_gibs", "NPC_Body_Eaten.FX.ps_rat_gibs_body"),
            ("blood_limb", "Vfx_PhysMat.Blood.SeveredLimbs.Pfx_BloodSLimbBody"),
            ("blood_throat", "Vfx_PhysMat.Blood.Fatalities.Pfx_BloodThroat"),
            ("blood_lunge", "Vfx_PhysMat.Blood.Fatalities.Pfx_BloodLunge"),
            ("possess_target", "Vfx_GamePlay.Possession.PossesTarget_Human_01"),
            ("mana_empty", "Vfx_GamePlay.Powers.Ps_Mana_Empty"),
            ("water_projectile", "Vfx_PhysMat.Water.water_projectile"),
            ("krust_spew", "Vfx_GamePlay.RiverKrust.Ps_RiverKrust_Spew"),
            ("krust_death", "Vfx_GamePlay.RiverKrust.Ps_RiverKrust_Death"),
            ("krust_impact", "Vfx_GamePlay.RiverKrust.RK_Impact_Default"),
            ("krust_sword", "Vfx_GamePlay.RiverKrust.RK_SwordImpact"),
            ("krust_lens", "Vfx_GamePlay.RiverKrust.ps_camera_riverkrust"),
            ("assassin_vanish", "Vfx_GamePlay.assassin.Assassins_Disap_Spine_01"),
            ("assassin_appear", "Vfx_GamePlay.assassin.Assassins_Appear_Spine_01"),
            ("assassin_pull", "Vfx_GamePlay.assassin.Assassin_AssassinPull_01"),
            ("assassin_pull_camera", "Vfx_GamePlay.assassin.Assassin_PlayerPull_01"),
            // Dark Vision: the characters' sight (unaware, alerted) and the sounds they hear
            ("dv_cone_blue", "Vfx_GamePlay.DarkVision.Ps_DVision_VCone_Blue_01"),
            ("dv_cone_red", "Vfx_GamePlay.DarkVision.PS_DVision_VCone_Red_01"),
            ("dv_sound", "Vfx_GamePlay.DarkVision.Ps_DVision_Sounds_Base01"),
            // the Outsider's mark glowing on Corvo's hand as he casts
            ("tattoo_glow_1", "Vfx_GamePlay.Powers.Ps_Tattoo_Glow_01"),
            ("tattoo_glow_2", "Vfx_GamePlay.Powers.Ps_Tattoo_Glow_02"),
            ("tattoo_glow_3", "Vfx_GamePlay.Powers.Ps_Tattoo_Glow_03"),
            ("tattoo_glow_4", "Vfx_GamePlay.Powers.Ps_Tattoo_Glow_04"),
            ("tattoo_glow_wb", "Vfx_GamePlay.Powers.Ps_Tattoo_Glow_WB"),
        ];
        // the textures gameplay effects sample (`DarkVision_PMAT`, `PPG_DarkVisionFinal_Mat`)
        const GAME_TEXTURES: &[(&str, &str)] = &[
            ("dv_cloud", "Screen_Effects.PostProcessTextures.Cloud_64_d"),
            ("dv_smoke", "Vfx_Weapon.Textures.Smoke_01_d"),
            ("eyelid_gradient", "AltScreen_Effects.PostProcessTextures.fxt_eyelid_gradient"),
        ];
        // the post-process graph's materials (`AltScreen_Effects.PostProcessChain.Test_PPG`): the
        // vector fields its motion blur follows (Bend Time, Blink, under water, adrenaline,
        // possession, a knock-out, the spyglass), its compositions and screen effects
        const POST_MATERIALS: &[(&str, &str)] = &[
            ("bendtime_vectors", "AltScreen_Effects.PostProcessMaterial.PPG_BendTimeVectors_Std_INST"),
            ("bendtime_vectors_water", "AltScreen_Effects.PostProcessMaterial.PPG_BendTimeVectors_UnderWater_INST"),
            ("blink_vectors", "AltScreen_Effects.PostProcessMaterial.PPG_BlinkVectors_Std_INST"),
            ("blink_vectors_previous", "AltScreen_Effects.PostProcessMaterial.PPG_BlinkVectors_WithPrevious_INST"),
            ("underwater_vectors", "AltScreen_Effects.PostProcessMaterial.PPG_UnderWaterVectorField"),
            ("adrenaline_vectors", "AltScreen_Effects.PostProcessMaterial.PPG_AdrenalineVectors_INST"),
            ("possession_vectors", "AltScreen_Effects.PostProcessMaterial.PPG_PossessionVectors"),
            ("ko_vectors", "AltScreen_Effects.PostProcessMaterial.PPG_KOVectors"),
            ("lens_vectors", "AltScreen_Effects.PostProcessMaterial.PPG_LensVectors"),
            ("ko_combine", "AltScreen_Effects.PostProcessMaterial.PPG_KO_Combine"),
            ("ko_backup", "AltScreen_Effects.PostProcessMaterial.TEST_PPG_BackupRed"),
            ("lens_compose", "AltScreen_Effects.PostProcessMaterial.PPG_LensCompose"),
            ("blinded", "AltScreen_Effects.PostProcessMaterial.TEST_PPG_Blinded_INST"),
            ("spyglass", "AltScreen_Effects.PostProcessMaterial.TEST_PPG_Lens"),
            ("outsider", "AltScreen_Effects.PostProcessMaterial.PPG_Outsider"),
            ("possession_in", "Screen_Effects.PostProcessMaterial.POSSESSION_IN_INST"),
            ("possession_out", "Screen_Effects.PostProcessMaterial.POSSESSION_OUT_INST"),
        ];
        for (name, path) in POST_MATERIALS {
            match self.assets.find(path) {
                Some(obj) => {
                    let id = self.material(&obj);
                    self.scene.post_materials.push((name.to_string(), id));
                    // (a time-varying instance: its curves)
                    let curves = mitv_curves(&obj);
                    if !curves.is_empty() {
                        self.scene.post_curves.push((name.to_string(), curves));
                    }
                }
                None => log::debug!("post material {path} not found"),
            }
        }
        if let Some(m) = self.assets.find("vfx_interactivity.interactivity_INST") {
            self.scene.highlight_material = Some(self.material(&m));
        }
        for (name, path) in GAME_TEXTURES {
            match self.assets.find(path) {
                Some(obj) => {
                    let id = self.texture(TexJob::Plain(obj));
                    self.scene.game_textures.push((name.to_string(), id));
                }
                None => log::warn!("texture {path} not found"),
            }
        }
        for (name, path) in GAME_EFFECTS {
            let Some(obj) = self.assets.find(path) else {
                log::debug!("effect {path} not found");
                continue;
            };
            if let Some(id) = self.particle_system(&obj) {
                self.scene.effects.push((name.to_string(), id));
            }
        }
        const GAME_PROPS: &[(&str, &str)] = &[
            ("player_sword", "Wpn_PlySwords.Wpn_PlySword01"),
            ("crossbow", "Crossbows.crossbow_01"),
            ("pistol", "CorvoGuns.CorvoGun_01"),
            ("elite_sword", "Wpn_EliteSword.Wpn_EliteSword"),
            ("city_sword", "Wpn_CitySword.Wpn_CitySword"),
            ("thug_sword", "ThugSword.ThugSword"),
            ("overseer_sword", "OverseerSword.OverseerSword"),
            ("assassin_sword", "AssassinSword.AssassinSword"),
            ("bolt", "Crossbows.bolt_01"),
            ("bolt_sleep", "Crossbows.Bolt_Sleep"),
            ("bolt_flare", "Crossbows.Bolt_Flare"),
            ("heart", "Heart.Heart"),
            ("grenade", "Grenade.Grenade"),
            ("springrazor", "SpringRazor.SpringRazor"),
        ];
        for (name, path) in GAME_PROPS {
            let Some(obj) = self.assets.find(path) else {
                log::warn!("prop {path} not found");
                continue;
            };
            let d = match skeletal::read_skeletal_mesh(&obj.pkg, obj.idx) {
                Ok(d) => d,
                Err(e) => {
                    log::warn!("prop {path}: {e:#}");
                    continue;
                }
            };
            let key = format!("prop:{}", obj.key());
            let file = format!("meshes/{}.mesh", sanitize(&key));
            let mf = skeletal::to_mesh_file(&d);
            let (min, max) = bounds(&mf.positions);
            if (self.opts.force || !self.root.join(&file).exists()) && mf.write(&self.root.join(&file)).is_err() {
                continue;
            }
            let mats: Vec<u32> = d
                .materials
                .iter()
                .map(|&m| match self.assets.resolve(&obj.pkg, m) {
                    Some(o) => self.material(&o),
                    None => 0,
                })
                .collect();
            let materials = d.sections.iter().map(|s| *mats.get(s.material as usize).unwrap_or(&0)).collect();
            let mesh = self.scene.meshes.len() as u32;
            self.scene.meshes.push(MeshRef { name: obj.path(), file, min, max, simple: Vec::new(), ..Default::default() });
            // (a sword: its blade's ends, what its swings' trails span)
            let blade = keyhole_socket(&obj, "BladeExtent_BL").zip(keyhole_socket(&obj, "BladeExtent_UR")).map(|(a, b)| [a.to_array(), b.to_array()]);
            self.scene.props.push(PropDef { name: name.to_string(), mesh, materials, blade });
        }
        // a blade in flesh: its blood (the contact system's sword against a body)
        self.scene.blade_blood = self.blade_blood();
        self.scene.severed_limbs = self.severed_limbs();
        // Corvo's first-person arms: a skinned rig exposed as the "player_arms" character type
        if let Some(obj) = self.assets.find("Engine.Ply_Player.Skm_Player") {
            match skeletal::read_skeletal_mesh(&obj.pkg, obj.idx) {
                Ok(d) => {
                    let skeleton = self.scene.skeletons.len() as u32;
                    self.scene.skeletons.push(SkeletonDef { name: obj.path(), bones: skeletal::to_skeleton(&d), sockets: mesh_sockets(&obj) });
                    // Corvo's first-person animation sets
                    const PLAYER_SETS: &[&str] = &[
                        "Ply_Empty_Idle_as.Ply_Empty_Idle_as",
                        "Ply_Empty_Locomotion_as.Ply_Empty_Locomotion_as",
                        "Ply_Generic_as.Ply_Generic_as",
                        "Ply_Sword_Idle_as.Ply_Sword_Idle_as",
                        "Ply_Sword_Locomotion_as.Ply_Sword_Locomotion_as",
                        "Ply_Sword_Attack_as.Ply_Sword_Attack_as",
                        "Ply_Sword_Versus_as.Ply_Sword_Versus_as",
                        "Ply_Sword_Hit_as.Ply_Sword_Hit_as",
                        "Ply_Sword_Choke_as.Ply_Sword_Choke_as",
                        "Ply_Sword_Assassination_as.Ply_Sword_Assassination_as",
                        "Ply_Sword_Fatality_as.Ply_Sword_Fatality_as",
                        // the head's bob (additive: `DisAnimNodeBlendByHeadBob`'s "Head Bob On")
                        "Ply_Head_Locomotion_as.Ply_Head_Locomotion_as",
                        "Ply_Powers_as.Ply_Powers_as",
                        "Ply_Gadgets_as.Ply_Gadgets_as",
                        "Ply_Crossbow_as.Ply_Crossbow_as",
                        "Ply_Pistol_as.Ply_Pistol_as",
                        "Ply_Gadgets_Heart_as.Ply_Gadgets_Heart_as",
                        "Ply_Gadgets_Grenade_as.Ply_Gadgets_Grenade_as",
                        "Ply_Gadgets_SpringRazor_as.Ply_Gadgets_SpringRazor_as",
                        "Ply_WeeperAttack_as.Ply_WeeperAttack_as",
                        "Ply_Empty_CarryCorpse_as.Ply_Empty_CarryCorpse_as",
                    ];
                    let mut sets = Vec::new();
                    for path in PLAYER_SETS {
                        match self.assets.find(path) {
                            Some(set) => sets.push(set),
                            None => log::warn!("player anim set {path} not found"),
                        }
                    }
                    let anim_sets = Vec::new();
                    // the clips' effects at the hands (`AnimNotify_PlayParticleEffect`: the
                    // Mark glowing as he casts)
                    for set in &sets {
                        let Ok(sp) = set.props() else { continue };
                        for q in object_array(&set.pkg, &sp, "Sequences") {
                            let Some(seq) = self.assets.resolve(&set.pkg, q) else { continue };
                            let Ok(qp) = seq.props() else { continue };
                            let clip = qp.name("SequenceName").unwrap_or_default().to_string();
                            let Some((c, o, sz)) = qp.array("Notifies") else { continue };
                            for n in parse_struct_array(&seq.pkg, o, sz, c).unwrap_or_default() {
                                let Some(no) = n.object("Notify").filter(|o| *o > 0) else { continue };
                                let class = seq.pkg.class_name(no);
                                // the finishers' slow motion (class defaults: 0.1 both, 0.1 s eases)
                                if class == "DisNotify_BendTime_Ranged" || class == "DisNotify_AdrenalineBendTime_Ranged" {
                                    let Ok(np) = upk::read_object(&seq.pkg, no).map(|o| o.props) else { continue };
                                    self.scene.arm_bend.push(ArmBend {
                                        clip: clip.clone(),
                                        time: n.float("Time").unwrap_or(0.0),
                                        duration: n.float("Duration").unwrap_or(0.0),
                                        player: np.float("m_fPlayerTimeScale").unwrap_or(0.1),
                                        world: np.float("m_fWorldTimeScale").unwrap_or(0.1),
                                        fade_in: np.float("m_fTransitionInTime").unwrap_or(0.1),
                                        fade_out: np.float("m_fTransitionOutTime").unwrap_or(0.1),
                                        adrenaline: class == "DisNotify_AdrenalineBendTime_Ranged",
                                    });
                                    continue;
                                }
                                // the blade in the victim's flesh (`DishonoredNotify_PlayerMeleeWeaponHitFlesh`):
                                // its blood, at the pseudo-socket "flesh" (the system set at run time)
                                if class == "DishonoredNotify_PlayerMeleeWeaponHitFlesh" {
                                    self.scene.arm_fx.push(ArmFx { clip: clip.clone(), time: n.float("Time").unwrap_or(0.0), socket: "flesh".into(), system: u32::MAX });
                                    continue;
                                }
                                // the blood on the lens (`AnimNotify_CameraEffect`: its effect class's
                                // `PS_CameraEffect`), at the socket "lens"
                                if class == "AnimNotify_CameraEffect" {
                                    let Ok(np) = upk::read_object(&seq.pkg, no).map(|o| o.props) else { continue };
                                    let Some(cls) = np.object("CameraLensEffect").filter(|o| *o != 0).and_then(|o| self.assets.resolve(&seq.pkg, o)) else { continue };
                                    let name = cls.name().to_string();
                                    let def = ["DishonoredGame", "Engine", "GameFramework"].iter().find_map(|p| self.assets.find(&format!("{p}.Default__{name}")));
                                    let Some(ps) = def.and_then(|d| self.chain(&d).obj(self.assets, "PS_CameraEffect")) else { continue };
                                    let Some(system) = self.particle_system(&ps) else { continue };
                                    self.scene.arm_fx.push(ArmFx { clip: clip.clone(), time: n.float("Time").unwrap_or(0.0), socket: "lens".into(), system });
                                    continue;
                                }
                                if class != "AnimNotify_PlayParticleEffect" {
                                    continue;
                                }
                                let Ok(np) = upk::read_object(&seq.pkg, no).map(|o| o.props) else { continue };
                                let Some(ps) = np.object("PSTemplate").filter(|o| *o != 0).and_then(|o| self.assets.resolve(&seq.pkg, o)) else { continue };
                                let Some(system) = self.particle_system(&ps) else { continue };
                                let socket = np.name("SocketName").unwrap_or("None").to_string();
                                self.scene.arm_fx.push(ArmFx { clip: clip.clone(), time: n.float("Time").unwrap_or(0.0), socket, system });
                            }
                        }
                    }
                    let pending = PendingAnims::new(self.scene.npc_types.len(), sets, &d);
                    match self.cook_skinned(&obj, &d, None) {
                        Ok((body, body_materials)) => {
                            self.pending_anims.push(pending);
                            // before the Outsider marks him: the same materials, untattooed skin
                            if let (Some(tat), Some(plain)) = (self.assets.find("Engine.Ply_Player.PlayerArmsTatooed_D"), self.assets.find("Engine.Ply_Player.PlayerArms_D")) {
                                let tat_id = self.texture(TexJob::Plain(tat));
                                let plain_id = self.texture(TexJob::Plain(plain));
                                self.scene.arms_no_mark = body_materials
                                    .iter()
                                    .map(|&m| {
                                        let mut d = self.scene.materials[m as usize].clone();
                                        let mut changed = false;
                                        if d.diffuse == Some(tat_id) {
                                            d.diffuse = Some(plain_id);
                                            changed = true;
                                        }
                                        if let Some(u) = d.ue3.as_mut() {
                                            for t in u.textures.iter_mut().filter(|t| **t == Some(tat_id)) {
                                                *t = Some(plain_id);
                                                changed = true;
                                            }
                                        }
                                        if changed {
                                            d.name.push_str("_NoMark");
                                            self.scene.materials.push(d);
                                            self.scene.materials.len() as u32 - 1
                                        } else {
                                            m
                                        }
                                    })
                                    .collect();
                            }
                            self.scene.npc_types.push(NpcType {
                            attachments: Vec::new(),
                            out_of_bend: false,
                            ragdoll: None,
                            name: "player_arms".into(),
                            kind: "player".into(),
                            skeleton: Some(skeleton),
                            body: Some(body),
                            body_materials,
                            head: None,
                            head_materials: Vec::new(),
                            anim_sets,
                            voices: Vec::new(),
                            faction: String::new(),
                            hostile: false,
                            story_group: String::new(),
                            possess: None,
                            body_slots: Vec::new(),
                            head_slots: Vec::new(),
                            facefx: None,
                            sight: None,
                            stats: None,
                        })
                        }
                        Err(e) => log::warn!("player arms: {e:#}"),
                    }
                }
                Err(e) => log::warn!("player arms: {e:#}"),
            }
        } else {
            log::warn!("player arms mesh not found");
        }
        // the rats of swarms (and the Devouring Swarm): the crowd agent's mesh and animations
        if let Some(mat) = self.assets.find("Npc_SmallRat.Materials.SmallRat2_inst") {
            self.scene.white_rat_material = Some(self.material(&mat));
        }
        if let (Some(obj), Some(comp)) = (
            self.assets.find("Npc_SmallRat.Mesh.Npc_SmallRat"),
            self.assets.find("RatSwarm.CrowdAgent_SmallRat.SkeletalMeshComponent0"),
        ) {
            match skeletal::read_skeletal_mesh(&obj.pkg, obj.idx) {
                Ok(d) => {
                    let skeleton = self.scene.skeletons.len() as u32;
                    self.scene.skeletons.push(SkeletonDef { name: obj.path(), bones: skeletal::to_skeleton(&d), sockets: mesh_sockets(&obj) });
                    let mut sets = Vec::new();
                    if let Ok(cp) = comp.props() {
                        for o in upk::props::object_array(&comp.pkg, &cp, "AnimSets") {
                            if let Some(set) = self.assets.resolve(&comp.pkg, o).filter(|s| s.class() == "AnimSet") {
                                sets.push(set);
                            }
                        }
                    }
                    let pending = PendingAnims::new(self.scene.npc_types.len(), sets, &d);
                    match self.cook_skinned(&obj, &d, None) {
                        Ok((body, body_materials)) => {
                            // a swarm's rats take the base animal's body
                            let possess = self.assets.find("Twk_Possessable_BaseNPC.Twk_Possessable_BaseAnimal").map(|t| self.possessable(&t));
                            self.pending_anims.push(pending);
                            self.scene.rat_type = Some(self.scene.npc_types.len() as u32);
                            self.scene.npc_types.push(NpcType {
                                attachments: Vec::new(),
                                out_of_bend: false,
                                ragdoll: None,
                                name: "rat".into(),
                                kind: "rat".into(),
                                skeleton: Some(skeleton),
                                body: Some(body),
                                body_materials,
                                head: None,
                                head_materials: Vec::new(),
                                anim_sets: Vec::new(),
                                voices: Vec::new(),
                                faction: "Rats".into(),
                                hostile: true,
                                story_group: "Twk_ID_Rats".into(),
                                possess,
                                body_slots: Vec::new(),
                                head_slots: Vec::new(),
                                facefx: None,
                                sight: None,
                                stats: None,
                            });
                        }
                        Err(e) => log::warn!("rat: {e:#}"),
                    }
                }
                Err(e) => log::warn!("rat: {e:#}"),
            }
        } else {
            log::warn!("rat mesh not found");
        }
    }

    /// Spawners are linked to patrol routes by Kismet in the original; approximate by
    /// proximity of the spawner to any route point.
    fn assign_routes(&mut self) {
        let routes = &self.scene.routes;
        for sp in self.scene.spawners.iter_mut() {
            let p = Vec3::from(sp.position);
            let best = routes
                .iter()
                .enumerate()
                .filter_map(|(i, r)| {
                    let d = r.points.iter().map(|q| Vec3::from(*q).distance(p)).fold(f32::MAX, f32::min);
                    (d < 6.0).then_some((i, d))
                })
                .min_by(|a, b| a.1.total_cmp(&b.1));
            sp.route = best.map(|(i, _)| i as u32);
        }
    }

    fn cook_textures(&mut self) -> Result<()> {
        let jobs = std::mem::take(&mut self.tex_jobs);
        let total = jobs.len().max(1);
        let done = AtomicUsize::new(0);
        let failed = AtomicUsize::new(0);
        let max = self.opts.max_texture_size;
        let force = self.opts.force;
        let root = self.root.clone();
        let scene = &self.scene;
        let progress = self.progress;
        jobs.par_iter().for_each(|(id, job)| {
            let path = root.join(&scene.textures[*id as usize].file);
            if force || !path.exists() || stale_uncompressed(&path) {
                if let Err(e) = cook_texture_job(job, max, &path) {
                    failed.fetch_add(1, Ordering::Relaxed);
                    log::warn!("texture {} failed: {e:#}", scene.textures[*id as usize].name);
                    // write a 1x1 placeholder so we don't retry forever
                    let _ = TexFile { format: TexFormat::Rgba8, width: 1, height: 1, srgb: true, clamp: false, mips: vec![vec![200, 0, 200, 255]] }
                        .write(&path);
                }
            }
            let d = done.fetch_add(1, Ordering::Relaxed) + 1;
            if d % 16 == 0 || d == total {
                progress(0.4 + 0.58 * d as f32 / total as f32, &format!("Textures {d}/{total}"));
            }
        });
        self.warnings += failed.load(Ordering::Relaxed);
        Ok(())
    }
}

/// True for block-aligned RGBA8/R8 textures written before cook-time compression existed.
fn stale_uncompressed(path: &Path) -> bool {
    use std::io::Read;
    let mut h = [0u8; 28];
    let ok = std::fs::File::open(path).and_then(|mut f| f.read_exact(&mut h)).is_ok();
    if !ok {
        return false;
    }
    let u = |i: usize| u32::from_le_bytes(h[4 + i * 4..8 + i * 4].try_into().unwrap());
    let (fmt, w, hh) = (TexFormat::from_u32(u(0)), u(1), u(2));
    matches!(fmt, Some(TexFormat::Rgba8) | Some(TexFormat::R8)) && w % 4 == 0 && hh % 4 == 0 && w > 4
}

fn bounds(p: &[[f32; 3]]) -> ([f32; 3], [f32; 3]) {
    let mut min = [f32::MAX; 3];
    let mut max = [f32::MIN; 3];
    for v in p {
        for i in 0..3 {
            min[i] = min[i].min(v[i]);
            max[i] = max[i].max(v[i]);
        }
    }
    if p.is_empty() {
        ([0.0; 3], [0.0; 3])
    } else {
        (min, max)
    }
}

fn srgb_to_linear(c: u8) -> f32 {
    let c = c as f32 / 255.0;
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

pub fn static_to_mesh_file(d: &upk::mesh::StaticMeshData) -> MeshFile {
    let n = d.positions.len();
    let mut mf = MeshFile {
        positions: d.positions.iter().map(|p| ue_point(*p)).collect(),
        normals: d.normals.iter().map(|v| ue_dir(*v)).collect(),
        tangents: d.tangents.iter().map(|t| {
            let v = ue_dir([t[0], t[1], t[2]]);
            [v[0], v[1], v[2], -t[3]]
        }).collect(),
        uv0: d.uvs.first().cloned().unwrap_or_else(|| vec![[0.0, 0.0]; n]),
        uv1: {
            // UE3 binds the last UV channel when the light-map index is out of range
            let li = d.lightmap_coord_index.max(0) as usize;
            d.uvs.get(li).or(d.uvs.last()).cloned().unwrap_or_default()
        },
        colors: d.colors.clone().unwrap_or_default(),
        indices: Vec::with_capacity(d.indices.len()),
        sections: Vec::new(),
        joints: Vec::new(),
        weights: Vec::new(),
    };
    for s in &d.sections {
        let first = s.first_index as usize;
        let count = s.num_faces as usize * 3;
        let start = mf.indices.len() as u32;
        if first + count > d.indices.len() {
            mf.sections.push((start, 0));
            continue;
        }
        for tri in d.indices[first..first + count].chunks(3) {
            // UE3 clockwise-front in a left-handed space == counter-clockwise after the Y/Z swap
            mf.indices.extend_from_slice(&[tri[0], tri[1], tri[2]]);
        }
        mf.sections.push((start, count as u32));
    }
    mf
}

fn cook_texture_job(job: &TexJob, max: u32, path: &Path) -> Result<()> {
    match job {
        TexJob::Plain(o) => {
            let t = upk::texture::read_texture2d(&o.pkg, o.idx, &tfc_for(o), max)?;
            let tf = to_tex_file(&t)?;
            tf.write(path)
        }
        TexJob::Masked(d, m) => {
            let td = upk::texture::read_texture2d(&d.pkg, d.idx, &tfc_for(d), max)?;
            let tm = upk::texture::read_texture2d(&m.pkg, m.idx, &tfc_for(m), max)?;
            let (w, h) = (td.mips[0].width, td.mips[0].height);
            let mut rgba = decode_rgba(td.format, w, h, &td.mips[0].data)?;
            // pick the mask mip closest to the diffuse size
            let mm = tm.mips.iter().find(|x| x.width <= w).unwrap_or(&tm.mips[0]);
            let mask = decode_rgba(tm.format, mm.width, mm.height, &mm.data)?;
            let use_alpha = tm.format == PixelFormat::Dxt5 || tm.format == PixelFormat::Dxt3;
            for y in 0..h {
                for x in 0..w {
                    let mx = (x as u64 * mm.width as u64 / w as u64) as u32;
                    let my = (y as u64 * mm.height as u64 / h as u64) as u32;
                    let mi = ((my * mm.width + mx) * 4) as usize;
                    let a = if use_alpha { mask[mi + 3] } else { mask[mi] };
                    rgba[((y * w + x) * 4 + 3) as usize] = a;
                }
            }
            compress_rgba(build_rgba_mips(rgba, w, h), w, h, true, false).write(path)
        }
        TexJob::Mask(m) => {
            let tm = upk::texture::read_texture2d(&m.pkg, m.idx, &tfc_for(m), max)?;
            let (w, h) = (tm.mips[0].width, tm.mips[0].height);
            let mut rgba = decode_rgba(tm.format, w, h, &tm.mips[0].data)?;
            let use_alpha = tm.format == PixelFormat::Dxt5 || tm.format == PixelFormat::Dxt3;
            for px in rgba.chunks_exact_mut(4) {
                let a = if use_alpha { px[3] } else { px[0] };
                px.copy_from_slice(&[a, a, a, a]);
            }
            compress_rgba(build_rgba_mips(rgba, w, h), w, h, false, false).write(path)
        }
        TexJob::ShadowPage(size, entries) => {
            let size = *size;
            let mut page = vec![0u8; (size * size) as usize];
            for (o, ox, oy) in entries {
                let t = upk::texture::read_texture2d(&o.pkg, o.idx, &tfc_for(o), 4096)?;
                let m = &t.mips[0];
                let rgba = decode_rgba(t.format, m.width, m.height, &m.data)?;
                for y in 0..m.height.min(size - oy) {
                    for x in 0..m.width.min(size - ox) {
                        page[((oy + y) * size + ox + x) as usize] = rgba[((y * m.width + x) * 4) as usize];
                    }
                }
            }
            // R8 mips via box filter
            let mut mips = vec![page];
            let mut s = size;
            while mips.len() < 4 {
                let prev = mips.last().unwrap();
                let n = s / 2;
                let mut next = vec![0u8; (n * n) as usize];
                for y in 0..n {
                    for x in 0..n {
                        let i = |xx: u32, yy: u32| prev[(yy * s + xx) as usize] as u32;
                        next[(y * n + x) as usize] = ((i(2 * x, 2 * y) + i(2 * x + 1, 2 * y) + i(2 * x, 2 * y + 1) + i(2 * x + 1, 2 * y + 1)) / 4) as u8;
                    }
                }
                mips.push(next);
                s = n;
            }
            compress_r8(mips, size, size, true).write(path)
        }
        TexJob::LightmapRaw(size, entries, which) => {
            let size = *size;
            let mut page = vec![0u8; (size * size * 4) as usize];
            for (pair, ox, oy) in entries {
                let ta = upk::texture::read_texture2d(&pair.nac.pkg, pair.nac.idx, &tfc_for(&pair.nac), 4096)?;
                let (w, h) = (ta.mips[0].width, ta.mips[0].height);
                let src = if *which == 0 {
                    decode_rgba(ta.format, w, h, &ta.mips[0].data)?
                } else {
                    // directional components resampled to the colour texture's size
                    let tb = upk::texture::read_texture2d(&pair.dmc.pkg, pair.dmc.idx, &tfc_for(&pair.dmc), 4096)?;
                    let mb = tb.mips.iter().find(|m| m.width == w && m.height == h).unwrap_or(&tb.mips[0]);
                    let d = decode_rgba(tb.format, mb.width, mb.height, &mb.data)?;
                    let mut out = vec![0u8; (w * h * 4) as usize];
                    for y in 0..h {
                        for x in 0..w {
                            let bx = (x as u64 * mb.width as u64 / w as u64) as u32;
                            let by = (y as u64 * mb.height as u64 / h as u64) as u32;
                            let j = ((by * mb.width + bx) * 4) as usize;
                            let i = ((y * w + x) * 4) as usize;
                            out[i..i + 4].copy_from_slice(&d[j..j + 4]);
                        }
                    }
                    out
                };
                for y in 0..h.min(size - oy) {
                    for x in 0..w.min(size - ox) {
                        let i = ((y * w + x) * 4) as usize;
                        let o = (((oy + y) * size + ox + x) * 4) as usize;
                        page[o..o + 3].copy_from_slice(&src[i..i + 3]);
                        page[o + 3] = 255;
                    }
                }
            }
            let rgba_mips = build_rgba_mips(page, size, size);
            let mut mips = Vec::new();
            let mut mw = size;
            for m in rgba_mips.into_iter().take(4) {
                let fmt = texpresso::Format::Bc1;
                let mut out = vec![0u8; fmt.compressed_size(mw as usize, mw as usize)];
                let params = texpresso::Params { algorithm: texpresso::Algorithm::RangeFit, ..Default::default() };
                compress_parallel(fmt, &m, mw as usize, params, &mut out);
                mips.push(out);
                mw /= 2;
            }
            // both coefficient textures are ordinary (sRGB) textures: LightMapTexture2D keeps
            // UTexture's SRGB default, and the directional scales (up to 16) assume it
            TexFile { format: TexFormat::Bc1, width: size, height: size, srgb: true, clamp: true, mips }.write(path)
        }
        TexJob::LightmapPage(size, entries) => {
            let size = *size;
            let mut page = vec![0u8; (size * size * 4) as usize];
            for (pair, ox, oy) in entries {
                let ta = upk::texture::read_texture2d(&pair.nac.pkg, pair.nac.idx, &tfc_for(&pair.nac), 4096)?;
                let tb = upk::texture::read_texture2d(&pair.dmc.pkg, pair.dmc.idx, &tfc_for(&pair.dmc), 4096)?;
                let (w, h) = (ta.mips[0].width, ta.mips[0].height);
                let nac = decode_rgba(ta.format, w, h, &ta.mips[0].data)?;
                let mb = tb.mips.iter().find(|m| m.width == w && m.height == h).unwrap_or(&tb.mips[0]);
                let dmc = decode_rgba(tb.format, mb.width, mb.height, &mb.data)?;
                let (sa, sb) = (pair.s_nac, pair.s_dmc);
                for y in 0..h.min(size - oy) {
                    for x in 0..w.min(size - ox) {
                        let i = ((y * w + x) * 4) as usize;
                        let bx = (x as u64 * mb.width as u64 / w as u64) as u32;
                        let by = (y as u64 * mb.height as u64 / h as u64) as u32;
                        let j = ((by * mb.width + bx) * 4) as usize;
                        // flat-normal evaluation of the 3-basis directional lightmap
                        let d = (dmc[j] as f32 * sb[0] + dmc[j + 1] as f32 * sb[1] + dmc[j + 2] as f32 * sb[2]) / (3.0 * 255.0);
                        let o = (((oy + y) * size + ox + x) * 4) as usize;
                        for c in 0..3 {
                            page[o + c] = (nac[i + c] as f32 * sa[c] * d).round().clamp(0.0, 255.0) as u8;
                        }
                        page[o + 3] = 255;
                    }
                }
            }
            let rgba_mips = build_rgba_mips(page, size, size);
            let mut mips = Vec::new();
            let mut mw = size;
            for m in rgba_mips.into_iter().take(4) {
                let fmt = texpresso::Format::Bc1;
                let mut out = vec![0u8; fmt.compressed_size(mw as usize, mw as usize)];
                let params = texpresso::Params { algorithm: texpresso::Algorithm::RangeFit, ..Default::default() };
                compress_parallel(fmt, &m, mw as usize, params, &mut out);
                mips.push(out);
                mw /= 2;
            }
            TexFile { format: TexFormat::Bc1, width: size, height: size, srgb: true, clamp: true, mips }.write(path)
        }
    }
}

thread_local! {
    static TFC: std::cell::RefCell<Option<(PathBuf, Arc<upk::texture::TfcCache>)>> = const { std::cell::RefCell::new(None) };
}

fn tfc_for(o: &Obj) -> Arc<upk::texture::TfcCache> {
    let dir = o.pkg.path.parent().map(|p| p.to_path_buf()).unwrap_or_default();
    TFC.with(|t| {
        let mut t = t.borrow_mut();
        if let Some((d, c)) = t.as_ref() {
            if *d == dir {
                return c.clone();
            }
        }
        let c = Arc::new(upk::texture::TfcCache::new(&dir));
        *t = Some((dir, c.clone()));
        c
    })
}

pub fn decode_rgba(f: PixelFormat, w: u32, h: u32, d: &[u8]) -> Result<Vec<u8>> {
    let mut out = vec![0u8; (w * h * 4) as usize];
    let fmt = match f {
        PixelFormat::Dxt1 => Some(texpresso::Format::Bc1),
        PixelFormat::Dxt3 => Some(texpresso::Format::Bc2),
        PixelFormat::Dxt5 => Some(texpresso::Format::Bc3),
        PixelFormat::Bc5 => Some(texpresso::Format::Bc5),
        _ => None,
    };
    if let Some(fmt) = fmt {
        if w % 4 == 0 && h % 4 == 0 {
            fmt.decompress(d, w as usize, h as usize, &mut out);
        } else {
            // pad to block size
            let pw = w.div_ceil(4) * 4;
            let ph = h.div_ceil(4) * 4;
            let mut tmp = vec![0u8; (pw * ph * 4) as usize];
            fmt.decompress(d, pw as usize, ph as usize, &mut tmp);
            for y in 0..h {
                let s = (y * pw * 4) as usize;
                let dd = (y * w * 4) as usize;
                out[dd..dd + (w * 4) as usize].copy_from_slice(&tmp[s..s + (w * 4) as usize]);
            }
        }
    } else {
        match f {
            PixelFormat::Bgra8 => {
                for (i, c) in d.chunks(4).enumerate().take((w * h) as usize) {
                    out[i * 4..i * 4 + 4].copy_from_slice(&[c[2], c[1], c[0], c[3]]);
                }
            }
            PixelFormat::G8 => {
                for (i, c) in d.iter().enumerate().take((w * h) as usize) {
                    out[i * 4..i * 4 + 4].copy_from_slice(&[*c, *c, *c, 255]);
                }
            }
            PixelFormat::V8U8 => {
                for (i, c) in d.chunks(2).enumerate().take((w * h) as usize) {
                    let u = (c[0] as i8 as i32 + 128) as u8;
                    let v = (c[1] as i8 as i32 + 128) as u8;
                    out[i * 4..i * 4 + 4].copy_from_slice(&[u, v, 255, 255]);
                }
            }
            _ => bail!("cannot decode format {f:?}"),
        }
    }
    Ok(out)
}

fn build_rgba_mips(mut rgba: Vec<u8>, mut w: u32, mut h: u32) -> Vec<Vec<u8>> {
    let mut mips = Vec::new();
    loop {
        let next_w = (w / 2).max(1);
        let next_h = (h / 2).max(1);
        let done = w == 1 && h == 1;
        let mut next = vec![0u8; (next_w * next_h * 4) as usize];
        if !done {
            for y in 0..next_h {
                for x in 0..next_w {
                    for c in 0..4 {
                        let mut s = 0u32;
                        for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                            let sx = (x * 2 + dx).min(w - 1);
                            let sy = (y * 2 + dy).min(h - 1);
                            s += rgba[((sy * w + sx) * 4 + c) as usize] as u32;
                        }
                        next[((y * next_w + x) * 4 + c) as usize] = (s / 4) as u8;
                    }
                }
            }
        }
        mips.push(std::mem::replace(&mut rgba, next));
        if done {
            break;
        }
        w = next_w;
        h = next_h;
    }
    mips
}

fn to_tex_file(t: &upk::texture::TextureData) -> Result<TexFile> {
    let base = &t.mips[0];
    let clamp = t.address_x == "TA_Clamp" && t.address_y == "TA_Clamp";
    let srgb = t.srgb;
    let block_ok = base.width % 4 == 0 && base.height % 4 == 0;
    let fmt = match t.format {
        PixelFormat::Dxt1 if block_ok => Some(TexFormat::Bc1),
        PixelFormat::Dxt3 if block_ok => Some(TexFormat::Bc2),
        PixelFormat::Dxt5 if block_ok => Some(TexFormat::Bc3),
        PixelFormat::Bc5 if block_ok => Some(TexFormat::Bc5),
        PixelFormat::G8 => Some(TexFormat::R8),
        _ => None,
    };
    // collect a consecutive mip chain
    let need0 = t.format.mip_size(base.width, base.height).min(base.data.len());
    let mut mips = vec![base.data[..need0].to_vec()];
    let (mut w, mut h) = (base.width, base.height);
    for m in &t.mips[1..] {
        let (nw, nh) = ((w / 2).max(1), (h / 2).max(1));
        if m.width != nw || m.height != nh {
            break;
        }
        let need = t.format.mip_size(nw, nh);
        if m.data.len() < need {
            break;
        }
        mips.push(m.data[..need].to_vec());
        w = nw;
        h = nh;
    }
    if let Some(format) = fmt {
        return Ok(TexFile { format, width: base.width, height: base.height, srgb, clamp, mips });
    }
    let rgba = decode_rgba(t.format, base.width, base.height, &base.data)?;
    Ok(compress_rgba(build_rgba_mips(rgba, base.width, base.height), base.width, base.height, srgb, clamp))
}

/// Block-compress an RGBA8 mip chain (BC1 when opaque, BC3 with alpha) so every cooked
/// texture stays GPU-compressed. Bases that aren't block aligned stay RGBA8.
fn compress_rgba(mips: Vec<Vec<u8>>, w: u32, h: u32, srgb: bool, clamp: bool) -> TexFile {
    if w % 4 != 0 || h % 4 != 0 || mips.is_empty() {
        return TexFile { format: TexFormat::Rgba8, width: w, height: h, srgb, clamp, mips };
    }
    let alpha = mips[0].chunks_exact(4).any(|p| p[3] < 250);
    let (format, bc) = if alpha { (TexFormat::Bc3, texpresso::Format::Bc3) } else { (TexFormat::Bc1, texpresso::Format::Bc1) };
    let params = texpresso::Params { algorithm: texpresso::Algorithm::RangeFit, ..Default::default() };
    let mips = mips
        .iter()
        .enumerate()
        .map(|(i, m)| {
            let (mw, mh) = ((w >> i).max(1) as usize, (h >> i).max(1) as usize);
            let mut out = vec![0u8; bc.compressed_size(mw, mh)];
            if mw == mh && mw >= 256 {
                compress_parallel(bc, m, mw, params, &mut out);
            } else {
                bc.compress(m, mw, mh, params, &mut out);
            }
            out
        })
        .collect();
    TexFile { format, width: w, height: h, srgb, clamp, mips }
}

/// BC4-compress a single channel mip chain.
fn compress_r8(mips: Vec<Vec<u8>>, w: u32, h: u32, clamp: bool) -> TexFile {
    if w % 4 != 0 || h % 4 != 0 {
        return TexFile { format: TexFormat::R8, width: w, height: h, srgb: false, clamp, mips };
    }
    let params = texpresso::Params { algorithm: texpresso::Algorithm::RangeFit, ..Default::default() };
    let mips = mips
        .iter()
        .enumerate()
        .map(|(i, m)| {
            let (mw, mh) = ((w >> i).max(1) as usize, (h >> i).max(1) as usize);
            let rgba: Vec<u8> = m.iter().flat_map(|&v| [v, v, v, 255]).collect();
            let bc = texpresso::Format::Bc4;
            let mut out = vec![0u8; bc.compressed_size(mw, mh)];
            if mw == mh && mw >= 256 {
                compress_parallel(bc, &rgba, mw, params, &mut out);
            } else {
                bc.compress(&rgba, mw, mh, params, &mut out);
            }
            out
        })
        .collect();
    TexFile { format: TexFormat::Bc4, width: w, height: h, srgb: false, clamp, mips }
}

/// BCn-compress a square RGBA image using rayon over horizontal bands of blocks.
fn compress_parallel(fmt: texpresso::Format, rgba: &[u8], size: usize, params: texpresso::Params, out: &mut [u8]) {
    let band_rows = 64usize; // pixel rows per band (multiple of 4)
    let block_bytes = fmt.compressed_size(4, 4);
    let band_out = (size / 4) * (band_rows / 4) * block_bytes;
    out.par_chunks_mut(band_out).enumerate().for_each(|(bi, chunk)| {
        let y0 = bi * band_rows;
        let rows = band_rows.min(size - y0);
        let src = &rgba[y0 * size * 4..(y0 + rows) * size * 4];
        fmt.compress(src, size, rows, params, chunk);
    });
}

/// A character's AnimSets waiting to be bound to its skeleton.
struct PendingAnims {
    npc_type: usize,
    sets: Vec<Obj>,
    /// mesh bone names, in mesh order
    bones: Vec<String>,
    root_rot: glam::Quat,
    root_offset: Vec3,
}

impl PendingAnims {
    fn new(npc_type: usize, sets: Vec<Obj>, d: &skeletal::SkeletalMeshData) -> PendingAnims {
        let (q, t) = d.component_xform();
        PendingAnims { npc_type, sets, bones: d.bones.iter().map(|b| b.name.clone()).collect(), root_rot: q, root_offset: t }
    }
}

/// How an AnimSet's Edge joints map onto a character.
struct AnimLayout {
    /// bone name of each Edge joint
    names: Vec<String>,
    /// Edge joint of the mesh's root bone (its keys become root motion)
    root: Option<usize>,
    root_rot: glam::Quat,
    /// the mesh's component offset (applied to the root bone like its bind pose)
    root_offset: Vec3,
    hash: u32,
}

/// Edge joints are the tracked bones in the order of the skeleton the set was compressed
/// for. That's the character's own bone order, unless it lacks some tracked bones: then
/// another character's skeleton that has them all provides the order.
fn anim_layout(set: &Obj, p: &PendingAnims, refs: &[&[String]]) -> Result<AnimLayout> {
    let od = upk::read_object(&set.pkg, set.idx)?;
    let mut tracks = Vec::new();
    if let Some((n, offset, _)) = od.props.array("TrackBoneNames") {
        let mut r = Reader::at(&set.pkg.data, offset);
        for _ in 0..n {
            tracks.push(set.pkg.read_name(&mut r)?);
        }
    }
    let names = animation_binding_names(&tracks, &p.bones, refs);
    let root = p.bones.first().and_then(|r| names.iter().position(|n| n.eq_ignore_ascii_case(r)));
    let hash = anim_layout_hash(&names, root, p.root_rot, p.root_offset);
    Ok(AnimLayout { names, root, root_rot: p.root_rot, root_offset: p.root_offset, hash })
}

fn animation_binding_names(tracks: &[String], bones: &[String], refs: &[&[String]]) -> Vec<String> {
    let keys: HashSet<_> = tracks.iter().map(|t| t.to_ascii_lowercase()).collect();
    let filter = |bones: &[String]| bones.iter().filter(|b| keys.contains(&b.to_ascii_lowercase())).cloned().collect::<Vec<_>>();
    let names = filter(bones);
    if names.len() == tracks.len() { return names; }
    refs.iter().map(|r| filter(r)).find(|n| n.len() == tracks.len()).unwrap_or_else(|| {
        // Some original devices reference a clip for an unrelated rig. Keep its
        // timeline/notifies and source names; runtime binding ignores absent bones.
        tracks.to_vec()
    })
}

/// Every binding input that changes cooked tracks belongs in the cache identity.
/// In particular, meshes with the same names can have different component offsets.
fn anim_layout_hash(names: &[String], root: Option<usize>, rotation: glam::Quat, offset: Vec3) -> u32 {
    let mut h: u32 = 0x811c_9dc5;
    let mut eat = |b: &[u8]| {
        for &x in b {
            h = (h ^ x as u32).wrapping_mul(0x0100_0193);
        }
    };
    eat(b"anim-layout-v3/");
    for n in names {
        eat(n.to_ascii_lowercase().as_bytes());
        eat(b"/");
    }
    eat(&root.map(|i| i as u32).unwrap_or(u32::MAX).to_le_bytes());
    for v in rotation.to_array().into_iter().chain(offset.to_array()) {
        eat(&v.to_bits().to_le_bytes());
    }
    h
}

#[cfg(test)]
mod anim_cache_tests {
    use super::*;

    #[test]
    fn unmatched_animation_rigs_keep_source_tracks_without_partial_reindexing() {
        let tracks = vec!["Root_jnt".into(), "rope_right_jnt".into(), "Cart_jnt".into()];
        let launcher = vec!["Root_trap_jnt".into(), "launcher_jnt".into()];
        assert_eq!(animation_binding_names(&tracks, &launcher, &[]), tracks);
        let partial = vec!["Root_jnt".into(), "Other_jnt".into()];
        assert_eq!(animation_binding_names(&tracks, &partial, &[]), tracks);
        let reference = vec!["root_jnt".into(), "Cart_jnt".into(), "rope_right_jnt".into(), "Extra_jnt".into()];
        let expected = reference[..3].to_vec();
        assert_eq!(animation_binding_names(&tracks, &launcher, &[&reference]), expected);
        assert_eq!(animation_binding_names(&tracks, &reference, &[]), expected);
        assert!(animation_binding_names(&[], &launcher, &[]).is_empty());
    }

    #[test]
    fn animation_cache_distinguishes_root_binding_and_component_transforms() {
        let names = vec!["root0_jnt".to_string(), "Root_jnt".to_string()];
        let rotation = glam::Quat::IDENTITY;
        let offset = Vec3::new(0.0, 0.0, -90.25);
        let key = anim_layout_hash(&names, Some(0), rotation, offset);
        // These offsets occur on small city guards and thugs in Streets1.
        assert_ne!(key, anim_layout_hash(&names, Some(0), rotation, Vec3::new(0.0, 0.0, -91.0)));
        assert_ne!(key, anim_layout_hash(&names, None, rotation, offset));
        assert_ne!(key, anim_layout_hash(&names, Some(1), rotation, offset));
        assert_ne!(key, anim_layout_hash(&names, Some(0), glam::Quat::from_rotation_z(0.0001), offset));
        let uppercase = names.iter().map(|s| s.to_uppercase()).collect::<Vec<_>>();
        assert_eq!(key, anim_layout_hash(&uppercase, Some(0), rotation, offset));
    }
}

/// The sound notifies (`AnimNotify_AkEvent`) of an AnimSequence: (time, Wwise event name).
/// A `MaterialInstanceTimeVarying`'s scalar curves (`ScalarParameterValues`: the parameter, its
/// value before it plays, the curve's keys).
fn mitv_curves(obj: &Obj) -> Vec<PostCurve> {
    if obj.pkg.class_name(obj.idx) != "MaterialInstanceTimeVarying" {
        return Vec::new();
    }
    let Ok(p) = obj.props() else { return Vec::new() };
    let Some((c, o, sz)) = p.array("ScalarParameterValues") else { return Vec::new() };
    parse_struct_array(&obj.pkg, o, sz, c)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|v| {
            let param = v.name("ParameterName")?.to_string();
            let mut keys = Vec::new();
            if let Some(upk::Value::Struct(_, sub)) = v.get("ParameterValueCurve") {
                let curve = upk::Props(sub.clone());
                if let Some((c, o, sz)) = curve.array("Points") {
                    for k in parse_struct_array(&obj.pkg, o, sz, c).unwrap_or_default() {
                        keys.push([k.float("InVal").unwrap_or(0.0), k.float("OutVal").unwrap_or(0.0)]);
                    }
                }
            }
            Some(PostCurve { param, default: v.float("ParameterValue").unwrap_or(0.0), keys })
        })
        .collect()
}

fn sound_notifies(pkg: &upk::Package, seq: &Props) -> Vec<(f32, String)> {
    let mut out = Vec::new();
    let Some((c, off, sz)) = seq.array("Notifies") else { return out };
    for n in parse_struct_array(pkg, off, sz, c).unwrap_or_default() {
        let Some(no) = n.object("Notify").filter(|o| *o > 0) else { continue };
        if pkg.class_name(no) != "AnimNotify_AkEvent" {
            continue;
        }
        let Some(ev) = upk::read_object(pkg, no).ok().and_then(|o| o.props.object("AkEvent")).filter(|e| *e != 0) else { continue };
        let name = pkg.obj_path(ev);
        out.push((n.float("Time").unwrap_or(0.0), name.rsplit('.').next().unwrap_or(&name).to_string()));
    }
    out
}

/// A door's sounds, from its tweaks (`m_pDoorTweaks`): the open / close animation notifies
/// and the locked / unlocked events.
fn door_sounds(assets: &Assets, tweak: &Obj) -> Option<DoorSounds> {
    let p = tweak.props().ok()?;
    let name = |prop: &str| match p.object(prop).filter(|o| *o != 0) {
        Some(o) => {
            let path = tweak.pkg.obj_path(o);
            path.rsplit('.').next().unwrap_or(&path).to_string()
        }
        None => String::new(),
    };
    let mut d = DoorSounds { locked: name("m_pUseWhileLockedSoundEvent"), unlocked: name("m_pUnlockSoundEvent"), ..Default::default() };
    for set in object_array(&tweak.pkg, &p, "m_AnimSets") {
        let Some(set) = assets.resolve(&tweak.pkg, set) else { continue };
        let Ok(sp) = set.props() else { continue };
        for s in object_array(&set.pkg, &sp, "Sequences") {
            let Ok(so) = upk::read_object(&set.pkg, s) else { continue };
            match so.props.name("SequenceName").unwrap_or("") {
                "Player_Open_Cw" if d.open.is_empty() => d.open = sound_notifies(&set.pkg, &so.props),
                "Player_Close_Cw" if d.close.is_empty() => d.close = sound_notifies(&set.pkg, &so.props),
                _ => {}
            }
        }
    }
    Some(d)
}

/// Decode an AnimSet's Edge sequences, bind them to the mesh's bones and write them to
/// `path`. Returns false when no sequence fits the mesh.
fn cook_anim_set(set: &Obj, layout: &AnimLayout, path: &Path) -> Result<bool> {
    let od = upk::read_object(&set.pkg, set.idx)?;
    let cq = layout.root_rot;
    let mut out = AnimFile { bones: layout.names.clone(), clips: Vec::new() };
    let mut skipped = 0;
    for s in object_array(&set.pkg, &od.props, "Sequences") {
        let Ok(so) = upk::read_object(&set.pkg, s) else { continue };
        let name = so.props.name("SequenceName").unwrap_or("").to_string();
        let mut r = so.reader;
        let (Ok(_), Ok(len)) = (r.i32(), r.i32()) else { continue };
        let Ok(blob) = r.bytes(len.max(0) as usize) else { continue };
        let a = match edge::EdgeAnim::decode(blob) {
            // A zero-track sequence can still carry duration and sound notifies.
            Ok(a) if a.num_joints == 0 || a.num_joints == layout.names.len() => a,
            _ => {
                skipped += 1;
                continue;
            }
        };
        let mut clip = AnimClip {
            name,
            duration: a.duration,
            rate: a.frequency,
            frames: a.num_frames.min(u16::MAX as usize) as u16,
            ..Default::default()
        };
        clip.sounds = sound_notifies(&set.pkg, &so.props);
        // a carried body's clips place it relative to the carrier through its root bone; the
        // fish is modelled upright and swims laid flat by its root
        let keep_root = clip.name.starts_with("Corpses_CarryCorpse") || clip.name.starts_with("Empty_CarryCorpse") || clip.name.starts_with("Fish_");
        for t in a.rotations {
            let root = layout.root == Some(t.joint as usize);
            if root && !keep_root {
                continue;
            }
            // the root bone goes through the mesh's component transform (as its bind pose)
            let values = t
                .values
                .iter()
                .map(|q| {
                    let q = if root { (cq * glam::Quat::from_array(*q).normalize()).to_array() } else { *q };
                    [-q[0], -q[2], -q[1], q[3]]
                })
                .collect();
            clip.rotations.push(KeyTrack { bone: t.joint, frames: t.frames, values });
        }
        for t in a.translations {
            if layout.root == Some(t.joint as usize) && !keep_root {
                if let (Some(a), Some(b)) = (t.values.first(), t.values.last()) {
                    let delta = cq * (Vec3::from(*b) - Vec3::from(*a));
                    clip.root_motion = ue_point(delta.to_array());
                }
                continue;
            }
            let root = layout.root == Some(t.joint as usize);
            let values = t.values.iter().map(|v| ue_point(if root { (cq * Vec3::from(*v) + layout.root_offset).to_array() } else { *v })).collect();
            clip.translations.push(KeyTrack { bone: t.joint, frames: t.frames, values });
        }
        out.clips.push(clip);
    }
    if skipped > 0 {
        log::debug!("anim set {}: {skipped} sequences don't fit {} joints", set.path(), layout.names.len());
    }
    if out.clips.is_empty() {
        return Ok(false);
    }
    out.write(path)?;
    Ok(true)
}

/// A skeletal mesh's sockets (`Sockets`), relative to their bones in Bevy space.
/// How far the AI hear a noise of an `EAINoiseLoudness` (the numbered levels are guesses: their
/// table is native).
fn noise_meters(n: &str) -> f32 {
    match n {
        n if n.ends_with('m') => n.trim_start_matches("EAINoiseLoudness").trim_end_matches('m').parse::<f32>().unwrap_or(10.0),
        "EAINoiseLoudness1" => 5.0,
        "EAINoiseLoudness2" => 10.0,
        "EAINoiseLoudness3" => 20.0,
        "EAINoiseLoudness4" => 30.0,
        _ => 0.0,
    }
}

/// A skeletal mesh's socket in mesh space, through its bind pose (a door's keyhole, a tap's
/// spout).
fn keyhole_socket(mesh: &Obj, name: &str) -> Option<Vec3> {
    let sockets = mesh_sockets(mesh);
    let socket = sockets.iter().find(|s| s.name.eq_ignore_ascii_case(name))?;
    let data = skeletal::read_skeletal_mesh(&mesh.pkg, mesh.idx).ok()?;
    let bones = skeletal::to_skeleton(&data);
    let mut model: Vec<Mat4> = Vec::with_capacity(bones.len());
    for b in &bones {
        let local = Mat4::from_rotation_translation(glam::Quat::from_array(b.rotation).normalize(), Vec3::from(b.translation));
        let m = if b.parent >= 0 && (b.parent as usize) < model.len() { model[b.parent as usize] * local } else { local };
        model.push(m);
    }
    let i = bones.iter().position(|b| b.name.eq_ignore_ascii_case(&socket.bone))?;
    Some(model[i].transform_point3(Vec3::from(socket.translation)))
}

fn mesh_sockets(mesh: &Obj) -> Vec<SocketDef> {
    let Ok(od) = upk::read_object(&mesh.pkg, mesh.idx) else { return Vec::new() };
    let mut out = Vec::new();
    for s in object_array(&mesh.pkg, &od.props, "Sockets") {
        if s <= 0 {
            continue;
        }
        let so = Obj { pkg: mesh.pkg.clone(), idx: s };
        let Ok(p) = so.props() else { continue };
        let (Some(name), Some(bone)) = (p.name("SocketName"), p.name("BoneName")) else { continue };
        let loc = p.vector("RelativeLocation").unwrap_or([0.0; 3]);
        let rot = p.rotator("RelativeRotation").unwrap_or([0; 3]);
        let q = glam::Quat::from_mat4(&rot_matrix(rot)).normalize();
        out.push(SocketDef {
            name: name.to_string(),
            bone: bone.to_string(),
            translation: ue_point(loc),
            rotation: [-q.x, -q.z, -q.y, q.w],
        });
    }
    out
}

/// Arkane post-process settings: the level only stores what differs from the defaults.
fn post_process(pp: &Props) -> PostProcess {
    let mut out = PostProcess::default();
    let sub = |p: &Props, n: &str| match p.get(n) {
        Some(Value::Struct(_, s)) => Props(s.clone()),
        _ => Props(Vec::new()),
    };
    let uber = sub(pp, "m_UberPpParameters");
    let dof = sub(&uber, "m_DOFParameters");
    let cb = sub(&uber, "m_CBParameters");
    let hdr = sub(&uber, "m_HDRParameters");
    let bloom = sub(pp, "m_PpBloomParameters");
    if let Some(v) = cb.vector("m_CrMgYbShadTones") {
        out.shadows = v;
    }
    if let Some(v) = cb.vector("m_CrMgYbMidTones") {
        out.midtones = v;
    }
    if let Some(v) = cb.vector("m_CrMgYbHighTones") {
        out.highlights = v;
    }
    let f = |p: &Props, n: &str, d: f32| p.float(n).unwrap_or(d);
    out.balance_opacity = f(&cb, "m_Opacity", out.balance_opacity);
    out.pre_desaturation = f(&cb, "m_PreDesaturation", out.pre_desaturation);
    out.post_desaturation = f(&cb, "m_PostDesaturation", out.post_desaturation);
    out.exposure = f(&hdr, "m_Exposure", out.exposure);
    out.gamma = f(&hdr, "m_GammaAdjustment", out.gamma);
    out.film_grain = f(&hdr, "m_FilmGrainNoise", out.film_grain);
    out.brightness = f(&hdr, "m_GimpBrightness", out.brightness);
    out.contrast = f(&hdr, "m_GimpContrast", out.contrast);
    out.bloom = bloom.bool("m_bEnable").unwrap_or(out.bloom);
    if let Some(Value::LinearColor(c)) = bloom.get("m_Tint") {
        out.bloom_tint = [c[0], c[1], c[2]];
    }
    out.bloom_threshold = f(&bloom, "m_Threshold", out.bloom_threshold);
    out.bloom_scale = f(&bloom, "m_Scale", out.bloom_scale);
    out.focus_distance = f(&dof, "m_FocusDistance", 1000.0) * UNIT;
    out.in_focus_radius = f(&dof, "m_InFocusRadius", 300.0) * UNIT;
    out.far_blur = f(&dof, "m_FarBlurAmount", out.far_blur);
    out
}

/// A particle module's distributions and plain values.
/// Structs are stored as deltas against the class defaults (`defaults`).
fn particle_module(o: &Obj, p: &Props, defaults: Option<&Props>) -> ModuleDef {
    let mut m = ModuleDef { class: o.class(), ..Default::default() };
    for prop in &p.0 {
        match &prop.value {
            Value::Struct(name, sub) if name.starts_with("RawDistribution") => {
                let def = defaults.and_then(|d| d.struct_props(&prop.name));
                if let Some(d) = raw_distribution(o, &Props(sub.clone()), def.as_ref(), name == "RawDistributionVector") {
                    m.dists.insert(prop.name.clone(), d);
                }
            }
            Value::Float(f) => {
                m.values.insert(prop.name.clone(), *f);
            }
            Value::Int(i) => {
                m.values.insert(prop.name.clone(), *i as f32);
            }
            Value::Bool(b) => {
                m.values.insert(prop.name.clone(), *b as i32 as f32);
            }
            Value::Byte(b) => {
                m.values.insert(prop.name.clone(), *b as f32);
            }
            Value::Enum(e) | Value::Name(e) => {
                m.names.insert(prop.name.clone(), e.clone());
            }
            _ => {}
        }
    }
    m
}

/// A cooked raw distribution (lookup table), or the distribution object it was baked from.
fn raw_distribution(o: &Obj, p: &Props, defaults: Option<&Props>, vector: bool) -> Option<Dist> {
    let byte_of = |p: &Props, n: &str| match p.get(n) {
        Some(Value::Byte(b)) => Some(*b),
        Some(Value::Int(i)) => Some(*i as u8),
        _ => None,
    };
    let byte = |n: &str| byte_of(p, n).or_else(|| defaults.and_then(|d| byte_of(d, n)));
    let float = |n: &str| p.float(n).or_else(|| defaults.and_then(|d| d.float(n)));
    let default_chunk = if vector { 3 } else { 1 };
    if let Some((count, offset, _)) = p.array("LookupTable") {
        let mut r = Reader::at(&o.pkg.data, offset);
        let table: Vec<f32> = (0..count).filter_map(|_| r.f32().ok()).collect();
        if table.len() > 2 {
            return Some(Dist {
                op: byte("Op").unwrap_or(1),
                chunk: byte("LookupTableChunkSize").unwrap_or(default_chunk),
                scale: float("LookupTableTimeScale").unwrap_or(0.0),
                start: float("LookupTableStartTime").unwrap_or(0.0),
                table: table[2..].to_vec(),
            });
        }
    }
    // uncooked: constant / uniform distribution objects
    let d = p.object("Distribution").filter(|d| *d > 0)?;
    let dp = upk::read_object(&o.pkg, d).ok()?.props;
    let cls = o.pkg.class_name(d);
    let val = |n: &str| -> Option<Vec<f32>> {
        match dp.get(n)? {
            Value::Float(f) => Some(vec![*f]),
            Value::Vector(v) => Some(v.to_vec()),
            _ => None,
        }
    };
    if cls.ends_with("Constant") {
        let v = val("Constant").unwrap_or(vec![0.0; default_chunk as usize]);
        return Some(Dist { op: 1, chunk: v.len() as u8, scale: 0.0, start: 0.0, table: v });
    }
    if cls.ends_with("Uniform") {
        let lo = val("Min").unwrap_or(vec![0.0; default_chunk as usize]);
        let hi = val("Max").unwrap_or(vec![0.0; default_chunk as usize]);
        let mut t = lo.clone();
        t.extend(hi);
        return Some(Dist { op: 2, chunk: t.len() as u8, scale: 0.0, start: 0.0, table: t });
    }
    None
}

/// The raw GFx data of a package's `SwfMovie` export.
pub fn swf_movie(pkg: &upk::Package, name: &str) -> Result<Vec<u8>> {
    let idx = pkg.find_export(name).with_context(|| format!("no movie {name} in {}", pkg.name))?;
    let od = upk::read_object(pkg, idx)?;
    let (c, off, _) = od.props.array("RawData").context("no RawData")?;
    Ok(pkg.data[off..off + c].to_vec())
}

/// Cook the user interface assets: the original fonts (Scaleform font library) as TrueType.
pub fn cook_ui(cooked: &Path, root: &Path) -> Result<Vec<PathBuf>> {
    let dir = root.join("ui").join("fonts");
    std::fs::create_dir_all(&dir)?;
    let pkg = upk::Package::open(&cooked.join("DisFonts_SF.upk"))?;
    let movie = swf::Movie::parse(&swf_movie(&pkg, "DisFonts.gfxfontlib")?)?;
    let mut out = Vec::new();
    for f in movie.fonts() {
        let family = f.name.trim().to_string();
        let file: String = family.chars().filter(|c| c.is_ascii_alphanumeric()).collect();
        let path = dir.join(format!("{file}.ttf"));
        std::fs::write(&path, ttf::build(&f, &family))?;
        log::info!("font {family}: {} glyphs -> {}", f.glyphs.len(), path.display());
        out.push(path);
    }
    // loading screens: the missions' illustrations and the chapter title strip
    // ... the powers' journal art, item icons and the equipment icons of the HUD and wheel
    // ... and the mission statistics screens' backgrounds (`UI_MissionStatsBg_<Mission>_SF`)
    let mut sources: Vec<(String, &str, &str)> = [
        ("UI_Mission_Large", "missions", ""),
        // (and their strips on the save cards)
        ("UI_Mission_Small", "missions", ""),
        ("UI_Loading_SF", "loading", ""),
        ("UI_Powers_Large", "powers", ""),
        ("UI_ItemIcons_Large", "items", ""),
        ("Startup", "itemsmall", "UI_ItemIcons_Small."),
        ("Startup", "icons", "UI_EquipmentIcons."),
        // the mask's lens frame
        ("Startup", "effects", "AltScreen_Effects.SpyGlass"),
        ("UI_portraits_objectives", "portraits", ""),
    ]
    .iter()
    .map(|(p, d, x)| (p.to_string(), *d, *x))
    .collect();
    for m in ["Prison", "Overseer", "Brothel", "Bridge", "Boyle", "TowerRet", "Flooded", "Hub", "LightH"] {
        sources.push((format!("UI_MissionStatsBg_{m}_SF"), "missionstats", ""));
    }
    // ... and the location maps found in the levels (`UI_Map_*`)
    for m in ["Boyle", "Bridge", "BrothelExt", "BrothelInt", "LightHouseExt1", "LightHouseExt2", "LightHouseExt3", "OverseerExt", "OverseerInt1", "OverseerInt2", "Streets", "TowerReturnInt", "TowerReturnInt2"] {
        sources.push((format!("UI_Map_{m}_SF"), "maps", ""));
    }
    // ... and Dunwall City Trials': the challenges' pictures, their briefings' and results'
    // backgrounds
    sources.push(("UI_ChallengesImages_DLC05_SF".into(), "dlc05", ""));
    for c in ["Arena", "AssassinsTraining", "BendTimeMassacre", "ChainKill", "Countdown", "DropAttack", "MysteryMan", "OilRain", "Race", "Thief"] {
        sources.push((format!("UI_ResBg_{c}_SF"), "dlc05", ""));
        sources.push((format!("UI_Brf_{c}_SF"), "dlc05", ""));
    }
    // (and the mystery man's possible targets, in its script level: `DLC05_H_MMTarget`)
    sources.push(("L_DLC05_MystMan_Script".into(), "dlc05", "UI_MysteryManTargets_DLC05."));
    // ... and its gallery: the small pictures, and the large, a package each
    // (`UI_G<mode>_<name>_L_SF`)
    sources.push(("UI_GalleryImg_Small_NoSF".into(), "dlc05gallery", ""));
    for d in crate::resolver::dlc_dirs(cooked) {
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        let mut large: Vec<String> = rd.filter_map(|e| e.ok()).filter_map(|e| e.file_name().to_str().map(str::to_string)).filter(|n| (n.starts_with("UI_GE_") || n.starts_with("UI_GN_")) && n.ends_with("_L_SF.upk")).map(|n| n.trim_end_matches(".upk").to_string()).collect();
        large.sort();
        for n in large {
            sources.push((n, "dlc05gallery", ""));
        }
    }
    for (pkg_name, dir_name, prefix) in sources {
        // (the game's packages, or a DLC's, with its texture caches)
        let Some(pdir) = std::iter::once(cooked.to_path_buf()).chain(crate::resolver::dlc_dirs(cooked)).find(|d| d.join(format!("{pkg_name}.upk")).exists()) else { continue };
        let Ok(pkg) = upk::Package::open(&pdir.join(format!("{pkg_name}.upk"))) else { continue };
        let dir = root.join("ui").join(dir_name);
        std::fs::create_dir_all(&dir)?;
        let tfc = upk::texture::TfcCache::new(&pdir);
        for idx in 1..=pkg.exports.len() as i32 {
            if pkg.class_name(idx) != "Texture2D" {
                continue;
            }
            let path = pkg.obj_path(idx);
            if !path.starts_with(prefix) {
                continue;
            }
            let stem = path.rsplit('.').next().unwrap_or(&path).to_string();
            // (the gallery's large pieces, up to 4096 tall, seen no larger than the screen)
            let cap = if dir_name == "dlc05gallery" { 2048 } else { 4096 };
            let Ok(t) = upk::texture::read_texture2d(&pkg, idx, &tfc, cap) else { continue };
            let m = &t.mips[0];
            let Ok(rgba) = decode_rgba(t.format, m.width, m.height, &m.data) else { continue };
            image::save_buffer(dir.join(format!("{stem}.png")), &rgba, m.width, m.height, image::ExtendedColorType::Rgba8)?;
        }
    }
    // the movies' bitmaps: atlas sub-images and stand-alone images, by character id (and the
    // HUD's effects: the stealth shroud, directional damage)
    for (pkg_name, only) in [
        // (the shared library and the options and load screens the menus import from)
        ("Startup", ""),
        ("UI_HUD_SF", ""),
        ("UI_PauseMenu_SF", ""),
        ("UI_PowerWheel_SF", ""),
        ("UI_MissionStats_SF", ""),
        ("UI_Loading_SF", ""),
        ("Dishonored_MainMenu", ""),
        ("UI_Journal_SF", ""),
        ("UI_Shop_SF", ""),
        ("DishonoredGame", "UI_HUDFX."),
        // (the message boxes and help bar over every screen; the notes and tutorials read)
        ("DishonoredGame", "UI_Global."),
        ("DishonoredGame", "UI_Note."),
        // Dunwall City Trials: its HUD, briefing, results and leaderboards
        ("UI_HUD_DLC05_SF", ""),
        ("UI_Brief_DLC05_SF", ""),
        ("UI_Results_DLC05_SF", ""),
        // (its challenge menu, in its menu map; its pause menu)
        ("L_DLC05_MainMenu_P", "UI_ChallengesMenu_DLC05."),
        ("UI_PauseMenu_DLC05_SF", "UI_PauseMenu_DLC05."),
    ] {
        // (the game's packages, or the DLC's)
        let path = std::iter::once(cooked.to_path_buf()).chain(crate::resolver::dlc_dirs(cooked)).map(|d| d.join(format!("{pkg_name}.upk"))).find(|p| p.exists());
        let Some(Ok(pkg)) = path.map(|p| upk::Package::open(&p)) else { continue };
        for idx in 1..=pkg.exports.len() as i32 {
            if pkg.class_name(idx) != "SwfMovie" {
                continue;
            }
            let path = pkg.obj_path(idx);
            if !path.starts_with(only) {
                continue;
            }
            match ui_images(cooked, &pkg, &path, root) {
                Ok(n) => log::info!("{path}: {n} images"),
                Err(e) => log::warn!("{path}: {e:#}"),
            }
        }
    }
    Ok(out)
}

/// A movie's imports (`ImportAssets`) made its own: each imported symbol's tree (sprites,
/// shapes, their bitmaps) copied from the other movie's cooked timeline under fresh ids, the
/// import's id its root (the menus' `lib_titleMc`, the options and load screens). Fonts are
/// left out (their text is drawn by the game).
fn inline_imports(movie: &swf::Movie, tl: &mut crate::format::Timelines, root: &Path, dir: &Path, index: &mut serde_json::Map<String, serde_json::Value>) -> Result<()> {
    use crate::format::{ShapeBitmap, SpriteTimeline, TlOp};
    // the other movies by file name, as cooked from Startup
    let source = |url: &str| -> Option<&'static str> {
        let f = url.rsplit(['/', '\\']).next().unwrap_or(url).to_ascii_lowercase();
        Some(match f.as_str() {
            "lib.swf" => "lib",
            "loadgame.swf" => "LoadGame",
            "optionsmenu.swf" => "OptionsMenu",
            _ => return None,
        })
    };
    let mut cache: HashMap<&'static str, crate::format::Timelines> = HashMap::new();
    // fresh ids: sprites and shapes from 20000, bitmaps from 60000
    let mut next_char: u16 = 20000;
    let mut next_bitmap: u16 = 60000;
    let mut n = 0;
    for (id, url, name) in movie.imports() {
        let Some(src) = source(&url) else { continue };
        if !cache.contains_key(src) {
            let Ok(bytes) = std::fs::read(root.join("ui").join(src).join("timeline.json")) else { continue };
            let Ok(t) = serde_json::from_slice::<crate::format::Timelines>(&bytes) else { continue };
            cache.insert(src, t);
        }
        let st = &cache[src];
        let Some(&root_id) = st.exports.get(&name) else { continue };
        // the tree's characters, depth first; the root keeps the import's id
        let mut map: HashMap<u16, u16> = HashMap::new();
        map.insert(root_id, id);
        let mut stack = vec![root_id];
        let mut order = Vec::new();
        while let Some(c) = stack.pop() {
            order.push(c);
            if let Some(sp) = st.sprites.get(&c) {
                for f in &sp.frames {
                    for op in &f.ops {
                        if let TlOp::Place { id: Some(k), .. } = op {
                            if !map.contains_key(k) {
                                map.insert(*k, next_char);
                                next_char += 1;
                                stack.push(*k);
                            }
                        }
                    }
                }
            }
        }
        let mut bitmaps: HashMap<u16, u16> = HashMap::new();
        for c in order {
            let new = map[&c];
            if let Some(sp) = st.sprites.get(&c) {
                let mut sp: SpriteTimeline = sp.clone();
                for f in &mut sp.frames {
                    for op in &mut f.ops {
                        if let TlOp::Place { id: Some(k), .. } = op {
                            *k = map[k];
                        }
                    }
                }
                tl.sprites.insert(new, sp);
            }
            if let Some(list) = st.shapes.get(&c) {
                let mut list: Vec<ShapeBitmap> = list.clone();
                for b in &mut list {
                    let nb = *bitmaps.entry(b.bitmap).or_insert_with(|| {
                        let v = next_bitmap;
                        next_bitmap += 1;
                        v
                    });
                    // its image, copied over
                    let from = root.join("ui").join(src).join(format!("{}.png", b.bitmap));
                    if std::fs::copy(&from, dir.join(format!("{nb}.png"))).is_ok() {
                        if let Ok((w, h)) = image::image_dimensions(&from) {
                            let mut e = serde_json::Map::new();
                            e.insert("name".into(), "".into());
                            e.insert("w".into(), w.into());
                            e.insert("h".into(), h.into());
                            index.insert(nb.to_string(), e.into());
                        }
                    }
                    b.bitmap = nb;
                }
                tl.shapes.insert(new, list);
            }
            if let Some(b) = st.bounds.get(&c) {
                tl.bounds.insert(new, *b);
            }
        }
        n += 1;
    }
    if n > 0 {
        log::info!("{}: {n} imported symbols copied in", dir.display());
    }
    Ok(())
}

/// Write a movie's bitmaps to `ui/<movie>/<id>.png` (+ `index.json`: id -> export name, size;
/// `timeline.json`: its sprites' timelines).
fn ui_images(cooked: &Path, pkg: &upk::Package, movie_path: &str, root: &Path) -> Result<usize> {
    let movie = swf::Movie::parse(&swf_movie(pkg, movie_path)?)?;
    // (a DLC's movie by its name and the DLC's: `UI_HUD_DLC05.HUD` is `HUD_DLC05`)
    let last = movie_path.rsplit('.').next().unwrap_or(movie_path);
    let dlc = movie_path.split('.').next().and_then(|p| p.rsplit('_').next()).filter(|t| t.starts_with("DLC"));
    let name_s = match dlc {
        Some(t) => format!("{last}_{t}"),
        None => last.to_string(),
    };
    let name = name_s.as_str();
    let dir = root.join("ui").join(name);
    std::fs::create_dir_all(&dir)?;
    let tfc = upk::texture::TfcCache::new(cooked);
    let exports = movie.exports();
    let (atlases, subs) = movie.images();
    // decoded textures by file name
    let mut decoded: HashMap<String, (u32, u32, Vec<u8>)> = HashMap::new();
    let mut load = |file: &str| -> Option<(u32, u32, Vec<u8>)> {
        let stem = file.trim_end_matches(".tga").trim_end_matches(".dds").to_string();
        if let Some(d) = decoded.get(&stem) {
            return Some(d.clone());
        }
        let idx = (1..=pkg.exports.len() as i32).find(|&i| pkg.class_name(i) == "Texture2D" && pkg.obj_path(i).rsplit('.').next() == Some(stem.as_str()))?;
        let t = upk::texture::read_texture2d(pkg, idx, &tfc, 4096).ok()?;
        let m = &t.mips[0];
        let rgba = decode_rgba(t.format, m.width, m.height, &m.data).ok()?;
        decoded.insert(stem.clone(), (m.width, m.height, rgba));
        decoded.get(&stem).cloned()
    };
    let mut index = serde_json::Map::new();
    let mut n = 0;
    let mut save = |id: u32, w: u32, h: u32, rgba: &[u8]| -> Result<()> {
        image::save_buffer(dir.join(format!("{id}.png")), rgba, w, h, image::ExtendedColorType::Rgba8)?;
        let mut e = serde_json::Map::new();
        e.insert("name".into(), exports.get(&(id as u16)).cloned().unwrap_or_default().into());
        e.insert("w".into(), w.into());
        e.insert("h".into(), h.into());
        index.insert(id.to_string(), e.into());
        n += 1;
        Ok(())
    };
    // stand-alone images (ids below the atlas range)
    for (id, file, _, _) in &atlases {
        if *id >= 0x90000 {
            continue;
        }
        if let Some((w, h, rgba)) = load(file) {
            save(*id, w, h, &rgba)?;
        }
    }
    for (id, (ai, r)) in &subs {
        let Some((_, file, dw, dh)) = atlases.iter().find(|a| a.0 == 0x90000 + *ai as u32) else { continue };
        let Some((w, h, rgba)) = load(file) else { continue };
        // atlases may be stored larger than declared
        let (sx, sy) = (w as f32 / (*dw).max(1) as f32, h as f32 / (*dh).max(1) as f32);
        let (x0, y0) = ((r[0] as f32 * sx) as u32, (r[1] as f32 * sy) as u32);
        let (x1, y1) = (((r[2] as f32 * sx) as u32).min(w), ((r[3] as f32 * sy) as u32).min(h));
        if x1 <= x0 || y1 <= y0 {
            continue;
        }
        let (cw, ch) = (x1 - x0, y1 - y0);
        let mut out = vec![0u8; (cw * ch * 4) as usize];
        for y in 0..ch {
            let s = (((y0 + y) * w + x0) * 4) as usize;
            out[(y * cw * 4) as usize..((y + 1) * cw * 4) as usize].copy_from_slice(&rgba[s..s + (cw * 4) as usize]);
        }
        save(*id as u32, cw, ch, &out)?;
    }
    // (its animated clips, played by the game's timeline player; the full-screen menus' vector
    // shapes drawn into images, the HUD's left as they are: their hit areas would show)
    let raster = !matches!(name, "HUD" | "HUDFX" | "HUD_DLC05");
    let mut vectors = Vec::new();
    // (the bitmaps saved above, for the shapes cut out of them)
    let cache: std::cell::RefCell<HashMap<u16, Option<(u32, u32, Vec<u8>)>>> = Default::default();
    let bitmap = |id: u16| -> Option<(u32, u32, Vec<u8>)> {
        cache
            .borrow_mut()
            .entry(id)
            .or_insert_with(|| {
                let img = image::open(dir.join(format!("{id}.png"))).ok()?.to_rgba8();
                Some((img.width(), img.height(), img.into_raw()))
            })
            .clone()
    };
    let mut tl = movie.timelines_with(raster.then_some(swf::Raster { out: &mut vectors, bitmap: &bitmap }));
    for (id, w, h, rgba) in &vectors {
        save(*id as u32, *w, *h, rgba)?;
    }
    // the symbols it imports from the library and the other screens, copied in
    if raster {
        inline_imports(&movie, &mut tl, root, &dir, &mut index)?;
    }
    std::fs::write(dir.join("index.json"), serde_json::to_vec_pretty(&index)?)?;
    std::fs::write(dir.join("timeline.json"), serde_json::to_vec(&tl)?)?;
    Ok(n)
}

/// A skeleton's reference pose, hashed (bone names and transforms).
fn skeleton_hash(d: &skeletal::SkeletalMeshData) -> u32 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    for b in &d.bones {
        b.name.hash(&mut h);
        for v in b.rotation.iter().chain(b.position.iter()) {
            v.to_bits().hash(&mut h);
        }
    }
    h.finish() as u32
}

/// A string array property (`TArray<FString>`), wherever in the chain it is set.
fn str_array(ch: &Chain, name: &str) -> Vec<String> {
    match ch.get_pkg(name) {
        Some((pkg, Value::Array { count, offset, .. })) => {
            let mut r = Reader::at(&pkg.data, *offset);
            (0..*count).filter_map(|_| r.fstring().ok()).filter(|s| !s.is_empty()).collect()
        }
        _ => Vec::new(),
    }
}

/// A `RenderingChannelContainer`'s set fields as bits (`REFLECT_CHANNELS`), over the defaults.
fn reflect_bits(v: Option<&Value>, default: u16) -> u16 {
    let mut bits = default;
    if let Some(Value::Struct(_, f)) = v {
        for p in f {
            if let (Some(i), Value::Bool(b)) = (REFLECT_CHANNELS.iter().position(|c| *c == p.name), &p.value) {
                if *b {
                    bits |= 1 << i;
                } else {
                    bits &= !(1 << i);
                }
            }
        }
    }
    bits
}
