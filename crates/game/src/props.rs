//! Physics props and breakables: the levels' `DishonoredMovable`s (bottles, pans, cups,
//! chairs) fall, roll and get knocked about; the light ones Corvo can pick up ([Use]), drop
//! ([Use]) and throw ([Attack]). Their knocks sound and carry as the contact system says
//! (`Dis_ContactSystem`: the prop's contact type against the world or a body: the sound, the
//! effect, how far the AI hear it), and a hard enough one (`m_fMinSpeedToBeDamaged`, thrown;
//! `...OnDrop`, dropped) breaks them (`m_Steps`: the sound, the AI noise, the effect, the
//! pieces). The breakables in the way (`DishonoredBreakableNavBlock`: crates, boarded
//! passages) stay put until blades, bolts, bullets or blasts wear down their `m_Health`.

use crate::audio::PostEvent;
use crate::bindings::{hint, Act, Bindings};
use crate::gadgets::Explosion;
use crate::gameplay::{HitKind, Noise, NpcStagger, Strikeable, Struck};
use crate::interact::InteractFocus;
use crate::level::{GameAssets, InstanceCollider, LevelInfo, LevelInstance, GROUP_NPC, GROUP_PLAYER, GROUP_PROP, GROUP_WORLD};
use crate::npc::Npc;
use crate::particles::SpawnEffect;
use crate::player::{Player, PlayerCamera};
use crate::GameState;
use bevy::prelude::*;
use bevy_rapier3d::prelude::*;
use dhcook::format::{Impact, Movable};
use std::collections::HashMap;

pub struct PropsPlugin;

impl Plugin for PropsPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Held>()
            .init_resource::<PropRestore>()
            .init_resource::<TankBlasts>()
            .init_resource::<PropsTaken>()
            .init_resource::<PropKnocks>()
            .add_systems(OnEnter(GameState::InGame), ((|mut h: ResMut<Held>| *h = Held::default()), setup_props.after(crate::level::LevelSpawnSet)))
            .add_systems(Update, prop_focus.after(crate::interact::FocusSet).before(crate::interact::use_focus).run_if(in_state(GameState::InGame)))
            .add_systems(Update, (restore_props, hold_prop, prop_impacts, prop_hits, sync_props, burst_tanks, prop_events, script_physics).chain().after(crate::player::PlayerMoveSet).run_if(in_state(GameState::InGame)));
    }
}

/// Kilograms by `m_WeightClass` (tiny, small, medium, large).
const MASS: [f32; 4] = [0.4, 2.0, 8.0, 25.0];
/// The share of Corvo's throw (`ThrowStrength`) by weight class.
const THROW: [f32; 4] = [1.0, 0.85, 0.6, 0.4];
/// The groups a loose prop collides with.
const LOOSE: Group = GROUP_WORLD.union(GROUP_PROP).union(GROUP_NPC).union(GROUP_PLAYER);

/// A prop on its level instance: its tweak (`scene.movables`), body and wear.
#[derive(Component)]
pub struct Prop {
    pub index: usize,
    health: f32,
    /// the instance's collider entity (a loose prop's physics body)
    body: Option<Entity>,
    last_vel: Vec3,
    /// thrown, not yet landed
    thrown: bool,
    /// impacts sound at most this often
    quiet: f32,
    /// held off Corvo for a moment after leaving his hands
    released: f32,
    /// the body sits at its box's centre, this far (rotated) from the instance's origin
    offset: Vec3,
    /// still settling where the level put it
    settling: bool,
    /// hanging by joints (`Movable::joints`) the scripts haven't destroyed
    hung: bool,
}

impl Prop {
    /// Its physics body and where that sits from the prop's origin.
    pub fn body(&self) -> (Option<Entity>, Vec3) {
        (self.body, self.offset)
    }
}

/// Props bursting (a whale oil tank's `m_pExplosion`): where, and the blast; and the props
/// broken (their instances, for the level scripts).
#[derive(Resource, Default)]
pub struct TankBlasts(pub Vec<(Vec3, dhcook::format::TrapBlast)>, pub Vec<u32>);

/// Props Corvo picked up (their instances), for the level scripts.
#[derive(Resource, Default)]
pub struct PropsTaken(pub Vec<u32>);

