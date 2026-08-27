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
//! # Where aimdb's own reporting goes
//!
//! `aimdb-core` reports through a crate-private facade with two optional
//! destinations. Neither is on by default; with both off a call site still
//! borrows its arguments (so it compiles warning-free) but emits nothing.
//!
//! - **`tracing`** — the ordinary choice for a Rust binary, and the recommended
//!   one. Install a subscriber and you get spans, `EnvFilter`'s per-target
//!   directives, and everything else in that ecosystem.
//! - **`log`** — for a host that *cannot* install a process-global `tracing`
//!   subscriber, chiefly an FFI layer loaded into a non-Rust process. A
//!   `log::Log` impl is an ordinary value, so a C or Python binding can keep its
//!   callback's context pointer inside the destination itself rather than in a
//!   static of its own.
//!
//! ## Using the `log` destination
//!
//! Enable `aimdb-core/log` and install a logger before building the database:
//!
//! ```toml
//! aimdb-core = { version = "1.3", features = ["log"] }
//! ```
//!
//! ```ignore
//! log::set_logger(&MY_LOGGER)?;      // or `set_boxed_logger` / `Box::leak`
//! log::set_max_level(log::LevelFilter::Info);
//! ```
//!
//! Events arrive with `Record::target()` set to the emitting module —
//! `aimdb_core::builder`, `aimdb_core::session::pump`, `aimdb_sync::handle` —
//! the same strings a `tracing` subscriber sees.
//!
//! ## What a destination must guarantee
//!
//! No signature can carry these, and an FFI layer that gets one wrong produces a
//! deadlock or memory unsafety rather than a bad log line:
//!
//! 1. **It is called from any thread**, aimdb's runtime thread included — the
//!    same thread every shutdown waits for. `log::Log` requires `Send + Sync`;
//!    that requirement is load-bearing, not a formality.
//! 2. **It must not unwind.** For a `cdylib` reached from C or C++ an unwind
//!    across the boundary is undefined behaviour, not a crash. Catch on both
//!    sides.
//! 3. **It must not call back into aimdb on a path that itself logs.** A
//!    destination that publishes a reading recurses without bound: unlike
//!    `tracing`'s dispatcher, `log` has no reentrancy guard, so this is an
//!    unbounded recursion on the runtime thread rather than a dropped event.
//!    Calling the *getters* is fine.
//! 4. **It must not block on anything a thread might hold while calling into
//!    aimdb.** That is the whole lock ordering; it is what makes "the
//!    destination must not free the database" a rule rather than a suggestion.
//! 5. **It cannot be uninstalled.** `log::set_logger` is once per process by
//!    construction, and reports that honestly: a second call returns `Err` and
//!    the first destination keeps receiving. Whatever the logger points at must
//!    outlive the process.
//!
//! ## Filtering
//!
//! `log::set_max_level` is one relaxed load, checked before the arguments are
//! formatted, and covers the common case. There is no `EnvFilter` equivalent:
//! per-target directives are the destination's business, and a `Log` impl that
//! wants them matches a prefix list against `record.target()` itself.
//!
//! ## Both destinations at once
//!
//! `tracing` and `log` may both be enabled, and then both arms of the facade
//! run. A process that installed two destinations sees each event once at each.
//! A process that installed *one* can still see an event twice, and feature
//! unification across a workspace can turn `log` on without anyone asking for
//! it, so it is worth knowing the three bridges that cause it:
//!
//! - `tracing-subscriber`'s `tracing-log` feature (**on by default**) —
//!   `SubscriberInitExt::init`/`try_init` installs a `LogTracer`, which converts
//!   `log` records into `tracing` events. Turn the feature off, or build the
//!   subscriber without `init()`.
//! - `tracing`'s own optional `log` feature, which emits a `log` record for
//!   every `tracing` event.
//! - Any `log`-to-`tracing` bridge the process installs itself, such as
//!   `tracing_log::LogTracer::init()` called directly.
//!
//! If duplicates are unacceptable and the host is an FFI layer, the clean answer
//! is to build `aimdb-core` with `--no-default-features` plus `log` and leave
//! `tracing` off entirely.
//!
//! ## What the `log` destination does not see
//!
//! Only events emitted through the facade. `aimdb-mqtt-connector`,
//! `aimdb-knx-connector`, `aimdb-serial-connector` and `aimdb-uds-connector`
//! still call `tracing::` directly (24 call sites between them); those reach a
//! `tracing` subscriber only. Migrating them to the facade is follow-up work.

#![cfg_attr(not(feature = "std"), no_std)]

extern crate alloc;

// Must precede the other modules: `macro_rules!` visibility is textual.
#[macro_use]
mod log;

/// Implementation detail of the `log_*` macros. Not a public API: it carries no
/// stability guarantee and may change in a patch release.
///
/// The facade macros are `#[macro_export]`ed, so their bodies expand in the
/// calling crate and can only name paths reachable from *there*. Re-exporting
/// the `log` crate here is what lets a facade user enable the destination
/// without also declaring `log` as a dependency of its own.
#[doc(hidden)]
pub mod __private {
    // `::log` and not `log`: this crate has a private `log` module at its root,
    // and the leading `::` says unambiguously that this is the external crate.
    #[cfg(feature = "log")]
    pub use ::log;
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
