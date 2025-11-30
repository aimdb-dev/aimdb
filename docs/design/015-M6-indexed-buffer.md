# RFC: IndexedBuffer - Source-Aware Record Storage

- **Status**: Draft
- **Author**: AimDB Team
- **Created**: 2025-11-30
- **Updated**: 2025-11-30
- **Target Release**: v0.5.0

## Summary

Add a new buffer type `IndexedLatest` that automatically indexes records by a key extracted from the record itself. This enables efficient per-source queries and subscriptions in multi-source systems where thousands of sensors/stations produce records with the same `TypeId`.

## Motivation

### The Problem: Multi-Source Convergence

AimDB is designed to link thousands of sensors into a shared instance. Each sensor produces records with the **same type** (same `TypeId`), but from **different sources**:

```
┌─────────────────────────────────────────────────────────────────────────┐
│                        DISTRIBUTED SENSOR MESH                          │
├─────────────────────────────────────────────────────────────────────────┤
│                                                                         │
│  Station 1          Station 2          Station 3         Station N      │
│  ┌─────────┐        ┌─────────┐        ┌─────────┐       ┌─────────┐   │
│  │ AimDB   │        │ AimDB   │        │ AimDB   │       │ AimDB   │   │
│  │ local   │        │ local   │        │ local   │       │ local   │   │
│  │         │        │         │        │         │       │         │   │
│  │ Reading │        │ Reading │        │ Reading │       │ Reading │   │
│  │ TypeId:X│        │ TypeId:X│        │ TypeId:X│       │ TypeId:X│   │
│  └────┬────┘        └────┬────┘        └────┬────┘       └────┬────┘   │
│       │                  │                  │                 │        │
│       └──────────────────┴────────┬─────────┴─────────────────┘        │
│                                   │                                     │
│                                   ▼                                     │
│                        ┌─────────────────────┐                         │
│                        │   Central AimDB     │                         │
│                        │                     │                         │
│                        │  Reading (TypeId:X) │ ← ALL stations' data    │
│                        │  ONE type, but need │   converges here        │
│                        │  per-source access  │                         │
│                        └─────────────────────┘                         │
│                                                                         │
│  Current limitation:                                                    │
│  • Cannot query "Station 42's latest reading" efficiently               │
│  • Cannot subscribe to "only Station 42's updates"                      │
│  • MCP tools cannot introspect per-source state                         │
│                                                                         │
└─────────────────────────────────────────────────────────────────────────┘
```

### Why This Belongs in AimDB Core

1. **Infrastructure, not application logic** - Source-based indexing is fundamental to any multi-source distributed system

2. **Remote access requires it** - MCP tools and agents need to query/subscribe to specific sources

3. **Connectors need it** - MQTT topics, KNX group addresses, and other protocols naturally have source identity

4. **Embedded needs it** - Embassy devices aggregating multiple sensors need efficient per-source queries

5. **Type routing isn't enough** - AimDB handles type-based routing well, but lacks source-based indexing within a type

### Use Cases

1. **Weather Mesh**: 1000+ stations, need latest state per station + per-station subscriptions
2. **Building Automation**: 500 KNX devices, query "room 3.14 temperature"
3. **Fleet Management**: Vehicle telemetry indexed by vehicle ID
4. **Industrial IoT**: Thousands of sensors on a factory floor
5. **Home Automation**: Devices indexed by room or location

### Goals

- Enable per-source "latest value" queries within a record type
- Support source-filtered subscriptions
- Support wildcard subscriptions (all sources)
- Efficient O(1) lookup by extracted key
- **No changes to record type definitions** - key is extracted, not stored separately
- Flexible key extraction - any derived key (ID, location, composite)
- MCP/remote access support for source-aware queries
- Maintain AimDB's flow-control semantics (not queryable history)

### Non-Goals

- Per-source ring buffers (history per source) - See "Buffer Semantics" section
- Cross-source transactions
- Automatic source discovery/registration

## Design

### Core Concept

An `IndexedLatest` buffer extracts a key from each record and maintains per-key latest values:

```
┌─────────────────────────────────────────────────────────────────────────┐
│            IndexedLatest<SensorReading> with key = source_id            │
├─────────────────────────────────────────────────────────────────────────┤
│                                                                         │
│  Incoming records:                                                      │
│  ┌─────────────────────────────────────────────────────────────────┐   │
│  │ SensorReading { source_id: 1, temp: 22.5, ts: ... }             │   │
│  │ SensorReading { source_id: 2, temp: 18.3, ts: ... }             │   │
│  │ SensorReading { source_id: 1, temp: 22.7, ts: ... }  // update  │   │
│  └─────────────────────────────────────────────────────────────────┘   │
│                           │                                             │
│                           ▼                                             │
│              ┌─────────────────────────┐                               │
│              │  Key Extractor          │                               │
│              │  |r| r.source_id        │                               │
│              └────────────┬────────────┘                               │
│                           │                                             │
│                           ▼                                             │
│  ┌─────────────────────────────────────────────────────────────────┐   │
│  │  Index: HashMap<SourceId, Watch<SensorReading>>                 │   │
│  ├─────────────────────────────────────────────────────────────────┤   │
│  │  Key        │  Latest Value                                     │   │
│  ├─────────────┼───────────────────────────────────────────────────┤   │
│  │  SourceId(1)│  Watch { temp: 22.7, ts: ... }  ← updated         │   │
│  │  SourceId(2)│  Watch { temp: 18.3, ts: ... }                    │   │
│  └─────────────┴───────────────────────────────────────────────────┘   │
│                           │                                             │
│                           ▼                                             │
│              ┌─────────────────────────┐                               │
│              │  Broadcast<T> for       │                               │
│              │  wildcard subscribers   │                               │
│              └─────────────────────────┘                               │
│                                                                         │
└─────────────────────────────────────────────────────────────────────────┘
```

