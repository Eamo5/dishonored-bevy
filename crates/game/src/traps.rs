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
            .add_systems(Update, (restore_traps, trap_clocks, trip_wires, launch, fly_darts).chain().after(crate::player::PlayerMoveSet).run_if(in_state(GameState::InGame)));
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
    collider: Option<Entity>,
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
    source: Option<Entity>,
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
    let collider = if def.launcher {
        // the tripod stands in the way
        let c = commands.spawn((Transform::from_translation(Vec3::Y * 0.35), Collider::cuboid(0.3, 0.35, 0.3), CollisionGroups::new(GROUP_PROP, Group::ALL))).id();
        commands.entity(root).add_child(c);
        Some(c)
    } else { None };
    let mut ec = commands.entity(root);
    ec.insert(TrapPart { index, launcher: def.launcher, sprung: false, disarmed: false, firing: None, wire, sockets, collider });
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

/// Both tripwire and launcher skeletons advance on the same clock as their timers.
fn trap_clocks(tc: Res<TimeControl>, mut traps: Query<&mut Animator, With<TrapPart>>) {
    for mut anim in &mut traps { anim.time_scale = tc.world_scale().max(0.0); }
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
    let dt = time.delta_secs() * tc.world_scale().max(0.0);
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
        // Accept the order while stopped, but defer its timer and projectile.
        if dt <= 0.0 { continue; }
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
        let dart = commands.spawn((Dart { trap: t.index, source: t.collider, vel, gravity: g, life: 5.0 }, Transform::from_translation(from), Visibility::default(), DespawnOnExit(GameState::InGame))).id();
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
    player: Query<(&Transform, &Player), Without<Dart>>,
    npcs: Query<(Entity, &crate::npc::Npc, &Transform), Without<Dart>>,
    (mut stats, mut msgs): (ResMut<PlayerStats>, ResMut<HudMessages>),
    (mut blasts, mut hits): (MessageWriter<crate::gadgets::Explosion>, MessageWriter<NpcHit>),
    mut sfx: MessageWriter<PostEvent>,
) {
    let Some(level) = level else { return };
    let dt = time.delta_secs() * tc.world_scale();
    let Ok(ctx) = rapier.single() else { return };
    let walls = QueryFilter::default().exclude_sensors().groups(CollisionGroups::new(Group::ALL, GROUP_WORLD | GROUP_PROP));
    let pp = player.single().ok().map(|(t, p)| (t.translation, p.crouched));
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
        let direction = step / len;
        let filter = d.source.map(|e| walls.exclude_collider(e)).unwrap_or(walls);
        let wall = ctx.cast_ray_and_get_normal(a, direction, len, true, filter);
        let mut nearest = wall.as_ref().map(|(_, h)| h.time_of_impact).unwrap_or(len);
        let mut hit = wall.map(|(_, h)| (a + direction * h.time_of_impact + h.normal * 0.1, None, false));
        for (ne, n, nt) in &npcs {
            if n.is_down() { continue; }
            if let Some(distance) = crate::krust::capsule_ray_hit(a, direction, nearest, nt.translation,
                crate::npc::NPC_HALF, crate::npc::NPC_RADIUS + 0.05).filter(|d| hit.is_none() || *d < nearest) {
                nearest = distance;
                hit = Some((a + direction * distance, Some(ne), false));
            }
        }
        if let Some((p, crouched)) = pp.filter(|_| !stats.dead) {
            let half = if crouched { crate::player::CROUCH_HALF } else { crate::player::STAND_HALF };
            if let Some(distance) = crate::krust::capsule_ray_hit(a, direction, nearest, p, half, crate::player::RADIUS + 0.05)
                .filter(|d| hit.is_none() || *d < nearest) {
                hit = Some((a + direction * distance, None, true));
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
                blasts.write(crate::gadgets::Explosion { at, radius: b.radius, full: b.full, damage, effect: "grenade", player: Some([b.player_radius, b.player_full]), kind: crate::gameplay::HitKind::Explosion });
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

#[cfg(test)]
mod projectile_tests {
    use super::*;

    #[test]
    fn stopped_launcher_retains_zero_delay_shot_until_time_resumes() {
        let mut app = App::new();
        app.init_resource::<Time>().insert_resource(TimeControl { bend_remaining: 10.0, world_dilation: 0.0, ..default() })
            .init_resource::<TrapLog>().add_message::<PostEvent>().add_message::<SpawnEffect>()
            .insert_resource(LevelInfo { scene: dhcook::format::Scene { traps: vec![dhcook::format::Trap {
                launcher: true, speed: 10.0, fire_at: 0.0, ..default()
            }], ..default() } }).add_systems(Update, (trap_clocks, launch).chain());
        let skeleton = dhcook::format::SkeletonDef::default();
        let lib = std::sync::Arc::new(crate::anim::CharAnims::new(&skeleton, vec![]));
        let trap = app.world_mut().spawn((TrapPart { index: 0, launcher: true, sprung: true, disarmed: false,
            firing: Some((0.0, false)), wire: (Vec3::ZERO, Vec3::ZERO), sockets: vec![], collider: None },
            Transform::default(), Animator::new(lib, &skeleton, vec![]))).id();
        app.world_mut().resource_mut::<Time>().advance_by(std::time::Duration::from_millis(100));
        app.update();
        assert_eq!(app.world().get::<TrapPart>(trap).unwrap().firing, Some((0.0, false)));
        assert_eq!(app.world().get::<Animator>(trap).unwrap().time_scale, 0.0);
        assert_eq!(app.world_mut().query::<&Dart>().iter(app.world()).count(), 0);
        app.world_mut().resource_mut::<TimeControl>().world_dilation = 0.5;
        app.update();
        assert_eq!(app.world().get::<TrapPart>(trap).unwrap().firing, Some((0.05, true)));
        assert_eq!(app.world().get::<Animator>(trap).unwrap().time_scale, 0.5);
        assert_eq!(app.world_mut().query::<&Dart>().iter(app.world()).count(), 1);
        app.update();
        assert_eq!(app.world_mut().query::<&Dart>().iter(app.world()).count(), 1, "resuming must fire the queued shot only once");
    }

    #[test]
    fn disarming_requires_active_gameplay_and_clear_access_to_launcher() {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, AssetPlugin::default(), bevy::scene::ScenePlugin, TransformPlugin, RapierPhysicsPlugin::<NoUserData>::default()))
            .init_resource::<Assets<Mesh>>().init_resource::<ButtonInput<KeyCode>>().init_resource::<Bindings>()
            .insert_resource(crate::hud::Paused(true)).init_resource::<InteractFocus>()
            .init_resource::<crate::carry::Carry>().init_resource::<crate::possession::Possession>()
            .init_resource::<crate::props::Held>().init_resource::<PlayerStats>().init_resource::<HudMessages>()
            .init_resource::<TrapLog>().init_resource::<crate::gamedata::Attrs>().add_message::<SpawnEffect>()
            .insert_resource(LevelInfo { scene: dhcook::format::Scene { traps: vec![dhcook::format::Trap { launcher: true, ..default() }], ..default() } })
            .add_systems(Last, disarm_focus);
        app.world_mut().spawn((PlayerCamera, Transform::from_xyz(0.0, 0.4, 1.5)));
        let collider = app.world_mut().spawn((Collider::cuboid(0.3, 0.35, 0.3), Transform::from_xyz(0.0, 0.35, 0.0), CollisionGroups::new(GROUP_PROP, Group::ALL))).id();
        let trap = app.world_mut().spawn((TrapPart { index: 0, launcher: true, sprung: false, disarmed: false,
            firing: None, wire: (Vec3::ZERO, Vec3::ZERO), sockets: vec![], collider: Some(collider) }, Transform::default())).id();
        let key = app.world().resource::<Bindings>().key(Act::Use);
        app.world_mut().resource_mut::<ButtonInput<KeyCode>>().press(key);
        app.update();
        assert!(!app.world().get::<TrapPart>(trap).unwrap().disarmed);
        assert!(!app.world().resource::<InteractFocus>().0);
        app.world_mut().resource_mut::<crate::hud::Paused>().0 = false;
        let cover = app.world_mut().spawn((Collider::cuboid(1.0, 1.0, 0.01), Transform::from_xyz(0.0, 0.4, 0.75), CollisionGroups::new(GROUP_WORLD, Group::ALL))).id();
        for group in [GROUP_WORLD, GROUP_PROP] {
            app.world_mut().entity_mut(cover).insert(CollisionGroups::new(group, Group::ALL));
            app.update();
            assert!(!app.world().get::<TrapPart>(trap).unwrap().disarmed);
            assert!(!app.world().resource::<InteractFocus>().0);
        }
        app.world_mut().entity_mut(cover).insert(Sensor);
        app.world_mut().resource_mut::<PlayerStats>().dead = true;
        app.update();
        assert!(!app.world().get::<TrapPart>(trap).unwrap().disarmed);
        app.world_mut().resource_mut::<PlayerStats>().dead = false;
        app.update();
        assert!(app.world().get::<TrapPart>(trap).unwrap().disarmed);
        assert_eq!(app.world().resource::<TrapLog>().state.get(&0), Some(&2));
        assert!(app.world().resource::<InteractFocus>().0);
    }

    #[test]
    fn fast_darts_respect_cover_crouching_and_explode_at_first_contact() {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, AssetPlugin::default(), bevy::scene::ScenePlugin, TransformPlugin, RapierPhysicsPlugin::<NoUserData>::default()))
            .init_resource::<Assets<Mesh>>().init_resource::<TimeControl>().init_resource::<PlayerStats>()
            .init_resource::<HudMessages>().init_resource::<crate::settings::Settings>()
            .add_message::<NpcHit>().add_message::<PostEvent>().add_message::<crate::gadgets::Explosion>()
            .insert_resource(bevy::time::TimeUpdateStrategy::ManualDuration(std::time::Duration::from_millis(100)))
            .insert_resource(LevelInfo { scene: dhcook::format::Scene { traps: vec![dhcook::format::Trap { damage: 7.0, ..default() }], ..default() } })
            .add_systems(Last, fly_darts);
        let player = app.world_mut().spawn((Transform::from_xyz(3.0, 0.0, 0.0), Player {
            velocity: Vec3::ZERO, yaw: 0.0, pitch: 0.0, crouched: false, sprinting: false,
            grounded: true, lean: 0.0, noclip: false, eye_height: 1.0, locked: false,
            air_time: 0.0, spawn: Vec3::ZERO, mantle: None, step_timer: 0.0,
            fall_speed: 0.0, power_jump: 0.0, pull: Vec3::ZERO,
        })).id();
        let source = app.world_mut().spawn((Collider::ball(0.8), Transform::default(), CollisionGroups::new(GROUP_PROP, Group::ALL))).id();
        let wall = app.world_mut().spawn((Collider::cuboid(0.01, 2.0, 2.0), Transform::from_xyz(1.0, 0.0, 0.0), CollisionGroups::new(GROUP_WORLD, Group::ALL))).id();
        let fire = |app: &mut App, y| {
            let e = app.world_mut().spawn((Dart { trap: 0, source: Some(source), vel: Vec3::X * 50.0, gravity: 0.0, life: 5.0 }, Transform::from_xyz(0.0, y, 0.0))).id();
            app.update();
            e
        };
        app.update();
        let health = app.world().resource::<PlayerStats>().health;
        for group in [GROUP_WORLD, GROUP_PROP] {
            app.world_mut().entity_mut(wall).insert(CollisionGroups::new(group, Group::ALL));
            let dart = fire(&mut app, 0.6);
            assert!(app.world().get::<Dart>(dart).is_none());
            assert_eq!(app.world().resource::<PlayerStats>().health, health);
        }
        app.world_mut().entity_mut(wall).insert(Sensor);
        fire(&mut app, 0.6);
        assert_eq!(app.world().resource::<PlayerStats>().health, health - 7.0);
        app.world_mut().get_mut::<Player>(player).unwrap().crouched = true;
        app.world_mut().get_mut::<Transform>(player).unwrap().translation.y = -0.4;
        let overhead = fire(&mut app, 0.6);
        assert!(app.world().get::<Dart>(overhead).is_some());
        assert_eq!(app.world().resource::<PlayerStats>().health, health - 7.0);
        fire(&mut app, -0.4);
        assert_eq!(app.world().resource::<PlayerStats>().health, health - 14.0);
        app.world_mut().entity_mut(wall).remove::<Sensor>();
        app.world_mut().resource_mut::<LevelInfo>().scene.traps[0].blast = Some(dhcook::format::TrapBlast { damage: [20.0; 4], ..default() });
        fire(&mut app, 0.0);
        let blasts = app.world().resource::<Messages<crate::gadgets::Explosion>>();
        let mut cursor = blasts.get_cursor();
        let blast = cursor.read(blasts).next().unwrap();
        assert!((blast.at.x - 0.89).abs() < 0.001, "blast must be on the near side of cover, not at a farther victim: {:?}", blast.at);
    }
}

