# RFC: Optional Buffer Introspection Metrics

> **Status**: ✅ Implemented  
> **Target**: `aimdb-core` (feature-flagged)  
> **Priority**: Medium  
> **Implementation Date**: November 2025  
> **Feature Flag**: `metrics`

---

## Summary

Add optional buffer-level metrics to AimDB that expose operational data already known internally (or cheaply trackable). This enables users to diagnose producer-consumer imbalances without external instrumentation.

---

## Background

A user building a ROS2 camera streaming pipeline reported spending 30+ minutes debugging a throughput bottleneck that proper metrics would have surfaced in seconds.

This RFC proposes a **minimal, focused scope** that:
1. Exposes data AimDB already tracks or can track cheaply
2. Doesn't dictate application-level patterns
3. Integrates naturally with existing MCP introspection
4. Has zero cost when disabled

---

## Current State Analysis

### What's Already Tracked

| Data | Location | Status |
|------|----------|--------|
| `producer_count` | `RecordMetadata` | ✅ Exposed via MCP |
| `consumer_count` | `RecordMetadata` | ✅ Exposed via MCP |
| `created_at` | `RecordMetadataTracker` | ✅ Exposed via MCP |
| `last_update` | `RecordMetadataTracker` | ✅ Exposed via MCP |
| `buffer_type` | `RecordMetadata` | ✅ Exposed via MCP |
| `buffer_capacity` | `RecordMetadata` | ✅ Exposed via MCP |
| `dropped` count | Protocol placeholder | 🚧 Field exists but `TODO: Implement` |

### What's NOT Tracked (Gaps)

| Data | Difficulty | Value |
|------|------------|-------|
| `produced_count` (total items pushed) | Easy - atomic counter | High |
| `consumed_count` (total items received) | Easy - atomic counter | High |
| `dropped_count` (overflow/lag events) | Medium - per-buffer type | High |
| `current_occupancy` | Medium - buffer-specific | Medium |
| `throughput_estimate` (items/sec) | Medium - sliding window | High |

---

## Proposed Design

### 1. New Trait: `BufferMetrics` (in `aimdb-core`)

```rust
/// Optional metrics for buffer introspection
/// 
/// Implemented by buffer types when the `metrics` feature is enabled.
/// All methods have zero-cost default implementations when disabled.
#[cfg(feature = "metrics")]
pub trait BufferMetrics {
    /// Total items pushed to this buffer since creation
    fn produced_count(&self) -> u64 { 0 }
    
    /// Total items successfully consumed from this buffer
    fn consumed_count(&self) -> u64 { 0 }
    
    /// Total items dropped due to overflow/lag (SPMC ring only)
    fn dropped_count(&self) -> u64 { 0 }
    
    /// Current buffer occupancy: (items_in_buffer, capacity)
    /// Returns (0, 0) for SingleLatest/Mailbox
    fn occupancy(&self) -> (usize, usize) { (0, 0) }
    
    /// Reset all counters (useful for windowed metrics)
    fn reset_metrics(&self) {}
}
```

### 2. Extend `RecordMetadata` (optional fields)

Add new optional fields to the existing `RecordMetadata` struct:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecordMetadata {
    // ... existing fields ...
    
    /// Total items produced (if metrics enabled)
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg(feature = "metrics")]
    pub produced_count: Option<u64>,
    
    /// Total items consumed (if metrics enabled)
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg(feature = "metrics")]
    pub consumed_count: Option<u64>,
    
    /// Items dropped due to overflow (if metrics enabled)
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg(feature = "metrics")]
    pub dropped_count: Option<u64>,
    
    /// Current buffer occupancy (items, capacity) - None if not applicable
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg(feature = "metrics")]
    pub occupancy: Option<(usize, usize)>,
}
```

### 3. Implementation in Adapters

#### Tokio Adapter (`aimdb-tokio-adapter`)

```rust
#[cfg(feature = "metrics")]
struct BufferMetricsInner {
    produced: AtomicU64,
    consumed: AtomicU64,
    dropped: AtomicU64,
}

impl<T: Clone + Send + Sync + 'static> TokioBuffer<T> {
    #[cfg(feature = "metrics")]
    fn increment_produced(&self) {
        if let Some(metrics) = &self.metrics {
            metrics.produced.fetch_add(1, Ordering::Relaxed);
        }
    }
    
    // Called in push()
    fn push(&self, value: T) {
        #[cfg(feature = "metrics")]
        self.increment_produced();
        
        match &*self.inner {
            // ... existing implementation ...
        }
    }
}
```

#### Embassy Adapter (`aimdb-embassy-adapter`)

Same pattern but using `portable-atomic` for no_std support:

```rust
#[cfg(feature = "metrics")]
use portable_atomic::{AtomicU64, Ordering};
```

### 4. MCP Integration

Extend existing MCP tools to include new metrics when available:

```json
// list_records response with metrics enabled
{
  "name": "sensor::Temperature",
  "buffer_type": "spmc_ring",
  "buffer_capacity": 1024,
  "producer_count": 1,
  "consumer_count": 2,
  "produced_count": 15420,
  "consumed_count": 15380,
  "dropped_count": 40,
  "occupancy": [12, 1024]
}
```

Add a new MCP tool for metrics-specific queries:

```
mcp_aimdb_get_buffer_metrics(socket_path, record_name)
```

Returns:
```json
{
  "produced_count": 15420,
  "consumed_count": 15380,
  "dropped_count": 40,
  "occupancy": [12, 1024],
  "saturation_ratio": 1.003,
  "drop_rate_percent": 0.26
}
```

---

## Feature Flag Strategy

```toml
# aimdb-core/Cargo.toml
[features]
default = []
metrics = []  # Opt-in buffer metrics

