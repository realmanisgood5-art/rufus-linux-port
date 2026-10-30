// SPDX-License-Identifier: GPL-3.0-or-later
//! Read-only access to the files of an ISO 9660 or UDF image.
//!
//! Translated from the libcdio code Rufus v4.15 uses to list and read ISO
//! contents (GPL-3.0-or-later): `src/libcdio/iso9660/iso9660_fs.c`
//! (`iso9660_ifs_readdir()`, `_iso9660_dir_to_statbuf()`,
//! `iso9660_check_dir_block_end()`, `_fs_stat_traverse()`,
//! `iso9660_ifs_stat_translate()`, the El Torito image table of
//! `iso9660_ifs_read_superblock()`), `src/libcdio/iso9660/rock.c`
//! (`get_rock_ridge_filename()`), `src/libcdio/iso9660/iso9660.c`
//! (`iso9660_name_translate_ext()`) and `src/libcdio/udf/udf_fs.c` /
//! `udf_file.c` (`udf_readdir()`, `udf_opendir()`, `udf_get_lba()`,
//! `offset_to_lba()`, `unicode16_decode()`), plus Rufus
//! `src/iso.c:ReadISOFileToBuffer()`.
//!
//! The same images are accepted and rejected as by Rufus, with these
//! differences, all limits against hostile images: a directory is at most
//! [`MAX_DIR_BYTES`]; continuation areas and records never read outside
//! their buffers (libcdio does not check); Rock Ridge child links (`CL`)
//! are followed to the directory they name instead of by a search of the
//! whole image; file reads are capped by the caller.

#![deny(clippy::indexing_slicing, clippy::arithmetic_side_effects)]

use super::io::{ByteSource, le_u16, le_u32, le_u64, read_exact_at, slice};
use super::iso9660::decode_ucs2be;
use crate::Result;

/// ISO 9660 and UDF block size.
pub const BLOCK: u64 = 2048;
/// Largest directory read, in bytes.
pub const MAX_DIR_BYTES: u64 = 64 * 1024 * 1024;
/// Largest file [`Volume::read_file`] reads (`ReadISOFileToBuffer()`'s limit).
pub const MAX_FILE_BYTES: u64 = 1024 * 1024 * 1024;
/// Rock Ridge continuation areas followed per record, as in libcdio.
const MAX_CE: u8 = 64;
/// El Torito images listed (`MAX_BOOT_IMAGES`).
const MAX_BOOT_IMAGES: usize = 8;
/// `eltorito_media_name[]`.
const MEDIA_NAMES: [&str; 5] = ["NoEmul", "1.2M", "1.44M", "2.88M", "HardDisk"];

const ISO_MULTIEXTENT: u8 = 0x80;
const ISO_DIRECTORY: u8 = 0x02;

// Rock Ridge system use fields (`ISO_ROCK_SUF_*`).
const SUF_SP: u32 = 0x001;
const SUF_ER: u32 = 0x002;
const SUF_CE: u32 = 0x004;
const SUF_PX: u32 = 0x008;
const SUF_PN: u32 = 0x010;
const SUF_SL: u32 = 0x020;
const SUF_NM: u32 = 0x040;
const SUF_TF: u32 = 0x080;
const SUF_CL: u32 = 0x100;
/// A Rock Ridge deep directory parent link.
pub const SUF_PL: u32 = 0x200;
const SUF_RE: u32 = 0x400;
const SUF_SF: u32 = 0x800;
const SUF_FORMAL: u32 =
    SUF_ER | SUF_PX | SUF_PN | SUF_SL | SUF_NM | SUF_CL | SUF_PL | SUF_RE | SUF_TF | SUF_SF;

/// The ISO 9660 extensions libcdio is allowed to use (`iso_extension_mask`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Extensions {
    /// Joliet (`ISO_EXTENSION_JOLIET`).
    pub joliet: bool,
    /// Rock Ridge (`ISO_EXTENSION_ROCK_RIDGE`).
    pub rock_ridge: bool,
}

impl Extensions {
    /// `ISO_EXTENSION_ALL`.
    pub const ALL: Self = Self {
        joliet: true,
        rock_ridge: true,
    };

    /// The mask `ExtractISO()` uses when scanning: Joliet is disabled when
    /// Rock Ridge is enabled (Alt-J and Alt-K toggle them in Rufus).
    pub fn for_scan(enable_joliet: bool, enable_rock_ridge: bool) -> Self {
        Self {
            joliet: enable_joliet && !enable_rock_ridge,
            rock_ridge: enable_rock_ridge,
        }
    }
}

