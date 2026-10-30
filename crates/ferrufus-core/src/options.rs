// SPDX-License-Identifier: GPL-3.0-or-later
//! The format options Rufus offers and proposes: partition scheme, target
//! system, file system, cluster size and volume label.
//!
//! Translated from Rufus `src/rufus.c` at v4.15 (Copyright © 2011-2026
//! Pete Batard, GPL-3.0-or-later): `SetPartitionSchemeAndTargetSystem()`,
//! `SetAllowedFileSystems()`, `SetFileSystemAndClusterSize()`,
//! `SetClusterSizes()`, `SetFSFromISO()` and `SetProposedLabel()`, applied
//! in the order `ImageScanThread()` calls them once an image is scanned.
//! The dialog state those functions read (combo box contents, globals) is
//! passed explicitly. ReFS is never offered: Rufus only offers it on some
//! Windows editions (`IsRefsAvailable()`), and Linux cannot create it.

use crate::image::ImageReport;
use crate::image::iso_scan::IsoScan;
use crate::units::{GB, KB, MB, SizeUnits, TB, size_to_human_readable};
use crate::upstream::UpstreamRef;
use serde::Serialize;

/// `MIN_EXT_SIZE`.
const MIN_EXT_SIZE: u64 = 256 * MB;
/// `SINGLE_CLUSTERSIZE_DEFAULT`.
const SINGLE_CLUSTERSIZE_DEFAULT: u32 = 0x100;
/// `LARGE_FAT32_SIZE`.
pub const LARGE_FAT32_SIZE: u64 = 32 * GB;
/// `MAX_FAT32_SIZE`.
const MAX_FAT32_SIZE: u64 = 2 * TB;
/// `FAT32_CLUSTER_THRESHOLD` (a float in upstream).
const FAT32_CLUSTER_THRESHOLD: f32 = 1.011;

/// `enum boot_type`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum BootType {
    /// Non bootable.
    NonBootable,
    /// MS-DOS.
    MsDos,
    /// FreeDOS.
    FreeDos,
    /// Disk or ISO image.
    Image,
    /// Syslinux 4 (advanced mode).
    SyslinuxV4,
    /// Syslinux 6 (advanced mode).
    SyslinuxV6,
    /// ReactOS (advanced mode).
    ReactOs,
    /// Grub4DOS (advanced mode).
    Grub4Dos,
    /// GRUB 2 (advanced mode).
    Grub2,
    /// UEFI:NTFS (advanced mode).
    UefiNtfs,
}

/// Partition schemes (`PARTITION_STYLE_*`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum PartitionScheme {
    /// MBR.
    Mbr,
    /// GPT.
    Gpt,
    /// Super Floppy Disk: no partition table.
    Sfd,
}

impl PartitionScheme {
    /// The name shown in the list.
    pub fn label(self) -> &'static str {
        match self {
            Self::Mbr => "MBR",
            Self::Gpt => "GPT",
            Self::Sfd => "Super Floppy Disk",
        }
    }
}

/// `enum target_type`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum TargetType {
    /// `TT_BIOS`.
    Bios,
    /// `TT_UEFI`.
    Uefi,
}

/// An entry of the target system list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum TargetSystem {
    /// "BIOS (or UEFI-CSM)" (`MSG_031`, `TT_BIOS`).
    BiosOrCsm,
    /// "UEFI (non CSM)" (`MSG_032`, `TT_UEFI`).
    Uefi,
    /// "BIOS or UEFI" (`MSG_033`, `TT_BIOS`).
    BiosOrUefi,
}

impl TargetSystem {
    /// The name shown in the list.
    pub fn label(self) -> &'static str {
        match self {
            Self::BiosOrCsm => "BIOS (or UEFI-CSM)",
            Self::Uefi => "UEFI (non CSM)",
            Self::BiosOrUefi => "BIOS or UEFI",
        }
    }

    /// The `target_type` value of the entry.
    pub fn target_type(self) -> TargetType {
        match self {
            Self::Uefi => TargetType::Uefi,
            Self::BiosOrCsm | Self::BiosOrUefi => TargetType::Bios,
        }
    }
}

