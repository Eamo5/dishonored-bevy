//! Dunwall City Trials (DLC05): a challenge's run. The challenge menu launches a map
//! (`DisDLC05GameInfo.m_Challenges`, cooked into the game data); once it is up the game plays
//! its opening (the matinee the level scripts never start themselves) and raises
//! `DisSeqEvent_DLC05_Challenge` "Started". The scripts then run the challenge: they ask for
//! timers (`DisSeqAct_DLC05_Timer`), the HUD's counters (`DisSeqAct_DLC05_ShowHUDItem`), wave
//! titles, countdowns and phase results, scoring rules (`DisSeqAct_DLC05_SetScoringRules`), and
//! send the challenge's events (`DisSeqAct_DLC05_SendChallengeEvent`: `ECE_Challenge_End`,
//! `_Failed`, `_Backup` / `_Restore` for a retry, `_Pause` / `_Resume`). A death goes to the
//! scripts (`DisSeqEvent_DLC05_PlayerDeath`) rather than to the game over menu; they resurrect
//! Corvo (`DisSeqAct_DLC05_PlayerResurrect`) or end the run. At the end, the results: the score
//! against the challenge's medals, the best kept with the profile (`dlc05.json`, with the
//! clockwork dolls found).

use crate::gameplay::PlayerStats;
use crate::GameState;
use bevy::prelude::*;
use dhcook::format::ChallengeDef;
use std::collections::{BTreeMap, BTreeSet, HashMap};

pub struct ChallengePlugin;

impl Plugin for ChallengePlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Challenge>()
            .init_resource::<ChallengeLaunch>()
            .insert_resource(ChallengeProfile::load())
            .add_systems(OnEnter(GameState::InGame), begin.after(crate::level::LevelSpawnSet))
            .add_systems(Update, (start, apply, tick, deaths).chain().in_set(ChallengeSet).run_if(in_state(GameState::InGame)))
            .add_systems(Update, hud.run_if(in_state(GameState::InGame)));
    }
}

/// The run's systems (the briefing goes before).
#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub struct ChallengeSet;

/// How the challenge menu launched the run.
#[derive(Resource, Default)]
pub struct ChallengeLaunch {
    pub expert: bool,
    /// back from a run to the challenges (the results' "Exit Challenge"), the one run
    pub back_to_challenges: bool,
    pub last: Option<usize>,
    /// the pause menu's "End Challenge"
    pub end_now: bool,
}

/// A challenge timer's settings (`DisSeqAct_DLC05_Timer`).
#[derive(Clone, Debug)]
pub struct TimerParams {
    pub initial: f32,
    /// it completes there (`m_bUseTargetTime`, `m_fTargetTime`)
    pub target: Option<f32>,
    pub increment: bool,
    pub reset_on_stop: bool,
    /// back to its start at each kill (`m_bAutoResetOnKill`: the kill chain's)
    pub reset_on_kill: bool,
    /// `DDHT_DefaultTimer`, `DDHT_CountdownTimer`, `DDHT_KillChainTimer`
    pub kind: String,
}

/// What the scripts gave before an unlock was shown (`DisSeqAct_DLC05_ShowEquipmentUnlock`
/// lists them): a power at a level (`DisSeqAct_AddPower`), an upgrade (`DisSeqAct_GiveUpgrade`).
#[derive(Clone, Debug)]
pub enum Grant {
    Power(String, u8),
    Upgrade(String),
}

/// What the level scripts ask of the challenge.
#[derive(Clone, Debug)]
pub enum ChallengeFx {
    /// `EDisChallengeEvent`
    Event(String),
    Rules(String),
    CustomRule(String),
    Timer { op: u32, input: u32, params: TimerParams, modifier: f32 },
    /// a HUD counter: Show (0), Hide (1), Reset (2)
    HudItem { item: String, input: u32, initial: Option<i32>, max: Option<i32> },
    Wave { number: Option<i32>, text: Option<String> },
    Countdown { op: u32, go: bool },
    PhaseResults { op: u32, name: String, last: bool, show_possible: bool, bonus_next: bool, possible: i32, required: i32, effective: i32 },
    EquipmentUnlock(Vec<Grant>),
    Difficulty(String),
    Resurrect,
    Heal(f32),
    InfiniteAmmo(bool),
    Doll,
    WaveBendTime { op: u32, secs: f32 },
    Text(Option<String>),
    StopAllSounds,
    /// Oil Rain's tank waves (`DisSeqAct_DLC05_WobWave`: begin, a tank, end)
    WobWave(u32),
    /// the mystery foe's portrait, and its side (`DisSeqAct_DLC05_SetMysteryFoe`)
    MysteryFoe { portrait: String, blue: bool },
}

