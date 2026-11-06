// SPDX-License-Identifier: GPL-3.0-or-later
//! Random-access pseudo random data stream.
//!
//! ChaCha20 keyed from the OS CSPRNG. The stream is addressable by byte offset,
//! which gives two properties we rely on:
//! * wipes can be verified by regenerating the data instead of storing it;
//! * every offset has unique content, so a fake drive that maps several logical
//!   addresses onto the same flash block is detected on read-back.

use crate::error::{Error, Result};
use rand_chacha::ChaCha20Rng;
use rand_chacha::rand_core::{Rng, SeedableRng};

pub struct PatternStream {
    rng: ChaCha20Rng,
}

impl PatternStream {
    /// Creates a stream with a fresh key from the operating system.
    pub fn random() -> Result<Self> {
        let mut seed = [0u8; 32];
        os_random(&mut seed)?;
        Ok(Self::from_seed(seed))
    }

    pub fn from_seed(seed: [u8; 32]) -> Self {
        Self { rng: ChaCha20Rng::from_seed(seed) }
    }

    /// Fills `buf` with the stream content at `offset`, which must be a multiple of 4.
    pub fn fill(&mut self, offset: u64, buf: &mut [u8]) {
        debug_assert_eq!(offset % 4, 0);
        self.rng.set_word_pos(u128::from(offset / 4));
        self.rng.fill_bytes(buf);
    }
}

/// Fills a small buffer with OS randomness, for keys, serial numbers and GUIDs.
pub fn os_random(buf: &mut [u8]) -> Result<()> {
    getrandom::fill(buf).map_err(|e| Error::Unsupported(format!("OS random generator failed: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stream_is_random_access() {
        let mut s = PatternStream::from_seed([7; 32]);
        let mut all = vec![0u8; 8192];
        s.fill(0, &mut all);
        let mut part = vec![0u8; 4096];
        s.fill(4096, &mut part);
        assert_eq!(&all[4096..], &part[..]);
    }

    #[test]
    fn offsets_have_unique_content() {
        let mut s = PatternStream::from_seed([1; 32]);
        let (mut a, mut b) = (vec![0u8; 512], vec![0u8; 512]);
        s.fill(0, &mut a);
        s.fill(1 << 33, &mut b);
        assert_ne!(a, b);
    }

    #[test]
    fn different_keys_differ() {
        let (mut a, mut b) = (vec![0u8; 64], vec![0u8; 64]);
        PatternStream::from_seed([1; 32]).fill(0, &mut a);
        PatternStream::from_seed([2; 32]).fill(0, &mut b);
        assert_ne!(a, b);
    }
}
