// SPDX-License-Identifier: GPL-3.0-or-later
//! Operating system specific device access.
//!
//! Each backend provides the same four functions. To port the tool to a new
//! platform, implement them in a new module; see `docs/ARCHITECTURE.md`.

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
pub use linux::{enumerate, open_exclusive, open_read, read_smart};

#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use windows::{enumerate, open_exclusive, open_read, read_smart};

#[cfg(not(any(windows, target_os = "linux")))]
mod unsupported;
#[cfg(not(any(windows, target_os = "linux")))]
pub use unsupported::{enumerate, open_exclusive, open_read, read_smart};

/// CDB for ATA PASS-THROUGH (16) carrying SMART READ DATA, per the SAT standard.
#[cfg_attr(not(any(windows, target_os = "linux")), allow(dead_code))]
pub(crate) const SAT_SMART_READ_CDB: [u8; 16] = [
    0x85, // ATA PASS-THROUGH (16)
    0x08, // protocol 4: PIO data-in
    0x0E, // transfer direction in, length in sectors, taken from the count field
    0x00, 0xD0, // features: SMART READ DATA
    0x00, 0x01, // sector count: 1
    0x00, 0x00, // LBA low
    0x00, 0x4F, // LBA mid: SMART signature
    0x00, 0xC2, // LBA high: SMART signature
    0xA0, // device
    0xB0, // command: SMART
    0x00,
];
