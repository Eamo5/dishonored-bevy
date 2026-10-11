//! First-person player (Corvo) controller.

use crate::footsteps::{Footfall, Gait, Walker};
use crate::gameplay::{HudMessages, Noise, PlayerStats};
use crate::hud::Paused;
use crate::level::{LevelInfo, LevelSpawnSet, GROUP_NPC, GROUP_PLAYER, GROUP_PROP, GROUP_WORLD};
use crate::powers::Powers;
use crate::{Config, GameState};
use bevy::camera::Exposure;
use bevy::core_pipeline::tonemapping::Tonemapping;
use bevy::input::mouse::AccumulatedMouseMotion;
use bevy::pbr::{DistanceFog, FogFalloff};
use bevy::prelude::*;
use bevy::window::{CursorGrabMode, CursorOptions};
use bevy_rapier3d::prelude::*;

pub struct PlayerPlugin;

impl Plugin for PlayerPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<VirtualInput>()
            .init_resource::<MantleSpot>()
            .add_systems(OnEnter(GameState::InGame), spawn_player.after(LevelSpawnSet))
            .add_systems(
                Update,
                (cursor_grab, player_look, crate::swim::water_state, player_move, mantle, update_camera_offset)
                    .chain()
                    .in_set(PlayerMoveSet)
                    .run_if(in_state(GameState::InGame)),
            );
    }
}

/// The player's controller (look, water, move, climb, camera).
#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub struct PlayerMoveSet;

pub const RADIUS: f32 = 0.32;
pub const STAND_HALF: f32 = 0.58; // capsule half segment => total 1.80 m
pub const CROUCH_HALF: f32 = 0.18; // total 1.0 m
pub const STAND_EYE: f32 = 0.72; // above capsule centre
pub const CROUCH_EYE: f32 = 0.3;
pub const GRAVITY: f32 = 19.0;
pub const WALK_SPEED: f32 = 3.4;
pub const SPRINT_SPEED: f32 = 6.2;
pub const CROUCH_SPEED: f32 = 1.9;
pub const JUMP_SPEED: f32 = 5.6;

#[derive(Component)]
pub struct Player {
    pub velocity: Vec3,
    pub yaw: f32,
    pub pitch: f32,
    pub crouched: bool,
    pub sprinting: bool,
    pub grounded: bool,
    pub lean: f32,
    pub noclip: bool,
    pub eye_height: f32,
    /// Set while a scripted move (mantle, blink) is in progress.
    pub locked: bool,
    pub air_time: f32,
    pub spawn: Vec3,
    pub mantle: Option<(Vec3, Vec3, f32)>,
    pub step_timer: f32,
    pub fall_speed: f32,
    /// the highest point since leaving the ground (a fall's height)
    pub air_peak: f32,
    /// Agility: seconds the held jump keeps pushing up
    pub power_jump: f32,
    /// dragged along (an assassin's Attract Spell), m/s
    pub pull: Vec3,
}

#[derive(Component)]
pub struct PlayerCamera;

/// Scripted input used by automated tests (and future gamepad/cutscene control).
#[derive(Resource, Default)]
pub struct VirtualInput {
    pub forward: f32,
    pub jump: bool,
}

#[derive(Resource)]
pub struct MouseSettings {
    pub sensitivity: f32,
    pub invert_y: bool,
    /// averaged over two frames (`PSI_Mouse_bSmooting`)
    pub smoothing: bool,
}

impl Default for MouseSettings {
    fn default() -> Self {
        MouseSettings { sensitivity: 0.002, invert_y: false, smoothing: false }
    }
}

pub fn player_groups() -> CollisionGroups {
    CollisionGroups::new(GROUP_PLAYER, GROUP_WORLD | GROUP_NPC | GROUP_PROP)
}

