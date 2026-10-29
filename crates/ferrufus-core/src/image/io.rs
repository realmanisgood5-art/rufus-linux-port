// SPDX-License-Identifier: GPL-3.0-or-later
//! Bounded, position-based reads of untrusted images.
//!
//! Every parser reads through [`ByteSource`]: reads are absolute (no shared
//! cursor), short reads are retried until the requested range is complete,
//! and a single read can never exceed [`MAX_READ`]. A range that extends past
//! the end of the image is reported as `None`, never as a partial buffer.

#![deny(clippy::indexing_slicing, clippy::arithmetic_side_effects)]

use crate::{Error, Result};
use std::fs::File;
use std::io::{self, ErrorKind};
use std::os::unix::fs::FileExt;

/// Largest single read any parser may request.
pub const MAX_READ: usize = 1024 * 1024;

/// Random-access, read-only image data.
pub trait ByteSource {
    /// Total size in bytes.
    fn size(&self) -> u64;

    /// Reads up to `buf.len()` bytes at `offset`; returns the number read
    /// (0 at the end of the data).
    fn read_at(&self, buf: &mut [u8], offset: u64) -> io::Result<usize>;
}

impl ByteSource for [u8] {
    fn size(&self) -> u64 {
        u64::try_from(self.len()).unwrap_or(u64::MAX)
    }

    fn read_at(&self, buf: &mut [u8], offset: u64) -> io::Result<usize> {
        let Ok(start) = usize::try_from(offset) else {
            return Ok(0);
        };
        let Some(available) = self.get(start..) else {
            return Ok(0);
        };
        let count = available.len().min(buf.len());
        if let (Some(dst), Some(src)) = (buf.get_mut(..count), available.get(..count)) {
            dst.copy_from_slice(src);
        }
        Ok(count)
    }
}

impl ByteSource for Vec<u8> {
    fn size(&self) -> u64 {
        self.as_slice().size()
    }

    fn read_at(&self, buf: &mut [u8], offset: u64) -> io::Result<usize> {
        self.as_slice().read_at(buf, offset)
    }
}

/// A regular file and the size it had when it was opened.
#[derive(Debug)]
pub struct FileSource {
    file: File,
    size: u64,
}

impl FileSource {
    /// Wraps an open file.
    pub fn new(file: File, size: u64) -> Self {
        Self { file, size }
    }
}

impl ByteSource for FileSource {
    fn size(&self) -> u64 {
        self.size
    }

    fn read_at(&self, buf: &mut [u8], offset: u64) -> io::Result<usize> {
        self.file.read_at(buf, offset)
    }
}

/// Reads exactly `len` bytes at `offset`.
///
/// Returns `Ok(None)` if the range does not lie entirely within the source,
/// and an error if the read fails or `len` exceeds [`MAX_READ`].
pub fn read_exact_at(source: &dyn ByteSource, offset: u64, len: usize) -> Result<Option<Vec<u8>>> {
    if len > MAX_READ {
        return Err(Error::InvalidInput(format!(
            "refusing a {len}-byte read (limit {MAX_READ})"
        )));
    }
    let Some(end) = u64::try_from(len)
        .ok()
        .and_then(|len| offset.checked_add(len))
    else {
        return Ok(None);
    };
    if end > source.size() {
        return Ok(None);
    }
    let mut buf = vec![0u8; len];
    let mut filled = 0usize;
    while filled < len {
        let position = u64::try_from(filled)
            .ok()
            .and_then(|filled| offset.checked_add(filled));
        let (Some(position), Some(rest)) = (position, buf.get_mut(filled..)) else {
            return Ok(None);
        };
        match source.read_at(rest, position) {
            // The source is shorter than it claims (truncated while open).
            Ok(0) => return Ok(None),
            Ok(count) => filled = filled.saturating_add(count),
            Err(e) if e.kind() == ErrorKind::Interrupted => {}
            Err(e) => return Err(Error::io(format!("read error at offset {position}"), e)),
        }
    }
    Ok(Some(buf))
}

