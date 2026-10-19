//! Security systems (the original `DisWallOfLight`, `DisDefenceTower` arc pylons,
//! `DisWatchTower`, `DisAlarmBell`), powered by whale oil tanks in receptacles: take the tank
//! and the device dies; rewire it (a rewire tool at the receptacle) and it turns on its
//! owners. Walls of light disintegrate whoever crosses them unless their faction is spared;
//! pylons charge and strike intruders inside their detection cylinder; alarm bells, rung by
//! guards in a fight, call everyone nearby and the reinforcements of their spawners.

use crate::audio::PostEvent;
use crate::gameplay::{HitKind, HudMessages, Noise, NoiseKind, NpcHit, PlayerStats};
use crate::interact::{Interaction, Usable};
use crate::level::{LevelInfo, LevelInstance, LevelSpawnSet};
use crate::npc::{Alert, Mode, Npc, SpawnRequest};
use crate::player::Player;
use crate::GameState;
use bevy::prelude::*;
use dhcook::format::Security;
use std::collections::HashMap;

pub struct SecurityPlugin;

impl Plugin for SecurityPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Devices>()
            .add_systems(OnEnter(GameState::InGame), setup.after(LevelSpawnSet))
            .add_systems(Update, (use_devices, tank_seats, walls.before(crate::powers::update_projectiles), wall_sparks, eyes, pylons, alarms, receptacle_events).chain().run_if(in_state(GameState::InGame)))
            .add_systems(Update, insert_tank.after(crate::interact::FocusSet).before(crate::interact::use_focus).run_if(in_state(GameState::InGame)));
    }
}

/// Usable ids above this are security parts (tanks, receptacle panels, bells), not level
/// script actors.
pub const SECURITY_USABLE: u32 = 0xFFFF_0000;

#[derive(Clone, Copy, PartialEq, Default, Debug, serde::Serialize, serde::Deserialize)]
enum Phase {
    #[default]
    Idle,
    /// arc pylon: charging, then the strike
    Charging(f32),
    Firing(f32),
    /// alarm bell: ringing for so long
    Ringing(f32),
}

#[derive(Clone, Default)]
pub struct Device {
    pub def: Security,
    /// receptacles: the tank is in (which)
    pub has_tank: bool,
    tank: Option<Entity>,
    pub rewired: bool,
    phase: Phase,
    cooldown: f32,
    /// an alarm bell: the guard on the way to ring it (and for how long), and how long it has
    /// been there
    ringer: Option<Entity>,
    ringer_t: f32,
    fighting_near: f32,
    /// walls of light: the wall's box (world -> local matrix and local bounds)
    wall: Option<(Mat4, Vec3, Vec3)>,
    /// level instances of the actor (tanks to hide)
    instances: Vec<Entity>,
    /// its receptacle (`list`): the nearest of its name (sublevels reuse names)
    receptacle_idx: Option<usize>,
    /// turned off by the level's scripts (`DisSeqAct_WallofLightControl`)
    pub off: bool,
    /// walls of light: seconds to its next spark
    spark_t: f32,
    /// its actor's meshes' box in the world (min, max)
    bounds: Option<(Vec3, Vec3)>,
}

impl Device {
    /// A receptacle's tank's place (where its tank sits).
    fn tank_seat(&self) -> Option<Vec3> {
        self.tank.map(|_| Vec3::from(self.def.position))
    }

    /// Who lays its zones (its actor's first mesh).
    pub fn requestor(&self) -> Option<Entity> {
        self.instances.first().copied()
    }

    /// The zone a wall or pylon no one trusts any more lays (`AddWhaleOilShape` 0x005683E0):
    /// its actor's colliding meshes' box grown on every side by half its half size's length,
    /// and that grown box's half size's length, the radius its push takes about its middle.
    pub fn distrust_box(&self) -> Option<(Vec3, Vec3, f32)> {
        let (lo, hi) = self.bounds?;
        let h = 0.5 * ((hi - lo) * 0.5).length();
        let (lo, hi) = (lo - Vec3::splat(h), hi + Vec3::splat(h));
        Some((lo, hi, ((hi - lo) * 0.5).length()))
    }

    /// A wall of light's middle and the way through it.
    pub fn wall_face(&self) -> Option<(Vec3, Vec3)> {
        let (inv, min, max) = self.wall?;
        let world = inv.inverse();
        let size = max - min;
        // the thin side runs through it
        let axis = if size.x <= size.y && size.x <= size.z {
            Vec3::X
        } else if size.y <= size.z {
            Vec3::Y
        } else {
            Vec3::Z
        };
        Some((world.transform_point3((min + max) * 0.5), world.transform_vector3(axis).normalize_or(Vec3::X)))
    }
}

#[derive(Resource, Default)]
pub struct Devices {
    pub list: Vec<Device>,
    /// walls that just struck someone (their actors), for the scripts'
    /// `DisSeqEvent_WallOfLight` "Kill Effect"
    pub wall_kills: Vec<String>,
    /// where whale oil tanks sit: (receptacle, position, rotation), found from the tanks the
    /// level puts in them
    seats: Vec<(usize, Vec3, Quat)>,
    seats_found: bool,
    /// whale oil tanks' charges (by prop), and what the devices spent from the tank by where
    /// it sits: 1 a kill, 2 an animal's, 3 a watchtower's shot (`DisTweaks_WhaleOilBattery`)
    pub charges: std::collections::HashMap<Entity, f32>,
    pub drains: Vec<(Vec3, usize)>,
}

/// Persist stable level indices, never ECS entity ids (those change on reload).
#[derive(serde::Serialize, serde::Deserialize, Default)]
pub struct DevicesSave {
    states: Vec<(usize, bool, bool)>,
    charges: Vec<(usize, f32)>,
    #[serde(default)]
    progress: Vec<(usize, Phase, f32, f32)>,
}

/// A device's panel in `SecuritySystems_twk` (`WallOfLight_eye_panel_twk`...).
pub(crate) fn panel_group(kind: &str) -> &'static str {
    match kind {
        "WallOfLight" => "WallOfLight",
        "ArcPylon" => "ArcPylon",
        "WatchTower" => "WatchTower",
        "AlarmBell" => "Alarm",
        _ => "Generic",
    }
}

/// A panel's message (`m_UseMessage`: "This Wall of Light will now harm enemies instead of
/// you."; `m_CannotUseMessage`: "You don't have any rewiring tools."), its group's else the
/// generic panel's.
pub(crate) fn panel_text(data: &crate::gamedata::Data, group: &str, key: &str) -> String {
    let t = data.text("Security", &format!("{group}.{key}"));
    if t.is_empty() { data.text("Security", &format!("Generic.{key}")) } else { t }
}

impl Device {
    /// Its level instances.
    pub fn instances(&self) -> &[Entity] {
        &self.instances
    }
}

impl Devices {
    pub fn save(&self, props: impl Iterator<Item = (Entity, usize)>) -> DevicesSave {
        DevicesSave {
            states: self.list.iter().enumerate().map(|(i, d)| (i, d.rewired, d.off)).collect(),
            charges: props.filter_map(|(entity, index)| self.charges.get(&entity).map(|charge| (index, *charge))).collect(),
            progress: self.list.iter().enumerate().map(|(i, d)| (i, d.phase, d.cooldown, d.fighting_near)).collect(),
        }
    }

    pub fn restore(&mut self, saved: DevicesSave, props: impl Iterator<Item = (Entity, usize)>) {
        for d in &mut self.list {
            d.phase = Phase::Idle;
            d.cooldown = 0.0;
            d.fighting_near = 0.0;
        }
        for (index, phase, cooldown, fighting_near) in saved.progress {
            if let Some(d) = self.list.get_mut(index) {
                d.phase = phase;
                d.cooldown = cooldown;
                d.fighting_near = fighting_near;
            }
        }
        for (index, rewired, off) in saved.states {
            if let Some(device) = self.list.get_mut(index) {
                device.rewired = rewired;
                device.off = off;
            }
        }
        let charges: std::collections::HashMap<usize, f32> = saved.charges.into_iter().collect();
        self.charges.clear();
        self.drains.clear();
        for (entity, index) in props {
            if let Some(&charge) = charges.get(&index).filter(|c| c.is_finite()) {
                self.charges.insert(entity, charge.max(0.0));
            }
        }
    }

