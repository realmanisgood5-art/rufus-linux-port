// SPDX-License-Identifier: GPL-3.0-or-later
//! Dry-run planning of a DD (raw image) write.
//!
//! Translated from the checks Rufus makes before writing an image in DD
//! mode, `src/rufus.c:BootCheckThread()` and `src/rufus.c:ImageScanThread()`,
//! and the stage sequence of `src/format.c:FormatThread()` and
//! `src/format.c:WriteDrive()` (Copyright © 2011-2026 Pete Batard,
//! GPL-3.0-or-later) at the v4.15 baseline. Stricter where data loss is at
//! stake: no size slack, no truncation, unresolved system mounts block.
//!
//! A plan describes what would happen; this build never executes it.

use crate::device::DeviceEntry;
use crate::device::identity::DeviceIdentity;
use crate::device::policy::ReasonCode;
use crate::device::system::{Resolution, SystemInfo};
use crate::image::compression::MagicCheck;
use crate::image::{Bootability, Classification, ImageReport};
use crate::units::{MB, SizeUnits, size_to_human_readable};
use crate::upstream::UpstreamRef;
use serde::Serialize;
use sha2::{Digest as _, Sha256};

/// `DD_BUFFER_SIZE`: the write buffer of a DD write.
pub const DD_BUFFER_SIZE: u64 = 32 * MB;
/// `WRITE_RETRIES`: attempts per block before giving up.
pub const WRITE_RETRIES: u32 = 4;

const BOOT_CHECK: UpstreamRef = UpstreamRef("src/rufus.c:BootCheckThread()");
const IMAGE_SCAN: UpstreamRef = UpstreamRef("src/rufus.c:ImageScanThread()");
const GET_DEVICES: UpstreamRef = UpstreamRef("src/dev.c:GetDevices()");

/// Options of a DD plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct PlanOptions {
    /// Read the device back and compare it with the image after writing
    /// (a Ferrufus addition; on by default).
    pub verify: bool,
}

impl Default for PlanOptions {
    fn default() -> Self {
        Self { verify: true }
    }
}

/// The outcome of one check.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum CheckStatus {
    /// Nothing to report.
    Pass,
    /// Worth knowing; does not block.
    Warn,
    /// Blocks the plan.
    Fail,
}

/// A check made before writing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Check {
    /// Stable identifier.
    pub id: &'static str,
    /// Outcome.
    pub status: CheckStatus,
    /// Explanation.
    pub message: String,
    /// The upstream rule.
    pub upstream: UpstreamRef,
}

/// One stage of the write.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Step {
    /// Stable identifier.
    pub id: &'static str,
    /// What the stage does.
    pub description: String,
    /// The upstream stage.
    pub upstream: UpstreamRef,
    /// Whether this build can perform it (never, in this milestone).
    pub implemented: bool,
}

/// A partition that would be destroyed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LostPartition {
    /// Device node.
    pub devnode: String,
    /// Size in bytes.
    pub size_bytes: Option<u64>,
    /// File system type.
    pub fs_type: Option<String>,
    /// File system label.
    pub label: Option<String>,
}

/// Everything on the device that the write destroys.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DataLoss {
    /// Device node.
    pub devnode: String,
    /// The name shown in the device list.
    pub display_name: String,
    /// Device size in bytes.
    pub size_bytes: u64,
    /// Partition table type.
    pub partition_table: Option<String>,
    /// File system on the whole disk, without a partition table.
    pub whole_disk_fs: Option<String>,
    /// Partitions.
    pub partitions: Vec<LostPartition>,
    /// Mount points that would be unmounted.
    pub mounts: Vec<String>,
    /// Summary sentence.
    pub summary: String,
}

