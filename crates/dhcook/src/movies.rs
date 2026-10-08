//! The game's Bink movies (`Movies/*.bik`), cooked once with ffmpeg (its Bink decoder) into
//! MJPEG frames and a WAV soundtrack the game plays: the new game's intro (the Empress's
//! letter, with its subtitles), the Tower's title card, the loading screens' loops (each map's
//! `m_LoadingMovieName`), the credits.
//!
//! A cooked movie (`movies/<name>.dhmv`): `DHMV`, fps (f32), width, height (u32), the frame
//! count (u32), the frames' offsets (u64, one past the last), then the JPEG frames.

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

pub const MOVIE_MAGIC: &[u8; 4] = b"DHMV";

/// What the game needs to know of the cooked movies (`movies/index.json`).
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct MovieIndex {
    /// cooked movies by (lower-case) name: seconds, and whether it has a soundtrack
    pub movies: BTreeMap<String, MovieInfo>,
    /// the loading movie of each map (`DefaultEngine.ini` `m_MapConfig`), lower-case names
    pub maps: BTreeMap<String, String>,
    /// the default one (`[FullScreenMovie] LoadMapMovies`)
    pub default_loading: String,
    /// subtitles by movie: (start, end seconds, text) (`Movies/<name>.txt` cues,
    /// `Localization/INT/Subtitles.int` lines)
    pub subtitles: BTreeMap<String, Vec<(f32, f32, String)>>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct MovieInfo {
    pub seconds: f32,
    pub fps: f32,
    pub width: u32,
    pub height: u32,
    pub audio: bool,
}

/// A cooked movie's frames.
pub struct MovieFile {
    pub fps: f32,
    pub width: u32,
    pub height: u32,
    pub offsets: Vec<u64>,
    pub data: Vec<u8>,
}

impl MovieFile {
    pub fn frames(&self) -> usize {
        self.offsets.len().saturating_sub(1)
    }
    pub fn frame(&self, i: usize) -> Option<&[u8]> {
        let (a, b) = (*self.offsets.get(i)? as usize, *self.offsets.get(i + 1)? as usize);
        self.data.get(a..b)
    }
    pub fn read(path: &Path) -> Result<MovieFile> {
        let d = std::fs::read(path)?;
        if d.len() < 20 || &d[..4] != MOVIE_MAGIC {
            bail!("bad movie {}", path.display());
        }
        let f32_at = |p: usize| f32::from_le_bytes(d[p..p + 4].try_into().unwrap());
        let u32_at = |p: usize| u32::from_le_bytes(d[p..p + 4].try_into().unwrap());
        let (fps, width, height, n) = (f32_at(4), u32_at(8), u32_at(12), u32_at(16) as usize);
        let base = 20 + 8 * (n + 1);
        if d.len() < base {
            bail!("truncated movie {}", path.display());
        }
        let offsets: Vec<u64> = (0..=n).map(|i| u64::from_le_bytes(d[20 + 8 * i..28 + 8 * i].try_into().unwrap())).collect();
        Ok(MovieFile { fps, width, height, offsets, data: d[base..].to_vec() })
    }
}

pub fn movies_dir(cache: &Path) -> PathBuf {
    cache.join("movies")
}
pub fn movie_path(cache: &Path, name: &str) -> PathBuf {
    movies_dir(cache).join(format!("{}.dhmv", name.to_ascii_lowercase()))
}
pub fn audio_path(cache: &Path, name: &str) -> PathBuf {
    movies_dir(cache).join(format!("{}.wav", name.to_ascii_lowercase()))
}
pub fn index_path(cache: &Path) -> PathBuf {
    movies_dir(cache).join("index.json")
}

/// A Bink file's frame rate and length (its header: `BIK?`, frames at 8, fps at 28 / 32).
pub fn bink_header(path: &Path) -> Option<(f32, f32)> {
    let mut f = std::fs::File::open(path).ok()?;
    let mut h = [0u8; 36];
    std::io::Read::read_exact(&mut f, &mut h).ok()?;
    if &h[..3] != b"BIK" && &h[..3] != b"KB2" {
        return None;
    }
    let frames = u32::from_le_bytes(h[8..12].try_into().ok()?) as f32;
    let (num, den) = (u32::from_le_bytes(h[28..32].try_into().ok()?) as f32, u32::from_le_bytes(h[32..36].try_into().ok()?) as f32);
    let fps = if den > 0.0 { num / den } else { 30.0 };
    Some((fps, frames / fps.max(1.0)))
}

/// ffmpeg: `DH_FFMPEG`, else the one on the path.
fn ffmpeg() -> PathBuf {
    std::env::var_os("DH_FFMPEG").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("ffmpeg"))
}

