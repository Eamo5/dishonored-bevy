//! Possession: Corvo merges with an animal (level 1) or an unaware person (level 2) and moves
//! as them for a while. The AI sees only the host; when it ends (the left hand again, or the
//! time runs out) Corvo steps out and the host staggers, disoriented.
//!
//! Inside a creature (a wolfhound, a swarm's rat, a fish, a river krust) Corvo has its body
//! (`DisTweaks_Possessable`: the collision cylinder, the ground and water speeds, whether it
//! swims): a rat fits through a rat's ways, a fish swims anywhere in its water without
//! breathing, a krust stays rooted and spits.

use crate::audio::PostEvent;
use crate::gamedata::Data;
use crate::gameplay::{HudMessages, NpcStagger, PlayerStats};
use crate::level::{GROUP_PROP, GROUP_WORLD};
use crate::npc::{Alert, Kind, Mode, Npc, NPC_CENTER};
use crate::player::{Player, RADIUS, STAND_HALF};
use crate::GameState;
use bevy::prelude::*;
use bevy_rapier3d::prelude::*;

pub struct PossessionPlugin;

impl Plugin for PossessionPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Possession>()
            .init_resource::<PossessOverrides>()
            .init_resource::<PossessionRestore>()
            .add_message::<PossessRequest>()
            .add_systems(OnEnter(GameState::InGame), |mut p: ResMut<Possession>, mut o: ResMut<PossessOverrides>| {
                *p = Possession::default();
                *o = PossessOverrides::default();
            })
            .add_systems(Update, restore_possession.after(crate::swarm::restore_swarms).after(crate::fish::restore_fish).after(crate::krust::restore_krust_state).after(crate::save::restore_npcs).before(script_overrides).run_if(in_state(GameState::InGame)))
            .add_systems(Update, (script_overrides, start, ride).chain().run_if(in_state(GameState::InGame)));
    }
}

#[derive(Resource, Default)]
pub struct Possession {
    pub host: Option<Entity>,
    /// seconds left inside the host
    pub left: f32,
    pub level: u8,
    pub exit_requested: bool,
    warned: bool,
    /// the body Corvo has inside a creature that isn't a character
    pub body: Option<HostBody>,
    /// where Corvo comes out (the scripts' exit point), else beside the host
    exit: Option<Vec3>,
}

/// The level scripts' possession settings for characters (`DisSeqAct_OverridePossess`, by
/// spawner): Slackjaw and the bridge's guards can't be taken, the Art Dealer and Samuel can,
/// for so long, and Corvo comes out where the scene needs him.
#[derive(Resource, Default)]
pub struct PossessOverrides(pub std::collections::HashMap<u32, PossessOverride>);

#[derive(Clone, Copy, Debug, serde::Serialize, serde::Deserialize)]
pub struct PossessOverride {
    pub disallow: bool,
    /// seconds at levels 1 and 2 (0: the power's own)
    pub secs: [f32; 2],
    pub level: u8,
    pub exit: Option<Vec3>,
}

fn script_overrides(vm: Option<ResMut<crate::kismet::Vm>>, mut overrides: ResMut<PossessOverrides>) {
    let Some(mut vm) = vm else { return };
    if vm.possess_overrides.is_empty() {
        return;
    }
    let g = vm.g.clone();
    for (targets, op, exit) in std::mem::take(&mut vm.possess_overrides) {
        let spawners = targets.iter().filter_map(|v| if let crate::kismet::Val::Actor(a) = v { g.actors.get(*a as usize)?.spawner } else { None });
        let ov = op.and_then(|op| g.ops.get(op as usize)).map(|o| {
            use dhcook::format::KVal;
            let secs = match o.props.get("possess_secs") {
                Some(KVal::List(l)) => [0, 1].map(|k| if let Some(KVal::Float(f)) = l.get(k) { *f } else { 0.0 }),
                _ => [0.0; 2],
            };
            PossessOverride {
                disallow: matches!(o.props.get("possess_disallow"), Some(KVal::Bool(true))),
                secs,
                level: match o.props.get("possess_level") {
                    Some(KVal::Int(l)) => (*l).clamp(1, 3) as u8,
                    _ => 2,
                },
                exit,
            }
        });
        for s in spawners {
            if std::env::var("DH_SCRIPT_WORLD_LOG").is_ok() {
                info!("level scripts: possession of spawner {s}: {ov:?}");
            }
            match ov {
                Some(o) => {
                    overrides.0.insert(s, o);
                }
                None => {
                    overrides.0.remove(&s);
                }
            }
        }
    }
}

