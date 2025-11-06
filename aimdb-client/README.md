# aimdb-client

Internal client library for the AimX v1 protocol.

## Overview

`aimdb-client` is an **internal library** that provides Rust client implementation for the AimX v1 remote access protocol. It enables programmatic connections to running AimDB instances via Unix domain sockets.

**This library is used by:**
- `tools/aimdb-cli` - Command-line interface for AimDB
- `tools/aimdb-mcp` - Model Context Protocol server for LLM integration

**Not intended for direct use by application developers.** If you need to interact with AimDB, use the CLI or MCP tools instead.

## Features

- **Async Connection Management**: Non-blocking Unix socket communication
- **Protocol Implementation**: Full AimX v1 handshake and message handling
- **Instance Discovery**: Automatic detection of running AimDB instances
- **Record Operations**: List, get, set, and subscribe to records
- **Type-Safe**: Strongly typed API with serde integration

## API Overview

### Core Types

- `AimxClient` - Main client for connecting to AimDB instances
- `InstanceInfo` - Information about discovered instances
- `RecordMetadata` - Metadata about registered records
- `ClientError` - Error types for client operations

### Main Operations

- **Discovery**: `discover_instances()`, `find_instance()`
- **Connection**: `AimxClient::connect()`
- **Records**: `list_records()`, `get_record()`, `set_record()`
- **Subscriptions**: `subscribe()`, `unsubscribe()`, `receive_event()`

### Discovery

Automatically scans for running AimDB instances:
- `/tmp/*.sock`
- `/var/run/aimdb/*.sock`

### Error Types

- `ClientError::NoInstancesFound` - No running instances discovered
- `ClientError::ConnectionFailed` - Socket connection failed
- `ClientError::ServerError` - Server returned error response
- `ClientError::Io` - I/O operation failed
- `ClientError::Json` - JSON serialization failed

## Protocol

The client implements **AimX v1** protocol over Unix domain sockets:

- **Transport**: Unix domain sockets
- **Encoding**: NDJSON (Newline Delimited JSON)
- **Pattern**: JSON-RPC 2.0 style request/response

See `docs/design/008-M3-remote-access.md` for full protocol specification.

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

For protocol specification, see `docs/design/008-M3-remote-access.md`.

## License

See [LICENSE](../LICENSE) file.
