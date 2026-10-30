// SPDX-License-Identifier: GPL-3.0-or-later
//! UDF volume recognition and the logical volume identifier.
//!
//! Translated from libcdio as shipped with Rufus v4.15:
//! `src/libcdio/udf/udf_fs.c:udf_checktag()`, `udf_open()`,
//! `udf_get_root()`, `udf_get_logical_volume_id()` and `unicode16_decode()`
//! (GPL-3.0-or-later). Rufus tries UDF before ISO 9660 and uses the UDF
//! logical volume identifier as the image label when the UDF root directory
//! can be found (`src/iso.c:ExtractISO()`).
//!
//! Differences from upstream: the volume descriptor sequence scan is capped
//! at [`MAX_VDS_SECTORS`]; 8-bit identifiers are decoded as Latin-1 instead
//! of being copied as raw bytes; a zero identifier length yields an empty
//! label (upstream would overrun its buffer).

#![deny(clippy::indexing_slicing, clippy::arithmetic_side_effects)]

use super::io::{ByteSource, le_u16, le_u32, read_exact_at, slice};
use super::iso9660::decode_ucs2be;
use crate::Result;
use serde::Serialize;

/// UDF logical block size used by libcdio (`UDF_BLOCKSIZE`).
const UDF_BLOCKSIZE: u64 = 2048;
/// Sector of the anchor volume descriptor pointer.
const ANCHOR_SECTOR: u64 = 256;
/// Upper bound on the number of volume descriptor sequence sectors scanned.
pub const MAX_VDS_SECTORS: u32 = 256;

const TAGID_PRI_VOL: u16 = 0x0001;
const TAGID_ANCHOR: u16 = 0x0002;
const TAGID_PARTITION: u16 = 0x0005;
const TAGID_LOGVOL: u16 = 0x0006;
const TAGID_FSD: u16 = 0x0100;
const TAGID_FILE_ENTRY: u16 = 0x0105;

/// The UDF view of an image.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct UdfReport {
    /// Whether `udf_get_root()` found the root directory. Rufus only treats
    /// the image as UDF (and uses its label) when it does.
    pub root_found: bool,
    /// The logical volume identifier, when the root was found and it is not empty.
    pub logical_volume_id: Option<String>,
    /// Start of the partition, in blocks.
    pub partition_start: u32,
}

/// Looks for a UDF volume. Returns `None` when `udf_open()` would fail (no
/// valid anchor at sector 256, or no primary volume descriptor).
pub fn parse(source: &dyn ByteSource) -> Result<Option<UdfReport>> {
    let Some(anchor) = read_block(source, ANCHOR_SECTOR)? else {
        return Ok(None);
    };
    if !check_tag(&anchor, TAGID_ANCHOR) {
        return Ok(None);
    }
    let start = le_u32(&anchor, 20).unwrap_or(0);
    let len = le_u32(&anchor, 16).unwrap_or(0);
    // mvds_end = start + (len - 1) / UDF_BLOCKSIZE, in 32-bit arithmetic.
    // When this wraps below `start`, libcdio's root lookup scans nothing and
    // Rufus falls back to ISO 9660, which is what returning `None` achieves.
    let end = start.wrapping_add(len.wrapping_sub(1) / 2048);
    let end = end.min(start.saturating_add(MAX_VDS_SECTORS));
    if end <= start {
        return Ok(None);
    }

    // udf_open(): a primary volume descriptor must exist in the sequence.
    let mut has_pvd = false;
    for lba in start..end {
        let Some(block) = read_block(source, u64::from(lba))? else {
            return Ok(None);
        };
        if check_tag(&block, TAGID_PRI_VOL) {
            has_pvd = true;
            break;
        }
    }
    if !has_pvd {
        return Ok(None);
    }

    // udf_get_root(b_any_partition = true): find the partition and the
    // logical volume (2048-byte blocks), then the file set descriptor and the
    // root file entry.
    let mut partition_start = 0u32;
    let mut lvd: Option<(u32, Vec<u8>)> = None;
    for lba in start..end {
        let Some(block) = read_block(source, u64::from(lba))? else {
            return Ok(no_root(partition_start));
        };
        if check_tag(&block, TAGID_PARTITION) {
            partition_start = le_u32(&block, 188).unwrap_or(0);
            if lvd.is_some() {
                break;
            }
        } else if check_tag(&block, TAGID_LOGVOL) && le_u32(&block, 212) == Some(2048) {
            lvd = Some((lba, block));
            if partition_start != 0 {
                break;
            }
        }
    }
    let Some((_lvd_lba, lvd)) = lvd.filter(|_| partition_start != 0) else {
        return Ok(no_root(partition_start));
    };

    let fsd_offset = le_u32(&lvd, 252).unwrap_or(0);
    let Some(fsd) = read_block(
        source,
        u64::from(partition_start).saturating_add(u64::from(fsd_offset)),
    )?
    else {
        return Ok(no_root(partition_start));
    };
    if !check_tag(&fsd, TAGID_FSD) {
        return Ok(no_root(partition_start));
    }
    let root_icb = le_u32(&fsd, 404).unwrap_or(0);
    let Some(file_entry) = read_block(
        source,
        u64::from(partition_start).saturating_add(u64::from(root_icb)),
    )?
    else {
        return Ok(no_root(partition_start));
    };
    // Only a plain file entry is accepted, as in libcdio.
    if !check_tag(&file_entry, TAGID_FILE_ENTRY) {
        return Ok(no_root(partition_start));
    }

    let logical_volume_id = logical_volume_id(&lvd).filter(|label| !label.is_empty());
    Ok(Some(UdfReport {
        root_found: true,
        logical_volume_id,
        partition_start,
    }))
}

