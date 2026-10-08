//! Audiographs (`DisAbstractItemAudioLog`): punched cards whose recordings play on a player
//! (`DisAudioLogPlayer`: "Use" plays the card in it, "Stop the audiograph" stops it) or from the
//! journal ("PLAY AUDIOGRAPH"). A recording is its speaker's `Dlg_AudioGraphs` blurbs one after
//! another, their words as subtitles; the player's flavor sounds (the card's hiss) open and close
//! it. Hearing one files its card in the journal.

use crate::audio::{event_id, event_length, PostEvent, StopEvent};
use crate::barks::Subtitle;
use crate::GameState;
use bevy::prelude::*;

pub struct AudiographPlugin;

impl Plugin for AudiographPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Playback>()
            .add_message::<PlayAudiograph>()
            .add_systems(Update, play.run_if(in_state(GameState::InGame)))
            .add_systems(OnExit(GameState::InGame), |mut p: ResMut<Playback>| *p = Playback::default());
    }
}

/// Play a card (its key, `AbstractInv_Graphs.AG_..`; at a player, else from the journal), or
/// stop the one playing (`key: None`).
#[derive(Message, Clone, Default)]
pub struct PlayAudiograph {
    pub key: Option<String>,
    pub at: Option<Vec3>,
    /// the player's usable (its card comes back out when the recording ends)
    pub rig: Option<u32>,
    /// its start and stop sounds (`m_pFlavorSounds`)
    pub flavor: Vec<String>,
}

/// The journal's key for a card.
pub fn note_key(key: &str) -> String {
    format!("ag:{key}")
}

/// The recording under way.
#[derive(Resource, Default)]
pub struct Playback {
    current: Option<Current>,
    /// the players whose recordings ended (their cards come back out)
    pub ended: Vec<u32>,
}

impl Playback {
    /// The card playing.
    pub fn playing(&self) -> Option<&str> {
        self.current.as_ref().map(|c| c.key.as_str())
    }
    /// The player it plays on.
    pub fn rig(&self) -> Option<u32> {
        self.current.as_ref().and_then(|c| c.rig)
    }
}

struct Current {
    key: String,
    at: Option<Vec3>,
    rig: Option<u32>,
    stop_sound: Option<String>,
    /// the lines' events and when each starts; the subtitles' words, start and length
    lines: Vec<(f32, String)>,
    subs: Vec<(f32, f32, String)>,
    next_line: usize,
    next_sub: usize,
    t: f32,
    end: f32,
    /// the line playing
    event: Option<u32>,
}

/// The hiss before the voice starts, and between lines (s).
const LEAD: f32 = 0.9;
const GAP: f32 = 0.5;
/// The longest subtitle (characters).
const SUB_CHARS: usize = 120;

/// A line's words in subtitle-sized pieces, at sentence ends where it can.
fn chunks(text: &str) -> Vec<String> {
    let mut sentences: Vec<String> = Vec::new();
    let mut cur = String::new();
    let chars: Vec<char> = text.split_whitespace().collect::<Vec<_>>().join(" ").chars().collect();
    for (i, &c) in chars.iter().enumerate() {
        cur.push(c);
        if matches!(c, '.' | '!' | '?') && chars.get(i + 1) == Some(&' ') {
            sentences.push(std::mem::take(&mut cur).trim().to_string());
        }
    }
    if !cur.trim().is_empty() {
        sentences.push(cur.trim().to_string());
    }
    let mut out: Vec<String> = Vec::new();
    for s in sentences {
        // a long sentence breaks at its commas, else its spaces
        let mut s = s.as_str();
        while s.chars().count() > SUB_CHARS {
            let cut = s.char_indices().take_while(|(i, _)| *i < SUB_CHARS).filter(|(_, c)| *c == ',').map(|(i, _)| i + 1).last().or_else(|| s.char_indices().take_while(|(i, _)| *i < SUB_CHARS).filter(|(_, c)| *c == ' ').map(|(i, _)| i).last());
            let Some(cut) = cut.filter(|&c| c > 20) else { break };
            out.push(s[..cut].trim().to_string());
            s = s[cut..].trim_start();
        }
        match out.last_mut() {
            Some(last) if last.chars().count() + s.chars().count() < SUB_CHARS / 2 && !last.ends_with(',') => {
                last.push(' ');
                last.push_str(s);
            }
            _ => out.push(s.to_string()),
        }
    }
    out.retain(|s| !s.is_empty());
    out
}

