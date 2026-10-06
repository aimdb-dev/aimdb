//! Connector infrastructure for external protocol integration
//!
//! Provides the `.link_to()` / `.link_from()` builder API for ergonomic
//! connector setup with automatic client lifecycle management. Connectors
//! bridge AimDB records to external systems (MQTT, KNX, WebSocket, …).
//!
//! # Design Philosophy
//!
//! - **User Extensions**: Connector implementations are provided by users
//! - **Shared Clients**: Single client instance shared across tasks (Arc or static)
//! - **No Buffering**: Direct access to protocol clients, no intermediate queues
//! - **Type Safety**: Compile-time guarantees via typed handlers
//!
//! # Example
//!
//! ```no_run
//! # use aimdb_core::AimDbBuilder;
//! # #[derive(Clone, Debug)] struct WeatherAlert { level: u8 }
//! # fn wire(builder: &mut AimDbBuilder) {
//! builder.configure::<WeatherAlert>("weather.alert", |reg| {
//!     // .buffer(BufferCfg::SingleLatest) — via your runtime adapter's ext trait
//!     reg.link_to("mqtt://alerts/weather")
//!         .with_serializer(|_ctx, alert: &WeatherAlert| Ok(vec![alert.level]))
//!         .finish();
//! });
//! # }
//! ```

use core::fmt::{self, Debug};
use core::future::Future;
use core::pin::Pin;

use alloc::{
    boxed::Box,
    string::{String, ToString},
    sync::Arc,
    vec::Vec,
};

use alloc::format;

use crate::{builder::AimDb, DbResult};

/// Error shared by outbound record serialization operations.
///
/// Deliberately separate from the session `CodecError`, which describes
/// failures in an envelope codec (AimX, WebSocket, and similar framed
/// protocols). A link codec sits one layer higher: it turns a typed record into
/// the opaque payload passed to a connector.
///
/// Uses an enum instead of `String` for predictable `no_std` behavior and
/// `defmt` logging support on Embassy targets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SerializeError {
    /// Output buffer is too small for the serialized data
    BufferTooSmall,

    /// Invalid data that cannot be serialized
    InvalidData,
}

#[cfg(feature = "defmt")]
impl defmt::Format for SerializeError {
    fn format(&self, f: defmt::Formatter) {
        match self {
            Self::BufferTooSmall => defmt::write!(f, "BufferTooSmall"),
            Self::InvalidData => defmt::write!(f, "InvalidData"),
        }
    }
}

#[cfg(feature = "std")]
impl std::fmt::Display for SerializeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BufferTooSmall => write!(f, "Output buffer too small"),
            Self::InvalidData => write!(f, "Invalid data for serialization"),
        }
    }
}

#[cfg(feature = "std")]
impl std::error::Error for SerializeError {}

// ============================================================================
// TopicWriter - Destinations written into bounded storage
// ============================================================================

/// Writes an outbound link's destination for each value (outbound only).
///
/// Closures implement it, and [`with_topic_fn`](crate::typed_api::OutboundConnectorBuilder::with_topic_fn)
/// takes one directly. A writer type goes to
/// [`with_topic_writer`](crate::typed_api::OutboundConnectorBuilder::with_topic_writer).
///
/// # Example
///
/// ```rust
/// use aimdb_core::connector::{TopicBuf, TopicOverflow, TopicWriter};
/// use core::fmt::Write;
/// # #[derive(Clone, Debug)] struct Temperature { sensor_id: u32 }
///
/// struct SensorTopic;
///
/// impl TopicWriter<Temperature> for SensorTopic {
///     fn write_topic(&self, value: &Temperature, out: &mut TopicBuf<'_>) -> Result<bool, TopicOverflow> {
///         write!(out, "sensors/temp/{}", value.sensor_id)?;
///         Ok(true)
///     }
/// }
/// ```
pub trait TopicWriter<T>: Send + Sync {
    /// Write the destination for `value` into `out`.
    ///
    /// `Ok(true)` publishes to what was written, `Ok(false)` to the static
    /// topic from the `link_to()` URL. A value whose topic does not fit is
    /// skipped, whatever this returns; the topic is never truncated.
    fn write_topic(&self, value: &T, out: &mut TopicBuf<'_>) -> Result<bool, TopicOverflow>;
}

impl<T, F> TopicWriter<T> for F
where
    F: Fn(&T, &mut TopicBuf<'_>) -> Result<bool, TopicOverflow> + Send + Sync,
{
    fn write_topic(&self, value: &T, out: &mut TopicBuf<'_>) -> Result<bool, TopicOverflow> {
        self(value, out)
    }
}

/// A topic did not fit in its link's topic capacity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TopicOverflow;

impl From<core::fmt::Error> for TopicOverflow {
    fn from(_: core::fmt::Error) -> Self {
        TopicOverflow
    }
}

/// Bounded topic storage handed to a [`TopicWriter`].
///
/// A write that does not fit is refused whole, so the contents are always
/// valid UTF-8. After the first refused write every later write is refused
/// too.
pub struct TopicBuf<'a> {
    buf: &'a mut [u8],
    len: usize,
    overflowed: bool,
}

impl<'a> TopicBuf<'a> {
    /// An empty topic over `buf`; its capacity is `buf.len()`.
    pub fn new(buf: &'a mut [u8]) -> Self {
        Self {
            buf,
            len: 0,
            overflowed: false,
        }
    }

