//! UE3 tagged property parsing.

use crate::package::Package;
use crate::reader::Reader;
use anyhow::{bail, Result};

pub const RF_CLASS_DEFAULT: u64 = 0x200;
pub const RF_HAS_STACK: u64 = 0x0200_0000_0000_0000;

#[derive(Debug, Clone)]
pub enum Value {
    Int(i32),
    Float(f32),
    Bool(bool),
    Byte(u8),
    Enum(String),
    Name(String),
    Str(String),
    Object(i32),
    Delegate(i32, String),
    Vector([f32; 3]),
    Rotator([i32; 3]),
    Color([u8; 4]), // stored as B,G,R,A in file; we keep R,G,B,A
    LinearColor([f32; 4]),
    Vec4([f32; 4]),
    Vec2([f32; 2]),
    Guid([u32; 4]),
    Box { min: [f32; 3], max: [f32; 3] },
    Matrix([f32; 16]),
    IntPoint([i32; 2]),
    Struct(String, Vec<Prop>),
    /// Array: element count + raw element bytes (absolute offset into package data).
    Array { count: usize, offset: usize, size: usize },
    Raw { offset: usize, size: usize },
}

#[derive(Debug, Clone)]
pub struct Prop {
    pub name: String,
    pub ty: String,
    pub index: i32,
    pub value: Value,
}

#[derive(Debug, Clone, Default)]
pub struct Props(pub Vec<Prop>);

impl Props {
    pub fn get(&self, name: &str) -> Option<&Value> {
        self.0.iter().find(|p| p.name.eq_ignore_ascii_case(name) && p.index == 0).map(|p| &p.value)
    }
    pub fn get_idx(&self, name: &str, idx: i32) -> Option<&Value> {
        self.0.iter().find(|p| p.name.eq_ignore_ascii_case(name) && p.index == idx).map(|p| &p.value)
    }
    pub fn all<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a Prop> + 'a {
        self.0.iter().filter(move |p| p.name.eq_ignore_ascii_case(name))
    }
    pub fn int(&self, name: &str) -> Option<i32> {
        match self.get(name)? {
            Value::Int(v) => Some(*v),
            _ => None,
        }
    }
    pub fn float(&self, name: &str) -> Option<f32> {
        match self.get(name)? {
            Value::Float(v) => Some(*v),
            Value::Int(v) => Some(*v as f32),
            _ => None,
        }
    }
    pub fn bool(&self, name: &str) -> Option<bool> {
        match self.get(name)? {
            Value::Bool(v) => Some(*v),
            _ => None,
        }
    }
    pub fn object(&self, name: &str) -> Option<i32> {
        match self.get(name)? {
            Value::Object(v) => Some(*v),
            _ => None,
        }
    }
    pub fn name(&self, name: &str) -> Option<&str> {
        match self.get(name)? {
            Value::Name(v) | Value::Enum(v) | Value::Str(v) => Some(v.as_str()),
            _ => None,
        }
    }
    pub fn byte(&self, name: &str) -> Option<u8> {
        match self.get(name)? {
            Value::Byte(v) => Some(*v),
            _ => None,
        }
    }
    pub fn vector(&self, name: &str) -> Option<[f32; 3]> {
        match self.get(name)? {
            Value::Vector(v) => Some(*v),
            _ => None,
        }
    }
    pub fn rotator(&self, name: &str) -> Option<[i32; 3]> {
        match self.get(name)? {
            Value::Rotator(v) => Some(*v),
            _ => None,
        }
    }
    pub fn color(&self, name: &str) -> Option<[u8; 4]> {
        match self.get(name)? {
            Value::Color(v) => Some(*v),
            _ => None,
        }
    }
    pub fn linear_color(&self, name: &str) -> Option<[f32; 4]> {
        match self.get(name)? {
            Value::LinearColor(v) => Some(*v),
            _ => None,
        }
    }
    pub fn struct_props(&self, name: &str) -> Option<Props> {
        match self.get(name)? {
            Value::Struct(_, p) => Some(Props(p.clone())),
            _ => None,
        }
    }
    pub fn array(&self, name: &str) -> Option<(usize, usize, usize)> {
        match self.get(name)? {
            Value::Array { count, offset, size } => Some((*count, *offset, *size)),
            _ => None,
        }
    }
}

