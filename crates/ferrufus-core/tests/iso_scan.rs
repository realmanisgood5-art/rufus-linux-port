//! The ISO scan (`check_iso_props()` and the scan half of `ExtractISO()`)
//! on synthetic images.

// Test code: a failed expectation should panic.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use ferrufus_core::image::iso_scan::{EfiBootType, IsoScan, ScanFileSystem, ScanOptions, scan};
use ferrufus_testkit::images;
use ferrufus_testkit::isofs::{IsoFs, Node};

const GIB: u64 = 1024 * 1024 * 1024;

fn rock(label: &str, root: Vec<Node>) -> IsoScan {
    let mut fs = IsoFs::new(label, root);
    fs.rock_ridge = true;
    fs.joliet = Some(label.to_lowercase());
    let image = fs.build();
    scan(&image, ScanOptions::default())
        .unwrap()
        .expect("the scan should succeed")
}

fn udf(label: &str, root: Vec<Node>) -> IsoScan {
    let mut fs = IsoFs::new("ISO_LABEL", root);
    fs.udf = Some(label.into());
    let image = fs.build();
    scan(&image, ScanOptions::default())
        .unwrap()
        .expect("the scan should succeed")
}

/// A GRUB module or EFI binary with the strings Rufus looks for.
fn grub_binary(version: &str, filesystems: &[&str]) -> Vec<u8> {
    let mut data = vec![0u8; 256];
    data.extend_from_slice(format!("GRUB  version\0{version}\0").as_bytes());
    for fs in filesystems {
        data.extend_from_slice(format!("fshelp\0{fs}\0").as_bytes());
    }
    data.extend(vec![0u8; 256]);
    data
}

fn isolinux(version: &str) -> Vec<u8> {
    let mut data = vec![0u8; 4096];
    let text = format!("ISOLINUX {version} ");
    data[512..512 + text.len()].copy_from_slice(text.as_bytes());
    data
}

#[test]
fn ubuntu_like_iso() {
    let scan = rock(
        "Ubuntu 24.04.1 LTS amd64",
        vec![
            Node::dir(
                "boot",
                vec![Node::dir(
                    "grub",
                    vec![
                        Node::dir(
                            "i386-pc",
                            vec![Node::file("normal.mod", &grub_binary("2.12", &[]))],
                        ),
                        Node::file("grub.cfg", b"menuentry"),
                    ],
                )],
            ),
            Node::dir(
                "casper",
                vec![
                    Node::file("vmlinuz", b"k"),
                    Node::sized("filesystem.squashfs", 5 * GIB),
                ],
            ),
            Node::dir(
                "EFI",
                vec![Node::dir(
                    "boot",
                    vec![
                        Node::file("bootx64.efi", &[0x4d; 1024]),
                        Node::file(
                            "grubx64.efi",
                            &grub_binary("2.12", &["fat", "iso9660", "ntfs"]),
                        ),
                        Node::file("mmx64.efi", b"mok"),
                    ],
                )],
            ),
            Node::file("md5sum.txt", b"sums"),
        ],
    );
    assert_eq!(scan.file_system, Some(ScanFileSystem::Iso9660));
    assert_eq!(scan.label, "Ubuntu 24.04.1 LTS amd64");
    assert!(scan.uses_casper);
    assert_eq!(scan.has_md5sum, 1);
    // GRUB BIOS directory (1) plus GRUB EFI (0x80).
    assert_eq!(scan.has_grub2, 0x81);
    assert_eq!(scan.grub2_version, "2.12");
    assert_eq!(scan.has_grub2_fs, 0b101);
    assert_eq!(scan.has_efi, 2 << 2);
    let kinds: Vec<_> = scan
        .efi_boot_entries
        .iter()
        .map(|e| (e.kind, e.path.as_str()))
        .collect();
    assert_eq!(
        kinds,
        [
            (EfiBootType::Main, "/EFI/boot/bootx64.efi"),
            (EfiBootType::Grub, "/EFI/boot/grubx64.efi"),
            (EfiBootType::MokManager, "/EFI/boot/mmx64.efi")
        ]
    );
    // One file of 4 GB or more, which EFI GRUB can read (it has NTFS).
    assert_eq!(scan.has_4gb_file, 1);
    assert!(!scan.disable_iso);
    assert!(scan.is_bios_bootable() && scan.is_efi_bootable());
    assert!(scan.has_persistence());
}

#[test]
fn grub_without_large_file_support_disables_iso_mode() {
    let scan = rock(
        "LINUX",
        vec![
            Node::dir(
                "EFI",
                vec![Node::dir(
                    "BOOT",
                    vec![Node::file("grubx64.efi", &grub_binary("2.06", &["fat"]))],
                )],
            ),
            Node::sized("live.squashfs", 5 * GIB),
        ],
    );
    assert_eq!(scan.has_grub2_fs, 0b001);
    assert!(
        scan.disable_iso,
        "a 4 GB file needs exFAT or NTFS support in EFI GRUB"
    );
    // 2.06 gets the sanitized label appended.
    assert_eq!(scan.grub2_version, "2.06-linux");
}

