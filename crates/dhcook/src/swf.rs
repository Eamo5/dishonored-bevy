//! Scaleform GFx movies (SWF with GFx extension tags), as stored in `SwfMovie.RawData`:
//! tag walking, the texture atlas sub-images (`DefineExternalImage2` / `DefineSubImage`),
//! exported symbol names, and `DefineFont3` glyph outlines (converted to TrueType by
//! [`crate::ttf`]).

use anyhow::{bail, Result};
use std::collections::HashMap;

/// A tag: code and its body range in the (decompressed) movie.
#[derive(Clone, Copy, Debug)]
pub struct Tag {
    pub code: u16,
    pub start: usize,
    pub len: usize,
    /// nesting depth (tags inside DefineSprite)
    pub depth: u8,
}

pub struct Movie {
    pub version: u8,
    pub body: Vec<u8>,
    pub frame: [f32; 4],
    pub tags: Vec<Tag>,
}

struct Bits<'a> {
    d: &'a [u8],
    pos: usize,
}

impl<'a> Bits<'a> {
    fn new(d: &'a [u8], byte: usize) -> Self {
        Bits { d, pos: byte * 8 }
    }
    fn ub(&mut self, n: u32) -> u32 {
        let mut v = 0u32;
        for _ in 0..n {
            let b = self.d.get(self.pos / 8).copied().unwrap_or(0);
            v = (v << 1) | ((b >> (7 - self.pos % 8)) & 1) as u32;
            self.pos += 1;
        }
        v
    }
    fn sb(&mut self, n: u32) -> i32 {
        if n == 0 {
            return 0;
        }
        let v = self.ub(n) as i32;
        if v & (1 << (n - 1)) != 0 {
            v - (1 << n)
        } else {
            v
        }
    }
    fn byte_pos(&self) -> usize {
        self.pos.div_ceil(8)
    }
}

fn u16_at(d: &[u8], p: usize) -> u16 {
    u16::from_le_bytes([d.get(p).copied().unwrap_or(0), d.get(p + 1).copied().unwrap_or(0)])
}

fn u32_at(d: &[u8], p: usize) -> u32 {
    u32::from_le_bytes([0, 1, 2, 3].map(|i| d.get(p + i).copied().unwrap_or(0)))
}

impl Movie {
    pub fn parse(raw: &[u8]) -> Result<Movie> {
        if raw.len() < 8 {
            bail!("short movie");
        }
        let sig = &raw[..3];
        let version = raw[3];
        let body = match sig {
            b"GFX" | b"FWS" => raw[8..].to_vec(),
            b"CFX" | b"CWS" => {
                use std::io::Read;
                let mut out = Vec::new();
                flate2::read::ZlibDecoder::new(&raw[8..]).read_to_end(&mut out)?;
                out
            }
            _ => bail!("not a GFx/SWF movie"),
        };
        let mut b = Bits::new(&body, 0);
        let n = b.ub(5);
        let r = [b.sb(n), b.sb(n), b.sb(n), b.sb(n)];
        let p = b.byte_pos() + 4;
        let mut m = Movie { version, frame: r.map(|v| v as f32 / 20.0), tags: Vec::new(), body: Vec::new() };
        let mut tags = Vec::new();
        walk(&body, p, body.len(), 0, &mut tags);
        m.tags = tags;
        m.body = body;
        Ok(m)
    }

    /// Exported symbol names by character id.
    pub fn exports(&self) -> HashMap<u16, String> {
        let mut out = HashMap::new();
        for t in self.tags.iter().filter(|t| t.code == 56) {
            let d = &self.body[t.start..t.start + t.len];
            let n = u16_at(d, 0) as usize;
            let mut q = 2;
            for _ in 0..n {
                let id = u16_at(d, q);
                q += 2;
                let e = d[q..].iter().position(|&c| c == 0).map(|i| q + i).unwrap_or(d.len());
                out.insert(id, String::from_utf8_lossy(&d[q..e]).to_string());
                q = e + 1;
            }
        }
        out
    }

