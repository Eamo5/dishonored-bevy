//! Matinee playback: the level scripts' `SeqAct_Interp` sequences (cooked into
//! `kismet.matinees`) move actors along their keyframed paths, show / hide and switch them,
//! cut between cameras and fade the screen. The VM keeps each matinee's clock and fires its
//! event and sound keys; this module applies the tracks to the world every frame.

use crate::arms::ArmsRoot;
use crate::combat::ViewModel;
use crate::kismet::Vm;
use crate::level::{InstanceCollider, LevelInstance, LevelLight};
use crate::particles::ParticleEmitter;
use crate::player::{Player, PlayerCamera};
use crate::GameState;
use bevy::prelude::*;
use dhcook::format::{KActor, KCurvePoint, KMatinee, KTrack};
use dhcook::xform::{ue_dir, ue_point};
use std::collections::{HashMap, HashSet};

pub struct MatineePlugin;

impl Plugin for MatineePlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<MatineeState>()
            .init_resource::<CinematicFade>()
            .init_resource::<SceneArms>()
            .add_systems(OnEnter(GameState::InGame), reset)
            // (skipping: held, `skip`)
            .add_systems(Update, (play_matinees, ride_platforms).chain().after(crate::save::restore_npcs).run_if(in_state(GameState::InGame)))
            .add_systems(PostUpdate, cinematic_camera.before(TransformSystems::Propagate).run_if(in_state(GameState::InGame)));
    }
}

/// Corvo's arms in a scene (the player group's animation keys: holding the dying Empress):
/// the sequence, the time into it, looping, rate, and the key (matinee, index).
#[derive(Resource, Default)]
pub struct SceneArms(pub Option<(String, f32, bool, f32, (u32, usize))>);

/// Screen fade set by a matinee's director (0 clear .. 1 black), overriding script fades.
#[derive(Resource, Default)]
pub struct CinematicFade(pub Option<f32>);

#[derive(Resource, Default)]
pub struct MatineeState {
    /// characters walking in a scene, and those a scene moves; those looking at something
    npc_walk: HashSet<Entity>,
    lookers: HashSet<Entity>,
    scene_npcs: HashSet<Entity>,
    /// actors' initial placement (UE space), for tracks relative to it
    initial: HashMap<u32, Mat4>,
    /// render / collider entities' transforms before any matinee moved them
    base: HashMap<Entity, Transform>,
    /// instance index -> (render entity, collider entity)
    by_instance: HashMap<u32, (Entity, Option<Entity>)>,
    /// the director's camera: world transform and vertical field of view (degrees)
    pub camera: Option<(Transform, f32)>,
    /// the player's camera before the cut-scene took it
    saved: Option<(Transform, f32)>,
    /// colliders moved this frame: (entity, before, after)
    platforms: Vec<(Entity, Transform, Transform)>,
    /// props a scene animates: the key each one plays (matinee, key index)
    prop_keys: HashMap<Entity, (u32, usize)>,
    /// NPCs a scene animates: the key each one plays (matinee, key index)
    npc_keys: HashMap<Entity, (u32, usize)>,
    /// (matinee op, NPC) already put on their stage mark
    staged: HashSet<(u32, Entity)>,
    /// the player carried by a scene: where the ride began, and the scene (op)
    player_base: Option<Transform>,
    player_ride: Option<u32>,
    /// carried characters: where they were when the ride began
    ride_base: HashMap<Entity, Transform>,
}

impl MatineeState {
    fn record_platform_motion(&mut self, entity: Entity, before: Transform, after: Transform) {
        if let Some(motion) = self.platforms.iter_mut().find(|m| m.0 == entity) {
            motion.2 = after;
        } else {
            self.platforms.push((entity, before, after));
        }
    }
    pub fn saved_player_ride(&self) -> Option<(u32, [f32; 16])> {
        Some((self.player_ride?, self.player_base?.to_matrix().to_cols_array()))
    }

    pub fn restore_player_ride(&mut self, saved: Option<(u32, [f32; 16])>) {
        self.player_ride = saved.map(|(op, _)| op);
        self.player_base = saved.map(|(_, base)| Transform::from_matrix(Mat4::from_cols_array(&base)));
    }

    pub fn saved_ride_base(&self, entity: Entity) -> Option<[f32; 16]> {
        self.ride_base.get(&entity).map(|t| t.to_matrix().to_cols_array())
    }

    pub fn restore_ride_base(&mut self, entity: Entity, base: [f32; 16]) {
        self.ride_base.insert(entity, Transform::from_matrix(Mat4::from_cols_array(&base)));
    }

    /// A mover's place before any matinee moved it (taken as it is now if not known yet): a
    /// save puts movers back where their matinees left them, relative to it.
    pub fn base_of(&mut self, e: Entity, now: Transform) -> Transform {
        *self.base.entry(e).or_insert(now)
    }
}

fn reset(mut st: ResMut<MatineeState>, mut fade: ResMut<CinematicFade>) {
    *st = MatineeState::default();
    fade.0 = None;
}

#[cfg(test)]
mod ride_save_tests {
    use super::*;