pub fn spawn_player(mut commands: Commands, level: Option<Res<LevelInfo>>, config: Res<Config>) {
    let (pos, yaw) = match level.as_ref().and_then(|l| {
        crate::level::choose_start(&l.scene, config.spawn_index).and_then(|i| l.scene.player_starts.get(i)).cloned()
    }) {
        Some(ps) => (Vec3::from(ps.position) + Vec3::Y * 0.15, ps.yaw),
        None => (Vec3::new(0.0, 2.0, 0.0), 0.0),
    };
    info!("spawning player at {pos} yaw {yaw}");
    // the map's DisFog: linear distance fog with the authored colour and maximum opacity
    let fog = match level.as_ref().and_then(|l| l.scene.fog.clone()) {
        Some(f) => DistanceFog {
            color: Color::linear_rgba(f.color[0], f.color[1], f.color[2], f.density.max(0.05)),
            falloff: FogFalloff::Linear { start: f.start, end: f.end },
            ..default()
        },
        None => DistanceFog {
            color: Color::srgba(0.18, 0.2, 0.22, 1.0),
            falloff: FogFalloff::Linear { start: 60.0, end: 420.0 },
            ..default()
        },
    };
    commands
        .spawn((
            Player {
                velocity: Vec3::ZERO,
                yaw,
                pitch: 0.0,
                crouched: false,
                sprinting: false,
                grounded: false,
                lean: 0.0,
                noclip: false,
                eye_height: STAND_EYE,
                locked: false,
                air_time: 0.0,
                spawn: pos,
                mantle: None,
                step_timer: 0.0,
                fall_speed: 0.0, air_peak: 0.0,
                power_jump: 0.0,
                pull: Vec3::ZERO,
            },
            DespawnOnExit(GameState::InGame),
            Transform::from_translation(pos).with_rotation(Quat::from_rotation_y(yaw)),
            Visibility::default(),
            Collider::capsule_y(STAND_HALF, RADIUS),
            player_groups(),
            KinematicCharacterController {
                up: Vec3::Y,
                offset: CharacterLength::Absolute(0.03),
                slide: true,
                autostep: Some(CharacterAutostep {
                    max_height: CharacterLength::Absolute(0.42),
                    min_width: CharacterLength::Absolute(0.08),
                    include_dynamic_bodies: false,
                }),
                max_slope_climb_angle: 52f32.to_radians(),
                min_slope_slide_angle: 60f32.to_radians(),
                snap_to_ground: Some(CharacterLength::Absolute(0.35)),
                apply_impulse_to_dynamic_bodies: false,
                filter_groups: Some(player_groups()),
                ..default()
            },
        ))
        .with_children(|p| {
            p.spawn((
                PlayerCamera,
                Camera3d::default(),
                Projection::from(PerspectiveProjection {
                    fov: 75f32.to_radians(),
                    near: 0.05,
                    // sky domes are scaled out to ~20 km
                    far: 60000.0,
                    ..default()
                }),
                Exposure { ev100: 6.5 },
                Tonemapping::TonyMcMapface,
                // scene depth for the original translucent shaders (depth fading)
                bevy::core_pipeline::prepass::DepthPrepass,
                fog,
                Transform::from_xyz(0.0, STAND_EYE, 0.0),
            ));
        });
}

fn cursor_grab(
    mut cursor: Single<&mut CursorOptions>,
    mouse: Res<ButtonInput<MouseButton>>,
    keys: Res<ButtonInput<KeyCode>>,
    paused: Res<Paused>,
    stats: Res<PlayerStats>,
) {
    if paused.0 || stats.dead {
        return;
    }
    if mouse.just_pressed(MouseButton::Left) && cursor.grab_mode == CursorGrabMode::None {
        cursor.visible = false;
        cursor.grab_mode = CursorGrabMode::Locked;
    }
    let _ = keys;
}

#[cfg(test)]
mod look_tests {
    use super::*;

