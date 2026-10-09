//! Dunwall City Trials' results (`UI_Results_DLC05.Results`, cooked as `Results_DLC05`), as
//! its classes show them when a run ends (`ShowResults`): the screen goes black
//! (`FadeToBlack`, 0.85 s), the success or failure sting plays, and over the challenge's
//! backdrop (`UI_ResBg_<challenge>`, under the movie's blades, shards and 60% black) the
//! results screen (`R_ResultsScreen`) opens part by part as its `MultipleDelayedAnimations`
//! have it: the title (the challenge's name, its mode and its kind's icon), the time, the
//! statistics of the scoring rule set (`m_ResultsMenuStats`: `R_Stats_item` rows, scrolled
//! with the wheel or the arrows), the medal (`DLC05_CM_Medal`: the score and its stars), the
//! clockwork doll (found or not), and the final score over the best (a new record's seal);
//! a failed run its words instead. Then the next screen (`R_Next_Screen`): replay the
//! challenge, leave it for the challenges, or leave the Trials for the main menu.

use crate::challenge::{Challenge, ChallengeLaunch, ChallengeProfile};
use crate::flash::{Clip, Ease, FlashClip, MovieTimelines, Props, PropsTo, IDENTITY};
use crate::GameState;
use bevy::prelude::*;
use bevy::window::{CursorGrabMode, CursorOptions};
use std::collections::HashMap;

pub struct Dlc05ResultsPlugin;

impl Plugin for Dlc05ResultsPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Update, (open, run, input).chain().run_if(in_state(GameState::InGame)));
    }
}

const MOVIE: &str = "Results_DLC05";
const MEDAL_MOVIE: &str = "Brief_DLC05";
const R_TEXTS: &str = "DisDLC05MoviePlayerResultsMenu_Texts";
const B_TEXTS: &str = "DisGFxMoviePlayerBase_Texts";
/// `FadeToBlack`: the veil's time, and the wait after
const FADE: f32 = 0.85;
const FADE_HOLD: f32 = 0.25;
/// `R_ResultsScreen_Stats_List._offsetY`
const ROW_GAP: f32 = 5.0;
/// the stats list's rows shown at once (its mask's height)
const ROWS_SHOWN: usize = 9;

type Later = Box<dyn FnOnce(&mut FlashClip) + Send + Sync>;

/// A clip of the results with what it keeps: its instances as placed (`_props`) and its
/// delayed animations.
#[derive(Component)]
struct Part {
    kind: PartKind,
    saved: HashMap<String, Props>,
    later: Vec<(f32, Later)>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum PartKind {
    Backdrop,
    Screen,
    Medal,
    Next,
}

impl Part {
    fn saved(&self, path: &str) -> Props {
        self.saved.get(path).copied().unwrap_or(Props { x: 0.0, y: 0.0, xscale: 1.0, yscale: 1.0, rotation: 0.0, alpha: 1.0 })
    }

    fn save(&mut self, fc: &FlashClip, path: &str) {
        if let Some(p) = fc.props(path) {
            self.saved.insert(path.to_string(), p);
        }
    }

    /// One of `MultipleDelayedAnimations`' animations: hidden at its start properties until
    /// its delay, then shown and eased back to as placed.
    fn delayed(&mut self, fc: &mut FlashClip, path: &str, delay: f32, from: PropsTo, secs: f32, ease: Ease) {
        fc.tween_end(path, false);
        fc.set(path, from);
        fc.set_visible(path, false);
        let back: PropsTo = self.saved(path).into();
        let p = path.to_string();
        self.after(delay, move |fc| {
            fc.set_visible(&p, true);
            fc.tween(&p, back, secs, ease);
        });
    }

