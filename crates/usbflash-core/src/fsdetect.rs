// SPDX-License-Identifier: GPL-3.0-or-later
//! Filesystem and container detection by on-disk signature.
//!
//! Works without any OS support for the filesystem, so a stick that Windows only
//! offers to format can still be identified.

use crate::device::{BlockDevice, read_unaligned};
use serde::Serialize;

/// Bytes needed from the start of a volume to find all supported signatures.
const PROBE_LEN: usize = 136 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FsInfo {
    /// e.g. `FAT32`, `ext4`, `LUKS2`.
    pub name: String,
    pub label: String,
    pub uuid: String,
    /// The content is encrypted; it cannot be inspected without the key.
    pub encrypted: bool,
    /// Windows can mount this without additional drivers.
    pub windows_native: bool,
}

fn le16(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([b[o], b[o + 1]])
}
fn le32(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}
fn be32(b: &[u8], o: usize) -> u32 {
    u32::from_be_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}

fn text(b: &[u8]) -> String {
    let end = b.iter().position(|&c| c == 0).unwrap_or(b.len());
    String::from_utf8_lossy(&b[..end]).trim().to_string()
}

fn hex_uuid(b: &[u8]) -> String {
    let h: Vec<String> = b.iter().map(|x| format!("{x:02x}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        h[0..4].concat(),
        h[4..6].concat(),
        h[6..8].concat(),
        h[8..10].concat(),
        h[10..16].concat()
    )
}

fn fs(name: &str) -> FsInfo {
    FsInfo {
        name: name.to_string(),
        label: String::new(),
        uuid: String::new(),
        encrypted: false,
        windows_native: false,
    }
}

fn native(name: &str) -> FsInfo {
    FsInfo { windows_native: true, ..fs(name) }
}

fn encrypted(name: &str) -> FsInfo {
    FsInfo { encrypted: true, ..fs(name) }
}

/// True if the sector is a volume boot record of a DOS-family filesystem rather than an MBR.
pub fn looks_like_boot_sector(s: &[u8]) -> bool {
    &s[3..11] == b"NTFS    " || &s[3..11] == b"EXFAT   " || &s[3..11] == b"-FVE-FS-" || fat_type(s).is_some()
}

/// Validates a BIOS parameter block and derives the FAT type from the cluster count.
fn fat_type(s: &[u8]) -> Option<&'static str> {
    if s[510] != 0x55 || s[511] != 0xAA || !matches!(s[0], 0xEB | 0xE9) {
        return None;
    }
    let bytes_per_sec = le16(s, 11) as u64;
    let sec_per_clus = s[13] as u64;
    let reserved = le16(s, 14) as u64;
    let fats = s[16] as u64;
    let root_entries = le16(s, 17) as u64;
    if !matches!(bytes_per_sec, 512 | 1024 | 2048 | 4096)
        || !sec_per_clus.is_power_of_two()
        || reserved == 0
        || !(1..=2).contains(&fats)
    {
        return None;
    }
    let fat_size = match le16(s, 22) as u64 {
        0 => le32(s, 36) as u64,
        n => n,
    };
    let total = match le16(s, 19) as u64 {
        0 => le32(s, 32) as u64,
        n => n,
    };
    if fat_size == 0 || total == 0 {
        return None;
    }
    let root_sectors = (root_entries * 32).div_ceil(bytes_per_sec);
    let data_start = reserved + fats * fat_size + root_sectors;
    let clusters = total.checked_sub(data_start)? / sec_per_clus;
    Some(if clusters < 4085 {
        "FAT12"
    } else if clusters < 65525 {
        "FAT16"
    } else {
        "FAT32"
    })
}

