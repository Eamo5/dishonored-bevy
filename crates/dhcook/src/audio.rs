//! Wwise audio (Dishonored ships Wwise 2011 banks, bank version 65).
//!
//! Every `.pck` file package (and the localized `Bk_*.INT` voice packages) holds sound banks
//! and streamed media. A bank's `HIRC` chunk is the sound hierarchy: events -> actions ->
//! sounds / containers / interactive music -> media (WEM) sources; its `DIDX`/`DATA` chunks
//! hold the embedded media. The cooker resolves every event into what it plays and converts
//! the media (Wwise Vorbis) into standard Ogg Vorbis files the game plays directly.

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

/// FNV-1 32-bit hash of a lower-cased name: Wwise's event / object ids.
pub fn fnv(name: &str) -> u32 {
    let mut h: u32 = 2166136261;
    for c in name.to_ascii_lowercase().bytes() {
        h = h.wrapping_mul(16777619);
        h ^= c as u32;
    }
    h
}

/// Where a media file lives.
#[derive(Clone, Debug)]
enum Media {
    /// inside a bank's DATA chunk
    Embedded { file: usize, offset: usize, size: usize },
    /// a streamed file of a package
    Streamed { file: usize, offset: usize, size: usize },
}

#[derive(Clone, Debug, Default)]
struct Node {
    kind: u8,
    parent: u32,
    children: Vec<u32>,
    /// media sources (sounds, music tracks)
    sources: Vec<u32>,
    volume_db: f32,
    /// Wwise loop count property (Some(0) = infinite)
    loop_count: Option<u32>,
    /// event: actions; action: (type, target)
    actions: Vec<u32>,
    action: Option<(u16, u32)>,
    /// attenuation: max distance (game units)
    max_distance: f32,
    /// random/sequence container playing its children in order
    sequence: bool,
    /// switch container (sound or music): (state/switch group, default, cases)
    switch: Option<(u32, u32, Vec<(u32, Vec<u32>)>)>,
    /// SetState action: (state group, state)
    set_state: Option<(u32, u32)>,
    /// its RTPC curves (`InitialRTPC`)
    rtpcs: Vec<RtpcCurve>,
    /// raw data, for reference scans
    data: Vec<u8>,
}

/// What an event plays.
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct EventDef {
    pub id: u32,
    pub plays: Vec<PlayDef>,
    /// stops (event ids or object ids) are not modelled; set when the event only stops
    #[serde(default)]
    pub stop: bool,
    /// state changes: (state group, state)
    #[serde(default)]
    pub states: Vec<(u32, u32)>,
}

/// The sound hierarchy under a played object.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum PlayNode {
    /// a media file, with its volume offset (dB) relative to the played object
    Media(u32, f32),
    /// one child at random (or in order when the flag is set)
    Random(Vec<PlayNode>, bool),
    /// every child at once
    Layer(Vec<PlayNode>),
    /// the child for the current value of a state group (Wwise states are global)
    Switch { group: u32, default: u32, cases: Vec<(u32, PlayNode)> },
}

impl PlayNode {
    fn shift(&mut self, db: f32) {
        match self {
            PlayNode::Media(_, v) => *v += db,
            PlayNode::Random(c, _) | PlayNode::Layer(c) => c.iter_mut().for_each(|n| n.shift(db)),
            PlayNode::Switch { cases, .. } => cases.iter_mut().for_each(|(_, n)| n.shift(db)),
        }
    }

    pub fn media(&self, out: &mut Vec<u32>) {
        match self {
            PlayNode::Media(m, _) => out.push(*m),
            PlayNode::Random(c, _) | PlayNode::Layer(c) => c.iter().for_each(|n| n.media(out)),
            PlayNode::Switch { cases, .. } => cases.iter().for_each(|(_, n)| n.media(out)),
        }
    }
}

/// An RTPC curve on a sound object: the game parameter it follows, the property it drives
/// (`AkRTPC_ParameterID`: 0 volume (dB), 2 pitch (cents), 3 low-pass, ...), the curve's
/// scaling (`AkCurveScaling`), and its points (game parameter value, property value, the
/// interpolation (`AkCurveInterpolation`) on to the next).
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct RtpcCurve {
    pub rtpc: u32,
    pub param: u32,
    pub scaling: u8,
    pub points: Vec<(f32, f32, u32)>,
}

impl RtpcCurve {
    /// The property's value at a game parameter value: Wwise's interpolation between the
    /// points about it, held at the ends.
    pub fn eval(&self, x: f32) -> f32 {
        let p = &self.points;
        let (Some(first), Some(last)) = (p.first(), p.last()) else { return 0.0 };
        if x <= first.0 {
            return first.1;
        }
        if x >= last.0 {
            return last.1;
        }
        let k = p.windows(2).position(|w| x < w[1].0).unwrap_or(p.len() - 2);
        let (a, b) = (p[k], p[k + 1]);
        let t = if b.0 > a.0 { (x - a.0) / (b.0 - a.0) } else { 1.0 };
        a.1 + (b.1 - a.1) * curve_shape(a.2, t)
    }
}