/// Immutable (natively serialized) structs and their binary sizes.
fn native_struct(pkg: &Package, name: &str, r: &mut Reader, size: usize) -> Result<Option<Value>> {
    let v = match (name, size) {
        ("Vector", 12) => Value::Vector(r.vec3()?),
        ("Rotator", 12) => Value::Rotator([r.i32()?, r.i32()?, r.i32()?]),
        ("Color", 4) => {
            let b = r.bytes(4)?;
            Value::Color([b[2], b[1], b[0], b[3]])
        }
        ("LinearColor", 16) => Value::LinearColor(r.vec4()?),
        ("Vector4" | "Plane" | "Quat", 16) => Value::Vec4(r.vec4()?),
        ("Vector2D", 8) => Value::Vec2([r.f32()?, r.f32()?]),
        ("Guid", 16) => Value::Guid(r.guid()?),
        ("Box", 25) => {
            let min = r.vec3()?;
            let max = r.vec3()?;
            r.u8()?;
            Value::Box { min, max }
        }
        ("Matrix", 64) => {
            // four planes (X, Y, Z axes, origin), each serialized W first (the struct `Plane`
            // extends `Vector`: its own field comes before the inherited ones); kept X Y Z W
            let mut m = [0f32; 16];
            for plane in m.chunks_exact_mut(4) {
                let w = r.f32()?;
                plane[0] = r.f32()?;
                plane[1] = r.f32()?;
                plane[2] = r.f32()?;
                plane[3] = w;
            }
            Value::Matrix(m)
        }
        ("IntPoint", 8) => Value::IntPoint([r.i32()?, r.i32()?]),
        _ => {
            let _ = pkg;
            return Ok(None);
        }
    };
    Ok(Some(v))
}

/// Parse a tagged property list until "None".
pub fn parse_props(pkg: &Package, r: &mut Reader) -> Result<Props> {
    let mut out = Vec::new();
    loop {
        let name = pkg.read_name(r)?;
        if name == "None" {
            break;
        }
        let ty = pkg.read_name(r)?;
        let size = r.i32()?;
        let index = r.i32()?;
        if size < 0 || size as usize > r.remaining() + 1 {
            bail!("bad property size {size} for {name}:{ty} at {}", r.pos);
        }
        let size = size as usize;
        let mut struct_name = String::new();
        let mut bool_val = false;
        match ty.as_str() {
            "StructProperty" => struct_name = pkg.read_name(r)?,
            "BoolProperty" => bool_val = r.u8()? != 0,
            "ByteProperty" => {
                let _enum_name = pkg.read_name(r)?;
            }
            _ => {}
        }
        let start = r.pos;
        let mut vr = Reader::at(r.data, start);
        let value = match ty.as_str() {
            "IntProperty" => Value::Int(vr.i32()?),
            "FloatProperty" => Value::Float(vr.f32()?),
            "BoolProperty" => Value::Bool(bool_val),
            "ByteProperty" => {
                if size == 1 {
                    Value::Byte(vr.u8()?)
                } else if size == 8 {
                    Value::Enum(pkg.read_name(&mut vr)?)
                } else {
                    Value::Raw { offset: start, size }
                }
            }
            "NameProperty" => Value::Name(pkg.read_name(&mut vr)?),
            "StrProperty" => Value::Str(vr.fstring()?),
            "ObjectProperty" | "ComponentProperty" | "ClassProperty" | "InterfaceProperty" => {
                Value::Object(vr.i32()?)
            }
            "DelegateProperty" => {
                let o = vr.i32()?;
                Value::Delegate(o, pkg.read_name(&mut vr)?)
            }
            "ArrayProperty" => {
                let count = vr.i32()?.max(0) as usize;
                Value::Array { count, offset: start + 4, size: size.saturating_sub(4) }
            }
            "StructProperty" => match native_struct(pkg, &struct_name, &mut vr, size)? {
                Some(v) => v,
                None => {
                    // Try nested tagged properties; fall back to raw.
                    let sub = &r.data[..start + size];
                    let mut sr = Reader::at(sub, start);
                    match parse_props(pkg, &mut sr) {
                        Ok(p) if sr.pos == start + size => Value::Struct(struct_name.clone(), p.0),
                        _ => Value::Raw { offset: start, size },
                    }
                }
            },
            _ => Value::Raw { offset: start, size },
        };
        r.pos = start + size;
        out.push(Prop { name, ty, index, value });
    }
    Ok(Props(out))
}

/// Parsed object header + properties; `data_pos` is where native data begins.
pub struct ObjectData<'a> {
    pub props: Props,
    pub reader: Reader<'a>,
}

static COMPONENT_CLASSES: std::sync::OnceLock<std::collections::HashSet<String>> = std::sync::OnceLock::new();

