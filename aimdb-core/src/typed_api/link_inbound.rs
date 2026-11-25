use alloc::{
    string::{String, ToString},
    sync::Arc,
    vec::Vec,
};

use core::fmt::Debug;

use crate::typed_api::registrar::{wire_link, WireLink};
use crate::typed_api::{Detached, DetachedContract, Input, Producer, RecordRegistrar};
use crate::AimDb;

// ============================================================================
// InboundLinkBuilder - Fluent inbound connector configuration
// ============================================================================

/// The producer an inbound ingest factory writes to. Factories run during
/// build() after every record is registered and validated, so a failed lookup
/// is an aimdb bug, not a user mistake.
#[allow(
    clippy::panic,
    reason = "the factory returns no Result and this lookup was validated at build() time"
)]
fn inbound_producer<T>(db: &AimDb, record_key: &str) -> Producer<T>
where
    T: Send + Sync + 'static + Debug + Clone,
{
    let writer = db
        .inner()
        .get_typed_record_by_key_raw::<T>(record_key)
        .and_then(|rec| rec.writer_handle())
        .unwrap_or_else(|e| {
            panic!(
                "ingest factory: no producer for record '{record_key}' ({e:?}) — \
                 this is a bug in aimdb-core"
            )
        });
    Producer::<T>::new(writer)
}

/// Fused ingest factory: resolves the typed producer once at route-collection
/// time; per message the returned closure runs deserialize + produce with no
/// erasure crossing.
pub(super) fn ingest_factory<T>(
    record_key: String,
    deser: TypedMatchDeserializerFn<T>,
) -> crate::connector::IngestFactoryFn
where
    T: Send + Sync + 'static + Debug + Clone,
{
    Arc::new(move |db: &AimDb| {
        let producer = inbound_producer::<T>(db, &record_key);
        let deser = deser.clone();
        Arc::new(
            move |ctx: &crate::RuntimeContext, m: &crate::TopicMatch<'_>, payload: &[u8]| {
                let admitted = deser(ctx, m, payload)?;
                producer.produce(admitted.value);
                Ok(admitted.outcome)
            },
        ) as crate::connector::IngestFn
    })
}

/// Type alias for typed context-aware deserializer callbacks
///
/// Stays typed until registration fuses it with the producer — no
/// per-message erasure.
type TypedContextDeserializerFn<T> = Arc<
    dyn Fn(crate::RuntimeContext, &[u8]) -> Result<crate::IngestValue<T>, crate::IngestError>
        + Send
        + Sync
        + 'static,
>;

/// Like [`TypedContextDeserializerFn`], also receiving the topic match.
pub(super) type TypedMatchDeserializerFn<T> = Arc<
    dyn Fn(
            &crate::RuntimeContext,
            &crate::TopicMatch<'_>,
            &[u8],
        ) -> Result<crate::IngestValue<T>, crate::IngestError>
        + Send
        + Sync
        + 'static,
>;

type InboundWireFinalizer<T> =
    Arc<dyn Fn(InboundLink<T>) -> Result<InboundLink<T>, String> + Send + Sync>;

/// Inbound link builder attached to a record: what
/// [`RecordRegistrar::link_from`] returns.
///
/// `'r` is the borrow of the registrar taken by `link_from()`; `'a` is the
/// registrar's own borrow of the record being configured.
pub type InboundConnectorBuilder<'r, 'a, T, S = crate::Undeclared> =
    InboundLinkBuilder<T, &'r mut RecordRegistrar<'a, T, S>>;

/// Inbound link not attached to a record yet, built with
/// [`InboundLink::new`] and registered by [`RecordRegistrar::input`].
pub type InboundLink<T> = InboundLinkBuilder<T, Detached>;

/// Inbound link carrying a data contract, built by
/// [`InboundLink::into_contract`]: an [`Input`] only of a record whose key
/// declares the contract.
pub type ContractInboundLink<T> = InboundLinkBuilder<T, DetachedContract>;

/// Builder for configuring inbound connector links (External → AimDB)
///
/// `R` is what the builder is attached to. [`RecordRegistrar::link_from`]
/// returns the attached form, [`InboundConnectorBuilder`], which
/// [`finish()`](InboundLinkBuilder::finish) registers on its record.
/// [`InboundLink::new`] builds the detached form, [`InboundLink`], before
/// any record exists; [`RecordRegistrar::input`] registers it, and checks
/// it then, not when it is built. Both register the same link:
///
/// ```rust,ignore
/// reg.link_from("mqtt://in").with_deserializer(de).finish();
/// reg.input(InboundLink::new("mqtt://in").with_deserializer(de));
/// ```
///
/// The option methods below, and the connector crates' extension traits,
/// work on both forms.
#[must_use = "a link does nothing until it is passed to `.input()` or finished with `.finish()`"]
pub struct InboundLinkBuilder<T: Send + Sync + 'static + Debug + Clone, R> {
    wire: crate::link_wire::WireOptions,
    wire_codec: Option<crate::link_wire::WireCodecInfo>,
    wire_finalizer: Option<InboundWireFinalizer<T>>,
    at: R,
    pub(super) url: String,
    pub(super) connector: Option<crate::ConnectorHandle>,
    pub(super) config: Vec<(String, String)>,
    pub(super) context_deserializer: Option<TypedContextDeserializerFn<T>>,
    pub(super) match_deserializer: Option<TypedMatchDeserializerFn<T>>,
    pub(super) upgrade_versions: Vec<u32>,
    pub(super) key: Option<(String, u16)>,
    pub(super) topic_resolver: Option<crate::connector::TopicResolverFn>,
}

