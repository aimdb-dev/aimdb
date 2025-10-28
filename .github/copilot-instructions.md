# GitHub Copilot Instructions for AimDB

## Project Status & Quick Start
AimDB is an async, in-memory database for real-time data synchronization across **MCU → edge → cloud** environments, targeting <50ms reactivity. The core architecture is fully implemented and functional.

**Implementation Status:**
- ✅ **Core Database** - Type-safe records with optimized TypeId lookups
- ✅ **Runtime Adapters** - Tokio (std) and Embassy (embedded) with unified API
- ✅ **Buffer Systems** - SPMC Ring, SingleLatest, Mailbox with simplified config
- ✅ **Producer-Consumer** - Complete with typed patterns
- ✅ **MQTT Connector** - Both std (rumqttc) and embedded (mountain-mqtt)
- ✅ **Examples & Tests** - Comprehensive coverage including embedded cross-compilation
- 🚧 **Kafka/DDS Connectors** - Planned
- 🚧 **CLI Tools** - Skeleton only

**Workspace Structure:**
```
aimdb-core/              # Core database engine
aimdb-executor/          # Runtime trait abstractions
aimdb-tokio-adapter/     # Tokio runtime adapter
aimdb-embassy-adapter/   # Embassy runtime adapter (configurable task pool)
aimdb-mqtt-connector/    # MQTT for std and embedded
examples/                # Working demos (tokio-mqtt, embassy-mqtt)
tools/aimdb-cli/         # CLI (skeleton)
```

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

**Runtime Adapters:**
- Tokio and Embassy implementations complete
- Macro-generated extension traits for consistency
- Embassy: configurable task pool (8/16/32 via features)
- Simplified Embassy buffer API (2 params vs 4)

**MQTT Connector:**
- Tokio support with `rumqttc`
- Embassy support with `mountain-mqtt`
- Dual runtime compatibility

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
