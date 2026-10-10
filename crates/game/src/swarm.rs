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
use crate::gameplay::{HitKind, NpcHit, NpcStagger, PlayerStats, TimeControl};
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

fn player_bite_distance(data: &Data, crouched: bool) -> f32 {
    let (key, fallback) = if crouched {
        ("m_fCrouchedRatSwarmAttackDistance", 60.0)
    } else {
        ("m_fStandUpRatSwarmAttackDistance", 200.0)
    };
    data.pawn(key, fallback).max(0.0) * 0.01
}

fn feeding_required(data: &Data, swarm: &Swarm, spawner: Option<&dhcook::format::RatSpawner>) -> usize {
    let count = if swarm.wild.is_some() {
        spawner.and_then(|s| s.params.get("m_EatRequiredRatCount")).copied().unwrap_or(5.0)
    } else {
        summon_setting(data, swarm.level, "m_EatRequiredRatCount", 5.0)
    };
    count.max(0.0) as usize
}

fn wild_feeding_duration(spawner: Option<&dhcook::format::RatSpawner>) -> f32 {
    let param = |key, fallback| spawner.and_then(|s| s.params.get(key)).copied().unwrap_or(fallback).max(0.0);
    // Preserve the current four-stage body approximation; the stage timings come
    // from the actual swarm, with DisTweaks_RatSwarm class defaults as fallback.
    param("m_fEatStartupDuration", 2.0) + param("m_fEatPerLimbDuration", 1.85) * 4.0
}

/// Only the portion of this world-time step after the post-kill delay can feed.
fn feeding_step(corpse_age: f32, delay: f32, dt: f32) -> f32 {
    (corpse_age - delay.max(0.0)).clamp(0.0, dt.max(0.0))
}

#[cfg(test)]
mod feeding_delay_tests {
    use super::feeding_step;

    #[test]
    fn fresh_corpses_wait_while_old_corpses_feed_immediately() {
        let mut age = 0.0;
        let mut fed = 0.0;
        for _ in 0..6 {
            age += 0.2;
            fed += feeding_step(age, 1.0, 0.2);
        }
        assert!((fed - 0.2).abs() < 1e-5);
        assert!((feeding_step(1.05, 1.0, 0.1) - 0.05).abs() < 1e-5);
        assert_eq!(feeding_step(40.0, 1.0, 0.1), 0.1);
        assert_eq!(feeding_step(40.0, 1.0, 0.0), 0.0);
        assert_eq!(feeding_step(1.2, 2.0, 0.2), 0.0);
    }
}

fn summon_setting(data: &Data, level: u8, name: &str, fallback: f32) -> f32 {
    data.pawn(&format!("swarm.{}.{name}", level.clamp(1, 2)), fallback)
}

fn summoned_bite_damage(data: &Data, level: u8, rats: usize) -> f32 {
    let min = summon_setting(data, level, "m_fMinDamage", 2.0).max(0.0);
    let max = summon_setting(data, level, "m_fMaxDamage", if level >= 2 { 10.0 } else { 5.0 }).max(min);
    (summon_setting(data, level, "m_fDamagePerBite", 0.4) * rats as f32).clamp(min, max)
}

/// Bites Corvo took from rats (`DisSeqEvent_AttackedByRats`).
#[derive(Resource, Default)]
pub struct RatBites(pub u32);

impl Plugin for SwarmPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<RatBites>().init_resource::<SwarmRestore>();
        app.add_message::<SummonSwarm>()
            .add_message::<KillRats>()
            .add_systems(OnEnter(GameState::InGame), level_swarms.after(crate::level::LevelSpawnSet))
            .add_systems(Update, restore_swarms.after(crate::save::restore_npcs).before(summon).run_if(in_state(GameState::InGame)))
            .add_systems(Update, (summon, scripted_swarms, color_rats, swarm_brain, move_rats, npc_stomps, kill_rats).chain().after(crate::npc::age_corpses).run_if(in_state(GameState::InGame)));
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
    pub by_player: bool,
    /// Blade origin or blast centre, for testing cover before killing a rat.
    pub source: Vec3,
}

/// A level swarm's behaviour, from its tweak (distances in metres).
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct Wild {
    pub home: Vec3,
    pub roam: f32,
    pub detect: f32,
    pub escape: f32,
    /// rats needed before it attacks
    pub aggressive: usize,
    pub bite: f32,
    pub min_damage: f32,
    pub max_damage: f32,
    pub bite_interval: f32,
    pub initial_delay: f32,
    /// roaming: where to and for how long
    pub goal: Vec3,
    pub wait: f32,
}

impl Wild {
    fn bite_damage(&self, rats: usize) -> f32 {
        (self.bite * rats as f32).clamp(self.min_damage, self.max_damage.max(self.min_damage))
    }
}

/// Count elapsed bites without dropping time at low frame rates. The first bite
/// occurs at the initial-delay boundary, then repeats at the tweak's interval.
fn advance_bites(delay: &mut f32, elapsed: &mut f32, dt: f32, interval: f32) -> u32 {
    if dt <= 0.0 { return 0; }
    let interval = interval.max(0.01);
    let mut first = 0;
    let active = if *delay > 0.0 {
        let remaining = *delay - dt;
        *delay = remaining.max(0.0);
        if remaining > 0.0 { return 0; }
        first = 1;
        *elapsed = 0.0;
        -remaining
    } else { dt };
    *elapsed += active;
    let repeats = (*elapsed / interval).floor() as u32;
    *elapsed -= repeats as f32 * interval;
    first + repeats
}

#[cfg(test)]
mod bite_tests {
    use super::*;

    #[test]
    fn wild_feeding_respects_sewer_startup_override_and_class_defaults() {
        let mut spawner = dhcook::format::RatSpawner::default();
        assert!((wild_feeding_duration(Some(&spawner)) - 9.4).abs() < 1e-5);
        spawner.params.insert("m_fEatStartupDuration".into(), 4.0);
        spawner.params.insert("m_fEatPerLimbDuration".into(), 2.5);
        assert_eq!(wild_feeding_duration(Some(&spawner)), 14.0);
        spawner.params.insert("m_fEatStartupDuration".into(), 10.0);
        assert_eq!(wild_feeding_duration(Some(&spawner)), 20.0);
    }

