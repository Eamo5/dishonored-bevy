//! The sound output and its Speaker Configuration (`PSI_AudioPC_SpeakerConfiguration`:
//! `SpeakerConfiguration_Auto` / `_Stereo` / `_5Point1`). The game's sounds play on a stream of
//! that layout — the device's own (Auto), two channels or six (FL FR FC LFE BL BR) — each
//! through a panner: a sound in the world is set among the speakers by its direction from the
//! listener (pairwise constant power between the speakers about it, as Wwise's 3D positioning:
//! the front pair and the surrounds, the centre and LFE left to the screen's), spreading over
//! them all as it comes close; a sound of the screen's (2D) plays on the front pair.

use bevy::audio::{AudioSource, Decodable, Source};
use bevy::prelude::*;
use std::num::NonZero;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

/// The most output channels laid out (7.1).
pub const MAX_CH: usize = 8;

/// The reverb of the room Corvo hears from (its environment's: decay seconds, high damping,
/// wet gain), shared with the audio thread; every world sound rings in it.
pub struct RoomReverb([AtomicU32; 3]);

pub static ROOM_REVERB: RoomReverb = RoomReverb([AtomicU32::new(0), AtomicU32::new(0), AtomicU32::new(0)]);

impl RoomReverb {
    /// Set the room's reverb (a decay of 0 or no wet: none).
    pub fn set(&self, decay: f32, damp: f32, wet: f32) {
        for (a, v) in self.0.iter().zip([decay, damp, wet]) {
            a.store(v.to_bits(), Ordering::Relaxed);
        }
    }
    fn get(&self) -> (f32, f32, f32) {
        let g = |k: usize| f32::from_bits(self.0[k].load(Ordering::Relaxed));
        (g(0), g(1), g(2))
    }
}

/// A small reverb (Schroeder: four damped combs into two allpasses) a world sound rings
/// through, the room's decay and damping setting its combs' feedback.
struct Verb {
    combs: [(Vec<f32>, usize, f32); 4],
    aps: [(Vec<f32>, usize); 2],
    rate: f32,
    /// the decay the combs' feedback is set for, and that feedback
    decay: f32,
    gains: [f32; 4],
}

impl Verb {
    fn new(rate: f32) -> Verb {
        let s = rate / 44100.0;
        let len = |n: f32| ((n * s) as usize).max(8);
        Verb {
            combs: [1116.0, 1277.0, 1422.0, 1557.0].map(|n| (vec![0.0; len(n)], 0, 0.0)),
            aps: [556.0, 341.0].map(|n| (vec![0.0; len(n)], 0)),
            rate,
            decay: -1.0,
            gains: [0.0; 4],
        }
    }

    /// One sample through (decay seconds, damping 0..1).
    fn run(&mut self, x: f32, decay: f32, damp: f32) -> f32 {
        if decay != self.decay {
            // (60 dB down over the decay, for each comb's length)
            self.decay = decay;
            for (g, (buf, _, _)) in self.gains.iter_mut().zip(&self.combs) {
                *g = 10f32.powf(-3.0 * buf.len() as f32 / (decay.max(0.05) * self.rate));
            }
        }
        let mut out = 0.0;
        for ((buf, i, store), &g) in self.combs.iter_mut().zip(&self.gains) {
            let y = buf[*i];
            *store = y * (1.0 - damp) + *store * damp;
            buf[*i] = x + *store * g;
            *i = (*i + 1) % buf.len();
            out += y;
        }
        for (buf, i) in self.aps.iter_mut() {
            let b = buf[*i];
            buf[*i] = out + b * 0.5;
            out = b - out;
            *i = (*i + 1) % buf.len();
        }
        out * 0.25
    }
}

/// The output stream, as opened for the setting.
#[derive(Resource)]
pub struct Output {
    sink: Option<rodio::MixerDeviceSink>,
    /// the setting it was opened for
    pub config: u8,
    pub channels: usize,
}

