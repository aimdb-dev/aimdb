# Buffer Usage Guide

**Status**: ✅ Implemented  
**Target Milestone**: M1 - Runtime Integration  
**Related**: [002-M1_pluggable_buffers.md](./002-M1_pluggable_buffers.md)

## Overview

AimDB provides three pluggable buffer types for per-record data streaming, designed to support different communication patterns across MCU, edge, and cloud deployments.

This guide explains **when to use each buffer type**, **how to configure them**, and **best practices** for production use.

---

## Buffer Types

### 1. SPMC Ring Buffer

**Pattern**: Single Producer, Multiple Consumers  
**Semantics**: Bounded queue with lag detection  
**Best For**: High-frequency telemetry, event streams, audit logs

#### Characteristics

- ✅ **Multiple consumers** can read independently
- ✅ **Bounded capacity** provides backpressure
- ✅ **Lag detection** when consumers fall behind
- ✅ **No data loss** for fast consumers
- ⚠️  **Slow consumers** may miss messages (lag error)

#### When to Use

```
📊 Sensor Data Streaming
   → 100 Hz temperature readings
   → Multiple analysis pipelines (real-time, historical, ML)
   → Some consumers may lag, but latest data matters most

🔍 System Event Monitoring  
   → Application events, errors, warnings
   → Multiple subscribers (logging, metrics, alerting)
   → Bounded capacity prevents memory exhaustion

📡 Telemetry Pipelines
   → High-frequency data from embedded sensors
   → Multiple edge processors
   → Lag detection allows graceful degradation
```

#### Configuration

**Tokio**:
```rust
use aimdb_tokio_adapter::TokioBuffer;
use aimdb_core::buffer::{BufferBackend, BufferCfg};

let buffer = TokioBuffer::<SensorReading>::new(
    &BufferCfg::SpmcRing { capacity: 100 }
);

// Subscribe multiple consumers
let reader1 = buffer.subscribe();
let reader2 = buffer.subscribe();
```

**Embassy (no_std)**:
```rust
use aimdb_embassy_adapter::EmbassyBuffer;
use static_cell::StaticCell;

// Const generics: <T, CAP, SUBS, PUBS, WATCH_N>
type SensorBuffer = EmbassyBuffer<SensorReading, 100, 4, 1, 1>;

static BUFFER: StaticCell<SensorBuffer> = StaticCell::new();
let buffer = BUFFER.init(SensorBuffer::new_spmc());
```

#### Handling Lag

```rust
loop {
    match reader.recv().await {
        Ok(value) => {
            // Process value normally
            process(value).await;
        }
        Err(DbError::BufferLagged { lag_count, .. }) => {
            // Consumer fell behind, skipped `lag_count` messages
            log::warn!("Lagged behind by {} messages", lag_count);
            // Continue processing - next recv() gets latest data
            continue;
        }
        Err(DbError::BufferClosed { .. }) => {
            // Producer closed, exit gracefully
            break;
        }
        Err(e) => {
            log::error!("Buffer error: {:?}", e);
            break;
        }
    }
}
```

---

### 2. SingleLatest

**Pattern**: Latest value only  
**Semantics**: Watch channel with intermediate skipping  
**Best For**: Configuration, state updates, latest measurements

#### Characteristics

- ✅ **Always get latest value** when ready to process
- ✅ **Intermediate updates skipped** automatically
- ✅ **Low latency** for state synchronization
- ✅ **Multiple consumers** all receive latest value
- ⚠️  **No history** - intermediate values are lost

#### When to Use

```
⚙️  Configuration Management
   → Application settings updated from control plane
   → Only current config matters
   → Intermediate updates can be skipped

🎛️  State Synchronization
   → Device status (online/offline)
   → Latest measurement from slow sensor
   → UI state updates

🌡️  Infrequent Updates with Fast Production
   → Temperature setpoint changes
   → User preference updates
   → Feature flag toggles
```

#### Configuration

**Tokio**:
```rust
let buffer = TokioBuffer::<ConfigData>::new(
    &BufferCfg::SingleLatest
);

let reader = buffer.subscribe();
```

**Embassy**:
```rust
// Const generics: capacity not used for Watch
type ConfigBuffer = EmbassyBuffer<ConfigData, 1, 4, 1, 4>;

static BUFFER: StaticCell<ConfigBuffer> = StaticCell::new();
let buffer = BUFFER.init(ConfigBuffer::new_watch());
```

#### Consumer Pattern

