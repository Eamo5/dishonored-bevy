//! Save games: the map, the player, the level scripts' runtime state and the world changes
//! (NPCs, pickups taken, doors, shown / hidden actors, lights, actors moved by matinees).
//! Slots live in `%APPDATA%/DishonoredBevy/saves` (slot 0 is the quicksave: F5 / F9).

use crate::gameplay::{HudMessages, PlayerStats};
use crate::interact::{Door, Pickup};
use crate::kismet::{Vm, VmSave};
use crate::level::{GameAssets, LevelInfo, LevelInstance, LevelLight};
use crate::npc::{spawn_npc, Alert, FromSpawner, Mode, Npc};
use crate::player::Player;
use crate::world_light::WorldLighting;
use crate::{Config, GameState};
use bevy::prelude::*;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

pub struct SavePlugin;

impl Plugin for SavePlugin {
    fn build(&self, app: &mut App) {
        app.add_message::<SaveRequest>()
            .add_message::<LoadRequest>()
            .insert_resource(SaveSlots::scan())
            .add_systems(OnEnter(GameState::InGame), arm_pending)
            .add_systems(Update, (quick_keys, save_game, load_game, discard_level_states).chain().after(crate::kismet::apply_effects).run_if(in_state(GameState::InGame)))
            .add_systems(OnEnter(GameState::InGame), return_to_level.after(arm_pending).after(crate::level::LevelSpawnSet))
            .add_systems(Update, (apply_pending, restore_npcs).chain().run_if(in_state(GameState::InGame)));
    }
}

#[derive(Message, Clone, Copy)]
pub struct SaveRequest(pub usize);

/// The slot the level scripts' autosaves go to.
pub const AUTOSAVE_SLOT: usize = 9;

#[derive(Message, Clone, Copy)]
pub struct LoadRequest(pub usize);

const VERSION: u32 = 1;

#[derive(Serialize, Deserialize, Clone)]
pub struct NpcSave {
    spawner: u32,
    position: [f32; 3],
    yaw: f32,
    mode: Mode,
    alert: Alert,
    health: f32,
    awareness: f32,
    down_t: f32,
    route_idx: usize,
    /// the bones lost to a beheading (`gore::Severed`)
    #[serde(default)]
    severed: Vec<usize>,
    #[serde(default)]
    falling: Option<crate::carry::Falling>,
    #[serde(default)]
    consumed: bool,
    #[serde(default)]
    corpse_age: Option<f32>,
    /// Original passenger placement, before a cinematic vehicle's movement.
    #[serde(default)]
    ride_base: Option<[f32; 16]>,
}

#[derive(Serialize, Deserialize)]
pub struct SaveGame {
    version: u32,
    pub map: String,
    /// seconds since the Unix epoch
    pub saved_at: u64,
    pub play_time: f32,
    player: [f32; 3],
    yaw: f32,
    pitch: f32,
    crouched: bool,
    stats: PlayerStats,
    kismet: Option<VmSave>,
    npcs: Vec<NpcSave>,
    taken: Vec<u32>,
    #[serde(default)]
    ammo_pickups: Vec<crate::interact::AmmoPickupSave>,
    /// (instance, open, target, swing direction, locked)
    doors: Vec<(u32, f32, f32, f32, bool)>,
    /// instances shown / hidden differently from the level's start
    shown: Vec<(u32, bool)>,
    lights: Vec<(u32, bool)>,
    /// instances moved by matinees: (instance, translation, rotation, scale)
    moved: Vec<(u32, [f32; 3], [f32; 4], [f32; 3])>,
    /// river krusts killed: (krust, pearl taken)
    #[serde(default)]
    krusts: Vec<(u32, bool)>,
    /// tripwires and launchers: sprung (1) or disarmed (2)
    #[serde(default)]
    traps: Vec<(u32, u8)>,
    /// usable objects: the stage last entered, done with
    #[serde(default)]
    usables: Vec<(u32, usize, bool)>,
    /// usable objects locked / unlocked since the level began
    #[serde(default)]
    usable_locks: Vec<(u32, bool)>,
    /// pickups the scripts' factories made, still there: (pickup, where)
    #[serde(default)]
    factory_made: Vec<(u32, [f32; 3])>,
    /// props: (movable, `None` broken, else where a loose one lies)
    #[serde(default)]
    props: Vec<(u32, Option<([f32; 3], [f32; 4])>)>,
    #[serde(default)]
    held_prop: Option<u32>,
    #[serde(default)]
    carry: Option<crate::carry::CarrySave>,
    #[serde(default)]
    prop_states: Vec<crate::props::PropStateSave>,
    /// Rewiring, script-controlled power and each tank's remaining whale oil.
    /// Absent in older saves: retain the level's initial security state.
    #[serde(default)]
    security: Option<crate::security::DevicesSave>,
    /// Live projectiles, held grenades and deployed springrazors.
    #[serde(default)]
    gadgets: Option<crate::gadgets::GadgetsSave>,
    #[serde(default)]
    projectiles: Vec<crate::powers::ProjectileSave>,
    #[serde(default)]
    powers: Option<crate::powers::PowersSave>,
    #[serde(default)]
    swarms: Option<crate::swarm::SwarmsSave>,
    #[serde(default)]
    fish: Option<crate::fish::FishSaveWorld>,
    #[serde(default)]
    krust_spit: Option<Vec<crate::krust::SpitSave>>,
    #[serde(default)]
    krust_state: Option<Vec<crate::krust::KrustSave>>,
    #[serde(default)]
    trap_state: Option<crate::traps::TrapsSave>,
    #[serde(default)]
    possession: Option<crate::possession::PossessionSave>,
    /// what the scripts set on the characters: senses, health, who stands how with whom
    #[serde(default)]
    overrides: crate::script_world::OverridesSave,
    /// the scripts' cinematic mode: (hide the HUD, hold Corvo, hide his arms)
    #[serde(default)]
    cinematic: Option<(bool, bool, bool)>,
    /// Active ride's script operation and the player's pre-ride placement.
    #[serde(default)]
    player_ride: Option<(u32, [f32; 16])>,
    /// the HUD's parts the scripts had hidden (`DHE_Health` until Corvo wakes in his cell)
    #[serde(default)]
    hud_hidden: Option<Vec<String>>,
    /// what Corvo took at the Hound Pits on earlier visits (`Campaign::hub_taken`)
    #[serde(default)]
    hub_taken: Vec<String>,
    /// the doors broken (Wind Blast, blasts)
    #[serde(default)]
    broken_doors: Vec<u32>,
    /// the maps' states kept for a return to them (`Campaign::level_states`)
    #[serde(default)]
    level_states: Vec<(String, Vec<String>, String)>,
}

