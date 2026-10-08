//! The AI's places in the world and the spawners' squads, as the original has them:
//! - forbidden zones (`DisForbiddenZone`): a character of an owning faction that sees someone
//!   of a forbidden faction inside an enabled zone takes them for an enemy while they're there
//!   (Lady Boyle's guards and her upper floors), the scripts setting and clearing zones'
//!   factions (`DisSeqAct_ForbiddenZoneOverride`);
//! - tethers (`DisTetherVolume`, the spawner's `m_TetherVolumes`): a character fighting or
//!   searching goes home rather than leave them (`DisBehaviorGoHome`);
//! - flee points (`DisFleePointActor`): a fleeing character runs to the nearest of its squad's
//!   flee points away from the danger, and on along their chain, the level scripts hearing it
//!   arrive (`DisSeqEvent_FleepointReached`: an alarm rung) — `npc.rs`;
//! - watch points and hideouts (`DisGuardWatchPoint`, `DisHideoutVolume` /
//!   `DisHideoutAccessPoint`): a searching guard checks the watch points about where Corvo was
//!   lost, watching each for its time, and looks into a hideout he was last seen in from its
//!   access point — `npc.rs`;
//! - ambushes (`DisAmbushPoint`, `DisSeqAct_AIAmbush`): a character lies in wait at its point and
//!   springs when Corvo comes into the point's area;
//! - guards protecting the neutral (`DisTweaks_AIBrain.m_bProtectNeutrals`,
//!   `DisSeqAct_AIProtectNeutralsOverride`): Corvo striking a neutral in their sight turns them
//!   on him;
//! - reinforcements: spawners waiting for the alarm (`m_SpawnAtSuspicionLevel`) or their squad's
//!   call for help (`m_bSpawnOnHearHelpRequest`) spawn then, unseen by Corvo and away from him
//!   unless allowed (`m_bCanSpawnWhenVisible`, `m_bCanSpawnWhenPlayerNear`).

use crate::gameplay::{HitKind, NpcHit};
use crate::kismet::Val;
use crate::level::LevelInfo;
use crate::npc::{Alert, FromSpawner, Mode, Npc, SpawnRequest};
use crate::player::{Player, PlayerCamera};
use crate::GameState;
use bevy::prelude::*;
use bevy_rapier3d::prelude::*;
use std::collections::{HashMap, HashSet};

pub struct AiWorldPlugin;

impl Plugin for AiWorldPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<AiWorld>()
            .add_message::<FleePointReached>()
            .add_systems(OnEnter(GameState::InGame), build_ai_world.after(crate::level::LevelSpawnSet))
            .add_systems(Update, (script_commands, trespass, protect_neutrals).chain().before(crate::npc::npc_perception).run_if(in_state(GameState::InGame)))
            .add_systems(Update, (tethers, ambushes, reinforcements).chain().after(crate::npc::npc_brain).run_if(in_state(GameState::InGame)));
    }
}

/// A fleeing character reached a flee point: its name and the character's spawner.
#[derive(Message, Clone)]
pub struct FleePointReached {
    pub point: String,
    pub spawner: u32,
}

struct Zone {
    name: String,
    hulls: Vec<Collider>,
    owners: Vec<String>,
    forbidden: Vec<String>,
    enabled: bool,
    /// as the level has it (for `Clear`)
    initial: (Vec<String>, Vec<String>, bool),
}

#[derive(Resource, Default)]
pub struct AiWorld {
    zones: Vec<Zone>,
    /// tether volumes by `scene.volumes` index
    tethers: HashMap<u32, Vec<Collider>>,
    hideouts: Vec<Vec<Collider>>,
    /// possession volumes Corvo can't leave his host in
    possession: Vec<Vec<Collider>>,
    pub markers: dhcook::format::AiMarkers,
    /// spawners already called in as reinforcements
    reinforced: HashSet<u32>,
    /// squads that called for help
    called: HashSet<String>,
}

fn inside(hulls: &[Collider], p: Vec3) -> bool {
    hulls.iter().any(|c| c.contains_point(Vec3::ZERO, Quat::IDENTITY, p))
}

