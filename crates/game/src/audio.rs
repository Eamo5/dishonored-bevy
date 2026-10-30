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
use dhcook::audio::{AudioIndex, Control, EventDef, PlayDef, PlayNode, RtpcCurve};
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

pub struct GameAudioPlugin;

impl Plugin for GameAudioPlugin {
    fn build(&self, app: &mut App) {
        app.add_message::<PostEvent>().add_message::<StopEvent>()
            .init_resource::<AudioHandles>()
            .init_resource::<Instances>()
            .init_resource::<Ambients>()
            .init_resource::<WwiseStates>()
            .init_resource::<TimedSounds>()
            .init_resource::<Music>()
            .init_resource::<Rtpcs>()
            .init_resource::<DominantSpeaker>()
            .add_systems(Startup, |mut commands: Commands, settings: Res<crate::settings::Settings>| commands.insert_resource(Output::open(settings.speaker_config)))
            .add_systems(OnEnter(GameState::InGame), (reset_audio_state, setup_ambients.after(LevelSpawnSet)))
            .add_systems(Update, apply_speakers)
            .add_systems(
                Update,
                (attach_listener, script_states, run_ambients, run_timed, event_controls, play_events, follow_sounds, advance_playlists, finish_voices, stop_events, run_fades, attenuate, pause_voices).chain().after(apply_speakers).run_if(in_state(GameState::InGame)),
            );
    }
}

/// Master volume of the original mix (Wwise buses add their own levels).
const MASTER: f32 = 0.8;
/// Reach of positional sounds without an attenuation (metres).
const DEFAULT_REACH: f32 = 25.0;

static INDEX: OnceLock<Option<Arc<Library>>> = OnceLock::new();

struct Library {
    environments: HashMap<u32, dhcook::audio::Reverb>,
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
                environments: idx.environments,
                events: idx.events.into_iter().map(|e| (e.id, e)).collect(),
                media: idx.media,
                durations: idx.durations,
            }))
        })
        .clone()
}

/// A room environment's reverb (`DishonoredAudioVolume.m_Environment`), if the banks have it.
pub fn environment(name: &str) -> Option<dhcook::audio::Reverb> {
    library()?.environments.get(&dhcook::audio::fnv(name)).copied()
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
    /// carried by this entity (it follows it, and fades once it's gone)
    pub follow: Option<Entity>,
    /// the game object it sounds from, for its playback limit (else what it follows or who
    /// says it)
    pub object: Option<Entity>,
}

impl PostEvent {
    pub fn named(name: &str, at: Option<Vec3>) -> Self {
        let event = event_id(name);
        if log_enabled() {
            NAMES.lock().unwrap().get_or_insert_with(HashMap::new).entry(event).or_insert_with(|| name.to_string());
        }
        PostEvent { event, at, speaker: None, voice: false, follow: None, object: None }
    }

    /// Carried by `e` (a weeper's buzzing flies).
    pub fn following(mut self, e: Entity) -> Self {
        self.follow = Some(e);
        self
    }

