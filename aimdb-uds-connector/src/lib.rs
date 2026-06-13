//! Unix-domain-socket transport connector for AimDB — record mirroring and
//! remote access over a local socket.
//!
//! A thin, swappable transport crate: it contributes only the
//! `Dialer`/`Listener`/`Connection` triple ([`UdsConnection`] /
//! [`UdsDialer`] / [`UdsListener`]); the AimX codec + dispatch and the engine
//! wiring are reused from `aimdb-core`. Two ergonomic constructors wrap the
//! generic core connectors:
//!
//! - [`UdsClient`] — dials a peer over UDS and mirrors records under a scheme
//!   (`"uds"` by default), using `link_to`/`link_from` like any data-plane
//!   connector. Sugar over [`SessionClientConnector`]`<UdsDialer, AimxCodec>`.
//! - [`UdsServer`] — accepts connections and serves the AimX toolset over UDS;
//!   register it with `with_connector` to stand up remote access. Sugar over
//!   [`SessionServerConnector`].
//!
//! ```no_run
//! use aimdb_core::buffer::BufferCfg;
//! use aimdb_core::AimDbBuilder;
//! use aimdb_uds_connector::{UdsClient, UdsServer};
//! # use aimdb_tokio_adapter::{TokioAdapter, TokioRecordRegistrarExt};
//! # use std::sync::Arc;
//! # #[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
//! # struct Temp { celsius: f32 }
//! # async fn demo() -> Result<(), Box<dyn std::error::Error>> {
//! # let rt = Arc::new(TokioAdapter::new()?);
//!
//! // server: expose this db over a socket (no links)
//! AimDbBuilder::new().runtime(rt.clone())
//!     .with_connector(UdsServer::new("/run/aimdb.sock").max_connections(32))
//!     .build().await?;
//!
//! // client: mirror a record to a peer over the socket
//! let mut b = AimDbBuilder::new().runtime(rt)
//!     .with_connector(UdsClient::new("/run/aimdb.sock"));
//! b.configure::<Temp>("temp", |r| {
//!     r.buffer(BufferCfg::SingleLatest)
//!         .with_remote_access()
//!         .link_to("uds://temp")
//!         .with_serializer_raw(|t: &Temp| Ok(serde_json::to_vec(t).expect("serialize")))
//!         .finish();
//! });
//! b.build().await?;
//! # Ok(())
//! # }
//! ```

mod transport;

pub use transport::{UdsConnection, UdsDialer, UdsListener};

use std::future::Future;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;

use aimdb_core::connector::ConnectorBuilder;
use aimdb_core::remote::AimxConfig;
use aimdb_core::session::aimx::{AimxCodec, AimxDispatch};
use aimdb_core::session::{
    Dispatch, SessionClientConnector, SessionConfig, SessionLimits, SessionServerConnector,
};
use aimdb_core::{AimDb, DbError, DbResult};

type BoxFuture = Pin<Box<dyn Future<Output = ()> + Send + 'static>>;
type BuildFuture<'a> = Pin<Box<dyn Future<Output = DbResult<Vec<BoxFuture>>> + Send + 'a>>;

/// The default scheme `UdsClient`/`UdsServer` register when none is given.
///
/// Transport-matched (like MQTT's `"mqtt"`), so `link_to("uds://<record>")` reads
/// at the call site. Override with `.scheme(...)` when running more than one
/// remote connector.
pub const DEFAULT_SCHEME: &str = "uds";

// ===========================================================================
// Client sugar
// ===========================================================================

/// Constructs a [`SessionClientConnector`] that dials an AimX peer over a
/// Unix-domain socket. `UdsClient::new(path)` is sugar; chain `.scheme(...)` /
/// `.with_config(...)` on the returned connector.
pub struct UdsClient;

impl UdsClient {
    /// Mirror records to/from the AimX peer listening at `socket_path` (scheme
    /// defaults to [`DEFAULT_SCHEME`]).
    // Sugar constructor: intentionally returns the generic connector, not `Self`.
    #[allow(clippy::new_ret_no_self)]
    pub fn new(socket_path: impl Into<PathBuf>) -> SessionClientConnector<UdsDialer, AimxCodec> {
        SessionClientConnector::new(UdsDialer::new(socket_path), AimxCodec).scheme(DEFAULT_SCHEME)
    }
}

// ===========================================================================
// Server sugar
// ===========================================================================