### Flexible Key Extraction

The key extractor is a user-provided function, enabling various indexing strategies:

```rust
// Index by source ID (most common)
|reading: &SensorReading| reading.source_id.clone()

// Index by location (lat/lon grid cell)
|reading: &SensorReading| GridCell::from_coords(reading.lat, reading.lon)

// Index by composite key
|reading: &SensorReading| (reading.region_id, reading.device_type)

// Index by geohash (spatial indexing)
|reading: &SensorReading| geohash::encode(reading.lat, reading.lon, 6)

// Index by time bucket (e.g., hourly)
|reading: &SensorReading| reading.timestamp / 3600
```

### API Design

#### 1. Buffer Configuration Extension

```rust
// aimdb-core/src/buffer/cfg.rs

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BufferCfg {
    // Existing variants - unchanged
    SpmcRing { capacity: usize },
    SingleLatest,
    Mailbox,
    
    // NEW: Indexed latest value storage
    IndexedLatest {
        /// Maximum number of unique keys (sources) to track
        /// None = unbounded (grows dynamically)
        /// Some(n) = bounded, rejects new keys when full
        max_sources: Option<usize>,
        
        /// Capacity for the wildcard subscription broadcast channel
        broadcast_capacity: usize,
    },
}

impl BufferCfg {
    /// Convenience constructor for IndexedLatest with defaults
    pub fn indexed_latest() -> Self {
        BufferCfg::IndexedLatest {
            max_sources: None,
            broadcast_capacity: 1024,
        }
    }
    
    /// IndexedLatest with bounded source count
    pub fn indexed_latest_bounded(max_sources: usize) -> Self {
        BufferCfg::IndexedLatest {
            max_sources: Some(max_sources),
            broadcast_capacity: max_sources.saturating_mul(4).max(256),
        }
    }
}
```

#### 2. Core Traits

```rust
// aimdb-core/src/buffer/indexed.rs (new file)

use core::hash::Hash;
use core::future::Future;
use core::pin::Pin;

/// Trait bounds for index keys
pub trait IndexKey: Hash + Eq + Clone + Send + Sync + 'static {}
impl<T: Hash + Eq + Clone + Send + Sync + 'static> IndexKey for T {}

/// Key extractor function type
/// 
/// Extracts an index key from a record. Called on every push().
/// Returns `Some(key)` to index the record, `None` to filter it out.
/// Should be cheap (ideally just field access or simple derivation).
pub type KeyExtractor<T, K> = Arc<dyn Fn(&T) -> Option<K> + Send + Sync>;

/// Indexed buffer trait for source-aware record storage
/// 
/// Stores records indexed by an extracted key, maintaining latest value per key.
/// Supports both key-specific and wildcard subscriptions.
pub trait IndexedBuffer<T: Clone + Send + 'static, K: IndexKey>: Send + Sync + 'static {
    /// Reader type for source-specific subscriptions
    type SourceReader: BufferReader<T> + 'static;
    
    /// Reader type for wildcard (all sources) subscriptions  
    type AllReader: BufferReader<T> + 'static;

    /// Push a record (key extracted automatically via configured extractor)
    /// 
    /// Behavior:
    /// - Extracts key using `key_extractor(&value)`
    /// - If `Some(key)`: updates latest value for that key, notifies subscribers
    /// - If `None`: record is filtered out (not indexed, but still broadcast to wildcard)
    /// - Always broadcasts to wildcard subscribers (even filtered records)
    fn push(&self, value: T);
    
    /// Get the latest record for a specific source
    /// 
    /// Returns `None` if no record from that source has been received.
    fn get_latest(&self, key: &K) -> Option<T>;
    
    /// Get all known source keys
    fn sources(&self) -> Vec<K>;
    
    /// Get latest record for each source (snapshot)
    fn snapshot(&self) -> Vec<(K, T)>;
    
    /// Get number of known sources
    fn source_count(&self) -> usize;
    
    /// Check if a source exists
    fn has_source(&self, key: &K) -> bool;
    
    /// Subscribe to updates from a specific source
    /// 
    /// Returns a reader that yields records only from the specified source.
    /// If the source doesn't exist yet, waits for first record.
    fn subscribe_source(&self, key: K) -> Self::SourceReader;
    
    /// Subscribe to all updates (wildcard)
    /// 
    /// Returns a reader that yields all records from all sources.
    /// Uses broadcast semantics - may lag if consumer is slow.
    fn subscribe_all(&self) -> Self::AllReader;
    
    /// Remove a source and its latest value
    /// 
    /// Returns the last stored value if the source existed.
    /// Subscribers to this source will continue waiting (won't receive error).
    fn remove_source(&self, key: &K) -> Option<T>;
}

/// Type-erased indexed buffer for storage in TypedRecord
pub trait DynIndexedBuffer<T: Clone + Send + 'static>: Send + Sync {
    /// Push a record (key extracted internally)
    fn push(&self, value: T);
    
    /// Get latest by key (key as serialized JSON for type erasure)
    fn get_latest_json(&self, key_json: &serde_json::Value) -> Option<T>;
    
    /// Get all source keys as JSON
    fn sources_json(&self) -> Vec<serde_json::Value>;
    
    /// Get snapshot as (key_json, value) pairs
    fn snapshot_json(&self) -> Vec<(serde_json::Value, T)>;
    
    /// Remove a source by JSON key
    fn remove_source_json(&self, key_json: &serde_json::Value) -> Option<T>;
    
    /// Subscribe to a specific source (key as JSON)
    fn subscribe_source_json(&self, key_json: &serde_json::Value) 
        -> Option<Box<dyn BufferReader<T> + Send>>;
    
    /// Subscribe to all updates
    fn subscribe_all_boxed(&self) -> Box<dyn BufferReader<T> + Send>;
    
    /// Number of sources
    fn source_count(&self) -> usize;
}
```

