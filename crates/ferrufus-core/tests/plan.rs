//! The DD dry-run planner, on fake systems and synthetic images.

// Test code: a failed expectation should panic.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use ferrufus_core::device::policy::ListingOptions;
use ferrufus_core::device::sysfs::SysRoot;
use ferrufus_core::device::system::{Resolution, SystemInfo};
use ferrufus_core::device::{DeviceScan, ScanOptions, mounts, scan};
use ferrufus_core::image::ms_sys::{MbrKind, reference_sector};
use ferrufus_core::image::{ImageReport, InspectOptions, inspect_source};
use ferrufus_core::plan::{CheckStatus, Plan, PlanOptions, plan_dd};
use ferrufus_testkit::images::{self, IsoBuilder};
use ferrufus_testkit::isofs::{IsoFs, Node};
use ferrufus_testkit::system::{Bus, Disk, FakeSystem, Partition};
use proptest::prelude::*;
use std::path::Path;

const MB: usize = 1024 * 1024;
/// 8 MB in sectors: the smallest listed device.
const SMALL: u64 = 16_384;

struct Setup {
    system: FakeSystem,
}

impl Setup {
    /// An NVMe system disk mounted at `/`, and a USB stick `sdb` of
    /// `sectors` with one labelled partition, mounted under /media.
    fn new(sectors: u64, stick: impl FnOnce(Disk) -> Disk) -> Self {
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
        let stick_dir = system.add_disk(&stick(Disk::usb_stick("sdb", (8, 16), sectors)));
        system.add_partition(
            &stick_dir,
            &Partition {
                name: "sdb1",
                dev: (8, 17),
                number: 1,
                start: 2048,
                sectors: sectors / 2,
            },
        );
        system.udev(
            (8, 17),
            &[("ID_FS_TYPE", "vfat"), ("ID_FS_LABEL", "PHOTOS")],
            &[],
        );
        system.mount((8, 17), "/media/user/PHOTOS", "vfat", "/dev/sdb1", "rw");
        Self { system }
    }

    fn root(&self) -> SysRoot {
        SysRoot::new(self.system.root())
    }

    fn scan(&self) -> DeviceScan {
        let options = ScanOptions {
            listing: ListingOptions::default(),
            program_paths: Vec::new(),
        };
        scan(&self.root(), &options).unwrap()
    }

    fn locate(&self, image: &str) -> Resolution {
        let root = self.root();
        let mount_list = mounts::parse_mountinfo(&root.read("/proc/self/mountinfo").unwrap());
        SystemInfo::disks_of_path(&root, &mount_list, Path::new(image))
    }

    fn plan(&self, device: &str, image: &ImageReport, options: PlanOptions) -> Plan {
        let scan = self.scan();
        plan_dd(
            scan.find(device).unwrap(),
            &scan.system,
            image,
            &self.locate(&image.path),
            options,
        )
    }
}

fn report(name: &str, data: &[u8]) -> ImageReport {
    inspect_source(name, &data.to_vec(), &InspectOptions::default()).unwrap()
}

fn disk_image(name: &str, size: usize) -> ImageReport {
    report(
        name,
        &images::disk_image(&reference_sector(MbrKind::Grub2), size),
    )
}

fn status(plan: &Plan, id: &str) -> Option<CheckStatus> {
    plan.checks.iter().find(|c| c.id == id).map(|c| c.status)
}

fn failures(plan: &Plan) -> Vec<&'static str> {
    plan.failures().map(|c| c.id).collect()
}

#[test]
fn a_good_plan_is_still_not_executable() {
    let setup = Setup::new(SMALL * 4, |d| d);
    let image = disk_image("/home/user/os.img", 4 * MB);
    let plan = setup.plan("sdb", &image, PlanOptions::default());
    assert_eq!(failures(&plan), Vec::<&str>::new(), "{:#?}", plan.checks);
    assert!(!plan.blocked);
    assert!(!plan.executable);
    assert_eq!(status(&plan, "mounts"), Some(CheckStatus::Warn));
    assert_eq!(plan.payload_size, Some(4 * MB as u64));
    assert_eq!(plan.padding_bytes, 0);
    assert!(plan.steps.iter().all(|s| !s.implemented));
    let ids: Vec<_> = plan.steps.iter().map(|s| s.id).collect();
    assert_eq!(
        ids,
        [
            "revalidate",
            "open-exclusive",
            "unmount",
            "clear-partition-tables",
            "write",
            "flush",
            "verify",
            "rescan"
        ]
    );
    assert_eq!(plan.fingerprint.len(), 64);
    assert_eq!(plan.device.name, "sdb");

    let loss = &plan.data_loss;
    assert_eq!(loss.partitions.len(), 1);
    assert_eq!(loss.partitions[0].label.as_deref(), Some("PHOTOS"));
    assert_eq!(loss.mounts, ["/media/user/PHOTOS"]);
    assert!(
        loss.summary.starts_with("ALL DATA ON /dev/sdb"),
        "{}",
        loss.summary
    );
}

