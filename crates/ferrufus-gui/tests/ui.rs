//! The GUI, driven headlessly through its accessibility tree, on a fake
//! system tree. Nothing here touches a real device.

// Test code: a failed expectation should panic.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use eframe::egui::accesskit::Role;
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT, Queryable};
use ferrufus_core::image::ms_sys::{MbrKind, reference_sector};
use ferrufus_gui::{BANNER, FerrufusApp, NO_DEVICE, Options};
use ferrufus_testkit::images;
use ferrufus_testkit::system::{Bus, Disk, FakeSystem, Partition};
use std::path::PathBuf;

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
    // Exactly one eligible device: Rufus would select it; Ferrufus must not.
    system.add_disk(&Disk::usb_stick("sdb", (8, 16), 31_250_000));
    system
}

fn harness(system: &FakeSystem, image: Option<PathBuf>) -> Harness<'static, FerrufusApp> {
    let app = FerrufusApp::new(Options {
        sysroot: Some(system.root().to_path_buf()),
        image,
        synchronous: true,
        ..Options::default()
    });
    let mut harness = Harness::new_ui_state(|ui, app: &mut FerrufusApp| app.show(ui), app);
    harness.run();
    harness
}

fn button_labels(harness: &Harness<'_, FerrufusApp>) -> Vec<String> {
    harness
        .query_all_by_role(Role::Button)
        .filter_map(|node| node.accesskit_node().label())
        .collect()
}

#[test]
fn starts_read_only_with_nothing_selected() {
    let system = system();
    let harness = harness(&system, None);
    harness.get_by_label(BANNER);
    assert_eq!(harness.state().scan().unwrap().listed().count(), 1);
    assert!(harness.state().selected_device().is_none());
    let combo = harness.get_by_role_and_label(Role::ComboBox, "Device");
    assert_eq!(combo.accesskit_node().value().as_deref(), Some(NO_DEVICE));
}

#[test]
fn has_no_destructive_controls() {
    let system = system();
    let harness = harness(&system, None);
    let labels = button_labels(&harness);
    assert!(
        labels.iter().any(|l| l == "Review plan (dry run)"),
        "{labels:?}"
    );
    for label in &labels {
        let lower = label.to_lowercase();
        for word in ["start", "write", "format", "flash", "erase", "burn"] {
            assert!(
                !lower.contains(word),
                "destructive-looking button `{label}`"
            );
        }
    }
}

#[test]
fn reviews_a_dry_run_plan() {
    let system = system();
    let dir = tempfile::tempdir().unwrap();
    let image = images::write_file(
        dir.path(),
        "disk.img",
        &images::disk_image(&reference_sector(MbrKind::Grub2), 1024 * 1024),
    );
    let mut harness = harness(&system, Some(image));
    assert!(
        harness
            .state()
            .log()
            .iter()
            .any(|l| l.contains("DiskImage")),
        "{:?}",
        harness.state().log()
    );

    let stick = harness
        .state()
        .scan()
        .unwrap()
        .listed()
        .next()
        .unwrap()
        .stable_id
        .clone();
    assert!(harness.state_mut().select(&stick));
    harness.run();
    harness.get_by_label("Review plan (dry run)").click();
    harness.run();
    harness.get_by_label_contains("cannot execute it");
    harness.get_by_label_contains("ALL DATA ON /dev/sdb");
}

#[test]
fn a_removed_device_is_deselected_not_replaced() {
    let mut system = system();
    let mut harness = harness(&system, None);
    let stick = harness
        .state()
        .scan()
        .unwrap()
        .listed()
        .next()
        .unwrap()
        .stable_id
        .clone();
    harness.state_mut().select(&stick);
    harness.run();
    assert!(harness.state().selected_device().is_some());

    // Unplug it and plug in another stick.
    std::fs::remove_file(system.root().join("sys/class/block/sdb")).unwrap();
    system.add_disk(&Disk {
        bus: Bus::Usb {
            vid: 0x0951,
            pid: 0x1666,
            driver: "usb-storage",
            serial: "OTHER",
        },
        ..Disk::usb_stick("sdc", (8, 32), 31_250_000)
    });
    harness.get_by_label("Refresh").click();
    harness.run();
    assert!(harness.state().selected_device().is_none());
    assert_eq!(harness.state().scan().unwrap().listed().count(), 1);
    assert!(
        harness
            .state()
            .log()
            .iter()
            .any(|l| l.contains("selection cleared"))
    );
}

#[test]
fn excluded_devices_cannot_be_selected() {
    let system = system();
    let mut harness = harness(&system, None);
    let nvme = harness
        .state()
        .scan()
        .unwrap()
        .find("nvme0n1")
        .unwrap()
        .stable_id
        .clone();
    assert!(!harness.state_mut().select(&nvme));
    assert!(harness.state().selected_device().is_none());
}
