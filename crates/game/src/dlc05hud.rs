//! Dunwall City Trials' HUD: the elements of `UI_HUD_DLC05.HUD` (cooked as `HUD_DLC05`) as
//! the movie's classes run them, driven by the run (`challenge.rs`). Each element is its symbol
//! put where `DLC05_H_BaseElement.SetPosition` puts it (its side of the stage's safe area, 90%
//! on PC: `_hPos`, `_vPos`), opened, updated and closed with its class's tweens, delays and
//! sounds (the HUD tweak's sound theme: `H_DLC05_Wave` plays `UI_H_DLC05_Wave`...):
//! - the score (`DLC05_H_Score`, left): the points scored and their multiplier over the total,
//!   cashed in once the scoring stops (`Cash`), then gone;
//! - the timer (`DLC05_H_Timer`, top right): minutes, seconds and hundredths, a field a digit
//!   (`Timer_timerTxt_txt`) laid right to left, over three shadows;
//! - the kills and tanks counts (`DLC05_H_KillsCount`, `_TanksCount`), the coins and eggs
//!   (`DLC05_H_BaseCount`: `cur/max`), the chances left (`DLC05_H_BustedCount`: an icon each,
//!   `busted` as one goes), the gates (`DLC05_H_CPCount`) and the enemies left
//!   (`DLC05_H_EnemiesLeftCount`: a skull each, `ELC_skullIc`, killed as they fall);
//! - round titles (`DLC05_H_Wave`: "ROUND n" by its number in a turning circle, or a title of
//!   the scripts'), the count before a start (`DLC05_H_CountdownStart`: 3, 2, 1, GO!) and a
//!   countdown timer's seconds (`DLC05_H_Countdown`: an hourglass, red under ten);
//! - a round's results (`DLC05_H_PhaseResultsScreen`): its name with its success or failure,
//!   the kills possible, wanted and made (`PR_stat`), and the way on (`PR_menu_btn`: the next
//!   round, or the bonus round, or the round again; the end of the challenge), the game held
//!   until one is chosen;
//! - a drop's height as Corvo falls (`DLC05_H_JumpHeight`, Drop Attack), a clockwork egg found
//!   (`DLC05_H_EggDiscovery`), the kill chain's time left (`DLC05_H_ChainGauge`: a gauge and
//!   its seconds, "Chain broken" when it runs out), the mystery man's clues as they're found
//!   (`DLC05_H_Clues`) and, all found, its portrait (`DLC05_H_MMTarget`), and what the
//!   scripts unlocked (`DLC05_H_EquipmentUnlocked`: powers and upgrades, their pictures, the
//!   powers' levels). Their times are the HUD's own (`DisDLC05MoviePlayerHUD` defaults: a
//!   round's title 3 s, a flair 2 s, a count 2 s, an unlock 5 s, the portrait 3 s on).

use crate::challenge::{Challenge, Grant, HudEvent, HudItem};
use crate::flash::{Clip, Ease, FlashClip, MovieTimelines, Props, PropsTo, IDENTITY};
use crate::GameState;
use bevy::prelude::*;
use std::collections::{BTreeMap, HashMap};

pub struct Dlc05HudPlugin;

impl Plugin for Dlc05HudPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Update, (drive, phase_input).chain().run_if(in_state(GameState::InGame)));
    }
}

const MOVIE: &str = "HUD_DLC05";
/// the stage's safe area on PC (`ScreenPosition.GetMovieSpaceCoord(0.9)`)
const SAFE_MIN: Vec2 = Vec2::new(64.0, 36.0);
const SAFE_MAX: Vec2 = Vec2::new(1216.0, 684.0);
const H_TEXTS: &str = "DisDLC05MoviePlayerHUD_Texts";
/// `DLC05_H_Score._scoreDisplayDuration` (the movie's own test sets 1.5 s)
const SCORE_SHOWN: f32 = 1.5;
/// how long the score waits for more before it cashes in
const CASH_AFTER: f32 = 1.0;
/// a round's title (`m_fWaveNumberDisplayDuration`)
const WAVE_SHOWN: f32 = 3.0;
/// a flair named (`m_fTrickDisplayDuration_Single`)
const TRICK_SHOWN: f32 = 2.0;
/// a count shown on its own (`m_fItemCountDisplayDuration`: the egg found)
const ITEM_SHOWN: f32 = 2.0;
/// what was unlocked (`m_fEquipmentUnlockDisplayDuration`)
const UNLOCK_SHOWN: f32 = 5.0;
/// the clues all found: the mystery foe shown after (`m_fMysteryFoeRevealDelay`)
const FOE_REVEAL: f32 = 3.0;
/// a fall shown once this high (metres)
const MIN_DROP: f32 = 3.0;
/// the kill chain's parts: its countdown (`DLC05_H_ChainGauge_CD`, critical under 2), "Chain
/// broken" (`DLC05_H_ChainGauge_Broken`, a round title of its own)
const CHAIN_CD: &str = "_chain_mc.cd_mc._cd_mc";
const CHAIN_CRITICAL: i64 = 2;
/// a clue's line: its words' field's height (`Clues_clueItem.txt`)
const CLUE_LINE: f32 = 29.7;
/// `DLC05_H_Timer`'s digit fields, right to left, and the gap after each (`_offsetX`, -4;
/// the separators' +5, +10)
const TIMER_FIELDS: [(&str, f32); 8] = [("hun_0", -4.0), ("hun_1", -4.0), ("sep_sec", 1.0), ("sec_0", -4.0), ("sec_1", -4.0), ("sep_min", 6.0), ("min_0", -4.0), ("min_1", -4.0)];
const TIMER_MCS: [&str; 4] = ["_timer_mc.txt_mc", "_timer_mc.txtShad0_mc", "_timer_mc.txtShad1_mc", "_timer_mc.txtShad2_mc"];
/// `DLC05_H_Countdown`: under this the seconds are critical (`_criticalValue`), their shadows
/// red (`_rColor`) rather than dark (`_defColor`)
const CRITICAL: i64 = 10;
const RED: &str = "#821415";
const DARK: &str = "#17191c";
/// `DLC05_H_BaseCount`'s colours (`_whiteColorTag`, `_greyColorTag`)
const WHITE: &str = "#e4f3d7";
const GREY: &str = "#a8b49e";
/// the gap between the enemies' skulls
const SKULL_GAP: f32 = 2.0;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum Kind {
    Score,
    Timer,
    Kills,
    Tanks,
    Coins,
    Eggs,
    Busted,
    Gates,
    Enemies,
    Wave,
    CountdownStart,
    Countdown,
    PhaseResults,
    Tricks,
    JumpHeight,
    EggDiscovery,
    ChainGauge,
    Clues,
    MMTarget,
    EquipmentUnlocked,
}

const KINDS: [Kind; 20] = [
    Kind::Score,
    Kind::Timer,
    Kind::Kills,
    Kind::Tanks,
    Kind::Coins,
    Kind::Eggs,
    Kind::Busted,
    Kind::Gates,
    Kind::Enemies,
    Kind::Wave,
    Kind::CountdownStart,
    Kind::Countdown,
    Kind::JumpHeight,
    Kind::EggDiscovery,
    Kind::ChainGauge,
    Kind::Clues,
    Kind::MMTarget,
    Kind::EquipmentUnlocked,
    Kind::Tricks,
    Kind::PhaseResults,
];

impl Kind {
    /// Its symbol, and where it goes (`_hPos`, `_vPos`: 0 left / top, 1 middle, 2 right /
    /// bottom).
    fn symbol(self) -> (&'static str, u8, u8) {
        match self {
            Kind::Score => ("DLC05_Score", 0, 1),
            Kind::Timer => ("DLC05_Timer", 2, 0),
            Kind::Kills => ("DLC05_H_KillsCount", 2, 0),
            Kind::Tanks => ("DLC05_H_TanksCount", 2, 2),
            Kind::Coins => ("DLC05_H_CoinsCount", 2, 0),
            Kind::Eggs => ("DLC05_H_EggsCount", 2, 0),
            Kind::Busted => ("DLC05_H_BustedCount", 2, 0),
            Kind::Gates => ("DLC05_H_CPCount", 1, 2),
            Kind::Enemies => ("DLC05_H_EnemiesLeftCount", 1, 0),
            Kind::Wave => ("DLC05_H_Wave", 1, 0),
            Kind::CountdownStart => ("DLC05_H_CountdownStart", 1, 1),
            Kind::Countdown => ("DLC05_H_Countdown", 1, 2),
            Kind::PhaseResults => ("DLC05_H_PhaseResultsScreen", 1, 0),
            Kind::Tricks => ("DLC05_H_Tricks", 0, 1),
            Kind::JumpHeight => ("DLC05_H_JumpHeight", 0, 1),
            Kind::EggDiscovery => ("DLC05_H_EggDiscovery", 1, 0),
            Kind::ChainGauge => ("DLC05_H_ChainGauge", 1, 0),
            Kind::Clues => ("DLC05_H_Clues", 2, 0),
            Kind::MMTarget => ("DLC05_H_MMTarget", 2, 0),
            Kind::EquipmentUnlocked => ("DLC05_H_EquipmentUnlocked", 2, 2),
        }
    }

    /// The instances whose properties it keeps as placed (`SaveProperties`).
    fn saved(self) -> &'static [&'static str] {
        match self {
            Kind::Score => &["", "_score_mc", "_inc_mc", "_multiplier_mc"],
            Kind::Timer => &[""],
            Kind::Kills | Kind::Tanks => &["", "_kills_mc.txt_mc"],
            Kind::Coins | Kind::Eggs | Kind::Busted => &["", "_mc", "_mc.txt_mc", "_mc.ic_mc"],
            Kind::Gates => &["", "_cp_mc", "_cp_mc.circleExt_mc", "_cp_mc.circleInt_mc", "_cp_mc.bkgd_mc", "_cp_mc.txt_mc"],
            Kind::Enemies => &[""],
            Kind::Wave => &["", "_wave_mc", "_wave_mc.number_mc", "_wave_mc.circle_mc", "_wave_mc.txt_mc", "_wave_mc.bkgd_mc", "_wave_mc.sepU_mc", "_wave_mc.sepD_mc", "_waveInf_mc", "_waveInf_mc.txt_mc", "_waveInf_mc.bkgd_mc", "_waveInf_mc.sepU_mc", "_waveInf_mc.sepD_mc"],
            Kind::CountdownStart => &["", "_countdown_mc.number_mc", "_countdown_mc.bkgd_mc", "_countdown_mc.blade0_mc", "_countdown_mc.blade1_mc", "_countdown_mc.circle_mc", "_countdown_mc.circleInt_mc", "_countdown_mc.circleDotLine_mc"],
            Kind::Countdown => &["", "_cd_mc", "_cd_mc.hGlass_mc", "_cd_mc.bkgd_mc", "_cd_mc.circle_mc", "_cd_mc.circle_arrow_mc", "_cd_mc.trame_mc", "_cd_mc.txt_mc", "_cd_mc.txt_mc.t0", "_cd_mc.txt_mc.t1"],
            Kind::PhaseResults => &["", "_res_mc", "_res_mc.bkgd_mc", "_res_mc.sepL_mc", "_res_mc.sepR_mc", "_res_mc.title_mc", "_res_mc.menu_mc"],
            Kind::Tricks => &["", "_trick_mc"],
            Kind::JumpHeight => &["", "_jumpH_mc", "_jumpH_mc.circle_mc", "_jumpH_mc.bkgd_mc", "_jumpH_mc.txt_mc"],
            Kind::EggDiscovery => &["", "_ed_mc", "_ed_mc.bkgd_mc", "_ed_mc.circleI_mc", "_ed_mc.circleE_mc", "_ed_mc.sepU_mc", "_ed_mc.sepD_mc", "_ed_mc.txt_mc", "_ed_mc.icon_mc", "_ed_mc.iconBkgd_mc"],
            Kind::ChainGauge => &[
                "",
                "_chain_mc.gauge_mc",
                "_chain_mc.circle_mc",
                "_chain_mc.skull_mc",
                "_chain_mc.chainsBkgd_mc",
                "_chain_mc.cd_mc._cd_mc",
                "_chain_mc.cd_mc._cd_mc.bkgd_mc",
                "_chain_mc.cd_mc._cd_mc.txt_mc",
                "_chain_mc.cd_mc._cd_mc.txt_mc.t0",
                "_chain_mc.cd_mc._cd_mc.txt_mc.t1",
                "_broken_mc._waveInf_mc",
                "_broken_mc._waveInf_mc.txt_mc",
                "_broken_mc._waveInf_mc.bkgd_mc",
                "_broken_mc._waveInf_mc.sepU_mc",
                "_broken_mc._waveInf_mc.sepD_mc",
            ],
            Kind::Clues | Kind::MMTarget => &[""],
            Kind::EquipmentUnlocked => &["", "_eq_mc", "_eq_mc.sepU_mc"],
        }
    }
}

type Later = Box<dyn FnOnce(&mut FlashClip, &mut Elem) + Send + Sync>;

