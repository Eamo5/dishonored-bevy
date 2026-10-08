//! UE3 `ShaderCache` objects (`RefShaderCache-PC-D3D-SM3.upk`): the compiled D3D9 shaders
//! of every material, keyed by GUID, and the material shader maps that select them.

use anyhow::{bail, Result};

/// One compiled shader.
#[derive(Clone, Debug)]
pub struct ShaderRecord {
    /// shader type name, e.g. `TBasePassPixelShaderFNoLightMapPolicyNoSkyLightFALSEFALSE`
    pub ty: String,
    pub guid: [u8; 16],
    /// 0 vertex, 1 pixel
    pub frequency: u8,
    /// offsets of the bytecode and the record end, relative to the export data
    pub code: std::ops::Range<usize>,
    pub end: usize,
}

/// Shader records of a ShaderCache export (`data` = the export's bytes, `base` = its file
/// offset, which the record skip offsets are relative to).
pub fn records(data: &[u8], names: &[String], base: usize) -> Result<Vec<ShaderRecord>> {
    let u32_at = |o: usize| -> Result<u32> {
        match data.get(o..o + 4) {
            Some(b) => Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]])),
            None => bail!("truncated shader cache at {o:#x}"),
        }
    };
    let count = u32_at(0x11)? as usize;
    let mut p = 0x15;
    let mut out = Vec::with_capacity(count);
    for _ in 0..count {
        let ty = names.get(u32_at(p)? as usize).cloned().unwrap_or_default();
        let mut guid = [0u8; 16];
        guid.copy_from_slice(&data[p + 8..p + 24]);
        let end = (u32_at(p + 44)? as usize).checked_sub(base).unwrap_or(0);
        if end <= p || end > data.len() {
            bail!("bad shader record skip at {p:#x}");
        }
        let n = u32_at(p + 48)? as usize;
        let q = p + 52 + n * 2;
        let frequency = data[q + 1];
        let len = u32_at(q + 2)? as usize;
        let code = q + 6..q + 6 + len;
        if code.end > end {
            bail!("shader code past record end at {p:#x}");
        }
        out.push(ShaderRecord { ty, guid, frequency, code, end });
        p = end;
    }
    Ok(out)
}

/// A material's uniform expression (evaluated per frame, feeding shader constants).
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub enum Expr {
    Constant([f32; 4]),
    VectorParameter(String, [f32; 4]),
    ScalarParameter(String, f32),
    /// index into the material's `UniformExpressionTextures`
    Texture(i32),
    TextureParameter(String, i32),
    FlipBook(i32),
    Time,
    RealTime,
    Sine(Box<Expr>, bool),
    Periodic(Box<Expr>),
    Floor(Box<Expr>),
    Ceil(Box<Expr>),
    Frac(Box<Expr>),
    Abs(Box<Expr>),
    Square(Box<Expr>),
    Clamp(Box<Expr>, Box<Expr>, Box<Expr>),
    Min(Box<Expr>, Box<Expr>),
    Max(Box<Expr>, Box<Expr>),
    Fmod(Box<Expr>, Box<Expr>),
    /// op: 0 add, 1 sub, 2 mul, 3 div, 4 dot
    Math(Box<Expr>, Box<Expr>, u8),
    /// (a, b, number of components of a)
    Append(Box<Expr>, Box<Expr>, u32),
}

/// Uniform expressions of one shader frequency.
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct FrequencyExprs {
    pub vectors: Vec<Expr>,
    pub scalars: Vec<Expr>,
    pub textures: Vec<Expr>,
}

#[derive(Clone, Debug, Default)]
pub struct StaticParameterSet {
    pub base_id: [u8; 16],
    pub switches: Vec<(String, bool)>,
    pub masks: Vec<(String, [bool; 4])>,
}

/// One material shader map: the compiled shaders of a material (static permutation) per
/// vertex factory, and its uniform expressions.
#[derive(Clone, Debug, Default)]
pub struct ShaderMap {
    pub params: StaticParameterSet,
    pub material_id: [u8; 16],
    pub name: String,
    /// shaders independent of the vertex factory: (shader type, guid)
    pub shaders: Vec<(String, [u8; 16])>,
    /// (vertex factory, [(shader type, guid)])
    pub mesh_maps: Vec<(String, Vec<(String, [u8; 16])>)>,
    pub pixel: FrequencyExprs,
    pub cube_textures: Vec<Expr>,
    pub vertex: FrequencyExprs,
}

