// SPDX-License-Identifier: GPL-3.0-or-later
//! Non-destructive surface scan: reads the device and measures errors and speed.

use crate::device::{AlignedBuf, BlockDevice, RangeList};
use crate::error::Result;
use crate::progress::Ctx;
use crate::sweep::{Sweep, chunk_size, spaced};
use serde::{Deserialize, Serialize};
use std::time::Instant;

const PROFILE_BUCKETS: usize = 128;
const QUICK_SAMPLES: u64 = 256;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ScanMode {
    /// Reads evenly spread samples, about 256 MiB in total.
    Quick,
    /// Reads every sector.
    Full,
}

#[derive(Debug, Clone, Serialize)]
pub struct ScanReport {
    pub mode: ScanMode,
    pub bytes_read: u64,
    pub unreadable: RangeList,
    pub avg_bytes_per_sec: f64,
    pub min_bytes_per_sec: f64,
    pub max_bytes_per_sec: f64,
    /// Read speed in bytes per second over the device; zero where not sampled.
    pub speed_profile: Vec<f32>,
    pub duration_secs: f64,
    /// The scan stopped early because the device stopped responding.
    pub aborted: bool,
}

/// Reads the device in the chosen mode.
pub fn scan(dev: &mut dyn BlockDevice, mode: ScanMode, ctx: &mut Ctx) -> Result<ScanReport> {
    let ss = dev.sector_size() as u64;
    let size = dev.size() / ss * ss;
    let chunk = (chunk_size(dev.sector_size()) as u64).min(size.max(ss));
    let mut buf = AlignedBuf::new(chunk as usize);
    let mut sweep = Sweep::default();

    let chunks = size / chunk;
    let positions: Box<dyn Iterator<Item = u64>> = match mode {
        ScanMode::Full => Box::new((0..size.div_ceil(chunk)).map(move |i| i * chunk)),
        ScanMode::Quick => {
            let n = QUICK_SAMPLES.min(chunks).max(1);
            let span = size.saturating_sub(chunk);
            Box::new((0..n).map(move |i| spaced(span, n, i, ss)))
        }
    };
    let total = match mode {
        ScanMode::Full => size,
        ScanMode::Quick => QUICK_SAMPLES.min(chunks).max(1) * chunk,
    };

    ctx.begin_phase(if mode == ScanMode::Full { "Full surface scan" } else { "Quick surface scan" }, total);
    let start = Instant::now();
    let mut done = 0u64;
    let mut read_ok = 0u64;
    let mut busy = 0f64;
    let (mut min_speed, mut max_speed) = (f64::INFINITY, 0f64);
    let mut bucket_bytes = vec![0u64; PROFILE_BUCKETS];
    let mut bucket_time = vec![0f64; PROFILE_BUCKETS];
    let mut aborted = false;

    for offset in positions {
        ctx.check_cancel()?;
        let len = chunk.min(size - offset) as usize;
        let t = Instant::now();
        let ok = sweep.read(dev, offset, &mut buf[..len]);
        let dt = t.elapsed().as_secs_f64();
        done += len as u64;
        if ok {
            read_ok += len as u64;
            busy += dt;
            // Very short tails distort throughput figures.
            if len as u64 == chunk && dt > 0.0 {
                let v = len as f64 / dt;
                min_speed = min_speed.min(v);
                max_speed = max_speed.max(v);
            }
            let b = ((offset as u128 * PROFILE_BUCKETS as u128) / size.max(1) as u128) as usize;
            let b = b.min(PROFILE_BUCKETS - 1);
            bucket_bytes[b] += len as u64;
            bucket_time[b] += dt;
        }
        ctx.update(done, sweep.bad.total());
        if sweep.gave_up() {
            aborted = true;
            break;
        }
    }

    let speed_profile = bucket_bytes
        .iter()
        .zip(&bucket_time)
        .map(|(&b, &t)| if t > 0.0 { (b as f64 / t) as f32 } else { 0.0 })
        .collect();

    Ok(ScanReport {
        mode,
        bytes_read: read_ok,
        unreadable: sweep.bad,
        avg_bytes_per_sec: if busy > 0.0 { read_ok as f64 / busy } else { 0.0 },
        min_bytes_per_sec: if min_speed.is_finite() { min_speed } else { 0.0 },
        max_bytes_per_sec: max_speed,
        speed_profile,
        duration_secs: start.elapsed().as_secs_f64(),
        aborted,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::MemDevice;

    #[test]
    fn full_scan_reads_everything_of_a_healthy_device() {
        let mut d = MemDevice::new((3 << 20) + 4096, 512);
        let r = scan(&mut d, ScanMode::Full, &mut Ctx::silent()).unwrap();
        assert_eq!(r.bytes_read, (3 << 20) + 4096);
        assert!(r.unreadable.is_empty());
        assert!(!r.aborted);
        assert_eq!(r.speed_profile.len(), PROFILE_BUCKETS);
    }

    #[test]
    fn bad_area_is_narrowed_down() {
        let mut d = MemDevice::new(4 << 20, 512);
        d.bad.push((1 << 20, (1 << 20) + 512));
        let r = scan(&mut d, ScanMode::Full, &mut Ctx::silent()).unwrap();
        // One 64 KiB retry piece instead of the full 1 MiB chunk.
        assert_eq!(r.unreadable.total(), 64 * 1024);
        assert_eq!(r.unreadable.first_offset(), Some(1 << 20));
    }

    #[test]
    fn dead_device_aborts_instead_of_timing_out_forever() {
        let mut d = MemDevice::new(128 << 20, 512);
        d.bad.push((0, 128 << 20));
        let r = scan(&mut d, ScanMode::Full, &mut Ctx::silent()).unwrap();
        assert!(r.aborted);
        assert_eq!(r.bytes_read, 0);
    }

    #[test]
    fn quick_scan_samples_and_includes_the_last_chunk() {
        let mut d = MemDevice::new(512 << 20, 512);
        d.bad.push(((512 << 20) - 512, 512 << 20));
        let r = scan(&mut d, ScanMode::Quick, &mut Ctx::silent()).unwrap();
        assert!(r.bytes_read <= 256 << 20);
        assert!(!r.unreadable.is_empty(), "the last sectors must be sampled");
    }

    #[test]
    fn device_smaller_than_a_chunk() {
        let mut d = MemDevice::new(4096, 512);
        for mode in [ScanMode::Quick, ScanMode::Full] {
            let r = scan(&mut d, mode, &mut Ctx::silent()).unwrap();
            assert_eq!(r.bytes_read, 4096);
        }
    }
}