    /// Is a device fed: its receptacle holds a tank (devices without one always run).
    /// A wall or pylon running: powered and switched on (what a whale oil device's record and
    /// path cost ask, 0x00A19470 / 0x00A25A00).
    pub fn live(&self, i: usize) -> bool {
        self.list.get(i).is_some_and(|d| matches!(d.def.kind.as_str(), "WallOfLight" | "ArcPylon") && self.powered(d))
    }

    /// The walls of light a character's path may not enter (each wall's `CostFor` 0x00A30D90:
    /// 10,000,000, which the search refuses, unless 0x00A29650 lets it by). Only a wall that
    /// weighs paths (`m_bWoLAffectPathFinding`), switched on, with a charged tank in its
    /// receptacle, bars anyone; none bars one heedless of them (`m_eWallsOfLightConfidence`
    /// 0) or panicking. One fearing them is barred by all; one confident, by those not of its
    /// friends (`m_FriendlyFactions`, rewired or not) and those it saw kill an ally.
    pub fn walls_barring(&self, walls: u8, faction: &str, panicking: bool, distrust: &[usize]) -> Vec<(Mat4, Vec3, Vec3)> {
        if walls == 0 || panicking {
            return Vec::new();
        }
        self.list
            .iter()
            .enumerate()
            .filter(|(_, d)| d.def.params.get("m_bWoLAffectPathFinding").is_none_or(|v| *v != 0.0) && !d.off)
            .filter(|(_, d)| d.receptacle_idx.is_some_and(|r| self.list[r].has_tank && self.list[r].tank.is_none_or(|t| self.charged(t))))
            .filter(|(i, d)| walls == 1 || !d.def.friendly.iter().any(|f| f == faction) || distrust.contains(i))
            .filter_map(|(_, d)| d.wall)
            .collect()
    }

    fn powered(&self, d: &Device) -> bool {
        if d.off {
            return false;
        }
        if d.def.receptacle.is_empty() {
            return true;
        }
        // (a tank in, with oil left)
        d.receptacle_idx.map(|r| self.list[r].has_tank && self.list[r].tank.is_none_or(|t| self.charged(t))).unwrap_or(true)
    }

    /// Whether a running wall of light stands between two points (it meets projectiles while
    /// powered: `ShouldTrace`, as roshan's r614/blink research decoded it).
    pub fn wall_between(&self, a: Vec3, b: Vec3) -> bool {
        self.list.iter().any(|d| d.wall.as_ref().is_some_and(|w| self.powered(d) && wall_crossing(w, a, b).is_some()))
    }

    /// A tank has oil left.
    pub fn charged(&self, tank: Entity) -> bool {
        self.charges.get(&tank).is_none_or(|c| *c > 0.0)
    }

    /// The device a level script names (the nearest of that name).
    pub fn find(&self, name: &str, near: Vec3) -> Option<usize> {
        self.list
            .iter()
            .enumerate()
            .filter(|(_, d)| d.def.actor == name)
            .min_by(|a, b| Vec3::from(a.1.def.position).distance(near).total_cmp(&Vec3::from(b.1.def.position).distance(near)))
            .map(|(i, _)| i)
    }

    /// `DisSeqAct_WallofLightControl`: off (0), on (1), toggle (2), switch polarity (3).
    pub fn control_wall(&mut self, i: usize, cmd: usize) {
        let Some(d) = self.list.get_mut(i) else { return };
        match cmd {
            0 => d.off = true,
            1 => d.off = false,
            2 => d.off = !d.off,
            _ => d.rewired = !d.rewired,
        }
    }

    /// `DisSeqAct_AlarmBell`: silence it (false) or set it ringing; the sound to post.
    pub fn ring(&mut self, i: usize, on: bool) -> Option<(String, Vec3)> {
        let d = self.list.get_mut(i)?;
        let pos = Vec3::from(d.def.position);
        match (on, d.phase) {
            (true, Phase::Ringing(_)) | (false, Phase::Idle) => None,
            (true, _) => {
                d.phase = Phase::Ringing(0.0);
                d.def.sounds.get("m_pAlarmRingBeginEvent").map(|s| (s.clone(), pos))
            }
            (false, _) => {
                d.phase = Phase::Idle;
                d.def.sounds.get("m_pAlarmRingEndEvent").map(|s| (s.clone(), pos))
            }
        }
    }

    /// A circuitry panel rewired: the nearest device of its kind (the panel's label names it:
    /// "Wall of Light Circuitry", "Arc Pylon Circuitry", "Alarm Circuitry"; else any) turns on
    /// its owners. Whether one was found.
    /// The device a panel rewires: the nearest of its kind; its message group
    /// (`SecuritySystems_twk`'s) if one was.
    pub fn rewire_near(&mut self, label: &str, at: Vec3) -> Option<&'static str> {
        let kind = if label.contains("Wall of Light") {
            Some("WallOfLight")
        } else if label.contains("Arc Pylon") {
            Some("ArcPylon")
        } else if label.contains("Alarm") {
            Some("AlarmBell")
        } else {
            None
        };
        let near = self
            .list
            .iter()
            .enumerate()
            .filter(|(_, d)| kind.map_or(matches!(d.def.kind.as_str(), "WallOfLight" | "ArcPylon" | "WatchTower" | "AlarmBell"), |k| d.def.kind == k))
            .map(|(i, d)| (i, Vec3::from(d.def.position).distance(at)))
            .filter(|(_, d)| *d < 30.0)
            .min_by(|a, b| a.1.total_cmp(&b.1));
        let (i, _) = near?;
        self.list[i].rewired = true;
        // an alarm's panel silences it
        if self.list[i].def.kind == "AlarmBell" {
            self.list[i].off = true;
        }
        Some(panel_group(&self.list[i].def.kind))
    }

    /// `DisSeqAct_PlugWhaleOilBattery`: a tank into (or out of) a receptacle.
    pub fn plug(&mut self, receptacle: usize, plug: bool) {
        if let Some(r) = self.list.get_mut(receptacle) {
            r.has_tank = plug;
        }
    }
    fn rewired(&self, d: &Device) -> bool {
        d.rewired || d.receptacle_idx.is_some_and(|r| self.list[r].rewired)
    }

    /// A device's state by its actor: powered (not turned off, fed), rewired (`watchtower`).
    pub fn state(&self, actor: &str) -> Option<(bool, bool)> {
        let d = self.list.iter().find(|d| d.def.actor == actor)?;
        Some((self.powered(d), self.rewired(d)))
    }

    /// Where the tank feeding a device sits (its receptacle's, with a tank in), to spend from.
    pub fn feed_seat(&self, actor: &str) -> Option<Vec3> {
        let d = self.list.iter().find(|d| d.def.actor == actor)?;
        self.list.get(d.receptacle_idx?)?.tank_seat()
    }
}

#[cfg(test)]
mod save_tests {
    use super::*;