#### 3. Record Registration API

The `indexed_buffer()` method returns a builder that supports the same `.source()`, `.tap()`, and `.with_serialization()` chaining as regular buffers:

> **Note:** Source-specific taps (e.g., `.tap_source(key, handler)`) and transforms are planned for a future RFC on the `.transform()` API. For now, use wildcard `.tap()` with filtering, or subscribe to specific sources via `db.subscribe_source()` after building.

```rust
// Usage in builder configuration

#[derive(Clone, Debug, Serialize, Deserialize)]
struct SensorReading {
    source_id: SourceId,
    timestamp: i64,
    temp_c: f32,
    humidity_pct: f32,
    lat: f64,
    lon: f64,
}

// === Example 1: Basic indexed buffer with source and tap ===
builder.configure::<SensorReading>(|reg| {
    reg.indexed_buffer(
        BufferCfg::indexed_latest(),
        |r: &SensorReading| Some(r.source_id),  // Index by station ID
    )
    .with_serialization()
    .source(|ctx, producer| async move {
        // Produce readings from multiple stations
        loop {
            for station_id in 1..=250 {
                let reading = fetch_weather(StationId(station_id)).await;
                producer.produce(reading).await.unwrap();
            }
            tokio::time::sleep(Duration::from_secs(60)).await;
        }
    })
    .tap(|ctx, consumer| async move {
        // Wildcard tap: receives ALL readings from ALL stations
        while let Ok(reading) = consumer.recv().await {
            println!("Any station: {} → {:.1}°C", 
                reading.station_id.0, reading.temp_c);
        }
    });
});

// === Example 2: Tap with filtering (current approach) ===
builder.configure::<SensorReading>(|reg| {
    reg.indexed_buffer(
        BufferCfg::indexed_latest(),
        |r: &SensorReading| Some(r.source_id),
    )
    .with_serialization()
    .source(|ctx, producer| async move {
        // ... produce readings
    })
    // Wildcard tap with manual filtering
    .tap(|ctx, consumer| async move {
        while let Ok(reading) = consumer.recv().await {
            // Filter for specific stations
            if reading.source_id == StationId(42) {
                println!("Station 42: {:.1}°C, {:.0}% humidity", 
                    reading.temp_c, reading.humidity_pct);
            }
        }
    });
});

// Alternative: Subscribe to specific source after build
let db = builder.build().await?;
let mut station_42 = db.subscribe_source::<SensorReading, StationId>(StationId(42))?;
tokio::spawn(async move {
    while let Ok(reading) = station_42.recv().await {
        println!("Station 42: {:.1}°C", reading.temp_c);
    }
});

// === Example 3: Multiple sources and taps ===
builder.configure::<SensorReading>(|reg| {
    reg.indexed_buffer(
        BufferCfg::indexed_latest_bounded(1000),  // Max 1000 stations
        |r: &SensorReading| Some(r.source_id),
    )
    .with_serialization()
    // Multiple data sources
    .source(|ctx, producer| async move {
        fetch_from_open_meteo(producer).await;
    })
    .source(|ctx, producer| async move {
        fetch_from_local_sensors(producer).await;
    })
    // Wildcard tap for logging and alerting
    .tap(|ctx, consumer| async move {
        while let Ok(reading) = consumer.recv().await {
            metrics::counter!("readings_received").increment(1);
            
            // Alert on critical stations
            if reading.source_id == StationId(1) && reading.temp_c > 40.0 {
                send_alert("Station 1 overheating!").await;
            }
        }
    });
});

// === Example 4: Index by grid cell (spatial) ===
#[derive(Clone, Hash, Eq, PartialEq, Debug, Serialize, Deserialize)]
struct GridCell { lat_bucket: i32, lon_bucket: i32 }

impl GridCell {
    fn from_coords(lat: f64, lon: f64, resolution: f64) -> Option<Self> {
        // Filter out invalid coordinates
        if !lat.is_finite() || !lon.is_finite() {
            return None;
        }
        Some(GridCell {
            lat_bucket: (lat / resolution).floor() as i32,
            lon_bucket: (lon / resolution).floor() as i32,
        })
    }
}

builder.configure::<SensorReading>(|reg| {
    reg.indexed_buffer(
        BufferCfg::indexed_latest(),
        |r: &SensorReading| GridCell::from_coords(r.lat, r.lon, 0.5), // 0.5° grid
    )
    .with_serialization()
    .tap(|ctx, consumer| async move {
        let vienna = GridCell { lat_bucket: 96, lon_bucket: 32 };
        while let Ok(reading) = consumer.recv().await {
            // Filter for Vienna area
            if let Some(cell) = GridCell::from_coords(reading.lat, reading.lon, 0.5) {
                if cell == vienna {
                    println!("Vienna area: {:.1}°C", reading.temp_c);
                }
            }
        }
    });
});

// === Example 5: Index by geohash ===
builder.configure::<SensorReading>(|reg| {
    reg.indexed_buffer(
        BufferCfg::indexed_latest(),
        |r: &SensorReading| Some(geohash::encode_f64(r.lat, r.lon, 6)), // ~1km precision
    )
    .with_serialization();
});

// === Example 6: Composite key ===
builder.configure::<SensorReading>(|reg| {
    reg.indexed_buffer(
        BufferCfg::indexed_latest(),
        |r: &SensorReading| Some((r.source_id.region(), r.source_id.device_type())),
    )
    .with_serialization();
});
```

