//! Cascade particle systems (`scene.particle_systems`) placed in the level
//! (`scene.particles`), simulated on the CPU near the player.
//!
//! Module distributions are the cooked UE3 lookup tables, evaluated like the original
//! (`Dist`): spawn rate and bursts, lifetime, start size / colour / velocity / location /
//! rotation, acceleration, and the over-life curves (colour, alpha, size, velocity). Sprites
//! are camera-facing (or velocity-aligned) quads with flipbook sub-images, written into one
//! dynamic mesh per emitter; mesh emitters draw pooled mesh instances.

use crate::level::{GameAssets, LevelInfo, LevelSpawnSet};
use crate::player::PlayerCamera;
use crate::GameState;
use bevy::asset::RenderAssetUsages;
use bevy::camera::visibility::NoFrustumCulling;
use bevy::mesh::{Indices, PrimitiveTopology};
use bevy::prelude::*;
use dhcook::format::{Dist, EmitterDef, ModuleDef, Scene};
use std::sync::Arc;

pub struct ParticlePlugin;

impl Plugin for ParticlePlugin {
    fn build(&self, app: &mut App) {
        app.add_message::<ParticleEventFired>().add_message::<SpawnEffect>()
            .init_resource::<LensDone>()
            .init_resource::<LensFov>()
            .add_systems(OnEnter(GameState::InGame), spawn_emitters.after(LevelSpawnSet))
            .add_systems(Update, (lens_fov, lens_effects, spawn_effects, simulate).chain().after(crate::kismet::apply_effects).run_if(in_state(GameState::InGame)));
    }
}

/// Emitters farther than this (or their own draw distance) are not simulated.
const SIM_RANGE: f32 = 45.0;
/// An axis lock: the sprite's plane (up and right), or the axis it turns about.
#[derive(Clone, Copy)]
enum Lock {
    Plane(Vec3, Vec3),
    Turn(Vec3),
}

/// Particles per emitter at most.
const MAX_PARTICLES: usize = 400;
/// Seconds simulated when an emitter comes into range, so it starts in its steady state.
const WARMUP: f32 = 2.0;

/// A placed particle system.
#[derive(Component)]
pub struct ParticleEmitter {
    /// index into `scene.particles`
    pub index: u32,
    pub active: bool,
    system: u32,
    xf: Mat4,
    scale: f32,
    range: f32,
    awake: bool,
    emitters: Vec<EmitterState>,
    /// a gameplay effect: seconds left (it starts fresh, not in a steady state)
    life: Option<f32>,
    /// carried by an entity (follows it)
    follow: Option<(Entity, Vec3)>,
    /// turning with what carries it: its own rotation relative to the carrier's
    turn: Option<Quat>,
    /// a camera lens effect (`DisSeqAct_SpawnCameraLensEffect`): held before the eye
    lens: Option<Lens>,
    /// drawn with Corvo's hands (the view model's layer)
    view: bool,
    /// where it was last frame (its velocity)
    last_pos: Vec3,
}

struct Lens {
    /// the script op told when it is over
    op: u32,
    /// loops until stopped (else ends with its emitters)
    looping: bool,
    /// no more particles: the last ones run out
    stopping: bool,
    age: f32,
}

/// Lens effects that ended (their ops' "Finished").
#[derive(Resource, Default)]
struct LensDone(Vec<u32>);

/// UE3 holds a camera lens effect 90 units before the eye; drawn at a third of that (a third
/// the size, the same on screen) so walls close by don't cut it.
const LENS_SCALE: f32 = 1.0 / 3.0;

/// `EmitterCameraLensEffectBase`: effects are made for an 80 degree (horizontal) view and held
/// nearer or further for the camera's so they cover the screen alike
/// (`DistFromCamera · tan(BaseFOV/2) / tan(FOV/2)`): that factor for the view.
#[derive(Resource)]
pub struct LensFov(pub f32);

impl Default for LensFov {
    fn default() -> Self {
        LensFov(1.0)
    }
}

fn lens_fov(cam: Query<&Projection, With<PlayerCamera>>, mut k: ResMut<LensFov>) {
    if let Ok(Projection::Perspective(p)) = cam.single() {
        let half = (p.fov * 0.5).tan() * p.aspect_ratio.max(0.1);
        k.0 = 40f32.to_radians().tan() / half.max(0.01);
    }
}

fn lens_xf(cam: &GlobalTransform, k: f32) -> Mat4 {
    let (_, rot, pos) = cam.to_scale_rotation_translation();
    // the effect's X axis (UE forward) along the view, Y to the right
    Mat4::from_scale_rotation_translation(Vec3::splat(LENS_SCALE), rot * Quat::from_rotation_y(std::f32::consts::FRAC_PI_2), pos + rot * Vec3::NEG_Z * 0.9 * k * LENS_SCALE)
}

/// The scripts' camera lens effects (rain on the lens, the Hound Pits sickness, water after
/// a dive): started, told to stop looping, and their ops told when they are over.
fn lens_effects(
    mut commands: Commands,
    vm: Option<ResMut<crate::kismet::Vm>>,
    level: Option<Res<LevelInfo>>,
    assets: Option<Res<GameAssets>>,
    mut meshes: ResMut<Assets<Mesh>>,
    cam: Query<(Entity, &GlobalTransform), With<PlayerCamera>>,
    mut emitters: Query<&mut ParticleEmitter>,
    mut done: ResMut<LensDone>,
    fov: Res<LensFov>,
) {
    let Some(mut vm) = vm else { return };
    // (gameplay's lens effects have no op to tell)
    for op in done.0.drain(..) {
        if op < crate::hudfx::GAMEPLAY_LENS {
            vm.signal(op, 0);
        }
    }
    if vm.lens.is_empty() {
        return;
    }
    let (Some(level), Some(assets), Ok((ce, cg))) = (level, assets, cam.single()) else { return };
    for (op, start) in std::mem::take(&mut vm.lens) {
        // a new one replaces the op's running one; "Stop Looping" lets it run out
        for mut em in &mut emitters {
            if let Some(l) = em.lens.as_mut().filter(|l| l.op == op) {
                l.stopping = true;
            }
        }
        let Some((system, looping, life)) = start else { continue };
        let Some(sys) = level.scene.particle_systems.get(system as usize) else {
            if op < crate::hudfx::GAMEPLAY_LENS {
                vm.signal(op, 1);
            }
            continue;
        };
        let xf = lens_xf(cg, fov.0);
        if std::env::var("DH_PFX_LOG").is_ok() {
            let mats: Vec<String> = sys.emitters.iter().map(|e| format!("{}:{}{}", e.name, e.material, if assets.particle_mats.contains_key(&e.material) { "" } else { "(no mat)" })).collect();
            info!("lens {op:#x} -> {} {:?}", sys.name, mats);
        }
        let emitters = emitter_states(&mut commands, sys, &assets, &mut meshes, xf.w_axis.truncate(), false);
        // (the effect actor's LifeSpan ends even a looping one)
        let life = if life > 0.0 { Some(life) } else if looping { None } else { Some(20.0) };
        commands.spawn((
            ParticleEmitter {
                index: u32::MAX,
                active: true,
                system,
                xf,
                scale: LENS_SCALE,
                range: SIM_RANGE,
                awake: true,
                emitters,
                life,
                follow: Some((ce, Vec3::ZERO)),
                turn: None,
                view: false,
                lens: Some(Lens { op, looping, stopping: false, age: 0.0 }),
                last_pos: xf.w_axis.truncate(),
            },
            DespawnOnExit(GameState::InGame),
        ));
    }
}

impl ParticleEmitter {
    /// End a gameplay effect now.
    pub fn stop(&mut self) {
        self.life = Some(0.0);
    }

    /// Where the emitter is (Bevy space).
    pub fn xf(&self) -> Mat4 {
        self.xf
    }

    /// Move it (a matinee carries it).
    pub fn set_xf(&mut self, xf: Mat4) {
        self.xf = xf;
    }

    /// Carried by an entity from now on, at this offset from it (`SeqAct_AttachToActor`), or
    /// let go where it is.
    pub fn attach(&mut self, to: Option<(Entity, Vec3)>) {
        self.follow = to;
    }
}

/// Play one of the original gameplay effects (`scene.effects`, e.g. "grenade") once.
#[derive(Message, Clone)]
pub struct SpawnEffect {
    pub name: &'static str,
    pub at: Vec3,
    pub rot: Quat,
    /// seconds it lasts (looping effects such as fire stop then)
    pub secs: f32,
    /// follow this entity (offset `at` from it)
    pub follow: Option<Entity>,
    /// a particle system of the level by index (instead of the name)
    pub system: Option<u32>,
    /// turn with the followed entity (`rot` relative to it)
    pub turn: bool,
    /// put `EffectTag` on the effect (to find it again: Dark Vision's cones)
    pub tag: Option<Entity>,
    /// drawn with Corvo's hands (`m_bMatchAttachmentDPG`: the view model's layer)
    pub view: bool,
}

