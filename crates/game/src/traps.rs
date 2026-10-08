//! Tripwire traps (`DisTripwire`, `DisProjectileLauncher`): a wire strung low across a passage,
//! and launchers on tripods aimed at it. Walking into the wire snaps it (its "TripodTrap_Fire"
//! sequence) and raises its `DisSeqEvent_Tripwire`; the level's script then fires the
//! launchers (`DisSeqAct_ActivateProjectileLauncher`), which shoot their projectile
//! (`m_pProjectileTweak`: the whiskey launcher's invisible explosive arrow, the bolt launcher's
//! bolt) at their target point at the sequence's `DishonoredNotify_FireProjectile`. A launcher
//! not yet fired can be disarmed ([Use] "Disarm"): its "TripodTrap_Defuse" sequence, and its
//! ammunition (`m_HarvestedAmmo`) is Corvo's. Jumping or Blinking over the wire, or a creature
//! small enough to pass under it, leaves it be.

use crate::anim::Animator;
use crate::audio::PostEvent;
use crate::bindings::{hint, Act, Bindings};
use crate::gameplay::{HitKind, HudMessages, NpcHit, PlayerStats, TimeControl};
use crate::interact::{InteractFocus, Interaction};
use crate::level::{GameAssets, LevelInfo, GROUP_PROP, GROUP_WORLD};
use crate::particles::SpawnEffect;
use crate::player::{Player, PlayerCamera};
use crate::world_light::{LitActor, WorldLighting};
use crate::GameState;
use bevy::camera::visibility::DynamicSkinnedMeshBounds;
use bevy::mesh::skinning::SkinnedMesh;
use bevy::mesh::MeshTag;
use bevy::prelude::*;
use bevy_rapier3d::prelude::*;
use dhcook::format::Trap as TrapDef;
use dhcook::xform::{rot_matrix, ue_to_bevy};
use std::collections::HashMap;

pub struct TrapsPlugin;

impl Plugin for TrapsPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<TrapLog>()
            .add_systems(OnEnter(GameState::InGame), spawn_traps.after(crate::level::LevelSpawnSet))
            .add_systems(Update, disarm_focus.after(crate::interact::FocusSet).before(crate::interact::use_focus).run_if(in_state(GameState::InGame)))
            .add_systems(Update, (restore_traps, trip_wires, launch, fly_darts).chain().after(crate::player::PlayerMoveSet).run_if(in_state(GameState::InGame)));
    }
}

/// The original's gravity (1500 uu/s²), which the shots' `m_fGravityMultiplier` scales.
pub(crate) const GRAVITY: f32 = 15.0;
/// How near Corvo disarms a launcher (m).
const REACH: f32 = 2.0;

/// What became of the level's traps (for saves): sprung (1) or disarmed (2).
#[derive(Resource, Default)]
pub struct TrapLog {
    pub state: HashMap<u32, u8>,
    /// a save was loaded: set them as it had them
    pub restore: bool,
}

#[derive(Component)]
pub struct TrapPart {
    index: u32,
    launcher: bool,
    /// a wire tripped, a launcher fired
    sprung: bool,
    disarmed: bool,
    /// a launcher firing: seconds into its sequence, and whether it has shot yet
    firing: Option<(f32, bool)>,
    /// a wire: its ends
    wire: (Vec3, Vec3),
    /// its sockets: (name, joint, offset from the joint)
    sockets: Vec<(String, Entity, Transform)>,
}

impl TrapPart {
    /// a wire's ends (none for a launcher)
    pub fn wire(&self) -> Option<(Vec3, Vec3)> {
        (!self.launcher).then_some(self.wire)
    }
    pub fn describe(&self) -> String {
        format!("#{} {} sprung {} disarmed {}", self.index, if self.launcher { "launcher" } else { "wire" }, self.sprung, self.disarmed)
    }
    fn socket(&self, name: &str, globals: &Query<&GlobalTransform>) -> Option<GlobalTransform> {
        let (_, j, at) = self.sockets.iter().find(|s| s.0.eq_ignore_ascii_case(name))?;
        globals.get(*j).ok().map(|g| g.mul_transform(*at))
    }
}

/// A launcher's shot in flight.
#[derive(Component)]
struct Dart {
    trap: u32,
    vel: Vec3,
    gravity: f32,
    life: f32,
}

