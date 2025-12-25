//! Record identification types for stable, O(1) lookups
//!
//! This module provides key types for record identification:
//!
//! - [`RecordKey`]: A **trait** for type-safe record identifiers
//! - [`StringKey`]: Default string-based key implementation
//! - [`RecordId`]: An internal index for O(1) hot-path lookups
//!
//! # Design Rationale
//!
//! AimDB separates *logical identity* (RecordKey) from *physical identity* (RecordId):
//!
//! - **RecordKey** is a trait that can be implemented by user-defined enums for
//!   compile-time checked keys, or use the default `StringKey` for string-based keys.
//!
//! - **RecordId** is the hot-path identifier used internally for O(1) Vec indexing
//!   during produce/consume operations.
//!
//! # Examples
//!
//! ## StringKey (default, edge/cloud)
//!
//! ```rust
//! use aimdb_core::record_id::StringKey;
//!
//! // Static keys (zero allocation, preferred)
//! let key: StringKey = "sensors.temperature".into();
//! assert!(key.is_static());
//!
//! // Dynamic keys (for runtime-generated names)
//! let tenant_id = "acme";
//! let key = StringKey::intern(format!("tenant.{}.sensors", tenant_id));
//! assert!(!key.is_static());
//! ```
//!
//! ## Enum Keys (compile-time safe, embedded)
//!
//! ```rust,ignore
//! use aimdb_derive::RecordKey;
//!
//! #[derive(RecordKey, Clone, Copy, PartialEq, Eq, Hash)]
//! pub enum AppKey {
//!     #[key = "temp.indoor"]
//!     TempIndoor,
//!     #[key = "temp.outdoor"]
//!     TempOutdoor,
//! }
//!
//! // Compile-time typo detection!
//! let producer = db.producer::<Temperature>(AppKey::TempIndoor);
//! ```

#[cfg(not(feature = "std"))]
extern crate alloc;

#[cfg(not(feature = "std"))]
use alloc::{boxed::Box, string::ToString};

#[cfg(feature = "std")]
use std::boxed::Box;

#[cfg(feature = "std")]
use core::sync::atomic::{AtomicUsize, Ordering};

/// Counter for interned keys (debug builds only, std only)
#[cfg(all(debug_assertions, feature = "std"))]
static INTERNED_KEY_COUNT: AtomicUsize = AtomicUsize::new(0);

/// Maximum expected interned keys before warning.
/// If exceeded, a debug assertion fires to catch potential misuse.
#[cfg(all(debug_assertions, feature = "std"))]
const MAX_EXPECTED_INTERNED_KEYS: usize = 1000;

// Re-export derive macro when feature is enabled
#[cfg(feature = "derive")]
pub use aimdb_derive::RecordKey;

// ============================================================================
// RecordKey Trait
// ============================================================================

/// Trait for record key types
///
/// Enables compile-time checked enum keys for embedded while preserving
/// String flexibility for edge/cloud deployments.
///
/// The `Borrow<str>` bound is required for O(1) HashMap lookups by string
/// in the remote access layer (e.g., `hashmap.get("record.name")`).
///
/// # Implementing RecordKey
///
/// The easiest way is to use the derive macro:
///
/// ```rust,ignore
/// #[derive(RecordKey, Clone, Copy, PartialEq, Eq, Hash)]
/// pub enum AppKey {
///     #[key = "temp.indoor"]
///     TempIndoor,
///     #[key = "temp.outdoor"]
///     TempOutdoor,
/// }
/// ```
///
/// With connector metadata (MQTT topics, KNX addresses):
///
/// ```rust,ignore
/// #[derive(RecordKey, Clone, Copy, PartialEq, Eq, Hash)]
/// pub enum SensorKey {
///     #[key = "temp.indoor"]
///     #[link_address = "mqtt://sensors/temp/indoor"]
///     TempIndoor,
///
///     #[key = "temp.outdoor"]
///     #[link_address = "knx://9/1/0"]
///     TempOutdoor,
/// }
/// ```
///
/// For manual implementation:
///
/// ```rust
/// use aimdb_core::RecordKey;
/// use core::borrow::Borrow;
///
/// #[derive(Clone, Copy, PartialEq, Eq, Hash)]
/// pub enum MyKey {
///     Temperature,
///     Humidity,
/// }
///
/// impl RecordKey for MyKey {
///     fn as_str(&self) -> &str {
///         match self {
///             Self::Temperature => "sensor.temp",
///             Self::Humidity => "sensor.humid",
///         }
///     }
/// }
///
/// impl Borrow<str> for MyKey {
///     fn borrow(&self) -> &str {
///         self.as_str()
///     }
/// }
/// ```
///
/// **Important:** The `Hash` implementation must hash the same value as
/// `as_str()` for HashMap lookups to work correctly. The derive macro
/// handles this automatically.
pub trait RecordKey:
    Clone + Eq + core::hash::Hash + core::borrow::Borrow<str> + Send + Sync + 'static
{
    /// String representation for connectors, logging, serialization, remote access
    fn as_str(&self) -> &str;

    /// Connector address for this key
    ///
    /// Returns the URL/address to use with connectors (MQTT topics, KNX addresses, etc.).
    /// Use with `.link_to()` for outbound or `.link_from()` for inbound connections.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// #[derive(RecordKey)]
    /// pub enum SensorKey {
    ///     #[key = "temp.indoor"]
    ///     #[link_address = "mqtt://sensors/temp/indoor"]
    ///     TempIndoor,
    /// }
    ///
    /// // Use with link_to for outbound
    /// reg.link_to(SensorKey::TempIndoor.link_address().unwrap())
    ///
    /// // Or with link_from for inbound
    /// reg.link_from(SensorKey::TempIndoor.link_address().unwrap())
    /// ```
    #[inline]
    fn link_address(&self) -> Option<&str> {
        None
    }
}

