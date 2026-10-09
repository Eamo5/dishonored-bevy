//! What the level scripts do to the characters and the world beyond moving and showing things:
//! patrols (`DisSeqAct_AISetPatrol`), senses (`DisSeqAct_AISetSenses`), who is hostile to
//! Corvo (`DisSeqAct_SetDisposition`), who can't die yet (`DisSeqAct_LimitPawnMinHealth`),
//! collision (`SeqAct_ChangeCollision`), the security devices (`DisSeqAct_WallofLightControl`,
//! `DisSeqAct_AlarmBell`, `DisSeqAct_PlugWhaleOilBattery`), what they hand Corvo
//! (`DisSeqAct_GivePickup`) and the mission's failure (`DisSeqAct_GameOver`).

use crate::gameplay::{HudMessages, PlayerStats};
use crate::kismet::{Val, Vm};
use crate::level::{InstanceCollider, LevelInfo, LevelInstance};
use crate::npc::{FromSpawner, Mode, Npc};
use crate::GameState;
use bevy::prelude::*;
use bevy_rapier3d::prelude::ColliderDisabled;
use dhcook::format::KVal;

pub struct ScriptWorldPlugin;

impl Plugin for ScriptWorldPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Cinematic>()
            .init_resource::<ScriptedShots>()
            .init_resource::<SpawnerOverrides>()
            .add_systems(OnEnter(GameState::InGame), |mut o: ResMut<SpawnerOverrides>| *o = SpawnerOverrides::default())
            .add_systems(OnEnter(GameState::InGame), |mut c: ResMut<Cinematic>| *c = Cinematic::default())
            .init_resource::<crate::kismet::ScriptSwitches>()
            .add_systems(Update, (apply_ai_fx.after(crate::kismet::apply_effects), apply_overrides, follow_and_sense, track_targets, cinematic_hud, scripted_shots, script_time, vanish_unseen, player_track, behaviors, beg, script_blasts, npc_materials).chain().run_if(in_state(GameState::InGame)))
            .add_systems(Update, (sync_switches, scripted_severs, spawn_props, foreground_npcs).after(crate::kismet::apply_effects).run_if(in_state(GameState::InGame)));
    }
}

/// The scripts' requests for the characters and the world.
#[derive(Clone, Debug)]
pub enum AiFx {
    /// the targets' bodies turn to ash when they die (`DisSeqAct_BodyShadowKill`)
    AshOnDeath { targets: Vec<Val> },
    /// the targets' brain flags (`DisSeqAct_AISetBrainFlags`): set (0), cleared (1) or toggled
    BrainFlags { targets: Vec<Val>, flags: Vec<u8>, input: u8 },
    /// a simple behaviour (`DisSeqAct_AIDoSimpleBehaviors`): beg on the knees, or panic;
    /// started or aborted
    Simple { targets: Vec<Val>, panic: bool, start: bool },
    /// a pickup made and put in a character's pocket (`DisSeqAct_SpawnStealable`)
    SpawnStealable { pickup: u32, targets: Vec<Val> },
    /// patrol (from a nav point or route actor), or stop
    Patrol { targets: Vec<Val>, start: Option<u32>, stop: bool },
    Senses { targets: Vec<Val>, blind: bool, deaf: bool, numb: bool },
    /// the targets' stance toward the recipients (and theirs back, reciprocated): set (0),
    /// cleared (1), all of the targets' cleared (2)
    Disposition { targets: Vec<Val>, recipients: Vec<Val>, input: u8, hostile: bool, reciprocate: bool },
    MinHealth { targets: Vec<Val>, min: f32 },
    Collision { targets: Vec<Val>, block: bool },
    /// off (0), on (1), toggle (2), switch polarity (3)
    Wall { targets: Vec<Val>, cmd: usize },
    Bell { targets: Vec<Val>, on: bool },
    Plug { receptacles: Vec<Val>, plug: bool },
    /// the giving op (its cooked pickup)
    GivePickup(u32),
    /// a noise at a place (or its maker), heard so far, in alarm or not
    Noise { maker: Vec<Val>, at: Vec<Val>, radius: f32, combat: bool },
    /// go and look at something (`DisSeqAct_AIStartDistraction`)
    Distract { targets: Vec<Val>, to: Vec<Val> },
    /// suspicious (true) or calm
    Suspicion { targets: Vec<Val>, suspecting: bool },
    Search { targets: Vec<Val>, around: Vec<Val>, abort: bool },
    /// stand guard at a place (or go back to patrolling)
    Guard { targets: Vec<Val>, home: Vec<Val>, stop: bool },
    /// a Whaler's teleport to a place (`DisSeqAct_NPCDoTeleportSpell`)
    Teleport { targets: Vec<Val>, to: Vec<Val>, set_home: bool },
    ClearAttention { targets: Vec<Val> },
    Psychic { targets: Vec<Val>, on: bool },
    Follow { targets: Vec<Val>, leader: Vec<Val>, stop: bool },
    /// a level swarm goes to a place (`DisSeqAct_SetRatSwarmCustomBehavior`)
    RatsGo { swarms: Vec<Val>, to: Vec<Val> },
    /// characters shoot at someone (`DisSeqAct_AIShoot`): shots, accuracy
    Shoot { shooters: Vec<Val>, target: Vec<Val>, shots: u32, accuracy: f32 },
    /// a factory's pickup appears (`scene.pickups`, at its spawn point)
    SpawnPickup { pickup: u32, at: Vec3 },
    /// things carried by others (`SeqAct_AttachToActor`): emitters follow a character or a prop
    Attach { targets: Vec<Val>, attachments: Vec<Val>, detach: bool },
    /// cinematic mode on (`Some(on)`) or toggled, hiding the HUD, holding Corvo
    Cinematic { on: Option<bool>, hide_hud: bool, hold: bool, hide_player: bool },
    GameOver(String),
    /// a weapon given to or taken from characters (`DisSeqAct_AddInventoryItem` /
    /// `RemoveInventoryItem`): the item's tweak or class path
    Arm { targets: Vec<Val>, item: String, add: bool },
    /// go and ring the alarm (`DisSeqAct_AIRingAlarm`): they fight, and a bell near a fight rings
    RingAlarm { targets: Vec<Val> },
    /// characters follow someone with their eyes (`DisSeqAct_NPCTrackTarget`), or stop
    Track { npcs: Vec<Val>, target: Vec<Val>, on: bool },
}

/// The scripts' cinematic mode (`SeqAct_ToggleCinematicMode`): the HUD hidden, Corvo held.
/// (Held a minute at most, should a script never let go.)
#[derive(Resource, Default)]
pub struct Cinematic {
    pub on: bool,
    pub hide_hud: bool,
    pub hold: bool,
    /// Corvo's arms and weapons hidden (`bHidePlayer`)
    pub hide_player: bool,
    pub since: f32,
}

impl Cinematic {
    fn live(&self) -> bool {
        self.on && self.since < 60.0
    }
    pub fn holds_movement(&self) -> bool {
        self.live() && self.hold
    }
    pub fn hides_hud(&self) -> bool {
        self.live() && self.hide_hud
    }
    pub fn hides_player(&self) -> bool {
        self.live() && self.hide_player
    }
}

fn cinematic_hud(time: Res<Time>, mut cine: ResMut<Cinematic>) {
    if cine.on {
        cine.since += time.delta_secs();
    }
}

/// A side in the relationships the scripts set: Corvo (and his faction,
/// `Faction_Corvo_Default`), a faction, a spawner's character.
#[derive(Clone, PartialEq, Eq, Hash, Debug, serde::Serialize, serde::Deserialize)]
pub enum Party {
    Corvo,
    Faction(String),
    Npc(u32),
}

impl Party {
    /// A faction by its tweak's name (`DisFaction_Defaults.Faction_Guard_Default`).
    pub fn faction(name: &str) -> Party {
        let n = name.rsplit('.').next().unwrap_or(name);
        if n.eq_ignore_ascii_case("Faction_Corvo_Default") {
            Party::Corvo
        } else {
            Party::Faction(n.to_string())
        }
    }
}

