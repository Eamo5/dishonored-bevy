//! Background map loading: cooks the original UE3 content on first run, then reads
//! the cache and prepares GPU-ready mesh data and physics colliders off the main thread.

use crate::{Config, GameState};
use bevy::prelude::*;
use bevy::text::FontSize;
use bevy_rapier3d::prelude::*;
use bevy_rapier3d::rapier::prelude::TriMeshFlags;
use dhcook::format::{AnimFile, MeshFile, Scene, TexFile, SCENE_VERSION};
use rayon::prelude::*;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};

pub struct LoadingPlugin;

impl Plugin for LoadingPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(OnEnter(GameState::Loading), (start_loading, spawn_loading_ui))
            .add_systems(Update, (poll_loading, hold_loading, await_movie, relayout_title).run_if(in_state(GameState::Loading)))
            .add_systems(OnExit(GameState::Loading), despawn_loading_ui);
    }
}

/// One material section of a mesh with compacted vertex data.
pub struct SubMesh {
    pub positions: Vec<[f32; 3]>,
    pub normals: Vec<[f32; 3]>,
    pub tangents: Vec<[f32; 4]>,
    pub uv0: Vec<[f32; 2]>,
    pub uv1: Vec<[f32; 2]>,
    /// vertex colours (RGBA), empty when the mesh has none
    pub colors: Vec<[u8; 4]>,
    pub joints: Vec<[u16; 4]>,
    pub weights: Vec<[f32; 4]>,
    pub indices: Vec<u32>,
}

pub struct PreparedMesh {
    pub sections: Vec<Option<SubMesh>>,
}

pub struct LoadedLevel {
    pub scene: Scene,
    pub meshes: Vec<Option<Arc<PreparedMesh>>>,
    pub textures: Vec<Option<TexFile>>,
    /// Per instance: collider with its (unscaled) world placement.
    pub colliders: Vec<Option<InstanceShape>>,
    /// `scene.anim_sets`, decoded
    pub anims: Vec<Option<Arc<AnimFile>>>,
    /// Lightmass volume samples
    pub light_volume: Vec<dhcook::format::VolumeSample>,
    pub load_seconds: f32,
}

enum LoadMsg {
    Progress(f32, String),
    Done(Box<LoadedLevel>),
    Error(String),
}

#[derive(Resource)]
struct LoaderRx(Mutex<Receiver<LoadMsg>>);

#[derive(Resource)]
pub struct PendingLevel(pub Option<Box<LoadedLevel>>);

#[derive(Component)]
pub(crate) struct LoadingUi;

#[derive(Component)]
pub(crate) struct LoadingText;

#[derive(Component)]
pub(crate) struct LoadingBar;

pub fn cache_dir() -> PathBuf {
    if let Ok(p) = std::env::var("DH_CACHE") {
        return PathBuf::from(p);
    }
    let cwd = std::env::current_dir().unwrap_or_default();
    if cwd.join("Cargo.toml").exists() || cwd.join("cache").exists() {
        return cwd.join("cache");
    }
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("cache")))
        .unwrap_or_else(|| PathBuf::from("cache"))
}

fn start_loading(mut commands: Commands, config: Res<Config>) {
    let (tx, rx) = channel();
    let cfg = config.clone();
    std::thread::Builder::new()
        .name("map-loader".into())
        .spawn(move || {
            let t = std::time::Instant::now();
            match load_level(&cfg, &tx) {
                Ok(mut lvl) => {
                    lvl.load_seconds = t.elapsed().as_secs_f32();
                    let _ = tx.send(LoadMsg::Done(Box::new(lvl)));
                }
                Err(e) => {
                    let _ = tx.send(LoadMsg::Error(format!("{e:#}")));
                }
            }
        })
        .expect("spawn loader thread");
    commands.insert_resource(LoaderRx(Mutex::new(rx)));
}

