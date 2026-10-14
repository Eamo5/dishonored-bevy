//! A body's blood on the world: the pool it bleeds lying still (`DisTweaks_NPCPawn_Body
//! .m_BloodDecal`), spreading under one of its bones, and the splatter it leaves where it's
//! thrown against something hard enough (the contact system's `Limb` x `Environment`: the
//! ragdolls' `RGDL_Body` and `RGDL_Limb` are `Limb`). Both are decals (`decals.rs`).

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
        app.add_systems(Update, (bleed, splatter).run_if(in_state(GameState::InGame)));
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
/// A body leaves at most one splatter in this long (s: our reading, so one body flung about
/// doesn't take all ten decals).
const SPLATTER_EVERY: f32 = 1.0;
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

/// A limp body's part that stops short against the world, having come faster than the
/// contact's least speed (750 uu/s), leaves its blood splatter there, along the way it came
/// (none in water; a body one a second).
#[allow(clippy::too_many_arguments)]
fn splatter(
    time: Res<Time>,
    (level, waters): (Option<Res<LevelInfo>>, Option<Res<crate::swim::Waters>>),
    parts: Query<(Entity, &RagdollPart, &Transform, &Velocity)>,
    surfaces: Query<&crate::footsteps::ColliderSurfaces>,
    rapier: ReadRapierContext,
    mut last: Local<HashMap<Entity, (Vec3, Vec3)>>,
    mut splattered: Local<HashMap<Entity, f32>>,
    mut decals: MessageWriter<SpawnDecal>,
) {
    let dt = time.delta_secs();
    let (Some(level), Ok(ctx)) = (level, rapier.single()) else {
        last.clear();
        return;
    };
    let clock = time.elapsed_secs();
    splattered.retain(|_, t| clock - *t < SPLATTER_EVERY);
    let mut now = HashMap::with_capacity(parts.iter().len());
    for (e, part, t, v) in &parts {
        now.insert(e, (t.translation, v.linear));
        let Some(&(from, was)) = last.get(&e) else { continue };
        let speed = was.length();
        if dt <= 0.0 || speed < 1.0 || v.linear.length() > speed * STRUCK_LOSS || splattered.contains_key(&part.owner) {
            continue;
        }
        // what it struck, along the way it came
        let dir = was / speed.max(1e-3);
        let world = QueryFilter::default().groups(CollisionGroups::new(Group::ALL, GROUP_WORLD));
        let Some((_, toi, surface, normal)) = crate::footsteps::ray_surface_normal(&ctx, &surfaces, from, dir, speed * dt + 0.4, world) else { continue };
        let at = from + dir * toi;
        if waters.as_ref().is_some_and(|w| w.at(at).is_some()) {
            continue;
        }
        let struck = crate::impacts::struck(surface);
        if crate::impacts::mark(&mut decals, &level.scene, "Limb", struck, at, normal, dir, speed / UU) {
            splattered.insert(part.owner, clock);
            info!("blood: a body strikes {struck} at {:.1} m/s, at {at}", speed);
        }
    }
    *last = now;
}
