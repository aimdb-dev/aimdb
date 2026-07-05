# Remote Access Demo

This example demonstrates the AimX v1 remote access protocol with `record.list` functionality.

## What It Does

**Server** (`server.rs`):
- Creates an AimDB instance with 6 record types (Temperature, TemperatureFahrenheit, SystemStatus, UserEvent, Config, AppSettings)
- Enables remote access on Unix domain socket `/tmp/aimdb-demo.sock`
- Uses ReadWrite security policy (`server::AppSettings` is the only writable key)
- Drives `Temperature` and `SystemStatus` from in-AimDB `.source()` tasks with named `.tap()` consumers, so the `profiling` feature can time every stage automatically (see [Stage Profiling](#stage-profiling) below)
- Derives `TemperatureFahrenheit` from `Temperature` via `.transform()`, so the `profiling` feature also times single-input transform stages

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

`TemperatureFahrenheit` is derived from `Temperature` via `.transform()` instead —
it has no `.source()` of its own:

| Record                 | Transform stage        |
|------------------------|-------------------------|
| TemperatureFahrenheit  | `celsius_to_fahrenheit` |

`.source()`/`.tap()` stages are timed as the wall-clock interval *between* calls
(the actual callback loops internally, so AimDB times the gap between
`produce()`/`recv()` calls). A single-input `.transform()` stage is timed
differently: AimDB owns the call boundary directly, so it wraps the transform
closure itself — `celsius_to_fahrenheit`'s `avg_time_ns` is the per-call cost of
converting one `Temperature` into one `TemperatureFahrenheit`, not the gap
between conversions.

Once the server has been running for a few seconds, query stage profiling via the
`aimdb-mcp` server using the `get_stage_profiling` tool with `record_key="SystemStatus"`.
The result is a list of `{call_count, avg_time_ns, min_time_ns, max_time_ns, name, ...}`
entries plus a `bottleneck` field — for SystemStatus the bottleneck will point at
`slow_status_processor` with an average around 100 ms. Query
`record_key="TemperatureFahrenheit"` to see the `celsius_to_fahrenheit` transform
stage reported the same way, with `stage_type: "transform"`. Reset the counters
between windows with the `reset_stage_profiling` tool.

You can also call the raw RPC method directly:

```bash
# reset the counters (requires write permission, ReadWrite policy already enabled)
echo '{"id":1,"method":"profiling.reset"}' | socat - UNIX-CONNECT:/tmp/aimdb-demo.sock
```

The per-stage snapshot is also embedded in each record's metadata via the
`stage_profiling` field of `record.list`.

## Buffer Metrics

The server is also built with the `metrics` feature, so every buffer tracks
`produced_count` / `consumed_count` / `dropped_count` / `occupancy`. The
`Temperature` and `SystemStatus` records exercise this naturally — the
simulators produce on a 2 s / 5 s cadence and the taps consume.

After letting the server run for ~30 seconds, query buffer metrics via the
`aimdb-mcp` `get_buffer_metrics` tool with `record_key="SystemStatus"`. Each
matching record's `buffer_metrics` field will look like:

```json
{
  "produced_count": 6,
  "consumed_count": 5,
  "dropped_count": 0,
  "occupancy": [1, 50]
}
```

`produced - consumed` reflects the slow consumer's lag; `occupancy` is
`(current_items, capacity)`. The same fields are embedded in each entry of
`record.list`, so you can also see them via the client demo.

### Reset end-to-end

A small companion binary, [`verify_buffer_metrics`](src/verify_buffer_metrics.rs),
exercises the full reset round-trip via `AimxClient`. Start the server, let it
run for ~10 seconds, then in a separate terminal:

```bash
cargo run --package remote-access-demo --bin verify_buffer_metrics
```

Expected output:

```
🔌 Connecting to /tmp/aimdb-demo.sock
📊 Before reset: produced=49 consumed=49 occupancy=Some((0, 50))
🧹 Calling buffer_metrics.reset
   response: {"reset":true}
📊 After  reset: produced=0 consumed=0 occupancy=Some((0, 50))
✅ buffer_metrics.reset verified end-to-end
```

The server's `ReadWrite` security policy permits the reset. To verify the
write-permission gate, change [server.rs](src/server.rs) `SecurityPolicy::read_write()`
to `SecurityPolicy::read_only()` and rerun the server — the verifier then exits
with:

```
Error: ServerError { code: "permission_denied", message: "buffer_metrics.reset requires write permission (ReadOnly security policy)", ... }
```

## Next Steps

Future enhancements will add:
- `record.get` - Get current value
- `record.set` - Set writable records
- `record.subscribe` - Stream live updates
- `instance.shutdown` - Graceful shutdown
