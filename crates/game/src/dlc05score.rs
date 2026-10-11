//! Dunwall City Trials' scoring: the rule set the scripts put in force
//! (`DisSeqAct_DLC05_SetScoringRules`: a `DisDLC05Tweaks_ChallengeScoringRuleset`'s rules,
//! their modifiers and the combos that multiply them), applied to what Corvo does. The rules'
//! classes are the game's native code; what each scores is read from its settings and the
//! briefings' words:
//! - kills (`ScoringRule_WaveKill`: the victim's gain by its kind, `ScoringRule_BTM_Kill`,
//!   `ScoringRule_Kill`), with the flair of their modifiers (`ScoringModifier_*`: Assassinate,
//!   Drop Kill, Adrenaline, Headshot, In Flames, Dark Kill, Get Back; their extra gain, and
//!   once each a Novelty's), headshots and shots (`ScoringRule_Headshot`, `_Shoot`,
//!   `_SurpriseShot`), mayhem (`_Mayhem`: the world's harm), the chain (`_ChainKill`: so many a
//!   second left on the kill chain's timer) and drops (`_DropAssassination`: so many a metre
//!   fallen); escaping sight once spotted in a fight (`_Vanish`: its settings, a spotted
//!   entry, a vanish and a no-kill duration);
//! - a round's bonuses (`ScoringBonus_*`: quick, all by the blade, all by shots, no elixir,
//!   unhurt, unseen) and a bent time's kills (`ScoringBonus_BTM_Kills`);
//! - the combos' multipliers (`ChallengeRule_Combo_*`: kills in quick succession, unseen,
//!   by the blade, rounds survived, several at once);
//! - time (`ScoringRule_Chrono`: a gain over the time taken; `_ChronoBonus`: by thresholds, of
//!   the whole run or between the scripts' time markers), health and mana left
//!   (`_PlayerStat`), the thief's loot and stealth (`_Thief`: coins, eggs in order, each
//!   person's attention, the eggs' multiplier) and the mystery foe's (`_MysteryMan`).

use crate::challenge::Challenge;
use crate::gameplay::{HitKind, PlayerStats, PlayerTakedown, TimeControl};
use bevy::prelude::*;
use dhcook::format::{RulesetDef, ScoreRuleDef};
use std::collections::{HashMap, HashSet};

pub struct Dlc05ScorePlugin;

impl Plugin for Dlc05ScorePlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Scoring>().add_systems(Update, (score, tanks).after(crate::challenge::ChallengeSet).run_if(in_state(crate::GameState::InGame)));
    }
}

/// What the rules keep of the run.
#[derive(Resource, Default)]
pub struct Scoring {
    /// the modifiers used once (their Novelty given)
    novelty: HashSet<String>,
    /// the combos: the last kill's time, the kills in quick succession, unseen, by the blade
    last_kill: Option<f32>,
    streak: u32,
    unseen: u32,
    brutal: u32,
    /// the kills by shots each soon after the last (`Combo_MultipleShots`), by blasts
    /// (`Combo_Blast`): how many, the last's time
    shot_streak: (u32, Option<f32>),
    blast_streak: (u32, Option<f32>),
    /// kills in this frame (several at once: Boom!)
    burst: u32,
    /// rounds survived (`Survivor`)
    rounds: u32,
    /// the round's: its start, its kills (all, by the blade, by shots, unseen), elixirs and
    /// health at its start, hurt
    round: Option<Round>,
    /// a bent time's kills
    bend_kills: u32,
    bent: bool,
    /// the time markers' last (`ECE_Challenge_TimeMarker`)
    last_marker: f32,
    /// each person's highest attention, knocked out, killed (the thief's and the foe's
    /// stealth)
    attention: HashMap<Entity, (u8, bool, bool)>,
    health_seen: f32,
    /// Oil Drop: the tanks shot down, the last one's time and the quick succession it ends,
    /// the shots fired before the run; the wave's tanks thrown and missed
    tanks: u32,
    tank_last: Option<f32>,
    tank_combo: u32,
    shots0: Option<u32>,
    wave: Option<(u32, u32)>,
    /// each person's part in a kill's flair: it hurt Corvo (`Payback`), its blow was parried
    /// (`PerfectBlock`), it was knocked down (`Bellup`: Wind Blast, a blast), when
    marks: HashMap<Entity, Marks>,
    /// the last kill's faction, when (`FactionCombo`)
    last_faction: Option<(String, f32)>,
    /// the kills by a shot (an accuracy's hits: `ScoringRule_Accuracy`)
    shot_kills: u32,
    /// seen in a fight and not yet out of sight (`ScoringRule_Vanish`); unseen since when
    spotted: bool,
    unseen_since: Option<f32>,
    /// the powers Corvo used lately, when (`Power Combo`)
    casts: Vec<(crate::powers::Power, f32)>,
}