/// Wwise's curve shapes (`AkCurveInterpolation`) over 0..1.
pub fn curve_shape(kind: u32, t: f32) -> f32 {
    use std::f32::consts::FRAC_PI_2;
    let t = t.clamp(0.0, 1.0);
    match kind {
        // logarithmic (base 3), sine (constant power in), logarithmic (base 1.41)
        0 => 1.0 - (1.0 - t).powi(3),
        1 => (t * FRAC_PI_2).sin(),
        2 => 1.0 - (1.0 - t).powf(1.41),
        // inverted S, S
        3 => {
            if t < 0.5 {
                0.5 * (1.0 - (1.0 - 2.0 * t).powi(2))
            } else {
                0.5 + 0.5 * (2.0 * t - 1.0).powi(2)
            }
        }
        5 => t * t * (3.0 - 2.0 * t),
        // exponential (base 1.41), sine (constant power out), exponential (base 3)
        6 => t.powf(1.41),
        7 => 1.0 - (t * FRAC_PI_2).cos(),
        8 => t.powi(3),
        // constant: the first point's value until the next
        9 => 0.0,
        _ => t,
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct PlayDef {
    /// every media id it may play; `layered`: the root plays its children together
    pub media: Vec<u32>,
    pub layered: bool,
    /// what to play: random / layered containers down to media
    #[serde(default)]
    pub tree: Option<PlayNode>,
    pub looping: bool,
    pub volume_db: f32,
    /// attenuation max distance in metres (0 = not positional)
    pub max_distance: f32,
    pub music: bool,
    /// the RTPC curves on it, its ancestors and what it plays
    #[serde(default)]
    pub rtpcs: Vec<RtpcCurve>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct AudioIndex {
    pub events: Vec<EventDef>,
    /// media id -> cooked file (relative to the cache)
    pub media: HashMap<u32, String>,
    /// media id -> length in seconds
    #[serde(default)]
    pub durations: HashMap<u32, f32>,
}

/// A sound switch container: group type (u32), group, default switch, continuous
/// validation (u8), the children, then each switch's items and per-item settings.
fn sound_switch(d: &[u8], ids: &HashSet<u32>) -> Option<(u32, u32, Vec<(u32, Vec<u32>)>)> {
    let mut p = 0;
    while p + 17 <= d.len() {
        let ty = u32_at(d, p)?;
        let c = u32_at(d, p + 13)? as usize;
        if ty <= 1 && (1..=256).contains(&c) && p + 17 + c * 4 + 4 <= d.len() && (0..c).all(|k| u32_at(d, p + 17 + k * 4).is_some_and(|x| ids.contains(&x))) {
            let (group, default) = (u32_at(d, p + 4)?, u32_at(d, p + 8)?);
            let mut q = p + 17 + c * 4;
            let ng = u32_at(d, q)? as usize;
            q += 4;
            let mut cases = Vec::new();
            for _ in 0..ng.min(256) {
                let s = u32_at(d, q)?;
                let n = u32_at(d, q + 4)? as usize;
                let items: Vec<u32> = (0..n.min(256)).filter_map(|k| u32_at(d, q + 8 + k * 4)).collect();
                q += 8 + n * 4;
                if !items.is_empty() {
                    cases.push((s, items));
                }
            }
            return Some((group, default, cases));
        }
        p += 1;
    }
    None
}

/// A music switch container ends with: group type (u32), group, default switch, continue
/// playback (u8), then (switch, node) associations.
fn music_switch(d: &[u8]) -> Option<(u32, u32, Vec<(u32, Vec<u32>)>)> {
    for n in 0..=256usize {
        let Some(p) = d.len().checked_sub(17 + 8 * n) else { break };
        if u32_at(d, p)? <= 1 && u32_at(d, p + 13)? as usize == n {
            let cases = (0..n)
                .filter_map(|k| Some((u32_at(d, p + 17 + k * 8)?, u32_at(d, p + 21 + k * 8)?)))
                .filter(|(s, node)| *s != 0 && *node != 0)
                .map(|(s, node)| (s, vec![node]))
                .collect();
            return Some((u32_at(d, p + 4)?, u32_at(d, p + 8)?, cases));
        }
    }
    None
}

/// Length of a WEM in seconds, from its format header (and Vorbis sample count).
pub fn wem_duration(wem: &[u8]) -> Option<f32> {
    let ch = chunks(wem.get(12..)?);
    let &(fo, fs) = ch.get(b"fmt ")?;
    let fo = fo + 12;
    let codec = u16::from_le_bytes([*wem.get(fo)?, *wem.get(fo + 1)?]);
    let channels = u16::from_le_bytes([*wem.get(fo + 2)?, *wem.get(fo + 3)?]).max(1) as f32;
    let rate = u32_at(wem, fo + 4)? as f32;
    if rate <= 0.0 {
        return None;
    }
    match codec {
        0xFFFF if fs >= 0x42 => Some(u32_at(wem, fo + 0x18)? as f32 / rate),
        0x0001 | 0xFFFE => {
            let bits = u16::from_le_bytes([*wem.get(fo + 14)?, *wem.get(fo + 15)?]).max(8) as f32;
            let &(_, ds) = ch.get(b"data")?;
            Some(ds as f32 / (channels * bits / 8.0) / rate)
        }
        _ => None,
    }
}

pub struct Wwise {
    files: Vec<PathBuf>,
    media: HashMap<u32, Media>,
    nodes: HashMap<u32, Node>,
    /// bank id -> (file, DATA offset)
    banks: HashMap<u32, (usize, usize)>,
}

fn u32_at(d: &[u8], p: usize) -> Option<u32> {
    d.get(p..p + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

fn chunks(b: &[u8]) -> HashMap<[u8; 4], (usize, usize)> {
    let mut out = HashMap::new();
    let mut p = 0;
    while p + 8 <= b.len() {
        let mut id = [0u8; 4];
        id.copy_from_slice(&b[p..p + 4]);
        let size = u32_at(b, p + 4).unwrap_or(0) as usize;
        out.insert(id, (p + 8, size));
        p += 8 + size;
    }
    out
}

/// The NodeBaseParams of a sound or container starting at `p`: (parent, volume, loop).
fn node_base(d: &[u8], mut p: usize) -> Option<(u32, f32, Option<u32>)> {
    let _override_fx = *d.get(p)?;
    let num_fx = *d.get(p + 1)? as usize;
    p += 2;
    if num_fx > 0 {
        p += 1 + num_fx * 7;
    }
    let _bus = u32_at(d, p)?;
    let parent = u32_at(d, p + 4)?;
    p += 8 + 2;
    let n = *d.get(p)? as usize;
    p += 1;
    let ids = d.get(p..p + n)?.to_vec();
    p += n;
    let mut volume = 0.0;
    let mut looping = None;
    for (k, id) in ids.iter().enumerate() {
        let v = u32_at(d, p + k * 4)?;
        match id {
            0 => volume = f32::from_bits(v),
            7 => looping = Some(v),
            _ => {}
        }
    }
    Some((parent, volume, looping))
}

/// The RTPC block (`InitialRTPC`: a count (u16), then per curve the game parameter, the
/// property, the curve's id, its scaling (u8), the point count (u16) and the points (from, to,
/// interpolation)) at `p`, and where it ends; None where what's there isn't one.
fn rtpc_block(d: &[u8], mut p: usize) -> Option<(Vec<RtpcCurve>, usize)> {
    let n = u16::from_le_bytes([*d.get(p)?, *d.get(p + 1)?]) as usize;
    p += 2;
    if n > 32 {
        return None;
    }
    let mut out = Vec::with_capacity(n);
    for _ in 0..n {
        let rtpc = u32_at(d, p)?;
        let param = u32_at(d, p + 4)?;
        let scaling = *d.get(p + 12)?;
        let size = u16::from_le_bytes([*d.get(p + 13)?, *d.get(p + 14)?]) as usize;
        p += 15;
        if rtpc == 0 || param > 64 || scaling > 5 || size == 0 || size > 256 {
            return None;
        }
        let mut points: Vec<(f32, f32, u32)> = Vec::with_capacity(size);
        for _ in 0..size {
            let (x, y, i) = (f32::from_bits(u32_at(d, p)?), f32::from_bits(u32_at(d, p + 4)?), u32_at(d, p + 8)?);
            if !x.is_finite() || !y.is_finite() || i > 9 || points.last().is_some_and(|l| l.0 > x) {
                return None;
            }
            points.push((x, y, i));
            p += 12;
        }
        out.push(RtpcCurve { rtpc, param, scaling, points });
    }
    Some((out, p))
}

/// The state chunk (a count (u32), then per state group its id, sync type (u8), and its states
/// (u16 count, each the state and its instance)) at `p`: where it ends.
fn state_chunk(d: &[u8], mut p: usize) -> Option<usize> {
    let n = u32_at(d, p)? as usize;
    p += 4;
    if n > 64 {
        return None;
    }
    for _ in 0..n {
        let c = u16::from_le_bytes([*d.get(p + 5)?, *d.get(p + 6)?]) as usize;
        p += 7 + c * 8;
    }
    (p <= d.len()).then_some(p)
}

/// The RTPC curves of the NodeBaseParams starting at `p` (Wwise 2011, bank version 65): the
/// effects, bus, parent and priority; the property and ranged property bundles; positioning
/// (overridden: whether it's 3D, a 2D one's panner, a 3D one's type, attenuation and
/// spatialization, a user-defined path's vertices and playlist, a game-defined one's dynamic
/// flag); the advanced settings (`adv` bytes); the states; then the curves. With where they end.
fn node_rtpcs_with(d: &[u8], mut p: usize, adv: usize) -> Option<(Vec<RtpcCurve>, usize)> {
    let num_fx = *d.get(p + 1)? as usize;
    p += 2;
    if num_fx > 0 {
        p += 1 + num_fx * 7;
    }
    p += 8 + 2;
    let n = *d.get(p)? as usize;
    p += 1 + n * 5;
    let rn = *d.get(p)? as usize;
    p += 1 + rn * 9;
    let overridden = *d.get(p)? != 0;
    p += 1;
    if overridden {
        let is3d = *d.get(p)? != 0;
        p += 1;
        if !is3d {
            p += 1;
        } else {
            let ty = u32_at(d, p)?;
            p += 4 + 4 + 1;
            match ty {
                2 => {
                    p += 4 + 1 + 4 + 1;
                    let verts = u32_at(d, p)? as usize;
                    p += 4 + verts * 16;
                    let items = u32_at(d, p)? as usize;
                    p += 4 + items * 8 + items * 8;
                }
                3 => p += 1,
                _ => {}
            }
        }
    }
    p += adv;
    let p = state_chunk(d, p)?;
    rtpc_block(d, p)
}

/// Whether a children list (a count, then known ids) is at `p`.
fn children_at(d: &[u8], p: usize, ids: &HashSet<u32>) -> bool {
    let Some(c) = u32_at(d, p) else { return false };
    c <= 512 && p + 4 + c as usize * 4 <= d.len() && (0..c as usize).all(|k| u32_at(d, p + 4 + k * 4).is_some_and(|x| ids.contains(&x)))
}

/// A sound object's RTPC curves: its NodeBaseParams read with the advanced settings at 9 bytes,
/// else 8, whichever leaves what follows them where it should be (a sound: the end; a random /
/// sequence container: its 24 bytes of settings, then its children; an actor-mixer or layer
/// container: its children).
fn node_rtpcs(kind: u8, d: &[u8], p: usize, ids: &HashSet<u32>) -> Option<Vec<RtpcCurve>> {
    let fits = |end: usize| match kind {
        2 => end == d.len(),
        5 => children_at(d, end + 24, ids),
        6 => true,
        _ => children_at(d, end, ids),
    };
    [9, 8].iter().find_map(|&adv| node_rtpcs_with(d, p, adv).filter(|(_, end)| fits(*end))).map(|r| r.0)
}

/// Where a sound object's NodeBaseParams start (a sound's follow its source).
fn base_start(kind: u8, d: &[u8]) -> usize {
    match kind {
        2 if u32_at(d, 4) == Some(1) => 17,
        2 => 25,
        _ => 0,
    }
}

impl Wwise {
    /// Index every Wwise package of the cooked directory.
    pub fn load(cooked: &Path) -> Result<Wwise> {
        let mut files: Vec<PathBuf> = std::fs::read_dir(cooked)?
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| {
                let n = p.file_name().map(|n| n.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
                n.ends_with(".pck") || (n.starts_with("bk_") && n.ends_with(".int"))
            })
            .collect();
        files.sort();
        let mut w = Wwise { files: Vec::new(), media: HashMap::new(), nodes: HashMap::new(), banks: HashMap::new() };
        for path in files {
            let fi = w.files.len();
            w.files.push(path.clone());
            let d = std::fs::read(&path)?;
            if let Err(e) = w.index_package(fi, &d) {
                log::warn!("{}: {e:#}", path.display());
            }
        }
        // children from parent pointers (sounds and containers point at their parent)
        let links: Vec<(u32, u32)> = w.nodes.iter().filter(|(_, n)| n.parent != 0).map(|(&id, n)| (n.parent, id)).collect();
        for (parent, child) in links {
            if let Some(p) = w.nodes.get_mut(&parent) {
                if !p.children.contains(&child) {
                    p.children.push(child);
                }
            }
        }
        // explicit children lists (music nodes, containers): a count followed by known ids
        let ids: HashSet<u32> = w.nodes.keys().copied().collect();
        for n in w.nodes.values_mut() {
            if !matches!(n.kind, 5 | 6 | 7 | 9 | 10 | 12 | 13) {
                continue;
            }
            let d = n.data.clone();
            let mut p = 0;
            let mut first = true;
            while p + 8 <= d.len() {
                let c = u32_at(&d, p).unwrap_or(0) as usize;
                if (1..=256).contains(&c) && p + 4 + c * 4 <= d.len() {
                    let list: Vec<u32> = (0..c).filter_map(|k| u32_at(&d, p + 4 + k * 4)).collect();
                    if list.iter().all(|x| ids.contains(x)) {
                        // random / sequence container settings precede its children:
                        // loop count (u16), 3 transition floats, avoid repeat (u16), transition
                        // mode, random mode, mode, use weight, reset, restart backward,
                        // continuous, global
                        if n.kind == 5 && first && p >= 24 {
                            let loops = u16::from_le_bytes([d[p - 24], d[p - 23]]);
                            let continuous = d[p - 2] != 0;
                            if continuous && loops == 0 {
                                n.loop_count = Some(0);
                            }
                            n.sequence = d[p - 8] == 1;
                        }
                        first = false;
                        for x in list {
                            if !n.children.contains(&x) {
                                n.children.push(x);
                            }
                        }
                        p += 4 + c * 4;
                        continue;
                    }
                }
                p += 1;
            }
        }
        for n in w.nodes.values_mut() {
            n.switch = match n.kind {
                6 => sound_switch(&n.data, &ids),
                12 => music_switch(&n.data),
                _ => None,
            };
            if matches!(n.kind, 2 | 5 | 6 | 7 | 9) {
                n.rtpcs = node_rtpcs(n.kind, &n.data, base_start(n.kind, &n.data), &ids).unwrap_or_default();
            }
        }
        log::info!("wwise: {} packages, {} objects, {} media", w.files.len(), w.nodes.len(), w.media.len());
        Ok(w)
    }

    fn index_package(&mut self, fi: usize, d: &[u8]) -> Result<()> {
        if d.get(0..4) != Some(b"AKPK".as_slice()) {
            bail!("not a Wwise package");
        }
        let lang = u32_at(d, 12).unwrap_or(0) as usize;
        let banks_size = u32_at(d, 16).unwrap_or(0) as usize;
        let stm_size = u32_at(d, 20).unwrap_or(0) as usize;
        let table = |p: usize| -> Vec<(u32, usize, usize)> {
            let n = u32_at(d, p).unwrap_or(0) as usize;
            (0..n.min(100_000))
                .filter_map(|k| {
                    let q = p + 4 + k * 20;
                    let id = u32_at(d, q)?;
                    let mult = u32_at(d, q + 4)?.max(1) as usize;
                    let size = u32_at(d, q + 8)? as usize;
                    let off = u32_at(d, q + 12)? as usize * mult;
                    Some((id, off, size))
                })
                .collect()
        };
        let banks_at = 28 + lang;
        let banks = table(banks_at);
        let streams = table(banks_at + banks_size);
        let _ = stm_size;
        for (id, off, size) in streams {
            self.media.entry(id).or_insert(Media::Streamed { file: fi, offset: off, size });
        }
        for (bank_id, off, size) in banks {
            let Some(b) = d.get(off..off + size) else { continue };
            self.index_bank(fi, off, bank_id, b);
        }
        Ok(())
    }

    fn index_bank(&mut self, fi: usize, base: usize, bank_id: u32, b: &[u8]) {
        let ch = chunks(b);
        if let (Some(&(di, ds)), Some(&(data, _))) = (ch.get(b"DIDX"), ch.get(b"DATA")) {
            self.banks.insert(bank_id, (fi, base + data));
            for k in 0..ds / 12 {
                let q = di + k * 12;
                let (Some(id), Some(off), Some(size)) = (u32_at(b, q), u32_at(b, q + 4), u32_at(b, q + 8)) else { continue };
                self.media.entry(id).or_insert(Media::Embedded { file: fi, offset: base + data + off as usize, size: size as usize });
            }
        }
        let Some(&(h, _)) = ch.get(b"HIRC") else { return };
        let n = u32_at(b, h).unwrap_or(0) as usize;
        let mut p = h + 4;
        for _ in 0..n {
            let Some(&kind) = b.get(p) else { break };
            let size = u32_at(b, p + 1).unwrap_or(0) as usize;
            let id = u32_at(b, p + 5).unwrap_or(0);
            let data = b.get(p + 9..p + 5 + size).unwrap_or(&[]).to_vec();
            p += 5 + size;
            let mut node = Node { kind, ..Default::default() };
            match kind {
                // sound: source then NodeBaseParams
                2 => {
                    let stream = u32_at(&data, 4).unwrap_or(0);
                    let src = u32_at(&data, 8).unwrap_or(0);
                    node.sources.push(src);
                    let q = if stream == 1 { 17 } else { 25 };
                    if let Some((parent, vol, lp)) = node_base(&data, q) {
                        node.parent = parent;
                        node.volume_db = vol;
                        node.loop_count = lp;
                    }

                }
                3 => {
                    let ty = u16::from_le_bytes([data.first().copied().unwrap_or(0), data.get(1).copied().unwrap_or(0)]);
                    node.action = Some((ty, u32_at(&data, 2).unwrap_or(0)));
                    // set state: ... state group, target state (the action's last 8 bytes)
                    if ty == 0x1204 && data.len() >= 16 {
                        node.set_state = Some((u32_at(&data, data.len() - 8).unwrap_or(0), u32_at(&data, data.len() - 4).unwrap_or(0)));
                    }
                }
                4 => {
                    let c = u32_at(&data, 0).unwrap_or(0) as usize;
                    node.actions = (0..c.min(64)).filter_map(|k| u32_at(&data, 4 + k * 4)).collect();
                }
                5 | 6 | 7 => {
                    if let Some((parent, vol, lp)) = node_base(&data, 0) {
                        node.parent = parent;
                        node.volume_db = vol;
                        node.loop_count = lp;
                    }
                }
                // music track: sources (plugin, stream type, source, file[, offset, size], bits)
                11 => {
                    let c = u32_at(&data, 0).unwrap_or(0) as usize;
                    let mut q = 4;
                    for _ in 0..c.min(32) {
                        let stream = u32_at(&data, q + 4).unwrap_or(0);
                        if let Some(src) = u32_at(&data, q + 8) {
                            node.sources.push(src);
                        }
                        q += if stream == 1 { 17 } else { 25 };
                    }
                }
                // attenuation: the far end of the first (distance) curve
                14 => {
                    node.max_distance = attenuation_distance(&data);
                }
                _ => {}
            }
            node.data = data;
            self.nodes.entry(id).or_insert(node);
        }
    }

    /// Attenuation reach for a node: an attenuation id referenced by it or an ancestor.
    fn max_distance(&self, mut id: u32) -> f32 {
        for _ in 0..16 {
            let Some(n) = self.nodes.get(&id) else { break };
            let d = &n.data;
            let mut p = 0;
            while p + 4 <= d.len() {
                if let Some(a) = u32_at(d, p).and_then(|x| self.nodes.get(&x)).filter(|a| a.kind == 14) {
                    return a.max_distance;
                }
                p += 1;
            }
            if n.parent == 0 {
                break;
            }
            id = n.parent;
        }
        0.0
    }

    /// The hierarchy under `id`. The played object's own volume goes to `out.volume_db`,
    /// its descendants' to their media; any looping node makes the play loop.
    fn collect(&self, id: u32, depth: usize, music: bool, out: &mut PlayDef) -> Option<PlayNode> {
        let n = self.nodes.get(&id)?;
        if depth > 12 {
            return None;
        }
        if n.loop_count.is_some() {
            out.looping = true;
        }
        for c in &n.rtpcs {
            if !out.rtpcs.contains(c) {
                out.rtpcs.push(c.clone());
            }
        }
        let media = |n: &Node| -> Vec<PlayNode> { n.sources.iter().filter(|m| self.media.contains_key(m)).map(|&m| PlayNode::Media(m, 0.0)).collect() };
        let one = |mut v: Vec<PlayNode>, layer: bool, seq: bool| match v.len() {
            0 => None,
            1 => v.pop(),
            _ if layer => Some(PlayNode::Layer(v)),
            _ => Some(PlayNode::Random(v, seq)),
        };
        let mut node = match n.kind {
            2 => one(media(n), false, false),
            // a music track's clips, a segment's tracks: together
            11 => one(media(n), true, false),
            10 => one(n.children.iter().filter_map(|c| self.nodes.get(c)).filter(|t| t.kind == 11).flat_map(|t| media(t)).collect(), true, false),
            // layer container: every child at once
            9 => one(n.children.iter().filter_map(|&c| self.collect(c, depth + 1, music, out)).collect(), true, false),
            // switch containers: a case per state
            6 | 12 if n.switch.is_some() => {
                let (group, default, list) = n.switch.as_ref()?;
                let cases: Vec<(u32, PlayNode)> = list
                    .iter()
                    .filter_map(|(s, nodes)| {
                        let v: Vec<PlayNode> = nodes.iter().filter_map(|&c| self.collect(c, depth + 1, music, out)).collect();
                        one(v, true, false).map(|p| (*s, p))
                    })
                    .collect();
                (!cases.is_empty()).then_some(PlayNode::Switch { group: *group, default: *default, cases })
            }
            12 if music => n.children.iter().find_map(|&c| self.collect(c, depth + 1, music, out)),
            // containers: every child is a variation
            _ => one(n.children.iter().filter_map(|&c| self.collect(c, depth + 1, music, out)).collect(), false, n.sequence),
        }?;
        if depth == 0 {
            out.volume_db += n.volume_db;
        } else {
            node.shift(n.volume_db);
        }
        Some(node)
    }

    /// Resolve an event id into what it plays.
    pub fn event(&self, id: u32) -> Option<EventDef> {
        let e = self.nodes.get(&id).filter(|n| n.kind == 4)?;
        let mut def = EventDef { id, ..Default::default() };
        let mut stops = 0;
        for a in &e.actions {
            let Some((ty, target)) = self.nodes.get(a).and_then(|n| n.action) else { continue };
            match ty {
                0x0403 => {
                    let music = self.nodes.get(&target).is_some_and(|n| matches!(n.kind, 10 | 12 | 13));
                    let mut p = PlayDef { music, ..Default::default() };
                    p.tree = self.collect(target, 0, music, &mut p);
                    // the curves of its ancestors (actor-mixers, containers) apply to it too
                    let mut up = self.nodes.get(&target).map(|n| n.parent).unwrap_or(0);
                    for _ in 0..16 {
                        let Some(n) = self.nodes.get(&up).filter(|_| up != 0) else { break };
                        for c in &n.rtpcs {
                            if !p.rtpcs.contains(c) {
                                p.rtpcs.push(c.clone());
                            }
                        }
                        up = n.parent;
                    }
                    if let Some(t) = &p.tree {
                        t.media(&mut p.media);
                        p.layered = matches!(t, PlayNode::Layer(_));
                    }
                    p.max_distance = self.max_distance(target) * 0.01;
                    p.media.sort();
                    p.media.dedup();
                    if !p.media.is_empty() {
                        def.plays.push(p);
                    }
                }
                0x0102 | 0x0103 | 0x0104 => stops += 1,
                _ => {}
            }
        }
        for a in &e.actions {
            if let Some(s) = self.nodes.get(a).and_then(|n| n.set_state) {
                def.states.push(s);
            }
        }
        def.stop = def.plays.is_empty() && stops > 0;
        (!def.plays.is_empty() || def.stop || !def.states.is_empty()).then_some(def)
    }

    /// Every object's RTPC curves (by object id).
    pub fn rtpc_curves(&self) -> Vec<(u32, u8, &[RtpcCurve])> {
        self.nodes.iter().filter(|(_, n)| !n.rtpcs.is_empty()).map(|(&id, n)| (id, n.kind, n.rtpcs.as_slice())).collect()
    }

    /// How many sound objects had NodeBaseParams the RTPC reader could follow (kinds 2, 5-7, 9),
    /// by kind and the advanced settings' size: (kind, 9-byte, 8-byte, neither).
    pub fn rtpc_coverage(&self) -> Vec<(u8, usize, usize, usize)> {
        let ids: HashSet<u32> = self.nodes.keys().copied().collect();
        let mut out: Vec<(u8, usize, usize, usize)> = [2, 5, 6, 7, 9].iter().map(|&k| (k, 0, 0, 0)).collect();
        for n in self.nodes.values().filter(|n| matches!(n.kind, 2 | 5 | 6 | 7 | 9)) {
            let p = base_start(n.kind, &n.data);
            let fits = |adv: usize| {
                node_rtpcs_with(&n.data, p, adv).is_some_and(|(_, end)| match n.kind {
                    2 => end == n.data.len(),
                    5 => children_at(&n.data, end + 24, &ids),
                    6 => true,
                    _ => children_at(&n.data, end, &ids),
                })
            };
            let row = out.iter_mut().find(|r| r.0 == n.kind).unwrap();
            if fits(9) {
                row.1 += 1;
            } else if fits(8) {
                row.2 += 1;
            } else {
                row.3 += 1;
            }
        }
        out
    }

    /// Debug: each sound object's bytes from its positioning on (kinds 2, 5-7, 9), and whether
    /// the RTPC reader followed it.
    pub fn positioning_bytes(&self) -> Vec<(u32, u8, bool, Vec<u8>)> {
        self.nodes
            .iter()
            .filter(|(_, n)| matches!(n.kind, 2 | 5 | 6 | 7 | 9))
            .filter_map(|(&id, n)| {
                let d = &n.data;
                let mut p = base_start(n.kind, d);
                let ok = !n.rtpcs.is_empty() || node_rtpcs_with(d, p, 9).is_some();
                let num_fx = *d.get(p + 1)? as usize;
                p += 2;
                if num_fx > 0 {
                    p += 1 + num_fx * 7;
                }
                p += 10;
                let k = *d.get(p)? as usize;
                p += 1 + k * 5;
                let rn = *d.get(p)? as usize;
                p += 1 + rn * 9;
                Some((id, n.kind, ok, d.get(p..(p + 48).min(d.len()))?.to_vec()))
            })
            .collect()
    }

    /// Debug: objects of a kind whose NodeBaseParams the RTPC reader rejects, with where its read
    /// ended (9-byte advanced settings) and their data.
    pub fn rtpc_rejects(&self, kind: u8) -> Vec<(u32, usize, Option<usize>, Vec<u8>)> {
        let ids: HashSet<u32> = self.nodes.keys().copied().collect();
        self.nodes
            .iter()
            .filter(|(_, n)| n.kind == kind && node_rtpcs(n.kind, &n.data, base_start(n.kind, &n.data), &ids).is_none())
            .map(|(&id, n)| (id, base_start(n.kind, &n.data), node_rtpcs_with(&n.data, base_start(n.kind, &n.data), 9).map(|r| r.1), n.data.clone()))
            .collect()
    }

    /// Debug view of a hierarchy object: kind, children, actions, raw data.
    pub fn node_debug(&self, id: u32) -> Option<(u8, Vec<u32>, Vec<u32>, Vec<u8>)> {
        let n = self.nodes.get(&id)?;
        Some((n.kind, n.children.clone(), n.actions.clone(), n.data.clone()))
    }

    pub fn event_ids(&self) -> Vec<u32> {
        let mut v: Vec<u32> = self.nodes.iter().filter(|(_, n)| n.kind == 4).map(|(&id, _)| id).collect();
        v.sort();
        v
    }

    /// The raw WEM (RIFF) bytes of a media id.
    pub fn wem(&self, id: u32) -> Result<Vec<u8>> {
        let (file, offset, size) = match self.media.get(&id) {
            Some(Media::Embedded { file, offset, size }) | Some(Media::Streamed { file, offset, size }) => (*file, *offset, *size),
            None => bail!("media {id} not found"),
        };
        use std::io::{Read, Seek, SeekFrom};
        let mut f = std::fs::File::open(&self.files[file])?;
        f.seek(SeekFrom::Start(offset as u64))?;
        let mut buf = vec![0u8; size];
        f.read_exact(&mut buf)?;
        Ok(buf)
    }
}

/// Max distance of an attenuation's first curve (its last point's x).
fn attenuation_distance(d: &[u8]) -> f32 {
    // curves: (u32 point count, then (x f32, y f32, interp u32) per point); take the first
    // plausible curve
    let mut p = 0;
    while p + 4 <= d.len() {
        let n = u32_at(d, p).unwrap_or(0) as usize;
        if (2..=32).contains(&n) && p + 4 + n * 12 <= d.len() {
            let xs: Vec<f32> = (0..n).map(|k| f32::from_bits(u32_at(d, p + 4 + k * 12).unwrap_or(0))).collect();
            if xs[0] == 0.0 && xs.windows(2).all(|w| w[1] > w[0]) && xs[n - 1] > 10.0 && xs[n - 1] < 100_000.0 {
                return xs[n - 1];
            }
        }
        p += 1;
    }
    0.0
}

/// Convert a WEM to a playable file: Ogg Vorbis for Wwise Vorbis, WAV for PCM.
pub fn convert_wem(wem: &[u8]) -> Result<(Vec<u8>, &'static str)> {
    let codec = if wem.len() > 22 { u16::from_le_bytes([wem[20], wem[21]]) } else { 0 };
    match codec {
        0xFFFF => {
            let mut out = Vec::new();
            let mut last = None;
            for books in [ww2ogg::CodebookLibrary::aotuv_codebooks(), ww2ogg::CodebookLibrary::default_codebooks()] {
                let books = books.map_err(|e| anyhow::anyhow!("{e}"))?;
                out.clear();
                let r = ww2ogg::WwiseRiffVorbis::new(std::io::Cursor::new(wem), books)
                    .and_then(|mut c| c.generate_ogg(&mut out))
                    .and_then(|_| ww2ogg::validate(&out));
                match r {
                    Ok(()) => return Ok((out, "ogg")),
                    Err(e) => last = Some(e),
                }
            }
            bail!("vorbis: {}", last.map(|e| e.to_string()).unwrap_or_default())
        }
        0x0001 | 0xFFFE => {
            // plain PCM: the RIFF is a valid WAV once the format tag reads PCM
            let mut w = wem.to_vec();
            w[20] = 1;
            w[21] = 0;
            Ok((w, "wav"))
        }
        c => bail!("unsupported codec {c:#x}"),
    }
}

/// Cook every event and its media into `root/audio`.
pub fn cook(cooked: &Path, root: &Path, progress: &(dyn Fn(f32, &str) + Sync)) -> Result<AudioIndex> {
    let w = Wwise::load(cooked)?;
    let dir = root.join("audio");
    std::fs::create_dir_all(&dir)?;
    let mut index = AudioIndex::default();
    let mut needed: Vec<u32> = Vec::new();
    for id in w.event_ids() {
        if let Some(e) = w.event(id) {
            for p in &e.plays {
                needed.extend(p.media.iter().copied());
            }
            index.events.push(e);
        }
    }
    needed.sort();
    needed.dedup();
    let total = needed.len().max(1);
    let done = std::sync::atomic::AtomicUsize::new(0);
    use rayon::prelude::*;
    let results: Vec<(u32, Option<String>, Option<f32>)> = needed
        .par_iter()
        .map(|&m| {
            let k = done.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            if k % 500 == 0 {
                progress(k as f32 / total as f32, &format!("Audio {k}/{total}"));
            }
            let wem = match w.wem(m) {
                Ok(wem) => wem,
                Err(e) => {
                    log::debug!("media {m}: {e:#}");
                    return (m, None, None);
                }
            };
            let duration = wem_duration(&wem);
            for ext in ["ogg", "wav"] {
                let rel = format!("audio/{m}.{ext}");
                if root.join(&rel).exists() {
                    return (m, Some(rel), duration);
                }
            }
            let r = convert_wem(&wem).and_then(|(bytes, ext)| {
                let rel = format!("audio/{m}.{ext}");
                std::fs::write(root.join(&rel), bytes)?;
                Ok(rel)
            });
            match r {
                Ok(rel) => (m, Some(rel), duration),
                Err(e) => {
                    log::debug!("media {m}: {e:#}");
                    (m, None, None)
                }
            }
        })
        .collect();
    let mut failed = 0;
    for (m, f, d) in results {
        match f {
            Some(f) => {
                index.media.insert(m, f);
                if let Some(d) = d {
                    index.durations.insert(m, d);
                }
            }
            None => failed += 1,
        }
    }
    log::info!("audio: {} events, {} media files ({failed} failed)", index.events.len(), index.media.len());
    std::fs::write(dir.join("index.json"), serde_json::to_vec(&index)?)?;
    envelopes(root, &index)?;
    Ok(index)
}

/// Frames per second of the loudness envelopes.
pub const ENVELOPE_RATE: f32 = 30.0;

/// Loudness envelopes of the short clips (spoken lines among them), for the speakers' jaws:
/// `audio/envelopes.bin` = count, then (media, frames, one byte per frame, the clip's loudest
/// frame at 255).
fn envelopes(root: &Path, index: &AudioIndex) -> Result<()> {
    let out = root.join("audio").join("envelopes.bin");
    if out.exists() {
        return Ok(());
    }
    use rayon::prelude::*;
    let clips: Vec<(u32, String)> = index
        .media
        .iter()
        .filter(|(m, f)| f.ends_with(".ogg") && index.durations.get(*m).is_some_and(|d| (0.4..30.0).contains(d)))
        .map(|(m, f)| (*m, f.clone()))
        .collect();
    let envs: Vec<(u32, Vec<u8>)> = clips.par_iter().filter_map(|(m, f)| Some((*m, ogg_envelope(&root.join(f))?))).collect();
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&(envs.len() as u32).to_le_bytes());
    for (m, e) in &envs {
        bytes.extend_from_slice(&m.to_le_bytes());
        bytes.extend_from_slice(&(e.len() as u32).to_le_bytes());
        bytes.extend_from_slice(e);
    }
    std::fs::write(&out, bytes)?;
    log::info!("audio: {} loudness envelopes", envs.len());
    Ok(())
}

fn ogg_envelope(path: &Path) -> Option<Vec<u8>> {
    let f = std::fs::File::open(path).ok()?;
    let mut r = lewton::inside_ogg::OggStreamReader::new(std::io::BufReader::new(f)).ok()?;
    let rate = r.ident_hdr.audio_sample_rate as f32;
    let channels = r.ident_hdr.audio_channels.max(1) as usize;
    let window = (rate / ENVELOPE_RATE).max(1.0) as usize;
    let (mut acc, mut n) = (0f64, 0usize);
    let mut rms: Vec<f32> = Vec::new();
    while let Ok(Some(pck)) = r.read_dec_packet_itl() {
        for frame in pck.chunks(channels) {
            let v = frame.iter().map(|s| *s as f64 / 32768.0).sum::<f64>() / channels as f64;
            acc += v * v;
            n += 1;
            if n == window {
                rms.push((acc / n as f64).sqrt() as f32);
                acc = 0.0;
                n = 0;
            }
        }
    }
    let peak = rms.iter().copied().fold(0.0, f32::max);
    if peak <= 1e-4 {
        return None;
    }
    Some(rms.iter().map(|v| (v / peak * 255.0).round() as u8).collect())
}
