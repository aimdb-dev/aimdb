//! Typed operational reporting; payloads live only for the synchronous call.

/// `tracing`/`log` targets: the module that raises each event, as before
/// logging moved here, so per-module target filters keep matching.
/// `tests/log_targets.rs` in aimdb-core checks each names a real module.
#[allow(dead_code)] // some arms are feature-gated
mod target {
    pub(super) const TYPED_RECORD: &str = "aimdb_core::typed_record";
}

#[derive(Debug)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub(crate) enum RemoteWriteEvent<'a> {
    Rejected {
        operation: &'a str,
        record_type: &'a str,
    },
}

pub(crate) fn report(event: RemoteWriteEvent<'_>) {
    #[cfg(test)]
    capture::record(&event);
    match event {
        RemoteWriteEvent::Rejected {
            operation,
            record_type,
        } => log_warn!(
            target: target::TYPED_RECORD,
            "Rejected {} for '{}': has active producer or transform",
            operation,
            record_type
        ),
    }
}

#[cfg(test)]
crate::__event_capture!(super::RemoteWriteEvent<'_>);