    #[test]
    fn smoothing_does_not_replay_motion_after_wheel_or_cursor_release() {
        let mut app = App::new();
        app.init_resource::<AccumulatedMouseMotion>()
            .insert_resource(MouseSettings { smoothing: true, ..default() })
            .init_resource::<crate::wheel::Wheel>()
            .init_resource::<crate::zoom::Zoom>()
            .init_resource::<crate::aim::Aim>()
            .init_resource::<Paused>()
            .add_systems(Update, player_look);
        let cursor = app.world_mut().spawn(CursorOptions { grab_mode: CursorGrabMode::Locked, ..default() }).id();
        let player = app.world_mut().spawn((Player {
            velocity: Vec3::ZERO, yaw: 0.0, pitch: 0.0, crouched: false, sprinting: false,
            grounded: false, lean: 0.0, noclip: false, eye_height: STAND_EYE, locked: false,
            air_time: 0.0, spawn: Vec3::ZERO, mantle: None, step_timer: 0.0, fall_speed: 0.0, air_peak: 0.0,
            power_jump: 0.0, pull: Vec3::ZERO,
        }, Transform::IDENTITY)).id();
        for mode in 0..3 {
            app.world_mut().resource_mut::<AccumulatedMouseMotion>().delta = Vec2::new(100.0, 50.0);
            app.update();
            let before = app.world().get::<Player>(player).map(|p| (p.yaw, p.pitch)).unwrap();
            if mode == 0 { app.world_mut().resource_mut::<crate::wheel::Wheel>().open = true; }
            else if mode == 1 { app.world_mut().get_mut::<CursorOptions>(cursor).unwrap().grab_mode = CursorGrabMode::None; }
            else { app.world_mut().resource_mut::<Paused>().0 = true; }
            app.world_mut().resource_mut::<AccumulatedMouseMotion>().delta = Vec2::splat(500.0);
            app.update();
            assert_eq!(app.world().get::<Player>(player).map(|p| (p.yaw, p.pitch)).unwrap(), before);
            app.world_mut().resource_mut::<crate::wheel::Wheel>().open = false;
            app.world_mut().resource_mut::<Paused>().0 = false;
            app.world_mut().get_mut::<CursorOptions>(cursor).unwrap().grab_mode = CursorGrabMode::Locked;
            app.world_mut().resource_mut::<AccumulatedMouseMotion>().delta = Vec2::ZERO;
            app.update();
            assert_eq!(app.world().get::<Player>(player).map(|p| (p.yaw, p.pitch)).unwrap(), before);
        }
        app.world_mut().resource_mut::<MouseSettings>().smoothing = false;
        app.world_mut().resource_mut::<AccumulatedMouseMotion>().delta = Vec2::splat(100.0);
        app.update();
        let before = app.world().get::<Player>(player).map(|p| (p.yaw, p.pitch)).unwrap();
        app.world_mut().resource_mut::<MouseSettings>().smoothing = true;
        app.world_mut().resource_mut::<AccumulatedMouseMotion>().delta = Vec2::ZERO;
        app.update();
        assert_eq!(app.world().get::<Player>(player).map(|p| (p.yaw, p.pitch)).unwrap(), before);

        app.world_mut().resource_mut::<AccumulatedMouseMotion>().delta = Vec2::splat(100.0);
        app.update();
        let mut replacement = app.world_mut().entity_mut(player).take::<Player>().unwrap();
        app.world_mut().despawn(player);
        replacement.yaw = 0.0;
        replacement.pitch = 0.0;
        let next_player = app.world_mut().spawn((replacement, Transform::IDENTITY)).id();
        app.world_mut().resource_mut::<AccumulatedMouseMotion>().delta = Vec2::ZERO;
        app.update();
        assert_eq!(app.world().get::<Player>(next_player).map(|p| (p.yaw, p.pitch)).unwrap(), (0.0, 0.0));
    }
}

