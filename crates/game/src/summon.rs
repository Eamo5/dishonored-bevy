//! Daud's Summon Assassin (The Knife of Dunwall: `DLC06_Pwr_Assassummon`): an assassin of his
//! (the power level's `m_pAssassinTweaks`: a novice, then a master) appears where he aims, or
//! beside the enemy he aims at (`m_fDistFromTarget` before it) and goes for it; a threatening
//! noise where he appears (`m_bMakeNoiseOnSpawn`). As many as the level allows
//! (`m_MaxAssassins`: one, the one before goes); gone once out of a fight for a while
//! (`m_bDisappearAfterCombat`, `m_fNoCombatTimeout`). With Arcane Bond II he moves through
//! bent time as Daud does.

use crate::gameplay::{Noise, NoiseKind, PlayerStats};
use crate::level::{GameAssets, LevelInfo};
use crate::npc::{Mode, Npc, NPC_CENTER};
use crate::world_light::WorldLighting;
use crate::GameState;
use bevy::prelude::*;

pub struct SummonPlugin;

impl Plugin for SummonPlugin {
    fn build(&self, app: &mut App) {
        app.add_message::<Summon>().add_systems(Update, (summon, summoned).chain().run_if(in_state(GameState::InGame)));
    }
}

/// Summon an assassin: where, the enemy aimed at, at what power level.
#[derive(Message, Clone, Copy)]
pub struct Summon {
    pub at: Vec3,
    pub target: Option<Entity>,
    pub level: u8,
}

/// A summoned assassin: the enemy he was sent for (until he has it), how long he has been out
/// of a fight.
#[derive(Component)]
pub struct Summoned {
    foe: Option<Entity>,
    fresh: bool,
    idle: f32,
}

#[allow(clippy::too_many_arguments)]
fn summon(
    mut commands: Commands,
    mut requests: MessageReader<Summon>,
    level: Option<Res<LevelInfo>>,
    assets: Option<Res<GameAssets>>,
    mut wl: Option<ResMut<WorldLighting>>,
    settings: Res<crate::settings::Settings>,
    existing: Query<(Entity, &Transform), With<Summoned>>,
    player: Query<&Transform, (With<crate::player::Player>, Without<Summoned>)>,
    mut fx: MessageWriter<crate::particles::SpawnEffect>,
    mut sfx: MessageWriter<crate::audio::PostEvent>,
    mut noise: MessageWriter<Noise>,
) {
    let (Some(level), Some(assets)) = (level, assets) else {
        requests.clear();
        return;
    };
    for r in requests.read() {
        let scene = &level.scene;
        let Some(&si) = scene.summons.get(r.level.clamp(1, 2) as usize - 1).or(scene.summons.first()) else {
            warn!("summon: no assassin on this map");
            continue;
        };
        // (one at a time: the one before goes)
        for (e, t) in &existing {
            sfx.write(crate::audio::PostEvent::named("AI_Assassin_Pwr_Teleport_Start", Some(t.translation)));
            commands.entity(e).try_despawn();
        }
        let Some(e) = crate::npc::spawn_npc_as(&mut commands, &assets, scene, wl.as_deref_mut(), si as usize, settings.difficulty, None) else { continue };
        // placed, facing Daud's way
        let from = player.single().map(|p| p.translation).unwrap_or(r.at);
        let to = (r.at - from).with_y(0.0);
        let yaw = (-to.x).atan2(-to.z);
        commands.entity(e).insert((
            Transform::from_translation(r.at + Vec3::Y * (NPC_CENTER + 0.05)).with_rotation(Quat::from_rotation_y(yaw)),
            Summoned { foe: r.target, fresh: true, idle: 0.0 },
        ));
        fx.write(crate::particles::SpawnEffect::at("summon_trigger", r.at));
        noise.write(Noise { pos: r.at, radius: 4.0, kind: NoiseKind::Threatening });
        if std::env::var_os("DH_SUMMON_LOG").is_some() {
            info!("summon: assassin (level {}) at {:.1}{}", r.level, r.at, if r.target.is_some() { ", for an enemy" } else { "" });
        }
    }
}

/// The summoned go for their enemy, keep Daud's pace through bent time with Arcane Bond II,
/// and go once out of a fight long enough.
fn summoned(
    mut commands: Commands,
    time: Res<Time>,
    data: Res<crate::gamedata::Data>,
    stats: Res<PlayerStats>,
    mut q: Query<(Entity, &mut Npc, &mut Summoned, &Transform)>,
    mut sfx: MessageWriter<crate::audio::PostEvent>,
    mut fx: MessageWriter<crate::particles::SpawnEffect>,
) {
    let timeout = data.active("SummonAssassin").and_then(|a| a.params.get("m_fNoCombatTimeout")).copied().unwrap_or(5.0);
    let bond = stats.power("ArcaneBond");
    for (e, mut npc, mut s, t) in &mut q {
        if s.fresh {
            s.fresh = false;
            npc.foe = s.foe.take();
            npc.mode = Mode::Idle;
        }
        npc.out_of_bend = bond >= 2;
        if npc.is_down() {
            continue;
        }
        let fighting = npc.foe.is_some() || npc.mode == Mode::Combat;
        s.idle = if fighting { 0.0 } else { s.idle + time.delta_secs() };
        if s.idle > timeout {
            sfx.write(crate::audio::PostEvent::named("AI_Assassin_Pwr_Teleport_Start", Some(t.translation)));
            fx.write(crate::particles::SpawnEffect::at("summon_trigger", t.translation - Vec3::Y * NPC_CENTER));
            commands.entity(e).try_despawn();
        }
    }
}
