// SPDX-License-Identifier: GPL-3.0-or-later
//! Read-only discovery of storage devices.
//!
//! The Linux counterpart of Rufus `src/dev.c:GetDevices()`: whole disks are
//! read from sysfs, procfs and the udev database (no privileges needed),
//! the listing rules decide which ones are offered, and the list is sorted
//! by size. Unlike upstream, no device is ever selected automatically.

pub mod facts;
pub mod hdd_heuristic;
mod hdd_tables;
pub mod identity;
pub mod mounts;
pub mod policy;
pub mod sysfs;
pub mod system;
pub mod udev_db;

use crate::units::{SizeUnits, size_to_human_readable};
use crate::{Error, Result};
use facts::DeviceFacts;
use identity::DeviceIdentity;
use policy::{ListingDecision, ListingOptions};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::path::PathBuf;
use sysfs::SysRoot;
use system::SystemInfo;

/// A device number, `major:minor`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct DevNum {
    /// Major number.
    pub major: u32,
    /// Minor number.
    pub minor: u32,
}

impl DevNum {
    /// Parses `major:minor`.
    pub fn parse(text: &str) -> Option<Self> {
        let (major, minor) = text.trim().split_once(':')?;
        Some(Self {
            major: major.parse().ok()?,
            minor: minor.parse().ok()?,
        })
    }

    /// Decodes a Linux `dev_t` (as in `st_dev`) the way glibc's
    /// `gnu_dev_major()`/`gnu_dev_minor()` do.
    pub fn from_dev_t(dev: u64) -> Self {
        let major = ((dev >> 8) & 0xfff) | ((dev >> 32) & !0xfff);
        let minor = (dev & 0xff) | ((dev >> 12) & !0xff);
        Self {
            major: u32::try_from(major).unwrap_or(u32::MAX),
            minor: u32::try_from(minor).unwrap_or(u32::MAX),
        }
    }
}

impl fmt::Display for DevNum {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.major, self.minor)
    }
}

/// Options of a device scan.
#[derive(Debug, Clone, Default)]
pub struct ScanOptions {
    /// Listing rules.
    pub listing: ListingOptions,
    /// Files of the running program (its executable and, for an AppImage,
    /// the AppImage file): their disks are never offered.
    pub program_paths: Vec<PathBuf>,
}

impl ScanOptions {
    /// Options for the current process: its executable and `$APPIMAGE`.
    pub fn for_this_process(listing: ListingOptions) -> Self {
        let mut program_paths = Vec::new();
        if let Ok(exe) = std::env::current_exe() {
            program_paths.push(exe);
        }
        if let Some(appimage) = std::env::var_os("APPIMAGE") {
            program_paths.push(PathBuf::from(appimage));
        }
        Self {
            listing,
            program_paths,
        }
    }
}

/// One whole disk and the decision about it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DeviceEntry {
    /// The name shown in the device list, as Rufus formats it:
    /// "<label> (<device>) [<size>]".
    pub display_name: String,
    /// A stable identifier: the `/dev/disk/by-id` name when available.
    pub stable_id: String,
    /// The identity compared before destructive stages.
    pub identity: DeviceIdentity,
    /// Whether and why the device is offered.
    pub decision: ListingDecision,
    /// Mount points of file systems on this disk.
    pub mounts: Vec<String>,
    /// Everything probed.
    pub facts: DeviceFacts,
}

/// The result of a scan.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DeviceScan {
    /// Listed devices first, by increasing size (as Rufus sorts them), then
    /// excluded devices by name.
    pub devices: Vec<DeviceEntry>,
    /// What the running system depends on.
    pub system: SystemInfo,
}

impl DeviceScan {
    /// The devices offered as targets.
    pub fn listed(&self) -> impl Iterator<Item = &DeviceEntry> {
        self.devices.iter().filter(|device| device.decision.listed)
    }

    /// Finds the device the user means: a kernel name (`sdb`), a device node
    /// (`/dev/sdb`), a `/dev/disk/by-id/...` path or name, or a stable id.
    /// Excluded devices can be found too; callers decide what to do with them.
    pub fn find(&self, query: &str) -> Result<&DeviceEntry> {
        let query = query.trim();
        let by_id = query.strip_prefix("/dev/disk/by-id/").unwrap_or(query);
        let matches: Vec<&DeviceEntry> = self
            .devices
            .iter()
            .filter(|d| {
                d.facts.name == query
                    || d.facts.devnode == query
                    || d.stable_id == by_id
                    || d.facts.by_id.as_deref() == Some(by_id)
            })
            .collect();
        match matches.as_slice() {
            [device] => Ok(device),
            [] => Err(Error::DeviceNotFound(format!(
                "no whole-disk device matches `{query}`"
            ))),
            _ => Err(Error::AmbiguousDevice(format!(
                "`{query}` matches {} devices",
                matches.len()
            ))),
        }
    }
}

