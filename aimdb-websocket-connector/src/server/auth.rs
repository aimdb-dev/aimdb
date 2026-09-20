//! Authentication and authorization for WebSocket connections.
//!
//! The [`AuthHandler`] trait provides pluggable auth hooks for:
//!
//! 1. **Connection upgrade** — `authenticate()`: resolve per-client permissions into bitmasks
//!    which checked for before message broadcasting.
//!    `authenticate()` does not gate topic subscription, so clients could claim unregistered topics,
//!    and receive nothing during their lifetime.
//! 2. **Inbound writes** — `authorize_write()`: gate which records a client may write to.
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
    /// Bitsets represent accessibility of record at index `i`
    /// Records maintain the same order as in `AimdDb.inner`
    pub record_perms: Arc<RecordsBits>,
}

/// Per-client permission set assigned during authentication.
///
/// Each field is a list of record key *patterns* (supporting `*` and `#` wildcards
/// as defined by [`aimdb_core::topic_matches`]).
///
/// An empty `Vec` means *"no access"*. Use `["#"]` for unrestricted access.
#[derive(Debug, Clone, Default)]
pub struct Permissions {
    /// Record name patterns the client may read from.
    pub read_patterns: Vec<String>,
    /// Record name patterns the client may write to.
    pub write_patterns: Vec<String>,
}

impl Permissions {
    /// Creates a permission set that grants full access to all records.
    pub fn allow_all() -> Self {
        Self {
            read_patterns: vec!["#".to_string()],
            write_patterns: vec!["#".to_string()],
        }
    }

    /// Returns `true` if the client is allowed to access to record with `key`.
    ///
    /// `key` is a registered record key or a pattern of record key —
    /// so this asks **pattern containment**
    /// Currently, the server passes concrete keys to setup permissions bitmasks
    /// for a client during http upgrade.
    /// ([`pattern_contains`](aimdb_core::pattern_contains)):
    /// does a granted pattern cover the *whole* requested pattern? Plain
    /// [`topic_matches`](aimdb_core::topic_matches) would let a one-level grant
    /// (`sensors.*`) admit an all-levels request (`sensors.#`) by having the
    /// `*` swallow the `#`, silently widening the grant. For a concrete request
    /// `pattern_contains` collapses to `topic_matches`, so exact permissions are
    /// unaffected.
    pub fn can_read(&self, key: &str) -> bool {
        self.read_patterns
            .iter()
            .any(|p| aimdb_core::pattern_contains(p, key))
    }

    /// Returns `true` if the client is allowed to write to a record given with key.
    ///
    /// Writes target a single concrete record, so `key` is never a wildcard
    /// here and plain [`topic_matches`](aimdb_core::topic_matches) is the right
    /// check (a wildcard write key would resolve to no record downstream).
    pub fn can_write(&self, topic: &str) -> bool {
        self.write_patterns
            .iter()
            .any(|p| aimdb_core::topic_matches(p, topic))
    }
}

/// Bit mask to store records that client has access to.
///
/// Index `i` of the mask represent access to record id `i`.
/// The record having `id` assigned incrementally, according to [`aimdb_core::builder::AimDbInner`]
/// As the crate does not use any dependency for bit set, we craft one
/// from vector of `u8` for finer granularity.
///
/// `i` is proportionate to bit significance, so a block mask could be easily constructed
/// using `1u8 << i`
#[derive(Debug, Clone)]
pub struct RecordsBits {
    length: usize,
    masks: Vec<u8>,
}

impl RecordsBits {
    pub fn new(length: usize) -> Self {
        let blocks = length.div_ceil(8);
        let masks: Vec<u8> = if length > 0 {
            (0..blocks).into_iter().map(|_| 0u8).collect()
        } else {
            Vec::new()
        };
        Self { length, masks }
    }

    pub fn len(&self) -> usize {
        self.length
    }

    pub fn is_empty(&self) -> bool {
        self.length == 0
    }

    /// Create from Permissions
    pub fn resolve_permissions(records: &[String], permissions: &Permissions) -> Self {
        let mut records_bits = Self::new(records.len());
        records.iter().enumerate().for_each(|(i, key)| {
            if permissions.can_read(key.as_str()) {
                let _ = records_bits.set(i);
            };
        });
        records_bits
    }

