//! Typed operational reporting; payloads live only for the synchronous call.

/// `tracing`/`log` targets: the module that raises each event, as before
/// logging moved here, so per-module target filters keep matching.
/// `tests/log_targets.rs` in aimdb-core checks each names a real module.
#[allow(dead_code)] // some arms are feature-gated
mod target {
    pub(super) const ROUTER: &str = "aimdb_core::router";
}

#[derive(Debug)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub(crate) enum RouterEvent<'a> {
    IngestFailed {
        topic: &'a str,
        record: &'a str,
        /// Formatted only if a logger prints it.
        reason: crate::IngestDetail<'a>,
        rejection: crate::Rejection,
    },
}

pub(crate) fn report(event: RouterEvent<'_>) {
    #[cfg(test)]
    capture::record(&event);
    match event {
        RouterEvent::IngestFailed {
            topic,
            record,
            reason,
            rejection,
        } => {
            log_warn!(target: target::ROUTER, "Failed to ingest message on '{}' for record '{}': {} ({})", topic, record, reason, rejection)
        }
    }
}

#[cfg(test)]
crate::__event_capture!(super::RouterEvent<'_>);