```rust
// Consumer automatically skips intermediate values
loop {
    match reader.recv().await {
        Ok(config) => {
            // Always the latest config
            apply_config(config).await;
            // If 10 updates happened during apply_config(),
            // next recv() returns the 10th update (skipping 1-9)
        }
        Err(DbError::BufferClosed { .. }) => break,
        Err(e) => {
            log::error!("Config error: {:?}", e);
            break;
        }
    }
}
```

---

### 3. Mailbox

**Pattern**: Single-slot with overwrite  
**Semantics**: Latest unread message  
**Best For**: Commands, control messages, RPC-style communication

#### Characteristics

- ✅ **Single slot** - minimal memory overhead
- ✅ **Overwrite semantics** - latest command wins
- ✅ **Best for commands** where only latest matters
- ✅ **Backpressure** through overwrite (no blocking)
- ⚠️  **Single consumer** recommended
- ⚠️  **Messages may be overwritten** before reading

#### When to Use

```
🎮 Device Control Commands
   → Start/stop operations
   → Mode changes (auto/manual)
   → Latest command overwrites pending commands

🔧 Actuator Control
   → Motor speed setpoints
   → Valve positions
   → LED brightness
   → Only latest command matters

📨 RPC-Style Messaging
   → One request in-flight at a time
   → Cancel pending request with new one
   → Minimal memory footprint
```

#### Configuration

**Tokio**:
```rust
let buffer = TokioBuffer::<Command>::new(
    &BufferCfg::Mailbox
);

let reader = buffer.subscribe();
```

**Embassy**:
```rust
// Const generics: capacity=1 for mailbox
type CommandBuffer = EmbassyBuffer<Command, 1, 4, 1, 1>;

static BUFFER: StaticCell<CommandBuffer> = StaticCell::new();
let buffer = BUFFER.init(CommandBuffer::new_mailbox());
```

#### Consumer Pattern

```rust
loop {
    match reader.recv().await {
        Ok(cmd) => {
            // Execute latest command
            execute_command(cmd).await;
            // If multiple commands sent during execute,
            // only the latest is in the mailbox
        }
        Err(DbError::BufferClosed { .. }) => break,
        Err(e) => {
            log::error!("Command error: {:?}", e);
            break;
        }
    }
}
```

---

## Choosing the Right Buffer

| Requirement | SPMC Ring | SingleLatest | Mailbox |
|-------------|-----------|--------------|---------|
| **Multiple consumers** | ✅ Yes | ✅ Yes | ⚠️  Possible but not recommended |
| **All messages delivered** | ✅ To fast consumers | ❌ Latest only | ❌ Latest only |
| **Memory bounded** | ✅ Fixed capacity | ✅ Single slot | ✅ Single slot |
| **Backpressure** | ✅ Lag detection | ✅ Skipping | ✅ Overwrite |
| **Best for high-frequency** | ✅ Yes | ❌ No | ❌ No |
| **Best for state updates** | ❌ No | ✅ Yes | ✅ Yes |
| **Best for commands** | ❌ No | ⚠️  Acceptable | ✅ Yes |

### Decision Tree

```
Do you need every message delivered?
├─ YES: Use SPMC Ring (with appropriate capacity)
│   └─ Can consumers lag? → Configure capacity for acceptable lag
│
└─ NO: Only latest value matters
    │
    ├─ Multiple consumers need latest?
    │   └─ YES: Use SingleLatest
    │
    └─ Single consumer, minimal memory?
        └─ YES: Use Mailbox
```

---

## Advanced Patterns

### Dispatcher Pattern

Both `TokioBuffer` and `EmbassyBuffer` support spawning background tasks that automatically drain the buffer and call a handler function.

**Tokio**:
```rust
use std::sync::Arc;

let buffer = Arc::new(TokioBuffer::<Event>::new(
    &BufferCfg::SpmcRing { capacity: 50 }
));

// Spawn dispatcher that processes all events
let handle = buffer.spawn_dispatcher(|event| async move {
    process_event(event).await;
});

// Buffer is drained automatically in background
// Handle can be used to cancel or await completion
```

**Embassy**:
```rust
#[embassy_executor::task]
async fn event_dispatcher(buffer: &'static EventBuffer) {
    buffer.dispatcher_task(|event| async move {
        process_event(event).await;
    }).await;
}

// In main:
spawner.spawn(event_dispatcher(buffer).expect("spawn failed"));
```

### Hybrid Patterns

For complex scenarios, use multiple buffers:

```rust
// High-frequency telemetry: SPMC Ring
let telemetry = TokioBuffer::<Telemetry>::new(
    &BufferCfg::SpmcRing { capacity: 200 }
);

// Configuration updates: SingleLatest
let config = TokioBuffer::<Config>::new(
    &BufferCfg::SingleLatest
);

// Control commands: Mailbox
let commands = TokioBuffer::<Command>::new(
    &BufferCfg::Mailbox
);
```

