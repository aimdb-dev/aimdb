//! Typed operational reporting; payloads live only for the synchronous call.

/// `tracing`/`log` targets: the module that raises each event, as before
/// logging moved here, so per-module target filters keep matching.
/// `tests/log_targets.rs` in aimdb-core checks each names a real module.
#[allow(dead_code)] // some arms are feature-gated
mod target {
    pub(super) const BUILDER_EXT: &str = "aimdb_persistence::builder_ext";
    pub(super) const EXT: &str = "aimdb_persistence::ext";
    pub(super) const QUERY_EXT: &str = "aimdb_persistence::query_ext";
}

#[derive(Debug)]
pub(crate) enum PersistenceEvent<'a> {
    CleanupFailed {
        error: &'a crate::error::PersistenceError,
    },
    BackendMissing {
        record: &'a str,
    },
    SerializeFailed {
        record: &'a str,
        error: &'a serde_json::Error,
    },
    StoreFailed {
        record: &'a str,
        error: &'a crate::error::PersistenceError,
    },
    RowDecodeFailed {
        record: &'a str,
        error: &'a serde_json::Error,
    },
}

pub(crate) fn report(event: PersistenceEvent<'_>) {
    #[cfg(test)]
    capture::record(&event);
    match event {
        PersistenceEvent::CleanupFailed { error } => {
            aimdb_core::log_warn!(
                target: target::BUILDER_EXT,
                "Persistence cleanup failed: {}",
                error
            );
            #[cfg(not(any(feature = "tracing", feature = "log")))]
            eprintln!("[aimdb-persistence] retention cleanup failed: {error}");
        }
        PersistenceEvent::BackendMissing { record } => {
            aimdb_core::log_warn!(
                target: target::EXT,
                "Record '{}' marked for persistence, but no backend is configured via with_persistence(); .persist() will be a no-op",
                record
            );
        }
        PersistenceEvent::SerializeFailed { record, error } => {
            aimdb_core::log_warn!(
                target: target::EXT,
                "Persistence: failed to serialize '{}': {}",
                record,
                error
            );
        }
        PersistenceEvent::StoreFailed { record, error } => {
            aimdb_core::log_warn!(
                target: target::EXT,
                "Persistence: failed to store '{}': {}",
                record,
                error
            );
        }
        PersistenceEvent::RowDecodeFailed { record, error } => {
            aimdb_core::log_warn!(
                target: target::QUERY_EXT,
                "Skipping persisted row for '{}': deserialization failed: {}",
                record,
                error
            );
        }
    }
}

