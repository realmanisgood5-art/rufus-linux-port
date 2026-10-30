// SPDX-License-Identifier: GPL-3.0-or-later
//! Minimal read-only FAT directory lookup, enough to find the UEFI boot
//! loaders inside an `efi.img` stored in an ISO.
//!
//! Translated from syslinux libfat as bundled with Rufus v4.15
//! (`src/syslinux/libfat/open.c:libfat_open()`, `searchdir.c:libfat_searchdir()`,
//! `fatchain.c:libfat_clustertosector()` and `libfat_nextsector()`,
//! Copyright 2004-2008 H. Peter Anvin, GPL-2.0-or-later), with the reading
//! scheme of Rufus `src/iso.c:iso9660_readfat()` (512-byte sectors counted
//! from the start of the image file). Differences from upstream: a lookup
//! visits at most [`MAX_SECTORS`] sectors, so a cyclic cluster chain ends
//! the search instead of looping forever.

#![deny(clippy::indexing_slicing, clippy::arithmetic_side_effects)]

use super::io::{ByteSource, le_u16, le_u32, read_exact_at};
use crate::Result;

/// `LIBFAT_SECTOR_SIZE`.
const SECTOR: u64 = 512;
/// Sectors a directory lookup may visit.
pub const MAX_SECTORS: u32 = 1 << 16;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FatType {
    Fat12,
    Fat16,
    Fat28,
}

/// A FAT file system starting at byte `base` of `source`.
#[allow(missing_debug_implementations)]
pub struct Fat<'a> {
    source: &'a dyn ByteSource,
    base: u64,
    clust_shift: u32,
    end: u64,
    fat: u64,
    rootdir: u64,
    data: u64,
    end_cluster: u64,
    fat_type: FatType,
    root_cluster: u64,
}

/// What `libfat_searchdir()` returns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Found {
    /// A matching entry: its raw 32-byte directory entry and first cluster
    /// (0 for an empty file).
    Entry([u8; 32], u32),
    /// Not found (-2).
    NotFound,
    /// Read error or invalid chain (-1).
    Error,
}

impl<'a> Fat<'a> {
    fn sector(&self, sector: u64) -> Result<Option<Vec<u8>>> {
        let Some(offset) = sector
            .checked_mul(SECTOR)
            .and_then(|o| o.checked_add(self.base))
        else {
            return Ok(None);
        };
        read_exact_at(self.source, offset, 512)
    }

    /// `libfat_open()`.
    pub fn open(source: &'a dyn ByteSource, base: u64) -> Result<Option<Self>> {
        let Some(bs) = read_exact_at(source, base, 512)? else {
            return Ok(None);
        };
        if le_u16(&bs, 11) != Some(512) {
            return Ok(None);
        }
        let per_cluster = bs.get(13).copied().unwrap_or(0);
        // 2^8 wraps to 0 in a byte: 0 is accepted as 256 sectors.
        let Some(clust_shift) = (0u32..=8).find(|&i| (1u32 << i).to_le_bytes()[0] == per_cluster)
        else {
            return Ok(None);
        };
        let mut sectors = u64::from(le_u16(&bs, 19).unwrap_or(0));
        if sectors == 0 {
            sectors = u64::from(le_u32(&bs, 32).unwrap_or(0));
        }
        let fat = u64::from(le_u16(&bs, 14).unwrap_or(0));
        let mut fat_size = u64::from(le_u16(&bs, 22).unwrap_or(0));
        if fat_size == 0 {
            fat_size = u64::from(le_u32(&bs, 36).unwrap_or(0));
        }
        let fats = u64::from(bs.get(16).copied().unwrap_or(0));
        let rootdir = fat.saturating_add(fat_size.saturating_mul(fats));
        let root_entries = u64::from(le_u16(&bs, 17).unwrap_or(0));
        let rootdir_size = root_entries.saturating_mul(32).saturating_add(SECTOR - 1) / SECTOR;
        let data = rootdir.saturating_add(rootdir_size);
        if data >= sectors {
            return Ok(None);
        }
        let clusters = sectors.saturating_sub(data) >> clust_shift;
        let end_cluster = clusters.saturating_add(2);
        let (fat_type, min_fat) = if clusters <= 0xff4 {
            (FatType::Fat12, end_cluster.saturating_add(end_cluster >> 1))
        } else if clusters <= 0xfff4 {
            (FatType::Fat16, end_cluster.saturating_mul(2))
        } else if clusters <= 0xfff_fff4 {
            (FatType::Fat28, end_cluster.saturating_mul(4))
        } else {
            return Ok(None);
        };
        if min_fat.saturating_add(SECTOR - 1) / SECTOR > fat_size {
            return Ok(None);
        }
        let root_cluster = if fat_type == FatType::Fat28 {
            u64::from(le_u32(&bs, 44).unwrap_or(0))
        } else {
            0
        };
        Ok(Some(Self {
            source,
            base,
            clust_shift,
            end: sectors,
            fat,
            rootdir,
            data,
            end_cluster,
            fat_type,
            root_cluster,
        }))
    }

