//! The search's crumbs, as the original lays them (`DishonoredSearchCrumbsComponent` on Corvo,
//! decoded for this rewrite from the user's own Dishonored.exe: the reset 0x00ABFB60, the tick
//! 0x00ACEAE0, the wander 0x00AC3C60, the drop 0x00AC8AC0, `GetNextCrumb` 0x00AC8ED0).
//! - One proxy for where Corvo might be, shared by every searcher. Every noise he makes and
//!   every tick of a fight with him sets it on him, held there a second
//!   (`m_fStickyProxyDuration`); seeing him doesn't.
//! - Every 0.8 s (`m_fHeartbeatInterval`) it wanders on from there as he might have gone: the
//!   way it last went, turned up to 60 degrees (`m_fWanderAngularRange`), 5-10 m a step
//!   (`m_fMinWanderStep`, `m_fMaxWanderStep`), stopped by the walls. After a long step, once in
//!   four (`m_fChanceOfPsychicSearch`) it heads straight at him; after short ones it turns off
//!   the wall it met. Three steps that go nowhere and it starts again from him.
//! - Ahead of it, crumbs: 25 (`m_iNumSearchCrumbs`), one where none lies within 8 m
//!   (`m_fDistanceBetweenCrumbs`) or the last is out of its sight, each lasting 150 s
//!   (`m_fCrumbLifetime`).
//! - A searcher goes to a crumb taken at random within its tether, which no other searcher then
//!   takes (`npc.rs`, `Mode::Search`).

use crate::gameplay::Noise;
use crate::level::GROUP_WORLD;
use crate::navmesh::NavGrid;
use crate::npc::{Alert, Mode, Npc};
use crate::player::{Player, RADIUS, STAND_HALF};
use crate::GameState;
use bevy::prelude::*;
use bevy_rapier3d::prelude::*;
use std::f32::consts::TAU;

pub struct CrumbsPlugin;

impl Plugin for CrumbsPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Crumbs>()
            .add_systems(OnEnter(GameState::InGame), |mut c: ResMut<Crumbs>| *c = Crumbs::default())
            .add_systems(Update, (reset, lay).chain().before(crate::npc::npc_brain).run_if(in_state(GameState::InGame)));
    }
}

const CRUMBS: usize = 25;
const LIFETIME: f32 = 150.0;
const SPACING: f32 = 8.0;
const BEAT: f32 = 0.8;
const WANDER: f32 = 60.0;
const PSYCHIC: f32 = 0.25;
const STEP: (f32, f32) = (5.0, 10.0);
const STICKY: f32 = 1.0;
/// a step this long on some axis is a long one (500 uu)
const LONG_STEP: f32 = 5.0;
/// Corvo's centre above his feet: the proxy and the crumbs are kept at his height
pub const HEIGHT: f32 = STAND_HALF + RADIUS;

/// Corvo's search proxy and the crumbs it lays (centres, at his height above the floor).
#[derive(Resource)]
pub struct Crumbs {
    proxy: Option<Vec3>,
    sticky: f32,
    beat: f32,
    /// its last step, and the wall that stopped it
    moved: Vec3,
    wall: Option<Vec3>,
    /// steps that went nowhere
    bad: u32,
    /// where each lies and the seconds it has left (none left: gone, or taken)
    crumbs: [(Vec3, f32); CRUMBS],
    last: Option<usize>,
}

impl Default for Crumbs {
    fn default() -> Self {
        Crumbs { proxy: None, sticky: 0.0, beat: BEAT, moved: Vec3::ZERO, wall: None, bad: 0, crumbs: [(Vec3::ZERO, 0.0); CRUMBS], last: None }
    }
}

impl Crumbs {
    /// A searcher's next point: from a random place in the list, the first crumb still lying
    /// that `within` allows (its tether), taken so no one else goes to it.
    pub fn next(&mut self, within: impl Fn(Vec3) -> bool) -> Option<Vec3> {
        let start = rand::random_range(0..CRUMBS);
        for k in 0..CRUMBS {
            let i = (start + k) % CRUMBS;
            let (at, life) = self.crumbs[i];
            if life > 0.0 && within(at) {
                self.crumbs[i].1 = -1.0;
                return Some(at);
            }
        }
        None
    }
}

/// A random turn of up to `WANDER` degrees either way.
fn turn(v: Vec3) -> Vec3 {
    Quat::from_rotation_y((rand::random::<f32>() * 2.0 - 1.0) * WANDER.to_radians()) * v
}

fn any_way() -> Vec3 {
    Quat::from_rotation_y(rand::random::<f32>() * TAU) * Vec3::NEG_Z
}

fn flat(v: Vec3) -> Vec3 {
    v.with_y(0.0).normalize_or_zero()
}

/// The proxy set on Corvo: by any noise he makes (0x00C5A7E4), and every tick of a fight with
/// him (0x00AE8780). Not while he is inside a host.
fn reset(
    mut noises: MessageReader<Noise>,
    npcs: Query<&Npc>,
    player: Query<&Transform, With<Player>>,
    possession: Res<crate::possession::Possession>,
    mut crumbs: ResMut<Crumbs>,
) {
    let heard = noises.read().filter(|n| n.corvo && n.radius > 0.0).count() > 0;
    let fought = npcs.iter().any(|n| n.mode == Mode::Combat && n.alert == Alert::Combat && n.foe.is_none() && !n.is_down());
    if (heard || fought) && possession.host.is_none() {
        if let Ok(t) = player.single() {
            crumbs.proxy = Some(t.translation);
            crumbs.sticky = STICKY;
        }
    }
}

