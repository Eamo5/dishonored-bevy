//! Material / MaterialInstanceConstant resolution to a flat texture/parameter set.

use crate::format::Blend;
use crate::resolver::{Assets, Obj};
use upk::props::{object_array, parse_struct_array};
use upk::Value;

#[derive(Clone, Default)]
pub struct ResolvedMaterial {
    /// Effective (leaf-most) value per texture parameter.
    pub textures: Vec<(String, Obj)>,
    /// Every texture parameter value seen, with its chain level (0 = leaf).
    pub tex_levels: Vec<(String, Obj, usize)>,
    /// Textures referenced by the compiled shader of the nearest compiled resource, and its level.
    pub used: Option<(std::collections::HashSet<String>, usize)>,
    pub unnamed: Vec<Obj>,
    pub vectors: Vec<(String, [f32; 4])>,
    pub scalars: Vec<(String, f32)>,
    pub blend: Blend,
    pub two_sided: bool,
    pub unlit: bool,
    pub cutoff: f32,
    pub chain: Vec<String>,
    /// Effective static switch values (from the leaf-most compiled instance).
    pub switches: Vec<(String, bool)>,
    /// Effective static component masks (RGBA channel selection).
    pub masks: Vec<(String, [bool; 4])>,
    /// The compiled resource the renderer uses: the leaf-most instance with a static
    /// permutation, else the base material.
    pub resource: Option<(Obj, MaterialResource)>,
    /// The base material's resource (its id keys the static permutations' shader maps).
    pub base_resource: Option<MaterialResource>,
    /// Physical material name (leaf-most), e.g. `Phm_Stone`: footstep / impact surface.
    pub phys: Option<String>,
}

/// The FMaterialResource serialized after a material's tagged properties.
#[derive(Clone, Debug)]
pub struct MaterialResource {
    pub id: [u8; 16],
    /// `UniformExpressionTextures` (object references in the owner's package)
    pub textures: Vec<i32>,
}

pub fn material_resource(o: &Obj) -> Option<MaterialResource> {
    let od = upk::read_object(&o.pkg, o.idx).ok()?;
    let mut r = od.reader;
    let n = r.count(4).ok()?;
    for _ in 0..n {
        r.fstring().ok()?;
    }
    let n = r.count(8).ok()?;
    r.skip(n * 8).ok()?;
    let _max_dep = r.i32().ok()?;
    let g = r.guid().ok()?;
    let mut id = [0u8; 16];
    for (k, w) in g.iter().enumerate() {
        id[k * 4..k * 4 + 4].copy_from_slice(&w.to_le_bytes());
    }
    let _num_uv = r.u32().ok()?;
    let n = r.count(4).ok()?;
    if n > 256 {
        return None;
    }
    let textures = (0..n).map(|_| r.i32()).collect::<Result<Vec<_>, _>>().ok()?;
    Some(MaterialResource { id, textures })
}

/// FStaticParameterSet of a material instance with a static permutation resource.
#[derive(Clone, Default, Debug)]
pub struct StaticParams {
    pub switches: Vec<(String, bool)>,
    pub masks: Vec<(String, [bool; 4])>,
}

