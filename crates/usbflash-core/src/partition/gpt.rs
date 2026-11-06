// SPDX-License-Identifier: GPL-3.0-or-later
//! GUID Partition Table parsing and writing.

use super::{Partition, PartitionTable, TableKind};
use crate::device::{AlignedBuf, BlockDevice, read_unaligned, write_sectors};
use crate::partition::mbr;
use std::io;

const SIGNATURE: &[u8; 8] = b"EFI PART";
const ENTRY_SIZE: usize = 128;
const ENTRY_COUNT: usize = 128;
/// Microsoft basic data partition.
pub const BASIC_DATA: &str = "EBD0A0A2-B9E5-4433-87C0-68B6B72699C7";

/// Formats a GUID stored in the on-disk mixed-endian layout.
pub fn format_guid(b: &[u8]) -> String {
    format!(
        "{:02X}{:02X}{:02X}{:02X}-{:02X}{:02X}-{:02X}{:02X}-{:02X}{:02X}-{:02X}{:02X}{:02X}{:02X}{:02X}{:02X}",
        b[3], b[2], b[1], b[0], b[5], b[4], b[7], b[6], b[8], b[9], b[10], b[11], b[12], b[13], b[14], b[15]
    )
}

/// Converts a textual GUID to its on-disk layout.
pub fn parse_guid(s: &str) -> Option<[u8; 16]> {
    let hex: Vec<u8> = s
        .replace('-', "")
        .as_bytes()
        .chunks(2)
        .map(|c| u8::from_str_radix(std::str::from_utf8(c).ok()?, 16).ok())
        .collect::<Option<_>>()?;
    if hex.len() != 16 {
        return None;
    }
    let mut g = [0u8; 16];
    g[..4].copy_from_slice(&[hex[3], hex[2], hex[1], hex[0]]);
    g[4..6].copy_from_slice(&[hex[5], hex[4]]);
    g[6..8].copy_from_slice(&[hex[7], hex[6]]);
    g[8..].copy_from_slice(&hex[8..]);
    Some(g)
}

pub fn type_name(guid: &str) -> &'static str {
    match guid {
        BASIC_DATA => "Microsoft basic data",
        "C12A7328-F81F-11D2-BA4B-00A0C93EC93B" => "EFI system",
        "E3C9E316-0B5C-4DB8-817D-F92DF00215AE" => "Microsoft reserved",
        "DE94BBA4-06D1-4D40-A16A-BFD50179D6AC" => "Windows recovery",
        "0FC63DAF-8483-4772-8E79-3D69D8477DE4" => "Linux filesystem",
        "0657FD6D-A4AB-43C4-84E5-0933C84B4F4F" => "Linux swap",
        "E6D6D379-F507-44C2-A23C-238F2A3DF928" => "Linux LVM",
        "A19D880F-05FC-4D3B-A006-743F0F84911E" => "Linux RAID",
        "CA7D7CCB-63ED-4C53-861C-1742536059CC" => "LUKS",
        "4F68BCE3-E8CD-4DB1-96E7-FBCAF984B709" => "Linux root (x86-64)",
        "48465300-0000-11AA-AA11-00306543ECAC" => "Apple HFS+",
        "7C3457EF-0000-11AA-AA11-00306543ECAC" => "Apple APFS",
        "426F6F74-0000-11AA-AA11-00306543ECAC" => "Apple boot",
        "516E7CB4-6ECF-11D6-8FF8-00022D09712B" => "FreeBSD UFS",
        "516E7CBA-6ECF-11D6-8FF8-00022D09712B" => "FreeBSD ZFS",
        "6A898CC3-1DD2-11B2-99A6-080020736631" => "ZFS",
        "21686148-6449-6E6F-744E-656564454649" => "BIOS boot",
        _ => "Unknown",
    }
}

struct Header {
    entries_lba: u64,
    count: usize,
    entry_size: usize,
    disk_guid: String,
    entries_crc: u32,
    backup_lba: u64,
}

fn parse_header(sector: &[u8]) -> Option<Header> {
    if &sector[0..8] != SIGNATURE {
        return None;
    }
    let size = u32::from_le_bytes(sector[12..16].try_into().ok()?) as usize;
    if !(92..=sector.len()).contains(&size) {
        return None;
    }
    let stored = u32::from_le_bytes(sector[16..20].try_into().ok()?);
    let mut copy = sector[..size].to_vec();
    copy[16..20].fill(0);
    if crc32fast::hash(&copy) != stored {
        return None;
    }
    Some(Header {
        backup_lba: u64::from_le_bytes(sector[32..40].try_into().ok()?),
        disk_guid: format_guid(&sector[56..72]),
        entries_lba: u64::from_le_bytes(sector[72..80].try_into().ok()?),
        count: u32::from_le_bytes(sector[80..84].try_into().ok()?) as usize,
        entry_size: u32::from_le_bytes(sector[84..88].try_into().ok()?) as usize,
        entries_crc: u32::from_le_bytes(sector[88..92].try_into().ok()?),
    })
}

