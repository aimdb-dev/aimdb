# AimX v1.1: Record History API

**Version:** 2.0  
**Status:** 📋 Proposed  
**Last Updated:** February 6, 2026  
**Milestone:** M8 — History & Time-Series Access  
**Depends On:** [008-M3-remote-access](008-M3-remote-access.md), [009-M4-mcp-integration](009-M4-mcp-integration.md)

---

## Table of Contents

- [Summary](#summary)
- [Motivation](#motivation)
- [Design Constraints](#design-constraints)
- [Architecture](#architecture)
- [Implementation](#implementation)
- [AimX Protocol Extension](#aimx-protocol-extension)
- [Client Library Extension](#client-library-extension)
- [MCP Tool Extension](#mcp-tool-extension)
- [Memory Budget](#memory-budget)
- [Testing Strategy](#testing-strategy)
- [Implementation Plan](#implementation-plan)
- [Alternatives Considered](#alternatives-considered)
- [Open Points & Roadblocks](#open-points--roadblocks)

---

## Summary

Add a `record.drain` method to the AimX v1.1 protocol that returns all values
accumulated in a record's ring buffer since the last drain call. This uses a
**dedicated drain reader** — a `BufferReader` created lazily on the first
`record.drain` request that persists for the server's lifetime, draining
accumulated values via non-blocking `try_recv()` on subsequent calls.

No separate accumulator, no background task, no extra memory. The ring buffer
itself serves as the history store. Timestamps are carried inside the data
structs themselves, so no additional metadata wrapping is needed.

---

## Motivation

### The Problem

Today, AimX provides two ways to read record data:

1. **`record.get`** — returns the single latest snapshot (atomic peek)
2. **`record.subscribe`** — streams future values from the point of subscription

Neither supports the query: *"Give me all temperature readings from the last 12 hours."*

The SPMC ring buffer does retain historical values internally, but:

- Only accessible via a `BufferReader` that was subscribed **before** the values
  were written (the cursor starts at the subscribe point, not the ring tail)
- Ring capacity is finite — a capacity-100 ring with 30-min writes only holds
  ~50 hours, but a capacity-20 forecast ring at 6h intervals holds only ~5 days
- No filtering by timestamp — consumers see raw sequence, not time ranges
- **No remote access** — ring contents are only available in-process

### The Use Case

The weather forecast confidence predictor (see `aimdb-pro` design doc 005) needs
to perform a **12-hour batch analysis** every heartbeat:

| Data | Source Record | Write Interval | Readings per 12h |
|------|--------------|----------------|-------------------|
| Actual temperatures | `temp.{city}` | ~30 min | ~24 |
| Actual humidity | `humidity.{city}` | ~30 min | ~24 |
| Forecast updates | `forecast.{city}.24h` | ~6 hours | ~2 |
| Own past predictions | `prediction.{city}` | ~12 hours | ~1 |
| Validation results | `validation.{city}` | ~12 hours | ~1 |

The agent must read the **full time-series** (not just latest) to:

- Detect temperature trends (warming/cooling rate)
- Calculate min/max/variance over the window
- Count forecast changes (stability analysis)
- Find the actual temperature at a specific past time (validation)
- Review its own prediction history (self-improvement)

### Why a Dedicated Drain Reader?

The SPMC ring buffer already retains historical values — a capacity-100 ring
with 30-minute writes holds ~50 hours of data. The problem is access: a remote
MCP client calling `record.subscribe` starts receiving from the **current
tail**, missing all previously written values.

The solution is straightforward: create a `BufferReader` on the first
`record.drain` request and keep it alive for the connection's lifetime. This
reader's cursor starts at the point of creation and advances only when values
are explicitly drained. Between drain calls, values accumulate naturally in
the ring buffer — no separate storage needed.

> **Note:** The first `record.drain` call returns empty (the reader was just
> created and has no backlog). Subsequent calls return all values written
> since the previous drain. This is a one-time cold-start cost per connection.
> If a client disconnects and reconnects, a new drain reader is created and
> the first drain returns empty again. For long-lived connections (typical
> for MCP agents), this is a non-issue.

**Why drain semantics (not peek)?**

- **Intentionally destructive.** Each consumer maintains its own read cursor.
  Draining advances the cursor, so the next drain returns only new values.
  This matches the batch-query pattern: read everything since last check,
  process it, repeat.
- **One reader per consumer.** The UI websocket gets its own `subscribe()` +
  `recv()` reader. The MCP agent gets its own drain reader. They don't
  interfere with each other. Additional consumers can be added via
  configuration (each gets a dedicated reader slot on the ring buffer).
- **Ring capacity bounds history depth.** This is acceptable when properly
  sized — capacity-100 at 30-minute intervals gives ~50 hours of lookback,
  far exceeding the 12-hour batch window needed by the prediction agent.

---

## Design Constraints

1. **No external database** — All history lives in the ring buffer's in-process memory
2. **Minimal buffer trait extension** — Add `try_recv()` to `BufferReader` and
   `try_recv_json()` to `JsonBufferReader` (non-breaking additions)
3. **Bounded memory** — Ring buffer capacity IS the history depth; no separate storage
4. **no_std compatible** — `try_recv()` is a natural operation on embedded ring buffers
5. **AimX protocol backward-compatible** — New method, existing methods unchanged
6. **Zero overhead when unused** — Drain readers are created lazily on first
   `record.drain` call; no impact on records that are never drained
7. **Destructive reads by design** — Each drain reader is a dedicated consumer slot
   on the ring buffer; drain advances that reader's cursor only

---

## Architecture

### Component Diagram

```
┌───────────────────────────────────────────────────────────────────┐
│  AimDB Instance (weather-hub process)                             │
│                                                                   │
│  ┌──────────────────┐                                             │
│  │ TypedRecord<T>   │                                             │
│  │                  │                                             │
│  │  buffer.push(v)──┼──► SPMC Ring Buffer (capacity: 100)         │
│  │                  │         │                                    │
│  │  subscribe() ────┼─────────┤                                    │
│  │                  │         │                                    │
│  │  latest()        │         ├──► UI Reader (recv() loop → WS)   │
│  └──────────────────┘         │                                    │
│                               ├──► Drain Reader (created lazily    │
│                               │    on first record.drain call)     │
│                               │                                    │
│                               └──► [Future readers as needed]      │
│                                                                    │
│  ┌─────────────────────────────────────────────────────────────┐  │
│  │  AimX Handler                                               │  │
│  │                                                             │  │
│  │  record.list          → existing                            │  │
│  │  record.get           → existing (reads latest_snapshot)    │  │
│  │  record.set           → existing                            │  │
│  │  record.subscribe     → existing                            │  │
│  │  record.unsubscribe   → existing                            │  │
│  │  record.drain         → NEW (try_recv loop on drain reader) │  │
│  └──────────────────────┬──────────────────────────────────────┘  │
│                         │                                          │
│                         │ AimX Socket                               │
│                         ▼                                          │
│  ┌─────────────────────────────────────────────────────────────┐  │
│  │  aimdb-client / aimdb-mcp                                   │  │
│  │                                                             │  │
│  │  client.drain_record("temp.berlin")                         │  │
│  │  → returns Vec<serde_json::Value>                           │  │
│  └─────────────────────────────────────────────────────────────┘  │
└───────────────────────────────────────────────────────────────────┘
```

### Data Flow

```
Producer writes value
       │
       ▼
  TypedRecord::push(value)
       │
       ├──► Ring buffer slot N
       │       │
       │       ├──► UI reader: recv() → serialize → WebSocket push
       │       │
       │       └──► Drain reader (if created): cursor stays at last drain position
       │
       └──► latest_snapshot (existing) ──► record.get


  Remote client sends: record.drain("temp.berlin")
       │
       ▼
  AimX handler → DrainReaders::drain()
       │
       ├── First call? → subscribe_json() → store reader
       │                  (returns empty on first drain)
       │
       └── Subsequent calls → reuse stored reader
              │
              ▼
         try_recv_json() in loop
              │
              ├── Ok(value) → collect into Vec
              ├── Err(BufferEmpty) → break, return collected values
              ├── Err(BufferLagged) → log warning, continue (cursor resets)
              └── Err(BufferClosed) → break
```

### Key Insight: Ring Buffer as History Store

The ring buffer already stores exactly the data we need. The only missing
piece is a way to **batch-read accumulated values** without blocking. Today,
`BufferReader::recv()` is async and blocks until the next value arrives —
there is no way to say "give me everything you have right now, then stop."

Adding `try_recv()` (non-blocking, returns `BufferEmpty` when caught up)
completes the API and enables the drain pattern with zero additional storage:

- **No separate VecDeque** — The ring buffer IS the history store
- **No background reader task** — The drain reader sits idle between requests
- **No duplicate data** — Values exist once, in the ring buffer
- **Natural memory bound** — Ring capacity limits retention (already configured)
- **Buffer-type aware** — `try_recv()` on `SingleLatest` returns at most one
  value; on `Mailbox` returns the pending slot; on `SpmcRing` returns the full
  backlog. Each buffer type's semantics are preserved.

---

## Implementation

### 6.1 Trait Extension: `try_recv()`

Add a non-blocking receive method to both buffer reader traits:

```rust
// aimdb-core/src/buffer/traits.rs

pub trait BufferReader<T: Clone + Send>: Send {
    /// Async receive — blocks until next value is available.
    fn recv(&mut self) -> Pin<Box<dyn Future<Output = Result<T, DbError>> + Send + '_>>;
    
    /// Non-blocking receive — returns immediately.
    /// Returns `Err(DbError::BufferEmpty)` if no pending values.
    fn try_recv(&mut self) -> Result<T, DbError>;
}

pub trait JsonBufferReader: Send {
    /// Async receive as JSON — blocks until next value.
    fn recv_json(
        &mut self,
    ) -> Pin<Box<dyn Future<Output = Result<serde_json::Value, DbError>> + Send + '_>>;
    
    /// Non-blocking receive as JSON — returns immediately.
    /// Returns `Err(DbError::BufferEmpty)` if no pending values.
    fn try_recv_json(&mut self) -> Result<serde_json::Value, DbError>;
}
```

### 6.2 New Error Variant

```rust
// aimdb-core/src/error.rs

pub enum DbError {
    // ... existing variants ...
    
    /// Non-blocking receive found no pending values.
    BufferEmpty,
}
```

### 6.3 Tokio Adapter Implementation

Map `try_recv()` to the underlying `tokio::sync::broadcast::Receiver::try_recv()`:

```rust
// aimdb-tokio-adapter/src/buffer.rs

impl<T: Clone + Send + Sync + 'static> BufferReader<T> for TokioBufferReader<T> {
    // ... existing recv() ...
    
    fn try_recv(&mut self) -> Result<T, DbError> {
        match self {
            TokioBufferReader::Broadcast { rx, .. } => {
                match rx.try_recv() {
                    Ok(val) => Ok(val),
                    Err(broadcast::error::TryRecvError::Empty) => {
                        Err(DbError::BufferEmpty)
                    }
                    Err(broadcast::error::TryRecvError::Lagged(n)) => {
                        Err(DbError::BufferLagged {
                            lag_count: n,
                            buffer_name: "broadcast".to_string(),
                        })
                    }
                    Err(broadcast::error::TryRecvError::Closed) => {
                        Err(DbError::BufferClosed {
                            buffer_name: "broadcast".to_string(),
                        })
                    }
                }
            }
            TokioBufferReader::Watch { rx, .. } => {
                if rx.has_changed().unwrap_or(false) {
                    let val = rx.borrow_and_update().clone();
                    match val {
                        Some(v) => Ok(v),
                        None => Err(DbError::BufferEmpty),
                    }
                } else {
                    Err(DbError::BufferEmpty)
                }
            }
            TokioBufferReader::Notify { slot, .. } => {
                let mut guard = slot.lock().unwrap();
                match guard.take() {
                    Some(val) => Ok(val),
                    None => Err(DbError::BufferEmpty),
                }
            }
        }
    }
}
```

### 6.4 Embassy Adapter Implementation

> **⚠️ Prerequisite refactor:** The current `EmbassyBufferReader::recv()` for
> `SpmcRing` creates a **new subscriber on every call** via `channel.subscriber()`.
> This means there is no persistent cursor — each call starts fresh at the
> current tail. For `try_recv()` (and `recv()` correctness in general), the
> reader must store a persistent `Subscriber` for SpmcRing, just as it already
> stores a persistent `watch_receiver` for Watch buffers.
>
> This is a prerequisite refactor that improves `recv()` correctness as well.
> It follows the exact same pattern as the existing `watch_receiver` field.

Add a persistent `spmc_subscriber` field to `EmbassyBufferReader` and use it
for both `recv()` and `try_recv()`:

```rust
// aimdb-embassy-adapter/src/buffer.rs

pub struct EmbassyBufferReader<T, const CAP: usize, const SUBS: usize, const PUBS: usize, const WATCH_N: usize> {
    buffer: Arc<EmbassyBufferInner<T, CAP, SUBS, PUBS, WATCH_N>>,
    /// Persistent Watch receiver (existing)
    watch_receiver: Option<embassy_sync::watch::Receiver<'static, CriticalSectionRawMutex, T, WATCH_N>>,
    /// Persistent SpmcRing subscriber (NEW — same pattern as watch_receiver)
    spmc_subscriber: Option<embassy_sync::pubsub::Subscriber<'static, CriticalSectionRawMutex, T, CAP, SUBS, PUBS>>,
}

impl<T: Clone + Send + 'static, const CAP: usize, const SUBS: usize, const PUBS: usize, const WATCH_N: usize>
    BufferReader<T> for EmbassyBufferReader<T, CAP, SUBS, PUBS, WATCH_N>
{
    fn try_recv(&mut self) -> Result<T, DbError> {
        match &*self.buffer {
            EmbassyBufferInner::SpmcRing(channel) => {
                // Lazily create persistent subscriber (same pattern as watch_receiver)
                if self.spmc_subscriber.is_none() {
                    let channel_static: &'static _ = unsafe { &*(channel as *const _) };
                    self.spmc_subscriber = Some(
                        channel_static.subscriber().map_err(|_| DbError::BufferClosed { _buffer_name: () })?
                    );
                }
                match self.spmc_subscriber.as_mut().unwrap().try_next_message_pure() {
                    Some(val) => Ok(val),
                    None => Err(DbError::BufferEmpty),
                }
            }
            EmbassyBufferInner::Watch(_) => {
                if let Some(ref mut rx) = self.watch_receiver {
                    if rx.has_changed() {
                        Ok(rx.get().clone())
                    } else {
                        Err(DbError::BufferEmpty)
                    }
                } else {
                    Err(DbError::BufferEmpty)
                }
            }
            EmbassyBufferInner::Mailbox(channel) => {
                match channel.try_receive() {
                    Ok(val) => Ok(val),
                    Err(_) => Err(DbError::BufferEmpty),
                }
            }
        }
    }
}
```

> **Note:** The existing `recv()` for SpmcRing should also be refactored to use
> the persistent `spmc_subscriber` field. This is the same safety pattern as
> `watch_receiver` — the `Arc` keeps the channel alive for the reader's lifetime.
```

### 6.5 Drain Reader Storage

Drain readers live as **owned state on `ConnectionState`** — the same struct
that already holds per-client subscription handles. No `Arc`, no `Mutex`,
no shared state. This works because the `select!` loop in the connection
handler processes requests sequentially, so `&mut` access is always safe.

```rust
// aimdb-core/src/remote/handler.rs

use std::collections::HashMap;

/// Per-connection state — owned, not shared.
struct ConnectionState {
    subscriptions: HashMap<String, SubscriptionHandle>,
    next_subscription_id: u64,
    event_tx: mpsc::UnboundedSender<Event>,
    /// Per-record drain readers, created lazily on first record.drain call.
    /// One drain reader per record, per connection.
    drain_readers: HashMap<String, Box<dyn JsonBufferReader + Send>>,
}
```

The drain handler gets `&mut ConnectionState` naturally — exactly like
subscribe/unsubscribe do today:

```rust
/// Handle a record.drain request.
async fn handle_record_drain<R>(
    db: &Arc<AimDb<R>>,
    conn_state: &mut ConnectionState,
    request_id: u64,
    params: DrainParams,
) -> Response
where
    R: crate::RuntimeAdapter + crate::Spawn + 'static,
{
    let record_name = &params.name;
    
    // Lazily create drain reader on first call for this record
    if !conn_state.drain_readers.contains_key(record_name) {
        // Resolve record key → RecordId → AnyRecord → subscribe_json()
        // (same resolution path as handle_record_subscribe)
        let id = match db.inner().resolve_str(record_name) {
            Some(id) => id,
            None => return Response::error(request_id, "not_found",
                format!("Record '{}' not found", record_name)),
        };
        let record = match db.inner().storage(id) {
            Some(r) => r,
            None => return Response::error(request_id, "not_found",
                format!("Record '{}' storage not found", record_name)),
        };
        let reader = match record.subscribe_json() {
            Ok(r) => r,
            Err(e) => return Response::error(request_id, "remote_access_not_enabled",
                format!("Record '{}' not configured with .with_remote_access(): {}", record_name, e)),
        };
        conn_state.drain_readers.insert(record_name.clone(), reader);
    }
    
    let reader = conn_state.drain_readers.get_mut(record_name).unwrap();
    let limit = params.limit.unwrap_or(usize::MAX);
    let mut values = Vec::new();
    
    loop {
        if values.len() >= limit {
            break;
        }
        match reader.try_recv_json() {
            Ok(val) => values.push(val),
            Err(DbError::BufferEmpty) => break,
            Err(DbError::BufferLagged { .. }) => {
                // Ring overflowed since last drain — cursor resets
                // to oldest available. Log warning, keep draining.
                continue;
            }
            Err(_) => break,
        }
    }
    
    Ok(Response::success(request_id, json!({
        "record_name": record_name,
        "values": values,
        "count": values.len(),
    })))
}
```

**Lifecycle:**
- **Created:** Lazily on first `record.drain` call for a given record name
- **Cleaned up:** Automatically when the connection drops (`ConnectionState`
  is dropped, all drain readers are dropped with it)
- **Limit:** One drain reader per record per connection (enforced by `HashMap` key)
- **Independence:** Each connection gets its own drain cursors; multiple
  clients draining the same record do not interfere with each other
```

### 6.6 No Builder Changes Needed

Drain is a server-level capability that requires **no per-record configuration**.
It uses the existing `AimxConfig` and `.with_remote_access()` infrastructure:

```rust
// In weather-hub-streaming/src/main.rs

// Remote access — already configured, drain works automatically
let remote_config = AimxConfig::uds_default()
    .socket_path("/tmp/aimdb-weather.sock")
    .max_connections(5);

let mut builder = AimDbBuilder::new()
    .runtime(runtime)
    .with_remote_access(remote_config);

// Records just need .with_remote_access() (already required for any AimX read)
builder.configure::<Temperature>("temp.berlin", |reg| {
    reg.buffer(BufferCfg::SpmcRing { capacity: 100 })
        .with_remote_access()  // enables record.get, record.subscribe, AND record.drain
        .finish();
});
```

When the first `record.drain` request arrives for a record, the AimX handler:

1. Calls `subscribe_json()` on the record (same as `record.subscribe` does)
2. Stores the reader in the `DrainReaders` map, keyed by record name
3. Immediately drains it (returns empty on the very first call, since the
   reader was just created and has no backlog yet)
4. On subsequent `record.drain` calls, the stored reader is reused —
   `try_recv_json()` returns all values accumulated since the last drain

This mirrors how `record.subscribe` already creates readers lazily on-demand.
Drain readers follow the same per-connection lifecycle as subscription readers:
created on first use, dropped on disconnect. Each client gets independent
drain cursors — draining from one connection does not affect another.

---

## AimX Protocol Extension

### New Method: `record.drain`

**Request:**
```json
{
  "id": 7,
  "method": "record.drain",
  "params": {
    "name": "temp.berlin"
  }
}
```

**Response:**
```json
{
  "id": 7,
  "result": {
    "record_name": "temp.berlin",
    "values": [
      { "schema_version": 2, "celsius": 3.2, "timestamp": 1738722600000 },
      { "schema_version": 2, "celsius": 2.8, "timestamp": 1738724400000 },
      { "schema_version": 2, "celsius": 3.5, "timestamp": 1738726200000 }
    ],
    "count": 3
  }
}
```

**Request Parameters:**
- `name` (string, required): Record name (same as `record.get`)
- `limit` (integer, optional): Maximum number of values to drain (default: all pending)

**Result Fields:**
- `record_name` (string): Echo of the queried record name
- `values` (array): Chronologically ordered values (raw JSON, as written by the producer)
- `count` (integer): Number of values returned

**Errors:**
- `NOT_FOUND` — Record doesn't exist
- `REMOTE_ACCESS_NOT_ENABLED` — Record exists but `.with_remote_access()` was
  not configured (required for any AimX value read including drain)
- `INVALID_PARAMS` — Invalid parameter value

**Important:** Values are returned as-is from the ring buffer, without wrapper
metadata. Timestamps, sequence numbers, or any other temporal information must
be part of the data struct itself (e.g., `Temperature { celsius: f64, timestamp: u64 }`).

### Protocol Compatibility

This is a **purely additive** change:

- New method name (`record.drain`) — existing clients never send this
- No changes to existing methods or their responses
- Clients that don't know about drain simply never call it
- Server that doesn't support drain returns `method_not_found` (existing error path)

---

## Client Library Extension

### aimdb-client

```rust
// aimdb-client/src/connection.rs

impl AimxClient {
    /// Drain all pending values from a record's drain reader.
    ///
    /// Returns all values accumulated since the last drain call,
    /// in chronological order. This is a destructive read — drained
    /// values will not be returned again.
    ///
    /// # Errors
    /// - Record not found
    /// - Drain not enabled for this record
    /// - Connection error
    pub async fn drain_record(
        &mut self,
        name: &str,
    ) -> ClientResult<DrainResponse> {
        let params = serde_json::json!({ "name": name });
        let result = self.send_request("record.drain", Some(params)).await?;
        Ok(serde_json::from_value(result)?)
    }
    
    /// Drain with a limit on the number of values returned.
    pub async fn drain_record_with_limit(
        &mut self,
        name: &str,
        limit: u32,
    ) -> ClientResult<DrainResponse> {
        let params = serde_json::json!({
            "name": name,
            "limit": limit,
        });
        let result = self.send_request("record.drain", Some(params)).await?;
        Ok(serde_json::from_value(result)?)
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct DrainResponse {
    pub record_name: String,
    pub values: Vec<serde_json::Value>,
    pub count: usize,
}
```

---

## MCP Tool Extension

### aimdb-mcp: `drain_record` Tool

```rust
// tools/aimdb-mcp/src/tools/record.rs (add to existing file alongside get_record, set_record)

/// Parameters for drain_record tool
#[derive(Debug, Clone, Serialize, Deserialize)]
struct DrainRecordParams {
    /// Unix socket path to the AimDB instance
    socket_path: String,
    /// Name of the record to drain
    record_name: String,
    /// Maximum number of values to drain (optional)
    #[serde(skip_serializing_if = "Option::is_none")]
    limit: Option<u32>,
}

/// Drain all pending values from a record since the last drain call.
/// Returns values in chronological order. This is a destructive read.
pub async fn drain_record(args: Value) -> McpResult<Vec<Content>> {
    let params: DrainRecordParams = serde_json::from_value(args)
        .map_err(|e| McpError::invalid_params(format!("Invalid params: {}", e)))?;
    
    debug!("Draining record: {} from {}", params.record_name, params.socket_path);
    
    // Use connection pool (matches existing get_record/set_record pattern)
    let pool = super::connection_pool()
        .ok_or_else(|| McpError::internal("Connection pool not initialized"))?;
    let mut client = pool.get_or_connect(&params.socket_path).await?;
    
    let response = match params.limit {
        Some(limit) => client.drain_record_with_limit(&params.record_name, limit).await?,
        None => client.drain_record(&params.record_name).await?,
    };
    
    let summary = format!(
        "Record: {}\nValues drained: {}",
        response.record_name,
        response.count,
    );
    
    Ok(vec![Content::text(format!(
        "{}\n\n{}",
        summary,
        serde_json::to_string_pretty(&response.values)?
    ))])
}
```

### Tool Registration

Add to the tool list in `server.rs`:

```rust
Tool {
    name: "drain_record".to_string(),
    description: Some(
        "Drain all pending values from a record since the last drain call. \
         Returns values in chronological order. This is a destructive read — \
         drained values won't be returned again. Use this for batch analysis \
         of accumulated data. Requires socket_path and record_name.".to_string()
    ),
    input_schema: json!({
        "type": "object",
        "required": ["socket_path", "record_name"],
        "properties": {
            "socket_path": {
                "type": "string",
                "description": "Path to AimDB Unix socket"
            },
            "record_name": {
                "type": "string",
                "description": "Name of the record (e.g., 'temp.berlin')"
            },
            "limit": {
                "type": "integer",
                "description": "Maximum number of values to drain. Optional."
            }
        }
    }),
}
```

---

## Memory Budget

The drain-based approach requires **zero additional memory** beyond the ring
buffer that is already configured. The drain reader is just a cursor position
into the existing ring — it does not copy or duplicate data.

### Ring Buffer as History Depth

| Record Type | Count | Capacity | Write Interval | History Depth | Drain Interval | Values per Drain |
|-------------|-------|----------|----------------|---------------|----------------|------------------|
| `temp.{city}` | 5 | 100 | ~30 min | ~50 hours | ~12 hours | ~24 |
| `humidity.{city}` | 5 | 100 | ~30 min | ~50 hours | ~12 hours | ~24 |
| `forecast.{city}.24h` | 5 | 20 | ~6 hours | ~5 days | ~12 hours | ~2 |
| `prediction.{city}` | 5 | 20 | ~12 hours | ~10 days | ~12 hours | ~1 |
| `validation.{city}` | 5 | 20 | ~12 hours | ~10 days | ~12 hours | ~1 |

**Safety margin:** With the prediction agent draining every ~12 hours and ring
capacities of 100, the ring holds ~50 hours of temperature data. The agent
only needs ~24 values per drain. Even if the agent misses a drain cycle, the
ring has 4× headroom before overflow.

### Comparison with Accumulator Approach

| | Drain-based (this design) | Accumulator (v1 design) |
|---|---|---|
| **Extra memory** | 0 | ~1.25 MB (25 records × 50 KB) |
| **Background tasks** | 0 | 25 (one per record) |
| **Data copies** | 0 (reads from ring) | 1 (ring → VecDeque) |
| **Complexity** | Low (trait + handler) | Medium (registry + tasks + store) |

---

## Testing Strategy

### Unit Tests

```rust
#[tokio::test]
async fn test_try_recv_empty_buffer() {
    let (tx, rx) = broadcast::channel::<i32>(16);
    let mut reader = TokioBufferReader::Broadcast { rx, /* ... */ };
    
    // No values written — try_recv returns Empty
    assert!(matches!(reader.try_recv(), Err(DbError::BufferEmpty)));
    
    // Write a value
    tx.send(42).unwrap();
    
    // Now try_recv succeeds
    assert_eq!(reader.try_recv().unwrap(), 42);
    
    // Buffer is empty again
    assert!(matches!(reader.try_recv(), Err(DbError::BufferEmpty)));
}

#[tokio::test]
async fn test_try_recv_drains_all_pending() {
    let (tx, rx) = broadcast::channel::<i32>(16);
    let mut reader = TokioBufferReader::Broadcast { rx, /* ... */ };
    
    // Write 5 values
    for i in 0..5 {
        tx.send(i).unwrap();
    }
    
    // Drain all
    let mut values = Vec::new();
    loop {
        match reader.try_recv() {
            Ok(val) => values.push(val),
            Err(DbError::BufferEmpty) => break,
            Err(_) => panic!("unexpected error"),
        }
    }
    
    assert_eq!(values, vec![0, 1, 2, 3, 4]);
}

#[tokio::test]
async fn test_try_recv_handles_lag() {
    let (tx, rx) = broadcast::channel::<i32>(4); // small ring
    let mut reader = TokioBufferReader::Broadcast { rx, /* ... */ };
    
    // Write 10 values into capacity-4 ring — reader falls behind
    for i in 0..10 {
        tx.send(i).unwrap();
    }
    
    // First try_recv returns Lagged error
    let first = reader.try_recv();
    assert!(matches!(first, Err(DbError::BufferLagged { .. })));
    
    // Subsequent calls return the values still in the ring
    let mut values = Vec::new();
    loop {
        match reader.try_recv() {
            Ok(val) => values.push(val),
            Err(DbError::BufferEmpty) => break,
            Err(DbError::BufferLagged { .. }) => continue,
            Err(_) => panic!("unexpected error"),
        }
    }
    
    // Only the last ~4 values survive in the ring
    assert!(!values.is_empty());
    assert!(values.len() <= 4);
}
```

### Integration Tests

```rust
#[tokio::test]
async fn test_drain_via_aimx_protocol() {
    // 1. Start AimDB with drain-enabled records
    // 2. Write several values via producer
    // 3. Connect AimX client
    // 4. Call record.drain
    // 5. Verify returned values match written values (chronological order)
    // 6. Call record.drain again — should return empty (all drained)
}

#[tokio::test]
async fn test_drain_requires_remote_access() {
    // 1. Start AimDB with a record WITHOUT .with_remote_access()
    // 2. Call record.drain
    // 3. Verify REMOTE_ACCESS_NOT_ENABLED error
}

#[tokio::test]
async fn test_drain_independent_of_other_readers() {
    // 1. Configure record with drain reader + live subscriber
    // 2. Write 10 values
    // 3. Live subscriber reads all 10 via recv()
    // 4. Drain reader returns all 10 via record.drain
    // 5. Both readers consumed independently — no interference
}

#[tokio::test]
async fn test_drain_survives_between_calls() {
    // 1. Write 5 values
    // 2. Drain → returns 5
    // 3. Write 3 more values
    // 4. Drain → returns 3 (only new ones)
}

#[tokio::test]
async fn test_drain_with_ring_overflow() {
    // 1. Configure SpmcRing capacity=5 with drain reader
    // 2. Write 20 values without draining
    // 3. Drain — returns ≤5 values (oldest overwritten)
    // 4. Verify lag warning was logged
}

#[tokio::test]
async fn test_mcp_drain_record_tool() {
    // 1. Start AimDB + MCP server
    // 2. Write values
    // 3. Call MCP tool drain_record
    // 4. Verify response format matches MCP content spec
}
```

---

## Implementation Plan

### Phase 0: Embassy SpmcRing Persistent Subscriber Fix (standalone correctness fix)

> **This is an independent bug fix, not just a drain prerequisite.**
> The current `EmbassyBufferReader::recv()` for SpmcRing creates a new
> `channel.subscriber()` on every call. This causes three concrete bugs
> documented in [Open Point #4](#4-embassy-spmcring-persistent-subscriber-bug-fix).
> This phase should be done first and can be merged independently.

| File | Action | Description |
|------|--------|-------------|
| `aimdb-embassy-adapter/src/buffer.rs` | Modify | Add `spmc_subscriber: Option<Subscriber<'static, ...>>` field to `EmbassyBufferReader` (same pattern as existing `watch_receiver`) |
| `aimdb-embassy-adapter/src/buffer.rs` | Modify | Refactor `recv()` for `SpmcRing` to lazily init and reuse persistent subscriber |
| `aimdb-embassy-adapter/src/buffer.rs` | Modify | Update `subscribe()` in `EmbassyBuffer` to initialize `spmc_subscriber: None` |

**Deliverable:** `recv()` uses a persistent subscriber for SpmcRing. Existing
embedded cross-compilation passes (`cargo check --target thumbv7em-none-eabihf`).
No functional change to Watch or Mailbox paths.

### Phase 1: Buffer Trait Extension (aimdb-core)

| File | Action | Description |
|------|--------|-------------|
| `aimdb-core/src/buffer/traits.rs` | Modify | Add `try_recv()` to `BufferReader<T>` |
| `aimdb-core/src/buffer/traits.rs` | Modify | Add `try_recv_json()` to `JsonBufferReader` |
| `aimdb-core/src/error.rs` | Modify | Add `DbError::BufferEmpty` variant (+ no_std Display, `error_code()`, `with_context()` match arms) |

**Deliverable:** Traits compile, no implementors yet.

### Phase 2: Runtime Adapter Implementations

| File | Action | Description |
|------|--------|-------------|
| `aimdb-tokio-adapter/src/buffer.rs` | Modify | Implement `try_recv()` for `TokioBufferReader` (all 3 variants) |
| `aimdb-embassy-adapter/src/buffer.rs` | Modify | Implement `try_recv()` for `EmbassyBufferReader` (uses persistent subscriber from Phase 0) |
| `aimdb-core/src/typed_record.rs` | Modify | Add `try_recv_json()` to `JsonReaderAdapter` (delegates to `inner.try_recv()` + serialize) |

**Deliverable:** `try_recv()` works on both runtimes. Unit tests passing.

### Phase 3: Drain Infrastructure (aimdb-core)

| File | Action | Description |
|------|--------|-------------|
| `aimdb-core/src/remote/handler.rs` | Modify | Add `record.drain` handler, `DrainReaders` struct with lazy creation |
| `aimdb-core/src/remote/protocol.rs` | Modify | Add request/response types for drain |

**Deliverable:** `record.drain` works via AimX socket. No builder changes needed.

### Phase 4: Client Library + MCP (aimdb-client, aimdb-mcp)

| File | Action | Description |
|------|--------|-------------|
| `aimdb-client/src/connection.rs` | Modify | Add `drain_record()` and `drain_record_with_limit()` |
| `tools/aimdb-mcp/src/tools/record.rs` | Modify | Add `drain_record()` tool (alongside existing `get_record`, `set_record`) |
| `tools/aimdb-mcp/src/tools/mod.rs` | Modify | Register new tool |
| `tools/aimdb-mcp/src/server.rs` | Modify | Add tool to list + dispatch |

**Deliverable:** MCP agent can call `drain_record` via MCP.

### Phase 5: Weather Hub Verification (aimdb-pro)

| File | Action | Description |
|------|--------|-------------|
| `demo/weather-hub-streaming/src/main.rs` | Verify | Ensure records use `.with_remote_access()` (already the case) |

**Deliverable:** Running weather hub exposes drain for all serialized records — no code changes needed.

---

## Alternatives Considered

### A. Read Ring Buffer Contents Directly

**Idea:** Add a `snapshot()` or `drain_available()` method to the `DynBuffer` trait
that returns all values currently in the ring.

**Rejected because:**
- Requires changes to the `DynBuffer` trait (higher-impact than extending `BufferReader`)
- `tokio::sync::broadcast` doesn't expose internal storage; only subscribers can read
- `DynBuffer` is producer-facing; drain is a consumer-facing operation

### B. External Database (SQLite / Sled)

**Idea:** Persist all values to an on-disk database; query it for history.

**Rejected because:**
- Adds a dependency and operational complexity
- Overkill for in-memory data with finite retention
- Latency overhead for every write

### C. Create New Subscriber On-Demand

**Idea:** On `record.drain` request, create a new subscriber and call `try_recv()`
in a loop to drain whatever is still in the ring.

**Rejected because:**
- New subscribers via `tokio::sync::broadcast::subscribe()` start at the **current
  tail** — `try_recv()` returns `Empty` immediately, not historical values
- This is fundamentally different from the chosen approach, which creates the reader
  **at startup** so it sees every value from the beginning

### D. Per-Record Write-Ahead Log

**Idea:** Append every value to a per-record file; read from file on history query.

**Rejected as v1 scope creep because:**
- Good idea for persistence across restarts (future enhancement)
- Unnecessary for in-memory database with volatile data
- Could be added later as an optional persistence layer

### E. History Accumulator with Background Task (v1 of this design)

**Idea:** Spawn a persistent background reader task per record that subscribes to the
buffer and appends every value (as timestamped JSON) into a bounded `VecDeque`.
AimX queries this `VecDeque` with timestamp-based filtering (`since_ms`).

**Rejected in favor of drain-based approach because:**
- **Extra memory:** Duplicates ring buffer contents into a separate `VecDeque`
  (~1.25 MB for the weather hub deployment)
- **Background tasks:** One perpetually running task per history-enabled record
- **Over-engineered** for the single-agent batch-query use case
- **Adds complexity:** `HistoryRegistry`, `RecordHistory`, `HistoryConfig`,
  `HistoryEntry` types, plus timestamp management and eviction logic

The accumulator design does have advantages for multi-client non-destructive queries
and timestamp-based filtering. It could be revisited if these requirements emerge.

---

## Open Points & Roadblocks

### 1. Rename `with_serialization()` → `with_remote_access()`

**Status:** 🔴 Blocker — Must be completed BEFORE implementing drain

The existing method `with_serialization()` on `TypedRecordConfig` must be
renamed to `with_remote_access()` across the entire codebase. This is **not**
a new method — it is a rename of the existing one. Do NOT create a second
method; the old name must be removed.

**Files that reference `with_serialization()`:**

| File | Type | Count |
|------|------|-------|
| `aimdb-core/src/typed_record.rs` | **Definition** + doc comments | ~9 references (lines 9, 75, 113, 310, 349, 512, 518, 537, 719) |
| `aimdb-core/src/buffer/traits.rs` | Doc comment in `JsonBufferReader` | 1 reference (line 117) |
| `aimdb-core/src/remote/handler.rs` | Doc comment | 1 reference (line 632) |
| `aimdb-mqtt-connector/src/tokio_client.rs` | Doc comment example | 1 reference |
| `examples/remote-access-demo/src/server.rs` | Usage | 5 call sites |

Search for all occurrences with `grep -rn "with_serialization" --include="*.rs"`
and rename every one. Update doc comments to say "enables remote access" instead
of "enables JSON serialization." Run `make check` to verify no references remain.

### 2. `try_recv()` Missing from Buffer Reader Traits

**Status:** 🔴 Blocker

`BufferReader<T>` and `JsonBufferReader` only expose async `recv()`. The
non-blocking `try_recv()` method must be added to both traits in
`aimdb-core/src/buffer/traits.rs`. This is a cross-cutting change that affects:

- `aimdb-core/src/buffer/traits.rs` — Trait definitions
- `aimdb-core/src/typed_record.rs` — `JsonReaderAdapter::try_recv_json()` implementation
- `aimdb-tokio-adapter/src/buffer.rs` — Tokio implementation
- `aimdb-embassy-adapter/src/buffer.rs` — Embassy implementation (requires persistent subscriber refactor)

The underlying Tokio primitives support it (`broadcast::Receiver::try_recv()`,
`watch::Receiver::has_changed()`, `Mutex<Option<T>>::take()`). Embassy's
`try_next_message_pure()` is also available, but requires a persistent
`Subscriber` field that does not currently exist on `EmbassyBufferReader`
for SpmcRing (see Open Point #4).

### 3. `DbError::BufferEmpty` Variant Needed

**Status:** 🔴 Blocker

`try_recv()` needs a way to signal "no pending values." A new `DbError::BufferEmpty`
variant must be added to `aimdb-core/src/error.rs`. This also requires updating
error Display/Debug implementations and any exhaustive match blocks.

### 4. Embassy SpmcRing Persistent Subscriber Bug Fix

**Status:** 🔴 Blocker — Standalone correctness fix (not just a drain prerequisite)

**Root cause:** `EmbassyBufferReader::recv()` for `SpmcRing` calls
`channel.subscriber()` **inside every `recv()` invocation**, creating a fresh
subscriber each time. The subscriber is dropped at the end of the async block.
This is unlike Watch, which correctly stores a persistent `watch_receiver`.

The relevant code in `aimdb-embassy-adapter/src/buffer.rs`:
```rust
EmbassyBufferInner::SpmcRing(channel) => match channel.subscriber() {
    Ok(mut sub) => match sub.next_message().await { ... }
    // sub is dropped here — slot released, cursor lost
}
```

Compare with Watch, which stores persistent state:
```rust
if self.watch_receiver.is_none() {
    // lazily create and store persistent receiver
    self.watch_receiver = watch_static.receiver();
}
if let Some(ref mut rx) = self.watch_receiver { ... }
```

#### Bug 1: Subscriber Slot Churn on a Fixed-Size Pool

Every `recv()` call does `subscriber_count += 1` → await → drop →
`subscriber_count -= 1`. The `SUBS` const generic limits concurrent
subscribers. If the pool is sized for actual consumers (e.g., `SUBS=4`)
and 4 readers call `recv()` simultaneously, the next call gets
`MaximumSubscribersReached`. With persistent subscribers, each reader
holds exactly 1 slot for its lifetime — predictable and correct.

#### Bug 2: Premature Queue Eviction (Silent Data Loss)

When a temporary subscriber drops, `unregister_subscriber()` decrements
the pending-reader count on **every queued message** the subscriber never
read. If this drives a message's count to 0, it gets **popped from the
queue** — even if other persistent readers haven't consumed it yet.

From Embassy's `PubSubState::unregister_subscriber()`:
```rust
fn unregister_subscriber(&mut self, subscriber_next_message_id: u64) {
    self.subscriber_count -= 1;
    // All messages that haven't been read yet by this subscriber
    // must have their counter decremented
    let start_id = self.next_message_id - self.queue.len() as u64;
    // ... decrements counters, pops front items with count == 0
}
```

This silently destroys data in the ring buffer that other consumers
(subscriptions, drain readers, dispatcher tasks) may still need.

#### Bug 3: Missed Messages Between Calls

Since each `recv()` creates a subscriber at `next_message_id` (current
tail), any messages published between two `recv()` calls are invisible.
The reader has no cursor continuity. A sequence of:
1. `recv()` → sees message A
2. Producer publishes message B
3. `recv()` → creates new subscriber at current tail, waits for C
4. Message B is never seen

#### Why It "Works" Today

The `dispatcher_task()` method (the primary consumer pattern on Embassy)
creates **one reader** and loops calling `recv()` inside it. Because the
reader is reused, each `recv()` call enters the same `Box::pin(async { })`
block and creates a fresh subscriber — but the tight loop means the
window for missed messages is small, and the single-task pattern means
slot churn doesn't hit the limit.

However, any usage that calls `recv()` from different contexts, or
multiple concurrent readers on the same buffer, will hit these bugs.
The drain feature requires persistent cursors by design, making this
fix mandatory.

#### Fix

Add a `spmc_subscriber` field to `EmbassyBufferReader`, following the
exact same pattern as the existing `watch_receiver` field:

```rust
pub struct EmbassyBufferReader<T, const CAP: usize, const SUBS: usize, const PUBS: usize, const WATCH_N: usize> {
    buffer: Arc<EmbassyBufferInner<T, CAP, SUBS, PUBS, WATCH_N>>,
    watch_receiver: Option<embassy_sync::watch::Receiver<'static, CriticalSectionRawMutex, T, WATCH_N>>,
    /// NEW: Persistent SpmcRing subscriber (same pattern as watch_receiver)
    spmc_subscriber: Option<embassy_sync::pubsub::Subscriber<'static, CriticalSectionRawMutex, T, CAP, SUBS, PUBS>>,
}
```

Refactor `recv()` for SpmcRing to lazily init and reuse the persistent subscriber:
```rust
EmbassyBufferInner::SpmcRing(channel) => {
    if self.spmc_subscriber.is_none() {
        // SAFETY: Same pattern as watch_receiver — Arc keeps channel alive.
        let channel_static: &'static _ = unsafe { &*(channel as *const _) };
        self.spmc_subscriber = Some(
            channel_static.subscriber()
                .map_err(|_| DbError::BufferClosed { _buffer_name: () })?
        );
    }
    match self.spmc_subscriber.as_mut().unwrap().next_message().await {
        WaitResult::Message(value) => Ok(value),
        WaitResult::Lagged(n) => Err(DbError::BufferLagged {
            lag_count: n, _buffer_name: ()
        }),
    }
}
```

Then `try_recv()` uses the same persistent subscriber:
```rust
fn try_recv(&mut self) -> Result<T, DbError> {
    match &*self.buffer {
        EmbassyBufferInner::SpmcRing(_) => {
            // Lazily init same as recv() — omitted for brevity
            match self.spmc_subscriber.as_mut().unwrap().try_next_message_pure() {
                Some(value) => Ok(value),
                None => Err(DbError::BufferEmpty),
            }
        }
        // Watch and Mailbox variants...
    }
}
```

#### Scope & Risk

- **Pattern:** Identical to the existing `watch_receiver` approach — same
  `unsafe` lifetime extension, same lazy init, same `Option<>` field
- **Impact on `recv()`:** Fixes all three bugs above; no behavioral change
  for the `dispatcher_task()` happy path
- **Impact on `try_recv()`:** Enables it — persistent cursor is required
- **Risk:** Low — the `unsafe` block is the same one already reviewed and
  used for `watch_receiver`. The `Arc` lifetime guarantee is identical.
- **Testing:** Existing `cargo check --target thumbv7em-none-eabihf` validates
  compilation. Functional tests require Embassy executor (existing test
  infrastructure).
- **Standalone PR:** Yes — this can and should be merged before the drain
  feature. It fixes pre-existing bugs.

> **Note:** Embassy's `try_next_message_pure()` silently skips lag, while
> Tokio's `try_recv()` returns `TryRecvError::Lagged(n)` that we must handle.
> This is an acceptable asymmetry — both adapters surface the same
> `BufferReader::try_recv()` trait, but lag reporting differs. On Embassy,
> lag is invisible to the drain caller. On Tokio, we log a warning and
> continue draining.

### ~~5. Drain Reader Lifecycle & Cleanup~~

**Status:** ✅ Resolved

**Decision: Per-connection owned state, no shared state.**

Drain readers are a plain `HashMap<String, Box<dyn JsonBufferReader + Send>>`
field on `ConnectionState` — the same struct that already holds per-client
subscription handles. No `Arc`, no `Mutex`, no shared state.

This works because the AimX handler's `select!` loop processes requests
sequentially within a connection, so `&mut ConnectionState` access is always
safe. This matches how subscriptions already work.

**Lifecycle rules:**
- **Created:** Lazily on first `record.drain` call for a given record name
- **Cleaned up on disconnect:** Automatically — `ConnectionState` is dropped
  when the connection closes, taking all drain readers with it
- **Cleaned up on unregister:** Explicitly removed from the `HashMap` if a
  record is unregistered during a live connection
- **Limit:** One drain reader per record, per connection (enforced by `HashMap` key)
- **Multi-client:** Each connection gets independent drain cursors; multiple
  clients draining the same record do not interfere

**Tradeoff:** Drain readers are per-connection, not per-server. If a client
disconnects and reconnects, the drain reader is recreated and the first drain
returns empty (cold start). This is acceptable because:
- MCP agents typically maintain long-lived connections
- Cold start cost is one empty drain — same as the very first drain anyway
- Avoids all shared state and synchronization complexity
- Matches the existing subscription lifecycle model exactly

### ~~6. `try_recv_json()` Adapter Plumbing~~

**Status:** ✅ Resolved — Straightforward

`JsonBufferReader` is a type-erased wrapper around `BufferReader<T>` that
serializes values to JSON. The existing `recv_json()` delegates to `recv()`
internally. The new `try_recv_json()` follows the same pattern — delegate to
`try_recv()` and serialize the result:

```rust
// aimdb-core/src/typed_record.rs — in JsonReaderAdapter<T> impl
fn try_recv_json(&mut self) -> Result<serde_json::Value, DbError> {
    let value = self.inner.try_recv()?;
    (self.serializer)(&value).ok_or_else(|| DbError::RuntimeError {
        message: "Failed to serialize value to JSON".to_string(),
    })
}
```

> **Note:** This uses the same `(self.serializer)(&value)` pattern as the
> existing `recv_json()` implementation in `JsonReaderAdapter`, not
> `serde_json::to_value()`, because the serializer is the configured
> function from `.with_serialization()` / `.with_remote_access()`.

No design decisions needed — this is mechanical plumbing.

---

## References

- [008-M3-remote-access](008-M3-remote-access.md) — AimX v1 protocol specification
- [009-M4-mcp-integration](009-M4-mcp-integration.md) — MCP server architecture
- [tokio::sync::broadcast](https://docs.rs/tokio/latest/tokio/sync/broadcast/) — Underlying ring buffer implementation
- [embassy_sync::pubsub](https://docs.rs/embassy-sync/latest/embassy_sync/pubsub/) — Embassy PubSub channel

---

**End of Record History API Specification**
