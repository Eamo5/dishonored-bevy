//! Front end and pause menu. The main menu runs inside the original menu map
//! (`Dishonored_MainMenu`), whose level scripts fly the camera when the menu fires their remote
//! events (`StartCam_Play` for the title screen, `MainMenu_Play` for the menu). Art and fonts
//! are the original Scaleform ones (logo, brush strokes, mask).

use crate::gameplay::PlayerStats;
use crate::hud::{HudRoot, Paused};
use crate::kismet::Vm;
use crate::level::{LevelInfo, LevelSpawnSet};
use crate::player::Player;
use crate::save::{LoadRequest, SaveRequest, SaveSlots};
use crate::settings::Settings;
use crate::ui_fonts::UiFonts;
use crate::ui_images::UiImages;
use crate::{Config, GameState};
use bevy::prelude::*;
use bevy::window::{CursorGrabMode, CursorOptions};

pub const MENU_MAP: &str = "Dishonored_MainMenu";
/// The first mission (the prologue at the Tower).
pub const FIRST_MAP: &str = "L_Tower_P";

pub struct MenuPlugin;

impl Plugin for MenuPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Menu>().init_resource::<HudHidden>()
            .add_systems(OnEnter(GameState::InGame), enter_level.after(LevelSpawnSet))
            .add_systems(Update, (open_pause, menu_input, menu_build, menu_world).chain().in_set(MenuSet).run_if(in_state(GameState::InGame)));
    }
}

