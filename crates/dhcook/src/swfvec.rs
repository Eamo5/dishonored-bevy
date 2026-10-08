//! Vector shapes of the Scaleform movies drawn into images at cook time: their solid and
//! gradient fills and their strokes (the game's timeline player only draws bitmaps). Each
//! fill style's edges are linked into closed contours and filled even-odd; each line style's
//! edges are stroked at its width. A bitmap fill that is just the shape's rectangle is left to
//! the player (drawn from the bitmap itself); one cut to another outline (a frame of brush
//! strokes, a torn edge) is drawn here too, the bitmap as its pattern.

use tiny_skia::{Color, FilterQuality, FillRule, GradientStop, IntSize, LinearGradient, Paint, PathBuilder, Pattern, Pixmap, Point, RadialGradient, Shader, SpreadMode, Stroke, Transform};

/// A bitmap's pixels by id: width, height, straight RGBA, its declared size (stage units).
pub type BitmapSource<'a> = &'a dyn Fn(u16) -> Option<(u32, u32, Vec<u8>, [f32; 2])>;

/// A shape drawn: its image's bounds, pixels per unit, size, RGBA, and whether its bitmap fills
/// are in it (the player then draws only the image).
pub struct Drawn {
    pub bounds: [f32; 4],
    pub scale: f32,
    pub w: u32,
    pub h: u32,
    pub rgba: Vec<u8>,
    pub with_bitmaps: bool,
}

/// Pixels per stage unit, and the largest image side.
const SCALE: f32 = 2.0;
const MAX_SIDE: f32 = 2048.0;

struct Bits<'a> {
    d: &'a [u8],
    p: usize,
}