impl SpawnEffect {
    pub fn at(name: &'static str, at: Vec3) -> Self {
        SpawnEffect { name, at, rot: Quat::IDENTITY, secs: 3.0, follow: None, system: None, turn: false, tag: None, view: false }
    }
}

/// A gameplay effect spawned for something (`SpawnEffect::tag`).
#[derive(Component)]
pub struct EffectTag(pub Entity);

/// Emitter states for a system (sprite emitters get a dynamic mesh with the material).
fn emitter_states(commands: &mut Commands, sys: &dhcook::format::ParticleSystemDef, assets: &GameAssets, meshes: &mut Assets<Mesh>, origin: Vec3, view: bool) -> Vec<EmitterState> {
    let mut states = Vec::new();
    for e in &sys.emitters {
        let mat = assets.particle_mats.get(&e.material).cloned();
        let (entity, mesh) = match (mat, e.mesh) {
            (Some(mat), None) => {
                let mesh = meshes.add(placeholder_mesh());
                // placed at the emitter: materials read the object position (distance fades)
                let mut ec = commands.spawn((Mesh3d(mesh.clone()), Transform::from_translation(origin), Visibility::Hidden, NoFrustumCulling, bevy::light::NotShadowCaster, DespawnOnExit(GameState::InGame)));
                mat.apply(&mut ec);
                if view {
                    ec.insert(bevy::camera::visibility::RenderLayers::layer(crate::combat::VIEW_LAYER));
                }
                (ec.id(), Some(mesh))
            }
            _ => (commands.spawn((Transform::IDENTITY, Visibility::Hidden, DespawnOnExit(GameState::InGame))).id(), None),
        };
        states.push(EmitterState { entity, mesh, pool: Vec::new(), particles: Vec::new(), accum: 0.0, time: 0.0, loops: 0, bursts: vec![false; e.bursts.len()] });
    }
    states
}

fn spawn_effects(
    mut commands: Commands,
    mut msgs: MessageReader<SpawnEffect>,
    level: Option<Res<LevelInfo>>,
    assets: Option<Res<GameAssets>>,
    mut meshes: ResMut<Assets<Mesh>>,
    followed: Query<&GlobalTransform>,
) {
    let (Some(level), Some(assets)) = (level, assets) else {
        msgs.clear();
        return;
    };
    for m in msgs.read() {
        // (no name, no system: nothing)
        if m.name.is_empty() && m.system.is_none() {
            continue;
        }
        // a named gameplay effect, else any of the level's systems by name (testing)
        let found = m.system.or_else(|| level.scene.effects.iter().find(|(n, _)| n == m.name).map(|e| e.1)).or_else(|| {
            let want = m.name.to_ascii_lowercase();
            level.scene.particle_systems.iter().position(|s| s.name.to_ascii_lowercase().ends_with(&want)).map(|i| i as u32)
        });
        let Some(system) = found else { continue };
        let Some(sys) = level.scene.particle_systems.get(system as usize) else { continue };
        let carrier = m.follow.and_then(|e| followed.get(e).ok());
        let base = carrier.map(|g| g.translation()).unwrap_or(Vec3::ZERO);
        let rot = match (m.turn, carrier) {
            (true, Some(g)) => g.rotation() * m.rot,
            _ => m.rot,
        };
        let xf = Mat4::from_rotation_translation(rot, base + m.at);
        let emitters = emitter_states(&mut commands, sys, &assets, &mut meshes, base + m.at, m.view);
        if std::env::var("DH_PFX_LOG").is_ok() {
            let mats: Vec<String> = sys.emitters.iter().map(|e| format!("{}:{}{}", e.name, e.material, if assets.particle_mats.contains_key(&e.material) { "" } else { "(no mat)" })).collect();
            info!("effect {} -> {} {:?}", m.name, sys.name, mats);
        }
        let mut ec = commands.spawn((
            ParticleEmitter {
                index: u32::MAX,
                active: true,
                system,
                xf,
                scale: 1.0,
                range: SIM_RANGE,
                awake: true,
                emitters,
                life: Some(m.secs),
                follow: m.follow.map(|e| (e, m.at)),
                turn: m.turn.then_some(m.rot),
                lens: None,
                view: m.view,
                last_pos: Vec3::ZERO,
            },
            DespawnOnExit(GameState::InGame),
        ));
        if let Some(t) = m.tag {
            ec.insert(EffectTag(t));
        }
    }
}

struct EmitterState {
    entity: Entity,
    mesh: Option<Handle<Mesh>>,
    /// pooled mesh-particle entities
    pool: Vec<Entity>,
    particles: Vec<Particle>,
    accum: f32,
    time: f32,
    loops: u32,
    bursts: Vec<bool>,
}

#[derive(Clone, Default)]
struct Particle {
    pos: Vec3,
    vel: Vec3,
    acc: Vec3,
    age: f32,
    life: f32,
    size: Vec3,
    rot: f32,
    rot_rate: f32,
    color: Vec3,
    alpha: f32,
    /// per-particle random numbers for the curves
    r: [f32; 3],
    image: f32,
    /// a beam's far end (`ParticleModuleBeamTarget`; the near one is `pos`)
    target: Option<Vec3>,
    /// the velocity the over-life scaling scales (`ParticleModuleVelocityOverLifetime`)
    vel_base: Vec3,
    rot_rate_base: f32,
    /// a mesh particle's turn about UE's X, Y and Z (radians) and its rate
    /// (`ParticleModuleMeshRotation`, `..RotationRate`, `..RotationRateOverLife`)
    rot3: Vec3,
    rot3_rate: Vec3,
    rot3_rate_base: Vec3,
    /// an orbit about it (`ParticleModuleOrbit`): offset (UE units), turn and rate (turns)
    orbit: Option<(Vec3, Vec3, Vec3)>,
    /// collisions left before it ends (`ParticleModuleCollision`), and its damping
    collisions: i32,
    damping: Vec3,
    /// stopped where it struck (`EPCC_Freeze`)
    frozen: bool,
    /// lying on a surface (a decal): its normal
    normal: Option<Vec3>,
}

/// Where a particle is to appear (rain splashes, an event received, another emitter's
/// particle): the place, the surface's normal there, a velocity to take.
#[derive(Clone, Copy)]
struct Spot {
    pos: Vec3,
    normal: Option<Vec3>,
    vel: Option<Vec3>,
}

/// An emitter's event this frame (`ParticleModuleEventGenerator`): its kind (0 spawn, 1 death,
/// 2 collision), name, where, the particle's velocity and the surface's normal.
struct PfxEvent {
    kind: u8,
    name: String,
    pos: Vec3,
    vel: Vec3,
    normal: Option<Vec3>,
}

/// What `step` reaches beyond the emitter: the world to collide with (from, direction, length ->
/// point, normal), the emitter's own velocity.
struct StepCtx<'a> {
    ray: Option<&'a dyn Fn(Vec3, Vec3, f32) -> Option<(Vec3, Vec3)>>,
    emitter_vel: Vec3,
}

/// A beam emitter's noise (`ParticleModuleBeamNoise`): points along it, their reach (UE
/// units) and how long a pattern holds.
struct BeamNoise {
    points: u32,
    range: Vec3,
    lock: f32,
}

fn beam_noise(e: &EmitterDef) -> Option<BeamNoise> {
    let m = e.modules.iter().find(|m| m.class == "ParticleModuleBeamNoise")?;
    let range = f3(m, "NoiseRange", 0.0, [1.0, 1.0, 1.0], Vec3::ZERO).abs() * f1(m, "NoiseRangeScale", 0.0, [0.5; 3], 1.0);
    Some(BeamNoise { points: value(m, "Frequency", 0.0).clamp(0.0, 32.0) as u32, range, lock: value(m, "NoiseLockTime", 0.0).max(0.02) })
}

/// A repeatable random number in -1..1.
fn hash(a: f32, b: u32, c: u32) -> f32 {
    let mut h = (a.to_bits() ^ b.wrapping_mul(0x9e37_79b9) ^ c.wrapping_mul(0x85eb_ca6b)).wrapping_mul(0x27d4_eb2d);
    h ^= h >> 15;
    h = h.wrapping_mul(0x2c1b_3c6d);
    h ^= h >> 12;
    (h & 0xffff) as f32 / 32767.5 - 1.0
}

#[derive(Resource)]
struct ParticleData {
    scene: Arc<Scene>,
}

/// UE3 vector (centimetres, Z up) to Bevy (metres, Y up).
fn ue(v: Vec3) -> Vec3 {
    Vec3::new(v.x, v.z, v.y) * 0.01
}