/// Parses the GPT, falling back to the backup header if the primary is damaged.
/// Returns `None` if neither copy is valid.
pub fn parse(dev: &mut dyn BlockDevice) -> Option<PartitionTable> {
    let ss = dev.sector_size() as u64;
    let last_lba = (dev.size() / ss).checked_sub(1)?;
    let mut issues = vec![];

    let primary = read_unaligned(dev, ss, ss as usize).ok().and_then(|s| parse_header(&s));
    let backup = read_unaligned(dev, last_lba * ss, ss as usize).ok().and_then(|s| parse_header(&s));
    let header = match (primary, backup) {
        (Some(p), b) => {
            if p.backup_lba != last_lba {
                issues.push(format!(
                    "backup GPT header expected at LBA {} but the device ends at LBA {last_lba} \
                     (image of a different size, or wrong reported capacity)",
                    p.backup_lba
                ));
            } else if b.is_none() {
                issues.push("backup GPT header is missing or damaged".to_string());
            }
            p
        }
        (None, Some(b)) => {
            issues.push("primary GPT header is damaged, using the backup".to_string());
            b
        }
        (None, None) => return None,
    };

    if header.entry_size < 128 || header.count == 0 || header.count > 4096 {
        issues.push("GPT header declares an implausible partition array".to_string());
        return Some(PartitionTable { kind: TableKind::Gpt, disk_id: header.disk_guid, partitions: vec![], issues });
    }

    let mut partitions = vec![];
    let total = header.count * header.entry_size;
    match read_unaligned(dev, header.entries_lba * ss, total) {
        Err(e) => issues.push(format!("cannot read the GPT partition array: {e}")),
        Ok(array) => {
            if crc32fast::hash(&array) != header.entries_crc {
                issues.push("GPT partition array checksum mismatch".to_string());
            }
            for (i, e) in array.chunks_exact(header.entry_size).enumerate() {
                if e[..16].iter().all(|&b| b == 0) {
                    continue;
                }
                let guid = format_guid(&e[..16]);
                let first = u64::from_le_bytes(e[32..40].try_into().unwrap_or_default());
                let last = u64::from_le_bytes(e[40..48].try_into().unwrap_or_default());
                let name: Vec<u16> = e[56..128]
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|c| u16::from_le_bytes([c[0], c[1]]))
                    .take_while(|&c| c != 0)
                    .collect();
                if last < first {
                    issues.push(format!("partition {} has an invalid extent", i + 1));
                    continue;
                }
                if last > last_lba {
                    issues.push(format!("partition {} ends beyond the end of the device", i + 1));
                }
                partitions.push(Partition {
                    index: i as u32 + 1,
                    start: first * ss,
                    size: (last - first + 1) * ss,
                    type_name: type_name(&guid).to_string(),
                    type_id: guid,
                    name: String::from_utf16_lossy(&name),
                    bootable: false,
                });
            }
        }
    }
    Some(PartitionTable { kind: TableKind::Gpt, disk_id: header.disk_guid, partitions, issues })
}

/// Number of sectors the partition array occupies.
pub fn array_sectors(ss: u64) -> u64 {
    ((ENTRY_COUNT * ENTRY_SIZE) as u64).div_ceil(ss)
}

/// First byte offset after the usable area, i.e. where the backup structures start.
pub fn usable_end(device_size: u64, ss: u64) -> u64 {
    device_size / ss * ss - (array_sectors(ss) + 1) * ss
}

