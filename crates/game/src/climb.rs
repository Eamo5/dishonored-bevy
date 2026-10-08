//! Climbing the levels' hanging chains (`DisClimbable`, `DisTweaks_Climbable`): walk into one
//! (or jump onto it) to grab it, forward / back climb up and down it, jump lets go (a mantle
//! takes over at a ledge) and crouch drops off; the chain rattles as Corvo climbs
//! (`m_pClimbingSound`, `m_fClimbingSoundMin/MaxDelay`).

use crate::bindings::{Act, Bindings};
use crate::level::LevelInfo;
use crate::player::{Player, RADIUS, STAND_HALF};
use crate::GameState;
use bevy::prelude::*;
use bevy_rapier3d::prelude::*;

pub struct ClimbPlugin;

impl Plugin for ClimbPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Climb>().add_systems(OnEnter(GameState::InGame), build_climbables.after(crate::level::LevelSpawnSet));
    }
}

/// A chain: its axis (x, z) and the heights it spans.
struct Chain {
    /// its level instance (the scripts' `DisSeqEvent_Climb`)
    instance: u32,
    axis: Vec2,
    top: f32,
    bottom: f32,
}

#[derive(Resource, Default)]
pub struct Climbables(Vec<Chain>);

impl Climbables {
    /// A chain's level instance.
    pub fn instance(&self, i: usize) -> Option<u32> {
        self.0.get(i).map(|c| c.instance)
    }
}

/// The chain Corvo hangs on.
#[derive(Resource, Default)]
pub struct Climb {
    pub on: Option<usize>,
    sound_t: f32,
    /// a moment after letting go before grabbing again
    cooldown: f32,
}

const CLIMB_SPEED: f32 = 2.2;
const REACH: f32 = 0.9;

fn build_climbables(mut commands: Commands, level: Option<Res<LevelInfo>>, mut climb: ResMut<Climb>) {
    *climb = Climb::default();
    let mut out = Vec::new();
    if let Some(level) = level {
        for (ii, inst) in level.scene.instances.iter().enumerate().filter(|(_, i)| i.class == "DisClimbable") {
            let Some(mesh) = level.scene.meshes.get(inst.mesh as usize) else { continue };
            let m = Mat4::from_cols_array(&inst.transform);
            let (mut lo, mut hi) = (Vec3::INFINITY, Vec3::NEG_INFINITY);
            for i in 0..8 {
                let c = Vec3::new(
                    if i & 1 == 0 { mesh.min[0] } else { mesh.max[0] },
                    if i & 2 == 0 { mesh.min[1] } else { mesh.max[1] },
                    if i & 4 == 0 { mesh.min[2] } else { mesh.max[2] },
                );
                let w = m.transform_point3(c);
                lo = lo.min(w);
                hi = hi.max(w);
            }
            // chains hang: tall and thin
            if hi.y - lo.y < 1.0 {
                continue;
            }
            out.push(Chain { instance: ii as u32, axis: Vec2::new((lo.x + hi.x) * 0.5, (lo.z + hi.z) * 0.5), top: hi.y, bottom: lo.y });
        }
    }
    if !out.is_empty() {
        info!("{} climbable chains", out.len());
    }
    commands.insert_resource(Climbables(out));
}

/// Grab a chain within reach (walking into it, or in the air), or climb the one held. Returns
/// true while climbing (the controller then does nothing else).
#[allow(clippy::too_many_arguments)]
pub fn climb_move(
    climb: &mut Climb,
    chains: &Climbables,
    p: &mut Player,
    t: &mut Transform,
    kcc: &mut KinematicCharacterController,
    keys: &ButtonInput<KeyCode>,
    bind: &Bindings,
    (forward, jump): (f32, bool),
    dt: f32,
    sounds: &mut MessageWriter<crate::audio::PostEvent>,
    relative: bool,
) -> bool {
    climb.cooldown = (climb.cooldown - dt).max(0.0);
    let fwd = Quat::from_rotation_y(p.yaw) * Vec3::NEG_Z;
    let pos = Vec2::new(t.translation.x, t.translation.z);
    let want_fwd = keys.pressed(bind.key(Act::Forward)) || forward > 0.0;
    let Some(i) = climb.on else {
        if climb.cooldown > 0.0 || p.crouched || (p.grounded && !want_fwd) {
            return false;
        }
        if std::env::var("DH_CLIMB_LOG").is_ok() {
            for c in &chains.0 {
                let d = (c.axis - pos).length();
                if d < 2.0 {
                    info!("climb: chain at {:.2} d {:.2} feet {:.2} bottom {:.2} top {:.2} grounded {}", c.axis, d, t.translation.y - STAND_HALF - RADIUS, c.bottom, c.top, p.grounded);
                }
            }
        }
        let hit = chains.0.iter().position(|c| {
            let to = c.axis - pos;
            // the hands (well above the feet) reach its end: a jump grabs a high chain
            let feet = t.translation.y - STAND_HALF - RADIUS;
            // facing it (in the air, anywhere within reach)
            let facing = p.grounded.then_some(0.3).unwrap_or(-0.5);
            to.length() < REACH && (to.length() < 0.2 || to.normalize_or_zero().dot(Vec2::new(fwd.x, fwd.z)) > facing) && t.translation.y < c.top && feet > c.bottom - 2.3
        });
        let Some(i) = hit else { return false };
        climb.on = Some(i);
        p.velocity = Vec3::ZERO;
        return true;
    };
    let Some(c) = chains.0.get(i) else {
        climb.on = None;
        return false;
    };
    // hang just off the chain, facing it
    let away = (pos - c.axis).normalize_or(-Vec2::new(fwd.x, fwd.z));
    let hold = c.axis + away * 0.35;
    let mut v = Vec3::new((hold.x - pos.x) * 8.0, 0.0, (hold.y - pos.y) * 8.0);
    // (`PSI_Gameplay_CameraRelativeClimbing`: looking down, forward climbs down)
    let way = if relative && p.pitch < -0.35 { -1.0 } else { 1.0 };
    if want_fwd {
        v.y += CLIMB_SPEED * way;
    }
    if keys.pressed(bind.key(Act::Back)) {
        v.y -= CLIMB_SPEED * way;
    }
    // the hands stay on the chain: from its top to its end
    let head = t.translation.y + 0.75;
    if head >= c.top && v.y > 0.0 {
        v.y = 0.0;
    }
    let feet = t.translation.y - STAND_HALF - RADIUS;
    let off_bottom = feet < c.bottom - 2.3 || (p.grounded && v.y < 0.0);
    // jump lets go (pushing off); use or crouch drops (`DUI_Context_ClimbableDrop`)
    if jump || keys.just_pressed(bind.key(Act::Use)) || keys.just_pressed(bind.key(Act::Crouch)) || off_bottom {
        climb.on = None;
        climb.cooldown = 0.6;
        p.velocity = if jump { -fwd * 2.5 + Vec3::Y * 3.5 } else { Vec3::ZERO };
        return false;
    }
    if v.y.abs() > 0.1 {
        climb.sound_t -= dt;
        if climb.sound_t <= 0.0 {
            climb.sound_t = 0.55;
            sounds.write(crate::audio::PostEvent::named("Snd_P_Chain_Climbing", Some(t.translation)));
        }
    }
    p.velocity = v;
    p.grounded = false;
    p.air_time = 0.0;
    p.fall_speed = 0.0;
    kcc.snap_to_ground = None;
    kcc.translation = Some(v * dt);
    true
}
