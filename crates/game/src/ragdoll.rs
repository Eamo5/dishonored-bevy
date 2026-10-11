//! Ragdolls: a character's physics asset (`m_pPhysicsAsset`, cooked into `scene.ragdolls`)
//! made into bodies on its bones, joined by its constraints' ball joints within their swing
//! and twist limits. A death goes limp where its clip says (`DishonoredNotify_Ragdoll`, near
//! its end; else at its end); the blown away (Wind Blast, explosions) and the bodies Corvo
//! drops or throws go limp at once. The bones follow the bodies; once they lie still the
//! bodies are put away and the bones kept where they lay (in saves too). Picking a body up
//! gives it back to its clips.

use crate::anim::Animator;
use crate::level::{LevelInfo, GROUP_PROP, GROUP_WORLD};
use crate::npc::{Mode, Npc, NpcRig, NpcVisual, ScriptedAnim};
use crate::GameState;
use bevy::prelude::*;
use bevy::transform::TransformSystems;
use bevy_rapier3d::prelude::*;

/// The ragdolls' bodies (they meet the world and loose props, not each other, Corvo or the
/// living).
pub const GROUP_RAGDOLL: Group = Group::GROUP_5;

/// How high above the hips a limp character's own position is kept (what finds a body to
/// pick up looks there less this).
const HIPS_BELOW: f32 = 0.75;

pub struct RagdollPlugin;

impl Plugin for RagdollPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<RagdollStep>()
            .add_systems(PostUpdate, (end_ragdolls, start_ragdolls, push_ragdolls, scale_ragdolls).chain().before(PhysicsSet::SyncBackend).run_if(in_state(GameState::InGame)))
            .add_systems(
                PostUpdate,
                (unscale_ragdolls, pose_ragdolls).chain().after(PhysicsSet::Writeback).after(crate::anim::AnimPose).before(TransformSystems::Propagate).run_if(in_state(GameState::InGame)),
            );
    }
}

/// A character about to go limp: its bodies' starting velocity, how many frames to wait for
/// its pose to be drawn first, and a point it's known to be clear of walls from (the eye of
/// whoever let go of it).
#[derive(Component, Clone, Default)]
pub struct GoLimp {
    pub vel: Vec3,
    pub wait: u8,
    pub clear_from: Option<Vec3>,
    /// (where its bodies' bones were the frame before)
    moved: Vec<(usize, Vec3)>,
}

impl GoLimp {
    /// Limp at its pose a frame or two from now.
    pub fn soon() -> GoLimp {
        GoLimp { wait: 2, ..default() }
    }

    /// Limp now, moving so (let go of: clear of walls from there).
    pub fn now(vel: Vec3, clear_from: Option<Vec3>) -> GoLimp {
        GoLimp { vel, clear_from, ..default() }
    }
}

/// A limp character: its bodies (each's physics entity and bone) while they move; once they
/// lie still, its body bones where they lay (in the character's frame).
#[derive(Component, Default)]
pub struct Ragdoll {
    parts: Vec<(Entity, usize)>,
    rest: Vec<(usize, Transform)>,
    t: f32,
    still: f32,
}

impl Ragdoll {
    /// Lying still where it lay (a saved body).
    pub fn at_rest(rest: Vec<(usize, Transform)>) -> Ragdoll {
        Ragdoll { rest, ..default() }
    }

    /// Its body bones in the character's frame (for saves): where they lay, or where its
    /// bodies are now.
    pub fn pose(&self, owner: &GlobalTransform, rig: &NpcRig, joints: &Query<&GlobalTransform>) -> Vec<(usize, Transform)> {
        if self.parts.is_empty() {
            return self.rest.clone();
        }
        let inv = owner.affine().inverse();
        self.parts
            .iter()
            .filter_map(|&(_, b)| {
                let g = joints.get(rig.joint_at(b)?).ok()?;
                let (_, r, t) = (bevy::math::Affine3A::from(inv) * g.affine()).to_scale_rotation_translation();
                Some((b, Transform::from_translation(t).with_rotation(r)))
            })
            .collect()
    }
}

/// A ragdoll's body: whose.
#[derive(Component)]
pub struct RagdollPart {
    pub owner: Entity,
}