/// A DD write plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Plan {
    /// Always `dd`.
    pub mode: &'static str,
    /// The target, as identified when planning.
    pub device: DeviceIdentity,
    /// The image.
    pub image: String,
    /// Image file size.
    pub image_size: u64,
    /// Bytes written from the image, when known.
    pub payload_size: Option<u64>,
    /// Zero bytes appended to complete the last sector.
    pub padding_bytes: u64,
    /// Options.
    pub options: PlanOptions,
    /// Checks, in order.
    pub checks: Vec<Check>,
    /// Stages, in order.
    pub steps: Vec<Step>,
    /// What would be destroyed.
    pub data_loss: DataLoss,
    /// Whether a check failed.
    pub blocked: bool,
    /// Whether this build could execute the plan: always `false`.
    pub executable: bool,
    /// Why the plan cannot be executed.
    pub not_executable_reason: &'static str,
    /// SHA-256 over the device identity, image, options, checks and steps;
    /// an executor must refuse a plan whose fingerprint does not match a
    /// fresh plan.
    pub fingerprint: String,
}

impl Plan {
    /// The checks that block the plan.
    pub fn failures(&self) -> impl Iterator<Item = &Check> {
        self.checks.iter().filter(|c| c.status == CheckStatus::Fail)
    }
}

