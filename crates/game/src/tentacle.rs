//! The witches' tentacles (The Brigmore Witches: `DLC07_Pwn_Tentacle_*`), called up by a witch
//! (`DisDLC07Tweaks_NPCSummonTentacle`) at one of the level's tentacle spots
//! (`DisDLC07TentacleSpawner`) near Daud, as its attack substate has them
//! (`DLC07_Bhvr_Tentacle_Idle_Default`'s `DisDLC07Tweaks_AISubState_TentacleAttack`):
//! - it rises (`Tentacle_PopOut_01`, `DLC07_Tentacle_Spawn_In`) and waits a moment
//!   (`m_fWaitBeforeAttack` 0.5 s);
//! - the ground about it burns whoever stands there (`m_fAOERadius` 4 m, `m_fAOEHalfHeight` 2 m,
//!   `m_AOEDamage` 1/1/2/4 by difficulty every `m_fAOEDamageInterval` 0.2 s);
//! - from afar (`m_fAttractionMinRange` 3 m to `m_fAttractionRadius` 12 m, in sight) it draws him
//!   in (`Tentacle_Pull_In`, `_Loop`, `_Out`, `DLC07_Tentacle_Pull_Cast`), every 2-4 s
//!   (`m_AttractionCooldown`), letting go once he is lost to it for `m_fNoLOSStopAttractionWait`;
//! - close (`m_fGrabStartRange` 5 m) it winds up (`m_fGrabWindupTime` 0.8 s,
//!   `Tentacle_GrabArm_WindUp`) and seizes his arm (`Tentacle_GrabArm_In`, `_Loop`; his
//!   `Ply_ArmGrab_In`, `_Loop`), the scripts told (`DisDLC07SeqEvent_GrabbedByTentacle`); he
//!   breaks free with his blade (`DisDLC07Tweaks_NPCTentacleGrab.m_LoopSettings`' presses by
//!   difficulty), every 3-5 s at most (`m_GrabWindupCooldown`);
//! - blades and bolts hurt it (its pawn's 25 health, `NoneTentacle_Hit*`); dead, or after
//!   `m_fDespawnTime` 8 s without a hold on him, it sinks (`Tentacle_DeSpawn`,
//!   `DLC07_Tentacle_Spawn_Out`), its spot's death told to the scripts (`SeqEvent_Death`: the
//!   void's Delilah). Our reading: the draw's speed (up to 6 m/s, as a whaler's).

use crate::anim::Animator;
use crate::audio::PostEvent;
use crate::gameplay::{HitKind, PlayerStats, Strikeable, Struck, TimeControl};
use crate::level::{GameAssets, LevelInfo, GROUP_PROP, GROUP_WORLD};
use crate::particles::SpawnEffect;
use crate::player::Player;
use crate::world_light::{LitActor, WorldLighting};
use crate::GameState;
use bevy::camera::visibility::DynamicSkinnedMeshBounds;
use bevy::mesh::skinning::SkinnedMesh;
use bevy::mesh::MeshTag;
use bevy::prelude::*;
use bevy_rapier3d::prelude::*;

pub struct TentaclePlugin;

impl Plugin for TentaclePlugin {
    fn build(&self, app: &mut App) {
        app.add_message::<SummonTentacle>().add_systems(
            Update,
            (summon, tentacle_hits, tentacle_brain).chain().after(crate::witch::WitchSet).after(crate::assassin::WhalerSet).before(crate::player::PlayerMoveSet).run_if(in_state(GameState::InGame)),
        );
    }
}

/// A witch calls one up: at a spot within `reach` of Daud (`near`) and 35 m of her.
#[derive(Message, Clone, Copy)]
pub struct SummonTentacle {
    pub witch: Entity,
    pub witch_at: Vec3,
    pub near: Vec3,
    pub reach: f32,
}