pub fn ffmpeg_available() -> bool {
    Command::new(ffmpeg()).arg("-version").stdout(Stdio::null()).stderr(Stdio::null()).status().is_ok_and(|s| s.success())
}

/// Transcode one movie: its frames scaled to `width` (JPEG quality `q`, ffmpeg's 2..31) and its
/// soundtrack. Returns its info.
pub fn cook_movie(src: &Path, cache: &Path, name: &str, width: u32, q: u32) -> Result<MovieInfo> {
    let (fps, _) = bink_header(src).with_context(|| format!("not a Bink movie: {}", src.display()))?;
    let out = Command::new(ffmpeg())
        .args(["-v", "error", "-i"])
        .arg(src)
        .args(["-an", "-vf", &format!("scale={width}:-2:flags=bicubic"), "-c:v", "mjpeg", "-q:v", &q.to_string(), "-f", "mjpeg", "-"])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .context("running ffmpeg")?;
    if !out.status.success() || out.stdout.is_empty() {
        bail!("ffmpeg failed on {}", src.display());
    }
    let data = out.stdout;
    // the frames: each starts with its SOI marker (byte-stuffing keeps FF D8 FF out of the
    // entropy-coded data)
    let mut starts: Vec<u64> = data.windows(3).enumerate().filter(|(_, w)| w[0] == 0xFF && w[1] == 0xD8 && w[2] == 0xFF).map(|(i, _)| i as u64).collect();
    if starts.is_empty() {
        bail!("no frames in {}", src.display());
    }
    starts.push(data.len() as u64);
    // the size of the first frame
    let (w, h) = jpeg_size(&data[starts[0] as usize..starts[1] as usize]).unwrap_or((width, width * 9 / 16));
    let n = starts.len() - 1;
    let mut file = Vec::with_capacity(20 + 8 * (n + 1) + data.len());
    file.extend_from_slice(MOVIE_MAGIC);
    file.extend_from_slice(&fps.to_le_bytes());
    file.extend_from_slice(&w.to_le_bytes());
    file.extend_from_slice(&h.to_le_bytes());
    file.extend_from_slice(&(n as u32).to_le_bytes());
    let base = starts[0];
    for s in &starts {
        file.extend_from_slice(&(s - base).to_le_bytes());
    }
    file.extend_from_slice(&data[base as usize..]);
    std::fs::create_dir_all(movies_dir(cache))?;
    crate::format::write_atomic(&movie_path(cache, name), &file)?;
    // the soundtrack, where there is one
    let wav = audio_path(cache, name);
    let audio = Command::new(ffmpeg())
        .args(["-v", "error", "-y", "-i"])
        .arg(src)
        .args(["-vn", "-ac", "2", "-ar", "48000", "-c:a", "pcm_s16le"])
        .arg(&wav)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
        && std::fs::metadata(&wav).is_ok_and(|m| m.len() > 1024);
    if !audio {
        let _ = std::fs::remove_file(&wav);
    }
    Ok(MovieInfo { seconds: n as f32 / fps.max(1.0), fps, width: w, height: h, audio })
}

/// A JPEG's size (its SOF marker).
fn jpeg_size(d: &[u8]) -> Option<(u32, u32)> {
    let mut p = 2;
    while p + 9 < d.len() {
        if d[p] != 0xFF {
            return None;
        }
        let m = d[p + 1];
        let len = u16::from_be_bytes([d[p + 2], d[p + 3]]) as usize;
        if (0xC0..=0xC3).contains(&m) {
            let h = u16::from_be_bytes([d[p + 5], d[p + 6]]) as u32;
            let w = u16::from_be_bytes([d[p + 7], d[p + 8]]) as u32;
            return Some((w, h));
        }
        p += 2 + len;
    }
    None
}

/// `Key="text"` lines of a UTF-16 localization file.
fn int_strings(path: &Path) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    let Ok(d) = std::fs::read(path) else { return out };
    let text = if d.len() >= 2 && d[0] == 0xFF && d[1] == 0xFE {
        let u: Vec<u16> = d[2..].chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
        String::from_utf16_lossy(&u)
    } else {
        String::from_utf8_lossy(&d).into_owned()
    };
    for line in text.lines() {
        if let Some((k, v)) = line.split_once('=') {
            let v = v.trim().trim_matches('"').replace("\\n", "\n");
            out.insert(k.trim().to_string(), v);
        }
    }
    out
}