#[test]
fn the_fingerprint_covers_the_options_and_is_stable() {
    let setup = Setup::new(SMALL * 4, |d| d);
    let image = disk_image("/home/user/os.img", MB);
    let first = setup.plan("sdb", &image, PlanOptions::default());
    let again = setup.plan("sdb", &image, PlanOptions::default());
    let unverified = setup.plan("sdb", &image, PlanOptions { verify: false });
    assert_eq!(first.fingerprint, again.fingerprint);
    assert_ne!(first.fingerprint, unverified.fingerprint);
    assert!(unverified.steps.iter().all(|s| s.id != "verify"));
}

#[test]
fn the_system_disk_is_refused() {
    let setup = Setup::new(SMALL * 4, |d| d);
    let image = disk_image("/home/user/os.img", MB);
    let plan = setup.plan("nvme0n1", &image, PlanOptions::default());
    assert!(plan.blocked);
    let failed = failures(&plan);
    assert!(failed.contains(&"not-system-disk"), "{failed:?}");
    assert!(failed.contains(&"target-listed"), "{failed:?}");
    // And the image is on that disk too.
    assert!(failed.contains(&"image-not-on-target"), "{failed:?}");
}

#[test]
fn an_image_on_the_target_is_refused() {
    let setup = Setup::new(SMALL * 4, |d| d);
    let image = disk_image("/media/user/PHOTOS/os.img", MB);
    let plan = setup.plan("sdb", &image, PlanOptions::default());
    assert_eq!(failures(&plan), ["image-not-on-target"]);
}

#[test]
fn an_image_whose_disk_is_unknown_is_refused() {
    let setup = Setup::new(SMALL * 4, |d| d);
    let image = disk_image("relative/os.img", MB);
    let plan = setup.plan("sdb", &image, PlanOptions::default());
    assert_eq!(failures(&plan), ["image-not-on-target"]);
}

#[test]
fn size_limits_are_exact() {
    let setup = Setup::new(SMALL, |d| d);
    let device_size = 8 * MB;
    let exact = setup.plan(
        "sdb",
        &disk_image("/home/user/a.img", device_size),
        PlanOptions::default(),
    );
    assert_eq!(status(&exact, "fits"), Some(CheckStatus::Pass));
    assert!(!exact.blocked);

    // Upstream allows 4 KB of slack; Ferrufus never truncates.
    let over = setup.plan(
        "sdb",
        &disk_image("/home/user/a.img", device_size + 512),
        PlanOptions::default(),
    );
    assert_eq!(failures(&over), ["fits"]);

    // A partial last sector is padded, and the padding must fit too.
    let partial = setup.plan(
        "sdb",
        &disk_image("/home/user/a.img", device_size - 100),
        PlanOptions::default(),
    );
    assert_eq!(partial.padding_bytes, 100);
    assert_eq!(status(&partial, "sector-padding"), Some(CheckStatus::Warn));
    assert_eq!(status(&partial, "fits"), Some(CheckStatus::Pass));
    let barely_over = setup.plan(
        "sdb",
        &disk_image("/home/user/a.img", device_size + 1),
        PlanOptions::default(),
    );
    assert_eq!(failures(&barely_over), ["fits"]);
}

#[test]
fn read_only_media_is_refused() {
    let setup = Setup::new(SMALL * 4, |d| Disk {
        read_only: true,
        ..d
    });
    let plan = setup.plan(
        "sdb",
        &disk_image("/home/user/a.img", MB),
        PlanOptions::default(),
    );
    assert_eq!(failures(&plan), ["writable"]);
}

