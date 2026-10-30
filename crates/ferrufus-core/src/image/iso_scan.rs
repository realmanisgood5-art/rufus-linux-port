// SPDX-License-Identifier: GPL-3.0-or-later
//! The ISO scan: what Rufus learns about an ISO before it decides how to
//! write it.
//!
//! Translated from Rufus `src/iso.c` at v4.15 (Copyright © 2011-2026 Pete
//! Batard, GPL-3.0-or-later): `check_iso_props()`, the scan mode of
//! `iso_extract_files()`, `udf_extract_files()` and `ExtractISO()`,
//! `GetGrubVersion()`, `GetGrubFs()`, `GetEfiBootInfo()` and
//! `HasEfiImgBootLoaders()`; `src/syslinux.c:GetSyslinuxVersion()`;
//! `src/parser.c:sanitize_label()` and `get_token_data_buffer()`; and the
//! ISO part of `src/rufus.c:ImageScanThread()`. Field names follow
//! `RUFUS_IMG_REPORT`.
//!
//! Not analyzed in this build: the Windows version inside `install.wim`
//! (`GetWimVersion()`, needs a WIM reader) and UEFI bootloader signatures
//! and revocations (`GetBootladerInfo()`); both are reported as such.

use super::fat;
use super::io::ByteSource;
use super::isofs::{Entry, Extensions, Iso, Volume, translate_name};
use crate::Result;
use serde::Serialize;

/// `efi_archname[]`, indexed by `ARCH_*`.
pub const EFI_ARCH_NAMES: [&str; 9] = [
    "",
    "ia32",
    "x64",
    "arm",
    "aa64",
    "ia64",
    "riscv64",
    "loongarch64",
    "ebc",
];
const EFI_BOOT_NAMES: [&str; 3] = ["boot", "grub", "mm"];
const SYSLINUX_CFG: [&str; 5] = [
    "isolinux.cfg",
    "syslinux.cfg",
    "extlinux.conf",
    "txt.cfg",
    "live.cfg",
];
const ISOLINUX_BIN: [&str; 2] = ["isolinux.bin", "boot.bin"];
const GRUB_DIRNAMES: [&str; 2] = ["/boot/grub/i386-pc", "/boot/grub2/i386-pc"];
const OLD_C32_NAMES: [&str; 2] = ["menu.c32", "vesamenu.c32"];
const OLD_C32_THRESHOLD: [u64; 2] = [53500, 148000];
const PE_DIRNAMES: [&str; 3] = ["/i386", "/amd64", "/minint"];
const PE_FILES: [&str; 3] = ["ntdetect.com", "setupldr.bin", "txtsetup.sif"];
const PE_BASEDIRS: [&str; 3] = ["i386", "amd64", "minint"];
const REACTOS_NAMES: [&str; 2] = ["setupldr.sys", "freeldr.sys"];
const WININST_NAMES: [&str; 3] = ["install.wim", "install.esd", "install.swm"];
const MD5SUM_NAMES: [&str; 2] = ["md5sum.txt", "MD5SUMS"];
/// `MAX_WININST`.
const MAX_WININST: usize = 4;
/// Entries in `efi_boot_entry[]`.
const MAX_EFI_BOOT_ENTRIES: usize = 64;
/// `WINPE_I386`, `WINPE_AMD64`, `WINPE_MININT`.
const WINPE_I386: u16 = 0x0007;
const WINPE_AMD64: u16 = 0x0023;
const WINPE_MININT: u16 = 0x01c0;
/// Syslinux versions embedded in Rufus 4.15 (`embedded_sl_version[]`,
/// from `res/syslinux/ldlinux_v4.sys` and `ldlinux_v6.sys`).
pub const EMBEDDED_SL_VERSION: [u16; 2] = [0x0407, 0x0604];
/// Directories followed below the root (a hostile image cannot recurse
/// without bound).
pub const MAX_DEPTH: usize = 64;
/// Entries scanned in total.
pub const MAX_ENTRIES: usize = 2_000_000;
/// Largest file read to detect a version.
const MAX_VERSION_FILE: u64 = 64 * 1024 * 1024;

/// Options of the scan (Rufus' Alt-J and Alt-K cheat modes).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScanOptions {
    /// `enable_joliet`.
    pub joliet: bool,
    /// `enable_rockridge`.
    pub rock_ridge: bool,
}

impl Default for ScanOptions {
    fn default() -> Self {
        Self {
            joliet: true,
            rock_ridge: true,
        }
    }
}

/// `efi_boot_entry_t.type`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum EfiBootType {
    /// `EBT_MAIN` (`boot<arch>.efi`).
    Main,
    /// `EBT_GRUB` (`grub<arch>.efi`).
    Grub,
    /// `EBT_MOKMANAGER` (`mm<arch>.efi`).
    MokManager,
    /// `EBT_BOOTMGR` (`/bootmgr.efi`).
    Bootmgr,
}

/// A UEFI boot loader found in the image.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EfiBootEntry {
    /// Type.
    pub kind: EfiBootType,
    /// Path in the image.
    pub path: String,
}

/// Which file system the scan used.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ScanFileSystem {
    /// UDF (tried first).
    Udf,
    /// ISO 9660.
    Iso9660,
}

