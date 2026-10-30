// SPDX-License-Identifier: GPL-3.0-or-later
//! Master boot record identification.
//!
//! Translated from Rufus `src/drive.c:AnalyzeMBR()` and its `known_mbr[]`
//! table (Copyright © 2011-2026 Pete Batard, GPL-3.0-or-later) and from the
//! matching functions of ms-sys `src/ms-sys/br.c` (Copyright © 2009-2015
//! Henrik Carlqvist, GPL-2.0-or-later), at the Rufus v4.15 baseline. The boot
//! code tables in `ms_sys_tables` are generated from upstream.

#![deny(clippy::indexing_slicing, clippy::arithmetic_side_effects)]

use super::ms_sys_tables as t;
use serde::Serialize;

/// Kinds of master boot record known to Rufus, in the order
/// `src/drive.c:AnalyzeMBR()` tests them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum MbrKind {
    /// `is_dos_mbr()`: "DOS/NT/95A".
    Dos,
    /// `is_dos_f2_mbr()`: "DOS/NT/95A (F2)".
    DosF2,
    /// `is_95b_mbr()`: "Windows 95B/98/98SE/ME".
    Windows95B,
    /// `is_2000_mbr()`: "Windows 2000/XP/2003".
    Windows2000,
    /// `is_vista_mbr()`: "Windows Vista".
    WindowsVista,
    /// `is_win7_mbr()`: "Windows 7".
    Windows7,
    /// `is_rufus_mbr()`: "Rufus".
    Rufus,
    /// `is_syslinux_mbr()`: "Syslinux".
    Syslinux,
    /// `is_isolinux_mbr()`: "Isolinux".
    Isolinux,
    /// `is_reactos_mbr()`: "ReactOS".
    ReactOs,
    /// `is_kolibrios_mbr()`: "KolibriOS".
    KolibriOs,
    /// `is_grub4dos_mbr()`: "Grub4DOS".
    Grub4Dos,
    /// `is_grub2_mbr()`: "Grub 2.0".
    Grub2,
    /// `is_zero_mbr_not_including_disk_signature_or_copy_protect()`: "Zeroed".
    Zeroed,
}

impl MbrKind {
    /// Every kind, in upstream matching order.
    pub const ALL: [Self; 14] = [
        Self::Dos,
        Self::DosF2,
        Self::Windows95B,
        Self::Windows2000,
        Self::WindowsVista,
        Self::Windows7,
        Self::Rufus,
        Self::Syslinux,
        Self::Isolinux,
        Self::ReactOs,
        Self::KolibriOs,
        Self::Grub4Dos,
        Self::Grub2,
        Self::Zeroed,
    ];

    /// The name Rufus prints (`Image has a <name> Master Boot Record`).
    pub fn name(self) -> &'static str {
        match self {
            Self::Dos => "DOS/NT/95A",
            Self::DosF2 => "DOS/NT/95A (F2)",
            Self::Windows95B => "Windows 95B/98/98SE/ME",
            Self::Windows2000 => "Windows 2000/XP/2003",
            Self::WindowsVista => "Windows Vista",
            Self::Windows7 => "Windows 7",
            Self::Rufus => "Rufus",
            Self::Syslinux => "Syslinux",
            Self::Isolinux => "Isolinux",
            Self::ReactOs => "ReactOS",
            Self::KolibriOs => "KolibriOS",
            Self::Grub4Dos => "Grub4DOS",
            Self::Grub2 => "Grub 2.0",
            Self::Zeroed => "Zeroed",
        }
    }

    /// Whether `sector` (the first bytes of a disk) contains this boot code.
    fn matches(self, sector: &[u8]) -> bool {
        match self {
            Self::Dos => contains(sector, 0x0, &t::MBR_DOS_0X0) && is_br(sector),
            Self::DosF2 => contains(sector, 0x0, &t::MBR_DOS_F2_0X0) && is_br(sector),
            Self::Windows95B => {
                contains(sector, 0x0, &t::MBR_95B_0X0)
                    && contains(sector, 0x0e0, &t::MBR_95B_0X0E0)
                    && is_br(sector)
            }
            Self::Windows2000 => {
                contains_prefix(sector, 0x0, &t::MBR_2000_0X0, t::MBR_2000_LANG_INDEP_LEN)
                    && is_br(sector)
            }
            Self::WindowsVista => {
                contains_prefix(sector, 0x0, &t::MBR_VISTA_0X0, t::MBR_VISTA_LANG_INDEP_LEN)
                    && is_br(sector)
            }
            Self::Windows7 => {
                contains_prefix(sector, 0x0, &t::MBR_WIN7_0X0, t::MBR_WIN7_LANG_INDEP_LEN)
                    && is_br(sector)
            }
            Self::Rufus => contains(sector, 0x0, &t::MBR_RUFUS_0X0) && is_br(sector),
            Self::Syslinux => contains(sector, 0x0, &t::MBR_SYSLINUX_0X0) && is_br(sector),
            Self::Isolinux => is_isolinux(sector),
            Self::ReactOs => contains(sector, 0x0, &t::MBR_REACTOS_0X0) && is_br(sector),
            Self::KolibriOs => contains(sector, 0x0, &t::MBR_KOLIBRI_0X0) && is_br(sector),
            Self::Grub4Dos => contains(sector, 0x0, &t::MBR_GRUB_0X0) && is_br(sector),
            Self::Grub2 => is_grub2(sector),
            // Does not check the boot marker: `AnalyzeMBR()` has already done so.
            Self::Zeroed => contains_prefix(sector, 0x0, &t::MBR_ZERO_0X0, 0x1b8),
        }
    }
}

