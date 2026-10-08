//! Corvo's breath (`DisTweaks_PlayerPawn`'s breathlessness): sprinting winds him
//! (`m_fBreathlessnessSpeedSprint` a second; running, `m_fBreathlessnessSpeedRun` up to
//! `m_fMaxBreathlessnessRun`; never under `m_fMinBaseBreathlessness`) and he gets it back as he
//! eases off (`m_fBreathRegainSpeed`). His panting plays while it rises
//! (`m_pBreathlessnessIncreaseEvent`), his catching his breath while it falls
//! (`m_pBreathlessnessDecreaseEvent`); how winded he is is the game parameter
//! `m_BreathlessnessRTPC` (`Player_Sprint_Breath`), on which their curves set their volume and
//! pitch (`audio::Rtpcs`: silent until a fifth winded, louder and higher as he tires), and his
//! head heaves with it (`ADD_Head_OutOfbreath` weighted by `m_fBreathlessnessAnimWeight`:
//! `arms.rs`).

use crate::audio::{event_id, PostEvent, Rtpcs, StopEvent};
use crate::gamedata::Data;
use crate::player::Player;
use crate::GameState;
use bevy::prelude::*;

pub struct BreathPlugin;

impl Plugin for BreathPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Breath>()
            .add_systems(OnEnter(GameState::InGame), |mut b: ResMut<Breath>| *b = Breath::default())
            .add_systems(Update, breathe.after(crate::player::PlayerMoveSet).run_if(in_state(GameState::InGame)));
    }
}

/// How winded Corvo is (0-1), and the breath sounding (panting: true; recovering: false).
#[derive(Resource, Default)]
pub struct Breath {
    pub level: f32,
    playing: Option<bool>,
    /// how long it has gone the other way (a pause in a sprint doesn't change the sound)
    turn: f32,
}

#[allow(clippy::too_many_arguments)]
fn breathe(
    time: Res<Time>,
    data: Res<Data>,
    mut b: ResMut<Breath>,
    player: Query<&Player>,
    swim: Res<crate::swim::Swim>,
    mut rtpcs: ResMut<Rtpcs>,
    mut sfx: MessageWriter<PostEvent>,
    mut stop: MessageWriter<StopEvent>,
) {
    let Ok(p) = player.single() else { return };
    let dt = time.delta_secs();
    let speed = Vec2::new(p.velocity.x, p.velocity.z).length();
    let v = |k: &str, d: f32| data.pawn(k, d);
    let (sprint, run, run_max, regain, base) =
        (v("m_fBreathlessnessSpeedSprint", 0.03), v("m_fBreathlessnessSpeedRun", 0.0), v("m_fMaxBreathlessnessRun", 0.0), v("m_fBreathRegainSpeed", 0.08), v("m_fMinBaseBreathlessness", 0.0));
    let on_foot = p.grounded && !swim.swimming() && !p.noclip;
    let rate = if on_foot && p.sprinting && speed > 4.5 {
        sprint
    } else if on_foot && speed > 2.6 && b.level < run_max {
        run
    } else {
        -regain
    };
    let before = b.level;
    b.level = (b.level + rate * dt).clamp(base.clamp(0.0, 1.0), 1.0);
    // the sound for the way it goes (held a moment before it changes), none once recovered
    let way = if b.level > before + 1e-6 {
        Some(true)
    } else if b.level < before - 1e-6 {
        Some(false)
    } else {
        b.playing
    };
    let want = if b.level <= base + 1e-3 { None } else { way };
    b.turn = if want != b.playing { b.turn + dt } else { 0.0 };
    let (inc, dec) = (data.pawn_text("m_pBreathlessnessIncreaseEvent", "Snd_P_Sprint_Breath"), data.pawn_text("m_pBreathlessnessDecreaseEvent", "Snd_P_Sprint_Breath_Stop"));
    if want != b.playing && (b.turn > 0.3 || want.is_none()) {
        if let Some(r) = b.playing {
            stop.write(StopEvent(event_id(if r { &inc } else { &dec })));
        }
        if let Some(r) = want {
            sfx.write(PostEvent::named(if r { &inc } else { &dec }, None));
        }
        b.playing = want;
        b.turn = 0.0;
    }
    rtpcs.set(&data.pawn_text("m_BreathlessnessRTPC", "Player_Sprint_Breath"), b.level.clamp(0.0, 1.0));
}
