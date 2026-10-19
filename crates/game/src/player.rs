//! First-person player (Corvo) controller.

use crate::footsteps::{Footfall, Gait, Walker};
use crate::gameplay::{Noise, NoiseKind, PlayerStats};
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
        app.init_resource::<Slide>().init_resource::<AutoCrouch>().init_resource::<MantleEdge>().init_resource::<HardLanding>().init_resource::<LeanView>();
        app.init_resource::<CameraShake>().add_message::<ShakeCamera>();
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
/// What pulls Corvo down (m/s²): the world's 1500 uu/s², doubled as the original's falling
/// physics applies it (its 800 uu/s `JumpZ` rises 106.7 uu and lands after 0.533 s, as measured
/// in roshan's r614/blink research).
pub const GRAVITY: f32 = 30.0;
pub const WALK_SPEED: f32 = 3.4;
pub const SPRINT_SPEED: f32 = 6.2;
pub const CROUCH_SPEED: f32 = 1.9;
/// The jump's speed without the attributes (`JumpZ` 800 uu/s).
pub const JUMP_SPEED: f32 = 8.0;

/// A slide under way (crouching from a sprint): since it began (s), which way, how fast at
/// first (m/s).
#[derive(Resource, Default)]
pub struct Slide(pub Option<(f32, Vec3, f32)>);

/// The slide (`StatePlayerMasterSlide`, as roshan's r614/blink research measured it): from a
/// sprint of at least 400 uu/s, the speed eases to the sneaking one over 1.1 s, along the way
/// it began; it ends there, or when the way turns more than 25 degrees; let go after 0.55 s.
const SLIDE_TIME: f32 = 1.1;
const SLIDE_MIN_SPEED: f32 = 4.0;
const SLIDE_CANCEL: f32 = 0.55;

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
    /// the speed cap last frame (a slowing blends in: `m_fSpeedBlendDownSpeed`)
    pub speed_cap: f32,
    /// Agility: seconds the held jump keeps pushing up
    pub power_jump: f32,
    /// dragged along (an assassin's Attract Spell), m/s
    pub pull: Vec3,
}

#[derive(Component)]
pub struct PlayerCamera;