const WAIT_BEFORE_ATTACK: f32 = 0.5;
const AOE_RADIUS: f32 = 4.0;
const AOE_HALF_HEIGHT: f32 = 2.0;
const AOE_INTERVAL: f32 = 0.2;
const AOE_DAMAGE: [f32; 4] = [1.0, 1.0, 2.0, 4.0];
const ATTRACT_RANGE: (f32, f32) = (3.0, 12.0);
const ATTRACT_HEIGHT: f32 = 12.0;
const ATTRACT_COOLDOWN: [f32; 2] = [2.0, 4.0];
const ATTRACT_NO_LOS: f32 = 2.0;
const ATTRACT_SPEED: (f32, f32) = (2.0, 6.0);
const GRAB_RANGE: f32 = 5.0;
const GRAB_WINDUP: f32 = 0.8;
const GRAB_COOLDOWN: [f32; 2] = [3.0, 5.0];
const GRAB_PRESSES: [u32; 4] = [1, 2, 3, 4];
const DESPAWN_TIME: f32 = 8.0;
const WITCH_REACH: f32 = 35.0;
const HEALTH: f32 = 25.0;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum State {
    Rising,
    Idle,
    Pulling,
    WindUp,
    Holding,
    Letting,
    Leaving,
}

#[derive(Component)]
pub struct Tentacle {
    pub summoner: Option<Entity>,
    spawner: u32,
    state: State,
    t: f32,
    hp: f32,
    life: f32,
    aoe: f32,
    pull_cd: f32,
    grab_cd: f32,
    unseen: f32,
    collider: Entity,
    /// the floor it rises from
    ground: Vec3,
}

fn rand_in(r: [f32; 2]) -> f32 {
    r[0] + (r[1] - r[0]).max(0.0) * rand::random::<f32>()
}

fn clip(a: &mut Animator, name: &str, looping: bool) {
    if let Some(c) = a.lib.find(name) {
        a.restart(c, looping, 1.0, 0.15);
    }
}

/// The free tentacle spot nearest Daud within `reach` of him and 35 m of the witch: (spawner,
/// place, its type). The witch calls none where there is none.
pub fn spot(scene: &dhcook::format::Scene, taken: &[u32], near: Vec3, witch_at: Vec3, reach: f32) -> Option<(usize, Vec3, u32)> {
    scene
        .spawners
        .iter()
        .enumerate()
        .filter(|(i, s)| s.pawn.contains("Tentacle") && !taken.contains(&(*i as u32)))
        .filter_map(|(i, s)| s.npc_type.map(|t| (i, Vec3::from(s.position), t)))
        .filter(|(_, p, _)| p.distance(near) <= reach.max(2.0) + 1.0 && p.distance(witch_at) <= WITCH_REACH)
        .min_by(|x, y| x.1.distance(near).total_cmp(&y.1.distance(near)))
}

/// The spots in use.
pub fn taken(tentacles: &Query<&Tentacle>) -> Vec<u32> {
    tentacles.iter().map(|t| t.spawner).collect()
}

/// Called up: the nearest free spot to Daud within reach of him and of the witch.
#[allow(clippy::too_many_arguments)]
fn summon(
    mut commands: Commands,
    mut asks: MessageReader<SummonTentacle>,
    assets: Option<Res<GameAssets>>,
    level: Option<Res<LevelInfo>>,
    mut wl: Option<ResMut<WorldLighting>>,
    existing: Query<&Tentacle>,
    mut fx: MessageWriter<SpawnEffect>,
) {
    let (Some(assets), Some(level)) = (assets, level) else {
        asks.clear();
        return;
    };
    // (and those risen this frame)
    let mut busy = taken(&existing);
    for a in asks.read() {
        let Some((si, at, tid)) = spot(&level.scene, &busy, a.near, a.witch_at, a.reach) else {
            if std::env::var("DH_WITCH_LOG").is_ok() {
                info!("tentacle: no spot within {:.1} m of Daud", a.reach);
            }
            continue;
        };
        let Some(vis) = assets.npc_types.get(tid as usize).and_then(|v| v.as_ref()) else { continue };
        let slot = wl.as_mut().map(|w| w.alloc_slot()).unwrap_or(0);
        let face = (a.near - at).with_y(0.0);
        let yaw = if face.length() > 0.1 { (-face.x).atan2(-face.z) } else { 0.0 };
        busy.push(si as u32);
        spawn(&mut commands, vis, at, yaw, si as u32, Some(a.witch).filter(|w| *w != Entity::PLACEHOLDER), slot);
        fx.write(SpawnEffect::at("tentacle_in", at));
        if std::env::var("DH_WITCH_LOG").is_ok() {
            info!("tentacle: rises at {at:.1}, {:.1} m from Daud", at.distance(a.near));
        }
    }
}

