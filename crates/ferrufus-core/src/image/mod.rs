// SPDX-License-Identifier: GPL-3.0-or-later
//! Image inspection.
//!
//! Reproduces how Rufus v4.15 analyzes a selected image in
//! `src/rufus.c:ImageScanThread()`: the ISO/UDF scan of
//! `src/iso.c:ExtractISO()` (label only in this milestone), then
//! `src/vhd.c:IsBootableImage()` and `IsCompressedBootableImage()` for the
//! boot marker, container type, sizes and WIM detection (Copyright ©
//! 2011-2026 Pete Batard, GPL-3.0-or-later).
//!
//! All input is untrusted: every read is bounded and malformed structures
//! are reported as "not recognized", never as errors or panics. Only I/O
//! failures are errors.

pub mod compression;
pub mod gpt;
pub mod io;
pub mod iso9660;
pub mod isofs;
pub mod mbr;
pub mod ms_sys;
mod ms_sys_tables;
pub mod udf;
pub mod vhd;

use crate::upstream::UpstreamRef;
use crate::{Error, Result};
use compression::{Association, MagicCheck};
use io::{ByteSource, FileSource, read_exact_at};
use serde::Serialize;
use std::fs::File;
use std::path::Path;

/// `WIM_MAGIC` ("MSWIM\0\0\0").
const WIM_MAGIC: &[u8; 8] = b"MSWIM\0\0\0";

/// Options that change how an image is classified.
#[derive(Debug, Clone, Copy, Default)]
pub struct InspectOptions {
    /// Treat an image without a boot marker as bootable (Rufus Alt-M,
    /// `SETTING_IGNORE_BOOT_MARKER`). The result is reported as "forced".
    pub ignore_boot_marker: bool,
}

/// `img_report.is_bootable_img`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Bootability {
    /// 0: no boot marker.
    NotBootable,
    /// 1: boot marker present (or, for compressed images, forced: upstream
    /// collapses that case to 1).
    Bootable,
    /// 2: no boot marker, but `ignore_boot_marker` is set.
    Forced,
    /// The boot marker could not be checked in this build (compressed and
    /// VHDX images, which need decompression or a VHDX reader).
    Unknown,
}

impl Bootability {
    /// `IS_DD_BOOTABLE()`: `is_bootable_img > 0`.
    pub fn is_dd_bootable(self) -> bool {
        matches!(self, Self::Bootable | Self::Forced)
    }
}

/// How the image can be written, following `ImageScanThread()` and
/// `BootCheckThread()`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Classification {
    /// "Unsupported image" (`MSG_081`): not an ISO, not DD-bootable and not
    /// a Windows image.
    Unsupported,
    /// A disk image written as-is (DD mode only).
    DiskImage,
    /// An ISO without a boot marker: ISO (file copy) mode only.
    Iso,
    /// An ISOHybrid: ISO mode (recommended upstream) or DD mode.
    IsoHybrid,
    /// A Windows WIM image (`is_windows_img`).
    WindowsImage,
    /// The type is recognized but cannot be analyzed in this build.
    NotYetSupported,
}

/// The analysis of an image file. Field names follow upstream
/// `RUFUS_IMG_REPORT` where they have the same meaning.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ImageReport {
    /// The file that was inspected.
    pub path: String,
    /// `image_size`: size of the file in bytes.
    pub image_size: u64,
    /// `projected_size`: size of the data the image expands to, when known
    /// (virtual disk size for VHDs and VTSI, the file size for raw images).
    pub projected_size: Option<u64>,
    /// Container type chosen from the extension (`compression_type`).
    pub association: Option<Association>,
    /// Whether the file's magic bytes agree with `association`.
    pub magic: Option<MagicCheck>,
    /// `is_iso`: the image opens as UDF (with a root directory) or ISO 9660.
    pub is_iso: bool,
    /// `is_bootable_img`.
    pub is_bootable_img: Bootability,
    /// `is_vhd`.
    pub is_vhd: bool,
    /// `is_windows_img`: the file starts with the WIM magic.
    pub is_windows_img: bool,
    /// `label`: UDF logical volume identifier or ISO 9660 volume identifier.
    pub label: Option<String>,
    /// The first sector of the (virtual) disk.
    pub mbr: Option<mbr::MbrReport>,
    /// The GUID partition table, when present.
    pub gpt: Option<gpt::GptReport>,
    /// ISO 9660 descriptors, when present.
    pub iso9660: Option<iso9660::Iso9660Report>,
    /// UDF recognition, when an anchor is present.
    pub udf: Option<udf::UdfReport>,
    /// VHD footer, for `.vhd` files.
    pub vhd: Option<vhd::VhdReport>,
    /// Ventoy sparse image footer, for `.vtsi` files.
    pub vtsi: Option<vhd::VtsiReport>,
    /// How the image can be written.
    pub classification: Classification,
    /// Findings in the words Rufus logs them, with their upstream source.
    pub notes: Vec<Note>,
    /// Problems with the image.
    pub warnings: Vec<String>,
}

