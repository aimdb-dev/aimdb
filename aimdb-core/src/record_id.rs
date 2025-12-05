//! Record identification types for stable, O(1) lookups
//!
//! This module provides two key types for record identification:
//!
//! - [`RecordKey`]: A stable, human-readable identifier for external APIs
//! - [`RecordId`]: An internal index for O(1) hot-path lookups
//!
//! # Design Rationale
//!
//! AimDB separates *logical identity* (RecordKey) from *physical identity* (RecordId):
//!
//! - **RecordKey** is used by external systems (MCP, CLI, config files) and supports
//!   multiple records of the same Rust type (e.g., "sensors.outdoor" and "sensors.indoor"
//!   can both be `Temperature` records).
//!
//! - **RecordId** is the hot-path identifier used internally for O(1) Vec indexing
//!   during produce/consume operations.
//!
//! # Examples
//!
//! ```rust
//! use aimdb_core::record_id::RecordKey;
//!
//! // Static keys (zero allocation, preferred)
//! let key: RecordKey = "sensors.temperature".into();
//! assert!(key.is_static());
//!
//! // Dynamic keys (for runtime-generated names)
//! let tenant_id = "acme";
//! let key = RecordKey::dynamic(format!("tenant.{}.sensors", tenant_id));
//! assert!(!key.is_static());
//!
//! // RecordId is internal, obtained from the database (not user-constructed)
//! // let id = db.resolve("sensors.temperature").unwrap();
//! ```

#[cfg(not(feature = "std"))]
extern crate alloc;

#[cfg(not(feature = "std"))]
use alloc::sync::Arc;

#[cfg(feature = "std")]
use std::sync::Arc;

/// Stable identifier for a record
///
/// Supports both static (zero-cost) and dynamic (Arc-allocated) names.
/// Use string literals for the common case; they auto-convert via `From`.
///
/// # Naming Convention
///
/// Recommended format: `<namespace>.<category>.<instance>`
///
/// Examples:
/// - `sensors.temperature.outdoor`
/// - `sensors.temperature.indoor`
/// - `mesh.weather.sf-bay`
/// - `config.app.settings`
/// - `tenant.acme.sensors.temp`
///
/// # Examples
///
/// ```rust
/// use aimdb_core::record_id::RecordKey;
///
/// // Static (preferred) - zero allocation
/// let key: RecordKey = "sensors.temperature".into();
///
/// // Dynamic - for runtime-generated names
/// let key = RecordKey::dynamic(format!("tenant.{}.sensors", "acme"));
/// ```
#[derive(Clone, Debug)]
pub struct RecordKey(RecordKeyInner);

#[derive(Clone, Debug)]
enum RecordKeyInner {
    /// Static string literal (zero allocation, pointer comparison possible)
    Static(&'static str),
    /// Dynamic runtime string (Arc for cheap cloning)
    Dynamic(Arc<str>),
}

impl RecordKey {
    /// Create from a static string literal
    ///
    /// This is a const fn, usable in const contexts.
    #[inline]
    pub const fn new(s: &'static str) -> Self {
        Self(RecordKeyInner::Static(s))
    }

    /// Create from a runtime-generated string
    ///
    /// Use this for dynamic names (multi-tenant, config-driven, etc.).
    #[inline]
    pub fn dynamic(s: impl Into<Arc<str>>) -> Self {
        Self(RecordKeyInner::Dynamic(s.into()))
    }

    /// Create from a runtime string (alias for `dynamic`)
    ///
    /// Allocates an `Arc<str>` to store the string.
    #[inline]
    pub fn from_dynamic(s: &str) -> Self {
        Self(RecordKeyInner::Dynamic(Arc::from(s)))
    }

    /// Get the string representation
    #[inline]
    pub fn as_str(&self) -> &str {
        match &self.0 {
            RecordKeyInner::Static(s) => s,
            RecordKeyInner::Dynamic(s) => s,
        }
    }

