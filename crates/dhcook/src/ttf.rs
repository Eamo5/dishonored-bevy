//! A minimal TrueType writer: turns the games's Scaleform fonts (SWF `DefineFont3` glyph
//! outlines, quadratic like TrueType's) into `.ttf` files any text renderer can use.

use crate::swf::Font;

fn be16(o: &mut Vec<u8>, v: u16) {
    o.extend_from_slice(&v.to_be_bytes());
}
fn bei16(o: &mut Vec<u8>, v: i16) {
    o.extend_from_slice(&v.to_be_bytes());
}
fn be32(o: &mut Vec<u8>, v: u32) {
    o.extend_from_slice(&v.to_be_bytes());
}

fn checksum(d: &[u8]) -> u32 {
    let mut s = 0u32;
    for c in d.chunks(4) {
        let mut b = [0u8; 4];
        b[..c.len()].copy_from_slice(c);
        s = s.wrapping_add(u32::from_be_bytes(b));
    }
    s
}

fn clamp16(v: i32) -> i16 {
    v.clamp(i16::MIN as i32, i16::MAX as i32) as i16
}

/// Build a TrueType font (1024 units per em) from a converted Scaleform font.
pub fn build(font: &Font, family: &str) -> Vec<u8> {
    // glyph 0: .notdef (empty); then the font's glyphs, sorted by character code
    let mut glyphs: Vec<&crate::swf::Glyph> = font.glyphs.iter().filter(|g| g.code != 0).collect();
    glyphs.sort_by_key(|g| g.code);
    glyphs.dedup_by_key(|g| g.code);
    let n = glyphs.len() + 1;

    // glyf + loca
    let mut glyf = Vec::new();
    // .notdef is empty: it starts and ends at 0
    let mut loca: Vec<u32> = vec![0, 0];
    let mut bbox = [i32::MAX, i32::MAX, i32::MIN, i32::MIN];
    let (mut max_pts, mut max_ctr) = (0usize, 0usize);
    let mut metrics: Vec<(u16, i16)> = vec![(font.ascent.max(1) as u16 / 2, 0)];
    for g in &glyphs {
        let pts: Vec<&(i32, i32, bool)> = g.contours.iter().flatten().collect();
        let mut lsb = 0;
        if !pts.is_empty() {
            let (x0, y0) = (pts.iter().map(|p| p.0).min().unwrap(), pts.iter().map(|p| p.1).min().unwrap());
            let (x1, y1) = (pts.iter().map(|p| p.0).max().unwrap(), pts.iter().map(|p| p.1).max().unwrap());
            bbox = [bbox[0].min(x0), bbox[1].min(y0), bbox[2].max(x1), bbox[3].max(y1)];
            lsb = x0;
            bei16(&mut glyf, g.contours.len() as i16);
            for v in [x0, y0, x1, y1] {
                bei16(&mut glyf, clamp16(v));
            }
            let mut end = 0usize;
            for c in &g.contours {
                end += c.len();
                be16(&mut glyf, (end - 1) as u16);
            }
            be16(&mut glyf, 0); // instructions
            for p in &pts {
                glyf.push(if p.2 { 1 } else { 0 });
            }
            let (mut px, mut py) = (0, 0);
            for p in &pts {
                bei16(&mut glyf, clamp16(p.0 - px));
                px = p.0;
            }
            for p in &pts {
                bei16(&mut glyf, clamp16(p.1 - py));
                py = p.1;
            }
            while glyf.len() % 4 != 0 {
                glyf.push(0);
            }
            max_pts = max_pts.max(pts.len());
            max_ctr = max_ctr.max(g.contours.len());
        }
        loca.push(glyf.len() as u32);
        metrics.push((g.advance.max(0) as u16, clamp16(lsb)));
    }
    if bbox[0] == i32::MAX {
        bbox = [0, 0, 0, 0];
    }
    let ascent = font.ascent.max(bbox[3]);
    let descent = font.descent.max(-bbox[1]);
    let adv_max = metrics.iter().map(|m| m.0).max().unwrap_or(0);

    let mut tables: Vec<([u8; 4], Vec<u8>)> = Vec::new();

    let mut head = Vec::new();
    be32(&mut head, 0x0001_0000);
    be32(&mut head, 0x0001_0000);
    be32(&mut head, 0); // checksum adjustment, patched below
    be32(&mut head, 0x5F0F_3CF5);
    be16(&mut head, 0x000B);
    be16(&mut head, 1024);
    head.extend_from_slice(&[0u8; 16]); // created, modified
    for v in bbox {
        bei16(&mut head, clamp16(v));
    }
    be16(&mut head, (font.bold as u16) | ((font.italic as u16) << 1));
    be16(&mut head, 8);
    bei16(&mut head, 2);
    bei16(&mut head, 1); // long loca
    bei16(&mut head, 0);
    tables.push((*b"head", head));

    let mut hhea = Vec::new();
    be32(&mut hhea, 0x0001_0000);
    bei16(&mut hhea, clamp16(ascent));
    bei16(&mut hhea, clamp16(-descent));
    bei16(&mut hhea, clamp16(font.leading));
    be16(&mut hhea, adv_max);
    bei16(&mut hhea, clamp16(bbox[0]));
    bei16(&mut hhea, 0);
    bei16(&mut hhea, clamp16(bbox[2]));
    bei16(&mut hhea, 1);
    bei16(&mut hhea, 0);
    bei16(&mut hhea, 0);
    hhea.extend_from_slice(&[0u8; 8]);
    bei16(&mut hhea, 0);
    be16(&mut hhea, n as u16);
    tables.push((*b"hhea", hhea));

    let mut maxp = Vec::new();
    be32(&mut maxp, 0x0001_0000);
    be16(&mut maxp, n as u16);
    be16(&mut maxp, max_pts as u16);
    be16(&mut maxp, max_ctr as u16);
    be16(&mut maxp, 0);
    be16(&mut maxp, 0);
    be16(&mut maxp, 2);
    maxp.extend_from_slice(&[0u8; 18]);
    tables.push((*b"maxp", maxp));

    let first = glyphs.first().map(|g| g.code).unwrap_or(32);
    let last = glyphs.last().map(|g| g.code).unwrap_or(32);
    let mut os2 = Vec::new();
    be16(&mut os2, 4);
    bei16(&mut os2, clamp16(metrics.iter().map(|m| m.0 as i32).sum::<i32>() / n as i32));
    be16(&mut os2, if font.bold { 700 } else { 400 });
    be16(&mut os2, 5);
    be16(&mut os2, 0);
    for _ in 0..10 {
        bei16(&mut os2, 0); // sub/superscript, strikeout
    }
    bei16(&mut os2, 0); // family class
    os2.extend_from_slice(&[0u8; 10]); // panose
    os2.extend_from_slice(&[0u8; 16]); // unicode ranges
    os2.extend_from_slice(b"DHNR");
    be16(&mut os2, if font.bold { 0x20 } else { 0x40 } | if font.italic { 1 } else { 0 });
    be16(&mut os2, first);
    be16(&mut os2, last);
    bei16(&mut os2, clamp16(ascent));
    bei16(&mut os2, clamp16(-descent));
    bei16(&mut os2, clamp16(font.leading));
    be16(&mut os2, ascent.max(0) as u16);
    be16(&mut os2, descent.max(0) as u16);
    be32(&mut os2, 1);
    be32(&mut os2, 0);
    bei16(&mut os2, clamp16(ascent / 2));
    bei16(&mut os2, clamp16(ascent * 2 / 3));
    be16(&mut os2, 0);
    be16(&mut os2, 32);
    be16(&mut os2, 0);
    tables.push((*b"OS/2", os2));

    let mut hmtx = Vec::new();
    for (a, l) in &metrics {
        be16(&mut hmtx, *a);
        bei16(&mut hmtx, *l);
    }
    tables.push((*b"hmtx", hmtx));

    // cmap format 4: a segment per character
    let mut segs: Vec<(u16, u16, i16)> = glyphs.iter().enumerate().map(|(i, g)| (g.code, g.code, ((i as i32 + 1) - g.code as i32) as i16)).collect();
    segs.push((0xFFFF, 0xFFFF, 1));
    let segx2 = (segs.len() * 2) as u16;
    let mut sr = 2u16;
    let mut es = 0u16;
    while sr * 2 <= segx2 {
        sr *= 2;
        es += 1;
    }
    let mut sub = Vec::new();
    be16(&mut sub, 4);
    be16(&mut sub, (16 + segs.len() * 8) as u16);
    be16(&mut sub, 0);
    be16(&mut sub, segx2);
    be16(&mut sub, sr);
    be16(&mut sub, es);
    be16(&mut sub, segx2 - sr);
    for s in &segs {
        be16(&mut sub, s.1);
    }
    be16(&mut sub, 0);
    for s in &segs {
        be16(&mut sub, s.0);
    }
    for s in &segs {
        bei16(&mut sub, s.2);
    }
    for _ in &segs {
        be16(&mut sub, 0);
    }
    let mut cmap = Vec::new();
    be16(&mut cmap, 0);
    be16(&mut cmap, 1);
    be16(&mut cmap, 3);
    be16(&mut cmap, 1);
    be32(&mut cmap, 12);
    cmap.extend_from_slice(&sub);
    tables.push((*b"cmap", cmap));

    let mut loca_b = Vec::new();
    for o in &loca {
        be32(&mut loca_b, *o);
    }
    tables.push((*b"loca", loca_b));
    tables.push((*b"glyf", glyf));

    // names (Windows, Unicode BMP, en-US)
    let style = match (font.bold, font.italic) {
        (true, true) => "Bold Italic",
        (true, false) => "Bold",
        (false, true) => "Italic",
        _ => "Regular",
    };
    let ps: String = family.chars().filter(|c| c.is_ascii_alphanumeric()).collect();
    let names = [(1u16, family.to_string()), (2, style.to_string()), (4, format!("{family} {style}")), (6, ps)];
    let mut strings = Vec::new();
    let mut recs = Vec::new();
    for (id, s) in &names {
        let enc: Vec<u8> = s.encode_utf16().flat_map(|u| u.to_be_bytes()).collect();
        recs.push((*id, strings.len() as u16, enc.len() as u16));
        strings.extend_from_slice(&enc);
    }
    let mut name = Vec::new();
    be16(&mut name, 0);
    be16(&mut name, recs.len() as u16);
    be16(&mut name, (6 + recs.len() * 12) as u16);
    for (id, off, len) in recs {
        for v in [3, 1, 0x409, id, len, off] {
            be16(&mut name, v);
        }
    }
    name.extend_from_slice(&strings);
    tables.push((*b"name", name));

    let mut post = Vec::new();
    be32(&mut post, 0x0003_0000);
    be32(&mut post, 0);
    bei16(&mut post, -100);
    bei16(&mut post, 50);
    post.extend_from_slice(&[0u8; 20]);
    tables.push((*b"post", post));

    tables.sort_by(|a, b| a.0.cmp(&b.0));
    let nt = tables.len() as u16;
    let mut sr = 1u16;
    let mut es = 0u16;
    while sr * 2 <= nt {
        sr *= 2;
        es += 1;
    }
    let mut out = Vec::new();
    be32(&mut out, 0x0001_0000);
    be16(&mut out, nt);
    be16(&mut out, sr * 16);
    be16(&mut out, es);
    be16(&mut out, nt * 16 - sr * 16);
    let mut offset = 12 + 16 * tables.len();
    let mut head_off = 0;
    for (tag, data) in &tables {
        out.extend_from_slice(tag);
        be32(&mut out, checksum(data));
        be32(&mut out, offset as u32);
        be32(&mut out, data.len() as u32);
        if tag == b"head" {
            head_off = offset;
        }
        offset += data.len().div_ceil(4) * 4;
    }
    for (_, data) in &tables {
        out.extend_from_slice(data);
        while out.len() % 4 != 0 {
            out.push(0);
        }
    }
    let adj = 0xB1B0_AFBAu32.wrapping_sub(checksum(&out));
    out[head_off + 8..head_off + 12].copy_from_slice(&adj.to_be_bytes());
    out
}