fn load_level(cfg: &Config, tx: &Sender<LoadMsg>) -> anyhow::Result<LoadedLevel> {
    let progress = |f: f32, m: &str| {
        let _ = tx.send(LoadMsg::Progress(f, m.to_string()));
    };
    let cache = cache_dir();
    let scene_path = dhcook::scene_path(&cache, &cfg.map);
    let mut need_cook = cfg.recook || !scene_path.exists();
    if !need_cook {
        match std::fs::read(&scene_path).ok().and_then(|d| serde_json::from_slice::<Scene>(&d).ok()) {
            Some(s) if s.version == SCENE_VERSION => {}
            _ => need_cook = true,
        }
    }
    if need_cook {
        progress(0.0, "Locating Dishonored installation");
        let cooked = dhcook::find_cooked_dir().ok_or_else(|| {
            anyhow::anyhow!("Dishonored installation not found. Set DISHONORED_DIR to the game folder.")
        })?;
        info!("cooking {} from {}", cfg.map, cooked.display());
        let assets = dhcook::resolver::Assets::new(&cooked)?;
        let opts = dhcook::CookOptions {
            max_texture_size: cfg.max_texture_size,
            force: cfg.recook,
            include_streaming: true,
        };
        let p = |f: f32, m: &str| progress(f * 0.6, &format!("Converting game data: {m}"));
        dhcook::cook_map(&assets, &cfg.map, &cache, opts, &p)?;
    }
    progress(0.6, "Reading scene");
    let scene: Scene = serde_json::from_slice(&std::fs::read(&scene_path)?)?;

    progress(0.62, "Loading meshes");
    let raw: Vec<Option<MeshFile>> = scene
        .meshes
        .par_iter()
        .map(|m| match MeshFile::read(&cache.join(&m.file)) {
            Ok(f) => Some(f),
            Err(e) => {
                warn!("mesh {} unreadable: {e:#}", m.name);
                None
            }
        })
        .collect();
    let meshes: Vec<Option<Arc<PreparedMesh>>> =
        raw.par_iter().map(|m| m.as_ref().map(|m| Arc::new(prepare_mesh(m)))).collect();

    progress(0.7, "Loading textures");
    // (the texture detail: the largest mips left out; the lighting's pages kept whole)
    let cap = crate::settings::TEXTURE_CAP.load(std::sync::atomic::Ordering::Relaxed);
    let textures: Vec<Option<TexFile>> = scene
        .textures
        .par_iter()
        // (render targets are drawn at run time: no file)
        .map(|t| match if t.render_target.is_some() { Err(anyhow::anyhow!("render target")) } else { TexFile::read(&cache.join(&t.file)) } {
            Ok(mut f) => {
                let lighting = t.name.starts_with("lmpage.") || t.name.starts_with("lmraw") || t.name.starts_with("shpage.");
                while !lighting && f.mips.len() > 1 && f.width.max(f.height) > cap {
                    f.mips.remove(0);
                    f.width = (f.width / 2).max(1);
                    f.height = (f.height / 2).max(1);
                }
                Some(f)
            }
            Err(_) if t.render_target.is_some() => None,
            Err(e) => {
                warn!("texture {} unreadable: {e:#}", t.name);
                None
            }
        })
        .collect();

    progress(0.78, "Loading animations");
    let anims: Vec<Option<Arc<AnimFile>>> = scene
        .anim_sets
        .par_iter()
        .map(|a| match AnimFile::read(&cache.join(&a.file)) {
            Ok(f) => Some(Arc::new(f)),
            Err(e) => {
                warn!("animations {} unreadable: {e:#}", a.name);
                None
            }
        })
        .collect();

    if crate::ue3mat::enabled() {
        progress(0.75, "Loading shaders");
        crate::ue3mat::library();
    }
    progress(0.8, "Building collision");
    let colliders = build_colliders(&scene, &raw);
    let light_volume = scene
        .light_volume
        .as_ref()
        .and_then(|f| dhcook::format::read_light_volume(&cache_dir().join(f)).map_err(|e| warn!("light volume: {e:#}")).ok())
        .unwrap_or_default();
    progress(1.0, "Spawning world");
    Ok(LoadedLevel { scene, meshes, textures, colliders, anims, light_volume, load_seconds: 0.0 })
}