    #[test]
    fn summoned_swarm_levels_have_distinct_damage_caps() {
        let mut data = Data::default();
        assert_eq!(summoned_bite_damage(&data, 1, 30), 5.0);
        assert_eq!(summoned_bite_damage(&data, 2, 30), 10.0);
        assert_eq!(summoned_bite_damage(&data, 1, 1), 2.0);
        data.0.pawn.insert("swarm.2.m_fMaxDamage".into(), 7.0);
        data.0.pawn.insert("swarm.2.m_fDamagePerBite".into(), 0.5);
        assert_eq!(summoned_bite_damage(&data, 2, 8), 4.0);
        assert_eq!(summoned_bite_damage(&data, 2, 30), 7.0);
        assert_eq!(summoned_bite_damage(&data, 1, 30), 5.0);
    }

    #[test]
    fn albinos_adds_to_original_white_rat_probability() {
        let count = |bonus| (0..100).filter(|i| is_white_rat(0.1, bonus, (*i as f32 + 0.5) / 100.0)).count();
        assert_eq!(count(0.0), 10);
        assert_eq!(count(0.15), 25);
        assert_eq!(count(-1.0), 0);
        assert_eq!(count(2.0), 100);
    }

    #[test]
    fn white_rat_material_replaces_every_lod_once() {
        let mut app = App::new();
        app.init_resource::<Data>().init_resource::<crate::gameplay::PlayerStats>()
            .init_resource::<crate::settings::Settings>()
            .insert_resource(GameAssets { white_rat_material: Some(crate::level::PartMat::Std(Handle::default())), ..default() })
            .add_systems(Update, color_rats);
        let rat = app.world_mut().spawn((RatWhiteChance(1.0), Rat { offset: Vec3::ZERO, speed: 1.0, yaw: 0.0, state: RatAnim::Idle, spawn_t: 0.0 })).id();
        let visual = app.world_mut().spawn(ChildOf(rat)).id();
        let parts: Vec<_> = (0..3).map(|_| app.world_mut().spawn((ChildOf(visual), RatMesh, MeshMaterial3d::<crate::ue3mat::Ue3Material>(Handle::default()))).id()).collect();
        app.update();
        assert!(app.world().get::<WhiteRat>(rat).is_some());
        for e in parts {
            assert!(app.world().get::<MeshMaterial3d<crate::lightmap::WorldMaterial>>(e).is_some());
            assert!(app.world().get::<MeshMaterial3d<crate::ue3mat::Ue3Material>>(e).is_none());
        }
        app.world_mut().entity_mut(rat).insert(RatWhiteChance(0.0));
        app.update();
        assert!(app.world().get::<WhiteRat>(rat).is_some());
    }

    #[test]
    fn saved_colors_override_new_spawn_rolls() {
        let mut app = App::new();
        app.init_resource::<Data>().init_resource::<crate::gameplay::PlayerStats>()
            .init_resource::<crate::settings::Settings>()
            .insert_resource(GameAssets { white_rat_material: Some(crate::level::PartMat::Std(Handle::default())), ..default() })
            .add_systems(Update, color_rats);
        let rat = Rat { offset: Vec3::X, speed: 2.0, yaw: 1.0, state: RatAnim::Run, spawn_t: 0.0 };
        let white = app.world_mut().spawn((rat.clone(), RatWhiteChance(0.0), SavedRatColor(true))).id();
        let ordinary = app.world_mut().spawn((rat, RatWhiteChance(1.0), SavedRatColor(false))).id();
        app.update();
        assert!(app.world().get::<WhiteRat>(white).is_some());
        assert!(app.world().get::<WhiteRat>(ordinary).is_none());
    }

    #[test]
    fn swarm_snapshot_keeps_survivors_colors_timers_and_stable_targets() {
        let mut world = World::new();
        let npc = world.spawn_empty().id();
        let alive = world.spawn_empty().id();
        let dead = world.spawn_empty().id();
        let rat = Rat { offset: Vec3::X, speed: 2.0, yaw: 1.0, state: RatAnim::Run, spawn_t: 0.0 };
        let pose = Transform::from_xyz(1.0, 2.0, 3.0).with_rotation(Quat::from_rotation_y(1.0));
        let swarm = Swarm { wild: None, spawner: Some(7), left: f32::INFINITY, level: 2, target: Some(npc), eating: Some((npc, 3.25)),
            delay: 0.25, bite_t: 0.125, rats: vec![alive, dead], scattering: true };
        let snapshot = SwarmsSave::capture([(&swarm, &pose)], [(alive, &rat, &pose, Some(&WhiteRat))], [(npc, 42)], 6, Some(alive));
        let restored: SwarmsSave = serde_json::from_slice(&serde_json::to_vec(&snapshot).unwrap()).unwrap();
        assert_eq!(restored.bites, 6);
        let s = &restored.swarms[0];
        assert_eq!(s.spawner, Some(7));
        assert_eq!(s.left, None);
        assert_eq!(s.target, Some(42));
        assert_eq!(s.eating, Some((42, 3.25)));
        assert_eq!(s.delay, 0.25);
        assert_eq!(s.bite_t, 0.125);
        assert!(s.scattering);
        assert_eq!(s.rats.len(), 1);
        assert!(s.rats[0].white);
        assert_eq!(s.rats[0].position, pose.translation.to_array());
        assert_eq!(s.rats[0].rotation, pose.rotation.to_array());
        assert_eq!(s.rats[0].rat.offset, Vec3::X);
    }

