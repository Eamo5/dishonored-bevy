//! AI barks: what NPCs say as their situation changes, from the dialog hooks of their voice
//! (`scene.barks`, cooked from `DisDialogVoiceData` + `DisDialogTree`): noticing something,
//! spotting the player, getting hurt, dying, losing the target, muttering on patrol...

use crate::audio::{event_length, PostEvent};
use crate::gameplay::PlayerStats;
use crate::level::LevelInfo;
use crate::npc::{Alert, Mode, Npc};
use crate::player::Player;
use crate::GameState;
use bevy::prelude::*;

pub struct BarksPlugin;

impl Plugin for BarksPlugin {
    fn build(&self, app: &mut App) {
        app.add_message::<Subtitle>()
            .init_resource::<BarkClock>()
            .add_systems(Update, npc_barks.run_if(in_state(GameState::InGame)));
    }
}

/// A line to show at the bottom of the screen (barks don't replace script dialogue).
#[derive(Message, Clone, Debug)]
pub struct Subtitle {
    pub text: String,
    pub secs: f32,
    /// it replaces what's on screen (an audiograph's words)
    pub priority: bool,
    /// a bark (not main dialogue: `SubtitlesMode_Dialogs` leaves it out)
    pub ambient: bool,
}

/// Per-NPC bark bookkeeping.
#[derive(Component)]
pub struct BarkState {
    alert: Alert,
    down: bool,
    health: f32,
    /// time until this NPC may bark again
    quiet: f32,
    /// next combat taunt / idle mutter
    next_chatter: f32,
    victory: bool,
    /// the last bark asked for (its count)
    bark_seen: u32,
    /// its clip when last seen, and how far in (the clip's cued lines between then and now)
    clip: Option<(crate::anim::ClipId, f32)>,
    /// down, its cry held for its clip to cue it (else said when this runs out), and once
    /// it has cried
    dying: Option<(f32, &'static [&'static str])>,
    cried: bool,
    /// seconds since it last spoke
    since: f32,
}

/// Global pacing: one voice at a time near the player.
#[derive(Resource, Default)]
struct BarkClock {
    busy: f32,
}

/// Barks are heard (and subtitled) within this range (metres).
const HEAR: f32 = 25.0;

