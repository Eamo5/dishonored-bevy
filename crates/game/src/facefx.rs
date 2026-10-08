//! Characters' faces as the original's FaceFX drives them (`dhcook::facefx`): a spoken line's
//! curves (`open`, `W`, `PBM`, `Blink`, head orientation...) run through the character's
//! compiled face graph, and its bone-pose nodes move the face bones (jaw, lips, tongue,
//! eyelids, brows) over the body's animation. Lines without facial animation keep the
//! loudness-driven jaw (`npc::speaking_jaw`).

use crate::level::LevelInfo;
use crate::npc::{Npc, NpcRig};
use crate::GameState;
use bevy::prelude::*;
use dhcook::format::{FaceFx, FxAnim};
use std::collections::HashMap;
use std::sync::Arc;

pub struct FaceFxPlugin;

impl Plugin for FaceFxPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(
            PostUpdate,
            face_animate
                .after(crate::anim::AnimPose)
                .before(crate::npc::speaking_jaw)
                .before(bevy::transform::TransformSystems::Propagate)
                .run_if(in_state(GameState::InGame)),
        );
    }
}

/// The map's faces and facial animations, and the lines' by voice event.
struct FaceData {
    map: String,
    fx: Arc<FaceFx>,
    by_event: HashMap<u32, String>,
}

/// A line's face playing: its animation, how far in, and the face bones' joints (by the
/// actor's bones).
#[derive(Component)]
pub struct FaceAnim {
    key: String,
    actor: u32,
    t: f32,
    joints: Vec<Option<usize>>,
}

/// A curve's value at `t` (Hermite between keys: time, value, slope in, slope out).
fn sample(keys: &[[f32; 4]], t: f32) -> f32 {
    let Some(first) = keys.first() else { return 0.0 };
    if t <= first[0] {
        return first[1];
    }
    let last = keys[keys.len() - 1];
    if t >= last[0] {
        return last[1];
    }
    let i = keys.partition_point(|k| k[0] <= t).saturating_sub(1).min(keys.len() - 2);
    let (a, b) = (keys[i], keys[i + 1]);
    let h = (b[0] - a[0]).max(1e-6);
    let s = (t - a[0]) / h;
    let (s2, s3) = (s * s, s * s * s);
    (2.0 * s3 - 3.0 * s2 + 1.0) * a[1] + (s3 - 2.0 * s2 + s) * h * a[3] + (-2.0 * s3 + 3.0 * s2) * b[1] + (s3 - s2) * h * b[2]
}

/// A link function of the face graph: 1 linear (slope, offset), 2 quadratic, 3 cubic, 4 square
/// root, 5 negate, 6 inverse, 7 one, 8 constant.
fn link(f: u32, p: &[f32], x: f32) -> f32 {
    let a = p.first().copied().unwrap_or(1.0);
    match f {
        1 => a * x + p.get(1).copied().unwrap_or(0.0),
        2 => a * x * x,
        3 => a * x * x * x,
        4 => a * x.max(0.0).sqrt(),
        5 => -x,
        6 => {
            if x.abs() > 1e-6 {
                1.0 / x
            } else {
                0.0
            }
        }
        7 => 1.0,
        8 => a,
        _ => x,
    }
}

fn anim_length(a: &FxAnim) -> f32 {
    a.curves.iter().filter_map(|(_, k)| k.last().map(|k| k[0])).fold(0.0, f32::max)
}

