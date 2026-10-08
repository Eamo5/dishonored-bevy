//! Severed limbs (`DishonoredNotify_SeverLimb`, `DisSeveredLimbInfo`): a beheading finisher's
//! notify breaks the victim at its bone (`neck_jnt`). The body keeps all but the limb (the bone
//! hidden as `HideBoneByName` hides it: what it carries drawn to nothing at the cut), and the
//! limb goes its own way: the character's meshes again, on a copy of the limb's bones as posed
//! at the cut (every other bone drawn to nothing), falling and tumbling with the notify's push
//! along the bone. `SLInfo_Default`'s blood plays at the limb's sockets (`Gore_Head` on the
//! piece, `Gore_Neck` at the stump; `m_bAttach`: held to them), and lands on the lens when Corvo
//! is near (`m_CameraEffectEmitterInfoWhenSevered`).

use crate::level::{GameAssets, LevelInfo, GROUP_PROP, GROUP_WORLD};
use crate::npc::{NpcPart, NpcRig};
use crate::player::Player;
use crate::GameState;
use bevy::camera::visibility::DynamicSkinnedMeshBounds;
use bevy::mesh::skinning::SkinnedMesh;
use bevy::mesh::MeshTag;
use bevy::prelude::*;
use bevy::transform::TransformSystems;
use bevy_rapier3d::prelude::*;

pub struct GorePlugin;

impl Plugin for GorePlugin {
    fn build(&self, app: &mut App) {
        app.add_message::<SeverLimb>()
            .add_systems(Update, (sever, log_pieces).run_if(in_state(GameState::InGame)))
            .add_systems(Update, swap_to_gore.after(sever).run_if(in_state(GameState::InGame)))
            .add_systems(PostUpdate, hide_severed.after(crate::anim::AnimPose).before(TransformSystems::Propagate));
    }
}

/// A limb to sever: the character, the bone it breaks at, the push along the bone (unreal units).
#[derive(Message, Clone)]
pub struct SeverLimb {
    pub npc: Entity,
    pub bone: String,
    pub impulse: f32,
}

/// The bones a character has lost (by rig index): hidden from its body.
#[derive(Component, Default, Clone)]
pub struct Severed(pub Vec<usize>);

/// A body that shows its dismemberment LOD (the cuts' caps) in place of its own meshes.
#[derive(Component)]
pub struct GoreSwapped;

/// Whether a dismemberment LOD's part shows, for the bones cut: one that answers to no cut
/// always; a cap (`m_bShowIfCut`) once its cut is made; the rest while it isn't.
fn gore_part_shown(part: &(String, String, bool), cuts: &[String]) -> bool {
    if part.1.eq_ignore_ascii_case("None") {
        return true;
    }
    let cut = cuts.iter().any(|c| c.eq_ignore_ascii_case(&part.1));
    if part.2 { cut } else { !cut }
}

/// The bones under the cuts (by rig index), and their names.
fn cut_limbs(rig: &NpcRig, cuts: &[usize]) -> Vec<bool> {
    let n = rig.len();
    let mut limb = vec![false; n];
    for i in 0..n {
        limb[i] = cuts.contains(&i) || rig.parent_of(i).is_some_and(|p| p < i && limb[p]);
    }
    limb
}

/// A cut body (as cut, or from a save) takes its dismemberment LOD where it has one: its own
/// meshes go, the LOD's parts that are the body's (not under a cut) and showing come.
#[allow(clippy::type_complexity)]
fn swap_to_gore(
    mut commands: Commands,
    assets: Option<Res<GameAssets>>,
    npcs: Query<(Entity, &NpcRig, &Severed, &Children), Without<GoreSwapped>>,
    children: Query<&Children>,
    parts: Query<(&NpcPart, &MeshTag)>,
    joints_of: Query<&bevy::mesh::skinning::SkinnedMesh>,
) {
    let Some(assets) = assets else { return };
    for (e, rig, sev, kids) in &npcs {
        // the visual holding its parts, and a part to learn the type and the skin from
        let mut found = None;
        for v in kids.iter() {
            for p in children.get(v).into_iter().flatten() {
                if let (Ok((np, tag)), Ok(skin)) = (parts.get(*p), joints_of.get(*p)) {
                    found = Some((v, np.npc_type, tag.0, skin.joints.clone()));
                }
            }
        }
        let Some((visual, tid, slot, skin_joints)) = found else { continue };
        let Some(vis) = assets.npc_types.get(tid as usize).and_then(|v| v.as_ref()) else { continue };
        let Some((gp, info)) = vis.gore.as_ref() else {
            // (none: the cut bone is drawn to nothing, `hide_severed`)
            commands.entity(e).try_insert(GoreSwapped);
            commands.entity(e).try_insert(KeepBones);
            continue;
        };
        let limb = cut_limbs(rig, &sev.0);
        let cuts: Vec<String> = sev.0.iter().filter_map(|&b| rig.name_of(b).map(|s| s.to_string())).collect();
        for p in children.get(visual).into_iter().flatten() {
            if parts.contains(*p) {
                commands.entity(*p).try_despawn();
            }
        }
        for (pi, ((mesh, mat), part)) in gp.parts.iter().zip(info.iter()).enumerate() {
            let owned = rig.index(&part.0).is_some_and(|o| limb[o]);
            if owned || !gore_part_shown(part, &cuts) {
                continue;
            }
            let mut ec = commands.spawn((
                Mesh3d(mesh.clone()),
                MeshTag(slot),
                SkinnedMesh { inverse_bindposes: vis.inverse_bindposes.clone(), joints: skin_joints.clone() },
                DynamicSkinnedMeshBounds,
                crate::fxlight::LitPart,
                NpcPart { index: pi as u32, npc_type: tid, lod: crate::npc::GORE_LOD },
                ChildOf(visual),
            ));
            mat.apply(&mut ec);
        }
        commands.entity(e).try_insert(GoreSwapped);
    }
}