/// What each slot holds (map, time), for the menus.
#[derive(Resource, Default)]
pub struct SaveSlots {
    slots: Vec<(usize, String, String, u64)>,
}

fn saves_dir() -> PathBuf {
    let d = crate::settings::user_dir().join("saves");
    let _ = std::fs::create_dir_all(&d);
    d
}

fn slot_path(slot: usize) -> PathBuf {
    saves_dir().join(if slot == 0 { "quicksave.json".to_string() } else { format!("slot{slot}.json") })
}

/// Keep the previous slot intact until the replacement has been fully written.
/// The temporary file is beside the destination so the rename stays on one volume.
fn replace_save(path: &std::path::Path, write: impl FnOnce(&mut std::fs::File) -> std::io::Result<()>) -> std::io::Result<()> {
    let pending = path.with_extension("json.tmp");
    let result = (|| {
        let mut file = std::fs::File::create(&pending)?;
        write(&mut file)?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&pending, path)
    })();
    if result.is_err() { let _ = std::fs::remove_file(&pending); }
    result
}

#[cfg(test)]
mod write_tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn failed_save_keeps_previous_slot_and_successful_save_replaces_it() {
        let dir = std::env::temp_dir().join("opencode").join(format!("dishonored-save-{}-{}", std::process::id(), rand::random::<u64>()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("slot1.json");
        let original = br#"{"checkpoint":"original"}"#;
        std::fs::write(&path, original).unwrap();
        let result = replace_save(&path, |file| {
            file.write_all(b"{\"checkpoint\":")?;
            Err(std::io::Error::other("simulated interrupted write"))
        });
        assert!(result.is_err());
        assert_eq!(std::fs::read(&path).unwrap(), original);
        assert!(!path.with_extension("json.tmp").exists());
        // A leftover file from an interrupted process is safely superseded.
        std::fs::write(path.with_extension("json.tmp"), b"partial").unwrap();
        let replacement = br#"{"checkpoint":"replacement"}"#;
        replace_save(&path, |file| file.write_all(replacement)).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), replacement);
        assert!(!path.with_extension("json.tmp").exists());
        let new_slot = dir.join("slot2.json");
        replace_save(&new_slot, |file| file.write_all(replacement)).unwrap();
        assert_eq!(std::fs::read(&new_slot).unwrap(), replacement);
        std::fs::remove_dir_all(dir).unwrap();
    }
}

/// A readable mission name for a map.
pub fn mission_name(map: &str) -> String {
    let m = map.to_ascii_lowercase();
    let names = [
        ("l_tower_p", "Dishonored"),
        ("l_prison", "Coldridge Prison"),
        ("l_prsnsewer", "Coldridge Prison"),
        // the Outsider's dream happens on the first night at the Hound Pits
        ("l_outsiderdream", "The Hound Pits Pub"),
        ("l_pub_assault", "The Loyalists"),
        ("l_pub", "The Hound Pits Pub"),
        ("l_streets", "High Overseer Campbell"),
        ("l_distillery", "High Overseer Campbell"),
        ("l_ovrsr", "High Overseer Campbell"),
        ("l_brothel", "House of Pleasure"),
        ("l_boyle", "Lady Boyle's Last Party"),
        ("l_bridge", "The Royal Physician"),
        ("l_galvani", "The Royal Physician"),
        ("l_artdealer", "The Royal Physician"),
        ("l_towerrtrn", "Return to the Tower"),
        ("l_flooded", "The Flooded District"),
        ("l_out", "The Loyalists"),
        ("l_isl", "The Light at the End"),
        ("l_lighth", "The Light at the End"),
    ];
    names.iter().find(|(k, _)| m.starts_with(k)).map(|(_, n)| n.to_string()).unwrap_or_else(|| map.to_string())
}

