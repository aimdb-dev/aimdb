# aimdb-client

Internal client library for the AimX remote access protocol (v2 NDJSON wire).

## Overview

`aimdb-client` is an **internal library** that provides the Rust client
implementation for the AimX remote access protocol. It enables programmatic
connections to running AimDB instances over a transport picked at runtime via
a `scheme://` endpoint URL — Unix domain sockets (`unix://PATH` / `uds://PATH`,
or a bare path), serial (`serial://DEVICE?baud=N`), and TCP
(`tcp://HOST:PORT`, behind the `transport-tcp` feature).

**This library is used by:**
- `tools/aimdb-cli` - Command-line interface for AimDB
- `tools/aimdb-mcp` - Model Context Protocol server for LLM integration

**Not intended for direct use by application developers.** If you need to interact with AimDB, use the CLI or MCP tools instead.

## Features

- **Async Connection Management**: `AimxConnection` over the shared session engine
- **Protocol Implementation**: AimX handshake plus RPC and streaming subscriptions
- **Instance Discovery**: Automatic detection of running AimDB instances (UDS)
- **Record Operations**: List, get, set, subscribe, drain, graph introspection, query
- **Type-Safe**: Strongly typed API with serde integration

## API Overview

### Core Types

- `AimxConnection` - Main client for connecting to AimDB instances
- `InstanceInfo` - Information about discovered instances
- `RecordMetadata` - Metadata about registered records
- `ClientError` - Error types for client operations

### Main Operations

- **Discovery**: `discover_instances()`, `find_instance()`
- **Connection**: `AimxConnection::connect(endpoint)`, `connect_over(dialer)`
- **Records**: `list_records()`, `get_record()` (a `Stamped<Value>`: `.value` and
  `.stamp()`), `set_record()`, `drain_record()` (each value a `Stamped<Value>`)
- **Subscriptions**: `subscribe()` (returns a `Stream` of `Result<RecordUpdate, ClientError>` —
  value, write stamp `ts`, topic, gap count, snapshot-burst marker; a refused subscription is
  one `Err` item)
- **Introspection**: `graph_nodes()`, `graph_edges()`, `graph_topo_order()`, `query()`
- **Identity** (since protocol 3.1): `instance_info()` (build info, instance name and UID),
  `post_link()` (the written post-link bytes)

### Discovery

Automatically scans for running AimDB instances:
- `/tmp/*.sock`
- `/var/run/aimdb/*.sock`

## Protocol

The client speaks the **AimX** wire: NDJSON (newline-delimited JSON) tagged
frames mapping onto the session engine's role-neutral message set. It declares
`PROTOCOL_VERSION` (currently `4.0`) at `hello`; compatibility is by major
version, so a 3.x peer is refused at the handshake (and a WebSocket upgrade
with `?v=3.x`).

### Write timestamps (AimX 4.0)

Every record value on the wire carries `ts`, the time it was written into the
server instance ([design 058](../docs/design/058-write-timestamps.md)). It is
mandatory: a value frame or reply without it is a protocol error
(`ClientError::Protocol`).

| Message | Shape |
|---|---|
| `event` | `{"t":"event","sub","seq","topic"?,"ts","data"}` |
| `snap` | `{"t":"snap","sub","seq","topic","last"?,"ts","data"}` |
| `record.get` reply | `{"value": <value>, "ts": <ns>}` |
| `record.drain` reply | `{"record_name", "values": [{"value", "ts"}…], "count"}` |
| `record.set` reply | `{"status", "value", "ts"}`, or `{"status"}` when the record has no readable latest value |

```json
{"t":"event","sub":"7","seq":42,"ts":81234567000,"data":{"celsius":21.5}}
```

- **What `ts` is:** integer nanoseconds since the server instance started, on
  its monotonic clock. It says when the value was written into that instance:
  a source value when produced, a transform or join output when computed, an
  inbound connector value when ingested, a `record.set` when applied. It is
  not wall-clock time, and not when a sensor measured; put measurement time in
  the record's own data.
- **Scope:** stamps compare only within one instance lifetime. A restart
  starts again near zero, so reset timestamp comparisons on every reconnect.
- **Order:** the server's `seq` is the authority on order; `ts` is the
  authority on time. Values written concurrently can arrive with stamps out
  of order by the width of that race.
- **JavaScript:** `ts` passes 2⁵³ after about 104 days of uptime, beyond what
  `JSON.parse` represents exactly. Browser code that parses frames itself
  should read `ts` as a `BigInt` (or with a big-number-aware JSON parser).
  Rust clients and the WASM bindings are unaffected.

See `docs/design/remote-access-via-connectors.md` for the architecture and
`aimdb-core/src/session/aimx/` for the codec.

## Usage Examples

This library is used internally by:

- **`tools/aimdb-cli`** - Command-line interface for interacting with AimDB instances
- **`tools/aimdb-mcp`** - Model Context Protocol server for LLM integration

See these tools for real-world usage patterns.

## Testing

```bash
# Run tests
cargo test -p aimdb-client

# Run TCP endpoint resolver tests
cargo test -p aimdb-client --no-default-features --features transport-tcp

# Start test server
cargo run --example remote-access-demo
```

## Documentation

For detailed API documentation:
```bash
cargo doc -p aimdb-client --open
```

## License

See [LICENSE](../LICENSE) file.
