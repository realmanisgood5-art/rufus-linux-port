// SPDX-License-Identifier: GPL-3.0-or-later
//! Synthetic disk images: MBR, GPT, ISO 9660 (with Joliet and El Torito),
//! UDF, VHD, WIM and FFU headers. Images are built in memory; nothing here
//! touches a device.

use std::path::{Path, PathBuf};

/// ISO 9660 and UDF block size.
pub const BLOCK: usize = 2048;

/// Sets the 0x55 0xAA boot marker of a sector.
pub fn set_boot_marker(sector: &mut [u8]) {
    sector[0x1fe] = 0x55;
    sector[0x1ff] = 0xaa;
}

/// Writes MBR partition entry `index` (0 to 3).
pub fn set_mbr_partition(
    sector: &mut [u8],
    index: usize,
    active: bool,
    type_id: u8,
    start_lba: u32,
    sectors: u32,
) {
    let base = 0x1be + 16 * index;
    sector[base] = if active { 0x80 } else { 0 };
    sector[base + 4] = type_id;
    sector[base + 8..base + 12].copy_from_slice(&start_lba.to_le_bytes());
    sector[base + 12..base + 16].copy_from_slice(&sectors.to_le_bytes());
}

/// A raw disk image of `size` bytes starting with `first_sector`.
pub fn disk_image(first_sector: &[u8; 512], size: usize) -> Vec<u8> {
    let mut image = vec![0u8; size.max(512)];
    image[..512].copy_from_slice(first_sector);
    image.truncate(size);
    image
}

fn padded(text: &str, len: usize) -> Vec<u8> {
    let mut field = vec![b' '; len];
    field[..text.len()].copy_from_slice(text.as_bytes());
    field
}

