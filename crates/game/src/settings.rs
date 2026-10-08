//! Player options, kept in `%APPDATA%/DishonoredBevy/settings.json`.

use bevy::prelude::*;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Resource, Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct Settings {
    /// mouse look (radians per pixel x 1000)
    pub sensitivity: f32,
    pub invert_y: bool,
    /// vertical field of view (degrees)
    pub fov: f32,
    pub master_volume: f32,
    pub music_volume: f32,
    pub sfx_volume: f32,
    pub voice_volume: f32,
    /// `PSI_Audio_SubtitlesMode` (`ESubtitlesMode`): 0 off, 1 main dialogue (no barks), 2 all
    /// speeches
    pub subtitle_mode: u8,
    /// 0 easy, 1 normal, 2 hard, 3 very hard
    pub difficulty: u8,
    /// key bindings changed from the defaults: (action, key)
    pub bindings: Vec<(String, String)>,
    pub fullscreen: bool,
    pub vsync: bool,
    /// `PSI_GraphicsPC_Resolution`: the display's size (fullscreen: its video mode; windowed: the
    /// window's), none for the desktop's
    pub resolution: Option<[u32; 2]>,
    /// `PSI_HUD_CrosshairStyle` (`ECrosshairStyle`): 0 off, 1 simple (the dot alone), 2 normal
    pub crosshair_style: u8,
    /// `PSI_HUD_bCrosshairMovement`: the weapons' reticles open with their dispersion
    pub crosshair_movement: bool,
    /// `PSI_HUD_CrosshairOpacity`: 0-100
    pub crosshair_opacity: f32,
    /// objective markers on the HUD
    #[serde(default = "yes")]
    pub markers: bool,
    /// `PSI_HUD_Visibility` (`EHUDVisibility`): the health and mana gauges: 0 off, 1 contextual
    /// (while they change or run low), 2 always
    pub hud_gauges: u8,
    /// the HUD's other parts (`PSI_HUD_bShow*`): objective popups, tutorials (the hints and
    /// their window), the interaction window, the focus highlight, the pickup log, the special
    /// moves' icons, the stance (the stealth shroud), the grenade warnings, the awareness
    /// markers, the Heart's marks
    pub objective_popups: bool,
    pub tutorials: bool,
    pub interactions: bool,
    pub focus_highlight: bool,
    pub pickup_log: bool,
    pub contextual_icons: bool,
    pub player_stance: bool,
    pub grenade_markers: bool,
    pub awareness_markers: bool,
    pub heart_markers: bool,
    /// `PSI_Gameplay_bAutoSaveInMenu`: opening the journal saves (not in a fight)
    pub auto_save_journal: bool,
    /// `PSI_Gameplay_HeadBobAmount`: 0-1
    pub head_bob: f32,
    /// `PSI_Gameplay_CameraRelativeClimbing`: on a chain, forward climbs the way Corvo looks
    pub camera_relative_climbing: bool,
    /// `PSI_GraphicsPC_ModelDetails`: 0 normal (the characters' lesser meshes at a distance),
    /// 1 high
    pub model_details: u8,
    /// `PSI_GraphicsPC_LightShaftEnable`
    pub light_shafts: bool,
    /// `PSI_GraphicsPC_RatShadows`: the swarms' rats cast shadows
    pub rat_shadows: bool,
    /// `PSI_AudioPC_SpeakerConfiguration`: 0 auto (the device's layout), 1 stereo, 2 5.1
    pub speaker_config: u8,
    /// display brightness (the level's gamma is scaled by it)
    pub brightness: f32,
    /// `PSI_GraphicsPC_TextureDetails`: 0 low, 1 medium, 2 high (the textures' largest mip at
    /// a level's load: 512, 1024, all)
    pub texture_detail: u8,
    /// `PSI_GraphicsPC_AntiAliasingMode`: 0 off, 1 MLAA (SMAA here), 2 FXAA; both over the
    /// scene's 4x multisampling
    pub anti_aliasing: u8,
    /// `PSI_Gameplay_bAutoUseManaElixir`: a power wanting mana drinks a remedy
    pub auto_mana_elixir: bool,
    /// `PSI_Mouse_bSmooting`: the mouse's movement averaged over two frames
    pub mouse_smoothing: bool,
    /// `PSI_Gameplay_KillCamMode`: the finishers' slow motion: 0 off, 1 normal (the last foe,
    /// else one in three), 2 frequent (every one)
    pub kill_cam: u8,
}

