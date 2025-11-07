# Contributing to AimDB

Thank you for your interest in contributing to AimDB! This document provides guidelines and information for contributors.

## Project Overview

---

AimDB is an async, in-memory database designed for data synchronization across **MCU → edge → cloud** environments, targeting <50ms reactivity. The project is built in Rust and supports multiple platform targets from embedded microcontrollers to cloud deployments.

## Getting Started

### Prerequisites

- **Rust**: Latest stable version (2021 edition)
- **Git**: For version control
- **Make**: For build automation
- **Docker**: For running integration tests (optional)

### Development Setup

1. **Clone the repository:**
   ```bash
   git clone https://github.com/aimdb-dev/aimdb.git
   cd aimdb
   ```

2. **Quick development check:**
   ```bash
   make check  # Runs fmt + clippy + test
   ```

3. **Build the project:**
   ```bash
   make build  # Build with all features
   ```

4. **Run tests:**
   ```bash
   make test   # Run all tests
   ```

### Available Make Commands

- `make help` - Show all available commands
- `make check` - Quick dev checks (fmt + clippy + test)
- `make build` - Build with all features
- `make test` - Run all tests
- `make fmt` - Format code
- `make clippy` - Run linter with strict settings
- `make doc` - Generate and open documentation
- `make clean` - Clean build artifacts

### Security and License Auditing

AimDB uses `cargo deny` for dependency auditing:

```bash
cargo deny check          # Full audit (advisories, licenses, bans)
cargo deny check licenses # License compliance only
cargo deny check advisories # Security advisories only
```

## Code Standards

### Rust Guidelines

- **Edition**: Rust 2021
- **Error Handling**: Use `DbResult<T>` with `DbError` enum
- **Async**: All operations must be async/await compatible
- **Testing**: Use `tokio-test` for async test utilities
- **Documentation**: Include examples in doc comments for public APIs

### Code Organization Pattern

When implementing modules, follow this structure:

```rust
//! Module-level docs explaining purpose and integration points

use crate::{DbError, DbResult};  // Consistent error handling
use tracing::{debug, info, warn, error};  // Observability

/// Public API with comprehensive docs and examples
pub struct ComponentName {
    // Implementation
}

impl ComponentName {
    /// Constructor with error handling
    pub fn new() -> DbResult<Self> {
        // Implementation
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

- Target <50ms latency for data operations
- Use lock-free data structures where possible
- Minimize allocations in hot paths
- Design for zero-copy operations

### Naming Conventions

- Domain-specific names: `stream_handler` not `handler`
- Channel endpoints: `_tx` / `_rx` suffixes
- Buffers: `_buf` suffix
- Configuration: `Config` suffix

## Feature Flags

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

## Testing

### Running Tests

```bash
# All tests with all features
make test

# Embedded target cross-compilation check
make test-embedded

# Specific test
cargo test test_name --features tokio-runtime
```

### Test Requirements

- All public APIs must have tests
- Use `tokio-test` for async test utilities
- Include integration tests for cross-module functionality
- Test both success and error paths

## Documentation

- All public APIs must have documentation with examples
- Use `///` for public item documentation
- Use `//!` for module-level documentation
- Include code examples in doc comments
- Generate docs with: `make doc`

## Submission Process

### Before Committing

1. **Run the check command:**
   ```bash
   make check  # This runs fmt + clippy + test
   ```

2. **Ensure all tests pass:**
   ```bash
   make test
   ```

3. **Check license compliance:**
   ```bash
   cargo deny check  # Verify dependencies meet license requirements
   ```

4. **Check documentation:**
   ```bash
   make doc
   ```

### Pull Request Process

1. **Clone the repository** and create a feature branch
2. **Make your changes** following the code standards above
3. **Add tests** for new functionality
4. **Update documentation** as needed
5. **Run `make check`** to ensure code quality
6. **Submit a pull request** using our PR template

### Commit Messages

Use clear, descriptive commit messages:

```
add async stream handler for data sync

Implements bidirectional streaming between embedded and cloud layers

## Project Structure

```
aimdb-core/              # Core database engine
aimdb-executor/          # Runtime trait abstractions
aimdb-tokio-adapter/     # Tokio runtime adapter  
aimdb-embassy-adapter/   # Embassy runtime adapter (embedded)
aimdb-mqtt-connector/    # MQTT protocol connector
tools/aimdb-cli/         # Command-line interface (skeleton)
examples/                # Demo applications
```

## Next Development Areas

See `.github/copilot-instructions.md` for current implementation status and priorities:

- **Kafka Connector** - Kafka integration using `rdkafka`
- **DDS Connector** - DDS protocol support
- **CLI Tools** - Introspection, monitoring, debugging commands
- **Performance** - Benchmarks and profiling infrastructure

## Getting Help

- **Issues**: Use GitHub issues for bug reports and feature requests
- **Discussions**: Use GitHub discussions for general questions
- **Code Review**: All PRs require review before merging

## License Compliance

### Dependency Licensing

AimDB follows a permissive licensing strategy compatible with commercial use. The project accepts dependencies with these licenses:

- **Primary**: MIT, Apache-2.0 (preferred for new dependencies)
- **Compatible**: BSD-2-Clause, BSD-3-Clause, ISC
- **Unicode Data**: Unicode-3.0, Unicode-DFS-2016 (for Unicode processing crates)

### Adding Dependencies

Before adding new dependencies:

1. **Check the license** with `cargo deny check`
2. **Ensure compatibility** with our allowed licenses in `deny.toml`
3. **Avoid copyleft licenses** (GPL, LGPL, etc.) that could restrict commercial use
4. **Document the rationale** for any new license additions in your PR

If you need to add a dependency with a new license:
- Verify it's OSI-approved and business-friendly
- Update `deny.toml` to include the new license
- Explain the necessity in your PR description

### License Audit

Run license checks as part of development:
```bash
cargo deny check licenses  # Check license compliance
make check                 # Includes all development checks
```

## Code of Conduct

Please be respectful and constructive in all interactions. We're building this project together and want everyone to feel welcome to contribute.

## License

By contributing to AimDB, you agree that your contributions will be licensed under the same license as the project.
