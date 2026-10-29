//! Typed operational reporting; payloads live only for the synchronous call.

/// `tracing`/`log` targets: the module that raises each event, as before
/// logging moved here, so per-module target filters keep matching.
/// `tests/log_targets.rs` in aimdb-core checks each names a real module.
#[allow(dead_code)] // some arms are feature-gated
mod target {
    pub(super) const STREAM: &str = "aimdb_core::remote::stream";
}

#[derive(Debug)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub(crate) enum RecordStreamEvent<'a> {
    Lagged {
        record: &'a str,
        skipped: u64,
    },
    Terminated {
        record: &'a str,
        error: &'a crate::DbError,
    },
}

pub(crate) fn report(event: RecordStreamEvent<'_>) {
    #[cfg(test)]
    capture::record(&event);
    match event {
        RecordStreamEvent::Lagged { record, skipped } => log_warn!(
            target: target::STREAM,
            "stream_record_updates: record '{}' subscription lagged by {} messages",
            record,
            skipped
        ),
        RecordStreamEvent::Terminated { record, error } => log_error!(
            target: target::STREAM,
            "stream_record_updates: record '{}' terminating on error: {:?}",
            record,
            error
        ),
    }
}

#[cfg(test)]
crate::__event_capture!(super::RecordStreamEvent<'_>);
