//! Characters stopping on their rounds (`DisNPCDistractor` and its
//! `DisNPCDistractionComponent`): a calm patroller passing within a distractor's range (on one
//! of its routes, when it isn't cooling down) may stop and play its scene, the original's
//! "soiree" (`DistractionSoiree.*`: scavenging the ground, leaning on a railing, a smoke) -
//! its character group's animations, the "Loop" section repeated the distractor's count
//! (`InterpTrackSoireeControl` pins), turning to face its marks, its sounds - then go on. The
//! level scripts hear of it (`DisSeqEvent_Distracted` Start / End, with the pawn). Anything
//! that alarms the character breaks it off.

use crate::anim::Animator;
use crate::audio::PostEvent;
use crate::interact::Interaction;
use crate::level::LevelInfo;
use crate::npc::{Alert, Kind, Mode, Npc, ScriptedAnim};
use crate::player::Player;
use crate::GameState;
use bevy::prelude::*;
use dhcook::format::{KMatinee, KTrack};
use std::collections::HashSet;

pub struct DistractionPlugin;

impl Plugin for DistractionPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Distractions>()
            .add_systems(OnEnter(GameState::InGame), |mut d: ResMut<Distractions>| *d = Distractions::default())
            .add_systems(Update, distract.run_if(in_state(GameState::InGame)));
    }
}

/// Each distractor's wait before it draws anyone again, and the passers-by already given
/// their chance at one (until they leave its range).
#[derive(Resource, Default)]
struct Distractions {
    cool: Vec<f32>,
    near: HashSet<(Entity, usize)>,
}

/// A character playing a distractor's scene.
#[derive(Component)]
pub struct Distracted {
    d: usize,
    t: f32,
    /// the animation key playing
    key: Option<usize>,
    /// sections still to repeat (pin, times)
    repeats: Vec<(String, u32)>,
    /// where it stood when the scene began (it settles onto the spot)
    from: Vec3,
    settle: f32,
    /// still walking there (for so long), and its home before
    walking: Option<f32>,
    home_was: Vec3,
}

/// The character group of a scene (its `DistractedPawn`; else the first with animations).
fn pawn_group(m: &KMatinee) -> Option<usize> {
    m.groups
        .iter()
        .position(|g| g.name.eq_ignore_ascii_case("DistractedPawn"))
        .or_else(|| m.groups.iter().position(|g| g.tracks.iter().any(|t| matches!(t, KTrack::Anim(_)))))
}

