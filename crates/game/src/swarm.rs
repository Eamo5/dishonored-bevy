//! Rat swarms: the Devouring Swarm power (and its rats, the original crowd agent's mesh and
//! animations). A swarm hunts the nearest living person, bites (`m_fDamagePerBite`, with a
//! minimum and maximum per second), eats corpses, and scatters when its time is up.
//!
//! The levels' own swarms come from their `DisRatSpawner`s (at the start, or when the scripts
//! activate them): they roam around their spawner, eat the bodies left lying, run from Corvo
//! while few, and attack him and anyone near once there are enough of them
//! (`m_InitialRatCountToBeAggressive`).

use crate::anim::{Animator, ClipId};
use crate::audio::PostEvent;
use crate::gamedata::Data;
use crate::gameplay::{HitKind, NpcHit, NpcStagger, TimeControl};
use crate::level::{GameAssets, LevelInfo, GROUP_PROP, GROUP_WORLD};
use crate::npc::{Kind, Mode, Npc};
use crate::world_light::{LitActor, WorldLighting};
use crate::GameState;
use bevy::mesh::skinning::SkinnedMesh;
use bevy::mesh::MeshTag;
use bevy::prelude::*;
use bevy_rapier3d::prelude::*;
use std::f32::consts::FRAC_PI_2;

pub struct SwarmPlugin;

/// Bites Corvo took from rats (`DisSeqEvent_AttackedByRats`).
#[derive(Resource, Default)]
pub struct RatBites(pub u32);

impl Plugin for SwarmPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<RatBites>();
        app.add_message::<SummonSwarm>()
            .add_message::<KillRats>()
            .add_systems(OnEnter(GameState::InGame), level_swarms.after(crate::level::LevelSpawnSet))
            .add_systems(Update, (summon, scripted_swarms, swarm_brain, move_rats, npc_stomps, kill_rats).chain().run_if(in_state(GameState::InGame)));
    }
}

/// Call a Devouring Swarm at a spot.
#[derive(Message, Clone, Copy)]
pub struct SummonSwarm {
    pub at: Vec3,
    pub level: u8,
}

/// Rats within reach die (explosions, the sword's sweep).
#[derive(Message, Clone, Copy)]
pub struct KillRats {
    pub at: Vec3,
    pub radius: f32,
}

/// A level swarm's behaviour, from its tweak (distances in metres).
#[derive(Clone)]
pub struct Wild {
    pub home: Vec3,
    pub roam: f32,
    pub detect: f32,
    pub escape: f32,
    /// rats needed before it attacks
    pub aggressive: usize,
    pub bite: f32,
    pub max_damage: f32,
    /// roaming: where to and for how long
    pub goal: Vec3,
    pub wait: f32,
}

#[derive(Component)]
pub struct Swarm {
    pub wild: Option<Wild>,
    /// the level's rat spawner it came from (`scene.rat_spawners`)
    pub spawner: Option<u32>,
    pub left: f32,
    pub level: u8,
    pub target: Option<Entity>,
    /// a corpse being eaten and for how long
    pub eating: Option<(Entity, f32)>,
    /// seconds before the first bite (`m_fInitialDelayBeforeDealingDamage`)
    pub delay: f32,
    pub bite_t: f32,
    pub rats: Vec<Entity>,
    pub scattering: bool,
}

#[derive(Component)]
struct Rat {
    /// place in the swarm (around its centre)
    offset: Vec3,
    speed: f32,
    yaw: f32,
    state: RatAnim,
    spawn_t: f32,
}

#[derive(Clone, Copy, PartialEq)]
enum RatAnim {
    Spawn,
    Idle,
    Run,
    Attack,
}

struct RatClips {
    spawn: Vec<ClipId>,
    idle: Vec<ClipId>,
    run: Option<ClipId>,
    walk: Option<ClipId>,
    attack: Vec<ClipId>,
}

