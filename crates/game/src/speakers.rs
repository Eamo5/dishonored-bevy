//! The sound output and its Speaker Configuration (`PSI_AudioPC_SpeakerConfiguration`:
//! `SpeakerConfiguration_Auto` / `_Stereo` / `_5Point1`). The game's sounds play on a stream of
//! that layout — the device's own (Auto), two channels or six (FL FR FC LFE BL BR) — each
//! through a panner: a sound in the world is set among the speakers by its direction from the
//! listener (pairwise constant power between the speakers about it, as Wwise's 3D positioning:
//! the front pair and the surrounds, the centre and LFE left to the screen's), spreading over
//! them all as it comes close; a sound of the screen's (2D) plays on the front pair.

use bevy::audio::{AudioSource, Decodable, Source};
use bevy::prelude::*;
use std::collections::{HashMap, HashSet};
use std::num::NonZero;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};

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

/// The output stream, as opened for the setting: the device's (none muted), and the game's own
/// mixer feeding it, its work timed.
#[derive(Resource)]
pub struct Output {
    sink: Option<rodio::MixerDeviceSink>,
    mixer: Option<rodio::mixer::Mixer>,
    /// a muted output's drain, stopped as it goes
    drain: Option<Arc<AtomicBool>>,
    /// the setting it was opened for
    pub config: u8,
    pub channels: usize,
}

impl Drop for Output {
    fn drop(&mut self) {
        if let Some(d) = &self.drain {
            d.store(false, Ordering::Relaxed);
        }
    }
}

/// The device went away (unplugged, switched, reset by Windows): the output opens again on the
/// default device (`audio::apply_speakers`), what was sounding started on it.
pub static OUTPUT_LOST: AtomicBool = AtomicBool::new(false);

/// What the device's stream reports going wrong: lost, it is opened again.
fn on_stream_error(e: rodio::cpal::StreamError) {
    warn!("audio: the output stream failed ({e}); opening it again");
    OUTPUT_LOST.store(true, Ordering::Relaxed);
}

/// The audio thread's time spent on the game's sounds (ns) for the samples it made: the share of
/// real time the mixing takes (`DH_AUDIO_LOG`).
static MIX_NS: AtomicU64 = AtomicU64::new(0);
static MIX_SAMPLES: AtomicU64 = AtomicU64::new(0);

/// The mixing's share of real time since last asked (0..; over 1 the sound breaks up).
pub fn mix_load(rate: u32, channels: usize) -> f32 {
    let ns = MIX_NS.swap(0, Ordering::Relaxed) as f64;
    let samples = MIX_SAMPLES.swap(0, Ordering::Relaxed) as f64;
    if samples <= 0.0 {
        return 0.0;
    }
    let real = samples / (rate as f64 * channels.max(1) as f64) * 1e9;
    (ns / real.max(1.0)) as f32
}

/// The game's mix as the device pulls it, its work timed (`DH_AUDIO_LOG` only: each sample's,
/// summed, as the device pulls in bursts).
struct Timed<S: Source> {
    inner: S,
    n: u32,
    ns: u64,
    on: bool,
}

impl<S: Source> Timed<S> {
    fn new(inner: S) -> Self {
        Timed { inner, n: 0, ns: 0, on: std::env::var("DH_AUDIO_LOG").is_ok() }
    }
}

impl<S: Source> Iterator for Timed<S> {
    type Item = rodio::Sample;
    fn next(&mut self) -> Option<rodio::Sample> {
        if !self.on {
            return self.inner.next();
        }
        let t = Instant::now();
        let v = self.inner.next();
        self.ns += t.elapsed().as_nanos() as u64;
        self.n += 1;
        if self.n >= 4096 {
            MIX_NS.fetch_add(std::mem::take(&mut self.ns), Ordering::Relaxed);
            MIX_SAMPLES.fetch_add(self.n as u64, Ordering::Relaxed);
            self.n = 0;
        }
        v
    }
}

impl<S: Source> Source for Timed<S> {
    fn current_span_len(&self) -> Option<usize> {
        self.inner.current_span_len()
    }
    fn channels(&self) -> rodio::ChannelCount {
        self.inner.channels()
    }
    fn sample_rate(&self) -> rodio::SampleRate {
        self.inner.sample_rate()
    }
    fn total_duration(&self) -> Option<Duration> {
        None
    }
}

/// Short sounds decoded once into memory (16-bit), so the audio thread only reads them: decoding
/// Vorbis as it plays is most of the mixing's work. Decoded off the main thread the first time
/// a sound plays (it plays from its file meanwhile); at most `PCM_MAX_SECS` long, `PCM_BUDGET`
/// samples in all.
struct Pcm {
    samples: Arc<[i16]>,
    channels: NonZero<u16>,
    rate: NonZero<u32>,
}

