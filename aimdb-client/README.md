# aimdb-client

Internal client library for the AimX remote access protocol (v2 NDJSON wire).

## Overview

`aimdb-client` is an **internal library** that provides the Rust client
implementation for the AimX remote access protocol. It enables programmatic
connections to running AimDB instances over a transport picked at runtime via
a `scheme://` endpoint URL — Unix domain sockets (`unix://PATH` / `uds://PATH`,
or a bare path) and serial (`serial://DEVICE?baud=N`).

**This library is used by:**
- `tools/aimdb-cli` - Command-line interface for AimDB
- `tools/aimdb-mcp` - Model Context Protocol server for LLM integration

**Not intended for direct use by application developers.** If you need to interact with AimDB, use the CLI or MCP tools instead.

## Features

- **Async Connection Management**: `AimxConnection` over the shared session engine
- **Protocol Implementation**: AimX-v2 handshake plus RPC and streaming subscriptions
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
- **Records**: `list_records()`, `get_record()`, `set_record()`, `drain_record()`
- **Subscriptions**: `subscribe()` (returns a `Stream` of values)
- **Introspection**: `graph_nodes()`, `graph_edges()`, `graph_topo_order()`, `query()`

### Discovery

Automatically scans for running AimDB instances:
- `/tmp/*.sock`
- `/var/run/aimdb/*.sock`

## Protocol

The client speaks the **AimX v2** wire: NDJSON (newline-delimited JSON) tagged
frames mapping onto the session engine's role-neutral message set. It is not
backward-compatible with the legacy AimX v1 framing.

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