/// Evaluate a cooked distribution at `t` with random numbers `r` (one per component).
pub(crate) fn sample(d: &Dist, t: f32, r: [f32; 3], out: &mut [f32; 3]) -> usize {
    let chunk = d.chunk.max(1) as usize;
    let n = d.table.len() / chunk;
    if n == 0 {
        return 0;
    }
    let x = ((t - d.start) * d.scale).max(0.0);
    let i0 = (x as usize).min(n - 1);
    let i1 = (i0 + 1).min(n - 1);
    let a = if i1 == i0 { 0.0 } else { (x - i0 as f32).clamp(0.0, 1.0) };
    let e0 = &d.table[i0 * chunk..i0 * chunk + chunk];
    let e1 = &d.table[i1 * chunk..i1 * chunk + chunk];
    let v = |k: usize| e0[k] + (e1[k] - e0[k]) * a;
    match d.op {
        2 | 3 if chunk >= 2 => {
            let half = chunk / 2;
            for k in 0..half.min(3) {
                let (lo, hi) = (v(k), v(half + k));
                out[k] = if d.op == 3 { if r[k] < 0.5 { lo } else { hi } } else { lo + (hi - lo) * r[k] };
            }
            half.min(3)
        }
        _ => {
            for k in 0..chunk.min(3) {
                out[k] = v(k);
            }
            chunk.min(3)
        }
    }
}

fn f1(m: &ModuleDef, name: &str, t: f32, r: [f32; 3], default: f32) -> f32 {
    let mut o = [default; 3];
    match m.dists.get(name) {
        Some(d) if sample(d, t, r, &mut o) > 0 => o[0],
        _ => default,
    }
}

fn f3(m: &ModuleDef, name: &str, t: f32, r: [f32; 3], default: Vec3) -> Vec3 {
    let mut o = default.to_array();
    match m.dists.get(name) {
        Some(d) => match sample(d, t, r, &mut o) {
            0 => default,
            1 => Vec3::splat(o[0]),
            _ => Vec3::from(o),
        },
        None => default,
    }
}

fn flag(m: &ModuleDef, name: &str, default: bool) -> bool {
    m.values.get(name).map(|v| *v != 0.0).unwrap_or(default)
}

fn value(m: &ModuleDef, name: &str, default: f32) -> f32 {
    m.values.get(name).copied().unwrap_or(default)
}

fn rand3() -> [f32; 3] {
    [rand::random(), rand::random(), rand::random()]
}

fn spawn_emitters(mut commands: Commands, level: Option<Res<LevelInfo>>, assets: Option<Res<GameAssets>>, mut meshes: ResMut<Assets<Mesh>>) {
    let (Some(level), Some(assets)) = (level, assets) else { return };
    if std::env::var("DH_NO_PARTICLES").is_ok() {
        return;
    }
    let scene = level.scene.clone();
    let mut n = 0;
    for (i, inst) in scene.particles.iter().enumerate() {
        let Some(sys) = scene.particle_systems.get(inst.system as usize) else { continue };
        let xf = Mat4::from_cols_array(&inst.transform);
        if !xf.is_finite() {
            continue;
        }
        let states = emitter_states(&mut commands, sys, &assets, &mut meshes, xf.transform_point3(Vec3::ZERO), false);
        let (scale, _, _) = xf.to_scale_rotation_translation();
        commands.spawn((
            ParticleEmitter {
                index: i as u32,
                active: inst.active,
                system: inst.system,
                xf,
                scale: scale.x.abs().max(1e-3),
                range: if inst.max_draw > 0.0 { inst.max_draw.min(SIM_RANGE) } else { SIM_RANGE },
                awake: false,
                emitters: states,
                life: None,
                follow: None,
                turn: None,
                view: false,
                lens: None,
                last_pos: xf.transform_point3(Vec3::ZERO),
            },
            DespawnOnExit(GameState::InGame),
        ));
        n += 1;
    }
    commands.insert_resource(ParticleData { scene: Arc::new(scene) });
    info!("{n} particle emitters");
}

/// A degenerate, invisible quad: particle meshes are never empty (the GPU allocator
/// can't hold zero-sized meshes).
fn placeholder_mesh() -> Mesh {
    Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::default())
        .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, vec![[0.0f32; 3]; 4])
        .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, vec![[0.0f32, 1.0, 0.0]; 4])
        .with_inserted_attribute(Mesh::ATTRIBUTE_UV_0, vec![[0.0f32; 2]; 4])
        .with_inserted_attribute(Mesh::ATTRIBUTE_UV_1, vec![[0.0f32; 2]; 4])
        .with_inserted_attribute(Mesh::ATTRIBUTE_TANGENT, vec![[1.0f32, 0.0, 0.0, 1.0]; 4])
        .with_inserted_attribute(Mesh::ATTRIBUTE_COLOR, vec![[0.0f32; 4]; 4])
        .with_inserted_indices(Indices::U32(vec![0, 1, 2, 0, 2, 3]))
}

/// Spawn and age one emitter's particles over `dt`.
/// The rain box about the eye (`DisParticleModuleRainDrops`): half its size, and how far ahead
/// of the eye its middle is (metres).
const RAIN_HALF: Vec3 = Vec3::new(2.5, 3.0, 2.5);
const RAIN_AHEAD: f32 = 2.0;

/// Rain drops: the box about the eye kept filled to the drop count at the emitter's rate,
/// each falling until it leaves the box or meets what is above it (a splash there).
fn rain_drops(def: &EmitterDef, st: &mut EmitterState, rain: &crate::rain::Rain, eye: Vec3, fwd: Vec3, dt: f32, impacts: &mut Vec<Vec3>) {
    let Some(m) = def.modules.iter().find(|m| m.class == "DisParticleModuleRainDrops") else { return };
    let center = eye + fwd.with_y(0.0).normalize_or_zero() * RAIN_AHEAD;
    st.particles.retain_mut(|p| {
        p.age += dt;
        p.pos += p.vel * dt;
        match rain.top(p.pos) {
            Some(t) if p.pos.y > t => ((p.pos - center).abs().cmple(RAIN_HALF)).all(),
            Some(t) => {
                if t.is_finite() && p.pos.y > t - 1.0 {
                    impacts.push(p.pos.with_y(t + 0.03));
                }
                false
            }
            None => false,
        }
    });
    let rate = f1(&def.spawn, "Rate", 0.0, rand3(), 0.0).max(100.0);
    st.accum += rate * dt;
    let mut n = st.accum.floor() as u32;
    st.accum -= n as f32;
    while n > 0 && st.particles.len() < rain.drops as usize {
        n -= 1;
        let r = Vec3::new(rand::random::<f32>(), rand::random::<f32>(), rand::random::<f32>()) * 2.0 - Vec3::ONE;
        let pos = center + r * RAIN_HALF;
        if rain.top(pos).is_none_or(|t| pos.y <= t) {
            continue;
        }
        st.particles.push(Particle {
            pos,
            vel: ue(f3(m, "m_StartVelocity", 0.0, rand3(), Vec3::new(0.0, 0.0, -1000.0))),
            acc: Vec3::ZERO,
            age: 0.0,
            life: f32::INFINITY,
            size: f3(m, "m_StartSize", 0.0, rand3(), Vec3::ONE),
            rot: 0.0,
            rot_rate: 0.0,
            color: f3(m, "m_StartColor", 0.0, rand3(), Vec3::ONE),
            alpha: f1(m, "m_StartAlpha", 0.0, rand3(), 1.0),
            r: rand3(),
            image: 0.0,
            target: None,
            ..Default::default()
        });
    }
}

/// A placed emitter's particle event for the level scripts (`ParticleModuleEventGenerator` ->
/// `SeqEvent_ParticleEvent`): the placed system (`scene.particles`), the event's name.
#[derive(Message, Clone)]
pub struct ParticleEventFired {
    pub index: u32,
    pub name: String,
}