fn stop(cur: Current, sfx: &mut MessageWriter<PostEvent>, stops: &mut MessageWriter<StopEvent>, ended: &mut Vec<u32>) {
    if let Some(e) = cur.event {
        stops.write(StopEvent(e));
    }
    if let Some(s) = &cur.stop_sound {
        sfx.write(PostEvent::named(s, cur.at));
    }
    if let Some(r) = cur.rig {
        ended.push(r);
    }
}

#[allow(clippy::too_many_arguments)]
fn play(
    time: Res<Time<Real>>,
    data: Option<Res<crate::gamedata::Data>>,
    mut asks: MessageReader<PlayAudiograph>,
    mut pb: ResMut<Playback>,
    mut sfx: MessageWriter<PostEvent>,
    mut stops: MessageWriter<StopEvent>,
    mut subs: MessageWriter<Subtitle>,
    mut stats: ResMut<crate::gameplay::PlayerStats>,
    mut plog: ResMut<crate::pickuplog::PickupLog>,
) {
    let pb = &mut *pb;
    for ask in asks.read() {
        if let Some(cur) = pb.current.take() {
            stop(cur, &mut sfx, &mut stops, &mut pb.ended);
            // (its words go with it)
            subs.write(Subtitle { text: String::new(), secs: 0.0, priority: true, ambient: false });
        }
        let Some(key) = &ask.key else { continue };
        let Some(ag) = data.as_ref().and_then(|d| d.0.audiographs.get(key)) else {
            warn!("audiograph {key}: not in the game data");
            continue;
        };
        // filed in the journal (the tweak's `m_pPickupSoundEvent`)
        let note = note_key(key);
        if !stats.notes.contains(&note) {
            stats.notes.push(note);
            plog.add(ag.title.clone(), Some("Audiograph_Small"));
            sfx.write(PostEvent::named("Snd_UI_Ingame_Book_Pickup", None));
        }
        let (start, stop_sound) = (ask.flavor.iter().find(|f| !f.ends_with("_Stop")), ask.flavor.iter().find(|f| f.ends_with("_Stop")));
        if let Some(s) = start {
            sfx.write(PostEvent::named(s, ask.at));
        }
        let mut t = if start.is_some() { LEAD } else { 0.2 };
        let (mut lines, mut list) = (Vec::new(), Vec::new());
        for (event, text) in &ag.lines {
            let pieces = chunks(text);
            let total: usize = pieces.iter().map(|p| p.chars().count()).sum::<usize>().max(1);
            let len = event_length(event).unwrap_or(total as f32 / 14.0);
            lines.push((t, event.clone()));
            let mut at = t;
            for p in pieces {
                let d = len * p.chars().count() as f32 / total as f32;
                list.push((at, (d - 0.1).max(0.5), p));
                at += d;
            }
            t += len + GAP;
        }
        info!("audiograph {key}: \"{}\", {} lines, {:.1} s", ag.title, lines.len(), t);
        pb.current = Some(Current { key: key.clone(), at: ask.at, rig: ask.rig, stop_sound: stop_sound.cloned(), lines, subs: list, next_line: 0, next_sub: 0, t: 0.0, end: t, event: None });
    }
    let Some(cur) = pb.current.as_mut() else { return };
    cur.t += time.delta_secs();
    while let Some((at, ev)) = cur.lines.get(cur.next_line) {
        if *at > cur.t {
            break;
        }
        sfx.write(PostEvent::named(ev, cur.at).voiced());
        cur.event = Some(event_id(ev));
        cur.next_line += 1;
    }
    while let Some((at, secs, text)) = cur.subs.get(cur.next_sub) {
        if *at > cur.t {
            break;
        }
        subs.write(Subtitle { text: text.clone(), secs: *secs, priority: true, ambient: false });
        cur.next_sub += 1;
    }
    if cur.t >= cur.end {
        let mut cur = pb.current.take().unwrap();
        cur.event = None;
        stop(cur, &mut sfx, &mut stops, &mut pb.ended);
    }
}

#[cfg(test)]
mod tests {
    use super::chunks;

    #[test]
    fn splits_at_sentences() {
        let c = chunks("Sokolov here. Excellent progress today. Test subject 312 is declining rapidly, as I theorized, and the formula holds, though the dosage must be raised again before the week is out, or all is lost.");
        assert!(c.iter().all(|s| s.chars().count() <= super::SUB_CHARS), "{c:?}");
        assert_eq!(c[0], "Sokolov here. Excellent progress today.");
    }
}