    #[test]
    fn a_bolt_crossing_a_wall_of_light_in_one_step_meets_it() {
        // a wall 0.2 m thick across x = 0, 4 m wide and 3 m high
        let w = (Mat4::IDENTITY, Vec3::new(-0.1, 0.0, -2.0), Vec3::new(0.1, 3.0, 2.0));
        // from in front of it to behind it in one frame: met at its (grown) face
        let at = wall_crossing(&w, Vec3::new(-1.0, 1.5, 0.0), Vec3::new(1.0, 1.5, 0.0)).unwrap();
        assert!((at.x + 0.35).abs() < 1e-4, "{at}");
        // beside it, or short of it: not
        assert!(wall_crossing(&w, Vec3::new(-1.0, 1.5, 3.0), Vec3::new(1.0, 1.5, 3.0)).is_none());
        assert!(wall_crossing(&w, Vec3::new(-2.0, 1.5, 0.0), Vec3::new(-1.0, 1.5, 0.0)).is_none());
        // lying in it
        assert!(wall_crossing(&w, Vec3::new(0.0, 1.5, 0.0), Vec3::new(0.0, 1.5, 0.0)).is_some());
        // a live wall stands between; an unpowered one (scripted off) doesn't
        let mut devices = Devices::default();
        devices.list.push(Device { wall: Some(w), ..default() });
        assert!(devices.wall_between(Vec3::new(-1.0, 1.5, 0.0), Vec3::new(1.0, 1.5, 0.0)));
        devices.list[0].off = true;
        assert!(!devices.wall_between(Vec3::new(-1.0, 1.5, 0.0), Vec3::new(1.0, 1.5, 0.0)));
    }

    #[test]
    fn security_progress_restores_all_phases_and_legacy_saves_clear_stale_timers() {
        let mut devices = Devices::default();
        devices.list = [Phase::Charging(0.8), Phase::Firing(0.3), Phase::Ringing(12.0), Phase::Idle]
            .into_iter().map(|phase| Device { phase, cooldown: 2.0, fighting_near: 3.5, ..default() }).collect();
        let saved = serde_json::to_value(devices.save(std::iter::empty())).unwrap();
        devices.list.reverse();
        devices.restore(serde_json::from_value(saved.clone()).unwrap(), std::iter::empty());
        assert_eq!(devices.list.iter().map(|d| d.phase).collect::<Vec<_>>(),
            vec![Phase::Charging(0.8), Phase::Firing(0.3), Phase::Ringing(12.0), Phase::Idle]);
        assert!(devices.list.iter().all(|d| d.cooldown == 2.0 && d.fighting_near == 3.5));
        let mut legacy = saved;
        legacy.as_object_mut().unwrap().remove("progress");
        devices.restore(serde_json::from_value(legacy).unwrap(), std::iter::empty());
        assert!(devices.list.iter().all(|d| d.phase == Phase::Idle && d.cooldown == 0.0 && d.fighting_near == 0.0));
    }

    #[test]
    fn security_round_trip_remaps_tanks_and_keeps_empty_tanks_empty() {
        let mut world = World::new();
        let old_empty = world.spawn_empty().id();
        let old_partial = world.spawn_empty().id();
        let mut before = Devices::default();
        before.list = vec![Device { rewired: true, ..default() }, Device { off: true, ..default() }];
        before.charges.insert(old_empty, 0.0);
        before.charges.insert(old_partial, 17.5);
        let json = serde_json::to_vec(&before.save([(old_empty, 4), (old_partial, 9)].into_iter())).unwrap();

        let new_empty = world.spawn_empty().id();
        let new_partial = world.spawn_empty().id();
        let untouched = world.spawn_empty().id();
        let mut after = Devices::default();
        after.list = vec![Device::default(), Device::default()];
        after.charges.insert(new_empty, 50.0);
        after.drains.push((Vec3::ZERO, 1));
        // Reverse the query's order: matching must use the movable index.
        after.restore(serde_json::from_slice(&json).unwrap(), [(new_partial, 9), (untouched, 10), (new_empty, 4)].into_iter());

        assert!(after.list[0].rewired);
        assert!(after.list[1].off);
        assert!(!after.charged(new_empty));
        assert_eq!(after.charges.get(&new_partial), Some(&17.5));
        assert!(!after.charges.contains_key(&old_empty));
        assert!(!after.charges.contains_key(&untouched));
        assert!(after.drains.is_empty());
    }

    #[test]
    fn missing_devices_and_tanks_in_an_older_level_do_not_break_restore() {
        let mut devices = Devices::default();
        let saved = DevicesSave { states: vec![(99, true, true)], charges: vec![(42, 0.0)], ..default() };
        devices.restore(saved, std::iter::empty());
        assert!(devices.list.is_empty());
        assert!(devices.charges.is_empty());
    }
}

/// Whale oil tanks that are props: a receptacle is fed while a tank sits in its seat (as the
/// level placed it, or put back).
fn tank_seats(
    mut devices: ResMut<Devices>,
    level: Option<Res<LevelInfo>>,
    tanks: Query<(Entity, &crate::props::Prop, &Transform)>,
    held: Res<crate::props::Held>,
    mut sfx: MessageWriter<PostEvent>,
    vm: Option<ResMut<crate::kismet::Vm>>,
) {
    let Some(level) = level else { return };
    let is_tank = |p: &crate::props::Prop| level.scene.movables.get(p.index).is_some_and(|m| m.tank.is_some());
    // the tanks' charges: full as the level puts them (`m_InitialNumberOfCharges`), spent by
    // the devices they feed, topped up by the scripts (`DisSeqAct_RefillWhaleOilBattery`)
    let full = |p: &crate::props::Prop| level.scene.movables.get(p.index).and_then(|m| m.charges).map(|c| c[0]).unwrap_or(50.0);
    for (e, p, _) in tanks.iter().filter(|(_, p, _)| is_tank(p)) {
        devices.charges.entry(e).or_insert_with(|| full(p));
    }
    for (at, kind) in std::mem::take(&mut devices.drains) {
        let Some((e, p, _)) = tanks.iter().filter(|(_, p, t)| is_tank(p) && t.translation.distance(at) < 2.5).min_by(|a, b| a.2.translation.distance(at).total_cmp(&b.2.translation.distance(at))) else { continue };
        let cost = level.scene.movables.get(p.index).and_then(|m| m.charges).map(|c| c[kind.min(3)]).unwrap_or(1.0);
        if let Some(c) = devices.charges.get_mut(&e) {
            let was = *c;
            *c = (*c - cost).max(0.0);
            if std::env::var("DH_TANK_LOG").is_ok() {
                info!("tank {e:?}: {was:.0} -> {:.0}", *c);
            }
        }
    }
    if let Some(mut vm) = vm {
        for (insts, pct) in std::mem::take(&mut vm.tank_refills) {
            for (e, p, _) in tanks.iter().filter(|(_, p, _)| is_tank(p) && level.scene.movables.get(p.index).is_some_and(|m| insts.contains(&m.instance))) {
                devices.charges.insert(e, full(p) * (pct / 100.0).clamp(0.0, 1.0));
            }
        }
    }
    if !devices.seats_found {
        devices.seats_found = true;
        // each tank's receptacle: the nearest that names it
        let mut seats = Vec::new();
        for (_, p, t) in tanks.iter().filter(|(_, p, _)| is_tank(p)) {
            let name = level.scene.movables[p.index].tank.clone().unwrap_or_default();
            let rec = devices
                .list
                .iter()
                .enumerate()
                .filter(|(_, d)| d.def.kind == "Receptacle" && d.def.battery == name)
                .min_by(|a, b| Vec3::from(a.1.def.position).distance(t.translation).total_cmp(&Vec3::from(b.1.def.position).distance(t.translation)))
                .map(|(i, _)| i);
            if let Some(r) = rec {
                seats.push((r, t.translation, t.rotation));
            }
        }
        devices.seats = seats;
    }
    let seats = devices.seats.clone();
    for (r, at, _) in seats {
        let tank = tanks.iter().find(|(e, p, t)| is_tank(p) && held.0 != Some(*e) && t.translation.distance(at) < 0.5).map(|x| x.0);
        let filled = tank.is_some();
        let d = &mut devices.list[r];
        d.tank = tank;
        if d.has_tank != filled {
            d.has_tank = filled;
            let key = if filled { "m_PlugSound" } else { "m_UnplugSound" };
            let s = d.def.sounds.get(key).cloned().unwrap_or_else(|| if filled { "Whale_Oil_Battery_Plug".into() } else { "Whale_Oil_Battery_Unplug".into() });
            sfx.write(PostEvent::named(&s, Some(at)));
        }
    }
}