/// What the ISO scan found. Field names follow `RUFUS_IMG_REPORT`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct IsoScan {
    /// File system the scan used.
    pub file_system: Option<ScanFileSystem>,
    /// `label`: UDF logical volume identifier or ISO volume identifier.
    pub label: String,
    /// `cfg_path`: the Syslinux/Isolinux configuration file to use.
    pub cfg_path: Option<String>,
    /// `reactos_path`.
    pub reactos_path: Option<String>,
    /// `wininst_path[]` (without upstream's `?:` prefix).
    pub wininst_paths: Vec<String>,
    /// `efi_boot_entry[]`.
    pub efi_boot_entries: Vec<EfiBootEntry>,
    /// `efi_img_path`.
    pub efi_img_path: Option<String>,
    /// `projected_size`: blocks of all files plus 1%.
    pub projected_size: u64,
    /// `mismatch_size`: ISO volume size minus file size.
    pub mismatch_size: i64,
    /// `has_efi` bits: 0 `bootmgr.efi`, 1 + `ARCH_*` for `boot<arch>.efi`,
    /// 0x4000 broken `bootx64.efi`, 0x8000 boot loaders in `efi.img`.
    pub has_efi: u16,
    /// `has_md5sum`: 1 `md5sum.txt`, 2 `MD5SUMS`.
    pub has_md5sum: u8,
    /// `has_symlinks`: 1 Rock Ridge, 2 UDF.
    pub has_symlinks: u8,
    /// `has_4GB_file`: low nibble counts files of 4 GB or more (up to 15);
    /// bits 4-6 flag `install.wim`/`.esd`/`.swm` of 4 GB or more.
    pub has_4gb_file: u8,
    /// `has_long_filename`: a Rock Ridge name longer than 64 characters.
    pub has_long_filename: bool,
    /// `has_deep_directories`: Rock Ridge deep directories.
    pub has_deep_directories: bool,
    /// `has_bootmgr`.
    pub has_bootmgr: bool,
    /// `has_bootmgr_efi`.
    pub has_bootmgr_efi: bool,
    /// `has_old_c32[]`: `menu.c32`, `vesamenu.c32` older than Syslinux 5.
    pub has_old_c32: [bool; 2],
    /// `has_efi_syslinux`.
    pub has_efi_syslinux: bool,
    /// `has_grub4dos`.
    pub has_grub4dos: bool,
    /// `has_grub2`: 1 or 2 for the `/boot/grub[2]/i386-pc` directory,
    /// 0x80 for GRUB EFI.
    pub has_grub2: u8,
    /// `has_grub2_fs` bits: FAT, exFAT, NTFS supported by EFI GRUB.
    pub has_grub2_fs: u8,
    /// `has_kolibrios`.
    pub has_kolibrios: bool,
    /// `has_panther_unattend`.
    pub has_panther_unattend: bool,
    /// `needs_syslinux_overwrite`.
    pub needs_syslinux_overwrite: bool,
    /// `needs_ntfs`.
    pub needs_ntfs: bool,
    /// `uses_casper`.
    pub uses_casper: bool,
    /// `uses_minint`.
    pub uses_minint: bool,
    /// `disable_iso`: ISO mode is not supported for this image.
    pub disable_iso: bool,
    /// `rh8_derivative`.
    pub rh8_derivative: bool,
    /// `winpe` bits.
    pub winpe: u16,
    /// `sl_version` (major << 8 | minor), 0 when there is no Syslinux.
    pub sl_version: u16,
    /// `sl_version_str`.
    pub sl_version_str: String,
    /// `sl_version_ext`.
    pub sl_version_ext: String,
    /// Whether `sl_version` is the embedded fallback (not found in the image).
    pub sl_version_embedded: bool,
    /// `grub2_version`.
    pub grub2_version: String,
    /// UEFI boot loader versions found (`GetEfiBootInfo()`).
    pub efi_boot_info: Vec<String>,
    /// Findings, as Rufus logs them.
    pub log: Vec<String>,
    /// What this build does not analyze.
    pub not_analyzed: Vec<String>,
}

impl IsoScan {
    /// `HAS_SYSLINUX()`.
    pub fn has_syslinux(&self) -> bool {
        self.sl_version != 0
    }
    /// `HAS_GRUB()`.
    pub fn has_grub(&self) -> bool {
        self.has_grub2 != 0 || self.has_grub4dos
    }
    /// `HAS_REACTOS()`.
    pub fn has_reactos(&self) -> bool {
        self.reactos_path.is_some()
    }
    /// `HAS_BOOTMGR()`.
    pub fn has_bootmgr(&self) -> bool {
        self.has_bootmgr || self.has_bootmgr_efi
    }
    /// `HAS_WINPE()`.
    pub fn has_winpe(&self) -> bool {
        self.winpe & WINPE_I386 == WINPE_I386
            || self.winpe & WINPE_AMD64 == WINPE_AMD64
            || self.winpe & WINPE_MININT == WINPE_MININT
    }
    /// `HAS_WININST()`.
    pub fn has_wininst(&self) -> bool {
        !self.wininst_paths.is_empty()
    }
    /// `HAS_WINDOWS()`.
    pub fn has_windows(&self) -> bool {
        self.has_bootmgr() || self.uses_minint || self.has_winpe()
    }
    /// `HAS_EFI_IMG()`.
    pub fn has_efi_img(&self) -> bool {
        self.efi_img_path.is_some()
    }
    /// `IS_EFI_BOOTABLE()`.
    pub fn is_efi_bootable(&self) -> bool {
        self.has_efi != 0
    }
    /// `IS_BIOS_BOOTABLE()`.
    pub fn is_bios_bootable(&self) -> bool {
        self.has_bootmgr()
            || self.has_syslinux()
            || self.has_winpe()
            || self.has_grub()
            || self.has_reactos()
            || self.has_kolibrios
    }
    /// `HAS_WINTOGO()`.
    pub fn has_wintogo(&self) -> bool {
        self.has_bootmgr() && self.is_efi_bootable() && self.has_wininst()
    }
    /// `HAS_PERSISTENCE()`.
    pub fn has_persistence(&self) -> bool {
        (self.has_syslinux() || self.has_grub())
            && !(self.has_windows() || self.has_reactos() || self.has_kolibrios)
    }
    /// `HAS_WIN7_EFI()`.
    pub fn has_win7_efi(&self) -> bool {
        self.has_efi == 1 && self.has_wininst()
    }
    /// `HAS_FATLESS_GRUB()`.
    pub fn has_fatless_grub(&self) -> bool {
        self.has_grub2 & 0x80 != 0 && self.has_grub2_fs & 0x1 == 0
    }
    /// `HAS_NTFSLESS_GRUB()`.
    pub fn has_ntfsless_grub(&self) -> bool {
        self.has_grub2 & 0x80 != 0 && self.has_grub2_fs & 0x4 == 0
    }
    /// `HAS_REGULAR_EFI()`.
    pub fn has_regular_efi(&self) -> bool {
        self.has_efi & 0x7ffe != 0
    }
}

fn eq(a: &str, b: &str) -> bool {
    a.eq_ignore_ascii_case(b)
}