#[derive(Default, Clone, Copy)]
struct Marks {
    hurt_me: Option<f32>,
    parried: Option<f32>,
    floored: Option<f32>,
}

/// What a kill's flair may depend on besides the kill: the time, the victim's marks, the
/// previous kill's faction.
struct Flair<'a> {
    now: f32,
    dark: bool,
    marks: Marks,
    last_faction: Option<&'a (String, f32)>,
    casts: &'a [(crate::powers::Power, f32)],
}

#[derive(Default, Clone)]
struct Round {
    start: f32,
    kills: u32,
    blade: u32,
    shots: u32,
    unseen: u32,
    elixirs: u32,
    hurt: bool,
}

impl Scoring {
    pub fn reset(&mut self) {
        *self = Scoring::default();
    }

    /// The combos' multiplier in force (each by its settings: the kills it takes, its gain).
    pub fn multiplier(&self, set: Option<&RulesetDef>) -> f32 {
        let Some(set) = set else { return 1.0 };
        let mut m = 1.0;
        for c in &set.multipliers {
            let p = |k: &str, d: f32| c.params.get(k).copied().unwrap_or(d);
            let gain = p("m_iMultiplierGain", 1.0);
            let class = c.class.as_str();
            m += match class {
                // kills each within `m_fTimeBetweenTwoKill` of the last: one more for each
                // after the first
                "DisDLC05ChallengeRule_Combo_DeathStreak" => self.streak.saturating_sub(1) as f32,
                // so many unseen in a row, by the blade in a row
                "DisDLC05ChallengeRule_Combo_StealthyStreak" => gain * (self.unseen >= p("m_iRequiredKills", 3.0) as u32) as u8 as f32,
                "DisDLC05ChallengeRule_Combo_BrutalStreak" => gain * (self.brutal >= p("m_iRequiredKills", 5.0) as u32) as u8 as f32,
                // the rounds survived
                "DisDLC05ChallengeRule_Combo_Survivor" => gain * (self.rounds >= p("m_iNumWaves", 13.0) as u32) as u8 as f32,
                // several at once
                "DisDLC05ChallengeRule_Combo_Boom" => p("m_iExtraMultiplierGain", 1.0) * (self.burst >= 2) as u8 as f32,
                // blasts' kills each within `m_fTimeOut` of the last: the first pair's gain, then
                // each more's
                "DisDLC05ChallengeRule_Combo_Blast" => {
                    let n = self.blast_streak.0;
                    if n >= 2 {
                        p("m_iFirstMultiplierGain", 1.0) + (n - 2) as f32 * p("m_iExtraMultiplierGain", 1.0)
                    } else {
                        0.0
                    }
                }
                // shots' kills each within `m_fTimeOut` of the last
                "DisDLC05ChallengeRule_Combo_MultipleShots" => gain * self.shot_streak.0.saturating_sub(1) as f32,
                _ => 0.0,
            };
        }
        m
    }
}

/// The rule set in force.
pub fn set_of<'a>(ch: &Challenge, level: Option<&'a crate::level::LevelInfo>) -> Option<&'a RulesetDef> {
    level?.scene.challenge_rules.iter().find(|r| Some(r.name.as_str()) == ch.rules.as_deref())
}

fn rule<'a>(set: &'a RulesetDef, class: &str) -> impl Iterator<Item = &'a ScoreRuleDef> {
    let class = class.to_string();
    set.rules.iter().filter(move |r| r.class == class)
}

fn entry_of(r: &ScoreRuleDef, fallback: &str) -> String {
    if !r.entry.is_empty() {
        r.entry.clone()
    } else if !r.name.is_empty() {
        r.name.clone()
    } else {
        fallback.to_string()
    }
}

fn blade(k: HitKind) -> bool {
    matches!(k, HitKind::Sword | HitKind::Assassinate | HitKind::Fatality)
}

fn shot(k: HitKind) -> bool {
    matches!(k, HitKind::Bolt | HitKind::Bullet | HitKind::ExplosiveBullet | HitKind::Fire | HitKind::SleepDart)
}