/// A directory entry, as libcdio's `iso9660_stat_t` or `udf_dirent_t`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// The name libcdio reports: the Rock Ridge name if any, otherwise the
    /// Joliet name or the raw ISO 9660 identifier (with its `;1`), `.`,
    /// `..`, or the UDF name.
    pub name: String,
    /// Whether this is a directory.
    pub is_dir: bool,
    /// Size in bytes (all extents of a multi-extent file).
    pub size: u64,
    /// Whether Rock Ridge fields were found (`rr.b3_rock == yep`).
    pub rock: bool,
    /// Rock Ridge system use fields found (`rr.u_su_fields`).
    pub su_fields: u32,
    /// Symbolic link target (Rock Ridge `SL`), or `Some("")` for a UDF
    /// symbolic link (Rufus only checks the file type there).
    pub symlink: Option<String>,
    location: Location,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Location {
    /// First block of the data (ISO 9660).
    Iso { lsn: u32 },
    /// The virtual `[BOOT]` directory.
    BootDir,
    /// UDF: block of the file entry, relative to the partition, and
    /// whether the directory entry is the parent link.
    Udf { icb: u32, parent: bool },
}

impl Entry {
    /// Whether this is a Rock Ridge deep directory placeholder (`PL`).
    pub fn is_deep_dir_link(&self) -> bool {
        self.su_fields & SUF_PL != 0
    }
}

/// `iso9660_name_translate_ext()`: lower case (unless Joliet), drop a
/// trailing `;1` or `.;1`, turn other `;` into `.`.
pub fn translate_name(name: &str, joliet_level: u8) -> String {
    let bytes = name.as_bytes();
    let len = bytes.len();
    let mut out = Vec::with_capacity(len);
    for (i, &c) in bytes.iter().enumerate() {
        if c == 0 {
            break;
        }
        let c = if joliet_level == 0 {
            c.to_ascii_lowercase()
        } else {
            c
        };
        let rest = bytes.get(i.saturating_add(1)..).unwrap_or_default();
        if c == b'.' && rest == b";1" {
            break;
        }
        if c == b';' && rest == b"1" {
            break;
        }
        out.push(if c == b';' { b'.' } else { c });
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// An El Torito image of the virtual `[BOOT]` directory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct BootImage {
    media: u8,
    lsn: u32,
    sectors: u32,
}

/// An open ISO 9660 file system.
pub struct Iso<'a> {
    source: &'a dyn ByteSource,
    root: (u32, u64),
    joliet_level: u8,
    rock_ridge: bool,
    boot: Vec<BootImage>,
}

impl std::fmt::Debug for Iso<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Iso")
            .field("root", &self.root)
            .field("joliet_level", &self.joliet_level)
            .finish()
    }
}

fn read_blocks(source: &dyn ByteSource, lsn: u64, bytes: u64) -> Result<Option<Vec<u8>>> {
    let (Some(offset), Ok(len)) = (lsn.checked_mul(BLOCK), usize::try_from(bytes)) else {
        return Ok(None);
    };
    read_exact_at(source, offset, len)
}

/// Both-endian 32-bit value: libcdio uses the little-endian half.
fn both32(buf: &[u8], offset: usize) -> Option<u32> {
    le_u32(buf, offset)
}

impl<'a> Iso<'a> {
    /// `iso9660_open_ext()`: the PVD must be valid; the first Joliet SVD
    /// (allowed by `extensions`) provides the Joliet root.
    pub fn open(source: &'a dyn ByteSource, extensions: Extensions) -> Result<Option<Self>> {
        let Some(pvd) = read_blocks(source, 16, BLOCK)? else {
            return Ok(None);
        };
        if pvd.first() != Some(&1) || pvd.get(1..6) != Some(b"CD001".as_slice()) {
            return Ok(None);
        }
        let volume_space = both32(&pvd, 80).unwrap_or(0);
        let mut svd: Option<Vec<u8>> = None;
        let mut joliet_level = 0u8;
        let mut boot = Vec::new();
        for sector in 17u64..17 + 64 {
            let Some(vd) = read_blocks(source, sector, BLOCK)? else {
                break;
            };
            match vd.first().copied() {
                Some(255) => break,
                Some(0) if vd.get(7..30) == Some(b"EL TORITO SPECIFICATION".as_slice()) => {
                    boot = boot_images(source, le_u32(&vd, 0x47).unwrap_or(0), volume_space)?;
                }
                Some(2) => {
                    if joliet_level == 0 {
                        svd = Some(vd.clone());
                    }
                    if extensions.joliet && vd.get(88..90) == Some(b"%/".as_slice()) {
                        match vd.get(90) {
                            Some(0x40) => joliet_level = 1,
                            Some(0x43) => joliet_level = 2,
                            Some(0x45) => joliet_level = 3,
                            _ => {}
                        }
                    }
                }
                _ => {}
            }
        }
        let root_vd = match (&svd, joliet_level) {
            (Some(svd), level) if level > 0 => svd.as_slice(),
            _ => pvd.as_slice(),
        };
        let root = (
            both32(root_vd, 156 + 2).unwrap_or(0),
            u64::from(both32(root_vd, 156 + 10).unwrap_or(0)),
        );
        Ok(Some(Self {
            source,
            root,
            joliet_level,
            rock_ridge: extensions.rock_ridge,
            boot,
        }))
    }

