//! Security systems (the original `DisWallOfLight`, `DisDefenceTower` arc pylons,
//! `DisWatchTower`, `DisAlarmBell`), powered by whale oil tanks in receptacles: take the tank
//! and the device dies; rewire it (a rewire tool at the receptacle) and it turns on its
//! owners. Walls of light disintegrate whoever crosses them unless their faction is spared;
//! pylons charge and strike intruders inside their detection cylinder; alarm bells, rung by
//! guards in a fight, call everyone nearby and the reinforcements of their spawners.

use crate::audio::PostEvent;
use crate::gameplay::{HitKind, HudMessages, Noise, NpcHit, PlayerStats};
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
            .add_systems(Update, (use_devices, tank_seats, walls, eyes, pylons, alarms, receptacle_events).chain().run_if(in_state(GameState::InGame)))
            .add_systems(Update, insert_tank.after(crate::interact::FocusSet).before(crate::interact::use_focus).run_if(in_state(GameState::InGame)));
    }
}

/// Usable ids above this are security parts (tanks, receptacle panels, bells), not level
/// script actors.
pub const SECURITY_USABLE: u32 = 0xFFFF_0000;

#[derive(Clone, Copy, PartialEq, Default, Debug)]
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
    /// receptacles: the tank is in
    pub has_tank: bool,
    pub rewired: bool,
    phase: Phase,
    cooldown: f32,
    /// walls of light: the wall's box (world -> local matrix and local bounds)
    wall: Option<(Mat4, Vec3, Vec3)>,
    /// level instances of the actor (tanks to hide)
    instances: Vec<Entity>,
    /// its receptacle (`list`): the nearest of its name (sublevels reuse names)
    receptacle_idx: Option<usize>,
    /// turned off by the level's scripts (`DisSeqAct_WallofLightControl`)
    pub off: bool,
}

impl Device {
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
}

impl Device {
    /// Its level instances.
    pub fn instances(&self) -> &[Entity] {
        &self.instances
    }
}