fn starts_with_ignore_case(text: &str, prefix: &str) -> bool {
    text.len() >= prefix.len()
        && text
            .get(..prefix.len())
            .is_some_and(|head| head.eq_ignore_ascii_case(prefix))
}

fn ends_with_ignore_case(text: &str, suffix: &str) -> bool {
    text.len() >= suffix.len()
        && text
            .get(text.len().saturating_sub(suffix.len())..)
            .is_some_and(|tail| tail.eq_ignore_ascii_case(suffix))
}

/// Scan state that is not part of the report.
struct State<'a> {
    source: &'a dyn ByteSource,
    options: ScanOptions,
    scan: IsoScan,
    config_paths: Vec<String>,
    isolinux_paths: Vec<String>,
    grub_filesystems: Vec<String>,
    has_ldlinux_c32: bool,
    total_blocks: u64,
    entries: usize,
}

impl State<'_> {
    /// `check_iso_props()` in scan mode.
    fn check_props(&mut self, dirname: &str, length: u64, basename: &str, fullpath: &str) {
        let s = &mut self.scan;
        for (i, cfg) in SYSLINUX_CFG.iter().enumerate() {
            if eq(basename, cfg) {
                if i < 3 && !self.config_paths.iter().any(|p| p == fullpath) {
                    self.config_paths.push(fullpath.to_string());
                }
                if i == 1 && eq(dirname, "/efi/boot") {
                    s.has_efi_syslinux = true;
                }
            }
        }
        let mut is_old_c32 = [false; 2];
        for i in 0..2 {
            if OLD_C32_NAMES.get(i).is_some_and(|n| eq(basename, n))
                && OLD_C32_THRESHOLD.get(i).is_some_and(|&t| length <= t)
                && let Some(flag) = is_old_c32.get_mut(i)
            {
                *flag = true;
            }
        }
        for (i, dir) in GRUB_DIRNAMES.iter().enumerate() {
            if eq(dirname, dir) {
                s.has_grub2 = u8::try_from(i).unwrap_or(0).saturating_add(1);
            }
        }
        if eq(basename, "ldlinux.c32") {
            self.has_ldlinux_c32 = true;
        }
        if starts_with_ignore_case(dirname, "/casper") {
            s.uses_casper = true;
            if dirname.contains("pop-os") {
                s.disable_iso = true;
            }
        }
        if eq(dirname, "/proxmox") {
            s.disable_iso = true;
        }
        if dirname.is_empty() {
            if eq(basename, "bootmgr") {
                s.has_bootmgr = true;
            }
            if eq(basename, "bootmgr.efi") {
                if s.efi_boot_entries.len() < MAX_EFI_BOOT_ENTRIES {
                    s.efi_boot_entries.push(EfiBootEntry {
                        kind: EfiBootType::Bootmgr,
                        path: fullpath.to_string(),
                    });
                }
                s.has_efi |= 1;
                s.has_bootmgr_efi = true;
            }
            if eq(basename, "grldr") {
                s.has_grub4dos = true;
            }
            if eq(basename, "kolibri.img") {
                s.has_kolibrios = true;
            }
            if eq(basename, ".miso") {
                s.disable_iso = true;
            }
            for (i, name) in MD5SUM_NAMES.iter().enumerate() {
                if eq(basename, name) {
                    s.has_md5sum = u8::try_from(i).unwrap_or(0).saturating_add(1);
                }
            }
        }
        if s.reactos_path.is_none() && REACTOS_NAMES.iter().any(|n| eq(basename, n)) {
            s.reactos_path = Some(fullpath.to_string());
        }
        if s.efi_img_path.is_none()
            && basename.len() >= 7
            && starts_with_ignore_case(basename, "efi")
            && ends_with_ignore_case(basename, ".img")
        {
            s.efi_img_path = Some(fullpath.to_string());
        }
        if s.efi_img_path.is_none() && eq(fullpath, "/[BOOT]/0-Boot-HardDisk.img") {
            s.efi_img_path = Some(fullpath.to_string());
        }
        if starts_with_ignore_case(dirname, "/efi/") {
            for (k, boot) in EFI_BOOT_NAMES.iter().enumerate() {
                for (i, arch) in EFI_ARCH_NAMES.iter().enumerate() {
                    if eq(basename, &format!("{boot}{arch}.efi")) {
                        if k == 0 {
                            s.has_efi |=
                                2u16.checked_shl(u32::try_from(i).unwrap_or(0)).unwrap_or(0);
                        }
                        if s.efi_boot_entries.len() < MAX_EFI_BOOT_ENTRIES {
                            let kind = match k {
                                0 => EfiBootType::Main,
                                1 => EfiBootType::Grub,
                                _ => EfiBootType::MokManager,
                            };
                            s.efi_boot_entries.push(EfiBootEntry {
                                kind,
                                path: fullpath.to_string(),
                            });
                        }
                    }
                }
            }
            if eq(basename, "bootx64.efi") && length < 256 {
                s.has_efi |= 0x4000;
                s.efi_img_path = Some("[BOOT]/1-Boot-NoEmul.img".to_string());
            }
        }
        if ends_with_ignore_case(dirname, "/sources") {
            for (i, name) in WININST_NAMES.iter().enumerate() {
                if eq(basename, name) && s.wininst_paths.len() < MAX_WININST {
                    s.wininst_paths.push(fullpath.to_string());
                    if length >= 4 << 30 {
                        s.has_4gb_file |= 0x10u8 << i;
                    }
                }
            }
        }
        if eq(dirname, "/sources/$OEM$/$$/Panther") && eq(basename, "unattend.xml") {
            s.has_panther_unattend = true;
        }
        for (i, dir) in PE_DIRNAMES.iter().enumerate() {
            if eq(dirname, dir) {
                for (j, file) in PE_FILES.iter().enumerate() {
                    if eq(basename, file) {
                        let bit = (1u16 << j) << (3 * i);
                        s.winpe |= bit;
                    }
                }
            }
        }
        if ISOLINUX_BIN.iter().any(|n| eq(basename, n))
            && !self.isolinux_paths.iter().any(|p| p == fullpath)
        {
            self.isolinux_paths.push(fullpath.to_string());
        }
        for (flag, found) in s.has_old_c32.iter_mut().zip(is_old_c32) {
            *flag |= found;
        }
        if length >= 4 << 30 && s.has_4gb_file & 0x0f != 0x0f {
            s.has_4gb_file = s.has_4gb_file.saturating_add(1);
        }
        if length != 0 {
            self.total_blocks = self.total_blocks.saturating_add(length.div_ceil(2048));
        }
    }

    /// `iso_extract_files()` in scan mode: 0 done, 1 error, -1 stop the
    /// parent directory.
    fn walk_iso(
        &mut self,
        iso: &Iso<'_>,
        entries: Vec<Entry>,
        path: &str,
        depth: usize,
    ) -> Result<i8> {
        for entry in entries {
            self.entries = self.entries.saturating_add(1);
            if self.entries > MAX_ENTRIES {
                self.scan
                    .log
                    .push(format!("Stopped after {MAX_ENTRIES} entries"));
                return Ok(1);
            }
            if entry.rock && self.options.rock_ridge && entry.is_deep_dir_link() {
                if !self.scan.has_deep_directories {
                    self.scan
                        .log
                        .push("The ISO uses Rock Ridge deep directories".to_string());
                }
                self.scan.has_deep_directories = true;
                self.total_blocks = self.total_blocks.saturating_add(1);
                return Ok(-1);
            }
            if entry.name == "." || entry.name == ".." {
                continue;
            }
            let mut is_symlink = false;
            let basename = if entry.rock && self.options.rock_ridge {
                if entry.name.len() > 64 {
                    self.scan.has_long_filename = true;
                }
                is_symlink = entry.symlink.is_some();
                if is_symlink {
                    self.scan.has_symlinks = 1;
                }
                entry.name.clone()
            } else {
                translate_name(&entry.name, iso.joliet_level())
            };
            let fullpath = format!("{path}/{basename}");
            if entry.is_dir {
                if depth >= MAX_DEPTH {
                    self.scan
                        .log
                        .push(format!("Not scanning below {fullpath}: too deep"));
                    continue;
                }
                let Some(children) = iso.read_dir(&entry)? else {
                    self.scan
                        .log
                        .push(format!("Could not access directory {fullpath}"));
                    return Ok(1);
                };
                match self.walk_iso(iso, children, &fullpath, depth.saturating_add(1))? {
                    r if r > 0 => return Ok(r),
                    r if r < 0 => break,
                    _ => {}
                }
            } else {
                self.check_props(path, entry.size, &basename, &fullpath);
                if is_symlink && entry.size == 0 {
                    let target = entry.symlink.as_deref().unwrap_or_default();
                    if path == "/firmware" {
                        if let Some(target) = iso.stat(&format!("{path}/{target}"))? {
                            self.total_blocks =
                                self.total_blocks.saturating_add(target.size.div_ceil(2048));
                        }
                    } else if entry.name == "live" && target == "casper" {
                        self.scan.needs_ntfs = true;
                    }
                }
            }
        }
        Ok(0)
    }

    /// `udf_extract_files()` in scan mode.
    fn walk_udf(
        &mut self,
        volume: &Volume<'_>,
        entries: Vec<Entry>,
        path: &str,
        depth: usize,
    ) -> Result<i8> {
        for entry in entries {
            self.entries = self.entries.saturating_add(1);
            if self.entries > MAX_ENTRIES {
                self.scan
                    .log
                    .push(format!("Stopped after {MAX_ENTRIES} entries"));
                return Ok(1);
            }
            if entry.name.is_empty() {
                continue;
            }
            let fullpath = format!("{path}/{}", entry.name);
            if entry.symlink.is_some() {
                self.scan.has_symlinks = 2;
            }
            if entry.is_dir {
                if depth >= MAX_DEPTH {
                    self.scan
                        .log
                        .push(format!("Not scanning below {fullpath}: too deep"));
                    continue;
                }
                if let Some(children) = volume.read_dir(&entry)?
                    && self.walk_udf(volume, children, &fullpath, depth.saturating_add(1))? != 0
                {
                    return Ok(1);
                }
            } else {
                self.check_props(path, entry.size, &entry.name, &fullpath);
            }
        }
        Ok(0)
    }

    fn read_file(&self, path: &str) -> Result<Option<Vec<u8>>> {
        Volume::read_file(self.source, path, MAX_VERSION_FILE)
    }
}

