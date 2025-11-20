# GitHub Copilot Instructions for AimDB

## Project Status & Quick Start
AimDB is an async, in-memory database for data synchronization across **MCU → edge → cloud** environments, targeting <50ms reactivity. The core architecture is fully implemented and functional.

**Implementation Status:**
- ✅ **Core Database** - Type-safe records with optimized TypeId lookups
- ✅ **Runtime Adapters** - Tokio (std) and Embassy (embedded) with unified API
- ✅ **Buffer Systems** - SPMC Ring, SingleLatest, Mailbox with simplified config
- ✅ **Producer-Consumer** - Complete with typed patterns
- ✅ **MQTT Connector** - Both std (rumqttc) and embedded (mountain-mqtt)
- ✅ **KNX Connector** - Building automation support for std and embedded
- ✅ **MCP Server** - LLM-powered introspection with schema inference
- ✅ **Client Library** - AimX protocol client for remote database access
- ✅ **Sync API** - Synchronous wrapper for blocking code integration
- ✅ **CLI Tools** - Basic introspection and management commands
- ✅ **Examples & Tests** - Comprehensive coverage including embedded cross-compilation
- 🚧 **Kafka/DDS Connectors** - Planned

**Workspace Structure:**
```
aimdb-core/              # Core database engine + AimX protocol
aimdb-executor/          # Runtime trait abstractions
aimdb-tokio-adapter/     # Tokio runtime adapter
aimdb-embassy-adapter/   # Embassy runtime adapter (configurable task pool)
aimdb-client/            # AimX protocol client for remote access
aimdb-sync/              # Synchronous API wrapper
aimdb-mqtt-connector/    # MQTT for std and embedded
aimdb-knx-connector/     # KNX/IP for building automation (std and embedded)
tools/aimdb-cli/         # CLI tools for introspection
tools/aimdb-mcp/         # MCP server for LLM-powered introspection
examples/                # Working demos (see below)
```

**Example Projects:**
```
examples/tokio-mqtt-connector-demo/     # MQTT with Tokio runtime
examples/embassy-mqtt-connector-demo/   # MQTT on embedded (RP2040)
examples/tokio-knx-connector-demo/      # KNX with Tokio runtime
examples/embassy-knx-connector-demo/    # KNX on embedded
examples/sync-api-demo/                 # Synchronous API usage
examples/remote-access-demo/            # AimX protocol server
```

## AimDB MCP Tools

**IMPORTANT - Use MCP tools whenever possible for AimDB introspection:**

When working with running AimDB instances, **always prefer using the MCP tools** instead of writing custom code or using other methods. The MCP tools provide direct access to live database instances.

**Available MCP Tools:**
- `mcp_aimdb_discover_instances` - Find running AimDB servers
- `mcp_aimdb_list_records` - List all records in an instance
- `mcp_aimdb_get_record` - Get current value of a record
- `mcp_aimdb_set_record` - Set value of writable records
- `mcp_aimdb_query_schema` - Infer JSON Schema from record values
- `mcp_aimdb_subscribe_record` - Subscribe to live updates
- `mcp_aimdb_unsubscribe_record` - Unsubscribe from updates
- `mcp_aimdb_get_instance_info` - Get server version and capabilities
- `mcp_aimdb_list_subscriptions` - List active subscriptions
- `mcp_aimdb_get_notification_directory` - Get subscription data directory

**Usage Examples:**
```
# Find running instances
mcp_aimdb_discover_instances()

# Query schema for a record
mcp_aimdb_query_schema(
  socket_path: "/tmp/aimdb-demo.sock",
  record_name: "server::Config",
  include_example: true
)

# Get record value
mcp_aimdb_get_record(
  socket_path: "/tmp/aimdb-demo.sock", 
  record_name: "server::Temperature"
)

# Set writable record
mcp_aimdb_set_record(
  socket_path: "/tmp/aimdb-demo.sock",
  record_name: "server::AppSettings",
  value: {"log_level": "debug", "max_connections": 100, "feature_flag_alpha": false}
)
```

**When to use MCP tools:**
- ✅ Inspecting running AimDB instances
- ✅ Testing record schemas and values
- ✅ Debugging database state
- ✅ Monitoring live data
- ✅ Validating record configurations
- ✅ Quick data exploration

**Test server:**
The `examples/remote-access-demo` provides a test server with sample records at `/tmp/aimdb-demo.sock`.