    /// `libfat_clustertosector()`; `None` is -1.
    fn cluster_to_sector(&self, cluster: u64) -> Option<u64> {
        let cluster = if cluster == 0 {
            self.root_cluster
        } else {
            cluster
        };
        if cluster == 0 {
            Some(self.rootdir)
        } else if cluster < 2 || cluster >= self.end_cluster {
            None
        } else {
            Some(
                self.data
                    .saturating_add(cluster.saturating_sub(2) << self.clust_shift),
            )
        }
    }

    /// `libfat_nextsector()`: `Ok(Some(0))` is the end of the chain,
    /// `Ok(None)` an error.
    fn next_sector(&self, s: u64) -> Result<Option<u64>> {
        if s < self.data {
            if s < self.rootdir {
                return Ok(None);
            }
            let next = s.saturating_add(1);
            return Ok(Some(if next < self.data { next } else { 0 }));
        }
        let rs = s.saturating_sub(self.data);
        let mask = (1u64 << self.clust_shift).saturating_sub(1);
        if !rs & mask != 0 {
            return Ok(Some(s.saturating_add(1)));
        }
        let cluster = (rs >> self.clust_shift).saturating_add(2);
        if cluster >= self.end_cluster {
            return Ok(None);
        }
        let byte_at = |offset: u64| -> Result<Option<u8>> {
            let sector = self.sector(self.fat.saturating_add(offset / SECTOR))?;
            Ok(sector.and_then(|data| {
                data.get(usize::try_from(offset % SECTOR).unwrap_or(0))
                    .copied()
            }))
        };
        let next = match self.fat_type {
            FatType::Fat12 => {
                let offset = cluster.saturating_add(cluster >> 1);
                let (Some(lo), Some(hi)) = (byte_at(offset)?, byte_at(offset.saturating_add(1))?)
                else {
                    return Ok(None);
                };
                let mut value = u64::from(lo) | (u64::from(hi) << 8);
                value = if cluster & 1 == 1 {
                    value >> 4
                } else {
                    value & 0x0fff
                };
                if value >= 0x0ff8 {
                    return Ok(Some(0));
                }
                value
            }
            FatType::Fat16 => {
                let offset = cluster.saturating_mul(2);
                let (Some(lo), Some(hi)) = (byte_at(offset)?, byte_at(offset.saturating_add(1))?)
                else {
                    return Ok(None);
                };
                let value = u64::from(lo) | (u64::from(hi) << 8);
                if value >= 0xfff8 {
                    return Ok(Some(0));
                }
                value
            }
            FatType::Fat28 => {
                let offset = cluster.saturating_mul(4);
                let mut value = 0u64;
                for i in 0..4u64 {
                    let Some(b) = byte_at(offset.saturating_add(i))? else {
                        return Ok(None);
                    };
                    value |= u64::from(b) << (8u64.saturating_mul(i));
                }
                value &= 0x0fff_ffff;
                if value >= 0x0fff_fff8 {
                    return Ok(Some(0));
                }
                value
            }
        };
        Ok(self.cluster_to_sector(next))
    }

