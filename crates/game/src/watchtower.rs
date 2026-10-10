//! Watch towers (`DisWatchTower`, `DisTweaks_WatchTower`): the searchlight sweeps the ground
//! about the tower (between `m_fStateLightHighestRotDeg` and `m_fStateLightLowestRotDeg` from
//! the vertical, turning at up to `m_fStateMaxTowerRotSpeedDeg`), its beam the original
//! `Regent_Light_Cone` stretched from the lamp (`BeamOrigin_Socket`) to where it falls
//! (`m_fDesiredLightRadius` wide there). Corvo caught in it alerts the tower (its chirp, the
//! scripts' `DisSeqEvent_WatchTower` "Activated"); the beam follows him, the attack warning
//! sounds `m_fSoundAttackWarningTime` before the first volley (`m_fStateAttackInitialDelay`),
//! and it fires `m_VolleyShots` explosive arrows (`m_pProjectileTweak`) from `Arrow_Socket`
//! every `m_fDelayBetweenVolleys` while it sees him, giving up after `m_fStateAttackTimeout`.
//! Its whale oil tank taken, it goes dark. Looking into the beam up close blinds
//! (`m_fBlindnessDistance`, `m_fBlindnessRadius`, `m_fBlindnessAngle`: the post-process
//! graph's Blinded node).

use crate::audio::PostEvent;
use crate::gameplay::PlayerStats;
use crate::level::{GameAssets, LevelInfo, LevelSpawnSet, GROUP_PROP, GROUP_WORLD};
use crate::particles::SpawnEffect;
use crate::player::{Player, PlayerCamera};
use crate::GameState;
use bevy::mesh::MeshTag;
use bevy::prelude::*;
use bevy_rapier3d::prelude::*;

pub struct WatchTowerPlugin;

impl Plugin for WatchTowerPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Blinding>()
            .add_systems(OnEnter(GameState::InGame), setup_towers.after(LevelSpawnSet))
            .add_systems(Update, (towers, fly_arrows).chain().run_if(in_state(GameState::InGame)));
    }
}

/// How blinded Corvo is by a searchlight, 0..1 (the Blinded node's opacity).
#[derive(Resource, Default)]
pub struct Blinding(pub f32);

#[derive(Clone, Copy, PartialEq, Debug)]
enum State {
    /// sweeping (seconds into the sweep)
    Explore,
    /// someone caught: seconds since
    Alert(f32),
    /// firing: seconds to the next shot, shots left in the volley
    Attack(f32, u32),
}

#[derive(Component)]
struct Tower {
    /// `scene.security`
    index: usize,
    origin: Vec3,
    arrow: Vec3,
    /// where its tank sits (the tower is dark without one there), if it had one; its
    /// receptacle, and whether the tank was looked for (the level's props come after it)
    tank: Option<Vec3>,
    receptacle: Option<Vec3>,
    tank_found: bool,
    yaw: f32,
    /// the beam's angle from straight down
    pitch: f32,
    sweep: f32,
    state: State,
    /// seconds without seeing its target
    lost: f32,
    warned: bool,
    cone: Option<Entity>,
    cone_size: Vec3,
    /// the head turns about the pole: its axis, its heading at rest (the beam's), and its
    /// parts as they stand then (the level's instances: housing, head; the gun and the
    /// generator at their sockets)
    pivot: Vec3,
    yaw0: f32,
    head: Vec<(Entity, Transform)>,
}

#[derive(Component)]
struct Arrow {
    tower: usize,
    vel: Vec3,
    gravity: f32,
    life: f32,
}

