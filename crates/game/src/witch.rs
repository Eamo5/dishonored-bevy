//! Delilah's witches (The Brigmore Witches: `DLC07_Pwn_Witch_*`), fighting with their hand's
//! spells as its contexts have them (`DisDLC06Tweaks_WepWitchHand`, cooked as `NpcWitch`), beside
//! their blades:
//! - the scream (`DisDLC06Tweaks_NPCWitchScream`, `Sword_Attack_Scream`): Daud close before
//!   her, it strikes along her line at the clip's `DisDLC07Notify_WitchScreamDamage`, a strong
//!   blow (`DisDLC06DamageType_WitchScream`) that can't be blocked;
//! - her magic (`DisTweaks_NPCFireBow`, `Sword_Power_RangedAttack`): four bolts
//!   (`DLC07_Proj_WitchMagic`, invisible but for their trail) loosed at the clip's four
//!   `DishonoredNotify_FireProjectile`s, flying at the context's speed and falling a little
//!   (`m_fGravityMultiplier`);
//! - her vanishing (`DisDLC06Tweaks_NPCWitchTeleport`): gone in her effect for
//!   `m_ReappearanceDelay`, back where Daud is in sight, as far from him as the context says,
//!   within its angle of where she was;
//! - a tentacle called up by him (`DisDLC07Tweaks_NPCSummonTentacle`,
//!   `Sword_Power_SummonTentacle`: `tentacle`).

use crate::audio::PostEvent;
use crate::level::GROUP_WORLD;
use crate::npc::{Mode, MoveKind, Npc, Swing};
use crate::particles::SpawnEffect;
use crate::player::Player;
use crate::GameState;
use bevy::prelude::*;
use bevy_rapier3d::prelude::*;

pub struct WitchPlugin;

impl Plugin for WitchPlugin {
    fn build(&self, app: &mut App) {
        app.add_message::<WitchSpell>()
            .add_systems(Update, (mark_witches, witch_powers, witch_spells, fly_magic).chain().in_set(WitchSet).before(crate::player::PlayerMoveSet).run_if(in_state(GameState::InGame)));
    }
}

/// The witches' systems (the tentacles they call up come after).
#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub struct WitchSet;

/// A witch's spell at its clip's cue (from `npc.rs`'s blows).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Spell {
    Scream,
    Magic,
    Summon,
}

#[derive(Message, Clone, Copy)]
pub struct WitchSpell {
    pub witch: Entity,
    pub spell: Spell,
    pub from: Vec3,
    pub facing: Vec3,
}

#[derive(Component)]
pub struct Witch {
    scream_cd: f32,
    magic_cd: f32,
    teleport_cd: f32,
    summon_cd: f32,
    /// vanished: seconds before she comes back, and where she was
    gone: Option<(f32, Vec3)>,
}

/// A bolt of a witch's magic in flight.
#[derive(Component)]
struct Magic {
    velocity: Vec3,
    drop: f32,
    damage: f32,
    owner: Entity,
    life: f32,
}

fn rand_in(r: [f32; 2]) -> f32 {
    r[0] + (r[1] - r[0]).max(0.0) * rand::random::<f32>()
}

fn mark_witches(mut commands: Commands, npcs: Query<(Entity, &Npc), Added<Npc>>) {
    for (e, n) in &npcs {
        if n.arms.witch.is_some() {
            commands.entity(e).try_insert(Witch { scream_cd: 0.0, magic_cd: rand_in([1.0, 3.0]), teleport_cd: rand_in([3.0, 8.0]), summon_cd: rand_in([2.0, 6.0]), gone: None });
        }
    }
}

/// Is she free to begin a spell (fighting, not mid-blow, not reeling)?
fn free(npc: &Npc) -> bool {
    npc.mode == Mode::Combat && !npc.is_down() && npc.stagger <= 0.0 && npc.attack_t.is_none() && npc.swing.is_none() && !npc.casting && npc.grab.is_none()
}

fn start(npc: &mut Npc, kind: MoveKind, to: Vec3) {
    npc.attack_t = Some(0.0);
    npc.swing = Some(Swing { kind, big: false, len: 1.2, hit: 0.5, reach: 40.0, lunge: 0.0, no_versus: true, more: Vec::new() });
    npc.target = None;
    let flat = to.with_y(0.0);
    if flat.length() > 0.1 {
        npc.yaw = (-flat.x).atan2(-flat.z);
    }
}

