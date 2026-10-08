//! Sound: the original Wwise events, cooked by `dhcook::audio` into `cache/audio`
//! (`index.json`: what each event plays; Ogg/WAV media files).
//!
//! An event plays each of its "play" entries: one random variation, or all media together
//! (music segments are layered tracks). Positional sounds fade out with distance up to the
//! reach of their Wwise attenuation; music replaces the current music. Placed ambient
//! emitters (`scene.ambient_sounds`) play while the player is within their reach.
//! Gameplay and the level scripts post events with [`PostEvent`]. The sounds play on the
//! output the Speaker Configuration opens (`speakers.rs`), set among its speakers.

use crate::level::{LevelInfo, LevelSpawnSet};
use crate::player::PlayerCamera;
use crate::speakers::{screen_gains, world_gains, Output, Voice};
use crate::GameState;
use bevy::audio::{AudioSource, SpatialListener};
use bevy::prelude::*;
use dhcook::audio::{AudioIndex, EventDef, PlayDef, PlayNode, RtpcCurve};
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

pub struct GameAudioPlugin;

impl Plugin for GameAudioPlugin {
    fn build(&self, app: &mut App) {
        app.add_message::<PostEvent>().add_message::<StopEvent>()
            .init_resource::<AudioHandles>()
            .init_resource::<Ambients>()
            .init_resource::<WwiseStates>()
            .init_resource::<TimedSounds>()
            .init_resource::<Music>()
            .init_resource::<Rtpcs>()
            .add_systems(Startup, |mut commands: Commands, settings: Res<crate::settings::Settings>| commands.insert_resource(Output::open(settings.speaker_config)))
            .add_systems(OnEnter(GameState::InGame), (reset_audio_state, setup_ambients.after(LevelSpawnSet)))
            .add_systems(Update, apply_speakers)
            .add_systems(
                Update,
                (attach_listener, run_ambients, run_timed, play_events, advance_playlists, finish_voices, stop_events, run_fades, attenuate, pause_voices).chain().after(apply_speakers).run_if(in_state(GameState::InGame)),
            );
    }
}

/// Master volume of the original mix (Wwise buses add their own levels).
const MASTER: f32 = 0.8;
/// Reach of positional sounds without an attenuation (metres).
const DEFAULT_REACH: f32 = 25.0;

static INDEX: OnceLock<Option<Arc<Library>>> = OnceLock::new();

struct Library {
    events: HashMap<u32, EventDef>,
    media: HashMap<u32, String>,
    durations: HashMap<u32, f32>,
}

fn library() -> Option<Arc<Library>> {
    INDEX
        .get_or_init(|| {
            let path = crate::loading::cache_dir().join("audio").join("index.json");
            let idx: AudioIndex = match std::fs::read(&path).map_err(anyhow::Error::from).and_then(|d| Ok(serde_json::from_slice(&d)?)) {
                Ok(i) => i,
                Err(e) => {
                    warn!("audio index {}: {e:#} (run `dhtool cook-audio`)", path.display());
                    return None;
                }
            };
            info!("audio: {} events, {} media", idx.events.len(), idx.media.len());
            Some(Arc::new(Library {
                events: idx.events.into_iter().map(|e| (e.id, e)).collect(),
                media: idx.media,
                durations: idx.durations,
            }))
        })
        .clone()
}

/// The Wwise id of an event name (or an `AkEvent` object path).
pub fn event_id(name: &str) -> u32 {
    dhcook::audio::fnv(name.rsplit('.').next().unwrap_or(name))
}

/// Events to post after a delay (animation notifies of procedurally animated objects).
#[derive(Resource, Default)]
pub struct TimedSounds(Vec<(f32, String, Option<Vec3>)>);

impl TimedSounds {
    /// Post `notifies` ((time, event)) from now on, at `at`.
    pub fn schedule(&mut self, notifies: &[(f32, String)], at: Option<Vec3>) {
        self.0.extend(notifies.iter().map(|(t, e)| (*t, e.clone(), at)));
    }
}

