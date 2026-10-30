//! `ferrufus-cli`: the command-line interface of Ferrufus, an unofficial
//! Linux port of Rufus (not affiliated with or endorsed by the Rufus
//! project).
//!
//! This build is read-only: it lists devices, inspects images, computes
//! checksums and plans writes, but never writes anything.

#![forbid(unsafe_code)]

use clap::{Args, Parser, Subcommand};
use ferrufus_core::cancel::CancelToken;
use ferrufus_core::device::policy::ListingOptions;
use ferrufus_core::device::sysfs::SysRoot;
use ferrufus_core::device::{DeviceEntry, DeviceScan, ScanOptions, scan};
use ferrufus_core::hash::{ExpectedHash, HashAlgorithm, HashReport, hash_file};
use ferrufus_core::image::{ImageReport, InspectOptions, inspect};
use ferrufus_core::options::{
    BootType, Choices, DeviceInput, FileSystem, FormatOptions, ImageOption, PartitionScheme,
    TargetSystem, format_options,
};
use ferrufus_core::plan::{
    CheckStatus, PlanContext, PlanOptions, PrepareRequest, WriteMode, prepare,
};
use ferrufus_core::units::{SizeUnits, size_to_human_readable};
use ferrufus_core::{Error, ErrorKind, upstream};
use serde::Serialize;
use std::fmt::Write as _;
use std::io::Write as _;
use std::path::PathBuf;
use std::process::ExitCode;

/// Exit codes. Stable: scripts may rely on them.
mod exit {
    pub const OK: u8 = 0;
    pub const ERROR: u8 = 1;
    pub const USAGE: u8 = 2;
    pub const BLOCKED: u8 = 3;
    pub const DEVICE: u8 = 4;
    pub const IMAGE: u8 = 5;
    pub const MISMATCH: u8 = 6;
    pub const INTERRUPTED: u8 = 130;
}

const ABOUT: &str = "Ferrufus: an unofficial Linux port of Rufus — not affiliated with or endorsed by the Rufus project.\n\
This build is a read-only preview: it never writes to devices.";

#[derive(Debug, Parser)]
#[command(name = "ferrufus-cli", version = version(), about = ABOUT)]
struct Cli {
    /// Print machine-readable JSON.
    #[arg(long, global = true)]
    json: bool,
    /// Read devices from this system tree instead of / (for testing).
    #[arg(long, global = true, hide = true, value_name = "DIR")]
    sysroot: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// List the devices that can be written to.
    Devices {
        /// Also show excluded devices and why they are excluded.
        #[arg(long)]
        all: bool,
        #[command(flatten)]
        listing: ListingArgs,
    },
    /// Analyze an image file.
    Inspect {
        /// The image.
        image: PathBuf,
        #[command(flatten)]
        inspect: InspectArgs,
    },
    /// Compute checksums of a file (MD5, SHA-1 and SHA-256 by default).
    Hash {
        /// The file.
        file: PathBuf,
        /// Also compute SHA-512.
        #[arg(long)]
        sha512: bool,
        /// Compute only these algorithms (md5, sha1, sha256, sha512).
        #[arg(long, value_delimiter = ',', value_parser = parse_algorithm)]
        algo: Vec<HashAlgorithm>,
        /// Compare with an expected checksum (`sha256:<hex>`, or bare hex).
        #[arg(long, value_parser = parse_expected)]
        expect: Option<ExpectedHash>,
    },
    /// Show the format options Rufus would offer and select.
    Options {
        /// The target device. Never chosen automatically.
        #[arg(long)]
        device: String,
        /// The image to boot from (none: non-bootable or `--boot`).
        #[arg(long)]
        image: Option<PathBuf>,
        #[command(flatten)]
        format: FormatArgs,
        #[command(flatten)]
        inspect: InspectArgs,
        #[command(flatten)]
        listing: ListingArgs,
    },
    /// Plan writing to a device (dry run: nothing is written).
    Plan {
        /// The target: kernel name (sdb), device node (/dev/sdb) or
        /// /dev/disk/by-id name. Never chosen automatically.
        #[arg(long)]
        device: String,
        /// The image (required for `dd` and `iso`).
        #[arg(long)]
        image: Option<PathBuf>,
        /// How to write: `dd` (default with an image), `iso` (extract the
        /// files to a new file system), `format` (default without an image).
        #[arg(long, value_enum)]
        mode: Option<ModeArg>,
        /// Do not plan a read-back verification.
        #[arg(long)]
        no_verify: bool,
        #[command(flatten)]
        format: FormatArgs,
        #[command(flatten)]
        inspect: InspectArgs,
        #[command(flatten)]
        listing: ListingArgs,
    },
}