impl std::fmt::Display for MbrKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

/// Result of `AnalyzeMBR()` on the first sector of a disk or image.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(tag = "status", content = "kind", rename_all = "kebab-case")]
pub enum MbrIdentification {
    /// No 0x55 0xAA boot marker at offset 0x1FE ("does not have a Boot Marker").
    NoBootMarker,
    /// A boot marker and boot code Rufus recognizes.
    Known(MbrKind),
    /// A boot marker but unrecognized boot code ("has an unknown Master Boot Record").
    Unknown,
}

impl MbrIdentification {
    /// `AnalyzeMBR()`'s return value: whether the disk "seems bootable".
    pub fn has_boot_marker(self) -> bool {
        !matches!(self, Self::NoBootMarker)
    }
}

/// A 512-byte sector containing the boot code of `kind` where the upstream
/// identification functions look for it, an empty partition table and the
/// boot marker. For tests and, later, for writing boot records.
pub fn reference_sector(kind: MbrKind) -> [u8; 512] {
    let mut sector = [0u8; 512];
    let mut put = |offset: usize, data: &[u8]| {
        if let Some(target) = offset
            .checked_add(data.len())
            .and_then(|end| sector.get_mut(offset..end))
        {
            target.copy_from_slice(data);
        }
    };
    match kind {
        MbrKind::Dos => put(0, &t::MBR_DOS_0X0),
        MbrKind::DosF2 => put(0, &t::MBR_DOS_F2_0X0),
        MbrKind::Windows95B => {
            put(0, &t::MBR_95B_0X0);
            put(0xe0, &t::MBR_95B_0X0E0);
        }
        MbrKind::Windows2000 => put(0, &t::MBR_2000_0X0),
        MbrKind::WindowsVista => put(0, &t::MBR_VISTA_0X0),
        MbrKind::Windows7 => put(0, &t::MBR_WIN7_0X0),
        MbrKind::Rufus => put(0, &t::MBR_RUFUS_0X0),
        MbrKind::Syslinux => put(0, &t::MBR_SYSLINUX_0X0),
        // The table starts at disk offset 0x20, so its part from 0x60 lands at 0x80.
        MbrKind::Isolinux => put(0x20, &t::MBR_ISOLINUX_0X0),
        MbrKind::ReactOs => put(0, &t::MBR_REACTOS_0X0),
        MbrKind::KolibriOs => put(0, &t::MBR_KOLIBRI_0X0),
        MbrKind::Grub4Dos => put(0, &t::MBR_GRUB_0X0),
        MbrKind::Grub2 => put(0, &t::MBR_GRUB2_0X0),
        MbrKind::Zeroed => {}
    }
    put(0x1fe, &[0x55, 0xaa]);
    sector
}

/// Identifies the master boot record in `sector`, which holds at least the
/// first 512 bytes of a disk or image (fewer bytes means no boot marker).
pub fn identify(sector: &[u8]) -> MbrIdentification {
    if !is_br(sector) {
        return MbrIdentification::NoBootMarker;
    }
    MbrKind::ALL
        .into_iter()
        .find(|kind| kind.matches(sector))
        .map_or(MbrIdentification::Unknown, MbrIdentification::Known)
}

/// `is_br()`: the 0x55 0xAA boot marker at offset 0x1FE.
pub fn is_br(sector: &[u8]) -> bool {
    contains(sector, 0x1fe, &[0x55, 0xaa])
}

/// `contains_data()`: `data` occurs at `position` of `sector`.
fn contains(sector: &[u8], position: usize, data: &[u8]) -> bool {
    position
        .checked_add(data.len())
        .and_then(|end| sector.get(position..end))
        .is_some_and(|found| found == data)
}

/// `contains_data()` with an explicit length: the first `len` bytes of `data`.
fn contains_prefix(sector: &[u8], position: usize, data: &[u8], len: usize) -> bool {
    data.get(..len)
        .is_some_and(|prefix| contains(sector, position, prefix))
}

