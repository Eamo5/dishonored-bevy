//! Carrying bodies: Corvo picks up the unconscious and the dead ([F]), carries them over his
//! shoulder (`Ply_Empty_CarryCorpse_as`, the body playing the matching `Corpses_CarryCorpse_*
//! _Slave` clips in step), and drops them where he stands ([F]) or throws them (attack). The
//! carried body is drawn with the view model; the level scripts hear of it
//! (`DisSeqEvent_Corpse`: picked up, dropped), and the unconscious drown in water.

use crate::anim::Animator;
use crate::arms::ArmsRoot;
use crate::combat::VIEW_LAYER;
use crate::gameplay::{HudMessages, PlayerStats};
use crate::interact::{InteractFocus, Interaction};
use crate::level::{GameAssets, LevelInfo, GROUP_WORLD};
use crate::npc::{FromSpawner, Mode, Npc, NpcVisual, ScriptedAnim};
use crate::player::{Player, PlayerCamera};
use crate::GameState;
use bevy::camera::visibility::{NoFrustumCulling, RenderLayers};
use bevy::mesh::skinning::SkinnedMesh;
use bevy::mesh::MeshTag;
use bevy::prelude::*;
use bevy_rapier3d::prelude::*;

pub struct CarryPlugin;

impl Plugin for CarryPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Carry>()
            .init_resource::<CarryRestore>()
            .add_systems(OnExit(GameState::InGame), |mut c: ResMut<Carry>| *c = Carry::default())
            .add_systems(Update, (carry_input, sync_body).chain().after(crate::save::restore_npcs).after(crate::interact::FocusSet).after(crate::powers::bolt_focus).after(crate::arms::ArmsAnimSet).run_if(in_state(GameState::InGame)))
            .add_systems(Update, prewarm_bodies.run_if(in_state(GameState::InGame)))
            .add_systems(PostUpdate, (place_body, fall).after(crate::arms::ArmsAlign).before(TransformSystems::Propagate))
            .add_systems(PostUpdate, log_joints.after(TransformSystems::Propagate));
    }
}

/// How a body leaves the shoulder (the original's drop clips).
#[derive(Clone, Copy, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize)]
pub enum DropKind {
    Low,
    LowSneak,
    NoMove,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, serde::Serialize, serde::Deserialize)]
pub enum CarryPhase {
    #[default]
    None,
    /// lifting it onto the shoulder
    In,
    Hold,
    Drop(DropKind),
}

/// The body on Corvo's shoulder.
#[derive(Resource, Default)]
pub struct Carry {
    pub body: Option<Entity>,
    pub phase: CarryPhase,
    /// bumped when a one-shot (lift, drop) starts, for the arms
    pub seq: u32,
    /// the body's foreground copies (drawn with the view model), and its world parts
    view_parts: Vec<Entity>,
    world_parts: Vec<Entity>,
    /// the NPC's visual root (rotation relative to the NPC)
    visual: Option<(Entity, Transform)>,
    /// its weapons (hidden meanwhile)
    swords: Vec<Entity>,
    spawner: Option<u32>,
    /// seconds into the current phase
    t: f32,
    /// Last master clip and cursor; retained across saves during lift/drop transitions.
    pub(crate) animation: Option<(String, f32)>,
    pub(crate) resume_animation: bool,
}

impl Carry {
    pub fn save(&self) -> Option<CarrySave> {
        self.body?;
        Some(CarrySave { spawner: self.spawner?, phase: self.phase, time: self.t, animation: self.animation.clone() })
    }
    pub fn carrying(&self) -> bool {
        self.body.is_some()
    }
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct CarrySave {
    spawner: u32,
    phase: CarryPhase,
    time: f32,
    #[serde(default)]
    animation: Option<(String, f32)>,
}

#[derive(Resource, Default)]
pub struct CarryRestore(pub Option<CarrySave>);

/// A body leaving the shoulder: it lies down where its hips are and falls to the ground
/// (thrown: along the view).
#[derive(Component, Clone, serde::Serialize, serde::Deserialize)]
pub(crate) struct Falling {
    vel: Vec3,
    placed: bool,
    /// Hip position sampled before changing to the lying animation.
    #[serde(default)]
    release_hips: Option<Vec3>,
}

/// Lying on its back (the pose of the levels' corpses).
pub(crate) fn lay_down(anim: &mut Animator) {
    if let Some(c) = anim.lib.first(&["Corpses_DeathPose_OnBack1", "Generic_DeathFrontA", "Generic_DeathFront_A", "Generic_DeathA"]) {
        // Flight uses a settled pose, including when the world is stopped. A
        // world-time crossfade here would preserve a half-carried pose indefinitely.
        anim.restart(c, false, 1.0, 0.0);
        anim.frozen = false;
        let d = anim.lib.duration(c);
        anim.seek(d);
    }
}

/// Bodies whose animations include being carried.
pub fn can_carry(anim: &Animator) -> bool {
    anim.lib.find("Corpses_CarryCorpse_Idle_Slave").is_some()
}

/// The name of the first-person master clip for a phase.
pub fn master_clip(phase: CarryPhase, moving: bool) -> Option<&'static str> {
    Some(match phase {
        CarryPhase::None => return None,
        CarryPhase::In => "Empty_CarryCorpse_In_Master",
        CarryPhase::Hold if moving => "Empty_CarryCorpse_Walk_Master",
        CarryPhase::Hold => "Empty_CarryCorpse_Idle_Master",
        CarryPhase::Drop(DropKind::Low) => "Empty_CarryCorpse_DropLow_Master",
        CarryPhase::Drop(DropKind::LowSneak) => "Empty_CarryCorpse_DropLowSneak_Master",
        CarryPhase::Drop(DropKind::NoMove) => "Empty_CarryCorpse_DropNoMove_Master",
    })
}