#### 4. Query and Subscription API

```rust
let db = builder.build()?;

// Produce records (same as before - push extracts key automatically)
let producer = db.producer::<SensorReading>();
producer.produce(SensorReading { 
    source_id: SourceId(42), 
    temp_c: 22.5,
    ... 
}).await?;

// === NEW: Source-specific queries ===

// Get latest from a specific source
let latest = db.get_indexed::<SensorReading, SourceId>(&SourceId(42))?;

// List all known sources
let sources = db.sources::<SensorReading, SourceId>()?;

// Get snapshot of all latest values
let snapshot = db.snapshot_indexed::<SensorReading, SourceId>()?;

// === NEW: Source-specific subscription ===

// Subscribe to only one source
let mut reader = db.subscribe_source::<SensorReading, SourceId>(SourceId(42))?;
while let Ok(reading) = reader.recv().await {
    // Only receives updates from source 42
    println!("Station 42: {:.1}°C", reading.temp_c);
}

// Subscribe to all sources (existing behavior, unchanged)
let mut reader = db.subscribe::<SensorReading>()?;
while let Ok(reading) = reader.recv().await {
    // Receives updates from ALL sources
    println!("Station {}: {:.1}°C", reading.source_id.0, reading.temp_c);
}
```

#### 5. Tokio Implementation