    /// The Joliet level in use (0 when Joliet is not used).
    pub fn joliet_level(&self) -> u8 {
        self.joliet_level
    }

    /// `iso9660_ifs_readdir()` of the root directory.
    pub fn read_root(&self) -> Result<Option<Vec<Entry>>> {
        self.read_extent(self.root.0, self.root.1, true)
    }

    /// `iso9660_ifs_readdir()` of a directory entry. `None` is libcdio's
    /// NULL (an unreadable or malformed directory).
    pub fn read_dir(&self, dir: &Entry) -> Result<Option<Vec<Entry>>> {
        match dir.location {
            Location::BootDir => Ok(Some(self.boot_entries())),
            Location::Iso { lsn } if dir.is_dir => self.read_extent(lsn, dir.size, false),
            _ => Ok(None),
        }
    }

    fn boot_entries(&self) -> Vec<Entry> {
        self.boot
            .iter()
            .take_while(|image| image.lsn != 0)
            .enumerate()
            .map(|(index, image)| Entry {
                name: format!(
                    "{index}-Boot-{}.img",
                    MEDIA_NAMES
                        .get(usize::from(image.media))
                        .copied()
                        .unwrap_or("NoEmul")
                ),
                is_dir: false,
                size: u64::from(image.sectors).saturating_mul(512),
                rock: false,
                su_fields: 0,
                symlink: None,
                location: Location::Iso { lsn: image.lsn },
            })
            .collect()
    }

    fn read_extent(&self, lsn: u32, size: u64, is_root: bool) -> Result<Option<Vec<Entry>>> {
        let blocks = size.div_ceil(BLOCK);
        if blocks == 0 || size > MAX_DIR_BYTES {
            return Ok(None);
        }
        let Some(buf) = read_blocks(self.source, u64::from(lsn), blocks.saturating_mul(BLOCK))?
        else {
            return Ok(None);
        };
        let mut entries = Vec::new();
        // The virtual El Torito directory comes first in the root.
        if is_root && self.boot.first().is_some_and(|image| image.lsn != 0) {
            entries.push(Entry {
                name: "[BOOT]".to_string(),
                is_dir: true,
                size: 0,
                rock: false,
                su_fields: 0,
                symlink: None,
                location: Location::BootDir,
            });
        }
        let total = buf.len();
        let mut offset = 0usize;
        let mut pending: Option<Entry> = None;
        let mut skip_following = false;
        while offset < total {
            let rec_len = usize::from(buf.get(offset).copied().unwrap_or(0));
            // iso9660_check_dir_block_end()
            let block_rest = 2048usize.saturating_sub(offset % 2048);
            if rec_len == 0 || (offset % 2048).saturating_add(rec_len) > 2048 {
                offset = offset.saturating_add(block_rest);
                continue;
            }
            let Some(record) = buf.get(offset..offset.saturating_add(rec_len)) else {
                return Ok(None);
            };
            let flags = record.get(25).copied().unwrap_or(0);
            if skip_following {
                pending = None;
            } else {
                match self.dir_to_stat(record, pending.take())? {
                    None => skip_following = true,
                    Some(stat) if stat.su_fields & SUF_RE != 0 => {
                        // Relocated entry: ignored.
                        offset = offset.saturating_add(rec_len);
                        continue;
                    }
                    Some(stat) => pending = Some(stat),
                }
            }
            if flags & ISO_MULTIEXTENT == 0 {
                skip_following = false;
                if let Some(stat) = pending.take() {
                    entries.push(stat);
                }
            }
            offset = offset.saturating_add(rec_len);
        }
        Ok((offset == total).then_some(entries))
    }