#[allow(clippy::too_many_arguments)]
fn setup_towers(
    mut commands: Commands,
    level: Option<Res<LevelInfo>>,
    assets: Option<Res<GameAssets>>,
    mut wl: Option<ResMut<crate::world_light::WorldLighting>>,
    props: Query<(&crate::props::Prop, &Transform)>,
    instances: Query<(Entity, &crate::level::LevelInstance, &Transform), Without<crate::props::Prop>>,
) {
    let (Some(level), Some(assets)) = (level, assets) else { return };
    let mut n = 0;
    for (i, d) in level.scene.security.iter().enumerate() {
        if d.kind != "WatchTower" {
            continue;
        }
        let (Some(sk), Some(rig)) = (d.skeleton.and_then(|s| level.scene.skeletons.get(s as usize)), d.rig) else { continue };
        let rig = Mat4::from_cols_array(&rig);
        // the bind pose, in the tower's frame
        let mut model: Vec<Mat4> = Vec::with_capacity(sk.bones.len());
        for b in &sk.bones {
            let local = Mat4::from_rotation_translation(Quat::from_array(b.rotation).normalize(), Vec3::from(b.translation));
            let m = if b.parent >= 0 && (b.parent as usize) < model.len() { model[b.parent as usize] * local } else { local };
            model.push(m);
        }
        let socket = |name: &str| -> Option<Mat4> {
            let s = sk.sockets.iter().find(|s| s.name.eq_ignore_ascii_case(name))?;
            let b = sk.bones.iter().position(|b| b.name.eq_ignore_ascii_case(&s.bone))?;
            Some(rig * model[b] * Mat4::from_rotation_translation(Quat::from_array(s.rotation).normalize(), Vec3::from(s.translation)))
        };
        let beam = socket("BeamOrigin_Socket").or_else(|| socket("Light_Socket")).unwrap_or(rig * Mat4::from_translation(Vec3::Y * 10.0));
        let origin = beam.w_axis.truncate();
        let arrow = socket("Arrow_Socket").map(|m| m.w_axis.truncate()).unwrap_or(origin);
        // the beam's bind heading, to start the sweep from
        let dir = beam.transform_vector3(Vec3::NEG_Y).normalize_or(Vec3::NEG_Z);
        let yaw = (-dir.x).atan2(-dir.z);
        // its tank, if one sits in the receptacle
        let at = socket("Oil_Receptacle").or_else(|| socket("Battery_Socket")).map(|m| m.w_axis.truncate());
        let tank = at.filter(|a| props.iter().any(|(p, t)| level.scene.movables.get(p.index).is_some_and(|m| m.tank.is_some()) && t.translation.distance(*a) < 2.5));
        let tank_found = props.iter().next().is_some();
        // the beam: the cone's mesh (1.87 m long, 0.54 across, pointing down its -Y)
        let mut cone = None;
        let mut cone_size = Vec3::new(0.54, 1.87, 0.54);
        if let (Some(parts), Some((mesh, _))) = (assets.props.get(&format!("wt_cone_{i}")), d.cone.as_ref()) {
            if let Some(m) = level.scene.meshes.get(*mesh as usize) {
                cone_size = Vec3::new((m.max[0] - m.min[0]) * 0.5, (m.max[1] - m.min[1]).max(0.1), (m.max[2] - m.min[2]) * 0.5);
            }
            let slot = wl.as_mut().map(|w| w.alloc_slot()).unwrap_or(0);
            let root = commands.spawn((Transform::from_translation(origin), Visibility::default(), DespawnOnExit(GameState::InGame))).id();
            for (mesh, mat) in &parts.parts {
                let mut ec = commands.spawn((Mesh3d(mesh.clone()), MeshTag(slot), bevy::light::NotShadowCaster));
                mat.apply(&mut ec);
                let e = ec.id();
                commands.entity(root).add_child(e);
            }
            cone = Some(root);
        }
        let p = |k: &str, v: f32| d.params.get(k).copied().unwrap_or(v);
        let pitch = (p("m_fStateLightHighestRotDeg", 60.0) + p("m_fStateLightLowestRotDeg", 70.0)).to_radians() * 0.5;
        // the head's parts: the level's instances of the tower above its pole, the gun and
        // the generator wall (placed at the actor, attached to their sockets in the original)
        let pivot = rig.w_axis.truncate();
        let mut head = Vec::new();
        for (e, li, tf) in &instances {
            if li.actor != d.actor {
                continue;
            }
            let name = level.scene.instances.get(li.index as usize).and_then(|x| level.scene.meshes.get(x.mesh as usize)).map(|m| m.name.to_ascii_lowercase()).unwrap_or_default();
            let at = if name.contains("gun") {
                socket("Gun_Socket")
            } else if name.contains("generator") {
                socket("Generator_Socket")
            } else if name.contains("base") {
                continue;
            } else {
                Some(Mat4::from_scale_rotation_translation(tf.scale, tf.rotation, tf.translation))
            };
            if let Some(m) = at {
                head.push((e, Transform::from_matrix(m)));
            }
        }
        commands.spawn((
            Tower { index: i, origin, arrow, tank, receptacle: at, tank_found, yaw, pitch, sweep: 0.0, state: State::Explore, lost: 0.0, warned: false, cone, cone_size, pivot, yaw0: yaw, head },
            Transform::from_translation(origin),
            DespawnOnExit(GameState::InGame),
        ));
        n += 1;
    }
    if n > 0 {
        info!("{n} watch towers");
    }
}

/// Turn `from` towards `to` by at most `step` (radians, the short way).
fn turn(from: f32, to: f32, step: f32) -> f32 {
    let d = (to - from + std::f32::consts::PI).rem_euclid(std::f32::consts::TAU) - std::f32::consts::PI;
    from + d.clamp(-step, step)
}