impl AiWorld {
    /// The flee point a character of `squad` at `pos` runs to from danger at `threat`: the
    /// nearest of its squad's (or any squad's, none being its) not towards the danger.
    pub fn flee_target(&self, squad: &str, pos: Vec3, threat: Vec3) -> Option<usize> {
        let f = &self.markers.flee;
        let mine: Vec<usize> = (0..f.len()).filter(|&i| !squad.is_empty() && f[i].squads.iter().any(|s| s.eq_ignore_ascii_case(squad))).collect();
        let pool: Vec<usize> = if mine.is_empty() { (0..f.len()).filter(|&i| f[i].squads.is_empty()).collect() } else { mine };
        let away = |i: &usize| Vec3::from(f[*i].position).distance(threat) > pos.distance(threat) * 0.8;
        pool.iter().copied().filter(away).min_by(|a, b| Vec3::from(f[*a].position).distance(pos).total_cmp(&Vec3::from(f[*b].position).distance(pos)))
    }
    pub fn flee_point(&self, i: usize) -> Option<&dhcook::format::FleePoint> {
        self.markers.flee.get(i)
    }
    /// A watch point within `r` of `center` the guard hasn't checked yet.
    pub fn watch_near(&self, center: Vec3, r: f32, used: &[usize]) -> Option<usize> {
        let w = &self.markers.watch;
        (0..w.len())
            .filter(|i| !used.contains(i))
            .map(|i| (i, Vec3::from(w[i].position).distance(center)))
            .filter(|(_, d)| *d < r)
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(i, _)| i)
    }
    pub fn watch_point(&self, i: usize) -> Option<&dhcook::format::WatchPoint> {
        self.markers.watch.get(i)
    }
    /// Where a guard looks into the hideout `p` is in (its nearest access point).
    pub fn hideout_access(&self, p: Vec3) -> Option<(Vec3, f32)> {
        if !self.hideouts.iter().any(|h| inside(h, p)) {
            return None;
        }
        self.markers
            .hideout_access
            .iter()
            .map(|a| (Vec3::new(a[0], a[1], a[2]), a[3]))
            .filter(|(at, _)| at.distance(p) < 8.0)
            .min_by(|a, b| a.0.distance(p).total_cmp(&b.0.distance(p)))
    }
    /// Whether Corvo can't leave his host here (`DisPossessionVolume` `m_bDisallowUnpossession`:
    /// a rat in a pipe).
    pub fn no_unpossess(&self, p: Vec3) -> bool {
        self.possession.iter().any(|h| inside(h, p))
    }
    /// Whether a character stays within its tether volumes there (none: anywhere).
    pub fn within_tether(&self, tether: &[u32], p: Vec3) -> bool {
        tether.is_empty() || tether.iter().filter_map(|v| self.tethers.get(v)).any(|h| inside(h, p)) || !tether.iter().any(|v| self.tethers.contains_key(v))
    }
}

fn hulls_of(v: &dhcook::format::Volume) -> Vec<Collider> {
    v.hulls.iter().filter_map(|h| Collider::convex_hull(&h.iter().map(|p| Vec3::from(*p)).collect::<Vec<_>>())).collect()
}

fn build_ai_world(level: Option<Res<LevelInfo>>, mut ai: ResMut<AiWorld>) {
    *ai = AiWorld::default();
    let Some(level) = level else { return };
    for (i, v) in level.scene.volumes.iter().enumerate() {
        match v.kind.as_str() {
            "DisForbiddenZone" => {
                let z = v.zone.clone().unwrap_or_default();
                ai.zones.push(Zone { name: v.name.clone(), hulls: hulls_of(v), owners: z.owners.clone(), forbidden: z.forbidden.clone(), enabled: z.enabled, initial: (z.owners, z.forbidden, z.enabled) });
            }
            "DisTetherVolume" => {
                ai.tethers.insert(i as u32, hulls_of(v));
            }
            "DisHideoutVolume" => {
                let h = hulls_of(v);
                ai.hideouts.push(h);
            }
            "DisPossessionVolume" if v.no_unpossess => {
                let h = hulls_of(v);
                ai.possession.push(h);
            }
            _ => {}
        }
    }
    ai.markers = level.scene.ai_markers.clone();
    let m = &ai.markers;
    if !ai.zones.is_empty() || !m.flee.is_empty() || !m.watch.is_empty() {
        info!(
            "AI places: {} forbidden zones, {} tether volumes, {} watch points, {} flee points, {} hideouts, {} ambush points",
            ai.zones.len(),
            ai.tethers.len(),
            m.watch.len(),
            m.flee.len(),
            ai.hideouts.len(),
            m.ambush.len()
        );
    }
}

