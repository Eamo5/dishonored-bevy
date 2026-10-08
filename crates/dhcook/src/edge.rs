//! Decoder for Dishonored's `ACF_EdgeAnim` animation sequences (PlayStation Edge
//! animation format, also used by the PC build).
//!
//! Layout of the blob (all little-endian, offsets at 0x38.. are self-relative):
//! - `0x00` tag `50AE`, `0x04` duration, `0x08` sample frequency
//! - `0x0e` joint count, `0x10` frame count, `0x12` frame-set count
//! - `0x16` channel counts: constant R/T/S/U, animated R/T/S/U; `0x26` flags (bit 0: packed
//!   rotations, otherwise 48-bit smallest-three quaternions)
//! - `0x38` frame-set DMA table, `0x3c` frame-set info, `0x40..0x4c` constant data,
//!   `0x50` packing specs
//! - `0x60` joint lists (constant rotations padded to 8 entries, the others to 4)
//!
//! Each frame set holds a base key for every animated channel, then per-channel bit masks
//! of the intra frames that carry a key, then the keys. Values are bit-packed MSB-first;
//! a packing spec gives, per component, a mantissa/exponent/sign width. Frames without a
//! key are linearly interpolated between the surrounding keys.
//!
//! Joint indices refer to the animated skeleton's bones in mesh order (the mesh's bones
//! that the AnimSet has tracks for).

use anyhow::{bail, Result};

pub const TAG: u32 = 0x4541_3035;

/// One channel: keyed frames (ascending) and their values.
#[derive(Debug, Clone, Default)]
pub struct Track<T> {
    pub joint: u16,
    pub frames: Vec<u16>,
    pub values: Vec<T>,
}

/// A decoded sequence, in UE space (local joint transforms, centimetres).
#[derive(Debug, Clone, Default)]
pub struct EdgeAnim {
    pub num_joints: usize,
    pub num_frames: usize,
    pub duration: f32,
    pub frequency: f32,
    pub rotations: Vec<Track<[f32; 4]>>,
    pub translations: Vec<Track<[f32; 3]>>,
    pub scales: Vec<Track<[f32; 3]>>,
}

fn u16_at(d: &[u8], o: usize) -> Result<u16> {
    match d.get(o..o + 2) {
        Some(b) => Ok(u16::from_le_bytes([b[0], b[1]])),
        None => bail!("edge blob truncated at {o:#x}"),
    }
}

fn u32_at(d: &[u8], o: usize) -> Result<u32> {
    match d.get(o..o + 4) {
        Some(b) => Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]])),
        None => bail!("edge blob truncated at {o:#x}"),
    }
}

/// MSB-first bit reader.
struct Bits<'a> {
    d: &'a [u8],
    pos: usize,
}

impl<'a> Bits<'a> {
    fn new(d: &'a [u8]) -> Self {
        Bits { d, pos: 0 }
    }
    fn get(&mut self, w: u32) -> u64 {
        let mut v = 0u64;
        for _ in 0..w {
            let byte = self.d.get(self.pos >> 3).copied().unwrap_or(0);
            v = (v << 1) | ((byte >> (7 - (self.pos & 7))) & 1) as u64;
            self.pos += 1;
        }
        v
    }
}

/// Component encoding: `m` mantissa bits, `e` exponent bits, `s` sign bit.
#[derive(Clone, Copy, Debug, Default)]
struct Comp {
    m: u32,
    e: u32,
    s: u32,
}

impl Comp {
    fn width(self) -> u32 {
        self.m + self.e + self.s
    }
    fn value(self, raw: u64) -> f32 {
        let Comp { m, e, s } = self;
        if m + e + s == 0 {
            return 0.0;
        }
        let mmask = (1u64 << m) - 1;
        if e == 0 {
            // fixed point in [-1, 1]
            if m == 0 {
                return 0.0;
            }
            let mant = (raw & mmask) as i64;
            let neg = s == 1 && (raw >> m) & 1 == 1;
            let v = if neg { mant - (1i64 << m) } else { mant };
            return v as f32 / mmask as f32;
        }
        // small float without zero/denormals: [s][E][M]
        let sign = s == 1 && (raw >> (e + m)) & 1 == 1;
        let ex = ((raw >> m) & ((1u64 << e) - 1)) as i32;
        let mant = (raw & mmask) as f64;
        let v = (1.0 + mant / (1u64 << m) as f64) * 2f64.powi(ex + 1 - (1 << (e - 1)));
        (if sign { -v } else { v }) as f32
    }
}

/// A channel's packing: smallest-three index (rotations) and three components in stream order.
#[derive(Clone, Copy, Debug)]
enum Spec {
    Packed { fmt: usize, comps: [Comp; 3] },
    /// 48-bit smallest-three quaternion: [pad 1][3 x 15 bits][index 2]
    Q48,
}

