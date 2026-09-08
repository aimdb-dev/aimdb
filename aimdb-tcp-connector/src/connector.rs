//! Runtime-neutral TCP client and server sugar.
//!
//! Both are generic over core's [`StreamDialer`] / [`StreamListener`], so the
//! socket comes from an adapter and this crate contributes only the
//! length-prefix [`LengthFramer`](crate::framing::LengthFramer), bounded by a
//! [`LengthFramers`] factory so the cap stays settable.

use alloc::boxed::Box;
use alloc::string::{String, ToString};
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::future::Future;
use core::pin::Pin;

use aimdb_core::connector::ConnectorBuilder;
use aimdb_core::remote::{AimxConfig, SecurityPolicy};
use aimdb_core::session::aimx::{AimxCodec, AimxDispatch};
use aimdb_core::session::{
    Dispatch, FramingDialer, FramingListener, OneShot, SessionClientConnector, SessionConfig,
    SessionLimits, SessionServerConnector, StreamDialer, StreamListener,
};
use aimdb_core::{AimDb, DbError, DbResult};

use crate::framing::{LengthFramers, DEFAULT_MAX_FRAME};
use crate::DEFAULT_SCHEME;

type BoxFuture = Pin<Box<dyn Future<Output = ()> + Send + 'static>>;
type BuildFuture<'a> = Pin<Box<dyn Future<Output = DbResult<Vec<BoxFuture>>> + Send + 'a>>;

/// Per-`read` chunk handed to the byte stream.
pub const READ_CHUNK: usize = 1024;
/// Per-`write_all` chunk.
pub const WRITE_CHUNK: usize = 1024;

/// The dialer half, framed.
pub type TcpFramingDialer<D> = FramingDialer<D, LengthFramers, READ_CHUNK, WRITE_CHUNK>;
/// The listener half, framed.
pub type TcpFramingListener<L> = FramingListener<L, LengthFramers, READ_CHUNK, WRITE_CHUNK>;

/// Port used when an endpoint names only a host.
pub const DEFAULT_PORT: u16 = 7001;

/// Why an endpoint is not a `host:port`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndpointError {
    /// The endpoint names no host.
    EmptyHost,
    /// A bracketed IPv6 literal with no closing `]`.
    UnclosedBracket,
    /// A port was written, and is not one.
    BadPort,
}

impl core::fmt::Display for EndpointError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(match self {
            Self::EmptyHost => "endpoint names no host",
            Self::UnclosedBracket => "missing closing bracket for IPv6 host",
            Self::BadPort => "port is not a number in 0..=65535",
        })
    }
}

/// Split a `host:port` endpoint, defaulting the port when the endpoint names
/// only a host.
///
/// A bracketed IPv6 literal carries colons of its own, so only a colon *after*
/// the closing bracket separates the port; the brackets are stripped, because
/// that is the form both adapters resolve. An **unbracketed** IPv6 literal
/// cannot carry a port at all — brackets are what add one — so every colon in
/// it belongs to the address.
///
/// A port that is written but is not one is an error, not a fallback to
/// `default_port`: silently dialing a different service is worse than not
/// dialing.
pub fn split_host_port(endpoint: &str, default_port: u16) -> Result<(String, u16), EndpointError> {
    if let Some(rest) = endpoint.strip_prefix('[') {
        let Some((host, tail)) = rest.split_once(']') else {
            return Err(EndpointError::UnclosedBracket);
        };
        if host.is_empty() {
            return Err(EndpointError::EmptyHost);
        }
        let port = if tail.is_empty() {
            default_port
        } else {
            // Anything but `:port` after `]` is junk, including junk that has a
            // port behind it — `[::1]oops:7003` names no reachable service.
            tail.strip_prefix(':')
                .ok_or(EndpointError::BadPort)?
                .parse()
                .map_err(|_| EndpointError::BadPort)?
        };
        return Ok((host.to_string(), port));
    }
    match endpoint.rsplit_once(':') {
        // Another colon before the last one: an unbracketed IPv6 literal, whose
        // trailing group is part of the address rather than a port.
        Some((head, _)) if head.contains(':') => Ok((endpoint.to_string(), default_port)),
        Some(("", _)) => Err(EndpointError::EmptyHost),
        Some((host, port)) => Ok((
            host.to_string(),
            port.parse().map_err(|_| EndpointError::BadPort)?,
        )),
        None if endpoint.is_empty() => Err(EndpointError::EmptyHost),
        None => Ok((endpoint.to_string(), default_port)),
    }
}

/// Frame an adapter's dialer for a `host:port` endpoint.
///
/// The split-then-dial sugar every caller wants; use [`framed_dialer`] directly
/// when host and port are already separate.
pub fn framed_dialer_at<D: StreamDialer>(
    dialer: D,
    endpoint: &str,
) -> Result<TcpFramingDialer<D>, EndpointError> {
    let (host, port) = split_host_port(endpoint, DEFAULT_PORT)?;
    Ok(framed_dialer(dialer, host, port))
}

