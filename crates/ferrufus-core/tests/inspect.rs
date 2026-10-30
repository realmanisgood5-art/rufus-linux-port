//! End-to-end image inspection on synthetic images.

// Test code: a failed expectation should panic.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use ferrufus_core::ErrorKind;
use ferrufus_core::image::compression::{Association, MagicCheck};
use ferrufus_core::image::iso9660::Platform;
use ferrufus_core::image::ms_sys::{MbrIdentification, MbrKind, reference_sector};
use ferrufus_core::image::vhd::VhdDiskType;
use ferrufus_core::image::{
    Bootability, Classification, ImageReport, InspectOptions, inspect, inspect_source,
};
use ferrufus_testkit::images::{self, BootEntry, GptPartition, IsoBuilder};
use ferrufus_testkit::isofs::{IsoFs, Node};

fn check(name: &str, data: &[u8]) -> ImageReport {
    inspect_source(name, &data.to_vec(), &InspectOptions::default()).unwrap()
}

fn has_note(report: &ImageReport, text: &str) -> bool {
    report.notes.iter().any(|note| note.message.contains(text))
}

#[test]
fn raw_disk_image_with_grub2() {
    let image = images::disk_image(&reference_sector(MbrKind::Grub2), 8 * 1024 * 1024);
    let report = check("/images/disk.img", &image);
    assert_eq!(report.classification, Classification::DiskImage);
    assert_eq!(report.is_bootable_img, Bootability::Bootable);
    assert!(!report.is_iso);
    assert_eq!(report.association, None);
    assert_eq!(report.projected_size, Some(8 * 1024 * 1024));
    assert_eq!(report.dd_payload_size(), Some(8 * 1024 * 1024));
    assert_eq!(
        report.mbr.as_ref().unwrap().identification,
        MbrIdentification::Known(MbrKind::Grub2)
    );
    assert!(has_note(&report, "Image has a Grub 2.0 Master Boot Record"));
}

#[test]
fn raw_image_without_boot_marker() {
    let image = vec![0u8; 1024 * 1024];
    let report = check("/images/blank.img", &image);
    assert_eq!(report.classification, Classification::Unsupported);
    assert_eq!(report.is_bootable_img, Bootability::NotBootable);
    assert!(has_note(&report, "Unsupported image"));

    // Alt-M: ignore the boot marker.
    let forced = inspect_source(
        "/images/blank.img",
        &image,
        &InspectOptions {
            ignore_boot_marker: true,
            ..InspectOptions::default()
        },
    )
    .unwrap();
    assert_eq!(forced.is_bootable_img, Bootability::Forced);
    assert_eq!(forced.classification, Classification::DiskImage);
    assert!(has_note(&forced, "FORCED"));
}

#[test]
fn windows_iso_without_hybrid_mbr() {
    let image = IsoFs::new(
        "WIN_INSTALL",
        vec![
            Node::file("bootmgr", b"bm"),
            Node::file("bootmgr.efi", b"bmefi"),
            Node::dir(
                "efi",
                vec![Node::dir(
                    "boot",
                    vec![Node::file("bootx64.efi", &[0x4d; 4096])],
                )],
            ),
            Node::dir("sources", vec![Node::file("install.wim", b"MSWIM\0\0\0")]),
        ],
    )
    .build();
    let report = check("/images/windows.iso", &image);
    assert_eq!(report.classification, Classification::Iso);
    assert!(report.is_iso);
    assert_eq!(report.is_bootable_img, Bootability::NotBootable);
    assert_eq!(report.label.as_deref(), Some("WIN_INSTALL"));
    assert!(has_note(&report, "ISO9660"));
    let scan = report.iso_scan.as_ref().unwrap();
    assert!(scan.has_bootmgr && scan.has_bootmgr_efi);
    // bootmgr.efi is bit 0, bootx64.efi is bit 1 + ARCH_X86_64.
    assert_eq!(scan.has_efi, 1 | (2 << 2));
    assert_eq!(scan.wininst_paths, ["/sources/install.wim"]);
}

#[test]
fn iso_without_boot_files_is_unsupported() {
    // ImageScanThread(): no boot method Rufus supports.
    let report = check("/images/empty.iso", &IsoBuilder::new("EMPTY").build());
    assert!(report.is_iso);
    assert_eq!(report.classification, Classification::Unsupported);
    // An ISOHybrid without an ISO boot method is written in DD mode.
    let hybrid = IsoBuilder::new("HYBRID")
        .hybrid(reference_sector(MbrKind::Isolinux))
        .build();
    let report = check("/images/hybrid.iso", &hybrid);
    assert!(!report.is_iso);
    assert_eq!(report.classification, Classification::DiskImage);
    assert!(has_note(&report, "DD image mode enforced"));
}

