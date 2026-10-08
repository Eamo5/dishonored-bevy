//! Usable objects (`DishonoredUsableObject`): lockers, chests, trunks, bins, hatches, levers,
//! valves... Their moving part plays the sequence of each stage it's used into (`m_Stages`:
//! "Open", then "Close" with its own prompt), in place of the bind pose cooked into the level;
//! the level scripts hear it used (`DisSeqEvent_Used`) and can set a stage themselves
//! (`DisSeqAct_ActivateUsable`, `m_TargetStageIndex`).

use crate::anim::Animator;
use crate::bindings::{hint, Act, Bindings};
use crate::interact::{InteractFocus, Interaction};
use crate::level::{GameAssets, InstanceCollider, LevelInfo, LevelInstance};
use crate::player::{Player, PlayerCamera};
use crate::world_light::{LitActor, WorldLighting};
use crate::GameState;
use bevy::camera::visibility::DynamicSkinnedMeshBounds;
use bevy::mesh::skinning::SkinnedMesh;
use bevy::mesh::MeshTag;
use bevy::prelude::*;
use bevy_rapier3d::prelude::ColliderDisabled;

pub struct UsablesPlugin;

impl Plugin for UsablesPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<UsableLog>()
            .add_systems(OnEnter(GameState::InGame), spawn_usables.after(crate::level::LevelSpawnSet))
            .add_systems(Update, (restore_usables, scripted_stages, usable_focus.after(crate::interact::FocusSet).before(crate::interact::use_focus), audiograph_cards).run_if(in_state(GameState::InGame)));
    }
}

/// Where the level's usables stand (for saves): the stage last entered, and whether they're
/// done with.
#[derive(Resource, Default)]
pub struct UsableLog {
    pub entered: std::collections::HashMap<u32, (usize, bool)>,
    /// locked (true) or unlocked since the level began
    pub locks: std::collections::HashMap<u32, bool>,
    /// a save was loaded: set them as it had them
    pub restore: bool,
}

/// How near Corvo uses one (m).
const REACH: f32 = 2.0;

#[derive(Component)]
pub struct UsableRig {
    index: u32,
    /// the stage the next use moves it into
    stage: usize,
    done: bool,
    /// its actor in the level scripts
    actor: Option<u32>,
    /// the middle of its moving part
    center: Vec3,
    /// locked (`m_bLocked`; a key on Corvo's ring opens it)
    locked: bool,
}

impl UsableRig {
    pub fn center(&self) -> Vec3 {
        self.center
    }
    /// Its actor in the level scripts.
    pub fn actor(&self) -> Option<u32> {
        self.actor
    }
    pub fn index(&self) -> u32 {
        self.index
    }
}

/// Does a level-script actor have a moving part of its own (its prompt is this module's)?
pub fn animated(level: &LevelInfo, usable: Option<u32>) -> bool {
    usable.and_then(|u| level.scene.usables.get(u as usize)).is_some_and(|u| u.npc_type.is_some() && u.instance.is_some())
}

#[allow(clippy::too_many_arguments)]
fn spawn_usables(
    mut commands: Commands,
    assets: Option<Res<GameAssets>>,
    level: Option<Res<LevelInfo>>,
    mut wl: Option<ResMut<WorldLighting>>,
    instances: Query<(Entity, &LevelInstance, &Transform, Option<&InstanceCollider>, Option<&Children>)>,
    meshes: Query<(), With<Mesh3d>>,
    mut log: ResMut<UsableLog>,
) {
    *log = UsableLog::default();
    let (Some(assets), Some(level)) = (assets, level) else { return };
    let scene = &level.scene;
    if scene.usables.is_empty() {
        return;
    }
    let by_index: std::collections::HashMap<u32, (Entity, Transform, Option<Entity>, Vec<Entity>)> =
        instances.iter().map(|(e, li, t, c, ch)| (li.index, (e, *t, c.map(|c| c.0), ch.map(|c| c.iter().collect()).unwrap_or_default()))).collect();
    let actor_of: std::collections::HashMap<u32, u32> = scene.kismet.actors.iter().enumerate().filter_map(|(i, a)| a.usable.map(|u| (u, i as u32))).collect();
    let mut n = 0;
    for (ui, u) in scene.usables.iter().enumerate() {
        let (Some(t), Some(inst)) = (u.npc_type, u.instance) else { continue };
        let Some(vis) = assets.npc_types.get(t as usize).and_then(|v| v.as_ref()) else { continue };
        let Some((e, tf, col, kids)) = by_index.get(&inst) else { continue };
        // its bind pose gives way to the animated part
        if meshes.get(*e).is_ok() {
            commands.entity(*e).remove::<Mesh3d>();
        }
        for k in kids {
            if meshes.get(*k).is_ok() {
                commands.entity(*k).insert(Visibility::Hidden);
            }
        }
        if let Some(c) = col {
            commands.entity(*c).try_insert(ColliderDisabled);
        }
        let center = scene
            .instances
            .get(inst as usize)
            .and_then(|i| scene.meshes.get(i.mesh as usize))
            .map(|m| tf.transform_point((Vec3::from(m.min) + Vec3::from(m.max)) * 0.5))
            .unwrap_or(tf.translation);
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
        ec.insert(UsableRig { index: ui as u32, stage: 0, done: false, actor: actor_of.get(&(ui as u32)).copied(), center, locked: u.locked });
        if let Some(lib) = vis.anims.clone() {
            ec.insert(Animator::new(lib, &vis.skeleton, joints));
        }
        n += 1;
    }
    if n > 0 {
        info!("{n} animated usable objects");
    }
}

