// SPDX-License-Identifier: GPL-3.0-or-later
//! In-memory fake devices for unit tests, including misbehaving ones.

use crate::device::{BUFFER_ALIGN, BlockDevice};
use std::io;

/// RAM disk with optional fault injection.
pub struct MemDevice {
    pub data: Vec<u8>,
    pub sector: u32,
    /// Size claimed to the caller. Larger than `data` simulates a fake-capacity
    /// stick: addresses wrap around onto the real flash.
    pub claimed: u64,
    /// Byte ranges (start, end) whose access fails.
    pub bad: Vec<(u64, u64)>,
    /// Writes in these ranges silently vanish; reads return the old data.
    pub dropped_writes: Vec<(u64, u64)>,
}

impl MemDevice {
    pub fn new(size: usize, sector: u32) -> Self {
        Self { data: vec![0; size], sector, claimed: size as u64, bad: vec![], dropped_writes: vec![] }
    }

    /// A stick that claims `claimed` bytes but only stores `real` bytes.
    pub fn fake(real: usize, claimed: u64) -> Self {
        Self { claimed, ..Self::new(real, 512) }
    }

    fn check(&self, offset: u64, len: usize, ptr: *const u8) -> io::Result<()> {
        let ss = self.sector as u64;
        assert_eq!(offset % ss, 0, "unaligned offset {offset}");
        assert_eq!(len as u64 % ss, 0, "unaligned length {len}");
        assert_eq!(ptr as usize % BUFFER_ALIGN, 0, "unaligned buffer");
        assert!(offset + len as u64 <= self.claimed, "access beyond the device end");
        let end = offset + len as u64;
        if self.bad.iter().any(|&(s, e)| offset < e && s < end) {
            return Err(io::Error::other("injected I/O error"));
        }
        Ok(())
    }
}

impl BlockDevice for MemDevice {
    fn size(&self) -> u64 {
        self.claimed
    }
    fn sector_size(&self) -> u32 {
        self.sector
    }
    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        self.check(offset, buf.len(), buf.as_ptr())?;
        let real = self.data.len() as u64;
        let mut at = offset;
        for part in chunks_until_wrap(buf.len(), offset, real) {
            let start = (at % real) as usize;
            let done = (at - offset) as usize;
            buf[done..done + part].copy_from_slice(&self.data[start..start + part]);
            at += part as u64;
        }
        Ok(())
    }
    fn write_at(&mut self, offset: u64, buf: &[u8]) -> io::Result<()> {
        self.check(offset, buf.len(), buf.as_ptr())?;
        let real = self.data.len() as u64;
        if self.dropped_writes.is_empty() {
            let mut at = offset;
            for part in chunks_until_wrap(buf.len(), offset, real) {
                let start = (at % real) as usize;
                let done = (at - offset) as usize;
                self.data[start..start + part].copy_from_slice(&buf[done..done + part]);
                at += part as u64;
            }
            return Ok(());
        }
        for (i, b) in buf.iter().enumerate() {
            let at = offset + i as u64;
            if self.dropped_writes.iter().any(|&(s, e)| at >= s && at < e) {
                continue;
            }
            self.data[(at % real) as usize] = *b;
        }
        Ok(())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Splits an access into the lengths of its pieces between wrap-arounds of the real flash.
fn chunks_until_wrap(len: usize, offset: u64, real: u64) -> Vec<usize> {
    let mut parts = vec![];
    let (mut left, mut at) = (len as u64, offset);
    while left > 0 {
        let part = left.min(real - at % real);
        parts.push(part as usize);
        left -= part;
        at += part;
    }
    parts
}
