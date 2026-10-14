//! The Overseers' music boxes: a musical Overseer (`Pwn_MusicOverseer_*`, `DisWepMusicBox`)
//! fighting Corvo plays the protective tune (`DisTweaks_Tune_Protection`), and so do the
//! level's tune sources (`DisProtectionTuneSource`): within its disorient radius (6 m) the
//! world sways, within its inhibit radius (4 m) Corvo's powers fail. Each radius has its start
//! and stop sounds. A musical Overseer facing Corvo plays its combat tune at him
//! (`DisTweaks_NPCTune_Combat`: 20 m, 45 degrees; `Music_Amp_Damage`), whose musical damage
//! (90 every 0.3 s) tears through the rats of a swarm in its cone; it stops a moment after he
//! leaves the cone (`m_fStopWhenOutOfRangeTimer`) or its sight (`m_fStopWhenNotSeeingTimer`).

use crate::audio::{event_id, PostEvent, StopEvent};
use crate::level::LevelInfo;
use crate::npc::{Mode, Npc};
use crate::player::{Player, PlayerCamera};
use crate::GameState;
use bevy::prelude::*;

pub struct MusicBoxPlugin;

impl Plugin for MusicBoxPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Tune>()
            .add_systems(OnEnter(GameState::InGame), |mut t: ResMut<Tune>| *t = Tune::default())
            .add_systems(Update, (hear_tunes, combat_tunes, mark_playing).before(crate::player::PlayerMoveSet).run_if(in_state(GameState::InGame)))
            .add_systems(Update, sway.after(crate::player::PlayerMoveSet).run_if(in_state(GameState::InGame)))
            .add_systems(PostUpdate, follow_wearer.before(crate::anim::AnimPose).run_if(in_state(GameState::InGame)));
    }
}

/// The music box's tune (`Twk_TuneSource_Protection_Default`): radii and sounds.
const DISORIENT: f32 = 6.0;
const INHIBIT: f32 = 4.0;
const SOUNDS: [&str; 4] = ["Music_Tune_Protect_Disorient", "Music_Tune_Protect_Disorient_Stop", "Music_Tune_Protect_Inhibit", "Music_Tune_Protect_Inhibit_Stop"];

/// What the tune does to Corvo now.
#[derive(Resource, Default)]
pub struct Tune {
    /// his powers fail
    pub inhibited: bool,
    /// how much the world sways (0..1)
    pub disorient: f32,
    in_disorient: bool,
    t: f32,
}

/// A music box's own rig, worn by a musical Overseer (`owner`): its parts move in the clip
/// named as the one its wearer's arms play.
#[derive(Component)]
pub struct MusicBoxRig {
    pub owner: Entity,
}

/// The music box's skinned rig under its wearer's `MusicBox` socket (`at` on `joint`), lit
/// with its wearer (`slot`); its owner is set once the wearer is spawned.
pub fn spawn_rig(commands: &mut Commands, vis: &crate::level::NpcVisual, joint: Entity, at: Transform, slot: u32) -> Entity {
    use bevy::camera::visibility::DynamicSkinnedMeshBounds;
    use bevy::mesh::skinning::SkinnedMesh;
    use bevy::mesh::MeshTag;
    let root = commands.spawn((at, Visibility::Inherited)).id();
    commands.entity(joint).add_child(root);
    let bones = &vis.skeleton.bones;
    let joints: Vec<Entity> = bones
        .iter()
        .map(|b| commands.spawn(Transform::from_translation(Vec3::from(b.translation)).with_rotation(Quat::from_array(b.rotation).normalize())).id())
        .collect();
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
    if let Some(lib) = vis.anims.clone() {
        let mut anim = crate::anim::Animator::new(lib, &vis.skeleton, joints);
        anim.sounds = false;
        commands.entity(root).insert(anim);
    }
    root
}

/// A music box's parts follow its wearer's arms (the box's `DisWepMusicBox` mesh animated with
/// them): the clip of the name its wearer plays (the upper body's, else the whole body's), at
/// the same time in it; else the box at rest (`Musicbox_Idle`).
fn follow_wearer(mut rigs: Query<(&MusicBoxRig, &mut crate::anim::Animator)>, wearers: Query<&crate::anim::Animator, Without<MusicBoxRig>>) {
    for (rig, mut anim) in &mut rigs {
        let Ok(w) = wearers.get(rig.owner) else { continue };
        let lib = anim.lib.clone();
        let same = |p: crate::anim::Playing| lib.find(&w.lib.clip(p.clip).name).map(|c| (c, p));
        match w.upper_playing().and_then(same).or_else(|| w.current().and_then(same)) {
            Some((c, p)) => {
                if anim.current().map(|q| q.clip) != Some(c) {
                    if std::env::var("DH_TUNE_LOG").is_ok() {
                        info!("music box: {}", lib.clip(c).name);
                    }
                    anim.restart(c, p.looping, p.speed, 0.15);
                }
                anim.seek(p.t);
            }
            None => {
                if let Some(c) = lib.find("Musicbox_Idle").filter(|c| anim.current().map(|q| q.clip) != Some(*c)) {
                    anim.restart(c, true, 1.0, 0.2);
                }
            }
        }
    }
}

