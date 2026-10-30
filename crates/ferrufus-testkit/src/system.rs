// SPDX-License-Identifier: GPL-3.0-or-later
//! A fake Linux system tree (`sys/`, `proc/`, `dev/`, `run/udev/data/`) in
//! a temporary directory, laid out like the real sysfs so that device
//! discovery can be tested without any real device.

use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

/// How a fake disk is attached.
#[derive(Debug, Clone)]
pub enum Bus {
    /// USB mass storage.
    Usb {
        /// Vendor ID.
        vid: u16,
        /// Product ID.
        pid: u16,
        /// `usb-storage` or `uas`.
        driver: &'static str,
        /// Serial number string.
        serial: &'static str,
    },
    /// SATA/SCSI disk on an ATA controller.
    Sata,
    /// NVMe namespace.
    Nvme,
    /// virtio block device.
    Virtio,
    /// MMC/SD host with a card of the given type (`SD` or `MMC`).
    Mmc {
        /// Card type.
        card_type: &'static str,
    },
    /// Loop device, attached to the given backing file.
    Loop {
        /// Backing file path, or `None` when detached.
        backing_file: Option<&'static str>,
    },
}

/// A fake whole disk.
#[derive(Debug, Clone)]
pub struct Disk {
    /// Kernel name, such as `sdb`.
    pub name: &'static str,
    /// `major:minor`.
    pub dev: (u32, u32),
    /// Size in 512-byte sectors.
    pub sectors: u64,
    /// Removable media flag.
    pub removable: bool,
    /// Read-only flag.
    pub read_only: bool,
    /// Disk sequence number.
    pub diskseq: Option<u64>,
    /// Attachment.
    pub bus: Bus,
    /// SCSI vendor (padded to 8 characters like sysfs does).
    pub vendor: &'static str,
    /// SCSI model (padded to 16 characters like sysfs does).
    pub model: &'static str,
}

impl Disk {
    /// A USB flash drive with typical values.
    pub fn usb_stick(name: &'static str, dev: (u32, u32), sectors: u64) -> Self {
        Self {
            name,
            dev,
            sectors,
            removable: true,
            read_only: false,
            diskseq: Some(u64::from(dev.1) + 100),
            bus: Bus::Usb {
                vid: 0x0781,
                pid: 0x5567,
                driver: "usb-storage",
                serial: "4C530001230922115084",
            },
            vendor: "SanDisk",
            model: "Cruzer Blade",
        }
    }
}

/// A fake partition.
#[derive(Debug, Clone)]
pub struct Partition {
    /// Kernel name, such as `sdb1`.
    pub name: &'static str,
    /// `major:minor`.
    pub dev: (u32, u32),
    /// Partition number.
    pub number: u32,
    /// Start in 512-byte sectors.
    pub start: u64,
    /// Size in 512-byte sectors.
    pub sectors: u64,
}

/// A fake system tree. Dropping it deletes the tree.
#[derive(Debug)]
pub struct FakeSystem {
    dir: TempDir,
    mountinfo: Vec<String>,
    swaps: Vec<String>,
    next_port: u32,
}

impl Default for FakeSystem {
    fn default() -> Self {
        Self::new()
    }
}

fn write(path: &Path, content: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, content).unwrap();
}

fn link(target: &str, at: &Path) {
    if let Some(parent) = at.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    symlink(target, at).unwrap();
}

/// `count` levels of `../`.
fn up(count: usize) -> String {
    "../".repeat(count)
}