/// What the HUD is to show (`dlc05hud.rs`; the counters and timers it reads itself).
#[derive(Clone, Debug)]
pub enum HudEvent {
    /// points scored
    Scored(i64),
    /// a round's title: its number, or a title of the scripts'
    Wave { number: Option<i32>, text: Option<String> },
    /// the count before a start (3, 2, 1, and GO! or not)
    CountdownStart { go: bool },
    /// a scoring's flair named (`DLC05_H_Tricks.UpdateTrick`)
    Trick(String),
    /// a clockwork egg found, of so many (`DLC05_H_EggDiscovery.Update`)
    EggFound { found: i32, max: i32 },
    /// what was unlocked (`DLC05_H_EquipmentUnlocked.ShowEquipmentUnlocked`)
    EquipmentUnlock(Vec<Grant>),
    /// a round's results (`DLC05_H_PhaseResultsScreen.Show`)
    PhaseResults { name: String, success: bool, possible: Option<i32>, goal: i32, kills: i32, bonus: bool, last: bool },
}

struct RunTimer {
    params: TimerParams,
    value: f32,
    running: bool,
}

/// A run's state at a checkpoint (`ECE_Challenge_Backup`), for a retry (`_Restore`).
#[derive(Clone, Default)]
struct Backup {
    score: i64,
    kills: u32,
    items: BTreeMap<String, HudItem>,
}

/// A counter on the challenge's HUD (`DDHI_Kills`, `DDHI_EnemiesLeft`...).
#[derive(Clone, Debug, Default)]
pub struct HudItem {
    pub value: i32,
    pub max: Option<i32>,
    pub shown: bool,
}

/// The run.
#[derive(Resource, Default)]
pub struct Challenge {
    pub def: Option<ChallengeDef>,
    pub map: String,
    pub expert: bool,
    started: bool,
    since: f32,
    pub score: i64,
    pub kills: u32,
    /// the scoring rule set in force, and what scored (entry, points)
    pub rules: Option<String>,
    pub history: Vec<(String, i64)>,
    timers: HashMap<u32, RunTimer>,
    pub items: BTreeMap<String, HudItem>,
    /// a title in the middle (wave, phase): text, seconds left
    pub banner: Option<(String, f32)>,
    /// the count before a start: op, seconds left, whether "GO!" ends it
    countdown: Option<(u32, f32, bool)>,
    pub text: Option<String>,
    /// over (failed or not), and the medal won (0 none .. 3 gold)
    pub ended: Option<bool>,
    pub medal: usize,
    pub best: i64,
    paused: bool,
    was_dead: bool,
    last_kills: u32,
    bend: Vec<(u32, f32)>,
    infinite_ammo: Option<[u32; 3]>,
    backup: Option<Backup>,
    /// the coins found and times seen when the run began (the counters the game keeps itself)
    base: (u32, u32),
    pub hud_events: Vec<HudEvent>,
    /// the briefing read (`dlc05brief.rs`): the run may begin
    pub briefed: bool,
    /// a round's results waiting on the player (the op), and their choice: on (true), or
    /// end the challenge
    pub phase_op: Option<u32>,
    pub phase_choice: Option<bool>,
    /// the combos' multiplier in force, the drops' heights, the elixirs held (the scoring's)
    pub multiplier: f32,
    pub drop_heights: Vec<f32>,
    pub elixirs_now: u32,
    /// the mystery foe's portrait (`UI_MysteryManTargets_DLC05`)
    pub foe: Option<String>,
    /// the mystery foe's side (blue), and whether it's down
    pub foe_blue: bool,
    pub foe_down: bool,
    /// the results' figures the rules leave (`DDSL_Custom_DisplayStatParameter`: accuracy,
    /// health and mana left; `_ChronoBonus`: the speed reached), by stat name
    pub stat_params: BTreeMap<String, String>,
    /// the kill chain's kills, and its best (`DDSL_Custom_ChainKillBestChain`)
    pub chain: u32,
    pub best_chain: u32,
    /// what the run unlocked (`R_ResultsScreen_Unlocks`): an artwork or a challenge's expert
    /// mode, its name, its picture (folder, name)
    pub unlocks: Vec<Unlock>,
    /// "Started" waits on the scripts' setting the scoring (`DisSeqAct_DLC05_SetScoringRules`)
    await_rules: bool,
    /// the times seen when the thief's watch last looked
    busted_seen: u32,
    /// the run's time (started, not over, not paused), the best score before it, the last
    /// title the scripts showed (a failure's words), what they stored
    /// (`EDisDLC05StatStorageEvent`)
    pub elapsed: f32,
    pub prev_best: i64,
    pub last_title: Option<String>,
    pub storage: HashMap<String, i64>,
}

impl Challenge {
    pub fn active(&self) -> bool {
        self.def.is_some()
    }

    /// A kind of timer's time (`DDHT_DefaultTimer`...): the one running, else any.
    pub fn timer_of(&self, kind: &str) -> Option<f32> {
        let of = || self.timers.values().filter(|t| t.params.kind == kind);
        of().find(|t| t.running).or_else(|| of().next()).map(|t| t.value)
    }

