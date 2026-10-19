//! Pursuers following Corvo through a level transition, as the original's travel manager does
//! (`UDisNPCTravelManager`, decoded for this rewrite from the user's own Dishonored.exe:
//! leaving 0x00AE2300, arriving 0x00AE2C90, the delay 0x00AE3760, going home 0x00AE0AE0).
//! - As he leaves a map, each enemy hunting him there (its attention busted, fighting him) goes
//!   after him, unless it is down, choked, being assassinated or burning
//!   (`m_DisallowedNPCStates`); one frozen by Bend Time, carrying a key or held to a tether he is
//!   outside of stays and forgets him. One takes d / (its run speed x
//!   `m_fTravelingNPCSpeedModifier` 1.5) + `m_fTravelingNPCDelay` (5 s) to come through after
//!   him (d: how far it was from him).
//! - Where he comes in, the map's travel spawners (`DisTravelSpawner`) that list his
//!   destination's tag (`m_PlayerTravelLocationTags`) and make the same kind of character bring
//!   them in, one each, in turn, the nearest `m_MaxTravelingNPCs` (3) only; the rest stay home.
//!   They come in hunting him.
//! - One that loses him walks back to where it came in and goes home through the door: the
//!   next time he is in its home map it stands where it was, its memory of him gone. One that
//!   dies stays where it fell. Chased back home, it comes through as itself.

use crate::gameplay::Campaign;
use crate::level::{GameAssets, LevelInfo};
use crate::npc::{spawn_npc, Alert, FromSpawner, Mode, Npc};
use crate::player::Player;
use crate::world_light::WorldLighting;
use crate::{Config, GameState};
use bevy::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

pub struct TravelPlugin;

impl Plugin for TravelPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Arrivals>()
            .add_systems(OnEnter(GameState::InGame), plan_arrivals.after(crate::level::LevelSpawnSet).before(crate::save::return_to_level))
            .add_systems(Update, depart.after(crate::save::save_game).run_if(in_state(GameState::InGame)))
            .add_systems(Update, (come_through, arrived, pursuers, rejoin).chain().run_if(in_state(GameState::InGame)));
    }
}

/// `DisTweaks_PlayerPawn`'s `m_MaxTravelingNPCs`, `m_fTravelingNPCSpeedModifier`,
/// `m_fTravelingNPCDelay` (Corvo's).
const MAX_TRAVELING: usize = 3;
const SPEED_MODIFIER: f32 = 1.5;
const DELAY: f32 = 5.0;

/// A pursuer's trip (`DisNPCTravelInfo`).
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct Trip {
    pub id: u32,
    /// the map it lives in, and its spawner there
    pub home: String,
    pub spawner: u32,
    /// the kind of character it is (`m_pPawnTweaks`): a travel spawner making the same brings it in
    pub pawn: String,
    pub health: f32,
    /// the map it is in, or on its way to
    pub visiting: String,
    /// seconds before it comes through (`m_fTimeToSpawn`)
    pub time: f32,
    /// on its way in (`m_bAwaitingArrival`)
    pub awaiting: bool,
    /// its saved self, as its home map's kept state had it (put back there when it goes home)
    pub body: Option<String>,
    /// the travel spawner it came in by (in the map it visits)
    pub arrival: Option<u32>,
}

/// A character visiting from another map after Corvo (its trip).
#[derive(Component)]
pub struct Pursuer(pub u32);

/// Coming through: where, and how it comes in.
#[derive(Component)]
struct Arrived {
    at: Vec3,
    yaw: f32,
    /// from its own spawner's place (chased back home)
    from: Option<Vec3>,
    health: f32,
    corvo: Vec3,
    visiting: bool,
}

/// The pursuers due in this map: (trip, travel spawner).
#[derive(Resource, Default)]
struct Arrivals(Vec<(u32, u32)>);

/// Back home: its saved self put back where it was, its memory of Corvo gone.
fn go_home(campaign: &mut Campaign, trip: Trip) {
    if let Some(body) = &trip.body {
        crate::save::put_kept_npc(campaign, &trip.home, body);
    }
}