fn spawn(commands: &mut Commands, vis: &crate::level::NpcVisual, at: Vec3, yaw: f32, spawner: u32, summoner: Option<Entity>, slot: u32) {
    // (its mesh hangs 2 m under its pawn, `Origin` 200: its root at its spot's floor, whence
    // it rises)
    let lift = vis.skeleton.bones.first().map(|b| (-b.translation[1]).max(0.0)).unwrap_or(0.0);
    let root = commands
        .spawn((
            Transform::from_translation(at + Vec3::Y * lift).with_rotation(Quat::from_rotation_y(yaw)),
            Visibility::default(),
            LitActor { slot, probe_height: 1.0, brightness: 0.3, color: Vec3::splat(0.05), sun: 0.0, dominant: None },
            DespawnOnExit(GameState::InGame),
        ))
        .id();
    let bones = &vis.skeleton.bones;
    let mut joints = Vec::with_capacity(bones.len());
    for b in bones {
        let local = Transform::from_translation(Vec3::from(b.translation)).with_rotation(Quat::from_array(b.rotation).normalize());
        joints.push(commands.spawn(local).id());
    }
    for (i, b) in bones.iter().enumerate() {
        let parent = if b.parent >= 0 && (b.parent as usize) < i { joints[b.parent as usize] } else { root };
        commands.entity(parent).add_child(joints[i]);
    }
    for (mesh, mat) in &vis.parts.parts {
        let mut ec = commands.spawn((
            Mesh3d(mesh.clone()),
            MeshTag(slot),
            SkinnedMesh { inverse_bindposes: vis.inverse_bindposes.clone(), joints: joints.clone() },
            DynamicSkinnedMeshBounds,
            crate::fxlight::LitPart,
        ));
        mat.apply(&mut ec);
        let m = ec.id();
        commands.entity(root).add_child(m);
    }
    // its body: what blades and bolts strike (a stalk some 2.4 m tall)
    let collider = commands.spawn((Transform::from_xyz(0.0, 1.2 - lift, 0.0), Collider::capsule_y(0.9, 0.3), CollisionGroups::new(GROUP_PROP, Group::ALL), Strikeable(root))).id();
    commands.entity(root).add_child(collider);
    let mut tentacle = Tentacle { summoner, spawner, state: State::Rising, t: 0.0, hp: HEALTH, life: DESPAWN_TIME, aoe: 0.0, pull_cd: rand_in(ATTRACT_COOLDOWN), grab_cd: 0.0, unseen: 0.0, collider, ground: at };
    if let Some(lib) = vis.anims.clone() {
        let mut a = Animator::new(lib, &vis.skeleton, joints);
        clip(&mut a, "Tentacle_PopOut_01", false);
        commands.entity(root).insert(a);
    } else {
        tentacle.state = State::Idle;
    }
    commands.entity(root).insert(tentacle);
}