/// A takedown's flair by a modifier's kind (`DisDLC05ScoringModifier_<class>`), within its
/// time out (`m_fTimeOut`) where it has one.
fn modifier_applies(m: &dhcook::format::ScoreModifierDef, t: &PlayerTakedown, f: &Flair) -> bool {
    let (kinds, dark) = (&m.kinds, f.dark);
    let within = |at: Option<f32>| at.is_some_and(|at| f.now - at <= m.params.get("m_fTimeOut").copied().unwrap_or(2.0));
    match m.class.trim_start_matches("DisDLC05ScoringModifier_") {
        // killed the one who hurt Corvo, soon after (`m_fTimeOut`: a second)
        "Payback" => within(f.marks.hurt_me),
        // killed after parrying its blow
        "PerfectBlock" => within(f.marks.parried),
        // killed while down (blown off its feet)
        "Bellup" => within(f.marks.floored),
        // a kill of another faction's soon after one of the first's
        "FactionCombo" => !t.faction.is_empty() && f.last_faction.is_some_and(|(fac, at)| *fac != t.faction && within(Some(*at))),
        // (the class's own words aside, its window: `m_fTimeOut`, 3 s) two powers or more
        // used together, the kill within it
        "PowerCombo" => {
            let mut used: Vec<crate::powers::Power> = Vec::new();
            for (p, _) in f.casts.iter().filter(|(_, at)| within(Some(*at))) {
                if !used.contains(p) {
                    used.push(*p);
                }
            }
            used.len() >= 2
        }
        "Assassinate" => match kinds.get("m_AssassinationType").map(String::as_str) {
            Some("DLC05MAT_DropAssassinate") => t.drop.is_some(),
            _ => t.kind == HitKind::Assassinate && t.drop.is_none(),
        },
        "Adrenaline" => t.kind == HitKind::Fatality,
        "Headshot" => t.head,
        "InFlames" => t.kind == HitKind::Fire,
        "DarkKill" => dark,
        "Getback" => t.kind == HitKind::Windblast,
        "LimbSevered" => matches!(t.kind, HitKind::Explosion | HitKind::GrenadeThrowback | HitKind::StickyGrenade | HitKind::SpringRazor),
        _ => false,
    }
}

