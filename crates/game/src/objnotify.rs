//! The objectives popup (the HUD movie's `hud_objW`, class `ObjectivesNotification`,
//! disassembled) at the top right: the HUD tweak's title for what happened (`New Objective`,
//! `Objective Updated`, `Objective Completed`, `Objective Failed`: dark capitals on the pale
//! bar), the objective's name on the dark bar below, the lozenge and state mark beside them,
//! then its tasks one at a time beneath (each fading in with a glow of its state's colour and
//! its mark, 3.5 s each, the last fading as the next comes). It all comes in as the class
//! tweens it (backing, bars and rules sliding in, lozenges spinning down from 150% / 300%,
//! the mark from 250%) and goes the same way; each plays the UI sound theme's `obj_*` sound.
//! Popups queue one after another.

use crate::flash::{concat, turn_scale, Clip, FlashClip, Mat, MovieTimelines};
use crate::GameState;
use bevy::prelude::*;
use bevy::window::PrimaryWindow;
use std::collections::VecDeque;

pub struct ObjNotifyPlugin;

impl Plugin for ObjNotifyPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<ObjectiveQueue>()
            .init_resource::<Popup>()
            .add_systems(OnEnter(GameState::InGame), |mut p: ResMut<Popup>, mut q: ResMut<ObjectiveQueue>| {
                *p = Popup::default();
                q.0.clear();
            })
            .add_systems(Update, play_popup.run_if(in_state(GameState::InGame)));
    }
}

/// `EDisUIObjectiveEvent` / `EDisUITaskEvent`
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ObjState {
    Added,
    Updated,
    Completed,
    Failed,
}

/// What happened to an objective, and to which of its tasks.
#[derive(Clone, Debug)]
pub struct ObjectiveNotice {
    pub name: String,
    pub state: ObjState,
    pub tasks: Vec<(String, ObjState)>,
}

/// Popups to come.
#[derive(Resource, Default)]
pub struct ObjectiveQueue(pub VecDeque<ObjectiveNotice>);

impl ObjectiveQueue {
    /// Queue one; an update of the objective just queued joins it.
    pub fn push(&mut self, n: ObjectiveNotice) {
        if let Some(last) = self.0.back_mut() {
            if last.name == n.name && (last.state == n.state || n.state == ObjState::Updated && last.state == ObjState::Added) {
                for t in n.tasks {
                    if !last.tasks.contains(&t) {
                        last.tasks.push(t);
                    }
                }
                return;
            }
        }
        self.0.push_back(n);
    }
}

const MOVIE: &str = "HUD";
const SYMBOL: &str = "hud_objW";
/// `objectives_mc` (1184, 54) + `_window_mc` (95, -42): the window's origin (the clip
/// is `objectives_mc`, drawn from its own origin)
const AT: Vec2 = Vec2::new(1279.0, 12.0);
const WINDOW_IN_CLIP: Vec2 = Vec2::new(95.0, -42.0);
/// `_tasksApparitionDelay`, `_taskLifeDuration_const`
const TASKS_AFTER: f32 = 0.7;
const TASK_LIFE: f32 = 3.5;
/// the title (`title_mc.txt_mc.txt`: the title font at 30, 1.2 high, right-aligned, dark, with
/// three pale copies about it), the name (`name_mc.txt_mc.txt`: 24, pale), a task
/// (`hud_objW_tasksList_item`: the body font at 24, pale, wrapping in 486)
const TITLE_RIGHT: f32 = -169.65;
const TITLE_TOP: f32 = 41.85;
const TITLE_SIZE: f32 = 30.0;
const TITLE_COPIES: [Vec2; 3] = [Vec2::new(2.25, 0.0), Vec2::new(-2.5, 1.5), Vec2::new(-1.5, 1.5)];
const NAME_RIGHT: f32 = -175.15;
const NAME_TOP: f32 = 85.3;
const NAME_SIZE: f32 = 24.0;
const TASK_RIGHT: f32 = -148.0;
const TASK_TOP: f32 = 129.0;
const TASK_SIZE: f32 = 24.0;
const TASK_W: f32 = 486.0;
/// the task's mark (`ic_mc`, at half size)
const TASK_MARK: Vec2 = Vec2::new(-127.1, 144.1);
const PALE: Color = Color::srgb(225.0 / 255.0, 240.0 / 255.0, 212.0 / 255.0);
const DARK: Color = Color::srgb(23.0 / 255.0, 25.0 / 255.0, 28.0 / 255.0);

