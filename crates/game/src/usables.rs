//! Usable objects (`DishonoredUsableObject`): lockers, chests, trunks, bins, hatches, levers,
//! valves... Their moving part plays the sequence of each stage it's used into (`m_Stages`:
//! "Open", then "Close" with its own prompt), in place of the bind pose cooked into the level;
//! the level scripts hear it used (`DisSeqEvent_Used`, the stage's own output) and can set a
//! stage themselves (`DisSeqAct_ActivateUsable`, `m_TargetStageIndex`). A valve's or a wheel's
//! stage is held to use (`m_bHoldToUse`): see `hold_usables`.

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
            .init_resource::<HoldGauge>()
            .add_systems(OnEnter(GameState::InGame), spawn_usables.after(crate::level::LevelSpawnSet))
            .add_systems(
                Update,
                (restore_usables, scripted_stages, (usable_focus.after(crate::interact::FocusSet).before(crate::interact::use_focus), hold_usables).chain(), audiograph_cards).run_if(in_state(GameState::InGame)),
            );
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

/// A hold under way's progress, for the HUD's interaction gauge (0..1).
#[derive(Resource, Default)]
pub struct HoldGauge(pub Option<f32>);

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
    /// a hold stage's progress (`m_fHeldTransitionProgress`), Corvo's hand on it
    /// (`m_bIsBeingUsed`), running back once let go, and at its end without completing
    progress: f32,
    holding: bool,
    rewinding: bool,
    at_end: bool,
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
        ec.insert(UsableRig { index: ui as u32, stage: 0, done: false, actor: actor_of.get(&(ui as u32)).copied(), center, locked: u.locked, progress: 0.0, holding: false, rewinding: false, at_end: false });
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
    bind: Res<Bindings>,
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
    data: Res<crate::gamedata::Data>,
) {
    let Some(level) = level else { return };
    // (one held: its hand stays on it, `hold_usables`)
    if rigs.iter().any(|(r, _, _)| r.holding) {
        focus.0 = true;
        return;
    }
    if focus.0 || carry.carrying() || possession.host.is_some() || held.0.is_some() || player.single().is_ok_and(|p| p.locked) {
        return;
    }
    let Ok(c) = cam.single() else { return };
    let (eye, fwd) = (c.translation(), c.forward().as_vec3());
    let seen = |r: &UsableRig, v: Option<&InheritedVisibility>| {
        let to = r.center - eye;
        // (not in a sublevel streamed out: a Hound Pits variant's)
        !r.done && v.is_none_or(|v| v.get()) && to.length() < REACH && to.normalize_or_zero().dot(fwd) > 0.8
    };
    // (an open box whose next move is to close, with something else in it: its doors are out of
    // the way, and what it holds is what's looked at: a security panel's circuitry)
    let centers: Vec<Vec3> = rigs.iter().filter(|(r, _, v)| seen(r, v.as_deref())).map(|(r, _, _)| r.center).collect();
    let open_box = |r: &UsableRig| {
        level.scene.usables.get(r.index as usize).and_then(|u| u.stages.get(r.stage)).is_some_and(|st| st.text.contains("Close"))
            && centers.iter().any(|c| c.distance(r.center) > 1e-3 && c.distance(r.center) < 0.5)
    };
    let near = rigs
        .iter_mut()
        .filter(|(r, _, v)| seen(r, v.as_deref()) && !open_box(r))
        .min_by(|a, b| a.0.center.distance(eye).total_cmp(&b.0.center.distance(eye)));
    let Some((mut r, a, _)) = near else { return };
    let Some(u) = level.scene.usables.get(r.index as usize) else { return };
    let Some(st) = u.stages.get(r.stage) else { return };
    let raw = if !st.text.is_empty() { st.text.clone() } else if !u.text.is_empty() { u.text.clone() } else { format!("`GBA_Use` {}", if st.name.is_empty() { "Use" } else { &st.name }) };
    // locked: a key on the ring opens it
    let key = r.locked.then(|| stats.keys.iter().find(|k| u.keys.iter().any(|m| m.eq_ignore_ascii_case(k))).cloned()).flatten();
    focus.0 = true;
    focus.1 = None;
    // (its name above: `m_Name`, "Wall of Light Circuitry")
    focus.3 = u.label.clone();
    // locked: its own words (`m_UnlockInteractText`; its `m_LockedMessage`: "This locker is
    // locked", "`k needed" with the key it wants, plain "Locked" when none is named)
    let say = |k: &str, or: &str| {
        let t = u.texts.get(k).map(String::as_str).unwrap_or(or);
        match u.keys.first() {
            Some(key) => t.replace("`k", key),
            None if t.contains("`k") => "Locked".to_string(),
            None => t.to_string(),
        }
    };
    focus.2 = if !r.locked {
        let line = crate::gamedata::readable(&raw.replace("`GBA_Use`", &hint(Act::Use)));
        // (and its own line under it: the circuitry's "Rewire Tools Remaining: `q", the tools
        // he has)
        match u.texts.get("focus") {
            Some(f) => format!("{line}\n{}", crate::gamedata::readable(&f.replace("`q", &stats.items.get("RewireTool_twk").copied().unwrap_or(0).to_string()))),
            None => line,
        }
    } else if key.is_some() {
        crate::gamedata::readable(&say("unlock", "`GBA_Use` Unlock").replace("`GBA_Use`", &hint(Act::Use)))
    } else {
        crate::gamedata::readable(&say(if u.texts.contains_key("focus_locked") { "focus_locked" } else { "locked" }, "Locked"))
    };
    if !bind.just_pressed(Act::Use) {
        return;
    }
    if r.locked {
        match key {
            Some(_) => {
                r.locked = false;
                log.locks.insert(r.index, false);
                if let Some(actor) = r.actor {
                    used.write(Interaction::UsableLock { actor, locked: false });
                }
            }
            // (its line is already up; the scripts hear it tried: "Used But Locked")
            None => {
                if let Some(actor) = r.actor {
                    used.write(Interaction::UsableOut { actor, out: u.stages.len() });
                }
            }
        }
        return;
    }
    // a security device's circuitry: rewired with a Rewire Tool
    if u.label.contains("Circuitry") {
        let tools = stats.items.get("RewireTool_twk").copied().unwrap_or(0);
        if tools == 0 {
            msgs.push(crate::security::panel_text(&data, "Generic", "m_CannotUseMessage"));
            return;
        }
        stats.items.insert("RewireTool_twk".into(), tools - 1);
        if let Some(group) = devices.rewire_near(&u.label, r.center) {
            msgs.push(crate::security::panel_text(&data, group, "m_UseMessage"));
        }
        sfx.write(crate::audio::PostEvent::named("UI_Validation", Some(r.center)));
    }
    let stage = r.stage;
    // held to use: Corvo's hand on it ("Started Holding", on every press: `hold_usables` takes
    // it on), its loop from the start or resumed, a rewind under way stopped (0x00A5BC30)
    if let Some(h) = &st.hold {
        let remote_at = remote_at(&level, u, r.center);
        let mut post = |i: usize, at: Vec3| {
            if let Some(s) = h.sounds.get(i).filter(|s| !s.is_empty()) {
                sfx.write(crate::audio::PostEvent::named(s, Some(at)));
            }
        };
        if r.rewinding {
            post(REVERSE_STOP, r.center);
            post(REVERSE_STOP + REMOTE, remote_at);
        }
        let start = if r.progress.abs() < 1e-4 { FORWARD_START } else { FORWARD_RESUME };
        post(start, r.center);
        post(start + REMOTE, remote_at);
        r.holding = true;
        r.rewinding = false;
        r.at_end = false;
        if let Some(mut a) = a {
            scrub(&mut a, &st.anim, r.progress);
        }
        if let Some(actor) = r.actor {
            used.write(Interaction::UsableOut { actor, out: u.stages.len() + 1 });
        }
        return;
    }
    enter(&mut r, a.map(|a| a.into_inner()), &level, stage, &mut log, false);
    if let Some(actor) = r.actor {
        used.write(Interaction::Usable(actor, stage));
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

/// A hold stage's sounds (`HoldUse::sounds`), the driven actor's seven after its own.
const FORWARD_START: usize = 0;
const FORWARD_STOP: usize = 1;
const FORWARD_RESUME: usize = 2;
const FORWARD_COMPLETE: usize = 3;
const REVERSE_START: usize = 4;
const REVERSE_STOP: usize = 5;
const REVERSE_COMPLETE: usize = 6;
const REMOTE: usize = 7;

/// Where a usable's driven actor stands (else the usable).
fn remote_at(level: &LevelInfo, u: &dhcook::format::UsableObj, or: Vec3) -> Vec3 {
    u.remote.and_then(|a| level.scene.kismet.actors.get(a as usize)).map(|a| Vec3::from(a.position)).unwrap_or(or)
}

/// A sequence held at a share of its length (a hold's: set, not played).
fn scrub(a: &mut Animator, clip: &str, at: f32) {
    let Some(c) = a.lib.find(clip) else { return };
    if a.current().map(|p| p.clip) != Some(c) {
        a.restart(c, false, 0.0, 0.1);
        a.sounds = false;
    }
    a.seek(at.clamp(0.0, 1.0) * a.lib.duration(c));
}

/// Holding [Use] on a hold stage (`m_HoldToUseData`, 0x00A574E0, decoded for this rewrite): its
/// progress rises over its use time while held, its sequence and the driven actor's set to it
/// (the HUD's interaction gauge showing it). Let go, out of reach or looking away: "Stopped
/// Holding", and it runs back over its rewind time where it rewinds, else stays. At the end:
/// the stage's output, then "Stopped Holding", and on to the next stage; one that never
/// completes waits there until let go.
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
fn hold_usables(
    time: Res<Time>,
    bind: Res<Bindings>,
    level: Option<Res<LevelInfo>>,
    mut rigs: Query<(&mut UsableRig, Option<&mut Animator>), Without<crate::propanim::PropRig>>,
    mut remotes: Query<(&crate::propanim::PropRig, &mut Animator), Without<UsableRig>>,
    player: Query<&Player>,
    cam: Query<&GlobalTransform, With<PlayerCamera>>,
    (carry, possession): (Res<crate::carry::Carry>, Res<crate::possession::Possession>),
    mut used: MessageWriter<Interaction>,
    mut sfx: MessageWriter<crate::audio::PostEvent>,
    mut log: ResMut<UsableLog>,
    mut gauge: ResMut<HoldGauge>,
) {
    gauge.0 = None;
    let Some(level) = level else { return };
    let dt = time.delta_secs();
    let view = cam.single().ok().map(|c| (c.translation(), c.forward().as_vec3()));
    let free = !carry.carrying() && possession.host.is_none() && !player.single().is_ok_and(|p| p.locked);
    // (+1 over its time a second; at once where it has none)
    let rate = |t: f32| if t > 0.0 { 1.0 / t } else { 1.0 };
    for (mut r, a) in &mut rigs {
        if !r.holding && !r.rewinding {
            continue;
        }
        let Some(u) = level.scene.usables.get(r.index as usize) else { continue };
        let Some(st) = u.stages.get(r.stage) else { continue };
        let Some(h) = &st.hold else {
            r.holding = false;
            r.rewinding = false;
            continue;
        };
        let n = u.stages.len();
        let remote_at = remote_at(&level, u, r.center);
        let mut post = |i: usize, at: Vec3| {
            if let Some(s) = h.sounds.get(i).filter(|s| !s.is_empty()) {
                sfx.write(crate::audio::PostEvent::named(s, Some(at)));
            }
        };
        let mut shown = r.progress;
        if r.holding {
            // (still held: the button down, it before him and in reach)
            let held = free
                && bind.pressed(Act::Use)
                && view.is_some_and(|(eye, fwd)| {
                    let to = r.center - eye;
                    to.length() < REACH * 1.2 && to.normalize_or_zero().dot(fwd) > 0.7
                });
            if held {
                r.progress = (r.progress + dt * rate(h.use_time)).min(1.0);
                shown = r.progress;
                gauge.0 = Some(r.progress);
                if r.progress >= 1.0 && !r.at_end {
                    post(FORWARD_STOP, r.center);
                    post(FORWARD_STOP + REMOTE, remote_at);
                    if h.never_complete {
                        r.at_end = true;
                    } else {
                        // done: the stage's output, then "Stopped Holding" (the button still
                        // down), and on to the next stage, the progress back to nothing
                        post(FORWARD_COMPLETE, r.center);
                        post(FORWARD_COMPLETE + REMOTE, remote_at);
                        let stage = r.stage;
                        if let Some(actor) = r.actor {
                            used.write(Interaction::Usable(actor, stage));
                            used.write(Interaction::UsableOut { actor, out: n + 2 });
                        }
                        r.holding = false;
                        r.progress = 0.0;
                        r.stage = (stage + 1) % n.max(1);
                        r.done = st.once;
                        log.entered.insert(r.index, (stage, r.done));
                        gauge.0 = None;
                    }
                }
            } else {
                // let go: "Stopped Holding", its loop stopped; it runs back, or stays
                if let Some(actor) = r.actor {
                    used.write(Interaction::UsableOut { actor, out: n + 2 });
                }
                if !r.at_end {
                    post(FORWARD_STOP, r.center);
                    post(FORWARD_STOP + REMOTE, remote_at);
                }
                r.holding = false;
                r.at_end = false;
                if h.auto_rewind && r.progress > 0.0 {
                    r.rewinding = true;
                    post(REVERSE_START, r.center);
                    post(REVERSE_START + REMOTE, remote_at);
                }
            }
        } else {
            r.progress = (r.progress - dt * rate(h.rewind_time)).max(0.0);
            shown = r.progress;
            if r.progress <= 0.0 {
                r.rewinding = false;
                for i in [REVERSE_STOP, REVERSE_COMPLETE] {
                    post(i, r.center);
                    post(i + REMOTE, remote_at);
                }
            }
        }
        // its sequence and the driven actor's, set to the progress
        if let Some(mut a) = a {
            scrub(&mut a, &st.anim, shown);
        }
        if let (Some(ra), false) = (u.remote, h.remote_anim.is_empty()) {
            for (pr, mut a) in &mut remotes {
                if pr.actor == ra {
                    scrub(&mut a, &h.remote_anim, shown);
                }
            }
        }
    }
}
