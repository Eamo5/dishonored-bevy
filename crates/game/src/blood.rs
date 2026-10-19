//! A body on the world: the pool it bleeds lying still (`DisTweaks_NPCPawn_Body
//! .m_BloodDecal`), spreading under one of its bones, and its contacts as it falls and is thrown
//! about (`DishonoredNPCPawn.m_CorpseContactPhysicsInfo`: the contact system's `Corpse`): its
//! thud and the noise the AI hear, and the splatter it leaves where it strikes hard enough. The
//! pool and the splatter are decals (`decals.rs`).

use crate::decals::SpawnDecal;
use crate::level::{LevelInfo, GROUP_WORLD};
use crate::npc::{Ashes, Mode, Npc, NpcRig, ScriptedAnim};
use crate::ragdoll::{Ragdoll, RagdollPart};
use crate::GameState;
use bevy::prelude::*;
use bevy_rapier3d::prelude::*;
use std::collections::HashMap;

pub struct BloodPlugin;

impl Plugin for BloodPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Update, (bleed, contacts).run_if(in_state(GameState::InGame)));
    }
}

/// How far above and below its bone a body's floor is looked for (m).
const FLOOR_ABOVE: f32 = 0.25;
const FLOOR_BELOW: f32 = 1.0;
/// A pool's floor may fall no further than this below its middle all round it (m), else it
/// spreads less: ours lies flat where the original's projects, so it mustn't hang over a ledge
/// (under a wall or a step up it's out of sight).
const FLOOR_DROP: f32 = 0.12;
/// A body's part has struck something when it loses this share of its speed in a step.
const STRUCK_LOSS: f32 = 0.5;
/// A dead body's contacts: the least speed one is felt at (m/s), and how soon another may be
/// (s: `DishonoredNPCPawn.m_CorpseContactPhysicsInfo`'s `m_fMinImpactSpeed` 50 uu/s and
/// `m_fRefireDelay` 0.3).
const CONTACT_MIN_SPEED: f32 = 0.5;
const CONTACT_REFIRE: f32 = 0.3;
/// unreal units to metres
const UU: f32 = 0.01;

/// A body that has bled (its pool spread, or none where it lay).
#[derive(Component)]
pub struct Bled;

/// The dead, once they lie still, bleed a pool on the floor under one of their bones (none in
/// water, nor for those burnt to ash; not on Corvo's shoulder). The level's dead (`Pwn_Dead*`)
/// and a saved game's bodies, lying so from the start, have bled already.
#[allow(clippy::type_complexity)]
fn bleed(
    mut commands: Commands,
    (level, waters, carry): (Option<Res<LevelInfo>>, Option<Res<crate::swim::Waters>>, Res<crate::carry::Carry>),
    bodies: Query<(Entity, &Npc, &NpcRig, &Ragdoll), (Without<Bled>, Without<Ashes>, Without<ScriptedAnim>)>,
    joints: Query<&GlobalTransform>,
    rapier: ReadRapierContext,
    mut decals: MessageWriter<SpawnDecal>,
) {
    let Some(level) = level else { return };
    let Ok(ctx) = rapier.single() else { return };
    for (e, npc, rig, ragdoll) in &bodies {
        if npc.mode != Mode::Dead || npc.corpse || carry.body == Some(e) {
            continue;
        }
        if !ragdoll.lying_still() {
            continue;
        }
        commands.entity(e).try_insert(Bled);
        if ragdoll.saved() {
            continue;
        }
        let Some(bp) = npc.npc_type.and_then(|t| level.scene.npc_types.get(t as usize)).and_then(|t| t.blood_pool.as_ref()) else { continue };
        // under one of its bones, at random
        let under: Vec<Vec3> = bp.bones.iter().filter_map(|b| rig.joint(b)).filter_map(|j| joints.get(j).ok()).map(|g| g.translation()).collect();
        if under.is_empty() {
            continue;
        }
        let at = under[rand::random_range(0..under.len())];
        let floor = QueryFilter::default().groups(CollisionGroups::new(Group::ALL, GROUP_WORLD));
        let Some((_, hit)) = ctx.cast_ray_and_get_normal(at + Vec3::Y * FLOOR_ABOVE, Vec3::NEG_Y, FLOOR_ABOVE + FLOOR_BELOW, true, floor) else { continue };
        if hit.normal.y < 0.7 || waters.as_ref().is_some_and(|w| w.at(hit.point + Vec3::Y * 0.05).is_some()) {
            continue;
        }
        // (no further than the floor goes)
        let mut decal = bp.clone();
        let reach = floor_reach(&ctx, floor, hit.point, Vec2::from(bp.max).max_element() * UU * 0.5);
        if reach * 2.0 < Vec2::from(bp.min).max_element() * UU {
            info!("blood: no floor round {} for a pool at {}", npc.name, hit.point);
            continue;
        }
        decal.max = [decal.max[0].min(reach * 2.0 / UU), decal.max[1].min(reach * 2.0 / UU)];
        decals.write(SpawnDecal { decal, at: hit.point, normal: hit.normal, along: None });
        info!("blood: a pool under {} at {} (its bone at {:.2})", npc.name, hit.point, at.y);
    }
}

