//! Corvo's breath under water as the HUD movie shows it (`hud_oMeter`, class `OxygenGauge`,
//! at the bottom right): the gauge swings in over its swirling backing (1.35 s, the backing
//! looping), its fill masked to the breath left with the pointer at its top (tweened 0.45 s,
//! gone when full), a glow pulsing behind (15-30% over 2.25 s); it fades away once he has his
//! breath back.

use crate::flash::{concat, turn_scale, Clip, FlashClip, Mat, MovieTimelines};
use crate::GameState;
use bevy::prelude::*;
use bevy::window::PrimaryWindow;

pub struct OxygenPlugin;

impl Plugin for OxygenPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Gauge>()
            .add_systems(OnEnter(GameState::InGame), |mut g: ResMut<Gauge>| *g = Gauge::default())
            .add_systems(Update, update_oxygen.after(crate::player::PlayerMoveSet).run_if(in_state(GameState::InGame)));
    }
}

const MOVIE: &str = "HUD";
const SYMBOL: &str = "hud_oMeter";
/// `oxygenGauge_mc` on the stage
const AT: Vec2 = Vec2::new(1184.0, 665.0);
const OPEN: f32 = 1.35;
const FADE: f32 = 0.35;
/// `_meterFillDuration`, `_glowMinAlpha`/`_glowMaxAlpha`, `_glowDuration`
const FILL: f32 = 0.45;
const GLOW: (f32, f32, f32) = (0.15, 0.30, 2.25);