impl<T> InboundLinkBuilder<T, Detached>
where
    T: Send + Sync + 'static + Debug + Clone,
{
    /// Starts an inbound link from `address` on a connector registered with
    /// [`AimDbBuilder::connect`](crate::AimDbBuilder::connect).
    ///
    /// `address` is the part after `scheme://`; the connector supplies the
    /// scheme. A full URL (as a `#[link_address]` key gives) is accepted if
    /// its scheme is the connector's. A different scheme, or a handle from
    /// another builder, is reported by `build()` once the link is passed to
    /// [`RecordRegistrar::input`].
    pub fn on(connector: crate::ConnectorHandle, address: &str) -> Self {
        Self {
            connector: Some(connector),
            ..Self::new(address)
        }
    }

    /// Starts an inbound link from `url` that is not attached to a record yet.
    pub fn new(url: &str) -> Self {
        Self {
            at: Detached,
            wire: Default::default(),
            wire_codec: None,
            wire_finalizer: None,
            url: url.to_string(),
            connector: None,
            config: Vec::new(),
            context_deserializer: None,
            match_deserializer: None,
            upgrade_versions: Vec::new(),
            key: None,
            topic_resolver: None,
        }
    }

    /// Marks this link as carrying a data contract, so it wires only into a
    /// record whose key declares the contract. `aimdb-data-contracts`'
    /// `subscribe` calls this; the link's options are unchanged.
    pub fn into_contract(self) -> ContractInboundLink<T> {
        self.attach(DetachedContract)
    }
}

impl<T, A> InboundLinkBuilder<T, A>
where
    T: Send + Sync + 'static + Debug + Clone,
{
    pub(super) fn attach<R>(self, at: R) -> InboundLinkBuilder<T, R> {
        InboundLinkBuilder {
            at,
            wire: self.wire,
            wire_codec: self.wire_codec,
            wire_finalizer: self.wire_finalizer,
            url: self.url,
            connector: self.connector,
            config: self.config,
            context_deserializer: self.context_deserializer,
            match_deserializer: self.match_deserializer,
            upgrade_versions: self.upgrade_versions,
            key: self.key,
            topic_resolver: self.topic_resolver,
        }
    }
}

