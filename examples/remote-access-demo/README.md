# Remote Access Demo

This example demonstrates the AimX v1 remote access protocol with `record.list` functionality.

## What It Does

**Server** (`server.rs`):
- Creates an AimDB instance with 5 record types (Temperature, SystemStatus, UserEvent, Config, AppSettings)
- Enables remote access on Unix domain socket `/tmp/aimdb-demo.sock`
- Uses ReadWrite security policy (`server::AppSettings` is the only writable key)
- Drives `Temperature` and `SystemStatus` from in-AimDB `.source()` tasks with named `.tap()` consumers, so the `profiling` feature can time every stage automatically (see [Stage Profiling](#stage-profiling) below)

**Client** (`client.rs`):
- Connects to the server via Unix domain socket
- Performs protocol handshake (Hello/Welcome)
- Calls `record.list` method
- Displays all registered records with their metadata

## Running the Demo

### Terminal 1 - Start the Server

```bash
cargo run --example remote-access-demo --bin server
```

You should see:
```
🚀 Starting AimDB Remote Access Demo Server
📡 Remote access will be available at: /tmp/aimdb-demo.sock
🔒 Security policy: ReadOnly
✅ Database initialized with 4 record types
📝 Populated initial record data
🎯 Server ready!
```

### Terminal 2 - Run the Client

```bash
cargo run --example remote-access-demo --bin client
```

You should see:
```
🔌 Connecting to AimDB server...
✅ Connected!
📤 Sending handshake...
📥 Received welcome from server: aimdb
📤 Requesting record list...
✅ Success!
📋 Registered Records:
[
  {
    "name": "Temperature",
    "type_id": "...",
    "buffer_type": "none",
    "producer_count": 1,
    "consumer_count": 0,
    "writable": false,
    ...
  },
  ...
]
```

## Manual Testing with `socat`

You can also test manually using `socat`:

```bash
# Send handshake
echo '{"version":"1.0","client":"test"}' | socat - UNIX-CONNECT:/tmp/aimdb-demo.sock

# Send record.list request (after handshake)
(echo '{"version":"1.0","client":"test"}'; sleep 0.1; echo '{"id":1,"method":"record.list"}') | socat - UNIX-CONNECT:/tmp/aimdb-demo.sock
```

## What to Observe

- **Handshake**: Client sends Hello, server responds with Welcome
- **Permissions**: Server reports ReadOnly permissions
- **Record Metadata**: Each record shows:
  - Type name (Rust struct name)
  - TypeId (unique identifier)
  - Buffer configuration
  - Producer/consumer counts
  - Write permissions
  - Timestamps

## Stage Profiling

The server is built with the `profiling` feature (see [Cargo.toml](Cargo.toml)) and
registers both `Temperature` and `SystemStatus` via `.source()` + `.tap()` so AimDB
owns the producer/consumer tasks and can time them automatically. Each stage is
named via `.with_name("...")`:

| Record         | Source stage         | Tap stage                                    |
|----------------|----------------------|----------------------------------------------|
| Temperature    | `temp_simulator`     | `temp_logger` (fast)                         |
| SystemStatus   | `status_simulator`   | `slow_status_processor` (sleeps 100 ms each) |

Once the server has been running for a few seconds, query stage profiling via the
`aimdb-mcp` server using the `get_stage_profiling` tool with `record_key="SystemStatus"`.
The result is a list of `{call_count, avg_time_ns, min_time_ns, max_time_ns, name, ...}`
entries plus a `bottleneck` field — for SystemStatus the bottleneck will point at
`slow_status_processor` with an average around 100 ms. Reset the counters between
windows with the `reset_stage_profiling` tool.

You can also call the raw RPC method directly:

```bash
# reset the counters (requires write permission, ReadWrite policy already enabled)
echo '{"id":1,"method":"profiling.reset"}' | socat - UNIX-CONNECT:/tmp/aimdb-demo.sock
```

The per-stage snapshot is also embedded in each record's metadata via the
`stage_profiling` field of `record.list`.

## Next Steps

Future enhancements will add:
- `record.get` - Get current value
- `record.set` - Set writable records
- `record.subscribe` - Stream live updates
- `instance.shutdown` - Graceful shutdown