fn arch_like() -> Vec<u8> {
    let mut isolinux = vec![0u8; 4096];
    let version = b"ISOLINUX 6.04 6.04-pre1 ";
    isolinux[1000..1000 + version.len()].copy_from_slice(version);
    let mut fs = IsoFs::new(
        "ARCH_202610",
        vec![
            Node::dir(
                "boot",
                vec![Node::dir(
                    "syslinux",
                    vec![
                        Node::file("isolinux.bin", &isolinux),
                        Node::file("isolinux.cfg", b"DEFAULT arch\n"),
                        Node::file("ldlinux.c32", b"c32"),
                    ],
                )],
            ),
            Node::dir(
                "EFI",
                vec![Node::dir(
                    "BOOT",
                    vec![Node::file("BOOTx64.EFI", &[0x4d; 4096])],
                )],
            ),
        ],
    );
    fs.rock_ridge = true;
    fs.joliet = Some("Arch Linux".into());
    fs.boot = vec![
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
    ];
    let mut image = fs.build();
    image[..512].copy_from_slice(&reference_sector(MbrKind::Isolinux));
    image
}

#[test]
fn iso_hybrid_with_bios_and_uefi_boot() {
    let report = check("/images/archlinux.iso", &arch_like());
    assert_eq!(report.classification, Classification::IsoHybrid);
    assert_eq!(report.is_bootable_img, Bootability::Bootable);
    // The scan disables Joliet when Rock Ridge is enabled, so the label is
    // the PVD identifier, as in Rufus.
    assert_eq!(report.label.as_deref(), Some("ARCH_202610"));
    assert_eq!(
        report.mbr.as_ref().unwrap().identification,
        MbrIdentification::Known(MbrKind::Isolinux)
    );
    let el_torito = report.iso9660.as_ref().unwrap().el_torito.as_ref().unwrap();
    assert!(el_torito.has_platform(Platform::X86));
    assert!(el_torito.has_platform(Platform::Efi));
    let scan = report.iso_scan.as_ref().unwrap();
    assert_eq!(
        scan.cfg_path.as_deref(),
        Some("/boot/syslinux/isolinux.cfg")
    );
    assert_eq!(scan.sl_version, 0x0604);
    assert_eq!(scan.sl_version_str, "6.04");
    assert_eq!(scan.sl_version_ext, "/pre1");
    assert!(!scan.sl_version_embedded);
    assert_eq!(scan.has_efi, 2 << 2);
    assert!(scan.is_bios_bootable() && scan.is_efi_bootable());
}

#[test]
fn joliet_label_needs_rock_ridge_disabled() {
    // Alt-K: without Rock Ridge, Joliet is used and its label preferred.
    let options = InspectOptions {
        disable_rock_ridge: true,
        ..InspectOptions::default()
    };
    let report = inspect_source("/images/archlinux.iso", &arch_like(), &options).unwrap();
    assert_eq!(report.label.as_deref(), Some("Arch Linux"));
}

#[test]
fn udf_label_takes_precedence() {
    let mut image = IsoBuilder::new("ISO9660_LABEL").blocks(320).build();
    images::add_udf(&mut image, "CCCOMA_X64FRE_EN-US_DV9");
    let report = check("/images/windows.iso", &image);
    assert!(report.is_iso);
    assert!(report.udf.as_ref().unwrap().root_found);
    assert_eq!(report.label.as_deref(), Some("CCCOMA_X64FRE_EN-US_DV9"));
    assert_eq!(
        report.iso9660.as_ref().unwrap().label.as_deref(),
        Some("ISO9660_LABEL")
    );
    assert!(has_note(&report, "UDF"));
}

#[test]
fn gpt_disk_image() {
    let partitions = [
        GptPartition {
            type_guid: images::ESP_TYPE,
            first_lba: 2048,
            last_lba: 4095,
            name: "EFI".into(),
        },
        GptPartition {
            type_guid: images::BASIC_DATA_TYPE,
            first_lba: 4096,
            last_lba: 16383,
            name: "data".into(),
        },
    ];
    let image = images::gpt_disk(512, 16384, &reference_sector(MbrKind::Grub2), &partitions);
    let report = check("/images/gpt.img", &image);
    assert_eq!(report.classification, Classification::DiskImage);
    let gpt = report.gpt.as_ref().unwrap();
    assert!(gpt.header_crc_valid);
    assert_eq!(gpt.entries_crc_valid, Some(true));
    assert_eq!(gpt.partitions.len(), 2);
    assert_eq!(gpt.partitions[0].type_name, Some("EFI System"));
    assert_eq!(gpt.partitions[1].name, "data");
    assert_eq!(gpt.disk_bytes(), Some(16384 * 512));
    assert!(report.mbr.as_ref().unwrap().is_protective());
}

