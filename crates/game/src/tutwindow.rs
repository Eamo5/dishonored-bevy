//! The tutorial window (the HUD movie's `tutorialWindow_mc`, class `TutorialWindow`) at the
//! bottom left: a hint with its picture — a new rune, a new bone charm, mission clues — on the
//! brushed backing. It pops in from the left at 250% (0.2 s), its picture and strokes settling
//! a moment later; while up its parts drift to and fro (`Anim` 5 s / `AnimLoop` 4.25 s); after
//! the HUD tweak's `m_fTutorialWindowDuration` it slides out (0.4 s). The picture is
//! `_img_mc`'s frame: 0 a gear (a tutorial note), 1 a note (mission clues), 2 a rune, 3 a bone
//! charm.

use crate::flash::{turn_scale, Clip, Cx, FlashClip, Mat, MovieTimelines};
use crate::GameState;
use bevy::prelude::*;
use bevy::window::PrimaryWindow;

pub struct TutWindowPlugin;

impl Plugin for TutWindowPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<TutorialWindow>()
            .add_systems(OnEnter(GameState::InGame), |mut w: ResMut<TutorialWindow>| w.reset())
            .add_systems(Update, update_window.before(crate::flash::PlayClips).run_if(in_state(GameState::InGame)));
    }
}

/// The pictures (`_img_mc`'s frames).
pub const IMG_NOTE: u8 = 1;
pub const IMG_RUNE: u8 = 2;
pub const IMG_CHARM: u8 = 3;

const MOVIE: &str = "HUD";
const SYMBOL: &str = "hud_tutoWin_";
/// `tutorialWindow_mc` on the stage
const AT: Vec2 = Vec2::new(96.0, 664.35);
/// `m_fTutorialWindowDuration`
const DURATION: f32 = 10.0;
/// `_txt_mc.txt`: the body font at 22, `#e3f2d6`, 256 wide (its gutter 2)
const TEXT_SIZE: f32 = 22.0;
const TEXT_W: f32 = 256.0;
const TEXT_COLOR: Color = Color::srgb(227.0 / 255.0, 242.0 / 255.0, 214.0 / 255.0);
const SHADOW: Color = Color::srgba(23.0 / 255.0, 25.0 / 255.0, 28.0 / 255.0, 0.85);
/// the parts of `_mc` that move
const PARTS: [&str; 5] = ["_bkgd_mc", "_img_mc", "_txt_mc", "_blackStroke_mc", "_whiteStroke_mc"];

/// The hints waiting, the one up and since when (closing since when).
#[derive(Resource, Default)]
pub struct TutorialWindow {
    /// the scripts hold the game's own tutorials back
    pub held: bool,
    queue: Vec<(String, u8)>,
    shown: Option<(String, u8)>,
    t: f32,
    closing: Option<f32>,
    root: Option<Entity>,
    /// `_mc`'s and its parts' authored matrices and colour transforms (`props`)
    props: Vec<(Mat, Cx)>,
}

impl TutorialWindow {
    /// `SetTutorialWindow`: a hint and its picture (none while the scripts hold the game's own
    /// tutorials back: `DisSeqAct_EnableSystemicTutorials`).
    pub fn push(&mut self, text: impl Into<String>, img: u8) {
        if !self.held {
            self.queue.push((text.into(), img));
        }
    }

    fn reset(&mut self) {
        let root = self.root.take();
        *self = TutorialWindow::default();
        self.root = root;
    }
}

#[derive(Component)]
struct WindowClip;
#[derive(Component)]
struct WindowText;

fn strong_out(x: f32) -> f32 {
    1.0 - (1.0 - x.clamp(0.0, 1.0)).powi(5)
}

fn strong_in_out(x: f32) -> f32 {
    let x = x.clamp(0.0, 1.0) * 2.0;
    if x < 1.0 {
        0.5 * x.powi(5)
    } else {
        0.5 * ((x - 2.0).powi(5) + 2.0)
    }
}

fn back_out(x: f32) -> f32 {
    let x = x.clamp(0.0, 1.0) - 1.0;
    let s = 1.70158;
    x * x * ((s + 1.0) * x + s) + 1.0
}

/// A matrix as `_x`, `_y`, `_xscale`, `_yscale`, `_rotation` (degrees, clockwise).
fn parts(m: &Mat) -> (Vec2, Vec2, f32) {
    let sx = m[0].hypot(m[1]);
    let sy = m[2].hypot(m[3]);
    (Vec2::new(m[4], m[5]), Vec2::new(sx, sy), m[1].atan2(m[0]).to_degrees())
}

fn compose(at: Vec2, scale: Vec2, deg: f32) -> Mat {
    let mut m = turn_scale(deg, scale.x, scale.y);
    m[4] = at.x;
    m[5] = at.y;
    m
}

