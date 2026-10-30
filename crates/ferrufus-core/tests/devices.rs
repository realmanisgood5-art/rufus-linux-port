//! Device discovery and the listing policy, on fake system trees.
//!
//! No test here opens a real device; the host smoke test only reads sysfs
//! and procfs.

// Test code: a failed expectation should panic.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use ferrufus_core::ErrorKind;
use ferrufus_core::device::facts::Transport;
use ferrufus_core::device::identity::{Revalidation, revalidate};
use ferrufus_core::device::mounts;
use ferrufus_core::device::policy::{Category, ListingOptions, ReasonCode};
use ferrufus_core::device::sysfs::SysRoot;
use ferrufus_core::device::system::SystemInfo;
use ferrufus_core::device::{DeviceEntry, DeviceScan, ScanOptions, scan};
use ferrufus_testkit::system::{Bus, Disk, FakeSystem, Partition};
use std::fs;
use std::path::{Path, PathBuf};

/// 16 GB in sectors.
const STICK: u64 = 31_250_000;

fn run(system: &FakeSystem, listing: ListingOptions) -> DeviceScan {
    run_with(
        system,
        ScanOptions {
            listing,
            program_paths: Vec::new(),
        },
    )
}

fn run_with(system: &FakeSystem, options: ScanOptions) -> DeviceScan {
    scan(&SysRoot::new(system.root()), &options).unwrap()
}

fn entry<'a>(scan: &'a DeviceScan, name: &str) -> &'a DeviceEntry {
    scan.find(name).unwrap()
}

fn codes(entry: &DeviceEntry) -> Vec<ReasonCode> {
    entry.decision.reasons.iter().map(|r| r.code).collect()
}

fn listed_names(scan: &DeviceScan) -> Vec<&str> {
    scan.listed().map(|d| d.facts.name.as_str()).collect()
}

fn partition(name: &'static str, dev: (u32, u32), number: u32) -> Partition {
    Partition {
        name,
        dev,
        number,
        start: 2048,
        sectors: 1_000_000,
    }
}

fn disk(name: &'static str, dev: (u32, u32), sectors: u64, removable: bool, bus: Bus) -> Disk {
    Disk {
        name,
        dev,
        sectors,
        removable,
        read_only: false,
        diskseq: Some(7),
        bus,
        vendor: "ATA",
        model: "Disk",
    }
}

#[test]
fn usb_stick_is_listed_with_rufus_style_name() {
    let mut system = FakeSystem::new();
    let dir = system.add_disk(&Disk::usb_stick("sdb", (8, 16), STICK));
    system.add_partition(&dir, &partition("sdb1", (8, 17), 1));
    system.udev(
        (8, 16),
        &[("ID_SERIAL_SHORT", "4C530001230922115084")],
        &["disk/by-id/usb-SanDisk_Cruzer_Blade_4C53-0:0"],
    );
    system.udev(
        (8, 17),
        &[("ID_FS_TYPE", "vfat"), ("ID_FS_LABEL", "MYSTICK")],
        &[],
    );

    let scan = run(&system, ListingOptions::default());
    let stick = entry(&scan, "sdb");
    assert!(stick.decision.listed, "{:?}", stick.decision);
    assert_eq!(stick.decision.category, Category::UsbDrive);
    assert!(
        stick
            .decision
            .hdd_score
            .as_ref()
            .is_some_and(|s| !s.is_hdd())
    );
    assert_eq!(stick.display_name, "MYSTICK (/dev/sdb) [16 GB]");
    assert_eq!(stick.stable_id, "usb-SanDisk_Cruzer_Blade_4C53-0:0");
    let usb = stick.facts.usb().unwrap();
    assert_eq!((usb.vid, usb.pid), (0x0781, 0x5567));
    assert_eq!(usb.driver.as_deref(), Some("usb-storage"));
    assert_eq!(stick.facts.partitions.len(), 1);
    assert_eq!(stick.facts.partitions[0].start_bytes, Some(2048 * 512));
    assert_eq!(stick.identity.diskseq, Some(116));
    // Partitions are never offered on their own.
    assert!(scan.find("sdb1").is_err());
}