/// The rapier step's view of the parts in bent time (as the props': velocities scaled, gravity
/// by the square; stopped, held still), put back after it.
#[derive(Resource, Default)]
struct RagdollStep(Vec<(Entity, Velocity, f32)>);

fn log() -> bool {
    std::env::var_os("DH_RAGDOLL_LOG").is_some()
}

/// A character's ragdoll, if it has one.
fn ragdoll_of<'a>(level: &'a LevelInfo, npc: &Npc) -> Option<&'a dhcook::format::RagdollDef> {
    npc.npc_type.and_then(|i| level.scene.npc_types.get(i as usize)).and_then(|ty| ty.ragdoll).and_then(|r| level.scene.ragdolls.get(r as usize))
}

/// Its bodies' bones' world poses now (and their scale).
fn body_poses(def: &dhcook::format::RagdollDef, rig: &NpcRig, joints: &Query<&GlobalTransform>) -> Vec<(usize, Quat, Vec3, f32)> {
    let mut bodies = Vec::new();
    for (bi, b) in def.bodies.iter().enumerate() {
        let Some(g) = rig.index(&b.bone).and_then(|i| rig.joint_at(i)).and_then(|j| joints.get(j).ok()) else { continue };
        let (s, r, p) = g.to_scale_rotation_translation();
        if p.is_finite() && r.is_finite() {
            bodies.push((bi, r, p, s.x.abs().max(0.01)));
        }
    }
    bodies
}

