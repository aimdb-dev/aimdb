# Design: Trait-Based Record Keys

**Status**: Proposed  
**Author**: Architecture Discussion  
**Date**: 2024-12-23

## Summary

Introduce a `RecordKey` trait to make AimDB generic over key types, enabling compile-time checked enum keys for embedded while preserving `String` flexibility for edge/cloud.

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

## Design

### Core Trait

```rust
// aimdb-core/src/record_key.rs

pub trait RecordKey: Clone + Eq + core::hash::Hash + Send + Sync + 'static {
    /// String representation for connectors, logging, serialization
    fn as_str(&self) -> &str;
}

// Blanket implementations
#[cfg(feature = "alloc")]
impl RecordKey for alloc::string::String {
    fn as_str(&self) -> &str { self }
}

impl RecordKey for &'static str {
    fn as_str(&self) -> &str { self }
}
```

### Generic Database

```rust
pub struct AimDb<R, K = String>
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
    records: Vec<Box<dyn Any + Send + Sync>>,
}
```

### Generic Producer/Consumer

```rust
pub struct Producer<T, R, K = String>
where
    R: Spawn + 'static,
    K: RecordKey,
{
    db: Arc<AimDb<R, K>>,
    key: K,
    _phantom: PhantomData<T>,
}

pub struct Consumer<T, R, K = String>
where
    R: Spawn + 'static,
    K: RecordKey,
{
    db: Arc<AimDb<R, K>>,
    key: K,
    _phantom: PhantomData<T>,
}
```

### User-Defined Enum Keys

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
```

## Usage Examples

### Edge/Cloud (String keys - unchanged)

```rust
let mut builder = AimDbBuilder::new().runtime(runtime);

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

## Connector Integration

Connectors receive `&str` via `RecordKey::as_str()`:

```rust
impl<T, R, K: RecordKey> OutboundLinkBuilder<'_, T, R, K> {
    pub fn link_to(self, url: &str) -> Self {
        let key_str = self.registrar.key.as_str();
        // Register with connector using key_str for topic mapping
    }
}
```

## Migration Strategy

### Phase 1: Add trait, default to String
- Add `RecordKey` trait with blanket impls
- Make `AimDb<R, K = String>` generic with default
- **Zero breaking changes** - existing code compiles unchanged

### Phase 2: Update adapters
- `TokioRecordRegistrarExt` becomes `TokioRecordRegistrarExt<K: RecordKey>`
- `EmbassyRecordRegistrarExt` becomes `EmbassyRecordRegistrarExt<K: RecordKey>`

### Phase 3: Documentation & examples
- Add enum key example for embedded
- Document `RecordKey` impl pattern

## Files to Modify

| File | Change |
|------|--------|
| `aimdb-core/src/record_key.rs` | New file - trait definition |
| `aimdb-core/src/lib.rs` | Export `RecordKey` trait |
| `aimdb-core/src/builder.rs` | Add `K: RecordKey` generic to `AimDb`, `AimDbInner`, `AimDbBuilder` |
| `aimdb-core/src/typed_api.rs` | Add `K: RecordKey` generic to `Producer`, `Consumer`, `RecordRegistrar` |
| `aimdb-core/src/database.rs` | Add `K: RecordKey` generic to `Database` wrapper |
| `aimdb-tokio-adapter/src/lib.rs` | Update extension trait bounds |
| `aimdb-embassy-adapter/src/lib.rs` | Update extension trait bounds |
| `aimdb-sync/src/lib.rs` | Add `K: RecordKey` generic (or keep String-only for simplicity) |

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

### 2. Macro-generated keys
- Pros: Less boilerplate for users
- Cons: Adds proc-macro dependency, complexity

### 3. Keep String only
- Pros: Simplest API
- Cons: No path to compile-time safety for embedded

## Decision

The trait-based approach with `K = String` default provides:
- **Zero breaking changes** for existing users
- **Opt-in compile-time safety** for embedded
- **Single HashMap implementation** (hashbrown) across all platforms
- **Natural connector integration** via `as_str()`

## Future Enhancements

1. **Derive macro**: `#[derive(RecordKey)]` for enum boilerplate
2. **FromStr impl**: Parse keys from config files
3. **Const key count**: Enable fixed-size array storage for extreme embedded optimization