    /// `_iso9660_dir_to_statbuf()`.
    fn dir_to_stat(&self, record: &[u8], previous: Option<Entry>) -> Result<Option<Entry>> {
        let name_len = usize::from(record.get(32).copied().unwrap_or(0));
        let raw_name = record
            .get(33..33usize.saturating_add(name_len))
            .unwrap_or_default();
        let flags = record.get(25).copied().unwrap_or(0);
        let extent = both32(record, 2).unwrap_or(0);
        let size = u64::from(both32(record, 10).unwrap_or(0));
        let first_extent = previous.is_none();
        let mut stat = previous.unwrap_or(Entry {
            name: String::new(),
            is_dir: false,
            size: 0,
            rock: false,
            su_fields: 0,
            symlink: None,
            location: Location::Iso { lsn: extent },
        });
        if !first_extent && stat.su_fields & SUF_RE != 0 {
            return Ok(None);
        }
        stat.is_dir = flags & ISO_DIRECTORY != 0;
        if stat.size > 0 {
            // A follow-up extent must be contiguous.
            let Location::Iso { lsn } = stat.location else {
                return Ok(None);
            };
            if u64::from(lsn).saturating_add(stat.size / BLOCK) != u64::from(extent)
                || !stat.size.is_multiple_of(BLOCK)
            {
                return Ok(None);
            }
        }
        stat.size = stat.size.saturating_add(size);
        let record_name = self.record_name(raw_name);
        if flags & ISO_MULTIEXTENT == 0 {
            if !first_extent && record_name.as_deref() != Some(stat.name.as_str()) {
                return Ok(None);
            }
            let rr_name = if self.rock_ridge {
                self.rock_ridge_name(record, &mut stat)?
            } else {
                None
            };
            match rr_name {
                Some(name) => stat.name = name,
                None if raw_name == [0] => stat.name = ".".to_string(),
                None if raw_name == [1] => stat.name = "..".to_string(),
                None => match record_name {
                    Some(name) => stat.name = name,
                    None => return Ok(None),
                },
            }
        } else {
            match record_name {
                Some(name) => stat.name = name,
                None => return Ok(None),
            }
        }
        if record.len() < 33 {
            return Ok(None);
        }
        Ok(Some(stat))
    }

    /// `_iso9660_recname_to_cstring()`: UCS-2BE for Joliet, raw bytes
    /// otherwise (up to the first NUL, as `strncpy` copies).
    fn record_name(&self, raw: &[u8]) -> Option<String> {
        if self.joliet_level > 0 {
            Some(decode_ucs2be(raw))
        } else {
            let end = raw.iter().position(|&b| b == 0).unwrap_or(raw.len());
            Some(String::from_utf8_lossy(raw.get(..end).unwrap_or_default()).into_owned())
        }
    }

    /// `get_rock_ridge_filename()`: returns the `NM` name, and records the
    /// fields found, the symbolic link and whether this is Rock Ridge.
    fn rock_ridge_name(&self, record: &[u8], stat: &mut Entry) -> Result<Option<String>> {
        let name_len = usize::from(record.get(32).copied().unwrap_or(0));
        let mut start = 33usize.saturating_add(name_len);
        if start % 2 == 1 {
            start = start.saturating_add(1);
        }
        let is_dot = name_len == 1 && matches!(record.get(33), Some(0 | 1));
        let mut area: Vec<u8> = record.get(start..).unwrap_or_default().to_vec();
        let mut name: Vec<u8> = Vec::new();
        let mut name_returned = 0usize;
        let mut truncate = false;
        let mut symlink: Vec<u8> = Vec::new();
        let mut ce_count = 0u8;
        loop {
            let mut continuation: Option<(u32, usize, usize)> = None;
            let mut pos = 0usize;
            while area.len().saturating_sub(pos) > 1 {
                let Some(entry) = area.get(pos..) else { break };
                let sig = entry.get(..2).unwrap_or_default();
                let len = usize::from(entry.get(2).copied().unwrap_or(0));
                if len == 0 {
                    return Ok(None);
                }
                let body = entry.get(..len.min(entry.len())).unwrap_or_default();
                pos = pos.saturating_add(len);
                match sig {
                    b"SP" => {
                        if body.get(4..6) != Some([0xbe, 0xef].as_slice()) {
                            return Ok(None);
                        }
                        stat.su_fields |= SUF_SP;
                    }
                    b"CE" if !is_dot => {
                        let extent = both32(body, 4).unwrap_or(0);
                        let offset = both32(body, 12).unwrap_or(0);
                        let size = both32(body, 20).unwrap_or(0);
                        if u64::from(offset) >= BLOCK || u64::from(size) >= BLOCK {
                            return Ok(None);
                        }
                        stat.su_fields |= SUF_CE;
                        continuation = Some((extent, offset as usize, size as usize));
                    }
                    b"NM" => {
                        stat.su_fields |= SUF_NM;
                        if truncate {
                            continue;
                        }
                        let nm_flags = body.get(4).copied().unwrap_or(0);
                        if nm_flags & 4 != 0 {
                            name.extend_from_slice(b"..");
                            name_returned = 3;
                            continue;
                        } else if nm_flags & 2 != 0 {
                            name.push(b'.');
                            name_returned = 2;
                            continue;
                        }
                        if nm_flags & !1 != 0 {
                            continue;
                        }
                        let text = body.get(5..).unwrap_or_default();
                        if name.len().saturating_add(len.saturating_sub(5)) >= 254 {
                            truncate = true;
                            continue;
                        }
                        // strncat stops at a NUL.
                        let end = text.iter().position(|&b| b == 0).unwrap_or(text.len());
                        name.extend_from_slice(text.get(..end).unwrap_or_default());
                        name_returned = name_returned.saturating_add(len.saturating_sub(5));
                    }
                    b"PX" => stat.su_fields |= SUF_PX,
                    b"SL" => {
                        stat.su_fields |= SUF_SL;
                        parse_symlink(body, &mut symlink);
                    }
                    b"TF" => stat.su_fields |= SUF_TF,
                    b"CL" => {
                        stat.su_fields |= SUF_CL;
                        // The relocated directory: its "." record gives the size.
                        let target = both32(body, 4).unwrap_or(0);
                        if let Some(dot) = read_blocks(self.source, u64::from(target), BLOCK)? {
                            stat.is_dir = true;
                            stat.size = u64::from(both32(&dot, 10).unwrap_or(0));
                            stat.location = Location::Iso { lsn: target };
                        }
                    }
                    b"PL" => stat.su_fields |= SUF_PL,
                    b"RE" => stat.su_fields |= SUF_RE,
                    b"SF" => stat.su_fields |= SUF_SF,
                    b"ER" => stat.su_fields |= SUF_ER,
                    b"PN" => stat.su_fields |= SUF_PN,
                    _ => {}
                }
            }
            let Some((extent, offset, size)) = continuation else {
                break;
            };
            ce_count = ce_count.saturating_add(1);
            if ce_count > MAX_CE {
                break;
            }
            let Some(block) = read_blocks(self.source, u64::from(extent), BLOCK)? else {
                return Ok(None);
            };
            area = block
                .get(offset..offset.saturating_add(size).min(block.len()))
                .unwrap_or_default()
                .to_vec();
        }
        if stat.su_fields & SUF_SL != 0 {
            stat.symlink = Some(String::from_utf8_lossy(&symlink).into_owned());
        }
        if stat.su_fields & SUF_FORMAL != 0 {
            stat.rock = true;
        }
        Ok((name_returned > 0).then(|| String::from_utf8_lossy(&name).into_owned()))
    }

