//! The grenade warning (the HUD movie's `grenadeIndicator`): for each live grenade near Corvo,
//! the grenade icon (`_grenade_mc`) about the crosshair on its side, the locator's arc
//! (`_locator_mc`) turned to point at it; both run through their 201 frames as the fuse burns,
//! their glow pulsing faster.

use crate::flash::{turn_scale, Clip, FlashClip, MovieTimelines};
use crate::gadgets::Grenade;
use crate::player::{Player, PlayerCamera};
use crate::GameState;
use bevy::prelude::*;
use bevy::window::PrimaryWindow;

pub struct GrenadeIndPlugin;

impl Plugin for GrenadeIndPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Update, (update_indicators, cooking_gauge).before(crate::flash::PlayClips).run_if(in_state(GameState::InGame)));
    }
}

const MOVIE: &str = "HUD";
const SYMBOL: &str = "grenadeIndicator";
/// the crosshair on the stage, and how far from it the warnings sit
const CENTER: Vec2 = Vec2::new(640.0, 360.0);
const RADIUS: f32 = 110.0;
const MAX: usize = 4;
/// its last frame
const LAST: f32 = 200.0;
/// how far beyond its reach to Corvo a grenade is warned of
const MARGIN: f32 = 2.0;

/// The grenade cooking gauge (`grenadeCooking_mc` at the stage's middle, class
/// `GrenadeCooking`): `_gauge_mc`'s 301 frames are the fuse's hundredths of a second burnt;
/// every half second (`_levelOffset` 50) its marker and glow play and its stroke flashes into
/// the next level's loop. Faded in over 0.25 s.
#[derive(Component)]
struct CookGauge {
    level: usize,
    shown: f32,
}

#[allow(clippy::type_complexity)]
fn cooking_gauge(
    mut commands: Commands,
    time: Res<Time>,
    mut timelines: ResMut<MovieTimelines>,
    window: Query<&Window, With<PrimaryWindow>>,
    hud: Query<Entity, With<crate::hud::HudRoot>>,
    powers: Res<crate::powers::Powers>,
    mut gauge: Query<(&mut CookGauge, &mut FlashClip, &mut Node, &mut Visibility)>,
) {
    let Ok((mut g, mut fc, mut n, mut v)) = gauge.single_mut() else {
        let (Ok(h), Some(tl)) = (hud.single(), timelines.get(MOVIE)) else { return };
        let Some(c) = Clip::export(&tl, "hud_grCook_") else { return };
        commands.spawn((CookGauge { level: 0, shown: 0.0 }, FlashClip::new(MOVIE, tl.clone(), c), Node { position_type: PositionType::Absolute, ..default() }, Visibility::Hidden, Pickable::IGNORE, ChildOf(h)));
        return;
    };
    let Ok(w) = window.single() else { return };
    let Some((_, t)) = powers.cooking else {
        g.shown = 0.0;
        g.level = 0;
        if *v != Visibility::Hidden {
            *v = Visibility::Hidden;
        }
        return;
    };
    let s = (w.width() / 1280.0).min(w.height() / 720.0).max(0.01);
    let off = (Vec2::new(w.width(), w.height()) - Vec2::new(1280.0, 720.0) * s) * 0.5;
    let p = off + CENTER * s;
    n.left = Val::Px(p.x);
    n.top = Val::Px(p.y);
    fc.scale = s;
    g.shown = (g.shown + time.delta_secs() / 0.25).min(1.0);
    fc.alpha = 1.0 - (1.0 - g.shown).powi(5);
    // `SetGauge`
    let value = ((t * 100.0) as usize).min(300);
    let tl = fc.tl.clone();
    if let Some(c) = fc.clip.child_mut("_gauge_mc") {
        if c.frame != value {
            c.goto(&tl, value, false);
        }
    }
    let level = value / 50;
    if level > g.level {
        g.level = level;
        for part in ["_gauge_mc._indic_mc", "_gauge_mc._glow_mc"] {
            if let Some(c) = fc.clip.child_mut(part) {
                c.playing = true;
            }
        }
        fc.goto("_gauge_mc._stroke_mc", "flash", true);
        fc.goto("_gauge_mc._stroke_mc._stroke_mc", &format!("loop{level}"), true);
    }
    if *v != Visibility::Inherited {
        *v = Visibility::Inherited;
    }
}

