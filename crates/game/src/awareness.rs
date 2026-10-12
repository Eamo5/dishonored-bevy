//! The awareness markers around the heads of the characters who notice Corvo: the HUD movie's
//! `awarenessIndicator` (the HUD tweak's `m_AwarenessMarkerSettings`), two mirrored fans of
//! three lightning bolts played as authored (`flash`). A bolt lights for each attention level
//! reached (head track, turn to face, investigate: `fillIn`, a red flash settling white; back
//! down, `emptyOut`), all three fly off red once he is busted (`busted_1..3`); the fans fade in
//! as the marker comes up and out once the attention has leaked away (`fadeOut`), or at once
//! when the character goes down (`quickFadeOut`). Markers shrink with distance (500 /
//! (tan(fov / 2) x distance in units), at 75%: `m_ScaleVariation`) and fade out within 2 m
//! (`m_AlphaVariation`), on enemies and on those the scripts show
//! (`DisSeqAct_OverrideAwarenessDisplay`). A character off the screen has its marker at the
//! screen's edge, the symbol's second frame turned towards it (mirrored on the left), sized
//! 50-150% (`m_fMin/MaxScaleOutsideFOV`). A marker shows while its character is seen (on the
//! screen, or in a clear line from the eye) and for `m_fFadeOutDelay_NowHidden` (1 s) after,
//! as roshan's r614/blink research decoded the marker's tick. Reaching turn to face plays the
//! detection stinger (`m_pAwarenessStingerEvent` at `m_AttentionLevelToPlayAwarenessStinger`).

use crate::flash::{Clip, FlashClip, MovieTimelines};
use crate::npc::{Alert, Npc};
use crate::player::PlayerCamera;
use crate::GameState;
use bevy::prelude::*;
use bevy::window::PrimaryWindow;

pub struct AwarenessPlugin;

impl Plugin for AwarenessPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Stinger>().add_systems(Update, update_markers.run_if(in_state(GameState::InGame)));
    }
}

const MOVIE: &str = "HUD";
const SYMBOL: &str = "awarenessIndicator";
const SIDES: [&str; 2] = ["right_mc", "left_mc"];
/// `m_ScaleVariation` (percent)
const SCALE: f32 = 0.75;
/// `m_AlphaVariation`: full from 200 units away, gone at 150
const FADE_FAR: f32 = 2.0;
const FADE_NEAR: f32 = 1.5;
/// `m_fDisplay_NPCMaxDist_NoVision`
const MAX_DIST: f32 = 100.0;
/// `m_fFadeOutDelay_NowHidden`: seconds a marker stays once its character is out of sight
const NOW_HIDDEN: f32 = 1.0;
/// `m_fMinScaleOutsideFOV`, `m_fMaxScaleOutsideFOV` (percent)
const OUTSIDE_SCALE: (f32, f32) = (0.5, 1.5);
/// how far in from the screen's edge an off-screen marker stands (stage units)
const EDGE: f32 = 70.0;
/// from the character's middle to its head
const HEAD: f32 = 0.82;
/// `fadeOut` and `quickFadeOut` (frames at 30 per second)
const FADE_OUT: f32 = 49.0 / 30.0;
const QUICK_FADE_OUT: f32 = 7.0 / 30.0;
/// `DAL_TurnToFace`
const STINGER_LEVEL: u8 = 2;
const STINGER: &str = "Music_Player_Detected";

#[derive(Resource, Default)]
struct Stinger {
    /// seconds before it may play again
    wait: f32,
}

#[derive(Component)]
struct Marker {
    npc: Entity,
    /// bolts lit
    lit: u8,
    busted: bool,
    /// going: seconds left
    leaving: Option<f32>,
    /// the attention level last seen (for the stinger)
    level: u8,
    /// seconds it still shows with its character out of sight
    shown_for: f32,
    /// standing at the screen's edge (the symbol's off-screen frame)
    edge: bool,
}

/// (tests: the script's `aware 1,2,3,4,0`) the markers step through these attention levels,
/// 1.5 s each from when first seen.
#[derive(Resource)]
pub struct ForcedLevels {
    pub levels: Vec<u8>,
    pub t0: Option<f32>,
}

