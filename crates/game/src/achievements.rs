//! Achievements (`DisTweaks_PlayerStats.m_Achievements`): judged from the player statistics
//! (`m_StatInfos`: kills by weapon and by victim, NPCs alerted this mission, money stolen,
//! distance travelled...) as they change, or when the level scripts say
//! (`DisSeqAct_EvalAchievement`: the story's, with no conditions; the stealthy missions', from
//! the mission's statistics). A condition is a statistic against a threshold, or a streak: so
//! much gained within so many seconds (`m_fStreakValue` / `m_fStreakTime`: six kills in a
//! second, thirty metres in one). The statistics that span the game ("Kills", "Amount of money
//! stolen") are the profile's running totals, as Steam keeps them; those of "this mission" are
//! the mission's. Unlocked achievements and the totals are kept with the profile
//! (`achievements.json` beside the options) and announced as Steam's overlay announces the
//! original's: a card in the screen's lower right, over everything (the HUD may be hidden).

use crate::gameplay::PlayerStats;
use crate::player::Player;
use crate::GameState;
use bevy::prelude::*;
use dhcook::format::{AchievementDef, StatInfoDef};
use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};

pub struct AchievementsPlugin;

impl Plugin for AchievementsPlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(Achievements::load()).add_systems(Update, (judge.run_if(in_state(GameState::InGame)), toasts));
    }
}

/// An unlock's card: how long it has been up, and its place in the stack.
#[derive(Component)]
struct Toast {
    t: f32,
    slot: usize,
}

/// How long a card is up (s), and its fades.
const TOAST_SECS: f32 = 6.0;
const TOAST_FADE: f32 = 0.4;

fn spawn_toast(commands: &mut Commands, slot: usize, name: &str) {
    let gold = Color::srgba(0.86, 0.78, 0.6, 0.0);
    commands
        .spawn((
            Toast { t: 0.0, slot },
            Node {
                position_type: PositionType::Absolute,
                right: px(28),
                bottom: px(28),
                width: px(340),
                padding: UiRect::axes(px(16), px(10)),
                border: UiRect::left(px(3)),
                flex_direction: FlexDirection::Column,
                row_gap: px(3),
                ..default()
            },
            BackgroundColor(Color::srgba(0.04, 0.045, 0.05, 0.0)),
            BorderColor::all(gold),
            GlobalZIndex(96),
            Pickable::IGNORE,
        ))
        .with_children(|c| {
            c.spawn((Text::new("Achievement unlocked"), TextFont { font_size: bevy::text::FontSize::Px(14.0), ..default() }, TextColor(gold)));
            c.spawn((Text::new(title(name)), TextFont { font_size: bevy::text::FontSize::Px(22.0), ..default() }, TextColor(Color::srgba(0.92, 0.95, 0.88, 0.0))));
        });
}

/// The cards: in, up a while, out (the newest at the bottom, the others stacked above).
fn toasts(mut commands: Commands, time: Res<Time<Real>>, mut cards: Query<(Entity, &mut Toast, &mut Node, &mut BackgroundColor, &mut BorderColor, &Children)>, mut texts: Query<&mut TextColor>) {
    let dt = time.delta_secs().min(0.1);
    for (e, mut toast, mut n, mut bg, mut border, kids) in &mut cards {
        toast.t += dt;
        if toast.t > TOAST_SECS {
            commands.entity(e).despawn();
            continue;
        }
        let a = (toast.t / TOAST_FADE).min((TOAST_SECS - toast.t) / TOAST_FADE).clamp(0.0, 1.0);
        let rise = (1.0 - (toast.t / TOAST_FADE).min(1.0)).powi(3) * 24.0;
        n.bottom = px(28.0 + toast.slot as f32 * 82.0 - rise);
        bg.0 = bg.0.with_alpha(0.82 * a);
        *border = BorderColor::all(Color::srgba(0.86, 0.78, 0.6, a));
        for k in kids.iter() {
            if let Ok(mut c) = texts.get_mut(k) {
                c.0 = c.0.with_alpha(a);
            }
        }
    }
}

