//! Developer tasks for the Ferrufus repository.
//!
//! - `cargo xtask policy`: checks the repository policy (no `unsafe` code, no
//!   FFI, no C/C++ sources, no native-linking dependencies, and no file
//!   writes in the shipped crates during the read-only milestone).
//! - `cargo xtask gen-tables --upstream <rufus checkout> [--check]`:
//!   generates the data tables translated from upstream Rufus, or checks that
//!   the committed tables match what the upstream checkout produces.

#![forbid(unsafe_code)]

mod policy;
mod tables;

use std::path::{Path, PathBuf};
use std::process::ExitCode;

const USAGE: &str = "usage:
  cargo xtask policy
  cargo xtask gen-tables --upstream <path to a Rufus checkout> [--check]";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let root = workspace_root();
    let result = match args.split_first() {
        Some((command, rest)) if command == "policy" && rest.is_empty() => policy::run(&root),
        Some((command, rest)) if command == "gen-tables" => parse_gen_tables(rest)
            .and_then(|(upstream, check)| tables::run(&root, &upstream, check)),
        _ => Err(USAGE.to_string()),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("xtask: {message}");
            ExitCode::FAILURE
        }
    }
}

fn parse_gen_tables(args: &[String]) -> Result<(PathBuf, bool), String> {
    let mut upstream = None;
    let mut check = false;
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--upstream" => upstream = iter.next().map(PathBuf::from),
            "--check" => check = true,
            _ => return Err(USAGE.to_string()),
        }
    }
    upstream
        .map(|path| (path, check))
        .ok_or_else(|| USAGE.to_string())
}

/// The workspace root is the parent of this crate's directory.
fn workspace_root() -> PathBuf {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    manifest_dir.parent().unwrap_or(manifest_dir).to_path_buf()
}