    /// `iso9660_ifs_stat_translate()`: finds a file by path. Components
    /// match the reported name exactly or, without Joliet or Rock Ridge,
    /// its translation.
    pub fn stat(&self, path: &str) -> Result<Option<Entry>> {
        let trimmed = path.strip_prefix('/').unwrap_or(path);
        if self.boot.first().is_some_and(|image| image.lsn != 0)
            && let Some(rest) = strip_prefix_ignore_case(trimmed, "[BOOT]/")
        {
            return Ok(self
                .boot_entries()
                .into_iter()
                .find(|e| e.name.eq_ignore_ascii_case(rest)));
        }
        let mut current = Entry {
            name: String::new(),
            is_dir: true,
            size: self.root.1,
            rock: false,
            su_fields: 0,
            symlink: None,
            location: Location::Iso { lsn: self.root.0 },
        };
        for component in trimmed.split('/').filter(|c| !c.is_empty()) {
            if !current.is_dir {
                return Ok(None);
            }
            let Some(entries) = self.read_dir(&current)? else {
                return Ok(None);
            };
            let found = entries.into_iter().find(|entry| {
                entry.location != Location::BootDir
                    && (entry.name == component
                        || (self.joliet_level == 0
                            && !entry.rock
                            && translate_name(&entry.name, 0) == component))
            });
            match found {
                Some(entry) => current = entry,
                None => return Ok(None),
            }
        }
        Ok(Some(current))
    }

    /// Byte offset of an entry's data in the image.
    pub fn data_offset(&self, entry: &Entry) -> Option<u64> {
        match entry.location {
            Location::Iso { lsn } => u64::from(lsn).checked_mul(BLOCK),
            _ => None,
        }
    }

    /// Reads a file the way `ReadISOFileToBuffer()` does: its blocks are
    /// read as one contiguous run. `None` when it cannot be read.
    pub fn read(&self, entry: &Entry, max: u64) -> Result<Option<Vec<u8>>> {
        let Location::Iso { lsn } = entry.location else {
            return Ok(None);
        };
        if entry.is_dir || entry.size > max.min(MAX_FILE_BYTES) {
            return Ok(None);
        }
        let Some(mut data) = read_blocks(
            self.source,
            u64::from(lsn),
            entry.size.div_ceil(BLOCK).saturating_mul(BLOCK),
        )?
        else {
            return Ok(None);
        };
        data.truncate(usize::try_from(entry.size).unwrap_or(usize::MAX));
        Ok(Some(data))
    }
}

fn strip_prefix_ignore_case<'s>(text: &'s str, prefix: &str) -> Option<&'s str> {
    let head = text.get(..prefix.len())?;
    head.eq_ignore_ascii_case(prefix)
        .then(|| text.get(prefix.len()..))
        .flatten()
}