/// The menu's systems (the front end draws after them).
#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub struct MenuSet;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MenuKind {
    Main,
    Pause,
    /// the pause screen after a death or the scripts' game over (`ShowGameOverMenu`)
    GameOver,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Page {
    Title,
    Main,
    Difficulty,
    Options,
    Controls,
    Load,
    Save,
    /// the downloadable content (`t_DownloadableContent_Caps`), the Dunwall City Trials'
    /// challenges, one challenge's modes
    Dlc,
    Challenges,
    Challenge(usize),
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Opt {
    Difficulty,
    Sensitivity,
    InvertY,
    Fov,
    Master,
    Music,
    Sfx,
    Voice,
    Subtitles,
    Fullscreen,
    VSync,
    Crosshair,
    Brightness,
    Markers,
    TextureDetail,
    AntiAliasing,
    AutoMana,
    Smoothing,
    KillCam,
    HudGauges,
    ObjPopups,
    Tutorials,
    Interactions,
    Highlight,
    PickupLog,
    ContextIcons,
    Stance,
    GrenadeMarkers,
    AwarenessMarkers,
    HeartMarkers,
    CrosshairMove,
    CrosshairOpacity,
    AutoSave,
    HeadBob,
    ClimbRelative,
    Resolution,
    ModelDetails,
    LightShafts,
    RatShadows,
    Speakers,
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Action {
    Continue,

    Difficulty(u8),
    Page(Page),
    Back,
    Adjust(Opt),
    /// bind an action to the next key pressed
    Rebind(crate::bindings::Act),
    ResetBindings,
    Load(usize),
    Save(usize),
    Resume,
    QuitToMenu,
    QuitGame,
    /// a challenge (`DisDLC05GameInfo.m_Challenges`), in expert mode or not
    StartChallenge(usize, bool),
}

#[derive(Resource)]
pub struct Menu {
    pub open: Option<MenuKind>,
    page: Page,
    stack: Vec<Page>,
    sel: usize,
    dirty: bool,
    items: Vec<(String, Action)>,
    started: bool,
    /// waiting for the key to bind to this action
    capture: Option<crate::bindings::Act>,
    /// the options' category (General, Controls, Graphics, Audio) and sub-category
    opt_cat: u8,
    opt_sub: u8,
    /// a question asked before an action (`ShowMessageBox`): its words, the action, the
    /// button chosen (0: yes)
    confirm: Option<(String, Action, usize)>,
    /// a button of the question clicked
    pub confirm_click: Option<usize>,
}

/// How an option row shows its value (`OptionsStepperB_widget`, `OptionsStepper_widget`,
/// `Slider_widget`, a key binding's box).
pub enum OptView {
    /// two choices side by side, the second chosen or not
    Toggle(bool),
    /// one of these words
    Stepper(Vec<String>, usize),
    /// a fraction along the track, and the number on its thumb
    Slider(f32, String),
    /// the key bound (or "press a key" while one is awaited)
    Key(String, bool),
}

/// The options' categories (`m_SettingsCategory_*`) and their sub-categories (none: an empty
/// list).
pub const OPT_CATEGORIES: [(&str, &[&str]); 4] = [
    ("m_SettingsCategory_General", &["m_SettingsSubCategory_GameplaySettings", "m_SettingsSubCategory_HUDSettings"]),
    ("m_SettingsCategory_Controls", &["m_SettingsSubCategory_KeyboardMapping", "m_SettingsSubCategory_MouseSettings"]),
    ("m_SettingsCategory_Graphics", &[]),
    ("m_SettingsCategory_Audio", &[]),
];

impl Default for Menu {
    fn default() -> Self {
        Menu { open: None, page: Page::Main, stack: Vec::new(), sel: 0, dirty: true, items: Vec::new(), started: false, capture: None, opt_cat: 0, opt_sub: 0, confirm: None, confirm_click: None }
    }
}

impl Menu {
    /// The main menu's screens the original movie draws (`frontend`): 0 the title, 1 the
    /// front page, 2 the new game's difficulties; with their items' labels.
    pub fn front_page(&self) -> Option<(u8, Vec<String>)> {
        let labels = || self.items.iter().map(|i| i.0.clone()).collect();
        match (self.open, self.page) {
            (Some(MenuKind::Main), Page::Title) => Some((0, Vec::new())),
            (Some(MenuKind::Main), Page::Main) => Some((1, labels())),
            (Some(MenuKind::Main), Page::Difficulty) => Some((2, labels())),
            (Some(MenuKind::Pause), Page::Main) => Some((3, labels())),
            (Some(MenuKind::GameOver), Page::Main) => Some((4, labels())),
            // (the save and load screens: `savescreen`)
            (Some(_), Page::Load) => Some((5, labels())),
            (Some(_), Page::Save) => Some((6, labels())),
            // (the options: `optscreen`)
            (Some(_), Page::Options) => Some((7, labels())),
            _ => None,
        }
    }
    /// The item chosen with the keys.
    pub fn selected(&self) -> usize {
        self.sel
    }
    /// The save slot an item of the save or load screen stands for.
    pub fn item_slot(&self, i: usize) -> Option<usize> {
        match self.items.get(i)?.1 {
            Action::Load(s) | Action::Save(s) => Some(s),
            _ => None,
        }
    }
    /// The main menu's (not over a game).
    pub fn in_main(&self) -> bool {
        self.open == Some(MenuKind::Main)
    }
    /// The question asked (its words, the button chosen), if any (`msgbox`).
    pub fn question(&self) -> Option<(&str, usize)> {
        self.confirm.as_ref().map(|c| (c.0.as_str(), c.2))
    }
    /// The options' category and sub-category shown.
    pub fn options_tab(&self) -> (u8, u8) {
        (self.opt_cat, self.opt_sub)
    }
    /// Show another category (its first sub-category) or sub-category.
    pub fn set_options_tab(&mut self, cat: u8, sub: u8) {
        if self.capture.is_some() || self.confirm.is_some() {
            return;
        }
        if (cat, sub) != (self.opt_cat, self.opt_sub) {
            self.opt_cat = cat.min(OPT_CATEGORIES.len() as u8 - 1);
            self.opt_sub = sub.min((OPT_CATEGORIES[self.opt_cat as usize].1.len() as u8).saturating_sub(1));
            self.sel = 0;
            self.capture = None;
            self.dirty = true;
        }
    }
    /// Whether keyboard input is being captured for a binding.
    pub fn capturing_binding(&self) -> bool {
        self.capture.is_some()
    }

    /// How the options' row `i` shows its value.
    pub fn option_view(&self, i: usize, s: &Settings, data: &crate::gamedata::Data) -> Option<OptView> {
        let words = |keys: &[&str]| keys.iter().map(|k| data.text("Settings.ProfileSettingValues", k)).collect::<Vec<_>>();
        let slider = |v: f32, lo: f32, hi: f32, shown: String| OptView::Slider(((v - lo) / (hi - lo)).clamp(0.0, 1.0), shown);
        Some(match self.items.get(i)?.1 {
            Action::Adjust(o) => match o {
                Opt::Difficulty => OptView::Stepper(words(&["EDifficulty_Easy", "EDifficulty_Normal", "EDifficulty_Hard", "EDifficulty_VeryHard"]), s.difficulty as usize),
                Opt::Crosshair => OptView::Stepper(words(&["CS_Off", "CS_Simple", "CS_Normal"]), s.crosshair_style.min(2) as usize),
                Opt::HudGauges => OptView::Stepper(words(&["HV_Off", "HV_Contextual", "HV_Always"]), s.hud_gauges.min(2) as usize),
                Opt::ModelDetails => OptView::Stepper(words(&["ModelDetails_Normal", "ModelDetails_High"]), s.model_details.min(1) as usize),
                Opt::Resolution => {
                    let list = crate::settings::resolutions();
                    let i = resolution_index(s, &list);
                    OptView::Stepper(list.iter().map(|r| format!("{}x{}", r[0], r[1])).collect(), i)
                }
                Opt::ObjPopups => OptView::Toggle(s.objective_popups),
                Opt::Tutorials => OptView::Toggle(s.tutorials),
                Opt::Interactions => OptView::Toggle(s.interactions),
                Opt::Highlight => OptView::Toggle(s.focus_highlight),
                Opt::PickupLog => OptView::Toggle(s.pickup_log),
                Opt::ContextIcons => OptView::Toggle(s.contextual_icons),
                Opt::Stance => OptView::Toggle(s.player_stance),
                Opt::GrenadeMarkers => OptView::Toggle(s.grenade_markers),
                Opt::AwarenessMarkers => OptView::Toggle(s.awareness_markers),
                Opt::HeartMarkers => OptView::Toggle(s.heart_markers),
                Opt::CrosshairMove => OptView::Toggle(s.crosshair_movement),
                Opt::AutoSave => OptView::Toggle(s.auto_save_journal),
                Opt::ClimbRelative => OptView::Toggle(s.camera_relative_climbing),
                Opt::LightShafts => OptView::Toggle(s.light_shafts),
                Opt::RatShadows => OptView::Toggle(s.rat_shadows),
                Opt::CrosshairOpacity => slider(s.crosshair_opacity, 0.0, 100.0, format!("{:.0}", s.crosshair_opacity)),
                Opt::HeadBob => slider(s.head_bob, 0.0, 1.0, format!("{:.0}", s.head_bob * 100.0)),
                Opt::Subtitles => OptView::Stepper(words(&["SubtitlesMode_Off", "SubtitlesMode_Dialogs", "SubtitlesMode_All"]), s.subtitle_mode.min(2) as usize),
                Opt::Speakers => OptView::Stepper(words(&["SpeakerConfiguration_Auto", "SpeakerConfiguration_Stereo", "SpeakerConfiguration_5Point1"]), s.speaker_config.min(2) as usize),
                Opt::TextureDetail => OptView::Stepper(words(&["TextureDetails_Low", "TextureDetails_Medium", "TextureDetails_High"]), s.texture_detail.min(2) as usize),
                Opt::AntiAliasing => OptView::Stepper(words(&["AntialiasingMode_Off", "AntialiasingMode_MLAA", "AntialiasingMode_FXAA"]), s.anti_aliasing.min(2) as usize),
                Opt::AutoMana => OptView::Toggle(s.auto_mana_elixir),
                Opt::KillCam => OptView::Stepper(words(&["KCM_Off", "KCM_Normal", "KCM_Frequent"]), s.kill_cam.min(2) as usize),
                Opt::Smoothing => OptView::Toggle(s.mouse_smoothing),
                Opt::InvertY => OptView::Toggle(s.invert_y),
                Opt::Fullscreen => OptView::Toggle(s.fullscreen),
                Opt::VSync => OptView::Toggle(s.vsync),
                Opt::Markers => OptView::Toggle(s.markers),
                Opt::Sensitivity => slider(s.sensitivity, 0.5, 6.0, format!("{:.0}", (s.sensitivity - 0.5) / 5.5 * 100.0)),
                Opt::Fov => slider(s.fov, 65.0, 110.0, format!("{:.0}", s.fov)),
                Opt::Brightness => slider(s.brightness, 0.5, 1.6, format!("{:.0}", (s.brightness - 0.5) / 1.1 * 100.0)),
                Opt::Master => slider(s.master_volume, 0.0, 1.0, format!("{:.0}", s.master_volume * 100.0)),
                Opt::Music => slider(s.music_volume, 0.0, 1.0, format!("{:.0}", s.music_volume * 100.0)),
                Opt::Sfx => slider(s.sfx_volume, 0.0, 1.0, format!("{:.0}", s.sfx_volume * 100.0)),
                Opt::Voice => slider(s.voice_volume, 0.0, 1.0, format!("{:.0}", s.voice_volume * 100.0)),
            },
            Action::Rebind(a) => {
                let bind = crate::bindings::Bindings::from_settings(s);
                OptView::Key(crate::bindings::display(bind.key(a)), self.capture == Some(a))
            }
            _ => return None,
        })
    }
    /// The category's settings back to their defaults (`OnResetOptions`).
    fn restore_settings(&self, s: &mut Settings) {
        let d = Settings::default();
        match (self.opt_cat, self.opt_sub) {
            (0, 0) => {
                s.difficulty = d.difficulty;
                s.auto_mana_elixir = d.auto_mana_elixir;
                s.kill_cam = d.kill_cam;
                s.auto_save_journal = d.auto_save_journal;
                s.head_bob = d.head_bob;
                s.camera_relative_climbing = d.camera_relative_climbing;
            }
            (0, _) => {
                s.markers = d.markers;
                s.crosshair_style = d.crosshair_style;
                s.crosshair_movement = d.crosshair_movement;
                s.crosshair_opacity = d.crosshair_opacity;
                s.hud_gauges = d.hud_gauges;
                s.objective_popups = d.objective_popups;
                s.tutorials = d.tutorials;
                s.interactions = d.interactions;
                s.focus_highlight = d.focus_highlight;
                s.pickup_log = d.pickup_log;
                s.contextual_icons = d.contextual_icons;
                s.player_stance = d.player_stance;
                s.grenade_markers = d.grenade_markers;
                s.awareness_markers = d.awareness_markers;
                s.heart_markers = d.heart_markers;
            }
            (1, 0) => s.bindings.clear(),
            (1, _) => {
                s.sensitivity = d.sensitivity;
                s.invert_y = d.invert_y;
                s.mouse_smoothing = d.mouse_smoothing;
            }
            (2, _) => {
                s.brightness = d.brightness;
                s.fullscreen = d.fullscreen;
                s.vsync = d.vsync;
                s.fov = d.fov;
                s.texture_detail = d.texture_detail;
                s.anti_aliasing = d.anti_aliasing;
                s.resolution = d.resolution;
                s.model_details = d.model_details;
                s.light_shafts = d.light_shafts;
                s.rat_shadows = d.rat_shadows;
            }
            _ => {
                s.master_volume = d.master_volume;
                s.music_volume = d.music_volume;
                s.sfx_volume = d.sfx_volume;
                s.voice_volume = d.voice_volume;
                s.subtitle_mode = d.subtitle_mode;
                s.speaker_config = d.speaker_config;
            }
        }
    }
    fn go(&mut self, page: Page) {
        self.stack.push(self.page);
        self.page = page;
        // (the new game opens on Normal: `NewGameMenu._curSelectionIdx`)
        self.sel = if page == Page::Difficulty { 1 } else { 0 };
        self.dirty = true;
    }
    fn back(&mut self) -> bool {
        match self.stack.pop() {
            Some(p) => {
                self.page = p;
                self.sel = 0;
                self.dirty = true;
                true
            }
            None => false,
        }
    }
}

#[derive(Component)]
struct MenuRoot;

#[derive(Component)]
pub struct MenuItem(pub usize);

fn enter_level(level: Option<Res<LevelInfo>>, mut menu: ResMut<Menu>, vm: Option<ResMut<Vm>>, mut launch: ResMut<crate::challenge::ChallengeLaunch>) {
    let main = level.as_ref().is_some_and(|l| l.scene.name.eq_ignore_ascii_case(MENU_MAP));
    *menu = Menu::default();
    if main {
        menu.open = Some(MenuKind::Main);
        menu.page = Page::Title;
        // (back from a challenge's results to the challenges)
        if std::mem::take(&mut launch.back_to_challenges) {
            menu.page = Page::Challenges;
            menu.stack = vec![Page::Main, Page::Dlc];
            menu.started = true;
        }
        if let Some(mut vm) = vm {
            vm.remote_event("StartCam_Play");
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn open_pause(
    keys: Res<ButtonInput<KeyCode>>,
    mut menu: ResMut<Menu>,
    stats: Res<PlayerStats>,
    mut paused: ResMut<Paused>,
    mission_end: Option<Res<crate::mission::MissionEnd>>,
    journal: Res<crate::journal::Journal>,
    store: Res<crate::store::Store>,
    (time, data, mut dead_t): (Res<Time<Real>>, Res<crate::gamedata::Data>, Local<f32>),
    note: Res<crate::notescreen::NoteScreen>,
    challenge: Res<crate::challenge::Challenge>,
) {
    // dead (or the scripts ended it): the game over menu, once the death has faded
    // (`m_fDeathFadeDelay`); in a challenge its scripts decide (`challenge`)
    if (stats.dead || stats.game_over.is_some()) && !challenge.active() {
        *dead_t += time.delta_secs();
        let delay = if stats.game_over.is_some() { 0.0 } else { data.pawn("m_fDeathFadeDelay", 5.0) };
        if *dead_t >= delay && menu.open.is_none() {
            menu.open = Some(MenuKind::GameOver);
            menu.page = Page::Main;
            menu.stack.clear();
            menu.sel = 0;
            menu.dirty = true;
        }
    } else {
        *dead_t = 0.0;
    }
    if mission_end.is_some() {
        paused.0 = true;
        return;
    }
    // the journal and stores close on Escape themselves
    if journal.open || store.open.is_some() || journal.is_changed() || store.is_changed() || note.open.is_some() || note.is_changed() {
        return;
    }
    if menu.open.is_none() && keys.just_pressed(KeyCode::Escape) && !stats.dead {
        menu.open = Some(MenuKind::Pause);
        menu.page = Page::Main;
        menu.stack.clear();
        menu.sel = 0;
        menu.dirty = true;
    }
    paused.0 = matches!(menu.open, Some(MenuKind::Pause | MenuKind::GameOver));
}

fn opt_value(s: &Settings, o: Opt) -> String {
    let pct = |v: f32| format!("{:.0}%", v * 100.0);
    match o {
        Opt::Difficulty => s.difficulty_name().to_string(),
        Opt::Sensitivity => format!("{:.1}", s.sensitivity),
        Opt::InvertY => if s.invert_y { "On" } else { "Off" }.into(),
        Opt::Fov => format!("{:.0}", s.fov),
        Opt::Master => pct(s.master_volume),
        Opt::Music => pct(s.music_volume),
        Opt::Sfx => pct(s.sfx_volume),
        Opt::Voice => pct(s.voice_volume),
        Opt::Subtitles => ["Off", "Main Dialogue", "All Speeches"][s.subtitle_mode.min(2) as usize].into(),
        Opt::Speakers => ["Auto", "Stereo", "5.1"][s.speaker_config.min(2) as usize].into(),
        Opt::Fullscreen => if s.fullscreen { "Fullscreen" } else { "Windowed" }.into(),
        Opt::VSync => if s.vsync { "On" } else { "Off" }.into(),
        Opt::Crosshair => ["Off", "Simple", "Normal"][s.crosshair_style.min(2) as usize].into(),
        Opt::HudGauges => ["Off", "Contextual", "Always"][s.hud_gauges.min(2) as usize].into(),
        Opt::ModelDetails => ["Normal", "High"][s.model_details.min(1) as usize].into(),
        Opt::Resolution => s.resolution.map(|r| format!("{}x{}", r[0], r[1])).unwrap_or_else(|| "Desktop".into()),
        Opt::CrosshairOpacity => format!("{:.0}", s.crosshair_opacity),
        Opt::HeadBob => pct(s.head_bob),
        Opt::ObjPopups => on(s.objective_popups),
        Opt::Tutorials => on(s.tutorials),
        Opt::Interactions => on(s.interactions),
        Opt::Highlight => on(s.focus_highlight),
        Opt::PickupLog => on(s.pickup_log),
        Opt::ContextIcons => on(s.contextual_icons),
        Opt::Stance => on(s.player_stance),
        Opt::GrenadeMarkers => on(s.grenade_markers),
        Opt::AwarenessMarkers => on(s.awareness_markers),
        Opt::HeartMarkers => on(s.heart_markers),
        Opt::CrosshairMove => on(s.crosshair_movement),
        Opt::AutoSave => on(s.auto_save_journal),
        Opt::ClimbRelative => on(s.camera_relative_climbing),
        Opt::LightShafts => on(s.light_shafts),
        Opt::RatShadows => on(s.rat_shadows),
        Opt::Markers => if s.markers { "On" } else { "Off" }.into(),
        Opt::Brightness => pct(s.brightness),
        Opt::TextureDetail => ["Low", "Medium", "High"][s.texture_detail.min(2) as usize].into(),
        Opt::AntiAliasing => ["Off", "MLAA", "FXAA"][s.anti_aliasing.min(2) as usize].into(),
        Opt::AutoMana => if s.auto_mana_elixir { "On" } else { "Off" }.into(),
        Opt::KillCam => ["Off", "Normal", "Frequent"][s.kill_cam.min(2) as usize].into(),
        Opt::Smoothing => if s.mouse_smoothing { "On" } else { "Off" }.into(),
    }
}

/// The resolution chosen among those offered (the desktop's: the largest).
fn resolution_index(s: &Settings, list: &[[u32; 2]]) -> usize {
    s.resolution.and_then(|r| list.iter().position(|x| *x == r)).unwrap_or(list.len().saturating_sub(1))
}

fn on(b: bool) -> String {
    if b { "On" } else { "Off" }.into()
}

fn adjust(s: &mut Settings, o: Opt, dir: f32) {
    let step = |v: &mut f32, d: f32, lo: f32, hi: f32| {
        *v = ((*v + d * dir) * 100.0).round() / 100.0;
        if *v > hi + 1e-3 {
            *v = lo;
        } else if *v < lo - 1e-3 {
            *v = hi;
        }
    };
    match o {
        // (no wrapping round: `isLoop` false)
        Opt::Difficulty => s.difficulty = (s.difficulty as f32 + dir).clamp(0.0, 3.0) as u8,
        Opt::Sensitivity => step(&mut s.sensitivity, 0.25, 0.5, 6.0),
        Opt::InvertY => s.invert_y = !s.invert_y,
        Opt::Fov => step(&mut s.fov, 5.0, 65.0, 110.0),
        Opt::Master => step(&mut s.master_volume, 0.1, 0.0, 1.0),
        Opt::Music => step(&mut s.music_volume, 0.1, 0.0, 1.0),
        Opt::Sfx => step(&mut s.sfx_volume, 0.1, 0.0, 1.0),
        Opt::Voice => step(&mut s.voice_volume, 0.1, 0.0, 1.0),
        Opt::Subtitles => s.subtitle_mode = (s.subtitle_mode as f32 + dir).clamp(0.0, 2.0) as u8,
        Opt::Speakers => s.speaker_config = (s.speaker_config as f32 + dir).clamp(0.0, 2.0) as u8,
        Opt::Fullscreen => s.fullscreen = !s.fullscreen,
        Opt::VSync => s.vsync = !s.vsync,
        Opt::Crosshair => s.crosshair_style = (s.crosshair_style as f32 + dir).clamp(0.0, 2.0) as u8,
        Opt::HudGauges => s.hud_gauges = (s.hud_gauges as f32 + dir).clamp(0.0, 2.0) as u8,
        Opt::ModelDetails => s.model_details = (s.model_details as f32 + dir).clamp(0.0, 1.0) as u8,
        Opt::Resolution => {
            let list = crate::settings::resolutions();
            let i = (resolution_index(s, &list) as f32 + dir).clamp(0.0, list.len().saturating_sub(1) as f32) as usize;
            s.resolution = list.get(i).copied();
        }
        // (`PSI_HUD_CrosshairOpacity` 0-100 by 10, `PSI_Gameplay_HeadBobAmount` 0-1 by 0.1)
        Opt::CrosshairOpacity => s.crosshair_opacity = (s.crosshair_opacity + 10.0 * dir).clamp(0.0, 100.0),
        Opt::HeadBob => s.head_bob = ((s.head_bob + 0.1 * dir).clamp(0.0, 1.0) * 10.0).round() / 10.0,
        Opt::ObjPopups => s.objective_popups = !s.objective_popups,
        Opt::Tutorials => s.tutorials = !s.tutorials,
        Opt::Interactions => s.interactions = !s.interactions,
        Opt::Highlight => s.focus_highlight = !s.focus_highlight,
        Opt::PickupLog => s.pickup_log = !s.pickup_log,
        Opt::ContextIcons => s.contextual_icons = !s.contextual_icons,
        Opt::Stance => s.player_stance = !s.player_stance,
        Opt::GrenadeMarkers => s.grenade_markers = !s.grenade_markers,
        Opt::AwarenessMarkers => s.awareness_markers = !s.awareness_markers,
        Opt::HeartMarkers => s.heart_markers = !s.heart_markers,
        Opt::CrosshairMove => s.crosshair_movement = !s.crosshair_movement,
        Opt::AutoSave => s.auto_save_journal = !s.auto_save_journal,
        Opt::ClimbRelative => s.camera_relative_climbing = !s.camera_relative_climbing,
        Opt::LightShafts => s.light_shafts = !s.light_shafts,
        Opt::RatShadows => s.rat_shadows = !s.rat_shadows,
        Opt::Markers => s.markers = !s.markers,
        Opt::Brightness => step(&mut s.brightness, 0.05, 0.5, 1.6),
        Opt::TextureDetail => s.texture_detail = (s.texture_detail as f32 + dir).clamp(0.0, 2.0) as u8,
        Opt::AntiAliasing => s.anti_aliasing = (s.anti_aliasing as f32 + dir).clamp(0.0, 2.0) as u8,
        Opt::AutoMana => s.auto_mana_elixir = !s.auto_mana_elixir,
        Opt::KillCam => s.kill_cam = (s.kill_cam as f32 + dir).clamp(0.0, 2.0) as u8,
        Opt::Smoothing => s.mouse_smoothing = !s.mouse_smoothing,
    }
}

#[cfg(test)]
mod modal_tests {
    use super::*;

    #[test]
    fn option_tabs_cannot_redirect_a_pending_restore_or_key_capture() {
        let mut menu = Menu::default();
        menu.set_options_tab(1, 0);
        menu.confirm = Some(("Restore keyboard mappings?".into(), Action::ResetBindings, 0));
        menu.set_options_tab(2, 0);
        assert_eq!(menu.options_tab(), (1, 0));
        let mut settings = Settings::default();
        settings.bindings.push(("forward".into(), "KeyZ".into()));
        settings.brightness = 1.3;
        menu.restore_settings(&mut settings);
        assert!(settings.bindings.is_empty());
        assert_eq!(settings.brightness, 1.3);

        menu.confirm = None;
        menu.capture = Some(crate::bindings::Act::Forward);
        menu.set_options_tab(1, 1);
        assert_eq!(menu.options_tab(), (1, 0));
        assert!(menu.capturing_binding());
        menu.capture = None;
        menu.set_options_tab(2, 0);
        assert_eq!(menu.options_tab(), (2, 0));
    }
}

fn items(menu: &Menu, settings: &Settings, slots: &SaveSlots, data: &crate::gamedata::Data, profile: &crate::challenge::ChallengeProfile) -> Vec<(String, Action)> {
    let has_saves = slots.any();
    match (menu.open, menu.page) {
        (_, Page::Title) => vec![("Press any key".into(), Action::Page(Page::Main))],
        (Some(MenuKind::Main), Page::Main) => {
            let mut v = Vec::new();
            // (the original's words: `t_ContinueGame`, `t_NewGame`, `t_LoadGame`, `t_Options`,
            // `t_QuitGame`)
            if has_saves {
                v.push(("Continue".into(), Action::Continue));
            }
            v.push(("New Game".into(), Action::Page(Page::Difficulty)));
            if has_saves {
                v.push(("Load".into(), Action::Page(Page::Load)));
            }
            // (`t_DownloadableContent_Caps`: the installed DLC)
            if !data.0.challenges.is_empty() {
                v.push(("Downloadable Content".into(), Action::Page(Page::Dlc)));
            }
            v.push(("Options".into(), Action::Page(Page::Options)));
            v.push(("Quit Game".into(), Action::QuitGame));
            v
        }
        (_, Page::Dlc) => vec![(data.text("DisDLC05MoviePlayerChallengeMenu_Texts", "t_DLC05_Name"), Action::Page(Page::Challenges)), ("Back".into(), Action::Back)],
        (_, Page::Challenges) => {
            // each challenge with its stars (its medals reached by the best score)
            let mut v: Vec<(String, Action)> = data
                .0
                .challenges
                .iter()
                .enumerate()
                .map(|(i, c)| {
                    let best = profile.best.get(&c.id).copied().unwrap_or(0);
                    let stars = c.medals.iter().filter(|m| **m > 0 && best >= **m as i64).count();
                    (format!("{}   {}", c.name, "*".repeat(stars)), Action::Page(Page::Challenge(i)))
                })
                .collect();
            v.push(("Back".into(), Action::Back));
            v
        }
        (_, Page::Challenge(i)) => {
            let base = "DisDLC05MoviePlayerChallengeMenu_Texts";
            let start = data.text(base, "t_StartChallenge");
            vec![(start.clone(), Action::StartChallenge(i, false)), (format!("{}{start}", data.text(base, "t_ExpertModeTitle")), Action::StartChallenge(i, true)), ("Back".into(), Action::Back)]
        }
        // (`SetGameOverMenu`: `t_ContinueFromLastSave`, `t_LoadASpecificSave`,
        // `t_BackToMainMenu`, `t_BackToWindows`)
        (Some(MenuKind::GameOver), Page::Main) => vec![
            ("Resume From Last Save".into(), Action::Continue),
            ("Load game".into(), Action::Page(Page::Load)),
            ("Back to Main Menu".into(), Action::QuitToMenu),
            ("Back to Windows".into(), Action::QuitGame),
        ],
        // (the original's: `t_ResumeGame`, `t_SaveGame`, `t_LoadGamePauseMenu`, `t_Options`,
        // `t_BackToMainMenu`, `t_BackToWindows`)
        (_, Page::Main) => vec![
            ("Resume".into(), Action::Resume),
            ("Save Game".into(), Action::Page(Page::Save)),
            ("Load Game".into(), Action::Page(Page::Load)),
            ("Options".into(), Action::Page(Page::Options)),
            ("Back to Main Menu".into(), Action::QuitToMenu),
            ("Back to Windows".into(), Action::QuitGame),
        ],
        (_, Page::Difficulty) => {
            let mut v: Vec<(String, Action)> = ["Easy", "Normal", "Hard", "Very Hard"].iter().enumerate().map(|(i, n)| (n.to_string(), Action::Difficulty(i as u8))).collect();
            v.push(("Back".into(), Action::Back));
            v
        }
        (_, Page::Options) => {
            let label = |k: &str| data.text("Settings.ProfileSettingIDs", k);
            let rows: Vec<(&str, Opt)> = match (menu.opt_cat, menu.opt_sub) {
                // (each sub-category's settings in `EProfileSettingID`'s order)
                (0, 0) => vec![
                    ("PSI_Gameplay_Difficulty", Opt::Difficulty),
                    ("PSI_Gameplay_KillCamMode", Opt::KillCam),
                    ("PSI_Gameplay_bAutoUseManaElixir", Opt::AutoMana),
                    ("PSI_Gameplay_bAutoSaveInMenu", Opt::AutoSave),
                    ("PSI_Gameplay_HeadBobAmount", Opt::HeadBob),
                    ("PSI_Gameplay_CameraRelativeClimbing", Opt::ClimbRelative),
                ],
                (0, _) => vec![
                    ("PSI_HUD_Visibility", Opt::HudGauges),
                    ("PSI_HUD_bShowObjectivePopups", Opt::ObjPopups),
                    ("PSI_HUD_bShowTutorialNotifications", Opt::Tutorials),
                    ("PSI_HUD_bShowInteractions", Opt::Interactions),
                    ("PSI_HUD_bShowHighlight", Opt::Highlight),
                    ("PSI_HUD_bShowPickupLog", Opt::PickupLog),
                    ("PSI_HUD_bShowContextualIcons", Opt::ContextIcons),
                    ("PSI_HUD_bShowPlayerStance", Opt::Stance),
                    ("PSI_HUD_bShowObjectiveMarkers", Opt::Markers),
                    ("PSI_HUD_bShowGrenadeMarkers", Opt::GrenadeMarkers),
                    ("PSI_HUD_bShowAwarenessMarkers", Opt::AwarenessMarkers),
                    ("PSI_HUD_bShowHeartTargetMarkers", Opt::HeartMarkers),
                    ("PSI_HUD_CrosshairStyle", Opt::Crosshair),
                    ("PSI_HUD_bCrosshairMovement", Opt::CrosshairMove),
                    ("PSI_HUD_CrosshairOpacity", Opt::CrosshairOpacity),
                ],
                (1, 0) => {
                    let mut v: Vec<(String, Action)> = crate::bindings::Act::ALL.iter().map(|a| (data.text("Settings.ProfileSettingIDs", &format!("PSI_GBA_{}", a.psi())), Action::Rebind(*a))).collect();
                    for (l, a) in v.iter_mut() {
                        if l.is_empty() {
                            if let Action::Rebind(act) = a {
                                *l = act.label().to_string();
                            }
                        }
                    }
                    return v;
                }
                (1, _) => vec![("PSI_Mouse_Sensitivity", Opt::Sensitivity), ("PSI_Mouse_bInvertY", Opt::InvertY), ("PSI_Mouse_bSmooting", Opt::Smoothing)],
                (2, _) => vec![
                    ("PSI_Graphics_Gamma", Opt::Brightness),
                    ("PSI_GraphicsPC_Resolution", Opt::Resolution),
                    ("PSI_GraphicsPC_bFullScreen", Opt::Fullscreen),
                    ("PSI_GraphicsPC_bVSync", Opt::VSync),
                    ("PSI_GraphicsPC_FOV", Opt::Fov),
                    ("PSI_GraphicsPC_TextureDetails", Opt::TextureDetail),
                    ("PSI_GraphicsPC_ModelDetails", Opt::ModelDetails),
                    ("PSI_GraphicsPC_LightShaftEnable", Opt::LightShafts),
                    ("PSI_GraphicsPC_AntiAliasingMode", Opt::AntiAliasing),
                    ("PSI_GraphicsPC_RatShadows", Opt::RatShadows),
                ],
                _ => vec![
                    ("PSI_Audio_GlobalVolume", Opt::Master),
                    ("PSI_Audio_MusicVolume", Opt::Music),
                    ("PSI_Audio_SFXVolume", Opt::Sfx),
                    ("PSI_Audio_VoicesVolume", Opt::Voice),
                    ("PSI_Audio_SubtitlesMode", Opt::Subtitles),
                    ("PSI_AudioPC_SpeakerConfiguration", Opt::Speakers),
                ],
            };
            return rows.into_iter().map(|(k, o)| (label(k), Action::Adjust(o))).collect();
        }
        (_, Page::Controls) => {
            let bind = crate::bindings::Bindings::from_settings(settings);
            let mut v: Vec<(String, Action)> = crate::bindings::Act::ALL
                .iter()
                .map(|a| {
                    let key = if menu.capture == Some(*a) { "Press a key...".to_string() } else { crate::bindings::display(bind.key(*a)) };
                    (format!("{}   {key}", a.label()), Action::Rebind(*a))
                })
                .collect();
            v.push(("Reset to Defaults".into(), Action::ResetBindings));
            v.push(("Back".into(), Action::Back));
            v
        }
        // the saves, newest first (`savescreen` draws them; Escape goes back)
        (_, Page::Load) => slots.list().into_iter().map(|(i, desc)| (desc, Action::Load(i))).collect(),
        // a new save first (the first free slot), then the saves to overwrite
        (_, Page::Save) => {
            let mut v: Vec<(String, Action)> = Vec::new();
            if let Some(free) = (1..=8).find(|i| slots.describe(*i).is_none()) {
                v.push(("Create a new save".into(), Action::Save(free)));
            }
            v.extend(slots.list().into_iter().filter(|(i, _)| (1..=8).contains(i)).map(|(i, desc)| (desc, Action::Save(i))));
            v
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn menu_input(
    keys: Res<ButtonInput<KeyCode>>,
    mouse: Res<ButtonInput<MouseButton>>,
    mut menu: ResMut<Menu>,
    mut settings: ResMut<Settings>,
    mut config: ResMut<Config>,
    mut next: ResMut<NextState<GameState>>,
    mut exit: MessageWriter<AppExit>,
    (mut save, mut load, slots): (MessageWriter<SaveRequest>, MessageWriter<LoadRequest>, Res<SaveSlots>),
    hovered: Query<(&Interaction, &MenuItem), Changed<Interaction>>,
    (vm, mut campaign, scripted, mut intro): (Option<ResMut<Vm>>, ResMut<crate::gameplay::Campaign>, Option<Res<crate::script::Scripted>>, ResMut<crate::movie::IntroPending>),
    (data, mut launch): (Res<crate::gamedata::Data>, ResMut<crate::challenge::ChallengeLaunch>),
) {
    let Some(kind) = menu.open else { return };
    // binding a key: the next one pressed (Escape cancels)
    if let Some(a) = menu.capture {
        if keys.just_pressed(KeyCode::Escape) {
            menu.capture = None;
            menu.dirty = true;
        } else if let Some(k) = keys.get_just_pressed().copied().find(|k| crate::bindings::SUPPORTED.contains(k)) {
            crate::bindings::Bindings::rebind(&mut settings, a, k);
            settings.save();
            menu.capture = None;
            menu.dirty = true;
        }
        return;
    }
    // a question asked: Left / Right choose, Enter answers, Escape says no
    let mut answered = None;
    if let Some((_, action, choice)) = menu.confirm.clone() {
        let mut choice = choice;
        if keys.just_pressed(KeyCode::ArrowLeft) || keys.just_pressed(KeyCode::ArrowRight) || keys.just_pressed(KeyCode::KeyA) || keys.just_pressed(KeyCode::KeyD) {
            choice = 1 - choice.min(1);
        }
        let click = menu.confirm_click.take();
        if let Some(c) = click {
            choice = c;
        }
        if keys.just_pressed(KeyCode::Escape) {
            menu.confirm = None;
            menu.dirty = true;
            return;
        }
        if keys.just_pressed(KeyCode::Enter) || keys.just_pressed(KeyCode::Space) || keys.just_pressed(KeyCode::KeyE) || click.is_some() {
            menu.confirm = None;
            menu.dirty = true;
            if choice != 0 {
                return;
            }
            answered = Some(action);
        } else {
            if let Some(c) = menu.confirm.as_mut() {
                if c.2 != choice {
                    c.2 = choice;
                }
            }
            return;
        }
    }
    if menu.page == Page::Title {
        if keys.get_just_pressed().next().is_some() || mouse.get_just_pressed().next().is_some() {
            if let Some(mut vm) = vm {
                vm.remote_event("StartCam_Stop");
                vm.remote_event("MainMenu_Play");
            }
            menu.page = Page::Main;
            menu.sel = 0;
            menu.dirty = true;
        }
        return;
    }
    // the options: Tab / Shift+Tab the categories (`LB`/`RB`), Q / E the sub-categories
    // (`LT`/`RT`), R restores the category's settings (`X`)
    if menu.page == Page::Options && menu.capture.is_none() {
        let (cat, sub) = menu.options_tab();
        let ncat = OPT_CATEGORIES.len() as u8;
        let nsub = OPT_CATEGORIES[cat as usize].1.len() as u8;
        if keys.just_pressed(KeyCode::Tab) {
            let back = keys.pressed(KeyCode::ShiftLeft) || keys.pressed(KeyCode::ShiftRight);
            menu.set_options_tab(if back { (cat + ncat - 1) % ncat } else { (cat + 1) % ncat }, 0);
            return;
        }
        if nsub > 1 && (keys.just_pressed(KeyCode::KeyQ) || keys.just_pressed(KeyCode::KeyE)) {
            menu.set_options_tab(cat, if keys.just_pressed(KeyCode::KeyE) { (sub + 1) % nsub } else { (sub + nsub - 1) % nsub });
            return;
        }
        if answered.is_none() && keys.just_pressed(KeyCode::KeyR) {
            // (asked first: `t_Q_RestoreSettings`, the category named in blue)
            let (cat, sub) = menu.options_tab();
            let (c, subs) = OPT_CATEGORIES[cat as usize];
            let mut name = data.text("Twk_GFxMoviePlayerMenuBase", c);
            if let Some(s) = subs.get(sub as usize) {
                name = format!("{name} -> {}", data.text("Twk_GFxMoviePlayerMenuBase", s));
            }
            let q = data.text("DisGFxMoviePlayerMenuBase_Texts", "t_Q_RestoreSettings").replace("§CATEGORY§", &name);
            menu.confirm = Some((q, Action::ResetBindings, 0));
            menu.dirty = true;
            return;
        }
    }
    let n = menu.items.len();
    if n == 0 {
        // (an empty list, once built: only Escape, back; a menu just opened has none yet)
        if !menu.dirty && keys.just_pressed(KeyCode::Escape) && !menu.back() && kind == MenuKind::Pause {
            menu.open = None;
            menu.dirty = true;
        }
        return;
    }
    let mut activate = None;
    let mut dir = 0.0;
    // (a scripted run ignores the real pointer)
    for (i, item) in hovered.iter().filter(|_| scripted.is_none()) {
        match i {
            Interaction::Hovered => {
                if menu.sel != item.0 {
                    menu.sel = item.0;
                    menu.dirty = true;
                }
            }
            Interaction::Pressed => {
                menu.sel = item.0;
                activate = Some(item.0);
                dir = 1.0;
            }
            Interaction::None => {}
        }
    }
    if keys.just_pressed(KeyCode::ArrowDown) || keys.just_pressed(KeyCode::KeyS) {
        menu.sel = (menu.sel + 1) % n;
        menu.dirty = true;
    }
    if keys.just_pressed(KeyCode::ArrowUp) || keys.just_pressed(KeyCode::KeyW) {
        menu.sel = (menu.sel + n - 1) % n;
        menu.dirty = true;
    }
    if keys.just_pressed(KeyCode::Enter) || keys.just_pressed(KeyCode::Space) || keys.just_pressed(KeyCode::KeyE) {
        activate = Some(menu.sel);
        dir = 1.0;
    }
    let left_right = if keys.just_pressed(KeyCode::ArrowRight) || keys.just_pressed(KeyCode::KeyD) {
        1.0
    } else if keys.just_pressed(KeyCode::ArrowLeft) || keys.just_pressed(KeyCode::KeyA) {
        -1.0
    } else {
        0.0
    };
    // the front page's bar runs across the screen: left and right move along it
    if left_right != 0.0 && menu.front_page().is_some_and(|f| f.0 == 1) {
        menu.sel = if left_right > 0.0 { (menu.sel + 1) % n } else { (menu.sel + n - 1) % n };
        menu.dirty = true;
    }
    if left_right != 0.0 {
        if let Some((_, Action::Adjust(_))) = menu.items.get(menu.sel) {
            activate = Some(menu.sel);
            dir = left_right;
        }
    }
    if keys.just_pressed(KeyCode::Escape) && activate.is_none() {
        if !menu.back() && kind == MenuKind::Pause {
            menu.open = None;
            menu.dirty = true;
        }
        return;
    }
    // the action: chosen, or the question's yes
    let action = match answered {
        Some(a) => a,
        None => {
            let Some(i) = activate else { return };
            let Some((_, action)) = menu.items.get(i).cloned() else { return };
            // asked first, as the original asks (`ShowMessageBox`)
            let base = "DisGFxMoviePlayerMenuBase_Texts";
            let question = match action {
                Action::Save(slot) if slots.describe(slot).is_some() => Some(data.text(base, "t_Q_SaveGame")),
                Action::Load(_) => Some(data.text(base, if kind == MenuKind::Main { "t_Q_LoadGameMainMenu" } else { "t_Q_LoadGame" })),
                Action::QuitToMenu => Some(data.text(base, "t_Q_BackToMainMenu")),
                Action::QuitGame => Some(data.text(base, if kind == MenuKind::Main { "t_Q_QuitGame" } else { "t_Q_BackToWindows" })),
                Action::Difficulty(_) if slots.any() => Some(data.text("DisGFxMoviePlayerMainMenu_Texts", "t_Q_NewGame")),
                _ => None,
            };
            if let Some(q) = question.filter(|q| !q.is_empty()) {
                menu.confirm = Some((q, action, 0));
                menu.dirty = true;
                return;
            }
            action
        }
    };
    match action {
        Action::Page(p) => menu.go(p),
        Action::Back => {
            menu.back();
        }
        Action::Adjust(o) => {
            adjust(&mut settings, o, dir);
            settings.save();
            menu.dirty = true;
        }
        Action::Rebind(a) => {
            menu.capture = Some(a);
            menu.dirty = true;
        }
        Action::ResetBindings => {
            // (the options' restore: the category shown)
            if menu.page == Page::Options {
                menu.restore_settings(&mut settings);
            } else {
                settings.bindings.clear();
            }
            settings.save();
            menu.dirty = true;
        }
        Action::Difficulty(d) => {
            *campaign = Default::default();
            settings.difficulty = d;
            settings.save();
            config.map = FIRST_MAP.into();
            config.spawn_index = None;
            menu.open = None;
            // (the intro plays while the Tower loads)
            intro.0 = true;
            next.set(GameState::Loading);
        }
        Action::Continue => {
            if let Some(slot) = slots.latest() {
                load.write(LoadRequest(slot));
                menu.open = None;
            } else if kind == MenuKind::GameOver {
                // (nothing saved: the mission again)
                menu.open = None;
                next.set(GameState::Loading);
            }
        }
        Action::Load(slot) => {
            load.write(LoadRequest(slot));
            menu.open = None;
        }
        Action::Save(slot) => {
            save.write(SaveRequest(slot));
            menu.back();
        }
        Action::Resume => {
            menu.open = None;
            menu.dirty = true;
        }
        Action::QuitToMenu => {
            config.map = MENU_MAP.into();
            config.spawn_index = None;
            menu.open = None;
            next.set(GameState::Loading);
        }
        Action::QuitGame => {
            exit.write(AppExit::Success);
        }
        Action::StartChallenge(i, expert) => {
            if let Some(c) = data.0.challenges.get(i) {
                config.map = c.map.clone();
                config.spawn_index = None;
                launch.expert = expert;
                menu.open = None;
                next.set(GameState::Loading);
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn menu_build(
    mut commands: Commands,
    mut menu: ResMut<Menu>,
    settings: Res<Settings>,
    slots: Res<SaveSlots>,
    fonts: Res<UiFonts>,
    mut ui: ResMut<UiImages>,
    mut images: ResMut<Assets<Image>>,
    roots: Query<Entity, With<MenuRoot>>,
    (data, profile): (Res<crate::gamedata::Data>, Res<crate::challenge::ChallengeProfile>),
) {
    if !menu.dirty && !(settings.is_changed() && menu.page == Page::Options) {
        return;
    }
    menu.dirty = false;
    for e in &roots {
        commands.entity(e).despawn();
    }
    let Some(kind) = menu.open else { return };
    menu.items = items(&menu, &settings, &slots, &data, &profile);
    if menu.sel >= menu.items.len() {
        menu.sel = 0;
    }
    // the title screen and the front page are the original movie's (`frontend`)
    if menu.front_page().is_some() {
        return;
    }
    let gold = Color::srgb(0.86, 0.80, 0.66);
    let white = Color::srgb(0.98, 0.96, 0.92);
    let title_font: bevy::text::FontSource = fonts.title.clone().into();
    let logo = ui.get(&mut images, "MainMenu", 162).map(|x| x.0);
    let sel_bar = ui.get(&mut images, "MainMenu", 2).map(|x| x.0);
    let title_bar = ui.get(&mut images, "PauseMenu", 1).map(|x| x.0);
    let mask = ui.get(&mut images, "PauseMenu", 77).map(|x| x.0);
    let over_game = kind != MenuKind::Main;
    let bg = if over_game { Color::srgba(0.01, 0.01, 0.015, 0.72) } else { Color::NONE };
    let root = commands
        .spawn((
            MenuRoot,
            Node { position_type: PositionType::Absolute, width: percent(100), height: percent(100), ..default() },
            BackgroundColor(bg),
            GlobalZIndex(50),
            DespawnOnExit(GameState::InGame),
        ))
        .id();
    commands.entity(root).with_children(|r| {
        if over_game {
            if let Some(m) = &mask {
                r.spawn((ImageNode::new(m.clone()).with_color(Color::srgba(1.0, 1.0, 1.0, 0.55)), Node { position_type: PositionType::Absolute, right: percent(4), top: percent(12), height: percent(78), aspect_ratio: Some(1.0), ..default() }));
            }
            if let Some(b) = &title_bar {
                r.spawn((ImageNode::new(b.clone()), Node { position_type: PositionType::Absolute, left: px(0), top: percent(9), width: percent(70), height: px(80), ..default() }));
            }
            r.spawn((
                Text::new("PAUSED"),
                TextFont { font: title_font.clone(), font_size: bevy::text::FontSize::Px(54.0), ..default() },
                TextColor(white),
                Node { position_type: PositionType::Absolute, left: percent(9), top: percent(10), ..default() },
            ));
        } else if let Some(l) = &logo {
            r.spawn((ImageNode::new(l.clone()), Node { position_type: PositionType::Absolute, left: percent(6), top: percent(8), width: percent(44), aspect_ratio: Some(1912.0 / 304.0), ..default() }));
        }
        let long = menu.items.len() > 6;
        // the key bindings: compact
        let very_long = menu.items.len() > 14;
        let (left, top) = if menu.page == Page::Title {
            (percent(40), percent(78))
        } else if very_long {
            (percent(9), percent(19))
        } else if over_game || long {
            (percent(9), percent(24))
        } else {
            (percent(9), percent(36))
        };
        let size = if very_long { 22.0 } else if long { 26.0 } else { 34.0 };
        r.spawn(Node { position_type: PositionType::Absolute, left, top, flex_direction: FlexDirection::Column, row_gap: px(if long { 0.0 } else { 4.0 }), ..default() }).with_children(|col| {
            let heading = match menu.page {
                Page::Difficulty => Some("Select Difficulty"),
                Page::Options => Some("Options"),
                Page::Controls => Some("Controls"),
                Page::Load => Some("Load Game"),
                Page::Save => Some("Save Game"),
                _ => None,
            };
            if let Some(h) = heading {
                col.spawn((Text::new(h), TextFont { font: title_font.clone(), font_size: bevy::text::FontSize::Px(30.0), ..default() }, TextColor(gold), Node { margin: UiRect::bottom(px(14)), ..default() }));
            }
            for (i, (label, _)) in menu.items.iter().enumerate() {
                let selected = i == menu.sel && menu.page != Page::Title;
                let mut item = col.spawn((Button, MenuItem(i), Node { padding: UiRect::axes(px(26), px(if very_long { 3.0 } else if long { 5.0 } else { 8.0 })), min_width: px(420), ..default() }));
                item.with_children(|b| {
                    if selected {
                        if let Some(s) = &sel_bar {
                            b.spawn((ImageNode::new(s.clone()).with_color(Color::srgba(0.0, 0.0, 0.0, 0.85)), Node { position_type: PositionType::Absolute, left: px(-30), top: px(-6), width: percent(115), height: percent(125), ..default() }));
                        }
                    }
                    b.spawn((
                        Text::new(label.clone()),
                        TextFont { font: title_font.clone(), font_size: bevy::text::FontSize::Px(if menu.page == Page::Title { 30.0 } else { size }), ..default() },
                        TextColor(if selected { white } else { gold.with_alpha(0.85) }),
                        TextShadow::default(),
                    ));
                });
            }
        });
        if kind == MenuKind::Main && menu.page == Page::Main {
            r.spawn((
                Text::new(format!("Difficulty: {}", settings.difficulty_name())),
                TextFont { font_size: bevy::text::FontSize::Px(20.0), ..default() },
                TextColor(gold.with_alpha(0.7)),
                Node { position_type: PositionType::Absolute, left: percent(9), bottom: percent(6), ..default() },
            ));
        }
    });
}

/// Whether the HUD is put away (a menu or screen over the game, a cinematic, the kill cam): the
/// HUD's own pieces that set their visibility each frame follow it.
#[derive(Resource, Default)]
pub struct HudHidden(pub bool);

/// While a menu is open: the player can't act, the HUD hides, the cursor is free.
#[allow(clippy::type_complexity, clippy::too_many_arguments)]
fn menu_world(
    mut menu: ResMut<Menu>,
    mut hidden: ResMut<HudHidden>,
    mut player: Query<&mut Player>,
    mut hud: Query<&mut Visibility, Or<(With<HudRoot>, With<crate::gauges::GaugesRoot>, With<crate::kismet::SubtitleText>, With<crate::kismet::ObjectivesText>)>>,
    mut view_models: Query<
        &mut Visibility,
        (Or<(With<crate::arms::ArmsRoot>, With<crate::combat::ViewModel>)>, Without<HudRoot>, Without<crate::gauges::GaugesRoot>, Without<crate::kismet::SubtitleText>, Without<crate::kismet::ObjectivesText>),
    >,
    mut cursor: Single<&mut CursorOptions>,
    scripted: Option<Res<crate::script::Scripted>>,
    cine: Res<crate::script_world::Cinematic>,
    (journal, store, stats): (Res<crate::journal::Journal>, Res<crate::store::Store>, Query<(), With<crate::mission::StatsScreen>>),
    (note, killcam): (Res<crate::notescreen::NoteScreen>, Res<crate::killcam::KillCam>),
) {
    let open = menu.open.is_some();
    // (the screens over the game cover the HUD and its subtitles)
    let covered = open || journal.open || store.open.is_some() || !stats.is_empty() || note.open.is_some();
    let main = menu.open == Some(MenuKind::Main);
    if main {
        if let Ok(mut p) = player.single_mut() {
            p.locked = true;
        }
    } else if menu.started && !open {
        menu.started = false;
    }
    if open {
        menu.started = true;
    }
    // (the HUD also hides in the scripts' cinematics)
    hidden.0 = covered || cine.hides_hud() || killcam.active();
    for mut v in &mut hud {
        let want = if hidden.0 { Visibility::Hidden } else { Visibility::Inherited };
        if *v != want {
            *v = want;
        }
    }
    if main {
        for mut v in &mut view_models {
            *v = Visibility::Hidden;
        }
    }
    if open && scripted.is_none() && cursor.grab_mode != CursorGrabMode::None {
        cursor.visible = true;
        cursor.grab_mode = CursorGrabMode::None;
    }
}
