//! A whale butcher's saw (`DisDLC06Tweaks_WepSaw`: The Knife of Dunwall's saw butchers), as its
//! NPC contexts have it.
//! - Out and running while he fights (`m_SawEquippedIdleAnim` `Saw_On`), the saw sputters
//!   (`m_pSawEquippedPS`, `Butcher_Saw_Melee`) and drones (`AI_Butcher_Saw_Idle_Loop`).
//! - Running, it cuts whoever is before its blade (`DisDLC06Tweaks_NPCSawCut`, "Continuous
//!   Blade"): within `m_fSawDamageRadius` (2 m) of its `BladeEdge`, in a wedge
//!   `m_fSawDamageOriginWidth` (0.85 m) wide at the edge opening at `m_fSawDamageAngleDeg` (40
//!   degrees) along his facing, `m_SawDamagePerSecond` (5, 10, 20, 40 by difficulty) dealt at most
//!   `m_MaxSawDamageApplicationsPerSecond` (20) times a second. Not while he is down, held,
//!   choked, possessed, falling, staggered or spraying (`m_DamageDisabledStates`). A block doesn't
//!   stop it, and it cuts Corvo alone (our readings).
//! - Its spray of shrapnel is one of his moves (`npc`: `MoveKind::SawFire`); while it sprays
//!   it lays its combat behaviour's danger along its line (`m_fSawDangerCapsuleLength`, 7.5 m,
//!   `_Radius`, 5 m), as a fighter's line of fire does.

use crate::gameplay::{PlayerHit, PlayerStats, TimeControl};
use crate::npc::{Alert, Mode, MoveKind, Npc};
use crate::player::Player;
use crate::GameState;
use bevy::prelude::*;

pub struct SawPlugin;

impl Plugin for SawPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Update, (crate::npc::fly_shrapnel, saw_cut.after(crate::npc::npc_brain)).run_if(in_state(GameState::InGame)));
    }
}

/// The saw in a butcher's hand: its mesh (for its blade), its edge in the mesh's frame, the cut
/// owed and the time since the last, and what shows it running.
#[derive(Component)]
pub struct SawBlade {
    saw: Entity,
    edge: Vec3,
    owed: f32,
    since: f32,
    /// what carries its running effect and drone (gone, they stop)
    running: Option<Entity>,
    /// time since its cut was last heard
    heard: f32,
}

impl SawBlade {
    pub fn new(saw: Entity, edge: Vec3) -> Self {
        SawBlade { saw, edge, owed: 0.0, since: 0.0, running: None, heard: 9.0 }
    }
}

/// Whether a point is in the saw's cut: within `radius` of the edge, in the wedge opening from
/// it along `facing` (`width` at the edge, widening at `angle` degrees), and not far above or
/// below it.
pub fn in_cut(edge: Vec3, facing: Vec3, at: Vec3, saw: &dhcook::format::NpcSaw) -> bool {
    let rel = at - edge;
    if rel.length() > saw.radius || rel.y.abs() > 1.2 {
        return false;
    }
    let f = facing.with_y(0.0).normalize_or_zero();
    let flat = rel.with_y(0.0);
    let along = flat.dot(f);
    // (pressed against him, behind the edge but before his body, still in it)
    if along < -0.6 {
        return false;
    }
    let side = (flat - f * along).length();
    side <= saw.width * 0.5 + along.max(0.0) * (saw.angle * 0.5).to_radians().tan()
}

