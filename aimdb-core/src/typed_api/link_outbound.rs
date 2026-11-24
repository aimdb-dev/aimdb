use alloc::{
    string::{String, ToString},
    sync::Arc,
    vec::Vec,
};

use core::fmt::Debug;

use super::registrar::{
    wire_link, TypedContextSerializerFn, TypedContextSerializerIntoFn, WireLink,
};
use crate::typed_api::{Consumer, Detached, DetachedContract, Output, RecordRegistrar};
use crate::AimDb;

// ============================================================================
// Outbound routes
// ============================================================================

/// Type alias for the unified typed serializer captured by [`TypedRoute`]
///
/// Raw and context-aware serializers collapse into this shape at `finish()`;
/// the raw variant simply ignores the threaded context.
pub(super) type FusedSerializeFn<T> = Arc<
    dyn Fn(&crate::RuntimeContext, &T) -> Result<Vec<u8>, crate::connector::SerializeError>
        + Send
        + Sync,
>;

/// Builds a link's `Consumer<T>` from the live database, once per route.
pub(super) type ConsumerFactoryFn<T> = Arc<dyn Fn(&AimDb) -> Consumer<T> + Send + Sync>;

/// Optional allocation-free serializer captured beside [`FusedSerializeFn`].
///
/// The callback writes into the `OutboundRoutes` scratch buffer. Returning
/// `BufferTooSmall` selects the owned serializer for that value; other failures
/// retain the existing skip-and-log behavior.
pub(super) type FusedSerializeIntoFn<T> = Arc<
    dyn Fn(&crate::RuntimeContext, &T, &mut [u8]) -> Result<usize, crate::connector::SerializeError>
        + Send
        + Sync,
>;

/// An outbound link's topic writer and the capacity it writes into.
type TopicWriterCfg<T> = (usize, Arc<dyn crate::connector::TopicWriter<T>>);

/// Runs `writer` for `value` into `out`. `Ok(true)`: publish to what `out`
/// holds; `Ok(false)`: to the link's default topic; `Err`: the topic did not
/// fit and the value is skipped, whatever the writer returned.
pub(super) fn write_topic<T, W: crate::connector::TopicWriter<T> + ?Sized>(
    writer: &W,
    value: &T,
    out: &mut crate::connector::TopicBuf<'_>,
) -> Result<bool, crate::connector::TopicOverflow> {
    match writer.write_topic(value, out) {
        Ok(written) if !out.overflowed() => Ok(written),
        _ => Err(crate::connector::TopicOverflow),
    }
}

/// Serializes one outbound value: into `scratch` through
/// `with_serializer_into` when the link has one, else (or when the value does
/// not fit) through the owned serializer.
pub(super) fn serialize_outbound<T>(
    serialize: &FusedSerializeFn<T>,
    serialize_into: Option<&FusedSerializeIntoFn<T>>,
    ctx: &crate::RuntimeContext,
    value: &T,
    scratch: &mut [u8],
) -> Result<crate::outbound::StagedPayload, crate::outbound::SerializeFailure> {
    use crate::connector::SerializeError;
    use crate::outbound::{SerializeFailure, StagedPayload};

    let Some(serialize_into) = serialize_into else {
        return serialize(ctx, value)
            .map(StagedPayload::Owned)
            .map_err(SerializeFailure::Owned);
    };
    match serialize_into(ctx, value, scratch) {
        Ok(len) if len <= scratch.len() => Ok(StagedPayload::Scratch(len)),
        Ok(len) => Err(SerializeFailure::InvalidLength {
            len,
            capacity: scratch.len(),
        }),
        Err(SerializeError::BufferTooSmall) => serialize(ctx, value)
            .map(StagedPayload::Owned)
            .map_err(SerializeFailure::Fallback),
        Err(e) => Err(SerializeFailure::Into(e)),
    }
}

/// One outbound link's per-route state inside
/// [`OutboundRoutes`](crate::OutboundRoutes): reader, topic writer and
/// serializers, all typed.
pub(super) struct TypedRoute<T: Clone + Send + 'static> {
    pub(super) reader: crate::buffer::Reader<T>,
    pub(super) writer: Option<Arc<dyn crate::connector::TopicWriter<T>>>,
    pub(super) serialize: FusedSerializeFn<T>,
    pub(super) serialize_into: Option<FusedSerializeIntoFn<T>>,
}