/// Scans an ISO image. Returns `None` when `ExtractISO()` would fail (the
/// image is then not treated as an ISO).
pub fn scan(source: &dyn ByteSource, options: ScanOptions) -> Result<Option<IsoScan>> {
    let extensions = Extensions::for_scan(options.joliet, options.rock_ridge);
    let Some(volume) = Volume::open(source, extensions)? else {
        return Ok(None);
    };
    let mut state = State {
        source,
        options,
        scan: IsoScan::default(),
        config_paths: Vec::new(),
        isolinux_paths: Vec::new(),
        grub_filesystems: Vec::new(),
        has_ldlinux_c32: false,
        total_blocks: 0,
        entries: 0,
    };
    // For UDF, the image is also opened as plain ISO 9660 for size checks.
    let iso_view = match &volume {
        Volume::Udf(_) => Iso::open(
            source,
            Extensions {
                joliet: false,
                rock_ridge: false,
            },
        )?,
        Volume::Iso(_) => None,
    };
    let r = match &volume {
        Volume::Udf(udf) => {
            state.scan.file_system = Some(ScanFileSystem::Udf);
            state.scan.label = super::udf::parse(source)?
                .and_then(|u| u.logical_volume_id)
                .unwrap_or_default();
            match udf.read_root()? {
                Some(root) => state.walk_udf(&volume, root, "", 0)?,
                None => 1,
            }
        }
        Volume::Iso(iso) => {
            state.scan.file_system = Some(ScanFileSystem::Iso9660);
            state.scan.label = iso_label(source, iso.joliet_level())?;
            match iso.read_root()? {
                Some(root) => state.walk_iso(iso, root, "", 0)?,
                None => 1,
            }
        }
    };
    if r != 0 {
        return Ok(None);
    }
    finish(
        &mut state,
        iso_view.as_ref().or(match &volume {
            Volume::Iso(iso) => Some(iso),
            Volume::Udf(_) => None,
        }),
    )?;
    Ok(Some(state.scan))
}

