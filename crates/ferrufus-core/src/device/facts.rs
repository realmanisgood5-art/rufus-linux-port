// SPDX-License-Identifier: GPL-3.0-or-later
//! Properties of whole-disk block devices, read from sysfs and the udev
//! database. This is the Linux counterpart of the SetupAPI and
//! `IOCTL_STORAGE_*` queries in Rufus `src/dev.c:GetDevices()` and
//! `src/dev.c:GetUSBProperties()`.

use super::DevNum;
use super::sysfs::SysRoot;
use super::udev_db::UdevRecord;
use serde::Serialize;
use std::path::{Path, PathBuf};

/// The bus or driver a disk is attached through.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "bus", rename_all = "kebab-case")]
pub enum Transport {
    /// USB mass storage, through `usb-storage` (bulk-only) or `uas`.
    Usb(UsbInfo),
    /// An MMC/SD host controller (`mmcblk*`).
    Mmc {
        /// Card type reported by the kernel: `SD`, `MMC` (eMMC) or `SDIO`.
        card_type: Option<String>,
    },
    /// NVMe.
    Nvme,
    /// SCSI disk not on USB (SATA, SAS, ...).
    Scsi,
    /// virtio block device.
    Virtio,
    /// Loop device.
    Loop {
        /// Backing file, when attached.
        backing_file: Option<String>,
    },
    /// Other virtual block devices (device mapper, MD RAID, zram, ...).
    Virtual {
        /// Name prefix, such as `dm` or `zram`.
        kind: String,
    },
    /// Optical drives.
    Optical,
    /// Anything else.
    Other,
}

/// USB device properties.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct UsbInfo {
    /// Vendor ID.
    pub vid: u16,
    /// Product ID.
    pub pid: u16,
    /// Interface driver: `usb-storage` or `uas`.
    pub driver: Option<String>,
    /// Negotiated speed in Mbit/s (sysfs `speed`).
    pub speed_mbps: Option<String>,
    /// USB version of the device (sysfs `version`, such as `3.20`).
    pub version: Option<String>,
    /// Manufacturer string.
    pub manufacturer: Option<String>,
    /// Product string.
    pub product: Option<String>,
    /// Serial number string.
    pub serial: Option<String>,
}

impl UsbInfo {
    /// Whether the device uses the USB Attached SCSI driver (Rufus `is_UASP`).
    pub fn is_uas(&self) -> bool {
        self.driver.as_deref() == Some("uas")
    }
}

/// A partition of a disk.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PartitionFacts {
    /// Kernel name, such as `sdb1`.
    pub name: String,
    /// Device node, such as `/dev/sdb1`.
    pub devnode: String,
    /// `major:minor`.
    pub dev: DevNum,
    /// Partition number.
    pub number: Option<u32>,
    /// Start offset in bytes.
    pub start_bytes: Option<u64>,
    /// Size in bytes.
    pub size_bytes: Option<u64>,
    /// File system type (from udev).
    pub fs_type: Option<String>,
    /// File system label (from udev).
    pub fs_label: Option<String>,
    /// File system UUID (from udev).
    pub fs_uuid: Option<String>,
    /// GPT partition name (from udev).
    pub part_name: Option<String>,
    /// Devices stacked on this partition (device mapper, MD RAID).
    pub holders: Vec<String>,
}

/// Everything known about a whole-disk block device.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DeviceFacts {
    /// Kernel name, such as `sdb`.
    pub name: String,
    /// Device node, such as `/dev/sdb`.
    pub devnode: String,
    /// `major:minor`.
    pub dev: DevNum,
    /// Disk sequence number (Linux 5.15+), unique for the boot.
    pub diskseq: Option<u64>,
    /// Size in bytes (0 when there is no medium).
    pub size_bytes: u64,
    /// Logical sector size.
    pub logical_block_size: u32,
    /// Physical sector size.
    pub physical_block_size: u32,
    /// The kernel's removable-media flag.
    pub removable: bool,
    /// Whether the device is read-only (for example a locked SD card).
    pub read_only: bool,
    /// How the disk is attached.
    pub transport: Transport,
    /// SCSI vendor identification, as reported (8 characters, space-padded).
    pub scsi_vendor: Option<String>,
    /// SCSI product identification, as reported (16 characters, space-padded).
    pub scsi_model: Option<String>,
    /// SCSI product revision, as reported.
    pub scsi_revision: Option<String>,
    /// Model name for display (SCSI product, MMC card name or udev model).
    pub model: Option<String>,
    /// Serial number.
    pub serial: Option<String>,
    /// The `/dev/disk/by-id/` name, when udev provides one.
    pub by_id: Option<String>,
    /// Partition table type (from udev: `gpt` or `dos`).
    pub partition_table: Option<String>,
    /// File system directly on the disk, without a partition table (from udev).
    pub fs_type: Option<String>,
    /// Label of that file system (from udev).
    pub fs_label: Option<String>,
    /// Canonical sysfs path of the device, as on the system (`/sys/devices/...`).
    pub sysfs_path: String,
    /// Partitions.
    pub partitions: Vec<PartitionFacts>,
    /// Devices stacked on the disk itself (device mapper, MD RAID).
    pub holders: Vec<String>,
}