/// A ragdoll's bodies made where its bones are (`bodies`: each body's, from `body_poses`),
/// brought back to this side of any wall between them and `clear_from`, each moving at `vel`
/// plus its bone's own motion (`moved`: where each body was a moment ago, `dt` before), joined
/// to the bodies they hang from.
#[allow(clippy::too_many_arguments)]
fn spawn_parts(
    commands: &mut Commands,
    owner: Entity,
    def: &dhcook::format::RagdollDef,
    rig: &NpcRig,
    mut bodies: Vec<(usize, Quat, Vec3, f32)>,
    vel: Vec3,
    clear_from: Option<Vec3>,
    ctx: Option<&RapierContext>,
    moved: &[(usize, Vec3)],
    dt: f32,
) -> Vec<(Entity, usize)> {
    // let go of against a wall: brought back to this side of it
    if let (Some(from), Some(ctx)) = (clear_from, ctx) {
        let walls = QueryFilter::default().exclude_sensors().groups(CollisionGroups::new(Group::ALL, GROUP_WORLD));
        let mut back = 0.0f32;
        let mut dir = Vec3::ZERO;
        for &(_, _, p, _) in &bodies {
            let to = p - from;
            let d = to.length();
            if d < 1e-3 {
                continue;
            }
            if let Some((_, toi)) = ctx.cast_ray(from, to / d, d, true, walls) {
                if d - toi + 0.15 > back {
                    back = d - toi + 0.15;
                    dir = (to / d).with_y(0.0).normalize_or_zero();
                }
            }
        }
        if back > 0.0 {
            for b in &mut bodies {
                b.2 -= dir * back;
            }
        }
    }
    let mut parts: Vec<(Entity, usize)> = Vec::new();
    let mut made: Vec<(usize, Entity)> = Vec::new();
    for &(bi, r, p, s) in &bodies {
        let b = &def.bodies[bi];
        let shapes: Vec<(Vec3, Quat, Collider)> = b
            .shapes
            .iter()
            .filter_map(|sh| match sh {
                dhcook::format::RagShape::Sphere { at, r } => Some((Vec3::from(*at) * s, Quat::IDENTITY, Collider::ball(r * s))),
                dhcook::format::RagShape::Capsule { a, b, r } => Some((Vec3::ZERO, Quat::IDENTITY, Collider::capsule(Vec3::from(*a) * s, Vec3::from(*b) * s, r * s))),
                dhcook::format::RagShape::Box { at, rot, half } => Some((Vec3::from(*at) * s, Quat::from_array(*rot).normalize(), Collider::cuboid(half[0] * s, half[1] * s, half[2] * s))),
                dhcook::format::RagShape::Hull { points } => Collider::convex_hull(&points.iter().map(|q| Vec3::from(*q) * s).collect::<Vec<_>>()).map(|c| (Vec3::ZERO, Quat::IDENTITY, c)),
            })
            .collect();
        if shapes.is_empty() {
            continue;
        }
        let Some(bone) = rig.index(&b.bone) else { continue };
        // (its bone's own motion, as the clip had it moving)
        let own = moved.iter().find(|m| m.0 == bi).filter(|_| dt > 1e-4).map(|m| ((p - m.1) / dt).clamp_length_max(8.0)).unwrap_or(Vec3::ZERO);
        let part = commands
            .spawn((
                RagdollPart { owner },
                Transform::from_translation(p).with_rotation(r),
                RigidBody::Dynamic,
                Collider::compound(shapes),
                CollisionGroups::new(GROUP_RAGDOLL, GROUP_WORLD | GROUP_PROP),
                // (flesh: about water's density, the trunk's bodies heavier by their scale)
                ColliderMassProperties::Density(1000.0 * b.mass_scale.max(0.1)),
                Friction::coefficient(0.8),
                Restitution::coefficient(0.0),
                Damping { linear_damping: 0.15, angular_damping: 0.9 },
                Velocity::linear(vel + own),
                GravityScale(1.0),
                Ccd::enabled(),
                Sleeping::default(),
                DespawnOnExit(GameState::InGame),
            ))
            .id();
        parts.push((part, bone));
        made.push((bi, part));
    }
    // the joints, each on the body that hangs from the other (its frames in each's bone,
    // limits about the frame's axes: twist X, swings Z and Y)
    let part_of = |name: &str| def.bodies.iter().position(|b| b.bone.eq_ignore_ascii_case(name)).and_then(|bi| made.iter().find(|m| m.0 == bi)).map(|m| m.1);
    let s = bodies.first().map(|b| b.3).unwrap_or(1.0);
    for j in &def.joints {
        let (Some(child), Some(parent)) = (part_of(&j.child), part_of(&j.parent)) else { continue };
        let mut g = GenericJointBuilder::new(JointAxesMask::LOCKED_SPHERICAL_AXES)
            .local_anchor1(Vec3::from(j.parent_frame.0) * s)
            .local_basis1(Quat::from_array(j.parent_frame.1).normalize())
            .local_anchor2(Vec3::from(j.child_frame.0) * s)
            .local_basis2(Quat::from_array(j.child_frame.1).normalize());
        if let Some(tw) = j.twist {
            let a = tw.max(1.0).to_radians();
            g = g.limits(JointAxis::AngX, [-a, a]);
        }
        if let Some([s1, s2]) = j.swing {
            let (a1, a2) = (s1.max(1.0).to_radians(), s2.max(1.0).to_radians());
            g = g.limits(JointAxis::AngZ, [-a1, a1]).limits(JointAxis::AngY, [-a2, a2]);
        }
        let mut joint = g.build();
        joint.set_contacts_enabled(false);
        commands.entity(child).try_insert(ImpulseJoint::new(parent, TypedJoint::GenericJoint(joint)));
    }
    parts
}

/// Make the bodies of those about to go limp, where their bones are (while they wait for
/// their pose, noting where it was: the bodies keep the clip's motion).
#[allow(clippy::type_complexity)]
fn start_ragdolls(
    mut commands: Commands,
    time: Res<Time>,
    level: Option<Res<LevelInfo>>,
    rapier: ReadRapierContext,
    mut npcs: Query<(Entity, &mut Npc, &NpcRig, &mut GoLimp, &mut Transform, Option<&mut Animator>, Option<&mut crate::npc::NpcAnim>, (Has<crate::npc::Ashes>, Has<crate::npc::ConsumedBody>)), (Without<Ragdoll>, Without<ScriptedAnim>)>,
    joints: Query<&GlobalTransform>,
) {
    let Some(level) = level else { return };
    let ctx = rapier.single().ok();
    let dt = time.delta_secs();
    for (e, mut npc, rig, mut limp, mut t, anim, st, (ashes, consumed)) in &mut npcs {
        let Some(def) = ragdoll_of(&level, &npc).filter(|_| !ashes && !consumed) else {
            // (burnt to ash, eaten: never limp)
            commands.entity(e).try_remove::<GoLimp>();
            if let Some(mut st) = st {
                st.ragdoll = false;
            }
            continue;
        };
        let bodies = body_poses(def, rig, &joints);
        if limp.wait > 0 {
            limp.wait -= 1;
            limp.moved = bodies.iter().map(|b| (b.0, b.2)).collect();
            continue;
        }
        commands.entity(e).try_remove::<GoLimp>();
        if bodies.len() < 2 {
            continue;
        }
        let parts = spawn_parts(&mut commands, e, def, rig, bodies, limp.vel, limp.clear_from, ctx.as_ref(), &limp.moved, dt);
        if log() {
            info!("ragdoll: {} ({}) limp with {} bodies, velocity {:.2}", npc.name, def.name, parts.len(), limp.vel);
        }
        // its own frame upright (a body let go of from the shoulder is tilted), the bones
        // following the bodies from now on
        let fwd = t.rotation * Vec3::NEG_Z;
        let yaw = (-fwd.x).atan2(-fwd.z);
        t.rotation = Quat::from_rotation_y(yaw);
        npc.yaw = yaw;
        npc.grounded = true;
        npc.velocity = Vec3::ZERO;
        if let Some(mut a) = anim {
            a.frozen = true;
        }
        commands.entity(e).try_remove::<crate::carry::Falling>();
        commands.entity(e).try_insert(Ragdoll { parts, ..default() });
    }
}

