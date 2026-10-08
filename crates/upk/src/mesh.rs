//! UStaticMesh decoding (UE3 v801, Dishonored).

use crate::package::Package;
use crate::props::{read_object, Props};
use crate::reader::Reader;
use anyhow::{bail, Context, Result};

#[derive(Debug, Clone)]
pub struct MeshSection {
    /// Material object reference (package-local index).
    pub material: i32,
    pub first_index: u32,
    pub num_faces: u32,
    pub material_index: i32,
}

#[derive(Debug, Clone, Default)]
pub struct StaticMeshData {
    pub positions: Vec<[f32; 3]>,
    pub normals: Vec<[f32; 3]>,
    pub tangents: Vec<[f32; 4]>,
    /// One Vec per UV channel.
    pub uvs: Vec<Vec<[f32; 2]>>,
    pub colors: Option<Vec<[u8; 4]>>,
    pub indices: Vec<u32>,
    pub sections: Vec<MeshSection>,
    pub bounds_origin: [f32; 3],
    pub bounds_extent: [f32; 3],
    pub bounds_radius: f32,
    pub lightmap_coord_index: i32,
    pub props: Props,
}

pub fn unpack_normal(v: u32) -> [f32; 4] {
    let b = v.to_le_bytes();
    let f = |x: u8| x as f32 / 127.5 - 1.0;
    [f(b[0]), f(b[1]), f(b[2]), f(b[3])]
}

pub fn half_to_f32(h: u16) -> f32 {
    let sign = ((h >> 15) & 1) as u32;
    let exp = ((h >> 10) & 0x1f) as u32;
    let mant = (h & 0x3ff) as u32;
    let bits = if exp == 0 {
        if mant == 0 {
            sign << 31
        } else {
            // subnormal
            let mut e = 127 - 15 + 1;
            let mut m = mant;
            while m & 0x400 == 0 {
                m <<= 1;
                e -= 1;
            }
            (sign << 31) | ((e as u32) << 23) | ((m & 0x3ff) << 13)
        }
    } else if exp == 31 {
        (sign << 31) | 0x7f80_0000 | (mant << 13)
    } else {
        (sign << 31) | ((exp + 127 - 15) << 23) | (mant << 13)
    };
    f32::from_bits(bits)
}

/// Skip an FUntypedBulkData header + inline payload.
pub fn skip_bulk(r: &mut Reader) -> Result<()> {
    let flags = r.u32()?;
    let _count = r.i32()?;
    let size_on_disk = r.i32()?;
    let _offset = r.i32()?;
    if flags & 1 == 0 && size_on_disk > 0 {
        r.skip(size_on_disk as usize)?;
    }
    Ok(())
}

pub fn read_static_mesh(pkg: &Package, idx: i32) -> Result<StaticMeshData> {
    let od = read_object(pkg, idx).with_context(|| format!("static mesh props {}", pkg.obj_path(idx)))?;
    let mut r = od.reader;
    let props = od.props;
    let mut out = StaticMeshData {
        lightmap_coord_index: props.int("LightMapCoordinateIndex").unwrap_or(1),
        ..Default::default()
    };
    out.bounds_origin = r.vec3()?;
    out.bounds_extent = r.vec3()?;
    out.bounds_radius = r.f32()?;
    let _body_setup = r.i32()?;
    let _kdop_nodes = r.bulk_array()?;
    let _kdop_tris = r.bulk_array()?;
    let _internal_version = r.i32()?;
    let nlods = r.count(16)?;
    if nlods == 0 {
        bail!("static mesh has no LODs");
    }
    // LOD 0 only.
    skip_bulk(&mut r)?;
    let nsec = r.count(32)?;
    for _ in 0..nsec {
        let material = r.i32()?;
        let _f10 = r.i32()?;
        let _f14 = r.i32()?;
        let _shadow = r.i32()?;
        let first_index = r.u32()?;
        let num_faces = r.u32()?;
        let _min_v = r.i32()?;
        let _max_v = r.i32()?;
        let material_index = r.i32()?;
        let nfrag = r.count(8)?;
        r.skip(nfrag * 8)?;
        let ps3 = r.u8()?;
        if ps3 != 0 {
            // FPS3StaticMeshData: 2x TArray<int>, 6x TArray<uint16>
            for i in 0..8 {
                let n = r.count(0)?;
                r.skip(n * if i < 2 { 4 } else { 2 })?;
            }
        }
        out.sections.push(MeshSection { material, first_index, num_faces, material_index });
    }

    // Position stream
    let _vsize = r.i32()?;
    let nverts = r.i32()? as usize;
    let (esz, n, pos) = r.bulk_array()?;
    if esz != 12 || n != nverts {
        bail!("unexpected position stream {esz}x{n} (nverts {nverts})");
    }
    let mut pr = Reader::new(pos);
    out.positions.reserve(n);
    for _ in 0..n {
        out.positions.push(pr.vec3()?);
    }

    // UV / tangent stream
    let num_tc = r.i32()? as usize;
    let _item = r.i32()?;
    let _nv = r.i32()?;
    let full_uv = r.i32()? != 0;
    let (esz, n, uvd) = r.bulk_array()?;
    let expected = 8 + num_tc * if full_uv { 8 } else { 4 };
    if esz != expected || n != nverts {
        bail!("unexpected uv stream {esz}x{n} (tc {num_tc} full {full_uv}, expected {expected})");
    }
    let mut ur = Reader::new(uvd);
    out.uvs = vec![Vec::with_capacity(n); num_tc];
    out.normals.reserve(n);
    out.tangents.reserve(n);
    for _ in 0..n {
        let tx = unpack_normal(ur.u32()?);
        let tz = unpack_normal(ur.u32()?);
        out.normals.push([tz[0], tz[1], tz[2]]);
        let sign = if tz[3] < 0.0 { -1.0 } else { 1.0 };
        out.tangents.push([tx[0], tx[1], tx[2], sign]);
        for c in 0..num_tc {
            let uv = if full_uv {
                [ur.f32()?, ur.f32()?]
            } else {
                [half_to_f32(ur.u16()?), half_to_f32(ur.u16()?)]
            };
            out.uvs[c].push(uv);
        }
    }

    // Color stream
    let _csize = r.i32()?;
    let cverts = r.i32()?;
    if cverts > 0 {
        let (esz, n, cd) = r.bulk_array()?;
        if esz == 4 && n == nverts {
            out.colors = Some(cd.chunks(4).map(|c| [c[2], c[1], c[0], c[3]]).collect());
        }
    }
    let _num_verts = r.i32()?;
    let (esz, n, idx_data) = r.bulk_array()?;
    if esz != 2 {
        bail!("unexpected index size {esz}");
    }
    out.indices = idx_data.chunks(2).map(|c| u16::from_le_bytes([c[0], c[1]]) as u32).collect();
    let _ = n;
    out.props = props;
    Ok(out)
}