/// The display's sizes to choose from (its video modes, smallest first), as the window system
/// reports them.
pub static RESOLUTIONS: std::sync::RwLock<Vec<[u32; 2]>> = std::sync::RwLock::new(Vec::new());

/// The resolutions offered: the display's, else the common ones.
pub fn resolutions() -> Vec<[u32; 2]> {
    let r = RESOLUTIONS.read().map(|r| r.clone()).unwrap_or_default();
    if r.is_empty() {
        vec![[1280, 720], [1366, 768], [1600, 900], [1920, 1080], [2560, 1440], [3840, 2160]]
    } else {
        r
    }
}

/// The largest texture (texels a side) a level loads, by the texture detail setting; the
/// loader thread reads it.
pub static TEXTURE_CAP: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(u32::MAX);

/// The texture detail's largest texture.
pub fn texture_cap(detail: u8) -> u32 {
    match detail {
        0 => 512,
        1 => 1024,
        _ => u32::MAX,
    }
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            sensitivity: 2.0,
            invert_y: false,
            fov: 75.0,
            master_volume: 1.0,
            music_volume: 0.9,
            sfx_volume: 0.9,
            voice_volume: 1.0,
            subtitle_mode: 2,
            difficulty: 1,
            bindings: Vec::new(),
            fullscreen: false,
            vsync: true,
            resolution: None,
            crosshair_style: 2,
            crosshair_movement: true,
            crosshair_opacity: 100.0,
            markers: true,
            hud_gauges: 2,
            objective_popups: true,
            tutorials: true,
            interactions: true,
            focus_highlight: true,
            pickup_log: true,
            contextual_icons: true,
            player_stance: true,
            grenade_markers: true,
            awareness_markers: true,
            heart_markers: true,
            auto_save_journal: true,
            head_bob: 1.0,
            camera_relative_climbing: true,
            model_details: 0,
            light_shafts: true,
            rat_shadows: false,
            speaker_config: 0,
            brightness: 1.0,
            texture_detail: 2,
            // (`ArkProfileSettings`' defaults: MLAA, the remedies drunk as wanted)
            anti_aliasing: 1,
            auto_mana_elixir: true,
            mouse_smoothing: false,
            kill_cam: 1,
        }
    }
}

/// Where the game keeps its user data (options, saves).
pub fn user_dir() -> PathBuf {
    let base = std::env::var("APPDATA").map(PathBuf::from).unwrap_or_else(|_| crate::loading::cache_dir());
    let dir = base.join("DishonoredBevy");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

impl Settings {
    pub fn load() -> Settings {
        std::fs::read(user_dir().join("settings.json")).ok().and_then(|d| serde_json::from_slice(&d).ok()).unwrap_or_default()
    }

    pub fn save(&self) {
        if let Ok(d) = serde_json::to_vec_pretty(self) {
            let _ = std::fs::write(user_dir().join("settings.json"), d);
        }
    }

    pub fn difficulty_name(&self) -> &'static str {
        ["Easy", "Normal", "Hard", "Very Hard"][self.difficulty.min(3) as usize]
    }
}

pub struct SettingsPlugin;

impl Plugin for SettingsPlugin {
    fn build(&self, app: &mut App) {
        let s = Settings::load();
        TEXTURE_CAP.store(texture_cap(s.texture_detail), std::sync::atomic::Ordering::Relaxed);
        app.insert_resource(crate::bindings::Bindings::from_settings(&s)).insert_resource(s).init_resource::<crate::player::MouseSettings>().add_systems(Update, (list_resolutions, apply_settings, apply_anti_aliasing, rat_shadows));
    }
}