/// An element of the HUD: what it keeps (the class's members).
#[derive(Component)]
struct Elem {
    kind: Kind,
    /// the instances' properties as placed or last saved (`_props`), by path
    saved: HashMap<String, Props>,
    /// its intervals: seconds to go, what then
    later: Vec<(f32, Later)>,
    opened: bool,
    /// what it shows (`_scoreValue`, `_curValue`, `_enemiesCount`...)
    value: i64,
    max: i64,
    /// the count of enemies it began with (`_enemiesCount_init`); the skulls' width
    init: i64,
    skull: f32,
    critical: bool,
    digits: Vec<char>,
    /// the round shown is the last (`_bInfiniteWave`)
    final_wave: bool,
    /// a round's results: the way on chosen (on, or the end), the one chosen, taking input
    choices: Vec<bool>,
    labels: Vec<String>,
    sel: usize,
    taking: bool,
    /// its backdrop's width as made (`_defBkgdW`); the kill chain's gauge, and its "Chain
    /// broken", shown (`_bGaugeOpened`, `_broken_mc._bOpened`)
    w0: f32,
    gauge_open: bool,
    title_open: bool,
}

impl Elem {
    fn new(kind: Kind) -> Elem {
        Elem {
            kind,
            saved: HashMap::new(),
            later: Vec::new(),
            opened: false,
            value: 0,
            max: 0,
            init: 0,
            skull: 30.0,
            critical: false,
            digits: Vec::new(),
            final_wave: false,
            choices: Vec::new(),
            labels: Vec::new(),
            sel: 0,
            taking: false,
            w0: 0.0,
            gauge_open: false,
            title_open: false,
        }
    }

    fn saved(&self, path: &str) -> Props {
        self.saved.get(path).copied().unwrap_or(Props { x: 0.0, y: 0.0, xscale: 1.0, yscale: 1.0, rotation: 0.0, alpha: 1.0 })
    }

    /// `SaveProperties`: as it is now.
    fn save(&mut self, fc: &FlashClip, path: &str) {
        if let Some(p) = fc.props(path) {
            self.saved.insert(path.to_string(), p);
        }
    }

    fn after(&mut self, secs: f32, f: impl FnOnce(&mut FlashClip, &mut Elem) + Send + Sync + 'static) {
        self.later.push((secs, Box::new(f)));
    }

    /// `ClearIntervals`
    fn clear(&mut self) {
        self.later.clear();
    }
}

fn to() -> PropsTo {
    PropsTo::default()
}

/// An instance shown from some properties, eased back to its saved ones (the classes'
/// usual entrance: `_alpha = 0; _xscale = ...; tweenTo(t, _props, ease)`).
fn pop_in(fc: &mut FlashClip, el: &Elem, path: &str, from: PropsTo, secs: f32, ease: Ease) {
    if !path.is_empty() {
        fc.set_visible(path, true);
    }
    fc.tween_end(path, false);
    fc.set(path, from);
    fc.tween(path, el.saved(path).into(), secs, ease);
}

/// An instance eased to some properties from where it is.
fn ease_to(fc: &mut FlashClip, path: &str, target: PropsTo, secs: f32, ease: Ease) {
    fc.tween_end(path, false);
    fc.tween(path, target, secs, ease);
}

/// `_width = w` (its scale to make it so).
fn set_width(fc: &mut FlashClip, path: &str, w: f32) {
    if let (Some(size), Some(p)) = (fc.size(path), fc.props(path)) {
        if size.x > 1e-3 && p.xscale.abs() > 1e-6 {
            let natural = size.x / p.xscale.abs();
            fc.set(path, to().xscale(w / natural));
        }
    }
}

/// The same text in a field and its shadows (`txt`, `txtShad0`...).
fn set_texts(fc: &mut FlashClip, paths: &[String], text: &str) {
    for p in paths {
        fc.set_text(p, text.to_string());
    }
}

fn hud_sound(sfx: &mut MessageWriter<crate::audio::PostEvent>, name: &str) {
    // (the HUD tweak's sound theme: the event of each name)
    let event = match name {
        "H_DLC05_EquipmentUnlocked" => "UI_H_DLC05_Eq_Unlocked".to_string(),
        n => format!("UI_{n}"),
    };
    sfx.write(crate::audio::PostEvent::named(&event, None));
}

/// What the HUD last showed of the run.
#[derive(Default)]
struct Shown {
    items: BTreeMap<String, HudItem>,
    /// points scored since the score last cashed in, and when it does
    pending: i64,
    cash_in: Option<f32>,
    /// the count before a start: seconds since, whether "GO!" ends it, the number shown
    count: Option<(f32, bool, i64)>,
    timer: bool,
    countdown: Option<i64>,
    /// a round title shown: seconds until it goes
    wave_left: Option<f32>,
    started: bool,
    /// the drop's height shown (metres), and how long it stays once landed
    jump: Option<i64>,
    jump_hold: f32,
    /// the kill chain: its time when last seen, the seconds shown
    chain: Option<f32>,
    chain_secs: i64,
    /// the clues found (in order), the mystery foe's reveal to come, shown, gone
    clues: Vec<String>,
    reveal: Option<f32>,
    foe_shown: bool,
    foe_gone: bool,
}

#[derive(Component)]
struct HudRoot;

#[allow(clippy::too_many_arguments)]
fn drive(
    mut commands: Commands,
    mut ch: ResMut<Challenge>,
    roots: Query<(Entity, &mut Node, &mut Visibility), With<HudRoot>>,
    mut elems: Elems,
    window: Query<&Window>,
    mut timelines: ResMut<MovieTimelines>,
    data: Res<crate::gamedata::Data>,
    (time, real): (Res<Time>, Res<Time<Real>>),
    mut sfx: MessageWriter<crate::audio::PostEvent>,
    mut shown: Local<Shown>,
    (player, stats, level): (Query<(&crate::player::Player, &Transform)>, Res<crate::gameplay::PlayerStats>, Option<Res<crate::level::LevelInfo>>),
    (mut ui, mut images): (ResMut<crate::ui_images::UiImages>, ResMut<Assets<Image>>),
) {
    if !ch.active() {
        if !roots.is_empty() {
            *shown = Shown::default();
        }
        for (e, ..) in &roots {
            commands.entity(e).despawn();
        }
        return;
    }
    let Ok(w) = window.single() else { return };
    let s = (w.width() / 1280.0).min(w.height() / 720.0).max(0.01);
    let off = (Vec2::new(w.width(), w.height()) - Vec2::new(1280.0, 720.0) * s) * 0.5;
    if roots.is_empty() {
        let Some(tl) = timelines.get(MOVIE) else { return };
        *shown = Shown::default();
        let root = commands
            .spawn((HudRoot, Node { position_type: PositionType::Absolute, left: Val::Px(off.x), top: Val::Px(off.y), ..default() }, GlobalZIndex(38), Pickable::IGNORE, DespawnOnExit(GameState::InGame)))
            .id();
        for kind in KINDS {
            let (symbol, h, v) = kind.symbol();
            let clip = Clip::export(&tl, symbol).unwrap_or_else(Clip::empty);
            let mut fc = FlashClip::new(MOVIE, tl.clone(), clip).with_texts();
            // (the movie's time is the real one: its intervals and tweens go on through a held
            // game, a round's results, Bend Time)
            fc.real_time = true;
            fc.scale = s;
            let at = Vec2::new([SAFE_MIN.x, 640.0, SAFE_MAX.x][h as usize], [SAFE_MIN.y, 360.0, SAFE_MAX.y][v as usize]);
            fc.m = [1.0, 0.0, 0.0, 1.0, at.x, at.y];
            let mut el = Elem::new(kind);
            init(&mut fc, &mut el, &data);
            for p in kind.saved() {
                el.save(&fc, p);
            }
            // (`DLC05_H_BaseElement`: hidden until opened)
            fc.set_visible("", false);
            commands.spawn((el, fc, Node { position_type: PositionType::Absolute, ..default() }, Pickable::IGNORE, ChildOf(root)));
        }
        return;
    }
    // (gone with the run: the results take the screen)
    let want = if ch.ended.is_some() { Visibility::Hidden } else { Visibility::Inherited };
    for (_, mut n, mut v) in roots {
        if n.left != Val::Px(off.x) || n.top != Val::Px(off.y) {
            n.left = Val::Px(off.x);
            n.top = Val::Px(off.y);
        }
        if *v != want {
            *v = want;
        }
    }
    let dt = time.delta_secs();
    // the elements' intervals
    for (mut el, mut fc) in &mut elems {
        if fc.scale != s {
            fc.scale = s;
        }
        let el = &mut *el;
        if el.later.is_empty() {
            continue;
        }
        let dt = if fc.real_time { real.delta_secs() } else { dt };
        for l in el.later.iter_mut() {
            l.0 -= dt;
        }
        let (due, rest): (Vec<_>, Vec<_>) = std::mem::take(&mut el.later).into_iter().partition(|l| l.0 <= 0.0);
        el.later = rest;
        for (_, f) in due {
            f(&mut fc, el);
        }
    }
    let ended = ch.ended.is_some();
    // what the run asked for
    for ev in std::mem::take(&mut ch.hud_events) {
        match ev {
            HudEvent::Scored(points) => {
                shown.pending += points;
                shown.cash_in = Some(CASH_AFTER);
                if let Some((mut el, mut fc)) = get(&mut elems, Kind::Score) {
                    score_update(&mut fc, &mut el, shown.pending, ch.multiplier.max(1.0));
                }
            }
            HudEvent::Wave { number, text } => {
                if let Some((mut el, mut fc)) = get(&mut elems, Kind::Wave) {
                    let (idx, txt) = match (number, text) {
                        (_, Some(t)) => (-1, Some(t)),
                        (Some(n), None) => (n as i64, None),
                        (None, None) => (-1, Some(data.text(H_TEXTS, "t_Wave_inf"))),
                    };
                    let sound = show_wave(&mut fc, &mut el, idx, txt.as_deref(), &data);
                    hud_sound(&mut sfx, sound);
                    shown.wave_left = Some(0.5 + WAVE_SHOWN);
                }
            }
            HudEvent::EggFound { found, max } => {
                if let Some((mut el, mut fc)) = get(&mut elems, Kind::EggDiscovery) {
                    egg_update(&mut fc, &mut el, found.max(1) as i64, max as i64, &data);
                    el.after(ITEM_SHOWN, egg_close);
                }
                hud_sound(&mut sfx, "H_DLC05_EggFound");
            }
            HudEvent::EquipmentUnlock(list) => {
                let items = unlocked(&list, &data, &mut ui, &mut images);
                if !items.is_empty() {
                    if let Some((mut el, mut fc)) = get(&mut elems, Kind::EquipmentUnlocked) {
                        eq_show(&mut fc, &mut el, &items, &data);
                    }
                    hud_sound(&mut sfx, "H_DLC05_EquipmentUnlocked");
                }
            }
            HudEvent::Trick(name) => {
                if let Some((mut el, mut fc)) = get(&mut elems, Kind::Tricks) {
                    trick_show(&mut fc, &mut el, &name);
                }
            }
            HudEvent::PhaseResults { name, success, possible, goal, kills, bonus, last } => {
                if let Some((mut el, mut fc)) = get(&mut elems, Kind::PhaseResults) {
                    phase_show(&mut fc, &mut el, &data, &name, success, possible, goal, kills, bonus, last);
                }
                hud_sound(&mut sfx, if success { "H_DLC05_PhaseSuccess" } else { "H_DLC05_PhaseFail" });
            }
            HudEvent::CountdownStart { go } => {
                if let Some((mut el, mut fc)) = get(&mut elems, Kind::CountdownStart) {
                    countdown_start(&mut fc, &mut el, 3, &data);
                    hud_sound(&mut sfx, "H_DLC05_CDStart_Number");
                    shown.count = Some((0.0, go, 3));
                }
            }
        }
    }
    // the score cashes in once the scoring stops
    if let Some(t) = shown.cash_in.as_mut() {
        *t -= dt;
        if *t <= 0.0 {
            shown.cash_in = None;
            shown.pending = 0;
            if let Some((mut el, mut fc)) = get(&mut elems, Kind::Score) {
                score_cash(&mut fc, &mut el, ch.score);
            }
        }
    }
    // a round title goes after a while
    if let Some(t) = shown.wave_left.as_mut() {
        *t -= dt;
        if *t <= 0.0 {
            shown.wave_left = None;
            if let Some((mut el, mut fc)) = get(&mut elems, Kind::Wave) {
                hide_wave(&mut fc, &mut el);
            }
        }
    }
    // the count before a start: a number a second, then GO!
    if let Some((t, go, n)) = shown.count.as_mut() {
        *t += dt;
        let want = 3 - t.floor() as i64;
        let over = *t >= 3.0 + if *go { 0.8 } else { 0.0 };
        if over {
            shown.count = None;
            if let Some((mut el, mut fc)) = get(&mut elems, Kind::CountdownStart) {
                countdown_complete(&mut fc, &mut el);
            }
        } else if want != *n && (want > 0 || *go) {
            *n = want;
            if let Some((mut el, mut fc)) = get(&mut elems, Kind::CountdownStart) {
                countdown_number(&mut fc, &mut el, want.max(0), &data);
            }
            hud_sound(&mut sfx, if want > 0 { "H_DLC05_CDStart_Number" } else { "H_DLC05_CDStart_GO" });
        }
    }
    // the timers: the default one's time, a countdown's seconds
    let default_timer = ch.timer_of("DDHT_DefaultTimer");
    let show_timer = default_timer.is_some() && !ended;
    if show_timer != shown.timer {
        shown.timer = show_timer;
        if let Some((_, mut fc)) = get(&mut elems, Kind::Timer) {
            fc.set_visible("", show_timer);
        }
    }
    if let (Some(v), Some((_, mut fc))) = (default_timer, get(&mut elems, Kind::Timer)) {
        timer_show(&mut fc, v);
    }
    let countdown = ch.timer_of("DDHT_CountdownTimer").filter(|_| !ended).map(|v| v.max(0.0).ceil() as i64);
    if countdown != shown.countdown {
        if let Some((mut el, mut fc)) = get(&mut elems, Kind::Countdown) {
            match (shown.countdown, countdown) {
                (None, Some(v)) => countdown_open(&mut fc, &mut el, v),
                (Some(_), Some(v)) => {
                    countdown_value(&mut fc, &mut el, v);
                    hud_sound(&mut sfx, "H_DLC05_CD_Number");
                }
                (_, None) => countdown_close(&mut fc, &mut el),
            }
        }
        shown.countdown = countdown;
    }
    // the counters, as the scripts and the run have them
    let items = ch.items.clone();
    for (name, it) in &items {
        let was = shown.items.get(name).cloned().unwrap_or_default();
        if was.shown == it.shown && was.value == it.value && was.max == it.max {
            continue;
        }
        let kind = match name.as_str() {
            "DDHI_Kills" => Kind::Kills,
            "DDHI_BatteriesDestroyed" => Kind::Tanks,
            "DDHI_CoinCount" => Kind::Coins,
            "DDHI_EggCount" => Kind::Eggs,
            "DDHI_BustedCount" => Kind::Busted,
            "DDHI_GateCount" => Kind::Gates,
            "DDHI_EnemiesLeft" => Kind::Enemies,
            _ => continue,
        };
        let Some((mut el, mut fc)) = get(&mut elems, kind) else { continue };
        let (el, fc) = (&mut *el, &mut *fc);
        let (v, max) = (it.value as i64, it.max.unwrap_or(0) as i64);
        if !it.shown {
            if was.shown {
                match kind {
                    Kind::Kills | Kind::Tanks => kills_close(fc, el),
                    Kind::Coins | Kind::Eggs | Kind::Busted => count_close(fc, el),
                    Kind::Gates => gates_close(fc, el),
                    Kind::Enemies => enemies_close(fc, el),
                    _ => {}
                }
            }
        } else {
            match kind {
                // (`_KType_Enemy`, `_KType_OilTank`)
                Kind::Kills => kills_update(fc, el, v, 1),
                Kind::Tanks => kills_update(fc, el, v, 3),
                Kind::Coins | Kind::Eggs => {
                    if was.shown {
                        count_update(fc, el, v, max);
                    } else {
                        count_show(fc, el, v, max);
                    }
                }
                // (the chances left)
                Kind::Busted => busted_show(fc, el, (max - v).max(0), max),
                Kind::Gates => gates_update(fc, el, v),
                Kind::Enemies => {
                    if !was.shown || v > el.value {
                        enemies_add(fc, el, v);
                    } else if v < el.value {
                        enemies_remove(fc, el, v);
                    }
                }
                _ => {}
            }
        }
        shown.items.insert(name.clone(), it.clone());
    }
    // the kill chain: its gauge and seconds while it runs, "Chain broken" when it runs out
    let chain = ch.timer_state("DDHT_KillChainTimer").filter(|_| !ended);
    if let Some((mut el, mut fc)) = get(&mut elems, Kind::ChainGauge) {
        let (el, fc) = (&mut *el, &mut *fc);
        match chain {
            Some((v, start, true)) => {
                let gauge = ((v / start).clamp(0.0, 1.0) * 100.0).round() as i64;
                let secs = (v.ceil() as i64 - 1).max(0);
                if shown.chain.is_none() {
                    chain_show(fc, el, gauge, secs);
                    hud_sound(&mut sfx, "H_DLC05_CD_Number");
                } else {
                    chain_gauge(fc, gauge);
                    if secs != shown.chain_secs {
                        countdown_value_at(fc, el, CHAIN_CD, secs, CHAIN_CRITICAL);
                        hud_sound(&mut sfx, "H_DLC05_CD_Number");
                    }
                }
                shown.chain = Some(v);
                shown.chain_secs = secs;
            }
            Some((v, _, false)) if shown.chain.is_some() => {
                shown.chain = None;
                if v <= 1e-3 {
                    chain_broken(fc, el, &data);
                    el.after(2.0, chain_close);
                } else {
                    chain_close(fc, el);
                }
            }
            None if shown.chain.is_some() => {
                shown.chain = None;
                chain_close(fc, el);
            }
            _ => {}
        }
    }
    // a drop's height as Corvo falls (where drops score: Drop Attack), kept a moment once down
    let set = crate::dlc05score::set_of(&ch, level.as_deref());
    let drops = set.is_some_and(|s| s.rules.iter().any(|r| r.class.ends_with("_DropAssassination")));
    if drops && !ended {
        let fall = player.iter().next().filter(|(p, _)| !p.grounded).map(|(p, t)| p.air_peak - t.translation.y);
        match fall {
            Some(h) if h >= MIN_DROP => {
                let m = h.round() as i64;
                if let Some((mut el, mut fc)) = get(&mut elems, Kind::JumpHeight) {
                    match shown.jump {
                        None => jump_show(&mut fc, &mut el, m),
                        Some(was) if was != m => jump_set(&mut fc, &mut el, m),
                        _ => {}
                    }
                }
                shown.jump = Some(m);
                shown.jump_hold = 1.0;
            }
            _ if shown.jump.is_some() => {
                shown.jump_hold -= dt;
                if shown.jump_hold <= 0.0 {
                    shown.jump = None;
                    if let Some((mut el, mut fc)) = get(&mut elems, Kind::JumpHeight) {
                        jump_hide(&mut fc, &mut el);
                    }
                }
            }
            _ => {}
        }
    }
    // the mystery man's clues as they're found; all found, its portrait a while after
    if ch.foe.is_some() && !shown.foe_gone {
        let wanted = set.and_then(|s| s.rules.iter().find_map(|r| r.params.get("m_iNumClues").copied())).unwrap_or(4.0).max(1.0) as usize;
        let found: Vec<String> = stats.notes.iter().filter(|k| k.starts_with("Clues.") && !k.contains("PickupSample") && !shown.clues.contains(k)).cloned().collect();
        for k in found {
            let text = data.0.abstract_items.get(&k).map(|a| a.1.clone()).unwrap_or_default();
            shown.clues.push(k);
            if !shown.foe_shown && shown.reveal.is_none() {
                if let Some((mut el, mut fc)) = get(&mut elems, Kind::Clues) {
                    clue_add(&mut fc, &mut el, &text);
                }
                hud_sound(&mut sfx, "H_DLC05_NewClue");
                if shown.clues.len() >= wanted {
                    shown.reveal = Some(FOE_REVEAL);
                }
            }
        }
    }
    if let Some(t) = shown.reveal.as_mut() {
        *t -= dt;
        if *t <= 0.0 {
            shown.reveal = None;
            shown.foe_shown = true;
            if let Some((mut el, mut fc)) = get(&mut elems, Kind::Clues) {
                clues_hide(&mut fc, &mut el);
            }
            let stem = ch.foe.as_deref().map(|p| p.rsplit('.').next().unwrap_or(p).to_string()).unwrap_or_default();
            let img = ui.file(&mut images, "dlc05", &stem);
            let blue = ch.foe_blue;
            if let Some((mut el, _)) = get(&mut elems, Kind::MMTarget) {
                el.after(0.25, move |fc, el| mm_show(fc, el, img, blue));
            }
            hud_sound(&mut sfx, "H_DLC05_TargetIdentified");
        }
    }
    // (the foe down: its portrait, or the clues, gone)
    if ch.foe_down && !shown.foe_gone {
        shown.foe_gone = true;
        shown.reveal = None;
        if let Some((mut el, mut fc)) = get(&mut elems, Kind::MMTarget) {
            if shown.foe_shown {
                mm_hide(&mut fc, &mut el);
            } else {
                el.clear();
            }
        }
        if let Some((mut el, mut fc)) = get(&mut elems, Kind::Clues) {
            if el.opened {
                clues_hide(&mut fc, &mut el);
            }
        }
    }
    // the run over: the counters go
    if ended && !shown.started {
        shown.started = true;
        for (mut el, mut fc) in &mut elems {
            el.clear();
            fc.set_visible("", false);
        }
    }
}