/// The Hound Pits between missions (and the Outsider's dream): no mission ends there.
pub fn is_hub(map: &str) -> bool {
    let m = map.to_ascii_lowercase();
    (m.starts_with("l_pub") && !m.starts_with("l_pub_assault")) || m.starts_with("l_outsiderdream")
}

/// Where a mission sits in the campaign (0: the prologue).
pub fn mission_index(map: &str) -> usize {
    const ORDER: [&str; 11] = [
        "Dishonored",
        "Coldridge Prison",
        "The Hound Pits Pub",
        "High Overseer Campbell",
        "House of Pleasure",
        "The Royal Physician",
        "Lady Boyle's Last Party",
        "Return to the Tower",
        "The Flooded District",
        "The Loyalists",
        "The Light at the End",
    ];
    let m = mission_name(map);
    ORDER.iter().position(|n| *n == m).unwrap_or(0)
}

impl SaveSlots {
    pub fn scan() -> SaveSlots {
        let mut slots = Vec::new();
        for slot in 0..=AUTOSAVE_SLOT {
            #[derive(Deserialize)]
            struct Head {
                map: String,
                saved_at: u64,
            }
            let Ok(d) = std::fs::read(slot_path(slot)) else { continue };
            let Ok(h) = serde_json::from_slice::<Head>(&d) else { continue };
            slots.push((slot, h.map.clone(), mission_name(&h.map), h.saved_at));
        }
        SaveSlots { slots }
    }
    pub fn any(&self) -> bool {
        !self.slots.is_empty()
    }
    /// A slot's save: its map, mission name, time saved.
    pub fn info(&self, slot: usize) -> Option<(&str, &str, u64)> {
        self.slots.iter().find(|s| s.0 == slot).map(|s| (s.1.as_str(), s.2.as_str(), s.3))
    }
    /// Changes when a save is written.
    pub fn stamp(&self) -> u64 {
        self.slots.iter().map(|s| s.3 ^ s.0 as u64).fold(0, |a, b| a.wrapping_add(b))
    }
    /// The most recent save.
    pub fn latest(&self) -> Option<usize> {
        self.slots.iter().max_by_key(|s| s.3).map(|s| s.0)
    }
    pub fn describe(&self, slot: usize) -> Option<String> {
        let s = self.slots.iter().find(|s| s.0 == slot)?;
        let name = match slot {
            0 => "Quicksave".to_string(),
            AUTOSAVE_SLOT => "Autosave".to_string(),
            _ => format!("Slot {slot}"),
        };
        Some(format!("{name}  -  {}  -  {}", s.2, ago(s.3)))
    }
    pub fn list(&self) -> Vec<(usize, String)> {
        let mut v: Vec<_> = self.slots.iter().map(|s| (s.0, s.3)).collect();
        v.sort_by(|a, b| b.1.cmp(&a.1));
        v.into_iter().filter_map(|(slot, _)| self.describe(slot).map(|d| (slot, d))).collect()
    }
}

fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn ago(t: u64) -> String {
    let s = now().saturating_sub(t);
    match s {
        0..=59 => "just now".into(),
        60..=3599 => format!("{} min ago", s / 60),
        3600..=86399 => format!("{} h ago", s / 3600),
        _ => format!("{} days ago", s / 86400),
    }
}

fn quick_keys(
    (keys, bind): (Res<ButtonInput<KeyCode>>, Res<crate::bindings::Bindings>),
    menu: Res<crate::menu::Menu>,
    mut save: MessageWriter<SaveRequest>,
    mut load: MessageWriter<LoadRequest>,
    slots: Res<SaveSlots>,
) {
    if menu.open.is_some() {
        return;
    }
    if keys.just_pressed(bind.key(crate::bindings::Act::QuickSave)) {
        save.write(SaveRequest(0));
    }
    if keys.just_pressed(bind.key(crate::bindings::Act::QuickLoad)) && slots.slots.iter().any(|s| s.0 == 0) {
        load.write(LoadRequest(0));
    }
}

