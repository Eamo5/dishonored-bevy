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
/// Dunwall City Trials' own menu map (`DisDLC05GameInfo`'s front end)
pub const TRIALS_MAP: &str = "L_DLC05_MainMenu_P";
/// The first mission (the prologue at the Tower).
pub const FIRST_MAP: &str = "L_Tower_P";
/// A story pack: its first map (`missions`: The Knife of Dunwall's mission 11, The Brigmore
/// Witches' 16), its name (`t_DLC06_Name`), its own front end's map and movie (the map its whole
/// game package opens on, `DisDLC06GameInfo.m_DLC06MainMenuCommand`; `UI_MainMenu_DLC06`) and the
/// movie's logo, its maps' prefix, its whole game package (what its end starts) and its credits
/// (its menu tweak's `m_TrailerName`).
pub struct DlcPack {
    pub first: &'static str,
    pub name: &'static str,
    pub menu: &'static str,
    pub movie: &'static str,
    pub logo: u16,
    pub prefix: &'static str,
    pub full: &'static str,
    pub credits: &'static str,
}

pub const DLC_PACKS: [DlcPack; 2] = [
    DlcPack { first: "DLC06_Tower_P", name: "t_DLC06_Name", menu: "DLC06_Dishonored_MainMenu", movie: "MainMenu_DLC06", logo: 79, prefix: "dlc06_", full: "dlc06_dishonoredgamefull_p", credits: "CreditsDLC06" },
    DlcPack { first: "L_DLC07_BaseIntro_P", name: "t_DLC07_Name", menu: "L_DLC07_MainMenu", movie: "MainMenu_DLC07", logo: 94, prefix: "l_dlc07_", full: "l_dlc07_gamefull_p", credits: "CreditsDLC07" },
];

/// The story pack a map is of (its own front end's too).
pub fn dlc_of(map: &str) -> Option<usize> {
    let m = map.to_ascii_lowercase();
    DLC_PACKS.iter().position(|p| m.starts_with(p.prefix))
}

/// A story pack's front end, if this map is one.
pub fn dlc_menu_of(map: &str) -> Option<usize> {
    DLC_PACKS.iter().position(|p| map.eq_ignore_ascii_case(p.menu))
}

/// A story pack's words with its names put in (`§DLC06_NAME§`, `§DLC07_NAME§`: the DLC's texts
/// speak of the pack they are shown in as `DLC06`).
fn pack_words(data: &crate::gamedata::Data, pack: usize, s: String) -> String {
    let name = data.text("DisGFxMoviePlayerBase_Texts", DLC_PACKS[pack].name);
    s.replace("\u{a7}DLC06_NAME\u{a7}", &name).replace("\u{a7}DLC07_NAME\u{a7}", &data.text("DisGFxMoviePlayerBase_Texts", DLC_PACKS[1].name))
}

/// The newest save of a campaign (none: the game's own; else a story pack's).
fn latest_of(slots: &SaveSlots, pack: Option<usize>) -> Option<usize> {
    slots.list().into_iter().map(|(i, _)| i).find(|&i| slots.info(i).is_some_and(|(map, ..)| dlc_of(map) == pack))
}

/// Whether a map is cooked (its scene in the cache).
fn map_cooked(map: &str) -> bool {
    crate::loading::cache_dir().join("maps").join(format!("{}.json", map.to_ascii_lowercase())).exists()
}

pub struct MenuPlugin;