```rust
// aimdb-tokio-adapter/src/indexed_buffer.rs

use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use tokio::sync::{broadcast, watch};

pub struct TokioIndexedBuffer<T, K>
where
    T: Clone + Send + Sync + 'static,
    K: IndexKey,
{
    inner: Arc<TokioIndexedBufferInner<T, K>>,
}

struct TokioIndexedBufferInner<T, K>
where
    T: Clone + Send + Sync + 'static,
    K: IndexKey,
{
    /// Key extractor function (returns Option<K> to enable filtering)
    key_extractor: KeyExtractor<T, K>,
    
    /// Per-source watch channels (latest value per source)
    sources: RwLock<HashMap<K, watch::Sender<Option<T>>>>,
    
    /// Broadcast for wildcard subscribers
    broadcast_tx: broadcast::Sender<T>,
    
    /// Configuration
    max_sources: Option<usize>,
}

impl<T, K> TokioIndexedBuffer<T, K>
where
    T: Clone + Send + Sync + 'static,
    K: IndexKey,
{
    pub fn new(
        key_extractor: KeyExtractor<T, K>,
        max_sources: Option<usize>,
        broadcast_capacity: usize,
    ) -> Self {
        let (broadcast_tx, _) = broadcast::channel(broadcast_capacity);
        Self {
            inner: Arc::new(TokioIndexedBufferInner {
                key_extractor,
                sources: RwLock::new(HashMap::new()),
                broadcast_tx,
                max_sources,
            }),
        }
    }
}

impl<T, K> IndexedBuffer<T, K> for TokioIndexedBuffer<T, K>
where
    T: Clone + Send + Sync + 'static,
    K: IndexKey,
{
    type SourceReader = TokioSourceReader<T>;
    type AllReader = TokioAllReader<T>;

    fn push(&self, value: T) {
        // Extract key from value (Option<K> enables filtering)
        let key = (self.inner.key_extractor)(&value);
        
        // Always broadcast to wildcard subscribers (even filtered records)
        let _ = self.inner.broadcast_tx.send(value.clone());
        
        // If key extractor returned None, record is filtered (not indexed)
        let key = match key {
            Some(k) => k,
            None => return,  // Filtered out - already broadcast, skip indexing
        };
        
        // Update per-source watch channel
        {
            let mut sources = self.inner.sources.write().unwrap();
            
            if let Some(tx) = sources.get(&key) {
                // Source exists - update value
                let _ = tx.send(Some(value));
            } else {
                // New source - check capacity
                if let Some(max) = self.inner.max_sources {
                    if sources.len() >= max {
                        #[cfg(feature = "tracing")]
                        tracing::warn!(
                            "IndexedBuffer at capacity ({}), dropping new source",
                            max
                        );
                        return;
                    }
                }
                
                // Create new watch channel for this source
                let (tx, _) = watch::channel(Some(value));
                sources.insert(key, tx);
            }
        }
    }

    fn get_latest(&self, key: &K) -> Option<T> {
        let sources = self.inner.sources.read().unwrap();
        sources.get(key).and_then(|tx| tx.borrow().clone())
    }

    fn sources(&self) -> Vec<K> {
        let sources = self.inner.sources.read().unwrap();
        sources.keys().cloned().collect()
    }

    fn snapshot(&self) -> Vec<(K, T)> {
        let sources = self.inner.sources.read().unwrap();
        sources
            .iter()
            .filter_map(|(k, tx)| {
                tx.borrow().clone().map(|v| (k.clone(), v))
            })
            .collect()
    }

    fn source_count(&self) -> usize {
        self.inner.sources.read().unwrap().len()
    }

    fn has_source(&self, key: &K) -> bool {
        self.inner.sources.read().unwrap().contains_key(key)
    }

    fn remove_source(&self, key: &K) -> Option<T> {
        let mut sources = self.inner.sources.write().unwrap();
        // Remove and return the last known value
        sources.remove(key).and_then(|tx| tx.borrow().clone())
    }

    fn subscribe_source(&self, key: K) -> Self::SourceReader {
        let sources = self.inner.sources.read().unwrap();
        
        let rx = if let Some(tx) = sources.get(&key) {
            tx.subscribe()
        } else {
            // Source doesn't exist yet - create pending channel
            drop(sources);
            let mut sources = self.inner.sources.write().unwrap();
            
            // Double-check (another thread might have created it)
            if let Some(tx) = sources.get(&key) {
                tx.subscribe()
            } else {
                let (tx, rx) = watch::channel(None);
                sources.insert(key, tx);
                rx
            }
        };
        
        TokioSourceReader { rx }
    }

    fn subscribe_all(&self) -> Self::AllReader {
        TokioAllReader {
            rx: self.inner.broadcast_tx.subscribe(),
        }
    }
}

/// Reader for source-specific subscriptions
pub struct TokioSourceReader<T: Clone + Send + Sync + 'static> {
    rx: watch::Receiver<Option<T>>,
}

impl<T: Clone + Send + Sync + 'static> BufferReader<T> for TokioSourceReader<T> {
    fn recv(&mut self) -> Pin<Box<dyn Future<Output = Result<T, DbError>> + Send + '_>> {
        Box::pin(async move {
            loop {
                self.rx.changed().await.map_err(|_| DbError::BufferClosed {
                    buffer_name: "indexed_watch".to_string(),
                })?;
                
                if let Some(value) = self.rx.borrow().clone() {
                    return Ok(value);
                }
                // None = source created but no value yet, keep waiting
            }
        })
    }
}

/// Reader for wildcard subscriptions
pub struct TokioAllReader<T: Clone + Send + Sync + 'static> {
    rx: broadcast::Receiver<T>,
}

impl<T: Clone + Send + Sync + 'static> BufferReader<T> for TokioAllReader<T> {
    fn recv(&mut self) -> Pin<Box<dyn Future<Output = Result<T, DbError>> + Send + '_>> {
        Box::pin(async move {
            match self.rx.recv().await {
                Ok(value) => Ok(value),
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    Err(DbError::BufferLagged {
                        lag_count: n,
                        buffer_name: "indexed_broadcast".to_string(),
                    })
                }
                Err(broadcast::error::RecvError::Closed) => {
                    Err(DbError::BufferClosed {
                        buffer_name: "indexed_broadcast".to_string(),
                    })
                }
            }
        })
    }
}
```

#### 6. Remote Access / MCP Integration

```rust
// Extended AimX protocol commands

/// List all sources for a record type
#[derive(Serialize, Deserialize)]
struct ListSourcesRequest {
    record_type: String,  // e.g., "SensorReading"
}

#[derive(Serialize, Deserialize)]
struct ListSourcesResponse {
    sources: Vec<serde_json::Value>,  // Keys as JSON
    count: usize,
}

/// Get latest record from a specific source
#[derive(Serialize, Deserialize)]
struct GetLatestRequest {
    record_type: String,
    source_key: serde_json::Value,  // e.g., {"source_id": 42} or {"lat": 48, "lon": 16}
}

/// Subscribe to a specific source
#[derive(Serialize, Deserialize)]
struct SubscribeSourceRequest {
    record_type: String,
    source_key: Option<serde_json::Value>,  // None = wildcard
    queue_size: usize,
}

/// Get snapshot of all sources
#[derive(Serialize, Deserialize)]  
struct SnapshotRequest {
    record_type: String,
}

#[derive(Serialize, Deserialize)]
struct SnapshotResponse {
    entries: Vec<SnapshotEntry>,
}

#[derive(Serialize, Deserialize)]
struct SnapshotEntry {
    source_key: serde_json::Value,
    value: serde_json::Value,
}
```

**MCP Tools:**

