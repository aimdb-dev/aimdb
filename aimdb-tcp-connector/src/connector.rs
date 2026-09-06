//! Runtime-neutral TCP client and server sugar.
//!
//! Both are generic over core's [`StreamDialer`] / [`StreamListener`], so the
//! socket comes from an adapter and this crate contributes only the
//! length-prefix [`LengthFramer`].

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

use crate::framing::LengthFramer;
use crate::DEFAULT_SCHEME;

type BoxFuture = Pin<Box<dyn Future<Output = ()> + Send + 'static>>;
type BuildFuture<'a> = Pin<Box<dyn Future<Output = DbResult<Vec<BoxFuture>>> + Send + 'a>>;

/// Per-`read` chunk handed to the byte stream.
pub const READ_CHUNK: usize = 1024;
/// Per-`write_all` chunk.
pub const WRITE_CHUNK: usize = 1024;

/// The dialer half, framed.
pub type TcpFramingDialer<D> = FramingDialer<D, fn() -> LengthFramer, READ_CHUNK, WRITE_CHUNK>;
/// The listener half, framed.
pub type TcpFramingListener<L> = FramingListener<L, fn() -> LengthFramer, READ_CHUNK, WRITE_CHUNK>;

/// Frame an adapter's dialer for `host:port` with length-prefix framing.
pub fn framed_dialer<D: StreamDialer>(
    dialer: D,
    host: impl Into<String>,
    port: u16,
) -> TcpFramingDialer<D> {
    FramingDialer::new(
        dialer,
        LengthFramer::new as fn() -> LengthFramer,
        host,
        port,
    )
}

/// Frame an adapter's listener with length-prefix framing.
pub fn framed_listener<L: StreamListener>(listener: L) -> TcpFramingListener<L> {
    FramingListener::new(listener, LengthFramer::new as fn() -> LengthFramer)
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
        SessionClientConnector::new(
            FramingDialer::new(
                dialer,
                LengthFramer::new as fn() -> LengthFramer,
                host,
                port,
            ),
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
        }
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
            let framed = OneShot::new(framed_listener(listener));
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