impl Devices {
    /// Is a device fed: its receptacle holds a tank (devices without one always run).
    fn powered(&self, d: &Device) -> bool {
        if d.off {
            return false;
        }
        if d.def.receptacle.is_empty() {
            return true;
        }
        d.receptacle_idx.map(|r| self.list[r].has_tank).unwrap_or(true)
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
    pub fn rewire_near(&mut self, label: &str, at: Vec3) -> bool {
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
        match near {
            Some((i, _)) => {
                self.list[i].rewired = true;
                // an alarm's panel silences it
                if self.list[i].def.kind == "AlarmBell" {
                    self.list[i].off = true;
                }
                true
            }
            None => false,
        }
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
}

/// Whale oil tanks that are props: a receptacle is fed while a tank sits in its seat (as the
/// level placed it, or put back).
fn tank_seats(
    mut devices: ResMut<Devices>,
    level: Option<Res<LevelInfo>>,
    tanks: Query<(Entity, &crate::props::Prop, &Transform)>,
    held: Res<crate::props::Held>,
    mut sfx: MessageWriter<PostEvent>,
) {
    let Some(level) = level else { return };
    let is_tank = |p: &crate::props::Prop| level.scene.movables.get(p.index).is_some_and(|m| m.tank.is_some());
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
        let filled = tanks.iter().any(|(e, p, t)| is_tank(p) && held.0 != Some(e) && t.translation.distance(at) < 0.5);
        let d = &mut devices.list[r];
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

/// The visible sheet of a wall of light (the original draws it with beam particles).
#[derive(Component)]
struct WallSheet(usize);

#[allow(clippy::too_many_arguments)]
fn setup(
    mut commands: Commands,
    level: Option<Res<LevelInfo>>,
    mut devices: ResMut<Devices>,
    instances: Query<(Entity, &LevelInstance)>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut mats: ResMut<Assets<StandardMaterial>>,
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
        if s.kind == "WallOfLight" {
            // the wall: its (editor-only) mesh's box
            if let Some(inst) = insts.first().and_then(|x| scene.instances.get(x.1 as usize)) {
                if let Some(m) = scene.meshes.get(inst.mesh as usize) {
                    let world = Mat4::from_cols_array(&inst.transform);
                    let (min, max) = (Vec3::from(m.min), Vec3::from(m.max));
                    d.wall = Some((world.inverse(), min, max));
                    // its light: a thin glowing sheet over the wall's box
                    let size = (max - min).max(Vec3::splat(0.02));
                    let mat = mats.add(StandardMaterial {
                        base_color: Color::srgba(0.35, 0.75, 1.0, 0.06),
                        emissive: LinearRgba::rgb(0.5, 1.6, 3.2),
                        alpha_mode: AlphaMode::Add,
                        unlit: true,
                        double_sided: true,
                        cull_mode: None,
                        ..default()
                    });
                    let local = Transform::from_translation((min + max) * 0.5);
                    commands.spawn((
                        WallSheet(i),
                        Mesh3d(meshes.add(Cuboid::new(size.x.min(0.03), size.y, size.z))),
                        MeshMaterial3d(mat),
                        Transform::from_matrix(world * local.to_matrix()),
                        bevy::light::NotShadowCaster,
                        DespawnOnExit(GameState::InGame),
                    ));
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
        let label = match s.kind.as_str() {
            "Battery" if tank_prop => None,
            "Battery" => Some("Take Whale Oil Tank"),
            "Receptacle" if !circuitry_near => Some("Rewire"),
            "AlarmBell" => Some("Silence Alarm"),
            _ => None,
        };
        if let Some(label) = label {
            commands.spawn((
                Usable { actor: SECURITY_USABLE + i as u32, label: label.into() },
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
                msgs.push("Whale oil tank removed");
            }
            "Receptacle" => {
                let tools = stats.items.get("RewireTool_twk").copied().unwrap_or(0);
                if devices.list[i].rewired {
                    msgs.push("Already rewired");
                } else if tools == 0 {
                    msgs.push("A Rewire Tool is needed");
                } else {
                    stats.items.insert("RewireTool_twk".into(), tools - 1);
                    devices.list[i].rewired = true;
                    sfx.write(PostEvent::named("UI_Validation", Some(Vec3::from(d.def.position))));
                    msgs.push("Rewired: it now protects you");
                }
            }
            "AlarmBell" => {
                if let Phase::Ringing(_) = devices.list[i].phase {
                    devices.list[i].phase = Phase::Idle;
                    let s = d.def.sounds.get("m_pAlarmRingEndEvent").cloned().unwrap_or_else(|| "Alarm_Stop".into());
                    sfx.write(PostEvent::named(&s, Some(Vec3::from(d.def.position))));
                    msgs.push("Alarm silenced");
                }
            }
            _ => {}
        }
    }
}

/// Walls of light: crossing one while it runs is death, unless its friends (or, rewired,
/// Corvo).
#[allow(clippy::too_many_arguments)]
fn walls(
    mut devices: ResMut<Devices>,
    mut stats: ResMut<PlayerStats>,
    mut msgs: ResMut<HudMessages>,
    (mut sfx, mut fx): (MessageWriter<PostEvent>, MessageWriter<crate::particles::SpawnEffect>),
    mut hits: MessageWriter<NpcHit>,
    player: Query<&Transform, With<Player>>,
    npcs: Query<(Entity, &Npc, &Transform), Without<Player>>,
    mut ambient: Local<Vec<bool>>,
    (time, mut sheets): (Res<Time>, Query<(&WallSheet, &mut Visibility, &MeshMaterial3d<StandardMaterial>)>),
    mut mats: ResMut<Assets<StandardMaterial>>,
) {
    // the sheets show while their wall runs (blue; rewired, green), flickering
    let flicker = 0.8 + 0.2 * (time.elapsed_secs() * 23.0).sin() * (time.elapsed_secs() * 7.0).cos();
    for (s, mut v, m) in &mut sheets {
        let Some(d) = devices.list.get(s.0) else { continue };
        let on = devices.powered(d);
        let want = if on { Visibility::Inherited } else { Visibility::Hidden };
        if *v != want {
            *v = want;
        }
        if on {
            if let Some(mut mat) = mats.get_mut(&m.0) {
                let c = if devices.rewired(d) { Vec3::new(0.2, 0.9, 0.3) } else { Vec3::new(0.15, 0.5, 1.1) } * flicker;
                mat.emissive = LinearRgba::rgb(c.x, c.y, c.z);
            }
        }
    }
    if ambient.len() != devices.list.len() {
        *ambient = vec![false; devices.list.len()];
    }
    let inside = |w: &(Mat4, Vec3, Vec3), p: Vec3| {
        let l = w.0.transform_point3(p);
        l.cmpge(w.1 - Vec3::splat(0.25)).all() && l.cmple(w.2 + Vec3::splat(0.25)).all()
    };
    let mut killed = Vec::new();
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
        if !on {
            continue;
        }
        let rewired = devices.rewired(d);
        if let Ok(pt) = player.single() {
            if !rewired && !stats.dead && inside(w, pt.translation) {
                stats.health = 0.0;
                stats.dead = true;
                msgs.push("Disintegrated by a Wall of Light");
                let s = d.def.sounds.get("m_ShockSound").cloned().unwrap_or_else(|| "Snd_P_Death_Light".into());
                sfx.write(PostEvent::named(&s, None));
            }
        }
        for (e, n, t) in &npcs {
            if n.is_down() {
                continue;
            }
            let spared = d.def.friendly.iter().any(|f| *f == n.faction);
            // rewired, it spares Corvo's friends instead
            let dies = if rewired { n.enemy } else { !spared && !n.faction.is_empty() };
            if dies && inside(w, t.translation) {
                hits.write(NpcHit { npc: e, damage: 999.0, kind: HitKind::Fatality, from: t.translation });
                killed.push(d.def.actor.clone());
                fx.write(crate::particles::SpawnEffect { follow: Some(e), secs: 1.5, ..crate::particles::SpawnEffect::at("electrified", Vec3::Y * 0.9) });
                if let Some(s) = d.def.sounds.get("m_pStartKillEffectSound") {
                    sfx.write(PostEvent::named(s, Some(t.translation)));
                }
            }
        }
    }
    devices.wall_kills.extend(killed);
}

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
) {
    let dt = time.delta_secs();
    let ppos = player.single().map(|t| t.translation).ok();
    for i in 0..devices.list.len() {
        if devices.list[i].def.kind != "ArcPylon" {
            continue;
        }
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
                    match who {
                        Some(e) => {
                            hits.write(NpcHit { npc: e, damage, kind: HitKind::Bullet, from: base });
                        }
                        None => {
                            stats.health = (stats.health - damage).max(0.0);
                            stats.damage_flash = 1.0;
                            stats.hit_from = Some(base);
                            if stats.health <= 0.0 {
                                stats.dead = true;
                            }
                        }
                    }
                    noise.write(Noise { pos: base, radius: 20.0, combat: true });
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
    mut msgs: ResMut<HudMessages>,
    player: Query<&Transform, With<Player>>,
    mut npcs: Query<(&mut Npc, &Transform), Without<Player>>,
    mut fighting_near: Local<HashMap<usize, f32>>,
    mut stats: ResMut<PlayerStats>,
) {
    let dt = time.delta_secs();
    let ppos = player.single().map(|t| t.translation).unwrap_or(Vec3::ZERO);
    for i in 0..devices.list.len() {
        if devices.list[i].def.kind != "AlarmBell" {
            continue;
        }
        let d = &mut devices.list[i];
        let pos = Vec3::from(d.def.position);
        d.cooldown = (d.cooldown - dt).max(0.0);
        match d.phase {
            Phase::Ringing(t) => {
                // the area hears it and converges on Corvo's last known position
                if t < 0.1 {
                    for (mut n, nt) in &mut npcs {
                        if n.hostile() && !n.is_down() && nt.translation.distance(pos) < 60.0 && n.mode != Mode::Combat {
                            n.alert = Alert::Combat;
                            n.awareness = n.awareness.max(0.9);
                            n.last_seen = Some(ppos);
                            n.set_mode(Mode::Search);
                        }
                    }
                }
                d.phase = if t + dt > 30.0 { Phase::Idle } else { Phase::Ringing(t + dt) };
                if d.phase == Phase::Idle {
                    if let Some(s) = d.def.sounds.get("m_pAlarmRingEndEvent") {
                        sfx.write(PostEvent::named(s, Some(pos)));
                    }
                }
            }
            _ => {
                // a guard fighting near the bell goes and rings it
                let reach = d.def.params.get("m_fNPCGoToAlarmCloseness").copied().unwrap_or(3500.0) * 0.01;
                let fighting = npcs.iter().any(|(n, t)| n.hostile() && n.mode == Mode::Combat && !n.is_down() && t.translation.distance(pos) < reach);
                let w = fighting_near.entry(i).or_insert(0.0);
                *w = if fighting { *w + dt } else { 0.0 };
                // the time it takes a guard to get there
                if *w > 4.0 && d.cooldown <= 0.0 {
                    *w = 0.0;
                    d.phase = Phase::Ringing(0.0);
                    stats.alarms_rung += 1;
                    d.cooldown = d.def.params.get("m_fTimeInBetweenUses").copied().unwrap_or(30.0);
                    if let Some(s) = d.def.sounds.get("m_pAlarmRingBeginEvent") {
                        sfx.write(PostEvent::named(s, Some(pos)));
                    }
                    msgs.push("The alarm has been raised");
                    if let Some(level) = level.as_ref() {
                        let max = d.def.params.get("m_MaxSpawnedPawnsPerRing").copied().unwrap_or(2.0) as usize;
                        for name in d.def.spawners.iter().take(max) {
                            if let Some(si) = level.scene.spawners.iter().position(|s| s.name == *name) {
                                spawn.write(SpawnRequest(si as u32));
                            }
                        }
                    }
                }
            }
        }
    }
}