/// The level scripts hear of props picked up (`DisSeqEvent_MovablePickedUp`) and broken
/// (`DisSeqEvent_BreakableBroken`).
/// Props knocked about (their instances, how hard), for the scripts' `SeqEvent_RigidBodyCollision`.
#[derive(Resource, Default)]
pub struct PropKnocks(pub Vec<(u32, f32)>);

/// `SeqAct_SetPhysics` on props: falling (`PHYS_RigidBody`), held still (`PHYS_None`), or
/// moved by a matinee (`PHYS_Interpolating`).
fn script_physics(mut commands: Commands, vm: Option<ResMut<crate::kismet::Vm>>, level: Option<Res<LevelInfo>>, mut props: Query<&mut Prop>, mut bodies: Query<(&Transform, &mut Velocity), Without<Prop>>) {
    let (Some(mut vm), Some(level)) = (vm, level) else { return };
    let g = vm.g.clone();
    // the scripts' physics bursts (`RB_RadialImpulseActor`): the loose things in reach are
    // thrown outward (an impulse over their weight, or a speed outright), less at the edge
    for (at, [radius, strength, vel_change, linear]) in std::mem::take(&mut vm.impulses) {
        for p in &props {
            let Some(m) = level.scene.movables.get(p.index).filter(|m| !m.fixed) else { continue };
            let Some(b) = p.body else { continue };
            let Ok((bt, mut v)) = bodies.get_mut(b) else { continue };
            let to = bt.translation - at;
            let d = to.length();
            if d > radius {
                continue;
            }
            let fade = if linear > 0.0 { 1.0 - d / radius.max(0.01) } else { 1.0 };
            let dv = strength * 0.01 * fade / if vel_change > 0.0 { 1.0 } else { MASS[m.weight.min(3) as usize] };
            v.linear += to.normalize_or(Vec3::Y) * dv;
            commands.entity(b).try_insert((RigidBody::Dynamic, Sleeping { sleeping: false, ..default() }));
        }
    }
    // the joints the scripts destroyed: what hung by them falls
    if !vm.joints_broken.is_empty() {
        for mut p in &mut props {
            let Some(m) = level.scene.movables.get(p.index) else { continue };
            if !p.hung || !m.joints.iter().all(|j| vm.joints_broken.contains(j)) {
                continue;
            }
            p.hung = false;
            p.settling = false;
            if let Some(b) = p.body {
                commands.entity(b).try_insert((RigidBody::Dynamic, Sleeping { sleeping: false, ..default() }));
            }
        }
    }
    // velocities the scripts give things (`SeqAct_SetVelocity`)
    for (a, vel) in std::mem::take(&mut vm.velocities) {
        let Some(ka) = g.actors.get(a as usize) else { continue };
        for p in &props {
            let Some(m) = level.scene.movables.get(p.index) else { continue };
            if ka.instances.contains(&m.instance) && !m.fixed {
                if let Some(b) = p.body {
                    commands.entity(b).try_insert((RigidBody::Dynamic, Velocity { linear: vel, angular: Vec3::ZERO }, Sleeping { sleeping: false, ..default() }));
                }
            }
        }
    }
    if vm.set_physics.is_empty() {
        return;
    }
    for (targets, mode) in std::mem::take(&mut vm.set_physics) {
        let insts: Vec<u32> = targets.iter().filter_map(|v| if let crate::kismet::Val::Actor(a) = v { g.actors.get(*a as usize) } else { None }).flat_map(|a| a.instances.iter().copied()).collect();
        for p in &props {
            let Some(m) = level.scene.movables.get(p.index) else { continue };
            if !insts.contains(&m.instance) || m.fixed {
                continue;
            }
            let Some(b) = p.body else { continue };
            match mode.as_str() {
                "PHYS_RigidBody" => {
                    commands.entity(b).try_insert((RigidBody::Dynamic, Sleeping { sleeping: false, ..default() }));
                }
                "PHYS_Interpolating" => {
                    commands.entity(b).try_insert(RigidBody::KinematicPositionBased);
                }
                _ => {
                    commands.entity(b).try_insert(RigidBody::Fixed);
                }
            }
        }
    }
}

