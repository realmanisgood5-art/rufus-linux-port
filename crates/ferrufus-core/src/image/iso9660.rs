// SPDX-License-Identifier: GPL-3.0-or-later
//! ISO 9660 volume descriptors, Joliet, and El Torito boot catalogs.
//!
//! Translated from libcdio as shipped with Rufus v4.15:
//! `src/libcdio/iso9660/iso9660_fs.c:check_pvd()`,
//! `iso9660_ifs_read_superblock()` and `get_member_id()` (Copyright © 2003-2024
//! Rocky Bernstein, © 2018-2024 Pete Batard, © 2018 Thomas Schmitt, © 2001
//! Herbert Valerio Riedel; GPL-3.0-or-later).
//!
//! Differences from upstream: the descriptor scan stops after
//! [`MAX_DESCRIPTORS`] sectors instead of continuing until a read fails, and
//! UCS-2 text is decoded with replacement characters for invalid code units.

#![deny(clippy::indexing_slicing, clippy::arithmetic_side_effects)]

use super::io::{ByteSource, le_u16, le_u32, read_exact_at, slice};
use crate::Result;
use serde::Serialize;

/// ISO 9660 logical sector size (`ISO_BLOCKSIZE`).
pub const ISO_BLOCKSIZE: u64 = 2048;
/// Sector of the primary volume descriptor (`ISO_PVD_SECTOR`).
const ISO_PVD_SECTOR: u64 = 16;
/// Upper bound on the number of volume descriptors examined.
pub const MAX_DESCRIPTORS: u64 = 256;
/// `MAX_BOOT_IMAGES` in libcdio.
const MAX_BOOT_IMAGES: usize = 8;
/// `EL_TORITO_ID`: compared over all 32 bytes, including the zero padding.
const EL_TORITO_ID: &[u8; 32] = b"EL TORITO SPECIFICATION\0\0\0\0\0\0\0\0\0";

/// The ISO 9660 view of an image.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Iso9660Report {
    /// System identifier from the primary volume descriptor.
    pub system_id: String,
    /// Volume identifier from the primary volume descriptor (trailing spaces removed).
    pub pvd_volume_id: String,
    /// Joliet level (0 when no Joliet descriptor is present).
    pub joliet_level: u8,
    /// Volume identifier from the Joliet descriptor, when there is one.
    pub joliet_volume_id: Option<String>,
    /// The label Rufus uses (`iso9660_ifs_get_volume_id()`), if not empty.
    pub label: Option<String>,
    /// Volume space size in logical blocks.
    pub volume_space_blocks: u32,
    /// Logical block size.
    pub logical_block_size: u16,
    /// Publisher identifier.
    pub publisher_id: String,
    /// Application identifier.
    pub application_id: String,
    /// El Torito boot catalog, when present.
    pub el_torito: Option<ElTorito>,
}

impl Iso9660Report {
    /// Size of the ISO 9660 volume in bytes.
    pub fn volume_bytes(&self) -> Option<u64> {
        u64::from(self.volume_space_blocks).checked_mul(u64::from(self.logical_block_size))
    }
}

/// An El Torito boot catalog.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ElTorito {
    /// Sector of the boot catalog.
    pub catalog_sector: u32,
    /// Bootable images, as libcdio lists them (`[BOOT]/<n>-Boot-<media>.img` in Rufus).
    pub images: Vec<BootImage>,
}

impl ElTorito {
    /// Whether any boot image targets the given platform.
    pub fn has_platform(&self, platform: Platform) -> bool {
        self.images
            .iter()
            .any(|image| image.platform == Some(platform))
    }
}

/// A bootable El Torito image.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BootImage {
    /// Emulation type (`media_type & 0x0F`).
    pub media: BootMedia,
    /// Platform of the catalog section the entry belongs to.
    pub platform: Option<Platform>,
    /// First sector of the image.
    pub lsn: u32,
    /// Size in 512-byte virtual sectors, as recorded in the catalog.
    pub virtual_sectors: u16,
}

/// El Torito emulation types (`eltorito_media_name[]` in libcdio).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum BootMedia {
    /// No emulation.
    NoEmul,
    /// 1.2 MB floppy.
    Floppy12M,
    /// 1.44 MB floppy.
    Floppy144M,
    /// 2.88 MB floppy.
    Floppy288M,
    /// Hard disk.
    HardDisk,
}