#[allow(clippy::too_many_arguments)]
fn score(
    mut ch: ResMut<Challenge>,
    mut sc: ResMut<Scoring>,
    mut takedowns: MessageReader<PlayerTakedown>,
    level: Option<Res<crate::level::LevelInfo>>,
    stats: Res<PlayerStats>,
    tc: Res<TimeControl>,
    powers: Res<crate::powers::Powers>,
    npcs: Query<(Entity, &crate::npc::Npc)>,
    (mut player_hits, mut staggers, mut npc_hits): (MessageReader<crate::gameplay::PlayerHit>, MessageReader<crate::gameplay::NpcStagger>, MessageReader<crate::gameplay::NpcHit>),
    mut casts: MessageReader<crate::powers::PowerUsed>,
) {
    // who hurt Corvo, whose blow he parried, who was blown off their feet
    let at = ch.elapsed;
    for h in player_hits.read().filter(|h| h.damage > 0.0) {
        sc.marks.entry(h.npc).or_default().hurt_me = Some(at);
    }
    for s in staggers.read().filter(|s| s.parried) {
        sc.marks.entry(s.npc).or_default().parried = Some(at);
    }
    for h in npc_hits.read().filter(|h| matches!(h.kind, HitKind::Windblast | HitKind::Explosion | HitKind::GrenadeThrowback | HitKind::StickyGrenade | HitKind::ExplosiveBullet)) {
        sc.marks.entry(h.npc).or_default().floored = Some(at);
    }
    // the powers used (the last ten seconds')
    for c in casts.read() {
        sc.casts.push((c.0, at));
    }
    sc.casts.retain(|(_, t)| at - *t <= 10.0);
    if !ch.active() {
        takedowns.clear();
        return;
    }
    let Some(set) = set_of(&ch, level.as_deref()).cloned() else {
        takedowns.clear();
        return;
    };
    let now = ch.elapsed;
    if ch.ended.is_some() || !ch.started_run() {
        takedowns.clear();
        return;
    }
    // each person's highest attention (unaware, suspicious, searching, combat)
    for (e, n) in &npcs {
        let level = match n.alert {
            crate::npc::Alert::Unaware => 0,
            crate::npc::Alert::Suspicious => {
                if n.awareness > 0.5 {
                    2
                } else {
                    1
                }
            }
            crate::npc::Alert::Combat => 4,
        };
        let a = sc.attention.entry(e).or_insert((0, false, false));
        a.0 = a.0.max(level);
        if n.mode == crate::npc::Mode::Unconscious {
            a.1 = true;
        }
        if n.mode == crate::npc::Mode::Dead {
            a.2 = true;
        }
    }
    // the round: hurt
    if stats.health < sc.health_seen - 0.01 {
        if let Some(r) = sc.round.as_mut() {
            r.hurt = true;
        }
    }
    sc.health_seen = stats.health;
    // a bent time's kills, once it is over (`ScoringBonus_BTM_Kills`: so many at once)
    let bent = tc.bend_remaining > 0.0 || tc.scripted.is_some();
    if sc.bent && !bent && sc.bend_kills > 0 {
        let n = sc.bend_kills;
        for r in rule(&set, "DisDLC05ScoringBonus_BTM_Kills") {
            let (key, entry) = if n >= 7 {
                ("m_iBloodOrgyGain", "Blood Orgy")
            } else if n >= 5 {
                ("m_iKillFeastGain", "Kill Feast")
            } else if n >= 3 {
                ("m_iMurderGain", "Murder")
            } else {
                continue;
            };
            let p = r.params.get(key).copied().unwrap_or(0.0) as i64;
            ch.score_entry(entry, p);
        }
        sc.bend_kills = 0;
    }
    sc.bent = bent;
    sc.burst = 0;
    let takes: Vec<PlayerTakedown> = takedowns.read().cloned().collect();
    let lethal_now = takes.iter().filter(|t| t.lethal && t.hostile).count() as u32;
    sc.burst = lethal_now;
    for t in &takes {
        if !t.lethal {
            continue;
        }
        if !t.hostile {
            continue;
        }
        // the combos (their windows their settings')
        let window = |class: &str, key: &str, d: f32| set.multipliers.iter().find(|c| c.class.ends_with(class)).and_then(|c| c.params.get(key).copied()).unwrap_or(d);
        let quick = sc.last_kill.is_some_and(|l| now - l <= window("Combo_DeathStreak", "m_fTimeBetweenTwoKill", 4.0));
        sc.streak = if quick { sc.streak + 1 } else { 1 };
        sc.unseen = if t.unaware { sc.unseen + 1 } else { 0 };
        sc.brutal = if blade(t.kind) { sc.brutal + 1 } else { 0 };
        sc.last_kill = Some(now);
        let chained = |s: (u32, Option<f32>), on: bool, w: f32| -> (u32, Option<f32>) {
            match (on, s.1) {
                (false, _) => (0, None),
                (true, Some(at)) if now - at <= w => (s.0 + 1, Some(now)),
                (true, _) => (1, Some(now)),
            }
        };
        sc.shot_streak = chained(sc.shot_streak, shot(t.kind), window("Combo_MultipleShots", "m_fTimeOut", 4.0));
        let blast = matches!(t.kind, HitKind::Explosion | HitKind::GrenadeThrowback | HitKind::StickyGrenade | HitKind::ExplosiveBullet);
        sc.blast_streak = chained(sc.blast_streak, blast, window("Combo_Blast", "m_fTimeOut", 2.0));
        if bent {
            sc.bend_kills += 1;
        }
        if let Some(r) = sc.round.as_mut() {
            r.kills += 1;
            r.blade += blade(t.kind) as u32;
            r.shots += shot(t.kind) as u32;
            r.unseen += t.unaware as u32;
        }
        let before = ch.multiplier;
        let mult = sc.multiplier(Some(&set));
        ch.multiplier = mult;
        let mut points: Vec<(String, i64)> = Vec::new();
        let mut tricks: Vec<String> = Vec::new();
        // (a combo made: its name)
        if mult > before && mult > 1.0 {
            if let Some(c) = set.multipliers.iter().find(|c| !c.entry.is_empty()) {
                tricks.push(format!("{} x{}", c.entry, mult));
            }
        }
        for r in &set.rules {
            match r.class.as_str() {
                // the victim's gain by its kind, then the modifiers' flair
                "DisDLC05ScoringRule_WaveKill" | "DisDLC05ScoringRule_BTM_Kill" | "DisDLC05ScoringRule_Kill" => {
                    let tail = |s: &str| s.rsplit('.').next().unwrap_or(s).to_ascii_lowercase();
                    let by_kind = r.gains.iter().find(|(g, _, _)| !t.story_group.is_empty() && tail(g) == tail(&t.story_group));
                    let (base, entry) = match by_kind {
                        Some((_, p, e)) => (*p as i64, if e.is_empty() { entry_of(r, "Kill") } else { e.clone() }),
                        None => (r.base_gain as i64, entry_of(r, "Kill")),
                    };
                    if base > 0 {
                        points.push((entry, base));
                    }
                    let flair = Flair { now, dark: powers.dark_vision, marks: sc.marks.get(&t.npc).copied().unwrap_or_default(), last_faction: sc.last_faction.as_ref(), casts: &sc.casts };
                    let applying: Vec<&dhcook::format::ScoreModifierDef> = r.modifiers.iter().filter(|m| modifier_applies(m, t, &flair)).collect();
                    for m in applying {
                        {
                            let name = if m.entry.is_empty() { m.name.clone() } else { m.entry.clone() };
                            if m.extra > 0 {
                                points.push((name.clone(), m.extra as i64));
                            }
                            tricks.push(name.clone());
                            if m.novelty > 0 && sc.novelty.insert(name) {
                                points.push(("Novelty".into(), m.novelty as i64));
                            }
                        }
                    }
                }
                "DisDLC05ScoringRule_Headshot" if t.head => points.push((entry_of(r, "Headshot"), r.base_gain as i64)),
                "DisDLC05ScoringRule_Shoot" if shot(t.kind) => points.push((entry_of(r, "Shoot"), r.base_gain as i64)),
                "DisDLC05ScoringRule_SurpriseShot" if shot(t.kind) && t.unaware => points.push((entry_of(r, "Surprise shot"), r.base_gain as i64)),
                // the world's harm: explosions, fire, things thrown, traps
                "DisDLC05ScoringRule_Mayhem" if matches!(t.kind, HitKind::Explosion | HitKind::Impact | HitKind::SpringRazor | HitKind::Fire | HitKind::GrenadeThrowback | HitKind::StickyGrenade) => {
                    points.push((entry_of(r, "Mayhem"), r.base_gain as i64))
                }
                // so many a second left on the kill chain's timer
                "DisDLC05ScoringRule_ChainKill" => {
                    let left = ch.timer_of("DDHT_KillChainTimer").unwrap_or(0.0).max(0.0);
                    let p = (left * r.params.get("m_iGainPerSecond").copied().unwrap_or(0.0)) as i64;
                    if p > 0 {
                        points.push((entry_of(r, "Chain Kill"), p));
                    }
                }
                // so many a metre fallen (and the medals' heights)
                "DisDLC05ScoringRule_DropAssassination" => {
                    if let Some(h) = t.drop {
                        let per = r.params.get("m_fPerFallMeterGain").copied().unwrap_or(0.0);
                        let base = r.params.get("m_DropAssassitationBaseGain").copied().unwrap_or(0.0);
                        let p = (base + h * per) as i64;
                        if p > 0 {
                            points.push(("Drop Assassination".into(), p));
                        }
                        ch.drop_heights.push(h);
                    }
                }
                _ => {}
            }
        }
        // (the next kill's flair: this one's faction; an accuracy's hits)
        sc.last_faction = Some((t.faction.clone(), now));
        sc.shot_kills += shot(t.kind) as u32;
        for (e, p) in points {
            let p = (p as f32 * mult).round() as i64;
            ch.score_entry(&e, p);
        }
        if let Some(t) = tricks.last() {
            ch.trick(t);
        }
    }
    // escaping sight once spotted (`ScoringRule_Vanish`): seen by an enemy in a fight, then
    // out of every enemy's sight for `m_fVanishDuration` with no kill from `m_fNoKillDuration`
    // before (killing the witness isn't vanishing); once a spotting, the first a Novelty's more
    for r in rule(&set, "DisDLC05ScoringRule_Vanish") {
        let seen = npcs.iter().any(|(_, n)| n.hostile() && !n.is_down() && n.sees_player && n.alert == crate::npc::Alert::Combat);
        let log = std::env::var("DH_CHALLENGE_LOG").is_ok();
        if seen {
            if log && (!sc.spotted || sc.unseen_since.is_some()) {
                info!("challenge: spotted");
            }
            sc.spotted = true;
            sc.unseen_since = None;
            continue;
        }
        if log && sc.spotted && sc.unseen_since.is_none() {
            info!("challenge: out of sight");
        }
        if !sc.spotted {
            continue;
        }
        let since = *sc.unseen_since.get_or_insert(now);
        let (dur, calm) = (r.params.get("m_fVanishDuration").copied().unwrap_or(2.0), r.params.get("m_fNoKillDuration").copied().unwrap_or(2.0));
        if now - since < dur {
            continue;
        }
        sc.spotted = false;
        sc.unseen_since = None;
        if sc.last_kill.is_some_and(|k| k >= since - calm) {
            continue;
        }
        let e = entry_of(r, "Vanish");
        ch.score_entry(&e, r.base_gain as i64);
        let extra = r.params.get("m_iNoveltyExtraGain").copied().unwrap_or(2.0) as i64;
        if extra > 0 && sc.novelty.insert(e.clone()) {
            ch.score_entry("Novelty", extra);
        }
        ch.trick(&e);
    }
}