fn player_look(
    motion: Res<AccumulatedMouseMotion>,
    cursor: Single<&CursorOptions>,
    scripted: Option<Res<crate::script::Scripted>>,
    settings: Res<MouseSettings>,
    mut q: Query<(&mut Player, &mut Transform), Without<PlayerCamera>>,
    mut cam: Query<&mut Transform, With<PlayerCamera>>,
    (wheel, zoom, aim, paused): (Res<crate::wheel::Wheel>, Res<crate::zoom::Zoom>, Res<crate::aim::Aim>, Res<Paused>),
    mut last: Local<Vec2>,
) {
    let Ok((mut p, mut t)) = q.single_mut() else { *last = Vec2::ZERO; return };
    if p.is_added() { *last = Vec2::ZERO; }
    // the wheel takes the mouse while it's open; the lens slows the aim
    if !paused.0 && cursor.grab_mode != CursorGrabMode::None && scripted.is_none() && !wheel.open {
        let raw = motion.delta;
        let d = if settings.smoothing { (raw + *last) * 0.5 } else { raw };
        *last = if settings.smoothing { raw } else { Vec2::ZERO };
        let sens = settings.sensitivity / zoom.factor.max(1.0);
        p.yaw -= d.x * sens;
        let dy = if settings.invert_y { -d.y } else { d.y };
        p.pitch = (p.pitch - dy * sens).clamp(-1.5, 1.5);
    } else {
        // Menu/wheel motion belongs to the UI. Do not replay the last gameplay
        // delta through the smoothing filter when mouse-look resumes.
        *last = Vec2::ZERO;
    }
    t.rotation = Quat::from_rotation_y(p.yaw);
    if let Ok(mut ct) = cam.single_mut() {
        // (a shot's kick on top: `aim`)
        ct.rotation = Quat::from_rotation_z(-p.lean * 0.18) * Quat::from_rotation_x(p.pitch + aim.view_kick());
    }
}