#[allow(clippy::too_many_arguments, clippy::type_complexity)]
fn face_animate(
    mut commands: Commands,
    time: Res<Time>,
    level: Option<Res<LevelInfo>>,
    mut data: Local<Option<FaceData>>,
    speaking: Query<(Entity, &Npc, &NpcRig, &crate::audio::Speaking), Without<FaceAnim>>,
    mut faces: Query<(Entity, &NpcRig, &mut FaceAnim, Option<&crate::audio::Speaking>)>,
    mut joints: Query<&mut Transform, Without<Npc>>,
) {
    let Some(level) = level else { return };
    let scene = &level.scene;
    // the map's faces, read when it changes
    if data.as_ref().is_none_or(|d| d.map != scene.name) {
        let fx: FaceFx = std::fs::read(dhcook::facefx_path(&crate::loading::cache_dir(), &scene.name))
            .ok()
            .and_then(|d| serde_json::from_slice(&d).ok())
            .unwrap_or_default();
        let by_event = fx.anims.keys().map(|k| (crate::audio::event_id(&format!("Play_{k}")), k.clone())).collect();
        if std::env::var("DH_FACEFX_LOG").is_ok() {
            info!("facefx: {} facial animations, {} faces", fx.anims.len(), fx.actors.len());
        }
        *data = Some(FaceData { map: scene.name.clone(), fx: Arc::new(fx), by_event });
    }
    let Some(d) = data.as_ref() else { return };
    // a line starts: its face (when it has one and the speaker a face graph)
    for (e, npc, rig, sp) in &speaking {
        let Some(key) = d.by_event.get(&sp.event) else { continue };
        let Some(actor) = scene.spawners.get(npc.spawner as usize).and_then(|s| s.npc_type).and_then(|t| scene.npc_types.get(t as usize)).and_then(|t| t.facefx) else {
            continue;
        };
        let Some(a) = d.fx.actors.get(actor as usize) else { continue };
        let joints: Vec<Option<usize>> = a.bones.iter().map(|b| rig.index(&b.name)).collect();
        if std::env::var("DH_FACEFX_LOG").is_ok() {
            info!("facefx: {} speaks {key} with {} ({} of {} face bones)", npc.name, a.name, joints.iter().flatten().count(), joints.len());
        }
        commands.entity(e).try_insert(FaceAnim { key: key.clone(), actor, t: 0.0, joints });
    }
    let dt = time.delta_secs();
    for (e, rig, mut fa, sp) in &mut faces {
        let Some(anim) = d.fx.anims.get(&fa.key) else {
            commands.entity(e).try_remove::<FaceAnim>();
            continue;
        };
        // (a new line replaces it)
        if sp.is_some_and(|s| d.by_event.get(&s.event).is_some_and(|k| *k != fa.key)) {
            commands.entity(e).try_remove::<FaceAnim>();
            continue;
        }
        fa.t += dt;
        let len = anim_length(anim);
        if fa.t > len + anim.blend_out {
            commands.entity(e).try_remove::<FaceAnim>();
            continue;
        }
        let Some(actor) = d.fx.actors.get(fa.actor as usize) else { continue };
        // fading in and out
        let w = (fa.t / anim.blend_in.max(0.01)).min(1.0) * ((len + anim.blend_out - fa.t) / anim.blend_out.max(0.01)).clamp(0.0, 1.0);
        // the face graph, node by node (inputs come before what they feed)
        let mut values = vec![0.0f32; actor.nodes.len()];
        for (i, n) in actor.nodes.iter().enumerate() {
            let mut track = anim.curves.iter().find(|(c, _)| c.eq_ignore_ascii_case(&n.name)).map(|(_, k)| sample(k, fa.t)).unwrap_or(0.0);
            // (`DH_FACEFX_FORCE=NODE`: that node full on, for checking a face)
            if std::env::var("DH_FACEFX_FORCE").is_ok_and(|f| f.eq_ignore_ascii_case(&n.name)) {
                track = 1.0;
            }
            let mut v = track;
            for (src, f, p) in &n.inputs {
                if let Some(&x) = values.get(*src as usize) {
                    v += link(*f, p, x);
                }
            }
            values[i] = if n.max > n.min { v.clamp(n.min, n.max) } else { v };
        }
        if std::env::var("DH_FACEFX_LOG").is_ok() && (fa.t * 4.0).floor() != ((fa.t - dt) * 4.0).floor() {
            let named = |n: &str| actor.nodes.iter().position(|x| x.name == n).map(|i| values[i]).unwrap_or(-1.0);
            info!("facefx: {} t {:.2} w {:.2} open {:.2} W {:.2} wide {:.2} PBM {:.2} blink {:.2}", fa.key, fa.t, w, named("open"), named("W"), named("wide"), named("PBM"), named("Blink"));
        }
        // the face bones: each bone-pose node's share of its delta
        for (b, j) in actor.bones.iter().zip(&fa.joints) {
            let Some(j) = *j else { continue };
            let Some(je) = rig.joint_at(j) else { continue };
            let Ok(mut jt) = joints.get_mut(je) else { continue };
            let mut rot = Quat::IDENTITY;
            let mut pos = Vec3::ZERO;
            for l in &b.links {
                let v = values.get(l.node as usize).copied().unwrap_or(0.0) * w;
                if v.abs() < 1e-4 {
                    continue;
                }
                let mut q = Quat::from_array(l.rot);
                if q.w < 0.0 {
                    q = -q;
                }
                rot *= Quat::IDENTITY.slerp(q.normalize(), v.clamp(-1.0, 1.0));
                pos += Vec3::from(l.pos) * v;
            }
            jt.rotation *= rot;
            jt.translation += pos;
        }
    }
}