    #[test]
    fn platform_motion_uses_frame_endpoints_not_intermediate_attachment_updates() {
        let entity = Entity::PLACEHOLDER;
        let mut state = MatineeState::default();
        let start = Transform::from_xyz(2.0, 0.0, 3.0);
        let intermediate = Transform::from_rotation(Quat::from_rotation_y(1.57));
        let end = Transform::from_xyz(2.1, 0.0, 3.0);
        state.record_platform_motion(entity, start, intermediate);
        state.record_platform_motion(entity, intermediate, end);
        assert_eq!(state.platforms.len(), 1);
        let (_, before, after) = state.platforms[0];
        let delta = after.to_matrix() * before.to_matrix().inverse();
        assert!(delta.abs_diff_eq(Mat4::from_translation(Vec3::X * 0.1), 1e-5));
    }

    #[test]
    fn player_ride_round_trip_retains_initial_placement_and_clears_stale_state() {
        let initial = Transform::from_xyz(12.0, 2.0, -7.0)
            .with_rotation(Quat::from_rotation_y(0.4));
        let mut original = MatineeState::default();
        original.player_ride = Some(42);
        original.player_base = Some(initial);
        let encoded = serde_json::to_string(&original.saved_player_ride()).unwrap();
        let mut restored = MatineeState::default();
        restored.restore_player_ride(serde_json::from_str(&encoded).unwrap());
        assert_eq!(restored.player_ride, Some(42));
        // On a ride without a stage mark, the restored current transform must
        // never replace the original base, otherwise movement gets applied twice.
        let delta = Mat4::from_translation(Vec3::new(4.0, 0.0, 3.0));
        let current = Transform::from_matrix(delta * initial.to_matrix());
        let base = restored.player_base.get_or_insert(current);
        assert!((delta * base.to_matrix()).abs_diff_eq(current.to_matrix(), 1e-5));
        restored.restore_player_ride(None);
        assert!(restored.player_base.is_none());
        assert!(restored.player_ride.is_none());
    }

    #[test]
    fn restored_passenger_base_does_not_apply_vehicle_motion_twice() {
        let mut world = World::new();
        let old = world.spawn_empty().id();
        let new = world.spawn_empty().id();
        let base = Transform::from_xyz(-89.0, -20.0, 65.0)
            .with_rotation(Quat::from_rotation_y(0.3));
        let delta = Transform::from_xyz(2.0, 0.0, -12.0)
            .with_rotation(Quat::from_rotation_y(-0.2)).to_matrix();
        let saved_position = Transform::from_matrix(delta * base.to_matrix());
        let mut original = MatineeState::default();
        original.ride_base.insert(old, base);
        let data = original.saved_ride_base(old).unwrap();
        let mut restored = MatineeState::default();
        restored.restore_ride_base(new, data);
        let initial = *restored.ride_base.entry(new).or_insert(saved_position);
        let resumed = delta * initial.to_matrix();
        assert!(resumed.abs_diff_eq(saved_position.to_matrix(), 1e-5));
        assert!(!(delta * saved_position.to_matrix()).abs_diff_eq(resumed, 0.01));
        assert!(restored.saved_ride_base(old).is_none());
    }
}

/// UE3 FRotationMatrix (column vectors, UE space).
fn rot_matrix(r: [i32; 3]) -> Mat4 {
    let k = std::f32::consts::TAU / 65536.0;
    let (sp, cp) = (r[0] as f32 * k).sin_cos();
    let (sy, cy) = (r[1] as f32 * k).sin_cos();
    let (sr, cr) = (r[2] as f32 * k).sin_cos();
    Mat4::from_cols(
        Vec4::new(cp * cy, cp * sy, sp, 0.0),
        Vec4::new(sr * sp * cy - cr * sy, sr * sp * sy + cr * cy, -sr * cp, 0.0),
        Vec4::new(-(cr * sp * cy + sr * sy), cy * sr - cr * sp * sy, cr * cp, 0.0),
        Vec4::W,
    )
}

/// A UE-space matrix in Bevy space (swap Y/Z, centimetres to metres).
fn ue_to_bevy(m: Mat4) -> Mat4 {
    let swap = Mat4::from_cols(Vec4::X, Vec4::Z, Vec4::Y, Vec4::W);
    Mat4::from_scale(Vec3::splat(0.01)) * swap * m * swap * Mat4::from_scale(Vec3::splat(100.0))
}

fn cubic(p0: f32, t0: f32, p1: f32, t1: f32, a: f32) -> f32 {
    let a2 = a * a;
    let a3 = a2 * a;
    (2.0 * a3 - 3.0 * a2 + 1.0) * p0 + (a3 - 2.0 * a2 + a) * t0 + (a3 - a2) * t1 + (-2.0 * a3 + 3.0 * a2) * p1
}

/// UE3 FInterpCurve evaluation (linear, constant or Hermite segments).
fn eval(points: &[KCurvePoint], t: f32) -> Option<[f32; 3]> {
    let first = points.first()?;
    if points.len() == 1 || t <= first.t {
        return Some(first.v);
    }
    let last = points.last()?;
    if t >= last.t {
        return Some(last.v);
    }
    let i = points.windows(2).position(|w| t >= w[0].t && t < w[1].t)?;
    let (a, b) = (&points[i], &points[i + 1]);
    let d = (b.t - a.t).max(1e-6);
    let k = (t - a.t) / d;
    Some(match a.mode {
        2 => a.v,
        0 => [0, 1, 2].map(|c| a.v[c] + (b.v[c] - a.v[c]) * k),
        _ => [0, 1, 2].map(|c| cubic(a.v[c], a.leave[c] * d, b.v[c], b.arrive[c] * d, k)),
    })
}

