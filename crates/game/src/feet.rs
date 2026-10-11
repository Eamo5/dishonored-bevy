//! Feet on the ground: characters standing or walking on stairs and slopes put each foot on
//! what is under it (a ray down from the foot, `DisTweaks_NPCPawn_Body.m_fMaxFootIKCastHeight`
//! above it), the hips lowered for the lower foot to reach, each leg bent to its foot (two
//! bones: thigh and shin turning in the knee's plane) and the foot turned to the ground's
//! slope. The clips say when (`DisNotify_FootPlacement`: a kick's off as it starts, on again
//! as it ends; it holds until another says otherwise).

use crate::anim::{AnimPose, Animator};
use crate::level::GROUP_WORLD;
use crate::npc::{Npc, NpcAnim, NpcRig, NpcVisual, ScriptedAnim, NPC_CENTER};
use crate::player::Player;
use crate::GameState;
use bevy::prelude::*;
use bevy::transform::TransformSystems;
use bevy_rapier3d::prelude::*;

pub struct FeetPlugin;

impl Plugin for FeetPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(PostUpdate, place_feet.after(AnimPose).before(TransformSystems::Propagate).run_if(in_state(GameState::InGame)));
    }
}

/// How far from Corvo characters' feet are placed.
const NEAR: f32 = 30.0;
/// The ray's start above the foot (`m_fMaxFootIKCastHeight`, 75 units) and how far below it
/// the ground may be.
const CAST_UP: f32 = 0.75;
const CAST_DOWN: f32 = 0.5;
/// The most a foot is moved, the most the foot turns to the slope.
const MAX_SHIFT: f32 = 0.45;
const MAX_TILT: f32 = 0.45;

/// A character's legs (thigh, shin, foot bones, left then right; its hips) and their
/// placement now (eased): the hips' drop, each foot's lift and turn.
#[derive(Component)]
pub struct Feet {
    legs: [[usize; 3]; 2],
    hips: usize,
    /// placed now (its clips' `DisNotify_FootPlacement`s say), and the clip they were last
    /// read from, how far in
    on: bool,
    clip: Option<(crate::anim::ClipId, f32)>,
    drop: f32,
    lift: [f32; 2],
    tilt: [Quat; 2],
}

/// Bone `i`'s world rotation and position from the animator's pose, under `base`.
fn world_of(anim: &Animator, base: &Transform, i: usize) -> (Quat, Vec3) {
    let m = anim.model(i);
    (base.rotation * m.rotation, base.transform_point(m.translation))
}