fn clips(a: &Animator) -> RatClips {
    let lib = &a.lib;
    let all = |names: &[&str]| names.iter().filter_map(|n| lib.find(n)).collect::<Vec<_>>();
    RatClips {
        spawn: all(&["Spawn", "Spawn1", "Spawn2"]),
        idle: all(&["Idle", "Idle1", "Idle2", "Idle3", "Idle4"]),
        run: lib.find("RunN"),
        walk: lib.find("WalkN"),
        attack: all(&["SwarmAttack", "SwarmAttack1"]),
    }
}

fn pick(v: &[ClipId]) -> Option<ClipId> {
    (!v.is_empty()).then(|| v[rand::random_range(0..v.len())])
}

#[allow(clippy::too_many_arguments)]
fn summon(
    mut commands: Commands,
    mut requests: MessageReader<SummonSwarm>,
    data: Res<Data>,
    assets: Option<Res<GameAssets>>,
    level: Option<Res<LevelInfo>>,
    mut wl: Option<ResMut<WorldLighting>>,
    mut sfx: MessageWriter<PostEvent>,
) {
    for r in requests.read() {
        let (Some(assets), Some(level)) = (assets.as_ref(), level.as_ref()) else { continue };
        let Some(vis) = level.scene.rat_type.and_then(|t| assets.npc_types.get(t as usize)).and_then(|v| v.as_ref()) else {
            warn!("no rat in this map's cooked data (recook it)");
            continue;
        };
        // level 1 calls a few dozen, level 2 more (the original asks for hundreds; the
        // global manager keeps what's on screen far lower)
        let count = (data.power_f("DevouringSwarm", r.level, "m_RatSpawnerSettings.SpawnNum", 30.0) as usize).clamp(8, 60);
        let radius = (data.power_f("DevouringSwarm", r.level, "m_RatSpawnerSettings.SpawnRadius", 55.0) * 0.01).max(0.4);
        let slot = wl.as_mut().map(|w| w.alloc_slot()).unwrap_or(0);
        let mut rats = Vec::new();
        for i in 0..count {
            let a = i as f32 * 2.399 + rand::random::<f32>() * 0.5;
            let rr = radius * (0.3 + 0.7 * (i as f32 / count as f32).sqrt());
            let offset = Vec3::new(a.cos() * rr, 0.0, a.sin() * rr);
            let yaw = rand::random::<f32>() * std::f32::consts::TAU;
            let e = spawn_rat(&mut commands, vis, level.scene.rat_type, r.at + offset, yaw, slot);
            commands.entity(e).insert(Rat { offset, speed: 3.0 + rand::random::<f32>() * 1.2, yaw, state: RatAnim::Spawn, spawn_t: rand::random::<f32>() * 0.35 });
            rats.push(e);
        }
        info!("swarm: {count} rats at {:.1} (level {})", r.at, r.level);
        commands.spawn((
            Swarm {
                wild: None,
                spawner: None,
                left: data.power_f("DevouringSwarm", r.level, "m_fDuration", 30.0),
                level: r.level,
                target: None,
                eating: None,
                delay: 2.0,
                bite_t: 0.0,
                rats,
                scattering: false,
            },
            Transform::from_translation(r.at),
            LitActor { slot, probe_height: 0.3, brightness: 0.3, color: Vec3::splat(0.05), sun: 0.0, dominant: None },
            DespawnOnExit(GameState::InGame),
        ));
        sfx.write(PostEvent::named(data.power_sound("DevouringSwarm", r.level, "m_pImpactSoundEvent").unwrap_or("Snd_Power_P_Devouring_Swarm_Rat_Spawn"), Some(r.at)));
    }
}

