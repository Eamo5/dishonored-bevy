//! UE3 package (.upk) container: summary, decompression, name/import/export tables.

use crate::lzo;
use crate::reader::Reader;
use anyhow::{bail, Context, Result};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

pub const PACKAGE_TAG: u32 = 0x9E2A83C1;

pub const COMPRESS_ZLIB: u32 = 1;
pub const COMPRESS_LZO: u32 = 2;

#[derive(Debug, Clone)]
pub struct Import {
    pub class_package: String,
    pub class_name: String,
    pub outer: i32,
    pub name: String,
}

#[derive(Debug, Clone)]
pub struct Export {
    pub class: i32,
    pub super_: i32,
    pub outer: i32,
    pub name: String,
    pub archetype: i32,
    pub flags: u64,
    pub size: usize,
    pub offset: usize,
    pub export_flags: u32,
}

#[derive(Debug, Clone, Copy)]
pub struct Chunk {
    pub uncompressed_offset: u32,
    pub uncompressed_size: u32,
    pub compressed_offset: u32,
    pub compressed_size: u32,
}

pub struct Package {
    pub name: String,
    pub path: PathBuf,
    pub version: u16,
    pub licensee: u16,
    pub package_flags: u32,
    pub names: Vec<String>,
    pub imports: Vec<Import>,
    pub exports: Vec<Export>,
    /// Fully decompressed package image (offsets in the tables index into this).
    pub data: Vec<u8>,
    export_by_path: HashMap<String, usize>,
}

/// Decompress a UE3 compressed-chunk stream (FCompressedChunkHeader + blocks)
/// located at `pos` in `src`. Used for both package chunks and compressed bulk data.
pub fn decompress_chunk(src: &[u8], pos: usize, flags: u32) -> Result<Vec<u8>> {
    let mut r = Reader::at(src, pos);
    let tag = r.u32()?;
    if tag != PACKAGE_TAG {
        bail!("bad compressed chunk tag {tag:08x} at {pos}");
    }
    let block_size = r.i32()?;
    let _sum_c = r.i32()?;
    let sum_u = r.i32()? as usize;
    let block_size = if block_size == PACKAGE_TAG as i32 { 0x20000 } else { block_size as usize };
    let nblocks = sum_u.div_ceil(block_size.max(1));
    let mut blocks = Vec::with_capacity(nblocks);
    for _ in 0..nblocks {
        let c = r.i32()? as usize;
        let u = r.i32()? as usize;
        blocks.push((c, u));
    }
    let mut out = Vec::with_capacity(sum_u);
    for (c, u) in blocks {
        let comp = r.bytes(c)?;
        match flags {
            COMPRESS_LZO => {
                let d = lzo::decompress(comp, u).with_context(|| format!("lzo block c={c} u={u}"))?;
                if d.len() != u {
                    bail!("lzo block size mismatch: got {} expected {}", d.len(), u);
                }
                out.extend_from_slice(&d);
            }
            COMPRESS_ZLIB => {
                use std::io::Read;
                let mut d = Vec::with_capacity(u);
                flate2::read::ZlibDecoder::new(comp).read_to_end(&mut d)?;
                out.extend_from_slice(&d);
            }
            other => bail!("unsupported compression flags {other}"),
        }
    }
    Ok(out)
}

impl Package {
    pub fn open(path: &Path) -> Result<Package> {
        let file = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
        let name = path.file_stem().unwrap().to_string_lossy().to_string();
        Self::from_bytes(name, path.to_path_buf(), file)
    }