fn spawn_traps(mut commands: Commands, assets: Option<Res<GameAssets>>, level: Option<Res<LevelInfo>>, mut wl: Option<ResMut<WorldLighting>>, mut log: ResMut<TrapLog>) {
    *log = TrapLog::default();
    let (Some(assets), Some(level)) = (assets, level) else { return };
    let mut n = 0;
    for (i, def) in level.scene.traps.iter().enumerate() {
        let Some(vis) = def.npc_type.and_then(|t| assets.npc_types.get(t as usize)).and_then(|v| v.as_ref()) else { continue };
        let slot = wl.as_mut().map(|w| w.alloc_slot()).unwrap_or(0);
        spawn_trap(&mut commands, vis, def, i as u32, slot);
        n += 1;
    }
    if n > 0 {
        info!("{n} tripwires and launchers");
    }
}

fn spawn_trap(commands: &mut Commands, vis: &crate::level::NpcVisual, def: &TrapDef, index: u32, slot: u32) {
    let rot = Transform::from_matrix(Mat4::from_cols_array(&ue_to_bevy(rot_matrix(def.rotation)).to_cols_array())).rotation;
    let root_tf = Transform::from_translation(Vec3::from(def.position)).with_rotation(rot).with_scale(Vec3::splat(def.scale.max(0.1)));
    let root = commands.spawn((root_tf, Visibility::default(), LitActor { slot, probe_height: 0.3, brightness: 0.3, color: Vec3::splat(0.05), sun: 0.0, dominant: None }, DespawnOnExit(GameState::InGame))).id();
    // the skeleton, straight under the actor
    let bones = &vis.skeleton.bones;
    let mut joints = Vec::with_capacity(bones.len());
    let mut model: Vec<Transform> = Vec::with_capacity(bones.len());
    for b in bones {
        let local = Transform::from_translation(Vec3::from(b.translation)).with_rotation(Quat::from_array(b.rotation).normalize());
        let m = if b.parent >= 0 && (b.parent as usize) < model.len() { model[b.parent as usize] * local } else { local };
        model.push(m);
        joints.push(commands.spawn(local).id());
    }
    for (i, b) in bones.iter().enumerate() {
        let parent = if b.parent >= 0 && (b.parent as usize) < i { joints[b.parent as usize] } else { root };
        commands.entity(parent).add_child(joints[i]);
    }
    for (mesh, mat) in &vis.parts.parts {
        let mut ec = commands.spawn((
            Mesh3d(mesh.clone()),
            MeshTag(slot),
            SkinnedMesh { inverse_bindposes: vis.inverse_bindposes.clone(), joints: joints.clone() },
            DynamicSkinnedMeshBounds,
            crate::fxlight::LitPart,
        ));
        mat.apply(&mut ec);
        let m = ec.id();
        commands.entity(root).add_child(m);
    }
    let bone = |name: &str| bones.iter().position(|b| b.name.eq_ignore_ascii_case(name));
    let sockets = vis
        .skeleton
        .sockets
        .iter()
        .map(|s| {
            let j = bone(&s.bone).map(|i| joints[i]).unwrap_or(root);
            (s.name.clone(), j, Transform::from_translation(Vec3::from(s.translation)).with_rotation(Quat::from_array(s.rotation).normalize()))
        })
        .collect();
    // the wire runs between its pulleys (else its rope's ends)
    let at = |names: &[&str]| names.iter().find_map(|n| bone(n)).map(|i| root_tf.transform_point(model[i].translation));
    let wire = if def.launcher {
        (Vec3::ZERO, Vec3::ZERO)
    } else {
        let rope: Vec<usize> = (0..bones.len()).filter(|&i| bones[i].name.to_ascii_lowercase().starts_with("rope_jnt")).collect();
        let a = at(&["wheel_left_jnt1", "wheel_left_jnt"]).or_else(|| rope.last().map(|&i| root_tf.transform_point(model[i].translation)));
        let b = at(&["wheel_right_jnt1", "wheel_right_jnt"]).or_else(|| rope.first().map(|&i| root_tf.transform_point(model[i].translation)));
        (a.unwrap_or(root_tf.translation), b.unwrap_or(root_tf.translation))
    };
    if def.launcher {
        // the tripod stands in the way
        let c = commands.spawn((Transform::from_translation(Vec3::Y * 0.35), Collider::cuboid(0.3, 0.35, 0.3), CollisionGroups::new(GROUP_PROP, Group::ALL))).id();
        commands.entity(root).add_child(c);
    }
    let mut ec = commands.entity(root);
    ec.insert(TrapPart { index, launcher: def.launcher, sprung: false, disarmed: false, firing: None, wire, sockets });
    if let Some(lib) = vis.anims.clone() {
        ec.insert(Animator::new(lib, &vis.skeleton, joints));
    }
}