/// The latest key at or before `t` of a stepped track.
fn latest<T: Copy>(keys: &[(f32, T)], t: f32) -> Option<T> {
    keys.iter().filter(|k| k.0 <= t + 1e-4).last().map(|k| k.1)
}

fn actor_tm(a: &KActor) -> Mat4 {
    Mat4::from_translation(Vec3::from(a.ue_location)) * rot_matrix(a.rotation)
}

/// A UE-space actor transform evaluated from a move track.
fn move_tm(pos: &[KCurvePoint], rot: &[KCurvePoint], relative: bool, init: Mat4, t: f32) -> Mat4 {
    let key = |t: f32| {
        let p = eval(pos, t).map(Vec3::from).unwrap_or(Vec3::ZERO);
        // Euler track: X roll, Y pitch, Z yaw, in degrees
        let e = eval(rot, t).unwrap_or([0.0; 3]);
        let k = 65536.0 / 360.0;
        Mat4::from_translation(p) * rot_matrix([(e[1] * k) as i32, (e[2] * k) as i32, (e[0] * k) as i32])
    };
    if relative {
        // relative to where the actor stood when it began: the first key is that place (the
        // Tower's boat is lowered from the dock, sails, and rises in the waterlock)
        init * key(f32::MIN).inverse() * key(t)
    } else {
        key(t)
    }
}

/// A Bevy camera transform from a UE actor matrix (UE cameras look down +X, up +Z).
fn camera_transform(m: Mat4) -> Transform {
    let pos = Vec3::from(ue_point(m.w_axis.truncate().to_array()));
    let fwd = Vec3::from(ue_dir(m.x_axis.truncate().to_array())).normalize_or(Vec3::NEG_Z);
    let up = Vec3::from(ue_dir(m.z_axis.truncate().to_array())).normalize_or(Vec3::Y);
    Transform::from_translation(pos).looking_to(fwd, up)
}

/// An ambient scene's enemy fighting Corvo (no cinematic holding him) breaks off it: its AI
/// takes over.
fn broke_off(cine: &crate::script_world::Cinematic, npc: &crate::npc::Npc) -> bool {
    !cine.on && npc.enemy && npc.alert == crate::npc::Alert::Combat && npc.mode == crate::npc::Mode::Combat
}

