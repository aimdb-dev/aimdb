# Buffer Statistics and Metrics Infrastructure - Design Document

**Version:** 1.0  
**Status:** Design Document (Future Work)  
**Created:** November 5, 2025  
**Related Issue:** TBD (Post-#43)  
**Dependencies:** None (core infrastructure)

---

## Executive Summary

This document describes a comprehensive **buffer statistics infrastructure** for AimDB core and runtime adapters. This is prerequisite work for meaningful operational metrics and monitoring.

**Scope:** Core `aimdb-core` and adapter crates (`aimdb-tokio-adapter`, `aimdb-embassy-adapter`)

**NOT in scope:** MCP server metrics resource (depends on this work)

### Current Gap

The current AimDB architecture has **no runtime buffer statistics**:
- ❌ `Buffer<T>` trait has only `push()` and `subscribe()` - no stats methods
- ❌ No tracking of buffer fill levels, push/pull counts, or drops
- ❌ `RecordMetadata` contains only static configuration (capacity, type)
- ❌ No instrumentation at the buffer layer

**Impact:** Cannot monitor buffer pressure, detect consumer lag, or identify bottlenecks.

### Solution Overview

Add opt-in buffer statistics tracking to:
1. Buffer trait definitions (`aimdb-core`)
2. Concrete buffer implementations (adapter crates)
3. `RecordMetadata` protocol type
4. `TypedRecord` metadata collection

**Key features:**
- Atomic counters for pushes, pulls, drops
- Active reader tracking
- Buffer utilization calculations
- Opt-in via `buffer-stats` feature flag (<10ns overhead)

### Estimated Effort

**5-7 days** for complete implementation across core and adapters.

---

## Table of Contents

- [Problem Statement](#problem-statement)
- [Architecture Overview](#architecture-overview)
- [Core Design](#core-design)
- [Adapter Implementation](#adapter-implementation)
- [Protocol Changes](#protocol-changes)
- [Implementation Challenges](#implementation-challenges)
- [Testing Strategy](#testing-strategy)
- [Performance Considerations](#performance-considerations)
- [Future Enhancements](#future-enhancements)

---

## Problem Statement

### Current State

**What's missing:**
```rust
// Current Buffer trait - NO statistics methods
pub trait Buffer<T: Clone + Send>: Send + Sync + 'static {
    type Reader: BufferReader<T> + 'static;
    fn new(cfg: &BufferCfg) -> Self;
    fn push(&self, value: T);              // No counters
    fn subscribe(&self) -> Self::Reader;   // No reader tracking
}
```

**What we can't measure:**
- Current buffer fill level (how many messages queued?)
- Push/pull throughput (operations per second)
- Consumer lag (how far behind is each reader?)
- Drop rate (overflow frequency in ring buffers)
- Active subscriptions (how many readers alive?)

### Use Cases

**1. Buffer Pressure Detection**
```
Alert: Temperature buffer at 98% capacity
       Consumers falling behind, risk of data loss
Action: Increase buffer size or add consumers
```

**2. Performance Monitoring**
```
Metrics: 1000 pushes/sec, 850 pulls/sec
Status:  Consumer lag increasing (150 msg/sec deficit)
Action:  Scale up consumers or reduce producer rate
```

**3. Resource Management**
```
Status: 5 records, 12 active subscriptions
        Total memory: ~480KB (40KB per subscription)
Action: Within limits, system healthy
```

**4. Anomaly Detection**
```
Alert: Config record has 0 producers
Status: Last update 2 hours ago
Action: Verify producer service is running
```

---

## Architecture Overview

### Component Diagram

```
┌─────────────────────────────────────────────────────────┐
│                     aimdb-core                          │
│                                                         │
│  ┌──────────────────┐      ┌───────────────────┐      │
│  │  BufferStats     │      │  DynBuffer trait  │      │
│  │   (struct)       │◄─────│  + stats() method │      │
│  └──────────────────┘      └───────────────────┘      │
│                                                         │
│  ┌──────────────────┐      ┌───────────────────┐      │
│  │ RecordMetadata   │      │   TypedRecord     │      │
│  │ + buffer_stats   │◄─────│  collect_stats()  │      │
│  └──────────────────┘      └───────────────────┘      │
└─────────────────────────────────────────────────────────┘
                             │
              ┌──────────────┴──────────────┐
              ▼                             ▼
    ┌──────────────────┐        ┌──────────────────┐
    │ aimdb-tokio-     │        │ aimdb-embassy-   │
    │   adapter        │        │   adapter        │
    │                  │        │                  │
    │  TokioRingBuffer │        │ EmbassyRing-     │
    │  + AtomicU64     │        │   Buffer         │
    │    counters      │        │  + atomic stats  │
    └──────────────────┘        └──────────────────┘
```

### Data Flow

**Statistics Collection:**
1. Buffer increments atomic counters on each operation
2. Readers tracked via `Arc<AtomicUsize>` (increment on create, decrement on drop)
3. `TypedRecord::collect_metadata()` calls `buffer.stats()`
4. Stats included in `RecordMetadata` response
5. MCP server (future) consumes stats via `record.list`

---

## Core Design

### 1. BufferStats Struct

```rust
// In aimdb-core/src/buffer/stats.rs

/// Statistics snapshot for a buffer
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct BufferStats {
    /// Total values pushed since creation
    pub total_pushes: u64,
    
    /// Total values pulled by all consumers (sum across readers)
    /// Note: May be unavailable for some buffer types
    pub total_pulls: Option<u64>,
    
    /// Current buffer fill level (for bounded buffers)
    /// None for unbounded or single-value buffers
    pub current_size: Option<usize>,
    
    /// Buffer capacity (for bounded buffers)
    pub capacity: Option<usize>,
    
    /// Number of active subscriptions/readers
    pub active_readers: usize,
    
    /// Number of values dropped due to overflow (SPMC ring only)
    pub dropped_count: u64,
}

impl BufferStats {
    /// Calculate buffer utilization percentage (0-100)
    pub fn utilization_percent(&self) -> Option<f64> {
        match (self.current_size, self.capacity) {
            (Some(size), Some(cap)) if cap > 0 => {
                Some((size as f64 / cap as f64) * 100.0)
            }
            _ => None,
        }
    }
    
    /// Check if buffer is near capacity (>80%)
    pub fn is_high_utilization(&self) -> bool {
        self.utilization_percent()
            .map(|p| p > 80.0)
            .unwrap_or(false)
    }
    
    /// Check if buffer is critical (>95%)
    pub fn is_critical_utilization(&self) -> bool {
        self.utilization_percent()
            .map(|p| p > 95.0)
            .unwrap_or(false)
    }
    
    /// Calculate push rate (if duration provided)
    pub fn push_rate(&self, duration: Duration) -> f64 {
        self.total_pushes as f64 / duration.as_secs_f64()
    }
}
```

### 2. Extended DynBuffer Trait

```rust
// In aimdb-core/src/buffer/traits.rs

pub trait DynBuffer<T: Clone + Send>: Send + Sync {
    fn push(&self, value: T);
    fn subscribe_boxed(&self) -> Box<dyn BufferReader<T> + Send>;
    fn as_any(&self) -> &dyn core::any::Any;
    
    /// Get current buffer statistics (opt-in)
    ///
    /// Returns None if buffer doesn't implement statistics tracking.
    /// Enable via `buffer-stats` feature flag to reduce overhead.
    fn stats(&self) -> Option<BufferStats> {
        None  // Default: no stats available
    }
}
```

### 3. Feature Flag

```toml
# In aimdb-core/Cargo.toml

[features]
default = ["std"]
std = ["serde/std"]

# Enable runtime buffer statistics tracking
# Adds ~5-10ns overhead per push/subscribe operation
buffer-stats = []
```

---

## Adapter Implementation

### Tokio SPMC Ring Buffer

```rust
// In aimdb-tokio-adapter/src/buffer.rs

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;

pub struct TokioRingBuffer<T: Clone + Send> {
    tx: broadcast::Sender<T>,
    capacity: usize,
    
    #[cfg(feature = "buffer-stats")]
    stats: BufferStatsInner,
}

#[cfg(feature = "buffer-stats")]
struct BufferStatsInner {
    total_pushes: AtomicU64,
    dropped_count: AtomicU64,
    active_readers: Arc<AtomicUsize>,
}

impl<T: Clone + Send + 'static> Buffer<T> for TokioRingBuffer<T> {
    type Reader = TokioRingBufferReader<T>;
    
    fn new(cfg: &BufferCfg) -> Self {
        let capacity = cfg.capacity.unwrap_or(100);
        let (tx, _) = broadcast::channel(capacity);
        
        Self {
            tx,
            capacity,
            #[cfg(feature = "buffer-stats")]
            stats: BufferStatsInner {
                total_pushes: AtomicU64::new(0),
                dropped_count: AtomicU64::new(0),
                active_readers: Arc::new(AtomicUsize::new(0)),
            },
        }
    }
    
    fn push(&self, value: T) {
        #[cfg(feature = "buffer-stats")]
        self.stats.total_pushes.fetch_add(1, Ordering::Relaxed);
        
        match self.tx.send(value) {
            Ok(_) => {},
            Err(_) => {
                // No active receivers - normal during startup
                // Don't count as drop
            }
        }
    }
    
    fn subscribe(&self) -> Self::Reader {
        #[cfg(feature = "buffer-stats")]
        self.stats.active_readers.fetch_add(1, Ordering::Relaxed);
        
        let rx = self.tx.subscribe();
        
        TokioRingBufferReader {
            rx,
            #[cfg(feature = "buffer-stats")]
            parent_stats: Arc::clone(&self.stats.active_readers),
        }
    }
}

impl<T: Clone + Send + 'static> DynBuffer<T> for TokioRingBuffer<T> {
    fn push(&self, value: T) {
        <Self as Buffer<T>>::push(self, value)
    }
    
    fn subscribe_boxed(&self) -> Box<dyn BufferReader<T> + Send> {
        Box::new(self.subscribe())
    }
    
    fn as_any(&self) -> &dyn core::any::Any {
        self
    }
    
    #[cfg(feature = "buffer-stats")]
    fn stats(&self) -> Option<BufferStats> {
        Some(BufferStats {
            total_pushes: self.stats.total_pushes.load(Ordering::Relaxed),
            total_pulls: None, // Not tracked by broadcast channel
            current_size: None, // Not exposed by tokio::sync::broadcast
            capacity: Some(self.capacity),
            active_readers: self.stats.active_readers.load(Ordering::Relaxed),
            dropped_count: self.stats.dropped_count.load(Ordering::Relaxed),
        })
    }
}

pub struct TokioRingBufferReader<T: Clone> {
    rx: broadcast::Receiver<T>,
    
    #[cfg(feature = "buffer-stats")]
    parent_stats: Arc<AtomicUsize>,
}

impl<T: Clone> Drop for TokioRingBufferReader<T> {
    fn drop(&mut self) {
        #[cfg(feature = "buffer-stats")]
        self.parent_stats.fetch_sub(1, Ordering::Relaxed);
    }
}

impl<T: Clone + Send> BufferReader<T> for TokioRingBufferReader<T> {
    fn recv(&mut self) -> Pin<Box<dyn Future<Output = Result<T, DbError>> + Send + '_>> {
        Box::pin(async move {
            match self.rx.recv().await {
                Ok(value) => Ok(value),
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    // Track drops
                    #[cfg(feature = "buffer-stats")]
                    self.parent_stats.fetch_add(n as usize, Ordering::Relaxed);
                    
                    Err(DbError::BufferLagged(n))
                }
                Err(broadcast::error::RecvError::Closed) => {
                    Err(DbError::BufferClosed)
                }
            }
        })
    }
}
```

---

## Protocol Changes

### RecordMetadata Extension

```rust
// In aimdb-core/src/remote/metadata.rs

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecordMetadata {
    pub name: String,
    pub type_id: String,
    pub buffer_type: String,
    pub buffer_capacity: Option<usize>,
    pub producer_count: usize,
    pub consumer_count: usize,
    pub writable: bool,
    pub created_at: String,
    pub last_update: Option<String>,
    pub connector_count: usize,
    
    /// Runtime buffer statistics (available when buffer-stats feature enabled)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub buffer_stats: Option<BufferStatsSummary>,
}

/// Summary of buffer statistics for protocol responses
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BufferStatsSummary {
    /// Current buffer fill level
    pub current_size: Option<usize>,
    
    /// Buffer utilization percentage (0-100)
    pub utilization_percent: Option<f64>,
    
    /// Total operations since creation
    pub total_pushes: u64,
    
    /// Total pulls across all consumers (if available)
    pub total_pulls: Option<u64>,
    
    /// Number of active subscriptions
    pub active_readers: usize,
    
    /// Values dropped (SPMC ring overflow)
    pub dropped_count: u64,
}
```

### TypedRecord Integration

```rust
// In aimdb-core/src/typed_record.rs

impl<T, R> TypedRecord<T, R> {
    pub fn collect_metadata(&self, type_id: TypeId) -> RecordMetadata {
        let mut metadata = RecordMetadata::new(
            type_id,
            self.name.clone(),
            self.buffer_type_str(),
            self.buffer_capacity(),
            self.producer_count,
            self.consumer_count,
            self.writable,
            self.created_at.clone(),
            self.connector_count,
        );
        
        // Add runtime statistics if available
        if let Some(buf) = &self.buffer {
            if let Some(stats) = buf.stats() {
                metadata.buffer_stats = Some(BufferStatsSummary {
                    current_size: stats.current_size,
                    utilization_percent: stats.utilization_percent(),
                    total_pushes: stats.total_pushes,
                    total_pulls: stats.total_pulls,
                    active_readers: stats.active_readers,
                    dropped_count: stats.dropped_count,
                });
            }
        }
        
        metadata
    }
}
```

---

## Implementation Challenges

### Challenge 1: tokio::sync::broadcast Limitations

**Problem:** `broadcast` channel doesn't expose current queue depth.

**Solutions:**
1. **Manual tracking** - Use atomic counter (complex, race conditions)
2. **Alternative queue** - Use `crossbeam-channel` or custom impl with `len()`
3. **Accept limitation** - Report `current_size: None` for broadcast buffers
4. **Wrapper layer** - Instrument broadcast with tracking shim

**Recommendation:** Option 3 short-term (document limitation), evaluate Option 2 long-term.

### Challenge 2: Performance Overhead

**Problem:** Atomic operations add ~5-10ns per push/subscribe.

**Solution:** Opt-in via feature flag:
- Default: No overhead, `stats()` returns `None`
- With `buffer-stats`: Full tracking enabled
- Benchmark: <5% overhead for typical workloads

### Challenge 3: Embassy Support

**Problem:** Embassy uses different async primitives (no-std).

**Solution:** Similar approach but with `no_std`-compatible atomics:
```rust
#[cfg(not(feature = "std"))]
use core::sync::atomic::{AtomicU64, AtomicUsize};
```

### Challenge 4: Cross-Platform Atomics

**Problem:** Some embedded platforms lack 64-bit atomics.

**Solution:** Use conditional compilation:
```rust
#[cfg(target_has_atomic = "64")]
type CounterType = AtomicU64;

#[cfg(not(target_has_atomic = "64"))]
type CounterType = AtomicU32;  // Saturate at u32::MAX
```

---

## Testing Strategy

### Unit Tests

```rust
#[cfg(all(test, feature = "buffer-stats"))]
mod stats_tests {
    use super::*;

    #[tokio::test]
    async fn test_push_counter() {
        let cfg = BufferCfg::spmc_ring(10);
        let buffer = TokioRingBuffer::<i32>::new(&cfg);
        
        buffer.push(1);
        buffer.push(2);
        buffer.push(3);
        
        let stats = buffer.stats().unwrap();
        assert_eq!(stats.total_pushes, 3);
    }

    #[tokio::test]
    async fn test_active_readers() {
        let cfg = BufferCfg::spmc_ring(10);
        let buffer = TokioRingBuffer::<i32>::new(&cfg);
        
        let stats = buffer.stats().unwrap();
        assert_eq!(stats.active_readers, 0);
        
        let _reader1 = buffer.subscribe();
        let stats = buffer.stats().unwrap();
        assert_eq!(stats.active_readers, 1);
        
        let _reader2 = buffer.subscribe();
        let stats = buffer.stats().unwrap();
        assert_eq!(stats.active_readers, 2);
        
        drop(_reader1);
        let stats = buffer.stats().unwrap();
        assert_eq!(stats.active_readers, 1);
    }

    #[tokio::test]
    async fn test_utilization_calculation() {
        let stats = BufferStats {
            current_size: Some(98),
            capacity: Some(100),
            total_pushes: 1000,
            total_pulls: None,
            active_readers: 3,
            dropped_count: 10,
        };
        
        assert_eq!(stats.utilization_percent(), Some(98.0));
        assert!(stats.is_critical_utilization());
        assert!(stats.is_high_utilization());
    }
}
```

### Integration Tests

```rust
#[cfg(all(test, feature = "buffer-stats"))]
#[tokio::test]
async fn test_record_metadata_with_stats() {
    let db = AimDbBuilder::new()
        .register::<i32>(|b| b.name("test::Counter").with_buffer_spmc_ring(10))
        .build_tokio()
        .await
        .unwrap();
    
    // Push some values
    db.produce(1).await.unwrap();
    db.produce(2).await.unwrap();
    
    // Subscribe
    let _reader = db.subscribe::<i32>().unwrap();
    
    // Collect metadata
    let metadata = db.inner_aimdb().list_records();
    let counter_meta = metadata.iter().find(|m| m.name == "test::Counter").unwrap();
    
    // Verify stats
    let stats = counter_meta.buffer_stats.as_ref().unwrap();
    assert_eq!(stats.total_pushes, 2);
    assert_eq!(stats.active_readers, 1);
}
```

---

## Performance Considerations

### Overhead Analysis

**Without `buffer-stats` feature:**
- Zero overhead
- `stats()` returns `None` (inline to empty function)

**With `buffer-stats` feature:**
- Push: 1 atomic increment (~5ns)
- Subscribe: 1 atomic increment (~5ns)
- Drop: 1 atomic decrement (~5ns)
- Stats query: 3-5 atomic loads (~15-25ns)

**Total overhead: <5% for typical workloads**

### Benchmark Results (Expected)

```
Buffer::push (no stats):     ~50ns
Buffer::push (with stats):   ~55ns (+10%)

Buffer::subscribe (no stats): ~100ns
Buffer::subscribe (with stats): ~105ns (+5%)

stats() call:                ~20ns
```

### Memory Overhead

Per buffer instance with stats:
- `total_pushes`: 8 bytes
- `dropped_count`: 8 bytes
- `active_readers`: 8 bytes (Arc)
- **Total: 24 bytes** (negligible)

---

## Future Enhancements

### 1. Histogram Metrics

Track latency distribution for push/pull operations:
```rust
pub struct BufferStats {
    // ... existing fields ...
    pub push_latency_p50: Option<Duration>,
    pub push_latency_p99: Option<Duration>,
}
```

### 2. Consumer-Specific Stats

Track per-reader metrics:
```rust
pub struct ReaderStats {
    pub reader_id: usize,
    pub total_pulls: u64,
    pub lag_messages: usize,
    pub last_pull_at: SystemTime,
}
```

### 3. Time-Series Metrics

Store historical stats for trend analysis:
```rust
pub struct BufferTimeSeries {
    pub samples: VecDeque<(SystemTime, BufferStats)>,
    pub max_samples: usize,
}
```

### 4. Prometheus Integration

Export stats in Prometheus format:
```
aimdb_buffer_pushes_total{record="server::Temperature"} 1523
aimdb_buffer_active_readers{record="server::Temperature"} 3
aimdb_buffer_utilization_percent{record="server::Temperature"} 45.2
```

---

## Implementation Checklist

### Phase 1: Core (aimdb-core)
- [ ] Create `src/buffer/stats.rs` with `BufferStats` struct
- [ ] Add `stats()` method to `DynBuffer` trait
- [ ] Add `buffer-stats` feature flag to `Cargo.toml`
- [ ] Extend `RecordMetadata` with `buffer_stats` field
- [ ] Create `BufferStatsSummary` type
- [ ] Update `TypedRecord::collect_metadata()` to include stats
- [ ] Write unit tests for `BufferStats` calculations

### Phase 2: Tokio Adapter (aimdb-tokio-adapter)
- [ ] Add atomic counters to `TokioRingBuffer`
- [ ] Instrument `push()` with counter increment
- [ ] Instrument `subscribe()` with reader tracking
- [ ] Implement `Drop` for reader to decrement counter
- [ ] Implement `stats()` method
- [ ] Write unit tests for stats tracking
- [ ] Write integration tests with real buffers

### Phase 3: Embassy Adapter (aimdb-embassy-adapter)
- [ ] Add no-std-compatible atomic counters
- [ ] Instrument buffer operations
- [ ] Implement `stats()` method
- [ ] Test on embedded target (cross-compile)

### Phase 4: Documentation & Testing
- [ ] Update core README with `buffer-stats` feature
- [ ] Document performance characteristics
- [ ] Add benchmarks for overhead measurement
- [ ] Integration tests with multiple buffer types
- [ ] Examples demonstrating stats usage

---

## Dependencies

**None** - This is foundational infrastructure.

**Enables:**
- MCP metrics resource (future issue)
- Monitoring dashboards
- Alerting systems
- Performance analysis tools

---

**End of Document**
