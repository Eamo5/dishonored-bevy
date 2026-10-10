//! River krusts (`DisRiverKrust` + `DisTweaks_RiverKrust`): the shellfish clamped to the
//! Flooded District's walls. Shut in its shell (`m_ProtectedAnimStates`) it shrugs off blades
//! and bolts; when someone comes within its aggressive range and in sight it opens and spits
//! volleys of acid (`Twk_Proj_RiverKrust_NonExplosive`: a slow arc, `m_ProjectileDamage` by
//! difficulty, led at a moving target), and clamps shut when they come close. Open, it has a
//! few points of health (`m_MaximumHealth`); dead, the pearl inside can be taken. Its states and
//! their sequences are the tweak's (`m_AnimSequences` by `ERiverKrustAnimState`), the damage
//! it takes is filtered by type (`m_DamageFilters`, `m_ProtectedDamageFilters`), and the level
//! scripts can have it spit at something (`DisSeqAct_RiverKrustSpitAtTarget`).

use crate::anim::Animator;
use crate::audio::{event_id, PostEvent, StopEvent};
use crate::gadgets::Explosion;
use crate::gameplay::{HitKind, PlayerStats, Strikeable, Struck, TimeControl};
use crate::interact::{Pickup, PickupKind};
use crate::level::{GameAssets, LevelInfo, GROUP_PROP, GROUP_WORLD};
use crate::particles::SpawnEffect;
use crate::player::{Player, PlayerCamera};
use crate::world_light::{LitActor, WorldLighting};
use crate::GameState;
use bevy::camera::visibility::DynamicSkinnedMeshBounds;
use bevy::mesh::skinning::SkinnedMesh;
use bevy::mesh::MeshTag;
use bevy::prelude::*;
use bevy_rapier3d::prelude::*;
use dhcook::format::Krust as KrustDef;
use dhcook::xform::{rot_matrix, ue_to_bevy};
use std::collections::HashMap;

pub struct KrustPlugin;

impl Plugin for KrustPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<KrustLog>()
            .add_systems(OnEnter(GameState::InGame), spawn_krusts.after(crate::level::LevelSpawnSet))
            .add_systems(Update, (restore_krusts, scripted_spits, krust_hits, possessed_krust, krust_brain, fly_spit).chain().run_if(in_state(GameState::InGame)));
    }
}

// `ERiverKrustAnimState`
const AMBIENT: usize = 0;
const DEFENSIVE: usize = 1;
const AGGRESSIVE: usize = 2;
const AMBIENT_TO_DEFENSIVE: usize = 3;
const AMBIENT_TO_AGGRESSIVE: usize = 4;
const DEFENSIVE_TO_AMBIENT: usize = 5;
const DEFENSIVE_TO_AGGRESSIVE: usize = 6;
const AGGRESSIVE_TO_AMBIENT: usize = 7;
const AGGRESSIVE_TO_DEFENSIVE: usize = 8;
const FIRING: usize = 9;
const AMBIENT_DEATH: usize = 10;
const DEAD: usize = 13;
const POSSESSED: usize = 14;
const DEFENSIVE_HIT_REACT: usize = 15;

/// `m_ProtectedAnimStates`: shut in the shell
const PROTECTED: [usize; 5] = [AMBIENT, DEFENSIVE, AMBIENT_TO_DEFENSIVE, DEFENSIVE_TO_AMBIENT, DEFENSIVE_HIT_REACT];
/// `m_DeathAnimStateMap`: the death of each state
const DEATH_OF: [usize; 17] = [10, 11, 12, 11, 12, 10, 12, 10, 11, 12, 10, 11, 12, 10, 10, 11, 10];
/// the original's gravity (1500 uu/s², as the player's jump)
const GRAVITY: f32 = 15.0;
/// the death notify's effect (`AnimNotify_PlayParticleEffect` at 0.43 s)
const DEATH_FX_AT: f32 = 0.43;
/// `DishonoredNotify_FireProjectile` and the spew of `Spit_Attack`
const FIRE_AT: f32 = 0.25;
/// `m_fLostTargetRetentionTime`
const LOST_RETENTION: f32 = 2.0;
const LOOP_SOUND: &str = "River_Krust_Hostile_Idle_Loop";

/// What the shell does about who's around (`RiverKrustState_*`).
#[derive(Clone, Copy, PartialEq, Debug)]
enum Mode {
    Ambient,
    Defensive,
    Aggressive,
}

/// Krusts killed in this level and whether their pearl was taken (for saves).
#[derive(Resource, Default)]
pub struct KrustLog {
    pub dead: HashMap<u32, bool>,
    /// a save was loaded: put the dead back
    pub restore: bool,
}

#[derive(Component)]
pub struct Krust {
    /// where it looks (level): its field of fire is about it
    pub facing: Vec3,
    index: u32,
    state: usize,
    mode: Mode,
    hp: f32,
    /// someone inside the defensive range for; out of the mode's range for
    near_t: f32,
    away_t: f32,
    exit_delay: f32,
    /// until the next volley, shots left in this one
    volley: f32,
    shots: u32,
    fired: bool,
    death_fx: bool,
    /// a hit keeps it shut
    reaction: f32,
    lost: f32,
    /// the scripts' target
    order: Option<Vec3>,
    launch: (Entity, Transform),
    spew: (Entity, Transform),
    vision: (Entity, Transform),
    center: Entity,
    pearl: Option<Entity>,
    pearl_pickup: Option<Entity>,
    pearl_lying: bool,
    collider: Entity,
    /// where it looked from and what blocked its view (diagnostics)
    eye: Vec3,
    blocked: Option<Vec3>,
}

impl Krust {
    fn protected(&self) -> bool {
        PROTECTED.contains(&self.state)
    }
    fn dead(&self) -> bool {
        self.state >= AMBIENT_DEATH && self.state <= DEAD
    }
    pub fn index(&self) -> u32 {
        self.index
    }
    /// the collider weapons strike
    pub fn body(&self) -> Entity {
        self.collider
    }
    pub fn describe(&self) -> String {
        format!(
            "#{} {:?} state {} hp {:.1} volley {:.1} shots {} pearl {} lost {:.1} eye {:.2} blocked {:?}",
            self.index,
            self.mode,
            self.state,
            self.hp,
            self.volley,
            self.shots,
            self.pearl.is_some(),
            self.lost,
            self.eye,
            self.blocked
        )
    }
}

#[cfg(test)]
mod cover_tests {
    use super::*;