    /// Returns true if this is a static (zero-allocation) key
    #[inline]
    pub fn is_static(&self) -> bool {
        matches!(self.0, RecordKeyInner::Static(_))
    }
}

// ===== Trait Implementations =====

impl core::hash::Hash for RecordKey {
    fn hash<H: core::hash::Hasher>(&self, state: &mut H) {
        // Hash the string content, not the enum variant
        self.as_str().hash(state);
    }
}

impl PartialEq for RecordKey {
    fn eq(&self, other: &Self) -> bool {
        self.as_str() == other.as_str()
    }
}

impl Eq for RecordKey {}

impl PartialOrd for RecordKey {
    fn partial_cmp(&self, other: &Self) -> Option<core::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for RecordKey {
    fn cmp(&self, other: &Self) -> core::cmp::Ordering {
        self.as_str().cmp(other.as_str())
    }
}

/// Ergonomic conversion from string literals
impl From<&'static str> for RecordKey {
    #[inline]
    fn from(s: &'static str) -> Self {
        Self::new(s)
    }
}

impl core::fmt::Display for RecordKey {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl AsRef<str> for RecordKey {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

/// Enable O(1) HashMap lookup by &str
///
/// This allows `hashmap.get("string_literal")` without allocating a RecordKey.
impl core::borrow::Borrow<str> for RecordKey {
    fn borrow(&self) -> &str {
        self.as_str()
    }
}

// ===== Serde Support (std only) =====

#[cfg(feature = "std")]
impl serde::Serialize for RecordKey {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

#[cfg(feature = "std")]
impl<'de> serde::Deserialize<'de> for RecordKey {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        Ok(Self::dynamic(s))
    }
}

// ===== RecordId =====

/// Internal record identifier (index into storage Vec)
///
/// This is the hot-path identifier used for O(1) lookups during
/// produce/consume operations. Not exposed to external APIs.
///
/// # Why u32?
///
/// - 4 billion records is more than enough for any deployment
/// - Smaller than `usize` on 64-bit systems (cache efficiency)
/// - Copy-friendly (no clone overhead)
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub struct RecordId(pub(crate) u32);

impl RecordId {
    /// Create a new RecordId from an index
    ///
    /// # Arguments
    /// * `index` - The index into the record storage array
    #[inline]
    pub const fn new(index: u32) -> Self {
        Self(index)
    }

    /// Get the underlying index
    #[inline]
    pub const fn index(self) -> usize {
        self.0 as usize
    }

    /// Get the raw u32 value (for serialization)
    #[inline]
    pub const fn raw(self) -> u32 {
        self.0
    }
}

impl core::fmt::Display for RecordId {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "RecordId({})", self.0)
    }
}

#[cfg(feature = "std")]
impl serde::Serialize for RecordId {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_u32(self.0)
    }
}

#[cfg(feature = "std")]
impl<'de> serde::Deserialize<'de> for RecordId {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let index = u32::deserialize(deserializer)?;
        Ok(Self(index))
    }
}

// ===== Unit Tests (std only - uses String, HashMap, format!) =====

#[cfg(all(test, feature = "std"))]
mod tests {
    use super::*;

    #[test]
    fn test_record_key_static() {
        let key: RecordKey = "sensors.temperature".into();
        assert!(key.is_static());
        assert_eq!(key.as_str(), "sensors.temperature");
    }

    #[test]
    fn test_record_key_dynamic() {
        let key = RecordKey::dynamic("sensors.temperature".to_string());
        assert!(!key.is_static());
        assert_eq!(key.as_str(), "sensors.temperature");
    }

    #[test]
    fn test_record_key_equality() {
        let static_key: RecordKey = "sensors.temp".into();
        let dynamic_key = RecordKey::dynamic("sensors.temp".to_string());

        // Static and dynamic keys with same content should be equal
        assert_eq!(static_key, dynamic_key);
    }

    #[test]
    fn test_record_key_hash_consistency() {
        use core::hash::{Hash, Hasher};
        use std::collections::hash_map::DefaultHasher;

        fn hash_key(key: &RecordKey) -> u64 {
            let mut hasher = DefaultHasher::new();
            key.hash(&mut hasher);
            hasher.finish()
        }

        let static_key: RecordKey = "sensors.temp".into();
        let dynamic_key = RecordKey::dynamic("sensors.temp".to_string());

        // Hash should be the same for equal keys
        assert_eq!(hash_key(&static_key), hash_key(&dynamic_key));
    }

    #[test]
    fn test_record_key_borrow() {
        use std::collections::HashMap;

        let mut map: HashMap<RecordKey, i32> = HashMap::new();
        map.insert("sensors.temp".into(), 42);

        // Can lookup by &str without allocation
        assert_eq!(map.get("sensors.temp"), Some(&42));
    }

    #[test]
    fn test_record_id_basic() {
        let id = RecordId::new(42);
        assert_eq!(id.index(), 42);
        assert_eq!(id.raw(), 42);
    }

    #[test]
    fn test_record_id_copy() {
        let id1 = RecordId::new(10);
        let id2 = id1; // Copy, not move
        assert_eq!(id1, id2);
    }

    #[test]
    fn test_record_key_display() {
        let key: RecordKey = "sensors.temperature".into();
        assert_eq!(format!("{}", key), "sensors.temperature");
    }

    #[test]
    fn test_record_id_display() {
        let id = RecordId::new(42);
        assert_eq!(format!("{}", id), "RecordId(42)");
    }
}