/// The beam's direction: its heading and its angle from straight down.
fn beam_dir(yaw: f32, pitch: f32) -> Vec3 {
    let h = Quat::from_rotation_y(yaw) * Vec3::NEG_Z;
    (h * pitch.sin() + Vec3::NEG_Y * pitch.cos()).normalize()
}

#[allow(clippy::too_many_arguments)]
fn towers(
    mut commands: Commands,
    time: Res<Time>,
    tc: Res<crate::gameplay::TimeControl>,
    level: Option<Res<LevelInfo>>,
    rapier: ReadRapierContext,
    mut towers: Query<&mut Tower>,
    mut cones: Query<(&mut Transform, &mut Visibility), (Without<Player>, Without<crate::props::Prop>, Without<crate::npc::Npc>)>,
    player: Query<(Entity, &Transform), With<Player>>,
    cam: Query<&GlobalTransform, With<PlayerCamera>>,
    props: Query<(Entity, &crate::props::Prop, &Transform), (Without<Player>, Without<Tower>)>,
    (stats, mut blinding, mut sfx, mut fx): (Res<PlayerStats>, ResMut<Blinding>, MessageWriter<PostEvent>, MessageWriter<SpawnEffect>),
    mut vm: Option<ResMut<crate::kismet::Vm>>,
    (mut devices, npcs): (ResMut<crate::security::Devices>, Query<(&crate::npc::Npc, &Transform), (Without<Player>, Without<crate::props::Prop>, Without<Tower>)>),
) {
    let Some(level) = level else { return };
    let Ok(ctx) = rapier.single() else { return };
    let dt = time.delta_secs() * tc.world_scale();
    // Keep due attacks and scripted volleys queued while the world is stopped.
    // A zero delta alone would still allow perception and zero-delay shots.
    if dt <= 0.0 { return; }
    let mut blind = 0.0f32;
    let Ok((pe, pt)) = player.single() else { return };
    let chest = pt.translation + Vec3::Y * 0.3;
    let walls = QueryFilter::default().exclude_collider(pe).exclude_sensors().groups(CollisionGroups::new(Group::ALL, GROUP_WORLD | GROUP_PROP));
    for mut t in &mut towers {
        let t = &mut *t;
        let Some(d) = level.scene.security.get(t.index) else { continue };
        // the head as it is turned now: the lamp and the arrows' socket with it
        let turned = Quat::from_rotation_y(t.yaw - t.yaw0);
        let round = |p: Vec3| t.pivot + turned * (p - t.pivot);
        for (e, bind) in &t.head {
            if let Ok((mut ht, _)) = cones.get_mut(*e) {
                ht.translation = round(bind.translation);
                ht.rotation = turned * bind.rotation;
            }
        }
        let lamp = round(t.origin);
        let arrow_at = round(t.arrow);
        // the scripts' order to fire at something (`DisSeqAct_WatchTowerShootAtTarget`): a volley
        let ordered: Vec<Vec3> = match vm.as_mut() {
            Some(vm) if vm.tower_shots.iter().any(|(a, _)| *a == d.actor) => {
                let (mine, rest): (Vec<_>, Vec<_>) = std::mem::take(&mut vm.tower_shots).into_iter().partition(|(a, _)| *a == d.actor);
                vm.tower_shots = rest;
                mine.into_iter().map(|(_, at)| at).collect()
            }
            _ => Vec::new(),
        };
        for at in ordered {
            let speed = d.params.get("m_fProjectileSpeed").copied().unwrap_or(2250.0) * 0.01;
            let g = crate::traps::GRAVITY * d.params.get("gravity").copied().unwrap_or(1.0);
            for k in 0..d.params.get("m_VolleyShots").copied().unwrap_or(3.0) as u32 {
                let jitter = Vec3::new(rand_unit(), 0.0, rand_unit()) * k.min(1) as f32 * 0.5;
                let vel = crate::traps::aim(arrow_at, at + jitter, speed, g);
                let arrow = commands.spawn((Arrow { tower: t.index, vel, gravity: g, life: 6.0 }, Transform::from_translation(arrow_at), Visibility::default(), DespawnOnExit(GameState::InGame))).id();
                if let Some(trail) = d.trail {
                    fx.write(SpawnEffect { follow: Some(arrow), system: Some(trail), secs: 6.0, ..SpawnEffect::at("", Vec3::ZERO) });
                }
            }
            if let Some(s) = d.sounds.get("fly") {
                sfx.write(PostEvent::named(s, Some(arrow_at)));
            }
        }
        // the scripts working it (`DisSeqAct_WatchTower`): off, on, toggled, made to explore or
        // to look for someone, its polarity switched
        let cmds: Vec<(u8, Vec<crate::kismet::Val>)> = match vm.as_mut() {
            Some(vm) if !vm.tower_cmds.is_empty() => {
                let g = vm.g.clone();
                let is_me = |vals: &Vec<crate::kismet::Val>| vals.iter().any(|v| matches!(v, crate::kismet::Val::Actor(a) if g.actors.get(*a as usize).is_some_and(|ka| ka.name == d.actor)));
                let (mine, rest): (Vec<_>, Vec<_>) = std::mem::take(&mut vm.tower_cmds).into_iter().partition(|(targets, _, _)| is_me(targets));
                vm.tower_cmds = rest;
                mine.into_iter().map(|(_, i, track)| (i, track)).collect()
            }
            _ => Vec::new(),
        };
        if let Some(dev) = devices.list.iter_mut().find(|x| x.def.actor == d.actor) {
            for (i, _track) in &cmds {
                match i {
                    0 => dev.off = true,
                    1 => dev.off = false,
                    2 => dev.off = !dev.off,
                    3 => t.state = State::Explore,
                    4 => t.state = State::Alert(0.0),
                    _ => dev.rewired = !dev.rewired,
                }
            }
        }
        let p = |k: &str, v: f32| d.params.get(k).copied().unwrap_or(v);
        // dark without its tank, or turned off by the scripts; rewired, it turns on its owners
        let (on, hacked) = devices.state(&d.actor).unwrap_or((true, false));
        // (its tank: the one in its receptacle once the props are there)
        if !t.tank_found && props.iter().next().is_some() {
            t.tank_found = true;
            t.tank = t.receptacle.filter(|a| props.iter().any(|(_, pr, pt)| level.scene.movables.get(pr.index).is_some_and(|m| m.tank.is_some()) && pt.translation.distance(*a) < 2.5));
            if std::env::var("DH_TOWER_LOG").is_ok() {
                let near = t.receptacle.and_then(|a| props.iter().filter(|(_, pr, _)| level.scene.movables.get(pr.index).is_some_and(|m| m.tank.is_some())).map(|(_, _, pt)| pt.translation.distance(a)).min_by(f32::total_cmp));
                info!("watch tower {}: receptacle {:.2?}, nearest tank {near:.2?} m", d.actor, t.receptacle);
            }
        }
        // (and its tank has oil left)
        let powered = on && t.tank.is_none_or(|a| props.iter().any(|(e, pr, pt)| level.scene.movables.get(pr.index).is_some_and(|m| m.tank.is_some()) && pt.translation.distance(a) < 2.5 && devices.charged(e)));
        if let Some((_, mut v)) = t.cone.and_then(|c| cones.get_mut(c).ok()) {
            let want = if powered { Visibility::Inherited } else { Visibility::Hidden };
            if *v != want {
                *v = want;
            }
        }
        if !powered {
            t.state = State::Explore;
            continue;
        }
        let reach = p("m_fLightRadius", 7000.0) * 0.01;
        let spot = p("m_fDesiredLightRadius", 300.0) * 0.01;
        let tower_speed = p("m_fStateMaxTowerRotSpeedDeg", 40.0).to_radians();
        let light_speed = p("m_fStateMaxLightRotSpeedDeg", 40.0).to_radians();
        // where the beam falls
        let dir = beam_dir(t.yaw, t.pitch);
        // (from clear of the tower's own head)
        const OUT: f32 = 2.0;
        let hit = ctx.cast_ray(lamp + dir * OUT, dir, reach - OUT, true, walls).map(|(_, d)| d + OUT).unwrap_or(reach).max(OUT);
        // its target in it, and in the tower's sight: Corvo, or (rewired) the nearest of its
        // owners up
        let chest = if hacked {
            match npcs
                .iter()
                .filter(|(n, _)| !n.is_down() && d.friendly.iter().any(|f| *f == n.faction))
                .map(|(_, nt)| nt.translation)
                .min_by(|a, b| a.distance(lamp + dir * hit).total_cmp(&b.distance(lamp + dir * hit)))
            {
                Some(at) => at,
                None => Vec3::splat(1.0e6),
            }
        } else {
            chest
        };
        let v = chest - lamp;
        let along = v.dot(dir);
        let perp = (v - dir * along).length();
        let in_beam = (hacked || !stats.dead) && along > 0.0 && along < hit + 2.0 && perp < along / hit * spot + 0.6;
        let clear = in_beam && ctx.cast_ray(lamp + v.normalize_or_zero() * OUT, v.normalize_or_zero(), v.length() - 0.5 - OUT, true, walls).is_none();
        let sees = in_beam && clear;
        if std::env::var("DH_TOWER_LOG").is_ok() && (t.sweep * 0.5).fract() < dt * 0.5 {
            info!("watch tower {}: {:?} lamp {lamp:.2} beam falls at {:.1} ({hit:.1} m), Corvo along {along:.1} off {perp:.1} clear {clear} blinding {:.2}", d.actor, t.state, lamp + dir * hit, blinding.0);
        }
        // looking into it up close: blinded
        if let Ok(c) = cam.single() {
            let to_lamp = lamp - c.translation();
            if !hacked && sees && to_lamp.length() < p("m_fBlindnessDistance", 5000.0) * 0.01 && perp < p("m_fBlindnessRadius", 250.0) * 0.01 + along / hit * spot {
                let cos = c.forward().as_vec3().dot(to_lamp.normalize_or_zero());
                let edge = p("m_fBlindnessAngle", 5.0).to_radians().cos();
                if cos > edge {
                    blind = blind.max(((cos - edge) / (1.0 - edge) * 4.0).clamp(0.0, 1.0));
                }
            }
        }
        let event = |vm: &mut Option<ResMut<crate::kismet::Vm>>, out: &'static str| {
            if let Some(vm) = vm.as_mut() {
                if let Some(a) = vm.g.actors.iter().position(|a| a.name == d.actor) {
                    vm.actor_event(a as u32, &["DisSeqEvent_WatchTower"], crate::kismet::OutSel::Desc(out), Some(crate::kismet::Val::Player));
                }
            }
        };
        let sound = |sfx: &mut MessageWriter<PostEvent>, k: &str, at: Vec3| {
            if let Some(s) = d.sounds.get(k) {
                sfx.write(PostEvent::named(s, Some(at)));
            }
        };
        match t.state {
            State::Explore => {
                // the sweep: round the tower, the beam nodding between its angles
                t.sweep += dt;
                t.yaw += tower_speed * 0.5 * dt;
                let (hi, lo) = (p("m_fStateLightHighestRotDeg", 60.0).to_radians(), p("m_fStateLightLowestRotDeg", 70.0).to_radians());
                let want = hi + (lo - hi) * (0.5 + 0.5 * (t.sweep * 0.6).sin());
                t.pitch = turn(t.pitch, want, light_speed * dt);
                if sees {
                    if std::env::var("DH_TOWER_LOG").is_ok() {
                        info!("watch tower {}: alerted", d.actor);
                    }
                    t.state = State::Alert(0.0);
                    t.lost = 0.0;
                    t.warned = false;
                    sound(&mut sfx, "m_pSoundChirp", lamp);
                    event(&mut vm, "Activated");
                }
            }
            State::Alert(s) | State::Attack(s, _) => {
                // the beam follows Corvo
                let flat = (chest - lamp).with_y(0.0);
                if flat.length() > 0.1 {
                    t.yaw = turn(t.yaw, (-flat.x).atan2(-flat.z), tower_speed * dt);
                }
                let down = (chest - lamp).normalize_or(Vec3::NEG_Y);
                t.pitch = turn(t.pitch, down.dot(Vec3::NEG_Y).clamp(-1.0, 1.0).acos(), light_speed * dt);
                t.lost = if sees { 0.0 } else { t.lost + dt };
                if t.lost > p("m_fStateAttackTimeout", 7.0) {
                    if std::env::var("DH_TOWER_LOG").is_ok() {
                        info!("watch tower {}: lost him", d.actor);
                    }
                    t.state = State::Explore;
                    sound(&mut sfx, "m_pSoundTargetLost", lamp);
                    event(&mut vm, "Deactivated");
                    continue;
                }
                if let State::Alert(s) = t.state {
                    let s = s + dt;
                    let delay = p("m_fStateAttackInitialDelay", 2.0);
                    if !t.warned && s >= delay - p("m_fSoundAttackWarningTime", 1.5) {
                        t.warned = true;
                        sound(&mut sfx, "m_pSoundAttackWarning", lamp);
                    }
                    t.state = if s >= delay { State::Attack(0.0, p("m_VolleyShots", 3.0) as u32) } else { State::Alert(s) };
                    continue;
                }
                let State::Attack(_, left) = t.state else { continue };
                let wait = s - dt;
                if wait > 0.0 {
                    t.state = State::Attack(wait, left);
                    continue;
                }
                if t.lost > 0.0 {
                    // (holding fire while it can't see him)
                    t.state = State::Attack(0.2, left);
                    continue;
                }
                // a shot, spread about him
                let spread = p("m_fFireArrowMaxSpread", 200.0) * 0.01;
                let jitter = Vec3::new(rand_unit(), 0.0, rand_unit()) * spread;
                let speed = p("m_fProjectileSpeed", 2250.0) * 0.01;
                let g = crate::traps::GRAVITY * p("gravity", 1.0);
                let aim_at = if hacked { chest - Vec3::Y * 0.3 } else { pt.translation };
                let vel = crate::traps::aim(arrow_at, aim_at + jitter, speed, g);
                let arrow = commands.spawn((Arrow { tower: t.index, vel, gravity: g, life: 6.0 }, Transform::from_translation(arrow_at), Visibility::default(), DespawnOnExit(GameState::InGame))).id();
                if let Some(trail) = d.trail {
                    fx.write(SpawnEffect { follow: Some(arrow), system: Some(trail), secs: 6.0, ..SpawnEffect::at("", Vec3::ZERO) });
                }
                sound(&mut sfx, "fly", arrow_at);
                if std::env::var("DH_TOWER_LOG").is_ok() {
                    info!("watch tower {}: fires at {:.1}", d.actor, pt.translation);
                }
                // (each shot spends its tank's oil: `m_WatchtowerChargeCost`; the tank in its own
                // socket, or in the receptacle feeding it)
                if let Some(a) = t.tank.or_else(|| devices.feed_seat(&d.actor)) {
                    devices.drains.push((a, 3));
                }
                t.state = if left > 1 {
                    State::Attack(p("m_fVolleyDelayBetweenShots", 0.2), left - 1)
                } else {
                    State::Attack(p("m_fDelayBetweenVolleys", 4.0), p("m_VolleyShots", 3.0) as u32)
                };
            }
        }
        // the beam drawn: from the lamp to where it falls
        if let Some((mut ct, _)) = t.cone.and_then(|c| cones.get_mut(c).ok()) {
            let dir = beam_dir(t.yaw, t.pitch);
            ct.translation = lamp;
            ct.rotation = Quat::from_rotation_arc(Vec3::NEG_Y, dir);
            ct.scale = Vec3::new(spot / t.cone_size.x.max(0.01), hit / t.cone_size.y, spot / t.cone_size.z.max(0.01));
        }
    }
    if blind > 0.0 && std::env::var("DH_TOWER_LOG").is_ok() {
        info!("watch tower: blinding {blind:.2}");
    }
    // (eyes adjust)
    blinding.0 = if blind > blinding.0 { blind } else { (blinding.0 - dt * 0.8).max(blind) };
}

