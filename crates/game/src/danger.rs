//! The AI's danger zones as the original keeps them (`DisGlobalAIDangerManager`, decoded for
//! this rewrite from the user's own Dishonored.exe: adding 0x00C46160, the tick 0x00C51850,
//! the push 0x00C566F0, the brains' `DisSteeringInfluence_Danger` 0x00B050F0).
//! - One shape a requestor, a sphere or a capsule: a new one takes its place. Each lasts its
//!   lifetime (-1: for ever).
//! - Who lays them: a watch tower shooting at what it has fully in sight (a sphere about its
//!   middle, `m_fArrowDangerRadius`, 2 s, renewed while it shoots; with `m_ArrowDangerShape` a
//!   capsule from its arrows' socket), a tallboy fighting (a sphere about where it knows its
//!   target to be, `m_fAimingDangerSphereRadius`, 2 s, while its aim is clear), a grenade (a
//!   sphere about it as it is thrown and where it strikes, its reach x `m_fAIDangerRatio`, for
//!   what is left of its fuse), a character's grenade throw (a capsule from it to its target,
//!   `m_fGrenadeDangerZone`, 1 s), and a tallboy's arrow in flight (a capsule along the next
//!   2 s of its path, `TallboyExplosiveArrow_twk`'s 2000 uu).
//! - Each pushes a character within it (not the one that laid it) away from it, `1 - d²/r²`
//!   strong; a character's pushes are summed (at most 1), eased towards at
//!   `m_fInfluenceSmoothSpeed` a second and faded in at `m_fInfluenceFadeSpeed` (none for
//!   those whose brain doesn't tick it: Brisby, Samuel, the weepers, the Outsider; it fades out
//!   while one escapes an explosion). The push bends a character's way as it moves (`npc_move`).

use crate::gameplay::TimeControl;
use crate::npc::{Mode, Npc};
use crate::player::Player;
use crate::GameState;
use bevy::prelude::*;

pub struct DangerPlugin;

impl Plugin for DangerPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<DangerZones>()
            .add_systems(OnEnter(GameState::Loading), |mut z: ResMut<DangerZones>| z.zones.clear())
            .add_systems(Update, (tick, (aims, throws), steer).chain().before(crate::npc::npc_move).run_if(in_state(GameState::InGame)));
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Shape {
    Sphere(Vec3),
    Capsule(Vec3, Vec3),
}

#[derive(Clone, Copy, Debug)]
pub struct Zone {
    pub requestor: Entity,
    pub shape: Shape,
    pub radius: f32,
    /// seconds left; below 0, for ever
    pub life: f32,
}

#[derive(Resource, Default)]
pub struct DangerZones {
    pub zones: Vec<Zone>,
}

impl DangerZones {
    /// A sphere laid by `requestor`, in place of its last shape.
    pub fn sphere(&mut self, requestor: Entity, life: f32, at: Vec3, radius: f32) {
        self.put(Zone { requestor, shape: Shape::Sphere(at), radius, life });
    }

    /// A capsule laid by `requestor`, in place of its last shape.
    pub fn capsule(&mut self, requestor: Entity, life: f32, from: Vec3, to: Vec3, radius: f32) {
        self.put(Zone { requestor, shape: Shape::Capsule(from, to), radius, life });
    }

    fn put(&mut self, z: Zone) {
        if z.radius <= 0.0 {
            return;
        }
        match self.zones.iter_mut().find(|s| s.requestor == z.requestor) {
            Some(s) => *s = z,
            None => self.zones.push(z),
        }
    }

    /// The zones' push on a character at `pos` (`who`: its own zone doesn't push it), facing
    /// `facing`: summed, at most 1.
    pub fn push(&self, who: Entity, pos: Vec3, facing: Vec3) -> Vec3 {
        let sum: Vec3 = self.zones.iter().filter(|z| z.requestor != who).map(|z| push_of(z.shape, z.radius, pos, facing)).sum();
        sum.clamp_length_max(1.0)
    }
}

