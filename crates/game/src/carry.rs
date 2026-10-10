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
            .add_systems(OnExit(GameState::InGame), |mut c: ResMut<Carry>| *c = Carry::default())
            .add_systems(Update, (carry_input, sync_body).chain().after(crate::interact::FocusSet).after(crate::powers::bolt_focus).after(crate::arms::ArmsAnimSet).run_if(in_state(GameState::InGame)))
            .add_systems(Update, prewarm_bodies.run_if(in_state(GameState::InGame)))
            .add_systems(PostUpdate, (place_body, fall).after(crate::arms::ArmsAlign).before(TransformSystems::Propagate))
            .add_systems(PostUpdate, log_joints.after(TransformSystems::Propagate));
    }
}

/// How a body leaves the shoulder (the original's drop clips).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DropKind {
    Low,
    LowSneak,
    NoMove,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
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
}

impl Carry {
    pub fn carrying(&self) -> bool {
        self.body.is_some()
    }
}

/// A body leaving the shoulder: it lies down where its hips are and falls to the ground
/// (thrown: along the view).
#[derive(Component)]
struct Falling {
    vel: Vec3,
    placed: bool,
}

/// Lying on its back (the pose of the levels' corpses).
fn lay_down(anim: &mut Animator) {
    if let Some(c) = anim.lib.first(&["Corpses_DeathPose_OnBack1", "Generic_DeathFrontA", "Generic_DeathFront_A", "Generic_DeathA"]) {
        anim.restart(c, false, 1.0, 0.2);
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
    (level, assets): (Option<Res<LevelInfo>>, Option<Res<GameAssets>>),
    (stats, mut msgs): (Res<PlayerStats>, ResMut<HudMessages>),
    player: Query<(&Player, &Transform)>,
    cam: Query<&GlobalTransform, With<PlayerCamera>>,
    npcs: Query<(&Npc, &FromSpawner, &Children)>,
    visuals: Query<(&Transform, &Children), With<NpcVisual>>,
    parts: Query<(&SkinnedMesh, &MeshTag), With<Mesh3d>>,
    mut vis: Query<&mut Visibility>,
    mut used: MessageWriter<Interaction>,
    (arms, mut anims, npc_anims): (Query<&Animator, With<ArmsRoot>>, Query<&mut Animator, Without<ArmsRoot>>, Query<&crate::npc::NpcAnim>),
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
    match carry.phase {
        CarryPhase::None => {
            // pick up the body looked at
            let Some(e) = focus.1.filter(|_| keys.just_pressed(bind.key(crate::bindings::Act::Use))) else { return };
            let Ok((npc, from, children)) = npcs.get(e) else { return };
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
            for (mesh, mat) in &nv.parts.view_parts {
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
            *carry = Carry { body: Some(e), phase: CarryPhase::In, seq: carry.seq + 1, view_parts, world_parts, visual: Some((visual, vt)), swords, spawner: Some(from.0), t: 0.0 };
            used.write(Interaction::Corpse { spawner: from.0, what: 0 });
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
                commands.entity(b).try_insert(Falling { vel: dir * 6.0 + Vec3::Y * 1.5, placed: false });
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
                    commands.entity(b).try_insert(Falling { vel: Vec3::ZERO, placed: false });
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
fn sync_body(carry: Res<Carry>, arms: Query<&Animator, With<ArmsRoot>>, mut bodies: Query<&mut Animator, Without<ArmsRoot>>) {
    let (Some(b), Ok(arms)) = (carry.body, arms.single()) else { return };
    let Ok(mut anim) = bodies.get_mut(b) else { return };
    let Some(cur) = arms.current() else { return };
    let name = &arms.lib.clip(cur.clip).name;
    let Some(rest) = name.strip_prefix("Empty_CarryCorpse_") else { return };
    let slave = format!("Corpses_CarryCorpse_{}", rest.replace("_Master", "_Slave"));
    let Some(clip) = anim.lib.find(&slave) else {
        if std::env::var("DH_CARRY_LOG").is_ok() {
            warn!("carry: no {slave} for the body");
        }
        return;
    };
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

/// The carried body's skeleton sits where the first-person rig's root is.
#[allow(clippy::type_complexity)]
fn place_body(
    carry: Res<Carry>,
    player: Query<&Transform, (With<Player>, Without<Npc>)>,
    cam: Query<&Transform, (With<PlayerCamera>, Without<Npc>, Without<Player>)>,
    arms: Query<&Transform, (With<ArmsRoot>, Without<Npc>, Without<Player>, Without<PlayerCamera>)>,
    mut bodies: Query<&mut Transform, (With<Npc>, Without<Player>, Without<PlayerCamera>, Without<ArmsRoot>, Without<NpcVisual>)>,
    mut visuals: Query<&mut Transform, (With<NpcVisual>, Without<Npc>, Without<Player>, Without<PlayerCamera>, Without<ArmsRoot>)>,
) {
    let (Some(b), Some((visual, vt))) = (carry.body, carry.visual) else { return };
    if let Ok(mut v) = visuals.get_mut(visual) {
        *v = vt;
    }
    let (Ok(pt), Ok(ct), Ok(at)) = (player.single(), cam.single(), arms.single()) else { return };
    let Ok(mut t) = bodies.get_mut(b) else { return };
    let root = pt.to_matrix() * ct.to_matrix() * at.to_matrix() * vt.to_matrix().inverse();
    let (_, r, p) = root.to_scale_rotation_translation();
    t.translation = p;
    t.rotation = r;
    if std::env::var("DH_CARRY_LOG").is_ok() {
        info!("carry: player {:.2} body {:.2} arms root local {:.2} rot {:.2?} {:?}", pt.translation, p, at.translation, at.rotation.to_euler(EulerRot::YXZ), carry.phase);
    }
}

/// Bodies fall until they meet the ground (thrown, or dropped over an edge); the unconscious
/// drown in water.
#[allow(clippy::type_complexity)]
fn fall(
    mut commands: Commands,
    time: Res<Time>,
    rapier: ReadRapierContext,
    waters: Option<Res<crate::swim::Waters>>,
    mut stats: ResMut<PlayerStats>,
    mut msgs: ResMut<HudMessages>,
    mut q: Query<(Entity, &mut Transform, &mut Falling, &mut Npc, Option<&crate::npc::NpcRig>)>,
    joints: Query<&GlobalTransform>,
) {
    let dt = time.delta_secs().min(0.05);
    let Ok(ctx) = rapier.single() else { return };
    let ground = QueryFilter::default().groups(CollisionGroups::new(Group::ALL, GROUP_WORLD));
    for (e, mut t, mut f, mut npc, rig) in &mut q {
        if !f.placed {
            // lying where the hips were, level
            f.placed = true;
            if let Some(hips) = rig.and_then(|r| r.joint("Root_jnt")).and_then(|j| joints.get(j).ok()) {
                t.translation = hips.translation() + Vec3::Y * 0.2;
            }
            let fwd = t.rotation * Vec3::NEG_Z;
            let yaw = (-fwd.x).atan2(-fwd.z);
            t.rotation = Quat::from_rotation_y(yaw);
            npc.yaw = yaw;
        }
        f.vel.y -= 9.8 * dt;
        let step = f.vel * dt;
        let feet = t.translation - Vec3::Y * crate::npc::NPC_CENTER;
        let down = (feet.y - (t.translation.y + step.y - crate::npc::NPC_CENTER)).max(0.0);
        let hit = ctx.cast_ray(t.translation + Vec3::new(step.x, 0.0, step.z), Vec3::NEG_Y, crate::npc::NPC_CENTER + down + 0.05, true, ground);
        let in_water = waters.as_ref().and_then(|w| w.at(t.translation - Vec3::Y * 0.6));
        if hit.is_some() || in_water.is_some() {
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