/// A round begins (a wave's title): the one before judged (`ScoringBonus_*`).
pub fn round_begins(ch: &mut Challenge, sc: &mut Scoring, set: Option<&RulesetDef>, stats: &PlayerStats) {
    round_ends(ch, sc, set);
    sc.round = Some(Round { start: ch.elapsed, elixirs: stats.health_elixirs + stats.mana_elixirs, ..default() });
    sc.health_seen = stats.health;
}

/// The round over: its bonuses.
pub fn round_ends(ch: &mut Challenge, sc: &mut Scoring, set: Option<&RulesetDef>) {
    let Some(r) = sc.round.take() else { return };
    let Some(set) = set else { return };
    if r.kills == 0 {
        return;
    }
    sc.rounds += 1;
    let took = ch.elapsed - r.start;
    for b in &set.rules {
        let ok = match b.class.as_str() {
            "DisDLC05ScoringBonus_Expeditious" => took <= b.params.get("m_fTimeOut").copied().unwrap_or(20.0),
            "DisDLC05ScoringBonus_Fencer" => r.blade == r.kills,
            "DisDLC05ScoringBonus_SharpShooter" => r.shots == r.kills,
            "DisDLC05ScoringBonus_Skinflint" => ch.elixirs_now >= r.elixirs,
            "DisDLC05ScoringBonus_Invincible" => !r.hurt,
            "DisDLC05ScoringBonus_Ghost" => r.unseen == r.kills,
            _ => continue,
        };
        if ok {
            let e = entry_of(b, "Bonus");
            ch.score_entry(&e, b.base_gain as i64);
            ch.trick(&e);
        }
    }
}

