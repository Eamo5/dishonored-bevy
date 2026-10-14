//! Gameplay decals (`DisDecalManager`): the pools the dead bleed, bullet holes, the blood a
//! thrown body leaves where it strikes.
//!
//! The decals' ways are as roshan's r614/blink research decoded the original's
//! `DishonoredDecalComponent` and `DisDecalManager`: one of its materials at random, its texture
//! along the strike, a step of growth each time step up to its most, at most ten decals live (a
//! new one takes the place of the first unseen for 0.2 s, else there's none), and a grown one
//! gone once unseen for its lifetime. Ours lies flat on the surface where the original's
//! projects on what's there.

use crate::level::GameAssets;
use crate::GameState;
use bevy::asset::RenderAssetUsages;
use bevy::light::NotShadowCaster;
use bevy::mesh::{Indices, PrimitiveTopology};
use bevy::prelude::*;
use dhcook::format::Decal;

pub struct DecalsPlugin;

impl Plugin for DecalsPlugin {
    fn build(&self, app: &mut App) {
        app.add_message::<SpawnDecal>().add_systems(Update, (place, spread).chain().run_if(in_state(GameState::InGame)));
    }
}

/// The most decals live at once (`DisDecalManager.MaxDecalCount`).
const MAX_DECALS: usize = 10;
/// A decal unseen this long gives way to a new one (s).
const EVICT_UNSEEN: f32 = 0.2;
/// unreal units to metres
const UU: f32 = 0.01;

/// A decal to lay on a surface at a point, its texture along a direction (else turned at
/// random).
#[derive(Message, Clone)]
pub struct SpawnDecal {
    pub decal: Decal,
    pub at: Vec3,
    pub normal: Vec3,
    pub along: Option<Vec3>,
}

/// A live decal: its size now and its most (uu), its growth (uu a step, s a step, s into this
/// step), its lifetime once grown (s), how long it's been unseen (s), and when it was made.
#[derive(Component)]
pub struct Live {
    size: Vec2,
    max: Vec2,
    step: Vec2,
    every: f32,
    t: f32,
    lifetime: f32,
    unseen: f32,
    born: f32,
}

/// The quad a decal lies on: a unit square, flat, facing up, its texture's X along +X.
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

/// A decal's turn: its face out along the normal, its texture's X along the strike over the
/// surface (else at random).
fn turn(normal: Vec3, along: Option<Vec3>) -> Quat {
    let n = normal.normalize_or(Vec3::Y);
    let face = Quat::from_rotation_arc(Vec3::Y, n);
    let x = face * Vec3::X;
    let angle = match along.map(|a| a - n * a.dot(n)).filter(|a| a.length_squared() > 1e-6) {
        Some(a) => {
            let a = a.normalize();
            x.cross(a).dot(n).atan2(x.dot(a))
        }
        None => rand::random::<f32>() * std::f32::consts::TAU,
    };
    Quat::from_axis_angle(n, angle) * face
}

/// New decals laid where they're asked for, while there's room among the ten.
fn place(mut commands: Commands, time: Res<Time>, assets: Option<Res<GameAssets>>, mut asks: MessageReader<SpawnDecal>, decals: Query<(Entity, &Live)>) {
    let Some(assets) = assets else {
        asks.clear();
        return;
    };
    let Some(quad) = assets.decal_quad.clone() else {
        asks.clear();
        return;
    };
    // the live decals, in the order they were made
    let mut live: Vec<(Entity, f32, f32)> = decals.iter().map(|(e, d)| (e, d.born, d.unseen)).collect();
    live.sort_by(|a, b| a.1.total_cmp(&b.1));
    for ask in asks.read() {
        let d = &ask.decal;
        let Some(mat) = (!d.materials.is_empty()).then(|| d.materials[rand::random_range(0..d.materials.len())]).and_then(|m| assets.decal_materials.get(&m)) else { continue };
        // ten live: the first unseen a moment gives way, else there's none
        if live.len() >= MAX_DECALS {
            match live.iter().position(|l| l.2 > EVICT_UNSEEN) {
                Some(i) => commands.entity(live.remove(i).0).try_despawn(),
                None => continue,
            }
        }
        let max = Vec2::from(d.max);
        let size = if d.stretch { Vec2::from(d.min).min(max) } else { max };
        let born = time.elapsed_secs();
        let n = ask.normal.normalize_or(Vec3::Y);
        let mut ec = commands.spawn((
            Mesh3d(quad.clone()),
            Transform::from_translation(ask.at + n * 0.01).with_rotation(turn(n, ask.along)).with_scale(Vec3::new(size.x * UU, 1.0, size.y * UU)),
            Live { size, max, step: Vec2::from(d.step), every: d.time_step, t: 0.0, lifetime: d.lifetime, unseen: 0.0, born },
            NotShadowCaster,
            DespawnOnExit(GameState::InGame),
        ));
        mat.apply(&mut ec);
        live.push((ec.id(), born, 0.0));
        info!("decal: {:.0} uu at {}", max.x, ask.at);
    }
}

