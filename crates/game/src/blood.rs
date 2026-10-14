//! Blood pools under the dead (`DisTweaks_NPCPawn_Body.m_BloodDecal`): a body lying still
//! bleeds a pool under one of its bones, spreading from its least size to its most.
//!
//! The decal's ways are as roshan's r614/blink research decoded the original's
//! `DishonoredDecalComponent` and `DisDecalManager`: one of its materials at random, a step of
//! growth each time step up to its most, at most ten decals live (a new one takes the place of
//! the first unseen for 0.2 s, else there's none), and a grown one gone once unseen for its
//! lifetime. Ours lies flat on the floor where the original's projects on what's there.

use crate::level::{GameAssets, LevelInfo, GROUP_WORLD};
use crate::npc::{Ashes, Mode, Npc, NpcRig, ScriptedAnim};
use crate::ragdoll::Ragdoll;
use crate::GameState;
use bevy::asset::RenderAssetUsages;
use bevy::light::NotShadowCaster;
use bevy::mesh::{Indices, PrimitiveTopology};
use bevy::prelude::*;
use bevy_rapier3d::prelude::*;

pub struct BloodPlugin;

impl Plugin for BloodPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Update, (bleed, spread).chain().run_if(in_state(GameState::InGame)));
    }
}

/// The most decals live at once (`DisDecalManager.MaxDecalCount`).
const MAX_DECALS: usize = 10;
/// A decal unseen this long gives way to a new one (s).
const EVICT_UNSEEN: f32 = 0.2;
/// How far above and below its bone a body's floor is looked for (m).
const FLOOR_ABOVE: f32 = 0.25;
const FLOOR_BELOW: f32 = 1.0;
/// unreal units to metres
const UU: f32 = 0.01;

/// A body that has bled (its pool spread, or none where it lay).
#[derive(Component)]
pub struct Bled;

/// A blood pool: its size now and its most (uu), its growth (uu a step, s a step, s into this
/// step), its lifetime once grown (s), how long it's been unseen (s), and when it was made.
#[derive(Component)]
pub struct Pool {
    size: Vec2,
    max: Vec2,
    step: Vec2,
    every: f32,
    t: f32,
    lifetime: f32,
    unseen: f32,
    born: f32,
}

/// The quad a pool lies on: a unit square, flat, facing up.
pub fn quad() -> Mesh {
    Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::default())
        .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, vec![[-0.5f32, 0.0, 0.5], [0.5, 0.0, 0.5], [0.5, 0.0, -0.5], [-0.5, 0.0, -0.5]])
        .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, vec![[0.0f32, 1.0, 0.0]; 4])
        .with_inserted_attribute(Mesh::ATTRIBUTE_UV_0, vec![[0.0f32, 1.0], [1.0, 1.0], [1.0, 0.0], [0.0, 0.0]])
        .with_inserted_attribute(Mesh::ATTRIBUTE_UV_1, vec![[0.0f32, 1.0], [1.0, 1.0], [1.0, 0.0], [0.0, 0.0]])
        .with_inserted_attribute(Mesh::ATTRIBUTE_TANGENT, vec![[1.0f32, 0.0, 0.0, 1.0]; 4])
        .with_inserted_attribute(Mesh::ATTRIBUTE_COLOR, vec![[1.0f32; 4]; 4])
        .with_inserted_indices(Indices::U32(vec![0, 1, 2, 0, 2, 3]))
}

