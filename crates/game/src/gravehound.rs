//! Delilah's grave hounds (The Brigmore Witches: `DLC07_Pwn_Gravehound`, cooked as
//! `NpcGravehound` from its `DisDLC07Tweaks_Gravehound`), bones that won't stay down:
//! - alive, smoke trails from their spine and shoulders (`m_AliveLoopingEffects`);
//! - killed, they fall apart (`m_DeathEffects`, gone over `m_fFadeOutDuration`) and leave their
//!   skull (`m_pSkull`, `DLC07_Twk_Gravehound_Skull_V2`: `m_fSkullSpawnDelay` after, smoking,
//!   `DLC07_Skull_Idle`);
//! - while the skull lasts the hound comes back after `m_RespawnTime` (`Generic_Respawn_In`,
//!   `m_RespawnEffects`, `Gravehound_Respawn`): where the skull lies, or within
//!   `m_fRespawnLocationSearchRadius` of it if Daud stands within
//!   `m_fRespawnLocationAvoidPlayerRadius`; again and again (`m_RespawnCountLimit` -1);
//! - the skull broken (its `m_Health` by blades, bolts, bullets and blasts; `DLC07_Skull_Destroy`,
//!   `Gravehound_Skull_Impact`), it is gone for good;
//! - some lie as their skull from the start (their spawner's `m_bStartAsSkull`) and get up once
//!   disturbed (`m_StartAsSkullDisturbedSpawnTime`): the scripts calling them
//!   (`DisSeqAct_StartSpawn`: the manor's sleeping hounds waking one after another), their skull
//!   struck, or someone near them come to suspect Daud (their spawner's
//!   `m_SpawnAtSuspicionLevel`, Suspecting; our reading: someone within 30 m alerted).

use crate::audio::PostEvent;
use crate::gameplay::{HitKind, Strikeable, Struck};
use crate::level::{GameAssets, LevelInfo, GROUP_PROP, GROUP_WORLD};
use crate::npc::{Alert, Mode, Npc, NpcRig, NpcSpawned, SpawnRequest};
use crate::particles::SpawnEffect;
use crate::player::Player;
use crate::world_light::WorldLighting;
use crate::GameState;
use bevy::mesh::MeshTag;
use bevy::prelude::*;
use bevy_rapier3d::prelude::*;
use dhcook::format::NpcGravehound;

pub struct GravehoundPlugin;

impl Plugin for GravehoundPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(OnEnter(GameState::InGame), sleeping_skulls.after(crate::level::LevelSpawnSet))
            .add_systems(Update, (mark_hounds, hound_smoke, hound_deaths, skull_hits, skulls).chain().run_if(in_state(GameState::InGame)));
    }
}

/// Where its smoke trails from (`m_AliveLoopingEffects`' `Smoke_Spine_01`..`05`, `Smoke_Arm_*`
/// sockets, on these bones) and which.
const SMOKE: [(&str, &str); 7] = [
    ("spine_0_jnt", "hound_smoke_b"),
    ("spine_1_jnt", "hound_smoke_a"),
    ("spine_2_jnt", "hound_smoke_b"),
    ("spine_3_jnt", "hound_smoke_b"),
    ("neck0_jnt", "hound_smoke_a"),
    ("upper_arm_L_jnt", "hound_smoke_c"),
    ("upper_arm_R_jnt", "hound_smoke_c"),
];
/// Where it falls apart and comes back (`m_DeathEffects`, `m_RespawnEffects`' `Trans_*`).
const LIMBS: [&str; 6] = ["upper_arm_L_jnt", "upper_arm_R_jnt", "lower_arm_L_jnt", "lower_arm_R_jnt", "upper_leg_L_jnt", "upper_leg_R_jnt"];
const SPINE: [&str; 4] = ["neck1_jnt", "spine_1_jnt", "spine_2_jnt", "spine_3_jnt"];
/// How near a suspecting character must be to wake a sleeping skull (m).
const WAKE_REACH: f32 = 30.0;