/// The `SL` component records of one entry, appended as libcdio does.
fn parse_symlink(body: &[u8], out: &mut Vec<u8>) {
    let entry_flags = body.get(4).copied().unwrap_or(0);
    let mut parts = body.get(5..).unwrap_or_default();
    let mut slen = parts.len();
    while slen > 1 {
        let flags = parts.first().copied().unwrap_or(0);
        let len = usize::from(parts.get(1).copied().unwrap_or(0));
        let text = parts.get(2..2usize.saturating_add(len)).unwrap_or_default();
        let mut root = false;
        match flags & !1 {
            0 => out.extend_from_slice(text),
            4 => out.extend_from_slice(b".."),
            2 => out.push(b'.'),
            8 => {
                root = true;
                out.push(b'/');
            }
            _ => {}
        }
        slen = slen.saturating_sub(len.saturating_add(2));
        parts = parts.get(len.saturating_add(2)..).unwrap_or_default();
        if slen < 2 {
            if entry_flags & 1 != 0 && flags & 1 == 0 {
                // libcdio leaves a gap here; a NUL ends the string.
                out.push(0);
            }
            break;
        }
        if !root && flags & 1 == 0 {
            out.push(b'/');
        }
    }
    if let Some(end) = out.iter().position(|&b| b == 0) {
        out.truncate(end);
    }
}

/// The El Torito image table of `iso9660_ifs_read_superblock()`, with the
/// size expansion for entries of 0 or 1 sectors.
fn boot_images(
    source: &dyn ByteSource,
    catalog_sector: u32,
    volume_space: u32,
) -> Result<Vec<BootImage>> {
    let mut images = Vec::new();
    let Some(catalog) = read_blocks(source, u64::from(catalog_sector), BLOCK)? else {
        return Ok(images);
    };
    for entry in catalog.as_chunks::<32>().0 {
        if images.len() >= MAX_BOOT_IMAGES {
            break;
        }
        let [id, media, ..] = *entry;
        if id == 0x88 && media & 0x0f <= 4 {
            images.push(BootImage {
                media: media & 0x0f,
                lsn: le_u32(entry, 8).unwrap_or(0),
                sectors: u32::from(le_u16(entry, 6).unwrap_or(0)),
            });
        }
    }
    let lsns: Vec<u32> = images.iter().map(|image| image.lsn).collect();
    for image in &mut images {
        if image.lsn == 0 {
            continue;
        }
        let next = lsns
            .iter()
            .copied()
            .filter(|&lsn| lsn > image.lsn && lsn < volume_space)
            .min()
            .unwrap_or(volume_space);
        let gap = next.saturating_sub(image.lsn);
        if image.sectors <= 1 && gap >= 0x1000 {
            image.sectors = gap.saturating_mul(4);
        }
    }
    Ok(images)
}

// UDF.

const TAG_FID: u16 = 257;
const TAG_FILE_ENTRY: u16 = 261;
const FILE_TYPE_DIRECTORY: u8 = 4;
const FILE_TYPE_SYMLINK: u8 = 12;

/// An open UDF file system (`udf_open()` and `udf_get_root()` succeeded).
pub struct Udf<'a> {
    source: &'a dyn ByteSource,
    partition_start: u32,
    root_icb: u32,
}

impl std::fmt::Debug for Udf<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Udf")
            .field("partition_start", &self.partition_start)
            .field("root_icb", &self.root_icb)
            .finish()
    }
}

/// `udf_checktag()`.
fn check_tag(block: &[u8], id: u16) -> bool {
    let Some(tag) = slice(block, 0, 16) else {
        return false;
    };
    if le_u16(tag, 0) != Some(id) {
        return false;
    }
    let sum = tag
        .iter()
        .take(15)
        .enumerate()
        .filter(|(i, _)| *i != 4)
        .fold(0u8, |s, (_, &b)| s.wrapping_add(b));
    tag.get(4) == Some(&sum)
}

impl<'a> Udf<'a> {
    /// Opens the UDF file system, if `udf_open()` and `udf_get_root()`
    /// would both succeed.
    pub fn open(source: &'a dyn ByteSource) -> Result<Option<Self>> {
        let Some(report) = super::udf::parse(source)? else {
            return Ok(None);
        };
        if !report.root_found {
            return Ok(None);
        }
        Ok(Some(Self {
            source,
            partition_start: report.partition_start,
            root_icb: report.root_icb,
        }))
    }

    fn block(&self, icb: u32) -> Result<Option<Vec<u8>>> {
        read_blocks(
            self.source,
            u64::from(self.partition_start).saturating_add(u64::from(icb)),
            BLOCK,
        )
    }

