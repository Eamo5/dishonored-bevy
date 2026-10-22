//! The campaign: the original persistent level's scripts (`DishonoredGameFull_P`, cooked
//! to `cache/game/campaign.json`) run alongside every map's. The maps end their missions
//! with `ChangeLvl_*` remote events; the campaign scripts answer with the mission statistics,
//! chapter notes, story flags and the map change (`SeqAct_PrepareMapChange` /
//! `SeqAct_CommitMapChange`), choosing the high-chaos version of a mission when the
//! campaign's chaos (`DisSeqVar_DarknessLevel`) has reached its threshold.

use crate::gamedata::Data;
use crate::gameplay::{Campaign, PlayerStats};
use crate::kismet::Vm;
use crate::npc::{Kind, Npc, NpcSpawned};
use crate::GameState;
use bevy::prelude::*;
use dhcook::format::{DarknessDef, KVal, Kismet as Graph};
use std::sync::Arc;

pub struct CampaignPlugin;

impl Plugin for CampaignPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Update, track_hub.run_if(in_state(crate::GameState::InGame)))
            .add_systems(OnEnter(crate::GameState::InGame), restore_hub.after(crate::level::LevelSpawnSet));
        let path = std::path::Path::new("cache/game/campaign.json");
        let script = match std::fs::read(path).map_err(anyhow::Error::from).and_then(|d| Ok(serde_json::from_slice::<Graph>(&d)?)) {
            Ok(g) => {
                info!("campaign scripts: {} ops", g.ops.len());
                Some(Arc::new(g))
            }
            Err(e) => {
                warn!("no campaign scripts ({}: {e}); run `dhtool cook-gamedata`", path.display());
                None
            }
        };
        app.insert_resource(CampaignScript(script)).add_systems(
            Update,
            apply_campaign.after(crate::kismet::apply_effects).run_if(in_state(GameState::InGame).and_then(resource_exists::<Vm>)),
        );
    }
}

#[derive(Resource)]
pub struct CampaignScript(pub Option<Arc<Graph>>);

/// What the campaign scripts ask for.
#[derive(Clone, Debug)]
pub enum CampaignFx {
    /// `SeqAct_CommitMapChange`: load the map, arriving at the travel destination; the
    /// commit op's output fires there
    ChangeMap { map: String, tag: Option<String>, op: u32, levels: Vec<String> },
    MissionBegin(String),
    MissionEnd(String),
    ShowStats { tweak: String, op: u32 },
    Loadout(String),
    AbstractItem(String),
    Chapter(String),
    IncrementStat(String),
    /// `DisSeqAct_AddDarkness`: points onto the chaos score
    Darkness(i32),
}

/// Append the campaign graph to a map's: op and variable indices shift past the map's.
/// Returns the campaign's first op and variable.
pub fn merge(g: &mut Graph, c: &Graph) -> (u32, u32) {
    let (oo, vo, mo) = (g.ops.len() as u32, g.vars.len() as u32, g.matinees.len() as i32);
    fn fix(v: &KVal, oo: u32, vo: u32) -> KVal {
        match v {
            KVal::Op(x) => KVal::Op(x + oo),
            KVal::Var(x) => KVal::Var(x + vo),
            // the persistent level has no actors of the map's
            KVal::Actor(_) => KVal::Str(String::new()),
            KVal::List(l) => KVal::List(l.iter().map(|x| fix(x, oo, vo)).collect()),
            other => other.clone(),
        }
    }
    for op in &c.ops {
        let mut o = op.clone();
        o.parent = o.parent.map(|p| p + oo);
        for i in &mut o.inputs {
            i.linked = i.linked.map(|x| x + oo);
        }
        for out in &mut o.outputs {
            out.linked = out.linked.map(|x| x + oo);
            for l in &mut out.links {
                l.0 += oo;
            }
        }
        for l in &mut o.vars {
            for v in &mut l.vars {
                *v += vo;
            }
        }
        o.props = o.props.iter().map(|(k, v)| (k.clone(), fix(v, oo, vo))).collect();
        g.ops.push(o);
    }
    for var in &c.vars {
        let mut v = var.clone();
        v.parent = v.parent.map(|p| p + oo);
        v.props = v
            .props
            .iter()
            .map(|(k, x)| {
                let x = match (k.as_str(), x) {
                    ("Matinee", KVal::Int(i)) => KVal::Int(i + mo),
                    _ => fix(x, oo, vo),
                };
                (k.clone(), x)
            })
            .collect();
        g.vars.push(v);
    }
    g.matinees.extend(c.matinees.iter().cloned());
    g.objectives.extend(c.objectives.iter().cloned());
    (oo, vo)
}