fn no_root(partition_start: u32) -> Option<UdfReport> {
    Some(UdfReport {
        root_found: false,
        logical_volume_id: None,
        partition_start,
    })
}

/// `udf_checktag()`: the tag identifier matches and the checksum equals the
/// sum of tag bytes 0 to 14 except byte 4 (libcdio does not include byte 15;
/// ECMA-167 does, which only matters for tag locations of 2^24 or more).
fn check_tag(block: &[u8], tag_id: u16) -> bool {
    let Some(tag) = slice(block, 0, 16) else {
        return false;
    };
    if le_u16(tag, 0) != Some(tag_id) {
        return false;
    }
    let sum = tag
        .iter()
        .take(15)
        .enumerate()
        .filter(|(i, _)| *i != 4)
        .fold(0u8, |sum, (_, &b)| sum.wrapping_add(b));
    tag.get(4) == Some(&sum)
}

/// `udf_get_logical_volume_id()`: decodes the 128-byte `logvol_id` dstring
/// at offset 84 using the length stored in its last byte.
fn logical_volume_id(lvd: &[u8]) -> Option<String> {
    let length = usize::from(*lvd.get(84 + 127)?);
    // `unicode16_decode(data, u_len)` reads u_len - 1 characters after the
    // compression id, possibly past the 128-byte field (still inside the block).
    let compression = *lvd.get(84)?;
    let count = length.checked_sub(1)?;
    let data = slice(lvd, 85, count)?;
    let text = match compression {
        8 => data
            .iter()
            .take_while(|&&b| b != 0)
            .map(|&b| char::from(b))
            .collect(),
        16 => decode_ucs2be(data),
        _ => String::new(),
    };
    Some(text)
}

fn read_block(source: &dyn ByteSource, lba: u64) -> Result<Option<Vec<u8>>> {
    let Some(offset) = lba.checked_mul(UDF_BLOCKSIZE) else {
        return Ok(None);
    };
    read_exact_at(source, offset, 2048)
}

#[cfg(test)]
#[allow(clippy::arithmetic_side_effects, clippy::cast_possible_truncation)] // Fixture arithmetic.
mod tests {
    use super::*;

    fn set_tag(block: &mut [u8], id: u16, location: u32) {
        block[0..2].copy_from_slice(&id.to_le_bytes());
        block[2..4].copy_from_slice(&2u16.to_le_bytes());
        block[12..16].copy_from_slice(&location.to_le_bytes());
        let sum = block[..15]
            .iter()
            .enumerate()
            .filter(|(i, _)| *i != 4)
            .fold(0u8, |s, (_, &b)| s.wrapping_add(b));
        block[4] = sum;
    }