/// `enum fs_type`, in upstream order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum FileSystem {
    /// FAT (FAT16).
    Fat16,
    /// FAT32.
    Fat32,
    /// NTFS.
    Ntfs,
    /// UDF.
    Udf,
    /// exFAT.
    ExFat,
    /// ReFS.
    Refs,
    /// ext2.
    Ext2,
    /// ext3.
    Ext3,
    /// ext4.
    Ext4,
}

impl FileSystem {
    /// All file systems, in upstream order.
    pub const ALL: [Self; 9] = [
        Self::Fat16,
        Self::Fat32,
        Self::Ntfs,
        Self::Udf,
        Self::ExFat,
        Self::Refs,
        Self::Ext2,
        Self::Ext3,
        Self::Ext4,
    ];

    /// `FileSystemLabel[]`.
    pub fn label(self) -> &'static str {
        match self {
            Self::Fat16 => "FAT",
            Self::Fat32 => "FAT32",
            Self::Ntfs => "NTFS",
            Self::Udf => "UDF",
            Self::ExFat => "exFAT",
            Self::Refs => "ReFS",
            Self::Ext2 => "ext2",
            Self::Ext3 => "ext3",
            Self::Ext4 => "ext4",
        }
    }

    /// Looks a file system up by its label (case-insensitive).
    pub fn from_label(label: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|fs| fs.label().eq_ignore_ascii_case(label))
    }

    fn index(self) -> u32 {
        match self {
            Self::Fat16 => 0,
            Self::Fat32 => 1,
            Self::Ntfs => 2,
            Self::Udf => 3,
            Self::ExFat => 4,
            Self::Refs => 5,
            Self::Ext2 => 6,
            Self::Ext3 => 7,
            Self::Ext4 => 8,
        }
    }

    fn is_fat(self) -> bool {
        matches!(self, Self::Fat16 | Self::Fat32)
    }
}

/// `enum image_option_type`: how a Windows image is installed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ImageOption {
    /// Standard Windows installation.
    #[default]
    Standard,
    /// Extended installation (TPM/Secure Boot/RAM checks removed).
    Extended,
    /// Windows To Go.
    WindowsToGo,
}

/// The device the options are for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceInput {
    /// `SelectedDrive.DiskSize`.
    pub size: u64,
    /// `SelectedDrive.SectorSize`.
    pub sector_size: u32,
    /// `SelectedDrive.PartitionStyle`: the current partition table.
    pub partition_scheme: Option<PartitionScheme>,
    /// The label of the device's volume, if it has exactly one.
    pub volume_label: Option<String>,
}

/// What the user chose, or what Rufus remembers from earlier choices.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Choices {
    /// Boot selection (`Image` when an image is given).
    pub boot_type: Option<BootType>,
    /// Windows installation type.
    pub image_option: ImageOption,
    /// `allow_dual_uefi_bios` (Alt-E).
    pub allow_dual_uefi_bios: bool,
    /// `selected_pt`: a partition scheme the user picked.
    pub partition_scheme: Option<PartitionScheme>,
    /// The target system the user picked (the first entry otherwise).
    pub target_system: Option<TargetSystem>,
    /// `selected_fs`: a file system the user picked.
    pub file_system: Option<FileSystem>,
    /// `preselected_fs` (`-f`/`--filesystem`).
    pub preselected_fs: Option<FileSystem>,
    /// `advanced_mode_format`: show ext2/3/4.
    pub advanced_format: bool,
    /// `force_large_fat32` (Alt-L).
    pub force_large_fat32: bool,
    /// A label the user typed (`user_changed_label`).
    pub label: Option<String>,
}

/// A file system the user can pick.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FileSystemEntry {
    /// The file system.
    pub file_system: FileSystem,
    /// The name shown ("Large FAT32", "NTFS (Default)", ...).
    pub label: String,
    /// Allowed cluster sizes in bytes.
    pub cluster_sizes: Vec<u32>,
    /// Default cluster size in bytes (0 means "the file system's default").
    pub default_cluster_size: u32,
}