/// The campaign mission a map belongs to (`m_MissionsGame`), -1 for the prologue.
pub fn mission_number(data: &Data, map: &str) -> i32 {
    data.0.missions.iter().find(|(_, maps)| maps.iter().any(|m| m.trim().eq_ignore_ascii_case(map))).map(|m| m.0).unwrap_or(-1)
}

/// The chaos level a score reaches: the last of `m_DarknessLevels`' steps at or under it.
pub fn darkness_level(levels: &[i32], score: i32) -> i32 {
    if levels.is_empty() {
        return 0;
    }
    levels.iter().rposition(|&l| score >= l).map(|i| i as i32).unwrap_or(-1)
}

/// Points onto the chaos score (never below 0), and the level it now reaches.
pub fn add_chaos(dark: &DarknessDef, s: &mut PlayerStats, n: i32) {
    let before = s.chaos_level;
    s.chaos_score = s.chaos_score.saturating_add(n).max(0);
    s.chaos_level = darkness_level(&dark.levels, s.chaos_score);
    if s.chaos_level != before {
        info!("campaign: chaos score {} (level {before} -> {})", s.chaos_score, s.chaos_level);
    }
}

/// A statistic the deeds count grew (`DisDarknessManager`'s deeds): every deed on it whose
/// victim filter takes the victim (its pawn tweak lineage, none excluded, the deed's own in it;
/// no victim: anyone) counts one, and a deed reaching its threshold adds its points and starts
/// again (no carry). As roshan's r614/blink research decoded the original.
pub fn notify_deed(dark: &DarknessDef, s: &mut PlayerStats, stat: &str, victim: Option<&[String]>) {
    if s.deed_counters.len() < dark.deeds.len() {
        s.deed_counters.resize(dark.deeds.len(), 0);
    }
    for (i, d) in dark.deeds.iter().enumerate() {
        if d.stat != stat {
            continue;
        }
        if let Some(v) = victim {
            let is = |n: &str| v.iter().any(|x| x.eq_ignore_ascii_case(n));
            if d.excluded.iter().any(|x| is(x)) || (!d.tweak.is_empty() && !is(&d.tweak)) {
                continue;
            }
        }
        s.deed_counters[i] += 1;
        if s.deed_counters[i] >= d.threshold {
            s.deed_counters[i] = 0;
            add_chaos(dark, s, d.value);
        }
    }
}

/// A pawn tweak's lineage (itself and what it falls back on) in the level's characters.
pub fn lineage(level: Option<&crate::level::LevelInfo>, pawn: &str) -> Vec<String> {
    let own = || vec![pawn.rsplit('.').next().unwrap_or(pawn).to_string()];
    level.and_then(|l| l.scene.npc_types.iter().find(|t| t.name == pawn)).map(|t| if t.lineage.is_empty() { own() } else { t.lineage.clone() }).unwrap_or_else(own)
}

/// Is the campaign high chaos now (the threshold of the mission being played)?
pub fn chaos_high(data: &Data, s: &PlayerStats, mission: i32) -> bool {
    let t = data.0.chaos_thresholds.get(mission.max(0) as usize).copied().unwrap_or(i32::MAX);
    s.chaos_level >= t.max(1)
}

/// A start loadout (`Twk_PlayerLoadout_*`): weapons, gadgets, runes, coins, powers, upgrades.
pub fn apply_loadout(data: &Data, name: &str, s: &mut PlayerStats) -> bool {
    let ok = apply_loadout_from(&data.0.loadouts, name, s);
    alias_powers(data, s);
    ok
}

/// The powers given by their own names played as the ones they are ("Transversal": Blink).
pub fn alias_powers(data: &Data, s: &mut PlayerStats) {
    for (from, to) in &data.0.power_aliases {
        if let Some(l) = s.powers.remove(from) {
            let e = s.powers.entry(to.clone()).or_insert(0);
            *e = (*e).max(l);
        }
    }
}