/// A creature that isn't a character and can be possessed (a swarm's rat, a fish, a river
/// krust): its NPC type (for its `possess` body), and where Corvo sits in a rooted one.
#[derive(Component, Clone, Copy)]
pub struct Host {
    pub npc_type: Option<u32>,
    pub rooted: bool,
    pub fish: bool,
    /// a rooted host: Corvo's seat (offset from its origin) and the way it faces
    pub seat: Vec3,
    pub facing: Vec3,
}

/// The body Corvo has inside a creature.
#[derive(Clone, Copy, Debug, serde::Serialize, serde::Deserialize)]
pub struct HostBody {
    /// the capsule's half segment and radius
    pub half: f32,
    pub radius: f32,
    /// the eye above the centre
    pub eye: f32,
    /// m/s on the ground and in the water
    pub ground: f32,
    pub water: f32,
    pub swim: bool,
    pub fish: bool,
    pub rooted: bool,
}

impl HostBody {
    fn of(p: Option<&dhcook::format::Possessable>, host: &Host) -> HostBody {
        // (`Twk_Possessable_BaseAnimal`: 9 uu, 300 uu/s)
        let (h, r, ground, water, swim) = match p {
            Some(p) if p.height > 0.0 => (p.height, p.radius, p.ground_speed, p.water_speed, p.swim),
            _ => (0.09, 0.09, 3.0, 3.0, true),
        };
        let radius = r.min(h).max(0.05);
        HostBody {
            half: (h - radius).max(0.01),
            radius,
            eye: if host.fish { 0.0 } else { h * 0.6 },
            ground,
            water,
            swim: swim || host.fish,
            fish: host.fish,
            rooted: host.rooted,
        }
    }

    /// the centre above the feet
    pub fn center(&self) -> f32 {
        self.half + self.radius + 0.02
    }
}

#[derive(Message, Clone, Copy)]
pub struct PossessRequest {
    pub host: Entity,
    pub level: u8,
}

/// The NPC Corvo is riding: its AI and movement are suspended.
#[derive(Component)]
pub struct Possessed;

#[derive(Component)]
pub(crate) struct RestoredRatHost;

#[derive(Clone, Copy, serde::Serialize, serde::Deserialize)]
pub(crate) enum SavedHost { Npc(u32), Rat, Fish(u32), Krust(u32) }

#[derive(serde::Serialize, serde::Deserialize)]
pub struct PossessionSave {
    host: Option<SavedHost>,
    left: f32,
    level: u8,
    exit_requested: bool,
    warned: bool,
    body: Option<HostBody>,
    exit: Option<Vec3>,
    overrides: std::collections::HashMap<u32, PossessOverride>,
}

impl PossessionSave {
    pub(crate) fn level_return(mut self) -> Self {
        self.host = None;
        self
    }
    pub(crate) fn capture(pos: &Possession, overrides: &PossessOverrides, hosts: impl IntoIterator<Item = (Entity, SavedHost)>) -> Self {
        Self { host: pos.host.and_then(|host| hosts.into_iter().find(|(e, _)| *e == host).map(|(_, key)| key)),
            left: pos.left, level: pos.level, exit_requested: pos.exit_requested, warned: pos.warned, body: pos.body, exit: pos.exit, overrides: overrides.0.clone() }
    }
}

#[derive(Resource, Default)]
pub struct PossessionRestore(pub Option<PossessionSave>);