    /// The symbols it imports from other movies (`ImportAssets`/`ImportAssets2`): (the id it
    /// gives each, the other movie's file, its export name).
    pub fn imports(&self) -> Vec<(u16, String, String)> {
        let mut out = Vec::new();
        for t in self.tags.iter().filter(|t| t.code == 57 || t.code == 71) {
            let d = &self.body[t.start..t.start + t.len];
            let (url, mut q) = cstr(d, 0);
            if t.code == 71 {
                q += 2;
            }
            let n = u16_at(d, q) as usize;
            q += 2;
            for _ in 0..n {
                let id = u16_at(d, q);
                let (name, e) = cstr(d, q + 2);
                q = e;
                out.push((id, url.clone(), name));
            }
        }
        out
    }

    /// Texture atlases (`DefineExternalImage2`: id -> (file name, declared size)) and the
    /// sub-images cut from them (`DefineSubImage`: id -> (atlas index, x1, y1, x2, y2)).
    pub fn images(&self) -> (Vec<(u32, String, u16, u16)>, HashMap<u16, (u16, [u16; 4])>) {
        let mut atlases = Vec::new();
        let mut subs = HashMap::new();
        for t in &self.tags {
            let d = &self.body[t.start..t.start + t.len];
            match t.code {
                1009 => {
                    let id = u32_at(d, 0);
                    let (w, h) = (u16_at(d, 6), u16_at(d, 8));
                    let en = d[10] as usize;
                    let fl = d.get(11 + en).copied().unwrap_or(0) as usize;
                    let file = String::from_utf8_lossy(&d[12 + en..(12 + en + fl).min(d.len())]).to_string();
                    atlases.push((id, file, w, h));
                }
                1008 => {
                    subs.insert(u16_at(d, 0), (u16_at(d, 2), [u16_at(d, 4), u16_at(d, 6), u16_at(d, 8), u16_at(d, 10)]));
                }
                _ => {}
            }
        }
        (atlases, subs)
    }

    /// The `DefineFont3` fonts of the movie.
    pub fn fonts(&self) -> Vec<Font> {
        self.tags.iter().filter(|t| t.code == 75).filter_map(|t| parse_font3(&self.body[t.start..t.start + t.len]).ok()).collect()
    }
}

fn walk(d: &[u8], mut p: usize, end: usize, depth: u8, out: &mut Vec<Tag>) {
    while p + 2 <= end {
        let h = u16_at(d, p);
        p += 2;
        let code = h >> 6;
        let mut len = (h & 0x3f) as usize;
        if len == 0x3f {
            len = u32_at(d, p) as usize;
            p += 4;
        }
        if p + len > end {
            break;
        }
        out.push(Tag { code, start: p, len, depth });
        if code == 39 && len > 4 {
            walk(d, p + 4, p + len, depth + 1, out);
        }
        p += len;
        if code == 0 && depth > 0 {
            break;
        }
    }
}

/// A glyph outline: contours of points (x, y, on-curve) in font units (1024 per em, y up).
#[derive(Clone, Debug, Default)]
pub struct Glyph {
    pub code: u16,
    pub contours: Vec<Vec<(i32, i32, bool)>>,
    pub advance: i32,
}

#[derive(Clone, Debug, Default)]
pub struct Font {
    pub name: String,
    pub bold: bool,
    pub italic: bool,
    pub ascent: i32,
    pub descent: i32,
    pub leading: i32,
    pub glyphs: Vec<Glyph>,
}

/// DefineFont3: coordinates in 1/20 of a 1024-unit em square (y down).
fn parse_font3(d: &[u8]) -> Result<Font> {
    let flags = d[2];
    let has_layout = flags & 0x80 != 0;
    let wide_offsets = flags & 0x08 != 0;
    let wide_codes = flags & 0x04 != 0;
    let nl = d[4] as usize;
    let name = String::from_utf8_lossy(&d[5..5 + nl]).trim_end_matches('\0').to_string();
    let mut p = 5 + nl;
    let ng = u16_at(d, p) as usize;
    p += 2;
    let table = p;
    let off = |i: usize| if wide_offsets { u32_at(d, table + i * 4) as usize } else { u16_at(d, table + i * 2) as usize };
    let code_off = off(ng);
    let mut glyphs = Vec::with_capacity(ng);
    for i in 0..ng {
        let start = table + off(i);
        glyphs.push(Glyph { contours: shape_contours(d, start), ..Default::default() });
    }
    let mut q = table + code_off;
    for g in glyphs.iter_mut() {
        g.code = if wide_codes { u16_at(d, q) } else { d[q] as u16 };
        q += if wide_codes { 2 } else { 1 };
    }
    let mut f = Font { name, bold: flags & 1 != 0, italic: flags & 2 != 0, glyphs, ..Default::default() };
    if has_layout {
        f.ascent = u16_at(d, q) as i32 / 20;
        f.descent = u16_at(d, q + 2) as i32 / 20;
        f.leading = u16_at(d, q + 4) as i16 as i32 / 20;
        q += 6;
        for g in f.glyphs.iter_mut() {
            g.advance = u16_at(d, q) as i16 as i32 / 20;
            q += 2;
        }
    }
    Ok(f)
}

