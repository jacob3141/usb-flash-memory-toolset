// SPDX-License-Identifier: GPL-3.0-or-later
//! Overwrites a whole device, with verification and honest failure reporting.
//!
//! Limitation documented in `docs/SAFETY.md`: flash controllers keep spare blocks
//! that cannot be addressed from outside, so overwriting cannot guarantee that
//! every physical copy of the old data is gone.

use crate::device::{AlignedBuf, BlockDevice, RangeList};
use crate::error::Result;
use crate::pattern::PatternStream;
use crate::progress::Ctx;
use crate::sweep::{Sweep, chunk_size};
use serde::{Deserialize, Serialize};
use std::time::Instant;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WipeMode {
    /// One pass of zeros.
    Zero,
    /// One pass of cryptographically strong random data.
    Random,
    /// Random data, then zeros, so the stick also reads as blank afterwards.
    RandomThenZero,
}

impl WipeMode {
    fn passes(self) -> &'static [Pass] {
        match self {
            WipeMode::Zero => &[Pass::Zero],
            WipeMode::Random => &[Pass::Random],
            WipeMode::RandomThenZero => &[Pass::Random, Pass::Zero],
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            WipeMode::Zero => "Zeros (1 pass)",
            WipeMode::Random => "Random data (1 pass)",
            WipeMode::RandomThenZero => "Random, then zeros (2 passes)",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Pass {
    Zero,
    Random,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WipeStatus {
    /// Everything written and read back correctly.
    Verified,
    /// Everything written without an error, verification not requested.
    WrittenUnverified,
    /// Some areas may still contain old data. See the failure lists.
    Incomplete,
}

#[derive(Debug, Clone, Serialize)]
pub struct WipeReport {
    pub mode: WipeMode,
    pub bytes: u64,
    pub status: WipeStatus,
    pub write_failures: RangeList,
    pub verify_failures: RangeList,
    pub aborted: bool,
    pub duration_secs: f64,
}

/// Overwrites the entire device.
pub fn wipe(dev: &mut dyn BlockDevice, mode: WipeMode, verify: bool, ctx: &mut Ctx) -> Result<WipeReport> {
    let ss = dev.sector_size() as u64;
    let size = dev.size() / ss * ss;
    let chunk = chunk_size(dev.sector_size()) as u64;
    let start = Instant::now();
    let mut buf = AlignedBuf::new(chunk as usize);
    let mut write_failures = RangeList::default();
    let mut aborted = false;
    let passes = mode.passes();
    let mut last_stream = None;

    for (i, &pass) in passes.iter().enumerate() {
        let name = if pass == Pass::Random { "random data" } else { "zeros" };
        ctx.begin_phase(format!("Pass {}/{}: writing {name}", i + 1, passes.len()), size);
        let mut stream = match pass {
            Pass::Random => Some(PatternStream::random()?),
            Pass::Zero => {
                buf.fill(0);
                None
            }
        };
        let mut sweep = Sweep::default();
        let mut offset = 0;
        while offset < size {
            ctx.check_cancel()?;
            let len = chunk.min(size - offset) as usize;
            if let Some(s) = stream.as_mut() {
                s.fill(offset, &mut buf[..len]);
            }
            sweep.write(dev, offset, &buf[..len]);
            offset += len as u64;
            ctx.update(offset, sweep.bad.total());
            if sweep.gave_up() {
                aborted = true;
                sweep.bad.push(offset, size - offset);
                break;
            }
        }
        dev.flush()?;
        // A range only counts as failed if the *last* pass could not write it,
        // but keeping the union errs on the side of reporting too much.
        for r in sweep.bad.ranges {
            write_failures.ranges.push(r);
        }
        last_stream = stream;
        if aborted {
            break;
        }
    }
    write_failures.ranges.sort_unstable_by_key(|r| r.offset);

    let mut verify_failures = RangeList::default();
    if verify && !aborted {
        ctx.begin_phase("Verifying", size);
        let mut expected = AlignedBuf::new(chunk as usize);
        let mut sweep = Sweep::default();
        let mut offset = 0;
        while offset < size {
            ctx.check_cancel()?;
            let len = chunk.min(size - offset) as usize;
            let read_ok = dev.read_at(offset, &mut buf[..len]).is_ok();
            let ok = read_ok && {
                match last_stream.as_mut() {
                    Some(s) => {
                        s.fill(offset, &mut expected[..len]);
                        buf[..len] == expected[..len]
                    }
                    None => buf[..len].iter().all(|&b| b == 0),
                }
            };
            sweep.record(offset, len as u64, ok);
            offset += len as u64;
            ctx.update(offset, sweep.bad.total());
            if sweep.gave_up() {
                aborted = true;
                sweep.bad.push(offset, size - offset);
                break;
            }
        }
        verify_failures = sweep.bad;
    }

    let status = if aborted || !write_failures.is_empty() || !verify_failures.is_empty() {
        WipeStatus::Incomplete
    } else if verify {
        WipeStatus::Verified
    } else {
        WipeStatus::WrittenUnverified
    };

    Ok(WipeReport {
        mode,
        bytes: size,
        status,
        write_failures,
        verify_failures,
        aborted,
        duration_secs: start.elapsed().as_secs_f64(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analyze::entropy;
    use crate::testutil::MemDevice;

    fn dirty(size: usize, ss: u32) -> MemDevice {
        let mut d = MemDevice::new(size, ss);
        d.data.fill(0xAB);
        d
    }

    #[test]
    fn zero_wipe_leaves_only_zeros() {
        let mut d = dirty((3 << 20) + 512, 512);
        let r = wipe(&mut d, WipeMode::Zero, true, &mut Ctx::silent()).unwrap();
        assert_eq!(r.status, WipeStatus::Verified);
        assert!(d.data.iter().all(|&b| b == 0));
    }

    #[test]
    fn random_wipe_overwrites_everything_with_high_entropy() {
        let mut d = dirty(4 << 20, 4096);
        let r = wipe(&mut d, WipeMode::Random, true, &mut Ctx::silent()).unwrap();
        assert_eq!(r.status, WipeStatus::Verified);
        assert!(entropy(&d.data) > 7.99);
        // Not one sector may retain the old pattern.
        assert!(d.data.chunks(4096).all(|s| s.iter().any(|&b| b != 0xAB)));
    }

    #[test]
    fn random_then_zero_ends_blank() {
        let mut d = dirty(2 << 20, 512);
        let r = wipe(&mut d, WipeMode::RandomThenZero, true, &mut Ctx::silent()).unwrap();
        assert_eq!(r.status, WipeStatus::Verified);
        assert!(d.data.iter().all(|&b| b == 0));
    }

    #[test]
    fn without_verify_status_says_so() {
        let mut d = dirty(1 << 20, 512);
        let r = wipe(&mut d, WipeMode::Zero, false, &mut Ctx::silent()).unwrap();
        assert_eq!(r.status, WipeStatus::WrittenUnverified);
    }

    #[test]
    fn silently_dropped_writes_are_caught_by_verify() {
        let mut d = dirty(4 << 20, 512);
        d.dropped_writes.push((1 << 20, 2 << 20));
        let r = wipe(&mut d, WipeMode::Random, true, &mut Ctx::silent()).unwrap();
        assert_eq!(r.status, WipeStatus::Incomplete);
        assert_eq!(r.verify_failures.first_offset(), Some(1 << 20));
    }

    #[test]
    fn bad_blocks_do_not_stop_the_wipe_but_are_reported() {
        let mut d = dirty(4 << 20, 512);
        d.bad.push((1 << 20, (1 << 20) + 512));
        let r = wipe(&mut d, WipeMode::Zero, true, &mut Ctx::silent()).unwrap();
        assert_eq!(r.status, WipeStatus::Incomplete);
        assert!(!r.write_failures.is_empty());
        // Data behind the bad block was still overwritten.
        assert!(d.data[(3 << 20)..].iter().all(|&b| b == 0));
    }

    #[test]
    fn dead_device_reports_the_remainder_as_failed() {
        let mut d = dirty(128 << 20, 512);
        d.bad.push((0, 128 << 20));
        let r = wipe(&mut d, WipeMode::Zero, true, &mut Ctx::silent()).unwrap();
        assert!(r.aborted);
        assert_eq!(r.status, WipeStatus::Incomplete);
        assert_eq!(r.write_failures.total(), 128 << 20);
    }

    #[test]
    fn cancellation_is_an_error_not_a_success() {
        let mut d = dirty(4 << 20, 512);
        let flag = crate::CancelFlag::default();
        flag.store(true, std::sync::atomic::Ordering::Relaxed);
        let mut ctx = Ctx::new(flag, |_| {});
        assert!(matches!(wipe(&mut d, WipeMode::Zero, true, &mut ctx), Err(crate::Error::Cancelled)));
    }
}
