// SPDX-License-Identifier: GPL-3.0-or-later
//! Progress reporting and cooperative cancellation for long running operations.

use crate::error::{Error, Result};
use serde::Serialize;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// Shared flag; setting it asks the running operation to stop at the next chunk.
pub type CancelFlag = Arc<AtomicBool>;

#[derive(Debug, Clone, Default, Serialize)]
pub struct Progress {
    /// Human readable description of the current phase, e.g. `Pass 1/2: random`.
    pub phase: String,
    pub done: u64,
    pub total: u64,
    pub bytes_per_sec: f64,
    pub eta_secs: Option<f64>,
    /// Number of bytes that failed so far in this phase.
    pub errors: u64,
}

impl Progress {
    pub fn fraction(&self) -> f32 {
        if self.total == 0 { 0.0 } else { (self.done as f64 / self.total as f64).clamp(0.0, 1.0) as f32 }
    }
}

/// Execution context handed to every long running operation.
pub struct Ctx {
    cancel: CancelFlag,
    sink: Box<dyn FnMut(&Progress) + Send>,
    current: Progress,
    phase_start: Instant,
    last_report: Option<Instant>,
}

impl Ctx {
    pub fn new(cancel: CancelFlag, sink: impl FnMut(&Progress) + Send + 'static) -> Self {
        Self {
            cancel,
            sink: Box::new(sink),
            current: Progress::default(),
            phase_start: Instant::now(),
            last_report: None,
        }
    }

    /// A context without progress output and without external cancellation.
    pub fn silent() -> Self {
        Self::new(CancelFlag::default(), |_| {})
    }

    pub fn check_cancel(&self) -> Result<()> {
        if self.cancel.load(Ordering::Relaxed) { Err(Error::Cancelled) } else { Ok(()) }
    }

    pub fn begin_phase(&mut self, phase: impl Into<String>, total: u64) {
        self.current = Progress { phase: phase.into(), total, ..Progress::default() };
        self.phase_start = Instant::now();
        self.last_report = None;
        (self.sink)(&self.current);
    }

    /// Updates the current phase. Forwarded to the sink at most ten times a second.
    pub fn update(&mut self, done: u64, errors: u64) {
        self.current.done = done;
        self.current.errors = errors;
        let now = Instant::now();
        let due = self.last_report.is_none_or(|t| now.duration_since(t) >= Duration::from_millis(100));
        if due || done >= self.current.total {
            let elapsed = now.duration_since(self.phase_start).as_secs_f64();
            if elapsed > 0.0 {
                self.current.bytes_per_sec = done as f64 / elapsed;
                self.current.eta_secs = (self.current.bytes_per_sec > 0.0)
                    .then(|| self.current.total.saturating_sub(done) as f64 / self.current.bytes_per_sec);
            }
            self.last_report = Some(now);
            (self.sink)(&self.current);
        }
    }
}