#[allow(clippy::too_many_arguments, clippy::type_complexity)]
fn play_matinees(
    vm: Option<Res<Vm>>,
    mut st: ResMut<MatineeState>,
    mut fade: ResMut<CinematicFade>,
    mut instances: Query<(Entity, &LevelInstance, &mut Transform, &mut Visibility, Option<&InstanceCollider>), Without<LevelLight>>,
    mut colliders: Query<&mut Transform, (Without<LevelInstance>, Without<LevelLight>, Without<crate::npc::Npc>, Without<Player>, With<bevy_rapier3d::prelude::Collider>)>,
    (mut commands, mut npcs): (
        Commands,
        Query<(Entity, &crate::npc::FromSpawner, &mut crate::anim::Animator, &mut Transform, &mut crate::npc::Npc, Option<&crate::npc::NpcAnim>), (Without<LevelInstance>, Without<LevelLight>)>,
    ),
    mut lights: Query<(&LevelLight, &mut Visibility), Without<LevelInstance>>,
    mut emitters: Query<(Entity, &mut ParticleEmitter)>,
    (time, mut player, mut fog_state, mut light_levels, mut scene_arms): (
        Res<Time>,
        Query<(&mut Transform, &mut Player), (Without<crate::npc::Npc>, Without<LevelInstance>)>,
        ResMut<crate::fog::FogState>,
        ResMut<crate::fxlight::LightLevels>,
        ResMut<SceneArms>,
    ),
    (rigs, globals, mut props, cine): (
        Query<(&crate::npc::FromSpawner, &crate::npc::NpcRig)>,
        Query<&GlobalTransform>,
        Query<(Entity, &crate::propanim::PropRig, &mut crate::anim::Animator), Without<crate::npc::FromSpawner>>,
        Res<crate::script_world::Cinematic>,
    ),
) {
    let Some(vm) = vm else { return };
    let plays = vm.matinees();
    st.camera = None;
    st.platforms.clear();
    scene_arms.0 = None;
    fade.0 = None;
    let mut scripted: HashSet<Entity> = HashSet::new();
    if plays.is_empty() {
        // scenes over: their characters go back to their own behaviour
        for (e, _) in st.npc_keys.drain() {
            commands.entity(e).try_remove::<crate::npc::ScriptedAnim>();
        }
        for e in st.scene_npcs.drain() {
            commands.entity(e).try_remove::<crate::npc::ScriptedAnim>();
        }
        for e in st.lookers.drain() {
            commands.entity(e).try_remove::<crate::npc::SceneLook>();
        }
        st.npc_walk.clear();
        st.staged.clear();
        st.ride_base.clear();
        if st.player_base.take().is_some() {
            if let Ok((_, mut p)) = player.single_mut() {
                p.locked = false;
            }
        }
        return;
    }
    if st.by_instance.is_empty() {
        st.by_instance = instances.iter().map(|(e, li, _, _, c)| (li.index, (e, c.map(|c| c.0)))).collect();
    }
    let g = vm.g.clone();
    if std::env::var("DH_MATINEE_LOG").is_ok() {
        static LAST: std::sync::Mutex<f32> = std::sync::Mutex::new(0.0);
        let mut last = LAST.lock().unwrap();
        if vm.time - *last > 2.0 {
            *last = vm.time;
            for (op, mi, t, binds) in &plays {
                let names: Vec<String> = binds
                    .iter()
                    .map(|(gi, a)| {
                        let gn = g.matinees.get(*mi as usize).and_then(|m| m.groups.get(*gi as usize)).map(|gr| gr.name.clone()).unwrap_or_default();
                        format!("{gn}={:?}", a.iter().filter_map(|x| g.actors.get(*x as usize)).map(|ka| format!("{}({} inst)", ka.name, ka.instances.len())).collect::<Vec<_>>())
                    })
                    .collect();
                info!("matinee op {op} #{mi} t={t:.1}: {names:?}");
            }
        }
    }
    let mut cam: Option<(Transform, f32)> = None;
    let dt = time.delta_secs();
    let player_pos = player.single().ok().map(|(t, _)| t.translation);
    let mut player_rides = false;
    let npc_pos: HashMap<u32, Vec3> = npcs.iter().filter(|x| !x.4.is_down()).map(|x| (x.1 .0, x.3.translation)).collect();
    for (op, mi, t, binds) in plays {
        let Some(m): Option<&KMatinee> = g.matinees.get(mi as usize) else { continue };
        // a distraction's scene (`ESAP_Distractor`): the character is the distraction's to
        // move (distraction.rs, with its loops); the scene carries the props and effects
        let drives_pawns = !matches!(g.ops.get(op as usize).and_then(|o| o.props.get("m_AIBehaviorPriority")), Some(dhcook::format::KVal::Str(p)) if p == "ESAP_Distractor");
        // where each camera group's actor is now
        let mut group_tm: HashMap<u32, (Mat4, f32)> = HashMap::new();
        // `InterpTrackAttachment`: a group's character carries the actors of another group on
        // a bone (a glass in the hand, a cigarette, a letter); what each carried group's
        // actors move by now (Bevy space). A prop carried by a prop keeps its place on it.
        let mut rides: HashMap<u32, Mat4> = HashMap::new();
        let first_actor = |gi: u32| binds.iter().find(|b| b.0 == gi).and_then(|b| b.1.first()).copied();
        for _ in 0..3 {
            for (gi, _) in binds {
                let Some(group) = m.groups.get(*gi as usize) else { continue };
                for tr in &group.tracks {
                    let KTrack::Attach { target, keys } = tr else { continue };
                    let Some(k) = keys.iter().rev().find(|k| k.0 <= t + 1e-4).filter(|k| k.1) else { continue };
                    let Some(tg) = m.groups.iter().position(|gr| gr.name.eq_ignore_ascii_case(target)).map(|x| x as u32) else { continue };
                    if rides.contains_key(&tg) || tg == *gi {
                        continue;
                    }
                    // the player's group rides without a binding
                    let target_player = m.groups.get(tg as usize).is_some_and(|gr| gr.player);
                    let Some(oa) = first_actor(*gi) else { continue };
                    let ta = first_actor(tg);
                    if ta.is_none() && !target_player {
                        continue;
                    }
                    let Some(owner) = g.actors.get(oa as usize) else { continue };
                    let rider_init = match ta.and_then(|ta| g.actors.get(ta as usize).map(|r| (ta, r))) {
                        Some((ta, rider)) => ue_to_bevy(*st.initial.entry(ta).or_insert_with(|| actor_tm(rider))),
                        None => Mat4::IDENTITY,
                    };
                    let owner_init = *st.initial.entry(oa).or_insert_with(|| actor_tm(owner));
                    let local = ue_to_bevy(Mat4::from_translation(Vec3::from(k.3)) * rot_matrix(k.4));
                    let bone = owner
                        .spawner
                        .and_then(|s| rigs.iter().find(|(f, _)| f.0 == s))
                        .and_then(|(_, rig)| rig.joint(&k.2))
                        .and_then(|j| globals.get(j).ok())
                        .map(|gt| {
                            let (_, r, p) = gt.to_scale_rotation_translation();
                            Mat4::from_rotation_translation(r, p)
                        });
                    let delta = match bone {
                        Some(b) => Some(b * local * rider_init.inverse()),
                        None => rides
                            .get(gi)
                            .copied()
                            .or_else(|| {
                                group.tracks.iter().find_map(|tr| match tr {
                                    KTrack::Move { pos, rot, relative } => Some(ue_to_bevy(move_tm(pos, rot, *relative, owner_init, t) * owner_init.inverse())),
                                    _ => None,
                                })
                            })
                            .map(|d| if k.2.to_ascii_lowercase().contains("root") { d * ue_to_bevy(owner_init) * local * rider_init.inverse() } else { d }),
                    };
                    if let Some(d) = delta {
                        rides.insert(tg, d);
                    }
                }
            }
        }
        for (gi, actors) in binds {
            let Some(group) = m.groups.get(*gi as usize) else { continue };
            for &a in actors {
                let Some(ka) = g.actors.get(a as usize) else { continue };
                let init = *st.initial.entry(a).or_insert_with(|| actor_tm(ka));
                let mut tm = init;
                let mut fov = if ka.fov > 0.0 { ka.fov } else { 90.0 };
                // instances follow the actor (and their colliders); carried, it goes where the
                // bone takes it
                let mut delta = rides.get(gi).copied();
                if delta.is_none() {
                    if let Some(KTrack::Move { pos, rot, relative }) = group.tracks.iter().find(|tr| matches!(tr, KTrack::Move { .. })) {
                        tm = move_tm(pos, rot, *relative, init, t);
                        delta = Some(ue_to_bevy(tm * init.inverse()));
                    }
                }
                if let Some(delta) = delta {
                    // carried effects (smoke on a cigarette)
                    if rides.contains_key(gi) {
                        if !ka.particles.is_empty() {
                            for (pe, mut em) in &mut emitters {
                                if ka.particles.contains(&em.index) {
                                    let base = *st.base.entry(pe).or_insert_with(|| Transform::from_matrix(em.xf()));
                                    em.set_xf(delta * base.to_matrix());
                                }
                            }
                        }
                    }
                    for &i in ka.instances.iter().chain(&ka.riders) {
                        let Some(&(e, col)) = st.by_instance.get(&i) else { continue };
                        for ent in [Some(e), col].into_iter().flatten() {
                            let base = match st.base.get(&ent) {
                                Some(b) => *b,
                                None => {
                                    let b = if ent == e {
                                        instances.get(e).map(|x| *x.2).unwrap_or_default()
                                    } else {
                                        colliders.get(ent).map(|x| *x).unwrap_or_default()
                                    };
                                    st.base.insert(ent, b);
                                    b
                                }
                            };
                            let new = Transform::from_matrix(delta * base.to_matrix());
                            if ent == e {
                                if let Ok((_, _, mut tf, _, _)) = instances.get_mut(e) {
                                    *tf = new;
                                }
                            } else if let Ok(mut tf) = colliders.get_mut(ent) {
                                // remember the motion: whoever stands on it rides along
                                let prev = *tf;
                                if prev.translation != new.translation || prev.rotation != new.rotation {
                                    st.platforms.push((ent, prev, new));
                                }
                                *tf = new;
                            }
                        }
                    }
                }
                for tr in &group.tracks {
                    match tr {
                        KTrack::Visibility(keys) => {
                            if let Some(show) = latest(keys, t) {
                                for &i in &ka.instances {
                                    if let Some(&(e, _)) = st.by_instance.get(&i) {
                                        if let Ok((_, _, _, mut vis, _)) = instances.get_mut(e) {
                                            *vis = if show { Visibility::Inherited } else { Visibility::Hidden };
                                        }
                                    }
                                }
                            }
                        }
                        KTrack::Toggle(keys) => {
                            if let Some(on) = latest(keys, t) {
                                for (ll, mut vis) in &mut lights {
                                    if ka.lights.contains(&ll.0) {
                                        *vis = if on { Visibility::Inherited } else { Visibility::Hidden };
                                    }
                                }
                                let set: HashSet<u32> = ka.particles.iter().copied().collect();
                                for (_, mut em) in &mut emitters {
                                    if set.contains(&em.index) && em.active != on {
                                        em.active = on;
                                    }
                                }
                            }
                        }
                        KTrack::Float { prop, points } if prop.eq_ignore_ascii_case("FOVAngle") => {
                            if let Some(v) = eval(points, t) {
                                fov = v[0];
                            }
                        }
                        // a fog actor's layer (`DisFogComponent0.Opacity`: the Tower's boat ride
                        // clears the river fog)
                        KTrack::Float { prop, points } if prop.starts_with("DisFogComponent") => {
                            if let (Some(layer), Some(v)) = (ka.fog, eval(points, t)) {
                                let field = prop.rsplit('.').next().unwrap_or("").to_string();
                                fog_state.overrides.insert((layer as usize, field), v[0]);
                            }
                        }
                        // a light's brightness (flickers, lightning)
                        KTrack::Float { prop, points } if prop.ends_with(".Brightness") => {
                            if let Some(v) = eval(points, t) {
                                for &li in &ka.lights {
                                    light_levels.want.insert(li, v[0]);
                                }
                            }
                        }
                        _ => {}
                    }
                }
                group_tm.entry(*gi).or_insert((tm, fov));
                // scripted scenes: the NPC of a spawner plays the group's animations
                if let (true, Some(s)) = (drives_pawns, ka.spawner) {
                    let anim_keys = group.tracks.iter().find_map(|tr| if let KTrack::Anim(k) = tr { Some(k) } else { None });
                    if let Some(keys) = anim_keys {
                        for (e, from, mut animator, mut ntf, mut npc, _) in &mut npcs {
                            if from.0 != s || npc.is_down() {
                                continue;
                            }
                            if broke_off(&cine, &npc) {
                                continue;
                            }
                            scripted.insert(e);
                            // the stage mark: where the scene puts the character
                            if !group.stage_mark.is_empty() && st.staged.insert((op, e)) {
                                let mark = m.groups.iter().position(|gr| gr.name.eq_ignore_ascii_case(&group.stage_mark));
                                let actor = mark.and_then(|mi| binds.iter().find(|b| b.0 == mi as u32)).and_then(|b| b.1.first()).and_then(|a| g.actors.get(*a as usize));
                                if let Some(mark) = actor.filter(|m| m.spawner != Some(s)) {
                                    ntf.translation = Vec3::from(mark.position) + Vec3::Y * (crate::npc::NPC_CENTER + 0.05);
                                    npc.yaw = mark.yaw;
                                    ntf.rotation = Quat::from_rotation_y(mark.yaw);
                                }
                            }
                            let Some(k) = keys.iter().rposition(|k| k.0 <= t + 1e-4) else { continue };
                            let (start, seq, offset, rate, looping) = &keys[k];
                            if st.npc_keys.get(&e) == Some(&(mi, k)) {
                                continue;
                            }
                            let Some(clip) = animator.lib.find(seq) else { continue };
                            if std::env::var("DH_MATINEE_LOG").is_ok() {
                                info!("matinee #{mi}: {} plays {seq} at t={t:.1}", ka.name);
                            }
                            animator.restart(clip, *looping, rate.max(0.01), 0.2);
                            animator.seek(offset + (t - start).max(0.0) * rate);
                            st.npc_keys.insert(e, (mi, k));
                            commands.entity(e).try_insert(crate::npc::ScriptedAnim);
                        }
                    }
                }
                // a prop the scene animates (the gangway lowering)
                if ka.device.is_some() {
                    if let Some(keys) = group.tracks.iter().find_map(|tr| if let KTrack::Anim(k) = tr { Some(k) } else { None }) {
                        if let Some(k) = keys.iter().rposition(|k| k.0 <= t + 1e-4) {
                            let (start, seq, offset, rate, looping) = &keys[k];
                            for (e, pr, mut animator) in &mut props {
                                if pr.actor != a || st.prop_keys.get(&e) == Some(&(mi, k)) {
                                    continue;
                                }
                                let Some(clip) = animator.lib.find(seq) else { continue };
                                if std::env::var("DH_MATINEE_LOG").is_ok() {
                                    info!("matinee #{mi}: prop {} plays {seq} at t={t:.1}", ka.name);
                                }
                                animator.restart(clip, *looping, rate.max(0.01), 0.0);
                                animator.seek(offset + (t - start).max(0.0) * rate);
                                st.prop_keys.insert(e, (mi, k));
                            }
                        }
                    }
                }
                // carried characters (riding a boat), from where the scene put them
                if let (Some(s), Some(delta)) = (ka.spawner, rides.get(gi).copied()) {
                    for (e, from, _, mut ntf, mut npc, _) in &mut npcs {
                        if from.0 != s || npc.is_down() {
                            continue;
                        }
                        let base = *st.ride_base.entry(e).or_insert(*ntf);
                        let new = Transform::from_matrix(delta * base.to_matrix());
                        ntf.translation = new.translation;
                        ntf.rotation = new.rotation;
                        npc.yaw = new.rotation.to_euler(EulerRot::YXZ).0;
                        scripted.insert(e);
                        st.scene_npcs.insert(e);
                        commands.entity(e).try_insert(crate::npc::ScriptedAnim);
                    }
                }
            }
        }
        // Corvo's arms play the player group's animation keys
        if let Some(gr) = m.groups.iter().find(|gr| gr.player) {
            if let Some(keys) = gr.tracks.iter().find_map(|tr| if let KTrack::Anim(k) = tr { Some(k) } else { None }) {
                if let Some(k) = keys.iter().rposition(|k| k.0 <= t + 1e-4) {
                    let (start, seq, offset, rate, looping) = &keys[k];
                    scene_arms.0 = Some((seq.clone(), offset + (t - start).max(0.0) * rate, *looping, rate.max(0.01), (mi, k)));
                }
            }
        }
        // the player riding (a boat): from the player group's stage mark, carried along, the
        // controls held until it lets go
        if let Some(pg) = m.groups.iter().position(|gr| gr.player) {
            // let go: the original steps Corvo off with the player group's animation
            // (`m_bSnapToRootBoneLocationWhenFinished`); here, onto the scene's drop area
            if !rides.contains_key(&(pg as u32)) && st.player_base.is_some() && st.player_ride == Some(op) {
                st.player_base = None;
                st.player_ride = None;
                if let Ok((mut pt, mut p)) = player.single_mut() {
                    p.locked = false;
                    let drop = m
                        .groups
                        .iter()
                        .position(|gr| gr.name.to_ascii_lowercase().contains("droparea"))
                        .and_then(|di| binds.iter().find(|b| b.0 == di as u32))
                        .and_then(|b| b.1.first())
                        .and_then(|a| g.actors.get(*a as usize));
                    if let Some(d) = drop {
                        pt.translation = Vec3::from(d.position) + Vec3::Y * (crate::player::STAND_HALF + crate::player::RADIUS + 0.1);
                        p.velocity = Vec3::ZERO;
                    }
                }
            }
            if let (Some(delta), Ok((mut pt, mut p))) = (rides.get(&(pg as u32)).copied(), player.single_mut()) {
                st.player_ride = Some(op);
                let base = *st.player_base.get_or_insert_with(|| {
                    let mark = m.groups[pg].stage_mark.as_str();
                    let actor = (!mark.is_empty())
                        .then(|| m.groups.iter().position(|gr| gr.name.eq_ignore_ascii_case(mark)))
                        .flatten()
                        .and_then(|mi| binds.iter().find(|b| b.0 == mi as u32))
                        .and_then(|b| b.1.first())
                        .and_then(|a| g.actors.get(*a as usize));
                    match actor {
                        Some(a) => {
                            p.yaw = a.yaw;
                            Transform::from_translation(Vec3::from(a.position) + Vec3::Y * (crate::player::STAND_HALF + crate::player::RADIUS + 0.05)).with_rotation(Quat::from_rotation_y(a.yaw))
                        }
                        None => *pt,
                    }
                });
                let new = Transform::from_matrix(delta * base.to_matrix());
                pt.translation = new.translation;
                p.locked = true;
                p.velocity = Vec3::ZERO;
                player_rides = true;
            }
        }
        // scene movement: characters walk to their marks and turn to face one another
        for (gi, actors) in binds.iter().filter(|_| drives_pawns) {
            let Some(group) = m.groups.get(*gi as usize) else { continue };
            let loco = group.tracks.iter().find_map(|tr| if let KTrack::Locomotion(k) = tr { k.iter().find(|k| t >= k.0 && t < k.0 + k.1.max(0.5)) } else { None });
            let face = group.tracks.iter().find_map(|tr| if let KTrack::FaceTo(k) = tr { k.iter().find(|k| t >= k.0 && t < k.0 + k.1.max(0.3)) } else { None });
            let look = group.tracks.iter().find_map(|tr| if let KTrack::LookAt(k) = tr { k.iter().find(|k| t >= k.0 && t < k.0 + k.1.max(0.3)) } else { None });
            if loco.is_none() && face.is_none() && look.is_none() {
                continue;
            }
            // a target: the player, or the actor bound to the group of that name (where its
            // character is now)
            let target = |name: &str| -> Option<Vec3> {
                if name.eq_ignore_ascii_case("Player") {
                    return player_pos;
                }
                let tg = m.groups.iter().position(|gr| gr.name.eq_ignore_ascii_case(name))?;
                let a = binds.iter().find(|b| b.0 == tg as u32).and_then(|b| b.1.first()).and_then(|a| g.actors.get(*a as usize))?;
                Some(a.spawner.and_then(|s| npc_pos.get(&s).copied()).unwrap_or(Vec3::from(a.position)))
            };
            for &a in actors {
                let Some(s) = g.actors.get(a as usize).and_then(|ka| ka.spawner) else { continue };
                for (e, from, mut animator, mut ntf, mut npc, nanim) in &mut npcs {
                    if from.0 != s || npc.is_down() || broke_off(&cine, &npc) {
                        continue;
                    }
                    scripted.insert(e);
                    st.scene_npcs.insert(e);
                    commands.entity(e).try_insert(crate::npc::ScriptedAnim);
                    let pos = ntf.translation;
                    let mut walking = false;
                    if let Some((_, _, gait, to)) = loco {
                        if let Some(tp) = target(to) {
                            let d = (tp - pos).with_y(0.0);
                            if d.length() > 0.3 {
                                let run = gait.eq_ignore_ascii_case("Run") || gait.eq_ignore_ascii_case("Sprint");
                                let step = if run { 4.0 } else { 1.45 } * dt;
                                ntf.translation += d.normalize() * step.min(d.length());
                                npc.yaw = (-d.x).atan2(-d.z);
                                walking = true;
                                if !st.npc_walk.contains(&e) {
                                    if std::env::var("DH_MATINEE_LOG").is_ok() {
                                        info!("matinee #{mi}: {} walks ({gait}) to {to} ({:.1} m)", npc.name, d.length());
                                    }
                                    if let Some(c) = nanim.and_then(|na| na.gait_clip(run)) {
                                        animator.play(c, true, 1.0, 0.2);
                                    }
                                    st.npc_walk.insert(e);
                                }
                            }
                        }
                    }
                    if !walking && st.npc_walk.remove(&e) {
                        // back to the scene's animation
                        st.npc_keys.remove(&e);
                    }
                    if let (false, Some((_, _, to))) = (walking, face) {
                        if let Some(tp) = target(to) {
                            let d = (tp - pos).with_y(0.0);
                            if d.length() > 0.1 {
                                let want = (-d.x).atan2(-d.z);
                                let dy = (want - npc.yaw + std::f32::consts::PI).rem_euclid(std::f32::consts::TAU) - std::f32::consts::PI;
                                npc.yaw += dy.clamp(-3.0 * dt, 3.0 * dt);
                            }
                        }
                    }
                    ntf.rotation = Quat::from_rotation_y(npc.yaw);
                    // the head turns to what it looks at, easing in and out over the key
                    if let Some((start, len, to)) = look {
                        if let Some(tp) = target(to) {
                            let w = ((t - start) / 0.4).min((start + len - t) / 0.4).clamp(0.0, 1.0);
                            commands.entity(e).try_insert(crate::npc::SceneLook { target: tp, weight: w });
                            st.lookers.insert(e);
                        }
                    } else if st.lookers.remove(&e) {
                        commands.entity(e).try_remove::<crate::npc::SceneLook>();
                    }
                }
            }
        }
        // director: the camera of the latest cut
        if let Some(name) = latest(&m.cuts.iter().map(|c| (c.0, c.1.as_str())).collect::<Vec<_>>(), t) {
            let gi = m.groups.iter().position(|gr| gr.name.eq_ignore_ascii_case(name));
            if let Some((tm, fov)) = gi.and_then(|gi| group_tm.get(&(gi as u32))) {
                cam = Some((camera_transform(*tm), *fov));
            }
        }
        if let Some(v) = eval(&m.fade, t) {
            fade.0 = Some(v[0].clamp(0.0, 1.0));
        }
    }
    st.camera = cam;
    // the ride let go of the player
    if !player_rides && st.player_base.take().is_some() {
        if let Ok((_, mut p)) = player.single_mut() {
            p.locked = false;
        }
    }
    // characters whose scene ended return to their AI
    let ended: Vec<Entity> = st.npc_keys.keys().chain(st.scene_npcs.iter()).filter(|e| !scripted.contains(e)).copied().collect();
    for e in ended {
        st.npc_keys.remove(&e);
        st.scene_npcs.remove(&e);
        st.npc_walk.remove(&e);
        st.ride_base.remove(&e);
        if st.lookers.remove(&e) {
            commands.entity(e).try_remove::<crate::npc::SceneLook>();
        }
        commands.entity(e).try_remove::<crate::npc::ScriptedAnim>();
    }
}