/// The saw running while its butcher fights, cutting Corvo before its blade.
#[allow(clippy::type_complexity, clippy::too_many_arguments)]
fn saw_cut(
    mut commands: Commands,
    time: Res<Time>,
    tc: Res<TimeControl>,
    settings: Res<crate::settings::Settings>,
    mut saws: Query<(Entity, &mut SawBlade, &Npc, &Transform)>,
    parts: Query<&GlobalTransform>,
    player: Query<&Transform, (With<Player>, Without<Npc>)>,
    (stats, possession): (Res<PlayerStats>, Res<crate::possession::Possession>),
    (mut hits, mut sfx, mut fx, mut zones): (MessageWriter<PlayerHit>, MessageWriter<crate::audio::PostEvent>, MessageWriter<crate::particles::SpawnEffect>, ResMut<crate::danger::DangerZones>),
) {
    let dt = time.delta_secs() * tc.world_scale();
    if dt <= 0.0 {
        return;
    }
    let pt = player.single().ok().map(|t| t.translation);
    for (e, mut blade, npc, t) in &mut saws {
        let Some(saw) = npc.arms.saw.as_ref() else { continue };
        let spraying = npc.attack_t.is_some() && npc.swing.as_ref().is_some_and(|s| s.kind == MoveKind::SawFire);
        let out = npc.mode == Mode::Combat && npc.alert == Alert::Combat && !npc.is_down();
        let running = out && npc.grounded && npc.stagger <= 0.0 && !spraying && possession.host != Some(e);
        // (its running look and drone, carried by its blade)
        match (out, blade.running) {
            (true, None) => {
                let holder = commands.spawn((Transform::from_translation(blade.edge * 0.5), Visibility::default(), ChildOf(blade.saw))).id();
                fx.write(crate::particles::SpawnEffect { follow: Some(holder), secs: 3600.0, ..crate::particles::SpawnEffect::at("saw_running", Vec3::ZERO) });
                sfx.write(crate::audio::PostEvent::named("AI_Butcher_Saw_Idle_Loop", Some(t.translation)).following(holder));
                blade.running = Some(holder);
            }
            (false, Some(h)) => {
                commands.entity(h).try_despawn();
                blade.running = None;
            }
            _ => {}
        }
        // (spraying: the danger along its line, for the others to keep out of)
        if spraying && saw.danger[0] > 0.0 {
            let from = t.translation + Vec3::Y * 0.3;
            zones.capsule(e, 0.25, from, from + npc.forward() * saw.danger[0], saw.danger[1]);
        }
        blade.since += dt;
        blade.heard += dt;
        let (Some(pt), Ok(g)) = (pt, parts.get(blade.saw)) else { continue };
        let edge = g.transform_point(blade.edge);
        let cutting = running && !stats.dead && possession.host.is_none() && in_cut(edge, npc.forward(), pt + Vec3::Y * 0.3, saw);
        if !cutting {
            blade.owed = 0.0;
            continue;
        }
        blade.owed += saw.cut_dps[(settings.difficulty as usize).min(3)] * dt;
        // (in whole points at least, as often as it may)
        if blade.owed >= 2.0 && blade.since >= 1.0 / saw.rate.max(1.0) {
            hits.write(PlayerHit { npc: e, from: t.translation, damage: blade.owed, kick: false, big: false, push: false, unblockable: true, missile: true });
            blade.owed = 0.0;
            blade.since = 0.0;
            if blade.heard > 1.0 {
                blade.heard = 0.0;
                sfx.write(crate::audio::PostEvent::named("AI_Butcher_Saw_Cut_Meat", Some(edge)));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The cut's wedge: before the blade within its reach, not beside or behind the butcher.
    #[test]
    fn the_cut_is_a_wedge_before_the_blade() {
        let saw = dhcook::format::NpcSaw { cut_dps: [5.0, 10.0, 20.0, 40.0], radius: 2.0, angle: 40.0, width: 0.85, rate: 20.0, ..Default::default() };
        let edge = Vec3::new(0.0, 1.0, -0.9);
        let f = Vec3::NEG_Z;
        assert!(in_cut(edge, f, Vec3::new(0.0, 1.0, -2.5), &saw));
        assert!(in_cut(edge, f, Vec3::new(0.3, 1.0, -0.9), &saw));
        // (beyond its reach, well aside, behind him, high above)
        assert!(!in_cut(edge, f, Vec3::new(0.0, 1.0, -3.1), &saw));
        assert!(!in_cut(edge, f, Vec3::new(1.2, 1.0, -1.2), &saw));
        assert!(!in_cut(edge, f, Vec3::new(0.0, 1.0, 0.5), &saw));
        assert!(!in_cut(edge, f, Vec3::new(0.0, 2.5, -1.5), &saw));
    }
}