fn restore_possession(mut commands: Commands, mut pending: ResMut<PossessionRestore>, mut pos: ResMut<Possession>, mut overrides: ResMut<PossessOverrides>,
    npcs: Query<(Entity, &crate::npc::FromSpawner)>, rats: Query<Entity, With<RestoredRatHost>>, fish: Query<(Entity, &crate::fish::Fish)>, krusts: Query<(Entity, &crate::krust::Krust)>,
    mut player: Query<(&mut Player, &mut Collider, &mut KinematicCharacterController)>, mut arms: Query<&mut Visibility, With<crate::arms::ArmsRoot>>) {
    let Some(saved) = pending.0.take() else { return };
    overrides.0 = saved.overrides;
    let host = saved.host.and_then(|key| match key {
        SavedHost::Npc(id) => npcs.iter().find(|(_, s)| s.0 == id).map(|(e, _)| e),
        SavedHost::Rat => rats.iter().next(),
        SavedHost::Fish(id) => fish.iter().find(|(_, f)| f.index() == id).map(|(e, _)| e),
        SavedHost::Krust(id) => krusts.iter().find(|(_, k)| k.index() == id).map(|(e, _)| e),
    });
    let Some(host) = host else { return };
    let Ok((mut p, mut collider, mut controller)) = player.single_mut() else { return };
    p.crouched = false;
    p.velocity = Vec3::ZERO;
    if let Some(body) = saved.body {
        *collider = Collider::capsule_y(body.half, body.radius);
    } else {
        *collider = Collider::capsule_y(STAND_HALF, RADIUS);
    }
    fit_controller(&mut controller, saved.body);
    commands.entity(host).insert(Possessed);
    if matches!(saved.host, Some(SavedHost::Npc(_))) {
        commands.entity(host).insert(ColliderDisabled);
    } else {
        commands.entity(host).insert(Visibility::Hidden);
    }
    for mut v in &mut arms { *v = Visibility::Hidden; }
    *pos = Possession { host: Some(host), left: saved.left, level: saved.level, exit_requested: saved.exit_requested, warned: saved.warned, body: saved.body, exit: saved.exit };
}

/// The best host in front of the player: the original weighs distance against how far off
/// the aim it is (`m_fDistanceWeight` 1, `m_fAngleWeight` 10) within a fraction of the view.
pub fn find_target<'a>(
    ctx: &RapierContext,
    player: Entity,
    eye: Vec3,
    dir: Vec3,
    range: f32,
    level: u8,
    npcs: impl Iterator<Item = (Entity, &'a Transform, &'a Npc)>,
    hosts: impl Iterator<Item = (Entity, Vec3)>,
    overrides: &PossessOverrides,
) -> Option<Entity> {
    let filter = QueryFilter::default().exclude_collider(player).groups(CollisionGroups::new(Group::ALL, GROUP_WORLD | GROUP_PROP));
    let mut best: Option<(f32, Entity)> = None;
    let people = npcs.filter_map(|(e, t, npc)| {
        // (the scripts may allow a story character, or forbid anyone)
        let ov = overrides.0.get(&npc.spawner);
        if npc.is_down() || ov.is_some_and(|o| o.disallow) || (npc.kind == Kind::Story && ov.is_none()) {
            return None;
        }
        let animal = npc.kind == Kind::Creature;
        // people only at level 2, and only while unaware of Corvo
        if !animal && (level < ov.map(|o| o.level).unwrap_or(2) || npc.alert == Alert::Combat || npc.mode == Mode::Combat) {
            return None;
        }
        Some((e, t.translation + Vec3::Y * 0.3))
    });
    // the other creatures: any level
    for (e, chest) in people.chain(hosts) {
        let to = chest - eye;
        let d = to.length();
        if d > range || d < 0.5 {
            continue;
        }
        let angle = to.normalize().dot(dir).clamp(-1.0, 1.0).acos();
        if angle > 0.4 {
            continue;
        }
        if ctx.cast_ray(eye, to / d, d - 0.4, true, filter).is_some() {
            continue;
        }
        let score = d + angle * 10.0;
        if best.is_none_or(|b| score < b.0) {
            best = Some((score, e));
        }
    }
    best.map(|b| b.1)
}