    fn test_krust(collider: Entity) -> Krust {
        let socket = (Entity::PLACEHOLDER, Transform::IDENTITY);
        Krust {
            facing: Vec3::X, index: 0, state: AGGRESSIVE, mode: Mode::Aggressive, hp: 10.0,
            near_t: 0.0, away_t: 0.0, exit_delay: 0.0, volley: 0.0, shots: 0, fired: false,
            death_fx: false, reaction: 0.0, lost: 0.0, order: None, launch: socket, spew: socket,
            vision: socket, center: Entity::PLACEHOLDER, pearl: None, pearl_pickup: None,
            pearl_lying: false, collider, eye: Vec3::ZERO, blocked: None,
        }
    }

    #[test]
    fn possessed_krust_ignores_menu_clicks_and_restores_player_animation_clock() {
        let mut app = App::new();
        app.init_resource::<ButtonInput<MouseButton>>().insert_resource(crate::hud::Paused(true))
            .init_resource::<crate::settings::Settings>()
            .insert_resource(LevelInfo { scene: dhcook::format::Scene { krusts: vec![KrustDef::default()], ..default() } })
            .add_message::<SpawnEffect>().add_systems(Update, possessed_krust);
        let skeleton = dhcook::format::SkeletonDef::default();
        let lib = std::sync::Arc::new(crate::anim::CharAnims::new(&skeleton, vec![]));
        let mut anim = Animator::new(lib, &skeleton, vec![]);
        anim.time_scale = 0.0;
        anim.frozen = true;
        let mut krust = test_krust(Entity::PLACEHOLDER);
        krust.state = POSSESSED;
        let entity = app.world_mut().spawn((krust, anim, crate::possession::Possessed)).id();
        app.world_mut().spawn((PlayerCamera, GlobalTransform::IDENTITY));
        app.world_mut().resource_mut::<ButtonInput<MouseButton>>().press(MouseButton::Left);
        app.update();
        let k = app.world().get::<Krust>(entity).unwrap();
        assert_eq!(k.state, POSSESSED);
        assert!(k.order.is_none());
        app.world_mut().resource_mut::<ButtonInput<MouseButton>>().reset_all();
        app.world_mut().resource_mut::<crate::hud::Paused>().0 = false;
        app.update();
        assert_eq!(app.world().get::<Krust>(entity).unwrap().state, POSSESSED);
        let anim = app.world().get::<Animator>(entity).unwrap();
        assert_eq!(anim.time_scale, 1.0);
        assert!(!anim.frozen);
        app.world_mut().resource_mut::<ButtonInput<MouseButton>>().press(MouseButton::Left);
        app.update();
        let k = app.world().get::<Krust>(entity).unwrap();
        assert_eq!(k.state, FIRING);
        assert!(k.order.is_some());
    }

    #[test]
    fn krust_blasts_use_shell_position_cover_and_remove_dead_hosts() {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, AssetPlugin::default(), bevy::scene::ScenePlugin, TransformPlugin, RapierPhysicsPlugin::<NoUserData>::default()))
            .init_resource::<Assets<Mesh>>().init_resource::<KrustLog>()
            .insert_resource(LevelInfo { scene: dhcook::format::Scene { krusts: vec![KrustDef::default()], ..default() } })
            .add_message::<Struck>().add_message::<Explosion>().add_message::<PostEvent>()
            .add_message::<StopEvent>().add_message::<SpawnEffect>()
            .add_systems(Last, (restore_krusts, krust_hits).chain());
        let shell = app.world_mut().spawn((Transform::from_xyz(2.0, 0.0, 0.0), Collider::ball(0.35),
            CollisionGroups::new(GROUP_PROP, Group::ALL), Strikeable(Entity::PLACEHOLDER))).id();
        let skeleton = dhcook::format::SkeletonDef::default();
        let lib = std::sync::Arc::new(crate::anim::CharAnims::new(&skeleton, vec![]));
        let host = crate::possession::Host { npc_type: None, rooted: true, fish: false, seat: Vec3::ZERO, facing: Vec3::X };
        let entity = app.world_mut().spawn((test_krust(shell), Animator::new(lib, &skeleton, vec![]), Transform::from_xyz(20.0, 0.0, 0.0), host)).id();
        let cover = app.world_mut().spawn((Transform::from_xyz(1.0, 0.0, 0.0), Collider::cuboid(0.01, 2.0, 2.0),
            CollisionGroups::new(GROUP_WORLD, Group::ALL))).id();
        let blast = |app: &mut App| {
            app.world_mut().write_message(Explosion { at: Vec3::ZERO, radius: 3.0, full: 3.0, damage: 100.0,
                effect: "", player: None, kind: HitKind::Explosion });
            app.update();
        };
        app.update();
        blast(&mut app);
        assert_eq!(app.world().get::<Krust>(entity).unwrap().hp, 10.0);
        app.world_mut().entity_mut(cover).insert(CollisionGroups::new(GROUP_PROP, Group::ALL));
        blast(&mut app);
        assert_eq!(app.world().get::<Krust>(entity).unwrap().hp, 10.0);
        app.world_mut().entity_mut(cover).insert(Sensor);
        blast(&mut app);
        assert!(app.world().get::<Krust>(entity).unwrap().dead(), "the exposed shell is in range even though its actor origin is not");
        assert!(app.world().get::<crate::possession::Host>(entity).is_none());
        assert!(app.world().get::<Strikeable>(shell).is_none());
        assert_eq!(app.world().resource::<KrustLog>().dead.get(&0), Some(&false));
        // A map spawn restores Host before the dead log is applied on load.
        app.world_mut().entity_mut(entity).insert(crate::possession::Host { npc_type: None, rooted: true, fish: false, seat: Vec3::ZERO, facing: Vec3::X });
        app.world_mut().resource_mut::<KrustLog>().restore = true;
        app.update();
        assert_eq!(app.world().get::<Krust>(entity).unwrap().state, DEAD);
        assert!(app.world().get::<crate::possession::Host>(entity).is_none());
    }
}

/// A gob of acid in flight.
#[derive(Component)]
struct Spit {
    source: Entity,
    vel: Vec3,
    gravity: f32,
    damage: f32,
    life: f32,
}

/// The pearl of a dead krust, lying there to be taken.
#[derive(Component)]
struct KrustPearl;

fn rand_in(r: [f32; 2]) -> f32 {
    r[0] + (r[1] - r[0]).max(0.0) * rand::random::<f32>()
}

