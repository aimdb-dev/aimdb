# Design: Trait-Based Record Keys

**Status**: Proposed  
**Author**: Architecture Discussion  
**Date**: 2024-12-23

## Summary

Rework the existing `RecordKey` struct into a `RecordKey` trait to make AimDB generic over key types, enabling compile-time checked enum keys for embedded while preserving `String` flexibility for edge/cloud.

## Motivation

Current API uses `String` keys:
```rust
db.producer::<Temperature>("sensor.temp.indoor")
```

**Limitations:**
- Runtime-only typo detection (`"sensor.temprature"` fails at runtime)
- String allocation on every key (minor cost on std, overhead on embedded)
- No exhaustiveness checking for record handling

**Goal:** Support user-defined enum keys without losing String flexibility.

## Current State

The codebase has an existing `RecordKey` **struct** in `aimdb-core/src/record_id.rs`:

```rust
pub struct RecordKey(RecordKeyInner);

enum RecordKeyInner {
    Static(&'static str),   // Zero allocation
    Dynamic(Arc<str>),      // Runtime strings
}
```

This struct already provides `Hash`, `Eq`, `Borrow<str>` for O(1) HashMap lookups. We will:
1. **Convert this struct to a trait** (breaking change accepted)
2. **Provide a `StringKey` struct** as the default implementation (replaces the old struct)
3. **Enable user-defined enum keys** for compile-time safety

## Design

### Core Trait

```rust
// aimdb-core/src/record_id.rs (replaces existing RecordKey struct)

/// Trait for record key types
///
/// Enables compile-time checked enum keys for embedded while preserving
/// String flexibility for edge/cloud deployments.
///
/// The `Borrow<str>` bound is required for O(1) HashMap lookups by string
/// in the remote access layer (e.g., `hashmap.get("record.name")`).
pub trait RecordKey: Clone + Eq + core::hash::Hash + core::borrow::Borrow<str> + Send + Sync + 'static {
    /// String representation for connectors, logging, serialization, remote access
    fn as_str(&self) -> &str;
}

// Blanket implementations
impl RecordKey for &'static str {
    fn as_str(&self) -> &str { self }
}

#[cfg(feature = "alloc")]
impl RecordKey for alloc::string::String {
    fn as_str(&self) -> &str { self }
}
```

### StringKey - Default Implementation

```rust
// aimdb-core/src/record_id.rs

/// Default string-based record key (replaces old RecordKey struct)
///
/// Supports both static (zero-cost) and dynamic (Arc-allocated) names.
/// This is the default key type when no generic is specified.
#[derive(Clone)]
pub struct StringKey(StringKeyInner);

#[derive(Clone)]
enum StringKeyInner {
    Static(&'static str),
    Dynamic(Arc<str>),
}

impl RecordKey for StringKey {
    fn as_str(&self) -> &str {
        match &self.0 {
            StringKeyInner::Static(s) => s,
            StringKeyInner::Dynamic(s) => s,
        }
    }
}

// Ergonomic conversions (preserved from old API)
impl From<&'static str> for StringKey { ... }
impl From<String> for StringKey { ... }
impl Borrow<str> for StringKey { ... }  // Enables HashMap::get("literal")
```

### Generic Database

```rust
pub struct AimDb<R, K = StringKey>
where
    R: Spawn + 'static,
    K: RecordKey,
{
    inner: Arc<AimDbInner<K>>,
    runtime: Arc<R>,
}

pub struct AimDbInner<K: RecordKey> {
    by_key: hashbrown::HashMap<K, RecordId>,  // no_std compatible
    by_type: hashbrown::HashMap<TypeId, Vec<RecordId>>,
    storages: Vec<Box<dyn AnyRecord>>,
    types: Vec<TypeId>,
    keys: Vec<K>,  // Now generic!
}
```

### Generic Producer/Consumer

```rust
pub struct Producer<T, R, K = StringKey>
where
    R: Spawn + 'static,
    K: RecordKey,
{
    db: Arc<AimDb<R, K>>,
    key: K,
    _phantom: PhantomData<T>,
}

pub struct Consumer<T, R, K = StringKey>
where
    R: Spawn + 'static,
    K: RecordKey,
{
    db: Arc<AimDb<R, K>>,
    key: K,
    _phantom: PhantomData<T>,
}
```

### Generic RecordRegistrar

