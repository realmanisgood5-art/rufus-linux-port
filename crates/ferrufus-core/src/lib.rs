//! Shared logic of Ferrufus, an unofficial Linux port of Rufus.
//!
//! Every storage operation exists exactly once, here, and is used by both the
//! command-line and the desktop front ends. Behavior translated from Rufus
//! names its upstream source (see [`upstream`]).
//!
//! This milestone is read-only: nothing in this crate writes to devices or
//! files.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod cancel;
pub mod error;
pub mod hash;
pub mod image;
pub mod units;
pub mod upstream;

pub use error::{Error, ErrorKind, Result};
