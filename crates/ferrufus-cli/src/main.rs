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
use ferrufus_core::plan::{CheckStatus, PlanContext, PlanOptions, prepare_dd};
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
    /// Plan writing an image to a device (dry run: nothing is written).
    Plan {
        /// The target: kernel name (sdb), device node (/dev/sdb) or
        /// /dev/disk/by-id name. Never chosen automatically.
        #[arg(long)]
        device: String,
        /// The image.
        #[arg(long)]
        image: PathBuf,
        /// Do not plan a read-back verification.
        #[arg(long)]
        no_verify: bool,
        #[command(flatten)]
        listing: ListingArgs,
    },
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
        Command::Inspect { image } => {
            let report = inspect(image, &InspectOptions::default()).map_err(image_failure)?;
            emit(cli, "ferrufus.image-report/v1", &report, || {
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
        Command::Plan {
            device,
            image,
            no_verify,
            listing,
        } => {
            if !image.is_file() {
                return Err(image_failure(Error::InvalidInput(format!(
                    "{} is not a regular file",
                    image.display()
                ))));
            }
            let context = prepare_dd(
                &root,
                &scan_options(listing),
                device,
                image,
                PlanOptions { verify: !no_verify },
            )
            .map_err(image_failure)?;
            emit(cli, "ferrufus.plan/v1", &context, || plan_text(&context))?;
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
    let _ = writeln!(
        out,
        "Write {} to {} in DD mode",
        plan.image, plan.data_loss.display_name
    );
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
