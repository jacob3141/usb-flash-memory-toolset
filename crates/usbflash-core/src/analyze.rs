// SPDX-License-Identifier: GPL-3.0-or-later
//! Read-only analysis of a device: layout, filesystems and what the data looks like.

use crate::device::{AlignedBuf, BlockDevice};
use crate::error::Result;
use crate::fsdetect::{self, FsInfo};
use crate::partition::{self, PartitionTable, TableKind};
use crate::progress::Ctx;
use serde::Serialize;

const SAMPLE_COUNT: u64 = 256;
const SAMPLE_SIZE: usize = 64 * 1024;
/// Bits per byte above which a sample counts as random, encrypted or compressed.
const HIGH_ENTROPY: f64 = 7.9;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ContentKind {
    /// All samples read as zeros or 0xFF: the device looks erased.
    Blank,
    /// Nearly all samples are indistinguishable from random data.
    RandomOrEncrypted,
    /// Regular data was found.
    Data,
    /// Most samples could not be read.
    Unreadable,
}

impl ContentKind {
    pub fn label(self) -> &'static str {
        match self {
            ContentKind::Blank => "Blank (erased)",
            ContentKind::RandomOrEncrypted => "Random or encrypted data",
            ContentKind::Data => "Contains data",
            ContentKind::Unreadable => "Unreadable",
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ContentStats {
    pub samples: u32,
    pub blank: u32,
    pub high_entropy: u32,
    pub data: u32,
    pub unreadable: u32,
}

#[derive(Debug, Clone, Serialize)]
pub struct PartitionReport {
    #[serde(flatten)]
    pub partition: partition::Partition,
    pub filesystem: Option<FsInfo>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Analysis {
    pub size_bytes: u64,
    pub sector_size: u32,
    pub table_kind: TableKind,
    pub disk_id: String,
    pub partitions: Vec<PartitionReport>,
    /// Filesystem found at the very start of the device (no partition table).
    pub whole_device_fs: Option<FsInfo>,
    pub content: ContentKind,
    pub stats: ContentStats,
    pub findings: Vec<String>,
}

impl Analysis {
    /// One line description of the filesystems, for lists and summaries.
    pub fn fs_summary(&self) -> String {
        let mut names: Vec<String> =
            self.partitions.iter().filter_map(|p| p.filesystem.as_ref().map(|f| f.name.clone())).collect();
        if let Some(f) = &self.whole_device_fs {
            names.insert(0, f.name.clone());
        }
        names.dedup();
        if names.is_empty() { "no filesystem".to_string() } else { names.join(", ") }
    }
}

/// Shannon entropy of a buffer in bits per byte.
pub fn entropy(data: &[u8]) -> f64 {
    if data.is_empty() {
        return 0.0;
    }
    let mut counts = [0u64; 256];
    for &b in data {
        counts[b as usize] += 1;
    }
    let n = data.len() as f64;
    counts
        .iter()
        .filter(|&&c| c > 0)
        .map(|&c| {
            let p = c as f64 / n;
            -p * p.log2()
        })
        .sum()
}

fn is_uniform(data: &[u8]) -> bool {
    data.iter().all(|&b| b == data[0])
}

/// Samples evenly over the device to characterize the content.
pub fn sample_content(dev: &mut dyn BlockDevice, ctx: &mut Ctx) -> Result<ContentStats> {
    let ss = dev.sector_size() as u64;
    let size = dev.size();
    let mut stats = ContentStats::default();
    let sample = (SAMPLE_SIZE as u64).max(ss).min(size / ss * ss);
    if sample == 0 {
        return Ok(stats);
    }
    let mut buf = AlignedBuf::new(sample as usize);
    let count = SAMPLE_COUNT.min(size / sample).max(1);
    ctx.begin_phase("Sampling content", count * sample);
    for i in 0..count {
        ctx.check_cancel()?;
        let offset = crate::sweep::spaced(size / ss * ss - sample, count, i, ss);
        stats.samples += 1;
        match dev.read_at(offset, &mut buf) {
            Err(_) => stats.unreadable += 1,
            Ok(()) => {
                if is_uniform(&buf) && matches!(buf[0], 0x00 | 0xFF) {
                    stats.blank += 1;
                } else if entropy(&buf) >= HIGH_ENTROPY {
                    stats.high_entropy += 1;
                } else {
                    stats.data += 1;
                }
            }
        }
        ctx.update((i + 1) * sample, stats.unreadable as u64 * sample);
    }
    Ok(stats)
}

pub fn classify(s: &ContentStats) -> ContentKind {
    let n = s.samples.max(1);
    if s.unreadable * 2 > n {
        ContentKind::Unreadable
    } else if s.data == 0 && s.high_entropy == 0 {
        ContentKind::Blank
    } else if s.high_entropy * 10 >= (n - s.unreadable) * 9 {
        ContentKind::RandomOrEncrypted
    } else {
        ContentKind::Data
    }
}

fn fs_findings(f: &FsInfo, out: &mut Vec<String>) {
    if f.encrypted {
        out.push(format!("{} encrypted volume: contents cannot be inspected without the key", f.name));
    } else if !f.windows_native {
        out.push(format!("{}: not readable on Windows without additional drivers", f.name));
    }
}

fn findings(
    table: &PartitionTable,
    parts: &[PartitionReport],
    whole: &Option<FsInfo>,
    kind: ContentKind,
) -> Vec<String> {
    let mut out = table.issues.clone();
    if let Some(f) = whole {
        if table.kind == TableKind::None {
            out.push(format!("{} directly on the device, without a partition table", f.name));
        }
        fs_findings(f, &mut out);
    }
    for p in parts {
        match &p.filesystem {
            Some(f) => fs_findings(f, &mut out),
            None => out.push(format!(
                "Partition {} has no recognizable filesystem (damaged, or VeraCrypt-style encryption)",
                p.partition.index
            )),
        }
    }
    match kind {
        ContentKind::Blank => {
            if table.kind == TableKind::None && whole.is_none() {
                out.push("No traces of data in the sampled areas; this is a spot check, not proof".to_string());
            }
        }
        ContentKind::RandomOrEncrypted => {
            if whole.is_none() && parts.iter().all(|p| p.filesystem.is_none()) {
                out.push(
                    "Random-looking data without any header: a previous wipe, or a VeraCrypt-style container"
                        .to_string(),
                );
            }
        }
        ContentKind::Data => out.push("Contains data: wipe before giving this device away".to_string()),
        ContentKind::Unreadable => out.push("Most of the sampled areas could not be read".to_string()),
    }
    out
}

/// Runs the complete read-only analysis.
pub fn analyze(dev: &mut dyn BlockDevice, ctx: &mut Ctx) -> Result<Analysis> {
    ctx.begin_phase("Reading partition table", 0);
    let table = partition::read_table(dev);
    let size = dev.size();
    let mut partitions = vec![];
    for p in &table.partitions {
        ctx.check_cancel()?;
        let filesystem = fsdetect::probe(dev, p.start, p.size);
        partitions.push(PartitionReport { partition: p.clone(), filesystem });
    }
    // Hybrid ISOs carry both a partition table and a filesystem at offset 0.
    let whole_device_fs = if matches!(table.kind, TableKind::None | TableKind::Mbr) {
        fsdetect::probe(dev, 0, size).filter(|f| table.kind == TableKind::None || f.name == "ISO 9660")
    } else {
        None
    };
    let stats = sample_content(dev, ctx)?;
    let content = classify(&stats);
    let findings = findings(&table, &partitions, &whole_device_fs, content);
    Ok(Analysis {
        size_bytes: size,
        sector_size: dev.sector_size(),
        table_kind: table.kind,
        disk_id: table.disk_id,
        partitions,
        whole_device_fs,
        content,
        stats,
        findings,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entropy_extremes() {
        assert_eq!(entropy(&[0u8; 100]), 0.0);
        let all: Vec<u8> = (0..=255).collect();
        assert!((entropy(&all) - 8.0).abs() < 1e-9);
    }

    fn stats(blank: u32, high: u32, data: u32, bad: u32) -> ContentStats {
        ContentStats { samples: blank + high + data + bad, blank, high_entropy: high, data, unreadable: bad }
    }

    #[test]
    fn classification() {
        assert_eq!(classify(&stats(256, 0, 0, 0)), ContentKind::Blank);
        assert_eq!(classify(&stats(0, 256, 0, 0)), ContentKind::RandomOrEncrypted);
        assert_eq!(classify(&stats(200, 40, 16, 0)), ContentKind::Data);
        assert_eq!(classify(&stats(255, 0, 1, 0)), ContentKind::Data);
        assert_eq!(classify(&stats(0, 0, 0, 256)), ContentKind::Unreadable);
    }
}