/// `GetStateColor`: the glow's colour for a state
fn state_color(s: ObjState) -> Color {
    match s {
        ObjState::Added | ObjState::Updated => Color::srgb(227.0 / 255.0, 242.0 / 255.0, 214.0 / 255.0),
        ObjState::Completed => Color::srgb(74.0 / 255.0, 84.0 / 255.0, 15.0 / 255.0),
        ObjState::Failed => Color::srgb(130.0 / 255.0, 20.0 / 255.0, 21.0 / 255.0),
    }
}

/// `GetIconStateLabel`
fn state_label(s: ObjState) -> &'static str {
    match s {
        ObjState::Added | ObjState::Updated => "updated",
        ObjState::Completed => "completed",
        ObjState::Failed => "failed",
    }
}

/// `GetSoundID` through the HUD's sound theme
fn state_sound(s: ObjState) -> &'static str {
    match s {
        ObjState::Added | ObjState::Updated => "UI_Objective_New_Task",
        ObjState::Completed => "UI_Objective_Task_Succes",
        ObjState::Failed => "UI_Objective_Task_Fail",
    }
}

/// the HUD tweak's popup titles
fn title(s: ObjState) -> &'static str {
    match s {
        ObjState::Added => "New Objective",
        ObjState::Updated => "Objective Updated",
        ObjState::Completed => "Objective Completed",
        ObjState::Failed => "Objective Failed",
    }
}

fn strong_out(x: f32) -> f32 {
    1.0 - (1.0 - x.clamp(0.0, 1.0)).powi(5)
}

fn strong_in_out(x: f32) -> f32 {
    let x = x.clamp(0.0, 1.0);
    if x < 0.5 {
        16.0 * x.powi(5)
    } else {
        1.0 - (-2.0 * x + 2.0).powi(5) / 2.0
    }
}

/// From `from` to 1 (or the other way) `t` into `secs`.
fn k(t: f32, secs: f32, ease: fn(f32) -> f32) -> f32 {
    ease(if secs > 0.0 { t / secs } else { 1.0 })
}