#[test]
fn usb_hard_drive_is_hidden_unless_requested() {
    let mut system = FakeSystem::new();
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

    let hidden = run(&system, ListingOptions::default());
    let drive = entry(&hidden, "sdc");
    assert!(!drive.decision.listed);
    assert_eq!(codes(drive), [ReasonCode::UsbHddHidden]);
    assert!(drive.decision.hdd_score.as_ref().unwrap().is_hdd());

    let shown = run(
        &system,
        ListingOptions {
            list_usb_hdd: true,
            ..ListingOptions::default()
        },
    );
    assert!(entry(&shown, "sdc").decision.listed);
}

#[test]
fn uas_drive_is_classified_as_uas() {
    let mut system = FakeSystem::new();
    system.add_disk(&Disk {
        bus: Bus::Usb {
            vid: 0x0781,
            pid: 0x5581,
            driver: "uas",
            serial: "AA01",
        },
        model: "Extreme",
        ..Disk::usb_stick("sdd", (8, 48), STICK * 4)
    });
    let scan = run(&system, ListingOptions::default());
    let drive = entry(&scan, "sdd");
    assert_eq!(drive.decision.category, Category::UasDrive);
    assert!(drive.facts.usb().unwrap().is_uas());
    assert!(drive.decision.hdd_score.is_some());
}

#[test]
fn sd_card_in_an_sd_host_is_a_listed_card() {
    let mut system = FakeSystem::new();
    system.add_disk(&disk(
        "mmcblk0",
        (179, 0),
        STICK,
        false,
        Bus::Mmc { card_type: "SD" },
    ));
    let scan = run(&system, ListingOptions::default());
    let card = entry(&scan, "mmcblk0");
    assert_eq!(
        card.facts.transport,
        Transport::Mmc {
            card_type: Some("SD".to_string())
        }
    );
    assert_eq!(card.decision.category, Category::CardReader);
    assert!(card.decision.listed, "{:?}", card.decision);
    assert_eq!(card.facts.serial.as_deref(), Some("0x1234abcd"));
}

#[test]
fn emmc_is_internal() {
    let mut system = FakeSystem::new();
    system.add_disk(&disk(
        "mmcblk1",
        (179, 8),
        STICK,
        false,
        Bus::Mmc { card_type: "MMC" },
    ));
    let scan = run(&system, ListingOptions::default());
    let emmc = entry(&scan, "mmcblk1");
    assert!(!emmc.decision.listed);
    assert_eq!(codes(emmc), [ReasonCode::InternalDisk]);
}

#[test]
fn card_reader_without_media_is_excluded() {
    let mut system = FakeSystem::new();
    system.add_disk(&Disk {
        model: "SD/MMC Reader",
        ..Disk::usb_stick("sde", (8, 64), 0)
    });
    let scan = run(&system, ListingOptions::default());
    let reader = entry(&scan, "sde");
    assert!(!reader.decision.listed);
    assert!(codes(reader).contains(&ReasonCode::NoMedia));
}

#[test]
fn internal_disks_are_never_listed() {
    let mut system = FakeSystem::new();
    system.add_disk(&disk("nvme0n1", (259, 0), STICK * 32, false, Bus::Nvme));
    system.add_disk(&disk("sda", (8, 0), STICK * 32, false, Bus::Sata));
    system.add_disk(&disk("vda", (253, 0), STICK * 2, false, Bus::Virtio));
    let all = ListingOptions {
        list_usb_hdd: true,
        list_non_usb_removable: true,
        list_virtual: true,
        ignore_usb: Vec::new(),
    };
    for options in [ListingOptions::default(), all] {
        let scan = run(&system, options);
        assert!(listed_names(&scan).is_empty());
        assert_eq!(entry(&scan, "nvme0n1").facts.transport, Transport::Nvme);
        assert_eq!(entry(&scan, "sda").facts.transport, Transport::Scsi);
        assert_eq!(entry(&scan, "vda").facts.transport, Transport::Virtio);
        for name in ["nvme0n1", "sda", "vda"] {
            assert_eq!(entry(&scan, name).decision.category, Category::Internal);
            assert!(codes(entry(&scan, name)).contains(&ReasonCode::InternalDisk));
        }
    }
}