#[derive(Debug, Clone, Copy, clap::ValueEnum)]
enum ModeArg {
    Dd,
    Iso,
    Format,
}

#[derive(Debug, Clone, Copy, clap::ValueEnum)]
enum BootArg {
    NonBootable,
    MsDos,
    Freedos,
    SyslinuxV4,
    SyslinuxV6,
    Reactos,
    Grub4dos,
    Grub2,
    UefiNtfs,
}

#[derive(Debug, Clone, Copy, clap::ValueEnum)]
enum SchemeArg {
    Mbr,
    Gpt,
    Sfd,
}

#[derive(Debug, Clone, Copy, clap::ValueEnum)]
enum TargetArg {
    /// BIOS (or UEFI-CSM).
    Bios,
    /// UEFI (non CSM).
    Uefi,
    /// BIOS or UEFI.
    BiosOrUefi,
}

/// Image analysis switches (Rufus cheat modes).
#[derive(Debug, Args)]
struct InspectArgs {
    /// Skip the ISO file scan.
    #[arg(long)]
    no_scan: bool,
    /// Do not use Joliet (Rufus Alt-J).
    #[arg(long)]
    no_joliet: bool,
    /// Do not use Rock Ridge (Rufus Alt-K).
    #[arg(long)]
    no_rock_ridge: bool,
    /// Treat images without a boot marker as bootable (Rufus Alt-M).
    #[arg(long)]
    ignore_boot_marker: bool,
}

impl InspectArgs {
    fn options(&self) -> InspectOptions {
        InspectOptions {
            ignore_boot_marker: self.ignore_boot_marker,
            disable_joliet: self.no_joliet,
            disable_rock_ridge: self.no_rock_ridge,
            no_scan: self.no_scan,
        }
    }
}

/// Format choices; anything left out gets Rufus' default.
#[derive(Debug, Args)]
struct FormatArgs {
    /// Boot selection when there is no image.
    #[arg(long, value_enum)]
    boot: Option<BootArg>,
    /// Partition scheme.
    #[arg(long, value_enum)]
    scheme: Option<SchemeArg>,
    /// Target system.
    #[arg(long, value_enum)]
    target: Option<TargetArg>,
    /// File system (FAT, FAT32, NTFS, UDF, exFAT, ext2, ext3, ext4).
    #[arg(long = "fs", value_parser = parse_fs)]
    file_system: Option<FileSystem>,
    /// Cluster size in bytes (4096, 32K, ...).
    #[arg(long, value_parser = parse_size)]
    cluster: Option<u64>,
    /// Volume label.
    #[arg(long)]
    label: Option<String>,
    /// Linux persistence partition size (4G, 512M, ...).
    #[arg(long, value_parser = parse_size)]
    persistence: Option<u64>,
    /// Add fixes for old BIOSes (extra partition, alignment).
    #[arg(long)]
    old_bios_fixes: bool,
    /// Allow dual BIOS+UEFI Windows media (Rufus Alt-E).
    #[arg(long)]
    dual_uefi_bios: bool,
    /// Windows To Go instead of a Windows installer.
    #[arg(long)]
    windows_to_go: bool,
    /// Show ext2/ext3/ext4 (Rufus' advanced format options).
    #[arg(long)]
    advanced: bool,
}