    /// Sounding from `e` (its game object: a clip's sound, a footstep), for the limits.
    pub fn from_object(mut self, e: Entity) -> Self {
        self.object = Some(e);
        self
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

/// A loop out of earshot, paused so its file isn't read for nothing; it goes on once heard.
#[derive(Component)]
struct Hushed;

fn finish_voices(
    mut commands: Commands,
    q: Query<(Entity, &Voice), With<OneShot>>,
    all: Query<(&Voice, Has<Hushed>, &Emitter)>,
    time: Res<Time>,
    mut log_t: Local<f32>,
    out: Option<Res<Output>>,
) {
    for (e, v) in &q {
        if v.player.empty() {
            commands.entity(e).despawn();
        }
    }
    // (DH_AUDIO_LOG: how many sound, and how far the furthest has played, each second)
    *log_t += time.delta_secs();
    if *log_t > 1.0 && log_enabled() {
        *log_t = 0.0;
        let far = all.iter().map(|(v, ..)| v.player.get_pos().as_secs_f32()).fold(0.0, f32::max);
        let hushed = all.iter().filter(|(_, h, _)| *h).count();
        // (the events sounding most)
        let mut by: HashMap<u32, usize> = HashMap::new();
        for (_, _, em) in &all {
            *by.entry(em.event).or_default() += 1;
        }
        let mut top: Vec<(u32, usize)> = by.into_iter().collect();
        top.sort_by(|a, b| b.1.cmp(&a.1));
        let top: Vec<String> = top.iter().take(4).map(|(e, n)| format!("{} x{n}", event_name(*e))).collect();
        info!("audio: most: {}", top.join(", "));
        let (rate, ch) = out.as_ref().map_or((48000, 2), |o| o.format());
        info!("audio: {} voices ({hushed} out of earshot), furthest {far:.1}s, mixing {:.0}% of real time", all.iter().len(), crate::speakers::mix_load(rate, ch) * 100.0);
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

/// A new Speaker Configuration, or the device lost (unplugged, switched, reset): the output opened
/// again, and what was sounding started on it (the ambient emitters in reach, the music). With
/// no device to open, it tries again every few seconds.
#[allow(clippy::too_many_arguments)]
fn apply_speakers(
    mut commands: Commands,
    settings: Res<crate::settings::Settings>,
    out: Option<ResMut<Output>>,
    emitters: Query<Entity, With<Emitter>>,
    mut ambients: ResMut<Ambients>,
    music: Res<Music>,
    mut posts: MessageWriter<PostEvent>,
    (time, mut retry): (Res<Time<Real>>, Local<Option<f32>>),
) {
    let Some(mut out) = out else { return };
    let lost = crate::speakers::OUTPUT_LOST.swap(false, std::sync::atomic::Ordering::Relaxed);
    if lost {
        // (a moment for the system to settle on its new device)
        *retry = Some(time.elapsed_secs() + 0.5);
    }
    let due = retry.is_some_and(|t| time.elapsed_secs() >= t);
    if out.config == settings.speaker_config && !due {
        return;
    }
    *out = Output::open(settings.speaker_config);
    *retry = (!out.live() && std::env::var("DH_MUTE").is_err()).then(|| time.elapsed_secs() + 3.0);
    for e in &emitters {
        commands.entity(e).despawn();
    }
    for a in &mut ambients.list {
        a.playing = false;
        a.retrigger = 0.0;
    }
    if let Some((event, _, _)) = music.current {
        posts.write(PostEvent { event, at: None, speaker: None, voice: false, follow: None, object: None });
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

/// A sound carried by an entity.
#[derive(Component)]
struct Following(Entity);

/// Sounds go with what carries them, and fade once it's gone.
fn follow_sounds(mut q: Query<(&Following, &mut Transform, &mut Emitter)>, carriers: Query<&GlobalTransform>) {
    for (f, mut t, mut em) in &mut q {
        match carriers.get(f.0) {
            Ok(g) => t.translation = g.translation(),
            Err(_) if em.fade_rate >= 0.0 => {
                em.playlist = None;
                em.fade_rate = -4.0;
            }
            Err(_) => {}
        }
    }
}

/// The stops, pauses and resumes of the events posted (`EventDef::controls`), on the sounds already
/// playing: those under the object they name (it, or an object above what they play), only those
/// from the same game object for an `_O` action (its entity, else within a metre of where it was
/// posted), all for an `_ALL`: after its delay, a stop fading over its transition. Before, the events
/// that stopped sounds did nothing: the scripts' loops (machinery, alarms, fires, music) kept on.
fn event_controls(
    time: Res<Time>,
    mut msgs: MessageReader<PostEvent>,
    mut q: Query<(&mut Emitter, &Transform, &Voice)>,
    mut music: ResMut<Music>,
    mut pending: Local<Vec<(f32, Control, Option<Entity>, Option<Vec3>, u32)>>,
) {
    let Some(lib) = library() else {
        msgs.clear();
        return;
    };
    let log = std::env::var("DH_AUDIO_LOG").is_ok();
    let mut now: Vec<(Control, Option<Entity>, Option<Vec3>, u32)> = Vec::new();
    // (a delayed one acts on what plays then, the sound its own event just started too: a
    // guard's `Snd_AI_Guard_Head_Scratch_Short` cut off after 0.25 s)
    let dt = time.delta_secs();
    pending.retain_mut(|(left, c, o, at, ev)| {
        *left -= dt;
        if *left <= 0.0 {
            now.push((*c, *o, *at, *ev));
            false
        } else {
            true
        }
    });
    for m in msgs.read() {
        let Some(ev) = lib.events.get(&m.event).filter(|e| !e.controls.is_empty()) else { continue };
        let obj = m.object.or(m.follow).or(m.speaker);
        for c in &ev.controls {
            if c.delay > 0.01 {
                pending.push((c.delay, *c, obj, m.at, m.event));
            } else {
                now.push((*c, obj, m.at, m.event));
            }
        }
    }
    for (c, obj, at, event) in now {
        let mut n = 0;
        for (mut em, t, v) in &mut q {
            if c.target != 0 && !em.objects.contains(&c.target) {
                continue;
            }
            if c.on_object {
                let same = match (obj, at) {
                    (Some(o), _) => em.object == Some(o),
                    (None, Some(at)) => em.positional && t.translation.distance(at) < 1.0,
                    (None, None) => em.object.is_none() && !em.positional,
                };
                if !same {
                    continue;
                }
            }
            n += 1;
            match c.kind {
                0 => {
                    em.playlist = None;
                    em.fade_rate = em.fade_rate.min(-1.0 / c.fade.max(0.02));
                    if em.music && music.current.as_ref().is_some_and(|cur| cur.0 == em.event) {
                        music.current = None;
                    }
                }
                1 => {
                    if !v.player.is_paused() {
                        v.player.pause();
                        em.paused = true;
                    }
                }
                _ => {
                    if em.paused {
                        v.player.play();
                        em.paused = false;
                    }
                }
            }
        }
        if log {
            info!("audio: {} {} {:#x}{} ({n} sounds)", event_name(event), ["stops", "pauses", "resumes"][c.kind.min(2) as usize], c.target, if c.on_object { " on its object" } else { "" });
        }
    }
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
    /// a spoken line, and who says it
    voice: bool,
    speaker: Option<Entity>,
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
    /// the objects it plays under (what an event's stop or pause of one of them reaches), and
    /// its game object
    objects: Arc<Vec<u32>>,
    object: Option<Entity>,
    /// paused by an event (`Pause_E`), until one resumes it
    paused: bool,
}

/// Who says the dominant line (`attenuate`'s, the voices' ducking): its subtitle comes first
/// (`kismet`: the dominant hook's weight of 99).
#[derive(Resource, Default)]
pub struct DominantSpeaker(pub Option<Entity>);

/// Dialogue's ducking: the share of its reach within which a line can be the dominant one, the
/// others' volume, and how fast they blend (a second's worth).
const DOMINANT_REACH: f32 = 0.4;
const NON_DOMINANT: f32 = 0.3;
const DUCK_BLEND: f32 = 5.0;

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

/// The sounds playing under a playback limit, by event and the game object they sound from
/// (none: a global limit), oldest first: their voices and the frame they started.
#[derive(Resource, Default)]
struct Instances(HashMap<(u32, Option<Entity>), Vec<(Entity, u32)>>);

/// What starts a play entry.
struct Start {
    event: u32,
    at: Option<Vec3>,
    ambient: Option<usize>,
    fade_in: bool,
    speaker: Option<Entity>,
    voice: bool,
    follow: Option<Entity>,
    /// its game object (`PostEvent::object`, a swarm's for its rats)
    object: Option<Entity>,
}

/// Room for one more of a play entry under its playback limit (`PlayDef::limit`, the banks'
/// `u16MaxNumInstance`): within it, or the oldest stopped for it; none with `bKillNewest`. Counted
/// per game object, or over all with `bIsGlobalLimit`; a sound from no object is its own.
fn make_room(commands: &mut Commands, inst: &mut Instances, play: &PlayDef, s: &Start, alive: &dyn Fn(Entity) -> bool, frame: u32) -> Result<Option<(u32, Option<Entity>)>, ()> {
    let Some(l) = play.limit.filter(|l| l.max > 0) else { return Ok(None) };
    let obj = s.object.or(s.follow).or(s.speaker);
    if !l.global && obj.is_none() {
        return Ok(None);
    }
    let key = (s.event, if l.global { None } else { obj });
    let list = inst.0.entry(key).or_default();
    list.retain(|(e, f)| *f == frame || alive(*e));
    if list.len() >= l.max as usize {
        if l.kill_newest {
            return Err(());
        }
        let (old, _) = list.remove(0);
        commands.entity(old).try_despawn();
    }
    Ok(Some(key))
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
    (inst, alive, frame): (&mut Instances, &dyn Fn(Entity) -> bool, u32),
) -> Vec<(u32, u32)> {
    let mut used = Vec::new();
    // (a one-shot beyond its reach is silent: not started; Wwise's below-threshold voices)
    if let (Some(at), Some(l)) = (s.at, ear) {
        let reach = if play.max_distance > 0.5 { play.max_distance } else { DEFAULT_REACH };
        if !play.music && !play.looping && at.distance(l.translation()) > reach {
            return used;
        }
    }
    let Ok(limited) = make_room(commands, inst, play, &s, alive, frame) else { return used };
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
    let objects = Arc::new(play.objects.clone());
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
                speaker: s.speaker,
                ambient: s.ambient,
                playlist: pl,
                fade: if s.fade_in { 0.0 } else { 1.0 },
                fade_rate: if s.fade_in { 1.0 / FADE } else { 0.0 },
                curves: curves.clone(),
                objects: objects.clone(),
                object: s.object.or(s.follow).or(s.speaker),
                paused: false,
            },
            DespawnOnExit(GameState::InGame),
        ));
        if one_shot {
            ec.insert(OneShot);
        }
        if let Some(f) = s.follow {
            ec.insert(Following(f));
        }
        if let Some(key) = limited {
            inst.0.entry(key).or_default().push((ec.id(), frame));
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

/// The states the level scripts set (`SeqAct_AkSetState`), by their names' ids.
fn script_states(vm: Option<ResMut<crate::kismet::Vm>>, mut states: ResMut<WwiseStates>) {
    let Some(mut vm) = vm else { return };
    if vm.wwise_states.is_empty() {
        return;
    }
    for (group, state) in std::mem::take(&mut vm.wwise_states) {
        states.0.insert(event_id(&group), event_id(&state));
    }
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
    (mut inst, voices, swarms, frames): (ResMut<Instances>, Query<(), With<Voice>>, Query<(Entity, &crate::swarm::Swarm)>, Res<bevy::diagnostic::FrameCount>),
) {
    let Some(lib) = library() else {
        msgs.clear();
        return;
    };
    // (a swarm's rats sound from the swarm, one game object, as its own actor's)
    let swarm_of: HashMap<Entity, Entity> = swarms.iter().flat_map(|(s, sw)| sw.rats.iter().map(move |r| (*r, s))).collect();
    let alive = |e: Entity| voices.contains(e);
    let frame = frames.0;
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
            let object = m.object.map(|o| swarm_of.get(&o).copied().unwrap_or(o));
            let used = start_play(
                &mut commands,
                &lib,
                &mut handles,
                &out,
                listener.single().ok(),
                &states.0,
                play,
                Start { event: m.event, at: m.at, ambient, fade_in, speaker: m.speaker, voice: m.voice, follow: m.follow, object },
                (&mut inst, &alive, frame),
            );
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
                    let used = start_play(
                        &mut commands,
                        &lib,
                        &mut handles,
                        &out,
                        listener.single().ok(),
                        &states.0,
                        play,
                        Start { event, at: None, ambient: None, fade_in: true, speaker: None, voice: false, follow: None, object: None },
                        (&mut inst, &alive, frame),
                    );
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
                    speaker: em.speaker,
                    ambient: em.ambient,
                    playlist: p,
                    fade: em.fade,
                    fade_rate: em.fade_rate,
                    curves: em.curves.clone(),
                    objects: em.objects.clone(),
                    object: em.object,
                    paused: false,
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
    voices: Query<(Entity, &Transform, &Emitter, &Voice, Has<OneShot>, Has<Hushed>)>,
    (time, mut log_t, mut ducks): (Res<Time>, Local<f32>, Local<HashMap<Entity, f32>>),
    (rooms, movies, mut dominant_speaker): (Res<crate::audiorooms::AudioRooms>, Res<crate::movie::Movies>, ResMut<DominantSpeaker>),
    (mut commands, virt): (Commands, Res<Time<Virtual>>),
) {
    let Ok(l) = listener.single() else { return };
    // (a movie's own soundtrack over the music: the trials' credits over their menu's)
    let music_volume = if movies.soundtrack() { 0.0 } else { settings.music_volume };
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
    // (the ways through the doorways from where Corvo hears, once for all the sounds)
    let field = rooms.field(ear, false);
    // dialogue: the dominant line (the nearest within 40 % of its reach; one heard everywhere
    // above all) keeps its volume, the others' fall to 30 %, blended at 5 a second
    // (`DisConvGlobalMan`'s `m_fDominantMaxDistance_Percent`, `m_fNonDominantVolumeScale`,
    // `m_fAudioDuckBlendSpeed`, as roshan's r614/blink research decoded them)
    let dominance = |t: &Transform, em: &Emitter| if em.positional { 1.0 - t.translation.distance(ear) / (DOMINANT_REACH * em.reach).max(0.1) } else { 1.0 };
    let dominant = voices.iter().filter(|(_, _, em, ..)| em.voice).map(|(e, t, em, ..)| (e, dominance(t, em))).filter(|(_, d)| *d > 1e-8).max_by(|a, b| a.1.total_cmp(&b.1)).map(|(e, _)| e);
    let speaker = dominant.and_then(|d| voices.get(d).ok()).and_then(|(_, _, em, ..)| em.speaker);
    if dominant_speaker.0 != speaker {
        dominant_speaker.0 = speaker;
    }
    ducks.retain(|e, _| voices.contains(*e));
    let blend = DUCK_BLEND * time.delta_secs();
    for (e, t, em, voice, one_shot, hushed) in &voices {
        let cat = if em.music { music_volume } else if em.voice { settings.voice_volume } else { settings.sfx_volume };
        let (g, speed) = mods(em);
        if log && ((g - 1.0).abs() > 1e-3 || (speed - 1.0).abs() > 1e-3) {
            info!("rtpc: {} gain {g:.3} speed {speed:.3}", event_name(em.event));
        }
        let mut v = em.gain * g * em.fade.clamp(0.0, 1.0) * cat * settings.master_volume;
        if em.voice {
            let to = if dominant.is_none_or(|d| d == e) { 1.0 } else { NON_DOMINANT };
            let duck = ducks.entry(e).or_insert(to);
            *duck += (to - *duck).clamp(-blend, blend);
            v *= *duck;
        }
        if em.positional {
            // (from another room: the way through the doorways, from the first of them,
            // muffled by each)
            let (dist, from, through) = match field.as_ref().and_then(|f| f.route(&rooms, t.translation)) {
                Some(r) => (r.dist, ear + (r.toward - ear).normalize_or(Vec3::Z) * r.dist.max(0.1), r.gain),
                None => (t.translation.distance(ear), t.translation, 1.0),
            };
            let x = (1.0 - dist / em.reach).clamp(0.0, 1.0);
            v *= x * x * through;
            voice.pan.set(&world_gains(out.channels, l, from));
        }
        if (voice.player.volume() - v).abs() > 1e-4 {
            voice.player.set_volume(v);
        }
        // (a loop out of earshot pauses, its file no longer read for nothing, and goes on once
        // heard again; not while the game is paused, which holds every sound)
        let quiet = em.positional && !one_shot && v <= 1e-5;
        if quiet && !hushed && !voice.player.is_paused() {
            voice.player.pause();
            commands.entity(e).try_insert(Hushed);
        } else if !quiet && hushed && !virt.is_paused() {
            voice.player.play();
            commands.entity(e).try_remove::<Hushed>();
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
            requests.push((PostEvent { event: a.event, at: Some(a.position), speaker: None, voice: false, follow: None, object: None }, i));
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