#[allow(clippy::too_many_arguments, clippy::type_complexity)]
fn save_game(
    mut requests: MessageReader<SaveRequest>,
    level: Option<Res<LevelInfo>>,
    mut vm: Option<ResMut<Vm>>,
    stats: Res<PlayerStats>,
    time: Res<Time>,
    mut slots: ResMut<SaveSlots>,
    mut msgs: ResMut<HudMessages>,
    player: Query<(&Transform, &Player)>,
    npcs: Query<(Entity, &Npc, &FromSpawner, &Transform, Option<&crate::gore::Severed>, Option<&crate::carry::Falling>, Has<crate::npc::ConsumedBody>)>,
    pickups: Query<(&Pickup, &Transform)>,
    instances: Query<(&LevelInstance, &Visibility, &Transform, Option<&Door>)>,
    lights: Query<(&LevelLight, &Visibility)>,
    (krusts, props, traps, usables, prop_bodies): (Res<crate::krust::KrustLog>, Query<(Entity, &crate::props::Prop, &Transform), Without<Player>>, Res<crate::traps::TrapLog>, Res<crate::usables::UsableLog>, Query<(&bevy_rapier3d::prelude::Velocity, &bevy_rapier3d::prelude::GravityScale, &bevy_rapier3d::prelude::RigidBody)>),
    (overrides, devices, grenades, razors, held, npc_ids, projectiles, swarms, rats, bites, possession, possess_overrides, fish, krust_hosts, fish_meals): (
        Res<crate::script_world::SpawnerOverrides>, Res<crate::security::Devices>,
        Query<(Entity, &crate::gadgets::Grenade, &Transform)>, Query<(&crate::gadgets::Razor, &Transform)>,
        Res<crate::props::Held>, Query<(Entity, &FromSpawner), With<Npc>>,
        Query<(&crate::powers::Projectile, &Transform)>,
        Query<(&crate::swarm::Swarm, &Transform)>,
        Query<(Entity, &crate::swarm::Rat, &Transform, Option<&crate::swarm::WhiteRat>)>,
        Res<crate::swarm::RatBites>,
        Res<crate::possession::Possession>, Res<crate::possession::PossessOverrides>,
        Query<(Entity, &crate::fish::Fish, &Transform, Option<&crate::anim::Animator>)>, Query<(Entity, &crate::krust::Krust, Option<&crate::anim::Animator>)>, Res<crate::fish::FishMeals>,
    ),
    (cine, script_ui, mut campaign, powers, tc, carry, matinee, krust_spit, trap_parts, darts): (Res<crate::script_world::Cinematic>, Option<Res<crate::kismet::ScriptUi>>, ResMut<crate::gameplay::Campaign>, Res<crate::powers::Powers>, Res<crate::gameplay::TimeControl>, Res<crate::carry::Carry>, Res<crate::matinee::MatineeState>, Query<(&crate::krust::Spit, &Transform)>, Query<(&crate::traps::TrapPart, Option<&crate::anim::Animator>)>, Query<(&crate::traps::Dart, &Transform)>),
    mut saving: MessageWriter<crate::globalui::ShowSaving>,
) {
    // the scripts keeping the map's state for a return (`DisSeqAct_SaveLevelState`; a partial
    // one, the Hound Pits': only its pickups, `Campaign::hub_taken`)
    let keep = vm.as_mut().and_then(|v| v.level_state_save.take()).filter(|partial| !partial);
    let slot = requests.read().last().map(|r| r.0);
    if slot.is_none() && keep.is_none() {
        return;
    }
    let Some(level) = level else { return };
    let Ok((pt, p)) = player.single() else { return };
    if stats.dead && slot.is_some() {
        msgs.push("Can't save now");
        return;
    }
    let scene = &level.scene;
    let present: std::collections::HashSet<u32> = pickups.iter().map(|(p, _)| p.index).collect();
    let mut s = SaveGame {
        version: VERSION,
        map: scene.name.clone(),
        saved_at: now(),
        play_time: time.elapsed_secs(),
        player: pt.translation.to_array(),
        yaw: p.yaw,
        pitch: p.pitch,
        crouched: p.crouched,
        stats: stats.clone(),
        kismet: vm.as_ref().map(|v| v.save_state()),
        krusts: krusts.dead.iter().map(|(k, v)| (*k, *v)).collect(),
        traps: traps.state.iter().map(|(k, v)| (*k, *v)).collect(),
        overrides: overrides.save(),
        cinematic: cine.on.then_some((cine.hide_hud, cine.hold, cine.hide_player)),
        hud_hidden: script_ui.map(|u| u.hud_hidden.iter().cloned().collect()),
        hub_taken: campaign.hub_taken.iter().cloned().collect(),
        broken_doors: Vec::new(),
        level_states: Vec::new(),
        usables: usables.entered.iter().map(|(k, (s, d))| (*k, *s, *d)).collect(),
        usable_locks: usables.locks.iter().map(|(k, l)| (*k, *l)).collect(),
        factory_made: pickups.iter().filter(|(p, _)| level.scene.pickups.get(p.index as usize).is_some_and(|sp| sp.factory)).map(|(p, t)| (p.index, t.translation.to_array())).collect(),
        ammo_pickups: pickups.iter().filter_map(|(p, t)| match &p.kind {
            crate::interact::PickupKind::Ammo(amounts) | crate::interact::PickupKind::Weapon(_, amounts) => Some(crate::interact::AmmoPickupSave { index: p.index, position: t.translation.to_array(), amounts: amounts.clone() }),
            _ => None,
        }).collect(),
        security: Some(devices.save(props.iter().map(|(e, p, _)| (e, p.index)))),
        gadgets: Some(crate::gadgets::GadgetsSave::capture(grenades.iter(), razors.iter(), npc_ids.iter().map(|(e, s)| (e, s.0)), held.0)),
        projectiles: crate::powers::save_projectiles(projectiles.iter(), npc_ids.iter().map(|(e, s)| (e, s.0))),
        powers: Some(crate::powers::PowersSave::capture(&powers, &tc)),
        swarms: Some(crate::swarm::SwarmsSave::capture(swarms.iter(), rats.iter(), npc_ids.iter().map(|(e, id)| (e, id.0)), bites.0, possession.host)),
        fish: Some(crate::fish::FishSaveWorld::capture(fish.iter().map(|(_, f, t, a)| (f, t, a)), &fish_meals, npc_ids.iter().map(|(e, id)| (e, id.0)))),
        krust_spit: Some(crate::krust::save_spits(krust_spit.iter(), krust_hosts.iter().map(|(_, k, _)| k))),
        krust_state: Some(krust_hosts.iter().map(|(_, k, a)| crate::krust::KrustSave::capture(k, a)).collect()),
        trap_state: Some(crate::traps::TrapsSave::capture(trap_parts.iter(), darts.iter())),
        possession: Some(crate::possession::PossessionSave::capture(&possession, &possess_overrides,
            npc_ids.iter().map(|(e, id)| (e, crate::possession::SavedHost::Npc(id.0)))
                .chain(rats.iter().map(|(e, _, _, _)| (e, crate::possession::SavedHost::Rat)))
                .chain(fish.iter().map(|(e, f, _, _)| (e, crate::possession::SavedHost::Fish(f.index()))))
                .chain(krust_hosts.iter().map(|(e, k, _)| (e, crate::possession::SavedHost::Krust(k.index())))))),
        held_prop: held.0.and_then(|e| props.get(e).ok()).map(|(_, p, _)| p.index as u32),
        carry: carry.save(),
        player_ride: matinee.saved_player_ride(),
        prop_states: props.iter().map(|(_, p, _)| p.save_state(p.body().0.and_then(|b| prop_bodies.get(b).ok()))).collect(),
        props: {
            // the broken (no longer there) and where the loose ones lie
            let alive: std::collections::HashMap<usize, &Transform> = props.iter().map(|(_, p, t)| (p.index, t)).collect();
            (0..level.scene.movables.len())
                .filter_map(|i| match alive.get(&i) {
                    None => Some((i as u32, None)),
                    Some(t) if !level.scene.movables[i].fixed => Some((i as u32, Some((t.translation.to_array(), t.rotation.to_array())))),
                    Some(_) => None,
                })
                .collect()
        },
        npcs: npcs
            .iter()
            .map(|(entity, n, f, t, sev, falling, consumed)| NpcSave {
                spawner: f.0,
                position: t.translation.to_array(),
                yaw: n.yaw,
                mode: n.mode,
                alert: n.alert,
                health: n.health,
                awareness: n.awareness,
                down_t: n.down_t,
                route_idx: n.route_idx,
                severed: sev.map(|s| s.0.clone()).unwrap_or_default(),
                falling: falling.cloned(),
                consumed,
                corpse_age: Some(n.corpse_age),
                ride_base: matinee.saved_ride_base(entity),
            })
            .collect(),
        taken: (0..scene.pickups.len() as u32).filter(|i| !present.contains(i)).collect(),
        doors: Vec::new(),
        shown: Vec::new(),
        lights: Vec::new(),
        moved: Vec::new(),
    };
    for (li, vis, t, door) in &instances {
        let Some(inst) = scene.instances.get(li.index as usize) else { continue };
        let visible = *vis != Visibility::Hidden;
        if visible != inst.visible {
            s.shown.push((li.index, visible));
        }
        if let Some(d) = door {
            s.doors.push((li.index, d.open, d.target, d.dir, d.locked));
            if d.broken {
                s.broken_doors.push(li.index);
            }
        } else if inst.dynamic {
            let m = Mat4::from_cols_array(&inst.transform);
            let (sc, r, tr) = m.to_scale_rotation_translation();
            if tr.distance(t.translation) > 1e-3 || r.angle_between(t.rotation) > 1e-3 {
                let _ = sc;
                s.moved.push((li.index, t.translation.to_array(), t.rotation.to_array(), t.scale.to_array()));
            }
        }
    }
    for (ll, vis) in &lights {
        let on = *vis != Visibility::Hidden;
        if scene.lights.get(ll.0 as usize).is_some_and(|l| l.enabled != on) {
            s.lights.push((ll.0, on));
        }
    }
    if keep.is_some() {
        let map = s.map.clone();
        if let Ok(json) = serde_json::to_string(&s) {
            campaign.level_states.retain(|(m, _, _)| !m.eq_ignore_ascii_case(&map));
            campaign.level_states.push((map.clone(), scene.packages.clone(), json));
            info!("level state kept for {map} ({} NPCs)", s.npcs.len());
        }
    }
    let Some(slot) = slot else { return };
    s.level_states = campaign.level_states.clone();
    match serde_json::to_vec(&s).map_err(anyhow::Error::from).and_then(|d| {
        use std::io::Write;
        Ok(replace_save(&slot_path(slot), |file| file.write_all(&d))?)
    }) {
        Ok(()) => {
            saving.write(crate::globalui::ShowSaving);
            msgs.push(if slot == 0 { "Quicksaved".to_string() } else { format!("Saved to slot {slot}") });
            *slots = SaveSlots::scan();
        }
        Err(e) => {
            warn!("save {slot}: {e:#}");
            msgs.push("Save failed");
        }
    }
}