```bash
# List all sources (stations, devices, etc.)
mcp_aimdb_list_sources(
    socket_path: "/tmp/weather-mesh.sock",
    record_name: "SensorReading"
)
# Returns: [{"source_id": 1}, {"source_id": 2}, ...]

# Get latest from specific source
mcp_aimdb_get_latest(
    socket_path: "/tmp/weather-mesh.sock",
    record_name: "SensorReading",
    source_key: {"source_id": 42}
)
# Returns: {"source_id": 42, "temp_c": 22.5, "humidity_pct": 65.0, ...}

# Get latest by grid cell (if indexed by location)
mcp_aimdb_get_latest(
    socket_path: "/tmp/weather-mesh.sock",
    record_name: "SensorReading",
    source_key: {"lat_bucket": 48, "lon_bucket": 16}
)

# Subscribe to specific source
mcp_aimdb_subscribe_source(
    socket_path: "/tmp/weather-mesh.sock",
    record_name: "SensorReading",
    source_key: {"source_id": 42},
    max_samples: 10
)

# Get full snapshot
mcp_aimdb_snapshot(
    socket_path: "/tmp/weather-mesh.sock",
    record_name: "SensorReading"
)
# Returns: [
#   {"source_key": {"source_id": 1}, "value": {...}},
#   {"source_key": {"source_id": 2}, "value": {...}},
#   ...
# ]
```

## Buffer Semantics: Flow Control, Not Storage

### What IndexedLatest IS

- **Per-source latest value** - Each source has exactly one "current" value
- **Source-filtered subscriptions** - Get updates only from sources you care about
- **Wildcard subscriptions** - Get all updates (existing SPMC behavior)
- **Instant queries** - O(1) lookup of latest value for any source

### What IndexedLatest is NOT

- **Per-source history** - No "last 10 readings from station 42"
- **Queryable ring buffer** - No random access to past values
- **Time-series storage** - Use TimescaleDB/InfluxDB for that

This follows AimDB's design philosophy: **hot-path state synchronization, not historical storage**.

```
┌─────────────────────────────────────────────────────────────────────┐
│  AimDB IndexedLatest        │  External Storage                     │
├─────────────────────────────┼───────────────────────────────────────┤
│  "What is station 42's      │  "What were station 42's readings     │
│   current temperature?"     │   over the last 24 hours?"            │
│                             │                                       │
│  → Instant O(1) lookup      │  → TimescaleDB / InfluxDB / Supabase │
│  → Sub-millisecond          │  → SQL query with time range          │
└─────────────────────────────┴───────────────────────────────────────┘
```

## TypedRecord Integration

```rust
// aimdb-core/src/typed_record.rs modifications

pub struct TypedRecord<T: Send + 'static + Debug + Clone, R: Spawn + 'static> {
    // Existing fields...
    buffer: Option<Box<dyn DynBuffer<T>>>,
    
    // NEW: Optional indexed buffer (mutually exclusive with regular buffer)
    indexed_buffer: Option<IndexedBufferStorage<T>>,
}

/// Storage for indexed buffers with type-erased key handling
struct IndexedBufferStorage<T: Clone + Send + 'static> {
    /// The actual indexed buffer
    buffer: Box<dyn DynIndexedBuffer<T> + Send + Sync>,
    
    /// Key type name (for introspection)
    key_type_name: &'static str,
    
    /// Key serializer (for remote access)
    #[cfg(feature = "std")]
    key_serializer: Option<KeySerializerFn>,
    
    /// Key deserializer (for remote access)
    #[cfg(feature = "std")]
    key_deserializer: Option<KeyDeserializerFn>,
}
```

## Embassy Considerations

Embassy implementation requires compile-time sizing:

```rust
// aimdb-embassy-adapter/src/indexed_buffer.rs

use heapless::FnvIndexMap;

pub struct EmbassyIndexedBuffer<
    T: Clone + Send,
    K: IndexKey,
    const MAX_SOURCES: usize,
    const SUBS_PER_SOURCE: usize,
    const BROADCAST_CAP: usize,
> {
    key_extractor: fn(&T) -> K,  // Function pointer (no heap allocation)
    sources: Mutex<FnvIndexMap<K, Watch<CriticalSectionRawMutex, T, SUBS_PER_SOURCE>, MAX_SOURCES>>,
    broadcast: PubSubChannel<CriticalSectionRawMutex, T, BROADCAST_CAP, 4, 1>,
}

// Usage
type StationBuffer = EmbassyIndexedBuffer<SensorReading, SourceId, 256, 4, 1024>;
static STATION_BUFFER: StationBuffer = EmbassyIndexedBuffer::new(|r| r.source_id);
```

**Recommendation:** Implement Tokio first, Embassy in follow-up PR.

## Implementation Plan

### Phase 1: Core Infrastructure (Week 1)
- [ ] Add `IndexedLatest` to `BufferCfg`
- [ ] Define `IndexedBuffer` trait and `KeyExtractor` type
- [ ] Add `DynIndexedBuffer` for type erasure
- [ ] Extend `RecordRegistrar` with `indexed_buffer()` method
- [ ] Unit tests for core traits

### Phase 2: Tokio Implementation (Week 1-2)
- [ ] Implement `TokioIndexedBuffer<T, K>`
- [ ] Implement `TokioSourceReader` and `TokioAllReader`
- [ ] Integration tests
- [ ] Benchmark: lookup, subscription, push throughput

### Phase 3: AimDB Integration (Week 2)
- [ ] Extend `TypedRecord` for indexed buffer storage
- [ ] Add `get_indexed()`, `sources()`, `snapshot_indexed()` to `AimDb`
- [ ] Add `subscribe_source()` to `AimDb`
- [ ] Ensure backward compatibility with non-indexed records