impl<T: Clone + Send + 'static> crate::outbound::PollRoute for TypedRoute<T> {
    fn poll_route(
        &mut self,
        cx: &mut core::task::Context<'_>,
        ctx: &crate::RuntimeContext,
        topic: &mut [u8],
        payload: &mut [u8],
    ) -> core::task::Poll<crate::outbound::RouteOutcome> {
        use crate::outbound::RouteOutcome;
        use core::task::Poll;

        let sample = match self.reader.poll_recv(cx) {
            Poll::Pending => return Poll::Pending,
            Poll::Ready(Ok(sample)) => sample,
            Poll::Ready(Err(crate::DbError::BufferLagged { lag_count, .. })) => {
                return Poll::Ready(RouteOutcome::Lagged(lag_count));
            }
            Poll::Ready(Err(e)) => return Poll::Ready(RouteOutcome::Closed(e)),
        };

        let topic_len = match &self.writer {
            None => None,
            Some(writer) => {
                let mut out = crate::connector::TopicBuf::new(topic);
                match write_topic(&**writer, &sample.value, &mut out) {
                    Ok(written) => written.then_some(out.len()),
                    Err(_) => return Poll::Ready(RouteOutcome::TopicOverflow),
                }
            }
        };

        let serialized = serialize_outbound(
            &self.serialize,
            self.serialize_into.as_ref(),
            ctx,
            &sample.value,
            payload,
        );
        Poll::Ready(match serialized {
            Ok(payload) => RouteOutcome::Staged {
                topic_len,
                payload,
                metadata: *sample.metadata(),
            },
            Err(failure) => RouteOutcome::SerializeFailed(failure),
        })
    }
}

// ============================================================================
// Link builders - Fluent connector link configuration
// ============================================================================

/// Outbound link builder attached to a record: what
/// [`RecordRegistrar::link_to`] returns.
///
/// `'r` is the borrow of the registrar taken by `link_to()`; `'a` is the
/// registrar's own borrow of the record being configured.
pub type OutboundConnectorBuilder<'r, 'a, T, S = crate::Undeclared> =
    OutboundLinkBuilder<T, &'r mut RecordRegistrar<'a, T, S>>;

/// Outbound link not attached to a record yet, built with
/// [`OutboundLink::new`] and registered by [`RecordRegistrar::output`].
pub type OutboundLink<T> = OutboundLinkBuilder<T, Detached>;

/// Outbound link carrying a data contract, built by
/// [`OutboundLink::into_contract`]: an [`Output`] only of a record whose key
/// declares the contract.
pub type ContractOutboundLink<T> = OutboundLinkBuilder<T, DetachedContract>;

type OutboundWireFinalizer<T> =
    Arc<dyn Fn(OutboundLink<T>) -> Result<OutboundLink<T>, String> + Send + Sync>;

/// Builder for configuring outbound connector links (AimDB → External)
///
/// `R` is what the builder is attached to. [`RecordRegistrar::link_to`]
/// returns the attached form, [`OutboundConnectorBuilder`], which
/// [`finish()`](OutboundLinkBuilder::finish) registers on its record.
/// [`OutboundLink::new`] builds the detached form, [`OutboundLink`], before
/// any record exists; [`RecordRegistrar::output`] registers it, and checks
/// it then, not when it is built. Both register the same link:
///
/// ```rust,ignore
/// reg.link_to("mqtt://out").with_serializer(ser).finish();
/// reg.output(OutboundLink::new("mqtt://out").with_serializer(ser));
/// ```
///
/// The option methods below, and the connector crates' extension traits,
/// work on both forms.
#[must_use = "a link does nothing until it is passed to `.output()` or finished with `.finish()`"]
pub struct OutboundLinkBuilder<T: Send + Sync + 'static + Debug + Clone, R> {
    wire: crate::link_wire::WireOptions,
    wire_codec: Option<crate::link_wire::WireCodecInfo>,
    wire_finalizer: Option<OutboundWireFinalizer<T>>,
    at: R,
    pub(super) url: String,
    pub(super) connector: Option<crate::ConnectorHandle>,
    pub(super) config: Vec<(String, String)>,
    pub(super) context_serializer: Option<TypedContextSerializerFn<T>>,
    pub(super) context_serializer_into: Option<(usize, TypedContextSerializerIntoFn<T>)>,
    pub(super) topic_writer: Option<TopicWriterCfg<T>>,
}