impl ShaderMap {
    pub fn shader(&self, vertex_factory: &str, ty: &str) -> Option<[u8; 16]> {
        let (_, list) = self.mesh_maps.iter().find(|(vf, _)| vf == vertex_factory)?;
        list.iter().find(|(t, _)| t == ty).map(|(_, g)| *g)
    }
}

struct Rd<'a> {
    d: &'a [u8],
    p: usize,
    names: &'a [String],
}

impl Rd<'_> {
    fn bytes(&mut self, n: usize) -> Result<&[u8]> {
        match self.d.get(self.p..self.p + n) {
            Some(b) => {
                self.p += n;
                Ok(b)
            }
            None => bail!("truncated at {:#x}", self.p),
        }
    }
    fn u8(&mut self) -> Result<u8> {
        Ok(self.bytes(1)?[0])
    }
    fn u32(&mut self) -> Result<u32> {
        let b = self.bytes(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }
    fn i32(&mut self) -> Result<i32> {
        Ok(self.u32()? as i32)
    }
    fn f32(&mut self) -> Result<f32> {
        Ok(f32::from_bits(self.u32()?))
    }
    fn count(&mut self) -> Result<usize> {
        let n = self.u32()? as usize;
        if n > 100_000 {
            bail!("bad count {n} at {:#x}", self.p);
        }
        Ok(n)
    }
    fn guid(&mut self) -> Result<[u8; 16]> {
        let mut g = [0u8; 16];
        g.copy_from_slice(self.bytes(16)?);
        Ok(g)
    }
    fn name(&mut self) -> Result<String> {
        let i = self.u32()? as usize;
        let n = self.u32()?;
        let base = self.names.get(i).cloned().unwrap_or_default();
        Ok(if n == 0 { base } else { format!("{base}_{}", n - 1) })
    }
    fn fstring(&mut self) -> Result<String> {
        let n = self.i32()?;
        if n < 0 {
            let b = self.bytes((-n) as usize * 2)?;
            let w: Vec<u16> = b.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
            Ok(String::from_utf16_lossy(&w).trim_end_matches('\0').to_string())
        } else {
            let b = self.bytes(n as usize)?;
            Ok(b.iter().map(|&c| c as char).collect::<String>().trim_end_matches('\0').to_string())
        }
    }
    fn vec4(&mut self) -> Result<[f32; 4]> {
        Ok([self.f32()?, self.f32()?, self.f32()?, self.f32()?])
    }
    fn static_params(&mut self) -> Result<StaticParameterSet> {
        let mut s = StaticParameterSet { base_id: self.guid()?, ..Default::default() };
        for _ in 0..self.count()? {
            let n = self.name()?;
            let v = self.u32()? != 0;
            self.u32()?;
            self.guid()?;
            s.switches.push((n, v));
        }
        for _ in 0..self.count()? {
            let n = self.name()?;
            let m = [self.u32()? != 0, self.u32()? != 0, self.u32()? != 0, self.u32()? != 0];
            self.u32()?;
            self.guid()?;
            s.masks.push((n, m));
        }
        for _ in 0..self.count()? {
            // normal parameters
            self.name()?;
            self.u8()?;
            self.u32()?;
            self.guid()?;
        }
        for _ in 0..self.count()? {
            // terrain layer weights
            self.name()?;
            self.i32()?;
            self.u32()?;
            self.guid()?;
        }
        Ok(s)
    }
    fn shader_list(&mut self) -> Result<Vec<(String, [u8; 16])>> {
        let n = self.count()?;
        let mut v = Vec::with_capacity(n);
        for _ in 0..n {
            let ty = self.name()?;
            let g = self.guid()?;
            self.name()?;
            v.push((ty, g));
        }
        Ok(v)
    }
    fn expr(&mut self) -> Result<Expr> {
        let t = self.name()?;
        let b = |r: &mut Self| -> Result<Box<Expr>> { Ok(Box::new(r.expr()?)) };
        Ok(match t.trim_start_matches("FMaterialUniformExpression") {
            "Constant" => {
                let v = self.vec4()?;
                self.u8()?;
                Expr::Constant(v)
            }
            "VectorParameter" => Expr::VectorParameter(self.name()?, self.vec4()?),
            "ScalarParameter" => Expr::ScalarParameter(self.name()?, self.f32()?),
            "Texture" => Expr::Texture(self.i32()?),
            "TextureParameter" => Expr::TextureParameter(self.name()?, self.i32()?),
            "FlipBookTextureParameter" => Expr::FlipBook(self.i32()?),
            "Time" => Expr::Time,
            "RealTime" => Expr::RealTime,
            "Sine" => Expr::Sine(b(self)?, self.u32()? != 0),
            "Periodic" => Expr::Periodic(b(self)?),
            "Floor" => Expr::Floor(b(self)?),
            "Ceil" => Expr::Ceil(b(self)?),
            "Frac" => Expr::Frac(b(self)?),
            "Abs" => Expr::Abs(b(self)?),
            "Square" => Expr::Square(b(self)?),
            "Clamp" => Expr::Clamp(b(self)?, b(self)?, b(self)?),
            "Min" => Expr::Min(b(self)?, b(self)?),
            "Max" => Expr::Max(b(self)?, b(self)?),
            "Fmod" => Expr::Fmod(b(self)?, b(self)?),
            "FoldedMath" => Expr::Math(b(self)?, b(self)?, self.u8()?),
            "AppendVector" => Expr::Append(b(self)?, b(self)?, self.u32()?),
            other => bail!("unknown uniform expression {other} at {:#x}", self.p),
        })
    }
    fn exprs(&mut self) -> Result<Vec<Expr>> {
        let n = self.count()?;
        (0..n).map(|_| self.expr()).collect()
    }
}

