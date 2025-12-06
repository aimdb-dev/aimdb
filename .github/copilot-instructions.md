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
- 🚧 **Advanced CLI Features** - Extended introspection and monitoring

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

## Common Pitfalls to Avoid

**❌ NEVER do these:**
- Use `std` features in `aimdb-core` or `aimdb-embassy-adapter` (breaks no_std)
- Add dependencies without checking `no_std` compatibility via `default-features = false`
- Write custom introspection code when MCP tools exist
- Run tests from subdirectories - always use workspace root (`/aimdb`)
- Use both `tokio-runtime` and `embassy-runtime` features simultaneously
- Use `tokio-runtime` feature for embedded targets
- Use `embassy-runtime` for cloud/server deployments
- Forget `#[tokio::test]` for async tests in std environments
- Use blocking operations in async contexts without proper handling

**✅ DO instead:**
- Check `cargo check --target thumbv7em-none-eabihf` for no_std compatibility
- Use MCP tools: `mcp_aimdb_*` functions for database introspection
- Run `make check` from `/aimdb` before committing
- Use appropriate runtime features for your target platform
- Use `#[test]` for sync tests, `#[tokio::test]` for async tests

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

## Naming Conventions

**Records & Types:**
- Record types: `PascalCase` (e.g., `Temperature`, `SensorData`, `AppConfig`)
- Record names: `namespace::RecordType` format (e.g., `server::Config`, `sensor::Temperature`)
- Struct fields: `snake_case` (e.g., `celsius`, `timestamp`, `sensor_id`)

**Buffer Parameters:**
- Use descriptive const generics: `CAPACITY`, `MAX_CONSUMERS`
- Embassy buffer API: `buffer_sized::<CAP, CONSUMERS>(buffer_type)`
- Tokio buffer API: `buffer_with_cfg(BufferCfg::new(...))`

**Feature Naming:**
- Runtime selection: `tokio-runtime` OR `embassy-runtime` (mutually exclusive)
- Task pool size: `embassy-task-pool-8` | `embassy-task-pool-16` | `embassy-task-pool-32`
- Connectors: `mqtt`, `knx`, `kafka` (when available)
- Observability: `tracing`, `metrics`, `defmt`

**File Organization:**
- Tests: `tests/integration_test.rs` or `tests/feature_name_tests.rs`
- Examples: `examples/runtime-connector-demo/` pattern
- Modules: `src/module_name.rs` or `src/module_name/mod.rs`

## Testing Guidelines

**Test Types:**
```rust
// Sync test (no async runtime needed)
#[test]
fn sync_operation_test() {
    // Implementation
}

// Async test in std environments
#[tokio::test]
async fn async_operation_test() {
    // Implementation
}

// Integration test structure
use aimdb_core::{buffer::BufferCfg, AimDbBuilder};
use aimdb_tokio_adapter::{TokioAdapter, TokioRecordRegistrarExt};
```

**When to use each:**
- `#[test]` - For pure logic, sync wrapper tests, data structure validation
- `#[tokio::test]` - For async operations, runtime adapter tests, connector tests
- Integration tests - In `tests/` directory for cross-crate functionality

**Embedded Testing:**
- Use `#[test]` (not `#[tokio::test]`) for Embassy adapter tests
- Cross-compile validation: `cargo check --target thumbv7em-none-eabihf`
- Embassy examples use `rust-toolchain.toml` with Rust 1.90+

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
2. **Advanced CLI Features** - Extended monitoring, debugging, and performance analysis
3. **Performance Optimization** - Benchmarks, profiling, latency measurement, memory optimization
4. **DDS Connector** - Real-time systems integration
5. **Documentation** - Quickstart guide, architecture docs, best practices
6. **Monitoring & Metrics** - Enhanced observability and telemetry

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

## Troubleshooting

**Build Issues:**
- **Build fails on embedded target**: Check `no_std` compatibility with `cargo check --target thumbv7em-none-eabihf`
- **Dependency conflicts**: Ensure `default-features = false` for no_std crates
- **Feature flag errors**: Don't mix `tokio-runtime` and `embassy-runtime`

**Runtime Issues:**
- **Tests hang**: Verify async runtime is properly configured (`#[tokio::test]` vs `#[test]`)
- **Embassy task pool overflow**: Use appropriate `embassy-task-pool-*` feature (8/16/32)
- **Memory allocation errors**: Check if `alloc` feature is enabled for no_std heap usage

**MCP & Database Issues:**
- **MCP tools can't find instance**: Ensure AimDB server is running and socket path is correct
- **Schema inference fails**: Verify record has been written at least once with valid data
- **Permission denied on socket**: Check Unix socket permissions and user access
- **Subscription data missing**: Use `mcp_aimdb_get_notification_directory()` to find data location

**Performance Issues:**
- **High latency (>50ms)**: Check buffer configuration and consumer count
- **Memory leaks**: Verify consumers are properly dropping subscriptions
- **Embedded stack overflow**: Reduce buffer sizes or increase stack size

**Quick Diagnostics:**
```bash
# Verify workspace health
make check

# Find running AimDB instances
# (Use MCP tool in AI context)
find /tmp -name "aimdb*.sock" 2>/dev/null

# Check embedded compatibility
cargo check --target thumbv7em-none-eabihf --features embassy-runtime
```

---

For detailed architecture and design decisions, see `/aimdb/docs/design/`.
