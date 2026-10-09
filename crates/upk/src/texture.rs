//! UTexture2D decoding with Texture File Cache (.tfc) support.

use crate::package::{decompress_chunk, Package, COMPRESS_LZO, COMPRESS_ZLIB};
use crate::props::{read_object, Props};
use crate::reader::Reader;
use anyhow::{bail, Context, Result};
use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

pub const BULK_SEPARATE_FILE: u32 = 0x01;
pub const BULK_ZLIB: u32 = 0x02;
pub const BULK_LZO: u32 = 0x10;
pub const BULK_UNUSED: u32 = 0x20;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PixelFormat {
    Dxt1,
    Dxt3,
    Dxt5,
    Bgra8,
    G8,
    V8U8,
    Bc5,
    Unknown,
}

impl PixelFormat {
    pub fn from_name(s: &str) -> PixelFormat {
        match s {
            "PF_DXT1" => PixelFormat::Dxt1,
            "PF_DXT3" => PixelFormat::Dxt3,
            "PF_DXT5" => PixelFormat::Dxt5,
            "PF_A8R8G8B8" => PixelFormat::Bgra8,
            "PF_G8" => PixelFormat::G8,
            "PF_V8U8" => PixelFormat::V8U8,
            "PF_BC5" | "PF_ATI2" => PixelFormat::Bc5,
            _ => PixelFormat::Unknown,
        }
    }

    /// Bytes for a mip of the given size.
    pub fn mip_size(self, w: u32, h: u32) -> usize {
        let bw = w.div_ceil(4).max(1) as usize;
        let bh = h.div_ceil(4).max(1) as usize;
        match self {
            PixelFormat::Dxt1 => bw * bh * 8,
            PixelFormat::Dxt3 | PixelFormat::Dxt5 | PixelFormat::Bc5 => bw * bh * 16,
            PixelFormat::Bgra8 => (w * h * 4) as usize,
            PixelFormat::G8 => (w * h) as usize,
            PixelFormat::V8U8 => (w * h * 2) as usize,
            PixelFormat::Unknown => 0,
        }
    }

    pub fn is_block(self) -> bool {
        matches!(self, PixelFormat::Dxt1 | PixelFormat::Dxt3 | PixelFormat::Dxt5 | PixelFormat::Bc5)
    }
}

#[derive(Debug, Clone)]
pub struct Mip {
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct TextureData {
    pub format: PixelFormat,
    pub format_name: String,
    /// Largest-first, only mips that had data.
    pub mips: Vec<Mip>,
    pub srgb: bool,
    pub address_x: String,
    pub address_y: String,
    pub props: Props,
}

/// Shared handle cache for .tfc files.
pub struct TfcCache {
    dir: PathBuf,
    files: Mutex<HashMap<String, Option<PathBuf>>>,
}

impl TfcCache {
    pub fn new(dir: &Path) -> Self {
        Self { dir: dir.to_path_buf(), files: Mutex::new(HashMap::new()) }
    }

    fn path(&self, name: &str) -> Option<PathBuf> {
        let mut m = self.files.lock().unwrap();
        m.entry(name.to_ascii_lowercase())
            .or_insert_with(|| {
                let p = self.dir.join(format!("{name}.tfc"));
                // (a DLC package's textures may be in the game's caches:
                // `DLC\PCConsole\DLCnn` -> `CookedPCConsole`)
                let base = self.dir.join("..").join("..").join("..").join("CookedPCConsole").join(format!("{name}.tfc"));
                if p.exists() {
                    Some(p)
                } else {
                    base.exists().then_some(base)
                }
            })
            .clone()
    }

    pub fn read(&self, name: &str, offset: u64, size: usize) -> Result<Vec<u8>> {
        let p = self.path(name).with_context(|| format!("missing tfc {name}"))?;
        let mut f = File::open(&p)?;
        f.seek(SeekFrom::Start(offset))?;
        let mut buf = vec![0u8; size];
        f.read_exact(&mut buf).with_context(|| format!("reading {size} bytes at {offset} from {}", p.display()))?;
        Ok(buf)
    }
}

fn decode_bulk(flags: u32, raw: &[u8]) -> Result<Vec<u8>> {
    if flags & BULK_LZO != 0 {
        decompress_chunk(raw, 0, COMPRESS_LZO)
    } else if flags & BULK_ZLIB != 0 {
        decompress_chunk(raw, 0, COMPRESS_ZLIB)
    } else {
        Ok(raw.to_vec())
    }
}

pub fn read_texture2d(pkg: &Package, idx: i32, tfc: &TfcCache, max_size: u32) -> Result<TextureData> {
    let od = read_object(pkg, idx)?;
    let props = od.props;
    let mut r: Reader = od.reader;
    let format_name = props.name("Format").unwrap_or("PF_DXT1").to_string();
    let format = PixelFormat::from_name(&format_name);
    let tfc_name = props.name("TextureFileCacheName").map(|s| s.to_string());
    // UTexture::SourceArt
    {
        let flags = r.u32()?;
        let _count = r.i32()?;
        let size = r.i32()?;
        let _off = r.i32()?;
        if flags & BULK_SEPARATE_FILE == 0 && size > 0 {
            r.skip(size as usize)?;
        }
    }
    let nmips = r.count(16)?;
    // First pass: record where each mip's payload lives without decoding it.
    enum Src<'a> {
        Inline(&'a [u8]),
        Tfc(u64, usize),
    }
    let mut entries: Vec<(u32, u32, u32, Src)> = Vec::new();
    for _ in 0..nmips {
        let flags = r.u32()?;
        let count = r.i32()?;
        let size = r.i32()?;
        let offset = r.i32()? as u32;
        let mut src = None;
        if flags & BULK_SEPARATE_FILE != 0 {
            if flags & BULK_UNUSED == 0 && size > 0 && count > 0 && tfc_name.is_some() {
                src = Some(Src::Tfc(offset as u64, size as usize));
            }
        } else if size > 0 {
            src = Some(Src::Inline(r.bytes(size as usize)?));
        }
        let w = r.i32()? as u32;
        let h = r.i32()? as u32;
        if let Some(src) = src {
            entries.push((w, h, flags, src));
        }
    }
    // Use mips within the size budget; if there are none, use the smallest one available.
    let mut chosen: Vec<&(u32, u32, u32, Src)> = entries.iter().filter(|e| e.0 <= max_size && e.1 <= max_size).collect();
    if chosen.is_empty() {
        if let Some(smallest) = entries.iter().min_by_key(|e| e.0 * e.1) {
            chosen.push(smallest);
        }
    }
    let mut mips = Vec::new();
    for (w, h, flags, src) in chosen {
        let raw = match src {
            Src::Inline(d) => d.to_vec(),
            Src::Tfc(off, size) => tfc.read(tfc_name.as_deref().unwrap(), *off, *size)?,
        };
        let data = decode_bulk(*flags, &raw).with_context(|| format!("mip {w}x{h} flags {flags:x}"))?;
        let need = format.mip_size(*w, *h);
        if need > 0 && data.len() < need {
            bail!("mip {w}x{h} too small: {} < {}", data.len(), need);
        }
        mips.push(Mip { width: *w, height: *h, data });
    }
    if mips.is_empty() {
        bail!("texture has no usable mips");
    }
    let srgb = props.bool("SRGB").unwrap_or(true);
    let address_x = props.name("AddressX").unwrap_or("TA_Wrap").to_string();
    let address_y = props.name("AddressY").unwrap_or("TA_Wrap").to_string();
    Ok(TextureData { format, format_name, mips, srgb, address_x, address_y, props })
}