#[allow(clippy::too_many_arguments, clippy::type_complexity)]
fn witch_powers(
    mut commands: Commands,
    time: Res<Time>,
    tc: Res<crate::gameplay::TimeControl>,
    rapier: ReadRapierContext,
    mut witches: Query<(Entity, &mut Witch, &mut Npc, &mut Transform, &mut Visibility)>,
    player: Query<(&Transform, &Player), Without<Npc>>,
    (stats, possession): (Res<crate::gameplay::PlayerStats>, Res<crate::possession::Possession>),
    (mut sfx, mut fx): (MessageWriter<PostEvent>, MessageWriter<SpawnEffect>),
    tentacles: Query<&crate::tentacle::Tentacle>,
    level: Option<Res<crate::level::LevelInfo>>,
) {
    let Ok((pt, _)) = player.single() else { return };
    let ctx = rapier.single().ok();
    let walls = QueryFilter::default().exclude_sensors().groups(CollisionGroups::new(Group::ALL, GROUP_WORLD));
    let ppos = pt.translation;
    let eye = ppos + Vec3::Y * 0.6;
    let clear = |from: Vec3, to: Vec3| {
        let d = to - from;
        let l = d.length();
        l < 0.01 || ctx.as_ref().is_some_and(|c| c.cast_ray(from, d / l, l - 0.2, true, walls).is_none())
    };
    let log = std::env::var("DH_WITCH_LOG").is_ok();
    for (e, mut w, mut npc, mut t, mut vis) in &mut witches {
        let Some(sp) = npc.arms.witch.clone() else { continue };
        let dt = time.delta_secs() * if npc.out_of_bend { tc.own_scale() } else { tc.world_scale() };
        w.scream_cd -= dt;
        w.magic_cd -= dt;
        w.teleport_cd -= dt;
        w.summon_cd -= dt;
        // vanished: back once her delay is over, where Daud is in sight
        if let Some((left, from)) = w.gone {
            npc.casting = true;
            npc.attack_t = None;
            npc.swing = None;
            if left > dt && !npc.is_down() {
                w.gone = Some((left - dt, from));
                continue;
            }
            // (as the context has it first; then, as the original tries again with its
            // `m_fReattempt*` steps, nearer him and from any side)
            let back = (from - ppos).with_y(0.0).normalize_or(Vec3::X);
            let mut landed = None;
            for k in 0..32 {
                let (angle, near) = if k < 16 { (sp.teleport_player_angle, sp.teleport_from_player) } else { (180.0, [3.0, sp.teleport_from_player[1]]) };
                let a = (rand::random::<f32>() - 0.5) * 2.0 * angle.to_radians();
                let at = ppos + Quat::from_rotation_y(a) * back * rand_in(near);
                let Some((_, down)) = ctx.as_ref().and_then(|c| c.cast_ray(at + Vec3::Y * 2.0, Vec3::NEG_Y, 6.0, true, walls)) else { continue };
                let feet = at + Vec3::Y * (2.0 - down);
                let reach = feet.distance(from);
                // (room to stand: nothing within her reach about her body)
                let room = ctx.as_ref().is_none_or(|c| {
                    let mut free = true;
                    let body = Collider::capsule_y(0.6, 0.3);
                    c.intersect_shape(feet + Vec3::Y * crate::npc::NPC_CENTER, Quat::IDENTITY, body.raw.as_ref(), walls, |_| {
                        free = false;
                        false
                    });
                    free
                });
                if (feet.y - (ppos.y - 0.9)).abs() > 4.0 || reach > sp.teleport_reach[1] || !room || !clear(feet + Vec3::Y * 1.4, eye) {
                    continue;
                }
                landed = Some(feet + Vec3::Y * crate::npc::NPC_CENTER);
                break;
            }
            let at = landed.unwrap_or(from);
            if log {
                info!("witch {} comes back {:.1} m from Daud{}", npc.name, at.distance(ppos), if landed.is_none() { " (where she was)" } else { "" });
            }
            t.translation = at;
            npc.home = at;
            npc.velocity = Vec3::ZERO;
            let flat = (ppos - at).with_y(0.0);
            npc.yaw = (-flat.x).atan2(-flat.z);
            t.rotation = Quat::from_rotation_y(npc.yaw);
            *vis = Visibility::Inherited;
            commands.entity(e).remove::<ColliderDisabled>();
            npc.casting = false;
            w.gone = None;
            fx.write(SpawnEffect::at("witch_appear", at));
            sfx.write(PostEvent::named("DLC07_Witch_Teleport_Out", Some(at)));
            continue;
        }
        if !free(&npc) || stats.dead || possession.host.is_some() {
            continue;
        }
        let me = t.translation;
        let to = ppos - me;
        let d = to.length();
        let flat = to.with_y(0.0).normalize_or_zero();
        let ahead = flat.dot(npc.forward());
        let sees = npc.sees_player && clear(me + Vec3::Y * 0.7, eye);
        // the scream: Daud close before her (one turn in two as her blade's moves have theirs)
        if sp.scream_ctx_range > 0.0 && w.scream_cd <= 0.0 && d < sp.scream_ctx_range && to.y.abs() < sp.scream_height + 0.9 && ahead >= sp.scream_angle.to_radians().cos() {
            w.scream_cd = rand_in(sp.scream_cooldown) + 1.6;
            if rand::random::<f32>() < 0.5 {
                start(&mut npc, MoveKind::Scream, to);
                if log {
                    info!("witch {} screams at {d:.1} m", npc.name);
                }
                continue;
            }
        }
        // vanishing: at him from beyond her ideal reach, out of his sight, or (a turn in four)
        // to get away from his blade
        let ready = sp.teleport_ctx_range > 0.0 && w.teleport_cd <= 0.0 && d < sp.teleport_ctx_range;
        if ready && !(d > 15.0 || !sees || rand::random::<f32>() < 0.25) {
            w.teleport_cd = rand_in([2.0, 4.0]);
        } else if ready {
            w.teleport_cd = rand_in(sp.teleport_cooldown);
            w.gone = Some((rand_in(sp.teleport_delay), me));
            npc.casting = true;
            *vis = Visibility::Hidden;
            commands.entity(e).insert(ColliderDisabled);
            fx.write(SpawnEffect::at("witch_vanish", me));
            sfx.write(PostEvent::named("DLC07_Witch_Teleport_In", Some(me)));
            if log {
                info!("witch {} vanishes {d:.1} m from Daud", npc.name);
            }
            continue;
        }
        // a tentacle called up by him (one of hers at a time)
        let hers = tentacles.iter().filter(|x| x.summoner == Some(e)).count();
        let room = || level.as_ref().is_some_and(|l| crate::tentacle::spot(&l.scene, &crate::tentacle::taken(&tentacles), ppos, me, sp.summon_near_target).is_some());
        if sp.summon_ctx_range > 0.0 && w.summon_cd <= 0.0 && hers == 0 && sees && d < sp.summon_ctx_range && d > 2.5 && ahead >= sp.summon_angle.to_radians().cos() && room() {
            w.summon_cd = rand_in(sp.summon_cooldown);
            start(&mut npc, MoveKind::Summon, to);
            if log {
                info!("witch {} calls up a tentacle at {d:.1} m", npc.name);
            }
            continue;
        }
        // her magic: from afar, in sight, before her
        let fire = crate::npc::fire_context(&npc);
        let [near, far, _] = fire.range;
        if w.magic_cd <= 0.0 && sees && d > near && d < far && ahead >= fire.angle.to_radians().cos() {
            let unreachable = to.y.abs() > 1.6 || d > 6.0;
            let cd = if unreachable { fire.cooldown_unreachable } else { fire.cooldown };
            w.magic_cd = rand_in(cd) + 1.7;
            start(&mut npc, MoveKind::Magic, to);
            if log {
                info!("witch {} casts her magic at {d:.1} m", npc.name);
            }
        }
    }
}