/// The material shader maps, which follow the last shader record.
pub fn shader_maps(data: &[u8], names: &[String], base: usize, records: &[ShaderRecord]) -> Result<Vec<ShaderMap>> {
    let start = records.last().map(|r| r.end).unwrap_or(0x15);
    let mut r = Rd { d: data, p: start, names };
    let n = r.count()?;
    let mut out = Vec::with_capacity(n);
    for _ in 0..n {
        let params = r.static_params()?;
        r.u32()?;
        r.u32()?;
        let end = (r.u32()? as usize).checked_sub(base).unwrap_or(0);
        if end <= r.p || end > data.len() {
            bail!("bad shader map skip at {:#x}", r.p);
        }
        let parse = |r: &mut Rd, params: StaticParameterSet| -> Result<ShaderMap> {
            let shaders = r.shader_list()?;
            let mut mesh_maps = Vec::new();
            for _ in 0..r.count()? {
                let list = r.shader_list()?;
                mesh_maps.push((r.name()?, list));
            }
            let material_id = r.guid()?;
            let name = r.fstring()?;
            r.static_params()?;
            let pixel = FrequencyExprs { vectors: r.exprs()?, scalars: r.exprs()?, textures: r.exprs()? };
            let cube_textures = r.exprs()?;
            let vertex = FrequencyExprs { vectors: r.exprs()?, scalars: r.exprs()?, textures: Vec::new() };
            Ok(ShaderMap { params, material_id, name, shaders, mesh_maps, pixel, cube_textures, vertex })
        };
        match parse(&mut r, params) {
            Ok(m) => out.push(m),
            Err(e) => log::warn!("shader map at {:#x}: {e:#}", r.p),
        }
        r.p = end;
    }
    Ok(out)
}

/// The reference shader cache: shader bytecode by GUID and the material shader maps.
pub struct ShaderLibrary {
    pub data: Vec<u8>,
    pub records: std::collections::HashMap<[u8; 16], ShaderRecord>,
    pub maps: Vec<ShaderMap>,
    /// material id -> shader maps
    pub by_material: std::collections::HashMap<[u8; 16], Vec<usize>>,
    /// base material id of the static parameter set -> shader maps
    pub by_base: std::collections::HashMap<[u8; 16], Vec<usize>>,
}

impl ShaderLibrary {
    pub fn load(pkg: &upk::Package) -> Result<ShaderLibrary> {
        let Some(idx) = pkg.exports_of_class("ShaderCache").next() else { bail!("no ShaderCache export") };
        let e = &pkg.exports[idx as usize - 1];
        let data = pkg.export_data(idx)?.to_vec();
        let recs = records(&data, &pkg.names, e.offset)?;
        let maps = shader_maps(&data, &pkg.names, e.offset, &recs)?;
        let mut by_material: std::collections::HashMap<[u8; 16], Vec<usize>> = Default::default();
        let mut by_base: std::collections::HashMap<[u8; 16], Vec<usize>> = Default::default();
        for (i, m) in maps.iter().enumerate() {
            by_material.entry(m.material_id).or_default().push(i);
            by_base.entry(m.params.base_id).or_default().push(i);
        }
        Ok(ShaderLibrary { data, records: recs.into_iter().map(|r| (r.guid, r)).collect(), maps, by_material, by_base })
    }

