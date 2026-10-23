//! The pad's rumble as the original plays it (`ForceFeedbackWaveform`): each waveform's samples
//! drive the large (left) and small (right) motors, at their amplitudes (0-100) and in their
//! shapes over each sample (`EWaveformFunction`: constant, rising, falling, the sine's quarters
//! and half, noise), several at once at the strongest of them. What plays them: Corvo's clips
//! (`AnimNotify_Rumble`: the pistol's and crossbow's shots, the powers' casts, the choke, the
//! blades locked), the powers as they are used (`m_RumbleAndShake` by level), the Heart's beats,
//! a power refused (`m_pDisabledPowerForceFeedback`), the wheel's each item pointed at
//! (`m_pSelectionForceFeedback`), and the contact system
//! (`DisContactSub_Rumble`): a blade or a missile striking him, his sword striking a body or a
//! wall, the blades meeting, a rat's bite, and, within their reach, a tallboy's step and an
//! explosion. Only with the pad's `Vibration` on.

use bevy::input::gamepad::{Gamepad, GamepadRumbleIntensity, GamepadRumbleRequest};
use bevy::prelude::*;
use dhcook::format::RumbleSample;
use std::time::Duration;

pub struct RumblePlugin;

impl Plugin for RumblePlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Playing>().add_systems(Update, (power_rumbles, drive).chain());
    }
}

/// What is asked to rumble (from anywhere: the systems that know).
enum Ask {
    Key(String),
    Near(String, Vec3),
    Anim(usize),
    Wave(Vec<RumbleSample>),
    Stop,
}

static ASKED: std::sync::Mutex<Vec<Ask>> = std::sync::Mutex::new(Vec::new());

/// Rumble a waveform of the game data's (`rumbles`: `heart.beat.0`, `contact.Weapon.Body.struck`).
pub fn play(key: &str) {
    if let Ok(mut a) = ASKED.lock() {
        a.push(Ask::Key(key.to_string()));
    }
}

/// Rumble a contact's waveform felt within its reach (`m_RumbleForRadius`: `m_fProbeRadius`) of
/// where it happened.
pub fn play_near(key: &str, at: Vec3) {
    if let Ok(mut a) = ASKED.lock() {
        a.push(Ask::Near(key.to_string(), at));
    }
}

/// Rumble a waveform of the scripts' (`SeqAct_ForceFeedback`'s own), or stop what plays (its
/// Stop).
pub fn play_wave(wave: Vec<RumbleSample>) {
    if let Ok(mut a) = ASKED.lock() {
        a.push(if wave.is_empty() { Ask::Stop } else { Ask::Wave(wave) });
    }
}

/// Rumble one of Corvo's clips' notifies (`anim_rumbles`).
pub fn play_anim(i: usize) {
    if let Ok(mut a) = ASKED.lock() {
        a.push(Ask::Anim(i));
    }
}

/// The clips' rumble notifies by clip name: (time, its place in `anim_rumbles`).
pub fn marks(data: &crate::gamedata::Data) -> std::collections::HashMap<String, Vec<(f32, usize)>> {
    let mut out: std::collections::HashMap<String, Vec<(f32, usize)>> = std::collections::HashMap::new();
    for (i, r) in data.0.anim_rumbles.iter().enumerate() {
        out.entry(r.clip.clone()).or_default().push((r.time, i));
    }
    out
}

/// The waveforms playing (each with how far in), the motors' last levels, when they were last
/// sent.
#[derive(Resource, Default)]
struct Playing {
    waves: Vec<(Vec<RumbleSample>, f32)>,
    last: (f32, f32),
    since: f32,
    seen_refusals: u32,
}

/// The clips' rumble notifies (`marks`), once the game data is in.
pub static MARKS: std::sync::OnceLock<std::collections::HashMap<String, Vec<(f32, usize)>>> = std::sync::OnceLock::new();

/// A motor's level at a time within a sample.
fn level(amp: u8, shape: u8, x: f32) -> f32 {
    // (some ask for more than the motors give: a tallboy's step, 200)
    let a = (amp as f32 / 100.0).min(1.0);
    let x = x.clamp(0.0, 1.0);
    let h = std::f32::consts::FRAC_PI_2;
    a * match shape {
        1 => x,
        2 => 1.0 - x,
        3 => (x * h).sin(),
        4 => (h + x * h).sin(),
        5 => (x * 2.0 * h).sin(),
        6 => rand::random::<f32>(),
        _ => 1.0,
    }
}

/// A waveform's motors at a time into it (`None` once it is over).
fn sample(w: &[RumbleSample], t: f32) -> Option<(f32, f32)> {
    let mut start = 0.0;
    for s in w {
        if t < start + s.duration {
            let x = if s.duration > 0.0 { (t - start) / s.duration } else { 0.0 };
            return Some((level(s.left, s.left_fn, x), level(s.right, s.right_fn, x)));
        }
        start += s.duration;
    }
    None
}