/// The spells at their cues: the scream striking, a bolt loosed, a tentacle called up.
#[allow(clippy::too_many_arguments)]
fn witch_spells(
    mut commands: Commands,
    mut spells: MessageReader<WitchSpell>,
    npcs: Query<&Npc>,
    player: Query<&Transform, With<Player>>,
    settings: Res<crate::settings::Settings>,
    (mut hits, mut fx, mut summon): (MessageWriter<crate::gameplay::PlayerHit>, MessageWriter<SpawnEffect>, MessageWriter<crate::tentacle::SummonTentacle>),
) {
    let Ok(pt) = player.single() else {
        spells.clear();
        return;
    };
    let d = (settings.difficulty as usize).min(3);
    for s in spells.read() {
        let Ok(npc) = npcs.get(s.witch) else { continue };
        let Some(sp) = npc.arms.witch.as_ref() else { continue };
        match s.spell {
            Spell::Scream => {
                // along her line from her mouth, as far as its range, within its radius of it
                // and his body about it
                let mouth = s.from + Vec3::Y * 0.75 + s.facing * 0.25;
                fx.write(SpawnEffect { rot: Transform::IDENTITY.looking_to(s.facing, Vec3::Y).rotation, ..SpawnEffect::at("witch_scream", mouth) });
                let rel = pt.translation - mouth;
                let along = rel.dot(s.facing);
                let off = (rel - s.facing * along).with_y(0.0).length();
                if along > -0.3 && along < sp.scream_range + crate::player::RADIUS && off < sp.scream_radius + crate::player::RADIUS + 0.2 && rel.y.abs() < 1.6 {
                    hits.write(crate::gameplay::PlayerHit { npc: s.witch, from: s.from, damage: sp.scream_damage[d], kick: false, big: true, push: false, unblockable: true, missile: true });
                }
            }
            Spell::Magic => {
                // from her left hand, aimed to fall onto his chest
                let from = s.from + Vec3::Y * 0.55 + s.facing * 0.45;
                let to = pt.translation + Vec3::Y * 0.2;
                let speed = sp.magic_speed.max(1.0);
                let drop = 9.81 * sp.magic_gravity;
                let dist = (to - from).length();
                let flight = dist / speed;
                let mut v = (to - from).normalize_or(s.facing) * speed;
                v.y += 0.5 * drop * flight;
                let e = commands.spawn((Magic { velocity: v, drop, damage: npc.arms.ranged, owner: s.witch, life: 6.0 }, Transform::from_translation(from), DespawnOnExit(GameState::InGame))).id();
                fx.write(SpawnEffect { follow: Some(e), secs: 6.0, ..SpawnEffect::at("witch_magic", Vec3::ZERO) });
            }
            Spell::Summon => {
                fx.write(SpawnEffect::at("witch_summon", s.from + Vec3::Y * 0.6 + s.facing * 0.4));
                summon.write(crate::tentacle::SummonTentacle { witch: s.witch, witch_at: s.from, near: pt.translation, reach: sp.summon_near_target });
            }
        }
    }
}