/// [Use] holding a whale oil tank at an empty receptacle: the tank goes in.
#[allow(clippy::too_many_arguments)]
fn insert_tank(
    paused: Res<crate::hud::Paused>,
    (keys, bind): (Res<ButtonInput<KeyCode>>, Res<crate::bindings::Bindings>),
    level: Option<Res<LevelInfo>>,
    devices: Res<Devices>,
    mut held: ResMut<crate::props::Held>,
    props: Query<&crate::props::Prop>,
    mut commands: Commands,
    mut bodies: Query<(&mut Transform, &mut bevy_rapier3d::prelude::Velocity, &mut bevy_rapier3d::prelude::GravityScale, &mut bevy_rapier3d::prelude::Sleeping)>,
    cam: Query<&GlobalTransform, With<crate::player::PlayerCamera>>,
    mut focus: ResMut<crate::interact::InteractFocus>,
) {
    if paused.0 { return; }
    let (Some(level), Some(e)) = (level, held.0) else { return };
    let Ok(p) = props.get(e) else { return };
    if level.scene.movables.get(p.index).is_none_or(|m| m.tank.is_none()) {
        return;
    }
    let Ok(c) = cam.single() else { return };
    let (eye, fwd) = (c.translation(), c.forward().as_vec3());
    let seat = devices
        .seats
        .iter()
        .filter(|(r, at, _)| !devices.list[*r].has_tank && at.distance(eye) < 2.4 && (*at - eye).normalize_or_zero().dot(fwd) > 0.75)
        .min_by(|a, b| a.1.distance(eye).total_cmp(&b.1.distance(eye)))
        .copied();
    let Some((_, at, rot)) = seat else { return };
    focus.0 = true;
    focus.1 = None;
    focus.2 = format!("{} Insert Whale Oil Tank", crate::bindings::hint(crate::bindings::Act::Use));
    if !keys.just_pressed(bind.key(crate::bindings::Act::Use)) {
        return;
    }
    // into its seat, at rest
    let (body, offset) = p.body();
    if let Some(Ok((mut t, mut v, mut g, mut s))) = body.map(|b| bodies.get_mut(b)) {
        t.translation = at + rot * offset;
        t.rotation = rot;
        v.linear = Vec3::ZERO;
        v.angular = Vec3::ZERO;
        g.0 = 1.0;
        s.sleeping = true;
    }
    if let Some(b) = body {
        commands.entity(b).try_insert(bevy_rapier3d::prelude::RigidBody::Fixed);
    }
    held.0 = None;
    held.1 = 0.4;
}

/// The level scripts hear of receptacles getting or losing a tank
/// (`DisSeqEvent_WhaleOilReceptacle` Plugged / Unplugged).
fn receptacle_events(devices: Res<Devices>, mut last: Local<Vec<bool>>, mut used: MessageWriter<crate::interact::Interaction>) {
    let now: Vec<bool> = devices.list.iter().map(|d| d.has_tank).collect();
    if last.len() == now.len() {
        for (i, d) in devices.list.iter().enumerate() {
            if d.def.kind == "Receptacle" && now[i] != last[i] {
                used.write(crate::interact::Interaction::Receptacle { actor: d.def.actor.clone(), at: Vec3::from(d.def.position), plugged: now[i] });
            }
        }
    }
    *last = now;
}

/// A wall of light's eye (`DisDetectionEye`): its looks (`DisTweaks_DetectionEye`) by what it
/// sees in its cylinder: nobody (neutral), someone the wall would burn (a threat), someone it
/// lets pass (a friend); dark when the wall has no power.
#[derive(Component)]
pub struct EyeLooks {
    /// its wall (`scene.security`)
    pub device: usize,
    pub looks: [Option<Handle<crate::ue3mat::Ue3Material>>; 4],
    pub shown: usize,
}

/// The eyes watch who comes near their wall.
fn eyes(
    devices: Res<Devices>,
    mut eyes: Query<(&mut EyeLooks, &mut MeshMaterial3d<crate::ue3mat::Ue3Material>)>,
    player: Query<&Transform, With<Player>>,
    npcs: Query<(&Npc, &Transform), Without<Player>>,
    stats: Res<PlayerStats>,
    mut sfx: MessageWriter<PostEvent>,
) {
    for (mut look, mut mat) in &mut eyes {
        let Some(d) = devices.list.get(look.device) else { continue };
        let state = if !devices.powered(d) {
            3
        } else {
            let rewired = devices.rewired(d);
            let at = Vec3::from(d.def.detect_at);
            let inside = |p: Vec3| (p - at).with_y(0.0).length() < d.def.radius && (p.y - at.y).abs() < d.def.height.max(0.5);
            let (mut threat, mut friend) = (false, false);
            if let Ok(pt) = player.single() {
                if !stats.dead && inside(pt.translation) {
                    // rewired, the wall is Corvo's
                    if rewired {
                        friend = true;
                    } else {
                        threat = true;
                    }
                }
            }
            for (n, t) in &npcs {
                if n.is_down() || !inside(t.translation) {
                    continue;
                }
                let spared = d.def.friendly.iter().any(|f| *f == n.faction);
                if if rewired { n.enemy } else { !spared && !n.faction.is_empty() } {
                    threat = true;
                } else {
                    friend = true;
                }
            }
            if threat {
                1
            } else if friend {
                2
            } else {
                0
            }
        };
        if state == look.shown {
            continue;
        }
        let at = Some(Vec3::from(d.def.position));
        if state == 1 {
            if let Some(s) = d.def.sounds.get("eye_threat") {
                sfx.write(PostEvent::named(s, at));
            }
        } else if look.shown == 1 {
            if let Some(s) = d.def.sounds.get("eye_threat_stop") {
                sfx.write(PostEvent::named(s, at));
            }
        }
        if let Some(h) = &look.looks[state] {
            mat.0 = h.clone();
        }
        look.shown = state;
    }
}