impl FormatArgs {
    fn choices(&self) -> Choices {
        Choices {
            boot_type: self.boot.map(|b| match b {
                BootArg::NonBootable => BootType::NonBootable,
                BootArg::MsDos => BootType::MsDos,
                BootArg::Freedos => BootType::FreeDos,
                BootArg::SyslinuxV4 => BootType::SyslinuxV4,
                BootArg::SyslinuxV6 => BootType::SyslinuxV6,
                BootArg::Reactos => BootType::ReactOs,
                BootArg::Grub4dos => BootType::Grub4Dos,
                BootArg::Grub2 => BootType::Grub2,
                BootArg::UefiNtfs => BootType::UefiNtfs,
            }),
            image_option: if self.windows_to_go {
                ImageOption::WindowsToGo
            } else {
                ImageOption::Standard
            },
            allow_dual_uefi_bios: self.dual_uefi_bios,
            partition_scheme: self.scheme.map(|s| match s {
                SchemeArg::Mbr => PartitionScheme::Mbr,
                SchemeArg::Gpt => PartitionScheme::Gpt,
                SchemeArg::Sfd => PartitionScheme::Sfd,
            }),
            target_system: self.target.map(|t| match t {
                TargetArg::Bios => TargetSystem::BiosOrCsm,
                TargetArg::Uefi => TargetSystem::Uefi,
                TargetArg::BiosOrUefi => TargetSystem::BiosOrUefi,
            }),
            file_system: self.file_system,
            preselected_fs: None,
            advanced_format: self.advanced,
            force_large_fat32: false,
            cluster_size: self.cluster.and_then(|c| u32::try_from(c).ok()),
            label: self.label.clone(),
        }
    }
}

fn parse_fs(text: &str) -> Result<FileSystem, String> {
    FileSystem::from_label(text).ok_or_else(|| format!("unknown file system `{text}`"))
}

/// Sizes: bytes, or a number with K, M, G or T (powers of 1024).
fn parse_size(text: &str) -> Result<u64, String> {
    let text = text.trim();
    let (digits, shift) = match text.chars().last().map(|c| c.to_ascii_uppercase()) {
        Some('K') => (&text[..text.len() - 1], 10),
        Some('M') => (&text[..text.len() - 1], 20),
        Some('G') => (&text[..text.len() - 1], 30),
        Some('T') => (&text[..text.len() - 1], 40),
        _ => (text, 0),
    };
    let value: u64 = digits
        .trim()
        .parse()
        .map_err(|_| format!("`{text}` is not a size"))?;
    value
        .checked_mul(1u64 << shift)
        .ok_or_else(|| format!("`{text}` is too large"))
}

#[derive(Debug, Args)]
struct ListingArgs {
    /// List USB hard drives too (Rufus `-x`/"List USB Hard Drives").
    #[arg(short = 'x', long = "list-usb-hdd")]
    usb_hdd: bool,
    /// List non-USB removable drives too.
    #[arg(long)]
    list_non_usb_removable: bool,
    /// List loop devices too.
    #[arg(long)]
    list_virtual: bool,
}

impl ListingArgs {
    fn options(&self) -> ListingOptions {
        ListingOptions {
            list_usb_hdd: self.usb_hdd,
            list_non_usb_removable: self.list_non_usb_removable,
            list_virtual: self.list_virtual,
            ignore_usb: Vec::new(),
        }
    }
}

/// The version, with the commit when the build sets `FERRUFUS_GIT_SHA`.
fn version() -> &'static str {
    match option_env!("FERRUFUS_GIT_SHA") {
        // Built once per process, for clap.
        Some(sha) if !sha.is_empty() => {
            Box::leak(format!("{} ({sha})", env!("CARGO_PKG_VERSION")).into_boxed_str())
        }
        _ => env!("CARGO_PKG_VERSION"),
    }
}

