//! What characters carry on their sockets (`m_pAttachmentsTweaks`): a tallboy's whale oil tanks
//! and shields. The breakable ones wear down under blows (`m_Health`), break at once to some
//! (`m_pInstantBreakDamageTypes`: a bullet, a bolt in a tank), shrug others off
//! (`m_pImmuneToDamageTypes`); a tank breaking bursts (its break step's blast), killing the
//! tallboy that carried it.

use crate::audio::PostEvent;
use crate::gameplay::{HitKind, Noise, NpcHit, Struck};
use crate::level::{GameAssets, LevelInfo};
use crate::GameState;
use bevy::prelude::*;

pub struct NpcPartsPlugin;

impl Plugin for NpcPartsPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Update, part_hits.run_if(in_state(GameState::InGame)));
    }
}

/// A breakable part on a character: whose, which of its type's attachments, its health.
#[derive(Component)]
pub struct NpcAttached {
    pub npc: Entity,
    pub npc_type: u32,
    pub index: usize,
    pub health: f32,
}

#[allow(clippy::too_many_arguments)]
fn part_hits(
    mut commands: Commands,
    mut struck: MessageReader<Struck>,
    mut blasts: MessageReader<crate::gadgets::Explosion>,
    level: Option<Res<LevelInfo>>,
    assets: Option<Res<GameAssets>>,
    attrs: Res<crate::gamedata::Attrs>,
    mut parts: Query<(Entity, &mut NpcAttached, &GlobalTransform)>,
    (mut sfx, mut noise, mut fx, mut hits): (MessageWriter<PostEvent>, MessageWriter<Noise>, MessageWriter<crate::particles::SpawnEffect>, MessageWriter<NpcHit>),
    mut tank_blasts: ResMut<crate::props::TankBlasts>,
) {
    let (Some(level), Some(assets)) = (level, assets) else {
        struck.clear();
        blasts.clear();
        return;
    };
    // the blows: struck directly, or caught in a blast
    let mut blows: Vec<(Entity, f32, &'static str)> = struck
        .read()
        .filter(|s| parts.contains(s.target))
        .map(|s| (s.target, s.damage, crate::worlddamage::hit_type(s.kind, 0.0, &attrs)))
        .collect();
    for b in blasts.read() {
        for (e, _, gt) in &parts {
            let d = gt.translation().distance(b.at);
            if d < b.radius {
                let fade = if d <= b.full { 1.0 } else { 1.0 - (d - b.full) / (b.radius - b.full).max(0.01) };
                blows.push((e, b.damage * fade, "DisDamageType_Grenade"));
            }
        }
    }
    for (e, damage, kind) in blows {
        let Ok((_, mut part, gt)) = parts.get_mut(e) else { continue };
        let Some(def) = level.scene.npc_types.get(part.npc_type as usize).and_then(|t| t.attachments.get(part.index)) else { continue };
        if def.immune.iter().any(|t| crate::worlddamage::is_a(kind, t)) {
            continue;
        }
        let was = part.health;
        if def.instant.iter().any(|t| crate::worlddamage::is_a(kind, t)) {
            part.health = 0.0;
        } else {
            part.health -= damage;
        }
        if was <= 0.0 || part.health > 0.0 {
            continue;
        }
        // broken: its pieces, its sound and effect; a tank bursts and its carrier dies with it
        let at = gt.translation();
        if std::env::var("DH_PROP_LOG").is_ok() {
            info!("npc part {} breaks at {at:.2} ({kind})", def.prop);
        }
        if let Some(b) = &def.breaks {
            crate::props::break_pieces(&mut commands, &assets, b, &Transform::from_translation(at), &mut sfx, &mut noise, &mut fx);
            if let Some(bl) = &b.blast {
                tank_blasts.0.push((at, bl.clone()));
                hits.write(NpcHit { npc: part.npc, damage: 999.0, kind: HitKind::Explosion, from: at });
            }
        }
        commands.entity(e).try_despawn();
    }
}
