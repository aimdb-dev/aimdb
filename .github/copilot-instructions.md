# GitHub Copilot Instructions for AimDB

## Project Status & Quick Start
AimDB is in **active development** - the core architecture is implemented and functional. Working features include runtime adapters (Tokio & Embassy), three buffer types (SPMC Ring, SingleLatest, Mailbox), type-safe producer-consumer patterns, and comprehensive examples. Focus is now on protocol connectors, performance optimization, and expanded features.

**Current Implementation Status:**
- ✅ Core Database Engine - Fully functional with type-safe records
- ✅ Runtime Adapters - Tokio (std) and Embassy (embedded) working
- ✅ Buffer Systems - Three buffer types implemented
- ✅ Producer-Consumer - Complete with emitter and cross-record communication  
- ✅ Examples & Testing - Multiple working demos with comprehensive coverage
- 🚧 Protocol Connectors - MQTT, Kafka, DDS (planned, not yet implemented)
- 🚧 CLI Tools - Skeleton structure exists, needs implementation

**Key Development Workflow:**
- Use `make help` to see all available commands
- Run `make check` for quick dev checks (fmt + clippy + test)  
- Use `make build` and `make test` for standard development
- CI runs automatically on push/PR using the Makefile commands

**IMPORTANT - Testing Protocol:**
- **Always run `make check` from `/aimdb` directory** after making changes
- This ensures: code formatting, linter checks, all tests pass, embedded cross-compilation works
- Do NOT run tests from subdirectories - the Makefile must be executed from workspace root
- If `make check` fails, fix issues before proceeding with further changes

## Project Architecture
AimDB is an async, in-memory database for real-time data synchronization across **MCU → edge → cloud** environments, targeting <50ms reactivity.

### Future Usage Vision
See `docs/vision/future_usage.md` for an aspirational end-user example. The current implementation already supports most of these patterns - see the `examples/` directory for working code.

### Current Workspace Structure
```
aimdb-core/              # ✅ Core database engine - IMPLEMENTED
aimdb-executor/          # ✅ Runtime trait abstractions - IMPLEMENTED  
aimdb-tokio-adapter/     # ✅ Tokio runtime adapter - IMPLEMENTED
aimdb-embassy-adapter/   # ✅ Embassy runtime adapter - IMPLEMENTED
aimdb-macros/            # ✅ Proc macros - IMPLEMENTED
examples/
  ├── tokio-runtime-demo/          # ✅ Tokio with all buffer types
  ├── embassy-runtime-demo/        # ✅ Embedded example
  ├── producer-consumer-demo/      # ✅ Type-safe patterns
  └── shared/                      # ✅ Runtime-agnostic services
tools/aimdb-cli/         # 🚧 CLI - SKELETON ONLY
```

**Note:** Protocol connectors (`aimdb-mqtt-connector`, `aimdb-kafka-connector`) do not exist yet - this is the next major milestone.

### Platform Targets
- **MCU**: `no_std` + Embassy executor for embedded async ✅ WORKING
- **Edge**: Linux devices with Tokio/async-std ✅ WORKING
- **Cloud**: Container/VM deployments with full std library ✅ WORKING

## Implementation Guidelines

### Rust Standards
- **Edition**: 2021 (configured in Cargo.toml files)
- **Error Handling**: Use `thiserror` for library errors (✅ `DbError` implemented)
- **Async**: All operations are async/await compatible (✅ fully implemented)
- **Testing**: Use `tokio-test` for async test utilities (✅ in use)
- **Docs**: Include examples in doc comments for public APIs (✅ established pattern)
- **no_std Support**: Core and Embassy adapter work in embedded environments (✅ working)

### Code Organization Pattern
When implementing modules, follow this structure:
```rust
//! Module-level docs explaining purpose and integration points

use crate::DbError;  // Consistent error handling
use tracing::{debug, info, warn, error};  // Observability

/// Public API with comprehensive docs and examples
pub struct ComponentName {
    // Implementation
}

impl ComponentName {
    /// Constructor with error handling
    pub fn new() -> DbResult<Self> {
        #[cfg(feature = "tracing")]
        debug!("Creating ComponentName");
        
        Ok(Self { /* ... */ })
    }
    
    /// Async methods for all operations
    pub async fn process(&self) -> DbResult<()> {
        // Implementation with tracing
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[tokio::test]
    async fn test_functionality() {
        // Test implementation
    }
}
```

### Performance Requirements
- Target <50ms latency for data operations (architecture supports this)
- Lock-free data structures implemented in buffer systems
- Minimal allocations in hot paths (designed into buffer traits)
- Zero-copy operations where possible (emitter design)

### Feature Flags (Implemented)
The project uses feature flags for conditional compilation:
```toml
[features]
# Core features (aimdb-core)
default = ["std"]
std = []
embedded = []

# Runtime features
tokio-runtime = ["dep:tokio"]
embassy-runtime = ["dep:embassy-executor", "dep:embassy-time"]

# Optional features
tracing = ["dep:tracing"]
metrics = ["dep:metrics"]

# Future protocol connectors (not yet implemented)
# mqtt = ["rumqttc"]
# kafka = ["rdkafka"]
```

### Naming Conventions
- Domain-specific names: `stream_handler` not `handler`
- Channel endpoints: `_tx` / `_rx` suffixes
- Buffers: `_buf` suffix
- Configuration: `Config` suffix

