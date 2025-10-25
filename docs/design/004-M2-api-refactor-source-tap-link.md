# API Refactoring: `.source()`, `.tap()`, `.link()`

## Status: Approved for Implementation
**Date**: 2025-10-25

## Quick Summary

**Goal**: Rename `.producer()` → `.tap()` for better semantic clarity.

**Reason**: `.producer()` is misleading - it registers observers/side-effects, not data producers. The actual data producers are services that call `db.produce()`.

**Implementation**: Add `.tap()` as alias, update examples, deprecate `.producer()`, remove in v0.3.0+

**Key Files**:
- Implementation: `aimdb-core/src/producer_consumer.rs` (line ~52)
- Examples: `examples/mqtt-connector-demo/src/main.rs`, `examples/embassy-mqtt-connector-demo/src/main.rs`

---

## Problem Statement

The current API uses confusing terminology:

```rust
// Current (confusing)
builder.configure::<Temperature>(|reg| {
    reg.producer(|_em, temp| { /* logs temperature */ })  // ❌ Not producing, just observing!
       .link("mqtt://...")                                 // ✅ Clear
       .finish();
});
```

**Issues:**
- `.producer()` suggests data generation, but it's actually an observer/side-effect
- The actual producer (`temperature_producer` service) is spawned separately
- No clear distinction between observers and external connectors

## Proposed API

### New Method Names

| Method | Purpose | Type | Example |
|--------|---------|------|---------|
| `.source()` | Register data source | Optional | Temperature sensor reading |
| `.tap()` | Side-effect observer | Observer | Logging, metrics, validation |
| `.link()` | External system connector | Consumer | MQTT, Kafka, HTTP endpoint |

### Example Usage

```rust
builder.configure::<Temperature>(|reg| {
    // Optional: Register managed data source
    reg.source(temperature_sensor_service)
    
    // Register side-effect observers (tap into the data stream)
    .tap(|_em, temp| async move {
        info!("Temperature: {:.1}°C", temp.celsius);
    })
    .tap(|_em, temp| async move {
        metrics::gauge!("temperature", temp.celsius);
    })
    
    // Link to external systems
    .link("mqtt://sensors/temperature")
        .with_qos(1)
        .with_serializer(|temp| Ok(temp.to_json_vec()))
        .finish()
    
    .link("http://api.example.com/sensors")
        .with_method("POST")
        .with_serializer(|temp| Ok(temp.to_json_vec()))
        .finish();
});
```

## Semantic Clarity

### Data Flow Architecture

```
┌─────────────────────────────────────────────────────────┐
│                     Data Source                          │
│  (temperature_producer service calls db.produce())      │
└──────────────────────┬──────────────────────────────────┘
                       │
                       ▼
┌─────────────────────────────────────────────────────────┐
│                   AimDB Database                         │
│              (Record: Temperature)                       │
└──────┬───────────────┬──────────────────────────────────┘
       │               │
       ▼               ▼
   ┌───────┐      ┌────────────┐
   │ .tap()│      │   .link()  │
   │       │      │            │
   │ Log   │      │ MQTT Pub   │
   │ Metrics│      │ HTTP POST  │
   └───────┘      └────────────┘
  Side Effects   External Systems
```

### Method Semantics

#### `.source()` - Data Generator (Optional)

Registers a service that generates data and calls `db.produce()`:

```rust
reg.source(temperature_producer)
```

**When to use:**
- When the builder should manage the service lifecycle
- For sources that need initialization by the builder
- Currently **not implemented** - sources are spawned manually

**Current Workaround:**
```rust
// Sources are currently spawned separately
spawner.spawn(temperature_producer(db));
```

#### `.tap()` - Side-Effect Observer

Registers a callback for observing/monitoring without consuming:

```rust
.tap(|_em, temp| async move {
    info!("Temperature: {:.1}°C", temp.celsius);
})
```

**Characteristics:**
- Non-consuming observation
- Side effects only (logging, metrics, validation)
- Multiple taps can be chained
- Errors don't block other observers

**Use Cases:**
- Logging and debugging
- Metrics collection
- Validation and alerting
- Audit trails
- Cache updates

#### `.link()` - External System Connector

Connects to external systems (databases, message brokers, APIs):

```rust
.link("mqtt://sensors/temperature")
    .with_serializer(|temp| Ok(temp.to_json_vec()))
    .finish()
```

**Characteristics:**
- Connects to external systems
- Manages serialization
- Handles connection pooling
- Protocol-specific configuration

**Use Cases:**
- MQTT publishing
- Kafka streaming
- HTTP webhooks
- Database replication
- File writing

## Migration Path

### Phase 1: Add New Methods (Alias)

Add `.tap()` as an alias to `.producer()`:

```rust
impl RecordRegistrar {
    pub fn tap(&mut self, callback: ...) -> &mut Self {
        self.producer(callback) // Delegate to existing
    }
}
```

### Phase 2: Update Examples