/// Frame an adapter's dialer for `host:port` with length-prefix framing,
/// bounded by [`DEFAULT_MAX_FRAME`].
pub fn framed_dialer<D: StreamDialer>(
    dialer: D,
    host: impl Into<String>,
    port: u16,
) -> TcpFramingDialer<D> {
    framed_dialer_bounded(dialer, host, port, DEFAULT_MAX_FRAME)
}

/// As [`framed_dialer`], capping an inbound frame at `max_frame` payload bytes.
pub fn framed_dialer_bounded<D: StreamDialer>(
    dialer: D,
    host: impl Into<String>,
    port: u16,
    max_frame: usize,
) -> TcpFramingDialer<D> {
    FramingDialer::new(dialer, LengthFramers::new(max_frame), host, port)
}

/// Frame an adapter's listener with length-prefix framing, bounded by
/// [`DEFAULT_MAX_FRAME`].
pub fn framed_listener<L: StreamListener>(listener: L) -> TcpFramingListener<L> {
    framed_listener_bounded(listener, DEFAULT_MAX_FRAME)
}

/// As [`framed_listener`], capping an inbound frame at `max_frame` payload bytes.
pub fn framed_listener_bounded<L: StreamListener>(
    listener: L,
    max_frame: usize,
) -> TcpFramingListener<L> {
    FramingListener::new(listener, LengthFramers::new(max_frame))
}

/// Constructs a TCP session client connector over an adapter's dialer.
pub struct TcpClient;

impl TcpClient {
    /// Mirror records to and from an AimX peer at `host:port`.
    ///
    /// `dialer` comes from an adapter (`TokioNet::tcp()`, `EmbassyNet::tcp(..)`),
    /// which also resolves the host.
    #[allow(clippy::new_ret_no_self)]
    pub fn new<D: StreamDialer>(
        dialer: D,
        host: impl Into<String>,
        port: u16,
    ) -> SessionClientConnector<TcpFramingDialer<D>, AimxCodec> {
        Self::bounded(dialer, host, port, DEFAULT_MAX_FRAME)
    }

    /// As [`new`](Self::new), capping an inbound frame at `max_frame` payload
    /// bytes rather than [`DEFAULT_MAX_FRAME`].
    pub fn bounded<D: StreamDialer>(
        dialer: D,
        host: impl Into<String>,
        port: u16,
        max_frame: usize,
    ) -> SessionClientConnector<TcpFramingDialer<D>, AimxCodec> {
        SessionClientConnector::new(
            framed_dialer_bounded(dialer, host, port, max_frame),
            AimxCodec,
        )
        .scheme(DEFAULT_SCHEME)
    }
}

/// Accepts AimX connections over an adapter's TCP listener.
///
/// The listener is moved in, so it is taken once at `build`; a second `build`
/// fails rather than silently serving nothing.
pub struct TcpServer<L> {
    listener: OneShot<L>,
    config: AimxConfig,
    scheme: String,
    max_frame: usize,
}

impl<L> TcpServer<L> {
    /// Serve AimX on an already-bound listener.
    ///
    /// Prefer loopback bind addresses unless the deployment provides its own
    /// network-layer protection.
    pub fn new(listener: L) -> Self {
        Self {
            listener: OneShot::new(listener),
            config: AimxConfig::uds_default(),
            scheme: DEFAULT_SCHEME.to_string(),
            max_frame: DEFAULT_MAX_FRAME,
        }
    }

    /// Cap an inbound frame at `max_frame` payload bytes.
    ///
    /// The default is [`DEFAULT_MAX_FRAME`]. Lower it on a memory-constrained
    /// target, or on a port reachable by peers you do not control: it bounds
    /// what one connection can make the receiver buffer.
    pub fn max_frame(mut self, max_frame: usize) -> Self {
        self.max_frame = max_frame;
        self
    }

    /// Use a prepared [`AimxConfig`] for limits and security policy.
    pub fn with_config(mut self, config: AimxConfig) -> Self {
        self.config = config;
        self
    }

    /// Set the security policy.
    pub fn security_policy(mut self, policy: SecurityPolicy) -> Self {
        self.config = self.config.security_policy(policy);
        self
    }

    /// Maximum concurrently served connections.
    pub fn max_connections(mut self, max: usize) -> Self {
        self.config = self.config.max_connections(max);
        self
    }

    /// Maximum live subscriptions per connection.
    pub fn max_subs_per_connection(mut self, max: usize) -> Self {
        self.config = self.config.max_subs_per_connection(max);
        self
    }

    /// Override the scheme this connector registers.
    pub fn scheme(mut self, scheme: impl Into<String>) -> Self {
        self.scheme = scheme.into();
        self
    }
}