/// Little-endian `u16` at `offset` of `buf`.
pub fn le_u16(buf: &[u8], offset: usize) -> Option<u16> {
    bytes(buf, offset).map(u16::from_le_bytes)
}

/// Little-endian `u32` at `offset` of `buf`.
pub fn le_u32(buf: &[u8], offset: usize) -> Option<u32> {
    bytes(buf, offset).map(u32::from_le_bytes)
}

/// Little-endian `u64` at `offset` of `buf`.
pub fn le_u64(buf: &[u8], offset: usize) -> Option<u64> {
    bytes(buf, offset).map(u64::from_le_bytes)
}

/// Big-endian `u16` at `offset` of `buf`.
pub fn be_u16(buf: &[u8], offset: usize) -> Option<u16> {
    bytes(buf, offset).map(u16::from_be_bytes)
}

/// Big-endian `u32` at `offset` of `buf`.
pub fn be_u32(buf: &[u8], offset: usize) -> Option<u32> {
    bytes(buf, offset).map(u32::from_be_bytes)
}

/// Big-endian `u64` at `offset` of `buf`.
pub fn be_u64(buf: &[u8], offset: usize) -> Option<u64> {
    bytes(buf, offset).map(u64::from_be_bytes)
}

/// `N` bytes at `offset` of `buf`.
pub fn bytes<const N: usize>(buf: &[u8], offset: usize) -> Option<[u8; N]> {
    let end = offset.checked_add(N)?;
    buf.get(offset..end)?.try_into().ok()
}

/// `len` bytes at `offset` of `buf`.
pub fn slice(buf: &[u8], offset: usize, len: usize) -> Option<&[u8]> {
    buf.get(offset..offset.checked_add(len)?)
}

#[cfg(test)]
#[allow(clippy::arithmetic_side_effects, clippy::cast_possible_truncation)] // Fixture arithmetic.
mod tests {
    use super::*;

    struct ShortReads(Vec<u8>);

    impl ByteSource for ShortReads {
        fn size(&self) -> u64 {
            self.0.size()
        }

        fn read_at(&self, buf: &mut [u8], offset: u64) -> io::Result<usize> {
            // Return at most 3 bytes per call.
            let limit = buf.len().min(3);
            self.0.read_at(&mut buf[..limit], offset)
        }
    }

    #[test]
    fn short_reads_are_completed() {
        let source = ShortReads((0..=255).collect());
        let data = read_exact_at(&source, 10, 100).unwrap().unwrap();
        assert_eq!(data, (10..110).collect::<Vec<u8>>());
    }

    #[test]
    fn ranges_past_the_end_are_none() {
        let data: Vec<u8> = vec![1, 2, 3, 4];
        assert_eq!(read_exact_at(&data, 0, 4).unwrap(), Some(vec![1, 2, 3, 4]));
        assert_eq!(read_exact_at(&data, 1, 4).unwrap(), None);
        assert_eq!(read_exact_at(&data, u64::MAX, 1).unwrap(), None);
        assert_eq!(read_exact_at(&data, 4, 0).unwrap(), Some(vec![]));
    }

    #[test]
    fn oversized_reads_are_refused() {
        let data: Vec<u8> = vec![0; 16];
        assert!(read_exact_at(&data, 0, MAX_READ + 1).is_err());
    }

    #[test]
    fn integer_helpers_are_bounds_checked() {
        let buf = [0x01, 0x02, 0x03, 0x04];
        assert_eq!(le_u16(&buf, 0), Some(0x0201));
        assert_eq!(be_u16(&buf, 2), Some(0x0304));
        assert_eq!(le_u32(&buf, 0), Some(0x0403_0201));
        assert_eq!(le_u32(&buf, 1), None);
        assert_eq!(le_u64(&buf, 0), None);
        assert_eq!(le_u16(&buf, usize::MAX), None);
        assert_eq!(slice(&buf, 1, 2), Some(&buf[1..3]));
        assert_eq!(slice(&buf, 3, 2), None);
    }
}