/// Blades, bolts and bullets on it; dead, it sinks.
#[allow(clippy::too_many_arguments)]
fn tentacle_hits(
    mut commands: Commands,
    mut struck: MessageReader<Struck>,
    mut tentacles: Query<(&mut Tentacle, &mut Animator, &GlobalTransform)>,
    (mut fx, mut sfx): (MessageWriter<SpawnEffect>, MessageWriter<PostEvent>),
    vm: Option<ResMut<crate::kismet::Vm>>,
) {
    let mut vm = vm;
    for s in struck.read() {
        let Ok((mut t, mut a, g)) = tentacles.get_mut(s.target) else { continue };
        if matches!(t.state, State::Leaving) {
            continue;
        }
        let sword = matches!(s.kind, HitKind::Sword | HitKind::Assassinate | HitKind::Fatality);
        fx.write(SpawnEffect { rot: Transform::IDENTITY.looking_to(s.at - g.translation(), Vec3::Y).rotation, ..SpawnEffect::at(if sword { "tentacle_hit_sword" } else { "tentacle_hit" }, s.at) });
        sfx.write(PostEvent::named("Snd_DLC07_AI_Tentacles_Hit", Some(s.at)));
        t.hp -= s.damage.max(1.0);
        if t.hp <= 0.0 {
            t.state = State::Leaving;
            t.t = 0.0;
            clip(&mut a, "Tentacle_DeSpawn", false);
            commands.entity(t.collider).remove::<Strikeable>();
            // (its spot's death, for the scripts: the void's Delilah)
            if let Some(vm) = vm.as_mut() {
                if let Some(actor) = vm.actor_of_spawner(t.spawner) {
                    vm.actor_event_pub(actor, "SeqEvent_Death");
                }
            }
            continue;
        }
        // its flinch, from where it was struck
        let local = g.affine().inverse().transform_point3(s.at);
        let side = if local.z.abs() > local.x.abs() { if local.z < 0.0 { "Front" } else { "Back" } } else if local.x > 0.0 { "Right" } else { "Left" };
        if matches!(t.state, State::Idle) {
            clip(&mut a, &format!("NoneTentacle_Hit{side}"), false);
        }
    }
}

