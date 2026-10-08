//! Scripted input for automated gameplay tests.
//!
//! `DH_SCRIPT="wait 2; tp 12 11 24 0 0; attack; wait 0.5; shot hit; log; exit"`
//!
//! Commands: `wait S`, `tp X Y Z YAW PITCH` (physics), `fly X Y Z YAW PITCH` (noclip),
//! `goto X Y Z [S]` (walk there over the navmesh), `gototask [N] [S]` (to the N-th objective
//! marker), `look YAW PITCH`, `tpnpc [D] [NAME|-] [PITCH]` (in front of the nearest hostile NPC), `krust [D] [N]` (hovering in front of the nearest river krust, or the N-th), `possess fish|krust|rat` (the nearest, at once), `prop [NAME]` (standing before the nearest loose prop), `explode [D]` (a grenade blast ahead), `flare [D] [BRIGHTNESS]` (a steady gameplay light ahead), `select POWER` (hold a power or item), `weapons` (the pistol and crossbow), `walk S`, `attack`, `clash` (a sword lock with the nearest guard), `block S`, `key K` (tap: E Z X F C Space 1-6),
//! `hold K S`, `rmb`, `rmbhold S`, `shot NAME`, `log`, `god`, `health N`, `npcidle` (the nearest
//! hostile character stands where it is, facing the player), `exit`.
//! Inputs are injected into Bevy's `ButtonInput` resources so gameplay code is untouched.

use crate::gameplay::PlayerStats;
use crate::npc::Npc;
use crate::player::Player;
use crate::GameState;
use bevy::prelude::*;
use bevy::render::view::screenshot::{save_to_disk, Screenshot};

pub struct ScriptPlugin;

impl Plugin for ScriptPlugin {
    fn build(&self, app: &mut App) {
        if let Ok(s) = std::env::var("DH_SCRIPT") {
            let cmds: Vec<Vec<String>> = s
                .split(';')
                .map(|c| c.split_whitespace().map(|x| x.to_string()).collect::<Vec<_>>())
                .filter(|c| !c.is_empty())
                .collect();
            app.insert_resource(Scripted);
            app.insert_resource(Script { cmds, idx: 0, timer: 0.0, started: false, held: Vec::new(), dir: std::env::var("DH_SHOT_DIR").unwrap_or_else(|_| "cache/shots".into()), god: false });
            app.init_resource::<Goto>();
            app.init_resource::<CamAt>();
            app.init_resource::<CamPos>();
            app.add_systems(PreUpdate, (script_goto, run_script).chain().after(bevy::input::InputSystems).run_if(in_state(GameState::InGame)));
            app.add_systems(PostUpdate, cam_at.after(bevy::transform::TransformSystems::Propagate).run_if(in_state(GameState::InGame)));
        }
    }
}

/// `camat NAME [D] [SIDE]`: the view held D metres in front of a character's face (rides and
/// scenes notwithstanding), looking at it; `camat -` lets go.
#[derive(Resource, Default)]
struct CamAt(Option<(String, f32, f32)>);

/// `campos X Y Z YAW PITCH`: the view held at a point (rides and scenes notwithstanding);
/// `campos -` lets go.
#[derive(Resource, Default)]
struct CamPos(Option<Transform>);

fn cam_at(
    (cam_at, cam_pos): (Res<CamAt>, Res<CamPos>),
    npcs: Query<(&crate::npc::Npc, &crate::npc::NpcRig, &GlobalTransform, Has<crate::facefx::FaceAnim>)>,
    joints: Query<&GlobalTransform, (Without<crate::npc::Npc>, Without<crate::player::PlayerCamera>)>,
    mut cams: Query<&mut GlobalTransform, (With<crate::player::PlayerCamera>, Without<crate::npc::Npc>)>,
    player: Query<&Transform, With<crate::player::Player>>,
) {
    if let Some(t) = cam_pos.0 {
        for mut g in &mut cams {
            *g = GlobalTransform::from(t);
        }
        return;
    }
    let Some((name, d, side)) = cam_at.0.as_ref() else { return };
    // (the one speaking, among those of that name)
    let Some((npc, rig, nt, _)) = npcs.iter().filter(|(n, _, _, _)| n.name.contains(name.as_str()) || n.pawn.contains(name.as_str())).max_by_key(|(_, _, _, speaking)| *speaking) else { return };
    let head = rig.joint("head_jnt").and_then(|j| joints.get(j).ok()).map(|g| g.translation()).unwrap_or(nt.translation() + Vec3::Y * 1.6);
    // (from where Corvo stands: whom they speak to)
    let fwd = player.single().ok().map(|p| (p.translation + Vec3::Y * 0.7 - head).with_y(0.0).normalize_or_zero()).filter(|v| *v != Vec3::ZERO).unwrap_or(npc.forward());
    let eye = head + (Quat::from_rotation_y(*side) * fwd) * *d + Vec3::Y * 0.03;
    let t = Transform::from_translation(eye).looking_at(head + Vec3::Y * 0.02, Vec3::Y);
    for mut g in &mut cams {
        *g = GlobalTransform::from(t);
    }
}

/// `goto X Y Z [secs]`: walk there over the level's navmesh (the script waits until there, or
/// the time is up).
#[derive(Resource, Default)]
struct Goto {
    goal: Option<Vec3>,
    left: f32,
    path: Vec<Vec3>,
    repath: f32,
    /// where it last made headway, how long since, and what it tried
    last: Vec3,
    stuck: f32,
    tries: u32,
    /// facing something to use (yaw, pitch) for a moment
    face: Option<(f32, f32, f32)>,
    /// (`gototask`) take or use what is at the goal on arrival; the press pending
    take: bool,
    take_pickup: Option<u32>,
    take_actor: Option<u32>,
    use_in: Option<f32>,
}