/// Corvo leaving the map: who follows him.
#[allow(clippy::type_complexity)]
fn depart(
    mut requests: MessageReader<crate::mission::TravelRequest>,
    level: Option<Res<LevelInfo>>,
    mut campaign: ResMut<Campaign>,
    (tc, ai): (Res<crate::gameplay::TimeControl>, Res<crate::aiworld::AiWorld>),
    player: Query<&Transform, With<Player>>,
    npcs: Query<(&Npc, &Transform, &FromSpawner, Option<&Pursuer>, Has<crate::npc::Electrocuted>)>,
) {
    let Some(r) = requests.read().last().cloned() else { return };
    let (Some(level), Ok(pt)) = (level, player.single()) else { return };
    let here = level.scene.name.clone();
    if r.map.eq_ignore_ascii_case(&here) {
        return;
    }
    let stopped = tc.bend_remaining > 0.0 && tc.world_dilation <= 0.0;
    let hunting = |n: &Npc| n.hostile() && n.alert == Alert::Combat && n.mode == Mode::Combat;
    if std::env::var("DH_TRAVEL_LOG").is_ok() {
        info!("travel: Corvo leaves {here} for {} ({} hunting him)", r.map, npcs.iter().filter(|(n, ..)| hunting(n)).count());
    }
    let time = |n: &Npc, t: &Transform| t.translation.distance(pt.translation) / (n.gait[2].max(0.5) * SPEED_MODIFIER) + DELAY;
    let log = std::env::var("DH_TRAVEL_LOG").is_ok();
    // those visiting here go on after him, or home
    for (n, t, from, p, burnt) in &npcs {
        let Some(Pursuer(id)) = p else { continue };
        let Some(i) = campaign.trips.iter().position(|tr| tr.id == *id) else { continue };
        if n.is_down() || n.mode == Mode::Choked || burnt {
            // (`m_TravelEndingNPCStates`: it stays here for good)
            campaign.trips.remove(i);
            continue;
        }
        crate::save::take_kept_npc(&mut campaign, &here, from.0);
        if hunting(n) && !(stopped && !n.out_of_bend) {
            let tr = &mut campaign.trips[i];
            tr.visiting = r.map.clone();
            tr.awaiting = true;
            tr.time = time(n, t);
            tr.health = n.health;
            if log {
                info!("travel: {} goes on after Corvo ({:.1} s)", n.name, tr.time);
            }
        } else {
            let tr = campaign.trips.remove(i);
            go_home(&mut campaign, tr);
        }
    }
    // those living here who hunt him follow
    for (n, t, from, p, burnt) in &npcs {
        if p.is_some() || !hunting(n) || n.is_down() || n.mode == Mode::Choked || burnt {
            continue;
        }
        let sp = level.scene.spawners.get(from.0 as usize);
        // (a key in its pocket: `DisKey_Base`, the keys' tweaks)
        let key = sp.and_then(|s| s.stealable).and_then(|i| level.scene.pickups.get(i as usize)).is_some_and(|k| {
            let kind = k.kind.to_ascii_lowercase();
            kind.contains("_keys.") || kind.contains(".key_")
        });
        let tethered = !n.tether.is_empty() && !ai.within_tether(&n.tether, pt.translation);
        if (stopped && !n.out_of_bend) || key || tethered {
            if log {
                info!("travel: {} stays behind (frozen {}, a key {key}, tethered {tethered})", n.name, stopped && !n.out_of_bend);
            }
            // (it stays, and forgets him)
            crate::save::calm_kept_npc(&mut campaign, &here, from.0);
            continue;
        }
        let body = crate::save::take_kept_npc(&mut campaign, &here, from.0);
        campaign.next_trip += 1;
        let id = campaign.next_trip;
        let tr = Trip {
            id,
            home: here.clone(),
            spawner: from.0,
            pawn: sp.map(|s| s.pawn.clone()).unwrap_or_default(),
            health: n.health,
            visiting: r.map.clone(),
            time: time(n, t),
            awaiting: true,
            body,
            arrival: None,
        };
        if log {
            info!("travel: {} follows Corvo to {} ({:.1} s: {:.1} m away, running {:.2} m/s)", n.name, r.map, tr.time, t.translation.distance(pt.translation), n.gait[2]);
        }
        campaign.trips.push(tr);
    }
}