fn prop_events(mut taken: ResMut<PropsTaken>, mut blasts: ResMut<TankBlasts>, mut knocks: ResMut<PropKnocks>, mut used: MessageWriter<crate::interact::Interaction>) {
    for (instance, speed) in knocks.0.drain(..) {
        used.write(crate::interact::Interaction::Knocked { instance, speed });
    }
    for instance in taken.0.drain(..) {
        used.write(crate::interact::Interaction::Movable { instance, broken: false });
    }
    for instance in blasts.1.drain(..) {
        used.write(crate::interact::Interaction::Movable { instance, broken: true });
    }
}

fn burst_tanks(mut q: ResMut<TankBlasts>, settings: Res<crate::settings::Settings>, mut blasts: MessageWriter<crate::gadgets::Explosion>) {
    for (at, b) in q.0.drain(..) {
        let damage = b.damage[settings.difficulty.min(3) as usize];
        blasts.write(crate::gadgets::Explosion { at, radius: b.radius.max(1.0), full: b.full, damage, effect: "grenade", player: Some([b.player_radius, b.player_full]), kind: HitKind::Explosion });
    }
}

/// The prop in Corvo's hands (its instance), and how long his hands stay busy after a throw.
#[derive(Resource, Default)]
pub struct Held(pub Option<Entity>, pub f32);

impl Held {
    /// no sword while the hands are full (or just threw)
    pub fn busy(&self) -> bool {
        self.0.is_some() || self.1 > 0.0
    }
}

/// A loaded save's props to put back: (movable, `None` broken, else where it lies).
#[derive(Resource, Default)]
pub struct PropRestore(pub Option<Vec<(u32, Option<([f32; 3], [f32; 4])>)>>);

/// The props as a save left them: the broken ones gone (quietly), the loose ones where they lay.
fn restore_props(mut commands: Commands, mut restore: ResMut<PropRestore>, props: Query<(Entity, &Prop)>, mut tf: Query<&mut Transform>) {
    let Some(list) = restore.0.take() else { return };
    let saved: HashMap<u32, Option<([f32; 3], [f32; 4])>> = list.into_iter().collect();
    for (e, p) in &props {
        match saved.get(&(p.index as u32)) {
            Some(None) => {
                commands.entity(e).despawn();
                if let Some(b) = p.body {
                    commands.entity(b).despawn();
                }
            }
            Some(Some((at, rot))) => {
                let (at, rot) = (Vec3::from(*at), Quat::from_array(*rot));
                if let Ok(mut t) = tf.get_mut(e) {
                    t.translation = at;
                    t.rotation = rot;
                }
                if let Some(Ok(mut bt)) = p.body.map(|b| tf.get_mut(b)) {
                    bt.translation = at + rot * p.offset;
                    bt.rotation = rot;
                }
            }
            None => {}
        }
    }
}

/// A broken prop's piece, gone after a while.
#[derive(Component)]
struct Chunk(f32);