/// Play one of its sequences (from the start, or straight to its end).
fn play(a: &mut Animator, name: &str, at_end: bool) {
    let Some(clip) = a.lib.find(name) else { return };
    a.restart(clip, false, 1.0, 0.0);
    if at_end {
        a.sounds = false;
        let d = a.lib.duration(clip);
        a.seek(d);
    } else {
        a.sounds = true;
    }
}

/// A loaded save's traps: the sprung and disarmed ones as they were left.
fn restore_traps(mut log: ResMut<TrapLog>, level: Option<Res<LevelInfo>>, mut traps: Query<(&mut TrapPart, Option<&mut Animator>)>) {
    if !log.restore {
        return;
    }
    log.restore = false;
    let Some(level) = level else { return };
    for (mut t, a) in &mut traps {
        let Some(&s) = log.state.get(&t.index) else { continue };
        let Some(def) = level.scene.traps.get(t.index as usize) else { continue };
        t.sprung = s == 1;
        t.disarmed = s == 2;
        if let Some(mut a) = a {
            play(&mut a, if t.disarmed { &def.disarm_anim } else { &def.fire_anim }, true);
        }
    }
}

/// Corvo walking into a wire snaps it.
#[allow(clippy::too_many_arguments)]
fn trip_wires(
    level: Option<Res<LevelInfo>>,
    mut traps: Query<(&mut TrapPart, Option<&mut Animator>)>,
    player: Query<(&Transform, &Player)>,
    (possession, stats): (Res<crate::possession::Possession>, Res<PlayerStats>),
    mut log: ResMut<TrapLog>,
    mut used: MessageWriter<Interaction>,
) {
    let Some(level) = level else { return };
    let Ok((pt, p)) = player.single() else { return };
    if p.noclip || stats.dead {
        return;
    }
    // his body: a creature's, else his own
    let (half, radius) = match possession.body.as_ref() {
        Some(b) => (b.half, b.radius),
        None => (if p.crouched { crate::player::CROUCH_HALF } else { crate::player::STAND_HALF }, crate::player::RADIUS),
    };
    let lo = pt.translation - Vec3::Y * half;
    let hi = pt.translation + Vec3::Y * half;
    for (mut t, a) in &mut traps {
        let Some(def) = level.scene.traps.get(t.index as usize) else { continue };
        if def.launcher || t.sprung || t.disarmed {
            continue;
        }
        if crate::krust::segment_distance(t.wire.0, t.wire.1, lo, hi) > radius {
            continue;
        }
        t.sprung = true;
        log.state.insert(t.index, 1);
        if std::env::var("DH_TRAP_LOG").is_ok() {
            info!("tripwire {} tripped at {:.2}", def.name, pt.translation);
        }
        if let Some(mut a) = a {
            play(&mut a, &def.fire_anim, false);
        }
        used.write(Interaction::Tripwire(t.index));
    }
}

/// Launchers the scripts fire: their sequence, and the shot at its notify.
#[allow(clippy::too_many_arguments)]
fn launch(
    mut commands: Commands,
    time: Res<Time>,
    tc: Res<TimeControl>,
    level: Option<Res<LevelInfo>>,
    vm: Option<ResMut<crate::kismet::Vm>>,
    mut traps: Query<(&mut TrapPart, &Transform, Option<&mut Animator>)>,
    globals: Query<&GlobalTransform>,
    player: Query<&Transform, With<Player>>,
    mut log: ResMut<TrapLog>,
    (mut sfx, mut fx): (MessageWriter<PostEvent>, MessageWriter<SpawnEffect>),
) {
    let Some(level) = level else { return };
    let dt = time.delta_secs() * tc.world_scale();
    let orders = vm.map(|mut vm| std::mem::take(&mut vm.launches)).unwrap_or_default();
    let log_on = std::env::var("DH_TRAP_LOG").is_ok();
    for (mut t, tf, a) in &mut traps {
        let Some(def) = level.scene.traps.get(t.index as usize) else { continue };
        if !def.launcher {
            continue;
        }
        if orders.contains(&t.index) && !t.sprung && !t.disarmed && t.firing.is_none() {
            t.firing = Some((0.0, false));
            t.sprung = true;
            log.state.insert(t.index, 1);
            if let Some(mut a) = a {
                play(&mut a, &def.fire_anim, false);
            }
            if log_on {
                info!("launcher {} fires", def.name);
            }
        }
        let Some((secs, shot)) = t.firing else { continue };
        let secs = secs + dt;
        t.firing = Some((secs, shot));
        if shot || secs < def.fire_at {
            if secs > 3.0 {
                t.firing = None;
            }
            continue;
        }
        t.firing = Some((secs, true));
        // the shot, from its socket, at its target (else at Corvo, else straight ahead)
        let from = t.socket(&def.socket, &globals).or_else(|| t.socket("Disarm_Socket", &globals)).map(|g| g.translation()).unwrap_or(tf.translation + Vec3::Y * 0.5);
        let to = def.target.map(Vec3::from).or_else(|| player.single().ok().map(|p| p.translation).filter(|p| p.distance(from) < 30.0)).unwrap_or(from + tf.rotation * Vec3::NEG_Z * 10.0);
        let g = GRAVITY * def.gravity;
        let vel = aim(from, to, def.speed.max(1.0), g);
        if log_on {
            info!("launcher {} shoots from {from:.2} at {to:.2}", def.name);
        }
        if let Some((sock, ps)) = &def.fire_fx {
            let at = t.socket(sock, &globals).map(|g| g.translation()).unwrap_or(from);
            fx.write(SpawnEffect { system: Some(*ps), secs: 2.0, ..SpawnEffect::at("", at) });
        }
        if !def.fly_sound.is_empty() {
            sfx.write(PostEvent::named(&def.fly_sound, Some(from)));
        }
        let dart = commands.spawn((Dart { trap: t.index, vel, gravity: g, life: 5.0 }, Transform::from_translation(from), Visibility::default(), DespawnOnExit(GameState::InGame))).id();
        if let Some(trail) = def.trail {
            fx.write(SpawnEffect { follow: Some(dart), system: Some(trail), secs: 5.0, ..SpawnEffect::at("", Vec3::ZERO) });
        }
    }
}

