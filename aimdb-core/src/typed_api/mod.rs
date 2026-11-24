//! Type-safe Producer-Consumer API
//!
//! Provides the complete typed API for producer-consumer patterns including:
//! - `Producer<T>` - Type-safe value production
//! - `Consumer<T>` - Type-safe value consumption
//! - `RecordRegistrar` - Fluent record configuration API
//! - `RecordT` trait - Self-registering records
//!
//! # Producer Example
//!
//! ```no_run
//! # use aimdb_core::{Producer, RuntimeContext};
//! # #[derive(Clone, Debug)] struct Temperature { celsius: f32 }
//! # async fn read_sensor() -> Temperature { Temperature { celsius: 21.0 } }
//! async fn temperature_producer(
//!     ctx: RuntimeContext,
//!     producer: Producer<Temperature>,
//! ) {
//!     loop {
//!         let temp = read_sensor().await;
//!         producer.produce(temp);
//!         ctx.time().sleep_secs(1).await;
//!     }
//! }
//! ```
//!
//! # Consumer Example
//!
//! ```no_run
//! # use aimdb_core::{Consumer, RuntimeContext};
//! # #[derive(Clone, Debug)] struct Temperature { celsius: f32 }
//! async fn temperature_monitor(
//!     ctx: RuntimeContext,
//!     consumer: Consumer<Temperature>,
//! ) {
//!     let mut rx = consumer.subscribe();
//!     // Each value comes with the metadata of the write that stored it.
//!     while let Ok(temp) = rx.recv().await {
//!         ctx.log().info(&format!(
//!             "Temp: {:.1}°C, written at {:?}",
//!             temp.value.celsius,
//!             temp.stamp()
//!         ));
//!     }
//! }
//! ```
//!
//! # Record Registration Example
//!
//! Illustrative (not compiled: `.buffer()` comes from your runtime adapter's
//! registrar extension trait, which `aimdb-core` cannot depend on):
//!
//! ```rust,ignore
//! let record_1 = aimdb_core::Name::<Temperature>::new("sensors.outdoor");
//! builder.configure(record_1, |reg| {
//!     reg.buffer(cfg)
//!        .source(temperature_producer)
//!        .tap(temperature_monitor)
//!        .link_to("mqtt://sensors/temp")
//!        .with_serializer(|_ctx, t| serde_json::to_vec(t))
//!        .finish();
//! });
//! ```

use core::fmt::Debug;

mod consumer;
mod link_inbound;
mod link_outbound;
mod producer;
mod registrar;

pub use crate::typed_api::{
    consumer::Consumer,
    link_inbound::{ContractInboundLink, InboundConnectorBuilder, InboundLink, InboundLinkBuilder},
    link_outbound::{
        ContractOutboundLink, OutboundConnectorBuilder, OutboundLink, OutboundLinkBuilder,
    },
    producer::Producer,
    registrar::{RecordRegistrar, StageKind},
};

/// Marks a link builder that is not attached to a record yet.
///
/// See [`OutboundLink`] and [`InboundLink`].
#[derive(Debug, Clone, Copy, Default)]
pub struct Detached;

/// Marks a detached link that carries a data contract: what
/// `aimdb-data-contracts`' `publish` / `subscribe` return.
///
/// It takes every option a [`Detached`] link takes, but is an
/// [`Input`] / [`Output`] only of a record whose key declares the contract
/// ([`ContractDeclared`](crate::ContractDeclared)).
#[derive(Debug, Clone, Copy, Default)]
pub struct DetachedContract;

// ============================================================================
// Input / Output - what `.input()` and `.output()` accept
// ============================================================================

/// Something that can write into a `T` record, passed to
/// [`RecordRegistrar::input`].
///
/// [`InboundLink<T>`] implements it. `wire` receives the full registrar, so an
/// input may call `.source()`, `.link_from()`, `.buffer()` or anything else a
/// `configure` closure can; inputs are expected to only add a writer.
///
/// Two conventions make wiring mistakes compile errors:
///
/// - **An input yields an event, and records are built with `From`.** A
///   device input implements `Input<T>` for every `T: From<Event>`, so one
///   input type serves every contract that can be built from its event.
/// - **A device channel is an owned value that is not `Clone`.** A board
///   hands out its channels once, and an input takes one by value, so wiring
///   one device into two records is `use of moved value`.
///
/// ```
/// use aimdb_core::{Input, RecordRegistrar};
/// use core::fmt::Debug;
///
/// /// Produces 0, 1, 2 into any record built from an `i32`.
/// struct Counter;
///
/// impl<T, S: aimdb_core::SchemaState> Input<T, S> for Counter
/// where
///     T: From<i32> + Send + Sync + 'static + Debug + Clone,
/// {
///     fn wire(self, reg: &mut RecordRegistrar<'_, T, S>) {
///         reg.source(|_ctx, producer| async move {
///             for i in 0..3 {
///                 producer.produce(T::from(i));
///             }
///         });
///     }
/// }
/// ```
///
/// A channel used for a second record does not compile:
///
/// ```compile_fail,E0382
/// # use aimdb_core::{Input, RecordRegistrar};
/// # use core::fmt::Debug;
/// /// A device channel, handed out once by the board. Not `Clone`.
/// struct Channel;
///
/// /// An input on one channel.
/// struct Sensor(Channel);
///
/// impl<T, S: aimdb_core::SchemaState> Input<T, S> for Sensor
/// where
///     T: From<u8> + Send + Sync + 'static + Debug + Clone,
/// {
///     fn wire(self, reg: &mut RecordRegistrar<'_, T, S>) {
///         reg.source(|_ctx, producer| async move { producer.produce(T::from(0)) });
///     }
/// }
///
/// #[derive(Clone, Debug)]
/// struct Level(u8);
///
/// impl From<u8> for Level {
///     fn from(raw: u8) -> Self {
///         Level(raw)
///     }
/// }
///
/// fn wire(
///     first: &mut RecordRegistrar<'_, Level>,
///     second: &mut RecordRegistrar<'_, Level>,
///     channel: Channel,
/// ) {
///     first.input(Sensor(channel));
///     second.input(Sensor(channel)); // use of moved value: `channel`
/// }
/// ```
#[diagnostic::on_unimplemented(
    message = "`{Self}` cannot be the input of a `{T}` record",
    label = "not an input for `{T}`",
    note = "an input implements `Input<{T}>`; device inputs usually do so for every `T: From<Event>`"
)]
pub trait Input<T: Send + Sync + 'static + Debug + Clone, S: crate::SchemaState> {
    /// Registers this input's writer on the record.
    fn wire(self, reg: &mut RecordRegistrar<'_, T, S>);
}

/// Something that can read from a `T` record, passed to
/// [`RecordRegistrar::output`].
///
/// [`OutboundLink<T>`] implements it. Like [`Input`], `wire` receives the
/// full registrar.
#[diagnostic::on_unimplemented(
    message = "`{Self}` cannot be an output of a `{T}` record",
    label = "not an output for `{T}`"
)]
pub trait Output<T: Send + Sync + 'static + Debug + Clone, S: crate::SchemaState> {
    /// Registers this output's reader on the record.
    fn wire(self, reg: &mut RecordRegistrar<'_, T, S>);
}

#[cfg(test)]
mod tests;
