// SPDX-License-Identifier: GPL-3.0-or-later
//! Error type shared by the whole library.

use std::io;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),
    /// A safety guard refused the operation. Never retried automatically.
    #[error("refused: {0}")]
    Refused(String),
    /// The device changed between selection and use (replugged, renumbered).
    #[error("device changed: {0}")]
    DeviceChanged(String),
    #[error("device is write protected: {0}")]
    WriteProtected(String),
    #[error("administrator/root privileges are required: {0}")]
    PermissionDenied(String),
    #[error("not supported: {0}")]
    Unsupported(String),
    #[error("invalid argument: {0}")]
    Invalid(String),
    #[error("operation cancelled")]
    Cancelled,
}

impl Error {
    /// Converts an I/O error from opening `path`, turning "access denied" into a helpful message.
    pub fn from_open(err: io::Error, path: &str) -> Self {
        if err.kind() == io::ErrorKind::PermissionDenied {
            Error::PermissionDenied(format!("cannot open {path}"))
        } else {
            Error::Io(io::Error::new(err.kind(), format!("{path}: {err}")))
        }
    }
}