#[test]
fn fixed_vhd() {
    let disk = images::disk_image(&reference_sector(MbrKind::Windows7), 2 * 1024 * 1024);
    let image = images::vhd_fixed(&disk);
    let report = check("/images/disk.vhd", &image);
    assert_eq!(report.association, Some(Association::Vhd));
    assert!(report.is_vhd);
    assert_eq!(report.vhd.as_ref().unwrap().disk_type, VhdDiskType::Fixed);
    assert_eq!(report.image_size, 2 * 1024 * 1024 + 512);
    assert_eq!(report.projected_size, Some(2 * 1024 * 1024));
    assert_eq!(report.dd_payload_size(), Some(2 * 1024 * 1024));
    assert_eq!(report.classification, Classification::DiskImage);
}

#[test]
fn vhd_without_footer() {
    let image = images::disk_image(&reference_sector(MbrKind::Windows7), 1024 * 1024);
    let report = check("/images/fake.vhd", &image);
    assert!(!report.is_vhd);
    assert_eq!(report.classification, Classification::Unsupported);
    assert!(report.warnings.iter().any(|w| w.contains("footer")));
}

#[test]
fn windows_wim() {
    let report = check("/images/install.wim", &images::wim_header());
    assert!(report.is_windows_img);
    assert_eq!(report.classification, Classification::WindowsImage);
}

#[test]
fn ffu_is_not_supported_on_linux() {
    let report = check("/images/disk.ffu", &images::ffu_header());
    assert_eq!(report.association, Some(Association::Ffu));
    assert_eq!(report.magic, Some(MagicCheck::Match));
    assert_eq!(report.classification, Classification::Unsupported);
    assert!(has_note(&report, "dism"));
}

#[test]
fn compressed_images() {
    let mut gz = vec![0x1f, 0x8b, 0x08, 0x00];
    gz.resize(4096, 0);
    let report = check("/images/raspios.img.gz", &gz);
    assert_eq!(report.association, Some(Association::Gzip));
    assert_eq!(report.magic, Some(MagicCheck::Match));
    assert_eq!(report.is_bootable_img, Bootability::Unknown);
    assert_eq!(report.classification, Classification::NotYetSupported);
    assert_eq!(report.projected_size, None);

    let report = check("/images/not-really.xz", &vec![0u8; 4096]);
    assert_eq!(report.magic, Some(MagicCheck::Mismatch));
    assert_eq!(report.classification, Classification::Unsupported);
    assert!(!report.warnings.is_empty());
}

#[test]
fn vhdx_and_vtsi_are_recognized() {
    let mut vhdx = b"vhdxfile".to_vec();
    vhdx.resize(1024 * 1024, 0);
    let report = check("/images/disk.vhdx", &vhdx);
    assert_eq!(report.magic, Some(MagicCheck::Match));
    assert_eq!(report.classification, Classification::NotYetSupported);

    let mut vtsi = vec![0u8; 4096];
    let footer = vtsi.len() - 512;
    vtsi[footer..footer + 8].copy_from_slice(b"VENTOY\0\0");
    vtsi[footer + 12..footer + 20].copy_from_slice(&(16u64 << 30).to_le_bytes());
    let report = check("/images/windows.vtsi", &vtsi);
    assert_eq!(report.projected_size, Some(16 << 30));
    assert_eq!(report.classification, Classification::NotYetSupported);
}

#[test]
fn empty_and_tiny_files() {
    let report = check("/images/empty.img", &[]);
    assert_eq!(report.image_size, 0);
    assert_eq!(report.classification, Classification::Unsupported);
    let report = check("/images/tiny.img", &[0x55, 0xaa]);
    assert_eq!(report.is_bootable_img, Bootability::NotBootable);
}

#[test]
fn files_on_disk() {
    let dir = tempfile::tempdir().unwrap();
    let image = images::disk_image(&reference_sector(MbrKind::Rufus), 64 * 1024);
    let path = images::write_file(dir.path(), "disk.img", &image);
    let report = inspect(&path, &InspectOptions::default()).unwrap();
    assert_eq!(report.classification, Classification::DiskImage);
    assert_eq!(report.path, path.to_string_lossy());

    let error = inspect(dir.path(), &InspectOptions::default()).unwrap_err();
    assert_eq!(error.kind(), ErrorKind::InvalidInput);
    let error = inspect(&dir.path().join("missing.iso"), &InspectOptions::default()).unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Io);
}