/// Locate and parse the FStaticParameterSet in an instance's native data.
///
/// It follows the compiled FMaterialResource whose layout we don't fully decode, so the
/// switch array is found by its shape: `count` entries of {FName, UBOOL value, UBOOL
/// override, FGuid} followed by `count` component masks of {FName, 4 x UBOOL, UBOOL, FGuid}.
pub fn static_params(o: &Obj) -> Option<StaticParams> {
    let od = upk::read_object(&o.pkg, o.idx).ok()?;
    let e = o.pkg.exports.get(o.idx as usize - 1)?;
    let (start, end) = (od.reader.pos, (e.offset + e.size).min(o.pkg.data.len()));
    let d = &o.pkg.data;
    let u32_at = |p: usize| -> Option<u32> { d.get(p..p + 4).map(|b| u32::from_le_bytes(b.try_into().unwrap())) };
    let name_at = |p: usize| -> Option<String> {
        let idx = u32_at(p)? as usize;
        let num = u32_at(p + 4)?;
        if idx == 0 || idx >= o.pkg.names.len() || num > 64 {
            return None;
        }
        Some(o.pkg.name_of(idx as i32, num as i32))
    };
    let is_bool = |p: usize| matches!(u32_at(p), Some(0) | Some(1));
    let mut p = start;
    while p + 8 <= end {
        let n = u32_at(p)? as usize;
        if (1..=512).contains(&n) && p + 4 + n * 32 + 4 <= end {
            let ok = (0..n).all(|k| {
                let q = p + 4 + k * 32;
                name_at(q).is_some() && is_bool(q + 8) && is_bool(q + 12)
            });
            if ok {
                let mut sp = StaticParams::default();
                for k in 0..n {
                    let q = p + 4 + k * 32;
                    sp.switches.push((name_at(q)?, u32_at(q + 8)? == 1));
                }
                let mp = p + 4 + n * 32;
                let m = u32_at(mp).unwrap_or(0) as usize;
                if m <= 512 && mp + 4 + m * 44 <= end {
                    for k in 0..m {
                        let q = mp + 4 + k * 44;
                        let (Some(nm), true) = (name_at(q), (0..5).all(|c| is_bool(q + 8 + c * 4))) else { break };
                        let ch = [0, 1, 2, 3].map(|c| u32_at(q + 8 + c * 4) == Some(1));
                        sp.masks.push((nm, ch));
                    }
                }
                return Some(sp);
            }
        }
        p += 4;
    }
    None
}

impl ResolvedMaterial {
    /// Whether a texture parameter is actually sampled by the compiled material
    /// (false for parameters disabled through static switches).
    pub fn param_used(&self, name: &str) -> bool {
        let Some((used, lvl)) = &self.used else { return true };
        let visible = self
            .tex_levels
            .iter()
            .filter(|(n, _, l)| n.eq_ignore_ascii_case(name) && *l >= *lvl)
            .min_by_key(|(_, _, l)| *l);
        match visible {
            Some((_, o, _)) => used.contains(&o.key()),
            None => true,
        }
    }

    pub fn obj_used(&self, o: &Obj) -> bool {
        match &self.used {
            Some((used, _)) => used.contains(&o.key()),
            None => true,
        }
    }

    pub fn tex(&self, name: &str) -> Option<&Obj> {
        self.textures.iter().find(|(n, _)| n.eq_ignore_ascii_case(name)).map(|(_, o)| o)
    }

    pub fn vector(&self, name: &str) -> Option<[f32; 4]> {
        self.vectors.iter().find(|(n, _)| n.eq_ignore_ascii_case(name)).map(|(_, v)| *v)
    }

    pub fn scalar(&self, name: &str) -> Option<f32> {
        self.scalars.iter().find(|(n, _)| n.eq_ignore_ascii_case(name)).map(|(_, v)| *v)
    }

    /// Effective static switch value, if the material exposes that switch.
    pub fn switch(&self, name: &str) -> Option<bool> {
        self.switches.iter().find(|(n, _)| n.eq_ignore_ascii_case(name)).map(|(_, v)| *v)
    }

    /// Selected channel (0..3) of a static component mask parameter.
    pub fn mask_channel(&self, name: &str) -> Option<usize> {
        self.masks.iter().find(|(n, _)| n.eq_ignore_ascii_case(name)).and_then(|(_, m)| m.iter().position(|b| *b))
    }

    /// Find the first texture parameter whose name matches any predicate, in priority order.
    pub fn find_tex(&self, exact: &[&str], contains: &[&str], exclude: &[&str]) -> Option<Obj> {
        for e in exact {
            if let Some(o) = self.tex(e) {
                if self.param_used(e) && !is_placeholder(o) {
                    return Some(o.clone());
                }
            }
        }
        for c in contains {
            for (n, o) in &self.textures {
                let l = n.to_ascii_lowercase();
                if l.contains(c) && !exclude.iter().any(|x| l.contains(x)) && self.param_used(n) && !is_placeholder(o) {
                    return Some(o.clone());
                }
            }
        }
        None
    }