/// Corvo in a map: who of those after him comes through, and by which travel spawner.
fn plan_arrivals(level: Option<Res<LevelInfo>>, config: Res<Config>, mut campaign: ResMut<Campaign>, mut arrivals: ResMut<Arrivals>) {
    arrivals.0.clear();
    let Some(level) = level else { return };
    let here = level.scene.name.clone();
    let tag = config.spawn_index.and_then(|i| level.scene.player_starts.get(i)).map(|s| s.tag.clone()).unwrap_or_default();
    // the travel spawners at the start he comes in at, by the kind they make
    let mut by_pawn: HashMap<String, Vec<u32>> = HashMap::new();
    if !tag.is_empty() {
        for (i, s) in level.scene.spawners.iter().enumerate() {
            if s.travel_tags.iter().any(|t| t.eq_ignore_ascii_case(&tag)) {
                by_pawn.entry(s.pawn.to_ascii_lowercase()).or_default().push(i as u32);
            }
        }
    }
    // those on their way here that a spawner can bring in, the nearest first
    let mut coming: Vec<(u32, f32)> = campaign
        .trips
        .iter()
        .filter(|t| t.awaiting && t.visiting.eq_ignore_ascii_case(&here) && by_pawn.contains_key(&t.pawn.to_ascii_lowercase()))
        .map(|t| (t.id, t.time))
        .collect();
    coming.sort_by(|a, b| a.1.total_cmp(&b.1));
    coming.truncate(MAX_TRAVELING);
    // the rest (and any bound elsewhere) stay home
    let staying: Vec<u32> = campaign.trips.iter().filter(|t| t.awaiting && !coming.iter().any(|c| c.0 == t.id)).map(|t| t.id).collect();
    for id in staying {
        if let Some(i) = campaign.trips.iter().position(|t| t.id == id) {
            let tr = campaign.trips.remove(i);
            go_home(&mut campaign, tr);
        }
    }
    // in turn by kind (`m_ArrivalSpawners`)
    let mut turn: HashMap<String, usize> = HashMap::new();
    for (id, _) in coming {
        let Some(tr) = campaign.trips.iter_mut().find(|t| t.id == id) else { continue };
        let kind = tr.pawn.to_ascii_lowercase();
        let list = &by_pawn[&kind];
        let k = turn.entry(kind).or_default();
        let sp = list[*k % list.len()];
        *k += 1;
        tr.arrival = Some(sp);
        arrivals.0.push((id, sp));
        if std::env::var("DH_TRAVEL_LOG").is_ok() {
            info!("travel: trip {id} comes in by {} in {:.1} s", level.scene.spawners[sp as usize].name, tr.time);
        }
    }
}

/// The delay run out: through the door.
#[allow(clippy::too_many_arguments)]
fn come_through(
    mut commands: Commands,
    time: Res<Time>,
    tc: Res<crate::gameplay::TimeControl>,
    (level, assets, mut wl, settings): (Option<Res<LevelInfo>>, Option<Res<GameAssets>>, Option<ResMut<WorldLighting>>, Res<crate::settings::Settings>),
    mut campaign: ResMut<Campaign>,
    mut arrivals: ResMut<Arrivals>,
    player: Query<&Transform, With<Player>>,
) {
    if arrivals.0.is_empty() {
        return;
    }
    let (Some(level), Some(assets), Ok(pt)) = (level, assets, player.single()) else { return };
    let dt = time.delta_secs() * tc.world_scale();
    let here = level.scene.name.clone();
    let mut left = Vec::new();
    for (id, sp) in std::mem::take(&mut arrivals.0) {
        let Some(i) = campaign.trips.iter().position(|t| t.id == id) else { continue };
        campaign.trips[i].time -= dt;
        if campaign.trips[i].time > 0.0 {
            left.push((id, sp));
            continue;
        }
        let Some(door) = level.scene.spawners.get(sp as usize) else { continue };
        let tr = campaign.trips[i].clone();
        // chased back home: itself, from its own spawner; else the travel spawner's
        let home = tr.home.eq_ignore_ascii_case(&here);
        let si = if home { tr.spawner } else { sp };
        let Some(e) = spawn_npc(&mut commands, &assets, &level.scene, wl.as_deref_mut(), si as usize, settings.difficulty) else { continue };
        let from = home.then(|| level.scene.spawners.get(tr.spawner as usize).map(|s| Vec3::from(s.position))).flatten();
        commands.entity(e).insert(Arrived { at: Vec3::from(door.position), yaw: door.yaw, from, health: tr.health, corvo: pt.translation, visiting: !home });
        if home {
            campaign.trips.remove(i);
        } else {
            campaign.trips[i].awaiting = false;
            commands.entity(e).insert(Pursuer(id));
        }
        if std::env::var("DH_TRAVEL_LOG").is_ok() {
            info!("travel: trip {id} comes through at {}", door.name);
        }
    }
    arrivals.0 = left;
}