fn run_timed(time: Res<Time>, mut timed: ResMut<TimedSounds>, mut out: MessageWriter<PostEvent>) {
    let dt = time.delta_secs();
    timed.0.retain_mut(|(t, e, at)| {
        *t -= dt;
        if *t <= 0.0 {
            out.write(PostEvent::named(e, *at));
            false
        } else {
            true
        }
    });
}

/// Whether the cooked audio has an event of this name.
pub fn has_event(name: &str) -> bool {
    library().is_some_and(|l| l.events.contains_key(&event_id(name)))
}

/// How long an event plays (its longest medium), if known.
pub fn event_length(name: &str) -> Option<f32> {
    let lib = library()?;
    let e = lib.events.get(&event_id(name))?;
    e.plays.iter().flat_map(|p| p.media.iter()).filter_map(|m| lib.durations.get(m)).copied().reduce(f32::max)
}

static NAMES: std::sync::Mutex<Option<HashMap<u32, String>>> = std::sync::Mutex::new(None);

fn log_enabled() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var("DH_AUDIO_LOG").is_ok())
}

/// The name an event was posted by (with `DH_AUDIO_LOG`).
fn event_name(id: u32) -> String {
    NAMES.lock().unwrap().as_ref().and_then(|m| m.get(&id).cloned()).unwrap_or_else(|| format!("{id:#x}"))
}

/// Play a Wwise event (at a position, or non-positional).
#[derive(Message, Clone, Debug)]
pub struct PostEvent {
    pub event: u32,
    pub at: Option<Vec3>,
    /// the character saying it (its jaw follows the line)
    pub speaker: Option<Entity>,
    /// a spoken line (the voice volume)
    pub voice: bool,
}

impl PostEvent {
    pub fn named(name: &str, at: Option<Vec3>) -> Self {
        let event = event_id(name);
        if log_enabled() {
            NAMES.lock().unwrap().get_or_insert_with(HashMap::new).entry(event).or_insert_with(|| name.to_string());
        }
        PostEvent { event, at, speaker: None, voice: false }
    }

    /// Said by a character.
    pub fn spoken_by(mut self, speaker: Entity) -> Self {
        self.speaker = Some(speaker);
        self.voice = true;
        self
    }

    /// A spoken line.
    pub fn voiced(mut self) -> Self {
        self.voice = true;
        self
    }
}

/// The media read so far.
#[derive(Resource, Default)]
struct AudioHandles(HashMap<u32, Option<AudioSource>>);

impl AudioHandles {
    fn get(&mut self, lib: &Library, media: u32) -> Option<AudioSource> {
        self.0
            .entry(media)
            .or_insert_with(|| {
                let rel = lib.media.get(&media)?;
                let bytes = std::fs::read(crate::loading::cache_dir().join(rel)).ok()?;
                Some(AudioSource { bytes: Arc::from(bytes) })
            })
            .clone()
    }
}

/// A sound that goes once it has played (a playlist's own goes on to the next).
#[derive(Component)]
struct OneShot;

fn finish_voices(mut commands: Commands, q: Query<(Entity, &Voice), With<OneShot>>, all: Query<&Voice>, time: Res<Time>, mut log_t: Local<f32>) {
    for (e, v) in &q {
        if v.player.empty() {
            commands.entity(e).despawn();
        }
    }
    // (DH_AUDIO_LOG: how many sound, and how far the furthest has played, each second)
    *log_t += time.delta_secs();
    if *log_t > 1.0 && log_enabled() {
        *log_t = 0.0;
        let far = all.iter().map(|v| v.player.get_pos().as_secs_f32()).fold(0.0, f32::max);
        info!("audio: {} voices, furthest {far:.1}s", all.iter().len());
    }
}

