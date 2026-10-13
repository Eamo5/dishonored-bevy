//! AI barks: what NPCs say as their situation changes, from the dialog hooks of their voice
//! (`scene.barks`, cooked from `DisDialogVoiceData` + `DisDialogTree`): noticing something,
//! spotting the player, getting hurt, dying, losing the target, muttering on patrol...

use crate::audio::{event_length, PostEvent};
use crate::gameplay::{HitKind, PlayerStats};
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
    /// what it was doing, burning, heading for a bell, a grenade by it (for the hooks of
    /// their changes)
    mode: Mode,
    burning: bool,
    alarm: bool,
    grenade: bool,
    /// falling, aiming its pistol, the gesture last seen (its count), and how long since a
    /// projectile last struck it
    falling: bool,
    aiming: bool,
    /// a body found, a death seen (as last seen)
    saw_body: bool,
    witnessed_death: bool,
    act_seen: u32,
    shot: f32,
}

/// Global pacing: one voice at a time near the player.
#[derive(Resource, Default)]
struct BarkClock {
    busy: f32,
}

/// Falling this fast (m/s down) is a fall worth a cry.
const FALL_SPEED: f32 = -6.0;
/// A weeper's barks as it follows or weeps (`DisTweaks_AIBehavior_Weep`'s `m_fFollowingBark_Min`
/// / `_Max`, `m_fStandStillWeepingBark_Min` / `_Max`).
const WEEP_BARK: [f32; 2] = [2.0, 5.0];

/// Others of its side within this (m) make a character's lines its group's
/// (`m_eBarkToPlayWhenGrouped`: `COMBAT_THREAT_GROUP`, `COMBAT_LOST_GROUP`,
/// `SEARCH_STOPHUNTING_GROUP`), as far as a fight spreads.
const GROUP_REACH: f32 = 10.0;