fn parse_algorithm(text: &str) -> Result<HashAlgorithm, String> {
    HashAlgorithm::from_name(text).ok_or_else(|| format!("unknown algorithm `{text}`"))
}

fn parse_expected(text: &str) -> Result<ExpectedHash, String> {
    text.parse().map_err(|e: Error| e.to_string())
}

/// JSON output: every document names its schema.
#[derive(Serialize)]
struct Envelope<'a, T: Serialize> {
    schema: &'static str,
    ferrufus_version: &'static str,
    upstream_baseline: &'static str,
    #[serde(flatten)]
    data: &'a T,
}

fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("FERRUFUS_LOG")
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .init();
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(e) => {
            let code = if e.use_stderr() {
                exit::USAGE
            } else {
                exit::OK
            };
            let _ = e.print();
            return ExitCode::from(code);
        }
    };
    match run(&cli) {
        Ok(code) => ExitCode::from(code),
        Err(failure) => {
            eprintln!("ferrufus-cli: {}", failure.error);
            ExitCode::from(failure.code)
        }
    }
}

/// An error and the exit code it maps to.
struct Failure {
    code: u8,
    error: Error,
}

impl From<Error> for Failure {
    fn from(error: Error) -> Self {
        let code = match error.kind() {
            ErrorKind::DeviceNotFound | ErrorKind::AmbiguousDevice => exit::DEVICE,
            ErrorKind::Cancelled => exit::INTERRUPTED,
            ErrorKind::Io | ErrorKind::Unsupported | ErrorKind::InvalidInput => exit::ERROR,
        };
        Self { code, error }
    }
}

/// Errors reading or analyzing an image exit with the image code.
fn image_failure(error: Error) -> Failure {
    match error.kind() {
        ErrorKind::Io | ErrorKind::Unsupported | ErrorKind::InvalidInput => Failure {
            code: exit::IMAGE,
            error,
        },
        _ => error.into(),
    }
}

