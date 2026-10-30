// SPDX-License-Identifier: GPL-3.0-or-later
//! Which devices are offered as targets.
//!
//! Translated from the filtering in Rufus `src/dev.c:GetDevices()`
//! (Copyright © 2014-2026 Pete Batard, GPL-3.0-or-later) at the v4.15
//! baseline. Windows driver and enumerator checks map to Linux transports:
//! `USBSTOR` is `usb-storage`, `UASPSTOR` is `uas`, the `SD` enumerator is
//! an MMC host with an SD card, and mounted VHDs are loop devices. Every
//! applicable reason is reported, rather than only the first.

use super::facts::{DeviceFacts, Transport};
use super::hdd_heuristic::{self, HddInput, HddScore};
use super::system::SystemInfo;
use crate::units::{GB, MB};
use crate::upstream::UpstreamRef;
use serde::Serialize;

/// `MIN_DRIVE_SIZE`.
pub const MIN_DRIVE_SIZE: u64 = 8 * MB;
/// `MAX_DEFAULT_LIST_CARD_SIZE`.
pub const MAX_DEFAULT_LIST_CARD_SIZE: u64 = 500 * GB;

/// `scsi_card_name[]`: substrings of a SCSI hardware ID that mean "card reader".
const SCSI_CARD_NAMES: [&str; 8] = [
    "_SD_",
    "_SDHC_",
    "_SDXC_",
    "_MMC_",
    "_MS_",
    "_MSPro_",
    "_xDPicture_",
    "_O2Media_",
];

/// Options that widen the list, as in Rufus.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ListingOptions {
    /// "List USB Hard Drives" (`-x`/`--extra-devs`, Alt-F, `enable_HDDs`).
    pub list_usb_hdd: bool,
    /// List non-USB removable drives (Ctrl-Alt-F, `list_non_usb_removable_drives`).
    pub list_non_usb_removable: bool,
    /// List loop devices (Rufus lists mounted VHDs unless Alt-G disables them;
    /// Ferrufus lists loop devices only on request).
    pub list_virtual: bool,
    /// USB devices to ignore, as `IgnoreUsb01`–`IgnoreUsb08` do.
    pub ignore_usb: Vec<(u16, u16)>,
}

/// How a device is classified.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Category {
    /// USB mass storage (bulk-only).
    UsbDrive,
    /// USB Attached SCSI.
    UasDrive,
    /// A card reader (SD host, or a SCSI device with a card reader name).
    CardReader,
    /// A removable drive that is not on USB.
    NonUsbRemovable,
    /// A loop device (the Linux counterpart of a mounted VHD).
    VirtualDisk,
    /// A fixed disk that is not on USB (NVMe, SATA, virtio, eMMC).
    Internal,
    /// Not a disk Rufus would consider (device mapper, zram, optical, ...).
    Other,
}

/// Why a device is excluded, or a notable property of a listed one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ReasonCode {
    /// Not a storage device type Rufus enumerates.
    UnsupportedType,
    /// Non-USB and non-removable: most likely a system drive.
    InternalDisk,
    /// Non-USB removable drives are hidden by default.
    NonUsbRemovableHidden,
    /// Loop devices are hidden by default.
    VirtualHidden,
    /// Ignored by VID:PID.
    Ignored,
    /// No medium.
    NoMedia,
    /// Smaller than `MIN_DRIVE_SIZE`.
    TooSmall,
    /// Detected as a hard drive or SSD.
    UsbHddHidden,
    /// A card larger than 500 GB.
    LargeCardHidden,
    /// Backs the running system or holds the running program.
    SystemDisk,
}

/// A reason, with the upstream rule it comes from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Reason {
    /// Machine-readable code.
    pub code: ReasonCode,
    /// Explanation.
    pub message: String,
    /// The upstream rule.
    pub upstream: UpstreamRef,
}

/// The listing decision for one device.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ListingDecision {
    /// Whether the device is offered as a target.
    pub listed: bool,
    /// Classification.
    pub category: Category,
    /// The hard drive heuristic, when it applies.
    pub hdd_score: Option<HddScore>,
    /// Why the device is excluded (empty when listed).
    pub reasons: Vec<Reason>,
}

const GET_DEVICES: UpstreamRef = UpstreamRef("src/dev.c:GetDevices()");