fn setup_props(
    mut commands: Commands,
    level: Option<Res<LevelInfo>>,
    instances: Query<(Entity, &LevelInstance, &Transform, Option<&InstanceCollider>)>,
) {
    let Some(level) = level else { return };
    let scene = &level.scene;
    let by_index: HashMap<u32, (Entity, Transform, Option<Entity>)> = instances.iter().map(|(e, li, t, c)| (li.index, (e, *t, c.map(|c| c.0)))).collect();
    let (mut loose, mut fixed) = (0, 0);
    for (i, m) in scene.movables.iter().enumerate() {
        let Some(&(e, t, col)) = by_index.get(&m.instance) else { continue };
        // (settling at the start makes no sound)
        let mut prop = Prop { index: i, health: m.health.max(1.0), body: None, last_vel: Vec3::ZERO, thrown: false, quiet: 1.0, released: 0.0, offset: Vec3::ZERO, settling: true, hung: !m.joints.is_empty() };
        if m.fixed {
            // a breakable in the way: weapons wear it down (its collider goes with it)
            // (the scripts may remove it as the level starts: `try_`)
            if let Some(c) = col {
                commands.entity(c).try_insert(Strikeable(e));
            }
            prop.body = col;
            fixed += 1;
        } else if let Some(inst) = scene.instances.get(m.instance as usize) {
            // loose: a box of its bounds, of its weight
            let Some(mesh) = scene.meshes.get(inst.mesh as usize) else { continue };
            let (min, max) = (Vec3::from(mesh.min) * t.scale, Vec3::from(mesh.max) * t.scale);
            let half = ((max - min) * 0.5).max(Vec3::splat(0.02));
            // a plain box about the body's origin (a compound one trips the character
            // controllers' pushing), the body at its centre
            let shape = Collider::cuboid(half.x, half.y, half.z);
            prop.offset = (min + max) * 0.5;
            let at = Transform::from_translation(t.translation + t.rotation * prop.offset).with_rotation(t.rotation);
            let body = col.unwrap_or_else(|| commands.spawn(DespawnOnExit(GameState::InGame)).id());
            // the light ones stop no one (a thrown one striking someone is caught by
            // `prop_impacts`)
            let with = if m.weight <= 1 { LOOSE.difference(GROUP_PLAYER).difference(GROUP_NPC) } else { LOOSE };
            commands
                .entity(body)
                .try_remove::<crate::footsteps::ColliderSurfaces>()
                .try_insert((
                    at,
                    // (a whale oil tank stays seated in its receptacle until taken, a PA speaker
                    // hangs by its joint)
                    if m.tank.is_some() || prop.hung { RigidBody::Fixed } else { RigidBody::Dynamic },
                    shape,
                    ColliderMassProperties::Mass(MASS[m.weight.min(3) as usize]),
                    CollisionGroups::new(GROUP_PROP, with),
                    Velocity::zero(),
                    Damping { linear_damping: 0.1, angular_damping: 0.4 },
                    Ccd::enabled(),
                    // at rest where it was put, until something disturbs it
                    Sleeping { sleeping: true, ..default() },
                    GravityScale(1.0),
                    Strikeable(e),
                ));
            prop.body = Some(body);
            loose += 1;
        }
        commands.entity(e).try_insert(prop);
    }
    if loose + fixed > 0 {
        info!("{loose} loose props, {fixed} breakables");
    }
}