/// The velocity that carries a shot from `from` to `to` at `speed` under gravity `g` (the low
/// arc; straight at it when out of reach).
pub(crate) fn aim(from: Vec3, to: Vec3, speed: f32, g: f32) -> Vec3 {
    let d = to - from;
    let flat = d.with_y(0.0);
    let x = flat.length();
    if x < 0.01 || g <= 0.0 {
        return d.normalize_or(Vec3::NEG_Z) * speed;
    }
    let v2 = speed * speed;
    let disc = v2 * v2 - g * (g * x * x + 2.0 * d.y * v2);
    if disc < 0.0 {
        return d.normalize_or(Vec3::NEG_Z) * speed;
    }
    let tan = (v2 - disc.sqrt()) / (g * x);
    (flat / x + Vec3::Y * tan).normalize() * speed
}

/// The shots in flight: whoever or whatever they strike, they burst on (or wound).
#[allow(clippy::too_many_arguments)]
fn fly_darts(
    mut commands: Commands,
    time: Res<Time>,
    tc: Res<TimeControl>,
    rapier: ReadRapierContext,
    level: Option<Res<LevelInfo>>,
    settings: Res<crate::settings::Settings>,
    mut darts: Query<(Entity, &mut Dart, &mut Transform)>,
    player: Query<&Transform, (With<Player>, Without<Dart>)>,
    npcs: Query<(Entity, &crate::npc::Npc, &Transform), Without<Dart>>,
    (mut stats, mut msgs): (ResMut<PlayerStats>, ResMut<HudMessages>),
    (mut blasts, mut hits): (MessageWriter<crate::gadgets::Explosion>, MessageWriter<NpcHit>),
    mut sfx: MessageWriter<PostEvent>,
) {
    let Some(level) = level else { return };
    let dt = time.delta_secs() * tc.world_scale();
    let Ok(ctx) = rapier.single() else { return };
    let walls = QueryFilter::default().groups(CollisionGroups::new(Group::ALL, GROUP_WORLD));
    let pp = player.single().ok().map(|t| t.translation);
    for (e, mut d, mut t) in &mut darts {
        d.life -= dt;
        let Some(def) = level.scene.traps.get(d.trap as usize).filter(|_| d.life > 0.0) else {
            commands.entity(e).despawn();
            continue;
        };
        d.vel.y -= d.gravity * dt;
        let step = d.vel * dt;
        let len = step.length();
        if len <= 0.0 {
            continue;
        }
        let a = t.translation;
        // what it strikes first: a character in the way, else the world
        let mut hit: Option<(Vec3, Option<Entity>, bool)> = None;
        if let Some((ne, _, _)) = npcs.iter().find(|(_, n, nt)| {
            !n.is_down() && crate::krust::segment_distance(a, a + step, nt.translation - Vec3::Y * crate::npc::NPC_HALF, nt.translation + Vec3::Y * crate::npc::NPC_HALF) < crate::npc::NPC_RADIUS + 0.05
        }) {
            hit = Some((a + step * 0.5, Some(ne), false));
        }
        if let Some(p) = pp.filter(|_| hit.is_none() && !stats.dead) {
            let (lo, hi) = (p - Vec3::Y * crate::player::STAND_HALF, p + Vec3::Y * crate::player::STAND_HALF);
            if crate::krust::segment_distance(a, a + step, lo, hi) < crate::player::RADIUS + 0.05 {
                hit = Some((a + step * 0.5, None, true));
            }
        }
        if hit.is_none() {
            if let Some((_, h)) = ctx.cast_ray_and_get_normal(a, step / len, len, true, walls) {
                hit = Some((a + step / len * h.time_of_impact + h.normal * 0.1, None, false));
            }
        }
        let Some((at, npc, on_player)) = hit else {
            t.translation = a + step;
            continue;
        };
        if std::env::var("DH_TRAP_LOG").is_ok() {
            info!("{} shot strikes at {at:.2}{}", def.name, if on_player { " (Corvo)" } else if npc.is_some() { " (someone)" } else { "" });
        }
        match &def.blast {
            Some(b) => {
                let damage = b.damage[settings.difficulty.min(3) as usize];
                blasts.write(crate::gadgets::Explosion { at, radius: b.radius, full: b.full, damage, effect: "grenade", player: Some([b.player_radius, b.player_full]) });
            }
            None => {
                // a bolt: the original's damage
                if let Some(ne) = npc {
                    hits.write(NpcHit { npc: ne, damage: def.damage, kind: HitKind::Bolt, from: a });
                    sfx.write(PostEvent::named("Imp_Bullet_on_Body", Some(at)));
                } else if on_player {
                    stats.hit_from = Some(a);
                    crate::gameplay::hurt_player(&mut stats, &mut msgs, &mut sfx, def.damage);
                }
            }
        }
        commands.entity(e).despawn();
    }
}