/// Writes a protective MBR and primary and backup GPT with one partition.
pub fn write_single(
    dev: &mut dyn BlockDevice,
    disk_guid: [u8; 16],
    part_guid: [u8; 16],
    type_guid: [u8; 16],
    name: &str,
    start: u64,
    size: u64,
) -> io::Result<()> {
    let ss = dev.sector_size() as u64;
    let last_lba = dev.size() / ss - 1;
    let array_len = array_sectors(ss);
    let first_usable = 2 + array_len;
    let last_usable = last_lba - array_len - 1;

    let mut array = vec![0u8; (array_len * ss) as usize];
    let e = &mut array[..ENTRY_SIZE];
    e[..16].copy_from_slice(&type_guid);
    e[16..32].copy_from_slice(&part_guid);
    e[32..40].copy_from_slice(&(start / ss).to_le_bytes());
    e[40..48].copy_from_slice(&((start + size) / ss - 1).to_le_bytes());
    for (i, c) in name.encode_utf16().take(36).enumerate() {
        e[56 + i * 2..58 + i * 2].copy_from_slice(&c.to_le_bytes());
    }
    let array_crc = crc32fast::hash(&array[..ENTRY_COUNT * ENTRY_SIZE]);

    let make_header = |current: u64, backup: u64, entries: u64| {
        let mut h = vec![0u8; ss as usize];
        h[..8].copy_from_slice(SIGNATURE);
        h[8..12].copy_from_slice(&0x0001_0000u32.to_le_bytes());
        h[12..16].copy_from_slice(&92u32.to_le_bytes());
        h[24..32].copy_from_slice(&current.to_le_bytes());
        h[32..40].copy_from_slice(&backup.to_le_bytes());
        h[40..48].copy_from_slice(&first_usable.to_le_bytes());
        h[48..56].copy_from_slice(&last_usable.to_le_bytes());
        h[56..72].copy_from_slice(&disk_guid);
        h[72..80].copy_from_slice(&entries.to_le_bytes());
        h[80..84].copy_from_slice(&(ENTRY_COUNT as u32).to_le_bytes());
        h[84..88].copy_from_slice(&(ENTRY_SIZE as u32).to_le_bytes());
        h[88..92].copy_from_slice(&array_crc.to_le_bytes());
        let crc = crc32fast::hash(&h[..92]);
        h[16..20].copy_from_slice(&crc.to_le_bytes());
        h
    };

    // The backup goes first: if the stick dies midway, no valid primary claims
    // that a complete table exists.
    let aligned = AlignedBuf::from_slice(&array);
    dev.write_at((last_lba - array_len) * ss, &aligned)?;
    write_sectors(dev, last_lba * ss, &make_header(last_lba, 1, last_lba - array_len))?;
    dev.write_at(2 * ss, &aligned)?;
    write_sectors(dev, ss, &make_header(1, last_lba, 2))?;

    // Protective MBR spanning the disk; clamped for disks beyond the MBR limit.
    let pmbr_len = last_lba.min(u32::MAX as u64);
    mbr::write_single(dev, 0, 0xEE, ss, pmbr_len * ss)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::device::FileDevice;

    #[test]
    fn guid_round_trip() {
        let g = parse_guid(BASIC_DATA).unwrap();
        assert_eq!(format_guid(&g), BASIC_DATA);
        // Known on-disk layout of the basic data GUID.
        assert_eq!(&g[..4], &[0xA2, 0xA0, 0xD0, 0xEB]);
    }

    fn write_sample(path: &std::path::Path, ss: u32) -> FileDevice {
        let mut d = FileDevice::create(path, 32 << 20, ss).unwrap();
        let g = parse_guid(BASIC_DATA).unwrap();
        write_single(&mut d, [1; 16], [2; 16], g, "USB", 1 << 20, 16 << 20).unwrap();
        d
    }

    #[test]
    fn round_trip_512_and_4k() {
        for ss in [512, 4096] {
            let p = std::env::temp_dir().join(format!("usbflash-gpt-{}-{ss}", std::process::id()));
            let mut d = write_sample(&p, ss);
            let t = crate::partition::read_table(&mut d);
            assert_eq!(t.kind, TableKind::Gpt);
            assert_eq!(t.issues, Vec::<String>::new());
            assert_eq!(t.partitions.len(), 1);
            assert_eq!(t.partitions[0].start, 1 << 20);
            assert_eq!(t.partitions[0].size, 16 << 20);
            assert_eq!(t.partitions[0].name, "USB");
            assert_eq!(t.partitions[0].type_name, "Microsoft basic data");
            drop(d);
            std::fs::remove_file(p).ok();
        }
    }

    #[test]
    fn falls_back_to_backup_header() {
        let p = std::env::temp_dir().join(format!("usbflash-gpt-bk-{}", std::process::id()));
        let mut d = write_sample(&p, 512);
        write_sectors(&mut d, 512, &[0u8; 512]).unwrap();
        let t = crate::partition::read_table(&mut d);
        assert_eq!(t.kind, TableKind::Gpt);
        assert_eq!(t.partitions.len(), 1);
        assert!(t.issues.iter().any(|i| i.contains("primary")));
        drop(d);
        std::fs::remove_file(p).ok();
    }
}