/// One new particle of an emitter (its spawn modules), at its place in the emitter or at `spot`.
fn spawn_one(e: &EmitterDef, xf: &Mat4, scale: f32, t_em: f32, origin: Vec3, spot: Option<&Spot>, cx: &StepCtx) -> Particle {
    let mut p = Particle { life: 1.0, size: Vec3::ONE, color: Vec3::ONE, alpha: 1.0, r: rand3(), ..Default::default() };
    let mut local = Vec3::ZERO;
    let mut local_vel = Vec3::ZERO;
    let mut world_vel = Vec3::ZERO;
    let mut radial = 0.0;
    for m in &e.modules {
        match m.class.as_str() {
            // a zero (or unset) lifetime lives forever
            "ParticleModuleLifetime" => {
                let l = f1(m, "Lifetime", t_em, rand3(), 0.0);
                p.life = if l > 0.0 { l } else { f32::INFINITY };
            }
            "ParticleModuleSize" => p.size = f3(m, "StartSize", t_em, rand3(), Vec3::ONE),
            "ParticleModuleColor" => {
                p.color = f3(m, "StartColor", t_em, rand3(), Vec3::ONE);
                p.alpha = f1(m, "StartAlpha", t_em, rand3(), 1.0);
            }
            "ParticleModuleVelocity" => {
                let v = f3(m, "StartVelocity", t_em, rand3(), Vec3::ZERO);
                if flag(m, "bInWorldSpace", false) {
                    world_vel += v;
                } else {
                    local_vel += v;
                }
                radial += f1(m, "StartVelocityRadial", t_em, rand3(), 0.0);
            }
            // the emitter's own motion (`ParticleModuleVelocityInheritParent`)
            "ParticleModuleVelocityInheritParent" => {
                let k = f3(m, "Scale", t_em, rand3(), Vec3::ONE);
                world_vel += Vec3::new(cx.emitter_vel.x * k.x, cx.emitter_vel.z * k.y, cx.emitter_vel.y * k.z) * 100.0;
            }
            "ParticleModuleLocation" => local += f3(m, "StartLocation", t_em, rand3(), Vec3::ZERO),
            "ParticleModuleLocationPrimitiveSphere" => {
                let radius = f1(m, "StartRadius", t_em, rand3(), 50.0);
                let d = Vec3::new(rand::random::<f32>() - 0.5, rand::random::<f32>() - 0.5, rand::random::<f32>() - 0.5).normalize_or_zero();
                let rr = if flag(m, "SurfaceOnly", false) { 1.0 } else { rand::random::<f32>().cbrt() };
                local += d * radius * rr;
                if flag(m, "Velocity", false) {
                    local_vel += d * radius * f1(m, "VelocityScale", t_em, rand3(), 1.0);
                }
            }
            "ParticleModuleLocationPrimitiveCylinder" => {
                let radius = f1(m, "StartRadius", t_em, rand3(), 50.0);
                let height = f1(m, "StartHeight", t_em, rand3(), 50.0);
                let a = rand::random::<f32>() * std::f32::consts::TAU;
                let rr = radius * rand::random::<f32>().sqrt();
                local += Vec3::new(a.cos() * rr, a.sin() * rr, (rand::random::<f32>() - 0.5) * height);
            }
            "ParticleModuleRotation" => p.rot = f1(m, "StartRotation", t_em, rand3(), 0.0) * std::f32::consts::TAU,
            "ParticleModuleRotationRate" => p.rot_rate = f1(m, "StartRotationRate", t_em, rand3(), 0.0) * std::f32::consts::TAU,
            "ParticleModuleMeshRotation" => p.rot3 = f3(m, "StartRotation", t_em, rand3(), Vec3::ZERO) * std::f32::consts::TAU,
            "ParticleModuleMeshRotationRate" => p.rot3_rate = f3(m, "StartRotationRate", t_em, rand3(), Vec3::ZERO) * std::f32::consts::TAU,
            "ParticleModuleAcceleration" => {
                let a = f3(m, "Acceleration", t_em, rand3(), Vec3::ZERO);
                p.acc += if flag(m, "bAlwaysInWorldSpace", false) { ue(a) } else { xf.transform_vector3(ue(a)) };
            }
            "ParticleModuleSubImageIndex" => p.image = f1(m, "SubImageIndex", 0.0, rand3(), 0.0),
            // a beam: from the particle to its target (emitter space)
            "ParticleModuleBeamTarget" => p.target = Some(xf.transform_point3(ue(f3(m, "Target", t_em, rand3(), Vec3::new(0.0, 0.0, 100.0))))),
            "ParticleModuleBeamSource" => local += f3(m, "Source", t_em, rand3(), Vec3::ZERO),
            "ParticleModuleOrbit" => {
                p.orbit = Some((f3(m, "OffsetAmount", t_em, rand3(), Vec3::ZERO), f3(m, "RotationAmount", t_em, rand3(), Vec3::ZERO), f3(m, "RotationRateAmount", t_em, rand3(), Vec3::ZERO)));
            }
            "ParticleModuleCollision" => {
                p.collisions = f1(m, "MaxCollisions", t_em, rand3(), 1.0).max(1.0) as i32;
                p.damping = f3(m, "DampingFactor", t_em, rand3(), Vec3::ZERO);
            }
            _ => {}
        }
    }
    p.pos = match spot {
        Some(sp) => sp.pos,
        None => xf.transform_point3(ue(local)),
    };
    p.vel = xf.transform_vector3(ue(local_vel)) + ue(world_vel);
    if let Some(v) = spot.and_then(|sp| sp.vel) {
        p.vel += v;
    }
    if radial != 0.0 {
        p.vel += (p.pos - origin).normalize_or_zero() * radial * 0.01;
    }
    p.normal = spot.and_then(|sp| sp.normal);
    p.size *= scale;
    p.vel_base = p.vel;
    p.rot_rate_base = p.rot_rate;
    p.rot3_rate_base = p.rot3_rate;
    p
}

/// Advance an emitter: spawn (`forced`: where the new ones appear, as many as there are at
/// most), move, collide and age its particles; how many it spawned and how many died, and its
/// events for the system's other emitters and the level scripts.
#[allow(clippy::too_many_arguments)]
fn step(e: &EmitterDef, st: &mut EmitterState, xf: &Mat4, scale: f32, dt: f32, halt: bool, mut forced: Option<&mut Vec<Spot>>, cx: &StepCtx, events: &mut Vec<PfxEvent>) -> (u32, u32) {
    let req = &e.required;
    // a zero duration never loops (bursts at time 0 fire once)
    let duration = value(req, "EmitterDuration", 1.0);
    let looping = duration > 0.0;
    let duration = duration.max(0.01);
    let loops = value(req, "EmitterLoops", 0.0) as u32;
    let delay = value(req, "EmitterDelay", 0.0);
    st.time += dt;
    let t_em = if looping { ((st.time - delay).max(0.0) % duration) / duration } else { 0.0 };
    if looping && st.time - delay > duration * (st.loops + 1) as f32 {
        st.loops += 1;
        for b in st.bursts.iter_mut() {
            *b = false;
        }
    }
    let spawning = !halt && st.time >= delay && (loops == 0 || st.loops < loops);
    let mut new = 0u32;
    if spawning {
        let rate = f1(&e.spawn, "Rate", t_em, rand3(), 0.0) * f1(&e.spawn, "RateScale", t_em, rand3(), 1.0);
        let rate = if rate <= 0.0 && e.spawn.dists.is_empty() { f1(req, "SpawnRate", t_em, rand3(), 0.0) } else { rate };
        st.accum += rate.max(0.0) * dt;
        new += st.accum.floor() as u32;
        st.accum -= st.accum.floor();
        for (k, b) in e.bursts.iter().enumerate() {
            if !st.bursts[k] && t_em * duration >= b.0 {
                st.bursts[k] = true;
                new += b.1;
            }
        }
    }
    let origin = xf.transform_point3(Vec3::ZERO);
    let new = match forced.as_ref() {
        Some(f) => new.min(f.len() as u32),
        None => new,
    };
    let ev = |kind: u8| e.events.iter().filter(move |(k, _)| *k == kind).map(|(_, n)| n);
    let mut spawned = 0u32;
    for _ in 0..new.min(64) {
        if st.particles.len() >= MAX_PARTICLES {
            break;
        }
        let spot = forced.as_mut().and_then(|f| f.pop());
        let p = spawn_one(e, xf, scale, t_em, origin, spot.as_ref(), cx);
        for n in ev(0) {
            events.push(PfxEvent { kind: 0, name: n.clone(), pos: p.pos, vel: p.vel, normal: None });
        }
        st.particles.push(p);
        spawned += 1;
    }
    // the update modules
    let find = |c: &str| e.modules.iter().find(|m| m.class == c);
    let (vol, aol, attract, rrml, mrrol, kill, coll) = (
        find("ParticleModuleVelocityOverLifetime"),
        find("ParticleModuleAccelerationOverLifetime"),
        find("ParticleModuleAttractorPoint"),
        find("ParticleModuleRotationRateMultiplyLife"),
        find("ParticleModuleMeshRotationRateOverLife"),
        find("ParticleModuleKillHeight"),
        find("ParticleModuleCollision"),
    );
    let completion = coll.and_then(|m| m.names.get("CollisionCompletionOption")).map(|s| s.as_str()).unwrap_or("EPCC_Kill");
    let mut died = 0u32;
    st.particles.retain_mut(|p| {
        p.age += dt;
        if p.age >= p.life {
            died += 1;
            for n in ev(1) {
                events.push(PfxEvent { kind: 1, name: n.clone(), pos: p.pos, vel: p.vel, normal: None });
            }
            return false;
        }
        if p.frozen {
            return true;
        }
        let t = (p.age / p.life).clamp(0.0, 1.0);
        // accelerations, steady and over its life, and the attractor's pull
        let mut acc = p.acc;
        if let Some(m) = aol {
            let a = f3(m, "AccelOverLife", t, p.r, Vec3::ZERO);
            acc += if flag(m, "bAlwaysInWorldSpace", false) { ue(a) } else { xf.transform_vector3(ue(a)) };
        }
        if let Some(m) = attract {
            let at = xf.transform_point3(ue(f3(m, "Position", t, p.r, Vec3::ZERO)));
            let range = f1(m, "Range", t, p.r, 0.0) * 0.01;
            let to = at - p.pos;
            let d = to.length();
            if range <= 0.0 || d < range {
                let mut k = f1(m, "Strength", t, p.r, 0.0) * 0.01;
                if flag(m, "bStrengthByDistance", true) && range > 0.0 {
                    k *= 1.0 - d / range;
                }
                let pull = to.normalize_or_zero() * k;
                if flag(m, "bOverrideVelocity", false) {
                    p.vel_base = pull;
                } else {
                    p.vel_base += pull * dt;
                }
            }
        }
        p.vel_base += acc * dt;
        p.vel = match vol {
            Some(m) => {
                let v = f3(m, "VelOverLife", t, p.r, Vec3::ONE);
                if flag(m, "Absolute", false) {
                    let w = if flag(m, "bInWorldSpace", false) { ue(v) } else { xf.transform_vector3(ue(v)) };
                    p.vel_base = w;
                    w
                } else {
                    // (UE's axes: X, Y, Z -> x, z, y)
                    p.vel_base * Vec3::new(v.x, v.z, v.y)
                }
            }
            None => p.vel_base,
        };
        if let Some(m) = rrml {
            p.rot_rate = p.rot_rate_base * f1(m, "LifeMultiplier", t, p.r, 1.0);
        }
        if let Some(m) = mrrol {
            let k = f3(m, "RotRate", t, p.r, Vec3::ONE);
            p.rot3_rate = if flag(m, "bScaleRotRate", false) { p.rot3_rate_base * k } else { k * std::f32::consts::TAU };
        }
        let from = p.pos;
        p.pos += p.vel * dt;
        p.rot += p.rot_rate * dt;
        p.rot3 += p.rot3_rate * dt;
        if let Some(o) = p.orbit.as_mut() {
            o.1 += o.2 * dt;
        }
        // what it strikes: it bounces (damped), and after its collisions it ends, stops, or
        // passes on (`CollisionCompletionOption`)
        if let (Some(_), Some(ray), true) = (coll, cx.ray, p.collisions > 0) {
            let d = p.pos - from;
            let len = d.length();
            if len > 1e-4 {
                if let Some((at, n)) = ray(from, d / len, len) {
                    for name in ev(2) {
                        events.push(PfxEvent { kind: 2, name: name.clone(), pos: at, vel: p.vel, normal: Some(n) });
                    }
                    p.collisions -= 1;
                    let damp = Vec3::new(p.damping.x, p.damping.z, p.damping.y);
                    p.vel_base = (p.vel_base - 2.0 * p.vel_base.dot(n) * n) * damp;
                    p.vel = p.vel_base;
                    p.pos = at + n * 0.01;
                    if p.collisions <= 0 {
                        match completion {
                            "EPCC_Freeze" | "EPCC_FreezeTranslation" | "EPCC_FreezeVelocity" => p.frozen = true,
                            "EPCC_HaltCollisions" => p.collisions = 0,
                            _ => {
                                died += 1;
                                return false;
                            }
                        }
                    }
                }
            }
        }
        // under (or over) a height: gone (`ParticleModuleKillHeight`)
        if let Some(m) = kill {
            let mut h = f1(m, "Height", t, p.r, 0.0) * 0.01;
            if !flag(m, "bAbsolute", false) {
                h += origin.y;
            }
            let floor = flag(m, "bFloor", false);
            if (floor && p.pos.y < h) || (!floor && p.pos.y > h) {
                died += 1;
                return false;
            }
        }
        true
    });
    (spawned, died)
}