    pub fn code(&self, guid: &[u8; 16]) -> Option<&[u8]> {
        self.records.get(guid).map(|r| &self.data[r.code.clone()])
    }

    /// Index of the shader map of a material: a static permutation (base material id +
    /// switch / mask values) when `switches` or `masks` are given, else the base material's
    /// own map.
    pub fn find(&self, base_id: &[u8; 16], switches: &[(String, bool)], masks: &[(String, [bool; 4])]) -> Option<usize> {
        if switches.is_empty() && masks.is_empty() {
            return self.by_material.get(base_id)?.iter().copied().find(|&i| self.maps[i].params.switches.is_empty() && self.maps[i].params.masks.is_empty());
        }
        let cands = self.by_base.get(base_id)?;
        // agreement score: every listed value must match
        let score = |m: &ShaderMap| -> i32 {
            let mut s = 0;
            for (n, v) in &m.params.switches {
                match switches.iter().find(|(k, _)| k.eq_ignore_ascii_case(n)) {
                    Some((_, w)) if w == v => s += 1,
                    Some(_) => s -= 1000,
                    None => s -= 1,
                }
            }
            for (n, v) in &m.params.masks {
                match masks.iter().find(|(k, _)| k.eq_ignore_ascii_case(n)) {
                    Some((_, w)) if w == v => s += 1,
                    Some(_) => s -= 1000,
                    None => s -= 1,
                }
            }
            s
        };
        let best = cands.iter().copied().filter(|&i| !self.maps[i].params.switches.is_empty() || !self.maps[i].params.masks.is_empty()).max_by_key(|&i| score(&self.maps[i]))?;
        (score(&self.maps[best]) > -1000).then_some(best)
    }
}

fn walk_params<'a>(e: &'a Expr, out: &mut Vec<&'a Expr>) {
    match e {
        Expr::VectorParameter(..) | Expr::ScalarParameter(..) => out.push(e),
        Expr::Sine(a, _) | Expr::Periodic(a) | Expr::Floor(a) | Expr::Ceil(a) | Expr::Frac(a) | Expr::Abs(a) | Expr::Square(a) => {
            walk_params(a, out)
        }
        Expr::Clamp(a, b, c) => {
            walk_params(a, out);
            walk_params(b, out);
            walk_params(c, out);
        }
        Expr::Min(a, b) | Expr::Max(a, b) | Expr::Fmod(a, b) | Expr::Math(a, b, _) | Expr::Append(a, b, _) => {
            walk_params(a, out);
            walk_params(b, out);
        }
        _ => {}
    }
}

/// The parameter nodes of a shader map's uniform expressions (pixel vectors, pixel scalars,
/// vertex vectors, vertex scalars; depth first): the slots of a material's parameter values.
pub fn param_nodes(m: &CookedMap) -> Vec<&Expr> {
    let mut out = Vec::new();
    for e in m.pixel.vectors.iter().chain(&m.pixel.scalars).chain(&m.vertex.vectors).chain(&m.vertex.scalars) {
        walk_params(e, &mut out);
    }
    out
}

/// A shader map as cooked for the game.
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct CookedMap {
    pub name: String,
    /// shaders not tied to a vertex factory (shader type, guid): post-process materials'
    #[serde(default)]
    pub shaders: Vec<(String, [u8; 16])>,
    /// (vertex factory, [(shader type, guid)])
    pub mesh_maps: Vec<(String, Vec<(String, [u8; 16])>)>,
    pub pixel: FrequencyExprs,
    pub cube_textures: Vec<Expr>,
    pub vertex: FrequencyExprs,
}

impl CookedMap {
    pub fn shader(&self, vertex_factory: &str, ty: &str) -> Option<[u8; 16]> {
        let (_, list) = self.mesh_maps.iter().find(|(vf, _)| vf == vertex_factory)?;
        list.iter().find(|(t, _)| t == ty).map(|(_, g)| *g)
    }
}

impl From<&ShaderMap> for CookedMap {
    fn from(m: &ShaderMap) -> Self {
        CookedMap {
            name: m.name.clone(),
            shaders: m.shaders.clone(),
            mesh_maps: m.mesh_maps.clone(),
            pixel: m.pixel.clone(),
            cube_textures: m.cube_textures.clone(),
            vertex: m.vertex.clone(),
        }
    }
}

