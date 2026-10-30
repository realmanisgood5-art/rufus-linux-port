//! Property tests: image inspection never panics, whatever the input.
//!
//! Arbitrary bytes exercise the "nothing recognized" paths; mutations of
//! valid images reach deep into each parser with corrupted fields.

// Test code: a failed expectation should panic.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use ferrufus_core::image::ms_sys::{MbrKind, reference_sector};
use ferrufus_core::image::{InspectOptions, inspect_source};
use ferrufus_testkit::images::{self, BootEntry, GptPartition, IsoBuilder};
use proptest::prelude::*;

const NAMES: [&str; 8] = [
    "/i/disk.img",
    "/i/disk.iso",
    "/i/disk.vhd",
    "/i/disk.vhdx",
    "/i/disk.img.gz",
    "/i/disk.vtsi",
    "/i/disk.ffu",
    "/i/install.wim",
];

fn inspect_all_ways(data: &[u8]) {
    let data = data.to_vec();
    for name in NAMES {
        for ignore_boot_marker in [false, true] {
            let options = InspectOptions {
                ignore_boot_marker,
                ..InspectOptions::default()
            };
            inspect_source(name, &data, &options).unwrap();
        }
    }
}

/// Applies `(offset, value)` writes to `image`, wrapping offsets into
/// the image's metadata area (`limit` bytes).
fn mutate(mut image: Vec<u8>, writes: &[(usize, u8)], limit: usize) -> Vec<u8> {
    let limit = limit.min(image.len()).max(1);
    for &(offset, value) in writes {
        image[offset % limit] = value;
    }
    image
}

fn iso_fixture() -> Vec<u8> {
    let mut image = IsoBuilder::new("FIXTURE")
        .joliet("Fixture")
        .el_torito(&[
            BootEntry {
                platform: 0,
                lsn: 40,
                sectors: 4,
            },
            BootEntry {
                platform: 0xef,
                lsn: 41,
                sectors: 8,
            },
        ])
        .hybrid(reference_sector(MbrKind::Isolinux))
        .blocks(320)
        .build();
    images::add_udf(&mut image, "FIXTURE_UDF");
    image
}

fn gpt_fixture() -> Vec<u8> {
    let partitions = [GptPartition {
        type_guid: images::ESP_TYPE,
        first_lba: 34,
        last_lba: 200,
        name: "EFI".into(),
    }];
    images::gpt_disk(512, 4096, &reference_sector(MbrKind::Grub2), &partitions)
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 256, ..ProptestConfig::default() })]

    #[test]
    fn arbitrary_bytes(data in prop::collection::vec(any::<u8>(), 0..40_000)) {
        inspect_all_ways(&data);
    }
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 64, ..ProptestConfig::default() })]

    #[test]
    fn mutated_iso_and_udf(writes in prop::collection::vec((any::<usize>(), any::<u8>()), 1..64)) {
        // Volume descriptors, the boot catalog (block 30) and the UDF
        // structures (blocks 32 to 34 and 256, 300 and 302).
        let image = iso_fixture();
        let regions = [16 * 2048..20 * 2048, 30 * 2048..31 * 2048, 32 * 2048..35 * 2048, 256 * 2048..257 * 2048, 300 * 2048..303 * 2048];
        let mut mutated = image.clone();
        for (i, &(offset, value)) in writes.iter().enumerate() {
            let region = &regions[i % regions.len()];
            mutated[region.start + offset % region.len()] = value;
        }
        inspect_all_ways(&mutated);
    }

    #[test]
    fn mutated_gpt(writes in prop::collection::vec((any::<usize>(), any::<u8>()), 1..64)) {
        // The protective MBR, the header and the entry array.
        inspect_all_ways(&mutate(gpt_fixture(), &writes, 512 * 40));
    }

    #[test]
    fn mutated_vhd_footer(writes in prop::collection::vec((any::<usize>(), any::<u8>()), 1..32)) {
        let disk = images::disk_image(&reference_sector(MbrKind::Windows7), 64 * 1024);
        let mut image = images::vhd_fixed(&disk);
        let footer = image.len() - 512;
        for &(offset, value) in &writes {
            image[footer + offset % 512] = value;
        }
        inspect_all_ways(&image);
    }
}