/// `iso9660_ifs_get_volume_id()`: the PVD identifier, or the Joliet one
/// when Joliet is in use and differs (see `iso9660::parse`).
fn iso_label(source: &dyn ByteSource, joliet_level: u8) -> Result<String> {
    let report = super::iso9660::parse(source)?;
    Ok(match report {
        Some(report) if joliet_level > 0 => report.label.unwrap_or_default(),
        Some(report) => report.pvd_volume_id,
        None => String::new(),
    })
}

/// The post-scan part of `ExtractISO()` and the ISO part of
/// `ImageScanThread()`.
fn finish(state: &mut State<'_>, iso: Option<&Iso<'_>>) -> Result<()> {
    let source = state.source;
    let size = source.size();
    if let Some(report) = super::iso9660::parse(source)?
        && iso.is_some()
    {
        let volume = i128::from(report.volume_space_blocks).saturating_mul(2048);
        state.scan.mismatch_size =
            i64::try_from(volume.saturating_sub(i128::from(size))).unwrap_or(i64::MAX);
    }
    // Trailing spaces were already removed from the label.
    state.scan.label = state.scan.label.trim_end().to_string();
    state.scan.projected_size = projected_size(state.total_blocks);

    if !state.config_paths.is_empty() {
        let mut chosen: Option<String> = None;
        for path in &state.config_paths {
            if eq(path, "/boot/i386/loader/isolinux.cfg")
                || eq(path, "/boot/x86_64/loader/isolinux.cfg")
            {
                chosen = Some(path.clone());
                state.scan.needs_syslinux_overwrite = true;
                break;
            }
            if chosen.as_ref().is_none_or(|c| c.len() >= path.len()) {
                chosen = Some(path.clone());
            }
        }
        state.scan.cfg_path = chosen;
        if let Some(cfg) = &state.scan.cfg_path {
            state
                .scan
                .log
                .push(format!("Will use '{cfg}' for Syslinux"));
        }
        let mut sl_index = 0usize;
        for (i, path) in state.isolinux_paths.clone().iter().enumerate() {
            let Some(buf) = state.read_file(path)? else {
                state.scan.log.push(format!("Could not access {path}"));
                continue;
            };
            let (version, ext) = syslinux_version(&buf);
            if state.scan.sl_version == 0 {
                state.scan.sl_version = version;
                state.scan.sl_version_ext = ext;
                sl_index = i;
            } else if state.scan.sl_version != version || state.scan.sl_version_ext != ext {
                state.scan.log.push(format!(
                    "Found conflicting Isolinux versions in '{}' and '{path}'",
                    state
                        .isolinux_paths
                        .get(sl_index)
                        .cloned()
                        .unwrap_or_default()
                ));
                // Prefer the one in the same directory as the configuration file.
                let cfg = state.scan.cfg_path.clone().unwrap_or_default();
                let dir_len = cfg.rfind('/').unwrap_or(0);
                if dir_len > 0
                    && path.len() >= dir_len
                    && path.get(..dir_len).is_some_and(|p| {
                        p.eq_ignore_ascii_case(cfg.get(..dir_len).unwrap_or_default())
                    })
                {
                    state.scan.sl_version = version;
                    state.scan.sl_version_ext = ext;
                    sl_index = i;
                }
            }
        }
        if state.scan.sl_version != 0 {
            let v = state.scan.sl_version;
            state.scan.sl_version_str = format!("{}.{:02}", v >> 8, v & 0xff);
            if (state.has_ldlinux_c32 && v >> 8 < 5) || (!state.has_ldlinux_c32 && v >> 8 >= 5) {
                state.scan.log.push(
                    "Conflict between Isolinux version and the presence of ldlinux.c32".to_string(),
                );
            }
        } else {
            let v = EMBEDDED_SL_VERSION
                .get(usize::from(state.has_ldlinux_c32))
                .copied()
                .unwrap_or(0);
            state.scan.sl_version = v;
            state.scan.sl_version_str = format!("{}.{:02}", v >> 8, v & 0xff);
            state.scan.sl_version_embedded = true;
            state.scan.log.push(format!(
                "Could not detect Isolinux version - Forcing to {} (embedded)",
                state.scan.sl_version_str
            ));
        }
    }

    if !state.scan.is_efi_bootable()
        && let (Some(path), Some(iso)) = (state.scan.efi_img_path.clone(), iso)
        && let Some(entry) = iso.stat(&path)?
        && let Some(offset) = iso_entry_offset(iso, &entry)
    {
        let arches = fat::efi_img_boot_loaders(source, offset, &EFI_ARCH_NAMES)?;
        if !arches.is_empty() {
            state.scan.has_efi = 0x8000;
            for index in arches {
                state.scan.log.push(format!(
                    "Detected EFI bootloader 'boot{}.efi' (from '{path}')",
                    EFI_ARCH_NAMES.get(index).copied().unwrap_or_default()
                ));
            }
        }
    }

    if state.scan.has_winpe() {
        let w = state.scan.winpe;
        let base = if w & WINPE_I386 == WINPE_I386 {
            0
        } else if w & WINPE_AMD64 == WINPE_AMD64 {
            1
        } else {
            2
        };
        let path = format!(
            "/{}/txtsetup.sif",
            PE_BASEDIRS.get(base).copied().unwrap_or_default()
        );
        if let Some(buf) = state.read_file(&path)?
            && let Some(options) = token_data(&buf, "OsLoadOptions")
        {
            let options = options.to_lowercase();
            state
                .scan
                .log
                .push(format!("Checking txtsetup.sif: OsLoadOptions = {options}"));
            state.scan.uses_minint = options.contains("/minint");
        }
    }
    if state.scan.has_wininst() {
        state
            .scan
            .not_analyzed
            .push("Windows version of the install image (needs a WIM reader)".to_string());
    }

    if state.scan.has_grub2 != 0 {
        let dir = GRUB_DIRNAMES
            .get(usize::from(state.scan.has_grub2.saturating_sub(1)))
            .copied()
            .unwrap_or_default();
        let path = format!("{dir}/normal.mod");
        state.scan.grub2_version.clear();
        match state.read_file(&path)? {
            Some(buf) => grub_version(state, &buf, &path),
            None => state
                .scan
                .log
                .push(format!("Could not read Grub version from '{path}'")),
        }
        if state.scan.grub2_version.is_empty() {
            state
                .scan
                .log
                .push("Could not detect Grub version".to_string());
            state.scan.has_grub2 &= 0x80;
        }
    }
    for entry in state.scan.efi_boot_entries.clone() {
        match entry.kind {
            EfiBootType::Grub => match state.read_file(&entry.path)? {
                Some(buf) => {
                    state.scan.has_grub2 |= 0x80;
                    grub_version(state, &buf, &entry.path);
                    grub_filesystems(state, &buf);
                }
                None => state
                    .scan
                    .log
                    .push(format!("Could not read Grub version from '{}'", entry.path)),
            },
            EfiBootType::Main => match state.read_file(&entry.path)? {
                Some(buf) => {
                    if let Some(info) = efi_boot_info(&buf) {
                        state
                            .scan
                            .efi_boot_info
                            .push(format!("{info} (from '{}')", entry.path));
                    }
                }
                None => state
                    .scan
                    .log
                    .push(format!("Could not parse '{}'", entry.path)),
            },
            _ => {}
        }
    }
    for fs in &state.grub_filesystems {
        for (j, name) in ["fat", "exfat", "ntfs"].iter().enumerate() {
            if fs.eq_ignore_ascii_case(name) {
                state.scan.has_grub2_fs |= 1 << j;
            }
        }
    }
    if state.scan.is_efi_bootable() {
        state
            .scan
            .not_analyzed
            .push("UEFI bootloader signatures and revocations".to_string());
    }

    // ImageScanThread()
    state.scan.rh8_derivative = is_rh8_derivative(&state.scan.label);
    if ["Install-SUSE", "Install-LEAP", "openSUSE-Tumbleweed"]
        .iter()
        .any(|p| state.scan.label.starts_with(p))
    {
        state.scan.disable_iso = true;
    }
    if state.scan.has_grub2 & 0x80 != 0
        && (state.scan.has_grub2_fs & 0x7 == 0
            || (state.scan.has_4gb_file != 0 && state.scan.has_grub2_fs & 0x6 == 0))
    {
        state.scan.disable_iso = true;
    }
    Ok(())
}

