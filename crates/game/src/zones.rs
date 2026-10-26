//! Physics volumes (`PhysicsVolume`): the pain some cause (Boyle's spiked fences: `bPainCausing`,
//! `DamagePerSec` each `PainInterval`, and on entry with `bEntryPain`) and the push of their
//! `ZoneVelocity` (all of it while falling, a little while walking). The volume Corvo is in is the
//! highest `Priority` one around his centre. The rules are the original's movement and the
//! engine's `PhysicsVolume` script, as roshan's r614/blink research decoded them.

use crate::gameplay::{PlayerStats, TimeControl};
use crate::level::LevelInfo;
use crate::player::Player;
use crate::GameState;
use bevy::prelude::*;
use bevy_rapier3d::prelude::Collider;

pub struct ZonesPlugin;

impl Plugin for ZonesPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Zones>().init_resource::<ZonePush>().add_systems(Update, (build_zones, zone_effects).chain().run_if(in_state(GameState::InGame)));
    }
}

/// The push of the volume Corvo is in (m/s), for his movement.
#[derive(Resource, Default)]
pub struct ZonePush(pub Vec3);

struct Zone {
    hulls: Vec<Collider>,
    min: Vec3,
    max: Vec3,
    pain: Option<dhcook::format::VolumePain>,
    push: Vec3,
    priority: i32,
    /// the scripts' actor for it (its collision changed by them), and whether it starts off
    actor: Option<u32>,
    off: bool,
}

#[derive(Resource, Default)]
struct Zones {
    level: String,
    list: Vec<Zone>,
    /// the pain's clock (from the level's start) and the volume he was in
    clock: f32,
    was_in: Option<usize>,
}

fn build_zones(level: Option<Res<LevelInfo>>, mut zones: ResMut<Zones>) {
    let Some(level) = level else { return };
    if zones.level == level.scene.name {
        return;
    }
    zones.level = level.scene.name.clone();
    zones.clock = 0.0;
    zones.was_in = None;
    zones.list = level
        .scene
        .volumes
        .iter()
        .enumerate()
        .filter(|(_, v)| v.pain.is_some() || v.push.is_some())
        .map(|(vi, v)| {
            let hulls: Vec<Collider> = v.hulls.iter().filter_map(|h| Collider::convex_hull(&h.iter().map(|p| Vec3::from(*p)).collect::<Vec<_>>())).collect();
            let (min, max) = v.hulls.iter().flatten().fold((Vec3::MAX, Vec3::MIN), |(lo, hi), p| (lo.min(Vec3::from(*p)), hi.max(Vec3::from(*p))));
            let actor = level.scene.kismet.actors.iter().position(|a| a.volume == Some(vi as u32)).map(|a| a as u32);
            Zone { hulls, min, max, pain: v.pain.clone(), push: v.push.map(Vec3::from).unwrap_or(Vec3::ZERO), priority: v.priority, actor, off: v.off }
        })
        .collect();
    if !zones.list.is_empty() {
        info!("zones: {} physics volumes with pain or push", zones.list.len());
    }
}

fn zone_effects(time: Res<Time>, tc: Res<TimeControl>, mut zones: ResMut<Zones>, mut push: ResMut<ZonePush>, mut stats: ResMut<PlayerStats>, player: Query<&Transform, With<Player>>, vm: Option<Res<crate::kismet::Vm>>) {
    let Ok(pt) = player.single() else { return };
    let at = pt.translation;
    // (one colliding with nothing does nothing: as it starts, or as the scripts set it)
    let on = |z: &Zone| match (z.actor, vm.as_ref()) {
        (Some(a), Some(vm)) => vm.collides(a, z.off),
        _ => !z.off,
    };
    let inside = |z: &Zone| on(z) && at.cmpge(z.min).all() && at.cmple(z.max).all() && z.hulls.iter().any(|h| h.contains_point(Vec3::ZERO, Quat::IDENTITY, at));
    let current = zones.list.iter().enumerate().filter(|(_, z)| inside(z)).max_by_key(|(_, z)| z.priority).map(|(i, _)| i);
    push.0 = current.map(|i| zones.list[i].push).unwrap_or(Vec3::ZERO);
    let dt = time.delta_secs() * tc.world_scale();
    let before = zones.clock;
    zones.clock += dt;
    let entered = current.is_some() && current != zones.was_in;
    zones.was_in = current;
    let Some(pain) = current.and_then(|i| zones.list[i].pain.clone()) else { return };
    if stats.dead {
        return;
    }
    let interval = pain.interval.max(0.05);
    let ticked = (zones.clock / interval).floor() > (before / interval).floor();
    if (entered && pain.entry) || ticked {
        let damage = (pain.per_sec * interval).trunc();
        if damage > 0.0 {
            stats.take_damage(damage);
            stats.damage_flash = 1.0;
            stats.hit_from = Some(at);
            if stats.health <= 0.0 {
                stats.dead = true;
            }
        }
    }
}