/// A restriction, with the rule it comes from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OptionNote {
    /// Explanation.
    pub message: String,
    /// The upstream rule.
    pub upstream: UpstreamRef,
}

/// What Rufus would offer and select.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FormatOptions {
    /// Boot selection.
    pub boot_type: BootType,
    /// Partition schemes offered.
    pub partition_schemes: Vec<PartitionScheme>,
    /// Selected partition scheme.
    pub partition_scheme: Option<PartitionScheme>,
    /// Target systems offered.
    pub target_systems: Vec<TargetSystem>,
    /// Selected target system.
    pub target_system: Option<TargetSystem>,
    /// File systems offered, with their cluster sizes.
    pub file_systems: Vec<FileSystemEntry>,
    /// Selected file system.
    pub file_system: Option<FileSystem>,
    /// Selected cluster size (bytes; 0 means "the file system's default").
    pub cluster_size: Option<u32>,
    /// Proposed volume label.
    pub label: String,
    /// Whether FAT32 will be formatted as "Large FAT32" (fat32format).
    pub large_fat32: bool,
    /// Why options are restricted.
    pub notes: Vec<OptionNote>,
}

/// The image facts the rules use.
struct ImageFacts<'a> {
    present: bool,
    is_iso: bool,
    is_windows_img: bool,
    dd_bootable: bool,
    scan: Option<&'a IsoScan>,
    label: Option<&'a str>,
}

impl ImageFacts<'_> {
    fn scan_flag(&self, f: impl Fn(&IsoScan) -> bool) -> bool {
        self.scan.is_some_and(f)
    }
    fn is_efi_bootable(&self) -> bool {
        self.scan_flag(IsoScan::is_efi_bootable)
    }
    fn is_bios_bootable(&self) -> bool {
        self.scan_flag(IsoScan::is_bios_bootable)
    }
    fn has_windows(&self) -> bool {
        self.scan_flag(IsoScan::has_windows)
    }
    fn has_wintogo(&self) -> bool {
        self.scan_flag(IsoScan::has_wintogo)
    }
    fn has_4gb_file(&self) -> u8 {
        self.scan.map_or(0, |s| s.has_4gb_file)
    }
    /// `IS_FAT32_COMPAT()`.
    fn is_fat32_compat(&self, allow_dual_uefi_bios: bool) -> bool {
        let Some(scan) = self.scan else { return true };
        ((scan.has_4gb_file == 0 && !scan.has_fatless_grub())
            || (scan.has_4gb_file == 0x11 && allow_dual_uefi_bios))
            && !scan.needs_ntfs
    }
}

const RULES: UpstreamRef = UpstreamRef("src/rufus.c:SetPartitionSchemeAndTargetSystem()");
const FS_RULES: UpstreamRef = UpstreamRef("src/rufus.c:SetAllowedFileSystems()");