pub fn apply_loadout_from(list: &[dhcook::format::LoadoutDef], name: &str, s: &mut PlayerStats) -> bool {
    let Some(l) = list.iter().find(|l| l.name.eq_ignore_ascii_case(name)) else { return false };
    // (Daud's wristbow his crossbow)
    let bow = |i: &String| i.contains("Crossbow") || i.contains("Wristbow");
    s.weapons = l.items.iter().any(|i| bow(i) || i.contains("Pistol"));
    s.no_crossbow = s.weapons && !l.items.iter().any(bow);
    // (Daud's: the wristbow alone, neither pistol nor Heart)
    let wristbow = l.items.iter().any(|i| i.contains("Wristbow"));
    s.no_pistol = wristbow && !l.items.iter().any(|i| i.contains("Pistol"));
    // (the Heart only where it lists it: not before the Outsider's dream, Hub1)
    s.no_heart = !l.items.iter().any(|i| i.contains("Heart"));
    // its ammunition (by the original's `eDisAmmoType`) and elixirs, where it gives them
    if !l.ammo.is_empty() {
        for ty in 0..crate::gadgets::AMMO_TYPES as u8 {
            if let Some(n) = crate::gadgets::ammo_mut(s, ty, false) {
                *n = 0;
            }
        }
    }
    for (ty, n) in &l.ammo {
        match crate::gadgets::ammo_index(ty).and_then(|t| crate::gadgets::ammo_mut(s, t, true)) {
            Some(c) => *c += n,
            None => *s.items.entry(format!("Ammo_{ty}")).or_default() += n,
        }
    }
    if let Some([h, m]) = l.elixirs {
        s.health_elixirs = h;
        s.mana_elixirs = m;
    }
    // the gadgets held (their ammunition the loadout's own; a loadout without any, two)
    for i in &l.items {
        use crate::gadgets::*;
        let gadget = if i.contains("GrenadeDaud") {
            // (Daud's grenade hand: chokedust, and grenades found)
            Some(CHOKEDUST)
        } else if i.contains("Grenade") {
            Some(GRENADES)
        } else if i.contains("SpringRazor") {
            Some(RAZORS)
        } else if i.contains("Inv_StunMine") {
            Some(STUN_MINES)
        } else if i.contains("Inv_ArcMine") {
            Some(ARC_MINES)
        } else {
            None
        };
        if let Some(g) = gadget {
            *s.items.entry(g.to_string()).or_default() += if l.ammo.is_empty() { 2 } else { 0 };
        }
    }
    for (item, n) in &l.abstract_items {
        let lower = item.to_ascii_lowercase();
        if lower.starts_with("rune") {
            s.runes += n;
        } else if lower.starts_with("coins") {
            s.coins += n;
        } else if !s.upgrades.contains(item) {
            s.upgrades.push(item.clone());
        }
    }
    for (p, lvl) in &l.powers {
        s.powers.insert(p.clone(), *lvl);
    }
    for u in &l.upgrades {
        if !s.upgrades.contains(u) {
            s.upgrades.push(u.clone());
        }
    }
    true
}

/// A map started on its own begins with the original loadout of its mission.
pub fn direct_start_loadout(map: &str) -> Option<&'static str> {
    let m = map.to_ascii_lowercase();
    let name = match m.as_str() {
        "l_streets1_p" | "l_distillery_p" | "l_galvani1_p" | "l_ovrsr_p" | "l_ovrsr_kennel_p" | "l_ovrsr_back_p" => "Twk_PlayerLoadout_Streets1",
        "l_streets2_p" | "l_distillery2_p" | "l_galvani2_p" | "l_artdealer_p" => "Twk_PlayerLoadout_Streets2",
        "l_brothel_p" => "Twk_PlayerLoadout_Brothel",
        "l_bridge_part1a_p" | "l_bridge_part1b_p" | "l_bridge_part1c_p" | "l_bridge_part2_p" => "Twk_PlayerLoadout_Bridge",
        "l_boyle_ext_p" | "l_boyle_int_p" => "Twk_PlayerLoadout_Boyle",
        "l_towerrtrn_yard_p" | "l_towerrtrn_int_p" => "Twk_PlayerLoadout_TowerRtrn",
        "l_flooded_fintro_p" | "l_flooded_fstreets_p" | "l_flooded_fassassins_p" | "l_flooded_fgate_p" | "l_flooded_frefinery_p" | "l_streetsewer_p" => "Twk_PlayerLoadout_Flooded",
        "l_pub_assault_p" => "Twk_PlayerLoadout_HubAssault",
        "l_isl_lowchaos_p" | "l_isl_highchaos_p" | "l_lighth_lowchaos_p" | "l_lighth_highchaos_p" => "Twk_PlayerLoadout_LightHouse",
        // (The Knife of Dunwall: its first mission begins with the campaign's loadout, the
        // later ones with the fuller default)
        m if m.starts_with("dlc06_slaughter") => "DLC06_Twk_PlayerLoadout_Campaign",
        m if m.starts_with("dlc06_") => "DLC06_Twk_PlayerLoadout_Default",
        _ => return None,
    };
    Some(name)
}