/// Move a usable into a stage: its sequence plays (or, restoring, is already over), the next
/// use goes on from there.
fn enter(rig: &mut UsableRig, anim: Option<&mut Animator>, level: &LevelInfo, stage: usize, log: &mut UsableLog, at_end: bool) {
    let Some(u) = level.scene.usables.get(rig.index as usize) else { return };
    let Some(st) = u.stages.get(stage) else { return };
    if let Some(a) = anim {
        if let Some(clip) = a.lib.find(&st.anim) {
            a.restart(clip, false, 1.0, if at_end { 0.0 } else { 0.1 });
            if at_end {
                a.sounds = false;
                let d = a.lib.duration(clip);
                a.seek(d);
            } else {
                a.sounds = true;
            }
        }
    }
    rig.stage = (stage + 1) % u.stages.len().max(1);
    rig.done = st.once;
    log.entered.insert(rig.index, (stage, rig.done));
}

/// A loaded save's usables, as they were left.
fn restore_usables(mut log: ResMut<UsableLog>, level: Option<Res<LevelInfo>>, mut rigs: Query<(&mut UsableRig, Option<&mut Animator>)>) {
    if !log.restore {
        return;
    }
    log.restore = false;
    let Some(level) = level else { return };
    let saved = log.entered.clone();
    for (mut r, a) in &mut rigs {
        if let Some(&l) = log.locks.get(&r.index) {
            r.locked = l;
        }
        if let Some(&(stage, _)) = saved.get(&r.index) {
            enter(&mut r, a.map(|a| a.into_inner()), &level, stage, &mut log, true);
        }
    }
}

/// `DisSeqAct_ActivateUsable`: the scripts move it (into a given stage, else the next).
fn scripted_stages(vm: Option<ResMut<crate::kismet::Vm>>, level: Option<Res<LevelInfo>>, mut rigs: Query<(&mut UsableRig, Option<&mut Animator>)>, mut log: ResMut<UsableLog>) {
    let (Some(mut vm), Some(level)) = (vm, level) else { return };
    // the scripts lock and unlock them (`DisSeqAct_Lock`)
    for (actor, lock) in std::mem::take(&mut vm.usable_locks) {
        for (mut r, _) in &mut rigs {
            if r.actor == Some(actor) {
                r.locked = lock;
                log.locks.insert(r.index, lock);
            }
        }
    }
    if vm.usable_stages.is_empty() {
        return;
    }
    for (actor, stage) in std::mem::take(&mut vm.usable_stages) {
        for (mut r, a) in &mut rigs {
            if r.actor == Some(actor) {
                let s = stage.map(|s| s.max(0) as usize).unwrap_or(r.stage);
                enter(&mut r, a.map(|a| a.into_inner()), &level, s, &mut log, false);
            }
        }
    }
}

/// An audiograph player's card: it loops while its recording plays (`m_PlayingAnimName`), and
/// comes back out ("Stopped") when the recording ends or another takes its place.
fn audiograph_cards(level: Option<Res<LevelInfo>>, mut pb: ResMut<crate::audiograph::Playback>, mut rigs: Query<(&mut UsableRig, Option<&mut Animator>)>, mut log: ResMut<UsableLog>) {
    let Some(level) = level else { return };
    let ended = std::mem::take(&mut pb.ended);
    let playing = pb.rig();
    for (mut r, a) in &mut rigs {
        let Some(u) = level.scene.usables.get(r.index as usize) else { continue };
        if u.audiograph.is_empty() {
            continue;
        }
        // (stage 1 next: it's playing)
        if ended.contains(&r.index) && r.stage == 1 && playing != Some(r.index) {
            enter(&mut r, a.map(|a| a.into_inner()), &level, 1, &mut log, false);
            continue;
        }
        if playing == Some(r.index) {
            if let Some(mut a) = a {
                if a.finished() && !u.playing_anim.is_empty() {
                    if let Some(c) = a.lib.find(&u.playing_anim) {
                        a.restart(c, true, 1.0, 0.1);
                    }
                }
            }
        }
    }
}