/// A grave hound: its smoke's anchors (lit once they stand), and its skull left once dead.
#[derive(Component)]
pub struct Hound {
    def: NpcGravehound,
    anchors: Vec<(Entity, &'static str)>,
    lit: bool,
    /// dead: seconds since, and whether its skull is down yet
    dead: Option<(f32, bool)>,
}

/// A grave hound's skull on the ground.
#[derive(Component)]
pub struct Skull {
    pub spawner: u32,
    def: NpcGravehound,
    hp: f32,
    /// lying as it started (`m_bStartAsSkull`), till disturbed
    sleeping: bool,
    /// seconds till its hound is back
    rise: Option<f32>,
    /// the hound it was (dead, hidden): gone as it comes back
    body: Option<Entity>,
    /// it was hunting Daud: back hunting him
    hunting: bool,
    /// falling to the floor: its speed
    fall: f32,
    ground: f32,
    /// broken before its hound ever came (a sleeping one): its hound to be made, gone for good
    broken: bool,
}

fn rand_in(r: [f32; 2]) -> f32 {
    r[0] + (r[1] - r[0]).max(0.0) * rand::random::<f32>()
}

fn hound_def(level: &LevelInfo, npc: &Npc) -> Option<NpcGravehound> {
    npc.npc_type.and_then(|t| level.scene.npc_types.get(t as usize)).and_then(|t| t.stats.as_ref()).and_then(|s| s.gravehound.clone())
}

/// The floor under a place (within 3 m up and 6 m down), else the place.
fn floor(ctx: Option<&RapierContext>, at: Vec3) -> f32 {
    let walls = QueryFilter::default().exclude_sensors().groups(CollisionGroups::new(Group::ALL, GROUP_WORLD | GROUP_PROP));
    ctx.and_then(|c| c.cast_ray(at + Vec3::Y * 3.0, Vec3::NEG_Y, 9.0, true, walls)).map(|(_, t)| at.y + 3.0 - t).unwrap_or(at.y)
}

fn spawn_skull(commands: &mut Commands, assets: &GameAssets, def: &NpcGravehound, spawner: u32, at: Vec3, ground: f32, sleeping: bool, slot: u32) -> Entity {
    let e = commands
        .spawn((
            Transform::from_translation(at).with_rotation(Quat::from_rotation_y(rand::random::<f32>() * std::f32::consts::TAU)),
            Visibility::default(),
            crate::world_light::LitActor { slot, probe_height: 0.2, brightness: 0.3, color: Vec3::splat(0.05), sun: 0.0, dominant: None },
            Collider::ball(0.18),
            Sensor,
            CollisionGroups::new(GROUP_PROP, Group::ALL),
            DespawnOnExit(GameState::InGame),
        ))
        .id();
    commands.entity(e).insert((Strikeable(e), Skull { spawner, def: def.clone(), hp: def.skull_health.max(1.0), sleeping, rise: None, body: None, hunting: false, fall: 0.0, ground, broken: false }));
    for (mesh, mat) in assets.props.get(&def.prop).iter().flat_map(|p| &p.parts) {
        let mut ec = commands.spawn((Mesh3d(mesh.clone()), MeshTag(slot), Transform::IDENTITY));
        mat.apply(&mut ec);
        let m = ec.id();
        commands.entity(e).add_child(m);
    }
    e
}

/// The hounds that start as their skull lie there, asleep (their spawners make no hound yet).
fn sleeping_skulls(mut commands: Commands, assets: Option<Res<GameAssets>>, level: Option<Res<LevelInfo>>, mut wl: Option<ResMut<WorldLighting>>, rapier: ReadRapierContext, mut fx: MessageWriter<SpawnEffect>) {
    let (Some(assets), Some(level)) = (assets, level) else { return };
    let ctx = rapier.single().ok();
    let mut n = 0;
    for (i, sp) in level.scene.spawners.iter().enumerate() {
        if !sp.start_as_skull || !sp.spawn_on_begin_play {
            continue;
        }
        let Some(def) = sp.npc_type.and_then(|t| level.scene.npc_types.get(t as usize)).and_then(|t| t.stats.as_ref()).and_then(|s| s.gravehound.clone()) else { continue };
        let at = Vec3::from(sp.position);
        let ground = floor(ctx.as_ref(), at + Vec3::Y * 0.5);
        let slot = wl.as_mut().map(|w| w.alloc_slot()).unwrap_or(0);
        let e = spawn_skull(&mut commands, &assets, &def, i as u32, Vec3::new(at.x, ground + 0.12, at.z), ground, true, slot);
        fx.write(SpawnEffect { follow: Some(e), secs: f32::MAX, ..SpawnEffect::at("skull_idle", Vec3::ZERO) });
        n += 1;
    }
    if n > 0 {
        info!("{n} grave hound skulls asleep");
    }
}

/// A grave hound come: its smoke's anchors on its bones.
fn mark_hounds(mut commands: Commands, level: Option<Res<LevelInfo>>, npcs: Query<(Entity, &Npc, &NpcRig), Added<NpcRig>>) {
    let Some(level) = level else { return };
    for (e, n, rig) in &npcs {
        let Some(def) = hound_def(&level, n) else { continue };
        let mut anchors = Vec::new();
        for (bone, effect) in SMOKE {
            if let Some(j) = rig.joint(bone) {
                anchors.push((commands.spawn((Transform::IDENTITY, ChildOf(j))).id(), effect));
            }
        }
        commands.entity(e).try_insert(Hound { def, anchors, lit: false, dead: None });
    }
}

/// Its smoke, once its anchors stand.
fn hound_smoke(mut hounds: Query<&mut Hound>, placed: Query<(), With<GlobalTransform>>, mut fx: MessageWriter<SpawnEffect>) {
    for mut h in &mut hounds {
        if h.lit || h.dead.is_some() || !h.anchors.iter().all(|(a, _)| placed.contains(*a)) {
            continue;
        }
        h.lit = true;
        for (a, effect) in &h.anchors {
            fx.write(SpawnEffect { follow: Some(*a), secs: f32::MAX, ..SpawnEffect::at(effect, Vec3::ZERO) });
        }
    }
}

/// Killed: it falls apart and its skull drops where its head was.
#[allow(clippy::too_many_arguments)]
fn hound_deaths(
    mut commands: Commands,
    time: Res<Time>,
    assets: Option<Res<GameAssets>>,
    mut wl: Option<ResMut<WorldLighting>>,
    rapier: ReadRapierContext,
    mut hounds: Query<(Entity, &mut Hound, &Npc, &NpcRig, &Transform), Without<crate::npc::ConsumedBody>>,
    globals: Query<&GlobalTransform>,
    mut fx: MessageWriter<SpawnEffect>,
) {
    let Some(assets) = assets else { return };
    let ctx = rapier.single().ok();
    let at = |rig: &NpcRig, bone: &str, t: &Transform| rig.joint(bone).and_then(|j| globals.get(j).ok()).map(|g| g.translation()).unwrap_or(t.translation);
    for (e, mut h, n, rig, t) in &mut hounds {
        if n.mode != Mode::Dead {
            continue;
        }
        let Some((since, dropped)) = h.dead else {
            // (its body gone over its fade: as a shadow-killed body's)
            h.dead = Some((0.0, false));
            commands.entity(e).try_insert(crate::npc::Ashes(0.6 - h.def.fade_out));
            for (a, _) in std::mem::take(&mut h.anchors) {
                commands.entity(a).try_despawn();
            }
            fx.write(SpawnEffect::at("hound_death", at(rig, "spine_1_jnt", t)));
            for b in SPINE {
                fx.write(SpawnEffect::at("hound_death_spine", at(rig, b, t)));
            }
            for b in LIMBS {
                fx.write(SpawnEffect::at("hound_death_limbs", at(rig, b, t)));
            }
            continue;
        };
        let since = since + time.delta_secs();
        h.dead = Some((since, dropped));
        if dropped || since < h.def.skull_delay {
            continue;
        }
        h.dead = Some((since, true));
        let head = at(rig, "head_jnt", t);
        let ground = floor(ctx.as_ref(), head);
        let slot = wl.as_mut().map(|w| w.alloc_slot()).unwrap_or(0);
        let s = spawn_skull(&mut commands, &assets, &h.def, n.spawner, head, ground, false, slot);
        let hunting = n.alert == Alert::Combat;
        let def = h.def.clone();
        commands.queue(move |w: &mut World| {
            if let Some(mut sk) = w.get_mut::<Skull>(s) {
                sk.rise = Some(rand_in(def.respawn));
                sk.body = Some(e);
                sk.hunting = hunting;
            }
        });
        fx.write(SpawnEffect::at("skull_spawn", head));
        info!("grave hound {} falls apart, its skull at {:.1}", n.spawner, Vec3::new(head.x, ground, head.z));
        fx.write(SpawnEffect { follow: Some(s), secs: f32::MAX, ..SpawnEffect::at("skull_idle", Vec3::ZERO) });
    }
}

/// Blades, bolts, bullets and blasts on a skull: broken, its hound is gone for good; a sleeping
/// one struck but whole wakes.
fn skull_hits(
    mut commands: Commands,
    mut struck: MessageReader<Struck>,
    mut blasts: MessageReader<crate::gadgets::Explosion>,
    mut skulls: Query<(Entity, &mut Skull, &Transform)>,
    (mut fx, mut sfx): (MessageWriter<SpawnEffect>, MessageWriter<PostEvent>),
) {
    let mut hits: Vec<(Entity, f32)> = struck.read().filter(|s| !matches!(s.kind, HitKind::Rats | HitKind::ByOthers | HitKind::EnemyExplosion | HitKind::Choke)).map(|s| (s.target, s.damage.max(1.0))).collect();
    for b in blasts.read() {
        for (e, _, t) in &skulls {
            if t.translation.distance(b.at) < b.radius {
                hits.push((e, b.damage.max(1.0)));
            }
        }
    }
    for (target, damage) in hits {
        let Ok((e, mut s, t)) = skulls.get_mut(target) else { continue };
        if std::env::var_os("DH_NPC_LOG").is_some() {
            info!("grave hound skull {} struck for {damage}", s.spawner);
        }
        if s.hp <= 0.0 {
            continue;
        }
        s.hp -= damage;
        sfx.write(PostEvent::named("Gravehound_Skull_Impact", Some(t.translation)));
        if s.hp <= 0.0 {
            fx.write(SpawnEffect::at("skull_destroy", t.translation));
            // (its body kept as gone for good: a save brings no skull back for it; a sleeping
            // one's hound is made so, skulls)
            match s.body {
                Some(b) => {
                    commands.entity(b).try_insert(crate::npc::ConsumedBody);
                    commands.entity(e).try_despawn();
                }
                None => s.broken = true,
            }
            info!("grave hound skull broken: {} is gone", s.spawner);
        } else if s.sleeping {
            s.sleeping = false;
            s.rise = Some(rand_in(s.def.disturbed));
        }
    }
}

/// The skulls: falling to the floor; asleep, woken; their hound back on its feet.
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
fn skulls(
    mut commands: Commands,
    time: Res<Time>,
    (assets, level, settings): (Option<Res<GameAssets>>, Option<Res<LevelInfo>>, Res<crate::settings::Settings>),
    mut wl: Option<ResMut<WorldLighting>>,
    mut skulls: Query<(Entity, &mut Skull, &mut Transform)>,
    npcs: Query<(&Npc, &crate::npc::FromSpawner, &Transform), Without<Skull>>,
    player: Query<&Transform, (With<Player>, Without<Skull>, Without<Npc>)>,
    nav: Option<Res<crate::navmesh::NavGrid>>,
    mut requests: MessageReader<SpawnRequest>,
    (mut spawned, mut fx, mut sfx): (MessageWriter<NpcSpawned>, MessageWriter<SpawnEffect>, MessageWriter<PostEvent>),
) {
    let (Some(assets), Some(level)) = (assets, level) else {
        requests.clear();
        return;
    };
    let dt = time.delta_secs();
    let ppos = player.single().ok().map(|t| t.translation);
    // (the scripts calling a sleeping skull's spawner wake it)
    let called: Vec<u32> = requests.read().map(|r| r.0).collect();
    for (e, mut s, mut t) in &mut skulls {
        // broken asleep: its hound, never to come (kept in saves as a body gone)
        if s.broken {
            commands.entity(e).try_despawn();
            if let Some(h) = crate::npc::spawn_npc(&mut commands, &assets, &level.scene, wl.as_deref_mut(), s.spawner as usize, settings.difficulty) {
                commands.queue(move |w: &mut World| {
                    if let Some(mut n) = w.get_mut::<Npc>(h) {
                        n.set_mode(Mode::Dead);
                        n.down_t = 1.0;
                        n.health = 0.0;
                        n.corpse = true;
                    }
                    w.entity_mut(h).insert((crate::npc::ConsumedBody, Visibility::Hidden, ColliderDisabled));
                });
            }
            continue;
        }
        // falling to the floor
        if t.translation.y > s.ground + 0.12 {
            s.fall += 9.81 * dt;
            t.translation.y = (t.translation.y - s.fall * dt).max(s.ground + 0.12);
        }
        if s.sleeping {
            // (its hound already come, as a save had it: no sleeping skull; a dead one's own
            // comes back with it)
            if npcs.iter().any(|(_, f, _)| f.0 == s.spawner) {
                commands.entity(e).try_despawn();
                continue;
            }
            let wakes = level.scene.spawners.get(s.spawner as usize).is_some_and(|sp| sp.spawn_at.is_some());
            let near_alarm = wakes && npcs.iter().any(|(n, _, nt)| n.enemy && !n.is_down() && n.alert != Alert::Unaware && nt.translation.distance(t.translation) < WAKE_REACH);
            if called.contains(&s.spawner) || near_alarm {
                s.sleeping = false;
                s.hunting = near_alarm;
                s.rise = Some(rand_in(s.def.disturbed));
            }
            continue;
        }
        let Some(left) = s.rise else { continue };
        if left > dt {
            s.rise = Some(left - dt);
            continue;
        }
        // back: where the skull lies, or away from Daud within its reach of it
        let skull_at = t.translation;
        let mut at = Vec3::new(skull_at.x, s.ground, skull_at.z);
        if let Some(p) = ppos.filter(|p| p.distance(at) < s.def.avoid_player) {
            let away = (at - p).with_y(0.0).normalize_or(Vec3::X);
            let mut best = None;
            for k in 0..16 {
                let a = (k as f32 / 16.0) * std::f32::consts::TAU;
                let dir = Quat::from_rotation_y(a) * away;
                let want = at + dir * s.def.respawn_reach * (0.5 + 0.5 * rand::random::<f32>());
                let Some(q) = nav.as_ref().and_then(|g| g.snap(want, 3.0)) else { continue };
                if q.distance(p) >= s.def.avoid_player && q.distance(at) <= s.def.respawn_reach + 1.0 {
                    let score = q.distance(at);
                    if best.is_none_or(|(_, b)| score < b) {
                        best = Some((q, score));
                    }
                }
            }
            if let Some((q, _)) = best {
                at = q;
            }
        }
        let slot_owner = s.spawner;
        if let Some(b) = s.body.take() {
            commands.entity(b).try_despawn();
        }
        commands.entity(e).try_despawn();
        let Some(h) = crate::npc::spawn_npc(&mut commands, &assets, &level.scene, wl.as_deref_mut(), slot_owner as usize, settings.difficulty) else { continue };
        spawned.write(NpcSpawned { spawner: slot_owner, entity: h });
        let face = ppos.map(|p| (p - at).with_y(0.0)).filter(|f| f.length() > 0.1).map(|f| (-f.x).atan2(-f.z)).unwrap_or(0.0);
        let hunting = s.hunting;
        let body = at + Vec3::Y * (crate::npc::NPC_CENTER + 0.05);
        commands.queue(move |w: &mut World| {
            if let Some(mut tr) = w.get_mut::<Transform>(h) {
                tr.translation = body;
                tr.rotation = Quat::from_rotation_y(face);
            }
            if let Some(mut n) = w.get_mut::<Npc>(h) {
                n.home = body;
                n.yaw = face;
                // (getting up: `DisDLC07NPCAnim_GravehoundSpawn`, standing for it)
                n.stagger = n.stagger.max(1.2);
                if hunting {
                    n.alert = Alert::Combat;
                    n.awareness = 1.0;
                    n.last_seen = ppos;
                    n.set_mode(Mode::Combat);
                }
            }
            w.entity_mut(h).insert(crate::npc::FinisherClip("Generic_Respawn_In".into(), false));
        });
        fx.write(SpawnEffect::at("hound_respawn", body));
        for (k, _) in LIMBS.iter().enumerate() {
            let a = k as f32 / LIMBS.len() as f32 * std::f32::consts::TAU;
            fx.write(SpawnEffect::at("hound_respawn_limbs", body + Quat::from_rotation_y(face + a) * Vec3::new(0.35, -0.2, 0.0)));
        }
        fx.write(SpawnEffect::at("hound_respawn_spine", body + Vec3::Y * 0.2));
        sfx.write(PostEvent::named("Gravehound_Respawn", Some(body)));
        info!("grave hound {} back on its feet", slot_owner);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_respawn_time_falls_in_its_range() {
        for _ in 0..50 {
            let t = rand_in([3.75, 4.25]);
            assert!((3.75..=4.25).contains(&t));
        }
        assert_eq!(rand_in([2.0, 1.0]), 2.0);
    }
}
