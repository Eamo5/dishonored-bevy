//! The Heart: held in the left hand it beats faster as Corvo nears (and faces) runes and bone
//! charms (`DisTweaks_Heart`: 20° directness, 40 m reach, 0.25-4 beats a second); used on a
//! person, a rat or nowhere in particular it whispers its secrets, the original
//! `Dlg_HeartGadget` lines chosen by the target's story group and the campaign's progress.

use crate::audio::PostEvent;
use crate::gamedata::Data;
use crate::interact::{Pickup, PickupKind};
use crate::npc::Npc;
use crate::player::PlayerCamera;
use crate::powers::{Power, Powers};
use crate::GameState;
use bevy::prelude::*;
use bevy::window::{CursorGrabMode, CursorOptions};
use dhcook::format::ConvGraph;
use std::collections::HashMap;

pub struct HeartPlugin;

impl Plugin for HeartPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Heart>()
            .add_systems(Update, (beat, whisper).run_if(in_state(GameState::InGame)));
    }
}

#[derive(Resource, Default)]
pub struct Heart {
    /// seconds to the next beat
    next_beat: f32,
    beat_toggle: bool,
    /// how strongly it calls (0..1), for the HUD and the glow
    pub pulse: f32,
    /// sequential branches' progress (node -> next output)
    seq: HashMap<u32, usize>,
    cooldown: f32,
    /// within reach of a rune or charm (its hint given), and the hints given
    near_target: bool,
    hints_shown: u32,
}

/// Unreal units to metres.
const UU: f32 = 0.01;

fn beat(
    time: Res<Time>,
    powers: Res<Powers>,
    mut heart: ResMut<Heart>,
    cam: Query<&GlobalTransform, With<PlayerCamera>>,
    pickups: Query<(&Pickup, &Transform)>,
    mut sfx: MessageWriter<PostEvent>,
    (stats, mut msgs, data): (Res<crate::gameplay::PlayerStats>, ResMut<crate::gameplay::HudMessages>, Res<crate::gamedata::Data>),
    switches: Res<crate::kismet::ScriptSwitches>,
) {
    let dt = time.delta_secs();
    heart.cooldown = (heart.cooldown - dt).max(0.0);
    heart.pulse = (heart.pulse - dt * 2.0).max(0.0);
    let Ok(cg) = cam.single() else { return };
    let eye = cg.translation();
    let fwd = cg.forward().as_vec3();
    let reach = 4000.0 * UU;
    if powers.selected != Power::Heart {
        // a rune or charm within the Heart's reach, the Heart not in hand: the pawn's
        // `m_HeartTargetTutorialMessage`, once per approach (`m_TutorialHeartTargetDistanceBuffer`
        // beyond the reach to leave it) and `m_TutorialHeartTargetShowCount` times at most
        if !Power::Heart.owned(&stats) {
            return;
        }
        let nearest = pickups.iter().filter(|(p, _)| matches!(p.kind, PickupKind::Rune | PickupKind::BoneCharm)).map(|(_, t)| t.translation.distance(eye)).reduce(f32::min);
        if std::env::var("DH_HEART_LOG").is_ok() {
            info!("heart: nearest target {nearest:?}, near {}, hints {}", heart.near_target, heart.hints_shown);
        }
        let buffer = data.pawn("m_TutorialHeartTargetDistanceBuffer", 100.0) * UU;
        let cap = data.pawn("m_TutorialHeartTargetShowCount", 350.0) as u32;
        match nearest {
            Some(d) if d < reach && !heart.near_target => {
                heart.near_target = true;
                if heart.hints_shown < cap && !switches.0.systemic_off {
                    heart.hints_shown += 1;
                    msgs.tutorial(data.pawn_text("m_HeartTargetTutorialMessage", "Rune or Bone Charm nearby. Equip the Heart."), 10.0);
                }
            }
            Some(d) if d < reach + buffer => {}
            _ => heart.near_target = false,
        }
        return;
    }
    heart.near_target = true;
    // the closest rune or charm, and whether Corvo faces it
    let mut best: Option<(f32, bool)> = None;
    for (p, t) in &pickups {
        if !matches!(p.kind, PickupKind::Rune | PickupKind::BoneCharm) {
            continue;
        }
        let to = t.translation - eye;
        let d = to.length();
        if d > reach {
            continue;
        }
        let facing = to.normalize_or_zero().dot(fwd) > 20f32.to_radians().cos();
        if best.is_none_or(|b| d < b.0) {
            best = Some((d, facing));
        }
    }
    let (rate, facing) = match best {
        Some((d, facing)) => {
            let near = 1.0 - d / reach;
            if facing {
                (0.6 + (4.0 - 0.6) * near * near, true)
            } else {
                (0.25 + 0.75 * near, false)
            }
        }
        None => (0.25, false),
    };
    heart.next_beat -= dt;
    if heart.next_beat <= 0.0 {
        heart.next_beat = 1.0 / rate;
        heart.beat_toggle = !heart.beat_toggle;
        sfx.write(PostEvent::named(if heart.beat_toggle { "Heart_Gadget_Beat_1" } else { "Heart_Gadget_Beat_2" }, None));
        heart.pulse = if facing { 1.0 } else { 0.4 };
    }
}