/// How often the statistics are read (s).
const TICK: f32 = 0.2;

/// The achievements the original judges itself as a mission's statistics come up (flagged as
/// the scripts', which never name them): any mission's past the prologue, and the game's (its
/// last mission's), over the whole campaign.
const MISSION_END: &[&str] = &["eAchievement_Shadow"];
const GAME_END: &[&str] = &["eAchievement_Ghost", "eAchievement_CleanHands", "eAchievement_FleshAndSteel"];
const PROLOGUE: &str = "Twk_M0_Prison";
const FINALE: &str = "Twk_M8_Lighthouse";

#[derive(Resource, Default)]
pub struct Achievements {
    pub unlocked: BTreeSet<String>,
    /// the profile's running totals of the statistics that span the game (by `stat_infos` index)
    lifetime: BTreeMap<u32, f32>,
    /// each statistic's value last read (its mission value), and its recent values (time,
    /// value: for the streaks)
    last: HashMap<u32, f32>,
    recent: HashMap<u32, VecDeque<(f32, f32)>>,
    /// what the game measures as it goes: distance travelled (UE units), time possessing (s)
    live: HashMap<&'static str, f32>,
    last_pos: Option<Vec3>,
    clock: f32,
    timer: f32,
    since_save: f32,
    dirty: bool,
}

#[derive(serde::Serialize, serde::Deserialize, Default)]
struct Profile {
    unlocked: BTreeSet<String>,
    #[serde(default)]
    lifetime: BTreeMap<u32, f32>,
}

impl Achievements {
    fn path() -> std::path::PathBuf {
        crate::settings::user_dir().join("achievements.json")
    }
    fn load() -> Self {
        let data = std::fs::read(Self::path()).ok();
        // (the profile, or the bare list of an older one)
        let p: Profile = data
            .as_ref()
            .and_then(|d| serde_json::from_slice::<Profile>(d).ok().or_else(|| serde_json::from_slice::<BTreeSet<String>>(d).ok().map(|unlocked| Profile { unlocked, ..default() })))
            .unwrap_or_default();
        Achievements { unlocked: p.unlocked, lifetime: p.lifetime, ..default() }
    }
    fn save(&mut self) {
        let p = Profile { unlocked: self.unlocked.clone(), lifetime: self.lifetime.clone() };
        if let Ok(d) = serde_json::to_vec_pretty(&p) {
            let _ = std::fs::write(Self::path(), d);
        }
        self.dirty = false;
        self.since_save = 0.0;
    }
}

/// A statistic's value this mission (or, for those the game measures itself, so far): kills
/// and the like counted by the blow's type and the victim (`<stat>|<damage type>|<pawn>`),
/// filtered as the statistic says, else the statistic itself.
pub fn stat_value(info: &StatInfoDef, stats: &PlayerStats, live: &HashMap<&'static str, f32>) -> f32 {
    if let Some(v) = live.get(info.stat.as_str()) {
        return *v;
    }
    // bone charms and valuables picked up (`ePlayerStat_ItemsCollected` of a pickup's tweak)
    if info.stat == "ePlayerStat_ItemsCollected" && info.tweaks.iter().any(|t| t.contains("BoneCharm")) {
        return stats.charms_found as f32;
    }
    if info.damage_types.is_empty() && info.tweaks.is_empty() && info.excluded.is_empty() {
        return stats.stat(&info.stat) as f32;
    }
    let family = |pawn: &str, tweak: &str| {
        let (p, t) = (pawn.rsplit('.').next().unwrap_or(pawn).to_ascii_lowercase(), tweak.rsplit('.').next().unwrap_or(tweak).to_ascii_lowercase());
        p == t || p.starts_with(t.trim_end_matches("_base"))
    };
    let prefix = format!("{}|", info.stat);
    let mut keyed = false;
    let mut sum = 0.0;
    for (k, v) in &stats.counters {
        let Some(rest) = k.strip_prefix(&prefix) else { continue };
        keyed = true;
        let (kind, pawn) = rest.split_once('|').unwrap_or((rest, ""));
        if !info.damage_types.is_empty() && !info.damage_types.iter().any(|d| crate::worlddamage::is_a(kind, d)) {
            continue;
        }
        if !info.tweaks.is_empty() && !info.tweaks.iter().any(|t| family(pawn, t)) {
            continue;
        }
        if info.excluded.iter().any(|t| family(pawn, t)) {
            continue;
        }
        sum += *v as f32;
    }
    // (nothing counted by kind: the statistic as it stands, if it isn't filtered by kind)
    if !keyed && info.damage_types.is_empty() && info.tweaks.is_empty() {
        return stats.stat(&info.stat) as f32;
    }
    sum
}