/// A UE3 field of view (horizontal, degrees: `m_fDefaultFOV`, the options' FOV) as Bevy takes
/// it (vertical, radians) at an aspect ratio. UE3 holds the horizontal angle (no aspect
/// constraint), so a wider screen sees less above and below.
pub fn ue_fov(horizontal_deg: f32, aspect: f32) -> f32 {
    2.0 * ((horizontal_deg.to_radians() * 0.5).tan() / aspect.max(0.1)).atan()
}

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
                fall_speed: 0.0, air_peak: 0.0, speed_cap: 0.0,
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
                    fov: ue_fov(75.0, 16.0 / 9.0),
                    near: 0.05,
                    // sky domes are scaled out to ~20 km
                    far: 60000.0,
                    ..default()
                }),
                Exposure { ev100: 6.5 },
                Tonemapping::TonyMcMapface,
                // (its frame HDR, finished gamma-encoded for the interface to blend over in
                // gamma space: `gammaout`)
                bevy::camera::Hdr,
                bevy::camera::CompositingSpace::Srgb,
                crate::gammaout::GammaOut,
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
    choice: Option<Res<crate::choice::Choice>>,
) {
    // (a choice up keeps the cursor for its options)
    if paused.0 || stats.dead || choice.is_some_and(|c| c.pending.is_some()) {
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
    fn friction_divides_by_the_strength_and_never_quickens() {
        let m = Vec2::new(0.4, 0.5);
        assert_eq!(friction_scale(m, 100.0), m);
        assert!((friction_scale(m, 50.0) - Vec2::new(0.8, 1.0)).length() < 1e-6);
        assert_eq!(friction_scale(m, 20.0), Vec2::ONE);
        assert_eq!(friction_scale(m, 0.0), Vec2::ONE);
    }

    #[test]
    fn a_jolt_shakes_the_view_and_settles() {
        let mut s = CameraShake::default();
        s.jolt(200.0);
        // (a few centimetres at most: 2 m/s against the spring's 34.6 rad/s)
        let mut peak: f32 = 0.0;
        for _ in 0..30 {
            s.step(1.0 / 60.0);
            peak = peak.max(s.offset.length());
        }
        assert!(peak > 0.02 && peak < 0.07, "{peak}");
        for _ in 0..120 {
            s.step(1.0 / 60.0);
        }
        assert_eq!(s.offset, Vec3::ZERO);
    }

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
            air_time: 0.0, spawn: Vec3::ZERO, mantle: None, step_timer: 0.0, fall_speed: 0.0, air_peak: 0.0, speed_cap: 0.0,
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

/// A lean's hold: the side held first, the side leant to, the time left before it lets go.
#[derive(Default)]
pub struct LeanHold {
    side: f32,
    held: f32,
    release: f32,
}

/// Where the look was as a lean began: it stays within `LEAN_LOOK` of it while leaning
/// (`StatePlayerMasterLeaning`'s view limits).
#[derive(Resource, Default)]
pub struct LeanView(pub Option<(f32, f32)>);

/// The lean's break speed (`m_fBreakLean_Speed_PC` 200 uu/s, m/s), its release
/// (`m_fLeanReleaseTime`, s), and the look's limits while leaning (yaw, pitch: 60 and 35 degrees).
const LEAN_BREAK_SPEED: f32 = 2.0;
const LEAN_RELEASE: f32 = 0.2;
const LEAN_LOOK: (f32, f32) = (1.0471976, 0.6108652);

/// The original's default mouse sensitivity (`DefaultInput.ini` `MouseSensitivity`), the
/// options' 2.0, and the look's limits while peeking and sprinting.
const MOUSE_SENSITIVITY: f32 = 13.0;
const LIMIT_MOUSE_KEYHOLE: f32 = 7.0;
const LIMIT_MOUSE_SPRINT: f32 = 10.0;
const LIMIT_PAD_KEYHOLE: f32 = 0.5;
const LIMIT_PAD_SPRINT: f32 = 0.85;

/// The aim friction's slowing of the turn and the look up: its multipliers over the strength
/// (the option / 100), never quicker than none.
fn friction_scale(mult: Vec2, strength: f32) -> Vec2 {
    let k = strength / 100.0;
    if k <= 0.0 {
        return Vec2::ONE;
    }
    (mult / k).min(Vec2::ONE)
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
    pad: Option<Res<crate::pad::Pad>>,
    (peek, game_settings): (Option<Res<crate::keyhole::Peek>>, Option<Res<crate::settings::Settings>>),
    (data, over): (Option<Res<crate::gamedata::Data>>, Option<Res<crate::crosshair::OverEnemy>>),
    mut slowed: Local<bool>,
    lean_view: Option<Res<LeanView>>,
) {
    let Ok((mut p, mut t)) = q.single_mut() else { *last = Vec2::ZERO; return };
    if p.is_added() { *last = Vec2::ZERO; }
    // the look's sensitivity held down while peeking through a keyhole or sprinting
    // (`DisTweaks_PlayerInput`: `m_fLimitedMouseSensitivity_Keyhole` 7 and `_Sprint` 10 against
    // the mouse's 13, the pad's `m_fLimitedJoySensitivity_Keyhole` 0.5 and `_Sprint` 0.85, as
    // roshan's r614/blink research found them limiting it)
    let peeking = peek.as_ref().is_some_and(|k| k.at.is_some());
    let limit = if peeking { Some((LIMIT_MOUSE_KEYHOLE, LIMIT_PAD_KEYHOLE)) } else if p.sprinting { Some((LIMIT_MOUSE_SPRINT, LIMIT_PAD_SPRINT)) } else { None };
    let mouse_cap = limit.map_or(1.0, |(m, _)| (m / (MOUSE_SENSITIVITY * settings.sensitivity / 0.002).max(1e-3)).min(1.0));
    let pad_cap = limit.map_or(1.0, |(_, j)| {
        let user = game_settings.as_ref().map_or(1.0, |s| crate::pad::look_scale(s.pad_look_x));
        (j / user.max(1e-3)).min(1.0)
    });
    // the aim's friction, as `DishonoredPlayerInput`'s tick does it (0x00ABDD90, decoded for this
    // rewrite): with the option on for the device (the menu's "Aim Assist", `PSI_Mouse_bFriction`
    // off by default, the pad's on), zoomed if `m_bUseAimFriction_Zoomed`, else if
    // `m_bUseAimFriction_NonZoomed` or the device's auto aim is off, over a hostile character the
    // turn and the look up are multiplied by `m_fFrictionMult_Horiz` and `_Vert` over the strength
    // (stored as the option / 100 by its settings listener, 0x00ABD610). The original's division
    // would quicken the look under 40 and divide by zero at 0; it stops here at no slowing.
    let friction = |pad: bool| -> Vec2 {
        let (Some(s), Some(data), Some(over)) = (game_settings.as_ref(), data.as_ref(), over.as_ref()) else { return Vec2::ONE };
        let (on, strength, auto_aim) = if pad { (s.pad_friction, s.pad_friction_strength, s.pad_auto_aim) } else { (s.mouse_friction, s.mouse_friction_strength, s.mouse_auto_aim) };
        let flag = |k: &str| data.pawn(k, 0.0) > 0.0;
        let used = if zoom.level > 0 { flag("input.m_bUseAimFriction_Zoomed") } else { flag("input.m_bUseAimFriction_NonZoomed") || !auto_aim };
        if !on || !used || !over.0 {
            return Vec2::ONE;
        }
        friction_scale(Vec2::new(data.pawn("input.m_fFrictionMult_Horiz", 0.5), data.pawn("input.m_fFrictionMult_Vert", 0.5)), strength)
    };
    // the wheel takes the mouse while it's open; the lens slows the aim
    if !paused.0 && cursor.grab_mode != CursorGrabMode::None && scripted.is_none() && !wheel.open {
        let raw = motion.delta;
        let d = if settings.smoothing { (raw + *last) * 0.5 } else { raw };
        *last = if settings.smoothing { raw } else { Vec2::ZERO };
        let sens = settings.sensitivity / zoom.factor.max(1.0) * mouse_cap;
        let f = friction(false);
        p.yaw -= d.x * sens * f.x;
        let dy = if settings.invert_y { -d.y } else { d.y };
        p.pitch = (p.pitch - dy * sens * f.y).clamp(-1.5, 1.5);
    } else {
        // Menu/wheel motion belongs to the UI. Do not replay the last gameplay
        // delta through the smoothing filter when mouse-look resumes.
        *last = Vec2::ZERO;
    }
    // the pad's right stick (`pad`: its turn and look this frame), the lens slowing it too
    let pad_look = pad.as_ref().map_or(Vec2::ZERO, |p| p.look);
    if !paused.0 && !wheel.open && pad_look != Vec2::ZERO {
        let k = pad_cap / zoom.factor.max(1.0);
        let f = friction(true);
        if std::env::var("DH_AIM_LOG").is_ok() && (f != Vec2::ONE) != *slowed {
            *slowed = f != Vec2::ONE;
            info!("aim friction: the pad's look x{f:.2}");
        }
        p.yaw += pad_look.x * k * f.x;
        p.pitch = (p.pitch + pad_look.y * k * f.y).clamp(-1.5, 1.5);
    }
    // (leaning: about where the look was as it began)
    if let Some((y0, p0)) = lean_view.as_ref().and_then(|l| l.0) {
        let dy = (p.yaw - y0 + std::f32::consts::PI).rem_euclid(std::f32::consts::TAU) - std::f32::consts::PI;
        p.yaw = y0 + dy.clamp(-LEAN_LOOK.0, LEAN_LOOK.0);
        p.pitch = p.pitch.clamp(p0 - LEAN_LOOK.1, p0 + LEAN_LOOK.1);
    }
    t.rotation = Quat::from_rotation_y(p.yaw);
    if let Ok(mut ct) = cam.single_mut() {
        // (a shot's kick on top: `aim`)
        ct.rotation = Quat::from_rotation_z(-p.lean * 0.18) * Quat::from_rotation_x(p.pitch + aim.view_kick());
    }
}

#[allow(clippy::too_many_arguments)]
/// How the fall under way began: sneaking (`m_bIsStealthFall`, from the sneak at the fall's
/// start), and whether from a jump.
#[derive(Default)]
struct FallStart {
    sneaking: bool,
    jumped: bool,
    /// set at the fall's start, cleared on the ground
    airborne: bool,
}

/// A fall not from a jump lasting no longer than this is no landing (`m_fAnimateFallTime`).
const ANIMATE_FALL_TIME: f32 = 0.25;

fn player_move(
    time: Res<Time>,
    (keys, bind): (Res<ButtonInput<KeyCode>>, Res<crate::bindings::Bindings>),
    rapier: ReadRapierContext,
    level: Option<Res<LevelInfo>>,
    mut vinput: ResMut<VirtualInput>,
    mut stats: ResMut<PlayerStats>,
    (mut noise, mut steps): (MessageWriter<Noise>, MessageWriter<Footfall>),
    (attrs, zone): (Res<crate::gamedata::Attrs>, Res<crate::zones::ZonePush>),
    (swim, waters, carry, possession): (Res<crate::swim::Swim>, Option<Res<crate::swim::Waters>>, Res<crate::carry::Carry>, Res<crate::possession::Possession>),
    (mut climb, climbables, mut sfx): (ResMut<crate::climb::Climb>, Option<Res<crate::climb::Climbables>>, MessageWriter<crate::audio::PostEvent>),
    (cine, settings, paused, tc, mut slide, mut auto, mut hard, mut lean_view): (
        Res<crate::script_world::Cinematic>,
        Res<crate::settings::Settings>,
        Res<Paused>,
        Res<crate::gameplay::TimeControl>,
        ResMut<Slide>,
        ResMut<AutoCrouch>,
        ResMut<HardLanding>,
        ResMut<LeanView>,
    ),
    pad: Option<Res<crate::pad::Pad>>,
    mut q: Query<(
        Entity,
        &mut Player,
        &mut Transform,
        &mut KinematicCharacterController,
        &mut Collider,
        Option<&KinematicCharacterControllerOutput>,
    )>,
    mut fall: Local<FallStart>,
    mut lean_hold: Local<LeanHold>,
) {
    let Ok((entity, mut p, mut t, mut kcc, mut col, out)) = q.single_mut() else { return };
    if paused.0 {
        kcc.translation = None;
        return;
    }
    let dt = time.delta_secs().min(0.05);
    hard.0 = (hard.0 - dt).max(0.0);
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
    // the pad's left stick: as far as it is pushed (`pad`)
    let pad_walk = pad.as_ref().map_or(Vec2::ZERO, |p| p.walk);
    let analog = pad_walk != Vec2::ZERO;
    wish += fwd * pad_walk.y + right * pad_walk.x;
    // the scripts' cinematics hold Corvo where he is
    let held = cine.holds_movement();
    let wish = if held { Vec3::ZERO } else if analog { wish.clamp_length_max(1.0) } else { wish.normalize_or_zero() };
    let jump_pressed = !held && (keys.just_pressed(bind.key(crate::bindings::Act::Jump)) || std::mem::take(&mut vinput.jump));
    // (held in the air with the world while Daud's Blink is aimed)
    let dt = if wish == Vec3::ZERO && !jump_pressed { dt * tc.aim_freeze.clamp(0.0, 1.0) } else { dt };
    // (his own pace through Bend Time: `m_PlayerDilation`)
    let dt = dt * tc.player_rate();

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
    // knocked down (a wolfhound's jump attack): down for his knockdown's moment
    if stats.knocked > 0.0 {
        stats.knocked = (stats.knocked - dt).max(0.0);
        p.velocity = Vec3::ZERO;
        kcc.translation = None;
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
        // off an edge (not a jump: its own start is kept): how the fall began, on its first
        // frame in the air
        if !p.grounded && !fall.airborne {
            *fall = FallStart { sneaking: p.crouched, jumped: false, airborne: true };
        } else if p.grounded && was_grounded {
            fall.airborne = false;
        }
        if p.grounded && !was_grounded {
            // landing: noise and fall damage (by the height fallen)
            let v = p.fall_speed;
            // a hard one (`m_Action_JumpLandBig`): he stops sneaking (`m_bAutomaticallyStopSneaking`:
            // up once there's room) and his left hand is taken by it (`m_bActionUsesLeftHand`) until
            // its clip's exit, at his landing rate; he moves on as before
            if v > FALL_STUN {
                hard.0 = LAND_BIG_EXIT / attrs.land_rate.max(0.1);
                if p.crouched {
                    auto.0 = true;
                }
            }
            // a landing (as roshan's r614/blink research read the original's): none at all for
            // a fall not from a jump that lasted no more than `m_fAnimateFallTime` (0.25 s: a
            // step down); harder than `m_fFall_Stun_Vel` the stun landing (`JumpLand_Big`, 8 m),
            // else a fall begun sneaking lands silently (`JumpLand_Stealth`), else `JumpLand`
            // (4 m), each heard as an anomaly
            let fell = v * v / (2.0 * GRAVITY);
            let landing = fall.jumped || p.air_time > ANIMATE_FALL_TIME;
            fall.airborne = false;
            if landing {
                let gait = if fell > 2.1 { Gait::LandHigh } else { Gait::LandSmall };
                steps.write(Footfall { pos: t.translation, gait, who: Walker::Player, by: Some(entity) });
                // the original ignores footfalls for a moment after landing
                p.step_timer = 0.8;
                let radius = if v > FALL_STUN { Some(8.0) } else if fall.sneaking { None } else { Some(4.0) };
                if std::env::var_os("DH_NOISE_LOG").is_some() {
                    info!("landing: {v:.1} m/s after {:.2} s (jumped {}, sneaking {}): heard {radius:?} m", p.air_time, fall.jumped, fall.sneaking);
                }
                if let Some(radius) = radius {
                    noise.write(Noise { corvo: true, pos: t.translation, radius, kind: NoiseKind::Anomaly });
                }
            }
            // the original's: none up to `MaxSpeedBeforeFallingDamage`, all of his health at
            // `MaxSpeedBeforeFallingDeath`, in between in proportion (whole points; Agility
            // raises both)
            let (hurt, death) = (attrs.fall_hurt, attrs.fall_death.max(attrs.fall_hurt + 0.1));
            if v > hurt && !stats.fall_damage_off {
                let dmg = (stats.max_health * ((v - hurt) / (death - hurt)).clamp(0.0, 1.0)).trunc();
                stats.take_damage(dmg);
                stats.damage_flash = 1.0;
                if stats.health <= 0.0 {
                    stats.dead = true;
                    stats.died_falling = true;
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
    let room = |t: &Transform| {
        rapier.single().ok().is_none_or(|ctx| {
            ctx.cast_ray(t.translation, Vec3::Y, STAND_HALF + RADIUS + 0.45, true, QueryFilter::default().exclude_collider(entity).groups(player_groups())).is_none()
        })
    };
    let toggled = keys.just_pressed(bind.key(crate::bindings::Act::Crouch)) && host.is_none();
    if toggled {
        if p.crouched && !auto.0 {
            // need headroom to stand; without it he stays down until there is (auto-crouched)
            if room(&t) {
                p.crouched = false;
                *col = Collider::capsule_y(STAND_HALF, RADIUS);
                t.translation.y += STAND_HALF - CROUCH_HALF;
            } else {
                auto.0 = true;
            }
        } else if p.crouched {
            // (auto-crouched, he means to sneak after all)
            auto.0 = false;
        } else {
            // (from a sprint: a slide)
            let horiz = p.velocity.with_y(0.0);
            if p.sprinting && p.grounded && horiz.length() >= SLIDE_MIN_SPEED && !carry.carrying() {
                slide.0 = Some((0.0, horiz.normalize(), horiz.length()));
            }
            p.crouched = true;
            auto.0 = false;
            *col = Collider::capsule_y(CROUCH_HALF, RADIUS);
            t.translation.y -= STAND_HALF - CROUCH_HALF;
        }
    }
    // auto-crouched, he stands by himself once there is room
    if !p.crouched {
        auto.0 = false;
    } else if auto.0 && !toggled && host.is_none() && room(&t) {
        info!("player: room to stand again: stands up");
        auto.0 = false;
        p.crouched = false;
        *col = Collider::capsule_y(STAND_HALF, RADIUS);
        t.translation.y += STAND_HALF - CROUCH_HALF;
    }
    // a slide ends at its time, when its way turns (a wall), when let go late enough, off the
    // ground, or stood up
    if let Some((st, way, _)) = slide.0 {
        let moved = out.map(|o| o.effective_translation.with_y(0.0)).unwrap_or(Vec3::ZERO);
        let turned = moved.length() > 1e-3 && moved.normalize().dot(way) < 25f32.to_radians().cos();
        if st >= SLIDE_TIME || turned || (st >= SLIDE_CANCEL && wish == Vec3::ZERO) || !p.grounded || !p.crouched || host.is_some() {
            slide.0 = None;
        }
    }

    // (sprinting in any direction, as the original allows)
    p.sprinting = keys.pressed(bind.key(crate::bindings::Act::Sprint)) && !p.crouched && wish != Vec3::ZERO && !carry.carrying() && host.is_none();
    // sideways or backwards (the original's: within 10 degrees of sideways going forward, 50
    // going back; else within 40 of straight back)
    let (ahead, side) = (wish.dot(fwd), wish.dot(right));
    let mag = wish.length().max(1e-4);
    let off_side = (side.abs() / mag).clamp(0.0, 1.0).acos().to_degrees();
    let off_back = (ahead.abs() / mag).clamp(0.0, 1.0).acos().to_degrees();
    let strafing = wish != Vec3::ZERO && off_side < if ahead >= 0.0 { 10.0 } else { 50.0 };
    let backwards = !strafing && ahead < 0.0 && off_back < 40.0;
    // the original's ground speeds (Agility 2 sprints faster; `GroundSpeedCarryingCorpse`),
    // sideways and backwards at their multipliers; a creature's (`m_fGroundSpeedWhilePossessed`)
    // (bitten by rats: 40 % slower for a second)
    stats.slowed = (stats.slowed - dt).max(0.0);
    let bitten = if stats.slowed > 0.0 { 0.6 } else { 1.0 };
    let speed = bitten * (if let Some(b) = host {
        b.ground
    } else if carry.carrying() {
        attrs.carry.min(if p.crouched { attrs.crouch } else { attrs.walk })
    } else if p.crouched {
        if strafing { attrs.walk * attrs.strafe[2] } else { attrs.crouch }
    } else if p.sprinting {
        attrs.sprint * if strafing { attrs.strafe[1] } else if backwards { attrs.backward[1] } else { 1.0 }
    } else {
        attrs.walk * if strafing { attrs.strafe[0] } else if backwards { attrs.backward[0] } else { 1.0 }
    });
    let weapon_factor = if host.is_none() && !stats.unarmed && !stats.sheathed { attrs.weapon_speed } else { 1.0 };
    // (a slowing blends in at `m_fSpeedBlendDownSpeed`, a quickening at once)
    let cap = speed * weapon_factor;
    p.speed_cap = if cap < p.speed_cap { p.speed_cap + (cap - p.speed_cap) * (dt * attrs.speed_blend_down).clamp(0.0, 1.0) } else { cap };
    let target = wish * p.speed_cap;
    let accel = if p.grounded { 14.0 } else { 3.0 };
    let horiz = Vec3::new(p.velocity.x, 0.0, p.velocity.z);
    let mut new_h = horiz + (target - horiz) * (accel * dt).min(1.0);
    if let Some((st, way, v0)) = slide.0.as_mut() {
        // v0 + (crouch - v0)(1 - cos(pi t / 1.1)) / 2
        *st += dt;
        let to = attrs.crouch * weapon_factor;
        let v = *v0 + (to - *v0) * (1.0 - (std::f32::consts::PI * (*st / SLIDE_TIME).min(1.0)).cos()) * 0.5;
        new_h = *way * v;
        p.speed_cap = p.speed_cap.max(v);
    }
    p.velocity.x = new_h.x;
    p.velocity.z = new_h.z;

    if p.grounded {
        p.velocity.y = -1.0;
        if jump_pressed && host.is_none() {
            if p.crouched {
                // stand up first
            } else {
                p.velocity.y = if carry.carrying() { attrs.jump_carrying } else { attrs.jump };
                p.grounded = false;
                *fall = FallStart { sneaking: p.crouched, jumped: true, airborne: true };
                // (the jump button's time held while rising, for Agility's push)
                p.power_jump = 0.0;
            }
        }
    } else {
        // Agility: Jump held through the rise (more than 0.05 s) gives, at the top, one push up of
        // `JumpZ_PowerJump` (`FixedNormalJump_FixedPowerJump`, as roshan's r614/blink research
        // decoded it); `power_jump` the time held, below 0 spent
        if p.power_jump >= 0.0 {
            let held = keys.pressed(bind.key(crate::bindings::Act::Jump));
            if !held {
                p.power_jump = 0.0;
            } else if p.velocity.y > 1e-4 {
                p.power_jump += dt;
            }
            if p.velocity.y <= 1e-4 {
                if held && p.power_jump > 0.05 && attrs.power_jump_max > 1e-4 {
                    p.velocity.y = attrs.power_jump_max;
                }
                p.power_jump = -1.0;
            }
        }
        p.velocity.y -= GRAVITY * dt;
        p.velocity.y = p.velocity.y.max(-40.0);
    }

    // lean (Q/E), held: asked for only while his whole velocity (falling too) is under
    // `m_fBreakLean_Speed_PC` (200 uu/s), so walking lets it go and it comes back once he slows
    // with the key still down; the side held first wins (left if both went down together); let
    // go, it holds `m_fLeanReleaseTime` (0.2 s) before it eases back (the input's tick
    // 0x00AB3FF0 and `StatePlayerMasterLeaning` 0x00A76590, decoded for this rewrite; no lean
    // forward on PC as shipped)
    let (left, right) = (keys.pressed(bind.key(crate::bindings::Act::LeanLeft)), keys.pressed(bind.key(crate::bindings::Act::LeanRight)));
    lean_hold.side = match (left, right) {
        (false, false) => 0.0,
        (true, false) => -1.0,
        (false, true) => 1.0,
        (true, true) if lean_hold.side == 0.0 => -1.0,
        (true, true) => lean_hold.side,
    };
    let asked = if p.velocity.length() < LEAN_BREAK_SPEED { lean_hold.side } else { 0.0 };
    if asked != 0.0 {
        lean_hold.release = LEAN_RELEASE;
        lean_hold.held = asked;
    } else {
        lean_hold.release = (lean_hold.release - dt).max(0.0);
    }
    let lean_target = if lean_hold.release > 0.0 { lean_hold.held } else { 0.0 };
    p.lean += (lean_target - p.lean) * (10.0 * dt).min(1.0);
    // (the look held about where it was as the lean began)
    if lean_target != 0.0 {
        if lean_view.0.is_none() {
            lean_view.0 = Some((p.yaw, p.pitch));
        }
    } else if p.lean.abs() < 0.02 {
        lean_view.0 = None;
    }

    // a physics volume's push (`ZoneVelocity`): all of it falling, a little walking
    let push = if p.grounded { zone.0.with_y(0.0) * 25.0 * dt } else { zone.0 };
    kcc.translation = Some((p.velocity + p.pull + push) * dt);
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
            steps.write(Footfall { pos: t.translation, gait, who: Walker::Player, by: Some(entity) });
            let radius = if p.crouched || (p.sprinting && attrs.silent_sprinting) || (!p.sprinting && attrs.silent_running) {
                0.0
            } else if p.sprinting {
                12.0
            } else {
                6.0
            };
            if radius > 0.0 {
                noise.write(Noise { corvo: true, pos: t.translation, radius, kind: NoiseKind::Anomaly });
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

/// The mantle under way: its edge's height above his feet (m), and whether only a crouch fits
/// on top (the finder's crouch flag, from a standing box).
#[derive(Resource, Default)]
pub struct MantleEdge {
    pub h: f32,
    pub crouch: bool,
}

/// The original's mantles (`DishonoredMantleEdgeFinderComponent` and `StatePlayerMasterMantle`, as
/// roshan's r614/blink research decoded them): edges up to `m_MantleMaxEdgeHeight` 230 uu, at least
/// 55 (155 falling faster than 1000 uu/s, none faster than 2000); up to 120 an instant step-up,
/// to 170 a 0.700 s climb, higher 1.033 s (0.633 for a low one into a crouch).
const MANTLE_MAX: f32 = 2.3;
const MANTLE_STEP_UP: f32 = 1.2;

/// Crouched by the world rather than by choice (the original's auto-crouch, `bonus_autocrouching`
/// to his visibility): after a mantle into a space too low to stand, or trying to stand without
/// room; he stands by himself once there is room.
#[derive(Resource, Default)]
pub struct AutoCrouch(pub bool);

/// A hard landing's time left (s): his left hand busy with it.
#[derive(Resource, Default)]
pub struct HardLanding(pub f32);

/// A landing harder than this is a hard one (m/s: `StatePlayerMasterFalling.m_fFall_Stun_Vel`
/// 2200 uu/s, as roshan's r614/blink research decoded the landing's actions).
pub const FALL_STUN: f32 = 22.0;
/// The hard landing's clip leaves its state at this (s: `Generic_JumpLandBig`'s
/// `DishonoredNotify_AnimStateExit`).
const LAND_BIG_EXIT: f32 = 0.935;

fn mantle_time(h: f32, crouch: bool) -> f32 {
    if h <= MANTLE_STEP_UP {
        if crouch { 0.633 } else { 0.0 }
    } else if h <= 1.7 {
        0.700
    } else {
        1.033
    }
}

/// Where a mantle from `t` facing `yaw` would put Corvo (standing, or `crouched` sneaking: he
/// stays down), the ledge's height above his feet, and whether only a crouch fits there (the
/// finder's crouch flag, from a standing box).
pub(crate) fn mantle_dest(ctx: &bevy_rapier3d::prelude::RapierContext, e: Entity, t: &Transform, yaw: f32, fall: f32, crouched: bool) -> Option<(Vec3, f32, bool)> {
    let fwd = Quat::from_rotation_y(yaw) * Vec3::NEG_Z;
    let feet = t.translation - Vec3::Y * (if crouched { CROUCH_HALF } else { STAND_HALF } + RADIUS);
    let filter = QueryFilter::default().exclude_collider(e).exclude_sensors().groups(CollisionGroups::new(Group::ALL, GROUP_WORLD | GROUP_PROP));
    // wall in front at waist height?
    let (_, wall) = ctx.cast_ray_and_get_normal(feet + Vec3::Y * 0.7, fwd, RADIUS + 0.6, true, filter)?;
    if wall.normal.y.abs() > 0.5 {
        return None;
    }
    // ledge top within reach (a top no steeper than 45 degrees)
    let probe = wall.point + fwd * 0.35;
    let probe = Vec3::new(probe.x, feet.y + MANTLE_MAX, probe.z);
    let (top_e, top) = ctx.cast_ray_and_get_normal(probe, Vec3::NEG_Y, MANTLE_MAX, true, filter)?;
    let h = top.point.y - feet.y;
    let least = if fall > 20.0 {
        return None;
    } else if fall > 10.0 {
        1.55
    } else {
        0.55
    };
    if top.normal.y < 0.7 || !(least..=MANTLE_MAX).contains(&h) {
        return None;
    }
    // room for him at the destination: standing, else crouched
    let fits = |half: f32| {
        let at = top.point + Vec3::Y * (half + RADIUS + 0.05) + fwd * 0.2;
        // (held a little clear of the top's own bumps)
        let body = Collider::capsule_y(half, RADIUS * 0.9);
        let mut blocked = false;
        ctx.intersect_shape(at + Vec3::Y * 0.12, Quat::IDENTITY, &*body.raw, filter, |hit| {
            if std::env::var("DH_MANTLE_LOG").is_ok() {
                let c = ctx.colliders.entity2collider().get(&hit).and_then(|h| ctx.colliders.colliders.get(*h));
                info!("mantle fit: {hit:?} blocks {at:.2} (edge {h:.2}, half {half}, ledge's own {}, top n {:.2}): {:?} {:?}", hit == top_e, top.normal, c.map(|c| c.shape().shape_type()), c.map(|c| c.compute_aabb()));
            }
            blocked = true;
            false
        });
        (!blocked).then_some(at)
    };
    if crouched {
        return fits(CROUCH_HALF).map(|at| (at, h, fits(STAND_HALF).is_none()));
    }
    if let Some(at) = fits(STAND_HALF) {
        return Some((at, h, false));
    }
    fits(CROUCH_HALF).map(|at| (at, h, true))
}

fn mantle(
    time: Res<Time>,
    paused: Res<Paused>,
    (keys, bind): (Res<ButtonInput<KeyCode>>, Res<crate::bindings::Bindings>),
    rapier: ReadRapierContext,
    stats: Res<PlayerStats>,
    attrs: Res<crate::gamedata::Attrs>,
    mut q: Query<(Entity, &mut Player, &mut Transform, &mut Collider), Without<PlayerCamera>>,
    (mut spot, mut auto, mut edge): (ResMut<MantleSpot>, ResMut<AutoCrouch>, ResMut<MantleEdge>),
) {
    spot.0 = false;
    if paused.0 { return; }
    let Ok((e, mut p, mut t, mut col)) = q.single_mut() else { return };
    if let Some((from, to, k)) = p.mantle {
        // Acrobat modifies MantleAnimRate; the head's additive mantle clip uses
        // this same progress, keeping its motion in step with the faster climb.
        let dur = mantle_time(edge.h, edge.crouch).max(0.3);
        let nk = (k + time.delta_secs() * attrs.mantle_rate.max(0.01) / dur).min(1.0);
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
    if p.locked || p.noclip || stats.dead {
        return;
    }
    let Ok(ctx) = rapier.single() else { return };
    let fall = if p.grounded { 0.0 } else { (-p.velocity.y).max(0.0) };
    let found = mantle_dest(&ctx, e, &t, p.yaw, fall, p.crouched);
    spot.0 = found.is_some_and(|(_, h, _)| h > 0.9);
    // (the Jump press, or Jump held while falling: no forward needed)
    let wants = keys.just_pressed(bind.key(crate::bindings::Act::Jump)) || (!p.grounded && keys.pressed(bind.key(crate::bindings::Act::Jump)));
    if !wants {
        return;
    }
    let Some((dest, h, crouch)) = found else { return };
    *edge = MantleEdge { h, crouch };
    info!("mantle: {h:.2} m edge, {:.3} s{}", mantle_time(h, crouch), if crouch { ", crouched" } else { "" });
    if crouch {
        // (into a space too low to stand: crouched by it, not by choice)
        auto.0 |= !p.crouched;
        p.crouched = true;
        *col = Collider::capsule_y(CROUCH_HALF, RADIUS);
    }
    if mantle_time(h, crouch) <= 0.0 {
        // a low edge: stepped onto at once
        t.translation = dest;
        p.velocity = Vec3::ZERO;
        p.grounded = true;
        return;
    }
    p.mantle = Some((t.translation, dest, 0.0));
    p.locked = true;
    p.velocity = Vec3::ZERO;
}

#[allow(clippy::too_many_arguments)]
fn update_camera_offset(
    time: Res<Time>,
    rapier: ReadRapierContext,
    (mut shake, mut jolts): (ResMut<CameraShake>, MessageReader<ShakeCamera>),
    powers: Res<Powers>,
    stats: Res<PlayerStats>,
    mut q: Query<(Entity, &mut Player, &Transform), Without<PlayerCamera>>,
    mut cam: Query<(&mut Transform, &mut Projection), With<PlayerCamera>>,
    (swim, settings, zoom, possession): (Res<crate::swim::Swim>, Res<crate::settings::Settings>, Res<crate::zoom::Zoom>, Res<crate::possession::Possession>),
    mut world_light: Option<ResMut<crate::world_light::WorldLighting>>,
    death_fall: Res<crate::arms::DeathFall>,
) {
    let Ok((entity, mut p, t)) = q.single_mut() else { return };
    let Ok((mut ct, mut proj)) = cam.single_mut() else { return };
    if let Projection::Perspective(pp) = proj.as_mut() {
        // the options' field of view (horizontal, as UE3's), widened by Blink's kick, narrowed by
        // the mask's lens; the arms keep the unzoomed one (the zoom's `m_bLockArms`)
        let base = ue_fov(settings.fov.clamp(55.0, 110.0) + 14.0 * powers.fov_kick, pp.aspect_ratio);
        pp.fov = 2.0 * ((base * 0.5).tan() / zoom.factor.max(1.0)).atan();
        if let Some(wl) = world_light.as_mut() {
            let arms = 1.0 / (base * 0.5).tan();
            if (wl.ue_entries.get(2).map_or(0.0, |e| e[0]) - arms).abs() > 1e-4 {
                if let Some(e) = wl.ue_entries.get_mut(2) {
                    e[0] = arms;
                    wl.dirty = true;
                }
            }
        }
    }
    let dt = time.delta_secs();
    // (dead, the view sinks to the floor, unless his death's clip carries it down: `arms`)
    let target_eye = if stats.dead && death_fall.0 {
        p.eye_height
    } else if stats.dead {
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
    // the lean: the head swung about a pivot below it (`m_fLeanHeightPct` 2 x the cylinder's
    // height: 350 uu standing), to `m_fMaxLeanAngle` 15 degrees and its soften 5 (as roshan's
    // r614/blink research decoded `DishonoredCamera_Lean`), as far as the walls let it
    let pivot = if p.crouched { 2.6 } else { 3.5 };
    let mut lean_off = 0.0;
    if p.lean.abs() > 0.01 {
        let dir = t.rotation * Vec3::X * p.lean.signum();
        let eye = t.translation + Vec3::Y * p.eye_height;
        let max = pivot * (20f32.to_radians() * p.lean.abs()).sin();
        let hit = rapier.single().ok().and_then(|ctx| {
            ctx.cast_ray(eye, dir, max + 0.15, true, QueryFilter::default().exclude_collider(entity).groups(player_groups()))
        });
        let allowed = hit.map(|(_, toi)| (toi - 0.15).max(0.0)).unwrap_or(max);
        lean_off = allowed.min(max) * p.lean.signum();
    }
    let swing = (lean_off.abs() / pivot).clamp(0.0, 1.0).asin();
    // (its shake on top)
    for j in jolts.read() {
        if std::env::var("DH_BOB_LOG").is_ok() {
            info!("shake: a jolt of {:.0} uu/s", j.0);
        }
        shake.jolt(j.0);
    }
    shake.step(dt);
    ct.translation = Vec3::new(lean_off, p.eye_height - pivot * (1.0 - swing.cos()), 0.0) + shake.offset;
}

/// A jolt to the view (uu/s: the shake's magnitude, `m_fCamShake_OnHitEnv`...).
#[derive(Message, Clone, Copy)]
pub struct ShakeCamera(pub f32);

/// The view's shake (`DishonoredCamera_Shake`): the eye's place on a spring
/// (`m_StabilityConstants`: springiness 1200, damping 12), each jolt a push of its magnitude (uu/s)
/// in a direction at random (our reading of the shake's input).
#[derive(Resource, Default)]
pub struct CameraShake {
    pub offset: Vec3,
    velocity: Vec3,
}

/// The shake's spring (`DishonoredCamera_Shake.m_StabilityConstants`).
const SHAKE_SPRING: f32 = 1200.0;
const SHAKE_DAMPING: f32 = 12.0;
/// Unreal units to metres.
const SHAKE_UU: f32 = 0.01;

impl CameraShake {
    pub fn jolt(&mut self, magnitude: f32) {
        let dir = Vec3::new(rand::random::<f32>() - 0.5, rand::random::<f32>() - 0.5, rand::random::<f32>() - 0.5).normalize_or(Vec3::Y);
        self.velocity += dir * magnitude * SHAKE_UU;
    }

    /// The spring's step (a few small ones, so a long frame keeps it steady).
    pub fn step(&mut self, dt: f32) {
        if self.offset == Vec3::ZERO && self.velocity == Vec3::ZERO {
            return;
        }
        let n = 4;
        let h = dt.min(0.1) / n as f32;
        for _ in 0..n {
            let a = -SHAKE_SPRING * self.offset - SHAKE_DAMPING * self.velocity;
            self.velocity += a * h;
            self.offset += self.velocity * h;
        }
        if self.offset.length() < 1e-4 && self.velocity.length() < 1e-3 {
            *self = CameraShake::default();
        }
    }
}