/// The heartbeat: the proxy held on Corvo or wandering on, and a crumb laid ahead of it.
fn lay(
    time: Res<Time>,
    mut crumbs: ResMut<Crumbs>,
    player: Query<(Entity, &Transform), With<Player>>,
    possession: Res<crate::possession::Possession>,
    grid: Option<Res<NavGrid>>,
    rapier: ReadRapierContext,
) {
    let dt = time.delta_secs();
    let c = &mut *crumbs;
    c.sticky -= dt;
    for k in &mut c.crumbs {
        k.1 -= dt;
    }
    c.beat -= dt;
    if c.beat > 0.0 {
        return;
    }
    c.beat += BEAT;
    let (Ok((pe, pt)), Some(grid), Ok(ctx)) = (player.single(), grid, rapier.single()) else { return };
    let corvo = pt.translation;
    let possessing = possession.host.is_some();
    let proxy = *c.proxy.get_or_insert(corvo);
    let feet = |p: Vec3| p - Vec3::Y * HEIGHT;
    let world = QueryFilter::default().exclude_collider(pe).exclude_sensors().groups(CollisionGroups::new(Group::ALL, GROUP_WORLD));
    let base;
    if (c.sticky > 0.0 || c.bad > 3) && !possessing {
        // (held on him)
        base = corvo;
        c.proxy = Some(corvo);
        c.moved = Vec3::ZERO;
        c.wall = None;
        c.bad = 0;
    } else {
        // (off the mesh, it starts again from him)
        base = if grid.on_mesh(feet(proxy)) { proxy } else { corvo };
        let dir = if c.moved.abs().max_element() >= LONG_STEP {
            if !possessing && rand::random::<f32>() < PSYCHIC { flat(corvo - base) } else { turn(flat(c.moved)) }
        } else {
            if c.moved.length() < 0.01 {
                c.bad += 1;
            }
            match c.wall {
                Some(n) => turn(flat(flat(c.moved) + n * 1.5)),
                None => any_way(),
            }
        };
        let dir = if dir.length_squared() < 1e-6 { any_way() } else { dir };
        let len = STEP.0 + rand::random::<f32>() * (STEP.1 - STEP.0);
        // (his shape swept along it, stopped short of a wall; then down to the floor)
        let shape = Collider::capsule_y(STAND_HALF, RADIUS);
        let opts = ShapeCastOptions { max_time_of_impact: 1.0, target_distance: 0.01, stop_at_penetration: false, ..default() };
        let (end, wall) = match ctx.cast_shape(base, Quat::IDENTITY, dir * len, shape.raw.as_ref(), opts, world) {
            Some((_, hit)) => (base + dir * len * hit.time_of_impact.clamp(0.0, 1.0), Some(hit.details.and_then(|d| d.normal1.with_y(0.0).try_normalize()).unwrap_or(-dir))),
            None => (base + dir * len, None),
        };
        let at = match ctx.cast_ray(end, Vec3::NEG_Y, 10.0, true, world) {
            Some((_, toi)) => end - Vec3::Y * toi + Vec3::Y * HEIGHT,
            None => end,
        };
        c.moved = at - base;
        c.wall = wall;
        c.proxy = Some(at);
    }
    // a crumb ahead of it, where none lies near or the last is out of its sight
    let p = c.proxy.unwrap_or(corvo);
    if !grid.on_mesh(feet(p)) {
        return;
    }
    let near = c.crumbs.iter().any(|k| k.1 > 0.0 && k.0.distance(p) < SPACING);
    let hidden = c.last.map(|i| c.crumbs[i]).filter(|k| k.1 > 0.0).is_some_and(|k| {
        let d = k.0.distance(p);
        d > 0.01 && ctx.cast_ray(k.0, (p - k.0) / d, d, true, world).is_some()
    });
    if near && !hidden {
        return;
    }
    let ahead = flat(c.moved);
    let dir = if ahead.length_squared() < 1e-6 { any_way() } else { turn(ahead) };
    let len = STEP.0 + rand::random::<f32>() * (STEP.1 - STEP.0);
    let reach = ctx.cast_ray(base, dir, len, true, world).map_or(len, |(_, toi)| (toi - RADIUS).max(0.0));
    let Some(f) = grid.snap(feet(base + dir * reach), 5.0) else { return };
    let slot = c.crumbs.iter().position(|k| k.1 <= 0.0).unwrap_or_else(|| (0..CRUMBS).min_by(|a, b| c.crumbs[*a].1.total_cmp(&c.crumbs[*b].1)).unwrap_or(0));
    c.crumbs[slot] = (f + Vec3::Y * HEIGHT, LIFETIME);
    c.last = Some(slot);
    if std::env::var_os("DH_SEARCH_LOG").is_some() {
        info!("crumb {slot} at {:.1} (proxy {p:.1}, Corvo {corvo:.1})", f + Vec3::Y * HEIGHT);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_searcher_takes_a_crumb_no_one_else_gets() {
        let mut c = Crumbs::default();
        c.crumbs[3] = (Vec3::new(1.0, 0.0, 0.0), 10.0);
        c.crumbs[7] = (Vec3::new(2.0, 0.0, 0.0), 10.0);
        c.crumbs[9] = (Vec3::new(50.0, 0.0, 0.0), 10.0);
        // (its tether keeps it within 10 m)
        let within = |p: Vec3| p.x < 10.0;
        let a = c.next(within).unwrap();
        let b = c.next(within).unwrap();
        assert_ne!(a, b);
        assert!(c.next(within).is_none());
        // (the one beyond its tether is still there for another)
        assert_eq!(c.next(|_| true), Some(Vec3::new(50.0, 0.0, 0.0)));
    }
}