/// Blasts and Wind Blast push the limp (the lying still stirred again): each body away from
/// the blast, along the wind, by how near it is.
#[allow(clippy::type_complexity)]
fn push_ragdolls(
    mut commands: Commands,
    level: Option<Res<LevelInfo>>,
    mut blasts: MessageReader<crate::gadgets::Explosion>,
    mut winds: MessageReader<crate::worlddamage::WorldDamage>,
    mut npcs: Query<(Entity, &Npc, &NpcRig, &Transform, &mut Ragdoll), Without<ScriptedAnim>>,
    mut parts: Query<(&Transform, &mut Velocity, &mut Sleeping), With<RagdollPart>>,
    joints: Query<&GlobalTransform>,
) {
    // (where, how far, how hard, along what within what angle)
    let mut pushes: Vec<(Vec3, f32, f32, Option<(Vec3, f32)>)> = blasts.read().map(|b| (b.at, b.radius.max(1.0), 6.0, None)).collect();
    for w in winds.read() {
        if let (crate::worlddamage::Reach::Cone { at, dir, len, half }, "DisDamageType_WindBlast") = (w.reach, w.kind) {
            pushes.push((at, len, 11.0, Some((dir, half))));
        }
    }
    if pushes.is_empty() {
        return;
    }
    let Some(level) = level else { return };
    let kick = |p: Vec3, (at, reach, speed, cone): (Vec3, f32, f32, Option<(Vec3, f32)>)| -> Option<Vec3> {
        let to = p - at;
        let d = to.length();
        if d > reach {
            return None;
        }
        let k = (1.0 - d / reach).clamp(0.2, 1.0);
        match cone {
            Some((dir, half)) => (to.normalize_or_zero().dot(dir) >= half.cos()).then(|| (dir.with_y(0.0).normalize_or_zero() + Vec3::Y * 0.35) * speed * k),
            None => Some(to.with_y(0.0).normalize_or_zero() * speed * k + Vec3::Y * (3.0 * k).max(1.0)),
        }
    };
    for (e, npc, rig, t, mut rd) in &mut npcs {
        if rd.parts.is_empty() {
            // lying still: stirred again, from where it lies
            let hips = t.translation - Vec3::Y * HIPS_BELOW;
            let Some(v) = pushes.iter().find_map(|&p| kick(hips, p)) else { continue };
            let Some(def) = ragdoll_of(&level, npc) else { continue };
            let bodies = body_poses(def, rig, &joints);
            if bodies.len() < 2 {
                continue;
            }
            rd.parts = spawn_parts(&mut commands, e, def, rig, bodies, v, None, None, &[], 0.0);
            rd.t = 0.0;
            rd.still = 0.0;
            if log() {
                info!("ragdoll: {} stirred at {:.2}", npc.name, v);
            }
            continue;
        }
        let mut stirred = false;
        for &(p, _) in &rd.parts {
            let Ok((pt, mut v, mut sleep)) = parts.get_mut(p) else { continue };
            for &push in &pushes {
                if let Some(k) = kick(pt.translation, push) {
                    v.linear += k;
                    sleep.sleeping = false;
                    stirred = true;
                }
            }
        }
        if stirred {
            rd.still = 0.0;
        }
    }
}