/// The edges of a glyph SHAPE as contours (twips / 20, y flipped).
fn shape_contours(d: &[u8], start: usize) -> Vec<Vec<(i32, i32, bool)>> {
    let mut b = Bits::new(d, start);
    let mut fill_bits = b.ub(4);
    let mut line_bits = b.ub(4);
    let (mut x, mut y) = (0i32, 0i32);
    let mut contours: Vec<Vec<(i32, i32, bool)>> = Vec::new();
    let mut cur: Vec<(i32, i32, bool)> = Vec::new();
    let pt = |x: i32, y: i32, on: bool| ((x as f32 / 20.0).round() as i32, (-y as f32 / 20.0).round() as i32, on);
    let flush = |cur: &mut Vec<(i32, i32, bool)>, contours: &mut Vec<Vec<(i32, i32, bool)>>| {
        if cur.len() > 2 {
            // drop the closing point if it repeats the start
            if cur.first().map(|p| (p.0, p.1)) == cur.last().map(|p| (p.0, p.1)) {
                cur.pop();
            }
            contours.push(std::mem::take(cur));
        } else {
            cur.clear();
        }
    };
    for _ in 0..100_000 {
        let edge = b.ub(1);
        if edge == 0 {
            let fl = b.ub(5);
            if fl == 0 {
                break;
            }
            if fl & 1 != 0 {
                let n = b.ub(5);
                x = b.sb(n);
                y = b.sb(n);
                flush(&mut cur, &mut contours);
                cur.push(pt(x, y, true));
            }
            if fl & 2 != 0 {
                b.ub(fill_bits);
            }
            if fl & 4 != 0 {
                b.ub(fill_bits);
            }
            if fl & 8 != 0 {
                b.ub(line_bits);
            }
            if fl & 16 != 0 {
                // new styles: not used by glyphs
                fill_bits = b.ub(4);
                line_bits = b.ub(4);
            }
        } else {
            let straight = b.ub(1);
            let n = b.ub(4) + 2;
            if cur.is_empty() {
                cur.push(pt(x, y, true));
            }
            if straight == 1 {
                let general = b.ub(1);
                if general == 1 {
                    x += b.sb(n);
                    y += b.sb(n);
                } else if b.ub(1) == 1 {
                    y += b.sb(n);
                } else {
                    x += b.sb(n);
                }
                cur.push(pt(x, y, true));
            } else {
                let (cx, cy) = (b.sb(n), b.sb(n));
                let (ax, ay) = (b.sb(n), b.sb(n));
                cur.push(pt(x + cx, y + cy, false));
                x += cx + ax;
                y += cy + ay;
                cur.push(pt(x, y, true));
            }
        }
    }
    flush(&mut cur, &mut contours);
    contours
}

fn cstr(d: &[u8], p: usize) -> (String, usize) {
    let p = p.min(d.len());
    let e = d[p..].iter().position(|&c| c == 0).map(|i| p + i).unwrap_or(d.len());
    (String::from_utf8_lossy(&d[p..e]).to_string(), e + 1)
}

/// A MATRIX record: [a, b, c, d, tx, ty] (translation in stage units).
fn read_matrix(b: &mut Bits) -> [f32; 6] {
    let mut m = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];
    if b.ub(1) == 1 {
        let n = b.ub(5);
        m[0] = b.sb(n) as f32 / 65536.0;
        m[3] = b.sb(n) as f32 / 65536.0;
    }
    if b.ub(1) == 1 {
        let n = b.ub(5);
        m[1] = b.sb(n) as f32 / 65536.0;
        m[2] = b.sb(n) as f32 / 65536.0;
    }
    let n = b.ub(5);
    m[4] = b.sb(n) as f32 / 20.0;
    m[5] = b.sb(n) as f32 / 20.0;
    m
}