impl Output {
    /// Open the default device for a Speaker Configuration (0 auto: the device's layout; 1
    /// stereo; 2 5.1); where it won't take the layout asked, its own.
    pub fn open(config: u8) -> Self {
        let want = match config {
            1 => Some(2u16),
            2 => Some(6),
            _ => None,
        };
        let asked = rodio::DeviceSinkBuilder::from_default_device().and_then(|b| match want {
            Some(c) => b.with_channels(NonZero::new(c).unwrap()).open_stream(),
            None => b.open_stream(),
        });
        let sink = asked
            .or_else(|e| {
                warn!("audio: the device won't open as asked ({e}); its own layout");
                rodio::DeviceSinkBuilder::open_default_sink()
            })
            .map(|mut s| {
                s.log_on_drop(false);
                s
            })
            .inspect_err(|e| warn!("audio: no output ({e})"))
            .ok();
        let channels = sink.as_ref().map(|s| s.config().channel_count().get() as usize).unwrap_or(2);
        info!("audio: output {} channels ({})", channels, ["auto", "stereo", "5.1"][config.min(2) as usize]);
        Output { sink, config, channels }
    }

    /// Start a sound through a panner set to `gains` (a world sound: its mono mix at each
    /// speaker's gain; else its channels on the front pair).
    pub fn play(&self, source: &AudioSource, looping: bool, world: bool, gains: [f32; MAX_CH], volume: f32) -> Option<Voice> {
        let mixer = self.sink.as_ref()?.mixer();
        let pan = Arc::new(Pan::default());
        pan.set(&gains);
        let player = rodio::Player::connect_new(mixer);
        player.set_volume(volume);
        let dec = source.decoder();
        if looping {
            player.append(Panned::new(dec.repeat_infinite(), self.channels, world, pan.clone(), gains));
        } else {
            player.append(Panned::new(dec, self.channels, world, pan.clone(), gains));
        }
        Some(Voice { player, pan })
    }
}

/// A sound playing on the output: its player (volume, speed) and its panner's gains.
#[derive(Component)]
pub struct Voice {
    pub player: rodio::Player,
    pub pan: Arc<Pan>,
}

/// The gains a panner moves to, one a speaker (shared with the audio thread).
#[derive(Default)]
pub struct Pan([AtomicU32; MAX_CH]);

impl Pan {
    pub fn set(&self, gains: &[f32; MAX_CH]) {
        for (a, g) in self.0.iter().zip(gains) {
            a.store(g.to_bits(), Ordering::Relaxed);
        }
    }
    fn get(&self, k: usize) -> f32 {
        f32::from_bits(self.0[k].load(Ordering::Relaxed))
    }
}

/// The speakers' directions (degrees clockwise from ahead) in an output's channel order; None for
/// those a world sound isn't set on (centre, LFE).
fn layout(channels: usize) -> &'static [Option<f32>] {
    match channels {
        1 => &[Some(0.0)],
        2 => &[Some(-90.0), Some(90.0)],
        4 => &[Some(-45.0), Some(45.0), Some(-135.0), Some(135.0)],
        6 => &[Some(-30.0), Some(30.0), None, None, Some(-110.0), Some(110.0)],
        8 => &[Some(-30.0), Some(30.0), None, None, Some(-150.0), Some(150.0), Some(-90.0), Some(90.0)],
        _ => &[Some(-90.0), Some(90.0)],
    }
}

