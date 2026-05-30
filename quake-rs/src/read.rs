//! A bounds-checked, little-endian byte cursor.
//!
//! Quake stores everything little-endian on disk; the C engine sprinkled
//! `LittleLong` / `LittleShort` calls to byte-swap on big-endian hosts. Here
//! every multi-byte read goes through `*::from_le_bytes`, so the loaders are
//! endian-correct on any platform, and out-of-range reads return a
//! [`QError::Truncated`] instead of reading past the buffer.

use crate::error::{QError, Result};

/// A sequential reader over a borrowed byte slice.
#[derive(Clone, Debug)]
pub struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    /// Create a reader positioned at the start of `buf`.
    pub fn new(buf: &'a [u8]) -> Self {
        Reader { buf, pos: 0 }
    }

    /// Create a reader positioned at `pos` within `buf`.
    pub fn at(buf: &'a [u8], pos: usize) -> Self {
        Reader { buf, pos }
    }

    /// Current cursor position.
    pub fn pos(&self) -> usize {
        self.pos
    }

    /// Total length of the underlying buffer.
    pub fn len(&self) -> usize {
        self.buf.len()
    }

    /// Whether the underlying buffer is empty.
    pub fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }

    /// Bytes remaining from the cursor to the end of the buffer.
    pub fn remaining(&self) -> usize {
        self.buf.len().saturating_sub(self.pos)
    }

    /// The whole underlying buffer.
    pub fn whole(&self) -> &'a [u8] {
        self.buf
    }

    /// Move the cursor to an absolute position. Errors if past the end.
    pub fn seek(&mut self, pos: usize) -> Result<()> {
        if pos > self.buf.len() {
            return Err(QError::Truncated {
                context: "seek",
                need: pos,
                have: self.buf.len(),
            });
        }
        self.pos = pos;
        Ok(())
    }

    /// Advance the cursor by `n` bytes.
    pub fn skip(&mut self, n: usize) -> Result<()> {
        let target = self
            .pos
            .checked_add(n)
            .ok_or_else(|| QError::invalid("skip overflow"))?;
        self.seek(target)
    }

    /// Borrow the next `n` bytes and advance the cursor.
    pub fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        if self.remaining() < n {
            return Err(QError::Truncated {
                context: "take",
                need: n,
                have: self.remaining(),
            });
        }
        let s = &self.buf[self.pos..self.pos + n];
        self.pos += n;
        Ok(s)
    }

    /// Borrow `n` bytes starting at absolute `off` without moving the cursor.
    pub fn slice_at(&self, off: usize, n: usize) -> Result<&'a [u8]> {
        let end = off
            .checked_add(n)
            .ok_or_else(|| QError::invalid("slice overflow"))?;
        if end > self.buf.len() {
            return Err(QError::Truncated {
                context: "slice_at",
                need: end,
                have: self.buf.len(),
            });
        }
        Ok(&self.buf[off..end])
    }

    /// Read a fixed-size byte array.
    pub fn bytes<const N: usize>(&mut self) -> Result<[u8; N]> {
        let s = self.take(N)?;
        let mut out = [0u8; N];
        out.copy_from_slice(s);
        Ok(out)
    }

    pub fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }
    pub fn i8(&mut self) -> Result<i8> {
        Ok(self.take(1)?[0] as i8)
    }
    pub fn u16(&mut self) -> Result<u16> {
        Ok(u16::from_le_bytes(self.bytes::<2>()?))
    }
    pub fn i16(&mut self) -> Result<i16> {
        Ok(i16::from_le_bytes(self.bytes::<2>()?))
    }
    pub fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_le_bytes(self.bytes::<4>()?))
    }
    pub fn i32(&mut self) -> Result<i32> {
        Ok(i32::from_le_bytes(self.bytes::<4>()?))
    }
    pub fn f32(&mut self) -> Result<f32> {
        Ok(f32::from_le_bytes(self.bytes::<4>()?))
    }

    /// Read three little-endian `f32`s as a `[f32; 3]` (Quake's `vec3_t`).
    pub fn vec3(&mut self) -> Result<[f32; 3]> {
        Ok([self.f32()?, self.f32()?, self.f32()?])
    }

    /// Read an `n`-byte, NUL-padded fixed name field, trimming at the first
    /// NUL. Quake stores names this way (`char name[16]`, `char name[56]`).
    /// Non-UTF-8 bytes are replaced lossily.
    pub fn name(&mut self, n: usize) -> Result<String> {
        let s = self.take(n)?;
        let end = s.iter().position(|&b| b == 0).unwrap_or(s.len());
        Ok(String::from_utf8_lossy(&s[..end]).into_owned())
    }
}

/// Read a little-endian `i32` at absolute offset `off` in `buf`.
pub fn i32_at(buf: &[u8], off: usize) -> Result<i32> {
    Reader::at(buf, off).i32()
}

/// Read a little-endian `u32` at absolute offset `off` in `buf`.
pub fn u32_at(buf: &[u8], off: usize) -> Result<u32> {
    Reader::at(buf, off).u32()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_little_endian_scalars() {
        let buf = [0x01, 0x00, 0x00, 0x00, 0xff, 0xff, 0xff, 0xff];
        let mut r = Reader::new(&buf);
        assert_eq!(r.i32().unwrap(), 1);
        assert_eq!(r.i32().unwrap(), -1);
        assert_eq!(r.remaining(), 0);
    }

    #[test]
    fn truncation_is_an_error() {
        let buf = [0x00, 0x01];
        let mut r = Reader::new(&buf);
        assert!(r.i32().is_err());
    }

    #[test]
    fn name_trims_at_nul() {
        let buf = b"progs/\0\0\0\0";
        let mut r = Reader::new(buf);
        assert_eq!(r.name(8).unwrap(), "progs/");
    }

    #[test]
    fn vec3_roundtrip() {
        let mut buf = Vec::new();
        for v in [1.5f32, -2.0, 3.25] {
            buf.extend_from_slice(&v.to_le_bytes());
        }
        let got = Reader::new(&buf).vec3().unwrap();
        assert_eq!(got, [1.5, -2.0, 3.25]);
    }
}