/// Looking at a light prop: [Use] picks it up; held: [Use] drops it, [Attack] throws it.
#[allow(clippy::too_many_arguments)]
pub(crate) fn prop_focus(
    mut commands: Commands,
    mut taken: ResMut<PropsTaken>,
    (keys, mouse, bind): (Res<ButtonInput<KeyCode>>, Res<ButtonInput<MouseButton>>, Res<Bindings>),
    level: Option<Res<LevelInfo>>,
    rapier: ReadRapierContext,
    mut focus: ResMut<InteractFocus>,
    mut held: ResMut<Held>,
    mut props: Query<&mut Prop>,
    grenades: Query<(), With<crate::gadgets::Grenade>>,
    strikeables: Query<&Strikeable>,
    mut bodies: Query<(&mut Velocity, &mut GravityScale, &mut CollisionGroups)>,
    player: Query<(Entity, &Player)>,
    cam: Query<&GlobalTransform, With<PlayerCamera>>,
    (carry, possession, peek, stats): (Res<crate::carry::Carry>, Res<crate::possession::Possession>, Res<crate::keyhole::Peek>, Res<crate::gameplay::PlayerStats>),
    (mut sfx, attrs): (MessageWriter<PostEvent>, Res<crate::gamedata::Attrs>),
) {
    let Some(level) = level else { return };
    let (Ok((pe, p)), Ok(c)) = (player.single(), cam.single()) else { return };
    let use_key = bind.key(Act::Use);
    // Live grenades share the carrying context, but their fuse and release are
    // handled by `gadgets`. Do not clear that hold or take a prop behind one.
    if held.0.or(focus.1).is_some_and(|e| grenades.contains(e)) {
        return;
    }
    // in hand
    if let Some(e) = held.0 {
        let Ok(mut prop) = props.get_mut(e) else {
            held.0 = None;
            return;
        };
        // (drop, throw: the context line's, `prompts`)
        focus.0 = true;
        focus.1 = None;
        let throw = mouse.just_pressed(MouseButton::Left);
        if keys.just_pressed(use_key) || throw || stats.dead || possession.host.is_some() {
            held.0 = None;
            held.1 = 0.3;
            prop.released = 0.35;
            if let Some(Ok((mut v, mut g, _))) = prop.body.map(|b| bodies.get_mut(b)) {
                g.0 = 1.0;
                if throw {
                    let m = &level.scene.movables[prop.index];
                    v.linear = c.forward().as_vec3() * attrs.throw.max(5.0) * THROW[m.weight.min(3) as usize] + Vec3::new(p.velocity.x, 0.0, p.velocity.z);
                    v.angular = Vec3::new(rand::random::<f32>() - 0.5, rand::random::<f32>() - 0.5, rand::random::<f32>() - 0.5) * 6.0;
                    if std::env::var("DH_PROP_LOG").is_ok() {
                        info!("throw {} at {:.1} m/s (strength {:.1}, weight {})", m.name, v.linear.length(), attrs.throw, m.weight);
                    }
                    prop.thrown = true;
                    // (a wall right ahead is struck at this speed)
                    prop.last_vel = v.linear;
                } else {
                    v.linear *= 0.3;
                }
            }
        }
        return;
    }
    // (not again straight after letting one go)
    if carry.carrying() || possession.host.is_some() || peek.at.is_some() || p.locked || held.1 > 0.0 {
        return;
    }
    // what Corvo looks at, within reach
    let Ok(ctx) = rapier.single() else { return };
    let filter = QueryFilter::default().exclude_collider(pe).groups(CollisionGroups::new(Group::ALL, GROUP_WORLD | GROUP_PROP | GROUP_NPC));
    let Some((hit, toi)) = ctx.cast_ray(c.translation(), c.forward().as_vec3(), 2.2, true, filter) else { return };
    if std::env::var("DH_PROP_LOG").is_ok() && keys.just_pressed(use_key) {
        info!("prop focus: ray hits {hit} at {toi:.2} (strikeable {:?})", strikeables.get(hit).ok().map(|s| s.0));
    }
    let Some(e) = strikeables.get(hit).ok().map(|s| s.0) else { return };
    let Ok(prop) = props.get(e) else { return };
    let m = &level.scene.movables[prop.index];
    if !m.interactable || m.fixed || prop.body.is_none() {
        return;
    }
    // nearer than whatever else is focused
    if focus.0 && focus.1.is_some() && toi > 1.2 {
        return;
    }
    let name = if m.name.is_empty() { "Object".to_string() } else { m.name.clone() };
    focus.0 = true;
    focus.1 = None;
    // (`DUI_Crosshair_Carry`)
    focus.2 = format!("Hold {} Carry", hint(Act::Use));
    focus.3 = name.to_string();
    if keys.just_pressed(use_key) {
        held.0 = Some(e);
        taken.0.push(m.instance);
        if let Some(b) = prop.body {
            commands.entity(b).try_insert(RigidBody::Dynamic);
        }
        if !m.grab_sound.is_empty() {
            sfx.write(PostEvent::named(&m.grab_sound, None));
        }
        if let Some(Ok((_, mut g, _))) = prop.body.map(|b| bodies.get_mut(b)) {
            g.0 = 0.0;
        }
    }
}

/// The prop in hand floats before Corvo's eyes, kept off him.
fn hold_prop(
    mut held: ResMut<Held>,
    mut props: Query<&mut Prop>,
    mut bodies: Query<(&Transform, &mut Velocity, &mut CollisionGroups)>,
    cam: Query<&GlobalTransform, With<PlayerCamera>>,
    level: Option<Res<LevelInfo>>,
    time: Res<Time>,
) {
    let dt = time.delta_secs();
    held.1 = (held.1 - dt).max(0.0);
    let Some(level) = level else { return };
    // after release: back to colliding with Corvo
    for mut prop in &mut props {
        if prop.released > 0.0 {
            prop.released -= dt;
            if prop.released <= 0.0 {
                if let Some(Ok((_, _, mut g))) = prop.body.map(|b| bodies.get_mut(b)) {
                    let m = &level.scene.movables[prop.index];
                    g.filters = if m.weight <= 1 { LOOSE.difference(GROUP_PLAYER).difference(GROUP_NPC) } else { LOOSE };
                }
            }
        }
    }
    let (Some(e), Ok(c)) = (held.0, cam.single()) else { return };
    let Ok(prop) = props.get(e) else { return };
    let Some(Ok((t, mut v, mut g))) = prop.body.map(|b| bodies.get_mut(b)) else { return };
    g.filters = LOOSE.difference(GROUP_PLAYER);
    let target = c.translation() + c.forward().as_vec3() * 1.0 - Vec3::Y * 0.15;
    let to = target - t.translation;
    v.linear = (to * 14.0).clamp_length_max(12.0);
    v.angular *= 0.8;
}

