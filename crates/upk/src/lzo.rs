//! LZO1X decompressor (port of the reference `lzo1x_decompress_safe` algorithm).
//! Unreal Engine 3 on PC compresses package chunks and bulk data with LZO1X-1.

use anyhow::{bail, Result};

#[inline]
fn byte(src: &[u8], ip: usize) -> Result<usize> {
    match src.get(ip) {
        Some(b) => Ok(*b as usize),
        None => bail!("lzo: input overrun at {ip}"),
    }
}

fn copy_literals(src: &[u8], ip: &mut usize, dst: &mut Vec<u8>, n: usize) -> Result<()> {
    if *ip + n > src.len() {
        bail!("lzo: literal overrun ({} + {} > {})", ip, n, src.len());
    }
    dst.extend_from_slice(&src[*ip..*ip + n]);
    *ip += n;
    Ok(())
}

fn copy_match(dst: &mut Vec<u8>, dist: usize, n: usize) -> Result<()> {
    if dist == 0 || dist > dst.len() {
        bail!("lzo: lookbehind overrun (dist {dist}, have {})", dst.len());
    }
    let start = dst.len() - dist;
    if dist >= n {
        dst.extend_from_within(start..start + n);
    } else {
        for i in 0..n {
            let b = dst[start + i];
            dst.push(b);
        }
    }
    Ok(())
}

/// Reads an LZO "extended length" run: a sequence of zero bytes each worth 255,
/// terminated by a non-zero byte that is added to `base`.
fn ext_len(src: &[u8], ip: &mut usize, base: usize) -> Result<usize> {
    let mut t = 0usize;
    while byte(src, *ip)? == 0 {
        t += 255;
        *ip += 1;
    }
    t += base + byte(src, *ip)?;
    *ip += 1;
    Ok(t)
}

/// Decompress an LZO1X stream. `expected` is the uncompressed size hint.
pub fn decompress(src: &[u8], expected: usize) -> Result<Vec<u8>> {
    let mut dst: Vec<u8> = Vec::with_capacity(expected);
    let mut ip = 0usize;

    #[derive(Clone, Copy)]
    enum State {
        Loop,
        FirstLiteralRun,
        Match(usize),
        MatchNext(usize),
    }

    let mut state = State::Loop;
    if byte(src, 0)? > 17 {
        let t = byte(src, 0)? - 17;
        ip = 1;
        if t < 4 {
            state = State::MatchNext(t);
        } else {
            copy_literals(src, &mut ip, &mut dst, t)?;
            state = State::FirstLiteralRun;
        }
    }

    loop {
        match state {
            State::Loop => {
                let mut t = byte(src, ip)?;
                ip += 1;
                if t >= 16 {
                    state = State::Match(t);
                    continue;
                }
                if t == 0 {
                    t = ext_len(src, &mut ip, 15)?;
                }
                copy_literals(src, &mut ip, &mut dst, t + 3)?;
                state = State::FirstLiteralRun;
            }
            State::FirstLiteralRun => {
                let t = byte(src, ip)?;
                ip += 1;
                if t >= 16 {
                    state = State::Match(t);
                    continue;
                }
                // M1 match right after a literal run: 3 bytes, distance base 0x801
                let dist = 1 + 0x0800 + (t >> 2) + (byte(src, ip)? << 2);
                ip += 1;
                copy_match(&mut dst, dist, 3)?;
                let t = src[ip - 2] as usize & 3;
                state = if t == 0 { State::Loop } else { State::MatchNext(t) };
            }
            State::Match(mut t) => {
                let dist;
                if t >= 64 {
                    // M2
                    dist = 1 + ((t >> 2) & 7) + (byte(src, ip)? << 3);
                    ip += 1;
                    t = (t >> 5) - 1;
                } else if t >= 32 {
                    // M3
                    t &= 31;
                    if t == 0 {
                        t = ext_len(src, &mut ip, 31)?;
                    }
                    let b0 = byte(src, ip)?;
                    let b1 = byte(src, ip + 1)?;
                    dist = 1 + (b0 >> 2) + (b1 << 6);
                    ip += 2;
                } else if t >= 16 {
                    // M4
                    let mut d = (t & 8) << 11;
                    t &= 7;
                    if t == 0 {
                        t = ext_len(src, &mut ip, 7)?;
                    }
                    let b0 = byte(src, ip)?;
                    let b1 = byte(src, ip + 1)?;
                    d += (b0 >> 2) + (b1 << 6);
                    ip += 2;
                    if d == 0 {
                        // end of stream marker
                        return Ok(dst);
                    }
                    dist = d + 0x4000;
                } else {
                    // M1: 2-byte match after trailing literals
                    let dist = 1 + (t >> 2) + (byte(src, ip)? << 2);
                    ip += 1;
                    copy_match(&mut dst, dist, 2)?;
                    let t = src[ip - 2] as usize & 3;
                    state = if t == 0 { State::Loop } else { State::MatchNext(t) };
                    continue;
                }
                copy_match(&mut dst, dist, t + 2)?;
                let t = src[ip - 2] as usize & 3;
                state = if t == 0 { State::Loop } else { State::MatchNext(t) };
            }
            State::MatchNext(t) => {
                copy_literals(src, &mut ip, &mut dst, t)?;
                let t = byte(src, ip)?;
                ip += 1;
                state = State::Match(t);
            }
        }
    }
}
