// SPDX-License-Identifier: GPL-3.0-or-later
//! Virtual disk containers: VHD, VHDX, Ventoy sparse images (VTSI) and FFU.
//!
//! Rufus mounts VHD and VHDX files through Windows to read their first
//! sector and size (`src/vhd.c:VhdMountImageAndGetSize()`). Ferrufus reads
//! the VHD format directly instead: the footer gives the virtual disk size,
//! and the first sector is found at offset 0 (fixed disks) or through the
//! block allocation table (dynamic disks). VHDX, VTSI and FFU files are only
//! recognized in this milestone.

#![deny(clippy::indexing_slicing, clippy::arithmetic_side_effects)]

use super::io::{ByteSource, be_u32, be_u64, le_u64, read_exact_at, slice};
use crate::Result;
use serde::Serialize;

/// VHD disk types.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum VhdDiskType {
    /// Fixed-size disk (type 2): the data is stored as-is before the footer.
    Fixed,
    /// Dynamically expanding disk (type 3).
    Dynamic,
    /// Differencing disk (type 4), which needs its parent.
    Differencing,
    /// Any other value.
    Other(u32),
}

/// A parsed VHD footer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct VhdReport {
    /// Disk type.
    pub disk_type: VhdDiskType,
    /// Virtual disk size in bytes.
    pub current_size: u64,
    /// Whether the footer checksum is correct.
    pub checksum_valid: bool,
    /// Offset of the dynamic disk header (dynamic and differencing disks).
    pub data_offset: u64,
    /// Creator application (four characters).
    pub creator_application: String,
}

/// Parses the footer in the last 512 bytes of a VHD.
pub fn parse_vhd(source: &dyn ByteSource) -> Result<Option<VhdReport>> {
    let Some(offset) = source.size().checked_sub(512) else {
        return Ok(None);
    };
    let Some(footer) = read_exact_at(source, offset, 512)? else {
        return Ok(None);
    };
    Ok(parse_footer(&footer))
}

fn parse_footer(footer: &[u8]) -> Option<VhdReport> {
    if slice(footer, 0, 8) != Some(b"conectix".as_slice()) {
        return None;
    }
    let stored = be_u32(footer, 64)?;
    let sum = footer
        .iter()
        .enumerate()
        .filter(|(i, _)| !(64..68).contains(i))
        .fold(0u32, |sum, (_, &b)| sum.wrapping_add(u32::from(b)));
    let disk_type = match be_u32(footer, 60)? {
        2 => VhdDiskType::Fixed,
        3 => VhdDiskType::Dynamic,
        4 => VhdDiskType::Differencing,
        other => VhdDiskType::Other(other),
    };
    Some(VhdReport {
        disk_type,
        current_size: be_u64(footer, 48)?,
        checksum_valid: !sum == stored,
        data_offset: be_u64(footer, 16)?,
        creator_application: slice(footer, 28, 4)?
            .iter()
            .map(|&b| char::from(b))
            .collect(),
    })
}

/// The first 512 bytes of the virtual disk, if they can be located.
pub fn first_sector(source: &dyn ByteSource, vhd: &VhdReport) -> Result<Option<Vec<u8>>> {
    match vhd.disk_type {
        VhdDiskType::Fixed => {
            if vhd.current_size < 512 {
                return Ok(None);
            }
            read_exact_at(source, 0, 512)
        }
        VhdDiskType::Dynamic => dynamic_first_sector(source, vhd.data_offset),
        VhdDiskType::Differencing | VhdDiskType::Other(_) => Ok(None),
    }
}

/// Sector 0 of a dynamic VHD: block 0 of the block allocation table, after
/// the block's sector bitmap. An unallocated block reads as zeros.
fn dynamic_first_sector(source: &dyn ByteSource, header_offset: u64) -> Result<Option<Vec<u8>>> {
    let Some(header) = read_exact_at(source, header_offset, 1024)? else {
        return Ok(None);
    };
    if slice(&header, 0, 8) != Some(b"cxsparse".as_slice()) {
        return Ok(None);
    }
    let (Some(table_offset), Some(entries), Some(block_size)) = (
        be_u64(&header, 16),
        be_u32(&header, 28),
        be_u32(&header, 32),
    ) else {
        return Ok(None);
    };
    if entries == 0 || block_size < 512 || block_size % 512 != 0 {
        return Ok(None);
    }
    let Some(entry) = read_exact_at(source, table_offset, 4)? else {
        return Ok(None);
    };
    let Some(block_sector) = be_u32(&entry, 0) else {
        return Ok(None);
    };
    if block_sector == u32::MAX {
        return Ok(Some(vec![0u8; 512]));
    }
    // One bitmap bit per 512-byte sector, padded to a whole sector.
    let bitmap_bytes = u64::from(block_size / 512)
        .div_ceil(8)
        .div_ceil(512)
        .saturating_mul(512);
    let Some(offset) = u64::from(block_sector)
        .checked_mul(512)
        .and_then(|start| start.checked_add(bitmap_bytes))
    else {
        return Ok(None);
    };
    read_exact_at(source, offset, 512)
}

/// Whether the file starts with the VHDX signature `vhdxfile`.
pub fn is_vhdx(first_bytes: &[u8]) -> bool {
    slice(first_bytes, 0, 8) == Some(b"vhdxfile".as_slice())
}

/// Whether the file has the FFU signature that Rufus checks for
/// (`SignedImage ` at offset 4, as written by dism).
pub fn is_ffu(first_bytes: &[u8]) -> bool {
    slice(first_bytes, 4, 12) == Some(b"SignedImage ".as_slice())
}

/// A Ventoy sparse image footer (`src/bled/decompress_vtsi.c`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct VtsiReport {
    /// Size of the disk the image expands to.
    pub disk_size: u64,
}