#[allow(clippy::too_many_arguments, clippy::type_complexity)]
fn carry_input(
    mut commands: Commands,
    paused: Res<crate::hud::Paused>,
    time: Res<Time>,
    (keys, mouse, bind): (Res<ButtonInput<KeyCode>>, Res<ButtonInput<MouseButton>>, Res<crate::bindings::Bindings>),
    focus: Res<InteractFocus>,
    mut carry: ResMut<Carry>,
    (level, assets, mut restore): (Option<Res<LevelInfo>>, Option<Res<GameAssets>>, ResMut<CarryRestore>),
    (stats, mut msgs): (Res<PlayerStats>, ResMut<HudMessages>),
    player: Query<(&Player, &Transform)>,
    cam: Query<&GlobalTransform, With<PlayerCamera>>,
    npcs: Query<(Entity, &Npc, &FromSpawner, &Children), Without<crate::npc::ConsumedBody>>,
    visuals: Query<(&Transform, &Children), With<NpcVisual>>,
    parts: Query<(&SkinnedMesh, &MeshTag), With<Mesh3d>>,
    mut vis: Query<&mut Visibility>,
    mut used: MessageWriter<Interaction>,
    (arms, mut anims, npc_anims, rigs, joints): (Query<&Animator, With<ArmsRoot>>, Query<&mut Animator, Without<ArmsRoot>>, Query<&crate::npc::NpcAnim>, Query<&crate::npc::NpcRig>, Query<&GlobalTransform>),
) {
    if paused.0 { return; }
    let dt = time.delta_secs();
    carry.t += dt;
    let Ok((p, _)) = player.single() else { return };
    // the body went away (streamed out, scripts) or Corvo died
    if let Some(b) = carry.body {
        if npcs.get(b).is_err() || stats.dead {
            release(&mut commands, &mut carry, &mut vis, None);
            return;
        }
    }
    let arms_done = arms.single().map(|a| a.finished()).unwrap_or(true);
    let release_hips = |body| rigs.get(body).ok().and_then(|r| r.joint("Root_jnt"))
        .and_then(|joint| joints.get(joint).ok()).map(GlobalTransform::translation);
    match carry.phase {
        CarryPhase::None => {
            // pick up the body looked at
            let restoring = restore.0.as_ref();
            let target = if let Some(saved) = restoring {
                npcs.iter().find(|(_, npc, from, _)| from.0 == saved.spawner && npc.is_down()).map(|(e, _, _, _)| e)
            } else { focus.1.filter(|_| keys.just_pressed(bind.key(crate::bindings::Act::Use))) };
            let Some(e) = target else { return };
            let Ok((_, npc, from, children)) = npcs.get(e) else { return };
            if !npc.is_down() || p.locked || stats.dead {
                return;
            }
            let (Some(level), Some(assets)) = (level.as_ref(), assets.as_ref()) else { return };
            let Some(visual) = children.iter().find(|c| visuals.contains(*c)) else { return };
            let Ok((_, vchildren)) = visuals.get(visual) else { return };
            // the body's skeleton upright (lying characters tilt their visual root)
            let vt = Transform::from_rotation(Quat::from_rotation_y(std::f32::consts::FRAC_PI_2));
            if !anims.get(e).is_ok_and(can_carry) {
                return;
            }
            let world_parts: Vec<Entity> = vchildren.iter().filter(|c| parts.contains(*c)).collect();
            let Some(Ok((skin, tag))) = world_parts.first().map(|c| parts.get(*c)) else { return };
            let Some(Some(nv)) = level.scene.spawners.get(from.0 as usize).and_then(|s| s.npc_type).and_then(|t| assets.npc_types.get(t as usize)) else { return };
            // the body's foreground copy, on the same skeleton
            let mut view_parts = Vec::new();
            let render_parts = if std::env::var_os("DH_CARRY_WORLD_MATERIAL").is_some() { &nv.parts.parts } else { &nv.parts.view_parts };
            for (mesh, mat) in render_parts {
                if std::env::var_os("DH_CARRY_HIDE_VIEW").is_some() { break; }
                let mut ec = commands.spawn((
                    Mesh3d(mesh.clone()),
                    tag.clone(),
                    SkinnedMesh { inverse_bindposes: skin.inverse_bindposes.clone(), joints: skin.joints.clone() },
                    bevy::camera::visibility::DynamicSkinnedMeshBounds,
                    RenderLayers::layer(VIEW_LAYER),
                    NoFrustumCulling,
                    bevy::light::NotShadowCaster,
                    crate::fxlight::LitPart,
                    ChildOf(visual),
                ));
                mat.try_apply(&mut ec);
                view_parts.push(ec.id());
            }
            let swords: Vec<Entity> = npc_anims.get(e).map(|a| a.swords().to_vec()).unwrap_or_default();
            for &w in world_parts.iter().chain(swords.iter()) {
                if let Ok(mut v) = vis.get_mut(w) {
                    *v = Visibility::Hidden;
                }
            }
            commands.entity(e).try_insert(ScriptedAnim);
            let phase = restoring.map(|s| s.phase).unwrap_or(CarryPhase::In);
            let time = restoring.map(|s| s.time).unwrap_or(0.0);
            let animation = restoring.and_then(|s| s.animation.clone());
            let resume_animation = restoring.is_some();
            *carry = Carry { body: Some(e), phase, seq: carry.seq + 1, view_parts, world_parts, visual: Some((visual, vt)), swords, spawner: Some(from.0), t: time, animation, resume_animation };
            if restore.0.take().is_none() {
                used.write(Interaction::Corpse { spawner: from.0, what: 0 });
            }
            let _ = &mut msgs;
        }
        CarryPhase::In => {
            if carry.t > 0.3 && arms_done {
                carry.phase = CarryPhase::Hold;
                carry.t = 0.0;
            }
        }
        CarryPhase::Hold => {
            let drop = keys.just_pressed(bind.key(crate::bindings::Act::Use));
            let throw = mouse.just_pressed(MouseButton::Left);
            if !(drop || throw) || p.locked {
                return;
            }
            if throw {
                // off the shoulder along the view
                let dir = cam.single().map(|c| c.forward().as_vec3()).unwrap_or(Vec3::NEG_Z);
                let Some(b) = carry.body else { return };
                let hips = release_hips(b);
                if std::env::var_os("DH_CARRY_LOG").is_some() {
                    info!("carry: sampled release hips {hips:?} before throw pose");
                }
                commands.entity(b).try_insert(Falling { vel: dir * 6.0 + Vec3::Y * 1.5, placed: false, release_hips: hips });
                if let Ok(mut a) = anims.get_mut(b) {
                    lay_down(&mut a);
                }
                let spawner = carry.spawner;
                release(&mut commands, &mut carry, &mut vis, None);
                if let Some(s) = spawner {
                    used.write(Interaction::Corpse { spawner: s, what: 1 });
                }
                return;
            }
            let kind = if p.crouched { DropKind::LowSneak } else if p.velocity.length() < 0.5 { DropKind::NoMove } else { DropKind::Low };
            carry.phase = CarryPhase::Drop(kind);
            carry.seq += 1;
            carry.t = 0.0;
        }
        CarryPhase::Drop(_) => {
            if carry.t > 0.3 && arms_done {
                let spawner = carry.spawner;
                let b = carry.body;
                release(&mut commands, &mut carry, &mut vis, None);
                // it settles on whatever is below
                if let Some(b) = b {
                    commands.entity(b).try_insert(Falling { vel: Vec3::ZERO, placed: false, release_hips: release_hips(b) });
                    if let Ok(mut a) = anims.get_mut(b) {
                        lay_down(&mut a);
                    }
                }
                if let Some(s) = spawner {
                    used.write(Interaction::Corpse { spawner: s, what: 1 });
                }
            }
        }
    }
}