#[allow(clippy::type_complexity, clippy::too_many_arguments)]
fn update_markers(
    mut commands: Commands,
    time: Res<Time>,
    window: Query<&Window, With<PrimaryWindow>>,
    cam: Query<(&Camera, &GlobalTransform), With<PlayerCamera>>,
    npcs: Query<(Entity, &Npc, &GlobalTransform, Option<&crate::npc::FromSpawner>)>,
    mut markers: Query<(Entity, &mut Marker, &mut FlashClip, &mut Node, &mut Visibility)>,
    mut timelines: ResMut<MovieTimelines>,
    mut stinger: ResMut<Stinger>,
    mut sounds: MessageWriter<crate::audio::PostEvent>,
    vm: Option<Res<crate::kismet::Vm>>,
    script_ui: Option<Res<crate::kismet::ScriptUi>>,
    forced: Option<ResMut<ForcedLevels>>,
    settings: Res<crate::settings::Settings>,
    (rapier, projection): (bevy_rapier3d::prelude::ReadRapierContext, Query<&Projection, With<PlayerCamera>>),
) {
    let dt = time.delta_secs();
    let force = forced.and_then(|mut f| {
        let now = time.elapsed_secs();
        let t0 = *f.t0.get_or_insert(now);
        let i = ((now - t0) / 1.5) as usize;
        f.levels.get(i.min(f.levels.len().saturating_sub(1))).copied()
    });
    stinger.wait = (stinger.wait - dt).max(0.0);
    let Ok(w) = window.single() else { return };
    let Ok((camera, cgt)) = cam.single() else { return };
    let s = (w.width() / 1280.0).min(w.height() / 720.0).max(0.01);
    let hud_off = script_ui.is_some_and(|u| u.hud_hidden.contains(crate::kismet::HUD_ALL));
    // the spawners of the characters the scripts show the awareness of
    let shown: std::collections::HashSet<u32> = vm.iter().flat_map(|vm| vm.awareness_shown.iter().filter_map(|a| vm.g.actors.get(*a as usize).and_then(|ka| ka.spawner))).collect();
    // (`PSI_HUD_bShowAwarenessMarkers` off: only those the scripts show)
    let wanted = |npc: &Npc, from: Option<&crate::npc::FromSpawner>| ((npc.hostile() && settings.awareness_markers) || from.is_some_and(|f| shown.contains(&f.0))) && !npc.is_down();
    let mut has: std::collections::HashSet<Entity> = std::collections::HashSet::new();
    for (e, mut m, mut fc, mut node, mut vis) in &mut markers {
        let fc = &mut *fc;
        let found = npcs.get(m.npc).ok();
        // its character gone or down: away at once
        let Some((_, npc, gt, _)) = found.filter(|(_, n, _, f)| wanted(n, *f)) else {
            match m.leaving.as_mut() {
                Some(t) => {
                    *t -= dt;
                    if *t <= 0.0 {
                        commands.entity(e).try_despawn();
                    }
                }
                None => {
                    for side in SIDES {
                        fc.goto(side, "quickFadeOut", true);
                    }
                    m.leaving = Some(QUICK_FADE_OUT);
                }
            }
            continue;
        };
        has.insert(m.npc);
        let level = force.unwrap_or(npc.attn_level).min(4);
        // the stinger, as the attention reaches turn to face
        if level >= STINGER_LEVEL && m.level < STINGER_LEVEL && npc.alert != Alert::Combat && stinger.wait <= 0.0 {
            sounds.write(crate::audio::PostEvent::named(STINGER, None));
            stinger.wait = 3.0;
        }
        m.level = level;
        if level == 0 && npc.alert != Alert::Combat {
            match m.leaving.as_mut() {
                Some(t) => {
                    *t -= dt;
                    if *t <= 0.0 {
                        commands.entity(e).try_despawn();
                        continue;
                    }
                }
                None => {
                    for side in SIDES {
                        fc.goto(side, "fadeOut", true);
                    }
                    m.leaving = Some(FADE_OUT);
                }
            }
        } else {
            if m.leaving.take().is_some() {
                for side in SIDES {
                    fc.goto(side, "fadeIn", true);
                }
            }
            if level >= 4 {
                if !m.busted {
                    m.busted = true;
                    for side in SIDES {
                        for k in 1..=3 {
                            fc.goto(&format!("{side}.mc{k}"), &format!("busted_{k}"), true);
                        }
                    }
                }
            } else {
                for k in 1..=3u8 {
                    let on = k <= level;
                    let was = !m.busted && k <= m.lit;
                    for side in SIDES {
                        let path = format!("{side}.mc{k}");
                        if on && !was {
                            fc.goto(&path, "fillIn", true);
                        } else if !on && was {
                            fc.goto(&path, "emptyOut", true);
                        } else if !on && m.busted {
                            fc.goto(&path, "empty", false);
                        }
                    }
                }
                m.busted = false;
                m.lit = level;
            }
        }
        // where: around the head, shrinking with distance, faded out close up; off the screen at
        // its edge, turned towards the character
        let head = gt.translation() + Vec3::Y * HEAD;
        let eye = cgt.translation();
        let d = head.distance(eye);
        let size = window_size(w);
        let on_screen = camera.world_to_viewport(cgt, head).ok().filter(|p| p.x >= 0.0 && p.y >= 0.0 && p.x <= size.x && p.y <= size.y && (head - eye).dot(cgt.forward().as_vec3()) > 0.0);
        // (seen: on the screen, or in a clear line from the eye)
        let clear = || {
            rapier.single().ok().is_some_and(|ctx| {
                let to = head - eye;
                let filter = bevy_rapier3d::prelude::QueryFilter::default().groups(bevy_rapier3d::prelude::CollisionGroups::new(bevy_rapier3d::prelude::Group::ALL, crate::level::GROUP_WORLD));
                ctx.cast_ray(eye, to / d.max(1e-3), (d - 0.3).max(0.0), true, filter).is_none()
            })
        };
        if on_screen.is_some() || clear() {
            m.shown_for = NOW_HIDDEN;
        } else {
            m.shown_for -= dt;
        }
        let tan = projection.single().ok().and_then(|p| if let Projection::Perspective(pp) = p { Some((pp.fov * 0.5).tan() * pp.aspect_ratio) } else { None }).unwrap_or(0.767);
        let fit = 5.0 / (tan * d).max(0.01);
        if d > MAX_DIST || hud_off || (m.shown_for <= 0.0 && level < 4) {
            if *vis != Visibility::Hidden {
                *vis = Visibility::Hidden;
            }
        } else {
            let (p, edge, turn) = match on_screen {
                Some(p) => (p, false, None),
                None => {
                    // the way to it from the screen's middle (behind: as if before)
                    let local = cgt.affine().inverse().transform_point3(head);
                    let way = Vec2::new(local.x, -local.y).normalize_or(Vec2::Y);
                    let half = size * 0.5 - Vec2::splat(EDGE * s);
                    let k = (half.x / way.x.abs().max(1e-4)).min(half.y / way.y.abs().max(1e-4));
                    (size * 0.5 + way * k, true, Some(way))
                }
            };
            if edge != m.edge {
                m.edge = edge;
                fc.goto_frame("", if edge { 2 } else { 1 }, false);
            }
            node.left = Val::Px(p.x);
            node.top = Val::Px(p.y);
            match turn {
                Some(way) => {
                    // (Flash turns clockwise; the symbol mirrored on the screen's left)
                    let deg = way.y.atan2(way.x).to_degrees() + 90.0;
                    fc.m = crate::flash::turn_scale(deg, if way.x < 0.0 { -1.0 } else { 1.0 }, 1.0);
                    fc.scale = s * fit.clamp(OUTSIDE_SCALE.0, OUTSIDE_SCALE.1);
                    fc.alpha = 1.0;
                }
                None => {
                    fc.m = crate::flash::IDENTITY;
                    fc.scale = s * SCALE * fit;
                    fc.alpha = ((d - FADE_NEAR) / (FADE_FAR - FADE_NEAR)).clamp(0.0, 1.0);
                }
            }
            if *vis != Visibility::Inherited {
                *vis = Visibility::Inherited;
            }
        }
    }
    // new markers, for those starting to notice him
    let Some(tl) = timelines.get(MOVIE) else { return };
    for (e, npc, _, from) in &npcs {
        if has.contains(&e) || force.unwrap_or(npc.attn_level) == 0 || !wanted(npc, from) {
            continue;
        }
        let Some(clip) = Clip::export(&tl, SYMBOL) else { return };
        let mut fc = FlashClip::new(MOVIE, tl.clone(), clip);
        for side in SIDES {
            fc.goto(side, "fadeIn", true);
        }
        fc.scale = s * SCALE;
        commands.spawn((
            Marker { npc: e, lit: 0, busted: false, leaving: None, level: 0, shown_for: 0.0, edge: false },
            fc,
            Node { position_type: PositionType::Absolute, ..default() },
            Visibility::Hidden,
            GlobalZIndex(-1),
            Pickable::IGNORE,
            DespawnOnExit(GameState::InGame),
        ));
    }
}

/// The window's size in its logical pixels (what the UI nodes are placed in).
fn window_size(w: &Window) -> Vec2 {
    Vec2::new(w.width(), w.height())
}