    /// Append `s`, or refuse it whole if it does not fit.
    pub fn push_str(&mut self, s: &str) -> Result<(), TopicOverflow> {
        let end = self.len + s.len();
        match self.buf.get_mut(self.len..end) {
            Some(dst) if !self.overflowed => {
                dst.copy_from_slice(s.as_bytes());
                self.len = end;
                Ok(())
            }
            _ => {
                self.overflowed = true;
                Err(TopicOverflow)
            }
        }
    }

    /// Bytes written so far.
    pub fn len(&self) -> usize {
        self.len
    }

    /// Whether nothing has been written.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// The most bytes this topic can hold.
    pub fn capacity(&self) -> usize {
        self.buf.len()
    }

    /// The topic written so far.
    pub fn as_str(&self) -> &str {
        // Only whole `&str`s are ever copied in, so this cannot fail.
        self.buf
            .get(..self.len)
            .and_then(|b| core::str::from_utf8(b).ok())
            .unwrap_or_default()
    }

    /// Whether a write was refused.
    pub(crate) fn overflowed(&self) -> bool {
        self.overflowed
    }
}

impl core::fmt::Write for TopicBuf<'_> {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        self.push_str(s).map_err(|_| core::fmt::Error)
    }
}

/// Address of a record link: `scheme://resource` (e.g. `mqtt://sensors/temp`).
///
/// A link address names a topic/resource on a connector's already-configured
/// endpoint — it is *not* a URL: there is no host, port, or credential
/// component (the connector constructor takes the endpoint
/// [`ConnectorUrl`]). The scheme selects which registered connector serves
/// the link; the resource is passed to it verbatim (e.g. an MQTT topic, a
/// KNX group address).
///
/// A trailing `?key=value` query is accepted and ignored; per-link options
/// are passed via `.with_config()` / `.with_timeout_ms()`.
#[derive(Clone, Debug, PartialEq)]
pub struct LinkAddress {
    scheme: String,
    resource: String,
}

impl LinkAddress {
    /// Parses a `scheme://resource` link address.
    ///
    /// # Example
    ///
    /// ```rust
    /// use aimdb_core::connector::LinkAddress;
    ///
    /// let addr = LinkAddress::parse("mqtt://sensors/temp").unwrap();
    /// assert_eq!(addr.scheme(), "mqtt");
    /// assert_eq!(addr.resource_id(), "sensors/temp");
    /// ```
    pub fn parse(s: &str) -> DbResult<Self> {
        use crate::DbError;

        let (scheme, rest) = s
            .split_once("://")
            .ok_or_else(|| DbError::InvalidOperation {
                operation: "LinkAddress::parse".into(),
                reason: alloc::format!("link address '{s}' missing '://' separator"),
            })?;

        // Per-link options travel via .with_config(); a query here is legal
        // input but carries no meaning — strip it.
        let resource = rest.split_once('?').map(|(r, _)| r).unwrap_or(rest);
        let resource = resource.trim_start_matches('/');

        if scheme.is_empty() || resource.is_empty() {
            return Err(DbError::InvalidOperation {
                operation: "LinkAddress::parse".into(),
                reason: alloc::format!("link address '{s}' needs both a scheme and a resource"),
            });
        }

        Ok(Self {
            scheme: scheme.to_string(),
            resource: resource.to_string(),
        })
    }