```rust
pub struct RecordRegistrar<'a, T, R, K = StringKey>
where
    T: Send + Sync + 'static + Debug + Clone,
    R: Spawn + 'static,
    K: RecordKey,
{
    pub(crate) rec: &'a mut TypedRecord<T, R>,
    pub(crate) connector_builders: &'a [Box<dyn ConnectorBuilder<R>>],
    pub(crate) record_key: K,  // Was: String
}
```

### User-Defined Enum Keys

With the derive macro (recommended):

```rust
use aimdb_core::RecordKey;

#[derive(RecordKey, Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum AppKey {
    #[key = "temp.indoor"]
    TempIndoor,
    #[key = "temp.outdoor"]
    TempOutdoor,
    #[key = "light.main"]
    LightMain,
}
```

Manual implementation (if derive macro is not available):

```rust
// User's application code
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum AppKey {
    TempIndoor,
    TempOutdoor,
    LightMain,
}

impl RecordKey for AppKey {
    fn as_str(&self) -> &str {
        match self {
            Self::TempIndoor => "temp.indoor",
            Self::TempOutdoor => "temp.outdoor",
            Self::LightMain => "light.main",
        }
    }
}

// Required by RecordKey trait bound for O(1) HashMap lookups
impl core::borrow::Borrow<str> for AppKey {
    fn borrow(&self) -> &str {
        self.as_str()
    }
}
```

## Usage Examples

### Edge/Cloud (StringKey - default, API unchanged)

```rust
// Type inference gives AimDbBuilder<TokioAdapter, StringKey>
let mut builder = AimDbBuilder::new().runtime(runtime);

// String literals auto-convert to StringKey via From<&'static str>
builder.configure::<Temperature>("sensor.temp", |reg| {
    reg.buffer(BufferCfg::SpmcRing { capacity: 10 })
       .link_to("mqtt://sensors/temp")
       .finish();
});

let db = builder.build()?;
let producer = db.producer::<Temperature>("sensor.temp");
```

### Embedded (Enum keys - compile-time safe)

```rust
// Explicitly specify AppKey as the key type
let mut builder = AimDbBuilder::<EmbassyAdapter, AppKey>::new()
    .runtime(runtime);

builder.configure::<Temperature>(AppKey::TempIndoor, |reg| {
    reg.buffer_sized::<10, 2>(BufferType::SpmcRing)
       .link_to("mqtt://sensors/temp/indoor")
       .finish();
});

let db = builder.build()?;
let producer = db.producer::<Temperature>(AppKey::TempIndoor);

// Compile error - typo caught at build time!
// let bad = db.producer::<Temperature>(AppKey::TempIndor);
```

## Remote Access & Protocol Consideration

The AimX protocol and MCP tools communicate via JSON with string-based record names.
This is **intentional** — external protocols always use strings regardless of internal key type:

```rust
// Remote access handler uses as_str() for JSON serialization
impl<K: RecordKey> AimDbInner<K> {
    pub fn list_records(&self) -> Vec<RecordMetadata> {
        self.keys.iter().map(|k| RecordMetadata {
            name: k.as_str().to_string(),  // Always serialized as string
            ...
        }).collect()
    }
    
    // Lookup by string for remote access (O(1) via Borrow<str>)
    pub fn resolve_str(&self, name: &str) -> Option<RecordId> {
        self.by_key.get(name).copied()  // Works because K: Borrow<str>
    }
}
```

**Important:** For `resolve_str()` to work with custom key types, they must implement `Borrow<str>`:

```rust
impl Borrow<str> for AppKey {
    fn borrow(&self) -> &str {
        self.as_str()
    }
}
```

## Connector Integration

Connectors receive `&str` via `RecordKey::as_str()`:

```rust
impl<'a, T, R, K: RecordKey> OutboundConnectorBuilder<'a, T, R, K> {
    pub fn finish(self) -> &'a mut RecordRegistrar<'a, T, R, K> {
        let key_str = self.registrar.record_key.as_str();
        // Register with connector using key_str for topic mapping
        ...
    }
}
```

## Migration Strategy

### Phase 1: Rework RecordKey (Breaking Change)
- Convert `RecordKey` struct → `RecordKey` trait in `record_id.rs`
- Add `Borrow<str>` to trait bounds for O(1) string lookups
- Rename old struct to `StringKey` (implements the trait)
- Add blanket impls for `String` and `&'static str`
- Update `AimDbInner` to be generic: `AimDbInner<K: RecordKey>`