#[allow(clippy::too_many_arguments, clippy::type_complexity)]
fn tentacle_brain(
    mut commands: Commands,
    (time, tc, settings): (Res<Time>, Res<TimeControl>, Res<crate::settings::Settings>),
    rapier: ReadRapierContext,
    mut tentacles: Query<(Entity, &mut Tentacle, &mut Animator, &Transform)>,
    mut player: Query<(Entity, &Transform, &mut Player, Option<&crate::npc::Grabbed>), Without<Tentacle>>,
    mut stats: ResMut<PlayerStats>,
    possession: Res<crate::possession::Possession>,
    (mut fx, mut sfx): (MessageWriter<SpawnEffect>, MessageWriter<PostEvent>),
    vm: Option<ResMut<crate::kismet::Vm>>,
    witches: Query<&crate::npc::Npc>,
    no_target: Option<Res<crate::script::NoTarget>>,
) {
    let mut vm = vm;
    let dt = time.delta_secs() * tc.world_scale();
    let Ok((pe, pt, mut p, grabbed)) = player.single_mut() else { return };
    let ctx = rapier.single().ok();
    let walls = QueryFilter::default().exclude_sensors().groups(CollisionGroups::new(Group::ALL, GROUP_WORLD));
    let ppos = pt.translation;
    let d_i = (settings.difficulty as usize).min(3);
    let log = std::env::var("DH_WITCH_LOG").is_ok();
    let mut pull = Vec3::ZERO;
    for (e, mut t, mut a, tf) in &mut tentacles {
        t.t += dt;
        let base = t.ground;
        // (the floor from its lifted root)
        let floor = base - tf.translation;
        let to = ppos - (base + Vec3::Y * 1.2);
        let d = to.with_y(0.0).length();
        let sees = ctx.as_ref().is_none_or(|c| {
            let l = to.length();
            l < 0.1 || c.cast_ray(base + Vec3::Y * 1.6, to / l, l - 0.3, true, walls).is_none()
        });
        t.unseen = if sees { 0.0 } else { t.unseen + dt };
        let holding = grabbed.is_some_and(|g| g.by == e);
        let alive = !stats.dead && possession.host.is_none() && no_target.is_none();
        // its summoner fallen, or its time up without a hold on him: it sinks
        let summoner_down = t.summoner.is_some_and(|s| witches.get(s).is_ok_and(|n| n.is_down()) || witches.get(s).is_err());
        if !matches!(t.state, State::Leaving | State::Holding | State::Letting) {
            t.life -= dt;
            if t.life <= 0.0 || summoner_down {
                t.state = State::Leaving;
                t.t = 0.0;
                clip(&mut a, "Tentacle_DeSpawn", false);
                commands.entity(t.collider).remove::<Strikeable>();
            }
        }
        // the burning ground about it
        if !matches!(t.state, State::Rising | State::Leaving) && alive {
            t.aoe -= dt;
            let inside = d < AOE_RADIUS && (ppos.y - (base.y + AOE_HALF_HEIGHT * 0.5)).abs() < AOE_HALF_HEIGHT + 0.9;
            if inside && t.aoe <= 0.0 {
                t.aoe = AOE_INTERVAL;
                crate::gameplay::hurt_player(&mut stats, &mut sfx, AOE_DAMAGE[d_i]);
            }
        }
        match t.state {
            State::Rising => {
                if a.finished() && t.t > WAIT_BEFORE_ATTACK {
                    t.state = State::Idle;
                    t.t = 0.0;
                    clip(&mut a, "Tentacle_Idle_Thrashing", true);
                    // (on the floor it rose from: its root is lifted above it)
                    fx.write(SpawnEffect { follow: Some(e), secs: DESPAWN_TIME + 4.0, ..SpawnEffect::at("tentacle_aoe", floor) });
                }
            }
            State::Idle => {
                t.pull_cd -= dt;
                t.grab_cd -= dt;
                if a.finished() {
                    clip(&mut a, "Tentacle_Idle_Thrashing", true);
                }
                if !alive {
                    continue;
                }
                if d < GRAB_RANGE && t.grab_cd <= 0.0 && sees && grabbed.is_none() && (ppos.y - base.y).abs() < 3.0 {
                    t.state = State::WindUp;
                    t.t = 0.0;
                    clip(&mut a, "Tentacle_GrabArm_WindUp", false);
                } else if d > ATTRACT_RANGE.0 && d < ATTRACT_RANGE.1 && (ppos.y - base.y).abs() < ATTRACT_HEIGHT && t.pull_cd <= 0.0 && sees {
                    t.state = State::Pulling;
                    t.t = 0.0;
                    clip(&mut a, "Tentacle_Pull_In", false);
                    fx.write(SpawnEffect { follow: Some(e), secs: 3.0, ..SpawnEffect::at("tentacle_pull", floor + Vec3::Y * 2.0) });
                    if log {
                        info!("tentacle: draws Daud in from {d:.1} m");
                    }
                }
            }
            State::Pulling => {
                if t.t > 0.3 && a.finished() {
                    clip(&mut a, "Tentacle_Pull_Loop", true);
                }
                let done = !alive || d < GRAB_RANGE - 0.5 || t.unseen > ATTRACT_NO_LOS || t.t > 4.0;
                if done {
                    t.state = State::Idle;
                    t.pull_cd = rand_in(ATTRACT_COOLDOWN);
                    clip(&mut a, "Tentacle_Pull_Out", false);
                } else {
                    let k = (t.t / 2.0).min(1.0);
                    let dir = -to.with_y(0.0).normalize_or_zero();
                    pull += dir * (ATTRACT_SPEED.0 + (ATTRACT_SPEED.1 - ATTRACT_SPEED.0) * k);
                }
            }
            State::WindUp => {
                if t.t >= GRAB_WINDUP {
                    if alive && d < GRAB_RANGE + 0.5 && grabbed.is_none() && sees {
                        t.state = State::Holding;
                        t.t = 0.0;
                        t.life = DESPAWN_TIME;
                        clip(&mut a, "Tentacle_GrabArm_In", false);
                        commands.entity(pe).insert(crate::npc::Grabbed { by: e, presses: 0, hound: false, tentacle: true, need: GRAB_PRESSES[d_i] });
                        p.locked = true;
                        p.velocity = Vec3::ZERO;
                        sfx.write(PostEvent::named("Snd_P_Clothes_Grab_01_Cue_ak", Some(ppos)));
                        if let Some(vm) = vm.as_mut() {
                            vm.global_event("DisDLC07SeqEvent_GrabbedByTentacle");
                        }
                        if log {
                            info!("tentacle: has Daud's arm ({} presses to break free)", GRAB_PRESSES[d_i]);
                        }
                    } else {
                        t.state = State::Idle;
                        t.grab_cd = rand_in(GRAB_COOLDOWN);
                        clip(&mut a, "Tentacle_Idle_Thrashing", true);
                    }
                }
            }
            State::Holding => {
                if a.finished() {
                    clip(&mut a, "Tentacle_GrabArm_Loop", true);
                }
                // he is turned to face it, and drawn up to it
                let face = (base - ppos).with_y(0.0);
                if face.length() > 0.1 {
                    let want = (-face.x).atan2(-face.z);
                    let dy = (want - p.yaw + std::f32::consts::PI).rem_euclid(std::f32::consts::TAU) - std::f32::consts::PI;
                    p.yaw += dy * (8.0 * time.delta_secs()).min(1.0);
                }
                let broke = !holding || grabbed.is_some_and(|g| g.presses >= g.need) || stats.dead;
                if broke {
                    t.state = State::Letting;
                    t.t = 0.0;
                    t.grab_cd = rand_in(GRAB_COOLDOWN);
                    clip(&mut a, "Tentacle_GrabArm_Out", false);
                    if holding {
                        commands.entity(pe).remove::<crate::npc::Grabbed>();
                        p.locked = false;
                    }
                    if log {
                        info!("tentacle: Daud breaks free");
                    }
                }
            }
            State::Letting => {
                if a.finished() || t.t > 1.5 {
                    t.state = State::Idle;
                    t.life = t.life.max(2.0);
                    clip(&mut a, "Tentacle_Idle_Thrashing", true);
                }
            }
            State::Leaving => {
                if holding {
                    commands.entity(pe).remove::<crate::npc::Grabbed>();
                    p.locked = false;
                }
                if a.finished() || t.t > 3.0 {
                    fx.write(SpawnEffect::at("tentacle_out", base));
                    commands.entity(e).despawn();
                }
            }
        }
    }
    if pull != Vec3::ZERO {
        p.pull += pull;
    }
}

