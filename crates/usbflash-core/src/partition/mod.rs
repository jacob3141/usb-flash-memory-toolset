// SPDX-License-Identifier: GPL-3.0-or-later
//! Partition table parsing and writing.

pub mod apm;
pub mod gpt;
pub mod mbr;

use crate::device::{BlockDevice, read_unaligned};
use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum TableKind {
    /// No recognizable partition table.
    None,
    Mbr,
    Gpt,
    /// Apple Partition Map.
    Apm,
}

impl TableKind {
    pub fn label(self) -> &'static str {
        match self {
            TableKind::None => "None",
            TableKind::Mbr => "MBR",
            TableKind::Gpt => "GPT",
            TableKind::Apm => "Apple Partition Map",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Partition {
    pub index: u32,
    pub start: u64,
    pub size: u64,
    /// Readable partition type, e.g. `FAT32 (LBA)` or `Linux filesystem`.
    pub type_name: String,
    /// Raw type as stored on disk: MBR byte in hex or GPT GUID.
    pub type_id: String,
    pub name: String,
    pub bootable: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PartitionTable {
    pub kind: TableKind,
    pub disk_id: String,
    pub partitions: Vec<Partition>,
    /// Inconsistencies found while parsing.
    pub issues: Vec<String>,
}

impl PartitionTable {
    fn none() -> Self {
        Self { kind: TableKind::None, disk_id: String::new(), partitions: vec![], issues: vec![] }
    }
}

/// Reads the partition table. Read errors in table sectors become issues.
pub fn read_table(dev: &mut dyn BlockDevice) -> PartitionTable {
    let ss = dev.sector_size() as usize;
    let first = match read_unaligned(dev, 0, ss.max(1024)) {
        Ok(b) => b,
        Err(e) => {
            let mut t = PartitionTable::none();
            t.issues.push(format!("cannot read the first sectors: {e}"));
            return t;
        }
    };
    if let Some(t) = gpt::parse(dev) {
        return t;
    }
    if let Some(t) = apm::parse(dev, &first) {
        return t;
    }
    mbr::parse(dev, &first).unwrap_or_else(PartitionTable::none)
}
