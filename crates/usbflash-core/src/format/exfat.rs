// SPDX-License-Identifier: GPL-3.0-or-later
//! exFAT formatter following Microsoft's "exFAT file system specification".

use super::FormatInfo;
use crate::device::{BlockDevice, write_sectors, zero_range};
use crate::error::{Error, Result};
use crate::pattern::os_random;
use crate::progress::Ctx;

const BOOT_REGION_SECTORS: u64 = 12;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Layout {
    pub sector_size: u64,
    pub total_sectors: u64,
    pub sec_per_cluster: u64,
    pub fat_offset: u64,
    pub fat_sectors: u64,
    pub heap_offset: u64,
    pub clusters: u64,
}

fn default_cluster_bytes(bytes: u64, ss: u64) -> u64 {
    const MIB: u64 = 1024 * 1024;
    const GIB: u64 = 1024 * MIB;
    let c = match bytes {
        b if b <= 256 * MIB => 4096,
        b if b <= 32 * GIB => 32 * 1024,
        _ => 128 * 1024,
    };
    u64::max(c, ss)
}

/// Chooses the layout of a volume with `total` sectors of size `ss`.
pub fn plan(ss: u64, total: u64) -> Result<Layout> {
    let bytes = ss * total;
    if bytes < 1024 * 1024 {
        return Err(Error::Invalid("exFAT requires at least 1 MiB".into()));
    }
    let cluster = default_cluster_bytes(bytes, ss);
    let spc = cluster / ss;
    // The FAT starts behind main and backup boot region. Larger volumes align it
    // to 1 MiB, which keeps metadata away from the first flash erase block.
    let fat_offset = if bytes >= 64 * 1024 * 1024 {
        (1024 * 1024 / ss).max(2 * BOOT_REGION_SECTORS)
    } else {
        2 * BOOT_REGION_SECTORS
    };
    // Grow the FAT until it covers all clusters of the remaining heap.
    let mut fat_sectors = 0u64;
    loop {
        let heap_offset = (fat_offset + fat_sectors).div_ceil(spc) * spc;
        let clusters = total
            .checked_sub(heap_offset)
            .map(|h| h / spc)
            .filter(|&c| c > 8)
            .ok_or_else(|| Error::Invalid("volume too small for exFAT".into()))?;
        let needed = ((clusters + 2) * 4).div_ceil(ss);
        if needed <= fat_sectors {
            if clusters >= 0xFFFF_FFF5 {
                return Err(Error::Invalid("volume too large for exFAT at this cluster size".into()));
            }
            return Ok(Layout {
                sector_size: ss,
                total_sectors: total,
                sec_per_cluster: spc,
                fat_offset,
                fat_sectors,
                heap_offset,
                clusters,
            });
        }
        fat_sectors = needed;
    }
}

/// Rotating 32 bit checksum used by the boot region and the up-case table.
fn checksum_add(sum: u32, byte: u8) -> u32 {
    sum.rotate_right(1).wrapping_add(byte as u32)
}

/// Builds the compressed up-case table.
///
/// The specification requires a correct mapping for the first 128 characters; the
/// rest is derived from Unicode simple case mappings. Non-ASCII characters whose
/// uppercase is ASCII (dotless i, long s) stay unmapped to keep names distinct.
pub fn upcase_table() -> Vec<u8> {
    let map = |u: u16| -> u16 {
        let Some(c) = char::from_u32(u as u32) else { return u };
        let mut up = c.to_uppercase();
        match (up.next(), up.next()) {
            (Some(x), None) if (x as u32) <= 0xFFFF && (u < 128 || !x.is_ascii()) => x as u16,
            _ => u,
        }
    };
    let mut out = vec![];
    let push = |out: &mut Vec<u8>, v: u16| out.extend_from_slice(&v.to_le_bytes());
    let mut c: u32 = 0;
    while c <= 0xFFFF {
        let u = c as u16;
        if map(u) == u {
            // Identity run, stored as 0xFFFF followed by the run length.
            let mut run = 1u32;
            while c + run <= 0xFFFF && map((c + run) as u16) == (c + run) as u16 {
                run += 1;
            }
            if (3..=0xFFFF).contains(&run) {
                push(&mut out, 0xFFFF);
                push(&mut out, run as u16);
                c += run;
                continue;
            }
            // Short runs are shorter as plain entries. U+FFFF itself would be read
            // as the run marker, so it is always written as a run.
            if u == 0xFFFF {
                push(&mut out, 0xFFFF);
                push(&mut out, 1);
                c += 1;
                continue;
            }
        }
        push(&mut out, map(u));
        c += 1;
    }
    out
}