#[allow(clippy::too_many_arguments, clippy::type_complexity)]
fn distract(
    mut commands: Commands,
    time: Res<Time>,
    level: Option<Res<LevelInfo>>,
    mut st: ResMut<Distractions>,
    mut npcs: Query<(Entity, &mut Npc, &mut Transform, &mut Animator, Option<&mut Distracted>, Has<ScriptedAnim>)>,
    player: Query<&Transform, (With<Player>, Without<Npc>)>,
    mut used: MessageWriter<Interaction>,
    mut sfx: MessageWriter<PostEvent>,
) {
    let Some(level) = level else { return };
    let scene = &level.scene;
    // (`DH_NO_DISTRACT`: none, for comparison)
    if scene.distractors.is_empty() || std::env::var("DH_NO_DISTRACT").is_ok() {
        return;
    }
    let dt = time.delta_secs();
    if st.cool.len() != scene.distractors.len() {
        st.cool = vec![0.0; scene.distractors.len()];
    }
    for c in st.cool.iter_mut() {
        *c -= dt;
    }
    let ppos = player.single().map(|t| t.translation).ok();
    let matinees = &scene.kismet.matinees;
    for (e, mut npc, mut tf, mut animator, distracted, scripted) in &mut npcs {
        // ---- playing a scene
        if let Some(mut dz) = distracted {
            let d = &scene.distractors[dz.d];
            let m = d.matinee.and_then(|i| matinees.get(i as usize));
            let group = m.and_then(|m| pawn_group(m).map(|g| &m.groups[g]));
            let length = m.map(|m| m.length).unwrap_or(4.0);
            let alarmed = npc.is_down() || npc.alert != Alert::Unaware || npc.mode == Mode::Combat;
            let walked_off = dz.walking.is_some_and(|w| w > 10.0);
            if alarmed || dz.t >= length || group.is_none() || walked_off {
                commands.entity(e).try_remove::<(Distracted, ScriptedAnim)>();
                // back to its rounds
                npc.home = dz.home_was;
                if !alarmed && !npc.route.is_empty() {
                    npc.set_mode(Mode::Patrol);
                }
                used.write(Interaction::Distracted { distractor: d.name.clone(), at: Vec3::from(d.position), spawner: npc.spawner, start: false });
                continue;
            }
            // walking there first (its "home" for now)
            if let Some(w) = dz.walking {
                let spot = Vec3::from(d.position);
                if (spot.xz() - tf.translation.xz()).length() > 0.7 || !npc.grounded && w < 2.0 {
                    dz.walking = Some(w + dt);
                    continue;
                }
                dz.walking = None;
                dz.from = tf.translation;
                commands.entity(e).try_insert(ScriptedAnim);
                if std::env::var("DH_DISTRACT_LOG").is_ok() {
                    info!("distraction: {} at {} arrives at {:.2} after {w:.1} s", npc.name, d.name, tf.translation);
                }
            }
            let group = group.unwrap();
            if std::env::var("DH_DISTRACT_TRACE").is_ok() && dz.t < 0.5 {
                info!("distraction trace: {e} {} y {:.3} from {:.3} t {:.3} settle {:.2} dt {:.4}", npc.name, tf.translation.y, dz.from.y, dz.t, dz.settle, dt);
            }
            let prev = dz.t;
            dz.t += dt;
            // its sections repeat
            for tr in &group.tracks {
                if let KTrack::Pins(pins) = tr {
                    for (s, len, pin) in pins {
                        let end = s + len;
                        if prev < end && dz.t >= end && *len > 0.05 {
                            if let Some(r) = dz.repeats.iter_mut().find(|r| r.0.eq_ignore_ascii_case(pin)).filter(|r| r.1 > 0) {
                                r.1 -= 1;
                                dz.t -= len;
                                dz.key = None;
                            }
                        }
                    }
                }
            }
            let t = dz.t;
            // onto the spot, facing the way the scene wants
            dz.settle = (dz.settle + dt / 0.4).min(1.0);
            let spot = Vec3::new(d.position[0], tf.translation.y, d.position[2]);
            tf.translation = dz.from.lerp(spot, dz.settle);
            let mut face = d.yaw;
            for tr in &group.tracks {
                match tr {
                    KTrack::FaceTo(keys) => {
                        if let Some(k) = keys.iter().rev().find(|k| t >= k.0 && t <= k.0 + k.1.max(0.1)) {
                            let at = if k.2.eq_ignore_ascii_case("Player") { ppos } else { d.marks.iter().find(|mk| mk.0.eq_ignore_ascii_case(&k.2)).map(|mk| Vec3::from(mk.1)) };
                            if let Some(at) = at {
                                let to = (at - tf.translation).with_y(0.0);
                                if to.length() > 0.05 {
                                    face = (-to.x).atan2(-to.z);
                                }
                            }
                        }
                    }
                    KTrack::Anim(keys) => {
                        if let Some(k) = keys.iter().rposition(|k| k.0 <= t + 1e-4) {
                            if dz.key != Some(k) {
                                let (start, seq, offset, rate, looping) = &keys[k];
                                if let Some(clip) = animator.lib.find(seq) {
                                    animator.restart(clip, *looping, rate.max(0.01), 0.2);
                                    animator.seek(offset + (t - start).max(0.0) * rate);
                                }
                                dz.key = Some(k);
                            }
                        }
                    }
                    KTrack::Sound(keys) => {
                        for (kt, ev) in keys {
                            if prev < *kt && t >= *kt {
                                sfx.write(PostEvent::named(ev, Some(tf.translation)));
                            }
                        }
                    }
                    _ => {}
                }
            }
            let diff = (face - npc.yaw + std::f32::consts::PI).rem_euclid(std::f32::consts::TAU) - std::f32::consts::PI;
            npc.yaw += diff * (dt * 6.0).min(1.0);
            tf.rotation = Quat::from_rotation_y(npc.yaw);
            continue;
        }
        // ---- passing by
        if scripted || npc.is_down() || npc.mode != Mode::Patrol || npc.kind == Kind::Story || npc.alert == Alert::Combat {
            continue;
        }
        let route = scene.spawners.get(npc.spawner as usize).and_then(|sp| sp.route).and_then(|r| scene.routes.get(r as usize)).map(|r| r.name.as_str());
        let pos = tf.translation;
        for (di, d) in scene.distractors.iter().enumerate() {
            let at = Vec3::from(d.position);
            let flat = (at.xz() - pos.xz()).length();
            let reach = d.range.max(1.0) + 0.3;
            if flat > reach + 1.5 || (at.y - pos.y).abs() > 2.5 {
                st.near.remove(&(e, di));
                continue;
            }
            if flat > reach || st.near.contains(&(e, di)) {
                continue;
            }
            st.near.insert((e, di));
            if !d.routes.is_empty() && !route.is_some_and(|r| d.routes.iter().any(|x| x == r)) {
                continue;
            }
            let chance = if npc.alert == Alert::Unaware { d.chance } else { d.chance_wary };
            if st.cool[di] > 0.0 || rand::random::<f32>() >= chance {
                continue;
            }
            // only those whose kind has the scene's animations (the guards' scenes aren't the
            // weepers')
            let Some(m) = d.matinee.and_then(|i| matinees.get(i as usize)) else { continue };
            let fits = pawn_group(m).is_some_and(|g| {
                m.groups[g].tracks.iter().any(|t| matches!(t, KTrack::Anim(k) if !k.is_empty() && k.iter().all(|k| animator.lib.find(&k.1).is_some())))
            });
            if !fits {
                continue;
            }
            st.cool[di] = d.cooldown.max(5.0);
            let repeats = d.loops.iter().map(|(pin, lo, hi)| (pin.clone(), (lo + (rand::random::<f32>() * (hi - lo + 1) as f32) as u32).min(*hi).saturating_sub(1))).collect();
            // it walks to the spot (unless there), then plays the scene
            let home_was = npc.home;
            npc.home = Vec3::new(at.x, pos.y, at.z);
            npc.set_mode(Mode::Idle);
            commands.entity(e).try_insert(Distracted { d: di, t: 0.0, key: None, repeats, from: pos, settle: 0.0, walking: Some(0.0), home_was });
            used.write(Interaction::Distracted { distractor: d.name.clone(), at, spawner: npc.spawner, start: true });
            if std::env::var("DH_DISTRACT_LOG").is_ok() {
                info!("distraction: {} stops at {} ({:.1} m) from {pos:.2}, the spot {at:.2}", npc.name, d.name, flat);
            }
            break;
        }
    }
}
