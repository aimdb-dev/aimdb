#[cfg(feature = "remote")]
use crate::JsonRecordAccess;
use crate::TypedRecord;
use alloc::{boxed::Box, string::String, sync::Arc, vec::Vec};
use core::{any::Any, fmt::Debug};

/// Type-erased trait for records — the storage and lifecycle contract.
///
/// Allows storage of heterogeneous record types in a single collection
/// while maintaining type safety through downcast operations.
///
/// This trait carries the storage/lifecycle
/// surface, plus graph/metadata introspection and observability counter
/// resets. JSON remote access is the one genuinely optional capability: it
/// lives in `JsonRecordAccess`, reachable via `json_access`, both behind the
/// `remote` feature.
///
/// Consumers: `AimDbBuilder::build()` (config-error draining, the dependency
/// graph fed to [`crate::graph`], typed downcasts via [`AnyRecordExt`]),
/// the builder's inbound/outbound route collection, `AimDbInner::list_records`
/// (remote introspection metadata), and connectors applying the remote-access
/// security policy (`set_writable_erased`).
///
/// # Thread Safety Requirements
///
/// This trait requires both `Send` and `Sync` because:
/// - Records are stored in `Arc<Box<dyn AnyRecord>>` and shared across threads
/// - The router system needs to access records from multiple connector tasks
/// - Emitter needs to be `Send+Sync` to work in async contexts
/// - The `FnOnce` consumers are moved out during spawning, so they don't affect `Sync`
///
/// **BREAKING CHANGE (v0.2.0):** Added `Sync` bound to `AnyRecord` trait.
/// Record types must now be both `Send + Sync`. Types that were previously
/// `Send` but not `Sync` can no longer be used as records. This change enables:
/// - Concurrent access to records from multiple connector tasks
/// - Safe sharing of record metadata across thread boundaries
/// - Type-safe routing in the bidirectional connector system
///
/// **Migration:** If your record type `T` is not `Sync`, wrap non-`Sync` fields
/// in `Arc<Mutex<_>>` or `Arc<RwLock<_>>` to achieve interior mutability with
/// thread-safe sharing.
pub trait AnyRecord: Send + Sync {
    /// Returns self as Any for downcasting
    fn as_any(&self) -> &dyn Any;

    /// Returns self as mutable Any for downcasting
    fn as_any_mut(&mut self) -> &mut dyn Any;

    /// Drains the configuration mistakes recorded during registration, plus
    /// build-time consistency findings (e.g. remote access without a buffer).
    ///
    /// Called by `AimDbBuilder::build()`, which fills in the record key and
    /// reports every finding via
    /// [`DbError::InvalidConfiguration`](crate::DbError::InvalidConfiguration).
    fn drain_config_errors(&mut self) -> Vec<crate::error::ConfigError>;

    /// Sets the writable flag for this record (type-erased)
    ///
    /// Used internally by the builder to apply security policy to records.
    fn set_writable_erased(&self, writable: bool);

    /// Binds the instance clock every write to this record is stamped from.
    ///
    /// Called by `AimDbBuilder::build()` for every record before any task
    /// runs (design 058 §3.3).
    fn bind_clock(&mut self, clock: Arc<dyn crate::clock::InstanceClock>);

    /// Returns the record's JSON remote-access surface, if it has one.
    ///
    /// This accessor is the single place the `remote` cfg-gate lives
    /// for consumers: they query the capability here instead of cfg-gating
    /// every call site. `TypedRecord` always returns `Some`; the runtime
    /// "configured with `.with_remote_access()`" checks stay inside the
    /// [`JsonRecordAccess`] methods.
    #[cfg(feature = "remote")]
    fn json_access(&self) -> Option<&dyn JsonRecordAccess> {
        None
    }

    // ── Graph and metadata introspection ────────────────────────────────

    /// Gets the outbound connector links
    ///
    /// Returns outbound connector configuration list for spawning logic.
    fn outbound_connectors(&self) -> &[crate::connector::ConnectorLink];

    /// Get the inbound connector links for this record
    fn inbound_connectors(&self) -> &[crate::connector::InboundConnectorLink];

    /// The schema the record carries, if one was set while configuring it
    fn schema(&self) -> Option<&crate::graph::SchemaInfo>;

    /// Returns the number of registered consumers (tap observers)
    fn consumer_count(&self) -> usize;

    /// Returns whether a producer service is registered
    fn has_producer(&self) -> bool;

    /// Returns whether a buffer is configured
    fn has_buffer(&self) -> bool;

    /// Returns whether a transform is registered for this record
    fn has_transform(&self) -> bool;

    /// Returns how this record gets its values (for dependency graph construction)
    fn record_origin(&self) -> crate::graph::RecordOrigin;

    /// Returns the buffer type name and capacity (for dependency graph)
    ///
    /// Returns (buffer_type_name, optional_capacity).
    fn buffer_info(&self) -> (String, Option<usize>);

    /// Returns the transform input keys (if a transform is registered)
    fn transform_input_keys(&self) -> Option<Vec<String>>;

    /// Returns each transform input's key and the type it is read as (empty
    /// if no transform is registered). `build()` checks them against the
    /// input records' types.
    fn transform_input_types(&self) -> Vec<(String, core::any::TypeId, &'static str)>;

    /// Returns the name of the record's value type, for error messages.
    fn type_name(&self) -> &'static str;

    /// Collects metadata for this record
    #[cfg(feature = "remote")]
    fn collect_metadata(
        &self,
        type_id: core::any::TypeId,
        key: crate::record_id::StringKey,
        id: crate::record_id::RecordId,
    ) -> crate::remote::RecordMetadata;

    // ── Observability counter resets ─────────────────────────────────────

    /// Resets this record's stage profiling counters (feature `observability`).
    ///
    /// Default implementation is a no-op; `TypedRecord` overrides it.
    /// Consumers: `AimDb::reset_profiling`, driven by the AimX
    /// `control.reset_buffer_metrics` RPC and the MCP buffer-metrics tool.
    #[cfg(feature = "observability")]
    fn reset_profiling(&self) {}

    /// Resets this record's buffer introspection counters (feature `observability`).
    ///
    /// Default implementation is a no-op; `TypedRecord` overrides it.
    #[cfg(feature = "observability")]
    fn reset_buffer_metrics(&self) {}
}

// Helper extension trait for type-safe downcasting
pub trait AnyRecordExt {
    /// Attempts to downcast to a typed record reference
    ///
    /// # Type Parameters
    /// * `T` - The expected record type
    ///
    /// # Returns
    /// `Some(&TypedRecord<T>)` if types match, `None` otherwise
    fn as_typed<T: Send + 'static + Debug + Clone>(&self) -> Option<&TypedRecord<T>>;

    /// Attempts to downcast to a mutable typed record reference
    ///
    /// # Type Parameters
    /// * `T` - The expected record type
    ///
    /// # Returns
    /// `Some(&mut TypedRecord<T>)` if types match, `None` otherwise
    fn as_typed_mut<T: Send + 'static + Debug + Clone>(&mut self) -> Option<&mut TypedRecord<T>>;
}

impl AnyRecordExt for Box<dyn AnyRecord> {
    fn as_typed<T: Send + 'static + Debug + Clone>(&self) -> Option<&TypedRecord<T>> {
        self.as_any().downcast_ref::<TypedRecord<T>>()
    }

    fn as_typed_mut<T: Send + 'static + Debug + Clone>(&mut self) -> Option<&mut TypedRecord<T>> {
        self.as_any_mut().downcast_mut::<TypedRecord<T>>()
    }
}