impl Spec {
    fn from_word(w: u32) -> Spec {
        let g = |k: u32| {
            let g = (w >> (2 + 10 * k)) & 0x3ff;
            Comp { m: g & 31, e: (g >> 5) & 15, s: (g >> 9) & 1 }
        };
        // the bitstream reads the descriptor groups in reverse order
        Spec::Packed { fmt: (w & 3) as usize, comps: [g(2), g(1), g(0)] }
    }

    fn read_vec(&self, b: &mut Bits) -> [f32; 3] {
        match self {
            Spec::Packed { comps, .. } => {
                let mut v = [0.0; 3];
                for (i, c) in comps.iter().enumerate() {
                    v[i] = c.value(b.get(c.width()));
                }
                v
            }
            Spec::Q48 => {
                let q = self.read_quat(b);
                [q[0], q[1], q[2]]
            }
        }
    }

    fn read_quat(&self, b: &mut Bits) -> [f32; 4] {
        let (fmt, v) = match self {
            Spec::Packed { fmt, .. } => (*fmt, self.read_vec(b)),
            Spec::Q48 => {
                let raw = b.get(48);
                let mut v = [0.0f32; 3];
                for (k, x) in v.iter_mut().enumerate() {
                    let c = (raw >> (48 - 1 - 15 * (k as u32 + 1))) & 0x7fff;
                    *x = (c as f32 - 16384.0) / 16384.0 * std::f32::consts::FRAC_1_SQRT_2;
                }
                ((raw & 3) as usize, v)
            }
        };
        let ss = v[0] * v[0] + v[1] * v[1] + v[2] * v[2];
        let big = (1.0 - ss).clamp(0.0, 1.0).sqrt();
        let mut q = [0.0; 4];
        let mut k = 0;
        for (i, out) in q.iter_mut().enumerate() {
            if i == fmt {
                *out = big;
            } else {
                *out = v[k];
                k += 1;
            }
        }
        q
    }
}