type Elems<'w, 's> = Query<'w, 's, (&'static mut Elem, &'static mut FlashClip)>;

fn get<'a>(q: &'a mut Elems, k: Kind) -> Option<(Mut<'a, Elem>, Mut<'a, FlashClip>)> {
    q.iter_mut().find(|(el, _)| el.kind == k)
}

/// An element's class constructor.
fn init(fc: &mut FlashClip, el: &mut Elem, data: &crate::gamedata::Data) {
    match el.kind {
        Kind::Score => {
            for p in ["_score_mc", "_inc_mc", "_multiplier_mc"] {
                fc.set_visible(p, false);
            }
            fc.set_text("_score_mc.txt_mc.unit_txt", data.text("DisGFxMoviePlayerBase_Texts", "t_Pts"));
        }
        Kind::Timer => {
            // the digit fields (`InitTextFields`)
            for mc in TIMER_MCS {
                for (name, _) in TIMER_FIELDS {
                    fc.attach(mc, "Timer_timerTxt_txt", name, IDENTITY);
                    let t = format!("{mc}.{name}.txt");
                    fc.autosize.insert(t.clone(), 2);
                    fc.set_text(
                        &t,
                        match name {
                            "sep_sec" => "\u{201d}",
                            "sep_min" => "\u{2019}",
                            _ => "0",
                        },
                    );
                }
            }
            timer_layout(fc);
        }
        Kind::Kills | Kind::Tanks => kills_display(fc, 0),
        Kind::Enemies => {
            fc.create_empty("", "_elc_mc", [1.0, 0.0, 0.0, 1.0, 0.0, 20.0]);
            fc.create_empty("_elc_mc", "_ic_mc", IDENTITY);
            // (a skull's width)
            fc.attach("_elc_mc._ic_mc", "ELC_skullIc", "probe", IDENTITY);
            el.skull = fc.size("_elc_mc._ic_mc.probe").map_or(30.0, |s| s.x);
            fc.remove("_elc_mc._ic_mc.probe");
        }
        Kind::Wave => {
            for mc in ["_wave_mc", "_waveInf_mc"] {
                fc.set_visible(mc, false);
                fc.set(&format!("{mc}.bkgd_mc"), to().x(0.0));
            }
        }
        Kind::CountdownStart => {
            for p in ["number_mc", "bkgd_mc", "blade0_mc", "blade1_mc", "circle_mc", "circleDotLine_mc"] {
                fc.set_visible(&format!("_countdown_mc.{p}"), false);
            }
        }
        Kind::Countdown => {
            fc.goto_frame("_cd_mc.hGlass_mc", 1, false);
            fc.goto_frame("_cd_mc.bkgd_mc", 1, false);
            countdown_digit(fc, "_cd_mc.txt_mc.t0", '0', false);
            countdown_digit(fc, "_cd_mc.txt_mc.t1", '0', false);
        }
        Kind::JumpHeight => el.w0 = fc.size("_jumpH_mc.bkgd_mc").map_or(150.0, |s| s.x),
        Kind::ChainGauge => {
            // (its countdown's display; "Chain broken" a backdrop at full, hidden until shown)
            fc.goto_frame(&format!("{CHAIN_CD}.bkgd_mc"), 1, false);
            countdown_digit(fc, &format!("{CHAIN_CD}.txt_mc.t0"), '0', false);
            countdown_digit(fc, &format!("{CHAIN_CD}.txt_mc.t1"), '0', false);
            fc.set("_broken_mc._waveInf_mc.bkgd_mc", to().alpha(1.0));
            fc.set_visible("_broken_mc", false);
        }
        Kind::EquipmentUnlocked => {
            fc.set_visible("_eq_mc.bkgd_mc", false);
            fc.set_visible("_eq_mc.content_mc.txt_mc", false);
            el.w0 = fc.size("_eq_mc.bkgd_mc").map_or(400.0, |s| s.x);
            // (`SetPosition`: the backdrop out to the stage's right edge)
            fc.set("_eq_mc.bkgd_mc", to().x(1280.0 - SAFE_MAX.x));
        }
        _ => {}
    }
}

// ---- DLC05_H_Score

fn score_update(fc: &mut FlashClip, el: &mut Elem, inc: i64, mult: f32) {
    el.clear();
    // `UpdateIncrement`
    let s = el.saved("_inc_mc");
    pop_in(fc, el, "_inc_mc", to().alpha(0.0).y(s.y + 50.0), 0.2, Ease::BackInOut);
    fc.set_text("_inc_mc.txt_mc.txt", format!("+{inc}"));
    if mult > 1.0 {
        let s = el.saved("_multiplier_mc");
        pop_in(fc, el, "_multiplier_mc", to().alpha(0.0).scale(1.5).x(s.x).y(s.y), 0.2, Ease::StrongOut);
        fc.set_text("_multiplier_mc.txt_mc.txt", format!("x{}", (mult * 10.0).round() / 10.0));
    } else {
        score_hide_multiplier(fc, el);
    }
    if !el.opened {
        score_show(fc, el);
        el.opened = true;
        fc.set_visible("", true);
        ease_to(fc, "", el.saved("").into(), 0.2, Ease::StrongOut);
    }
}

fn score_hide_multiplier(fc: &mut FlashClip, el: &Elem) {
    let s = el.saved("_multiplier_mc");
    ease_to(fc, "_multiplier_mc", to().x(s.x - 50.0).scale(1.2).alpha(0.0), 0.2, Ease::StrongOut);
}

