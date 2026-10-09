//! The levels' navigation meshes (`DisPylon` + `NavigationMeshBase`): the original's walkable
//! polygons and how they join, for the characters' paths.
//!
//! A `NavigationMeshBase` (version 28) after its (empty) properties holds: its version twice;
//! the vertices (UE position + the polygons using it); the edge storage table (each edge's data
//! offset, size and class name: `FNavMeshEdgeBase` or `FNavMeshPathObjectEdge`); the polygons
//! (vertex and edge indices, centre, normal, bounds, height); its local-to-world and
//! world-to-local matrices; its bounds; and the edges' data in the table's order. A pylon's
//! obstacle mesh has the same layout and no edges.

use anyhow::{bail, Result};
use upk::Reader;

pub struct RawNavMesh {
    /// UE units, world space
    pub verts: Vec<[f32; 3]>,
    pub polys: Vec<Vec<u16>>,
    /// the polygons each edge joins (vertex pair, from, to; one way when the flag is set)
    pub links: Vec<(u16, u16, u16, u16, bool)>,
    /// edges onto another pylon's mesh: (vertex pair, the polygon on this side)
    pub cross: Vec<(u16, u16, u16)>,
}

const VERSION: i32 = 28;

/// Serialized edge records by class (each the in-memory stride less 23 bytes).
fn edge_size(class: &str) -> Option<usize> {
    Some(match class {
        "FNavMeshEdgeBase" | "FNavMeshBasicOneWayEdge" => 29,
        "FNavMeshCrossPylonEdge" => 81,
        "FNavMeshPathObjectEdge" => 105,
        _ => return None,
    })
}

pub fn read(pkg: &upk::Package, data: &[u8]) -> Result<RawNavMesh> {
    let mut r = Reader::at(data, 0);
    // net index, then the properties' terminating "None"
    r.i32()?;
    let none = r.i32()?;
    if pkg.names.get(none as usize).map(|s| s.as_str()) != Some("None") {
        bail!("navmesh with properties");
    }
    r.i32()?;
    let (v, v2) = (r.i32()?, r.i32()?);
    if v != VERSION || v2 != VERSION {
        bail!("navmesh version {v}/{v2}");
    }
    let n = count(&mut r, 1 << 20)?;
    let mut verts = Vec::with_capacity(n);
    for _ in 0..n {
        verts.push([r.f32()?, r.f32()?, r.f32()?]);
        let c = count(&mut r, 1 << 16)?;
        r.skip(2 * c)?;
    }
    // edge storage: (offset, size, class name, name number)
    let ne = count(&mut r, 1 << 20)?;
    let mut classes = Vec::with_capacity(ne);
    for _ in 0..ne {
        r.i32()?;
        r.u16()?;
        let name = r.i32()?;
        r.i32()?;
        classes.push(pkg.names.get(name as usize).cloned().unwrap_or_default());
    }
    let np = count(&mut r, 1 << 20)?;
    let mut polys = Vec::with_capacity(np);
    // which polygon each edge borders (for the cross-pylon edges, which don't say)
    let mut edge_poly = vec![u16::MAX; ne];
    for pi in 0..np {
        let c = count(&mut r, 256)?;
        let mut pv = Vec::with_capacity(c);
        for _ in 0..c {
            pv.push(r.u16()?);
        }
        let c = count(&mut r, 1 << 12)?;
        for _ in 0..c {
            let e = r.u16()? as usize;
            if e < ne && edge_poly[e] == u16::MAX {
                edge_poly[e] = pi as u16;
            }
        }
        // centre, normal, bounds (min, max, valid), height
        r.skip(12 * 4 + 1 + 4)?;
        polys.push(pv);
    }
    // (matrices, bounds and, in some meshes, more) then the edges' records, last
    let mut total = 0;
    for c in &classes {
        total += edge_size(c).ok_or_else(|| anyhow::anyhow!("navmesh edge class {c}"))?;
    }
    if total > data.len() - r.pos {
        bail!("navmesh edges overrun");
    }
    let mut r = Reader::at(data, data.len() - total);
    let mut links = Vec::with_capacity(ne);
    let mut cross = Vec::new();
    for (ei, class) in classes.iter().enumerate() {
        let (v0, v1) = (r.u16()?, r.u16()?);
        match class.as_str() {
            "FNavMeshEdgeBase" | "FNavMeshBasicOneWayEdge" => {
                let (p0, p1) = (r.u16()?, r.u16()?);
                // lengths, centre, flags
                r.skip(5 * 4 + 1)?;
                links.push((v0, v1, p0, p1, class == "FNavMeshBasicOneWayEdge"));
            }
            "FNavMeshPathObjectEdge" => {
                // through a path object (a door): the base edge, then the two polygons as
                // (pylon, guid, polygon id) references, then the path object's reference
                r.skip(29 + 20)?;
                let p0 = r.u16()?;
                r.skip(2 + 20)?;
                let p1 = r.u16()?;
                r.skip(2 + 24)?;
                links.push((v0, v1, p0, p1, false));
            }
            _ => {
                // onto another pylon: matched by its ends when the meshes are merged
                r.skip(81 - 4)?;
                if let Some(&p) = edge_poly.get(ei).filter(|p| **p != u16::MAX) {
                    cross.push((v0, v1, p));
                }
            }
        }
    }
    Ok(RawNavMesh { verts, polys, links, cross })
}