/// Split a mesh into per-section compacted sub meshes.
fn prepare_mesh(m: &MeshFile) -> PreparedMesh {
    let n = m.positions.len();
    let has_uv1 = m.uv1.len() == n;
    let has_tan = m.tangents.len() == n;
    let has_skin = m.joints.len() == n && m.weights.len() == n;
    let has_color = m.colors.len() == n;
    let sections = m
        .sections
        .iter()
        .map(|&(first, count)| {
            if count == 0 {
                return None;
            }
            let idx = &m.indices[first as usize..(first + count) as usize];
            let mut remap: HashMap<u32, u32> = HashMap::with_capacity(idx.len());
            let mut s = SubMesh {
                positions: Vec::new(),
                normals: Vec::new(),
                tangents: Vec::new(),
                uv0: Vec::new(),
                uv1: Vec::new(),
                colors: Vec::new(),
                joints: Vec::new(),
                weights: Vec::new(),
                indices: Vec::with_capacity(idx.len()),
            };
            for &i in idx {
                if i as usize >= n {
                    return None;
                }
                let ni = *remap.entry(i).or_insert_with(|| {
                    let k = s.positions.len() as u32;
                    let i = i as usize;
                    s.positions.push(m.positions[i]);
                    s.normals.push(m.normals.get(i).copied().unwrap_or([0.0, 1.0, 0.0]));
                    if has_tan {
                        s.tangents.push(m.tangents[i]);
                    }
                    s.uv0.push(m.uv0.get(i).copied().unwrap_or([0.0, 0.0]));
                    if has_uv1 {
                        s.uv1.push(m.uv1[i]);
                    }
                    if has_color {
                        s.colors.push(m.colors[i]);
                    }
                    if has_skin {
                        s.joints.push(m.joints[i]);
                        s.weights.push(m.weights[i]);
                    }
                    k
                });
                s.indices.push(ni);
            }
            Some(s)
        })
        .collect();
    PreparedMesh { sections }
}

fn is_pickup_class(c: &str) -> bool {
    c.contains("Pickup") || c == "DisElixirHealth" || c.starts_with("DisKey") || c == "DisAudioLogPlayer" || c == "DisWhaleBoneCharm"
}

/// A world collider: shape, placement and the mesh section of each triangle.
pub type InstanceShape = (Collider, Vec3, Quat, Arc<Vec<u8>>);

fn build_colliders(scene: &Scene, raw: &[Option<MeshFile>]) -> Vec<Option<InstanceShape>> {
    // Group instances by (mesh, quantized scale) so identical shapes are built once.
    let mut keys: Vec<Option<(u32, [i32; 3])>> = Vec::with_capacity(scene.instances.len());
    let mut placement: Vec<(Vec3, Quat, Vec3)> = Vec::with_capacity(scene.instances.len());
    for inst in &scene.instances {
        let m = Mat4::from_cols_array(&inst.transform);
        let (s, r, t) = m.to_scale_rotation_translation();
        placement.push((t, r, s));
        // walls of light don't stop anyone: they disintegrate them (security.rs)
        let ok = inst.collide
            && !is_pickup_class(&inst.class)
            && inst.class != "DisWallOfLight"
            && raw.get(inst.mesh as usize).map(|m| m.is_some()).unwrap_or(false)
            && s.is_finite()
            && s.abs().min_element() > 1e-4;
        keys.push(ok.then(|| (inst.mesh, [(s.x * 1000.0) as i32, (s.y * 1000.0) as i32, (s.z * 1000.0) as i32])));
    }
    let mut unique: Vec<(u32, [i32; 3])> = keys.iter().flatten().copied().collect();
    unique.sort();
    unique.dedup();
    let built: HashMap<(u32, [i32; 3]), (Collider, Arc<Vec<u8>>)> = unique
        .par_iter()
        .filter_map(|&(mesh, sq)| {
            let m = raw[mesh as usize].as_ref()?;
            let s = Vec3::new(sq[0] as f32, sq[1] as f32, sq[2] as f32) / 1000.0;
            // the simplified collision where the original has it: convex hulls, all one
            // surface (the mesh's largest section)
            // (`DH_NO_SIMPLE_COLLISION`: the triangles everywhere, for comparison)
            let simple = &scene.meshes[mesh as usize].simple;
            if !simple.is_empty() && std::env::var("DH_NO_SIMPLE_COLLISION").is_err() {
                let hulls: Vec<Collider> = simple.iter().filter_map(|h| Collider::convex_hull(&h.iter().map(|p| Vec3::from(*p) * s).collect::<Vec<_>>())).collect();
                if !hulls.is_empty() {
                    let main = m.sections.iter().enumerate().max_by_key(|(_, &(_, c))| c).map(|(i, _)| i.min(255) as u8).unwrap_or(0);
                    let c = if hulls.len() == 1 {
                        hulls.into_iter().next().unwrap()
                    } else {
                        Collider::compound(hulls.into_iter().map(|h| (Vec3::ZERO, Quat::IDENTITY, h)).collect())
                    };
                    return Some(((mesh, sq), (c, Arc::new(vec![main]))));
                }
            }
            let verts: Vec<Vec3> = m.positions.iter().map(|p| Vec3::from(*p) * s).collect();
            let section = |i: usize| m.sections.iter().position(|&(f, c)| i as u32 >= f && (i as u32) < f + c).unwrap_or(0).min(255) as u8;
            let (tris, secs): (Vec<[u32; 3]>, Vec<u8>) = m
                .indices
                .chunks_exact(3)
                .enumerate()
                .filter(|(_, t)| {
                    let (a, b, c) = (verts[t[0] as usize], verts[t[1] as usize], verts[t[2] as usize]);
                    (b - a).cross(c - a).length_squared() > 1e-12
                })
                .map(|(k, t)| ([t[0], t[1], t[2]], section(k * 3)))
                .unzip();
            if tris.is_empty() {
                return None;
            }
            let c = Collider::trimesh_with_flags(
                verts.clone(),
                tris.clone(),
                TriMeshFlags::FIX_INTERNAL_EDGES | TriMeshFlags::MERGE_DUPLICATE_VERTICES,
            )
            .or_else(|_| Collider::trimesh(verts, tris))
            .ok()?;
            Some(((mesh, sq), (c, Arc::new(secs))))
        })
        .collect();
    keys.iter()
        .zip(placement.iter())
        .map(|(k, (t, r, _))| k.and_then(|k| built.get(&k).map(|(c, s)| (c.clone(), *t, *r, s.clone()))))
        .collect()
}