/// A save game being loaded: applied once its map is up (or a map's kept state, on a return
/// to it: the world only).
#[derive(Resource)]
struct PendingLoad {
    save: SaveGame,
    frames: u32,
    armed: bool,
    level_only: bool,
}

/// The scripts dropping maps' kept states (`DisSeqAct_DiscardLevelState` by map or sublevel,
/// `DisSeqAct_DiscardAllLevelStates`); the Hound Pits' own drop what Corvo took there.
fn discard_level_states(vm: Option<ResMut<Vm>>, mut campaign: ResMut<crate::gameplay::Campaign>) {
    let Some(mut vm) = vm else { return };
    for name in std::mem::take(&mut vm.level_state_discards) {
        if name == "*" {
            campaign.level_states.clear();
            campaign.hub_taken.clear();
            continue;
        }
        campaign.level_states.retain(|(m, pkgs, _)| !m.eq_ignore_ascii_case(&name) && !pkgs.iter().any(|p| p.eq_ignore_ascii_case(&name)));
        if name.to_ascii_lowercase().starts_with("l_pub") {
            campaign.hub_taken.clear();
        }
    }
}

/// Back in a map whose state was kept: as Corvo left it.
fn return_to_level(mut commands: Commands, level: Option<Res<LevelInfo>>, campaign: Res<crate::gameplay::Campaign>, pending: Option<Res<PendingLoad>>) {
    let Some(level) = level else { return };
    if pending.is_some() {
        return;
    }
    let Some((_, _, json)) = campaign.level_states.iter().find(|(m, _, _)| m.eq_ignore_ascii_case(&level.scene.name)) else { return };
    match serde_json::from_str::<SaveGame>(json) {
        Ok(save) => {
            info!("level state: back in {} as it was left", level.scene.name);
            commands.insert_resource(PendingLoad { save, frames: 0, armed: true, level_only: true });
        }
        Err(e) => warn!("level state for {}: {e:#}", level.scene.name),
    }
}