#[test]
fn windows_udf_iso() {
    let scan = udf(
        "CCCOMA_X64FRE_EN-US_DV9",
        vec![
            Node::file("bootmgr", b"bm"),
            Node::file("bootmgr.efi", b"bme"),
            Node::dir(
                "efi",
                vec![Node::dir(
                    "boot",
                    vec![Node::file("bootx64.efi", &[0x4d; 2048])],
                )],
            ),
            Node::dir(
                "sources",
                vec![
                    Node::sized("install.wim", 5 * GIB),
                    Node::file("boot.wim", b"wim"),
                    Node::dir(
                        "$OEM$",
                        vec![Node::dir(
                            "$$",
                            vec![Node::dir("Panther", vec![Node::file("unattend.xml", b"x")])],
                        )],
                    ),
                ],
            ),
        ],
    );
    assert_eq!(scan.file_system, Some(ScanFileSystem::Udf));
    assert_eq!(scan.label, "CCCOMA_X64FRE_EN-US_DV9");
    assert!(scan.has_bootmgr && scan.has_bootmgr_efi);
    assert_eq!(scan.wininst_paths, ["/sources/install.wim"]);
    // One 4 GB file, and it is install.wim.
    assert_eq!(scan.has_4gb_file, 0x11);
    assert!(scan.has_panther_unattend);
    assert_eq!(scan.has_efi, 1 | (2 << 2));
    assert_eq!(scan.efi_boot_entries[0].kind, EfiBootType::Bootmgr);
    assert!(scan.has_wintogo());
    assert!(!scan.has_persistence());
    assert!(scan.not_analyzed.iter().any(|n| n.contains("WIM")));
}

#[test]
fn syslinux_detection_and_config_choice() {
    let scan = rock(
        "TAILS",
        vec![
            Node::dir(
                "EFI",
                vec![Node::dir("BOOT", vec![Node::file("isolinux.cfg", b"x")])],
            ),
            Node::dir(
                "isolinux",
                vec![
                    Node::file("isolinux.cfg", b"x"),
                    Node::file("isolinux.bin", &isolinux("4.07 2013-07-25")),
                ],
            ),
            Node::dir(
                "syslinux",
                vec![Node::file("isolinux.bin", &isolinux("6.03"))],
            ),
            Node::file("vesamenu.c32", &[0; 1000]),
        ],
    );
    // Equal lengths: the later one wins.
    assert_eq!(scan.cfg_path.as_deref(), Some("/isolinux/isolinux.cfg"));
    // Conflicting versions: the one next to the configuration file wins.
    assert_eq!(scan.sl_version, 0x0407);
    assert_eq!(scan.sl_version_ext, "/2013-07-25");
    assert_eq!(scan.has_old_c32, [false, true]);
    assert!(scan.log.iter().any(|l| l.contains("conflicting")));

    // No version string: the embedded version is used.
    let scan = rock(
        "X",
        vec![
            Node::file("syslinux.cfg", b"x"),
            Node::file("isolinux.bin", &[0u8; 4096]),
            Node::file("ldlinux.c32", b"x"),
        ],
    );
    assert_eq!(scan.cfg_path.as_deref(), Some("/syslinux.cfg"));
    assert!(scan.sl_version_embedded);
    assert_eq!(scan.sl_version_str, "6.04");

    // openSUSE's loader configuration is forced.
    let scan = rock(
        "openSUSE",
        vec![
            Node::file("syslinux.cfg", b"x"),
            Node::dir(
                "boot",
                vec![Node::dir(
                    "x86_64",
                    vec![Node::dir("loader", vec![Node::file("isolinux.cfg", b"x")])],
                )],
            ),
        ],
    );
    assert_eq!(
        scan.cfg_path.as_deref(),
        Some("/boot/x86_64/loader/isolinux.cfg")
    );
    assert!(scan.needs_syslinux_overwrite);
}

#[test]
fn efi_img_boot_loaders_are_found() {
    let fat = images::efi_fat_image(&[(b"BOOTX64 EFI", &[1u8; 1500]), (b"BOOTIA32EFI", b"x")]);
    let scan = rock(
        "SOLUS",
        vec![
            Node::dir("efi", vec![Node::file("efiboot.img", &fat)]),
            Node::file("isolinux.cfg", b"x"),
        ],
    );
    assert_eq!(scan.efi_img_path.as_deref(), Some("/efi/efiboot.img"));
    assert_eq!(scan.has_efi, 0x8000);
    assert!(scan.is_efi_bootable());
    // An efi.img without boot loaders does not count.
    let empty = images::efi_fat_image(&[]);
    let scan = rock("X", vec![Node::file("efi.img", &empty)]);
    assert_eq!(scan.has_efi, 0);
}