/// A CXFORMWITHALPHA record: multiply (0..1) then add (0..255) terms.
fn read_cxform(b: &mut Bits) -> [f32; 8] {
    let has_add = b.ub(1) == 1;
    let has_mult = b.ub(1) == 1;
    let n = b.ub(4);
    let mut cx = [1.0, 1.0, 1.0, 1.0, 0.0, 0.0, 0.0, 0.0];
    if has_mult {
        for v in cx.iter_mut().take(4) {
            *v = b.sb(n) as f32 / 256.0;
        }
    }
    if has_add {
        for v in cx.iter_mut().skip(4) {
            *v = b.sb(n) as f32;
        }
    }
    cx
}

/// A frame script's controls of its own clip: `stop()`, `play()`, `gotoAndPlay/Stop` with a
/// frame or label, `this._visible = ...`.
fn frame_actions(d: &[u8]) -> Vec<crate::format::TlAction> {
    use crate::format::TlAction;
    #[derive(Clone, PartialEq)]
    enum V {
        Str(String),
        Bool(bool),
        Var(String),
        Other,
    }
    let mut out = Vec::new();
    let mut pool: Vec<String> = Vec::new();
    let mut stack: Vec<V> = Vec::new();
    let mut p = 0;
    while p < d.len() {
        let op = d[p];
        p += 1;
        if op == 0 {
            break;
        }
        let (body, next) = if op >= 0x80 {
            let len = u16_at(d, p) as usize;
            (&d[(p + 2).min(d.len())..(p + 2 + len).min(d.len())], p + 2 + len)
        } else {
            (&d[0..0], p)
        };
        p = next;
        match op {
            0x07 => out.push(TlAction::Stop),
            0x06 => out.push(TlAction::Play),
            0x81 => out.push(TlAction::GotoFrame(u16_at(body, 0))),
            0x8C => out.push(TlAction::GotoLabel(cstr(body, 0).0)),
            0x88 => {
                pool.clear();
                let n = u16_at(body, 0) as usize;
                let mut q = 2;
                for _ in 0..n {
                    let (s, e) = cstr(body, q);
                    pool.push(s);
                    q = e;
                }
            }
            0x96 => {
                let mut q = 0;
                while q < body.len() {
                    let t = body[q];
                    q += 1;
                    let v = match t {
                        0 => {
                            let (s, e) = cstr(body, q);
                            q = e;
                            V::Str(s)
                        }
                        1 | 7 => {
                            q += 4;
                            V::Other
                        }
                        2 | 3 => V::Other,
                        4 => {
                            q += 1;
                            V::Other
                        }
                        5 => {
                            q += 1;
                            V::Bool(body.get(q - 1).copied().unwrap_or(0) != 0)
                        }
                        6 => {
                            q += 8;
                            V::Other
                        }
                        8 => {
                            q += 1;
                            pool.get(body.get(q - 1).copied().unwrap_or(0) as usize).cloned().map(V::Str).unwrap_or(V::Other)
                        }
                        9 => {
                            q += 2;
                            pool.get(u16_at(body, q - 2) as usize).cloned().map(V::Str).unwrap_or(V::Other)
                        }
                        _ => break,
                    };
                    stack.push(v);
                }
            }
            // GetVariable
            0x1C => {
                let v = match stack.pop() {
                    Some(V::Str(s)) => V::Var(s),
                    _ => V::Other,
                };
                stack.push(v);
            }
            // SetMember
            0x4F => {
                let value = stack.pop();
                let name = stack.pop();
                let obj = stack.pop();
                if let (Some(V::Bool(v)), Some(V::Str(n)), Some(V::Var(o))) = (value, name, obj) {
                    if o == "this" && n == "_visible" {
                        out.push(TlAction::Visible(v));
                    }
                }
            }
            _ => stack.clear(),
        }
    }
    out
}