/// A player start of a cooked map by tag (the map's own name as cooked).
pub(crate) fn start_in(map: &str, tag: Option<&str>) -> Option<(String, usize)> {
    #[derive(serde::Deserialize)]
    struct Starts {
        name: String,
        player_starts: Vec<dhcook::format::PlayerStart>,
    }
    let dir = crate::loading::cache_dir().join("maps");
    for e in std::fs::read_dir(dir).ok()?.flatten() {
        let p = e.path();
        let stem = p.file_stem().and_then(|s| s.to_str()).unwrap_or("");
        if !stem.eq_ignore_ascii_case(map) {
            continue;
        }
        let d = std::fs::read(&p).ok()?;
        let s: Starts = serde_json::from_slice(&d).ok()?;
        let i = tag.and_then(|t| s.player_starts.iter().position(|ps| ps.tag.eq_ignore_ascii_case(t))).unwrap_or(0);
        return Some((s.name, i));
    }
    None
}

#[allow(clippy::too_many_arguments)]
fn apply_campaign(
    time: Res<Time>,
    mut vm: ResMut<Vm>,
    mut campaign: ResMut<Campaign>,
    mut stats: ResMut<PlayerStats>,
    data: Res<Data>,
    level: Option<Res<crate::level::LevelInfo>>,
    mut spawned: MessageReader<NpcSpawned>,
    npcs: Query<&Npc>,
    (mut travel, mut show, mut tw): (MessageWriter<crate::mission::TravelRequest>, MessageWriter<crate::mission::ShowMissionStats>, ResMut<crate::tutwindow::TutorialWindow>),
) {
    // the people of the mission (whose deaths make chaos)
    for s in spawned.read() {
        if npcs.get(s.entity).is_ok_and(|n| matches!(n.kind, Kind::Guard | Kind::Thug | Kind::Civilian)) {
            stats.npcs_met += 1;
        }
    }
    let map = level.as_ref().map(|l| l.scene.name.as_str()).unwrap_or("");
    // the deeds: kills (by the victim's lineage), alarms, bodies found
    for (stat, pawn) in std::mem::take(&mut stats.deeds_pending) {
        let victim = (!pawn.is_empty()).then(|| lineage(level.as_deref(), &pawn));
        notify_deed(&data.0.darkness, &mut stats, stat, victim.as_deref());
    }
    stats.chaos_high = chaos_high(&data, &stats, mission_number(&data, map));
    vm.darkness_levels.clone_from(&data.0.darkness.levels);
    vm.darkness = stats.chaos_level;
    vm.darkness_score = stats.chaos_score;
    // a travel destination in no loaded map, and the campaign didn't change map
    if let Some((tag, left)) = vm.goto_fallback.take() {
        let left = left - time.delta_secs();
        if left > 0.0 {
            vm.goto_fallback = Some((tag, left));
        } else if let Some((map, start)) = crate::kismet::find_start(&tag) {
            info!("level scripts: travel to {map} (start {tag}; no campaign map change)");
            travel.write(crate::mission::TravelRequest { map, start });
        }
    }
    if vm.campaign_fx.is_empty() {
        return;
    }
    for fx in std::mem::take(&mut vm.campaign_fx) {
        match fx {
            CampaignFx::ChangeMap { map, tag, op, levels } => {
                vm.goto_fallback = None;
                let Some((name, start)) = start_in(&map, tag.as_deref()) else {
                    warn!("campaign: map {map} is not cooked");
                    continue;
                };
                info!("campaign: map change to {name} (start {} {:?})", start, tag);
                if let Some((oo, _)) = vm.campaign {
                    campaign.resume = Some(op - oo);
                }
                campaign.script_state = vm.campaign_state();
                campaign.travel = tag;
                campaign.levels = Some(levels);
                travel.write(crate::mission::TravelRequest { map: name, start });
            }
            CampaignFx::MissionBegin(tweak) => {
                info!("campaign: mission {tweak} begins");
                stats.past_kills += stats.kills;
                stats.past_knockouts += stats.knockouts;
                stats.past_detected += stats.times_detected;
                stats.kills = 0;
                stats.knockouts = 0;
                stats.times_detected = 0;
                stats.civilians_killed = 0;
                stats.coins_found = 0;
                stats.runes_found = 0;
                stats.charms_found = 0;
                stats.alarms_rung = 0;
                stats.bodies_found = 0;
                stats.npcs_met = 0;
                stats.counters.clear();
            }
            // (the deeds' counters start again; the score stays)
            CampaignFx::MissionEnd(tweak) => {
                info!("campaign: mission {tweak} ends, chaos score {} (level {})", stats.chaos_score, stats.chaos_level);
                stats.deed_counters.clear();
            }
            CampaignFx::Darkness(n) => add_chaos(&data.0.darkness, &mut stats, n),
            CampaignFx::ShowStats { tweak, op } => {
                show.write(crate::mission::ShowMissionStats { tweak, op });
            }
            CampaignFx::Loadout(name) => {
                let local = level.as_ref().is_some_and(|l| apply_loadout_from(&l.scene.loadouts, &name, &mut stats));
                if local {
                    alias_powers(&data, &mut stats);
                }
                if !local && !apply_loadout(&data, &name, &mut stats) {
                    warn!("campaign: loadout {name} unknown");
                }
            }
            CampaignFx::AbstractItem(item) => {
                let lower = item.to_ascii_lowercase();
                // (a mission clue: the HUD tweak's `m_PressKeyToReadChapterNoteMessage`, in the
                // tutorial window with the note)
                let clue = data.hud_text("m_PressKeyToReadChapterNoteMessage", "Mission Clues updated<br />Press `GBA_Journal` to read");
                if data.0.abstract_items.contains_key(&item) && !lower.contains("coins") && !lower.contains("rune_") && !lower.starts_with("bp_") {
                    if !stats.notes.contains(&item) {
                        if item.contains("ChapterLog") {
                            tw.push(clue, crate::tutwindow::IMG_NOTE);
                        }
                        stats.notes.push(item);
                    }
                } else if item.contains("ChapterLog") || item.contains("Note") {
                    if !stats.notes.contains(&item) {
                        if item.contains("ChapterLog") {
                            tw.push(clue, crate::tutwindow::IMG_NOTE);
                        }
                        stats.notes.push(item);
                    }
                } else if item.to_ascii_lowercase().starts_with("rune") {
                    stats.runes += 1;
                } else if !stats.upgrades.contains(&item) {
                    stats.upgrades.push(item);
                }
            }
            CampaignFx::Chapter(c) => stats.chapter = c,
            CampaignFx::IncrementStat(s) => {
                if let Some(d) = ["ePlayerStat_NumKills", "ePlayerStat_NumAlarmsTriggered", "ePlayerStat_NumCorpsesDiscovered"].into_iter().find(|d| *d == s) {
                    notify_deed(&data.0.darkness, &mut stats, d, None);
                }
                *stats.counters.entry(s).or_default() += 1
            }
        }
    }
}