/// A faction's name in the scripts' terms (`Boyle_Factions.Neutral_Guard` -> `Neutral_Guard`).
fn faction_name(v: &Val) -> Option<String> {
    match v {
        Val::Str(s) => Some(s.rsplit('.').next().unwrap_or(s).to_string()),
        _ => None,
    }
}

/// The scripts' orders: forbidden zones' factions, guards protecting the neutral or not,
/// characters' flags, ambushes.
fn script_commands(vm: Option<ResMut<crate::kismet::Vm>>, mut ai: ResMut<AiWorld>, mut npcs: Query<(&mut Npc, &FromSpawner)>) {
    let Some(mut vm) = vm else { return };
    let g = vm.g.clone();
    let spawners = |vals: &[Val]| -> Vec<u32> { vals.iter().filter_map(|v| if let Val::Actor(a) = v { g.actors.get(*a as usize)?.spawner } else { None }).collect() };
    for (targets, owners, forbidden, set) in std::mem::take(&mut vm.zone_overrides) {
        let names: Vec<String> = targets.iter().filter_map(|v| if let Val::Actor(a) = v { g.actors.get(*a as usize).map(|a| a.name.clone()) } else { None }).collect();
        for z in ai.zones.iter_mut().filter(|z| names.contains(&z.name)) {
            if set {
                let o: Vec<String> = owners.iter().filter_map(faction_name).collect();
                let f: Vec<String> = forbidden.iter().filter_map(faction_name).collect();
                if !o.is_empty() {
                    z.owners = o;
                }
                if !f.is_empty() {
                    z.forbidden = f;
                }
                z.enabled = true;
            } else {
                (z.owners, z.forbidden, z.enabled) = z.initial.clone();
            }
        }
    }
    for (targets, protect) in std::mem::take(&mut vm.protect_overrides) {
        let s = spawners(&targets);
        for (mut n, from) in &mut npcs {
            if s.contains(&from.0) {
                n.protect_neutrals = protect;
            }
        }
    }
    for (targets, flag, on) in std::mem::take(&mut vm.npc_flags) {
        let s = spawners(&targets);
        for (mut n, from) in &mut npcs {
            if s.contains(&from.0) {
                match flag {
                    0 => n.ignore_rb_damage = on,
                    _ => n.no_navmesh_teleport = on,
                }
            }
        }
    }
    for (targets, points, start) in std::mem::take(&mut vm.ambush_cmds) {
        let s = spawners(&targets);
        let point = points.iter().find_map(|v| if let Val::Actor(a) = v { g.actors.get(*a as usize).map(|a| a.name.clone()) } else { None }).and_then(|n| ai.markers.ambush.iter().position(|p| p.name == n));
        for (mut n, from) in &mut npcs {
            if s.contains(&from.0) {
                n.ambush_at = if start { point } else { None };
                if start {
                    if let Some(p) = point.and_then(|i| ai.markers.ambush.get(i)) {
                        n.home = Vec3::from(p.position);
                        n.home_yaw = p.yaw;
                        n.set_mode(Mode::Return);
                    }
                }
            }
        }
    }
}