/// A body picked up (or brought back to life by the scripts) is its clips' again; one burnt
/// to ash or eaten has no more bodies; those of one gone (streamed out) go too.
#[allow(clippy::type_complexity)]
fn end_ragdolls(
    mut commands: Commands,
    npcs: Query<(Entity, &Npc, &Ragdoll, Has<ScriptedAnim>, Has<crate::npc::Ashes>, Has<crate::npc::ConsumedBody>)>,
    parts: Query<(Entity, &RagdollPart)>,
    owners: Query<(), With<Ragdoll>>,
    mut anims: Query<&mut Animator>,
) {
    for (p, part) in &parts {
        if !owners.contains(part.owner) {
            commands.entity(p).try_despawn();
        }
    }
    for (e, npc, rd, scripted, ashes, consumed) in &npcs {
        if !scripted && npc.is_down() && !ashes && !consumed {
            continue;
        }
        for &(p, _) in &rd.parts {
            commands.entity(p).try_despawn();
        }
        commands.entity(e).try_remove::<Ragdoll>();
        if let Ok(mut a) = anims.get_mut(e) {
            a.frozen = false;
        }
        if log() {
            info!("ragdoll: {} taken up", npc.name);
        }
    }
}

/// In bent time the bodies move by world time (as the props do).
fn scale_ragdolls(tc: Res<crate::gameplay::TimeControl>, mut parts: Query<(Entity, &mut Velocity, &mut GravityScale, &mut Damping), With<RagdollPart>>, mut step: ResMut<RagdollStep>) {
    step.0.clear();
    let scale = tc.world_scale().clamp(0.0, 1.0);
    if scale == 1.0 {
        return;
    }
    for (e, mut v, mut g, mut d) in &mut parts {
        step.0.push((e, *v, g.0));
        v.linear *= scale;
        v.angular *= scale;
        g.0 *= scale * scale;
        if scale == 0.0 {
            d.linear_damping = 1e4;
            d.angular_damping = 1e4;
        }
    }
}

fn unscale_ragdolls(tc: Res<crate::gameplay::TimeControl>, mut parts: Query<(&mut Velocity, &mut GravityScale, &mut Damping), With<RagdollPart>>, mut step: ResMut<RagdollStep>) {
    let scale = tc.world_scale().clamp(0.0, 1.0);
    for (e, saved, gravity) in step.0.drain(..) {
        let Ok((mut v, mut g, mut d)) = parts.get_mut(e) else { continue };
        if scale <= 0.0 {
            *v = saved;
        } else {
            v.linear /= scale;
            v.angular /= scale;
        }
        g.0 = gravity;
        *d = Damping { linear_damping: 0.15, angular_damping: 0.9 };
    }
}