/// [Use] at a launcher not yet fired: disarm it for its ammunition.
#[allow(clippy::too_many_arguments)]
fn disarm_focus(
    (keys, bind): (Res<ButtonInput<KeyCode>>, Res<Bindings>),
    level: Option<Res<LevelInfo>>,
    mut focus: ResMut<InteractFocus>,
    mut traps: Query<(&mut TrapPart, &Transform, Option<&mut Animator>)>,
    globals: Query<&GlobalTransform>,
    player: Query<&Player>,
    cam: Query<&GlobalTransform, With<PlayerCamera>>,
    (carry, possession, held): (Res<crate::carry::Carry>, Res<crate::possession::Possession>, Res<crate::props::Held>),
    (mut stats, mut msgs, mut log): (ResMut<PlayerStats>, ResMut<HudMessages>, ResMut<TrapLog>),
    mut fx: MessageWriter<SpawnEffect>,
) {
    let Some(level) = level else { return };
    if focus.0 || carry.carrying() || possession.host.is_some() || held.0.is_some() || player.single().is_ok_and(|p| p.locked) {
        return;
    }
    let Ok(c) = cam.single() else { return };
    let (eye, fwd) = (c.translation(), c.forward().as_vec3());
    let near = traps
        .iter_mut()
        .filter(|(t, tf, _)| {
            let launcher = level.scene.traps.get(t.index as usize).is_some_and(|d| d.launcher);
            let to = tf.translation + Vec3::Y * 0.4 - eye;
            launcher && !t.sprung && !t.disarmed && to.length() < REACH && to.normalize_or_zero().dot(fwd) > 0.8
        })
        .min_by(|a, b| a.1.translation.distance(eye).total_cmp(&b.1.translation.distance(eye)));
    let Some((mut t, _, a)) = near else { return };
    let Some(def) = level.scene.traps.get(t.index as usize) else { return };
    focus.0 = true;
    focus.1 = None;
    let verb = if def.verb.is_empty() { "Disarm" } else { &def.verb };
    focus.2 = format!("{} {verb} {}", hint(Act::Use), def.label).trim_end().to_string();
    if !keys.just_pressed(bind.key(Act::Use)) {
        return;
    }
    t.disarmed = true;
    log.state.insert(t.index, 2);
    if let Some(mut a) = a {
        play(&mut a, &def.disarm_anim, false);
    }
    if let Some((sock, ps)) = &def.disarm_fx {
        if let Some(g) = t.socket(sock, &globals) {
            fx.write(SpawnEffect { system: Some(*ps), secs: 2.0, ..SpawnEffect::at("", g.translation()) });
        }
    }
    if !def.used_message.is_empty() {
        msgs.push(def.used_message.clone());
    }
    for &(ty, n) in &def.harvest {
        let name = crate::gadgets::give_ammo(&mut stats, ty, n);
        msgs.push(format!("{name} +{n}"));
    }
}