/// `UpdateScore`: the total, popping in.
fn score_show(fc: &mut FlashClip, el: &Elem) {
    pop_in(fc, el, "_score_mc", to().alpha(0.0).scale(1.5), 0.2, Ease::StrongOut);
    fc.set_text("_score_mc.txt_mc.txt", el.value.to_string());
}

fn score_cash(fc: &mut FlashClip, el: &mut Elem, total: i64) {
    el.clear();
    el.value = total;
    let s = el.saved("_inc_mc");
    ease_to(fc, "_inc_mc", to().y(s.y + 50.0).alpha(0.0), 0.2, Ease::StrongOut);
    score_hide_multiplier(fc, el);
    score_show(fc, el);
    if !el.opened {
        el.opened = true;
        fc.set_visible("", true);
        ease_to(fc, "", el.saved("").into(), 0.2, Ease::StrongOut);
    }
    el.after(SCORE_SHOWN, |fc, el| {
        // `HideScore`
        el.opened = false;
        ease_to(fc, "", to().alpha(0.0), 0.2, Ease::StrongOut);
        let s = el.saved("_score_mc");
        ease_to(fc, "_score_mc", to().rotation(s.rotation - 10.0).x(s.x - 50.0).alpha(0.0), 0.2, Ease::StrongOut);
    });
}

// ---- DLC05_H_Timer

/// The digit fields laid right to left (`InitTextFields`: each `_offsetX` after the one
/// before, their widths their text's).
fn timer_layout(fc: &mut FlashClip) {
    for mc in TIMER_MCS {
        let mut prev = 0.0;
        for (i, (name, gap)) in TIMER_FIELDS.iter().enumerate() {
            let path = format!("{mc}.{name}");
            let w = fc.text_width(&format!("{path}.txt")).unwrap_or(16.0);
            let x = if i == 0 { -w } else { prev - gap - w };
            if fc.props(&path).is_some_and(|p| (p.x - x).abs() > 0.01) {
                fc.set(&path, to().x(x));
            }
            prev = x;
        }
    }
}

fn timer_show(fc: &mut FlashClip, secs: f32) {
    let t = secs.max(0.0);
    let total_hun = (t * 100.0).floor() as u64;
    let (min, sec, hun) = (total_hun / 6000, (total_hun / 100) % 60, total_hun % 100);
    let digits = [("hun_0", hun % 10), ("hun_1", hun / 10), ("sec_0", sec % 10), ("sec_1", sec / 10), ("min_0", min % 10), ("min_1", (min / 10) % 10)];
    for mc in TIMER_MCS {
        for (name, d) in digits {
            fc.set_text(&format!("{mc}.{name}.txt"), d.to_string());
        }
    }
    timer_layout(fc);
}

// ---- DLC05_H_KillsCount, DLC05_H_TanksCount

fn kills_display(fc: &mut FlashClip, n: i64) {
    for p in ["_kills_mc.txt_mc.txt", "_kills_mc.txtShad0_mc.txt", "_kills_mc.txtShad1_mc.txt", "_kills_mc.txtShad2_mc.txt"] {
        fc.set_text(p, n.to_string());
        fc.autosize.insert(p.to_string(), 1);
    }
}

fn kills_update(fc: &mut FlashClip, el: &mut Elem, n: i64, kind: usize) {
    el.clear();
    fc.goto_frame("_kills_mc.ic_mc.ic_mc", kind, false);
    kills_display(fc, n);
    // `PlayUpdateAnim`
    fc.tween_end("_kills_mc.txt_mc", false);
    fc.set("_kills_mc.txt_mc", to().alpha(0.0).scale(1.9).rotation(-5.0));
    fc.tween("_kills_mc.txt_mc", to().rotation(0.0).scale(1.0).alpha(1.0), 0.2, Ease::BackInOut);
    fc.goto("_kills_mc.ic_mc", "update", true);
    if !el.opened {
        el.opened = true;
        fc.set_visible("", true);
        let s = el.saved("");
        pop_in(fc, el, "", to().rotation(-5.0).x(s.x + 100.0).scale(1.5).alpha(0.0), 0.25, Ease::StrongOut);
    }
    el.value = n;
}

fn kills_close(fc: &mut FlashClip, el: &mut Elem) {
    el.clear();
    el.opened = false;
    let s = el.saved("");
    ease_to(fc, "", to().rotation(5.0).x(s.x + 50.0).alpha(0.0), 0.25, Ease::StrongOut);
    el.after(0.25, |fc, _| fc.set_visible("", false));
}

// ---- DLC05_H_BaseCount: coins, eggs; DLC05_H_BustedCount

fn count_text(fc: &mut FlashClip, cur: i64, max: i64) {
    fc.set_text("_mc.txt_mc.txt", format!("<font color=\"{WHITE}\">{cur}</font><font color=\"{GREY}\">/{max}</font>"));
}

fn count_show(fc: &mut FlashClip, el: &mut Elem, cur: i64, max: i64) {
    el.value = cur;
    el.max = max;
    count_text(fc, cur, max);
    // `SetBkgd` (`_bkgdAlpha`), `Open`
    fc.set("_mc.bkgd_mc", to().alpha(0.8));
    el.clear();
    el.opened = true;
    fc.set_visible("", true);
    let s = el.saved("_mc");
    pop_in(fc, el, "_mc", to().x(s.x + 50.0), 0.25, Ease::StrongOut);
}

fn count_update(fc: &mut FlashClip, el: &mut Elem, cur: i64, max: i64) {
    count_text(fc, cur, max);
    let s = el.saved("_mc.txt_mc");
    pop_in(fc, el, "_mc.txt_mc", to().alpha(0.0).x(s.x + 50.0), 0.25, Ease::StrongOut);
    pop_in(fc, el, "_mc.ic_mc", to().alpha(0.0).xscale(2.0).yscale(2.5), 0.25, Ease::BackInOut);
    el.value = cur;
    el.max = max;
}

fn count_close(fc: &mut FlashClip, el: &mut Elem) {
    el.clear();
    el.opened = false;
    let s = el.saved("_mc");
    ease_to(fc, "_mc", to().x(s.x + 50.0).alpha(0.0), 0.25, Ease::StrongOut);
}

/// The chances: an icon each (`BC_ic`, laid from the right, `MovieClipCreator` with
/// `_offsetX` -7.5), the one lost playing `busted`.
fn busted_show(fc: &mut FlashClip, el: &mut Elem, cur: i64, max: i64) {
    if !el.opened || max != el.max {
        fc.remove("_mc._ic_mc");
        fc.create_empty("_mc", "_ic_mc", IDENTITY);
        let mut w = 0.0;
        for i in 0..max {
            fc.attach("_mc._ic_mc", "BC_ic", &format!("ic{i}"), IDENTITY);
            let iw = fc.size(&format!("_mc._ic_mc.ic{i}")).map_or(30.0, |s| s.x);
            w = iw;
            fc.set(&format!("_mc._ic_mc.ic{i}"), to().x((iw * 0.5 + i as f32 * (iw - 7.5)).round()));
        }
        let total = max as f32 * (w - 7.5) + 7.5;
        fc.set("_mc._ic_mc", to().x(-total));
        count_show(fc, el, cur, max);
        // (`_curValue` read before: the lost ones already lost)
        for i in cur..max {
            fc.goto_frame(&format!("_mc._ic_mc.ic{i}"), 21, false);
        }
        fc.set_text("_mc.txt_mc.txt", String::new());
    } else if cur < el.value {
        for i in cur..el.value {
            fc.goto(&format!("_mc._ic_mc.ic{i}"), "busted", true);
        }
    }
    el.value = cur;
    el.max = max;
}

// ---- DLC05_H_CPCount: the gates

fn gates_update(fc: &mut FlashClip, el: &mut Elem, n: i64) {
    el.value = n;
    for p in ["_cp_mc.txt_mc.txt", "_cp_mc.txt_mc.txtShad0", "_cp_mc.txt_mc.txtShad1"] {
        fc.set_text(p, n.to_string());
    }
    if el.opened {
        gates_txt(fc, el);
        gates_circle(fc, el);
        return;
    }
    el.clear();
    el.opened = true;
    fc.set_visible("", true);
    ease_to(fc, "_cp_mc", el.saved("_cp_mc").into(), 0.25, Ease::StrongOut);
    for p in ["circleExt_mc", "circleInt_mc", "bkgd_mc", "splash_mc", "txt_mc"] {
        fc.set_visible(&format!("_cp_mc.{p}"), false);
    }
    el.after(0.06, gates_circle);
    el.after(0.001, gates_txt);
    el.after(0.15, |fc, el| pop_in(fc, el, "_cp_mc.bkgd_mc", to().alpha(0.0).xscale(0.1), 0.25, Ease::BackOut));
    el.after(0.11, |fc, _| {
        fc.set_visible("_cp_mc.splash_mc", true);
        fc.goto_frame("_cp_mc.splash_mc", 1, true);
    });
}

fn gates_circle(fc: &mut FlashClip, el: &mut Elem) {
    pop_in(fc, el, "_cp_mc.circleExt_mc", to().alpha(0.0).xscale(3.0).yscale(2.0).rotation(-50.0), 0.35, Ease::BackInOut);
    pop_in(fc, el, "_cp_mc.circleInt_mc", to().alpha(0.0).scale(0.2), 0.25, Ease::StrongOut);
    el.after(0.35, |fc, _| fc.spin("_cp_mc.circleExt_mc", 20.0, false));
}

fn gates_txt(fc: &mut FlashClip, el: &mut Elem) {
    pop_in(fc, el, "_cp_mc.txt_mc", to().alpha(0.0).xscale(2.0).yscale(3.5), 0.25, Ease::BackInOut);
}

fn gates_close(fc: &mut FlashClip, el: &mut Elem) {
    el.clear();
    el.opened = false;
    ease_to(fc, "_cp_mc", to().alpha(0.0), 0.25, Ease::StrongOut);
    el.after(0.25, |fc, _| {
        fc.stop_spin("_cp_mc.circleExt_mc");
        fc.set_visible("", false);
    });
}

// ---- DLC05_H_EnemiesLeftCount: a skull an enemy

fn skull_x(el: &Elem, i: i64) -> f32 {
    el.skull * 0.5 + i as f32 * (el.skull + SKULL_GAP)
}

fn skulls_width(el: &Elem, n: i64) -> f32 {
    (n as f32 * (el.skull + SKULL_GAP) - SKULL_GAP).max(0.0)
}

fn enemies_add(fc: &mut FlashClip, el: &mut Elem, n: i64) {
    el.clear();
    el.init = n;
    el.value = n;
    fc.remove("_elc_mc._ic_mc");
    fc.create_empty("_elc_mc", "_ic_mc", IDENTITY);
    for i in 0..n {
        let p = format!("_elc_mc._ic_mc.ic{i}");
        fc.attach("_elc_mc._ic_mc", "ELC_skullIc", &format!("ic{i}"), [1.0, 0.0, 0.0, 1.0, skull_x(el, i), 0.0]);
        fc.set_visible(&p, false);
        el.save(fc, &p);
    }
    fc.set("_elc_mc._ic_mc", to().x(-0.5 * skulls_width(el, n)));
    // (one after another, `PlayAddAnim`)
    for i in 0..n {
        el.after(0.0225 * i as f32 + 0.001, move |fc, el| {
            let p = format!("_elc_mc._ic_mc.ic{i}");
            let s = el.saved(&p);
            pop_in(fc, el, &p, to().scale(2.5).alpha(0.0).y(s.y - 50.0), 0.25, Ease::BackInOut);
            fc.goto(&p, "open", true);
        });
    }
    el.opened = true;
    fc.set_visible("", true);
    fc.set("_elc_mc", to().alpha(0.0));
    ease_to(fc, "_elc_mc", to().alpha(1.0), 0.2, Ease::StrongOut);
}

fn enemies_remove(fc: &mut FlashClip, el: &mut Elem, n: i64) {
    let was = el.value;
    for (k, i) in (n..was).rev().enumerate() {
        el.after(0.02 * k as f32 + 0.001, move |fc, el| {
            let p = format!("_elc_mc._ic_mc.ic{i}");
            fc.goto(&p, "kill", true);
            let s = el.saved(&p);
            ease_to(fc, &p, to().alpha(0.0).y(s.y + 80.0).x(s.x), 0.2, Ease::StrongOut);
        });
    }
    el.value = n;
    let w = skulls_width(el, n);
    let x = -0.5 * w - skull_x(el, 0) + el.skull * 0.5;
    ease_to(fc, "_elc_mc._ic_mc", to().x(x), 0.2, Ease::BackOut);
}

fn enemies_close(fc: &mut FlashClip, el: &mut Elem) {
    el.clear();
    el.opened = false;
    ease_to(fc, "_elc_mc", to().alpha(0.0), 0.2, Ease::StrongOut);
}

// ---- DLC05_H_Wave