    /// Returns the scheme (selects the registered connector, e.g. `"mqtt"`).
    pub fn scheme(&self) -> &str {
        &self.scheme
    }

    /// Returns the resource identifier the connector addresses (topic, group
    /// address, path — passed to the connector verbatim).
    pub fn resource_id(&self) -> &str {
        &self.resource
    }
}

impl fmt::Display for LinkAddress {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}://{}", self.scheme, self.resource)
    }
}

/// Parsed connector *endpoint* URL with protocol, host, port, and credentials
///
/// This is the URL a connector is constructed with (broker/gateway/server
/// endpoint) — record links use the far simpler [`LinkAddress`] instead.
/// The parser is scheme-agnostic: any `scheme://…` URL parses. Connectors in
/// this workspace use e.g.:
/// - MQTT: `mqtt://host:port`, `mqtts://host:port`
/// - KNX: `knx://gateway:3671`
/// - WebSocket: `ws://host:port/path`, `wss://host:port/path`
#[derive(Clone, Debug, PartialEq)]
pub struct ConnectorUrl {
    /// Protocol scheme (e.g. mqtt, mqtts, knx, ws, wss, uds, serial)
    pub scheme: String,

    /// Host, or a comma-separated host list (preserved verbatim)
    pub host: String,

    /// Port number (optional, protocol-specific defaults)
    pub port: Option<u16>,

    /// Path component (optional)
    pub path: Option<String>,

    /// Username for authentication (optional)
    pub username: Option<String>,

    /// Password for authentication (optional)
    pub password: Option<String>,

    /// Query parameters (optional, parsed from URL)
    pub query_params: Vec<(String, String)>,
}

impl ConnectorUrl {
    /// Parses a connector URL string
    ///
    /// # Supported Formats
    ///
    /// - `mqtt://host:port`
    /// - `mqtt://user:pass@host:port`
    /// - `mqtts://host:port` (TLS)
    /// - `knx://gateway:3671`
    /// - `ws://host:port/path?key=value` (WebSocket)
    /// - `wss://host:port/path` (WebSocket Secure)
    ///
    /// Any other `scheme://…` parses the same way; comma-separated host
    /// lists are preserved verbatim in [`host`](ConnectorUrl::host).
    ///
    /// # Example
    ///
    /// ```rust
    /// use aimdb_core::connector::ConnectorUrl;
    ///
    /// let url = ConnectorUrl::parse("mqtt://user:pass@broker.example.com:1883").unwrap();
    /// assert_eq!(url.scheme, "mqtt");
    /// assert_eq!(url.host, "broker.example.com");
    /// assert_eq!(url.port, Some(1883));
    /// assert_eq!(url.username, Some("user".to_string()));
    /// ```
    pub fn parse(url: &str) -> DbResult<Self> {
        parse_connector_url(url)
    }

    /// Returns the default port for this protocol scheme
    pub fn default_port(&self) -> Option<u16> {
        match self.scheme.as_str() {
            "mqtt" | "ws" => Some(1883),
            "mqtts" | "wss" => Some(8883),
            _ => None,
        }
    }

    /// Returns the effective port (explicit or default)
    pub fn effective_port(&self) -> Option<u16> {
        self.port.or_else(|| self.default_port())
    }

    /// Returns true if this is a secure connection (TLS)
    pub fn is_secure(&self) -> bool {
        matches!(self.scheme.as_str(), "mqtts" | "https" | "wss")
    }

    /// Returns the URL scheme (protocol)
    pub fn scheme(&self) -> &str {
        &self.scheme
    }

    /// Returns the path component, or "/" if not specified
    pub fn path(&self) -> &str {
        self.path.as_deref().unwrap_or("/")
    }
}

