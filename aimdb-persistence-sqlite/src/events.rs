//! Typed operational reporting; payloads live only for the synchronous call.

/// `tracing`/`log` targets: the module that raises each event, as before
/// logging moved here, so per-module target filters keep matching.
/// `tests/log_targets.rs` in aimdb-core checks each names a real module.
#[allow(dead_code)] // some arms are feature-gated
mod target {
    pub(super) const ROOT: &str = "aimdb_persistence_sqlite";
}

#[derive(Debug)]
pub(crate) enum SqliteEvent<'a> {
    CorruptJson { error: &'a serde_json::Error },
}

pub(crate) fn report(event: SqliteEvent<'_>) {
    #[cfg(test)]
    capture::record(&event);
    match event {
        SqliteEvent::CorruptJson { error } => {
            aimdb_core::log_warn!(
                target: target::ROOT,
                "SQLite: corrupted JSON in record_history row, substituting null: {}",
                error
            );
        }
    }
}

#[cfg(test)]
aimdb_core::__event_capture!(super::SqliteEvent<'_>);
