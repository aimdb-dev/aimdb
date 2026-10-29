//! Typed operational reporting; payloads live only for the synchronous call.

/// `tracing`/`log` targets: the module that raises each event, as before
/// logging moved here, so per-module target filters keep matching.
/// `tests/log_targets.rs` in aimdb-core checks each names a real module.
#[allow(dead_code)] // some arms are feature-gated
mod target {
    pub(super) const HANDLE: &str = "aimdb_sync::handle";
}

#[derive(Debug)]
pub(crate) enum SyncEvent<'a> {
    RuntimeStartFailed { error: &'a std::io::Error },
    RuntimeHandoffFailed,
    DatabaseBuildFailed { error: &'a aimdb_core::DbError },
    DatabaseHandoffFailed,
    ImplicitShutdown,
    ShutdownSignalled,
}

pub(crate) fn report(event: SyncEvent<'_>) {
    #[cfg(test)]
    capture::record(&event);
    match event {
        SyncEvent::RuntimeStartFailed { error } => {
            aimdb_core::log_error!(
                target: target::HANDLE,
                "Failed to create Tokio runtime: {}",
                error
            )
        }
        SyncEvent::RuntimeHandoffFailed => {
            aimdb_core::log_error!(
                target: target::HANDLE,
                "Failed to send runtime handle to main thread"
            )
        }
        SyncEvent::DatabaseBuildFailed { error } => {
            aimdb_core::log_error!(target: target::HANDLE, "Failed to build database: {}", error)
        }
        SyncEvent::DatabaseHandoffFailed => {
            aimdb_core::log_error!(target: target::HANDLE, "Failed to send database to main thread")
        }
        SyncEvent::ImplicitShutdown => {
            aimdb_core::log_warn!(
                target: target::HANDLE,
                "AimDbHandle dropped without calling shutdown()"
            )
        }
        SyncEvent::ShutdownSignalled => {
            aimdb_core::log_warn!(
                target: target::HANDLE,
                "Shutdown was signalled; the runtime thread stops on its own"
            )
        }
    }
}

#[cfg(test)]
aimdb_core::__event_capture!(super::SyncEvent<'_>);
