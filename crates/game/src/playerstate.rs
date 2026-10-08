//! Corvo's stance as the HUD movie shows it (`hud_icPlayerStates`, class `PlayerStatesIcons`,
//! at the bottom left, by `EDisUIPlayerState`): crouched, the figure crouching (`crouch`) or
//! creeping (`sneak`) as he moves, played in from standing (`open`) over the swirl backing
//! (swung in from 15 degrees, faded in 0.35 s at 85%), back up to standing as he stands
//! (`close`, faded out 0.35 s).

use crate::flash::{concat, turn_scale, Clip, FlashClip, Mat, MovieTimelines};
use crate::player::Player;
use crate::GameState;
use bevy::prelude::*;
use bevy::window::PrimaryWindow;

pub struct PlayerStatePlugin;

impl Plugin for PlayerStatePlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<StateIcon>()
            .add_systems(OnEnter(GameState::InGame), |mut s: ResMut<StateIcon>| *s = StateIcon::default())
            .add_systems(Update, update_state_icon.after(crate::player::PlayerMoveSet).run_if(in_state(GameState::InGame)));
    }
}

const MOVIE: &str = "HUD";
const SYMBOL: &str = "hud_icPlayerStates";
/// `playerStatesIcon_mc` on the stage
const AT: Vec2 = Vec2::new(96.05, 663.05);
const FADE: f32 = 0.35;
/// the icon's size in the clip
const ICON_SCALE: f32 = 0.85;

#[derive(Resource, Default)]
struct StateIcon {
    root: Option<Entity>,
    /// the icon shown (symbol), since when; closing since when
    icon: Option<&'static str>,
    since: f32,
    closing: Option<f32>,
    t: f32,
    bkgd: Option<Mat>,
}

#[derive(Component)]
struct StateClip;
#[derive(Component)]
struct StateGlyph;

fn strong_out(x: f32) -> f32 {
    1.0 - (1.0 - x.clamp(0.0, 1.0)).powi(5)
}

#[allow(clippy::type_complexity)]
fn update_state_icon(
    mut commands: Commands,
    time: Res<Time>,
    mut st: ResMut<StateIcon>,
    mut timelines: ResMut<MovieTimelines>,
    window: Query<&Window, With<PrimaryWindow>>,
    hud: Query<Entity, With<crate::hud::HudRoot>>,
    player: Query<&Player>,
    mut clips: Query<(&mut FlashClip, &mut Node, &mut Visibility), (With<StateClip>, Without<StateGlyph>)>,
    mut glyphs: Query<(&mut FlashClip, &mut Node), (With<StateGlyph>, Without<StateClip>)>,
) {
    st.t += time.delta_secs();
    let t = st.t;
    let Ok(w) = window.single() else { return };
    let Some(tl) = timelines.get(MOVIE) else { return };
    let s = (w.width() / 1280.0).min(w.height() / 720.0).max(0.01);
    let off = (Vec2::new(w.width(), w.height()) - Vec2::new(1280.0, 720.0) * s) * 0.5;
    // `EDisUIPlayerState`: crouched still, crouched moving
    let want: Option<&'static str> = player.single().ok().filter(|p| p.crouched && !p.locked).map(|p| if p.velocity.with_y(0.0).length() > 0.3 { "sneak" } else { "crouch" });
    // the clip (built once, under the HUD)
    let root = match st.root.filter(|r| clips.contains(*r)) {
        Some(r) => r,
        None => {
            let (Ok(h), Some(c)) = (hud.single(), Clip::export(&tl, SYMBOL)) else { return };
            st.bkgd = c.placed("_bkgd_mc").map(|p| p.0);
            let r = commands.spawn((StateClip, FlashClip::new(MOVIE, tl.clone(), c), Node { position_type: PositionType::Absolute, ..default() }, Visibility::Hidden, Pickable::IGNORE, ChildOf(h))).id();
            st.root = Some(r);
            return;
        }
    };
    // `SetIcon`: a new icon (opened), or none (closed)
    match want {
        Some(n) if st.icon != Some(n) || st.closing.is_some() => {
            let opening = st.icon.is_none() || st.closing.is_some();
            if let Ok((mut g, _)) = glyphs.single_mut() {
                if let Some(c) = Clip::export(&tl, n) {
                    g.clip = c;
                }
            } else if let Some(c) = Clip::export(&tl, n) {
                commands.spawn((StateGlyph, FlashClip::new(MOVIE, tl.clone(), c), Node { position_type: PositionType::Absolute, ..default() }, Pickable::IGNORE, ChildOf(root)));
            }
            if opening {
                st.since = t;
            }
            st.icon = Some(n);
            st.closing = None;
        }
        None if st.icon.is_some() && st.closing.is_none() => {
            st.closing = Some(t);
            if let Ok((mut g, _)) = glyphs.single_mut() {
                g.goto("", "close", true);
            }
        }
        _ => {}
    }
    let Ok((mut fc, mut node, mut vis)) = clips.get_mut(root) else { return };
    if let Some(c) = st.closing {
        if t - c > FADE {
            st.icon = None;
            st.closing = None;
            *vis = Visibility::Hidden;
            for (mut g, _) in &mut glyphs {
                g.alpha = 0.0;
            }
            return;
        }
    }
    if st.icon.is_none() {
        *vis = Visibility::Hidden;
        return;
    }
    let p = off + AT * s;
    node.left = Val::Px(p.x);
    node.top = Val::Px(p.y);
    fc.scale = s;
    let a = match st.closing {
        Some(c) => 1.0 - strong_out((t - c) / FADE),
        None => strong_out((t - st.since) / FADE),
    };
    fc.alpha = a;
    *vis = Visibility::Inherited;
    // the backing swung in from x -10 turned 15
    let k = strong_out((t - st.since) / FADE);
    if let (Some(b), Some((m, _))) = (st.bkgd, fc.clip.placed_mut("_bkgd_mc")) {
        let r = concat(&turn_scale(15.0 * (1.0 - k), 1.0, 1.0), &[b[0], b[1], b[2], b[3], 0.0, 0.0]);
        *m = [r[0], r[1], r[2], r[3], b[4] - 10.0 * (1.0 - k), b[5]];
    }
    for (mut g, mut gn) in &mut glyphs {
        g.scale = s * ICON_SCALE;
        g.alpha = a;
        gn.left = Val::Px(0.0);
        gn.top = Val::Px(0.0);
    }
}