/// A knock: its sound, effect and noise (the contact system's), and a hard one breaks it.
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
fn prop_impacts(
    mut commands: Commands,
    time: Res<Time>,
    level: Option<Res<LevelInfo>>,
    assets: Option<Res<GameAssets>>,
    waters: Option<Res<crate::swim::Waters>>,
    held: Res<Held>,
    mut props: Query<(Entity, &mut Prop, &Transform)>,
    mut bodies: Query<(&Transform, &mut Velocity), Without<Prop>>,
    npcs: Query<(Entity, &Transform), (With<Npc>, Without<Prop>)>,
    (mut sfx, mut noise, mut fx, mut stagger): (MessageWriter<PostEvent>, MessageWriter<Noise>, MessageWriter<SpawnEffect>, MessageWriter<NpcStagger>),
    mut tank_blasts: ResMut<TankBlasts>,
    (mut knocks, mut world_hits, mut npc_hits): (ResMut<PropKnocks>, MessageWriter<crate::worlddamage::WorldDamage>, MessageWriter<crate::gameplay::NpcHit>),
) {
    let (Some(level), Some(assets)) = (level, assets) else { return };
    let dt = time.delta_secs();
    for (e, mut prop, it) in &mut props {
        prop.quiet = (prop.quiet - dt).max(0.0);
        if prop.quiet <= 0.0 {
            prop.settling = false;
        }
        let Some(Ok((bt, mut v))) = prop.body.map(|b| bodies.get_mut(b)) else { continue };
        let before = prop.last_vel;
        prop.last_vel = v.linear;
        if prop.thrown && std::env::var("DH_PROP_LOG").is_ok() {
            info!("thrown {}: {:.1} -> {:.1} m/s", level.scene.movables[prop.index].name, before.length(), v.linear.length());
        }
        if held.0 == Some(e) {
            continue;
        }
        // a thrown one flying into someone
        let struck = prop.thrown && before.length() > 3.0 && npcs.iter().any(|(_, nt)| (nt.translation + Vec3::Y * 0.3 - bt.translation).length() < 0.75);
        // a sudden stop is a knock, at the speed it had (a throw speeds it up: no knock)
        let speed = before.length();
        if !struck && (speed < 1.5 || v.linear.length() > speed * 0.75 || (before - v.linear).length() < speed * 0.4) {
            continue;
        }
        if struck {
            // it glances off them
            v.linear = -before * 0.2 + Vec3::Y;
            prop.last_vel = v.linear;
        }
        let m = &level.scene.movables[prop.index];
        let at = bt.translation;
        // against whom
        let body_hit = npcs.iter().find(|(_, nt)| (nt.translation + Vec3::Y * 0.3 - at).length() < 1.0);
        let in_water = waters.as_ref().is_some_and(|w| w.at(at).is_some());
        let against = if body_hit.is_some() {
            "Body"
        } else if in_water {
            "Env_Water"
        } else {
            "Environment"
        };
        if prop.quiet <= 0.0 {
            prop.quiet = 0.15;
            if let Some(im) = impact(m, against) {
                if !im.sound.is_empty() && (!im.scale_with_speed || speed >= im.min_speed * 0.5) {
                    sfx.write(PostEvent::named(&im.sound, Some(at)));
                }
                if let Some(ps) = im.particle {
                    fx.write(SpawnEffect { system: Some(ps), ..SpawnEffect::at("", at) });
                }
                if im.noise > 0.0 {
                    noise.write(Noise { pos: at, radius: im.noise, combat: im.threatening });
                }
            }
        }
        if !prop.settling {
            knocks.0.push((m.instance, speed));
        }
        // the blow of a knock (`DisDamageType_Impact`): on what a thrown thing struck (a PA
        // speaker's scripts count three), and on itself (a fallen speaker breaks)
        if speed > 3.0 {
            use crate::worlddamage::{Reach, WorldDamage};
            if prop.thrown {
                world_hits.write(WorldDamage::player(Reach::Near { at, radius: 0.5, full: 0.5 }, speed * 10.0, "DisDamageType_Impact"));
            } else if let Some(b) = prop.body {
                world_hits.write(WorldDamage { reach: Reach::Hit { collider: b, at }, damage: speed * 10.0, kind: "DisDamageType_Impact", by_player: false });
            }
        }
        // a thrown prop that strikes someone staggers and hurts them (`m_Damage`)
        if let (Some((ne, _)), true) = (body_hit, prop.thrown) {
            stagger.write(NpcStagger { npc: ne, secs: 0.6, parried: false });
            npc_hits.write(crate::gameplay::NpcHit { npc: ne, damage: m.damage, kind: crate::gameplay::HitKind::Impact, from: at });
            noise.write(Noise { pos: at, radius: 6.0, combat: true });
        }
        // (settling as the level starts breaks nothing)
        let breaking = if prop.thrown { m.break_speed } else if prop.quiet > 0.0 && prop.settling { f32::INFINITY } else { m.break_speed_drop };
        if std::env::var("DH_PROP_LOG").is_ok() {
            info!("prop {} knocks {against} at {speed:.1} m/s (breaks at {breaking:.1})", m.name);
        }
        prop.thrown = false;
        if m.breaks.is_some() && speed >= breaking {
            break_prop(&mut commands, &assets, m, e, it, prop.body, &mut sfx, &mut noise, &mut fx, &mut tank_blasts);
        }
    }
}

