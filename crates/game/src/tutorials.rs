//! The tutorial notes (`EDisTutorialNote`, `DisGFxMoviePlayerNote`'s `m_TutorialNotes`): the
//! level scripts hand one over with a pickup (`DisSeqAct_GivePickup` of a
//! `DisTweaks_AbstractItemPickupNote`, its `m_TutorialNote`) and put up their message to read it
//! ("Press `GBA_Journal` to view the Combat Tutorial"); some items bring their own (the
//! blueprints', the rewire tool's `m_TutorialNote`). While the message is up the journal's key
//! reads it: the note movie's tutorial page (`n_Tutorial`, its content at the note's label, its
//! pictures from `UI_Tuto_*`, its words `t_tuto_<note>_*`). Each is kept, to read again from the
//! pause menu's help.
//!
//! The game's own tutorials (`DishonoredPlayerPawn`'s `m_TutorialTrackers`, one
//! `DisTutorialTracker` per `ETutorialTracker`), as the exe raises them into the HUD's tutorial
//! stack, their numbers and words from Corvo's pawn tweak:
//! - the block: blows he might have blocked landing `m_BlockMissThreshold` (2) times in a row
//!   ("Press and hold Ctrl to block"); a block puts it away and starts the count again;
//! - the perfect block: plain blocks `m_PerfectBlockMissThreshold` (4) in a row; a perfect
//!   one (a tap parry) puts it away;
//! - the elixirs: the first of each kind picked up (`m_AcquiredElixirTutorialMessage_PC`); his
//!   health or mana brought below `m_TutorialElixirThreshold` (10) with one of that kind in
//!   hand (`m_UseElixirTutorialMessage_PC`: "Your Health is Low...");
//! - a movable: the first one picked up that is not a whale oil tank (`DisWhaleOilBattery`):
//!   "Press Left Mouse to throw objects...";
//! - the wheel: `m_TutorialWheelItemCountThreshold` (2) things on it, once: the Quick-access
//!   Wheel's note (`DTN_Wheel`) in the tutorial window and its message;
//! - the drop assassination: an enemy under the crosshair more than
//!   `m_fDropAssassinationTutorialHeight` (3 m) below and within
//!   `m_fDropAssassinationTutorialDistance` (10 m), once: its note (`DTN_DropAssassination`);
//! - the defense: an enemy fighting within `m_fDefenseTutorialDistance` (5 m) of him, sword in
//!   hand, once: a second on, the perfect block's note (`DTN_PerfectBlock`); his first perfect
//!   block brings the counter-attack's (`DTN_CounterAttack`) a second after it, as their
//!   trackers' delayed notes (`DelayedNoteInfo`) do;
//! - the music box's tune stopping his powers: `m_PowerInhibitedMessage`, up while it lasts.
//! The level scripts switch trackers off and on (`DisSeqAct_ToggleTutorial`); the dream holds
//! them all back (`DisSeqAct_EnableSystemicTutorials`). (The Heart's is `heart.rs`'s.)

use crate::gamedata::Data;
use crate::gameplay::PlayerStats;
use crate::GameState;
use bevy::prelude::*;

pub struct TutorialsPlugin;

impl Plugin for TutorialsPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Update, (prompt, trackers).run_if(in_state(GameState::InGame)));
    }
}

/// A tutorial note's key among the notes read (`tuto:DTN_Combat`).
pub const KEY: &str = "tuto:";
/// How long the message to read it is up (the HUD tweak's `m_fTutorialDuration`).
pub const PROMPT: f32 = 10.0;
/// The note movie's texts.
const NOTE: &str = "DisGFxMoviePlayerNote_Texts";

/// A tutorial note received: kept, and readable while its message is up. Whether it is new.
pub fn received(stats: &mut PlayerStats, id: &str) -> bool {
    if id.is_empty() || id == "DTN_None" {
        return false;
    }
    let new = !stats.tutorials.iter().any(|t| t == id);
    if new {
        stats.tutorials.push(id.to_string());
    }
    stats.tutorial_prompt = Some((id.to_string(), PROMPT));
    new
}