/// Detects the filesystem in a buffer holding the start of a volume.
/// The buffer may be shorter than the probe range; signatures beyond it are skipped.
pub fn detect_in_buffer(b: &[u8]) -> Option<FsInfo> {
    let at = |o: usize, m: &[u8]| b.len() >= o + m.len() && &b[o..o + m.len()] == m;

    // Encrypted containers first: their headers are unambiguous.
    if at(0, b"LUKS\xBA\xBE") && b.len() >= 208 {
        let v = u16::from_be_bytes([b[6], b[7]]);
        return Some(FsInfo { uuid: text(&b[168..208]), ..encrypted(&format!("LUKS{v}")) });
    }
    if at(3, b"-FVE-FS-") {
        return Some(FsInfo { windows_native: true, ..encrypted("BitLocker") });
    }

    // DOS family: an identifier at offset 3.
    if b.len() >= 512 {
        if at(3, b"NTFS    ") {
            return Some(FsInfo {
                uuid: format!("{:016X}", u64::from_le_bytes(b[0x48..0x50].try_into().ok()?)),
                ..native("NTFS")
            });
        }
        if at(3, b"EXFAT   ") {
            let id = le32(b, 100);
            return Some(FsInfo { uuid: format!("{:04X}-{:04X}", id >> 16, id & 0xFFFF), ..native("exFAT") });
        }
        if at(3, b"ReFS\0\0\0\0") {
            return Some(native("ReFS"));
        }
    }

    let simple: &[(usize, &[u8], &str)] = &[
        (0, b"XFSB", "XFS"),
        (0, b"hsqs", "SquashFS"),
        (32, b"NXSB", "APFS"),
        (0, b"\x45\x3D\xCD\x28", "CramFS"),
        (32768, b"JFS1", "JFS"),
        (65588, b"ReIsEr2Fs", "ReiserFS 3.6"),
        (65588, b"ReIsEr3Fs", "ReiserFS (journal relocated)"),
        (65588, b"ReIsErFs", "ReiserFS 3.5"),
    ];
    for &(o, m, name) in simple {
        if at(o, m) {
            let mut f = fs(name);
            if name == "XFS" && b.len() >= 120 {
                f.uuid = hex_uuid(&b[32..48]);
                f.label = text(&b[108..120]);
            }
            return Some(f);
        }
    }

    if at(65536 + 64, b"_BHRfS_M") && b.len() >= 65536 + 555 {
        return Some(FsInfo {
            uuid: hex_uuid(&b[65536 + 32..65536 + 48]),
            label: text(&b[65536 + 299..65536 + 555]),
            ..fs("Btrfs")
        });
    }
    const BCACHEFS: [u8; 16] =
        [0xc6, 0x85, 0x73, 0xf6, 0x66, 0xce, 0x90, 0xa9, 0xd9, 0x6a, 0x60, 0xcf, 0x80, 0x3d, 0xf7, 0xef];
    if at(4096 + 24, &BCACHEFS) {
        return Some(fs("bcachefs"));
    }
    if b.len() >= 2048 {
        // ext2/3/4 superblock at 1024.
        if le16(b, 1024 + 56) == 0xEF53 {
            let compat = le32(b, 1024 + 92);
            let incompat = le32(b, 1024 + 96);
            let ro_compat = le32(b, 1024 + 100);
            // extents, 64bit, flex_bg, huge_file, dir_nlink, extra_isize
            let ext4 = incompat & (0x40 | 0x80 | 0x200) != 0 || ro_compat & (0x8 | 0x20 | 0x40) != 0;
            let name = if ext4 {
                "ext4"
            } else if compat & 0x4 != 0 {
                "ext3"
            } else {
                "ext2"
            };
            return Some(FsInfo {
                uuid: hex_uuid(&b[1024 + 104..1024 + 120]),
                label: text(&b[1024 + 120..1024 + 136]),
                ..fs(name)
            });
        }
        if le32(b, 1024) == 0xF2F5_2010 {
            return Some(fs("F2FS"));
        }
        if le16(b, 1024 + 6) == 0x3434 {
            return Some(fs("NILFS2"));
        }
        if at(1024, b"H+") {
            return Some(fs("HFS+"));
        }
        if at(1024, b"HX") {
            return Some(fs("HFSX"));
        }
        // HFS: "BD" plus a plausible allocation block size, to reduce false positives.
        if at(1024, b"BD") && be32(b, 1024 + 20) != 0 && be32(b, 1024 + 20).is_power_of_two() {
            return Some(fs("HFS"));
        }
        if matches!(le16(b, 1024 + 16), 0x137F | 0x138F | 0x2468 | 0x2478) {
            return Some(fs("Minix"));
        }
    }
    if at(512, b"LABELONE") && at(512 + 24, b"LVM2 001") {
        return Some(fs("LVM2 physical volume"));
    }
    if b.len() >= 4100 && (le32(b, 0) == 0xA92B_4EFC || le32(b, 4096) == 0xA92B_4EFC) {
        return Some(fs("Linux RAID member"));
    }
    if at(4086, b"SWAPSPACE2") || at(4086, b"SWAP-SPACE") {
        return Some(FsInfo { label: text(&b[1052..1068]), ..fs("Linux swap") });
    }
    if b.len() >= 8192 + 1376 && (le32(b, 8192 + 1372) == 0x0001_1954 || be32(b, 8192 + 1372) == 0x0001_1954) {
        return Some(fs("UFS1"));
    }
    if b.len() >= 65536 + 1376 && (le32(b, 65536 + 1372) == 0x1954_0119 || be32(b, 65536 + 1372) == 0x1954_0119) {
        return Some(fs("UFS2"));
    }

    // Optical formats, also found on bootable "hybrid" ISO sticks.
    if at(32769, b"CD001") && b.len() >= 32768 + 72 {
        return Some(FsInfo { label: text(&b[32768 + 40..32768 + 72]), ..native("ISO 9660") });
    }
    if at(32769, b"BEA01") || at(32769, b"NSR02") || at(32769, b"NSR03") {
        return Some(native("UDF"));
    }

    // ZFS uberblock array at 128 KiB.
    if b.len() >= 128 * 1024 + 8 {
        let m = &b[128 * 1024..128 * 1024 + 8];
        if m == 0x00BA_B10Cu64.to_le_bytes() || m == 0x00BA_B10Cu64.to_be_bytes() {
            return Some(fs("ZFS"));
        }
    }

    // FAT last: it has the weakest signature.
    if b.len() >= 512
        && let Some(kind) = fat_type(b)
    {
        let (id, label) = if kind == "FAT32" { (67, 71) } else { (39, 43) };
        let has_ext = b[id - 1] == 0x29;
        let vid = le32(b, id);
        return Some(FsInfo {
            uuid: if has_ext { format!("{:04X}-{:04X}", vid >> 16, vid & 0xFFFF) } else { String::new() },
            label: if has_ext {
                let l = text(&b[label..label + 11]);
                if l == "NO NAME" { String::new() } else { l }
            } else {
                String::new()
            },
            ..native(kind)
        });
    }
    None
}

