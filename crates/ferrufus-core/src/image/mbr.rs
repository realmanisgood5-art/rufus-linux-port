// SPDX-License-Identifier: GPL-3.0-or-later
//! The DOS/MBR partition table of an image.
//!
//! Rufus only checks the boot marker and boot code of an image
//! (`src/drive.c:AnalyzeMBR()`); Ferrufus also reports the partition table so
//! that a dry-run plan can show what writing the image will create.

#![deny(clippy::indexing_slicing, clippy::arithmetic_side_effects)]

use super::io::{le_u32, slice};
use super::ms_sys::{self, MbrIdentification};
use serde::Serialize;

/// Size of the MBR sector Rufus analyzes (`MBR_SIZE`).
pub const MBR_SIZE: usize = 512;

/// The parsed first sector of an image.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MbrReport {
    /// Boot marker and boot code, as `AnalyzeMBR()` reports them.
    pub identification: MbrIdentification,
    /// The Windows disk signature at offset 0x1B8.
    pub disk_signature: u32,
    /// Non-empty primary partition entries (only when the boot marker is present).
    pub partitions: Vec<MbrPartition>,
}

/// One primary partition entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MbrPartition {
    /// Entry number, 1 to 4.
    pub number: u8,
    /// Whether the active (bootable) flag is set.
    pub active: bool,
    /// Partition type byte.
    pub type_id: u8,
    /// A name for the type, when known.
    pub type_name: Option<&'static str>,
    /// First sector (512-byte units, as recorded in the entry).
    pub start_lba: u32,
    /// Number of sectors.
    pub sectors: u32,
}

impl MbrPartition {
    /// End of the partition in bytes, assuming 512-byte sectors.
    pub fn end_bytes(&self) -> u64 {
        (u64::from(self.start_lba).saturating_add(u64::from(self.sectors))).saturating_mul(512)
    }
}

impl MbrReport {
    /// Parses the first sector of an image. `sector` may be shorter than 512
    /// bytes, in which case there is no boot marker.
    pub fn parse(sector: &[u8]) -> Self {
        let identification = ms_sys::identify(sector);
        let disk_signature = le_u32(sector, 0x1b8).unwrap_or(0);
        let mut partitions = Vec::new();
        if identification.has_boot_marker() {
            let entries = slice(sector, 0x1be, 64).unwrap_or_default();
            for (number, entry) in (1u8..).zip(entries.as_chunks::<16>().0) {
                let type_id = entry.get(4).copied().unwrap_or(0);
                let start_lba = le_u32(entry, 8).unwrap_or(0);
                let sectors = le_u32(entry, 12).unwrap_or(0);
                if type_id == 0 && sectors == 0 {
                    continue;
                }
                partitions.push(MbrPartition {
                    number,
                    active: entry.first() == Some(&0x80),
                    type_id,
                    type_name: type_name(type_id),
                    start_lba,
                    sectors,
                });
            }
        }
        Self {
            identification,
            disk_signature,
            partitions,
        }
    }

    /// Whether the table only contains a GPT protective partition (type 0xEE).
    pub fn is_protective(&self) -> bool {
        self.partitions.iter().any(|p| p.type_id == 0xee)
    }

    /// Bytes covered by the primary partitions (the largest end offset).
    pub fn extent_bytes(&self) -> u64 {
        self.partitions
            .iter()
            .map(MbrPartition::end_bytes)
            .max()
            .unwrap_or(0)
    }
}

/// Names of common MBR partition types.
fn type_name(type_id: u8) -> Option<&'static str> {
    Some(match type_id {
        0x01 => "FAT12",
        0x04 | 0x06 | 0x0e => "FAT16",
        0x05 | 0x0f => "Extended",
        0x07 => "NTFS/exFAT/UDF",
        0x0b | 0x0c => "FAT32",
        0x11 | 0x14 | 0x16 | 0x1b | 0x1c | 0x1e => "Hidden FAT",
        0x17 => "Hidden NTFS",
        0x27 => "Windows recovery",
        0x82 => "Linux swap",
        0x83 => "Linux",
        0x85 => "Linux extended",
        0x8e => "Linux LVM",
        0xa5 => "FreeBSD",
        0xa6 => "OpenBSD",
        0xa9 => "NetBSD",
        0xaf => "macOS HFS+",
        0xee => "GPT protective",
        0xef => "EFI System",
        0xfd => "Linux RAID",
        _ => return None,
    })
}

#[cfg(test)]
#[allow(clippy::arithmetic_side_effects, clippy::cast_possible_truncation)] // Fixture arithmetic.
mod tests {
    use super::*;

    fn entry(sector: &mut [u8], index: usize, active: bool, type_id: u8, start: u32, count: u32) {
        let base = 0x1be + 16 * index;
        sector[base] = if active { 0x80 } else { 0 };
        sector[base + 4] = type_id;
        sector[base + 8..base + 12].copy_from_slice(&start.to_le_bytes());
        sector[base + 12..base + 16].copy_from_slice(&count.to_le_bytes());
    }

    #[test]
    fn partitions_are_parsed() {
        let mut sector = vec![0u8; 512];
        sector[0x1b8..0x1bc].copy_from_slice(&0xdead_beef_u32.to_le_bytes());
        entry(&mut sector, 0, true, 0x0c, 2048, 1_000_000);
        entry(&mut sector, 2, false, 0xef, 1_002_048, 8192);
        sector[0x1fe] = 0x55;
        sector[0x1ff] = 0xaa;
        let report = MbrReport::parse(&sector);
        assert_eq!(report.disk_signature, 0xdead_beef);
        assert_eq!(report.partitions.len(), 2);
        assert_eq!(report.partitions[0].number, 1);
        assert!(report.partitions[0].active);
        assert_eq!(report.partitions[0].type_name, Some("FAT32"));
        assert_eq!(report.partitions[1].number, 3);
        assert_eq!(report.extent_bytes(), (1_002_048 + 8192) * 512);
        assert!(!report.is_protective());
    }

    #[test]
    fn no_partitions_without_a_boot_marker() {
        let mut sector = vec![0u8; 512];
        entry(&mut sector, 0, true, 0x83, 1, 10);
        let report = MbrReport::parse(&sector);
        assert_eq!(report.identification, MbrIdentification::NoBootMarker);
        assert!(report.partitions.is_empty());
    }

    #[test]
    fn short_input_is_handled() {
        let report = MbrReport::parse(&[0u8; 100]);
        assert_eq!(report.identification, MbrIdentification::NoBootMarker);
        assert_eq!(report.disk_signature, 0);
    }

    #[test]
    fn huge_entries_saturate() {
        let mut sector = vec![0u8; 512];
        entry(&mut sector, 0, false, 0x83, u32::MAX, u32::MAX);
        sector[0x1fe] = 0x55;
        sector[0x1ff] = 0xaa;
        let report = MbrReport::parse(&sector);
        assert_eq!(report.extent_bytes(), (u64::from(u32::MAX) * 2) * 512);
    }
}