#[test]
fn non_usb_removable_needs_the_option() {
    let mut system = FakeSystem::new();
    system.add_disk(&disk("sdf", (8, 80), STICK, true, Bus::Sata));
    let hidden = run(&system, ListingOptions::default());
    assert_eq!(
        entry(&hidden, "sdf").decision.category,
        Category::NonUsbRemovable
    );
    assert_eq!(
        codes(entry(&hidden, "sdf")),
        [ReasonCode::NonUsbRemovableHidden]
    );
    let shown = run(
        &system,
        ListingOptions {
            list_non_usb_removable: true,
            ..ListingOptions::default()
        },
    );
    assert!(entry(&shown, "sdf").decision.listed);
}

#[test]
fn loop_devices_need_the_option() {
    let mut system = FakeSystem::new();
    system.add_disk(&disk(
        "loop0",
        (7, 0),
        STICK,
        false,
        Bus::Loop {
            backing_file: Some("/home/user/disk.img"),
        },
    ));
    system.add_disk(&disk(
        "loop1",
        (7, 1),
        0,
        false,
        Bus::Loop { backing_file: None },
    ));
    let hidden = run(&system, ListingOptions::default());
    let attached = entry(&hidden, "loop0");
    assert_eq!(attached.decision.category, Category::VirtualDisk);
    assert_eq!(codes(attached), [ReasonCode::VirtualHidden]);
    assert_eq!(
        attached.facts.transport,
        Transport::Loop {
            backing_file: Some("/home/user/disk.img".to_string())
        }
    );

    let shown = run(
        &system,
        ListingOptions {
            list_virtual: true,
            ..ListingOptions::default()
        },
    );
    assert_eq!(listed_names(&shown), ["loop0"]);
    assert_eq!(codes(entry(&shown, "loop1")), [ReasonCode::NoMedia]);
}

#[test]
fn device_mapper_and_zram_are_other() {
    let mut system = FakeSystem::new();
    let dir = system.add_disk(&Disk::usb_stick("sdb", (8, 16), STICK));
    system.add_partition(&dir, &partition("sdb1", (8, 17), 1));
    system.add_virtual("dm-0", (253, 0), 1_000_000, &["sdb1"]);
    system.add_virtual("zram0", (252, 0), 1_000_000, &[]);
    let scan = run(
        &system,
        ListingOptions {
            list_virtual: true,
            ..ListingOptions::default()
        },
    );
    for name in ["dm-0", "zram0"] {
        let device = entry(&scan, name);
        assert_eq!(device.decision.category, Category::Other);
        assert_eq!(codes(device), [ReasonCode::UnsupportedType]);
    }
    // The stick itself is in use but not by the system: listed, with holders.
    let stick = entry(&scan, "sdb");
    assert!(stick.decision.listed);
    assert_eq!(stick.facts.all_holders(), ["dm-0"]);
}

#[test]
fn tiny_devices_are_excluded() {
    let mut system = FakeSystem::new();
    system.add_disk(&Disk::usb_stick("sdb", (8, 16), 8_000));
    let scan = run(&system, ListingOptions::default());
    assert_eq!(codes(entry(&scan, "sdb")), [ReasonCode::TooSmall]);
}