/// What the scripts set on a spawner's characters, kept for those it spawns later (the
/// scripts often set it as the level starts, before anyone is there): senses (blind, deaf,
/// numb), the health they're kept above; and who stands how with whom
/// (`DisSeqAct_SetDisposition`: one side hostile to the other or not, between Corvo,
/// factions and characters, over the factions' own enemies).
#[derive(Resource, Default)]
pub struct SpawnerOverrides {
    pub senses: std::collections::HashMap<u32, (bool, bool, bool)>,
    pub min_health: std::collections::HashMap<u32, f32>,
    pub stance: std::collections::HashMap<(Party, Party), bool>,
}

/// The scripts' settings, for a save.
#[derive(Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct OverridesSave {
    senses: Vec<(u32, (bool, bool, bool))>,
    min_health: Vec<(u32, f32)>,
    stance: Vec<(Party, Party, bool)>,
}

impl SpawnerOverrides {
    pub fn save(&self) -> OverridesSave {
        OverridesSave {
            senses: self.senses.iter().map(|(k, v)| (*k, *v)).collect(),
            min_health: self.min_health.iter().map(|(k, v)| (*k, *v)).collect(),
            stance: self.stance.iter().map(|((a, b), h)| (a.clone(), b.clone(), *h)).collect(),
        }
    }
    pub fn load(&mut self, s: OverridesSave) {
        self.senses = s.senses.into_iter().collect();
        self.min_health = s.min_health.into_iter().collect();
        self.stance = s.stance.into_iter().map(|(a, b, h)| ((a, b), h)).collect();
    }
    /// Is a character (its spawner, faction) hostile to another side? Its own stance comes
    /// first, then its faction's, then the faction's enemies (`default`).
    pub fn hostile(&self, spawner: u32, faction: &str, to: &[Party], default: bool) -> bool {
        for a in [Party::Npc(spawner), Party::faction(faction)] {
            for b in to {
                if let Some(h) = self.stance.get(&(a.clone(), b.clone())) {
                    return *h;
                }
            }
        }
        default
    }
    /// ... to Corvo.
    pub fn against_corvo(&self, spawner: u32, faction: &str, default: bool) -> bool {
        self.hostile(spawner, faction, &[Party::Corvo], default)
    }
    /// A character hurt by Corvo fights back (as the original's attacked pawns turn on him).
    pub fn wronged(&mut self, spawner: u32) {
        self.stance.insert((Party::Npc(spawner), Party::Corvo), true);
    }
}

/// Characters spawned after the scripts spoke of their spawner take what they set.
fn apply_overrides(o: Res<SpawnerOverrides>, mut npcs: Query<(&mut Npc, &FromSpawner), Added<Npc>>) {
    for (mut n, f) in &mut npcs {
        n.enemy = o.against_corvo(f.0, &n.faction, n.enemy_default);
        if let Some(&(b, d, nu)) = o.senses.get(&f.0) {
            n.blind = b;
            n.deaf = d;
            n.numb = nu;
        }
        if let Some(&m) = o.min_health.get(&f.0) {
            n.min_health = m;
        }
    }
}

/// Shots the scripts ordered: (shooter's spawner, target: Corvo or a spawner's character,
/// shots left, until the next, accuracy).
#[derive(Resource, Default)]
pub struct ScriptedShots(Vec<(u32, Option<u32>, u32, f32, f32)>);

#[allow(clippy::too_many_arguments)]
fn scripted_shots(
    time: Res<Time>,
    mut queue: ResMut<ScriptedShots>,
    mut npcs: Query<(Entity, &mut Npc, &FromSpawner, &Transform)>,
    player: Query<&Transform, (With<crate::player::Player>, Without<Npc>)>,
    (mut player_hits, mut npc_hits, mut shots, mut noise): (
        MessageWriter<crate::gameplay::PlayerHit>,
        MessageWriter<crate::gameplay::NpcHit>,
        MessageWriter<crate::npc::NpcShot>,
        MessageWriter<crate::gameplay::Noise>,
    ),
) {
    if queue.0.is_empty() {
        return;
    }
    let dt = time.delta_secs();
    let ppos = player.single().map(|t| t.translation).ok();
    let spots: Vec<(Entity, u32, Vec3, bool)> = npcs.iter().map(|(e, n, f, t)| (e, f.0, t.translation, n.is_down())).collect();
    for q in queue.0.iter_mut() {
        q.3 -= dt;
        if q.3 > 0.0 || q.2 == 0 {
            continue;
        }
        let Some((se, mut sn, _, st)) = npcs.iter_mut().find(|(_, n, f, _)| f.0 == q.0 && !n.is_down()) else {
            q.2 = 0;
            continue;
        };
        let target = match q.1 {
            None => ppos.map(|p| (None, p)),
            Some(s) => spots.iter().find(|x| x.1 == s && !x.3).map(|x| (Some(x.0), x.2)),
        };
        let Some((te, at)) = target else {
            q.2 = 0;
            continue;
        };
        let from = st.translation + Vec3::Y * 0.5;
        let flat = (at - st.translation).with_y(0.0);
        if flat.length() > 0.1 {
            sn.yaw = (-flat.x).atan2(-flat.z);
        }
        shots.write(crate::npc::NpcShot { from, to: at, bow: sn.ranged >= 2 });
        noise.write(crate::gameplay::Noise { pos: from, radius: 40.0, combat: true });
        if rand::random::<f32>() < q.4 {
            // (the pistol's 20; a character's bullet kills another outright:
            // `m_bInstaDeath_NonPlayerToNPC`)
            match te {
                None => {
                    player_hits.write(crate::gameplay::PlayerHit { npc: se, from, damage: 20.0, kick: false, big: false, push: false });
                }
                Some(e) => {
                    npc_hits.write(crate::gameplay::NpcHit { npc: e, damage: 999.0, kind: crate::gameplay::HitKind::Bullet, from });
                }
            }
        }
        q.2 -= 1;
        q.3 = 0.6;
    }
    queue.0.retain(|q| q.2 > 0);
}

/// Is a DLC (`eDisDLC_05`) installed with the game?
pub fn dlc_installed(kind: &str) -> bool {
    let Some(n) = kind.rsplit('_').next().filter(|n| n.chars().all(|c| c.is_ascii_digit())) else { return false };
    let root = std::env::var("DH_GAME_DIR").unwrap_or_else(|_| "S:/Games/SteamLibrary/steamapps/common/Dishonored".into());
    std::path::Path::new(&root).join("DishonoredGame/DLC/PCConsole").join(format!("DLC{n}")).is_dir()
}