fn white_rat_duration(base: f32, white: bool, bonus: f32) -> f32 {
    base + if white { bonus.max(0.0) } else { 0.0 }
}

#[cfg(test)]
mod charm_tests {
    use super::*;

    #[test]
    fn restoring_rat_possession_remaps_host_and_restores_creature_body() {
        let mut old = World::new();
        let host = old.spawn_empty().id();
        let body = HostBody { half: 0.01, radius: 0.09, eye: 0.05, ground: 3.0, water: 3.0, swim: true, fish: false, rooted: false };
        let pos = Possession { host: Some(host), left: 23.5, level: 1, warned: true, body: Some(body), exit: Some(Vec3::X), ..default() };
        let mut overrides = PossessOverrides::default();
        overrides.0.insert(7, PossessOverride { disallow: true, secs: [4.0, 8.0], level: 2, exit: Some(Vec3::Z) });
        let saved = PossessionSave::capture(&pos, &overrides, [(host, SavedHost::Rat)]);
        let level_return = PossessionSave::capture(&pos, &overrides, [(host, SavedHost::Rat)]).level_return();
        assert!(level_return.host.is_none());
        assert!(level_return.overrides[&7].disallow);
        let saved = serde_json::from_slice::<PossessionSave>(&serde_json::to_vec(&saved).unwrap()).unwrap();
        let mut app = App::new();
        app.init_resource::<Possession>().init_resource::<PossessOverrides>()
            .insert_resource(PossessionRestore(Some(saved))).add_systems(Update, restore_possession);
        for _ in 0..5 { app.world_mut().spawn_empty(); }
        let new_host = app.world_mut().spawn((RestoredRatHost, Visibility::Inherited)).id();
        assert_ne!(host, new_host);
        let player = app.world_mut().spawn((Player {
            velocity: Vec3::Y, yaw: 0.0, pitch: 0.0, crouched: true, sprinting: false, grounded: true, lean: 0.0, noclip: false,
            eye_height: 0.6, locked: false, air_time: 0.0, spawn: Vec3::ZERO, mantle: None, step_timer: 0.0, fall_speed: 0.0,
            power_jump: 0.0, pull: Vec3::ZERO,
        }, Collider::capsule_y(STAND_HALF, RADIUS), KinematicCharacterController::default())).id();
        let arms = app.world_mut().spawn((crate::arms::ArmsRoot, Visibility::Inherited)).id();
        app.update();
        let restored = app.world().resource::<Possession>();
        assert_eq!(restored.host, Some(new_host));
        assert_eq!(restored.left, 23.5);
        assert!(restored.warned);
        assert_eq!(restored.exit, Some(Vec3::X));
        assert_eq!(restored.body.unwrap().radius, 0.09);
        assert!(app.world().get::<Possessed>(new_host).is_some());
        assert_eq!(app.world().get::<Visibility>(new_host), Some(&Visibility::Hidden));
        assert_eq!(app.world().get::<Visibility>(arms), Some(&Visibility::Hidden));
        assert!(!app.world().get::<Player>(player).unwrap().crouched);
        assert_eq!(app.world().get::<Collider>(player).unwrap().as_capsule().unwrap().radius(), 0.09);
        assert!(app.world().resource::<PossessOverrides>().0[&7].disallow);
        // Human hosts use their stable spawner and restore a standing player
        // collider while disabling the host's own collision.
        let npc = app.world_mut().spawn((crate::npc::FromSpawner(42), Visibility::Inherited)).id();
        let human = Possession { host: Some(host), left: 11.0, level: 2, ..default() };
        app.world_mut().resource_mut::<PossessionRestore>().0 = Some(PossessionSave::capture(&human, &overrides, [(host, SavedHost::Npc(42))]));
        app.update();
        assert_eq!(app.world().resource::<Possession>().host, Some(npc));
        assert_eq!(app.world().resource::<Possession>().left, 11.0);
        assert!(app.world().get::<ColliderDisabled>(npc).is_some());
        assert!(app.world().get::<Possessed>(npc).is_some());
        assert_eq!(app.world().get::<Collider>(player).unwrap().as_capsule().unwrap().radius(), RADIUS);
    }