impl<'a> Bits<'a> {
    fn new(d: &'a [u8], byte: usize) -> Self {
        Bits { d, p: byte * 8 }
    }
    fn ub(&mut self, n: u32) -> u32 {
        let mut v = 0u32;
        for _ in 0..n {
            let byte = self.d.get(self.p >> 3).copied().unwrap_or(0);
            v = (v << 1) | ((byte >> (7 - (self.p & 7))) & 1) as u32;
            self.p += 1;
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
        self.p.div_ceil(8)
    }
}

fn u16_at(d: &[u8], p: usize) -> u16 {
    u16::from_le_bytes([d.get(p).copied().unwrap_or(0), d.get(p + 1).copied().unwrap_or(0)])
}

/// [a, b, c, d, tx, ty]: a..d unitless, translation in stage units.
fn matrix(b: &mut Bits) -> [f32; 6] {
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

#[derive(Clone)]
enum Fill {
    Solid([u8; 4]),
    /// linear (false) or radial (true); gradient matrix; spread; stops (ratio, rgba)
    Gradient(bool, [f32; 6], u8, Vec<(f32, [u8; 4])>),
    /// a bitmap fill: the bitmap, its matrix (stage units per bitmap pixel), repeating,
    /// smoothed
    Bitmap(u16, [f32; 6], bool, bool),
}

#[derive(Clone)]
struct Line {
    width: f32,
    color: [u8; 4],
}

fn color(d: &[u8], p: &mut usize, rgba: bool) -> [u8; 4] {
    let c = [d.get(*p).copied().unwrap_or(0), d.get(*p + 1).copied().unwrap_or(0), d.get(*p + 2).copied().unwrap_or(0), if rgba { d.get(*p + 3).copied().unwrap_or(255) } else { 255 }];
    *p += if rgba { 4 } else { 3 };
    c
}

fn fill_style(d: &[u8], p: &mut usize, rgba: bool) -> Option<Fill> {
    let t = *d.get(*p)?;
    *p += 1;
    Some(match t {
        0x00 => Fill::Solid(color(d, p, rgba)),
        0x10 | 0x12 | 0x13 => {
            let mut b = Bits::new(d, *p);
            let m = matrix(&mut b);
            *p = b.byte_pos();
            let h = *d.get(*p)?;
            *p += 1;
            let spread = (h >> 6) & 3;
            let n = (h & 15) as usize;
            let mut stops = Vec::with_capacity(n);
            for _ in 0..n {
                let r = *d.get(*p)? as f32 / 255.0;
                *p += 1;
                stops.push((r, color(d, p, rgba)));
            }
            if t == 0x13 {
                *p += 2;
            }
            Fill::Gradient(t != 0x10, m, spread, stops)
        }
        0x40..=0x43 => {
            let id = u16_at(d, *p);
            *p += 2;
            let mut b = Bits::new(d, *p);
            let m = matrix(&mut b);
            *p = b.byte_pos();
            // (its scale in twips per pixel)
            Fill::Bitmap(id, [m[0] / 20.0, m[1] / 20.0, m[2] / 20.0, m[3] / 20.0, m[4], m[5]], t == 0x40 || t == 0x42, t == 0x40 || t == 0x41)
        }
        _ => return None,
    })
}

fn styles(d: &[u8], p: &mut usize, code: u16) -> Option<(Vec<Fill>, Vec<Line>)> {
    let rgba = code == 32 || code == 83;
    let mut n = *d.get(*p)? as usize;
    *p += 1;
    if n == 0xff && code != 2 {
        n = u16_at(d, *p) as usize;
        *p += 2;
    }
    let mut fills = Vec::with_capacity(n);
    for _ in 0..n {
        fills.push(fill_style(d, p, rgba)?);
    }
    let mut n = *d.get(*p)? as usize;
    *p += 1;
    if n == 0xff {
        n = u16_at(d, *p) as usize;
        *p += 2;
    }
    let mut lines = Vec::with_capacity(n);
    for _ in 0..n {
        let width = u16_at(d, *p) as f32 / 20.0;
        *p += 2;
        if code == 83 {
            let f0 = *d.get(*p)?;
            let f1 = *d.get(*p + 1)?;
            *p += 2;
            let join = (f0 >> 4) & 3;
            let has_fill = f0 & 8 != 0;
            let _ = f1;
            if join == 2 {
                *p += 2;
            }
            if has_fill {
                let f = fill_style(d, p, true)?;
                let c = match f {
                    Fill::Solid(c) => c,
                    Fill::Gradient(_, _, _, s) => s.first().map(|x| x.1).unwrap_or([0, 0, 0, 255]),
                    Fill::Bitmap(..) => [0, 0, 0, 0],
                };
                lines.push(Line { width, color: c });
            } else {
                lines.push(Line { width, color: color(d, p, true) });
            }
        } else {
            lines.push(Line { width, color: color(d, p, rgba) });
        }
    }
    Some((fills, lines))
}

/// An edge: from, optional control point, to (stage units).
type Edge = ((f32, f32), Option<(f32, f32)>, (f32, f32));

/// A DefineShape (tag body, tag code) drawn (`None` when it has nothing but bitmap fills the
/// player draws itself).
pub fn rasterize(d: &[u8], code: u16, bitmaps: BitmapSource) -> Option<Drawn> {
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
    let (mut fills, mut lines) = styles(d, &mut p, code)?;
    // fill and line styles by global index: (group base) + local index - 1
    let mut fill_base = 0usize;
    let mut line_base = 0usize;
    let mut all_fills = fills.clone();
    let mut all_lines = lines.clone();
    let mut fill_edges: Vec<Vec<Edge>> = vec![Vec::new(); all_fills.len()];
    let mut line_edges: Vec<Vec<Edge>> = vec![Vec::new(); all_lines.len()];
    let mut bits = Bits::new(d, p);
    let mut fill_bits = bits.ub(4);
    let mut line_bits = bits.ub(4);
    let (mut x, mut y) = (0.0f32, 0.0f32);
    let (mut f0, mut f1, mut ln) = (0usize, 0usize, 0usize);
    for _ in 0..200_000 {
        if bits.ub(1) == 0 {
            let fl = bits.ub(5);
            if fl == 0 {
                break;
            }
            if fl & 1 != 0 {
                let n = bits.ub(5);
                x = bits.sb(n) as f32 / 20.0;
                y = bits.sb(n) as f32 / 20.0;
            }
            if fl & 2 != 0 {
                f0 = bits.ub(fill_bits) as usize;
            }
            if fl & 4 != 0 {
                f1 = bits.ub(fill_bits) as usize;
            }
            if fl & 8 != 0 {
                ln = bits.ub(line_bits) as usize;
            }
            if fl & 16 != 0 && code != 2 {
                // new styles: a new group of indices
                let mut q = bits.byte_pos();
                let (nf, nl) = styles(d, &mut q, code)?;
                fill_base = all_fills.len();
                line_base = all_lines.len();
                all_fills.extend(nf.iter().cloned());
                all_lines.extend(nl.iter().cloned());
                fill_edges.resize(all_fills.len(), Vec::new());
                line_edges.resize(all_lines.len(), Vec::new());
                fills = nf;
                lines = nl;
                bits = Bits::new(d, q);
                fill_bits = bits.ub(4);
                line_bits = bits.ub(4);
                f0 = 0;
                f1 = 0;
                ln = 0;
            }
        } else {
            let straight = bits.ub(1) == 1;
            let n = bits.ub(4) + 2;
            let from = (x, y);
            let mut ctrl = None;
            if straight {
                if bits.ub(1) == 1 {
                    x += bits.sb(n) as f32 / 20.0;
                    y += bits.sb(n) as f32 / 20.0;
                } else if bits.ub(1) == 1 {
                    y += bits.sb(n) as f32 / 20.0;
                } else {
                    x += bits.sb(n) as f32 / 20.0;
                }
            } else {
                let (cx, cy) = (bits.sb(n) as f32 / 20.0, bits.sb(n) as f32 / 20.0);
                let (ax, ay) = (bits.sb(n) as f32 / 20.0, bits.sb(n) as f32 / 20.0);
                ctrl = Some((x + cx, y + cy));
                x += cx + ax;
                y += cy + ay;
            }
            let to = (x, y);
            if f1 > 0 && f1 <= fills.len() {
                fill_edges[fill_base + f1 - 1].push((from, ctrl, to));
            }
            if f0 > 0 && f0 <= fills.len() {
                fill_edges[fill_base + f0 - 1].push((to, ctrl, from));
            }
            if ln > 0 && ln <= lines.len() {
                line_edges[line_base + ln - 1].push((from, ctrl, to));
            }
        }
    }
    // a bitmap fill the player can draw: its outline the shape's own rectangle
    let rect_fill = |edges: &[Edge]| -> bool {
        if edges.len() != 4 || edges.iter().any(|e| e.1.is_some() || (e.0 .0 != e.2 .0 && e.0 .1 != e.2 .1)) {
            return false;
        }
        let xs = edges.iter().flat_map(|e| [e.0 .0, e.2 .0]);
        let ys = edges.iter().flat_map(|e| [e.0 .1, e.2 .1]);
        let (lx, hx) = xs.fold((f32::MAX, f32::MIN), |(a, b), v| (a.min(v), b.max(v)));
        let (ly, hy) = ys.fold((f32::MAX, f32::MIN), |(a, b), v| (a.min(v), b.max(v)));
        (lx - bounds[0]).abs() < 0.6 && (ly - bounds[1]).abs() < 0.6 && (hx - bounds[2]).abs() < 0.6 && (hy - bounds[3]).abs() < 0.6
    };
    let with_bitmaps = all_fills.iter().enumerate().any(|(i, f)| matches!(f, Fill::Bitmap(..)) && !fill_edges[i].is_empty() && !rect_fill(&fill_edges[i]));
    let drawable = with_bitmaps || all_fills.iter().enumerate().any(|(i, f)| !matches!(f, Fill::Bitmap(..)) && !fill_edges[i].is_empty()) || line_edges.iter().any(|e| !e.is_empty());
    if !drawable {
        return None;
    }
    // the image: the bounds (with the lines' half widths) at the scale, at most MAX_SIDE
    let pad = all_lines.iter().map(|l| l.width).fold(1.0f32, f32::max);
    let (x0, y0, x1, y1) = (bounds[0] - pad, bounds[1] - pad, bounds[2] + pad, bounds[3] + pad);
    let (w, h) = ((x1 - x0).max(1.0), (y1 - y0).max(1.0));
    let s = SCALE.min(MAX_SIDE / w).min(MAX_SIDE / h);
    let (pw, ph) = ((w * s).ceil() as u32, (h * s).ceil() as u32);
    let mut pm = Pixmap::new(pw.max(1), ph.max(1))?;
    let to_px = Transform::from_row(s, 0.0, 0.0, s, -x0 * s, -y0 * s);
    let rgba = |c: [u8; 4]| Color::from_rgba8(c[0], c[1], c[2], c[3]);
    // the bitmaps the fills use, as patterns (premultiplied)
    let mut patterns: std::collections::HashMap<u16, Option<(Pixmap, [f32; 2])>> = std::collections::HashMap::new();
    for (i, f) in all_fills.iter().enumerate() {
        if fill_edges[i].is_empty() || (matches!(f, Fill::Bitmap(..)) && !with_bitmaps) {
            continue;
        }
        let Some(path) = contours(&fill_edges[i], true) else { continue };
        let mut paint = Paint { anti_alias: true, ..Default::default() };
        match f {
            Fill::Solid(c) => paint.set_color(rgba(*c)),
            Fill::Gradient(radial, m, spread, stops) => {
                let gs: Vec<GradientStop> = stops.iter().map(|(r, c)| GradientStop::new(*r, rgba(*c))).collect();
                let mode = match spread {
                    1 => SpreadMode::Reflect,
                    2 => SpreadMode::Repeat,
                    _ => SpreadMode::Pad,
                };
                // the gradient square, -819.2..819.2 stage units, placed by its matrix
                let gt = Transform::from_row(m[0], m[1], m[2], m[3], m[4], m[5]);
                let shader = if *radial {
                    RadialGradient::new(Point::from_xy(0.0, 0.0), Point::from_xy(0.0, 0.0), 819.2, gs, mode, gt)
                } else {
                    LinearGradient::new(Point::from_xy(-819.2, 0.0), Point::from_xy(819.2, 0.0), gs, mode, gt)
                };
                match shader {
                    Some(sh) => paint.shader = sh,
                    None => paint.set_color(rgba(stops.first().map(|x| x.1).unwrap_or([0, 0, 0, 0]))),
                }
                if matches!(paint.shader, Shader::SolidColor(_)) && stops.is_empty() {
                    continue;
                }
            }
            Fill::Bitmap(id, m, repeat, smooth) => {
                let entry = patterns.entry(*id).or_insert_with(|| {
                    let (w, h, px, declared) = bitmaps(*id)?;
                    let mut pre = px;
                    for c in pre.chunks_mut(4) {
                        let a = c[3] as u16;
                        for v in &mut c[..3] {
                            *v = ((*v as u16 * a + 127) / 255) as u8;
                        }
                    }
                    let pm = Pixmap::from_vec(pre, IntSize::from_wh(w, h)?)?;
                    Some((pm, [declared[0] / w as f32, declared[1] / h as f32]))
                });
                let Some((img, k)) = entry.as_ref() else { continue };
                // bitmap pixels -> declared pixels -> stage units
                let t = Transform::from_row(m[0], m[1], m[2], m[3], m[4], m[5]).pre_scale(k[0], k[1]);
                let quality = if *smooth { FilterQuality::Bilinear } else { FilterQuality::Nearest };
                paint.shader = Pattern::new(img.as_ref(), if *repeat { SpreadMode::Repeat } else { SpreadMode::Pad }, quality, 1.0, t);
            }
        }
        pm.fill_path(&path, &paint, FillRule::EvenOdd, to_px, None);
    }
    for (i, l) in all_lines.iter().enumerate() {
        if line_edges[i].is_empty() || l.color[3] == 0 {
            continue;
        }
        let Some(path) = contours(&line_edges[i], false) else { continue };
        let mut paint = Paint { anti_alias: true, ..Default::default() };
        paint.set_color(rgba(l.color));
        // (hairlines: a pixel)
        let stroke = Stroke { width: l.width.max(1.0 / s), ..Default::default() };
        pm.stroke_path(&path, &paint, &stroke, to_px, None);
    }
    // straight (not premultiplied) RGBA
    let px = pm.pixels().iter().flat_map(|p| {
        let c = p.demultiply();
        [c.red(), c.green(), c.blue(), c.alpha()]
    });
    Some(Drawn { bounds: [x0, y0, x1, y1], scale: s, w: pw, h: ph, rgba: px.collect(), with_bitmaps })
}

/// Edges linked end to start into contours (closed for fills).
fn contours(edges: &[Edge], close: bool) -> Option<tiny_skia::Path> {
    let key = |p: (f32, f32)| ((p.0 * 20.0).round() as i64, (p.1 * 20.0).round() as i64);
    let mut by_start: std::collections::HashMap<(i64, i64), Vec<usize>> = std::collections::HashMap::new();
    for (i, e) in edges.iter().enumerate() {
        by_start.entry(key(e.0)).or_default().push(i);
    }
    let mut used = vec![false; edges.len()];
    let mut pb = PathBuilder::new();
    for i in 0..edges.len() {
        if used[i] {
            continue;
        }
        let start = edges[i].0;
        pb.move_to(start.0, start.1);
        let mut cur = i;
        loop {
            used[cur] = true;
            let (_, c, to) = edges[cur];
            match c {
                Some(c) => pb.quad_to(c.0, c.1, to.0, to.1),
                None => pb.line_to(to.0, to.1),
            }
            if key(to) == key(start) {
                break;
            }
            let next = by_start.get(&key(to)).and_then(|v| v.iter().copied().find(|&j| !used[j]));
            match next {
                Some(j) => cur = j,
                None => break,
            }
        }
        if close {
            pb.close();
        }
    }
    pb.finish()
}