#[allow(clippy::too_many_arguments)]
fn apply_ai_fx(
    vm: Option<ResMut<Vm>>,
    level: Option<Res<LevelInfo>>,
    data: Res<crate::gamedata::Data>,
    mut npcs: Query<(&mut Npc, &FromSpawner, &mut Transform), Without<crate::player::Player>>,
    player: Query<&Transform, With<crate::player::Player>>,
    colliders: Query<(&LevelInstance, &InstanceCollider)>,
    mut commands: Commands,
    mut devices: ResMut<crate::security::Devices>,
    (mut stats, mut msgs, mut sfx, attrs): (ResMut<PlayerStats>, ResMut<HudMessages>, MessageWriter<crate::audio::PostEvent>, Res<crate::gamedata::Attrs>),
    (mut noise, mut effects): (MessageWriter<crate::gameplay::Noise>, MessageWriter<crate::particles::SpawnEffect>),
    mut instances: Query<(Entity, &LevelInstance, &mut Transform, &mut Visibility), (Without<Npc>, Without<crate::player::Player>)>,
    mut spawned: Local<std::collections::HashSet<u32>>,
    (mut cine, mut shots, mut swarms, mut overrides): (ResMut<Cinematic>, ResMut<ScriptedShots>, Query<&mut crate::swarm::Swarm>, ResMut<SpawnerOverrides>),
    (mut emitters, npc_ents, inst_ents): (
        Query<&mut crate::particles::ParticleEmitter>,
        Query<(Entity, &FromSpawner, &GlobalTransform), With<Npc>>,
        Query<(Entity, &LevelInstance, &GlobalTransform), Without<Npc>>,
    ),
) {
    let Some(mut vm) = vm else { return };
    if vm.ai_fx.is_empty() {
        return;
    }
    let fxs = std::mem::take(&mut vm.ai_fx);
    let g = vm.g.clone();
    let actor = |v: &Val| if let Val::Actor(a) = v { g.actors.get(*a as usize) } else { None };
    let spawners = |vals: &[Val]| -> Vec<u32> { vals.iter().filter_map(|v| actor(v).and_then(|a| a.spawner)).collect() };
    let log = std::env::var("DH_SCRIPT_WORLD_LOG").is_ok();
    let ppos = player.single().map(|t| t.translation).ok();
    // where a script value is: Corvo, a spawner's character, an actor
    let live: Vec<(u32, Vec3)> = npcs.iter().map(|(_, f, t)| (f.0, t.translation)).collect();
    let place = |v: &Val| -> Option<Vec3> {
        match v {
            Val::Player => ppos,
            Val::Actor(a) => {
                let a = g.actors.get(*a as usize)?;
                a.spawner.and_then(|s| live.iter().find(|l| l.0 == s).map(|l| l.1)).or(Some(Vec3::from(a.position)))
            }
            _ => None,
        }
    };
    for fx in fxs {
        if log {
            info!("level scripts: {fx:?}");
        }
        match fx {
            AiFx::Patrol { targets, start, stop } => {
                let sp = spawners(&targets);
                let start = start.and_then(|a| g.actors.get(a as usize));
                for (mut n, from, t) in &mut npcs {
                    if !sp.contains(&from.0) || n.is_down() {
                        continue;
                    }
                    if stop {
                        if n.mode == Mode::Patrol {
                            n.home = t.translation;
                            n.set_mode(Mode::Idle);
                        }
                        continue;
                    }
                    // the route the start point lies on (a route actor by its name)
                    if let (Some(s), Some(level)) = (start, level.as_ref()) {
                        let at = Vec3::from(s.position);
                        let found = level.scene.routes.iter().find(|r| r.name == s.name).map(|r| (r, nearest(&r.points, at))).or_else(|| {
                            level
                                .scene
                                .routes
                                .iter()
                                .filter_map(|r| r.points.iter().position(|p| Vec3::from(*p).distance(at) < 0.5).map(|i| (r, i)))
                                .next()
                        });
                        if let Some((r, i)) = found {
                            n.route = r.points.iter().map(|p| Vec3::from(*p)).collect();
                            n.route_idx = i;
                            n.route_dir = 1;
                            n.route_pingpong = r.kind == "ERT_Linear";
                        } else {
                            // a lone point: go and stand there
                            n.target = Some(at);
                            n.home = at;
                            continue;
                        }
                    }
                    if n.route.len() >= 2 && matches!(n.mode, Mode::Idle | Mode::Return | Mode::Patrol) {
                        n.set_mode(Mode::Patrol);
                    }
                }
            }
            AiFx::Senses { targets, blind, deaf, numb } => {
                let sp = spawners(&targets);
                for s in &sp {
                    overrides.senses.insert(*s, (blind, deaf, numb));
                }
                for (mut n, from, _) in &mut npcs {
                    if sp.contains(&from.0) {
                        n.blind = blind;
                        n.deaf = deaf;
                        n.numb = numb;
                    }
                }
            }
            AiFx::Disposition { targets, recipients, input, hostile, reciprocate } => {
                let parties = |vals: &[Val]| -> Vec<Party> {
                    let mut out = Vec::new();
                    for v in vals {
                        match v {
                            Val::Player => out.push(Party::Corvo),
                            Val::Str(s) if !s.is_empty() => out.push(Party::faction(s)),
                            Val::Actor(_) => out.extend(spawners(std::slice::from_ref(v)).into_iter().map(Party::Npc)),
                            _ => {}
                        }
                    }
                    out
                };
                let (ts, rs) = (parties(&targets), parties(&recipients));
                match input {
                    0 | 1 => {
                        for t in &ts {
                            for r in &rs {
                                let mut pairs = vec![(t.clone(), r.clone())];
                                if reciprocate {
                                    pairs.push((r.clone(), t.clone()));
                                }
                                for k in pairs {
                                    if input == 0 {
                                        overrides.stance.insert(k, hostile);
                                    } else {
                                        overrides.stance.remove(&k);
                                    }
                                }
                            }
                        }
                    }
                    _ => overrides.stance.retain(|(a, b), _| !ts.contains(a) && !(reciprocate && ts.contains(b))),
                }
                // the characters concerned take their stance toward Corvo anew
                let touched = |s: u32, f: &str| ts.iter().chain(&rs).any(|p| *p == Party::Npc(s) || *p == Party::faction(f) || *p == Party::Corvo);
                for (mut n, from, _) in &mut npcs {
                    if touched(from.0, &n.faction) {
                        let before = n.enemy;
                        n.enemy = overrides.against_corvo(from.0, &n.faction, n.enemy_default);
                        if log && before != n.enemy {
                            info!("level scripts: {} ({}) {} Corvo", n.name, n.faction, if n.enemy { "turns on" } else { "stands with" });
                        }
                    }
                }
            }
            AiFx::MinHealth { targets, min } => {
                let sp = spawners(&targets);
                for s in &sp {
                    overrides.min_health.insert(*s, min);
                }
                for (mut n, from, _) in &mut npcs {
                    if sp.contains(&from.0) {
                        n.min_health = min;
                    }
                }
            }
            AiFx::Collision { targets, block } => {
                let insts: Vec<u32> = targets.iter().filter_map(|v| actor(v)).flat_map(|a| a.instances.clone()).collect();
                for (li, c) in &colliders {
                    if insts.contains(&li.index) {
                        if block {
                            commands.entity(c.0).try_remove::<ColliderDisabled>();
                        } else {
                            commands.entity(c.0).try_insert(ColliderDisabled);
                        }
                    }
                }
            }
            AiFx::Wall { targets, cmd } => {
                for a in targets.iter().filter_map(|v| actor(v)) {
                    if let Some(i) = devices.find(&a.name, Vec3::from(a.position)) {
                        devices.control_wall(i, cmd);
                    }
                }
            }
            AiFx::Bell { targets, on } => {
                for a in targets.iter().filter_map(|v| actor(v)) {
                    if let Some(i) = devices.find(&a.name, Vec3::from(a.position)) {
                        if let Some((s, at)) = devices.ring(i, on) {
                            sfx.write(crate::audio::PostEvent::named(&s, Some(at)));
                        }
                    }
                }
            }
            AiFx::Plug { receptacles, plug } => {
                for a in receptacles.iter().filter_map(|v| actor(v)) {
                    if let Some(i) = devices.find(&a.name, Vec3::from(a.position)) {
                        devices.plug(i, plug);
                    }
                }
            }
            AiFx::GivePickup(op) => give_pickup(&g.ops[op as usize].props, &data, &attrs, &mut stats, &mut msgs, &mut vm),
            AiFx::Noise { maker, at, radius, combat } => {
                if let Some(pos) = at.iter().chain(maker.iter()).find_map(|v| place(v)) {
                    noise.write(crate::gameplay::Noise { pos, radius, combat });
                }
            }
            AiFx::Distract { targets, to } => {
                let sp = spawners(&targets);
                let Some(at) = to.iter().find_map(|v| place(v)) else { continue };
                for (mut n, from, _) in &mut npcs {
                    if sp.contains(&from.0) && !n.is_down() && n.mode != Mode::Combat {
                        n.target = Some(at);
                        n.last_seen = Some(at);
                        n.set_mode(Mode::Investigate);
                    }
                }
            }
            AiFx::Suspicion { targets, suspecting } => {
                let sp = spawners(&targets);
                for (mut n, from, _) in &mut npcs {
                    if sp.contains(&from.0) && !n.is_down() && n.mode != Mode::Combat {
                        if suspecting {
                            n.alert = crate::npc::Alert::Suspicious;
                            n.awareness = n.awareness.max(0.5);
                        } else {
                            n.alert = crate::npc::Alert::Unaware;
                            n.awareness = n.awareness.min(0.2);
                        }
                    }
                }
            }
            AiFx::Search { targets, around, abort } => {
                let sp = spawners(&targets);
                let at = around.iter().find_map(|v| place(v));
                for (mut n, from, t) in &mut npcs {
                    if !sp.contains(&from.0) || n.is_down() {
                        continue;
                    }
                    if abort {
                        if n.mode == Mode::Search {
                            n.set_mode(Mode::Return);
                        }
                    } else {
                        n.last_seen = Some(at.or(n.last_seen).unwrap_or(t.translation));
                        n.alert = crate::npc::Alert::Suspicious;
                        n.set_mode(Mode::Search);
                    }
                }
            }
            AiFx::AshOnDeath { targets } => {
                let sp = spawners(&targets);
                for (mut n, from, _) in &mut npcs {
                    if sp.contains(&from.0) {
                        n.ash_on_death = true;
                    }
                }
            }
            AiFx::BrainFlags { targets, flags, input } => {
                let sp = spawners(&targets);
                let bits = flags.iter().filter(|&&f| f < 8).fold(0u8, |a, &f| a | 1 << f);
                for (mut n, from, _) in &mut npcs {
                    if !sp.contains(&from.0) {
                        continue;
                    }
                    n.brain_flags = match input {
                        0 => n.brain_flags | bits,
                        1 => n.brain_flags & !bits,
                        _ => n.brain_flags ^ bits,
                    };
                    if log {
                        info!("level scripts: spawner {} brain flags {:#04b}", from.0, n.brain_flags);
                    }
                }
            }
            AiFx::Simple { targets, panic, start } => {
                let sp = spawners(&targets);
                for (e, from, _) in &npc_ents {
                    if !sp.contains(&from.0) {
                        continue;
                    }
                    if log {
                        info!("level scripts: spawner {} {} ({})", from.0, if panic { "panics" } else { "begs" }, if start { "start" } else { "abort" });
                    }
                    if panic {
                        if let Ok((mut n, _, _)) = npcs.get_mut(e) {
                            if start && !n.is_down() {
                                n.set_mode(Mode::Flee);
                            }
                        }
                    } else if start {
                        commands.entity(e).try_insert(Begging::default());
                    } else {
                        commands.entity(e).queue(|mut ec: EntityWorldMut| {
                            if let Some(mut b) = ec.get_mut::<Begging>() {
                                b.stop = true;
                            }
                        });
                    }
                }
            }
            AiFx::Guard { targets, home, stop } => {
                let sp = spawners(&targets);
                let at = home.iter().find_map(|v| place(v));
                for (mut n, from, _) in &mut npcs {
                    if !sp.contains(&from.0) || n.is_down() {
                        continue;
                    }
                    if stop {
                        if n.route.len() >= 2 && n.mode == Mode::Idle {
                            n.set_mode(Mode::Patrol);
                        }
                    } else if let Some(h) = at {
                        n.home = h;
                        if matches!(n.mode, Mode::Patrol | Mode::Idle | Mode::Return) {
                            n.set_mode(Mode::Idle);
                        }
                    }
                }
            }
            AiFx::Teleport { targets, to, set_home } => {
                let sp = spawners(&targets);
                let Some(at) = to.iter().find_map(|v| place(v)) else { continue };
                for (mut n, from, mut t) in &mut npcs {
                    if !sp.contains(&from.0) || n.is_down() {
                        continue;
                    }
                    sfx.write(crate::audio::PostEvent::named("AI_Assassin_Pwr_Teleport_Start", Some(t.translation)));
                    effects.write(crate::particles::SpawnEffect::at("assassin_vanish", t.translation + Vec3::Y * 0.2));
                    let dest = at + Vec3::Y * crate::npc::NPC_CENTER;
                    t.translation = dest;
                    n.velocity = Vec3::ZERO;
                    n.target = None;
                    if set_home {
                        n.home = dest;
                    }
                    sfx.write(crate::audio::PostEvent::named("AI_Assassin_Pwr_Teleport_End", Some(dest)));
                    effects.write(crate::particles::SpawnEffect::at("assassin_appear", dest + Vec3::Y * 0.2));
                }
            }
            AiFx::ClearAttention { targets } => {
                let sp = spawners(&targets);
                for (mut n, from, _) in &mut npcs {
                    if sp.contains(&from.0) && !n.is_down() {
                        n.awareness = 0.0;
                        n.sees_player = false;
                        if n.mode != Mode::Combat {
                            n.alert = crate::npc::Alert::Unaware;
                        }
                    }
                }
            }
            AiFx::Psychic { targets, on } => {
                let sp = spawners(&targets);
                for (mut n, from, _) in &mut npcs {
                    if sp.contains(&from.0) {
                        n.psychic = on;
                    }
                }
            }
            AiFx::Follow { targets, leader, stop } => {
                let sp = spawners(&targets);
                let who = leader.iter().find_map(|v| match v {
                    Val::Player => Some(u32::MAX),
                    Val::Actor(a) => g.actors.get(*a as usize).and_then(|a| a.spawner),
                    _ => None,
                });
                for (mut n, from, _) in &mut npcs {
                    if sp.contains(&from.0) {
                        n.follow = if stop { None } else { who };
                    }
                }
            }
            AiFx::SpawnStealable { pickup, targets } => {
                let Some(level) = level.as_ref() else { continue };
                let Some(p) = level.scene.pickups.get(pickup as usize) else { continue };
                let sp = spawners(&targets);
                let Some((npc, at)) = npc_ents.iter().find(|(_, f, _)| sp.contains(&f.0)).map(|(e, _, g)| (e, g.translation())) else { continue };
                if !spawned.insert(pickup) {
                    continue;
                }
                let mut meshes = Vec::new();
                for (e, li, mut t, mut v) in &mut instances {
                    if Some(li.index) == p.instance {
                        t.translation = at;
                        *v = Visibility::Inherited;
                        meshes.push((e, *t));
                    }
                }
                let pk = crate::interact::make_pickup(level, &data, pickup as usize, meshes.iter().map(|m| m.0).collect());
                if log {
                    info!("level scripts: {} put in a pocket", pk.label);
                }
                commands.spawn((pk, Transform::from_translation(at), crate::pickpocket::Pocket::new(npc, meshes, at, 1), DespawnOnExit(GameState::InGame)));
            }
            AiFx::SpawnPickup { pickup, at } => {
                let Some(level) = level.as_ref() else { continue };
                let Some(p) = level.scene.pickups.get(pickup as usize) else { continue };
                if !spawned.insert(pickup) {
                    continue;
                }
                // its mesh, moved to the spawn point and shown
                let mut entities = Vec::new();
                for (e, li, mut t, mut v) in &mut instances {
                    if Some(li.index) == p.instance {
                        t.translation = at;
                        *v = Visibility::Inherited;
                        entities.push(e);
                    }
                }
                let pk = crate::interact::make_pickup(level, &data, pickup as usize, entities);
                if log {
                    info!("level scripts: a factory makes {} at {at:.2}", pk.label);
                }
                commands.spawn((pk, Transform::from_translation(at), DespawnOnExit(GameState::InGame)));
            }
            AiFx::RatsGo { swarms: who, to } => {
                let Some(at) = to.iter().find_map(|v| place(v)) else { continue };
                let ids: Vec<u32> = who.iter().filter_map(|v| actor(v).and_then(|a| a.rat_spawner)).collect();
                for mut s in &mut swarms {
                    if s.spawner.is_some_and(|i| ids.contains(&i)) {
                        if let Some(w) = s.wild.as_mut() {
                            w.home = at;
                            w.goal = at;
                            w.wait = 0.0;
                        }
                    }
                }
            }
            AiFx::Shoot { shooters, target, shots: n, accuracy } => {
                let at = target.iter().find_map(|v| match v {
                    Val::Player => Some(None),
                    Val::Actor(a) => g.actors.get(*a as usize).and_then(|a| a.spawner).map(Some),
                    _ => None,
                });
                let Some(at) = at else { continue };
                for s in spawners(&shooters) {
                    shots.0.push((s, at, n.max(1), 0.2, accuracy));
                }
            }
            AiFx::Attach { targets, attachments, detach } => {
                // the carrier: a character, else a placed mesh (a physics prop, a mover)
                let carrier = targets.iter().find_map(|v| {
                    let Val::Actor(a) = v else { return None };
                    let a = g.actors.get(*a as usize)?;
                    if let Some(s) = a.spawner {
                        return npc_ents.iter().find(|(_, f, _)| f.0 == s).map(|(e, _, gt)| (e, gt.translation()));
                    }
                    inst_ents.iter().find(|(_, li, _)| a.instances.contains(&li.index)).map(|(e, _, gt)| (e, gt.translation()))
                });
                let parts: Vec<u32> = attachments.iter().filter_map(|v| if let Val::Actor(b) = v { g.actors.get(*b as usize) } else { None }).flat_map(|b| b.particles.clone()).collect();
                if parts.is_empty() {
                    continue;
                }
                for mut em in &mut emitters {
                    if !parts.contains(&em.index) {
                        continue;
                    }
                    match (detach, carrier) {
                        (false, Some((e, at))) => {
                            let here = em.xf().w_axis.truncate();
                            em.attach(Some((e, here - at)));
                        }
                        _ => em.attach(None),
                    }
                }
            }
            AiFx::Cinematic { on, hide_hud, hold, hide_player } => {
                let on = on.unwrap_or(!cine.on);
                *cine = Cinematic { on, hide_hud, hold, hide_player, since: 0.0 };
            }
            AiFx::GameOver(reason) => {
                if !stats.dead {
                    stats.dead = true;
                    stats.game_over = Some(reason);
                }
            }
            AiFx::Arm { targets, item, add } => {
                // the kind of weapon by its tweak or class (`Twk_Inv_SwordThug`, `DishonoredWepSword`,
                // `DisWepNPCAssassinHand`, pistols, crossbows)
                let it = item.to_ascii_lowercase();
                // Corvo's own: his sword (or everything) taken or given back, a pistol given
                if targets.iter().any(|v| matches!(v, Val::Player)) {
                    if it.contains("sword") || (!add && it.ends_with("dishonoredinventoryitem")) {
                        stats.unarmed = !add;
                    } else if add && it.contains("pistol") {
                        stats.weapons = true;
                    } else if add && it.contains("crossbow") {
                        stats.weapons = true;
                        stats.no_crossbow = false;
                    }
                }
                let sp = spawners(&targets);
                for (mut n, from, _) in &mut npcs {
                    if !sp.contains(&from.0) {
                        continue;
                    }
                    if it.contains("sword") {
                        n.has_sword = add;
                    } else if it.contains("assassinhand") {
                        n.ranged = if add { 3 } else if n.ranged == 3 { 0 } else { n.ranged };
                    } else if it.contains("pistol") {
                        n.ranged = if add { 1 } else if n.ranged == 1 { 0 } else { n.ranged };
                    } else if it.contains("crossbow") || it.contains("bow") {
                        n.ranged = if add { 2 } else if n.ranged == 2 { 0 } else { n.ranged };
                    }
                }
            }
            AiFx::Track { npcs: who, target, on } => {
                let at = target.iter().find_map(|v| match v {
                    Val::Player => Some(None),
                    Val::Actor(a) => g.actors.get(*a as usize).and_then(|a| a.spawner).map(Some),
                    _ => None,
                });
                let sp = spawners(&who);
                for (e, f, _) in npc_ents.iter() {
                    if !sp.contains(&f.0) {
                        continue;
                    }
                    match (on, at) {
                        (true, Some(at)) => {
                            commands.entity(e).try_insert(Tracking(at));
                        }
                        _ => {
                            commands.entity(e).try_remove::<(Tracking, crate::npc::SceneLook)>();
                        }
                    }
                }
            }
            AiFx::RingAlarm { targets } => {
                let sp = spawners(&targets);
                for (mut n, from, _) in &mut npcs {
                    if sp.contains(&from.0) && !n.is_down() {
                        n.enemy = true;
                        n.alert = crate::npc::Alert::Combat;
                        n.awareness = 1.0;
                        n.last_seen = ppos;
                        n.set_mode(Mode::Combat);
                    }
                }
            }
        }
    }
}

