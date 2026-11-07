# AimDB CLI

Command-line interface for introspecting and managing running AimDB instances.

## Overview

The AimDB CLI is a thin client over the AimX remote access protocol, providing intuitive commands for:
- Discovering running AimDB instances
- Listing and inspecting records
- Getting current record values
- Watching records for live updates
- Setting writable record values

## Installation

Build from source:

```bash
cd /aimdb
cargo build --release -p aimdb-cli
```

The binary will be available at `target/release/aimdb`.

## Quick Start

### 1. Discover Running Instances

```bash
aimdb instance list
```

Example output:
```
┌──────────────────────┬────────────────┬──────────┬─────────┬──────────┬───────────────┐
│ Socket Path          │ Server Version │ Protocol │ Records │ Writable │ Authenticated │
├──────────────────────┼────────────────┼──────────┼─────────┼──────────┼───────────────┤
│ /tmp/aimdb-demo.sock │ aimdb          │ 1.0      │ 2       │ 0        │ no            │
└──────────────────────┴────────────────┴──────────┴─────────┴──────────┴───────────────┘
```

### 2. List All Records

```bash
aimdb record list
```

Example output:
```
┌─────────────────────┬────────────────────────────────────────────┬────────────────┬───────────────┬───────────┬───────────┬──────────┬────────┐
│ Name                │ Type ID                                    │ Schema         │ Buffer Type   │ Producers │ Consumers │ Writable │ Signal │
├─────────────────────┼────────────────────────────────────────────┼────────────────┼───────────────┼───────────┼───────────┼──────────┼────────┤
│ server::Temperature │ TypeId(0xaee15e261d918c67cee5a96c2f604ce0) │ temperature v2 │ single_latest │ 1         │ 2         │ no       │ -      │
├─────────────────────┼────────────────────────────────────────────┼────────────────┼───────────────┼───────────┼───────────┼──────────┼────────┤
│ server::Config      │ TypeId(0xc2af5c8376864a24e916c87f88505fac) │ -              │ mailbox       │ 0         │ 3         │ yes      │ -      │
└─────────────────────┴────────────────────────────────────────────┴────────────────┴───────────────┴───────────┴───────────┴──────────┴────────┘
```

`Schema` is the record's declared data contract (`name vN`), or `-` when the
record was configured without one. `--format json` carries it as
`schema_type` / `schema_version`.

### 3. Get Current Record Value

```bash
aimdb record get server::Temperature
```

Example output:
```json
{
  "celsius": 23.5,
  "sensor_id": "sensor-001",
  "timestamp": 1730379296
}
```

### 4. Watch a Record for Live Updates

```bash
aimdb watch server::Temperature
```

Example output:
```
📡 Watching record: server::Temperature (subscription: sub-123)
Press Ctrl+C to stop

2025-11-02 10:30:45.123 | seq:42 | {"celsius":23.5,"sensor_id":"sensor-001","timestamp":1730379296}
2025-11-02 10:30:47.456 | seq:43 | {"celsius":23.6,"sensor_id":"sensor-001","timestamp":1730379298}
2025-11-02 10:30:49.789 | seq:44 | {"celsius":23.7,"sensor_id":"sensor-001","timestamp":1730379300}
^C
✅ Stopped watching
```

### 5. Set a Writable Record

```bash
aimdb record set server::Config '{"log_level":"debug","max_connections":100}'
```

## Command Reference