impl fmt::Display for ConnectorUrl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}://", self.scheme)?;

        if let Some(ref username) = self.username {
            write!(f, "{}", username)?;
            if self.password.is_some() {
                write!(f, ":****")?; // Don't expose password in Display
            }
            write!(f, "@")?;
        }

        write!(f, "{}", self.host)?;

        if let Some(port) = self.port {
            write!(f, ":{}", port)?;
        }

        if let Some(ref path) = self.path {
            if !path.starts_with('/') {
                write!(f, "/")?;
            }
            write!(f, "{}", path)?;
        }

        Ok(())
    }
}

/// Configuration for an outbound connector link
///
/// Stores the parsed URL, configuration, and the route factory until the
/// database is built. `OutboundRoutes` runs the factory once per link.
#[derive(Clone)]
pub struct ConnectorLink {
    /// Parsed link address (`scheme://resource`)
    pub url: LinkAddress,

    /// Additional configuration options (protocol-specific)
    pub config: Vec<(String, String)>,

    /// Builds the link's route for `OutboundRoutes`.
    pub(crate) route_factory: crate::outbound::RouteFactoryFn,
}

impl Debug for ConnectorLink {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ConnectorLink")
            .field("url", &self.url)
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

impl ConnectorLink {
    /// Creates a new connector link from a link address and route factory
    pub(crate) fn new(url: LinkAddress, route_factory: crate::outbound::RouteFactoryFn) -> Self {
        Self {
            url,
            config: Vec::new(),
            route_factory,
        }
    }
}

/// Fused inbound ingest callback: deserialize + produce in one typed closure
///
/// Built where the record type `T` is known (`InboundConnectorBuilder::finish`),
/// so no `Box<dyn Any>` crosses the connector boundary per message. The closure
/// captures the typed producer and deserializer; callers only see bytes.
///
/// Synchronous by design: `Producer<T>::produce` is sync and infallible
///, so the only failure is the user
/// deserializer's — reported as the same `String` the deserializer API uses.
///
/// The [`RuntimeContext`](crate::RuntimeContext) and the
/// [`TopicMatch`](crate::TopicMatch) the message arrived on are threaded per
/// call (not captured).
pub type IngestFn = Arc<
    dyn Fn(&crate::RuntimeContext, &crate::TopicMatch<'_>, &[u8]) -> Result<(), String>
        + Send
        + Sync,
>;

/// Type alias for ingest factory callback (alloc feature)
///
/// Takes the live [`AimDb`] and returns the fused [`IngestFn`]. This allows
/// capturing the record type T at link_from() time while storing the factory
/// in a type-erased InboundConnectorLink. The factory runs once at
/// route-collection time, not per message.
///
/// Available in both `std` and `no_std + alloc` environments.
pub type IngestFactoryFn = Arc<dyn Fn(&AimDb) -> IngestFn + Send + Sync>;

/// Topic resolver function for inbound connections (late-binding)
///
/// Called once at connector startup to resolve the subscription topic.
/// Returns `Some(topic)` to use a dynamic topic, or `None` to fall back
/// to the static topic from the `link_from()` URL.
///
/// # Use Cases
///
/// - Topics determined from smart contracts at runtime
/// - Service discovery integration
/// - Environment-specific topic configuration
/// - Topics read from configuration files or databases
///
/// # no_std Compatibility
///
/// Works in both `std` and `no_std + alloc` environments.
pub type TopicResolverFn = Arc<dyn Fn() -> Option<String> + Send + Sync>;

/// Configuration for an inbound connector link (External → AimDB)
///
/// Stores the parsed URL, configuration, and the fused ingest factory. The
/// factory captures the type T at creation time, allowing type-safe
/// deserialize+produce later without needing PhantomData or type parameters.
#[derive(Clone)]
#[non_exhaustive]
pub struct InboundConnectorLink {
    /// Parsed link address (`scheme://resource`)
    pub url: LinkAddress,

    /// Additional configuration options (protocol-specific)
    pub config: Vec<(String, String)>,

    /// Fused ingest factory (alloc feature)
    ///
    /// Takes the live [`AimDb`] and returns the [`IngestFn`] that
    /// deserializes bytes and produces into the record's buffer in one typed
    /// closure. Captures the record type T at link_from() call time —
    /// `finish()` validates the deserializer is present before registering
    /// the link, so the factory is always set.
    ///
    /// Available in both `std` and `no_std + alloc` environments.
    pub ingest_factory: IngestFactoryFn,

    /// Set by `.key(..)`: the keyed capture and the key table's capacity.
    pub key: Option<(String, core::num::NonZeroU16)>,

    /// Optional dynamic topic resolver (late-binding)
    ///
    /// Called once at connector startup to determine the subscription topic.
    /// If the resolver returns `None`, the static topic from the URL is used.
    ///
    /// Available in both `std` and `no_std + alloc` environments.
    pub topic_resolver: Option<TopicResolverFn>,
}

impl Debug for InboundConnectorLink {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("InboundConnectorLink")
            .field("url", &self.url)
            .field("config", &self.config)
            .field("ingest_factory", &"<factory>")
            .field("key", &self.key)
            .field(
                "topic_resolver",
                &self.topic_resolver.as_ref().map(|_| "<function>"),
            )
            .finish()
    }
}

impl InboundConnectorLink {
    /// Creates a new inbound connector link from a link address and ingest factory
    pub fn new(url: LinkAddress, ingest_factory: IngestFactoryFn) -> Self {
        Self {
            url,
            config: Vec::new(),
            ingest_factory,
            key: None,
            topic_resolver: None,
        }
    }