/// A musical Overseer playing its music box now (its arms crank it: `npc.rs`).
#[derive(Component)]
pub struct TunePlaying;

/// Marks the musical Overseers playing their music boxes.
fn mark_playing(mut commands: Commands, npcs: Query<(Entity, &Npc, &crate::npc::FromSpawner, Has<TunePlaying>)>, vm: Option<Res<crate::kismet::Vm>>) {
    for (e, n, from, on) in &npcs {
        if n.arms.tunes.is_none() && !n.pawn.contains("MusicOverseer") {
            continue;
        }
        let p = plays(n, from.0, vm.as_deref());
        if p && !on {
            commands.entity(e).try_insert(TunePlaying);
        } else if !p && on {
            commands.entity(e).try_remove::<TunePlaying>();
        }
    }
}

#[allow(clippy::too_many_arguments)]
/// Whether a musical Overseer plays its music box: as the scripts force or forbid
/// (`DisSeqAct_PlayMusicBox`), else while it fights Corvo.
fn plays(n: &Npc, spawner: u32, vm: Option<&crate::kismet::Vm>) -> bool {
    if n.is_down() {
        return false;
    }
    match vm.and_then(|v| v.music_box.get(&spawner)) {
        Some(forced) => *forced,
        None => n.mode == Mode::Combat,
    }
}

fn hear_tunes(
    time: Res<Time>,
    level: Option<Res<LevelInfo>>,
    mut tune: ResMut<Tune>,
    player: Query<&Transform, With<Player>>,
    npcs: Query<(&Npc, &Transform, &crate::npc::FromSpawner), Without<Player>>,
    vm: Option<Res<crate::kismet::Vm>>,
    stats: Res<crate::gameplay::PlayerStats>,
    (mut sfx, mut stop): (MessageWriter<PostEvent>, MessageWriter<StopEvent>),
) {
    let Ok(pt) = player.single() else { return };
    let dt = time.delta_secs();
    tune.t += dt;
    let at = pt.translation;
    // the nearest tune playing: (distance, radii, sounds, where)
    let mut best: Option<(f32, f32, f32, [String; 4], Vec3)> = None;
    let mut consider = |d: f32, dis: f32, inh: f32, snd: [String; 4], pos: Vec3| {
        if d < dis && best.as_ref().is_none_or(|b| d < b.0) {
            best = Some((d, dis, inh, snd, pos));
        }
    };
    if let Some(level) = level.as_ref() {
        for s in &level.scene.tune_sources {
            let pos = Vec3::from(s.position);
            consider(pos.distance(at), s.disorient, s.inhibit, s.sounds.clone(), pos);
        }
    }
    for (n, t, from) in &npcs {
        // a musical Overseer playing at Corvo (its music box's protective tune)
        if plays(n, from.0, vm.as_deref()) {
            if let Some(tu) = n.arms.tunes.as_ref() {
                let snd = std::array::from_fn(|i| if tu.protect_sounds[i].is_empty() { SOUNDS[i].to_string() } else { tu.protect_sounds[i].clone() });
                consider(t.translation.distance(at), tu.disorient, tu.inhibit, snd, t.translation);
            } else if n.pawn.contains("MusicOverseer") {
                consider(t.translation.distance(at), DISORIENT, INHIBIT, SOUNDS.map(String::from), t.translation);
            }
        }
    }
    let alive = !stats.dead;
    let (inhibit, disorient, snd, pos) = match best.filter(|_| alive) {
        Some((d, dis, inh, snd, pos)) => (d < inh, (1.0 - (d / dis.max(0.1))).clamp(0.0, 1.0).sqrt(), Some(snd), pos),
        None => (false, 0.0, None, at),
    };
    let snd = snd.unwrap_or_else(|| SOUNDS.map(String::from));
    // the sounds as Corvo crosses the radii
    if disorient > 0.0 && !tune.in_disorient {
        sfx.write(PostEvent::named(&snd[0], Some(pos)));
    } else if disorient <= 0.0 && tune.in_disorient {
        stop.write(StopEvent(event_id(&snd[0])));
        sfx.write(PostEvent::named(&snd[1], Some(pos)));
    }
    if inhibit && !tune.inhibited {
        sfx.write(PostEvent::named(&snd[2], Some(pos)));
    } else if !inhibit && tune.inhibited {
        stop.write(StopEvent(event_id(&snd[2])));
        sfx.write(PostEvent::named(&snd[3], Some(pos)));
    }
    tune.in_disorient = disorient > 0.0;
    tune.inhibited = inhibit;
    tune.disorient += (disorient - tune.disorient) * (dt * 3.0).min(1.0);
}

