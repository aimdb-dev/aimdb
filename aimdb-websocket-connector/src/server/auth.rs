//! Authentication and authorization for WebSocket connections.
//!
//! The [`AuthHandler`] trait provides pluggable auth hooks for:
//!
//! 1. **Connection upgrade** — `authenticate()`: decide whether to accept the WebSocket
//!    handshake and assign per-client permissions.
//! 2. **Topic subscriptions** — `authorize_subscribe()`: gate which topics a client can
//!    receive data from.
//! 3. **Inbound writes** — `authorize_write()`: gate which topics a client may write to.
//! 4. **Historical reads** — `authorize_query()`: gate the `record.query` pattern.
//! 5. **Introspection** — `authorize_list()`: gate which `record.list` rows a client sees.
//!
//! (4) and (5) default to (2), so overriding `authorize_subscribe` governs all
//! three read paths.
//!
//! The default implementation ([`NoAuth`]) allows all operations.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;

use core::future::Future;
use core::pin::Pin;

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
/// as defined by [`aimdb_core::topic_matches`]).
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
    ///
    /// `topic` is the client's requested subscription, which may itself be a
    /// wildcard — so this asks **pattern containment**
    /// ([`pattern_contains`](aimdb_core::pattern_contains)):
    /// does a granted pattern cover the *whole* requested pattern? Plain
    /// [`topic_matches`](aimdb_core::topic_matches) would let a one-level grant
    /// (`sensors.*`) admit an all-levels request (`sensors.#`) by having the
    /// `*` swallow the `#`, silently widening the grant. For a concrete request
    /// `pattern_contains` collapses to `topic_matches`, so exact subscribes are
    /// unaffected.
    pub fn can_subscribe(&self, topic: &str) -> bool {
        self.subscribe_patterns
            .iter()
            .any(|p| aimdb_core::pattern_contains(p, topic))
    }

    /// Returns `true` if the client is allowed to write to `topic`.
    ///
    /// Writes target a single concrete record, so `topic` is never a wildcard
    /// here and plain [`topic_matches`](aimdb_core::topic_matches) is the right
    /// check (a wildcard write topic would resolve to no record downstream).
    pub fn can_write(&self, topic: &str) -> bool {
        self.write_patterns
            .iter()
            .any(|p| aimdb_core::topic_matches(p, topic))
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
/// ```no_run
/// use aimdb_websocket_connector::{AuthHandler, AuthRequest, AuthError, Permissions};
/// # use core::future::Future;
/// # use core::pin::Pin;
///
/// struct BearerAuth { valid_token: String }
///
/// impl AuthHandler for BearerAuth {
///     fn authenticate<'a>(
///         &'a self,
///         req: &'a AuthRequest,
///     ) -> Pin<Box<dyn Future<Output = Result<Permissions, AuthError>> + Send + 'a>> {
///         Box::pin(async move {
///             let token = req.headers
///                 .get("Authorization")
///                 .and_then(|v| v.to_str().ok())
///                 .and_then(|v| v.strip_prefix("Bearer "))
///                 .ok_or_else(|| AuthError::new("missing token"))?;
///
///             if token == self.valid_token {
///                 Ok(Permissions::allow_all())
///             } else {
///                 Err(AuthError::new("invalid token"))
///             }
///         })
///     }
/// }
/// ```
pub trait AuthHandler: Send + Sync + 'static {
    /// Called during WebSocket upgrade to authenticate the client.
    ///
    /// Return `Ok(Permissions)` to accept the connection with the assigned
    /// permissions, or `Err(AuthError)` to reject it (HTTP 401).
    fn authenticate<'a>(
        &'a self,
        request: &'a AuthRequest,
    ) -> Pin<Box<dyn Future<Output = Result<Permissions, AuthError>> + Send + 'a>>;

    /// Called before allowing a topic subscription.
    ///
    /// The default implementation delegates to [`Permissions::can_subscribe`].
    fn authorize_subscribe<'a>(
        &'a self,
        client: &'a ClientInfo,
        topic: &'a str,
    ) -> Pin<Box<dyn Future<Output = bool> + Send + 'a>> {
        Box::pin(async move { client.permissions.can_subscribe(topic) })
    }

    /// Called before routing an inbound write to a producer.
    ///
    /// The default implementation delegates to [`Permissions::can_write`].
    fn authorize_write<'a>(
        &'a self,
        client: &'a ClientInfo,
        topic: &'a str,
    ) -> Pin<Box<dyn Future<Output = bool> + Send + 'a>> {
        Box::pin(async move { client.permissions.can_write(topic) })
    }

    /// Called before serving a `record.query` (historical read).
    ///
    /// `pattern` is the query's `name`, possibly wildcarded — so this is the
    /// containment check [`authorize_subscribe`](Self::authorize_subscribe)
    /// performs, which it delegates to by default. A query omitting `name` asks
    /// for `"*"`, which a narrower grant does not contain: it fails closed.
    fn authorize_query<'a>(
        &'a self,
        client: &'a ClientInfo,
        pattern: &'a str,
    ) -> Pin<Box<dyn Future<Output = bool> + Send + 'a>> {
        self.authorize_subscribe(client, pattern)
    }

    /// Called for each `record.list` row; `false` drops the row and the call
    /// still succeeds. Defaults to
    /// [`authorize_subscribe`](Self::authorize_subscribe).
    ///
    /// `record_key` is the row's database key, *not* its WebSocket topic. The
    /// two coincide under `link_to("ws://<key>")`, but a `TopicProvider` that
    /// computes the topic per value has no single topic to check against —
    /// grant the record key itself (`["sensors.#", "inject"]`) to keep such a
    /// record introspectable.
    fn authorize_list<'a>(
        &'a self,
        client: &'a ClientInfo,
        record_key: &'a str,
    ) -> Pin<Box<dyn Future<Output = bool> + Send + 'a>> {
        self.authorize_subscribe(client, record_key)
    }
}