---

## Performance Considerations

### Memory Usage

| Buffer Type | Memory per Buffer | Notes |
|-------------|-------------------|-------|
| **SPMC Ring** | `capacity × sizeof(T)` | Plus overhead for sync primitives |
| **SingleLatest** | `sizeof(T)` | One slot only |
| **Mailbox** | `sizeof(T)` | One slot only |

### Latency Characteristics

- **SPMC Ring**: ~50-200ns per message (depends on capacity)
- **SingleLatest**: ~30-100ns per update (skipping is free)
- **Mailbox**: ~30-100ns per command (overwrite is immediate)

### Embedded Constraints

Embassy buffers require **const generics** for capacity, which must be known at compile time:

```rust
// ❌ Cannot do this in Embassy (runtime capacity)
let buffer = EmbassyBuffer::new(&BufferCfg::SpmcRing { 
    capacity: user_input 
});

// ✅ Must use const generics
type MyBuffer = EmbassyBuffer<T, 100, 4, 1, 1>; // Capacity is 100
```

This is a fundamental constraint of `no_std` environments where heap allocation may not be available.

---

## Best Practices

### ✅ Do

1. **Choose capacity based on producer/consumer speed ratio**
   ```rust
   // Producer: 100 Hz, Consumer: 50 Hz → Need ~2 seconds buffer
   capacity = 100 Hz × 2 sec = 200
   ```

2. **Handle lag errors gracefully**
   ```rust
   Err(DbError::BufferLagged { lag_count, .. }) => {
       metrics.increment_lag_events();
       log::warn!("Lagged by {}", lag_count);
       continue; // Keep processing
   }
   ```

3. **Use SingleLatest for state synchronization**
   ```rust
   // ✅ Good: Config only needs latest value
   let config_buffer = TokioBuffer::new(&BufferCfg::SingleLatest);
   ```

4. **Use Mailbox for commands where order doesn't matter**
   ```rust
   // ✅ Good: Only execute latest command
   let cmd_buffer = TokioBuffer::new(&BufferCfg::Mailbox);
   ```

### ❌ Don't

1. **Don't use SPMC Ring for infrequent updates**
   ```rust
   // ❌ Bad: Wastes memory for rare config updates
   let config = TokioBuffer::new(&BufferCfg::SpmcRing { 
       capacity: 1000 
   });
   
   // ✅ Good: Use SingleLatest instead
   let config = TokioBuffer::new(&BufferCfg::SingleLatest);
   ```

2. **Don't ignore lag errors in critical pipelines**
   ```rust
   // ❌ Bad: Silent data loss
   Err(DbError::BufferLagged { .. }) => {
       continue; // No logging or metrics!
   }
   ```

3. **Don't use Mailbox for ordered message sequences**
   ```rust
   // ❌ Bad: Messages may be lost/reordered
   for msg in messages {
       mailbox.push(msg);
   }
   
   // ✅ Good: Use SPMC Ring for sequences
   let buffer = TokioBuffer::new(&BufferCfg::SpmcRing { 
       capacity: messages.len() * 2 
   });
   ```

---

## Examples

See working examples in the repository:

- **Tokio Demo**: `examples/tokio-runtime-demo/src/main.rs`
  - Demonstrates all three buffer types
  - Shows lag handling, skipping, and overwrite behavior
  
- **Embassy Demo**: `examples/embassy-runtime-demo/src/main.rs`
  - no_std implementation for STM32
  - Static buffer allocation with const generics
  
- **Shared Services**: `examples/shared/src/lib.rs`
  - Runtime-agnostic consumer/producer services
  - Works on both Tokio and Embassy

---

## Testing

All buffer implementations include comprehensive tests:

```bash
# Run Tokio buffer tests (17 tests)
cargo test --package aimdb-tokio-adapter --lib buffer

# Tests cover:
# - Basic operations (push, subscribe, recv)
# - Multiple consumers
# - Lag detection and recovery
# - Intermediate value skipping (SingleLatest)
# - Overwrite semantics (Mailbox)
# - Dispatcher integration
```

---

## Future Enhancements

Planned improvements for future milestones:

- **Dynamic capacity adjustment** (runtime resize for SPMC)
- **Priority queues** (high/low priority messages)
- **Filtering** (subscribe with predicate)
- **Batching** (receive multiple messages at once)
- **Metrics integration** (automatic lag/overwrite tracking)

---

## Related Documentation

- [Design Document: Pluggable Buffers](./002-M1_pluggable_buffers.md)
- [Architecture Overview](../vision/future_usage.md)
- [Runtime Integration](./001-M1_runtime_integration.md)