/// A DefineShape's bounds (x min, y min, x max, y max) and bitmap fills (bitmap id, fill
/// matrix, repeating).
#[allow(clippy::type_complexity)]
fn shape_bitmaps(d: &[u8], code: u16) -> (u16, [f32; 4], Vec<(u16, [f32; 6], bool)>) {
    let id = u16_at(d, 0);
    let mut b = Bits::new(d, 2);
    let n = b.ub(5);
    let r: Vec<f32> = (0..4).map(|_| b.sb(n) as f32 / 20.0).collect();
    let bounds = [r[0], r[2], r[1], r[3]];
    let mut p = b.byte_pos();
    if code == 83 {
        let mut b = Bits::new(d, p);
        let n = b.ub(5);
        for _ in 0..4 {
            b.sb(n);
        }
        p = b.byte_pos() + 1;
    }
    let mut count = d.get(p).copied().unwrap_or(0) as usize;
    p += 1;
    if count == 0xff && code != 2 {
        count = u16_at(d, p) as usize;
        p += 2;
    }
    let rgba = code == 32 || code == 83;
    let mut out = Vec::new();
    for _ in 0..count {
        let Some(&t) = d.get(p) else { break };
        p += 1;
        match t {
            0x00 => p += if rgba { 4 } else { 3 },
            0x10 | 0x12 | 0x13 => {
                let mut b = Bits::new(d, p);
                read_matrix(&mut b);
                p = b.byte_pos();
                let ng = (d.get(p).copied().unwrap_or(0) & 15) as usize;
                p += 1 + ng * if rgba { 5 } else { 4 };
                if t == 0x13 {
                    p += 2;
                }
            }
            0x40..=0x43 => {
                let bid = u16_at(d, p);
                let mut b = Bits::new(d, p + 2);
                let m = read_matrix(&mut b);
                p = b.byte_pos();
                if bid != 0xffff {
                    out.push((bid, m, t == 0x40 || t == 0x42));
                }
            }
            _ => break,
        }
    }
    (id, bounds, out)
}

/// Vector shapes drawn into images (`swfvec`) get synthetic bitmap ids from here.
pub const VECTOR_IMAGE_BASE: u16 = 40000;

/// A shape drawn into an image: (synthetic bitmap id, width, height, RGBA).
pub type VectorImage = (u16, u32, u32, Vec<u8>);

/// Where shapes are drawn into images (`swfvec`): the images made, and the bitmaps' pixels by
/// id (width, height, RGBA) for the shapes filled with them.
pub struct Raster<'a> {
    pub out: &'a mut Vec<VectorImage>,
    pub bitmap: &'a dyn Fn(u16) -> Option<(u32, u32, Vec<u8>)>,
}