/// Test-only poll-scoped observation. Spawned work must be wrapped explicitly.
#[cfg(test)]
#[allow(dead_code)]
pub(crate) mod capture {
    extern crate std;
    use super::PersistenceEvent;
    use core::future::{poll_fn, Future};
    use std::{boxed::Box, cell::RefCell, sync::Arc};
    pub(crate) type Observer = Arc<dyn Fn(&PersistenceEvent<'_>) + Send + Sync>;
    std::thread_local! {
        static CURRENT: RefCell<Option<Observer>> = const { RefCell::new(None) };
    }
    struct Guard(Option<Observer>);
    impl Drop for Guard {
        fn drop(&mut self) {
            CURRENT.with(|slot| {
                slot.replace(self.0.take());
            });
        }
    }
    pub(crate) fn with<T>(observer: Observer, f: impl FnOnce() -> T) -> T {
        let _guard = Guard(CURRENT.with(|slot| slot.replace(Some(observer))));
        f()
    }
    pub(crate) async fn future<F: Future>(observer: Observer, future: F) -> F::Output {
        let mut future = Box::pin(future);
        poll_fn(|cx| with(observer.clone(), || future.as_mut().poll(cx))).await
    }
    pub(super) fn record(event: &PersistenceEvent<'_>) {
        let observer = CURRENT.with(|slot| slot.borrow().clone());
        if let Some(observer) = observer {
            observer(event);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::{BoxFuture, PersistenceBackend, QueryParams, StoredValue};
    use crate::error::PersistenceError;
    use crate::{AimDbBuilderPersistExt, AimDbQueryExt, RecordRegistrarPersistExt};
    use std::sync::{Arc, Mutex};
    #[derive(Clone, Debug)]
    struct Unencodable;
    impl serde::Serialize for Unencodable {
        fn serialize<S: serde::Serializer>(&self, _: S) -> Result<S::Ok, S::Error> {
            Err(serde::ser::Error::custom("cannot encode"))
        }
    }
    struct BrokenBackend;
    impl PersistenceBackend for BrokenBackend {
        fn store<'a>(
            &'a self,
            _: &'a str,
            _: &'a serde_json::Value,
            _: u64,
        ) -> BoxFuture<'a, Result<(), PersistenceError>> {
            Box::pin(async { Err(PersistenceError::Backend("store failed".into())) })
        }
        fn query<'a>(
            &'a self,
            _: &'a str,
            _: QueryParams,
        ) -> BoxFuture<'a, Result<Vec<StoredValue>, PersistenceError>> {
            Box::pin(async {
                Ok(vec![StoredValue {
                    record_name: "bad".into(),
                    value: serde_json::json!("not a u32"),
                    stored_at: 7,
                }])
            })
        }
        fn cleanup(&self, _: u64) -> BoxFuture<'_, Result<u64, PersistenceError>> {
            Box::pin(async { Err(PersistenceError::Backend("cleanup failed".into())) })
        }
    }
    #[tokio::test]
    async fn persistence_reports_missing_backend_skipped_rows_store_and_cleanup_failures() {
        use aimdb_tokio_adapter::{TokioAdapter, TokioRecordRegistrarExt};
        let runtime = Arc::new(TokioAdapter::new().unwrap());
        let mut missing = aimdb_core::AimDbBuilder::new().runtime(runtime.clone());
        let seen = Arc::new(Mutex::new(Vec::new()));
        let observer: capture::Observer = {
            let seen = seen.clone();
            Arc::new(move |event| {
                seen.lock().unwrap().push(match event {
                    PersistenceEvent::BackendMissing { record } => {
                        assert_eq!(*record, "missing");
                        "missing"
                    }
                    PersistenceEvent::RowDecodeFailed { record, .. } => {
                        assert_eq!(*record, "bad");
                        "row"
                    }
                    PersistenceEvent::SerializeFailed { record, .. } => {
                        assert_eq!(*record, "encode");
                        "serialize"
                    }
                    PersistenceEvent::StoreFailed { record, .. } => {
                        assert_eq!(*record, "value");
                        "store"
                    }
                    PersistenceEvent::CleanupFailed { .. } => "cleanup",
                });
            })
        };
        capture::with(observer.clone(), || {
            missing.configure::<u32>("missing", |reg| {
                reg.persist("missing");
            });
        });
        let mut builder = aimdb_core::AimDbBuilder::new()
            .runtime(runtime)
            .with_persistence(Arc::new(BrokenBackend), core::time::Duration::from_secs(1));
        builder.configure::<u32>("value", |reg| {
            reg.buffer(aimdb_core::buffer::BufferCfg::SingleLatest)
                .persist("value");
        });
        builder.configure::<Unencodable>("encode", |reg| {
            reg.buffer(aimdb_core::buffer::BufferCfg::SingleLatest)
                .persist("encode");
        });
        let (db, runner) = builder.build().await.unwrap();
        let values = capture::future(observer.clone(), db.query_latest::<u32>("#", 1))
            .await
            .unwrap();
        assert!(values.is_empty());
        let producer = db.producer::<u32>("value").unwrap();
        producer.produce(7);
        db.producer::<Unencodable>("encode")
            .unwrap()
            .produce(Unencodable);
        // Stop as soon as both background failure paths have run; no sleeps or
        // process-global subscriber is needed to observe the decisions.
        let (stop_tx, stop_rx) = tokio::sync::oneshot::channel();
        let stop_tx = Mutex::new(Some(stop_tx));
        let background: capture::Observer = {
            let seen = seen.clone();
            let observer = observer.clone();
            Arc::new(move |event| {
                observer(event);
                let entries = seen.lock().unwrap();
                if entries.contains(&"store")
                    && entries.contains(&"cleanup")
                    && entries.contains(&"serialize")
                {
                    if let Some(tx) = stop_tx.lock().unwrap().take() {
                        let _ = tx.send(());
                    }
                }
            })
        };
        let run = capture::future(background, runner.run());
        tokio::time::timeout(core::time::Duration::from_secs(2), async {
            tokio::select! { _ = run => panic!("runner ended"), result = stop_rx => result.unwrap() }
        }).await.unwrap();
        let seen = seen.lock().unwrap();
        for tag in ["missing", "row", "store", "cleanup", "serialize"] {
            assert!(seen.contains(&tag));
        }
    }
}
