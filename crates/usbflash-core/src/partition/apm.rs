// SPDX-License-Identifier: GPL-3.0-or-later
//! Read-only parser for the classic Apple Partition Map.

use super::{Partition, PartitionTable, TableKind};
use crate::device::{BlockDevice, read_unaligned};

fn cstr(b: &[u8]) -> String {
    let end = b.iter().position(|&c| c == 0).unwrap_or(b.len());
    String::from_utf8_lossy(&b[..end]).trim().to_string()
}

pub fn parse(dev: &mut dyn BlockDevice, first: &[u8]) -> Option<PartitionTable> {
    if &first[0..2] != b"ER" {
        return None;
    }
    // Block size of the map as declared by the driver descriptor.
    let block = u16::from_be_bytes([first[2], first[3]]) as u64;
    let block = if block == 0 { 512 } else { block };
    let entry = read_unaligned(dev, block, 512).ok()?;
    if &entry[0..2] != b"PM" {
        return None;
    }
    let count = u32::from_be_bytes(entry[4..8].try_into().ok()?).min(256);
    let mut partitions = vec![];
    let mut issues = vec![];
    for i in 0..count as u64 {
        let Ok(e) = read_unaligned(dev, block * (i + 1), 512) else {
            issues.push(format!("cannot read partition map entry {}", i + 1));
            break;
        };
        if &e[0..2] != b"PM" {
            issues.push(format!("invalid partition map entry {}", i + 1));
            break;
        }
        let start = u32::from_be_bytes(e[8..12].try_into().ok()?) as u64;
        let len = u32::from_be_bytes(e[12..16].try_into().ok()?) as u64;
        let ptype = cstr(&e[48..80]);
        if ptype == "Apple_partition_map" || ptype == "Apple_Free" {
            continue;
        }
        partitions.push(Partition {
            index: i as u32 + 1,
            start: start * block,
            size: len * block,
            type_name: ptype.clone(),
            type_id: ptype,
            name: cstr(&e[16..48]),
            bootable: false,
        });
    }
    Some(PartitionTable { kind: TableKind::Apm, disk_id: String::new(), partitions, issues })
}
