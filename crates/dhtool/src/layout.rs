//! A script class's or struct's native layout, as UE3 links it (`UStruct::Link`): its
//! properties in declaration order (the `Children` / `Next` chain) after its super's, each at
//! the next offset its alignment allows, consecutive bools sharing a 32-bit word. With it the
//! exe's `[this + 0x2dc]` reads are the properties they are.

use std::collections::HashMap;

/// A property's place: offset, count, its class and (for a bool) its bit.
pub struct Field {
    pub name: String,
    pub kind: String,
    pub offset: u32,
    pub dim: u32,
    pub bit: Option<u32>,
}

pub struct Layouter {
    pkgs: HashMap<String, upk::Package>,
    /// (package, export) -> (size, alignment)
    sizes: HashMap<(String, i32), (u32, u32)>,
}

impl Layouter {
    pub fn new() -> Self {
        Layouter { pkgs: HashMap::new(), sizes: HashMap::new() }
    }

    fn pkg(&mut self, name: &str) -> Option<&upk::Package> {
        if !self.pkgs.contains_key(name) {
            let p = upk::Package::open(&crate::pkg_path(name)).ok()?;
            self.pkgs.insert(name.to_string(), p);
        }
        self.pkgs.get(name)
    }

    /// An object reference as (package, export).
    fn resolve(&mut self, pkg: &str, idx: i32) -> Option<(String, i32)> {
        if idx > 0 {
            return Some((pkg.to_string(), idx));
        }
        if idx == 0 {
            return None;
        }
        let path = self.pkg(pkg)?.obj_path(idx);
        let (p, rest) = path.split_once('.')?;
        let e = self.pkg(p)?.find_export(rest)?;
        Some((p.to_string(), e))
    }

    /// A field's `Next`, and the reader after it.
    fn next_of(p: &upk::Package, idx: i32) -> Option<(i32, upk::Reader<'_>)> {
        let od = upk::read_object(p, idx).ok()?;
        let mut r = od.reader;
        let next = r.i32().ok()?;
        Some((next, r))
    }

    /// The fields of a class or struct, in declaration order.
    fn chain(&mut self, pkg: &str, owner: i32) -> Vec<i32> {
        let Some(p) = self.pkg(pkg) else { return Vec::new() };
        let own: Vec<i32> = (1..=p.exports.len() as i32).filter(|&i| p.exports[i as usize - 1].outer == owner).collect();
        let nexts: HashMap<i32, i32> = own.iter().filter_map(|&i| Self::next_of(p, i).map(|(n, _)| (i, n))).collect();
        let pointed: std::collections::HashSet<i32> = nexts.values().copied().collect();
        let Some(&head) = own.iter().find(|i| !pointed.contains(i) && nexts.contains_key(i)) else { return Vec::new() };
        let mut out = vec![head];
        let mut cur = head;
        while let Some(&n) = nexts.get(&cur) {
            if n <= 0 || out.contains(&n) {
                break;
            }
            out.push(n);
            cur = n;
        }
        out
    }

    /// A class's or struct's properties' places, and its size and alignment.
    pub fn layout(&mut self, pkg: &str, idx: i32) -> (Vec<Field>, u32, u32) {
        let Some(p) = self.pkg(pkg) else { return (Vec::new(), 0, 1) };
        let sup = p.exports[idx as usize - 1].super_;
        let (mut cur, mut align) = match self.resolve(pkg, sup) {
            Some((sp, si)) => self.size_of(&sp, si),
            None => (0, 1),
        };
        let mut fields = Vec::new();
        let mut prev_bool: Option<(u32, u32)> = None;
        for f in self.chain(pkg, idx) {
            let p = self.pkg(pkg).unwrap();
            let kind = p.class_name(f);
            if !kind.ends_with("Property") {
                continue;
            }
            let name = p.obj_name(f).to_string();
            let Some((_, mut r)) = Self::next_of(p, f) else { continue };
            let dim = r.i32().unwrap_or(1).max(1) as u32;
            let flags = r.u64().unwrap_or(0);
            let _category = (r.i32().ok(), r.i32().ok());
            let _size_enum = r.i32().ok();
            if flags & 0x20 != 0 {
                let _rep = r.u16().ok();
            }
            let inner = r.i32().unwrap_or(0);
            if kind == "BoolProperty" {
                match prev_bool {
                    Some((off, bit)) if bit < 31 => {
                        prev_bool = Some((off, bit + 1));
                        fields.push(Field { name, kind, offset: off, dim, bit: Some(bit + 1) });
                    }
                    _ => {
                        let off = cur.next_multiple_of(4);
                        cur = off + 4;
                        align = align.max(4);
                        prev_bool = Some((off, 0));
                        fields.push(Field { name, kind, offset: off, dim, bit: Some(0) });
                    }
                }
                continue;
            }
            prev_bool = None;
            let (size, a) = match kind.as_str() {
                "ByteProperty" => (1, 1),
                "IntProperty" | "FloatProperty" | "ObjectProperty" | "ClassProperty" | "ComponentProperty" => (4, 4),
                "NameProperty" | "InterfaceProperty" => (8, 4),
                "StrProperty" | "ArrayProperty" | "DelegateProperty" => (12, 4),
                "MapProperty" => (60, 4),
                "StructProperty" => match self.resolve(pkg, inner) {
                    Some((sp, si)) => self.size_of(&sp, si),
                    None => (0, 4),
                },
                _ => (4, 4),
            };
            let off = cur.next_multiple_of(a);
            cur = off + size * dim;
            align = align.max(a);
            fields.push(Field { name, kind, offset: off, dim, bit: None });
        }
        (fields, cur.next_multiple_of(align), align)
    }

    fn size_of(&mut self, pkg: &str, idx: i32) -> (u32, u32) {
        if let Some(&s) = self.sizes.get(&(pkg.to_string(), idx)) {
            return s;
        }
        let (_, size, align) = self.layout(pkg, idx);
        self.sizes.insert((pkg.to_string(), idx), (size, align));
        (size, align)
    }
}