/// A time marker (`ECE_Challenge_TimeMarker`: a beam reached): the thresholds' bonuses of the
/// time since the last (`ScoringRule_ChronoBonus` with `m_bChallengeTime` off).
pub fn time_marker(ch: &mut Challenge, sc: &mut Scoring, set: Option<&RulesetDef>) {
    let Some(set) = set else { return };
    let since = ch.elapsed - sc.last_marker;
    sc.last_marker = ch.elapsed;
    for r in rule(set, "DisDLC05ScoringRule_ChronoBonus") {
        if r.params.get("m_bChallengeTime").copied().unwrap_or(1.0) > 0.5 {
            continue;
        }
        if let Some(p) = chrono_bonus(r, since) {
            ch.score_entry("Time Bonus", p);
        }
    }
}

/// `ScoringRule_ChronoBonus`: the gain of the quickest threshold made (none: zero).
fn chrono_bonus(r: &ScoreRuleDef, t: f32) -> Option<i64> {
    let g = |k: &str| r.params.get(k).copied().unwrap_or(0.0);
    for (time, gain) in [("m_fVeryFastTime", "m_VeryFastGain"), ("m_fFastTime", "m_FastGain"), ("m_fNormalTime", "m_NormalGain")] {
        if g(time) > 0.0 && t <= g(time) && g(gain) > 0.0 {
            return Some(g(gain) as i64);
        }
    }
    None
}

/// Oil Drop's tanks shot down (`props::TankEvents`): each worth its points
/// (`ScoringRule_OilRain_Destroy`), more in quick succession (`_Combo`, within `m_fTimeOut`),
/// a bonus each so many (`_BoilingOil`, `m_iRequiredNumberOfTanks`). Their gifts are the
/// scripts' (`<kind>WoTShot`: bent time, healing, Dark Vision), told by the tank's damage
/// (`props::prop_hits`). The tanks missed spoil the wave.
#[allow(clippy::too_many_arguments)]
fn tanks(
    mut ch: ResMut<Challenge>,
    mut sc: ResMut<Scoring>,
    mut events: ResMut<crate::props::TankEvents>,
    level: Option<Res<crate::level::LevelInfo>>,
    stats: Res<PlayerStats>,
) {
    if !ch.active() {
        events.burst.clear();
        events.missed = 0;
        return;
    }
    if ch.started_run() && sc.shots0.is_none() {
        sc.shots0 = Some(stats.shots_fired);
    }
    let set = set_of(&ch, level.as_deref()).cloned();
    let now = ch.elapsed;
    for _ in std::mem::take(&mut events.burst) {
        if ch.ended.is_some() {
            continue;
        }
        sc.tanks += 1;
        if let Some(it) = ch.items.get_mut("DDHI_BatteriesDestroyed") {
            it.value += 1;
        }
        let Some(set) = set.as_ref() else { continue };
        let mut points: Vec<(String, i64)> = Vec::new();
        let mut trick = None;
        for r in &set.rules {
            match r.class.as_str() {
                "DisDLC05ScoringRule_OilRain_Destroy" => points.push((entry_of(r, "Destroy"), r.base_gain as i64)),
                "DisDLC05ScoringRule_OilRain_Combo" => {
                    let window = r.params.get("m_fTimeOut").copied().unwrap_or(1.0);
                    if sc.tank_last.is_some_and(|t| now - t <= window) {
                        sc.tank_combo += 1;
                        let e = entry_of(r, "Combo");
                        points.push((e.clone(), r.base_gain as i64));
                        trick = Some(format!("{e} x{}", sc.tank_combo));
                    } else {
                        sc.tank_combo = 1;
                    }
                }
                "DisDLC05ScoringRule_OilRain_BoilingOil" => {
                    let every = r.params.get("m_iRequiredNumberOfTanks").copied().unwrap_or(20.0).max(1.0) as u32;
                    if sc.tanks % every == 0 {
                        let e = entry_of(r, "Boiling Oil");
                        points.push((e.clone(), r.base_gain as i64));
                        trick = Some(e);
                    }
                }
                _ => {}
            }
        }
        sc.tank_last = Some(now);
        for (e, p) in points {
            ch.score_entry(&e, p);
        }
        if let Some(t) = trick {
            ch.trick(&t);
        }
    }
    if events.missed > 0 {
        if let Some(w) = sc.wave.as_mut() {
            w.1 += events.missed;
        }
        events.missed = 0;
    }
}