### Phase 2: Create Derive Macro Crate
- Create `aimdb-derive/` proc-macro crate
- Implement `#[derive(RecordKey)]` with `#[key = "..."]` attribute
- Add `#[key_prefix = "..."]` enum-level attribute
- Re-export from `aimdb-core` via `derive` feature

### Phase 2: Update Core Types
- `AimDb<R>` → `AimDb<R, K = StringKey>`
- `AimDbBuilder<R>` → `AimDbBuilder<R, K = StringKey>`
- `Producer<T, R>` → `Producer<T, R, K = StringKey>`
- `Consumer<T, R>` → `Consumer<T, R, K = StringKey>`
- `RecordRegistrar<'a, T, R>` → `RecordRegistrar<'a, T, R, K = StringKey>`

### Phase 3: Update Adapters & Macros
- Update `impl_record_registrar_ext!` macro to propagate `K` generic
- `TokioRecordRegistrarExt<'a, T>` → `TokioRecordRegistrarExt<'a, T, K>`
- `EmbassyRecordRegistrarExt<'a, T>` → `EmbassyRecordRegistrarExt<'a, T, K>`

### Phase 4: Documentation & Examples
- Add enum key example for embedded
- Document `RecordKey` impl pattern with `Borrow<str>`
- Update CHANGELOG with breaking changes

## Files to Modify

| File | Change |
|------|--------|
| `aimdb-derive/` | **New crate**: Proc-macro for `#[derive(RecordKey)]` |
| `aimdb-core/src/record_id.rs` | Convert `RecordKey` struct → trait, add `StringKey` struct |
| `aimdb-core/src/lib.rs` | Export `RecordKey` trait and `StringKey` struct, re-export derive |
| `aimdb-core/src/builder.rs` | Add `K: RecordKey` generic to `AimDb`, `AimDbInner`, `AimDbBuilder` |
| `aimdb-core/src/typed_api.rs` | Add `K: RecordKey` generic to `Producer`, `Consumer`, `RecordRegistrar` |
| `aimdb-core/src/typed_record.rs` | Update internal key storage from `String` to generic `K` |
| `aimdb-core/src/database.rs` | Add `K: RecordKey` generic to `Database` wrapper |
| `aimdb-core/src/connector.rs` | Update `OutboundConnectorBuilder`, `InboundConnectorBuilder` with `K` |
| `aimdb-core/src/ext_macros.rs` | Update macro to propagate `K` generic through generated traits |
| `aimdb-core/src/remote/handler.rs` | Use `as_str()` for string serialization |
| `aimdb-core/src/remote/metadata.rs` | Ensure metadata uses string representation |
| `aimdb-tokio-adapter/src/lib.rs` | Update extension trait with `K` generic |
| `aimdb-embassy-adapter/src/lib.rs` | Update extension trait with `K` generic |
| `aimdb-sync/src/lib.rs` | **Keep `StringKey` only** (no generic, simpler API) |
| `aimdb-client/src/lib.rs` | No change (uses strings for protocol) |
| `tools/aimdb-mcp/src/*` | No change (uses strings for protocol) |

## Sync API Decision

The sync API (`aimdb-sync`) will **remain `StringKey`-only** for simplicity:

```rust
// aimdb-sync/src/lib.rs - NO K generic
pub struct AimDbHandle {
    inner: Arc<AimDb<TokioAdapter, StringKey>>,  // Fixed to StringKey
    ...
}

impl AimDbHandle {
    pub fn producer<T>(&self, key: impl Into<StringKey>) -> DbResult<SyncProducer<T>> { ... }
    pub fn consumer<T>(&self, key: impl Into<StringKey>) -> DbResult<SyncConsumer<T>> { ... }
}
```

**Rationale:**
- Sync API is primarily for FFI, legacy code, simple scripts (edge/cloud use cases)
- These environments benefit from dynamic string keys, not compile-time enum safety
- Embedded targets use Embassy async, not the sync wrapper
- Keeps the sync API simple without propagating generics

## Trade-offs

