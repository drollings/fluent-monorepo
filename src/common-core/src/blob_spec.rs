//! Generic versioned blob framing: zero-copy, little-endian, 8-byte aligned
//! primitives validated via `rd_u32`/`slice` helpers. Callers own their magic
//! values and pass the allowlist to [`parse_header`]; no domain vocabulary
//! lives here.

#![forbid(unsafe_code)]

/// Header version (format of the header itself).
pub const HEADER_VERSION: u16 = 1;

/// Common header shape (per-artifact).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlobHeader {
    pub magic: u32,
    pub header_version: u16,
    pub section_version: u16,
    pub section_hash: u64,
    pub count: u32,
    pub section_off: u32,
    pub crc32: u32,
    pub sha256: [u8; 16],
}

impl BlobHeader {
    pub const LEN: usize = 44;
}

#[derive(Debug, thiserror::Error)]
pub enum BlobError {
    #[error("truncated header")]
    Truncated,
    #[error("bad magic {0:#010x}")]
    BadMagic(u32),
    #[error("unsupported header version {0}")]
    UnsupportedVersion(u16),
    #[error("crc mismatch")]
    CrcMismatch,
    #[error("sha mismatch")]
    ShaMismatch,
    #[error("out of range")]
    OutOfRange,
}

/// Read little-endian primitives (no unsafe, no mmap).
#[inline]
pub fn rd_u16(b: &[u8], o: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(o..o + 2)?.try_into().ok()?))
}
#[inline]
pub fn rd_u32(b: &[u8], o: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(o..o + 4)?.try_into().ok()?))
}
#[inline]
pub fn rd_u64(b: &[u8], o: usize) -> Option<u64> {
    Some(u64::from_le_bytes(b.get(o..o + 8)?.try_into().ok()?))
}
#[inline]
pub fn slice(b: &[u8], off: usize, len: usize) -> Option<&[u8]> {
    b.get(off..off.checked_add(len)?)
}

pub fn parse_header(data: &[u8], allowed_magics: &[u32]) -> Result<BlobHeader, BlobError> {
    if data.len() < BlobHeader::LEN {
        return Err(BlobError::Truncated);
    }
    let magic = rd_u32(data, 0).ok_or(BlobError::Truncated)?;
    if !allowed_magics.contains(&magic) {
        return Err(BlobError::BadMagic(magic));
    }
    let header_version = rd_u16(data, 4).ok_or(BlobError::Truncated)?;
    if header_version != HEADER_VERSION {
        return Err(BlobError::UnsupportedVersion(header_version));
    }
    let section_version = rd_u16(data, 6).ok_or(BlobError::Truncated)?;
    let section_hash = rd_u64(data, 8).ok_or(BlobError::Truncated)?;
    let count = rd_u32(data, 16).ok_or(BlobError::Truncated)?;
    let section_off = rd_u32(data, 20).ok_or(BlobError::Truncated)?;
    let crc32 = rd_u32(data, 24).ok_or(BlobError::Truncated)?;
    let mut sha256 = [0u8; 16];
    sha256.copy_from_slice(data.get(28..44).ok_or(BlobError::Truncated)?);
    Ok(BlobHeader {
        magic,
        header_version,
        section_version,
        section_hash,
        count,
        section_off,
        crc32,
        sha256,
    })
}

/// Validate `crc32` (IEEE) of payload slice.
pub fn validate_crc(payload: &[u8], expected: u32) -> Result<(), BlobError> {
    let mut hasher = crc32fast::Hasher::new();
    hasher.update(payload);
    if hasher.finalize() != expected {
        return Err(BlobError::CrcMismatch);
    }
    Ok(())
}

/// Validate first 16 bytes of sha256(payload) against header.
pub fn validate_sha(payload: &[u8], expected: &[u8; 16]) -> Result<(), BlobError> {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(payload);
    let out = h.finalize();
    if &out[..16] != expected {
        return Err(BlobError::ShaMismatch);
    }
    Ok(())
}