/// A finding, tied to the upstream code that produces the same message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Note {
    /// The finding.
    pub message: String,
    /// Where upstream makes the same determination.
    pub upstream: UpstreamRef,
}

impl ImageReport {
    /// The size of the data a DD write would put on the device, when known.
    pub fn dd_payload_size(&self) -> Option<u64> {
        match (self.association, &self.vhd) {
            (None, _) => Some(self.image_size),
            // A fixed VHD is the raw disk followed by a 512-byte footer.
            (Some(Association::Vhd), Some(v)) if v.disk_type == vhd::VhdDiskType::Fixed => {
                Some(v.current_size)
            }
            _ => self.projected_size,
        }
    }
}

/// Inspects the image file at `path`. Only regular files are accepted.
pub fn inspect(path: &Path, options: &InspectOptions) -> Result<ImageReport> {
    let file =
        File::open(path).map_err(|e| Error::io(format!("cannot open {}", path.display()), e))?;
    let metadata = file
        .metadata()
        .map_err(|e| Error::io(format!("cannot stat {}", path.display()), e))?;
    if !metadata.file_type().is_file() {
        return Err(Error::InvalidInput(format!(
            "{} is not a regular file",
            path.display()
        )));
    }
    let source = FileSource::new(file, metadata.len());
    inspect_source(&path.to_string_lossy(), &source, options)
}

/// Inspects image data; `name` is the path used for the extension lookup.
pub fn inspect_source(
    name: &str,
    source: &dyn ByteSource,
    options: &InspectOptions,
) -> Result<ImageReport> {
    let image_size = source.size();
    let head_len = usize::try_from(image_size.min(4096)).unwrap_or(4096);
    let head = read_exact_at(source, 0, head_len)?.unwrap_or_default();
    let association = compression::associate(name);
    let mut notes = Vec::new();
    let mut warnings = Vec::new();

    // ExtractISO(scan): UDF first, then ISO 9660.
    let udf = udf::parse(source)?;
    let iso9660 = iso9660::parse(source)?;
    let (is_iso, label) = match (&udf, &iso9660) {
        (Some(udf), _) if udf.root_found => {
            notes.push(note("Image is a UDF image", "src/iso.c:ExtractISO()"));
            (true, udf.logical_volume_id.clone())
        }
        (_, Some(iso)) => {
            notes.push(note("Image is an ISO9660 image", "src/iso.c:ExtractISO()"));
            (true, iso.label.clone())
        }
        _ => (false, None),
    };

    // IsBootableImage().
    let magic = association.map(|association| compression::check_magic(association, &head));
    if magic == Some(MagicCheck::Mismatch) {
        warnings.push(format!(
            "the file name says {} but the contents do not match that format",
            association.map_or("", Association::extension)
        ));
    }
    let mut projected_size = None;
    let mut is_vhd = false;
    let mut vhd_report = None;
    let mut vtsi_report = None;
    let mut first_sector = None;
    let is_bootable_img = match association {
        None => {
            first_sector = Some(
                head.get(..mbr::MBR_SIZE)
                    .map(<[u8]>::to_vec)
                    .unwrap_or_else(|| head.clone()),
            );
            marker_result(first_sector.as_deref(), options, &mut notes)
        }
        Some(Association::Vhd) => match vhd::parse_vhd(source)? {
            Some(report) if report.checksum_valid => {
                let sector = vhd::first_sector(source, &report)?;
                let result = match sector {
                    Some(sector) => {
                        is_vhd = true;
                        projected_size = Some(report.current_size);
                        let bootable = compressed_marker_result(&sector, options);
                        first_sector = Some(sector);
                        bootable
                    }
                    None => {
                        warnings.push(format!(
                            "{:?} VHDs are not supported in this build",
                            report.disk_type
                        ));
                        Bootability::NotBootable
                    }
                };
                vhd_report = Some(report);
                result
            }
            Some(report) => {
                warnings.push("the VHD footer checksum is invalid".to_string());
                vhd_report = Some(report);
                Bootability::NotBootable
            }
            None => {
                warnings.push("no VHD footer was found".to_string());
                Bootability::NotBootable
            }
        },
        Some(Association::Vhdx) => {
            notes.push(note(
                "VHDX images are not supported in this build",
                "src/vhd.c:IsCompressedBootableImage()",
            ));
            Bootability::Unknown
        }
        Some(Association::Ffu) => {
            // Rufus needs Windows' FFU support (dism) and otherwise reports
            // "this system does not have FFU support".
            notes.push(note(
                "FFU images can only be applied with Windows' dism, which is not available on Linux",
                "src/vhd.c:IsCompressedBootableImage()",
            ));
            Bootability::NotBootable
        }
        Some(Association::Vtsi) => {
            vtsi_report = vhd::parse_vtsi(source)?;
            match &vtsi_report {
                Some(vtsi) => projected_size = Some(vtsi.disk_size),
                None => warnings.push("no Ventoy sparse image footer was found".to_string()),
            }
            Bootability::Unknown
        }
        Some(_) => Bootability::Unknown,
    };
    if is_bootable_img == Bootability::Unknown
        && association.is_some_and(Association::is_compression)
    {
        notes.push(note(
            "compressed images need decompression, which is not supported in this build",
            "src/vhd.c:IsCompressedBootableImage()",
        ));
    }
    if projected_size.is_none() && association.is_none() {
        projected_size = Some(image_size);
    }
    let is_windows_img = head.get(..8) == Some(WIM_MAGIC.as_slice());

    let mbr = first_sector.as_deref().map(mbr::MbrReport::parse);
    // Upstream identifies the boot code (AnalyzeMBR) only for uncompressed
    // images; virtual disks only get the boot marker check.
    if let (None, Some(mbr)) = (association, &mbr) {
        notes.push(note(
            &mbr_message(mbr.identification),
            "src/drive.c:AnalyzeMBR()",
        ));
    }
    let gpt = match association {
        None => gpt::parse(source)?,
        Some(Association::Vhd)
            if vhd_report
                .as_ref()
                .is_some_and(|v| v.disk_type == vhd::VhdDiskType::Fixed) =>
        {
            gpt::parse(source)?
        }
        _ => None,
    };

    let classification = classify(
        image_size,
        is_iso,
        is_bootable_img,
        is_windows_img,
        association,
        magic,
    );
    if classification == Classification::Unsupported {
        notes.push(note(
            "Unsupported image: it is either non-bootable, or it uses a boot or compression method that is not supported",
            "src/rufus.c:ImageScanThread()",
        ));
    }
    Ok(ImageReport {
        path: name.to_string(),
        image_size,
        projected_size,
        association,
        magic,
        is_iso,
        is_bootable_img,
        is_vhd,
        is_windows_img,
        label,
        mbr,
        gpt,
        iso9660,
        udf,
        vhd: vhd_report,
        vtsi: vtsi_report,
        classification,
        notes,
        warnings,
    })
}

