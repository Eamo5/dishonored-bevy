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
use crate::gameplay::{HudMessages, PlayerStats};
use crate::level::{GROUP_PROP, GROUP_WORLD};
use crate::npc::{Kind, Mode, Npc, NPC_CENTER};
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
            .add_systems(Update, (script_overrides, start, ride).chain().run_if(in_state(GameState::InGame)))
            .init_resource::<PossessAim>()
            .add_systems(OnExit(GameState::InGame), |mut a: ResMut<PossessAim>| *a = PossessAim::default())
            .add_systems(Update, aim.run_if(in_state(GameState::InGame)));
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
    /// the way in still to go (`m_IntroDuration`: control passes once it ends)
    pub intro: f32,
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
    *pos = Possession { host: Some(host), left: saved.left, level: saved.level, exit_requested: saved.exit_requested, warned: saved.warned, body: saved.body, exit: saved.exit, intro: 0.0 };
}

/// The host the aim settles on, among those Corvo may take or (`refused_too`) also those he
/// may not, and whether it is one he may not.
#[allow(clippy::too_many_arguments)]
fn best_host<'a>(
    ctx: &RapierContext,
    player: Entity,
    eye: Vec3,
    dir: Vec3,
    range: f32,
    level: u8,
    npcs: impl Iterator<Item = (Entity, &'a Transform, &'a Npc)>,
    hosts: impl Iterator<Item = (Entity, Vec3)>,
    overrides: &PossessOverrides,
    refused_too: bool,
) -> Option<(Entity, bool)> {
    let filter = QueryFilter::default().exclude_collider(player).groups(CollisionGroups::new(Group::ALL, GROUP_WORLD | GROUP_PROP));
    let mut best: Option<(f32, Entity, bool)> = None;
    let people = npcs.filter_map(|(e, t, npc)| {
        // (the scripts may allow a story character, or forbid anyone)
        let ov = overrides.0.get(&npc.spawner);
        let refused = ov.is_some_and(|o| o.disallow) || (npc.kind == Kind::Story && ov.is_none());
        if npc.is_down() || npc.floored || (refused && !refused_too) {
            return None;
        }
        let animal = npc.kind == Kind::Creature;
        // people only at level 2 (`m_RequiredPossessPowerLevel`), whatever they know of Corvo but
        // while choked (`Twk_Possessable_BaseNPC.m_AwarenessSettings`: every state but the last)
        if !animal && (level < ov.map(|o| o.level).unwrap_or(2) || npc.mode == Mode::Choked) {
            return None;
        }
        // (the target's priority: `m_TargetingPriority`, combatants over civilians over animals)
        let weight = if animal { 0.0 } else if npc.fighter() { 2.0 } else { 1.0 };
        Some((e, t.translation + Vec3::Y * 0.3, weight, refused))
    });
    // the other creatures: any level
    for (e, chest, weight, refused) in people.chain(hosts.map(|(e, c)| (e, c, 0.0, false))) {
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
        // the original's choice (as roshan's r614/blink research decoded it): the angle counts
        // ten times the distance, each as how far inside its limit, plus the priority
        let fov = 0.4 / 0.3;
        let score = 10.0 * (fov - angle) / fov + (range - d) / range + weight;
        if best.is_none_or(|b| score > b.0) {
            best = Some((score, e, refused));
        }
    }
    best.map(|b| (b.1, b.2))
}

