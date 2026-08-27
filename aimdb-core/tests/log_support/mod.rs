//! Shared scaffolding for the design-050 `log` destination tests.
//!
//! `log::set_logger` is once per process, so each criterion that installs a
//! logger owns a test binary — hence a subdirectory, which Cargo does not build
//! as a binary of its own.
#![allow(dead_code)]

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

/// One delivered event, flattened — `log::Record` borrows, so it cannot be kept.
#[derive(Clone, Debug)]
pub struct Captured {
    pub target: String,
    pub level: log::Level,
    pub message: String,
}

/// A destination that keeps everything it is handed.
pub struct Capture {
    records: Mutex<Vec<Captured>>,
}

impl Capture {
    pub const fn new() -> Self {
        Self {
            records: Mutex::new(Vec::new()),
        }
    }

    pub fn taken(&self) -> Vec<Captured> {
        self.records.lock().unwrap().clone()
    }

    pub fn count(&self) -> usize {
        self.records.lock().unwrap().len()
    }

    pub fn with_message(&self, needle: &str) -> Vec<Captured> {
        self.taken()
            .into_iter()
            .filter(|r| r.message.contains(needle))
            .collect()
    }
}

impl log::Log for Capture {
    fn enabled(&self, _: &log::Metadata<'_>) -> bool {
        true
    }

    fn log(&self, record: &log::Record<'_>) {
        self.records.lock().unwrap().push(Captured {
            target: record.target().to_string(),
            level: record.level(),
            message: record.args().to_string(),
        });
    }

    fn flush(&self) {}
}

/// A format argument that counts its own formatting — the only way to observe
/// from outside that a filtered-out event never reached `Display::fmt`.
pub struct FormatProbe {
    formatted: AtomicUsize,
}

impl FormatProbe {
    pub const fn new() -> Self {
        Self {
            formatted: AtomicUsize::new(0),
        }
    }

    pub fn formatted(&self) -> usize {
        self.formatted.load(Ordering::Relaxed)
    }
}

impl core::fmt::Display for FormatProbe {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        self.formatted.fetch_add(1, Ordering::Relaxed);
        f.write_str("probe")
    }
}
