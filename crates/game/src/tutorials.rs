//! The tutorial notes (`EDisTutorialNote`, `DisGFxMoviePlayerNote`'s `m_TutorialNotes`): the
//! level scripts hand one over with a pickup (`DisSeqAct_GivePickup` of a
//! `DisTweaks_AbstractItemPickupNote`, its `m_TutorialNote`) and put up their message to read it
//! ("Press `GBA_Journal` to view the Combat Tutorial"); some items bring their own (the
//! blueprints', the rewire tool's `m_TutorialNote`). While the message is up the journal's key
//! reads it: the note movie's tutorial page (`n_Tutorial`, its content at the note's label, its
//! pictures from `UI_Tuto_*`, its words `t_tuto_<note>_*`). Each is kept, to read again from the
//! pause menu's help.

use crate::gamedata::Data;
use crate::gameplay::PlayerStats;
use crate::GameState;
use bevy::prelude::*;

pub struct TutorialsPlugin;

impl Plugin for TutorialsPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Update, prompt.run_if(in_state(GameState::InGame)));
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
    let own = data.text(NOTE, &format!("t_tuto_{name}_{k}_keyboard"));
    let t = if own.is_empty() { data.text(NOTE, &format!("t_tuto_{name}_{k}")) } else { own };
    crate::gamedata::readable(&t)
}