    /// Fallback: pick a texture by its object name suffix (e.g. "_d", "_n").
    pub fn find_by_suffix(&self, suffixes: &[&str]) -> Option<Obj> {
        let named = self.textures.iter().filter(|(n, _)| self.param_used(n)).map(|(_, o)| o);
        let unnamed = self.unnamed.iter().filter(|o| self.obj_used(o));
        for o in named.chain(unnamed) {
            let n = o.name().to_ascii_lowercase();
            if suffixes.iter().any(|s| n.ends_with(s)) && !is_placeholder(o) {
                return Some(o.clone());
            }
        }
        None
    }
}

/// Engine/template default textures that only exist as unset parameter placeholders.
pub fn is_placeholder(o: &Obj) -> bool {
    let k = o.key();
    k.starts_with("materials_ref.textures.") || k.starts_with("enginematerials.") || k.starts_with("engineresources.")
}

/// Read `UniformExpressionTextures` from a compiled FMaterialResource that follows the
/// object's tagged properties (UMaterial, or UMaterialInstance with a static permutation).
fn uniform_textures(assets: &Assets, o: &Obj) -> Option<std::collections::HashSet<String>> {
    let od = upk::read_object(&o.pkg, o.idx).ok()?;
    let mut r = od.reader;
    let n = r.count(4).ok()?;
    for _ in 0..n {
        r.fstring().ok()?;
    }
    let n = r.count(8).ok()?;
    r.skip(n * 8).ok()?;
    let _max_dep = r.i32().ok()?;
    let _id = r.guid().ok()?;
    let _num_uv = r.u32().ok()?;
    let n = r.count(4).ok()?;
    if n > 256 {
        return None;
    }
    let mut set = std::collections::HashSet::new();
    for _ in 0..n {
        let i = r.i32().ok()?;
        if i == 0 {
            continue;
        }
        match assets.resolve(&o.pkg, i) {
            Some(t) if t.class().starts_with("Texture") => {
                set.insert(t.key());
            }
            Some(_) => return None,
            None => {
                set.insert(o.pkg.obj_path(i).to_ascii_lowercase());
            }
        }
    }
    Some(set)
}

fn push_unique<T>(v: &mut Vec<(String, T)>, name: String, val: T) {
    if !v.iter().any(|(n, _)| n.eq_ignore_ascii_case(&name)) {
        v.push((name, val));
    }
}