fn spawn_krusts(mut commands: Commands, assets: Option<Res<GameAssets>>, level: Option<Res<LevelInfo>>, mut wl: Option<ResMut<WorldLighting>>, mut log: ResMut<KrustLog>) {
    *log = KrustLog::default();
    let (Some(assets), Some(level)) = (assets, level) else { return };
    let mut n = 0;
    for (i, def) in level.scene.krusts.iter().enumerate() {
        let Some(vis) = def.npc_type.and_then(|t| assets.npc_types.get(t as usize)).and_then(|v| v.as_ref()) else { continue };
        let slot = wl.as_mut().map(|w| w.alloc_slot()).unwrap_or(0);
        spawn_krust(&mut commands, &assets, vis, def, i as u32, slot);
        n += 1;
    }
    if n > 0 {
        info!("{n} river krusts");
    }
}

fn spawn_krust(commands: &mut Commands, assets: &GameAssets, vis: &crate::level::NpcVisual, def: &KrustDef, index: u32, slot: u32) {
    let rot = Transform::from_matrix(Mat4::from_cols_array(&ue_to_bevy(rot_matrix(def.rotation)).to_cols_array())).rotation;
    let root = commands
        .spawn((
            Transform::from_translation(Vec3::from(def.position)).with_rotation(rot).with_scale(Vec3::splat(def.scale.max(0.1))),
            Visibility::default(),
            LitActor { slot, probe_height: 0.3, brightness: 0.3, color: Vec3::splat(0.05), sun: 0.0, dominant: None },
            DespawnOnExit(GameState::InGame),
        ))
        .id();
    // the skeleton, straight under the actor (the mesh component has no offset)
    let bones = &vis.skeleton.bones;
    let mut joints = Vec::with_capacity(bones.len());
    let mut model: Vec<Transform> = Vec::with_capacity(bones.len());
    for b in bones {
        let local = Transform::from_translation(Vec3::from(b.translation)).with_rotation(Quat::from_array(b.rotation).normalize());
        let m = if b.parent >= 0 && (b.parent as usize) < model.len() { model[b.parent as usize] * local } else { local };
        model.push(m);
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
    let bone = |name: &str| bones.iter().position(|b| b.name.eq_ignore_ascii_case(name));
    let socket = |name: &str| -> (Entity, Transform) {
        match vis.skeleton.sockets.iter().find(|s| s.name.eq_ignore_ascii_case(name)) {
            Some(s) => (
                bone(&s.bone).map(|i| joints[i]).unwrap_or(root),
                Transform::from_translation(Vec3::from(s.translation)).with_rotation(Quat::from_array(s.rotation).normalize()),
            ),
            None => (joints.first().copied().unwrap_or(root), Transform::IDENTITY),
        }
    };
    let center_i = bone("center_jnt").unwrap_or(0);
    let center = joints.get(center_i).copied().unwrap_or(root);
    // the stalk it grows on and the pearl inside
    let parts = assets.krust_parts.get(index as usize);
    if let Some(stalk) = parts.and_then(|p| p.0.as_ref()) {
        let (j, at) = socket("Stalk_Attach");
        for (mesh, mat) in &stalk.parts {
            let mut ec = commands.spawn((Mesh3d(mesh.clone()), MeshTag(slot), at));
            mat.apply(&mut ec);
            let s = ec.id();
            commands.entity(j).add_child(s);
        }
    }
    let mut pearl = None;
    if let Some(pp) = parts.and_then(|p| p.1.as_ref()) {
        let (j, at) = socket("Pearl_Big");
        let holder = commands.spawn((at, Visibility::default())).id();
        commands.entity(j).add_child(holder);
        for (mesh, mat) in &pp.parts {
            let mut ec = commands.spawn((Mesh3d(mesh.clone()), MeshTag(slot), Transform::IDENTITY));
            mat.apply(&mut ec);
            let s = ec.id();
            commands.entity(holder).add_child(s);
        }
        pearl = Some(holder);
    }
    // the shell's body: what blades, bolts and bullets strike
    let c = model.get(center_i).map(|m| m.translation).unwrap_or(Vec3::ZERO);
    let launch = socket("ProjectileLaunch");
    let vision = socket("Vision");
    // where Corvo looks out from when he possesses it
    let model_of = |(j, at): (Entity, Transform)| joints.iter().position(|x| *x == j).and_then(|i| model.get(i)).map(|m| (*m * at).translation);
    let seat = model_of(socket("Possess")).map(|p| rot * (p * def.scale.max(0.1))).unwrap_or(Vec3::Y * 0.6);
    // it stands mouth up, looking out along its `Vision` socket: its field of fire is about that
    let look = joints.iter().position(|j| *j == vision.0).and_then(|i| model.get(i)).map(|m| (*m * vision.1).rotation * Vec3::X).unwrap_or(Vec3::X);
    let facing = (rot * look).with_y(0.0).normalize_or(rot * Vec3::X);
    let collider = commands
        .spawn((
            Transform::from_translation(c),
            Collider::ball(0.35),
            CollisionGroups::new(GROUP_PROP, Group::ALL),
            Strikeable(root),
        ))
        .id();
    commands.entity(root).add_child(collider);
    let mut k = Krust {
        facing,
        index,
        state: AMBIENT,
        mode: Mode::Ambient,
        hp: def.health.max(1.0),
        near_t: 0.0,
        away_t: 0.0,
        exit_delay: 0.0,
        volley: rand_in(def.first_volley),
        shots: 0,
        fired: false,
        death_fx: false,
        reaction: 0.0,
        lost: LOST_RETENTION,
        order: None,
        launch,
        spew: socket("Spew"),
        vision,
        center,
        pearl,
        pearl_pickup: None,
        pearl_lying: false,
        collider,
        eye: Vec3::ZERO,
        blocked: None,
    };
    if let Some(lib) = vis.anims.clone() {
        let mut a = Animator::new(lib, &vis.skeleton, joints);
        play(&mut a, def, AMBIENT, true);
        k.state = AMBIENT;
        commands.entity(root).insert(a);
    }
    commands.entity(root).insert((k, crate::possession::Host { npc_type: def.npc_type, rooted: true, fish: false, seat, facing }));
}

/// Play a state's sequence (its rate and start from the tweak's ranges).
fn play(a: &mut Animator, def: &KrustDef, state: usize, fresh: bool) {
    let Some(spec) = def.anims.get(state) else { return };
    let Some(clip) = a.lib.find(&spec.name) else { return };
    let rate = rand_in(spec.rate).max(0.05);
    let blend = if fresh { 0.0 } else { spec.blend.max(0.05) };
    // the resting states hold (the hostile idle isn't flagged looping: the original replays it)
    let looping = spec.looping || matches!(state, AMBIENT | DEFENSIVE | AGGRESSIVE | DEAD);
    if spec.restart || !looping {
        a.restart(clip, looping, rate, blend);
    } else {
        a.play(clip, looping, rate, blend);
    }
    if spec.start > 0.0 {
        a.seek(rand::random::<f32>() * spec.start);
    }
}

/// The transition between two modes.
fn transition(from: Mode, to: Mode) -> usize {
    match (from, to) {
        (Mode::Ambient, Mode::Defensive) => AMBIENT_TO_DEFENSIVE,
        (Mode::Ambient, Mode::Aggressive) => AMBIENT_TO_AGGRESSIVE,
        (Mode::Defensive, Mode::Ambient) => DEFENSIVE_TO_AMBIENT,
        (Mode::Defensive, Mode::Aggressive) => DEFENSIVE_TO_AGGRESSIVE,
        (Mode::Aggressive, Mode::Ambient) => AGGRESSIVE_TO_AMBIENT,
        (Mode::Aggressive, Mode::Defensive) => AGGRESSIVE_TO_DEFENSIVE,
        (_, Mode::Ambient) => AMBIENT,
        (_, Mode::Defensive) => DEFENSIVE,
        (_, Mode::Aggressive) => AGGRESSIVE,
    }
}

fn idle(m: Mode) -> usize {
    match m {
        Mode::Ambient => AMBIENT,
        Mode::Defensive => DEFENSIVE,
        Mode::Aggressive => AGGRESSIVE,
    }
}

fn world(joints: &Query<&GlobalTransform>, (j, at): (Entity, Transform)) -> Option<GlobalTransform> {
    joints.get(j).ok().map(|g| g.mul_transform(at))
}

/// Saved dead krusts: lying dead again, the pearl gone if it was taken.
fn restore_krusts(
    mut commands: Commands,
    mut log: ResMut<KrustLog>,
    level: Option<Res<LevelInfo>>,
    mut krusts: Query<(Entity, &mut Krust, &mut Animator)>,
) {
    if !log.restore {
        return;
    }
    log.restore = false;
    let Some(level) = level else { return };
    for (e, mut k, mut a) in &mut krusts {
        let Some(&taken) = log.dead.get(&k.index) else { continue };
        let Some(def) = level.scene.krusts.get(k.index as usize) else { continue };
        k.state = DEAD;
        k.death_fx = true;
        play(&mut a, def, DEAD, true);
        commands.entity(k.collider).remove::<Strikeable>();
        commands.entity(e).remove::<crate::possession::Host>();
        if taken {
            if let Some(p) = k.pearl.take() {
                commands.entity(p).despawn();
            }
        }
    }
}

/// `DisSeqAct_RiverKrustSpitAtTarget`: the scripts point a krust at something.
fn scripted_spits(vm: Option<ResMut<crate::kismet::Vm>>, mut krusts: Query<&mut Krust>) {
    let Some(mut vm) = vm else { return };
    if vm.krust_spits.is_empty() {
        return;
    }
    for (index, at) in std::mem::take(&mut vm.krust_spits) {
        if let Some(mut k) = krusts.iter_mut().find(|k| k.index == index) {
            if !k.dead() {
                if std::env::var("DH_KRUST_LOG").is_ok() {
                    info!("krust #{index}: the scripts have it spit at {at:.2}");
                }
                k.order = Some(at);
            }
        }
    }
}

/// The damage type a weapon deals, as the filters name it.
fn damage_type(kind: HitKind) -> &'static str {
    match kind {
        HitKind::Sword | HitKind::Choke => "DishonoredDamageType_FastHit",
        HitKind::Assassinate | HitKind::Fatality => "DisDamageType_Assassination",
        HitKind::Bullet => "DishonoredDamageType_Bullet",
        HitKind::Explosion | HitKind::EnemyExplosion | HitKind::GrenadeThrowback | HitKind::StickyGrenade | HitKind::ExplosiveBullet => "DishonoredDamageType_Explosion",
        HitKind::Windblast => "DisDamageType_WindBlast",
        HitKind::Rats => "DishonoredDamageType_Plague",
        HitKind::Bolt | HitKind::Fire | HitKind::SleepDart | HitKind::ByOthers => "DisDamageType_Arrow",
        HitKind::Impact => "DisDamageType_Impact",
        HitKind::SpringRazor => "DisDamageType_SpringRazor",
        HitKind::WallOfLight => "DisDamageType_WallOfLight",
    }
}