/// One zone's push on a point: away from its middle (a capsule's nearest point), `1 - d²/r²`
/// strong; at its very middle, along the facing at full strength.
pub fn push_of(shape: Shape, radius: f32, pos: Vec3, facing: Vec3) -> Vec3 {
    let at = match shape {
        Shape::Sphere(c) => c,
        Shape::Capsule(a, b) => {
            let ab = b - a;
            let len2 = ab.length_squared();
            if len2 <= 1e-8 {
                a
            } else {
                a + ab * ((pos - a).dot(ab) / len2).clamp(0.0, 1.0)
            }
        }
    };
    let d = pos - at;
    let (d2, r2) = (d.length_squared(), radius * radius);
    if d2 >= r2 {
        return Vec3::ZERO;
    }
    if d2 <= 1e-12 {
        return facing;
    }
    d / d2.sqrt() * (1.0 - d2 / r2)
}

/// The zones' lifetimes (world time).
fn tick(time: Res<Time>, tc: Res<TimeControl>, mut zones: ResMut<DangerZones>) {
    let dt = time.delta_secs() * tc.world_scale();
    zones.zones.retain_mut(|z| {
        if z.life < 0.0 {
            return true;
        }
        z.life -= dt;
        z.life > 0.0
    });
}

/// How long the aims' and throws' zones last (the tallboy's combat tick 0x00B11790, the throw
/// 0x00B26240).
const AIM_LIFE: f32 = 2.0;
const THROW_LIFE: f32 = 1.0;

/// A fighting tallboy's aim: a sphere about where it knows Corvo to be, while nothing hides him
/// from it.
fn aims(mut zones: ResMut<DangerZones>, npcs: Query<(Entity, &Npc), Without<Player>>, player: Query<&Transform, With<Player>>) {
    let corvo = player.single().map(|t| t.translation).ok();
    for (e, npc) in &npcs {
        let r = npc.arms.danger.aim_radius;
        if r <= 0.0 || npc.is_down() || npc.mode != Mode::Combat || !npc.sees_player {
            continue;
        }
        if let Some(at) = corvo.or(npc.last_seen) {
            zones.sphere(e, AIM_LIFE, at, r);
        }
    }
}

/// A character's grenade throw: a capsule from it to its target.
fn throws(mut zones: ResMut<DangerZones>, mut thrown: MessageReader<crate::gadgets::NpcGrenade>, npcs: Query<&Transform, With<Npc>>) {
    for g in thrown.read() {
        let from = npcs.get(g.owner).map(|t| t.translation).unwrap_or(g.from);
        zones.capsule(g.owner, THROW_LIFE, from, g.target, g.danger);
    }
}