#[derive(Component)]
struct Indicator {
    slot: usize,
    frame: Option<usize>,
}

#[allow(clippy::type_complexity)]
fn update_indicators(
    mut commands: Commands,
    mut timelines: ResMut<MovieTimelines>,
    window: Query<&Window, With<PrimaryWindow>>,
    hud: Query<Entity, With<crate::hud::HudRoot>>,
    cam: Query<&GlobalTransform, With<PlayerCamera>>,
    player: Query<&Transform, With<Player>>,
    grenades: Query<(&Grenade, &Transform), Without<Player>>,
    mut inds: Query<(&mut Indicator, &mut FlashClip, &mut Node, &mut Visibility)>,
    settings: Res<crate::settings::Settings>,
) {
    // the warnings (built once, under the HUD)
    if inds.is_empty() {
        let (Ok(h), Some(tl)) = (hud.single(), timelines.get(MOVIE)) else { return };
        for slot in 0..MAX {
            let Some(c) = Clip::export(&tl, SYMBOL) else { return };
            commands.spawn((Indicator { slot, frame: None }, FlashClip::new(MOVIE, tl.clone(), c), Node { position_type: PositionType::Absolute, ..default() }, Visibility::Hidden, Pickable::IGNORE, ChildOf(h)));
        }
        return;
    }
    let (Ok(w), Ok(cg), Ok(pt)) = (window.single(), cam.single(), player.single()) else { return };
    // the grenades in reach of him, nearest first
    let mut near: Vec<(f32, Vec3, f32)> = grenades
        .iter()
        .map(|(g, t)| (t.translation.distance(pt.translation), t.translation, 1.0 - g.fuse / g.fuse_total.max(0.01), g.blast[2]))
        .filter(|(d, _, _, reach)| *d < reach + MARGIN && settings.grenade_markers)
        .map(|(d, at, k, _)| (d, at, k))
        .collect();
    near.sort_by(|a, b| a.0.total_cmp(&b.0));
    let s = (w.width() / 1280.0).min(w.height() / 720.0).max(0.01);
    let off = (Vec2::new(w.width(), w.height()) - Vec2::new(1280.0, 720.0) * s) * 0.5;
    let f = cg.forward().as_vec3().with_y(0.0).normalize_or(Vec3::NEG_Z);
    let right = Vec3::new(-f.z, 0.0, f.x);
    for (mut ind, mut fc, mut node, mut vis) in &mut inds {
        let Some(&(_, at, k)) = near.get(ind.slot) else {
            if *vis != Visibility::Hidden {
                *vis = Visibility::Hidden;
            }
            ind.frame = None;
            continue;
        };
        // its side, clockwise from ahead
        let to = (at - cg.translation()).with_y(0.0);
        let a = to.dot(right).atan2(to.dot(f));
        let p = off + (CENTER + Vec2::new(a.sin(), -a.cos()) * RADIUS) * s;
        node.left = Val::Px(p.x);
        node.top = Val::Px(p.y);
        fc.scale = s;
        // the arc's point (down, as drawn) turned toward the grenade
        if let Some((m, _)) = fc.clip.placed_mut("_locator_mc") {
            *m = turn_scale(a.to_degrees() + 180.0, 1.0, 1.0);
        }
        let frame = (k.clamp(0.0, 1.0) * LAST).round() as usize;
        if ind.frame != Some(frame) {
            ind.frame = Some(frame);
            let tl = fc.tl.clone();
            for part in ["_locator_mc", "_grenade_mc"] {
                if let Some(c) = fc.clip.child_mut(part) {
                    c.goto(&tl, frame, false);
                }
            }
        }
        if *vis != Visibility::Inherited {
            *vis = Visibility::Inherited;
        }
    }
}