/// The output a hook node takes for what its speaker knows (`DisConv_Hook_<class>`, as roshan's
/// r614/blink research decoded them): a suspicion level's (`SuspicionLevel`, `SuspicionDist`)
/// by whether it suspects anything, a notice's (`Notice`) by whether it saw or heard; others
/// all their lines.
fn hook_output(class: &str, npc: &Npc) -> Option<usize> {
    match class {
        "SuspicionLevel" | "SuspicionDist" => Some((npc.alert != Alert::Unaware) as usize),
        "Notice" => Some(if npc.sees_player { 0 } else { 1 }),
        _ => None,
    }
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
    (mut used, burning, grenades): (MessageReader<crate::powers::PowerUsed>, Query<(), With<crate::powers::Ignite>>, Query<&Transform, With<crate::gadgets::Grenade>>),
    mut hits: MessageReader<crate::gameplay::NpcHit>,
) {
    let Some(level) = level else { return };
    let Ok(pt) = player.single() else { return };
    let dt = time.delta_secs();
    clock.busy = (clock.busy - dt).max(0.0);
    let log = std::env::var("DH_AUDIO_LOG").is_ok();
    // a power Corvo used this frame (Blink: he vanishes from where they saw him)
    let magic = used.read().last().map(|u| u.0);
    // (who a bolt or bullet struck this frame)
    let shot: Vec<Entity> = hits.read().filter(|h| matches!(h.kind, HitKind::Bolt | HitKind::Bullet | HitKind::ExplosiveBullet | HitKind::ExplosiveBolt)).map(|h| h.npc).collect();
    // (who stands where, for the barks said in a group: those of one side within reach)
    let standing: Vec<(Entity, Vec3, String)> = npcs.iter().filter(|(_, n, ..)| !n.is_down()).map(|(e, n, t, ..)| (e, t.translation, n.faction.clone())).collect();
    // (and the bodies: dead or knocked out)
    let bodies: Vec<(Vec3, bool)> = npcs.iter().filter(|(_, n, ..)| n.is_down()).map(|(_, n, t, ..)| (t.translation, n.mode == Mode::Dead)).collect();
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
                mode: npc.mode,
                burning: false,
                alarm: false,
                grenade: false,
                falling: false,
                aiming: false,
                saw_body: npc.saw_body,
                witnessed_death: npc.witnessed_death,
                act_seen: npc.act.1,
                shot: 10.0,
            });
            continue;
        };
        let Some(voice) = npc.voice.and_then(|v| level.scene.barks.get(v as usize)) else { continue };
        st.quiet -= dt;
        st.next_chatter -= dt;
        st.since += dt;
        st.shot = if shot.contains(&e) { 0.0 } else { st.shot + dt };
        // (a weeper turning on him starts its following barks' clock)
        if npc.arms.plague.is_some() && npc.alert == Alert::Combat && st.alert != Alert::Combat {
            st.next_chatter = st.next_chatter.min(WEEP_BARK[1]);
        }
        let d = t.translation.distance(pt.translation);
        let grouped = standing.iter().any(|(o, at, f)| *o != e && *f == npc.faction && at.distance(t.translation) < GROUP_REACH);
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
        // (the hook node's output, where the moment decides it)
        let mut out_hint: Option<usize> = None;
        let down = npc.is_down();
        if !down {
            st.cried = false;
        }
        if down && !st.down {
            // (the clip may cue it: else said shortly)
            let stealth = st.health > 0.0 && st.alert != Alert::Combat;
            if npc.mode == Mode::Dead && !stealth {
                st.dying = Some((0.35, &["COMBAT_DYING"]));
            } else if npc.mode == Mode::Dead && st.shot < 0.5 {
                // (unaware, a bolt or bullet: `COMBAT_STEALTHKILL_PROJECTILE`)
                st.dying = Some((0.35, &["COMBAT_STEALTHKILL_PROJECTILE", "COMBAT_BACKSTABBED"]));
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
        } else if !down && !npc.grounded && npc.velocity.y < FALL_SPEED && !st.falling {
            // falling (`FALLING`: off a ledge, thrown)
            want = Some((&["FALLING"], true));
        } else if !down && npc.act.1 != st.act_seen && npc.act.0 == 8 {
            // stamping on a rat (`PATROL_ATTACK_RATSINGLE`)
            want = Some((&["PATROL_ATTACK_RATSINGLE"], false));
        } else if !down && npc.aim.is_some() && !st.aiming {
            // drawing a bead on him (`COMBAT_RANGED_BEGIN`)
            want = Some((&["COMBAT_RANGED_BEGIN"], false));
        } else if !down && burning.contains(e) && !st.burning {
            // set alight (`COMBAT_IMMOLATED`)
            want = Some((&["COMBAT_IMMOLATED"], true));
        } else if !down && npc.mode == Mode::Flee && st.mode != Mode::Flee {
            // a civilian starting to run (`PANIC_START`, `FLEEING`)
            want = Some((&["PANIC_START", "FLEEING"], true));
        } else if !down && npc.alarm_run.is_some() && !st.alarm {
            // off to ring the alarm (`ALARM_GO_TO_RING_ALARM`)
            want = Some((&["ALARM_GO_TO_RING_ALARM", "ALARM_HEAR_AND_RESPOND"], true));
        } else if !down && npc.hostile() && !st.grenade && grenades.iter().any(|g| g.translation.distance(t.translation) < 5.0) {
            // a grenade by it (`ESCAPE_IMPENDING_EXPLOSION`)
            want = Some((&["ESCAPE_IMPENDING_EXPLOSION"], true));
        } else if !down && npc.hostile() && npc.sees_player && magic.is_some() {
            // Corvo's magic before its eyes: gone from where it saw him, else a power seen
            // (`MAGIC_DISAPPEARED`, `WITNESSEDMAGIC`)
            want = Some(if magic == Some(crate::powers::Power::Blink) { (&["MAGIC_DISAPPEARED", "MAGIC_WITNESSED"], true) } else { (&["MAGIC_WITNESSED"], true) });
        } else if !down && npc.witnessed_death && !st.witnessed_death && !npc.saw_body {
            // a death before its eyes (`SEARCH_WITNESS_DEATH`)
            want = Some((&["SEARCH_WITNESS_DEATH"], true));
        } else if !down && npc.saw_body && !st.saw_body {
            // a body found (`SEARCH_DETECT_CORPSE`: its `Hook_DeathMode` way for a dead one, or
            // one knocked out), the one it goes to
            let at = npc.target.unwrap_or(t.translation);
            let dead = bodies.iter().min_by(|a, b| a.0.distance(at).total_cmp(&b.0.distance(at))).is_none_or(|b| b.1);
            out_hint = Some(if dead { 0 } else { 1 });
            want = Some((if npc.witnessed_death { &["SEARCH_WITNESS_DEATH", "SEARCH_DETECT_CORPSE"] } else { &["SEARCH_DETECT_CORPSE"] }, true));
        } else if !down && npc.alert == Alert::Combat && st.alert == Alert::Combat && npc.mode == Mode::Combat && matches!(st.mode, Mode::Search | Mode::Investigate) {
            // found again while searching (`SEARCH_FIND_ENEMY`)
            want = Some((&["SEARCH_FIND_ENEMY", "COMBAT_THREAT"], true));
        } else if !down && npc.alert == Alert::Suspicious && st.alert == Alert::Suspicious && npc.mode == Mode::Investigate && st.mode != Mode::Investigate {
            // off to look again (`ATTENTION_INVESTIGATE`, else `ATTENTION_NOTICE_BEGIN`)
            want = Some((&["ATTENTION_INVESTIGATE", "ATTENTION_NOTICE_BEGIN"], false));
        } else if !down && npc.alert != st.alert {
            want = match (st.alert, npc.alert) {
                (Alert::Unaware, Alert::Suspicious) => Some((&["ATTENTION_NOTICE_BEGIN", "NOTICE_SUSPICIOUS_ACTIVITY", "ATTENTION_HEADTRACK", "ATTENTION_TRACKTARGET_BEGIN"], false)),
                // (a weeper's voice has only its own: `WEEP_NOTICED_TARGET`)
                (Alert::Unaware, Alert::Combat) => Some((&["COMBAT_SURPRISED", "COMBAT_THREAT", "WEEP_NOTICED_TARGET"], true)),
                // (with others of its side about, their group's lines first)
                (_, Alert::Combat) if grouped => Some((&["COMBAT_THREAT_GROUP", "COMBAT_THREAT", "COMBAT_SURPRISED", "WEEP_NOTICED_TARGET"], true)),
                (_, Alert::Combat) => Some((&["COMBAT_THREAT", "COMBAT_THREAT_GROUP", "COMBAT_SURPRISED", "WEEP_NOTICED_TARGET"], true)),
                (Alert::Combat, _) if grouped => Some((&["COMBAT_LOST_GROUP", "COMBAT_LOST", "SEARCH_STOPHUNTING_GROUP", "SEARCH_STOPHUNTING"], false)),
                (Alert::Combat, _) => Some((&["COMBAT_LOST", "SEARCH_STOPHUNTING", "SEARCH_GIVEUP_POSTCORPSE"], false)),
                // (a notice that came to nothing: `ATTENTION_NOTICE_END`)
                (Alert::Suspicious, Alert::Unaware) if grouped => Some((&["SEARCH_STOPHUNTING_GROUP", "SEARCH_STOPHUNTING", "ATTENTION_NOTICE_END", "COMBAT_GOING_HOME"], false)),
                (Alert::Suspicious, Alert::Unaware) => Some((&["SEARCH_STOPHUNTING", "ATTENTION_NOTICE_END", "COMBAT_GOING_HOME"], false)),
                _ => None,
            };
        } else if !down && npc.alert == Alert::Combat && stats.dead && !st.victory {
            st.victory = true;
            want = Some((&["COMBAT_VICTORY", "COMBAT_VICTORY_LOST_ALLIES"], false));
        } else if !down && st.next_chatter <= 0.0 && npc.arms.plague.is_some() && npc.alert == Alert::Combat {
            // a weeper after him moans as it follows or weeps standing (`m_fFollowingBark`,
            // `m_fStandStillWeepingBark`: every 2-5 s)
            st.next_chatter = WEEP_BARK[0] + rand::random::<f32>() * (WEEP_BARK[1] - WEEP_BARK[0]);
            let moving = npc.velocity.with_y(0.0).length() > 0.3;
            want = Some(if moving { (&["WEEP_FOLLOWING", "WEEP_MOAN"], false) } else { (&["WEEP_STAND_STILL_WEEPING", "WEEP_MOAN"], false) });
        } else if !down && st.next_chatter <= 0.0 {
            st.next_chatter = if npc.alert == Alert::Combat { 6.0 + rand::random::<f32>() * 8.0 } else { 15.0 + rand::random::<f32>() * 30.0 };
            want = match npc.alert {
                // (the taunt's line comes with its gesture, cued by its clip)
                Alert::Combat if npc.mode == Mode::Combat => Some((&["COMBAT_ATTACK", "COMBAT_THREAT_VERSUS"], false)),
                // (the patrol's chatter: `PATROL_MISC`, with others of its side about its group's)
                Alert::Unaware if d < 12.0 && grouped => Some((&["PATROL_MISC_GROUP", "PATROL_MISC"], false)),
                Alert::Unaware if d < 12.0 => Some((&["PATROL_MISC"], false)),
                _ => None,
            };
        }
        st.alert = npc.alert;
        st.down = down;
        st.health = npc.health;
        st.mode = npc.mode;
        st.burning = burning.contains(e);
        st.alarm = npc.alarm_run.is_some();
        st.grenade = grenades.iter().any(|g| g.translation.distance(t.translation) < 5.0);
        st.falling = !npc.grounded && npc.velocity.y < FALL_SPEED;
        st.saw_body = npc.saw_body;
        st.witnessed_death = npc.witnessed_death;
        st.aiming = npc.aim.is_some();
        st.act_seen = npc.act.1;
        let Some((hooks, urgent)) = want else { continue };
        if d > HEAR || (!urgent && (st.quiet > 0.0 || clock.busy > 0.0)) {
            continue;
        }
        let Some(bark) = hooks.iter().find_map(|h| voice.hooks.iter().find(|x| x.hook == *h)) else { continue };
        // (the lines of the hook node's output for what it knows, where it has some)
        let pool: Vec<usize> = match out_hint.or_else(|| hook_output(&bark.class, &npc)).and_then(|k| bark.by_output.get(k)).filter(|o| !o.is_empty()) {
            Some(o) => o.iter().map(|i| *i as usize).filter(|i| *i < bark.lines.len()).collect(),
            None => (0..bark.lines.len()).collect(),
        };
        if pool.is_empty() {
            continue;
        }
        let (event, text) = &bark.lines[pool[rand::random_range(0..pool.len())]];
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