    #[test]
    fn bite_cadence_preserves_delay_and_partial_intervals() {
        let (mut delay, mut elapsed) = (1.0, 0.0);
        assert_eq!(advance_bites(&mut delay, &mut elapsed, 0.75, 0.5), 0);
        assert_eq!(advance_bites(&mut delay, &mut elapsed, 0.0, 0.5), 0);
        assert_eq!(delay, 0.25);
        assert_eq!(advance_bites(&mut delay, &mut elapsed, 0.5, 0.5), 1);
        assert_eq!(elapsed, 0.25);
        assert_eq!(advance_bites(&mut delay, &mut elapsed, 0.25, 0.5), 1);
        assert_eq!(advance_bites(&mut delay, &mut elapsed, 1.25, 0.5), 2);
        assert_eq!(elapsed, 0.25);
        let (mut delay, mut elapsed) = (1.0, 0.0);
        assert_eq!(advance_bites(&mut delay, &mut elapsed, 2.75, 0.5), 4);
        assert_eq!(elapsed, 0.25);
    }

    #[test]
    fn bite_damage_uses_original_minimum_and_maximum() {
        let wild = Wild { home: Vec3::ZERO, roam: 1.0, detect: 10.0, escape: 15.0,
            aggressive: 10, bite: 0.2, min_damage: 1.0, max_damage: 10.0,
            bite_interval: 0.5, initial_delay: 1.0, goal: Vec3::ZERO, wait: 0.0 };
        assert_eq!(wild.bite_damage(1), 1.0);
        assert_eq!(wild.bite_damage(10), 2.0);
        assert_eq!(wild.bite_damage(100), 10.0);
        let mut data = Data::default();
        let mut swarm = Swarm { wild: Some(wild), spawner: Some(0), left: 100.0, level: 1,
            target: None, eating: None, delay: 0.0, bite_t: 0.0, rats: vec![], scattering: false };
        let mut spawner = dhcook::format::RatSpawner::default();
        assert_eq!(feeding_required(&data, &swarm, Some(&spawner)), 5);
        spawner.params.insert("m_EatRequiredRatCount".into(), 8.0);
        assert_eq!(feeding_required(&data, &swarm, Some(&spawner)), 8);
        swarm.wild = None;
        assert_eq!(feeding_required(&data, &swarm, Some(&spawner)), 5);
        data.0.pawn.insert("swarm.2.m_EatRequiredRatCount".into(), 3.0);
        assert_eq!(feeding_required(&data, &swarm, None), 5);
        swarm.level = 2;
        assert_eq!(feeding_required(&data, &swarm, None), 3);
    }
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

#[derive(Component, Clone, serde::Serialize, serde::Deserialize)]
pub(crate) struct Rat {
    /// place in the swarm (around its centre)
    offset: Vec3,
    speed: f32,
    yaw: f32,
    state: RatAnim,
    spawn_t: f32,
}

#[derive(Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
enum RatAnim {
    Spawn,
    Idle,
    Run,
    Attack,
}

#[derive(serde::Serialize, serde::Deserialize)]
pub struct SwarmsSave {
    swarms: Vec<SwarmSave>,
    bites: u32,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct SwarmSave {
    position: [f32; 3],
    wild: Option<Wild>,
    spawner: Option<u32>,
    /// Wild swarms have no expiry; JSON cannot encode infinity.
    left: Option<f32>,
    level: u8,
    target: Option<u32>,
    eating: Option<(u32, f32)>,
    delay: f32,
    bite_t: f32,
    scattering: bool,
    rats: Vec<RatSave>,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct RatSave {
    position: [f32; 3],
    rotation: [f32; 4],
    rat: Rat,
    white: bool,
    #[serde(default)]
    possessed: bool,
}

#[derive(Resource, Default)]
pub struct SwarmRestore(pub Option<SwarmsSave>);

impl SwarmsSave {
    pub(crate) fn capture<'a>(swarms: impl IntoIterator<Item = (&'a Swarm, &'a Transform)>, rats: impl IntoIterator<Item = (Entity, &'a Rat, &'a Transform, Option<&'a WhiteRat>)>, npcs: impl IntoIterator<Item = (Entity, u32)>, bites: u32, possessed: Option<Entity>) -> Self {
        let rats: std::collections::HashMap<_, _> = rats.into_iter().map(|(e, r, t, w)| (e, (r, t, w.is_some(), Some(e) == possessed))).collect();
        let npcs: std::collections::HashMap<_, _> = npcs.into_iter().collect();
        Self { bites, swarms: swarms.into_iter().map(|(s, t)| SwarmSave {
            position: t.translation.to_array(), wild: s.wild.clone(), spawner: s.spawner,
            left: s.left.is_finite().then_some(s.left), level: s.level,
            target: s.target.and_then(|e| npcs.get(&e).copied()),
            eating: s.eating.and_then(|(e, time)| npcs.get(&e).map(|id| (*id, time))),
            delay: s.delay, bite_t: s.bite_t, scattering: s.scattering,
            rats: s.rats.iter().filter_map(|e| rats.get(e)).map(|(r, t, white, possessed)| RatSave {
                position: t.translation.to_array(), rotation: t.rotation.to_array(), rat: (*r).clone(), white: *white, possessed: *possessed,
            }).collect(),
        }).collect() }
    }
}

pub(crate) fn restore_swarms(mut commands: Commands, mut pending: ResMut<SwarmRestore>, assets: Option<Res<GameAssets>>, level: Option<Res<LevelInfo>>, mut lighting: Option<ResMut<WorldLighting>>,
    old: Query<Entity, Or<(With<Swarm>, With<Rat>)>>, npcs: Query<(Entity, &crate::npc::FromSpawner)>, mut bites: ResMut<RatBites>) {
    if pending.0.is_none() { return; }
    let (Some(assets), Some(level)) = (assets, level) else { return };
    let vis = level.scene.rat_type.and_then(|t| assets.npc_types.get(t as usize)).and_then(|v| v.as_ref());
    if vis.is_none() && pending.0.as_ref().is_some_and(|s| s.swarms.iter().any(|s| !s.rats.is_empty())) { return; }
    let saved = pending.0.take().unwrap();
    for e in &old { commands.entity(e).despawn(); }
    bites.0 = saved.bites;
    let npcs: std::collections::HashMap<_, _> = npcs.iter().map(|(e, id)| (id.0, e)).collect();
    for s in saved.swarms {
        if s.rats.is_empty() { continue; }
        let slot = lighting.as_mut().map(|l| l.alloc_slot()).unwrap_or(0);
        let mut rats = Vec::new();
        for r in s.rats {
            let e = spawn_rat(&mut commands, vis.unwrap(), level.scene.rat_type, Vec3::from(r.position), r.rat.yaw, slot);
            // A saved identity overrides random coloring, including ordinary rats.
            commands.entity(e).insert((r.rat, SavedRatColor(r.white), Transform::from_translation(Vec3::from(r.position)).with_rotation(Quat::from_array(r.rotation))));
            if r.possessed { commands.entity(e).insert(crate::possession::RestoredRatHost); }
            rats.push(e);
        }
        commands.spawn((Swarm { wild: s.wild, spawner: s.spawner, left: s.left.unwrap_or(f32::INFINITY), level: s.level,
            target: s.target.and_then(|id| npcs.get(&id).copied()), eating: s.eating.and_then(|(id, time)| npcs.get(&id).map(|e| (*e, time))),
            delay: s.delay, bite_t: s.bite_t, scattering: s.scattering, rats },
            Transform::from_translation(Vec3::from(s.position)),
            LitActor { slot, probe_height: 0.3, brightness: 0.3, color: Vec3::splat(0.05), sun: 0.0, dominant: None },
            DespawnOnExit(GameState::InGame)));
    }
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
            // Both original Devouring Swarm level tweaks override the base 10%.
            commands.entity(e).insert(RatWhiteChance(summon_setting(&data, r.level, "m_fWhiteRatRatio", 0.15)));
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
                delay: summon_setting(&data, r.level, "m_fInitialDelayBeforeDealingDamage", 2.0).max(0.0),
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
        commands.entity(e).insert(RatWhiteChance(p("m_fWhiteRatRatio", 0.1)));
        commands.entity(e).insert(Rat { offset, speed: 2.6 + rand::random::<f32>() * 1.2, yaw, state: RatAnim::Spawn, spawn_t: rand::random::<f32>() * 0.35 });
        rats.push(e);
    }
    let wild = Wild {
        home: at,
        roam: (p("m_fIdleRoamingRadius", 300.0) * 0.01).max(1.0),
        detect: p("m_fPawnDetectionRadius", 700.0) * 0.01,
        escape: p("m_fPawnEscapeRadius", 750.0) * 0.01,
        aggressive: p("m_InitialRatCountToBeAggressive", 11.0) as usize,
        bite: p("m_fDamagePerBite", 0.2),
        min_damage: p("m_fMinDamage", 1.0).max(0.0),
        max_damage: p("m_fMaxDamage", 10.0),
        bite_interval: p("m_fDamageTimeInterval", 0.5).max(0.01),
        initial_delay: p("m_fInitialDelayBeforeDealingDamage", 1.0).max(0.0),
        goal: at,
        wait: 1.0,
    };
    commands.spawn((
        Swarm { delay: wild.initial_delay, wild: Some(wild), spawner: Some(spawner), left: f32::INFINITY, level: 1, target: None, eating: None, bite_t: 0.0, rats, scattering: false },
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

fn kill_rats(mut commands: Commands, mut kills: MessageReader<KillRats>, mut swarms: Query<&mut Swarm>, rats: Query<&Transform, With<Rat>>, mut stats: ResMut<PlayerStats>, attrs: Res<crate::gamedata::Attrs>, rapier: ReadRapierContext) {
    let context = rapier.single().ok();
    for k in kills.read() {
        for mut s in &mut swarms {
            s.rats.retain(|&r| match rats.get(r) {
                Ok(t) if t.translation.distance(k.at) < k.radius => {
                    let to = t.translation + Vec3::Y * 0.05 - k.source;
                    let distance = to.length();
                    if distance > 0.05 && context.as_ref().is_some_and(|ctx| ctx.cast_ray(k.source, to / distance, distance - 0.05, true,
                        QueryFilter::default().exclude_sensors().groups(CollisionGroups::new(Group::ALL, GROUP_WORLD | GROUP_PROP))).is_some()) {
                        return true;
                    }
                    commands.entity(r).despawn();
                    if k.by_player {
                        stats.gain_adrenaline(attrs.adrenaline_rat);
                    }
                    false
                }
                _ => true,
            });
        }
    }
}

#[cfg(test)]
mod kill_tests {
    use super::*;

    #[test]
    fn wild_swarm_bites_use_player_stance_and_original_distances() {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, AssetPlugin::default(), bevy::scene::ScenePlugin, TransformPlugin, RapierPhysicsPlugin::<NoUserData>::default()))
            .init_resource::<Assets<Mesh>>().init_resource::<Data>().init_resource::<TimeControl>()
            .init_resource::<PlayerStats>().init_resource::<RatBites>()
            .add_message::<NpcHit>().add_message::<NpcStagger>().add_message::<PostEvent>()
            .insert_resource(bevy::time::TimeUpdateStrategy::ManualDuration(std::time::Duration::from_millis(100)))
            .add_systems(Last, swarm_brain);
        let player = app.world_mut().spawn((Transform::from_xyz(1.7, 0.0, 0.0), crate::player::Player {
            velocity: Vec3::ZERO, yaw: 0.0, pitch: 0.0, crouched: true, sprinting: false,
            grounded: true, lean: 0.0, noclip: false, eye_height: 1.0, locked: false,
            air_time: 0.0, spawn: Vec3::ZERO, mantle: None, step_timer: 0.0,
            fall_speed: 0.0, power_jump: 0.0, pull: Vec3::ZERO,
        })).id();
        let swarm = app.world_mut().spawn((Transform::default(), Swarm {
            wild: Some(Wild { home: Vec3::ZERO, roam: 0.0, detect: 10.0, escape: 1.0,
                aggressive: 1, bite: 1.0, min_damage: 1.0, max_damage: 1.0,
                bite_interval: 0.1, initial_delay: 0.0, goal: Vec3::ZERO, wait: 100.0 }),
            spawner: None, left: 100.0, level: 1, target: None, eating: None,
            delay: 0.0, bite_t: 0.0, rats: vec![Entity::PLACEHOLDER], scattering: false,
        })).id();
        // Reset the swarm position each frame to measure bite reach, not approach speed.
        let tick = |app: &mut App| {
            app.world_mut().get_mut::<Transform>(swarm).unwrap().translation = Vec3::ZERO;
            app.update();
        };
        tick(&mut app);
        tick(&mut app);
        assert_eq!(app.world().resource::<RatBites>().0, 0);
        app.world_mut().get_mut::<crate::player::Player>(player).unwrap().crouched = false;
        tick(&mut app);
        assert!(app.world().resource::<RatBites>().0 > 0, "standing reach is 2m, beyond the old 1.4m cutoff");
        app.world_mut().get_mut::<crate::player::Player>(player).unwrap().crouched = true;
        let before = app.world().resource::<RatBites>().0;
        app.world_mut().get_mut::<Transform>(player).unwrap().translation = Vec3::X * 0.5;
        tick(&mut app);
        assert!(app.world().resource::<RatBites>().0 > before, "crouching is vulnerable within 0.6m");
        app.world_mut().resource_mut::<Data>().0.pawn.insert("m_fCrouchedRatSwarmAttackDistance".into(), 20.0);
        let before = app.world().resource::<RatBites>().0;
        tick(&mut app);
        assert_eq!(app.world().resource::<RatBites>().0, before, "cooked overrides use centimetres");
    }

    #[derive(Resource, Default)]
    struct VisibilityResult(bool);

    #[derive(Resource, Default)]
    struct MovementResult(Vec3);

    #[test]
    fn rat_movement_sweeps_thin_cover_but_ignores_floor_and_sensors() {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, AssetPlugin::default(), bevy::scene::ScenePlugin, TransformPlugin, RapierPhysicsPlugin::<NoUserData>::default()))
            .init_resource::<Assets<Mesh>>().init_resource::<MovementResult>()
            .add_systems(Last, |ctx: ReadRapierContext, mut result: ResMut<MovementResult>| {
                result.0 = rat_ground_step(&ctx.single().unwrap(), Vec3::ZERO, Vec3::X * 2.0);
            });
        app.world_mut().spawn((Collider::cuboid(5.0, 0.1, 5.0), Transform::from_xyz(0.0, -0.1, 0.0), CollisionGroups::new(GROUP_WORLD, Group::ALL)));
        app.update();
        assert_eq!(app.world().resource::<MovementResult>().0, Vec3::X * 2.0);
        for group in [GROUP_WORLD, GROUP_PROP] {
            let wall = app.world_mut().spawn((Collider::cuboid(0.01, 0.5, 1.0), Transform::from_xyz(1.0, 0.5, 0.0), CollisionGroups::new(group, Group::ALL))).id();
            app.update();
            let stopped = app.world().resource::<MovementResult>().0;
            assert!(stopped.x > 0.85 && stopped.x < 0.93, "{stopped:?}");
            assert_eq!(stopped.y, 0.0);
            app.world_mut().entity_mut(wall).insert(Sensor);
            app.update();
            assert_eq!(app.world().resource::<MovementResult>().0, Vec3::X * 2.0);
            app.world_mut().despawn(wall);
            app.update();
            assert_eq!(app.world().resource::<MovementResult>().0, Vec3::X * 2.0);
        }
    }

    #[test]
    fn swarm_targeting_respects_walls_and_removed_cover() {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, AssetPlugin::default(), bevy::scene::ScenePlugin, TransformPlugin, RapierPhysicsPlugin::<NoUserData>::default()))
            .init_resource::<Assets<Mesh>>()
            .init_resource::<VisibilityResult>()
            .add_systems(Last, |ctx: ReadRapierContext, mut result: ResMut<VisibilityResult>| {
                result.0 = swarm_target_visible(&ctx.single().unwrap(), Vec3::ZERO, Vec3::new(2.0, 0.8, 0.0));
            });
        app.update();
        assert!(app.world().resource::<VisibilityResult>().0);
        let wall = app.world_mut().spawn((Collider::cuboid(0.1, 2.0, 2.0), Transform::from_xyz(1.0, 0.0, 0.0), CollisionGroups::new(GROUP_WORLD, Group::ALL))).id();
        app.update();
        assert!(!app.world().resource::<VisibilityResult>().0);
        app.world_mut().despawn(wall);
        app.update();
        assert!(app.world().resource::<VisibilityResult>().0);
        let prop = app.world_mut().spawn((Collider::cuboid(0.1, 2.0, 2.0), Transform::from_xyz(1.0, 0.0, 0.0), CollisionGroups::new(GROUP_PROP, Group::ALL))).id();
        app.update();
        assert!(!app.world().resource::<VisibilityResult>().0);
        app.world_mut().entity_mut(prop).insert(Sensor);
        app.update();
        assert!(app.world().resource::<VisibilityResult>().0, "trigger volumes do not block targets");
    }

    #[test]
    fn rats_slide_along_rotated_cover_and_stop_at_corners() {
        for yaw in [0.0, 0.6] {
            let rotation = Quat::from_rotation_y(yaw);
            let mut app = App::new();
            app.add_plugins((MinimalPlugins, AssetPlugin::default(), bevy::scene::ScenePlugin, TransformPlugin, RapierPhysicsPlugin::<NoUserData>::default()))
                .init_resource::<Assets<Mesh>>().init_resource::<MovementResult>()
                .add_systems(Last, move |ctx: ReadRapierContext, mut result: ResMut<MovementResult>| {
                    let ctx = ctx.single().unwrap();
                    result.0 = rotation.inverse() * rat_ground_step(&ctx, Vec3::ZERO, rotation * Vec3::new(2.0, 0.0, 2.0));
                    let touching = rotation * Vec3::new(0.95, 0.0, 0.0);
                    let away = rotation.inverse() * rat_ground_step(&ctx, touching, rotation * Vec3::NEG_X);
                    assert!((away.x + 0.05).abs() < 0.01, "separating motion must escape overlap: {away:?}");
                    assert_eq!(rat_ground_step(&ctx, touching, Vec3::ZERO), touching);
                });
            app.world_mut().spawn((Collider::cuboid(0.01, 0.5, 5.0),
                Transform::from_translation(rotation * Vec3::new(1.0, 0.5, 0.0)).with_rotation(rotation), CollisionGroups::new(GROUP_WORLD, Group::ALL)));
            app.update();
            let slid = app.world().resource::<MovementResult>().0;
            assert!(slid.x > 0.85 && slid.x < 0.93, "{slid:?}");
            assert!((slid.z - 2.0).abs() < 0.01, "tangent movement must survive: {slid:?}");
            app.world_mut().spawn((Collider::cuboid(5.0, 0.5, 0.01),
                Transform::from_translation(rotation * Vec3::new(0.0, 0.5, 1.5)).with_rotation(rotation), CollisionGroups::new(GROUP_PROP, Group::ALL)));
            app.update();
            let corner = app.world().resource::<MovementResult>().0;
            assert!(corner.x > 0.85 && corner.x < 0.93, "{corner:?}");
            assert!(corner.z > 1.35 && corner.z < 1.43, "second sweep must block corner: {corner:?}");
        }
    }

    #[test]
    fn carrion_killer_counts_each_player_rat_kill_once() {
        let mut app = App::new();
        let mut stats = PlayerStats::default();
        stats.powers.insert("BloodThirsty".into(), 1);
        app.insert_resource(stats).insert_resource(crate::gamedata::Attrs { adrenaline_rat: 10.0, ..default() })
            .init_resource::<Time>().init_resource::<TimeControl>().init_resource::<crate::kismet::ScriptSwitches>()
            .add_message::<KillRats>().add_systems(Update, kill_rats).add_systems(Last, crate::gameplay::tick_adrenaline);
        let mut rats = Vec::new();
        for x in [0.0, 10.0] {
            rats.push(app.world_mut().spawn((Rat { offset: Vec3::ZERO, speed: 0.0, yaw: 0.0, state: RatAnim::Idle, spawn_t: 0.0 }, Transform::from_xyz(x, 0.0, 0.0))).id());
        }
        app.world_mut().spawn(Swarm { wild: None, spawner: None, left: 30.0, level: 1, target: None, eating: None, delay: 0.0, bite_t: 0.0, rats, scattering: false });
        // The same rat is inside two simultaneous blasts: only the first kills it.
        for _ in 0..2 {
            app.world_mut().write_message(KillRats { at: Vec3::ZERO, radius: 1.0, by_player: true, source: Vec3::Y });
        }
        app.update();
        assert_eq!(app.world().resource::<PlayerStats>().adrenaline, 10.0);
        app.world_mut().write_message(KillRats { at: Vec3::X * 10.0, radius: 1.0, by_player: false, source: Vec3::Y });
        app.update();
        assert_eq!(app.world().resource::<PlayerStats>().adrenaline, 10.0);
        let mut q = app.world_mut().query::<&Rat>();
        assert_eq!(q.iter(app.world()).count(), 0);
    }

    #[test]
    fn solid_cover_protects_rats_but_trigger_volumes_do_not() {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, AssetPlugin::default(), bevy::scene::ScenePlugin, TransformPlugin, RapierPhysicsPlugin::<NoUserData>::default()))
            .init_resource::<Assets<Mesh>>().init_resource::<PlayerStats>()
            .init_resource::<crate::gamedata::Attrs>().add_message::<KillRats>()
            .add_systems(Last, kill_rats);
        let rat = app.world_mut().spawn((Rat { offset: Vec3::ZERO, speed: 0.0, yaw: 0.0, state: RatAnim::Idle, spawn_t: 1.0 }, Transform::from_xyz(2.0, 0.0, 0.0))).id();
        let swarm = app.world_mut().spawn(Swarm { wild: None, spawner: None, left: 30.0, level: 1, target: None, eating: None, delay: 0.0, bite_t: 0.0, rats: vec![rat], scattering: false }).id();
        let cover = app.world_mut().spawn((Collider::cuboid(0.1, 2.0, 2.0), Transform::from_xyz(1.0, 0.0, 0.0), CollisionGroups::new(GROUP_PROP, Group::ALL))).id();
        app.update();
        let strike = |app: &mut App| {
            app.world_mut().write_message(KillRats { at: Vec3::ZERO, radius: 3.0, by_player: true, source: Vec3::Y });
            app.update();
        };
        strike(&mut app);
        assert!(app.world().get::<Rat>(rat).is_some());
        assert_eq!(app.world().get::<Swarm>(swarm).unwrap().rats, vec![rat]);
        app.world_mut().entity_mut(cover).insert(Sensor);
        strike(&mut app);
        assert!(app.world().get::<Rat>(rat).is_none());
        assert!(app.world().get::<Swarm>(swarm).unwrap().rats.is_empty());
    }
}