impl<T, R> InboundLinkBuilder<T, R>
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
        F: Fn(InboundLink<T>) -> Result<InboundLink<T>, String> + Send + Sync + 'static,
    {
        if replace || self.wire_finalizer.is_none() {
            self.wire_finalizer = Some(Arc::new(finalize));
        }
        self
    }

    fn detach(self) -> (R, InboundLink<T>) {
        let link = InboundLinkBuilder {
            at: Detached,
            wire: self.wire,
            wire_codec: self.wire_codec,
            wire_finalizer: self.wire_finalizer,
            url: self.url,
            connector: self.connector,
            config: self.config,
            context_deserializer: self.context_deserializer,
            match_deserializer: self.match_deserializer,
            upgrade_versions: self.upgrade_versions,
            key: self.key,
            topic_resolver: self.topic_resolver,
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

    /// Sets the deserialization callback
    ///
    /// The closure receives the [`RuntimeContext`](crate::RuntimeContext) for
    /// platform-independent timestamps and logging, plus the raw bytes from
    /// the external system. Ignore the context parameter (`|_ctx, data| …`)
    /// when it is not needed. Removes any recorded
    /// [`WIRE_FORMAT_KEY`](crate::connector::WIRE_FORMAT_KEY).
    pub fn with_deserializer<F>(mut self, f: F) -> Self
    where
        F: Fn(crate::RuntimeContext, &[u8]) -> Result<T, crate::IngestError>
            + Send
            + Sync
            + 'static,
    {
        self.context_deserializer = Some(Arc::new(move |ctx, bytes| {
            f(ctx, bytes).map(crate::IngestValue::current)
        }));
        self.upgrade_versions.clear();
        self.clear_wire_format()
    }

    /// Like [`with_deserializer`](Self::with_deserializer), also receiving the
    /// topic the message arrived on, its captures and its key. The context is
    /// borrowed, so no reference count changes per message. Removes any
    /// recorded [`WIRE_FORMAT_KEY`](crate::connector::WIRE_FORMAT_KEY).
    pub fn with_match_deserializer<F>(mut self, f: F) -> Self
    where
        F: Fn(
                &crate::RuntimeContext,
                &crate::TopicMatch<'_>,
                &[u8],
            ) -> Result<T, crate::IngestError>
            + Send
            + Sync
            + 'static,
    {
        self.match_deserializer = Some(Arc::new(move |ctx, m, bytes| {
            f(ctx, m, bytes).map(crate::IngestValue::current)
        }));
        self.upgrade_versions.clear();
        self.clear_wire_format()
    }

    /// Installs contract admission with success metadata and declared source
    /// versions. Counters are allocated once at registration. Like the match
    /// deserializer, this borrows the context and receives wildcard captures.
    pub fn with_admitting_deserializer<F>(mut self, versions: &[u32], f: F) -> Self
    where
        F: Fn(
                &crate::RuntimeContext,
                &crate::TopicMatch<'_>,
                &[u8],
            ) -> Result<crate::IngestValue<T>, crate::IngestError>
            + Send
            + Sync
            + 'static,
    {
        self.match_deserializer = Some(Arc::new(f));
        self.upgrade_versions = versions.to_vec();
        self.clear_wire_format()
    }

    /// Clear both inbound callback slots before installing a complete strategy.
    #[doc(hidden)]
    pub fn clear_deserializers(mut self) -> Self {
        self.context_deserializer = None;
        self.match_deserializer = None;
        self
    }

    /// Assigns each value of capture `name` a [`KeyId`](crate::KeyId), up to
    /// `capacity` values per record. Messages with a value beyond that are
    /// dropped. A value keeps its key even if its payload fails to
    /// deserialize, and keys are never freed, so anyone who can publish under
    /// the pattern can fill the table.
    pub fn key(mut self, name: &str, capacity: u16) -> Self {
        self.key = Some((name.to_string(), capacity));
        self
    }

    /// Sets the operation timeout in milliseconds (the connector interprets
    /// it; passed as the `timeout_ms` option — see
    /// `ConnectorConfig::from_query`)
    ///
    /// Protocol-specific knobs (e.g. MQTT subscribe QoS) are provided by the
    /// connector crates as extension traits over this builder, or generically
    /// via [`with_config`](Self::with_config).
    pub fn with_timeout_ms(mut self, timeout_ms: u32) -> Self {
        self.config
            .push(("timeout_ms".to_string(), timeout_ms.to_string()));
        self
    }

    /// Sets a dynamic topic resolver for late-binding scenarios
    ///
    /// The resolver is called once at connector startup to determine
    /// the subscription topic. Return `None` to use the default
    /// static topic from the URL.
    ///
    /// # Use Cases
    ///
    /// - Topics determined from smart contracts at runtime
    /// - Service discovery integration
    /// - Environment-specific topic configuration
    pub fn with_topic_resolver<F>(mut self, resolver: F) -> Self
    where
        F: Fn() -> Option<String> + Send + Sync + 'static,
    {
        self.topic_resolver = Some(Arc::new(resolver));
        self
    }
}

impl<'r, 'a, T, S: crate::SchemaState> InboundLinkBuilder<T, &'r mut RecordRegistrar<'a, T, S>>
where
    T: Send + Sync + 'static + Debug + Clone,
{
    /// Finalizes the inbound connector registration
    ///
    /// Configuration mistakes — an invalid URL, a missing deserializer, an
    /// unregistered scheme, or a conflict with `.source()`/`.transform()`
    /// (local producer + inbound connector would race as last-writer-wins) —
    /// are recorded instead of panicking: the link is **not** registered and
    /// `build()` reports every finding via `DbError::InvalidConfiguration`.
    /// The registrar is returned either way so chained configuration keeps
    /// compiling; the failed `build()` is the single error surface.
    ///
    /// The buffer requirement is validated by `build()` (calling `.buffer()`
    /// after `.link_from()` is fine).
    pub fn finish(self) -> &'r mut RecordRegistrar<'a, T, S> {
        let (registrar, link) = self.detach();
        registrar.push_inbound(link);
        registrar
    }
}

/// A contract link is an input only of a record whose key declares the
/// contract: `subscribe(..)` on a `Name<T>` record does not compile.
impl<T, S: crate::ContractDeclared> Input<T, S> for ContractInboundLink<T>
where
    T: Send + Sync + 'static + Debug + Clone,
{
    fn wire(self, reg: &mut RecordRegistrar<'_, T, S>) {
        reg.push_inbound(self.attach(Detached));
    }
}

wire_link!(InboundLink);

impl<T, S: crate::SchemaState> Input<T, S> for InboundLink<T>
where
    T: Send + Sync + 'static + Debug + Clone,
{
    fn wire(self, reg: &mut RecordRegistrar<'_, T, S>) {
        reg.push_inbound(self);
    }
}