#[allow(clippy::too_many_arguments)]
fn setup(
    mut commands: Commands,
    level: Option<Res<LevelInfo>>,
    mut devices: ResMut<Devices>,
    instances: Query<(Entity, &LevelInstance)>,
) {
    *devices = Devices::default();
    let Some(level) = level else { return };
    let scene = &level.scene;
    let mut by_inst: HashMap<&str, Vec<(Entity, u32)>> = HashMap::new();
    for (e, li) in &instances {
        by_inst.entry(li.actor.as_str()).or_default().push((e, li.index));
    }
    for (i, s) in scene.security.iter().enumerate() {
        let mut d = Device { def: s.clone(), has_tank: !s.battery.is_empty(), ..Default::default() };
        let insts = by_inst.get(s.actor.as_str()).cloned().unwrap_or_default();
        d.instances = insts.iter().map(|x| x.0).collect();
        // (its meshes' box in the world)
        for inst in insts.iter().filter_map(|x| scene.instances.get(x.1 as usize)) {
            let Some(m) = scene.meshes.get(inst.mesh as usize) else { continue };
            let world = Mat4::from_cols_array(&inst.transform);
            let (min, max) = (Vec3::from(m.min), Vec3::from(m.max));
            for c in 0..8 {
                let p = world.transform_point3(Vec3::new(if c & 1 == 0 { min.x } else { max.x }, if c & 2 == 0 { min.y } else { max.y }, if c & 4 == 0 { min.z } else { max.z }));
                d.bounds = Some(d.bounds.map_or((p, p), |(lo, hi)| (lo.min(p), hi.max(p))));
            }
        }
        if s.kind == "WallOfLight" {
            // the wall: its (editor-only, never drawn) mesh's box
            if let Some(inst) = insts.first().and_then(|x| scene.instances.get(x.1 as usize)) {
                if let Some(m) = scene.meshes.get(inst.mesh as usize) {
                    let world = Mat4::from_cols_array(&inst.transform);
                    let (min, max) = (Vec3::from(m.min), Vec3::from(m.max));
                    d.wall = Some((world.inverse(), min, max));
                }
            }
        }
        // what the player can use: tanks, receptacles (to rewire, where the level has no
        // circuitry panel of its own for it), bells (to silence)
        let circuitry_near = scene.usables.iter().filter(|u| u.label.contains("Circuitry")).filter_map(|u| u.instance.and_then(|i| scene.instances.get(i as usize))).any(|inst| {
            let at = Vec3::new(inst.transform[12], inst.transform[13], inst.transform[14]);
            at.distance(Vec3::from(s.position)) < 12.0
        });
        // (a tank that is a prop of its own is picked up as one)
        let tank_prop = s.kind == "Battery" && scene.movables.iter().any(|m| m.tank.as_deref() == Some(s.actor.as_str()));
        // (their words as `SecuritySystems_twk` has them: the tank's "Whale Oil Tank", "Carry";
        // the panel's "Circuitry", "Rewire"; the bell's name)
        let label = match s.kind.as_str() {
            "Battery" if tank_prop => None,
            "Battery" => Some(("Carry", "Whale Oil Tank")),
            "Receptacle" if !circuitry_near => Some(("Rewire", "Circuitry")),
            "AlarmBell" => Some(("Silence Alarm", "Alarm Bell")),
            _ => None,
        };
        if let Some((label, name)) = label {
            commands.spawn((
                Usable { actor: SECURITY_USABLE + i as u32, label: label.into(), name: name.into() },
                Transform::from_translation(Vec3::from(s.position) + Vec3::Y * 0.3),
                DespawnOnExit(GameState::InGame),
            ));
        }
        devices.list.push(d);
    }
    // each device's receptacle: the nearest of its name
    let recs: Vec<(usize, String, Vec3)> = devices.list.iter().enumerate().filter(|(_, d)| d.def.kind == "Receptacle").map(|(i, d)| (i, d.def.actor.clone(), Vec3::from(d.def.position))).collect();
    for d in devices.list.iter_mut().filter(|d| !d.def.receptacle.is_empty()) {
        let at = Vec3::from(d.def.position);
        d.receptacle_idx = recs.iter().filter(|r| r.1 == d.def.receptacle).min_by(|a, b| a.2.distance(at).total_cmp(&b.2.distance(at))).map(|r| r.0);
    }
    // a tank that sits in a receptacle feeds it; loose tanks don't
    let held: Vec<String> = devices.list.iter().filter(|d| d.def.kind == "Receptacle").map(|d| d.def.battery.clone()).collect();
    for d in devices.list.iter_mut().filter(|d| d.def.kind == "Battery") {
        d.has_tank = held.contains(&d.def.actor);
    }
    if !devices.list.is_empty() {
        info!("security: {} devices", devices.list.len());
    }
}

#[allow(clippy::too_many_arguments)]
fn use_devices(
    data: Res<crate::gamedata::Data>,
    mut used: MessageReader<Interaction>,
    mut devices: ResMut<Devices>,
    mut stats: ResMut<PlayerStats>,
    mut msgs: ResMut<HudMessages>,
    mut sfx: MessageWriter<PostEvent>,
    mut vis: Query<&mut Visibility>,
    mut commands: Commands,
    usables: Query<(Entity, &Usable)>,
) {
    for u in used.read() {
        let Interaction::Usable(id) = u else { continue };
        if *id < SECURITY_USABLE {
            continue;
        }
        let i = (*id - SECURITY_USABLE) as usize;
        let Some(d) = devices.list.get(i).cloned() else { continue };
        match d.def.kind.as_str() {
            "Battery" => {
                // out of its receptacle: whatever it fed dies
                let rec = devices.list.iter().position(|r| r.def.kind == "Receptacle" && r.def.battery == d.def.actor);
                if let Some(r) = rec {
                    devices.list[r].has_tank = false;
                    let s = devices.list[r].def.sounds.get("m_UnplugSound").cloned().unwrap_or_else(|| "Whale_Oil_Battery_Unplug".into());
                    sfx.write(PostEvent::named(&s, Some(Vec3::from(d.def.position))));
                }
                for &e in &d.instances {
                    if let Ok(mut v) = vis.get_mut(e) {
                        *v = Visibility::Hidden;
                    }
                }
                for (e, u) in &usables {
                    if u.actor == *id {
                        commands.entity(e).despawn();
                    }
                }
                *stats.items.entry("WhaleOilTank".into()).or_default() += 1;
            }
            "Receptacle" => {
                let tools = stats.items.get("RewireTool_twk").copied().unwrap_or(0);
                if devices.list[i].rewired {
                } else if tools == 0 {
                    msgs.push(panel_text(&data, "Generic", "m_CannotUseMessage"));
                } else {
                    stats.items.insert("RewireTool_twk".into(), tools - 1);
                    devices.list[i].rewired = true;
                    sfx.write(PostEvent::named("UI_Validation", Some(Vec3::from(d.def.position))));
                    // (the panel's message: what the tank feeds)
                    let fed = devices.list.iter().find(|x| x.receptacle_idx == Some(i)).map(|x| panel_group(&x.def.kind)).unwrap_or("Generic");
                    msgs.push(panel_text(&data, fed, "m_UseMessage"));
                }
            }
            "AlarmBell" => {
                if let Phase::Ringing(_) = devices.list[i].phase {
                    devices.list[i].phase = Phase::Idle;
                    let s = d.def.sounds.get("m_pAlarmRingEndEvent").cloned().unwrap_or_else(|| "Alarm_Stop".into());
                    sfx.write(PostEvent::named(&s, Some(Vec3::from(d.def.position))));
                }
            }
            _ => {}
        }
    }
}