/// Over-life curves for drawing: (colour, alpha, size, velocity scale).
fn over_life(e: &EmitterDef, p: &Particle) -> (Vec3, f32, Vec3) {
    let t = (p.age / p.life).clamp(0.0, 1.0);
    let mut color = p.color;
    let mut alpha = p.alpha;
    let mut size = p.size;
    for m in &e.modules {
        match m.class.as_str() {
            "ParticleModuleColorOverLife" => {
                if m.dists.contains_key("ColorOverLife") {
                    color = f3(m, "ColorOverLife", t, p.r, color);
                }
                alpha = f1(m, "AlphaOverLife", t, p.r, alpha);
                if flag(m, "bClampAlpha", true) {
                    alpha = alpha.clamp(0.0, 1.0);
                }
            }
            "ParticleModuleColorScaleOverLife" => {
                color *= f3(m, "ColorScaleOverLife", t, p.r, Vec3::ONE);
                alpha *= f1(m, "AlphaScaleOverLife", t, p.r, 1.0);
            }
            "ParticleModuleSizeMultiplyLife" => {
                let k = f3(m, "LifeMultiplier", t, p.r, Vec3::ONE);
                if flag(m, "MultiplyX", true) {
                    size.x *= k.x;
                }
                if flag(m, "MultiplyY", true) {
                    size.y *= k.y;
                }
                if flag(m, "MultiplyZ", true) {
                    size.z *= k.z;
                }
            }
            "ParticleModuleSizeScale" => size = p.size * f3(m, "SizeScale", t, p.r, Vec3::ONE),
            "ParticleModuleSizeScaleByTime" => {
                let k = f3(m, "SizeScaleByTime", p.age, p.r, Vec3::ONE);
                if flag(m, "bEnableX", true) {
                    size.x *= k.x;
                }
                if flag(m, "bEnableY", true) {
                    size.y *= k.y;
                }
                if flag(m, "bEnableZ", true) {
                    size.z *= k.z;
                }
            }
            // stretched by its speed (UE units): rain streaks
            "ParticleModuleSizeMultiplyVelocity" => {
                let k = f3(m, "VelocityMultiplier", t, p.r, Vec3::ONE) * p.vel.length() * 100.0;
                if flag(m, "MultiplyX", true) {
                    size.x *= k.x;
                }
                if flag(m, "MultiplyY", true) {
                    size.y *= k.y;
                }
                if flag(m, "MultiplyZ", true) {
                    size.z *= k.z;
                }
            }
            // rain drops fade in (`m_FadingRate`)
            "DisParticleModuleRainDrops" => alpha *= (p.age * f1(m, "m_FadingRate", 0.0, p.r, 8.0)).min(1.0),
            _ => {}
        }
    }
    (color, alpha, size)
}