/// A world sound's gains at each speaker, for a listener and the sound's place: the pair of
/// speakers about its direction share it at constant power; within `near` of the listener it
/// spreads evenly over them all.
pub fn world_gains(channels: usize, listener: &GlobalTransform, at: Vec3) -> [f32; MAX_CH] {
    let mut g = [0.0; MAX_CH];
    let speakers = layout(channels);
    let used: Vec<(usize, f32)> = speakers.iter().enumerate().filter_map(|(k, a)| a.map(|a| (k, a))).collect();
    if used.len() == 1 {
        g[used[0].0] = 1.0;
        return g;
    }
    let local = listener.affine().inverse().transform_point3(at);
    let flat = Vec2::new(local.x, -local.z);
    let dist = local.length();
    // clockwise from ahead, -180..180
    let az = flat.x.atan2(flat.y).to_degrees();
    let mut ring = used.clone();
    ring.sort_by(|a, b| a.1.total_cmp(&b.1));
    // the pair about it (round the back: the last and the first)
    let n = ring.len();
    let (a, b, t) = match ring.windows(2).find(|w| az >= w[0].1 && az < w[1].1) {
        Some(w) => (w[0], w[1], (az - w[0].1) / (w[1].1 - w[0].1)),
        None => {
            let (l, f) = (ring[n - 1], ring[0]);
            let off = if az >= l.1 { az - l.1 } else { az + 360.0 - l.1 };
            (l, f, off / (f.1 + 360.0 - l.1))
        }
    };
    let th = t.clamp(0.0, 1.0) * std::f32::consts::FRAC_PI_2;
    g[a.0] = th.cos();
    g[b.0] = th.sin();
    // close by: spread over them all
    let near = 1.5;
    let s = (1.0 - dist / near).clamp(0.0, 1.0);
    if s > 0.0 {
        let even = (1.0 / used.len() as f32).sqrt();
        for (k, _) in &used {
            g[*k] = ((1.0 - s) * g[*k] * g[*k] + s * even * even).sqrt();
        }
    }
    g
}

/// A screen sound's gains: every channel it has, as is.
pub fn screen_gains() -> [f32; MAX_CH] {
    [1.0; MAX_CH]
}

/// A sound's samples laid out on the output's channels, its gains eased to the panner's
/// (about 20 ms) so a moving sound doesn't click.
struct Panned<S: Source> {
    inner: S,
    out_ch: usize,
    world: bool,
    pan: Arc<Pan>,
    frame: [f32; MAX_CH],
    in_ch: usize,
    mono: f32,
    cur: [f32; MAX_CH],
    k: usize,
    ease: f32,
    /// a world sound's reverb, its ring this frame, and (its sound over) how much tail is left
    verb: Option<Box<Verb>>,
    wet: f32,
    tail: Option<f32>,
}

impl<S: Source> Panned<S> {
    fn new(inner: S, out_ch: usize, world: bool, pan: Arc<Pan>, gains: [f32; MAX_CH]) -> Self {
        let rate = inner.sample_rate().get() as f32;
        let verb = world.then(|| Box::new(Verb::new(rate)));
        Panned { out_ch: out_ch.clamp(1, MAX_CH), world, pan, frame: [0.0; MAX_CH], in_ch: 1, mono: 0.0, cur: gains, k: usize::MAX, ease: 1.0 - (-1.0 / (0.02 * rate)).exp(), inner, verb, wet: 0.0, tail: None }
    }
}

impl<S: Source> Iterator for Panned<S> {
    type Item = rodio::Sample;

    fn next(&mut self) -> Option<rodio::Sample> {
        if self.k >= self.out_ch {
            // the next frame in
            self.in_ch = (self.inner.channels().get() as usize).max(1);
            let mut sum = 0.0;
            for c in 0..self.in_ch {
                let v = match self.tail.is_some().then_some(0.0).or_else(|| self.inner.next()) {
                    Some(v) => v,
                    None if c == 0 => {
                        // the sound is over: its reverb rings on for the room's decay
                        let (decay, _, wet) = ROOM_REVERB.get();
                        if self.verb.is_none() || wet <= 0.0 || decay <= 0.0 {
                            return None;
                        }
                        self.tail = Some(decay.min(4.0) * self.verb.as_ref().map(|v| v.rate).unwrap_or(48000.0));
                        0.0
                    }
                    None => 0.0,
                };
                if c < MAX_CH {
                    self.frame[c] = v;
                }
                sum += v;
            }
            self.mono = sum / self.in_ch as f32;
            if let Some(t) = &mut self.tail {
                *t -= 1.0;
                if *t <= 0.0 {
                    return None;
                }
            }
            // (the room's ring, spread over the speakers about the listener)
            if let Some(v) = &mut self.verb {
                let (decay, damp, wet) = ROOM_REVERB.get();
                let level = self.cur.iter().take(self.out_ch).fold(0.0f32, |a, g| a.max(*g));
                self.wet = if wet > 0.0 && decay > 0.0 { v.run(self.mono * level, decay, damp) * wet } else { 0.0 };
            }
            for k in 0..self.out_ch {
                self.cur[k] += (self.pan.get(k) - self.cur[k]) * self.ease;
            }
            self.k = 0;
        }
        let k = self.k;
        self.k += 1;
        let v = if self.world {
            // (the ring is diffuse: the same at every speaker but the centre and LFE)
            let diffuse = if self.out_ch >= 6 && (k == 2 || k == 3) { 0.0 } else { 1.0 };
            return Some(if self.tail.is_some() { 0.0 } else { self.mono * self.cur[k] } + self.wet * diffuse);
        } else if self.out_ch == 1 {
            self.mono
        } else if self.in_ch == 1 {
            // a mono screen sound: the front pair
            if k < 2 { self.frame[0] } else { 0.0 }
        } else if k < self.in_ch.min(MAX_CH) {
            self.frame[k]
        } else {
            0.0
        };
        Some(v * self.cur[k])
    }
}