/// Walls of light: crossing one while it runs electrocutes characters, who vanish, unless its friends (or,
/// rewired, Corvo); Corvo it shocks for its `m_Damage` (100) as he touches it. While Bend Time
/// stops the world it can be walked through (as roshan's r614/blink research decoded it).
#[allow(clippy::too_many_arguments)]
fn walls(
    mut devices: ResMut<Devices>,
    mut stats: ResMut<PlayerStats>,
    (mut sfx, mut fx): (MessageWriter<PostEvent>, MessageWriter<crate::particles::SpawnEffect>),
    mut hits: MessageWriter<NpcHit>,
    player: Query<&Transform, With<Player>>,
    npcs: Query<(Entity, &Npc, &Transform), Without<Player>>,
    mut ambient: Local<Vec<bool>>,
    time: Res<Time>,
    (tc, mut touching, mut struck): (Res<crate::gameplay::TimeControl>, Local<Vec<bool>>, Local<std::collections::HashSet<Entity>>),
    (mut commands, carry, ashes, thrown): (
        Commands,
        Res<crate::carry::Carry>,
        Query<(), Or<(With<crate::npc::Ashes>, With<crate::npc::Electrocuted>)>>,
        Query<(Entity, &Transform, Option<&crate::powers::Projectile>), Or<(With<crate::powers::Projectile>, With<crate::gadgets::Grenade>)>>,
    ),
    mut last_thrown: Local<std::collections::HashMap<Entity, Vec3>>,
) {
    // (stopped time: no collision, nothing burns)
    let stopped = tc.bend_remaining > 0.0 && tc.world_dilation <= 0.0;
    if ambient.len() != devices.list.len() {
        *ambient = vec![false; devices.list.len()];
    }
    if touching.len() != devices.list.len() {
        *touching = vec![false; devices.list.len()];
    }
    let inside = |w: &(Mat4, Vec3, Vec3), p: Vec3| {
        let l = w.0.transform_point3(p);
        l.cmpge(w.1 - Vec3::splat(0.25)).all() && l.cmple(w.2 + Vec3::splat(0.25)).all()
    };
    // (each thrown thing's path: from where it was to where it is going this frame)
    let step = time.delta_secs() * tc.world_scale();
    let paths: Vec<(Entity, Vec3, Vec3)> = thrown
        .iter()
        .map(|(e, t, pr)| {
            let p = t.translation;
            let from = last_thrown.get(&e).copied().unwrap_or(p);
            (e, from, pr.filter(|pr| !pr.stuck).map(|pr| p + pr.vel * step).unwrap_or(p))
        })
        .collect();
    *last_thrown = thrown.iter().map(|(e, t, _)| (e, t.translation)).collect();
    let mut gone: Vec<Entity> = Vec::new();
    // the beams it strikes into what it shocks (`m_Beams`: activated with their far end on it,
    // 0x00A30CA0) while its kill effect runs (`m_ActivationTimeForPawnEffect` for a character,
    // else `m_ActivationTime`: 0.1 s)
    let strike = |fx: &mut MessageWriter<crate::particles::SpawnEffect>, d: &Device, to: Vec3, pawn: bool| {
        let secs = d.def.params.get(if pawn { "m_ActivationTimeForPawnEffect" } else { "m_ActivationTime" }).copied().unwrap_or(0.1);
        if secs <= 0.0 {
            return;
        }
        for (sys, m) in &d.def.beams {
            let (_, rot, at) = Mat4::from_cols_array(m).to_scale_rotation_translation();
            fx.write(crate::particles::SpawnEffect { system: Some(*sys), rot, secs, beam_to: Some(to), ..crate::particles::SpawnEffect::at("wall_beam", at) });
        }
    };
    let mut killed = Vec::new();
    let mut spent = Vec::new();
    for (i, d) in devices.list.iter().enumerate() {
        let Some(w) = &d.wall else { continue };
        let on = devices.powered(d);
        // its hum while it runs
        if on != ambient[i] {
            ambient[i] = on;
            let key = if on { "m_AmbientStart" } else { "m_DisactivateSound" };
            if let Some(s) = d.def.sounds.get(key) {
                sfx.write(PostEvent::named(s, Some(Vec3::from(d.def.position))));
            }
        }
        if std::env::var("DH_WALL_LOG").is_ok() && (time.elapsed_secs() * 1.0).fract() < time.delta_secs() {
            info!("wall {} on {on} (off {}, receptacle {:?}, tank {}) rewired {} stopped {stopped}", d.def.actor, d.off, d.receptacle_idx, d.receptacle_idx.is_some_and(|r| devices.list[r].has_tank), devices.rewired(d));
        }
        if !on || stopped {
            touching[i] = false;
            continue;
        }
        let rewired = devices.rewired(d);
        if let Ok(pt) = player.single() {
            let now_in = !rewired && !stats.dead && inside(w, pt.translation);
            if now_in && !touching[i] {
                strike(&mut fx, d, pt.translation, true);
                let damage = if d.def.damage > 0.0 { d.def.damage } else { 100.0 };
                stats.take_damage(damage);
                stats.damage_flash = 1.0;
                stats.hit_from = Some(pt.translation);
                let s = d.def.sounds.get("m_ShockSound").cloned().unwrap_or_else(|| "Snd_P_Death_Light".into());
                sfx.write(PostEvent::named(&s, None));
                if stats.health <= 0.0 {
                    stats.dead = true;
                }
            }
            touching[i] = now_in;
        }
        // a bolt, dart or grenade meeting it is destroyed with a shock, nothing bursting or
        // sticking (the wall's `TakeDamage` from a causer that isn't a pawn)
        for &(e, from, to) in &paths {
            if gone.contains(&e) {
                continue;
            }
            if let Some(at) = wall_crossing(w, from, to) {
                gone.push(e);
                commands.entity(e).try_despawn();
                // (its own shock particles, `m_ShockParticleSystemTemplate`, turned with it)
                fx.write(crate::particles::SpawnEffect { secs: 0.8, system: d.def.fx.get("shock").copied(), rot: Quat::from_rotation_y(d.def.yaw), ..crate::particles::SpawnEffect::at("electrified", at) });
                strike(&mut fx, d, at, false);
                let s = d.def.sounds.get("m_ShockSound").cloned().unwrap_or_else(|| "Snd_P_Death_Light".into());
                sfx.write(PostEvent::named(&s, Some(at)));
                if std::env::var("DH_WALL_LOG").is_ok() {
                    info!("wall {}: destroys a thrown thing at {at:.2}", d.def.actor);
                }
            }
        }
        for (e, n, t) in &npcs {
            if n.is_down() {
                // a body touching it is electrocuted and vanishes, at a kill's cost; one carried
                // through is spared (`StateNPCMasterDead_BeingCarried`)
                // (not those it struck down itself, electrocuted as they stood)
                if n.mode == crate::npc::Mode::Dead && !ashes.contains(e) && !struck.contains(&e) && carry.body != Some(e) && inside(w, t.translation) {
                    commands.entity(e).try_insert(crate::npc::Electrocuted::new(false));
                    if let Some(seat) = d.receptacle_idx.and_then(|r| devices.list[r].tank_seat()) {
                        spent.push((seat, 1));
                    }
                }
                continue;
            }
            let spared = d.def.friendly.iter().any(|f| *f == n.faction);
            // rewired, it spares Corvo's friends instead
            let dies = if rewired { n.enemy } else { !spared && !n.faction.is_empty() };
            if std::env::var("DH_WALL_LOG").is_ok() && t.translation.distance(w.0.inverse().transform_point3((w.1 + w.2) * 0.5)) < 4.0 {
                info!("wall {}: {} at {:.2} inside {} dies {dies} (rewired {rewired}, enemy {}, {})", d.def.actor, n.name, t.translation, inside(w, t.translation), n.enemy, n.faction);
            }
            if dies && inside(w, t.translation) {
                hits.write(NpcHit { npc: e, damage: 999.0, kind: HitKind::WallOfLight, from: t.translation });
                // (one turned against its own: those who see it no longer trust it)
                if rewired {
                    commands.entity(e).try_insert(crate::distrust::DeviceKill(i));
                }
                strike(&mut fx, d, t.translation + Vec3::Y * 0.3, true);
                struck.insert(e);
                killed.push(d.def.actor.clone());
                // (a kill spends its tank's oil: `m_PawnChargeCost`)
                if let Some(seat) = d.receptacle_idx.and_then(|r| devices.list[r].tank_seat()) {
                    spent.push((seat, 1));
                }
                if let Some(s) = d.def.sounds.get("m_pStartKillEffectSound") {
                    sfx.write(PostEvent::named(s, Some(t.translation)));
                }
            }
        }
    }
    devices.wall_kills.extend(killed);
    devices.drains.extend(spent);
}

/// Where a path from `a` to `b` first meets a wall of light's box (world -> local matrix and
/// local bounds, grown a little). A bolt crosses the thin wall in less than a frame, so what it
/// would reach this frame counts, as the original's projectile sweep meets the wall before
/// whoever stands behind it.
fn wall_crossing(w: &(Mat4, Vec3, Vec3), a: Vec3, b: Vec3) -> Option<Vec3> {
    let (la, lb) = (w.0.transform_point3(a), w.0.transform_point3(b));
    let (lo, hi) = (w.1 - Vec3::splat(0.25), w.2 + Vec3::splat(0.25));
    let dir = lb - la;
    let (mut t0, mut t1) = (0.0f32, 1.0f32);
    for k in 0..3 {
        if dir[k].abs() < 1e-6 {
            if la[k] < lo[k] || la[k] > hi[k] {
                return None;
            }
            continue;
        }
        let (ta, tb) = ((lo[k] - la[k]) / dir[k], (hi[k] - la[k]) / dir[k]);
        t0 = t0.max(ta.min(tb));
        t1 = t1.min(ta.max(tb));
        if t0 > t1 {
            return None;
        }
    }
    Some(a + (b - a) * t0)
}