/// The contact system's entry for a prop against something (the world if nothing closer).
fn impact<'a>(m: &'a Movable, against: &str) -> Option<&'a Impact> {
    m.impacts.iter().find(|i| i.against == against).or_else(|| m.impacts.iter().find(|i| i.against == "Environment"))
}

/// Gone to pieces: its break's sound, noise and effect, its chunks flying.
#[allow(clippy::too_many_arguments)]
fn break_prop(
    commands: &mut Commands,
    assets: &GameAssets,
    m: &Movable,
    e: Entity,
    t: &Transform,
    body: Option<Entity>,
    sfx: &mut MessageWriter<PostEvent>,
    noise: &mut MessageWriter<Noise>,
    fx: &mut MessageWriter<SpawnEffect>,
    tank_blasts: &mut TankBlasts,
) {
    let Some(b) = &m.breaks else { return };
    let at = t.translation;
    if let Some(bl) = &b.blast {
        tank_blasts.0.push((at, bl.clone()));
    }
    tank_blasts.1.push(m.instance);
    if std::env::var("DH_PROP_LOG").is_ok() {
        info!("prop {} breaks at {at:.2} ({} pieces)", m.name, b.chunks.len());
    }
    break_pieces(commands, assets, b, t, sfx, noise, fx);
    commands.entity(e).despawn();
    if let Some(b) = body {
        commands.entity(b).despawn();
    }
}

/// A break's sound, AI noise, effect and pieces (a prop's, a door's), where it stood.
pub fn break_pieces(commands: &mut Commands, assets: &GameAssets, b: &dhcook::format::BreakStep, t: &Transform, sfx: &mut MessageWriter<PostEvent>, noise: &mut MessageWriter<Noise>, fx: &mut MessageWriter<SpawnEffect>) {
    let at = t.translation;
    if !b.sound.is_empty() {
        sfx.write(PostEvent::named(&b.sound, Some(at)));
    }
    if b.noise > 0.0 {
        noise.write(Noise { pos: at, radius: b.noise, combat: b.threatening });
    }
    if let Some(ps) = b.particle {
        fx.write(SpawnEffect { system: Some(ps), ..SpawnEffect::at("", at + t.rotation * Vec3::from(b.particle_offset)) });
    }
    for (mesh, _) in &b.chunks {
        let Some(parts) = assets.prop_chunks.get(mesh) else { continue };
        let dir = Vec3::new(rand::random::<f32>() - 0.5, rand::random::<f32>() * 0.8, rand::random::<f32>() - 0.5).normalize_or(Vec3::Y);
        let mut ec = commands.spawn((
            Chunk(8.0),
            *t,
            Visibility::default(),
            RigidBody::Dynamic,
            Collider::ball(0.08 * t.scale.max_element().max(0.2)),
            CollisionGroups::new(GROUP_PROP, GROUP_WORLD | GROUP_PROP),
            Velocity { linear: dir * 3.0, angular: dir.cross(Vec3::Y) * 8.0 },
            DespawnOnExit(GameState::InGame),
        ));
        ec.with_children(|c| {
            for (mh, mat) in &parts.parts {
                let mut p = c.spawn((Mesh3d(mh.clone()), bevy::mesh::MeshTag(0)));
                mat.apply(&mut p);
            }
        });
    }
}

