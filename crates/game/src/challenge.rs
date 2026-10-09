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
            .add_systems(Update, (start, apply, tick, deaths).chain().run_if(in_state(GameState::InGame)))
            .add_systems(Update, hud.run_if(in_state(GameState::InGame)));
    }
}

/// How the challenge menu launched the run.
#[derive(Resource, Default)]
pub struct ChallengeLaunch {
    pub expert: bool,
    /// back from a run to the challenges (the results' "Exit Challenge")
    pub back_to_challenges: bool,
}

/// A challenge timer's settings (`DisSeqAct_DLC05_Timer`).
#[derive(Clone, Debug)]
pub struct TimerParams {
    pub initial: f32,
    /// it completes there (`m_bUseTargetTime`, `m_fTargetTime`)
    pub target: Option<f32>,
    pub increment: bool,
    pub reset_on_stop: bool,
    /// `DDHT_DefaultTimer`, `DDHT_CountdownTimer`, `DDHT_KillChainTimer`
    pub kind: String,
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
    PhaseResults { name: String, last: bool, possible: i32, required: i32, effective: i32 },
    EquipmentUnlock,
    Difficulty(String),
    Resurrect,
    Heal(f32),
    InfiniteAmmo(bool),
    Doll,
    WaveBendTime { op: u32, secs: f32 },
    Text(Option<String>),
    StopAllSounds,
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

    fn score_entry(&mut self, entry: &str, points: i64) {
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

/// The profile's challenge records: best scores (by challenge id), the dolls found (by map).
#[derive(Resource, Default, serde::Serialize, serde::Deserialize)]
pub struct ChallengeProfile {
    pub best: BTreeMap<String, i64>,
    pub dolls: BTreeSet<String>,
}

impl ChallengeProfile {
    fn path() -> std::path::PathBuf {
        crate::settings::user_dir().join("dlc05.json")
    }
    fn load() -> Self {
        std::fs::read(Self::path()).ok().and_then(|d| serde_json::from_slice(&d).ok()).unwrap_or_default()
    }
    fn save(&self) {
        if let Ok(d) = serde_json::to_vec_pretty(self) {
            let _ = std::fs::write(Self::path(), d);
        }
    }
}

/// The map that came up: a challenge's, or not.
fn begin(mut ch: ResMut<Challenge>, level: Option<Res<crate::level::LevelInfo>>, data: Res<crate::gamedata::Data>, launch: Res<ChallengeLaunch>, profile: Res<ChallengeProfile>) {
    let map = level.map(|l| l.scene.name.clone()).unwrap_or_default();
    let def = data.0.challenges.iter().find(|c| c.map.eq_ignore_ascii_case(&map)).cloned();
    *ch = Challenge::default();
    if let Some(d) = def {
        info!("challenge: {} ({}){}", d.name, d.id, if launch.expert { ", expert" } else { "" });
        ch.best = profile.best.get(&d.id).copied().unwrap_or(0);
        ch.prev_best = ch.best;
        ch.def = Some(d);
        ch.map = map;
        ch.expert = launch.expert;
    }
}

/// Once the level is up: the opening, and "Started".
fn start(mut ch: ResMut<Challenge>, vm: Option<ResMut<crate::kismet::Vm>>, warm: Option<Res<crate::warmup::Warmup>>, time: Res<Time>, profile: Res<ChallengeProfile>, stats: Res<PlayerStats>) {
    if !ch.active() || ch.started {
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
    vm.challenge_event(0);
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
                    ch.ended = Some(failed);
                    let def = ch.def.clone().unwrap_or_default();
                    let medals = if ch.expert && def.expert_medals.iter().any(|m| *m > 0) { def.expert_medals } else { def.medals };
                    ch.medal = if failed { 0 } else { medals.iter().filter(|m| **m > 0 && ch.score >= **m as i64).count() };
                    if !failed && ch.score > ch.best {
                        ch.best = ch.score;
                        profile.best.insert(def.id.clone(), ch.score);
                        profile.save();
                    }
                    info!("challenge: {} with {} points (medal {})", if failed { "failed" } else { "over" }, ch.score, ch.medal);
                    vm.challenge_event(if failed { 2 } else { 1 });
                }
                "ECE_Challenge_Pause" => ch.paused = true,
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
            ChallengeFx::Rules(t) => ch.rules = Some(t),
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
                ch.hud_events.push(HudEvent::Wave { number, text });
            }
            ChallengeFx::Countdown { op, go } => {
                ch.countdown = Some((op, 3.0 + if go { 0.8 } else { 0.0 }, go));
                ch.hud_events.push(HudEvent::CountdownStart { go });
            }
            ChallengeFx::PhaseResults { name, last, required, effective, .. } => {
                let t = if required > 0 { format!("{name}: {effective} / {required}") } else { name };
                ch.banner = Some((if last { format!("{t} (final)") } else { t }, 4.0));
            }
            ChallengeFx::EquipmentUnlock => ch.banner = Some(("New equipment".into(), 2.5)),
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
        }
    }
}

/// A scoring rule's base gain, by its entry name, from the rule set in force.
fn rule_points(sets: &[dhcook::format::RulesetDef], rules: Option<&str>, entry: &str) -> Option<i64> {
    let set = sets.iter().find(|r| Some(r.name.as_str()) == rules)?;
    set.rules.iter().find(|r| r.entry.eq_ignore_ascii_case(entry) || r.name.eq_ignore_ascii_case(entry)).map(|r| r.base_gain as i64)
}

/// Timers, the countdown, titles; kills scored.
#[allow(clippy::too_many_arguments)]
fn tick(mut ch: ResMut<Challenge>, vm: Option<ResMut<crate::kismet::Vm>>, time: Res<Time>, mut stats: ResMut<PlayerStats>, level: Option<Res<crate::level::LevelInfo>>, npcs: Query<&crate::npc::Npc>) {
    if !ch.active() {
        return;
    }
    let Some(mut vm) = vm else { return };
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
    // the kills Corvo makes score (the wave kill rule's gain for the victim's kind)
    if stats.kills > ch.last_kills && ch.ended.is_none() {
        let n = stats.kills - ch.last_kills;
        let dead: Vec<String> = npcs.iter().filter(|n| n.is_down()).map(|n| n.story_group.clone()).collect();
        for k in 0..n {
            let group = dead.get(dead.len().saturating_sub((n - k) as usize)).cloned().unwrap_or_default();
            let points = kill_points(level.as_ref().map(|l| l.scene.challenge_rules.as_slice()).unwrap_or(&[]), ch.rules.as_deref(), &group);
            ch.kills += 1;
            ch.score_entry("Kill", points);
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

/// A kill's points: the wave kill rule's gain for the victim's story group, else a default.
fn kill_points(sets: &[dhcook::format::RulesetDef], rules: Option<&str>, group: &str) -> i64 {
    let Some(set) = sets.iter().find(|r| Some(r.name.as_str()) == rules) else { return 100 };
    let tail = |s: &str| s.rsplit('.').next().unwrap_or(s).to_ascii_lowercase();
    set.rules
        .iter()
        .flat_map(|r| r.gains.iter())
        .find(|(g, _, _)| !group.is_empty() && tail(g) == tail(group))
        .map(|(_, p, _)| *p as i64)
        .or_else(|| set.rules.iter().find(|r| r.class.ends_with("WaveKill") || r.class.ends_with("_Kill")).map(|r| r.base_gain as i64).filter(|p| *p > 0))
        .unwrap_or(100)
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