    /// A UDF volume: anchor at 256, VDS at 32..48 (PVD, PD, LVD), partition
    /// starting at block 300, FSD at partition block 0, root FE at block 2.
    fn udf_image(label: &[u8], root_tag: u16) -> Vec<u8> {
        let mut image = vec![0u8; 2048 * 320];
        let block = |n: usize| n * 2048..(n + 1) * 2048;
        {
            let anchor = &mut image[block(256)];
            anchor[16..20].copy_from_slice(&(16 * 2048u32).to_le_bytes());
            anchor[20..24].copy_from_slice(&32u32.to_le_bytes());
            set_tag(anchor, TAGID_ANCHOR, 256);
        }
        set_tag(&mut image[block(32)], TAGID_PRI_VOL, 32);
        {
            let pd = &mut image[block(33)];
            pd[188..192].copy_from_slice(&300u32.to_le_bytes());
            set_tag(pd, TAGID_PARTITION, 33);
        }
        {
            let lvd = &mut image[block(34)];
            lvd[84..84 + label.len()].copy_from_slice(label);
            lvd[84 + 127] = u8::try_from(label.len()).unwrap();
            lvd[212..216].copy_from_slice(&2048u32.to_le_bytes());
            lvd[252..256].copy_from_slice(&0u32.to_le_bytes());
            set_tag(lvd, TAGID_LOGVOL, 34);
        }
        {
            let fsd = &mut image[block(300)];
            fsd[404..408].copy_from_slice(&2u32.to_le_bytes());
            set_tag(fsd, TAGID_FSD, 0);
        }
        set_tag(&mut image[block(302)], root_tag, 2);
        image
    }

    fn dstring8(text: &str) -> Vec<u8> {
        let mut field = vec![8u8];
        field.extend_from_slice(text.as_bytes());
        field
    }

    #[test]
    fn logical_volume_label() {
        let report = parse(&udf_image(
            &dstring8("CCCOMA_X64FRE_EN-US_DV9"),
            TAGID_FILE_ENTRY,
        ))
        .unwrap()
        .unwrap();
        assert!(report.root_found);
        assert_eq!(
            report.logical_volume_id.as_deref(),
            Some("CCCOMA_X64FRE_EN-US_DV9")
        );
        assert_eq!(report.partition_start, 300);
    }

    #[test]
    fn ucs2_label() {
        let mut field = vec![16u8];
        for unit in "Windöws".encode_utf16() {
            field.extend_from_slice(&unit.to_be_bytes());
        }
        let report = parse(&udf_image(&field, TAGID_FILE_ENTRY))
            .unwrap()
            .unwrap();
        assert_eq!(report.logical_volume_id.as_deref(), Some("Windöws"));
    }

    #[test]
    fn extended_file_entries_are_not_accepted() {
        // libcdio only accepts a plain file entry (tag 261) as the root.
        let report = parse(&udf_image(&dstring8("X"), 0x010a)).unwrap().unwrap();
        assert!(!report.root_found);
        assert_eq!(report.logical_volume_id, None);
    }

    #[test]
    fn bad_anchor_checksum() {
        let mut image = udf_image(&dstring8("X"), TAGID_FILE_ENTRY);
        image[256 * 2048 + 4] ^= 1;
        assert_eq!(parse(&image).unwrap(), None);
    }

    #[test]
    fn byte_15_is_not_part_of_the_checksum() {
        let mut image = udf_image(&dstring8("X"), TAGID_FILE_ENTRY);
        image[256 * 2048 + 15] = 0x7f;
        assert!(parse(&image).unwrap().is_some());
    }

    #[test]
    fn zero_length_identifier() {
        let mut image = udf_image(&dstring8("X"), TAGID_FILE_ENTRY);
        image[34 * 2048 + 84 + 127] = 0;
        let report = parse(&image).unwrap().unwrap();
        assert!(report.root_found);
        assert_eq!(report.logical_volume_id, None);
    }

    #[test]
    fn not_udf() {
        assert_eq!(parse(&vec![0u8; 600 * 1024]).unwrap(), None);
        assert_eq!(parse(&vec![0u8; 1024]).unwrap(), None);
    }

    #[test]
    fn wrapped_sequence_length() {
        let mut image = udf_image(&dstring8("X"), TAGID_FILE_ENTRY);
        let anchor = &mut image[256 * 2048..257 * 2048];
        anchor[16..20].copy_from_slice(&0u32.to_le_bytes());
        set_tag(anchor, TAGID_ANCHOR, 256);
        // len - 1 wraps to 0xFFFFFFFF; the scan is capped and still finds the volume.
        let report = parse(&image).unwrap().unwrap();
        assert!(report.root_found);
    }
}
