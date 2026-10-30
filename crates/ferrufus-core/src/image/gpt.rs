// SPDX-License-Identifier: GPL-3.0-or-later
//! GUID partition tables in images.
//!
//! Rufus does not parse an image's GPT before writing it; Ferrufus reports it
//! so that the dry-run plan can show what the image contains. The header is
//! looked for at LBA 1 for 512-byte and 4096-byte sectors, as bled does when
//! sizing compressed disk images (`src/bled/bled_size.c`).

#![deny(clippy::indexing_slicing, clippy::arithmetic_side_effects)]

use super::io::{ByteSource, le_u32, le_u64, read_exact_at, slice};
use crate::Result;
use serde::Serialize;

/// Largest partition entry array that will be read.
const MAX_ENTRIES_BYTES: usize = 1024 * 1024;

/// A parsed GPT header and its partition entries.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GptReport {
    /// Logical sector size the header was found with (512 or 4096).
    pub sector_size: u32,
    /// Whether the header CRC32 is correct.
    pub header_crc_valid: bool,
    /// Whether the partition entry array CRC32 is correct (`None` if the
    /// array could not be read).
    pub entries_crc_valid: Option<bool>,
    /// Disk GUID.
    pub disk_guid: String,
    /// LBA of the backup header.
    pub alternate_lba: u64,
    /// Last LBA usable by partitions.
    pub last_usable_lba: u64,
    /// Used partition entries.
    pub partitions: Vec<GptPartition>,
    /// Problems found while parsing.
    pub warnings: Vec<String>,
}

/// One used GPT partition entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GptPartition {
    /// Entry number, starting at 1.
    pub number: u32,
    /// Partition type GUID.
    pub type_guid: String,
    /// A name for the type, when known.
    pub type_name: Option<&'static str>,
    /// Unique partition GUID.
    pub unique_guid: String,
    /// First LBA.
    pub first_lba: u64,
    /// Last LBA (inclusive).
    pub last_lba: u64,
    /// Partition name.
    pub name: String,
}

impl GptReport {
    /// End of the disk area the GPT describes, in bytes (the backup header's
    /// LBA plus one, times the sector size), if it does not overflow.
    pub fn disk_bytes(&self) -> Option<u64> {
        self.alternate_lba
            .checked_add(1)?
            .checked_mul(u64::from(self.sector_size))
    }
}

/// Looks for a GPT header at LBA 1 for 512-byte, then 4096-byte sectors.
pub fn parse(source: &dyn ByteSource) -> Result<Option<GptReport>> {
    for sector_size in [512u32, 4096] {
        let Some(header) = read_exact_at(source, u64::from(sector_size), 512)? else {
            continue;
        };
        if header.get(..8) == Some(b"EFI PART".as_slice()) {
            return parse_at(source, sector_size, &header).map(Some);
        }
    }
    Ok(None)
}

fn parse_at(source: &dyn ByteSource, sector_size: u32, header: &[u8]) -> Result<GptReport> {
    let mut warnings = Vec::new();
    let header_size = le_u32(header, 12).unwrap_or(0);
    let header_crc_valid = match usize::try_from(header_size)
        .ok()
        .filter(|size| (92..=512).contains(size))
    {
        Some(size) => {
            let stored = le_u32(header, 16).unwrap_or(0);
            let mut copy = slice(header, 0, size).unwrap_or_default().to_vec();
            if let Some(field) = copy.get_mut(16..20) {
                field.fill(0);
            }
            crc32fast::hash(&copy) == stored
        }
        None => {
            warnings.push(format!("invalid GPT header size {header_size}"));
            false
        }
    };
    if !header_crc_valid {
        warnings.push("GPT header CRC32 mismatch".to_string());
    }

    let alternate_lba = le_u64(header, 32).unwrap_or(0);
    let last_usable_lba = le_u64(header, 48).unwrap_or(0);
    let disk_guid = slice(header, 56, 16).map(format_guid).unwrap_or_default();
    let entries_lba = le_u64(header, 72).unwrap_or(0);
    let entry_count = le_u32(header, 80).unwrap_or(0);
    let entry_size = le_u32(header, 84).unwrap_or(0);
    let entries_crc = le_u32(header, 88).unwrap_or(0);

    let mut partitions = Vec::new();
    let mut entries_crc_valid = None;
    let array_len = usize::try_from(entry_count)
        .ok()
        .zip(usize::try_from(entry_size).ok())
        .and_then(|(count, size)| count.checked_mul(size));
    let array_offset = entries_lba.checked_mul(u64::from(sector_size));
    match (array_len, array_offset) {
        _ if entry_size < 128 || !entry_size.is_multiple_of(8) => {
            warnings.push(format!("invalid GPT partition entry size {entry_size}"));
        }
        (Some(len), Some(offset)) if len <= MAX_ENTRIES_BYTES => {
            match read_exact_at(source, offset, len)? {
                Some(array) => {
                    entries_crc_valid = Some(crc32fast::hash(&array) == entries_crc);
                    if entries_crc_valid == Some(false) {
                        warnings.push("GPT partition entry array CRC32 mismatch".to_string());
                    }
                    let entry_size = usize::try_from(entry_size).unwrap_or(128);
                    for (number, entry) in (1u32..).zip(array.chunks_exact(entry_size)) {
                        if let Some(partition) = parse_entry(number, entry) {
                            partitions.push(partition);
                        }
                    }
                }
                None => warnings
                    .push("GPT partition entry array lies beyond the end of the image".to_string()),
            }
        }
        _ => warnings.push(format!(
            "GPT partition entry array too large ({entry_count} × {entry_size} bytes)"
        )),
    }

    Ok(GptReport {
        sector_size,
        header_crc_valid,
        entries_crc_valid,
        disk_guid,
        alternate_lba,
        last_usable_lba,
        partitions,
        warnings,
    })
}

