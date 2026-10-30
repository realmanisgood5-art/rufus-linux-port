// SPDX-License-Identifier: GPL-3.0-or-later
//! Device identity and revalidation.
//!
//! Rufus identifies the target by its drive index and checks nothing else
//! before formatting. Ferrufus records an identity when a device is
//! selected and compares it again before every destructive stage, so that a
//! replugged device, a reused `/dev/sdX` name or a swapped card is detected.

use super::DevNum;
use super::facts::DeviceFacts;
use super::sysfs::SysRoot;
use serde::{Deserialize, Serialize};

/// The properties compared on revalidation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceIdentity {
    /// Kernel name.
    pub name: String,
    /// `major:minor`.
    pub dev: DevNum,
    /// Disk sequence number: a new value means a new device or medium.
    pub diskseq: Option<u64>,
    /// Size in bytes.
    pub size_bytes: u64,
    /// Logical sector size.
    pub logical_block_size: u32,
    /// Serial number.
    pub serial: Option<String>,
    /// Model.
    pub model: Option<String>,
    /// USB vendor and product IDs.
    pub usb_id: Option<(u16, u16)>,
    /// Canonical sysfs path.
    pub sysfs_path: String,
}

impl DeviceIdentity {
    /// The identity of probed facts.
    pub fn of(facts: &DeviceFacts) -> Self {
        Self {
            name: facts.name.clone(),
            dev: facts.dev,
            diskseq: facts.diskseq,
            size_bytes: facts.size_bytes,
            logical_block_size: facts.logical_block_size,
            serial: facts.serial.clone(),
            model: facts.model.clone(),
            usb_id: facts.usb().map(|usb| (usb.vid, usb.pid)),
            sysfs_path: facts.sysfs_path.clone(),
        }
    }

    /// Differences between this (expected) identity and `current`.
    pub fn differences(&self, current: &Self) -> Vec<IdentityChange> {
        let mut changes = Vec::new();
        let mut compare = |field: &'static str, expected: String, found: String| {
            if expected != found {
                changes.push(IdentityChange {
                    field,
                    expected,
                    found,
                });
            }
        };
        let show = |value: &Option<String>| value.clone().unwrap_or_else(|| "(none)".to_string());
        compare("name", self.name.clone(), current.name.clone());
        compare("dev", self.dev.to_string(), current.dev.to_string());
        compare(
            "diskseq",
            format!("{:?}", self.diskseq),
            format!("{:?}", current.diskseq),
        );
        compare(
            "size",
            self.size_bytes.to_string(),
            current.size_bytes.to_string(),
        );
        compare(
            "logical block size",
            self.logical_block_size.to_string(),
            current.logical_block_size.to_string(),
        );
        compare("serial", show(&self.serial), show(&current.serial));
        compare("model", show(&self.model), show(&current.model));
        compare(
            "USB id",
            format!("{:?}", self.usb_id),
            format!("{:?}", current.usb_id),
        );
        compare(
            "sysfs path",
            self.sysfs_path.clone(),
            current.sysfs_path.clone(),
        );
        changes
    }
}

/// One property that changed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct IdentityChange {
    /// The property.
    pub field: &'static str,
    /// The recorded value.
    pub expected: String,
    /// The current value.
    pub found: String,
}

/// The outcome of [`revalidate`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "status", rename_all = "kebab-case")]
pub enum Revalidation {
    /// The device is still the same.
    Same,
    /// The device is gone.
    Gone,
    /// The device changed.
    Changed {
        /// What changed.
        changes: Vec<IdentityChange>,
    },
}

/// Probes the device again and compares it with `expected`.
pub fn revalidate(root: &SysRoot, expected: &DeviceIdentity) -> Revalidation {
    let Some(facts) = DeviceFacts::probe(root, &expected.name) else {
        return Revalidation::Gone;
    };
    let changes = expected.differences(&DeviceIdentity::of(&facts));
    if changes.is_empty() {
        Revalidation::Same
    } else {
        Revalidation::Changed { changes }
    }
}
