//! The crossbow's kill cam (`DisCamera_FollowProjectile` with the crossbow's
//! `DisTweaks_FireCrossbow.m_KillCamSettings`): a bolt that will kill (`Twk_Proj_Arrow`'s
//! `m_bKillCamEnabled`) is followed from just behind it (`m_Offset`, `m_Rotation`) with a narrow
//! view (`m_fFollowFOV`) as the world slows (`m_fFollowTimeScale`); the camera rides it so far
//! (`m_fCameraStopDistance`, 14 m) and stops there to watch the bolt strike, the world at
//! `m_fEndFollowTimeScale` for `m_fWitnessKillTime`, then the view is Corvo's again. The HUD
//! and his hands are put away meanwhile; `m_fTimeOut` (game time) ends a chase that runs long.

use crate::gameplay::TimeControl;
use crate::player::{Player, PlayerCamera};
use crate::powers::Projectile;
use crate::GameState;
use bevy::prelude::*;
use bevy::transform::TransformSystems;

pub struct KillCamPlugin;

impl Plugin for KillCamPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<KillCam>()
            .add_message::<StartKillCam>()
            .add_systems(OnEnter(GameState::InGame), |mut k: ResMut<KillCam>| *k = KillCam::default())
            .add_systems(PostUpdate, follow_bolt.before(TransformSystems::Propagate).run_if(in_state(GameState::InGame)));
    }
}

/// A lethal bolt to follow, and whom it will kill.
#[derive(Message, Clone, Copy)]
pub struct StartKillCam {
    pub bolt: Entity,
    pub npc: Entity,
}

/// The kill cam under way.
#[derive(Resource, Default)]
pub struct KillCam {
    on: Option<Chase>,
}

impl KillCam {
    pub fn active(&self) -> bool {
        self.on.is_some()
    }
}

struct Chase {
    bolt: Entity,
    npc: Entity,
    /// game seconds of the chase; real seconds since the bolt struck
    game_t: f32,
    struck: Option<f32>,
    /// the camera's place once it stopped (world), and where the bolt set out from
    held: Option<Transform>,
    from: Option<Vec3>,
    /// the last place it was put behind the bolt
    last: Option<Transform>,
}

/// The crossbow's settings (`killcam.*` in the player's numbers), metres and degrees.
struct Settings {
    witness: f32,
    fov: f32,
    follow: f32,
    end: f32,
    timeout: f32,
    stop: f32,
    offset: Vec3,
    pitch: f32,
}

fn settings(data: &crate::gamedata::Data) -> Settings {
    let v = |k: &str, d: f32| data.pawn(&format!("killcam.{k}"), d);
    Settings {
        witness: v("m_fWitnessKillTime", 1.5),
        fov: v("m_fFollowFOV", 30.0),
        follow: v("m_fFollowTimeScale", 0.1).max(0.01),
        end: v("m_fEndFollowTimeScale", 0.6).max(0.01),
        timeout: v("m_fTimeOut", 1.5),
        stop: v("m_fCameraStopDistance", 750.0) * 0.01905,
        // UE3 (x forward, y right, z up), unreal units -> behind, beside, above
        offset: Vec3::new(v("m_Offset.y", 0.0), v("m_Offset.z", 4.5), -v("m_Offset.x", -60.0)) * 0.01905,
        pitch: v("m_Rotation.pitch", -546.0) * std::f32::consts::TAU / 65536.0,
    }
}

#[allow(clippy::too_many_arguments, clippy::type_complexity)]
fn follow_bolt(
    time: Res<Time<bevy::time::Real>>,
    mut kc: ResMut<KillCam>,
    mut starts: MessageReader<StartKillCam>,
    data: Res<crate::gamedata::Data>,
    mut tc: ResMut<TimeControl>,
    bolts: Query<(&Transform, &Projectile), (Without<Player>, Without<PlayerCamera>)>,
    npcs: Query<&Transform, (With<crate::npc::Npc>, Without<Player>, Without<PlayerCamera>, Without<Projectile>)>,
    mut player: Query<(&Transform, &mut Player), (Without<PlayerCamera>, Without<Projectile>)>,
    mut cam: Query<(&mut Transform, &mut Projection), (With<PlayerCamera>, Without<Player>, Without<Projectile>)>,
    mut hands: Query<&mut Visibility, Or<(With<crate::arms::ArmsRoot>, With<crate::combat::ViewModel>)>>,
) {
    for s in starts.read() {
        if kc.on.is_none() {
            kc.on = Some(Chase { bolt: s.bolt, npc: s.npc, game_t: 0.0, struck: None, held: None, from: None, last: None });
            info!("kill cam: following the bolt");
        }
    }
    let Some(chase) = kc.on.as_mut() else { return };
    let st = settings(&data);
    let dt = time.delta_secs().min(0.1);
    let (Ok((pt, mut p)), Ok((mut ct, mut proj))) = (player.single_mut(), cam.single_mut()) else { return };
    p.locked = true;
    let bolt = bolts.get(chase.bolt).ok().filter(|(_, pr)| !pr.stuck).map(|(t, _)| *t);
    let target = npcs.get(chase.npc).map(|t| t.translation + Vec3::Y * 1.2).ok();
    // the bolt flying: behind it until it nears the victim; then the camera holds
    match (bolt, chase.struck) {
        (Some(bt), None) => {
            chase.game_t += dt * st.follow;
            tc.finisher = st.follow;
            let from = *chase.from.get_or_insert(bt.translation);
            let far = bt.translation.distance(from) > st.stop;
            let _ = target;
            if chase.held.is_none() && !far {
                let rot = bt.rotation * Quat::from_rotation_x(st.pitch);
                let at = bt.translation + bt.rotation * st.offset;
                let world = Transform::from_translation(at).with_rotation(rot);
                chase.last = Some(world);
                set_view(pt, &mut ct, world);
            } else {
                // (stopped short: watching it go in)
                let eye = eye_of(pt, &ct);
                let last = chase.last;
                let held = *chase.held.get_or_insert_with(|| last.unwrap_or(eye));
                set_view(pt, &mut ct, held);
            }
            if chase.game_t > st.timeout {
                chase.struck = Some(0.0);
            }
        }
        _ => {
            // struck (or gone): the kill witnessed a moment, the world at its end speed
            let t = chase.struck.get_or_insert(0.0);
            *t += dt;
            tc.finisher = st.end;
            let eye = eye_of(pt, &ct);
            let last = chase.last;
            let held = *chase.held.get_or_insert_with(|| last.unwrap_or(eye));
            set_view(pt, &mut ct, held);
            if *t > st.witness {
                kc.on = None;
                tc.finisher = 1.0;
                p.locked = false;
                for mut v in &mut hands {
                    *v = Visibility::Inherited;
                }
                info!("kill cam: done");
                return;
            }
        }
    }
    if let Projection::Perspective(pp) = proj.as_mut() {
        pp.fov = st.fov.to_radians();
    }
    for mut v in &mut hands {
        *v = Visibility::Hidden;
    }
}

/// Where Corvo's own view is (the camera as his systems left it).
fn eye_of(player: &Transform, cam: &Transform) -> Transform {
    let (_, r, tr) = (player.compute_affine() * cam.compute_affine()).to_scale_rotation_translation();
    Transform::from_translation(tr).with_rotation(r)
}

/// The camera (a child of Corvo) put at a place in the world.
fn set_view(player: &Transform, cam: &mut Transform, world: Transform) {
    let local = player.compute_affine().inverse() * world.compute_affine();
    let (_, r, tr) = local.to_scale_rotation_translation();
    cam.translation = tr;
    cam.rotation = r;
}
