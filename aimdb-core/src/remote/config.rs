//! Configuration types for AimX remote access

use core::any::TypeId;
use std::{collections::HashSet, path::PathBuf, string::String, vec::Vec};

/// Configuration for AimX remote access
///
/// Defines how the remote access layer behaves, including socket path,
/// security policy, connection limits, and subscription queue sizes.
#[derive(Debug, Clone)]
pub struct AimxConfig {
    /// Path to Unix domain socket
    pub socket_path: PathBuf,

    /// Security policy (read-only or read-write)
    pub security_policy: SecurityPolicy,

    /// Maximum number of concurrent connections
    pub max_connections: usize,

    /// Subscription queue size per client per subscription
    pub subscription_queue_size: usize,

    /// Optional authentication token
    pub auth_token: Option<String>,

    /// File permissions for the socket (Unix only)
    /// Format: octal mode (e.g., 0o600 for owner-only)
    pub socket_permissions: Option<u32>,
}

impl AimxConfig {
    /// Creates a default UDS configuration
    ///
    /// # Defaults
    /// - Socket path: `/tmp/aimdb.sock`
    /// - Security policy: Read-only
    /// - Max connections: 16
    /// - Subscription queue size: 100
    /// - No auth token
    /// - Socket permissions: 0o600 (owner-only)
    pub fn uds_default() -> Self {
        Self {
            socket_path: PathBuf::from("/tmp/aimdb.sock"),
            security_policy: SecurityPolicy::ReadOnly,
            max_connections: 16,
            subscription_queue_size: 100,
            auth_token: None,
            socket_permissions: Some(0o600),
        }
    }

    /// Sets the socket path
    pub fn socket_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.socket_path = path.into();
        self
    }

    /// Sets the security policy
    pub fn security_policy(mut self, policy: SecurityPolicy) -> Self {
        self.security_policy = policy;
        self
    }

    /// Sets the maximum number of concurrent connections
    pub fn max_connections(mut self, max: usize) -> Self {
        self.max_connections = max;
        self
    }

    /// Sets the subscription queue size per client
    pub fn subscription_queue_size(mut self, size: usize) -> Self {
        self.subscription_queue_size = size;
        self
    }

    /// Sets an authentication token
    pub fn auth_token(mut self, token: impl Into<String>) -> Self {
        self.auth_token = Some(token.into());
        self
    }

    /// Sets the socket file permissions (Unix only)
    ///
    /// # Example
    /// ```rust,ignore
    /// config.socket_permissions(0o600)  // Owner only
    /// config.socket_permissions(0o660)  // Owner + group
    /// ```
    pub fn socket_permissions(mut self, mode: u32) -> Self {
        self.socket_permissions = Some(mode);
        self
    }
}

/// Security policy for remote access
///
/// Defines which operations are permitted and for which records.
#[derive(Debug, Clone)]
pub enum SecurityPolicy {
    /// Read-only access (list, get, subscribe)
    ///
    /// This is the default and recommended policy for most deployments.
    /// No write operations are permitted.
    ReadOnly,

    /// Read-write access with explicit per-record opt-in
    ///
    /// Write operations (`record.set`) are only allowed for records
    /// whose TypeId is in the `writable_records` set.
    ReadWrite {
        /// Set of TypeIds that allow write operations
        writable_records: HashSet<TypeId>,
    },
}

impl SecurityPolicy {
    /// Creates a read-only policy
    pub fn read_only() -> Self {
        Self::ReadOnly
    }

    /// Creates a read-write policy with no writable records initially
    pub fn read_write() -> Self {
        Self::ReadWrite {
            writable_records: HashSet::new(),
        }
    }

    /// Adds a record type to the writable set
    ///
    /// Only has effect for ReadWrite policies. Panics if policy is ReadOnly.
    pub fn allow_write<T: 'static>(&mut self) {
        match self {
            Self::ReadWrite { writable_records } => {
                writable_records.insert(TypeId::of::<T>());
            }
            Self::ReadOnly => {
                panic!("Cannot allow writes in ReadOnly security policy");
            }
        }
    }

    /// Checks if a record type is writable
    pub fn is_writable(&self, type_id: TypeId) -> bool {
        match self {
            Self::ReadOnly => false,
            Self::ReadWrite { writable_records } => writable_records.contains(&type_id),
        }
    }

    /// Returns the list of granted permissions
    pub fn permissions(&self) -> &[&str] {
        match self {
            Self::ReadOnly => &["read", "subscribe"],
            Self::ReadWrite { .. } => &["read", "subscribe", "write"],
        }
    }

    /// Returns the list of writable record TypeIds (for ReadWrite policy)
    pub fn writable_records(&self) -> Vec<TypeId> {
        match self {
            Self::ReadOnly => Vec::new(),
            Self::ReadWrite { writable_records } => writable_records.iter().copied().collect(),
        }
    }
}

/// Builder helper for SecurityPolicy with chained API
impl SecurityPolicy {
    /// Builder pattern: Creates ReadWrite policy and allows write for a type
    pub fn with_writable_record<T: 'static>(mut self) -> Self {
        if let Self::ReadWrite {
            ref mut writable_records,
        } = self
        {
            writable_records.insert(TypeId::of::<T>());
        }
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(feature = "std")]
    fn test_default_config() {
        let config = AimxConfig::uds_default();
        assert_eq!(config.socket_path, PathBuf::from("/tmp/aimdb.sock"));
        assert_eq!(config.max_connections, 16);
        assert_eq!(config.subscription_queue_size, 100);
        assert!(matches!(config.security_policy, SecurityPolicy::ReadOnly));
        assert!(config.auth_token.is_none());
    }

    #[test]
    #[cfg(feature = "std")]
    fn test_config_builder() {
        let config = AimxConfig::uds_default()
            .socket_path("/var/run/aimdb.sock")
            .max_connections(32)
            .subscription_queue_size(200)
            .auth_token("secret-token")
            .socket_permissions(0o660);

        assert_eq!(config.socket_path, PathBuf::from("/var/run/aimdb.sock"));
        assert_eq!(config.max_connections, 32);
        assert_eq!(config.subscription_queue_size, 200);
        assert_eq!(config.auth_token, Some("secret-token".to_string()));
        assert_eq!(config.socket_permissions, Some(0o660));
    }

    #[test]
    fn test_security_policy_read_only() {
        let policy = SecurityPolicy::read_only();
        assert!(!policy.is_writable(TypeId::of::<i32>()));
        assert_eq!(policy.permissions(), &["read", "subscribe"]);
    }

    #[test]
    fn test_security_policy_read_write() {
        let mut policy = SecurityPolicy::read_write();
        assert!(!policy.is_writable(TypeId::of::<i32>()));

        policy.allow_write::<i32>();
        assert!(policy.is_writable(TypeId::of::<i32>()));
        assert!(!policy.is_writable(TypeId::of::<String>()));
        assert_eq!(policy.permissions(), &["read", "subscribe", "write"]);
    }

    #[test]
    #[should_panic(expected = "Cannot allow writes in ReadOnly security policy")]
    fn test_security_policy_read_only_panic() {
        let mut policy = SecurityPolicy::read_only();
        policy.allow_write::<i32>();
    }

    #[test]
    fn test_security_policy_builder() {
        let policy = SecurityPolicy::read_write()
            .with_writable_record::<i32>()
            .with_writable_record::<String>();

        assert!(policy.is_writable(TypeId::of::<i32>()));
        assert!(policy.is_writable(TypeId::of::<String>()));
        assert!(!policy.is_writable(TypeId::of::<f64>()));
    }
}
