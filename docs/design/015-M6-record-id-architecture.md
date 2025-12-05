# RecordId Architecture: Replacing TypeId with Stable Record Identifiers

**Version:** 1.0  
**Status:** Design Document  
**Milestone:** M7  
**Last Updated:** December 5, 2025

---

## Table of Contents

- [Overview](#overview)
- [Problem Statement](#problem-statement)
- [Design Goals](#design-goals)
- [Proposed Architecture](#proposed-architecture)
- [RecordKey Type](#recordkey-type)
- [RecordId Type](#recordid-type)
- [Core Registry Structure](#core-registry-structure)
- [API Changes](#api-changes)
- [Type Safety](#type-safety)
- [Migration Guide](#migration-guide)
- [Performance Analysis](#performance-analysis)
- [Implementation Plan](#implementation-plan)
- [Testing Strategy](#testing-strategy)
- [Future Extensions](#future-extensions)

---

## Overview

This document specifies the replacement of `TypeId`-based record storage with a `RecordId`-based architecture. This change enables:

1. **Multiple logical records of the same Rust type**
2. **Stable record identifiers** for external systems (MCP, CLI, config files)
3. **O(1) hot-path lookups** via Vec indexing
4. **Dynamic record registration** for runtime providers

### Key Insight

`TypeId` conflates two distinct concepts:
- **Rust type** (compile-time, for type safety)
- **Logical record** (runtime, for data flow identity)

By separating these, we unlock multi-instance patterns critical for multi-tenant, multi-sensor, and mesh network use cases.

---

## Problem Statement

### Current Architecture

```rust
pub struct AimDbInner {
    pub records: BTreeMap<TypeId, Box<dyn AnyRecord>>,
}
```

### Limitations

| Problem | Impact |
|---------|--------|
| One record per Rust type | Cannot have `Temperature` for factory floor AND outdoor sensor |
| TypeId not stable | Changes between Rust versions, cannot persist or transmit |
| O(n) name lookups | Remote access scans all records to find by name |
| No dynamic registration | Cannot add sensors at runtime |

### Blocking Use Cases

1. **Multi-tenant mesh servers** - Each tenant needs separate `MeshObservation` records
2. **Supabase integration** - Need stable IDs for database foreign keys
3. **Dynamic sensor providers** - Runtime sensor discovery/registration
4. **Configuration-driven setup** - Define records in YAML/JSON, not just Rust code

---

## Design Goals

| Goal | Priority | Rationale |
|------|----------|-----------|
| Multiple records per type | P0 | Core product requirement |
| Stable external identity | P0 | MCP, CLI, config files need it |
| O(1) hot-path access | P0 | Latency-critical data flow |
| Type safety preserved | P0 | Must not regress safety |
| Minimal API churn | P1 | Existing patterns should feel familiar |
| no_std compatible | P1 | Embassy adapter must work |
| Dynamic registration | P2 | Future provider system |

### Non-Goals (This Iteration)

- ❌ Hierarchical record namespaces with wildcard queries
- ❌ Record aliasing or symbolic links
- ❌ Cross-database record references
- ❌ Runtime type registration (types still known at compile time)

---

## Proposed Architecture

### High-Level Structure

```
┌─────────────────────────────────────────────────────────────────┐
│                         AimDbInner                              │
├─────────────────────────────────────────────────────────────────┤
│                                                                 │
│  ┌─────────────────────────────────────────────────────────┐   │
│  │ storages: Vec<Box<dyn AnyRecord>>                       │   │
│  │ ────────────────────────────────────────────────────────│   │
│  │ [0] TypedRecord<Temperature>  "sensors.outdoor"         │   │
│  │ [1] TypedRecord<Temperature>  "sensors.indoor"          │   │
│  │ [2] TypedRecord<Command>      "commands.hvac"           │   │
│  │ [3] TypedRecord<MeshObs>      "mesh.weather.sf"         │   │
│  └─────────────────────────────────────────────────────────┘   │
│           ▲                                                     │
│           │ RecordId(0), RecordId(1), ...                      │
│           │                                                     │
│  ┌────────┴────────────────────────────────────────────────┐   │
│  │ by_key: HashMap<RecordKey, RecordId>                    │   │
│  │ ───────────────────────────────────────────────────────│   │
│  │ "sensors.outdoor" → RecordId(0)                        │   │
│  │ "sensors.indoor"  → RecordId(1)                        │   │
│  │ "commands.hvac"   → RecordId(2)                        │   │
│  │ "mesh.weather.sf" → RecordId(3)                        │   │
│  └─────────────────────────────────────────────────────────┘   │
│                                                                 │
│  ┌─────────────────────────────────────────────────────────┐   │
│  │ by_type: HashMap<TypeId, Vec<RecordId>>  (optional)     │   │
│  │ ───────────────────────────────────────────────────────│   │
│  │ TypeId(Temperature) → [RecordId(0), RecordId(1)]       │   │
│  │ TypeId(Command)     → [RecordId(2)]                    │   │
│  │ TypeId(MeshObs)     → [RecordId(3)]                    │   │
│  └─────────────────────────────────────────────────────────┘   │
│                                                                 │
└─────────────────────────────────────────────────────────────────┘
```

---

## RecordKey Type

### Design Rationale

Records need human-readable, stable identifiers. We use a hybrid approach:

- **Static case (99%)**: Zero-allocation `&'static str` for compile-time literals
- **Dynamic case (1%)**: `Arc<str>` for runtime-generated names

### Implementation

```rust
/// Stable identifier for a record
///
/// Supports both static (zero-cost) and dynamic (Arc-allocated) names.
/// Use string literals for the common case; they auto-convert via `From`.
///
/// # Examples
///
/// ```rust
/// // Static (preferred) - zero allocation
/// let key: RecordKey = "sensors.temperature".into();
///
/// // Dynamic - for runtime-generated names
/// let key = RecordKey::dynamic(format!("tenant.{}.sensors", tenant_id));
/// ```
#[derive(Clone, Debug, Hash, Eq, PartialEq)]
pub struct RecordKey(RecordKeyInner);

#[derive(Clone, Debug, Hash, Eq, PartialEq)]
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

// Ergonomic conversion from string literals
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

// Serde support (std only)
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
```

### Naming Convention

Recommended (enforced by documentation, optionally validated):

```
<namespace>.<category>.<instance>

Examples:
  sensors.temperature.outdoor
  sensors.temperature.indoor
  mesh.weather.sf-bay
  config.app.settings
  tenant.acme.sensors.temp
```

### no_std Considerations

For `no_std` environments, `Arc<str>` requires the `alloc` crate. Since AimDB already requires `alloc` for `Vec` and `Box`, this is not a new dependency.

---

## RecordId Type

### Design

`RecordId` is the internal, hot-path identifier. It's a simple index into the storage `Vec`.

```rust
/// Internal record identifier (index into storage Vec)
///
/// This is the hot-path identifier used for O(1) lookups during
/// produce/consume operations. Not exposed to external APIs.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub struct RecordId(pub(crate) u32);

impl RecordId {
    /// Create a new RecordId
    #[inline]
    pub(crate) const fn new(index: u32) -> Self {
        Self(index)
    }

    /// Get the underlying index
    #[inline]
    pub(crate) const fn index(self) -> usize {
        self.0 as usize
    }
}

impl core::fmt::Display for RecordId {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "RecordId({})", self.0)
    }
}
```

### Why u32?

- 4 billion records is more than enough
- Smaller than `usize` on 64-bit systems (cache efficiency)
- Copy-friendly (no clone overhead)

---

## Core Registry Structure

### Implementation

```rust
/// Internal database state with RecordId-based storage
pub struct AimDbInner {
    /// Record storage (hot path - indexed by RecordId)
    ///
    /// Immutable after build(). Order matches registration order.
    storages: Vec<Box<dyn AnyRecord>>,

    /// Name → RecordId lookup (control plane)
    ///
    /// Used by remote access, CLI, MCP for O(1) name resolution.
    by_key: HashMap<RecordKey, RecordId>,

    /// TypeId → RecordIds lookup (introspection, optional)
    ///
    /// Enables "find all Temperature records" queries.
    /// Also used for backward-compat shim when only one record of a type exists.
    by_type: HashMap<TypeId, Vec<RecordId>>,

    /// RecordId → TypeId lookup (type safety assertions)
    ///
    /// Used to validate downcasts at runtime.
    types: Vec<TypeId>,
}
```

### Invariants

1. `storages.len() == types.len()` (always)
2. `by_key.len() == storages.len()` (all records have unique keys)
3. `∀ id ∈ by_key.values(): id.index() < storages.len()`
4. `∀ (tid, ids) ∈ by_type: ∀ id ∈ ids: types[id.index()] == tid`

### Methods

```rust
impl AimDbInner {
    /// Get storage by RecordId (hot path - O(1))
    #[inline]
    pub(crate) fn storage(&self, id: RecordId) -> &dyn AnyRecord {
        &*self.storages[id.index()]
    }

    /// Resolve RecordKey to RecordId (control plane - O(1) average)
    #[inline]
    pub fn resolve(&self, key: &RecordKey) -> Option<RecordId> {
        self.by_key.get(key).copied()
    }

    /// Resolve string to RecordId (convenience for remote access)
    pub fn resolve_str(&self, name: &str) -> Option<RecordId> {
        // Linear scan through keys - acceptable for control plane
        self.by_key.iter()
            .find(|(k, _)| k.as_str() == name)
            .map(|(_, id)| *id)
    }

    /// Get all RecordIds for a type (introspection)
    pub fn records_of_type<T: 'static>(&self) -> &[RecordId] {
        self.by_type
            .get(&TypeId::of::<T>())
            .map(|v| v.as_slice())
            .unwrap_or(&[])
    }

    /// Get typed record with safety check
    pub fn get_typed<T, R>(&self, id: RecordId) -> DbResult<&TypedRecord<T, R>>
    where
        T: Send + 'static + Debug + Clone,
        R: Spawn + 'static,
    {
        // Validate type
        let expected = TypeId::of::<T>();
        let actual = self.types.get(id.index())
            .ok_or(DbError::InvalidRecordId { id })?;
        
        if *actual != expected {
            return Err(DbError::TypeMismatch {
                expected: core::any::type_name::<T>(),
                actual_id: id,
            });
        }

        // Safe downcast (type validated above)
        self.storages[id.index()]
            .as_typed::<T, R>()
            .ok_or(DbError::InternalError { message: "downcast failed after type check" })
    }

    /// List all records with metadata
    #[cfg(feature = "std")]
    pub fn list_records(&self) -> Vec<RecordMetadata> {
        self.storages.iter()
            .enumerate()
            .map(|(i, storage)| {
                let id = RecordId::new(i as u32);
                let type_id = self.types[i];
                storage.collect_metadata(id, type_id)
            })
            .collect()
    }
}
```

---

## API Changes

### Builder API

**Before:**
```rust
builder.configure::<Temperature>(|reg| {
    reg.buffer(BufferCfg::SpmcRing { capacity: 10 })
       .source(temperature_producer)
       .tap(temperature_consumer);
});
```

**After:**
```rust
builder.configure::<Temperature>("sensors.temperature", |reg| {
    reg.buffer(BufferCfg::SpmcRing { capacity: 10 })
       .source(temperature_producer)
       .tap(temperature_consumer);
});

// Multiple instances of same type - now possible!
builder.configure::<Temperature>("sensors.outdoor", |reg| { ... });
builder.configure::<Temperature>("sensors.indoor", |reg| { ... });
```

### Method Signature Change

```rust
// Before
pub fn configure<T>(&mut self, f: impl FnOnce(&mut RecordRegistrar<T, R>)) -> &mut Self
where
    T: Send + Sync + 'static + Debug + Clone;

// After
pub fn configure<T>(
    &mut self,
    key: impl Into<RecordKey>,
    f: impl FnOnce(&mut RecordRegistrar<T, R>),
) -> &mut Self
where
    T: Send + Sync + 'static + Debug + Clone;
```

### Producer/Consumer Access

**Before:**
```rust
// Only way to get producer
let producer = db.producer::<Temperature>()?;
```

**After:**
```rust
// By key (preferred)
let producer = db.producer::<Temperature>("sensors.outdoor")?;

// By RecordId (hot path)
let id = db.resolve("sensors.outdoor")?;
let producer = db.producer_by_id::<Temperature>(id)?;

// Legacy shim: works when exactly one record of type exists
let producer = db.producer::<Temperature>()?;  // Returns error if ambiguous
```

### Remote Access (AimX Protocol)

The protocol already uses string names. Changes are internal only:

```json
{
  "method": "record.get",
  "params": { "name": "sensors.outdoor" }
}
```

Internal handler changes from O(n) scan to O(1) lookup:

```rust
// Before
let type_id_opt = db.inner().records.iter().find_map(|(tid, record)| {
    let metadata = record.collect_metadata(*tid);
    if metadata.name == record_name { Some(*tid) } else { None }
});

// After
let id = db.inner().resolve_str(&record_name)
    .ok_or_else(|| Response::error(request_id, "not_found", ...))?;
```

### RecordMetadata Updates

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecordMetadata {
    /// Record key (stable identifier)
    pub key: RecordKey,
    
    /// Internal record ID (for debugging)
    pub id: RecordId,
    
    /// Rust type name (for debugging/introspection)
    pub type_name: String,
    
    /// TypeId as hex string (for debugging only - not stable!)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub type_id_debug: Option<String>,
    
    // ... rest unchanged
    pub buffer_type: String,
    pub buffer_capacity: Option<usize>,
    pub producer_count: usize,
    pub consumer_count: usize,
    pub writable: bool,
    pub created_at: String,
    pub last_update: Option<String>,
    pub outbound_connector_count: usize,
}
```

---

## Type Safety

### Compile-Time Safety

Type `T` is still a generic parameter on `configure<T>()`, ensuring:
- Buffer stores values of type `T`
- Producers produce type `T`
- Consumers consume type `T`
- Serializers serialize type `T`

### Runtime Safety

When accessing by `RecordId`, we validate the `TypeId`:

```rust
pub fn producer_by_id<T>(&self, id: RecordId) -> DbResult<Producer<T, R>> {
    // 1. Validate RecordId in bounds
    if id.index() >= self.inner.storages.len() {
        return Err(DbError::InvalidRecordId { id });
    }
    
    // 2. Validate TypeId matches
    let expected = TypeId::of::<T>();
    let actual = self.inner.types[id.index()];
    if expected != actual {
        return Err(DbError::TypeMismatch { ... });
    }
    
    // 3. Safe to downcast
    ...
}
```

### Error Types

```rust
pub enum DbError {
    // Existing errors...
    
    /// RecordKey not found in registry
    RecordNotFound { key: RecordKey },
    
    /// RecordId out of bounds or invalid
    InvalidRecordId { id: RecordId },
    
    /// Type mismatch when accessing record by ID
    TypeMismatch {
        expected: &'static str,  // type_name::<T>()
        actual_id: RecordId,
    },
    
    /// Multiple records of same type exist (legacy API ambiguity)
    AmbiguousType {
        type_name: &'static str,
        record_ids: Vec<RecordId>,
    },
}
```

---

## Migration Guide

### Example Migration

**tokio-mqtt-connector-demo/src/main.rs**

```rust
// Before
builder.configure::<Temperature>(|reg| {
    reg.buffer(BufferCfg::SpmcRing { capacity: 10 })
        .source(temperature_producer)
        .tap(temperature_consumer)
        .link_to("mqtt://sensors/temperature")
        ...
});

builder.configure::<TemperatureCommand>(|reg| {
    reg.buffer(BufferCfg::SpmcRing { capacity: 10 })
        .tap(command_consumer)
        .link_from("mqtt://commands/temperature")
        ...
});

// After
builder.configure::<Temperature>("sensors.temperature", |reg| {
    reg.buffer(BufferCfg::SpmcRing { capacity: 10 })
        .source(temperature_producer)
        .tap(temperature_consumer)
        .link_to("mqtt://sensors/temperature")
        ...
});

builder.configure::<TemperatureCommand>("commands.temperature", |reg| {
    reg.buffer(BufferCfg::SpmcRing { capacity: 10 })
        .tap(command_consumer)
        .link_from("mqtt://commands/temperature")
        ...
});
```

### Files Requiring Updates

| File | Change Type | Complexity |
|------|-------------|------------|
| `aimdb-core/src/builder.rs` | Core refactor | High |
| `aimdb-core/src/typed_record.rs` | Metadata updates | Medium |
| `aimdb-core/src/error.rs` | New error variants | Low |
| `aimdb-core/src/remote/handler.rs` | Lookup simplification | Medium |
| `aimdb-core/src/remote/metadata.rs` | Add RecordKey field | Low |
| `aimdb-core/src/remote/config.rs` | Writable records by key | Medium |
| `aimdb-sync/src/handle.rs` | API updates | Medium |
| `aimdb-tokio-adapter/src/lib.rs` | Extension trait updates | Low |
| `aimdb-embassy-adapter/src/lib.rs` | Extension trait updates | Low |
| `tools/aimdb-mcp/src/tools/*.rs` | Use new lookup APIs | Low |
| `examples/*/src/main.rs` | Add record keys | Low |

---

## Performance Analysis

### Hot Path (Produce/Consume)

| Operation | Before | After |
|-----------|--------|-------|
| Get storage | `BTreeMap::get(&TypeId)` O(log n) | `Vec::index(RecordId)` O(1) |
| Type check | None (trusted) | `Vec::index(RecordId)` + compare O(1) |

**Net effect:** Faster (Vec index vs BTreeMap lookup)

### Control Plane (Remote Access)

| Operation | Before | After |
|-----------|--------|-------|
| Find by name | `iter().find_map()` O(n) | `HashMap::get()` O(1) avg |
| List records | `iter().map()` O(n) | `iter().map()` O(n) |

**Net effect:** Significantly faster for lookups

### Memory

| Before | After |
|--------|-------|
| `BTreeMap<TypeId, Box<dyn AnyRecord>>` | `Vec<Box<dyn AnyRecord>>` + `HashMap<RecordKey, RecordId>` + `HashMap<TypeId, Vec<RecordId>>` + `Vec<TypeId>` |

**Net effect:** Slightly more memory for the extra indexes, but:
- `RecordId` is 4 bytes vs `TypeId` is 8 bytes
- HashMap has better cache locality than BTreeMap
- Worth it for O(1) lookups

---

## Implementation Plan

### Phase 1: Core Types (PR #1)

1. Add `RecordKey` type to `aimdb-core/src/lib.rs`
2. Add `RecordId` type to `aimdb-core/src/lib.rs`
3. Add new error variants
4. Unit tests for `RecordKey` and `RecordId`

### Phase 2: Registry Refactor (PR #2)

1. Replace `AimDbInner` storage structure
2. Update `AimDbBuilder::configure()` signature
3. Update spawn function storage
4. Update all internal lookups

### Phase 3: API Updates (PR #3)

1. Update `producer()` / `consumer()` methods
2. Add `producer_by_id()` / `consumer_by_id()` methods
3. Update extension traits in adapters
4. Update `RecordMetadata` structure

### Phase 4: Remote Access (PR #4)

1. Simplify handler lookups to use `resolve_str()`
2. Update writable records tracking to use `RecordKey`
3. Update MCP tools

### Phase 5: Examples & Tests (PR #5)

1. Update all examples with record keys
2. Add multi-instance tests
3. Update documentation

### Phase 6: Cleanup (PR #6)

1. Remove deprecated code paths
2. Final documentation pass
3. CHANGELOG update

---

## Testing Strategy

### Unit Tests

```rust
#[test]
fn test_multiple_records_same_type() {
    let mut builder = AimDbBuilder::new().runtime(runtime);
    
    builder.configure::<Temperature>("sensors.outdoor", |reg| {
        reg.buffer(BufferCfg::SingleLatest);
    });
    
    builder.configure::<Temperature>("sensors.indoor", |reg| {
        reg.buffer(BufferCfg::SingleLatest);
    });
    
    let db = builder.build().await.unwrap();
    
    // Both records exist
    assert!(db.resolve("sensors.outdoor").is_some());
    assert!(db.resolve("sensors.indoor").is_some());
    
    // Different RecordIds
    let id1 = db.resolve("sensors.outdoor").unwrap();
    let id2 = db.resolve("sensors.indoor").unwrap();
    assert_ne!(id1, id2);
    
    // Both are Temperature type
    let ids = db.inner().records_of_type::<Temperature>();
    assert_eq!(ids.len(), 2);
}

#[test]
fn test_type_mismatch_panics() {
    let db = /* ... */;
    let temp_id = db.resolve("sensors.temperature").unwrap();
    
    // Wrong type should error
    let result = db.producer_by_id::<Command>(temp_id);
    assert!(matches!(result, Err(DbError::TypeMismatch { .. })));
}

#[test]
fn test_duplicate_key_rejected() {
    let mut builder = AimDbBuilder::new().runtime(runtime);
    
    builder.configure::<Temperature>("sensors.temp", |reg| { ... });
    
    // Second registration with same key should fail
    let result = std::panic::catch_unwind(|| {
        builder.configure::<Humidity>("sensors.temp", |reg| { ... });
    });
    assert!(result.is_err());
}
```

### Integration Tests

1. **Multi-instance data flow** - Produce to one record, verify other is unaffected
2. **Remote access by name** - Verify O(1) lookup behavior
3. **MCP introspection** - List records shows all instances with unique keys
4. **Embedded cross-compile** - Verify no_std compatibility

---

## Future Extensions

### 1. Hierarchical Queries (Post-MVP)

```rust
// List all sensor records
let sensors = db.list_records_matching("sensors.*");

// List all tenant records  
let tenant_records = db.list_records_matching("tenant.acme.*");
```

### 2. Dynamic Registration (Post-MVP)

```rust
// Runtime sensor discovery
let id = db.register_dynamic::<Temperature>(
    RecordKey::dynamic(format!("sensors.{}", sensor_id)),
    BufferCfg::SingleLatest,
)?;
```

### 3. Record Aliases (Future)

```rust
// Alias for backward compatibility
db.alias("temperature", "sensors.temperature.primary")?;
```

### 4. Record Groups (Future)

```rust
// Subscribe to all temperature records
let subscription = db.subscribe_group::<Temperature>()?;
```

---

## Appendix A: RecordKey vs String Alternatives Considered

| Option | Pros | Cons | Decision |
|--------|------|------|----------|
| Plain `String` | Simple | Heap alloc on every lookup | Rejected |
| `&'static str` only | Zero alloc | No dynamic names | Rejected |
| Type-level names | Compile-time safe | Verbose, no dynamic | Rejected |
| Enum IDs | Exhaustive | Closed set | Rejected |
| **Hybrid RecordKey** | Best of both | Slight complexity | **Chosen** |

---

## Appendix B: Compatibility Matrix

| Component | Compatible | Notes |
|-----------|------------|-------|
| Tokio adapter | ✅ | Full support |
| Embassy adapter | ✅ | Requires `alloc` (already required) |
| MQTT connector | ✅ | No changes needed |
| KNX connector | ✅ | No changes needed |
| MCP server | ✅ | Minor updates to use new APIs |
| Sync API | ✅ | API signature changes |
| CLI tools | ✅ | Uses string names (already compatible) |

---

## References

- [GitHub Issue: Replace TypeId Storage Keys](link-to-issue)
- [Design Doc 008: Remote Access Protocol](./008-M3-remote-access.md)
- [Design Doc 009: MCP Integration](./009-M4-mcp-integration.md)
