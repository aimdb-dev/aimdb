//! Typed operational reporting; payloads live only for the synchronous call.

/// `tracing`/`log` targets: the module that raises each event, so per-module
/// target filters keep matching.
/// `tests/log_targets.rs` in aimdb-core checks each names a real module.
#[allow(dead_code)] // some arms are feature-gated
mod target {
    pub(super) const RUNTIME: &str = "aimdb_tokio_adapter::runtime";
}

#[derive(Debug)]
pub(crate) enum AdapterEvent {
    /// A clock was created with no Tokio runtime current, so it reads
    /// `std::time::Instant` and ignores Tokio's paused time.
    ClockOutsideRuntime,
}

pub(crate) fn report(event: AdapterEvent) {
    #[cfg(test)]
    capture::record(&event);
    match event {
        AdapterEvent::ClockOutsideRuntime => {
            aimdb_core::log_debug!(
                target: target::RUNTIME,
                "tokio clock created outside a runtime: reads real time, not tokio's paused time"
            );
        }
    }
}

#[cfg(all(test, feature = "tokio-runtime"))]
aimdb_core::__event_capture!(super::AdapterEvent);