/// The sublevels the level scripts stream in that start loaded: those the campaign's map
/// change named (`InitiallyLoadedSecondaryLevelNames`), else, for a map started directly, those
/// of the campaign's first change to it (lower case).
pub fn initial_levels(scene: &dhcook::format::Scene, campaign: &crate::gameplay::Campaign, script: &CampaignScript) -> std::collections::HashSet<String> {
    if let Some(l) = &campaign.levels {
        return l.iter().map(|s| s.to_ascii_lowercase()).collect();
    }
    let Some(g) = script.0.as_ref() else { return Default::default() };
    let have = |n: &str| scene.levels.iter().any(|l| l.streamed && l.name.eq_ignore_ascii_case(n));
    // the campaign's changes to this map naming sublevels it has; the earliest in the story
    const STORY: [&str; 7] = ["prison", "ovrsr", "brothel", "bridge", "boyle", "twrreturn", "flooded"];
    let story = |l: &[String]| l.iter().map(|n| STORY.iter().position(|k| n.contains(k)).unwrap_or(STORY.len())).min().unwrap_or(STORY.len());
    g.ops
        .iter()
        .filter(|o| o.class == "SeqAct_PrepareMapChange")
        .filter(|o| matches!(o.props.get("MainLevelName"), Some(KVal::Str(m)) if m.eq_ignore_ascii_case(&scene.name)))
        .filter_map(|o| match o.props.get("InitiallyLoadedSecondaryLevelNames") {
            Some(KVal::List(l)) => {
                let names: Vec<String> = l.iter().filter_map(|v| if let KVal::Str(s) = v { Some(s.to_ascii_lowercase()) } else { None }).collect();
                // the Hound Pits' scripts for a return (a direct start of another map starts
                // with none: low chaos)
                names.iter().any(|n| have(n) && n.contains("_from")).then_some(names)
            }
            _ => None,
        })
        .min_by_key(|names| story(names))
        .map(|names| names.into_iter().collect())
        .unwrap_or_default()
}