impl<T> OutboundLinkBuilder<T, Detached>
where
    T: Send + Sync + 'static + Debug + Clone,
{
    /// Starts an outbound link to `address` on a connector registered with
    /// [`AimDbBuilder::connect`](crate::AimDbBuilder::connect).
    ///
    /// `address` is the part after `scheme://`; the connector supplies the
    /// scheme. A full URL (as a `#[link_address]` key gives) is accepted if
    /// its scheme is the connector's. A different scheme, or a handle from
    /// another builder, is reported by `build()` once the link is passed to
    /// [`RecordRegistrar::output`].
    pub fn on(connector: crate::ConnectorHandle, address: &str) -> Self {
        Self {
            connector: Some(connector),
            ..Self::new(address)
        }
    }

    /// Starts an outbound link to `url` that is not attached to a record yet.
    pub fn new(url: &str) -> Self {
        Self {
            at: Detached,
            wire: Default::default(),
            wire_codec: None,
            wire_finalizer: None,
            url: url.to_string(),
            connector: None,
            config: Vec::new(),
            context_serializer: None,
            context_serializer_into: None,
            topic_writer: None,
        }
    }

    /// Marks this link as carrying a data contract, so it wires only into a
    /// record whose key declares the contract. `aimdb-data-contracts`'
    /// `publish` calls this; the link's options are unchanged.
    pub fn into_contract(self) -> ContractOutboundLink<T> {
        self.attach(DetachedContract)
    }
}

impl<T, A> OutboundLinkBuilder<T, A>
where
    T: Send + Sync + 'static + Debug + Clone,
{
    pub(super) fn attach<R>(self, at: R) -> OutboundLinkBuilder<T, R> {
        OutboundLinkBuilder {
            at,
            wire: self.wire,
            wire_codec: self.wire_codec,
            wire_finalizer: self.wire_finalizer,
            url: self.url,
            connector: self.connector,
            config: self.config,
            context_serializer: self.context_serializer,
            context_serializer_into: self.context_serializer_into,
            topic_writer: self.topic_writer,
        }
    }
}

impl<T, R> OutboundLinkBuilder<T, R>
where
    T: Send + Sync + 'static + Debug + Clone,
{
    #[doc(hidden)]
    pub fn wire_options(&self) -> crate::link_wire::WireOptions {
        self.wire
    }
    #[doc(hidden)]
    pub fn with_wire_options(mut self, options: crate::link_wire::WireOptions) -> Self {
        self.wire = options;
        self
    }
    #[doc(hidden)]
    pub fn wire_codec_info(&self) -> Option<crate::link_wire::WireCodecInfo> {
        self.wire_codec
    }
    #[doc(hidden)]
    pub fn with_wire_codec_info(mut self, info: Option<crate::link_wire::WireCodecInfo>) -> Self {
        self.wire_codec = info;
        self
    }
    #[doc(hidden)]
    pub fn with_wire_finalizer<F>(mut self, replace: bool, finalize: F) -> Self
    where
        F: Fn(OutboundLink<T>) -> Result<OutboundLink<T>, String> + Send + Sync + 'static,
    {
        if replace || self.wire_finalizer.is_none() {
            self.wire_finalizer = Some(Arc::new(finalize));
        }
        self
    }

    fn detach(self) -> (R, OutboundLink<T>) {
        let link = OutboundLinkBuilder {
            at: Detached,
            wire: self.wire,
            wire_codec: self.wire_codec,
            wire_finalizer: self.wire_finalizer,
            url: self.url,
            connector: self.connector,
            config: self.config,
            context_serializer: self.context_serializer,
            context_serializer_into: self.context_serializer_into,
            topic_writer: self.topic_writer,
        };
        (self.at, link)
    }

    /// Adds a configuration option to the connector
    pub fn with_config(mut self, key: &str, value: &str) -> Self {
        self.config.push((key.to_string(), value.to_string()));
        self
    }

    /// Drops any recorded [`WIRE_FORMAT_KEY`](crate::connector::WIRE_FORMAT_KEY):
    /// a newly installed codec closure has no known format until a codec verb
    /// records one.
    fn clear_wire_format(mut self) -> Self {
        self.wire_codec = None;
        self.config
            .retain(|(key, _)| key != crate::connector::WIRE_FORMAT_KEY);
        self
    }

    /// Sets the serialization callback
    ///
    /// The closure receives the [`RuntimeContext`](crate::RuntimeContext) for
    /// platform-independent timestamps and logging, plus the typed value being
    /// serialized. Ignore the context parameter (`|_ctx, value| …`) when it is
    /// not needed. Removes any recorded
    /// [`WIRE_FORMAT_KEY`](crate::connector::WIRE_FORMAT_KEY).
    pub fn with_serializer<F>(mut self, f: F) -> Self
    where
        F: Fn(crate::RuntimeContext, &T) -> Result<Vec<u8>, crate::connector::SerializeError>
            + Send
            + Sync
            + 'static,
    {
        self.context_serializer = Some(Arc::new(f));
        self.clear_wire_format()
    }

    /// Adds an into-slice fast path beside the required owned serializer.
    ///
    /// One zero-initialized scratch buffer of `capacity` bytes is allocated when
    /// each route pump starts, then reused for every message. A successful
    /// callback returns the initialized prefix length. The framework validates
    /// that length before borrowing the prefix. `BufferTooSmall` falls back to
    /// [`with_serializer`](Self::with_serializer) for that value; any other
    /// codec error is logged and the value is skipped.
    ///
    /// This method is an optimization only: callers must still install the
    /// owned serializer so oversized and legacy values retain their old behavior.
    /// Removes any recorded [`WIRE_FORMAT_KEY`](crate::connector::WIRE_FORMAT_KEY).
    pub fn with_serializer_into<F>(mut self, capacity: usize, f: F) -> Self
    where
        F: Fn(
                crate::RuntimeContext,
                &T,
                &mut [u8],
            ) -> Result<usize, crate::connector::SerializeError>
            + Send
            + Sync
            + 'static,
    {
        self.context_serializer_into = Some((capacity, Arc::new(f)));
        self.clear_wire_format()
    }

    /// Removes a previously installed into-slice serializer.
    ///
    /// Higher-level builder extensions use this when replacing a bounded
    /// serialization strategy with an owned-only one. Clearing the fast path
    /// prevents the old scratch serializer from emitting a different wire
    /// format beside the replacement owned serializer.
    pub fn clear_serializer_into(mut self) -> Self {
        self.context_serializer_into = None;
        self
    }

    /// Sets the operation timeout in milliseconds (the connector interprets
    /// it; passed as the `timeout_ms` option — see
    /// `ConnectorConfig::from_query`)
    ///
    /// Protocol-specific knobs (e.g. MQTT QoS/retain) are provided by the
    /// connector crates as extension traits over this builder, or generically
    /// via [`with_config`](Self::with_config).
    pub fn with_timeout_ms(mut self, timeout_ms: u32) -> Self {
        self.config
            .push(("timeout_ms".to_string(), timeout_ms.to_string()));
        self
    }

    /// Sets a [`TopicWriter`](crate::connector::TopicWriter) that writes each
    /// value's destination.
    ///
    /// `capacity` is the longest topic the writer produces, in bytes. A value
    /// whose topic does not fit is skipped and logged. For a closure, use
    /// [`with_topic_fn`](Self::with_topic_fn).
    pub fn with_topic_writer<W>(mut self, capacity: usize, writer: W) -> Self
    where
        W: crate::connector::TopicWriter<T> + 'static,
    {
        self.topic_writer = Some((capacity, Arc::new(writer)));
        self
    }

    /// Sets a closure that writes each value's destination; see
    /// [`with_topic_writer`](Self::with_topic_writer).
    ///
    /// ```rust,ignore
    /// .with_topic_fn(32, |v, out| {
    ///     write!(out, "sensors/{}/{}", v.site, v.id)?;
    ///     Ok(true)
    /// })
    /// ```
    pub fn with_topic_fn<F>(self, capacity: usize, f: F) -> Self
    where
        F: Fn(
                &T,
                &mut crate::connector::TopicBuf<'_>,
            ) -> Result<bool, crate::connector::TopicOverflow>
            + Send
            + Sync
            + 'static,
    {
        self.with_topic_writer(capacity, f)
    }
}

