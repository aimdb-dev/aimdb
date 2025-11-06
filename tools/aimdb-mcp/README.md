# aimdb-mcp

Model Context Protocol (MCP) server for AimDB - enables LLM-powered introspection and debugging.

## Overview

`aimdb-mcp` provides an MCP server implementation that enables Large Language Models (like Claude, GPT-4, etc.) to interact with running AimDB instances for introspection, debugging, and monitoring.

**Key Features:**
- **LLM-Powered**: Natural language queries to AimDB instances
- **Auto-Discovery**: Automatically finds running AimDB servers
- **Schema Inference**: Infers JSON schemas from record values
- **Real-time Subscriptions**: Subscribe to record updates with automatic data capture
- **Rich Toolset**: 11 tools covering all AimDB operations
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
     4. Subscribing to updates
     
     Suggested location: assets/aimdb-mcp-demo.gif
     Usage: ![AimDB MCP in Action](../../assets/aimdb-mcp-demo.gif)
-->

## Available Tools

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

### 7. subscribe_record

Subscribe to real-time record updates:

```
Query: "Subscribe to temperature for 50 samples"

Action: Creates subscription, auto-saves updates to JSONL file
```

### 8. unsubscribe_record

Stop an active subscription:

```
Query: "Stop subscription sub-abc123"

Action: Unsubscribes and stops data collection
```

### 9. list_subscriptions

Show active subscriptions:

```
Query: "What subscriptions are active?"

Result: Subscription IDs, records, sample counts, file paths
```

### 10. get_notification_directory

Get directory where subscription data is saved:

```
Query: "Where is subscription data saved?"

Result: Path to notification directory
```

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

## Subscriptions

### How It Works

1. LLM requests subscription with sample limit
2. Server creates subscription and JSONL file
3. Updates automatically saved as they arrive
4. Auto-unsubscribes when limit reached
5. File path returned for analysis

### File Format

Subscription data saved as JSONL (one JSON object per line):

```jsonl
{"timestamp":"2025-11-06T10:30:45.123Z","sequence_number":1,"value":{"celsius":23.5,"sensor_id":"sensor-001"}}
{"timestamp":"2025-11-06T10:30:47.456Z","sequence_number":2,"value":{"celsius":23.6,"sensor_id":"sensor-001"}}
{"timestamp":"2025-11-06T10:30:49.789Z","sequence_number":3,"value":{"celsius":23.7,"sensor_id":"sensor-001"}}
```

### Sample Limits

**IMPORTANT:** Always ask user for sample limit before subscribing.

Suggested limits:
- **10-30 samples**: Quick check (~20-60 seconds)
- **50-100 samples**: Short monitoring (~2-3 minutes)
- **200-500 samples**: Extended analysis (~7-17 minutes)
- **null**: Unlimited (requires explicit user confirmation)

### File Location

Default: `~/.local/share/aimdb-mcp/notifications/`

Files named: `{subscription_id}.jsonl`

## Resources

MCP server provides 5 resources:

### 1. `aimdb://instances`
List of all discovered instances

### 2. `aimdb://instance/{socket_path}`
Details about specific instance

### 3. `aimdb://records/{socket_path}`
All records in an instance

### 4. `aimdb://record/{socket_path}/{record_name}`
Specific record value

### 5. `aimdb://schema/{socket_path}/{record_name}`
Inferred schema for record

## Prompts

MCP server provides 3 helper prompts:

### 1. `aimdb-quickstart`
Introduction and common usage patterns

### 2. `notification-directory`
Information about subscription data storage

### 3. `subscription-help`
Guide to subscriptions and data analysis

## Protocol Details

### Transport
- **stdio**: JSON-RPC 2.0 over standard input/output
- **Format**: NDJSON (newline-delimited JSON)

### Capabilities
- **Tools**: ✓ (11 tools)
- **Resources**: ✓ (5 resources)  
- **Prompts**: ✓ (3 prompts)
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
User: "Monitor temperature for 100 samples and analyze"

LLM:
1. subscribe_record(server::Temperature, 100)
2. Waits for completion
3. Reads JSONL file
4. Analyzes: min, max, avg, trends, anomalies
5. Generates report
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
- Read-only by default (list, get, subscribe)
- Write operations (set) require writable records
- No shell access or arbitrary code execution

### Data Privacy
- Subscription data stored locally
- No network communication
- All data stays on local machine

## Performance

- **Tool latency**: < 10ms for local operations
- **Subscription overhead**: Minimal (async streaming)
- **Memory usage**: ~5MB base + subscription buffers
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

### Subscription not working

1. Check notification directory:
```bash
ls -la ~/.local/share/aimdb-mcp/notifications/
```

2. Verify disk space:
```bash
df -h ~/.local/share/
```

## References

- **MCP Specification**: https://spec.modelcontextprotocol.io/
- **AimX Protocol**: `docs/design/008-M3-remote-access.md`
- **Schema Design**: `docs/design/011-M4-schema-query.md`

## License

See [LICENSE](../../LICENSE) file.