/// A part's state at `t` s since opening (`closing`: s since closing) against its authored
/// one: its offset, scale and alpha shares, its turn (from the authored `turn`).
fn part_state(name: &str, turn: f32, t: f32, closing: Option<f32>) -> (Vec2, f32, f32, f32) {
    // the drift: to the `Anim` targets over 5 s (cut at 4.5), back over 4.25 s (cut at 3.75)
    let d = {
        let t = t - 0.45;
        if t <= 0.0 {
            0.0
        } else {
            let t = t % 8.25;
            if t < 4.5 {
                strong_out(t / 5.0)
            } else {
                1.0 - strong_out((t - 4.5) / 4.25)
            }
        }
    };
    let lerp = |a: f32, b: f32, k: f32| a + (b - a) * k;
    // (before `OpenImage`, 0.15 s in, the parts are hidden)
    let o = t - 0.15;
    let shown = |e: f32| if o < 0.0 { 0.0 } else { e };
    let (off, mut k, deg, mut a) = match name {
        "_bkgd_mc" => (-10.0 * d, 1.0 + 0.02 * d, turn, 1.0 - 0.1 * d),
        "_img_mc" => {
            let e = strong_out(o / 0.2);
            (10.0 * d, 2.0 - e, lerp(turn, 2.0, d), shown(e))
        }
        "_txt_mc" => {
            let e = strong_in_out(o / 0.2);
            (-10.0 * d, 1.5 - 0.5 * e, turn, shown(e))
        }
        "_blackStroke_mc" => (0.0, 2.0 - back_out(o / 0.2), turn, shown(strong_out(o / 0.2))),
        _ => {
            // the white stroke: swung in from -15 degrees, drifting to -5
            let e = strong_in_out(o / 0.3);
            let deg = if t < 0.45 { lerp(-15.0, turn, e) } else { lerp(turn, -5.0, d) };
            (-7.0 * d, 2.0 - e, deg, shown(e))
        }
    };
    if let Some(c) = closing {
        match name {
            "_img_mc" => k *= 1.0 + 0.2 * strong_out(c / 0.2),
            "_blackStroke_mc" => {
                k *= 1.0 + 0.2 * strong_out(c / 0.2);
                a *= 1.0 - strong_out(c / 0.2);
            }
            "_whiteStroke_mc" => {
                k *= 1.0 + 0.3 * strong_out(c / 0.25);
                a *= 1.0 - strong_out(c / 0.25);
            }
            _ => {}
        }
    }
    (Vec2::new(off, 0.0), k, deg, a)
}