#[derive(Default)]
struct PcmCache {
    ready: HashMap<usize, Arc<Pcm>>,
    pending: HashSet<usize>,
    /// too long or not decodable: played from the file
    skip: HashSet<usize>,
    total: usize,
}

static PCM: LazyLock<Mutex<PcmCache>> = LazyLock::new(Default::default);
const PCM_MAX_SECS: f32 = 12.0;
const PCM_BUDGET: usize = 96 << 20;

/// A sound's samples from memory, once or round and round.
struct PcmSource {
    pcm: Arc<Pcm>,
    pos: usize,
    looping: bool,
}

impl Iterator for PcmSource {
    type Item = rodio::Sample;
    fn next(&mut self) -> Option<rodio::Sample> {
        if self.pos >= self.pcm.samples.len() {
            if !self.looping || self.pcm.samples.is_empty() {
                return None;
            }
            self.pos = 0;
        }
        let v = self.pcm.samples[self.pos];
        self.pos += 1;
        Some(v as f32 / 32768.0)
    }
}

impl Source for PcmSource {
    fn current_span_len(&self) -> Option<usize> {
        None
    }
    fn channels(&self) -> rodio::ChannelCount {
        self.pcm.channels
    }
    fn sample_rate(&self) -> rodio::SampleRate {
        self.pcm.rate
    }
    fn total_duration(&self) -> Option<Duration> {
        (!self.looping).then(|| Duration::from_secs_f64(self.pcm.samples.len() as f64 / (self.pcm.rate.get() as f64 * self.pcm.channels.get() as f64)))
    }
}

/// A sound's decoded samples if they are ready; else starts decoding them (off the main thread).
fn pcm_of(source: &AudioSource) -> Option<Arc<Pcm>> {
    let key = Arc::as_ptr(&source.bytes) as *const u8 as usize;
    let mut cache = PCM.lock().ok()?;
    if let Some(p) = cache.ready.get(&key) {
        return Some(p.clone());
    }
    if cache.pending.contains(&key) || cache.skip.contains(&key) || cache.total >= PCM_BUDGET {
        return None;
    }
    cache.pending.insert(key);
    let source = source.clone();
    bevy::tasks::AsyncComputeTaskPool::get()
        .spawn(async move {
            let dec = source.decoder();
            let (channels, rate) = (dec.channels(), dec.sample_rate());
            let cap = (PCM_MAX_SECS * rate.get() as f32 * channels.get() as f32) as usize;
            let mut samples: Vec<i16> = Vec::new();
            let mut whole = true;
            for v in dec {
                if samples.len() >= cap {
                    whole = false;
                    break;
                }
                samples.push((v.clamp(-1.0, 1.0) * 32767.0) as i16);
            }
            if let Ok(mut cache) = PCM.lock() {
                cache.pending.remove(&key);
                if whole && !samples.is_empty() {
                    cache.total += samples.len();
                    cache.ready.insert(key, Arc::new(Pcm { samples: samples.into(), channels, rate }));
                } else {
                    cache.skip.insert(key);
                }
            }
        })
        .detach();
    None
}

impl Output {
    /// Open the default device for a Speaker Configuration (0 auto: the device's layout; 1
    /// stereo; 2 5.1); where it won't take the layout asked, its own.
    pub fn open(config: u8) -> Self {
        // (DH_MUTE: nothing to the device, for unattended test runs; the sounds still play,
        // drained at their pace)
        if std::env::var("DH_MUTE").is_ok() {
            info!("audio: muted (DH_MUTE)");
            let (mixer, src) = rodio::mixer::mixer(NonZero::new(2).unwrap(), NonZero::new(48000).unwrap());
            let alive = Arc::new(AtomicBool::new(true));
            let on = alive.clone();
            std::thread::spawn(move || {
                let mut src = Timed::new(src);
                // (a fiftieth of a second at a time)
                while on.load(Ordering::Relaxed) {
                    let t = Instant::now();
                    for _ in 0..(48000 / 50 * 2) {
                        src.next();
                    }
                    std::thread::sleep(Duration::from_millis(20).saturating_sub(t.elapsed()));
                }
            });
            return Output { sink: None, mixer: Some(mixer), drain: Some(alive), config, channels: 2 };
        }
        let want = match config {
            1 => Some(2u16),
            2 => Some(6),
            _ => None,
        };
        let asked = rodio::DeviceSinkBuilder::from_default_device().and_then(|b| {
            let b = b.with_error_callback(on_stream_error as fn(rodio::cpal::StreamError));
            match want {
                Some(c) => b.with_channels(NonZero::new(c).unwrap()).open_stream(),
                None => b.open_stream(),
            }
        });
        let sink = asked
            .or_else(|e| {
                warn!("audio: the device won't open as asked ({e}); its own layout");
                rodio::DeviceSinkBuilder::from_default_device().and_then(|b| b.with_error_callback(on_stream_error as fn(rodio::cpal::StreamError)).open_sink_or_fallback())
            })
            .map(|mut s| {
                s.log_on_drop(false);
                s
            })
            .inspect_err(|e| warn!("audio: no output ({e})"))
            .ok();
        let channels = sink.as_ref().map(|s| s.config().channel_count().get() as usize).unwrap_or(2);
        info!("audio: output {} channels ({})", channels, ["auto", "stereo", "5.1"][config.min(2) as usize]);
        // (the game's sounds mixed apart and fed to the device's mixer, timed)
        let mixer = sink.as_ref().map(|s| {
            let (mixer, src) = rodio::mixer::mixer(s.config().channel_count(), s.config().sample_rate());
            s.mixer().add(Timed::new(src));
            mixer
        });
        Output { sink, mixer, drain: None, config, channels }
    }

