//! Whale oil devices the AI no longer trusts, as the original keeps them
//! (`AIKnowledgeRecord_WhaleOilDevice`, decoded for this rewrite from the user's own
//! Dishonored.exe: the witness 0x00B43A30 / 0x00B16AD0, the zone 0x00C4CB60, the push's filter
//! 0x00C516D0, the melee precondition 0x00C2EC30, the walls' path cost 0x00A30D90).
//! - A character who sees an ally killed by a wall of light or an arc pylon Corvo rewired
//!   (fighting or searching; otherwise only had it caught him already, which a fight is) keeps
//!   a record of that device, untrusted, for as long as it lives, and finds its way again.
//! - While anyone keeps one and the device runs, the device lays a zone: its meshes' box grown
//!   on every side by half its half size's length. It pushes only those who distrust it, as a
//!   sphere about its middle.
//! - One who distrusts it doesn't strike at Corvo inside that box or across it (the first
//!   such record it keeps), and its paths won't enter the wall (`security::walls_barring`).

use crate::danger::DangerZones;
use crate::level::{LevelInfo, GROUP_WORLD};
use crate::npc::{Alert, Mode, Npc};
use crate::player::Player;
use crate::security::Devices;
use crate::GameState;
use bevy::prelude::*;
use bevy_rapier3d::prelude::*;

pub struct DistrustPlugin;

impl Plugin for DistrustPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Update, (witness, zones).chain().before(crate::danger::steer).run_if(in_state(GameState::InGame)));
    }
}

/// A character killed by a rewired wall of light or arc pylon (its index in `Devices`).
#[derive(Component)]
pub struct DeviceKill(pub usize);

/// How far a witness sees an ally die (m), as a death is seen (`npc_perception`).
const WITNESS_RANGE: f32 = 14.0;

/// Those who see an ally fall to a device turned against them stop trusting it.
fn witness(
    killed: Query<(Entity, &Npc, &Transform, &DeviceKill), Added<DeviceKill>>,
    mut npcs: Query<(Entity, &mut Npc, &Transform), (Without<Player>, Without<DeviceKill>)>,
    level: Option<Res<LevelInfo>>,
    rapier: ReadRapierContext,
) {
    if killed.is_empty() {
        return;
    }
    let ctx = rapier.single().ok();
    let allies = level.as_ref().map(|l| &l.scene.allies);
    for (_, victim, vt, kill) in &killed {
        for (_, mut npc, t) in &mut npcs {
            if npc.is_down() || npc.distrust.contains(&kill.0) {
                continue;
            }
            // (its relation toward the victim: an ally, as its faction stands)
            let ally = !npc.faction.is_empty()
                && (npc.faction == victim.faction || allies.and_then(|a| a.get(&npc.faction)).is_some_and(|a| a.iter().any(|f| *f == victim.faction)));
            // (it sees that death for what it is fighting or searching, or with Corvo caught)
            let heeds = matches!(npc.mode, Mode::Combat | Mode::Search) || npc.alert == Alert::Combat;
            let eye = t.translation + Vec3::Y * 0.75;
            let at = vt.translation + Vec3::Y * 0.2;
            let d = eye.distance(at);
            if std::env::var("DH_DISTRUST_LOG").is_ok() {
                info!("distrust: {} ({}) {d:.1} m from {} ({}): ally {ally}, heeds {heeds} ({:?}, {:?})", npc.name, npc.faction, victim.name, victim.faction, npc.mode, npc.alert);
            }
            if !ally || !heeds || d > WITNESS_RANGE {
                continue;
            }
            let filter = QueryFilter::default().groups(CollisionGroups::new(Group::ALL, GROUP_WORLD));
            if ctx.as_ref().is_some_and(|c| c.cast_ray(eye, (at - eye) / d.max(1e-3), (d - 0.3).max(0.0), true, filter).is_some()) {
                continue;
            }
            npc.distrust.push(kill.0);
            // (its way found again by what it now shuns)
            npc.nav.goal = None;
            info!("{} saw {} killed by device {}: no longer trusts it", npc.name, victim.name, kill.0);
        }
    }
}