#[allow(clippy::too_many_arguments)]
fn walk_timeline(d: &[u8], mut p: usize, end: usize, frames: &mut Vec<crate::format::TlFrame>, tl: &mut crate::format::Timelines, declared: &dyn Fn(u16) -> Option<[f32; 2]>, vectors: &mut Option<Raster>) {
    use crate::format::{ShapeBitmap, SpriteTimeline, TlFrame, TlOp};
    let mut cur = TlFrame::default();
    while p + 2 <= end {
        let h = u16_at(d, p);
        p += 2;
        let code = h >> 6;
        let mut len = (h & 0x3f) as usize;
        if len == 0x3f {
            len = u32_at(d, p) as usize;
            p += 4;
        }
        if p + len > end {
            break;
        }
        let start = p;
        let body = &d[p..p + len];
        p += len;
        match code {
            0 => break,
            1 => frames.push(std::mem::take(&mut cur)),
            2 | 22 | 32 | 83 => {
                let (id, bounds, fills) = shape_bitmaps(body, code);
                tl.bounds.insert(id, bounds);
                let mut bm: Vec<ShapeBitmap> = fills
                    .into_iter()
                    .filter_map(|(bid, m, repeat)| Some(ShapeBitmap { bitmap: bid, size: declared(bid)?, m: [m[0] / 20.0, m[1] / 20.0, m[2] / 20.0, m[3] / 20.0, m[4], m[5]], bounds: Some(bounds), repeat }))
                    .collect();
                // its solid and gradient fills and strokes, drawn into an image beneath them (its
                // bitmap fills too when they are cut to an outline: then only the image)
                if let Some(r) = vectors.as_mut() {
                    let pixels = |bid: u16| -> Option<(u32, u32, Vec<u8>, [f32; 2])> {
                        let (w, h, px) = (r.bitmap)(bid)?;
                        Some((w, h, px, declared(bid)?))
                    };
                    if let Some(dr) = crate::swfvec::rasterize(body, code, &pixels) {
                        let synth = VECTOR_IMAGE_BASE + id;
                        let (bb, s) = (dr.bounds, dr.scale);
                        if dr.with_bitmaps {
                            bm.clear();
                        }
                        bm.insert(0, ShapeBitmap { bitmap: synth, size: [dr.w as f32, dr.h as f32], m: [1.0 / s, 0.0, 0.0, 1.0 / s, bb[0], bb[1]], bounds: Some(bounds), repeat: false });
                        r.out.push((synth, dr.w, dr.h, dr.rgba));
                    }
                }
                if !bm.is_empty() {
                    tl.shapes.insert(id, bm);
                }
            }
            39 if len >= 4 => {
                let id = u16_at(body, 0);
                let mut sf = Vec::new();
                walk_timeline(d, start + 4, p, &mut sf, tl, declared, vectors);
                tl.sprites.insert(id, SpriteTimeline { frames: sf });
            }
            26 | 70 if len >= 3 => {
                let f = body[0];
                let mut q = 1;
                let mut f2 = 0;
                if code == 70 {
                    f2 = body[1];
                    q = 2;
                }
                let depth = u16_at(body, q);
                q += 2;
                if code == 70 && f2 & 8 != 0 {
                    q = cstr(body, q).1;
                }
                let id = (f & 2 != 0).then(|| {
                    q += 2;
                    u16_at(body, q - 2)
                });
                let m = (f & 4 != 0).then(|| {
                    let mut b = Bits::new(body, q);
                    let m = read_matrix(&mut b);
                    q = b.byte_pos();
                    m
                });
                let cx = (f & 8 != 0).then(|| {
                    let mut b = Bits::new(body, q);
                    let c = read_cxform(&mut b);
                    q = b.byte_pos();
                    c
                });
                if f & 16 != 0 {
                    q += 2;
                }
                let name = (f & 32 != 0).then(|| {
                    let (s, e) = cstr(body, q);
                    q = e;
                    s
                });
                let clip = (f & 64 != 0).then(|| u16_at(body, q));
                cur.ops.push(TlOp::Place { depth, moved: f & 1 != 0, id, name, m, cx, clip });
            }
            28 => cur.ops.push(TlOp::Remove(u16_at(body, 0))),
            5 => cur.ops.push(TlOp::Remove(u16_at(body, 2))),
            43 => cur.labels.push(cstr(body, 0).0),
            12 => cur.actions.extend(frame_actions(body)),
            _ => {}
        }
    }
}

impl Movie {
    /// The movie's sprites' timelines (the main timeline as sprite 0) and its shapes' bitmaps.
    pub fn timelines(&self) -> crate::format::Timelines {
        self.timelines_with(None)
    }

    /// The timelines, the vector shapes drawn into images too (`vectors`: their pixels).
    pub fn timelines_with(&self, mut vectors: Option<Raster>) -> crate::format::Timelines {
        use crate::format::{SpriteTimeline, Timelines};
        let d = &self.body;
        let mut b = Bits::new(d, 0);
        let n = b.ub(5);
        let p0 = (5 + 4 * n as usize).div_ceil(8);
        let rate = u16_at(d, p0) as f32 / 256.0;
        let (atlases, subs) = self.images();
        let declared = |id: u16| -> Option<[f32; 2]> {
            if let Some((_, r)) = subs.get(&id) {
                return Some([r[2].saturating_sub(r[0]) as f32, r[3].saturating_sub(r[1]) as f32]);
            }
            atlases.iter().find(|a| a.0 == id as u32).map(|a| [a.2 as f32, a.3 as f32])
        };
        let mut tl = Timelines { rate, ..Default::default() };
        for (id, name) in self.exports() {
            tl.exports.insert(name, id);
        }
        let mut root = Vec::new();
        walk_timeline(d, p0 + 4, d.len(), &mut root, &mut tl, &declared, &mut vectors);
        tl.sprites.insert(0, SpriteTimeline { frames: root });
        tl
    }
}