/// A swarm rat's mesh (its shadow by the option: `settings::rat_shadows`).
#[derive(Component)]
pub struct RatMesh;

#[derive(Component)]
pub struct WhiteRat;

#[derive(Component)]
struct RatWhiteChance(f32);

#[derive(Component)]
struct SavedRatColor(bool);

fn is_white_rat(base: f32, bonus: f32, roll: f32) -> bool {
    roll < (base + bonus).clamp(0.0, 1.0)
}

fn color_rats(mut commands: Commands, rats: Query<(Entity, &RatWhiteChance, Option<&SavedRatColor>), Added<Rat>>, children: Query<&Children>, meshes: Query<(), With<RatMesh>>,
    assets: Option<Res<GameAssets>>, data: Res<Data>, stats: Res<crate::gameplay::PlayerStats>, settings: Res<crate::settings::Settings>) {
    let Some(mat) = assets.as_ref().and_then(|a| a.white_rat_material.as_ref()) else { return };
    let bonus = data.attribute("WhiteRatChanceBonus", settings.difficulty, &stats.powers, &stats.charms);
    for (e, chance, saved) in &rats {
        if !saved.map(|s| s.0).unwrap_or_else(|| is_white_rat(chance.0, bonus, rand::random::<f32>())) { continue; }
        commands.entity(e).insert(WhiteRat);
        for child in children.iter_descendants(e).filter(|c| meshes.contains(*c)) {
            let mut mesh = commands.entity(child);
            mesh.remove::<(MeshMaterial3d<crate::lightmap::WorldMaterial>, MeshMaterial3d<crate::ue3mat::Ue3Material>)>();
            mat.apply(&mut mesh);
        }
    }
}

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
        RatWhiteChance(0.1),
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
    (player, mut stats, mut bites): (Query<(&Transform, &crate::player::Player), (Without<Swarm>, Without<Npc>)>, ResMut<crate::gameplay::PlayerStats>, ResMut<RatBites>),
    level: Option<Res<LevelInfo>>,
) {
    let dt = time.delta_secs() * tc.world_scale();
    let Ok(ctx) = rapier.single() else { return };
    let ground = QueryFilter::default().exclude_sensors().groups(CollisionGroups::new(Group::ALL, GROUP_WORLD | GROUP_PROP));
    let ppos = player.single().map(|(t, p)| (t.translation, player_bite_distance(&data, p.crouched))).ok();
    for (se, mut s, mut st) in &mut swarms {
        if s.rats.is_empty() {
            commands.entity(se).despawn();
            continue;
        }
        let spawner = level.as_ref().and_then(|l| s.spawner.and_then(|i| l.scene.rat_spawners.get(i as usize)));
        let can_eat = s.rats.len() >= feeding_required(&data, &s, spawner);
        if !can_eat { s.eating = None; }
        if let Some(mut w) = s.wild.clone() {
            let center = st.translation;
            let n = s.rats.len();
            let big = n >= w.aggressive;
            let mut goal: Option<Vec3> = None;
            let mut busy = false;
            // Corvo: a big swarm goes for him, a small one runs
            if let Some((pp, attack_distance)) = ppos.filter(|_| !stats.dead) {
                let d = pp.distance(center);
                if big && d < w.detect && swarm_target_visible(&ctx, center, pp) {
                    goal = Some(pp);
                    busy = true;
                    if d < attack_distance {
                        let swarm = &mut *s;
                        let count = advance_bites(&mut swarm.delay, &mut swarm.bite_t, dt, w.bite_interval);
                        if count > 0 {
                            stats.take_damage(w.bite_damage(n) * count as f32);
                            stats.damage_flash = stats.damage_flash.max(0.4);
                            if stats.health <= 0.0 {
                                stats.dead = true;
                            }
                            sfx.write(PostEvent::named("Imp_Rat_on_Body", Some(pp)));
                            bites.0 += count;
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
                    .filter(|(_, nn, t, _)| !nn.is_down() && matches!(nn.kind, Kind::Guard | Kind::Thug | Kind::Civilian) && t.translation.distance(center) < w.detect && swarm_target_visible(&ctx, center, t.translation))
                    .min_by(|a, b| a.2.translation.distance(center).total_cmp(&b.2.translation.distance(center)))
                    .map(|(e, _, t, _)| (e, t.translation))
                {
                    goal = Some(tt);
                    busy = true;
                    if tt.distance(center) < 1.3 {
                        let swarm = &mut *s;
                        let count = advance_bites(&mut swarm.delay, &mut swarm.bite_t, dt, w.bite_interval);
                        if count > 0 {
                            hits.write(NpcHit { npc: te, damage: w.bite_damage(n) * count as f32, kind: HitKind::ByOthers, from: center });
                            stagger.write(NpcStagger { npc: te, secs: 0.6, parried: false });
                            sfx.write(PostEvent::named("Imp_Rat_on_Body", Some(tt)));
                        }
                    }
                }
            }
            if goal.is_none() && can_eat {
                let body = s.eating.map(|e| e.0).filter(|e| npcs.get(*e).is_ok_and(|(_, nn, t, v)| nn.mode == Mode::Dead && v != Some(&Visibility::Hidden) && swarm_target_visible(&ctx, center, t.translation))).or_else(|| {
                    npcs.iter()
                        .filter(|(_, nn, t, v)| nn.mode == Mode::Dead && !nn.corpse && *v != Some(&Visibility::Hidden) && t.translation.distance(w.home) < w.detect * 2.0 && swarm_target_visible(&ctx, center, t.translation))
                        .min_by(|a, b| a.2.translation.distance(center).total_cmp(&b.2.translation.distance(center)))
                        .map(|(e, _, _, _)| e)
                });
                if let Some((be, corpse, bt, _)) = body.and_then(|b| npcs.get(b).ok()) {
                    goal = Some(bt.translation);
                    busy = true;
                    let t = if s.eating.map(|x| x.0) == Some(be) { s.eating.unwrap().1 } else { 0.0 };
                    let delay = spawner.and_then(|s| s.params.get("m_fEatStartupDelayAfterKill")).copied().unwrap_or(1.0);
                    let t = if bt.translation.distance(center) < 1.2 { t + feeding_step(corpse.corpse_age, delay, dt) } else { t };
                    s.eating = Some((be, t));
                    if corpse.corpse_age >= delay.max(0.0) && t >= wild_feeding_duration(spawner) {
                        commands.entity(be).insert((crate::npc::ConsumedBody, Visibility::Hidden));
                        s.eating = None;
                    }
                } else {
                    s.eating = None;
                }
            }
            if !busy {
                s.delay = w.initial_delay;
                s.bite_t = 0.0;
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
            let desired = repel(level.as_deref(), center + step);
            let mut next = rat_ground_step(&ctx, center, desired - center);
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
        let detect = summon_setting(&data, s.level, "m_fPawnDetectionRadius", 700.0) * 0.01;
        let center = st.translation;
        // keep the living target while it lives and stays close; else find another
        let alive = |e: Entity| npcs.get(e).ok().filter(|(_, n, t, _)| !n.is_down() && swarm_target_visible(&ctx, center, t.translation));
        if s.target.and_then(alive).is_none_or(|(_, _, t, _)| t.translation.distance(center) > 9.0) {
            s.target = npcs
                .iter()
                .filter(|(_, n, t, _)| !n.is_down() && n.kind != Kind::Story && t.translation.distance(center) < detect && swarm_target_visible(&ctx, center, t.translation))
                .min_by(|a, b| a.2.translation.distance(center).total_cmp(&b.2.translation.distance(center)))
                .map(|(e, _, _, _)| e);
            s.delay = summon_setting(&data, s.level, "m_fInitialDelayBeforeDealingDamage", 2.0).max(0.0);
            s.bite_t = 0.0;
        }
        let mut goal = center;
        if let Some((te, _, tt, _)) = s.target.and_then(alive) {
            goal = tt.translation;
            if tt.translation.distance(center) < 1.3 {
                let interval = summon_setting(&data, s.level, "m_fDamageTimeInterval", 0.5);
                let swarm = &mut *s;
                let count = advance_bites(&mut swarm.delay, &mut swarm.bite_t, dt, interval);
                if count > 0 {
                    let bite = summoned_bite_damage(&data, s.level, s.rats.len());
                    hits.write(NpcHit { npc: te, damage: bite * count as f32, kind: HitKind::Rats, from: center });
                    stagger.write(NpcStagger { npc: te, secs: 0.6, parried: false });
                    sfx.write(PostEvent::named("Imp_Rat_on_Body", Some(tt.translation)));
                }
            }
            s.eating = None;
        } else if can_eat {
            // a body to eat?
            let body = s.eating.map(|e| e.0).filter(|e| npcs.get(*e).is_ok_and(|(_, n, t, v)| n.mode == Mode::Dead && v != Some(&Visibility::Hidden) && swarm_target_visible(&ctx, center, t.translation))).or_else(|| {
                npcs.iter()
                    .filter(|(_, n, t, v)| n.mode == Mode::Dead && *v != Some(&Visibility::Hidden) && t.translation.distance(center) < detect && swarm_target_visible(&ctx, center, t.translation))
                    .min_by(|a, b| a.2.translation.distance(center).total_cmp(&b.2.translation.distance(center)))
                    .map(|(e, _, _, _)| e)
            });
            match body.and_then(|b| npcs.get(b).ok()) {
                Some((be, corpse, bt, _)) => {
                    goal = bt.translation;
                    let t = if s.eating.map(|x| x.0) == Some(be) { s.eating.unwrap().1 } else { 0.0 };
                    let delay = summon_setting(&data, s.level, "m_fEatStartupDelayAfterKill", 1.0);
                    let t = if bt.translation.distance(center) < 1.2 { t + feeding_step(corpse.corpse_age, delay, dt) } else { t };
                    s.eating = Some((be, t));
                    // startup then the limbs (`m_fEatStartupDuration`, `m_fEatPerLimbDuration`)
                    let default_duration = if s.level >= 2 { 0.75 } else { 1.5 };
                    let eat = summon_setting(&data, s.level, "m_fEatStartupDuration", default_duration)
                        + summon_setting(&data, s.level, "m_fEatPerLimbDuration", default_duration) * if s.level >= 2 { 3.0 } else { 5.0 };
                    if corpse.corpse_age >= delay.max(0.0) && t >= eat {
                        commands.entity(be).insert((crate::npc::ConsumedBody, Visibility::Hidden));
                        s.eating = None;
                    }
                }
                None => s.eating = None,
            }
        }
        // the swarm moves as one, over the ground
        let to = (goal - center).with_y(0.0);
        let step = to.normalize_or_zero() * (3.6 * dt).min(to.length().max(0.0));
        let desired = repel(level.as_deref(), center + step);
        let mut next = rat_ground_step(&ctx, center, desired - center);
        if let Some((_, toi)) = ctx.cast_ray(next + Vec3::Y * 1.0, Vec3::NEG_Y, 3.0, true, ground) {
            next.y = next.y + 1.0 - toi;
        }
        st.translation = next;
    }
}

/// Advance over the ground without crossing solid cover.
fn rat_ground_step(ctx: &RapierContext, from: Vec3, step: Vec3) -> Vec3 {
    let mut remaining = step.with_y(0.0);
    let mut at = from;
    // Small ground-level volume, clear of the supporting floor. Sweeping the
    // whole frame prevents fast/scattering rats from tunnelling through cover.
    let shape = Collider::ball(0.08);
    let filter = QueryFilter::default().exclude_sensors()
        .groups(CollisionGroups::new(Group::ALL, GROUP_WORLD | GROUP_PROP));
    for _ in 0..3 {
        if remaining.length_squared() < 1e-10 { break; }
        let Some((_, hit)) = ctx.cast_shape(at + Vec3::Y * 0.16, Quat::IDENTITY, remaining, shape.raw.as_ref(),
            ShapeCastOptions { max_time_of_impact: 1.0, target_distance: 0.01, stop_at_penetration: false,
                compute_impact_geometry_on_penetration: true, ..default() }, filter) else {
            at += remaining;
            break;
        };
        let fraction = hit.time_of_impact.clamp(0.0, 1.0);
        at += remaining * fraction;
        remaining *= 1.0 - fraction;
        // Rapier's first normal is the world collider's world-space normal.
        // Remove only motion into it, then sweep the remaining tangent against
        // the next obstacle as well (corners must not bypass collision).
        let Some(normal) = hit.details.and_then(|d| d.normal1.with_y(0.0).try_normalize()) else { break };
        let inward = remaining.dot(normal);
        if inward >= -1e-6 { break; }
        remaining -= normal * inward;
    }
    at
}

/// Rats see from ground level; walls and movable cover interrupt attacks and feeding.
fn swarm_target_visible(ctx: &RapierContext, center: Vec3, target: Vec3) -> bool {
    let eye = center + Vec3::Y * 0.15;
    let to = target - eye;
    let distance = to.length();
    distance < 0.01 || ctx.cast_ray(eye, to / distance, distance, true,
        QueryFilter::default().exclude_sensors().groups(CollisionGroups::new(Group::ALL, GROUP_WORLD | GROUP_PROP))).is_none()
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
    let ground = QueryFilter::default().exclude_sensors().groups(CollisionGroups::new(Group::ALL, GROUP_WORLD | GROUP_PROP));
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
                t.translation = ctx.as_ref().map(|ctx| rat_ground_step(ctx, t.translation, v)).unwrap_or(t.translation + v);
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