/// Spawn a level spawner's swarm.
fn spawn_wild(commands: &mut Commands, vis: &crate::level::NpcVisual, ty: Option<u32>, def: &dhcook::format::RatSpawner, slot: u32, spawner: u32) {
    let p = |k: &str, d: f32| def.params.get(k).copied().unwrap_or(d);
    let count = def.count.clamp(1, 40) as usize;
    let at = Vec3::from(def.position);
    let radius = def.radius.clamp(0.3, 4.0);
    let mut rats = Vec::new();
    for i in 0..count {
        let a = i as f32 * 2.399 + rand::random::<f32>() * 0.5;
        let rr = (0.25 + radius * 0.6) * (0.3 + 0.7 * (i as f32 / count as f32).sqrt());
        let offset = Vec3::new(a.cos() * rr, 0.0, a.sin() * rr);
        let yaw = rand::random::<f32>() * std::f32::consts::TAU;
        let e = spawn_rat(commands, vis, ty, at + offset, yaw, slot);
        commands.entity(e).insert(Rat { offset, speed: 2.6 + rand::random::<f32>() * 1.2, yaw, state: RatAnim::Spawn, spawn_t: rand::random::<f32>() * 0.35 });
        rats.push(e);
    }
    let wild = Wild {
        home: at,
        roam: (p("m_fIdleRoamingRadius", 300.0) * 0.01).max(1.0),
        detect: p("m_fPawnDetectionRadius", 700.0) * 0.01,
        escape: p("m_fPawnEscapeRadius", 750.0) * 0.01,
        aggressive: p("m_InitialRatCountToBeAggressive", 11.0) as usize,
        bite: p("m_fDamagePerBite", 0.4),
        max_damage: p("m_fMaxDamage", 5.0),
        goal: at,
        wait: 1.0,
    };
    commands.spawn((
        Swarm { wild: Some(wild), spawner: Some(spawner), left: f32::INFINITY, level: 1, target: None, eating: None, delay: p("m_fInitialDelayBeforeDealingDamage", 2.0), bite_t: 0.0, rats, scattering: false },
        Transform::from_translation(at),
        LitActor { slot, probe_height: 0.3, brightness: 0.3, color: Vec3::splat(0.05), sun: 0.0, dominant: None },
        DespawnOnExit(GameState::InGame),
    ));
}

/// The level's swarms that are there from the start.
fn level_swarms(mut commands: Commands, assets: Option<Res<GameAssets>>, level: Option<Res<LevelInfo>>, mut wl: Option<ResMut<WorldLighting>>) {
    let (Some(assets), Some(level)) = (assets, level) else { return };
    let Some(vis) = level.scene.rat_type.and_then(|t| assets.npc_types.get(t as usize)).and_then(|v| v.as_ref()) else { return };
    let mut n = 0;
    for (si, def) in level.scene.rat_spawners.iter().enumerate().filter(|(_, d)| d.begin_play) {
        let slot = wl.as_mut().map(|w| w.alloc_slot()).unwrap_or(0);
        spawn_wild(&mut commands, vis, level.scene.rat_type, def, slot, si as u32);
        n += def.count.clamp(1, 40);
    }
    if n > 0 {
        info!("{n} rats in the level's swarms");
    }
}

/// Swarms the level scripts start.
fn scripted_swarms(mut commands: Commands, vm: Option<ResMut<crate::kismet::Vm>>, assets: Option<Res<GameAssets>>, level: Option<Res<LevelInfo>>, mut wl: Option<ResMut<WorldLighting>>) {
    let Some(mut vm) = vm else { return };
    if vm.rat_spawns.is_empty() {
        return;
    }
    let spawns = std::mem::take(&mut vm.rat_spawns);
    let (Some(assets), Some(level)) = (assets, level) else { return };
    let Some(vis) = level.scene.rat_type.and_then(|t| assets.npc_types.get(t as usize)).and_then(|v| v.as_ref()) else { return };
    for i in spawns {
        let Some(def) = level.scene.rat_spawners.get(i as usize) else { continue };
        let slot = wl.as_mut().map(|w| w.alloc_slot()).unwrap_or(0);
        info!("level scripts: rat swarm {} ({} rats)", def.name, def.count);
        spawn_wild(&mut commands, vis, level.scene.rat_type, def, slot, i);
    }
}

/// How near a rat must come to be stamped on (`DisTweaks_NPCStomp`: 250 units), when the heel
/// lands after the stomp begins, and how near it must still be then (m, s).
const STOMP_REACH: f32 = 2.5;
const STOMP_LAND: f32 = 0.45;
const STOMP_HIT: f32 = 1.4;