/// The dead, once they lie still, bleed a pool on the floor under one of their bones (none in
/// water, nor for those burnt to ash; not on Corvo's shoulder). The level's dead (`Pwn_Dead*`)
/// and a saved game's bodies, lying so from the start, have bled already.
#[allow(clippy::type_complexity)]
fn bleed(
    mut commands: Commands,
    time: Res<Time>,
    (level, assets, waters, carry): (Option<Res<LevelInfo>>, Option<Res<GameAssets>>, Option<Res<crate::swim::Waters>>, Res<crate::carry::Carry>),
    bodies: Query<(Entity, &Npc, &NpcRig, &Ragdoll), (Without<Bled>, Without<Ashes>, Without<ScriptedAnim>)>,
    joints: Query<&GlobalTransform>,
    pools: Query<(Entity, &Pool)>,
    rapier: ReadRapierContext,
) {
    let (Some(level), Some(assets)) = (level, assets) else { return };
    let Some(quad) = assets.blood_pool_quad.clone() else { return };
    let Ok(ctx) = rapier.single() else { return };
    // the live decals, in the order they were made
    let mut live: Vec<(Entity, f32, f32)> = pools.iter().map(|(e, p)| (e, p.born, p.unseen)).collect();
    live.sort_by(|a, b| a.1.total_cmp(&b.1));
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
        let Some(mat) = bp.materials.get(rand::random_range(0..bp.materials.len())).and_then(|m| assets.blood_pool_materials.get(m)) else { continue };
        // ten live: the first unseen a moment gives way, else there's none
        if live.len() >= MAX_DECALS {
            match live.iter().position(|l| l.2 > EVICT_UNSEEN) {
                Some(i) => commands.entity(live.remove(i).0).try_despawn(),
                None => continue,
            }
        }
        let max = Vec2::from(bp.max);
        let size = if bp.stretch { Vec2::from(bp.min).min(max) } else { max };
        let turn = Quat::from_rotation_arc(Vec3::Y, hit.normal) * Quat::from_rotation_y(rand::random::<f32>() * std::f32::consts::TAU);
        let born = time.elapsed_secs();
        let mut ec = commands.spawn((
            Mesh3d(quad.clone()),
            Transform::from_translation(hit.point + hit.normal * 0.01).with_rotation(turn).with_scale(Vec3::new(size.x * UU, 1.0, size.y * UU)),
            Pool { size, max, step: Vec2::from(bp.step), every: bp.time_step, t: 0.0, lifetime: bp.lifetime, unseen: 0.0, born },
            NotShadowCaster,
            DespawnOnExit(GameState::InGame),
        ));
        mat.apply(&mut ec);
        live.push((ec.id(), born, 0.0));
        info!("blood: a pool under {} at {} (its bone at {:.2})", npc.name, hit.point, at.y);
    }
}

/// Pools spread a step each time step to their most; grown, one unseen for its lifetime goes.
fn spread(mut commands: Commands, time: Res<Time>, mut pools: Query<(Entity, &mut Pool, &mut Transform, &ViewVisibility)>) {
    let dt = time.delta_secs();
    for (e, mut p, mut t, seen) in &mut pools {
        p.unseen = if seen.get() { 0.0 } else { p.unseen + dt };
        if p.size.cmplt(p.max).any() {
            if p.every <= 0.0 || p.step.max_element() <= 0.0 {
                p.size = p.max;
            } else {
                p.t += dt;
                while p.t >= p.every && p.size.cmplt(p.max).any() {
                    p.t -= p.every;
                    p.size = (p.size + p.step).min(p.max);
                }
            }
            t.scale = Vec3::new(p.size.x * UU, 1.0, p.size.y * UU);
        } else if p.lifetime > 0.0 && p.unseen > p.lifetime {
            commands.entity(e).try_despawn();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pool_spreads_a_step_each_time_step_to_its_most() {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .insert_resource(bevy::time::TimeUpdateStrategy::ManualDuration(std::time::Duration::from_millis(100)))
            .add_systems(Update, spread);
        let e = app
            .world_mut()
            .spawn((
                Transform::default(),
                ViewVisibility::default(),
                // (no lifetime: kept, unseen)
                Pool { size: Vec2::splat(20.0), max: Vec2::splat(200.0), step: Vec2::ONE, every: 0.05, t: 0.0, lifetime: 0.0, unseen: 0.0, born: 0.0 },
            ))
            .id();
        // (the first update starts the clock; then half a second: ten steps)
        for _ in 0..6 {
            app.update();
        }
        let p = app.world().get::<Pool>(e).unwrap();
        assert!((p.size.x - 30.0).abs() < 1.01, "{}", p.size.x);
        for _ in 0..100 {
            app.update();
        }
        let p = app.world().get::<Pool>(e).unwrap();
        assert_eq!(p.size, Vec2::splat(200.0));
        assert!((app.world().get::<Transform>(e).unwrap().scale.x - 2.0).abs() < 1e-5);
    }

    #[test]
    fn a_grown_pool_goes_once_unseen_for_its_lifetime() {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .insert_resource(bevy::time::TimeUpdateStrategy::ManualDuration(std::time::Duration::from_millis(200)))
            .add_systems(Update, spread);
        let pool = |lifetime| Pool { size: Vec2::splat(200.0), max: Vec2::splat(200.0), step: Vec2::ONE, every: 0.05, t: 0.0, lifetime, unseen: 0.0, born: 0.0 };
        let e = app.world_mut().spawn((Transform::default(), ViewVisibility::default(), pool(10.0))).id();
        let kept = app.world_mut().spawn((Transform::default(), ViewVisibility::default(), pool(0.0))).id();
        // (9.8 s unseen: kept; 10.4 s: gone)
        for _ in 0..50 {
            app.update();
        }
        assert!(app.world().get_entity(e).is_ok());
        for _ in 0..3 {
            app.update();
        }
        assert!(app.world().get_entity(e).is_err());
        // (no lifetime: kept)
        assert!(app.world().get_entity(kept).is_ok());
    }
}