#[allow(clippy::too_many_arguments)]
fn player_move(
    time: Res<Time>,
    (keys, bind): (Res<ButtonInput<KeyCode>>, Res<crate::bindings::Bindings>),
    rapier: ReadRapierContext,
    level: Option<Res<LevelInfo>>,
    mut vinput: ResMut<VirtualInput>,
    mut stats: ResMut<PlayerStats>,
    (mut noise, mut steps): (MessageWriter<Noise>, MessageWriter<Footfall>),
    (mut msgs, attrs): (ResMut<HudMessages>, Res<crate::gamedata::Attrs>),
    (swim, waters, carry, possession): (Res<crate::swim::Swim>, Option<Res<crate::swim::Waters>>, Res<crate::carry::Carry>, Res<crate::possession::Possession>),
    (mut climb, climbables, mut sfx): (ResMut<crate::climb::Climb>, Option<Res<crate::climb::Climbables>>, MessageWriter<crate::audio::PostEvent>),
    (cine, settings, paused): (Res<crate::script_world::Cinematic>, Res<crate::settings::Settings>, Res<Paused>),
    mut q: Query<(
        Entity,
        &mut Player,
        &mut Transform,
        &mut KinematicCharacterController,
        &mut Collider,
        Option<&KinematicCharacterControllerOutput>,
    )>,
) {
    let Ok((entity, mut p, mut t, mut kcc, mut col, out)) = q.single_mut() else { return };
    if paused.0 {
        kcc.translation = None;
        return;
    }
    let dt = time.delta_secs().min(0.05);
    if keys.just_pressed(KeyCode::KeyV) {
        p.noclip = !p.noclip;
        info!("noclip {}", p.noclip);
    }
    let fwd = Quat::from_rotation_y(p.yaw) * Vec3::NEG_Z;
    let right = Quat::from_rotation_y(p.yaw) * Vec3::X;
    let mut wish = Vec3::ZERO;
    if keys.pressed(bind.key(crate::bindings::Act::Forward)) {
        wish += fwd;
    }
    if keys.pressed(bind.key(crate::bindings::Act::Back)) {
        wish -= fwd;
    }
    if keys.pressed(bind.key(crate::bindings::Act::Right)) {
        wish += right;
    }
    if keys.pressed(bind.key(crate::bindings::Act::Left)) {
        wish -= right;
    }
    wish += fwd * vinput.forward;
    // the scripts' cinematics hold Corvo where he is
    let held = cine.holds_movement();
    let wish = if held { Vec3::ZERO } else { wish.normalize_or_zero() };
    let jump_pressed = !held && (keys.just_pressed(bind.key(crate::bindings::Act::Jump)) || std::mem::take(&mut vinput.jump));

    if p.noclip {
        let look = Quat::from_rotation_y(p.yaw) * Quat::from_rotation_x(p.pitch) * Vec3::NEG_Z;
        let mut v = Vec3::ZERO;
        if keys.pressed(bind.key(crate::bindings::Act::Forward)) {
            v += look;
        }
        if keys.pressed(bind.key(crate::bindings::Act::Back)) {
            v -= look;
        }
        v += wish.dot(right) * right;
        if keys.pressed(bind.key(crate::bindings::Act::Jump)) {
            v += Vec3::Y;
        }
        if keys.pressed(bind.key(crate::bindings::Act::Block)) {
            v -= Vec3::Y;
        }
        let speed = if keys.pressed(bind.key(crate::bindings::Act::Sprint)) { 30.0 } else { 8.0 };
        t.translation += v * speed * dt;
        kcc.translation = None;
        p.velocity = Vec3::ZERO;
        return;
    }
    if p.locked {
        kcc.translation = None;
        return;
    }
    if stats.dead {
        p.velocity.x = 0.0;
        p.velocity.z = 0.0;
        p.velocity.y = if p.grounded { -1.0 } else { (p.velocity.y - GRAVITY * dt).max(-40.0) };
        kcc.translation = Some(p.velocity * dt);
        if let Some(o) = out {
            p.grounded = o.grounded;
        }
        return;
    }
    // inside a creature: a rooted one doesn't move; a fish swims where it looks, never out of
    // its water
    let host = possession.body;
    if let Some(body) = host {
        if body.rooted {
            kcc.translation = None;
            p.velocity = Vec3::ZERO;
            return;
        }
        if let (true, Some(waters)) = (body.fish, waters.as_deref()) {
            let look = Quat::from_rotation_y(p.yaw) * Quat::from_rotation_x(p.pitch) * Vec3::NEG_Z;
            let mut v = (look * wish.dot(fwd) + right * wish.dot(right)) * body.water;
            // not out through the surface, nor out of the water at all
            if waters.at(t.translation + v * dt + Vec3::Y * (body.radius + 0.05)).is_none() {
                v.y = v.y.min(0.0);
            }
            if waters.at(t.translation + v * dt * 4.0).is_none() {
                v = Vec3::ZERO;
            }
            let cur = p.velocity;
            p.velocity = cur + (v - cur) * (5.0 * dt).min(1.0);
            kcc.snap_to_ground = None;
            kcc.translation = Some(p.velocity * dt);
            p.grounded = false;
            return;
        }
    }
    // on a chain
    if let Some(chains) = climbables.as_deref() {
        if !carry.carrying() && crate::climb::climb_move(&mut climb, chains, &mut p, &mut t, &mut kcc, &keys, &bind, (vinput.forward, jump_pressed), dt, &mut sfx, settings.camera_relative_climbing) {
            return;
        }
    }
    // in the water
    if let (true, Some(waters)) = (swim.swimming(), waters.as_deref()) {
        crate::swim::swim_move(&mut p, &t, &mut kcc, &mut col, &keys, &bind, vinput.forward, &attrs, &swim, waters, dt, host);
        return;
    }
    if kcc.snap_to_ground.is_none() {
        kcc.snap_to_ground = Some(CharacterLength::Absolute(0.35));
    }

    if let Some(o) = out {
        let was_grounded = p.grounded;
        // still rising from a jump: at high frame rates the first steps up stay within the
        // controller's ground margin, which isn't landing
        p.grounded = o.grounded && !(p.velocity.y > 0.5 && p.air_time < 0.3 && !was_grounded);
        // a creature's small body: held up while falling is standing
        if host.is_some() && o.desired_translation.y < -1e-4 && o.effective_translation.y > o.desired_translation.y * 0.3 {
            p.grounded = true;
        }
        if p.grounded && !was_grounded {
            // landing: noise and fall damage
            let v = p.fall_speed;
            if v > 3.0 {
                let gait = if v > 9.0 { Gait::LandHigh } else { Gait::LandSmall };
                steps.write(Footfall { pos: t.translation, gait, who: Walker::Player });
                // the original ignores footfalls for a moment after landing
                p.step_timer = 0.8;
            }
            if v > 7.0 {
                noise.write(Noise { pos: t.translation, radius: (v * 0.9).min(14.0), combat: false });
            }
            // Agility raises the speeds a fall can be taken at
            let hurt = 13.0 * attrs.fall_damage;
            if v > hurt && !stats.fall_damage_off {
                let dmg = ((v - hurt) * 9.0).min(150.0);
                stats.take_damage(dmg);
                stats.damage_flash = 1.0;
                if stats.health <= 0.0 {
                    stats.dead = true;
                    msgs.push("You fell to your death");
                }
            }
        }
        // hit a ceiling while rising
        if p.velocity.y > 0.0 && o.effective_translation.y + 1e-4 < o.desired_translation.y * 0.5 {
            p.velocity.y = 0.0;
        }
    }
    if p.grounded {
        p.air_time = 0.0;
    } else {
        p.air_time += dt;
    }

    // crouch toggle (Ctrl / C); a creature doesn't
    if keys.just_pressed(bind.key(crate::bindings::Act::Crouch)) && host.is_none() {
        if p.crouched {
            // need headroom to stand
            let ctx = rapier.single();
            let blocked = ctx.ok().and_then(|ctx| {
                ctx.cast_ray(
                    t.translation,
                    Vec3::Y,
                    STAND_HALF + RADIUS + 0.45,
                    true,
                    QueryFilter::default().exclude_collider(entity).groups(player_groups()),
                )
            });
            if blocked.is_none() {
                p.crouched = false;
                *col = Collider::capsule_y(STAND_HALF, RADIUS);
                t.translation.y += STAND_HALF - CROUCH_HALF;
            }
        } else {
            p.crouched = true;
            *col = Collider::capsule_y(CROUCH_HALF, RADIUS);
            t.translation.y -= STAND_HALF - CROUCH_HALF;
        }
    }

    p.sprinting = keys.pressed(bind.key(crate::bindings::Act::Sprint)) && !p.crouched && wish.dot(fwd) > 0.3 && !carry.carrying() && host.is_none();
    // the original's ground speeds (Agility 2 sprints faster; `GroundSpeedCarryingCorpse`);
    // a creature's (`m_fGroundSpeedWhilePossessed`)
    let speed = if let Some(b) = host {
        b.ground
    } else if carry.carrying() {
        attrs.carry.min(if p.crouched { attrs.crouch } else { attrs.walk })
    } else if p.crouched {
        attrs.crouch
    } else if p.sprinting {
        attrs.sprint
    } else {
        attrs.walk
    };
    let weapon_factor = if host.is_none() && !stats.unarmed && !stats.sheathed { attrs.weapon_speed } else { 1.0 };
    let target = wish * speed * weapon_factor;
    let accel = if p.grounded { 14.0 } else { 3.0 };
    let horiz = Vec3::new(p.velocity.x, 0.0, p.velocity.z);
    let new_h = horiz + (target - horiz) * (accel * dt).min(1.0);
    p.velocity.x = new_h.x;
    p.velocity.z = new_h.z;

    if p.grounded {
        p.velocity.y = -1.0;
        if jump_pressed && host.is_none() {
            if p.crouched {
                // stand up first
            } else {
                p.velocity.y = JUMP_SPEED;
                p.grounded = false;
                p.power_jump = attrs.power_jump_time;
            }
        }
    } else {
        // Agility: holding jump carries Corvo higher
        if p.power_jump > 0.0 && attrs.power_jump_accel > 0.0 && keys.pressed(bind.key(crate::bindings::Act::Jump)) && p.velocity.y > 0.0 {
            p.power_jump -= dt;
            p.velocity.y = (p.velocity.y + attrs.power_jump_accel * dt).min(attrs.power_jump_max.max(JUMP_SPEED));
        } else {
            p.power_jump = 0.0;
        }
        p.velocity.y -= GRAVITY * dt;
        p.velocity.y = p.velocity.y.max(-40.0);
    }

    // lean (Q/E) only when stationary-ish, like the original
    let lean_target = if keys.pressed(bind.key(crate::bindings::Act::LeanLeft)) {
        -1.0
    } else if keys.pressed(bind.key(crate::bindings::Act::LeanRight)) {
        1.0
    } else {
        0.0
    };
    p.lean += (lean_target - p.lean) * (10.0 * dt).min(1.0);

    kcc.translation = Some((p.velocity + p.pull) * dt);
    if !p.grounded {
        p.fall_speed = (-p.velocity.y).max(0.0);
        p.air_peak = p.air_peak.max(t.translation.y);
    } else {
        p.air_peak = t.translation.y;
    }

    // footsteps the AI can hear
    let hspeed = Vec3::new(p.velocity.x, 0.0, p.velocity.z).length();
    if p.grounded && hspeed > 0.5 {
        p.step_timer -= dt;
        if p.step_timer <= 0.0 {
            p.step_timer = if p.sprinting { 0.32 } else if p.crouched { 0.6 } else { 0.5 };
            let gait = if p.crouched { Gait::Sneak } else if p.sprinting { Gait::Sprint } else { Gait::Walk };
            steps.write(Footfall { pos: t.translation, gait, who: Walker::Player });
            let radius = if p.crouched { 0.0 } else if p.sprinting { 9.0 } else { 3.0 };
            if radius > 0.0 {
                noise.write(Noise { pos: t.translation, radius, combat: false });
            }
        }
    }

    // fell out of the world
    let kill_y = level.as_ref().map(|l| l.scene.kill_y).unwrap_or(-100.0);
    if t.translation.y < kill_y {
        warn!("player fell below kill height, respawning");
        t.translation = p.spawn;
        p.velocity = Vec3::ZERO;
    }
}