    /// A kind of timer that starts somewhere (not a count from nothing): its time, its start,
    /// whether it runs.
    pub fn timer_state(&self, kind: &str) -> Option<(f32, f32, bool)> {
        let of = || self.timers.values().filter(|t| t.params.kind == kind && t.params.initial > 0.0);
        of().find(|t| t.running).or_else(|| of().next()).map(|t| (t.value, t.params.initial, t.running))
    }

    /// Under way (its opening played, "Started" raised).
    pub fn started_run(&self) -> bool {
        self.started
    }

    /// The times seen when the run began.
    pub fn base_detected(&self) -> u32 {
        self.base.1
    }

    /// A scoring's flair shown by name (a modifier, a bonus, a combo).
    pub(crate) fn trick(&mut self, name: &str) {
        self.hud_events.push(HudEvent::Trick(name.to_string()));
    }

    pub(crate) fn score_entry(&mut self, entry: &str, points: i64) {
        self.score += points;
        self.history.push((entry.to_string(), points));
        if points != 0 {
            self.hud_events.push(HudEvent::Scored(points));
        }
        if std::env::var("DH_CHALLENGE_LOG").is_ok() {
            info!("challenge: {entry} +{points} = {}", self.score);
        }
    }
}

/// Something a run unlocked: a piece of the gallery (else a challenge's expert mode), its
/// name, its picture (a cooked folder's, by name).
#[derive(Clone, Debug)]
pub struct Unlock {
    pub artwork: bool,
    pub name: String,
    pub picture: (String, String),
}

/// The profile's challenge records: best scores (by challenge id), the dolls found (by map),
/// the gallery's pieces seen (by id: the others unlocked are new).
#[derive(Resource, Default, serde::Serialize, serde::Deserialize)]
pub struct ChallengeProfile {
    pub best: BTreeMap<String, i64>,
    pub dolls: BTreeSet<String>,
    #[serde(default)]
    pub gallery_seen: BTreeSet<String>,
    /// the welcome shown once (`m_bShowingWelcomeDisclaimer`, `OnWelcomeDisclaimerClosed`)
    #[serde(default)]
    pub welcome_seen: bool,
    /// each challenge's (and mode's) best runs, best first, and the last one's number: the
    /// local leaderboards (the original's were online, `req_DLC05_*Leaderboards`)
    #[serde(default)]
    pub runs: BTreeMap<String, Vec<Run>>,
    #[serde(default)]
    pub last_run: BTreeMap<String, u64>,
}

/// A finished run kept on a challenge's local leaderboard: its score, when (Unix seconds),
/// its number among the challenge's runs.
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct Run {
    pub score: i64,
    pub at: u64,
    #[serde(default)]
    pub n: u64,
}

/// The runs a local leaderboard keeps (`InitList(listContent, 1, 9)`: a page of nine).
pub const BOARD_RUNS: usize = 9;

impl ChallengeProfile {
    fn path() -> std::path::PathBuf {
        crate::settings::user_dir().join("dlc05.json")
    }
    fn load() -> Self {
        std::fs::read(Self::path()).ok().and_then(|d| serde_json::from_slice(&d).ok()).unwrap_or_default()
    }
    pub(crate) fn save(&self) {
        if let Ok(d) = serde_json::to_vec_pretty(self) {
            let _ = std::fs::write(Self::path(), d);
        }
    }

    /// A finished run onto its challenge's local leaderboard (`best_key`): the best kept.
    pub fn record_run(&mut self, key: &str, score: i64) {
        let at = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
        let n = self.last_run.get(key).copied().unwrap_or(0) + 1;
        let v = self.runs.entry(key.to_string()).or_default();
        v.push(Run { score, at, n });
        v.sort_by(|a, b| b.score.cmp(&a.score).then(a.n.cmp(&b.n)));
        v.truncate(BOARD_RUNS);
        self.last_run.insert(key.to_string(), n);
    }

    /// A challenge's local leaderboard: its runs, best first (a best from before there were
    /// boards, alone), and the last run's place on it.
    pub fn board(&self, key: &str) -> (Vec<Run>, Option<usize>) {
        let runs = match self.runs.get(key) {
            Some(v) if !v.is_empty() => v.clone(),
            _ => self.best.get(key).map(|b| vec![Run { score: *b, at: 0, n: 0 }]).unwrap_or_default(),
        };
        let last = self.last_run.get(key).and_then(|n| runs.iter().position(|r| r.n == *n && *n > 0));
        (runs, last)
    }

    /// A challenge's stars won, in a mode.
    pub fn stars(&self, c: &ChallengeDef, expert: bool) -> i32 {
        let medals = if expert { c.expert_medals } else { c.medals };
        let best = self.best.get(&best_key(&c.id, expert)).copied().unwrap_or(0);
        medals.iter().filter(|m| **m > 0 && best >= **m as i64).count() as i32
    }