impl EdgeAnim {
    pub fn decode(d: &[u8]) -> Result<EdgeAnim> {
        if u32_at(d, 0)? != TAG {
            bail!("not an Edge animation");
        }
        let f32_at = |o: usize| -> Result<f32> { Ok(f32::from_bits(u32_at(d, o)?)) };
        let duration = f32_at(4)?;
        let frequency = f32_at(8)?;
        let num_joints = u16_at(d, 0x0e)? as usize;
        let num_frames = u16_at(d, 0x10)? as usize;
        let num_sets = u16_at(d, 0x12)? as usize;
        let mut counts = [0usize; 8];
        for (i, c) in counts.iter_mut().enumerate() {
            *c = u16_at(d, 0x16 + 2 * i)? as usize;
        }
        let packed = u16_at(d, 0x26)? & 1 == 1;
        let off = |slot: usize| -> Result<Option<usize>> {
            let o = 0x38 + 4 * slot;
            let v = u32_at(d, o)? as usize;
            Ok(if v == 0 { None } else { Some(o + v) })
        };
        let (Some(dma), Some(info), Some(spec)) = (off(0)?, off(1)?, off(6)?) else {
            bail!("edge blob without frame sets");
        };
        let const_data = [off(2)?, off(3)?, off(4)?, off(5)?];

        // joint lists
        let mut lists: Vec<Vec<u16>> = Vec::with_capacity(8);
        let mut p = 0x60;
        for (i, &n) in counts.iter().enumerate() {
            let a = if i == 0 { 8 } else { 4 };
            let mut l = Vec::with_capacity(n);
            for k in 0..n {
                l.push(u16_at(d, p + 2 * k)?);
            }
            lists.push(l);
            p += n.div_ceil(a) * a * 2;
        }
        let [n_cr, n_ct, n_cs, _n_cu, n_r, n_t, n_s, _n_u] = counts;

        // packing specs: [const R][anim R..][const T][anim T..][const S][anim S..]
        let word = |i: usize| u32_at(d, spec + 4 * i);
        let rot_spec = |w: u32| if packed { Spec::from_word(w) } else { Spec::Q48 };
        let const_r_spec = rot_spec(word(0)?);
        let spec_r: Vec<Spec> = (0..n_r).map(|i| word(1 + i).map(rot_spec)).collect::<Result<_>>()?;
        let t0 = 1 + n_r;
        let const_t_spec = Spec::from_word(word(t0)?);
        let spec_t: Vec<Spec> = (0..n_t).map(|i| word(t0 + 1 + i).map(Spec::from_word)).collect::<Result<_>>()?;
        let s0 = t0 + 1 + n_t;
        let (const_s_spec, spec_s): (Spec, Vec<Spec>) = if n_cs + n_s > 0 {
            (Spec::from_word(word(s0)?), (0..n_s).map(|i| word(s0 + 1 + i).map(Spec::from_word)).collect::<Result<_>>()?)
        } else {
            (Spec::Q48, Vec::new())
        };

        let mut anim = EdgeAnim { num_joints, num_frames, duration, frequency, ..Default::default() };
        let end_of = |i: usize| const_data[i + 1..].iter().flatten().next().copied().unwrap_or(d.len());

        // constant channels: one key at frame 0
        if n_cr > 0 {
            let start = const_data[0].unwrap_or(0);
            let mut b = Bits::new(&d[start..end_of(0).max(start)]);
            for &j in &lists[0] {
                anim.rotations.push(Track { joint: j, frames: vec![0], values: vec![const_r_spec.read_quat(&mut b)] });
            }
        }
        if n_ct > 0 {
            let start = const_data[1].unwrap_or(0);
            let mut b = Bits::new(&d[start..end_of(1).max(start)]);
            for &j in &lists[1] {
                anim.translations.push(Track { joint: j, frames: vec![0], values: vec![const_t_spec.read_vec(&mut b)] });
            }
        }
        if n_cs > 0 {
            let start = const_data[2].unwrap_or(0);
            let mut b = Bits::new(&d[start..end_of(2).max(start)]);
            for &j in &lists[2] {
                anim.scales.push(Track { joint: j, frames: vec![0], values: vec![const_s_spec.read_vec(&mut b)] });
            }
        }

        // animated channels
        let first_r = anim.rotations.len();
        let first_t = anim.translations.len();
        let first_s = anim.scales.len();
        for &j in &lists[4] {
            anim.rotations.push(Track { joint: j, ..Default::default() });
        }
        for &j in &lists[5] {
            anim.translations.push(Track { joint: j, ..Default::default() });
        }
        for &j in &lists[6] {
            anim.scales.push(Track { joint: j, ..Default::default() });
        }
        for s in 0..num_sets {
            let body_off = u32_at(d, dma + 8 * s + 4)? as usize;
            let base = u16_at(d, info + 4 * s)?;
            let intra = u16_at(d, info + 4 * s + 2)? as usize;
            let Some(body) = d.get(body_off..) else { bail!("frame set {s} out of range") };
            let mut hdr = [0usize; 8];
            for (i, h) in hdr.iter_mut().enumerate() {
                *h = u16_at(body, 2 * i)? as usize;
            }
            let slice = |a: usize, n: usize| body.get(a..a + n).unwrap_or(&[]);
            let mut p = 16;
            let mut br = Bits::new(slice(p, hdr[0]));
            for (c, sp) in spec_r.iter().enumerate() {
                let t = &mut anim.rotations[first_r + c];
                t.frames.push(base);
                t.values.push(sp.read_quat(&mut br));
            }
            p += hdr[0];
            let mut bt = Bits::new(slice(p, hdr[1]));
            for (c, sp) in spec_t.iter().enumerate() {
                let t = &mut anim.translations[first_t + c];
                t.frames.push(base);
                t.values.push(sp.read_vec(&mut bt));
            }
            p += hdr[1];
            let mut bs = Bits::new(slice(p, hdr[2]));
            for (c, sp) in spec_s.iter().enumerate() {
                let t = &mut anim.scales[first_s + c];
                t.frames.push(base);
                t.values.push(sp.read_vec(&mut bs));
            }
            p += hdr[2] + hdr[3];
            if intra == 0 {
                continue;
            }
            let nch = n_r + n_t + n_s;
            let mask_bytes = (nch * intra).div_ceil(8);
            let mask = slice(p, mask_bytes);
            let has = |c: usize, k: usize| {
                let bit = c * intra + k;
                mask.get(bit >> 3).is_some_and(|b| (b >> (7 - (bit & 7))) & 1 == 1)
            };
            p += mask_bytes;
            let mut kr = Bits::new(slice(p, hdr[4]));
            let mut kt = Bits::new(slice(p + hdr[4], hdr[5]));
            let mut ks = Bits::new(slice(p + hdr[4] + hdr[5], hdr[6]));
            for (c, sp) in spec_r.iter().enumerate() {
                let t = &mut anim.rotations[first_r + c];
                for k in 0..intra {
                    if has(c, k) {
                        t.frames.push(base + 1 + k as u16);
                        t.values.push(sp.read_quat(&mut kr));
                    }
                }
            }
            for (c, sp) in spec_t.iter().enumerate() {
                let t = &mut anim.translations[first_t + c];
                for k in 0..intra {
                    if has(n_r + c, k) {
                        t.frames.push(base + 1 + k as u16);
                        t.values.push(sp.read_vec(&mut kt));
                    }
                }
            }
            for (c, sp) in spec_s.iter().enumerate() {
                let t = &mut anim.scales[first_s + c];
                for k in 0..intra {
                    if has(n_r + n_t + c, k) {
                        t.frames.push(base + 1 + k as u16);
                        t.values.push(sp.read_vec(&mut ks));
                    }
                }
            }
        }
        // padding entries point one past the last joint
        let nj = num_joints as u16;
        anim.rotations.retain(|t| t.joint < nj);
        anim.translations.retain(|t| t.joint < nj);
        anim.scales.retain(|t| t.joint < nj);
        Ok(anim)
    }
}