/// A body without a dismemberment LOD: its cut bones drawn to nothing.
#[derive(Component)]
pub struct KeepBones;

/// A severed limb, falling on its own.
#[derive(Component)]
pub struct SeveredPiece;

/// (tests: `DH_GORE_LOG`) where the severed pieces are.
fn log_pieces(time: Res<Time>, pieces: Query<&GlobalTransform, With<SeveredPiece>>, mut last: Local<f32>) {
    if std::env::var("DH_GORE_LOG").is_err() || time.elapsed_secs() - *last < 0.25 {
        return;
    }
    *last = time.elapsed_secs();
    for g in &pieces {
        info!("gore: piece at {:.2}", g.translation());
    }
}

/// Unreal units to metres.
const UU: f32 = 0.01905;

#[allow(clippy::too_many_arguments, clippy::type_complexity)]
fn sever(
    mut commands: Commands,
    mut msgs: MessageReader<SeverLimb>,
    (assets, level): (Option<Res<GameAssets>>, Option<Res<LevelInfo>>),
    mut npcs: Query<(&NpcRig, Option<&mut Severed>, &Children)>,
    children: Query<&Children>,
    parts: Query<(&NpcPart, &MeshTag)>,
    joints: Query<(&Transform, &GlobalTransform)>,
    player: Query<&Transform, With<Player>>,
    mut fx: MessageWriter<crate::particles::SpawnEffect>,
    mut vm: Option<ResMut<crate::kismet::Vm>>,
) {
    for m in msgs.read() {
        let (Some(assets), Some(level)) = (assets.as_ref(), level.as_ref()) else { continue };
        let Ok((rig, severed, kids)) = npcs.get_mut(m.npc) else { continue };
        let Some(b) = rig.index(&m.bone) else { continue };
        if severed.as_ref().is_some_and(|s| s.0.contains(&b)) {
            continue;
        }
        // the character's meshes (its visual's parts) and its lighting slot
        let mut type_slot = None;
        for v in kids.iter() {
            for p in children.get(v).into_iter().flatten() {
                if let Ok((np, tag)) = parts.get(*p) {
                    type_slot = Some((np.npc_type, tag.0));
                }
            }
        }
        let Some((tid, slot)) = type_slot else { continue };
        let Some(vis) = assets.npc_types.get(tid as usize).and_then(|v| v.as_ref()) else { continue };
        let n = rig.len();
        // the limb: the bone and all below it
        let mut limb = vec![false; n];
        for i in 0..n {
            limb[i] = i == b || rig.parent_of(i).is_some_and(|p| p < i && limb[p]);
        }
        let Some(Ok((_, cut))) = rig.joint_at(b).map(|j| joints.get(j)) else { continue };
        let (_, cut_rot, cut_at) = cut.to_scale_rotation_translation();
        // the piece: at the cut, tumbling off along the bone
        let push = (cut_rot * Vec3::X) * m.impulse.max(40.0) * UU + Vec3::Y * 0.8;
        let spin = Vec3::new(rand::random::<f32>() - 0.5, rand::random::<f32>() - 0.5, rand::random::<f32>() - 0.5) * 8.0;
        let root = commands
            .spawn((
                Transform::from_translation(cut_at).with_rotation(cut_rot),
                Visibility::default(),
                RigidBody::Dynamic,
                ColliderMassProperties::Mass(4.0),
                CollisionGroups::new(GROUP_PROP, GROUP_WORLD | GROUP_PROP),
                Velocity { linear: push, angular: spin },
                Damping { linear_damping: 0.3, angular_damping: 1.5 },
                SeveredPiece,
                DespawnOnExit(GameState::InGame),
            ))
            .id();
        // its bones: the limb's as posed now (under the cut), the rest drawn to nothing
        let mut copies = vec![Entity::PLACEHOLDER; n];
        for i in 0..n {
            let e = if i == b {
                commands.spawn((Transform::IDENTITY, Visibility::default(), ChildOf(root))).id()
            } else if limb[i] {
                let local = rig.joint_at(i).and_then(|j| joints.get(j).ok()).map(|(t, _)| *t).unwrap_or_default();
                commands.spawn((local, Visibility::default())).id()
            } else {
                commands.spawn((Transform::from_scale(Vec3::ZERO), Visibility::default(), ChildOf(root))).id()
            };
            copies[i] = e;
        }
        for i in 0..n {
            if i != b && limb[i] {
                if let Some(p) = rig.parent_of(i) {
                    commands.entity(copies[p]).add_child(copies[i]);
                }
            }
        }
        // (the collider about the limb's middle: half way to its end)
        let far = (0..n).filter(|i| limb[*i]).filter_map(|i| rig.joint_at(i).and_then(|j| joints.get(j).ok())).map(|(_, g)| g.translation()).fold(cut_at, |a, p| if p.distance(cut_at) > a.distance(cut_at) { p } else { a });
        commands.entity(root).insert(Collider::compound(vec![(cut_rot.inverse() * (far - cut_at) * 0.5, Quat::IDENTITY, Collider::ball(0.1))]));
        let cut_names: Vec<String> = severed.as_ref().map(|s| s.0.clone()).unwrap_or_default().into_iter().chain([b]).filter_map(|i| rig.name_of(i).map(|s| s.to_string())).collect();
        let piece: Vec<&(Handle<Mesh>, crate::level::PartMat)> = match vis.gore.as_ref() {
            Some((gp, info)) => gp.parts.iter().zip(info.iter()).filter(|(_, part)| rig.index(&part.0).is_some_and(|o| limb[o]) && gore_part_shown(part, &cut_names)).map(|(p, _)| p).collect(),
            None => vis.parts.parts.iter().collect(),
        };
        for (mesh, mat) in piece {
            let mut ec = commands.spawn((
                Mesh3d(mesh.clone()),
                MeshTag(slot),
                SkinnedMesh { inverse_bindposes: vis.inverse_bindposes.clone(), joints: copies.clone() },
                DynamicSkinnedMeshBounds,
                crate::fxlight::LitPart,
                ChildOf(root),
            ));
            mat.apply(&mut ec);
        }
        // the body loses it
        match severed {
            Some(mut s) => s.0.push(b),
            None => {
                commands.entity(m.npc).try_insert(Severed(vec![b]));
            }
        }
        // the blood: at each of the limb's sockets, held to the piece or the stump
        if let Some(info) = level.scene.severed_limbs.as_ref() {
            if let Some(l) = info.limbs.iter().find(|l| l.bone.eq_ignore_ascii_case(&m.bone)) {
                for (socket, system, attached) in &l.effects {
                    let Some(s) = vis.skeleton.sockets.iter().find(|s| s.name.eq_ignore_ascii_case(socket)) else { continue };
                    let Some(sb) = rig.index(&s.bone) else { continue };
                    let local = Transform::from_translation(Vec3::from(s.translation)).with_rotation(Quat::from_array(s.rotation).normalize());
                    let on = if limb[sb] { copies[sb] } else if let Some(j) = rig.joint_at(sb) { j } else { continue };
                    let at = commands.spawn((local, Visibility::default(), ChildOf(on))).id();
                    let follow = if *attached { Some(at) } else { None };
                    let world = rig.joint_at(sb).and_then(|j| joints.get(j).ok()).map(|(_, g)| g.mul_transform(local).translation()).unwrap_or(cut_at);
                    fx.write(crate::particles::SpawnEffect {
                        follow,
                        turn: *attached,
                        system: Some(*system),
                        secs: 4.0,
                        ..crate::particles::SpawnEffect::at("", if *attached { Vec3::ZERO } else { world })
                    });
                }
            }
            if let (Some((lens, near, chance)), Some(vm), Ok(pt)) = (info.lens, vm.as_mut(), player.single()) {
                if pt.translation.distance(cut_at) < near && rand::random::<f32>() < chance {
                    vm.lens.push((crate::hudfx::GAMEPLAY_LENS + 0x300, Some((lens, false, 0.0))));
                }
            }
        }
        info!("gore: {} severed at {}", m.bone, cut_at);
    }
}

/// A body's lost bones drawn to nothing (after its clips have posed it), where it has no
/// dismemberment LOD to show the cut.
fn hide_severed(npcs: Query<(&NpcRig, &Severed), With<KeepBones>>, mut joints: Query<&mut Transform>) {
    for (rig, s) in &npcs {
        for &b in &s.0 {
            if let Some(mut t) = rig.joint_at(b).and_then(|j| joints.get_mut(j).ok()) {
                t.scale = Vec3::ZERO;
            }
        }
    }
}