/// `data[from..]`, or an empty slice.
fn tail(data: &[u8], from: usize) -> &[u8] {
    data.get(from..).unwrap_or_default()
}

/// `is_isolinux_mbr()`: the table starts at disk offset 0x20, and its part
/// from 0x60 is found at disk offset 0x80 or 0x82.
fn is_isolinux(sector: &[u8]) -> bool {
    let table = &t::MBR_ISOLINUX_0X0;
    contains_prefix(sector, 0x20, table, 0x48)
        && (contains(sector, 0x80, tail(table, 0x60)) || contains(sector, 0x82, tail(table, 0x60)))
        && is_br(sector)
}

/// `is_grub2_mbr()`: compares the GRUB 2 `boot.img` code while skipping the
/// bytes GRUB patches at install time.
fn is_grub2(sector: &[u8]) -> bool {
    const SEGMENTS: [(usize, usize); 8] = [
        (0x0, 0x2),
        (0x64, 0x2),
        (0x68, 0x2c),
        (0x96, 0x29),
        (0xc1, 0x7),
        (0xca, 0x1f),
        (0xeb, 0x2b),
        (0x118, 0x7),
    ];
    let table = &t::MBR_GRUB2_0X0;
    let segment_matches =
        |(offset, len): (usize, usize)| contains_prefix(sector, offset, tail(table, offset), len);
    SEGMENTS.into_iter().all(segment_matches)
        && contains(sector, 0x121, tail(table, 0x121))
        && is_br(sector)
}

#[cfg(test)]
#[allow(clippy::arithmetic_side_effects, clippy::cast_possible_truncation)] // Fixture arithmetic.
mod tests {
    use super::*;

    /// A 512-byte sector starting with `code`, with the boot marker.
    fn sector_with(code: &[u8], offset: usize) -> Vec<u8> {
        let mut sector = vec![0u8; 512];
        sector[offset..offset + code.len()].copy_from_slice(code);
        sector[0x1fe] = 0x55;
        sector[0x1ff] = 0xaa;
        sector
    }

    fn sector_for(kind: MbrKind) -> Vec<u8> {
        reference_sector(kind).to_vec()
    }

    #[test]
    fn every_kind_is_identified() {
        for kind in MbrKind::ALL {
            assert_eq!(
                identify(&sector_for(kind)),
                MbrIdentification::Known(kind),
                "{kind}"
            );
        }
    }

    #[test]
    fn missing_boot_marker() {
        let mut sector = sector_for(MbrKind::Grub2);
        sector[0x1ff] = 0;
        assert_eq!(identify(&sector), MbrIdentification::NoBootMarker);
        assert_eq!(identify(&[0x55, 0xaa]), MbrIdentification::NoBootMarker);
        assert_eq!(identify(&[]), MbrIdentification::NoBootMarker);
    }

    #[test]
    fn unknown_boot_code_with_a_marker() {
        let sector = sector_with(&[0xeb, 0x3c, 0x90, 0x12, 0x34], 0);
        assert_eq!(identify(&sector), MbrIdentification::Unknown);
        assert!(identify(&sector).has_boot_marker());
    }

    #[test]
    fn language_dependent_bytes_are_ignored() {
        let mut sector = sector_for(MbrKind::Windows7);
        // Bytes past MBR_WIN7_LANG_INDEP_LEN vary with the Windows language.
        sector[t::MBR_WIN7_LANG_INDEP_LEN] ^= 0xff;
        assert_eq!(
            identify(&sector),
            MbrIdentification::Known(MbrKind::Windows7)
        );
    }

    #[test]
    fn grub2_patched_bytes_are_ignored() {
        let mut sector = sector_for(MbrKind::Grub2);
        // GRUB writes the core.img location and boot drive in these gaps.
        for gap in [0x5c, 0x66, 0x94, 0xbf, 0xc8, 0xe9, 0x116, 0x11f] {
            sector[gap] ^= 0xa5;
        }
        assert_eq!(identify(&sector), MbrIdentification::Known(MbrKind::Grub2));
    }

    #[test]
    fn isolinux_alternate_offset() {
        let mut sector = sector_for(MbrKind::Isolinux);
        let rest = &t::MBR_ISOLINUX_0X0[0x60..];
        sector[0x80..0x80 + rest.len()].fill(0);
        sector[0x82..0x82 + rest.len()].copy_from_slice(rest);
        assert_eq!(
            identify(&sector),
            MbrIdentification::Known(MbrKind::Isolinux)
        );
    }

    #[test]
    fn zeroed_code_area_is_identified() {
        // An all-zero code area matches "Zeroed", which is tested last.
        let sector = sector_with(&[], 0);
        assert_eq!(identify(&sector), MbrIdentification::Known(MbrKind::Zeroed));
    }
}
