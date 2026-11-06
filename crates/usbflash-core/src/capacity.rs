// SPDX-License-Identifier: GPL-3.0-or-later
//! Destructive write/verify test that finds bad flash and fake capacity.
//!
//! Counterfeit sticks report, say, 2 TB but contain 32 GB. Writes beyond the real
//! capacity either vanish or wrap around onto the real flash. Every offset gets
//! unique pseudo random content ([`PatternStream`]), written first and only
//! verified afterwards, so both kinds of fraud show up as mismatches.
//!
//! * Full mode writes everything and is authoritative.
//! * Quick mode is a heuristic. Random probes rarely collide on a wrapping fake,
//!   so it adds probes at the offsets where a wrap of a typical real capacity
//!   lands on an earlier probe.

use crate::device::{AlignedBuf, BlockDevice, RangeList};
use crate::error::Result;
use crate::pattern::PatternStream;
use crate::progress::Ctx;
use crate::scan::ScanMode;
use crate::sweep::{MAX_CONSECUTIVE_FAILURES, chunk_size, spaced};
use serde::Serialize;
use std::time::Instant;

const QUICK_PROBES: u64 = 1024;
const QUICK_PROBE_SIZE: u64 = 64 * 1024;
/// Base offset of the alias probes, away from the very first sectors.
const ALIAS_BASE: u64 = 1 << 20;
const MAX_ALIAS_STEPS: u64 = 4096;
/// A contiguous failing area must make up this share to indicate fake capacity.
const FAKE_MIN_SHARE: f64 = 0.10;

#[derive(Debug, Clone, Serialize)]
pub struct CapacityReport {
    pub mode: ScanMode,
    pub claimed_bytes: u64,
    /// Offset of the first failure, or the full size if there was none.
    pub verified_bytes: u64,
    /// The real capacity of a fake stick, if it can be derived from the test.
    pub estimated_real_bytes: Option<u64>,
    pub write_failures: RangeList,
    pub verify_failures: RangeList,
    pub write_bytes_per_sec: f64,
    pub read_bytes_per_sec: f64,
    /// The failure pattern is typical for a stick that lies about its capacity.
    pub looks_fake: bool,
    pub aborted: bool,
    pub duration_secs: f64,
}

impl CapacityReport {
    pub fn passed(&self) -> bool {
        !self.aborted && self.write_failures.is_empty() && self.verify_failures.is_empty()
    }
}

fn positions(mode: ScanMode, size: u64, ss: u64, chunk: u64) -> Vec<u64> {
    if mode == ScanMode::Full {
        return (0..size.div_ceil(chunk)).map(|i| i * chunk).collect();
    }
    let n = QUICK_PROBES.min(size / chunk).max(1);
    let span = size.saturating_sub(chunk);
    let mut list: Vec<u64> = (0..n).map(|i| spaced(span, n, i, ss)).collect();

    // If the real capacity is R, then ALIAS_BASE + R wraps onto ALIAS_BASE.
    // Real capacities are powers of two or close to whole (decimal) gigabytes.
    if ALIAS_BASE < span {
        list.push(ALIAS_BASE);
        let powers = (20..63).map(|k| 1u64 << k);
        let gib = (1..=MAX_ALIAS_STEPS).map(|j| j << 30);
        let gb = (1..=MAX_ALIAS_STEPS).map(|j| j * 1_000_000_000);
        list.extend(powers.chain(gib).chain(gb).map(|r| ALIAS_BASE + r).filter(|&o| o <= span).map(|o| o / ss * ss));
    }
    list.sort_unstable();
    list.dedup();
    list
}

/// Fisher-Yates shuffle driven by the pattern stream. A fake controller that only
/// remaps sequentially looks fine under sequential writes.
fn shuffle(order: &mut [u64], rng: &mut PatternStream) {
    let mut random = vec![0u8; order.len() * 8];
    rng.fill(1 << 62, &mut random);
    for i in (1..order.len()).rev() {
        let r = u64::from_le_bytes(random[i * 8..i * 8 + 8].try_into().unwrap_or_default());
        order.swap(i, (r % (i as u64 + 1)) as usize);
    }
}