/// Probes a volume. Unreadable parts of the probe range are treated as zeros,
/// so one bad sector does not hide a signature elsewhere.
pub fn probe(dev: &mut dyn BlockDevice, start: u64, size: u64) -> Option<FsInfo> {
    let ss = dev.sector_size() as u64;
    let end = (start + size).min(dev.size());
    let len = (end.saturating_sub(start)).min(PROBE_LEN as u64) / ss * ss;
    if len == 0 {
        return None;
    }
    let mut data = vec![0u8; len as usize];
    let piece = 4096u64.max(ss);
    let mut pos = 0;
    while pos < len {
        let n = piece.min(len - pos);
        if let Ok(chunk) = read_unaligned(dev, start + pos, n as usize) {
            data[pos as usize..(pos + n) as usize].copy_from_slice(&chunk);
        }
        pos += n;
    }
    let mut info = detect_in_buffer(&data)?;
    if info.name == "exFAT" {
        info.label = exfat_label(dev, start, &data).unwrap_or_default();
    }
    Some(info)
}

/// exFAT stores the label as a directory entry in the root directory.
fn exfat_label(dev: &mut dyn BlockDevice, start: u64, boot: &[u8]) -> Option<String> {
    if boot[108] > 12 || boot[109] > 25 {
        return None;
    }
    let ss = 1u64 << boot[108];
    let spc = 1u64 << boot[109];
    let heap = le32(boot, 88) as u64;
    let root = le32(boot, 96) as u64;
    let offset = start + (heap + (root.checked_sub(2)?) * spc) * ss;
    let dir = read_unaligned(dev, offset, ss as usize).ok()?;
    for e in dir.as_chunks::<32>().0 {
        if e[0] == 0 {
            return None;
        }
        if e[0] == 0x83 {
            let n = (e[1] as usize).min(11);
            let u: Vec<u16> =
                e[2..2 + n * 2].as_chunks::<2>().0.iter().map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
            return Some(String::from_utf16_lossy(&u));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn buf() -> Vec<u8> {
        vec![0u8; PROBE_LEN]
    }

    fn put(b: &mut [u8], o: usize, d: &[u8]) {
        b[o..o + d.len()].copy_from_slice(d);
    }

    fn name(b: &[u8]) -> Option<String> {
        detect_in_buffer(b).map(|f| f.name)
    }

    #[test]
    fn empty_is_unknown() {
        assert_eq!(name(&buf()), None);
    }

    #[test]
    fn detects_simple_magics() {
        let cases: &[(usize, &[u8], &str)] = &[
            (0, b"XFSB", "XFS"),
            (0, b"hsqs", "SquashFS"),
            (32, b"NXSB", "APFS"),
            (3, b"-FVE-FS-", "BitLocker"),
            (3, b"NTFS    ", "NTFS"),
            (3, b"EXFAT   ", "exFAT"),
            (3, b"ReFS\0\0\0\0", "ReFS"),
            (32768, b"JFS1", "JFS"),
            (65536 + 64, b"_BHRfS_M", "Btrfs"),
            (65588, b"ReIsEr2Fs", "ReiserFS 3.6"),
            (1024, b"H+", "HFS+"),
            (1024, b"HX", "HFSX"),
            (32769, b"CD001", "ISO 9660"),
            (32769, b"NSR02", "UDF"),
            (4086, b"SWAPSPACE2", "Linux swap"),
            (512, b"LABELONE", ""),
        ];
        for &(o, m, n) in cases {
            if n.is_empty() {
                continue;
            }
            let mut b = buf();
            put(&mut b, o, m);
            assert_eq!(name(&b).as_deref(), Some(n), "signature {n}");
        }
    }

    #[test]
    fn luks_reports_version_and_is_encrypted() {
        let mut b = buf();
        put(&mut b, 0, b"LUKS\xBA\xBE\x00\x02");
        put(&mut b, 168, b"abc-uuid");
        let f = detect_in_buffer(&b).unwrap();
        assert_eq!(f.name, "LUKS2");
        assert_eq!(f.uuid, "abc-uuid");
        assert!(f.encrypted);
    }

    #[test]
    fn ext_generations_are_told_apart() {
        let mut b = buf();
        put(&mut b, 1024 + 56, &0xEF53u16.to_le_bytes());
        put(&mut b, 1024 + 120, b"mylabel");
        let f = detect_in_buffer(&b).unwrap();
        assert_eq!((f.name.as_str(), f.label.as_str()), ("ext2", "mylabel"));
        put(&mut b, 1024 + 92, &4u32.to_le_bytes());
        assert_eq!(name(&b).as_deref(), Some("ext3"));
        put(&mut b, 1024 + 96, &0x40u32.to_le_bytes());
        assert_eq!(name(&b).as_deref(), Some("ext4"));
    }

    #[test]
    fn zfs_in_both_byte_orders() {
        for m in [0x00BA_B10Cu64.to_le_bytes(), 0x00BA_B10Cu64.to_be_bytes()] {
            let mut b = buf();
            put(&mut b, 128 * 1024, &m);
            assert_eq!(name(&b).as_deref(), Some("ZFS"));
        }
    }

    #[test]
    fn short_buffers_do_not_panic() {
        for len in [0, 1, 100, 511, 512, 1024, 2048, 4096, 8192, 40000, 66000] {
            assert_eq!(detect_in_buffer(&vec![0u8; len]), None, "len {len}");
        }
    }
}