/// Settings take effect as they change.
fn apply_settings(
    settings: Res<Settings>,
    mut mouse: ResMut<crate::player::MouseSettings>,
    mut cams: Query<&mut Projection, With<crate::player::PlayerCamera>>,
    new_cams: Query<(), Added<crate::player::PlayerCamera>>,
    mut volume: ResMut<GlobalVolume>,
    (mut bind, mut windows): (ResMut<crate::bindings::Bindings>, Query<&mut Window, With<bevy::window::PrimaryWindow>>),
    monitors: Query<&bevy::window::Monitor>,
) {
    // (the texture detail: for the next level loaded)
    if settings.is_changed() {
        TEXTURE_CAP.store(texture_cap(settings.texture_detail), std::sync::atomic::Ordering::Relaxed);
    }
    if !settings.is_changed() && new_cams.is_empty() {
        return;
    }
    *bind = crate::bindings::Bindings::from_settings(&settings);
    crate::bindings::set_current(&bind);
    // the display (test runs keep their window): fullscreen at the desktop's size (borderless)
    // or in the video mode chosen; windowed at the size chosen
    if settings.is_changed() && std::env::var("DH_SCRIPT").is_err() {
        if let Ok(mut w) = windows.single_mut() {
            use bevy::window::{MonitorSelection, VideoModeSelection, WindowMode};
            let mode = match (settings.fullscreen, settings.resolution) {
                (true, Some(r)) => {
                    let best = monitors.iter().flat_map(|m| m.video_modes.iter()).filter(|v| v.physical_size == UVec2::from(r)).max_by_key(|v| (v.refresh_rate_millihertz, v.bit_depth)).cloned();
                    match best {
                        Some(v) => WindowMode::Fullscreen(MonitorSelection::Current, VideoModeSelection::Specific(v)),
                        None => WindowMode::BorderlessFullscreen(MonitorSelection::Current),
                    }
                }
                (true, None) => WindowMode::BorderlessFullscreen(MonitorSelection::Current),
                (false, _) => WindowMode::Windowed,
            };
            if w.mode != mode {
                w.mode = mode;
            }
            if let (false, Some([x, y])) = (settings.fullscreen, settings.resolution) {
                if w.resolution.physical_width() != x || w.resolution.physical_height() != y {
                    w.resolution.set_physical_resolution(x, y);
                }
            }
            if std::env::var("DH_NOVSYNC").is_err() {
                w.present_mode = if settings.vsync { bevy::window::PresentMode::AutoVsync } else { bevy::window::PresentMode::AutoNoVsync };
            }
        }
    }
    mouse.sensitivity = settings.sensitivity * 0.001;
    mouse.invert_y = settings.invert_y;
    mouse.smoothing = settings.mouse_smoothing;
    volume.volume = bevy::audio::Volume::Linear(settings.master_volume);
    for mut p in &mut cams {
        if let Projection::Perspective(pp) = &mut *p {
            pp.fov = settings.fov.clamp(55.0, 110.0).to_radians();
        }
    }
}

/// The display's video modes, for the resolution option.
fn list_resolutions(monitors: Query<&bevy::window::Monitor, Changed<bevy::window::Monitor>>, all: Query<&bevy::window::Monitor>) {
    if monitors.is_empty() {
        return;
    }
    let mut sizes: Vec<[u32; 2]> = all.iter().flat_map(|m| m.video_modes.iter()).map(|v| [v.physical_size.x, v.physical_size.y]).filter(|s| s[0] >= 800 && s[1] >= 600).collect();
    sizes.sort_by_key(|s| (s[0] * s[1], s[0]));
    sizes.dedup();
    if let Ok(mut r) = RESOLUTIONS.write() {
        *r = sizes;
    }
}

/// The rats of the swarms (`swarm::RatMesh`) cast shadows or not, as the option has it
/// (`bAllowRatsShadow`).
fn rat_shadows(mut commands: Commands, settings: Res<Settings>, rats: Query<Entity, With<crate::swarm::RatMesh>>, new: Query<Entity, Added<crate::swarm::RatMesh>>) {
    let all = settings.is_changed();
    for e in rats.iter().filter(|e| all || new.contains(*e)) {
        if settings.rat_shadows {
            commands.entity(e).remove::<bevy::light::NotShadowCaster>();
        } else {
            commands.entity(e).insert(bevy::light::NotShadowCaster);
        }
    }
}

/// The anti-aliasing mode onto the player's camera: off (no multisampling either), MLAA (SMAA)
/// or FXAA over 4x multisampling.
fn apply_anti_aliasing(mut commands: Commands, settings: Res<Settings>, cams: Query<Entity, With<crate::player::PlayerCamera>>, new_cams: Query<(), Added<crate::player::PlayerCamera>>) {
    if !settings.is_changed() && new_cams.is_empty() {
        return;
    }
    use bevy::anti_alias::{fxaa::Fxaa, smaa::Smaa};
    for e in &cams {
        let mut ec = commands.entity(e);
        ec.remove::<(Fxaa, Smaa)>();
        match settings.anti_aliasing {
            0 => {
                ec.insert(Msaa::Off);
            }
            1 => {
                ec.insert((Msaa::Sample4, Smaa::default()));
            }
            _ => {
                ec.insert((Msaa::Sample4, Fxaa::default()));
            }
        }
    }
}

fn yes() -> bool {
    true
}
