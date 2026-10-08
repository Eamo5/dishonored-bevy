//! Pickpocketing: what a character carries (its spawner's `m_pStealablePickup`, a coin purse,
//! a key) hangs at its belt and goes where it goes; Corvo can take it ([F]) sneaking up behind
//! someone who hasn't noticed him, or from the body.

use crate::interact::Pickup;
use crate::level::LevelInfo;
use crate::npc::{Alert, FromSpawner, Npc};
use crate::player::Player;
use crate::GameState;
use bevy::prelude::*;

pub struct PickpocketPlugin;

impl Plugin for PickpocketPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Update, (attach_pockets, script_pockets, follow_pockets).chain().run_if(in_state(GameState::InGame)));
    }
}

/// A pickup carried by a character: where it and its meshes were placed.
#[derive(Component)]
pub struct Pocket {
    pub npc: Entity,
    meshes: Vec<(Entity, Transform)>,
    base: Vec3,
    /// its place along the belt (several pockets on one character: a purse and a key)
    slot: u8,
}

impl Pocket {
    /// Something in `npc`'s pocket, its meshes as they stand, at a place along the belt.
    pub fn new(npc: Entity, meshes: Vec<(Entity, Transform)>, base: Vec3, slot: u8) -> Pocket {
        Pocket { npc, meshes, base, slot }
    }

    /// Can Corvo take it now? From the body, or from behind someone unaware.
    pub fn reachable(&self, npcs: &Query<(&Npc, &Transform), Without<Player>>, from: Vec3) -> bool {
        let Ok((n, t)) = npcs.get(self.npc) else { return true };
        if n.is_down() {
            return true;
        }
        let to_player = (from - t.translation).with_y(0.0).normalize_or_zero();
        n.alert == Alert::Unaware && n.forward().dot(to_player) < 0.2
    }
}

fn attach_pockets(
    mut commands: Commands,
    level: Option<Res<LevelInfo>>,
    npcs: Query<(Entity, &FromSpawner), Added<FromSpawner>>,
    pickups: Query<(Entity, &Pickup, &Transform), Without<Pocket>>,
    transforms: Query<&Transform>,
) {
    let Some(level) = level else { return };
    for (e, from) in &npcs {
        let Some(p) = level.scene.spawners.get(from.0 as usize).and_then(|s| s.stealable) else { continue };
        let Some((pe, pk, pt)) = pickups.iter().find(|(_, pk, _)| pk.index == p) else {
            if std::env::var("DH_POCKET_LOG").is_ok() {
                info!("pocket: {} has no pickup {p} ({} pickups)", from.0, pickups.iter().count());
            }
            continue;
        };
        let meshes = pk.entities.iter().filter_map(|m| transforms.get(*m).ok().map(|t| (*m, *t))).collect();
        if std::env::var("DH_POCKET_LOG").is_ok() {
            info!("pocket: {} carries {} ({} meshes)", from.0, pk.label, pk.entities.len());
        }
        commands.entity(pe).try_insert(Pocket { npc: e, meshes, base: pt.translation, slot: 0 });
    }
}

/// `DisSeqAct_AttachPickup`: the scripts hang a pickup on someone (a key on a guard's belt) or
/// take it off where it is.
fn script_pockets(
    mut commands: Commands,
    vm: Option<ResMut<crate::kismet::Vm>>,
    npcs: Query<(Entity, &FromSpawner)>,
    pickups: Query<(Entity, &Pickup, &Transform)>,
    transforms: Query<&Transform>,
    worn: Query<&Pocket>,
) {
    let Some(mut vm) = vm else { return };
    if vm.attach_pickups.is_empty() {
        return;
    }
    let g = vm.g.clone();
    for (targets, items, attach) in std::mem::take(&mut vm.attach_pickups) {
        let npc = targets
            .iter()
            .find_map(|v| if let crate::kismet::Val::Actor(a) = v { g.actors.get(*a as usize)?.spawner } else { None })
            .and_then(|s| npcs.iter().find(|(_, f)| f.0 == s).map(|x| x.0));
        for v in &items {
            let crate::kismet::Val::Actor(a) = v else { continue };
            let Some(pi) = g.actors.get(*a as usize).and_then(|a| a.pickup) else { continue };
            let Some((pe, pk, pt)) = pickups.iter().find(|(_, pk, _)| pk.index == pi) else { continue };
            match (attach, npc) {
                (true, Some(n)) => {
                    let meshes = pk.entities.iter().filter_map(|m| transforms.get(*m).ok().map(|t| (*m, *t))).collect();
                    let slot = worn.iter().filter(|w| w.npc == n).count().min(3) as u8;
                    commands.entity(pe).try_insert(Pocket { npc: n, meshes, base: pt.translation, slot });
                }
                (false, _) => {
                    commands.entity(pe).try_remove::<Pocket>();
                }
                _ => {}
            }
        }
    }
}

/// The purse rides at the belt: the right hip, a little behind.
fn follow_pockets(npcs: Query<&Transform, (With<Npc>, Without<Pocket>)>, mut pockets: Query<(&Pocket, &mut Transform), Without<Npc>>, mut meshes: Query<&mut Transform, (Without<Npc>, Without<Pocket>)>) {
    for (pocket, mut t) in &mut pockets {
        let Ok(nt) = npcs.get(pocket.npc) else { continue };
        // (the next along the belt, around the hip)
        let at = nt.translation + nt.rotation * Vec3::new(0.2 - 0.07 * pocket.slot as f32, -0.05, 0.12 - 0.05 * pocket.slot as f32);
        let delta = at - pocket.base;
        t.translation = at;
        for (m, base) in &pocket.meshes {
            if let Ok(mut mt) = meshes.get_mut(*m) {
                mt.translation = base.translation + delta;
            }
        }
    }
}