#[allow(clippy::type_complexity)]
fn place_feet(
    mut commands: Commands,
    time: Res<Time>,
    rapier: ReadRapierContext,
    player: Query<&Transform, With<Player>>,
    mut npcs: Query<
        (Entity, &Npc, &Transform, &NpcRig, &Animator, &NpcAnim, &Children, Option<&mut Feet>),
        (Without<ScriptedAnim>, Without<crate::ragdoll::Ragdoll>, Without<crate::ragdoll::GoLimp>, Without<crate::carry::Falling>, Without<Player>),
    >,
    visuals: Query<&Transform, (With<NpcVisual>, Without<Npc>)>,
    mut joints: Query<&mut Transform, (Without<Npc>, Without<NpcVisual>, Without<Player>)>,
) {
    // (`DH_NO_FEET`: as the clips have them, for comparison)
    if std::env::var_os("DH_NO_FEET").is_some() {
        return;
    }
    let Ok(ctx) = rapier.single() else { return };
    let Ok(pt) = player.single() else { return };
    let dt = time.delta_secs().min(0.05);
    let ground = QueryFilter::default().exclude_sensors().groups(CollisionGroups::new(Group::ALL, GROUP_WORLD));
    let log = std::env::var_os("DH_FEET_LOG").is_some();
    for (e, npc, t, rig, anim, st, children, feet) in &mut npcs {
        if anim.frozen || t.translation.distance_squared(pt.translation) > NEAR * NEAR {
            continue;
        }
        let Some(mut feet) = feet else {
            // (its legs found once)
            let leg = |side: &str| -> Option<[usize; 3]> { Some([rig.index(&format!("upper_leg_{side}_jnt"))?, rig.index(&format!("lower_leg_{side}_jnt"))?, rig.index(&format!("foot_{side}_jnt"))?]) };
            if let (Some(l), Some(r), Some(h)) = (leg("L"), leg("R"), rig.index("Root_jnt")) {
                commands.entity(e).try_insert(Feet { legs: [l, r], hips: h, on: true, clip: None, drop: 0.0, lift: [0.0; 2], tilt: [Quat::IDENTITY; 2] });
            }
            continue;
        };
        let Some(vt) = children.iter().find_map(|c| visuals.get(c).ok()) else { continue };
        let base = t.mul_transform(*vt);
        // (what its clips' notifies have said: the last one left, the new one so far)
        if let Some(p) = anim.current() {
            let marks = |c, from: f32, to: f32| st.feet_marks(c).iter().filter(move |m| m.0 > from && m.0 <= to).map(|m| m.1);
            let said = match feet.clip {
                Some((c, t0)) if c == p.clip && t0 <= p.t => marks(c, t0, p.t).last(),
                Some((c, t0)) if c == p.clip => marks(c, t0, f32::MAX).chain(marks(c, -1.0, p.t)).last(),
                Some((c, t0)) => marks(c, t0, f32::MAX).chain(marks(p.clip, -1.0, p.t)).last(),
                None => marks(p.clip, -1.0, p.t).last(),
            };
            if let Some(s) = said {
                feet.on = s;
            }
            feet.clip = Some((p.clip, p.t));
        }
        // where each foot's ground is, against the floor the character stands on
        let on = !npc.is_down() && npc.grounded && npc.attack_t.is_none() && feet.on;
        let floor = t.translation.y - NPC_CENTER;
        let mut want = [0.0f32; 2];
        let mut slope = [Quat::IDENTITY; 2];
        if on {
            for k in 0..2 {
                let (_, f) = world_of(anim, &base, feet.legs[k][2]);
                if let Some((_, hit)) = ctx.cast_ray_and_get_normal(f + Vec3::Y * CAST_UP, Vec3::NEG_Y, CAST_UP + CAST_DOWN, true, ground) {
                    want[k] = (hit.point.y - floor).clamp(-MAX_SHIFT, MAX_SHIFT);
                    let n = hit.normal.normalize_or(Vec3::Y);
                    if n.y > 0.5 {
                        let q = Quat::from_rotation_arc(Vec3::Y, n);
                        let (axis, ang) = q.to_axis_angle();
                        slope[k] = Quat::from_axis_angle(axis, ang.min(MAX_TILT));
                    }
                }
            }
        }
        // eased toward (the hips drop for the lower foot)
        let k = (dt * 12.0).min(1.0);
        let drop = want[0].min(want[1]).min(0.0);
        feet.drop += (drop - feet.drop) * k;
        for i in 0..2 {
            feet.lift[i] += (want[i] - feet.lift[i]) * k;
            feet.tilt[i] = feet.tilt[i].slerp(slope[i], k);
        }
        if feet.drop.abs() < 1e-3 && feet.lift.iter().all(|l| l.abs() < 1e-3) && feet.tilt.iter().all(|q| q.angle_between(Quat::IDENTITY) < 1e-3) {
            continue;
        }
        // the hips down
        let hips_parent = rig.parent_of(feet.hips).map(|p| world_of(anim, &base, p).0).unwrap_or(base.rotation);
        if let Some(mut jt) = rig.joint_at(feet.hips).and_then(|j| joints.get_mut(j).ok()) {
            jt.translation += hips_parent.inverse() * Vec3::Y * feet.drop;
        }
        let down = Vec3::Y * feet.drop;
        for side in 0..2 {
            let [th, sh, ft] = feet.legs[side];
            let (wt, h) = world_of(anim, &base, th);
            let (ws, kn) = world_of(anim, &base, sh);
            let (wf, a) = world_of(anim, &base, ft);
            let (h, kn, a) = (h + down, kn + down, a + down);
            // the foot where its ground is
            let target = a + Vec3::Y * (feet.lift[side] - feet.drop);
            let (la, lb) = ((kn - h).length(), (a - kn).length());
            if la < 1e-3 || lb < 1e-3 {
                continue;
            }
            let d = (target - h).length().clamp((la - lb).abs() + 1e-3, (la + lb) * 0.999);
            let (u, v) = (h - kn, a - kn);
            let cur = u.angle_between(v);
            let new = ((la * la + lb * lb - d * d) / (2.0 * la * lb)).clamp(-1.0, 1.0).acos();
            // (the knee's plane; a straight leg bends forward)
            let n = u.cross(v).try_normalize().unwrap_or_else(|| (wt * Vec3::Z).normalize_or(Vec3::X));
            let bend = Quat::from_axis_angle(n, new - cur);
            let a1 = kn + bend * v;
            let swing = Quat::from_rotation_arc((a1 - h).normalize_or(Vec3::NEG_Y), (target - h).normalize_or(Vec3::NEG_Y));
            let wt2 = swing * wt;
            let ws2 = swing * bend * ws;
            let wf2 = feet.tilt[side] * wf;
            // back to the bones' own frames
            let parent_rot = |i: usize, fallback: Quat| rig.parent_of(i).map(|p| if p == th { wt2 } else if p == sh { ws2 } else { world_of(anim, &base, p).0 }).unwrap_or(fallback);
            let pth = parent_rot(th, base.rotation);
            let psh = parent_rot(sh, wt2);
            let pft = parent_rot(ft, ws2);
            for (i, p, w) in [(th, pth, wt2), (sh, psh, ws2), (ft, pft, wf2)] {
                if let Some(mut jt) = rig.joint_at(i).and_then(|j| joints.get_mut(j).ok()) {
                    jt.rotation = (p.inverse() * w).normalize();
                }
            }
        }
        if log && (time.elapsed_secs() * 2.0).floor() != ((time.elapsed_secs() - dt) * 2.0).floor() {
            info!("feet: {} at {:.2} drop {:.2} lift {:.2?}", npc.name, t.translation, feet.drop, feet.lift);
        }
    }
}