impl Plugin for MenuPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Menu>().init_resource::<HudHidden>()
            .add_systems(OnEnter(GameState::InGame), enter_level.after(LevelSpawnSet))
            .add_systems(Update, (open_pause, menu_input, menu_build, menu_world, open_sounds, dlc_scene).chain().in_set(MenuSet).run_if(in_state(GameState::InGame)));
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
    /// the downloadable content (`t_DownloadableContent_Caps`); Dunwall City Trials' home
    /// (`CM_Home_Screen`) and its challenges, normal and expert (`CM_ChallengesList_Screen`'s
    /// tabs)
    Dlc,
    Trials,
    Challenges,
    ExpertChallenges,
    /// the trials' gallery (`CM_Gallery_Screen`)
    Gallery,
    /// the trials' welcome, shown the first time (`CM_WelcomeDisclaimer_Screen`)
    Welcome,
    /// the trials' leaderboards, normal and expert (`Leaderboards_Screen`'s tabs): local
    Leaderboards,
    ExpertLeaderboards,
    /// the main menu's missions (`MissionsScreen`: `missionscreen`)
    Missions,
    /// the pause menu's help (`t_Help`: the tutorial notes received, `helpscreen`)
    Help,
    /// The Brigmore Witches' new game, a Knife of Dunwall end found: which to take up
    /// (`MM_TransitionFromDLC06_Screen`)
    Transition,
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
    /// the link to the video settings (fullscreen, vertical sync, resolution: `VideoSettings`)
    VideoSettings,
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
    ModelDetails,
    LightShafts,
    RatShadows,
    Speakers,
    /// the pad's: its control scheme (a link to its screen, `GamepadMapping`), its rumble, its
    /// look's sensitivities across and up, its look up inverted
    PadScheme,
    PadVibration,
    PadLookX,
    PadLookY,
    PadInvertY,
    /// the aim assist and the aim's friction, with their strengths: the mouse's, the pad's
    MouseAutoAim,
    MouseAutoAimStrength,
    MouseFriction,
    MouseFrictionStrength,
    PadAutoAim,
    PadAutoAimStrength,
    PadFriction,
    PadFrictionStrength,
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Action {
    Continue,

    Difficulty(u8),
    Page(Page),
    Back,
    /// a tutorial note of the help's (its place in `Menu::tutorials`) read
    ReadTutorial(usize),
    Adjust(Opt),
    /// bind an action to the next key pressed
    Rebind(crate::bindings::Act),
    ResetBindings,
    Load(usize),
    Save(usize),
    /// a save deleted (asked first: `t_Q_DeleteSave`)
    DeleteSave(usize),
    Resume,
    QuitToMenu,
    QuitGame,
    /// a challenge (`DisDLC05GameInfo.m_Challenges`), in expert mode or not; one locked
    StartChallenge(usize, bool),
    LockedChallenge(usize),
    /// a piece of the trials' gallery, to see large; one locked
    GalleryView(usize),
    LockedGallery(usize),
    /// a mission's start played again (its save, `OnMissionSelected`: not asked); one locked
    Mission(usize),
    LockedMission(usize),
    /// into Dunwall City Trials (its menu map), and out
    Trials,
    LeaveTrials,
    /// a story pack's campaign begun anew (its first map), once a difficulty is chosen
    NewCampaign(&'static str),
    /// into a story pack's own front end (its menu map), and out to the game's; its credits
    DlcMenu(usize),
    LeaveDlc,
    DlcCredits(usize),
    /// a story pack's last difficulty, not yet open (`t_LastDifficultyUnlockCondition`)
    LockedDifficulty,
    /// The Knife of Dunwall's end of that chaos taken up, or none (a new game)
    UseTransition(Option<bool>),
    /// the trials' welcome read (`APressed`: `OnWelcomeDisclaimerClosed`), their credits
    /// (`DLC05_Credits`: `CreditsDLC05`)
    WelcomeDone,
    TrialsCredits,
    /// a challenge's leaderboard in a mode
    Board(usize, bool),
    /// a challenge's pause menu (`UI_PauseMenu_DLC05`): again, over now, back to the
    /// challenges
    RestartChallenge,
    EndChallenge,
    ExitChallenge,
}

/// The trials' gallery's grid's columns (`FillGalleryList`: `InitList(list, 7, 3)`).
pub const GALLERY_COLS: usize = 7;

#[derive(Resource)]
pub struct Menu {
    pub open: Option<MenuKind>,
    page: Page,
    stack: Vec<Page>,
    sel: usize,
    dirty: bool,
    /// the options' brightness screen open (`GammaSetting`): the brightness it opened with
    pub gamma: Option<f32>,
    /// the options' controls scheme screen open (`GamepadMapping`): the scheme it opened with
    pub pad_map: Option<u8>,
    /// the options' video settings open (`VideoSettings`), and a part of it clicked
    pub video: Option<VideoEdit>,
    pub video_click: Option<VideoHit>,
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
    /// the challenge to choose once the list is built (a tab changed)
    pending_challenge: Option<usize>,
    /// the gallery's piece seen large (`CM_Gallery_ViewPort_Screen`), by item
    pub gallery_view: Option<usize>,
    /// a leaderboard's row chosen (else its last run's)
    pub board_row: Option<usize>,
    /// a leaderboard opened over a run's results, closed back to them
    board_over_results: bool,
    /// the tutorial notes the help lists (`EDisTutorialNote` names)
    tutorials: Vec<String>,
    /// the campaign a difficulty is being chosen for (a story pack's first map; none: the
    /// game's own)
    campaign_map: Option<&'static str>,
    /// the story pack whose front end or game this is (`DLC_PACKS`; none: the game's own)
    dlc: Option<usize>,
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
    /// a link to a screen of its own (`P_Link`): its words (the brightness's `t_Modify_Gamma`),
    /// and the value it shows while not chosen (the controls scheme's; else none)
    Link(String, String),
}

/// A menu sound as the movies ask for it (`SoundHandler.PlaySound`: the `Default` theme,
/// `UISoundTheme_Common` and the movie's own, to `Snd_UI_Ingame`'s events: `Move`, `Stop`,
/// `Change`, `validation`, `back`, `Failure`, `Confirmation`, `Reset`, `ChangeMainTab`,
/// `ChangeSubTab`, `OpenScreen`, `CloseScreen`, the main menu's `M_PressStart` /
/// `M_OpenMainMenu`, the pause menu's `GO_OpenGameOverMenu`).
fn ui(sfx: &mut MessageWriter<crate::audio::PostEvent>, event: &str) {
    sfx.write(crate::audio::PostEvent::named(event, None));
}

/// The display's settings on the video settings' lines (`FillVideoSettings`: Fullscreen,
/// Vertical sync, Resolution; no resolution: the desktop's).
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct VideoValues {
    pub fullscreen: bool,
    pub vsync: bool,
    pub resolution: Option<[u32; 2]>,
}

impl VideoValues {
    fn of(s: &Settings) -> Self {
        VideoValues { fullscreen: s.fullscreen, vsync: s.vsync, resolution: s.resolution }
    }
    /// Put on the display (`OnApplyVideoSettings`; the settings left alone if they are these).
    fn apply(&self, s: &mut Settings) {
        if VideoValues::of(s) != *self {
            (s.fullscreen, s.vsync, s.resolution) = (self.fullscreen, self.vsync, self.resolution);
        }
    }
}

/// The options' video settings, open (`VideoSettings`, the Graphics' link): its lines, changed
/// on it alone until APPLY puts them on the display and asks whether they are right, a
/// countdown putting them back unless they are kept (`t_Q_VideoSettings`, `AddMessageBoxTimer`).
#[derive(Clone, Debug)]
pub struct VideoEdit {
    /// the settings as it opened (`_initSettingsList`)
    pub init: VideoValues,
    /// as its lines show them
    pub lines: VideoValues,
    /// the line chosen
    pub sel: usize,
    /// a line changed (`_bHasModifiedSettings`: set again as the settings are put back)
    pub modified: bool,
    /// the resolution's drop list open (`DropDownMenu_list`): the one chosen in it, and the
    /// first of its lines shown
    pub drop: Option<(usize, usize)>,
    /// asked after APPLY: the question, the seconds since, the button chosen (0: yes)
    pub asked: Option<(String, f32, usize)>,
}

/// The video settings' lines, as `FillVideoSettings` lists them.
pub const VIDEO_LINES: [&str; 3] = ["PSI_GraphicsPC_bFullScreen", "PSI_GraphicsPC_bVSync", "PSI_GraphicsPC_Resolution"];
/// The question's countdown (`AddMessageBoxTimer`: `SetMessageBoxTimer(10)`), and the lines the
/// resolution's drop list shows at once (`CreateDropList`: `_nbListLines`).
pub const VIDEO_COUNTDOWN: f32 = 10.0;
pub const DROP_LINES: usize = 7;

/// A part of the video settings clicked (`onRelease`).
#[derive(Component, Clone, Copy, PartialEq, Debug)]
pub enum VideoHit {
    /// APPLY, CANCEL (`accept_mc`, `cancel_mc`)
    Apply,
    Cancel,
    /// a line's choice (`bkgd0_mc`, `bkgd1_mc`: off, on)
    Choice(usize, bool),
    /// the resolution's box (`_bkgd_mc`: `OpenDropList`), a resolution in its list
    Drop,
    DropItem(usize),
}

impl VideoEdit {
    /// Its lines' names and values: on or off, the resolution among those offered.
    pub fn views(&self, data: &crate::gamedata::Data) -> Vec<(String, OptView)> {
        let list = crate::settings::resolutions();
        let at = resolution_at(self.lines.resolution, &list);
        let values = [OptView::Toggle(self.lines.fullscreen), OptView::Toggle(self.lines.vsync), OptView::Stepper(list.iter().map(|r| format!("{}x{}", r[0], r[1])).collect(), at)];
        VIDEO_LINES.iter().zip(values).map(|(k, v)| (data.text("Settings.ProfileSettingIDs", k), v)).collect()
    }
}

/// The options' categories (`m_SettingsCategory_*`) and their sub-categories (none: an empty
/// list).
pub const OPT_CATEGORIES: [(&str, &[&str]); 4] = [
    ("m_SettingsCategory_General", &["m_SettingsSubCategory_GameplaySettings", "m_SettingsSubCategory_HUDSettings"]),
    ("m_SettingsCategory_Controls", &["m_SettingsSubCategory_KeyboardMapping", "m_SettingsSubCategory_MouseSettings", "m_SettingsSubCategory_GamepadSettings"]),
    ("m_SettingsCategory_Graphics", &[]),
    ("m_SettingsCategory_Audio", &[]),
];

impl Default for Menu {
    fn default() -> Self {
        Menu { open: None, page: Page::Main, stack: Vec::new(), sel: 0, dirty: true, gamma: None, pad_map: None, video: None, video_click: None, items: Vec::new(), started: false, capture: None, opt_cat: 0, opt_sub: 0, confirm: None, confirm_click: None, pending_challenge: None, gallery_view: None, board_row: None, board_over_results: false, tutorials: Vec::new(), campaign_map: None, dlc: None }
    }
}

impl Menu {
    /// The main menu's screens the original movie draws (`frontend`): 0 the title, 1 the
    /// front page, 2 the new game's difficulties; with their items' labels.
    pub fn front_page(&self) -> Option<(u8, Vec<String>)> {
        let labels = || self.items.iter().map(|i| i.0.clone()).collect();
        match (self.open, self.page) {
            (Some(MenuKind::Main), Page::Title) => Some((0, Vec::new())),
            // (a story pack's own home: `dlcmenu`)
            (Some(MenuKind::Main), Page::Main) if self.dlc_home() => Some((crate::dlcmenu::CODE, labels())),
            (Some(MenuKind::Main), Page::Main) => Some((1, labels())),
            (Some(MenuKind::Main), Page::Difficulty) if self.dlc_home() => Some((crate::dlcmenu::NEW_GAME, labels())),
            (Some(MenuKind::Main), Page::Difficulty) => Some((2, labels())),
            (Some(MenuKind::Pause), Page::Main) => Some((3, labels())),
            (Some(MenuKind::GameOver), Page::Main) => Some((4, labels())),
            // (the save and load screens: `savescreen`)
            (Some(_), Page::Load) => Some((5, labels())),
            (Some(_), Page::Save) => Some((6, labels())),
            // (the options: `optscreen`)
            (Some(_), Page::Options) => Some((7, labels())),
            // (Dunwall City Trials': `dlc05menu`)
            (Some(MenuKind::Main), Page::Trials) => Some((8, labels())),
            (Some(MenuKind::Main), Page::Challenges) => Some((9, labels())),
            (Some(MenuKind::Main), Page::ExpertChallenges) => Some((10, labels())),
            (Some(MenuKind::Main), Page::Gallery) => Some((11, labels())),
            (Some(MenuKind::Main), Page::Welcome) => Some((12, labels())),
            (Some(MenuKind::Main), Page::Leaderboards) => Some((13, labels())),
            (Some(MenuKind::Main), Page::ExpertLeaderboards) => Some((14, labels())),
            // (the help: `helpscreen`)
            (Some(_), Page::Help) => Some((crate::helpscreen::CODE, labels())),
            (Some(MenuKind::Main), Page::Missions) => Some((crate::missionscreen::CODE, labels())),
            _ => None,
        }
    }
    /// A challenge list's item: the challenge, in expert mode or not, locked or not.
    pub fn item_challenge(&self, i: usize) -> Option<(usize, bool, bool)> {
        match self.items.get(i)?.1 {
            Action::StartChallenge(c, expert) => Some((c, expert, false)),
            Action::LockedChallenge(c) => Some((c, true, true)),
            _ => None,
        }
    }
    /// A gallery's item: the piece, locked or not.
    pub fn item_gallery(&self, i: usize) -> Option<(usize, bool)> {
        match self.items.get(i)?.1 {
            Action::GalleryView(g) => Some((g, false)),
            Action::LockedGallery(g) => Some((g, true)),
            _ => None,
        }
    }

    /// The piece seen large steps to the next unlocked one along (`GetNextImageAvailable`,
    /// `GetPreviousImageAvailable`: round the list); whether it moved.
    pub fn gallery_step(&mut self, dir: i32) -> bool {
        let Some(at) = self.gallery_view else { return false };
        let n = self.items.len() as i32;
        if n == 0 {
            return false;
        }
        let mut i = at as i32;
        for _ in 0..n {
            i = (i + dir).rem_euclid(n);
            if self.item_gallery(i as usize).is_some_and(|g| !g.1) {
                break;
            }
        }
        if i as usize == at {
            return false;
        }
        self.gallery_view = Some(i as usize);
        self.sel = i as usize;
        self.dirty = true;
        true
    }

    /// Choose an item (the pointer over it).
    pub fn select(&mut self, i: usize) {
        if i < self.items.len() && self.sel != i {
            self.sel = i;
            self.dirty = true;
        }
    }
    /// The challenges' (or leaderboards') other tab (normal, expert), the same challenge
    /// chosen where it is.
    pub fn switch_trials_tab(&mut self, expert: bool) {
        let boards = matches!(self.page, Page::Leaderboards | Page::ExpertLeaderboards);
        let want = match (boards, expert) {
            (false, false) => Page::Challenges,
            (false, true) => Page::ExpertChallenges,
            (true, false) => Page::Leaderboards,
            (true, true) => Page::ExpertLeaderboards,
        };
        if !matches!(self.page, Page::Challenges | Page::ExpertChallenges | Page::Leaderboards | Page::ExpertLeaderboards) || self.page == want {
            return;
        }
        let chosen = self.item_trial(self.sel);
        self.page = want;
        self.pending_challenge = chosen;
        self.board_row = None;
        self.dirty = true;
    }
    /// A leaderboard's item: the challenge, in expert mode or not.
    pub fn item_board(&self, i: usize) -> Option<(usize, bool)> {
        match self.items.get(i)?.1 {
            Action::Board(c, expert) => Some((c, expert)),
            _ => None,
        }
    }
    /// The challenge of a challenges' or leaderboards' item.
    fn item_trial(&self, i: usize) -> Option<usize> {
        self.item_challenge(i).map(|c| c.0).or(self.item_board(i).map(|b| b.0))
    }
    /// A challenge's leaderboard over its run's results (`R_Next_Screen.OnLeaderboardsClicked`).
    pub fn open_board(&mut self, challenge: usize, expert: bool) {
        self.open = Some(MenuKind::Main);
        self.page = if expert { Page::ExpertLeaderboards } else { Page::Leaderboards };
        self.stack.clear();
        self.sel = 0;
        self.pending_challenge = Some(challenge);
        self.board_row = None;
        self.board_over_results = true;
        self.dirty = true;
    }
    /// A leaderboard open over a run's results (drawn over them).
    pub fn over_results(&self) -> bool {
        self.board_over_results && self.open.is_some()
    }
    /// A leaderboard's other challenge along (`LeftPressed`, `RightPressed`: round the list).
    pub fn board_step(&mut self, dir: i32) {
        let n = self.items.len() as i32;
        if n > 0 && matches!(self.page, Page::Leaderboards | Page::ExpertLeaderboards) {
            self.sel = (self.sel as i32 + dir).rem_euclid(n) as usize;
            self.board_row = None;
            self.dirty = true;
        }
    }
    /// The item chosen with the keys.
    /// The story pack whose front end or game this is.
    pub fn dlc(&self) -> Option<usize> {
        self.dlc
    }

    /// The choice moved along by y (a story pack's new game's arrows), kept within the list.
    pub fn step_sel(&mut self, by: i32) {
        let n = self.items.len() as i32;
        let to = (self.sel as i32 + by).clamp(0, (n - 1).max(0)) as usize;
        if to != self.sel {
            self.sel = to;
            self.dirty = true;
        }
    }

    /// An item not yet open (a story pack's last difficulty).
    pub fn item_locked(&self, i: usize) -> bool {
        matches!(self.items.get(i), Some((_, Action::LockedDifficulty)))
    }

    /// In a story pack's own front end.
    pub fn dlc_home(&self) -> bool {
        self.dlc.is_some() && self.open == Some(MenuKind::Main)
    }

    pub fn selected(&self) -> usize {
        self.sel
    }
    /// The save slot an item of the save or load screen stands for.
    pub fn item_slot(&self, i: usize) -> Option<usize> {
        match self.items.get(i)?.1 {
            Action::Load(s) | Action::Save(s) | Action::Mission(s) => Some(s),
            _ => None,
        }
    }
    /// The main menu's (not over a game).
    pub fn in_main(&self) -> bool {
        self.open == Some(MenuKind::Main)
    }
    /// The question asked (its words, the button chosen), if any (`msgbox`).
    pub fn question(&self) -> Option<(&str, usize)> {
        self.confirm.as_ref().map(|c| (c.0.as_str(), c.2)).or_else(|| self.video.as_ref()?.asked.as_ref().map(|a| (a.0.as_str(), a.2)))
    }
    /// The seconds the question's countdown shows (`Countdown`: whole seconds, down to 0).
    pub fn countdown(&self) -> Option<u32> {
        let t = self.video.as_ref()?.asked.as_ref()?.1;
        Some((VIDEO_COUNTDOWN - t.floor()).max(0.0) as u32)
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
                // (its own screen: `GammaMenu`'s link; the video settings', `VideoSettings`)
                Opt::Brightness => OptView::Link(data.text("DisGFxMoviePlayerMenuBase_Texts", "t_Modify_Gamma"), String::new()),
                Opt::VideoSettings => OptView::Link(data.text("DisGFxMoviePlayerMenuBase_Texts", "t_Modify_VideoSettings"), String::new()),
                // (the controls scheme's link shows its scheme: `Mapping_Names[Setting_Value]`)
                Opt::PadScheme => OptView::Link(data.text("DisGFxMoviePlayerMenuBase_Texts", "t_Modify_ControlsScheme"), pad_scheme_name(data, s.pad_binding_set)),
                Opt::PadLookX => slider(s.pad_look_x, 0.0, 100.0, format!("{:.0}", s.pad_look_x)),
                Opt::PadLookY => slider(s.pad_look_y, 0.0, 100.0, format!("{:.0}", s.pad_look_y)),
                Opt::PadInvertY => OptView::Toggle(s.pad_invert_y),
                Opt::PadVibration => OptView::Toggle(s.pad_vibration),
                Opt::MouseAutoAim => OptView::Toggle(s.mouse_auto_aim),
                Opt::MouseFriction => OptView::Toggle(s.mouse_friction),
                Opt::PadAutoAim => OptView::Toggle(s.pad_auto_aim),
                Opt::PadFriction => OptView::Toggle(s.pad_friction),
                Opt::MouseAutoAimStrength => slider(s.mouse_auto_aim_strength, 0.0, 100.0, format!("{:.0}", s.mouse_auto_aim_strength)),
                Opt::MouseFrictionStrength => slider(s.mouse_friction_strength, 0.0, 100.0, format!("{:.0}", s.mouse_friction_strength)),
                Opt::PadAutoAimStrength => slider(s.pad_auto_aim_strength, 0.0, 100.0, format!("{:.0}", s.pad_auto_aim_strength)),
                Opt::PadFrictionStrength => slider(s.pad_friction_strength, 0.0, 100.0, format!("{:.0}", s.pad_friction_strength)),
                Opt::Difficulty => OptView::Stepper(words(&["EDifficulty_Easy", "EDifficulty_Normal", "EDifficulty_Hard", "EDifficulty_VeryHard"]), s.difficulty as usize),
                Opt::Crosshair => OptView::Stepper(words(&["CS_Off", "CS_Simple", "CS_Normal"]), s.crosshair_style.min(2) as usize),
                Opt::HudGauges => OptView::Stepper(words(&["HV_Off", "HV_Contextual", "HV_Always"]), s.hud_gauges.min(2) as usize),
                Opt::ModelDetails => OptView::Stepper(words(&["ModelDetails_Normal", "ModelDetails_High"]), s.model_details.min(1) as usize),
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
                Opt::Markers => OptView::Toggle(s.markers),
                Opt::Sensitivity => slider(s.sensitivity, 0.5, 6.0, format!("{:.0}", (s.sensitivity - 0.5) / 5.5 * 100.0)),
                Opt::Fov => slider(s.fov, 65.0, 110.0, format!("{:.0}", s.fov)),
                Opt::Master => slider(s.master_volume, 0.0, 1.0, format!("{:.0}", s.master_volume * 100.0)),
                Opt::Music => slider(s.music_volume, 0.0, 1.0, format!("{:.0}", s.music_volume * 100.0)),
                Opt::Sfx => slider(s.sfx_volume, 0.0, 1.0, format!("{:.0}", s.sfx_volume * 100.0)),
                Opt::Voice => slider(s.voice_volume, 0.0, 1.0, format!("{:.0}", s.voice_volume * 100.0)),
            },
            Action::Rebind(a) => {
                let bind = crate::bindings::Bindings::from_settings(s);
                OptView::Key(crate::bindings::display_bind(bind.bind(a)), self.capture == Some(a))
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
            (1, 1) => {
                s.sensitivity = d.sensitivity;
                s.invert_y = d.invert_y;
                s.mouse_smoothing = d.mouse_smoothing;
                s.mouse_auto_aim = d.mouse_auto_aim;
                s.mouse_auto_aim_strength = d.mouse_auto_aim_strength;
                s.mouse_friction = d.mouse_friction;
                s.mouse_friction_strength = d.mouse_friction_strength;
            }
            (1, _) => {
                s.pad_binding_set = d.pad_binding_set;
                s.pad_look_x = d.pad_look_x;
                s.pad_look_y = d.pad_look_y;
                s.pad_invert_y = d.pad_invert_y;
                s.pad_vibration = d.pad_vibration;
                s.pad_auto_aim = d.pad_auto_aim;
                s.pad_auto_aim_strength = d.pad_auto_aim_strength;
                s.pad_friction = d.pad_friction;
                s.pad_friction_strength = d.pad_friction_strength;
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
        self.board_row = None;
        // (the new game opens on Normal: `NewGameMenu._curSelectionIdx`)
        self.sel = if page == Page::Difficulty { 1 } else { 0 };
        self.dirty = true;
    }
    fn back(&mut self) -> bool {
        self.gallery_view = None;
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

/// A story pack's front end's scene follows its menu, as its movie tells its map (`fscommand`,
/// the map's `GFxEvent_FSCommand`s): into the new game's screen (`ToNewGameScreen<n>`, by the
/// difficulty chosen), each difficulty as it is chosen (`SetDifficultyCameraAngle<n>`: its camera,
/// Daud's figure for it, its card), back out (`FromNewGameScreen<n>`), the saves' screen in and out
/// (`ToLoadScreen`, `FromLoadScreen`).
fn dlc_scene(menu: Res<Menu>, vm: Option<ResMut<Vm>>, mut was: Local<Option<(Page, usize)>>) {
    let now = menu.dlc_home().then_some((menu.page, menu.sel));
    if now == *was {
        return;
    }
    let before = std::mem::replace(&mut *was, now);
    let (Some(mut vm), Some((page, sel))) = (vm, now) else { return };
    let sel = sel.min(3);
    match (before.map(|b| b.0), page) {
        (Some(Page::Difficulty), Page::Difficulty) => vm.fs_command(&format!("SetDifficultyCameraAngle{sel}")),
        (_, Page::Difficulty) => vm.fs_command(&format!("ToNewGameScreen{sel}")),
        (Some(Page::Difficulty), _) => vm.fs_command(&format!("FromNewGameScreen{}", before.map_or(1, |b| b.1.min(3)))),
        (Some(Page::Load), p) if p != Page::Load => vm.fs_command("FromLoadScreen"),
        (b, Page::Load) if b != Some(Page::Load) => vm.fs_command("ToLoadScreen"),
        _ => {}
    }
}

#[derive(Component)]
struct MenuRoot;

#[derive(Component)]
pub struct MenuItem(pub usize);

fn enter_level(level: Option<Res<LevelInfo>>, mut menu: ResMut<Menu>, vm: Option<ResMut<Vm>>, mut launch: ResMut<crate::challenge::ChallengeLaunch>, profile: Res<crate::challenge::ChallengeProfile>) {
    let main = level.as_ref().is_some_and(|l| l.scene.name.eq_ignore_ascii_case(MENU_MAP));
    let trials = level.as_ref().is_some_and(|l| l.scene.name.eq_ignore_ascii_case(TRIALS_MAP));
    let name = level.as_ref().map(|l| l.scene.name.clone()).unwrap_or_default();
    *menu = Menu::default();
    menu.dlc = dlc_of(&name);
    // a story pack's own front end (its movie opening tells its map, `OpenMainMenuScreen`: its
    // camera brought in)
    if let Some(i) = dlc_menu_of(&name) {
        menu.open = Some(MenuKind::Main);
        menu.page = Page::Main;
        menu.started = true;
        menu.campaign_map = Some(DLC_PACKS[i].first);
        if let Some(mut vm) = vm {
            vm.fs_command("OpenMainMenuScreen");
        }
        return;
    }
    if trials {
        // Dunwall City Trials' home (or, back from a challenge's results, its challenges)
        menu.open = Some(MenuKind::Main);
        menu.page = Page::Trials;
        menu.started = true;
        if std::mem::take(&mut launch.back_to_challenges) {
            menu.page = if launch.expert { Page::ExpertChallenges } else { Page::Challenges };
            menu.stack = vec![Page::Trials];
            menu.pending_challenge = launch.last;
        } else if !profile.welcome_seen {
            // (the first time: the welcome, then the home)
            menu.page = Page::Welcome;
        }
    }
    if main {
        menu.open = Some(MenuKind::Main);
        menu.page = Page::Title;
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
    // (a challenge over: its results take the keys)
    if menu.open.is_none() && keys.just_pressed(KeyCode::Escape) && !stats.dead && challenge.ended.is_none() {
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
        Opt::VideoSettings => String::new(),
        Opt::Crosshair => ["Off", "Simple", "Normal"][s.crosshair_style.min(2) as usize].into(),
        Opt::HudGauges => ["Off", "Contextual", "Always"][s.hud_gauges.min(2) as usize].into(),
        Opt::ModelDetails => ["Normal", "High"][s.model_details.min(1) as usize].into(),
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
        Opt::PadScheme => format!("{}", s.pad_binding_set),
        Opt::PadLookX => format!("{:.0}", s.pad_look_x),
        Opt::PadLookY => format!("{:.0}", s.pad_look_y),
        Opt::PadInvertY => on(s.pad_invert_y),
        Opt::PadVibration => on(s.pad_vibration),
        Opt::MouseAutoAim => on(s.mouse_auto_aim),
        Opt::MouseFriction => on(s.mouse_friction),
        Opt::PadAutoAim => on(s.pad_auto_aim),
        Opt::PadFriction => on(s.pad_friction),
        Opt::MouseAutoAimStrength => format!("{:.0}", s.mouse_auto_aim_strength),
        Opt::MouseFrictionStrength => format!("{:.0}", s.mouse_friction_strength),
        Opt::PadAutoAimStrength => format!("{:.0}", s.pad_auto_aim_strength),
        Opt::PadFrictionStrength => format!("{:.0}", s.pad_friction_strength),
    }
}

/// A control scheme's name (`PBS_1`..`4`: Type A to D).
pub fn pad_scheme_name(data: &crate::gamedata::Data, set: u8) -> String {
    data.text("Settings.ProfileSettingValues", &format!("PBS_{}", set.min(3) + 1))
}

/// The resolution chosen among those offered (the desktop's: the largest).
fn resolution_at(r: Option<[u32; 2]>, list: &[[u32; 2]]) -> usize {
    r.and_then(|r| list.iter().position(|x| *x == r)).unwrap_or(list.len().saturating_sub(1))
}

fn on(b: bool) -> String {
    if b { "On" } else { "Off" }.into()
}

/// A setting stepped: Left / Right by `dir`, no further than its ends (`P_OptionsStepper`,
/// `P_OptionsSlider`: `isLoop` false; a toggle's Left is off, its Right on); clicked or Entered
/// (`flip`), a toggle flips and a slider steps round.
fn adjust(s: &mut Settings, o: Opt, dir: f32, flip: bool) {
    let step = |v: &mut f32, d: f32, lo: f32, hi: f32| {
        *v = ((*v + d * dir) * 100.0).round() / 100.0;
        if *v > hi + 1e-3 {
            *v = if flip { lo } else { hi };
        } else if *v < lo - 1e-3 {
            *v = if flip { hi } else { lo };
        }
    };
    let set = |b: &mut bool| *b = if flip { !*b } else { dir > 0.0 };
    match o {
        // (no wrapping round: `isLoop` false)
        Opt::Difficulty => s.difficulty = (s.difficulty as f32 + dir).clamp(0.0, 3.0) as u8,
        Opt::Sensitivity => step(&mut s.sensitivity, 0.25, 0.5, 6.0),
        Opt::InvertY => set(&mut s.invert_y),
        Opt::Fov => step(&mut s.fov, 5.0, 65.0, 110.0),
        Opt::Master => step(&mut s.master_volume, 0.1, 0.0, 1.0),
        Opt::Music => step(&mut s.music_volume, 0.1, 0.0, 1.0),
        Opt::Sfx => step(&mut s.sfx_volume, 0.1, 0.0, 1.0),
        Opt::Voice => step(&mut s.voice_volume, 0.1, 0.0, 1.0),
        Opt::Subtitles => s.subtitle_mode = (s.subtitle_mode as f32 + dir).clamp(0.0, 2.0) as u8,
        Opt::Speakers => s.speaker_config = (s.speaker_config as f32 + dir).clamp(0.0, 2.0) as u8,
        Opt::VideoSettings => {}
        Opt::Crosshair => s.crosshair_style = (s.crosshair_style as f32 + dir).clamp(0.0, 2.0) as u8,
        Opt::HudGauges => s.hud_gauges = (s.hud_gauges as f32 + dir).clamp(0.0, 2.0) as u8,
        Opt::ModelDetails => s.model_details = (s.model_details as f32 + dir).clamp(0.0, 1.0) as u8,
        // (`PSI_HUD_CrosshairOpacity` 0-100 by 10, `PSI_Gameplay_HeadBobAmount` 0-1 by 0.1)
        Opt::CrosshairOpacity => s.crosshair_opacity = (s.crosshair_opacity + 10.0 * dir).clamp(0.0, 100.0),
        Opt::HeadBob => s.head_bob = ((s.head_bob + 0.1 * dir).clamp(0.0, 1.0) * 10.0).round() / 10.0,
        Opt::ObjPopups => set(&mut s.objective_popups),
        Opt::Tutorials => set(&mut s.tutorials),
        Opt::Interactions => set(&mut s.interactions),
        Opt::Highlight => set(&mut s.focus_highlight),
        Opt::PickupLog => set(&mut s.pickup_log),
        Opt::ContextIcons => set(&mut s.contextual_icons),
        Opt::Stance => set(&mut s.player_stance),
        Opt::GrenadeMarkers => set(&mut s.grenade_markers),
        Opt::AwarenessMarkers => set(&mut s.awareness_markers),
        Opt::HeartMarkers => set(&mut s.heart_markers),
        Opt::CrosshairMove => set(&mut s.crosshair_movement),
        Opt::AutoSave => set(&mut s.auto_save_journal),
        Opt::ClimbRelative => set(&mut s.camera_relative_climbing),
        Opt::LightShafts => set(&mut s.light_shafts),
        Opt::RatShadows => set(&mut s.rat_shadows),
        Opt::Markers => set(&mut s.markers),
        Opt::Brightness => step(&mut s.brightness, 0.05, 0.5, 1.6),
        Opt::TextureDetail => s.texture_detail = (s.texture_detail as f32 + dir).clamp(0.0, 2.0) as u8,
        Opt::AntiAliasing => s.anti_aliasing = (s.anti_aliasing as f32 + dir).clamp(0.0, 2.0) as u8,
        Opt::AutoMana => set(&mut s.auto_mana_elixir),
        Opt::KillCam => s.kill_cam = (s.kill_cam as f32 + dir).clamp(0.0, 2.0) as u8,
        Opt::Smoothing => set(&mut s.mouse_smoothing),
        // (the schemes stepped on their screen, no further than Type D; the look's
        // sensitivities 0-100 by 10, `PSI_Gamepad_LookXSensitivity`)
        Opt::PadScheme => s.pad_binding_set = (s.pad_binding_set as f32 + dir).clamp(0.0, 3.0) as u8,
        Opt::PadLookX => step(&mut s.pad_look_x, 10.0, 0.0, 100.0),
        Opt::PadLookY => step(&mut s.pad_look_y, 10.0, 0.0, 100.0),
        Opt::PadInvertY => set(&mut s.pad_invert_y),
        Opt::PadVibration => set(&mut s.pad_vibration),
        // (the aim's assists, their strengths 0-100 by 10)
        Opt::MouseAutoAim => set(&mut s.mouse_auto_aim),
        Opt::MouseFriction => set(&mut s.mouse_friction),
        Opt::PadAutoAim => set(&mut s.pad_auto_aim),
        Opt::PadFriction => set(&mut s.pad_friction),
        Opt::MouseAutoAimStrength => step(&mut s.mouse_auto_aim_strength, 10.0, 0.0, 100.0),
        Opt::MouseFrictionStrength => step(&mut s.mouse_friction_strength, 10.0, 0.0, 100.0),
        Opt::PadAutoAimStrength => step(&mut s.pad_auto_aim_strength, 10.0, 0.0, 100.0),
        Opt::PadFrictionStrength => step(&mut s.pad_friction_strength, 10.0, 0.0, 100.0),
    }
}

#[cfg(test)]
mod modal_tests {
    use super::*;

    /// The story packs' maps, front ends and texts are told apart.
    #[test]
    fn story_packs_are_known_by_their_maps() {
        assert_eq!(dlc_of("DLC06_Slaughter_Int_P"), Some(0));
        assert_eq!(dlc_of("DLC06_Dishonored_MainMenu"), Some(0));
        assert_eq!(dlc_of("L_DLC07_Coldridge_P"), Some(1));
        assert_eq!(dlc_of("L_DLC05_Arena_P"), None);
        assert_eq!(dlc_of("L_Tower_P"), None);
        assert_eq!(dlc_menu_of("l_dlc07_mainmenu"), Some(1));
        assert_eq!(dlc_menu_of("DLC06_Tower_P"), None);
        let mut data = crate::gamedata::Data::default();
        data.0.texts.insert("DisGFxMoviePlayerBase_Texts.t_DLC06_Name".into(), "The Knife of Dunwall".into());
        data.0.texts.insert("DisGFxMoviePlayerBase_Texts.t_DLC07_Name".into(), "The Brigmore Witches".into());
        assert_eq!(pack_words(&data, 0, "Exit \u{a7}DLC06_NAME\u{a7}".into()), "Exit The Knife of Dunwall");
        assert_eq!(pack_words(&data, 1, "Exit \u{a7}DLC06_NAME\u{a7}".into()), "Exit The Brigmore Witches");
    }

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

/// A challenge run, for its pause menu: whether it may be ended early.
pub struct TrialRun {
    pub can_end_early: bool,
}

fn items(menu: &Menu, settings: &Settings, slots: &SaveSlots, data: &crate::gamedata::Data, profile: &crate::challenge::ChallengeProfile, trial: Option<TrialRun>) -> Vec<(String, Action)> {
    // (the help: a tutorial note received a row, by its title)
    if menu.page == Page::Help {
        return menu
            .tutorials
            .iter()
            .enumerate()
            .filter_map(|(i, id)| crate::tutorials::note(data, id).map(|n| (crate::tutorials::title(data, &n.name), Action::ReadTutorial(i))))
            .collect();
    }
    // (the saves of the campaign at hand: the game's own, or the story pack's)
    let has_saves = latest_of(slots, menu.dlc).is_some();
    match (menu.open, menu.page) {
        (_, Page::Title) => vec![("Press any key".into(), Action::Page(Page::Main))],
        // a story pack's home (`MM_Home_Menu`): its campaign's, its credits (`t_Credits`), and back
        // to the game's (`t_BackToMainMenu`)
        (Some(MenuKind::Main), Page::Main) if menu.dlc.is_some() => {
            let pack = menu.dlc.unwrap_or(0);
            let base = "DisGFxMoviePlayerMainMenu_Texts";
            let own = "DisDLC06MoviePlayerMainMenu_Texts";
            let mut v = Vec::new();
            if has_saves {
                v.push(("Continue".into(), Action::Continue));
            }
            v.push((data.text(base, "t_NewGame"), Action::NewCampaign(DLC_PACKS[pack].first)));
            if has_saves {
                v.push(("Load".into(), Action::Page(Page::Load)));
            }
            v.push(("Options".into(), Action::Page(Page::Options)));
            v.push((data.text(own, "t_Credits"), Action::DlcCredits(pack)));
            v.push((data.text(own, "t_BackToMainMenu"), Action::LeaveDlc));
            v
        }
        (Some(MenuKind::Main), Page::Main) => {
            let mut v = Vec::new();
            // (the original's words: `t_ContinueGame`, `t_NewGame`, `t_LoadGame`, `t_Options`,
            // `t_QuitGame`)
            if has_saves {
                v.push(("Continue".into(), Action::Continue));
            }
            v.push(("New Game".into(), Action::Page(Page::Difficulty)));
            // (`t_Missions`: always there, as the original's bar has it)
            v.push(("Missions".into(), Action::Page(Page::Missions)));
            if has_saves {
                v.push(("Load".into(), Action::Page(Page::Load)));
            }
            // (`t_DownloadableContent_Caps`: the installed DLC)
            if !data.0.challenges.is_empty() || DLC_PACKS.iter().any(|p| map_cooked(p.first)) {
                v.push(("Downloadable Content".into(), Action::Page(Page::Dlc)));
            }
            v.push(("Options".into(), Action::Page(Page::Options)));
            v.push(("Quit Game".into(), Action::QuitGame));
            v
        }
        (_, Page::Dlc) => {
            // (the installed packs: the trials, Daud's two stories)
            let mut v = Vec::new();
            if !data.0.challenges.is_empty() {
                v.push((data.text("DisDLC05MoviePlayerChallengeMenu_Texts", "t_DLC05_Name"), Action::Trials));
            }
            // (each into its own front end, where cooked; else straight to its new game)
            for (i, p) in DLC_PACKS.iter().enumerate() {
                if map_cooked(p.first) {
                    let to = if map_cooked(p.menu) { Action::DlcMenu(i) } else { Action::NewCampaign(p.first) };
                    v.push((data.text("DisGFxMoviePlayerBase_Texts", p.name), to));
                }
            }
            v.push(("Back".into(), Action::Back));
            v
        }
        // (`CM_Home_Screen.SetHomeMenu`: the challenges, the way out; leaderboards, gallery and
        // credits aside)
        (_, Page::Trials) => {
            let mut v = vec![(data.text("DisGFxMoviePlayerBase_Texts", "t_ChallengesList"), Action::Page(Page::Challenges))];
            if !data.0.gallery.is_empty() {
                v.push((data.text("DisDLC05MoviePlayerChallengeMenu_Texts", "t_Gallery"), Action::Page(Page::Gallery)));
            }
            v.insert(1, (data.text("DisGFxMoviePlayerBase_Texts", "t_Leaderboards_X360"), Action::Page(Page::Leaderboards)));
            v.push((data.text("DisDLC05MoviePlayerChallengeMenu_Texts", "t_Credits"), Action::TrialsCredits));
            v.push((data.text("DisDLC05MoviePlayerChallengeMenu_Texts", "t_BackToMainMenu"), Action::LeaveTrials));
            v
        }
        (_, Page::Welcome) => vec![(data.text("DisGFxMoviePlayerBase_Texts", "t_ContinueGame"), Action::WelcomeDone)],
        // every challenge's board in normal mode; in expert those with one
        (_, Page::Leaderboards) => data.0.challenges.iter().enumerate().map(|(i, c)| (c.name.clone(), Action::Board(i, false))).collect(),
        (_, Page::ExpertLeaderboards) => data.0.challenges.iter().enumerate().filter(|(_, c)| c.expert_medals.iter().any(|m| *m > 0)).map(|(i, c)| (c.name.clone(), Action::Board(i, true))).collect(),
        // the gallery's pieces, each open once its stars are won (`DisDLC05GalleryItem`)
        (_, Page::Gallery) => data
            .0
            .gallery
            .iter()
            .enumerate()
            .map(|(i, g)| (g.id.clone(), if profile.gallery_unlocked(&data.0, i) { Action::GalleryView(i) } else { Action::LockedGallery(i) }))
            .collect(),
        // every challenge in normal mode; in expert those with one, open once two stars are
        // won in normal (`t_ChallengeUnlockCondition`, `t_Welcome_txt2`)
        (_, Page::Challenges) => data.0.challenges.iter().enumerate().map(|(i, c)| (c.name.clone(), Action::StartChallenge(i, false))).collect(),
        (_, Page::ExpertChallenges) => data
            .0
            .challenges
            .iter()
            .enumerate()
            .filter(|(_, c)| c.expert_medals.iter().any(|m| *m > 0))
            .map(|(i, c)| {
                let best = profile.best.get(&c.id).copied().unwrap_or(0);
                let open = c.medals[1] > 0 && best >= c.medals[1] as i64;
                (c.name.clone(), if open { Action::StartChallenge(i, true) } else { Action::LockedChallenge(i) })
            })
            .collect(),
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
        // (a challenge's: `t_ResumeGame`, `t_RestartChallenge`, `t_EndChallenge` where it may end
        // early, `t_Options`, `t_ExitChallenge`, `t_ExitDLC05`)
        (Some(MenuKind::Pause), Page::Main) if trial.is_some() => {
            let b = "DisGFxMoviePlayerBase_Texts";
            let mut v = vec![("Resume".into(), Action::Resume), (data.text(b, "t_RestartChallenge"), Action::RestartChallenge)];
            if trial.as_ref().is_some_and(|t| t.can_end_early) {
                v.push((data.text(b, "t_EndChallenge"), Action::EndChallenge));
            }
            v.push(("Options".into(), Action::Page(Page::Options)));
            v.push((data.text(b, "t_ExitChallenge"), Action::ExitChallenge));
            v.push((data.text(b, "t_ExitDLC05"), Action::LeaveTrials));
            v
        }
        (_, Page::Main) => {
            let base = "DisGFxMoviePlayerMenuBase_Texts";
            let mut v = vec![
                ("Resume".into(), Action::Resume),
                ("Save Game".into(), Action::Page(Page::Save)),
                ("Load Game".into(), Action::Page(Page::Load)),
                ("Options".into(), Action::Page(Page::Options)),
                (data.text(base, "t_Help"), Action::Page(Page::Help)),
            ];
            // (a story pack's: out of the mission to its own front end, `t_BackToDLC06MainMenu`, and
            // out of it, `t_ExitDLC06`)
            match menu.dlc {
                Some(pack) => {
                    v.push((data.text(base, "t_BackToDLC06MainMenu"), Action::QuitToMenu));
                    v.push((pack_words(data, pack, data.text(base, "t_ExitDLC06")), Action::LeaveDlc));
                }
                None => v.push(("Back to Main Menu".into(), Action::QuitToMenu)),
            }
            v.push(("Back to Windows".into(), Action::QuitGame));
            v
        }
        // (Back is the help bar's: `SecondaryScreen`)
        // (a story pack's own words, `t_difficulty<n>_DLC06`: Novice to Master Assassin, the last open once
        // its campaign is done)
        (_, Page::Difficulty) => match menu.dlc {
            Some(pack) => (0..4u8)
                .map(|i| {
                    let name = data.text("DisGFxMoviePlayerMainMenu_Texts", &format!("t_difficulty{i}_DLC06"));
                    (name, if i == 3 && !settings.dlc_master[pack] { Action::LockedDifficulty } else { Action::Difficulty(i) })
                })
                .collect(),
            None => ["Easy", "Normal", "Hard", "Very Hard"].iter().enumerate().map(|(i, n)| (n.to_string(), Action::Difficulty(i as u8))).collect(),
        },
        (_, Page::Options) => {
            // (a setting's name; the video settings' link's, the menus' tweak's)
            let label = |k: &str| if k.starts_with("m_") { data.text("Twk_GFxMoviePlayerMenuBase", k) } else { data.text("Settings.ProfileSettingIDs", k) };
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
                // (each sub-category's settings in the profile's order: `PSI_Mouse_Start` to
                // `_End`, `PSI_Gamepad_Start` to `_End`)
                (1, 1) => vec![
                    ("PSI_Mouse_Sensitivity", Opt::Sensitivity),
                    ("PSI_Mouse_bSmooting", Opt::Smoothing),
                    ("PSI_Mouse_bInvertY", Opt::InvertY),
                    ("PSI_Mouse_bAutoAim", Opt::MouseAutoAim),
                    ("PSI_Mouse_AutoAimStrength", Opt::MouseAutoAimStrength),
                    ("PSI_Mouse_bFriction", Opt::MouseFriction),
                    ("PSI_Mouse_FrictionStrength", Opt::MouseFrictionStrength),
                ],
                (1, _) => vec![
                    ("PSI_Gamepad_BindingSet", Opt::PadScheme),
                    ("PSI_Gamepad_bVibration", Opt::PadVibration),
                    ("PSI_Gamepad_LookXSensitivity", Opt::PadLookX),
                    ("PSI_Gamepad_LookYSensitivity", Opt::PadLookY),
                    ("PSI_Gamepad_bInvertY", Opt::PadInvertY),
                    ("PSI_Gamepad_bAutoAim", Opt::PadAutoAim),
                    ("PSI_Gamepad_AutoAimStrength", Opt::PadAutoAimStrength),
                    ("PSI_Gamepad_bFriction", Opt::PadFriction),
                    ("PSI_Gamepad_FrictionStrength", Opt::PadFrictionStrength),
                ],
                (2, _) => vec![
                    ("PSI_Graphics_Gamma", Opt::Brightness),
                    // (the display's resolution, fullscreen and vertical sync: on a screen of
                    // their own, `VideoSettings`)
                    ("m_SettingsSubMenu_VideoSettings", Opt::VideoSettings),
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
                    let key = if menu.capture == Some(*a) { "Press a key...".to_string() } else { crate::bindings::display_bind(bind.bind(*a)) };
                    (format!("{}   {key}", a.label()), Action::Rebind(*a))
                })
                .collect();
            v.push(("Reset to Defaults".into(), Action::ResetBindings));
            v.push(("Back".into(), Action::Back));
            v
        }
        // the saves, newest first (`savescreen` draws them; Escape goes back)
        // (the campaign's at hand: the game's own, or the story pack's)
        (_, Page::Load) => slots.list().into_iter().filter(|(i, _)| slots.info(*i).is_some_and(|(map, ..)| dlc_of(map) == menu.dlc)).map(|(i, desc)| (desc, Action::Load(i))).collect(),
        // the missions as the exe lists them (`OnMissionsClicked`): up to the furthest started,
        // each by its start's save (`SGS_DisMission0..8`), locked without it
        (_, Page::Missions) => {
            let has = |i: usize| slots.info(crate::save::MISSION_SLOT + i).is_some();
            match (0..crate::missionscreen::MISSIONS.len()).filter(|&i| has(i)).max() {
                Some(last) => (0..=last).map(|i| (crate::missionscreen::mission_name(data, i), if has(i) { Action::Mission(crate::save::MISSION_SLOT + i) } else { Action::LockedMission(i) })).collect(),
                None => Vec::new(),
            }
        }
        // a new save first (the first free slot), then the saves to overwrite
        (_, Page::Save) => {
            let mut v: Vec<(String, Action)> = Vec::new();
            if let Some(free) = crate::save::manual_slots().find(|i| slots.describe(*i).is_none()) {
                v.push(("Create a new save".into(), Action::Save(free)));
            }
            v.extend(slots.list().into_iter().filter(|(i, _)| crate::save::is_manual(*i)).map(|(i, desc)| (desc, Action::Save(i))));
            v
        }
        // (the help's rows: above)
        (_, Page::Help) => Vec::new(),
        // (`MM_TransitionFromDLC06_Menu`: each end found by its chaos, then none)
        (_, Page::Transition) => {
            let t = "DisGFxMoviePlayerMainMenu_Texts";
            let [low, high] = crate::save::transitions_found();
            let mut v = Vec::new();
            if low {
                v.push((data.text(t, "t_LowChaos"), Action::UseTransition(Some(false))));
            }
            if high {
                v.push((data.text(t, "t_HighChaos"), Action::UseTransition(Some(true))));
            }
            v.push((data.text("DisGFxMoviePlayerMenuBase_Texts", "t_noDLC06Save"), Action::UseTransition(None)));
            v
        }
    }
}

/// The video settings' keys (`VideoSettings`, `VideoSettingsList`, `P_DropDownMenu`): Up / Down
/// its lines, Left / Right a line's value (Off / On; the resolution's drop list opened, as by
/// Shift, `X`), Enter applies them (`APressed`: closed if none changed), Escape puts them back
/// as they were and closes it (`BPressed`: `CancelModifications`). The drop list open: Up / Down,
/// Enter takes the one chosen (`ConfirmChoice`), Escape shuts it. Asked after applying them:
/// Left / Right choose, Enter answers, Escape says no; yes keeps them (saved), no or the
/// countdown's end puts them back (`OnVideoSettingsConfirm`: `RestoreInitSettings`, which
/// counts as a change again) and the screen stays.
#[allow(clippy::too_many_arguments)]
fn video_input(menu: &mut Menu, settings: &mut Settings, keys: &ButtonInput<KeyCode>, help: Option<crate::helpbar::Btn>, data: &crate::gamedata::Data, dt: f32, scripted: bool, sfx: &mut MessageWriter<crate::audio::PostEvent>) {
    use crate::helpbar::Btn;
    let Some(mut v) = menu.video.clone() else { return };
    let hit = menu.video_click.take().filter(|_| !scripted);
    let click = menu.confirm_click.take();
    let up = keys.just_pressed(KeyCode::ArrowUp) || keys.just_pressed(KeyCode::KeyW);
    let down = keys.just_pressed(KeyCode::ArrowDown) || keys.just_pressed(KeyCode::KeyS);
    let lr = if keys.just_pressed(KeyCode::ArrowRight) || keys.just_pressed(KeyCode::KeyD) {
        1
    } else if keys.just_pressed(KeyCode::ArrowLeft) || keys.just_pressed(KeyCode::KeyA) {
        -1
    } else {
        0
    };
    let enter = keys.just_pressed(KeyCode::Enter) || keys.just_pressed(KeyCode::Space) || keys.just_pressed(KeyCode::KeyE) || help == Some(Btn::A) || hit == Some(VideoHit::Apply);
    let escape = keys.just_pressed(KeyCode::Escape) || help == Some(Btn::B) || hit == Some(VideoHit::Cancel);
    let list = crate::settings::resolutions();
    let before = (v.sel, v.lines, v.drop, v.asked.as_ref().map(|a| a.2));
    // asked whether they are right
    if let Some((q, t, choice)) = v.asked.clone() {
        let t = t + dt;
        let mut choice = choice;
        if lr != 0 {
            choice = 1 - choice.min(1);
        }
        if let Some(c) = click {
            choice = c;
        }
        if choice != v.asked.as_ref().map_or(choice, |a| a.2) {
            ui(sfx, "UI_Move");
        }
        let kept = if t >= VIDEO_COUNTDOWN || escape {
            Some(false)
        } else if enter || click.is_some() {
            Some(choice == 0)
        } else {
            None
        };
        // (the box's answers: `Confirmation`, `Back`)
        if kept.is_some() && t < VIDEO_COUNTDOWN {
            ui(sfx, if escape { "UI_Back" } else { "UI_Confirmation" });
        }
        match kept {
            Some(true) => {
                settings.save();
                menu.video = None;
                menu.dirty = true;
                return;
            }
            Some(false) => {
                v.lines = v.init;
                v.init.apply(settings);
                v.modified = true;
                v.asked = None;
            }
            None => v.asked = Some((q, t, choice)),
        }
    } else if let Some((at, top)) = v.drop {
        // the drop list: the one chosen kept in view
        let mut at = at;
        if up || down {
            let to = if up { at.saturating_sub(1) } else { (at + 1).min(list.len().saturating_sub(1)) };
            ui(sfx, if to != at { "UI_Move" } else { "UI_Stop" });
            at = to;
        }
        if let Some(VideoHit::DropItem(i)) = hit {
            at = i;
        }
        let top = top.min(at).max((at + 1).saturating_sub(DROP_LINES));
        v.drop = Some((at, top));
        if enter || matches!(hit, Some(VideoHit::DropItem(_))) {
            v.lines.resolution = list.get(at).copied();
            v.modified = true;
            v.drop = None;
            ui(sfx, "UI_Validation");
        } else if escape {
            v.drop = None;
            ui(sfx, "UI_Back");
        }
    } else {
        // (its lines no further than their ends: `Stop`)
        let n = VIDEO_LINES.len();
        if up || down {
            let to = if up { v.sel.saturating_sub(1) } else { (v.sel + 1).min(n - 1) };
            ui(sfx, if to != v.sel { "UI_Move" } else { "UI_Stop" });
            v.sel = to;
        }
        // a value stepped (`LeftPressed` / `RightPressed`: no further than off or on), clicked,
        // or the drop list opened
        let mut set = |line: usize, on: bool, v: &mut VideoEdit| {
            let b = if line == 0 { &mut v.lines.fullscreen } else { &mut v.lines.vsync };
            if *b != on {
                *b = on;
                v.modified = true;
                ui(sfx, "UI_Change");
            } else {
                ui(sfx, "UI_Stop");
            }
        };
        match hit {
            Some(VideoHit::Choice(line, on)) => {
                v.sel = line;
                set(line, on, &mut v);
            }
            Some(VideoHit::Drop) => v.sel = 2,
            _ => {}
        }
        if v.sel < 2 && lr != 0 {
            set(v.sel, lr > 0, &mut v);
        }
        if v.sel == 2 && !list.is_empty() && (lr != 0 || hit == Some(VideoHit::Drop) || help == Some(Btn::X)) {
            let at = resolution_at(v.lines.resolution, &list);
            v.drop = Some((at, (at + 1).saturating_sub(DROP_LINES)));
            ui(sfx, "UI_Change");
        } else if enter {
            ui(sfx, "UI_Validation");
            if !v.modified {
                menu.video = None;
                menu.dirty = true;
                return;
            }
            v.lines.apply(settings);
            v.asked = Some((data.text("DisGFxMoviePlayerMenuBase_Texts", "t_Q_VideoSettings"), 0.0, 0));
        } else if escape {
            ui(sfx, "UI_Back");
            v.init.apply(settings);
            menu.video = None;
            menu.dirty = true;
            return;
        }
    }
    if before != (v.sel, v.lines, v.drop, v.asked.as_ref().map(|a| a.2)) {
        menu.dirty = true;
    }
    menu.video = Some(v);
}

/// The pause menu's opening and closing sounds (`Open`: `OpenScreen`, `Close`: `CloseScreen`;
/// `ShowGameOverMenu`: `GO_OpenGameOverMenu`).
fn open_sounds(menu: Res<Menu>, mut was: Local<Option<MenuKind>>, mut sfx: MessageWriter<crate::audio::PostEvent>) {
    if menu.open == *was {
        return;
    }
    match (*was, menu.open) {
        (_, Some(MenuKind::GameOver)) => ui(&mut sfx, "UI_GO_OpenGameOverMenu"),
        (None, Some(MenuKind::Pause)) => ui(&mut sfx, "UI_OpenScreen"),
        (Some(MenuKind::Pause), None) => ui(&mut sfx, "UI_CloseScreen"),
        _ => {}
    }
    *was = menu.open;
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
    (data, mut launch, real): (Res<crate::gamedata::Data>, ResMut<crate::challenge::ChallengeLaunch>, Res<Time<Real>>),
    (mut profile, movies, mut play): (ResMut<crate::challenge::ChallengeProfile>, Res<crate::movie::Movies>, MessageWriter<crate::movie::PlayMovie>),
    (note, mut read, mut sfx): (Res<crate::notescreen::NoteScreen>, MessageWriter<crate::journal::ReadNote>, MessageWriter<crate::audio::PostEvent>),
    (help, stats_up, taps, mut delete): (Query<(&Interaction, &crate::helpbar::HelpButton), Changed<Interaction>>, Option<Res<crate::mission::MissionEnd>>, Res<crate::helpbar::Taps>, MessageWriter<crate::save::DeleteSave>),
    scroll: Option<Res<bevy::input::mouse::AccumulatedMouseScroll>>,
) {
    let Some(kind) = menu.open else { return };
    // (a mission's statistics over it: their keys are their own)
    if stats_up.is_some() {
        return;
    }
    // the help bar's entries clicked (A as Enter, B as Escape), or X (Shift tapped)
    let help = crate::helpbar::clicked(&help).filter(|_| scripted.is_none()).or_else(|| crate::helpbar::Btn::X.just_pressed(&keys, &taps).then_some(crate::helpbar::Btn::X));
    // (a note read over it, or put away this frame: its keys are its own)
    if note.open.is_some() || note.shut {
        return;
    }
    // (a movie over the menu, the trials' credits: its keys are its own)
    if movies.holding() {
        return;
    }
    // binding a key: the next one pressed (Escape cancels)
    if let Some(a) = menu.capture {
        if keys.just_pressed(KeyCode::Escape) {
            menu.capture = None;
            menu.dirty = true;
        } else if let Some(k) = keys
            .get_just_pressed()
            .copied()
            .find(|k| crate::bindings::SUPPORTED.contains(k))
            .map(crate::bindings::Bind::Key)
            // (the mouse's buttons and its wheel bind too, as the original's mapping takes them)
            .or_else(|| mouse.get_just_pressed().copied().find(|m| crate::bindings::MOUSE.contains(m)).map(crate::bindings::Bind::Mouse))
            .or_else(|| scroll.filter(|s| s.delta.y != 0.0).map(|s| if s.delta.y > 0.0 { crate::bindings::Bind::WheelUp } else { crate::bindings::Bind::WheelDown }))
        {
            crate::bindings::Bindings::rebind(&mut settings, a, k);
            settings.save();
            menu.capture = None;
            menu.dirty = true;
        }
        return;
    }
    // the brightness screen (`GammaSetting`): Left / Right change it as it shows, Enter keeps it
    // (`A`), Escape puts it back as it was (`B`: `OnWidgetChange` to the start value)
    if let Some(start) = menu.gamma {
        let lr = if keys.just_pressed(KeyCode::ArrowRight) || keys.just_pressed(KeyCode::KeyD) {
            1.0
        } else if keys.just_pressed(KeyCode::ArrowLeft) || keys.just_pressed(KeyCode::KeyA) {
            -1.0
        } else {
            0.0
        };
        if lr != 0.0 {
            let was = settings.brightness;
            adjust(&mut settings, Opt::Brightness, lr, false);
            ui(&mut sfx, if settings.brightness != was { "UI_Change" } else { "UI_Stop" });
        }
        if keys.just_pressed(KeyCode::Enter) || help == Some(crate::helpbar::Btn::A) {
            settings.save();
            menu.gamma = None;
            menu.dirty = true;
            ui(&mut sfx, "UI_Validation");
        } else if keys.just_pressed(KeyCode::Escape) || help == Some(crate::helpbar::Btn::B) {
            settings.brightness = start;
            menu.gamma = None;
            menu.dirty = true;
            ui(&mut sfx, "UI_Back");
        }
        return;
    }
    // the controls scheme's screen (`GamepadMapping`): Left / Right step the schemes as it shows
    // them (`OnWidgetChange`: `req_GamepadMappingScreen`), Enter keeps the one shown (`A`),
    // Escape puts back the one it opened with (`B`)
    if let Some(start) = menu.pad_map {
        let lr = if keys.just_pressed(KeyCode::ArrowRight) || keys.just_pressed(KeyCode::KeyD) {
            1.0
        } else if keys.just_pressed(KeyCode::ArrowLeft) || keys.just_pressed(KeyCode::KeyA) {
            -1.0
        } else {
            0.0
        };
        if lr != 0.0 {
            let was = settings.pad_binding_set;
            adjust(&mut settings, Opt::PadScheme, lr, false);
            ui(&mut sfx, if settings.pad_binding_set != was { "UI_Change" } else { "UI_Stop" });
            menu.dirty = true;
        }
        if keys.just_pressed(KeyCode::Enter) || help == Some(crate::helpbar::Btn::A) {
            settings.save();
            menu.pad_map = None;
            menu.dirty = true;
            ui(&mut sfx, "UI_Validation");
        } else if keys.just_pressed(KeyCode::Escape) || help == Some(crate::helpbar::Btn::B) {
            settings.pad_binding_set = start;
            menu.pad_map = None;
            menu.dirty = true;
            ui(&mut sfx, "UI_Back");
        }
        return;
    }
    // the video settings (`VideoSettings`)
    if menu.video.is_some() {
        video_input(&mut menu, &mut settings, &keys, help, &data, real.delta_secs(), scripted.is_some(), &mut sfx);
        return;
    }
    // a question asked: Left / Right choose, Enter answers, Escape says no
    let mut answered = None;
    if let Some((_, action, choice)) = menu.confirm.clone() {
        let mut choice = choice;
        if keys.just_pressed(KeyCode::ArrowLeft) || keys.just_pressed(KeyCode::ArrowRight) || keys.just_pressed(KeyCode::KeyA) || keys.just_pressed(KeyCode::KeyD) {
            choice = 1 - choice.min(1);
            ui(&mut sfx, "UI_Move");
        }
        let click = menu.confirm_click.take();
        if let Some(c) = click {
            choice = c;
        }
        // (the box's answers: `Confirmation`, `Back`)
        if keys.just_pressed(KeyCode::Escape) {
            menu.confirm = None;
            menu.dirty = true;
            ui(&mut sfx, "UI_Back");
            return;
        }
        if keys.just_pressed(KeyCode::Enter) || keys.just_pressed(KeyCode::Space) || keys.just_pressed(KeyCode::KeyE) || click.is_some() {
            menu.confirm = None;
            menu.dirty = true;
            ui(&mut sfx, "UI_Confirmation");
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
            ui(&mut sfx, "UI_M_PressStart");
            ui(&mut sfx, "UI_M_OpenMainMenu");
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
            ui(&mut sfx, "UI_ChangeMainTab");
            return;
        }
        if nsub > 1 && (keys.just_pressed(KeyCode::KeyQ) || keys.just_pressed(KeyCode::KeyE)) {
            menu.set_options_tab(cat, if keys.just_pressed(KeyCode::KeyE) { (sub + 1) % nsub } else { (sub + nsub - 1) % nsub });
            ui(&mut sfx, "UI_ChangeSubTab");
            return;
        }
        if answered.is_none() && help == Some(crate::helpbar::Btn::X) {
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
    // the gallery: a piece seen large (`CM_Gallery_ViewPort_Screen`): the arrows (or the
    // shoulders) to the next unlocked, Escape back to the list
    if menu.page == Page::Gallery && menu.gallery_view.is_some() {
        if keys.just_pressed(KeyCode::Escape) || mouse.just_pressed(MouseButton::Right) {
            menu.gallery_view = None;
            menu.dirty = true;
        } else if keys.just_pressed(KeyCode::ArrowRight) || keys.just_pressed(KeyCode::KeyD) || keys.just_pressed(KeyCode::PageDown) {
            menu.gallery_step(1);
        } else if keys.just_pressed(KeyCode::ArrowLeft) || keys.just_pressed(KeyCode::KeyA) || keys.just_pressed(KeyCode::PageUp) {
            menu.gallery_step(-1);
        }
        return;
    }
    // the gallery's grid (`CM_Gallery_List`: 7 across)
    if menu.page == Page::Gallery && answered.is_none() {
        let step: i32 = if keys.just_pressed(KeyCode::ArrowRight) || keys.just_pressed(KeyCode::KeyD) {
            1
        } else if keys.just_pressed(KeyCode::ArrowLeft) || keys.just_pressed(KeyCode::KeyA) {
            -1
        } else if keys.just_pressed(KeyCode::ArrowDown) || keys.just_pressed(KeyCode::KeyS) {
            GALLERY_COLS as i32
        } else if keys.just_pressed(KeyCode::ArrowUp) || keys.just_pressed(KeyCode::KeyW) {
            -(GALLERY_COLS as i32)
        } else {
            0
        };
        if step != 0 {
            let to = menu.sel as i32 + step;
            if (0..n as i32).contains(&to) {
                menu.sel = to as usize;
                menu.dirty = true;
            }
            return;
        }
    }
    // a leaderboard (`Leaderboards_Screen`): Left / Right the challenge, Up / Down its rows,
    // Tab (or Page Up / Down) the mode's tab
    if matches!(menu.page, Page::Leaderboards | Page::ExpertLeaderboards) && answered.is_none() {
        if keys.just_pressed(KeyCode::Tab) || keys.just_pressed(KeyCode::PageUp) || keys.just_pressed(KeyCode::PageDown) {
            let expert = menu.page == Page::Leaderboards;
            menu.switch_trials_tab(expert);
            return;
        }
        if keys.just_pressed(KeyCode::ArrowRight) || keys.just_pressed(KeyCode::KeyD) {
            menu.board_step(1);
            return;
        }
        if keys.just_pressed(KeyCode::ArrowLeft) || keys.just_pressed(KeyCode::KeyA) {
            menu.board_step(-1);
            return;
        }
        let dy: i32 = if keys.just_pressed(KeyCode::ArrowDown) || keys.just_pressed(KeyCode::KeyS) {
            1
        } else if keys.just_pressed(KeyCode::ArrowUp) || keys.just_pressed(KeyCode::KeyW) {
            -1
        } else {
            0
        };
        if dy != 0 {
            if let Some((c, expert)) = menu.item_board(menu.sel) {
                let key = data.0.challenges.get(c).map(|d| crate::challenge::best_key(&d.id, expert)).unwrap_or_default();
                let (rows, last) = profile.board(&key);
                if !rows.is_empty() {
                    let cur = menu.board_row.or(last).unwrap_or(0) as i32;
                    menu.board_row = Some((cur + dy).clamp(0, rows.len() as i32 - 1) as usize);
                }
            }
            return;
        }
    }
    // the challenges' grid (`CM_ChallengesList_List`: 5 across): the arrows move about it,
    // Tab (or Page Up / Down) changes the mode's tab
    if matches!(menu.page, Page::Challenges | Page::ExpertChallenges) && answered.is_none() {
        if keys.just_pressed(KeyCode::Tab) || keys.just_pressed(KeyCode::PageUp) || keys.just_pressed(KeyCode::PageDown) {
            let expert = menu.page == Page::Challenges;
            menu.switch_trials_tab(expert);
            return;
        }
        let step: i32 = if keys.just_pressed(KeyCode::ArrowRight) || keys.just_pressed(KeyCode::KeyD) {
            1
        } else if keys.just_pressed(KeyCode::ArrowLeft) || keys.just_pressed(KeyCode::KeyA) {
            -1
        } else if keys.just_pressed(KeyCode::ArrowDown) || keys.just_pressed(KeyCode::KeyS) {
            5
        } else if keys.just_pressed(KeyCode::ArrowUp) || keys.just_pressed(KeyCode::KeyW) {
            -5
        } else {
            0
        };
        if step != 0 {
            let to = menu.sel as i32 + step;
            if (0..n as i32).contains(&to) {
                menu.sel = to as usize;
                menu.dirty = true;
            }
            return;
        }
    }
    // the save lists' X (`SaveList`): the chosen save deleted, asked first (`t_Q_DeleteSave`)
    if matches!(menu.page, Page::Load | Page::Save) && answered.is_none() && help == Some(crate::helpbar::Btn::X) {
        if let Some(slot) = menu.item_slot(menu.sel).filter(|s| slots.info(*s).is_some()) {
            menu.confirm = Some((data.text("DisGFxMoviePlayerMenuBase_Texts", "t_Q_DeleteSave"), Action::DeleteSave(slot), 0));
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
                    ui(&mut sfx, "UI_Move");
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
    // (the lists go no further than their ends: `SelectionHandler`'s `_bSelectionLoop` unset)
    let up_down = if keys.just_pressed(KeyCode::ArrowDown) || keys.just_pressed(KeyCode::KeyS) {
        Some(true)
    } else if keys.just_pressed(KeyCode::ArrowUp) || keys.just_pressed(KeyCode::KeyW) {
        Some(false)
    } else {
        None
    };
    if let Some(down) = up_down {
        let to = if down { (menu.sel + 1).min(n - 1) } else { menu.sel.saturating_sub(1) };
        ui(&mut sfx, if to != menu.sel { "UI_Move" } else { "UI_Stop" });
        if to != menu.sel {
            menu.sel = to;
            menu.dirty = true;
        }
    }
    if keys.just_pressed(KeyCode::Enter) || keys.just_pressed(KeyCode::Space) || keys.just_pressed(KeyCode::KeyE) || help == Some(crate::helpbar::Btn::A) {
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
    // (and the missions' slide: `SlideMenu`'s left and right)
    if left_right != 0.0 && menu.front_page().is_some_and(|f| f.0 == 1 || f.0 == crate::missionscreen::CODE || f.0 == crate::dlcmenu::NEW_GAME) {
        let to = if left_right > 0.0 { (menu.sel + 1).min(n - 1) } else { menu.sel.saturating_sub(1) };
        ui(&mut sfx, if to != menu.sel { "UI_Move" } else { "UI_Stop" });
        if to != menu.sel {
            menu.sel = to;
            menu.dirty = true;
        }
    }
    if left_right != 0.0 {
        // (the brightness's link and the video settings' open their screens: not stepped)
        if let Some((_, Action::Adjust(o))) = menu.items.get(menu.sel) {
            if !matches!(o, Opt::Brightness | Opt::VideoSettings | Opt::PadScheme) {
                activate = Some(menu.sel);
                dir = left_right;
            }
        }
    }
    if (keys.just_pressed(KeyCode::Escape) || help == Some(crate::helpbar::Btn::B)) && activate.is_none() {
        ui(&mut sfx, "UI_Back");
        if !menu.back() && (kind == MenuKind::Pause || menu.board_over_results) {
            menu.open = None;
            menu.board_over_results = false;
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
                Action::QuitToMenu => Some(match menu.dlc {
                    Some(pack) => pack_words(&data, pack, data.text(base, "t_Q_BackToDLC06MainMenu")),
                    None => data.text(base, "t_Q_BackToMainMenu"),
                }),
                Action::LeaveDlc => Some(pack_words(&data, menu.dlc.unwrap_or(0), data.text(base, if kind == MenuKind::Pause { "t_Q_ExitDLC06_IG" } else { "t_Q_ExitDLC06" }))),
                Action::QuitGame => Some(data.text(base, if kind == MenuKind::Main { "t_Q_QuitGame" } else { "t_Q_BackToWindows" })),
                // (a story pack's speaks of its own autosaves, `t_Q_NewGame_DLC07`)
                Action::Difficulty(_) if latest_of(&slots, menu.dlc).is_some() => Some(match menu.dlc {
                    Some(pack) => data.text("DisGFxMoviePlayerMainMenu_Texts", "t_Q_NewGame_DLC07").replace("\u{a7}DLC07_NAME\u{a7}", &data.text("DisGFxMoviePlayerBase_Texts", DLC_PACKS[pack].name)),
                    None => data.text("DisGFxMoviePlayerMainMenu_Texts", "t_Q_NewGame"),
                }),
                // (`t_useDLC06Save_q`, `t_useNoDLC06Save_q`)
                Action::UseTransition(c) => {
                    let q = data.text(base, if c.is_some() { "t_useDLC06Save_q" } else { "t_useNoDLC06Save_q" });
                    let save = match c {
                        Some(true) => data.text("DisGFxMoviePlayerMainMenu_Texts", "t_HighChaos"),
                        _ => data.text("DisGFxMoviePlayerMainMenu_Texts", "t_LowChaos"),
                    };
                    Some(pack_words(&data, 0, q.replace("\u{a7}SAVE\u{a7}", &save)))
                }
                Action::LeaveTrials if kind == MenuKind::Pause => Some(data.text("DisGFxMoviePlayerBase_Texts", "t_Q_ExitDLC05_IG")),
                Action::LeaveTrials => Some(data.text("DisDLC05MoviePlayerChallengeMenu_Texts", "t_Q_DLC05_BackToMain")),
                Action::RestartChallenge => Some(data.text("DisGFxMoviePlayerBase_Texts", "t_Q_RestartChallenge")),
                Action::EndChallenge => Some(data.text("DisGFxMoviePlayerBase_Texts", "t_Q_EndChallenge")),
                Action::ExitChallenge => Some(data.text("DisGFxMoviePlayerBase_Texts", "t_Q_BackToChallenges_IG")),
                _ => None,
            };
            match action {
                Action::Adjust(Opt::Brightness | Opt::VideoSettings | Opt::PadScheme) => ui(&mut sfx, "UI_Validation"),
                Action::Adjust(_) => {}
                Action::LockedChallenge(_) | Action::LockedGallery(_) | Action::LockedMission(_) | Action::LockedDifficulty => ui(&mut sfx, "UI_Failure"),
                Action::Back => ui(&mut sfx, "UI_Back"),
                _ => ui(&mut sfx, "UI_Validation"),
            }
            if let Some(q) = question.filter(|q| !q.is_empty()) {
                menu.confirm = Some((q, action, 0));
                menu.dirty = true;
                return;
            }
            action
        }
    };
    match action {
        Action::Page(p) => {
            // (the game's own campaign: New Game)
            if p == Page::Difficulty {
                menu.campaign_map = None;
            }
            menu.go(p)
        }
        Action::NewCampaign(map) => {
            menu.campaign_map = Some(map);
            menu.go(Page::Difficulty)
        }
        Action::ReadTutorial(i) => {
            if let Some(id) = menu.tutorials.get(i) {
                read.write(crate::journal::ReadNote(format!("{}{id}", crate::tutorials::KEY)));
            }
        }
        Action::Back => {
            menu.back();
        }
        // the brightness's link: its screen (`DisplayGammaSetting`)
        Action::Adjust(Opt::Brightness) => {
            menu.gamma = Some(settings.brightness);
            menu.dirty = true;
        }
        // the controls scheme's link: its screen (`DisplayGamepadMapping`)
        Action::Adjust(Opt::PadScheme) => {
            menu.pad_map = Some(settings.pad_binding_set);
            menu.dirty = true;
        }
        // the video settings' link: its screen (`DisplayVideoSettings`), on its first line
        Action::Adjust(Opt::VideoSettings) => {
            let v = VideoValues::of(&settings);
            menu.video = Some(VideoEdit { init: v, lines: v, sel: 0, modified: false, drop: None, asked: None });
            menu.video_click = None;
            menu.dirty = true;
        }
        Action::Adjust(o) => {
            let was = opt_value(&settings, o);
            adjust(&mut settings, o, dir, answered.is_none() && left_right == 0.0);
            ui(&mut sfx, if opt_value(&settings, o) != was { "UI_Change" } else { "UI_Stop" });
            settings.save();
            menu.dirty = true;
        }
        Action::Rebind(a) => {
            menu.capture = Some(a);
            menu.dirty = true;
        }
        Action::ResetBindings => {
            ui(&mut sfx, "UI_Reset");
            // (the options' restore: the category shown)
            if menu.page == Page::Options {
                menu.restore_settings(&mut settings);
            } else {
                settings.bindings.clear();
            }
            settings.save();
            menu.dirty = true;
        }
        // (The Brigmore Witches', a Knife of Dunwall end found: which to take up, first)
        Action::Difficulty(d) if menu.campaign_map == Some(DLC_PACKS[1].first) && crate::save::transitions_found().iter().any(|f| *f) => {
            settings.difficulty = d;
            settings.save();
            menu.go(Page::Transition);
        }
        Action::Difficulty(_) | Action::UseTransition(_) => {
            *crate::save::TRANSITION_CHOICE.lock().unwrap() = match action {
                Action::UseTransition(Some(high)) => crate::save::TransitionChoice::Chaos(high),
                Action::UseTransition(None) => crate::save::TransitionChoice::Nothing,
                _ => crate::save::TransitionChoice::Unasked,
            };
            *campaign = Default::default();
            if let Action::Difficulty(d) = action {
                settings.difficulty = d;
                settings.save();
            }
            // (a story pack's from its first map; the game's own with the intro while the Tower
            // loads)
            let start = menu.campaign_map.take();
            config.map = start.unwrap_or(FIRST_MAP).into();
            config.spawn_index = None;
            menu.open = None;
            intro.0 = true;
            next.set(GameState::Loading);
        }
        Action::Continue => {
            if let Some(slot) = latest_of(&slots, menu.dlc) {
                load.write(LoadRequest(slot));
                menu.open = None;
            } else if kind == MenuKind::GameOver {
                // (nothing saved: the mission again)
                menu.open = None;
                next.set(GameState::Loading);
            }
        }
        Action::Load(slot) | Action::Mission(slot) => {
            load.write(LoadRequest(slot));
            menu.open = None;
        }
        Action::Save(slot) => {
            save.write(SaveRequest(slot));
            menu.back();
        }
        Action::DeleteSave(slot) => {
            delete.write(crate::save::DeleteSave(slot));
            menu.dirty = true;
        }
        Action::Resume => {
            menu.open = None;
            menu.dirty = true;
        }
        Action::QuitToMenu => {
            // (out of a story pack's mission: to its own front end)
            config.map = match menu.dlc.map(|p| DLC_PACKS[p].menu).filter(|m| map_cooked(m)) {
                Some(m) => m.into(),
                None => MENU_MAP.into(),
            };
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
        Action::LockedChallenge(_) | Action::LockedGallery(_) | Action::LockedMission(_) | Action::LockedDifficulty => {}
        Action::DlcMenu(pack) => {
            config.map = DLC_PACKS[pack].menu.into();
            config.spawn_index = None;
            menu.open = None;
            next.set(GameState::Loading);
        }
        Action::LeaveDlc => {
            config.map = MENU_MAP.into();
            config.spawn_index = None;
            menu.open = None;
            next.set(GameState::Loading);
        }
        Action::DlcCredits(pack) => {
            play.write(crate::movie::PlayMovie { name: DLC_PACKS[pack].credits.into(), looping: false, skippable: true, layer: crate::movie::MovieLayer::Fullscreen });
        }
        Action::GalleryView(_) => {
            menu.gallery_view = Some(menu.sel);
            menu.dirty = true;
        }
        Action::RestartChallenge => {
            // (the same map, the same mode)
            config.spawn_index = None;
            menu.open = None;
            next.set(GameState::Loading);
        }
        Action::EndChallenge => {
            launch.end_now = true;
            menu.open = None;
            menu.dirty = true;
        }
        Action::ExitChallenge => {
            config.map = TRIALS_MAP.into();
            config.spawn_index = None;
            launch.back_to_challenges = true;
            menu.open = None;
            next.set(GameState::Loading);
        }
        Action::WelcomeDone => {
            profile.welcome_seen = true;
            profile.save();
            menu.page = Page::Trials;
            menu.stack.clear();
            menu.sel = 0;
            menu.dirty = true;
        }
        Action::Board(..) => {}
        Action::TrialsCredits => {
            play.write(crate::movie::PlayMovie { name: "CreditsDLC05".into(), looping: false, skippable: true, layer: crate::movie::MovieLayer::Fullscreen });
        }
        Action::Trials | Action::LeaveTrials => {
            config.map = if matches!(action, Action::Trials) { TRIALS_MAP.into() } else { MENU_MAP.into() };
            config.spawn_index = None;
            menu.open = None;
            next.set(GameState::Loading);
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
    (data, profile, challenge, stats): (Res<crate::gamedata::Data>, Res<crate::challenge::ChallengeProfile>, Res<crate::challenge::Challenge>, Res<crate::gameplay::PlayerStats>),
) {
    // (the saves read again, one deleted: the save lists rebuilt)
    let saves = slots.is_changed() && matches!(menu.page, Page::Load | Page::Save);
    if !menu.dirty && !(settings.is_changed() && menu.page == Page::Options) && !saves {
        return;
    }
    if menu.tutorials != stats.tutorials {
        menu.tutorials = stats.tutorials.clone();
    }
    menu.dirty = false;
    for e in &roots {
        commands.entity(e).despawn();
    }
    let Some(kind) = menu.open else { return };
    let trial = challenge.def.as_ref().map(|d| TrialRun { can_end_early: d.can_end_early });
    menu.items = items(&menu, &settings, &slots, &data, &profile, trial);
    if let Some(c) = menu.pending_challenge.take() {
        if let Some(i) = (0..menu.items.len()).find(|&i| menu.item_trial(i) == Some(c)) {
            menu.sel = i;
        }
    }
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
            // (the found end's words, `t_transitionFromDLC06Desc`, its line breaks the movie's)
            if menu.page == Page::Transition {
                let words = pack_words(&data, 0, data.text("DisGFxMoviePlayerMenuBase_Texts", "t_transitionFromDLC06Desc")).replace("<br/>", "\n");
                col.spawn((Text::new(words), TextFont { font: title_font.clone(), font_size: bevy::text::FontSize::Px(24.0), ..default() }, TextColor(white), Node { max_width: px(760), margin: UiRect::bottom(px(18)), ..default() }));
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
    mut hud: Query<(&mut Visibility, Has<crate::kismet::SubtitleText>), Or<(With<HudRoot>, With<crate::gauges::GaugesRoot>, With<crate::kismet::SubtitleText>, With<crate::kismet::ObjectivesText>)>>,
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
    for (mut v, subtitle) in &mut hud {
        // (the words of a scene are heard through it: only the screens over the game cover them)
        let want = if if subtitle { covered || killcam.active() } else { hidden.0 } { Visibility::Hidden } else { Visibility::Inherited };
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