    /// `udf_readdir()` over the root directory.
    pub fn read_root(&self) -> Result<Option<Vec<Entry>>> {
        let Some(fe) = self.block(self.root_icb)? else {
            return Ok(None);
        };
        self.read_dir_fe(&fe).map(Some)
    }

    /// `udf_opendir()` then `udf_readdir()`. `None` when the directory
    /// cannot be opened (not a plain file entry, or not a directory).
    pub fn read_dir(&self, dir: &Entry) -> Result<Option<Vec<Entry>>> {
        let Location::Udf { icb, parent: false } = dir.location else {
            return Ok(None);
        };
        if !dir.is_dir {
            return Ok(None);
        }
        let Some(fe) = self.block(icb)? else {
            return Ok(None);
        };
        if !check_tag(&fe, TAG_FILE_ENTRY) || fe.get(27) != Some(&FILE_TYPE_DIRECTORY) {
            return Ok(None);
        }
        self.read_dir_fe(&fe).map(Some)
    }

    /// The directory data of a file entry: the blocks of its first
    /// allocation descriptor (`udf_get_lba()`), walked FID by FID.
    fn read_dir_fe(&self, fe: &[u8]) -> Result<Vec<Entry>> {
        let mut entries = Vec::new();
        let mut dir_left = i128::from(le_u64(fe, 56).unwrap_or(0));
        let (start, end) = first_extent_blocks(fe).unwrap_or((0, 0));
        let blocks = u64::from(end.saturating_sub(start)).saturating_add(1);
        if blocks.saturating_mul(BLOCK) > MAX_DIR_BYTES {
            return Ok(entries);
        }
        let Some(data) = read_blocks(
            self.source,
            u64::from(self.partition_start).saturating_add(u64::from(start)),
            blocks.saturating_mul(BLOCK),
        )?
        else {
            return Ok(entries);
        };
        let mut pos = 0usize;
        while dir_left > 0 {
            let Some(fid) = data.get(pos..) else { break };
            if fid.len() < 38 || !check_tag(fid, TAG_FID) {
                break;
            }
            let characteristics = fid.get(18).copied().unwrap_or(0);
            let id_len = usize::from(fid.get(19).copied().unwrap_or(0));
            let icb = le_u32(fid, 24).unwrap_or(0);
            let iu_len = usize::from(le_u16(fid, 36).unwrap_or(0));
            let step = 38usize
                .saturating_add(iu_len)
                .saturating_add(id_len)
                .next_multiple_of(4);
            dir_left = dir_left.saturating_sub(i128::try_from(step).unwrap_or(i128::MAX));
            let Some(entry_fe) = self.block(icb)? else {
                break;
            };
            let ident = fid
                .get(38usize.saturating_add(iu_len)..)
                .unwrap_or_default();
            let name = decode_udf_name(ident.get(..id_len.min(ident.len())).unwrap_or_default());
            let is_symlink = entry_fe.get(27) == Some(&FILE_TYPE_SYMLINK);
            entries.push(Entry {
                name,
                is_dir: characteristics & 0x02 != 0,
                size: le_u64(&entry_fe, 56).unwrap_or(0),
                rock: false,
                su_fields: 0,
                symlink: is_symlink.then(String::new),
                location: Location::Udf {
                    icb,
                    parent: characteristics & 0x08 != 0,
                },
            });
            pos = pos.saturating_add(step);
        }
        Ok(entries)
    }

    /// `udf_fopen()`: components are compared exactly.
    pub fn stat(&self, path: &str) -> Result<Option<Entry>> {
        let mut entries = match self.read_root()? {
            Some(entries) => entries,
            None => return Ok(None),
        };
        let components: Vec<&str> = path.split(['/', '\\']).filter(|c| !c.is_empty()).collect();
        for (index, component) in components.iter().enumerate() {
            let Some(entry) = entries.into_iter().find(|e| e.name == *component) else {
                return Ok(None);
            };
            if index.saturating_add(1) == components.len() {
                return Ok(Some(entry));
            }
            entries = match self.read_dir(&entry)? {
                Some(entries) => entries,
                None => return Ok(None),
            };
        }
        Ok(None)
    }

    /// `udf_read_block()` of the whole file in one call, as
    /// `ReadISOFileToBuffer()` does: only the first extent is read, and
    /// the read must return exactly the file length.
    pub fn read(&self, entry: &Entry, max: u64) -> Result<Option<Vec<u8>>> {
        let Location::Udf { icb, .. } = entry.location else {
            return Ok(None);
        };
        if entry.size > max.min(MAX_FILE_BYTES) {
            return Ok(None);
        }
        let Some(fe) = self.block(icb)? else {
            return Ok(None);
        };
        // Strategy type 4 only.
        if le_u16(&fe, 16 + 4) != Some(4) {
            return Ok(None);
        }
        let Some((lbn, extent_len)) = first_extent(&fe) else {
            return Ok(None);
        };
        let nblocks = entry.size.div_ceil(BLOCK);
        let count = nblocks.min(u64::from(extent_len).div_ceil(BLOCK));
        let read_len = u64::from(extent_len).min(count.saturating_mul(BLOCK));
        if read_len != entry.size {
            return Ok(None);
        }
        let Some(data) = read_blocks(
            self.source,
            u64::from(self.partition_start).saturating_add(u64::from(lbn)),
            read_len,
        )?
        else {
            return Ok(None);
        };
        Ok(Some(data))
    }
}

