# aimdb-mcp

Model Context Protocol (MCP) server for AimDB - enables LLM-powered introspection and debugging.

## Overview

`aimdb-mcp` provides an MCP server implementation that enables Large Language Models (like Claude, GPT-4, etc.) to interact with running AimDB instances for introspection, debugging, and monitoring.

**Key Features:**
- **LLM-Powered**: Natural language queries to AimDB instances
- **Auto-Discovery**: Automatically finds running AimDB servers
- **Schema Inference**: Infers JSON schemas from record values
- **Architecture Agent**: Propose, validate, and apply schema/topology changes from natural language
- **Rich Toolset**: 30 tools covering introspection, record ops, the dependency graph, metrics/profiling, and architecture editing
- **VS Code Integration**: Works seamlessly with GitHub Copilot

## Architecture

```
┌──────────────────────────────┐
│  LLM Host (VS Code/Claude)   │
│  - Natural language queries  │
│  - Tool invocations          │
└──────────────┬───────────────┘
               │ stdio (JSON-RPC 2.0)
               ▼
┌──────────────────────────────┐
│     aimdb-mcp Server         │
│  - Protocol translation      │
│  - Tool implementations      │
│  - Schema inference          │
└──────────────┬───────────────┘
               │ aimdb-client
               ▼
┌──────────────────────────────┐
│  AimDB Instances             │
│  (Unix domain sockets)       │
└──────────────────────────────┘
```

## Quick Start

### Installation

Build from source:

```bash
cd /aimdb
cargo build --release -p aimdb-mcp
```

Binary will be at `target/release/aimdb-mcp`.

### VS Code Configuration

Add `.vscode/mcp.json` to your workspace:

```json
{
  "servers": {
    "aimdb": {
      "type": "stdio",
      "command": "/path/to/aimdb-mcp",
      "args": [],
      "env": {
        "RUST_LOG": "info"
      }
    }
  }
}
```

**Note:** VS Code will automatically detect and load MCP servers from `.vscode/mcp.json`.

### Claude Desktop Configuration

Add to `claude_desktop_config.json`:

```json
{
  "mcpServers": {
    "aimdb": {
      "command": "/path/to/aimdb-mcp",
      "args": []
    }
  }
}
```

### Choosing the Target Instance

Every tool that talks to an instance takes an optional `endpoint` argument — a
`scheme://` URL that selects the transport at runtime:

| Endpoint | Transport |
|---|---|
| `unix:///tmp/aimdb.sock` / `uds:///tmp/aimdb.sock` | Unix domain socket |
| `/tmp/aimdb.sock` (bare path) | Unix domain socket (the `unix://` shorthand) |
| `serial:///dev/ttyACM0?baud=115200` | Serial/UART (requires the `transport-serial` build feature) |

When a tool's `endpoint` is omitted, the server resolves it in this order:

1. The tool's explicit `endpoint` argument
2. The `--connect <ENDPOINT>` startup flag
3. The `AIMDB_CONNECT` environment variable

This lets you pin a single instance once at startup so the LLM never has to pass a
path. Pass it as a flag:

```json
{
  "mcpServers": {
    "aimdb": {
      "command": "/path/to/aimdb-mcp",
      "args": ["--connect", "unix:///tmp/aimdb-demo.sock"]
    }
  }
}
```

…or via the environment:

```json
{
  "mcpServers": {
    "aimdb": {
      "command": "/path/to/aimdb-mcp",
      "args": [],
      "env": { "AIMDB_CONNECT": "unix:///tmp/aimdb-demo.sock" }
    }
  }
}
```

In `--public` mode any client-supplied `endpoint` is stripped, so tools fall back
to the server-pinned `--connect` / `AIMDB_CONNECT` and clients cannot probe
arbitrary paths on the host.

The serial transport is **off by default** (it pulls `tokio-serial` → libudev on
Linux); build with `cargo build -p aimdb-mcp --features transport-serial` to dial
`serial://` endpoints.

### Test Server