/// Computes the options for a device, an image (when booting from one)
/// and the user's choices.
pub fn format_options(
    device: &DeviceInput,
    image: Option<&ImageReport>,
    choices: &Choices,
) -> FormatOptions {
    let facts = ImageFacts {
        present: image.is_some(),
        is_iso: image.is_some_and(|i| i.is_iso),
        is_windows_img: image.is_some_and(|i| i.is_windows_img),
        dd_bootable: image.is_some_and(|i| i.is_bootable_img.is_dd_bootable()),
        scan: image.and_then(|i| i.iso_scan.as_ref()),
        label: image.and_then(|i| i.label.as_deref()),
    };
    let boot_type = choices.boot_type.unwrap_or(if facts.present {
        BootType::Image
    } else {
        BootType::NonBootable
    });
    let mut notes = Vec::new();
    let is_wtg_selected = boot_type == BootType::Image
        && facts.present
        && facts.has_wintogo()
        && choices.image_option == ImageOption::WindowsToGo;

    // SetPartitionSchemeAndTargetSystem()
    let mut schemes = [true, true, false];
    let mut targets = [true, true, false];
    match boot_type {
        BootType::NonBootable => {
            schemes[2] = true;
            targets = [false, false, true];
        }
        BootType::Image if facts.present => {
            if !facts.is_efi_bootable() {
                schemes[1] = false;
                targets[1] = false;
                notes.push(OptionNote {
                    message: "the image is not UEFI bootable: GPT and UEFI are not offered".into(),
                    upstream: RULES,
                });
            } else if facts.is_bios_bootable() {
                if !facts.has_windows() || choices.allow_dual_uefi_bios || is_wtg_selected {
                    targets = [false, true, true];
                }
                if let Some(scan) = facts.scan
                    && scan.has_syslinux()
                    && scan.sl_version >> 8 < 5
                    && scan.has_4gb_file != 0
                    && !scan.has_bootmgr()
                    && !scan.has_winpe()
                    && !scan.has_grub()
                {
                    schemes[0] = false;
                    notes.push(OptionNote {
                        message: "Syslinux 4 cannot boot from NTFS, which a file over 4 GB requires: MBR is not offered".into(),
                        upstream: RULES,
                    });
                }
            } else {
                targets[0] = false;
            }
        }
        BootType::Image => {}
        BootType::MsDos
        | BootType::FreeDos
        | BootType::SyslinuxV4
        | BootType::SyslinuxV6
        | BootType::ReactOs
        | BootType::Grub4Dos
        | BootType::Grub2 => {
            schemes[1] = false;
            targets[1] = false;
        }
        BootType::UefiNtfs => targets[0] = false,
    }
    let partition_schemes: Vec<PartitionScheme> = [
        PartitionScheme::Mbr,
        PartitionScheme::Gpt,
        PartitionScheme::Sfd,
    ]
    .into_iter()
    .zip(schemes)
    .filter_map(|(scheme, allowed)| allowed.then_some(scheme))
    .collect();
    let mut selected_pt = choices.partition_scheme;
    if device.size > 2 * TB {
        selected_pt = Some(PartitionScheme::Gpt);
    }
    let mut preferred = device.partition_scheme;
    match boot_type {
        BootType::NonBootable => preferred = Some(selected_pt.unwrap_or(PartitionScheme::Mbr)),
        BootType::UefiNtfs => preferred = Some(selected_pt.unwrap_or(PartitionScheme::Gpt)),
        BootType::Image if facts.present && (facts.is_iso || facts.is_windows_img) => {
            if facts.has_windows() && facts.scan.is_some_and(|s| s.has_efi != 0) {
                preferred = Some(if choices.allow_dual_uefi_bios {
                    PartitionScheme::Mbr
                } else {
                    selected_pt.unwrap_or(PartitionScheme::Gpt)
                });
            }
            if facts.dd_bootable {
                preferred = Some(selected_pt.unwrap_or(PartitionScheme::Mbr));
            }
        }
        _ => {}
    }
    // SetComboEntry(): the preferred entry, or the first one.
    let partition_scheme = preferred
        .filter(|p| partition_schemes.contains(p))
        .or_else(|| partition_schemes.first().copied());

    let mut target_systems = Vec::new();
    if targets[0] && partition_scheme != Some(PartitionScheme::Gpt) {
        target_systems.push(TargetSystem::BiosOrCsm);
    }
    if targets[1]
        && !(partition_scheme == Some(PartitionScheme::Mbr)
            && boot_type == BootType::Image
            && facts.is_bios_bootable()
            && facts.is_efi_bootable())
    {
        target_systems.push(TargetSystem::Uefi);
    }
    if targets[2]
        && (partition_scheme != Some(PartitionScheme::Gpt) || boot_type == BootType::NonBootable)
    {
        target_systems.push(TargetSystem::BiosOrUefi);
    }
    let target_system = choices
        .target_system
        .filter(|t| target_systems.contains(t))
        .or_else(|| target_systems.first().copied());
    let target_type = target_system.map_or(TargetType::Bios, TargetSystem::target_type);

    // SetFileSystemAndClusterSize()
    let clusters = cluster_sizes(device, choices);
    let allowed = allowed_file_systems(
        boot_type,
        &facts,
        target_type,
        choices,
        is_wtg_selected,
        &mut notes,
    );
    let large_fat32 = device.size > LARGE_FAT32_SIZE || choices.force_large_fat32;
    let mut default_fs: Option<FileSystem> = None;
    let mut file_systems = Vec::new();
    for (fs, (allowed_mask, default)) in FileSystem::ALL.into_iter().zip(clusters) {
        if allowed_mask == 0 {
            continue;
        }
        let mut label = String::new();
        if fs == FileSystem::Fat32 && large_fat32 {
            label.push_str("Large ");
        }
        label.push_str(fs.label());
        if default_fs.is_none() {
            default_fs = Some(fs);
            label = format!("{label} (Default)");
        }
        if allowed.contains(&fs) {
            file_systems.push(FileSystemEntry {
                file_system: fs,
                label,
                cluster_sizes: cluster_list(allowed_mask),
                default_cluster_size: if default == 1 { 0 } else { default },
            });
        }
    }
    let offered =
        |fs: Option<FileSystem>| fs.filter(|f| file_systems.iter().any(|e| e.file_system == *f));
    let mut file_system = offered(choices.file_system)
        .or_else(|| offered(default_fs))
        .or_else(|| file_systems.first().map(|e| e.file_system));

    // SetFSFromISO(), for ISO and Windows images.
    if boot_type == BootType::Image && (facts.is_iso || facts.is_windows_img) {
        if let Some(fs) = fs_from_iso(&facts, &file_systems, target_type, choices, is_wtg_selected)
        {
            file_system = Some(fs);
        }
        if let Some(user) = offered(choices.file_system) {
            // A file system the user picked earlier stays selected.
            file_system = Some(user);
        }
    }
    let cluster_size = file_system
        .and_then(|fs| file_systems.iter().find(|e| e.file_system == fs))
        .map(|e| e.default_cluster_size);

    // SetProposedLabel()
    let label = match (&choices.label, facts.label) {
        (Some(user), _) => user.clone(),
        (None, Some(image_label)) if boot_type == BootType::Image && !image_label.is_empty() => {
            image_label.to_string()
        }
        _ => match device.volume_label.as_deref() {
            Some(label)
                if !label.is_empty()
                    && !label.eq_ignore_ascii_case("NO_LABEL")
                    && !label.eq_ignore_ascii_case("New Volume") =>
            {
                label.to_string()
            }
            _ => size_to_human_readable(device.size, SizeUnits::Fake),
        },
    };

    FormatOptions {
        boot_type,
        partition_schemes,
        partition_scheme,
        target_systems,
        target_system,
        file_systems,
        file_system,
        cluster_size,
        label,
        large_fat32,
        notes,
    }
}