fn parse_entry(number: u32, entry: &[u8]) -> Option<GptPartition> {
    let type_bytes = slice(entry, 0, 16)?;
    if type_bytes.iter().all(|&b| b == 0) {
        return None;
    }
    let type_guid = format_guid(type_bytes);
    let name_units: Vec<u16> = slice(entry, 56, 72)?
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| u16::from_le_bytes(*pair))
        .take_while(|&unit| unit != 0)
        .collect();
    Some(GptPartition {
        number,
        type_name: type_name(&type_guid),
        type_guid,
        unique_guid: format_guid(slice(entry, 16, 16)?),
        first_lba: le_u64(entry, 32)?,
        last_lba: le_u64(entry, 40)?,
        name: String::from_utf16_lossy(&name_units),
    })
}

/// Formats a GUID stored in the mixed-endian on-disk layout.
pub fn format_guid(bytes: &[u8]) -> String {
    let b = |i: usize| bytes.get(i).copied().unwrap_or(0);
    format!(
        "{:02X}{:02X}{:02X}{:02X}-{:02X}{:02X}-{:02X}{:02X}-{:02X}{:02X}-{:02X}{:02X}{:02X}{:02X}{:02X}{:02X}",
        b(3),
        b(2),
        b(1),
        b(0),
        b(5),
        b(4),
        b(7),
        b(6),
        b(8),
        b(9),
        b(10),
        b(11),
        b(12),
        b(13),
        b(14),
        b(15)
    )
}

/// Names of common GPT partition types.
fn type_name(guid: &str) -> Option<&'static str> {
    Some(match guid {
        "C12A7328-F81F-11D2-BA4B-00A0C93EC93B" => "EFI System",
        "21686148-6449-6E6F-744E-656564454649" => "BIOS boot",
        "EBD0A0A2-B9E5-4433-87C0-68B6B72699C7" => "Microsoft basic data",
        "E3C9E316-0B5C-4DB8-817D-F92DF00215AE" => "Microsoft reserved",
        "DE94BBA4-06D1-4D40-A16A-BFD50179D6AC" => "Windows recovery",
        "0FC63DAF-8483-4772-8E79-3D69D8477DE4" => "Linux filesystem",
        "0657FD6D-A4AB-43C4-84E5-0933C84B4F4F" => "Linux swap",
        "E6D6D379-F507-44C2-A23C-238F2A3DF928" => "Linux LVM",
        "A19D880F-05FC-4D3B-A006-743F0F84911E" => "Linux RAID",
        "4F68BCE3-E8CD-4DB1-96E7-FBCAF984B709" => "Linux root (x86-64)",
        "BC13C2FF-59E6-4262-A352-B275FD6F7172" => "Linux extended boot",
        "48465300-0000-11AA-AA11-00306543ECAC" => "Apple HFS+",
        "7C3457EF-0000-11AA-AA11-00306543ECAC" => "Apple APFS",
        "516E7CB4-6ECF-11D6-8FF8-00022D09712B" => "FreeBSD",
        "FE3A2A5D-4F32-41A7-B725-ACCC3285A309" => "ChromeOS kernel",
        "3CB8E202-3B7E-47DD-8A3C-7FF2A13CFCEC" => "ChromeOS root",
        _ => return None,
    })
}