/// A statistic over the campaign so far (the missions before this one and this one's).
pub fn campaign_value(info: &StatInfoDef, stats: &PlayerStats, live: &HashMap<&'static str, f32>) -> f32 {
    let past = match info.stat.as_str() {
        "ePlayerStat_NumKills" if info.damage_types.is_empty() && info.tweaks.is_empty() => stats.past_kills,
        "ePlayerStat_NPCsAlerted" => stats.past_detected,
        _ => 0,
    };
    stat_value(info, stats, live) + past as f32
}

/// Whether a statistic is the mission's (else the profile's running total).
fn per_mission(info: &StatInfoDef) -> bool {
    info.text.to_ascii_lowercase().contains("this mission")
}

/// Whether an achievement's conditions all hold: each statistic against its threshold, or its
/// streak (so much gained within so long); one with none holds when the scripts judge it.
fn holds(a: &AchievementDef, value: impl Fn(u32) -> f32, gained: impl Fn(u32, f32) -> f32) -> bool {
    a.evals.iter().enumerate().all(|(k, (i, op, threshold))| {
        if let Some([amount, within]) = a.streaks.get(k).copied().filter(|s| s[0] > 0.0) {
            return gained(*i, within) >= amount;
        }
        let v = value(*i);
        match op.as_str() {
            "eValueInequality_LessThan" => v < *threshold,
            "eValueInequality_Equal" | "eValueInequality_EqualTo" => (v - threshold).abs() < 1e-3,
            "eValueInequality_LessThanOrEqual" => v <= *threshold,
            "eValueInequality_GreaterThanOrEqual" => v >= *threshold,
            _ => v > *threshold,
        }
    })
}

/// "eAchievement_FleshAndSteel" -> "Flesh and Steel" (the small words lower, past the first).
fn title(name: &str) -> String {
    let n = name.trim_start_matches("eAchievement_");
    let n = n.split_once('_').filter(|(p, _)| p.starts_with("DLC")).map(|(_, r)| r).unwrap_or(n);
    let mut out = String::new();
    for (i, c) in n.chars().enumerate() {
        if i > 0 && c.is_uppercase() {
            out.push(' ');
        }
        out.push(c);
    }
    let small = ["And", "Of", "The", "A", "In", "To", "On", "For", "With"];
    out.split(' ').enumerate().map(|(i, w)| if i > 0 && small.contains(&w) { w.to_ascii_lowercase() } else { w.to_string() }).collect::<Vec<_>>().join(" ")
}