/// Climb onto ledges in front of the player (Space while facing a ledge, or holding
/// forward against one in mid-air).
/// A ledge Corvo could climb onto now (the HUD's mantle prompt): above waist height (lower
/// ones he steps or jumps onto).
#[derive(Resource, Default)]
pub struct MantleSpot(pub bool);

/// Where a mantle from `t` facing `yaw` would put Corvo, and the ledge's height above his feet.
fn mantle_dest(ctx: &bevy_rapier3d::prelude::RapierContext, e: Entity, t: &Transform, yaw: f32) -> Option<(Vec3, f32)> {
    let fwd = Quat::from_rotation_y(yaw) * Vec3::NEG_Z;
    let feet = t.translation - Vec3::Y * (STAND_HALF + RADIUS);
    let filter = QueryFilter::default().exclude_collider(e).groups(CollisionGroups::new(Group::ALL, GROUP_WORLD | GROUP_PROP));
    // wall in front at waist height?
    let (_, wall) = ctx.cast_ray_and_get_normal(feet + Vec3::Y * 0.7, fwd, RADIUS + 0.6, true, filter)?;
    if wall.normal.y.abs() > 0.5 {
        return None;
    }
    // ledge top within reach
    let probe = wall.point + fwd * 0.35;
    let probe = Vec3::new(probe.x, feet.y + 2.3, probe.z);
    let (_, top) = ctx.cast_ray_and_get_normal(probe, Vec3::NEG_Y, 2.3, true, filter)?;
    let h = top.point.y - feet.y;
    if top.normal.y < 0.7 || !(0.45..=2.05).contains(&h) {
        return None;
    }
    // headroom at the destination
    if ctx.cast_ray(top.point + Vec3::Y * 0.05, Vec3::Y, 1.8, true, filter).is_some() {
        return None;
    }
    Some((top.point + Vec3::Y * (STAND_HALF + RADIUS + 0.05) + fwd * 0.2, h))
}