/// The cooked shader library (`shaders.bin`): shader maps and the D3D9 bytecode of their
/// shaders, translated to WGSL when a level loads.
#[derive(Default)]
pub struct CookedLibrary {
    pub maps: Vec<CookedMap>,
    pub code: std::collections::HashMap<[u8; 16], Vec<u8>>,
    /// engine (global) shaders by name, e.g. Arkane's post-processing
    pub globals: std::collections::BTreeMap<String, Vec<u8>>,
}

const LIB_MAGIC: &[u8; 4] = b"DHS1";

impl CookedLibrary {
    pub fn from_library(lib: &ShaderLibrary) -> CookedLibrary {
        let maps: Vec<CookedMap> = lib.maps.iter().map(CookedMap::from).collect();
        let mut code = std::collections::HashMap::new();
        for m in &maps {
            for (_, g) in m.mesh_maps.iter().flat_map(|(_, list)| list.iter()).chain(&m.shaders) {
                if let Some(c) = lib.code(g) {
                    code.entry(*g).or_insert_with(|| c.to_vec());
                }
            }
        }
        CookedLibrary { maps, code, globals: Default::default() }
    }

    pub fn write(&self, path: &std::path::Path) -> Result<()> {
        let mut out = Vec::new();
        out.extend_from_slice(LIB_MAGIC);
        let json = serde_json::to_vec(&self.maps)?;
        out.extend_from_slice(&(json.len() as u32).to_le_bytes());
        out.extend_from_slice(&json);
        let mut keys: Vec<_> = self.code.keys().copied().collect();
        keys.sort();
        out.extend_from_slice(&(keys.len() as u32).to_le_bytes());
        for k in keys {
            let c = &self.code[&k];
            out.extend_from_slice(&k);
            out.extend_from_slice(&(c.len() as u32).to_le_bytes());
            out.extend_from_slice(c);
        }
        out.extend_from_slice(&(self.globals.len() as u32).to_le_bytes());
        for (name, c) in &self.globals {
            out.extend_from_slice(&(name.len() as u32).to_le_bytes());
            out.extend_from_slice(name.as_bytes());
            out.extend_from_slice(&(c.len() as u32).to_le_bytes());
            out.extend_from_slice(c);
        }
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, out)?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    }

    pub fn read(path: &std::path::Path) -> Result<CookedLibrary> {
        let d = std::fs::read(path)?;
        if d.get(0..4) != Some(LIB_MAGIC.as_slice()) {
            bail!("not a shader library");
        }
        let mut r = Rd { d: &d, p: 4, names: &[] };
        let n = r.u32()? as usize;
        let maps: Vec<CookedMap> = serde_json::from_slice(r.bytes(n)?)?;
        let count = r.u32()? as usize;
        let mut code = std::collections::HashMap::with_capacity(count);
        for _ in 0..count {
            let g = r.guid()?;
            let len = r.u32()? as usize;
            code.insert(g, r.bytes(len)?.to_vec());
        }
        let mut globals = std::collections::BTreeMap::new();
        if r.p < d.len() {
            for _ in 0..r.u32()? {
                let n = r.u32()? as usize;
                let name = String::from_utf8_lossy(r.bytes(n)?).to_string();
                let len = r.u32()? as usize;
                globals.insert(name, r.bytes(len)?.to_vec());
            }
        }
        Ok(CookedLibrary { maps, code, globals })
    }
}

/// A shader of the global shader cache (`GlobalShaderCache-PC-D3D-SM3.bin`): engine shaders
/// such as Arkane's post-processing.
#[derive(Clone, Debug)]
pub struct GlobalShader {
    pub name: String,
    pub guid: [u8; 16],
    /// 0 vertex, 1 pixel
    pub frequency: u8,
    pub code: Vec<u8>,
}

/// Shaders of the global shader cache file.
pub fn global_shaders(d: &[u8]) -> Result<Vec<GlobalShader>> {
    if d.get(0..4) != Some(b"BMSG".as_slice()) {
        bail!("not a global shader cache");
    }
    let mut r = Rd { d, p: 17, names: &[] };
    let count = r.count()?;
    let mut out = Vec::with_capacity(count);
    for _ in 0..count {
        let name = r.fstring()?;
        let guid = r.guid()?;
        r.bytes(20)?;
        let end = r.u32()? as usize;
        let n = r.count()?;
        r.bytes(n * 2)?;
        r.u8()?;
        let frequency = r.u8()?;
        let len = r.u32()? as usize;
        let code = r.bytes(len)?.to_vec();
        out.push(GlobalShader { name, guid, frequency, code });
        if end <= r.p.saturating_sub(len) || end > d.len() {
            bail!("bad global shader record end");
        }
        r.p = end;
    }
    Ok(out)
}