/// [Use] at a launcher not yet fired: disarm it for its ammunition.
#[allow(clippy::too_many_arguments)]
fn disarm_focus(
    (keys, bind): (Res<ButtonInput<KeyCode>>, Res<Bindings>),
    (paused, rapier): (Res<crate::hud::Paused>, ReadRapierContext),
    level: Option<Res<LevelInfo>>,
    mut focus: ResMut<InteractFocus>,
    mut traps: Query<(&mut TrapPart, &Transform, Option<&mut Animator>)>,
    globals: Query<&GlobalTransform>,
    player: Query<&Player>,
    cam: Query<&GlobalTransform, With<PlayerCamera>>,
    (carry, possession, held): (Res<crate::carry::Carry>, Res<crate::possession::Possession>, Res<crate::props::Held>),
    (mut stats, mut msgs, mut log, attrs): (ResMut<PlayerStats>, ResMut<HudMessages>, ResMut<TrapLog>, Res<crate::gamedata::Attrs>),
    mut fx: MessageWriter<SpawnEffect>,
) {
    if paused.0 || stats.dead { return; }
    let Some(level) = level else { return };
    if focus.0 || carry.carrying() || possession.host.is_some() || held.0.is_some() || player.single().is_ok_and(|p| p.locked) {
        return;
    }
    let Ok(c) = cam.single() else { return };
    let Ok(ctx) = rapier.single() else { return };
    let (eye, fwd) = (c.translation(), c.forward().as_vec3());
    let near = traps
        .iter_mut()
        .filter(|(t, tf, _)| {
            let launcher = level.scene.traps.get(t.index as usize).is_some_and(|d| d.launcher);
            let to = tf.translation + Vec3::Y * 0.4 - eye;
            let distance = to.length();
            if !launcher || t.sprung || t.disarmed || distance >= REACH || to.normalize_or_zero().dot(fwd) <= 0.8 { return false; }
            let filter = QueryFilter::default().exclude_sensors().groups(CollisionGroups::new(Group::ALL, GROUP_WORLD | GROUP_PROP));
            let filter = t.collider.map(|e| filter.exclude_collider(e)).unwrap_or(filter);
            ctx.cast_ray(eye, to / distance, distance, true, filter).is_none()
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
        let (name, n) = crate::gadgets::give_ammo(&mut stats, &attrs, ty, n);
        msgs.push(format!("{name} +{n}"));
    }
}
