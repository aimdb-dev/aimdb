# AimDB CLI

Command-line interface for introspecting and managing running AimDB instances.

## Overview

The AimDB CLI is a thin client over the AimX v1 remote access protocol, providing intuitive commands for:
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
┌──────────────────────┬────────────────────────────────────────────┬───────────────┬───────────┬───────────┬──────────┐
│ Name                 │ Type ID                                    │ Buffer Type   │ Producers │ Consumers │ Writable │
├──────────────────────┼────────────────────────────────────────────┼───────────────┼───────────┼───────────┼──────────┤
│ server::Temperature  │ TypeId(0xaee15e261d918c67cee5a96c2f604ce0) │ single_latest │ 1         │ 2         │ no       │
│ server::Config       │ TypeId(0xc2af5c8376864a24e916c87f88505fac) │ mailbox       │ 0         │ 3         │ yes      │
└──────────────────────┴────────────────────────────────────────────┴───────────────┴───────────┴───────────┴──────────┘
```

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

### Instance Commands

#### `instance list`
List all running AimDB instances by scanning for Unix domain socket files.

```bash
aimdb instance list [--format <FORMAT>]
```

Options:
- `-f, --format <FORMAT>`: Output format (table, json, json-compact, yaml)

#### `instance info`
Show detailed information about a specific instance.

```bash
aimdb instance info [--socket <PATH>]
```

Options:
- `-s, --socket <PATH>`: Socket path (uses auto-discovery if not specified)

#### `instance ping`
Test connection to an instance.

```bash
aimdb instance ping [--socket <PATH>]
```

### Record Commands

#### `record list`
List all registered records in an AimDB instance.

```bash
aimdb record list [OPTIONS]
```

Options:
- `-s, --socket <PATH>`: Socket path (uses auto-discovery if not specified)
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
- `-s, --socket <PATH>`: Socket path
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
- `-s, --socket <PATH>`: Socket path
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
- `-s, --socket <PATH>`: Socket path
- `-q, --queue-size <SIZE>`: Subscription queue size (default: 100)
- `-c, --count <N>`: Maximum number of events to receive (0 = unlimited)
- `-f, --full`: Show full pretty-printed JSON for each event

Press Ctrl+C to stop watching and unsubscribe cleanly.

## Output Formats

The CLI supports multiple output formats:

- **table** (default for lists): Human-readable formatted tables
- **json**: Pretty-printed JSON with indentation
- **json-compact**: Single-line JSON for scripting
- **yaml**: YAML format (requires `yaml` feature)

## Socket Discovery

The CLI automatically discovers running AimDB instances by scanning:
- `/tmp` directory
- `/var/run/aimdb` directory

Socket files must have a `.sock` extension.

You can override auto-discovery by specifying `--socket <PATH>` for any command.

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

## Protocol

The CLI uses the AimX v1 remote access protocol over Unix domain sockets with NDJSON message format.

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