/// The scripts' hand on time and Corvo's powers: bent time (a minute at most, should a
/// script never end it), active powers cancelled, and his abstract items counted for them.
fn script_time(
    vm: Option<ResMut<Vm>>,
    time: Res<Time<Real>>,
    mut tc: ResMut<crate::gameplay::TimeControl>,
    mut powers: ResMut<crate::powers::Powers>,
    mut stats: ResMut<PlayerStats>,
    data: Res<crate::gamedata::Data>,
    settings: Res<crate::settings::Settings>,
) {
    if let Some((_, t)) = tc.scripted.as_mut() {
        *t += time.delta_secs();
        if *t > 60.0 {
            tc.scripted = None;
        }
    }
    let Some(mut vm) = vm else { return };
    for b in std::mem::take(&mut vm.bend_time) {
        tc.scripted = b.map(|d| (d.max(0.0), 0.0));
    }
    // health and mana set, elixirs and upgrades given, keys taken
    for (h, m) in std::mem::take(&mut vm.vitals) {
        if let Some(h) = h {
            stats.health = (stats.max_health * h / 100.0).clamp(1.0, stats.max_health);
        }
        if let Some(m) = m {
            stats.mana = (stats.max_mana * m / 100.0).clamp(0.0, stats.max_mana);
        }
    }
    for (mana, n) in std::mem::take(&mut vm.elixirs) {
        if n >= 0 {
            let capacity = data.pawn(if mana { "m_nMaxManaElixir" } else { "m_nMaxHealthElixir" }, 10.0).max(0.0) as u32;
            stats.give_elixirs(mana, n as u32, capacity);
        } else {
            let e = if mana { &mut stats.mana_elixirs } else { &mut stats.health_elixirs };
            *e = e.saturating_sub(n.unsigned_abs());
        }
    }
    for u in std::mem::take(&mut vm.upgrades) {
        if !stats.upgrades.contains(&u) {
            stats.upgrades.push(u);
        }
    }
    for k in std::mem::take(&mut vm.keys_taken) {
        stats.keys.retain(|x| !x.eq_ignore_ascii_case(&k));
    }
    for (put_away, upgrades) in std::mem::take(&mut vm.inventory_ops) {
        stats.stash(put_away, upgrades, &data, settings.difficulty);
    }
    let capacities = data.ammo_capacities(&stats, settings.difficulty);
    for (how, list) in std::mem::take(&mut vm.ammo_mods) {
        for (ty, n) in list {
            let Some(&capacity) = capacities.get(ty as usize) else { continue };
            // (a gadget not had stays not had)
            let Some(v) = crate::gadgets::ammo_mut(&mut stats, ty, how != 1 || n > 0) else { continue };
            *v = match how {
                1 => (n.max(0) as u32).min(capacity),
                2 => v.saturating_sub(n.max(0) as u32),
                _ => v.saturating_add((n.max(0) as u32).min(capacity.saturating_sub(*v))),
            };
        }
    }
    // items put in Corvo's hands: empty hands (the sword away), the sword, or a left-hand one
    for (targets, item) in std::mem::take(&mut vm.equip) {
        if !targets.iter().any(|v| matches!(v, Val::Player)) {
            continue;
        }
        let it = item.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
        let left = match it.as_str() {
            "dishonoreditemempty" => {
                stats.sheathed = true;
                None
            }
            "dishonoredwepsword" => {
                // (put in his hand, he has it: a challenge's loadout after its inventory was
                // emptied)
                stats.sheathed = false;
                stats.unarmed = false;
                None
            }
            "dishonoredweppistol" => Some(crate::powers::Power::Pistol),
            "diswepcrossbow" => Some(crate::powers::Power::Crossbow),
            "disgadget_heart" => Some(crate::powers::Power::Heart),
            _ => None,
        };
        if let Some(l) = left {
            powers.selected = l;
        }
    }
    if std::mem::take(&mut vm.cancel_powers) {
        tc.bend_remaining = 0.0;
        powers.dark_vision = false;
    }
    // the statistics the scripts read (`DisSeqAct_GetPlayerStat`), and those they reset
    for name in std::mem::take(&mut vm.stat_resets) {
        stats.counters.insert(name, 0);
    }
    if stats.is_changed() || vm.stat_values.is_empty() {
        for name in crate::kismet::PLAYER_STATS {
            let v = stats.stat(name);
            vm.stat_values.insert(name.to_string(), v);
        }
    }
    if stats.is_changed() || vm.item_counts.is_empty() {
        let mut counts: std::collections::HashMap<String, u32> = std::collections::HashMap::new();
        for (k, v) in &stats.items {
            *counts.entry(k.clone()).or_default() += v;
        }
        // (notes the scripts gave, uncounted: one each)
        for n in &stats.notes {
            counts.entry(n.clone()).or_insert(1);
        }
        vm.item_counts = counts;
    }
}