/// Byte offset of a file's data, for reading a FAT image in place.
fn iso_entry_offset(iso: &Iso<'_>, entry: &Entry) -> Option<u64> {
    iso.data_offset(entry)
}

/// `(double)total_blocks * ISO_BLOCKSIZE * 1.01f`, truncated.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss
)] // Same float arithmetic as upstream.
fn projected_size(total_blocks: u64) -> u64 {
    (total_blocks as f64 * 2048.0 * f64::from(1.01f32)) as u64
}

/// `GetSyslinuxVersion()`: version (major << 8 | minor) and the extra
/// version string.
pub fn syslinux_version(buf: &[u8]) -> (u16, String) {
    if buf.len() < 256 {
        return (0, String::new());
    }
    let mut i = 64usize;
    while i < buf.len().saturating_sub(64) {
        let rest = buf.get(i..).unwrap_or_default();
        if !rest.starts_with(b"LINUX ") {
            i = i.saturating_add(1);
            continue;
        }
        let prefix = buf.get(i.saturating_sub(3)..i).unwrap_or_default();
        if prefix != b"ISO" && prefix != b"SYS" {
            i = i.saturating_add(1);
            continue;
        }
        let start = i.saturating_add(6);
        let text = buf.get(start..).unwrap_or_default();
        // The buffer is NUL-terminated after the file, as upstream reads it.
        let nul = text.iter().position(|&b| b == 0).unwrap_or(text.len());
        let text = text.get(..nul).unwrap_or_default();
        let (major, after_major) = strtoul(text);
        let mut version = 0u16;
        let mut p = after_major;
        if major < 256 {
            let (minor, after_minor) = strtoul(
                text.get(after_major.saturating_add(1)..)
                    .unwrap_or_default(),
            );
            p = after_major.saturating_add(1).saturating_add(after_minor);
            if minor < 256 {
                version = u16::try_from((major << 8).saturating_add(minor)).unwrap_or(0);
            }
        }
        if version == 0 {
            i = i.saturating_add(1);
            continue;
        }
        // Extra version string: p points after the minor version.
        let mut ext: Vec<u8> = text.get(p..).unwrap_or_default().to_vec();
        if let Some(first) = ext.first_mut() {
            *first = b'/';
        } else {
            ext.push(b'/');
        }
        // Remove the x.yz- duplicate if present.
        let mut j = 0usize;
        while text.get(j).is_some()
            && text.get(j) == ext.get(1usize.saturating_add(j))
            && text.get(j) != Some(&b' ')
        {
            j = j.saturating_add(1);
        }
        if ext.get(j.saturating_add(1)) == Some(&b'-') {
            j = j.saturating_add(1);
        }
        if j >= 4 {
            if let Some(slot) = ext.get_mut(j) {
                *slot = b'/';
            }
            ext = ext.get(j..).unwrap_or_default().to_vec();
        }
        while ext.len() > 1 && matches!(ext.last(), Some(b' ' | b'*')) {
            ext.pop();
        }
        for c in ext.iter_mut().skip(1) {
            if b"<>:|*?\\/".contains(c) {
                *c = b'_';
            }
        }
        let ext = if ext.len() <= 1 {
            String::new()
        } else {
            String::from_utf8_lossy(&ext).into_owned()
        };
        return (version, ext);
    }
    (0, String::new())
}

/// `strtoul(s, &end, 10)` for the digits at the start of `s`: the value
/// (saturated) and the number of bytes consumed (leading spaces included).
fn strtoul(s: &[u8]) -> (u64, usize) {
    let mut i = 0usize;
    while s.get(i).is_some_and(|c| c.is_ascii_whitespace()) {
        i = i.saturating_add(1);
    }
    let digits_start = i;
    let mut value = 0u64;
    while let Some(c) = s.get(i).filter(|c| c.is_ascii_digit()) {
        value = value.saturating_mul(10).saturating_add(u64::from(c - b'0'));
        i = i.saturating_add(1);
    }
    if i == digits_start {
        (0, 0)
    } else {
        (value, i)
    }
}

