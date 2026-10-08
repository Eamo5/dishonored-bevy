//! USkeletalMesh decoding (UE3 v801 / Dishonored). Produces reference-pose geometry,
//! the bone hierarchy and per-vertex skin weights (LOD 0 only).

use crate::format::{BoneDef, MeshFile};
use crate::xform::{rot_matrix, ue_dir, ue_point};
use glam::{Mat3, Quat, Vec3};
use anyhow::{bail, Result};
use upk::mesh::{half_to_f32, skip_bulk, unpack_normal};
use upk::{Package, Reader};

#[derive(Debug, Clone)]
pub struct Bone {
    pub name: String,
    pub parent: i32,
    /// Local rotation (x, y, z, w) and translation in UE space.
    pub rotation: [f32; 4],
    pub position: [f32; 3],
}

#[derive(Debug, Clone)]
pub struct SkelSection {
    pub material: i32,
    pub first_index: u32,
    pub num_tris: u32,
}

#[derive(Debug, Clone, Default)]
pub struct SkeletalMeshData {
    pub materials: Vec<i32>,
    pub bones: Vec<Bone>,
    pub positions: Vec<[f32; 3]>,
    pub normals: Vec<[f32; 3]>,
    pub tangents: Vec<[f32; 4]>,
    pub uvs: Vec<[f32; 2]>,
    pub joints: Vec<[u16; 4]>,
    pub weights: Vec<[f32; 4]>,
    pub indices: Vec<u32>,
    pub sections: Vec<SkelSection>,
    pub origin: [f32; 3],
    pub rot_origin: [i32; 3],
    /// Edge skeleton blob (joint order used by the mesh's Edge animations)
    pub edge_skeleton: Vec<u8>,
    /// the lesser LODs (LOD 1 on: their geometry, the same bones and materials)
    pub lods: Vec<SkeletalMeshData>,
    /// each LOD's `LODInfo.DisplayFactor` (from LOD 0)
    pub lod_factors: Vec<f32>,
    /// the bounds' sphere radius (unreal units)
    pub radius: f32,
    /// each LOD's `LODInfo.LODMaterialMap` (a section's material index -> the mesh's)
    pub lod_material_maps: Vec<Vec<i32>>,
    /// `m_MaterialsToBodyParts`: the dismemberment LOD's sections' (owner bone, cut bone,
    /// shown once cut)
    pub body_parts: Vec<(String, String, bool)>,
}

pub fn read_skeletal_mesh(pkg: &Package, idx: i32) -> Result<SkeletalMeshData> {
    let od = upk::read_object(pkg, idx)?;
    let mesh_path = pkg.obj_path(idx);
    let props = od.props;
    let has_colors = props.bool("bHasVertexColors").unwrap_or(false);
    let mut r: Reader = od.reader;
    let mut out = SkeletalMeshData::default();

    // Dishonored FUserBounds
    let _bone = pkg.read_name(&mut r)?;
    let _offset = r.vec3()?;
    let _radius = r.f32()?;
    // FBoxSphereBounds: origin, extent, sphere radius
    r.skip(24)?;
    out.radius = r.f32()?;
    let nmat = r.count(4)?;
    for _ in 0..nmat {
        out.materials.push(r.i32()?);
    }
    out.origin = r.vec3()?;
    out.rot_origin = [r.i32()?, r.i32()?, r.i32()?];
    // EdgeSkeleton (TArray<byte>)
    let n = r.count(1)?;
    out.edge_skeleton = r.bytes(n)?.to_vec();
    let nbones = r.count(40)?;
    for _ in 0..nbones {
        let name = pkg.read_name(&mut r)?;
        let _flags = r.u32()?;
        let rotation = r.vec4()?;
        let position = r.vec3()?;
        let _children = r.i32()?;
        let parent = r.i32()?;
        let _color = r.u32()?;
        out.bones.push(Bone { name, parent, rotation, position });
    }
    let _depth = r.i32()?;
    let nlods = r.count(16)?;
    if nlods == 0 {
        bail!("no LODs");
    }
    // ---- LOD 0, then the lesser ones (each as LOD 0 is laid out, after the vertex influences
    // of the one before; one that can't be read ends them)
    read_lod(&mut r, &mut out)?;
    if std::env::var("DH_SKEL_PROBE").is_ok() {
        let rest = vr_rest(&r);
        eprintln!("probe {}: lods {nlods} colors {has_colors} at {} rest {} next {:02x?}", mesh_path, r.pos, rest.len(), &rest[..rest.len().min(160)]);
    }
    if !has_colors {
        for _ in 1..nlods {
            let influences = r.i32().unwrap_or(-1);
            if influences != 0 {
                break;
            }
            let mut lod = SkeletalMeshData { materials: out.materials.clone(), bones: out.bones.clone(), origin: out.origin, rot_origin: out.rot_origin, ..Default::default() };
            let at = r.pos;
            match read_lod(&mut r, &mut lod) {
                Ok(()) if !lod.positions.is_empty() && !lod.indices.is_empty() => out.lods.push(lod),
                _ => {
                    log::debug!("{mesh_path}: LOD {} unreadable at {at}", out.lods.len() + 1);
                    break;
                }
            }
        }
    }
    // their display factors and material maps
    if let Some((c, o, sz)) = props.array("LODInfo") {
        let infos = upk::props::parse_struct_array(pkg, o, sz, c).unwrap_or_default();
        out.lod_factors = infos.iter().map(|l| l.float("DisplayFactor").unwrap_or(0.0)).collect();
        out.lod_material_maps = infos
            .iter()
            .map(|l| match l.array("LODMaterialMap") {
                Some((n, off, size)) if size == n * 4 => pkg.data[off..off + size].chunks(4).map(|b| i32::from_le_bytes([b[0], b[1], b[2], b[3]])).collect(),
                _ => Vec::new(),
            })
            .collect();
    }
    // the body parts of the dismemberment LOD
    if let Some((c, o, sz)) = props.array("m_MaterialsToBodyParts") {
        out.body_parts = upk::props::parse_struct_array(pkg, o, sz, c)
            .unwrap_or_default()
            .iter()
            .map(|p| (p.name("m_OwnerBone").unwrap_or("None").to_string(), p.name("m_CutBone").unwrap_or("None").to_string(), p.bool("m_bShowIfCut").unwrap_or(false)))
            .collect();
    }
    Ok(out)
}