fn load_game(
    mut commands: Commands,
    mut requests: MessageReader<LoadRequest>,
    mut config: ResMut<Config>,
    mut next: ResMut<NextState<GameState>>,
    mut msgs: ResMut<HudMessages>,
) {
    let Some(slot) = requests.read().last().map(|r| r.0) else { return };
    let save: SaveGame = match std::fs::read(slot_path(slot)).map_err(anyhow::Error::from).and_then(|d| Ok(serde_json::from_slice(&d)?)) {
        Ok(s) => s,
        Err(e) => {
            warn!("load {slot}: {e:#}");
            msgs.push("Load failed");
            return;
        }
    };
    config.map = save.map.clone();
    config.spawn_index = None;
    commands.insert_resource(PendingLoad { save, frames: 0, armed: false, level_only: false });
    next.set(GameState::Loading);
}

fn arm_pending(pending: Option<ResMut<PendingLoad>>) {
    if let Some(mut p) = pending {
        p.armed = true;
        p.frames = 0;
    }
}

/// An NPC to put back into its saved state once spawned.
#[derive(Component)]
pub(crate) struct RestoreNpc(NpcSave);

#[allow(clippy::too_many_arguments, clippy::type_complexity)]
fn apply_pending(
    mut commands: Commands,
    pending: Option<ResMut<PendingLoad>>,
    level: Option<Res<LevelInfo>>,
    assets: Option<Res<GameAssets>>,
    mut wl: Option<ResMut<WorldLighting>>,
    vm: Option<ResMut<Vm>>,
    mut stats: ResMut<PlayerStats>,
    mut player: Query<(&mut Transform, &mut Player, &mut bevy_rapier3d::prelude::Collider)>,
    npcs: Query<Entity, With<Npc>>,
    (pickups, mut ammo_pickups): (Query<(Entity, &Pickup)>, ResMut<crate::interact::AmmoPickupRestore>),
    mut instances: Query<(Entity, &LevelInstance, &mut Visibility, &mut Transform, Option<&mut Door>, Option<&crate::level::InstanceCollider>), Without<Player>>,
    mut lights: Query<(&LevelLight, &mut Visibility), (Without<LevelInstance>, Without<Player>)>,
    (mut krusts, mut prop_restore, mut traps, mut usables, mut carry_restore, mut krust_spit, mut krust_state, mut trap_state): (ResMut<crate::krust::KrustLog>, ResMut<crate::props::PropRestore>, ResMut<crate::traps::TrapLog>, ResMut<crate::usables::UsableLog>, ResMut<crate::carry::CarryRestore>, ResMut<crate::krust::SpitRestore>, ResMut<crate::krust::KrustRestore>, ResMut<crate::traps::TrapRestore>),
    (mut overrides, mut devices, props, mut gadgets, mut projectiles, mut swarms, mut possession, mut fish): (ResMut<crate::script_world::SpawnerOverrides>, ResMut<crate::security::Devices>, Query<(Entity, &crate::props::Prop)>, ResMut<crate::gadgets::GadgetRestore>, ResMut<crate::powers::ProjectileRestore>, ResMut<crate::swarm::SwarmRestore>, ResMut<crate::possession::PossessionRestore>, ResMut<crate::fish::FishRestore>),
    (mut cine, mut matinee, mut collider_tfs, script_ui, mut campaign, settings, mut cinematic_fade, mut powers, mut tc): (
        ResMut<crate::script_world::Cinematic>,
        ResMut<crate::matinee::MatineeState>,
        Query<&mut Transform, (With<bevy_rapier3d::prelude::Collider>, Without<LevelInstance>, Without<Player>, Without<Npc>)>,
        Option<ResMut<crate::kismet::ScriptUi>>,
        ResMut<crate::gameplay::Campaign>,
        Res<crate::settings::Settings>,
        ResMut<crate::matinee::CinematicFade>,
        ResMut<crate::powers::Powers>,
        ResMut<crate::gameplay::TimeControl>,
    ),
) {
    let Some(mut p) = pending else { return };
    if !p.armed {
        return;
    }
    // let the level scripts' start-up run first, then replace their results
    p.frames += 1;
    if p.frames < 4 {
        return;
    }
    let (Some(level), Some(assets)) = (level, assets) else { return };
    commands.remove_resource::<PendingLoad>();
    let level_only = p.level_only;
    let s = &mut p.save;
    if !s.map.eq_ignore_ascii_case(&level.scene.name) {
        return;
    }
    // (a kept level state: the world as it was left, Corvo and the story as they are now)
    if !level_only {
        if let Some(saved) = s.powers.take() {
            saved.restore(&mut powers, &mut tc);
        }
        if let Ok((mut t, mut pl, mut collider)) = player.single_mut() {
            t.translation = Vec3::from(s.player);
            pl.yaw = s.yaw;
            pl.pitch = s.pitch;
            pl.velocity = Vec3::ZERO;
            pl.locked = powers.blink.is_some();
            pl.crouched = s.crouched;
            pl.eye_height = if s.crouched { crate::player::CROUCH_EYE } else { crate::player::STAND_EYE };
            // The saved position is the capsule's centre. Restore its size as
            // well, so loading in a low passage does not stand Corvo into the roof.
            *collider = bevy_rapier3d::prelude::Collider::capsule_y(if s.crouched { crate::player::CROUCH_HALF } else { crate::player::STAND_HALF }, crate::player::RADIUS);
        }
        *stats = s.stats.clone();
    }
    let factory_made = std::mem::take(&mut s.factory_made);
    if let (Some(mut vm), Some(k)) = (vm, s.kismet.take()) {
        // the factories' pickups that were lying about
        for (p, at) in factory_made {
            vm.ai_fx.push(crate::script_world::AiFx::SpawnPickup { pickup: p, at: Vec3::from(at) });
        }
        let ok = if level_only { vm.restore_level_state(k) } else { vm.restore_state(k) };
        if !ok {
            warn!("save: the level scripts changed since this save; their state was not restored");
        }
        vm.darkness = stats.chaos_level;
    }
    if !level_only {
        // (the start-up's cinematic, e.g. waking in the cell, gives way to the save's)
        matinee.restore_player_ride(s.player_ride);
        *cine = match s.cinematic {
            Some((hide_hud, hold, hide_player)) => crate::script_world::Cinematic { on: true, hide_hud, hold, hide_player, since: 0.0 },
            None => crate::script_world::Cinematic::default(),
        };
        if let Some(mut ui) = script_ui {
            ui.restore(s.hud_hidden.take().unwrap_or_default().into_iter().collect());
        }
        // The opening matinee may have written black earlier this frame. Clear
        // that override together with ScriptUi, otherwise it reinstates black
        // until the eight-second fallback fade despite the saved scene being live.
        cinematic_fade.0 = None;
    }
    // (before the characters come back: they take it as they spawn)
    overrides.load(std::mem::take(&mut s.overrides));
    for e in &npcs {
        commands.entity(e).despawn();
    }
    for n in &s.npcs {
        if let Some(e) = spawn_npc(&mut commands, &assets, &level.scene, wl.as_deref_mut(), n.spawner as usize, settings.difficulty) {
            commands.entity(e).insert(RestoreNpc(n.clone()));
        }
    }
    if !level_only {
        campaign.hub_taken = s.hub_taken.iter().cloned().collect();
        campaign.level_states = std::mem::take(&mut s.level_states);
    }
    let taken: std::collections::HashSet<u32> = s.taken.iter().copied().collect();
    for (e, pk) in &pickups {
        if taken.contains(&pk.index) {
            for &m in &pk.entities {
                commands.entity(m).despawn();
            }
            commands.entity(e).despawn();
        }
    }
    let shown: std::collections::HashMap<u32, bool> = s.shown.iter().copied().collect();
    let doors: std::collections::HashMap<u32, (f32, f32, f32, bool)> = s.doors.iter().map(|d| (d.0, (d.1, d.2, d.3, d.4))).collect();
    let moved: std::collections::HashMap<u32, ([f32; 3], [f32; 4], [f32; 3])> = s.moved.iter().map(|m| (m.0, (m.1, m.2, m.3))).collect();
    let broken: std::collections::HashSet<u32> = s.broken_doors.iter().copied().collect();
    for (e, li, mut vis, mut t, door, col) in &mut instances {
        if let Some(&v) = shown.get(&li.index) {
            *vis = if v { Visibility::Inherited } else { Visibility::Hidden };
        }
        if let (Some(mut d), Some(&(open, target, dir, locked))) = (door, doors.get(&li.index)) {
            d.open = open;
            d.target = target;
            d.dir = dir;
            d.locked = locked;
            // (one broken stays in pieces: gone, nothing to bump into)
            if broken.contains(&li.index) {
                d.broken = true;
                *vis = Visibility::Hidden;
                if let Some(c) = d.collider {
                    commands.entity(c).insert(bevy_rapier3d::prelude::ColliderDisabled);
                }
            }
        }
        if let Some(&(tr, r, sc)) = moved.get(&li.index) {
            // where its matinee left it, its collider too: the same move from where both began
            // (the matinees' own base, for when they play on from there)
            let restored = Transform { translation: Vec3::from(tr), rotation: Quat::from_array(r), scale: Vec3::from(sc) };
            if let Some(inst) = level.scene.instances.get(li.index as usize) {
                let base = matinee.base_of(e, Transform::from_matrix(Mat4::from_cols_array(&inst.transform)));
                let delta = restored.to_matrix() * base.to_matrix().inverse();
                if let Some(Ok(mut ct)) = col.map(|c| collider_tfs.get_mut(c.0)) {
                    let cb = matinee.base_of(col.unwrap().0, *ct);
                    *ct = Transform::from_matrix(delta * cb.to_matrix());
                    info!("save: {} moved by its matinee, collider {:?} -> {:?}", li.index, cb.translation, ct.translation);
                } else {
                    info!("save: {} moved by its matinee (collider {:?})", li.index, col.map(|c| c.0));
                }
            }
            *t = restored;
        }
    }
    let lit: std::collections::HashMap<u32, bool> = s.lights.iter().copied().collect();
    for (ll, mut vis) in &mut lights {
        if let Some(&on) = lit.get(&ll.0) {
            *vis = if on { Visibility::Inherited } else { Visibility::Hidden };
        }
    }
    krusts.dead = s.krusts.iter().copied().collect();
    krusts.restore = true;
    traps.state = s.traps.iter().copied().collect();
    traps.restore = true;
    usables.entered = s.usables.iter().map(|&(k, st, d)| (k, (st, d))).collect();
    usables.locks = s.usable_locks.iter().copied().collect();
    usables.restore = true;
    prop_restore.0 = Some(std::mem::take(&mut s.props));
    prop_restore.1 = s.held_prop;
    carry_restore.0 = s.carry.take();
    prop_restore.2 = std::mem::take(&mut s.prop_states);
    gadgets.0 = s.gadgets.take();
    projectiles.0 = Some(std::mem::take(&mut s.projectiles));
    swarms.0 = s.swarms.take();
    fish.0 = s.fish.take();
    krust_spit.0 = s.krust_spit.take();
    krust_state.0 = s.krust_state.take();
    trap_state.0 = s.trap_state.take();
    ammo_pickups.0 = std::mem::take(&mut s.ammo_pickups);
    possession.0 = s.possession.take().map(|saved| if level_only { saved.level_return() } else { saved });
    if let Some(security) = s.security.take() {
        devices.restore(security, props.iter().map(|(e, p)| (e, p.index)));
    }
    info!("save: restored {} ({} NPCs)", s.map, s.npcs.len());
}