Update all examples to use the new API:

```rust
// Before
reg.producer(|_, temp| { log(temp) })

// After
reg.tap(|_, temp| { log(temp) })
```

### Phase 3: Deprecate Old Methods

Mark `.producer()` as deprecated:

```rust
#[deprecated(since = "0.2.0", note = "Use `.tap()` for observers")]
pub fn producer(&mut self, callback: ...) -> &mut Self {
    self.tap(callback)
}
```

### Phase 4: Remove Old Methods

Remove `.producer()` in a future major version.

## Benefits

### For Developers

1. **Semantic Clarity**: Method names reflect actual behavior
2. **Discoverability**: API is self-documenting
3. **Familiarity**: `.tap()` is familiar to Rust developers
4. **Consistency**: All record types use the same pattern

### For the Project

1. **Better Documentation**: API matches mental model
2. **Easier Onboarding**: New contributors understand the flow
3. **Future Expansion**: Room for `.source()` to manage lifecycle
4. **Alignment with Patterns**: Matches observer/stream patterns

## Examples Comparison

### Before (Current API)

```rust
builder.configure::<Temperature>(|reg| {
    reg.producer(|_em, temp| async move {
        info!("Temperature: {:.1}°C", temp.celsius);
    })
    .link("mqtt://sensors/temperature")
        .with_serializer(|temp| Ok(temp.to_json_vec()))
        .finish();
});

// Producer spawned separately
spawner.spawn(temperature_producer(db));
```

### After (Proposed API)

```rust
builder.configure::<Temperature>(|reg| {
    reg.tap(|_em, temp| async move {
        info!("Temperature: {:.1}°C", temp.celsius);
    })
    .link("mqtt://sensors/temperature")
        .with_serializer(|temp| Ok(temp.to_json_vec()))
        .finish();
});

// Producer still spawned separately (for now)
spawner.spawn(temperature_producer(db));
```

## Future Enhancement: Managed Sources

In the future, `.source()` could manage service lifecycle:

```rust
builder.configure::<Temperature>(|reg| {
    // Builder spawns and manages the source
    reg.source(temperature_producer)
    
    .tap(|_, temp| async move {
        info!("Temperature: {:.1}°C", temp.celsius);
    })
    
    .link("mqtt://sensors/temperature")
        .with_serializer(|temp| Ok(temp.to_json_vec()))
        .finish();
});

// No manual spawning needed!
let db = builder.build()?;
```

This would require:
- Source lifecycle management in the builder
- Dependency injection for sources
- Proper shutdown handling

## Implementation Checklist

### Phase 1: Add `.tap()` Method (Breaking: None, Additive Change)

- [ ] **Add method** to `aimdb-core/src/producer_consumer.rs` after line 62
  ```bash
  # Edit file and add .tap() method after .producer()
  ```
- [ ] **Run tests** to verify alias works
  ```bash
  cd /aimdb && cargo test --all-features
  ```
- [ ] **Build all examples** to ensure no breakage
  ```bash
  make build
  ```

### Phase 2: Update Examples (No Breaking Changes)

- [ ] **Update** `examples/mqtt-connector-demo/src/main.rs`
  - Line ~295: `reg.producer(...)` → `reg.tap(...)`
- [ ] **Update** `examples/embassy-mqtt-connector-demo/src/main.rs`
  - Line ~295: `reg.producer(...)` → `reg.tap(...)`
- [ ] **Update** `examples/producer-consumer-demo/src/main.rs`
  - Search and replace all `.producer(...)` → `.tap(...)`
- [ ] **Update** `examples/tokio-runtime-demo/src/main.rs` (if applicable)
- [ ] **Update** `examples/embassy-runtime-demo/src/main.rs` (if applicable)
- [ ] **Verify** all examples compile and run:
  ```bash
  cargo run --example mqtt-connector-demo
  cargo build --example embassy-mqtt-connector-demo --target thumbv8m.main-none-eabihf
  cargo run --example producer-consumer-demo
  ```

### Phase 3: Update Documentation

- [ ] **Update** `README.md` - Replace `.producer()` with `.tap()` in examples
- [ ] **Update** `docs/vision/future_usage.md` - Update aspirational examples
- [ ] **Update** `.github/copilot-instructions.md` - Update terminology
- [ ] **Add** explanation of `.tap()` vs `.link()` to main documentation
- [ ] **Generate** API docs and verify:
  ```bash
  cargo doc --all-features --no-deps --open
  ```

### Phase 4: Deprecate `.producer()` (Breaking: Warnings Only)

- [ ] **Add deprecation** attribute to `.producer()` method:
  ```rust
  #[deprecated(since = "0.2.0", note = "Use `.tap()` for side-effect observers")]
  ```
- [ ] **Run clippy** to find any internal uses:
  ```bash
  cargo clippy --all-targets --all-features
  ```
- [ ] **Update** any internal code that uses `.producer()`
- [ ] **Test** that deprecation warnings appear correctly