    /// Whether the gallery's piece is unlocked (`DisDLC05GalleryItem`): its stars won in its
    /// challenge, in a mode it opens in; or every other piece of its mode's.
    pub fn gallery_unlocked(&self, data: &dhcook::format::GameData, i: usize) -> bool {
        let (gallery, challenges) = (&data.gallery, &data.challenges);
        let Some(g) = gallery.get(i) else { return false };
        if g.all_normal || g.all_expert {
            let expert = g.all_expert;
            return gallery.iter().enumerate().filter(|(j, o)| *j != i && !o.all_normal && !o.all_expert && if expert { o.expert } else { o.normal }).all(|(j, _)| self.gallery_unlocked(data, j));
        }
        let Some(c) = challenges.iter().find(|c| c.leaderboard == g.challenge) else { return false };
        (g.normal && self.stars(c, false) >= g.stars) || (g.expert && self.stars(c, true) >= g.stars)
    }
}

/// The map that came up: a challenge's, or not.
fn begin(mut ch: ResMut<Challenge>, level: Option<Res<crate::level::LevelInfo>>, data: Res<crate::gamedata::Data>, mut launch: ResMut<ChallengeLaunch>, profile: Res<ChallengeProfile>, mut scoring: ResMut<crate::dlc05score::Scoring>) {
    scoring.reset();
    let map = level.map(|l| l.scene.name.clone()).unwrap_or_default();
    let found = data.0.challenges.iter().position(|c| c.map.eq_ignore_ascii_case(&map));
    let def = found.map(|i| data.0.challenges[i].clone());
    *ch = Challenge::default();
    if let Some(d) = def {
        info!("challenge: {} ({}){}", d.name, d.id, if launch.expert { ", expert" } else { "" });
        launch.last = found;
        ch.best = profile.best.get(&best_key(&d.id, launch.expert)).copied().unwrap_or(0);
        ch.prev_best = ch.best;
        ch.def = Some(d);
        ch.map = map;
        ch.expert = launch.expert;
    }
}

/// Once the level is up: the opening, and "Started".
fn start(mut ch: ResMut<Challenge>, vm: Option<ResMut<crate::kismet::Vm>>, warm: Option<Res<crate::warmup::Warmup>>, time: Res<Time>, profile: Res<ChallengeProfile>, stats: Res<PlayerStats>) {
    if !ch.active() || ch.started || !ch.briefed {
        return;
    }
    let Some(mut vm) = vm else { return };
    if warm.is_some() {
        return;
    }
    ch.since += time.delta_secs();
    if ch.since < 0.5 {
        return;
    }
    ch.started = true;
    ch.base = (stats.coins_found, stats.times_detected);
    vm.expert = ch.expert;
    vm.doll_found = profile.dolls.contains(&ch.map.to_ascii_lowercase());
    // "Started": when the scripts set its scoring (the run begins: after Drop Attack's fly-through
    // and countdown, its timer started and Corvo let go), now if they did already or never will
    ch.await_rules = ch.rules.is_none() && vm.has_op_class("DisSeqAct_DLC05_SetScoringRules");
    if !ch.await_rules {
        vm.challenge_event(0);
    }
    let n = vm.challenge_intro();
    info!("challenge: started ({n} opening matinees)");
}