/// Whether an instance or pickup belongs to a streamed sublevel that isn't loaded.
pub fn instance_unloaded(scene: &dhcook::format::Scene, loaded: &std::collections::HashSet<String>, instance: u32) -> bool {
    scene.levels.iter().any(|l| l.streamed && instance >= l.instances.0 && instance < l.instances.1 && !loaded.contains(&l.name.to_ascii_lowercase()))
}
pub fn pickup_unloaded(scene: &dhcook::format::Scene, loaded: &std::collections::HashSet<String>, pickup: u32) -> bool {
    scene.levels.iter().any(|l| l.streamed && pickup >= l.pickups.0 && pickup < l.pickups.1 && !loaded.contains(&l.name.to_ascii_lowercase()))
}

/// The streamed sublevel a volume belongs to (if it is one that comes and goes).
pub fn volume_level(scene: &dhcook::format::Scene, volume: u32) -> Option<&str> {
    scene.levels.iter().find(|l| l.streamed && volume >= l.volumes.0 && volume < l.volumes.1).map(|l| l.name.as_str())
}

/// Whether a spawner belongs to a streamed sublevel that isn't loaded.
pub fn spawner_unloaded(scene: &dhcook::format::Scene, loaded: &std::collections::HashSet<String>, spawner: u32) -> bool {
    scene.levels.iter().any(|l| l.streamed && spawner >= l.spawners.0 && spawner < l.spawners.1 && !loaded.contains(&l.name.to_ascii_lowercase()))
}

/// A Hound Pits pickup across the hub's map variants (they share their sublevels): its actor's
/// name where it lies.
pub fn hub_key(p: &dhcook::format::Pickup) -> String {
    format!("{}@{:.0},{:.0},{:.0}", p.name, p.position[0], p.position[1], p.position[2])
}

/// At the Hound Pits, what Corvo takes stays taken on his next visits.
pub fn track_hub(
    time: Res<Time>,
    level: Option<Res<crate::level::LevelInfo>>,
    mut campaign: ResMut<crate::gameplay::Campaign>,
    pickups: Query<&crate::interact::Pickup>,
    (mut wait, mut seen): (Local<f32>, Local<(String, std::collections::HashSet<u32>)>),
) {
    let Some(level) = level else { return };
    if !crate::save::is_hub(&level.scene.name) {
        return;
    }
    *wait -= time.delta_secs();
    if *wait > 0.0 {
        return;
    }
    *wait = 0.5;
    // (the pickups seen here this visit: those of sublevels not streamed in aren't taken)
    if seen.0 != level.scene.name {
        *seen = (level.scene.name.clone(), Default::default());
    }
    let present: std::collections::HashSet<u32> = pickups.iter().map(|p| p.index).collect();
    for (i, p) in level.scene.pickups.iter().enumerate() {
        let i = i as u32;
        if present.contains(&i) {
            seen.1.insert(i);
        } else if !p.factory && seen.1.contains(&i) {
            campaign.hub_taken.insert(hub_key(p));
        }
    }
}