fn mantle(
    time: Res<Time>,
    paused: Res<Paused>,
    (keys, bind): (Res<ButtonInput<KeyCode>>, Res<crate::bindings::Bindings>),
    rapier: ReadRapierContext,
    stats: Res<PlayerStats>,
    attrs: Res<crate::gamedata::Attrs>,
    mut q: Query<(Entity, &mut Player, &mut Transform), Without<PlayerCamera>>,
    mut spot: ResMut<MantleSpot>,
) {
    spot.0 = false;
    if paused.0 { return; }
    let Ok((e, mut p, mut t)) = q.single_mut() else { return };
    if let Some((from, to, k)) = p.mantle {
        // Acrobat modifies MantleAnimRate; the head's additive mantle clip uses
        // this same progress, keeping its motion in step with the faster climb.
        let nk = (k + time.delta_secs() * attrs.mantle_rate.max(0.01) / 0.42).min(1.0);
        // up first, then forward
        let up = (nk / 0.6).min(1.0);
        let fwd = ((nk - 0.45) / 0.55).clamp(0.0, 1.0);
        let pos = Vec3::new(from.x + (to.x - from.x) * fwd, from.y + (to.y - from.y) * up, from.z + (to.z - from.z) * fwd);
        t.translation = pos;
        if nk >= 1.0 {
            p.mantle = None;
            p.locked = false;
            p.velocity = Vec3::ZERO;
        } else {
            p.mantle = Some((from, to, nk));
        }
        return;
    }
    if p.locked || p.noclip || stats.dead || p.crouched {
        return;
    }
    let Ok(ctx) = rapier.single() else { return };
    let found = mantle_dest(&ctx, e, &t, p.yaw);
    spot.0 = found.is_some_and(|(_, h)| h > 0.9);
    let wants = keys.just_pressed(bind.key(crate::bindings::Act::Jump)) || (!p.grounded && keys.pressed(bind.key(crate::bindings::Act::Forward)) && keys.pressed(bind.key(crate::bindings::Act::Jump)));
    if !wants {
        return;
    }
    let Some((dest, _)) = found else { return };
    p.mantle = Some((t.translation, dest, 0.0));
    p.locked = true;
    p.velocity = Vec3::ZERO;
}

