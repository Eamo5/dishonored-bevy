//! Characters seeing Corvo's magic, as the original has them (decoded for this rewrite from the
//! user's own Dishonored.exe: the power broadcaster 0x00BF9FB0, the brain's witness rule
//! 0x00B23E50, `DisBehaviorMagicResponse` 0x00AFA080 / 0x00AF2800).
//! - What is witnessed: Blink (Corvo vanishing as it starts, appearing where it ends), Possession
//!   (vanishing into a host, appearing out of one, told again for `m_fMagicalAppearanceDelay`,
//!   0.8 s), Bend Time stopping the world (vanishing; appearing as it ends, or a spell seen if it
//!   only slowed it), and Wind Blast, the Devouring Swarm and Summon Assassin cast (a spell seen).
//!   Dark Vision isn't seen. Those who see him take it.
//! - Busted on him, one reacts to a spell or to his appearing; his vanishing arms a while
//!   (3 s) in which it reacts if it loses him (he vanished) or finds him (he appeared). Not busted,
//!   his appearing raises its attention (`m_IncreaseForMagic`) and arms the same while.
//! - Its reaction, a moment later (`m_MagicReactionDelay`, 0.2 to 0.3 s): a line
//!   (`DDH_MAGIC_WITNESSED`; gone, `DDH_MAGIC_DISAPPEARED`); gone and not his ally, it turns to
//!   where it saw him and plays its `Magic_OutOfSight` clip (`DisNPCAnim_MagicLostTarget`), the
//!   behaviour's slot above its fight (`EAISlot_Awe`).

use crate::npc::Npc;
use crate::player::Player;
use crate::powers::{Power, PowerUsed};
use crate::GameState;
use bevy::prelude::*;

pub struct MagicPlugin;

impl Plugin for MagicPlugin {
    fn build(&self, app: &mut App) {
        app.add_message::<Magic>().add_systems(Update, (broadcast, witness).chain().run_if(in_state(GameState::InGame)));
    }
}

/// How it is witnessed (`eDisMagicWitnessingMode`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MagicMode {
    Cast,
    Appear,
    Disappear,
}

/// Corvo's magic before whoever sees him.
#[derive(Message, Clone, Copy, Debug)]
pub struct Magic {
    pub mode: MagicMode,
    pub power: Power,
}

/// `DisBehaviorMagicResponse`'s while after his vanishing (3 s), and the reaction's delay
/// (`DishonoredGlobalAIManager.m_MagicReactionDelay`, 0.2 to 0.3 s).
const PENDING: f32 = 3.0;
const DELAY: [f32; 2] = [0.2, 0.3];
/// How long his coming out of a host is told (`Twk_Powers_Possess.m_fMagicalAppearanceDelay`).
const APPEARANCE: f32 = 0.8;

/// The powers' moments told to those watching.
#[allow(clippy::too_many_arguments)]
fn broadcast(
    time: Res<Time>,
    mut used: MessageReader<PowerUsed>,
    mut out: MessageWriter<Magic>,
    powers: Res<crate::powers::Powers>,
    tc: Res<crate::gameplay::TimeControl>,
    possession: Res<crate::possession::Possession>,
    player: Query<&Transform, With<Player>>,
    // (Blink under way and where it began; Bend Time running and whether it stops the world;
    // possessing; his coming out told for so long)
    mut was: Local<(Option<Vec3>, Option<bool>, bool, f32)>,
) {
    let pos = player.single().map(|t| t.translation).ok();
    for u in used.read() {
        let mode = match u.0 {
            Power::Blink => {
                was.0 = pos;
                Some(MagicMode::Disappear)
            }
            Power::Windblast | Power::DevouringSwarm | Power::SummonAssassin => Some(MagicMode::Cast),
            // (Bend Time: told only when it stops the world, as it does at its second level)
            Power::BendTime if tc.world_dilation <= 1e-8 => Some(MagicMode::Disappear),
            _ => None,
        };
        if let Some(mode) = mode {
            out.write(Magic { mode, power: u.0 });
        }
    }
    // Blink over: where he comes out (if he moved)
    if was.0.is_some() && powers.blink.is_none() {
        if let (Some(from), Some(to)) = (was.0, pos) {
            if from.distance(to) > 1e-3 {
                out.write(Magic { mode: MagicMode::Appear, power: Power::Blink });
            }
        }
        was.0 = None;
    }
    // Bend Time over: out of stopped time he appears; slowed, a spell seen
    let bending = tc.bend_remaining > 0.0;
    match (was.1, bending) {
        (Some(stopped), false) => {
            out.write(Magic { mode: if stopped { MagicMode::Appear } else { MagicMode::Cast }, power: Power::BendTime });
            was.1 = None;
        }
        (_, true) => was.1 = Some(tc.world_dilation <= 1e-8),
        _ => {}
    }
    // into a host: gone; out of one: there again, told a while
    let possessing = possession.host.is_some();
    if possessing && !was.2 {
        out.write(Magic { mode: MagicMode::Disappear, power: Power::Possess });
    }
    if !possessing && was.2 {
        was.3 = APPEARANCE;
    }
    was.2 = possessing;
    if was.3 > 0.0 {
        out.write(Magic { mode: MagicMode::Appear, power: Power::Possess });
        was.3 -= time.delta_secs();
    }
}