/// A round's title: its number (`ShowWave(n)`), or a title (`ShowWave(-1, text)`). The
/// sound it makes.
fn show_wave(fc: &mut FlashClip, el: &mut Elem, idx: i64, text: Option<&str>, data: &crate::gamedata::Data) -> &'static str {
    el.clear();
    if idx == -1 {
        show_title(fc, el, "", text.unwrap_or(""));
        return "H_DLC05_FinalWave";
    }
    fc.set_visible("", true);
    el.final_wave = false;
    let n = idx.to_string();
    let mut paths = vec!["_wave_mc.number_mc.txt_mc.txt".to_string()];
    paths.extend((0..3).map(|i| format!("_wave_mc.number_mc.txtShad{i}_mc.txt")));
    set_texts(fc, &paths, &n);
    for p in &paths {
        fc.autosize.insert(p.clone(), 2);
    }
    let nw = fc.text_width(&paths[0]).unwrap_or(40.0);
    let word = data.text(H_TEXTS, "t_Wave").to_uppercase();
    let tpaths: Vec<String> = ["txt", "txtShad0", "txtShad1"].iter().map(|f| format!("_wave_mc.txt_mc.{f}")).collect();
    set_texts(fc, &tpaths, &word);
    for p in &tpaths {
        fc.autosize.insert(p.clone(), 1);
    }
    let tw = fc.text_width(&tpaths[0]).unwrap_or(120.0);
    let tx = 0.5 * (tw - nw - 5.0);
    fc.set("_wave_mc.txt_mc", to().x(tx).alpha(1.0));
    el.save(fc, "_wave_mc.txt_mc");
    let nx = tx + 0.5 * nw + 5.0;
    let ns = el.saved("_wave_mc.number_mc");
    fc.set("_wave_mc.number_mc", to().x(nx).alpha(1.0).scale(ns.xscale).rotation(ns.rotation));
    el.save(fc, "_wave_mc.number_mc");
    let cs = el.saved("_wave_mc.circle_mc");
    fc.set("_wave_mc.circle_mc", to().x(nx).alpha(1.0).scale(cs.xscale));
    el.save(fc, "_wave_mc.circle_mc");
    fc.set("_wave_mc.splash_mc", to().x(nx));
    let bw = fc.size("_wave_mc.bkgd_mc").map_or(400.0, |s| s.x);
    set_width(fc, "_wave_mc.bkgd_mc", bw.max(tw + nw + 110.0));
    let a = el.saved("_wave_mc.bkgd_mc").alpha;
    fc.set("_wave_mc.bkgd_mc", to().alpha(a));
    el.save(fc, "_wave_mc.bkgd_mc");
    fc.set("_wave_mc.sepU_mc", to().x(nx - 85.0).alpha(1.0));
    el.save(fc, "_wave_mc.sepU_mc");
    fc.set("_wave_mc.sepD_mc", to().x(nx - 50.0).alpha(1.0));
    el.save(fc, "_wave_mc.sepD_mc");
    fc.set_visible("_wave_mc", true);
    fc.set_visible("_waveInf_mc", false);
    for p in ["number_mc", "circle_mc", "splash_mc"] {
        fc.set_visible(&format!("_wave_mc.{p}"), false);
    }
    el.after(0.02, |fc, el| {
        pop_in(fc, el, "_wave_mc.number_mc", to().alpha(0.0).scale(2.5).rotation(-25.0), 0.2, Ease::BackInOut);
        fc.goto("_wave_mc.number_mc", "loop", true);
        fc.set_visible("_wave_mc.splash_mc", true);
        fc.goto("_wave_mc.splash_mc", "anim", true);
    });
    el.after(0.001, |fc, el| {
        pop_in(fc, el, "_wave_mc.circle_mc", to().alpha(0.0).scale(2.5).rotation(-25.0), 0.2, Ease::BackInOut);
        el.after(0.22, |fc, _| fc.spin("_wave_mc.circle_mc", 20.5, true));
    });
    open_parts(fc, el, "_wave_mc".to_string());
    "H_DLC05_Wave"
}

/// `ShowWave(-1, text)` of a title within (`pre`: `""`, or `_broken_mc.` for the kill chain's
/// "Chain broken"): the words on a backdrop as wide.
fn show_title(fc: &mut FlashClip, el: &mut Elem, pre: &str, text: &str) {
    fc.set_visible(pre.trim_end_matches('.'), true);
    el.final_wave = true;
    el.title_open = true;
    let mc = format!("{pre}_waveInf_mc");
    let t = text.to_uppercase();
    let paths: Vec<String> = ["txt", "txtShad0", "txtShad1"].iter().map(|f| format!("{mc}.txt_mc.{f}")).collect();
    set_texts(fc, &paths, &t);
    for p in &paths {
        fc.autosize.insert(p.clone(), 1);
    }
    let tw = fc.text_width(&paths[0]).unwrap_or(200.0);
    let tm = format!("{mc}.txt_mc");
    fc.set(&tm, to().x(0.5 * tw).alpha(1.0));
    el.save(fc, &tm);
    let bm = format!("{mc}.bkgd_mc");
    let bw = fc.size(&bm).map_or(400.0, |s| s.x);
    set_width(fc, &bm, bw.max(tw + 100.0));
    let a = el.saved(&bm).alpha;
    fc.set(&bm, to().alpha(a));
    el.save(fc, &bm);
    fc.set_visible(&format!("{pre}_wave_mc"), false);
    fc.set_visible(&mc, true);
    open_parts(fc, el, mc);
}

/// `OpenBkgd`, `OpenSep`, `OpenTxt` of a title clip.
fn open_parts(fc: &mut FlashClip, el: &mut Elem, mc: String) {
    for p in ["bkgd_mc", "sepU_mc", "sepD_mc", "txt_mc"] {
        fc.set_visible(&format!("{mc}.{p}"), false);
    }
    let m2 = mc.clone();
    let m3 = mc.clone();
    el.after(0.05, move |fc, el| {
        let p = format!("{mc}.bkgd_mc");
        let s = el.saved(&p);
        fc.set_visible(&p, true);
        fc.tween_end(&p, false);
        fc.set(&p, to().alpha(0.0).x(-250.0).xscale(s.xscale * 0.05));
        fc.tween(&p, s.into(), 0.35, Ease::StrongOut);
    });
    el.after(0.04, move |fc, el| {
        let u = format!("{m2}.sepU_mc");
        let s = el.saved(&u);
        pop_in(fc, el, &u, to().alpha(0.0).x(s.x + 250.0), 0.25, Ease::BackOut);
        let d = format!("{m2}.sepD_mc");
        let s = el.saved(&d);
        pop_in(fc, el, &d, to().alpha(0.0).x(s.x - 250.0), 0.25, Ease::BackOut);
    });
    el.after(0.05, move |fc, el| {
        let p = format!("{m3}.txt_mc");
        let s = el.saved(&p);
        pop_in(fc, el, &p, to().alpha(0.0).x(s.x + 50.0), 0.15, Ease::BackInOut);
    });
}

/// `HideWave`'s backdrop, rules and words.
fn close_parts(fc: &mut FlashClip, el: &Elem, mc: &str) {
    let b = format!("{mc}.bkgd_mc");
    let s = el.saved(&b);
    ease_to(fc, &b, to().xscale(s.xscale * 0.03).alpha(0.0), 0.35, Ease::StrongOut);
    let u = format!("{mc}.sepU_mc");
    let su = el.saved(&u);
    ease_to(fc, &u, to().x(su.x + 100.0).alpha(0.0), 0.25, Ease::StrongOut);
    let d = format!("{mc}.sepD_mc");
    let sd = el.saved(&d);
    ease_to(fc, &d, to().x(sd.x - 150.0).alpha(0.0), 0.25, Ease::StrongOut);
    let t = format!("{mc}.txt_mc");
    let st = el.saved(&t);
    ease_to(fc, &t, to().x(st.x + 50.0).alpha(0.0), 0.15, Ease::BackInOut);
}

fn hide_wave(fc: &mut FlashClip, el: &mut Elem) {
    el.clear();
    if el.final_wave {
        hide_title(fc, el, "");
        return;
    }
    close_parts(fc, el, "_wave_mc");
    fc.goto_frame("_wave_mc.number_mc", 1, false);
    ease_to(fc, "_wave_mc.number_mc", to().rotation(5.0).yscale(1.5).xscale(1.8).alpha(0.0), 0.2, Ease::BackInOut);
    ease_to(fc, "_wave_mc.circle_mc", to().scale(1.8).alpha(0.0), 0.2, Ease::BackInOut);
    fc.stop_spin("_wave_mc.circle_mc");
    fc.goto("_wave_mc.splash_mc", "close", true);
    el.after(0.5, |fc, _| fc.set_visible("", false));
}

/// A title's `HideWave`, then it is gone.
fn hide_title(fc: &mut FlashClip, el: &mut Elem, pre: &str) {
    el.title_open = false;
    close_parts(fc, el, &format!("{pre}_waveInf_mc"));
    let root = pre.trim_end_matches('.').to_string();
    el.after(0.5, move |fc, _| fc.set_visible(&root, false));
}

// ---- DLC05_H_CountdownStart: 3, 2, 1, GO!

const CS: &str = "_countdown_mc";

fn countdown_start(fc: &mut FlashClip, el: &mut Elem, n: i64, data: &crate::gamedata::Data) {
    el.clear();
    fc.set_visible("", true);
    el.after(0.001, |fc, el| {
        // `OpenCircles`
        for (p, rot, scale, secs, ease) in [("blade0_mc", -25.0, 0.1, 0.35, Ease::BackInOut), ("blade1_mc", -25.0, 0.1, 0.25, Ease::BackInOut), ("circle_mc", -25.0, 0.2, 0.35, Ease::BackInOut), ("circleDotLine_mc", 45.0, 2.5, 0.35, Ease::StrongOut)] {
            let path = format!("{CS}.{p}");
            pop_in(fc, el, &path, to().alpha(0.0).rotation(rot).scale(scale), secs, ease);
        }
        el.after(0.35, |fc, _| {
            fc.spin(&format!("{CS}.circle_mc"), 20.0, false);
            fc.spin(&format!("{CS}.circleDotLine_mc"), 15.0, true);
            fc.spin(&format!("{CS}.blade0_mc"), 20.0, true);
            fc.spin(&format!("{CS}.blade1_mc"), 12.5, false);
        });
    });
    el.after(0.07, |fc, el| {
        let p = format!("{CS}.bkgd_mc");
        pop_in(fc, el, &p, to().rotation(-5.0).scale(0.25).alpha(0.0), 0.25, Ease::StrongOut);
    });
    countdown_number(fc, el, n, data);
}

fn countdown_number(fc: &mut FlashClip, el: &mut Elem, n: i64, data: &crate::gamedata::Data) {
    let t = if n > 0 { n.to_string() } else { data.text(H_TEXTS, "t_GO") };
    let paths: Vec<String> = ["txt_mc", "txtShad0_mc", "txtShad1_mc"].iter().map(|m| format!("{CS}.number_mc.{m}.txt")).collect();
    set_texts(fc, &paths, &t);
    let p = format!("{CS}.number_mc");
    // (`GetRandomNumber`)
    let r = |a: f32, b: f32| a + (b - a) * rand::random::<f32>();
    pop_in(fc, el, &p, to().alpha(0.0).xscale(r(2.5, 2.8)).yscale(r(2.5, 2.8)).rotation(r(-25.0, 25.0)), 0.25, Ease::StrongOut);
    fc.goto(&p, "loop", true);
}

fn countdown_complete(fc: &mut FlashClip, el: &mut Elem) {
    el.clear();
    for p in ["circle_mc", "circleInt_mc", "circleDotLine_mc", "blade0_mc", "blade1_mc"] {
        fc.stop_spin(&format!("{CS}.{p}"));
    }
    for (p, scale, secs) in [("blade0_mc", 0.5, 0.35), ("blade1_mc", 0.1, 0.35), ("circle_mc", 0.5, 0.35), ("circleInt_mc", 0.5, 0.25), ("circleDotLine_mc", 1.5, 0.35)] {
        ease_to(fc, &format!("{CS}.{p}"), to().scale(scale).alpha(0.0), secs, Ease::StrongOut);
    }
    el.after(0.035, |fc, _| ease_to(fc, &format!("{CS}.number_mc"), to().yscale(1.1).xscale(1.2).alpha(0.0), 0.25, Ease::StrongOut));
    el.after(0.045, |fc, _| ease_to(fc, &format!("{CS}.bkgd_mc"), to().yscale(1.1).xscale(1.2).alpha(0.0), 0.25, Ease::StrongOut));
    el.after(0.4, |fc, _| fc.set_visible("", false));
}

// ---- DLC05_H_Tricks: a scoring's flair

/// `UpdateTrick`: its name on its brush (as wide as the words and 100), popping in, then
/// gone (`HideTrick`).
fn trick_show(fc: &mut FlashClip, el: &mut Elem, name: &str) {
    el.clear();
    fc.set_visible("", true);
    let t = "_trick_mc.txt_mc.txt";
    fc.set_text(t, name.to_uppercase());
    fc.autosize.insert(t.into(), 2);
    fc.wrap.insert(t.into(), false);
    let tw = fc.text_width(t).unwrap_or(120.0);
    set_width(fc, "_trick_mc.bkgd_mc", tw + 100.0);
    let s = el.saved("_trick_mc");
    let w = fc.size("_trick_mc").map_or(tw + 100.0, |v| v.x);
    fc.tween_end("_trick_mc", false);
    fc.set("_trick_mc", to().x(0.5 * w).y(s.y).rotation(-10.0).alpha(0.0).scale(1.5));
    fc.tween("_trick_mc", to().x(0.5 * w).y(s.y).rotation(-10.0).alpha(1.0).scale(1.0), 0.2, Ease::StrongOut);
    el.after(TRICK_SHOWN, move |fc, _| {
        ease_to(fc, "_trick_mc", to().x(0.5 * w - 50.0).rotation(-15.0).alpha(0.0), 0.2, Ease::StrongOut);
    });
}

// ---- DLC05_H_PhaseResultsScreen: a round's results