#[test]
fn ignored_usb_ids_are_excluded() {
    let mut system = FakeSystem::new();
    system.add_disk(&Disk::usb_stick("sdb", (8, 16), STICK));
    let scan = run(
        &system,
        ListingOptions {
            ignore_usb: vec![(0x0781, 0x5567)],
            ..ListingOptions::default()
        },
    );
    assert_eq!(codes(entry(&scan, "sdb")), [ReasonCode::Ignored]);
}

#[test]
fn read_only_media_is_reported() {
    let mut system = FakeSystem::new();
    system.add_disk(&Disk {
        read_only: true,
        ..Disk::usb_stick("sdb", (8, 16), STICK)
    });
    let scan = run(&system, ListingOptions::default());
    let stick = entry(&scan, "sdb");
    assert!(stick.facts.read_only);
    // Listed as in Rufus; the planner refuses to write to it.
    assert!(stick.decision.listed);
}

#[test]
fn listed_devices_are_sorted_by_size_and_none_is_selected() {
    let mut system = FakeSystem::new();
    system.add_disk(&Disk::usb_stick("sdb", (8, 16), STICK * 4));
    system.add_disk(&Disk::usb_stick("sdc", (8, 32), STICK));
    system.add_disk(&Disk::usb_stick("sdd", (8, 48), STICK * 2));
    system.add_disk(&disk("sda", (8, 0), STICK, false, Bus::Sata));
    let scan = run(&system, ListingOptions::default());
    assert_eq!(listed_names(&scan), ["sdc", "sdd", "sdb"]);
    // Excluded devices come after the listed ones.
    assert_eq!(scan.devices.last().unwrap().facts.name, "sda");
}

#[test]
fn find_accepts_names_nodes_and_ids() {
    let mut system = FakeSystem::new();
    system.add_disk(&Disk::usb_stick("sdb", (8, 16), STICK));
    system.udev((8, 16), &[], &["disk/by-id/usb-Stick_1-0:0"]);
    let scan = run(&system, ListingOptions::default());
    for query in [
        "sdb",
        "/dev/sdb",
        " sdb\n",
        "usb-Stick_1-0:0",
        "/dev/disk/by-id/usb-Stick_1-0:0",
    ] {
        assert_eq!(scan.find(query).unwrap().facts.name, "sdb", "{query}");
    }
    assert_eq!(
        scan.find("sdz").unwrap_err().kind(),
        ErrorKind::DeviceNotFound
    );
    assert_eq!(
        scan.find("/dev/sdb1").unwrap_err().kind(),
        ErrorKind::DeviceNotFound
    );
}

#[test]
fn root_on_a_usb_partition_is_a_system_disk() {
    let mut system = FakeSystem::new();
    let dir = system.add_disk(&Disk::usb_stick("sdb", (8, 16), STICK));
    system.add_partition(&dir, &partition("sdb1", (8, 17), 1));
    system.add_disk(&Disk::usb_stick("sdc", (8, 32), STICK));
    system.mount((8, 17), "/", "ext4", "/dev/sdb1", "rw");
    system.mount((0, 22), "/proc", "proc", "proc", "rw");
    let scan = run(&system, ListingOptions::default());
    let root = entry(&scan, "sdb");
    assert!(!root.decision.listed);
    assert_eq!(codes(root), [ReasonCode::SystemDisk]);
    assert_eq!(root.mounts, ["/"]);
    assert_eq!(listed_names(&scan), ["sdc"]);
    assert!(
        scan.system.unresolved.is_empty(),
        "{:?}",
        scan.system.unresolved
    );
}