impl DeviceFacts {
    /// Probes the whole disk `name` (`/sys/class/block/<name>`).
    pub fn probe(root: &SysRoot, name: &str) -> Option<Self> {
        let class_dir = root.path(format!("/sys/class/block/{name}"));
        let dir = root.canonical(&class_dir)?;
        if dir.join("partition").exists() {
            return None;
        }
        let dev = DevNum::parse(&root.read_at(&dir.join("dev"))?)?;
        let size_bytes = root
            .read_u64(&dir.join("size"))
            .unwrap_or(0)
            .checked_mul(512)
            .unwrap_or(0);
        let queue = dir.join("queue");
        let block_size = |file: &str| {
            root.read_u64(&queue.join(file))
                .and_then(|v| u32::try_from(v).ok())
                .unwrap_or(512)
        };
        let udev = UdevRecord::read(root, dev);
        let transport = transport(root, name, &dir);
        let device_dir = dir.join("device");
        let scsi = |file: &str| {
            root.read_at(&device_dir.join(file))
                .filter(|_| matches!(name.get(..2), Some("sd")))
        };
        let scsi_vendor = scsi("vendor");
        let scsi_model = scsi("model");
        let scsi_revision = scsi("rev");
        let udev_get = |key: &str| udev.as_ref().and_then(|u| u.get(key)).map(str::to_string);

        let model = scsi_model
            .as_deref()
            .map(str::trim)
            .filter(|m| !m.is_empty())
            .map(str::to_string)
            .or_else(|| root.read_at(&device_dir.join("name")))
            .or_else(|| {
                root.read_at(&device_dir.join("model"))
                    .map(|m| m.trim().to_string())
            })
            .or_else(|| udev_get("ID_MODEL"));
        let usb_serial = match &transport {
            Transport::Usb(usb) => usb.serial.clone(),
            _ => None,
        };
        let serial = udev_get("ID_SERIAL_SHORT")
            .or(usb_serial)
            .or_else(|| {
                root.read_at(&device_dir.join("serial"))
                    .map(|s| s.trim().to_string())
            })
            .or_else(|| {
                root.read_at(&dir.join("serial"))
                    .map(|s| s.trim().to_string())
            })
            .filter(|s| !s.is_empty());

        let partitions = root
            .list(&dir)
            .into_iter()
            .filter(|entry| dir.join(entry).join("partition").exists())
            .filter_map(|entry| probe_partition(root, &dir.join(&entry), &entry))
            .collect();

        Some(Self {
            name: name.to_string(),
            devnode: format!("/dev/{name}"),
            dev,
            diskseq: root.read_u64(&dir.join("diskseq")),
            size_bytes,
            logical_block_size: block_size("logical_block_size"),
            physical_block_size: block_size("physical_block_size"),
            removable: root.read_u64(&dir.join("removable")) == Some(1),
            read_only: root.read_u64(&dir.join("ro")) == Some(1),
            transport,
            scsi_vendor,
            scsi_model,
            scsi_revision,
            model,
            serial,
            by_id: udev.as_ref().and_then(|u| u.by_id()).map(str::to_string),
            partition_table: udev_get("ID_PART_TABLE_TYPE"),
            fs_type: udev_get("ID_FS_TYPE"),
            fs_label: udev_get("ID_FS_LABEL"),
            sysfs_path: root
                .system_path(&dir)
                .map(|p| p.display().to_string())
                .unwrap_or_default(),
            partitions,
            holders: root.list(&dir.join("holders")),
        })
    }