fn run(cli: &Cli) -> Result<u8, Failure> {
    let root = cli
        .sysroot
        .as_ref()
        .map_or_else(SysRoot::host, SysRoot::new);
    let scan_options = |listing: &ListingArgs| {
        let mut options = ScanOptions::for_this_process(listing.options());
        if cli.sysroot.is_some() {
            // The program's own files are on the real system, not in the tree.
            options.program_paths.clear();
        }
        options
    };
    match &cli.command {
        Command::Devices { all, listing } => {
            let scan = scan(&root, &scan_options(listing))?;
            emit(cli, "ferrufus.devices/v1", &scan, || {
                devices_text(&scan, *all)
            })?;
            Ok(exit::OK)
        }
        Command::Inspect {
            image,
            inspect: switches,
        } => {
            let report = inspect(image, &switches.options()).map_err(image_failure)?;
            emit(cli, "ferrufus.image-report/v2", &report, || {
                inspect_text(&report)
            })?;
            Ok(exit::OK)
        }
        Command::Hash {
            file,
            sha512,
            algo,
            expect,
        } => {
            let mut algorithms = if algo.is_empty() {
                HashAlgorithm::UPSTREAM_DEFAULT.to_vec()
            } else {
                algo.clone()
            };
            if *sha512 {
                algorithms.push(HashAlgorithm::Sha512);
            }
            if let Some(expected) = expect {
                algorithms.push(expected.algorithm);
            }
            algorithms.sort();
            algorithms.dedup();
            let report = hash_file(file, &algorithms, &mut |_| {}, &CancelToken::new())?;
            let matches = expect.as_ref().and_then(|e| report.matches(e));
            let output = HashOutput {
                file: file.display().to_string(),
                report: &report,
                expected: expect.as_ref(),
                matches,
            };
            emit(cli, "ferrufus.hash/v1", &output, || hash_text(&output))?;
            Ok(if matches == Some(false) {
                exit::MISMATCH
            } else {
                exit::OK
            })
        }
        Command::Options {
            device,
            image,
            format,
            inspect: switches,
            listing,
        } => {
            let report = match image {
                Some(path) => Some(inspect(path, &switches.options()).map_err(image_failure)?),
                None => None,
            };
            let scan = scan(&root, &scan_options(listing))?;
            let entry = scan.find(device)?;
            let options =
                format_options(&DeviceInput::of(entry), report.as_ref(), &format.choices());
            emit(cli, "ferrufus.options/v1", &options, || {
                options_text(&options)
            })?;
            Ok(exit::OK)
        }
        Command::Plan {
            device,
            image,
            mode,
            no_verify,
            format,
            inspect: switches,
            listing,
        } => {
            let mode = match (mode, image) {
                (Some(ModeArg::Dd), _) | (None, Some(_)) => WriteMode::Dd,
                (Some(ModeArg::Iso), _) => WriteMode::Iso,
                (Some(ModeArg::Format), _) | (None, None) => WriteMode::Format,
            };
            if let Some(image) = image
                && !image.is_file()
            {
                return Err(image_failure(Error::InvalidInput(format!(
                    "{} is not a regular file",
                    image.display()
                ))));
            }
            if mode != WriteMode::Format && image.is_none() {
                return Err(Failure {
                    code: exit::USAGE,
                    error: Error::InvalidInput("--image is required for this mode".into()),
                });
            }
            let request = PrepareRequest {
                image: if mode == WriteMode::Format {
                    None
                } else {
                    image.clone()
                },
                inspect: switches.options(),
                choices: format.choices(),
                persistence: format.persistence.unwrap_or(0),
                old_bios_fixes: format.old_bios_fixes,
            };
            let context = prepare(
                &root,
                &scan_options(listing),
                device,
                mode,
                &request,
                PlanOptions { verify: !no_verify },
            )
            .map_err(image_failure)?;
            emit(cli, "ferrufus.plan/v2", &context, || plan_text(&context))?;
            Ok(if context.plan.blocked {
                exit::BLOCKED
            } else {
                exit::OK
            })
        }
    }
}

fn emit<T: Serialize>(
    cli: &Cli,
    schema: &'static str,
    data: &T,
    text: impl FnOnce() -> String,
) -> Result<(), Error> {
    let output = if cli.json {
        let envelope = Envelope {
            schema,
            ferrufus_version: env!("CARGO_PKG_VERSION"),
            upstream_baseline: upstream::BASELINE_TAG,
            data,
        };
        let mut json = serde_json::to_string_pretty(&envelope)
            .map_err(|e| Error::InvalidInput(format!("cannot encode JSON: {e}")))?;
        json.push('\n');
        json
    } else {
        text()
    };
    let mut stdout = std::io::stdout().lock();
    match stdout
        .write_all(output.as_bytes())
        .and_then(|()| stdout.flush())
    {
        // A closed pipe (`| head`) is not an error.
        Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => Ok(()),
        result => result.map_err(|e| Error::io("cannot write to standard output", e)),
    }
}

fn human(size: u64) -> String {
    size_to_human_readable(size, SizeUnits::Fake)
}

fn device_line(out: &mut String, device: &DeviceEntry) {
    let _ = writeln!(
        out,
        "  {:<10} {}  [{:?}] {}",
        device.facts.name, device.display_name, device.decision.category, device.stable_id
    );
}