/// The body leaves the view model: back to its world parts.
fn release(commands: &mut Commands, carry: &mut Carry, vis: &mut Query<&mut Visibility>, _at: Option<Vec3>) {
    for &e in &carry.view_parts {
        commands.entity(e).try_despawn();
    }
    for &w in carry.world_parts.iter().chain(carry.swords.iter()) {
        if let Ok(mut v) = vis.get_mut(w) {
            *v = Visibility::Inherited;
        }
    }
    if let Some(b) = carry.body {
        commands.entity(b).try_remove::<ScriptedAnim>();
    }
    let seq = carry.seq;
    *carry = Carry { seq, ..default() };
}

/// A carried body's foreground copy drawn during the shader warm-up (under the loading
/// screen), so that its pipelines are ready before the first body is picked up.
#[derive(Component)]
struct Prewarm;

#[allow(clippy::type_complexity)]
fn prewarm_bodies(
    mut commands: Commands,
    warm: Option<Res<crate::warmup::Warmup>>,
    mut done: Local<bool>,
    (level, assets): (Option<Res<LevelInfo>>, Option<Res<GameAssets>>),
    npcs: Query<(&FromSpawner, &Children, &Animator), With<Npc>>,
    visuals: Query<&Children, With<NpcVisual>>,
    parts: Query<(&SkinnedMesh, &MeshTag), With<Mesh3d>>,
    prewarmed: Query<Entity, With<Prewarm>>,
) {
    match warm {
        Some(_) if !*done => {
            *done = true;
            let (Some(level), Some(assets)) = (level, assets) else { return };
            let mut seen = std::collections::HashSet::new();
            for (from, children, anim) in &npcs {
                let Some(t) = level.scene.spawners.get(from.0 as usize).and_then(|s| s.npc_type) else { continue };
                if !can_carry(anim) || !seen.insert(t) {
                    continue;
                }
                let Some(Some(nv)) = assets.npc_types.get(t as usize) else { continue };
                let Some(visual) = children.iter().find(|c| visuals.contains(*c)) else { continue };
                let Some(Ok((skin, tag))) = visuals.get(visual).ok().and_then(|vc| vc.iter().find(|c| parts.contains(*c))).map(|c| parts.get(c)) else { continue };
                for (mesh, mat) in &nv.parts.view_parts {
                    let mut ec = commands.spawn((
                        Prewarm,
                        Mesh3d(mesh.clone()),
                        tag.clone(),
                        SkinnedMesh { inverse_bindposes: skin.inverse_bindposes.clone(), joints: skin.joints.clone() },
                        bevy::camera::visibility::DynamicSkinnedMeshBounds,
                        RenderLayers::layer(VIEW_LAYER),
                        NoFrustumCulling,
                        bevy::light::NotShadowCaster,
                        crate::fxlight::LitPart,
                        ChildOf(visual),
                    ));
                    mat.try_apply(&mut ec);
                }
            }
        }
        None if *done => {
            for e in &prewarmed {
                commands.entity(e).try_despawn();
            }
            if prewarmed.is_empty() {
                *done = false;
            }
        }
        _ => {}
    }
}