// Blanket implementation for &'static str
impl RecordKey for &'static str {
    #[inline]
    fn as_str(&self) -> &str {
        self
    }
}

// ============================================================================
// StringKey - Default Implementation
// ============================================================================

/// Default string-based record key
///
/// Supports both static (zero-cost) and interned (leaked) names.
/// Use string literals for the common case; they auto-convert via `From`.
///
/// # Memory Model
///
/// Dynamic keys are "interned" by leaking memory into `'static` lifetime.
/// This is optimal for AimDB's use case where keys are:
/// - Registered once at startup
/// - Never deallocated during runtime
/// - Frequently cloned (now just pointer copy)
///
/// The tradeoff: memory for dynamic keys is never freed. This is acceptable
/// because typical deployments have <100 keys totaling <4KB.
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
/// use aimdb_core::record_id::StringKey;
///
/// // Static (preferred) - zero allocation
/// let key: StringKey = "sensors.temperature".into();
///
/// // Dynamic - for runtime-generated names
/// let key = StringKey::intern(format!("tenant.{}.sensors", "acme"));
/// ```
#[derive(Clone, Copy)]
pub struct StringKey(StringKeyInner);

#[derive(Clone, Copy)]
enum StringKeyInner {
    /// Static string literal (zero allocation)
    Static(&'static str),
    /// Interned runtime string (leaked into 'static lifetime)
    ///
    /// Memory is intentionally leaked for O(1) cloning and comparison.
    /// This is safe because keys are registered once at startup.
    Interned(&'static str),
}

impl StringKey {
    /// Create from a static string literal
    ///
    /// This is a const fn, usable in const contexts.
    #[inline]
    #[must_use]
    pub const fn new(s: &'static str) -> Self {
        Self(StringKeyInner::Static(s))
    }

    /// Create from a runtime-generated string
    ///
    /// Use this for dynamic names (multi-tenant, config-driven, etc.).
    ///
    /// # Memory
    ///
    /// The string is leaked into `'static` lifetime. This is intentional:
    /// - Keys are registered once at startup
    /// - Enables O(1) Copy/Clone
    /// - Typical overhead: <4KB for 100 keys
    ///
    /// # Panics (debug builds only)
    ///
    /// In debug builds with `std` feature, panics if more than 1000 keys are
    /// interned. This catches accidental misuse (e.g., creating keys in a loop).
    /// Production builds have no limit.
    #[inline]
    #[must_use]
    pub fn intern(s: impl AsRef<str>) -> Self {
        #[cfg(all(debug_assertions, feature = "std"))]
        {
            let count = INTERNED_KEY_COUNT.fetch_add(1, Ordering::Relaxed);
            debug_assert!(
                count < MAX_EXPECTED_INTERNED_KEYS,
                "StringKey::intern() called {} times. This exceeds the expected limit of {}. \
                 Interned keys leak memory and should only be created at startup. \
                 Use static string literals or enum keys for better performance.",
                count + 1,
                MAX_EXPECTED_INTERNED_KEYS
            );
        }
        let leaked: &'static str = Box::leak(s.as_ref().to_string().into_boxed_str());
        Self(StringKeyInner::Interned(leaked))
    }

    /// Create from a runtime string (alias for `intern`)
    ///
    /// The string is leaked into `'static` lifetime for O(1) cloning.
    #[inline]
    #[must_use]
    pub fn from_dynamic(s: &str) -> Self {
        Self::intern(s)
    }

    /// Returns true if this is a static (compile-time) key
    #[inline]
    pub fn is_static(&self) -> bool {
        matches!(self.0, StringKeyInner::Static(_))
    }

    /// Returns true if this is an interned (runtime) key
    #[inline]
    pub fn is_interned(&self) -> bool {
        matches!(self.0, StringKeyInner::Interned(_))
    }
}

// Implement RecordKey trait for StringKey
impl RecordKey for StringKey {
    #[inline]
    fn as_str(&self) -> &str {
        match self.0 {
            StringKeyInner::Static(s) => s,
            StringKeyInner::Interned(s) => s,
        }
    }
}

// Custom Debug to show Static vs Interned variant
impl core::fmt::Debug for StringKey {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self.0 {
            StringKeyInner::Static(s) => f.debug_tuple("StringKey::Static").field(&s).finish(),
            StringKeyInner::Interned(s) => f.debug_tuple("StringKey::Interned").field(&s).finish(),
        }
    }
}

impl core::hash::Hash for StringKey {
    fn hash<H: core::hash::Hasher>(&self, state: &mut H) {
        // Hash the string content, not the enum variant
        self.as_str().hash(state);
    }
}

impl PartialEq for StringKey {
    fn eq(&self, other: &Self) -> bool {
        self.as_str() == other.as_str()
    }
}

impl Eq for StringKey {}

/// Enable direct comparison with &str
impl PartialEq<str> for StringKey {
    fn eq(&self, other: &str) -> bool {
        self.as_str() == other
    }
}

/// Enable direct comparison with &str reference
impl PartialEq<&str> for StringKey {
    fn eq(&self, other: &&str) -> bool {
        self.as_str() == *other
    }
}

impl PartialOrd for StringKey {
    fn partial_cmp(&self, other: &Self) -> Option<core::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for StringKey {
    fn cmp(&self, other: &Self) -> core::cmp::Ordering {
        self.as_str().cmp(other.as_str())
    }
}

/// Ergonomic conversion from string literals
impl From<&'static str> for StringKey {
    #[inline]
    fn from(s: &'static str) -> Self {
        Self::new(s)
    }
}

/// Ergonomic conversion from owned String (no_std with alloc)
#[cfg(all(feature = "alloc", not(feature = "std")))]
impl From<alloc::string::String> for StringKey {
    #[inline]
    fn from(s: alloc::string::String) -> Self {
        Self::intern(s)
    }
}

/// Ergonomic conversion from owned String (std)
#[cfg(feature = "std")]
impl From<String> for StringKey {
    #[inline]
    fn from(s: String) -> Self {
        Self::intern(s)
    }
}

impl core::fmt::Display for StringKey {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl AsRef<str> for StringKey {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

/// Enable O(1) HashMap lookup by &str
///
/// This allows `hashmap.get("string_literal")` without allocating a StringKey.
impl core::borrow::Borrow<str> for StringKey {
    fn borrow(&self) -> &str {
        self.as_str()
    }
}

// ===== Serde Support (std only) =====

#[cfg(feature = "std")]
impl serde::Serialize for StringKey {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

#[cfg(feature = "std")]
impl<'de> serde::Deserialize<'de> for StringKey {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        Ok(Self::intern(s))
    }
}

// ============================================================================
// RecordId - Internal Index
// ============================================================================

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
    fn test_string_key_static() {
        let key: StringKey = "sensors.temperature".into();
        assert!(key.is_static());
        assert_eq!(key.as_str(), "sensors.temperature");
    }