/// Plans writing `image` to `device` in DD mode. `image_location` holds the
/// disks the image file is stored on (see `SystemInfo::disks_of_path`).
pub fn plan_dd(
    device: &DeviceEntry,
    system: &SystemInfo,
    image: &ImageReport,
    image_location: &Resolution,
    options: PlanOptions,
) -> Plan {
    let facts = &device.facts;
    let mut checks = Vec::new();
    let mut check =
        |id: &'static str, status: CheckStatus, message: String, upstream: UpstreamRef| {
            checks.push(Check {
                id,
                status,
                message,
                upstream,
            });
        };

    // The device.
    match system.system_disks.get(&facts.name) {
        Some(why) => check(
            "not-system-disk",
            CheckStatus::Fail,
            format!(
                "{} is used by the running system: {}",
                facts.devnode,
                why.join("; ")
            ),
            GET_DEVICES,
        ),
        None => check(
            "not-system-disk",
            CheckStatus::Pass,
            "not used by the running system".into(),
            GET_DEVICES,
        ),
    }
    if system.unresolved.is_empty() {
        check(
            "system-resolved",
            CheckStatus::Pass,
            "every system mount was traced to its disk".into(),
            UpstreamRef::FERRUFUS,
        );
    } else {
        check(
            "system-resolved",
            CheckStatus::Fail,
            format!(
                "cannot tell which disks back the system: {}",
                system.unresolved.join("; ")
            ),
            UpstreamRef::FERRUFUS,
        );
    }
    let other_reasons: Vec<&str> = device
        .decision
        .reasons
        .iter()
        .filter(|r| r.code != ReasonCode::SystemDisk)
        .map(|r| r.message.as_str())
        .collect();
    if other_reasons.is_empty() {
        check(
            "target-listed",
            CheckStatus::Pass,
            "offered as a target by the listing rules".into(),
            GET_DEVICES,
        );
    } else {
        check(
            "target-listed",
            CheckStatus::Fail,
            format!("not offered as a target: {}", other_reasons.join("; ")),
            GET_DEVICES,
        );
    }
    if facts.read_only {
        check(
            "writable",
            CheckStatus::Fail,
            format!("{} is read-only", facts.devnode),
            UpstreamRef::FERRUFUS,
        );
    } else {
        check(
            "writable",
            CheckStatus::Pass,
            "not read-only".into(),
            UpstreamRef::FERRUFUS,
        );
    }
    let holders = facts.all_holders();
    if holders.is_empty() {
        check(
            "not-in-use",
            CheckStatus::Pass,
            "no device mapper or RAID device uses it".into(),
            UpstreamRef::FERRUFUS,
        );
    } else {
        check(
            "not-in-use",
            CheckStatus::Fail,
            format!(
                "in use by {} (close or deactivate them first)",
                holders.join(", ")
            ),
            UpstreamRef::FERRUFUS,
        );
    }
    if device.mounts.is_empty() {
        check(
            "mounts",
            CheckStatus::Pass,
            "nothing is mounted from it".into(),
            UpstreamRef("src/drive.c:UnmountVolume()"),
        );
    } else {
        check(
            "mounts",
            CheckStatus::Warn,
            format!(
                "mounted at {}; these would be unmounted",
                device.mounts.join(", ")
            ),
            UpstreamRef("src/drive.c:UnmountVolume()"),
        );
    }

    // The image.
    if image_location.disks.contains(&facts.name) {
        check(
            "image-not-on-target",
            CheckStatus::Fail,
            "the image is located on the target drive, which would be completely erased".into(),
            UpstreamRef("src/drive.c:IsSourceImageLocatedOnTargetDrive()"),
        );
    } else if !image_location.unresolved.is_empty() {
        check(
            "image-not-on-target",
            CheckStatus::Fail,
            format!(
                "cannot tell which disk holds the image: {}",
                image_location.unresolved.join("; ")
            ),
            UpstreamRef("src/drive.c:IsSourceImageLocatedOnTargetDrive()"),
        );
    } else {
        check(
            "image-not-on-target",
            CheckStatus::Pass,
            "the image is not on the target".into(),
            UpstreamRef("src/drive.c:IsSourceImageLocatedOnTargetDrive()"),
        );
    }
    if image.magic == Some(MagicCheck::Mismatch) {
        check(
            "image-format",
            CheckStatus::Fail,
            "the file contents do not match its extension".into(),
            UpstreamRef("src/vhd.c:IsCompressedBootableImage()"),
        );
    }
    let (status, message, upstream) = match image.classification {
        Classification::DiskImage => (CheckStatus::Pass, "a disk image, written as-is".to_string(), IMAGE_SCAN),
        Classification::IsoHybrid => (
            CheckStatus::Warn,
            "an ISOHybrid image: written in DD mode (Rufus recommends ISO mode, which is not available yet)".into(),
            BOOT_CHECK,
        ),
        Classification::Iso => (
            CheckStatus::Fail,
            "an ISO without a boot marker: needs ISO mode, which is not available yet".into(),
            BOOT_CHECK,
        ),
        Classification::WindowsImage => (
            CheckStatus::Fail,
            "a Windows image: needs ISO mode, which is not available yet".into(),
            IMAGE_SCAN,
        ),
        Classification::NotYetSupported => (
            CheckStatus::Fail,
            "this image type cannot be written by this build yet".into(),
            IMAGE_SCAN,
        ),
        Classification::Unsupported => (CheckStatus::Fail, "Unsupported image".into(), IMAGE_SCAN),
    };
    check("image-supported", status, message, upstream);
    if image.is_bootable_img == Bootability::Forced {
        check(
            "boot-marker",
            CheckStatus::Warn,
            "no boot marker: written only because the boot marker check is ignored".into(),
            UpstreamRef("src/vhd.c:IsBootableImage()"),
        );
    }
    if image.vhd.as_ref().is_some_and(|v| !v.checksum_valid) {
        check(
            "vhd-footer",
            CheckStatus::Warn,
            "the VHD footer checksum is wrong".into(),
            UpstreamRef("src/vhd.c:IsBootableImage()"),
        );
    }

    // Size: strict, unlike the 4 KB margin upstream allows (the payload is
    // never truncated).
    let payload = image.dd_payload_size();
    let sector = u64::from(facts.logical_block_size.max(1));
    let padding = payload.map_or(0, |size| match size % sector {
        0 => 0,
        rest => sector.saturating_sub(rest),
    });
    match payload.map(|size| size.checked_add(padding)) {
        None => check(
            "fits",
            CheckStatus::Fail,
            "the size of the data to write is unknown".into(),
            BOOT_CHECK,
        ),
        Some(Some(0)) => check(
            "fits",
            CheckStatus::Fail,
            "the image is empty".into(),
            BOOT_CHECK,
        ),
        Some(Some(total)) if total <= facts.size_bytes => check(
            "fits",
            CheckStatus::Pass,
            format!("{} of {}", human(total), human(facts.size_bytes)),
            BOOT_CHECK,
        ),
        Some(_) => check(
            "fits",
            CheckStatus::Fail,
            format!(
                "The image is too big for the selected target ({} for {})",
                human(payload.unwrap_or(0)),
                human(facts.size_bytes)
            ),
            BOOT_CHECK,
        ),
    }
    if padding > 0 {
        check(
            "sector-padding",
            CheckStatus::Warn,
            format!("the last sector is completed with {padding} zero bytes"),
            UpstreamRef("src/format.c:WriteDrive()"),
        );
    }

    let blocked = checks.iter().any(|c| c.status == CheckStatus::Fail);
    let steps = steps(device, options);
    let data_loss = data_loss(device);
    let mut plan = Plan {
        mode: "dd",
        device: device.identity.clone(),
        image: image.path.clone(),
        image_size: image.image_size,
        payload_size: payload,
        padding_bytes: padding,
        options,
        checks,
        steps,
        data_loss,
        blocked,
        executable: false,
        not_executable_reason: "this build of Ferrufus is a read-only preview and cannot write to devices",
        fingerprint: String::new(),
    };
    plan.fingerprint = fingerprint(&plan);
    plan
}