/// Back at the Hound Pits: what he took before is gone.
pub fn restore_hub(mut commands: Commands, level: Option<Res<crate::level::LevelInfo>>, campaign: Res<crate::gameplay::Campaign>, pickups: Query<(Entity, &crate::interact::Pickup)>) {
    let Some(level) = level else { return };
    if campaign.hub_taken.is_empty() || !crate::save::is_hub(&level.scene.name) {
        return;
    }
    for (e, pk) in &pickups {
        let Some(p) = level.scene.pickups.get(pk.index as usize) else { continue };
        if !p.factory && campaign.hub_taken.contains(&hub_key(p)) {
            for &m in &pk.entities {
                commands.entity(m).try_despawn();
            }
            commands.entity(e).try_despawn();
        }
    }
}

#[cfg(test)]
mod chaos_tests {
    use super::*;
    use dhcook::format::DeedDef;

    /// Corvo's table in short: civilians (not weepers) +6 each, guards +6 every fourth,
    /// alarms +6 every fifth.
    fn corvo() -> DarknessDef {
        let deed = |stat: &str, threshold, tweak: &str, excluded: &[&str]| DeedDef { stat: stat.into(), threshold, value: 6, tweak: tweak.into(), excluded: excluded.iter().map(|s| s.to_string()).collect() };
        DarknessDef {
            levels: vec![0, 12, 48, 72, 96, 114, 138, 162, 168, 200, 390],
            deeds: vec![
                deed("ePlayerStat_NumKills", 1, "Pwn_Civ_Base", &["Pwn_Weeper_Base"]),
                deed("ePlayerStat_NumKills", 4, "Pwn_Guard_Base", &[]),
                deed("ePlayerStat_NumKills", 4, "Pwn_Weeper_Base", &[]),
                deed("ePlayerStat_NumAlarmsTriggered", 5, "", &[]),
            ],
        }
    }

    fn line(names: &[&str]) -> Vec<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn deeds_raise_the_score_and_level() {
        let dark = corvo();
        let mut s = PlayerStats::default();
        let civ = line(&["Pwn_CivMiddle_F_1", "Pwn_Civ_Base"]);
        let weeper = line(&["Pwn_WeeperMiddle_F_1", "Pwn_Weeper_Base", "Pwn_Civ_Base"]);
        let guard = line(&["Pwn_CityGuard_MTall_1", "Pwn_Guard_Base"]);
        notify_deed(&dark, &mut s, "ePlayerStat_NumKills", Some(&civ));
        assert_eq!((s.chaos_score, s.chaos_level), (6, 0));
        notify_deed(&dark, &mut s, "ePlayerStat_NumKills", Some(&civ));
        assert_eq!((s.chaos_score, s.chaos_level), (12, 1));
        // a weeper is no civilian: it counts towards the weepers' fourth
        notify_deed(&dark, &mut s, "ePlayerStat_NumKills", Some(&weeper));
        assert_eq!(s.chaos_score, 12);
        for k in 1..=4 {
            notify_deed(&dark, &mut s, "ePlayerStat_NumKills", Some(&guard));
            assert_eq!(s.chaos_score, if k < 4 { 12 } else { 18 }, "guard kill {k}");
        }
        // five alarms; mission end empties the counters and keeps the score
        for _ in 0..4 {
            notify_deed(&dark, &mut s, "ePlayerStat_NumAlarmsTriggered", None);
        }
        assert_eq!(s.chaos_score, 18);
        s.deed_counters.clear();
        notify_deed(&dark, &mut s, "ePlayerStat_NumAlarmsTriggered", None);
        assert_eq!(s.chaos_score, 18);
        // the scripts' points: never below 0
        add_chaos(&dark, &mut s, -100);
        assert_eq!((s.chaos_score, s.chaos_level), (0, 0));
        add_chaos(&dark, &mut s, 100000);
        assert_eq!(s.chaos_level, 10);
    }

    #[test]
    fn level_steps() {
        let levels = corvo().levels;
        assert_eq!(darkness_level(&levels, 0), 0);
        assert_eq!(darkness_level(&levels, 47), 1);
        assert_eq!(darkness_level(&levels, 48), 2);
        assert_eq!(darkness_level(&levels, 389), 9);
        assert_eq!(darkness_level(&[], 50), 0);
    }
}