    pub fn from_bytes(name: String, path: PathBuf, file: Vec<u8>) -> Result<Package> {
        let mut r = Reader::new(&file);
        let tag = r.u32()?;
        if tag != PACKAGE_TAG {
            bail!("{}: not a UE3 package (tag {tag:08x})", path.display());
        }
        let version = r.u16()?;
        let licensee = r.u16()?;
        let _headers_size = r.i32()?;
        let _group = r.fstring()?;
        let package_flags = r.u32()?;
        let name_count = r.i32()? as usize;
        let name_offset = r.i32()? as usize;
        let export_count = r.i32()? as usize;
        let export_offset = r.i32()? as usize;
        let import_count = r.i32()? as usize;
        let import_offset = r.i32()? as usize;
        let _depends_offset = r.i32()?;
        if version >= 623 {
            let _ie_guids_off = r.i32()?;
            let _import_guids = r.i32()?;
            let _export_guids = r.i32()?;
        }
        if version >= 584 {
            let _thumb = r.i32()?;
        }
        let _guid = r.guid()?;
        let gens = r.count(12)?;
        r.skip(gens * 12)?;
        let _engine_ver = r.i32()?;
        let _cooker_ver = r.i32()?;
        let compression = r.u32()?;
        let nchunks = r.count(16)?;
        let mut chunks = Vec::with_capacity(nchunks);
        for _ in 0..nchunks {
            chunks.push(Chunk {
                uncompressed_offset: r.u32()?,
                uncompressed_size: r.u32()?,
                compressed_offset: r.u32()?,
                compressed_size: r.u32()?,
            });
        }

        let data = if chunks.is_empty() {
            file
        } else {
            let first = chunks.iter().map(|c| c.uncompressed_offset).min().unwrap() as usize;
            let total = chunks
                .iter()
                .map(|c| (c.uncompressed_offset + c.uncompressed_size) as usize)
                .max()
                .unwrap();
            let mut data = vec![0u8; total];
            let hdr = first.min(file.len());
            data[..hdr].copy_from_slice(&file[..hdr]);
            for c in &chunks {
                let d = decompress_chunk(&file, c.compressed_offset as usize, compression)
                    .with_context(|| format!("{}: chunk at {}", path.display(), c.compressed_offset))?;
                let start = c.uncompressed_offset as usize;
                let n = d.len().min(c.uncompressed_size as usize);
                data[start..start + n].copy_from_slice(&d[..n]);
            }
            data
        };

        // Name table
        let mut r = Reader::at(&data, name_offset);
        let mut names = Vec::with_capacity(name_count);
        for _ in 0..name_count {
            let s = r.fstring()?;
            let _flags = r.u64()?;
            names.push(s);
        }

        let fname = |r: &mut Reader, names: &Vec<String>| -> Result<String> {
            let idx = r.i32()?;
            let num = r.i32()?;
            let base = names
                .get(idx as usize)
                .with_context(|| format!("bad name index {idx}"))?;
            Ok(if num > 0 { format!("{}_{}", base, num - 1) } else { base.clone() })
        };

        let mut r = Reader::at(&data, import_offset);
        let mut imports = Vec::with_capacity(import_count);
        for _ in 0..import_count {
            let class_package = fname(&mut r, &names)?;
            let class_name = fname(&mut r, &names)?;
            let outer = r.i32()?;
            let name = fname(&mut r, &names)?;
            imports.push(Import { class_package, class_name, outer, name });
        }

        let mut r = Reader::at(&data, export_offset);
        let mut exports = Vec::with_capacity(export_count);
        for _ in 0..export_count {
            let class = r.i32()?;
            let super_ = r.i32()?;
            let outer = r.i32()?;
            let name = fname(&mut r, &names)?;
            let archetype = r.i32()?;
            let flags = r.u64()?;
            let size = r.i32()? as usize;
            let offset = r.i32()? as usize;
            let export_flags = r.u32()?;
            let net_count = r.count(4)?;
            r.skip(net_count * 4)?;
            let _guid = r.guid()?;
            let _pkg_flags = r.u32()?;
            exports.push(Export { class, super_, outer, name, archetype, flags, size, offset, export_flags });
        }

        let mut pkg = Package {
            name,
            path,
            version,
            licensee,
            package_flags,
            names,
            imports,
            exports,
            data,
            export_by_path: HashMap::new(),
        };
        let mut map = HashMap::with_capacity(pkg.exports.len());
        for i in 0..pkg.exports.len() {
            map.insert(pkg.obj_path(i as i32 + 1).to_ascii_lowercase(), i);
        }
        pkg.export_by_path = map;
        Ok(pkg)
    }

    pub fn name_of(&self, idx: i32, num: i32) -> String {
        let base = self.names.get(idx as usize).cloned().unwrap_or_else(|| format!("<badname {idx}>"));
        if num > 0 {
            format!("{}_{}", base, num - 1)
        } else {
            base
        }
    }

    /// Read an FName from a reader positioned in this package's data.
    pub fn read_name(&self, r: &mut Reader) -> Result<String> {
        let idx = r.i32()?;
        let num = r.i32()?;
        if idx < 0 || idx as usize >= self.names.len() {
            bail!("bad name index {idx} at {}", r.pos - 8);
        }
        Ok(self.name_of(idx, num))
    }

    pub fn obj_name(&self, idx: i32) -> &str {
        if idx > 0 {
            self.exports.get(idx as usize - 1).map(|e| e.name.as_str()).unwrap_or("<bad export>")
        } else if idx < 0 {
            self.imports.get((-idx - 1) as usize).map(|i| i.name.as_str()).unwrap_or("<bad import>")
        } else {
            "None"
        }
    }

    pub fn obj_outer(&self, idx: i32) -> i32 {
        if idx > 0 {
            self.exports.get(idx as usize - 1).map(|e| e.outer).unwrap_or(0)
        } else if idx < 0 {
            self.imports.get((-idx - 1) as usize).map(|i| i.outer).unwrap_or(0)
        } else {
            0
        }
    }

    /// Dotted path of an object, excluding the containing package file itself
    /// for exports (e.g. "TheWorld.PersistentLevel.StaticMeshActor_3").
    /// For imports, the first component is the source package.
    pub fn obj_path(&self, idx: i32) -> String {
        let mut parts = Vec::new();
        let mut cur = idx;
        let mut guard = 0;
        while cur != 0 && guard < 64 {
            parts.push(self.obj_name(cur).to_string());
            cur = self.obj_outer(cur);
            guard += 1;
        }
        parts.reverse();
        parts.join(".")
    }

    pub fn class_name(&self, idx: i32) -> String {
        if idx > 0 {
            let Some(e) = self.exports.get(idx as usize - 1) else { return "None".into() };
            if e.class == 0 {
                "Class".to_string()
            } else {
                self.obj_name(e.class).to_string()
            }
        } else if idx < 0 {
            self.imports.get((-idx - 1) as usize).map(|i| i.class_name.clone()).unwrap_or_else(|| "None".into())
        } else {
            "None".into()
        }
    }

    pub fn export_data(&self, idx: i32) -> Result<&[u8]> {
        let e = self.exports.get(idx as usize - 1).context("bad export index")?;
        if e.offset + e.size > self.data.len() {
            bail!("export {} out of range", e.name);
        }
        Ok(&self.data[e.offset..e.offset + e.size])
    }

    pub fn find_export(&self, path: &str) -> Option<i32> {
        self.export_by_path.get(&path.to_ascii_lowercase()).map(|i| *i as i32 + 1)
    }

    pub fn exports_of_class<'a>(&'a self, class: &'a str) -> impl Iterator<Item = i32> + 'a {
        (1..=self.exports.len() as i32).filter(move |&i| self.class_name(i).eq_ignore_ascii_case(class))
    }
}