# aimdb-tokio-adapter/Cargo.toml
[features]
metrics = ["aimdb-core/metrics"]

# aimdb-embassy-adapter/Cargo.toml
metrics = ["aimdb-core/metrics", "portable-atomic"]
```

**Rationale**: Users who don't need metrics pay zero cost. Those who do can enable with a single feature flag.

---

## Implementation Plan

### Phase 1: Core Infrastructure (Day 1-2)

1. [ ] Add `BufferMetrics` trait to `aimdb-core/src/buffer/traits.rs`
2. [ ] Add `metrics` feature flag to `aimdb-core/Cargo.toml`
3. [ ] Extend `RecordMetadata` with optional metrics fields
4. [ ] Unit tests for trait with metrics enabled/disabled

### Phase 2: Tokio Adapter (Day 2-3)

1. [ ] Add `BufferMetricsInner` struct with atomic counters
2. [ ] Implement `BufferMetrics` for `TokioBuffer`
3. [ ] Instrument `push()` to increment produced counter
4. [ ] Instrument `TokioBufferReader::recv()` to increment consumed counter
5. [ ] Track dropped count in SPMC ring lag detection
6. [ ] Integration tests with metrics enabled

### Phase 3: Embassy Adapter (Day 3-4)

1. [ ] Add `portable-atomic` dependency (conditional on metrics)
2. [ ] Implement `BufferMetrics` for `EmbassyBuffer`
3. [ ] Same instrumentation as Tokio adapter
4. [ ] Verify no_std compatibility

### Phase 4: MCP Integration (Day 4-5)

1. [ ] Update `collect_metadata()` to include metrics when available
2. [ ] Add `get_buffer_metrics` MCP tool
3. [ ] Update MCP documentation
4. [ ] End-to-end test with remote-access-demo

---

## What's NOT In Scope

| Feature | Reason |
|---------|--------|
| `StageProfiler` | Application-level concern; use `tracing` crate |
| `PipelineMonitor` | Application-level composition pattern |
| `LatencyHistogram` | Complex, can add later if needed |
| Throughput calculation | Can be derived from produced_count + timestamps |

Users can build higher-level monitoring on top of the raw metrics we expose.

---

## Success Criteria

After implementation, users should be able to:

1. **Diagnose saturation**: `produced_count >> consumed_count` indicates slow consumer
2. **Detect drops**: `dropped_count > 0` indicates overflow
3. **Monitor occupancy**: `occupancy` shows buffer fill level
4. **Use MCP tools**: Query metrics via existing introspection infrastructure

Example diagnosis from MCP:

```
$ mcp_aimdb_get_buffer_metrics(socket, "camera::Frame")

{
  "produced_count": 900,      # 30 FPS × 30 seconds
  "consumed_count": 360,      # Only 12 FPS consumed
  "dropped_count": 540,       # 60% dropped!
  "occupancy": [1024, 1024],  # Buffer full
  "diagnosis": "Consumer cannot keep up. 60% frame loss."
}
```

---

## Questions for Review

1. **Atomic ordering**: Should we use `Ordering::Relaxed` for counters (fastest) or `Ordering::SeqCst` (strictly consistent)?
   - **Recommendation**: `Relaxed` - metrics don't need strict ordering

2. **Per-consumer tracking**: Should we track consumed count per-reader or aggregate?
   - **Recommendation**: Aggregate first, per-reader can be added later

3. **Occupancy for watch/mailbox**: These are single-slot; should occupancy return (0,1) or (1,1)?
   - **Recommendation**: Return actual state (0 or 1, 1) for SingleLatest/Mailbox

4. **Reset semantics**: Should `reset_metrics()` be exposed via MCP?
   - **Recommendation**: Not initially; add if requested

---

## Files to Modify

```
aimdb-core/
├── Cargo.toml                    # Add metrics feature
├── src/buffer/
│   ├── mod.rs                    # Re-export BufferMetrics
│   └── traits.rs                 # Add BufferMetrics trait
├── src/remote/
│   └── metadata.rs               # Extend RecordMetadata
└── src/typed_record.rs           # Wire up metrics collection

aimdb-tokio-adapter/
├── Cargo.toml                    # Add metrics feature
└── src/buffer.rs                 # Implement BufferMetrics

aimdb-embassy-adapter/
├── Cargo.toml                    # Add metrics feature + portable-atomic
└── src/buffer.rs                 # Implement BufferMetrics

tools/aimdb-mcp/
├── src/tools/record.rs           # Include metrics in list_records
└── src/tools/metrics.rs          # New get_buffer_metrics tool (optional)
```

---

## References

- Buffer design: `docs/design/002-M1_pluggable-buffers.md`
- MCP integration: `docs/design/009-M4-mcp-integration.md`