/// Accepts AimX connections over a Unix-domain socket and serves the full AimX
/// toolset. Register it via `with_connector` to stand up remote access:
///
/// ```no_run
/// # use aimdb_uds_connector::UdsServer;
/// # fn demo(builder: aimdb_core::AimDbBuilder) {
/// builder.with_connector(UdsServer::new("/run/aimdb.sock").max_connections(32));
/// # }
/// ```
///
/// Unlike a data-plane connector, a server takes **no** `link_to`/`link_from` —
/// it answers introspection/subscribe/write for whatever records exist.
pub struct UdsServer {
    config: AimxConfig,
    scheme: String,
}

impl UdsServer {
    /// Serve AimX over the socket at `socket_path`, with default limits/policy.
    pub fn new(socket_path: impl Into<String>) -> Self {
        Self {
            config: AimxConfig::uds_default().socket_path(socket_path),
            scheme: DEFAULT_SCHEME.to_string(),
        }
    }

    /// Build from a full [`AimxConfig`] (the one-line migration from the former
    /// `AimDbBuilder::with_remote_access`).
    pub fn from_config(config: AimxConfig) -> Self {
        Self {
            config,
            scheme: DEFAULT_SCHEME.to_string(),
        }
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

    /// Socket file permissions (octal mode, e.g. `0o600`).
    pub fn socket_permissions(mut self, mode: u32) -> Self {
        self.config = self.config.socket_permissions(mode);
        self
    }

    /// Override the scheme this connector registers.
    pub fn scheme(mut self, scheme: impl Into<String>) -> Self {
        self.scheme = scheme.into();
        self
    }
}

impl ConnectorBuilder for UdsServer {
    fn build<'a>(&'a self, db: &'a AimDb) -> BuildFuture<'a> {
        let config = self.config.clone();
        let scheme = self.scheme.clone();
        Box::pin(async move {
            let session_config = SessionConfig {
                limits: SessionLimits {
                    max_connections: config.max_connections,
                    max_subs_per_connection: config.max_subs_per_connection,
                },
                reads_hello: false,
                // AimX's subscribe ack stays implicit (events flow); no ack frame.
                acks_subscribe: false,
            };
            let bind_config = config.clone();
            let dispatch_config = config;
            // Reuse the generic spine: bind (errors surface synchronously) + AimX
            // dispatch over the AimX codec.
            let connector = SessionServerConnector::new(
                move || bind_uds_listener(&bind_config),
                AimxCodec,
                move |db: &AimDb| -> Arc<dyn Dispatch> {
                    // Apply the security policy's writable marking so `record.list`
                    // reports the `writable` flag (the dispatch also enforces it).
                    apply_writable(db, &dispatch_config);
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

// ===========================================================================
// Shared bind / writable helpers
// ===========================================================================

/// Bind the Unix-domain socket synchronously (remove a stale socket file,
/// `bind`, `set_permissions`) so bind errors surface from `build`.
fn bind_uds_listener(config: &AimxConfig) -> DbResult<UdsListener> {
    let socket_path = &config.socket_path;

    #[cfg(feature = "tracing")]
    tracing::info!("Initializing AimX UDS server on socket: {}", socket_path);

    if std::path::Path::new(socket_path).exists() {
        std::fs::remove_file(socket_path).map_err(|e| DbError::IoWithContext {
            context: format!("Failed to remove existing socket file {}", socket_path),
            source: e,
        })?;
    }

    let listener =
        tokio::net::UnixListener::bind(socket_path).map_err(|e| DbError::IoWithContext {
            context: format!("Failed to bind Unix socket at {}", socket_path),
            source: e,
        })?;

    let permissions = config.socket_permissions.unwrap_or(0o600);
    let mut perms = std::fs::metadata(socket_path)
        .map_err(|e| DbError::IoWithContext {
            context: format!("Failed to read socket metadata for {}", socket_path),
            source: e,
        })?
        .permissions();
    perms.set_mode(permissions);
    std::fs::set_permissions(socket_path, perms).map_err(|e| DbError::IoWithContext {
        context: format!("Failed to set socket permissions for {}", socket_path),
        source: e,
    })?;

    #[cfg(feature = "tracing")]
    tracing::info!(
        "AimX socket bound at {} (mode {:o})",
        socket_path,
        permissions
    );

    Ok(UdsListener::new(listener))
}

/// Mark each record named in the policy's writable set as writable, so
/// `record.list` advertises the `writable` flag.
fn apply_writable(db: &AimDb, config: &AimxConfig) {
    for key in config.security_policy.writable_records() {
        if let Some(id) = db.inner().resolve_str(&key) {
            if let Some(storage) = db.inner().storage(id) {
                storage.set_writable_erased(true);
            }
        }
    }
}
