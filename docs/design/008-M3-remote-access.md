# AimX v1: Remote Access Protocol Specification

**Version:** 1.0  
**Status:** Design Document  
**Last Updated:** October 31, 2025

---

## Table of Contents

- [Overview](#overview)
- [Protocol Primitives](#protocol-primitives)
- [Request Methods](#request-methods)
- [Protocol Details](#protocol-details)
- [Security Model](#security-model)
- [Example Session](#example-session)
- [Error Handling](#error-handling)
- [Implementation Architecture](#implementation-architecture)
- [Performance Considerations](#performance-considerations)
- [Future Extensions](#future-extensions)

---

## Overview

**AimX (Aim eXchange)** is a lightweight request/stream protocol for remote introspection and interaction with AimDB instances. Version 1.0 provides read-only access by default, with optional write capabilities for specific records.

### Design Goals

1. **Simple** - NDJSON over Unix domain sockets, no binary protocols
2. **Secure** - Read-only default, OS-level permissions, optional auth tokens
3. **Efficient** - Bounded queues, drop-oldest policy, zero-copy where possible
4. **Evolvable** - Version negotiation, capability announcement
5. **Observable** - Foundation for CLI, dashboards, MCP integration

### Non-Goals (v1)

- ❌ Binary protocols (may come in v2)
- ❌ TCP/WebSocket transport (gateway work)
- ❌ Global control operations (shutdown, reload)
- ❌ Multi-instance coordination
- ❌ Distributed transactions

---

## Protocol Primitives

AimX v1 defines five message types:

### 1. `hello` - Client Handshake

Client announces version and capabilities upon connection.

```json
{
  "hello": {
    "version": "1.0",
    "client": "aimdb-cli/0.1.0",
    "capabilities": ["read", "subscribe"]
  }
}
```

**Fields:**
- `version` (string, required): Protocol version (semantic versioning)
- `client` (string, required): Client identification string
- `capabilities` (array<string>, optional): Desired capabilities

### 2. `welcome` - Server Response

Server responds with version, permissions, and server info.

```json
{
  "welcome": {
    "version": "1.0",
    "server": "aimdb/0.3.0",
    "permissions": ["read", "subscribe"],
    "writable_records": [],
    "max_subscriptions": 10
  }
}
```

**Fields:**
- `version` (string, required): Server protocol version
- `server` (string, required): Server identification string  
- `permissions` (array<string>, required): Granted capabilities
- `writable_records` (array<string>, required): Records that allow writes
- `max_subscriptions` (integer, optional): Per-connection subscription limit

### 3. `request` - Client Command

Client sends a request with unique ID.

```json
{
  "id": 1,
  "method": "record.list",
  "params": {}
}
```

**Fields:**
- `id` (integer, required): Unique request identifier (client-assigned)
- `method` (string, required): Method name (see [Request Methods](#request-methods))
- `params` (object, optional): Method-specific parameters

### 4. `response` - Server Reply

Server replies with matching ID and result or error.

```json
{
  "id": 1,
  "result": { ... }
}
```

Or on error:

```json
{
  "id": 1,
  "error": {
    "code": "NOT_FOUND",
    "message": "Record 'SensorData' not found"
  }
}
```

**Fields:**
- `id` (integer, required): Matches request ID
- `result` (any, conditional): Success result (mutually exclusive with `error`)
- `error` (object, conditional): Error details

**Error Object:**
- `code` (string, required): Error code (see [Error Codes](#error-codes))
- `message` (string, required): Human-readable error message
- `details` (any, optional): Additional error context

### 5. `event` - Server Push

Server pushes subscription updates asynchronously.

```json
{
  "event": {
    "subscription_id": "sub-123",
    "sequence": 42,
    "data": { ... },
    "timestamp": "2025-10-31T12:34:56.789Z"
  }
}
```

**Fields:**
- `subscription_id` (string, required): Subscription identifier from subscribe response
- `sequence` (integer, required): Monotonic sequence number per subscription
- `data` (any, required): Record value (JSON-serialized)
- `timestamp` (string, required): ISO 8601 timestamp with milliseconds
- `dropped` (integer, optional): Number of dropped events since last delivery (backpressure)

---

## Request Methods

### `record.list`

Lists all registered records with metadata.

**Request:**
```json
{
  "id": 1,
  "method": "record.list",
  "params": {}
}
```

**Response:**
```json
{
  "id": 1,
  "result": {
    "records": [
      {
        "name": "SensorData",
        "type_id": "0x7f8a4c2b1e90",
        "buffer_type": "spmc_ring",
        "buffer_capacity": 100,
        "producer_count": 1,
        "consumer_count": 2,
        "writable": false,
        "created_at": "2025-10-31T10:00:00.000Z",
        "last_update": "2025-10-31T12:34:56.789Z"
      }
    ]
  }
}
```

**Result Fields:**
- `records` (array): List of record metadata objects

**Record Metadata Fields:**
- `name` (string): Record type name (Rust type name)
- `type_id` (string): Hex representation of TypeId
- `buffer_type` (string): Buffer type: `"spmc_ring"`, `"single_latest"`, or `"mailbox"`
- `buffer_capacity` (integer, optional): Buffer capacity (null for unbounded)
- `producer_count` (integer): Number of registered producers
- `consumer_count` (integer): Number of registered consumers
- `writable` (boolean): Whether write operations are permitted
- `created_at` (string): ISO 8601 timestamp when record was registered
- `last_update` (string, nullable): ISO 8601 timestamp of last value update

### `record.get`

Retrieves current snapshot of a record.

**Request:**
```json
{
  "id": 2,
  "method": "record.get",
  "params": {
    "name": "SensorData"
  }
}
```

**Response:**
```json
{
  "id": 2,
  "result": {
    "value": {
      "temperature": 23.5,
      "humidity": 45.2,
      "timestamp": 1698753296
    },
    "timestamp": "2025-10-31T12:34:56.789Z",
    "sequence": 42
  }
}
```

**Request Parameters:**
- `name` (string): Record type name

**Result Fields:**
- `value` (any): Current record value (JSON-serialized)
- `timestamp` (string): When value was updated
- `sequence` (integer): Value sequence number (monotonic per record)

**Errors:**
- `NOT_FOUND` - Record doesn't exist
- `NO_VALUE` - Record has no value yet (no producer has written)

### `record.subscribe`

Subscribes to real-time updates for a record.

**Request:**
```json
{
  "id": 3,
  "method": "record.subscribe",
  "params": {
    "name": "SensorData",
    "send_initial": true
  }
}
```

**Response:**
```json
{
  "id": 3,
  "result": {
    "subscription_id": "sub-123",
    "queue_size": 100
  }
}
```

**Request Parameters:**
- `name` (string, required): Record type name
- `send_initial` (boolean, optional, default: true): Send current value immediately

**Result Fields:**
- `subscription_id` (string): Unique subscription identifier
- `queue_size` (integer): Bounded queue size for this subscription

**Events:**

After subscription, server sends `event` messages:

```json
{
  "event": {
    "subscription_id": "sub-123",
    "sequence": 1,
    "data": { "temperature": 23.5, "humidity": 45.2 },
    "timestamp": "2025-10-31T12:34:56.789Z"
  }
}
```

If backpressure occurs:

```json
{
  "event": {
    "subscription_id": "sub-123",
    "sequence": 50,
    "data": { "temperature": 24.1, "humidity": 46.0 },
    "timestamp": "2025-10-31T12:35:10.123Z",
    "dropped": 5
  }
}
```

**Errors:**
- `NOT_FOUND` - Record doesn't exist
- `TOO_MANY_SUBSCRIPTIONS` - Client exceeded max subscriptions
- `NO_BUFFER` - Record has no buffer configured

### `record.unsubscribe`

Cancels an active subscription.

**Request:**
```json
{
  "id": 4,
  "method": "record.unsubscribe",
  "params": {
    "subscription_id": "sub-123"
  }
}
```

**Response:**
```json
{
  "id": 4,
  "result": {}
}
```

**Request Parameters:**
- `subscription_id` (string): Subscription to cancel

**Errors:**
- `NOT_FOUND` - Subscription doesn't exist

### `record.set` (Optional - Write Mode)

Sets a record value (only allowed if explicitly enabled).

**Request:**
```json
{
  "id": 5,
  "method": "record.set",
  "params": {
    "name": "ConfigRecord",
    "value": { "log_level": "debug" }
  }
}
```

**Response:**
```json
{
  "id": 5,
  "result": {
    "sequence": 10,
    "timestamp": "2025-10-31T12:35:20.000Z"
  }
}
```

**Request Parameters:**
- `name` (string): Record type name
- `value` (any): New value (must match record schema)

**Result Fields:**
- `sequence` (integer): New sequence number
- `timestamp` (string): When value was set

**Errors:**
- `NOT_FOUND` - Record doesn't exist
- `PERMISSION_DENIED` - Record not writable
- `VALIDATION_ERROR` - Value doesn't match schema
- `INTERNAL_ERROR` - Failed to serialize/produce value

---

## Protocol Details

### Framing: NDJSON

**Newline-Delimited JSON (NDJSON)**
- Each message is a single line of JSON
- Lines terminated with `\n` (LF, 0x0A)
- UTF-8 encoding
- No trailing commas, whitespace minimization recommended

**Example Stream:**
```
{"hello":{"version":"1.0","client":"aimdb-cli/0.1.0"}}\n
{"welcome":{"version":"1.0","server":"aimdb/0.3.0","permissions":["read"]}}\n
{"id":1,"method":"record.list","params":{}}\n
{"id":1,"result":{"records":[...]}}\n
```

### Versioning

**Semantic Versioning:**
- **Major**: Incompatible protocol changes
- **Minor**: Backward-compatible additions
- **Patch**: Bug fixes, clarifications

**Version Negotiation:**
1. Client sends `hello` with desired version
2. Server responds with `welcome` containing supported version
3. If versions incompatible, server sends error and closes connection

**Compatibility Rules:**
- Server MUST support client version within same major version
- Server MAY support older major versions
- Client SHOULD handle missing optional fields
- Server MUST reject unknown required fields

### Error Codes

| Code | Description | Retry Safe |
|------|-------------|-----------|
| `PROTOCOL_ERROR` | Malformed message, invalid JSON | No |
| `VERSION_MISMATCH` | Incompatible protocol versions | No |
| `NOT_FOUND` | Record or subscription not found | Yes |
| `PERMISSION_DENIED` | Operation not allowed | No |
| `QUEUE_FULL` | Subscription queue overflow | Yes (backoff) |
| `INTERNAL_ERROR` | Server internal error | Yes (backoff) |
| `TOO_MANY_SUBSCRIPTIONS` | Client exceeded subscription limit | No |
| `NO_VALUE` | Record has no current value | Yes |
| `NO_BUFFER` | Record has no buffer for subscription | No |
| `VALIDATION_ERROR` | Invalid parameter or value | No |
| `AUTH_REQUIRED` | Authentication token required | No |
| `AUTH_FAILED` | Invalid authentication token | No |

### Auth Model

**Optional Token-Based Authentication:**

When auth is enabled, client must include token in `hello`:

```json
{
  "hello": {
    "version": "1.0",
    "client": "aimdb-cli/0.1.0",
    "auth_token": "secret-token-here"
  }
}
```

Server validates and responds:

```json
{
  "welcome": {
    "version": "1.0",
    "server": "aimdb/0.3.0",
    "permissions": ["read", "subscribe"],
    "authenticated": true
  }
}
```

Or rejects:

```json
{
  "error": {
    "code": "AUTH_FAILED",
    "message": "Invalid authentication token"
  }
}
```

**Security Layers:**
1. **Primary**: UDS file permissions (owner/group/mode)
2. **Optional**: Auth token (simple bearer token for v1)
3. **Future**: JWT tokens, OAuth, mTLS (v2+)

### Subscription Semantics

**Bounded Queues:**
- Each subscription has a fixed-size queue (configured at server startup)
- Default: 100 events per subscription
- Drop policy: **Drop oldest** when queue full
- Client notified via `dropped` field in next event

**Delivery Guarantees:**
- **At-most-once**: Events may be dropped under backpressure
- **Ordered**: Events delivered in sequence (within same subscription)
- **Best-effort**: Server attempts delivery but doesn't block producers

**Backpressure Handling:**
1. Producer writes to record → triggers subscribers
2. If client queue full → oldest event dropped, `dropped` counter incremented
3. Next successfully queued event includes `dropped` field
4. Client can detect data loss and take action (reconnect, alert, etc.)

**Example Backpressure Event:**
```json
{
  "event": {
    "subscription_id": "sub-123",
    "sequence": 105,
    "data": { ... },
    "timestamp": "2025-10-31T12:40:00.000Z",
    "dropped": 20
  }
}
```

This indicates 20 events (sequence 85-104) were dropped.

---

## Security Model

### Principles

1. **Read-only by default** - No writes unless explicitly enabled
2. **Explicit opt-in for writes** - Per-record basis via builder
3. **UDS permissions primary** - Leverage OS-level security
4. **Auth optional but recommended** - Token-based for additional layer
5. **Permission-scoped channels** - Each connection has clear capabilities
6. **No global control ops in v1** - Introspection only
7. **Bounded resources** - Connection limits, queue limits prevent DoS

### Access Control Levels

**Level 0: No Access**
```bash
chmod 600 /var/run/aimdb/aimdb.sock  # Owner only
```

**Level 1: Read-Only (Default)**
```rust
.with_remote_access(
    AimxConfig::uds_default()
        .security_policy(SecurityPolicy::ReadOnly)
)
```

Permissions: `["read", "subscribe"]`

**Level 2: Read-Write (Explicit Opt-In)**
```rust
.with_remote_access(
    AimxConfig::uds_default()
        .security_policy(SecurityPolicy::ReadWrite)
        .allow_write_to("AdminConfigRecord")
        .allow_write_to("FeatureFlagRecord")
)
```

Permissions: `["read", "subscribe", "write"]`  
Writable records announced in `welcome` message.

### Auth Token Configuration

```rust
.with_remote_access(
    AimxConfig::uds_default()
        .auth_token("secret-token-from-env")
        .security_policy(SecurityPolicy::ReadOnly)
)
```

**Token Best Practices:**
- Store in environment variables, not hardcoded
- Rotate regularly
- Use different tokens for different deployments
- Consider per-client tokens for audit trails (future)

### Permission Announcement

Server must announce capabilities in `welcome`:

```json
{
  "welcome": {
    "version": "1.0",
    "server": "aimdb/0.3.0",
    "permissions": ["read", "subscribe", "write"],
    "writable_records": ["AdminConfigRecord", "FeatureFlagRecord"]
  }
}
```

Clients use this to:
- Display available operations to users
- Avoid sending forbidden requests
- Implement client-side validation

---

## Example Session

### Full Session: List, Get, Subscribe

```json
→ {"hello":{"version":"1.0","client":"aimdb-cli/0.1.0"}}\n
← {"welcome":{"version":"1.0","server":"aimdb/0.3.0","permissions":["read","subscribe"],"writable_records":[],"max_subscriptions":10}}\n

→ {"id":1,"method":"record.list","params":{}}\n
← {"id":1,"result":{"records":[{"name":"SensorData","type_id":"0x7f8a4c2b1e90","buffer_type":"spmc_ring","buffer_capacity":100,"producer_count":1,"consumer_count":2,"writable":false,"created_at":"2025-10-31T10:00:00.000Z","last_update":"2025-10-31T12:34:56.789Z"}]}}\n

→ {"id":2,"method":"record.get","params":{"name":"SensorData"}}\n
← {"id":2,"result":{"value":{"temperature":23.5,"humidity":45.2},"timestamp":"2025-10-31T12:34:56.789Z","sequence":42}}\n

→ {"id":3,"method":"record.subscribe","params":{"name":"SensorData","send_initial":true}}\n
← {"id":3,"result":{"subscription_id":"sub-123","queue_size":100}}\n
← {"event":{"subscription_id":"sub-123","sequence":43,"data":{"temperature":23.5,"humidity":45.2},"timestamp":"2025-10-31T12:34:56.789Z"}}\n
← {"event":{"subscription_id":"sub-123","sequence":44,"data":{"temperature":23.6,"humidity":45.1},"timestamp":"2025-10-31T12:35:00.123Z"}}\n
← {"event":{"subscription_id":"sub-123","sequence":45,"data":{"temperature":23.7,"humidity":45.0},"timestamp":"2025-10-31T12:35:05.456Z"}}\n

→ {"id":4,"method":"record.unsubscribe","params":{"subscription_id":"sub-123"}}\n
← {"id":4,"result":{}}\n
```

### Error Handling Example

**Request for non-existent record:**
```json
→ {"id":10,"method":"record.get","params":{"name":"NonExistent"}}\n
← {"id":10,"error":{"code":"NOT_FOUND","message":"Record 'NonExistent' not found"}}\n
```

**Permission denied:**
```json
→ {"id":11,"method":"record.set","params":{"name":"SensorData","value":{...}}}\n
← {"id":11,"error":{"code":"PERMISSION_DENIED","message":"Record 'SensorData' is not writable"}}\n
```

**Backpressure notification:**
```json
← {"event":{"subscription_id":"sub-123","sequence":150,"data":{...},"timestamp":"2025-10-31T12:40:00.000Z","dropped":25}}\n
```

### Manual Testing with `nc`

```bash
# Terminal 1: Start AimDB with remote access
cargo run --example remote-access-demo

# Terminal 2: Connect and interact
nc -U /tmp/aimdb.sock
{"hello":{"version":"1.0","client":"manual-test"}}
# Server responds with welcome

{"id":1,"method":"record.list"}
# Server responds with record list

{"id":2,"method":"record.get","params":{"name":"SensorData"}}
# Server responds with current value

{"id":3,"method":"record.subscribe","params":{"name":"SensorData"}}
# Server responds with subscription_id, then streams events
```

---

## Implementation Architecture

### Task Structure

```
AimDB Instance
├─ RemoteSupervisor (1 task)
│  ├─ Binds to UDS path
│  ├─ Accepts connections
│  ├─ Spawns ConnectionHandler per client
│  └─ Tracks active connections
│
└─ ConnectionHandler (1 per client)
   ├─ Protocol handshake
   ├─ Request routing
   ├─ Subscription management
   └─ Connection cleanup
```

### RemoteSupervisor Responsibilities

1. **Socket Management**
   - Create UDS at configured path
   - Set file permissions
   - Clean up on shutdown

2. **Connection Acceptance**
   - Accept incoming connections
   - Enforce connection limits
   - Spawn ConnectionHandler tasks

3. **Lifecycle Management**
   - Graceful shutdown coordination
   - Active connection tracking
   - Resource cleanup

### ConnectionHandler Responsibilities

1. **Handshake**
   - Validate `hello` message
   - Perform version negotiation
   - Verify auth token (if configured)
   - Send `welcome` response

2. **Request Processing**
   - Parse NDJSON messages
   - Route to appropriate handlers
   - Send responses with matching IDs
   - Handle errors gracefully

3. **Subscription Management**
   - Create per-subscription queues (bounded)
   - Subscribe to record buffers
   - Push events as they arrive
   - Track dropped events for backpressure

4. **Cleanup**
   - Unsubscribe from all active subscriptions
   - Close connection
   - Release resources

### Internal APIs (aimdb-core)

```rust
impl AimDbInner {
    /// List all registered records with metadata
    pub(crate) async fn list_records(&self) -> Vec<RecordMetadata>;

    /// Read current snapshot of a record by TypeId
    pub(crate) async fn read_record_snapshot(
        &self,
        type_id: TypeId
    ) -> DbResult<serde_json::Value>;

    /// Subscribe to record updates
    pub(crate) async fn subscribe_record(
        &self,
        type_id: TypeId,
        queue_size: usize,
    ) -> DbResult<RecordSubscription>;
}

pub struct RecordSubscription {
    pub rx: UnboundedReceiver<serde_json::Value>,
    pub unsubscribe: oneshot::Sender<()>,
}
```

### Configuration Types

```rust
pub struct AimxConfig {
    socket_path: PathBuf,
    security_policy: SecurityPolicy,
    max_connections: usize,
    subscription_queue_size: usize,
    auth_token: Option<String>,
}

pub enum SecurityPolicy {
    ReadOnly,
    ReadWrite {
        writable_records: HashSet<TypeId>,
    },
}
```

---

## Performance Considerations

### Zero Overhead When Disabled

- Remote access is **opt-in** via `.with_remote_access()`
- No supervisor task spawned if not configured
- Zero memory overhead in core database
- No performance impact on production deployments without remote access

### Minimal Overhead When Enabled, No Clients

- Single supervisor task parked on `accept()`
- Negligible memory: socket handle + configuration
- No CPU usage when idle
- Immediate response to connection attempts

### Per-Client Overhead

**Memory:**
- ConnectionHandler: ~8KB stack per task
- Subscription queue: `queue_size * value_size` (bounded)
- Default: 100 events * ~1KB = ~100KB per subscription
- Max connections: configurable, default 16

**CPU:**
- JSON serialization: ~1-10µs per event (depends on value size)
- Queue operations: ~100ns per event (bounded channel ops)
- Network I/O: dominated by syscall overhead (~1-10µs per write)

**Backpressure Protection:**
- Bounded queues prevent memory exhaustion
- Drop-oldest policy prevents blocking producers
- Clients notified of drops via `dropped` field

### Optimization Strategies

1. **Lazy Serialization**: Only serialize when client subscribed
2. **Batch Events**: Group multiple events per NDJSON line (future)
3. **Compression**: Optional gzip for large values (future)
4. **Zero-Copy**: Use `serde_json::RawValue` for passthrough (future)

---

## Future Extensions

### v1.1 - Minor Enhancements

- **Batch Operations**: `record.get_many`, `record.subscribe_many`
- **Filtering**: Server-side subscription filters (reduce bandwidth)
- **Pagination**: `record.list` with cursor-based pagination
- **Metrics**: Built-in AimX metrics as records

### v2.0 - Major Extensions

- **Binary Protocol**: MessagePack, CBOR, or custom format
- **WebSocket Transport**: Remote access via gateway
- **JWT Authentication**: Stronger auth with claims
- **Write Transactions**: Multi-record atomic writes
- **Admin Operations**: Shutdown, reload, runtime control

### v3.0 - Distributed Features

- **Multi-Instance Discovery**: Query across multiple AimDB instances
- **Distributed Subscriptions**: Subscribe to records across cluster
- **Consensus Integration**: Raft/Paxos for write coordination
- **Federation**: Cross-datacenter synchronization

---

## Appendix A: Type Mappings

### Rust → JSON Serialization

| Rust Type | JSON Type | Example |
|-----------|-----------|---------|
| `bool` | `boolean` | `true`, `false` |
| `i32`, `i64`, `u32`, `u64` | `number` | `42`, `-17` |
| `f32`, `f64` | `number` | `3.14`, `2.718` |
| `String`, `&str` | `string` | `"hello"` |
| `Option<T>` | `T \| null` | `42`, `null` |
| `Vec<T>` | `array` | `[1, 2, 3]` |
| `HashMap<K, V>` | `object` | `{"key": "value"}` |
| Struct (with `#[derive(Serialize)]`) | `object` | `{"field": value}` |
| Enum (with `#[derive(Serialize)]`) | Various | Depends on serde attributes |

### TypeId Representation

TypeId is a 64-bit hash in Rust. For JSON, represent as:

- **Hexadecimal string**: `"0x7f8a4c2b1e90"`
- **Decimal string**: `"140241734827664"` (less readable, avoid)

Recommended: Use hex strings for consistency with debugging tools.

---

## Appendix B: Reference Implementation

See `aimdb-core/src/remote/` for complete implementation.

**Key files:**
- `mod.rs` - Public API exports
- `config.rs` - Configuration types
- `supervisor.rs` - RemoteSupervisor task
- `handler.rs` - ConnectionHandler task
- `protocol.rs` - Message types and parsing
- `errors.rs` - Protocol error codes

**Examples:**
- `examples/remote-access-basic/` - Minimal setup
- `examples/remote-access-client/` - Rust client implementation
- `examples/remote-access-secure/` - Auth token + permissions

---

## Appendix C: Testing Checklist

### Unit Tests

- [ ] Socket creation and cleanup
- [ ] Handshake protocol (valid and invalid)
- [ ] Version negotiation (compatible and incompatible)
- [ ] Error response formatting
- [ ] Permission checks (read-only vs read-write)
- [ ] Subscription queue overflow (drop-oldest)
- [ ] Auth token validation

### Integration Tests

- [ ] Full flow: connect → handshake → list → get → subscribe → unsubscribe
- [ ] Read-only enforcement (reject writes)
- [ ] Backpressure handling (dropped events)
- [ ] Multiple concurrent clients
- [ ] Graceful shutdown (active connections)
- [ ] Socket cleanup on server exit

### Fuzzing Tests

- [ ] Invalid JSON (malformed, truncated)
- [ ] Missing required fields
- [ ] Wrong field types
- [ ] Very large messages (>1MB)
- [ ] Connection drops mid-request
- [ ] Rapid connect/disconnect cycles
- [ ] Subscription spam (many subscriptions)

### Manual Tests

- [ ] `nc -U /tmp/aimdb.sock` - basic connectivity
- [ ] CLI tool against live instance
- [ ] Dashboard subscription under load
- [ ] Permission changes (file mode)
- [ ] Auth token rotation
- [ ] Multiple concurrent subscriptions

---

## References

- [NDJSON Specification](http://ndjson.org/)
- [JSON-RPC 2.0 Specification](https://www.jsonrpc.org/specification) (inspiration)
- [Semantic Versioning](https://semver.org/)
- [Unix Domain Sockets](https://en.wikipedia.org/wiki/Unix_domain_socket)

---

**End of AimX v1 Specification**