    /// USB properties, if the disk is on USB.
    pub fn usb(&self) -> Option<&UsbInfo> {
        match &self.transport {
            Transport::Usb(usb) => Some(usb),
            _ => None,
        }
    }

    /// Every device stacked on the disk or one of its partitions.
    pub fn all_holders(&self) -> Vec<String> {
        let mut holders = self.holders.clone();
        for partition in &self.partitions {
            holders.extend(partition.holders.iter().cloned());
        }
        holders.sort();
        holders.dedup();
        holders
    }
}

fn probe_partition(root: &SysRoot, dir: &Path, name: &str) -> Option<PartitionFacts> {
    let dev = DevNum::parse(&root.read_at(&dir.join("dev"))?)?;
    let udev = UdevRecord::read(root, dev);
    let udev_get = |key: &str| udev.as_ref().and_then(|u| u.get(key)).map(str::to_string);
    let sectors_to_bytes = |file: &str| {
        root.read_u64(&dir.join(file))
            .and_then(|v| v.checked_mul(512))
    };
    Some(PartitionFacts {
        name: name.to_string(),
        devnode: format!("/dev/{name}"),
        dev,
        number: root
            .read_u64(&dir.join("partition"))
            .and_then(|n| u32::try_from(n).ok()),
        start_bytes: sectors_to_bytes("start"),
        size_bytes: sectors_to_bytes("size"),
        fs_type: udev_get("ID_FS_TYPE"),
        fs_label: udev_get("ID_FS_LABEL"),
        fs_uuid: udev_get("ID_FS_UUID"),
        part_name: udev_get("ID_PART_ENTRY_NAME"),
        holders: root.list(&dir.join("holders")),
    })
}

fn transport(root: &SysRoot, name: &str, dir: &Path) -> Transport {
    let system_path = root.system_path(dir).unwrap_or_else(|| dir.to_path_buf());
    let components: Vec<String> = system_path
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    if name.starts_with("loop") {
        return Transport::Loop {
            backing_file: root.read_at(&dir.join("loop/backing_file")),
        };
    }
    if components.iter().any(|c| c == "virtual") {
        let kind: String = name
            .chars()
            .take_while(|c| c.is_ascii_alphabetic())
            .collect();
        return Transport::Virtual { kind };
    }
    let on_usb_bus = components.iter().any(|c| {
        c.starts_with("usb")
            && c.get(3..)
                .is_some_and(|n| n.chars().all(|d| d.is_ascii_digit()))
    });
    if on_usb_bus && let Some(usb) = usb_info(root, dir) {
        return Transport::Usb(usb);
    }
    if name.starts_with("nvme") {
        Transport::Nvme
    } else if name.starts_with("mmcblk") {
        Transport::Mmc {
            card_type: root.read_at(&dir.join("device/type")),
        }
    } else if name.starts_with("vd") || components.iter().any(|c| c.starts_with("virtio")) {
        Transport::Virtio
    } else if name.starts_with("sr") {
        Transport::Optical
    } else if name.starts_with("sd") {
        Transport::Scsi
    } else {
        Transport::Other
    }
}

/// Walks up from the disk to the USB device (the first ancestor with an
/// `idVendor` attribute); the directory just below it is the interface
/// whose driver is `usb-storage` or `uas`.
fn usb_info(root: &SysRoot, dir: &Path) -> Option<UsbInfo> {
    let mut child: Option<PathBuf> = None;
    let mut current = dir.parent();
    while let Some(candidate) = current {
        // Never walk above the system root.
        root.system_path(candidate)?;
        if candidate.join("idVendor").exists() {
            let hex = |file: &str| {
                root.read_at(&candidate.join(file))
                    .and_then(|v| u16::from_str_radix(v.trim(), 16).ok())
            };
            let text = |file: &str| {
                root.read_at(&candidate.join(file))
                    .map(|v| v.trim().to_string())
                    .filter(|v| !v.is_empty())
            };
            return Some(UsbInfo {
                vid: hex("idVendor")?,
                pid: hex("idProduct")?,
                driver: child.and_then(|interface| root.link_name(&interface.join("driver"))),
                speed_mbps: text("speed"),
                version: text("version"),
                manufacturer: text("manufacturer"),
                product: text("product"),
                serial: text("serial"),
            });
        }
        child = Some(candidate.to_path_buf());
        current = candidate.parent();
    }
    None
}