/// The powers used and refused (their counts on `Powers`): their rumbles.
fn power_rumbles(mut used: MessageReader<crate::powers::PowerUsed>, powers: Option<Res<crate::powers::Powers>>, stats: Res<crate::gameplay::PlayerStats>, mut st: ResMut<Playing>) {
    for u in used.read() {
        if let Some(k) = u.0.key() {
            play(&format!("power.{k}.{}", u.0.level(&stats).clamp(1, 2) - 1));
        }
    }
    if let Some(p) = powers {
        if p.no_mana_seq != st.seen_refusals {
            if p.no_mana_seq == st.seen_refusals.wrapping_add(1) {
                play("player.disabled_power");
            }
            st.seen_refusals = p.no_mana_seq;
        }
    }
}

fn drive(
    mut st: ResMut<Playing>,
    data: Res<crate::gamedata::Data>,
    settings: Res<crate::settings::Settings>,
    time: Res<Time>,
    pads: Query<Entity, With<Gamepad>>,
    player: Query<&Transform, With<crate::player::Player>>,
    mut req: MessageWriter<GamepadRumbleRequest>,
) {
    MARKS.get_or_init(|| marks(&data));
    let asked: Vec<Ask> = ASKED.lock().map(|mut a| std::mem::take(&mut *a)).unwrap_or_default();
    let on = settings.pad_vibration && !pads.is_empty();
    // (`DH_RUMBLE_LOG`: each rumble asked for, a pad or not)
    let log = std::env::var("DH_RUMBLE_LOG").is_ok();
    if on || log {
        let me = player.single().ok().map(|t| t.translation);
        for a in asked {
            let name = match &a {
                Ask::Key(k) | Ask::Near(k, _) => k.clone(),
                Ask::Anim(i) => data.0.anim_rumbles.get(*i).map(|r| format!("{}:{}", r.set, r.clip)).unwrap_or_default(),
                Ask::Wave(_) => "script".into(),
                Ask::Stop => {
                    st.waves.clear();
                    continue;
                }
            };
            let w = match a {
                Ask::Key(k) => data.0.rumbles.get(&k).cloned(),
                // (`m_fProbeRadius` in UE3's units)
                Ask::Near(k, at) => {
                    let reach = data.0.rumble_radius.get(&k).copied().unwrap_or(0.0) * 0.01;
                    if me.is_some_and(|p| p.distance(at) <= reach) {
                        data.0.rumbles.get(&k).cloned()
                    } else {
                        None
                    }
                }
                Ask::Anim(i) => data.0.anim_rumbles.get(i).map(|r| r.wave.clone()),
                Ask::Wave(w) => Some(w),
                Ask::Stop => None,
            };
            if log {
                info!("rumble: {name} {}", if w.is_some() { "plays" } else { "(none)" });
            }
            if let Some(w) = w.filter(|_| on) {
                st.waves.push((w, 0.0));
            }
        }
    } else {
        st.waves.clear();
    }
    // (the game's time: paused, they stop)
    let dt = time.delta_secs();
    for w in &mut st.waves {
        w.1 += dt;
    }
    let mut lv = (0.0f32, 0.0f32);
    st.waves.retain(|(w, t)| match sample(w, *t) {
        Some((l, r)) => {
            lv = (lv.0.max(l), lv.1.max(r));
            true
        }
        None => false,
    });
    if dt <= 0.0 {
        lv = (0.0, 0.0);
    }
    // the motors as the waves have them, sent a twentieth of a second at a time
    st.since += time.delta_secs().max(1.0 / 120.0);
    let quiet = lv.0 <= 0.0 && lv.1 <= 0.0;
    if quiet && st.last == (0.0, 0.0) {
        return;
    }
    if !quiet && st.since < 0.05 && st.last != (0.0, 0.0) {
        return;
    }
    st.since = 0.0;
    st.last = lv;
    for g in &pads {
        req.write(GamepadRumbleRequest::Stop { gamepad: g });
        if !quiet {
            req.write(GamepadRumbleRequest::Add { gamepad: g, duration: Duration::from_millis(100), intensity: GamepadRumbleIntensity { strong_motor: lv.0, weak_motor: lv.1 } });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn samples_follow_their_shapes_and_end() {
        let w = vec![RumbleSample { left: 70, right: 0, left_fn: 0, right_fn: 0, duration: 0.25 }, RumbleSample { left: 100, right: 50, left_fn: 2, right_fn: 1, duration: 0.5 }];
        assert_eq!(sample(&w, 0.1), Some((0.7, 0.0)));
        let (l, r) = sample(&w, 0.5).unwrap();
        assert!((l - 0.5).abs() < 1e-4 && (r - 0.25).abs() < 1e-4);
        assert_eq!(sample(&w, 0.8), None);
    }
}
