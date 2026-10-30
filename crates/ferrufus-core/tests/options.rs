//! Format options and partition layouts for images, as Rufus proposes them.

// Test code: a failed expectation should panic.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use ferrufus_core::image::ms_sys::{MbrKind, reference_sector};
use ferrufus_core::image::{ImageReport, InspectOptions, inspect_source};
use ferrufus_core::layout::{self, ExtraPartitions, LayoutInput, UEFI_NTFS_SIZE};
use ferrufus_core::options::{
    Choices, DeviceInput, FileSystem, ImageOption, PartitionScheme, TargetSystem, format_options,
};
use ferrufus_testkit::isofs::{IsoFs, Node};
use proptest::prelude::*;

const GIB: u64 = 1024 * 1024 * 1024;

fn stick(size: u64) -> DeviceInput {
    DeviceInput {
        size,
        sector_size: 512,
        partition_scheme: Some(PartitionScheme::Mbr),
        volume_label: Some("OLD".into()),
    }
}

fn report(name: &str, image: &[u8]) -> ImageReport {
    inspect_source(name, &image.to_vec(), &InspectOptions::default()).unwrap()
}

fn windows_iso(big_wim: bool) -> ImageReport {
    let install = if big_wim {
        Node::sized("install.wim", 5 * GIB)
    } else {
        Node::file("install.wim", b"MSWIM")
    };
    let mut fs = IsoFs::new(
        "ISO",
        vec![
            Node::file("bootmgr", b"bm"),
            Node::file("bootmgr.efi", b"bme"),
            Node::dir(
                "efi",
                vec![Node::dir(
                    "boot",
                    vec![Node::file("bootx64.efi", &[1; 4096])],
                )],
            ),
            Node::dir("sources", vec![install]),
        ],
    );
    fs.udf = Some("CCCOMA_X64FRE_EN-US_DV9".into());
    report("/i/windows.iso", &fs.build())
}

fn linux_hybrid() -> ImageReport {
    let mut fs = IsoFs::new(
        "DEBIAN_12",
        vec![
            Node::dir(
                "isolinux",
                vec![
                    Node::file("isolinux.cfg", b"x"),
                    Node::file("isolinux.bin", &[0; 4096]),
                ],
            ),
            Node::dir(
                "EFI",
                vec![Node::dir(
                    "boot",
                    vec![Node::file("bootx64.efi", &[1; 4096])],
                )],
            ),
        ],
    );
    fs.rock_ridge = true;
    let mut image = fs.build();
    image[..512].copy_from_slice(&reference_sector(MbrKind::Isolinux));
    report("/i/debian.iso", &image)
}

#[test]
fn windows_iso_defaults_to_gpt_uefi_ntfs_when_the_wim_is_large() {
    let image = windows_iso(true);
    let options = format_options(&stick(32 * GIB), Some(&image), &Choices::default());
    assert_eq!(options.partition_scheme, Some(PartitionScheme::Gpt));
    assert_eq!(options.target_systems, [TargetSystem::Uefi]);
    let fs: Vec<_> = options.file_systems.iter().map(|e| e.file_system).collect();
    assert_eq!(fs, [FileSystem::Ntfs], "a 4 GB install.wim needs NTFS");
    assert_eq!(options.file_system, Some(FileSystem::Ntfs));
    assert_eq!(options.label, "CCCOMA_X64FRE_EN-US_DV9");
    assert!(!options.notes.is_empty());
}

#[test]
fn windows_iso_on_mbr_bios_cannot_use_fat() {
    let image = windows_iso(false);
    let choices = Choices {
        partition_scheme: Some(PartitionScheme::Mbr),
        ..Choices::default()
    };
    let options = format_options(&stick(32 * GIB), Some(&image), &choices);
    // A Windows image on GPT by default; picking MBR keeps it.
    assert_eq!(options.partition_scheme, Some(PartitionScheme::Mbr));
    // BIOS and UEFI bootable on MBR: the UEFI entry is not offered.
    assert_eq!(options.target_systems, [TargetSystem::BiosOrCsm]);
    assert_eq!(options.target_system, Some(TargetSystem::BiosOrCsm));
    // Windows with a BIOS target: NTFS only.
    let fs: Vec<_> = options.file_systems.iter().map(|e| e.file_system).collect();
    assert_eq!(fs, [FileSystem::Ntfs]);
    // On the default GPT (UEFI target), FAT32 is offered and preferred.
    let options = format_options(&stick(32 * GIB), Some(&image), &Choices::default());
    assert_eq!(options.partition_scheme, Some(PartitionScheme::Gpt));
    assert_eq!(options.file_system, Some(FileSystem::Fat32));
}