/// A zero-copy little-endian cursor over a versioned blob: the single shared spelling of the `rd_u16` / `rd_u32` /
/// `rd_usize` / `rd_str` / `slice` readers previously hand-rolled per blob
/// (`spacy-rs` lemma + ortho loaders).
///
/// Advancing reads (`u16_le`, `u32_le`, `usize_le`, `sized_str`, `take`) move
/// the cursor; absolute reads (`peek_u16`, `peek_u32`, `slice`) do not — the
/// two call shapes (directory loops vs. header/section fixups) coexist on one
/// type. Every read is bounds-checked and returns `None` past the end: callers
/// map `None` to their own error type (loud failure, never silent misparse).
/// No unsafe, no allocation, no I/O.
#[derive(Debug, Clone, Copy)]
pub struct BlobCursor<'a> {
    data: &'a [u8],
    offset: usize,
}

impl<'a> BlobCursor<'a> {
    /// A cursor at the start of `data`.
    #[must_use]
    pub fn new(data: &'a [u8]) -> Self {
        Self { data, offset: 0 }
    }

    /// A cursor at `offset` (clamped reads still fail, never wrap — an
    /// out-of-range start yields `None` on the first read).
    #[must_use]
    pub fn with_offset(data: &'a [u8], offset: usize) -> Self {
        Self { data, offset }
    }

    /// The current read position.
    #[must_use]
    pub fn offset(&self) -> usize {
        self.offset
    }

    /// The whole underlying buffer (for absolute section fixups).
    #[must_use]
    pub fn data(&self) -> &'a [u8] {
        self.data
    }

    /// Bytes remaining from the cursor.
    #[must_use]
    pub fn remaining(&self) -> usize {
        self.data.len().saturating_sub(self.offset)
    }

    /// Read a little-endian `u16` at an absolute offset (no cursor move).
    #[must_use]
    pub fn peek_u16(&self, off: usize) -> Option<u16> {
        rd_u16(self.data, off)
    }

    /// Read a little-endian `u32` at an absolute offset (no cursor move).
    #[must_use]
    pub fn peek_u32(&self, off: usize) -> Option<u32> {
        rd_u32(self.data, off)
    }

    /// Read an absolute sub-slice (no cursor move); `None` on overrun
    /// (overflow-safe via `checked_add` in [`slice`]).
    #[must_use]
    pub fn slice(&self, off: usize, len: usize) -> Option<&'a [u8]> {
        slice(self.data, off, len)
    }

    /// Consume the magic word at the cursor, checking it against `magic`.
    pub fn expect_magic(&mut self, magic: u32) -> Result<(), BlobError> {
        let got = self.u32_le().ok_or(BlobError::Truncated)?;
        if got != magic {
            return Err(BlobError::BadMagic(got));
        }
        Ok(())
    }

    /// Consume a little-endian `u16` at the cursor.
    #[must_use]
    pub fn u16_le(&mut self) -> Option<u16> {
        let v = rd_u16(self.data, self.offset)?;
        self.offset += 2;
        Some(v)
    }

    /// Consume a little-endian `u32` at the cursor.
    #[must_use]
    pub fn u32_le(&mut self) -> Option<u32> {
        let v = rd_u32(self.data, self.offset)?;
        self.offset += 4;
        Some(v)
    }

    /// Consume a `u32`-wide `usize` at the cursor (the `rd_usize` spelling:
    /// offsets/counts are 32-bit on the wire on every blob format here).
    #[must_use]
    pub fn usize_le(&mut self) -> Option<usize> {
        let v = self.u32_le()? as usize;
        Some(v)
    }

    /// Consume a length-prefixed string at the cursor: one length byte,
    /// then that many UTF-8 bytes (the `rd_str` spelling). `None` on
    /// overrun or invalid UTF-8.
    #[must_use]
    pub fn sized_str(&mut self) -> Option<&'a str> {
        let n = usize::from(*self.data.get(self.offset)?);
        self.offset += 1;
        let s = self.data.get(self.offset..self.offset + n)?;
        self.offset += n;
        std::str::from_utf8(s).ok()
    }

    /// Consume `len` raw bytes at the cursor.
    #[must_use]
    pub fn take(&mut self, len: usize) -> Option<&'a [u8]> {
        let s = self.data.get(self.offset..self.offset.checked_add(len)?)?;
        self.offset += len;
        Some(s)
    }
}
