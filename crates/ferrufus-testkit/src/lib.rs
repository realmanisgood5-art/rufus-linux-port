//! Test fixtures for Ferrufus.
//!
//! - [`images`]: synthetic disk images built in memory.
//!
//! This crate is only used by tests. It deliberately does not depend on
//! `ferrufus-core`, so tests exercise the real public API. Fixture code
//! fails loudly on misuse, which is why the production lints against
//! unwrapping and lossy casts are relaxed here.

#![forbid(unsafe_code)]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::cast_possible_truncation
)]

pub mod images;