pub fn resolve_material(assets: &Assets, start: &Obj) -> ResolvedMaterial {
    let mut out = ResolvedMaterial { cutoff: 0.3333, ..Default::default() };
    let mut cur = Some(start.clone());
    let mut depth = 0;
    while let Some(o) = cur.take() {
        depth += 1;
        if depth > 16 {
            break;
        }
        let level = depth - 1;
        out.chain.push(o.path());
        let cls = o.class();
        let Ok(props) = o.props() else { break };
        if out.phys.is_none() {
            if let Some(p) = props.object("PhysMaterial").filter(|p| *p != 0) {
                let path = o.pkg.obj_path(p);
                out.phys = Some(path.rsplit('.').next().unwrap_or(&path).to_string());
            }
        }
        if cls.starts_with("MaterialInstance") {
            if let Some((c, off, sz)) = props.array("TextureParameterValues") {
                if let Ok(items) = parse_struct_array(&o.pkg, off, sz, c) {
                    for it in items {
                        let name = it.name("ParameterName").unwrap_or("").to_string();
                        if let Some(t) = it.object("ParameterValue").and_then(|i| assets.resolve(&o.pkg, i)) {
                            out.tex_levels.push((name.clone(), t.clone(), level));
                            push_unique(&mut out.textures, name, t);
                        }
                    }
                }
            }
            if let Some((c, off, sz)) = props.array("VectorParameterValues") {
                if let Ok(items) = parse_struct_array(&o.pkg, off, sz, c) {
                    for it in items {
                        let name = it.name("ParameterName").unwrap_or("").to_string();
                        if let Some(v) = it.linear_color("ParameterValue") {
                            push_unique(&mut out.vectors, name, v);
                        }
                    }
                }
            }
            if let Some((c, off, sz)) = props.array("ScalarParameterValues") {
                if let Ok(items) = parse_struct_array(&o.pkg, off, sz, c) {
                    for it in items {
                        let name = it.name("ParameterName").unwrap_or("").to_string();
                        if let Some(v) = it.float("ParameterValue") {
                            push_unique(&mut out.scalars, name, v);
                        }
                    }
                }
            }
            if props.bool("bHasStaticPermutationResource").unwrap_or(false) {
                if out.resource.is_none() {
                    out.resource = material_resource(&o).map(|r| (o.clone(), r));
                }
                if out.used.is_none() {
                    if let Some(u) = uniform_textures(assets, &o) {
                        out.used = Some((u, level));
                    }
                }
                // the leaf-most compiled instance holds the effective static parameter values
                if out.switches.is_empty() {
                    if let Some(sp) = static_params(&o) {
                        out.switches = sp.switches;
                        out.masks = sp.masks;
                    }
                }
            }
            cur = props.object("Parent").and_then(|p| assets.resolve(&o.pkg, p));
            continue;
        }
        if cls.ends_with("Material") {
            out.blend = match props.name("BlendMode").unwrap_or("BLEND_Opaque") {
                "BLEND_Masked" | "BLEND_SoftMasked" | "BLEND_DitheredTranslucent" => Blend::Masked,
                "BLEND_Translucent" | "BLEND_AlphaComposite" => Blend::Translucent,
                "BLEND_Additive" => Blend::Additive,
                "BLEND_Modulate" | "BLEND_ModulateAndAdd" => Blend::Modulate,
                _ => Blend::Opaque,
            };
            out.two_sided = props.bool("TwoSided").unwrap_or(false);
            let res = material_resource(&o);
            if out.resource.is_none() {
                out.resource = res.clone().map(|r| (o.clone(), r));
            }
            out.base_resource = res;
            if out.used.is_none() {
                if let Some(u) = uniform_textures(assets, &o) {
                    out.used = Some((u, level));
                }
            }
            out.unlit = props.name("LightingModel") == Some("MLM_Unlit");
            if let Some(c) = props.float("OpacityMaskClipValue") {
                out.cutoff = c;
            }
            let mut exprs = object_array(&o.pkg, &props, "Expressions");
            if exprs.is_empty() {
                exprs = (1..=o.pkg.exports.len() as i32).filter(|&i| o.pkg.obj_outer(i) == o.idx).collect();
            }
            for e in exprs {
                let Some(eo) = assets.resolve(&o.pkg, e) else { continue };
                let ecls = eo.class();
                if !ecls.starts_with("MaterialExpression") {
                    continue;
                }
                let Ok(ep) = eo.props() else { continue };
                let pname = ep.name("ParameterName").map(|s| s.to_string());
                if ecls.contains("TextureSample") {
                    if let Some(t) = ep.object("Texture").and_then(|i| assets.resolve(&eo.pkg, i)) {
                        if !t.class().starts_with("Texture2D") {
                            continue;
                        }
                        match pname {
                            Some(n) => {
                                out.tex_levels.push((n.clone(), t.clone(), level));
                                push_unique(&mut out.textures, n, t);
                            }
                            None => out.unnamed.push(t),
                        }
                    }
                } else if ecls.contains("VectorParameter") {
                    if let (Some(n), Some(Value::LinearColor(v))) = (pname, ep.get("DefaultValue")) {
                        push_unique(&mut out.vectors, n, *v);
                    }
                } else if ecls.contains("ScalarParameter") {
                    if let (Some(n), Some(v)) = (pname, ep.float("DefaultValue")) {
                        push_unique(&mut out.scalars, n, v);
                    }
                }
            }
        }
        break;
    }
    out
}
