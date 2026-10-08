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
            .add_systems(Update, (hear_tunes, combat_tunes).before(crate::player::PlayerMoveSet).run_if(in_state(GameState::InGame)))
            .add_systems(Update, sway.after(crate::player::PlayerMoveSet).run_if(in_state(GameState::InGame)));
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
                kill.write(crate::swarm::KillRats { at: t.translation + fwd * x, radius: (x * half).max(1.0) });
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