/// `Show`: the round's name and outcome (`SetTitle`), its kills (`SetList`: the possible, the
/// goal, the made, the last 10% larger and stroked), the way on (`SetMenu`), all coming in as
/// `Open` has them.
#[allow(clippy::too_many_arguments)]
fn phase_show(fc: &mut FlashClip, el: &mut Elem, data: &crate::gamedata::Data, name: &str, success: bool, possible: Option<i32>, goal: i32, kills: i32, bonus: bool, last: bool) {
    el.clear();
    fc.set_visible("", true);
    fc.tween_end("_res_mc", false);
    let r = el.saved("_res_mc");
    fc.set("_res_mc", to().alpha(0.0).y(r.y));
    fc.tween("_res_mc", r.into(), 0.35, Ease::StrongOut);
    // the title: the round, centred with its icon (completed or failed)
    let c = "_res_mc.title_mc._content_mc";
    fc.set_text(&format!("{c}.txt"), name.to_uppercase());
    fc.wrap.insert(format!("{c}.txt"), false);
    let tw = fc.text_width(&format!("{c}.txt")).unwrap_or(200.0);
    fc.set(c, to().x(-0.5 * (147.0 + tw) - 25.0));
    fc.goto(&format!("{c}.ic_mc"), if success { "completed" } else { "failed" }, false);
    // the kills (`PR_stat`: their names and counts, one under another, 5 apart)
    let h = H_TEXTS;
    fc.remove("_res_mc._list_mc");
    fc.create_empty("_res_mc", "_list_mc", [1.0, 0.0, 0.0, 1.0, -160.0, 255.0]);
    let rows: Vec<(String, i32)> = [(possible, "t_PossibleKC"), (Some(goal), "t_GoalKC"), (Some(kills), "t_KillsKC")].into_iter().filter_map(|(v, k)| v.map(|v| (data.text(h, k), v))).collect();
    let mut prev: Option<(f32, f32)> = None;
    for (i, (label, v)) in rows.iter().enumerate() {
        let p = format!("_res_mc._list_mc.stat{i}_mc");
        fc.attach("_res_mc._list_mc", "PR_stat", &format!("stat{i}_mc"), IDENTITY);
        let k = if i + 1 == rows.len() { 1.1 } else { 1.0 };
        let size = fc.size(&p).unwrap_or(Vec2::new(300.0, 36.0)) * k;
        let y = prev.map_or(0.5 * size.y, |(py, ph)| py + 0.5 * ph + 0.5 * size.y + 5.0);
        fc.set(&p, to().x(0.5 * size.x).y(y).scale(k));
        fc.set_text(&format!("{p}.txt_name.txt"), label.clone());
        fc.set_text(&format!("{p}.txt_val.txt"), v.to_string());
        prev = Some((y, size.y));
        // (one after another, 150 ms apart: turned and grown in, `open`)
        el.save(fc, &p);
        fc.set_visible(&p, false);
        let pp = p.clone();
        el.after(0.25 + 0.15 * i as f32, move |fc, el| {
            pop_in(fc, el, &pp, to().xscale(3.5).yscale(1.5).rotation(-30.0).alpha(0.0), 0.25, Ease::BackInOut);
            fc.goto(&pp, "open", true);
        });
        if i + 1 == rows.len() {
            fc.attach("_res_mc._list_mc", "PR_btnStroke_", "stroke_mc", [1.0, 0.0, 0.0, 1.0, 0.5 * size.x, y]);
            fc.goto("_res_mc._list_mc.stroke_mc", if success { "completed" } else { "failed" }, false);
        }
    }
    // the way on (`SetMenu`): the next (or bonus) round, or the round again, then the end
    let mut menu: Vec<(String, bool)> = Vec::new();
    if success && !last {
        menu.push((data.text(h, if bonus { "t_BonusPhase" } else { "t_NextPhase" }), true));
    } else if !success {
        menu.push((data.text("DisGFxMoviePlayerBase_Texts", "t_Retry"), true));
    }
    menu.push((data.text("DisGFxMoviePlayerBase_Texts", "t_EndChallenge"), false));
    for (i, (label, _)) in menu.iter().enumerate() {
        fc.remove(&format!("_res_mc.menu_mc.btn{i}"));
        fc.attach("_res_mc.menu_mc", "PR_menu_btn", &format!("btn{i}"), IDENTITY);
        let p = format!("_res_mc.menu_mc.btn{i}");
        fc.set_visible(&format!("{p}.btn"), false);
        fc.set_text(&format!("{p}._txt_mc.txt"), label.to_uppercase());
        fc.wrap.insert(format!("{p}._txt_mc.txt"), false);
        // (`DLC05_H_PhaseResultsScreen_MenuButton.SetText`: the words centred, the backing
        // 450 wide)
        let tw = fc.text_width(&format!("{p}._txt_mc.txt")).unwrap_or(200.0);
        fc.set(&format!("{p}._txt_mc"), to().x(-0.5 * tw));
        set_width(fc, &format!("{p}._bkgd_mc.mc"), 450.0);
        let n = menu.len() as f32;
        let bh = 34.0;
        let y = i as f32 * (bh + 5.0) + 0.5 * bh - 0.5 * (n * bh + (n - 1.0) * 5.0);
        fc.set(&p, to().x(0.0).y(y));
        el.save(fc, &p);
    }
    for i in menu.len()..4 {
        fc.remove(&format!("_res_mc.menu_mc.btn{i}"));
    }
    fc.set_visible("_res_mc.menu_mc", false);
    el.choices = menu.iter().map(|m| m.1).collect();
    el.labels = menu.iter().map(|m| m.0.to_uppercase()).collect();
    el.sel = 0;
    el.taking = false;
    el.opened = true;
    // `Open`: the backing from above, the separators, the title, the menu last
    for p in ["_res_mc.bkgd_mc", "_res_mc.sepL_mc", "_res_mc.sepR_mc", "_res_mc.title_mc"] {
        fc.set_visible(p, false);
    }
    el.after(0.16, |fc, el| {
        let b = el.saved("_res_mc.bkgd_mc");
        pop_in(fc, el, "_res_mc.bkgd_mc", to().y(-b.y.abs() - 300.0), 0.22, Ease::StrongOut);
    });
    el.after(0.001, |fc, el| {
        let l = el.saved("_res_mc.sepL_mc");
        pop_in(fc, el, "_res_mc.sepL_mc", to().y(l.y - 150.0).alpha(0.0), 0.25, Ease::BackInOut);
        let r = el.saved("_res_mc.sepR_mc");
        pop_in(fc, el, "_res_mc.sepR_mc", to().y(r.y + 250.0).alpha(0.0), 0.25, Ease::BackInOut);
    });
    el.after(0.01, |fc, el| pop_in(fc, el, "_res_mc.title_mc", to().scale(2.5).alpha(0.0).rotation(-25.0), 0.2, Ease::StrongOut));
    el.after(0.65, |fc, el| {
        fc.set_visible("_res_mc.menu_mc", true);
        phase_highlight(fc, el);
    });
    el.after(0.85, |_, el| el.taking = true);
}

/// The way on chosen lit (`_btnOverProps`: 110%, `over`, its words black on the light
/// backing), the others out (their words pale again).
fn phase_highlight(fc: &mut FlashClip, el: &Elem) {
    for i in 0..el.choices.len() {
        let p = format!("_res_mc.menu_mc.btn{i}");
        let on = i == el.sel;
        fc.tween(&p, to().scale(if on { 1.1 } else { 1.0 }), 0.25, Ease::BackOut);
        fc.goto(&p, if on { "over" } else { "out" }, true);
        let label = el.labels.get(i).cloned().unwrap_or_default();
        fc.set_text(&format!("{p}._txt_mc.txt"), if on { format!("<font color=\"#000000\">{label}</font>") } else { label });
    }
}

/// A way on's button (the pointer on it).
#[derive(Component)]
struct PhaseButton(usize);

/// The round's results taking input: the game held, the pointer free; the arrows and the
/// pointer choose, Enter (or a click) goes that way; then the screen goes (`Close`) and the
/// challenge has the choice.
#[allow(clippy::too_many_arguments)]
fn phase_input(
    mut commands: Commands,
    mut elems: Elems,
    mut ch: ResMut<Challenge>,
    keys: Res<ButtonInput<KeyCode>>,
    buttons: Query<(Entity, &Interaction, &PhaseButton)>,
    roots: Query<Entity, With<HudRoot>>,
    window: Query<&Window>,
    mut paused: ResMut<crate::hud::Paused>,
    mut cursor: Single<&mut bevy::window::CursorOptions>,
    mut sfx: MessageWriter<crate::audio::PostEvent>,
) {
    let Some((mut el, mut fc)) = get(&mut elems, Kind::PhaseResults) else { return };
    let (el, fc) = (&mut *el, &mut *fc);
    if !el.opened {
        for (e, ..) in &buttons {
            commands.entity(e).despawn();
        }
        return;
    }
    paused.0 = true;
    if cursor.grab_mode != bevy::window::CursorGrabMode::None {
        cursor.visible = true;
        cursor.grab_mode = bevy::window::CursorGrabMode::None;
    }
    if !el.taking {
        return;
    }
    // (the buttons over the items, once)
    if buttons.is_empty() {
        if let (Ok(w), Ok(root)) = (window.single(), roots.single()) {
            let s = (w.width() / 1280.0).min(w.height() / 720.0).max(0.01);
            let r = el.saved("_res_mc");
            let m = el.saved("_res_mc.menu_mc");
            for i in 0..el.choices.len() {
                let b = el.saved(&format!("_res_mc.menu_mc.btn{i}"));
                let c = Vec2::new(fc.m[4], fc.m[5]) + Vec2::new(r.x + m.x, r.y + m.y + b.y);
                let (bw, bh) = (360.0, 36.0);
                commands.spawn((PhaseButton(i), Button, Node { position_type: PositionType::Absolute, left: Val::Px((c.x - bw * 0.5) * s), top: Val::Px((c.y - bh * 0.5) * s), width: Val::Px(bw * s), height: Val::Px(bh * s), ..default() }, ZIndex(10), ChildOf(root)));
            }
        }
    }
    let n = el.choices.len().max(1);
    let mut moved = false;
    if keys.just_pressed(KeyCode::ArrowDown) || keys.just_pressed(KeyCode::KeyS) {
        el.sel = (el.sel + 1) % n;
        moved = true;
    }
    if keys.just_pressed(KeyCode::ArrowUp) || keys.just_pressed(KeyCode::KeyW) {
        el.sel = (el.sel + n - 1) % n;
        moved = true;
    }
    let mut chosen = (keys.just_pressed(KeyCode::Enter) || keys.just_pressed(KeyCode::Space) || keys.just_pressed(KeyCode::KeyE)).then_some(el.sel);
    for (_, i, b) in &buttons {
        match i {
            Interaction::Hovered if el.sel != b.0 => {
                el.sel = b.0;
                moved = true;
            }
            Interaction::Pressed => chosen = Some(b.0),
            _ => {}
        }
    }
    if moved {
        phase_highlight(fc, el);
    }
    let Some(c) = chosen else { return };
    // `Close`: up and away; the choice to the challenge
    sfx.write(crate::audio::PostEvent::named("UI_Validation", None));
    el.taking = false;
    el.opened = false;
    let r = el.saved("_res_mc");
    fc.tween("_res_mc", to().y(r.y - 250.0).alpha(0.0), 0.25, Ease::StrongOut);
    el.after(0.25, |fc, _| fc.set_visible("", false));
    ch.phase_choice = Some(el.choices.get(c).copied().unwrap_or(true));
    paused.0 = false;
    cursor.visible = false;
    cursor.grab_mode = bevy::window::CursorGrabMode::Locked;
}

// ---- DLC05_H_Countdown: a countdown timer's seconds

/// A digit's fields: its text, its shadows red when critical.
fn countdown_digit(fc: &mut FlashClip, mc: &str, d: char, critical: bool) {
    fc.set_text(&format!("{mc}.txt"), d.to_string());
    let shadow = if critical { RED } else { DARK };
    for f in ["txtShad0", "txtShad1"] {
        fc.set_text(&format!("{mc}.{f}"), format!("<font color=\"{shadow}\">{d}</font>"));
    }
}

fn countdown_open(fc: &mut FlashClip, el: &mut Elem, start: i64) {
    countdown_open_at(fc, el, "_cd_mc", start, CRITICAL);
}

fn countdown_value(fc: &mut FlashClip, el: &mut Elem, n: i64) {
    countdown_value_at(fc, el, "_cd_mc", n, CRITICAL);
}

fn countdown_close(fc: &mut FlashClip, el: &mut Elem) {
    countdown_close_at(fc, el, "_cd_mc");
    el.after(0.3, |fc, _| fc.set_visible("", false));
}