### Phase 4: Remote Access (Week 2-3)
- [ ] Extend AimX protocol: `list_sources`, `get_latest`, `subscribe_source`, `snapshot`
- [ ] Add MCP tools for indexed records
- [ ] JSON key serialization/deserialization
- [ ] Update schema inference for indexed records

### Phase 5: Documentation & Examples (Week 3)
- [ ] API documentation with key extractor examples
- [ ] Weather mesh demo using IndexedLatest
- [ ] Spatial indexing example (grid cells, geohash)
- [ ] Migration guide from manual HashMap management

### Phase 6: Embassy Support (Future)
- [ ] `EmbassyIndexedBuffer` implementation
- [ ] `heapless` FnvIndexMap integration
- [ ] Embedded example

### IndexedRing Buffer (Future)

Per-source history (ring buffer per key) may be considered if strong use cases emerge:

```rust
// Potential future buffer type
BufferCfg::IndexedRing {
    max_sources: 1000,
    history_per_source: 10,  // Last 10 values per source
}
```

This would enable queries like "last 10 readings from station 42" but adds significant complexity and memory overhead.

## Performance Characteristics

| Operation | Time Complexity | Notes |
|-----------|-----------------|-------|
| `push(value)` | O(1) amortized | Key extraction + HashMap insert + broadcast |
| `get_latest(key)` | O(1) | HashMap lookup |
| `sources()` | O(n) | Clones all keys |
| `snapshot()` | O(n) | Clones all entries |
| `subscribe_source(key)` | O(1) | Creates watch receiver |
| `subscribe_all()` | O(1) | Creates broadcast receiver |
| `source_count()` | O(1) | HashMap len |

**Memory:** ~64 bytes per source (watch channel) + value size + broadcast buffer

**Key extraction overhead:** Should be minimal (field access or simple computation). Avoid expensive operations in key extractors.

## Design Decisions (Resolved)

### 1. Key Extractor Signature: `Fn(&T) -> Option<K>`

**Decision:** Use `Option<K>` to enable filtering.

**Rationale:** Returning `None` allows records to be filtered out (not indexed, only broadcast to wildcard subscribers). This is important for:
- Filtering invalid/incomplete records
- Conditional indexing based on record content
- Soft deprecation of sources

```rust
// Example: Only index readings with valid coordinates
|r: &SensorReading| {
    if r.lat.is_finite() && r.lon.is_finite() {
        Some(GridCell::from_coords(r.lat, r.lon))
    } else {
        None  // Invalid reading - broadcast only, don't index
    }
}

// Example: Only index readings above a threshold
|r: &SensorReading| {
    if r.temp_c > -50.0 && r.temp_c < 60.0 {
        Some(r.source_id)
    } else {
        None  // Likely sensor error - filter out
    }
}
```

### 2. Source Lifecycle: Explicit `remove_source()`

**Decision:** Sources are not automatically expired. Use explicit `remove_source(key)` to remove.

**Rationale:**
- Predictable behavior - sources don't disappear unexpectedly
- Application knows best when a source is truly gone
- Avoids complexity of configurable timeouts
- Memory is bounded by `max_sources` if needed

```rust
// API
fn remove_source(&self, key: &K) -> Option<T>;  // Returns last value if existed

// Usage
db.remove_source::<SensorReading, StationId>(&StationId(42))?;
```

### 3. Key Serialization: `K: Serialize + DeserializeOwned`

**Decision:** Require serde traits for remote access.

**Rationale:**
- Consistent with existing AimDB serialization patterns
- JSON is the wire format for AimX protocol
- Most key types already derive serde traits
- Enables complex keys (structs, tuples) not just primitives

```rust
// Key type must be serializable for MCP/remote access
#[derive(Clone, Hash, Eq, PartialEq, Serialize, Deserialize)]
pub struct GridCell { lat_bucket: i16, lon_bucket: i16 }
```

### 4. Key Extraction: `&T` Reference (Not Owned)

**Decision:** Key extractor receives `&T`, not owned `T`.

**Rationale:**
- Key extraction is synchronous (no await points)
- Keys are typically `Copy` or cheap to `Clone`
- Extractor runs inside `push()` which owns the value
- No need to move data out of `T` for key derivation

```rust
// &T is sufficient - clone fields as needed for key
|r: &SensorReading| Some(r.source_id)           // Copy
|r: &SensorReading| Some(r.name.clone())        // Clone string
|r: &SensorReading| Some((r.region, r.device))  // Tuple of Copy fields
```

## Alternatives Considered

### 1. KeyedBuffer<K, V> (Rejected)

Separate key and value types, register as `configure_keyed::<K, V>()`.

**Rejected because:**
- Changes the record model (key separate from value)
- Doesn't work with existing records
- Less natural for the "source-aware records" use case

### 2. Per-Source TypeId (Rejected)

Generate unique types per source at compile time.

**Rejected because:**
- Can't add sources dynamically
- Compile-time explosion
- Doesn't match the distributed sensor mesh model

### 3. User-Space HashMap (Partial Solution)

Keep state in user-space `HashMap<K, V>`, outside AimDB.

**Rejected because:**
- No MCP/remote access introspection
- Duplicates AimDB's responsibility
- Doesn't integrate with connector framework

## References