/// The scripts' requests.
#[allow(clippy::too_many_arguments)]
fn apply(
    mut ch: ResMut<Challenge>,
    vm: Option<ResMut<crate::kismet::Vm>>,
    mut stats: ResMut<PlayerStats>,
    mut profile: ResMut<ChallengeProfile>,
    level: Option<Res<crate::level::LevelInfo>>,
    mut scoring: ResMut<crate::dlc05score::Scoring>,
    data: Res<crate::gamedata::Data>,
) {
    let rules_of: &[dhcook::format::RulesetDef] = level.as_ref().map(|l| l.scene.challenge_rules.as_slice()).unwrap_or(&[]);
    let Some(mut vm) = vm else { return };
    if vm.challenge_fx.is_empty() {
        return;
    }
    let log = std::env::var("DH_CHALLENGE_LOG").is_ok();
    for fx in std::mem::take(&mut vm.challenge_fx) {
        if log {
            info!("challenge: {fx:?}");
        }
        match fx {
            ChallengeFx::Event(e) => match e.as_str() {
                "ECE_Challenge_End" | "ECE_Challenge_Failed" => {
                    if ch.ended.is_some() {
                        continue;
                    }
                    let failed = e == "ECE_Challenge_Failed";
                    // (the run's last scorings: time, what's left, the loot)
                    if !failed {
                        let set = crate::dlc05score::set_of(&ch, level.as_deref()).cloned();
                        let coins = stats.coins_found.saturating_sub(ch.base.0);
                        let eggs = stats.items.keys().filter(|k| k.contains("Egg_AII")).count() as u32;
                        let clues = stats.items.keys().filter(|k| k.contains("Clue")).count() as u32;
                        crate::dlc05score::finish(&mut ch, &mut scoring, set.as_ref(), &stats, coins, eggs, clues);
                    }
                    ch.ended = Some(failed);
                    let def = ch.def.clone().unwrap_or_default();
                    let medals = if ch.expert && def.expert_medals.iter().any(|m| *m > 0) { def.expert_medals } else { def.medals };
                    ch.medal = if failed { 0 } else { medals.iter().filter(|m| **m > 0 && ch.score >= **m as i64).count() };
                    // (the run onto the local leaderboard: one that scored)
                    if !failed && ch.score > 0 && !def.id.is_empty() {
                        profile.record_run(&best_key(&def.id, ch.expert), ch.score);
                        profile.save();
                    }
                    if !failed && ch.score > ch.best {
                        // (what was locked before: the gallery's pieces, the expert mode)
                        let gallery_before: Vec<bool> = (0..data.0.gallery.len()).map(|i| profile.gallery_unlocked(&data.0, i)).collect();
                        let expert_before = profile.stars(&def, false) >= 2;
                        ch.best = ch.score;
                        profile.best.insert(best_key(&def.id, ch.expert), ch.score);
                        profile.save();
                        if !ch.expert && !expert_before && profile.stars(&def, false) >= 2 && def.expert_medals.iter().any(|m| *m > 0) {
                            ch.unlocks.push(Unlock { artwork: false, name: def.name.clone(), picture: ("dlc05".into(), format!("ChallengeImg_{}_Small", def.id)) });
                        }
                        for (i, was) in gallery_before.into_iter().enumerate() {
                            if was || !profile.gallery_unlocked(&data.0, i) {
                                continue;
                            }
                            let g = &data.0.gallery[i];
                            // (its name: its challenge's, the Outsider's for a mode's last)
                            let name = data.0.challenges.iter().find(|c| c.leaderboard == g.challenge).map_or_else(|| "The Outsider".to_string(), |c| c.name.clone());
                            ch.unlocks.push(Unlock { artwork: true, name, picture: ("dlc05gallery".into(), format!("UI_{}_S", g.id)) });
                        }
                    }
                    // (tests: `DH_TEST_UNLOCK` shows the first piece of the gallery as unlocked)
                    if !failed && ch.unlocks.is_empty() && std::env::var("DH_TEST_UNLOCK").is_ok() {
                        if let Some(g) = data.0.gallery.first() {
                            ch.unlocks.push(Unlock { artwork: true, name: def.name.clone(), picture: ("dlc05gallery".into(), format!("UI_{}_S", g.id)) });
                        }
                        ch.unlocks.push(Unlock { artwork: false, name: def.name.clone(), picture: ("dlc05".into(), format!("ChallengeImg_{}_Small", def.id)) });
                    }
                    info!("challenge: {} with {} points (medal {})", if failed { "failed" } else { "over" }, ch.score, ch.medal);
                    vm.challenge_event(if failed { 2 } else { 1 });
                }
                "ECE_Challenge_Pause" => ch.paused = true,
                // the rounds (their bonuses), the time markers (`ScoringRule_ChronoBonus`), the
                // mystery foe down
                "ECE_Challenge_BeginRound" => {
                    let set = crate::dlc05score::set_of(&ch, level.as_deref()).cloned();
                    crate::dlc05score::round_begins(&mut ch, &mut scoring, set.as_ref(), &stats);
                }
                "ECE_Challenge_EndRound" => {
                    let set = crate::dlc05score::set_of(&ch, level.as_deref()).cloned();
                    crate::dlc05score::round_ends(&mut ch, &mut scoring, set.as_ref());
                }
                "ECE_Challenge_TimeMarker" => {
                    let set = crate::dlc05score::set_of(&ch, level.as_deref()).cloned();
                    crate::dlc05score::time_marker(&mut ch, &mut scoring, set.as_ref());
                }
                "ECE_MysteryMan_TargetKilled" => {
                    ch.foe_down = true;
                    let set = crate::dlc05score::set_of(&ch, level.as_deref()).cloned();
                    let clues = stats.items.keys().filter(|k| k.contains("Clue")).count() as u32;
                    crate::dlc05score::foe_killed(&mut ch, &mut scoring, set.as_ref(), &stats, clues);
                }
                "ECE_Challenge_Resume" => ch.paused = false,
                "ECE_Challenge_Backup" => {
                    ch.backup = Some(Backup { score: ch.score, kills: ch.kills, items: ch.items.clone() });
                }
                "ECE_Challenge_Restore" => {
                    if let Some(b) = ch.backup.clone() {
                        ch.score = b.score;
                        ch.kills = b.kills;
                        ch.items = b.items;
                    }
                    vm.challenge_event(3);
                }
                _ => {}
            },
            ChallengeFx::Rules(t) => {
                ch.rules = Some(t);
                if std::mem::take(&mut ch.await_rules) {
                    vm.challenge_event(0);
                }
            }
            ChallengeFx::CustomRule(r) => {
                let points = rule_points(rules_of, ch.rules.as_deref(), &r).unwrap_or(0);
                ch.score_entry(&r, points);
            }
            ChallengeFx::Timer { op, input, params, modifier } => {
                let t = ch.timers.entry(op).or_insert_with(|| RunTimer { value: params.initial, params: params.clone(), running: false });
                match input {
                    0 => {
                        t.params = params;
                        t.value = t.params.initial;
                        t.running = true;
                    }
                    1 => {
                        t.running = false;
                        if t.params.reset_on_stop {
                            t.value = t.params.initial;
                        }
                    }
                    2 => t.value += modifier,
                    3 => t.running = false,
                    _ => t.running = true,
                }
            }
            // (a clockwork egg found: the HUD's one-shot, gone on its own; the scripts hide it
            // as soon as shown)
            ChallengeFx::HudItem { item, input, max, .. } if item == "DDHI_EggDiscovery" => {
                if input == 0 {
                    let found = stats.items.keys().filter(|k| k.contains("Egg_AII")).count() as i32;
                    ch.hud_events.push(HudEvent::EggFound { found, max: max.unwrap_or(found) });
                }
            }
            ChallengeFx::HudItem { item, input, initial, max } => {
                let it = ch.items.entry(item).or_default();
                match input {
                    0 => {
                        if let Some(i) = initial {
                            it.value = i;
                        }
                        it.max = max.or(it.max);
                        it.shown = true;
                    }
                    1 => it.shown = false,
                    _ => it.value = initial.unwrap_or(0),
                }
            }
            ChallengeFx::Wave { number, text } => {
                if text.is_some() {
                    ch.last_title = text.clone();
                }
                // (a numbered round's title: a round begins, the last judged)
                if number.is_some() && text.is_none() {
                    let set = crate::dlc05score::set_of(&ch, level.as_deref()).cloned();
                    crate::dlc05score::round_begins(&mut ch, &mut scoring, set.as_ref(), &stats);
                }
                ch.hud_events.push(HudEvent::Wave { number, text });
            }
            ChallengeFx::Countdown { op, go } => {
                ch.countdown = Some((op, 3.0 + if go { 0.8 } else { 0.0 }, go));
                ch.hud_events.push(HudEvent::CountdownStart { go });
            }
            ChallengeFx::PhaseResults { op, name, last, show_possible, bonus_next, possible, required, effective } => {
                ch.phase_op = Some(op);
                ch.phase_choice = None;
                ch.hud_events.push(HudEvent::PhaseResults { name, success: effective >= required, possible: show_possible.then_some(possible), goal: required, kills: effective, bonus: bonus_next, last });
            }
            ChallengeFx::EquipmentUnlock(list) => ch.hud_events.push(HudEvent::EquipmentUnlock(list)),
            ChallengeFx::Difficulty(_) => {}
            ChallengeFx::Resurrect => {
                stats.dead = false;
                stats.game_over = None;
                stats.health = stats.max_health;
                stats.mana = stats.max_mana;
                ch.was_dead = false;
            }
            ChallengeFx::Heal(pct) => stats.health = stats.health.max(stats.max_health * pct / 100.0),
            ChallengeFx::InfiniteAmmo(on) => ch.infinite_ammo = on.then_some([stats.bolts, stats.bullets, stats.sleep_darts]),
            ChallengeFx::Doll => {
                profile.dolls.insert(ch.map.to_ascii_lowercase());
                profile.save();
            }
            ChallengeFx::WaveBendTime { op, secs } => ch.bend.push((op, secs)),
            ChallengeFx::Text(t) => ch.text = t,
            ChallengeFx::StopAllSounds => {}
            ChallengeFx::WobWave(i) => {
                let set = crate::dlc05score::set_of(&ch, level.as_deref()).cloned();
                crate::dlc05score::wob_wave(&mut ch, &mut scoring, set.as_ref(), i);
            }
            ChallengeFx::MysteryFoe { portrait, blue } => {
                ch.foe = Some(portrait);
                ch.foe_blue = blue;
            }
        }
    }
}