// ════════════════════════════════════════════════════════════════════
// NoAuth — allow-all default
// ════════════════════════════════════════════════════════════════════

/// Default `AuthHandler` that allows all connections and operations.
pub struct NoAuth;

impl AuthHandler for NoAuth {
    fn authenticate<'a>(
        &'a self,
        _request: &'a AuthRequest,
    ) -> Pin<Box<dyn Future<Output = Result<Permissions, AuthError>> + Send + 'a>> {
        Box::pin(async move { Ok(Permissions::allow_all()) })
    }
}

/// Type-erased auth handler stored inside the connector.
pub(crate) type DynAuthHandler = Arc<dyn AuthHandler>;

#[cfg(test)]
mod tests {
    use super::Permissions;

    fn perms(subscribe: &[&str]) -> Permissions {
        Permissions {
            subscribe_patterns: subscribe.iter().map(|s| s.to_string()).collect(),
            write_patterns: Vec::new(),
        }
    }

    #[test]
    fn subscribe_grant_is_not_widened_by_a_wildcard_request() {
        // A one-level grant must not admit an all-levels request (the `*`-eats-`#`
        // escalation): `sensors.*` covers one level, `sensors.#` covers all.
        let p = perms(&["sensors.*"]);
        assert!(p.can_subscribe("sensors.temp")); // in scope
        assert!(!p.can_subscribe("sensors.#")); // escalation — denied
        assert!(!p.can_subscribe("sensors.temp.vienna")); // deeper — denied
    }

    #[test]
    fn subscribe_grant_with_an_interior_hash_keeps_its_suffix() {
        // A grant scoped to `secret` leaves at any depth must not admit the rest
        // of the subtree: the `#` used to short-circuit the whole match, so every
        // segment after it was ignored and this grant behaved like `tenant.#`.
        let p = perms(&["tenant.#.secret"]);
        assert!(p.can_subscribe("tenant.secret"));
        assert!(p.can_subscribe("tenant.a.b.secret"));
        assert!(!p.can_subscribe("tenant.public"));
        assert!(!p.can_subscribe("tenant.a.b.public"));
        assert!(!p.can_subscribe("tenant.#")); // no escalation to the subtree
    }

    #[test]
    fn subscribe_allows_requests_the_grant_actually_covers() {
        assert!(perms(&["#"]).can_subscribe("sensors.#"));
        let p = perms(&["sensors.#"]);
        assert!(p.can_subscribe("sensors.#"));
        assert!(p.can_subscribe("sensors.temp.#"));
        assert!(p.can_subscribe("sensors.temp"));
        // Out of the granted subtree stays denied.
        assert!(!p.can_subscribe("commands.#"));
    }
}
