//! Props the matinees animate (`KActor.device`: a skeletal level actor in a scene group with
//! animation keys — the Tower's gangway lowering, the Hound Pits' doors, the Lighthouse's
//! machinery): the level instance's bind-pose mesh gives way to a skinned rig whose animator
//! the matinee drives (`matinee::play_matinees`).

use crate::anim::Animator;
use crate::level::{GameAssets, LevelInfo, LevelInstance, LevelSpawnSet};
use crate::world_light::{LitActor, WorldLighting};
use crate::GameState;
use bevy::camera::visibility::DynamicSkinnedMeshBounds;
use bevy::mesh::skinning::SkinnedMesh;
use bevy::mesh::MeshTag;
use bevy::prelude::*;

pub struct PropAnimPlugin;

impl Plugin for PropAnimPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(OnEnter(GameState::InGame), spawn_prop_rigs.after(LevelSpawnSet));
    }
}

/// The animated rig of a prop (its script actor).
#[derive(Component)]
pub struct PropRig {
    pub actor: u32,
    attachments: std::collections::HashMap<String, (Entity, Transform)>,
}

impl PropRig {
    pub fn attachment(&self, name: &str) -> Option<(Entity, Transform)> {
        self.attachments.get(&name.to_ascii_lowercase()).copied()
    }
}

fn spawn_prop_rigs(
    mut commands: Commands,
    assets: Option<Res<GameAssets>>,
    level: Option<Res<LevelInfo>>,
    mut wl: Option<ResMut<WorldLighting>>,
    instances: Query<(Entity, &LevelInstance, Option<&Children>)>,
    meshes: Query<(), With<Mesh3d>>,
) {
    let (Some(assets), Some(level)) = (assets, level) else { return };
    let scene = &level.scene;
    let by_index: std::collections::HashMap<u32, (Entity, Vec<Entity>)> = instances.iter().map(|(e, li, ch)| (li.index, (e, ch.map(|c| c.iter().collect()).unwrap_or_default()))).collect();
    let mut n = 0;
    for (ai, a) in scene.kismet.actors.iter().enumerate() {
        let (Some(t), None) = (a.device, a.usable) else { continue };
        let Some(vis) = assets.npc_types.get(t as usize).and_then(|v| v.as_ref()) else { continue };
        for inst in &a.instances {
            let Some((e, kids)) = by_index.get(inst) else { continue };
            // its bind pose gives way to the animated part
            if meshes.get(*e).is_ok() {
                commands.entity(*e).remove::<Mesh3d>();
            }
            for k in kids {
                if meshes.get(*k).is_ok() {
                    commands.entity(*k).insert(Visibility::Hidden);
                }
            }
            let slot = wl.as_mut().map(|w| w.alloc_slot()).unwrap_or(0);
            let root = commands.spawn((Transform::IDENTITY, Visibility::Inherited, LitActor { slot, probe_height: 0.5, brightness: 0.3, color: Vec3::splat(0.05), sun: 0.0, dominant: None })).id();
            commands.entity(*e).add_child(root);
            let bones = &vis.skeleton.bones;
            let mut joints = Vec::with_capacity(bones.len());
            for b in bones {
                let local = Transform::from_translation(Vec3::from(b.translation)).with_rotation(Quat::from_array(b.rotation).normalize());
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
            let mut ec = commands.entity(root);
            let mut attachments: std::collections::HashMap<_, _> = bones.iter().zip(&joints)
                .map(|(bone, &entity)| (bone.name.to_ascii_lowercase(), (entity, Transform::IDENTITY))).collect();
            for socket in &vis.skeleton.sockets {
                if let Some(index) = bones.iter().position(|b| b.name.eq_ignore_ascii_case(&socket.bone)) {
                    attachments.insert(socket.name.to_ascii_lowercase(), (joints[index],
                        Transform::from_translation(Vec3::from(socket.translation)).with_rotation(Quat::from_array(socket.rotation).normalize())));
                }
            }
            ec.insert(PropRig { actor: ai as u32, attachments });
            if let Some(lib) = vis.anims.clone() {
                ec.insert(Animator::new(lib, &vis.skeleton, joints));
            }
            n += 1;
        }
    }
    if n > 0 {
        info!("{n} props animated by matinees");
    }
}