/// The game paused (a menu over it): what was sounding pauses (the original's `Pause_All`), and
/// goes on as it resumes (`Resume_All`); what starts meanwhile (the menus' own) plays.
fn pause_voices(time: Res<Time<Virtual>>, mut held: Local<Option<Vec<Entity>>>, voices: Query<(Entity, &Voice)>) {
    match (time.is_paused(), held.is_some()) {
        (true, false) => {
            let mut list = Vec::new();
            for (e, v) in &voices {
                if !v.player.is_paused() {
                    v.player.pause();
                    list.push(e);
                }
            }
            *held = Some(list);
        }
        (false, true) => {
            for e in held.take().unwrap_or_default() {
                if let Ok((_, v)) = voices.get(e) {
                    v.player.play();
                }
            }
        }
        _ => {}
    }
}

/// A new Speaker Configuration: the output opened again for it, and what was sounding started on
/// it (the ambient emitters in reach, the music).
#[allow(clippy::too_many_arguments)]
fn apply_speakers(
    mut commands: Commands,
    settings: Res<crate::settings::Settings>,
    out: Option<ResMut<Output>>,
    emitters: Query<Entity, With<Emitter>>,
    mut ambients: ResMut<Ambients>,
    music: Res<Music>,
    mut posts: MessageWriter<PostEvent>,
) {
    let Some(mut out) = out else { return };
    if out.config == settings.speaker_config {
        return;
    }
    *out = Output::open(settings.speaker_config);
    for e in &emitters {
        commands.entity(e).despawn();
    }
    for a in &mut ambients.list {
        a.playing = false;
        a.retrigger = 0.0;
    }
    if let Some((event, _, _)) = music.current {
        posts.write(PostEvent { event, at: None, speaker: None, voice: false });
    }
}

/// The game parameters (RTPCs) the game sets, by Wwise id (the original's `SetRTPCValue`:
/// Corvo's breath, `Player_Sprint_Breath`). The sounds' curves on them (`PlayDef::rtpcs`) set
/// their volume and pitch; on parameters the game never sets they stay neutral.
/// `TimeDilation` is set for each sound (`UDisAkComponent`): the world's time scale for the
/// world's sounds, Corvo's for his own.
#[derive(Resource, Default)]
pub struct Rtpcs(pub HashMap<u32, f32>);

impl Rtpcs {
    pub fn set(&mut self, name: &str, value: f32) {
        self.0.insert(dhcook::audio::fnv(name), value);
    }
}

/// What a sound's RTPC curves make of its volume (linear) and speed, at the parameters' values:
/// volume curves (`AkRTPC_ParameterID` 0) multiply, a dB-scaled one's value in its curve
/// editor's space (amplitude over -96.3..0: `AkCurveScaling_dB_96_3`), an unscaled one's in dB;
/// pitch curves (2) add up in cents.
fn rtpc_mods(curves: &[RtpcCurve], value: impl Fn(u32) -> Option<f32>) -> (f32, f32) {
    let (mut gain, mut cents) = (1.0, 0.0);
    for c in curves {
        let Some(x) = value(c.rtpc) else { continue };
        let y = c.eval(x);
        match c.param {
            0 if c.scaling == 0 => gain *= db(y),
            0 => gain *= (1.0 + y / 96.3).max(0.0),
            2 => cents += y,
            _ => {}
        }
    }
    (gain, 2f32.powf(cents / 1200.0))
}

/// Stop (fade out) the sounds an event started.
#[derive(Message, Clone, Copy)]
pub struct StopEvent(pub u32);

fn stop_events(mut msgs: MessageReader<StopEvent>, mut q: Query<&mut Emitter>) {
    for StopEvent(ev) in msgs.read() {
        for mut em in &mut q {
            if em.event == *ev {
                em.playlist = None;
                em.fade_rate = -4.0;
            }
        }
    }
}