/// The scripts' switches where the world reads them: Blood Thirst's adrenaline held empty while
/// it's off, the game's own tutorials held back.
fn sync_switches(vm: Option<Res<Vm>>, mut sw: ResMut<crate::kismet::ScriptSwitches>, mut stats: ResMut<PlayerStats>, mut tw: ResMut<crate::tutwindow::TutorialWindow>) {
    let Some(vm) = vm else { return };
    if sw.0 != vm.switches {
        sw.0 = vm.switches.clone();
    }
    if sw.0.adrenaline_off && stats.adrenaline > 0.0 {
        stats.adrenaline = 0.0;
    }
    if tw.held != sw.0.systemic_off {
        tw.held = sw.0.systemic_off;
    }
}

/// Characters the scripts put in the foreground group (`DisSeqAct_OutsiderConfig`): their
/// parts' materials drawn with their depths squeezed to the front, at the world's view.
fn foreground_npcs(
    vm: Option<Res<Vm>>,
    parts: Query<(&MeshMaterial3d<crate::ue3mat::Ue3Material>, &ChildOf), With<crate::npc::NpcPart>>,
    parents: Query<&ChildOf>,
    npcs: Query<&FromSpawner, With<Npc>>,
    mut mats: ResMut<Assets<crate::ue3mat::Ue3Material>>,
    mut done: Local<std::collections::HashSet<AssetId<crate::ue3mat::Ue3Material>>>,
) {
    let Some(vm) = vm else { return };
    if vm.foreground.is_empty() {
        return;
    }
    for (m, child) in &parts {
        let Ok(root) = parents.get(child.parent()) else { continue };
        let Ok(from) = npcs.get(root.parent()) else { continue };
        if !vm.foreground.contains(&from.0) || done.contains(&m.0.id()) {
            continue;
        }
        if let Some(mut mat) = mats.get_mut(&m.0) {
            mat.key.foreground_world = true;
        }
        done.insert(m.0.id());
    }
}