| Aspect | String Keys | Enum Keys |
|--------|-------------|-----------|
| Compile-time safety | ❌ Runtime errors | ✅ Compile errors |
| Flexibility | ✅ Dynamic/config-driven | ❌ Static |
| Memory | Heap allocation | Zero-copy (`Copy` enums) |
| Exhaustiveness | ❌ No `match` checking | ✅ `match` is exhaustive |

## Alternatives Considered

### 1. `&'static str` only
- Pros: Zero allocation for literals
- Cons: Still no compile-time typo checking

### 2. Trait-only (no derive macro)
- Pros: No proc-macro dependency
- Cons: Boilerplate for users implementing enum keys manually
- **Decision**: Include derive macro to reduce friction for embedded users

### 3. Keep String only
- Pros: Simplest API
- Cons: No path to compile-time safety for embedded

## Decision

The trait-based approach with `K = StringKey` default provides:
- **Breaking change accepted** - rework existing `RecordKey` struct to trait
- **Opt-in compile-time safety** for embedded via enum keys
- **Single HashMap implementation** (hashbrown) across all platforms
- **Natural connector integration** via `as_str()`
- **Sync API simplicity** - fixed to `StringKey`, no generics

## Breaking Changes Summary

| Change | Migration |
|--------|-----------|
| `RecordKey` struct → trait | Replace `RecordKey` with `StringKey` in type annotations |
| `RecordKey::new("...")` | Use `StringKey::new("...")` or `"...".into()` |
| `RecordKey::dynamic(s)` | Use `StringKey::dynamic(s)` |
| `keys: Vec<RecordKey>` | Use `keys: Vec<StringKey>` or make generic |

## Derive Macro Design

A proc-macro crate `aimdb-derive` provides `#[derive(RecordKey)]` to eliminate boilerplate:

### Crate Structure

```
aimdb-derive/
├── Cargo.toml
└── src/
    └── lib.rs
```

### Generated Code

For this input:

```rust
#[derive(RecordKey, Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum AppKey {
    #[key = "temp.indoor"]
    TempIndoor,
    #[key = "temp.outdoor"]
    TempOutdoor,
    #[key = "light.main"]
    LightMain,
}
```

The macro generates:

```rust
impl aimdb_core::RecordKey for AppKey {
    fn as_str(&self) -> &str {
        match self {
            Self::TempIndoor => "temp.indoor",
            Self::TempOutdoor => "temp.outdoor",
            Self::LightMain => "light.main",
        }
    }
}

impl core::borrow::Borrow<str> for AppKey {
    fn borrow(&self) -> &str {
        <Self as aimdb_core::RecordKey>::as_str(self)
    }
}
```

### Macro Attributes

| Attribute | Location | Description |
|-----------|----------|-------------|
| `#[key = "..."]` | Variant | **Required.** The string key for this variant |
| `#[key_prefix = "..."]` | Enum | Optional. Prefix prepended to all variant keys |

### Example with Prefix

```rust
#[derive(RecordKey, Clone, Copy, PartialEq, Eq, Hash)]
#[key_prefix = "sensors."]
pub enum SensorKey {
    #[key = "temp.indoor"]   // → "sensors.temp.indoor"
    TempIndoor,
    #[key = "temp.outdoor"]  // → "sensors.temp.outdoor"
    TempOutdoor,
}
```

### Error Handling

The macro emits compile errors for:
- Missing `#[key = "..."]` attribute on any variant
- Non-string literal in `#[key = ...]`
- Applied to non-enum types (structs, unions)
- Duplicate key strings across variants

### no_std Compatibility

The derive macro is fully `no_std` compatible:
- Uses `core::borrow::Borrow` (not `std::borrow`)
- No heap allocation in generated code
- Works with Embassy on embedded targets

### Re-export Strategy

`aimdb-core` re-exports the derive macro when the `derive` feature is enabled:

```toml
# aimdb-core/Cargo.toml
[features]
default = ["derive"]
derive = ["aimdb-derive"]

[dependencies]
aimdb-derive = { version = "0.1", optional = true }
```

Users import via:
```rust
use aimdb_core::RecordKey;  // Both trait and derive macro
```

## Future Enhancements

1. **FromStr impl**: Parse keys from config files (generate via derive macro)
2. **Const key count**: Enable fixed-size array storage for extreme embedded optimization
3. **Variant iteration**: Generate `AppKey::all() -> &'static [AppKey]` for introspection
4. **Serde integration**: Optional `#[key_serde]` to derive `Serialize`/`Deserialize` using key strings