All commands accept a global `--connect <ENDPOINT>` flag to choose the target
instance (see [Connecting to an Instance](#connecting-to-an-instance)). When
omitted, the CLI reads the `AIMDB_CONNECT` env var, then falls back to UDS
auto-discovery. `instance list` is discovery-only and ignores `--connect`.

### Instance Commands

#### `instance list`
List all running AimDB instances by scanning for Unix domain socket files.

```bash
aimdb instance list [--format <FORMAT>]
```

Options:
- `-f, --format <FORMAT>`: Output format (table, json, json-compact, yaml)

#### `instance info`
Show detailed information about a specific instance: its name and UID, the
build it runs (from the binary's build info, protocol 3.1), and its
permissions. Works over any endpoint, not just discovered sockets.

```bash
aimdb [--connect <ENDPOINT>] instance info [--format <FORMAT>] [--post-link]
```

Options:
- `-f, --format <FORMAT>`: Output format (table, json, json-compact, yaml)
- `--post-link`: Also read the bytes written into the build info's post-link
  region after linking, as hex (`post_link_data` in JSON)

Against a 3.0 server the build is shown as not reported; a binary without
build info shows it as not embedded.

#### `instance ping`
Test connection to an instance.

```bash
aimdb [--connect <ENDPOINT>] instance ping
```

### Record Commands

#### `record list`
List all registered records in an AimDB instance.

```bash
aimdb record list [OPTIONS]
```

Options:
- `-f, --format <FORMAT>`: Output format (table, json, json-compact, yaml)
- `-w, --writable`: Show only writable records

#### `record get`
Get the current value of a specific record.

```bash
aimdb record get <RECORD> [OPTIONS]
```

Arguments:
- `<RECORD>`: Record name (e.g., `server::Temperature`)

Options:
- `-f, --format <FORMAT>`: Output format (default: json)

#### `record set`
Set the value of a writable record.

```bash
aimdb record set <NAME> <VALUE> [OPTIONS]
```

Arguments:
- `<NAME>`: Record name
- `<VALUE>`: JSON value to set

Options:
- `--dry-run`: Validate but don't actually set

**Note**: Only records without producers can be set remotely.

### Watch Command

#### `watch`
Watch a record for live updates, displaying updates as they arrive.

```bash
aimdb watch <RECORD> [OPTIONS]
```

Arguments:
- `<RECORD>`: Record name to watch

Options:
- `-q, --queue-size <SIZE>`: Subscription queue size (default: 100)
- `-c, --count <N>`: Maximum number of events to receive (0 = unlimited)
- `-f, --full`: Show full pretty-printed JSON for each event
- `--format <table|json>`: `json` writes one NDJSON object per update on
  stdout and every status line on stderr
- `--new`: Count only updates written after the watch is armed. Without it, a
  `SingleLatest` record's current value can arrive first and pass as a new one.
- `--timeout <SECS>`: Give up after SECS. Exits 4 if `--count` updates have not
  arrived by then; with `--count 0` it is a normal end.

Press Ctrl+C to stop watching and unsubscribe cleanly.

The "Watching record" status line means **armed**: subscribed and, with
`--new`, the record's current write stamp taken as the baseline. An update
written after that line is caught. Armed does not guarantee delivery: a
failure the server reports later, such as a buffer out of subscriber slots,
still ends the watch with exit 1.

With `--format json`, the watch ends with one outcome line on stderr, so a
caller can read the result from a file without the exit code:

```json
{"outcome":"timeout","received":0,"skipped":0,"wanted":1}
```

| `outcome` | Exit | Meaning |
|---|---|---|
| `ok` | 0 | `--count` updates arrived (or, with `--count 0`, the stream or timeout ended) |
| `timeout` | 4 | `--timeout` passed first |
| `disconnected` | 3 | Connection lost before `--count` updates, or unreachable at start. Stamps are per instance lifetime, so the watch does not resume. |
| `refused` | 1 | Subscription refused or record not found; the line carries `error` |
| `stopped` | 0 | Ctrl+C |

#### Waiting for an event (agents, scripts)

To confirm that something happens ("press B1, then check `button.b1`
fired"), arm the watch first, then prompt, then read the result. Each step
works as a separate shell command, so it fits an agent whose tool calls each
run in a new shell:

```bash
# 1. Arm in the background and wait for the "armed" line.
aimdb watch button.b1 --new --format json --count 1 --timeout 60 >out.ndjson 2>status.log &
until grep -q 'Watching record' status.log; do sleep 0.1; done

# 2. Prompt: "Please press B1."

# 3. Later: wait for the outcome, then read the updates.
until grep -q '"outcome"' status.log; do sleep 0.5; done
grep '"outcome"' status.log
cat out.ndjson
```

A record that updates on its own (a periodic sensor) shows up as lines in
`out.ndjson` before the prompt. Check for those before prompting: if there
are any, the check cannot tell the person's action from the record's own
updates.

**Serial links** carry a single AimX connection, and a new `hello` resets any
other session on the line. Do not run `aimdb watch` over `serial://` while
`aimdb-mcp` (or another CLI command) holds the same link.

### Graph Commands

Commands for exploring the dependency graph of records in an AimDB instance.

#### `graph nodes`
List all nodes in the dependency graph, showing record origins, schemas, buffer types, tap counts, and the address of each outbound link.

```bash
aimdb graph nodes [OPTIONS]
```

Options:
- `-f, --format <FORMAT>`: Output format (table, json, json-compact, yaml)

Example output:
```
┌───────────────────┬────────────────┬────────────────┬───────────────┬──────────┬──────┬──────────────────────────┐
│ Key               │ Origin         │ Schema         │ Buffer Type   │ Capacity │ Taps │ Outbound                 │
├───────────────────┼────────────────┼────────────────┼───────────────┼──────────┼──────┼──────────────────────────┤
│ temp.vienna       │ link           │ temperature v2 │ single_latest │ -        │ 1    │ -                        │
│ temp.berlin       │ link           │ temperature v2 │ single_latest │ -        │ 1    │ -                        │
│ weather.aggregate │ transform_join │ -              │ single_latest │ -        │ 0    │ mqtt://weather/aggregate │
└───────────────────┴────────────────┴────────────────┴───────────────┴──────────┴──────┴──────────────────────────┘
```

#### `graph edges`
List all edges in the dependency graph, showing how data flows between records.

```bash
aimdb graph edges [OPTIONS]
```

Options:
- `-f, --format <FORMAT>`: Output format

Example output:
```
┌────────────────┬───────────────────┬─────────────────┐
│ From           │ To                │ Edge Type       │
├────────────────┼───────────────────┼─────────────────┤
│ temp.vienna    │ weather.aggregate │ transform_input │
│ temp.berlin    │ weather.aggregate │ transform_input │
└────────────────┴───────────────────┴─────────────────┘
```

#### `graph order`
Show the topological ordering of records (spawn/initialization order).

```bash
aimdb graph order [OPTIONS]
```

Options:
- `-f, --format <FORMAT>`: Output format

Example output:
```
┌───┬───────────────────┐
│ # │ Record Key        │
├───┼───────────────────┤
│ 1 │ temp.vienna       │
│ 2 │ temp.berlin       │
│ 3 │ weather.aggregate │
└───┴───────────────────┘
```

#### `graph dot`
Export the dependency graph in DOT format for visualization with Graphviz.

```bash
aimdb graph dot [OPTIONS]
```

Options:
- `-n, --name <NAME>`: Graph name (default: "aimdb")

Example:
```bash
# Generate DOT file and render as PNG
aimdb graph dot > graph.dot
dot -Tpng graph.dot -o graph.png

# Or pipe directly to dot
aimdb graph dot | dot -Tsvg > graph.svg
```

## Output Formats

The CLI supports multiple output formats:

- **table** (default for lists): Human-readable formatted tables
- **json**: Pretty-printed JSON with indentation
- **json-compact**: Single-line JSON for scripting
- **yaml**: YAML format (requires `yaml` feature)

## Connecting to an Instance

Pick the target instance with the global `--connect <ENDPOINT>` flag. The endpoint
is a `scheme://` URL — the scheme selects the transport at runtime, the same way
records pick one for links:

| Endpoint | Transport |
|---|---|
| `unix:///tmp/aimdb.sock` / `uds:///tmp/aimdb.sock` | Unix domain socket |
| `/tmp/aimdb.sock` (bare path) | Unix domain socket (the `unix://` shorthand) |
| `serial:///dev/ttyACM0?baud=115200` | Serial/UART (requires the `transport-serial` build feature) |
| `tcp://127.0.0.1:7001` | TCP (requires the `transport-tcp` build feature) |

```bash
# Explicit endpoint
aimdb --connect unix:///tmp/aimdb.sock record list

# Bare path shorthand
aimdb --connect /tmp/aimdb.sock record list

# Serial board (CLI built with --features transport-serial)
aimdb --connect serial:///dev/ttyACM0?baud=115200 record list

# TCP endpoint (CLI built with --features transport-tcp)
aimdb --connect tcp://127.0.0.1:7001 record list
```

An unknown scheme — or one whose transport isn't compiled into this binary — is
rejected with a clear error listing the built-in schemes.

### Resolution order

When `--connect` is omitted, the endpoint is resolved in this order:

1. `--connect <ENDPOINT>`
2. `AIMDB_CONNECT` environment variable
3. UDS auto-discovery — scans `/tmp` and `/var/run/aimdb` for `.sock` files and
   uses the first running instance.

```bash
export AIMDB_CONNECT=unix:///tmp/aimdb.sock
aimdb record list   # uses $AIMDB_CONNECT
```

`instance list` is always discovery-only and ignores `--connect`.

### Transport features

The Unix-socket transport is built in by default. The serial transport is
**off by default** (it pulls `tokio-serial` → libudev on Linux); enable it when
building:

```bash
cargo build --release -p aimdb-cli --features transport-serial
cargo build --release -p aimdb-cli --features transport-tcp
```

## Error Handling

The CLI provides clear, actionable error messages:

```
Error: Connection failed: /tmp/aimdb.sock
  Reason: connection timeout
  Hint: Check if AimDB instance is running
```

```
Error: Permission denied: record.set
  Record 'server::Temperature' is not writable
  Hint: Check 'writable' column in 'aimdb record list'
```

### Exit codes

Every command shares one exit-code taxonomy, so a script can retry an
unreachable instance without retrying a rejected request:

| Code | Class | Examples |
|---|---|---|
| 0 | Success | |
| 1 | Rejected: the instance or a remote service refused the request | record not found, record not writable, server error |
| 2 | Usage: bad arguments or malformed input | unknown subcommand or flag, unsupported `--connect` endpoint, invalid JSON for `record set` |
| 3 | Environment: worth retrying | connection failed, no instance found, protocol mismatch, I/O error, a reply that does not parse |
| 4 | Timeout | `watch --timeout` passed before `--count` updates arrived |

```bash
aimdb instance ping
case $? in
    0) echo "up" ;;
    3) echo "unreachable, retry later" ;;
    *) echo "failed" ;;
esac
```

## Examples

### Health Check Workflow

```bash
# Discover instances
aimdb instance list

# Check connectivity
aimdb instance ping

# List all records
aimdb record list

# Get specific values
aimdb record get server::Temperature
aimdb record get server::SystemStatus
```

### Debugging Workflow

```bash
# Find writable records
aimdb record list --writable

# Watch live updates
aimdb watch server::Temperature --count 10

# Update configuration
aimdb record set server::Config '{"log_level":"debug"}'

# Verify change
aimdb record get server::Config
```

### Monitoring Integration

```bash
# Export metrics as JSON
aimdb record list --format json > records.json

# Check specific value in script
TEMP=$(aimdb record get server::Temperature | jq '.celsius')
if (( $(echo "$TEMP > 80" | bc -l) )); then
    echo "Warning: High temperature!"
fi

# Continuous monitoring
watch -n 1 'aimdb record get server::Temperature | jq ".celsius"'
```

### Graph Analysis Workflow

```bash
# View all nodes and their origins
aimdb graph nodes

# See data flow edges
aimdb graph edges

# Check spawn order
aimdb graph order

# Generate visual graph
aimdb graph dot > aimdb-graph.dot
dot -Tpng aimdb-graph.dot -o aimdb-graph.png
open aimdb-graph.png

# Export as JSON for further analysis
aimdb graph nodes --format json | jq '.[] | select(.origin | objects | has("transform"))'
```

## Protocol

The CLI uses the AimX remote access protocol with NDJSON message format, over
whichever transport the `--connect` endpoint selects (Unix domain socket by
default; serial/UART with the `transport-serial` feature).

See `docs/design/008-M3-remote-access.md` for the full protocol specification.

## Development

Run tests:
```bash
cargo test -p aimdb-cli
```

Run with logging:
```bash
RUST_LOG=debug cargo run -p aimdb-cli -- record list
```

## License

See [LICENSE](../../LICENSE) file.