#[test]
fn lvm_on_luks_root_is_traced_to_its_disk() {
    let mut system = FakeSystem::new();
    let dir = system.add_disk(&Disk::usb_stick("sdb", (8, 16), STICK));
    system.add_partition(&dir, &partition("sdb1", (8, 17), 1));
    system.add_partition(&dir, &partition("sdb2", (8, 18), 2));
    system.add_virtual("dm-0", (253, 0), 900_000, &["sdb2"]);
    system.add_virtual("dm-1", (253, 1), 800_000, &["dm-0"]);
    system.add_dev_link("/dev/mapper/vg-root", "dm-1");
    system.mount((253, 1), "/", "ext4", "/dev/mapper/vg-root", "rw");
    system.mount((8, 17), "/boot", "ext4", "/dev/sdb1", "rw");
    let scan = run(&system, ListingOptions::default());
    let root = entry(&scan, "sdb");
    assert_eq!(codes(root), [ReasonCode::SystemDisk]);
    let reasons = &scan.system.system_disks["sdb"];
    assert!(reasons.iter().any(|r| r == "backs /"), "{reasons:?}");
    assert!(reasons.iter().any(|r| r == "backs /boot"), "{reasons:?}");
}

#[test]
fn btrfs_on_an_anonymous_device_covers_every_member() {
    let mut system = FakeSystem::new();
    let first = system.add_disk(&Disk::usb_stick("sdb", (8, 16), STICK));
    system.add_partition(&first, &partition("sdb1", (8, 17), 1));
    let second = system.add_disk(&Disk::usb_stick("sdc", (8, 32), STICK));
    system.add_partition(&second, &partition("sdc1", (8, 33), 1));
    system.btrfs("0b4e9c2a-1111-2222-3333-444455556666", &["sdb1", "sdc1"]);
    system.mount((0, 35), "/", "btrfs", "/dev/sdb1", "rw,subvol=/@");
    let scan = run(&system, ListingOptions::default());
    for name in ["sdb", "sdc"] {
        assert_eq!(
            codes(entry(&scan, name)),
            [ReasonCode::SystemDisk],
            "{name}"
        );
    }
}

#[test]
fn active_swap_marks_its_disk() {
    let mut system = FakeSystem::new();
    let dir = system.add_disk(&Disk::usb_stick("sdb", (8, 16), STICK));
    system.add_partition(&dir, &partition("sdb1", (8, 17), 1));
    system.swap("/dev/sdb1", "partition");
    let scan = run(&system, ListingOptions::default());
    assert_eq!(codes(entry(&scan, "sdb")), [ReasonCode::SystemDisk]);
    assert_eq!(
        scan.system.system_disks["sdb"],
        ["holds active swap (/dev/sdb1)"]
    );
}

#[test]
fn the_disk_holding_the_program_is_excluded() {
    let mut system = FakeSystem::new();
    let dir = system.add_disk(&Disk::usb_stick("sdb", (8, 16), STICK));
    system.add_partition(&dir, &partition("sdb1", (8, 17), 1));
    system.add_disk(&Disk::usb_stick("sdc", (8, 32), STICK));
    system.mount((8, 17), "/media/user/TOOLS", "vfat", "/dev/sdb1", "rw");
    let appimage = PathBuf::from("/media/user/TOOLS/Ferrufus-x86_64.AppImage");
    let scan = run_with(
        &system,
        ScanOptions {
            listing: ListingOptions::default(),
            program_paths: vec![appimage],
        },
    );
    let tools = entry(&scan, "sdb");
    assert_eq!(codes(tools), [ReasonCode::SystemDisk]);
    assert_eq!(tools.mounts, ["/media/user/TOOLS"]);
    assert_eq!(listed_names(&scan), ["sdc"]);
}

#[test]
fn a_mounted_but_non_critical_stick_stays_listed() {
    let mut system = FakeSystem::new();
    let dir = system.add_disk(&Disk::usb_stick("sdb", (8, 16), STICK));
    system.add_partition(&dir, &partition("sdb1", (8, 17), 1));
    system.mount((8, 17), "/media/user/MY STICK", "vfat", "/dev/sdb1", "rw");
    let scan = run(&system, ListingOptions::default());
    let stick = entry(&scan, "sdb");
    assert!(stick.decision.listed);
    assert_eq!(stick.mounts, ["/media/user/MY STICK"]);
}

