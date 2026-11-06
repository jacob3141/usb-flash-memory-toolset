// SPDX-License-Identifier: GPL-3.0-or-later
//! Master Boot Record parsing and writing.

use super::{Partition, PartitionTable, TableKind};
use crate::device::{BlockDevice, read_unaligned, write_sectors};
use std::io;

const EXTENDED: [u8; 3] = [0x05, 0x0F, 0x85];

pub fn type_name(t: u8) -> &'static str {
    match t {
        0x00 => "Empty",
        0x01 => "FAT12",
        0x04 | 0x06 => "FAT16",
        0x05 | 0x0F | 0x85 => "Extended",
        0x07 => "NTFS / exFAT / HPFS",
        0x0B => "FAT32 (CHS)",
        0x0C => "FAT32 (LBA)",
        0x0E => "FAT16 (LBA)",
        0x11 | 0x14 | 0x16 | 0x1B | 0x1C | 0x1E => "Hidden FAT",
        0x17 => "Hidden NTFS / exFAT",
        0x27 => "Windows recovery",
        0x42 => "Windows dynamic disk",
        0x82 => "Linux swap / Solaris",
        0x83 => "Linux filesystem",
        0x8E => "Linux LVM",
        0xA5 => "FreeBSD",
        0xA6 => "OpenBSD",
        0xA8 => "Apple UFS",
        0xA9 => "NetBSD",
        0xAB => "Apple boot",
        0xAF => "Apple HFS / HFS+",
        0xBE => "Solaris boot",
        0xBF => "Solaris",
        0xE8 => "LUKS",
        0xEE => "GPT protective",
        0xEF => "EFI system",
        0xFB => "VMware VMFS",
        0xFD => "Linux RAID",
        _ => "Unknown",
    }
}

fn entry(sector: &[u8], i: usize) -> (u8, u8, u64, u64) {
    let e = &sector[446 + i * 16..446 + (i + 1) * 16];
    let lba = u32::from_le_bytes([e[8], e[9], e[10], e[11]]) as u64;
    let len = u32::from_le_bytes([e[12], e[13], e[14], e[15]]) as u64;
    (e[0], e[4], lba, len)
}

/// Parses an MBR. Returns `None` if the sector does not contain a plausible one.
pub fn parse(dev: &mut dyn BlockDevice, first: &[u8]) -> Option<PartitionTable> {
    if first[510] != 0x55 || first[511] != 0xAA {
        return None;
    }
    // FAT/NTFS boot sectors share the signature. In a real MBR, all status bytes
    // are 0x00 or 0x80 and at least one partition is defined.
    let entries: Vec<_> = (0..4).map(|i| entry(first, i)).collect();
    if entries.iter().any(|e| e.0 != 0 && e.0 != 0x80) {
        return None;
    }
    if entries.iter().all(|e| e.1 == 0 || e.3 == 0) {
        return None;
    }
    if crate::fsdetect::looks_like_boot_sector(first) {
        return None;
    }

    let ss = dev.sector_size() as u64;
    let device_end = dev.size();
    let mut issues = vec![];
    let mut partitions = vec![];
    for (i, &(status, ptype, lba, len)) in entries.iter().enumerate() {
        if ptype == 0 || len == 0 {
            continue;
        }
        if EXTENDED.contains(&ptype) {
            read_extended(dev, lba, &mut partitions, &mut issues);
            continue;
        }
        partitions.push(Partition {
            index: i as u32 + 1,
            start: lba * ss,
            size: len * ss,
            type_name: type_name(ptype).to_string(),
            type_id: format!("0x{ptype:02X}"),
            name: String::new(),
            bootable: status == 0x80,
        });
    }
    for p in &partitions {
        if p.start + p.size > device_end {
            issues.push(format!(
                "partition {} ends beyond the end of the device (truncated image or wrong reported capacity)",
                p.index
            ));
        }
    }
    let sig = u32::from_le_bytes([first[440], first[441], first[442], first[443]]);
    Some(PartitionTable { kind: TableKind::Mbr, disk_id: format!("{sig:08X}"), partitions, issues })
}

