//! AimDB Core Database Engine
//!
//! # aimdb-core
//!
//! Type-safe, async in-memory database for data synchronization
//! across MCU → edge → cloud environments.
//!
//! # Architecture
//!
//! - **RecordKey/RecordId**: Stable identifiers for multi-instance records
//! - **Runtime Agnostic**: Works with Tokio (std) or Embassy (embedded)
//! - **Producer-Consumer**: Built-in typed message passing
//!
//! See examples in the repository for usage patterns.
//!
//! # A panic is a bug, not an error channel
//!
//! Every failure is a [`DbError`]. The crate is compiled under
//! `deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)` outside its
//! tests; the sites that remain carry an `allow` with a reason. A dependency
//! can still panic on its own, and a poisoned lock is recovered from.
//!
//! # Where aimdb's own reporting goes
//!
//! Two optional destinations, neither on by default: **`tracing`**, the ordinary
//! choice for a Rust binary, and **`log`**, for a host that cannot be handed a
//! process-global subscriber — chiefly an FFI layer, which needs its callback's
//! context pointer to live somewhere a `tracing::Layer` has no room for. Both
//! may be on at once; both off emits nothing. Events carry the emitting module
//! as their target (`aimdb_core::builder`) either way.
//! Every crate in this workspace that reports at all now does so through the
//! facade, so a destination sees the connectors too. See
//! [docs/design/050](https://github.com/aimdb-dev/aimdb/blob/main/docs/design/050-log-destination-for-ffi.md)
//! for filtering and duplicate delivery.
//!
//! ## What a `log` destination must guarantee
//!
//! Getting one wrong costs a deadlock or memory unsafety, not a bad log line:
//!
//! 1. **Called from any thread**, aimdb's runtime thread included.
//! 2. **Must not unwind** — across a C or C++ boundary that is UB.
//! 3. **Must not re-enter aimdb on a path that itself logs.** `log` has no
//!    reentrancy guard (`tracing` does), so this recurses without bound.
//!    Getters are fine.
//! 4. **Must not block on anything a thread might hold while calling into
//!    aimdb** — that is the lock ordering.
//! 5. **Cannot be uninstalled.** `set_logger` is once per process; a second
//!    call returns `Err` and the first destination keeps receiving.

// A panic here is a bug: two FFI doors sit above this crate, and a consumer's
// `panic = "abort"` compiles their guard out.
#![cfg_attr(
    not(test),
    deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)
)]
#![cfg_attr(not(feature = "std"), no_std)]

extern crate alloc;

// Must precede the other modules: `macro_rules!` visibility is textual.
#[macro_use]
mod log;

/// Implementation detail of the `log_*` macros; no stability guarantee.
///
/// The macros expand in the *calling* crate, so they can only name paths
/// reachable from there — this re-export is what saves a facade user from
/// declaring `log` itself.
#[doc(hidden)]
pub mod __private {
    // `::log`, not `log`: this crate has a private `log` module at its root.
    #[cfg(feature = "log")]
    pub use ::log;
    #[cfg(feature = "tracing")]
    pub use ::tracing;
}

pub mod buffer;
pub mod builder;
#[cfg(feature = "remote")]
pub mod codec;
pub mod connector;
pub mod context;
mod error;
pub mod executor;
pub mod extensions;
pub mod graph;
#[cfg(feature = "observability")]
pub mod profiling;
pub mod record_id;
#[cfg(feature = "remote")]
pub mod remote;
pub mod router;
#[cfg(feature = "connector-session")]
pub mod session;
pub mod signal;
pub mod transform;
pub mod transport;
pub mod typed_api;
pub mod typed_record;

// Public API exports
pub use context::RuntimeContext;
pub use error::{ConfigError, DbError, DbErrorKind, DbResult};
pub use extensions::Extensions;

// Runtime capability surface: the runtime travels as `Arc<dyn RuntimeOps>`,
// the one trait an adapter implements.
pub use executor::{BoxFuture, ExecutorError, ExecutorResult, LogLevel, RuntimeOps};

// Producer-Consumer Pattern exports
#[cfg(feature = "remote")]
pub use buffer::JsonReader;
pub use buffer::Reader;
pub use buffer::TryProduceError;
pub use builder::OutboundRoute;
pub use builder::{AimDb, AimDbBuilder};
pub use connector::ConnectorBuilder;
pub use transport::{Connector, ConnectorConfig, PublishError};
pub use typed_api::{
    Consumer, InboundConnectorBuilder, OutboundConnectorBuilder, Producer, RecordRegistrar,
    StageKind,
};
#[cfg(feature = "remote")]
pub use typed_record::JsonRecordAccess;
pub use typed_record::{AnyRecord, AnyRecordExt, TypedRecord};

// JSON codec (feature `remote`, no_std + alloc compatible)
#[cfg(feature = "remote")]
pub use codec::{JsonCodec, RemoteSerialize, SerdeJsonCodec};

// Record-key leaf/entity derivation, shared by record metadata and the
// connectors that surface the `entity` field.
#[cfg(feature = "remote")]
pub use remote::topic_leaf;

// connector-session contracts (feature `connector-session`, no_std + alloc
// compatible). See docs/design/remote-access-via-connectors.md.
#[cfg(feature = "connector-session")]
pub use session::{
    is_wildcard, pattern_contains, pump_sink, pump_source, topic_matches, AuthError, BoxFut,
    BoxStream, CodecError, Connection, Dialer, Dispatch, EnvelopeCodec, Inbound, Listener,
    Outbound, Payload, PeerInfo, RpcError, SessionCtx, SessionLimits, Source, SubUpdate,
    TransportError, TransportResult,
};

// Signal gauge handle (always available; inert without `observability`)
pub use signal::SignalGaugeHandle;

// Stage profiling exports (feature-gated)
#[cfg(feature = "observability")]
pub use profiling::{
    RecordProfilingMetrics, SignalGauge, SignalStats, SignalStatsInfo, StageMetrics,
    StageProfilingInfo,
};

// Connector Infrastructure exports
pub use connector::TopicProvider;
pub use connector::TopicResolverFn;
pub use connector::{ConnectorLink, ConnectorUrl, LinkAddress, SerializeError};
pub use connector::{IngestFactoryFn, IngestFn};
pub use connector::{
    SerializedPayload, SerializedReader, SerializedSource, SerializedValue, SerializedValueInto,
    SourceFactoryFn,
};

// Router exports for connector implementations
pub use router::{Route, Router, RouterBuilder};

// Record identification exports
pub use record_id::{RecordId, RecordKey, StringKey};

// Graph exports (dependency graph for record topology)
pub use graph::{DependencyGraph, EdgeType, GraphEdge, GraphNode, RecordOrigin};

// Transform API exports
pub use transform::{JoinBuilder, JoinEventRx, JoinPipeline, JoinTrigger};
pub use transform::{TransformBuilder, TransformPipeline};