/// Corvo where he mustn't be: the zone's owners that see him take him for an enemy there.
fn trespass(ai: Res<AiWorld>, player: Query<&Transform, With<Player>>, mut npcs: Query<&mut Npc>) {
    let Ok(pt) = player.single() else { return };
    let at = pt.translation;
    // (touching it: his cylinder, feet to head, in it)
    let reach = crate::player::STAND_HALF + crate::player::RADIUS;
    let touching = |h: &[Collider]| [-reach, -reach * 0.5, 0.0, reach * 0.5, reach].iter().any(|dy| inside(h, at + Vec3::Y * *dy));
    let zones: Vec<&Zone> = ai.zones.iter().filter(|z| z.enabled && z.forbidden.iter().any(|f| f.eq_ignore_ascii_case("Faction_Corvo_Default")) && touching(&z.hulls)).collect();
    for mut n in &mut npcs {
        if n.is_down() {
            continue;
        }
        let owner = zones.iter().any(|z| z.owners.iter().any(|o| o.eq_ignore_ascii_case(&n.faction)));
        match (owner, n.trespass) {
            (true, None) if !n.enemy => {
                n.trespass = Some(n.enemy);
                n.enemy = true;
                if std::env::var("DH_AI_LOG").is_ok() {
                    info!("trespass: {} ({}) takes Corvo for an enemy in its zone (Corvo at {at:.1})", n.name, n.faction);
                }
            }
            // (out of it: as they were, unless they're fighting him)
            (false, Some(was)) if n.alert != Alert::Combat => {
                n.enemy = was;
                n.trespass = None;
                if std::env::var("DH_AI_LOG").is_ok() {
                    info!("trespass: {} lets it go (Corvo out of its zone at {at:.1})", n.name);
                }
            }
            _ => {}
        }
    }
}

/// Corvo striking a neutral: the guards who see it (protecting the neutral) turn on him.
fn protect_neutrals(mut hits: MessageReader<NpcHit>, mut npcs: Query<(Entity, &mut Npc, &Transform)>, mut stances: ResMut<crate::script_world::SpawnerOverrides>, player: Query<&Transform, With<Player>>) {
    let Ok(pt) = player.single() else {
        hits.clear();
        return;
    };
    let struck: Vec<(Entity, Vec3)> = hits
        .read()
        .filter(|h| !matches!(h.kind, HitKind::ByOthers | HitKind::Rats))
        .filter_map(|h| npcs.get(h.npc).ok().filter(|(_, n, _)| !n.enemy).map(|(e, _, t)| (e, t.translation)))
        .collect();
    if struck.is_empty() {
        return;
    }
    for (e, mut n, t) in &mut npcs {
        if n.enemy || n.is_down() || !n.fighter() || struck.iter().any(|(v, _)| *v == e) {
            continue;
        }
        let protect = n.protect_neutrals.unwrap_or(n.protects_by_default);
        if !protect {
            continue;
        }
        let near = struck.iter().any(|(_, at)| at.distance(t.translation) < 15.0 && n.forward().dot((*at - t.translation).normalize_or_zero()) > -0.3);
        if near {
            n.enemy = true;
            n.alert = Alert::Combat;
            n.last_seen = Some(pt.translation);
            n.set_mode(Mode::Combat);
            stances.wronged(n.spawner);
        }
    }
}

/// Fighting or searching beyond its tether volumes: it goes home.
fn tethers(ai: Res<AiWorld>, mut npcs: Query<(&mut Npc, &Transform)>, time: Res<Time>, mut check: Local<f32>) {
    *check -= time.delta_secs();
    if *check > 0.0 {
        return;
    }
    *check = 0.5;
    for (mut n, t) in &mut npcs {
        if n.tether.is_empty() || n.is_down() || !matches!(n.mode, Mode::Combat | Mode::Search | Mode::Investigate) {
            continue;
        }
        if !ai.within_tether(&n.tether, t.translation) {
            n.set_mode(Mode::Return);
            n.target = Some(n.home);
        }
    }
}