fn script_goto(
    time: Res<Time<Real>>,
    mut goto: ResMut<Goto>,
    mut script: ResMut<Script>,
    grid: Option<Res<crate::navmesh::NavGrid>>,
    mut player: Query<(&mut Player, &Transform)>,
    doors: Query<&GlobalTransform, With<crate::interact::Door>>,
    things: Query<(&Transform, Option<&crate::interact::Pickup>, Option<&crate::interact::Usable>), Or<(With<crate::interact::Pickup>, With<crate::interact::Usable>)>>,
    rigs: Query<&crate::usables::UsableRig>,
) {
    // arrived where something is to be taken or used: facing it, then [F]
    if let Some(left) = goto.use_in {
        let Ok((mut p, _)) = player.single_mut() else { return };
        if let Some((yaw, pitch, _)) = goto.face {
            p.yaw = yaw;
            p.pitch = pitch;
        }
        let left = left - time.delta_secs();
        if left <= 0.0 {
            script.held.push((Input::Key(KeyCode::KeyF), 0.05));
            goto.use_in = None;
            goto.face = None;
        } else {
            goto.use_in = Some(left);
        }
        return;
    }
    let Some(goal) = goto.goal else { return };
    let Ok((mut p, t)) = player.single_mut() else { return };
    let dt = time.delta_secs();
    goto.left -= dt;
    let feet = t.translation - Vec3::Y * (crate::player::STAND_HALF + crate::player::RADIUS);
    let there = feet.with_y(0.0).distance(goal.with_y(0.0)) < 0.6 && (feet.y - goal.y).abs() < 2.0;
    if there || goto.left <= 0.0 {
        info!("script: goto {goal} {} at {feet}", if there { "arrived" } else { "gave up" });
        goto.goal = None;
        script.timer = 0.0;
        script.held.retain(|(i, _)| *i != Input::Key(KeyCode::KeyW));
        // (a task's thing: taken or used)
        if std::mem::take(&mut goto.take) {
            let eye = t.translation + Vec3::Y * crate::player::STAND_EYE;
            // (the task's own pickup, else what is nearest)
            let (want, actor) = (goto.take_pickup.take(), goto.take_actor.take());
            let own = want
                .and_then(|i| things.iter().find(|(_, p, _)| p.is_some_and(|p| p.index == i)).map(|(x, _, _)| x.translation))
                .or_else(|| actor.and_then(|a| rigs.iter().find(|r| r.actor() == Some(a)).map(|r| r.center())))
                .or_else(|| actor.and_then(|a| things.iter().find(|(_, p, u)| p.is_none() && u.is_some_and(|u| u.actor == a)).map(|(x, _, _)| x.translation)))
                .filter(|x| x.distance(eye) < 2.5);
            if let Some(at) = own.or_else(|| things.iter().map(|(x, _, _)| x.translation).filter(|x| x.distance(goal) < 2.5).min_by(|a, b| a.distance(goal).total_cmp(&b.distance(goal)))) {
                let to = at - eye;
                goto.face = Some(((-to.x).atan2(-to.z), (to.y / to.length().max(1e-3)).asin(), 1.0));
                goto.use_in = Some(0.3);
                script.timer = 1.0;
                info!("script: goto takes what is at {at:.2}");
            }
        }
        return;
    }
    goto.repath -= dt;
    if goto.path.is_empty() || goto.repath <= 0.0 {
        goto.repath = 1.0;
        goto.path = grid.as_ref().and_then(|g| g.path(feet, goal)).unwrap_or_else(|| vec![goal]);
        if std::env::var("DH_GOTO_LOG").is_ok() {
            info!("script: goto from {feet}: {:?}", goto.path.iter().take(6).collect::<Vec<_>>());
        }
    }
    while goto.path.len() > 1 && feet.with_y(0.0).distance(goto.path[0].with_y(0.0)) < 0.5 {
        goto.path.remove(0);
    }
    let d = (goto.path[0] - feet).with_y(0.0);
    if let Some((yaw, pitch, left)) = goto.face {
        p.yaw = yaw;
        p.pitch = pitch;
        goto.face = (left > dt).then_some((yaw, pitch, left - dt));
    } else if d.length() > 0.05 {
        p.yaw = (-d.x).atan2(-d.z);
    }
    // held up (a shut door, boards across the way): use what's ahead, then swing at it
    if feet.distance(goto.last) > 0.3 {
        goto.last = feet;
        goto.stuck = 0.0;
    } else {
        goto.stuck += dt;
    }
    if goto.stuck > 1.5 {
        goto.stuck = 0.0;
        goto.tries += 1;
        // (a ledge above: jump and climb onto it)
        let what = match goto.tries % 5 {
            1 => {
                // (facing the nearest door, if one is at hand)
                let eye = t.translation + Vec3::Y * crate::player::STAND_EYE;
                if let Some(at) = doors.iter().map(|g| g.translation() + Vec3::Y * 1.0).filter(|d| d.distance(eye) < 3.0).min_by(|a, b| a.distance(eye).total_cmp(&b.distance(eye))) {
                    let to = at - eye;
                    p.yaw = (-to.x).atan2(-to.z);
                    p.pitch = (to.y / to.length().max(1e-3)).asin();
                    goto.face = Some((p.yaw, p.pitch, 0.6));
                    script.held.push((Input::Key(KeyCode::KeyF), 0.05));
                    info!("script: goto held up at {feet:.2}: use (door at {at:.2})");
                    return;
                }
                script.held.push((Input::Key(KeyCode::KeyF), 0.05));
                "use"
            }
            2 => {
                script.held.push((Input::Mouse(MouseButton::Left), 0.05));
                "attack"
            }
            // (something in the way, a door swung across a corridor: step around it)
            3 => {
                script.held.push((Input::Key(KeyCode::KeyA), 0.9));
                "step left"
            }
            4 => {
                script.held.push((Input::Key(KeyCode::KeyD), 1.8));
                "step right"
            }
            _ => {
                // (held: a ledge out of reach from the ground is caught in the air)
                script.held.push((Input::Key(KeyCode::Space), 0.7));
                "jump"
            }
        };
        p.pitch = -0.2;
        info!("script: goto held up at {feet:.2}: {what}");
    }
    match script.held.iter_mut().find(|(i, _)| *i == Input::Key(KeyCode::KeyW)) {
        Some(h) => h.1 = 0.2,
        None => script.held.push((Input::Key(KeyCode::KeyW), 0.2)),
    }
}

/// Present while a test script drives the game: gameplay treats input as focused without
/// grabbing the OS cursor, and real mouse/keyboard input is ignored.
#[derive(Resource)]
pub struct Scripted;

#[derive(Resource)]
struct Script {
    cmds: Vec<Vec<String>>,
    idx: usize,
    timer: f32,
    started: bool,
    /// (key or mouse, remaining seconds)
    held: Vec<(Input, f32)>,
    dir: String,
    god: bool,
}

#[derive(Clone, Copy, PartialEq)]
enum Input {
    Key(KeyCode),
    Mouse(MouseButton),
}

fn key_of(s: &str) -> Option<KeyCode> {
    Some(match s.to_ascii_lowercase().as_str() {
        "e" => KeyCode::KeyE,
        "alt" => KeyCode::AltLeft,
        "z" => KeyCode::KeyZ,
        "x" => KeyCode::KeyX,
        "f" => KeyCode::KeyF,
        "c" => KeyCode::KeyC,
        "w" => KeyCode::KeyW,
        "a" => KeyCode::KeyA,
        "s" => KeyCode::KeyS,
        "d" => KeyCode::KeyD,
        "q" => KeyCode::KeyQ,
        "r" => KeyCode::KeyR,
        "shift" => KeyCode::ShiftLeft,
        "ctrl" => KeyCode::ControlLeft,
        "space" => KeyCode::Space,
        "esc" | "escape" => KeyCode::Escape,
        "enter" => KeyCode::Enter,
        "up" => KeyCode::ArrowUp,
        "down" => KeyCode::ArrowDown,
        "left" => KeyCode::ArrowLeft,
        "right" => KeyCode::ArrowRight,
        "f5" => KeyCode::F5,
        "f9" => KeyCode::F9,
        "1" => KeyCode::Digit1,
        "2" => KeyCode::Digit2,
        "3" => KeyCode::Digit3,
        "4" => KeyCode::Digit4,
        "5" => KeyCode::Digit5,
        "6" => KeyCode::Digit6,
        "7" => KeyCode::Digit7,
        "8" => KeyCode::Digit8,
        "9" => KeyCode::Digit9,
        "0" => KeyCode::Digit0,
        "t" => KeyCode::KeyT,
        "j" => KeyCode::KeyJ,
        "tab" => KeyCode::Tab,
        _ => return None,
    })
}