/// Install the set of class names deriving from `Component` (built from script packages).
pub fn set_component_classes(set: std::collections::HashSet<String>) {
    let _ = COMPONENT_CLASSES.set(set);
}

pub fn is_component_class(name: &str) -> bool {
    if let Some(set) = COMPONENT_CLASSES.get() {
        return set.contains(&name.to_ascii_lowercase());
    }
    name.ends_with("Component") || name.starts_with("Distribution")
}

/// Build the component class set from script packages: maps every UClass export to its
/// super class and keeps those that derive from `Component`.
pub fn component_classes_from(pkgs: &[&Package]) -> std::collections::HashSet<String> {
    let mut parent: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    for p in pkgs {
        for (i, e) in p.exports.iter().enumerate() {
            if e.class == 0 {
                let name = e.name.to_ascii_lowercase();
                let sup = p.obj_name(e.super_).to_ascii_lowercase();
                let _ = i;
                parent.entry(name).or_insert(sup);
            }
        }
    }
    let mut out = std::collections::HashSet::new();
    for k in parent.keys() {
        let mut cur = k.clone();
        let mut guard = 0;
        while guard < 64 {
            if cur == "component" {
                out.insert(k.clone());
                break;
            }
            match parent.get(&cur) {
                Some(p) if p != "none" => cur = p.clone(),
                _ => break,
            }
            guard += 1;
        }
    }
    out
}

/// Parse the standard UObject prefix (state frame, component template info, net index)
/// followed by tagged properties. Returns the reader positioned at native data.
pub fn read_object<'a>(pkg: &'a Package, idx: i32) -> Result<ObjectData<'a>> {
    if idx <= 0 || idx as usize > pkg.exports.len() {
        bail!("not an export: {idx}");
    }
    let e = &pkg.exports[idx as usize - 1];
    let start = e.offset;
    let end = e.offset + e.size;
    if end > pkg.data.len() {
        bail!("export {} out of range", e.name);
    }
    let data = &pkg.data[..end];
    let mut r = Reader::at(data, start);
    if e.flags & RF_HAS_STACK != 0 {
        let node = r.i32()?;
        let _state_node = r.i32()?;
        let _probe = r.u32()?;
        let _latent = r.u16()?;
        let stack = r.i32()?;
        if stack != 0 {
            // FPushedState { State, Node, Offset }
            for _ in 0..stack {
                r.i32()?;
                r.i32()?;
                r.i32()?;
            }
        }
        if node != 0 {
            let _offset = r.i32()?;
        }
    }
    let cls = pkg.class_name(idx);
    if cls.starts_with("Dominant") && cls.ends_with("LightComponent") && e.flags & RF_CLASS_DEFAULT == 0 {
        // DominantLightShadowMap (TArray<WORD>) precedes the object header.
        let n = r.count(2)?;
        r.skip(n * 2)?;
    }
    if is_component_class(&cls) && e.flags & RF_CLASS_DEFAULT == 0 {
        let _template_owner = r.i32()?;
        // Components that live inside a class default object carry their template name.
        let mut in_cdo = false;
        let mut cur = e.outer;
        let mut guard = 0;
        while cur > 0 && (cur as usize) <= pkg.exports.len() && guard < 32 {
            if pkg.exports[cur as usize - 1].flags & RF_CLASS_DEFAULT != 0 {
                in_cdo = true;
                break;
            }
            cur = pkg.exports[cur as usize - 1].outer;
            guard += 1;
        }
        if in_cdo {
            let _template_name = pkg.read_name(&mut r)?;
        }
    }
    let _net_index = r.i32()?;
    let props = parse_props(pkg, &mut r)?;
    Ok(ObjectData { props, reader: r })
}

/// Parse an array of tagged-property structs (`count` consecutive property lists).
pub fn parse_struct_array(pkg: &Package, offset: usize, size: usize, count: usize) -> Result<Vec<Props>> {
    let data = &pkg.data[..offset + size];
    let mut r = Reader::at(data, offset);
    let mut v = Vec::with_capacity(count);
    for _ in 0..count {
        v.push(parse_props(pkg, &mut r)?);
    }
    Ok(v)
}

/// Read an array property of object references.
pub fn object_array(pkg: &Package, props: &Props, name: &str) -> Vec<i32> {
    match props.array(name) {
        Some((count, offset, size)) if size >= count * 4 => {
            let mut r = Reader::at(&pkg.data, offset);
            (0..count).filter_map(|_| r.i32().ok()).collect()
        }
        _ => Vec::new(),
    }
}