/// Lying in wait at an ambush point: Corvo in its area, it springs.
fn ambushes(ai: Res<AiWorld>, player: Query<&Transform, With<Player>>, mut npcs: Query<(&mut Npc, &Transform)>) {
    let Ok(pt) = player.single() else { return };
    for (mut n, t) in &mut npcs {
        let Some(p) = n.ambush_at.and_then(|i| ai.markers.ambush.get(i)) else { continue };
        if n.is_down() {
            n.ambush_at = None;
            continue;
        }
        // the area: a box along its direction from the point
        let [dir, length, width, top, bottom] = p.area;
        let o = Vec3::from(p.position);
        let rel = pt.translation - o;
        let along_v = Quat::from_rotation_y(dir) * Vec3::NEG_Z;
        let along = rel.with_y(0.0).dot(along_v);
        let side = rel.with_y(0.0).dot(Vec3::new(-along_v.z, 0.0, along_v.x)).abs();
        if along >= 0.0 && along <= length && side <= width.max(1.0) * 0.5 + 1.0 && rel.y <= top && rel.y >= bottom {
            n.ambush_at = None;
            n.alert = Alert::Combat;
            n.awareness = 1.0;
            n.last_seen = Some(pt.translation);
            n.set_mode(Mode::Combat);
            info!("ambush: {} springs from {}", n.name, p.name);
        } else if t.translation.distance(o) < 1.0 && n.mode != Mode::Idle {
            n.set_mode(Mode::Idle);
            n.yaw = p.yaw;
        }
    }
}

/// The alarm's level (`DAISL_*`) a reinforcement waits for, as the characters' alert.
fn wanted_level(s: &str) -> u8 {
    let s = s.to_ascii_lowercase();
    if s.contains("suspect") || s.contains("curious") {
        1
    } else if s.contains("search") || s.contains("alert") || s.contains("investig") {
        2
    } else {
        3
    }
}

/// Reinforcements: the spawners waiting for the alarm, or their squad's call for help, spawn
/// (out of Corvo's sight and away from him unless allowed).
#[allow(clippy::too_many_arguments)]
fn reinforcements(
    level: Option<Res<LevelInfo>>,
    mut ai: ResMut<AiWorld>,
    npcs: Query<(&Npc, &FromSpawner)>,
    cam: Query<&GlobalTransform, With<PlayerCamera>>,
    rapier: ReadRapierContext,
    mut requests: MessageWriter<SpawnRequest>,
    time: Res<Time>,
    mut check: Local<f32>,
) {
    *check -= time.delta_secs();
    if *check > 0.0 {
        return;
    }
    *check = 0.5;
    let (Some(level), Ok(c)) = (level, cam.single()) else { return };
    let present: HashSet<u32> = npcs.iter().map(|(_, f)| f.0).collect();
    // how far the alarm has gone, and the squads now fighting (calling for help)
    let mut alarm = 0u8;
    for (n, _) in &npcs {
        if n.is_down() || !n.enemy {
            continue;
        }
        let l = match (n.alert, n.mode) {
            (Alert::Combat, _) => 3,
            (_, Mode::Search) => 2,
            (Alert::Suspicious, _) => 1,
            _ => 0,
        };
        alarm = alarm.max(l);
        if l == 3 && !n.squad.is_empty() {
            ai.called.insert(n.squad.to_ascii_lowercase());
        }
    }
    let eye = c.translation();
    let ctx = rapier.single().ok();
    let walls = QueryFilter::default().groups(CollisionGroups::new(Group::ALL, crate::level::GROUP_WORLD));
    for (i, sp) in level.scene.spawners.iter().enumerate() {
        let i = i as u32;
        if sp.spawn_on_begin_play || present.contains(&i) || ai.reinforced.contains(&i) {
            continue;
        }
        let by_alarm = sp.spawn_at.as_deref().is_some_and(|l| alarm > 0 && alarm >= wanted_level(l));
        let by_help = sp.spawn_on_help && !sp.squad.is_empty() && ai.called.contains(&sp.squad.to_ascii_lowercase());
        if !by_alarm && !by_help {
            continue;
        }
        let at = Vec3::from(sp.position) + Vec3::Y * 0.9;
        if !sp.spawn_near && at.distance(eye) < 12.0 {
            continue;
        }
        if !sp.spawn_visible {
            let to = at - eye;
            let in_view = c.forward().as_vec3().dot(to.normalize_or_zero()) > 0.5;
            let clear = in_view && ctx.as_ref().is_some_and(|x| x.cast_ray(eye, to.normalize_or_zero(), to.length() - 0.3, true, walls).is_none());
            if clear {
                continue;
            }
        }
        ai.reinforced.insert(i);
        info!("reinforcement: {} ({})", sp.name, if by_help { "help called" } else { "alarm" });
        requests.write(SpawnRequest(i));
    }
}