    /// Creates the fused ingest callback using the stored factory.
    ///
    /// Runs once at route-collection time; the returned [`IngestFn`] is the
    /// per-message path (deserialize + produce, no erasure crossing).
    ///
    /// Available in both `std` and `no_std + alloc` environments.
    pub fn create_ingest(&self, db: &AimDb) -> IngestFn {
        (self.ingest_factory)(db)
    }

    /// Resolves the subscription topic for this link
    ///
    /// If a topic resolver is configured, calls it to determine the topic.
    /// Otherwise, returns the static topic from the URL.
    ///
    /// This is called once at connector startup.
    pub fn resolve_topic(&self) -> String {
        self.topic_resolver
            .as_ref()
            .and_then(|resolver| resolver())
            .unwrap_or_else(|| self.url.resource_id().to_string())
    }
}

/// Parses a connector URL string into structured components
///
/// This is a simple parser that handles the most common URL formats.
/// For production use, consider using the `url` crate with feature flags.
fn parse_connector_url(url: &str) -> DbResult<ConnectorUrl> {
    use crate::DbError;

    // Split scheme from rest
    let (scheme, rest) = url
        .split_once("://")
        .ok_or_else(|| DbError::InvalidOperation {
            operation: "parse_connector_url".into(),
            reason: format!("Missing scheme in URL: {}", url),
        })?;

    // Extract credentials if present (user:pass@host)
    let (credentials, host_part) = if let Some(at_idx) = rest.find('@') {
        let creds = &rest[..at_idx];
        let host = &rest[at_idx + 1..];
        (Some(creds), host)
    } else {
        (None, rest)
    };

    let (username, password) = if let Some(creds) = credentials {
        if let Some((user, pass)) = creds.split_once(':') {
            (Some(user.to_string()), Some(pass.to_string()))
        } else {
            (Some(creds.to_string()), None)
        }
    } else {
        (None, None)
    };

    // Split path and query from host:port
    let (host_port, path, query_params) = if let Some(slash_idx) = host_part.find('/') {
        let hp = &host_part[..slash_idx];
        let path_query = &host_part[slash_idx..];

        // Split query parameters
        let (path_part, query_part) = if let Some(q_idx) = path_query.find('?') {
            (&path_query[..q_idx], Some(&path_query[q_idx + 1..]))
        } else {
            (path_query, None)
        };

        // Parse query parameters
        let params = if let Some(query) = query_part {
            query
                .split('&')
                .filter_map(|pair| {
                    let (k, v) = pair.split_once('=')?;
                    Some((k.to_string(), v.to_string()))
                })
                .collect()
        } else {
            Vec::new()
        };

        (hp, Some(path_part.to_string()), params)
    } else {
        (host_part, None, Vec::new())
    };

    // Split host and port
    let (host, port) = if let Some(colon_idx) = host_port.rfind(':') {
        let h = &host_port[..colon_idx];
        let p = &host_port[colon_idx + 1..];
        let port_num = p.parse::<u16>().ok();
        (h.to_string(), port_num)
    } else {
        (host_port.to_string(), None)
    };

    Ok(ConnectorUrl {
        scheme: scheme.to_string(),
        host,
        port,
        path,
        username,
        password,
        query_params,
    })
}

/// Trait for building connectors after the database is constructed
///
/// Connectors that need to collect routes from the database (for inbound routing)
/// implement this trait. The builder pattern allows connectors to be constructed
/// in two phases:
///
/// 1. Configuration phase: User provides broker URLs and settings
/// 2. Build phase: Connector collects routes from the database and initializes
///
/// # Example
///
/// Illustrative sketch of a connector author's `build()` (not compiled: the
/// client types and `MqttGrammar`, the connector's
/// [`TopicGrammar`](crate::TopicGrammar), are the connector's own — see
/// `aimdb-mqtt-connector` for a real one):
///
/// ```rust,ignore
/// pub struct MqttConnectorBuilder {
///     broker_url: String,
/// }
///
/// impl ConnectorBuilder for MqttConnectorBuilder {
///     fn build<'a>(
///         &'a self,
///         db: &'a AimDb,
///     ) -> Pin<Box<dyn Future<Output = DbResult<Vec<BoxFuture>>> + Send + 'a>> {
///         Box::pin(async move {
///             // Wildcard rules are the connector's; `&ExactGrammar` if it has none.
///             let inbound = InboundDispatch::new(db, self.scheme(), &MqttGrammar)?;
///             let outbound = OutboundRoutes::new(db, self.scheme())?;
///             let connector = MqttConnector::new(&self.broker_url, inbound, outbound).await?;
///             Ok(connector.futures())
///         })
///     }
///
///     fn scheme(&self) -> &str {
///         "mqtt"
///     }
/// }
/// ```
pub trait ConnectorBuilder: Send + Sync {
    /// Build the connector and return its driving futures.
    ///
    /// Called during `AimDbBuilder::build()` after the database has been
    /// constructed. The returned futures (typically the transport task)
    /// are appended to the builder's accumulator and driven by
    /// `AimDbRunner::run()`.
    ///
    /// # Arguments
    /// * `db` - The constructed database instance
    ///
    /// # Returns
    /// All `BoxFuture`s the connector needs to operate. Empty if the connector
    /// has no work to drive.
    #[allow(clippy::type_complexity)]
    fn build<'a>(
        &'a self,
        db: &'a AimDb,
    ) -> Pin<
        Box<
            dyn Future<Output = DbResult<Vec<Pin<Box<dyn Future<Output = ()> + Send + 'static>>>>>
                + Send
                + 'a,
        >,
    >;