/// The mission illustration (`UI_Mission_Large`) shown while a map loads.
pub(crate) fn mission_art(map: &str) -> Option<&'static str> {
    let m = map.to_ascii_lowercase();
    let table: [(&str, &str); 22] = [
        ("l_tower_p", "EmpressTower"),
        ("l_prsnsewer", "Sewer"),
        ("l_prison", "Prison"),
        ("l_pub", "Hub"),
        ("l_streetsewer", "StreetsSewer"),
        ("l_streets", "Streets"),
        ("l_distillery", "Distillery"),
        ("l_ovrsr", "Overseer"),
        ("l_brothel", "Brothel"),
        ("l_boyle", "Boyle"),
        ("l_bridge", "Bridge"),
        ("l_galvani", "Bridge"),
        ("l_artdealer", "Bridge"),
        ("l_towerrtrn", "TowerReturn"),
        ("l_flooded_fassassins", "AssassinsHQ"),
        ("l_flooded", "FloodedDistrict"),
        ("l_isl", "Lighthouse"),
        ("l_lighth", "Lighthouse"),
        ("l_outsiderdream", "Void"),
        ("l_out", "Outro"),
        ("dishonored_mainmenu", ""),
        ("", ""),
    ];
    table.iter().find(|(k, _)| !k.is_empty() && m.starts_with(k)).map(|(_, v)| *v).filter(|v| !v.is_empty())
}

/// The original loading hints of a map's mission (`DisBinkOverlayManager_HintSet_*`).
fn hint_set(map: &str) -> Option<&'static str> {
    let m = map.to_ascii_lowercase();
    let table: [(&str, &str); 18] = [
        ("l_tower_p", "Intro"),
        ("l_prsnsewer", "Sewer"),
        ("l_prison", "Prison"),
        ("l_pub_assault", "PubAttack"),
        ("l_pub", "Pub"),
        ("l_outsiderdream", "Pub"),
        ("l_streets", "Overseer"),
        ("l_distillery", "Overseer"),
        ("l_ovrsr", "Overseer"),
        ("l_brothel", "Brothel"),
        ("l_bridge", "Bridge"),
        ("l_galvani", "Bridge"),
        ("l_artdealer", "Bridge"),
        ("l_boyle", "Boyle"),
        ("l_towerrtrn", "TowerReturn"),
        ("l_flooded", "Flooded"),
        ("l_out", "PubAttack"),
        ("l_", "Lighthouse"),
    ];
    table.iter().find(|(k, _)| m.starts_with(k)).map(|(_, v)| *v)
}