/// `SetAllowedFileSystems()`.
fn allowed_file_systems(
    boot_type: BootType,
    facts: &ImageFacts<'_>,
    target_type: TargetType,
    choices: &Choices,
    is_wtg_selected: bool,
    notes: &mut Vec<OptionNote>,
) -> Vec<FileSystem> {
    let mut allowed = Vec::new();
    match boot_type {
        BootType::NonBootable => allowed.extend(FileSystem::ALL),
        BootType::MsDos | BootType::FreeDos => {
            allowed.extend([FileSystem::Fat16, FileSystem::Fat32])
        }
        BootType::Image => {
            if !facts.present || !facts.scan.is_some_and(IsoScan::has_ntfsless_grub) {
                allowed.push(FileSystem::Ntfs);
            }
            if facts.present && !facts.is_fat32_compat(choices.allow_dual_uefi_bios) {
                if allowed.contains(&FileSystem::Ntfs) {
                    notes.push(OptionNote {
                        message: "the image needs NTFS (a file of 4 GB or more, symbolic links, or a GRUB without FAT support)".into(),
                        upstream: FS_RULES,
                    });
                    return allowed;
                }
                notes.push(OptionNote {
                    message:
                        "FAT32 has been forcefully enabled, but this image may not work with FAT32"
                            .into(),
                    upstream: FS_RULES,
                });
            }
            if (!facts.has_windows()
                || target_type != TargetType::Bios
                || choices.allow_dual_uefi_bios)
                && (!facts.has_wintogo() || !is_wtg_selected)
            {
                allowed.extend([FileSystem::Fat16, FileSystem::Fat32]);
            }
        }
        BootType::Grub2 => {
            allowed.extend([
                FileSystem::Ext2,
                FileSystem::Ext3,
                FileSystem::Ext4,
                FileSystem::Ntfs,
            ]);
            allowed.extend([FileSystem::Fat16, FileSystem::Fat32]);
        }
        BootType::SyslinuxV6 | BootType::Grub4Dos => {
            allowed.extend([FileSystem::Ntfs, FileSystem::Fat16, FileSystem::Fat32]);
        }
        BootType::SyslinuxV4 | BootType::ReactOs => {
            allowed.extend([FileSystem::Fat16, FileSystem::Fat32])
        }
        BootType::UefiNtfs => allowed.extend([FileSystem::Ntfs, FileSystem::ExFat]),
    }
    allowed
}