#[test]
fn a_device_in_use_is_refused() {
    let mut setup = Setup::new(SMALL * 4, |d| d);
    setup.system.add_virtual("dm-3", (253, 3), 1000, &["sdb1"]);
    let plan = setup.plan(
        "sdb",
        &disk_image("/home/user/a.img", MB),
        PlanOptions::default(),
    );
    assert_eq!(failures(&plan), ["not-in-use"]);
}

#[test]
fn unresolved_system_mounts_block_every_plan() {
    let mut setup = Setup::new(SMALL * 4, |d| d);
    setup
        .system
        .mount((0, 50), "/var", "zfs", "rpool/var", "rw");
    let plan = setup.plan(
        "sdb",
        &disk_image("/home/user/a.img", MB),
        PlanOptions::default(),
    );
    assert_eq!(failures(&plan), ["system-resolved"]);
}

#[test]
fn hidden_devices_are_refused() {
    let setup = Setup::new(SMALL * 4, |d| Disk {
        removable: false,
        bus: Bus::Sata,
        ..d
    });
    let plan = setup.plan(
        "sdb",
        &disk_image("/home/user/a.img", MB),
        PlanOptions::default(),
    );
    assert_eq!(failures(&plan), ["target-listed"]);
}

#[test]
fn image_types_follow_the_upstream_rules() {
    let setup = Setup::new(SMALL * 4, |d| d);
    let plain_iso = IsoBuilder::new("PLAIN").blocks(64).build();
    // An ISOHybrid with an ISO boot method (Syslinux), and one without,
    // which Rufus only writes in DD mode.
    let mut hybrid_iso = IsoFs::new(
        "HYBRID",
        vec![
            Node::file("isolinux.cfg", b"x"),
            Node::file("isolinux.bin", &[0; 4096]),
        ],
    )
    .build();
    hybrid_iso[..512].copy_from_slice(&reference_sector(MbrKind::Isolinux));
    let dd_only_iso = IsoBuilder::new("DDONLY")
        .hybrid(reference_sector(MbrKind::Isolinux))
        .blocks(64)
        .build();
    let mut gzip = vec![0x1f, 0x8b, 8, 0];
    gzip.resize(4096, 0);

    let plan =
        |name: &str, data: &[u8]| setup.plan("sdb", &report(name, data), PlanOptions::default());
    assert_eq!(
        failures(&plan("/home/user/plain.iso", &plain_iso)),
        ["image-supported"]
    );
    let hybrid = plan("/home/user/hybrid.iso", &hybrid_iso);
    assert!(!hybrid.blocked, "{:#?}", hybrid.checks);
    assert_eq!(status(&hybrid, "image-supported"), Some(CheckStatus::Warn));
    let dd_only = plan("/home/user/dd-only.iso", &dd_only_iso);
    assert_eq!(status(&dd_only, "image-supported"), Some(CheckStatus::Pass));
    assert_eq!(
        failures(&plan("/home/user/blank.img", &[0u8; 4096])),
        ["image-supported"]
    );
    assert!(
        failures(&plan("/home/user/install.wim", &images::wim_header()))
            .contains(&"image-supported")
    );
    assert!(failures(&plan("/home/user/os.img.gz", &gzip)).contains(&"image-supported"));
    assert!(failures(&plan("/home/user/fake.img.gz", &[0u8; 4096])).contains(&"image-format"));
}

#[test]
fn a_fixed_vhd_writes_its_disk_without_the_footer() {
    let setup = Setup::new(SMALL * 4, |d| d);
    let disk = images::disk_image(&reference_sector(MbrKind::Grub2), MB);
    let plan = setup.plan(
        "sdb",
        &report("/home/user/disk.vhd", &images::vhd_fixed(&disk)),
        PlanOptions::default(),
    );
    assert_eq!(plan.payload_size, Some(MB as u64));
    assert!(!plan.blocked, "{:#?}", plan.checks);
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 64, ..ProptestConfig::default() })]

    /// Whatever the sizes, the planner never panics and only accepts a
    /// payload that fits once padded to whole sectors.
    #[test]
    fn size_checks_are_sound(image_size in any::<u64>(), device_size in any::<u64>(), sector in prop::sample::select(vec![512u32, 4096])) {
        let setup = Setup::new(SMALL * 4, |d| d);
        let scan = setup.scan();
        let mut device = scan.find("sdb").unwrap().clone();
        device.facts.size_bytes = device_size;
        device.facts.logical_block_size = sector;
        let mut image = disk_image("/home/user/a.img", 4096);
        image.image_size = image_size;
        let plan = plan_dd(&device, &scan.system, &image, &setup.locate(&image.path), PlanOptions::default());
        let padded = image_size.div_ceil(u64::from(sector)).checked_mul(u64::from(sector));
        let fits = image_size > 0 && padded.is_some_and(|p| p <= device_size);
        prop_assert_eq!(status(&plan, "fits") == Some(CheckStatus::Pass), fits);
        prop_assert!(!plan.executable);
    }
}