/// The body plays the slave clip of what the arms play, at the same time.
fn sync_body(mut carry: ResMut<Carry>, arms: Query<&Animator, With<ArmsRoot>>, mut bodies: Query<&mut Animator, Without<ArmsRoot>>) {
    // The arms consume the restored cursor on their next update. Do not overwrite it
    // with the unrelated clip that was playing when the NPC was restored.
    if carry.resume_animation { return; }
    let (Some(b), Ok(arms)) = (carry.body, arms.single()) else { return };
    let Ok(mut anim) = bodies.get_mut(b) else { return };
    let Some(cur) = arms.current() else { return };
    let name = &arms.lib.clip(cur.clip).name;
    let Some(rest) = name.strip_prefix("Empty_CarryCorpse_") else { return };
    carry.animation = Some((name.clone(), cur.t));
    let slave = format!("Corpses_CarryCorpse_{}", rest.replace("_Master", "_Slave"));
    let Some(clip) = anim.lib.find(&slave) else {
        if std::env::var("DH_CARRY_LOG").is_ok() {
            warn!("carry: no {slave} for the body");
        }
        return;
    };
    // ScriptedAnim suspends NPC animation updates while carried. Replace their
    // last world-time/LOD state so both paired clips advance and blend together.
    anim.time_scale = arms.time_scale;
    anim.frozen = false;
    if anim.current().map(|c| c.clip) != Some(clip) {
        anim.restart(clip, cur.looping, cur.speed, 0.15);
        if std::env::var("DH_CARRY_LOG").is_ok() {
            info!("carry: body plays {slave} ({:.2}s) at {:.2} with {name}", anim.lib.duration(clip), cur.t);
        }
    }
    anim.seek(cur.t);
}

/// Debug: where the carried body's joints are in camera space (`DH_CARRY_LOG`).
fn log_joints(carry: Res<Carry>, rigs: Query<&crate::npc::NpcRig>, g: Query<(&GlobalTransform, Option<&Name>)>, cam: Query<&GlobalTransform, With<PlayerCamera>>, arms: Query<&Children, With<ArmsRoot>>, kids: Query<&Children>, mut n: Local<u32>) {
    if std::env::var("DH_CARRY_LOG").is_err() {
        return;
    }
    let (Some(b), Ok(c)) = (carry.body, cam.single()) else { return };
    *n += 1;
    if *n % 3 != 0 || *n > 120 {
        return;
    }
    let Ok(rig) = rigs.get(b) else { return };
    let inv = c.affine().inverse();
    let at = |name: &str| rig.joint(name).and_then(|j| g.get(j).ok()).map(|j| inv.transform_point3(j.0.translation()));
    info!("carry joints (camera space, -z ahead): pelvis {:.2?} head {:.2?} knee_l {:.2?} knee_r {:.2?} {:?}", at("Root_jnt"), at("head_jnt"), at("lower_leg_L_jnt"), at("lower_leg_R_jnt"), carry.phase);
    // Corvo's hands (named arm joints)
    let mut stack: Vec<Entity> = arms.iter().flat_map(|c| c.iter()).collect();
    let mut out = Vec::new();
    while let Some(e) = stack.pop() {
        if let Ok((gt, Some(name))) = g.get(e) {
            if name.as_str().eq_ignore_ascii_case("hand_L_jnt") || name.as_str().eq_ignore_ascii_case("hand_R_jnt") || name.as_str().eq_ignore_ascii_case("anchor_jnt") || name.as_str().eq_ignore_ascii_case("root0_jnt") {
                out.push(format!("{} {:.2}", name, inv.transform_point3(gt.translation())));
            }
        }
        if let Ok(c) = kids.get(e) {
            stack.extend(c.iter());
        }
    }
    info!("carry: corvo {:?}", out);
}