fn dir_entries(label: &str, bitmap: (u32, u64), upcase: (u32, u64, u32)) -> (Vec<u8>, String) {
    let units: Vec<u16> = label.encode_utf16().take(11).collect();
    let mut d = vec![];
    if !units.is_empty() {
        let mut e = [0u8; 32];
        e[0] = 0x83;
        e[1] = units.len() as u8;
        for (i, u) in units.iter().enumerate() {
            e[2 + i * 2..4 + i * 2].copy_from_slice(&u.to_le_bytes());
        }
        d.extend_from_slice(&e);
    }
    let mut e = [0u8; 32];
    e[0] = 0x81;
    e[20..24].copy_from_slice(&bitmap.0.to_le_bytes());
    e[24..32].copy_from_slice(&bitmap.1.to_le_bytes());
    d.extend_from_slice(&e);
    let mut e = [0u8; 32];
    e[0] = 0x82;
    e[4..8].copy_from_slice(&upcase.2.to_le_bytes());
    e[20..24].copy_from_slice(&upcase.0.to_le_bytes());
    e[24..32].copy_from_slice(&upcase.1.to_le_bytes());
    d.extend_from_slice(&e);
    (d, String::from_utf16_lossy(&units))
}

fn boot_region(l: &Layout, part_offset: u64, serial: u32, root: u32, used: u64) -> Vec<u8> {
    let ss = l.sector_size as usize;
    let mut r = vec![0u8; ss * BOOT_REGION_SECTORS as usize];
    let s = &mut r[..ss];
    s[0..3].copy_from_slice(&[0xEB, 0x76, 0x90]);
    s[3..11].copy_from_slice(b"EXFAT   ");
    s[64..72].copy_from_slice(&part_offset.to_le_bytes());
    s[72..80].copy_from_slice(&l.total_sectors.to_le_bytes());
    s[80..84].copy_from_slice(&(l.fat_offset as u32).to_le_bytes());
    s[84..88].copy_from_slice(&(l.fat_sectors as u32).to_le_bytes());
    s[88..92].copy_from_slice(&(l.heap_offset as u32).to_le_bytes());
    s[92..96].copy_from_slice(&(l.clusters as u32).to_le_bytes());
    s[96..100].copy_from_slice(&root.to_le_bytes());
    s[100..104].copy_from_slice(&serial.to_le_bytes());
    s[104..106].copy_from_slice(&0x0100u16.to_le_bytes());
    s[108] = l.sector_size.trailing_zeros() as u8;
    s[109] = l.sec_per_cluster.trailing_zeros() as u8;
    s[110] = 1;
    s[111] = 0x80;
    s[112] = (used * 100 / l.clusters) as u8;
    s[510] = 0x55;
    s[511] = 0xAA;
    // Extended boot sectors 1..=8 carry only their signature.
    for i in 1..=8 {
        r[(i + 1) * ss - 4..(i + 1) * ss].copy_from_slice(&0xAA55_0000u32.to_le_bytes());
    }
    // Sectors 9 and 10 (OEM parameters, reserved) stay zero. Sector 11 holds the
    // checksum of the first eleven, skipping the mutable fields.
    let mut sum = 0u32;
    for (i, &b) in r[..11 * ss].iter().enumerate() {
        if !matches!(i, 106 | 107 | 112) {
            sum = checksum_add(sum, b);
        }
    }
    for c in r[11 * ss..12 * ss].as_chunks_mut::<4>().0 {
        c.copy_from_slice(&sum.to_le_bytes());
    }
    r
}

