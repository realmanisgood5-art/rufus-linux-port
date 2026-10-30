// SPDX-License-Identifier: GPL-3.0-or-later
//! The partition layout Rufus would create. Computed only; nothing is
//! written.
//!
//! Translated from Rufus v4.15 (Copyright © 2011-2026 Pete Batard,
//! GPL-3.0-or-later): the offset and size calculation of
//! `src/drive.c:CreatePartition()` and the choice of extra partitions in
//! `src/format.c:FormatThread()`. Windows-only conditions are resolved for
//! Linux: the ESP goes first whenever one is created (upstream does so on
//! fixed media and on Windows 10 1703 and later), and the drive geometry is
//! the conventional 63 sectors per track that Windows reports for USB
//! drives unless the caller knows better.

use crate::options::{FileSystem, PartitionScheme};
use crate::units::MB;
use serde::Serialize;

/// The ESP size Rufus uses.
pub const ESP_SIZE: u64 = 260 * MB;
/// The MSR size Rufus uses.
pub const MSR_SIZE: u64 = 128 * MB;
/// Size of `res/uefi/uefi-ntfs.img` at the v4.15 baseline.
pub const UEFI_NTFS_SIZE: u64 = 1024 * 1024;
/// `MAX_PARTITIONS`.
pub const MAX_PARTITIONS: usize = 16;
/// `RUFUS_EXTRA_PARTITION_TYPE`.
pub const EXTRA_PARTITION_TYPE: u8 = 0xea;
/// Sectors per track assumed when the geometry is unknown.
pub const DEFAULT_SECTORS_PER_TRACK: u32 = 63;

/// GPT partition type GUIDs Rufus uses.
pub mod gpt_type {
    /// `PARTITION_GENERIC_ESP`.
    pub const ESP: &str = "c12a7328-f81f-11d2-ba4b-00a0c93ec93b";
    /// `PARTITION_MICROSOFT_DATA`.
    pub const MICROSOFT_DATA: &str = "ebd0a0a2-b9e5-4433-87c0-68b6b72699c7";
    /// `PARTITION_LINUX_DATA`.
    pub const LINUX_DATA: &str = "0fc63daf-8483-4772-8e79-3d69d8477de4";
    /// `PARTITION_MICROSOFT_RESERVED`.
    pub const MICROSOFT_RESERVED: &str = "e3c9e316-0b5c-4db8-817d-f92df00215ae";
}

/// Extra partitions (`XP_*`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct ExtraPartitions {
    /// `XP_ESP | XP_MSR` (Windows To Go on GPT/UEFI).
    pub esp_and_msr: bool,
    /// `XP_UEFI_NTFS`.
    pub uefi_ntfs: bool,
    /// `XP_COMPAT` ("Add fixes for old BIOSes").
    pub compat: bool,
    /// `XP_PERSISTENCE`, with the persistence size in bytes.
    pub persistence: u64,
}

/// Everything the calculation depends on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LayoutInput {
    /// Partition scheme.
    pub scheme: PartitionScheme,
    /// File system of the main partition.
    pub file_system: FileSystem,
    /// Device size.
    pub disk_size: u64,
    /// Logical sector size.
    pub sector_size: u32,
    /// Sectors per track.
    pub sectors_per_track: u32,
    /// Cluster size (0: the file system's default, counted as 512).
    pub cluster_size: u32,
    /// "Add fixes for old BIOSes" (MBR only).
    pub old_bios_fixes: bool,
    /// Extra partitions.
    pub extra: ExtraPartitions,
    /// Whether the drive is made bootable (the MBR active flag).
    pub bootable: bool,
    /// `write_as_esp`: the main partition is an ESP.
    pub write_as_esp: bool,
}

/// A partition of the layout.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PlannedPartition {
    /// Name, as Rufus names it.
    pub name: &'static str,
    /// Offset in bytes.
    pub offset: u64,
    /// Size in bytes.
    pub size: u64,
    /// MBR partition type.
    pub mbr_type: Option<u8>,
    /// MBR active flag.
    pub active: bool,
    /// GPT partition type GUID.
    pub gpt_type: Option<&'static str>,
}

/// Why no layout can be made.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LayoutError(pub String);

fn ceiling_align(value: u64, align: u64) -> Option<u64> {
    if align == 0 {
        return Some(value);
    }
    value.checked_add(align - 1).map(|v| v / align * align)
}

fn floor_align(value: u64, align: u64) -> u64 {
    value.checked_div(align).map_or(value, |v| v * align)
}