fn read_extended(dev: &mut dyn BlockDevice, ext_base: u64, out: &mut Vec<Partition>, issues: &mut Vec<String>) {
    let ss = dev.sector_size() as u64;
    let mut next = ext_base;
    let mut index = 5;
    // The bound protects against loops in corrupt chains.
    for _ in 0..128 {
        let Ok(s) = read_unaligned(dev, next * ss, ss as usize) else {
            issues.push(format!("cannot read extended partition table at LBA {next}"));
            return;
        };
        if s[510] != 0x55 || s[511] != 0xAA {
            issues.push(format!("invalid extended partition table at LBA {next}"));
            return;
        }
        let (status, ptype, lba, len) = entry(&s, 0);
        if ptype != 0 && len != 0 {
            out.push(Partition {
                index,
                start: (next + lba) * ss,
                size: len * ss,
                type_name: type_name(ptype).to_string(),
                type_id: format!("0x{ptype:02X}"),
                name: String::new(),
                bootable: status == 0x80,
            });
            index += 1;
        }
        let (_, link_type, link_lba, _) = entry(&s, 1);
        if link_type == 0 || link_lba == 0 {
            return;
        }
        next = ext_base + link_lba;
    }
    issues.push("extended partition chain is too long or contains a loop".to_string());
}

/// Writes an MBR with one partition.
pub fn write_single(
    dev: &mut dyn BlockDevice,
    disk_signature: u32,
    partition_type: u8,
    start: u64,
    size: u64,
) -> io::Result<()> {
    let ss = dev.sector_size() as u64;
    let mut s = vec![0u8; ss as usize];
    s[440..444].copy_from_slice(&disk_signature.to_le_bytes());
    let e = &mut s[446..462];
    // CHS fields are obsolete; 0xFE 0xFF 0xFF means "use LBA".
    e[1..4].copy_from_slice(&[0xFE, 0xFF, 0xFF]);
    e[4] = partition_type;
    e[5..8].copy_from_slice(&[0xFE, 0xFF, 0xFF]);
    e[8..12].copy_from_slice(&((start / ss) as u32).to_le_bytes());
    e[12..16].copy_from_slice(&((size / ss) as u32).to_le_bytes());
    s[510] = 0x55;
    s[511] = 0xAA;
    write_sectors(dev, 0, &s)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::device::FileDevice;

    #[test]
    fn round_trip() {
        let p = std::env::temp_dir().join(format!("usbflash-mbr-{}", std::process::id()));
        let mut d = FileDevice::create(&p, 16 << 20, 512).unwrap();
        write_single(&mut d, 0xDEADBEEF, 0x0C, 1 << 20, 8 << 20).unwrap();
        let t = crate::partition::read_table(&mut d);
        assert_eq!(t.kind, TableKind::Mbr);
        assert_eq!(t.disk_id, "DEADBEEF");
        assert_eq!(t.partitions.len(), 1);
        assert_eq!(t.partitions[0].start, 1 << 20);
        assert_eq!(t.partitions[0].size, 8 << 20);
        assert!(t.issues.is_empty());
        drop(d);
        std::fs::remove_file(p).ok();
    }

    #[test]
    fn flags_partition_beyond_device_end() {
        let p = std::env::temp_dir().join(format!("usbflash-mbr2-{}", std::process::id()));
        let mut d = FileDevice::create(&p, 4 << 20, 512).unwrap();
        write_single(&mut d, 1, 0x0C, 1 << 20, 64 << 20).unwrap();
        let t = crate::partition::read_table(&mut d);
        assert_eq!(t.issues.len(), 1);
        drop(d);
        std::fs::remove_file(p).ok();
    }
}