impl<S: Source> Source for Panned<S> {
    fn current_span_len(&self) -> Option<usize> {
        None
    }
    fn channels(&self) -> rodio::ChannelCount {
        NonZero::new(self.out_ch as u16).unwrap()
    }
    fn sample_rate(&self) -> rodio::SampleRate {
        self.inner.sample_rate()
    }
    fn total_duration(&self) -> Option<std::time::Duration> {
        self.inner.total_duration()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reverb_decays() {
        // an impulse rings, and falls about 60 dB over the decay
        let rate = 48000.0;
        let mut v = Verb::new(rate);
        let energy = |v: &mut Verb, first: f32, secs: f32| -> f32 {
            let mut e = 0.0;
            for i in 0..(secs * rate) as usize {
                let y = v.run(if i == 0 { first } else { 0.0 }, 2.0, 0.3);
                e += y * y;
            }
            e
        };
        let early = energy(&mut v, 1.0, 0.5);
        let mid = energy(&mut v, 0.0, 0.5);
        let _ = energy(&mut v, 0.0, 1.0);
        let late = energy(&mut v, 0.0, 0.5);
        assert!(early > 0.0 && mid > 0.0);
        assert!(late < early * 1e-4, "early {early} late {late}");
    }

    fn at(channels: usize, x: f32, z: f32) -> [f32; MAX_CH] {
        // a listener at the origin looking down -Z
        world_gains(channels, &GlobalTransform::IDENTITY, Vec3::new(x, 0.0, z))
    }

    fn power(g: &[f32; MAX_CH]) -> f32 {
        g.iter().map(|v| v * v).sum()
    }

    #[test]
    fn stereo() {
        let ahead = at(2, 0.0, -10.0);
        assert!((ahead[0] - ahead[1]).abs() < 1e-4);
        let right = at(2, 10.0, 0.0);
        assert!(right[1] > 0.99 && right[0] < 0.05);
        let left = at(2, -10.0, -1.0);
        assert!(left[0] > left[1]);
        for g in [ahead, right, left, at(2, 3.0, 8.0)] {
            assert!((power(&g) - 1.0).abs() < 1e-3);
        }
    }

    #[test]
    fn five_one() {
        // FL FR FC LFE BL BR
        let ahead = at(6, 0.0, -10.0);
        assert!((ahead[0] - ahead[1]).abs() < 1e-4 && ahead[2] == 0.0 && ahead[3] == 0.0);
        let behind_left = at(6, -9.0, 5.0);
        assert!(behind_left[4] > 0.9);
        let right = at(6, 10.0, 0.0);
        assert!(right[1] > 0.3 && right[5] > 0.3 && right[0] == 0.0);
        let behind = at(6, 0.0, 10.0);
        assert!((behind[4] - behind[5]).abs() < 1e-4 && behind[4] > 0.6);
        // close by: over all four
        let close = at(6, 0.0, -0.1);
        assert!(close[4] > 0.4 && close[0] > 0.4);
        for g in [ahead, behind_left, right, behind, close] {
            assert!((power(&g) - 1.0).abs() < 1e-3);
        }
    }
}
