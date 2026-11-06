// SPDX-License-Identifier: GPL-3.0-or-later
//! Core library of the USB Flash Memory Toolset.
//!
//! All functionality lives here; the CLI and GUI are thin front ends. Everything
//! that touches a real device goes through [`ops`], which is the single place
//! where the safety guards in [`guard`] are enforced.

pub mod analyze;
pub mod capacity;
pub mod device;
pub mod error;
pub mod format;
pub mod fsdetect;
pub mod guard;
pub mod health;
pub mod image;
pub mod ops;
pub mod partition;
pub mod pattern;
pub mod platform;
pub mod progress;
pub mod report;
pub mod reset;
pub mod scan;
pub mod smart;
pub mod sweep;
#[cfg(test)]
pub(crate) mod testutil;
pub mod units;
pub mod wipe;

pub use device::{BlockDevice, BusType, DeviceInfo, FileDevice};
pub use error::{Error, Result};
pub use guard::Overrides;
pub use ops::Target;
pub use progress::{CancelFlag, Ctx, Progress};
