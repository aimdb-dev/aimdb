# Design: Context-Aware Deserializers

**Status:** 📋 Planned  
**Author:** GitHub Copilot  
**Date:** 2026-04-08  
**Issue:** [#56 — Context-aware deserializers](https://github.com/aimdb-dev/aimdb/issues/56)  
**Target:** Both `std` and `no_std` (with `alloc`) environments

---

## Table of Contents

- [Problem Statement](#problem-statement)
- [Goals](#goals)
- [Non-Goals](#non-goals)
- [Current Architecture](#current-architecture)
- [Design Constraints](#design-constraints)
- [Proposed Solution](#proposed-solution)
- [API Design](#api-design)
- [Implementation](#implementation)
- [Alternatives Considered](#alternatives-considered)
- [Migration & Backward Compatibility](#migration--backward-compatibility)
- [Testing Strategy](#testing-strategy)
- [Implementation Plan](#implementation-plan)

---

## Problem Statement

Connector deserializers currently receive only raw bytes (`&[u8]`), with no access to `RuntimeContext`. This prevents platform-independent timestamping, diagnostic logging, and consistent cross-runtime behavior during deserialization.

Today:

```rust
.link_from("knx://gateway/9/1/0")
    .with_deserializer(|data: &[u8]| {
        records::temperature::knx::from_knx(data, "9/1/0")
        // ❌ Cannot timestamp with ctx.time().now()
        // ❌ Cannot log deserialization diagnostics
    })
    .finish();
```

Monitors (`.tap()`) already receive `RuntimeContext<R>` via the `extract_from_any` pattern in `ext_macros.rs`, but deserializers are excluded from this capability. Users must work around this by:

1. **Setting timestamps in monitors** — complicates data flow; timestamp reflects processing time, not receive time
2. **Using `Debug` on `Instant`** — hacky, platform-dependent
3. **Restructuring architectures** — adding unnecessary monitors just to inject context

These workarounds violate AimDB's principle of clean, declarative data pipelines.

## Goals

1. **Context access in deserializers** — Provide `RuntimeContext` to deserialization closures for timestamps, logging, and runtime services
2. **Consistent `tap` / `tap_raw` convention** — `.with_deserializer()` injects context by default; `.with_deserializer_raw()` is the bytes-only escape hatch
3. **no_std compatible** — Works with both `std` and `no_std + alloc` environments
4. **All runtimes** — Consistent API across Tokio, Embassy, and WASM adapters
5. **Minimal core changes** — Follow the established type-erasure patterns already used by `DeserializerFn` and `TopicProvider`

## Non-Goals

- Backward compatibility for `.with_deserializer()` call sites (this is an intentional breaking change; existing callers migrate to `|ctx, data|` or rename to `.with_deserializer_raw()`)
- Providing mutable database access from within deserializers
- Async deserializers (deserialization should remain synchronous)
- Providing context to serializers (outbound direction — separate concern)

## Current Architecture

### Deserializer Type Chain

```
User closure                   Typed alias                    Type-erased storage
─────────────                  ───────────                    ───────────────────
|data: &[u8]| -> Result<T>    TypedDeserializerFn<T>         DeserializerFn
                               Arc<dyn Fn(&[u8])             Arc<dyn Fn(&[u8])
                                 -> Result<T, String>>          -> Result<Box<dyn Any>, String>>
```

**Defined in:**
- `TypedDeserializerFn<T>` — `aimdb-core/src/typed_api.rs` (typed, generic)
- `DeserializerFn` — `aimdb-core/src/connector.rs` (type-erased, stored in `InboundConnectorLink`)

### Data Flow (Inbound)

```
External System (MQTT/KNX/...)
        │
        ▼
  Connector Event Loop          ← receives raw bytes
        │
        ▼
  Router::route(topic, &[u8])   ← dispatches to matching routes
        │
        ▼
  (route.deserializer)(payload) ← ⭐ INJECTION POINT — no context available
        │
        ▼
  route.producer.produce_any()  ← pushes typed value into buffer
        │
        ▼
  Buffer → Consumers / Monitors
```

### How Monitors Get Context (Precedent)

The `.tap()` API already injects `RuntimeContext<R>` via a type-erasure boundary:

```rust
// ext_macros.rs — generated per runtime adapter
fn tap<F, Fut>(&'a mut self, f: F) -> ...
where
    F: FnOnce(RuntimeContext<R>, Consumer<T, R>) -> Fut + Send + 'static,
{
    self.tap_raw(|consumer, ctx_any| {
        let ctx = RuntimeContext::extract_from_any(ctx_any);  // downcast Arc<dyn Any> → R
        f(ctx, consumer)
    })
}
```

The new deserializer API should follow the same `extract_from_any` pattern.

## Design Constraints

1. **`DeserializerFn` is `Fn`, not `async`** — deserialization must remain synchronous. `RuntimeContext::time().now()` and `RuntimeContext::log()` are both synchronous, so this is fine.

2. **`DeserializerFn` is stored in `InboundConnectorLink`** — created during configuration, before the runtime `Arc<dyn Any>` context is available. Context must be injected later, at route construction or dispatch time.

3. **`Router::route()` is runtime-agnostic** — the router has no generic `R` parameter. Context must flow in as `Arc<dyn Any + Send + Sync>`, matching the existing erasure pattern.

4. **no_std: `Arc` comes from `alloc`** — the same approach as `DeserializerFn` and `TopicProviderAny`.

5. **WASM is `no_std + alloc` with `Arc`** — the WASM adapter uses `Arc<WasmAdapter>` (like Tokio), not `&'static` (like Embassy). Since WASM is single-threaded, `Send + Sync` are trivially satisfied via unsafe impl.

## Proposed Solution

### Overview

Follow the established `tap` / `tap_raw` convention: make `.with_deserializer()` context-aware by default, and add `.with_deserializer_raw()` as the low-level escape hatch that receives only bytes.

At the core level, introduce `ContextDeserializerFn` — a type-erased callback that accepts `(Arc<dyn Any + Send + Sync>, &[u8])`. This becomes the **primary** deserializer stored in `InboundConnectorLink`. The existing `DeserializerFn` (bytes-only) is retained as `with_deserializer_raw()` for cases where context is unnecessary.

Unlike `.tap()` (which requires macro generation because `RecordRegistrar` has no generic `R`), `InboundConnectorBuilder<'a, T, R>` already carries the runtime generic `R`. This means `.with_deserializer()` can be implemented **directly in `typed_api.rs`** with an `R: Runtime` bound — no `ext_macros.rs` changes needed. The method wraps the user's `|ctx, data|` closure with `RuntimeContext::extract_from_any`, reusing the existing downcast mechanism.

### Why This Approach

- **Mirrors `tap` / `tap_raw` exactly** — context injection is the default; raw is the escape hatch
- **Consistent API surface** — users learn one pattern for all context-injected APIs
- **No generic `R` on Router** — context stays type-erased at the router level
- **No macro generation needed** — `InboundConnectorBuilder` already has `R`, so `.with_deserializer()` lives in `typed_api.rs` directly
- **Synchronous** — `now()` and `log()` don't require async
- **Reuses `extract_from_any`** — no new downcast method needed; Arc clone cost (one atomic increment) is negligible vs. deserialization work

## API Design

### User-Facing API

```rust
// DEFAULT: Context-aware deserializer (mirrors .tap())
builder.configure::<Temperature>(SensorKey::Outdoor, |reg| {
    reg.buffer(BufferCfg::SingleLatest)
       .link_from("knx://gateway/9/1/0")
           .with_deserializer(|ctx, data: &[u8]| {
               let mut temp = records::temperature::knx::from_knx(data, "9/1/0")?;
               temp.timestamp = ctx.time().now();  // Platform-independent timing
               ctx.log().debug("Deserialized KNX temperature");
               Ok(temp)
           })
           .finish();
});

// RAW: Plain deserializer without context (mirrors .tap_raw())
builder.configure::<Temperature>(SensorKey::Outdoor, |reg| {
    reg.buffer(BufferCfg::SingleLatest)
       .link_from("knx://gateway/9/1/0")
           .with_deserializer_raw(|data: &[u8]| {
               records::temperature::knx::from_knx(data, "9/1/0")
           })
           .finish();
});
```

### Pattern Consistency with `tap` / `tap_raw`

| API | Context injected | Defined in |
|-----|-----------------|------------|
| `.tap(\|ctx, consumer\| ...)` | Yes | `ext_macros.rs` (generated) |
| `.tap_raw(\|consumer, ctx_any\| ...)` | No (raw `Arc<dyn Any>`) | `aimdb-core` |
| `.with_deserializer(\|ctx, data\| ...)` | **Yes** | `aimdb-core/src/typed_api.rs` (direct) |
| `.with_deserializer_raw(\|data\| ...)` | **No** | `aimdb-core/src/typed_api.rs` (direct) |

### Mutually Exclusive

`.with_deserializer()` and `.with_deserializer_raw()` are mutually exclusive on the same link — calling one replaces the other. This avoids ambiguity about which deserializer runs.

## Implementation

### 1. New Type Alias — `ContextDeserializerFn`

**File:** `aimdb-core/src/connector.rs`

```rust
/// Type alias for context-aware type-erased deserializer callbacks
///
/// Like `DeserializerFn`, but receives a type-erased runtime context
/// for platform-independent timestamps and logging during deserialization.
///
/// The first argument is the type-erased runtime (as `Arc<dyn Any + Send + Sync>`),
/// which is downcast to the concrete runtime type via `RuntimeContext::extract_from_any`.
pub type ContextDeserializerFn = Arc<
    dyn Fn(Arc<dyn core::any::Any + Send + Sync>, &[u8]) -> Result<Box<dyn core::any::Any + Send>, String>
        + Send
        + Sync,
>;
```

This uses `Arc` from `alloc::sync` (re-exported in `std`), matching the existing `DeserializerFn` definition which also uses bare `Arc` without cfg gates.

Note: the context is passed as `Arc<dyn Any>` (cloned from the connector's stored runtime Arc). This is a single atomic reference count increment per message — negligible compared to actual deserialization work. Using `Arc` (rather than `&dyn Any`) is **required** because `RuntimeContext::extract_from_any` needs to `downcast::<R>()` on an owned Arc, and on `no_std` it leaks the Arc to obtain a `&'static R` for the Embassy adapter's static-reference model.

### 2. Extend `InboundConnectorLink`

**File:** `aimdb-core/src/connector.rs`

Store the deserializer as an enum to enforce mutual exclusivity:

```rust
/// Which deserializer variant is registered for an inbound link
pub enum DeserializerKind {
    /// Plain bytes-only deserializer (from `.with_deserializer_raw()`)
    Raw(DeserializerFn),
    /// Context-aware deserializer (from `.with_deserializer()` in typed_api.rs)
    Context(ContextDeserializerFn),
}

pub struct InboundConnectorLink {
    pub url: ConnectorUrl,
    pub config: Vec<(String, String)>,
    pub deserializer: DeserializerKind,

    #[cfg(feature = "alloc")]
    pub producer_factory: Option<ProducerFactoryFn>,
    pub topic_resolver: Option<TopicResolverFn>,
}
```

### 3. Extend `Route` and `Router::route()`

**File:** `aimdb-core/src/router.rs`

```rust
pub struct Route {
    pub resource_id: Arc<str>,
    pub producer: Box<dyn ProducerTrait>,
    pub deserializer: DeserializerKind,
}
```

Update `Router::route()` to accept an optional context and dispatch accordingly:

```rust
impl Router {
    /// Route a message, with optional runtime context for deserializers
    ///
    /// Dispatches based on `DeserializerKind`:
    /// - `Raw` — calls deserializer with payload only
    /// - `Context` — calls deserializer with context + payload
    ///
    /// If a `Context` deserializer is registered but no context is provided,
    /// the route is skipped with a warning (connector hasn't been migrated yet).
    pub async fn route(
        &self,
        resource_id: &str,
        payload: &[u8],
        ctx: Option<&Arc<dyn core::any::Any + Send + Sync>>,
    ) -> Result<(), String> {
        for route in &self.routes {
            if route.resource_id.as_ref() == resource_id {
                let result = match &route.deserializer {
                    DeserializerKind::Raw(deser) => (deser)(payload),
                    DeserializerKind::Context(deser) => match ctx {
                        Some(ctx) => (deser)(ctx.clone(), payload),
                        None => {
                            #[cfg(feature = "tracing")]
                            tracing::warn!(
                                "Context deserializer on '{}' but no context provided",
                                resource_id
                            );
                            continue;
                        }
                    },
                };

                match result {
                    Ok(value_any) => {
                        route.producer.produce_any(value_any).await?;
                    }
                    Err(e) => { /* existing error logging */ }
                }
            }
        }
        Ok(())
    }
}
```

**Note on `Router::route()` signature change:** The existing `route(&self, resource_id, payload)` gains an `Option` context parameter (a reference to `Arc<dyn Any + Send + Sync>`). This is a **one-time breaking change** at the core level, but all call sites are internal (inside connectors), not user-facing. Existing connectors pass `None` until migrated to pass the runtime `Arc`.

### 4. Thread Context Through `collect_inbound_routes`

**File:** `aimdb-core/src/builder.rs`

Replace the separate `DeserializerFn` with `DeserializerKind`:

```rust
pub fn collect_inbound_routes(
    &self,
    scheme: &str,
) -> Vec<(
    String,
    Box<dyn crate::connector::ProducerTrait>,
    crate::connector::DeserializerKind,  // CHANGED from DeserializerFn
)> {
    // ... existing iteration logic ...
    routes.push((topic, producer, link.deserializer.clone()));
}
```

`RouterBuilder::from_routes()` is updated to accept `DeserializerKind` directly.

**Note:** This is a breaking change to an internal API. There are 2 call sites for `collect_inbound_routes` (both in `aimdb-websocket-connector`) and 5 call sites for `router.route()` across the MQTT, KNX, and WebSocket connectors, plus 3 unit tests in `router.rs`. All must be updated.

### 5. Core Builder Method — `with_deserializer_raw()`

**File:** `aimdb-core/src/typed_api.rs`

Rename the existing `with_deserializer()` to `with_deserializer_raw()` — this is the bytes-only variant that lives in core and requires no runtime generic:

```rust
impl<'a, T, R> InboundConnectorBuilder<'a, T, R>
where
    T: Send + Sync + 'static + Debug + Clone,
    R: Spawn + 'static,
{
    /// Sets a raw deserialization callback (bytes only, no context)
    ///
    /// Prefer `.with_deserializer(|ctx, data| ...)` for access to
    /// `RuntimeContext` (timestamps, logging). Use this raw variant
    /// only when context is unnecessary.
    pub fn with_deserializer_raw<F>(mut self, f: F) -> Self
    where
        F: Fn(&[u8]) -> Result<T, String> + Send + Sync + 'static,
    {
        self.deserializer = Some(Arc::new(f));
        self.context_deserializer = None;  // mutually exclusive
        self
    }
}
```

Note: The builder stores typed closures (`TypedDeserializerFn<T>` / `ContextDeserializerFn`). Type erasure and wrapping into `DeserializerKind` happens in `finish()`.

### 6. Context-Aware Builder Method — `with_deserializer()` (in typed_api.rs)

**File:** `aimdb-core/src/typed_api.rs`

Unlike `.tap()` — which needs `ext_macros.rs` generation because `RecordRegistrar` has no generic `R` — the `InboundConnectorBuilder<'a, T, R>` already carries `R` as a generic parameter. This means `.with_deserializer()` can be added **directly** in `typed_api.rs` with an additional `R: Runtime` bound:

```rust
impl<'a, T, R> InboundConnectorBuilder<'a, T, R>
where
    T: Send + Sync + 'static + Debug + Clone,
    R: Runtime + Send + Sync + 'static,
{
    /// Sets a context-aware deserialization callback
    ///
    /// The closure receives a `RuntimeContext<R>` for platform-independent
    /// timestamps and logging, plus the raw bytes from the external system.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// .link_from("knx://gateway/9/1/0")
    ///     .with_deserializer(|ctx, data: &[u8]| {
    ///         let mut temp = from_knx(data, "9/1/0")?;
    ///         temp.timestamp = ctx.time().now();
    ///         Ok(temp)
    ///     })
    /// ```
    pub fn with_deserializer<F>(mut self, f: F) -> Self
    where
        F: Fn(RuntimeContext<R>, &[u8]) -> Result<T, String> + Send + Sync + 'static,
    {
        let f = Arc::new(f);
        self.context_deserializer = Some(Arc::new(move |ctx_any, bytes| {
            let ctx = RuntimeContext::<R>::extract_from_any(ctx_any);
            (f)(ctx, bytes).map(|val| Box::new(val) as Box<dyn core::any::Any + Send>)
        }));
        self.deserializer = None;  // mutually exclusive
        self
    }
}
```

**Why no `extract_from_any_ref`?** The existing `extract_from_any` (which takes `Arc<dyn Any>`) is reused as-is. A borrowed `&dyn Any` variant was considered but is unsound:

- When a connector does `runtime_any.as_ref()` on `Arc<dyn Any>` where the inner type is `R`, the resulting `&dyn Any` has concrete type `R`, **not** `Arc<R>`. So `downcast_ref::<Arc<R>>()` would always return `None`.
- Even if corrected to `downcast_ref::<R>()`, it yields `&R` — but `RuntimeContext` requires `Arc<R>` (std) or `&'static R` (no_std). Neither can be obtained from a plain `&R`.
- On `no_std`, the existing `extract_from_any` leaks `Arc<R>` to `&'static R` via `Box::leak()`. This is acceptable for one-time `tap()` setup but would be a **memory leak per message** if called in a deserializer hot path via a ref variant.

The `Arc` clone (one atomic increment per message) is the correct approach — negligible cost compared to actual deserialization work, and consistent with the established pattern.

### 7. Update `InboundConnectorBuilder` Struct

**File:** `aimdb-core/src/typed_api.rs`

The builder needs a second optional field for the context-aware deserializer. Type erasure happens in `finish()`, not at set-time:

```rust
pub struct InboundConnectorBuilder<'a, T, R> {
    registrar: &'a mut RecordRegistrar<'a, T, R>,
    url: String,
    config: Vec<(String, String)>,
    deserializer: Option<TypedDeserializerFn<T>>,           // raw (bytes only)
    context_deserializer: Option<ContextDeserializerFn>,     // context-aware (already type-erased)
    topic_resolver: Option<crate::connector::TopicResolverFn>,
}
```

The `finish()` method resolves which variant was set:

```rust
pub fn finish(self) -> &'a mut RecordRegistrar<'a, T, R> {
    // ... existing URL parsing and validation ...

    // Resolve deserializer variant (mutually exclusive, validated above)
    let deser_kind = if let Some(ctx_deser) = self.context_deserializer {
        DeserializerKind::Context(ctx_deser)
    } else if let Some(raw_deser) = self.deserializer {
        // Type-erase the raw deserializer (existing pattern)
        let erased: DeserializerFn = Arc::new(move |bytes: &[u8]| {
            raw_deser(bytes).map(|val| Box::new(val) as Box<dyn core::any::Any + Send>)
        });
        DeserializerKind::Raw(erased)
    } else {
        panic!(
            "Inbound connector requires a deserializer. Call .with_deserializer() for {}",
            self.url
        );
    };

    let mut link = InboundConnectorLink::new(url, deser_kind);
    // ... rest of finish() unchanged ...
}
```

**Note:** `InboundConnectorLink::new()` signature changes to accept `DeserializerKind` instead of `DeserializerFn`.

### 8. Connector Adaptation — Pass Context to Router

Each connector passes the runtime `Arc` to `router.route()` as `Some(&ctx)`.

**Note:** There is no `runtime_any()` method on the database. The runtime `Arc<dyn Any + Send + Sync>` is materialized inline via Rust's coercion rules (`Arc<R>` → `Arc<dyn Any + Send + Sync>`), as already done in `spawn_consumer_service()` and `spawn_producer_service()`. Connectors must capture this during the build phase.

**MQTT example** (`aimdb-mqtt-connector/src/tokio_client.rs`):

```rust
// During build: capture runtime Arc via coercion from the database's inner runtime
// (The exact mechanism depends on how the connector accesses the database.
//  A new accessor method may be needed, or the runtime can be threaded
//  through the existing connector builder API.)
let runtime_any: Arc<dyn Any + Send + Sync> = runtime_arc.clone();  // Arc<R> → Arc<dyn Any>

// In event loop:
if let rumqttc::Event::Incoming(Packet::Publish(publish)) = notification {
    if let Err(e) = router.route(
        &publish.topic,
        &publish.payload,
        Some(&runtime_any),
    ).await {
        // error handling
    }
}
```

Connectors that haven't been migrated yet pass `None` — raw deserializers work, context deserializers log a warning and skip.

**Plumbing options for connectors to obtain the runtime Arc:**
1. Add a `runtime_any()` accessor to `AimDb<T, R>` that returns `&Arc<dyn Any + Send + Sync>`
2. Thread the runtime through the connector builder (e.g., `MqttConnectorBuilder::with_runtime(arc)`)
3. Extract from `collect_inbound_routes()` by extending its return tuple

Option 1 is simplest and consistent with how `collect_inbound_routes` already receives the database.

## Alternatives Considered

### A. Capture Context in Closure at Configuration Time

```rust
// Not viable — RuntimeContext<R> doesn't exist during configuration
let ctx = ???;
.with_deserializer(move |data| {
    let mut temp = from_knx(data)?;
    temp.timestamp = ctx.time().now();
    Ok(temp)
})
```

**Rejected:** The runtime adapter hasn't been constructed yet during `builder.configure()`. The context is only available after `AimDbBuilder::build()`.

### B. Two-Phase Deserializer (Curried)

```rust
// User provides a factory that receives context once, returns deserializer
.with_deserializer_factory(|ctx: RuntimeContext<R>| {
    move |data: &[u8]| { /* ... */ }
})
```

**Rejected:** More complex API, harder to understand, and the closure-returning-closure pattern is ergonomically awkward in Rust (lifetime and ownership friction).

### C. Make `DeserializerFn` Always Context-Aware

Change `DeserializerFn` to always take `(&dyn Any, &[u8])`, passing a dummy context for plain deserializers.

**Rejected:** Forces all deserializers to accept and ignore a context argument, even when they don't need it. The `DeserializerKind` enum is more expressive and avoids dummy values.

### D. Context via Thread-Local / Global

Store `RuntimeContext` in a thread-local during `Router::route()`, accessible by plain `DeserializerFn` closures.

**Rejected:** Incompatible with `no_std` / Embassy; couples deserializers to hidden global state; violates Rust's explicit-is-better principle.

### E. Context at the InboundConnectorLink Level (Lazy Injection)

Inject context into `InboundConnectorLink` during `collect_inbound_routes`, binding it into the existing `DeserializerFn` closure at that point.

**Rejected:** `collect_inbound_routes` returns owned tuples (not references to links), so there's no place to bind context after configuration but before route dispatch. Would require restructuring the entire collection pipeline.

### F. Borrowed Context via `&dyn Any` (Allocation-Free Dispatch)

Pass context as `&dyn Any` (borrowed from Arc) instead of cloning `Arc<dyn Any>` per message, to avoid the atomic increment cost.

**Rejected:** Unsound. When dereferencing `Arc<dyn Any>` where inner type is `R`, the resulting `&dyn Any` has concrete type `R`, not `Arc<R>`. `RuntimeContext::extract_from_any` requires an owned `Arc` for `downcast::<R>()`, and on `no_std` it leaks the Arc to `&'static R` — calling this per-message would be a memory leak. The Arc clone cost (one atomic increment) is negligible vs. deserialization.

### G. Generate `with_deserializer()` via `ext_macros.rs` (Like `tap()`)

Generate the context-aware method in the `impl_record_registrar_ext!` macro, mirroring how `.tap()` wraps `.tap_raw()`.

**Rejected (unnecessary):** Unlike `RecordRegistrar` (which has no generic `R`, requiring the macro to inject the concrete runtime type), `InboundConnectorBuilder<'a, T, R>` already carries `R` as a generic parameter. The method can be added directly in `typed_api.rs` with an `R: Runtime` bound, avoiding macro complexity for no benefit.

## Migration & Backward Compatibility

### Breaking Changes

- **`.with_deserializer()` signature changes** — now takes `|ctx, data|` instead of `|data|`. This is the only user-facing breaking change.
- **`Router::route()` gains `Option` context parameter** — internal to connectors, not user-facing. 5 connector call sites + 3 tests.
- **`InboundConnectorLink::deserializer` changes to `DeserializerKind`** — internal type, not user-facing.
- **`InboundConnectorLink::new()` takes `DeserializerKind`** — internal constructor change.
- **`collect_inbound_routes()` return type changes** — `DeserializerFn` → `DeserializerKind`. 2 call sites in `aimdb-websocket-connector`.
- **`RouterBuilder::from_routes()` / `add_route()` take `DeserializerKind`** — internal builder change.

### What Stays the Same

- **`DeserializerFn` type alias** — retained for `with_deserializer_raw()` and internal use
- **All existing connector logic** — connectors pass `None` until migrated

### Migration Path for Users

```rust
// BEFORE: plain deserializer
.with_deserializer(|data: &[u8]| {
    records::temperature::knx::from_knx(data, "9/1/0")
})

// AFTER (option A): upgrade to context-aware (recommended)
.with_deserializer(|ctx, data: &[u8]| {
    let mut temp = records::temperature::knx::from_knx(data, "9/1/0")?;
    temp.timestamp = ctx.time().now();
    Ok(temp)
})

// AFTER (option B): rename to raw if context not needed
.with_deserializer_raw(|data: &[u8]| {
    records::temperature::knx::from_knx(data, "9/1/0")
})
```

The compiler guides this migration — existing `|data|` closures won't type-check against the new `|ctx, data|` signature, and the error message points users to `.with_deserializer_raw()`.

## Testing Strategy

### Unit Tests

1. **`ContextDeserializerFn` type-erasure round-trip** — verify a typed context deserializer can be stored and called through the type-erased alias
2. **`extract_from_any` with Arc clone** — verify both `std` and `no_std` variants downcast correctly when cloning the `Arc<dyn Any>` per call, and panic on type mismatch
3. **`InboundConnectorLink` with both deserializer types** — verify `DeserializerKind` stores and clones correctly for both `Raw` and `Context` variants

### Integration Tests

4. **`Router::route()` with context** — build a router with a `DeserializerKind::Context` route, invoke `route()` with `Some(&ctx)`, verify the deserialized value includes context-derived data (e.g., a timestamp field)
5. **`Router::route()` raw fallback** — verify that `DeserializerKind::Raw` routes work correctly when called with `Some(&ctx)` (context is ignored)
6. **`Router::route()` with `None` context** — verify `DeserializerKind::Raw` routes work with `None`; verify `DeserializerKind::Context` routes are skipped with a warning
7. **Mixed routes** — router with both `Raw` and `Context` deserializers on different topics, verify each dispatches correctly

### Runtime Adapter Tests

8. **Tokio adapter** — `#[tokio::test]` with a full `AimDb` instance, register a context-aware deserializer via `.with_deserializer(|ctx, data| ...)`, simulate an inbound message, verify the record contains a platform timestamp
9. **Embassy adapter** — cross-compile check (`cargo check --target thumbv7em-none-eabihf`) ensuring `ContextDeserializerFn` compiles for `no_std`
10. **WASM adapter** — cross-compile check (`cargo check --target wasm32-unknown-unknown`) ensuring the `no_std + alloc + Arc` path works

### Connector Tests

11. **MQTT connector** — verify `route()` is called with `Some(&ctx)` when processing incoming MQTT messages
12. **KNX connector** — same for KNX telegrams (both std and embassy variants)
13. **WebSocket connector** — same for WebSocket messages

## Implementation Plan

### Phase 1: Core Types

- [ ] Add `ContextDeserializerFn` type alias in `connector.rs` (uses `alloc::sync::Arc`, no cfg split)
- [ ] Add `DeserializerKind` enum in `connector.rs`
- [ ] Refactor `InboundConnectorLink` to use `DeserializerKind` (update `new()`, `Clone` impl)
- [ ] Refactor `Route` to use `DeserializerKind`
- [ ] Update `Router::route()` to accept `Option<&Arc<dyn Any + Send + Sync>>` context (no cfg split)
- [ ] Update `RouterBuilder::from_routes()` and `add_route()` for `DeserializerKind`
- [ ] Unit tests for core types

### Phase 2: Builder API

- [ ] Add `context_deserializer: Option<ContextDeserializerFn>` field to `InboundConnectorBuilder`
- [ ] Rename existing `with_deserializer()` to `with_deserializer_raw()`
- [ ] Add context-aware `with_deserializer(|ctx, data| ...)` directly in `typed_api.rs` (with `R: Runtime` bound)
- [ ] Update `finish()` to resolve `DeserializerKind` from whichever field is set
- [ ] Update `collect_inbound_routes()` to return `DeserializerKind`
- [ ] Integration tests for builder round-trip

### Phase 3: Connector Migration

- [ ] Determine runtime Arc plumbing approach (add `runtime_any()` accessor or thread through builders)
- [ ] MQTT connector: pass `Some(&runtime_any)` to `router.route()`
- [ ] KNX connector (std + embassy): pass `Some(&runtime_any)` to `router.route()`
- [ ] WebSocket connector: pass `Some(&runtime_any)` to `router.route()`
- [ ] Update 3 unit tests in `router.rs` for new `route()` signature
- [ ] Connector-level tests

### Phase 4: Examples & Documentation

- [ ] Update KNX demo to show context-aware timestamping
- [ ] Add doc comment examples on `with_deserializer()`
- [ ] Verify `no_std` cross-compilation (Embassy: `thumbv7em-none-eabihf`, WASM: `wasm32-unknown-unknown`)