- [Tokio watch channel](https://docs.rs/tokio/latest/tokio/sync/watch/index.html) - Per-source latest value
- [Tokio broadcast channel](https://docs.rs/tokio/latest/tokio/sync/broadcast/index.html) - Wildcard subscriptions
- [Geohash](https://en.wikipedia.org/wiki/Geohash) - Spatial indexing strategy
- [heapless FnvIndexMap](https://docs.rs/heapless/latest/heapless/struct.FnvIndexMap.html) - Embassy no_std HashMap

---

## Appendix A: Weather Mesh Example

```rust
use aimdb_core::{AimDbBuilder, buffer::BufferCfg};
use aimdb_tokio_adapter::TokioAdapter;
use serde::{Serialize, Deserialize};
use std::sync::Arc;
use std::time::Duration;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct StationId(pub u32);

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WeatherReading {
    pub station_id: StationId,
    pub timestamp_ms: i64,
    pub temp_c: f32,
    pub humidity_pct: f32,
    pub wind_speed_kmh: f32,
    pub lat: f64,
    pub lon: f64,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let adapter = Arc::new(TokioAdapter::new()?);
    
    let mut builder = AimDbBuilder::new()
        .runtime(adapter.clone())
        .with_remote_access(AimxConfig::uds_default().socket_path("/tmp/weather.sock"));
    
    // Register with source indexing, source, and taps all in one place
    builder.configure::<WeatherReading>(|reg| {
        reg.indexed_buffer(
            BufferCfg::indexed_latest(),
            |r: &WeatherReading| Some(r.station_id),  // Index by station ID
        )
        .with_serialization()
        // Data source: fetch from Open-Meteo
        .source(|_ctx, producer| async move {
            loop {
                for station_id in 1..=250 {
                    let reading = fetch_weather(StationId(station_id)).await;
                    producer.produce(reading).await.unwrap();
                }
                tokio::time::sleep(Duration::from_secs(60)).await;
            }
        })
        // Wildcard tap: log all readings + handle critical stations
        .tap(|_ctx, consumer| async move {
            while let Ok(reading) = consumer.recv().await {
                tracing::debug!(
                    station = reading.station_id.0,
                    temp = reading.temp_c,
                    "Reading received"
                );
                
                // Handle specific stations inline
                match reading.station_id {
                    StationId(42) => {
                        println!("Station 42: {:.1}°C, {:.0}% humidity", 
                            reading.temp_c, reading.humidity_pct);
                    }
                    StationId(1) if reading.temp_c > 35.0 => {
                        send_heat_alert(reading.clone()).await;
                    }
                    _ => {}
                }
            }
        });
    });
    
    let db = builder.build().await?;
    
    // API: get latest for any station (ad-hoc query)
    let latest = db.get_indexed::<WeatherReading, StationId>(&StationId(42))?;
    println!("Latest from 42: {:?}", latest);
    
    // API: list all stations
    let stations = db.sources::<WeatherReading, StationId>()?;
    println!("Active stations: {:?}", stations);
    
    // API: get all latest readings (snapshot)
    let snapshot = db.snapshot_indexed::<WeatherReading, StationId>()?;
    for (station_id, reading) in snapshot {
        println!("{}: {:.1}°C", station_id.0, reading.temp_c);
    }
    
    // Keep running
    std::future::pending::<()>().await;
    Ok(())
}

async fn fetch_weather(station_id: StationId) -> WeatherReading {
    // Fetch from Open-Meteo API...
    todo!()
}

async fn send_heat_alert(reading: WeatherReading) {
    // Send alert via webhook, SMS, etc.
    todo!()
}
```

## Appendix B: Spatial Indexing Example

```rust
// Index by 0.5° grid cells for regional queries
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct GridCell {
    pub lat_bucket: i16,
    pub lon_bucket: i16,
}

impl GridCell {
    pub fn from_coords(lat: f64, lon: f64) -> Option<Self> {
        const RESOLUTION: f64 = 0.5;  // 0.5 degree grid (~55km at equator)
        // Filter invalid coordinates
        if !lat.is_finite() || !lon.is_finite() {
            return None;
        }
        Some(GridCell {
            lat_bucket: (lat / RESOLUTION).floor() as i16,
            lon_bucket: (lon / RESOLUTION).floor() as i16,
        })
    }
    
    pub fn center(&self) -> (f64, f64) {
        const RESOLUTION: f64 = 0.5;
        (
            (self.lat_bucket as f64 + 0.5) * RESOLUTION,
            (self.lon_bucket as f64 + 0.5) * RESOLUTION,
        )
    }
}

// Registration with source and spatial tap
builder.configure::<WeatherReading>(|reg| {
    reg.indexed_buffer(
        BufferCfg::indexed_latest(),
        |r: &WeatherReading| GridCell::from_coords(r.lat, r.lon),
    )
    .with_serialization()
    .source(|_ctx, producer| async move {
        // Fetch weather data from multiple sources
        fetch_european_stations(producer).await;
    });
});

// Query: get latest reading for the grid cell containing Vienna
let vienna_cell = GridCell::from_coords(48.2082, 16.3738).unwrap();
let reading = db.get_indexed::<WeatherReading, GridCell>(&vienna_cell)?;

// Subscribe to all readings in the Vienna area (ad-hoc, outside configure)
let mut vienna_readings = db.subscribe_source::<WeatherReading, GridCell>(vienna_cell)?;
```
