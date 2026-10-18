//! Tallboys' arrows (`NPCFireBow_1`, as roshan's r614/blink research decoded the bow): a
//! loosed arrow is a projectile, not a hit roll. One meant to hit is aimed at the ground under
//! Corvo, leading him, on the arc that reaches it (2000 uu/s, gravity halved); one meant to miss
//! turns aside, up to `m_fCheatAngleDeviation` (30 degrees). It bursts where it first strikes
//! (`TallboyArrowExplosion_Twk`: 40 within 1 m, nothing at 5 m), the world, a character or
//! Corvo; nothing flies past its life.

use crate::gameplay::{HitKind, PlayerStats};
use crate::level::{GameAssets, GROUP_PROP, GROUP_WORLD};
use crate::npc::Npc;
use crate::player::Player;
use crate::GameState;
use bevy::prelude::*;
use bevy_rapier3d::prelude::*;

pub struct TallboyPlugin;

impl Plugin for TallboyPlugin {
    fn build(&self, app: &mut App) {
        app.add_message::<TallboyShot>().add_systems(Update, (loose, fly).chain().run_if(in_state(GameState::InGame)));
    }
}

/// A tallboy looses an arrow at Corvo (meant to hit him, or to miss).
#[derive(Message, Clone, Copy)]
pub struct TallboyShot {
    pub from: Vec3,
    pub at: Vec3,
    pub hit: bool,
    pub source: Entity,
}

#[derive(Component)]
pub(crate) struct Arrow {
    vel: Vec3,
    life: f32,
    source: Entity,
}

impl Arrow {
    /// Turned along a Wind Blast: back the way the blast goes, its bow's tallboy no longer
    /// spared.
    pub(crate) fn blown(&mut self, dir: Vec3) {
        self.vel = dir * self.vel.length().max(SPEED);
        self.source = Entity::PLACEHOLDER;
    }
}

/// The arrow's speed (m/s: 2000 uu/s) and how much of gravity takes it.
const SPEED: f32 = 20.0;
/// The danger zone an arrow lays along its way (`TallboyExplosiveArrow_twk`:
/// `m_fDangerCapsuleRadius` 2000 uu, `m_fDangerCapsuleProjectionTime` and `_LifeTime` 2 s).
const DANGER_RADIUS: f32 = 20.0;
const DANGER_AHEAD: f32 = 2.0;
const GRAVITY: f32 = 9.81 * 0.5;
/// The most a miss turns aside (`m_fCheatAngleDeviation`).
const MISS: f32 = 30.0;
/// The burst (`TallboyArrowExplosion_Twk`): damage, full within, nothing past (m).
const BLAST: (f32, f32, f32) = (40.0, 1.0, 5.0);
/// How far under Corvo it looks for the ground to aim at (3 m: 300 uu).
const GROUND: f32 = 3.0;

/// The launch direction from `from` that brings an arrow at `SPEED` down on `to` (the low arc),
/// else straight at it raised to 45 degrees.
pub fn aim(from: Vec3, to: Vec3) -> Vec3 {
    let d = to - from;
    let flat = d.with_y(0.0);
    let x = flat.length().max(0.01);
    let y = d.y;
    let v2 = SPEED * SPEED;
    let disc = v2 * v2 - GRAVITY * (GRAVITY * x * x + 2.0 * y * v2);
    let pitch = if disc >= 0.0 { ((v2 - disc.sqrt()) / (GRAVITY * x)).atan() } else { std::f32::consts::FRAC_PI_4 };
    let h = flat / x;
    (h * pitch.cos() + Vec3::Y * pitch.sin()).normalize_or(Vec3::NEG_Z)
}

fn loose(
    mut commands: Commands,
    mut shots: MessageReader<TallboyShot>,
    rapier: ReadRapierContext,
    player: Query<&Player>,
    assets: Option<Res<GameAssets>>,
) {
    let Ok(ctx) = rapier.single() else { return };
    let ground = QueryFilter::default().exclude_sensors().groups(CollisionGroups::new(Group::ALL, GROUP_WORLD | GROUP_PROP));
    let lead = player.single().map(|p| p.velocity.with_y(0.0)).unwrap_or(Vec3::ZERO);
    for s in shots.read() {
        // (its sound is the loosing clip's cue, `Empty_Bow_Fire`: `npc.rs`)
        let dir = if s.hit {
            // the ground under him, where he will be as it comes down
            let floor = ctx.cast_ray(s.at, Vec3::NEG_Y, GROUND, true, ground).map(|(_, t)| s.at - Vec3::Y * t).unwrap_or(s.at - Vec3::Y * 0.9);
            let flight = (floor - s.from).with_y(0.0).length() / SPEED;
            aim(s.from, floor + lead * flight)
        } else {
            let a = (rand::random::<f32>() * 2.0 - 1.0) * MISS.to_radians();
            Quat::from_rotation_y(a) * aim(s.from, s.at)
        };
        let mut e = commands.spawn((Arrow { vel: dir * SPEED, life: 6.0, source: s.source }, Transform::from_translation(s.from).looking_to(dir, Vec3::Y), Visibility::default(), DespawnOnExit(GameState::InGame)));
        // (the bolt's model stands in for its arrow)
        crate::powers::projectile_model(&mut e, assets.as_deref(), HitKind::Bolt);
    }
}