#[derive(Resource, Default)]
struct Gauge {
    root: Option<Entity>,
    /// open (since when), or closing (since when)
    open: Option<f32>,
    closing: Option<f32>,
    t: f32,
    /// the children's own matrices: `_bkgd_mc`, `_gauge_mc`, and in it `_mask_mc`, `_indic_mc`
    base: Vec<(&'static str, Mat)>,
    /// the mask's height, the fill shown (tweened) and where it tweens from / to
    mask_h: f32,
    fill: (f32, f32, f32),
}

#[derive(Component)]
struct OxygenClip;

fn strong_out(x: f32) -> f32 {
    1.0 - (1.0 - x.clamp(0.0, 1.0)).powi(5)
}

#[allow(clippy::type_complexity)]
fn update_oxygen(
    mut commands: Commands,
    time: Res<Time>,
    swim: Res<crate::swim::Swim>,
    mut gauge: ResMut<Gauge>,
    mut timelines: ResMut<MovieTimelines>,
    window: Query<&Window, With<PrimaryWindow>>,
    hud: Query<Entity, With<crate::hud::HudRoot>>,
    mut clips: Query<(&mut FlashClip, &mut Node, &mut Visibility), With<OxygenClip>>,
) {
    let dt = time.delta_secs();
    gauge.t += dt;
    let t = gauge.t;
    let want = swim.under.is_some() || swim.breath < swim.max - 0.01;
    let Ok(w) = window.single() else { return };
    let s = (w.width() / 1280.0).min(w.height() / 720.0).max(0.01);
    let off = (Vec2::new(w.width(), w.height()) - Vec2::new(1280.0, 720.0) * s) * 0.5;
    // `Open` / `Close`
    if want && gauge.open.is_none() {
        let (Ok(h), Some(tl)) = (hud.single(), timelines.get(MOVIE)) else { return };
        let Some(c) = Clip::export(&tl, SYMBOL) else { return };
        if let Some(r) = gauge.root.take() {
            commands.entity(r).try_despawn();
        }
        let mut base = Vec::new();
        for n in ["_bkgd_mc", "_gauge_mc"] {
            if let Some(p) = c.placed(n) {
                base.push((n, p.0));
            }
        }
        let mut mask_h = 100.0;
        if let Some(g) = c.child("_gauge_mc") {
            for n in ["_mask_mc", "_indic_mc"] {
                if let Some(p) = g.placed(n) {
                    base.push((n, p.0));
                }
            }
            if let Some((a, b)) = g.child("_mask_mc").and_then(|m| m.bounds(&tl)) {
                mask_h = b.y - a.y;
            }
        }
        let mut fc = FlashClip::new(MOVIE, tl.clone(), c);
        fc.goto("_bkgd_mc", "loop", true);
        let e = commands.spawn((OxygenClip, fc, Node { position_type: PositionType::Absolute, ..default() }, Visibility::Hidden, Pickable::IGNORE, ChildOf(h))).id();
        let level = (swim.breath / swim.max.max(0.1)).clamp(0.0, 1.0);
        *gauge = Gauge { root: Some(e), open: Some(t), closing: None, t, base, mask_h, fill: (level, level, t) };
        return;
    }
    if !want && gauge.open.is_some() && gauge.closing.is_none() {
        gauge.closing = Some(t);
    }
    let Some(root) = gauge.root else { return };
    let Some(opened) = gauge.open else { return };
    if let Some(c) = gauge.closing {
        if t - c > FADE.max(0.25) {
            commands.entity(root).try_despawn();
            *gauge = Gauge { t, ..default() };
            return;
        }
    }
    // `FillOxygenGauge`: the level tweened to the breath left
    let level = (swim.breath / swim.max.max(0.1)).clamp(0.001, 1.0);
    let at = |f: (f32, f32, f32)| f.0 + (f.1 - f.0) * strong_out((t - f.2) / FILL);
    // (a new level: tweened to from where it shows now)
    if (level - gauge.fill.1).abs() > 0.005 {
        gauge.fill = (at(gauge.fill), level, t);
    }
    let (_, to, since) = gauge.fill;
    let shown = at(gauge.fill);
    let Ok((mut fc, mut node, mut vis)) = clips.get_mut(root) else { return };
    let p = off + AT * s;
    node.left = Val::Px(p.x);
    node.top = Val::Px(p.y);
    fc.scale = s;
    let a = match gauge.closing {
        Some(c) => 1.0 - strong_out((t - c) / FADE),
        None => strong_out((t - opened) / FADE),
    };
    fc.alpha = a;
    *vis = Visibility::Inherited;
    let base = gauge.base.clone();
    let get = |n: &str| base.iter().find(|b| b.0 == n).map(|b| b.1);
    // swung in: the backing from x 10 turned 15, the gauge from x 15 turned 25
    let k = strong_out((t - opened) / OPEN);
    if let (Some(b), Some((m, _))) = (get("_bkgd_mc"), fc.clip.placed_mut("_bkgd_mc")) {
        let r = concat(&turn_scale(15.0 * (1.0 - k), 1.0, 1.0), &[b[0], b[1], b[2], b[3], 0.0, 0.0]);
        *m = [r[0], r[1], r[2], r[3], 10.0 + (b[4] - 10.0) * k, b[5]];
    }
    if let (Some(b), Some((m, _))) = (get("_gauge_mc"), fc.clip.placed_mut("_gauge_mc")) {
        let turn0 = b[1].atan2(b[0]).to_degrees();
        let turn = 25.0 + (turn0 - 25.0) * k;
        let sc = (b[0] * b[0] + b[1] * b[1]).sqrt();
        let r = turn_scale(turn, sc, sc);
        *m = [r[0], r[1], r[2], r[3], 15.0 + (b[4] - 15.0) * k, b[5]];
    }
    let mask_h = gauge.mask_h;
    let glow = {
        // the glow's yo-yo between its alphas
        let ph = (t / GLOW.2) as u32;
        let x = strong_out((t % GLOW.2) / GLOW.2);
        if ph % 2 == 0 { GLOW.0 + (GLOW.1 - GLOW.0) * x } else { GLOW.1 + (GLOW.0 - GLOW.1) * x }
    };
    if let Some(g) = fc.clip.child_mut("_gauge_mc") {
        if let (Some(b), Some((m, _))) = (get("_mask_mc"), g.placed_mut("_mask_mc")) {
            *m = [b[0], b[1], b[2], b[3] * shown, b[4], b[5]];
        }
        if let (Some(b), Some((m, cx))) = (get("_indic_mc"), g.placed_mut("_indic_mc")) {
            let my = get("_mask_mc").map(|mb| mb[5]).unwrap_or(b[5]);
            *m = [b[0], b[1], b[2], b[3], b[4], my - mask_h * shown];
            cx[3] = if to < 0.999 { 1.0 } else { 1.0 - strong_out((t - since) / FILL) };
        }
        if let Some((_, cx)) = g.placed_mut("_glow_mc") {
            cx[3] = glow;
        }
    }
}