fn ui_png(images: &mut Assets<Image>, rel: &str) -> Option<Handle<Image>> {
    use bevy::image::{CompressedImageFormats, ImageSampler, ImageType};
    let bytes = std::fs::read(cache_dir().join("ui").join(rel)).ok()?;
    let img = Image::from_buffer(&bytes, ImageType::Extension("png"), CompressedImageFormats::NONE, true, ImageSampler::linear(), bevy::asset::RenderAssetUsages::RENDER_WORLD).ok()?;
    Some(images.add(img))
}

#[allow(clippy::too_many_arguments)]
fn spawn_loading_ui(
    mut commands: Commands,
    fonts: Res<crate::ui_fonts::UiFonts>,
    config: Res<crate::Config>,
    mut images: ResMut<Assets<Image>>,
    data: Res<crate::gamedata::Data>,
    (mut movies, mut play, mut intro): (ResMut<crate::movie::Movies>, MessageWriter<crate::movie::PlayMovie>, ResMut<crate::movie::IntroPending>),
) {
    // a new game: the intro (the Empress's letter) over the Tower's loading, skippable;
    // otherwise the map's loading movie loops behind the title and hints
    let intro_on = std::mem::take(&mut intro.0) && movies.has("INTRO_LOC") && (std::env::var("DH_SCRIPT").is_err() || std::env::var("DH_INTRO").is_ok());
    if intro_on {
        play.write(crate::movie::PlayMovie { name: "INTRO_LOC".into(), looping: false, skippable: true, layer: crate::movie::MovieLayer::Fullscreen });
    }
    let backdrop = if intro_on { None } else { movies.loading_movie(&config.map) };
    if let Some(m) = &backdrop {
        play.write(crate::movie::PlayMovie { name: m.clone(), looping: true, skippable: false, layer: crate::movie::MovieLayer::Backdrop });
    }
    // a hint of the mission's set, as the original's loading screens give
    let hint = hint_set(&config.map)
        .and_then(|s| data.0.hints.get(s))
        .filter(|h| !h.is_empty() && !config.map.eq_ignore_ascii_case(crate::menu::MENU_MAP))
        .map(|h| crate::gamedata::readable(&h[rand::random_range(0..h.len())]));
    let menu = config.map.eq_ignore_ascii_case(crate::menu::MENU_MAP);
    let art = if backdrop.is_some() { None } else { mission_art(&config.map).and_then(|a| ui_png(&mut images, &format!("missions/MissionsScreen_{a}_Large.png"))) };
    let strip = ui_png(&mut images, "loading/ChapterTitleBg.png");
    let title = if menu { "DISHONORED".to_string() } else { crate::save::mission_name(&config.map) };
    let gold = Color::srgb(0.86, 0.80, 0.66);
    let cam = commands.spawn((LoadingUi, Camera2d, Camera { order: 100, ..default() })).id();
    commands
        .spawn((
            LoadingUi,
            Node { width: percent(100), height: percent(100), ..default() },
            BackgroundColor(Color::srgb(0.03, 0.028, 0.025)),
            UiTargetCamera(cam),
            GlobalZIndex(i32::MAX - 1),
        ))
        .with_children(|p| {
            if backdrop.is_some() {
                p.spawn((crate::movie::MovieBackdrop, Node { position_type: PositionType::Absolute, left: px(0), top: px(0), width: percent(100), height: percent(100), ..default() }));
            }
            if let Some(a) = art {
                p.spawn((ImageNode::new(a), Node { position_type: PositionType::Absolute, right: percent(4), top: percent(4), height: percent(92), aspect_ratio: Some(960.0 / 1360.0), ..default() }));
            }
            // mission title over the chapter strip
            p.spawn(Node { position_type: PositionType::Absolute, left: px(0), top: percent(36), width: percent(64), height: px(110), justify_content: JustifyContent::FlexStart, align_items: AlignItems::Center, padding: UiRect::left(percent(7)), ..default() })
                .with_children(|t| {
                    if let Some(s) = &strip {
                        t.spawn((ImageNode::new(s.clone()), Node { position_type: PositionType::Absolute, left: px(0), top: px(0), width: percent(100), height: percent(100), ..default() }));
                    }
                    t.spawn((LoadingTitle, Text::new(title), TextFont { font: fonts.title.clone().into(), font_size: FontSize::Px(64.0), ..default() }, TextColor(Color::srgb(0.95, 0.92, 0.86)), TextShadow::default()));
                });
            p.spawn(Node { position_type: PositionType::Absolute, left: percent(7), bottom: percent(9), flex_direction: FlexDirection::Column, row_gap: px(10), ..default() })
                .with_children(|c| {
                    if let Some(h) = &hint {
                        c.spawn((Text::new(h.clone()), TextFont { font_size: FontSize::Px(24.0), ..default() }, TextColor(Color::srgb(0.88, 0.86, 0.8)), Node { max_width: px(760), margin: UiRect::bottom(px(14)), ..default() }));
                    }
                    c.spawn((LoadingText, Text::new("Loading"), TextFont { font_size: FontSize::Px(24.0), ..default() }, TextColor(gold.with_alpha(0.8))));
                    c.spawn((Node { width: px(420), height: px(4), ..default() }, BackgroundColor(Color::srgba(1.0, 1.0, 1.0, 0.08))))
                        .with_child((LoadingBar, Node { width: percent(0), height: percent(100), ..default() }, BackgroundColor(Color::srgb(0.75, 0.62, 0.42))));
                });
        });
}