/// The walkable meshes of a map's levels (their pylons' obstacle meshes have no edges), as one.
pub fn merge(levels: &[std::sync::Arc<upk::Package>]) -> crate::format::NavMesh {
    let mut out = crate::format::NavMesh::default();
    // the edges onto other pylons: (polygon, its two vertices)
    let mut cross: Vec<(u32, u32, u32)> = Vec::new();
    for pkg in levels {
        for i in pkg.exports_of_class("NavigationMeshBase").collect::<Vec<_>>() {
            let Ok(data) = pkg.export_data(i) else { continue };
            let m = match read(pkg, data) {
                Ok(m) => m,
                Err(e) => {
                    log::warn!("{}: navmesh {}: {e:#}", pkg.name, pkg.obj_path(i));
                    continue;
                }
            };
            if m.links.is_empty() && m.cross.is_empty() {
                continue;
            }
            let (vo, po) = (out.verts.len() as u32, out.polys.len() as u32);
            // (a mesh riding a mover: its polygons, for the scripts to join and part)
            let owner = pkg.obj_outer(i);
            if pkg.class_name(owner) == "ArkDynamicPylon" {
                out.dynamic.push((pkg.obj_name(owner).to_string(), po, po + m.polys.len() as u32));
            }
            out.verts.extend(m.verts.iter().map(|v| crate::xform::ue_point(*v)));
            out.polys.extend(m.polys.iter().map(|p| crate::format::NavPoly { verts: p.iter().map(|v| vo + *v as u32).collect(), links: Vec::new() }));
            let np = m.polys.len() as u16;
            for &(v0, v1, p0, p1, one_way) in &m.links {
                if p0 >= np || p1 >= np || p0 == p1 || v0 as usize >= m.verts.len() || v1 as usize >= m.verts.len() {
                    continue;
                }
                let (a, b) = (vo + v0 as u32, vo + v1 as u32);
                out.polys[(po + p0 as u32) as usize].links.push((po + p1 as u32, a, b));
                if !one_way {
                    out.polys[(po + p1 as u32) as usize].links.push((po + p0 as u32, a, b));
                }
            }
            for &(v0, v1, p) in &m.cross {
                if p < np && (v0 as usize) < m.verts.len() && (v1 as usize) < m.verts.len() {
                    cross.push((out.polys.len() as u32 - np as u32 + p as u32, vo + v0 as u32, vo + v1 as u32));
                }
            }
        }
    }
    // join the pylons where their edges meet
    let at = |i: u32| glam_vec(out.verts[i as usize]);
    let mut joined = 0;
    for i in 0..cross.len() {
        for j in i + 1..cross.len() {
            let ((pa, a0, a1), (pb, b0, b1)) = (cross[i], cross[j]);
            let same = |x: u32, y: u32| (at(x) - at(y)).abs().max_element() < 0.05;
            if pa != pb && (same(a0, b0) && same(a1, b1) || same(a0, b1) && same(a1, b0)) {
                out.polys[pa as usize].links.push((pb, a0, a1));
                out.polys[pb as usize].links.push((pa, a0, a1));
                joined += 1;
            }
        }
    }
    if !cross.is_empty() {
        log::info!("navmesh: {} cross-pylon edges, {joined} joined", cross.len());
    }
    out
}

fn glam_vec(v: [f32; 3]) -> glam::Vec3 {
    glam::Vec3::from(v)
}

fn count(r: &mut Reader, max: usize) -> Result<usize> {
    let n = r.i32()?;
    if n < 0 || n as usize > max {
        bail!("navmesh count {n}");
    }
    Ok(n as usize)
}