    #[test]
    fn welcoming_host_only_extends_white_rats() {
        assert_eq!(super::white_rat_duration(20.0, true, 10.0), 30.0);
        assert_eq!(super::white_rat_duration(20.0, false, 10.0), 20.0);
        assert_eq!(super::white_rat_duration(20.0, true, 0.0), 20.0);
    }
}

#[allow(clippy::too_many_arguments)]
fn start(
    mut commands: Commands,
    mut requests: MessageReader<PossessRequest>,
    data: Res<Data>,
    level: Option<Res<crate::level::LevelInfo>>,
    mut pos: ResMut<Possession>,
    mut player: Query<(&mut Transform, &mut Player, &mut Collider, &mut KinematicCharacterController)>,
    npcs: Query<(&Transform, &Npc), Without<Player>>,
    hosts: Query<(&Host, &GlobalTransform)>,
    mut arms: Query<&mut Visibility, With<crate::arms::ArmsRoot>>,
    mut sfx: MessageWriter<PostEvent>,
    mut msgs: ResMut<HudMessages>,
    overrides: Res<PossessOverrides>,
    white_rats: Query<(), With<crate::swarm::WhiteRat>>,
    stats: Res<crate::gameplay::PlayerStats>,
    settings: Res<crate::settings::Settings>,
) {
    let Some(r) = requests.read().last().copied() else { return };
    if pos.host.is_some() {
        return;
    }
    let Ok((mut pt, mut p, mut col, mut kcc)) = player.single_mut() else { return };
    // a creature that isn't a character: Corvo takes its body
    if let Ok((host, hg)) = hosts.get(r.host) {
        let ty = host.npc_type.and_then(|t| level.as_ref()?.scene.npc_types.get(t as usize)).and_then(|t| t.possess.clone());
        let body = HostBody::of(ty.as_ref(), host);
        let secs = ty.as_ref().map(|t| t.duration[(r.level.clamp(1, 2) - 1) as usize]).filter(|d| *d > 0.0).unwrap_or(data.0.possess_animal[(r.level.clamp(1, 2) - 1) as usize].max(20.0));
        let secs = white_rat_duration(secs, white_rats.contains(r.host), data.attribute("WhiteRatPossessDurationBonus", settings.difficulty, &stats.powers, &stats.charms));
        info!("possession: creature duration {secs:.2}s, white rat {}", white_rats.contains(r.host));
        let at = hg.translation();
        pt.translation = if host.rooted {
            at + host.seat
        } else if host.fish {
            at
        } else {
            at + Vec3::Y * body.center()
        };
        let f = if host.rooted { host.facing } else { hg.rotation() * Vec3::NEG_Z };
        p.yaw = (-f.x).atan2(-f.z);
        p.pitch = 0.0;
        p.velocity = Vec3::ZERO;
        p.crouched = false;
        p.noclip = false;
        *col = Collider::capsule_y(body.half, body.radius);
        fit_controller(&mut kcc, Some(body));
        // Corvo sees out of it, not its insides
        commands.entity(r.host).insert((Possessed, Visibility::Hidden));
        for mut v in &mut arms {
            *v = Visibility::Hidden;
        }
        sfx.write(PostEvent::named(data.power_sound("Possess", r.level, "m_pIntroSoundEvent").unwrap_or("Snd_Power_Possession_In_Lvl1"), None));
        msgs.push(if host.fish { "Possessing a fish" } else if host.rooted { "Possessing a river krust" } else { "Possessing a rat" });
        *pos = Possession { host: Some(r.host), left: secs, level: r.level, exit_requested: false, warned: false, body: Some(body), exit: None };
        return;
    }
    let Ok((ht, npc)) = npcs.get(r.host) else { return };
    let animal = npc.kind == Kind::Creature;
    let secs = if animal { data.0.possess_animal } else { data.0.possess_human };
    let ov = overrides.0.get(&npc.spawner).copied();
    let left = ov
        .map(|o| o.secs[(r.level.clamp(1, 2) - 1) as usize])
        .filter(|s| *s > 0.0)
        .unwrap_or_else(|| secs[(r.level.clamp(1, 2) - 1) as usize].max(if animal { 20.0 } else { 15.0 }));
    // an animal's body (a wolfhound's)
    let body = if animal {
        level
            .as_ref()
            .and_then(|l| l.scene.npc_types.iter().find(|t| t.name == npc.pawn))
            .and_then(|t| t.possess.as_ref())
            .filter(|p| p.height > 0.0)
            .map(|p| HostBody::of(Some(p), &Host { npc_type: None, rooted: false, fish: false, seat: Vec3::ZERO, facing: Vec3::NEG_Z }))
    } else {
        None
    };
    // Corvo takes the host's place and facing
    let feet = ht.translation - Vec3::Y * NPC_CENTER;
    pt.translation = feet + Vec3::Y * body.map(|b| b.center()).unwrap_or(STAND_HALF + RADIUS + 0.02);
    if let Some(b) = body {
        p.crouched = false;
        *col = Collider::capsule_y(b.half, b.radius);
        fit_controller(&mut kcc, Some(b));
    }
    p.yaw = npc.yaw + std::f32::consts::PI;
    p.velocity = Vec3::ZERO;
    commands.entity(r.host).insert((Possessed, ColliderDisabled));
    for mut v in &mut arms {
        *v = Visibility::Hidden;
    }
    sfx.write(PostEvent::named(data.power_sound("Possess", r.level, "m_pIntroSoundEvent").unwrap_or("Snd_Power_Possession_In_Lvl1"), None));
    msgs.push(format!("Possessing {}", npc.name));
    *pos = Possession { host: Some(r.host), left, level: r.level, exit_requested: false, warned: false, body, exit: ov.and_then(|o| o.exit) };
}

