//! Little-endian binary cursor used for all UE3 serialization.

use anyhow::{bail, Result};

#[derive(Clone)]
pub struct Reader<'a> {
    pub data: &'a [u8],
    pub pos: usize,
}

macro_rules! prim {
    ($name:ident, $t:ty, $n:expr) => {
        #[inline]
        pub fn $name(&mut self) -> Result<$t> {
            let b = self.bytes($n)?;
            Ok(<$t>::from_le_bytes(b.try_into().unwrap()))
        }
    };
}

impl<'a> Reader<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    pub fn at(data: &'a [u8], pos: usize) -> Self {
        Self { data, pos }
    }

    #[inline]
    pub fn remaining(&self) -> usize {
        self.data.len().saturating_sub(self.pos)
    }

    #[inline]
    pub fn eof(&self) -> bool {
        self.pos >= self.data.len()
    }

    #[inline]
    pub fn bytes(&mut self, n: usize) -> Result<&'a [u8]> {
        if self.pos + n > self.data.len() {
            bail!(
                "read overrun: want {} bytes at {} (len {})",
                n,
                self.pos,
                self.data.len()
            );
        }
        let s = &self.data[self.pos..self.pos + n];
        self.pos += n;
        Ok(s)
    }

    pub fn skip(&mut self, n: usize) -> Result<()> {
        self.bytes(n).map(|_| ())
    }

    prim!(u8, u8, 1);
    prim!(u16, u16, 2);
    prim!(i16, i16, 2);
    prim!(u32, u32, 4);
    prim!(i32, i32, 4);
    prim!(u64, u64, 8);
    prim!(i64, i64, 8);
    prim!(f32, f32, 4);

    pub fn bool32(&mut self) -> Result<bool> {
        Ok(self.u32()? != 0)
    }

    pub fn guid(&mut self) -> Result<[u32; 4]> {
        Ok([self.u32()?, self.u32()?, self.u32()?, self.u32()?])
    }

    pub fn vec3(&mut self) -> Result<[f32; 3]> {
        Ok([self.f32()?, self.f32()?, self.f32()?])
    }

    pub fn vec4(&mut self) -> Result<[f32; 4]> {
        Ok([self.f32()?, self.f32()?, self.f32()?, self.f32()?])
    }

    /// Array length with a sanity bound.
    pub fn count(&mut self, max_elem_size_hint: usize) -> Result<usize> {
        let n = self.i32()?;
        if n < 0 {
            bail!("negative array count {n} at {}", self.pos - 4);
        }
        let n = n as usize;
        if max_elem_size_hint > 0 && n.saturating_mul(max_elem_size_hint) > self.remaining() + 16 {
            bail!(
                "array count {n} (x{max_elem_size_hint}) too large at {} (remaining {})",
                self.pos - 4,
                self.remaining()
            );
        }
        Ok(n)
    }

    /// UE3 FString: i32 length (negative => UTF-16), includes terminating null.
    pub fn fstring(&mut self) -> Result<String> {
        let len = self.i32()?;
        if len == 0 {
            return Ok(String::new());
        }
        if len > 0 {
            if len > 1 << 20 {
                bail!("fstring too long: {len} at {}", self.pos - 4);
            }
            let b = self.bytes(len as usize)?;
            let b = &b[..b.len() - 1];
            Ok(b.iter().map(|&c| c as char).collect())
        } else {
            let n = (-len) as usize;
            if n > 1 << 20 {
                bail!("fstring too long: {len}");
            }
            let mut v = Vec::with_capacity(n);
            for _ in 0..n {
                v.push(self.u16()?);
            }
            v.pop();
            Ok(String::from_utf16_lossy(&v))
        }
    }

    pub fn array<T>(&mut self, elem: usize, mut f: impl FnMut(&mut Self) -> Result<T>) -> Result<Vec<T>> {
        let n = self.count(elem)?;
        let mut v = Vec::with_capacity(n);
        for _ in 0..n {
            v.push(f(self)?);
        }
        Ok(v)
    }

    /// Bulk-serialized array (UE3 TArray::BulkSerialize): i32 element size, i32 count, raw data.
    pub fn bulk_array(&mut self) -> Result<(usize, usize, &'a [u8])> {
        let elem = self.i32()?;
        let n = self.i32()?;
        if elem < 0 || n < 0 {
            bail!("bad bulk array header {elem} x {n} at {}", self.pos - 8);
        }
        let total = elem as usize * n as usize;
        let d = self.bytes(total)?;
        Ok((elem as usize, n as usize, d))
    }
}
