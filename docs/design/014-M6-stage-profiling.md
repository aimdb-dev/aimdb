# RFC: Integrated Stage Profiling

> **Status**: 📝 Draft (RFC - Not Yet Implemented)  
> **Target**: `aimdb-core` (feature-flagged)  
> **Priority**: Medium  
> **Estimated Effort**: 2-3 days  
> **Feature Flag**: `profiling`  
> **Date**: November 2025

---

## Summary

Add automatic timing instrumentation to AimDB's execution primitives (`.source()`, `.tap()`, `.link()`, and future `.transform()`), enabling users to identify slow stages without manual instrumentation.

---

## Motivation

AimDB **owns the execution boundary** for all user-provided callbacks:
- `.source()` - producer callbacks that generate data
- `.tap()` - consumer callbacks that react to data
- `.link()` - connector callbacks that bridge external systems
- `.transform()` (future) - multi-record consumers that produce derived records

Since AimDB spawns and awaits these callbacks, it can automatically measure their execution time. This gives users immediate visibility into which stage is the bottleneck - without requiring them to add any instrumentation code.

**User benefit**: *"AimDB tells me my `.tap()` callback averages 40ms per invocation"* → immediate bottleneck identification.

---

## Important Limitation: Wall-Clock Time

Stage profiling measures **wall-clock time**, not CPU time. This means timing includes:

- Waiting for I/O (network, disk, sensors)
- Waiting for async operations (`.await` points)
- Waiting for timers (`sleep`)
- Executor scheduling delays

### Example

```rust
.tap(|value| async move {
    let result = compute(value);      // 1ms CPU work
    database.insert(result).await;    // 50ms I/O wait
})
```

Profiling reports **51ms**, but the actual compute work is only 1ms.

### Why This Is Still Valuable

1. **Bottleneck identification**: If a tap takes 40ms wall-clock and source produces at 30 FPS (33ms interval), you have a throughput bottleneck regardless of whether it's compute or I/O
2. **Initial triage**: Profiling answers *"which stage is slow?"* - users can then dig deeper with `tracing` or `tokio-console` to understand *"why is it slow?"*
3. **Simplicity**: Tracking actual poll/CPU time is complex and runtime-specific

**Recommendation**: Use AimDB stage profiling for initial bottleneck identification, then use specialized tools (`tracing`, `tokio-console`) for detailed CPU vs I/O analysis.

---

## Proposed Design

### 1. Stage Metrics Structure

```rust
/// Metrics for a single stage (source, tap, link, transform)
#[cfg(feature = "profiling")]
#[derive(Debug, Default)]
pub struct StageMetrics {
    /// Total number of invocations
    call_count: AtomicU64,
    /// Total execution time in nanoseconds
    total_time_ns: AtomicU64,
    /// Minimum execution time in nanoseconds
    min_time_ns: AtomicU64,
    /// Maximum execution time in nanoseconds
    max_time_ns: AtomicU64,
}

#[cfg(feature = "profiling")]
impl StageMetrics {
    pub fn record(&self, duration: Duration) {
        let nanos = duration.as_nanos() as u64;
        self.call_count.fetch_add(1, Ordering::Relaxed);
        self.total_time_ns.fetch_add(nanos, Ordering::Relaxed);
        self.min_time_ns.fetch_min(nanos, Ordering::Relaxed);
        self.max_time_ns.fetch_max(nanos, Ordering::Relaxed);
    }
    
    pub fn call_count(&self) -> u64 {
        self.call_count.load(Ordering::Relaxed)
    }
    
    pub fn total_time(&self) -> Duration {
        Duration::from_nanos(self.total_time_ns.load(Ordering::Relaxed))
    }
    
    pub fn avg_time(&self) -> Duration {
        let count = self.call_count();
        if count == 0 {
            Duration::ZERO
        } else {
            Duration::from_nanos(self.total_time_ns.load(Ordering::Relaxed) / count)
        }
    }
    
    pub fn min_time(&self) -> Duration {
        Duration::from_nanos(self.min_time_ns.load(Ordering::Relaxed))
    }
    
    pub fn max_time(&self) -> Duration {
        Duration::from_nanos(self.max_time_ns.load(Ordering::Relaxed))
    }
    
    pub fn reset(&self) {
        self.call_count.store(0, Ordering::Relaxed);
        self.total_time_ns.store(0, Ordering::Relaxed);
        self.min_time_ns.store(u64::MAX, Ordering::Relaxed);
        self.max_time_ns.store(0, Ordering::Relaxed);
    }
}
```

### 2. Integration Points

#### Source Callbacks