/// `CreatePartition()`'s layout. A Super Floppy Disk has no partitions.
pub fn layout(input: &LayoutInput) -> Result<Vec<PlannedPartition>, LayoutError> {
    if input.scheme == PartitionScheme::Sfd {
        return Ok(Vec::new());
    }
    let sector = u64::from(input.sector_size.max(1));
    let track = u64::from(input.sectors_per_track).saturating_mul(sector);
    let cluster = if input.cluster_size == 0 {
        0x200
    } else {
        u64::from(input.cluster_size)
    };
    let cluster_aligned = cluster % sector == 0;
    let err = |message: &str| LayoutError(message.to_string());

    // (name, offset, size)
    let mut parts: Vec<(&'static str, u64, u64)> = Vec::new();
    let first_offset = if input.scheme == PartitionScheme::Gpt || !input.old_bios_fixes {
        MB
    } else {
        ceiling_align(track, cluster)
            .ok_or_else(|| err("invalid geometry"))?
            .saturating_mul(2)
    };
    let mut next_offset = first_offset;
    let mut esp_and_msr = input.extra.esp_and_msr;
    let align_next = |offset: u64, size: u64| -> Result<u64, LayoutError> {
        let end = offset
            .checked_add(size)
            .ok_or_else(|| err("partition too large"))?;
        let mut next = ceiling_align(end, track).ok_or_else(|| err("partition too large"))?;
        if cluster_aligned {
            next = floor_align(next, cluster);
        }
        Ok(next)
    };
    if esp_and_msr {
        if input.scheme != PartitionScheme::Gpt {
            return Err(err("an ESP and MSR need GPT"));
        }
        parts.push(("EFI System Partition", next_offset, ESP_SIZE));
        next_offset = align_next(next_offset, ESP_SIZE)?;
        parts.push(("Microsoft Reserved Partition", next_offset, MSR_SIZE));
        next_offset = align_next(next_offset, MSR_SIZE)?;
        esp_and_msr = false;
    }
    let main = parts.len();
    parts.push((
        if input.write_as_esp {
            "EFI System Partition"
        } else {
            "Main Data Partition"
        },
        next_offset,
        0,
    ));
    if input.extra.persistence > 0 {
        let size = ceiling_align(input.extra.persistence, track)
            .ok_or_else(|| err("persistence too large"))?;
        parts.push(("Linux Persistence", 0, size));
    }
    if esp_and_msr {
        let size = ceiling_align(ESP_SIZE, track).ok_or_else(|| err("invalid geometry"))?;
        parts.push(("EFI System Partition", 0, size));
    } else if input.extra.uefi_ntfs {
        let size = ceiling_align(UEFI_NTFS_SIZE, track).ok_or_else(|| err("invalid geometry"))?;
        parts.push(("UEFI:NTFS", 0, size));
    } else if input.extra.compat {
        parts.push(("BIOS Compatibility", 0, track));
    }
    if parts.len() > MAX_PARTITIONS {
        return Err(err("too many partitions"));
    }

    // Extra partitions go at the end, aligned to a track.
    let mut last_offset = input.disk_size;
    if input.scheme == PartitionScheme::Gpt {
        last_offset = last_offset.saturating_sub(33u64.saturating_mul(sector));
    }
    for part in parts.iter_mut().skip(main.saturating_add(1)).rev() {
        if part.2 >= last_offset {
            return Err(err("the device is too small for the extra partitions"));
        }
        part.1 = floor_align(last_offset - part.2, track);
        last_offset = part.1;
    }
    let main_offset = parts.get(main).map_or(0, |p| p.1);
    if last_offset <= main_offset {
        return Err(err("the device is too small"));
    }
    let mut main_size = floor_align(last_offset - main_offset, track);
    if cluster_aligned {
        main_size = floor_align(main_size, cluster);
    }
    if main_size == 0 {
        return Err(err("invalid main partition size"));
    }
    if let Some(part) = parts.get_mut(main) {
        part.2 = main_size;
    }

    let main_mbr_type = match input.file_system {
        FileSystem::Fat16 => 0x0e,
        FileSystem::Fat32 => 0x0c,
        FileSystem::Ntfs | FileSystem::ExFat | FileSystem::Udf | FileSystem::Refs => 0x07,
        FileSystem::Ext2 | FileSystem::Ext3 | FileSystem::Ext4 => 0x83,
    };
    Ok(parts
        .into_iter()
        .enumerate()
        .map(|(index, (name, offset, size))| {
            let (mbr_type, gpt_type) = if input.scheme == PartitionScheme::Mbr {
                let mbr = match name {
                    "EFI System Partition" | "UEFI:NTFS" => 0xef,
                    "Linux Persistence" => 0x83,
                    "BIOS Compatibility" => EXTRA_PARTITION_TYPE,
                    _ => main_mbr_type,
                };
                (Some(mbr), None)
            } else {
                let gpt = match name {
                    "UEFI:NTFS" => gpt_type::MICROSOFT_DATA,
                    "EFI System Partition" => gpt_type::ESP,
                    "Linux Persistence" => gpt_type::LINUX_DATA,
                    "Microsoft Reserved Partition" => gpt_type::MICROSOFT_RESERVED,
                    _ => gpt_type::MICROSOFT_DATA,
                };
                (None, Some(gpt))
            };
            PlannedPartition {
                name,
                offset,
                size,
                mbr_type,
                active: input.scheme == PartitionScheme::Mbr && index == main && input.bootable,
                gpt_type,
            }
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::units::GB;

    fn input(scheme: PartitionScheme, fs: FileSystem, size: u64) -> LayoutInput {
        LayoutInput {
            scheme,
            file_system: fs,
            disk_size: size,
            sector_size: 512,
            sectors_per_track: 63,
            cluster_size: 0,
            old_bios_fixes: false,
            extra: ExtraPartitions::default(),
            bootable: true,
            write_as_esp: false,
        }
    }

    #[test]
    fn single_fat32_partition_on_mbr() {
        let parts = layout(&input(
            PartitionScheme::Mbr,
            FileSystem::Fat32,
            16_000_000_000,
        ))
        .unwrap();
        assert_eq!(parts.len(), 1);
        let main = &parts[0];
        assert_eq!(main.offset, MB);
        assert_eq!(main.mbr_type, Some(0x0c));
        assert!(main.active);
        // Track aligned (63 * 512) and inside the disk.
        assert_eq!(main.size % (63 * 512), 0);
        assert!(main.offset + main.size <= 16_000_000_000);
        assert!(16_000_000_000 - (main.offset + main.size) < 63 * 512);
    }

    #[test]
    fn ntfs_with_uefi_ntfs_on_gpt() {
        let mut i = input(PartitionScheme::Gpt, FileSystem::Ntfs, 32 * GB);
        i.extra.uefi_ntfs = true;
        i.cluster_size = 4096;
        let parts = layout(&i).unwrap();
        assert_eq!(parts.len(), 2);
        let (main, uefi) = (&parts[0], &parts[1]);
        assert_eq!(uefi.name, "UEFI:NTFS");
        assert_eq!(uefi.gpt_type, Some(gpt_type::MICROSOFT_DATA));
        // The backup GPT (33 sectors) stays free.
        assert!(uefi.offset + uefi.size <= 32 * GB - 33 * 512);
        assert_eq!(uefi.offset % (63 * 512), 0);
        assert!(main.offset + main.size <= uefi.offset);
        assert_eq!(main.size % 4096, 0);
    }

    #[test]
    fn windows_to_go_gets_esp_and_msr_first() {
        let mut i = input(PartitionScheme::Gpt, FileSystem::Ntfs, 64 * GB);
        i.extra.esp_and_msr = true;
        let parts = layout(&i).unwrap();
        let names: Vec<_> = parts.iter().map(|p| p.name).collect();
        assert_eq!(
            names,
            [
                "EFI System Partition",
                "Microsoft Reserved Partition",
                "Main Data Partition"
            ]
        );
        assert_eq!(parts[0].offset, MB);
        assert_eq!(parts[0].size, ESP_SIZE);
        assert!(parts[1].offset >= parts[0].offset + parts[0].size);
        assert!(parts[2].offset >= parts[1].offset + parts[1].size);
        assert_eq!(parts[0].gpt_type, Some(gpt_type::ESP));
        assert_eq!(parts[1].gpt_type, Some(gpt_type::MICROSOFT_RESERVED));
        // ESP and MSR need GPT.
        let mut mbr = i.clone();
        mbr.scheme = PartitionScheme::Mbr;
        assert!(layout(&mbr).is_err());
    }

    #[test]
    fn persistence_and_old_bios_fixes() {
        let mut i = input(PartitionScheme::Mbr, FileSystem::Fat32, 16 * GB);
        i.extra.persistence = 4 * GB;
        i.extra.compat = true;
        i.old_bios_fixes = true;
        let parts = layout(&i).unwrap();
        let names: Vec<_> = parts.iter().map(|p| p.name).collect();
        assert_eq!(
            names,
            [
                "Main Data Partition",
                "Linux Persistence",
                "BIOS Compatibility"
            ]
        );
        // Old BIOS fixes: two tracks (aligned to the cluster) before the first partition.
        assert_eq!(parts[0].offset, 2 * 63 * 512);
        assert_eq!(parts[1].mbr_type, Some(0x83));
        assert_eq!(parts[2].mbr_type, Some(EXTRA_PARTITION_TYPE));
        assert_eq!(parts[2].size, 63 * 512);
        for pair in parts.windows(2) {
            assert!(pair[0].offset + pair[0].size <= pair[1].offset);
        }
    }

    #[test]
    fn super_floppy_and_too_small() {
        assert!(
            layout(&input(PartitionScheme::Sfd, FileSystem::Fat32, GB))
                .unwrap()
                .is_empty()
        );
        let mut i = input(PartitionScheme::Mbr, FileSystem::Fat32, 8 * MB);
        i.extra.persistence = 16 * MB;
        assert!(layout(&i).is_err());
    }
}