/// A playing sound.
#[derive(Component)]
pub struct Emitter {
    pub event: u32,
    gain: f32,
    reach: f32,
    positional: bool,
    pub music: bool,
    /// a spoken line
    voice: bool,
    /// the ambient emitter that started it
    ambient: Option<usize>,
    /// a looping random container: the variations to continue with (and the next one,
    /// for sequences), and the base gain
    playlist: Option<(Arc<Vec<PlayNode>>, bool, usize, f32)>,
    /// fade level (0..1) and its rate per second (negative: fading out, then stops)
    fade: f32,
    fade_rate: f32,
    /// its RTPC curves
    curves: Arc<Vec<RtpcCurve>>,
}

/// Music crossfade length (seconds).
const FADE: f32 = 1.5;

/// Current values of the Wwise state groups (states are global).
#[derive(Resource, Default)]
pub struct WwiseStates(pub HashMap<u32, u32>);

/// The music: the event (and play entry) that started it and the state cases it resolved
/// to. A state change that selects other cases switches the music.
#[derive(Resource, Default)]
struct Music {
    current: Option<(u32, usize, Vec<(u32, u32)>)>,
}

/// The node a switch selects for the current states (None: no case, silence).
fn case<'a>(group: u32, default: u32, cases: &'a [(u32, PlayNode)], states: &HashMap<u32, u32>) -> Option<&'a PlayNode> {
    let s = states.get(&group).copied().unwrap_or(default);
    cases.iter().find(|(k, _)| *k == s).map(|(_, n)| n)
}

/// Follow the switches at the top of a tree; `used` records the cases taken.
fn resolve<'a>(mut node: &'a PlayNode, states: &HashMap<u32, u32>, used: &mut Vec<(u32, u32)>) -> Option<&'a PlayNode> {
    for _ in 0..8 {
        match node {
            PlayNode::Switch { group, default, cases } => {
                used.push((*group, states.get(group).copied().unwrap_or(*default)));
                node = case(*group, *default, cases, states)?;
            }
            _ => return Some(node),
        }
    }
    None
}

/// Choose what a node plays now: (media, volume offset dB).
fn pick(node: &PlayNode, states: &HashMap<u32, u32>, out: &mut Vec<(u32, f32)>) {
    match node {
        PlayNode::Media(m, db) => out.push((*m, *db)),
        PlayNode::Random(c, _) => {
            if !c.is_empty() {
                pick(&c[rand::random_range(0..c.len())], states, out);
            }
        }
        PlayNode::Layer(c) => c.iter().for_each(|n| pick(n, states, out)),
        PlayNode::Switch { group, default, cases } => {
            if let Some(n) = case(*group, *default, cases, states) {
                pick(n, states, out);
            }
        }
    }
}

fn db(v: f32) -> f32 {
    10f32.powf(v / 20.0)
}

fn attach_listener(mut commands: Commands, cams: Query<Entity, (With<PlayerCamera>, Without<SpatialListener>)>) {
    for e in &cams {
        commands.entity(e).insert(SpatialListener::new(0.25));
    }
}

/// What starts a play entry.
struct Start {
    event: u32,
    at: Option<Vec3>,
    ambient: Option<usize>,
    fade_in: bool,
    speaker: Option<Entity>,
    voice: bool,
}