/// Rats underfoot (`DisBehaviorRatStomp`): a person with a rat within reach turns and stamps on
/// it (its `..StompRat` clip, the "rat" attack bark `PATROL_ATTACK_RATSINGLE`); still under the
/// heel as it lands, the rat dies (`DishonoredDamageType_Stomped`).
#[allow(clippy::type_complexity)]
fn npc_stomps(
    mut commands: Commands,
    time: Res<Time>,
    mut npcs: Query<(Entity, &mut Npc, &Transform), (Without<Rat>, Without<crate::possession::Possessed>)>,
    rats: Query<(Entity, &Transform), With<Rat>>,
    mut swarms: Query<&mut Swarm>,
    (mut cds, mut pending): (Local<std::collections::HashMap<Entity, f32>>, Local<Vec<(Entity, Entity, f32)>>),
) {
    if rats.is_empty() && pending.is_empty() {
        return;
    }
    let dt = time.delta_secs();
    for (e, mut npc, t) in &mut npcs {
        let cd = cds.entry(e).or_insert(0.0);
        *cd -= dt;
        if *cd > 0.0 || npc.is_down() || npc.kind == Kind::Creature || npc.versus || npc.mode == Mode::Choked || npc.attack_t.is_some() || npc.stagger > 0.0 {
            continue;
        }
        let near = rats
            .iter()
            .map(|(r, rt)| (r, rt.translation - t.translation))
            .filter(|(_, d)| d.with_y(0.0).length() < STOMP_REACH && d.y.abs() < 1.2)
            .min_by(|a, b| a.1.length().total_cmp(&b.1.length()));
        let Some((r, d)) = near else { continue };
        *cd = 1.2 + rand::random::<f32>() * 0.8;
        npc.yaw = (-d.x).atan2(-d.z);
        npc.gesture(8);
        npc.bark_req = (npc.bark_req.0.wrapping_add(1), "PATROL_ATTACK_RATSINGLE");
        pending.push((e, r, STOMP_LAND));
    }
    let mut landed = Vec::new();
    pending.retain_mut(|(e, r, left)| {
        *left -= dt;
        if *left > 0.0 {
            return true;
        }
        landed.push((*e, *r));
        false
    });
    for (e, r) in landed {
        let (Ok((_, npc, t)), Ok((_, rt))) = (npcs.get(e), rats.get(r)) else { continue };
        if npc.is_down() || rt.translation.distance(t.translation).max(0.0) > STOMP_HIT {
            continue;
        }
        for mut s in &mut swarms {
            s.rats.retain(|&x| x != r);
        }
        commands.entity(r).try_despawn();
        info!("{} stamps on a rat", npc.name);
    }
    cds.retain(|e, _| npcs.contains(*e));
}

fn kill_rats(mut commands: Commands, mut kills: MessageReader<KillRats>, mut swarms: Query<&mut Swarm>, rats: Query<&Transform, With<Rat>>) {
    for k in kills.read() {
        for mut s in &mut swarms {
            s.rats.retain(|&r| match rats.get(r) {
                Ok(t) if t.translation.distance(k.at) < k.radius => {
                    commands.entity(r).despawn();
                    false
                }
                _ => true,
            });
        }
    }
}

/// A swarm rat's mesh (its shadow by the option: `settings::rat_shadows`).
#[derive(Component)]
pub struct RatMesh;