/// One LOD (`FStaticLODModel`): its sections, indices, chunks and GPU-skin vertices.
fn read_lod(r: &mut Reader, out: &mut SkeletalMeshData) -> Result<()> {
    let nsec = r.count(11)?;
    for _ in 0..nsec {
        let material = r.i16()? as i32;
        let _chunk = r.i16()?;
        let first_index = r.u32()?;
        let num_tris = r.u16()? as u32;
        let _unk = r.u8()?;
        out.sections.push(SkelSection { material, first_index, num_tris });
    }
    let (esz, _n, idx) = r.bulk_array()?;
    if esz != 2 {
        bail!("unexpected skel index size {esz}");
    }
    let raw_indices: Vec<u32> = idx.chunks(2).map(|c| u16::from_le_bytes([c[0], c[1]]) as u32).collect();
    let n = r.count(2)?; // UsedBones
    r.skip(n * 2)?;
    let nchunks = r.count(20)?;
    let mut chunks: Vec<(u32, Vec<u16>, u32)> = Vec::new();
    for _ in 0..nchunks {
        let first = r.i32()? as u32;
        let nrigid = r.count(61)?;
        r.skip(nrigid * 61)?;
        let nsoft = r.count(68)?;
        r.skip(nsoft * 68)?;
        let nb = r.count(2)?;
        let mut bones = Vec::with_capacity(nb);
        for _ in 0..nb {
            bones.push(r.u16()?);
        }
        let num_rigid = r.i32()? as u32;
        let num_soft = r.i32()? as u32;
        let _max_inf = r.i32()?;
        chunks.push((first, bones, num_rigid + num_soft));
    }
    let _size = r.i32()?;
    let num_verts = r.i32()? as usize;
    let n = r.count(1)?; // RequiredBones
    r.skip(n)?;
    skip_bulk(r)?;
    let _lod_uv_sets = r.i32()?;
    // GPU skin vertex buffer
    let num_uv = r.i32()?.max(1) as usize;
    let full_uv = r.i32()? != 0;
    let _packed = r.i32()?;
    let _ext = r.vec3()?;
    let _org = r.vec3()?;
    let (esz, nv, vd) = r.bulk_array()?;
    let expect = 28 + num_uv * if full_uv { 8 } else { 4 };
    if esz != expect {
        bail!("unexpected gpu vertex size {esz} (expected {expect}, uv {num_uv} full {full_uv})");
    }
    if nv != num_verts {
        log::debug!("vertex count mismatch {nv} vs {num_verts}");
    }
    let mut vr = Reader::new(vd);
    let mut chunk_i = 0usize;
    for v in 0..nv {
        while chunk_i + 1 < chunks.len() && v as u32 >= chunks[chunk_i].0 + chunks[chunk_i].2 {
            chunk_i += 1;
        }
        let tx = unpack_normal(vr.u32()?);
        let tz = unpack_normal(vr.u32()?);
        let bi = vr.bytes(4)?;
        let bw = vr.bytes(4)?;
        let pos = vr.vec3()?;
        let mut uv = [0.0f32; 2];
        for k in 0..num_uv {
            let u = if full_uv {
                [vr.f32()?, vr.f32()?]
            } else {
                [half_to_f32(vr.u16()?), half_to_f32(vr.u16()?)]
            };
            if k == 0 {
                uv = u;
            }
        }
        out.positions.push(pos);
        out.normals.push([tz[0], tz[1], tz[2]]);
        out.tangents.push([tx[0], tx[1], tx[2], if tz[3] < 0.0 { -1.0 } else { 1.0 }]);
        out.uvs.push(uv);
        let map = chunks.get(chunk_i).map(|c| &c.1);
        let mut j = [0u16; 4];
        let mut w = [0f32; 4];
        for k in 0..4 {
            j[k] = map.and_then(|m| m.get(bi[k] as usize).copied()).unwrap_or(0);
            w[k] = bw[k] as f32 / 255.0;
        }
        let s: f32 = w.iter().sum();
        if s > 0.0 {
            for x in w.iter_mut() {
                *x /= s;
            }
        } else {
            w[0] = 1.0;
        }
        out.joints.push(j);
        out.weights.push(w);
    }
    out.indices = raw_indices;
    Ok(())
}