/// What is seen of a wall of light (the wall itself is hidden), as roshan's r614/blink research
/// found and the user's own Dishonored.exe shows (placement 0x00A28D80, tick 0x00A3EEE0): its
/// spark emitters on the pylons, lit while it is powered; and every `m_fMinSparkInterval` to
/// `m_fMaxSparkInterval` (2 to 3.5 s) a spark (`m_CrossSparkEffect`) set on one of them with its
/// beam's end on another, for `m_fSparkDuration` (0.17 s), seen while it is powered (the wall's
/// own component, hidden with it).
fn wall_sparks(
    time: Res<Time>,
    mut devices: ResMut<Devices>,
    mut emitters: Query<&mut crate::particles::ParticleEmitter>,
    mut fx: MessageWriter<crate::particles::SpawnEffect>,
) {
    let dt = time.delta_secs();
    let live: Vec<bool> = devices.list.iter().map(|d| d.def.kind == "WallOfLight" && devices.powered(d)).collect();
    let lit: std::collections::HashMap<u32, bool> = devices.list.iter().zip(&live).flat_map(|(d, on)| d.def.spark_particles.iter().map(move |p| (*p, *on))).collect();
    if !lit.is_empty() {
        for mut em in &mut emitters {
            if let Some(&on) = lit.get(&em.index) {
                if em.active != on {
                    em.active = on;
                }
            }
        }
    }
    for (d, on) in devices.list.iter_mut().zip(live) {
        let n = d.def.sparks.len();
        let Some(&sys) = d.def.fx.get("cross_spark").filter(|_| n >= 2) else { continue };
        d.spark_t -= dt;
        if d.spark_t > 0.0 {
            continue;
        }
        let p = |k: &str, v: f32| d.def.params.get(k).copied().unwrap_or(v);
        let (lo, hi, dur) = (p("m_fMinSparkInterval", 0.2), p("m_fMaxSparkInterval", 1.5), p("m_fSparkDuration", 0.08));
        d.spark_t = lo + rand::random::<f32>() * (hi - lo).max(0.0);
        if !on {
            continue;
        }
        // (two different ones)
        let a = rand::random::<u32>() as usize % n;
        let mut b = rand::random::<u32>() as usize % n;
        if b == a {
            b = (b + 1) % n;
        }
        if std::env::var("DH_WALL_LOG").is_ok() {
            info!("wall {}: a spark from {a} to {b}", d.def.actor);
        }
        fx.write(crate::particles::SpawnEffect {
            system: Some(sys),
            rot: Quat::from_rotation_y(d.def.yaw),
            secs: dur.max(0.05),
            beam_to: Some(Vec3::from(d.def.sparks[b])),
            ..crate::particles::SpawnEffect::at("wall_spark", Vec3::from(d.def.sparks[a]))
        });
    }
}

/// How near one of its own keeps a guard from ringing at a sighting (`m_fCompanionRange`, 2000 uu).
const COMPANION_RANGE: f32 = 20.0;

/// Arc pylons: an intruder in the cylinder makes them charge, then strike.
#[allow(clippy::too_many_arguments)]
fn pylons(
    time: Res<Time>,
    mut devices: ResMut<Devices>,
    mut stats: ResMut<PlayerStats>,
    mut sfx: MessageWriter<PostEvent>,
    mut hits: MessageWriter<NpcHit>,
    mut noise: MessageWriter<Noise>,
    mut gizmos: Gizmos,
    player: Query<&Transform, With<Player>>,
    npcs: Query<(Entity, &Npc, &Transform), Without<Player>>,
    mut commands: Commands,
) {
    let dt = time.delta_secs();
    let ppos = player.single().map(|t| t.translation).ok();
    let mut spent = Vec::new();
    for i in 0..devices.list.len() {
        if devices.list[i].def.kind != "ArcPylon" {
            continue;
        }
        let seat = devices.list[i].receptacle_idx.and_then(|r| devices.list[r].tank_seat());
        let powered = devices.powered(&devices.list[i]);
        let rewired = devices.rewired(&devices.list[i]);
        let d = &mut devices.list[i];
        if !powered {
            d.phase = Phase::Idle;
            continue;
        }
        let base = Vec3::from(d.def.detect_at);
        let inside = |p: Vec3| (p - base).with_y(0.0).length() < d.def.radius && (p.y - base.y).abs() < d.def.height;
        // the target: Corvo, or (rewired) the nearest of his enemies
        let target: Option<(Option<Entity>, Vec3)> = if rewired {
            npcs.iter().filter(|(_, n, t)| !n.is_down() && n.enemy && inside(t.translation)).map(|(e, _, t)| (Some(e), t.translation)).next()
        } else {
            ppos.filter(|p| inside(*p) && !stats.dead).map(|p| (None, p))
        };
        d.cooldown = (d.cooldown - dt).max(0.0);
        let charge = d.def.params.get("m_fChargeDuration").copied().unwrap_or(1.0) + d.def.params.get("m_fBeforeFireDuration").copied().unwrap_or(0.25);
        let fire = d.def.params.get("m_fFireDuration").copied().unwrap_or(0.75);
        d.phase = match (d.phase, target) {
            (Phase::Idle, Some(_)) if d.cooldown <= 0.0 => {
                if let Some(s) = d.def.sounds.get("m_pStartChargingSound") {
                    sfx.write(PostEvent::named(s, Some(base)));
                }
                Phase::Charging(0.0)
            }
            (Phase::Charging(t), Some((who, at))) => {
                if t + dt >= charge {
                    if let Some(s) = d.def.sounds.get("m_pStartFireSound") {
                        sfx.write(PostEvent::named(s, Some(base)));
                    }
                    let damage = if d.def.damage > 0.0 { d.def.damage } else { 50.0 };
                    // (a strike spends its tank's oil)
                    if let Some(s) = seat {
                        spent.push((s, 1));
                    }
                    match who {
                        Some(e) => {
                            hits.write(NpcHit { npc: e, damage, kind: HitKind::Bullet, from: base });
                            // (one it kills, turned against its own: those who see it no longer
                            // trust it)
                            if npcs.get(e).is_ok_and(|(_, n, _)| n.health <= damage) {
                                commands.entity(e).try_insert(crate::distrust::DeviceKill(i));
                            }
                        }
                        None => {
                            stats.take_damage(damage);
                            stats.damage_flash = 1.0;
                            stats.hit_from = Some(base);
                            if stats.health <= 0.0 {
                                stats.dead = true;
                            }
                        }
                    }
                    noise.write(Noise { corvo: false, pos: base, radius: 20.0, kind: NoiseKind::Threatening });
                    gizmos.line(base + Vec3::Y * d.def.height * 0.9, at, Color::srgb(0.6, 0.85, 1.0));
                    Phase::Firing(0.0)
                } else {
                    Phase::Charging(t + dt)
                }
            }
            (Phase::Charging(_), None) => Phase::Idle,
            (Phase::Firing(t), tgt) => {
                if let Some((_, at)) = tgt.filter(|_| t < 0.15) {
                    gizmos.line(base + Vec3::Y * d.def.height * 0.9, at, Color::srgb(0.6, 0.85, 1.0));
                }
                if t + dt >= fire {
                    d.cooldown = 0.5;
                    Phase::Idle
                } else {
                    Phase::Firing(t + dt)
                }
            }
            (p, _) => p,
        };
    }
    devices.drains.extend(spent);
}

