//! Daud's assassins (the Whalers) fight with the Outsider's gifts, as their left hand's
//! contexts have it (`Twk_Inv_Assassin.Twk_Inv_AssassinHand`):
//! - the Teleport Spell (`DisTweaks_NPCTeleportSpell`): gone in a swirl and back 3-8 m from
//!   Corvo, within 45° of where they were, every 15-20 s (5-10 s when he's out of reach);
//! - the Attract Spell (`DisTweaks_NPCAttractSpell`): from 8-30 m, in sight and within 60°,
//!   Corvo is dragged towards them for 4-8 s, every 20 s or more;
//! - the wrist bow (`DisTweaks_NPCFireBow`): in `npc.rs`, as their ranged attack.

use crate::audio::{event_id, PostEvent, StopEvent};
use crate::level::GROUP_WORLD;
use crate::npc::{Mode, Npc};
use crate::particles::SpawnEffect;
use crate::player::{Player, PlayerCamera};
use crate::GameState;
use bevy::prelude::*;
use bevy_rapier3d::prelude::*;

pub struct AssassinPlugin;

impl Plugin for AssassinPlugin {
    fn build(&self, app: &mut App) {
        app.add_message::<Teleported>().add_systems(Update, (mark_whalers, whaler_powers).chain().before(crate::player::PlayerMoveSet).run_if(in_state(GameState::InGame)));
    }
}

/// (min, max) seconds between teleports, and when Corvo is out of reach
const TELEPORT_COOLDOWN: (f32, f32) = (15.0, 20.0);
const TELEPORT_UNREACHABLE: (f32, f32) = (5.0, 10.0);
/// A Whaler's Teleport Spell, for the level scripts (`DisSeqEvent_TeleportSpell`
/// Disappearance / Reappearance).
#[derive(Message, Clone, Copy)]
pub struct Teleported {
    pub spawner: u32,
    pub from: Vec3,
    pub to: Vec3,
}

/// how far from Corvo they reappear (`m_DistanceFromTarget` 300-800 uu)
const TELEPORT_DISTANCE: (f32, f32) = (3.0, 8.0);
/// the Attract Spell: range (`m_fMinContextRange` 800, `m_fMaxContextRange` 3000 uu), angle,
/// duration, cooldown
const PULL_RANGE: (f32, f32) = (8.0, 30.0);
const PULL_ANGLE: f32 = 60.0;
const PULL_TIME: (f32, f32) = (4.0, 8.0);
const PULL_COOLDOWN: (f32, f32) = (20.0, 30.0);
/// the drag, rising over the spell (m/s; `m_SuctionStrength` 400 uu/s at first)
const PULL_SPEED: (f32, f32) = (4.0, 11.0);
const PULL_SOUND: &str = "AI_Assassin_Pwr_Attract_Loop";
/// The coup de grace's reach (`m_fMaxContextRange` 270 uu); no sword lock against it (its
/// `m_bDisableVersus`).
const COUP_RANGE: f32 = 2.7;

#[derive(Component)]
pub struct Whaler {
    teleport_cd: f32,
    pull_cd: f32,
    /// pulling Corvo: seconds left, and how long it lasts
    pulling: Option<(f32, f32)>,
}

fn rand_in(r: (f32, f32)) -> f32 {
    r.0 + (r.1 - r.0) * rand::random::<f32>()
}