/// A pseudo-random number in -1..1 (the shots' spread).
fn rand_unit() -> f32 {
    use std::sync::atomic::{AtomicU32, Ordering};
    static SEED: AtomicU32 = AtomicU32::new(0x9e37_79b9);
    let mut x = SEED.load(Ordering::Relaxed);
    x ^= x << 13;
    x ^= x >> 17;
    x ^= x << 5;
    SEED.store(x, Ordering::Relaxed);
    (x as f32 / u32::MAX as f32) * 2.0 - 1.0
}

#[cfg(test)]
mod projectile_tests {
    use super::*;

    #[test]
    fn tower_decisions_obey_world_time_and_solid_cover() {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, AssetPlugin::default(), bevy::scene::ScenePlugin, TransformPlugin, RapierPhysicsPlugin::<NoUserData>::default()))
            .init_resource::<Assets<Mesh>>().init_resource::<crate::gameplay::TimeControl>()
            .init_resource::<PlayerStats>().init_resource::<Blinding>().init_resource::<crate::security::Devices>()
            .add_message::<PostEvent>().add_message::<SpawnEffect>()
            .insert_resource(bevy::time::TimeUpdateStrategy::ManualDuration(std::time::Duration::from_millis(100)))
            .insert_resource(LevelInfo { scene: dhcook::format::Scene { security: vec![dhcook::format::Security::default()], ..default() } })
            .add_systems(Last, towers);
        app.world_mut().spawn((Transform::default(), Player {
            velocity: Vec3::ZERO, yaw: 0.0, pitch: 0.0, crouched: false, sprinting: false,
            grounded: true, lean: 0.0, noclip: false, eye_height: 1.0, locked: false,
            air_time: 0.0, spawn: Vec3::ZERO, mantle: None, step_timer: 0.0,
            fall_speed: 0.0, power_jump: 0.0, pull: Vec3::ZERO,
        }));
        let tower = app.world_mut().spawn(Tower {
            index: 0, origin: Vec3::new(0.0, 10.0, 0.0), arrow: Vec3::new(0.0, 9.0, 0.0),
            tank: None, receptacle: None, tank_found: true, yaw: 0.0, pitch: 0.0,
            sweep: 0.0, state: State::Attack(0.0, 3), lost: 0.0, warned: false,
            cone: None, cone_size: Vec3::ONE, pivot: Vec3::ZERO, yaw0: 0.0, head: vec![],
        }).id();
        app.world_mut().resource_mut::<crate::gameplay::TimeControl>().bend_remaining = 10.0;
        for _ in 0..3 { app.update(); }
        let t = app.world().get::<Tower>(tower).unwrap();
        assert_eq!((t.state, t.yaw, t.pitch, t.lost), (State::Attack(0.0, 3), 0.0, 0.0, 0.0));
        let mut arrows = app.world_mut().query::<&Arrow>();
        assert_eq!(arrows.iter(app.world()).count(), 0);
        app.world_mut().resource_mut::<crate::gameplay::TimeControl>().bend_remaining = 0.0;
        app.update();
        assert_eq!(arrows.iter(app.world()).count(), 1, "due shot fires on resume");
        assert_eq!(app.world().get::<Tower>(tower).unwrap().state, State::Attack(0.2, 2));
        app.world_mut().resource_mut::<crate::gameplay::TimeControl>().bend_remaining = 10.0;
        app.update();
        assert_eq!(arrows.iter(app.world()).count(), 1, "stopping again must not consume volley shots");
        app.world_mut().get_mut::<Tower>(tower).unwrap().state = State::Explore;
        app.world_mut().resource_mut::<crate::gameplay::TimeControl>().world_dilation = 0.25;
        app.update();
        let t = app.world().get::<Tower>(tower).unwrap();
        assert!((t.sweep - 0.025).abs() < 1.0e-6);
        assert!((t.yaw - 20.0_f32.to_radians() * 0.025).abs() < 1.0e-6);
        app.world_mut().resource_mut::<crate::gameplay::TimeControl>().bend_remaining = 0.0;
        let cover = app.world_mut().spawn((Collider::cuboid(2.0, 0.1, 2.0), Transform::from_xyz(0.0, 5.0, 0.0),
            CollisionGroups::new(GROUP_PROP, Group::ALL))).id();
        for group in [GROUP_PROP, GROUP_WORLD] {
            app.world_mut().entity_mut(cover).insert(CollisionGroups::new(group, Group::ALL));
            let mut t = app.world_mut().get_mut::<Tower>(tower).unwrap();
            t.state = State::Explore;
            t.pitch = 0.0;
            t.yaw = 0.0;
            app.update();
            assert_eq!(app.world().get::<Tower>(tower).unwrap().state, State::Explore, "solid cover hides the player");
        }
        app.world_mut().entity_mut(cover).insert(Sensor);
        let mut t = app.world_mut().get_mut::<Tower>(tower).unwrap();
        t.pitch = 0.0;
        t.yaw = 0.0;
        app.update();
        assert_eq!(app.world().get::<Tower>(tower).unwrap().state, State::Alert(0.0), "trigger volumes must not hide the player");
    }

    #[test]
    fn arrows_choose_nearest_contact_and_follow_player_stance() {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, AssetPlugin::default(), bevy::scene::ScenePlugin, TransformPlugin, RapierPhysicsPlugin::<NoUserData>::default()))
            .init_resource::<Assets<Mesh>>().init_resource::<crate::gameplay::TimeControl>()
            .init_resource::<crate::settings::Settings>().add_message::<crate::gadgets::Explosion>()
            .insert_resource(bevy::time::TimeUpdateStrategy::ManualDuration(std::time::Duration::from_millis(100)))
            .insert_resource(LevelInfo { scene: dhcook::format::Scene { security: vec![dhcook::format::Security {
                blast: Some(dhcook::format::TrapBlast::default()), ..default()
            }], ..default() } }).add_systems(Last, fly_arrows);
        let player = app.world_mut().spawn((Transform::from_xyz(2.0, 0.0, 0.0), Player {
            velocity: Vec3::ZERO, yaw: 0.0, pitch: 0.0, crouched: false, sprinting: false,
            grounded: true, lean: 0.0, noclip: false, eye_height: 1.0, locked: false,
            air_time: 0.0, spawn: Vec3::ZERO, mantle: None, step_timer: 0.0,
            fall_speed: 0.0, power_jump: 0.0, pull: Vec3::ZERO,
        })).id();
        let cover = app.world_mut().spawn((Collider::cuboid(0.01, 2.0, 2.0), Transform::from_xyz(4.0, 0.0, 0.0), CollisionGroups::new(GROUP_WORLD, Group::ALL))).id();
        app.update();
        let fire = |app: &mut App, y| {
            app.world_mut().resource_mut::<Messages<crate::gadgets::Explosion>>().clear();
            app.world_mut().spawn((Arrow { tower: 0, vel: Vec3::X * 50.0, gravity: 0.0, life: 6.0 }, Transform::from_xyz(0.0, y, 0.0)));
            app.update();
            let blasts = app.world().resource::<Messages<crate::gadgets::Explosion>>();
            let mut cursor = blasts.get_cursor();
            cursor.read(blasts).next().map(|b| b.at)
        };
        let hit = fire(&mut app, 0.0).unwrap();
        assert!((hit.x - 1.56).abs() < 0.001, "nearer player must intercept before farther wall: {hit:?}");
        app.world_mut().get_mut::<Transform>(cover).unwrap().translation.x = 1.0;
        for group in [GROUP_WORLD, GROUP_PROP] {
            app.world_mut().entity_mut(cover).insert(CollisionGroups::new(group, Group::ALL));
            let hit = fire(&mut app, 0.0).unwrap();
            assert!((hit.x - 0.89).abs() < 0.001, "nearer cover must intercept before player: {hit:?}");
        }
        app.world_mut().entity_mut(cover).insert(Sensor);
        assert!(fire(&mut app, 0.6).is_some());
        app.world_mut().get_mut::<Player>(player).unwrap().crouched = true;
        app.world_mut().get_mut::<Transform>(player).unwrap().translation.y = -0.4;
        assert!(fire(&mut app, 0.6).is_none(), "head-height arrow clears a crouching player");
        assert!(fire(&mut app, -0.4).is_some());
    }
}