impl<L> ConnectorBuilder for TcpServer<L>
where
    L: StreamListener + Send + 'static,
    L::Stream: 'static,
{
    fn build<'a>(&'a self, db: &'a AimDb) -> BuildFuture<'a> {
        let config = self.config.clone();
        let scheme = self.scheme.clone();
        Box::pin(async move {
            // Taken on first poll, not at call time: a `build()` future dropped
            // before it is polled must leave the listener where it was, or a
            // later build fails having never served anything.
            let listener = self
                .listener
                .take()
                .ok_or_else(|| DbError::InvalidOperation {
                    operation: "TcpServer::build".to_string(),
                    reason: "the moved-in listener was already taken; build() ran twice"
                        .to_string(),
                })?;
            let session_config = SessionConfig {
                limits: SessionLimits {
                    max_connections: config.max_connections,
                    max_subs_per_connection: config.max_subs_per_connection,
                },
                reads_hello: false,
                acks_subscribe: false,
            };
            let framed = OneShot::new(framed_listener_bounded(listener, self.max_frame));
            let dispatch_config = config;
            let connector = SessionServerConnector::new(
                move || {
                    framed.take().ok_or_else(|| DbError::InvalidOperation {
                        operation: "TcpServer::build".to_string(),
                        reason: "the moved-in listener was already taken".to_string(),
                    })
                },
                AimxCodec,
                move |db: &AimDb| -> Arc<dyn Dispatch> {
                    crate::apply_writable(db, &dispatch_config);
                    Arc::new(AimxDispatch::new(
                        Arc::new(db.clone()),
                        dispatch_config.clone(),
                    ))
                },
                session_config,
            )
            .scheme(scheme);
            connector.build(db).await
        })
    }

    fn scheme(&self) -> &str {
        &self.scheme
    }
}

#[cfg(test)]
mod tests {
    use super::{split_host_port, EndpointError, DEFAULT_PORT};

    /// `Ok` shorthand: the split every passing case expects.
    fn split(endpoint: &str) -> Result<(String, u16), EndpointError> {
        split_host_port(endpoint, DEFAULT_PORT)
    }

    #[test]
    fn splits_host_and_port() {
        assert_eq!(split("127.0.0.1:7002"), Ok(("127.0.0.1".into(), 7002)));
    }

    #[test]
    fn a_bare_host_takes_the_default_port() {
        assert_eq!(
            split("example.test"),
            Ok(("example.test".into(), DEFAULT_PORT))
        );
    }

    /// A written port that is not one names no service, so it is an error
    /// rather than a silent fallback to `DEFAULT_PORT` — that would dial a
    /// different, possibly live, server.
    #[test]
    fn an_unparsable_port_is_rejected() {
        assert_eq!(split("host:not-a-port"), Err(EndpointError::BadPort));
        assert_eq!(split("10.0.0.5:8080x"), Err(EndpointError::BadPort));
        assert_eq!(split("host:99999"), Err(EndpointError::BadPort));
        assert_eq!(split("host:"), Err(EndpointError::BadPort));
    }

    /// A bracketed IPv6 literal is full of colons; only the one after `]`
    /// separates the port, and the brackets are not part of the address.
    #[test]
    fn brackets_are_stripped_from_an_ipv6_literal() {
        assert_eq!(split("[::1]:7003"), Ok(("::1".into(), 7003)));
    }

    #[test]
    fn a_bracketed_ipv6_host_without_a_port_is_not_mangled() {
        assert_eq!(split("[::1]"), Ok(("::1".into(), DEFAULT_PORT)));
    }

    /// Brackets are what let an IPv6 literal carry a port, so without them
    /// every colon belongs to the address and the port is the default.
    #[test]
    fn an_unbracketed_ipv6_literal_keeps_all_its_colons() {
        assert_eq!(split("::1"), Ok(("::1".into(), DEFAULT_PORT)));
        assert_eq!(split("fe80::1"), Ok(("fe80::1".into(), DEFAULT_PORT)));
        assert_eq!(
            split("2001:db8::dead:beef"),
            Ok(("2001:db8::dead:beef".into(), DEFAULT_PORT)),
            "the trailing group is address, not a port"
        );
    }

    #[test]
    fn a_malformed_endpoint_is_rejected() {
        assert_eq!(split(":99999"), Err(EndpointError::EmptyHost));
        assert_eq!(split(""), Err(EndpointError::EmptyHost));
        assert_eq!(split("[]:7001"), Err(EndpointError::EmptyHost));
        assert_eq!(split("[::1"), Err(EndpointError::UnclosedBracket));
        assert_eq!(
            split("[::1]oops:7003"),
            Err(EndpointError::BadPort),
            "an explicit port behind junk is not silently honoured"
        );
    }
}