/// The bolts fly, fall a little, and strike the world or Daud.
#[allow(clippy::too_many_arguments)]
fn fly_magic(
    mut commands: Commands,
    time: Res<Time>,
    tc: Res<crate::gameplay::TimeControl>,
    rapier: ReadRapierContext,
    mut bolts: Query<(Entity, &mut Magic, &mut Transform)>,
    player: Query<&Transform, (With<Player>, Without<Magic>)>,
    (mut hits, mut fx, mut sfx): (MessageWriter<crate::gameplay::PlayerHit>, MessageWriter<SpawnEffect>, MessageWriter<PostEvent>),
) {
    let dt = time.delta_secs() * tc.world_scale();
    if dt <= 0.0 {
        return;
    }
    let ctx = rapier.single().ok();
    let walls = QueryFilter::default().exclude_sensors().groups(CollisionGroups::new(Group::ALL, GROUP_WORLD | crate::level::GROUP_PROP));
    let ppos = player.single().ok().map(|t| t.translation);
    for (e, mut m, mut t) in &mut bolts {
        m.life -= dt;
        let a = t.translation;
        m.velocity.y -= m.drop * dt;
        let b = a + m.velocity * dt;
        let seg = b - a;
        let len = seg.length().max(1e-4);
        // Daud: the step passing within his body
        if let Some(p) = ppos {
            let k = ((p - a).dot(seg) / (len * len)).clamp(0.0, 1.0);
            let near = a + seg * k;
            let off = near - p;
            if off.with_y(0.0).length() < crate::player::RADIUS + 0.1 && off.y.abs() < 0.95 {
                hits.write(crate::gameplay::PlayerHit { npc: m.owner, from: a, damage: m.damage, kick: false, big: false, push: false, unblockable: true, missile: true });
                fx.write(SpawnEffect::at("witch_magic_hit", near));
                sfx.write(PostEvent::named("DLC07_Witch_Bolt_Impact", Some(near)));
                commands.entity(e).despawn();
                continue;
            }
        }
        if let Some((_, toi)) = ctx.as_ref().and_then(|c| c.cast_ray(a, seg / len, len, true, walls)) {
            fx.write(SpawnEffect::at("witch_magic_hit", a + seg / len * toi));
            sfx.write(PostEvent::named("DLC07_Witch_Bolt_Impact", Some(a + seg / len * toi)));
            commands.entity(e).despawn();
            continue;
        }
        t.translation = b;
        if m.life <= 0.0 {
            commands.entity(e).despawn();
        }
    }
}