/// Where a challenge's best score is kept: by its id, its expert mode's apart.
pub fn best_key(id: &str, expert: bool) -> String {
    if expert {
        format!("{id}_Expert")
    } else {
        id.to_string()
    }
}

/// A scoring rule's base gain, by its entry name, from the rule set in force.
fn rule_points(sets: &[dhcook::format::RulesetDef], rules: Option<&str>, entry: &str) -> Option<i64> {
    let set = sets.iter().find(|r| Some(r.name.as_str()) == rules)?;
    set.rules.iter().find(|r| r.entry.eq_ignore_ascii_case(entry) || r.name.eq_ignore_ascii_case(entry)).map(|r| r.base_gain as i64)
}

/// Timers, the countdown, titles; kills scored.
#[allow(clippy::too_many_arguments)]
fn tick(mut ch: ResMut<Challenge>, vm: Option<ResMut<crate::kismet::Vm>>, time: Res<Time>, mut stats: ResMut<PlayerStats>, mut launch: ResMut<ChallengeLaunch>, attrs: Res<crate::gamedata::Attrs>) {
    if !ch.active() {
        return;
    }
    let Some(mut vm) = vm else { return };
    // the ammunition for the scripts (`DisSeqAct_DLC05_GetAmmoInfo`)
    for ty in 0..8u8 {
        vm.ammo[ty as usize] = (crate::gadgets::ammo_count(&stats, ty), attrs.ammo_capacity.get(ty as usize).copied().unwrap_or(0));
    }
    // the thief seen: "Busted" (`DisSeqAct_DLC05_PlayerBusted`)
    if stats.times_detected > ch.busted_seen {
        if let Some(op) = vm.busted {
            vm.signal(op, 2);
        }
    }
    ch.busted_seen = stats.times_detected;
    // (ended from the pause menu: as the scripts end it)
    if std::mem::take(&mut launch.end_now) && ch.ended.is_none() {
        vm.challenge_fx.push(ChallengeFx::Event("ECE_Challenge_End".into()));
    }
    // a round's results chosen: on (the op goes out), or the end
    if let Some(go_on) = ch.phase_choice.take() {
        if let Some(op) = ch.phase_op.take() {
            if go_on {
                vm.signal(op, 0);
            } else {
                vm.challenge_fx.push(ChallengeFx::Event("ECE_Challenge_End".into()));
            }
        }
    }
    let dt = time.delta_secs();
    if !ch.paused && ch.ended.is_none() {
        if ch.started {
            ch.elapsed += dt;
        }
        let mut done = Vec::new();
        for (&op, t) in ch.timers.iter_mut().filter(|(_, t)| t.running) {
            t.value += if t.params.increment { dt } else { -dt };
            vm.write_var(op, "Time", crate::kismet::Val::Float(t.value));
            if let Some(target) = t.params.target {
                if (t.params.increment && t.value >= target) || (!t.params.increment && t.value <= target) {
                    t.running = false;
                    done.push(op);
                }
            }
        }
        for op in done {
            vm.signal(op, 2);
        }
    }
    if let Some((op, left, go)) = ch.countdown.as_mut() {
        *left -= dt;
        if *left <= 0.0 {
            let (op, _go) = (*op, *go);
            ch.countdown = None;
            vm.signal(op, 1);
        }
    }
    if let Some((_, left)) = ch.banner.as_mut() {
        *left -= dt;
        if *left <= 0.0 {
            ch.banner = None;
        }
    }
    let mut bend = std::mem::take(&mut ch.bend);
    bend.retain_mut(|(op, left)| {
        *left -= dt;
        if *left <= 0.0 {
            vm.bend_time.push(None);
            vm.signal(*op, 2);
            false
        } else {
            true
        }
    });
    ch.bend = bend;
    if let Some([b, u, s]) = ch.infinite_ammo {
        stats.bolts = stats.bolts.max(b);
        stats.bullets = stats.bullets.max(u);
        stats.sleep_darts = stats.sleep_darts.max(s);
    }
    ch.elixirs_now = stats.health_elixirs + stats.mana_elixirs;
    // the kills Corvo makes count (their scoring: `dlc05score`)
    if stats.kills > ch.last_kills && ch.ended.is_none() {
        let n = stats.kills - ch.last_kills;
        // (the kill chain's timer begins again, its chain one longer)
        let chained = ch.timers.values().any(|t| t.running && t.params.reset_on_kill);
        ch.chain = if chained { ch.chain + n } else { n };
        ch.best_chain = ch.best_chain.max(ch.chain);
        for t in ch.timers.values_mut().filter(|t| t.running && t.params.reset_on_kill) {
            t.value = t.params.initial;
        }
        for _ in 0..n {
            ch.kills += 1;
            if let Some(it) = ch.items.get_mut("DDHI_Kills") {
                it.value += 1;
            }
            if let Some(it) = ch.items.get_mut("DDHI_EnemiesLeft") {
                it.value = (it.value - 1).max(0);
            }
        }
    }
    ch.last_kills = stats.kills;
    // the counters the game keeps itself (`EDisDLC05HUDItemCount`): coins stolen, clockwork
    // eggs found, times seen
    let (coins0, seen0) = ch.base;
    let eggs = stats.items.keys().filter(|k| k.contains("Egg_AII")).count() as i32;
    for (item, v) in [("DDHI_CoinCount", stats.coins_found.saturating_sub(coins0) as i32), ("DDHI_EggCount", eggs), ("DDHI_EggDiscovery", eggs), ("DDHI_BustedCount", stats.times_detected.saturating_sub(seen0) as i32)] {
        if let Some(it) = ch.items.get_mut(item) {
            it.value = v;
        }
    }
}