/// A combat tune playing: since when Corvo has been out of its cone and out of sight, and the
/// time to its next blow.
#[derive(Default)]
struct Playing {
    out: f32,
    unseen: f32,
    next: f32,
}

/// The musical Overseers' combat tunes: started as Corvo stands in one's cone, its blows every
/// period through the rats of a swarm in the cone, stopped a moment after he leaves it.
fn combat_tunes(
    time: Res<Time>,
    mut playing: Local<std::collections::HashMap<Entity, Playing>>,
    player: Query<&Transform, With<Player>>,
    npcs: Query<(Entity, &Npc, &Transform, &crate::npc::FromSpawner), Without<Player>>,
    (mut sfx, mut stop, mut kill): (MessageWriter<PostEvent>, MessageWriter<StopEvent>, MessageWriter<crate::swarm::KillRats>),
    vm: Option<Res<crate::kismet::Vm>>,
) {
    let Ok(pt) = player.single() else { return };
    let dt = time.delta_secs();
    let mut live = std::collections::HashSet::new();
    for (e, n, t, from) in &npcs {
        let Some(tu) = n.arms.tunes.as_ref() else { continue };
        let fighting = plays(n, from.0, vm.as_deref());
        let to = pt.translation - t.translation;
        let d = to.length();
        let off = n.forward().with_y(0.0).normalize_or_zero().angle_between(to.with_y(0.0).normalize_or_zero()).to_degrees();
        let in_cone = fighting && d <= tu.combat_range && off <= tu.combat_angle;
        if let Some(p) = playing.get_mut(&e) {
            p.out = if in_cone { 0.0 } else { p.out + dt };
            p.unseen = if n.sees_player { 0.0 } else { p.unseen + dt };
            if !fighting || p.out > tu.stop_out_of_range || p.unseen > tu.stop_unseen {
                playing.remove(&e);
                info!("{} stops the combat tune", n.name);
                stop.write(StopEvent(event_id(&tu.damage_sounds[0])));
                if !tu.damage_sounds[1].is_empty() {
                    sfx.write(PostEvent::named(&tu.damage_sounds[1], Some(t.translation)));
                }
                continue;
            }
        } else if in_cone && n.sees_player {
            playing.insert(e, Playing::default());
            info!("{} plays the combat tune at Corvo ({d:.1} m)", n.name);
            if !tu.damage_sounds[0].is_empty() {
                sfx.write(PostEvent::named(&tu.damage_sounds[0], Some(t.translation)));
            }
        }
        let Some(p) = playing.get_mut(&e) else { continue };
        live.insert(e);
        p.next -= dt;
        if p.next <= 0.0 {
            p.next = tu.period.max(0.05);
            // the blow down the cone: spheres along it, as wide as the cone there
            let fwd = n.forward().with_y(0.0).normalize_or_zero();
            let half = tu.combat_angle.to_radians().tan();
            let mut x = 1.5;
            while x <= tu.combat_range {
                kill.write(crate::swarm::KillRats { at: t.translation + fwd * x, radius: (x * half).max(1.0), by_player: false, source: t.translation + Vec3::Y });
                x += 2.0;
            }
        }
    }
    playing.retain(|e, _| live.contains(e));
}

/// The world swaying under the tune.
fn sway(tune: Res<Tune>, mut cam: Query<&mut Transform, With<PlayerCamera>>) {
    if tune.disorient < 0.01 {
        return;
    }
    let Ok(mut ct) = cam.single_mut() else { return };
    let t = tune.t;
    let k = tune.disorient;
    let roll = (t * 1.3).sin() * 0.09 * k;
    let pitch = (t * 0.9 + 1.0).sin() * 0.04 * k;
    ct.rotation = ct.rotation * Quat::from_rotation_z(roll) * Quat::from_rotation_x(pitch);
}