    /// `libfat_searchdir()`: looks for an 11-byte 8.3 name in the directory
    /// starting at `dir_cluster` (0 is the root directory).
    pub fn search_dir(&self, dir_cluster: u32, name: &[u8; 11]) -> Result<Found> {
        let Some(mut s) = self.cluster_to_sector(u64::from(dir_cluster)) else {
            return Ok(Found::Error);
        };
        for _ in 0..MAX_SECTORS {
            if s == 0 {
                return Ok(Found::NotFound);
            }
            if s >= self.end {
                return Ok(Found::Error);
            }
            let Some(sector) = self.sector(s)? else {
                return Ok(Found::Error);
            };
            for entry in sector.as_chunks::<32>().0 {
                let attribute = entry.get(11).copied().unwrap_or(0);
                if attribute & 0x48 == 0 && entry.get(..11) == Some(name.as_slice()) {
                    if le_u32(entry, 28) == Some(0) {
                        return Ok(Found::Entry(*entry, 0));
                    }
                    let cluster = u32::from(le_u16(entry, 26).unwrap_or(0))
                        | (u32::from(le_u16(entry, 20).unwrap_or(0)) << 16);
                    return Ok(Found::Entry(*entry, cluster));
                }
                if entry.first() == Some(&0) {
                    return Ok(Found::NotFound);
                }
            }
            match self.next_sector(s)? {
                Some(next) => s = next,
                None => return Ok(Found::Error),
            }
        }
        Ok(Found::Error)
    }
}

/// `HasEfiImgBootLoaders()` once the image file is located: the FAT file
/// system at `base` (or in the first MBR partition, if it is an active FAT
/// partition starting on a 2048-byte boundary) has `/EFI/BOOT/BOOT<arch>.EFI`
/// files. Returns the architectures found, as indexes into `efi_archname[]`.
pub fn efi_img_boot_loaders(
    source: &dyn ByteSource,
    mut base: u64,
    arch_names: &[&str],
) -> Result<Vec<usize>> {
    let mut found = Vec::new();
    let Some(head) = read_exact_at(source, base, 512)? else {
        return Ok(found);
    };
    let is_fat_type = |t: u8| matches!(t, 0x01 | 0x04 | 0x06 | 0x0b | 0x0c | 0x0e);
    if head.get(0x1fe..0x200) == Some([0x55, 0xaa].as_slice())
        && head.get(0x1be) == Some(&0x80)
        && head.get(0x1c2).copied().is_some_and(is_fat_type)
    {
        let lba = u64::from(le_u32(&head, 0x1c6).unwrap_or(0));
        if lba % 4 != 0 {
            return Ok(found);
        }
        base = base.saturating_add(lba.saturating_mul(SECTOR));
    }
    let Some(fat) = Fat::open(source, base)? else {
        return Ok(found);
    };
    let Found::Entry(efi, _) = fat.search_dir(0, b"EFI        ")? else {
        return Ok(found);
    };
    // Only the low 16 bits of the cluster, as upstream reads them.
    let dir = u32::from(le_u16(&efi, 26).unwrap_or(0));
    let Found::Entry(boot, _) = fat.search_dir(dir, b"BOOT       ")? else {
        return Ok(found);
    };
    let dir = u32::from(le_u16(&boot, 26).unwrap_or(0));
    for (index, arch) in arch_names.iter().enumerate().skip(1) {
        let a = arch.as_bytes();
        let pick = |i: usize| a.get(i).copied().unwrap_or(b' ');
        let (c3, c4) = if a.len() > 4 {
            (b'~', b'1')
        } else if a.len() > 3 {
            (pick(2), pick(3))
        } else {
            (pick(2), b' ')
        };
        let mut name = *b"BOOT    EFI";
        if let Some(slot) = name.get_mut(4..8) {
            slot.copy_from_slice(&[pick(0), pick(1), c3, c4]);
        }
        name.make_ascii_uppercase();
        if let Found::Entry(_, cluster) = fat.search_dir(dir, &name)?
            && cluster > 0
        {
            found.push(index);
        }
    }
    Ok(found)
}
