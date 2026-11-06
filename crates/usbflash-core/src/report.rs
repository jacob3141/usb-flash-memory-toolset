// SPDX-License-Identifier: GPL-3.0-or-later
//! Machine readable operation reports, e.g. as a record that a stick was wiped.

use crate::device::DeviceInfo;
use serde::Serialize;
use std::time::SystemTime;

pub const TOOL_NAME: &str = "USB Flash Memory Toolset";
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Debug, Clone, Serialize)]
pub struct Report<T: Serialize> {
    pub tool: &'static str,
    pub version: &'static str,
    pub timestamp: String,
    pub operation: String,
    pub device: Option<DeviceInfo>,
    pub result: T,
}

impl<T: Serialize> Report<T> {
    pub fn new(operation: impl Into<String>, device: Option<DeviceInfo>, result: T) -> Self {
        Self {
            tool: TOOL_NAME,
            version: VERSION,
            timestamp: humantime::format_rfc3339_seconds(SystemTime::now()).to_string(),
            operation: operation.into(),
            device,
            result,
        }
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_else(|e| format!("{{\"error\":\"{e}\"}}"))
    }
}