/// A death goes to the scripts; with none listening, the run is over.
fn deaths(mut ch: ResMut<Challenge>, vm: Option<ResMut<crate::kismet::Vm>>, stats: Res<PlayerStats>) {
    if !ch.active() || !ch.started {
        return;
    }
    let Some(mut vm) = vm else { return };
    if stats.dead && !ch.was_dead && ch.ended.is_none() {
        ch.was_dead = true;
        if !vm.challenge_death() {
            vm.challenge_fx.push(ChallengeFx::Event("ECE_Challenge_Failed".into()));
        }
    }
    if !stats.dead {
        ch.was_dead = false;
    }
}

#[derive(Component)]
struct ChallengeHud;

#[derive(Component)]
enum HudText {
    Side,
    Banner,
}

/// What of the challenge the HUD movie (`dlc05hud.rs`) doesn't show yet: phase results and
/// unlocks as titles, the scripts' drawn text.
fn hud(mut commands: Commands, ch: Res<Challenge>, roots: Query<Entity, With<ChallengeHud>>, mut texts: Query<(&HudText, &mut Text, &mut Visibility)>) {
    if !ch.active() {
        for e in &roots {
            commands.entity(e).despawn();
        }
        return;
    }
    if roots.is_empty() {
        let gold = Color::srgb(0.86, 0.78, 0.6);
        let font = |px: f32| TextFont { font_size: bevy::text::FontSize::Px(px), ..default() };
        commands
            .spawn((ChallengeHud, Node { position_type: PositionType::Absolute, width: percent(100), height: percent(100), ..default() }, GlobalZIndex(40), Pickable::IGNORE, DespawnOnExit(GameState::InGame)))
            .with_children(|r| {
                r.spawn((HudText::Side, Text::new(""), font(22.0), TextColor(Color::WHITE), TextLayout::justify(Justify::Center), Node { position_type: PositionType::Absolute, top: percent(62), width: percent(100), ..default() }, Visibility::Inherited, Pickable::IGNORE));
                r.spawn((HudText::Banner, Text::new(""), font(44.0), TextColor(gold), TextLayout::justify(Justify::Center), Node { position_type: PositionType::Absolute, top: percent(30), width: percent(100), ..default() }, Visibility::Hidden, Pickable::IGNORE));
            });
        return;
    }
    for (kind, mut text, mut vis) in &mut texts {
        let (t, show) = match kind {
            HudText::Side => {
                // (the counters the movie has no element for)
                let mut lines: Vec<String> = Vec::new();
                for (name, it) in ch.items.iter().filter(|(n, it)| it.shown && n.as_str() == "DDHI_EggDiscovery") {
                    lines.push(format!("{}  {} / {}", item_label(name), it.value, it.max.unwrap_or(0)));
                }
                if let Some(t) = &ch.text {
                    lines.push(t.clone());
                }
                (lines.join("\n"), ch.ended.is_none())
            }
            HudText::Banner => match &ch.banner {
                Some((t, _)) => (t.clone(), ch.ended.is_none()),
                None => (String::new(), false),
            },
        };
        if text.0 != t {
            text.0 = t;
        }
        let want = if show { Visibility::Inherited } else { Visibility::Hidden };
        if *vis != want {
            *vis = want;
        }
    }
}

