# AimDB MCP Integration Strategy

**Version:** 1.0  
**Status:** Design Document  
**Last Updated:** November 2, 2025  
**Milestone:** M4 - LLM Integration

---

## Table of Contents

- [Executive Summary](#executive-summary)
- [Background & Context](#background--context)
- [Architecture Overview](#architecture-overview)
- [MCP Server Design](#mcp-server-design)
- [AimX Integration](#aimx-integration)
- [MCP Tools & Resources](#mcp-tools--resources)
- [Implementation Plan](#implementation-plan)
- [Security Considerations](#security-considerations)
- [Performance & Scalability](#performance--scalability)
- [Future Extensions](#future-extensions)
- [Appendix](#appendix)

---

## Executive Summary

This document outlines the strategy for integrating Large Language Models (LLMs) with AimDB through the **Model Context Protocol (MCP)**. The proposed `aimdb-mcp` crate will implement an MCP server that communicates with running AimDB instances via the **AimX v1 protocol**, enabling LLM-powered introspection, monitoring, debugging, and automation.

### Key Design Decisions

1. **Protocol Layer Separation**: MCP server ↔ AimX client ↔ AimDB instance
2. **Transport**: MCP uses stdio transport; AimX uses Unix domain sockets
3. **Security**: Inherits AimX security model (UDS permissions + optional tokens)
4. **Scope**: Read-heavy operations initially, selective write capabilities later
5. **Discovery**: Automatic AimDB instance discovery via socket scanning

### Value Proposition

- **For Developers**: Natural language interface for debugging and introspection
- **For Operations**: LLM-assisted monitoring and anomaly detection
- **For Data Scientists**: Easy data exploration without custom tooling
- **For Systems**: Foundation for autonomous monitoring and remediation

---

## Background & Context

### What We Learned from aimx and aimdb-cli

From implementing **AimX v1** protocol and the **aimdb-cli** tool, we gained critical insights:

#### ✅ **What Works Well**

1. **NDJSON Protocol**: Simple, debuggable, easily extensible
2. **Unix Domain Sockets**: Low overhead, OS-level security, no network exposure
3. **Discovery Pattern**: Scanning `/tmp` and `/var/run/aimdb` for `.sock` files
4. **Request/Response Model**: Clean separation of concerns, easy to reason about
5. **Subscription Pattern**: Efficient for real-time monitoring (event funnel architecture)
6. **Security Model**: Read-only default with explicit write permissions per record
7. **Connection Handling**: Tokio-based async I/O, bounded queues prevent backpressure

#### 📝 **Lessons Learned**

1. **Discovery ergonomics matter**: Auto-discovery makes the CLI feel magical
2. **Error messages are critical**: Clear, actionable errors improve developer experience
3. **Output formatting flexibility**: Multiple formats (table, JSON, YAML) serve different use cases
4. **Subscription state management**: Event funnel pattern cleanly handles concurrency
5. **Handshake is essential**: Version negotiation prevents compatibility issues

#### 🎯 **Opportunities for MCP**

1. **Natural language queries**: LLMs can translate questions to AimX commands
2. **Contextual awareness**: MCP allows LLMs to maintain session state
3. **Multi-step workflows**: LLMs can chain operations (list → filter → get → analyze)
4. **Anomaly detection**: Pattern recognition over subscription streams
5. **Documentation generation**: Auto-document record schemas and system behavior

### Model Context Protocol (MCP) Overview

**MCP** is a standardized protocol for connecting LLMs to external data sources and tools. Created by Anthropic, it provides:

- **Bidirectional communication**: LLM ↔ MCP Server ↔ Data Source
- **Tool invocation**: LLMs can call functions with typed arguments
- **Resource exposure**: Serve data (records, schemas, logs) to LLM context
- **Prompt templates**: Pre-defined interaction patterns
- **Sampling**: LLM can request completions from other models

**Key MCP Concepts:**

```
┌─────────────┐         ┌─────────────┐         ┌─────────────┐
│   LLM Host  │◄───────►│ MCP Server  │◄───────►│   AimDB     │
│  (Claude)   │  stdio  │ (aimdb-mcp) │  UDS    │  Instance   │
└─────────────┘         └─────────────┘         └─────────────┘
```

---

## Architecture Overview

### System Architecture

```
┌──────────────────────────────────────────────────────────────────┐
│                        LLM Host / IDE                             │
│  (Claude Desktop, VS Code Copilot, Zed, etc.)                    │
└────────────────────────┬─────────────────────────────────────────┘
                         │ stdio (JSON-RPC 2.0)
                         │
┌────────────────────────▼─────────────────────────────────────────┐
│                      aimdb-mcp Server                             │
│  ┌──────────────────────────────────────────────────────────┐   │
│  │  MCP Protocol Handler                                     │   │
│  │  - Tool invocations (list, get, subscribe, set)          │   │
│  │  - Resource providers (schemas, metrics, logs)           │   │
│  │  - Prompt templates (debugging, monitoring)              │   │
│  └──────────────────────┬───────────────────────────────────┘   │
│                         │                                         │
│  ┌──────────────────────▼───────────────────────────────────┐   │
│  │  AimX Client (reuse aimdb-cli client module)             │   │
│  │  - Connection management                                  │   │
│  │  - Protocol handshake                                     │   │
│  │  - Request/response handling                              │   │
│  │  - Instance discovery                                     │   │
│  └──────────────────────┬───────────────────────────────────┘   │
└────────────────────────┬─────────────────────────────────────────┘
                         │ Unix Domain Socket (NDJSON)
                         │
┌────────────────────────▼─────────────────────────────────────────┐
│                    AimDB Instance(s)                              │
│  - AimX Remote Access Server                                     │
│  - Records, producers, consumers                                 │
│  - Runtime (Tokio/Embassy)                                       │
└──────────────────────────────────────────────────────────────────┘
```

### Component Responsibilities

| Component | Responsibility |
|-----------|---------------|
| **MCP Server** | Handle MCP protocol, manage LLM interactions |
| **AimX Client** | Reuse `aimdb-cli` connection logic, wrap in MCP tools |
| **AimDB Instance** | Provide data via AimX protocol (unchanged) |
| **LLM Host** | Invoke tools, consume resources, maintain conversation |

### Protocol Stack

```
┌─────────────────────────────────────────┐
│  Natural Language (User ↔ LLM)         │
├─────────────────────────────────────────┤
│  MCP (JSON-RPC 2.0 over stdio)         │
├─────────────────────────────────────────┤
│  AimX v1 (NDJSON over UDS)             │
├─────────────────────────────────────────┤
│  AimDB Core (Type-safe records)        │
└─────────────────────────────────────────┘
```

---

## MCP Server Design

### Crate Structure

```
aimdb-mcp/
├── Cargo.toml
├── README.md
├── src/
│   ├── lib.rs               # Library exports
│   ├── main.rs              # MCP server binary
│   ├── server.rs            # Core MCP server implementation
│   ├── tools/               # MCP tool implementations
│   │   ├── mod.rs
│   │   ├── instance.rs      # Instance discovery tools
│   │   ├── record.rs        # Record operations (list, get, set)
│   │   └── subscription.rs  # Real-time monitoring tools
│   ├── resources/           # MCP resource providers
│   │   ├── mod.rs
│   │   ├── schemas.rs       # Record schema resources
│   │   └── metrics.rs       # System metrics resources
│   ├── prompts/             # MCP prompt templates
│   │   ├── mod.rs
│   │   ├── debugging.rs     # Debugging workflows
│   │   └── monitoring.rs    # Monitoring patterns
│   ├── client.rs            # AimX client wrapper (reuse from aimdb-cli)
│   └── error.rs             # Error types
└── examples/
    └── basic_usage.rs       # Demonstration
```

### Core Dependencies

```toml
[dependencies]
# MCP protocol implementation
mcp-server = "0.1"           # Official Anthropic MCP SDK (when available)
# OR implement JSON-RPC 2.0 manually with:
# serde_json = "1"
# tokio = { version = "1", features = ["io-std", "macros", "rt-multi-thread"] }

# AimX client (reuse from aimdb-cli)
aimdb-core = { path = "../aimdb-core", features = ["std"] }

# Async runtime
tokio = { version = "1", features = ["full"] }

# Error handling
anyhow = "1"
thiserror = "1"

# Logging
tracing = "0.1"
tracing-subscriber = "0.3"
```

### MCP Tools Mapping

Map AimX operations to MCP tools:

| MCP Tool | AimX Method(s) | Description |
|----------|---------------|-------------|
| `discover_instances` | Discovery scan | Find running AimDB instances |
| `list_records` | `record.list` | List all records with metadata |
| `get_record` | `record.get` | Retrieve current value of record |
| `set_record` | `record.set` | Update writable record (if permitted) |
| `subscribe_record` | `record.subscribe` | Monitor record for changes |
| `unsubscribe_record` | `record.unsubscribe` | Stop monitoring record |
| `get_instance_info` | `hello`/`welcome` | Get instance metadata |
| `query_schema` | Derive from `record.list` | Infer record schema from metadata |

### MCP Resources

Expose data that LLMs can reference in context:

| Resource URI | Source | Description |
|--------------|--------|-------------|
| `aimdb://instances` | Discovery | List of available instances |
| `aimdb://<socket>/records` | `record.list` | All records for instance |
| `aimdb://<socket>/record/<name>` | `record.get` | Current record value |
| `aimdb://<socket>/schema/<name>` | Inferred | Record schema/metadata |
| `aimdb://<socket>/metrics` | Aggregated | Connection stats, queue sizes |

### MCP Prompts

Pre-defined interaction templates:

1. **Debugging Assistant**: Guide user through record inspection
2. **Monitoring Setup**: Help configure subscriptions for anomaly detection
3. **Performance Analysis**: Analyze buffer stats and identify bottlenecks
4. **Schema Explorer**: Interactive schema discovery and documentation

---

## AimX Integration

### Reusing aimdb-cli Client Module

The `aimdb-cli` crate already implements a robust AimX client. We should **reuse** this:

#### Option 1: Extract to Shared Library (Recommended)

Create `aimdb-client` crate:

```
aimdb-client/
├── Cargo.toml
└── src/
    ├── lib.rs
    ├── connection.rs      # From aimdb-cli
    ├── discovery.rs       # From aimdb-cli
    └── protocol.rs        # From aimdb-cli
```

**Benefits:**
- Single source of truth for AimX client logic
- Shared by `aimdb-cli` and `aimdb-mcp`
- Independent testing and versioning

**Dependencies:**
```toml
# Both tools depend on shared client
aimdb-cli → aimdb-client
aimdb-mcp → aimdb-client
```

#### Option 2: Direct Dependency (Simpler short-term)

`aimdb-mcp` depends on `aimdb-cli` as a library:

```toml
[dependencies]
aimdb-cli = { path = "../tools/aimdb-cli" }
```

**Trade-offs:**
- ✅ Faster to implement
- ❌ CLI binary pulls in unnecessary dependencies
- ❌ Unclear separation of concerns

**Recommendation**: Use Option 2 initially, refactor to Option 1 when stable.

### Connection Management

```rust
pub struct McpServer {
    /// Active AimX connection
    aimx_client: Option<AimxClient>,
    
    /// Cached instance discovery results
    discovered_instances: Vec<InstanceInfo>,
    
    /// Active subscriptions (for cleanup)
    active_subscriptions: HashMap<String, SubscriptionHandle>,
}

impl McpServer {
    /// Connect to a specific instance or auto-discover
    async fn connect(&mut self, socket_path: Option<&str>) -> Result<()> {
        if let Some(path) = socket_path {
            self.aimx_client = Some(AimxClient::connect(path).await?);
        } else {
            // Auto-discover and connect to first available
            let instances = discover_instances().await?;
            if let Some(instance) = instances.first() {
                self.aimx_client = Some(AimxClient::connect(&instance.socket_path).await?);
            }
        }
        Ok(())
    }
}
```

---

## MCP Tools & Resources

### Tool: `discover_instances`

Discover all running AimDB instances.

**Input Schema:**
```json
{
  "type": "object",
  "properties": {},
  "required": []
}
```

**Output:**
```json
{
  "instances": [
    {
      "socket_path": "/tmp/aimdb-demo.sock",
      "server_version": "aimdb/0.3.0",
      "protocol_version": "1.0",
      "record_count": 5,
      "writable_records": 1,
      "authenticated": false
    }
  ]
}
```

### Tool: `list_records`

List all records in an instance.

**Input Schema:**
```json
{
  "type": "object",
  "properties": {
    "socket": {
      "type": "string",
      "description": "Socket path (optional, uses auto-discovery if omitted)"
    },
    "writable_only": {
      "type": "boolean",
      "description": "Filter to show only writable records",
      "default": false
    }
  }
}
```

**Output:**
```json
{
  "records": [
    {
      "name": "server::Temperature",
      "type_id": "0xaee15e261d918c67cee5a96c2f604ce0",
      "buffer_type": "single_latest",
      "buffer_capacity": 1,
      "producer_count": 1,
      "consumer_count": 2,
      "writable": false,
      "created_at": "2025-11-02T10:30:00Z",
      "last_update": "2025-11-02T11:45:23Z"
    }
  ]
}
```

### Tool: `get_record`

Get current value of a record.

**Input Schema:**
```json
{
  "type": "object",
  "properties": {
    "record_name": {
      "type": "string",
      "description": "Full record name (e.g., 'server::Temperature')"
    },
    "socket": {
      "type": "string",
      "description": "Socket path (optional)"
    }
  },
  "required": ["record_name"]
}
```

**Output:**
```json
{
  "value": {
    "celsius": 23.5,
    "humidity": 45.2
  },
  "timestamp": "2025-11-02T11:45:23.123456789Z",
  "sequence": 42
}
```

### Tool: `subscribe_record`

Start monitoring a record for changes.

**Input Schema:**
```json
{
  "type": "object",
  "properties": {
    "record_name": {
      "type": "string",
      "description": "Record to monitor"
    },
    "socket": {
      "type": "string",
      "description": "Socket path (optional)"
    },
    "duration_seconds": {
      "type": "integer",
      "description": "How long to monitor (default: 60)",
      "default": 60
    },
    "max_events": {
      "type": "integer",
      "description": "Stop after N events (optional)"
    }
  },
  "required": ["record_name"]
}
```

**Output:**
```json
{
  "subscription_id": "sub-123",
  "events": [
    {
      "sequence": 43,
      "timestamp": "2025-11-02T11:45:24.456789123Z",
      "value": { "celsius": 23.6, "humidity": 45.1 }
    },
    {
      "sequence": 44,
      "timestamp": "2025-11-02T11:45:25.789123456Z",
      "value": { "celsius": 23.7, "humidity": 45.0 }
    }
  ],
  "dropped_events": 0
}
```

### Tool: `set_record`

Update a writable record.

**Input Schema:**
```json
{
  "type": "object",
  "properties": {
    "record_name": {
      "type": "string",
      "description": "Record to update"
    },
    "value": {
      "type": "object",
      "description": "New value (must match record schema)"
    },
    "socket": {
      "type": "string",
      "description": "Socket path (optional)"
    }
  },
  "required": ["record_name", "value"]
}
```

**Output:**
```json
{
  "success": true,
  "timestamp": "2025-11-02T11:46:00.000000000Z"
}
```

### Resource: `aimdb://instances`

List of available instances (updated on access).

**Content:**
```json
{
  "instances": [ /* ... */ ],
  "last_scan": "2025-11-02T11:45:00Z"
}
```

### Resource: `aimdb://<socket>/records`

All records for a specific instance.

**Example URI:** `aimdb:///tmp/aimdb-demo.sock/records`

**Content:**
```json
{
  "instance": {
    "socket": "/tmp/aimdb-demo.sock",
    "server": "aimdb/0.3.0"
  },
  "records": [ /* from record.list */ ]
}
```

### Prompt: Debugging Assistant

```yaml
name: debug_record
description: Guide user through debugging a specific record
arguments:
  - name: record_name
    description: Record to investigate
    required: true

instructions: |
  Help the user debug issues with a specific AimDB record by:
  1. Fetching current value and metadata
  2. Checking buffer type and capacity
  3. Inspecting producer/consumer counts
  4. Monitoring recent updates if available
  5. Suggesting potential issues (no producers, queue full, etc.)
```

---

## Implementation Plan

### Phase 1: Foundation (Week 1)

**Goal**: Basic MCP server with instance discovery and record listing.

**Tasks:**
1. Create `aimdb-mcp` crate structure
2. Add MCP JSON-RPC 2.0 protocol handler (stdio transport)
3. Integrate `aimdb-cli` client module (direct dependency initially)
4. Implement `discover_instances` tool
5. Implement `list_records` tool
6. Add basic error handling and logging
7. Create example demonstrating Claude Desktop integration

**Acceptance Criteria:**
- [ ] LLM can discover running AimDB instances
- [ ] LLM can list records with full metadata
- [ ] Errors are reported clearly in MCP responses
- [ ] Example runs successfully with Claude Desktop

### Phase 2: Core Operations (Week 2)

**Goal**: Enable read and write operations on records.

**Tasks:**
1. Implement `get_record` tool
2. Implement `set_record` tool
3. Add connection state management (auto-reconnect)
4. Implement `aimdb://instances` resource
5. Implement `aimdb://<socket>/records` resource
6. Add unit tests for tool handlers
7. Update documentation with usage examples

**Acceptance Criteria:**
- [ ] LLM can retrieve current record values
- [ ] LLM can update writable records
- [ ] Resources are accessible and cached appropriately
- [ ] Connection failures are handled gracefully
- [ ] 80%+ code coverage for tools

### Phase 3: Real-time Monitoring (Week 3)

**Goal**: Enable subscription-based monitoring.

**Tasks:**
1. Implement `subscribe_record` tool
2. Implement `unsubscribe_record` tool
3. Add subscription lifecycle management
4. Implement event aggregation for subscriptions
5. Add `aimdb://<socket>/metrics` resource
6. Create "Monitoring Assistant" prompt template
7. Add integration tests with live AimDB instance

**Acceptance Criteria:**
- [ ] LLM can monitor records for changes
- [ ] Subscriptions are properly cleaned up on disconnect
- [ ] Metrics resource provides useful system stats
- [ ] Prompt template guides effective monitoring setup

### Phase 4: Advanced Features (Week 4)

**Goal**: Schema inference, multi-instance support, and production hardening.

**Tasks:**
1. Implement schema inference from record metadata
2. Add `query_schema` tool
3. Support multiple simultaneous instance connections
4. Create "Debugging Assistant" prompt template
5. Add comprehensive error codes and recovery strategies
6. Performance optimization (connection pooling, caching)
7. Write detailed user guide and API reference
8. Add CI/CD pipeline for aimdb-mcp

**Acceptance Criteria:**
- [ ] LLM can infer and display record schemas
- [ ] Multiple instances can be queried in single session
- [ ] Performance meets target (<100ms tool invocation overhead)
- [ ] Documentation is complete and accurate
- [ ] CI runs all tests and lints successfully

### Phase 5: Refactoring & Polish (Optional)

**Goal**: Extract shared client library, improve architecture.

**Tasks:**
1. Create `aimdb-client` crate
2. Migrate connection logic from `aimdb-cli`
3. Update both `aimdb-cli` and `aimdb-mcp` to use shared client
4. Add advanced caching strategies
5. Implement connection pooling
6. Add telemetry and observability
7. Performance benchmarking

---

## Security Considerations

### Inherited Security Model

The MCP server inherits AimX security:

1. **Transport Security**: Unix domain sockets (local-only)
2. **Permissions**: File permissions on socket (0600, 0660, etc.)
3. **Authentication**: Optional bearer tokens
4. **Authorization**: Per-record write permissions

### MCP-Specific Concerns

1. **stdio Transport**: MCP server runs as subprocess of LLM host
   - **Risk**: LLM host compromise = MCP server compromise
   - **Mitigation**: Trust boundary is at LLM host level

2. **Tool Invocation**: LLM decides which tools to call
   - **Risk**: Malicious LLM could spam operations
   - **Mitigation**: Rate limiting, audit logging, user confirmation for writes

3. **Data Exposure**: Record values visible to LLM
   - **Risk**: Sensitive data in LLM context/logs
   - **Mitigation**: User controls which instances MCP can access via socket permissions

### Best Practices

1. **Read-Only Default**: MCP server should default to read-only mode
2. **Explicit Write Confirmation**: Require user confirmation for `set_record` operations
3. **Audit Logging**: Log all tool invocations to file or syslog
4. **Socket Allowlist**: Optional configuration to limit which sockets MCP can connect to
5. **Rate Limiting**: Throttle tool invocations to prevent abuse

### Configuration Example

```toml
# ~/.config/aimdb-mcp/config.toml

# Allow only specific sockets (optional)
allowed_sockets = [
  "/tmp/aimdb-*.sock",
  "/var/run/aimdb/*.sock"
]

# Require user confirmation for writes
confirm_writes = true

# Audit log path
audit_log = "/var/log/aimdb-mcp/audit.log"

# Rate limiting (requests per minute)
rate_limit = 100
```

---

## Performance & Scalability

### Performance Targets

| Operation | Target Latency | Notes |
|-----------|---------------|-------|
| Tool invocation overhead | <50ms | MCP → AimX round-trip |
| `discover_instances` | <100ms | Filesystem scan |
| `list_records` | <50ms | Single AimX request |
| `get_record` | <20ms | Single AimX request |
| `set_record` | <30ms | Single AimX request + ack |
| `subscribe_record` (setup) | <50ms | Initial subscription |
| Event delivery (subscription) | <10ms | Per event |

### Optimization Strategies

1. **Connection Pooling**: Reuse AimX connections across tool invocations
2. **Discovery Caching**: Cache instance discovery results (TTL: 30s)
3. **Record List Caching**: Cache `record.list` responses (TTL: 5s)
4. **Lazy Connection**: Don't connect until first tool invocation
5. **Async Tool Execution**: Execute independent tools in parallel
6. **Batch Operations**: Future AimX extension for `get_many`

### Scalability Considerations

- **Single Instance**: Handle 10+ concurrent tool invocations (typical LLM conversation)
- **Multi-Instance**: Support 5+ simultaneous AimDB connections
- **Subscriptions**: Limit to 5 active subscriptions per instance (prevent queue exhaustion)
- **Memory**: <50MB RSS under normal load

---

## Future Extensions

### v1.1 - Enhanced Observability

- **Tracing Integration**: Distributed tracing for tool invocations
- **Metrics Export**: Prometheus metrics for MCP server performance
- **Dashboard Resource**: Real-time dashboard data in MCP resource
- **Alerting Templates**: LLM-assisted alert rule generation

### v2.0 - Advanced Workflows

- **Multi-Step Tools**: Composite operations (e.g., "debug_pipeline")
- **Autonomous Monitoring**: LLM proactively monitors and alerts
- **Schema Validation**: Enforce schemas on `set_record` operations
- **Time-Series Queries**: Historical data access via subscriptions
- **Write Transactions**: Atomic multi-record updates

### v3.0 - Distributed Systems

- **Multi-Instance Federation**: Query across cluster of AimDB instances
- **Remote MCP**: MCP server runs on remote host, connects via TCP/SSH tunnel
- **Collaborative Debugging**: Multiple LLM agents coordinate debugging
- **Policy Engine**: Rule-based access control for MCP tools

---

## Appendix

### A. MCP Protocol Primer

**JSON-RPC 2.0 over stdio:**

Request:
```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "method": "tools/call",
  "params": {
    "name": "list_records",
    "arguments": {}
  }
}
```

Response:
```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "result": {
    "records": [ /* ... */ ]
  }
}
```

### B. Integration with LLM Clients

#### B.1. Claude Desktop

Add to `~/Library/Application Support/Claude/claude_desktop_config.json`:

```json
{
  "mcpServers": {
    "aimdb": {
      "command": "/path/to/aimdb-mcp",
      "args": [],
      "env": {
        "RUST_LOG": "info"
      }
    }
  }
}
```

#### B.2. VS Code with GitHub Copilot

**Option 1: User Settings**

Add to VS Code `settings.json` (User Settings):

```json
{
  "github.copilot.chat.mcp.servers": {
    "aimdb": {
      "command": "/path/to/aimdb/target/release/aimdb-mcp",
      "args": [],
      "env": {
        "RUST_LOG": "info"
      }
    }
  }
}
```

**Option 2: Workspace Settings** (Recommended for development)

Create `.vscode/settings.json` in your workspace:

```json
{
  "github.copilot.chat.mcp.servers": {
    "aimdb": {
      "command": "${workspaceFolder}/target/release/aimdb-mcp",
      "args": [],
      "env": {
        "RUST_LOG": "info"
      }
    }
  }
}
```

**Option 3: MCP Configuration File** (Future VS Code feature)

Create `.vscode/mcp.json`:

```json
{
  "mcpServers": {
    "aimdb": {
      "command": "${workspaceFolder}/target/release/aimdb-mcp",
      "args": [],
      "env": {
        "RUST_LOG": "info"
      }
    }
  }
}
```

**Using in VS Code Copilot Chat:**

Open Copilot Chat (`Ctrl+Shift+I` / `Cmd+Shift+I`) and use `@` mentions:

```
@aimdb show me all records in the running AimDB instance

@aimdb what's the current temperature reading?

@aimdb monitor the SystemStatus record for changes
```

The `@aimdb` mention tells Copilot to use the MCP server tools.

#### B.3. Zed Editor

Add to `~/.config/zed/settings.json`:

```json
{
  "language_models": {
    "mcp_servers": {
      "aimdb": {
        "command": "/path/to/aimdb-mcp",
        "args": [],
        "env": {
          "RUST_LOG": "info"
        }
      }
    }
  }
}
```

#### B.4. Cursor IDE

Add to Cursor settings (similar to VS Code):

```json
{
  "mcp.servers": {
    "aimdb": {
      "command": "/path/to/aimdb-mcp",
      "args": [],
      "env": {
        "RUST_LOG": "info"
      }
    }
  }
}
```

### C. Example Conversations

#### C.1. Claude Desktop / General Chat

**User:** "Show me all the records in my running AimDB instance"

**LLM:** [Invokes `discover_instances` → `list_records`]

```
I found an AimDB instance at /tmp/aimdb-demo.sock with 5 records:

1. server::Temperature (single_latest, 1 producer, 2 consumers)
2. server::SystemStatus (mailbox, 1 producer, 1 consumer)
3. server::UserEvents (spmc_ring, 3 producers, 5 consumers)
4. server::Config (mailbox, 0 producers, 3 consumers, writable)
5. server::AppSettings (single_latest, 1 producer, 1 consumer, writable)

Would you like to inspect any specific record?
```

**User:** "What's the current temperature?"

**Claude:** [Invokes `get_record` with `record_name: "server::Temperature"`]

```
Current temperature reading:
- Celsius: 23.5°C
- Humidity: 45.2%
- Timestamp: 2025-11-02 11:45:23 UTC
- Sequence: 42
```

**User:** "Monitor temperature for 30 seconds and alert me if it goes above 25"

**Claude:** [Invokes `subscribe_record` with duration=30, then analyzes events]

```
Monitored server::Temperature for 30 seconds (15 events received):

Temperature remained stable between 23.4°C and 23.8°C.
No readings exceeded 25°C threshold.

Peak: 23.8°C at 11:46:15
Average: 23.6°C
Trend: Slowly rising (+0.1°C/min)
```

#### C.2. VS Code Copilot Chat

**Developer in VS Code:** `@aimdb list all records`

**Copilot:** [Invokes `discover_instances` → `list_records`]

```
Found AimDB instance at /tmp/aimdb-demo.sock with 5 records:

📊 Records:
• Temperature (single_latest) - 1 producer, 2 consumers
• SystemStatus (mailbox) - 1 producer, 1 consumer
• UserEvents (spmc_ring) - 3 producers, 5 consumers
• Config (mailbox, writable) - 0 producers, 3 consumers
• AppSettings (single_latest, writable) - 1 producer, 1 consumer

💡 Tip: Use @aimdb get_record <name> to inspect a specific record.
```

**Developer:** `@aimdb why isn't Config updating?`

**Copilot:** [Invokes `list_records` → analyzes metadata]

```
🔍 Analysis of Config record:

⚠️  Issue Found: NO ACTIVE PRODUCERS
- Buffer type: mailbox (1 slot, overwrite semantics)
- Producer count: 0 ❌
- Consumer count: 3 ✅

📝 Diagnosis:
The Config record has no active producers writing to it. This could mean:
1. Producer task crashed or exited
2. Producer hasn't started yet
3. Initialization code missing

🔧 Suggested fixes:
1. Check if producer task is spawned: `grep "Config" src/**/*.rs`
2. Add logging to producer initialization
3. Verify producer isn't panicking at startup

Would you like me to search your codebase for Config producers?
```

**Developer:** `@aimdb monitor Temperature and alert if > 30°C`

**Copilot:** [Invokes `subscribe_record` with custom analysis]

```
🌡️  Monitoring Temperature for 60 seconds...

✅ All readings normal:
- Min: 23.2°C
- Max: 23.9°C
- Average: 23.6°C
- Readings: 28 samples
- Threshold: 30.0°C (never exceeded)

📈 Trend: Stable (±0.3°C variation)
🕐 Next check: In 5 minutes (auto-monitoring enabled)

No alerts triggered.
```

#### C.3. Debugging Workflow

**Developer (VS Code):** `@aimdb I'm debugging a data pipeline. Show me all records and their current states`

**Copilot:** [Multi-step: `discover_instances` → `list_records` → `get_record` for each]

```
🔍 Pipeline State Snapshot at 2025-11-02 11:45:23 UTC

📊 System Overview:
- Instance: /tmp/aimdb-demo.sock
- Total records: 5
- Active producers: 5
- Active consumers: 13

📝 Record Details:

1. Temperature (single_latest)
   Current: {"celsius": 23.5, "humidity": 45.2}
   Last update: 2 seconds ago
   Producers: 1 ✅ | Consumers: 2 ✅

2. SystemStatus (mailbox, writable)
   Current: {"status": "running", "uptime_secs": 3600}
   Last update: 5 seconds ago
   Producers: 1 ✅ | Consumers: 1 ✅

3. UserEvents (spmc_ring, capacity: 100)
   Current: {"user_id": 42, "event": "login", "timestamp": "..."}
   Last update: 0.5 seconds ago
   Producers: 3 ✅ | Consumers: 5 ✅
   Queue usage: 73/100 (73%)

4. Config (mailbox, writable) ⚠️
   No current value available
   Last update: never
   Producers: 0 ❌ | Consumers: 3 ⏳

5. AppSettings (single_latest, writable)
   Current: {"debug_mode": true, "log_level": "debug"}
   Last update: 10 minutes ago
   Producers: 1 ✅ | Consumers: 1 ✅

🚨 Attention Required:
- Config record has no producers (see item 4)
- UserEvents queue is 73% full (consider increasing capacity or adding consumers)
```

### D. Error Handling Matrix

| Error Scenario | AimX Error | MCP Response | Recovery Action |
|----------------|-----------|--------------|-----------------|
| No instances found | N/A | Error | Suggest starting AimDB with remote access |
| Connection refused | IO Error | Error | Check socket path and permissions |
| Record not found | NOT_FOUND | Error | List available records |
| Permission denied | PERMISSION_DENIED | Error | Explain write permissions needed |
| Subscription queue full | QUEUE_FULL | Warning | Suggest reducing subscription count |
| Protocol version mismatch | VERSION_MISMATCH | Error | Update aimdb-mcp or AimDB instance |

### E. Testing Strategy

**Unit Tests:**
- MCP protocol handler (JSON-RPC parsing)
- Tool input validation
- Resource URI parsing
- Error conversion

**Integration Tests:**
- Full MCP → AimX → AimDB round-trip
- Multiple concurrent tool invocations
- Subscription lifecycle (subscribe → events → unsubscribe)
- Connection failure and recovery

**End-to-End Tests:**
- Claude Desktop integration (manual)
- Example conversation flows
- Performance benchmarks

**Fuzzing:**
- Malformed MCP requests
- Invalid AimX responses
- Large payloads

---

## References

- [Model Context Protocol Specification](https://modelcontextprotocol.io/)
- [AimX v1 Protocol Specification](./008-M3-remote-access.md)
- [aimdb-cli Documentation](../../tools/aimdb-cli/README.md)
- [JSON-RPC 2.0 Specification](https://www.jsonrpc.org/specification)

---

**End of AimDB MCP Integration Strategy**
