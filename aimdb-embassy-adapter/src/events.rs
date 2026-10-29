//! Typed operational reporting; payloads live only for the synchronous call.

/// `tracing`/`log` targets: the module that raises each event, as before
/// logging moved here, so per-module target filters keep matching.
/// `tests/log_targets.rs` in aimdb-core checks each names a real module.
#[allow(dead_code)] // some arms are feature-gated
mod target {
    pub(super) const BUFFER: &str = "aimdb_embassy_adapter::buffer";
}

#[derive(Debug)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub(crate) enum AdapterEvent {
    SubscriberSlotsExhausted { capacity: usize },
}

pub(crate) fn report(event: AdapterEvent) {
    #[cfg(test)]
    capture::record(&event);
    match event {
        AdapterEvent::SubscriberSlotsExhausted { capacity } => {
            aimdb_core::log_error!(
                target: target::BUFFER,
                "AimDB: SpmcRing subscriber slot exhausted (max SUBS={}). \
             Increase the CONSUMERS const generic on buffer_sized<CAP, CONSUMERS>. \
             Count one slot per .tap(), .link_to() connector, and each transform_join input.",
                capacity
            );
        }
    }
}

#[cfg(test)]
aimdb_core::__event_capture!(super::AdapterEvent);