/// The message's time runs out.
fn prompt(time: Res<Time>, mut stats: ResMut<PlayerStats>) {
    if let Some((_, left)) = stats.tutorial_prompt.as_mut() {
        *left -= time.delta_secs();
        if *left <= 0.0 {
            stats.tutorial_prompt = None;
        }
    }
}

/// A tutorial note's page: its name (the content's label, its texts' stem) and its pictures'
/// package.
pub fn note<'a>(data: &'a Data, id: &str) -> Option<&'a dhcook::format::TutorialNote> {
    data.0.tutorial_notes.iter().find(|t| t.id == id)
}

/// Its title (`t_tuto_<name>_title`).
pub fn title(data: &Data, name: &str) -> String {
    crate::gamedata::readable(&data.text(NOTE, &format!("t_tuto_{name}_title")))
}

/// The words of its field `k` (`txt<k>`): `t_tuto_<name>_<k>`, the keyboard's where it has its
/// own.
pub fn words(data: &Data, name: &str, k: usize) -> String {
    // (the keyboard's own, or a pad in use the pad's: `_gamepad`)
    let own = data.text(NOTE, &format!("t_tuto_{name}_{k}_{}", if crate::pad::using() { "gamepad" } else { "keyboard" }));
    let t = if own.is_empty() { data.text(NOTE, &format!("t_tuto_{name}_{k}")) } else { own };
    crate::gamedata::readable(&t)
}

/// The trackers (`ETutorialTracker`), in order: their bits in `Switches::trackers_off`.
pub const TRACKERS: [&str; 8] = ["Block", "PerfectBlock", "Elixir", "Movable", "HeartTarget", "Wheel", "DropAssassination", "Defense"];
pub const TRACK_BLOCK: usize = 0;
pub const TRACK_PERFECT_BLOCK: usize = 1;
pub const TRACK_ELIXIR: usize = 2;
pub const TRACK_MOVABLE: usize = 3;
pub const TRACK_HEART: usize = 4;
pub const TRACK_WHEEL: usize = 5;
pub const TRACK_DROP: usize = 6;
pub const TRACK_DEFENSE: usize = 7;

/// Unreal units to metres.
const UU: f32 = 0.01;
/// The once-only ones shown (`Trackers::shown`).
const SHOWN_HEALTH: u32 = 1;
const SHOWN_MANA: u32 = 2;
const SHOWN_MOVABLE: u32 = 4;
const SHOWN_WHEEL: u32 = 8;
const SHOWN_DROP: u32 = 16;
const SHOWN_DEFENSE: u32 = 32;
const SHOWN_COUNTER: u32 = 64;
/// A delayed note's wait (`DelayedNoteInfo.m_fDelay`, as the trackers set it).
const NOTE_DELAY: f32 = 1.0;

/// What combat tells the trackers.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Hint {
    /// a blow he might have blocked landed
    BlockMissed,
    /// a blow blocked: a perfect block (a tap parry), or a plain one
    Blocked(bool),
}

/// The trackers' state: the once-only ones shown (kept with the game), the counts in a row.
#[derive(Clone, Default, Debug, serde::Serialize, serde::Deserialize)]
pub struct Trackers {
    #[serde(default)]
    pub shown: u32,
    #[serde(skip)]
    misses: u32,
    #[serde(skip)]
    plain_blocks: u32,
    /// this frame's word from combat
    #[serde(skip)]
    pub hints: Vec<Hint>,
}