#[test]
fn an_unknown_root_source_is_unresolved() {
    let mut system = FakeSystem::new();
    system.mount((0, 40), "/", "zfs", "rpool/ROOT/default", "rw");
    let scan = run(&system, ListingOptions::default());
    assert_eq!(
        scan.system.unresolved.len(),
        1,
        "{:?}",
        scan.system.unresolved
    );
    assert!(scan.system.unresolved[0].contains("rpool/ROOT/default"));
}

#[test]
fn image_files_are_located_on_their_disk() {
    let mut system = FakeSystem::new();
    let dir = system.add_disk(&Disk::usb_stick("sdb", (8, 16), STICK));
    system.add_partition(&dir, &partition("sdb1", (8, 17), 1));
    system.mount((8, 17), "/media/user/STICK", "vfat", "/dev/sdb1", "rw");
    let root = SysRoot::new(system.root());
    let mount_list = mounts::parse_mountinfo(&root.read("/proc/self/mountinfo").unwrap());
    let found = SystemInfo::disks_of_path(
        &root,
        &mount_list,
        Path::new("/media/user/STICK/images/../a.iso"),
    );
    assert_eq!(found.disks.into_iter().collect::<Vec<_>>(), ["sdb"]);
    let elsewhere = SystemInfo::disks_of_path(&root, &mount_list, Path::new("/tmp/a.iso"));
    assert!(elsewhere.disks.is_empty());
    assert!(!elsewhere.unresolved.is_empty());
}

#[test]
fn revalidation_detects_replacement_and_removal() {
    let mut system = FakeSystem::new();
    let dir = system.add_disk(&Disk::usb_stick("sdb", (8, 16), STICK));
    let root = SysRoot::new(system.root());
    let identity = run(&system, ListingOptions::default())
        .find("sdb")
        .unwrap()
        .identity
        .clone();
    assert_eq!(revalidate(&root, &identity), Revalidation::Same);

    // Another stick in the same port: new disk sequence number and size.
    fs::write(system.root().join(&dir).join("diskseq"), "200\n").unwrap();
    fs::write(system.root().join(&dir).join("size"), "62500000\n").unwrap();
    let Revalidation::Changed { changes } = revalidate(&root, &identity) else {
        panic!("change not detected");
    };
    let fields: Vec<_> = changes.iter().map(|c| c.field).collect();
    assert_eq!(fields, ["diskseq", "size"]);

    fs::remove_file(system.root().join("sys/class/block/sdb")).unwrap();
    assert_eq!(revalidate(&root, &identity), Revalidation::Gone);
}

#[test]
fn identity_round_trips_through_json() {
    let mut system = FakeSystem::new();
    system.add_disk(&Disk::usb_stick("sdb", (8, 16), STICK));
    let scan = run(&system, ListingOptions::default());
    let identity = &entry(&scan, "sdb").identity;
    let json = serde_json::to_string(identity).unwrap();
    let back = serde_json::from_str(&json).unwrap();
    assert_eq!(identity, &back);
}

/// Reads the running system (no writes, no device opened): the scan works
/// and no disk backing `/` is ever offered.
#[test]
fn host_scan_never_offers_the_root_disk() {
    let root = SysRoot::host();
    if !root.path("/sys/class/block").is_dir() {
        return;
    }
    let all = ListingOptions {
        list_usb_hdd: true,
        list_non_usb_removable: true,
        list_virtual: true,
        ignore_usb: Vec::new(),
    };
    let scan = scan(&root, &ScanOptions::for_this_process(all)).unwrap();
    let mount_list =
        mounts::parse_mountinfo(&root.read("/proc/self/mountinfo").unwrap_or_default());
    let backing = SystemInfo::disks_of_path(&root, &mount_list, Path::new("/"));
    for disk in &backing.disks {
        assert!(
            scan.listed().all(|d| &d.facts.name != disk),
            "{disk} backs / but is listed"
        );
    }
    for device in scan.listed() {
        assert!(!scan.system.system_disks.contains_key(&device.facts.name));
    }
}