/// Start one "play" entry of an event; returns the state cases it depends on.
#[allow(clippy::too_many_arguments)]
fn start_play(
    commands: &mut Commands,
    lib: &Library,
    handles: &mut AudioHandles,
    out: &Output,
    ear: Option<&GlobalTransform>,
    states: &HashMap<u32, u32>,
    play: &PlayDef,
    s: Start,
) -> Vec<(u32, u32)> {
    let mut used = Vec::new();
    // older indices: the flat media list
    let legacy;
    let root = match &play.tree {
        Some(t) => match resolve(t, states, &mut used) {
            Some(n) => n,
            None => return used,
        },
        None => {
            let m = play.media.iter().map(|&m| PlayNode::Media(m, 0.0)).collect();
            legacy = if play.layered { PlayNode::Layer(m) } else { PlayNode::Random(m, false) };
            &legacy
        }
    };
    let continues = play.looping || play.music;
    // a looping random container moves on to another variation when one ends
    let mut playlist = match root {
        PlayNode::Random(c, seq) if continues && c.len() > 1 => Some((Arc::new(c.clone()), *seq, 1, db(play.volume_db) * MASTER)),
        _ => None,
    };
    let mut chosen = Vec::new();
    match &playlist {
        Some((c, true, _, _)) => pick(&c[0], states, &mut chosen),
        _ => pick(root, states, &mut chosen),
    }
    // a character's line: its face plays the line's FaceFX animation (`facefx.rs`), else its
    // jaw follows the line's loudness
    if let (Some(sp), Some(&(media, _))) = (s.speaker, chosen.first()) {
        commands.entity(sp).try_insert(Speaking { media, event: s.event, t: 0.0 });
    }
    let positional = s.at.is_some() && !play.music;
    let reach = if play.max_distance > 0.5 { play.max_distance } else { DEFAULT_REACH };
    let looping = continues && playlist.is_none();
    let curves = Arc::new(play.rtpcs.clone());
    let at = s.at.unwrap_or(Vec3::ZERO);
    let gains = match ear {
        Some(l) if positional => world_gains(out.channels, l, at),
        _ => screen_gains(),
    };
    for (media, offset) in chosen {
        let Some(src) = handles.get(lib, media) else { continue };
        let gain = db(play.volume_db + offset) * MASTER;
        // only one of a layered variation carries the playlist on
        let pl = playlist.take();
        // (its volume set as it's attenuated, below)
        let Some(voice) = out.play(&src, looping, positional, gains, 0.0) else { continue };
        let one_shot = !looping && pl.is_none();
        let mut ec = commands.spawn((
            voice,
            Transform::from_translation(at),
            Emitter {
                event: s.event,
                gain,
                reach,
                positional,
                music: play.music,
                voice: s.voice,
                ambient: s.ambient,
                playlist: pl,
                fade: if s.fade_in { 0.0 } else { 1.0 },
                fade_rate: if s.fade_in { 1.0 / FADE } else { 0.0 },
                curves: curves.clone(),
            },
            DespawnOnExit(GameState::InGame),
        ));
        if one_shot {
            ec.insert(OneShot);
        }
    }
    used
}

fn fade_out_music(emitters: &mut Query<&mut Emitter>) {
    for mut em in emitters.iter_mut() {
        if em.music && em.fade_rate >= 0.0 {
            em.fade_rate = -1.0 / FADE;
            em.playlist = None;
        }
    }
}

fn reset_audio_state(mut states: ResMut<WwiseStates>, mut music: ResMut<Music>, mut timed: ResMut<TimedSounds>) {
    states.0.clear();
    timed.0.clear();
    music.current = None;
}