#[allow(clippy::too_many_arguments)]
fn judge(
    mut ach: ResMut<Achievements>,
    data: Res<crate::gamedata::Data>,
    stats: Res<PlayerStats>,
    vm: Option<ResMut<crate::kismet::Vm>>,
    mut commands: Commands,
    cards: Query<&Toast>,
    time: Res<Time>,
    player: Query<&Transform, With<Player>>,
    possession: Res<crate::possession::Possession>,
    mut ended: MessageReader<crate::mission::ShowMissionStats>,
) {
    let defs = &data.0.achievements;
    let infos = &data.0.stat_infos;
    if defs.is_empty() {
        return;
    }
    let dt = time.delta_secs();
    ach.clock += dt;
    ach.since_save += dt;
    // what the game measures itself: the way Corvo goes (not his teleports), his time in a host
    if let Ok(t) = player.single() {
        let step = ach.last_pos.map(|p| t.translation.distance(p)).unwrap_or(0.0);
        if step < 20.0 {
            *ach.live.entry("ePlayerStat_DistanceTravelled").or_default() += step * 100.0;
        }
        ach.last_pos = Some(t.translation);
    }
    if possession.host.is_some() {
        *ach.live.entry("ePlayerStat_TimePossessing").or_default() += dt;
    }
    let mut won: Vec<String> = Vec::new();
    ach.timer -= dt;
    let tick = ach.timer <= 0.0;
    if tick {
        ach.timer = TICK;
        // the statistics the achievements read: their gains add to the running totals
        let used: BTreeSet<u32> = defs.iter().flat_map(|a| a.evals.iter().map(|e| e.0)).collect();
        let clock = ach.clock;
        for i in used {
            let Some(info) = infos.get(i as usize) else { continue };
            let cur = stat_value(info, &stats, &ach.live);
            let last = ach.last.insert(i, cur).unwrap_or(cur);
            if cur > last {
                *ach.lifetime.entry(i).or_default() += cur - last;
                ach.dirty = true;
            }
            let v = if per_mission(info) { cur } else { ach.lifetime.get(&i).copied().unwrap_or(0.0) };
            let r = ach.recent.entry(i).or_default();
            r.push_back((clock, v));
            while r.front().is_some_and(|(t, _)| clock - t > 200.0) {
                r.pop_front();
            }
        }
    }
    let value = |i: u32| ach.recent.get(&i).and_then(|r| r.back()).map(|(_, v)| *v).unwrap_or(0.0);
    let gained = |i: u32, within: f32| {
        let Some(r) = ach.recent.get(&i) else { return 0.0 };
        let now = r.back().map(|(t, v)| (*t, *v)).unwrap_or((0.0, 0.0));
        let before = r.iter().filter(|(t, _)| now.0 - t <= within + 1e-3).map(|(_, v)| *v).fold(f32::INFINITY, f32::min);
        if before.is_finite() { now.1 - before } else { 0.0 }
    };
    // the scripts' judgements
    if let Some(mut vm) = vm {
        for (name, _reset) in std::mem::take(&mut vm.achievement_evals) {
            if let Some(a) = defs.iter().find(|a| a.name == name) {
                if holds(a, value, gained) {
                    won.push(a.name.clone());
                }
            }
        }
    }
    // the original's own, as a mission's statistics come up
    for ev in ended.read() {
        let tweak = ev.tweak.rsplit('.').next().unwrap_or(&ev.tweak).to_string();
        for a in defs.iter().filter(|a| !ach.unlocked.contains(&a.name)) {
            let game = GAME_END.contains(&a.name.as_str());
            let now = if game { tweak == FINALE } else { MISSION_END.contains(&a.name.as_str()) && tweak != PROLOGUE };
            let value = |i: u32| infos.get(i as usize).map(|info| if game { campaign_value(info, &stats, &ach.live) } else { stat_value(info, &stats, &ach.live) }).unwrap_or(0.0);
            if now && holds(a, value, |_, _| 0.0) {
                won.push(a.name.clone());
            }
        }
    }
    // the others, as the statistics change (those with no conditions are the scripts')
    if tick {
        for a in defs.iter().filter(|a| !a.kismet && !a.evals.is_empty() && !ach.unlocked.contains(&a.name)) {
            if holds(a, value, gained) {
                won.push(a.name.clone());
            }
        }
    }
    let mut slot = cards.iter().map(|t| t.slot + 1).max().unwrap_or(0);
    for name in won {
        if ach.unlocked.insert(name.clone()) {
            info!("achievement: {name}");
            spawn_toast(&mut commands, slot, &name);
            slot += 1;
            ach.dirty = true;
            ach.since_save = 1e9;
        }
    }
    if ach.dirty && ach.since_save > 30.0 {
        ach.save();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conditions() {
        let infos = vec![
            StatInfoDef { stat: "ePlayerStat_NPCsAlerted".into(), ..Default::default() },
            StatInfoDef { stat: "ePlayerStat_NumKills".into(), damage_types: vec!["DisDamageType_FastHit_Left".into(), "DisDamageType_FastHit_Right".into()], ..Default::default() },
            StatInfoDef { stat: "ePlayerStat_NumKills".into(), damage_types: vec!["DisDamageType_WallOfLight".into()], tweaks: vec!["Pwn_Guard_Base.Pwn_Guard_Base".into()], ..Default::default() },
        ];
        let live = HashMap::new();
        let shadow = AchievementDef { name: "eAchievement_Shadow".into(), kismet: true, evals: vec![(0, "eValueInequality_LessThan".into(), 1.0)], streaks: vec![] };
        let blade = AchievementDef { name: "x".into(), kismet: false, evals: vec![(1, "eValueInequality_GreaterThan".into(), 0.0)], streaks: vec![] };
        let mut stats = PlayerStats::default();
        let judge = |a: &AchievementDef, stats: &PlayerStats| holds(a, |i| stat_value(&infos[i as usize], stats, &live), |_, _| 0.0);
        assert!(judge(&shadow, &stats));
        assert!(!judge(&blade, &stats));
        stats.times_detected = 1;
        assert!(!judge(&shadow, &stats));
        // a sword kill, counted by its damage type
        let kind = crate::worlddamage::hit_type(crate::gameplay::HitKind::Sword, 0.0, &crate::gamedata::Attrs::default());
        stats.counters.insert(format!("ePlayerStat_NumKills|{kind}|Pwn_Guard_Pistol_MTall_1.Pwn_Guard_Pistol_MTall_1"), 1);
        assert!(judge(&blade, &stats));
        // a guard killed by a wall of light (its pawn of the guards' family)
        assert_eq!(stat_value(&infos[2], &stats, &live), 0.0);
        stats.counters.insert("ePlayerStat_NumKills|DisDamageType_WallOfLight|Pwn_Guard_Pistol_MTall_1.Pwn_Guard_Pistol_MTall_1".into(), 1);
        assert_eq!(stat_value(&infos[2], &stats, &live), 1.0);
    }

    #[test]
    fn streaks() {
        // six kills within a second
        let tempest = AchievementDef { name: "eAchievement_Tempest".into(), kismet: false, evals: vec![(2, "eValueInequality_LessThan".into(), 0.0)], streaks: vec![[6.0, 1.0]] };
        assert!(!holds(&tempest, |_| 0.0, |_, _| 5.0));
        assert!(holds(&tempest, |_| 0.0, |_, _| 6.0));
    }

    #[test]
    fn game_end() {
        // Clean Hands over the campaign: a kill in an earlier mission spoils it
        let info = StatInfoDef { stat: "ePlayerStat_NumKills".into(), ..Default::default() };
        let mut stats = PlayerStats::default();
        let live = HashMap::new();
        assert_eq!(campaign_value(&info, &stats, &live), 0.0);
        stats.past_kills = 1;
        assert_eq!(campaign_value(&info, &stats, &live), 1.0);
        // Flesh and Steel: Blink alone is one power acquired
        stats.powers.insert("Blink".into(), 1);
        assert_eq!(stats.stat("ePlayerStat_PowersAcquired"), 1);
        stats.powers.insert("DarkVision".into(), 1);
        assert_eq!(stats.stat("ePlayerStat_PowersAcquired"), 2);
    }

    #[test]
    fn titles() {
        assert_eq!(title("eAchievement_FleshAndSteel"), "Flesh and Steel");
        assert_eq!(title("eAchievement_SpeedOfDarkness"), "Speed of Darkness");
        assert_eq!(title("eAchievement_DLC05_VoidStar"), "Void Star");
    }
}