/// The zones of the devices someone distrusts, while they run (life -1, the device the
/// requestor); gone once no one living distrusts it or it stops.
fn zones(mut zones: ResMut<DangerZones>, devices: Res<Devices>, npcs: Query<(&Npc, &Transform)>, time: Res<Time>, mut logged: Local<f32>) {
    if std::env::var("DH_DISTRUST_LOG").is_ok() && time.elapsed_secs() - *logged >= 1.0 {
        *logged = time.elapsed_secs();
        for (n, t) in npcs.iter().filter(|(n, _)| !n.distrust.is_empty() && !n.is_down()) {
            let at = devices.list[n.distrust[0]].distrust_box().map(|(lo, hi, _)| (lo + hi) * 0.5).unwrap_or_default();
            info!("distrust: {} at {:.1} ({:?}), {:.1} m from the device, waiting {}", n.name, t.translation, n.mode, t.translation.distance(at), n.nav.barred);
        }
    }
    let mut held: Vec<usize> = npcs.iter().filter(|(n, _)| !n.is_down()).flat_map(|(n, _)| n.distrust.iter().copied()).collect();
    held.sort_unstable();
    held.dedup();
    zones.zones.retain(|z| z.device.is_none_or(|i| held.contains(&i) && devices.live(i)));
    for i in held {
        if !devices.live(i) {
            continue;
        }
        let (Some(at), Some((lo, hi, r))) = (devices.list[i].requestor(), devices.list[i].distrust_box()) else { continue };
        zones.device(at, i, (lo + hi) * 0.5, r);
    }
}

/// Whether a character may not strike at `to` from `from`: the first device it distrusts that
/// runs has `to` inside its zone's box or the way between crossing it (0x00C2EC30).
pub fn strike_barred(npc: &Npc, devices: &Devices, from: Vec3, to: Vec3) -> bool {
    let Some(&i) = npc.distrust.iter().find(|&&i| devices.live(i)) else { return false };
    let Some((lo, hi, _)) = devices.list[i].distrust_box() else { return false };
    inside(lo, hi, to) || crosses(lo, hi, from, to)
}

fn inside(lo: Vec3, hi: Vec3, p: Vec3) -> bool {
    p.cmpgt(lo).all() && p.cmplt(hi).all()
}

/// A segment against a box (a slab test: entering by its end, leaving after its start).
fn crosses(lo: Vec3, hi: Vec3, a: Vec3, b: Vec3) -> bool {
    let d = b - a;
    let (mut t0, mut t1) = (f32::NEG_INFINITY, f32::INFINITY);
    for k in 0..3 {
        if d[k].abs() < 1e-8 {
            if a[k] < lo[k] || a[k] > hi[k] {
                return false;
            }
            continue;
        }
        let (u, v) = ((lo[k] - a[k]) / d[k], (hi[k] - a[k]) / d[k]);
        t0 = t0.max(u.min(v));
        t1 = t1.min(u.max(v));
    }
    t0 <= t1 && t0 <= 1.0 && t1 >= 0.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_strike_is_barred_into_or_across_the_box_only() {
        let (lo, hi) = (Vec3::new(-1.0, 0.0, -0.2), Vec3::new(1.0, 3.0, 0.2));
        assert!(inside(lo, hi, Vec3::new(0.0, 1.0, 0.0)));
        assert!(!inside(lo, hi, Vec3::new(0.0, 1.0, 0.5)));
        // through it, ending short of it, beside it
        assert!(crosses(lo, hi, Vec3::new(0.0, 1.0, -3.0), Vec3::new(0.0, 1.0, 3.0)));
        assert!(!crosses(lo, hi, Vec3::new(0.0, 1.0, -3.0), Vec3::new(0.0, 1.0, -1.0)));
        assert!(!crosses(lo, hi, Vec3::new(2.0, 1.0, -3.0), Vec3::new(2.0, 1.0, 3.0)));
    }
}