fn spawn_rat(commands: &mut Commands, vis: &crate::level::NpcVisual, ty: Option<u32>, pos: Vec3, yaw: f32, slot: u32) -> Entity {
    let bones = &vis.skeleton.bones;
    let mut joints = Vec::with_capacity(bones.len());
    for b in bones {
        let q = Quat::from_array(b.rotation).normalize();
        joints.push(commands.spawn(Transform::from_translation(Vec3::from(b.translation)).with_rotation(q)).id());
    }
    let visual = commands.spawn((Transform::from_rotation(Quat::from_rotation_y(FRAC_PI_2)), Visibility::default())).id();
    for (i, b) in bones.iter().enumerate() {
        let parent = if b.parent >= 0 && (b.parent as usize) < i { joints[b.parent as usize] } else { visual };
        commands.entity(parent).add_child(joints[i]);
    }
    // its meshes, and its lesser LODs for the model details (`npc::model_details`)
    let lods = vis.lods.iter().enumerate().map(|(i, (_, p))| (i as u8 + 1, p));
    for (lod, parts) in std::iter::once((0u8, &vis.parts)).chain(lods) {
        if lod > 0 && ty.is_none() {
            break;
        }
        for (pi, (mesh, mat)) in parts.parts.iter().enumerate() {
            let mut ec = commands.spawn((
                Mesh3d(mesh.clone()),
                MeshTag(slot),
                SkinnedMesh { inverse_bindposes: vis.inverse_bindposes.clone(), joints: joints.clone() },
                bevy::camera::visibility::DynamicSkinnedMeshBounds,
                bevy::light::NotShadowCaster,
                RatMesh,
            ));
            if let Some(t) = ty {
                ec.insert(crate::npc::NpcPart { index: pi as u32, npc_type: t, lod });
            }
            if lod > 0 {
                ec.insert(Visibility::Hidden);
            }
            mat.apply(&mut ec);
            let m = ec.id();
            commands.entity(visual).add_child(m);
        }
    }
    let mut e = commands.spawn((
        Transform::from_translation(pos).with_rotation(Quat::from_rotation_y(yaw)),
        Visibility::default(),
        crate::possession::Host { npc_type: ty, rooted: false, fish: false, seat: Vec3::ZERO, facing: Vec3::NEG_Z },
        DespawnOnExit(GameState::InGame),
    ));
    e.add_child(visual);
    if let Some(lib) = vis.anims.clone() {
        let mut a = Animator::new(lib, &vis.skeleton, joints);
        let c = clips(&a);
        if let Some(s) = pick(&c.spawn).or(pick(&c.idle)) {
            a.restart(s, false, 1.0, 0.0);
        }
        e.insert(a);
    }
    e.id()
}

