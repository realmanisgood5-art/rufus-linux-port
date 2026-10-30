//! Test fixtures for Ferrufus.
//!
//! - [`images`]: synthetic disk images built in memory.
//! - [`isofs`]: ISO 9660/Joliet/Rock Ridge/UDF images with directory trees.
//! - [`system`]: fake sysfs, procfs, `/dev` and udev trees.
//!
//! This crate is only used by tests. It deliberately does not depend on
//! `ferrufus-core`, so tests exercise the real public API. Fixture code
//! fails loudly on misuse, which is why the production lints against
//! unwrapping and lossy casts are relaxed here.

#![forbid(unsafe_code)]
#![allow(
    clippy::needless_range_loop,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::cast_possible_truncation
)]

pub mod images;
pub mod isofs;
pub mod system;