/// Moving platforms (matinee movers): the player standing on one moves with it, like UE3's
/// actor basing.
fn ride_platforms(
    st: Res<MatineeState>,
    rapier: bevy_rapier3d::plugin::ReadRapierContext,
    mut player: Query<(Entity, &mut Transform, &mut Player)>,
) {
    if st.platforms.is_empty() {
        return;
    }
    let Ok((pe, mut t, mut p)) = player.single_mut() else { return };
    let Ok(ctx) = rapier.single() else { return };
    let filter = bevy_rapier3d::prelude::QueryFilter::default().exclude_collider(pe);
    let Some((hit, _)) = ctx.cast_ray(t.translation, Vec3::NEG_Y, 2.2, true, filter) else { return };
    let Some((_, before, after)) = st.platforms.iter().find(|x| x.0 == hit) else { return };
    // a platform put back to where its next scene starts isn't carrying anyone along
    if before.translation.distance(after.translation) > 3.0 {
        return;
    }
    let delta = after.to_matrix() * before.to_matrix().inverse();
    t.translation = delta.transform_point3(t.translation);
    // turn with it (about the vertical)
    let f = delta.transform_vector3(Vec3::NEG_Z).with_y(0.0);
    if f.length_squared() > 1e-6 {
        p.yaw += (-f.x).atan2(-f.z);
    }
}