/// The towers' arrows: they burst on whatever they strike.
#[allow(clippy::too_many_arguments)]
fn fly_arrows(
    mut commands: Commands,
    time: Res<Time>,
    tc: Res<crate::gameplay::TimeControl>,
    rapier: ReadRapierContext,
    level: Option<Res<LevelInfo>>,
    settings: Res<crate::settings::Settings>,
    mut arrows: Query<(Entity, &mut Arrow, &mut Transform)>,
    player: Query<(&Transform, &Player), Without<Arrow>>,
    npcs: Query<(&crate::npc::Npc, &Transform), Without<Arrow>>,
    mut blasts: MessageWriter<crate::gadgets::Explosion>,
) {
    let Some(level) = level else { return };
    let Ok(ctx) = rapier.single() else { return };
    let dt = time.delta_secs() * tc.world_scale();
    let pl = player.single().ok();
    let walls = QueryFilter::default().exclude_sensors().groups(CollisionGroups::new(Group::ALL, GROUP_WORLD | GROUP_PROP));
    for (e, mut a, mut t) in &mut arrows {
        a.life -= dt;
        a.vel.y -= a.gravity * dt;
        let step = a.vel * dt;
        let len = step.length();
        let from = t.translation;
        let mut at = None;
        if len > 0.0 {
            let direction = step / len;
            let mut nearest = len;
            if let Some((_, h)) = ctx.cast_ray_and_get_normal(from, direction, len, true, walls) {
                nearest = h.time_of_impact;
                at = Some(from + direction * nearest + h.normal * 0.1);
            }
            let mut character = |center, half, radius| {
                if let Some(distance) = crate::krust::capsule_ray_hit(from, direction, nearest, center, half, radius)
                    .filter(|d| at.is_none() || *d < nearest) {
                    nearest = distance;
                    at = Some(from + direction * distance);
                }
            };
            for (n, t) in &npcs {
                if !n.is_down() { character(t.translation, crate::npc::NPC_HALF, crate::npc::NPC_RADIUS + 0.12); }
            }
            if let Some((t, p)) = pl {
                let half = if p.crouched { crate::player::CROUCH_HALF } else { crate::player::STAND_HALF };
                character(t.translation, half, crate::player::RADIUS + 0.12);
            }
            if len > 0.01 {
                t.rotation = Quat::from_rotation_arc(Vec3::NEG_Z, step / len);
            }
        }
        match at {
            Some(at) => {
                if let Some(b) = level.scene.security.get(a.tower).and_then(|d| d.blast.as_ref()) {
                    let damage = b.damage[settings.difficulty.min(3) as usize];
                    blasts.write(crate::gadgets::Explosion { at, radius: b.radius, full: b.full, damage, effect: "grenade", player: Some([b.player_radius, b.player_full]), kind: crate::gameplay::HitKind::Explosion });
                }
                commands.entity(e).despawn();
            }
            None if a.life <= 0.0 => commands.entity(e).despawn(),
            None => t.translation = from + step,
        }
    }
}