/// A HUD counter's caption (`EDisDLC05HUDItem`).
fn item_label(item: &str) -> &str {
    match item {
        "DDHI_Kills" => "Kills",
        "DDHI_EnemiesLeft" => "Enemies left",
        "DDHI_GateCount" => "Gates",
        "DDHI_BatteriesDestroyed" => "Tanks destroyed",
        "DDHI_CoinCount" => "Coins",
        "DDHI_EggCount" => "Clockwork eggs",
        "DDHI_EggDiscovery" => "Eggs found",
        "DDHI_BustedCount" => "Spotted",
        other => other.trim_start_matches("DDHI_"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_board_keeps_best_runs_and_marks_the_last() {
        let mut p = ChallengeProfile::default();
        // (a best from before there were boards: alone, unmarked)
        p.best.insert("Race".into(), 500);
        assert_eq!(p.board("Race"), (vec![Run { score: 500, at: 0, n: 0 }], None));
        for s in [300, 900, 100, 700, 200, 800, 400, 600, 1000, 50] {
            p.record_run("Race", s);
        }
        let (runs, last) = p.board("Race");
        assert_eq!(runs.len(), BOARD_RUNS);
        assert_eq!(runs.iter().map(|r| r.score).collect::<Vec<_>>(), vec![1000, 900, 800, 700, 600, 400, 300, 200, 100]);
        // (the last run, 50, fell off the board: nothing marked)
        assert_eq!(last, None);
        p.record_run("Race", 850);
        let (runs, last) = p.board("Race");
        assert_eq!(last.map(|i| runs[i].score), Some(850));
        assert_eq!(p.board("Thief"), (vec![], None));
    }
}