/// `GetGrubVersion()`.
fn grub_version(state: &mut State<'_>, buf: &[u8], source: &str) {
    const PATTERNS: [&[u8]; 2] = [b"GRUB  version", b"GRUB version"];
    let mut version: Vec<u8> = Vec::new();
    let mut has_debug = false;
    if buf.len() > 32 {
        for i in 0..buf.len().saturating_sub(32) {
            let rest = buf.get(i..).unwrap_or_default();
            for pattern in PATTERNS {
                if rest.starts_with(pattern) {
                    // "%s" in the pattern is the NUL after the text.
                    let mut at = i.saturating_add(pattern.len()).saturating_add(1);
                    if buf.get(at) == Some(&0) {
                        at = at.saturating_add(1);
                    }
                    let tail = buf.get(at..).unwrap_or_default();
                    let end = tail
                        .iter()
                        .position(|&b| b == 0)
                        .unwrap_or(tail.len())
                        .min(191);
                    version = tail.get(..end).unwrap_or_default().to_vec();
                }
            }
            if rest.starts_with(b"grub_debug_is_enabled") {
                has_debug = true;
            }
        }
    }
    let version = String::from_utf8_lossy(&version).into_owned();
    state.scan.log.push(format!(
        "Detected GRUB version: {version} (from '{source}')"
    ));
    if !state.scan.grub2_version.is_empty() {
        return;
    }
    let mut version = if version.starts_with('0') {
        String::new()
    } else {
        version
    };
    if !version.is_empty() {
        let append_label = version == "2.06";
        if state.scan.has_grub2 & 0x7f > 1 {
            version.push_str("-nonstandard");
        }
        if has_debug {
            version.push_str("-gdie");
        }
        if append_label {
            version.push('-');
            version.push_str(&state.scan.label);
        }
        version = sanitize_label(&version);
    }
    state.scan.grub2_version = version;
}

/// `GetGrubFs()`: file system names after an `fshelp` string.
fn grub_filesystems(state: &mut State<'_>, buf: &[u8]) {
    if buf.len() <= 32 {
        return;
    }
    for i in 0..buf.len().saturating_sub(32) {
        let rest = buf.get(i..).unwrap_or_default();
        if rest.starts_with(b"fshelp\0") {
            let tail = rest.get(7..).unwrap_or_default();
            let end = tail.iter().position(|&b| b == 0).unwrap_or(tail.len());
            if end > 0 && end < 12 {
                let name =
                    String::from_utf8_lossy(tail.get(..end).unwrap_or_default()).into_owned();
                if !state
                    .grub_filesystems
                    .iter()
                    .any(|f| f.eq_ignore_ascii_case(&name))
                {
                    state.grub_filesystems.push(name);
                }
            }
        }
    }
}

/// `GetEfiBootInfo()`: the Shim or systemd-boot version.
fn efi_boot_info(buf: &[u8]) -> Option<String> {
    const INFO: [(&str, &[u8]); 2] = [
        ("Shim", b"UEFI SHIM\n$Version: "),
        ("systemd-boot", b"#### LoaderInfo: systemd-boot "),
    ];
    if buf.len() <= 64 {
        return None;
    }
    for i in 0..buf.len().saturating_sub(64) {
        let rest = buf.get(i..).unwrap_or_default();
        for (label, pattern) in INFO {
            if rest.starts_with(pattern) {
                let tail = rest.get(pattern.len()..).unwrap_or_default();
                let end = tail
                    .iter()
                    .take(32)
                    .position(|c| c.is_ascii_whitespace() || *c == 0)
                    .unwrap_or(tail.len().min(32));
                let version =
                    String::from_utf8_lossy(tail.get(..end).unwrap_or_default()).into_owned();
                return Some(format!("{label} {version}"));
            }
        }
    }
    None
}

/// `sanitize_label()`.
pub fn sanitize_label(label: &str) -> String {
    const REMOVE: [&str; 44] = [
        "-i386",
        "-i686",
        "-amd64",
        "-x86-64",
        ".x86-64",
        "-x64",
        "-armhf",
        "-arm64",
        "-aarch64",
        "-32-bit",
        "-64-bit",
        "-32bit",
        "-64bit",
        "-intel",
        "-cd",
        "-dvd",
        "-standard",
        "-live",
        "-install",
        "-server",
        "-net",
        "-desktop",
        "-lts",
        "-studio",
        "-baseos",
        "-kde",
        "-xfce",
        "-lxde",
        "-gnome",
        "-mate",
        "-unstable",
        "-debug",
        "-release",
        "-final",
        "-stream",
        "-cinnamon",
        "-cinn",
        "-leap",
        "-tumbleweed",
        "-budgie",
        "-ws",
        "-iot",
        "-ostree",
        ".iso",
    ];
    let mut s: Vec<u8> = label
        .bytes()
        .map(|c| {
            let c = c.to_ascii_lowercase();
            if (c < b'0' && c != b'.') || (c > b'9' && c < b'a') || c > b'z' {
                b'-'
            } else {
                c
            }
        })
        .collect();
    while s.first() == Some(&b'-') {
        s.remove(0);
    }
    if s.len() <= 1 {
        return String::from_utf8_lossy(&s).into_owned();
    }
    while s.len() > 1 && s.last() == Some(&b'-') {
        s.pop();
    }
    if s.len() <= 1 {
        return String::from_utf8_lossy(&s).into_owned();
    }
    // Remove duplicate '-' (upstream stops two characters before the end).
    let mut i = 0usize;
    while s.len() >= 2 && i < s.len().saturating_sub(2) {
        if s.get(i) == Some(&b'-') && s.get(i.saturating_add(1)) == Some(&b'-') {
            s.remove(i.saturating_add(1));
        } else {
            i = i.saturating_add(1);
        }
    }
    let mut text = String::from_utf8_lossy(&s).into_owned();
    for pattern in REMOVE {
        if let Some(at) = text.find(pattern) {
            text.replace_range(at..at.saturating_add(pattern.len()), "");
        }
    }
    text
}