## Development Workflow

### Testing Requirements - CRITICAL

**Always test from workspace root:**
```bash
cd /aimdb
make check
```

**What `make check` validates:**
1. ✅ **Code formatting** (`cargo fmt --all -- --check`)
2. ✅ **Linter checks** (`cargo clippy` with strict settings)
3. ✅ **All unit tests** pass (`cargo test --all-features`)
4. ✅ **Embedded cross-compilation** works (thumbv7em-none-eabihf target)

**Common Mistakes to Avoid:**
- ❌ Running `cargo test` from subdirectories (misses workspace checks)
- ❌ Skipping `make check` after changes (breaks CI)
- ❌ Only testing one feature flag combination
- ❌ Not verifying embedded compatibility

**Testing Workflow:**
1. Make code changes
2. Run `cd /aimdb && make check`
3. If it passes, commit
4. If it fails, fix issues and repeat

### Before Committing
```bash
cd /aimdb
make check  # Quick check: fmt + clippy + test
make all    # Full validation: build all targets + run all tests (recommended before push)
```

**Quick iteration:** Use `make check` for fast development feedback.  
**Final validation:** Use `make all` before committing to ensure everything builds correctly across all configurations.

### Working with the Makefile
The project uses a simple Makefile for automation:
- `make check` - Fast dev check (fmt + clippy + test)
- `make all` - Complete build and test (all targets and features)
- `make build` - Build with all features
- `make test` - Run all tests
- `make fmt` - Format code  
- `make clippy` - Run linter with strict settings
- `make doc` - Generate and open documentation
- `make clean` - Clean build artifacts

### CI Integration
GitHub Actions uses the Makefile commands, ensuring local development matches CI exactly.

### GitHub CLI Integration
Use `gh` commands to retrieve project information directly from GitHub:
- `gh issue list` - View current issues and their status
- `gh pr list` - Check open pull requests
- `gh repo view` - Get repository overview and recent activity
- `gh issue view <number>` - Get detailed issue information
- `gh pr view <number>` - Review pull request details and discussions

This helps stay current with project priorities, bug reports, and community contributions.

## Implementation Priorities

### ✅ Completed Features (Do Not Re-Implement)

**Core Database Engine (`aimdb-core`)**
- [x] `DbError` error type with `thiserror` - COMPLETE
- [x] In-memory storage with async operations - COMPLETE  
- [x] Three buffer types (SPMC Ring, SingleLatest, Mailbox) - COMPLETE
- [x] Producer-consumer pattern with typed records - COMPLETE
- [x] Emitter for cross-record communication - COMPLETE
- [x] Runtime-agnostic design - COMPLETE

**Runtime Adapters**
- [x] Tokio adapter for standard environments - COMPLETE
- [x] Embassy adapter for embedded/MCU targets - COMPLETE
- [x] Feature-gated compilation - COMPLETE
- [x] Unified trait system (RuntimeAdapter, TimeOps, Logger, Spawn) - COMPLETE

**Examples & Testing**
- [x] Working demos (tokio-runtime, embassy-runtime, producer-consumer) - COMPLETE
- [x] Runtime-agnostic shared services - COMPLETE
- [x] Comprehensive async test coverage - COMPLETE

### 🚧 Current Focus Areas (Based on Open Issues #9-#18)

**Protocol Connectors (`aimdb-connectors`) - NOT YET IMPLEMENTED**
The next major milestone is adding protocol bridges:
1. MQTT bridge using `rumqttc` for IoT device connectivity
2. Kafka bridge using `rdkafka` for cloud streaming
3. DDS bridge (future) for real-time systems
4. Feature flags for optional protocol support

These will be new workspace crates: `aimdb-mqtt-connector`, `aimdb-kafka-connector`, etc.

**CLI Tools (`tools/aimdb-cli`) - SKELETON ONLY**
1. Database introspection commands
2. Record monitoring and debugging
3. Performance profiling utilities
4. Configuration validation

**Ongoing Improvements**
- Error severity classification system
- Enhanced tracing/observability integration
- Cross-module error boundary handling
- Documentation and usage examples
- Performance validation and benchmarking

### Key Dependencies
1. Build out `examples/quickstart` as a working demo
2. Implement `aimdb-cli` for development/debugging
3. Add integration tests that exercise the full stack

## Key Dependencies (Current State)

### Implemented & In Use
- ✅ `tokio` (v1.47+) - async runtime for std environments
- ✅ `embassy-executor` - async runtime for embedded/no_std
- ✅ `embassy-time` - time operations for embedded systems
- ✅ `thiserror` (v2.0+) - error handling in libraries
- ✅ `tracing` - observability (feature-gated)
- ✅ `metrics` - performance metrics (feature-gated)
- ✅ `serde` - serialization support
- ✅ `tokio-test` - async testing utilities

### To Be Added (Protocol Connectors)
- 🚧 `rumqttc` - MQTT client (feature-gated)
- 🚧 `rdkafka` - Kafka client (feature-gated)
- 🚧 Additional protocol libraries as needed

When implementing new features, always consider:
1. **Async patterns** - All operations use async/await
2. **Error handling** - Use `DbResult<T>` with proper error variants
3. **Platform compatibility** - Test with both std and no_std where applicable
4. **Documentation** - Include comprehensive docs with working examples
5. **Testing** - Add both unit and integration tests
6. **Feature flags** - Gate optional dependencies appropriately
