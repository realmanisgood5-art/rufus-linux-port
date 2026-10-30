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

use crate::device::identity::DeviceIdentity;
use crate::device::policy::ReasonCode;
use crate::device::sysfs::SysRoot;
use crate::device::system::{Resolution, SystemInfo};
use crate::device::{DeviceEntry, ScanOptions, mounts, scan};
use crate::image::compression::MagicCheck;
use crate::image::iso_scan::IsoScan;
use crate::image::{Bootability, Classification, ImageReport, InspectOptions, inspect};
use crate::layout::{ExtraPartitions, LayoutInput, PlannedPartition};
use crate::options::{
    BootType, Choices, DeviceInput, FileSystem, FormatOptions, ImageOption, PartitionScheme,
    TargetSystem, TargetType, format_options,
};
use crate::units::{MB, SizeUnits, size_to_human_readable};
use crate::upstream::UpstreamRef;
use crate::{Error, Result};
use serde::Serialize;
use sha2::{Digest as _, Sha256};
use std::path::Path;

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
    /// `dd`, `iso` or `format`.
    pub mode: &'static str,
    /// The target, as identified when planning.
    pub device: DeviceIdentity,
    /// The image (none for a plain format).
    pub image: Option<String>,
    /// Image file size.
    pub image_size: Option<u64>,
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
    /// The format options (ISO mode and plain format).
    pub format: Option<FormatOptions>,
    /// The partitions that would be created (ISO mode and plain format).
    pub layout: Vec<PlannedPartition>,
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
    let mut checks = device_checks(device, system, true);
    let mut check =
        |id: &'static str, status: CheckStatus, message: String, upstream: UpstreamRef| {
            checks.push(Check {
                id,
                status,
                message,
                upstream,
            });
        };

    // The image.
    let location = image_location_check(&facts.name, image_location);
    check(
        location.id,
        location.status,
        location.message,
        location.upstream,
    );
    if image.magic == Some(MagicCheck::Mismatch) {
        check(
            "image-format",
            CheckStatus::Fail,
            "the file contents do not match its extension".into(),
            UpstreamRef("src/vhd.c:IsCompressedBootableImage()"),
        );
    }
    let (status, message, upstream) = match image.classification {
        Classification::DiskImage => (
            CheckStatus::Pass,
            "a disk image, written as-is".to_string(),
            IMAGE_SCAN,
        ),
        Classification::IsoHybrid => (
            CheckStatus::Warn,
            "an ISOHybrid image written in DD mode (Rufus recommends ISO mode)".into(),
            BOOT_CHECK,
        ),
        Classification::Iso => (
            CheckStatus::Fail,
            "an ISO without a boot marker can only be written in ISO mode".into(),
            BOOT_CHECK,
        ),
        Classification::WindowsImage => (
            CheckStatus::Fail,
            "a Windows image cannot be written in DD mode".into(),
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
        image: Some(image.path.clone()),
        image_size: Some(image.image_size),
        payload_size: payload,
        padding_bytes: padding,
        options,
        checks,
        steps,
        data_loss,
        format: None,
        layout: Vec::new(),
        blocked,
        executable: false,
        not_executable_reason: "this build of Ferrufus is a read-only preview and cannot write to devices",
        fingerprint: String::new(),
    };
    plan.fingerprint = fingerprint(&plan);
    plan
}

/// Checks about the target device, shared by every write mode.
fn device_checks(device: &DeviceEntry, system: &SystemInfo, bootable: bool) -> Vec<Check> {
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

    // Asked before every write (`MainCallback()`, `UM_FORMAT_START`).
    if facts.partitions.len() > 1 {
        check(
            "multiple-partitions",
            CheckStatus::Warn,
            format!(
                "{} contains {} partitions, all of which would be erased",
                facts.devnode,
                facts.partitions.len()
            ),
            UpstreamRef("src/rufus.c:MainCallback()"),
        );
    }
    if bootable && facts.logical_block_size != 512 {
        check(
            "sector-size",
            CheckStatus::Warn,
            format!(
                "the device uses {}-byte sectors; most systems cannot boot from it",
                facts.logical_block_size
            ),
            UpstreamRef("src/rufus.c:MainCallback()"),
        );
    }
    checks
}