/// The slave clip's anchor locates the carrier, rather than the body's mesh origin.
#[allow(clippy::type_complexity)]
fn place_body(
    carry: Res<Carry>,
    player: Query<&Transform, (With<Player>, Without<Npc>)>,
    cam: Query<&Transform, (With<PlayerCamera>, Without<Npc>, Without<Player>)>,
    arms: Query<(&Transform, &Animator), (With<ArmsRoot>, Without<Npc>, Without<Player>, Without<PlayerCamera>)>,
    rigs: Query<(&Animator, &crate::npc::NpcRig), Without<ArmsRoot>>,
    mut bodies: Query<&mut Transform, (With<Npc>, Without<Player>, Without<PlayerCamera>, Without<ArmsRoot>, Without<NpcVisual>)>,
    mut visuals: Query<&mut Transform, (With<NpcVisual>, Without<Npc>, Without<Player>, Without<PlayerCamera>, Without<ArmsRoot>)>,
) {
    let (Some(b), Some((visual, vt))) = (carry.body, carry.visual) else { return };
    if let Ok(mut v) = visuals.get_mut(visual) {
        *v = vt;
    }
    let (Ok(pt), Ok(ct), Ok((at, master))) = (player.single(), cam.single(), arms.single()) else { return };
    let Ok(mut t) = bodies.get_mut(b) else { return };
    let alignment = rigs.get(b).ok()
        .and_then(|(anim, rig)| rig.index("anchor_jnt").map(|i| carry_alignment(master.model(0), anim.model(i))))
        .unwrap_or(Mat4::IDENTITY);
    let root = pt.to_matrix() * ct.to_matrix() * at.to_matrix() * alignment * vt.to_matrix().inverse();
    let (_, r, p) = root.to_scale_rotation_translation();
    t.translation = p;
    t.rotation = r;
    if std::env::var("DH_CARRY_LOG").is_ok() {
        info!("carry: player {:.2} body {:.2} arms root local {:.2} rot {:.2?} {:?}", pt.translation, p, at.translation, at.rotation.to_euler(EulerRot::YXZ), carry.phase);
    }
}

fn carry_alignment(carrier_root: Transform, slave_anchor: Transform) -> Mat4 {
    carrier_root.to_matrix() * slave_anchor.to_matrix().inverse()
}

/// Sweep the body's width through its horizontal flight, including thin obstacles
/// that lie entirely between frame endpoints. Gravity/ground settling stays below.
fn body_flight_step(ctx: &RapierContext, entity: Entity, at: Vec3, velocity: &mut Vec3, dt: f32) -> Vec3 {
    let mut step = *velocity * dt;
    let horizontal = step.with_y(0.0);
    if horizontal.length_squared() <= 1e-10 { return step; }
    let shape = Collider::ball(crate::npc::NPC_RADIUS);
    let filter = QueryFilter::default().exclude_collider(entity).exclude_sensors()
        .groups(CollisionGroups::new(Group::ALL, GROUP_WORLD | crate::level::GROUP_PROP));
    if let Some((obstacle, hit)) = ctx.cast_shape(at, Quat::IDENTITY, horizontal, shape.raw.as_ref(),
        ShapeCastOptions { max_time_of_impact: 1.0, target_distance: 0.01, stop_at_penetration: false, ..default() }, filter) {
        let fraction = hit.time_of_impact.clamp(0.0, 1.0);
        step.x *= fraction;
        step.z *= fraction;
        velocity.x = 0.0;
        velocity.z = 0.0;
        if std::env::var_os("DH_CARRY_LOG").is_some() {
            info!("carry: body {entity:?} blocked by {obstacle:?} at flight fraction {fraction:.3}");
        }
    }
    step
}