#[derive(Resource, Default)]
struct Popup {
    root: Option<Entity>,
    playing: Option<(ObjectiveNotice, f32)>,
    /// `_window_mc`'s children's own matrices
    base: Vec<(&'static str, Mat)>,
    /// the task shown (index, since when)
    task: Option<(usize, f32)>,
}

#[derive(Component)]
struct PopupClip;
#[derive(Component)]
struct PopupText(Part);
#[derive(Component)]
struct TaskMark;

#[derive(Clone, Copy, PartialEq)]
enum Part {
    /// the title, and its pale copies
    Title(usize),
    Name,
    Task,
    /// the task's glow (a white copy in its state's colour)
    TaskGlow,
}

const CHILDREN: [&str; 8] = ["bkgd_mc", "name_mc", "title_mc", "lineD_mc", "lineU_mc", "lozengeExt_mc", "lozengeInt_mc", "objState_mc"];

#[allow(clippy::type_complexity, clippy::too_many_arguments)]
fn play_popup(
    mut commands: Commands,
    time: Res<Time>,
    mut popup: ResMut<Popup>,
    mut queue: ResMut<ObjectiveQueue>,
    mut timelines: ResMut<MovieTimelines>,
    window: Query<&Window, With<PrimaryWindow>>,
    hud: Query<Entity, With<crate::hud::HudRoot>>,
    fonts: Res<crate::ui_fonts::UiFonts>,
    mut nodes: Query<(&mut Node, &mut Visibility), (Without<PopupText>, Without<PopupClip>, Without<TaskMark>)>,
    mut clip: Query<&mut FlashClip, (With<PopupClip>, Without<TaskMark>)>,
    mut mark: Query<(&mut FlashClip, &mut Node), (With<TaskMark>, Without<PopupClip>)>,
    mut texts: Query<(&PopupText, &mut Text, &mut TextFont, &mut TextColor, &mut Node, &ComputedNode), (Without<PopupClip>, Without<TaskMark>)>,
    mut sounds: MessageWriter<crate::audio::PostEvent>,
    settings: Res<crate::settings::Settings>,
) {
    let dt = time.delta_secs();
    let Ok(w) = window.single() else { return };
    let s = (w.width() / 1280.0).min(w.height() / 720.0).max(0.01);
    let off = (Vec2::new(w.width(), w.height()) - Vec2::new(1280.0, 720.0) * s) * 0.5;
    // the next popup (`PSI_HUD_bShowObjectivePopups` off: none)
    if !settings.objective_popups {
        queue.0.clear();
    }
    if popup.playing.is_none() {
        let Some(n) = queue.0.pop_front() else { return };
        let (Ok(h), Some(tl)) = (hud.single(), timelines.get(MOVIE)) else { return };
        let Some(c) = Clip::export(&tl, SYMBOL) else { return };
        if let Some(r) = popup.root.take() {
            commands.entity(r).try_despawn();
        }
        popup.base = c.child("_window_mc").map(|wm| CHILDREN.iter().filter_map(|n| wm.placed(n).map(|p| (*n, p.0))).collect()).unwrap_or_default();
        let mark_clip = Clip::export(&tl, "hud_objW_tasksList_item").and_then(|i| i.child("ic_mc").cloned());
        let font = |size: f32, title: bool| TextFont { font: if title { fonts.title.clone().into() } else { Default::default() }, font_size: bevy::text::FontSize::Px(size * s), ..default() };
        let text = |part: Part, t: String, f: TextFont, c: Color| (PopupText(part), Text::new(t), f, TextColor(c.with_alpha(0.0)), TextLayout::new(Justify::Right, bevy::text::LineBreak::NoWrap), Node { position_type: PositionType::Absolute, ..default() }, Pickable::IGNORE);
        let title_text = title(n.state).to_uppercase();
        let name_text = n.name.to_uppercase();
        let root = commands
            .spawn((Node { position_type: PositionType::Absolute, ..default() }, Pickable::IGNORE, ChildOf(h)))
            .with_children(|r| {
                r.spawn((PopupClip, FlashClip::new(MOVIE, tl.clone(), c), Node { position_type: PositionType::Absolute, ..default() }, Pickable::IGNORE));
                for i in 0..TITLE_COPIES.len() {
                    r.spawn(text(Part::Title(i + 1), title_text.clone(), font(TITLE_SIZE, true), PALE));
                }
                r.spawn(text(Part::Title(0), title_text.clone(), font(TITLE_SIZE, true), DARK));
                r.spawn(text(Part::Name, name_text, font(NAME_SIZE, true), PALE));
                if let Some(m) = mark_clip {
                    r.spawn((TaskMark, FlashClip::new(MOVIE, tl.clone(), m), Node { position_type: PositionType::Absolute, ..default() }, Pickable::IGNORE));
                }
                let mut task = text(Part::Task, String::new(), font(TASK_SIZE, false), PALE);
                task.4 = TextLayout::new(Justify::Right, bevy::text::LineBreak::WordBoundary);
                r.spawn(task);
                let mut glow = text(Part::TaskGlow, String::new(), font(TASK_SIZE, false), Color::WHITE);
                glow.4 = TextLayout::new(Justify::Right, bevy::text::LineBreak::WordBoundary);
                r.spawn(glow);
            })
            .id();
        popup.root = Some(root);
        popup.task = None;
        if n.tasks.is_empty() {
            sounds.write(crate::audio::PostEvent::named(state_sound(n.state), None));
        }
        popup.playing = Some((n, 0.0));
        return;
    }
    let Some(root) = popup.root else { return };
    let (n, t) = {
        let p = popup.playing.as_mut().unwrap();
        p.1 += dt;
        (p.0.clone(), p.1)
    };
    // when it closes: after its tasks, or its life with none
    let close_at = if n.tasks.is_empty() { TASK_LIFE } else { TASKS_AFTER + TASK_LIFE * n.tasks.len() as f32 };
    let tc = t - close_at;
    if tc > 0.5 {
        commands.entity(root).try_despawn();
        popup.root = None;
        popup.playing = None;
        return;
    }
    if let Ok((mut node, mut v)) = nodes.get_mut(root) {
        let p = off + AT * s;
        node.left = Val::Px(p.x);
        node.top = Val::Px(p.y);
        *v = Visibility::Inherited;
    }
    // the task shown, and its sound as it comes
    let ti = if t >= TASKS_AFTER && !n.tasks.is_empty() { Some((((t - TASKS_AFTER) / TASK_LIFE) as usize).min(n.tasks.len() - 1)) } else { None };
    if ti.is_some() && popup.task.map(|p| p.0) != ti {
        let i = ti.unwrap();
        popup.task = Some((i, t));
        sounds.write(crate::audio::PostEvent::named(state_sound(n.tasks[i].1), None));
    }
    let task_t = popup.task.map(|p| t - p.1).unwrap_or(0.0);
    // the clip: each part as `Open` / `Close` tween it
    let base = popup.base.clone();
    let get = |name: &str| base.iter().find(|b| b.0 == name).map(|b| b.1).unwrap_or(crate::flash::IDENTITY);
    let closing = tc > 0.0;
    // (x offset, alpha, scale, turn) of a part
    let part = |name: &str| -> (f32, f32, f32, f32) {
        if closing {
            let (dx, scale, secs) = match name {
                "bkgd_mc" => (100.0, 1.0, 0.2),
                "name_mc" => (-50.0, 1.0, 0.25),
                "title_mc" => (50.0, 1.0, 0.25),
                "objState_mc" | "lozengeInt_mc" => (0.0, 1.2, 0.25),
                "lozengeExt_mc" => (0.0, 1.4, 0.25),
                "lineU_mc" => (-350.0, 1.0, 0.25),
                _ => (250.0, 1.0, 0.3),
            };
            let e = k(tc, secs, strong_out);
            return (dx * e, 1.0 - e, 1.0 + (scale - 1.0) * e, 0.0);
        }
        match name {
            "bkgd_mc" => {
                let e = k(t, 0.2, strong_out);
                (250.0 * (1.0 - e), e, 1.5 + (1.0 - 1.5) * e, 0.0)
            }
            "name_mc" => {
                let e = k(t, 0.25, strong_out);
                (850.0 * (1.0 - e), e, 1.0, 0.0)
            }
            "title_mc" => {
                let e = k(t, 0.3, strong_in_out);
                (-150.0 * (1.0 - e), e, 1.0, 0.0)
            }
            "lineU_mc" => {
                let e = k(t, 0.3, strong_in_out);
                (650.0 * (1.0 - e), e, 1.0, 0.0)
            }
            "lineD_mc" => {
                let e = k(t, 0.3, strong_out);
                (-250.0 * (1.0 - e), e, 1.0, 0.0)
            }
            "lozengeExt_mc" => {
                let e = k(t - 0.2, 0.25, strong_out);
                (0.0, if t >= 0.2 { 1.0 } else { 0.0 }, 1.5 + (1.0 - 1.5) * e, 50.0 * (1.0 - e))
            }
            "lozengeInt_mc" => {
                let e = k(t - 0.2, 0.25, strong_out);
                (0.0, if t >= 0.2 { 1.0 } else { 0.0 }, 3.0 + (1.0 - 3.0) * e, 50.0 * (1.0 - e))
            }
            _ => {
                let e = k(t, 0.25, strong_out);
                (0.0, e, 2.5 + (1.0 - 2.5) * e, 0.0)
            }
        }
    };
    if let Ok(mut fc) = clip.single_mut() {
        fc.scale = s;
        fc.m = [1.0, 0.0, 0.0, 1.0, -WINDOW_IN_CLIP.x, -WINDOW_IN_CLIP.y];
        let fc = &mut *fc;
        // the parts' own clips: the title and outer lozenge loop, the inner lozenge and the mark
        // play the state
        let label = state_label(n.state);
        let starts: [(&str, &str, f32); 4] = [("_window_mc.title_mc", "anim", 0.0), ("_window_mc.lozengeExt_mc", "anim", 0.2), ("_window_mc.lozengeInt_mc", label, 0.2), ("_window_mc.objState_mc", label, 0.1)];
        for (path, l, at) in starts {
            if t >= at && t - dt < at {
                fc.goto(path, l, true);
            }
        }
        if let Some(wm) = fc.clip.child_mut("_window_mc") {
            for name in CHILDREN {
                let (dx, a, sc, turn) = part(name);
                let b = get(name);
                if let Some((m, cx)) = wm.placed_mut(name) {
                    let r = concat(&turn_scale(turn, sc, sc), &[b[0], b[1], b[2], b[3], 0.0, 0.0]);
                    *m = [r[0], r[1], r[2], r[3], b[4] + dx, b[5]];
                    cx[3] = a;
                }
            }
        }
    }
    // the text over the bars and below
    let (title_dx, title_a, _, _) = part("title_mc");
    let (name_dx, name_a, _, _) = part("name_mc");
    // (the name's letters follow the bar 0.15 s later from 100 further)
    let letters = if closing { 1.0 } else { k(t - 0.15, 0.25, strong_out) };
    let name_dx = name_dx + 100.0 * (1.0 - letters);
    let name_a = name_a * if t >= 0.15 || closing { letters } else { 0.0 };
    let task_a = match popup.task {
        Some(_) if closing => 1.0 - k(tc, 0.25, strong_out),
        Some(_) => k(task_t, 0.2, strong_out),
        None => 0.0,
    };
    let glow_a = if popup.task.is_some() && task_t >= 0.2 { 1.0 - k(task_t - 0.2, 0.25, strong_in_out) } else { 0.0 };
    let task_dy = if closing { -50.0 * k(tc, 0.25, strong_out) } else { 0.0 };
    for (pt, mut text, mut font, mut col, mut node, cn) in &mut texts {
        let size = cn.size() * cn.inverse_scale_factor();
        // right-aligned at its field's right edge
        let right = |x: f32| Val::Px(x * s - size.x);
        let (left, top, a, c) = match pt.0 {
            Part::Title(i) => {
                let o = if i == 0 { Vec2::ZERO } else { TITLE_COPIES[i - 1] };
                (right(TITLE_RIGHT + title_dx + o.x), (TITLE_TOP + o.y) * s + (1.2 - 1.0) * 0.5 * size.y, title_a, if i == 0 { DARK } else { PALE })
            }
            Part::Name => (right(NAME_RIGHT + name_dx), NAME_TOP * s, name_a, PALE),
            Part::Task | Part::TaskGlow => {
                let txt = popup.task.map(|p| n.tasks[p.0].0.clone()).unwrap_or_default();
                if text.0 != txt {
                    text.0 = txt;
                }
                node.width = Val::Px(TASK_W * s);
                let (a, c) = if pt.0 == Part::Task { (task_a, PALE) } else { (glow_a * task_a, popup.task.map(|p| state_color(n.tasks[p.0].1)).unwrap_or(Color::WHITE)) };
                (Val::Px((TASK_RIGHT - TASK_W) * s), (TASK_TOP + task_dy) * s, a, c)
            }
        };
        if let Part::Title(_) = pt.0 {
            if font.font_size != bevy::text::FontSize::Px(TITLE_SIZE * s) {
                font.font_size = bevy::text::FontSize::Px(TITLE_SIZE * s);
            }
        }
        node.left = left;
        node.top = Val::Px(top);
        col.0 = c.with_alpha(a);
    }
    // the task's mark plays its state
    if let Ok((mut fc, mut node)) = mark.single_mut() {
        fc.scale = s * 0.5;
        fc.alpha = task_a;
        node.left = Val::Px(TASK_MARK.x * s);
        node.top = Val::Px((TASK_MARK.y + task_dy) * s);
        if let Some((i, since)) = popup.task {
            if task_t >= 0.2 && task_t - dt < 0.2 || since == t {
                let l = state_label(n.tasks[i].1);
                fc.goto("", l, true);
            }
        }
    }
}
