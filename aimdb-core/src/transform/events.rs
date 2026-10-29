//! Typed operational reporting; payloads live only for the synchronous call.

/// `tracing`/`log` targets: the module that raises each event, as before
/// logging moved here, so per-module target filters keep matching.
/// `tests/log_targets.rs` in aimdb-core checks each names a real module.
#[allow(dead_code)] // some arms are feature-gated
mod target {
    pub(super) const JOIN: &str = "aimdb_core::transform::join";
    pub(super) const SINGLE: &str = "aimdb_core::transform::single";
}

#[derive(Debug)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub(crate) enum TransformEvent<'a> {
    JoinConsumerResolutionFailed {
        input: &'a str,
        index: usize,
        error: &'a crate::DbError,
    },
    JoinLagged {
        input: &'a str,
        index: usize,
    },
    JoinTaskExited {
        output: &'a str,
    },
    ConsumerResolutionFailed {
        input: &'a str,
        output: &'a str,
        error: &'a crate::DbError,
    },
    Lagged {
        input: &'a str,
        output: &'a str,
    },
    InputClosed {
        input: &'a str,
        output: &'a str,
    },
}

pub(crate) fn report(event: TransformEvent<'_>) {
    #[cfg(test)]
    capture::record(&event);
    match event {
        TransformEvent::JoinConsumerResolutionFailed {
            input,
            index,
            error,
        } => log_error!(
            target: target::JOIN,
            "🔄 Join input '{}' (index {}) consumer resolution failed: {:?}",
            input,
            index,
            error
        ),
        TransformEvent::JoinLagged { input, index } => log_warn!(
            target: target::JOIN,
            "🔄 Join input '{}' (index {}) lagged behind, some values skipped",
            input,
            index
        ),
        TransformEvent::JoinTaskExited { output } => {
            log_warn!(target: target::JOIN, "🔄 Join transform '{}' user task exited", output)
        }
        TransformEvent::ConsumerResolutionFailed {
            input,
            output,
            error,
        } => log_error!(
            target: target::SINGLE,
            "🔄 Transform '{}' → '{}' FATAL: failed to resolve consumer: {:?}",
            input,
            output,
            error
        ),
        TransformEvent::Lagged { input, output } => log_warn!(
            target: target::SINGLE,
            "🔄 Transform '{}' → '{}' lagged behind, some values skipped",
            input,
            output
        ),
        TransformEvent::InputClosed { input, output } => log_warn!(
            target: target::SINGLE,
            "🔄 Transform '{}' → '{}' input closed, task exiting",
            input,
            output
        ),
    }
}

#[cfg(test)]
crate::__event_capture!(super::TransformEvent<'_>);
