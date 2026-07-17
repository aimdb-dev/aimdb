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

#![cfg_attr(not(feature = "std"), no_std)]

extern crate alloc;

// Must precede the other modules: `macro_rules!` visibility is textual.
#[macro_use]
mod log;

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
pub use error::{ConfigError, DbError, DbResult};
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

// connector-session contracts (feature `connector-session`, no_std + alloc
// compatible). See docs/design/remote-access-via-connectors.md.
#[cfg(feature = "connector-session")]
pub use session::{
    is_wildcard, pump_sink, pump_source, topic_matches, AuthError, BoxFut, BoxStream, CodecError,
    Connection, Dialer, Dispatch, EnvelopeCodec, Inbound, Listener, Outbound, Payload, PeerInfo,
    RpcError, SessionCtx, SessionLimits, Source, SubUpdate, TransportError, TransportResult,
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