#[allow(clippy::too_many_arguments)]
fn npc_barks(
    mut commands: Commands,
    time: Res<Time>,
    level: Option<Res<LevelInfo>>,
    stats: Res<PlayerStats>,
    mut clock: ResMut<BarkClock>,
    player: Query<&Transform, With<Player>>,
    mut npcs: Query<(Entity, &Npc, &Transform, Option<&mut BarkState>, Option<&crate::anim::Animator>, Option<&crate::npc::NpcAnim>)>,
    mut out: MessageWriter<PostEvent>,
    mut subs: MessageWriter<Subtitle>,
    data: Res<crate::gamedata::Data>,
) {
    let Some(level) = level else { return };
    let Ok(pt) = player.single() else { return };
    let dt = time.delta_secs();
    clock.busy = (clock.busy - dt).max(0.0);
    let log = std::env::var("DH_AUDIO_LOG").is_ok();
    for (e, npc, t, state, anim, clips) in &mut npcs {
        let Some(mut st) = state else {
            commands.entity(e).try_insert(BarkState {
                alert: npc.alert,
                down: npc.is_down(),
                health: npc.health,
                quiet: 2.0,
                next_chatter: 8.0 + rand::random::<f32>() * 20.0,
                victory: false,
                bark_seen: npc.bark_req.0,
                clip: None,
                dying: None,
                cried: false,
                since: 10.0,
            });
            continue;
        };
        let Some(voice) = npc.voice.and_then(|v| level.scene.barks.get(v as usize)) else { continue };
        st.quiet -= dt;
        st.next_chatter -= dt;
        st.since += dt;
        let d = t.translation.distance(pt.translation);
        // the line its clip cues as it passes it (`DishonoredNotify_FireDialogHook`)
        let mut cued: Option<String> = None;
        // (a cry its clip has still to cue)
        let mut cry_coming = false;
        let cry = |h: &str| h == "COMBAT_DYING" || h == "COMBAT_BACKSTABBED";
        if let (Some(anim), Some(clips)) = (anim, clips) {
            if let Some(p) = anim.current() {
                let from = match st.clip {
                    Some((c, t0)) if c == p.clip && t0 <= p.t => t0,
                    _ => -1.0,
                };
                cued = clips.clip_hooks(p.clip).iter().find(|(at, _)| *at > from && *at <= p.t).map(|(_, h)| h.clone());
                cry_coming = clips.clip_hooks(p.clip).iter().any(|(at, h)| *at > p.t && cry(h));
                st.clip = Some((p.clip, p.t));
            }
        }
        // (one cry a death)
        if cued.as_deref().is_some_and(|h| cry(h) && st.cried) {
            cued = None;
        }
        let one: [&str; 1];
        // (hooks to try in order, urgent: ignores pacing)
        let mut want: Option<(&[&str], bool)> = None;
        let down = npc.is_down();
        if !down {
            st.cried = false;
        }
        if down && !st.down {
            // (the clip may cue it: else said shortly)
            let stealth = st.health > 0.0 && st.alert != Alert::Combat;
            if npc.mode == Mode::Dead && !stealth {
                st.dying = Some((0.35, &["COMBAT_DYING"]));
            } else if npc.mode == Mode::Dead {
                st.dying = Some((0.35, &["COMBAT_BACKSTABBED"]));
            }
        }
        if let Some(h) = cued.as_deref().filter(|h| st.dying.is_some() || !(h.starts_with("COMBAT_OUCH") && st.since < 1.5)) {
            // its clip's own line, in time with it
            st.cried |= cry(h) || st.dying.is_some();
            st.dying = None;
            one = [h];
            want = Some((&one, true));
        } else if let Some((left, hooks)) = st.dying {
            if left - dt <= 0.0 && !cry_coming {
                st.dying = None;
                st.cried = true;
                want = Some((hooks, true));
            } else {
                st.dying = Some((left - dt, hooks));
            }
        } else if down {
        } else if npc.bark_req.0 != st.bark_seen {
            // a bark asked for (a shove's "personal space")
            st.bark_seen = npc.bark_req.0;
            want = Some((std::slice::from_ref(&npc.bark_req.1), true));
        } else if !down && npc.health < st.health - 0.5 {
            let big = st.health - npc.health >= npc.arms.big_hit;
            want = Some((if big { &["COMBAT_OUCH_BIG", "COMBAT_OUCH_SMALL"] } else { &["COMBAT_OUCH_SMALL", "COMBAT_OUCH_BIG"] }, true));
        } else if !down && npc.alert != st.alert {
            want = match (st.alert, npc.alert) {
                (Alert::Unaware, Alert::Suspicious) => Some((&["NOTICE_SUSPICIOUS_ACTIVITY", "SUSPICIONDIST", "ATTENTION_TRACKTARGET_BEGIN"], false)),
                (Alert::Unaware, Alert::Combat) => Some((&["COMBAT_SURPRISED", "COMBAT_THREAT"], true)),
                (_, Alert::Combat) => Some((&["COMBAT_THREAT", "COMBAT_THREAT_GROUP", "COMBAT_SURPRISED"], true)),
                (Alert::Combat, _) => Some((&["COMBAT_LOST", "SEARCH_STOPHUNTING", "SEARCH_GIVEUP_POSTCORPSE"], false)),
                (Alert::Suspicious, Alert::Unaware) => Some((&["SEARCH_STOPHUNTING", "COMBAT_GOING_HOME"], false)),
                _ => None,
            };
        } else if !down && npc.alert == Alert::Combat && stats.dead && !st.victory {
            st.victory = true;
            want = Some((&["COMBAT_VICTORY", "COMBAT_VICTORY_LOST_ALLIES"], false));
        } else if !down && st.next_chatter <= 0.0 {
            st.next_chatter = if npc.alert == Alert::Combat { 6.0 + rand::random::<f32>() * 8.0 } else { 15.0 + rand::random::<f32>() * 30.0 };
            want = match npc.alert {
                Alert::Combat if npc.mode == Mode::Combat => Some((&["COMBAT_ATTACK", "COMBAT_TAUNT_WITH_GESTURE", "COMBAT_THREAT_VERSUS"], false)),
                Alert::Unaware if d < 12.0 => Some((&["SUSPICIONLEVEL"], false)),
                _ => None,
            };
        }
        st.alert = npc.alert;
        st.down = down;
        st.health = npc.health;
        let Some((hooks, urgent)) = want else { continue };
        if d > HEAR || (!urgent && (st.quiet > 0.0 || clock.busy > 0.0)) {
            continue;
        }
        let Some(lines) = hooks.iter().find_map(|h| voice.hooks.iter().find(|x| x.hook == *h)).map(|h| &h.lines) else { continue };
        if lines.is_empty() {
            continue;
        }
        let (event, text) = &lines[rand::random_range(0..lines.len())];
        let len = event_length(event).unwrap_or(1.5);
        if log {
            info!("bark {} {:?}: {event} \"{text}\"", npc.name, hooks.first());
        }
        out.write(PostEvent::named(event, Some(t.translation + Vec3::Y * 0.7)).spoken_by(e));
        // sounds like *cough* are not subtitled
        if !text.is_empty() && !text.starts_with('*') && d < HEAR * 0.6 {
            // the character's own name ("Anton Sokolov"), else its kind ("City Watch Guard")
            let (package, object) = npc.pawn.split_once('.').unwrap_or((&npc.pawn, &npc.pawn));
            let speaker = data.0.pawn_names.get(object).or(data.0.pawn_names.get(package)).unwrap_or(&npc.name);
            subs.write(Subtitle { text: format!("{speaker}: {text}"), secs: len + 0.3, priority: false, ambient: true });
        }
        st.quiet = len + 3.0;
        st.since = 0.0;
        clock.busy = len.min(2.5);
    }
}