#[allow(clippy::too_many_arguments)]
fn play_events(
    mut commands: Commands,
    mut msgs: MessageReader<PostEvent>,
    mut handles: ResMut<AudioHandles>,
    (out, listener): (Res<Output>, Query<&GlobalTransform, With<SpatialListener>>),
    mut emitters: Query<&mut Emitter>,
    mut ambients: ResMut<Ambients>,
    (mut states, mut music): (ResMut<WwiseStates>, ResMut<Music>),
) {
    let Some(lib) = library() else {
        msgs.clear();
        return;
    };
    let mut posts: Vec<(PostEvent, Option<usize>)> = msgs.read().cloned().map(|m| (m, None)).collect();
    posts.extend(std::mem::take(&mut ambients.requests).into_iter().map(|(m, a)| (m, Some(a))));
    let log = std::env::var("DH_AUDIO_LOG").is_ok();
    let mut changed = false;
    for (m, ambient) in posts {
        let Some(ev) = lib.events.get(&m.event) else {
            if log {
                info!("audio: no event {}", event_name(m.event));
            }
            continue;
        };
        if log && ambient.is_none() {
            info!(
                "audio: event {} at {:?}: {:?} states {:x?}",
                event_name(m.event),
                m.at,
                ev.plays.iter().map(|p| (p.media.len(), p.music, p.looping, p.max_distance)).collect::<Vec<_>>(),
                ev.states
            );
        }
        for &(g, s) in &ev.states {
            changed |= states.0.insert(g, s) != Some(s);
        }
        for (k, play) in ev.plays.iter().enumerate() {
            let fade_in = play.music && music.current.is_some();
            if play.music {
                // one music at a time
                fade_out_music(&mut emitters);
            }
            let used = start_play(&mut commands, &lib, &mut handles, &out, listener.single().ok(), &states.0, play, Start { event: m.event, at: m.at, ambient, fade_in, speaker: m.speaker, voice: m.voice });
            if play.music {
                music.current = Some((m.event, k, used));
            }
        }
    }
    // the music follows its states
    if changed {
        if let Some((event, k, used)) = music.current.clone() {
            if let Some(play) = lib.events.get(&event).and_then(|e| e.plays.get(k)) {
                let mut now = Vec::new();
                if let Some(t) = &play.tree {
                    resolve(t, &states.0, &mut now);
                }
                if now != used {
                    if log {
                        info!("audio: music {event:#x} switches {used:x?} -> {now:x?}");
                    }
                    fade_out_music(&mut emitters);
                    let used = start_play(&mut commands, &lib, &mut handles, &out, listener.single().ok(), &states.0, play, Start { event, at: None, ambient: None, fade_in: true, speaker: None, voice: false });
                    music.current = Some((event, k, used));
                }
            }
        }
    }
}

/// Continue looping random containers with a new variation.
fn advance_playlists(
    mut commands: Commands,
    mut handles: ResMut<AudioHandles>,
    (out, listener): (Res<Output>, Query<&GlobalTransform, With<SpatialListener>>),
    states: Res<WwiseStates>,
    q: Query<(Entity, &Emitter, &Transform, &Voice)>,
) {
    let Some(lib) = library() else { return };
    for (e, em, t, voice) in &q {
        let Some((list, seq, next, base)) = &em.playlist else { continue };
        if !voice.player.empty() {
            continue;
        }
        commands.entity(e).despawn();
        let i = if *seq { next % list.len() } else { rand::random_range(0..list.len()) };
        let mut chosen = Vec::new();
        pick(&list[i], &states.0, &mut chosen);
        let base = *base;
        let mut pl = Some((list.clone(), *seq, i + 1, base));
        let gains = match listener.single() {
            Ok(l) if em.positional => world_gains(out.channels, l, t.translation),
            _ => screen_gains(),
        };
        for (media, offset) in chosen {
            let Some(src) = handles.get(&lib, media) else { continue };
            let p = pl.take();
            let Some(voice) = out.play(&src, false, em.positional, gains, 0.0) else { continue };
            let one_shot = p.is_none();
            let mut ec = commands.spawn((
                voice,
                *t,
                Emitter {
                    event: em.event,
                    gain: base * db(offset),
                    reach: em.reach,
                    positional: em.positional,
                    music: em.music,
                    voice: em.voice,
                    ambient: em.ambient,
                    playlist: p,
                    fade: em.fade,
                    fade_rate: em.fade_rate,
                    curves: em.curves.clone(),
                },
                DespawnOnExit(GameState::InGame),
            ));
            if one_shot {
                ec.insert(OneShot);
            }
        }
    }
}

/// Fade-ins and fade-outs (fully faded out sounds stop).
fn run_fades(time: Res<Time>, mut commands: Commands, mut q: Query<(Entity, &mut Emitter)>) {
    let dt = time.delta_secs();
    for (e, mut em) in &mut q {
        if em.fade_rate == 0.0 {
            continue;
        }
        em.fade += em.fade_rate * dt;
        if em.fade >= 1.0 {
            em.fade = 1.0;
            em.fade_rate = 0.0;
        } else if em.fade <= 0.0 {
            commands.entity(e).despawn();
        }
    }
}