/// Oil Drop's waves (`DisSeqAct_DLC05_WobWave`: begun, a tank thrown, ended): a wave whose
/// tanks were all shot down is perfect (`ScoringRule_OilRain_PerfectWave`).
pub fn wob_wave(ch: &mut Challenge, sc: &mut Scoring, set: Option<&RulesetDef>, input: u32) {
    match input {
        0 => sc.wave = Some((0, 0)),
        1 => {
            if let Some(w) = sc.wave.as_mut() {
                w.0 += 1;
            }
        }
        _ => {
            let Some((thrown, missed)) = sc.wave.take() else { return };
            if thrown == 0 || missed > 0 || ch.ended.is_some() {
                return;
            }
            if let Some(r) = set.and_then(|s| rule(s, "DisDLC05ScoringRule_OilRain_PerfectWave").next()) {
                let e = entry_of(r, "Perfect Wave");
                ch.score_entry(&e, r.base_gain as i64);
                ch.trick(&e);
            }
        }
    }
}

/// The run over (not failed): time, health and mana left, the thief's and the mystery foe's
/// totals.
pub fn finish(ch: &mut Challenge, sc: &mut Scoring, set: Option<&RulesetDef>, stats: &PlayerStats, coins: u32, eggs: u32, clues: u32) {
    // (a round cut short earns nothing)
    sc.round = None;
    let Some(set) = set.cloned() else { return };
    let t = ch.elapsed;
    for r in &set.rules {
        match r.class.as_str() {
            // a gain over the time taken
            "DisDLC05ScoringRule_Chrono" => {
                let base = r.params.get("m_fBaseGain").copied().unwrap_or(0.0);
                let off = r.params.get("m_fTimeOffset").copied().unwrap_or(0.0);
                let p = (base / (t + off).max(1.0)) as i64;
                ch.score_entry("Time", p);
            }
            "DisDLC05ScoringRule_ChronoBonus" if r.params.get("m_bChallengeTime").copied().unwrap_or(1.0) > 0.5 => {
                if let Some(p) = chrono_bonus(r, t) {
                    ch.score_entry("Time Bonus", p);
                }
                // (the speed reached, by the rules' names for it: every trial's are these)
                let g = |k: &str| r.params.get(k).copied().unwrap_or(0.0);
                let level = [("m_fVeryFastTime", "Haste"), ("m_fFastTime", "Precipitation"), ("m_fNormalTime", "Arrival")].iter().find(|(k, _)| g(k) > 0.0 && t <= g(k)).map_or("-", |(_, n)| *n);
                ch.stat_params.insert("Speed Level".into(), level.into());
            }
            // the targets shot for the bolts fired (Assassin's Training), so much a percent
            "DisDLC05ScoringRule_Accuracy" => {
                let shots = stats.shots_fired.saturating_sub(sc.shots0.unwrap_or(stats.shots_fired));
                let pct = if shots > 0 { (sc.shot_kills as f32 / shots as f32).min(1.0) * 100.0 } else { 0.0 };
                let e = entry_of(r, "Accuracy");
                ch.stat_params.insert(e.clone(), format!("{pct:.0}%"));
                let p = (pct * r.params.get("m_fGainByPercent").copied().unwrap_or(2.0)).round() as i64;
                if p > 0 {
                    ch.score_entry(&e, p);
                }
            }
            // the tanks shot down for the pistol's shots (Oil Drop), so much a percent
            "DisDLC05ScoringRule_OilRain_Accuracy" => {
                let shots = stats.shots_fired.saturating_sub(sc.shots0.unwrap_or(stats.shots_fired));
                let pct = if shots > 0 { (sc.tanks as f32 / shots as f32).min(1.0) * 100.0 } else { 0.0 };
                let e = entry_of(r, "Accuracy");
                ch.stat_params.insert(e.clone(), format!("{pct:.0}%"));
                let p = (pct * r.params.get("m_fGainByPercent").copied().unwrap_or(2.0)).round() as i64;
                if p > 0 {
                    ch.score_entry(&e, p);
                }
            }
            // so many a percent of health (or mana) left
            "DisDLC05ScoringRule_PlayerStat" => {
                let mana = r.kinds.get("m_StatType").is_some_and(|k| k.contains("Mana"));
                let pct = if mana { stats.mana / stats.max_mana.max(1.0) } else { stats.health / stats.max_health.max(1.0) } * 100.0;
                // (its stat's name: every trial's are these two)
                ch.stat_params.insert(if mana { "Mana Bonus" } else { "Hard to get" }.into(), format!("{pct:.0}%"));
                let p = (pct * r.params.get("m_fGainByPercent").copied().unwrap_or(0.0)) as i64;
                ch.score_entry(if mana { "Mana" } else { "Health" }, p);
            }
            // the thief: coins, eggs in their order, each person's stealth; the eggs' multiplier
            "DisDLC05ScoringRule_Thief" => {
                let coin = r.params.get("m_iCoinGain").copied().unwrap_or(1.0);
                ch.score_entry("Coin", (coins as f32 * coin) as i64);
                let gains = r.lists.get("m_EggGains");
                for i in 0..eggs as usize {
                    let g = gains.and_then(|l| l.get(i).copied().flatten()).unwrap_or(50.0);
                    ch.score_entry(&format!("Egg{}", i + 1), g as i64);
                }
                let levels = r.lists.get("m_AttentionLevels");
                let ko = r.params.get("m_iChockedGain").copied().unwrap_or(0.0);
                let mut stealth = 0.0;
                for (lvl, out, dead) in sc.attention.values() {
                    if *dead {
                        continue;
                    }
                    stealth += if *out { ko } else { levels.and_then(|l| l.get(*lvl as usize).copied().flatten()).unwrap_or(0.0) };
                }
                ch.score_entry("Stealth Bonus", stealth as i64);
                let m = r.lists.get("m_fMilestoneMultipliers").and_then(|l| l.get(eggs as usize).copied().flatten()).unwrap_or(1.0);
                if m > 0.0 && (m - 1.0).abs() > 1e-3 {
                    let bonus = (ch.score as f32 * (m - 1.0)) as i64;
                    ch.score_entry("Eggs Multiplier", bonus);
                }
            }
            _ => {}
        }
    }
    let _ = clues;
}