/// `SetFSFromISO()`: the preferred file system, if it is offered.
fn fs_from_iso(
    facts: &ImageFacts<'_>,
    entries: &[FileSystemEntry],
    target_type: TargetType,
    choices: &Choices,
    is_wtg_selected: bool,
) -> Option<FileSystem> {
    let has_4gb = facts.has_4gb_file() != 0;
    // Upstream seeds the mask with the enum values themselves (FS_NTFS is
    // 2, FS_FAT32 is 1), which sets the FAT16 and FAT32 bits.
    let mut mask: u32 = 2 | if has_4gb { 0 } else { 1 };
    for entry in entries {
        mask |= 1 << entry.file_system.index();
    }
    let has = |fs: FileSystem| mask & (1 << fs.index()) != 0;
    let scan = facts.scan;
    let flag = |f: fn(&IsoScan) -> bool| scan.is_some_and(f);
    let preferred = if let Some(fs) = choices.preselected_fs.filter(|&fs| has(fs)) {
        Some(fs)
    } else if flag(IsoScan::has_syslinux)
        || flag(IsoScan::has_reactos)
        || scan.is_some_and(|s| s.has_kolibrios)
        || (facts.is_efi_bootable()
            && target_type == TargetType::Uefi
            && !is_wtg_selected
            && !has_4gb)
    {
        if has(FileSystem::Fat32) {
            Some(FileSystem::Fat32)
        } else if has(FileSystem::Fat16) && !scan.is_some_and(|s| s.has_kolibrios) {
            Some(FileSystem::Fat16)
        } else {
            None
        }
    } else if is_wtg_selected || flag(IsoScan::has_bootmgr) || flag(IsoScan::has_winpe) {
        if has(FileSystem::Fat32) && !has_4gb && choices.allow_dual_uefi_bios {
            Some(FileSystem::Fat32)
        } else if has(FileSystem::Ntfs) {
            Some(FileSystem::Ntfs)
        } else {
            None
        }
    } else {
        None
    };
    preferred.filter(|fs| entries.iter().any(|e| e.file_system == *fs))
}