fn devices_text(scan: &DeviceScan, all: bool) -> String {
    let mut out = String::new();
    let listed: Vec<_> = scan.listed().collect();
    if listed.is_empty() {
        out.push_str("No device can be written to.\n");
    } else {
        out.push_str("Devices that can be written to (none is selected; name one explicitly):\n");
        for device in listed {
            device_line(&mut out, device);
        }
    }
    let excluded: Vec<_> = scan.devices.iter().filter(|d| !d.decision.listed).collect();
    if all {
        if !excluded.is_empty() {
            out.push_str("\nExcluded:\n");
        }
        for device in excluded {
            device_line(&mut out, device);
            for reason in &device.decision.reasons {
                let _ = writeln!(out, "      - {} ({})", reason.message, reason.upstream);
            }
        }
    } else if !excluded.is_empty() {
        let _ = writeln!(
            out,
            "\n{} more device(s) excluded; use --all to see why.",
            excluded.len()
        );
    }
    for unresolved in &scan.system.unresolved {
        let _ = writeln!(out, "warning: {unresolved}");
    }
    out
}

fn options_text(options: &FormatOptions) -> String {
    let mut out = String::new();
    let mark = |selected: bool| if selected { "*" } else { " " };
    let _ = writeln!(out, "Boot selection: {:?}", options.boot_type);
    out.push_str("Partition scheme:\n");
    for scheme in &options.partition_schemes {
        let _ = writeln!(
            out,
            " {} {}",
            mark(options.partition_scheme == Some(*scheme)),
            scheme.label()
        );
    }
    out.push_str("Target system:\n");
    for target in &options.target_systems {
        let _ = writeln!(
            out,
            " {} {}",
            mark(options.target_system == Some(*target)),
            target.label()
        );
    }
    out.push_str("File system:\n");
    for entry in &options.file_systems {
        let _ = writeln!(
            out,
            " {} {}",
            mark(options.file_system == Some(entry.file_system)),
            entry.label
        );
    }
    if let Some(cluster) = options.cluster_size {
        let _ = writeln!(
            out,
            "Cluster size: {}",
            if cluster == 0 {
                "default".to_string()
            } else {
                format!("{cluster} bytes")
            }
        );
    }
    let _ = writeln!(out, "Volume label: {}", options.label);
    for note in &options.notes {
        let _ = writeln!(out, "note: {} ({})", note.message, note.upstream);
    }
    out
}

fn inspect_text(report: &ImageReport) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "Image:          {}", report.path);
    let _ = writeln!(
        out,
        "Size:           {} ({} bytes)",
        human(report.image_size),
        report.image_size
    );
    if let Some(size) = report.projected_size {
        let _ = writeln!(out, "Projected size: {} ({size} bytes)", human(size));
    }
    let _ = writeln!(out, "Type:           {:?}", report.classification);
    let _ = writeln!(out, "Bootable (DD):  {:?}", report.is_bootable_img);
    if let Some(label) = &report.label {
        let _ = writeln!(out, "Label:          {label}");
    }
    if let Some(scan) = &report.iso_scan {
        let _ = writeln!(out, "ISO scan:");
        let flag = |set: bool, name: &str| {
            if set {
                format!(" {name}")
            } else {
                String::new()
            }
        };
        let _ = writeln!(
            out,
            "  boot:{}{}{}{}{}{}",
            flag(scan.is_bios_bootable(), "BIOS"),
            flag(scan.is_efi_bootable(), "UEFI"),
            flag(scan.has_windows(), "Windows"),
            flag(
                scan.has_syslinux(),
                &format!("Syslinux-{}{}", scan.sl_version_str, scan.sl_version_ext)
            ),
            flag(scan.has_grub2 != 0, &format!("GRUB-{}", scan.grub2_version)),
            flag(scan.has_reactos(), "ReactOS")
        );
        if let Some(cfg) = &scan.cfg_path {
            let _ = writeln!(out, "  Syslinux configuration: {cfg}");
        }
        for entry in &scan.efi_boot_entries {
            let _ = writeln!(out, "  UEFI: {} ({:?})", entry.path, entry.kind);
        }
        if scan.has_4gb_file != 0 {
            let _ = writeln!(out, "  has files of 4 GB or more (needs NTFS or exFAT)");
        }
        if scan.disable_iso {
            let _ = writeln!(out, "  ISO mode is not supported for this image");
        }
        for item in &scan.not_analyzed {
            let _ = writeln!(out, "  not analyzed: {item}");
        }
    }
    for note in &report.notes {
        let _ = writeln!(out, "  {} ({})", note.message, note.upstream);
    }
    for warning in &report.warnings {
        let _ = writeln!(out, "warning: {warning}");
    }
    out
}