/// What each swarm goes for: the nearest living person (not Corvo: the Devouring Swarm
/// ignores him for longer than it lives), else the nearest body to eat.
#[allow(clippy::too_many_arguments)]
fn swarm_brain(
    mut commands: Commands,
    time: Res<Time>,
    tc: Res<TimeControl>,
    data: Res<Data>,
    rapier: ReadRapierContext,
    mut swarms: Query<(Entity, &mut Swarm, &mut Transform)>,
    npcs: Query<(Entity, &Npc, &Transform, Option<&Visibility>), Without<Swarm>>,
    mut hits: MessageWriter<NpcHit>,
    mut stagger: MessageWriter<NpcStagger>,
    mut sfx: MessageWriter<PostEvent>,
    (player, mut stats, mut bites): (Query<&Transform, (With<crate::player::Player>, Without<Swarm>, Without<Npc>)>, ResMut<crate::gameplay::PlayerStats>, ResMut<RatBites>),
    level: Option<Res<LevelInfo>>,
) {
    let dt = time.delta_secs() * tc.world_scale();
    let Ok(ctx) = rapier.single() else { return };
    let ground = QueryFilter::default().groups(CollisionGroups::new(Group::ALL, GROUP_WORLD | GROUP_PROP));
    let ppos = player.single().map(|t| t.translation).ok();
    for (se, mut s, mut st) in &mut swarms {
        if s.rats.is_empty() {
            commands.entity(se).despawn();
            continue;
        }
        if let Some(mut w) = s.wild.clone() {
            let center = st.translation;
            let n = s.rats.len();
            let big = n >= w.aggressive;
            let mut goal: Option<Vec3> = None;
            let mut busy = false;
            // Corvo: a big swarm goes for him, a small one runs
            if let Some(pp) = ppos.filter(|_| !stats.dead) {
                let d = pp.distance(center);
                if big && d < w.detect {
                    goal = Some(pp);
                    busy = true;
                    if d < 1.4 {
                        s.delay -= dt;
                        s.bite_t += dt;
                        if s.delay <= 0.0 && s.bite_t >= 1.0 {
                            s.bite_t = 0.0;
                            stats.health = (stats.health - (w.bite * n as f32).min(w.max_damage)).max(0.0);
                            stats.damage_flash = stats.damage_flash.max(0.4);
                            if stats.health <= 0.0 {
                                stats.dead = true;
                            }
                            sfx.write(PostEvent::named("Imp_Rat_on_Body", Some(pp)));
                            bites.0 += 1;
                        }
                    }
                } else if !big && d < w.escape * 0.6 {
                    let away = (center - pp).with_y(0.0).normalize_or(Vec3::X);
                    goal = Some(center + away * 3.0);
                }
            }
            // people near a big swarm are eaten alive; bodies anywhere near are eaten
            if goal.is_none() && big {
                if let Some((te, tt)) = npcs
                    .iter()
                    .filter(|(_, nn, t, _)| !nn.is_down() && matches!(nn.kind, Kind::Guard | Kind::Thug | Kind::Civilian) && t.translation.distance(center) < w.detect)
                    .min_by(|a, b| a.2.translation.distance(center).total_cmp(&b.2.translation.distance(center)))
                    .map(|(e, _, t, _)| (e, t.translation))
                {
                    goal = Some(tt);
                    busy = true;
                    if tt.distance(center) < 1.3 {
                        s.delay -= dt;
                        s.bite_t += dt;
                        if s.delay <= 0.0 && s.bite_t >= 1.0 {
                            s.bite_t = 0.0;
                            hits.write(NpcHit { npc: te, damage: (w.bite * n as f32).min(w.max_damage), kind: HitKind::ByOthers, from: center });
                            stagger.write(NpcStagger { npc: te, secs: 0.6, parried: false });
                            sfx.write(PostEvent::named("Imp_Rat_on_Body", Some(tt)));
                        }
                    }
                }
            }
            if goal.is_none() {
                let body = s.eating.map(|e| e.0).filter(|e| npcs.get(*e).is_ok_and(|(_, nn, _, v)| nn.mode == Mode::Dead && v != Some(&Visibility::Hidden))).or_else(|| {
                    npcs.iter()
                        .filter(|(_, nn, t, v)| nn.mode == Mode::Dead && !nn.corpse && *v != Some(&Visibility::Hidden) && t.translation.distance(w.home) < w.detect * 2.0)
                        .min_by(|a, b| a.2.translation.distance(center).total_cmp(&b.2.translation.distance(center)))
                        .map(|(e, _, _, _)| e)
                });
                if let Some((be, _, bt, _)) = body.and_then(|b| npcs.get(b).ok()) {
                    goal = Some(bt.translation);
                    busy = true;
                    let t = if s.eating.map(|x| x.0) == Some(be) { s.eating.unwrap().1 } else { 0.0 };
                    let t = if bt.translation.distance(center) < 1.2 { t + dt } else { t };
                    s.eating = Some((be, t));
                    if t >= 4.0 + 2.5 * 4.0 {
                        commands.entity(be).insert(Visibility::Hidden);
                        s.eating = None;
                    }
                } else {
                    s.eating = None;
                }
            }
            if !busy {
                s.delay = 2.0;
            }
            // else roam about the spawner
            let goal = goal.unwrap_or_else(|| {
                w.wait -= dt;
                if w.wait <= 0.0 || w.goal.distance(center) < 0.3 {
                    let a = rand::random::<f32>() * std::f32::consts::TAU;
                    let r = w.roam * rand::random::<f32>().sqrt();
                    w.goal = w.home + Vec3::new(a.cos() * r, 0.0, a.sin() * r);
                    w.wait = 2.0 + rand::random::<f32>() * 4.0;
                }
                w.goal
            });
            s.target = busy.then_some(se);
            let to = (goal - center).with_y(0.0);
            let speed = if busy { 3.4 } else { 1.6 };
            let step = to.normalize_or_zero() * (speed * dt).min(to.length());
            let mut next = repel(level.as_deref(), center + step);
            if let Some((_, toi)) = ctx.cast_ray(next + Vec3::Y * 1.0, Vec3::NEG_Y, 3.0, true, ground) {
                next.y = next.y + 1.0 - toi;
            }
            st.translation = next;
            s.wild = Some(w);
            continue;
        }
        s.left -= dt;
        if s.left <= -3.0 {
            for r in &s.rats {
                commands.entity(*r).despawn();
            }
            commands.entity(se).despawn();
            continue;
        }
        if s.left <= 0.0 {
            if !s.scattering {
                s.scattering = true;
                sfx.write(PostEvent::named(data.power_sound("DevouringSwarm", s.level, "m_pPowerEndSoundEvent").unwrap_or("Snd_Power_P_Devouring_Swarm_Stop"), Some(st.translation)));
            }
            continue;
        }
        let detect = data.0.actives.iter().find(|a| a.name == "DevouringSwarm").map(|_| 7.0).unwrap_or(7.0);
        let center = st.translation;
        // keep the living target while it lives and stays close; else find another
        let alive = |e: Entity| npcs.get(e).ok().filter(|(_, n, _, _)| !n.is_down());
        if s.target.and_then(alive).is_none_or(|(_, _, t, _)| t.translation.distance(center) > 9.0) {
            s.target = npcs
                .iter()
                .filter(|(_, n, t, _)| !n.is_down() && n.kind != Kind::Story && t.translation.distance(center) < detect)
                .min_by(|a, b| a.2.translation.distance(center).total_cmp(&b.2.translation.distance(center)))
                .map(|(e, _, _, _)| e);
            s.delay = 2.0;
        }
        let mut goal = center;
        if let Some((te, _, tt, _)) = s.target.and_then(alive) {
            goal = tt.translation;
            if tt.translation.distance(center) < 1.3 {
                s.delay -= dt;
                s.bite_t += dt;
                if s.delay <= 0.0 && s.bite_t >= 1.0 {
                    s.bite_t = 0.0;
                    // bites per second from the rats on it, kept within the tweak's bounds
                    let bite = 0.4 * s.rats.len() as f32;
                    hits.write(NpcHit { npc: te, damage: bite.clamp(2.0, 5.0), kind: HitKind::Rats, from: center });
                    stagger.write(NpcStagger { npc: te, secs: 0.6, parried: false });
                    sfx.write(PostEvent::named("Imp_Rat_on_Body", Some(tt.translation)));
                }
            }
            s.eating = None;
        } else {
            // a body to eat?
            let body = s.eating.map(|e| e.0).filter(|e| npcs.get(*e).is_ok_and(|(_, n, _, v)| n.mode == Mode::Dead && v != Some(&Visibility::Hidden))).or_else(|| {
                npcs.iter()
                    .filter(|(_, n, t, v)| n.mode == Mode::Dead && *v != Some(&Visibility::Hidden) && t.translation.distance(center) < detect)
                    .min_by(|a, b| a.2.translation.distance(center).total_cmp(&b.2.translation.distance(center)))
                    .map(|(e, _, _, _)| e)
            });
            match body.and_then(|b| npcs.get(b).ok()) {
                Some((be, _, bt, _)) => {
                    goal = bt.translation;
                    let t = if s.eating.map(|x| x.0) == Some(be) { s.eating.unwrap().1 } else { 0.0 };
                    let t = if bt.translation.distance(center) < 1.2 { t + dt } else { t };
                    s.eating = Some((be, t));
                    // startup then the limbs (`m_fEatStartupDuration`, `m_fEatPerLimbDuration`)
                    let eat = 1.5 + 1.5 * if s.level >= 2 { 3.0 } else { 5.0 };
                    if t >= eat {
                        commands.entity(be).insert(Visibility::Hidden);
                        s.eating = None;
                    }
                }
                None => s.eating = None,
            }
        }
        // the swarm moves as one, over the ground
        let to = (goal - center).with_y(0.0);
        let step = to.normalize_or_zero() * (3.6 * dt).min(to.length().max(0.0));
        let mut next = repel(level.as_deref(), center + step);
        if let Some((_, toi)) = ctx.cast_ray(next + Vec3::Y * 1.0, Vec3::NEG_Y, 3.0, true, ground) {
            next.y = next.y + 1.0 - toi;
        }
        st.translation = next;
    }
}