/// The boot marker check of `IsBootableImage()` for uncompressed images:
/// `AnalyzeMBR() ? 1 : (ignore_boot_marker ? 2 : 0)`.
fn marker_result(
    sector: Option<&[u8]>,
    options: &InspectOptions,
    notes: &mut Vec<Note>,
) -> Bootability {
    let has_marker = sector.is_some_and(ms_sys::is_br);
    match (has_marker, options.ignore_boot_marker) {
        (true, _) => Bootability::Bootable,
        (false, true) => {
            notes.push(note(
                "Image is a FORCED non-bootable image",
                "src/rufus.c:ImageScanThread()",
            ));
            Bootability::Forced
        }
        (false, false) => Bootability::NotBootable,
    }
}

/// `IsCompressedBootableImage()` for virtual disks: 1 with a marker, 2 if
/// forced, collapsed to 1 by `IsBootableImage()`.
fn compressed_marker_result(sector: &[u8], options: &InspectOptions) -> Bootability {
    if ms_sys::is_br(sector) || options.ignore_boot_marker {
        Bootability::Bootable
    } else {
        Bootability::NotBootable
    }
}

/// `ImageScanThread()`: unsupported when the size is 0, or when the image is
/// neither an ISO, nor DD-bootable, nor a Windows image.
fn classify(
    image_size: u64,
    is_iso: bool,
    bootable: Bootability,
    is_windows_img: bool,
    association: Option<Association>,
    magic: Option<MagicCheck>,
) -> Classification {
    if image_size == 0 || magic == Some(MagicCheck::Mismatch) {
        return Classification::Unsupported;
    }
    if bootable == Bootability::Unknown {
        return Classification::NotYetSupported;
    }
    if is_windows_img {
        return Classification::WindowsImage;
    }
    match (is_iso, bootable.is_dd_bootable()) {
        (true, true) => Classification::IsoHybrid,
        (true, false) => Classification::Iso,
        (false, true) if association.is_none() || association == Some(Association::Vhd) => {
            Classification::DiskImage
        }
        (false, true) => Classification::NotYetSupported,
        (false, false) => Classification::Unsupported,
    }
}

fn mbr_message(identification: ms_sys::MbrIdentification) -> String {
    match identification {
        ms_sys::MbrIdentification::NoBootMarker => "Image does not have a Boot Marker".to_string(),
        ms_sys::MbrIdentification::Known(kind) => {
            format!("Image has a {} Master Boot Record", kind.name())
        }
        ms_sys::MbrIdentification::Unknown => "Image has an unknown Master Boot Record".to_string(),
    }
}

fn note(message: &str, upstream: &'static str) -> Note {
    Note {
        message: message.to_string(),
        upstream: UpstreamRef(upstream),
    }
}
