//! The overlays of `UI_Global` over every screen: the saving notification
//! (`g_SavingNotification_Icon`: `gl_sIcon_ic` at the bottom right of the 85% safe frame, its
//! circles and blades playing `anim` and its gears turning, `_utils.RotationAnimation`) while
//! a game is being saved.

use crate::flash::{Clip, FlashClip};
use crate::GameState;
use bevy::prelude::*;
use bevy::window::PrimaryWindow;

pub struct GlobalUiPlugin;

impl Plugin for GlobalUiPlugin {
    fn build(&self, app: &mut App) {
        app.add_message::<ShowSaving>().add_systems(Update, (show_saving, tick_saving).chain().run_if(in_state(GameState::InGame)));
    }
}

/// A game was saved: the notification shows a moment.
#[derive(Message, Clone, Copy)]
pub struct ShowSaving;

#[derive(Component)]
struct SavingIcon {
    t: f32,
}

const MOVIE: &str = "Global";
/// how long it shows (s), fading in and out
const SHOWN: f32 = 2.0;
const FADE: f32 = 0.25;

fn show_saving(mut commands: Commands, mut ev: MessageReader<ShowSaving>, mut timelines: ResMut<crate::flash::MovieTimelines>, icons: Query<Entity, With<SavingIcon>>) {
    if ev.read().last().is_none() {
        return;
    }
    for e in &icons {
        commands.entity(e).despawn();
    }
    let Some(tl) = timelines.get(MOVIE) else { return };
    let Some(c) = Clip::export(&tl, "gl_sIcon_ic") else { return };
    let mut fc = FlashClip::new(MOVIE, tl.clone(), c).real();
    fc.goto("", "anim", true);
    fc.alpha = 0.0;
    commands.spawn((SavingIcon { t: 0.0 }, fc, Node { position_type: PositionType::Absolute, ..default() }, GlobalZIndex(95), Pickable::IGNORE, DespawnOnExit(GameState::InGame)));
}

fn tick_saving(mut commands: Commands, time: Res<Time<bevy::time::Real>>, window: Query<&Window, With<PrimaryWindow>>, mut icons: Query<(Entity, &mut SavingIcon, &mut FlashClip, &mut Node)>) {
    let Ok(w) = window.single() else { return };
    let s = (w.width() / 1280.0).min(w.height() / 720.0).max(0.01);
    let off = (Vec2::new(w.width(), w.height()) - Vec2::new(1280.0, 720.0) * s) * 0.5;
    for (e, mut icon, mut fc, mut node) in &mut icons {
        icon.t += time.delta_secs();
        if icon.t > SHOWN + FADE {
            commands.entity(e).despawn();
            continue;
        }
        // the safe frame's corner (85%), the icon 50 left and 45 up of it
        let p = off + Vec2::new(640.0 + 0.85 * 640.0 - 50.0, 360.0 + 0.85 * 360.0 - 45.0) * s;
        node.left = Val::Px(p.x);
        node.top = Val::Px(p.y);
        fc.scale = s;
        fc.alpha = (icon.t / FADE).min(1.0).min(((SHOWN + FADE - icon.t) / FADE).max(0.0));
        // the gears turn, the small ones against the big one
        let t = icon.t;
        if let Some(g) = fc.clip.child_mut("gears_mc") {
            for (name, speed) in [("gear0_mc", 90.0f32), ("gear1_mc", -120.0), ("gear2_mc", 150.0)] {
                if let Some((m, _)) = g.placed_mut(name) {
                    let sc = (m[0] * m[0] + m[1] * m[1]).sqrt();
                    let flip = if m[0] * m[3] - m[1] * m[2] < 0.0 { -1.0 } else { 1.0 };
                    let (sn, cs) = (speed * t).to_radians().sin_cos();
                    m[0] = cs * sc * flip;
                    m[1] = sn * sc * flip;
                    m[2] = -sn * sc;
                    m[3] = cs * sc;
                }
            }
        }
    }
}