/// `IsSourceImageLocatedOnTargetDrive()`, stricter: an image whose disk is
/// unknown also blocks.
fn image_location_check(device_name: &str, image_location: &Resolution) -> Check {
    const UPSTREAM: UpstreamRef = UpstreamRef("src/drive.c:IsSourceImageLocatedOnTargetDrive()");
    let (status, message) = if image_location.disks.iter().any(|d| d == device_name) {
        (
            CheckStatus::Fail,
            "the image is located on the target drive, which would be completely erased"
                .to_string(),
        )
    } else if !image_location.unresolved.is_empty() {
        (
            CheckStatus::Fail,
            format!(
                "cannot tell which disk holds the image: {}",
                image_location.unresolved.join("; ")
            ),
        )
    } else {
        (
            CheckStatus::Pass,
            "the image is not on the target".to_string(),
        )
    };
    Check {
        id: "image-not-on-target",
        status,
        message,
        upstream: UPSTREAM,
    }
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
                "write the image in {} MiB blocks, retrying a failed block up to {WRITE_RETRIES} times",
                DD_BUFFER_SIZE / MB
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

/// Version of GRUB whose `core.img` Rufus 4.15 embeds (`GRUB2_PACKAGE_VERSION`).
pub const EMBEDDED_GRUB_VERSION: &str = "2.14";
/// Extra version string of the embedded Syslinux 6 (`embedded_sl_version_ext[1]`).
pub const EMBEDDED_SL6_EXT: &str = "/pre1";
/// `MAX_ISO_TO_ESP_SIZE`.
const MAX_ISO_TO_ESP_SIZE: u64 = crate::units::GB;

/// What an ISO-mode or format plan needs besides the device.
#[derive(Debug, Clone, Default)]
pub struct FormatRequest<'a> {
    /// The image, for ISO mode; `None` formats the drive.
    pub image: Option<&'a ImageReport>,
    /// The disks holding the image file.
    pub image_location: Option<&'a Resolution>,
    /// The user's choices.
    pub choices: Choices,
    /// Size of a Linux persistence partition (0 for none).
    pub persistence: u64,
    /// "Add fixes for old BIOSes".
    pub old_bios_fixes: bool,
}