/// Each sound's volume (its gain, the distance falloff of a positional one — a smooth fade to
/// silence at its reach —, its RTPC curves, fades, its category's volume and the master), its
/// speed (the curves' pitch) and, in the world, where among the speakers it sounds.
#[allow(clippy::type_complexity)]
fn attenuate(
    settings: Res<crate::settings::Settings>,
    rtpcs: Res<Rtpcs>,
    tc: Res<crate::gameplay::TimeControl>,
    out: Res<Output>,
    listener: Query<&GlobalTransform, With<SpatialListener>>,
    voices: Query<(&Transform, &Emitter, &Voice)>,
    (time, mut log_t): (Res<Time>, Local<f32>),
) {
    let Ok(l) = listener.single() else { return };
    let ear = l.translation();
    // (DH_RTPC_LOG: the sounds the curves change, each second)
    *log_t += time.delta_secs();
    let log = *log_t > 1.0 && std::env::var("DH_RTPC_LOG").is_ok();
    if log {
        *log_t = 0.0;
    }
    // `TimeDilation` for the world's sounds and for Corvo's (his time isn't bent)
    let dilation = dhcook::audio::fnv("TimeDilation");
    let (world, own) = (tc.world_scale(), tc.finisher.min(1.0) * tc.wheel);
    let mods = |em: &Emitter| {
        if em.curves.is_empty() {
            return (1.0, 1.0);
        }
        rtpc_mods(&em.curves, |id| if id == dilation { Some(if em.positional { world } else { own }) } else { rtpcs.0.get(&id).copied() })
    };
    for (t, em, voice) in &voices {
        let cat = if em.music { settings.music_volume } else if em.voice { settings.voice_volume } else { settings.sfx_volume };
        let (g, speed) = mods(em);
        if log && ((g - 1.0).abs() > 1e-3 || (speed - 1.0).abs() > 1e-3) {
            info!("rtpc: {} gain {g:.3} speed {speed:.3}", event_name(em.event));
        }
        let mut v = em.gain * g * em.fade.clamp(0.0, 1.0) * cat * settings.master_volume;
        if em.positional {
            let x = (1.0 - t.translation.distance(ear) / em.reach).clamp(0.0, 1.0);
            v *= x * x;
            voice.pan.set(&world_gains(out.channels, l, t.translation));
        }
        if (voice.player.volume() - v).abs() > 1e-4 {
            voice.player.set_volume(v);
        }
        if (voice.player.speed() - speed).abs() > 1e-3 {
            voice.player.set_speed(speed);
        }
    }
}

/// Placed ambient emitters: started when the player comes within reach, stopped beyond.
#[derive(Resource, Default)]
pub struct Ambients {
    list: Vec<AmbientState>,
    requests: Vec<(PostEvent, usize)>,
    timer: f32,
}

struct AmbientState {
    event: u32,
    position: Vec3,
    reach: f32,
    enabled: bool,
    playing: bool,
    /// one-shot ambient sounds re-trigger after a pause
    retrigger: f32,
    looping: bool,
}

impl Ambients {
    /// Start (or stop) an ambient emitter (level scripts).
    pub fn set_enabled(&mut self, index: usize, on: bool) {
        if let Some(a) = self.list.get_mut(index) {
            a.enabled = on;
        }
    }
    /// Add an emitter of the game's own (the song of a rune): it sounds while the listener
    /// is within the event's reach.
    pub fn add(&mut self, event: &str, position: Vec3) -> usize {
        let id = event_id(event);
        let lib = library();
        let ev = lib.as_ref().and_then(|l| l.events.get(&id));
        let reach = ev.and_then(|e| e.plays.iter().map(|p| p.max_distance).reduce(f32::max)).filter(|r| *r > 0.5).unwrap_or(DEFAULT_REACH);
        let looping = ev.is_some_and(|e| e.plays.iter().any(|p| p.looping));
        self.list.push(AmbientState { event: id, position, reach, enabled: true, playing: false, retrigger: 0.0, looping });
        self.list.len() - 1
    }
    pub fn len(&self) -> usize {
        self.list.len()
    }
}