/// The character controller's steps and skin for a body (Corvo's own when `None`).
fn fit_controller(kcc: &mut KinematicCharacterController, body: Option<HostBody>) {
    let (offset, step, width) = match body {
        Some(b) => (0.01, (b.half + b.radius) * 1.2, b.radius * 0.5),
        None => (0.03, 0.42, 0.08),
    };
    kcc.offset = CharacterLength::Absolute(offset);
    kcc.autostep = Some(CharacterAutostep { max_height: CharacterLength::Absolute(step), min_width: CharacterLength::Absolute(width), include_dynamic_bodies: false });
}

/// Where Corvo can stand coming out of a creature at `feet`: there, crouched there, or the
/// nearest free spot around (`DUET_OutFromCenter`). The centre and whether he crouches.
fn stand_out(ctx: &RapierContext, filter: QueryFilter, feet: Vec3) -> (Vec3, bool) {
    use crate::player::CROUCH_HALF;
    let fits = |f: Vec3, half: f32| {
        let c = f + Vec3::Y * (half + RADIUS + 0.03);
        let shape = Collider::capsule_y(half, RADIUS);
        let mut hit = false;
        ctx.intersect_shape(c, Quat::IDENTITY, &*shape.raw, filter, |_| {
            hit = true;
            false
        });
        !hit
    };
    let mut spots = vec![feet];
    for ring in [0.5_f32, 0.9, 1.4] {
        for k in 0..8 {
            let a = k as f32 * std::f32::consts::FRAC_PI_4;
            let d = Vec3::new(a.cos(), 0.0, a.sin()) * ring;
            // only where the way there is clear
            if ctx.cast_ray(feet + Vec3::Y * 0.1, d.normalize(), ring, true, filter).is_none() {
                spots.push(feet + d);
            }
        }
    }
    for f in &spots {
        if fits(*f, STAND_HALF) {
            return (*f + Vec3::Y * (STAND_HALF + RADIUS + 0.03), false);
        }
        if fits(*f, CROUCH_HALF) {
            return (*f + Vec3::Y * (CROUCH_HALF + RADIUS + 0.03), true);
        }
    }
    (feet + Vec3::Y * (CROUCH_HALF + RADIUS + 0.03), true)
}