/// What the trackers last saw (a level's first look only notes it).
#[derive(Default)]
struct Seen {
    start: bool,
    health: f32,
    mana: f32,
    elixirs: [u32; 2],
    held: Option<Entity>,
    inhibited: bool,
    wheel_check: f32,
    /// a tutorial note to give, and when
    note: Option<(&'static str, f32)>,
}

#[allow(clippy::too_many_arguments, clippy::type_complexity)]
fn trackers(
    time: Res<Time>,
    mut stats: ResMut<PlayerStats>,
    (data, settings, switches, tune, held): (Res<Data>, Res<crate::settings::Settings>, Res<crate::kismet::ScriptSwitches>, Res<crate::musicbox::Tune>, Res<crate::props::Held>),
    (mut msgs, mut window): (ResMut<crate::gameplay::HudMessages>, ResMut<crate::tutwindow::TutorialWindow>),
    (level, props): (Option<Res<crate::level::LevelInfo>>, Query<&crate::props::Prop>),
    player: Query<&Transform, With<crate::player::Player>>,
    cam: Query<&GlobalTransform, With<crate::player::PlayerCamera>>,
    npcs: Query<(&crate::npc::Npc, &Transform), Without<crate::player::Player>>,
    mut seen: Local<Seen>,
) {
    let hints = std::mem::take(&mut stats.trackers.hints);
    let sw = &switches.0;
    // (a tracker on: the scripts' switches, the tutorials option; else nothing is counted or
    // spent, as the HUD turns a tutorial away)
    let on = |t: usize| settings.tutorials && !sw.systemic_off && sw.trackers_off & 1 << t == 0;
    let text = |key: &str| data.pawn_text(key, "");
    let log = std::env::var("DH_TUTORIAL_LOG").is_ok();
    let say = |msgs: &mut crate::gameplay::HudMessages, key: &str| {
        let t = text(key);
        if log {
            info!("tutorial: {key}: {t:?}");
        }
        if !t.is_empty() {
            msgs.tutorial_cycling(t, PROMPT);
        }
    };
    let elixirs = [stats.health_elixirs, stats.mana_elixirs];
    // (a level loaded, a game too: what he carries in is not news)
    if !seen.start || level.as_ref().is_some_and(|l| l.is_changed()) {
        *seen = Seen { start: true, health: stats.health, mana: stats.mana, elixirs, held: held.0, inhibited: tune.inhibited, wheel_check: 0.0, note: None };
        return;
    }
    let tr = &mut stats.trackers;
    // the block and the perfect block (combat's word)
    for h in hints {
        match h {
            Hint::BlockMissed if on(TRACK_BLOCK) => {
                tr.misses += 1;
                if tr.misses == data.pawn("m_BlockMissThreshold", 2.0) as u32 {
                    say(&mut msgs, "m_BlockTutorialMessage");
                }
            }
            Hint::BlockMissed => {}
            Hint::Blocked(perfect) => {
                tr.misses = 0;
                msgs.drop_tutorial(&text("m_BlockTutorialMessage"));
                if perfect {
                    tr.plain_blocks = 0;
                    msgs.drop_tutorial(&text("m_PerfectBlockTutorialMessage"));
                    if on(TRACK_PERFECT_BLOCK) && tr.shown & SHOWN_COUNTER == 0 {
                        tr.shown |= SHOWN_COUNTER;
                        seen.note = Some(("DTN_CounterAttack", NOTE_DELAY));
                    }
                } else if on(TRACK_PERFECT_BLOCK) {
                    tr.plain_blocks += 1;
                    if tr.plain_blocks == data.pawn("m_PerfectBlockMissThreshold", 4.0) as u32 {
                        say(&mut msgs, "m_PerfectBlockTutorialMessage");
                    }
                }
            }
        }
    }
    // the elixirs: the first of each kind, and running low with one in hand
    if on(TRACK_ELIXIR) {
        for (k, bit) in [(0, SHOWN_HEALTH), (1, SHOWN_MANA)] {
            if elixirs[k] > seen.elixirs[k] && tr.shown & bit == 0 {
                tr.shown |= bit;
                say(&mut msgs, &format!("m_AcquiredElixirTutorialMessage_PC[{k}]"));
            }
        }
        let low = |k: usize| data.pawn(if k == 0 { "m_TutorialElixirThreshold" } else { "m_TutorialElixirThreshold[1]" }, 10.0);
        if !stats.dead && stats.health < seen.health && stats.health < low(0) && elixirs[0] > 0 {
            say(&mut msgs, "m_UseElixirTutorialMessage_PC[0]");
        }
        if stats.mana < seen.mana - 0.01 && stats.mana < low(1) && elixirs[1] > 0 {
            say(&mut msgs, "m_UseElixirTutorialMessage_PC[1]");
        }
    }
    let tr = &mut stats.trackers;
    // a movable picked up (a whale oil tank aside)
    if held.0 != seen.held && on(TRACK_MOVABLE) && tr.shown & SHOWN_MOVABLE == 0 {
        let tank = held.0.and_then(|e| props.get(e).ok()).and_then(|p| level.as_ref().and_then(|l| l.scene.movables.get(p.index))).map(|m| m.tank.is_some());
        if tank == Some(false) {
            tr.shown |= SHOWN_MOVABLE;
            say(&mut msgs, "m_ThrowMovableTutorialMessage");
        }
    }
    // the drop assassination: an enemy under the crosshair well below
    if on(TRACK_DROP) && tr.shown & SHOWN_DROP == 0 {
        if let (Ok(pt), Ok(cg)) = (player.single(), cam.single()) {
            let (eye, fwd) = (cg.translation(), cg.forward().as_vec3());
            // (Corvo's feet: his capsule's middle less half his 1.8 m)
            let feet = pt.translation.y - 0.9;
            let (height, reach) = (data.pawn("m_fDropAssassinationTutorialHeight", 300.0) * UU, data.pawn("m_fDropAssassinationTutorialDistance", 1000.0) * UU);
            let under = npcs.iter().any(|(n, t)| {
                let to = t.translation + Vec3::Y * 0.9 - eye;
                let along = to.dot(fwd);
                n.hostile() && !n.is_down() && along > 0.0 && (to - fwd * along).length() < 0.5 && feet - t.translation.y > height && (pt.translation - t.translation).with_y(0.0).length() < reach
            });
            if under {
                if log {
                    info!("tutorial: drop assassination");
                }
                tr.shown |= SHOWN_DROP;
                window.push(text("m_DropAssassinationNoteTitle"), crate::tutwindow::IMG_GEAR);
                received(&mut stats, "DTN_DropAssassination");
            }
        }
    }
    // the wheel: two things on it
    seen.wheel_check -= time.delta_secs();
    if seen.wheel_check <= 0.0 && on(TRACK_WHEEL) && stats.trackers.shown & SHOWN_WHEEL == 0 {
        seen.wheel_check = 0.5;
        let n = crate::wheel::arrange(&stats, data.is_daud()).0.len();
        if n >= data.pawn("m_TutorialWheelItemCountThreshold", 2.0) as usize {
            if log {
                info!("tutorial: wheel ({n} on it)");
            }
            stats.trackers.shown |= SHOWN_WHEEL;
            window.push(text("m_WheelTutorialNoteTitle"), crate::tutwindow::IMG_GEAR);
            received(&mut stats, "DTN_Wheel");
            say(&mut msgs, "m_WheelTutorialMessage");
        }
    }
    // the defense: an enemy fighting close, sword in hand
    if on(TRACK_DEFENSE) && stats.trackers.shown & SHOWN_DEFENSE == 0 && !stats.unarmed && !stats.dead {
        if let Ok(pt) = player.single() {
            let reach = data.pawn("m_fDefenseTutorialDistance", 500.0) * UU;
            if npcs.iter().any(|(n, t)| n.hostile() && n.mode == crate::npc::Mode::Combat && t.translation.distance(pt.translation) < reach) {
                stats.trackers.shown |= SHOWN_DEFENSE;
                seen.note = Some(("DTN_PerfectBlock", NOTE_DELAY));
            }
        }
    }
    // a delayed note given (as an item's note: the journal's key reads it while its word is up)
    if let Some((id, left)) = seen.note.as_mut() {
        *left -= time.delta_secs();
        if *left <= 0.0 {
            let id = *id;
            seen.note = None;
            if log {
                info!("tutorial: note {id}");
            }
            if received(&mut stats, id) {
                msgs.push(format!("{} {}", crate::bindings::hint(crate::bindings::Act::Journal), data.text("DisGFxMoviePlayerMenuBase_Texts", "t_ReadTutorial")));
            }
        }
    }
    // the music box's tune stopping his powers (up while it lasts; the HUD's own gate)
    if tune.inhibited != seen.inhibited {
        let t = text("m_PowerInhibitedMessage");
        if tune.inhibited && settings.tutorials && !t.is_empty() {
            msgs.tutorial(t, f32::INFINITY);
        } else {
            msgs.drop_tutorial(&t);
        }
    }
    *seen = Seen { start: true, health: stats.health, mana: stats.mana, elixirs, held: held.0, inhibited: tune.inhibited, wheel_check: seen.wheel_check, note: seen.note };
}