```rust
// In TypedRecord or wherever sources are spawned
#[cfg(feature = "profiling")]
async fn run_source_with_profiling<T, F, Fut>(
    producer: Producer<T, R>,
    callback: F,
    metrics: Arc<StageMetrics>,
) where
    F: Fn(Producer<T, R>) -> Fut,
    Fut: Future<Output = ()>,
{
    loop {
        let start = Instant::now();
        // Execute one iteration of the source
        callback(producer.clone()).await;
        metrics.record(start.elapsed());
    }
}
```

#### Tap Callbacks

```rust
// In dispatcher or wherever taps are executed
#[cfg(feature = "profiling")]
async fn run_tap_with_profiling<T, F, Fut>(
    value: T,
    callback: &F,
    metrics: &StageMetrics,
) where
    F: Fn(T) -> Fut,
    Fut: Future<Output = ()>,
{
    let start = Instant::now();
    callback(value).await;
    metrics.record(start.elapsed());
}
```

#### Link Callbacks (Connectors)

```rust
// In connector dispatch
#[cfg(feature = "profiling")]
async fn run_link_with_profiling<T, F, Fut>(
    value: T,
    callback: &F,
    metrics: &StageMetrics,
) where
    F: Fn(T) -> Fut,
    Fut: Future<Output = ()>,
{
    let start = Instant::now();
    callback(value).await;
    metrics.record(start.elapsed());
}
```

#### Transform Callbacks (Future)

```rust
// Transforms consume multiple records and produce one
#[cfg(feature = "profiling")]
async fn run_transform_with_profiling<I, O, F, Fut>(
    inputs: I,
    producer: Producer<O, R>,
    callback: &F,
    metrics: &StageMetrics,
) where
    F: Fn(I, Producer<O, R>) -> Fut,
    Fut: Future<Output = ()>,
{
    let start = Instant::now();
    callback(inputs, producer).await;
    metrics.record(start.elapsed());
}
```

### 3. Per-Record Stage Tracking

Each `TypedRecord` tracks metrics for its registered stages:

```rust
#[cfg(feature = "profiling")]
struct RecordProfilingMetrics {
    /// Metrics for each registered source (by index or name)
    sources: Vec<Arc<StageMetrics>>,
    /// Metrics for each registered tap
    taps: Vec<Arc<StageMetrics>>,
    /// Metrics for each registered link
    links: Vec<Arc<StageMetrics>>,
    /// Metrics for each registered transform (future)
    transforms: Vec<Arc<StageMetrics>>,
}
```

### 4. Metadata Extension

Extend `RecordMetadata` to include profiling data:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StageProfilingInfo {
    /// Stage type: "source", "tap", "link", "transform"
    pub stage_type: String,
    /// Stage index (0-based)
    pub index: usize,
    /// Optional stage name (if provided during registration)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Number of invocations
    pub call_count: u64,
    /// Total execution time in nanoseconds
    pub total_time_ns: u64,
    /// Average execution time in nanoseconds
    pub avg_time_ns: u64,
    /// Minimum execution time in nanoseconds
    pub min_time_ns: u64,
    /// Maximum execution time in nanoseconds
    pub max_time_ns: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecordMetadata {
    // ... existing fields ...
    
    /// Stage profiling metrics (if profiling enabled)
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg(feature = "profiling")]
    pub stage_profiling: Option<Vec<StageProfilingInfo>>,
}
```

### 5. MCP Integration

Add a new MCP tool for querying stage profiling:

```
mcp_aimdb_get_stage_profiling(socket_path, record_name)
```

Returns:
```json
{
  "record": "camera::Frame",
  "stages": [
    {
      "stage_type": "source",
      "index": 0,
      "name": "camera_capture",
      "call_count": 900,
      "total_time_ns": 45000000000,
      "avg_time_ns": 50000000,
      "min_time_ns": 48000000,
      "max_time_ns": 55000000
    },
    {
      "stage_type": "tap",
      "index": 0,
      "name": "frame_processor",
      "call_count": 360,
      "total_time_ns": 14400000000,
      "avg_time_ns": 40000000,
      "min_time_ns": 38000000,
      "max_time_ns": 52000000
    }
  ],
  "bottleneck": {
    "stage_type": "tap",
    "index": 0,
    "name": "frame_processor",
    "avg_time_ns": 40000000,
    "recommendation": "Tap 'frame_processor' averages 40.0ms per call - this is likely the bottleneck"
  }
}
```

---

## Feature Flag Strategy

```toml
# aimdb-core/Cargo.toml
[features]
default = []
profiling = []  # Opt-in stage profiling

# aimdb-tokio-adapter/Cargo.toml
[features]
profiling = ["aimdb-core/profiling"]

