//! FaceFX (OC3, archive version 1731 as Unreal Engine 3 stores it in `FaceFXAsset` /
//! `FaceFXAnimSet` native data): the lines' facial animations (curves by target: `open`, `W`,
//! `PBM`, `Blink`, head orientation...) and the characters' compiled face graphs (nodes adding
//! up their inputs through link functions) with the face bones' deltas per bone-pose node.
//!
//! An archive: `FACE`, version, licensee / project strings, licensee version, an endian
//! marker, the class table, the name table; then the object. Quaternions are (w, x, y, z), the
//! conjugate of Unreal's.

use crate::format::{FxActor, FxAnim, FxBone, FxBoneLink, FxNode};
use crate::xform::ue_point;
use anyhow::{bail, Result};

struct Rd<'a> {
    d: &'a [u8],
    p: usize,
}

impl Rd<'_> {
    fn bytes(&mut self, n: usize) -> Result<&[u8]> {
        match self.d.get(self.p..self.p + n) {
            Some(b) => {
                self.p += n;
                Ok(b)
            }
            None => bail!("facefx: truncated at {}", self.p),
        }
    }
    fn u16(&mut self) -> Result<u16> {
        Ok(u16::from_le_bytes(self.bytes(2)?.try_into()?))
    }
    fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_le_bytes(self.bytes(4)?.try_into()?))
    }
    fn f32(&mut self) -> Result<f32> {
        Ok(f32::from_le_bytes(self.bytes(4)?.try_into()?))
    }
    fn str(&mut self) -> Result<String> {
        let n = self.u32()? as usize;
        if n > 1 << 16 {
            bail!("facefx: bad string length {n}");
        }
        Ok(String::from_utf8_lossy(self.bytes(n)?).trim_end_matches('\0').to_string())
    }
    fn count(&mut self, max: usize) -> Result<usize> {
        let n = self.u32()? as usize;
        if n > max {
            bail!("facefx: bad count {n} at {}", self.p);
        }
        Ok(n)
    }
}

/// The archive header and name table; the reader is left at the object.
fn open(d: &[u8]) -> Result<(Rd<'_>, Vec<String>)> {
    // (UE3 prefixes the archive with its own count and size)
    let start = d.windows(4).take(64).position(|w| w == b"FACE").ok_or_else(|| anyhow::anyhow!("facefx: no archive"))?;
    let mut r = Rd { d, p: start + 4 };
    let version = r.u32()?;
    if version != 1731 {
        bail!("facefx: archive version {version}");
    }
    r.u32()?;
    r.str()?;
    r.str()?;
    r.u32()?;
    r.u16()?;
    let classes = r.count(256)?;
    for _ in 0..classes {
        r.u32()?;
        r.u32()?;
        r.str()?;
        r.u16()?;
    }
    let n = r.count(1 << 16)?;
    let mut names = Vec::with_capacity(n);
    for _ in 0..n {
        names.push(r.str()?);
    }
    Ok((r, names))
}

fn name(names: &[String], i: u32) -> Result<String> {
    names.get(i as usize).cloned().ok_or_else(|| anyhow::anyhow!("facefx: name {i} of {}", names.len()))
}

/// An animation set's animations: (name, animation) — a line's name is its voice event's id.
pub fn read_anim_set(d: &[u8]) -> Result<Vec<(String, FxAnim)>> {
    let (mut r, names) = open(d)?;
    // set name, ?, actor name, group name, then the group's animations
    r.u32()?;
    r.u32()?;
    r.u32()?;
    r.u32()?;
    let n = r.count(1 << 14)?;
    let mut out = Vec::with_capacity(n);
    for _ in 0..n {
        let anim_name = name(&names, r.u32()?)?;
        let nc = r.count(4096)?;
        let mut curves = Vec::with_capacity(nc);
        for _ in 0..nc {
            let c = name(&names, r.u32()?)?;
            r.u32()?; // interpolation (Hermite)
            curves.push(c);
        }
        let nk = r.count(1 << 20)?;
        let mut keys = Vec::with_capacity(nk);
        for _ in 0..nk {
            keys.push([r.f32()?, r.f32()?, r.f32()?, r.f32()?]);
        }
        let ncnt = r.count(4096)?;
        let mut counts = Vec::with_capacity(ncnt);
        for _ in 0..ncnt {
            counts.push(r.u32()? as usize);
        }
        let blend = (r.f32()?, r.f32()?);
        r.u32()?;
        r.u32()?;
        r.str()?; // the source audio
        r.u32()?;
        let mut anim = FxAnim { blend_in: blend.0, blend_out: blend.1, curves: Vec::new() };
        let mut k = 0;
        for (c, n) in curves.into_iter().zip(counts) {
            let end = (k + n).min(keys.len());
            if end > k {
                anim.curves.push((c, keys[k..end].to_vec()));
            }
            k = end;
        }
        out.push((anim_name, anim));
    }
    Ok(out)
}

/// A FaceFX quaternion (w, x, y, z; the conjugate of Unreal's) in Bevy's frame.
fn fx_quat(w: f32, x: f32, y: f32, z: f32) -> [f32; 4] {
    // Unreal (x, y, z, -w), then Unreal to Bevy (-x, -z, -y, w)
    [-x, -z, -y, -w]
}

/// A character's face: its compiled face graph and its face bones' deltas.
pub fn read_actor(d: &[u8]) -> Result<FxActor> {
    let (mut r, names) = open(d)?;
    let actor_name = names.first().cloned().unwrap_or_default();
    r.u32()?;
    r.u32()?;
    let nb = r.count(4096)?;
    let mut bones = Vec::with_capacity(nb);
    for _ in 0..nb {
        let bone = name(&names, r.u32()?)?;
        // reference transform (position, rotation, scale), its inverse rotation, the cached
        // skeleton index, a weight
        for _ in 0..3 + 4 + 3 + 4 {
            r.f32()?;
        }
        r.u32()?;
        r.f32()?;
        let nl = r.count(4096)?;
        let mut links = Vec::with_capacity(nl);
        for _ in 0..nl {
            let node = r.u32()?;
            r.u32()?;
            let p = [r.f32()?, r.f32()?, r.f32()?];
            let (w, x, y, z) = (r.f32()?, r.f32()?, r.f32()?, r.f32()?);
            let s = [r.f32()?, r.f32()?, r.f32()?];
            links.push(FxBoneLink { node, pos: ue_point(p), rot: fx_quat(w, x, y, z), scale: [s[0], s[2], s[1]] });
        }
        bones.push(FxBone { name: bone, links });
    }
    let nn = r.count(1 << 14)?;
    let mut nodes = Vec::with_capacity(nn);
    for _ in 0..nn {
        let kind = r.u32()?;
        let node = name(&names, r.u32()?)?;
        // (min, 1 / min, max, 1 / max)
        let (min, _, max, _) = (r.f32()?, r.f32()?, r.f32()?, r.f32()?);
        let op = r.u32()?;
        let nl = r.count(4096)?;
        let mut inputs = Vec::with_capacity(nl);
        for _ in 0..nl {
            let src = r.u32()?;
            let f = r.u32()?;
            let np = r.count(16)?;
            let mut params = Vec::with_capacity(np);
            for _ in 0..np {
                params.push(r.f32()?);
            }
            inputs.push((src, f, params));
        }
        r.u32()?;
        nodes.push(FxNode { name: node, kind, min, max, op, inputs });
    }
    Ok(FxActor { name: actor_name, nodes, bones })
}
