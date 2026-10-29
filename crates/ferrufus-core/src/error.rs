// SPDX-License-Identifier: GPL-3.0-or-later
//! Error type shared by all of `ferrufus-core`.

use std::io;

/// Errors returned by `ferrufus-core`.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// An I/O operation failed.
    #[error("{context}: {source}")]
    Io {
        /// What was being done, including the path involved.
        context: String,
        /// The underlying error.
        #[source]
        source: io::Error,
    },
    /// The operation was cancelled by the user.
    #[error("operation cancelled")]
    Cancelled,
    /// The input is valid but not supported (yet).
    #[error("unsupported: {0}")]
    Unsupported(String),
    /// The input is invalid or malformed.
    #[error("invalid input: {0}")]
    InvalidInput(String),
    /// No device matches the request.
    #[error("device not found: {0}")]
    DeviceNotFound(String),
    /// More than one device matches the request.
    #[error("ambiguous device: {0}")]
    AmbiguousDevice(String),
}

/// A stable classification of [`Error`], used for example to choose exit codes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ErrorKind {
    /// See [`Error::Io`].
    Io,
    /// See [`Error::Cancelled`].
    Cancelled,
    /// See [`Error::Unsupported`].
    Unsupported,
    /// See [`Error::InvalidInput`].
    InvalidInput,
    /// See [`Error::DeviceNotFound`].
    DeviceNotFound,
    /// See [`Error::AmbiguousDevice`].
    AmbiguousDevice,
}

impl Error {
    /// Creates an [`Error::Io`] with a description of what was being done.
    pub fn io(context: impl Into<String>, source: io::Error) -> Self {
        Self::Io {
            context: context.into(),
            source,
        }
    }

    /// The kind of this error.
    pub fn kind(&self) -> ErrorKind {
        match self {
            Self::Io { .. } => ErrorKind::Io,
            Self::Cancelled => ErrorKind::Cancelled,
            Self::Unsupported(_) => ErrorKind::Unsupported,
            Self::InvalidInput(_) => ErrorKind::InvalidInput,
            Self::DeviceNotFound(_) => ErrorKind::DeviceNotFound,
            Self::AmbiguousDevice(_) => ErrorKind::AmbiguousDevice,
        }
    }
}

/// Result type of `ferrufus-core`.
pub type Result<T, E = Error> = std::result::Result<T, E>;