/// Struck by a blade, a bolt or a bullet, or caught in a blast: worn down, broken, knocked.
#[allow(clippy::too_many_arguments)]
fn prop_hits(
    mut commands: Commands,
    mut struck: MessageReader<Struck>,
    mut blasts: MessageReader<Explosion>,
    level: Option<Res<LevelInfo>>,
    assets: Option<Res<GameAssets>>,
    mut props: Query<(Entity, &mut Prop, &Transform)>,
    mut bodies: Query<&mut Velocity>,
    (mut sfx, mut noise, mut fx): (MessageWriter<PostEvent>, MessageWriter<Noise>, MessageWriter<SpawnEffect>),
    mut tank_blasts: ResMut<TankBlasts>,
    vm: Option<ResMut<crate::kismet::Vm>>,
) {
    let (Some(level), Some(assets)) = (level, assets) else {
        struck.clear();
        blasts.clear();
        return;
    };
    let mut hits: Vec<(Entity, f32, Vec3)> = struck
        .read()
        .map(|s| {
            let dmg = match s.kind {
                HitKind::Explosion | HitKind::EnemyExplosion | HitKind::GrenadeThrowback | HitKind::StickyGrenade | HitKind::ExplosiveBullet | HitKind::Fire => 999.0,
                _ => s.damage,
            };
            (s.target, dmg, s.at)
        })
        .collect();
    // the scripts' damage (`SeqAct_ModifyHealth`: a fallen PA speaker's), no knock with it
    let mut scripted: Vec<Entity> = Vec::new();
    if let Some(mut vm) = vm {
        for (insts, dmg) in std::mem::take(&mut vm.prop_damage) {
            for (e, p, t) in &props {
                if level.scene.movables.get(p.index).is_some_and(|m| insts.contains(&m.instance)) {
                    hits.push((e, dmg, t.translation));
                    scripted.push(e);
                }
            }
        }
    }
    for b in blasts.read() {
        for (e, _, t) in &props {
            let d = t.translation.distance(b.at);
            if d < b.radius {
                hits.push((e, b.damage * (1.0 - d / b.radius).max(0.2), b.at));
            }
        }
    }
    for (e, dmg, from) in hits {
        let Ok((_, mut prop, t)) = props.get_mut(e) else { continue };
        let Some(m) = level.scene.movables.get(prop.index) else { continue };
        // knocked
        if let Some(Ok(mut v)) = prop.body.filter(|_| !scripted.contains(&e)).map(|b| bodies.get_mut(b)) {
            let push = (t.translation - from).normalize_or(Vec3::Y);
            v.linear += (push + Vec3::Y * 0.3) * (dmg * 0.4).clamp(1.0, 8.0) / MASS[m.weight.min(3) as usize].sqrt();
        }
        if dmg < m.threshold || m.breaks.is_none() {
            continue;
        }
        // (broken once: several blasts in one frame, a volley's arrows, hit it only once more)
        let was = prop.health;
        prop.health -= dmg;
        if was > 0.0 && prop.health <= 0.0 {
            let t = *t;
            break_prop(&mut commands, &assets, m, e, &t, prop.body, &mut sfx, &mut noise, &mut fx, &mut tank_blasts);
        }
    }
}

/// The loose props' looks follow their bodies; pieces fade away.
fn sync_props(
    mut commands: Commands,
    time: Res<Time>,
    props: Query<(&Prop, Entity)>,
    bodies: Query<&Transform, Without<Prop>>,
    mut visuals: Query<&mut Transform, With<Prop>>,
    mut chunks: Query<(Entity, &mut Chunk)>,
) {
    for (prop, e) in &props {
        let Some(Ok(bt)) = prop.body.map(|b| bodies.get(b)) else { continue };
        if let Ok(mut t) = visuals.get_mut(e) {
            let at = bt.translation - bt.rotation * prop.offset;
            if t.translation != at || t.rotation != bt.rotation {
                t.translation = at;
                t.rotation = bt.rotation;
            }
        }
    }
    for (e, mut c) in &mut chunks {
        c.0 -= time.delta_secs();
        if c.0 <= 0.0 {
            commands.entity(e).despawn();
        }
    }
}