/// The director's camera replaces the player's view (and the first-person model) while a
/// cut-scene plays.
#[allow(clippy::type_complexity)]
fn cinematic_camera(
    mut st: ResMut<MatineeState>,
    mut player: Query<(&GlobalTransform, &mut Player)>,
    mut cams: Query<(&mut Transform, &mut Projection), With<PlayerCamera>>,
    mut view_models: Query<&mut Visibility, Or<(With<ArmsRoot>, With<ViewModel>)>>,
) {
    let Ok((pgt, mut p)) = player.single_mut() else { return };
    let Ok((mut ct, mut proj)) = cams.single_mut() else { return };
    match st.camera {
        Some((world, fov)) => {
            if st.saved.is_none() {
                let f = match &*proj {
                    Projection::Perspective(pp) => pp.fov,
                    _ => 75f32.to_radians(),
                };
                st.saved = Some((*ct, f));
                p.locked = true;
                for mut v in &mut view_models {
                    *v = Visibility::Hidden;
                }
            }
            // the camera is the player's child: express the shot in the player's frame
            let local = pgt.affine().inverse() * world.compute_affine();
            *ct = Transform::from_matrix(local.into());
            if let Projection::Perspective(pp) = &mut *proj {
                // UE FOV is horizontal
                let aspect = pp.aspect_ratio.max(0.1);
                pp.fov = 2.0 * ((fov.to_radians() * 0.5).tan() / aspect).atan();
            }
        }
        None => {
            if let Some((t, f)) = st.saved.take() {
                *ct = t;
                if let Projection::Perspective(pp) = &mut *proj {
                    pp.fov = f;
                }
                p.locked = false;
                for mut v in &mut view_models {
                    *v = Visibility::Inherited;
                }
            }
        }
    }
}