/// `InitCountdown` of a countdown within (its `_cd_mc`): the seconds, then its parts in (an
/// hourglass and a turning circle where it has them).
fn countdown_open_at(fc: &mut FlashClip, el: &mut Elem, cd: &str, start: i64, crit: i64) {
    el.clear();
    el.digits.clear();
    countdown_value_at(fc, el, cd, start, crit);
    el.opened = true;
    fc.set_visible("", true);
    ease_to(fc, cd, el.saved(cd).into(), 0.25, Ease::StrongOut);
    for p in ["hGlass_mc", "circle_mc", "bkgd_mc", "trame_mc", "txt_mc", "circle_arrow_mc"] {
        fc.set_visible(&format!("{cd}.{p}"), false);
    }
    let c = cd.to_string();
    let c1 = c.clone();
    el.after(0.005, move |fc, el| {
        pop_in(fc, el, &format!("{c1}.hGlass_mc"), to().alpha(0.0).xscale(2.0).yscale(4.5).rotation(50.0), 0.25, Ease::StrongOut);
    });
    let c2 = c.clone();
    el.after(0.06, move |fc, el| {
        let (circle, arrow) = (format!("{c2}.circle_mc"), format!("{c2}.circle_arrow_mc"));
        pop_in(fc, el, &circle, to().alpha(0.0).xscale(2.5).yscale(3.5).rotation(70.0), 0.25, Ease::BackInOut);
        pop_in(fc, el, &arrow, to().alpha(0.0).xscale(0.5).yscale(0.35).rotation(-40.0), 0.25, Ease::BackInOut);
        el.after(0.25, move |fc, _| {
            fc.spin(&circle, start as f32 + 5.0, false);
            fc.spin(&arrow, 2.0, false);
        });
    });
    let c3 = c.clone();
    el.after(0.2, move |fc, el| {
        let (b, t) = (format!("{c3}.bkgd_mc"), format!("{c3}.trame_mc"));
        let s = el.saved(&b);
        pop_in(fc, el, &b, to().alpha(0.0).x(s.x - 50.0), 0.25, Ease::BackInOut);
        let s = el.saved(&t);
        pop_in(fc, el, &t, to().alpha(0.0).x(s.x + 50.0), 0.25, Ease::StrongOut);
    });
    el.after(0.25, move |fc, el| {
        let t = format!("{c}.txt_mc");
        let s = el.saved(&t);
        pop_in(fc, el, &t, to().alpha(0.0).x(s.x - 50.0), 0.25, Ease::BackInOut);
    });
}

/// `UpdateValue`: the seconds, a digit falling out and the new one in where it changed; the
/// hourglass and backdrop critical under `crit`.
fn countdown_value_at(fc: &mut FlashClip, el: &mut Elem, cd: &str, n: i64, crit: i64) {
    let critical = n < crit;
    if el.opened && critical != el.critical {
        if critical {
            fc.goto(&format!("{cd}.hGlass_mc"), "critical", true);
            fc.goto(&format!("{cd}.bkgd_mc"), "critical", true);
        } else {
            fc.goto(&format!("{cd}.hGlass_mc"), "loop", true);
            fc.goto_frame(&format!("{cd}.bkgd_mc"), 1, false);
        }
    }
    let s = format!("{n:02}");
    let digits: Vec<char> = s.chars().collect();
    for (i, d) in digits.iter().copied().enumerate().take(2) {
        let mc = format!("{cd}.txt_mc.t{i}");
        let before = el.digits.get(i).copied();
        if el.opened && before.is_some_and(|b| b != d) {
            let sp = el.saved(&mc);
            ease_to(fc, &mc, to().y(sp.y + 25.0).alpha(0.0), 0.25, Ease::StrongOut);
            el.after(0.25, move |fc, el| {
                countdown_digit(fc, &mc, d, critical);
                let sp = el.saved(&mc);
                fc.set(&mc, to().y(sp.y - 25.0));
                ease_to(fc, &mc, sp.into(), 0.2, Ease::BackOut);
            });
        } else {
            countdown_digit(fc, &mc, d, critical);
        }
    }
    el.digits = digits;
    el.critical = critical;
    el.value = n;
}

fn countdown_close_at(fc: &mut FlashClip, el: &mut Elem, cd: &str) {
    el.opened = false;
    ease_to(fc, cd, to().scale(1.2).alpha(0.0), 0.25, Ease::StrongOut);
}

// ---- DLC05_H_JumpHeight: a drop's height as Corvo falls

/// `SetJumpH`: the metres, the backdrop as wide (75 more, at least as made).
fn jump_set(fc: &mut FlashClip, el: &mut Elem, h: i64) {
    let t = "_jumpH_mc.txt_mc.txt";
    fc.set_text(t, format!("{h}m"));
    fc.autosize.insert(t.into(), 2);
    fc.wrap.insert(t.into(), false);
    let tw = fc.text_width(t).unwrap_or(60.0);
    set_width(fc, "_jumpH_mc.bkgd_mc", el.w0.max(tw + 75.0));
    el.value = h;
}

/// `ShowJumpHeight`: set, then open (the circle, the backdrop, the words).
fn jump_show(fc: &mut FlashClip, el: &mut Elem, h: i64) {
    jump_set(fc, el, h);
    el.clear();
    el.opened = true;
    fc.set_visible("", true);
    ease_to(fc, "_jumpH_mc", el.saved("_jumpH_mc").into(), 0.2, Ease::StrongOut);
    pop_in(fc, el, "_jumpH_mc.circle_mc", to().alpha(0.0).scale(2.5).rotation(45.0), 0.35, Ease::BackInOut);
    el.after(0.02, |fc, el| pop_in(fc, el, "_jumpH_mc.bkgd_mc", to().alpha(0.0).scale(2.5).rotation(-10.5), 0.2, Ease::StrongOut));
    el.after(0.05, |fc, el| pop_in(fc, el, "_jumpH_mc.txt_mc", to().alpha(0.0).scale(2.5).rotation(-15.0), 0.2, Ease::StrongOut));
}

fn jump_hide(fc: &mut FlashClip, el: &mut Elem) {
    el.clear();
    el.opened = false;
    ease_to(fc, "_jumpH_mc", to().scale(1.2).alpha(0.0), 0.2, Ease::StrongOut);
    el.after(1.0, |fc, _| fc.set_visible("", false));
}

// ---- DLC05_H_EggDiscovery: a clockwork egg found

/// `Update(cur, max)`: "n/max Clockwork Egg(s) found" on a backdrop as wide, centred, then
/// `Open`.
fn egg_update(fc: &mut FlashClip, el: &mut Elem, cur: i64, max: i64, data: &crate::gamedata::Data) {
    el.value = cur;
    el.max = max;
    let key = if cur > 1 { "t_NewEggDiscovered_p" } else { "t_NewEggDiscovered" };
    let t = data.text(H_TEXTS, key).replace("§COUNT§", &cur.to_string()).replace("§MAX§", &max.to_string()).to_uppercase();
    for f in ["txt", "txtShad0", "txtShad1"] {
        let p = format!("_ed_mc.txt_mc.{f}");
        fc.set_text(&p, t.clone());
        fc.autosize.insert(p.clone(), 1);
        fc.wrap.insert(p, false);
    }
    let tw = fc.text_width("_ed_mc.txt_mc.txt").unwrap_or(300.0) + 5.0;
    let w = tw + 130.0;
    set_width(fc, "_ed_mc.bkgd_mc", w);
    let tx = el.saved("_ed_mc.txt_mc").x;
    fc.set("_ed_mc.bkgd_mc", to().x(tx - 0.5 * w + 50.0));
    el.save(fc, "_ed_mc.bkgd_mc");
    let ew = fc.size("_ed_mc").map_or(w, |s| s.x);
    fc.set("_ed_mc", to().x(0.5 * ew));
    el.save(fc, "_ed_mc");
    egg_open(fc, el);
}

fn egg_open(fc: &mut FlashClip, el: &mut Elem) {
    el.clear();
    el.opened = true;
    fc.set_visible("", true);
    let s = el.saved("_ed_mc");
    fc.tween_end("_ed_mc", false);
    fc.set("_ed_mc", to().alpha(1.0).x(s.x));
    for p in ["circleE_mc", "circleI_mc", "iconBkgd_mc", "icon_mc", "txt_mc", "bkgd_mc", "sepU_mc", "sepD_mc"] {
        fc.set_visible(&format!("_ed_mc.{p}"), false);
    }
    el.after(0.025, |fc, el| {
        let s = el.saved("_ed_mc.bkgd_mc");
        pop_in(fc, el, "_ed_mc.bkgd_mc", to().alpha(0.0).x(s.x + 50.0), 0.35, Ease::BackInOut);
    });
    el.after(0.01, |fc, el| {
        let s = el.saved("_ed_mc.sepU_mc");
        pop_in(fc, el, "_ed_mc.sepU_mc", to().alpha(0.0).x(s.x - 150.0), 0.25, Ease::BackInOut);
        let s = el.saved("_ed_mc.sepD_mc");
        pop_in(fc, el, "_ed_mc.sepD_mc", to().alpha(0.0).x(s.x + 250.0), 0.25, Ease::BackInOut);
    });
    el.after(0.001, |fc, el| {
        pop_in(fc, el, "_ed_mc.icon_mc", to().alpha(0.0).xscale(2.0).yscale(2.5).rotation(25.0), 0.25, Ease::BackInOut);
        fc.goto("_ed_mc.icon_mc", "open", true);
        pop_in(fc, el, "_ed_mc.iconBkgd_mc", to().alpha(0.0).xscale(0.3).yscale(0.5), 0.35, Ease::BackInOut);
    });
    el.after(0.01, |fc, el| pop_in(fc, el, "_ed_mc.circleE_mc", to().alpha(0.0).xscale(3.0).yscale(3.5).rotation(95.0), 0.3, Ease::BackInOut));
    el.after(0.05, |fc, el| {
        let s = el.saved("_ed_mc.txt_mc");
        pop_in(fc, el, "_ed_mc.txt_mc", to().alpha(0.0).x(s.x + 150.0), 0.3, Ease::StrongInOut);
        pop_in(fc, el, "_ed_mc.circleI_mc", to().alpha(0.0).xscale(2.3).yscale(3.5).rotation(-75.0), 0.3, Ease::BackInOut);
    });
}

fn egg_close(fc: &mut FlashClip, el: &mut Elem) {
    el.clear();
    el.opened = false;
    let s = el.saved("_ed_mc");
    ease_to(fc, "_ed_mc", to().x(s.x + 50.0).alpha(0.0), 0.25, Ease::StrongOut);
    el.after(0.3, |fc, _| fc.set_visible("", false));
}

// ---- DLC05_H_ChainGauge: the kill chain's time left

/// `Show(gauge, countdown)`: its countdown's seconds, the gauge at so many hundredths, in.
fn chain_show(fc: &mut FlashClip, el: &mut Elem, gauge: i64, secs: i64) {
    el.clear();
    if el.title_open {
        hide_title(fc, el, "_broken_mc.");
    }
    countdown_open_at(fc, el, CHAIN_CD, secs, CHAIN_CRITICAL);
    chain_gauge(fc, gauge);
    fc.set_visible("", true);
    // `OpenGauge`
    el.gauge_open = true;
    pop_in(fc, el, "_chain_mc.gauge_mc", to().alpha(0.0).rotation(-25.0).xscale(1.25).yscale(1.5), 0.25, Ease::BackInOut);
    pop_in(fc, el, "_chain_mc.skull_mc", to().alpha(0.0).rotation(5.0).xscale(2.05).yscale(2.5), 0.35, Ease::BackInOut);
    pop_in(fc, el, "_chain_mc.chainsBkgd_mc", to().alpha(0.0).xscale(0.25).yscale(0.5), 0.25, Ease::BackInOut);
    pop_in(fc, el, "_chain_mc.circle_mc", to().alpha(0.0).xscale(3.25).yscale(2.5), 0.35, Ease::BackInOut);
}

/// `UpdateGauge`: its frame (0 to 100).
fn chain_gauge(fc: &mut FlashClip, gauge: i64) {
    fc.goto_frame("_chain_mc.gauge_mc.completion_mc", gauge.clamp(0, 100) as usize + 1, false);
}

fn chain_hide_gauge(fc: &mut FlashClip, el: &mut Elem) {
    el.gauge_open = false;
    ease_to(fc, "_chain_mc.gauge_mc", to().yscale(1.5).xscale(1.35).rotation(-25.0).alpha(0.0), 0.25, Ease::StrongOut);
    for p in ["skull_mc", "chainsBkgd_mc"] {
        ease_to(fc, &format!("_chain_mc.{p}"), to().yscale(1.5).xscale(1.25).alpha(0.0), 0.25, Ease::StrongOut);
    }
    ease_to(fc, "_chain_mc.circle_mc", to().yscale(1.5).xscale(1.25).alpha(0.0), 0.25, Ease::StrongInOut);
}

/// `ComboBroken`: "Chain broken", the countdown and gauge gone.
fn chain_broken(fc: &mut FlashClip, el: &mut Elem, data: &crate::gamedata::Data) {
    show_title(fc, el, "_broken_mc.", &data.text(H_TEXTS, "t_ComboBroken"));
    if el.opened {
        countdown_close_at(fc, el, CHAIN_CD);
    }
    if el.gauge_open {
        chain_hide_gauge(fc, el);
    }
}

fn chain_close(fc: &mut FlashClip, el: &mut Elem) {
    el.clear();
    if el.opened {
        countdown_close_at(fc, el, CHAIN_CD);
    }
    if el.title_open {
        hide_title(fc, el, "_broken_mc.");
    }
    if el.gauge_open {
        chain_hide_gauge(fc, el);
    }
    el.after(0.5, |fc, _| fc.set_visible("", false));
}

// ---- DLC05_H_Clues: the mystery man's clues, a line each