/// Formats `start..start + size` of the device.
pub fn format(dev: &mut dyn BlockDevice, start: u64, size: u64, label: &str, ctx: &mut Ctx) -> Result<FormatInfo> {
    let ss = dev.sector_size() as u64;
    let l = plan(ss, size / ss)?;
    let cluster_bytes = l.sec_per_cluster * ss;

    let upcase = upcase_table();
    let upcase_sum = upcase.iter().fold(0u32, |s, &b| checksum_add(s, b));

    // Heap layout: allocation bitmap, up-case table, root directory.
    let bitmap_bytes = l.clusters.div_ceil(8);
    let bitmap_clusters = bitmap_bytes.div_ceil(cluster_bytes);
    let upcase_clusters = (upcase.len() as u64).div_ceil(cluster_bytes);
    let bitmap_first = 2u64;
    let upcase_first = bitmap_first + bitmap_clusters;
    let root_first = upcase_first + upcase_clusters;
    let used = bitmap_clusters + upcase_clusters + 1;
    if used >= l.clusters {
        return Err(Error::Invalid("volume too small for exFAT metadata".into()));
    }

    let mut serial = [0u8; 4];
    os_random(&mut serial)?;
    let cluster_offset = |c: u64| start + (l.heap_offset + (c - 2) * l.sec_per_cluster) * ss;
    let meta_len = (l.heap_offset + used * l.sec_per_cluster) * ss;

    ctx.begin_phase("Creating exFAT filesystem", meta_len);
    ctx.check_cancel()?;
    zero_range(dev, start, meta_len)?;

    // FAT: media descriptor, end marker, then contiguous chains.
    let mut fat = Vec::with_capacity(((used + 2) * 4) as usize);
    fat.extend_from_slice(&0xFFFF_FFF8u32.to_le_bytes());
    fat.extend_from_slice(&0xFFFF_FFFFu32.to_le_bytes());
    for (first, count) in [(bitmap_first, bitmap_clusters), (upcase_first, upcase_clusters), (root_first, 1)] {
        for i in 0..count {
            let next = if i + 1 == count { 0xFFFF_FFFF } else { (first + i + 1) as u32 };
            fat.extend_from_slice(&next.to_le_bytes());
        }
    }
    fat.resize((fat.len() as u64).div_ceil(ss) as usize * ss as usize, 0);
    write_sectors(dev, start + l.fat_offset * ss, &fat)?;

    // Allocation bitmap with the metadata clusters marked used.
    let mut bitmap = vec![0u8; (used.div_ceil(8)).div_ceil(ss) as usize * ss as usize];
    for i in 0..used as usize {
        bitmap[i / 8] |= 1 << (i % 8);
    }
    write_sectors(dev, cluster_offset(bitmap_first), &bitmap)?;

    let mut padded = upcase.clone();
    padded.resize((upcase.len() as u64).div_ceil(ss) as usize * ss as usize, 0);
    write_sectors(dev, cluster_offset(upcase_first), &padded)?;

    let (mut root, label_used) =
        dir_entries(label, (bitmap_first as u32, bitmap_bytes), (upcase_first as u32, upcase.len() as u64, upcase_sum));
    root.resize((root.len() as u64).div_ceil(ss) as usize * ss as usize, 0);
    write_sectors(dev, cluster_offset(root_first), &root)?;

    let boot = boot_region(&l, start / ss, u32::from_le_bytes(serial), root_first as u32, used);
    // Backup first; the main boot region makes the volume valid.
    write_sectors(dev, start + BOOT_REGION_SECTORS * ss, &boot)?;
    write_sectors(dev, start, &boot)?;
    ctx.update(meta_len, 0);

    Ok(FormatInfo {
        filesystem: "exFAT".to_string(),
        cluster_size: cluster_bytes as u32,
        clusters: l.clusters,
        label: label_used,
        mbr_type: 0x07,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fsdetect;
    use crate::testutil::MemDevice;

    const MIB: u64 = 1024 * 1024;
    const GIB: u64 = 1024 * MIB;

    #[test]
    fn layouts_are_consistent() {
        for ss in [512u64, 4096] {
            for bytes in [2 * MIB, 32 * MIB, 256 * MIB, GIB, 32 * GIB, 64 * GIB, 2048 * GIB, 64 * 1024 * GIB] {
                let l = plan(ss, bytes / ss).unwrap();
                assert!(l.fat_offset >= 24);
                assert!(l.fat_offset + l.fat_sectors <= l.heap_offset);
                assert_eq!(l.heap_offset % l.sec_per_cluster, 0);
                assert!(l.heap_offset + l.clusters * l.sec_per_cluster <= l.total_sectors);
                assert!(l.fat_sectors * ss >= (l.clusters + 2) * 4);
            }
        }
    }

    #[test]
    fn rejects_tiny_volumes() {
        assert!(plan(512, 1024).is_err());
    }

    /// Expands the compressed table back to the full 64 K mapping.
    fn expand(table: &[u8]) -> Vec<u16> {
        let v: Vec<u16> = table.as_chunks::<2>().0.iter().map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
        let mut out = vec![];
        let mut i = 0;
        while i < v.len() {
            if v[i] == 0xFFFF {
                let start = out.len() as u32;
                for k in 0..v[i + 1] as u32 {
                    out.push((start + k) as u16);
                }
                i += 2;
            } else {
                out.push(v[i]);
                i += 1;
            }
        }
        out
    }

    #[test]
    fn upcase_table_covers_all_code_units_and_maps_ascii() {
        let m = expand(&upcase_table());
        assert_eq!(m.len(), 65536);
        for c in 0..128u16 {
            let expected = if (b'a' as u16..=b'z' as u16).contains(&c) { c - 32 } else { c };
            assert_eq!(m[c as usize], expected, "U+{c:04X}");
        }
        assert_eq!(m[0xE4], 0xC4, "a-umlaut maps to its uppercase");
        assert_eq!(m[0xDF], 0xDF, "sharp s has no single uppercase");
        assert_eq!(m[0x131], 0x131, "dotless i does not map to ASCII I");
        assert_eq!(m[0xD800], 0xD800, "surrogates are identity");
        assert_eq!(m[0xFFFF], 0xFFFF);
    }

    #[test]
    fn boot_checksum_covers_expected_bytes() {
        let bytes = 64 * MIB;
        let mut d = MemDevice::new(bytes as usize, 512);
        format(&mut d, 0, bytes, "x", &mut Ctx::silent()).unwrap();
        let boot = &d.data[..12 * 512];
        let mut sum = 0u32;
        for (i, &b) in boot[..11 * 512].iter().enumerate() {
            if !matches!(i, 106 | 107 | 112) {
                sum = checksum_add(sum, b);
            }
        }
        assert_eq!(&boot[11 * 512..11 * 512 + 4], &sum.to_le_bytes());
        // Backup region is identical.
        assert_eq!(boot, &d.data[12 * 512..24 * 512]);
    }

    #[test]
    fn formatted_volume_is_detected_with_label() {
        for ss in [512u32, 4096] {
            let bytes = 128 * MIB;
            let mut d = MemDevice::new(bytes as usize, ss);
            let info = format(&mut d, 0, bytes, "Mein Stick", &mut Ctx::silent()).unwrap();
            assert_eq!(info.filesystem, "exFAT");
            let f = fsdetect::probe(&mut d, 0, bytes).unwrap();
            assert_eq!(f.name, "exFAT");
            assert_eq!(f.label, "Mein Stick");
        }
    }

    #[test]
    fn metadata_chain_and_bitmap_are_consistent() {
        let bytes = 512 * MIB;
        let mut d = MemDevice::new(bytes as usize, 512);
        format(&mut d, 0, bytes, "", &mut Ctx::silent()).unwrap();
        let l = plan(512, bytes / 512).unwrap();
        let root_cluster = u32::from_le_bytes(d.data[96..100].try_into().unwrap());
        let fat = (l.fat_offset * 512) as usize;
        let entry = |c: u32| {
            let o = fat + c as usize * 4;
            u32::from_le_bytes(d.data[o..o + 4].try_into().unwrap())
        };
        assert_eq!(entry(0), 0xFFFF_FFF8);
        assert_eq!(entry(root_cluster), 0xFFFF_FFFF, "root directory is one cluster");
        assert_eq!(entry(root_cluster + 1), 0, "first free cluster is unallocated");

        // Exactly the clusters up to and including the root are allocated.
        let heap = (l.heap_offset * 512) as usize;
        let used = root_cluster - 1;
        for c in 0..used + 8 {
            let bit = d.data[heap + c as usize / 8] >> (c % 8) & 1;
            assert_eq!(bit == 1, c < used, "bitmap bit {c}");
        }
    }

    #[test]
    fn label_is_limited_to_eleven_units() {
        let bytes = 8 * MIB;
        let mut d = MemDevice::new(bytes as usize, 512);
        let i = format(&mut d, 0, bytes, "0123456789ABCDEF", &mut Ctx::silent()).unwrap();
        assert_eq!(i.label, "0123456789A");
    }

    #[test]
    fn works_at_a_partition_offset() {
        let bytes = 64 * MIB;
        let start = MIB;
        let mut d = MemDevice::new((bytes + start) as usize, 512);
        format(&mut d, start, bytes, "off", &mut Ctx::silent()).unwrap();
        let s = start as usize;
        assert_eq!(&d.data[s + 3..s + 11], b"EXFAT   ");
        assert_eq!(u64::from_le_bytes(d.data[s + 64..s + 72].try_into().unwrap()), start / 512);
        assert!(d.data[..s].iter().all(|&b| b == 0), "nothing written before the partition");
    }
}