### Phase 5: Final Validation

- [ ] **Run full CI** locally:
  ```bash
  make all
  ```
- [ ] **Review** all changes in `git diff`
- [ ] **Update** CHANGELOG.md with migration notes
- [ ] **Commit** with message: "feat: Add `.tap()` method as clearer alias for `.producer()`"

### Phase 6: Future Removal (Major Version)

- [ ] In v0.3.0 or later: Remove `.producer()` method entirely
- [ ] Update migration guide

## Implementation Details

### File Locations

**Core Implementation:**
- `aimdb-core/src/producer_consumer.rs` - Contains `RecordRegistrar` implementation
  - Add `.tap()` method as alias to existing `.producer()` method
  - Method signature: `pub fn tap<F, Fut>(&mut self, callback: F) -> &mut Self`
  - Where `F: Fn(&Emitter, &T) -> Fut + Send + Sync + 'static`
  - And `Fut: Future<Output = ()> + Send + 'static`

**Examples to Update:**
- `examples/mqtt-connector-demo/src/main.rs` - Lines ~295-305
- `examples/embassy-mqtt-connector-demo/src/main.rs` - Lines ~295-305
- `examples/producer-consumer-demo/src/main.rs` - Multiple locations
- `examples/tokio-runtime-demo/src/main.rs` - If applicable
- `examples/embassy-runtime-demo/src/main.rs` - If applicable

**Documentation Files:**
- `README.md` - Update API examples
- `docs/vision/future_usage.md` - Update aspirational examples
- `.github/copilot-instructions.md` - Update terminology

### Current Method Signature (Verified)

Located in `aimdb-core/src/producer_consumer.rs`:

```rust
// In RecordRegistrar<'a, T>
/// Registers a producer function
///
/// Called first when data is produced. Only one producer per record type.
/// Panics if producer already registered.
pub fn producer<F, Fut>(&'a mut self, f: F) -> &'a mut Self
where
    F: Fn(crate::emitter::Emitter, T) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    self.rec.set_producer(f);
    self
}
```

**Note**: The signature takes `T` by value (owned), not `&T` (reference).
This is important - the callback receives an owned value!

### New Method to Add (Corrected Signature)

Add this method to `RecordRegistrar<'a, T>` in `aimdb-core/src/producer_consumer.rs`:

```rust
/// Register a side-effect observer that taps into the data stream.
///
/// This is a non-consuming observer that gets called whenever `db.produce()` is invoked
/// for this record type. Multiple `.tap()` calls can be chained to register multiple observers.
///
/// **Note:** Despite the name, this receives owned data (not a reference) like `.consumer()`.
/// The "tap" metaphor refers to tapping into the data flow, not Rust borrowing.
///
/// # Use Cases
/// - Logging and debugging
/// - Metrics collection  
/// - Validation and alerting
/// - Audit trails
///
/// # Example
/// ```ignore
/// reg.tap(|_em, temp| async move {
///     info!("Temperature: {:.1}°C", temp.celsius);
/// })
/// .tap(|_em, temp| async move {
///     metrics::gauge!("temperature", temp.celsius);
/// })
/// ```
pub fn tap<F, Fut>(&'a mut self, f: F) -> &'a mut Self
where
    F: Fn(crate::emitter::Emitter, T) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    // Delegate to existing producer implementation
    self.producer(f)
}
```

### Deprecation Implementation

After examples are updated, add to `.producer()`:

```rust
#[deprecated(
    since = "0.2.0",
    note = "Use `.tap()` for side-effect observers. The term 'producer' is misleading since this registers observers, not data sources."
)]
pub fn producer<F, Fut>(&mut self, callback: F) -> &mut Self
where
    F: Fn(&Emitter, &T) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    // Existing implementation unchanged
}
```

### Testing Strategy

1. **Backward Compatibility**: Ensure `.producer()` still works
2. **Alias Correctness**: Verify `.tap()` produces identical behavior
3. **Multiple Taps**: Test chaining multiple `.tap()` calls
4. **Integration Tests**: Run all examples with new API
5. **Documentation Examples**: Verify all doc examples compile

### Search Patterns for Finding Usage

```bash
# Find all uses of .producer()
rg "\.producer\(" --type rust

# Find in examples specifically
rg "\.producer\(" examples/ --type rust

# Find in documentation
rg "\.producer\(" docs/ README.md
```

## References

- Rust `Iterator::tap` - https://doc.rust-lang.org/std/iter/trait.Iterator.html#method.tap
- Observer Pattern - https://en.wikipedia.org/wiki/Observer_pattern
- Reactive Programming - https://en.wikipedia.org/wiki/Reactive_programming

## Decision

**Status**: Approved for implementation
**Date**: 2025-10-25
**Decision Maker**: Project maintainers

The team agrees that `.tap()` provides better semantic clarity than `.producer()` for observer callbacks. Implementation will follow the phased migration path to maintain backward compatibility.
