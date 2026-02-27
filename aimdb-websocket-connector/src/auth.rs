//! Authentication and authorization for WebSocket connections.
//!
//! The [`AuthHandler`] trait provides pluggable auth hooks for:
//!
//! 1. **Connection upgrade** — `authenticate()`: decide whether to accept the WebSocket
//!    handshake and assign per-client permissions.
//! 2. **Topic subscriptions** — `authorize_subscribe()`: gate which topics a client can
//!    receive data from.
//! 3. **Inbound writes** — `authorize_write()`: gate which topics a client may write to.
//!
//! The default implementation ([`NoAuth`]) allows all operations.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;

use async_trait::async_trait;
use axum::http::HeaderMap;

// ════════════════════════════════════════════════════════════════════
// Public types
// ════════════════════════════════════════════════════════════════════

/// Opaque identifier for a connected WebSocket client.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ClientId(pub(crate) u64);

impl std::fmt::Display for ClientId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "client-{}", self.0)
    }
}

/// Information about a connected client, passed to authorization hooks.
#[derive(Debug, Clone)]
pub struct ClientInfo {
    pub id: ClientId,
    pub remote_addr: SocketAddr,
    pub permissions: Permissions,
}

/// Per-client permission set assigned during authentication.
///
/// Each field is a list of topic *patterns* (supporting `*` and `#` wildcards
/// as defined in [`crate::protocol`]).
///
/// An empty `Vec` means *"no access"*. Use `["#"]` for unrestricted access.
#[derive(Debug, Clone, Default)]
pub struct Permissions {
    /// Topic patterns the client may subscribe to.
    pub subscribe_patterns: Vec<String>,
    /// Topic patterns the client may write to.
    pub write_patterns: Vec<String>,
}

impl Permissions {
    /// Creates a permission set that grants full access to everything.
    pub fn allow_all() -> Self {
        Self {
            subscribe_patterns: vec!["#".to_string()],
            write_patterns: vec!["#".to_string()],
        }
    }

    /// Returns `true` if the client is allowed to subscribe to `topic`.
    pub fn can_subscribe(&self, topic: &str) -> bool {
        self.subscribe_patterns
            .iter()
            .any(|p| crate::protocol::topic_matches(p, topic))
    }

    /// Returns `true` if the client is allowed to write to `topic`.
    pub fn can_write(&self, topic: &str) -> bool {
        self.write_patterns
            .iter()
            .any(|p| crate::protocol::topic_matches(p, topic))
    }
}

/// Context provided to [`AuthHandler::authenticate`] during WebSocket upgrade.
#[derive(Debug)]
pub struct AuthRequest {
    pub headers: HeaderMap,
    pub query_params: HashMap<String, String>,
    pub remote_addr: SocketAddr,
}

/// Error returned when authentication fails.
///
/// The message is forwarded to the client as an HTTP 401 response body.
#[derive(Debug, Clone)]
pub struct AuthError {
    pub message: String,
}

impl AuthError {
    pub fn new(msg: impl Into<String>) -> Self {
        Self {
            message: msg.into(),
        }
    }
}

// ════════════════════════════════════════════════════════════════════
// AuthHandler trait
// ════════════════════════════════════════════════════════════════════

/// Pluggable authentication and authorization hook.
///
/// # Example — Bearer token auth
///
/// ```rust,ignore
/// use aimdb_websocket_connector::auth::{AuthHandler, AuthRequest, AuthError, Permissions};
///
/// struct BearerAuth { valid_token: String }
///
/// #[async_trait::async_trait]
/// impl AuthHandler for BearerAuth {
///     async fn authenticate(&self, req: &AuthRequest) -> Result<Permissions, AuthError> {
///         let token = req.headers
///             .get("Authorization")
///             .and_then(|v| v.to_str().ok())
///             .and_then(|v| v.strip_prefix("Bearer "))
///             .ok_or_else(|| AuthError::new("missing token"))?;
///
///         if token == self.valid_token {
///             Ok(Permissions::allow_all())
///         } else {
///             Err(AuthError::new("invalid token"))
///         }
///     }
/// }
/// ```
#[async_trait]
pub trait AuthHandler: Send + Sync + 'static {
    /// Called during WebSocket upgrade to authenticate the client.
    ///
    /// Return [`Ok(Permissions)`] to accept the connection with the assigned
    /// permissions, or [`Err(AuthError)`] to reject it (HTTP 401).
    async fn authenticate(&self, request: &AuthRequest) -> Result<Permissions, AuthError>;

    /// Called before allowing a topic subscription.
    ///
    /// The default implementation delegates to [`Permissions::can_subscribe`].
    async fn authorize_subscribe(&self, client: &ClientInfo, topic: &str) -> bool {
        client.permissions.can_subscribe(topic)
    }

    /// Called before routing an inbound write to a producer.
    ///
    /// The default implementation delegates to [`Permissions::can_write`].
    async fn authorize_write(&self, client: &ClientInfo, topic: &str) -> bool {
        client.permissions.can_write(topic)
    }
}

// ════════════════════════════════════════════════════════════════════
// NoAuth — allow-all default
// ════════════════════════════════════════════════════════════════════

/// Default `AuthHandler` that allows all connections and operations.
pub struct NoAuth;

#[async_trait]
impl AuthHandler for NoAuth {
    async fn authenticate(&self, _request: &AuthRequest) -> Result<Permissions, AuthError> {
        Ok(Permissions::allow_all())
    }
}

/// Type-erased auth handler stored inside the connector.
pub(crate) type DynAuthHandler = Arc<dyn AuthHandler>;
