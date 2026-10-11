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
        app.add_message::<KnockOff>().add_systems(Update, (part_hits, knock_off).run_if(in_state(GameState::InGame)));
    }
}

/// A hit on a character that may knock its armour off: what kind, whether to the head, from
/// where.
#[derive(Message, Clone, Copy)]
pub struct KnockOff {
    pub npc: Entity,
    pub kind: HitKind,
    pub head: bool,
    pub from: Vec3,
}

/// Armour knocked off (`DisTweaks_Armor`'s `m_DamageTypesThatKnockOff`: a bullet or a bolt to
/// the head, Wind Blast and a flare anywhere): it falls from the character, popping up
/// (`m_fPingUpSpeed`) and carried a little the blow's way (`m_fKnockoffVelocityPercent`).
fn knock_off(
    mut commands: Commands,
    mut knocks: MessageReader<KnockOff>,
    level: Option<Res<LevelInfo>>,
    attrs: Res<crate::gamedata::Attrs>,
    parts: Query<(Entity, &NpcAttached, &GlobalTransform)>,
    mut npcs: Query<&mut crate::npc::Npc>,
) {
    let Some(level) = level else {
        knocks.clear();
        return;
    };
    for k in knocks.read() {
        let kind = crate::worlddamage::hit_type(k.kind, 0.0, &attrs);
        for (e, part, gt) in &parts {
            if part.npc != k.npc {
                continue;
            }
            let Some(def) = level.scene.npc_types.get(part.npc_type as usize).and_then(|t| t.attachments.get(part.index)) else { continue };
            if !def.knock_off.iter().any(|(t, any)| (*any || k.head) && crate::worlddamage::is_a(kind, t)) {
                continue;
            }
            let at = gt.compute_transform();
            let away = (at.translation - k.from).with_y(0.0).normalize_or_zero();
            if let Ok(mut n) = npcs.get_mut(part.npc) {
                n.bare.push(part.index as u8);
            }
            commands.entity(e).remove::<(ChildOf, NpcAttached)>().insert((
                at,
                bevy_rapier3d::prelude::RigidBody::Dynamic,
                bevy_rapier3d::prelude::Collider::ball(0.12),
                bevy_rapier3d::prelude::CollisionGroups::new(crate::level::GROUP_PROP, crate::level::GROUP_WORLD | crate::level::GROUP_PROP),
                bevy_rapier3d::prelude::Velocity { linear: away * 1.5 + Vec3::Y * 2.0, angular: Vec3::new(3.0, 1.0, 2.0) },
            ));
        }
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
        // (armour isn't worn down: it is knocked off)
        if !def.knock_off.is_empty() {
            continue;
        }
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