fn setup_ambients(level: Option<Res<LevelInfo>>, mut ambients: ResMut<Ambients>) {
    let Some(level) = level else { return };
    let lib = library();
    ambients.list = level
        .scene
        .ambient_sounds
        .iter()
        .map(|a| {
            let id = event_id(&a.event);
            let ev = lib.as_ref().and_then(|l| l.events.get(&id));
            let reach = ev.and_then(|e| e.plays.iter().map(|p| p.max_distance).reduce(f32::max)).filter(|r| *r > 0.5).unwrap_or(DEFAULT_REACH);
            let looping = ev.is_some_and(|e| e.plays.iter().any(|p| p.looping));
            AmbientState { event: id, position: Vec3::from(a.position), reach, enabled: a.auto_play, playing: false, retrigger: 0.0, looping }
        })
        .collect();
    ambients.requests.clear();
    info!("audio: {} ambient emitters", ambients.list.len());
}

fn run_ambients(
    time: Res<Time>,
    mut commands: Commands,
    mut ambients: ResMut<Ambients>,
    listener: Query<&GlobalTransform, With<SpatialListener>>,
    playing: Query<(Entity, &Emitter)>,
) {
    let Ok(l) = listener.single() else { return };
    let ear = l.translation();
    let dt = time.delta_secs();
    ambients.timer -= dt;
    let check = ambients.timer <= 0.0;
    if check {
        ambients.timer = 0.5;
    }
    // which ambient emitters still sound
    let mut alive = vec![false; ambients.list.len()];
    for (_, em) in &playing {
        if let Some(i) = em.ambient {
            if let Some(a) = alive.get_mut(i) {
                *a = true;
            }
        }
    }
    let mut stop = Vec::new();
    let mut requests = Vec::new();
    for (i, a) in ambients.list.iter_mut().enumerate() {
        let near = a.enabled && a.position.distance(ear) < a.reach * 1.1;
        if a.playing && !alive[i] {
            // a one-shot finished: wait, then play again
            a.playing = false;
            a.retrigger = if a.looping { 0.0 } else { 3.0 + rand::random::<f32>() * 12.0 };
        }
        a.retrigger -= dt;
        if !check {
            continue;
        }
        if near && !a.playing && a.retrigger <= 0.0 {
            a.playing = true;
            requests.push((PostEvent { event: a.event, at: Some(a.position), speaker: None, voice: false }, i));
        } else if !near && a.playing {
            a.playing = false;
            stop.push(i);
        }
    }
    ambients.requests.extend(requests);
    for (e, em) in &playing {
        if em.ambient.is_some_and(|i| stop.contains(&i)) {
            commands.entity(e).despawn();
        }
    }
}

/// A character speaking a line: the clip and how far into it.
#[derive(Component)]
pub struct Speaking {
    pub media: u32,
    /// the line's voice event
    pub event: u32,
    pub t: f32,
}

static ENVELOPES: std::sync::OnceLock<HashMap<u32, Vec<u8>>> = std::sync::OnceLock::new();

/// A clip's loudness, frame by frame (`dhcook::audio::ENVELOPE_RATE`).
pub fn envelope(media: u32) -> Option<&'static [u8]> {
    ENVELOPES
        .get_or_init(|| {
            let mut out = HashMap::new();
            let Ok(d) = std::fs::read(crate::loading::cache_dir().join("audio").join("envelopes.bin")) else { return out };
            let u32_at = |p: usize| d.get(p..p + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]));
            let n = u32_at(0).unwrap_or(0) as usize;
            let mut p = 4;
            for _ in 0..n {
                let (Some(m), Some(len)) = (u32_at(p), u32_at(p + 4)) else { break };
                let len = len as usize;
                let Some(e) = d.get(p + 8..p + 8 + len) else { break };
                out.insert(m, e.to_vec());
                p += 8 + len;
            }
            out
        })
        .get(&media)
        .map(|v| v.as_slice())
}