/// Scans the block devices of the system under `root`.
pub fn scan(root: &SysRoot, options: &ScanOptions) -> Result<DeviceScan> {
    let class = root.path("/sys/class/block");
    let names = std::fs::read_dir(&class)
        .map_err(|e| Error::io(format!("cannot list {}", class.display()), e))?
        .filter_map(|entry| {
            entry
                .ok()
                .map(|e| e.file_name().to_string_lossy().into_owned())
        })
        .collect::<Vec<_>>();
    let mount_list =
        mounts::parse_mountinfo(&root.read("/proc/self/mountinfo").unwrap_or_default());
    let swaps = mounts::parse_swaps(&root.read("/proc/swaps").unwrap_or_default());
    let system = SystemInfo::analyze(root, &mount_list, &swaps, &options.program_paths);

    let mut devices: Vec<DeviceEntry> = names
        .iter()
        .filter_map(|name| DeviceFacts::probe(root, name))
        .map(|facts| {
            let decision = policy::decide(&facts, &system, &options.listing);
            DeviceEntry {
                display_name: display_name(&facts),
                stable_id: stable_id(&facts),
                identity: DeviceIdentity::of(&facts),
                decision,
                mounts: system
                    .disk_mounts
                    .get(&facts.name)
                    .cloned()
                    .unwrap_or_default(),
                facts,
            }
        })
        .collect();
    devices.sort_by(|a, b| {
        b.decision
            .listed
            .cmp(&a.decision.listed)
            .then_with(|| match a.decision.listed {
                true => a.facts.size_bytes.cmp(&b.facts.size_bytes),
                false => std::cmp::Ordering::Equal,
            })
            .then_with(|| a.facts.name.cmp(&b.facts.name))
    });
    Ok(DeviceScan { devices, system })
}

/// `GetDevices()` display names: the label of the only volume, "Multiple
/// Partitions" when there are several, or `NO_LABEL`; then the device in
/// parentheses (Rufus shows drive letters or "Disk N") and the size.
pub fn display_name(facts: &DeviceFacts) -> String {
    let volumes: Vec<_> = facts
        .partitions
        .iter()
        .filter(|p| p.fs_type.is_some())
        .collect();
    let label = match volumes.as_slice() {
        [] => facts.fs_label.clone(),
        [only] => only.fs_label.clone(),
        _ => Some("Multiple Partitions".to_string()),
    }
    .unwrap_or_else(|| "NO_LABEL".to_string());
    format!(
        "{label} ({}) [{}]",
        facts.devnode,
        size_to_human_readable(facts.size_bytes, SizeUnits::Fake)
    )
}

/// A stable identifier: the udev `by-id` name, or one built from the bus,
/// IDs and serial number.
pub fn stable_id(facts: &DeviceFacts) -> String {
    if let Some(by_id) = &facts.by_id {
        return by_id.clone();
    }
    let serial = facts.serial.as_deref().unwrap_or("noserial");
    match facts.usb() {
        Some(usb) => format!("usb-{:04x}:{:04x}-{serial}", usb.vid, usb.pid),
        None => format!("{}-{serial}", facts.name),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dev_numbers() {
        assert_eq!(
            DevNum::parse("8:16"),
            Some(DevNum {
                major: 8,
                minor: 16
            })
        );
        assert_eq!(
            DevNum::parse(" 259:3\n"),
            Some(DevNum {
                major: 259,
                minor: 3
            })
        );
        assert_eq!(DevNum::parse("8"), None);
        assert_eq!(DevNum::parse("a:b"), None);
        // makedev(8, 17) and makedev(259, 65537).
        assert_eq!(
            DevNum::from_dev_t(0x811),
            DevNum {
                major: 8,
                minor: 17
            }
        );
        let dev = (259u64 << 8) | (0x10000u64 << 12) | 1;
        assert_eq!(
            DevNum::from_dev_t(dev),
            DevNum {
                major: 259,
                minor: 0x10001
            }
        );
        assert_eq!(DevNum { major: 8, minor: 0 }.to_string(), "8:0");
    }
}
