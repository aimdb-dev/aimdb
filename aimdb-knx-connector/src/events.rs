//! Typed operational reporting; payloads live only for the synchronous call.

/// `tracing`/`log` targets: the module that raises each event, as before
/// logging moved here, so per-module target filters keep matching.
/// `tests/log_targets.rs` in aimdb-core checks each names a real module.
#[allow(dead_code)] // some arms are feature-gated
mod target {
    pub(super) const CLIENT: &str = "aimdb_knx_connector::client";
}

#[derive(Debug)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub(crate) enum KnxEvent<'a> {
    SendFailed,
    AckTimedOut {
        sequence: u8,
    },
    OutboundRejected {
        route: &'a str,
        error: aimdb_core::transport::PublishError,
    },
    BinderBusy,
    BindFailed,
}

pub(crate) fn report(event: KnxEvent<'_>) {
    #[cfg(test)]
    capture::record(&event);
    match event {
        KnxEvent::SendFailed => {
            aimdb_core::log_error!(target: target::CLIENT, "KNX send failed");
        }
        KnxEvent::AckTimedOut { sequence } => {
            aimdb_core::log_warn!(
                target: target::CLIENT,
                "KNX outbound: no ACK for sequence {}",
                sequence
            )
        }
        KnxEvent::OutboundRejected { route, error } => {
            aimdb_core::log_warn!(
                target: target::CLIENT,
                "KNX outbound: skipping a value for route '{}': {:?}",
                route,
                error
            );
        }
        KnxEvent::BinderBusy => {
            aimdb_core::log_error!(
                target: target::CLIENT,
                "KNX bind failed: the binder's socket is held by another handle \
                     (a clone of a single-socket binder?); retrying"
            );
        }
        KnxEvent::BindFailed => {
            aimdb_core::log_error!(target: target::CLIENT, "KNX bind failed; retrying");
        }
    }
}

#[cfg(test)]
aimdb_core::__event_capture!(super::KnxEvent<'_>);