/// `AddNewClue`: a line under the last (`Clues_clueItem`), in from the right.
fn clue_add(fc: &mut FlashClip, el: &mut Elem, text: &str) {
    el.clear();
    if !el.opened {
        fc.remove("_list_mc");
        fc.create_empty("", "_list_mc", IDENTITY);
        el.value = 0;
        el.opened = true;
        fc.set_visible("", true);
        fc.tween_end("", false);
        fc.set("", to().alpha(1.0));
    }
    let n = el.value;
    let name = format!("line{n}");
    fc.attach("_list_mc", "Clues_clueItem", &name, IDENTITY);
    let path = format!("_list_mc.{name}");
    // (a line as tall as its words' field: `_height` counts it)
    let h = fc.size(&path).map_or(0.0, |s| s.y).max(CLUE_LINE);
    let y = if n > 0 {
        let prev = format!("_list_mc.line{}", n - 1);
        let py = fc.props(&prev).map_or(0.0, |p| p.y);
        py + h
    } else {
        0.5 * h
    };
    fc.set(&path, to().y(y));
    let t = format!("{path}.txt");
    fc.set_text(&t, text.to_string());
    fc.autosize.insert(t.clone(), 1);
    fc.wrap.insert(t, false);
    fc.goto(&path, "open", true);
    el.save(fc, &path);
    let s = el.saved(&path);
    pop_in(fc, el, &path, to().alpha(0.0).x(s.x + 150.0), 0.25, Ease::BackInOut);
    el.value += 1;
}

fn clues_hide(fc: &mut FlashClip, el: &mut Elem) {
    el.clear();
    el.opened = false;
    ease_to(fc, "", to().alpha(0.0), 0.25, Ease::StrongOut);
    el.after(0.25, |fc, _| fc.remove("_list_mc"));
}

// ---- DLC05_H_MMTarget: the mystery foe's portrait

/// `Show(img, color)`: its picture in its side's circle (blue, else red), turning.
fn mm_show(fc: &mut FlashClip, el: &mut Elem, img: Option<(Handle<Image>, Vec2)>, blue: bool) {
    el.clear();
    fc.stop_spin("_tgt_mc.circleColor_mc");
    fc.stop_spin("_tgt_mc.circle_mc");
    fc.remove("_tgt_mc");
    fc.attach("", "TGT_mc", "_tgt_mc", [1.0, 0.0, 0.0, 1.0, -65.0, 90.0]);
    el.save(fc, "_tgt_mc");
    el.save(fc, "_tgt_mc.img_mc");
    fc.load_image("_tgt_mc.img_mc", img);
    fc.goto_frame("_tgt_mc", if blue { 1 } else { 2 }, false);
    el.save(fc, "_tgt_mc.circle_mc");
    el.save(fc, "_tgt_mc.circleColor_mc");
    el.opened = true;
    fc.set_visible("", true);
    pop_in(fc, el, "_tgt_mc", to().alpha(0.0).scale(2.0), 0.25, Ease::BackInOut);
    for p in ["circleColor_mc", "circle_mc", "img_mc"] {
        fc.set_visible(&format!("_tgt_mc.{p}"), false);
    }
    el.after(0.005, |fc, el| {
        pop_in(fc, el, "_tgt_mc.circleColor_mc", to().alpha(0.0).xscale(2.0).yscale(3.0).rotation(20.0), 0.25, Ease::BackInOut);
        el.after(0.25, |fc, _| fc.spin("_tgt_mc.circleColor_mc", 2.0, false));
    });
    el.after(0.025, |fc, el| {
        pop_in(fc, el, "_tgt_mc.circle_mc", to().alpha(0.0).xscale(2.0).yscale(3.0).rotation(20.0), 0.25, Ease::BackInOut);
        el.after(0.25, |fc, _| fc.spin("_tgt_mc.circle_mc", 1.0, true));
    });
    el.after(0.05, |fc, el| pop_in(fc, el, "_tgt_mc.img_mc", to().alpha(0.0).xscale(2.0).yscale(3.0).rotation(20.0), 0.25, Ease::BackInOut));
}

fn mm_hide(fc: &mut FlashClip, el: &mut Elem) {
    el.clear();
    el.opened = false;
    ease_to(fc, "_tgt_mc", to().scale(1.5).alpha(0.0), 0.25, Ease::StrongOut);
    el.after(0.25, |fc, _| {
        fc.load_image("_tgt_mc.img_mc", None);
        fc.set_visible("", false);
    });
}

// ---- DLC05_H_EquipmentUnlocked: what the scripts unlocked

/// How an unlock is drawn: a power cast (a round stroke, its icon), a passive power (square,
/// the movie's own picture), an upgrade (square, its picture).
#[derive(Clone, Copy, PartialEq)]
enum EqShape {
    Active,
    Passive,
    Upgrade,
}

/// An unlock's entry (`{itemName, selectionType, powerLevel, itemImg}`).
struct Unlocked {
    name: String,
    level: Option<u8>,
    shape: EqShape,
    image: Option<(Handle<Image>, Vec2)>,
}

/// What was given, as the unlock lists it: the powers by name (their levels), the upgrades
/// by their tweaks (their store names and pictures).
fn unlocked(list: &[Grant], data: &crate::gamedata::Data, ui: &mut crate::ui_images::UiImages, images: &mut Assets<Image>) -> Vec<Unlocked> {
    list.iter()
        .filter_map(|g| match g {
            Grant::Power(name, level) => {
                let k = name.to_ascii_lowercase();
                let passive = match k.as_str() {
                    "vitality" => Some("Vitality"),
                    "agility" | "celerity" => Some("Agility"),
                    "bloodthirsty" => Some("Bloodthirsty"),
                    "shadowkill" => Some("Shadow Kill"),
                    _ => None,
                };
                if let Some(n) = passive {
                    return Some(Unlocked { name: n.to_string(), level: Some(*level), shape: EqShape::Passive, image: None });
                }
                let key = match k.as_str() {
                    "possession" => "Possess",
                    "ratswarm" => "DevouringSwarm",
                    other => other,
                };
                let p = crate::powers::Power::from_key(key)?;
                Some(Unlocked { name: p.name().to_string(), level: Some(*level), shape: EqShape::Active, image: ui.file(images, "icons", p.icon()) })
            }
            Grant::Upgrade(id) => {
                let u = data.0.upgrades.iter().find(|u| u.id.eq_ignore_ascii_case(id))?;
                Some(Unlocked { name: u.name.clone(), level: None, shape: EqShape::Upgrade, image: ui.file(images, "items", &format!("{}_Large", u.icon)) })
            }
        })
        .collect()
}

const EQ_IMG: &str = "_eq_mc.content_mc._img_mc";

/// `ShowEquipmentUnlocked`: `SetDisplay` (a picture each, right to left, "Unlocked:" and
/// their names beside, the backdrop as wide) then `Open`; gone after a while.
fn eq_show(fc: &mut FlashClip, el: &mut Elem, items: &[Unlocked], data: &crate::gamedata::Data) {
    fc.tween_end("_eq_mc.bkgd_mc", false);
    fc.set("_eq_mc.bkgd_mc", to().x(1280.0 - SAFE_MAX.x));
    fc.remove(EQ_IMG);
    fc.create_empty("_eq_mc.content_mc", "_img_mc", IDENTITY);
    let mut names = String::new();
    let mut w = 100.0;
    for (i, it) in items.iter().enumerate() {
        if i > 0 {
            names.push('\n');
        }
        names.push_str(&it.name);
        let p = format!("{EQ_IMG}.img{i}");
        fc.attach(EQ_IMG, "EqU_img", &format!("img{i}"), IDENTITY);
        fc.set_visible(&format!("{p}.powerLevel_mc"), false);
        let square = it.shape != EqShape::Active;
        fc.set_visible(&format!("{p}.stroke_mc.circle_mc"), !square);
        fc.set_visible(&format!("{p}.stroke_mc.square_mc"), square);
        let loader = format!("{p}.imgLoader_mc");
        if it.shape == EqShape::Passive {
            fc.attach(&loader, "EqU_img_pPower", "img", [0.85, 0.0, 0.0, 0.85, 0.0, 0.0]);
        } else {
            fc.load_image(&loader, it.image.clone());
        }
        if let Some(l) = it.level {
            let r = match l {
                1 => "I",
                2 => "II",
                _ => "",
            };
            fc.set_visible(&format!("{p}.powerLevel_mc"), true);
            fc.set_text(&format!("{p}.powerLevel_mc.txt_mc.txt"), r);
            names.push(' ');
            names.push_str(r);
        }
        let sz = fc.size(&p).unwrap_or(Vec2::splat(100.0));
        w = sz.x;
        fc.set(&p, to().x(0.5 * w + i as f32 * (w - 10.0)).y(0.5 * sz.y - 15.0));
        fc.set_visible(&p, false);
        fc.set_visible(&format!("{p}.stroke_mc"), false);
        fc.goto(&format!("{p}.stroke_mc"), "loop", true);
        fc.set_visible(&loader, false);
        el.save(fc, &p);
        el.save(fc, &loader);
    }
    let n = items.len();
    let iw = if n > 0 { w + (n - 1) as f32 * (w - 10.0) } else { 0.0 };
    fc.set(EQ_IMG, to().x(-iw));
    let (title, list) = ("_eq_mc.content_mc.txt_mc.title_txt", "_eq_mc.content_mc.txt_mc.list_txt");
    fc.set_text(title, data.text(H_TEXTS, "t_Unlocked").to_uppercase());
    fc.set_text(list, names);
    for t in [title, list] {
        fc.autosize.insert(t.into(), 1);
        fc.wrap.insert(t.into(), false);
    }
    fc.set("_eq_mc.content_mc.txt_mc", to().x(-iw - 10.0));
    let tw = fc.text_width(title).unwrap_or(150.0).max(fc.text_width(list).unwrap_or(150.0));
    let bx = 1280.0 - SAFE_MAX.x;
    let bw = (iw + 10.0 + tw + 130.0 + bx).max(el.w0);
    set_width(fc, "_eq_mc.bkgd_mc", bw);
    let sw = fc.size("_eq_mc.sepD_mc").map_or(200.0, |s| s.x);
    fc.set("_eq_mc.sepD_mc", to().x(-bw + 300.0 + 0.5 * sw));
    for p in ["_eq_mc.content_mc.txt_mc", "_eq_mc.bkgd_mc", "_eq_mc.sepD_mc"] {
        el.save(fc, p);
    }
    el.value = n as i64;
    eq_open(fc, el);
    el.after(UNLOCK_SHOWN, eq_hide);
}

fn eq_open(fc: &mut FlashClip, el: &mut Elem) {
    el.clear();
    el.opened = true;
    fc.set_visible("", true);
    let s = el.saved("_eq_mc");
    fc.tween_end("_eq_mc", false);
    fc.set("_eq_mc", to().alpha(0.0).x(s.x + 200.0));
    fc.tween("_eq_mc", s.into(), 0.25, Ease::StrongOut);
    // `OpenSep`
    let d = "_eq_mc.sepD_mc";
    let sd = el.saved(d);
    fc.set_visible(d, true);
    fc.tween_end(d, false);
    fc.set(d, to().x(sd.x + 250.0).xscale(sd.xscale + 0.5).yscale(sd.xscale + 0.5).alpha(0.0));
    fc.tween(d, PropsTo::from(sd).alpha(1.0), 0.2, Ease::BackInOut);
    let u = "_eq_mc.sepU_mc";
    let su = el.saved(u);
    fc.set_visible(u, true);
    fc.tween_end(u, false);
    fc.set(u, to().x(su.x - 350.0).alpha(0.0));
    fc.tween(u, PropsTo::from(su).alpha(1.0), 0.3, Ease::BackInOut);
    el.after(0.015, |fc, el| {
        let b = "_eq_mc.bkgd_mc";
        let sb = el.saved(b);
        fc.set_visible(b, true);
        fc.tween_end(b, false);
        fc.set(b, to().x(sb.x + 250.0).alpha(0.0));
        fc.tween(b, PropsTo::from(sb).alpha(1.0), 0.3, Ease::BackInOut);
    });
    let n = el.value.max(0) as usize;
    el.after(0.15, move |fc, el| {
        fc.set_visible(EQ_IMG, true);
        for i in 0..n {
            el.after(i as f32 * 0.1, move |fc, el| {
                let p = format!("{EQ_IMG}.img{i}");
                fc.set_visible(&p, true);
                fc.set_visible(&format!("{p}.imgLoader_mc"), true);
                fc.tween_end(&p, false);
                fc.set(&p, to().xscale(3.0).yscale(5.5).alpha(0.0).rotation(50.0));
                fc.tween(&p, el.saved(&p).into(), 0.25, Ease::BackInOut);
            });
        }
    });
    el.after(0.25, move |_, el| {
        for i in 0..n {
            el.after(i as f32 * 0.15, move |fc, _| {
                let p = format!("{EQ_IMG}.img{i}.stroke_mc");
                fc.set_visible(&p, true);
                fc.tween_end(&p, false);
                fc.set(&p, to().scale(1.5).alpha(0.0).rotation(-25.0));
                fc.tween(&p, to().rotation(0.0).scale(1.0).alpha(1.0), 0.25, Ease::BackInOut);
            });
        }
    });
    el.after(0.2, |fc, el| {
        let t = "_eq_mc.content_mc.txt_mc";
        let st = el.saved(t);
        fc.set_visible(t, true);
        fc.tween_end(t, false);
        fc.set(t, to().x(st.x + 450.0).alpha(0.0));
        fc.tween(t, PropsTo::from(st).alpha(1.0), 0.3, Ease::StrongOut);
    });
}

fn eq_hide(fc: &mut FlashClip, el: &mut Elem) {
    el.clear();
    el.opened = false;
    let s = el.saved("_eq_mc");
    ease_to(fc, "_eq_mc", to().alpha(0.0).x(s.x + 150.0), 0.25, Ease::StrongOut);
    el.after(0.3, |fc, _| fc.set_visible("", false));
}
