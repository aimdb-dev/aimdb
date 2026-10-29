//! Typed operational reporting; payloads live only for the synchronous call.

/// `tracing`/`log` targets: the module that raises each event, as before
/// logging moved here, so per-module target filters keep matching.
/// `tests/log_targets.rs` in aimdb-core checks each names a real module.
#[allow(dead_code)] // some arms are feature-gated
mod target {
    pub(super) const ROUTES: &str = "aimdb_core::outbound::routes";
}

#[derive(Debug)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub(crate) enum OutboundEvent<'a> {
    Lagged {
        topic: &'a str,
        skipped: u64,
    },
    TopicOverflow {
        topic: &'a str,
        capacity: usize,
    },
    SerializeFailed {
        topic: &'a str,
        error: &'a crate::outbound::SerializeFailure,
    },
    Closed {
        topic: &'a str,
        error: &'a crate::DbError,
    },
}

pub(crate) fn report(event: OutboundEvent<'_>) {
    #[cfg(test)]
    capture::record(&event);
    match event {
        OutboundEvent::Lagged { topic, skipped } => {
            log_warn!(target: target::ROUTES, "outbound route '{}' lagged by {}", topic, skipped)
        }
        OutboundEvent::TopicOverflow { topic, capacity } => log_warn!(
            target: target::ROUTES,
            "outbound route '{}': topic does not fit in {} bytes, value skipped",
            topic,
            capacity
        ),
        OutboundEvent::SerializeFailed { topic, error } => {
            log_error!(
                target: target::ROUTES,
                "outbound route '{}': {}, value skipped",
                topic,
                error
            )
        }
        OutboundEvent::Closed { topic, error } => {
            log_info!(target: target::ROUTES, "outbound route '{}' closed: {:?}", topic, error)
        }
    }
}

#[cfg(test)]
crate::__event_capture!(super::OutboundEvent<'_>);