/// A swarm's step kept out of the level's rat repulsors (pushed back to their edge).
fn repel(level: Option<&LevelInfo>, at: Vec3) -> Vec3 {
    let Some(level) = level else { return at };
    let mut at = at;
    for r in &level.scene.rat_repulsors {
        let c = Vec3::from(r.position);
        let flat = (at - c).with_y(0.0);
        if (at.y - c.y).abs() < 4.0 && flat.length() < r.radius {
            at = c.with_y(at.y) + flat.normalize_or(Vec3::X) * r.radius;
        }
    }
    at
}

/// Can the Devouring Swarm be summoned here (`m_bPreventDevouringRatSpawn`)?
pub fn summon_allowed(level: &LevelInfo, at: Vec3) -> bool {
    !level.scene.rat_repulsors.iter().any(|r| r.no_summon && (at.y - r.position[1]).abs() < 4.0 && (at - Vec3::from(r.position)).with_y(0.0).length() < r.radius)
}

fn move_rats(
    time: Res<Time>,
    tc: Res<TimeControl>,
    rapier: ReadRapierContext,
    swarms: Query<(&Swarm, &Transform)>,
    mut rats: Query<(&mut Rat, &mut Transform, &mut Animator), (Without<Swarm>, Without<crate::possession::Possessed>)>,
) {
    let dt = time.delta_secs() * tc.world_scale();
    let ctx = rapier.single().ok();
    let ground = QueryFilter::default().groups(CollisionGroups::new(Group::ALL, GROUP_WORLD | GROUP_PROP));
    for (s, st) in &swarms {
        let busy = s.target.is_some() || s.eating.is_some();
        for (k, &re) in s.rats.iter().enumerate() {
            let Ok((mut r, mut t, mut a)) = rats.get_mut(re) else { continue };
            a.time_scale = tc.world_scale();
            let c = clips(&a);
            r.spawn_t += dt;
            if r.state == RatAnim::Spawn && r.spawn_t < 0.9 {
                continue;
            }
            // place: the swarm's ring, tighter on a victim; scattering rats run off
            let ring = if busy { 0.55 } else { 1.0 };
            let goal = if s.scattering {
                t.translation + r.offset.normalize_or_zero() * 6.0
            } else {
                let wobble = Vec3::new((time.elapsed_secs() * 1.3 + k as f32).sin(), 0.0, (time.elapsed_secs() * 1.1 + k as f32 * 0.7).cos()) * 0.15;
                st.translation + r.offset * ring + wobble
            };
            let to = (goal - t.translation).with_y(0.0);
            let d = to.length();
            let moving = d > 0.12;
            if moving {
                let v = to / d * (r.speed * dt).min(d);
                t.translation += v;
                let want = (-to.x).atan2(-to.z);
                let diff = (want - r.yaw + std::f32::consts::PI).rem_euclid(std::f32::consts::TAU) - std::f32::consts::PI;
                r.yaw += diff * (10.0 * dt).min(1.0);
            }
            // each rat runs over the ground under it
            let floor = ctx
                .as_ref()
                .and_then(|c| c.cast_ray(t.translation + Vec3::Y * 0.6, Vec3::NEG_Y, 1.6, true, ground))
                .map(|(_, toi)| t.translation.y + 0.6 - toi)
                .unwrap_or(st.translation.y);
            t.translation.y += (floor - t.translation.y) * (12.0 * dt).min(1.0);
            t.rotation = Quat::from_rotation_y(r.yaw);
            let want = if moving && d > 0.3 {
                RatAnim::Run
            } else if busy {
                RatAnim::Attack
            } else {
                RatAnim::Idle
            };
            if want != r.state || a.finished() {
                r.state = want;
                let clip = match want {
                    RatAnim::Run => c.run.or(c.walk),
                    RatAnim::Attack => pick(&c.attack),
                    _ => pick(&c.idle),
                };
                if let Some(cl) = clip {
                    let looping = matches!(want, RatAnim::Run);
                    a.play(cl, looping, if looping { (r.speed / 3.0).clamp(0.7, 1.6) } else { 1.0 }, 0.15);
                }
            }
        }
    }
}