fn simulate(
    time: Res<Time>,
    data: Option<Res<ParticleData>>,
    assets: Option<Res<GameAssets>>,
    mut commands: Commands,
    cam: Query<&GlobalTransform, With<PlayerCamera>>,
    mut emitters: Query<(Entity, &mut ParticleEmitter)>,
    globals: Query<&GlobalTransform, Without<ParticleEmitter>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut vis: Query<&mut Visibility>,
    mut transforms: Query<&mut Transform>,
    mut log_timer: Local<f32>,
    mut lens_done: ResMut<LensDone>,
    rain: Res<crate::rain::Rain>,
    lens_fov: Res<LensFov>,
    (mut events, rapier): (MessageWriter<ParticleEventFired>, bevy_rapier3d::prelude::ReadRapierContext),
) {
    // the world particles strike (its geometry), within a budget of rays a frame
    let rctx = rapier.single().ok();
    let rays_left = std::cell::Cell::new(4000u32);
    let world = bevy_rapier3d::prelude::QueryFilter::default().groups(bevy_rapier3d::prelude::CollisionGroups::new(bevy_rapier3d::prelude::Group::ALL, crate::level::GROUP_WORLD));
    let ray_fn = |from: Vec3, dir: Vec3, len: f32| -> Option<(Vec3, Vec3)> {
        let c = rctx.as_ref()?;
        if rays_left.get() == 0 {
            return None;
        }
        rays_left.set(rays_left.get() - 1);
        c.cast_ray_and_get_normal(from, dir, len, true, world).map(|(_, hit)| (hit.point, hit.normal))
    };
    let lens_k = lens_fov.0;
    let (Some(data), Some(assets), Ok(cam)) = (data, assets, cam.single()) else { return };
    let dt = time.delta_secs().min(0.1);
    // DH_PFX_LOG: nearby emitters and their particle counts every 2 s
    *log_timer -= dt;
    let log = std::env::var("DH_PFX_LOG").is_ok() && *log_timer <= 0.0;
    if log {
        *log_timer = if std::env::var("DH_PFX_FAST").is_ok() { 0.1 } else { 2.0 };
        for (_, em) in &emitters {
            let pos = em.xf.transform_point3(Vec3::ZERO);
            let d = pos.distance(cam.translation());
            if d > 15.0 {
                continue;
            }
            let Some(sys) = data.scene.particle_systems.get(em.system as usize) else { continue };
            let counts: Vec<String> = sys
                .emitters
                .iter()
                .zip(em.emitters.iter())
                .map(|(def, st)| {
                    let m = data.scene.materials.get(def.material as usize).map(|m| m.name.as_str()).unwrap_or("?");
                    let orig = assets.particle_mats.get(&def.material).is_some_and(|m| m.original());
                    let first = st.particles.first().map(|p| {
                        let (c, a, sz) = over_life(def, p);
                        format!(" [size {:.0} alpha {:.2} rgb {:.2} {:.2} {:.2} age {:.2}/{:.2}]", sz.x, a, c.x, c.y, c.z, p.age, p.life)
                    });
                    format!("{}{}={}{}", m.rsplit('.').next().unwrap_or(m), if orig { "*" } else { "" }, st.particles.len(), first.unwrap_or_default())
                })
                .collect();
            info!("pfx {} d={d:.1} active={} awake={} {:?}", sys.name.rsplit('.').next().unwrap_or(""), em.active, em.awake, counts);
        }
    }
    // DH_PFX_STATS: the busiest emitters (particles, pooled mesh instances) every 2 s
    if std::env::var("DH_PFX_STATS").is_ok() && log_timer.fract() < dt && *log_timer > 0.0 {
        let mut rows: Vec<(usize, usize, String)> = emitters
            .iter()
            .flat_map(|(_, em)| {
                let name = data.scene.particle_systems.get(em.system as usize).map(|s| s.name.rsplit('.').next().unwrap_or("").to_string()).unwrap_or_default();
                em.emitters.iter().enumerate().map(move |(k, st)| (st.particles.len(), st.pool.len(), format!("{name}#{k}")))
            })
            .collect();
        rows.sort_by(|a, b| b.0.cmp(&a.0));
        let total: usize = rows.iter().map(|r| r.0).sum();
        let pooled: usize = rows.iter().map(|r| r.1).sum();
        info!("pfx stats: {total} particles, {pooled} mesh instances; top {:?}", &rows[..rows.len().min(8)]);
    }
    if std::env::var("DH_PFX_STATS").is_ok() && *log_timer <= 0.0 {
        *log_timer = 2.0;
    }
    let eye = cam.translation();
    let right = cam.right().as_vec3();
    let up = cam.up().as_vec3();
    for (ent, mut em) in &mut emitters {
        let em = &mut *em;
        // gameplay effects: follow their carrier, end after their time
        if let (Some((e, _)), Some(_)) = (em.follow, em.lens.as_ref()) {
            // a lens effect turns with the view: its particles stay where they are on the lens
            if let Ok(g) = globals.get(e) {
                let new = lens_xf(g, lens_k);
                let delta = new * em.xf.inverse();
                for st in &mut em.emitters {
                    for p in &mut st.particles {
                        p.pos = delta.transform_point3(p.pos);
                        p.vel = delta.transform_vector3(p.vel);
                        p.acc = delta.transform_vector3(p.acc);
                        if let Some(t) = p.target.as_mut() {
                            *t = delta.transform_point3(*t);
                        }
                    }
                }
                em.xf = new;
                for st in &em.emitters {
                    if let Ok(mut t) = transforms.get_mut(st.entity) {
                        t.translation = new.w_axis.truncate();
                    }
                }
            }
            // over once stopped (or, not looping, its emitters done) and the last particles gone
            if let Some(sys) = data.scene.particle_systems.get(em.system as usize) {
                let l = em.lens.as_mut().unwrap();
                l.age += dt;
                let spent = sys.emitters.iter().zip(em.emitters.iter()).all(|(def, st)| {
                    let loops = value(&def.required, "EmitterLoops", 0.0) as u32;
                    loops > 0 && st.loops >= loops
                });
                let empty = em.emitters.iter().all(|st| st.particles.is_empty());
                if l.age > 0.5 && empty && (l.stopping || (!l.looping && spent)) {
                    em.life = Some(0.0);
                }
            }
        } else if let Some((e, off)) = em.follow {
            match globals.get(e) {
                Ok(g) => {
                    let at = g.translation() + off;
                    let mut new = match em.turn {
                        Some(r) => Mat4::from_rotation_translation(g.rotation() * r, at),
                        None => em.xf,
                    };
                    new.w_axis = at.extend(1.0);
                    // local-space emitters (`bUseLocalSpace`) carry their particles along
                    if let Some(sys) = data.scene.particle_systems.get(em.system as usize) {
                        let delta = new * em.xf.inverse();
                        for (def, st) in sys.emitters.iter().zip(em.emitters.iter_mut()) {
                            if value(&def.required, "bUseLocalSpace", 0.0) != 0.0 {
                                for p in &mut st.particles {
                                    p.pos = delta.transform_point3(p.pos);
                                    p.vel = delta.transform_vector3(p.vel);
                                }
                            }
                        }
                    }
                    em.xf = new;
                    for st in &em.emitters {
                        if let Ok(mut t) = transforms.get_mut(st.entity) {
                            t.translation = at;
                        }
                    }
                }
                Err(_) => em.life = Some(0.0),
            }
        }
        if let Some(life) = em.life.as_mut() {
            *life -= dt;
            if *life <= 0.0 {
                if let Some(l) = &em.lens {
                    lens_done.0.push(l.op);
                }
                for st in &em.emitters {
                    commands.entity(st.entity).despawn();
                    for &e in &st.pool {
                        commands.entity(e).despawn();
                    }
                }
                commands.entity(ent).despawn();
                continue;
            }
        }
        let Some(sys) = data.scene.particle_systems.get(em.system as usize) else { continue };
        // rain (Arkane's native modules): only the camera's rain box, about the eye
        let rain_sys = sys.emitters.iter().any(|d| d.modules.iter().any(|m| m.class.starts_with("DisParticleModuleRain")));
        let rain_box = rain_sys && rain.active() && rain.emitters.contains(&em.index);
        if rain_sys && !rain_box {
            if em.emitters.iter().any(|st| !st.particles.is_empty()) {
                for st in &mut em.emitters {
                    st.particles.clear();
                    if let Ok(mut v) = vis.get_mut(st.entity) {
                        *v = Visibility::Hidden;
                    }
                }
            }
            continue;
        }
        if rain_box {
            em.xf = Mat4::from_translation(eye);
            em.awake = true;
            for st in &em.emitters {
                if let Ok(mut t) = transforms.get_mut(st.entity) {
                    t.translation = eye;
                }
            }
        }
        let pos = em.xf.transform_point3(Vec3::ZERO);
        let near = em.active && pos.distance_squared(eye) < em.range * em.range;
        if !near {
            if em.awake {
                em.awake = false;
                for st in &mut em.emitters {
                    st.particles.clear();
                    st.time = 0.0;
                    st.loops = 0;
                    if let Ok(mut v) = vis.get_mut(st.entity) {
                        *v = Visibility::Hidden;
                    }
                    for &e in &st.pool {
                        if let Ok(mut v) = vis.get_mut(e) {
                            *v = Visibility::Hidden;
                        }
                    }
                }
            }
            continue;
        }
        let (xf, scale) = (em.xf, em.scale);
        let halt = em.lens.as_ref().is_some_and(|l| l.stopping);
        let rain_box = rain_box && em.index != u32::MAX;
        let mut impacts: Vec<Vec3> = Vec::new();
        // the emitter's own velocity (what it rides on), for `VelocityInheritParent`
        let at_now = xf.transform_point3(Vec3::ZERO);
        let emitter_vel = if dt > 0.0 { (at_now - em.last_pos) / dt } else { Vec3::ZERO };
        em.last_pos = at_now;
        let cx = StepCtx { ray: Some(&ray_fn), emitter_vel: if emitter_vel.length() < 50.0 { emitter_vel } else { Vec3::ZERO } };
        let quiet = StepCtx { ray: None, emitter_vel: Vec3::ZERO };
        let mut sys_events: Vec<PfxEvent> = Vec::new();
        if !em.awake {
            em.awake = true;
            // start in the steady state
            for (def, st) in sys.emitters.iter().zip(em.emitters.iter_mut()) {
                let mut t = 0.0;
                while t < WARMUP {
                    step(def, st, &xf, scale, 0.1, false, None, &quiet, &mut Vec::new());
                    t += 0.1;
                }
            }
        }
        // particles of the system's emitters by name, for those spawning at them
        // (`ParticleModuleLocationEmitter`)
        let sources: std::collections::HashMap<&str, Vec<(Vec3, Vec3)>> = sys
            .emitters
            .iter()
            .zip(em.emitters.iter())
            .filter(|(def, _)| sys.emitters.iter().any(|o| o.modules.iter().any(|m| m.class == "ParticleModuleLocationEmitter" && m.names.get("EmitterName").is_some_and(|n| *n == def.name))))
            .map(|(def, st)| (def.name.as_str(), st.particles.iter().map(|p| (p.pos, p.vel)).collect()))
            .collect();
        for (def, st) in sys.emitters.iter().zip(em.emitters.iter_mut()) {
            if rain_box && def.modules.iter().any(|m| m.class == "DisParticleModuleRainDrops") {
                rain_drops(def, st, &rain, eye, cam.forward().as_vec3(), dt, &mut impacts);
            } else if rain_box && def.modules.iter().any(|m| m.class == "DisParticleModuleRainImpacts") {
                let mut spots: Vec<Spot> = impacts.iter().map(|&pos| Spot { pos, normal: None, vel: None }).collect();
                step(def, st, &xf, scale, dt, halt, Some(&mut spots), &cx, &mut sys_events);
            } else if def.modules.iter().any(|m| m.class == "ParticleModuleEventReceiverSpawn") {
                // (spawned by the system's events: below)
                step(def, st, &xf, scale, dt, true, None, &cx, &mut sys_events);
            } else if let Some(m) = def.modules.iter().find(|m| m.class == "ParticleModuleLocationEmitter") {
                // at another emitter's particles
                let src = m.names.get("EmitterName").and_then(|n| sources.get(n.as_str())).cloned().unwrap_or_default();
                let inherit = flag(m, "InheritSourceVelocity", false);
                let k = value(m, "InheritSourceVelocityScale", 1.0);
                let mut spots: Vec<Spot> = (0..64)
                    .filter_map(|i| {
                        if src.is_empty() {
                            return None;
                        }
                        let (pos, vel) = if m.names.get("SelectionMethod").is_some_and(|s| s == "ELESM_Sequential") { src[i % src.len()] } else { src[rand::random_range(0..src.len())] };
                        Some(Spot { pos, normal: None, vel: inherit.then_some(vel * k) })
                    })
                    .collect();
                step(def, st, &xf, scale, dt, halt, Some(&mut spots), &cx, &mut sys_events);
            } else {
                step(def, st, &xf, scale, dt, halt, None, &cx, &mut sys_events);
            }
        }
        // the system's events: its receivers spawn at them (`ParticleModuleEventReceiverSpawn`:
        // a blood drop's strike makes a splatter there), the level scripts hear a placed one's
        // (`SeqEvent_ParticleEvent`)
        if !sys_events.is_empty() {
            for (def, st) in sys.emitters.iter().zip(em.emitters.iter_mut()) {
                for m in def.modules.iter().filter(|m| m.class == "ParticleModuleEventReceiverSpawn") {
                    let name = m.names.get("EventName").cloned().unwrap_or_default();
                    let kind = match m.names.get("EventGeneratorType").map(|s| s.as_str()) {
                        Some("EPET_Spawn") => 0,
                        Some("EPET_Death") => 1,
                        Some("EPET_Collision") => 2,
                        _ => 2,
                    };
                    let inherit = flag(m, "bInheritVelocity", false);
                    let k = f3(m, "InheritVelocityScale", 0.0, rand3(), Vec3::ONE);
                    for evt in sys_events.iter().filter(|e| e.kind == kind && (name.is_empty() || name == "None" || e.name == name)) {
                        let n = f1(m, "SpawnCount", 0.0, rand3(), 1.0).max(0.0) as u32;
                        let spot = Spot { pos: evt.pos, normal: evt.normal, vel: inherit.then_some(evt.vel * Vec3::new(k.x, k.z, k.y)) };
                        for _ in 0..n.min(16) {
                            if st.particles.len() >= MAX_PARTICLES {
                                break;
                            }
                            st.particles.push(spawn_one(def, &xf, scale, 0.0, xf.transform_point3(Vec3::ZERO), Some(&spot), &cx));
                        }
                    }
                }
            }
            if em.index != u32::MAX {
                let mut told = std::collections::HashSet::new();
                for evt in &sys_events {
                    if told.insert(evt.name.clone()) {
                        events.write(ParticleEventFired { index: em.index, name: evt.name.clone() });
                    }
                }
            }
        }
        for (def, st) in sys.emitters.iter().zip(em.emitters.iter_mut()) {
            // mesh particles: pooled instances (`DH_NO_MESH_PFX`: none, for profiling)
            if let Some(mesh_id) = def.mesh.filter(|_| std::env::var("DH_NO_MESH_PFX").is_err()) {
                let Some((mesh, mat)) = assets.particle_meshes.get(&mesh_id) else { continue };
                while st.pool.len() < st.particles.len().min(48) {
                    let mut ec = commands.spawn((Mesh3d(mesh.clone()), Transform::IDENTITY, Visibility::Hidden, bevy::light::NotShadowCaster, DespawnOnExit(GameState::InGame)));
                    mat.apply(&mut ec);
                    if em.view {
                        ec.insert(bevy::camera::visibility::RenderLayers::layer(crate::combat::VIEW_LAYER));
                    }
                    st.pool.push(ec.id());
                }
                // (turning with the emitter in local space, after the mesh's own turn)
                let base = def.mesh_rotation.map(Quat::from_array).unwrap_or(Quat::IDENTITY);
                let carried = if value(&def.required, "bUseLocalSpace", 0.0) != 0.0 { xf.to_scale_rotation_translation().1 } else { Quat::IDENTITY };
                for (k, &e) in st.pool.iter().enumerate() {
                    let shown = match st.particles.get(k) {
                        Some(p) => {
                            let (_, _, size) = over_life(def, p);
                            if let Ok(mut t) = transforms.get_mut(e) {
                                t.translation = p.pos;
                                // (its own turn about UE's X, Y, Z: Bevy's X, Z, Y)
                                let turn3 = Quat::from_rotation_x(p.rot3.x) * Quat::from_rotation_z(p.rot3.y) * Quat::from_rotation_y(p.rot3.z);
                                t.rotation = carried * base * turn3 * Quat::from_rotation_y(p.rot);
                                t.scale = Vec3::new(size.x, size.z, size.y).abs().max(Vec3::splat(1e-3));
                            }
                            true
                        }
                        None => false,
                    };
                    if let Ok(mut v) = vis.get_mut(e) {
                        let want = if shown { Visibility::Inherited } else { Visibility::Hidden };
                        if *v != want {
                            *v = want;
                        }
                    }
                }
                continue;
            }
            let Some(handle) = &st.mesh else { continue };
            let want = if st.particles.is_empty() { Visibility::Hidden } else { Visibility::Inherited };
            if let Ok(mut v) = vis.get_mut(st.entity) {
                if *v != want {
                    *v = want;
                }
            }
            if st.particles.is_empty() {
                continue;
            }
            let req = &def.required;
            let (sh, sv) = (value(req, "SubImages_Horizontal", 1.0).max(1.0), value(req, "SubImages_Vertical", 1.0).max(1.0));
            let images = sh * sv;
            let interp = req.names.get("InterpolationMethod").map(|s| s.as_str()).unwrap_or("PSUVIM_None");
            let align = req.names.get("ScreenAlignment").map(|s| s.as_str()).unwrap_or("PSA_Square");
            // ParticleModuleOrientationAxisLock: face along an axis (EPAL_X..: the sprite's up
            // and right are the emitter's axes as UE3's `GetAxisLockValues` has them), or turn
            // about it towards the camera (EPAL_ROTATE_X..)
            let axis = |v: Vec3| em.xf.transform_vector3(ue(v)).normalize_or(Vec3::Y);
            let lock = def
                .modules
                .iter()
                .find(|m| m.class.contains("OrientationAxisLock"))
                .and_then(|m| m.names.get("LockAxisFlags"))
                .and_then(|f| {
                    Some(match f.as_str() {
                        "EPAL_X" => Lock::Plane(axis(Vec3::Z), axis(Vec3::Y)),
                        "EPAL_Y" => Lock::Plane(axis(Vec3::Z), axis(Vec3::NEG_X)),
                        "EPAL_Z" => Lock::Plane(axis(Vec3::X), axis(Vec3::NEG_Y)),
                        "EPAL_NEGATIVE_X" => Lock::Plane(axis(Vec3::Z), axis(Vec3::NEG_Y)),
                        "EPAL_NEGATIVE_Y" => Lock::Plane(axis(Vec3::Z), axis(Vec3::X)),
                        "EPAL_NEGATIVE_Z" => Lock::Plane(axis(Vec3::X), axis(Vec3::Y)),
                        "EPAL_ROTATE_X" => Lock::Turn(axis(Vec3::X)),
                        "EPAL_ROTATE_Y" => Lock::Turn(axis(Vec3::Y)),
                        "EPAL_ROTATE_Z" => Lock::Turn(axis(Vec3::Z)),
                        _ => return None,
                    })
                });
            let original = assets.particle_mats.get(&def.material).is_some_and(|m| m.original());
            let additive = !original
                && matches!(data.scene.materials.get(def.material as usize).map(|m| m.blend), Some(dhcook::format::Blend::Additive));
            let n = st.particles.len();
            let mut positions = Vec::with_capacity(n * 4);
            let mut normals = Vec::with_capacity(n * 4);
            let mut uvs = Vec::with_capacity(n * 4);
            let mut colors = Vec::with_capacity(n * 4);
            let mut tangents = Vec::with_capacity(n * 4);
            let mut indices = Vec::with_capacity(n * 6);
            // back to front
            let mut order: Vec<usize> = (0..n).collect();
            order.sort_by(|&a, &b| st.particles[b].pos.distance_squared(eye).total_cmp(&st.particles[a].pos.distance_squared(eye)));
            let noise = beam_noise(def);
            // a flipbook over the particle's life (`ParticleModuleSubUV`), decals lying on what
            // they struck (`ParticleModuleTypeDataDecal`), a ribbon through the particles
            // (`ParticleModuleTypeDataRibbon`)
            let subuv = def.modules.iter().find(|m| m.class == "ParticleModuleSubUV");
            let decal = def.modules.iter().any(|m| m.class == "ParticleModuleTypeDataDecal");
            let ribbon = def.modules.iter().any(|m| m.class == "ParticleModuleTypeDataRibbon");
            if ribbon && st.particles.len() >= 2 {
                // oldest to newest, a camera-facing strip as wide as each point's size
                let mut pts: Vec<&Particle> = st.particles.iter().collect();
                pts.sort_by(|a, b| b.age.total_cmp(&a.age));
                let n = pts.len();
                for k in 0..n - 1 {
                    let (p0, p1) = (pts[k], pts[k + 1]);
                    let (c0, a0, s0) = over_life(def, p0);
                    let (c1, a1, s1) = over_life(def, p1);
                    let along = (p1.pos - p0.pos).normalize_or(Vec3::Y);
                    let side0 = along.cross(eye - p0.pos).normalize_or(right) * (s0.x * 0.005);
                    let side1 = along.cross(eye - p1.pos).normalize_or(right) * (s1.x * 0.005);
                    let base = positions.len() as u32;
                    let (u0, u1) = (k as f32 / (n - 1) as f32, (k + 1) as f32 / (n - 1) as f32);
                    let col = |c: Vec3, a: f32| {
                        let a = a.clamp(0.0, 1.0);
                        let rgb = if additive { c * a } else { c };
                        [rgb.x.max(0.0), rgb.y.max(0.0), rgb.z.max(0.0), a]
                    };
                    for (q, sd, u, v, c) in [(p0.pos, side0, u0, 0.0, col(c0, a0)), (p1.pos, side1, u1, 0.0, col(c1, a1)), (p1.pos, -side1, u1, 1.0, col(c1, a1)), (p0.pos, -side0, u0, 1.0, col(c0, a0))] {
                        positions.push((q + sd - pos).to_array());
                        normals.push((eye - q).normalize_or(Vec3::Y).to_array());
                        tangents.push(along.extend(1.0).to_array());
                        uvs.push([u, v]);
                        colors.push(c);
                    }
                    indices.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
                }
            }
            for &i in &order {
                if ribbon {
                    break;
                }
                let p = &st.particles[i];
                let (color, alpha, size) = over_life(def, p);
                // (orbiting about where it is: `ParticleModuleOrbit`, in the emitter's frame)
                let at = match p.orbit {
                    Some((off, rot, _)) => {
                        let q = Quat::from_euler(EulerRot::XYZ, rot.x * std::f32::consts::TAU, rot.y * std::f32::consts::TAU, rot.z * std::f32::consts::TAU);
                        p.pos + em.xf.transform_vector3(ue(q * off))
                    }
                    None => p.pos,
                };
                // a beam (`ParticleModuleTypeDataBeam2`, its source, target and noise modules): a
                // camera-facing ribbon through its noise points
                if let Some(target) = p.target {
                    let n = noise.as_ref().map(|n| n.points).unwrap_or(0) + 2;
                    let epoch = noise.as_ref().map(|nz| (p.age / nz.lock) as u32).unwrap_or(0);
                    let pts: Vec<Vec3> = (0..n)
                        .map(|k| {
                            let t = k as f32 / (n - 1) as f32;
                            let mut q = p.pos.lerp(target, t);
                            if let (Some(nz), true) = (noise.as_ref(), k > 0 && k + 1 < n) {
                                let off = Vec3::new(hash(p.r[0], k, epoch), hash(p.r[1], k, epoch), hash(p.r[2], k, epoch)) * nz.range;
                                q += em.xf.transform_vector3(ue(off));
                            }
                            q
                        })
                        .collect();
                    let w = size.x * 0.01 * 0.5;
                    let a = alpha.clamp(0.0, 1.0);
                    let rgb = if additive { color * a } else { color };
                    let c = [rgb.x.max(0.0), rgb.y.max(0.0), rgb.z.max(0.0), a];
                    for k in 0..(n as usize - 1) {
                        let (q0, q1) = (pts[k], pts[k + 1]);
                        let along = (q1 - q0).normalize_or(Vec3::Y);
                        let side = along.cross(eye - q0).normalize_or(right) * w;
                        let base = positions.len() as u32;
                        let (u0, u1) = (k as f32 / (n - 1) as f32, (k + 1) as f32 / (n - 1) as f32);
                        for (q, s, u, v) in [(q0, side, u0, 0.0), (q1, side, u1, 0.0), (q1, -side, u1, 1.0), (q0, -side, u0, 1.0)] {
                            positions.push((q + s - pos).to_array());
                            normals.push((eye - q).normalize_or(Vec3::Y).to_array());
                            tangents.push(along.extend(1.0).to_array());
                            uvs.push([u, v]);
                            colors.push(c);
                        }
                        indices.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
                    }
                    continue;
                }
                let (w, h) = (size.x * 0.01, if align == "PSA_Square" { size.x } else { size.y } * 0.01);
                let (r_axis, u_axis) = if let (true, Some(n)) = (decal, p.normal) {
                    // a decal: flat on the surface it struck, turned by its rotation
                    let t0 = n.any_orthonormal_vector();
                    let b0 = n.cross(t0);
                    let (s, c) = p.rot.sin_cos();
                    (t0 * c + b0 * s, b0 * c - t0 * s)
                } else if let Some(Lock::Turn(axis)) = lock {
                    // cylindrical: up along the axis, facing the camera around it
                    (axis.cross(eye - p.pos).normalize_or(right), axis)
                } else if let Some(Lock::Plane(lup, lright)) = lock {
                    // UE3's sprite: size X along `-up` turned by the rotation (the texture's u),
                    // size Y along `right` (v)
                    let (s, c) = p.rot.sin_cos();
                    let right_rot = -lup * c + lright * s;
                    let up_rot = lup * s + lright * c;
                    (right_rot, -up_rot)
                } else if align == "PSA_Velocity" && p.vel.length_squared() > 1e-6 {
                    let u = p.vel.normalize();
                    let r = u.cross(eye - p.pos).normalize_or(right);
                    (r, u)
                } else {
                    let (s, c) = p.rot.sin_cos();
                    (right * c + up * s, up * c - right * s)
                };
                let base = positions.len() as u32;
                let hw = r_axis * (w * 0.5);
                let hh = u_axis * (h * 0.5);
                // (a decal just off its surface)
                let at = match (decal, p.normal) {
                    (true, Some(n)) => at + n * 0.01,
                    _ => at,
                };
                let facing = match (decal, p.normal) {
                    (true, Some(n)) => n,
                    _ => (eye - at).normalize_or(Vec3::Y),
                };
                for (dx, dy) in [(-1.0, 1.0), (1.0, 1.0), (1.0, -1.0), (-1.0, -1.0)] {
                    positions.push((at - pos + hw * dx + hh * dy).to_array());
                    normals.push(facing.to_array());
                    tangents.push(r_axis.extend(1.0).to_array());
                }
                // flipbook cell
                let frame = match (subuv, interp) {
                    // the module's index over the particle's life
                    (Some(m), i) if i != "PSUVIM_None" => f1(m, "SubImageIndex", (p.age / p.life).clamp(0.0, 1.0), p.r, 0.0).floor().clamp(0.0, images - 1.0),
                    (_, "PSUVIM_Linear" | "PSUVIM_Linear_Blend") => ((p.age / p.life) * images).floor().min(images - 1.0),
                    (_, "PSUVIM_Random" | "PSUVIM_Random_Blend") => (p.r[0] * images).floor().min(images - 1.0),
                    _ => p.image.floor().clamp(0.0, images - 1.0),
                };
                let (cx, cy) = (frame % sh, (frame / sh).floor());
                let (u0, v0, u1, v1) = (cx / sh, cy / sv, (cx + 1.0) / sh, (cy + 1.0) / sv);
                uvs.extend_from_slice(&[[u0, v0], [u1, v0], [u1, v1], [u0, v1]]);
                let a = alpha.clamp(0.0, 1.0);
                let rgb = if additive { color * a } else { color };
                let c = [rgb.x.max(0.0), rgb.y.max(0.0), rgb.z.max(0.0), a];
                colors.extend_from_slice(&[c, c, c, c]);
                indices.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
            }
            // (nothing to draw after all - a ribbon of one point: hidden, its mesh left as it
            // was, an empty one having no room on the GPU)
            if positions.is_empty() {
                if let Ok(mut v) = vis.get_mut(st.entity) {
                    *v = Visibility::Hidden;
                }
                continue;
            }
            if let Some(mut mesh) = meshes.get_mut(handle) {
                mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions);
                mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, normals);
                mesh.insert_attribute(Mesh::ATTRIBUTE_UV_1, uvs.clone());
                mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, uvs);
                mesh.insert_attribute(Mesh::ATTRIBUTE_TANGENT, tangents);
                mesh.insert_attribute(Mesh::ATTRIBUTE_COLOR, colors);
                mesh.insert_indices(Indices::U32(indices));
            }
        }
    }
}