mod iso_and_format {
    use super::*;
    use ferrufus_core::options::{Choices, FileSystem, PartitionScheme, TargetSystem};
    use ferrufus_core::plan::{FormatRequest, plan_format_mode};

    /// 4 GiB: FAT32 needs at least 64 MB in Rufus' cluster tables.
    const STICK_4G: u64 = 8_388_608;

    fn iso(root: Vec<Node>) -> ImageReport {
        let mut fs = IsoFs::new("LIVE", root);
        fs.rock_ridge = true;
        report("/home/user/live.iso", &fs.build())
    }

    fn syslinux(version: &str) -> Vec<Node> {
        let mut bin = vec![0u8; 4096];
        let text = format!("ISOLINUX {version} ");
        bin[1000..1000 + text.len()].copy_from_slice(text.as_bytes());
        vec![
            Node::file("isolinux.cfg", b"x"),
            Node::file("isolinux.bin", &bin),
        ]
    }

    fn plan_with(setup: &Setup, image: Option<&ImageReport>, choices: Choices) -> Plan {
        let scan = setup.scan();
        let location = image.map(|i| setup.locate(&i.path));
        let request = FormatRequest {
            image,
            image_location: location.as_ref(),
            choices,
            persistence: 0,
            old_bios_fixes: false,
        };
        plan_format_mode(
            scan.find("sdb").unwrap(),
            &scan.system,
            &request,
            PlanOptions::default(),
        )
    }

    #[test]
    fn a_syslinux_iso_plans_fat32_with_one_partition() {
        let setup = Setup::new(STICK_4G, |d| d);
        let image = iso(syslinux("6.04 6.04-pre1"));
        let plan = plan_with(&setup, Some(&image), Choices::default());
        assert_eq!(plan.mode, "iso");
        assert!(!plan.blocked, "{:#?}", plan.checks);
        assert!(!plan.executable);
        let format = plan.format.as_ref().unwrap();
        assert_eq!(format.file_system, Some(FileSystem::Fat32));
        assert_eq!(format.partition_scheme, Some(PartitionScheme::Mbr));
        assert_eq!(format.label, "LIVE");
        assert_eq!(plan.layout.len(), 1);
        assert_eq!(plan.layout[0].mbr_type, Some(0x0c));
        assert!(plan.layout[0].active);
        // The embedded Syslinux matches: no download.
        assert_eq!(status(&plan, "syslinux-download"), None);
        let ids: Vec<_> = plan.steps.iter().map(|s| s.id).collect();
        assert!(
            ids.contains(&"extract") && ids.contains(&"write-mbr"),
            "{ids:?}"
        );
        assert!(plan.steps.iter().all(|s| !s.implemented));
    }

    #[test]
    fn other_syslinux_versions_need_a_download() {
        let setup = Setup::new(STICK_4G, |d| d);
        let image = iso(syslinux("6.03"));
        let plan = plan_with(&setup, Some(&image), Choices::default());
        assert_eq!(status(&plan, "syslinux-download"), Some(CheckStatus::Warn));
    }

    #[test]
    fn ntfs_with_syslinux_4_is_refused_for_bios() {
        let setup = Setup::new(STICK_4G, |d| d);
        let image = iso(syslinux("4.07 2013-07-25"));
        let choices = Choices {
            file_system: Some(FileSystem::Ntfs),
            ..Choices::default()
        };
        let plan = plan_with(&setup, Some(&image), choices);
        assert_eq!(
            plan.format.as_ref().unwrap().file_system,
            Some(FileSystem::Ntfs)
        );
        assert_eq!(failures(&plan), ["file-system-compat"]);
    }