fn despawn_loading_ui(mut commands: Commands, q: Query<Entity, With<LoadingUi>>) {
    // the shader warm-up keeps it up a little longer (`warmup.rs`)
    if crate::warmup::enabled() {
        return;
    }
    for e in &q {
        commands.entity(e).despawn();
    }
}

#[derive(Component)]
struct LoadingTitle;

/// Fonts loaded at start-up register a few frames late: lay the title out again once they have.
fn relayout_title(mut events: MessageReader<AssetEvent<Font>>, mut q: Query<&mut TextFont, With<LoadingTitle>>) {
    if events.read().next().is_some() {
        for mut f in &mut q {
            f.set_changed();
        }
    }
}

/// The level is ready but a movie that plays to its end (the intro) is still on.
#[derive(Resource)]
struct AwaitMovie;

fn await_movie(mut commands: Commands, wait: Option<Res<AwaitMovie>>, movies: Res<crate::movie::Movies>, mut next: ResMut<NextState<GameState>>) {
    if wait.is_some() && !movies.holding() {
        commands.remove_resource::<AwaitMovie>();
        next.set(GameState::InGame);
    }
}

/// Test hook: seconds left before leaving the loading screen.
#[derive(Resource)]
struct HoldLoading(f32);

fn hold_loading(mut commands: Commands, time: Res<Time<bevy::time::Real>>, hold: Option<ResMut<HoldLoading>>, mut next: ResMut<NextState<GameState>>) {
    let Some(mut h) = hold else { return };
    h.0 -= time.delta_secs();
    if h.0 <= 0.0 {
        commands.remove_resource::<HoldLoading>();
        next.set(GameState::InGame);
    }
}

fn poll_loading(
    mut commands: Commands,
    rx: Option<Res<LoaderRx>>,
    mut text: Query<&mut Text, With<LoadingText>>,
    mut bar: Query<&mut Node, With<LoadingBar>>,
    mut next: ResMut<NextState<GameState>>,
    movies: Res<crate::movie::Movies>,
) {
    let Some(rx) = rx else { return };
    let rx = rx.0.lock().unwrap();
    while let Ok(msg) = rx.try_recv() {
        match msg {
            LoadMsg::Progress(f, m) => {
                for mut t in &mut text {
                    t.0 = m.clone();
                }
                for mut n in &mut bar {
                    n.width = percent(f * 100.0);
                }
            }
            LoadMsg::Done(level) => {
                info!(
                    "level ready in {:.1}s: {} instances, {} meshes, {} textures",
                    level.load_seconds,
                    level.scene.instances.len(),
                    level.scene.meshes.len(),
                    level.scene.textures.len()
                );
                commands.insert_resource(PendingLevel(Some(level)));
                // test hook: capture the loading screen before entering the level
                if let Ok(path) = std::env::var("DH_LOADING_SHOT") {
                    commands.spawn(bevy::render::view::screenshot::Screenshot::primary_window()).observe(bevy::render::view::screenshot::save_to_disk(path));
                    commands.insert_resource(HoldLoading(1.0));
                } else if movies.holding() {
                    commands.insert_resource(AwaitMovie);
                } else {
                    next.set(GameState::InGame);
                }
            }
            LoadMsg::Error(e) => {
                error!("failed to load map: {e}");
                for mut t in &mut text {
                    t.0 = format!("Failed to load: {e}");
                }
            }
        }
    }
}