/// Decals spread a step each time step to their most; grown, one unseen for its lifetime goes.
fn spread(mut commands: Commands, time: Res<Time>, mut decals: Query<(Entity, &mut Live, &mut Transform, &ViewVisibility)>) {
    let dt = time.delta_secs();
    for (e, mut d, mut t, seen) in &mut decals {
        d.unseen = if seen.get() { 0.0 } else { d.unseen + dt };
        if d.size.cmplt(d.max).any() {
            if d.every <= 0.0 || d.step.max_element() <= 0.0 {
                d.size = d.max;
            } else {
                d.t += dt;
                while d.t >= d.every && d.size.cmplt(d.max).any() {
                    d.t -= d.every;
                    d.size = (d.size + d.step).min(d.max);
                }
            }
            t.scale = Vec3::new(d.size.x * UU, 1.0, d.size.y * UU);
        } else if d.lifetime > 0.0 && d.unseen > d.lifetime {
            commands.entity(e).try_despawn();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_decal_spreads_a_step_each_time_step_to_its_most() {
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
                Live { size: Vec2::splat(20.0), max: Vec2::splat(200.0), step: Vec2::ONE, every: 0.05, t: 0.0, lifetime: 0.0, unseen: 0.0, born: 0.0 },
            ))
            .id();
        // (the first update starts the clock; then half a second: ten steps)
        for _ in 0..6 {
            app.update();
        }
        let d = app.world().get::<Live>(e).unwrap();
        assert!((d.size.x - 30.0).abs() < 1.01, "{}", d.size.x);
        for _ in 0..100 {
            app.update();
        }
        let d = app.world().get::<Live>(e).unwrap();
        assert_eq!(d.size, Vec2::splat(200.0));
        assert!((app.world().get::<Transform>(e).unwrap().scale.x - 2.0).abs() < 1e-5);
    }

    #[test]
    fn a_grown_decal_goes_once_unseen_for_its_lifetime() {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .insert_resource(bevy::time::TimeUpdateStrategy::ManualDuration(std::time::Duration::from_millis(200)))
            .add_systems(Update, spread);
        let live = |lifetime| Live { size: Vec2::splat(200.0), max: Vec2::splat(200.0), step: Vec2::ONE, every: 0.05, t: 0.0, lifetime, unseen: 0.0, born: 0.0 };
        let e = app.world_mut().spawn((Transform::default(), ViewVisibility::default(), live(10.0))).id();
        let kept = app.world_mut().spawn((Transform::default(), ViewVisibility::default(), live(0.0))).id();
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

    #[test]
    fn a_decal_lies_on_its_surface_its_texture_along_the_strike() {
        // a wall facing +Z struck going right and down: the texture's X along the right-down
        // over the wall
        let q = turn(Vec3::Z, Some(Vec3::new(1.0, -1.0, -1.0)));
        assert!((q * Vec3::Y).distance(Vec3::Z) < 1e-5);
        assert!((q * Vec3::X).distance(Vec3::new(1.0, -1.0, 0.0).normalize()) < 1e-5);
        // the floor struck going -X
        let q = turn(Vec3::Y, Some(Vec3::new(-2.0, -1.0, 0.0)));
        assert!((q * Vec3::X).distance(Vec3::NEG_X) < 1e-5);
    }
}
