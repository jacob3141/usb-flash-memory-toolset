// SPDX-License-Identifier: GPL-3.0-or-later
//! Placeholder for platforms without a backend. Image files still work there.

use crate::device::{BlockDevice, DeviceInfo};
use crate::error::{Error, Result};

fn unsupported() -> Error {
    Error::Unsupported("device access is not implemented on this operating system".to_string())
}

pub fn enumerate() -> Result<Vec<DeviceInfo>> {
    Err(unsupported())
}

pub fn open_read(_: &DeviceInfo) -> Result<Box<dyn BlockDevice>> {
    Err(unsupported())
}

pub fn open_exclusive(_: &DeviceInfo) -> Result<Box<dyn BlockDevice>> {
    Err(unsupported())
}

pub fn read_smart(_: &DeviceInfo) -> Result<Vec<u8>> {
    Err(unsupported())
}
