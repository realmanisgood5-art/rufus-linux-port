//! `ferrufus`: the desktop interface of Ferrufus, an unofficial Linux port
//! of Rufus (not affiliated with or endorsed by the Rufus project).

#![forbid(unsafe_code)]

use clap::Parser;
use ferrufus_core::device::policy::ListingOptions;
use ferrufus_gui::{FerrufusApp, Options, running_as_root};
use std::path::PathBuf;
use std::process::ExitCode;

#[derive(Debug, Parser)]
#[command(
    name = "ferrufus",
    version,
    about = "Ferrufus: an unofficial Linux port of Rufus — not affiliated with or endorsed by the Rufus project."
)]
struct Args {
    /// Image to select at startup (Rufus `-i`/`--iso`).
    #[arg(short = 'i', long = "iso", value_name = "IMAGE")]
    image: Option<PathBuf>,
    /// List USB hard drives too (Rufus `-x`/`--extra-devs`).
    #[arg(short = 'x', long = "extra-devs")]
    extra_devs: bool,
}

fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("FERRUFUS_LOG")
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .init();
    let args = Args::parse();
    let status = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
    if running_as_root(&status) {
        eprintln!("ferrufus: refusing to run as root. Start Ferrufus as a normal user.");
        return ExitCode::FAILURE;
    }
    let options = Options {
        sysroot: None,
        image: args.image,
        listing: ListingOptions {
            list_usb_hdd: args.extra_devs,
            ..ListingOptions::default()
        },
        synchronous: false,
    };
    let native_options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title("Ferrufus (read-only preview)")
            .with_app_id("ferrufus")
            .with_inner_size([540.0, 760.0]),
        ..eframe::NativeOptions::default()
    };
    let result = eframe::run_native(
        "Ferrufus",
        native_options,
        Box::new(|_| Ok(Box::new(FerrufusApp::new(options)))),
    );
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("ferrufus: cannot open the window: {e}");
            ExitCode::FAILURE
        }
    }
}