/// Applies the listing rules to one device.
pub fn decide(
    facts: &DeviceFacts,
    system: &SystemInfo,
    options: &ListingOptions,
) -> ListingDecision {
    let usb = facts.usb();
    let is_uas = usb.is_some_and(|usb| usb.is_uas());
    let is_vhd = matches!(&facts.transport, Transport::Loop { .. });
    let sd_card = matches!(&facts.transport, Transport::Mmc { card_type: Some(t) } if t == "SD");
    // Windows "SCSI" enumerator: UAS devices and non-USB SCSI disks.
    let scsi_enumerated = is_uas || matches!(facts.transport, Transport::Scsi);
    let is_card = sd_card || (scsi_enumerated && has_card_reader_name(facts));
    // An SD card in an SD host is removable media, whatever the flag says.
    let removable = facts.removable || sd_card;

    let category = if is_vhd {
        Category::VirtualDisk
    } else if is_card {
        Category::CardReader
    } else if is_uas {
        Category::UasDrive
    } else if usb.is_some() {
        Category::UsbDrive
    } else if matches!(
        facts.transport,
        Transport::Virtual { .. } | Transport::Optical | Transport::Other
    ) {
        Category::Other
    } else if removable {
        Category::NonUsbRemovable
    } else {
        Category::Internal
    };

    let mut reasons = Vec::new();
    let mut exclude = |code: ReasonCode, message: String| {
        reasons.push(Reason {
            code,
            message,
            upstream: GET_DEVICES,
        })
    };

    match category {
        Category::Other => exclude(
            ReasonCode::UnsupportedType,
            "not a storage device type that is listed (virtual, optical or unknown device)"
                .to_string(),
        ),
        Category::VirtualDisk if !options.list_virtual => {
            exclude(
                ReasonCode::VirtualHidden,
                "loop devices are only listed on request".to_string(),
            );
        }
        Category::NonUsbRemovable if !options.list_non_usb_removable => exclude(
            ReasonCode::NonUsbRemovableHidden,
            "non-USB removable drives are only listed on request (Ctrl-Alt-F in Rufus)".to_string(),
        ),
        Category::Internal => exclude(
            ReasonCode::InternalDisk,
            "non-USB, non-removable disk: most likely a system drive".to_string(),
        ),
        _ => {}
    }
    // Non-USB SCSI devices (including card readers found by name) must be
    // removable: "Non removables should have been eliminated above, but since
    // we are potentially dealing with system drives, better safe than sorry".
    if matches!(facts.transport, Transport::Scsi | Transport::Mmc { .. })
        && !removable
        && category != Category::Internal
    {
        exclude(
            ReasonCode::InternalDisk,
            "non-USB device that is not removable".to_string(),
        );
    }
    if let Some(usb) = usb
        && options.ignore_usb.contains(&(usb.vid, usb.pid))
    {
        exclude(
            ReasonCode::Ignored,
            format!("ignored per settings ({:04X}:{:04X})", usb.vid, usb.pid),
        );
    }

    if facts.size_bytes == 0 {
        exclude(
            ReasonCode::NoMedia,
            "it appears to contain no media".to_string(),
        );
    } else if facts.size_bytes < MIN_DRIVE_SIZE {
        exclude(ReasonCode::TooSmall, "it is smaller than 8 MB".to_string());
    }

    let mut hdd_score = None;
    if let (Some(usb), false, false) = (usb, is_vhd, is_card) {
        let name = friendly_name(facts);
        let score = hdd_heuristic::score(&HddInput {
            fixed: !removable,
            size: facts.size_bytes,
            name: name.as_deref(),
            vid: usb.vid,
            pid: usb.pid,
        });
        if !options.list_usb_hdd && score.is_hdd() {
            exclude(
                ReasonCode::UsbHddHidden,
                format!(
                    "detected as a Hard Drive or SSD (score {} > 0); use \"List USB Hard Drives\" to show it",
                    score.total
                ),
            );
        }
        hdd_score = Some(score);
    }
    if !options.list_usb_hdd && is_card && facts.size_bytes > MAX_DEFAULT_LIST_CARD_SIZE {
        exclude(
            ReasonCode::LargeCardHidden,
            "a card larger than 500 GB; use \"List USB Hard Drives\" to show it".to_string(),
        );
    }

    if let Some(why) = system.system_disks.get(&facts.name) {
        exclude(
            ReasonCode::SystemDisk,
            format!("the running system depends on it: {}", why.join("; ")),
        );
    }

    ListingDecision {
        listed: reasons.is_empty(),
        category,
        hdd_score,
        reasons,
    }
}

/// The Windows-style hardware ID of a SCSI-attached disk
/// (`SCSI\Disk<vendor><product><revision>`, spaces replaced by `_`).
fn hardware_id(facts: &DeviceFacts) -> String {
    let field = |value: &Option<String>| value.as_deref().unwrap_or_default().replace(' ', "_");
    format!(
        "SCSI\\Disk{}{}{}",
        field(&facts.scsi_vendor),
        field(&facts.scsi_model),
        field(&facts.scsi_revision)
    )
}

/// The card reader name test of `GetDevices()`: each `scsi_card_name[]`
/// substring, and the same with its last `_` replaced by `&`.
fn has_card_reader_name(facts: &DeviceFacts) -> bool {
    let id = hardware_id(facts);
    SCSI_CARD_NAMES.iter().any(|name| {
        let ampersand = name.strip_suffix('_').map(|stem| format!("{stem}&"));
        id.contains(name) || ampersand.is_some_and(|alt| id.contains(&alt))
    })
}

/// The name Windows shows for a USB disk (`<vendor> <product> USB Device`),
/// which `IsHDD()` matches against; UAS devices end with "UAS Device", as
/// Rufus renames them.
pub fn friendly_name(facts: &DeviceFacts) -> Option<String> {
    let usb = facts.usb()?;
    let parts: Vec<&str> = [facts.scsi_vendor.as_deref(), facts.scsi_model.as_deref()]
        .into_iter()
        .flatten()
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .collect();
    if parts.is_empty() {
        return None;
    }
    let suffix = if usb.is_uas() {
        "UAS Device"
    } else {
        "USB Device"
    };
    Some(format!("{} {suffix}", parts.join(" ")))
}