    #[test]
    fn test_string_key_interned() {
        // Concatenation creates a truly dynamic String
        let key = StringKey::intern(["sensors", ".", "temperature"].concat());
        assert!(!key.is_static());
        assert!(key.is_interned());
        assert_eq!(key.as_str(), "sensors.temperature");
    }

    #[test]
    fn test_string_key_equality() {
        let static_key: StringKey = "sensors.temp".into();
        // Concatenation creates a truly dynamic String
        let interned_key = StringKey::intern(["sensors", ".", "temp"].concat());

        // Static and interned keys with same content should be equal
        assert_eq!(static_key, interned_key);
    }

    #[test]
    fn test_string_key_hash_consistency() {
        use core::hash::{Hash, Hasher};
        use std::collections::hash_map::DefaultHasher;

        fn hash_key(key: &StringKey) -> u64 {
            let mut hasher = DefaultHasher::new();
            key.hash(&mut hasher);
            hasher.finish()
        }

        let static_key: StringKey = "sensors.temp".into();
        // Concatenation creates a truly dynamic String
        let interned_key = StringKey::intern(["sensors", ".", "temp"].concat());

        // Hash should be the same for equal keys
        assert_eq!(hash_key(&static_key), hash_key(&interned_key));
    }

    #[test]
    fn test_string_key_borrow() {
        use std::collections::HashMap;

        let mut map: HashMap<StringKey, i32> = HashMap::new();
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
    fn test_string_key_display() {
        let key: StringKey = "sensors.temperature".into();
        assert_eq!(format!("{}", key), "sensors.temperature");
    }

    #[test]
    fn test_string_key_debug() {
        let static_key: StringKey = "sensors.temp".into();
        // Concatenation creates a truly dynamic String
        let interned_key = StringKey::intern(["sensors", ".", "temp"].concat());

        // Debug output should distinguish static vs interned
        let static_debug = format!("{:?}", static_key);
        let interned_debug = format!("{:?}", interned_key);

        assert!(static_debug.contains("Static"));
        assert!(interned_debug.contains("Interned"));
    }

    #[test]
    fn test_string_key_partial_eq_str() {
        let key: StringKey = "sensors.temperature".into();

        // Direct comparison with &str
        assert!(key == "sensors.temperature");
        assert!(key != "other.key");

        // Also works with str reference
        let s: &str = "sensors.temperature";
        assert!(key == s);
    }

    #[test]
    fn test_string_key_from_string() {
        let owned = "sensors.temperature".to_string();
        let key: StringKey = owned.into();

        assert!(!key.is_static());
        assert_eq!(key.as_str(), "sensors.temperature");
    }

    #[test]
    fn test_record_id_display() {
        let id = RecordId::new(42);
        assert_eq!(format!("{}", id), "RecordId(42)");
    }

    #[test]
    fn test_static_str_record_key() {
        // &'static str implements RecordKey
        let key: &'static str = "sensors.temp";
        assert_eq!(<&str as RecordKey>::as_str(&key), "sensors.temp");
    }

    #[test]
    fn test_string_key_record_key_trait() {
        use crate::RecordKey;

        let key: StringKey = "sensors.temp".into();
        assert_eq!(RecordKey::as_str(&key), "sensors.temp");
    }

    /// Test that hash(key) == hash(key.borrow()) per Rust's Borrow trait contract.
    ///
    /// This is critical for HashMap operations: if Borrow<str> is implemented,
    /// the hash of the key must match the hash of its borrowed string form.
    #[test]
    fn test_string_key_hash_borrow_contract() {
        use core::borrow::Borrow;
        use core::hash::{Hash, Hasher};
        use std::collections::hash_map::DefaultHasher;

        fn hash_value<T: Hash>(t: &T) -> u64 {
            let mut h = DefaultHasher::new();
            t.hash(&mut h);
            h.finish()
        }

        // Test static key
        let static_key: StringKey = "sensors.temp".into();
        let borrowed: &str = static_key.borrow();
        assert_eq!(
            hash_value(&static_key),
            hash_value(&borrowed),
            "Static StringKey hash must match its borrowed string hash"
        );

        // Test interned key
        let interned_key = StringKey::intern(["sensors", ".", "temp"].concat());
        let borrowed: &str = interned_key.borrow();
        assert_eq!(
            hash_value(&interned_key),
            hash_value(&borrowed),
            "Interned StringKey hash must match its borrowed string hash"
        );

        // Test that HashMap lookup by &str works (practical consequence)
        use std::collections::HashMap;
        let mut map: HashMap<StringKey, i32> = HashMap::new();
        map.insert("key.one".into(), 1);
        map.insert(StringKey::intern("key.two"), 2);

        assert_eq!(map.get("key.one"), Some(&1));
        assert_eq!(map.get("key.two"), Some(&2));
        assert_eq!(map.get("key.nonexistent"), None);
    }
}
