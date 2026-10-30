//! The `ferrufus-cli` binary: output formats and exit codes, on a fake
//! system tree (`--sysroot`). Nothing here touches a real device.

// Test code: a failed expectation should panic.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use ferrufus_testkit::images;
use ferrufus_testkit::system::{Bus, Disk, FakeSystem, Partition};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// A system with an NVMe root disk, a USB stick `sdb` and a USB hard drive `sdc`.
fn system() -> FakeSystem {
    let mut system = FakeSystem::new();
    let nvme = system.add_disk(&Disk {
        name: "nvme0n1",
        dev: (259, 0),
        sectors: 1_000_000_000,
        removable: false,
        read_only: false,
        diskseq: Some(1),
        bus: Bus::Nvme,
        vendor: "",
        model: "Samsung SSD 980",
    });
    system.add_partition(
        &nvme,
        &Partition {
            name: "nvme0n1p2",
            dev: (259, 2),
            number: 2,
            start: 2048,
            sectors: 900_000_000,
        },
    );
    system.mount((259, 2), "/", "ext4", "/dev/nvme0n1p2", "rw");
    system.add_disk(&Disk::usb_stick("sdb", (8, 16), 31_250_000));
    system.add_disk(&Disk {
        removable: false,
        bus: Bus::Usb {
            vid: 0x1058,
            pid: 0x2620,
            driver: "usb-storage",
            serial: "WX12",
        },
        vendor: "WD",
        model: "Elements 2620",
        ..Disk::usb_stick("sdc", (8, 32), 3_907_029_168)
    });
    system
}

fn run(system: &FakeSystem, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_ferrufus-cli"))
        .arg("--sysroot")
        .arg(system.root())
        .args(args)
        .env_remove("FERRUFUS_LOG")
        .output()
        .unwrap()
}

fn code(output: &Output) -> i32 {
    output.status.code().unwrap()
}