/// The cluster size tables of `SetFileSystemAndClusterSize()`: for each
/// file system in upstream order, the allowed sizes (a bit mask of byte
/// sizes, or `SINGLE_CLUSTERSIZE_DEFAULT`) and the default.
#[allow(clippy::cast_precision_loss)] // Upstream compares as float.
fn cluster_sizes(device: &DeviceInput, choices: &Choices) -> [(u32, u32); 9] {
    let size = device.size;
    let mut table = [(0u32, 0u32); 9];
    let [fat16, fat32, ntfs, udf, exfat, _refs, ext2, ext3, _ext4] = &mut table;
    if size < 4 * GB {
        let mut allowed: u32 = 0x0000_1e00;
        let mut default = 0;
        let mut i: u64 = 32;
        while i <= 4096 {
            if size < i.saturating_mul(MB) {
                default = u32::try_from(16u64.saturating_mul(i)).unwrap_or(0);
                break;
            }
            allowed <<= 1;
            i <<= 1;
        }
        *fat16 = (allowed & 0x0001_fe00, default);
    }
    if (32 * MB..MAX_FAT32_SIZE).contains(&size) {
        let mut allowed: u32 = 0x0000_01f8;
        let mut default = 0;
        let mut i: u64 = 32;
        while i <= 32 * 1024 {
            // DiskSize*1.0f < i * MB * FAT32_CLUSTER_THRESHOLD, in float.
            if (size as f32) < (i.saturating_mul(MB) as f32) * FAT32_CLUSTER_THRESHOLD {
                default = u32::try_from(8u64.saturating_mul(i)).unwrap_or(0);
                break;
            }
            allowed <<= 1;
            i <<= 1;
        }
        allowed &= 0x0001_fe00;
        if (256 * MB..32 * GB).contains(&size) {
            let mut i: u64 = 8;
            while i <= 32 {
                if (size as f32) < (i.saturating_mul(GB) as f32) * FAT32_CLUSTER_THRESHOLD {
                    default = u32::try_from((i / 2).saturating_mul(KB)).unwrap_or(0);
                    break;
                }
                i <<= 1;
            }
        }
        if size >= 32 * GB {
            allowed &= 0x0001_c000;
            default = 0x0000_8000;
        }
        *fat32 = (allowed, default);
    }
    if size < 256 * TB {
        let mut default = 0;
        let mut i: u64 = 16;
        while i <= 256 {
            if size < i.saturating_mul(TB) {
                default = u32::try_from((i / 4).saturating_mul(KB)).unwrap_or(0);
                break;
            }
            i <<= 1;
        }
        *ntfs = (0x0001_f000, default);
        let exfat_default = if size < 256 * MB {
            4 * KB
        } else if size < 32 * GB {
            32 * KB
        } else {
            128 * KB
        };
        *exfat = (0x03ff_fe00, u32::try_from(exfat_default).unwrap_or(0));
        *udf = (SINGLE_CLUSTERSIZE_DEFAULT, 1);
        if choices.advanced_format && size >= MIN_EXT_SIZE {
            *ext2 = (SINGLE_CLUSTERSIZE_DEFAULT, 1);
            *ext3 = (SINGLE_CLUSTERSIZE_DEFAULT, 1);
        }
    }
    // Remove sizes below the sector size; a lost default becomes the
    // smallest remaining size.
    let sector_mask = !(device.sector_size.max(1).saturating_sub(1));
    for (allowed, default) in &mut table {
        if *allowed != SINGLE_CLUSTERSIZE_DEFAULT {
            *allowed &= sector_mask;
            if *default & *allowed == 0 {
                *default = *allowed & allowed.wrapping_neg();
            }
        }
    }
    table
}

/// `SetClusterSizes()`: the sizes of a mask, from 256 bytes up.
fn cluster_list(allowed: u32) -> Vec<u32> {
    if allowed == SINGLE_CLUSTERSIZE_DEFAULT {
        return vec![0];
    }
    (8..28)
        .map(|shift| 1u32 << shift)
        .filter(|size| allowed & size != 0)
        .collect()
}