#[derive(Serialize)]
struct HashOutput<'a> {
    file: String,
    #[serde(flatten)]
    report: &'a HashReport,
    expected: Option<&'a ExpectedHash>,
    matches: Option<bool>,
}

fn hash_text(output: &HashOutput<'_>) -> String {
    let mut out = String::new();
    for digest in &output.report.digests {
        let _ = writeln!(
            out,
            "{:<7} {}  {}",
            digest.algorithm.name(),
            digest.hex,
            output.file
        );
    }
    match output.matches {
        Some(true) => out.push_str("Checksum matches.\n"),
        Some(false) => out.push_str("CHECKSUM MISMATCH.\n"),
        None => {}
    }
    out
}

fn plan_text(context: &PlanContext) -> String {
    let plan = &context.plan;
    let mut out = String::new();
    let _ = writeln!(out, "DRY RUN: nothing will be written.\n");
    let what = match plan.mode {
        "dd" => format!(
            "Write {} to {} in DD mode",
            plan.image.as_deref().unwrap_or("?"),
            plan.data_loss.display_name
        ),
        "iso" => format!(
            "Write {} to {} in ISO mode",
            plan.image.as_deref().unwrap_or("?"),
            plan.data_loss.display_name
        ),
        _ => format!("Format {}", plan.data_loss.display_name),
    };
    let _ = writeln!(out, "{what}");
    let _ = writeln!(out, "{}\n", plan.data_loss.summary);
    for partition in &plan.data_loss.partitions {
        let _ = writeln!(
            out,
            "  {} {} {} {}",
            partition.devnode,
            partition.size_bytes.map(human).unwrap_or_default(),
            partition.fs_type.as_deref().unwrap_or("-"),
            partition.label.as_deref().unwrap_or("")
        );
    }
    if let Some(format) = &plan.format {
        let _ = writeln!(
            out,
            "Options: {} / {} / {} / cluster {} / label \"{}\"",
            format.partition_scheme.map_or("-", PartitionScheme::label),
            format.target_system.map_or("-", TargetSystem::label),
            format.file_system.map_or("-", FileSystem::label),
            format.cluster_size.map_or("-".to_string(), |c| if c == 0 {
                "default".into()
            } else {
                c.to_string()
            }),
            format.label
        );
    }
    if !plan.layout.is_empty() {
        out.push_str("Partitions to create:\n");
        for part in &plan.layout {
            let _ = writeln!(
                out,
                "  {} at {} ({})",
                part.name,
                part.offset,
                human(part.size)
            );
        }
    }
    out.push_str("\nChecks:\n");
    for check in &plan.checks {
        let mark = match check.status {
            CheckStatus::Pass => "ok  ",
            CheckStatus::Warn => "warn",
            CheckStatus::Fail => "FAIL",
        };
        let _ = writeln!(
            out,
            "  [{mark}] {}: {} ({})",
            check.id, check.message, check.upstream
        );
    }
    out.push_str("\nSteps (not implemented in this build):\n");
    for (index, step) in plan.steps.iter().enumerate() {
        let _ = writeln!(
            out,
            "  {}. {} ({})",
            index.saturating_add(1),
            step.description,
            step.upstream
        );
    }
    let verdict = if plan.blocked {
        "BLOCKED"
    } else {
        "possible, but not executable in this build"
    };
    let _ = writeln!(out, "\nPlan {verdict}. Fingerprint: {}", plan.fingerprint);
    out
}