fn update_camera_offset(
    time: Res<Time>,
    rapier: ReadRapierContext,
    powers: Res<Powers>,
    stats: Res<PlayerStats>,
    mut q: Query<(Entity, &mut Player, &Transform), Without<PlayerCamera>>,
    mut cam: Query<(&mut Transform, &mut Projection), With<PlayerCamera>>,
    (swim, settings, zoom, possession): (Res<crate::swim::Swim>, Res<crate::settings::Settings>, Res<crate::zoom::Zoom>, Res<crate::possession::Possession>),
) {
    let Ok((entity, mut p, t)) = q.single_mut() else { return };
    let Ok((mut ct, mut proj)) = cam.single_mut() else { return };
    if let Projection::Perspective(pp) = proj.as_mut() {
        // the options' field of view, widened by Blink's kick, narrowed by the mask's lens
        let base = (settings.fov.clamp(55.0, 110.0) + 14.0 * powers.fov_kick).to_radians();
        pp.fov = 2.0 * ((base * 0.5).tan() / zoom.factor.max(1.0)).atan();
    }
    let dt = time.delta_secs();
    let target_eye = if stats.dead {
        -0.55
    } else if let Some(b) = possession.body {
        b.eye
    } else if swim.swimming() {
        crate::swim::SWIM_EYE
    } else if p.crouched {
        CROUCH_EYE
    } else {
        STAND_EYE
    };
    p.eye_height += (target_eye - p.eye_height) * (12.0 * dt).min(1.0);
    // lean offset with collision check
    let mut lean_off = 0.0;
    if p.lean.abs() > 0.01 {
        let dir = t.rotation * Vec3::X * p.lean.signum();
        let eye = t.translation + Vec3::Y * p.eye_height;
        let max = 0.55 * p.lean.abs();
        let hit = rapier.single().ok().and_then(|ctx| {
            ctx.cast_ray(eye, dir, max + 0.15, true, QueryFilter::default().exclude_collider(entity).groups(player_groups()))
        });
        let allowed = hit.map(|(_, toi)| (toi - 0.15).max(0.0)).unwrap_or(max);
        lean_off = allowed.min(max) * p.lean.signum();
    }
    ct.translation = Vec3::new(lean_off, p.eye_height - (lean_off.abs() * 0.1), 0.0);
}