impl<'r, 'a, T, S: crate::SchemaState> OutboundLinkBuilder<T, &'r mut RecordRegistrar<'a, T, S>>
where
    T: Send + Sync + 'static + Debug + Clone,
{
    /// Finalizes the connector registration
    ///
    /// Configuration mistakes — an invalid URL, a missing serializer, or an
    /// unregistered scheme — are recorded instead of panicking: the link is
    /// **not** registered and `build()` reports every finding via
    /// `DbError::InvalidConfiguration`. The registrar is returned either way
    /// so chained configuration keeps compiling; the failed `build()` is the
    /// single error surface.
    ///
    /// The buffer requirement is validated by `build()` (calling `.buffer()`
    /// after `.link_to()` is fine).
    pub fn finish(self) -> &'r mut RecordRegistrar<'a, T, S> {
        let (registrar, link) = self.detach();
        registrar.push_outbound(link);
        registrar
    }
}

/// A contract link is an output only of a record whose key declares the
/// contract: `publish(..)` on a `Name<T>` record does not compile.
impl<T, S: crate::ContractDeclared> Output<T, S> for ContractOutboundLink<T>
where
    T: Send + Sync + 'static + Debug + Clone,
{
    fn wire(self, reg: &mut RecordRegistrar<'_, T, S>) {
        reg.push_outbound(self.attach(Detached));
    }
}

wire_link!(OutboundLink);

impl<T, S: crate::SchemaState> Output<T, S> for OutboundLink<T>
where
    T: Send + Sync + 'static + Debug + Clone,
{
    fn wire(self, reg: &mut RecordRegistrar<'_, T, S>) {
        reg.push_outbound(self);
    }
}