/// The mystery foe killed (`ECE_MysteryMan_TargetKilled`): the target, the clues, unseen, the
/// people's stealth, quickly; the time's and clues' multipliers.
pub fn foe_killed(ch: &mut Challenge, sc: &mut Scoring, set: Option<&RulesetDef>, stats: &PlayerStats, clues: u32) {
    let Some(set) = set.cloned() else { return };
    for r in rule(&set, "DisDLC05ScoringRule_MysteryMan") {
        let g = |k: &str| r.params.get(k).copied().unwrap_or(0.0);
        ch.score_entry("Target", g("m_iTargetGain") as i64);
        ch.score_entry("Clue", (clues as f32 * g("m_iClueGain")) as i64);
        if clues < g("m_iNumClues").max(1.0) as u32 {
            ch.score_entry("Lucky Boy", g("m_iLuckyBoyGain") as i64);
        }
        if stats.times_detected == ch.base_detected() {
            ch.score_entry("Ghost", g("m_iGhostGain") as i64);
        }
        let levels = r.lists.get("m_AttentionLevels");
        let mut stealth = 0.0;
        for (lvl, out, dead) in sc.attention.values() {
            if *dead {
                continue;
            }
            stealth += if *out { g("m_iAsleepGain") } else { levels.and_then(|l| l.get(*lvl as usize).copied().flatten()).unwrap_or(0.0) };
        }
        ch.score_entry("Stealth Bonus", stealth as i64);
        // (the minutes taken: their multiplier)
        let minute = (ch.elapsed / 60.0) as usize;
        if let Some(m) = r.lists.get("m_fTimeMultipliers").and_then(|l| l.get(minute.min(l.len().saturating_sub(1))).copied().flatten()) {
            if minute < 3 {
                ch.score_entry("Quickly Done", g("m_iQuicklyDoneGain") as i64);
            }
            let bonus = (ch.score as f32 * (m - 1.0)) as i64;
            if bonus != 0 {
                ch.score_entry("Time Multiplier", bonus);
            }
        }
    }
}