/// Limbs the scripts cut (`DisSeqAct_SeverLimb`: the Overseers' back alley).
fn scripted_severs(vm: Option<ResMut<Vm>>, npcs: Query<(Entity, &FromSpawner), With<Npc>>, mut severs: MessageWriter<crate::gore::SeverLimb>) {
    let Some(mut vm) = vm else { return };
    // (the pre-order packs' charms the Hound Pits' factories make, `DisSeqAct_SetBoneCharmEffect`
    // behind `DisSeqCond_IsDLCUnlocked` eDisDLC_01..04: not in this install, so never made)
    for (at, id) in std::mem::take(&mut vm.charm_effects) {
        info!("level scripts: bone charm effect {id} for the charm at {at:.1}");
    }
    if vm.severs.is_empty() {
        return;
    }
    let g = vm.g.clone();
    for (targets, joint, _gore) in std::mem::take(&mut vm.severs) {
        let spawners: Vec<u32> = targets.iter().filter_map(|v| if let Val::Actor(a) = v { g.actors.get(*a as usize)?.spawner } else { None }).collect();
        for (e, from) in &npcs {
            if spawners.contains(&from.0) {
                severs.write(crate::gore::SeverLimb { npc: e, bone: joint.clone(), impulse: 80.0 });
            }
        }
    }
}

/// What spawners make, as the level and the scripts set them (`m_bAwareOfPlayerUponStartup`,
/// `SeqAct_ModifyProperty`): aware of Corvo from the start, already hunting him.
fn spawn_props(
    vm: Option<Res<Vm>>,
    level: Option<Res<crate::level::LevelInfo>>,
    mut npcs: Query<(&mut Npc, &FromSpawner), Added<Npc>>,
    player: Query<&Transform, With<crate::player::Player>>,
) {
    let pos = player.single().map(|t| t.translation).ok();
    for (mut npc, from) in &mut npcs {
        let mut aware = level.as_ref().and_then(|l| l.scene.spawners.get(from.0 as usize)).is_some_and(|s| s.aware);
        if let Some(props) = vm.as_ref().and_then(|v| v.spawn_props.get(&from.0)) {
            for (k, v) in props {
                if k.eq_ignore_ascii_case("m_bAwareOfPlayerUponStartup") {
                    aware = v.eq_ignore_ascii_case("true");
                }
            }
        }
        if aware && npc.alert == crate::npc::Alert::Unaware && !npc.is_down() {
            npc.alert = crate::npc::Alert::Combat;
            npc.awareness = 1.0;
            npc.last_seen = pos;
        }
    }
}