/// [Use] at one: its stage's prompt, then into that stage.
#[allow(clippy::too_many_arguments)]
fn usable_focus(
    (keys, bind): (Res<ButtonInput<KeyCode>>, Res<Bindings>),
    level: Option<Res<LevelInfo>>,
    mut focus: ResMut<InteractFocus>,
    mut rigs: Query<(&mut UsableRig, Option<&mut Animator>, Option<&InheritedVisibility>)>,
    player: Query<&Player>,
    cam: Query<&GlobalTransform, With<PlayerCamera>>,
    (carry, possession, held): (Res<crate::carry::Carry>, Res<crate::possession::Possession>, Res<crate::props::Held>),
    mut used: MessageWriter<Interaction>,
    mut log: ResMut<UsableLog>,
    (mut stats, mut msgs, mut devices, mut sfx): (ResMut<crate::gameplay::PlayerStats>, ResMut<crate::gameplay::HudMessages>, ResMut<crate::security::Devices>, MessageWriter<crate::audio::PostEvent>),
    mut graphs: MessageWriter<crate::audiograph::PlayAudiograph>,
) {
    let Some(level) = level else { return };
    if focus.0 || carry.carrying() || possession.host.is_some() || held.0.is_some() || player.single().is_ok_and(|p| p.locked) {
        return;
    }
    let Ok(c) = cam.single() else { return };
    let (eye, fwd) = (c.translation(), c.forward().as_vec3());
    let near = rigs
        .iter_mut()
        .filter(|(r, _, v)| {
            let to = r.center - eye;
            // (not in a sublevel streamed out: a Hound Pits variant's)
            !r.done && v.is_none_or(|v| v.get()) && to.length() < REACH && to.normalize_or_zero().dot(fwd) > 0.8
        })
        .min_by(|a, b| a.0.center.distance(eye).total_cmp(&b.0.center.distance(eye)));
    let Some((mut r, a, _)) = near else { return };
    let Some(u) = level.scene.usables.get(r.index as usize) else { return };
    let Some(st) = u.stages.get(r.stage) else { return };
    let raw = if !st.text.is_empty() { st.text.clone() } else if !u.text.is_empty() { u.text.clone() } else { format!("`GBA_Use` {}", if st.name.is_empty() { "Use" } else { &st.name }) };
    // locked: a key on the ring opens it
    let key = r.locked.then(|| stats.keys.iter().find(|k| u.keys.iter().any(|m| m.eq_ignore_ascii_case(k))).cloned()).flatten();
    focus.0 = true;
    focus.1 = None;
    focus.2 = if !r.locked {
        crate::gamedata::readable(&raw.replace("`GBA_Use`", &hint(Act::Use)))
    } else if key.is_some() {
        format!("{} Unlock", hint(Act::Use))
    } else {
        "Locked".to_string()
    };
    if !keys.just_pressed(bind.key(Act::Use)) {
        return;
    }
    if r.locked {
        match key {
            Some(k) => {
                r.locked = false;
                log.locks.insert(r.index, false);
                msgs.push(format!("Unlocked: {k}"));
                if let Some(actor) = r.actor {
                    used.write(Interaction::UsableLock { actor, locked: false });
                }
            }
            None => msgs.push("Locked"),
        }
        return;
    }
    // a security device's circuitry: rewired with a Rewire Tool
    if u.label.contains("Circuitry") {
        let tools = stats.items.get("RewireTool_twk").copied().unwrap_or(0);
        if tools == 0 {
            msgs.push("A Rewire Tool is needed");
            return;
        }
        stats.items.insert("RewireTool_twk".into(), tools - 1);
        if devices.rewire_near(&u.label, r.center) {
            msgs.push("Rewired: it now protects you");
        }
        sfx.write(crate::audio::PostEvent::named("UI_Validation", Some(r.center)));
    }
    let stage = r.stage;
    enter(&mut r, a.map(|a| a.into_inner()), &level, stage, &mut log, false);
    if let Some(actor) = r.actor {
        used.write(Interaction::Usable(actor));
    }
    // an audiograph player: "Play" plays its card, "Stopped" stops it
    if !u.audiograph.is_empty() {
        let play = stage == 0;
        graphs.write(crate::audiograph::PlayAudiograph {
            key: play.then(|| u.audiograph.clone()),
            at: Some(r.center),
            rig: Some(r.index),
            flavor: u.flavor.clone(),
        });
    }
}