    /// Whether it sounds on a device.
    pub fn live(&self) -> bool {
        self.sink.is_some()
    }

    /// The output's rate and channels, for its load.
    pub fn format(&self) -> (u32, usize) {
        (self.sink.as_ref().map_or(48000, |s| s.config().sample_rate().get()), self.channels)
    }

    /// Start a sound through a panner set to `gains` (a world sound: its mono mix at each
    /// speaker's gain; else its channels on the front pair).
    pub fn play(&self, source: &AudioSource, looping: bool, world: bool, gains: [f32; MAX_CH], volume: f32) -> Option<Voice> {
        let mixer = self.mixer.as_ref()?;
        let pan = Arc::new(Pan::default());
        pan.set(&gains);
        let player = rodio::Player::connect_new(mixer);
        player.set_volume(volume);
        // (from memory once decoded, else from its file)
        if let Some(pcm) = pcm_of(source) {
            player.append(Panned::new(PcmSource { pcm, pos: 0, looping }, self.channels, world, pan.clone(), gains));
        } else {
            let dec = source.decoder();
            if looping {
                player.append(Panned::new(dec.repeat_infinite(), self.channels, world, pan.clone(), gains));
            } else {
                player.append(Panned::new(dec, self.channels, world, pan.clone(), gains));
            }
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

    /// (benchmark, `--ignored`) N world voices of the cooked sounds mixed as the game mixes
    /// them: how long one second of output takes the audio thread.
    #[test]
    #[ignore]
    fn mix_cost() {
        let files: Vec<_> = std::fs::read_dir("../../cache/audio").map(|d| d.filter_map(|e| e.ok()).map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "ogg")).take(400).collect()).unwrap_or_default();
        let srcs: Vec<AudioSource> = files.iter().filter_map(|f| std::fs::read(f).ok()).filter(|b| b.len() > 20_000).take(48).map(|b| AudioSource { bytes: b.into() }).collect();
        for &(voices, ch) in &[(16usize, 2usize), (32, 2), (48, 2), (32, 6), (32, 8)] {
            let (mixer, mut out) = rodio::mixer::mixer(NonZero::new(ch as u16).unwrap(), NonZero::new(48000).unwrap());
            let mut players = Vec::new();
            for i in 0..voices {
                let p = rodio::Player::connect_new(&mixer);
                p.set_volume(0.5);
                p.append(Panned::new(srcs[i % srcs.len()].decoder().repeat_infinite(), ch, true, Arc::new(Pan::default()), [0.5; MAX_CH]));
                players.push(p);
            }
            let n = 48000 * ch;
            let t = std::time::Instant::now();
            let mut acc = 0.0f32;
            for _ in 0..n {
                acc += out.next().unwrap_or(0.0);
            }
            let ms = t.elapsed().as_secs_f64() * 1000.0;
            println!("{voices} voices, {ch} channels: {ms:.0} ms of work per second of sound ({acc:.1})");
        }
        // the parts of it, 32 voices: decoding alone, decoding and panning (no reverb), all
        let parts = |label: &str, f: &dyn Fn(&AudioSource) -> Box<dyn Iterator<Item = f32>>| {
            let t = std::time::Instant::now();
            let mut acc = 0.0;
            for i in 0..32 {
                let it = f(&srcs[i % srcs.len()]);
                acc += it.take(44100 * 2).sum::<f32>();
            }
            println!("32 voices, {label}: {:.0} ms ({acc:.1})", t.elapsed().as_secs_f64() * 1000.0);
        };
        parts("decode", &|s| Box::new(s.decoder().repeat_infinite()));
        parts("pan, no reverb", &|s| Box::new(Panned::new(s.decoder().repeat_infinite(), 2, false, Arc::new(Pan::default()), [0.5; MAX_CH])));
        parts("pan, reverb", &|s| Box::new(Panned::new(s.decoder().repeat_infinite(), 2, true, Arc::new(Pan::default()), [0.5; MAX_CH])));
    }

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