/// Parses the footer of a Ventoy sparse image (magic `VENTOY\0\0` in the
/// last 512 bytes).
pub fn parse_vtsi(source: &dyn ByteSource) -> Result<Option<VtsiReport>> {
    let Some(offset) = source.size().checked_sub(512) else {
        return Ok(None);
    };
    let Some(footer) = read_exact_at(source, offset, 512)? else {
        return Ok(None);
    };
    if slice(&footer, 0, 8) != Some(b"VENTOY\0\0".as_slice()) {
        return Ok(None);
    }
    Ok(le_u64(&footer, 12).map(|disk_size| VtsiReport { disk_size }))
}

#[cfg(test)]
#[allow(clippy::arithmetic_side_effects, clippy::cast_possible_truncation)] // Fixture arithmetic.
mod tests {
    use super::*;

    fn footer(disk_type: u32, size: u64, data_offset: u64) -> Vec<u8> {
        let mut footer = vec![0u8; 512];
        footer[..8].copy_from_slice(b"conectix");
        footer[12..16].copy_from_slice(&0x0001_0000u32.to_be_bytes());
        footer[16..24].copy_from_slice(&data_offset.to_be_bytes());
        footer[28..32].copy_from_slice(b"qemu");
        footer[40..48].copy_from_slice(&size.to_be_bytes());
        footer[48..56].copy_from_slice(&size.to_be_bytes());
        footer[60..64].copy_from_slice(&disk_type.to_be_bytes());
        let sum = footer
            .iter()
            .fold(0u32, |s, &b| s.wrapping_add(u32::from(b)));
        footer[64..68].copy_from_slice(&(!sum).to_be_bytes());
        footer
    }

    #[test]
    fn fixed_vhd() {
        let mut image = vec![0u8; 4096];
        image[0x1fe] = 0x55;
        image[0x1ff] = 0xaa;
        image.extend_from_slice(&footer(2, 4096, u64::MAX));
        let report = parse_vhd(&image).unwrap().unwrap();
        assert_eq!(report.disk_type, VhdDiskType::Fixed);
        assert_eq!(report.current_size, 4096);
        assert!(report.checksum_valid);
        assert_eq!(report.creator_application, "qemu");
        let sector = first_sector(&image, &report).unwrap().unwrap();
        assert_eq!(&sector[0x1fe..], &[0x55, 0xaa]);
    }

    #[test]
    fn bad_checksum_is_reported() {
        let mut image = vec![0u8; 1024];
        let mut f = footer(2, 1024, u64::MAX);
        f[100] = 1;
        image.extend_from_slice(&f);
        assert!(!parse_vhd(&image).unwrap().unwrap().checksum_valid);
    }

    fn dynamic_image(allocated: bool) -> Vec<u8> {
        // Footer copy at 0, dynamic header at 512, BAT at 1536, block 0 at
        // sector 4 (2 MB blocks: 4096 sectors, 512-byte bitmap).
        let mut image = vec![0u8; 512 * 4 + 512 + 512];
        image[..512].copy_from_slice(&footer(3, 64 * 1024 * 1024, 512));
        let header = &mut image[512..1536];
        header[..8].copy_from_slice(b"cxsparse");
        header[8..16].copy_from_slice(&u64::MAX.to_be_bytes());
        header[16..24].copy_from_slice(&1536u64.to_be_bytes());
        header[28..32].copy_from_slice(&32u32.to_be_bytes());
        header[32..36].copy_from_slice(&(2 * 1024 * 1024u32).to_be_bytes());
        let bat = if allocated { 4u32 } else { u32::MAX };
        image[1536..1540].copy_from_slice(&bat.to_be_bytes());
        // Sector 0 of the disk: after the 512-byte bitmap of block 0.
        let sector0 = 4 * 512 + 512;
        image[sector0 + 0x1fe] = 0x55;
        image[sector0 + 0x1ff] = 0xaa;
        image.extend_from_slice(&footer(3, 64 * 1024 * 1024, 512));
        image
    }

    #[test]
    fn dynamic_vhd_first_sector() {
        let image = dynamic_image(true);
        let report = parse_vhd(&image).unwrap().unwrap();
        assert_eq!(report.disk_type, VhdDiskType::Dynamic);
        assert_eq!(report.current_size, 64 * 1024 * 1024);
        let sector = first_sector(&image, &report).unwrap().unwrap();
        assert_eq!(&sector[0x1fe..], &[0x55, 0xaa]);
    }

    #[test]
    fn unallocated_dynamic_block_reads_as_zeros() {
        let image = dynamic_image(false);
        let report = parse_vhd(&image).unwrap().unwrap();
        assert_eq!(
            first_sector(&image, &report).unwrap().unwrap(),
            vec![0u8; 512]
        );
    }

    #[test]
    fn signatures() {
        assert!(is_vhdx(b"vhdxfile\0\0\0\0"));
        assert!(!is_vhdx(b"vhdx"));
        let mut ffu = vec![0u8; 32];
        ffu[4..16].copy_from_slice(b"SignedImage ");
        assert!(is_ffu(&ffu));
        assert!(!is_ffu(b"SignedImage "));
    }

    #[test]
    fn vtsi_footer() {
        let mut image = vec![0u8; 2048];
        let footer = &mut image[1536..];
        footer[..8].copy_from_slice(b"VENTOY\0\0");
        footer[12..20].copy_from_slice(&(8u64 << 30).to_le_bytes());
        assert_eq!(
            parse_vtsi(&image).unwrap(),
            Some(VtsiReport { disk_size: 8 << 30 })
        );
        assert_eq!(parse_vtsi(&vec![0u8; 100]).unwrap(), None);
    }
}
