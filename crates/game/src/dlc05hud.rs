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
//!   countdown timer's seconds (`DLC05_H_Countdown`: an hourglass, red under ten).

use crate::challenge::{Challenge, HudEvent, HudItem};
use crate::flash::{Clip, Ease, FlashClip, MovieTimelines, Props, PropsTo, IDENTITY};
use crate::GameState;
use bevy::prelude::*;
use std::collections::{BTreeMap, HashMap};

pub struct Dlc05HudPlugin;

impl Plugin for Dlc05HudPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Update, drive.run_if(in_state(GameState::InGame)));
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
/// `DLC05_H_Wave._waveDisplayDuration` (the movie's own test: 2 s)
const WAVE_SHOWN: f32 = 2.0;
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
}

const KINDS: [Kind; 12] = [Kind::Score, Kind::Timer, Kind::Kills, Kind::Tanks, Kind::Coins, Kind::Eggs, Kind::Busted, Kind::Gates, Kind::Enemies, Kind::Wave, Kind::CountdownStart, Kind::Countdown];

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
}

impl Elem {
    fn new(kind: Kind) -> Elem {
        Elem { kind, saved: HashMap::new(), later: Vec::new(), opened: false, value: 0, max: 0, init: 0, skull: 30.0, critical: false, digits: Vec::new(), final_wave: false }
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
    time: Res<Time>,
    mut sfx: MessageWriter<crate::audio::PostEvent>,
    mut shown: Local<Shown>,
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
                    score_update(&mut fc, &mut el, shown.pending, 1.0);
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
    fc.set_visible("", true);
    let (mc, sound) = if idx == -1 {
        el.final_wave = true;
        let t = text.unwrap_or("").to_uppercase();
        let paths: Vec<String> = ["txt", "txtShad0", "txtShad1"].iter().map(|f| format!("_waveInf_mc.txt_mc.{f}")).collect();
        set_texts(fc, &paths, &t);
        for p in &paths {
            fc.autosize.insert(p.clone(), 1);
        }
        let tw = fc.text_width(&paths[0]).unwrap_or(200.0);
        fc.set("_waveInf_mc.txt_mc", to().x(0.5 * tw).alpha(1.0));
        el.save(fc, "_waveInf_mc.txt_mc");
        let bw = fc.size("_waveInf_mc.bkgd_mc").map_or(400.0, |s| s.x);
        set_width(fc, "_waveInf_mc.bkgd_mc", bw.max(tw + 100.0));
        let a = el.saved("_waveInf_mc.bkgd_mc").alpha;
        fc.set("_waveInf_mc.bkgd_mc", to().alpha(a));
        el.save(fc, "_waveInf_mc.bkgd_mc");
        fc.set_visible("_wave_mc", false);
        fc.set_visible("_waveInf_mc", true);
        ("_waveInf_mc", "H_DLC05_FinalWave")
    } else {
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
        ("_wave_mc", "H_DLC05_Wave")
    };
    for p in ["bkgd_mc", "sepU_mc", "sepD_mc", "txt_mc"] {
        fc.set_visible(&format!("{mc}.{p}"), false);
    }
    let m = mc.to_string();
    let m2 = m.clone();
    let m3 = m.clone();
    el.after(0.05, move |fc, el| {
        // `OpenBkgd`
        let p = format!("{m}.bkgd_mc");
        let s = el.saved(&p);
        fc.set_visible(&p, true);
        fc.tween_end(&p, false);
        fc.set(&p, to().alpha(0.0).x(-250.0).xscale(s.xscale * 0.05));
        fc.tween(&p, s.into(), 0.35, Ease::StrongOut);
    });
    el.after(0.04, move |fc, el| {
        // `OpenSep`
        let u = format!("{m2}.sepU_mc");
        let s = el.saved(&u);
        pop_in(fc, el, &u, to().alpha(0.0).x(s.x + 250.0), 0.25, Ease::BackOut);
        let d = format!("{m2}.sepD_mc");
        let s = el.saved(&d);
        pop_in(fc, el, &d, to().alpha(0.0).x(s.x - 250.0), 0.25, Ease::BackOut);
    });
    el.after(0.05, move |fc, el| {
        // `OpenTxt`
        let p = format!("{m3}.txt_mc");
        let s = el.saved(&p);
        pop_in(fc, el, &p, to().alpha(0.0).x(s.x + 50.0), 0.15, Ease::BackInOut);
    });
    sound
}

fn hide_wave(fc: &mut FlashClip, el: &mut Elem) {
    el.clear();
    let mc = if el.final_wave { "_waveInf_mc" } else { "_wave_mc" };
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
    if !el.final_wave {
        fc.goto_frame("_wave_mc.number_mc", 1, false);
        ease_to(fc, "_wave_mc.number_mc", to().rotation(5.0).yscale(1.5).xscale(1.8).alpha(0.0), 0.2, Ease::BackInOut);
        ease_to(fc, "_wave_mc.circle_mc", to().scale(1.8).alpha(0.0), 0.2, Ease::BackInOut);
        fc.stop_spin("_wave_mc.circle_mc");
        fc.goto("_wave_mc.splash_mc", "close", true);
    }
    el.after(0.5, |fc, _| fc.set_visible("", false));
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
    el.clear();
    el.digits.clear();
    countdown_value(fc, el, start);
    el.opened = true;
    fc.set_visible("", true);
    ease_to(fc, "_cd_mc", el.saved("_cd_mc").into(), 0.25, Ease::StrongOut);
    for p in ["hGlass_mc", "circle_mc", "bkgd_mc", "trame_mc", "txt_mc", "circle_arrow_mc"] {
        fc.set_visible(&format!("_cd_mc.{p}"), false);
    }
    el.after(0.005, |fc, el| {
        pop_in(fc, el, "_cd_mc.hGlass_mc", to().alpha(0.0).xscale(2.0).yscale(4.5).rotation(50.0), 0.25, Ease::StrongOut);
    });
    el.after(0.06, move |fc, el| {
        pop_in(fc, el, "_cd_mc.circle_mc", to().alpha(0.0).xscale(2.5).yscale(3.5).rotation(70.0), 0.25, Ease::BackInOut);
        pop_in(fc, el, "_cd_mc.circle_arrow_mc", to().alpha(0.0).xscale(0.5).yscale(0.35).rotation(-40.0), 0.25, Ease::BackInOut);
        el.after(0.25, move |fc, _| {
            fc.spin("_cd_mc.circle_mc", start as f32 + 5.0, false);
            fc.spin("_cd_mc.circle_arrow_mc", 2.0, false);
        });
    });
    el.after(0.2, |fc, el| {
        let s = el.saved("_cd_mc.bkgd_mc");
        pop_in(fc, el, "_cd_mc.bkgd_mc", to().alpha(0.0).x(s.x - 50.0), 0.25, Ease::BackInOut);
        let s = el.saved("_cd_mc.trame_mc");
        pop_in(fc, el, "_cd_mc.trame_mc", to().alpha(0.0).x(s.x + 50.0), 0.25, Ease::StrongOut);
    });
    el.after(0.25, |fc, el| {
        let s = el.saved("_cd_mc.txt_mc");
        pop_in(fc, el, "_cd_mc.txt_mc", to().alpha(0.0).x(s.x - 50.0), 0.25, Ease::BackInOut);
    });
}

/// `UpdateValue`: the seconds, a digit falling out and the new one in where it changed; the
/// hourglass and backdrop critical under ten.
fn countdown_value(fc: &mut FlashClip, el: &mut Elem, n: i64) {
    let critical = n < CRITICAL;
    if el.opened && critical != el.critical {
        if critical {
            fc.goto("_cd_mc.hGlass_mc", "critical", true);
            fc.goto("_cd_mc.bkgd_mc", "critical", true);
        } else {
            fc.goto("_cd_mc.hGlass_mc", "loop", true);
            fc.goto_frame("_cd_mc.bkgd_mc", 1, false);
        }
    }
    let s = format!("{n:02}");
    let digits: Vec<char> = s.chars().collect();
    for (i, d) in digits.iter().copied().enumerate().take(2) {
        let mc = format!("_cd_mc.txt_mc.t{i}");
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

fn countdown_close(fc: &mut FlashClip, el: &mut Elem) {
    el.opened = false;
    ease_to(fc, "_cd_mc", to().scale(1.2).alpha(0.0), 0.25, Ease::StrongOut);
    el.after(0.3, |fc, _| fc.set_visible("", false));
}