impl BootMedia {
    fn from_type(media_type: u8) -> Option<Self> {
        Some(match media_type & 0x0f {
            0 => Self::NoEmul,
            1 => Self::Floppy12M,
            2 => Self::Floppy144M,
            3 => Self::Floppy288M,
            4 => Self::HardDisk,
            _ => return None,
        })
    }
}

/// El Torito platform identifiers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Platform {
    /// 80x86 (BIOS), id 0.
    X86,
    /// PowerPC, id 1.
    PowerPc,
    /// Macintosh, id 2.
    Mac,
    /// UEFI, id 0xEF.
    Efi,
    /// Any other id.
    Other(u8),
}

impl Platform {
    fn from_id(id: u8) -> Self {
        match id {
            0 => Self::X86,
            1 => Self::PowerPc,
            2 => Self::Mac,
            0xef => Self::Efi,
            other => Self::Other(other),
        }
    }
}

/// Reads the volume descriptors of an image. Returns `None` if the image has
/// no primary volume descriptor (libcdio `check_pvd()` fails).
pub fn parse(source: &dyn ByteSource) -> Result<Option<Iso9660Report>> {
    let Some(pvd) = read_sector(source, ISO_PVD_SECTOR)? else {
        return Ok(None);
    };
    // check_pvd(): type ISO_VD_PRIMARY and identifier "CD001".
    if pvd.first() != Some(&1) || pvd.get(1..6) != Some(b"CD001".as_slice()) {
        return Ok(None);
    }

    let mut joliet_level = 0u8;
    let mut svd: Option<Vec<u8>> = None;
    let mut el_torito = None;
    // iso9660_ifs_read_superblock(): every descriptor after the PVD until the
    // terminator (type 255) or the end of the image.
    for index in 1..MAX_DESCRIPTORS {
        let Some(sector) = ISO_PVD_SECTOR.checked_add(index) else {
            break;
        };
        let Some(descriptor) = read_sector(source, sector)? else {
            break;
        };
        match descriptor.first().copied() {
            Some(255) => break,
            // A later boot record replaces the image table of an earlier one.
            Some(0) if descriptor.get(7..39) == Some(EL_TORITO_ID.as_slice()) => {
                let catalog_sector = le_u32(&descriptor, 0x47).unwrap_or(0);
                el_torito = Some(parse_boot_catalog(source, catalog_sector)?);
            }
            Some(2) => {
                // Only the first Joliet descriptor is kept; until one is found,
                // each supplementary descriptor replaces the previous one.
                if joliet_level == 0 {
                    svd = Some(descriptor.clone());
                }
                if descriptor.get(88..90) == Some(b"%/".as_slice()) {
                    match descriptor.get(90) {
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

    let pvd_volume_raw = slice(&pvd, 40, 32).unwrap_or_default();
    let joliet_volume_raw = svd
        .as_deref()
        .and_then(|svd| slice(svd, 40, 32))
        .filter(|_| joliet_level > 0);
    let label = member_id(pvd_volume_raw, joliet_volume_raw);
    Ok(Some(Iso9660Report {
        system_id: trimmed_ascii(slice(&pvd, 8, 32).unwrap_or_default()),
        pvd_volume_id: trimmed_ascii(pvd_volume_raw),
        joliet_level,
        joliet_volume_id: joliet_volume_raw
            .map(|raw| decode_ucs2be(raw).trim_end_matches(' ').to_string()),
        label,
        volume_space_blocks: le_u32(&pvd, 80).unwrap_or(0),
        logical_block_size: le_u16(&pvd, 128).unwrap_or(0),
        publisher_id: trimmed_ascii(slice(&pvd, 318, 128).unwrap_or_default()),
        application_id: trimmed_ascii(slice(&pvd, 574, 128).unwrap_or_default()),
        el_torito,
    }))
}

/// `get_member_id()`: the Joliet string is used if it is not a prefix of the
/// (space-padded) PVD string and is not empty once trailing spaces are
/// removed; otherwise the PVD string without trailing spaces.
fn member_id(pvd_raw: &[u8], joliet_raw: Option<&[u8]>) -> Option<String> {
    if let Some(joliet_raw) = joliet_raw {
        let joliet = decode_ucs2be(joliet_raw);
        // strncmp(joliet, pvd, strlen(joliet)) on the untrimmed strings.
        let joliet_bytes = joliet.as_bytes();
        let same_prefix = pvd_raw.get(..joliet_bytes.len()) == Some(joliet_bytes);
        if !same_prefix {
            let trimmed = joliet.trim_end_matches(' ');
            if !trimmed.is_empty() {
                return Some(trimmed.to_string());
            }
        }
    }
    // Copy the PVD field while removing trailing spaces; an empty result is
    // a failure (no label).
    let pvd = trimmed_ascii(pvd_raw);
    (!pvd.is_empty()).then_some(pvd)
}

/// libcdio's boot image table: every 32-byte catalog entry with boot
/// indicator 0x88 and an emulation type up to 4, at most eight. The platform
/// is taken from the validation entry or the enclosing section header.
fn parse_boot_catalog(source: &dyn ByteSource, catalog_sector: u32) -> Result<ElTorito> {
    let mut images = Vec::new();
    if let Some(catalog) = read_sector(source, u64::from(catalog_sector))? {
        let mut platform = None;
        for entry in catalog.as_chunks::<32>().0 {
            match entry.first().copied() {
                // Validation entry: header id 1, key bytes 0x55 0xAA.
                Some(0x01) if entry.get(30..32) == Some([0x55, 0xaa].as_slice()) => {
                    platform = entry.get(1).copied().map(Platform::from_id);
                }
                // Section header (0x91 marks the last one).
                Some(0x90 | 0x91) => platform = entry.get(1).copied().map(Platform::from_id),
                Some(0x88) if images.len() < MAX_BOOT_IMAGES => {
                    let media_type = entry.get(1).copied().unwrap_or(0xff);
                    if let Some(media) =
                        BootMedia::from_type(media_type).filter(|_| media_type & 0x0f <= 4)
                    {
                        images.push(BootImage {
                            media,
                            platform,
                            lsn: le_u32(entry, 8).unwrap_or(0),
                            virtual_sectors: le_u16(entry, 6).unwrap_or(0),
                        });
                    }
                }
                _ => {}
            }
        }
    }
    Ok(ElTorito {
        catalog_sector,
        images,
    })
}

fn read_sector(source: &dyn ByteSource, sector: u64) -> Result<Option<Vec<u8>>> {
    let Some(offset) = sector.checked_mul(ISO_BLOCKSIZE) else {
        return Ok(None);
    };
    read_exact_at(source, offset, 2048)
}

/// An ISO 9660 a/d-character field read the way libcdio does: trailing
/// spaces are removed from the whole field, then the result is read as a C
/// string (up to the first NUL). Non-ASCII bytes become U+FFFD.
fn trimmed_ascii(raw: &[u8]) -> String {
    let end = raw
        .iter()
        .rposition(|&b| b != b' ')
        .map_or(0, |last| last.saturating_add(1));
    let field = raw.get(..end).unwrap_or_default();
    let field = field.split(|&b| b == 0).next().unwrap_or_default();
    field
        .iter()
        .map(|&b| {
            if b.is_ascii() {
                char::from(b)
            } else {
                '\u{fffd}'
            }
        })
        .collect()
}

/// UCS-2 big-endian text, stopping at the first NUL.
pub(crate) fn decode_ucs2be(raw: &[u8]) -> String {
    let units: Vec<u16> = raw
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| u16::from_be_bytes(*pair))
        .take_while(|&unit| unit != 0)
        .collect();
    String::from_utf16_lossy(&units)
}

#[cfg(test)]
#[allow(clippy::arithmetic_side_effects, clippy::cast_possible_truncation)] // Fixture arithmetic.
mod tests {
    use super::*;

    fn padded(text: &str, len: usize) -> Vec<u8> {
        let mut field = vec![b' '; len];
        field[..text.len()].copy_from_slice(text.as_bytes());
        field
    }

    fn ucs2(text: &str, len: usize) -> Vec<u8> {
        let mut field = Vec::with_capacity(len);
        for unit in text.encode_utf16() {
            field.extend_from_slice(&unit.to_be_bytes());
        }
        while field.len() < len {
            field.extend_from_slice(&[0, b' ']);
        }
        field.truncate(len);
        field
    }

    /// A minimal ISO 9660 image with an optional Joliet descriptor and boot record.
    struct IsoBuilder {
        image: Vec<u8>,
        next: usize,
    }

    impl IsoBuilder {
        fn new(volume_id: &str) -> Self {
            let mut image = vec![0u8; 2048 * 40];
            let pvd = &mut image[16 * 2048..17 * 2048];
            pvd[0] = 1;
            pvd[1..6].copy_from_slice(b"CD001");
            pvd[6] = 1;
            pvd[8..40].copy_from_slice(&padded("LINUX", 32));
            pvd[40..72].copy_from_slice(&padded(volume_id, 32));
            pvd[80..84].copy_from_slice(&40u32.to_le_bytes());
            pvd[84..88].copy_from_slice(&40u32.to_be_bytes());
            pvd[128..130].copy_from_slice(&2048u16.to_le_bytes());
            pvd[130..132].copy_from_slice(&2048u16.to_be_bytes());
            Self { image, next: 17 }
        }

        fn descriptor(&mut self) -> &mut [u8] {
            let sector = self.next;
            self.next += 1;
            &mut self.image[sector * 2048..(sector + 1) * 2048]
        }

        fn joliet(mut self, volume_id: &str, level_byte: u8) -> Self {
            let svd = self.descriptor();
            svd[0] = 2;
            svd[1..6].copy_from_slice(b"CD001");
            svd[40..72].copy_from_slice(&ucs2(volume_id, 32));
            svd[88..91].copy_from_slice(&[b'%', b'/', level_byte]);
            self
        }

        fn boot_record(mut self, catalog_sector: u32, catalog: &[u8]) -> Self {
            let brvd = self.descriptor();
            brvd[0] = 0;
            brvd[1..6].copy_from_slice(b"CD001");
            brvd[7..39].copy_from_slice(EL_TORITO_ID);
            brvd[0x47..0x4b].copy_from_slice(&catalog_sector.to_le_bytes());
            let start = catalog_sector as usize * 2048;
            self.image[start..start + catalog.len()].copy_from_slice(catalog);
            self
        }

        fn build(mut self) -> Vec<u8> {
            let terminator = self.descriptor();
            terminator[0] = 255;
            terminator[1..6].copy_from_slice(b"CD001");
            self.image
        }
    }

    #[test]
    fn plain_iso_label() {
        let report = parse(&IsoBuilder::new("UBUNTU 24_04").build())
            .unwrap()
            .unwrap();
        assert_eq!(report.pvd_volume_id, "UBUNTU 24_04");
        assert_eq!(report.joliet_level, 0);
        assert_eq!(report.label.as_deref(), Some("UBUNTU 24_04"));
        assert_eq!(report.system_id, "LINUX");
        assert_eq!(report.volume_bytes(), Some(40 * 2048));
    }

    #[test]
    fn joliet_label_is_preferred_when_different() {
        let image = IsoBuilder::new("CCCOMA_X64FRE_EN-US_DV9")
            .joliet("Win11 Install", 0x45)
            .build();
        let report = parse(&image).unwrap().unwrap();
        assert_eq!(report.joliet_level, 3);
        assert_eq!(report.joliet_volume_id.as_deref(), Some("Win11 Install"));
        assert_eq!(report.label.as_deref(), Some("Win11 Install"));
    }

    #[test]
    fn identical_joliet_label_falls_back_to_the_pvd() {
        // Joliet can only hold 16 characters; when it is a prefix of the PVD
        // identifier, the (possibly longer) PVD identifier is used.
        let image = IsoBuilder::new("ARCH_202610_LONG_LABEL")
            .joliet("ARCH_202610_LONG", 0x40)
            .build();
        let report = parse(&image).unwrap().unwrap();
        assert_eq!(report.joliet_level, 1);
        assert_eq!(report.label.as_deref(), Some("ARCH_202610_LONG_LABEL"));
    }

    #[test]
    fn blank_joliet_label_falls_back_to_the_pvd() {
        let image = IsoBuilder::new("DEBIAN").joliet("", 0x43).build();
        let report = parse(&image).unwrap().unwrap();
        assert_eq!(report.label.as_deref(), Some("DEBIAN"));
    }

    #[test]
    fn pvd_fields_follow_c_string_rules() {
        // Trailing spaces are stripped over the whole field before the NUL
        // terminates the string, so the space before the NULs is kept.
        let mut field = vec![0u8; 32];
        field[..2].copy_from_slice(b"A ");
        assert_eq!(trimmed_ascii(&field), "A ");
        let mut field = padded("", 32);
        field[..5].copy_from_slice(b"AB\0CD");
        assert_eq!(trimmed_ascii(&field), "AB");
        assert_eq!(trimmed_ascii(&padded("LABEL", 32)), "LABEL");
    }

    #[test]
    fn empty_labels_are_none() {
        let report = parse(&IsoBuilder::new("").build()).unwrap().unwrap();
        assert_eq!(report.label, None);
    }

    #[test]
    fn non_joliet_supplementary_descriptors_are_ignored() {
        let image = IsoBuilder::new("PVD").joliet("OTHER", b'Z').build();
        let report = parse(&image).unwrap().unwrap();
        assert_eq!(report.joliet_level, 0);
        assert_eq!(report.joliet_volume_id, None);
        assert_eq!(report.label.as_deref(), Some("PVD"));
    }

    fn catalog() -> Vec<u8> {
        let mut catalog = vec![0u8; 2048];
        // Validation entry for x86.
        catalog[0] = 0x01;
        catalog[1] = 0x00;
        catalog[30] = 0x55;
        catalog[31] = 0xaa;
        // Default entry: bootable, no emulation, 4 virtual sectors at LSN 30.
        catalog[32] = 0x88;
        catalog[32 + 6..32 + 8].copy_from_slice(&4u16.to_le_bytes());
        catalog[32 + 8..32 + 12].copy_from_slice(&30u32.to_le_bytes());
        // Final section header for EFI with one entry.
        catalog[64] = 0x91;
        catalog[65] = 0xef;
        catalog[96] = 0x88;
        catalog[96 + 6..96 + 8].copy_from_slice(&5760u16.to_le_bytes());
        catalog[96 + 8..96 + 12].copy_from_slice(&31u32.to_le_bytes());
        // A non-bootable entry is not listed.
        catalog[128] = 0x00;
        catalog
    }

    #[test]
    fn el_torito_images() {
        let image = IsoBuilder::new("HYBRID")
            .boot_record(20, &catalog())
            .joliet("Hybrid", 0x45)
            .build();
        let report = parse(&image).unwrap().unwrap();
        let el_torito = report.el_torito.unwrap();
        assert_eq!(el_torito.catalog_sector, 20);
        assert_eq!(el_torito.images.len(), 2);
        assert_eq!(el_torito.images[0].platform, Some(Platform::X86));
        assert_eq!(el_torito.images[0].media, BootMedia::NoEmul);
        assert_eq!(
            (el_torito.images[0].lsn, el_torito.images[0].virtual_sectors),
            (30, 4)
        );
        assert_eq!(el_torito.images[1].platform, Some(Platform::Efi));
        assert!(el_torito.has_platform(Platform::Efi));
        assert_eq!(report.joliet_level, 3);
    }

    #[test]
    fn non_iso_images() {
        assert_eq!(parse(&vec![0u8; 64 * 1024]).unwrap(), None);
        assert_eq!(parse(&vec![0u8; 1000]).unwrap(), None);
        let mut image = IsoBuilder::new("X").build();
        image[16 * 2048 + 1] = b'X';
        assert_eq!(parse(&image).unwrap(), None);
    }

    #[test]
    fn missing_terminator_and_truncation_are_handled() {
        // No terminator: the scan stops at the end of the image.
        let mut image = IsoBuilder::new("NOTERM").build();
        image.truncate(17 * 2048 + 100);
        let report = parse(&image).unwrap().unwrap();
        assert_eq!(report.label.as_deref(), Some("NOTERM"));
    }
}