#[test]
fn small_bootx64_is_a_broken_link() {
    let scan = rock(
        "MINT",
        vec![Node::dir(
            "EFI",
            vec![Node::dir("boot", vec![Node::file("bootx64.efi", b"tiny")])],
        )],
    );
    assert_eq!(scan.has_efi & 0x4000, 0x4000);
    assert_eq!(
        scan.efi_img_path.as_deref(),
        Some("[BOOT]/1-Boot-NoEmul.img")
    );
}

#[test]
fn rock_ridge_specifics() {
    let long = "a".repeat(80);
    let scan = rock(
        "LMDE",
        vec![
            Node::symlink("live", "casper"),
            Node::dir("casper", vec![Node::file("vmlinuz", b"k")]),
            Node::file(&long, b"x"),
            Node::dir(
                "firmware",
                vec![
                    Node::file("blob.bin", &[0; 5000]),
                    Node::symlink("alias.bin", "blob.bin"),
                ],
            ),
        ],
    );
    assert_eq!(scan.has_symlinks, 1);
    assert!(scan.needs_ntfs, "LMDE needs working symbolic links");
    assert!(scan.has_long_filename);
    // The /firmware symlink target is counted twice in the projected size:
    // 3 blocks for blob.bin, 3 more for the link, 1 each for the others.
    // 8 blocks * 2048 * 1.01f: (double)1.01f is 1.0099999904632568.
    assert_eq!(scan.projected_size, 16_547);
}

#[test]
fn deep_directories_stop_the_directory() {
    let scan = rock(
        "DEEP",
        vec![
            Node::dir(
                "a",
                vec![
                    Node::DeepDir {
                        name: "rr_moved".into(),
                    },
                    Node::file("never.cfg", b"x"),
                ],
            ),
            Node::file("kept", b"x"),
        ],
    );
    assert!(scan.has_deep_directories);
    assert!(scan.cfg_path.is_none());
}

#[test]
fn root_markers() {
    let scan = rock(
        "MISC",
        vec![
            Node::file("grldr", b"g"),
            Node::file("kolibri.img", b"k"),
            Node::file("MD5SUMS", b"m"),
            Node::dir("reactos", vec![Node::file("setupldr.sys", b"r")]),
        ],
    );
    assert!(scan.has_grub4dos && scan.has_kolibrios);
    assert_eq!(scan.has_md5sum, 2);
    assert_eq!(scan.reactos_path.as_deref(), Some("/reactos/setupldr.sys"));
    for (root, reason) in [
        (vec![Node::file(".miso", b"m")], "Manjaro"),
        (
            vec![Node::dir("proxmox", vec![Node::file("x", b"x")])],
            "Proxmox",
        ),
        (
            vec![Node::dir(
                "casper_pop-os_22.04",
                vec![Node::file("x", b"x")],
            )],
            "Pop!_OS",
        ),
    ] {
        assert!(rock("X", root).disable_iso, "{reason}");
    }
}

#[test]
fn windows_pe_and_minint() {
    let sif = b"[SetupData]\r\nOsLoadOptions = \"/fastdetect /MININT\"\r\n";
    let scan = rock(
        "WINPE",
        vec![Node::dir(
            "i386",
            vec![
                Node::file("ntdetect.com", b"n"),
                Node::file("setupldr.bin", b"s"),
                Node::file("txtsetup.sif", sif),
            ],
        )],
    );
    assert_eq!(scan.winpe, 0x0007);
    assert!(scan.has_winpe());
    assert!(scan.uses_minint);
}

#[test]
fn labels_follow_the_extension_rules() {
    let mut fs = IsoFs::new("PVD_LABEL", vec![Node::file("isolinux.cfg", b"x")]);
    fs.rock_ridge = true;
    fs.joliet = Some("Joliet Label".into());
    let image = fs.build();
    assert_eq!(
        scan(&image, ScanOptions::default()).unwrap().unwrap().label,
        "PVD_LABEL"
    );
    let no_rr = ScanOptions {
        joliet: true,
        rock_ridge: false,
    };
    assert_eq!(scan(&image, no_rr).unwrap().unwrap().label, "Joliet Label");
}

#[test]
fn red_hat_derivatives_and_suse() {
    assert!(
        rock(
            "Rocky-9-3-x86_64-dvd",
            vec![Node::file("isolinux.cfg", b"x")]
        )
        .rh8_derivative
    );
    assert!(
        rock(
            "openSUSE-Tumbleweed-DVD-x86_64",
            vec![Node::file("x", b"x")]
        )
        .disable_iso
    );
}

#[test]
fn not_an_iso() {
    assert!(
        scan(&vec![0u8; 64 * 2048], ScanOptions::default())
            .unwrap()
            .is_none()
    );
}