/// Characters dressed by the scripts (`DisSeqAct_NPCSetMaterials`: the Boyle sisters' red,
/// black and white): the parts whose material fills a named slot take the new one, or their
/// own back.
fn npc_materials(
    mut commands: Commands,
    vm: Option<ResMut<Vm>>,
    assets: Option<Res<crate::level::GameAssets>>,
    parts: Query<(Entity, &crate::npc::NpcPart, &ChildOf)>,
    parents: Query<&ChildOf>,
    npcs: Query<&FromSpawner, With<Npc>>,
) {
    let (Some(mut vm), Some(assets)) = (vm, assets) else { return };
    if vm.npc_materials.is_empty() {
        return;
    }
    let g = vm.g.clone();
    for (targets, op, set) in std::mem::take(&mut vm.npc_materials) {
        let spawners: Vec<u32> = targets.iter().filter_map(|v| if let Val::Actor(a) = v { g.actors.get(*a as usize)?.spawner } else { None }).collect();
        let Some(o) = g.ops.get(op as usize) else { continue };
        let list = |key: &str| -> Vec<(usize, u32)> {
            match o.props.get(key) {
                Some(KVal::List(l)) => l
                    .iter()
                    .filter_map(|e| match e {
                        KVal::List(p) => match (p.first(), p.get(1)) {
                            (Some(KVal::Int(s)), Some(KVal::Int(m))) => Some(((*s).max(0) as usize, *m as u32)),
                            _ => None,
                        },
                        _ => None,
                    })
                    .collect(),
                _ => Vec::new(),
            }
        };
        let (body, head) = (list("body_mats"), list("head_mats"));
        let mut n = 0;
        for (pe, part, child) in parts.iter().filter(|(_, p, _)| p.lod == 0) {
            // the part's character: its visual's parent
            let Ok(root) = parents.get(child.parent()) else { continue };
            let Ok(from) = npcs.get(root.parent()) else { continue };
            if !spawners.contains(&from.0) {
                continue;
            }
            let Some(Some(vis)) = assets.npc_types.get(part.npc_type as usize) else { continue };
            let Some(&(is_head, orig)) = vis.parts.mats.get(part.index as usize) else { continue };
            let (slots, news) = if is_head { (&vis.head_slots, &head) } else { (&vis.body_slots, &body) };
            let Some(&(_, new_id)) = news.iter().find(|(slot, _)| slots.get(*slot) == Some(&orig)) else { continue };
            let m = if set { assets.npc_mat_swaps.get(&new_id).cloned() } else { vis.parts.parts.get(part.index as usize).map(|p| p.1.clone()) };
            if let Some(m) = m {
                let mut ec = commands.entity(pe);
                ec.try_remove::<(MeshMaterial3d<crate::ue3mat::Ue3Material>, MeshMaterial3d<crate::lightmap::WorldMaterial>)>();
                m.try_apply(&mut ec);
                n += 1;
            }
        }
        if std::env::var("DH_SCRIPT_WORLD_LOG").is_ok() {
            info!("level scripts: dressed {n} parts of spawners {spawners:?} ({})", if set { "set" } else { "cleared" });
        }
    }
}

/// The scripts' explosions (`DisSeqAct_TriggerExplosion`): the cooked blast where they say.
fn script_blasts(
    vm: Option<ResMut<Vm>>,
    mut blasts: MessageWriter<crate::gadgets::Explosion>,
    (mut fx, mut sfx, mut shots): (MessageWriter<crate::particles::SpawnEffect>, MessageWriter<crate::audio::PostEvent>, MessageWriter<crate::npc::NpcShot>),
) {
    let Some(mut vm) = vm else { return };
    for (a, b, flare) in std::mem::take(&mut vm.projectiles) {
        shots.write(crate::npc::NpcShot { from: a, to: b, bow: true });
        if flare {
            fx.write(crate::particles::SpawnEffect { secs: 6.0, ..crate::particles::SpawnEffect::at("flare_trail", b) });
        }
    }
    if vm.explosions.is_empty() {
        return;
    }
    let g = vm.g.clone();
    for (at, op) in std::mem::take(&mut vm.explosions) {
        let Some(o) = g.ops.get(op as usize) else { continue };
        let f = |k: &str| if let Some(KVal::Float(v)) = o.props.get(k) { *v } else { 0.0 };
        let radius = f("blast_radius").max(1.0);
        blasts.write(crate::gadgets::Explosion { at, radius, full: f("blast_full"), damage: f("blast_damage"), effect: "", player: None, kind: crate::gameplay::HitKind::Explosion });
        if let Some(KVal::Int(e)) = o.props.get("blast_effect") {
            fx.write(crate::particles::SpawnEffect { system: Some(*e as u32), ..crate::particles::SpawnEffect::at("", at) });
        }
        if let Some(KVal::Str(s)) = o.props.get("blast_sound").filter(|s| !matches!(s, KVal::Str(x) if x.is_empty())) {
            sfx.write(crate::audio::PostEvent::named(s, Some(at)));
        }
    }
}

/// Characters' behaviours as the original names them, for `DisSeqEvent_BehaviorStarted`: a
/// civilian's panic, a guard's patrol, search and combat.
fn behaviors(vm: Option<ResMut<Vm>>, npcs: Query<(Entity, &Npc), Changed<Npc>>, mut last: Local<std::collections::HashMap<Entity, Mode>>) {
    let Some(mut vm) = vm else { return };
    for (e, n) in &npcs {
        let prev = last.insert(e, n.mode);
        if prev.is_none_or(|p| p == n.mode) {
            continue;
        }
        let b = match n.mode {
            Mode::Flee => "DisBehaviorPanic",
            Mode::Patrol => "DisBehaviorPatrol",
            Mode::Investigate => "DisBehaviorReact",
            Mode::Search => "DisBehaviorSearch",
            Mode::Combat => "DisBehaviorCombat",
            _ => continue,
        };
        vm.behavior_started(n.spawner, b);
    }
}

/// The scripts hold Corvo's view on something (`DisSeqAct_PlayerTrackTarget`): it turns
/// there over the blend time and stays.
fn player_track(vm: Option<Res<Vm>>, time: Res<Time<Real>>, mut player: Query<(&Transform, &mut crate::player::Player)>, npcs: Query<(&FromSpawner, &Transform), With<Npc>>) {
    let Some(vm) = vm else { return };
    let Some((target, blend)) = &vm.player_track else { return };
    let Ok((t, mut p)) = player.single_mut() else { return };
    let at = target.iter().find_map(|v| match v {
        Val::Actor(a) => vm.g.actors.get(*a as usize).map(|ka| match ka.spawner.and_then(|s| npcs.iter().find(|(f, _)| f.0 == s)) {
            Some((_, nt)) => nt.translation + Vec3::Y * 0.7,
            None => Vec3::from(ka.position),
        }),
        _ => None,
    });
    let Some(at) = at else { return };
    let eye = t.translation + Vec3::Y * p.eye_height;
    let to = at - eye;
    if to.length() < 0.2 {
        return;
    }
    let yaw = (-to.x).atan2(-to.z);
    let pitch = (to.y / to.with_y(0.0).length().max(1e-3)).atan();
    let k = (time.delta_secs() / blend.max(0.05) * 3.0).min(1.0);
    let dy = (yaw - p.yaw + std::f32::consts::PI).rem_euclid(std::f32::consts::TAU) - std::f32::consts::PI;
    p.yaw += dy * k;
    p.pitch += (pitch - p.pitch) * k;
}

/// Characters marked to vanish (`DisSeqAct_NPCMarkForVanish`) go once Corvo hasn't seen them
/// for a while (`m_fOffscreenTimeRequired`, 8 s); the scripts hear "Vanished".
#[derive(Component)]
pub struct Vanishing {
    op: u32,
    unseen: f32,
}

fn vanish_unseen(
    mut commands: Commands,
    vm: Option<ResMut<Vm>>,
    time: Res<Time>,
    mut npcs: Query<(Entity, &Npc, &FromSpawner, &Transform, Option<&mut Vanishing>)>,
    cam: Query<&GlobalTransform, With<crate::player::PlayerCamera>>,
) {
    let Some(mut vm) = vm else { return };
    let g = vm.g.clone();
    for (targets, op, mark) in std::mem::take(&mut vm.vanish) {
        let sp: Vec<u32> = targets.iter().filter_map(|v| if let Val::Actor(a) = v { g.actors.get(*a as usize).and_then(|a| a.spawner) } else { None }).collect();
        for (e, _, f, _, _) in &npcs {
            if sp.contains(&f.0) {
                if mark {
                    commands.entity(e).try_insert(Vanishing { op, unseen: 0.0 });
                } else {
                    commands.entity(e).try_remove::<Vanishing>();
                }
            }
        }
    }
    let Ok(c) = cam.single() else { return };
    let (eye, fwd) = (c.translation(), c.forward().as_vec3());
    let dt = time.delta_secs();
    for (e, n, _, t, v) in &mut npcs {
        let Some(mut v) = v else { continue };
        let need = vm.g.ops.get(v.op as usize).and_then(|o| o.props.get("m_fOffscreenTimeRequired")).and_then(|p| if let KVal::Float(f) = p { Some(*f) } else { None }).unwrap_or(8.0);
        let to = t.translation + Vec3::Y * 0.8 - eye;
        let seen = to.length() < 40.0 && to.normalize_or_zero().dot(fwd) > 0.5;
        // (not while fighting or searching)
        let busy = matches!(n.mode, Mode::Combat | Mode::Search);
        v.unseen = if seen || busy { 0.0 } else { v.unseen + dt };
        if v.unseen >= need {
            let op = v.op;
            commands.entity(e).try_despawn();
            vm.signal(op, 1);
        }
    }
}