/// (probe) what is left to read.
fn vr_rest<'a>(r: &Reader<'a>) -> &'a [u8] {
    &r.data[r.pos..]
}

impl SkeletalMeshData {
    /// UE3 applies `RotOrigin` and `Origin` between the mesh and its component:
    /// component = R(RotOrigin) * (v + Origin).
    pub fn component_xform(&self) -> (Quat, Vec3) {
        let m = rot_matrix(self.rot_origin);
        let q = Quat::from_mat3(&Mat3::from_mat4(m)).normalize();
        (q, q * Vec3::from(self.origin))
    }
}

fn ue_quat_to_bevy(q: Quat) -> [f32; 4] {
    [-q.x, -q.z, -q.y, q.w]
}

/// Bind-pose skeleton in Bevy space (root includes the mesh's component transform).
pub fn to_skeleton(d: &SkeletalMeshData) -> Vec<BoneDef> {
    let (cq, ct) = d.component_xform();
    d.bones
        .iter()
        .enumerate()
        .map(|(i, b)| {
            let mut q = Quat::from_xyzw(b.rotation[0], b.rotation[1], b.rotation[2], b.rotation[3]).normalize();
            let mut t = Vec3::from(b.position);
            let parent = if i == 0 { -1 } else { b.parent };
            if i == 0 {
                t = cq * t + ct;
                q = cq * q;
            }
            BoneDef { name: b.name.clone(), parent, translation: ue_point(t.to_array()), rotation: ue_quat_to_bevy(q) }
        })
        .collect()
}

/// Geometry as a cache mesh (Bevy space, component space incl. RotOrigin/Origin).
/// `joint_map` remaps this mesh's bone indices (e.g. a head onto the body skeleton).
pub fn to_mesh_file_skinned(d: &SkeletalMeshData, joint_map: Option<&[u16]>) -> MeshFile {
    let (cq, ct) = d.component_xform();
    let xp = |p: [f32; 3]| ue_point((cq * Vec3::from(p) + ct).to_array());
    let xd = |p: [f32; 3]| ue_dir((cq * Vec3::from(p)).to_array());
    let mut mf = MeshFile {
        positions: d.positions.iter().map(|p| xp(*p)).collect(),
        normals: d.normals.iter().map(|v| xd(*v)).collect(),
        tangents: d
            .tangents
            .iter()
            .map(|t| {
                let v = xd([t[0], t[1], t[2]]);
                [v[0], v[1], v[2], -t[3]]
            })
            .collect(),
        uv0: d.uvs.clone(),
        joints: d
            .joints
            .iter()
            .map(|j| {
                let mut o = *j;
                if let Some(m) = joint_map {
                    for x in o.iter_mut() {
                        *x = m.get(*x as usize).copied().unwrap_or(0);
                    }
                }
                o
            })
            .collect(),
        weights: d.weights.clone(),
        ..Default::default()
    };
    for s in &d.sections {
        let first = s.first_index as usize;
        let count = s.num_tris as usize * 3;
        let start = mf.indices.len() as u32;
        if first + count > d.indices.len() {
            mf.sections.push((start, 0));
            continue;
        }
        for tri in d.indices[first..first + count].chunks(3) {
            mf.indices.extend_from_slice(&[tri[0], tri[1], tri[2]]);
        }
        mf.sections.push((start, count as u32));
    }
    mf
}