#[cfg(test)]
#[allow(clippy::arithmetic_side_effects, clippy::cast_possible_truncation)] // Fixture arithmetic.
mod tests {
    use super::*;

    const ESP: [u8; 16] = [
        0x28, 0x73, 0x2a, 0xc1, 0x1f, 0xf8, 0xd2, 0x11, 0xba, 0x4b, 0x00, 0xa0, 0xc9, 0x3e, 0xc9,
        0x3b,
    ];

    /// Builds a minimal GPT disk image with one ESP entry.
    fn gpt_image(sector_size: usize, corrupt_header: bool) -> Vec<u8> {
        let entry_count = 128usize;
        let mut image = vec![0u8; sector_size * 40];
        let mut entries = vec![0u8; entry_count * 128];
        entries[..16].copy_from_slice(&ESP);
        entries[16] = 0x11;
        entries[32..40].copy_from_slice(&34u64.to_le_bytes());
        entries[40..48].copy_from_slice(&1000u64.to_le_bytes());
        for (i, unit) in "EFI".encode_utf16().enumerate() {
            entries[56 + 2 * i..58 + 2 * i].copy_from_slice(&unit.to_le_bytes());
        }
        let entries_lba = 2usize;
        image[entries_lba * sector_size..entries_lba * sector_size + entries.len()]
            .copy_from_slice(&entries);

        let mut header = vec![0u8; 92];
        header[..8].copy_from_slice(b"EFI PART");
        header[8..12].copy_from_slice(&0x0001_0000u32.to_le_bytes());
        header[12..16].copy_from_slice(&92u32.to_le_bytes());
        header[24..32].copy_from_slice(&1u64.to_le_bytes());
        header[32..40].copy_from_slice(&999_999u64.to_le_bytes());
        header[48..56].copy_from_slice(&999_966u64.to_le_bytes());
        header[56] = 0xab;
        header[72..80].copy_from_slice(&(entries_lba as u64).to_le_bytes());
        header[80..84].copy_from_slice(&(entry_count as u32).to_le_bytes());
        header[84..88].copy_from_slice(&128u32.to_le_bytes());
        header[88..92].copy_from_slice(&crc32fast::hash(&entries).to_le_bytes());
        let crc = crc32fast::hash(&header);
        header[16..20].copy_from_slice(&crc.to_le_bytes());
        if corrupt_header {
            header[60] ^= 1;
        }
        image[sector_size..sector_size + 92].copy_from_slice(&header);
        image
    }

    #[test]
    fn parses_512_and_4096_byte_sector_tables() {
        for sector_size in [512, 4096] {
            let report = parse(&gpt_image(sector_size, false)).unwrap().unwrap();
            assert_eq!(report.sector_size, sector_size as u32);
            assert!(report.header_crc_valid);
            assert_eq!(report.entries_crc_valid, Some(true));
            assert!(report.warnings.is_empty(), "{:?}", report.warnings);
            assert_eq!(report.partitions.len(), 1);
            let esp = &report.partitions[0];
            assert_eq!(esp.type_guid, "C12A7328-F81F-11D2-BA4B-00A0C93EC93B");
            assert_eq!(esp.type_name, Some("EFI System"));
            assert_eq!(esp.name, "EFI");
            assert_eq!((esp.first_lba, esp.last_lba), (34, 1000));
            assert_eq!(report.disk_bytes(), Some(1_000_000 * sector_size as u64));
        }
    }

    #[test]
    fn corrupted_header_is_reported() {
        let report = parse(&gpt_image(512, true)).unwrap().unwrap();
        assert!(!report.header_crc_valid);
        assert!(report.warnings.iter().any(|w| w.contains("header CRC32")));
    }

    #[test]
    fn no_gpt() {
        assert_eq!(parse(&vec![0u8; 8192]).unwrap(), None);
        assert_eq!(parse(&vec![0u8; 10]).unwrap(), None);
    }

    #[test]
    fn absurd_entry_arrays_are_not_read() {
        let mut image = gpt_image(512, false);
        image[512 + 80..512 + 84].copy_from_slice(&u32::MAX.to_le_bytes());
        let report = parse(&image).unwrap().unwrap();
        assert!(report.partitions.is_empty());
        assert!(report.warnings.iter().any(|w| w.contains("too large")));
    }
}