    /// The URL scheme this connector handles
    ///
    /// Returns the scheme (e.g., "mqtt", "knx", "uds") that this connector
    /// will be registered under. Used for routing `.link_from()` and `.link_to()`
    /// declarations to the appropriate connector.
    fn scheme(&self) -> &str;

    /// Whether registering a second connector under this scheme is an error.
    ///
    /// Say `true` when [`build`](Self::build) claims every route for its
    /// scheme — [`InboundDispatch`](crate::InboundDispatch),
    /// [`OutboundRoutes`](crate::OutboundRoutes) and `crate::session`'s
    /// `pump_client` (left unlinked: that module is behind
    /// `connector-session`, and this trait is not) all filter by scheme
    /// alone, so two such connectors each collect *all* of it: every
    /// `link_to` gets two publishers, and the routes
    /// cannot be divided between the two endpoints because nothing in a route
    /// names which connector it belongs to. That misconfiguration is otherwise
    /// silent, and it fails as duplicated or misdirected traffic at runtime
    /// rather than at build.
    ///
    /// Leave it `false` — the default — for a connector that only serves what
    /// it is given, such as a session *server*: it binds its own listener and
    /// collects no routes, so two of them under one scheme are two endpoints
    /// onto the same dispatch, which is useful rather than broken.
    fn owns_scheme(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::format;

    #[test]
    fn test_parse_simple_mqtt() {
        let url = ConnectorUrl::parse("mqtt://broker.example.com:1883").unwrap();
        assert_eq!(url.scheme, "mqtt");
        assert_eq!(url.host, "broker.example.com");
        assert_eq!(url.port, Some(1883));
        assert_eq!(url.username, None);
        assert_eq!(url.password, None);
    }

    #[test]
    fn test_parse_mqtt_with_credentials() {
        let url = ConnectorUrl::parse("mqtt://user:pass@broker.example.com:1883").unwrap();
        assert_eq!(url.scheme, "mqtt");
        assert_eq!(url.host, "broker.example.com");
        assert_eq!(url.port, Some(1883));
        assert_eq!(url.username, Some("user".to_string()));
        assert_eq!(url.password, Some("pass".to_string()));
    }

    #[test]
    fn test_parse_https_with_path() {
        let url = ConnectorUrl::parse("https://api.example.com:8443/events").unwrap();
        assert_eq!(url.scheme, "https");
        assert_eq!(url.host, "api.example.com");
        assert_eq!(url.port, Some(8443));
        assert_eq!(url.path, Some("/events".to_string()));
    }

    #[test]
    fn test_parse_with_query_params() {
        let url = ConnectorUrl::parse("http://api.example.com/data?key=value&foo=bar").unwrap();
        assert_eq!(url.scheme, "http");
        assert_eq!(url.host, "api.example.com");
        assert_eq!(url.path, Some("/data".to_string()));
        assert_eq!(url.query_params.len(), 2);
        assert_eq!(
            url.query_params[0],
            ("key".to_string(), "value".to_string())
        );
        assert_eq!(url.query_params[1], ("foo".to_string(), "bar".to_string()));
    }

    #[test]
    fn test_default_ports() {
        let mqtt = ConnectorUrl::parse("mqtt://broker.local").unwrap();
        assert_eq!(mqtt.default_port(), Some(1883));
        assert_eq!(mqtt.effective_port(), Some(1883));

        let mqtts = ConnectorUrl::parse("mqtts://broker.local").unwrap();
        assert_eq!(mqtts.default_port(), Some(8883));

        // No connector, no default: unknown schemes carry no port opinion.
        let knx = ConnectorUrl::parse("knx://gateway.local").unwrap();
        assert_eq!(knx.default_port(), None);
    }

    #[test]
    fn test_is_secure() {
        assert!(ConnectorUrl::parse("mqtts://broker.local")
            .unwrap()
            .is_secure());
        assert!(ConnectorUrl::parse("https://api.example.com")
            .unwrap()
            .is_secure());
        assert!(ConnectorUrl::parse("wss://ws.example.com")
            .unwrap()
            .is_secure());

        assert!(!ConnectorUrl::parse("mqtt://broker.local")
            .unwrap()
            .is_secure());
        assert!(!ConnectorUrl::parse("http://api.example.com")
            .unwrap()
            .is_secure());
        assert!(!ConnectorUrl::parse("ws://ws.example.com")
            .unwrap()
            .is_secure());
    }

    #[test]
    fn test_display_hides_password() {
        let url = ConnectorUrl::parse("mqtt://user:secret@broker.local:1883").unwrap();
        let display = format!("{}", url);
        assert!(display.contains("user:****"));
        assert!(!display.contains("secret"));
    }

    #[test]
    fn test_parse_kafka_style() {
        let url =
            ConnectorUrl::parse("kafka://broker1.local:9092,broker2.local:9092/my-topic").unwrap();
        assert_eq!(url.scheme, "kafka");
        // Note: Our simple parser doesn't handle the second port in comma-separated hosts perfectly
        // It parses "broker1.local:9092,broker2.local" as the host and "9092" as the port
        // This is acceptable for now - production connectors can handle this in their client factories
        assert!(url.host.contains("broker1.local"));
        assert!(url.host.contains("broker2.local"));
        assert_eq!(url.path, Some("/my-topic".to_string()));
    }

    #[test]
    fn test_parse_missing_scheme() {
        let result = ConnectorUrl::parse("broker.example.com:1883");
        assert!(result.is_err());
    }

    // ========================================================================
    // TopicBuf Tests
    // ========================================================================

    #[test]
    fn topic_buf_accepts_an_exact_fit() {
        let mut storage = [0u8; 6];
        let mut out = super::TopicBuf::new(&mut storage);
        assert!(out.is_empty());
        out.push_str("ab/").unwrap();
        out.push_str("cde").unwrap();
        assert_eq!((out.as_str(), out.len(), out.capacity()), ("ab/cde", 6, 6));
        assert!(!out.overflowed());
    }

    #[test]
    fn topic_buf_refuses_an_overflowing_write_whole() {
        let mut storage = [0u8; 6];
        let mut out = super::TopicBuf::new(&mut storage);
        out.push_str("ab/").unwrap();
        // "cdé" is 4 bytes: one over, and it ends in a two-byte character.
        assert_eq!(out.push_str("cdé"), Err(super::TopicOverflow));
        assert_eq!(out.as_str(), "ab/");
        assert!(out.overflowed());
        // Latched: a write that would fit is refused too.
        assert!(out.push_str("x").is_err());
        assert_eq!(out.as_str(), "ab/");
    }

    #[test]
    fn topic_buf_overflow_propagates_through_write() {
        use core::fmt::Write as _;
        fn writer(v: u32, out: &mut super::TopicBuf<'_>) -> Result<bool, super::TopicOverflow> {
            write!(out, "t/{v}")?;
            Ok(true)
        }
        let mut storage = [0u8; 4];
        assert_eq!(
            writer(12, &mut super::TopicBuf::new(&mut storage)),
            Ok(true)
        );
        assert_eq!(
            writer(123, &mut super::TopicBuf::new(&mut storage)),
            Err(super::TopicOverflow)
        );
    }

    // ========================================================================
    // TopicResolverFn Tests
    // ========================================================================

    #[test]
    fn test_topic_resolver_returns_some() {
        let resolver: super::TopicResolverFn = Arc::new(|| Some("resolved/topic".into()));

        assert_eq!(resolver(), Some("resolved/topic".into()));
    }

    #[test]
    fn test_topic_resolver_returns_none() {
        let resolver: super::TopicResolverFn = Arc::new(|| None);

        // Returns None, should fall back to default topic
        assert_eq!(resolver(), None);
    }

    #[cfg(feature = "std")]
    #[test]
    fn test_topic_resolver_with_captured_state() {
        use std::sync::Mutex;

        let config = Arc::new(Mutex::new(Some("dynamic/topic".to_string())));
        let config_clone = config.clone();

        let resolver: super::TopicResolverFn =
            Arc::new(move || config_clone.lock().unwrap().clone());

        assert_eq!(resolver(), Some("dynamic/topic".into()));

        // Clear config
        *config.lock().unwrap() = None;
        assert_eq!(resolver(), None);
    }

    /// Dummy ingest factory for link-construction tests (never invoked).
    fn dummy_ingest_factory() -> super::IngestFactoryFn {
        Arc::new(|_db| Arc::new(|_ctx, _m, _bytes| Ok(())))
    }

    #[test]
    fn test_inbound_connector_link_resolve_topic_default() {
        use super::{InboundConnectorLink, LinkAddress};

        let url = LinkAddress::parse("mqtt://sensors/temperature").unwrap();
        let link = InboundConnectorLink::new(url, dummy_ingest_factory());

        // No resolver configured, should return static topic from URL
        assert_eq!(link.resolve_topic(), "sensors/temperature");
    }

    #[test]
    fn test_inbound_connector_link_resolve_topic_dynamic() {
        use super::{InboundConnectorLink, LinkAddress};

        let url = LinkAddress::parse("mqtt://sensors/default").unwrap();
        let mut link = InboundConnectorLink::new(url, dummy_ingest_factory());

        // Configure dynamic resolver
        link.topic_resolver = Some(Arc::new(|| Some("sensors/dynamic/kitchen".into())));

        // Should return resolved topic, not URL topic
        assert_eq!(link.resolve_topic(), "sensors/dynamic/kitchen");
    }

    #[test]
    fn test_inbound_connector_link_resolve_topic_fallback() {
        use super::{InboundConnectorLink, LinkAddress};

        let url = LinkAddress::parse("mqtt://sensors/fallback").unwrap();
        let mut link = InboundConnectorLink::new(url, dummy_ingest_factory());

        // Configure resolver that returns None
        link.topic_resolver = Some(Arc::new(|| None));

        // Should fall back to static topic from URL
        assert_eq!(link.resolve_topic(), "sensors/fallback");
    }
}