#[allow(clippy::too_many_arguments, clippy::type_complexity)]
fn ride(
    mut commands: Commands,
    time: Res<Time>,
    data: Res<Data>,
    rapier: ReadRapierContext,
    mut pos: ResMut<Possession>,
    mut stats: ResMut<PlayerStats>,
    mut player: Query<(Entity, &mut Transform, &mut Player, &mut Collider, &mut KinematicCharacterController)>,
    mut npcs: Query<(&mut Transform, &mut Npc), Without<Player>>,
    mut hosts: Query<(&Host, &mut Transform), (Without<Player>, Without<Npc>)>,
    mut arms: Query<&mut Visibility, With<crate::arms::ArmsRoot>>,
    (mut sfx, mut stagger, mut msgs): (MessageWriter<PostEvent>, MessageWriter<NpcStagger>, ResMut<HudMessages>),
    ai: Res<crate::aiworld::AiWorld>,
) {
    let Some(host) = pos.host else { return };
    let Ok((pe, mut pt, mut p, mut col, mut kcc)) = player.single_mut() else { return };
    let dt = time.delta_secs();
    pos.left -= dt;
    // inside a creature that isn't a character
    if let Some(body) = pos.body.filter(|_| !npcs.contains(host)) {
        if pos.left < 4.0 && !pos.warned {
            pos.warned = true;
            msgs.push("Possession is ending");
        }
        let alive = hosts.get(host).is_ok();
        // (where he can't come out, a pipe: held in the creature until out of it)
        if alive && !stats.dead && ai.no_unpossess(pt.translation) {
            pos.left = pos.left.max(0.5);
            pos.exit_requested = false;
        }
        if pos.left > 0.0 && !pos.exit_requested && alive && !stats.dead {
            // the creature goes where Corvo goes
            if let Ok((_, mut ht)) = hosts.get_mut(host) {
                if body.fish {
                    ht.translation = pt.translation;
                    ht.rotation = Quat::from_rotation_y(p.yaw) * Quat::from_rotation_x(p.pitch * 0.7);
                } else if !body.rooted {
                    ht.translation = pt.translation - Vec3::Y * body.center();
                    ht.rotation = Quat::from_rotation_y(p.yaw);
                }
            }
            return;
        }
        if !alive {
            stats.health = 0.0;
            stats.dead = true;
        }
        // out: Corvo stands where the creature was (in front of a rooted one), or the nearest
        // spot with room; out of a fish he's swimming
        let Ok(ctx) = rapier.single() else { return };
        let filter = QueryFilter::default().exclude_collider(pe).groups(CollisionGroups::new(Group::ALL, GROUP_WORLD | GROUP_PROP));
        let feet = match hosts.get(host) {
            Ok((h, ht)) if h.rooted => ht.translation + h.facing.with_y(0.0).normalize_or_zero() * 0.9,
            _ => pt.translation - Vec3::Y * body.center(),
        };
        let (center, crouched) = if body.fish { (pt.translation, false) } else { stand_out(&ctx, filter, feet) };
        pt.translation = center;
        p.crouched = crouched;
        p.velocity = Vec3::ZERO;
        *col = Collider::capsule_y(if crouched { crate::player::CROUCH_HALF } else { STAND_HALF }, RADIUS);
        fit_controller(&mut kcc, None);
        commands.entity(host).try_remove::<Possessed>().try_insert(Visibility::Inherited);
        for mut v in &mut arms {
            *v = Visibility::Inherited;
        }
        sfx.write(PostEvent::named(data.power_sound("Possess", pos.level, "m_pOutroSoundEvent").unwrap_or("Snd_Power_Possession_Out"), None));
        *pos = Possession::default();
        return;
    }
    // the host's centre above Corvo's
    let center = pos.body.map(|b| b.center()).unwrap_or(STAND_HALF + RADIUS + 0.02);
    let alive = npcs.get(host).map(|(_, n)| !n.is_down()).unwrap_or(false);
    if pos.left < 4.0 && !pos.warned {
        pos.warned = true;
        msgs.push("Possession is ending");
    }
    if pos.left > 0.0 && !pos.exit_requested && alive && !stats.dead {
        // the host goes where Corvo goes
        if let Ok((mut ht, mut npc)) = npcs.get_mut(host) {
            ht.translation = pt.translation - Vec3::Y * center + Vec3::Y * NPC_CENTER;
            npc.yaw = p.yaw + std::f32::consts::PI;
            ht.rotation = Quat::from_rotation_y(npc.yaw);
            npc.velocity = Vec3::new(p.velocity.x, 0.0, p.velocity.z);
            npc.anim_speed = npc.velocity.length();
            npc.home = ht.translation;
        }
        return;
    }
    // out: the host is left where it stands, a step ahead, reeling
    if !alive {
        // "if your host were to die, you would endure the same fate"
        stats.health = 0.0;
        stats.dead = true;
    }
    let fwd = Quat::from_rotation_y(p.yaw) * Vec3::NEG_Z;
    if let Ok((mut ht, mut npc)) = npcs.get_mut(host) {
        let Ok(ctx) = rapier.single() else { return };
        let filter = QueryFilter::default().exclude_collider(pe).groups(CollisionGroups::new(Group::ALL, GROUP_WORLD | GROUP_PROP));
        // the first free side: ahead, behind, left, right
        let right = fwd.cross(Vec3::Y);
        let free = |d: Vec3| ctx.cast_ray(ht.translation, d, 1.0, true, filter).map(|(_, t)| (t - 0.4).max(0.0)).unwrap_or(0.9);
        let (dir, step) = [fwd, -fwd, -right, right].into_iter().map(|d| (d, free(d))).find(|(_, s)| *s >= 0.6).unwrap_or((fwd, free(fwd)));
        ht.translation += dir * step;
        npc.velocity = Vec3::ZERO;
        npc.anim_speed = 0.0;
        npc.home = ht.translation;
        npc.wait = 3.0;
    }
    // out of an animal's body: standing room
    if pos.body.is_some() {
        if let Ok(ctx) = rapier.single() {
            let filter = QueryFilter::default().exclude_collider(pe).groups(CollisionGroups::new(Group::ALL, GROUP_WORLD | GROUP_PROP));
            let (c, crouched) = stand_out(&ctx, filter, pt.translation - Vec3::Y * center);
            pt.translation = c;
            p.crouched = crouched;
            *col = Collider::capsule_y(if crouched { crate::player::CROUCH_HALF } else { STAND_HALF }, RADIUS);
            fit_controller(&mut kcc, None);
        }
    }
    // the scripts' exit point
    if let Some(at) = pos.exit {
        pt.translation = at + Vec3::Y * (STAND_HALF + RADIUS + 0.05);
        p.velocity = Vec3::ZERO;
    }
    commands.entity(host).remove::<(Possessed, ColliderDisabled)>();
    stagger.write(NpcStagger { npc: host, secs: 3.0, parried: false });
    for mut v in &mut arms {
        *v = Visibility::Inherited;
    }
    sfx.write(PostEvent::named(data.power_sound("Possess", pos.level, "m_pOutroSoundEvent").unwrap_or("Snd_Power_Possession_Out"), None));
    *pos = Possession::default();
}