/// `unicode16_decode()`: compression 8 is bytes, 16 is UCS-2BE, anything
/// else is an empty name.
fn decode_udf_name(ident: &[u8]) -> String {
    match ident.split_first() {
        Some((8, rest)) => {
            let end = rest.iter().position(|&b| b == 0).unwrap_or(rest.len());
            String::from_utf8_lossy(rest.get(..end).unwrap_or_default()).into_owned()
        }
        Some((16, rest)) => decode_ucs2be(rest),
        _ => String::new(),
    }
}

/// Offset of the allocation descriptors of a file entry (as libcdio reads
/// every entry with the File Entry layout).
fn ad_offset(fe: &[u8]) -> Option<usize> {
    let ea_len = le_u32(fe, 168)?;
    176usize.checked_add(usize::try_from(ea_len).ok()?)
}

/// `udf_get_lba()`: first and last block of the first extent.
fn first_extent_blocks(fe: &[u8]) -> Option<(u32, u32)> {
    if le_u32(fe, 172)? == 0 {
        return None;
    }
    let at = ad_offset(fe)?;
    let (len, lba) = match le_u16(fe, 16 + 18)? & 7 {
        0 => (le_u32(fe, at)?, le_u32(fe, at.checked_add(4)?)?),
        1 => (le_u32(fe, at)?, le_u32(fe, at.checked_add(4)?)?),
        2 => (le_u32(fe, at)?, le_u32(fe, at.checked_add(12)?)?),
        _ => return None,
    };
    let len = len & 0x3FFF_FFFF;
    Some((lba, lba.wrapping_add(len.wrapping_sub(1) / 2048)))
}

/// `offset_to_lba()` at offset 0: block and raw length of the first short
/// or long allocation descriptor.
fn first_extent(fe: &[u8]) -> Option<(u32, u32)> {
    let at = ad_offset(fe)?;
    match le_u16(fe, 16 + 18)? & 7 {
        0 | 1 => Some((le_u32(fe, at.checked_add(4)?)?, le_u32(fe, at)?)),
        _ => None,
    }
}

/// An ISO image's file system, as Rufus opens it: UDF first, then ISO 9660.
#[derive(Debug)]
pub enum Volume<'a> {
    /// UDF.
    Udf(Udf<'a>),
    /// ISO 9660.
    Iso(Iso<'a>),
}

impl<'a> Volume<'a> {
    /// `ExtractISO()`'s choice: UDF when its root can be found, otherwise
    /// ISO 9660 with the given extensions.
    pub fn open(source: &'a dyn ByteSource, extensions: Extensions) -> Result<Option<Self>> {
        if let Some(udf) = Udf::open(source)? {
            return Ok(Some(Self::Udf(udf)));
        }
        Ok(Iso::open(source, extensions)?.map(Self::Iso))
    }

    /// Root directory entries.
    pub fn read_root(&self) -> Result<Option<Vec<Entry>>> {
        match self {
            Self::Udf(udf) => udf.read_root(),
            Self::Iso(iso) => iso.read_root(),
        }
    }

    /// Entries of a directory.
    pub fn read_dir(&self, dir: &Entry) -> Result<Option<Vec<Entry>>> {
        match self {
            Self::Udf(udf) => udf.read_dir(dir),
            Self::Iso(iso) => iso.read_dir(dir),
        }
    }

    /// `ReadISOFileToBuffer()`: UDF when it opens (a missing root or file
    /// then fails, without falling back), otherwise ISO 9660 with all
    /// extensions. At most `max` bytes.
    pub fn read_file(source: &'a dyn ByteSource, path: &str, max: u64) -> Result<Option<Vec<u8>>> {
        if super::udf::parse(source)?.is_some() {
            let Some(udf) = Udf::open(source)? else {
                return Ok(None);
            };
            let Some(entry) = udf.stat(path)? else {
                return Ok(None);
            };
            return udf.read(&entry, max);
        }
        let Some(iso) = Iso::open(source, Extensions::ALL)? else {
            return Ok(None);
        };
        let Some(entry) = iso.stat(path)? else {
            return Ok(None);
        };
        iso.read(&entry, max)
    }
}
