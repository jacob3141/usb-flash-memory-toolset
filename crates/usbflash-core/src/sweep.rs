// SPDX-License-Identifier: GPL-3.0-or-later
//! Error tolerant sequential I/O shared by wipe, surface scan and capacity test.
//!
//! Failing flash must not stop a wipe at the first bad block: we want to overwrite
//! everything that can still be overwritten and report exactly what could not.

use crate::device::{BlockDevice, CHUNK_SIZE, RangeList};

/// A failing chunk is retried in pieces of this size to narrow down the bad area.
const RETRY_PIECE: usize = 64 * 1024;
/// After this many failing chunks in a row, the device is considered gone.
pub const MAX_CONSECUTIVE_FAILURES: u32 = 32;

/// The largest multiple of the sector size not exceeding the default chunk size.
pub fn chunk_size(ss: u32) -> usize {
    CHUNK_SIZE / ss as usize * ss as usize
}

/// Position `i` of `n` evenly spaced over `0..=span`, rounded down to a sector.
/// The last position is exactly `span` (if aligned), so device ends get covered.
pub fn spaced(span: u64, n: u64, i: u64, ss: u64) -> u64 {
    if n <= 1 {
        return 0;
    }
    ((span as u128 * i as u128 / (n - 1) as u128) as u64) / ss * ss
}

/// Accumulates failures of one sequential pass.
#[derive(Debug, Default)]
pub struct Sweep {
    pub bad: RangeList,
    consecutive: u32,
}

impl Sweep {
    /// True once so many chunks failed in a row that continuing is pointless.
    pub fn gave_up(&self) -> bool {
        self.consecutive >= MAX_CONSECUTIVE_FAILURES
    }

    /// Records the outcome of one chunk.
    pub fn record(&mut self, offset: u64, len: u64, ok: bool) {
        if ok {
            self.consecutive = 0;
        } else {
            self.consecutive += 1;
            self.bad.push(offset, len);
        }
    }

    /// Writes one chunk, retrying in pieces on failure.
    pub fn write(&mut self, dev: &mut dyn BlockDevice, offset: u64, buf: &[u8]) {
        if dev.write_at(offset, buf).is_ok() {
            self.consecutive = 0;
            return;
        }
        self.consecutive += 1;
        let piece = RETRY_PIECE.max(dev.sector_size() as usize);
        for (i, p) in buf.chunks(piece).enumerate() {
            let at = offset + (i * piece) as u64;
            if dev.write_at(at, p).is_err() {
                self.bad.push(at, p.len() as u64);
            }
        }
    }

    /// Reads one chunk, retrying in pieces on failure. Unreadable pieces stay zero.
    /// Returns true if the entire chunk was read.
    pub fn read(&mut self, dev: &mut dyn BlockDevice, offset: u64, buf: &mut [u8]) -> bool {
        if dev.read_at(offset, buf).is_ok() {
            self.consecutive = 0;
            return true;
        }
        self.consecutive += 1;
        let piece = RETRY_PIECE.max(dev.sector_size() as usize);
        for (i, p) in buf.chunks_mut(piece).enumerate() {
            let at = offset + (i * piece) as u64;
            if dev.read_at(at, p).is_err() {
                p.fill(0);
                self.bad.push(at, p.len() as u64);
            }
        }
        false
    }
}
