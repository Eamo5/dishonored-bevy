//! Characters keep out of each other's way, and Corvo's, as the original's avoidance does
//! (Arkane's `ArkAvoidable`, decoded for this rewrite from the user's own Dishonored.exe: the
//! manager's tick 0x00938440, a character's setup each tick 0x00B7CD10, the locomotion's fold
//! 0x0094A630).
//! - Each character, and Corvo, is an avoidable with a radius and a force. A character's
//!   radius is `m_fAvoidanceFightingRadius` (20 uu) while it fights, else
//!   `m_fAvoidanceRunningRadius` (20) while it runs, else `m_fAvoidanceDefaultRadius` (10),
//!   eased there at 100 uu a second; its force `m_fAvoidanceRootMotionForceMultiplier` (10)
//!   while a clip carries it, else `m_fAvoidanceDefaultForceMultiplier` (1), eased at 10 a
//!   second. Corvo's are his pawn tweak's `m_fAvoidanceRadius` (60) and
//!   `m_fAvoidanceForceMultiplier` (2); possessing, his own body isn't about.
//! - Each pair less than 300 uu apart in height pushes apart along the line between them: in
//!   full within their radii summed, then less, to nothing at three times that sum (80 uu at
//!   least). Unequal forces share it out: each is pushed by twice the other's force over their
//!   sum, the one giving way at most in full. A character gives way to Corvo, and hardly
//!   moves one a clip carries.
//! - Corvo is never pushed. A character's pushes are summed, and with the danger zones' they
//!   bend its way as it moves, together at most a full stride (`npc_move`); standing, it isn't
//!   moved.

use crate::gameplay::TimeControl;
use crate::npc::{Mode, Npc, ScriptedAnim};
use crate::player::Player;
use crate::GameState;
use bevy::prelude::*;

pub struct AvoidPlugin;

impl Plugin for AvoidPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Update, avoid.after(crate::danger::steer).before(crate::npc::npc_move).run_if(in_state(GameState::InGame)));
    }
}

/// How fast a radius eases to its want (m/s: 100 uu a second) and a force to its (a second).
const RADIUS_EASE: f32 = 1.0;
const FORCE_EASE: f32 = 10.0;
/// Pairs further apart in height push nothing (m: 300 uu).
const HEIGHT_APART: f32 = 3.0;
/// The push's reach is three times the radii summed, at least this (m: 80 uu).
const REACH_MIN: f32 = 0.8;
const UU: f32 = 0.01;

/// How strongly two avoidables `d` apart, their radii summed `r`, push apart.
fn strength(d: f32, r: f32) -> f32 {
    let reach = (3.0 * r).max(REACH_MIN);
    if d <= 0.0 || d >= reach {
        0.0
    } else if d <= r {
        1.0
    } else {
        (reach - d) / (reach - r)
    }
}

/// Each one's share of a push `s` strong between forces `fa` and `fb`.
fn shares(s: f32, fa: f32, fb: f32) -> (f32, f32) {
    if (fa - fb).abs() <= 0.01 {
        return (s, s);
    }
    let k = 2.0 / (fa + fb) * s;
    if fa > fb { (fb * k, (fa * k).min(1.0)) } else { ((fb * k).min(1.0), fa * k) }
}

