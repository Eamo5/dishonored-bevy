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
use dhcook::format::{KVal, Kismet as Graph};
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
    MissionEnd { tweak: String, darkness: i32 },
    ShowStats { tweak: String, op: u32 },
    Loadout(String),
    AbstractItem(String),
    Chapter(String),
    IncrementStat(String),
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

/// This mission's share of the chaos: lethal play against the people met. Killing a fifth
/// of them makes a mission high chaos; most of them, very high.
pub fn mission_chaos(s: &PlayerStats) -> i32 {
    let lethal = s.kills as f32 + s.civilians_killed as f32;
    let met = s.npcs_met.max(12) as f32;
    let r = lethal / met;
    if r >= 0.5 {
        2
    } else if r >= 0.2 {
        1
    } else {
        0
    }
}

/// Is the campaign high chaos now (the threshold of the mission being played)?
pub fn chaos_high(data: &Data, s: &PlayerStats, mission: i32) -> bool {
    let t = data.0.chaos_thresholds.get(mission.max(0) as usize).copied().unwrap_or(i32::MAX);
    s.chaos_level + mission_chaos(s) >= t.max(1)
}

/// A start loadout (`Twk_PlayerLoadout_*`): weapons, gadgets, runes, coins, powers, upgrades.
pub fn apply_loadout(data: &Data, name: &str, s: &mut PlayerStats) -> bool {
    apply_loadout_from(&data.0.loadouts, name, s)
}

pub fn apply_loadout_from(list: &[dhcook::format::LoadoutDef], name: &str, s: &mut PlayerStats) -> bool {
    let Some(l) = list.iter().find(|l| l.name.eq_ignore_ascii_case(name)) else { return false };
    s.weapons = l.items.iter().any(|i| i.contains("Crossbow") || i.contains("Pistol"));
    s.no_crossbow = s.weapons && !l.items.iter().any(|i| i.contains("Crossbow"));
    for i in &l.items {
        let gadget = if i.contains("Grenade") {
            Some(crate::gadgets::GRENADES)
        } else if i.contains("SpringRazor") {
            Some(crate::gadgets::RAZORS)
        } else {
            None
        };
        if let Some(g) = gadget {
            *s.items.entry(g.to_string()).or_default() += 2;
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
    vm.mission_chaos = mission_chaos(&stats);
    stats.chaos_high = chaos_high(&data, &stats, mission_number(&data, map));
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
            CampaignFx::MissionEnd { tweak, darkness } => {
                info!("campaign: mission {tweak} ends, chaos level {} -> {darkness}", stats.chaos_level);
                stats.chaos_level = darkness;
            }
            CampaignFx::Darkness(d) => stats.chaos_level = d,
            CampaignFx::ShowStats { tweak, op } => {
                show.write(crate::mission::ShowMissionStats { tweak, op });
            }
            CampaignFx::Loadout(name) => {
                let local = level.as_ref().is_some_and(|l| apply_loadout_from(&l.scene.loadouts, &name, &mut stats));
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
            CampaignFx::IncrementStat(s) => *stats.counters.entry(s).or_default() += 1,
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