/// A character the scripts have following someone with its eyes: Corvo (`None`) or a
/// spawner's character.
#[derive(Component)]
pub struct Tracking(pub Option<u32>);

fn track_targets(
    mut commands: Commands,
    trackers: Query<(Entity, &Tracking, &Npc)>,
    others: Query<(&FromSpawner, &Transform, &Npc)>,
    player: Query<&Transform, With<crate::player::Player>>,
) {
    let ppos = player.single().map(|t| t.translation).ok();
    for (e, tr, n) in &trackers {
        if n.is_down() {
            commands.entity(e).try_remove::<(Tracking, crate::npc::SceneLook)>();
            continue;
        }
        let at = match tr.0 {
            None => ppos.map(|p| p + Vec3::Y * 0.6),
            Some(s) => others.iter().find(|(f, _, o)| f.0 == s && !o.is_down()).map(|(_, t, _)| t.translation + Vec3::Y * 0.6),
        };
        if let Some(at) = at {
            commands.entity(e).try_insert(crate::npc::SceneLook { target: at, weight: 1.0 });
        }
    }
}

/// Those the scripts have following someone keep up with them; the psychic always know where
/// Corvo is.
fn follow_and_sense(mut npcs: Query<(&mut Npc, &Transform, &FromSpawner)>, player: Query<&Transform, (With<crate::player::Player>, Without<Npc>)>) {
    let ppos = player.single().map(|t| t.translation).ok();
    let leaders: Vec<(u32, Vec3)> = npcs.iter().filter(|(n, _, _)| !n.is_down()).map(|(_, t, f)| (f.0, t.translation)).collect();
    for (mut n, t, _) in &mut npcs {
        if n.is_down() {
            continue;
        }
        if let Some(who) = n.follow {
            let lead = if who == u32::MAX { ppos } else { leaders.iter().find(|l| l.0 == who).map(|l| l.1) };
            if let Some(l) = lead {
                if n.mode != Mode::Combat {
                    n.target = if l.distance(t.translation) > 2.2 { Some(l) } else { None };
                }
            }
        }
        if n.psychic {
            if let Some(p) = ppos {
                n.last_seen = Some(p);
                if n.mode != Mode::Combat && n.hostile() {
                    n.alert = crate::npc::Alert::Combat;
                    n.set_mode(Mode::Combat);
                }
            }
        }
    }
}

fn nearest(points: &[[f32; 3]], at: Vec3) -> usize {
    points.iter().enumerate().min_by(|a, b| Vec3::from(*a.1).distance(at).total_cmp(&Vec3::from(*b.1).distance(at))).map(|x| x.0).unwrap_or(0)
}

/// What a script hands Corvo: its pickup's contents (cooked onto the op).
fn give_pickup(props: &std::collections::BTreeMap<String, KVal>, data: &crate::gamedata::Data, attrs: &crate::gamedata::Attrs, stats: &mut PlayerStats, msgs: &mut HudMessages, vm: &mut Vm) {
    let s = |k: &str| match props.get(k) {
        Some(KVal::Str(v)) => v.clone(),
        _ => String::new(),
    };
    let int = |k: &str| match props.get(k) {
        Some(KVal::Int(v)) => *v,
        _ => 0,
    };
    let path = s("m_pPickup");
    let name = path.rsplit('.').next().unwrap_or("").to_string();
    let lname = name.to_ascii_lowercase();
    // a tutorial: its note to the journal (the scripts' own tutorial message points to it)
    if let Some(topic) = name.strip_suffix("_PickUp").filter(|t| t.ends_with("Tutorial")) {
        let note = format!("abstract_items.{topic}_Note");
        if data.0.abstract_items.contains_key(&note) && !stats.notes.contains(&note) {
            stats.notes.push(note);
        }
        return;
    }
    let mut gave = false;
    if let Some(KVal::List(ammo)) = props.get("pickup_ammo") {
        for a in ammo {
            if let KVal::List(p) = a {
                if let (Some(KVal::Int(ty)), Some(KVal::Int(n))) = (p.first(), p.get(1)) {
                    let (label, added) = crate::gadgets::give_ammo(stats, attrs, *ty as u8, (*n).max(0) as u32);
                    if added > 0 { msgs.push(format!("{label} +{added}")); }
                    gave = true;
                }
            }
        }
    }
    if gave {
        return;
    }
    let coins = matches!(props.get("pickup_coins"), Some(KVal::Bool(true))) || lname.contains("coin") || lname.contains("ingot");
    if coins {
        // its quantity, else the value in its name (`Ingot_100_twk`)
        let n = Some(int("pickup_quantity")).filter(|q| *q > 0).map(|q| q as u32).or_else(|| name.split('_').filter_map(|p| p.parse::<u32>().ok()).last()).unwrap_or(10);
        stats.coins += n;
        stats.coins_found += n;
        msgs.push(format!("+{n} coins"));
    } else if lname.contains("rune") {
        stats.runes += 1;
        stats.runes_found += 1;
        msgs.push("Rune collected");
    } else if lname.contains("bonecharm") {
        stats.bone_charms += 1;
        stats.charms_found += 1;
        msgs.push("Bone charm collected");
    } else {
        // an item for the inventory (an invitation), or a thing to carry
        let item = s("pickup_item");
        if !item.is_empty() {
            vm.campaign_fx.push(crate::campaign::CampaignFx::AbstractItem(item));
        }
        *stats.items.entry(name.clone()).or_default() += 1;
        let label = s("pickup_label");
        if !label.is_empty() {
            msgs.push(label);
        }
    }
}

/// A character begging on its knees for the scripts (`DisSeqAct_AIDoSimpleBehaviors` Beg:
/// `Empty_Beg_Kneeling_In`, its loop until aborted, `_Out`); its own mind waits meanwhile.
#[derive(Component, Default)]
pub struct Begging {
    pub stop: bool,
    phase: u8,
}

pub fn beg(mut commands: Commands, mut q: Query<(Entity, &mut Begging, &mut crate::anim::Animator, &Npc)>) {
    for (e, mut b, mut a, n) in &mut q {
        let clip = |a: &crate::anim::Animator, names: &[&str]| a.lib.first(names);
        // (gone down, or no such clips: back to itself)
        let done = n.is_down() || clip(&a, &["Empty_Beg_Kneeling_In"]).is_none();
        match b.phase {
            _ if done => {
                commands.entity(e).try_remove::<(Begging, crate::npc::ScriptedAnim)>();
            }
            0 => {
                if let Some(c) = clip(&a, &["Empty_Beg_Kneeling_In"]) {
                    a.restart(c, false, 1.0, 0.25);
                }
                commands.entity(e).try_insert(crate::npc::ScriptedAnim);
                b.phase = 1;
            }
            1 if a.finished() || b.stop => {
                if let Some(c) = clip(&a, &["Empty_Beg_Kneeling_Loop"]) {
                    a.restart(c, true, 1.0, 0.2);
                }
                b.phase = 2;
            }
            2 if b.stop => {
                if let Some(c) = clip(&a, &["Empty_Beg_Kneeling_Out", "Empty_Beg_Kneeling_Out_Down"]) {
                    a.restart(c, false, 1.0, 0.2);
                }
                b.phase = 3;
            }
            3 if a.finished() => {
                commands.entity(e).try_remove::<(Begging, crate::npc::ScriptedAnim)>();
            }
            _ => {}
        }
    }
}