Start the example server:

```bash
cd /aimdb/examples/remote-access-demo
cargo run
```

This creates an instance at `/tmp/aimdb-demo.sock` with sample records.

## Demo

> 🎬 **Demo video coming soon** - showing natural language queries via GitHub Copilot

<!-- TODO: Add GIF/video showing:
     1. Starting demo server
     2. Asking Copilot "What AimDB instances are running?"
     3. Asking "What's the current temperature?"
     4. Draining a record for trend analysis
     
     Suggested location: assets/aimdb-mcp-demo.gif
     Usage: ![AimDB MCP in Action](../../assets/aimdb-mcp-demo.gif)
-->

## Available Tools

30 tools in total: 14 introspection & operations tools (below) and 16
architecture-agent tools (see [Architecture Agent Tools](#architecture-agent-tools)).
In `--public` mode only `discover_instances`, `list_records`, and `get_record` are
advertised.

### 1. discover_instances

Find all running AimDB instances:

```
Query: "What AimDB instances are running?"

Result: Lists socket paths, versions, and record counts
```

### 2. get_instance_info

Get detailed information about a specific instance:

```
Query: "Show me details about /tmp/aimdb-demo.sock"

Result: Server version, protocol, permissions, capabilities
```

### 3. list_records

List all records in an instance:

```
Query: "What records are in the demo instance?"

Result: Record names, types, buffer configs, producer/consumer counts
```

### 4. get_record

Get current value of a record:

```
Query: "What's the current temperature?"

Result: JSON value of server::Temperature record
```

### 5. set_record

Set value of a writable record:

```
Query: "Set the config log_level to debug"

Action: Updates server::Config record
```

### 6. query_schema

Infer JSON schema from record values:

```
Query: "What's the schema of the Temperature record?"

Result: JSON Schema with types, required fields, and example
```

### 7. drain_record

Drain all values accumulated since the last drain (a destructive batch read):

```
Query: "Drain the temperature buffer and analyze the trend"

Result: Values in chronological order; the first call is a cold start (empty)
```

### 8. graph_nodes

List all nodes in the dependency graph:

```
Query: "Show me the record graph nodes"

Result: Per-record origin (source/link/transform/passive), buffer config, and edge counts
```

### 9. graph_edges

List the directed edges (data flow) between records:

```
Query: "How does data flow between records?"

Result: Directed edges from sources through transforms to consumers
```

### 10. graph_topo_order

Show the topological (spawn/initialization) order of records:

```
Query: "What order are records initialized in?"

Result: Record keys ordered so dependencies precede their dependents
```

### 11. get_stage_profiling

Show automatic per-stage timing — how long each `.source()` / `.tap()` / `.link()`
callback takes (wall-clock, including `.await` / I/O / sleeps) — for records
matching a key, and flag the slowest stage. Requires the target instance to be
built with the `profiling` feature; otherwise records carry no profiling data.

```
Query: "Which stage of my Temperature pipeline is the bottleneck?"

Result: Per-stage call_count / avg / min / max (ns) plus a "bottleneck" pointing
        at the stage with the highest average time, with a recommendation string.
```

Stage names come from `.with_name("...")` on the registrar; unnamed stages show as
`source[0]`, `tap[0]`, etc.

### 12. reset_stage_profiling

Reset stage profiling counters for every record on the target instance (requires
write permission and the `profiling` feature). Useful for windowed measurements.

```
Query: "Reset the stage profiling counters."

Result: { "reset": true }
```

### 13. get_buffer_metrics

Return live buffer introspection counters
(`produced_count` / `consumed_count` / `dropped_count` / `occupancy`) for records
matching a key. Requires the target instance to be built with the `metrics`
feature; otherwise records carry no buffer metrics.

```
Query: "What's the producer/consumer lag on my Temperature buffer?"

Result: Per-record produced/consumed/dropped counts and current (used, capacity).
```

### 14. reset_buffer_metrics

Reset buffer introspection counters for every record on the target instance
(requires write permission and the `metrics` feature). Useful for windowed
measurements.

```
Query: "Reset the buffer metrics counters."

Result: { "reset": true }
```

## Architecture Agent Tools

Beyond live introspection, the server exposes an **architecture agent** for
designing and editing an AimDB topology from natural language. It reads/writes
`.aimdb/state.toml` and, on confirmation, generates Mermaid and Rust artefacts.
These tools are not available in `--public` mode.

The editing tools follow a **propose → resolve** flow: a `propose_*` /
`remove_*` / `rename_record` call creates a *pending proposal* (shown to the user),
which `resolve_proposal` then confirms, rejects, or revises.

| Tool | Purpose |
|---|---|
| `get_architecture` | Return current state from `.aimdb/state.toml` (record count, validation summary, decision-log length). Run first when starting a session. |
| `validate_against_instance` | Compare `state.toml` to a live instance; report missing records, buffer/capacity mismatches, and connector diffs. |
| `propose_add_record` | Propose a new record (explicit, typed fields). |
| `propose_modify_buffer` | Propose changing a record's buffer type / capacity. |
| `propose_add_connector` | Propose adding a connector (MQTT, KNX, …) to a record. |
| `propose_modify_fields` | Propose replacing a record's value-struct fields (all fields). |
| `propose_modify_key_variants` | Propose updating a record's key variants. |
| `propose_add_task` | Propose a new task definition. |
| `propose_add_binary` | Propose a new binary definition. |
| `remove_task` | Propose removing a task. |
| `remove_binary` | Propose removing a binary (task definitions are preserved). |
| `remove_record` | Propose removing a record. |
| `rename_record` | Propose renaming a record (renames the generated key enum + value struct). |
| `resolve_proposal` | Confirm / reject / revise a pending proposal; on confirm writes `state.toml` and regenerates artefacts. |
| `save_memory` | Persist ideation context and rationale to `.aimdb/memory.md`. |
| `reset_session` | Discard pending proposals and start over. |

## Schema Inference

The MCP server can infer JSON schemas from record values:

```json
// Record value
{
  "celsius": 23.5,
  "sensor_id": "sensor-001",
  "timestamp": 1730379296
}

// Inferred schema
{
  "type": "object",
  "properties": {
    "celsius": { "type": "number" },
    "sensor_id": { "type": "string" },
    "timestamp": { "type": "integer" }
  },
  "required": ["celsius", "sensor_id", "timestamp"]
}
```

**Limitations:**
- Best-effort inference from current value
- May not capture full type constraints
- Nullable fields require multiple samples
- Ask user for clarification on ambiguous cases

## Resources

The server exposes two families of resources. Instance resources are discovered
by scanning for Unix sockets, so their URIs are keyed by socket path:

- `aimdb://instances` — list of all discovered instances
- `aimdb://instance/{socket_path}` — details about a specific instance
- `aimdb://{socket_path}/records` — all records in an instance

Architecture resources expose the `.aimdb/` design state used by the architecture
agent:

- `aimdb://architecture` — architecture overview
- `aimdb://architecture/state` — full `state.toml` as JSON
- `aimdb://architecture/conflicts` — conflicts vs. a live instance
- `aimdb://architecture/conventions` — naming/design conventions
- `aimdb://architecture/memory` — persisted ideation context (`memory.md`)

## Prompts

The server provides 4 helper prompts (not available in `--public` mode):

- `architecture_agent` — drive the propose → resolve design workflow
- `onboarding` — introduction and common usage patterns
- `breaking_change_review` — review the impact of a proposed schema change
- `troubleshooting` — guided diagnostics for connection/record issues

## Protocol Details

### Transport
- **stdio**: JSON-RPC 2.0 over standard input/output
- **Format**: NDJSON (newline-delimited JSON)

### Capabilities
- **Tools**: ✓ (30 tools; only 3 advertised in `--public` mode)
- **Resources**: ✓ (instances + architecture families)
- **Prompts**: ✓ (4 prompts)
- **Sampling**: ✗ (not supported)
- **Logging**: ✓ (stderr)

### Message Flow

```
Client → Server: initialize request
Client ← Server: initialize response

Client → Server: tools/list request
Client ← Server: tool definitions

Client → Server: tools/call (discover_instances)
Client ← Server: tool result

Client → Server: resources/read (aimdb://instances)
Client ← Server: resource content
```

## Usage Examples

### Health Check

```
User: "Check the health of all AimDB instances"

LLM:
1. discover_instances() → finds instances
2. For each: get_instance_info() → checks status
3. Reports: healthy/unhealthy with details
```

### Record Exploration

```
User: "What data is available in the demo instance?"

LLM:
1. list_records(/tmp/aimdb-demo.sock) → gets record list
2. For interesting records: get_record() → shows values
3. query_schema() → explains structure
4. Summarizes available data types
```

### Data Monitoring

```
User: "Monitor temperature and analyze the trend"

LLM:
1. drain_record(server::Temperature)   # cold start — creates the reader, returns empty
2. drain_record(server::Temperature)   # later: returns values accumulated since last drain
3. Analyzes: min, max, avg, trends, anomalies
4. Generates report
```

### Configuration Update

```
User: "Set the log level to debug"

LLM:
1. list_records() → finds writable config record
2. get_record(server::Config) → sees current value
3. set_record(server::Config, {"log_level": "debug", ...})
4. Confirms change
```

## Error Handling

The MCP server provides clear error messages:

```
Error: Connection failed: /tmp/aimdb.sock
  Reason: No such file or directory
  Hint: Check if AimDB instance is running
```

```
Error: Permission denied
  Record 'server::Temperature' is not writable
  Hint: Only records without producers can be set
```

## Development

### Building

```bash
cargo build -p aimdb-mcp
```

### Testing

```bash
# Unit tests
cargo test -p aimdb-mcp

# Integration test (requires running instance)
cargo run --example remote-access-demo  # Terminal 1
cargo test -p aimdb-mcp --test integration  # Terminal 2
```

### Debugging

Enable debug logging:

```bash
RUST_LOG=debug aimdb-mcp
```

Logs go to stderr, keeping stdio clean for MCP protocol.

### Adding Tools

1. Define tool in `tools.rs`:
```rust
pub fn my_new_tool() -> Tool {
    Tool {
        name: "my_tool".to_string(),
        description: "Does something useful".to_string(),
        input_schema: json!({ /* ... */ }),
    }
}
```

2. Implement handler in `server.rs`:
```rust
"my_tool" => {
    let result = handle_my_tool(params).await?;
    // Return result
}
```

3. Add to tool list in `list_tools()`.

## Security Considerations

### Authentication
- Currently no authentication required
- Unix socket permissions control access
- Future: Token-based auth planned

### Permissions
- Read-only by default (list, get, drain, graph)
- Write operations (set) require writable records
- Architecture-editing tools are disabled in `--public` mode
- No shell access or arbitrary code execution

### Data Privacy
- Architecture state (`.aimdb/`) is stored locally
- No network communication
- All data stays on local machine

## Performance

- **Tool latency**: < 10ms for local operations
- **Memory usage**: ~5MB base
- **Concurrent connections**: Single-threaded stdio

## Troubleshooting

### Server not responding

1. Check if MCP server is running:
```bash
ps aux | grep aimdb-mcp
```

2. Test stdio manually:
```bash
echo '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}' | aimdb-mcp
```

### Can't find instances

1. Check socket paths:
```bash
ls /tmp/*.sock /var/run/aimdb/*.sock
```

2. Verify permissions:
```bash
stat /tmp/aimdb-demo.sock
```

## References

- **MCP Specification**: https://spec.modelcontextprotocol.io/
- **AimX Protocol**: `docs/design/008-M3-remote-access.md`
- **Schema Design**: `docs/design/011-M4-schema-query.md`

## License

See [LICENSE](../../LICENSE) file.