pub(crate) fn restore_npcs(mut commands: Commands, mut q: Query<(Entity, &RestoreNpc, &mut Npc, &mut Transform)>, mut matinee: ResMut<crate::matinee::MatineeState>) {
    for (e, r, mut n, mut t) in &mut q {
        let s = &r.0;
        t.translation = Vec3::from(s.position);
        // Falling bodies bypass NPC steering, which otherwise updates this rotation.
        t.rotation = Quat::from_rotation_y(s.yaw);
        n.yaw = s.yaw;
        // Older snapshots have no death clock: treat their bodies as established
        // corpses rather than imposing a new post-kill delay on every load.
        n.corpse_age = if s.mode == Mode::Dead { s.corpse_age.unwrap_or(1_000_000.0).max(0.0) } else { 0.0 };
        if let Some(base) = s.ride_base {
            matinee.restore_ride_base(e, base);
        }
        // (saves from before health was the original's keep no more than it)
        n.health = s.health.min(n.max_health);
        n.alert = s.alert;
        n.awareness = s.awareness;
        n.route_idx = s.route_idx;
        n.down_t = s.down_t;
        if matches!(s.mode, Mode::Dead | Mode::Unconscious) {
            n.set_mode(s.mode);
            n.down_t = 1.0;
            commands.entity(e).remove::<bevy_rapier3d::prelude::KinematicCharacterController>();
            commands.entity(e).insert(bevy_rapier3d::prelude::CollisionGroups::new(bevy_rapier3d::prelude::Group::NONE, bevy_rapier3d::prelude::Group::NONE));
        } else {
            n.mode = s.mode;
        }
        if !s.severed.is_empty() {
            commands.entity(e).insert(crate::gore::Severed(s.severed.clone()));
        }
        if s.consumed {
            commands.entity(e).insert((crate::npc::ConsumedBody, Visibility::Hidden));
            if std::env::var_os("DH_CARRY_LOG").is_some() {
                info!("carry: restored consumed NPC {} hidden", s.spawner);
            }
        }
        if let Some(falling) = &s.falling {
            if std::env::var_os("DH_CARRY_LOG").is_some() {
                info!("carry: restored flight for NPC {} at {:?}, rotation {:?}, yaw {}", s.spawner, t.translation, t.rotation, s.yaw);
            }
            commands.entity(e).insert(falling.clone());
        }
        commands.entity(e).remove::<RestoreNpc>();
    }
}