fn human(size: u64) -> String {
    size_to_human_readable(size, SizeUnits::Fake)
}

fn steps(device: &DeviceEntry, options: PlanOptions) -> Vec<Step> {
    let node = &device.facts.devnode;
    let step = |id, description: String, upstream| Step {
        id,
        description,
        upstream,
        implemented: false,
    };
    let mut steps = vec![
        step(
            "revalidate",
            format!("check that {node} is still the device that was planned for"),
            UpstreamRef::FERRUFUS,
        ),
        step(
            "open-exclusive",
            format!("open {node} exclusively, failing if anything else has it open"),
            UpstreamRef("src/drive.c:GetPhysicalHandle()"),
        ),
        step(
            "unmount",
            "unmount every file system on the device".into(),
            UpstreamRef("src/drive.c:UnmountVolume()"),
        ),
        step(
            "clear-partition-tables",
            "erase the partition tables at the start and end of the device".into(),
            UpstreamRef("src/format.c:ClearMBRGPT()"),
        ),
        step(
            "write",
            format!(
                "write the image in {} blocks, retrying a failed block up to {WRITE_RETRIES} times",
                human(DD_BUFFER_SIZE)
            ),
            UpstreamRef("src/format.c:WriteDrive()"),
        ),
        step(
            "flush",
            "flush the data to the device and wait for it".into(),
            UpstreamRef::FERRUFUS,
        ),
    ];
    if options.verify {
        steps.push(step(
            "verify",
            "read the device back and compare it with the image".into(),
            UpstreamRef::FERRUFUS,
        ));
    }
    steps.push(step(
        "rescan",
        "have the kernel reread the partition table".into(),
        UpstreamRef("src/drive.c:RefreshDriveLayout()"),
    ));
    steps
}

fn data_loss(device: &DeviceEntry) -> DataLoss {
    let facts = &device.facts;
    let partitions: Vec<LostPartition> = facts
        .partitions
        .iter()
        .map(|p| LostPartition {
            devnode: p.devnode.clone(),
            size_bytes: p.size_bytes,
            fs_type: p.fs_type.clone(),
            label: p.fs_label.clone(),
        })
        .collect();
    let summary = format!(
        "ALL DATA ON {} ({}) WILL BE DESTROYED: {} partition(s){}",
        facts.devnode,
        device.display_name,
        partitions.len(),
        if device.mounts.is_empty() {
            String::new()
        } else {
            format!(", mounted at {}", device.mounts.join(", "))
        }
    );
    DataLoss {
        devnode: facts.devnode.clone(),
        display_name: device.display_name.clone(),
        size_bytes: facts.size_bytes,
        partition_table: facts.partition_table.clone(),
        whole_disk_fs: facts.fs_type.clone(),
        partitions,
        mounts: device.mounts.clone(),
        summary,
    }
}

/// SHA-256 of the plan's JSON without its fingerprint.
fn fingerprint(plan: &Plan) -> String {
    let json = serde_json::to_vec(plan).unwrap_or_default();
    Sha256::digest(&json)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}