/// The movies to cook: (name, width, quality).
fn wanted(index: &MovieIndex, available: &[String]) -> Vec<(String, u32, u32)> {
    let mut names: Vec<String> = vec!["Dishonored".into(), "INTRO_LOC".into(), "LoadingSavingNotification".into()];
    names.push(index.default_loading.clone());
    names.extend(index.maps.values().cloned());
    let mut out: Vec<(String, u32, u32)> = Vec::new();
    for n in names {
        // (the original spelling of the file)
        let Some(file) = available.iter().find(|a| a.eq_ignore_ascii_case(&n)) else { continue };
        if out.iter().any(|(o, _, _)| o.eq_ignore_ascii_case(file)) {
            continue;
        }
        out.push((file.clone(), 1280, 5));
    }
    // the credits (nine minutes of them) smaller
    if let Some(c) = available.iter().find(|a| a.eq_ignore_ascii_case("Credits")) {
        out.push((c.clone(), 960, 7));
    }
    out
}

/// Cook the game's movies into `cache/movies` (skipping those done; nothing without ffmpeg).
/// `game` is the install's `DishonoredGame` folder.
pub fn cook_movies(game: &Path, cache: &Path, force: bool) -> Result<MovieIndex> {
    let mut index = MovieIndex { default_loading: "Loading".into(), ..Default::default() };
    // each map's loading movie
    let engine = std::fs::read_to_string(game.join("Config").join("DefaultEngine.ini")).unwrap_or_default();
    for line in engine.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("+m_MapConfig=(") {
            let field = |k: &str| rest.split(',').find_map(|f| f.trim().strip_prefix(k)).map(|v| v.trim_matches(|c| c == '"' || c == ')').to_string());
            if let (Some(map), Some(movie)) = (field("m_Name="), field("m_LoadingMovieName=")) {
                if !movie.is_empty() {
                    index.maps.insert(map.to_ascii_lowercase(), movie);
                }
            }
        } else if let Some(m) = line.strip_prefix("+LoadMapMovies=") {
            index.default_loading = m.trim().to_string();
        }
    }
    // subtitles: `Movies/<name>.txt` cues (start ms, end ms, key) with the localized lines
    let subs = int_strings(&game.join("Localization").join("INT").join("Subtitles.int"));
    let movies = game.join("Movies");
    let available: Vec<String> = std::fs::read_dir(&movies)
        .map(|rd| rd.filter_map(|e| e.ok()).filter_map(|e| e.file_name().to_str()?.strip_suffix(".bik").map(str::to_string)).collect())
        .unwrap_or_default();
    for name in &available {
        let Ok(cues) = std::fs::read_to_string(movies.join(format!("{name}.txt"))) else { continue };
        let list: Vec<(f32, f32, String)> = cues
            .lines()
            .filter_map(|l| {
                let mut f = l.split(',');
                let (a, b, k) = (f.next()?.trim().parse::<f32>().ok()?, f.next()?.trim().parse::<f32>().ok()?, f.next()?.trim());
                Some((a / 1000.0, b / 1000.0, subs.get(k)?.clone()))
            })
            .collect();
        if !list.is_empty() {
            index.subtitles.insert(name.to_ascii_lowercase(), list);
        }
    }
    // the movies themselves (in parallel: ffmpeg's Bink decoder runs on one core)
    let have_ffmpeg = ffmpeg_available();
    if !have_ffmpeg {
        log::warn!("movies: ffmpeg not found (set DH_FFMPEG): the movies are left out");
    }
    let old: MovieIndex = std::fs::read(index_path(cache)).ok().and_then(|d| serde_json::from_slice(&d).ok()).unwrap_or_default();
    let jobs = wanted(&index, &available);
    let done: Vec<(String, MovieInfo)> = {
        use rayon::prelude::*;
        jobs.par_iter()
            .filter_map(|(name, width, q)| {
                let key = name.to_ascii_lowercase();
                if !force && movie_path(cache, name).exists() {
                    if let Some(i) = old.movies.get(&key) {
                        return Some((key, i.clone()));
                    }
                }
                if !have_ffmpeg {
                    return None;
                }
                let t = std::time::Instant::now();
                match cook_movie(&movies.join(format!("{name}.bik")), cache, name, *width, *q) {
                    Ok(i) => {
                        log::info!("movie {name}: {:.1} s, {}x{}, audio {} in {:.1}s", i.seconds, i.width, i.height, i.audio, t.elapsed().as_secs_f32());
                        Some((key, i))
                    }
                    Err(e) => {
                        log::warn!("movie {name}: {e:#}");
                        None
                    }
                }
            })
            .collect()
    };
    index.movies = done.into_iter().collect();
    std::fs::create_dir_all(movies_dir(cache))?;
    crate::format::write_atomic(&index_path(cache), &serde_json::to_vec_pretty(&index)?)?;
    Ok(index)
}