# aimdb-embassy-adapter/Cargo.toml  
profiling = ["aimdb-core/profiling", "portable-atomic"]
```

**Note**: The `profiling` feature is separate from the `metrics` feature (buffer introspection). Users can enable either or both independently.

---

## Implementation Plan

### Phase 1: Core Infrastructure (Day 1)

1. [ ] Add `StageMetrics` struct to `aimdb-core`
2. [ ] Add `profiling` feature flag to `aimdb-core/Cargo.toml`
3. [ ] Add `RecordProfilingMetrics` container
4. [ ] Unit tests for `StageMetrics`

### Phase 2: Instrumentation (Day 1-2)

1. [ ] Instrument `.source()` execution in `TypedRecord`
2. [ ] Instrument `.tap()` execution in dispatcher
3. [ ] Instrument `.link()` execution in connector dispatch
4. [ ] Wire up metrics collection to `RecordProfilingMetrics`

### Phase 3: Metadata & MCP (Day 2-3)

1. [ ] Add `StageProfilingInfo` to `RecordMetadata`
2. [ ] Update `collect_metadata()` to include profiling data
3. [ ] Add `get_stage_profiling` MCP tool
4. [ ] Add bottleneck detection logic (find slowest stage)
5. [ ] Update MCP documentation

### Phase 4: Embassy Support (Day 3)

1. [ ] Add `portable-atomic` dependency for no_std
2. [ ] Verify instrumentation works on Embassy adapter
3. [ ] Cross-compile test for `thumbv7em-none-eabihf`

---

## API Evolution: Named Stages

Use builder pattern for naming stages (backward compatible):

```rust
// Current API (still works)
reg.source(|producer| async move { ... });

// With name via builder pattern
reg.source(|producer| async move { ... })
   .with_name("camera_capture");

// Same for tap and link
reg.tap(|value| async move { ... })
   .with_name("frame_processor");

reg.link(connector)
   .with_name("mqtt_publisher");
```

This improves profiling output by giving stages human-readable names.

---

## Relationship to Buffer Metrics

| Feature | `metrics` flag | `profiling` flag |
|---------|----------------|------------------|
| `produced_count` | ✅ | ❌ |
| `consumed_count` | ✅ | ❌ |
| `dropped_count` | ✅ | ❌ |
| `occupancy` | ✅ | ❌ |
| Source timing | ❌ | ✅ |
| Tap timing | ❌ | ✅ |
| Link timing | ❌ | ✅ |
| Transform timing | ❌ | ✅ |

Users can enable both for complete observability:
```toml
aimdb-core = { version = "0.x", features = ["metrics", "profiling"] }
```

---

## Success Criteria

After implementation, users should be able to:

1. **Identify slow stages**: MCP shows which `.source()`, `.tap()`, or `.link()` is slowest
2. **No manual instrumentation**: AimDB profiles automatically
3. **Zero cost when disabled**: No overhead without `profiling` feature
4. **Actionable output**: Bottleneck detection points to specific stage

Example output:
```
$ mcp_aimdb_get_stage_profiling(socket, "camera::Frame")

Bottleneck detected:
  Stage: tap[0] "frame_processor"
  Avg time: 40.0ms
  Call count: 360
  
  Recommendation: This tap averages 40ms per call but source 
  produces at 30 FPS (33ms interval). Consumer cannot keep up.
```

---

## Design Decisions

1. **Named stages**: Use builder pattern `.with_name("xxx")` for backward compatibility
2. **Histogram support**: No - avg/min/max is sufficient for bottleneck detection
3. **Reset API**: Yes - add `reset_stage_profiling` MCP tool
4. **Transform support**: Yes - stub out structure now for future implementation

---

## Files to Modify

```
aimdb-core/
├── Cargo.toml                    # Add profiling feature
├── src/
│   ├── profiling/                # New module
│   │   ├── mod.rs
│   │   └── stage_metrics.rs      # StageMetrics struct
│   ├── typed_record.rs           # Add RecordProfilingMetrics, instrument sources
│   └── remote/
│       └── metadata.rs           # Add StageProfilingInfo

aimdb-tokio-adapter/
├── Cargo.toml                    # Add profiling feature
└── src/buffer.rs                 # Instrument dispatcher (tap execution)

aimdb-embassy-adapter/
├── Cargo.toml                    # Add profiling feature + portable-atomic
└── src/buffer.rs                 # Same instrumentation

tools/aimdb-mcp/
├── src/tools/
│   ├── mod.rs                    # Export new tool
│   └── profiling.rs              # get_stage_profiling tool
└── README.md                     # Document new tool
```

---

## References

- Buffer metrics RFC: `docs/design/013-M6-buffer-introspection-metrics.md`
- Buffer design: `docs/design/002-M1_pluggable-buffers.md`
- MCP integration: `docs/design/009-M4-mcp-integration.md`