#[cfg(test)]
mod spot_tests {
    use super::*;

    fn scene() -> dhcook::format::Scene {
        let at = |x: f32, pawn: &str| dhcook::format::Spawner { name: format!("s{x}"), position: [x, 0.0, 0.0], pawn: pawn.into(), npc_type: Some(1), ..Default::default() };
        dhcook::format::Scene { spawners: vec![at(2.0, "DLC07_Pwn_Tentacle_Default"), at(5.0, "DLC07_Pwn_Tentacle_Default"), at(1.0, "DLC07_Pwn_Witch_1"), at(40.0, "DLC07_Pwn_Tentacle_Default")], ..Default::default() }
    }

    /// The nearest free tentacle spot to Daud, within reach of him and of the witch.
    #[test]
    fn the_nearest_free_spot_in_reach_is_chosen() {
        let s = scene();
        assert_eq!(spot(&s, &[], Vec3::ZERO, Vec3::ZERO, 7.0).map(|x| x.0), Some(0));
        assert_eq!(spot(&s, &[0], Vec3::ZERO, Vec3::ZERO, 7.0).map(|x| x.0), Some(1));
        assert_eq!(spot(&s, &[0, 1], Vec3::ZERO, Vec3::ZERO, 7.0), None);
        // (too far from the witch)
        assert_eq!(spot(&s, &[], Vec3::new(40.0, 0.0, 0.0), Vec3::new(-10.0, 0.0, 0.0), 7.0), None);
    }
}