fn json(output: &Output) -> Value {
    serde_json::from_slice(&output.stdout).unwrap_or_else(|e| {
        panic!(
            "{e}: {}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    })
}

fn text(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// A bootable 1 MB disk image in a fresh directory.
fn image(dir: &Path) -> PathBuf {
    let sector = ferrufus_core::image::ms_sys::reference_sector(
        ferrufus_core::image::ms_sys::MbrKind::Grub2,
    );
    images::write_file(dir, "disk.img", &images::disk_image(&sector, 1024 * 1024))
}

fn listed(value: &Value) -> Vec<String> {
    value["devices"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|d| d["decision"]["listed"] == true)
        .map(|d| d["facts"]["name"].as_str().unwrap().to_string())
        .collect()
}

#[test]
fn devices_lists_only_eligible_devices() {
    let system = system();
    let output = run(&system, &["devices", "--json"]);
    assert_eq!(code(&output), 0);
    let value = json(&output);
    assert_eq!(value["schema"], "ferrufus.devices/v1");
    assert_eq!(value["upstream_baseline"], "v4.15");
    assert_eq!(listed(&value), ["sdb"]);
    assert!(value["system"]["system_disks"]["nvme0n1"].is_array());

    let hdd = json(&run(&system, &["devices", "--json", "-x"]));
    assert_eq!(listed(&hdd), ["sdb", "sdc"]);

    let plain = text(&run(&system, &["devices"]));
    assert!(plain.contains("none is selected"), "{plain}");
    assert!(plain.contains("/dev/sdb"), "{plain}");
    assert!(!plain.contains("/dev/nvme0n1"), "{plain}");
    let all = text(&run(&system, &["devices", "--all"]));
    assert!(
        all.contains("the running system depends on it: backs /"),
        "{all}"
    );
}

#[test]
fn inspect_reports_and_fails_with_the_image_code() {
    let system = system();
    let dir = tempfile::tempdir().unwrap();
    let path = image(dir.path());
    let output = run(&system, &["inspect", "--json", path.to_str().unwrap()]);
    assert_eq!(code(&output), 0);
    let value = json(&output);
    assert_eq!(value["schema"], "ferrufus.image-report/v2");
    assert_eq!(value["classification"], "disk-image");

    let missing = run(
        &system,
        &["inspect", dir.path().join("missing.iso").to_str().unwrap()],
    );
    assert_eq!(code(&missing), 5);
    assert!(missing.stdout.is_empty());
}

#[test]
fn hash_compares_with_the_expected_checksum() {
    let system = system();
    let dir = tempfile::tempdir().unwrap();
    let path = images::write_file(dir.path(), "abc.txt", b"abc");
    let sha256 = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
    let file = path.to_str().unwrap();

    let output = run(
        &system,
        &[
            "hash",
            "--json",
            file,
            "--expect",
            &format!("sha256:{sha256}"),
        ],
    );
    assert_eq!(code(&output), 0);
    let value = json(&output);
    assert_eq!(value["schema"], "ferrufus.hash/v1");
    assert_eq!(value["matches"], true);
    assert_eq!(value["digests"].as_array().unwrap().len(), 3);

    let wrong = format!("sha256:{}", "0".repeat(64));
    let mismatch = run(&system, &["hash", file, "--expect", &wrong]);
    assert_eq!(code(&mismatch), 6);
    assert!(text(&mismatch).contains("MISMATCH"));

    let sha512 = json(&run(&system, &["hash", "--json", "--sha512", file]));
    assert_eq!(sha512["digests"].as_array().unwrap().len(), 4);
    let only = json(&run(&system, &["hash", "--json", "--algo", "md5", file]));
    assert_eq!(
        only["digests"][0]["hex"],
        "900150983cd24fb0d6963f7d28e17f72"
    );
}

#[test]
fn plan_is_a_dry_run_with_stable_exit_codes() {
    let system = system();
    let dir = tempfile::tempdir().unwrap();
    let path = image(dir.path());
    let image = path.to_str().unwrap();

    let output = run(
        &system,
        &["plan", "--json", "--device", "/dev/sdb", "--image", image],
    );
    assert_eq!(
        code(&output),
        0,
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value = json(&output);
    assert_eq!(value["schema"], "ferrufus.plan/v2");
    assert_eq!(value["plan"]["blocked"], false);
    assert_eq!(value["plan"]["executable"], false);
    assert_eq!(value["image_disks"], serde_json::json!(["nvme0n1"]));

    let plain = text(&run(
        &system,
        &["plan", "--device", "sdb", "--image", image],
    ));
    assert!(
        plain.starts_with("DRY RUN: nothing will be written."),
        "{plain}"
    );
    assert!(plain.contains("ALL DATA ON /dev/sdb"), "{plain}");

    // The system disk, and a USB hard drive that is hidden by default.
    assert_eq!(
        code(&run(
            &system,
            &["plan", "--device", "nvme0n1", "--image", image]
        )),
        3
    );
    assert_eq!(
        code(&run(
            &system,
            &["plan", "--device", "sdc", "--image", image]
        )),
        3
    );
    assert_eq!(
        code(&run(
            &system,
            &["plan", "-x", "--device", "sdc", "--image", image]
        )),
        0
    );

    assert_eq!(
        code(&run(
            &system,
            &["plan", "--device", "sdz", "--image", image]
        )),
        4
    );
    let missing = dir.path().join("missing.img");
    assert_eq!(
        code(&run(
            &system,
            &[
                "plan",
                "--device",
                "sdb",
                "--image",
                missing.to_str().unwrap()
            ]
        )),
        5
    );
}

#[test]
fn there_is_no_default_device() {
    let system = system();
    let dir = tempfile::tempdir().unwrap();
    let path = image(dir.path());
    let output = run(&system, &["plan", "--image", path.to_str().unwrap()]);
    assert_eq!(code(&output), 2);
    assert!(String::from_utf8_lossy(&output.stderr).contains("--device"));
}

#[test]
fn usage_and_version() {
    let system = system();
    assert_eq!(code(&run(&system, &["bogus"])), 2);
    let version = run(&system, &["--version"]);
    assert_eq!(code(&version), 0);
    assert!(text(&version).starts_with(concat!("ferrufus-cli ", env!("CARGO_PKG_VERSION"))));
    let help = text(&run(&system, &["--help"]));
    assert!(
        help.contains("not affiliated with or endorsed by the Rufus project"),
        "{help}"
    );
}

/// A small Linux ISO with Syslinux and a UEFI boot loader.
fn linux_iso(dir: &Path) -> PathBuf {
    let mut isolinux = vec![0u8; 4096];
    isolinux[1000..1014].copy_from_slice(b"ISOLINUX 6.04 ");
    let mut fs = ferrufus_testkit::isofs::IsoFs::new(
        "LIVE_ISO",
        vec![
            ferrufus_testkit::isofs::Node::dir(
                "isolinux",
                vec![
                    ferrufus_testkit::isofs::Node::file("isolinux.cfg", b"x"),
                    ferrufus_testkit::isofs::Node::file("isolinux.bin", &isolinux),
                ],
            ),
            ferrufus_testkit::isofs::Node::dir(
                "EFI",
                vec![ferrufus_testkit::isofs::Node::dir(
                    "BOOT",
                    vec![ferrufus_testkit::isofs::Node::file(
                        "BOOTX64.EFI",
                        &[1; 4096],
                    )],
                )],
            ),
        ],
    );
    fs.rock_ridge = true;
    images::write_file(dir, "live.iso", &fs.build())
}

#[test]
fn options_and_iso_and_format_plans() {
    let system = system();
    let dir = tempfile::tempdir().unwrap();
    let iso = linux_iso(dir.path());
    let iso = iso.to_str().unwrap();

    let inspect = json(&run(&system, &["inspect", "--json", iso]));
    assert_eq!(inspect["iso_scan"]["cfg_path"], "/isolinux/isolinux.cfg");
    assert_eq!(inspect["classification"], "iso");

    let options = run(
        &system,
        &["options", "--json", "--device", "sdb", "--image", iso],
    );
    assert_eq!(
        code(&options),
        0,
        "{}",
        String::from_utf8_lossy(&options.stderr)
    );
    let options = json(&options);
    assert_eq!(options["schema"], "ferrufus.options/v1");
    assert_eq!(options["file_system"], "fat32");
    assert_eq!(options["label"], "LIVE_ISO");
    let text = text(&run(
        &system,
        &["options", "--device", "sdb", "--image", iso],
    ));
    assert!(text.contains("* FAT32"), "{text}");

    let plan = run(
        &system,
        &[
            "plan", "--json", "--mode", "iso", "--device", "sdb", "--image", iso,
        ],
    );
    assert_eq!(code(&plan), 0, "{}", String::from_utf8_lossy(&plan.stdout));
    let plan = json(&plan);
    assert_eq!(plan["plan"]["mode"], "iso");
    assert_eq!(plan["plan"]["executable"], false);
    assert_eq!(plan["plan"]["layout"][0]["name"], "Main Data Partition");
    assert_eq!(plan["plan"]["format"]["file_system"], "fat32");

    // ISO mode onto the system disk is blocked.
    assert_eq!(
        code(&run(
            &system,
            &[
                "plan", "--mode", "iso", "--device", "nvme0n1", "--image", iso
            ]
        )),
        3
    );

    // Without an image: a plain format.
    let format = run(
        &system,
        &[
            "plan", "--json", "--device", "sdb", "--fs", "exfat", "--label", "DATA",
        ],
    );
    assert_eq!(
        code(&format),
        0,
        "{}",
        String::from_utf8_lossy(&format.stdout)
    );
    let format = json(&format);
    assert_eq!(format["plan"]["mode"], "format");
    assert_eq!(format["plan"]["format"]["file_system"], "exfat");
    assert_eq!(format["plan"]["format"]["label"], "DATA");

    // ISO mode needs an image.
    assert_eq!(
        code(&run(&system, &["plan", "--mode", "iso", "--device", "sdb"])),
        2
    );
    // An unknown file system is a usage error.
    assert_eq!(
        code(&run(&system, &["plan", "--device", "sdb", "--fs", "zfs"])),
        2
    );
}