    /// Checks whether record at `index` is accessible.
    /// Out-of-index index return `false`, so no panic.
    pub fn is_allowed(&self, index: usize) -> bool {
        if index >= self.length {
            return false;
        };

        // Bit index inside the block,
        let block_index = self.block_index(index);
        let offset = Self::offset(index, block_index);
        let mask = 1u8 << offset;
        self.masks[block_index] & mask != 0
    }

    /// Set record at `index` accessible, our-of-index returns `false`
    pub fn set(&mut self, index: usize) -> bool {
        if index >= self.length {
            return false;
        };

        // Bit index inside the block,
        let block_index = self.block_index(index);
        let offset = Self::offset(index, block_index);
        let mask = 1u8 << offset;
        self.masks[block_index] |= mask;
        true
    }

    fn block_index(&self, index: usize) -> usize {
        index / 8
    }

    fn offset(index: usize, block_index: usize) -> usize {
        index - block_index * 8
    }

    pub fn has_permissions(&self) -> bool {
        self.masks.iter().any(|v| *v > 0)
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
    use super::{Permissions, RecordsBits};

    fn perms(subscribe: &[&str]) -> Permissions {
        Permissions {
            read_patterns: subscribe.iter().map(|s| s.to_string()).collect(),
            write_patterns: Vec::new(),
        }
    }

    #[test]
    fn records_bits_works() {
        let mut records_bits = RecordsBits::new(20);

        assert_eq!(records_bits.masks.len(), 3);

        records_bits.set(11);
        assert!(records_bits.is_allowed(11));
        assert!(!records_bits.is_allowed(10));

        // Ouf of index
        assert!(!records_bits.set(21));
    }

    #[test]
    fn subscribe_grant_is_not_widened_by_a_wildcard_request() {
        // A one-level grant must not admit an all-levels request (the `*`-eats-`#`
        // escalation): `sensors.*` covers one level, `sensors.#` covers all.
        let p = perms(&["sensors.*"]);
        assert!(p.can_read("sensors.temp")); // in scope
        assert!(!p.can_read("sensors.#")); // escalation — denied
        assert!(!p.can_read("sensors.temp.vienna")); // deeper — denied
    }

    #[test]
    fn subscribe_grant_with_an_interior_hash_keeps_its_suffix() {
        // A grant scoped to `secret` leaves at any depth must not admit the rest
        // of the subtree: the `#` used to short-circuit the whole match, so every
        // segment after it was ignored and this grant behaved like `tenant.#`.
        let p = perms(&["tenant.#.secret"]);
        assert!(p.can_read("tenant.secret"));
        assert!(p.can_read("tenant.a.b.secret"));
        assert!(!p.can_read("tenant.public"));
        assert!(!p.can_read("tenant.a.b.public"));
        assert!(!p.can_read("tenant.#")); // no escalation to the subtree
    }

    #[test]
    fn subscribe_allows_requests_the_grant_actually_covers() {
        assert!(perms(&["#"]).can_read("sensors.#"));
        let p = perms(&["sensors.#"]);
        assert!(p.can_read("sensors.#"));
        assert!(p.can_read("sensors.temp.#"));
        assert!(p.can_read("sensors.temp"));
        // Out of the granted subtree stays denied.
        assert!(!p.can_read("commands.#"));
    }

    #[test]
    fn permissions_bitmask_from_grant_and_perms() {
        // Seed permissions
        let perms = perms(&["home.#", "front.#.test", "garage.#"]);
        let mut records: Vec<String> = Vec::new();
        records.extend(
            [
                "home.1.1",
                "back.1",
                "front.1",
                "front.2.test",
                "back.2",
                "garage.inner.1",
                "garage.outer.1",
                "storage.1",
            ]
            .iter()
            .map(|s| s.to_string()),
        );

        let record_perms = RecordsBits::resolve_permissions(&records, &perms);

        assert_eq!(record_perms.len(), records.len());
        assert!(record_perms.is_allowed(0));
        assert!(!record_perms.is_allowed(1));
        assert!(!record_perms.is_allowed(2));
        assert!(record_perms.is_allowed(3));
        assert!(!record_perms.is_allowed(4));
        assert!(record_perms.is_allowed(5));
        assert!(record_perms.is_allowed(6));
        assert!(!record_perms.is_allowed(7));
    }
}