#[allow(clippy::type_complexity)]
fn fly(
    mut commands: Commands,
    time: Res<Time>,
    tc: Res<crate::gameplay::TimeControl>,
    rapier: ReadRapierContext,
    mut arrows: Query<(Entity, &mut Arrow, &mut Transform)>,
    player: Query<(&Transform, &Player), Without<Arrow>>,
    npcs: Query<(Entity, &Npc, &Transform), Without<Arrow>>,
    stats: Res<PlayerStats>,
    mut blasts: MessageWriter<crate::gadgets::Explosion>,
    mut zones: ResMut<crate::danger::DangerZones>,
) {
    let dt = time.delta_secs() * tc.world_scale();
    let Ok(ctx) = rapier.single() else { return };
    let walls = QueryFilter::default().exclude_sensors().groups(CollisionGroups::new(Group::ALL, GROUP_WORLD | GROUP_PROP));
    let pp = player.single().ok().map(|(t, p)| (t.translation, p.crouched));
    for (e, mut a, mut t) in &mut arrows {
        a.life -= dt;
        if a.life <= 0.0 {
            commands.entity(e).despawn();
            continue;
        }
        // in flight, the danger zone along its next while (`danger`: the arrow's tick 0x00C54F00,
        // `TallboyExplosiveArrow_twk`'s capsule 2000 uu across 2 s of its path, lasting 2 s)
        if dt > 0.0 {
            let ahead = t.translation + a.vel * DANGER_AHEAD - Vec3::Y * (0.5 * GRAVITY * DANGER_AHEAD * DANGER_AHEAD);
            zones.capsule(e, DANGER_AHEAD, t.translation, ahead, DANGER_RADIUS);
        }
        a.vel.y -= GRAVITY * dt;
        let step = a.vel * dt;
        let len = step.length();
        if len <= 0.0 {
            continue;
        }
        let from = t.translation;
        let dir = step / len;
        let wall = ctx.cast_ray_and_get_normal(from, dir, len, true, walls);
        let mut nearest = wall.as_ref().map(|(_, h)| h.time_of_impact).unwrap_or(len);
        let mut hit = wall.map(|(_, h)| from + dir * h.time_of_impact + h.normal * 0.1);
        for (ne, n, nt) in &npcs {
            if ne == a.source || n.is_down() {
                continue;
            }
            if let Some(d) = crate::krust::capsule_ray_hit(from, dir, nearest, nt.translation, crate::npc::NPC_HALF, crate::npc::NPC_RADIUS + 0.05) {
                nearest = d;
                hit = Some(from + dir * d);
            }
        }
        if let Some((p, crouched)) = pp.filter(|_| !stats.dead) {
            let half = if crouched { crate::player::CROUCH_HALF } else { crate::player::STAND_HALF };
            if let Some(d) = crate::krust::capsule_ray_hit(from, dir, nearest, p, half, crate::player::RADIUS + 0.05) {
                hit = Some(from + dir * d);
            }
        }
        match hit {
            Some(at) => {
                info!("tallboy arrow bursts at {at:.2}{}", pp.map(|(p, _)| format!(", {:.1} m from Corvo", p.distance(at))).unwrap_or_default());
                blasts.write(crate::gadgets::Explosion { at: at - dir * 0.1, radius: BLAST.2, full: BLAST.1, damage: BLAST.0, effect: "grenade", player: Some([BLAST.2, BLAST.1]), kind: HitKind::EnemyExplosion, silent: false });
                commands.entity(e).despawn();
            }
            None => {
                t.translation = from + step;
                t.look_to(a.vel.normalize_or(Vec3::NEG_Z), Vec3::Y);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_arc_comes_down_on_its_target() {
        let from = Vec3::new(0.0, 3.0, 0.0);
        let to = Vec3::new(12.0, 0.0, 5.0);
        let dir = aim(from, to);
        // fly it in small steps and see where it crosses the target's height
        let (mut p, mut v) = (from, dir * SPEED);
        for _ in 0..4000 {
            v.y -= GRAVITY * 0.001;
            p += v * 0.001;
            if p.y <= to.y && v.y < 0.0 {
                break;
            }
        }
        assert!(p.with_y(0.0).distance(to.with_y(0.0)) < 0.3, "{p:?}");
    }
}
