//! The awareness markers around the heads of the characters who notice Corvo: the HUD movie's
//! `awarenessIndicator` (the HUD tweak's `m_AwarenessMarkerSettings`), two mirrored fans of
//! three lightning bolts played as authored (`flash`). A bolt lights for each attention level
//! reached (head track, turn to face, investigate: `fillIn`, a red flash settling white; back
//! down, `emptyOut`), all three fly off red once he is busted (`busted_1..3`); the fans fade in
//! as the marker comes up and out once the attention has leaked away (`fadeOut`), or at once
//! when the character goes down (`quickFadeOut`). Markers stand at 75% and fade out within
//! 2 m (`m_ScaleVariation`, `m_AlphaVariation`), on enemies and on those the scripts show
//! (`DisSeqAct_OverrideAwarenessDisplay`). Reaching turn to face plays the detection stinger
//! (`m_pAwarenessStingerEvent` at `m_AttentionLevelToPlayAwarenessStinger`).

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
        // where: around the head, at 75% of the stage's scale, faded out close up
        let head = gt.translation() + Vec3::Y * HEAD;
        let d = head.distance(cgt.translation());
        let on_screen = if d <= MAX_DIST && !hud_off { camera.world_to_viewport(cgt, head).ok() } else { None };
        match on_screen {
            Some(p) => {
                node.left = Val::Px(p.x);
                node.top = Val::Px(p.y);
                fc.scale = s * SCALE;
                fc.alpha = ((d - FADE_NEAR) / (FADE_FAR - FADE_NEAR)).clamp(0.0, 1.0);
                if *vis != Visibility::Inherited {
                    *vis = Visibility::Inherited;
                }
            }
            None => {
                if *vis != Visibility::Hidden {
                    *vis = Visibility::Hidden;
                }
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
            Marker { npc: e, lit: 0, busted: false, leaving: None, level: 0 },
            fc,
            Node { position_type: PositionType::Absolute, ..default() },
            Visibility::Hidden,
            GlobalZIndex(-1),
            Pickable::IGNORE,
            DespawnOnExit(GameState::InGame),
        ));
    }
}