/// How far round `at` a pool may spread without hanging over a drop (m, up to `most`): the
/// largest of a few rings whose every point has a wall between it and `at`, or floor no further
/// than `FLOOR_DROP` below `at`.
fn floor_reach(ctx: &RapierContext, filter: QueryFilter, at: Vec3, most: f32) -> f32 {
    let holds = |r: f32| {
        (0..8).all(|k| {
            let a = k as f32 * std::f32::consts::FRAC_PI_4;
            let dir = Vec3::new(a.cos(), 0.0, a.sin());
            ctx.cast_ray(at + Vec3::Y * 0.05, dir, r, true, filter).is_some()
                || ctx.cast_ray(at + dir * r + Vec3::Y * 0.5, Vec3::NEG_Y, 0.5 + FLOOR_DROP, true, filter).is_some()
        })
    };
    [1.0, 0.8, 0.6, 0.45, 0.3, 0.2, 0.1].into_iter().map(|k| k * most).find(|r| holds(*r)).unwrap_or(0.0)
}

/// A dead body's part that stops short against the world: the contact system's `Corpse` on what
/// it struck (water's splash in water), at most one a body every `m_fRefireDelay`: its thud, the noise
/// the AI hear from 200 uu/s, and from 750 uu/s its blood splatter there, along the way it came.
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
fn contacts(
    time: Res<Time>,
    (level, waters): (Option<Res<LevelInfo>>, Option<Res<crate::swim::Waters>>),
    parts: Query<(Entity, &RagdollPart, &Transform, &Velocity)>,
    owners: Query<&Npc>,
    surfaces: Query<&crate::footsteps::ColliderSurfaces>,
    rapier: ReadRapierContext,
    (mut last, mut struck_at): (Local<HashMap<Entity, (Vec3, Vec3)>>, Local<HashMap<Entity, f32>>),
    (mut decals, mut sfx, mut noise): (MessageWriter<SpawnDecal>, MessageWriter<crate::audio::PostEvent>, MessageWriter<crate::gameplay::Noise>),
) {
    let dt = time.delta_secs();
    let (Some(level), Ok(ctx)) = (level, rapier.single()) else {
        last.clear();
        return;
    };
    let clock = time.elapsed_secs();
    struck_at.retain(|_, t| clock - *t < CONTACT_REFIRE);
    let mut now = HashMap::with_capacity(parts.iter().len());
    for (e, part, t, v) in &parts {
        now.insert(e, (t.translation, v.linear));
        let Some(&(from, was)) = last.get(&e) else { continue };
        let speed = was.length();
        if dt <= 0.0 || speed < CONTACT_MIN_SPEED || v.linear.length() > speed * STRUCK_LOSS || struck_at.contains_key(&part.owner) {
            continue;
        }
        // (the knocked out are a `SleepingBody`, which the contact system leaves silent)
        if owners.get(part.owner).is_ok_and(|n| n.mode != Mode::Dead) {
            continue;
        }
        // what it struck, along the way it came
        let dir = was / speed.max(1e-3);
        let world = QueryFilter::default().groups(CollisionGroups::new(Group::ALL, GROUP_WORLD));
        let Some((_, toi, surface, normal)) = crate::footsteps::ray_surface_normal(&ctx, &surfaces, from, dir, speed * dt + 0.4, world) else { continue };
        let at = from + dir * toi;
        // (in water: its splash, no splatter)
        let wet = waters.as_ref().is_some_and(|w| w.at(at).is_some());
        let struck = if wet { "Env_Water" } else { crate::impacts::struck(surface) };
        struck_at.insert(part.owner, clock);
        let impacts = &level.scene.corpse_impacts;
        if let Some(im) = impacts.iter().find(|i| i.against == struck).or_else(|| impacts.iter().find(|i| i.against == "Environment")) {
            if !im.sound.is_empty() {
                sfx.write(crate::audio::PostEvent::named(&im.sound, Some(at)));
            }
            if im.noise > 0.0 && speed >= im.noise_min_speed {
                use crate::gameplay::{Noise, NoiseKind};
                noise.write(Noise { corvo: false, pos: at, radius: im.noise, kind: if im.threatening { NoiseKind::Threatening } else { NoiseKind::Anomaly } });
            }
        }
        if !wet && crate::impacts::mark(&mut decals, &level.scene, "Corpse", struck, at, normal, dir, speed / UU) {
            info!("blood: a body strikes {struck} at {:.1} m/s, at {at}", speed);
        }
    }
    *last = now;
}