/// Those who see him take what he does (the brain's witness rule) and react in time.
fn witness(
    time: Res<Time>,
    mut magic: MessageReader<Magic>,
    level: Option<Res<crate::level::LevelInfo>>,
    mut npcs: Query<(&mut Npc, &Transform), Without<Player>>,
    player: Query<&Transform, With<Player>>,
) {
    let dt = time.delta_secs();
    let corvo = player.single().map(|t| t.translation).ok();
    let told: Vec<Magic> = magic.read().copied().collect();
    let log = std::env::var("DH_NPC_LOG").is_ok();
    for (mut npc, t) in &mut npcs {
        // (last frame's line said)
        npc.magic_bark = None;
        if npc.is_down() {
            npc.magic_pending = None;
            npc.magic_react = None;
            continue;
        }
        let busted = npc.attn_level >= 4;
        for m in &told {
            if !npc.sees_player {
                continue;
            }
            // (each telling clears the while before)
            npc.magic_pending = None;
            match m.mode {
                MagicMode::Cast if busted => react(&mut npc, false),
                MagicMode::Appear if busted => react(&mut npc, false),
                MagicMode::Appear => {
                    npc.magic_pending = Some((PENDING, false));
                    // his appearing catches its eye (`m_IncreaseForMagic`, `DACRT_Magic`)
                    if let Some(att) = level.as_ref().and_then(|l| crate::npc::attention_of(&npc, &l.scene)) {
                        crate::npc::raise_attention(&mut npc, att, att.magic, 13);
                        npc.last_seen = corvo;
                    }
                }
                MagicMode::Disappear if busted => npc.magic_pending = Some((PENDING, true)),
                _ => {}
            }
            if log {
                info!("{}: sees Corvo's {:?} ({:?}), busted {busted}", npc.name, m.power, m.mode);
            }
        }
        // the while: busted now (found him) or no longer (lost him)
        if let Some((left, was_busted)) = npc.magic_pending {
            if busted != was_busted {
                npc.magic_pending = None;
                react(&mut npc, was_busted);
            } else if left - dt <= 0.0 {
                npc.magic_pending = None;
            } else {
                npc.magic_pending = Some((left - dt, was_busted));
            }
        }
        // the reaction, its moment come
        if let Some((left, gone)) = npc.magic_react {
            if left - dt > 0.0 {
                npc.magic_react = Some((left - dt, gone));
                continue;
            }
            npc.magic_react = None;
            npc.magic_bark = Some(gone);
            let ally = dhcook::format::is_player_faction(&npc.faction);
            let busy = npc.attack_t.is_some() || npc.aim.is_some() || npc.grab.is_some();
            if gone && !ally && !busy {
                // (turned to where it saw him, taken aback)
                if let Some(at) = npc.last_seen {
                    let to = (at - t.translation).with_y(0.0);
                    if to.length() > 0.1 {
                        npc.yaw = (-to.x).atan2(-to.z);
                    }
                }
                npc.gesture(20);
            }
            if log {
                info!("{}: reacts to Corvo's magic (gone {gone})", npc.name);
            }
        }
    }
}

/// A reaction after `m_MagicReactionDelay`; gone: he vanished from it.
fn react(npc: &mut Npc, gone: bool) {
    npc.magic_react = Some((DELAY[0] + rand::random::<f32>() * (DELAY[1] - DELAY[0]), gone));
}