fn body_ground_hit(ctx: &RapierContext, at: Vec3, step: Vec3) -> Option<(Entity, f32)> {
    // A nearby floor is support only when descending. Testing it during ascent
    // would cancel the upward impulse before the body could leave the ground.
    if step.y > 0.0 { return None; }
    let filter = QueryFilter::default().groups(CollisionGroups::new(Group::ALL, GROUP_WORLD));
    ctx.cast_ray(at + Vec3::new(step.x, 0.0, step.z), Vec3::NEG_Y,
        crate::npc::NPC_CENTER - step.y + 0.05, true, filter)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flight_pose_settles_even_with_stopped_time_and_a_previously_frozen_rig() {
        use dhcook::format::{AnimClip, AnimFile, BoneDef, KeyTrack, SkeletonDef};
        use std::sync::Arc;
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, crate::anim::AnimPlugin)).add_message::<crate::audio::PostEvent>();
        let skeleton = SkeletonDef {
            bones: vec![BoneDef { name: "root".into(), parent: -1, rotation: Quat::IDENTITY.to_array(), ..default() }], ..default()
        };
        let lib = Arc::new(crate::anim::CharAnims::new(&skeleton, vec![Arc::new(AnimFile {
            bones: vec!["root".into()],
            clips: vec![
                AnimClip { name: "carried".into(), duration: 1.0, rate: 30.0, frames: 30, ..default() },
                AnimClip { name: "Corpses_DeathPose_OnBack1".into(), duration: 1.0, rate: 30.0, frames: 30,
                    translations: vec![KeyTrack { bone: 0, frames: vec![0, 29], values: vec![[0.0; 3], [0.0, -1.0, 0.0]] }], ..default() },
            ],
        })]));
        let joint = app.world_mut().spawn(Transform::default()).id();
        let mut anim = Animator::new(lib.clone(), &skeleton, vec![joint]);
        anim.restart(lib.find("carried").unwrap(), true, 1.0, 0.0);
        anim.time_scale = 0.0;
        anim.frozen = true;
        lay_down(&mut anim);
        app.world_mut().spawn((anim, GlobalTransform::default()));
        app.update();
        assert_eq!(app.world().get::<Transform>(joint).unwrap().translation, Vec3::NEG_Y);
    }

    #[test]
    fn thrown_body_leaves_nearby_floor_before_landing_on_descent() {
        #[derive(Resource)]
        struct Flight { position: Vec3, velocity: Vec3, peak: f32, landed: bool }
        let start = crate::npc::NPC_CENTER + 0.02;
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, AssetPlugin::default(), bevy::scene::ScenePlugin, TransformPlugin, RapierPhysicsPlugin::<NoUserData>::default()))
            .init_resource::<Assets<Mesh>>()
            .insert_resource(Flight { position: Vec3::Y * start, velocity: Vec3::new(6.0, 1.5, 0.0), peak: start, landed: false })
            .add_systems(Last, |ctx: ReadRapierContext, mut flight: ResMut<Flight>| {
                if flight.landed { return; }
                flight.velocity.y -= 9.8 * 0.02;
                let step = flight.velocity * 0.02;
                if let Some((_, distance)) = body_ground_hit(&ctx.single().unwrap(), flight.position, step) {
                    flight.position += step.with_y(0.0);
                    flight.position.y += crate::npc::NPC_CENTER + 0.05 - distance;
                    flight.landed = true;
                } else { flight.position += step; }
                flight.peak = flight.peak.max(flight.position.y);
            });
        app.world_mut().spawn((Collider::cuboid(20.0, 0.1, 20.0), Transform::from_xyz(0.0, -0.1, 0.0), CollisionGroups::new(GROUP_WORLD, Group::ALL)));
        app.update();
        assert!(!app.world().resource::<Flight>().landed);
        for _ in 0..30 { app.update(); }
        let flight = app.world().resource::<Flight>();
        assert!(flight.landed);
        assert!(flight.peak > start + 0.08, "upward throw must clear its starting floor");
        assert!(flight.position.x > 1.0);
        assert!((flight.position.y - (crate::npc::NPC_CENTER + 0.05)).abs() < 1e-5);
    }

    #[test]
    fn thrown_body_sweep_blocks_thin_cover_and_respects_removal() {
        #[derive(Resource, Default)]
        struct FlightResult(Vec3, Vec3, Vec3);
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, AssetPlugin::default(), bevy::scene::ScenePlugin, TransformPlugin, RapierPhysicsPlugin::<NoUserData>::default()))
            .init_resource::<Assets<Mesh>>()
            .init_resource::<FlightResult>()
            .add_systems(Last, |ctx: ReadRapierContext, mut result: ResMut<FlightResult>| {
                let mut velocity = Vec3::new(60.0, -2.0, 0.0);
                let step = body_flight_step(&ctx.single().unwrap(), Entity::PLACEHOLDER, Vec3::ZERO, &mut velocity, 0.05);
                let mut away_velocity = Vec3::NEG_X;
                let away = body_flight_step(&ctx.single().unwrap(), Entity::PLACEHOLDER, Vec3::X * 0.8, &mut away_velocity, 0.05);
                *result = FlightResult(step, velocity, away);
            });
        app.update();
        assert_eq!(app.world().resource::<FlightResult>().0.x, 3.0);
        for group in [GROUP_WORLD, crate::level::GROUP_PROP] {
            let wall = app.world_mut().spawn((Collider::cuboid(0.01, 2.0, 2.0), Transform::from_xyz(1.0, 0.0, 0.0), CollisionGroups::new(group, Group::ALL))).id();
            app.update();
            let result = app.world().resource::<FlightResult>();
            assert!(result.0.x > 0.6 && result.0.x < 0.66, "body must stop before its radius overlaps cover: {}", result.0.x);
            assert_eq!(result.0.y, -0.1);
            assert_eq!(result.1, Vec3::new(0.0, -2.0, 0.0));
            assert_eq!(result.2.x, -0.05, "a release already touching cover can move away from it");
            app.world_mut().despawn(wall);
            app.update();
            assert_eq!(app.world().resource::<FlightResult>().0.x, 3.0);
        }
        app.world_mut().spawn((Collider::cuboid(0.01, 2.0, 2.0), Sensor, Transform::from_xyz(1.0, 0.0, 0.0), CollisionGroups::new(GROUP_WORLD, Group::ALL)));
        app.update();
        assert_eq!(app.world().resource::<FlightResult>().0.x, 3.0);
    }

    #[test]
    fn carried_pose_blends_on_carrier_time_even_when_npc_was_frozen() {
        use dhcook::format::{AnimClip, AnimFile, BoneDef, KeyTrack, SkeletonDef};
        use std::sync::Arc;
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, crate::anim::AnimPlugin))
            .add_message::<crate::audio::PostEvent>()
            .insert_resource(bevy::time::TimeUpdateStrategy::ManualDuration(std::time::Duration::from_millis(50)))
            .add_systems(Update, sync_body);
        let skeleton = SkeletonDef {
            bones: vec![BoneDef { name: "root".into(), parent: -1, rotation: Quat::IDENTITY.to_array(), ..Default::default() }],
            ..Default::default()
        };
        let clip = |name: &str, y: f32| AnimClip {
            name: name.into(), duration: 2.0, rate: 30.0, frames: 60,
            translations: vec![KeyTrack { bone: 0, frames: vec![0], values: vec![[0.0, y, 0.0]] }],
            ..Default::default()
        };
        let lib = Arc::new(crate::anim::CharAnims::new(&skeleton, vec![Arc::new(AnimFile {
            bones: vec!["root".into()],
            clips: vec![clip("Empty_CarryCorpse_Idle_Master", 2.0), clip("Corpses_CarryCorpse_Idle_Slave", 2.0), clip("dead", 0.0)],
        })]));
        let hand = app.world_mut().spawn(Transform::default()).id();
        let joint = app.world_mut().spawn(Transform::default()).id();
        let mut master = Animator::new(lib.clone(), &skeleton, vec![hand]);
        master.restart(lib.find("Empty_CarryCorpse_Idle_Master").unwrap(), true, 1.0, 0.0);
        let master_entity = app.world_mut().spawn((ArmsRoot, master, GlobalTransform::default())).id();
        let mut slave = Animator::new(lib.clone(), &skeleton, vec![joint]);
        slave.restart(lib.find("dead").unwrap(), false, 1.0, 0.0);
        slave.time_scale = 0.0;
        slave.frozen = true;
        let body = app.world_mut().spawn((slave, GlobalTransform::default())).id();
        app.insert_resource(Carry { body: Some(body), phase: CarryPhase::Hold, ..Default::default() });
        for _ in 0..8 { app.update(); }
        let world = app.world();
        let master = world.get::<Animator>(master_entity).unwrap();
        let slave = world.get::<Animator>(body).unwrap();
        assert!((master.current().unwrap().t - slave.current().unwrap().t).abs() < 1e-6);
        assert!((world.get::<Transform>(joint).unwrap().translation.y - 2.0).abs() < 1e-6);
    }

    #[test]
    fn transition_save_retains_cursor_and_accepts_legacy_carry_state() {
        let legacy = r#"{"spawner":40,"phase":{"Drop":"LowSneak"},"time":0.31}"#;
        let saved: CarrySave = serde_json::from_str(legacy).unwrap();
        assert!(saved.animation.is_none());
        let carry = Carry {
            body: Some(Entity::PLACEHOLDER), spawner: Some(saved.spawner),
            phase: saved.phase, t: saved.time,
            animation: Some(("Empty_CarryCorpse_DropLowSneak_Master".into(), 0.30)),
            ..Default::default()
        };
        let encoded = serde_json::to_string(&carry.save().unwrap()).unwrap();
        let restored: CarrySave = serde_json::from_str(&encoded).unwrap();
        assert_eq!(restored.phase, CarryPhase::Drop(DropKind::LowSneak));
        assert_eq!(restored.animation, carry.animation);
        assert_eq!(restored.time, carry.t);
    }

    #[test]
    fn flight_save_accepts_legacy_state_and_retains_pending_release_origin() {
        let legacy: Falling = serde_json::from_str(r#"{"vel":[6.0,1.5,0.0],"placed":false}"#).unwrap();
        assert!(legacy.release_hips.is_none());
        let pending = Falling { release_hips: Some(Vec3::new(13.0, 30.0, -1.0)), ..legacy };
        let restored: Falling = serde_json::from_str(&serde_json::to_string(&pending).unwrap()).unwrap();
        assert!(!restored.placed);
        assert_eq!(restored.release_hips, pending.release_hips);
        assert_eq!(restored.vel, pending.vel);
    }

    #[test]
    fn carried_anchor_matches_carrier_through_camera_and_visual_transforms() {
        let camera = Transform::from_xyz(12.0, 31.0, -8.0)
            .with_rotation(Quat::from_euler(EulerRot::YXZ, -1.58, 0.6, 0.0)).to_matrix();
        let arms = Transform::from_xyz(0.0, -0.2, 0.1).to_matrix();
        let visual = Transform::from_rotation(Quat::from_rotation_y(std::f32::consts::FRAC_PI_2)).to_matrix();
        let carrier = Transform::from_xyz(0.0, -0.875, 0.0)
            .with_rotation(Quat::from_rotation_z(std::f32::consts::FRAC_PI_2));
        let anchor = Transform::from_xyz(0.3, -0.97, 0.23)
            .with_rotation(Quat::from_euler(EulerRot::XYZ, 2.93, 0.21, 1.57));
        let body = camera * arms * carry_alignment(carrier, anchor) * visual.inverse();
        let actual = body * visual * anchor.to_matrix();
        let expected = camera * arms * carrier.to_matrix();
        assert!(actual.abs_diff_eq(expected, 1e-5));
        assert!(!(camera * arms * anchor.to_matrix()).abs_diff_eq(expected, 0.01));
    }
}