/// Finds the fake capacity signature in per-position results of a full test:
/// a single failing area at the end (lost writes) or the start (overwritten by
/// wrapped writes). Returns the estimated real capacity.
fn fake_signature(bad: &[bool], chunk: u64, size: u64) -> Option<u64> {
    let first = bad.iter().position(|&b| b)?;
    let last = bad.iter().rposition(|&b| b)?;
    let contiguous = bad[first..=last].iter().all(|&b| b);
    let touches_end = first == 0 || last == bad.len() - 1;
    let share = (last - first + 1) as f64 / bad.len() as f64;
    if contiguous && touches_end && (FAKE_MIN_SHARE..1.0).contains(&share) {
        Some(size.saturating_sub((last - first + 1) as u64 * chunk))
    } else {
        None
    }
}

/// Runs the test. **Destroys all data on the device.**
pub fn test(dev: &mut dyn BlockDevice, mode: ScanMode, ctx: &mut Ctx) -> Result<CapacityReport> {
    let ss = dev.sector_size() as u64;
    let size = dev.size() / ss * ss;
    let chunk = match mode {
        ScanMode::Full => chunk_size(dev.sector_size()) as u64,
        ScanMode::Quick => QUICK_PROBE_SIZE.max(ss),
    }
    .min(size.max(ss));
    let start = Instant::now();
    let mut rng = PatternStream::random()?;
    let mut buf = AlignedBuf::new(chunk as usize);
    let list = positions(mode, size, ss, chunk);
    let total: u64 = list.iter().map(|&o| chunk.min(size - o)).sum();

    let mut write_order = list.clone();
    if mode == ScanMode::Quick {
        shuffle(&mut write_order, &mut rng);
    }

    ctx.begin_phase("Writing test pattern", total);
    let mut failed_writes: Vec<(u64, u64)> = vec![];
    let mut consecutive = 0;
    let mut aborted = false;
    let t = Instant::now();
    let mut written = 0u64;
    for &offset in &write_order {
        ctx.check_cancel()?;
        let len = chunk.min(size - offset) as usize;
        rng.fill(offset, &mut buf[..len]);
        if dev.write_at(offset, &buf[..len]).is_ok() {
            consecutive = 0;
        } else {
            consecutive += 1;
            failed_writes.push((offset, len as u64));
        }
        written += len as u64;
        ctx.update(written, failed_writes.iter().map(|f| f.1).sum());
        if consecutive >= MAX_CONSECUTIVE_FAILURES {
            aborted = true;
            break;
        }
    }
    let write_secs = t.elapsed().as_secs_f64();
    dev.flush().ok();
    failed_writes.sort_unstable();
    let mut write_failures = RangeList::default();
    for &(o, l) in &failed_writes {
        write_failures.push(o, l);
    }

    // Verification is always in ascending order, independent of the write order.
    ctx.begin_phase("Verifying test pattern", total);
    let mut expected = AlignedBuf::new(chunk as usize);
    let mut verify_failures = RangeList::default();
    let mut bad_flags = Vec::with_capacity(list.len());
    let mut silent_mismatch = false;
    let t = Instant::now();
    let mut verified = 0u64;
    if !aborted {
        consecutive = 0;
        for &offset in &list {
            ctx.check_cancel()?;
            let len = chunk.min(size - offset) as usize;
            let read_ok = dev.read_at(offset, &mut buf[..len]).is_ok();
            let write_ok = failed_writes.binary_search_by_key(&offset, |f| f.0).is_err();
            let matches = read_ok && {
                rng.fill(offset, &mut expected[..len]);
                buf[..len] == expected[..len]
            };
            if read_ok && !matches && write_ok {
                silent_mismatch = true;
            }
            if !matches {
                verify_failures.push(offset, len as u64);
            }
            bad_flags.push(!matches);
            consecutive = if read_ok { 0 } else { consecutive + 1 };
            verified += len as u64;
            ctx.update(verified, verify_failures.total());
            if consecutive >= MAX_CONSECUTIVE_FAILURES {
                aborted = true;
                break;
            }
        }
    }
    let read_secs = t.elapsed().as_secs_f64();

    let first_bad = [write_failures.first_offset(), verify_failures.first_offset()].into_iter().flatten().min();
    let signature = if aborted { None } else { fake_signature(&bad_flags, chunk, size) };
    let estimated_real_bytes = match mode {
        ScanMode::Full => signature,
        // Probes are too sparse to measure a wrap. Lost writes can be located.
        ScanMode::Quick => signature.filter(|_| bad_flags.last() == Some(&true)),
    };
    let looks_fake = !aborted
        && (signature.is_some()
            // A quick test that reads back wrong data without any I/O error has
            // caught addresses that alias, or a controller corrupting data.
            || (mode == ScanMode::Quick && silent_mismatch));

    Ok(CapacityReport {
        mode,
        claimed_bytes: size,
        verified_bytes: if aborted { 0 } else { first_bad.unwrap_or(size) },
        estimated_real_bytes,
        write_failures,
        verify_failures,
        write_bytes_per_sec: if write_secs > 0.0 { written as f64 / write_secs } else { 0.0 },
        read_bytes_per_sec: if read_secs > 0.0 { verified as f64 / read_secs } else { 0.0 },
        looks_fake,
        aborted,
        duration_secs: start.elapsed().as_secs_f64(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::MemDevice;

    #[test]
    fn healthy_device_passes_both_modes() {
        for mode in [ScanMode::Full, ScanMode::Quick] {
            let mut d = MemDevice::new(8 << 20, 512);
            let r = test(&mut d, mode, &mut Ctx::silent()).unwrap();
            assert!(r.passed(), "{mode:?}");
            assert_eq!(r.verified_bytes, 8 << 20);
            assert!(!r.looks_fake);
            assert_eq!(r.estimated_real_bytes, None);
        }
    }

    #[test]
    fn wrapping_fake_is_detected_with_real_capacity() {
        let real = 16u64 << 20;
        let mut d = MemDevice::fake(real as usize, 64 << 20);
        let r = test(&mut d, ScanMode::Full, &mut Ctx::silent()).unwrap();
        assert!(!r.passed());
        assert!(r.looks_fake);
        assert_eq!(r.estimated_real_bytes, Some(real));
    }

    #[test]
    fn dropping_fake_is_detected_at_the_real_capacity() {
        let real = 16u64 << 20;
        let mut d = MemDevice::new(64 << 20, 512);
        d.dropped_writes.push((real, 64 << 20));
        let r = test(&mut d, ScanMode::Full, &mut Ctx::silent()).unwrap();
        assert_eq!(r.verified_bytes, real);
        assert!(r.looks_fake);
        assert_eq!(r.estimated_real_bytes, Some(real));
    }

    #[test]
    fn quick_mode_detects_dropped_writes() {
        let mut d = MemDevice::new(64 << 20, 512);
        d.dropped_writes.push((16 << 20, 64 << 20));
        let r = test(&mut d, ScanMode::Quick, &mut Ctx::silent()).unwrap();
        assert!(!r.passed());
        assert!(r.looks_fake);
        assert!(r.verified_bytes <= 16 << 20);
        assert!(r.verified_bytes >= 15 << 20);
    }

    #[test]
    fn quick_mode_detects_wrapping_on_a_huge_fake() {
        // "1 TiB" stick with 64 MiB of flash; random probes alone rarely collide here.
        let mut d = MemDevice::fake(1 << 26, 1 << 40);
        let r = test(&mut d, ScanMode::Quick, &mut Ctx::silent()).unwrap();
        assert!(!r.passed());
        assert!(r.looks_fake);
    }

    #[test]
    fn quick_mode_detects_wrapping_at_decimal_capacity() {
        // Exactly 1 GB of flash. The zeroed allocation is only touched where probes land.
        let mut d = MemDevice::fake(1_000_000_000, 1 << 34);
        let r = test(&mut d, ScanMode::Quick, &mut Ctx::silent()).unwrap();
        assert!(r.looks_fake);
    }

    #[test]
    fn isolated_errors_are_not_called_fake() {
        let mut d = MemDevice::new(64 << 20, 512);
        d.bad.push((1 << 20, (1 << 20) + 512));
        let r = test(&mut d, ScanMode::Full, &mut Ctx::silent()).unwrap();
        assert!(!r.passed());
        assert!(!r.looks_fake);
    }

    #[test]
    fn dead_device_aborts() {
        let mut d = MemDevice::new(128 << 20, 512);
        d.bad.push((0, 128 << 20));
        let r = test(&mut d, ScanMode::Full, &mut Ctx::silent()).unwrap();
        assert!(r.aborted);
        assert!(!r.passed());
        assert!(!r.looks_fake);
    }

    #[test]
    fn signature_rules() {
        let f = |bad: &[bool]| fake_signature(bad, 1, bad.len() as u64);
        assert_eq!(f(&[false, false, true, true]), Some(2));
        assert_eq!(f(&[true, true, true, false]), Some(1));
        assert_eq!(f(&[false, true, true, false]), None, "not at either end");
        assert_eq!(f(&[true, false, true, false]), None, "scattered");
        assert_eq!(f(&[true, true]), None, "all bad means broken, not fake");
        assert_eq!(f(&[false, false]), None);
    }
}
