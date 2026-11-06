// SPDX-License-Identifier: GPL-3.0-or-later
//! Built-in filesystem formatters.
//!
//! These replace `format.com` and `mkfs.*`, which differ per platform and in the
//! case of Windows refuse FAT32 above 32 GiB. External validation with
//! `fsck.vfat` and `fsck.exfat` runs in CI; see `scripts/fsck-check.sh`.

pub mod exfat;
pub mod fat;

use crate::device::BlockDevice;
use crate::error::{Error, Result};
use crate::progress::Ctx;
use serde::{Deserialize, Serialize};

/// Up to this size, sticks ship with FAT32 from the factory (per the SD standard
/// too); larger ones with exFAT.
pub const FAT_AUTO_LIMIT: u64 = 32 * 1024 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FsChoice {
    /// What a stick of this size has from the factory.
    Auto,
    /// FAT12/16/32, the variant depending on the size.
    Fat,
    Exfat,
    /// Do not create a filesystem.
    None,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FormatInfo {
    pub filesystem: String,
    pub cluster_size: u32,
    pub clusters: u64,
    pub label: String,
    /// MBR partition type byte for this filesystem.
    #[serde(skip)]
    pub mbr_type: u8,
}

/// Formats the byte range `start..start + size` of the device.
pub fn format(
    dev: &mut dyn BlockDevice,
    choice: FsChoice,
    start: u64,
    size: u64,
    label: &str,
    ctx: &mut Ctx,
) -> Result<Option<FormatInfo>> {
    let choice = match choice {
        FsChoice::Auto if size <= FAT_AUTO_LIMIT => FsChoice::Fat,
        FsChoice::Auto => FsChoice::Exfat,
        c => c,
    };
    match choice {
        FsChoice::Fat => fat::format(dev, start, size, label, ctx).map(Some),
        FsChoice::Exfat => exfat::format(dev, start, size, label, ctx).map(Some),
        FsChoice::None => Ok(None),
        FsChoice::Auto => Err(Error::Invalid("unresolved filesystem choice".into())),
    }
}