/// Possession aimed and let go, as its component has it (`UDishonoredActivePowerComponent_Possess`,
/// decoded from the user's executable for this rewrite: its tick 0x00BFD6D0 runs a state by
/// `+0xB8`; the aim, state 1, 0x00BFD300; let go, 0x00BF3790; given up, 0x00BFB520). Held, the
/// hand comes up (`Powers_Cast_Possession_In`, `_Loop`) and the aim settles on a host each frame,
/// the possessable's own highlight on it (`m_pTargetingHighlightEffect` at
/// `m_TargetingHighlightEffectAttachSocket`: a person's `PossesTarget_Human_01` at the chest's
/// `WindBlast_Impact`, a rat's `PossesTarget_Rats_01` at its `Possess`). On someone he may not
/// take, the possessable's refusal (`m_DisallowPossessionMessage`, "This target cannot be
/// possessed right now") is up while it stays there (0x00BF66A0, only while aiming). Let go on
/// a host, he goes in (`_OutUU`); on none, the aim just ends. Use gives it up ("Cancel
/// Possess", `m_InteractionTexts[24]`): `m_pCancelSoundEvent`, the arms' `_Cancel`, and the
/// power held back `m_fCancelCooldown`.
#[derive(Resource, Default)]
pub struct PossessAim {
    pub on: bool,
    pub cancel_seq: u32,
    target: Option<(Entity, bool)>,
    shown: Option<(Entity, &'static str)>,
    /// the highlight's own tag (it follows the host)
    tag: Option<Entity>,
    refused: bool,
    /// the power held back after a cancel (s), and the hand held on after it
    cooldown: f32,
    swallow: bool,
}

/// A target's highlight and where on it (a rat's, else a person's at the chest).
fn highlight(rat: bool) -> (&'static str, Vec3) {
    if rat {
        ("possess_target_rat", Vec3::ZERO)
    } else {
        ("possess_target", Vec3::Y * 0.35)
    }
}

#[allow(clippy::too_many_arguments, clippy::type_complexity)]
fn aim(
    mut commands: Commands,
    (mouse, keys, bind, paused, cursor, scripted, time): (Res<ButtonInput<MouseButton>>, Res<ButtonInput<KeyCode>>, Res<crate::bindings::Bindings>, Res<crate::hud::Paused>, Single<&bevy::window::CursorOptions>, Option<Res<crate::script::Scripted>>, Res<Time>),
    (mut st, mut powers, mut stats, data, attrs, tune, settings, tc, held, mut possession, overrides): (
        ResMut<PossessAim>,
        ResMut<crate::powers::Powers>,
        ResMut<PlayerStats>,
        Res<Data>,
        Res<crate::gamedata::Attrs>,
        Res<crate::musicbox::Tune>,
        Res<crate::settings::Settings>,
        Res<crate::gameplay::TimeControl>,
        Res<crate::props::Held>,
        ResMut<Possession>,
        Res<PossessOverrides>,
    ),
    rapier: ReadRapierContext,
    player: Query<(Entity, &Player)>,
    cam: Query<&GlobalTransform, With<crate::player::PlayerCamera>>,
    npcs: Query<(Entity, &Transform, &Npc), Without<Player>>,
    hosts: Query<(Entity, &GlobalTransform, &Host), Without<Possessed>>,
    (mut msgs, mut sfx, mut fx, mut start_possess, mut used): (ResMut<HudMessages>, MessageWriter<PostEvent>, MessageWriter<crate::particles::SpawnEffect>, MessageWriter<PossessRequest>, MessageWriter<crate::powers::PowerUsed>),
    mut tagged: Query<(&crate::particles::EffectTag, &mut crate::particles::ParticleEmitter)>,
    level_info: Option<Res<crate::level::LevelInfo>>,
) {
    if paused.0 {
        return;
    }
    st.cooldown = (st.cooldown - time.delta_secs()).max(0.0);
    let power = crate::powers::Power::Possess;
    // on the way in (its intro, the component's state 2), Use gives it up too: out again
    if possession.host.is_some() && possession.intro > 0.0 && keys.just_pressed(bind.key(crate::bindings::Act::Use)) {
        possession.intro = 0.0;
        possession.exit_requested = true;
        sfx.write(PostEvent::named("Snd_Power_Empty", None));
        st.cooldown = data.active("Possess").and_then(|a| a.params.get("m_fCancelCooldown")).copied().unwrap_or(1.0);
        return;
    }
    let level = power.level(&stats);
    let Ok((pe, p)) = player.single() else { return };
    let grabbed = cursor.grab_mode != bevy::window::CursorGrabMode::None || scripted.is_some();
    let can_act = grabbed && !stats.dead && !p.locked && !held.busy() && powers.blink.is_none() && tc.wheel >= 1.0 && possession.host.is_none();
    let selected = powers.selected == power;
    if st.swallow && !mouse.pressed(MouseButton::Right) {
        st.swallow = false;
    }
    let refusal = data.text("DisTweaks_Possessable", "m_DisallowPossessionMessage");
    let end = |st: &mut PossessAim, powers: &mut crate::powers::Powers, msgs: &mut HudMessages| {
        st.on = false;
        st.target = None;
        powers.aiming = false;
        if st.refused {
            msgs.drop_tutorial(&refusal);
            st.refused = false;
        }
    };
    if st.on {
        if !selected || !can_act || !powers.aiming {
            end(&mut st, &mut powers, &mut msgs);
        } else if keys.just_pressed(bind.key(crate::bindings::Act::Use)) {
            // "Cancel Possess": its sound, and the power held back
            sfx.write(PostEvent::named("Snd_Power_Empty", None));
            st.cooldown = data.active("Possess").and_then(|a| a.params.get("m_fCancelCooldown")).copied().unwrap_or(1.0);
            st.cancel_seq = st.cancel_seq.wrapping_add(1);
            st.swallow = mouse.pressed(MouseButton::Right);
            end(&mut st, &mut powers, &mut msgs);
        } else if !mouse.pressed(MouseButton::Right) {
            // let go: into the host aimed at, if he may take it
            let cost = power.mana(&data);
            if let Some((host, false)) = st.target {
                if stats.mana >= cost {
                    stats.spend_mana(cost, &attrs);
                    start_possess.write(PossessRequest { host, level });
                    powers.cooldown = 1.0;
                    powers.cast_seq = powers.cast_seq.wrapping_add(1);
                    powers.cast_power = power;
                    used.write(crate::powers::PowerUsed(power));
                } else {
                    crate::powers::out_of_mana(&mut powers, &mut sfx);
                }
            }
            end(&mut st, &mut powers, &mut msgs);
        }
    } else if selected && can_act && !st.swallow && mouse.just_pressed(MouseButton::Right) && powers.cooldown <= 0.0 && st.cooldown <= 0.0 {
        let cost = power.mana(&data);
        if tune.inhibited {
            sfx.write(PostEvent::named("Snd_Power_Empty", None));
            powers.cooldown = 0.5;
        } else {
            // (short of mana: a remedy drunk first, if the option says so)
            if stats.mana < cost && settings.auto_mana_elixir && stats.mana_elixirs > 0 {
                stats.mana_elixirs -= 1;
                stats.mana = (stats.mana + attrs.mana_elixir_amount(stats.max_mana, rand::random::<f32>())).min(stats.max_mana);
                stats.mana_cap = stats.mana_cap.max(stats.mana);
                sfx.write(PostEvent::named("Snd_UI_Ingame_Wheel_Mana", None));
            }
            if stats.mana < cost {
                crate::powers::out_of_mana(&mut powers, &mut sfx);
            } else {
                st.on = true;
                powers.aiming = true;
            }
        }
    }
    // the host the aim settles on, while aiming
    if st.on {
        if let (Ok(ctx), Ok(cg)) = (rapier.single(), cam.single()) {
            let range = data.power_f("Possess", level, "m_fPossessRange", 1000.0) * 0.01;
            st.target = best_host(&ctx, pe, cg.translation(), cg.forward().as_vec3(), range, level, npcs.iter(), hosts.iter().map(|(e, g, _)| (e, g.translation() + Vec3::Y * 0.1)), &overrides, true);
        }
        // the possessable's refusal, up while it stays on one he may not take
        let refused = st.target.is_some_and(|t| t.1) && !refusal.is_empty();
        if refused != st.refused {
            if refused {
                msgs.tutorial(refusal.clone(), f32::INFINITY);
            } else {
                msgs.drop_tutorial(&refusal);
            }
            st.refused = refused;
        }
    }
    // the highlight on the host aimed at (on one he may take)
    let want = st.target.filter(|t| st.on && !t.1).map(|(e, _)| {
        let rat = hosts.get(e).is_ok_and(|(_, _, h)| !h.fish && !h.rooted);
        (e, rat)
    });
    let has = |n: &str| level_info.as_ref().is_some_and(|l| l.scene.effects.iter().any(|e| e.0 == n));
    let want = want.and_then(|(e, rat)| {
        let (name, _) = highlight(rat);
        has(name).then_some((e, name))
    });
    if want != st.shown {
        let tag = *st.tag.get_or_insert_with(|| commands.spawn((Transform::default(), DespawnOnExit(GameState::InGame))).id());
        if st.shown.is_some() {
            for (t, mut em) in &mut tagged {
                if t.0 == tag {
                    em.stop();
                }
            }
        }
        if let Some((e, name)) = want {
            let rat = name == "possess_target_rat";
            fx.write(crate::particles::SpawnEffect { follow: Some(e), tag: Some(tag), secs: 600.0, ..crate::particles::SpawnEffect::at(name, highlight(rat).1) });
        }
        st.shown = want;
    }
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
            power_jump: 0.0, pull: Vec3::ZERO, air_peak: 0.0, speed_cap: 0.0,
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
    overrides: Res<PossessOverrides>,
    white_rats: Query<(), With<crate::swarm::WhiteRat>>,
    stats: Res<crate::gameplay::PlayerStats>,
    (settings, mut tc): (Res<crate::settings::Settings>, ResMut<crate::gameplay::TimeControl>),
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
        *pos = Possession { host: Some(r.host), left: secs, level: r.level, exit_requested: false, warned: false, body: Some(body), exit: None, intro: data.power_f("Possess", r.level, "m_IntroDuration", 1.1) };
        extend_bend(&data, &mut tc, r.level);
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
    *pos = Possession { host: Some(r.host), left, level: r.level, exit_requested: false, warned: false, body, exit: ov.and_then(|o| o.exit), intro: data.power_f("Possess", r.level, "m_IntroDuration", 1.1) };
    extend_bend(&data, &mut tc, r.level);
}

/// Possessing in bent time draws it out (`m_fBendTimeExtension`: 4 s at level 2, as roshan's
/// r614/blink research found).
fn extend_bend(data: &Data, tc: &mut crate::gameplay::TimeControl, level: u8) {
    if tc.bend_remaining > 0.0 {
        tc.bend_remaining += data.power_f("Possess", level, "m_fBendTimeExtension", if level >= 2 { 4.0 } else { 0.0 });
    }
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
    mut sfx: MessageWriter<PostEvent>,
    ai: Res<crate::aiworld::AiWorld>,
) {
    let Some(host) = pos.host else { return };
    let Ok((pe, mut pt, mut p, mut col, mut kcc)) = player.single_mut() else { return };
    let dt = time.delta_secs();
    // the way in (`m_IntroDuration`): held until it is done, the time inside not yet running
    if pos.intro > 0.0 {
        pos.intro -= dt;
        p.locked = pos.intro > 0.0;
        if p.locked {
            return;
        }
    }
    pos.left -= dt;
    // inside a creature that isn't a character
    if let Some(body) = pos.body.filter(|_| !npcs.contains(host)) {
        if pos.left < 4.0 && !pos.warned {
            pos.warned = true;
            // (the original says so with its lens alone: `m_WarnMaxBlur`, `ppgraph.rs`)
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
        // (the original says so with its lens alone)
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
        // (reeling: thrown off as he comes out, then dazed or retching, `npc.rs`)
        npc.unpossessed = true;
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
    for mut v in &mut arms {
        *v = Visibility::Inherited;
    }
    sfx.write(PostEvent::named(data.power_sound("Possess", pos.level, "m_pOutroSoundEvent").unwrap_or("Snd_Power_Possession_Out"), None));
    *pos = Possession::default();
}