/// The bones follow the bodies (each body's bone where its body is, the others as posed
/// below it), the character's own place kept above its hips; still for a while, the bodies
/// are put away and the bones left where they lay. The unconscious drown in water.
#[allow(clippy::type_complexity)]
fn pose_ragdolls(
    mut commands: Commands,
    time: Res<Time>,
    tc: Res<crate::gameplay::TimeControl>,
    cfg: Query<&RapierConfiguration>,
    level: Option<Res<LevelInfo>>,
    waters: Option<Res<crate::swim::Waters>>,
    (mut stats, mut msgs): (ResMut<crate::gameplay::PlayerStats>, ResMut<crate::gameplay::HudMessages>),
    mut npcs: Query<(Entity, &mut Npc, &mut Transform, &NpcRig, &Children, &mut Ragdoll), Without<ScriptedAnim>>,
    parts: Query<(&Transform, &Velocity), (With<RagdollPart>, Without<Npc>)>,
    visuals: Query<&Transform, (With<NpcVisual>, Without<Npc>, Without<RagdollPart>)>,
    mut joints: Query<&mut Transform, (Without<Npc>, Without<RagdollPart>, Without<NpcVisual>)>,
) {
    let running = cfg.iter().next().is_none_or(|c| c.physics_pipeline_active) && tc.world_scale() > 0.0;
    let dt = time.delta_secs().min(0.05) * tc.world_scale().clamp(0.0, 1.0);
    let kill_y = level.as_ref().map(|l| l.scene.kill_y).unwrap_or(-200.0);
    let mut world: Vec<Option<Transform>> = Vec::new();
    for (_, mut npc, mut t, rig, children, mut rd) in &mut npcs {
        // the body bones' world poses
        world.clear();
        world.resize(rig.len(), None);
        if !rd.parts.is_empty() {
            let mut fastest = 0.0f32;
            let mut ready = true;
            for &(p, b) in &rd.parts {
                let Ok((pt, v)) = parts.get(p) else {
                    ready = false;
                    break;
                };
                if b < world.len() {
                    world[b] = Some(Transform::from_translation(pt.translation).with_rotation(pt.rotation));
                }
                fastest = fastest.max(v.linear.length()).max(v.angular.length() * 0.15);
            }
            if !ready {
                continue;
            }
            // (its place above its hips: the first body's, the trunk's)
            let hips = rd.parts.first().and_then(|&(_, b)| world.get(b).copied().flatten()).map(|w| w.translation);
            if let Some(h) = hips {
                t.translation = h + Vec3::Y * HIPS_BELOW;
            }
            if running {
                if log() && (rd.t / 0.5).floor() != ((rd.t + dt) / 0.5).floor() {
                    info!("ragdoll: {} at {:.1}s fastest {:.2}", npc.name, rd.t + dt, fastest);
                }
                rd.t += dt;
                rd.still = if fastest < 0.35 { rd.still + dt } else { 0.0 };
            }
            // the unconscious drown
            if let (Some(h), Some(w)) = (hips, waters.as_ref()) {
                if npc.mode == Mode::Unconscious && w.at(h).is_some() {
                    npc.set_mode(Mode::Dead);
                    stats.kills += 1;
                    stats.knockouts = stats.knockouts.saturating_sub(1);
                    msgs.push(format!("{} drowned", npc.name));
                }
            }
            let lost = hips.is_some_and(|h| h.y < kill_y || !h.is_finite());
            if rd.still > 0.8 || rd.t > 8.0 || lost {
                // lying still: the bones left where they lay
                let inv = t.compute_affine().inverse();
                let rest: Vec<(usize, Transform)> = rd
                    .parts
                    .iter()
                    .filter_map(|&(_, b)| {
                        let w = world.get(b).copied().flatten()?;
                        let (_, r, p) = (inv * w.compute_affine()).to_scale_rotation_translation();
                        Some((b, Transform::from_translation(p).with_rotation(r)))
                    })
                    .collect();
                for &(p, _) in &rd.parts {
                    commands.entity(p).try_despawn();
                }
                rd.parts.clear();
                rd.rest = rest;
                if log() {
                    info!("ragdoll: {} at rest after {:.1}s at {:.2}", npc.name, rd.t, t.translation);
                }
            }
        } else {
            for &(b, l) in &rd.rest {
                if b < world.len() {
                    world[b] = Some(t.mul_transform(l));
                }
            }
        }
        // the bones in order (parents first), from the character's visual frame
        let Some(vt) = children.iter().find_map(|c| visuals.get(c).ok()) else { continue };
        let base = t.mul_transform(*vt);
        let mut have: Vec<Transform> = Vec::with_capacity(rig.len());
        for i in 0..rig.len() {
            let parent = rig.parent_of(i).filter(|p| *p < i).map(|p| have[p]).unwrap_or(base);
            let Some(j) = rig.joint_at(i) else {
                have.push(parent);
                continue;
            };
            let Ok(mut jt) = joints.get_mut(j) else {
                have.push(parent);
                continue;
            };
            match world[i] {
                Some(w) => {
                    let inv = parent.compute_affine().inverse();
                    let (_, r, p) = (inv * w.compute_affine()).to_scale_rotation_translation();
                    if jt.translation != p || jt.rotation != r {
                        jt.translation = p;
                        jt.rotation = r;
                    }
                    have.push(Transform { translation: w.translation, rotation: w.rotation, scale: parent.scale * jt.scale });
                }
                None => have.push(parent.mul_transform(*jt)),
            }
        }
    }
}