    fn after(&mut self, secs: f32, f: impl FnOnce(&mut FlashClip) + Send + Sync + 'static) {
        self.later.push((secs, Box::new(f)));
    }
}

fn to() -> PropsTo {
    PropsTo::default()
}

/// The shown results: what was run, and where the screen is at.
#[derive(Resource)]
struct Results {
    t: f32,
    stage: Stage,
    root: Entity,
    veil: Entity,
    /// the stats' rows, the first shown
    rows: usize,
    scroll: usize,
    /// the next screen's choice
    sel: usize,
    hover: Option<usize>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Stage {
    /// going black
    Fading,
    /// the screen opening, then shown (input once open)
    Opening,
    Shown,
    /// closing for the next screen
    Closing,
    Next,
}

#[derive(Component)]
struct Veil;

/// A choice of the next screen (its button over the item).
#[derive(Component)]
struct NextButton(usize);

/// The next screen's choices (`R_Next_Screen.SetNextMenu`, leaderboards aside: no online
/// scores).
const NEXT: [(&str, &str); 3] = [(R_TEXTS, "t_ReplayChallenge"), (B_TEXTS, "t_ExitChallenge"), (B_TEXTS, "t_ExitDLC05")];

/// The run is over: the veil comes down.
fn open(mut commands: Commands, ch: Res<Challenge>, results: Option<Res<Results>>) {
    if ch.ended.is_none() || results.is_some() {
        return;
    }
    let root = commands.spawn((Node { position_type: PositionType::Absolute, width: percent(100), height: percent(100), ..default() }, GlobalZIndex(70), Pickable::IGNORE, DespawnOnExit(GameState::InGame))).id();
    let veil = commands.spawn((Veil, Node { position_type: PositionType::Absolute, width: percent(100), height: percent(100), ..default() }, BackgroundColor(Color::BLACK.with_alpha(0.0)), ZIndex(10), Pickable::IGNORE, ChildOf(root))).id();
    commands.insert_resource(Results { t: 0.0, stage: Stage::Fading, root, veil, rows: 0, scroll: 0, sel: 0, hover: None });
}

#[allow(clippy::too_many_arguments)]
fn run(
    mut commands: Commands,
    results: Option<ResMut<Results>>,
    ch: Res<Challenge>,
    profile: Res<ChallengeProfile>,
    level: Option<Res<crate::level::LevelInfo>>,
    mut parts: Query<(&mut Part, &mut FlashClip)>,
    mut veils: Query<&mut BackgroundColor, With<Veil>>,
    (mut timelines, data, time, window): (ResMut<MovieTimelines>, Res<crate::gamedata::Data>, Res<Time<Real>>, Query<&Window>),
    (mut ui, mut images): (ResMut<crate::ui_images::UiImages>, ResMut<Assets<Image>>),
    mut paused: ResMut<crate::hud::Paused>,
    mut sfx: MessageWriter<crate::audio::PostEvent>,
) {
    let Some(mut r) = results else { return };
    let dt = time.delta_secs();
    r.t += dt;
    // the parts' delayed animations
    for (mut part, mut fc) in &mut parts {
        if part.later.is_empty() {
            continue;
        }
        for l in part.later.iter_mut() {
            l.0 -= dt;
        }
        let (due, rest): (Vec<_>, Vec<_>) = std::mem::take(&mut part.later).into_iter().partition(|l| l.0 <= 0.0);
        part.later = rest;
        for (_, f) in due {
            f(&mut fc);
        }
    }
    let Ok(mut veil) = veils.get_mut(r.veil) else { return };
    match r.stage {
        Stage::Fading => {
            veil.0 = Color::BLACK.with_alpha(Ease::StrongOut.at(r.t / FADE));
            if r.t < FADE + FADE_HOLD {
                return;
            }
            // `FadeFromBlack`
            paused.0 = true;
            let Ok(w) = window.single() else { return };
            let (Some(tl), Some(mtl)) = (timelines.get(MOVIE), timelines.get(MEDAL_MOVIE)) else { return };
            sfx.write(crate::audio::PostEvent::named(if ch.ended == Some(false) { "UI_R_DLC05_Success" } else { "UI_R_DLC05_Fail" }, None));
            let rows = spawn_screen(&mut commands, r.root, &tl, &mtl, w, &ch, &profile, level.as_deref(), &data, &mut ui, &mut images);
            r.rows = rows;
            r.stage = Stage::Opening;
            r.t = 0.0;
        }
        Stage::Opening | Stage::Shown => {
            veil.0 = Color::BLACK.with_alpha(1.0 - Ease::StrongOut.at(r.t / 0.2));
            if r.stage == Stage::Opening && r.t >= 0.65 + 0.65 {
                r.stage = Stage::Shown;
            }
            // the stats list scrolled
            let (rows, scroll) = (r.rows, r.scroll);
            if let Some((_, mut fc)) = parts.iter_mut().find(|(p, _)| p.kind == PartKind::Screen) {
                stats_scroll(&mut fc, rows, scroll);
            }
        }
        Stage::Closing => {
            if r.t >= 0.15 {
                r.stage = Stage::Next;
                r.t = 0.0;
                if let Some((mut part, mut fc)) = parts.iter_mut().find(|(p, _)| p.kind == PartKind::Next) {
                    next_open(&mut commands, r.root, &mut part, &mut fc, &data, window.single().ok());
                }
            }
        }
        Stage::Next => {
            let (sel, hover) = (r.sel, r.hover);
            if let Some((_, mut fc)) = parts.iter_mut().find(|(p, _)| p.kind == PartKind::Next) {
                next_highlight(&mut fc, sel, hover);
            }
        }
    }
}

/// `R_ResultsScreen.ShowResults`, `FadeFromBlack`, `Open`: the backdrop and the screen, its
/// parts set and their opening queued. The stats' rows.
#[allow(clippy::too_many_arguments)]
fn spawn_screen(
    commands: &mut Commands,
    root: Entity,
    tl: &std::sync::Arc<dhcook::format::Timelines>,
    mtl: &std::sync::Arc<dhcook::format::Timelines>,
    w: &Window,
    ch: &Challenge,
    profile: &ChallengeProfile,
    level: Option<&crate::level::LevelInfo>,
    data: &crate::gamedata::Data,
    ui: &mut crate::ui_images::UiImages,
    images: &mut Assets<Image>,
) -> usize {
    let s = (w.width() / 1280.0).min(w.height() / 720.0).max(0.01);
    let off = (Vec2::new(w.width(), w.height()) - Vec2::new(1280.0, 720.0) * s) * 0.5;
    let def = ch.def.clone().unwrap_or_default();
    let success = ch.ended == Some(false);
    let node = |x: f32, y: f32| Node { position_type: PositionType::Absolute, left: Val::Px(off.x + x * s), top: Val::Px(off.y + y * s), ..default() };
    let place = |fc: &mut FlashClip, x: f32, y: f32| {
        fc.scale = s;
        fc.m = [1.0, 0.0, 0.0, 1.0, x, y];
    };
    // the challenge's backdrop (`ImgLoader` into `bkgd_mc.img_mc`, grown 2%)
    let art = match def.id.as_str() {
        "Countdown" => "ResultsBackground_CountDown".to_string(),
        id => format!("ResultsBackground_{id}"),
    };
    if let Some((h, _)) = ui.file(images, "dlc05", &art) {
        let (iw, ih) = (1280.0 * 1.02, 720.0 * 1.02);
        let p = off + (Vec2::new(640.0, 360.0) - Vec2::new(iw, ih) * 0.5) * s;
        commands.spawn((ImageNode::new(h), Node { position_type: PositionType::Absolute, left: Val::Px(p.x), top: Val::Px(p.y), width: Val::Px(iw * s), height: Val::Px(ih * s), ..default() }, Pickable::IGNORE, ChildOf(root)));
    }
    // (`bkgd_mc`: the blades, shards and black over the picture)
    if tl.sprites.contains_key(&233) {
        let mut fc = FlashClip::new(MOVIE, tl.clone(), Clip::new(tl, 233)).real();
        place(&mut fc, 0.0, 0.0);
        fc.set_visible("img_mc", false);
        commands.spawn((Part { kind: PartKind::Backdrop, saved: HashMap::new(), later: Vec::new() }, fc, node(640.0, 360.0), Pickable::IGNORE, ChildOf(root)));
    }
    // the screen (`R_ResultsScreen`, attached at the middle)
    let Some(clip) = Clip::export(tl, "R_ResultsScreen") else { return 0 };
    let mut fc = FlashClip::new(MOVIE, tl.clone(), clip).real().with_texts();
    place(&mut fc, 0.0, 0.0);
    let mut part = Part { kind: PartKind::Screen, saved: HashMap::new(), later: Vec::new() };
    // `SetStats`: the stats list attached (`R_ResultsScreen_Stats_List` at -185, -145), its
    // rows (`R_Stats_item`, 5 apart, each its next frame's backing)
    let rows = stats_rows(ch, level);
    fc.attach("_stats_mc", "R_ResultsScreen_Stats_List", "_stats_mc", [1.0, 0.0, 0.0, 1.0, -185.0, -145.0]);
    fc.create_empty("_stats_mc._stats_mc", "_stats_mc", IDENTITY);
    fc.set_visible("_stats_mc._stats_mc._mask_mc", false);
    fc.goto_frame("_stats_mc._stats_mc._stroke_mc", if success { 1 } else { 2 }, false);
    let mut y = 0.0;
    for (i, (name, value)) in rows.iter().enumerate() {
        let p = format!("_stats_mc._stats_mc._stats_mc.stats{i}");
        fc.attach("_stats_mc._stats_mc._stats_mc", "R_Stats_item", &format!("stats{i}"), [1.0, 0.0, 0.0, 1.0, 0.0, y]);
        fc.goto_frame(&p, 1 + i % 2, false);
        fc.set_text(&format!("{p}.txt_name"), name.clone());
        fc.set_text(&format!("{p}.txt_value"), value.clone());
        fc.autosize.insert(format!("{p}.txt_value"), 1);
        // (its height its line's: `txt_name`'s box, 28.8 and its gutter, over its backing strip)
        let h = fc.size(&p).map_or(0.0, |v| v.y).max(30.8);
        y += h + ROW_GAP;
    }
    // the doll (`SetDoll`): found in green, not in red
    let found = profile.dolls.contains(&ch.map.to_ascii_lowercase());
    fc.set_text("_stats_mc._doll_mc._title_mc.txt", data.text(R_TEXTS, "t_EmilyDoll").to_uppercase());
    let (word, color) = if found { (data.text(R_TEXTS, "t_Found"), "#9bbf36") } else { (data.text(R_TEXTS, "t_NotFound"), "#971618") };
    fc.set_text("_stats_mc._doll_mc._txt_mc.txt", format!("<font color=\"{color}\">{word}</font>"));
    // `SetScore`
    let new_record = success && ch.score > ch.prev_best;
    let pts = data.text(B_TEXTS, "t_Pts");
    if success {
        fc.set_text("_score_mc._score_mc.txt_mc.txt_name", data.text(R_TEXTS, "t_FinalScore"));
        fc.set_text("_score_mc._score_mc.txt_mc.txt_value", format!("{} {pts}", ch.score));
        if !new_record {
            fc.set_text("_score_mc._bestScore_mc.txt_mc.txt_name", data.text(R_TEXTS, "t_BestScore"));
            fc.set_text("_score_mc._bestScore_mc.txt_mc.txt_value", format!("{} {pts}", ch.prev_best));
            let medals = medals_of(&def, ch.expert);
            let best_level = medals.iter().filter(|m| **m > 0 && ch.prev_best >= **m as i64).count();
            for i in 0..3 {
                fc.goto(&format!("_score_mc._bestScore_mc.txt_mc.mc{i}"), if i < best_level { "on" } else { "off" }, false);
            }
        }
        // (`R_ResultsScreen_Score_Record`: its words, its backing as wide as they are and 150)
        let rec = "_score_mc._record_mc._txt_mc.txt";
        fc.set_text(rec, data.text(R_TEXTS, "t_NewRecord").to_uppercase());
        fc.autosize.insert(rec.into(), 2);
        fc.wrap.insert(rec.into(), false);
        let tw = fc.text_width(rec).unwrap_or(100.0);
        set_width(&mut fc, "_score_mc._record_mc._bkgd_mc", tw + 150.0);
    } else {
        fc.set_text("_score_mc._fail_mc.txt", ch.last_title.clone().unwrap_or_default());
    }
    // the time (minutes' seconds" hundredths)
    let t = ch.elapsed.max(0.0);
    let hun = (t * 100.0).floor() as u64;
    fc.set_text("_timer_mc.txt", format!("{:02}\u{2019}{:02}\u{201d}{:02}", hun / 6000, (hun / 100) % 60, hun % 100));
    // the title (`OpenTitle`: the name, the mode, the kind's icon)
    fc.set_text("_title_mc._txt_mc.txt", def.name.to_uppercase());
    let mode = if ch.expert { data.text(B_TEXTS, "t_ExpertMode") } else { data.text(B_TEXTS, "t_NormalMode") };
    fc.set_text("_title_mc._txt_mc.txt_difficulty", mode.to_uppercase());
    let kind = match def.kind.as_str() {
        "DDCT_Mobility" => 1,
        "DDCT_Stealth" => 2,
        "DDCT_Action" => 3,
        "DDCT_Puzzle" => 4,
        _ => 1,
    };
    fc.goto_frame("_title_mc._ic_mc", kind, false);
    for p in [
        "_timer_mc",
        "_circle_mc",
        "_stats_mc",
        "_score_mc",
        "_sep0_mc",
        "_sep1_mc",
        "_title_mc",
        "_title_mc._ic_mc",
        "_stats_mc._bkgd_mc",
        "_stats_mc._stats_mc",
        "_stats_mc._fail_mc",
        "_stats_mc._doll_mc",
        "_stats_mc._doll_mc._doll_mc",
        "_stats_mc._doll_mc._title_mc",
        "_stats_mc._doll_mc._stroke_mc",
        "_stats_mc._doll_mc._txt_mc",
        "_stats_mc._doll_mc._ic_mc",
        "_score_mc._bkgd_mc",
        "_score_mc._score_mc",
        "_score_mc._bestScore_mc",
        "_score_mc._fail_mc",
        "_score_mc._record_mc",
        "_score_mc._record_mc._bkgd_mc",
        "_score_mc._record_mc._sealBkgd_mc",
        "_score_mc._record_mc._sealStroke_mc",
        "_score_mc._record_mc._txt_mc",
    ] {
        part.save(&fc, p);
    }
    for i in 0..rows.len() {
        part.save(&fc, &format!("_stats_mc._stats_mc._stats_mc.stats{i}"));
    }
    // `Open`: what comes when
    let sep0x = part.saved("_sep0_mc").x;
    part.delayed(&mut fc, "_timer_mc", 0.125, to().rotation(-10.0).scale(2.0).alpha(0.0), 0.25, Ease::BackInOut);
    part.delayed(&mut fc, "_circle_mc", 0.255, to().rotation(80.0).yscale(3.0).xscale(2.0).alpha(0.0), 0.3, Ease::BackInOut);
    part.delayed(&mut fc, "_stats_mc", 0.15, to().scale(2.0).alpha(0.0), 0.25, Ease::StrongOut);
    part.delayed(&mut fc, "_score_mc", 0.65, to().scale(2.0).alpha(0.0), 0.25, Ease::StrongOut);
    part.delayed(&mut fc, "_sep0_mc", 0.13, to().rotation(-25.0).x(sep0x - 650.0).alpha(0.0), 0.2, Ease::BackInOut);
    part.delayed(&mut fc, "_sep1_mc", 0.55, to().rotation(20.0).x(sep0x + 650.0).alpha(0.0), 0.25, Ease::BackInOut);
    // `OpenTitle` (its icon turning in, then the words)
    fc.set_visible("_title_mc", false);
    let ic: PropsTo = part.saved("_title_mc._ic_mc").into();
    part.after(0.015, move |fc| {
        fc.set_visible("_title_mc", true);
        fc.set("_title_mc._ic_mc", to().xscale(2.5).yscale(3.0).rotation(-35.0).alpha(0.0));
        fc.tween("_title_mc._ic_mc", ic, 0.25, Ease::BackInOut);
    });
    // `OpenStats`: its backing, the list, the medal (or the failure), the doll
    for p in ["_stats_mc._fail_mc", "_stats_mc._stats_mc"] {
        fc.set_visible(p, false);
    }
    part.delayed(&mut fc, "_stats_mc._bkgd_mc", 0.15 + 0.001, to().rotation(20.0).yscale(2.0).xscale(1.5).alpha(0.0), 0.25, Ease::BackInOut);
    part.delayed(&mut fc, "_stats_mc._stats_mc", 0.15 + 0.05, to().scale(3.5).alpha(0.0), 0.25, Ease::BackInOut);
    if !success {
        part.after(0.15 + 0.15, |fc| fc.set_visible("_stats_mc._fail_mc", true));
    }
    // (`R_ResultsScreen_Stats_Doll.Open`)
    let d = "_stats_mc._doll_mc";
    fc.set_visible(d, false);
    part.after(0.15 + 0.1, |fc| fc.set_visible("_stats_mc._doll_mc", true));
    let big = to().rotation(20.0).yscale(2.0).xscale(1.5).alpha(0.0);
    part.delayed(&mut fc, &format!("{d}._doll_mc"), 0.25 + 0.02, big, 0.2, Ease::BackInOut);
    part.delayed(&mut fc, &format!("{d}._title_mc"), 0.25 + 0.035, to().rotation(-20.0).yscale(4.0).xscale(2.5).alpha(0.0), 0.35, Ease::BackInOut);
    part.delayed(&mut fc, &format!("{d}._stroke_mc"), 0.25 + 0.001, to().rotation(20.0).yscale(5.5).xscale(4.0).alpha(0.0), 0.2, Ease::BackInOut);
    part.delayed(&mut fc, &format!("{d}._txt_mc"), 0.25 + 0.17, big, 0.2, Ease::BackInOut);
    part.delayed(&mut fc, &format!("{d}._ic_mc"), 0.25 + 0.1, big, 0.2, Ease::BackInOut);
    let state = if found { "completed" } else { "failed" };
    part.after(0.25 + 0.12, move |fc| {
        fc.goto("_stats_mc._doll_mc._ic_mc", state, false);
        fc.goto("_stats_mc._doll_mc._ic_mc.anim_mc", state, true);
    });
    // `R_ResultsScreen_Score.Open`: the final score and the best (or the record), or the
    // failure
    let sc = "_score_mc";
    for p in ["_bkgd_mc", "_bestScore_mc", "_record_mc", "_score_mc", "_fail_mc"] {
        fc.set_visible(&format!("{sc}.{p}"), false);
    }
    part.delayed(&mut fc, &format!("{sc}._bkgd_mc"), 0.65 + 0.001, to().rotation(-20.0).yscale(2.0).xscale(1.5).alpha(0.0), 0.2, Ease::BackInOut);
    if success {
        part.delayed(&mut fc, &format!("{sc}._score_mc"), 0.65 + 0.015, to().rotation(45.0).yscale(2.0).xscale(1.5).alpha(0.0), 0.3, Ease::BackInOut);
        if new_record {
            // (`R_ResultsScreen_Score_Record.Open`)
            let rc = format!("{sc}._record_mc");
            part.after(0.65 + 0.15, |fc| fc.set_visible("_score_mc._record_mc", true));
            let t0 = 0.8;
            part.delayed(&mut fc, &format!("{rc}._bkgd_mc"), t0 + 0.15, big, 0.2, Ease::BackInOut);
            part.delayed(&mut fc, &format!("{rc}._sealBkgd_mc"), t0 + 0.015, to().rotation(-20.0).yscale(4.0).xscale(2.5).alpha(0.0), 0.35, Ease::BackInOut);
            part.delayed(&mut fc, &format!("{rc}._sealStroke_mc"), t0 + 0.005, to().rotation(20.0).yscale(5.5).xscale(4.0).alpha(0.0), 0.2, Ease::BackInOut);
            part.delayed(&mut fc, &format!("{rc}._txt_mc"), t0 + 0.17, big, 0.2, Ease::BackInOut);
        } else {
            part.after(0.65 + 0.15, |fc| fc.set_visible("_score_mc._bestScore_mc", true));
        }
    } else {
        part.delayed(&mut fc, &format!("{sc}._fail_mc"), 0.65 + 0.001, to().rotation(-20.0).yscale(2.0).xscale(1.5).alpha(0.0), 0.2, Ease::BackInOut);
    }
    commands.spawn((part, fc, node(640.0, 360.0), Pickable::IGNORE, ChildOf(root)));
    // the medal (`_DLC05.ChallengeMedal`, in `_stats_mc._medal_mc`): the score, the stars won
    if success {
        if let Some(clip) = Clip::export(mtl, "DLC05_CM_Medal") {
            let mut fc = FlashClip::new(MEDAL_MOVIE, mtl.clone(), clip).real().with_texts();
            place(&mut fc, 0.0, 0.0);
            let mut part = Part { kind: PartKind::Medal, saved: HashMap::new(), later: Vec::new() };
            for p in ["_circle_mc", "_score_mc", "_stars_mc", "_stars_mc.mc0", "_stars_mc.mc1", "_stars_mc.mc2", "_score_mc.txt_mc"] {
                part.save(&fc, p);
            }
            let medals = medals_of(&def, ch.expert);
            let level_won = medals.iter().filter(|m| **m > 0 && ch.score >= **m as i64).count();
            // `SetDisplay`: the score and its unit side by side, centred; the stars on or off
            fc.set_text("_score_mc.txt_mc.txt_score", ch.score.to_string());
            fc.set_text("_score_mc.txt_mc.txt_pts", pts.clone());
            fc.autosize.insert("_score_mc.txt_mc.txt_score".into(), 0);
            fc.autosize.insert("_score_mc.txt_mc.txt_pts".into(), 0);
            let sw = fc.text_width("_score_mc.txt_mc.txt_score").unwrap_or(80.0);
            let pw = fc.text_width("_score_mc.txt_mc.txt_pts").unwrap_or(40.0);
            let sx = fc.props("_score_mc.txt_mc.txt_score").map_or(2.0, |p| p.x);
            fc.set("_score_mc.txt_mc.txt_pts", to().x(sx + sw + 2.5));
            fc.set("_score_mc.txt_mc", to().x(-0.5 * (sw + 2.5 + pw)));
            part.save(&fc, "_score_mc.txt_mc");
            for i in 0..3 {
                fc.goto(&format!("_stars_mc.mc{i}"), if i < level_won { "on" } else { "off" }, false);
            }
            // `Open` (after the stats' 0.15 and its own 0.15)
            let t0 = 0.3;
            for p in ["_circle_mc", "_score_mc", "_stars_mc", "_stars_mc.mc0", "_stars_mc.mc1", "_stars_mc.mc2"] {
                fc.set_visible(p, false);
            }
            part.delayed(&mut fc, "_circle_mc", t0 + 0.005, to().alpha(0.0).xscale(2.5).yscale(3.5).rotation(-50.0), 0.25, Ease::BackInOut);
            part.delayed(&mut fc, "_score_mc", t0 + 0.15, to().alpha(0.0).scale(2.5).rotation(-50.0), 0.25, Ease::BackInOut);
            part.delayed(&mut fc, "_stars_mc", t0 + 0.05, to().alpha(0.0).scale(2.5).rotation(25.0), 0.25, Ease::BackInOut);
            for i in 0..3 {
                part.delayed(&mut fc, &format!("_stars_mc.mc{i}"), t0 + 0.15 + 0.05 * i as f32, to().alpha(0.0).xscale(2.5).yscale(3.0).rotation(25.0), 0.25, Ease::BackInOut);
            }
            // (where `_medal_mc` is: the stats at 0, 4.3; it at -353.8, -20.6)
            commands.spawn((part, fc, node(640.0 - 353.8, 360.0 + 4.3 - 20.6), Pickable::IGNORE, ChildOf(root)));
        }
    }
    // the next screen (`nextScreen_mc`, turned -3 degrees; shown later)
    if tl.sprites.contains_key(&209) {
        let mut fc = FlashClip::new(MOVIE, tl.clone(), Clip::new(tl, 209)).real().with_texts();
        place(&mut fc, 0.0, 0.0);
        fc.m = crate::flash::turn_scale(-3.0, 1.0, 1.0);
        fc.set_visible("", false);
        commands.spawn((Part { kind: PartKind::Next, saved: HashMap::new(), later: Vec::new() }, fc, node(640.0, 360.0), Pickable::IGNORE, ChildOf(root)));
    }
    // the vignette over it all
    if let Some(clip) = Clip::export(tl, "m_Vignette") {
        let mut fc = FlashClip::new(MOVIE, tl.clone(), clip).real();
        place(&mut fc, 0.0, 0.0);
        commands.spawn((fc, node(640.0, 360.0), Pickable::IGNORE, ChildOf(root)));
    }
    rows.len()
}

/// `_width = w` (its scale to make it so).
fn set_width(fc: &mut FlashClip, path: &str, w: f32) {
    if let (Some(size), Some(p)) = (fc.size(path), fc.props(path)) {
        if size.x > 1e-3 && p.xscale.abs() > 1e-6 {
            fc.set(path, to().xscale(w / (size.x / p.xscale.abs())));
        }
    }
}

fn medals_of(def: &dhcook::format::ChallengeDef, expert: bool) -> [i32; 3] {
    if expert && def.expert_medals.iter().any(|m| *m > 0) {
        def.expert_medals
    } else {
        def.medals
    }
}

/// The scoring rule set's results statistics (`m_ResultsMenuStats`), with their values from
/// the run: how often an entry scored (`DDSL_ScoringHistoryCount`), the kills
/// (`DDSL_Custom_WaveKillCount`), the time (`DDSL_Custom_BestTime`...); each once.
fn stats_rows(ch: &Challenge, level: Option<&crate::level::LevelInfo>) -> Vec<(String, String)> {
    let Some(set) = level.and_then(|l| l.scene.challenge_rules.iter().find(|r| Some(r.name.as_str()) == ch.rules.as_deref())) else { return Vec::new() };
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    let time = |t: f32| {
        let hun = (t.max(0.0) * 100.0).floor() as u64;
        format!("{:02}\u{2019}{:02}\u{201d}{:02}", hun / 6000, (hun / 100) % 60, hun % 100)
    };
    for (lookup, name) in &set.stats {
        if !seen.insert((lookup.clone(), name.clone())) {
            continue;
        }
        let value = match lookup.as_str() {
            "DDSL_ScoringHistoryCount" => ch.history.iter().filter(|(e, _)| e.eq_ignore_ascii_case(name)).count().to_string(),
            "DDSL_Custom_WaveKillCount" => ch.kills.to_string(),
            "DDSL_Custom_BestTime" | "DDSL_Custom_ArenaLastWaveDuration" => time(ch.elapsed),
            "DDSL_ScoringStorageCount" => ch.storage.get(name).copied().unwrap_or(0).to_string(),
            "DDSL_Custom_ThiefCoins" => ch.items.get("DDHI_CoinCount").map_or(0, |i| i.value).to_string(),
            "DDSL_Custom_BestNumberOfCheckpoints" => ch.items.get("DDHI_GateCount").map_or(0, |i| i.value).to_string(),
            _ => "0".into(),
        };
        out.push((name.clone(), value));
    }
    out
}

/// The stats list scrolled (its mask shows so many rows; the rest hidden).
fn stats_scroll(fc: &mut FlashClip, rows: usize, scroll: usize) {
    if rows == 0 {
        return;
    }
    let first = scroll.min(rows.saturating_sub(ROWS_SHOWN));
    let mut y0 = None;
    for i in 0..rows {
        let p = format!("_stats_mc._stats_mc._stats_mc.stats{i}");
        let shown = i >= first && i < first + ROWS_SHOWN;
        if fc.visible(&p) != shown {
            fc.set_visible(&p, shown);
        }
        if i == first {
            y0 = fc.props(&p).map(|p| p.y);
        }
    }
    let y = -y0.unwrap_or(0.0);
    if fc.props("_stats_mc._stats_mc._stats_mc").is_some_and(|p| (p.y - y).abs() > 0.01) {
        fc.set("_stats_mc._stats_mc._stats_mc", to().y(y));
    }
}

/// `R_Next_Screen.Open`: its parts in, the menu's choices (`R_Next_Menu_item`, 2 apart,
/// centred on the menu; upper case), their buttons.
fn next_open(commands: &mut Commands, root: Entity, part: &mut Part, fc: &mut FlashClip, data: &crate::gamedata::Data, w: Option<&Window>) {
    fc.set_visible("", true);
    for p in ["_bkgd_mc", "_sep0_mc", "_sep1_mc", "_sep2_mc", "_stroke_mc", "_menu_mc"] {
        part.save(fc, p);
    }
    let items: Vec<String> = NEXT.iter().map(|(sec, key)| data.text(sec, key).to_uppercase()).collect();
    let mut h = 42.0;
    for (i, t) in items.iter().enumerate() {
        fc.attach("_menu_mc", "R_Next_Menu_item", &format!("btn{i}"), IDENTITY);
        let p = format!("_menu_mc.btn{i}");
        fc.set_visible(&format!("{p}.btn"), false);
        fc.set_text(&format!("{p}._txt_mc.txt"), t.clone());
        if i == 0 {
            h = fc.size(&p).map_or(42.0, |v| v.y).clamp(20.0, 80.0);
        }
    }
    let total = items.len() as f32 * (h + 2.0) - 2.0;
    for i in 0..items.len() {
        let y = i as f32 * (h + 2.0) + 0.5 * h - 0.5 * total;
        fc.set(&format!("_menu_mc.btn{i}"), to().y(y).x(0.0));
        part.save(fc, &format!("_menu_mc.btn{i}"));
    }
    let b = part.saved("_bkgd_mc");
    part.delayed(fc, "_bkgd_mc", 0.2, to().x(b.x - 450.0).alpha(0.0), 0.25, Ease::StrongOut);
    for (p, dx, d, secs) in [("_sep0_mc", -150.0, 0.005, 0.2), ("_sep1_mc", -250.0, 0.01, 0.2), ("_sep2_mc", 150.0, 0.025, 0.2)] {
        let x = part.saved(p).x;
        part.delayed(fc, p, d, to().x(x + dx).alpha(0.0), secs, Ease::BackInOut);
    }
    part.delayed(fc, "_stroke_mc", 0.15, to().rotation(20.0).yscale(3.5).xscale(2.5).alpha(0.0), 0.25, Ease::BackInOut);
    for i in 0..items.len() {
        part.delayed(fc, &format!("_menu_mc.btn{i}"), 0.15 + 0.05 * i as f32, to().x(-60.0).alpha(0.0), 0.25, Ease::BackOut);
    }
    // the choices' buttons (over the items: 380 x 42 about their middles)
    let Some(w) = w else { return };
    let s = (w.width() / 1280.0).min(w.height() / 720.0).max(0.01);
    let off = (Vec2::new(w.width(), w.height()) - Vec2::new(1280.0, 720.0) * s) * 0.5;
    let m = crate::flash::turn_scale(-3.0, 1.0, 1.0);
    let menu = part.saved("_menu_mc");
    for i in 0..items.len() {
        let y = menu.y + i as f32 * (h + 2.0) + 0.5 * h - 0.5 * total;
        let c = Vec2::new(m[0] * menu.x + m[2] * y, m[1] * menu.x + m[3] * y) + Vec2::new(640.0, 360.0);
        let (bw, bh) = (380.0, h);
        commands.spawn((
            NextButton(i),
            Button,
            Node { position_type: PositionType::Absolute, left: Val::Px(off.x + (c.x - bw * 0.5) * s), top: Val::Px(off.y + (c.y - bh * 0.5) * s), width: Val::Px(bw * s), height: Val::Px(bh * s), ..default() },
            ZIndex(5),
            ChildOf(root),
        ));
    }
}

/// The chosen item over (`_btnOverProps`: 30 along, `over`), the others out.
fn next_highlight(fc: &mut FlashClip, sel: usize, hover: Option<usize>) {
    let lit = hover.unwrap_or(sel);
    for i in 0..NEXT.len() {
        let p = format!("_menu_mc.btn{i}");
        let want = if i == lit { 30.0 } else { 0.0 };
        if let Some(pr) = fc.props(&p) {
            if (pr.x - want).abs() > 0.5 && !fc.tweening(&p) {
                fc.tween(&p, to().x(want), 0.25, if i == lit { Ease::BackOut } else { Ease::StrongOut });
                fc.goto(&p, if i == lit { "over" } else { "out" }, true);
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn input(
    mut commands: Commands,
    results: Option<ResMut<Results>>,
    keys: Res<ButtonInput<KeyCode>>,
    mouse: Res<ButtonInput<MouseButton>>,
    mut wheel: MessageReader<bevy::input::mouse::MouseWheel>,
    buttons: Query<(&Interaction, &NextButton)>,
    mut parts: Query<(&mut Part, &mut FlashClip)>,
    mut cursor: Single<&mut CursorOptions>,
    (mut next, mut config, mut launch, mut stats, mut paused): (ResMut<NextState<GameState>>, ResMut<crate::Config>, ResMut<ChallengeLaunch>, ResMut<crate::gameplay::PlayerStats>, ResMut<crate::hud::Paused>),
    ch: Res<Challenge>,
    mut sfx: MessageWriter<crate::audio::PostEvent>,
) {
    let Some(mut r) = results else { return };
    if r.stage == Stage::Fading {
        return;
    }
    paused.0 = true;
    if cursor.grab_mode != CursorGrabMode::None {
        cursor.visible = true;
        cursor.grab_mode = CursorGrabMode::None;
    }
    let ok = keys.just_pressed(KeyCode::Enter) || keys.just_pressed(KeyCode::Space) || keys.just_pressed(KeyCode::KeyE);
    match r.stage {
        Stage::Shown => {
            // the list scrolled; the screen closed (`APressed`)
            let mut d: i32 = 0;
            for ev in wheel.read() {
                d += if ev.y < 0.0 { 1 } else if ev.y > 0.0 { -1 } else { 0 };
            }
            if keys.just_pressed(KeyCode::ArrowDown) || keys.just_pressed(KeyCode::KeyS) {
                d += 1;
            }
            if keys.just_pressed(KeyCode::ArrowUp) || keys.just_pressed(KeyCode::KeyW) {
                d -= 1;
            }
            let max = r.rows.saturating_sub(ROWS_SHOWN) as i32;
            r.scroll = (r.scroll as i32 + d).clamp(0, max) as usize;
            if ok || mouse.just_pressed(MouseButton::Left) {
                sfx.write(crate::audio::PostEvent::named("UI_Validation", None));
                if let Some((_, mut fc)) = parts.iter_mut().find(|(p, _)| p.kind == PartKind::Screen) {
                    for p in ["_stats_mc", "_score_mc", "_sep0_mc", "_sep1_mc", "_circle_mc", "_timer_mc"] {
                        fc.tween_end(p, false);
                        fc.tween(p, to().alpha(0.0), 0.25, Ease::StrongOut);
                    }
                }
                if let Some((_, mut fc)) = parts.iter_mut().find(|(p, _)| p.kind == PartKind::Medal) {
                    fc.tween("", to().alpha(0.0), 0.25, Ease::StrongOut);
                }
                r.stage = Stage::Closing;
                r.t = 0.0;
            }
        }
        Stage::Next => {
            let n = NEXT.len();
            if keys.just_pressed(KeyCode::ArrowDown) || keys.just_pressed(KeyCode::KeyS) {
                r.sel = (r.sel + 1) % n;
                r.hover = None;
            }
            if keys.just_pressed(KeyCode::ArrowUp) || keys.just_pressed(KeyCode::KeyW) {
                r.sel = (r.sel + n - 1) % n;
                r.hover = None;
            }
            let mut chosen = ok.then_some(r.sel);
            let mut hover = None;
            for (i, b) in &buttons {
                match i {
                    Interaction::Hovered => hover = Some(b.0),
                    Interaction::Pressed => {
                        hover = Some(b.0);
                        chosen = Some(b.0);
                    }
                    _ => {}
                }
            }
            if hover.is_some() {
                r.hover = hover;
                r.sel = hover.unwrap_or(r.sel);
            }
            let Some(c) = chosen else { return };
            sfx.write(crate::audio::PostEvent::named("UI_R_DLC05_ResultsClose", None));
            commands.entity(r.root).despawn();
            commands.remove_resource::<Results>();
            paused.0 = false;
            stats.dead = false;
            stats.game_over = None;
            match c {
                // replay: the same challenge, the same mode
                0 => launch.expert = ch.expert,
                // the challenges, or the main menu
                1 => {
                    config.map = crate::menu::MENU_MAP.into();
                    launch.back_to_challenges = true;
                }
                _ => config.map = crate::menu::MENU_MAP.into(),
            }
            config.spawn_index = None;
            next.set(GameState::Loading);
        }
        _ => {}
    }
}
