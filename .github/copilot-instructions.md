# GitHub Copilot Instructions for AimDB

## Project Status & Quick Start
AimDB is in **early development** - the architecture is defined but implementation is just beginning. The codebase currently contains skeleton modules that need to be built out according to the architectural vision below.

**Key Development Workflow:**
- Use `make help` to see all available commands
- Run `make check` for quick dev checks (fmt + clippy + test)  
- Use `make build` and `make test` for standard development
- CI runs automatically on push/PR using the Makefile commands

## Project Architecture Vision
AimDB is an async, in-memory database for real-time data synchronization across **MCU → edge → cloud** environments, targeting <50ms reactivity.

### Future Usage Vision
See `docs/vision/future_usage.md` for an aspirational (non-functional) end-user example illustrating the intended ergonomic API across runtimes. Treat it as a guide when designing traits and modules—incremental PRs should converge toward that shape without introducing unused stubs prematurely.

### Current Workspace Structure
```
aimdb-core/          # Core database engine (skeleton)
aimdb-connectors/    # Protocol bridges: MQTT, Kafka, DDS (skeleton) 
aimdb-adapters/      # Async runtime adapters: Tokio, Embassy (skeleton)
tools/aimdb-cli/     # Command-line interface (skeleton)
examples/quickstart/ # Demo application (skeleton)
```

### Platform Targets
- **MCU**: `no_std` + Embassy executor for embedded async
- **Edge**: Linux devices with Tokio/async-std 
- **Cloud**: Container/VM deployments with full std library

## Implementation Guidelines

### Rust Standards
- **Edition**: 2024 (configured in Cargo.toml files)
- **Error Handling**: Use `thiserror` for library errors, `anyhow` for applications
- **Async**: All operations must be async/await compatible
- **Testing**: Use `tokio-test` for async test utilities
- **Docs**: Include examples in doc comments for public APIs

### Code Organization Pattern
When implementing modules, follow this structure:
```rust
//! Module-level docs explaining purpose and integration points

use crate::core::AimDbError;  // Consistent error handling
use tracing::{debug, info, warn, error};  // Observability

/// Public API with comprehensive docs and examples
pub struct ComponentName {
    // Implementation
}

impl ComponentName {
    /// Constructor with error handling
    pub fn new() -> Result<Self, AimDbError> {
        // Implementation
    }
    
    /// Async methods for all operations
    pub async fn process(&self) -> Result<(), AimDbError> {
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
- Target <50ms latency for data operations
- Use lock-free data structures where possible
- Minimize allocations in hot paths  
- Design for zero-copy operations

### Feature Flags (To Implement)
When adding dependencies, organize with feature flags:
```toml
[features]
default = ["tokio-runtime"]
tokio-runtime = ["tokio"]
embassy-runtime = ["embassy-executor"] 
mqtt = ["rumqttc"]
kafka = ["rdkafka"]
embedded = ["no-std-compat"]
```

### Naming Conventions
- Domain-specific names: `stream_handler` not `handler`
- Channel endpoints: `_tx` / `_rx` suffixes
- Buffers: `_buf` suffix
- Configuration: `Config` suffix

## Development Workflow

### Before Committing
```bash
make check  # Runs fmt + clippy + test
```

### Working with the Makefile
The project uses a simple Makefile for automation:
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

### Core Database Engine (`aimdb-core`)
1. Define `AimDbError` error type with `thiserror`
2. Implement in-memory storage with async operations
3. Add ring buffer for high-throughput streaming
4. Create notification system for state changes

### Runtime Adapters (`aimdb-adapters`) 
1. Tokio adapter for standard environments
2. Embassy adapter for embedded/MCU targets
3. Feature-gated compilation for different runtimes

### Protocol Connectors (`aimdb-connectors`)
1. MQTT bridge using `rumqttc`
2. Kafka bridge using `rdkafka` 
3. DDS bridge (future)
4. Feature flags for optional protocol support

### Examples & CLI
1. Build out `examples/quickstart` as a working demo
2. Implement `aimdb-cli` for development/debugging
3. Add integration tests that exercise the full stack

## Key Dependencies (To Add)
- `tokio` or `async-std` for async runtime
- `serde` for serialization  
- `thiserror` for error handling
- `tracing` for observability
- `rumqttc` for MQTT (feature-gated)
- `rdkafka` for Kafka (feature-gated)

When implementing, always consider async patterns, error handling, platform compatibility, and include comprehensive documentation with examples.