fn ucs2be(text: &str, len: usize) -> Vec<u8> {
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

/// Block of the root directory of [`IsoBuilder`] images.
const ROOT_DIR: usize = 20;

/// A 34-byte directory record for the root directory (`.`).
fn root_record() -> [u8; 34] {
    let mut rec = [0u8; 34];
    rec[0] = 34;
    let lba = u32::try_from(ROOT_DIR).unwrap();
    rec[2..6].copy_from_slice(&lba.to_le_bytes());
    rec[6..10].copy_from_slice(&lba.to_be_bytes());
    rec[10..14].copy_from_slice(&2048u32.to_le_bytes());
    rec[14..18].copy_from_slice(&2048u32.to_be_bytes());
    rec[25] = 2;
    rec[28..32].copy_from_slice(&[1, 0, 0, 1]);
    rec[32] = 1;
    rec
}

/// An El Torito boot entry: platform id (0 = BIOS, 0xEF = UEFI), image LSN
/// and size in 512-byte sectors.
#[derive(Debug, Clone, Copy)]
pub struct BootEntry {
    /// Platform identifier.
    pub platform: u8,
    /// First sector of the boot image.
    pub lsn: u32,
    /// Size in 512-byte virtual sectors.
    pub sectors: u16,
}

/// Builds ISO 9660 images.
#[derive(Debug, Clone)]
pub struct IsoBuilder {
    volume_id: String,
    joliet: Option<String>,
    boot: Vec<BootEntry>,
    hybrid_mbr: Option<[u8; 512]>,
    blocks: usize,
}

impl IsoBuilder {
    /// An ISO with the given PVD volume identifier.
    pub fn new(volume_id: &str) -> Self {
        Self {
            volume_id: volume_id.to_string(),
            joliet: None,
            boot: Vec::new(),
            hybrid_mbr: None,
            blocks: 64,
        }
    }

    /// Adds a Joliet (level 3) supplementary volume descriptor.
    pub fn joliet(mut self, volume_id: &str) -> Self {
        self.joliet = Some(volume_id.to_string());
        self
    }

    /// Adds an El Torito boot catalog; the first entry is the default entry,
    /// the others each get their own section.
    pub fn el_torito(mut self, entries: &[BootEntry]) -> Self {
        self.boot = entries.to_vec();
        self
    }

    /// Makes the image an ISOHybrid by writing `mbr` to its first sector.
    pub fn hybrid(mut self, mbr: [u8; 512]) -> Self {
        self.hybrid_mbr = Some(mbr);
        self
    }

    /// Sets the image size in 2048-byte blocks (at least 40).
    pub fn blocks(mut self, blocks: usize) -> Self {
        self.blocks = blocks.max(40);
        self
    }

    /// Builds the image.
    pub fn build(&self) -> Vec<u8> {
        let mut image = vec![0u8; self.blocks * BLOCK];
        let blocks = u32::try_from(self.blocks).unwrap();
        {
            let pvd = &mut image[16 * BLOCK..17 * BLOCK];
            pvd[0] = 1;
            pvd[1..6].copy_from_slice(b"CD001");
            pvd[6] = 1;
            pvd[8..40].copy_from_slice(&padded("LINUX", 32));
            pvd[40..72].copy_from_slice(&padded(&self.volume_id, 32));
            pvd[80..84].copy_from_slice(&blocks.to_le_bytes());
            pvd[84..88].copy_from_slice(&blocks.to_be_bytes());
            pvd[128..130].copy_from_slice(&2048u16.to_le_bytes());
            pvd[130..132].copy_from_slice(&2048u16.to_be_bytes());
            pvd[156..190].copy_from_slice(&root_record());
        }
        // An empty root directory ("." and "..") at block 20.
        let root = &mut image[ROOT_DIR * BLOCK..(ROOT_DIR + 1) * BLOCK];
        let mut dot = root_record();
        root[..34].copy_from_slice(&dot);
        dot[33] = 1;
        root[34..68].copy_from_slice(&dot);
        let mut next = 17;
        if !self.boot.is_empty() {
            let catalog_sector = 30usize;
            let brvd = &mut image[next * BLOCK..(next + 1) * BLOCK];
            next += 1;
            brvd[1..6].copy_from_slice(b"CD001");
            brvd[6] = 1;
            brvd[7..30].copy_from_slice(b"EL TORITO SPECIFICATION");
            brvd[0x47..0x4b].copy_from_slice(&u32::try_from(catalog_sector).unwrap().to_le_bytes());
            let catalog = &mut image[catalog_sector * BLOCK..(catalog_sector + 1) * BLOCK];
            catalog[0] = 0x01;
            catalog[1] = self.boot[0].platform;
            catalog[30] = 0x55;
            catalog[31] = 0xaa;
            let mut offset = 32;
            for (index, entry) in self.boot.iter().enumerate() {
                if index > 0 {
                    catalog[offset] = if index + 1 == self.boot.len() {
                        0x91
                    } else {
                        0x90
                    };
                    catalog[offset + 1] = entry.platform;
                    catalog[offset + 2..offset + 4].copy_from_slice(&1u16.to_le_bytes());
                    offset += 32;
                }
                catalog[offset] = 0x88;
                catalog[offset + 6..offset + 8].copy_from_slice(&entry.sectors.to_le_bytes());
                catalog[offset + 8..offset + 12].copy_from_slice(&entry.lsn.to_le_bytes());
                offset += 32;
            }
        }
        if let Some(volume_id) = &self.joliet {
            let svd = &mut image[next * BLOCK..(next + 1) * BLOCK];
            next += 1;
            svd[0] = 2;
            svd[1..6].copy_from_slice(b"CD001");
            svd[6] = 1;
            svd[40..72].copy_from_slice(&ucs2be(volume_id, 32));
            svd[88..91].copy_from_slice(b"%/E");
            svd[156..190].copy_from_slice(&root_record());
        }
        let terminator = &mut image[next * BLOCK..(next + 1) * BLOCK];
        terminator[0] = 255;
        terminator[1..6].copy_from_slice(b"CD001");
        terminator[6] = 1;
        if let Some(mbr) = &self.hybrid_mbr {
            image[..512].copy_from_slice(mbr);
        }
        image
    }
}

fn set_udf_tag(block: &mut [u8], id: u16, location: u32) {
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

/// Adds a UDF volume (anchor at block 256, descriptors at 32 to 34, a
/// partition at block 300 with the file set and root entry) to `image`,
/// growing it as needed. Use with an [`IsoBuilder`] image to make a UDF
/// bridge disc like Windows installation media.
pub fn add_udf(image: &mut Vec<u8>, logical_volume_id: &str) {
    if image.len() < 310 * BLOCK {
        image.resize(310 * BLOCK, 0);
    }
    let block = |n: usize| n * BLOCK..(n + 1) * BLOCK;
    {
        let anchor = &mut image[block(256)];
        anchor[16..20].copy_from_slice(&(16 * 2048u32).to_le_bytes());
        anchor[20..24].copy_from_slice(&32u32.to_le_bytes());
        set_udf_tag(anchor, 2, 256);
    }
    set_udf_tag(&mut image[block(32)], 1, 32);
    {
        let pd = &mut image[block(33)];
        pd[188..192].copy_from_slice(&300u32.to_le_bytes());
        set_udf_tag(pd, 5, 33);
    }
    {
        let lvd = &mut image[block(34)];
        lvd[84] = 8;
        lvd[85..85 + logical_volume_id.len()].copy_from_slice(logical_volume_id.as_bytes());
        lvd[84 + 127] = u8::try_from(logical_volume_id.len() + 1).unwrap();
        lvd[212..216].copy_from_slice(&2048u32.to_le_bytes());
        set_udf_tag(lvd, 6, 34);
    }
    {
        let fsd = &mut image[block(300)];
        fsd[404..408].copy_from_slice(&2u32.to_le_bytes());
        set_udf_tag(fsd, 0x100, 0);
    }
    set_udf_tag(&mut image[block(302)], 0x105, 2);
}

/// A GPT partition for [`gpt_disk`].
#[derive(Debug, Clone)]
pub struct GptPartition {
    /// Type GUID in on-disk (mixed-endian) byte order.
    pub type_guid: [u8; 16],
    /// First LBA.
    pub first_lba: u64,
    /// Last LBA, inclusive.
    pub last_lba: u64,
    /// Partition name.
    pub name: String,
}

/// The EFI System partition type GUID, in on-disk byte order.
pub const ESP_TYPE: [u8; 16] = [
    0x28, 0x73, 0x2a, 0xc1, 0x1f, 0xf8, 0xd2, 0x11, 0xba, 0x4b, 0x00, 0xa0, 0xc9, 0x3e, 0xc9, 0x3b,
];
/// The Microsoft basic data partition type GUID, in on-disk byte order.
pub const BASIC_DATA_TYPE: [u8; 16] = [
    0xa2, 0xa0, 0xd0, 0xeb, 0xe5, 0xb9, 0x33, 0x44, 0x87, 0xc0, 0x68, 0xb6, 0xb7, 0x26, 0x99, 0xc7,
];

/// A GPT disk image of `sectors` sectors with a protective MBR (using
/// `boot_code` for the first 440 bytes) and the given partitions.
pub fn gpt_disk(
    sector_size: usize,
    sectors: u64,
    boot_code: &[u8; 512],
    partitions: &[GptPartition],
) -> Vec<u8> {
    let size = usize::try_from(sectors).unwrap() * sector_size;
    let mut image = vec![0u8; size];
    image[..440].copy_from_slice(&boot_code[..440]);
    let total = u32::try_from((sectors - 1).min(u64::from(u32::MAX))).unwrap();
    set_mbr_partition(&mut image[..512], 0, false, 0xee, 1, total);
    set_boot_marker(&mut image[..512]);

    let mut entries = vec![0u8; 128 * 128];
    for (index, partition) in partitions.iter().enumerate() {
        let entry = &mut entries[index * 128..(index + 1) * 128];
        entry[..16].copy_from_slice(&partition.type_guid);
        entry[16] = u8::try_from(index + 1).unwrap();
        entry[32..40].copy_from_slice(&partition.first_lba.to_le_bytes());
        entry[40..48].copy_from_slice(&partition.last_lba.to_le_bytes());
        for (i, unit) in partition.name.encode_utf16().take(36).enumerate() {
            entry[56 + 2 * i..58 + 2 * i].copy_from_slice(&unit.to_le_bytes());
        }
    }
    let entries_lba = 2u64;
    let start = usize::try_from(entries_lba).unwrap() * sector_size;
    image[start..start + entries.len()].copy_from_slice(&entries);

    let mut header = vec![0u8; 92];
    header[..8].copy_from_slice(b"EFI PART");
    header[8..12].copy_from_slice(&0x0001_0000u32.to_le_bytes());
    header[12..16].copy_from_slice(&92u32.to_le_bytes());
    header[24..32].copy_from_slice(&1u64.to_le_bytes());
    header[32..40].copy_from_slice(&(sectors - 1).to_le_bytes());
    header[40..48].copy_from_slice(&34u64.to_le_bytes());
    header[48..56].copy_from_slice(&(sectors - 34).to_le_bytes());
    header[56..72].copy_from_slice(&[0x5a; 16]);
    header[72..80].copy_from_slice(&entries_lba.to_le_bytes());
    header[80..84].copy_from_slice(&128u32.to_le_bytes());
    header[84..88].copy_from_slice(&128u32.to_le_bytes());
    header[88..92].copy_from_slice(&crc32fast::hash(&entries).to_le_bytes());
    let crc = crc32fast::hash(&header);
    header[16..20].copy_from_slice(&crc.to_le_bytes());
    image[sector_size..sector_size + 92].copy_from_slice(&header);
    image
}

/// A fixed VHD: `disk` followed by a valid 512-byte footer.
pub fn vhd_fixed(disk: &[u8]) -> Vec<u8> {
    let size = u64::try_from(disk.len()).unwrap();
    let mut footer = vec![0u8; 512];
    footer[..8].copy_from_slice(b"conectix");
    footer[8..12].copy_from_slice(&2u32.to_be_bytes());
    footer[12..16].copy_from_slice(&0x0001_0000u32.to_be_bytes());
    footer[16..24].copy_from_slice(&u64::MAX.to_be_bytes());
    footer[28..32].copy_from_slice(b"ftkt");
    footer[40..48].copy_from_slice(&size.to_be_bytes());
    footer[48..56].copy_from_slice(&size.to_be_bytes());
    footer[60..64].copy_from_slice(&2u32.to_be_bytes());
    let sum = footer
        .iter()
        .fold(0u32, |s, &b| s.wrapping_add(u32::from(b)));
    footer[64..68].copy_from_slice(&(!sum).to_be_bytes());
    let mut image = disk.to_vec();
    image.extend_from_slice(&footer);
    image
}

/// The start of a WIM file (magic "MSWIM\0\0\0").
pub fn wim_header() -> Vec<u8> {
    let mut image = vec![0u8; 4096];
    image[..8].copy_from_slice(b"MSWIM\0\0\0");
    image
}

/// The start of an FFU file ("SignedImage " at offset 4).
pub fn ffu_header() -> Vec<u8> {
    let mut image = vec![0u8; 4096];
    image[0..4].copy_from_slice(&0x0000_0010u32.to_le_bytes());
    image[4..16].copy_from_slice(b"SignedImage ");
    image
}

/// Writes `bytes` to `dir/name` and returns the path.
pub fn write_file(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, bytes).unwrap();
    path
}

/// A 1.44 MB FAT12 image holding `/EFI/BOOT/<files>`, like the `efi.img`
/// of Linux ISOs. Names are 11-byte 8.3 names (`b"BOOTX64 EFI"`).
pub fn efi_fat_image(files: &[(&[u8; 11], &[u8])]) -> Vec<u8> {
    const SECTORS: usize = 2880;
    let mut image = vec![0u8; SECTORS * 512];
    // BIOS parameter block.
    image[0..3].copy_from_slice(&[0xeb, 0x3c, 0x90]);
    image[3..11].copy_from_slice(b"MSDOS5.0");
    image[11..13].copy_from_slice(&512u16.to_le_bytes());
    image[13] = 1; // sectors per cluster
    image[14..16].copy_from_slice(&1u16.to_le_bytes()); // reserved
    image[16] = 2; // FATs
    image[17..19].copy_from_slice(&224u16.to_le_bytes()); // root entries
    image[19..21].copy_from_slice(&(SECTORS as u16).to_le_bytes());
    image[21] = 0xf0;
    image[22..24].copy_from_slice(&9u16.to_le_bytes()); // sectors per FAT
    image[510] = 0x55;
    image[511] = 0xaa;
    let fat_start = 512;
    let root = (1 + 2 * 9) * 512;
    let data = root + 14 * 512;
    let cluster_offset = |cluster: usize| data + (cluster - 2) * 512;
    let mut fat = vec![0u8; 9 * 512];
    let mut set = |cluster: usize, value: u16| {
        let at = cluster + cluster / 2;
        let (lo, hi) = (fat[at], fat[at + 1]);
        let word = u16::from_le_bytes([lo, hi]);
        let word = if cluster.is_multiple_of(2) {
            (word & 0xf000) | value
        } else {
            (word & 0x000f) | (value << 4)
        };
        fat[at..at + 2].copy_from_slice(&word.to_le_bytes());
    };
    set(0, 0xff0);
    set(1, 0xfff);
    let entry = |name: &[u8; 11], attribute: u8, cluster: usize, size: u32| {
        let mut e = [0u8; 32];
        e[..11].copy_from_slice(name);
        e[11] = attribute;
        e[26..28].copy_from_slice(&(cluster as u16).to_le_bytes());
        e[28..32].copy_from_slice(&size.to_le_bytes());
        e
    };
    // /EFI at cluster 2, /EFI/BOOT at cluster 3, files from cluster 4.
    image[root..root + 32].copy_from_slice(&entry(b"EFI        ", 0x10, 2, 0));
    set(2, 0xfff);
    set(3, 0xfff);
    let efi = cluster_offset(2);
    image[efi..efi + 32].copy_from_slice(&entry(b".          ", 0x10, 2, 0));
    image[efi + 32..efi + 64].copy_from_slice(&entry(b"..         ", 0x10, 0, 0));
    image[efi + 64..efi + 96].copy_from_slice(&entry(b"BOOT       ", 0x10, 3, 0));
    let boot = cluster_offset(3);
    image[boot..boot + 32].copy_from_slice(&entry(b".          ", 0x10, 3, 0));
    image[boot + 32..boot + 64].copy_from_slice(&entry(b"..         ", 0x10, 2, 0));
    let mut next = 4;
    for (index, (name, contents)) in files.iter().enumerate() {
        let clusters = contents.len().div_ceil(512).max(1);
        let at = boot + 64 + index * 32;
        image[at..at + 32].copy_from_slice(&entry(name, 0x20, next, contents.len() as u32));
        for c in 0..clusters {
            set(
                next + c,
                if c + 1 == clusters {
                    0xfff
                } else {
                    (next + c + 1) as u16
                },
            );
        }
        let offset = cluster_offset(next);
        image[offset..offset + contents.len()].copy_from_slice(contents);
        next += clusters;
    }
    image[fat_start..fat_start + fat.len()].copy_from_slice(&fat);
    image[fat_start + fat.len()..fat_start + 2 * fat.len()].copy_from_slice(&fat);
    image
}