#[allow(clippy::too_many_arguments)]
fn run_script(
    mut commands: Commands,
    time: Res<Time<Real>>,
    mut script: ResMut<Script>,
    mut keys: ResMut<ButtonInput<KeyCode>>,
    mut mouse: ResMut<ButtonInput<MouseButton>>,
    mut stats: ResMut<PlayerStats>,
    mut player: Query<(&mut Player, &mut Transform)>,
    npcs: Query<(&Npc, &Transform, Option<&crate::interact::Usable>), Without<Player>>,
    pickups: Query<&Transform, (With<crate::interact::Pickup>, Without<Player>, Without<Npc>)>,
    doors: Query<&GlobalTransform, With<crate::interact::Door>>,
    usables: Query<&Transform, (With<crate::interact::Usable>, Without<Player>, Without<Npc>, Without<crate::interact::Pickup>)>,
    (mut exit, mut fx, mut vm): (MessageWriter<AppExit>, MessageWriter<crate::particles::SpawnEffect>, Option<ResMut<crate::kismet::Vm>>),
    (diag, data, rapier, mut explosions): (Res<bevy::diagnostic::DiagnosticsStore>, Res<crate::gamedata::Data>, bevy_rapier3d::prelude::ReadRapierContext, MessageWriter<crate::gadgets::Explosion>),
    (mut powers, warming, swim, npc_ents, focus, pockets, krusts, strikeables, fish, (hosts, possession), (props, level, traps, devices, usable_rigs)): (
        ResMut<crate::powers::Powers>,
        Option<Res<crate::warmup::Warmup>>,
        Res<crate::swim::Swim>,
        Query<(Entity, &Npc, &Transform), Without<Player>>,
        Res<crate::interact::InteractFocus>,
        Query<(&crate::pickpocket::Pocket, &Transform), (Without<Player>, Without<Npc>)>,
        Query<(&crate::krust::Krust, &GlobalTransform)>,
        Query<(&crate::gameplay::Strikeable, &GlobalTransform)>,
        Query<(&crate::fish::Fish, &Transform), (Without<Player>, Without<Npc>)>,
        (Query<(Entity, &crate::possession::Host, &GlobalTransform)>, Res<crate::possession::Possession>),
        (Query<(&crate::props::Prop, &Transform), (Without<Player>, Without<Npc>)>, Option<Res<crate::level::LevelInfo>>, Query<(&crate::traps::TrapPart, &Transform), (Without<Player>, Without<Npc>)>, Res<crate::security::Devices>, Query<&crate::usables::UsableRig>),
    ),
) {
    let dt = time.delta_secs();
    // the level isn't shown yet
    if warming.is_some() {
        return;
    }
    if !script.started {
        // give the world a moment to settle (colliders, uploads)
        script.timer += dt;
        if script.timer < 1.5 {
            return;
        }
        script.started = true;
        script.timer = 0.0;
    }
    // drop real input so whoever is at the machine can't disturb the run
    let held_keys: Vec<KeyCode> = script.held.iter().filter_map(|(i, _)| if let Input::Key(k) = i { Some(*k) } else { None }).collect();
    let held_mouse: Vec<MouseButton> = script.held.iter().filter_map(|(i, _)| if let Input::Mouse(m) = i { Some(*m) } else { None }).collect();
    let real_keys: Vec<KeyCode> = keys.get_pressed().chain(keys.get_just_released()).copied().filter(|k| !held_keys.contains(k)).collect();
    for k in real_keys {
        keys.reset(k);
    }
    let real_mouse: Vec<MouseButton> = mouse.get_pressed().chain(mouse.get_just_released()).copied().filter(|m| !held_mouse.contains(m)).collect();
    for m in real_mouse {
        mouse.reset(m);
    }
    if script.god && stats.game_over.is_none() {
        stats.health = stats.max_health;
        stats.dead = false;
    }
    // held inputs
    let mut still = Vec::new();
    for (inp, t) in std::mem::take(&mut script.held) {
        let t = t - dt;
        if t > 0.0 {
            match inp {
                Input::Key(k) => keys.press(k),
                Input::Mouse(m) => mouse.press(m),
            }
            still.push((inp, t));
        } else {
            match inp {
                Input::Key(k) => keys.release(k),
                Input::Mouse(m) => mouse.release(m),
            }
        }
    }
    script.held = still;
    if script.timer > 0.0 {
        script.timer -= dt;
        return;
    }
    let Some(cmd) = script.cmds.get(script.idx).cloned() else { return };
    script.idx += 1;
    let f = |i: usize| cmd.get(i).and_then(|v| v.parse::<f32>().ok()).unwrap_or(0.0);
    let Ok((mut p, mut t)) = player.single_mut() else { return };
    match cmd[0].as_str() {
        "wait" => script.timer = f(1),
        "goto" => {
            let goal = Vec3::new(f(1), f(2), f(3));
            let secs = if cmd.len() > 4 { f(4) } else { 60.0 };
            commands.queue(move |w: &mut World| {
                let mut g = w.resource_mut::<Goto>();
                g.goal = Some(goal);
                g.left = secs;
                g.path.clear();
            });
            script.timer = secs + 1.0;
        }
        "save" | "load" => {
            // save N / load N: a save slot (resume a long test from where it got to)
            let slot = f(1) as usize;
            let save = cmd[0] == "save";
            commands.queue(move |w: &mut World| {
                if save {
                    w.write_message(crate::save::SaveRequest(slot));
                } else {
                    w.write_message(crate::save::LoadRequest(slot));
                }
            });
            script.timer = if save { 1.0 } else { 3.0 };
        }
        "campos" => {
            let t = (cmd.get(1).map(|v| v.as_str()) != Some("-")).then(|| Transform::from_translation(Vec3::new(f(1), f(2), f(3))).with_rotation(Quat::from_euler(EulerRot::YXZ, f(4), f(5), 0.0)));
            commands.queue(move |w: &mut World| {
                w.resource_mut::<CamPos>().0 = t;
            });
        }
        "camat" => {
            let target = cmd.get(1).cloned().filter(|n| n != "-");
            let d = if cmd.len() > 2 { f(2) } else { 0.6 };
            let side = f(3);
            commands.queue(move |w: &mut World| {
                w.resource_mut::<CamAt>().0 = target.map(|n| (n, d, side));
            });
        }
        "gototask" => {
            // gototask [N] [SECS]: walk to the N-th objective marker
            let pick = f(1) as usize;
            let secs = if cmd.len() > 2 { f(2) } else { 90.0 };
            commands.queue(move |w: &mut World| {
                let npcs: Vec<(u32, Vec3)> = w.query::<(&crate::npc::FromSpawner, &Transform, &crate::npc::Npc)>().iter(w).filter(|(_, _, n)| !n.is_down()).map(|(f, t, _)| (f.0, t.translation)).collect();
                let items: Vec<(u32, Vec3)> = w.query::<(&crate::interact::Pickup, &Transform)>().iter(w).map(|(p, t)| (p.index, t.translation)).collect();
                let pts = w
                    .get_resource::<crate::kismet::Vm>()
                    .map(|vm| crate::markers::task_targets(vm, |s| npcs.iter().find(|(f, _)| *f == s).map(|(_, p)| *p), |i| items.iter().find(|(k, _)| *k == i).map(|(_, p)| *p)))
                    .unwrap_or_default();
                match pts.get(pick) {
                    Some(&(p, pickup, actor, _)) => {
                        info!("script: goto task marker {pick} at {p:.2}");
                        let mut g = w.resource_mut::<Goto>();
                        g.goal = Some(p - Vec3::Y * 0.5);
                        g.left = secs;
                        g.path.clear();
                        g.take = true;
                        g.take_pickup = pickup;
                        g.take_actor = Some(actor);
                    }
                    None => {
                        info!("script: no task marker {pick} (of {})", pts.len());
                        w.resource_mut::<Script>().timer = 0.0;
                    }
                }
            });
            script.timer = secs + 1.0;
        }
        "tp" | "fly" => {
            t.translation = Vec3::new(f(1), f(2), f(3));
            p.yaw = f(4);
            p.pitch = f(5);
            p.velocity = Vec3::ZERO;
            p.noclip = cmd[0] == "fly";
            info!("script: {} to {}", cmd[0], t.translation);
        }
        "givepower" => {
            // givepower <Blink|DarkVision|...|Vitality|...> [level]
            if let Some(name) = cmd.get(1) {
                let lvl = cmd.get(2).and_then(|v| v.parse().ok()).unwrap_or(1u8);
                stats.powers.insert(name.clone(), lvl);
                info!("script: power {name} level {lvl}");
            }
        }
        "giveitem" => {
            // giveitem KEY: an abstract item, as the scripts give one (`HereticBrand_AbsItm`)
            if let Some(k) = cmd.get(1) {
                if !stats.notes.contains(k) {
                    stats.notes.push(k.clone());
                }
                *stats.items.entry(k.clone()).or_default() += 1;
                info!("script: item {k}");
            }
        }
        "giveupgrade" => {
            // giveupgrade <Twk_Upgrade_*>: an upgrade bought from Piero (the mask's optics...)
            if let Some(name) = cmd.get(1) {
                if !stats.upgrades.contains(name) {
                    stats.upgrades.push(name.clone());
                }
                info!("script: upgrade {name}");
            }
        }
        "weapons" => {
            // weapons: Corvo's sword, pistol and crossbow (a map started on its own may lack them)
            stats.weapons = true;
        }
        "charms" => {
            // charms <n>: find n bone charms (the first of the list), wearing the first three
            let n: usize = cmd.get(1).and_then(|v| v.parse().ok()).unwrap_or(4);
            let names: Vec<String> = data.0.charms.iter().filter_map(|c| c.levels.first()).map(|l| l.0.clone()).take(n).collect();
            for (i, name) in names.into_iter().enumerate() {
                if i < 3 {
                    stats.charms.push(name.clone());
                }
                stats.charms_owned.push(name);
            }
        }
        "chaos" => {
            // chaos N: the campaign's chaos level
            let n: i32 = cmd.get(1).and_then(|v| v.parse().ok()).unwrap_or(0);
            stats.chaos_level = n;
            if let Some(vm) = vm.as_mut() {
                vm.darkness = n;
            }
        }
        "kop" => {
            // kop OP [INPUT]: activate a level script op's input (testing a sequence's step)
            if let (Some(op), Some(vm)) = (cmd.get(1).and_then(|v| v.parse::<u32>().ok()), vm.as_mut()) {
                let input = cmd.get(2).and_then(|v| v.parse::<u32>().ok()).unwrap_or(0);
                info!("script: activate op {op} input {input}");
                vm.activate_op(op, input);
            }
        }
        "remote" => {
            // remote EVENT: as the level scripts' SeqAct_ActivateRemoteEvent
            if let (Some(name), Some(vm)) = (cmd.get(1), vm.as_mut()) {
                info!("script: remote event {name}");
                vm.remote_event(name);
            }
        }
        "fx" => {
            // fx <effect> [distance]: play an original gameplay effect in front of the view
            if let Some(name) = cmd.get(1) {
                let d = cmd.get(2).and_then(|v| v.parse::<f32>().ok()).unwrap_or(4.0);
                let fwd = Quat::from_euler(EulerRot::YXZ, p.yaw, p.pitch, 0.0) * Vec3::NEG_Z;
                let at = t.translation + Vec3::Y * 0.6 + fwd * d;
                let name: &'static str = Box::leak(name.clone().into_boxed_str());
                fx.write(crate::particles::SpawnEffect::at(name, at));
                info!("script: fx {name} at {at:.1}");
            }
        }
        "select" => {
            // select <power or item>: hold it (`IncendiaryBolt`, `Grenade`, `Blink`, ...)
            if let Some(name) = cmd.get(1) {
                if let Some(pw) = crate::powers::SHORTCUT_ORDER.iter().copied().find(|p| format!("{p:?}").eq_ignore_ascii_case(name)) {
                    powers.selected = pw;
                    powers.aiming = false;
                    info!("script: select {pw:?}");
                }
            }
        }
        "flare" => {
            // flare [distance]: a steady gameplay light ahead (a burning flare's), until exit
            let d = cmd.get(1).and_then(|v| v.parse::<f32>().ok()).unwrap_or(2.0);
            let brightness = cmd.get(2).and_then(|v| v.parse::<f32>().ok()).unwrap_or(4.0);
            let fwd = Quat::from_euler(EulerRot::YXZ, p.yaw, p.pitch, 0.0) * Vec3::NEG_Z;
            let at = t.translation + Vec3::Y * 0.6 + fwd * d;
            commands.spawn((
                crate::fxlight::FxLight { color: Vec3::new(1.0, 0.55, 0.2), brightness, radius: 6.0 },
                Transform::from_translation(at),
                DespawnOnExit(GameState::InGame),
            ));
            info!("script: flare at {at:.1}");
        }
        "explode" => {
            // explode [distance]: a grenade's blast in front of the view
            let d = cmd.get(1).and_then(|v| v.parse::<f32>().ok()).unwrap_or(5.0);
            let fwd = Quat::from_euler(EulerRot::YXZ, p.yaw, p.pitch, 0.0) * Vec3::NEG_Z;
            let at = t.translation + Vec3::Y * 0.3 + fwd * d;
            explosions.write(crate::gadgets::Explosion { at, radius: 6.0, full: 2.0, damage: 0.0, effect: "grenade", player: None });
            info!("script: explode at {at:.1}");
        }
        "ammo" => {
            // ammo <type 0-7> [n]: the original ammo types (0 bullets .. 6 grenades, 7 sticky)
            let ty: u8 = cmd.get(1).and_then(|v| v.parse().ok()).unwrap_or(6);
            let n: u32 = cmd.get(2).and_then(|v| v.parse().ok()).unwrap_or(3);
            let name = crate::gadgets::give_ammo(&mut stats, ty, n);
            info!("script: {name} +{n}");
        }
        "runes" => {
            stats.runes = cmd.get(1).and_then(|v| v.parse().ok()).unwrap_or(10);
        }
        "mana" => {
            stats.mana = cmd.get(1).and_then(|v| v.parse().ok()).unwrap_or(stats.max_mana);
            stats.mana_cap = stats.max_mana;
        }
        "tpnpc" | "tpbody" => {
            // stand `D` metres (default 2.5) in front of the nearest living hostile NPC (whose
            // name holds `NAME`, if given), facing it; `tpbody`: the nearest body
            let d = cmd.get(1).and_then(|s| s.parse::<f32>().ok()).unwrap_or(2.5);
            let filter = cmd.get(2).map(String::as_str).filter(|f| *f != "-").unwrap_or("");
            let pitch = cmd.get(3).and_then(|s| s.parse::<f32>().ok()).unwrap_or(0.0);
            let near = npcs
                .iter()
                .filter(|(n, _, _)| if cmd[0] == "tpbody" { n.is_down() } else { (n.hostile() || !filter.is_empty()) && !n.is_down() })
                .filter(|(n, _, _)| n.name.contains(filter) || n.pawn.contains(filter))
                .min_by(|a, b| a.1.translation.distance(t.translation).total_cmp(&b.1.translation.distance(t.translation)));
            if let Some((n, nt, _)) = near {
                // in front if there's room, else the first open side
                let open = |dir: Vec3| {
                    use bevy_rapier3d::prelude::*;
                    let Ok(ctx) = rapier.single() else { return true };
                    let f = QueryFilter::default().groups(CollisionGroups::new(Group::ALL, crate::level::GROUP_WORLD));
                    let from = nt.translation + Vec3::Y * 0.5;
                    ctx.cast_ray(from, dir, d + 0.5, true, f).is_none() && ctx.cast_ray(from + dir * d, -Vec3::Y, 2.5, true, f).is_some()
                };
                let dir = (0..8).map(|i| Quat::from_rotation_y(i as f32 * std::f32::consts::FRAC_PI_4) * n.forward()).find(|&dir| open(dir)).unwrap_or(n.forward());
                let pos = nt.translation + dir * d + Vec3::Y * 0.3;
                let to = nt.translation - pos;
                t.translation = pos;
                p.yaw = (-to.x).atan2(-to.z);
                p.pitch = pitch;
                p.velocity = Vec3::ZERO;
                info!("script: tpnpc to {} near {} at {}", pos, n.name, nt.translation);
            }
        }
        "card" => {
            // card KIND NAME PORTRAIT: the HUD's target card (tests; 0 assassinated .. 3 spared)
            if let Some(vm) = vm.as_mut() {
                let kind = cmd.get(1).and_then(|s| s.parse().ok()).unwrap_or(0);
                vm.target_cards.push((cmd.get(2).cloned().unwrap_or_default().replace('_', " "), cmd.get(3).cloned().unwrap_or_default(), kind));
            }
        }
        "objpopup" => {
            // objpopup added|updated|completed|failed NAME|TASK|TASK (tests; `_` for spaces, a
            // task's state after `:`)
            use crate::objnotify::ObjState;
            let st = |s: &str| match s {
                "completed" => ObjState::Completed,
                "failed" => ObjState::Failed,
                "updated" => ObjState::Updated,
                _ => ObjState::Added,
            };
            let state = st(cmd.get(1).map(String::as_str).unwrap_or("added"));
            let rest = cmd[2.min(cmd.len())..].join(" ").replace('_', " ");
            let mut parts = rest.split('|');
            let name = parts.next().unwrap_or("").to_string();
            let tasks = parts.map(|t| t.split_once(':').map(|(a, b)| (a.to_string(), st(b))).unwrap_or((t.to_string(), ObjState::Added))).collect();
            commands.queue(move |w: &mut World| {
                if let Some(mut q) = w.get_resource_mut::<crate::objnotify::ObjectiveQueue>() {
                    q.push(crate::objnotify::ObjectiveNotice { name, state, tasks });
                }
            });
        }
        "choice" => {
            // choice A|B|C: a choice put to Corvo (tests; `_` for spaces; `DH_CHOICE=show`
            // leaves it up)
            let opts: Vec<(usize, String)> = cmd[1..].join(" ").replace('_', " ").split('|').enumerate().map(|(i, t)| (i, t.to_string())).collect();
            commands.queue(move |w: &mut World| {
                if let Some(mut c) = w.get_resource_mut::<crate::choice::Choice>() {
                    c.ask(u32::MAX, opts);
                }
            });
        }
        "breath" => {
            // breath N: seconds of breath left (tests: the oxygen gauge, as it fills back)
            let b = f(1);
            commands.queue(move |w: &mut World| {
                if let Some(mut s) = w.get_resource_mut::<crate::swim::Swim>() {
                    s.breath = b.clamp(0.0, s.max);
                }
            });
        }
        "tutwindow" => {
            // tutwindow IMG TEXT: the tutorial window (0 gear, 1 note, 2 rune, 3 charm; `_` for
            // spaces, `|` for new lines)
            let img = cmd.get(1).and_then(|s| s.parse().ok()).unwrap_or(2);
            let text = cmd.get(2..).map(|w| w.join(" ")).unwrap_or_default().replace('_', " ").replace('|', "\n");
            commands.queue(move |w: &mut World| w.resource_mut::<crate::tutwindow::TutorialWindow>().push(text, img));
        }
        "message" | "tutorial" => {
            // message TEXT / tutorial TEXT: a HUD game message or tutorial (tests; `_` for
            // spaces)
            let text = cmd[1..].join(" ").replace('_', " ");
            let tutorial = cmd[0] == "tutorial";
            commands.queue(move |w: &mut World| {
                if let Some(mut m) = w.get_resource_mut::<crate::gameplay::HudMessages>() {
                    if tutorial {
                        m.tutorial(text, 10.0);
                    } else {
                        m.push(text);
                    }
                }
            });
        }
        "location" => {
            // location NAME: the location banner (tests)
            let name = cmd[1..].join(" ").replace('_', " ");
            commands.queue(move |w: &mut World| {
                if let Some(mut ui) = w.get_resource_mut::<crate::kismet::ScriptUi>() {
                    ui.location = Some(name);
                }
            });
        }
        "aware" => {
            // aware 1,2,3,4,0: the awareness markers step through these levels (tests)
            let levels = cmd.get(1).map(|l| l.split(',').filter_map(|x| x.parse().ok()).collect()).unwrap_or_default();
            commands.insert_resource(crate::awareness::ForcedLevels { levels, t0: None });
        }
        "npcidle" => {
            // (tests) the nearest living hostile character stops where it is, facing Corvo
            if let Some((e, _, nt)) = npc_ents.iter().filter(|(_, n, _)| n.hostile() && !n.is_down()).min_by(|a, b| a.2.translation.distance(t.translation).total_cmp(&b.2.translation.distance(t.translation))) {
                let to = t.translation - nt.translation;
                let yaw = (-to.x).atan2(-to.z);
                let home = nt.translation;
                commands.queue(move |w: &mut World| {
                    if let Some(mut n) = w.get_mut::<Npc>(e) {
                        n.route.clear();
                        n.home = home;
                        n.home_yaw = yaw;
                        n.yaw = yaw;
                        n.target = None;
                        n.set_mode(crate::npc::Mode::Idle);
                    }
                });
            }
        }
        "spawn" => {
            // (tests) spawn NAME: a spawner the level scripts would start (a wolfhound's kennel)
            let want = cmd.get(1).cloned().unwrap_or_default();
            if let Some(i) = level.as_ref().and_then(|l| l.scene.spawners.iter().position(|sp| sp.name == want)) {
                commands.queue(move |w: &mut World| {
                    w.write_message(crate::npc::SpawnRequest(i as u32));
                });
                info!("script: spawn {want} (#{i})");
            }
        }
        "flash" => {
            // (tests) flash MOVIE SYMBOL [X Y]: a Scaleform symbol of the original movies on
            // the screen, at stage X Y (1280x720, scaled to the window), over everything
            if let (Some(movie), Some(sym)) = (cmd.get(1).cloned(), cmd.get(2).cloned()) {
                let at = Vec2::new(if cmd.len() > 3 { f(3) } else { 0.0 }, if cmd.len() > 4 { f(4) } else { 0.0 });
                commands.queue(move |w: &mut World| {
                    let tl = w.resource_mut::<crate::flash::MovieTimelines>().get(&movie);
                    let Some(tl) = tl else { return };
                    let Some(clip) = crate::flash::Clip::export(&tl, &sym) else {
                        warn!("script: no symbol {sym} in {movie}");
                        return;
                    };
                    let (ww, wh) = w.query_filtered::<&Window, With<bevy::window::PrimaryWindow>>().iter(w).next().map(|w| (w.width(), w.height())).unwrap_or((1280.0, 720.0));
                    let s = (ww / 1280.0).min(wh / 720.0);
                    let off = (Vec2::new(ww, wh) - Vec2::new(1280.0, 720.0) * s) * 0.5;
                    let mut fc = crate::flash::FlashClip::new(&movie, tl.clone(), clip);
                    fc.scale = s;
                    let p = off + at * s;
                    w.spawn((fc, Node { position_type: PositionType::Absolute, left: Val::Px(p.x), top: Val::Px(p.y), ..default() }, GlobalZIndex(100), Pickable::IGNORE, DespawnOnExit(crate::GameState::InGame)));
                    info!("script: flash {movie} {sym}");
                });
            }
        }
        "tpactor" => {
            // tpactor NAME [D]: stand D m (default 1.5) before a level-script actor, facing it
            let want = cmd.get(1).cloned().unwrap_or_default();
            let dist = if cmd.len() > 2 { f(2) } else { 1.5 };
            if let Some(a) = level.as_ref().and_then(|l| l.scene.kismet.actors.iter().find(|a| a.name == want)) {
                let at = Vec3::from(a.position);
                let fwd = Quat::from_rotation_y(a.yaw) * Vec3::NEG_Z;
                t.translation = at + fwd * dist + Vec3::Y * 0.9;
                let to = at + Vec3::Y * 1.0 - (t.translation + Vec3::Y * 0.7);
                p.yaw = (-to.x).atan2(-to.z);
                p.pitch = (to.y / to.length().max(1e-3)).asin();
                p.velocity = Vec3::ZERO;
                info!("script: tpactor {want} at {at:.1}");
            }
        }
        "ambush" => {
            // (tests) ambush [NAME]: the nearest living swordsman (whose name or pawn holds NAME)
            // turns on Corvo and lies in wait for him where it stands
            let named = cmd.get(1).cloned().unwrap_or_default();
            if let Some((e, _, _)) = npc_ents.iter().filter(|(_, n, _)| !n.is_down() && n.has_sword && (named.is_empty() || n.name.contains(&named) || n.pawn.contains(&named))).min_by(|a, b| a.2.translation.distance(t.translation).total_cmp(&b.2.translation.distance(t.translation))) {
                commands.queue(move |w: &mut World| {
                    if let Some(mut n) = w.get_mut::<Npc>(e) {
                        n.enemy = true;
                        n.alert = crate::npc::Alert::Combat;
                        n.set_mode(crate::npc::Mode::Combat);
                        n.ambush = Some(0.0);
                        n.gesture(9);
                    }
                });
            }
        }
        "clash" => {
            // (tests) the nearest living hostile swordsman's blow comes as Corvo strikes: it
            // faces him mid-swing (its blow due as his lands), and he attacks — a sword lock
            if let Some((e, _, nt)) = npc_ents.iter().filter(|(_, n, _)| n.hostile() && !n.is_down() && n.has_sword).min_by(|a, b| a.2.translation.distance(t.translation).total_cmp(&b.2.translation.distance(t.translation))) {
                let to = t.translation - nt.translation;
                let yaw = (-to.x).atan2(-to.z);
                commands.queue(move |w: &mut World| {
                    if let Some(mut n) = w.get_mut::<Npc>(e) {
                        n.alert = crate::npc::Alert::Combat;
                        n.set_mode(crate::npc::Mode::Combat);
                        n.sees_player = true;
                        n.stagger = 0.0;
                        n.yaw = yaw;
                        n.swing = Some(crate::npc::Swing { kind: crate::npc::MoveKind::Short, big: false, len: 0.85, hit: 0.4, reach: 2.6, lunge: 0.0, no_versus: false });
                        n.attack_t = Some(0.4 - 0.18);
                    }
                });
                script.held.push((Input::Mouse(MouseButton::Left), 0.05));
            }
        }
        "note" => {
            // note KEY: read a note (an abstract item's key, or `map:DUM_...`)
            if let Some(key) = cmd.get(1).cloned() {
                commands.queue(move |w: &mut World| {
                    w.write_message(crate::journal::ReadNote(key));
                });
            }
        }
        "store" => {
            // store TWEAK: open a store (`Twk_Store_Craftsman`, `Twk_Store_GriffShop`)
            if let Some(tweak) = cmd.get(1).cloned() {
                commands.queue(move |w: &mut World| {
                    w.write_message(crate::store::OpenStore(tweak));
                });
            }
        }
        "stats" => {
            // stats TWEAK: the campaign's statistics screen (`Twk_M1_Overseers`...)
            if let Some(tweak) = cmd.get(1).cloned() {
                commands.queue(move |w: &mut World| {
                    w.write_message(crate::mission::ShowMissionStats { tweak, op: 0 });
                });
            }
        }
        "travel" => {
            // leave for another map as the level scripts would (start 0)
            if let Some(map) = cmd.get(1) {
                commands.queue({
                    let map = map.clone();
                    move |w: &mut World| {
                        w.write_message(crate::mission::TravelRequest { map, start: 0 });
                    }
                });
            }
        }
        "look" => {
            p.yaw = f(1);
            p.pitch = f(2);
        }
        "krust" => {
            // krust [D] [N]: hover D m (default 8) in front of the nearest river krust (or the
            // level's N-th), looking at it
            let d = if cmd.len() > 1 { f(1) } else { 8.0 };
            let mut list: Vec<_> = krusts.iter().map(|(k, g)| (g.translation(), k.facing, k.index(), k.body())).collect();
            list.sort_by(|a, b| a.0.distance(t.translation).total_cmp(&b.0.distance(t.translation)));
            let pick = if cmd.len() > 2 { list.iter().find(|k| k.2 == f(2) as u32) } else { list.first() };
            if let Some(&(at, facing, _, body)) = pick {
                let dir = facing.with_y(0.0).normalize_or(Vec3::X);
                t.translation = at + dir * d + Vec3::Y * 0.6;
                p.velocity = Vec3::ZERO;
                p.noclip = true;
                // at its body (where shots strike)
                let aim = strikeables.get(body).map(|(_, g)| g.translation()).unwrap_or(at + Vec3::Y * 0.5);
                let to = aim - (t.translation + Vec3::Y * 0.6);
                p.yaw = (-to.x).atan2(-to.z);
                p.pitch = (to.y / to.length().max(1e-3)).asin();
                info!("script: krust at {at:.2}, player at {:.2}", t.translation);
            }
        }
        "usable" => {
            // usable [LABEL]: stand 1.3 m from the nearest usable object (its label holding
            // LABEL), on its open side, looking at it
            let want = cmd.get(1).map(|s| s.to_ascii_lowercase());
            let dist = if cmd.len() > 2 { f(2) } else { 1.3 };
            let label = |r: &crate::usables::UsableRig| level.as_ref().and_then(|l| l.scene.usables.get(r.index() as usize)).map(|u| u.label.to_ascii_lowercase()).unwrap_or_default();
            let near = usable_rigs
                .iter()
                .filter(|r| want.as_ref().is_none_or(|w| label(r).contains(w.as_str())))
                .min_by(|a, b| a.center().distance(t.translation).total_cmp(&b.center().distance(t.translation)));
            if let Some(r) = near {
                let c = r.center();
                let ctx = rapier.single().ok();
                let walls = bevy_rapier3d::prelude::QueryFilter::default().groups(bevy_rapier3d::prelude::CollisionGroups::new(bevy_rapier3d::prelude::Group::ALL, crate::level::GROUP_WORLD));
                // the side with room to stand
                let mut best = None;
                // (from outside its own body: room in front, and floor under the spot)
                let mut best_room = 0.0;
                for k in 0..16 {
                    let dir = Quat::from_rotation_y(k as f32 * std::f32::consts::FRAC_PI_8) * Vec3::X;
                    let Some(cx) = ctx.as_ref() else { break };
                    let from = c + dir * 0.6;
                    let room = cx.cast_ray(from, dir, 2.0, true, walls).map(|(_, toi)| toi).unwrap_or(2.0);
                    let floor = cx.cast_ray(c + dir * dist, Vec3::NEG_Y, 2.5, true, walls).is_some();
                    // (and nothing between it and the spot but itself)
                    let back = cx.cast_ray(c + dir * dist, -dir, dist - 0.6, true, walls).is_none();
                    if floor && back && room > best_room {
                        best_room = room;
                        best = Some(dir);
                    }
                }
                let dir = best.unwrap_or(Vec3::X);
                let at = c + dir * dist;
                t.translation = at.with_y(c.y.max(at.y) + 0.3);
                p.velocity = Vec3::ZERO;
                p.noclip = true;
                let to = c - (t.translation + Vec3::Y * crate::player::STAND_EYE);
                p.yaw = (-to.x).atan2(-to.z);
                p.pitch = (to.y / to.length().max(1e-3)).asin();
                info!("script: usable #{} {} at {c:.2}, from {dir:.2} (room {best_room:.1})", r.index(), label(r));
            }
        }
        "wall" => {
            // wall [D]: stand on the floor D m (default 6) before the nearest wall of light,
            // looking at its eye
            let d = if cmd.len() > 1 { f(1) } else { 6.0 };
            let near = devices
                .list
                .iter()
                .filter_map(|dv| dv.wall_face().map(|w| (dv, w)))
                .min_by(|a, b| a.1 .0.distance(t.translation).total_cmp(&b.1 .0.distance(t.translation)));
            if let Some((dv, (mid, through))) = near {
                let flat = through.with_y(0.0).normalize_or(Vec3::X);
                let side = if (t.translation - mid).dot(flat) < 0.0 { -1.0 } else { 1.0 };
                let base = Vec3::from(dv.def.position).y;
                let at = (mid + flat * side * d).with_y(base + crate::player::STAND_HALF + crate::player::RADIUS + 0.05);
                t.translation = at;
                p.velocity = Vec3::ZERO;
                p.noclip = true;
                let eye = Vec3::from(dv.def.detect_at) + Vec3::Y * 1.0;
                let to = eye - (at + Vec3::Y * crate::player::STAND_EYE);
                p.yaw = (-to.x).atan2(-to.z);
                p.pitch = (to.y / to.length().max(1e-3)).asin();
                info!("script: wall {} mid {mid:.2}, player at {at:.2}", dv.def.actor);
            }
        }
        "trap" => {
            // trap [wire|launcher] [D]: stand on the floor D m (default 2.5) before the nearest
            // tripwire, facing across it (or before the nearest launcher, looking at it)
            let launcher = cmd.get(1).is_some_and(|s| *s == "launcher");
            let d = if cmd.len() > 2 { f(2) } else { 2.5 };
            let near = traps
                .iter()
                .filter(|(tp, _)| tp.wire().is_none() == launcher)
                .min_by(|a, b| a.1.translation.distance(t.translation).total_cmp(&b.1.translation.distance(t.translation)));
            if let Some((tp, tt)) = near {
                let floor = tt.translation.y;
                let (mid, across) = match tp.wire() {
                    Some((a, b)) => ((a + b) * 0.5, (b - a).cross(Vec3::Y).normalize_or(Vec3::X)),
                    None => (tt.translation + Vec3::Y * 0.4, tt.rotation * Vec3::X),
                };
                // the side he's on
                let side = if (t.translation - mid).dot(across) < 0.0 { -1.0 } else { 1.0 };
                let at = (mid + across * side * d).with_y(floor + crate::player::STAND_HALF + crate::player::RADIUS + 0.02);
                t.translation = at;
                p.velocity = Vec3::ZERO;
                p.noclip = false;
                let to = mid - (at + Vec3::Y * crate::player::STAND_EYE);
                p.yaw = (-to.x).atan2(-to.z);
                p.pitch = (to.y / to.length().max(1e-3)).asin();
                info!("script: trap {} at {mid:.2}, player at {at:.2}", tp.describe());
            }
        }
        "prop" => {
            // prop [NAME]: stand 1.3 m from the nearest loose prop (its name containing NAME),
            // on a side with floor and a clear view, looking at it
            let want = cmd.get(1).map(|s| s.to_ascii_lowercase());
            let names = |p: &crate::props::Prop| level.as_ref().and_then(|l| l.scene.movables.get(p.index)).map(|m| (m.name.to_ascii_lowercase(), m.fixed)).unwrap_or_default();
            let mut list: Vec<_> = props
                .iter()
                .filter(|(p, _)| {
                    let (n, fixed) = names(p);
                    // (a name finds the fixed breakables too)
                    match want.as_ref() {
                        Some(w) => n.contains(w.as_str()),
                        None => !fixed,
                    }
                })
                .map(|(_, pt)| pt.translation)
                .collect();
            list.sort_by(|a, b| a.distance(t.translation).total_cmp(&b.distance(t.translation)));
            let walls = bevy_rapier3d::prelude::QueryFilter::default().groups(bevy_rapier3d::prelude::CollisionGroups::new(bevy_rapier3d::prelude::Group::ALL, crate::level::GROUP_WORLD));
            if let Ok(ctx) = rapier.single() {
                'found: for at in list.iter().take(12) {
                    for k in 0..8 {
                        let a = k as f32 * std::f32::consts::FRAC_PI_4;
                        let eye = *at + Vec3::new(a.cos() * 1.3, 0.5, a.sin() * 1.3);
                        let to = *at + Vec3::Y * 0.1 - eye;
                        if ctx.cast_ray(eye, to.normalize(), to.length() - 0.15, true, walls).is_some() {
                            continue;
                        }
                        let Some((_, down)) = ctx.cast_ray(eye, Vec3::NEG_Y, 2.0, true, walls) else { continue };
                        let floor = eye.y - down;
                        t.translation = Vec3::new(eye.x, floor + crate::player::STAND_HALF + crate::player::RADIUS + 0.05, eye.z);
                        p.velocity = Vec3::ZERO;
                        let to = *at + Vec3::Y * 0.1 - (t.translation + Vec3::Y * 0.6);
                        p.yaw = (-to.x).atan2(-to.z);
                        p.pitch = (to.y / to.length().max(1e-3)).asin();
                        info!("script: prop at {at:.2}, player at {:.2}", t.translation);
                        break 'found;
                    }
                }
            }
        }
        "possess" => {
            // possess fish|krust|rat: the nearest such creature, at once (level 1)
            let kind = cmd.get(1).map(|s| s.as_str()).unwrap_or("rat");
            let near = hosts
                .iter()
                .filter(|(_, h, _)| match kind {
                    "fish" => h.fish,
                    "krust" => h.rooted,
                    _ => !h.fish && !h.rooted,
                })
                .min_by(|a, b| a.2.translation().distance(t.translation).total_cmp(&b.2.translation().distance(t.translation)));
            if let Some((e, _, g)) = near {
                info!("script: possess {kind} at {:.2}", g.translation());
                commands.queue(move |w: &mut World| {
                    w.write_message(crate::possession::PossessRequest { host: e, level: 1 });
                });
            }
        }
        "lookat" => {
            // aim the view at a world point (camera height above the player origin)
            let eye = t.translation + Vec3::Y * 0.6;
            let to = Vec3::new(f(1), f(2), f(3)) - eye;
            p.yaw = (-to.x).atan2(-to.z);
            p.pitch = (to.y / to.length().max(1e-3)).asin();
        }
        "face" => {
            // face [NAME]: aim the view at the nearest standing character (whose name or pawn
            // holds NAME)
            let named = cmd.get(1).cloned().unwrap_or_default();
            let eye = t.translation + Vec3::Y * 0.6;
            if let Some((_, at)) = npcs
                .iter()
                .filter(|(n, _, _)| !n.is_down() && (named.is_empty() || n.name.contains(&named) || n.pawn.contains(&named)))
                .map(|(_, nt, _)| (nt.translation.distance(t.translation), nt.translation + Vec3::Y * 0.6))
                .min_by(|a, b| a.0.total_cmp(&b.0))
            {
                let to = at - eye;
                p.yaw = (-to.x).atan2(-to.z);
                p.pitch = (to.y / to.length().max(1e-3)).asin();
            }
        }
        "walk" => script.held.push((Input::Key(KeyCode::KeyW), f(1))),
        "attack" => script.held.push((Input::Mouse(MouseButton::Left), 0.05)),
        "block" => script.held.push((Input::Key(KeyCode::ControlLeft), f(1))),
        "rmb" => script.held.push((Input::Mouse(MouseButton::Right), 0.05)),
        "rmbhold" => script.held.push((Input::Mouse(MouseButton::Right), f(1))),
        "mmbhold" => script.held.push((Input::Mouse(MouseButton::Middle), f(1))),
        "key" | "hold" => {
            if let Some(k) = cmd.get(1).and_then(|s| key_of(s)) {
                let secs = if cmd[0] == "hold" { f(2) } else { 0.05 };
                keys.press(k);
                script.held.push((Input::Key(k), secs));
            }
        }
        "god" => script.god = true,
        "health" => {
            // health N: set Corvo's health (tests: the near-death effects)
            script.god = false;
            stats.health = f(1).clamp(0.0, stats.max_health);
        }
        "die" => {
            // die: Corvo falls (tests: the game over menu)
            stats.health = 0.0;
            stats.dead = true;
        }
        "hurt" => {
            // hurt D ANGLE: a blow of D from ANGLE degrees off his view (clockwise; tests: the
            // damage feedback)
            let a = f(2).to_radians();
            let (fwd, right) = (Vec3::new(-p.yaw.sin(), 0.0, -p.yaw.cos()), Vec3::new(p.yaw.cos(), 0.0, -p.yaw.sin()));
            stats.health = (stats.health - f(1)).max(1.0);
            stats.hit_from = Some(t.translation + (fwd * a.cos() + right * a.sin()) * 2.0);
        }
        "epp" => {
            // epp <Epp_Knocked|Epp_Weepers|...> [0|1]: a scripted screen effect off / on
            // (`DisSeqAct_PostProcess`)
            if let Some(name) = cmd.get(1).cloned() {
                let on = cmd.get(2).is_none_or(|v| v != "0");
                commands.queue(move |w: &mut World| {
                    if let Some(mut vm) = w.get_resource_mut::<crate::kismet::Vm>() {
                        vm.post_effects.retain(|x| *x != name);
                        if on {
                            vm.post_effects.push(name);
                        }
                    }
                });
            }
        }
        "jump" => commands.queue(|w: &mut World| w.resource_mut::<crate::player::VirtualInput>().jump = true),
        // kuwa: the original's debug exec `KUWA` (the painterly post-process branch on / off)
        "kuwa" => commands.queue(|w: &mut World| {
            let mut k = w.resource_mut::<crate::kuwahara::KuwaSwitch>();
            k.0 = !k.0;
        }),
        "ko" => {
            // knock out the nearest standing NPC (a sleep dart); `ko NAME`: the nearest whose name
            // or pawn holds NAME
            let named = cmd.get(1).cloned().unwrap_or_default();
            if let Some((e, _)) = npc_ents.iter().filter(|(_, n, _)| !n.is_down() && (named.is_empty() || n.name.contains(&named) || n.pawn.contains(&named))).map(|(e, _, nt)| (e, nt.translation.distance(t.translation))).min_by(|a, b| a.1.total_cmp(&b.1)) {
                commands.queue(move |w: &mut World| {
                    w.write_message(crate::gameplay::NpcHit { npc: e, damage: 0.0, kind: crate::gameplay::HitKind::SleepDart, from: Vec3::ZERO });
                });
            }
        }
        "behind" | "front" | "side" => {
            // nearest standing NPC (optionally skipping the first N)
            let skip = f(1) as usize;
            // `behind N` skips the N nearest; `behind lone` picks the nearest NPC with nobody within 8 m
            let lone = cmd.get(1).map(|s| s == "lone").unwrap_or(false);
            // `behind NAME`: the nearest whose name or pawn holds NAME
            let named = cmd.get(1).filter(|s| *s != "lone" && s.parse::<usize>().is_err()).cloned().unwrap_or_default();
            let all: Vec<Vec3> = npcs.iter().filter(|(n, _, _)| !n.is_down()).map(|(_, nt, _)| nt.translation).collect();
            let mut list: Vec<_> = npcs
                .iter()
                .filter(|(n, _, _)| !n.is_down() && (n.hostile() || !named.is_empty()))
                .filter(|(n, _, _)| named.is_empty() || n.name.contains(&named) || n.pawn.contains(&named))
                .filter(|(_, nt, _)| !lone || all.iter().filter(|p| p.distance(nt.translation) < 8.0).count() <= 1)
                .map(|(n, nt, _)| (nt.translation.distance(t.translation), n.forward(), nt.translation))
                .collect();
            list.sort_by(|a, b| a.0.total_cmp(&b.0));
            if let Some((_, fwd, pos)) = list.get(skip) {
                // (`side`: 3 m off its right, a little ahead: its sight seen across)
                let at = match cmd[0].as_str() {
                    "behind" => *pos + *fwd * -cmd.get(2).and_then(|s| s.parse::<f32>().ok()).unwrap_or(1.25),
                    "side" => *pos + *fwd * 1.0 + Vec3::new(-fwd.z, 0.0, fwd.x) * 3.0,
                    _ => *pos + *fwd * 3.0,
                };
                t.translation = Vec3::new(at.x, pos.y, at.z);
                let to = (*pos - t.translation).with_y(0.0);
                p.yaw = (-to.x).atan2(-to.z);
                p.pitch = -0.15;
                p.noclip = false;
                p.velocity = Vec3::ZERO;
                info!("script: {} npc at {pos:.1}", cmd[0]);
            }
        }
        "pickup" | "door" | "use" => {
            let target = if cmd[0] == "pickup" {
                pickups.iter().map(|pt| pt.translation).min_by(|a, b| a.distance(t.translation).total_cmp(&b.distance(t.translation)))
            } else if cmd[0] == "use" {
                usables.iter().map(|ut| ut.translation).min_by(|a, b| a.distance(t.translation).total_cmp(&b.distance(t.translation)))
            } else {
                doors.iter().map(|g| g.translation() + Vec3::Y).min_by(|a, b| a.distance(t.translation).total_cmp(&b.distance(t.translation)))
            };
            if let Some(tp) = target {
                let dir = (t.translation - tp).with_y(0.0).normalize_or(Vec3::Z);
                t.translation = tp + dir * 1.3 + Vec3::Y * 0.6;
                let to = tp - (t.translation + Vec3::Y * 0.7);
                p.yaw = (-to.x).atan2(-to.z);
                p.pitch = (to.y / to.length()).asin();
                p.noclip = true;
                info!("script: near {} at {tp:.1}", cmd[0]);
            }
        }
        "shot" => {
            let name = cmd.get(1).cloned().unwrap_or_else(|| format!("s{}", script.idx));
            let _ = std::fs::create_dir_all(&script.dir);
            let path = format!("{}/{}.png", script.dir, name);
            info!("script: screenshot {path}");
            commands.spawn(Screenshot::primary_window()).observe(save_to_disk(path));
            script.timer = 0.2;
        }
        "log" => {
            let fps = diag
                .get(&bevy::diagnostic::FrameTimeDiagnosticsPlugin::FPS)
                .and_then(|d| d.smoothed())
                .unwrap_or(0.0);
            let ms = diag
                .get(&bevy::diagnostic::FrameTimeDiagnosticsPlugin::FRAME_TIME)
                .and_then(|d| d.smoothed())
                .unwrap_or(0.0);
            info!("script: fps {fps:.0} frame {ms:.2} ms");
            // render passes (`DH_RENDER_DIAG`), costliest first
            let mut passes: Vec<(String, f64)> = diag
                .iter()
                .filter(|d| d.path().as_str().starts_with("render/") && d.path().as_str().contains("elapsed"))
                .filter_map(|d| Some((d.path().as_str().to_string(), d.smoothed()?)))
                .collect();
            passes.sort_by(|a, b| b.1.total_cmp(&a.1));
            for (path, v) in passes.iter().take(24) {
                info!("script: {path} {v:.3}");
            }
            info!(
                "script: player pos {:.2} grounded {} locked {} vel {:.2} hp {:.0} mana {:.0} kills {} ko {} dead {}",
                t.translation, p.grounded, p.locked, p.velocity, stats.health, stats.mana, stats.kills, stats.knockouts, stats.dead
            );
            info!("script: runes {} coins {} powers {:?} charms {:?} items {:?}", stats.runes, stats.coins, stats.powers, stats.charms_owned, stats.items);
            commands.queue(|w: &mut World| {
                let c = w.resource::<crate::script_world::Cinematic>();
                let (on, hold, since) = (c.on, c.hold, c.since);
                let carry = w.resource::<crate::carry::Carry>().body.is_some();
                let climb = w.resource::<crate::climb::Climb>().on;
                info!("script: cinematic on {on} hold {hold} since {since:.1} carry {carry} climb {climb:?}");
            });
            info!("script: focus {:?}", focus.2);
            if let Some((pk, pt)) = pockets.iter().min_by(|a, b| a.1.translation.distance(t.translation).total_cmp(&b.1.translation.distance(t.translation))) {
                let owner = npc_ents.get(pk.npc).ok().map(|(_, n, nt)| (format!("{:?}", n.alert), nt.translation));
                info!("script: nearest pocket at {:.2} d {:.2} owner {:?}", pt.translation, pt.translation.distance(t.translation), owner);
            }
            if let Some(vm) = vm.as_ref() {
                info!("script: objectives {:?} targets {:?}", vm.objectives, vm.task_targets.iter().map(|(k, v)| (k.rsplit('.').next().unwrap_or(k).to_string(), v.1.clone(), vm.tasks.get(k).copied())).collect::<Vec<_>>());
            }
            for (tp, tt) in &traps {
                if tt.translation.distance(t.translation) < 30.0 {
                    info!("script: trap {} d {:.1}", tp.describe(), tt.translation.distance(t.translation));
                }
            }
            if let Some((k, g)) = krusts.iter().min_by(|a, b| a.1.translation().distance(t.translation).total_cmp(&b.1.translation().distance(t.translation))) {
                info!("script: nearest krust d {:.1}: {}", g.translation().distance(t.translation), k.describe());
            }
            if let Some(h) = possession.host {
                info!("script: possessing {h} for {:.1} s, body {:?}", possession.left, possession.body);
            }
            if let Some((f, ft)) = fish.iter().min_by(|a, b| a.1.translation.distance(t.translation).total_cmp(&b.1.translation.distance(t.translation))) {
                info!("script: nearest fish d {:.1} at {:.2} heading {:.2}: {}", ft.translation.distance(t.translation), ft.translation, ft.rotation * Vec3::NEG_Z, f.describe());
            }
            if swim.swimming() || swim.under.is_some() || swim.breath < swim.max {
                info!("script: swimming {} surface {:.2} under {} breath {:.1}/{:.0}", swim.swimming(), swim.surface, swim.under.is_some(), swim.breath, swim.max);
            }
            let mut list: Vec<_> = npcs.iter().map(|(n, nt, _)| (nt.translation.distance(t.translation), n, nt.translation)).collect();
            list.sort_by(|a, b| a.0.total_cmp(&b.0));
            for (d, n, pos) in list.iter().take(4) {
                info!(
                    "script:   npc {} d={:.1} pos={:.1} mode={:?} alert={:?} aware={:.2} hp={:.0} sees={} down={:.2} enemy={} pawn={}",
                    n.name, d, pos, n.mode, n.alert, n.awareness, n.health, n.sees_player, n.down_t, n.enemy, n.pawn
                );
            }
        }
        "census" => {
            // NPC health check: modes, and anyone far from home height (fell through / floating)
            let mut modes = std::collections::BTreeMap::new();
            let mut odd = 0;
            let (mut pathing, mut straight) = (0, 0);
            for (n, nt, u) in &npcs {
                *modes.entry(format!("{:?}", n.mode)).or_insert(0) += 1;
                if let (Some(tg), true) = (n.target, std::env::var("DH_NAV_LOG").is_ok()) {
                    let next = n.nav.points.get(n.nav.next).copied();
                    info!(
                        "script:   nav {} {:?} at {:.1} target {:.1} path {} next {:?} vel {:.2}",
                        n.name,
                        n.mode,
                        nt.translation,
                        tg,
                        n.nav.points.len(),
                        next.map(|p| (p * 10.0).round() / 10.0),
                        n.velocity.with_y(0.0).length()
                    );
                }
                if n.target.is_some() {
                    if n.nav.points.len() > 1 {
                        pathing += 1;
                    } else {
                        straight += 1;
                    }
                }
                if let Some(u) = u {
                    info!("script:   npc {} ({}) at {:.1}: {}", n.name, n.pawn, nt.translation, u.label);
                }
                if (nt.translation.y - n.home.y).abs() > 6.0 {
                    odd += 1;
                    info!("script:   npc {} at {:.1} far from home {:.1} mode {:?}", n.name, nt.translation, n.home, n.mode);
                }
            }
            info!("script: census {modes:?} off-height {odd} paths {pathing} straight {straight}");
        }
        "exit" => {
            exit.write(AppExit::Success);
        }
        other => warn!("script: unknown command {other}"),
    }
}