impl FakeSystem {
    /// An empty system.
    pub fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        for path in [
            "sys/class/block",
            "sys/dev/block",
            "proc/self",
            "run/udev/data",
            "dev",
        ] {
            fs::create_dir_all(dir.path().join(path)).unwrap();
        }
        let system = Self {
            dir,
            mountinfo: Vec::new(),
            swaps: Vec::new(),
            next_port: 1,
        };
        system.flush();
        system
    }

    /// The root of the tree (pass to `SysRoot::new`).
    pub fn root(&self) -> &Path {
        self.dir.path()
    }

    fn abs(&self, path: &str) -> PathBuf {
        self.dir.path().join(path.trim_start_matches('/'))
    }

    /// Adds a whole disk; returns its sysfs directory (relative to the root).
    pub fn add_disk(&mut self, disk: &Disk) -> String {
        let port = self.next_port;
        self.next_port += 1;
        let (device_dir, block_dir) = match &disk.bus {
            Bus::Usb {
                vid,
                pid,
                driver,
                serial,
            } => {
                let usb = format!("sys/devices/pci0000:00/0000:00:14.0/usb2/2-{port}");
                write(
                    &self.abs(&format!("{usb}/idVendor")),
                    &format!("{vid:04x}\n"),
                );
                write(
                    &self.abs(&format!("{usb}/idProduct")),
                    &format!("{pid:04x}\n"),
                );
                write(&self.abs(&format!("{usb}/speed")), "480\n");
                write(&self.abs(&format!("{usb}/version")), " 2.10\n");
                write(
                    &self.abs(&format!("{usb}/manufacturer")),
                    &format!("{}\n", disk.vendor),
                );
                write(
                    &self.abs(&format!("{usb}/product")),
                    &format!("{}\n", disk.model),
                );
                write(&self.abs(&format!("{usb}/serial")), &format!("{serial}\n"));
                let interface = format!("{usb}/2-{port}:1.0");
                fs::create_dir_all(self.abs(&interface)).unwrap();
                link(
                    &format!("{}bus/usb/drivers/{driver}", up(6)),
                    &self.abs(&format!("{interface}/driver")),
                );
                let scsi = format!("{interface}/host{port}/target{port}:0:0/{port}:0:0:0");
                (scsi.clone(), format!("{scsi}/block/{}", disk.name))
            }
            Bus::Sata => {
                let scsi = format!(
                    "sys/devices/pci0000:00/0000:00:17.0/ata{port}/host{port}/target{port}:0:0/{port}:0:0:0"
                );
                (scsi.clone(), format!("{scsi}/block/{}", disk.name))
            }
            Bus::Nvme => {
                let controller =
                    format!("sys/devices/pci0000:00/0000:00:1d.0/0000:3d:00.0/nvme/nvme{port}");
                write(
                    &self.abs(&format!("{controller}/model")),
                    &format!("{}\n", disk.model),
                );
                write(
                    &self.abs(&format!("{controller}/serial")),
                    "S4EWNX0N123456\n",
                );
                (controller.clone(), format!("{controller}/{}", disk.name))
            }
            Bus::Virtio => {
                let virtio = format!("sys/devices/pci0000:00/0000:00:0{port}.0/virtio{port}");
                (virtio.clone(), format!("{virtio}/block/{}", disk.name))
            }
            Bus::Mmc { card_type } => {
                let card = format!(
                    "sys/devices/pci0000:00/0000:00:1e.0/mmc_host/mmc{port}/mmc{port}:aaaa"
                );
                write(
                    &self.abs(&format!("{card}/type")),
                    &format!("{card_type}\n"),
                );
                write(
                    &self.abs(&format!("{card}/name")),
                    &format!("{}\n", disk.model),
                );
                write(&self.abs(&format!("{card}/serial")), "0x1234abcd\n");
                (card.clone(), format!("{card}/block/{}", disk.name))
            }
            Bus::Loop { backing_file } => {
                let dir = format!("sys/devices/virtual/block/{}", disk.name);
                if let Some(file) = backing_file {
                    write(
                        &self.abs(&format!("{dir}/loop/backing_file")),
                        &format!("{file}\n"),
                    );
                }
                (String::new(), dir)
            }
        };
        if matches!(disk.bus, Bus::Usb { .. } | Bus::Sata) {
            write(
                &self.abs(&format!("{device_dir}/vendor")),
                &format!("{:<8}\n", disk.vendor),
            );
            write(
                &self.abs(&format!("{device_dir}/model")),
                &format!("{:<16}\n", disk.model),
            );
            write(&self.abs(&format!("{device_dir}/rev")), "1.00\n");
        }
        self.block_attributes(&block_dir, disk.dev, disk.sectors);
        write(
            &self.abs(&format!("{block_dir}/removable")),
            if disk.removable { "1\n" } else { "0\n" },
        );
        write(
            &self.abs(&format!("{block_dir}/ro")),
            if disk.read_only { "1\n" } else { "0\n" },
        );
        if let Some(seq) = disk.diskseq {
            write(
                &self.abs(&format!("{block_dir}/diskseq")),
                &format!("{seq}\n"),
            );
        }
        write(
            &self.abs(&format!("{block_dir}/queue/logical_block_size")),
            "512\n",
        );
        write(
            &self.abs(&format!("{block_dir}/queue/physical_block_size")),
            "512\n",
        );
        fs::create_dir_all(self.abs(&format!("{block_dir}/holders"))).unwrap();
        if !device_dir.is_empty() {
            let depth = Path::new(&block_dir)
                .strip_prefix(&device_dir)
                .unwrap()
                .components()
                .count();
            link(
                up(depth).trim_end_matches('/'),
                &self.abs(&format!("{block_dir}/device")),
            );
        }
        self.register(disk.name, disk.dev, &block_dir);
        block_dir
    }

    fn block_attributes(&self, dir: &str, dev: (u32, u32), sectors: u64) {
        write(
            &self.abs(&format!("{dir}/dev")),
            &format!("{}:{}\n", dev.0, dev.1),
        );
        write(&self.abs(&format!("{dir}/size")), &format!("{sectors}\n"));
    }

    /// Creates the `class/block`, `dev/block` and `/dev` entries of a device.
    fn register(&self, name: &str, dev: (u32, u32), dir: &str) {
        let target = |depth: usize| format!("{}{}", up(depth), dir.trim_start_matches("sys/"));
        link(&target(2), &self.abs(&format!("sys/class/block/{name}")));
        link(
            &target(2),
            &self.abs(&format!("sys/dev/block/{}:{}", dev.0, dev.1)),
        );
        write(&self.abs(&format!("dev/{name}")), "");
    }

    /// Adds a partition to a disk added with [`Self::add_disk`].
    pub fn add_partition(&mut self, disk_dir: &str, partition: &Partition) -> String {
        let dir = format!("{disk_dir}/{}", partition.name);
        self.block_attributes(&dir, partition.dev, partition.sectors);
        write(
            &self.abs(&format!("{dir}/partition")),
            &format!("{}\n", partition.number),
        );
        write(
            &self.abs(&format!("{dir}/start")),
            &format!("{}\n", partition.start),
        );
        fs::create_dir_all(self.abs(&format!("{dir}/holders"))).unwrap();
        self.register(partition.name, partition.dev, &dir);
        dir
    }

    /// Adds a virtual device (device mapper, MD, zram) stacked on `slaves`
    /// (kernel names of partitions or disks), and returns its directory.
    pub fn add_virtual(
        &mut self,
        name: &'static str,
        dev: (u32, u32),
        sectors: u64,
        slaves: &[&str],
    ) -> String {
        let dir = format!("sys/devices/virtual/block/{name}");
        self.block_attributes(&dir, dev, sectors);
        write(&self.abs(&format!("{dir}/removable")), "0\n");
        write(&self.abs(&format!("{dir}/ro")), "0\n");
        fs::create_dir_all(self.abs(&format!("{dir}/holders"))).unwrap();
        fs::create_dir_all(self.abs(&format!("{dir}/slaves"))).unwrap();
        for slave in slaves {
            let slave_dir =
                fs::canonicalize(self.abs(&format!("sys/class/block/{slave}"))).unwrap();
            symlink(&slave_dir, self.abs(&format!("{dir}/slaves/{slave}"))).unwrap();
            symlink(self.abs(&dir), slave_dir.join("holders").join(name)).unwrap();
        }
        self.register(name, dev, &dir);
        dir
    }

    /// Adds `/dev/<link>` pointing at a device (for `/dev/mapper/...` names).
    pub fn add_dev_link(&self, link_path: &str, device: &str) {
        let at = self.abs(&format!("dev/{}", link_path.trim_start_matches("/dev/")));
        let depth = Path::new(link_path.trim_start_matches("/dev/"))
            .components()
            .count()
            - 1;
        link(&format!("{}{device}", up(depth)), &at);
    }

    /// Records a udev database entry for a device.
    pub fn udev(&self, dev: (u32, u32), properties: &[(&str, &str)], links: &[&str]) {
        let mut text = String::new();
        for link in links {
            text.push_str(&format!("S:{link}\n"));
        }
        for (key, value) in properties {
            text.push_str(&format!("E:{key}={value}\n"));
        }
        write(
            &self.abs(&format!("run/udev/data/b{}:{}", dev.0, dev.1)),
            &text,
        );
    }

    /// Adds a mount; `dev` is `(0, n)` for anonymous devices.
    pub fn mount(
        &mut self,
        dev: (u32, u32),
        mount_point: &str,
        fs_type: &str,
        source: &str,
        options: &str,
    ) {
        let id = self.mountinfo.len() + 20;
        let escaped = mount_point.replace(' ', "\\040");
        self.mountinfo.push(format!(
            "{id} 1 {}:{} / {escaped} rw,relatime - {fs_type} {source} {options}",
            dev.0, dev.1
        ));
        self.flush();
    }

    /// Adds an active swap area.
    pub fn swap(&mut self, path: &str, kind: &str) {
        self.swaps
            .push(format!("{path}\t\t\t\t{kind}\t\t1048572\t\t0\t\t-2"));
        self.flush();
    }

    /// Declares a btrfs file system with the given member devices.
    pub fn btrfs(&self, uuid: &str, members: &[&str]) {
        for member in members {
            write(
                &self.abs(&format!("sys/fs/btrfs/{uuid}/devices/{member}")),
                "",
            );
        }
    }

    fn flush(&self) {
        let mut mountinfo = self.mountinfo.join("\n");
        mountinfo.push('\n');
        write(&self.abs("proc/self/mountinfo"), &mountinfo);
        let mut swaps = String::from("Filename\t\t\t\tType\t\tSize\t\tUsed\t\tPriority\n");
        for line in &self.swaps {
            swaps.push_str(line);
            swaps.push('\n');
        }
        write(&self.abs("proc/swaps"), &swaps);
    }
}