fn mark_whalers(mut commands: Commands, npcs: Query<(Entity, &Npc), Added<Npc>>) {
    for (e, n) in &npcs {
        if n.faction.contains("Assassin") {
            commands.entity(e).try_insert(Whaler { teleport_cd: rand_in((3.0, 8.0)), pull_cd: rand_in((6.0, 12.0)), pulling: None });
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn whaler_powers(
    time: Res<Time>,
    tc: Res<crate::gameplay::TimeControl>,
    rapier: ReadRapierContext,
    mut whalers: Query<(&mut Whaler, &mut Npc, &mut Transform)>,
    mut player: Query<(&Transform, &mut Player), Without<Npc>>,
    cam: Query<Entity, With<PlayerCamera>>,
    (stats, possession, powers): (Res<crate::gameplay::PlayerStats>, Res<crate::possession::Possession>, Res<crate::powers::Powers>),
    (mut sfx, mut stop, mut fx): (MessageWriter<PostEvent>, MessageWriter<StopEvent>, MessageWriter<SpawnEffect>),
    mut was_pulling: Local<bool>,
    mut teleports: MessageWriter<Teleported>,
) {
    let (dt_world, dt_own) = (time.delta_secs() * tc.world_scale(), time.delta_secs() * tc.own_scale());
    let Ok((pt, mut p)) = player.single_mut() else { return };
    let ctx = rapier.single().ok();
    let walls = QueryFilter::default().groups(CollisionGroups::new(Group::ALL, GROUP_WORLD));
    let eye = pt.translation + Vec3::Y * 0.6;
    let clear = |from: Vec3, to: Vec3| {
        let d = to - from;
        let l = d.length();
        l < 0.01 || ctx.as_ref().is_some_and(|c| c.cast_ray(from, d / l, l - 0.2, true, walls).is_none())
    };
    let mut pull = Vec3::ZERO;
    let mut any_pulling = false;
    for (mut w, mut npc, mut t) in &mut whalers {
        let dt = if npc.out_of_bend { dt_own } else { dt_world };
        w.teleport_cd -= dt;
        w.pull_cd -= dt;
        let me = t.translation;
        let fighting = npc.mode == Mode::Combat && !npc.is_down() && npc.stagger <= 0.0 && !stats.dead && possession.host.is_none();
        // the spell holding Corvo
        if let Some((left, total)) = w.pulling {
            let to = me + Vec3::Y * 0.8 - eye;
            let d = to.length();
            let done = left <= 0.0 || !fighting || d < 1.6 || !clear(me + Vec3::Y * 1.4, eye) || powers.blink.is_some();
            if done {
                if std::env::var("DH_WHALER_LOG").is_ok() {
                    info!("whaler {} pull ends: {d:.1} m, {left:.1} s left, fighting {fighting}, attacking {}", npc.name, npc.attack_t.is_some());
                }
                w.pulling = None;
                w.pull_cd = rand_in(PULL_COOLDOWN);
                // pulled in to it: the coup de grace (`DisTweaks_NPCAttractSpellCoupDeGrace`:
                // within 2.7 m, from any angle, a big slash with its "big attack" bark)
                if fighting && d < COUP_RANGE {
                    let flat = to.with_y(0.0);
                    if flat.length() > 0.1 {
                        npc.yaw = flat.x.atan2(flat.z);
                    }
                    npc.attack_t = Some(0.0);
                    npc.swing = Some(crate::npc::Swing { kind: crate::npc::MoveKind::Short, big: true, len: 0.9, hit: 0.3, reach: COUP_RANGE, lunge: 0.0, no_versus: true });
                    npc.bark_req = (npc.bark_req.0.wrapping_add(1), "COMBAT_BIG_ATTACK");
                    info!("whaler {} strikes the pulled Corvo ({d:.1} m)", npc.name);
                }
            } else {
                w.pulling = Some((left - dt, total));
                // (its blade waits for the coup de grace)
                npc.attack_cd = npc.attack_cd.max(0.3);
                npc.attack_t = None;
                npc.swing = None;
                let k = 1.0 - left / total;
                pull += (to / d.max(0.1)) * (PULL_SPEED.0 + (PULL_SPEED.1 - PULL_SPEED.0) * k);
                any_pulling = true;
                // facing him (`to` runs from Corvo to the assassin)
                let flat = to.with_y(0.0);
                if flat.length() > 0.1 {
                    npc.yaw = flat.x.atan2(flat.z);
                }
            }
            continue;
        }
        if !fighting {
            continue;
        }
        let to = pt.translation - me;
        let d = to.length();
        let sees = clear(me + Vec3::Y * 1.4, eye);
        // the Attract Spell: from afar, in front, in sight
        let facing = to.with_y(0.0).normalize_or_zero().dot(npc.forward()) >= PULL_ANGLE.to_radians().cos();
        if w.pull_cd <= 0.0 && sees && facing && d > PULL_RANGE.0 && d < PULL_RANGE.1 && to.y.abs() < 3.0 {
            let l = rand_in(PULL_TIME);
            w.pulling = Some((l, l));
            if std::env::var("DH_WHALER_LOG").is_ok() {
                info!("whaler {} pulls Corvo from {d:.1} m for {l:.1} s", npc.name);
            }
            sfx.write(PostEvent::named(PULL_SOUND, Some(me)));
            fx.write(SpawnEffect { follow: None, secs: 4.0, ..SpawnEffect::at("assassin_pull", me + Vec3::Y * 1.2) });
            if let Ok(c) = cam.single() {
                fx.write(SpawnEffect { follow: Some(c), secs: 4.0, ..SpawnEffect::at("assassin_pull_camera", Vec3::NEG_Z * 0.4) });
            }
            continue;
        }
        // the Teleport Spell: closing in, flanking
        if w.teleport_cd <= 0.0 && (d > TELEPORT_DISTANCE.1 || !sees || rand::random::<f32>() < 0.3) {
            let unreachable = (to.y).abs() > 2.0 || !sees;
            let back = (me - pt.translation).with_y(0.0).normalize_or(Vec3::X);
            let mut landed = None;
            for _ in 0..8 {
                let a = (rand::random::<f32>() - 0.5) * 2.0 * 45f32.to_radians();
                let dir = Quat::from_rotation_y(a) * back;
                let at = pt.translation + dir * rand_in(TELEPORT_DISTANCE);
                // ground under it, room to stand, Corvo in sight
                let Some((_, down)) = ctx.as_ref().and_then(|c| c.cast_ray(at + Vec3::Y * 2.0, Vec3::NEG_Y, 5.0, true, walls)) else { continue };
                let feet = at + Vec3::Y * (2.0 - down);
                if (feet.y - (pt.translation.y - 0.9)).abs() > 3.0 || !clear(feet + Vec3::Y * 1.4, eye) {
                    continue;
                }
                landed = Some(feet + Vec3::Y * crate::npc::NPC_CENTER);
                break;
            }
            w.teleport_cd = if unreachable { rand_in(TELEPORT_UNREACHABLE) } else { rand_in(TELEPORT_COOLDOWN) };
            if let Some(at) = landed {
                if std::env::var("DH_WHALER_LOG").is_ok() {
                    info!("whaler {} teleports {:.1} m, to {:.1} m from Corvo", npc.name, me.distance(at), at.distance(pt.translation));
                }
                sfx.write(PostEvent::named("AI_Assassin_Pwr_Teleport_Start", Some(me)));
                fx.write(SpawnEffect::at("assassin_vanish", me + Vec3::Y * 0.2));
                t.translation = at;
                npc.velocity = Vec3::ZERO;
                npc.home = at;
                let flat = (pt.translation - at).with_y(0.0);
                npc.yaw = (-flat.x).atan2(-flat.z);
                t.rotation = Quat::from_rotation_y(npc.yaw);
                sfx.write(PostEvent::named("AI_Assassin_Pwr_Teleport_End", Some(at)));
                teleports.write(Teleported { spawner: npc.spawner, from: me, to: at });
                fx.write(SpawnEffect::at("assassin_appear", at + Vec3::Y * 0.2));
            }
        }
    }
    p.pull = pull;
    if *was_pulling && !any_pulling {
        stop.write(StopEvent(event_id(PULL_SOUND)));
    }
    *was_pulling = any_pulling;
}