## Development Workflow

**CRITICAL - Always test from workspace root:**
```bash
cd /aimdb
make check  # Fast: fmt + clippy + test + embedded cross-compile
make all    # Full: all targets and features (before pushing)
```

**Common Makefile commands:**
- `make help` - See all available commands
- `make build` - Build with all features
- `make test` - Run all tests
- `make fmt` - Format code
- `make clippy` - Linter checks
- `make doc` - Generate docs
- `make clean` - Clean artifacts

## Key Implementation Patterns

### Error Handling
Use `DbResult<T>` with `DbError` enum. All conversions centralized in `aimdb-core/src/error.rs`:
```rust
use crate::{DbError, DbResult};

pub async fn operation(&self) -> DbResult<()> {
    // Implementation
    Ok(())
}
```

### Runtime Adapters
All adapters implement traits from `aimdb-executor`:
- `RuntimeAdapter` - Platform identification
- `Spawn` - Task spawning
- `TimeOps` - Time operations (now, sleep)
- `Logger` - Logging abstraction

Extension traits generated via `impl_record_registrar_ext!` macro for API consistency.

### Buffer Configuration
Three buffer types available:
- **SPMC Ring** - Bounded backlog, independent consumers
- **SingleLatest** - Only newest value, skip intermediates
- **Mailbox** - Single slot, overwrite semantics

Embassy adapter uses simplified 2-parameter API: `buffer_sized::<CAP, CONSUMERS>(type)`

### Feature Flags
Key features in `Cargo.toml`:
- `tokio-runtime` / `embassy-runtime` - Runtime selection
- `embassy-task-pool-8/16/32` - Configurable task pool (Embassy only)
- `mqtt` - MQTT connector support
- `tracing` / `metrics` / `defmt` - Observability

## Completed Features (Do Not Re-Implement)

**Core (`aimdb-core`):**
- Type-safe records with `TypeId`-based routing
- Three buffer types with pluggable semantics
- Producer-consumer patterns
- `AimDbInner::get_typed_record<T, R>()` helper for lookups
- Centralized error conversions
- AimX protocol for remote access (Unix sockets, JSON-based)

**Runtime Adapters:**
- Tokio and Embassy implementations complete
- Macro-generated extension traits for consistency
- Embassy: configurable task pool (8/16/32 via features)
- Simplified Embassy buffer API (2 params vs 4)

**Client Library (`aimdb-client`):**
- AimX protocol client for remote database access
- Supports introspection, record read/write, subscriptions
- Works over Unix sockets for cross-process communication

**Sync API (`aimdb-sync`):**
- Synchronous wrapper for blocking code integration
- Bridges async AimDB to sync environments
- Uses Tokio channels for cross-thread communication

**MQTT Connector:**
- Tokio support with `rumqttc`
- Embassy support with `mountain-mqtt`
- Dual runtime compatibility

**KNX Connector:**
- Tokio support for std environments
- Embassy support for embedded environments
- Building automation integration (KNX/IP protocol)
- Dual runtime compatibility

**CLI Tools (`aimdb-cli`):**
- Basic database introspection commands
- Management and debugging utilities

**MCP Server (`aimdb-mcp`):**
- LLM-powered introspection with schema inference
- Provides tools for discovering, reading, and subscribing to records
- Enables AI-assisted debugging and monitoring

**Testing:**
- Comprehensive async test coverage
- Embedded cross-compilation validation (thumbv7em-none-eabihf)
- Working examples for both runtimes

## Next Priorities

1. **Kafka Connector** - `aimdb-kafka-connector` using `rdkafka` (std only)
2. **CLI Tools** - Introspection, monitoring, debugging commands
3. **Performance** - Benchmarks, profiling, latency measurement
4. **Documentation** - Quickstart guide, architecture docs, best practices

## Code Standards

- **Edition**: Rust 2021
- **async/await**: All operations are async
- **no_std**: Core and Embassy work without std
- **Docs**: Include examples in doc comments
- **Testing**: Use `tokio-test` for async tests
- **Performance**: Target <50ms latency, lock-free buffers, minimal allocations

## Platform Targets
- **MCU**: `no_std` + Embassy ✅
- **Edge**: Linux + Tokio ✅  
- **Cloud**: Containers + full std ✅

---

For detailed architecture and design decisions, see `/aimdb/docs/design/`.