/// Come through: at the door, hunting him (`m_SpawnAtSuspicionLevel`, MaxOutAttention at him, the
/// Traveled stimulus), its own health.
fn arrived(mut commands: Commands, mut npcs: Query<(Entity, &mut Npc, &mut Transform, &Arrived)>) {
    for (e, mut n, mut t, a) in &mut npcs {
        // (spawned at its spawner: moved to the door, its height kept)
        let off = a.from.map(|f| t.translation - f).unwrap_or(Vec3::ZERO);
        if a.from.is_some() {
            t.translation = a.at + off;
            n.yaw = a.yaw;
        }
        n.health = a.health.min(n.max_health).max(1.0);
        n.alert = Alert::Combat;
        n.awareness = 1.0;
        n.suspecting = true;
        n.last_seen = Some(a.corvo);
        n.set_mode(Mode::Combat);
        // (visiting: where it came in is where it goes back to, `ReturnTravelRequest`)
        if a.visiting {
            n.home = a.at;
            n.patrol = Default::default();
        }
        commands.entity(e).remove::<Arrived>();
    }
}

/// Visitors: one that dies stays (its trip ends); one that lost him and is back at its door goes
/// home through it (`DisBehaviorReturnTravel`).
fn pursuers(mut commands: Commands, mut campaign: ResMut<Campaign>, npcs: Query<(Entity, &Npc, &Transform, &Pursuer)>) {
    for (e, n, t, p) in &npcs {
        if n.mode == Mode::Dead {
            campaign.trips.retain(|tr| tr.id != p.0);
            commands.entity(e).remove::<Pursuer>();
            continue;
        }
        if n.alert == Alert::Unaware && n.mode == Mode::Idle && t.translation.with_y(0.0).distance(n.home.with_y(0.0)) < 1.5 {
            if std::env::var("DH_TRAVEL_LOG").is_ok() {
                info!("travel: {} goes home", n.name);
            }
            commands.entity(e).despawn();
            if let Some(i) = campaign.trips.iter().position(|tr| tr.id == p.0) {
                let tr = campaign.trips.remove(i);
                go_home(&mut campaign, tr);
            }
        }
    }
}

/// A save loaded in the map they visit: the visitors known again by the spawner they came in by.
fn rejoin(mut commands: Commands, level: Option<Res<LevelInfo>>, campaign: Res<Campaign>, npcs: Query<(Entity, &FromSpawner), (Added<FromSpawner>, Without<Pursuer>, Without<Arrived>)>) {
    let Some(level) = level else { return };
    for (e, from) in &npcs {
        if let Some(tr) = campaign.trips.iter().find(|t| !t.awaiting && t.arrival == Some(from.0) && t.visiting.eq_ignore_ascii_case(&level.scene.name) && !t.home.eq_ignore_ascii_case(&level.scene.name)) {
            commands.entity(e).insert(Pursuer(tr.id));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_nearest_three_come_through_in_turn_and_the_rest_go_home() {
        let mut app = App::new();
        let spawner = |name: &str, pawn: &str, tags: &[&str]| dhcook::format::Spawner { name: name.into(), pawn: pawn.into(), travel_tags: tags.iter().map(|t| t.to_string()).collect(), ..Default::default() };
        let mut scene = dhcook::format::Scene { name: "L_There_P".into(), ..Default::default() };
        scene.player_starts.push(dhcook::format::PlayerStart { tag: "PlayerStart_Door".into(), ..Default::default() });
        scene.spawners = vec![spawner("TS_0", "Guard", &["PlayerStart_Door"]), spawner("TS_1", "Guard", &["PlayerStart_Door"]), spawner("TS_2", "Elite", &["PlayerStart_Other"])];
        let trip = |id: u32, pawn: &str, time: f32| Trip { id, home: "L_Here_P".into(), pawn: pawn.into(), visiting: "L_There_P".into(), time, awaiting: true, ..Default::default() };
        let campaign = Campaign { trips: vec![trip(1, "Guard", 9.0), trip(2, "Guard", 6.0), trip(3, "Guard", 7.0), trip(4, "Guard", 12.0), trip(5, "Elite", 5.0)], ..Default::default() };
        app.insert_resource(LevelInfo { scene })
            .insert_resource(Config { map: String::new(), max_texture_size: 0, recook: false, spawn_index: Some(0) })
            .insert_resource(campaign)
            .init_resource::<Arrivals>()
            .add_systems(Update, plan_arrivals);
        app.update();
        let arrivals = &app.world().resource::<Arrivals>().0;
        // the three nearest guards, by the two guard spawners in turn; the far guard and the
        // elite (no spawner of its kind at this door) stay home
        assert_eq!(arrivals, &vec![(2, 0), (3, 1), (1, 0)]);
        let ids: Vec<u32> = app.world().resource::<Campaign>().trips.iter().map(|t| t.id).collect();
        assert_eq!(ids, vec![1, 2, 3]);
    }
}