    #[test]
    fn efi_isos_on_ntfs_get_a_uefi_ntfs_partition() {
        let setup = Setup::new(STICK_4G, |d| d);
        let image = iso(vec![Node::dir(
            "EFI",
            vec![Node::dir(
                "BOOT",
                vec![Node::file("BOOTX64.EFI", &[1; 4096])],
            )],
        )]);
        let choices = Choices {
            file_system: Some(FileSystem::Ntfs),
            partition_scheme: Some(PartitionScheme::Gpt),
            ..Choices::default()
        };
        let plan = plan_with(&setup, Some(&image), choices);
        assert_eq!(
            plan.format.as_ref().unwrap().target_system,
            Some(TargetSystem::Uefi)
        );
        let names: Vec<_> = plan.layout.iter().map(|p| p.name).collect();
        assert_eq!(names, ["Main Data Partition", "UEFI:NTFS"]);
        assert!(plan.steps.iter().any(|s| s.id == "uefi-ntfs"));
    }

    #[test]
    fn images_without_iso_mode_are_refused() {
        let setup = Setup::new(STICK_4G, |d| d);
        let disk = disk_image("/home/user/disk.img", MB);
        let plan = plan_with(&setup, Some(&disk), Choices::default());
        assert!(failures(&plan).contains(&"iso-mode"));
        let proxmox = iso(vec![
            Node::dir("proxmox", vec![Node::file("x", b"x")]),
            Node::dir("isolinux", syslinux("6.04")),
        ]);
        let plan = plan_with(&setup, Some(&proxmox), Choices::default());
        assert!(failures(&plan).contains(&"iso-mode"), "{:#?}", plan.checks);
    }

    #[test]
    fn plain_format() {
        let setup = Setup::new(STICK_4G, |d| d);
        let choices = Choices {
            label: Some("DATA".into()),
            ..Choices::default()
        };
        let plan = plan_with(&setup, None, choices);
        assert_eq!(plan.mode, "format");
        assert!(!plan.blocked, "{:#?}", plan.checks);
        assert_eq!(plan.image, None);
        let format = plan.format.as_ref().unwrap();
        assert_eq!(format.label, "DATA");
        assert_eq!(plan.layout.len(), 1);
        assert!(!plan.layout[0].active, "non-bootable");
        assert!(
            plan.steps
                .iter()
                .all(|s| s.id != "write-mbr" && s.id != "extract")
        );
        // Super Floppy Disk: no partitions.
        let sfd = Choices {
            partition_scheme: Some(PartitionScheme::Sfd),
            ..Choices::default()
        };
        let plan = plan_with(&setup, None, sfd);
        assert!(plan.layout.is_empty());
    }

    #[test]
    fn warnings_before_every_write() {
        // Two partitions: Rufus warns before erasing them.
        let mut setup = Setup::new(STICK_4G, |d| d);
        let root = std::fs::canonicalize(setup.system.root()).unwrap();
        let sdb = std::fs::canonicalize(root.join("sys/class/block/sdb")).unwrap();
        let sdb = sdb
            .strip_prefix(&root)
            .unwrap()
            .to_str()
            .unwrap()
            .to_string();
        setup.system.add_partition(
            &sdb,
            &Partition {
                name: "sdb2",
                dev: (8, 18),
                number: 2,
                start: 40000,
                sectors: 1000,
            },
        );
        let plan = plan_with(&setup, None, Choices::default());
        assert_eq!(
            status(&plan, "multiple-partitions"),
            Some(CheckStatus::Warn)
        );
        // MBR on a 3 TB device.
        let big = Setup::new(6_000_000_000, |d| Disk {
            removable: true,
            ..d
        });
        let choices = Choices {
            partition_scheme: Some(PartitionScheme::Mbr),
            ..Choices::default()
        };
        let plan = plan_with(&big, None, choices);
        // Over 2 TB, GPT is forced and MBR is not kept.
        assert_eq!(
            plan.format.as_ref().unwrap().partition_scheme,
            Some(PartitionScheme::Gpt)
        );
        assert_eq!(status(&plan, "mbr-2tb"), None);
    }
}