/// Each character's radius and force eased, and its pushes summed (`Npc::avoid`).
fn avoid(
    time: Res<Time>,
    tc: Res<TimeControl>,
    data: Res<crate::gamedata::Data>,
    possession: Res<crate::possession::Possession>,
    player: Query<&Transform, With<Player>>,
    mut npcs: Query<(Entity, &mut Npc, &Transform, Has<ScriptedAnim>), (Without<Player>, Without<crate::possession::Possessed>)>,
) {
    let (world, own) = (time.delta_secs().min(0.05) * tc.world_scale(), time.delta_secs().min(0.05) * tc.own_scale());
    let tw = |k: &str, d: f32| data.pawn(k, d);
    let (fight, run, walk) = (tw("npc.m_fAvoidanceFightingRadius", 20.0) * UU, tw("npc.m_fAvoidanceRunningRadius", 20.0) * UU, tw("npc.m_fAvoidanceDefaultRadius", 10.0) * UU);
    let (carried, plain) = (tw("npc.m_fAvoidanceRootMotionForceMultiplier", 10.0), tw("npc.m_fAvoidanceDefaultForceMultiplier", 1.0));
    // who (Corvo: none), where, radius, force
    let mut all: Vec<(Option<Entity>, Vec3, f32, f32)> = Vec::new();
    if let (Ok(pt), None) = (player.single(), possession.host) {
        all.push((None, pt.translation, tw("m_fAvoidanceRadius", 60.0) * UU, tw("m_fAvoidanceForceMultiplier", 2.0)));
    }
    for (e, mut npc, t, scripted) in &mut npcs {
        npc.avoid = Vec3::ZERO;
        if npc.is_down() || npc.floored {
            continue;
        }
        let dt = if npc.out_of_bend { own } else { world };
        let want = if npc.mode == Mode::Combat { fight } else if npc.running { run } else { walk };
        let force = if npc.pace.is_some() || scripted { carried } else { plain };
        if npc.avoid_r < 0.0 {
            (npc.avoid_r, npc.avoid_f) = (want, force);
        }
        npc.avoid_r += (want - npc.avoid_r).clamp(-RADIUS_EASE * dt, RADIUS_EASE * dt);
        npc.avoid_f += (force - npc.avoid_f).clamp(-FORCE_EASE * dt, FORCE_EASE * dt);
        all.push((Some(e), t.translation, npc.avoid_r, npc.avoid_f));
    }
    let mut push = vec![Vec3::ZERO; all.len()];
    // (`DH_NO_AVOID`, for tests: the characters ease their radii but push nothing)
    let n = if std::env::var_os("DH_NO_AVOID").is_some() { 0 } else { all.len() };
    for i in 0..n {
        for j in i + 1..all.len() {
            let ((_, pa, ra, fa), (_, pb, rb, fb)) = (all[i], all[j]);
            if (pa.y - pb.y).abs() >= HEIGHT_APART {
                continue;
            }
            let d = (pb - pa).with_y(0.0);
            let len = d.length();
            let s = strength(len, ra + rb);
            if s <= 0.0 {
                continue;
            }
            let (sa, sb) = shares(s, fa, fb);
            push[i] -= d / len * sa;
            push[j] += d / len * sb;
        }
    }
    let log = std::env::var("DH_AVOID_LOG").is_ok();
    for (k, (who, ..)) in all.iter().enumerate() {
        if let Some(Ok((_, mut npc, ..))) = who.map(|e| npcs.get_mut(e)) {
            npc.avoid = push[k];
            if log && push[k].length() > 0.05 {
                info!("avoid: {} pushed {:.2} (radius {:.2} m, force {:.1})", npc.name, push[k].length(), npc.avoid_r, npc.avoid_f);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_push_is_full_within_the_radii_and_fades_to_three_times_them() {
        // two walkers (10 uu each): full to 0.2 m, nothing from 0.8 m (the least reach)
        assert_eq!(strength(0.1, 0.2), 1.0);
        assert!((strength(0.5, 0.2) - 0.5).abs() < 1e-5);
        assert_eq!(strength(0.8, 0.2), 0.0);
        // a walker by Corvo (10 + 60 uu): it reaches 2.1 m
        assert!(strength(2.0, 0.7) > 0.0 && strength(2.1, 0.7) == 0.0);
    }

    #[test]
    fn the_weaker_force_gives_way() {
        assert_eq!(shares(0.6, 1.0, 1.0), (0.6, 0.6));
        // a character (1) by Corvo (2): it is pushed 4/3 as hard (at most in full), he 2/3
        let (npc, corvo) = shares(0.6, 1.0, 2.0);
        assert!((npc - 0.8).abs() < 1e-5 && (corvo - 0.4).abs() < 1e-5);
        assert_eq!(shares(1.0, 1.0, 2.0).0, 1.0);
        // one a clip carries (10) hardly moves
        let (carried, other) = shares(1.0, 10.0, 1.0);
        assert!(carried < 0.2 && other == 1.0);
    }
}