/// Plans an ISO-mode write (files extracted to a new file system) or, without
/// an image, a plain format. Translated from the ISO checks of
/// `BootCheckThread()`, the extra partitions of `FormatThread()` and the
/// stages `FormatThread()` runs.
pub fn plan_format_mode(
    device: &DeviceEntry,
    system: &SystemInfo,
    request: &FormatRequest<'_>,
    options: PlanOptions,
) -> Plan {
    let facts = &device.facts;
    let image = request.image;
    let scan = image.and_then(|i| i.iso_scan.as_ref());
    let device_input = DeviceInput::of(device);
    let format = format_options(&device_input, image, &request.choices);
    let bootable = format.boot_type != BootType::NonBootable;
    let mut checks = device_checks(device, system, bootable);
    let mut check =
        |id: &'static str, status: CheckStatus, message: String, upstream: UpstreamRef| {
            checks.push(Check {
                id,
                status,
                message,
                upstream,
            });
        };

    let fs = format.file_system;
    match fs {
        Some(fs) => check(
            "file-system",
            CheckStatus::Pass,
            format!(
                "{} on {}",
                fs.label(),
                format.partition_scheme.map_or("-", PartitionScheme::label)
            ),
            UpstreamRef("src/rufus.c:SetFileSystemAndClusterSize()"),
        ),
        None => check(
            "file-system",
            CheckStatus::Fail,
            "No file system is selectable for this drive".into(),
            UpstreamRef("src/rufus.c:SetFileSystemAndClusterSize()"),
        ),
    }
    if format.partition_scheme == Some(PartitionScheme::Mbr)
        && facts.size_bytes > 2 * crate::units::TB
    {
        check(
            "mbr-2tb",
            CheckStatus::Warn,
            format!(
                "MBR can only use 2 TB of this device; {} would be unavailable",
                human(facts.size_bytes - 2 * crate::units::TB)
            ),
            UpstreamRef("src/rufus.c:MainCallback()"),
        );
    }
    let target = format.target_system.map(TargetSystem::target_type);
    let is_wtg = request.choices.image_option == ImageOption::WindowsToGo
        && scan.is_some_and(IsoScan::has_wintogo);

    if let (Some(image), Some(location)) = (image, request.image_location) {
        let at = image_location_check(&facts.name, location);
        check(at.id, at.status, at.message, at.upstream);
        if !image.is_iso || scan.is_none() {
            check(
                "iso-mode",
                CheckStatus::Fail,
                "this image cannot be written in ISO mode (use DD mode)".into(),
                IMAGE_SCAN,
            );
        } else if scan.is_some_and(|s| s.disable_iso) {
            check(
                "iso-mode",
                CheckStatus::Fail,
                "ISO mode is not supported for this image (use DD mode)".into(),
                IMAGE_SCAN,
            );
        } else if image.is_bootable_img.is_dd_bootable() {
            check(
                "iso-mode",
                CheckStatus::Warn,
                "an ISOHybrid image: written in ISO mode (Rufus asks; DD mode is also possible)"
                    .into(),
                BOOT_CHECK,
            );
        } else {
            check(
                "iso-mode",
                CheckStatus::Pass,
                "files are extracted to the new file system".into(),
                BOOT_CHECK,
            );
        }
        // Projected size, with upstream's 4 KB margin (it is an estimate).
        let projected = image.projected_size.unwrap_or(image.image_size);
        if projected > facts.size_bytes.saturating_add(4 * 1024) {
            check(
                "fits",
                CheckStatus::Fail,
                "The image is too big for the selected target.".into(),
                BOOT_CHECK,
            );
        } else {
            check(
                "fits",
                CheckStatus::Pass,
                format!("about {} of {}", human(projected), human(facts.size_bytes)),
                BOOT_CHECK,
            );
        }
        if let (Some(scan), Some(fs)) = (scan, fs) {
            let is_fat = matches!(fs, FileSystem::Fat16 | FileSystem::Fat32);
            let allow_dual = request.choices.allow_dual_uefi_bios;
            if is_wtg {
                if fs != FileSystem::Ntfs {
                    check(
                        "windows-to-go",
                        CheckStatus::Fail,
                        "'Windows To Go' can only be applied if the file system is NTFS.".into(),
                        BOOT_CHECK,
                    );
                }
                check(
                    "windows-to-go-support",
                    CheckStatus::Warn,
                    "Windows To Go needs a WIM reader, which this build does not have".into(),
                    UpstreamRef("src/wue.c:SetupWinToGo()"),
                );
            } else if target == Some(TargetType::Uefi) {
                if !scan.is_efi_bootable() {
                    check(
                        "target-system",
                        CheckStatus::Fail,
                        "When using UEFI Target Type, only EFI bootable ISO images are supported."
                            .into(),
                        BOOT_CHECK,
                    );
                }
            } else if (fs == FileSystem::Ntfs
                && !scan.has_windows()
                && !scan.has_grub()
                && (!scan.has_syslinux() || scan.sl_version >> 8 <= 5))
                || (is_fat
                    && !scan.has_syslinux()
                    && !allow_dual
                    && !scan.is_efi_bootable()
                    && !scan.has_reactos()
                    && !scan.has_kolibrios
                    && !scan.has_grub())
                || (is_fat && (scan.has_windows() || scan.has_wininst()) && !allow_dual)
            {
                check(
                    "file-system-compat",
                    CheckStatus::Fail,
                    "The file system currently selected can not be used with this type of ISO."
                        .into(),
                    BOOT_CHECK,
                );
            } else if fs == FileSystem::Fat16 && scan.has_kolibrios {
                check(
                    "file-system-compat",
                    CheckStatus::Fail,
                    "This ISO image is not compatible with the selected filesystem".into(),
                    BOOT_CHECK,
                );
            }
            if is_fat && scan.has_4gb_file != 0 && scan.has_4gb_file != 0x11 {
                check(
                    "large-files",
                    CheckStatus::Fail,
                    "This ISO image contains a file larger than 4 GB, which is more than the maximum size allowed for a FAT or FAT32 file system.".into(),
                    BOOT_CHECK,
                );
            }
            if scan.has_regular_efi()
                && projected < MAX_ISO_TO_ESP_SIZE
                && format.partition_scheme == Some(PartitionScheme::Gpt)
                && is_fat
            {
                check("iso-to-esp", CheckStatus::Pass, "small enough to be written as an EFI System Partition (Rufus offers 'ISO → ESP')".into(), BOOT_CHECK);
            }
            if target == Some(TargetType::Bios) {
                if format.partition_scheme == Some(PartitionScheme::Mbr)
                    && scan.has_grub2 != 0
                    && !scan.grub2_version.is_empty()
                    && scan.grub2_version != EMBEDDED_GRUB_VERSION
                {
                    check(
                        "grub-download",
                        CheckStatus::Warn,
                        format!(
                            "This image uses Grub {}: Rufus downloads the matching core.img (downloads are not available in this build)",
                            scan.grub2_version
                        ),
                        BOOT_CHECK,
                    );
                }
                if scan.has_syslinux() {
                    if scan.sl_version >> 8 < 5 {
                        if scan.has_old_c32.iter().any(|&old| old) {
                            check("syslinux-download", CheckStatus::Warn, "obsolete .c32 files are replaced with downloaded ones (downloads are not available in this build)".into(), BOOT_CHECK);
                        }
                    } else if scan.sl_version != crate::image::iso_scan::EMBEDDED_SL_VERSION[1]
                        || scan.sl_version_ext != EMBEDDED_SL6_EXT
                    {
                        check(
                            "syslinux-download",
                            CheckStatus::Warn,
                            format!(
                                "This image uses Syslinux {}{}: Rufus downloads the matching ldlinux files (downloads are not available in this build)",
                                scan.sl_version_str, scan.sl_version_ext
                            ),
                            BOOT_CHECK,
                        );
                    }
                }
            }
            if !scan.not_analyzed.is_empty() {
                check(
                    "not-analyzed",
                    CheckStatus::Warn,
                    format!(
                        "not analyzed in this build: {}",
                        scan.not_analyzed.join("; ")
                    ),
                    IMAGE_SCAN,
                );
            }
        }
    }

    // FormatThread(): extra partitions, then CreatePartition().
    let mut extra = ExtraPartitions::default();
    let iso_efi = scan.is_some_and(IsoScan::is_efi_bootable);
    if format.boot_type == BootType::Image
        && scan.is_some_and(IsoScan::has_persistence)
        && request.persistence > 0
    {
        extra.persistence = request.persistence;
    }
    if is_wtg
        && target == Some(TargetType::Uefi)
        && format.partition_scheme == Some(PartitionScheme::Gpt)
    {
        extra.esp_and_msr = true;
    } else if (format.boot_type == BootType::Image
        && iso_efi
        && matches!(fs, Some(FileSystem::Ntfs | FileSystem::ExFat)))
        || format.boot_type == BootType::UefiNtfs
    {
        extra.uefi_ntfs = !(format.boot_type == BootType::Image
            && scan.is_some_and(|s| s.has_bootmgr)
            && !is_wtg
            && target == Some(TargetType::Bios)
            && !request.choices.allow_dual_uefi_bios);
    }
    extra.compat = request.old_bios_fixes;
    let mut planned = Vec::new();
    if let (Some(scheme), Some(fs)) = (format.partition_scheme, fs) {
        let input = LayoutInput {
            scheme,
            file_system: fs,
            disk_size: facts.size_bytes,
            sector_size: facts.logical_block_size,
            sectors_per_track: crate::layout::DEFAULT_SECTORS_PER_TRACK,
            cluster_size: format.cluster_size.unwrap_or(0),
            old_bios_fixes: request.old_bios_fixes,
            extra,
            bootable,
            write_as_esp: false,
        };
        match crate::layout::layout(&input) {
            Ok(parts) => {
                check(
                    "layout",
                    CheckStatus::Pass,
                    format!("{} partition(s)", parts.len()),
                    UpstreamRef("src/drive.c:CreatePartition()"),
                );
                planned = parts;
            }
            Err(e) => check(
                "layout",
                CheckStatus::Fail,
                e.0,
                UpstreamRef("src/drive.c:CreatePartition()"),
            ),
        }
    }

    let steps = format_steps(device, &format, &extra, image.is_some(), options);
    let blocked = checks.iter().any(|c| c.status == CheckStatus::Fail);
    let mut plan = Plan {
        mode: if image.is_some() { "iso" } else { "format" },
        device: device.identity.clone(),
        image: image.map(|i| i.path.clone()),
        image_size: image.map(|i| i.image_size),
        payload_size: None,
        padding_bytes: 0,
        options,
        checks,
        steps,
        data_loss: data_loss(device),
        format: Some(format),
        layout: planned,
        blocked,
        executable: false,
        not_executable_reason: "this build of Ferrufus is a read-only preview and cannot write to devices",
        fingerprint: String::new(),
    };
    plan.fingerprint = fingerprint(&plan);
    plan
}