#[test]
fn windows_to_go_on_gpt() {
    let image = windows_iso(false);
    let choices = Choices {
        image_option: ImageOption::WindowsToGo,
        ..Choices::default()
    };
    let options = format_options(&stick(64 * GIB), Some(&image), &choices);
    assert_eq!(options.partition_scheme, Some(PartitionScheme::Gpt));
    let fs: Vec<_> = options.file_systems.iter().map(|e| e.file_system).collect();
    assert_eq!(fs, [FileSystem::Ntfs], "Windows To Go excludes FAT");
}

#[test]
fn linux_isohybrid_defaults_to_mbr_bios_or_uefi_fat32() {
    let image = linux_hybrid();
    assert!(image.is_iso, "{:?}", image.notes);
    let options = format_options(&stick(16_000_000_000), Some(&image), &Choices::default());
    assert_eq!(
        options.partition_schemes,
        [PartitionScheme::Mbr, PartitionScheme::Gpt]
    );
    assert_eq!(options.partition_scheme, Some(PartitionScheme::Mbr));
    // BIOS and UEFI bootable on MBR: only the combined target.
    assert_eq!(options.target_systems, [TargetSystem::BiosOrUefi]);
    assert_eq!(options.file_system, Some(FileSystem::Fat32));
    assert_eq!(options.label, "DEBIAN_12");
}

#[test]
fn user_choices_are_kept_when_allowed() {
    let image = linux_hybrid();
    let choices = Choices {
        partition_scheme: Some(PartitionScheme::Gpt),
        file_system: Some(FileSystem::Ntfs),
        label: Some("MYSTICK".into()),
        ..Choices::default()
    };
    let options = format_options(&stick(16_000_000_000), Some(&image), &choices);
    assert_eq!(options.partition_scheme, Some(PartitionScheme::Gpt));
    assert_eq!(options.target_systems, [TargetSystem::Uefi]);
    assert_eq!(options.file_system, Some(FileSystem::Ntfs));
    assert_eq!(options.label, "MYSTICK");
}

#[test]
fn uefi_ntfs_image_matches_upstream() {
    // Optional: compare with the upstream resource when a checkout is given.
    let Some(dir) = std::env::var_os("FERRUFUS_UPSTREAM") else {
        return;
    };
    let path = std::path::Path::new(&dir).join("res/uefi/uefi-ntfs.img");
    assert_eq!(std::fs::metadata(path).unwrap().len(), UEFI_NTFS_SIZE);
}

proptest! {
    /// Layouts never overlap, stay inside the disk (and before the backup
    /// GPT), and fail instead of producing an empty main partition.
    #[test]
    fn layouts_are_sound(
        size in (8u64 << 20)..(4u64 << 40),
        gpt in any::<bool>(),
        sector in prop::sample::select(vec![512u32, 4096]),
        cluster in prop::sample::select(vec![0u32, 512, 4096, 32768, 65536]),
        persistence in prop::option::of((1u64 << 20)..(64u64 << 30)),
        uefi_ntfs in any::<bool>(),
        compat in any::<bool>(),
        old_bios in any::<bool>(),
        esp_and_msr in any::<bool>(),
    ) {
        let scheme = if gpt { PartitionScheme::Gpt } else { PartitionScheme::Mbr };
        let input = LayoutInput {
            scheme,
            file_system: FileSystem::Ntfs,
            disk_size: size,
            sector_size: sector,
            sectors_per_track: 63,
            cluster_size: cluster,
            old_bios_fixes: old_bios,
            extra: ExtraPartitions { esp_and_msr: esp_and_msr && gpt, uefi_ntfs, compat, persistence: persistence.unwrap_or(0) },
            bootable: true,
            write_as_esp: false,
        };
        if let Ok(parts) = layout::layout(&input) {
            let end = if gpt { size - 33 * u64::from(sector) } else { size };
            prop_assert!(!parts.is_empty());
            for part in &parts {
                prop_assert!(part.size > 0);
                prop_assert!(part.offset.checked_add(part.size).is_some_and(|e| e <= end));
                prop_assert!(part.offset >= 1 << 15);
            }
            let mut sorted = parts.clone();
            sorted.sort_by_key(|p| p.offset);
            for pair in sorted.windows(2) {
                prop_assert!(pair[0].offset + pair[0].size <= pair[1].offset);
            }
        }
    }
}