impl FormatOptions {
    /// Whether the selected file system is FAT.
    pub fn is_fat(&self) -> bool {
        self.file_system.is_some_and(FileSystem::is_fat)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn device(size: u64) -> DeviceInput {
        DeviceInput {
            size,
            sector_size: 512,
            partition_scheme: None,
            volume_label: None,
        }
    }

    fn defaults(size: u64) -> Vec<(FileSystem, u32)> {
        format_options(&device(size), None, &Choices::default())
            .file_systems
            .into_iter()
            .map(|e| (e.file_system, e.default_cluster_size))
            .collect()
    }

    #[test]
    fn cluster_defaults_follow_upstream_tables() {
        // 16 GB stick: FAT32 8 KB (the 256 MB-32 GB rule), NTFS 4 KB, exFAT 32 KB.
        let options = defaults(16 * GB);
        assert_eq!(
            options,
            [
                (FileSystem::Fat32, 8192),
                (FileSystem::Ntfs, 4096),
                (FileSystem::Udf, 0),
                (FileSystem::ExFat, 32768)
            ]
        );
        // 1 GB: FAT16 (32 KB), FAT32 4 KB.
        let options = defaults(GB);
        assert_eq!(options[0], (FileSystem::Fat16, 32768));
        assert_eq!(options[1], (FileSystem::Fat32, 4096));
        // 64 GB: Large FAT32 with 32 KB clusters, exFAT 128 KB.
        let options = format_options(&device(64 * GB), None, &Choices::default());
        assert!(options.large_fat32);
        assert_eq!(options.file_systems[0].label, "Large FAT32 (Default)");
        assert_eq!(options.file_systems[0].default_cluster_size, 32768);
        assert_eq!(options.file_systems[0].cluster_sizes, [16384, 32768, 65536]);
        assert_eq!(
            options
                .file_systems
                .iter()
                .find(|e| e.file_system == FileSystem::ExFat)
                .unwrap()
                .default_cluster_size,
            128 * 1024
        );
        // 3 TB: no FAT32, GPT forced.
        let options = format_options(&device(3 * TB), None, &Choices::default());
        assert_eq!(options.file_systems[0].file_system, FileSystem::Ntfs);
        assert_eq!(options.partition_scheme, Some(PartitionScheme::Gpt));
    }

    #[test]
    fn sector_size_removes_small_clusters() {
        let mut dev = device(GB);
        dev.sector_size = 4096;
        let options = format_options(&dev, None, &Choices::default());
        let fat32 = options
            .file_systems
            .iter()
            .find(|e| e.file_system == FileSystem::Fat32)
            .unwrap();
        assert!(fat32.cluster_sizes.iter().all(|&c| c >= 4096));
        assert_eq!(fat32.default_cluster_size, 4096);
    }

    #[test]
    fn non_bootable_defaults() {
        // A "16 GB" stick (16 × 10⁹ bytes; 16 GiB would show as "17 GB").
        let options = format_options(&device(16_000_000_000), None, &Choices::default());
        assert_eq!(options.boot_type, BootType::NonBootable);
        assert_eq!(
            options.partition_schemes,
            [
                PartitionScheme::Mbr,
                PartitionScheme::Gpt,
                PartitionScheme::Sfd
            ]
        );
        assert_eq!(options.partition_scheme, Some(PartitionScheme::Mbr));
        assert_eq!(options.target_systems, [TargetSystem::BiosOrUefi]);
        assert_eq!(options.file_system, Some(FileSystem::Fat32));
        assert_eq!(options.label, "16 GB");
    }

    #[test]
    fn freedos_is_fat_and_bios_only() {
        let choices = Choices {
            boot_type: Some(BootType::FreeDos),
            ..Choices::default()
        };
        let options = format_options(&device(8 * GB), None, &choices);
        assert_eq!(options.partition_schemes, [PartitionScheme::Mbr]);
        assert_eq!(options.target_systems, [TargetSystem::BiosOrCsm]);
        let fs: Vec<_> = options.file_systems.iter().map(|e| e.file_system).collect();
        // FAT16 only exists below 4 GB.
        assert_eq!(fs, [FileSystem::Fat32]);
        let small = format_options(&device(2 * GB), None, &choices);
        let fs: Vec<_> = small.file_systems.iter().map(|e| e.file_system).collect();
        assert_eq!(fs, [FileSystem::Fat16, FileSystem::Fat32]);
    }

    #[test]
    fn uefi_ntfs() {
        let choices = Choices {
            boot_type: Some(BootType::UefiNtfs),
            ..Choices::default()
        };
        let options = format_options(&device(8 * GB), None, &choices);
        assert_eq!(options.partition_scheme, Some(PartitionScheme::Gpt));
        assert_eq!(options.target_systems, [TargetSystem::Uefi]);
        let fs: Vec<_> = options.file_systems.iter().map(|e| e.file_system).collect();
        assert_eq!(fs, [FileSystem::Ntfs, FileSystem::ExFat]);
    }
}