#[allow(clippy::type_complexity, clippy::too_many_arguments)]
fn update_window(
    mut commands: Commands,
    time: Res<Time>,
    mut st: ResMut<TutorialWindow>,
    mut timelines: ResMut<MovieTimelines>,
    window: Query<&Window, With<PrimaryWindow>>,
    hud: Query<Entity, With<crate::hud::HudRoot>>,
    (paused, settings): (Res<crate::hud::Paused>, Res<crate::settings::Settings>),
    mut sfx: MessageWriter<crate::audio::PostEvent>,
    mut clips: Query<(&mut FlashClip, &mut Node, &mut Visibility), (With<WindowClip>, Without<WindowText>)>,
    mut text: Query<(&mut Text, &mut TextFont, &mut TextColor, &mut TextShadow, &mut Node, &mut UiTransform, &mut Visibility), (With<WindowText>, Without<WindowClip>)>,
) {
    let Ok(w) = window.single() else { return };
    // built once under the HUD
    let root = match st.root.filter(|r| clips.contains(*r)) {
        Some(r) => r,
        None => {
            let (Ok(h), Some(tl)) = (hud.single(), timelines.get(MOVIE)) else { return };
            let Some(c) = Clip::export(&tl, SYMBOL) else { return };
            let mut props = vec![c.placed("_mc").unwrap_or((crate::flash::IDENTITY, crate::flash::NO_CX))];
            let inner = c.child("_mc");
            for p in PARTS {
                props.push(inner.and_then(|m| m.placed(p)).unwrap_or((crate::flash::IDENTITY, crate::flash::NO_CX)));
            }
            st.props = props;
            let r = commands.spawn((WindowClip, FlashClip::new(MOVIE, tl.clone(), c), Node { position_type: PositionType::Absolute, ..default() }, Visibility::Hidden, Pickable::IGNORE, ChildOf(h))).id();
            commands.spawn((
                WindowText,
                Text::new(""),
                TextFont::default(),
                TextColor(TEXT_COLOR),
                TextShadow { offset: Vec2::splat(1.5), color: SHADOW },
                Node { position_type: PositionType::Absolute, ..default() },
                UiTransform::default(),
                Visibility::Hidden,
                Pickable::IGNORE,
                ChildOf(h),
            ));
            st.root = Some(r);
            return;
        }
    };
    let dt = if paused.0 { 0.0 } else { time.delta_secs() };
    // the next hint once the last is gone (`PSI_HUD_bShowTutorialNotifications` off: none)
    if !settings.tutorials {
        st.queue.clear();
    }
    if st.shown.is_none() && !st.queue.is_empty() {
        let next = st.queue.remove(0);
        st.shown = Some(next);
        st.t = 0.0;
        st.closing = None;
        sfx.write(crate::audio::PostEvent::named("UI_H_windowNotification", None));
    }
    let Some((msg, img)) = st.shown.clone() else {
        if let Ok((_, _, mut v)) = clips.get_mut(root) {
            if *v != Visibility::Hidden {
                *v = Visibility::Hidden;
            }
        }
        if let Ok((mut t, _, _, _, _, _, mut v)) = text.single_mut() {
            if *v != Visibility::Hidden {
                *v = Visibility::Hidden;
            }
            t.0.clear();
        }
        return;
    };
    st.t += dt;
    if st.closing.is_none() && st.t >= DURATION {
        st.closing = Some(0.0);
    }
    if let Some(c) = st.closing.as_mut() {
        *c += dt;
        if *c >= 0.4 {
            st.shown = None;
            st.closing = None;
            return;
        }
    }
    let (t, closing) = (st.t, st.closing);
    let s = (w.width() / 1280.0).min(w.height() / 720.0).max(0.01);
    let off = (Vec2::new(w.width(), w.height()) - Vec2::new(1280.0, 720.0) * s) * 0.5;
    let Ok((mut fc, mut n, mut v)) = clips.get_mut(root) else { return };
    let p = off + AT * s;
    n.left = Val::Px(p.x);
    n.top = Val::Px(p.y);
    fc.scale = s;
    if *v != Visibility::Inherited {
        *v = Visibility::Inherited;
    }
    // `_mc`: in from 250 left at 250% (0.2 s, Back.easeOut), out 30 to the left (0.4 s)
    let (mc_m, mc_cx) = st.props[0];
    let (mc_at, mc_scale, mc_turn) = parts(&mc_m);
    let (mc_off, mc_k, mc_a) = match closing {
        Some(c) => {
            let e = strong_out(c / 0.4);
            (-30.0 * e, 1.0, 1.0 - e)
        }
        None => {
            let e = back_out(t / 0.2);
            (-250.0 * (1.0 - e), 2.5 - 1.5 * e, strong_out(t / 0.2).min(1.0))
        }
    };
    let tl = fc.tl.clone();
    if let Some((m, cx)) = fc.clip.placed_mut("_mc") {
        *m = compose(mc_at + Vec2::new(mc_off, 0.0), mc_scale * mc_k, mc_turn);
        *cx = mc_cx;
        cx[3] = mc_cx[3] * mc_a;
    }
    let mut txt_state = (Vec2::ZERO, 1.0, 0.0, 1.0);
    if let Some(inner) = fc.clip.child_mut("_mc") {
        for (i, name) in PARTS.iter().enumerate() {
            let (pm, pcx) = st.props[i + 1];
            let (at, scale, turn) = parts(&pm);
            let (o, k, deg, a) = part_state(name, turn, t, closing);
            if *name == "_txt_mc" {
                txt_state = (at + o, k, deg, pcx[3] * a);
            }
            if let Some((m, cx)) = inner.placed_mut(name) {
                *m = compose(at + o, scale * k, deg);
                *cx = pcx;
                cx[3] = pcx[3] * a;
            }
        }
        // the picture
        if let Some(c) = inner.child_mut("_img_mc") {
            if c.frame != img as usize {
                c.goto(&tl, img as usize, false);
            }
        }
    }
    // the words: `_txt_mc.txt` in the window's frame
    if let Ok((mut tx, mut f, mut c, mut sh, mut tn, mut tf, mut tv)) = text.single_mut() {
        if tx.0 != msg {
            tx.0 = msg.clone();
        }
        let fs = bevy::text::FontSize::Px(TEXT_SIZE * s);
        if f.font_size != fs {
            f.font_size = fs;
        }
        let k = mc_k;
        let mc_pos = mc_at + Vec2::new(mc_off, 0.0);
        let at = AT + mc_pos + (txt_state.0 + Vec2::splat(2.0)) * k;
        let pt = off + at * s;
        tn.left = Val::Px(pt.x);
        tn.top = Val::Px(pt.y);
        tn.width = Val::Px(TEXT_W * s * k);
        tf.rotation = Rot2::degrees(txt_state.2);
        tf.scale = Vec2::splat(txt_state.1 * k);
        let a = (txt_state.3 * mc_a).clamp(0.0, 1.0);
        c.0 = TEXT_COLOR.with_alpha(a);
        sh.color = SHADOW.with_alpha(SHADOW.alpha() * a);
        sh.offset = Vec2::splat(1.5 * s);
        if *tv != Visibility::Inherited {
            *tv = Visibility::Inherited;
        }
    }
}