#[allow(clippy::too_many_arguments)]
fn krust_hits(
    mut commands: Commands,
    mut struck: MessageReader<Struck>,
    mut blasts: MessageReader<Explosion>,
    level: Option<Res<LevelInfo>>,
    mut log: ResMut<KrustLog>,
    mut krusts: Query<(Entity, &mut Krust, &mut Animator, &GlobalTransform)>,
    mut sfx: MessageWriter<PostEvent>,
    mut stop: MessageWriter<StopEvent>,
    mut fx: MessageWriter<SpawnEffect>,
    (rapier, globals): (ReadRapierContext, Query<&GlobalTransform>),
) {
    let Some(level) = level else {
        struck.clear();
        blasts.clear();
        return;
    };
    let mut hits: Vec<(Entity, &'static str, f32, Vec3)> = struck.read().map(|s| (s.target, damage_type(s.kind), s.damage, s.at)).collect();
    let context = rapier.single().ok();
    for b in blasts.read() {
        for (e, k, _, g) in &krusts {
            let at = globals.get(k.collider).map(|g| g.translation()).unwrap_or(g.translation());
            let d = at.distance(b.at);
            let blocked = d < b.radius && d > 0.001 && context.as_ref().is_some_and(|ctx| ctx.cast_ray(b.at, (at - b.at) / d, d, true,
                QueryFilter::default().exclude_collider(k.collider).exclude_sensors()
                    .groups(CollisionGroups::new(Group::ALL, GROUP_WORLD | GROUP_PROP))).is_some());
            if d < b.radius && !blocked {
                let k = if d <= b.full { 1.0 } else { (1.0 - (d - b.full) / (b.radius - b.full).max(0.01)).max(0.0) };
                hits.push((e, "DishonoredDamageType_Explosion", b.damage * k, b.at));
            }
        }
    }
    for (e, ty, damage, at) in hits {
        let Ok((_, mut k, mut a, g)) = krusts.get_mut(e) else { continue };
        let Some(def) = level.scene.krusts.get(k.index as usize) else { continue };
        if k.dead() {
            continue;
        }
        let protected = k.protected();
        let filters = if protected { &def.protected_filters } else { &def.filters };
        let mult = filters.iter().find(|(n, _)| n == ty).map(|f| f.1).unwrap_or(if protected { 0.0 } else { 1.0 });
        if protected && ty == "DishonoredDamageType_FastHit" {
            // the blade glances off the shell
            fx.write(SpawnEffect { rot: Transform::IDENTITY.looking_to(at - g.translation(), Vec3::Y).rotation, ..SpawnEffect::at("krust_sword", at) });
        }
        let dmg = damage * mult;
        if dmg > 0.0 {
            k.hp -= dmg;
        }
        if k.hp <= 0.0 {
            // dead: its last cry, the open-shell loop stops
            let death = DEATH_OF[k.state.min(16)];
            k.state = death;
            k.death_fx = false;
            play(&mut a, def, death, false);
            stop.write(StopEvent(event_id(LOOP_SOUND)));
            commands.entity(k.collider).remove::<Strikeable>();
            commands.entity(e).remove::<crate::possession::Host>();
            log.dead.insert(k.index, false);
            if std::env::var("DH_KRUST_LOG").is_ok() {
                info!("krust {} killed ({ty}, {dmg:.1})", def.name);
            }
            continue;
        }
        // hit, still alive: it shuts for a while (`m_DamageReactionState`)
        k.reaction = rand_in(def.reaction);
        if k.mode == Mode::Defensive && matches!(k.state, DEFENSIVE | DEFENSIVE_HIT_REACT) {
            k.state = DEFENSIVE_HIT_REACT;
            play(&mut a, def, DEFENSIVE_HIT_REACT, false);
        }
        if std::env::var("DH_KRUST_LOG").is_ok() {
            info!("krust {} hit ({ty}, x{mult}, {dmg:.1}; {:.1} left)", def.name, k.hp);
        }
        sfx.write(PostEvent::named("River_Krust_Pain", Some(g.translation())));
    }
}

#[allow(clippy::too_many_arguments, clippy::type_complexity)]
fn krust_brain(
    mut commands: Commands,
    (time, tc, settings): (Res<Time>, Res<TimeControl>, Res<crate::settings::Settings>),
    level: Option<Res<LevelInfo>>,
    rapier: ReadRapierContext,
    stats: Res<PlayerStats>,
    possession: Res<crate::possession::Possession>,
    mut log: ResMut<KrustLog>,
    player: Query<(&Transform, &Player)>,
    mut krusts: Query<(&mut Krust, &mut Animator, &GlobalTransform), Without<crate::possession::Possessed>>,
    joints: Query<&GlobalTransform>,
    pickups: Query<(), With<Pickup>>,
    (mut sfx, mut stop, mut fx): (MessageWriter<PostEvent>, MessageWriter<StopEvent>, MessageWriter<SpawnEffect>),
    mut looping: Local<bool>,
    names: Query<(Option<&crate::level::LevelInstance>, Option<&Name>, Option<&Collider>)>,
) {
    let Some(level) = level else { return };
    let dt = time.delta_secs() * tc.world_scale();
    let ctx = rapier.single().ok();
    let target = player.single().ok().filter(|_| !stats.dead && possession.host.is_none()).map(|(t, p)| (t.translation + Vec3::Y * 0.35, p.velocity));
    let walls = QueryFilter::default().groups(CollisionGroups::new(Group::ALL, GROUP_WORLD));
    let log_on = std::env::var("DH_KRUST_LOG").is_ok();
    let mut any_open = false;
    for (mut k, mut a, g) in &mut krusts {
        let Some(def) = level.scene.krusts.get(k.index as usize) else { continue };
        a.time_scale = tc.world_scale();
        // ---- dead: the death plays out, the pearl can be taken
        if k.dead() {
            let t = a.current().map(|c| c.t).unwrap_or(0.0);
            if !k.death_fx && t >= DEATH_FX_AT {
                k.death_fx = true;
                if let Some(at) = joints.get(k.center).ok().map(|j| j.translation()) {
                    fx.write(SpawnEffect::at("krust_death", at));
                }
            }
            if k.state != DEAD && a.finished() {
                k.state = DEAD;
                play(&mut a, def, DEAD, false);
            }
            if k.state == DEAD && k.pearl_pickup.is_none() && log.dead.get(&k.index) == Some(&false) {
                if let Some((p, pg)) = k.pearl.and_then(|p| joints.get(p).ok().map(|g| (p, g.translation()))) {
                    let e = commands
                        .spawn((
                            Pickup { kind: PickupKind::Coins(def.loot_coins.max(1)), label: def.pearl_name.clone(), entities: vec![p], index: u32::MAX },
                            KrustPearl,
                            Transform::from_translation(pg),
                            DespawnOnExit(GameState::InGame),
                        ))
                        .id();
                    k.pearl_pickup = Some(e);
                }
            }
            // taken (once it was there to take)
            if let Some(e) = k.pearl_pickup {
                match pickups.get(e) {
                    Ok(()) => k.pearl_lying = true,
                    Err(_) if k.pearl_lying && k.pearl.is_some() => {
                        k.pearl = None;
                        log.dead.insert(k.index, true);
                    }
                    Err(_) => {}
                }
            }
            continue;
        }
        let me = g.translation();
        // just let go by Corvo: shut, then as before
        if k.state == POSSESSED {
            k.state = DEFENSIVE;
            k.mode = Mode::Defensive;
            k.exit_delay = rand_in(def.defensive_exit_delay);
            play(&mut a, def, DEFENSIVE, false);
        }
        k.reaction = (k.reaction - dt).max(0.0);
        // ---- what it makes of whoever is there
        let (dist, seen, aim) = match target.or(k.order.map(|o| (o, Vec3::ZERO))) {
            Some((at, vel)) => {
                let d = me.distance(at);
                let eye = world(&joints, k.vision).map(|v| v.translation()).unwrap_or(me + Vec3::Y * 0.3);
                k.eye = eye;
                let mut blocked = None;
                let visible = d < def.aggressive[1] + 1.0
                    && ctx.as_ref().is_some_and(|c| {
                        let to = at - eye;
                        let l = to.length();
                        if l < 0.01 {
                            return true;
                        }
                        blocked = c.cast_ray(eye, to / l, l - 0.3, true, walls).map(|(e, toi)| {
                            if log_on && rand::random::<f32>() < 0.01 {
                                let n = names.get(e).ok().map(|(li, n, c)| (li.map(|l| (l.actor.clone(), l.class.clone())), n.cloned(), c.map(|c| format!("{:?}", c.raw.shape_type()))));
                                info!("krust {}: view blocked by {e} {n:?}", def.name);
                            }
                            eye + to / l * toi
                        });
                        blocked.is_none()
                    });
                k.blocked = blocked;
                if visible {
                    k.lost = 0.0;
                } else {
                    k.lost += dt;
                }
                (d, k.lost < LOST_RETENTION, Some((at, vel)))
            }
            None => {
                k.lost += dt;
                (f32::INFINITY, false, None)
            }
        };
        let scripted = k.order.is_some();
        let close = target.is_some() && dist < def.defensive[0];
        let want = if k.reaction > 0.0 {
            Mode::Defensive
        } else {
            match k.mode {
                Mode::Defensive => {
                    if dist > def.defensive[1] || target.is_none() {
                        k.away_t += dt;
                        if k.away_t >= k.exit_delay {
                            if seen && dist < def.aggressive[0] {
                                Mode::Aggressive
                            } else {
                                Mode::Ambient
                            }
                        } else {
                            Mode::Defensive
                        }
                    } else {
                        k.away_t = 0.0;
                        Mode::Defensive
                    }
                }
                _ if close => {
                    k.near_t += dt;
                    if k.near_t >= rand_in(def.defensive_delay) {
                        Mode::Defensive
                    } else {
                        k.mode
                    }
                }
                Mode::Aggressive => {
                    k.near_t = 0.0;
                    if !scripted && (!seen || dist > def.aggressive[1]) {
                        k.away_t += dt;
                        if k.away_t >= k.exit_delay {
                            Mode::Ambient
                        } else {
                            Mode::Aggressive
                        }
                    } else {
                        k.away_t = 0.0;
                        Mode::Aggressive
                    }
                }
                Mode::Ambient => {
                    k.near_t = 0.0;
                    if scripted || (seen && dist < def.aggressive[0]) {
                        Mode::Aggressive
                    } else {
                        Mode::Ambient
                    }
                }
            }
        };
        if want != k.mode {
            if log_on {
                info!("krust {}: {:?} -> {:?} (d {dist:.1}, seen {seen})", def.name, k.mode, want);
            }
            k.exit_delay = match want {
                Mode::Defensive => rand_in(def.defensive_exit_delay),
                Mode::Aggressive => rand_in(def.aggressive_exit_delay),
                Mode::Ambient => 0.0,
            };
            k.away_t = 0.0;
            k.near_t = 0.0;
            let from = k.mode;
            k.mode = want;
            // shutting can't wait; the rest finish what they're doing first
            let busy = matches!(k.state, FIRING) || (!PROTECTED.contains(&k.state) && k.state != AGGRESSIVE && !a.finished());
            if want == Mode::Defensive || !busy {
                let s = transition(from, want);
                k.state = s;
                play(&mut a, def, s, false);
            }
            if want == Mode::Aggressive {
                k.volley = rand_in(def.first_volley);
            }
        }
        // ---- transitions run to their end
        let stable = matches!(k.state, AMBIENT | DEFENSIVE | AGGRESSIVE);
        if !stable && k.state != FIRING && a.finished() {
            let s = idle(k.mode);
            if s == AGGRESSIVE && k.state != AGGRESSIVE {
                sfx.write(PostEvent::named(LOOP_SOUND, Some(me)));
            }
            k.state = s;
            play(&mut a, def, s, false);
        } else if stable && k.state != idle(k.mode) {
            let s = transition(match k.state {
                AMBIENT => Mode::Ambient,
                DEFENSIVE => Mode::Defensive,
                _ => Mode::Aggressive,
            }, k.mode);
            k.state = s;
            play(&mut a, def, s, false);
        }
        // ---- volleys
        if k.mode == Mode::Aggressive {
            any_open = true;
            let target_at = k.order.or(aim.map(|a| a.0));
            let in_cone = target_at.is_some_and(|t| k.facing.dot((t - me).with_y(0.0).normalize_or_zero()) >= (def.fire_angle.to_radians() * 0.5).cos());
            match k.state {
                AGGRESSIVE => {
                    k.volley -= dt;
                    if (k.volley <= 0.0 && seen && in_cone) || (scripted && in_cone) {
                        k.shots = if scripted { 1 } else { def.shots[0] + rand::random_range(0..=(def.shots[1].saturating_sub(def.shots[0]))) };
                        k.state = FIRING;
                        k.fired = false;
                        play(&mut a, def, FIRING, false);
                    }
                }
                FIRING => {
                    let t = a.current().map(|c| c.t).unwrap_or(0.0);
                    if !k.fired && t >= FIRE_AT {
                        k.fired = true;
                        if let (Some(launch), Some(spew)) = (world(&joints, k.launch), world(&joints, k.spew)) {
                            let (at, vel) = match k.order.take() {
                                Some(o) => (o, Vec3::ZERO),
                                None => aim.unwrap_or((me + Vec3::Y, Vec3::ZERO)),
                            };
                            fire(&mut commands, def, settings.difficulty, launch.translation(), at, vel, k.collider);
                            if log_on {
                                info!("krust {}: spits from {:.2} at {at:.2}", def.name, launch.translation());
                            }
                            fx.write(SpawnEffect { rot: spew.rotation(), ..SpawnEffect::at("krust_spew", spew.translation()) });
                        }
                    }
                    if a.finished() {
                        k.shots = k.shots.saturating_sub(1);
                        if k.shots > 0 && seen && in_cone {
                            k.fired = false;
                            play(&mut a, def, FIRING, false);
                        } else {
                            k.state = AGGRESSIVE;
                            k.volley = rand_in(def.between_volleys);
                            play(&mut a, def, AGGRESSIVE, false);
                        }
                    }
                }
                _ => {}
            }
        }
    }
    // the open shells' bubbling stops once none is open
    if any_open {
        *looping = true;
    } else if *looping {
        *looping = false;
        stop.write(StopEvent(event_id(LOOP_SOUND)));
    }
}

/// Corvo inside a krust: it holds open (`RiverKrustAnimState_Possessed`) and [Attack] spits
/// where he looks.
#[allow(clippy::too_many_arguments)]
fn possessed_krust(
    mut commands: Commands,
    mouse: Res<ButtonInput<MouseButton>>,
    paused: Res<crate::hud::Paused>,
    (level, settings): (Option<Res<LevelInfo>>, Res<crate::settings::Settings>),
    rapier: ReadRapierContext,
    player: Query<Entity, With<Player>>,
    cam: Query<&GlobalTransform, With<PlayerCamera>>,
    mut krusts: Query<(&mut Krust, &mut Animator), With<crate::possession::Possessed>>,
    joints: Query<&GlobalTransform>,
    mut fx: MessageWriter<SpawnEffect>,
) {
    if paused.0 { return; }
    let Some(level) = level else { return };
    for (mut k, mut a) in &mut krusts {
        let Some(def) = level.scene.krusts.get(k.index as usize) else { continue };
        if k.dead() {
            continue;
        }
        // Corvo's host must not retain the world's stopped/slowed clock.
        a.time_scale = 1.0;
        a.frozen = false;
        if k.state == FIRING {
            let t = a.current().map(|c| c.t).unwrap_or(0.0);
            if !k.fired && t >= FIRE_AT {
                k.fired = true;
                if let (Some(launch), Some(at)) = (world(&joints, k.launch), k.order.take()) {
                    fire(&mut commands, def, settings.difficulty, launch.translation(), at, Vec3::ZERO, k.collider);
                    if std::env::var("DH_KRUST_LOG").is_ok() {
                        info!("possessed krust {} spits at {at:.2}", def.name);
                    }
                    if let Some(spew) = world(&joints, k.spew) {
                        fx.write(SpawnEffect { rot: spew.rotation(), ..SpawnEffect::at("krust_spew", spew.translation()) });
                    }
                }
            }
            if a.finished() {
                k.state = POSSESSED;
                play(&mut a, def, POSSESSED, false);
            }
            continue;
        }
        if k.state != POSSESSED {
            k.state = POSSESSED;
            k.mode = Mode::Aggressive;
            play(&mut a, def, POSSESSED, false);
        }
        if mouse.just_pressed(MouseButton::Left) {
            // at what Corvo aims at
            let Ok(c) = cam.single() else { continue };
            let (eye, dir) = (c.translation(), c.forward().as_vec3());
            let mut filter = QueryFilter::default().groups(CollisionGroups::new(Group::ALL, GROUP_WORLD | GROUP_PROP | crate::level::GROUP_NPC)).exclude_collider(k.collider);
            if let Ok(pe) = player.single() {
                filter = filter.exclude_collider(pe);
            }
            let reach = def.aggressive[1].max(10.0);
            let d = rapier.single().ok().and_then(|ctx| ctx.cast_ray(eye + dir * 0.5, dir, reach, true, filter)).map(|(_, t)| t + 0.5).unwrap_or(reach);
            k.order = Some(eye + dir * d);
            k.state = FIRING;
            k.fired = false;
            play(&mut a, def, FIRING, false);
        }
    }
}

/// Spit: led at a moving target, lifted against its slight gravity; now and then off the mark
/// (`m_fChanceForAccurateProjectileAim`). Its speed grows with the distance.
fn fire(commands: &mut Commands, def: &KrustDef, difficulty: u8, from: Vec3, at: Vec3, target_vel: Vec3, source: Entity) {
    let d = from.distance(at);
    let [d0, d1] = def.speed_distance;
    let k = if d1 > d0 { ((d - d0) / (d1 - d0)).clamp(0.0, 1.0) } else { 0.5 };
    let speed = (def.speed[0] + (def.speed[1] - def.speed[0]) * k).max(1.0);
    let mut aim = at;
    let tt = d / speed;
    if def.lead {
        aim += target_vel.with_y(0.0) * tt;
    }
    if rand::random::<f32>() > def.accurate {
        let a = rand::random::<f32>() * std::f32::consts::TAU;
        aim += Vec3::new(a.cos(), rand::random::<f32>() - 0.3, a.sin()) * (0.8 + d * 0.06);
    }
    let g = GRAVITY * def.gravity;
    aim.y += 0.5 * g * tt * tt;
    let vel = (aim - from).normalize_or(Vec3::NEG_Z) * speed;
    let damage = def.damage[difficulty.min(3) as usize];
    let e = commands
        .spawn((Spit { source, vel, gravity: g, damage, life: 6.0 }, Transform::from_translation(from), Visibility::default(), DespawnOnExit(GameState::InGame)))
        .id();
    if let Some(trail) = def.trail {
        commands.write_message(SpawnEffect { follow: Some(e), secs: 6.0, system: Some(trail), ..SpawnEffect::at("", Vec3::ZERO) });
    }
}

#[allow(clippy::too_many_arguments)]
fn fly_spit(
    mut commands: Commands,
    time: Res<Time>,
    tc: Res<TimeControl>,
    rapier: ReadRapierContext,
    mut spits: Query<(Entity, &mut Spit, &mut Transform)>,
    player: Query<&Transform, (With<Player>, Without<Spit>)>,
    (npcs, mut hits, possession): (Query<(Entity, &crate::npc::Npc, &Transform), Without<Spit>>, MessageWriter<crate::gameplay::NpcHit>, Res<crate::possession::Possession>),
    cam: Query<Entity, With<PlayerCamera>>,
    mut stats: ResMut<PlayerStats>,
    mut msgs: ResMut<crate::gameplay::HudMessages>,
    mut sfx: MessageWriter<PostEvent>,
    mut fx: MessageWriter<SpawnEffect>,
) {
    let dt = time.delta_secs() * tc.world_scale();
    let Ok(ctx) = rapier.single() else { return };
    let walls = QueryFilter::default().exclude_sensors().groups(CollisionGroups::new(Group::ALL, GROUP_WORLD | GROUP_PROP));
    let pp = player.single().ok().map(|t| t.translation);
    for (e, mut s, mut t) in &mut spits {
        s.life -= dt;
        if s.life <= 0.0 {
            commands.entity(e).despawn();
            continue;
        }
        s.vel.y -= s.gravity * dt;
        let step = s.vel * dt;
        let len = step.length();
        if len <= 0.0 {
            continue;
        }
        let a = t.translation;
        let direction = step / len;
        let wall = ctx.cast_ray_and_get_normal(a, direction, len, true, walls.exclude_collider(s.source));
        let limit = wall.as_ref().map(|(_, hit)| hit.time_of_impact).unwrap_or(len);
        // Resolve all candidates along the swept segment, not by entity iteration
        // order. A character behind the first solid impact cannot intercept it.
        let npc = npcs.iter().filter(|(_, n, _)| !n.is_down()).filter_map(|(ne, _, nt)| {
            spit_capsule_hit(a, direction, limit, nt.translation, crate::npc::NPC_HALF, crate::npc::NPC_RADIUS + 0.1)
                .filter(|hit| wall.is_none() || *hit < limit).map(|hit| (ne, nt.translation, hit))
        }).min_by(|a, b| a.2.total_cmp(&b.2));
        // Corvo (not while he's in a creature)
        if let Some(p) = pp.filter(|_| !stats.dead && possession.body.is_none()) {
            let player_hit = spit_capsule_hit(a, direction, limit, p + Vec3::Y * 0.05,
                crate::player::STAND_HALF + 0.15, crate::player::RADIUS + 0.12)
                .filter(|hit| (wall.is_none() || *hit < limit) && npc.is_none_or(|n| *hit < n.2));
            if let Some(hit) = player_hit {
                stats.hit_from = Some(a);
                crate::gameplay::hurt_player(&mut stats, &mut msgs, &mut sfx, s.damage);
                if std::env::var("DH_KRUST_LOG").is_ok() {
                    info!("krust spit hits Corvo: -{} ({:.0} left)", s.damage, stats.health);
                }
                if let Ok(c) = cam.single() {
                    fx.write(SpawnEffect { follow: Some(c), secs: 2.0, ..SpawnEffect::at("krust_lens", Vec3::NEG_Z * 0.3) });
                }
                fx.write(SpawnEffect::at("krust_impact", a + direction * hit));
                commands.entity(e).despawn();
                continue;
            }
        }
        if let Some((ne, at, hit)) = npc {
            hits.write(crate::gameplay::NpcHit { npc: ne, damage: s.damage, kind: crate::gameplay::HitKind::Bullet, from: a });
            fx.write(SpawnEffect::at("krust_impact", a + direction * hit));
            sfx.write(PostEvent::named("Imp_Bullet_on_Body", Some(at)));
            commands.entity(e).despawn();
            continue;
        }
        if let Some((_, hit)) = wall {
            let at = a + step / len * hit.time_of_impact;
            if std::env::var("DH_KRUST_LOG").is_ok() {
                info!("krust spit splashes at {at:.2}");
            }
            fx.write(SpawnEffect { rot: Quat::from_rotation_arc(Vec3::Y, hit.normal.normalize_or(Vec3::Y)), ..SpawnEffect::at("krust_impact", at) });
            commands.entity(e).despawn();
            continue;
        }
        t.translation = a + step;
    }
}

fn spit_capsule_hit(from: Vec3, direction: Vec3, distance: f32, center: Vec3, half: f32, radius: f32) -> Option<f32> {
    Collider::capsule_y(half, radius).cast_ray(center, Quat::IDENTITY, from, direction, distance, true)
}

/// Shortest distance between segments `a0..a1` and `b0..b1`.
pub fn segment_distance(a0: Vec3, a1: Vec3, b0: Vec3, b1: Vec3) -> f32 {
    let d1 = a1 - a0;
    let d2 = b1 - b0;
    let r = a0 - b0;
    let a = d1.dot(d1);
    let e = d2.dot(d2);
    let f = d2.dot(r);
    let (s, t) = if a <= 1e-8 && e <= 1e-8 {
        (0.0, 0.0)
    } else if a <= 1e-8 {
        (0.0, (f / e).clamp(0.0, 1.0))
    } else {
        let c = d1.dot(r);
        if e <= 1e-8 {
            ((-c / a).clamp(0.0, 1.0), 0.0)
        } else {
            let b = d1.dot(d2);
            let denom = a * e - b * b;
            let mut s = if denom > 1e-8 { ((b * f - c * e) / denom).clamp(0.0, 1.0) } else { 0.0 };
            let mut t = (b * s + f) / e;
            if t < 0.0 {
                t = 0.0;
                s = (-c / a).clamp(0.0, 1.0);
            } else if t > 1.0 {
                t = 1.0;
                s = ((b - c) / a).clamp(0.0, 1.0);
            }
            (s, t)
        }
    };
    (a0 + d1 * s).distance(b0 + d2 * t)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fast_spit_hits_cover_before_player_and_ignores_its_source() {
        for group in [GROUP_WORLD, GROUP_PROP] {
            let mut app = App::new();
            app.add_plugins((MinimalPlugins, AssetPlugin::default(), bevy::scene::ScenePlugin, TransformPlugin, RapierPhysicsPlugin::<NoUserData>::default()))
                .init_resource::<Assets<Mesh>>().init_resource::<TimeControl>().init_resource::<PlayerStats>()
                .init_resource::<crate::gameplay::HudMessages>().init_resource::<crate::possession::Possession>()
                .add_message::<crate::gameplay::NpcHit>().add_message::<PostEvent>().add_message::<SpawnEffect>()
                .insert_resource(bevy::time::TimeUpdateStrategy::ManualDuration(std::time::Duration::from_millis(100)))
                .add_systems(Last, fly_spit);
            app.world_mut().spawn((Transform::from_xyz(3.0, 0.0, 0.0), Player {
                velocity: Vec3::ZERO, yaw: 0.0, pitch: 0.0, crouched: false, sprinting: false,
                grounded: true, lean: 0.0, noclip: false, eye_height: 1.0, locked: false,
                air_time: 0.0, spawn: Vec3::ZERO, mantle: None, step_timer: 0.0,
                fall_speed: 0.0, power_jump: 0.0, pull: Vec3::ZERO,
            }));
            let source = app.world_mut().spawn((Collider::ball(0.4), Transform::default(), CollisionGroups::new(GROUP_PROP, Group::ALL))).id();
            let cover = app.world_mut().spawn((Collider::cuboid(0.01, 1.0, 1.0), Transform::from_xyz(1.0, 0.0, 0.0), CollisionGroups::new(group, Group::ALL))).id();
            app.update();
            let before = app.world().resource::<PlayerStats>().health;
            let shot = |app: &mut App| {
                app.world_mut().spawn((Spit { source, vel: Vec3::X * 50.0, gravity: 0.0, damage: 7.0, life: 6.0 }, Transform::default())).id()
            };
            let blocked = shot(&mut app);
            app.update();
            assert!(app.world().get::<Spit>(blocked).is_none());
            assert_eq!(app.world().resource::<PlayerStats>().health, before, "cover must win before the farther capsule");
            app.world_mut().entity_mut(cover).insert(Sensor);
            let exposed = shot(&mut app);
            app.update();
            assert!(app.world().get::<Spit>(exposed).is_none());
            assert_eq!(app.world().resource::<PlayerStats>().health, before - 7.0, "source shell and triggers must not intercept the shot");
        }
    }

    #[test]
    fn spit_capsule_query_reports_entry_distance_and_respects_cover_limit() {
        let near = spit_capsule_hit(Vec3::ZERO, Vec3::X, 10.0, Vec3::X * 2.0, 0.6, 0.4).unwrap();
        let far = spit_capsule_hit(Vec3::ZERO, Vec3::X, 10.0, Vec3::X * 4.0, 0.6, 0.4).unwrap();
        // Convex ray casts use an iterative solver: require millimetre accuracy.
        assert!((near - 1.6).abs() < 0.001, "entry {near}");
        assert!((far - 3.6).abs() < 0.001, "entry {far}");
        assert!(near < far);
        assert!(spit_capsule_hit(Vec3::ZERO, Vec3::X, 1.0, Vec3::X * 2.0, 0.6, 0.4).is_none());
    }

    #[test]
    fn segments() {
        let d = segment_distance(Vec3::new(-1.0, 0.0, 1.0), Vec3::new(1.0, 0.0, 1.0), Vec3::new(0.0, -1.0, 0.0), Vec3::new(0.0, 1.0, 0.0));
        assert!((d - 1.0).abs() < 1e-5);
    }
}