/// `get_token_data_buffer(token, 1, ...)` for INI-style `token = value`.
fn token_data(buf: &[u8], token: &str) -> Option<String> {
    if buf.len() <= 4 || buf.len() > 65536 {
        return None;
    }
    let text = String::from_utf8_lossy(buf);
    for line in text.split(['\n', '\r']) {
        let rest = line.trim_start_matches([' ', '\t']);
        let rest = rest
            .strip_prefix('<')
            .unwrap_or(rest)
            .trim_start_matches([' ', '\t']);
        if !starts_with_ignore_case(rest, token) {
            continue;
        }
        let rest = rest
            .get(token.len()..)
            .unwrap_or_default()
            .trim_start_matches([' ', '\t']);
        let (xml, rest) = match rest.chars().next() {
            Some('>') => (true, rest.get(1..).unwrap_or_default()),
            Some('=') => (false, rest.get(1..).unwrap_or_default()),
            _ => continue,
        };
        let rest = rest.trim_start_matches([' ', '\t']);
        let (quoted, rest) = match rest.strip_prefix('"') {
            Some(r) => (true, r),
            None => (false, rest),
        };
        let end = rest
            .find(|c| (c == '"' && quoted) || (c == '<' && xml))
            .unwrap_or(rest.len());
        let value = rest.get(..end).unwrap_or_default();
        if !value.is_empty() {
            return Some(value.to_string());
        }
    }
    None
}

/// The `redhat8_derivative[]` patterns of `ImageScanThread()`.
fn is_rh8_derivative(label: &str) -> bool {
    let digit_after = |text: &str, prefix: &str, range: std::ops::RangeInclusive<u8>| {
        text.strip_prefix(prefix)
            .and_then(|r| r.bytes().next())
            .is_some_and(|c| range.contains(&c))
    };
    // "^<prefix>.*-<digits>": a '-' followed by a matching digit anywhere after the prefix.
    let dash_digit_anywhere = |prefix: &str, first: u8, range: std::ops::RangeInclusive<u8>| {
        label.strip_prefix(prefix).is_some_and(|rest| {
            rest.as_bytes().windows(2).enumerate().any(|(i, w)| {
                w.first() == Some(&b'-')
                    && if first == 0 {
                        w.get(1).is_some_and(|c| range.contains(c))
                    } else {
                        w.get(1) == Some(&first)
                            && rest
                                .as_bytes()
                                .get(i.saturating_add(2))
                                .is_some_and(|c| range.contains(c))
                    }
            })
        })
    };
    digit_after(label, "AlmaLinux-", b'8'..=b'9')
        || dash_digit_anywhere("Fedora", b'3', b'3'..=b'9')
        || dash_digit_anywhere("CentOS", 0, b'8'..=b'9')
        || dash_digit_anywhere("Circle", 0, b'8'..=b'9')
        || digit_after(label, "OL-", b'8'..=b'9')
        || digit_after(label, "RHEL-", b'8'..=b'9')
        || digit_after(label, "Rocky-", b'8'..=b'9')
        || digit_after(label, "MIRACLE-LINUX-", b'8'..=b'9')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn syslinux_versions() {
        let mut buf = vec![0u8; 512];
        let text = b"ISOLINUX 6.04 6.04-pre1* ";
        buf[200..200 + text.len()].copy_from_slice(text);
        assert_eq!(syslinux_version(&buf), (0x0604, "/pre1".to_string()));
        let mut buf = vec![0u8; 512];
        let text = b"SYSLINUX 4.07 2013-07-25";
        buf[200..200 + text.len()].copy_from_slice(text);
        assert_eq!(syslinux_version(&buf), (0x0407, "/2013-07-25".to_string()));
        let mut buf = vec![0u8; 512];
        let text = b"ISOLINUX 3.86 ";
        buf[200..200 + text.len()].copy_from_slice(text);
        assert_eq!(syslinux_version(&buf), (0x0356, String::new()));
        // Too early in the file, or no ISO/SYS prefix.
        let mut buf = vec![0u8; 512];
        buf[10..24].copy_from_slice(b"ISOLINUX 6.04 ");
        buf[300..313].copy_from_slice(b"XYZLINUX 6.04");
        assert_eq!(syslinux_version(&buf).0, 0);
    }

    #[test]
    fn labels_are_sanitized_like_upstream() {
        assert_eq!(
            sanitize_label("2.06-Ubuntu 22.04 LTS amd64"),
            "2.06-ubuntu-22.04"
        );
        assert_eq!(sanitize_label("--A--B--"), "a-b");
        assert_eq!(sanitize_label("Fedora-WS-Live-39-1-5"), "fedora-39-1-5");
    }

    #[test]
    fn tokens_are_read_like_get_token_data_buffer() {
        let sif = b"[SetupData]\r\nOsLoadOptions = \"/fastdetect /minint\"\r\nOther=1\r\n";
        assert_eq!(
            token_data(sif, "OsLoadOptions").as_deref(),
            Some("/fastdetect /minint")
        );
        assert_eq!(
            token_data(b"  osloadoptions=/noguiboot\n", "OsLoadOptions").as_deref(),
            Some("/noguiboot")
        );
        assert_eq!(token_data(b"Other=1\n", "OsLoadOptions"), None);
    }

    #[test]
    fn red_hat_8_derivatives() {
        for label in [
            "AlmaLinux-9-2-x86_64-dvd",
            "Fedora-WS-Live-38-1-6",
            "CentOS-Stream-9-BaseOS-x86_64",
            "RHEL-8-8-0-BaseOS-x86_64",
            "Rocky-9-3-x86_64-dvd",
            "OL-8-9-0-BaseOS-x86_64",
        ] {
            assert!(is_rh8_derivative(label), "{label}");
        }
        for label in [
            "Fedora-WS-Live-40-1-14",
            "AlmaLinux-10-0",
            "CentOS-7-x86_64",
            "Ubuntu 22.04",
            "RHEL-7.9",
        ] {
            assert!(!is_rh8_derivative(label), "{label}");
        }
    }

    #[test]
    fn efi_boot_info_versions() {
        let mut buf = vec![0u8; 200];
        let text = b"UEFI SHIM\n$Version: 15.8 $";
        buf[20..20 + text.len()].copy_from_slice(text);
        assert_eq!(efi_boot_info(&buf).as_deref(), Some("Shim 15.8"));
    }
}