/// Reference-pose world matrices (Bevy space) of a skeleton's bones.
pub fn ref_world(bones: &[BoneDef]) -> Vec<glam::Mat4> {
    let mut world: Vec<glam::Mat4> = Vec::with_capacity(bones.len());
    for b in bones {
        let local = glam::Mat4::from_rotation_translation(Quat::from_array(b.rotation).normalize(), Vec3::from(b.translation));
        let w = if b.parent >= 0 && (b.parent as usize) < world.len() { world[b.parent as usize] * local } else { local };
        world.push(w);
    }
    world
}

/// A mesh worn on another's skeleton (a head on its body): its bones are the body's of the
/// same name (else their nearest such ancestor's), and its vertices move from its own
/// reference pose into the body's, as UE3 skins it (its own reference pose, the body's
/// bones: `ParentAnimComponent`).
pub fn to_mesh_file_worn(d: &SkeletalMeshData, body: &SkeletalMeshData) -> MeshFile {
    let (own, theirs) = (to_skeleton(d), to_skeleton(body));
    let (own_w, their_w) = (ref_world(&own), ref_world(&theirs));
    let find = |n: &str| theirs.iter().position(|b| b.name.eq_ignore_ascii_case(n));
    // each of its bones: the body's bone it follows, and the move from its pose into the body's
    let mut map: Vec<u16> = Vec::with_capacity(own.len());
    let mut fix: Vec<glam::Mat4> = Vec::with_capacity(own.len());
    for (i, b) in own.iter().enumerate() {
        let mut k = i as i32;
        let mut hit = None;
        while k >= 0 {
            if let Some(j) = find(&own[k as usize].name) {
                hit = Some((k as usize, j));
                break;
            }
            k = own[k as usize].parent;
        }
        let _ = b;
        match hit {
            Some((k, j)) => {
                map.push(j as u16);
                fix.push(their_w[j] * own_w[k].inverse());
            }
            None => {
                map.push(0);
                fix.push(glam::Mat4::IDENTITY);
            }
        }
    }
    let mut mf = to_mesh_file_skinned(d, Some(&map));
    for (v, (j, w)) in d.joints.iter().zip(d.weights.iter()).enumerate() {
        let mut m = glam::Mat4::ZERO;
        let mut total = 0.0;
        for c in 0..4 {
            if w[c] > 0.0 {
                m += fix.get(j[c] as usize).copied().unwrap_or(glam::Mat4::IDENTITY) * w[c];
                total += w[c];
            }
        }
        if total <= 0.0 {
            continue;
        }
        let m = m * (1.0 / total);
        if let Some(p) = mf.positions.get_mut(v) {
            *p = m.transform_point3(Vec3::from(*p)).to_array();
        }
        if let Some(n) = mf.normals.get_mut(v) {
            *n = m.transform_vector3(Vec3::from(*n)).normalize_or_zero().to_array();
        }
        if let Some(t) = mf.tangents.get_mut(v) {
            let r = m.transform_vector3(Vec3::new(t[0], t[1], t[2])).normalize_or_zero();
            *t = [r.x, r.y, r.z, t[3]];
        }
    }
    mf
}

/// Geometry posed by bone world matrices (Bevy space) instead of the reference pose, as UE3
/// skins it (a component holding a frame of an animation: the Tower's lowered gangway).
pub fn to_mesh_file_posed(d: &SkeletalMeshData, posed: &[glam::Mat4]) -> MeshFile {
    let rest = ref_world(&to_skeleton(d));
    let fix: Vec<glam::Mat4> = rest.iter().enumerate().map(|(j, r)| posed.get(j).copied().unwrap_or(*r) * r.inverse()).collect();
    let mut mf = to_mesh_file(d);
    for (v, (j, w)) in d.joints.iter().zip(d.weights.iter()).enumerate() {
        let mut m = glam::Mat4::ZERO;
        let mut total = 0.0;
        for c in 0..4 {
            if w[c] > 0.0 {
                m += fix.get(j[c] as usize).copied().unwrap_or(glam::Mat4::IDENTITY) * w[c];
                total += w[c];
            }
        }
        if total <= 0.0 {
            continue;
        }
        let m = m * (1.0 / total);
        if let Some(p) = mf.positions.get_mut(v) {
            *p = m.transform_point3(Vec3::from(*p)).to_array();
        }
        if let Some(n) = mf.normals.get_mut(v) {
            *n = m.transform_vector3(Vec3::from(*n)).normalize_or_zero().to_array();
        }
        if let Some(t) = mf.tangents.get_mut(v) {
            let r = m.transform_vector3(Vec3::new(t[0], t[1], t[2])).normalize_or_zero();
            *t = [r.x, r.y, r.z, t[3]];
        }
    }
    mf
}

/// Reference-pose geometry (no skin data) for static use such as doors.
pub fn to_mesh_file(d: &SkeletalMeshData) -> MeshFile {
    let mut mf = to_mesh_file_skinned(d, None);
    mf.joints.clear();
    mf.weights.clear();
    mf
}