/// Walk a dialogue graph from a node to a line.
fn pick(g: &ConvGraph, node: i32, group: &str, flags: &dyn Fn(&str) -> bool, seq: &mut HashMap<u32, usize>, depth: u32) -> Option<u32> {
    if node < 0 || depth > 32 {
        return None;
    }
    let n = g.nodes.get(node as usize)?;
    match n.class.as_str() {
        "Blurb" => Some(node as u32),
        "SequentialBranch" => {
            let k = seq.entry(node as u32).or_insert(0);
            let i = (*k).min(n.outputs.len().saturating_sub(1));
            *k = if *k + 1 >= n.outputs.len() { if n.looping { 0 } else { n.outputs.len().saturating_sub(1) } } else { *k + 1 };
            pick(g, *n.outputs.get(i)?, group, flags, seq, depth + 1)
        }
        "RandomBranch" => {
            let total: f32 = n.weights.iter().sum();
            let mut r = rand::random::<f32>() * total.max(1e-3);
            let mut i = 0;
            for (k, w) in n.weights.iter().enumerate() {
                if r < *w {
                    i = k;
                    break;
                }
                r -= w;
                i = k;
            }
            pick(g, *n.outputs.get(i).or(n.outputs.first())?, group, flags, seq, depth + 1)
        }
        "SpeakerInStoryGroup" => {
            // the branch of the target's group, or the last ("anyone else")
            let i = n.groups.iter().position(|(gname, _)| !gname.is_empty() && gname.eq_ignore_ascii_case(group)).unwrap_or(n.outputs.len().saturating_sub(1));
            pick(g, *n.outputs.get(i)?, group, flags, seq, depth + 1)
        }
        "CheckStoryFlag" => {
            let i = if flags(&n.flag) { 0 } else { 1 };
            pick(g, *n.outputs.get(i).or(n.outputs.first())?, group, flags, seq, depth + 1)
        }
        // hooks and anything else: the first output that leads somewhere
        _ => n.outputs.iter().find_map(|o| pick(g, *o, group, flags, seq, depth + 1)),
    }
}

#[allow(clippy::too_many_arguments)]
fn whisper(
    mouse: Res<ButtonInput<MouseButton>>,
    cursor: Single<&CursorOptions>,
    scripted: Option<Res<crate::script::Scripted>>,
    powers: Res<Powers>,
    data: Res<Data>,
    vm: Option<Res<crate::kismet::Vm>>,
    mut heart: ResMut<Heart>,
    cam: Query<&GlobalTransform, With<PlayerCamera>>,
    npcs: Query<(&Npc, &Transform)>,
    (mut sfx, mut subs): (MessageWriter<PostEvent>, MessageWriter<crate::barks::Subtitle>),
) {
    let grabbed = cursor.grab_mode != CursorGrabMode::None || scripted.is_some();
    if powers.selected != Power::Heart || !grabbed || !mouse.just_pressed(MouseButton::Right) || heart.cooldown > 0.0 {
        return;
    }
    let g = &data.0.heart;
    let Ok(cg) = cam.single() else { return };
    let eye = cg.translation();
    let fwd = cg.forward().as_vec3();
    // aimed at someone? (the original reaches 250 m; a person must be in view)
    let target = npcs
        .iter()
        .filter(|(n, t)| !n.is_down() && (t.translation - eye).length() < 60.0)
        .map(|(n, t)| (n, (t.translation + Vec3::Y * 0.5 - eye).normalize_or_zero().dot(fwd)))
        .filter(|(_, c)| *c > 0.985)
        .max_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(n, _)| n);
    let flags = |f: &str| vm.as_ref().is_some_and(|v| v.flag(f));
    let (hook, out, group) = match target {
        Some(n) => ("HEART_TARGET_WHISPER", 2usize, n.story_group.clone()),
        None => ("HEART_WORLD_AMBIENT_WHISPER", 0, String::new()),
    };
    let Some(&h) = g.hooks.get(hook) else { return };
    let start = g.nodes[h as usize].outputs.get(out).or(g.nodes[h as usize].outputs.first()).copied().unwrap_or(-1);
    let mut seq = std::mem::take(&mut heart.seq);
    let line = pick(g, start, &group, &flags, &mut seq, 0);
    heart.seq = seq;
    let Some(line) = line else { return };
    let n = &g.nodes[line as usize];
    info!("heart: {} ({group}): {}", if group.is_empty() { "world" } else { "target" }, n.text);
    sfx.write(PostEvent::named("Snd_UI_Heart_Whisper", None));
    if !n.event.is_empty() {
        sfx.write(PostEvent::named(&n.event, None));
    }
    let secs = (2.0 + n.text.len() as f32 * 0.06).min(9.0);
    subs.write(crate::barks::Subtitle { text: n.text.clone(), secs, priority: false, ambient: false });
    heart.cooldown = secs.min(4.0);
    heart.pulse = 1.0;
}