/// Each character's push, eased and faded as its brain steers it (`DH_DANGER_LOG`: the zones
/// and who they push, every second).
fn steer(time: Res<Time>, tc: Res<TimeControl>, zones: Res<DangerZones>, mut npcs: Query<(Entity, &mut Npc, &Transform), Without<Player>>, mut logged: Local<f32>) {
    let log = std::env::var("DH_DANGER_LOG").is_ok() && time.elapsed_secs() - *logged >= 1.0;
    if log {
        *logged = time.elapsed_secs();
        for z in &zones.zones {
            info!("danger: {:?} r {:.1} m, {:.1} s left", z.shape, z.radius, z.life);
        }
    }
    let world = time.delta_secs() * tc.world_scale();
    let own = time.delta_secs() * tc.own_scale();
    for (e, mut npc, t) in &mut npcs {
        let dt = if npc.out_of_bend { own } else { world };
        let tw = npc.arms.danger.clone();
        if !tw.steered || npc.is_down() {
            npc.danger = Vec3::ZERO;
            continue;
        }
        if npc.escape.is_some() || npc.plague_escape.is_some() {
            // (switched off while it escapes: its push fades out)
            let l = (npc.danger.with_y(0.0).length() - tw.fade * dt).max(0.0);
            npc.danger = npc.danger.normalize_or_zero() * l;
            npc.danger_fade = l;
            continue;
        }
        let want = zones.push(e, t.translation, npc.forward());
        npc.danger_fade = (npc.danger_fade + tw.fade * dt).min(1.0);
        let diff = want - npc.danger_smooth;
        let len = diff.length();
        if len > 0.0 {
            let step = (tw.smooth * dt / len).min(1.0);
            npc.danger_smooth += diff * step;
        }
        npc.danger = npc.danger_smooth * npc.danger_fade;
        if log && npc.danger.length() > 0.05 {
            info!("danger: {} pushed {:.2} ({:?})", npc.name, npc.danger.length(), npc.mode);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_zone_pushes_out_of_it_weaker_towards_its_edge_and_not_its_own_requestor() {
        let mut z = DangerZones::default();
        let tower = Entity::from_raw_u32(1).unwrap();
        let guard = Entity::from_raw_u32(2).unwrap();
        // the tower's 30 m sphere about Corvo
        z.sphere(tower, 2.0, Vec3::ZERO, 30.0);
        let near = z.push(guard, Vec3::new(1.5, 0.0, 0.0), Vec3::Z);
        assert!((near - Vec3::X * (1.0 - 1.5f32.powi(2) / 900.0)).length() < 1e-5, "{near}");
        let far = z.push(guard, Vec3::new(0.0, 0.0, 29.0), Vec3::X);
        assert!(far.z > 0.0 && far.z < 0.07, "{far}");
        assert_eq!(z.push(guard, Vec3::new(31.0, 0.0, 0.0), Vec3::Z), Vec3::ZERO);
        // at its middle: along its facing; and the one that laid it isn't pushed
        assert_eq!(z.push(guard, Vec3::ZERO, Vec3::Z), Vec3::Z);
        z.sphere(guard, 2.0, Vec3::new(1.0, 0.0, 0.0), 5.0);
        assert_eq!(z.zones.len(), 2);
        assert!((z.push(guard, Vec3::new(1.5, 0.0, 0.0), Vec3::Z) - near).length() < 1e-5);
        // a new shape of a requestor's takes its last one's place; two pushes at most 1
        z.capsule(guard, 1.0, Vec3::new(0.0, 0.0, -10.0), Vec3::new(0.0, 0.0, 10.0), 5.0);
        assert_eq!(z.zones.len(), 2);
        let both = z.push(tower, Vec3::new(1.0, 0.0, 3.0), Vec3::Z);
        assert!((both - Vec3::X * (1.0 - 1.0 / 25.0)).length() < 1e-5, "{both}");
        z.sphere(Entity::from_raw_u32(3).unwrap(), 1.0, Vec3::new(0.5, 0.0, 3.0), 5.0);
        assert!(z.push(tower, Vec3::new(1.0, 0.0, 3.0), Vec3::Z).length() <= 1.0 + 1e-6);
    }

    #[test]
    fn a_fighter_steps_back_from_danger_ahead_aside_from_danger_at_its_side_and_not_from_behind() {
        use crate::npc::danger_step;
        // facing -Z: a push towards +Z is danger ahead of it
        let facing = Vec3::NEG_Z;
        assert_eq!(danger_step(facing, Vec3::Z * 0.8), Some("BackStep"));
        assert_eq!(danger_step(facing, Vec3::new(0.7, 0.0, 0.2)), Some("SideStep"));
        assert_eq!(danger_step(facing, Vec3::new(-0.7, 0.0, -0.2)), Some("SideStep"));
        assert_eq!(danger_step(facing, Vec3::NEG_Z * 0.8), None);
        assert_eq!(danger_step(facing, Vec3::splat(5e-5)), None);
    }
}