/// Alarm bells: guards in a fight nearby ring them; the whole area comes running, and the
/// bell's spawners send reinforcements (`m_MaxSpawnedPawnsPerRing`).
#[allow(clippy::too_many_arguments)]
fn alarms(
    time: Res<Time>,
    level: Option<Res<LevelInfo>>,
    mut devices: ResMut<Devices>,
    mut sfx: MessageWriter<PostEvent>,
    mut spawn: MessageWriter<SpawnRequest>,
    player: Query<&Transform, With<Player>>,
    mut npcs: Query<(Entity, &mut Npc, &Transform), Without<Player>>,
    mut stats: ResMut<PlayerStats>,
    mut noise: MessageWriter<Noise>,
    mut global_cooldown: Local<f32>,
) {
    let dt = time.delta_secs();
    let ppos = player.single().map(|t| t.translation).unwrap_or(Vec3::ZERO);
    *global_cooldown = (*global_cooldown - dt).max(0.0);
    // (the standing guards, by faction: a ringer's companions)
    let guards: Vec<(String, Vec3)> = npcs.iter().filter(|(_, n, _)| n.hostile() && !n.is_down()).map(|(_, n, t)| (n.faction.clone(), t.translation)).collect();
    for i in 0..devices.list.len() {
        if devices.list[i].def.kind != "AlarmBell" {
            continue;
        }
        let d = &mut devices.list[i];
        let pos = Vec3::from(d.def.position);
        d.cooldown = (d.cooldown - dt).max(0.0);
        match d.phase {
            Phase::Ringing(t) => {
                // after `m_fAlarmRingDelayBeforeAINoise` the area hears it (the bell's noise:
                // `Loudness4`, an alarm) and comes to look; the guards who saw Corvo know where
                // he was
                let delay = d.def.params.get("m_fAlarmRingDelayBeforeAINoise").copied().unwrap_or(2.5);
                if t < delay && t + dt >= delay {
                    noise.write(Noise { corvo: false, pos, radius: 60.0, kind: NoiseKind::Alarm });
                    for (_, mut n, nt) in &mut npcs {
                        if n.hostile() && !n.is_down() && nt.translation.distance(pos) < 60.0 && n.mode != Mode::Combat && n.alert == Alert::Combat {
                            n.last_seen = Some(ppos);
                            n.set_mode(Mode::Search);
                        }
                    }
                }
                // it rings while anyone still hunts him; then every bell waits
                // `DisAlarmGlobalManager.m_fAlarmCooldown`
                let hunted = npcs.iter().any(|(_, n, _)| n.hostile() && !n.is_down() && n.alert == Alert::Combat);
                d.phase = if t > delay + 1.0 && !hunted { Phase::Idle } else { Phase::Ringing(t + dt) };
                if d.phase == Phase::Idle {
                    *global_cooldown = 10.0;
                    if let Some(s) = d.def.sounds.get("m_pAlarmRingEndEvent") {
                        sfx.write(PostEvent::named(s, Some(pos)));
                    }
                }
            }
            _ => {
                // a guard who has found Corvo (busted) within `m_fNPCGoToAlarmCloseness` of the
                // bell runs to it, the nearest of them, unless Corvo is on him (`m_fFightRadius`
                // 5 m); within `m_fNPCUseCloseness` it waits `m_fWaitTimeBeforeRing` and rings
                // (`DisBehaviorTriggerAlarm`, as roshan's r614/blink research decoded it)
                let reach = d.def.params.get("m_fNPCGoToAlarmCloseness").copied().unwrap_or(5000.0) * 0.01;
                let usable = d.def.params.get("m_fNPCUseCloseness").copied().unwrap_or(150.0) * 0.01 + 0.6;
                // (`TweaksBehavior_TriggerAlarm_Default`: a sighting rings it alone, none of its own
                // within `m_fCompanionRange` 20 m (`m_fAlarmChanceUponSighting_Alone` 1, in a group
                // 0), a death it saw rings it either way (`m_fAlarmChanceUponWitnessDeath` 1)
                let alone = |n: &Npc, at: Vec3| !guards.iter().any(|(f, p)| *f == n.faction && p.distance(at) > 0.05 && p.distance(at) < COMPANION_RANGE);
                let may_ring = |n: &Npc, at: Vec3| n.hostile() && !n.is_down() && n.alert == Alert::Combat && n.kind == crate::npc::Kind::Guard && at.distance(ppos) > 5.0 && (n.witnessed_death || alone(n, at));
                // (the one on the way gives up: down, fighting him now, or unable to get there;
                // then the bell rests a while)
                if let Some(who) = d.ringer {
                    d.ringer_t += dt;
                    let lost = d.ringer_t > 20.0;
                    if lost {
                        d.cooldown = d.cooldown.max(10.0);
                    }
                    let keep = !lost && npcs.get(who).is_ok_and(|(_, n, nt)| n.alarm_run == Some(pos) && may_ring(n, nt.translation));
                    if !keep {
                        if let Ok((_, mut n, _)) = npcs.get_mut(who) {
                            n.alarm_run = None;
                        }
                        d.ringer = None;
                        d.fighting_near = 0.0;
                    }
                }
                if d.ringer.is_none() && d.cooldown <= 0.0 && *global_cooldown <= 0.0 {
                    let pick = npcs
                        .iter()
                        .filter(|(_, n, t)| n.alarm_run.is_none() && may_ring(n, t.translation) && t.translation.distance(pos) < reach)
                        .min_by(|a, b| a.2.translation.distance(pos).total_cmp(&b.2.translation.distance(pos)))
                        .map(|(e, _, _)| e);
                    if let Some(e) = pick {
                        if let Ok((_, mut n, _)) = npcs.get_mut(e) {
                            n.alarm_run = Some(pos);
                            // (sent to ring it: it suspects, `ForceRingAlarm`)
                            n.suspecting = true;
                            n.bark_req = (n.bark_req.0.wrapping_add(1), "ALARM_GO_TO_RING_ALARM");
                            info!("alarm: {} runs to ring the bell", n.name);
                        }
                        d.ringer = Some(e);
                        d.ringer_t = 0.0;
                        d.fighting_near = 0.0;
                    }
                }
                let mut rung = false;
                if let Some(who) = d.ringer {
                    if let Ok((_, mut n, nt)) = npcs.get_mut(who) {
                        if nt.translation.distance(pos) < usable {
                            d.fighting_near += dt;
                            if d.fighting_near >= 0.25 {
                                n.alarm_run = None;
                                // (its lever pulled: `Sword_TriggerAlarm`)
                                n.gesture(18);
                                rung = true;
                            }
                        }
                    }
                    if rung {
                        d.ringer = None;
                        d.fighting_near = 0.0;
                    }
                }
                if rung {
                    d.phase = Phase::Ringing(0.0);
                    stats.alarms_rung += 1;
                    stats.deeds_pending.push(("ePlayerStat_NumAlarmsTriggered", String::new()));
                    d.cooldown = d.def.params.get("m_fTimeInBetweenUses").copied().unwrap_or(30.0);
                    if let Some(s) = d.def.sounds.get("m_pAlarmRingBeginEvent") {
                        sfx.write(PostEvent::named(s, Some(pos)));
                    }
                    // its spawners come: those far enough from Corvo
                    // (`m_fSpawnerMinDistFromPlayer`) first, up to `m_MaxSpawnedPawnsPerRing`
                    if let Some(level) = level.as_ref() {
                        let max = d.def.params.get("m_MaxSpawnedPawnsPerRing").copied().unwrap_or(2.0) as usize;
                        let min_dist = d.def.params.get("m_fSpawnerMinDistFromPlayer").copied().unwrap_or(1500.0) * 0.01;
                        let mut list: Vec<(usize, bool)> = d
                            .def
                            .spawners
                            .iter()
                            .filter_map(|name| level.scene.spawners.iter().position(|s| s.name == *name))
                            .map(|si| (si, Vec3::from(level.scene.spawners[si].position).distance(ppos) >= min_dist))
                            .collect();
                        if list.iter().any(|(_, far)| *far) {
                            list.retain(|(_, far)| *far);
                        }
                        for (si, _) in list.into_iter().take(max) {
                            spawn.write(SpawnRequest(si as u32));
                        }
                    }
                }
            }
        }
    }
}