/// The stages of `FormatThread()` for ISO mode and plain formats.
fn format_steps(
    device: &DeviceEntry,
    format: &FormatOptions,
    extra: &ExtraPartitions,
    iso: bool,
    options: PlanOptions,
) -> Vec<Step> {
    let node = &device.facts.devnode;
    let step = |id, description: String, upstream| Step {
        id,
        description,
        upstream,
        implemented: false,
    };
    let fs = format.file_system.map_or("?", FileSystem::label);
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
            "create-partitions",
            format!(
                "create the {} partition table",
                format.partition_scheme.map_or("-", PartitionScheme::label)
            ),
            UpstreamRef("src/drive.c:CreatePartition()"),
        ),
    ];
    if extra.uefi_ntfs {
        steps.push(step(
            "uefi-ntfs",
            "write the UEFI:NTFS boot partition".into(),
            UpstreamRef("src/drive.c:CreatePartition()"),
        ));
    }
    steps.push(step(
        "format",
        format!(
            "format the main partition as {fs} with label \"{}\"",
            format.label
        ),
        UpstreamRef("src/format.c:FormatPartition()"),
    ));
    if format.boot_type != BootType::NonBootable {
        steps.push(step(
            "write-mbr",
            "write the boot code to the MBR".into(),
            UpstreamRef("src/format.c:WriteMBR()"),
        ));
        steps.push(step(
            "write-pbr",
            "write the boot code to the partition boot record".into(),
            UpstreamRef("src/format.c:WritePBR()"),
        ));
    }
    if iso {
        steps.push(step(
            "extract",
            "copy the files of the ISO to the new file system".into(),
            UpstreamRef("src/iso.c:ExtractISO()"),
        ));
        steps.push(step(
            "bootloader",
            "install and configure the boot loader".into(),
            UpstreamRef("src/syslinux.c:InstallSyslinux()"),
        ));
        if extra.persistence > 0 {
            steps.push(step(
                "persistence",
                format!(
                    "create a {} persistence partition",
                    human(extra.persistence)
                ),
                UpstreamRef("src/format.c:FormatThread()"),
            ));
        }
    }
    steps.push(step(
        "flush",
        "flush the data to the device and wait for it".into(),
        UpstreamRef::FERRUFUS,
    ));
    if iso && options.verify {
        steps.push(step(
            "verify",
            "read every file back and compare it with the image".into(),
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

/// How the image is written.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum WriteMode {
    /// The image is written as-is.
    Dd,
    /// The files of an ISO are copied to a new file system.
    Iso,
    /// The drive is only formatted (no image).
    Format,
}

/// A plan together with what it was made from.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PlanContext {
    /// The plan.
    pub plan: Plan,
    /// The image analysis.
    pub image: Option<ImageReport>,
    /// The disks holding the image file.
    pub image_disks: Vec<String>,
}

/// What [`prepare`] plans.
#[derive(Debug, Clone, Default)]
pub struct PrepareRequest {
    /// The image (required for DD and ISO modes).
    pub image: Option<std::path::PathBuf>,
    /// Inspection options.
    pub inspect: InspectOptions,
    /// Format choices (ISO mode and plain format).
    pub choices: Choices,
    /// Persistence size (ISO mode).
    pub persistence: u64,
    /// "Add fixes for old BIOSes".
    pub old_bios_fixes: bool,
}

/// Scans the devices under `root`, inspects the image and plans the write
/// to the device `device_query` (see `DeviceScan::find`). Read-only.
pub fn prepare(
    root: &SysRoot,
    scan_options: &ScanOptions,
    device_query: &str,
    mode: WriteMode,
    request: &PrepareRequest,
    options: PlanOptions,
) -> Result<PlanContext> {
    let image = match (&request.image, mode) {
        (Some(path), WriteMode::Dd | WriteMode::Iso) => {
            let path = std::fs::canonicalize(path)
                .map_err(|e| Error::io(format!("cannot find {}", path.display()), e))?;
            let report = inspect(&path, &request.inspect)?;
            Some((path, report))
        }
        (None, WriteMode::Dd | WriteMode::Iso) => {
            return Err(Error::InvalidInput(
                "an image is needed for this mode".into(),
            ));
        }
        (_, WriteMode::Format) => None,
    };
    let scan = scan(root, scan_options)?;
    let device = scan.find(device_query)?;
    let mount_list =
        mounts::parse_mountinfo(&root.read("/proc/self/mountinfo").unwrap_or_default());
    let location = image
        .as_ref()
        .map(|(path, _)| SystemInfo::disks_of_path(root, &mount_list, path));
    let plan = match (mode, &image, &location) {
        (WriteMode::Dd, Some((_, report)), Some(location)) => {
            plan_dd(device, &scan.system, report, location, options)
        }
        _ => {
            let format_request = FormatRequest {
                image: image.as_ref().map(|(_, report)| report),
                image_location: location.as_ref(),
                choices: Choices {
                    boot_type: request.choices.boot_type.or(Some(if image.is_some() {
                        BootType::Image
                    } else {
                        BootType::NonBootable
                    })),
                    ..request.choices.clone()
                },
                persistence: request.persistence,
                old_bios_fixes: request.old_bios_fixes,
            };
            plan_format_mode(device, &scan.system, &format_request, options)
        }
    };
    Ok(PlanContext {
        plan,
        image: image.map(|(_, report)| report),
        image_disks: location
            .map(|l| l.disks.into_iter().collect())
            .unwrap_or_default(),
    })
}

/// [`prepare`] in DD mode.
pub fn prepare_dd(
    root: &SysRoot,
    scan_options: &ScanOptions,
    device_query: &str,
    image_path: &Path,
    options: PlanOptions,
) -> Result<PlanContext> {
    let request = PrepareRequest {
        image: Some(image_path.to_path_buf()),
        ..PrepareRequest::default()
    };
    prepare(
        root,
        scan_options,
        device_query,
        WriteMode::Dd,
        &request,
        options,
    )
}