/// Bodies fall until they meet the ground (thrown, or dropped over an edge); the unconscious
/// drown in water.
#[allow(clippy::type_complexity)]
fn fall(
    mut commands: Commands,
    time: Res<Time>,
    tc: Res<crate::gameplay::TimeControl>,
    rapier: ReadRapierContext,
    waters: Option<Res<crate::swim::Waters>>,
    mut stats: ResMut<PlayerStats>,
    mut msgs: ResMut<HudMessages>,
    mut q: Query<(Entity, &mut Transform, &mut Falling, &mut Npc, Option<&crate::npc::NpcRig>)>,
    joints: Query<&GlobalTransform>,
) {
    let dt = time.delta_secs().min(0.05) * tc.world_scale();
    let Ok(ctx) = rapier.single() else { return };
    for (e, mut t, mut f, mut npc, rig) in &mut q {
        if !f.placed {
            // lying where the hips were, level
            f.placed = true;
            let hips = f.release_hips.or_else(|| rig.and_then(|r| r.joint("Root_jnt"))
                .and_then(|j| joints.get(j).ok()).map(GlobalTransform::translation));
            if let Some(hips) = hips {
                t.translation = hips + Vec3::Y * 0.2;
            }
            let fwd = t.rotation * Vec3::NEG_Z;
            let yaw = (-fwd.x).atan2(-fwd.z);
            t.rotation = Quat::from_rotation_y(yaw);
            npc.yaw = yaw;
            if std::env::var("DH_CARRY_LOG").is_ok() {
                info!("carry: released body {e:?} at {:?}, velocity {:?}, world scale {}", t.translation, f.vel, tc.world_scale());
            }
        }
        if dt <= 0.0 { continue; }
        f.vel.y -= 9.8 * dt;
        let step = body_flight_step(&ctx, e, t.translation, &mut f.vel, dt);
        let hit = body_ground_hit(&ctx, t.translation, step);
        let in_water = waters.as_ref().and_then(|w| w.at(t.translation - Vec3::Y * 0.6));
        if hit.is_some() || in_water.is_some() {
            if std::env::var_os("DH_CARRY_LOG").is_some() {
                info!("carry: body {e:?} settled from {:?} with velocity {:?}, water {}", t.translation, f.vel, in_water.is_some());
            }
            if let Some((_, toi)) = hit {
                t.translation += Vec3::new(step.x, 0.0, step.z);
                t.translation.y = t.translation.y - toi + crate::npc::NPC_CENTER + 0.05;
            }
            commands.entity(e).try_remove::<Falling>();
            // the unconscious drown (the original counts it as a kill)
            if in_water.is_some() && npc.mode == Mode::Unconscious {
                npc.set_mode(Mode::Dead);
                stats.kills += 1;
                stats.knockouts = stats.knockouts.saturating_sub(1);
                msgs.push(format!("{} drowned", npc.name));
            }
        } else {
            t.translation += step;
        }
        if t.translation.y < -500.0 {
            commands.entity(e).try_remove::<Falling>();
        }
    }
}
