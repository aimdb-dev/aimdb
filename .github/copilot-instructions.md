# GitHub Copilot Instructions for AimDB

## Project Overview
AimDB is an async, in-memory database designed for real-time data synchronization across MCU → edge → cloud environments. This project focuses on millisecond-level reactivity, lock-free data structures, and platform portability.

## Core Architecture Principles

### 1. Async-First Design
- All operations should be async/await compatible
- Support for multiple async runtimes (Tokio, async-std, Embassy for embedded)
- Use `no_std` compatibility where applicable for MCU targets
- Prefer non-blocking algorithms and lock-free data structures

### 2. Performance Requirements
- Target <50ms reactivity for data operations
- Use ring buffers for high-throughput streaming
- Minimize allocations in hot paths
- Optimize for zero-copy operations where possible

### 3. Platform Targets
- **MCU**: `no_std` environments with Embassy executor
- **Edge**: Linux-class devices with standard async runtimes
- **Cloud**: Container/VM deployments with full std library

## Coding Standards

### Rust-Specific Guidelines
- Use `rustfmt` with default settings
- Follow Clippy recommendations with `-D warnings`
- Prefer explicit error handling with `Result<T, E>`
- Use `thiserror` for error types, `anyhow` for error contexts
- Document public APIs with comprehensive doc comments
- Include examples in doc comments for public functions

### Code Organization
```rust
// Preferred module structure
pub mod core {
    pub mod engine;
    pub mod database;
    pub mod notifications;
}
pub mod connectors {
    pub mod mqtt;
    pub mod kafka;
    pub mod dds;
}
pub mod adapters {
    pub mod embassy;
    pub mod tokio;
    pub mod async_std;
}
```

### Naming Conventions
- Use descriptive names that reflect AimDB domain concepts
- Prefer `stream_handler` over generic names like `handler`
- Use `_tx` / `_rx` suffixes for channel endpoints
- Use `_buf` suffix for buffer types
- Use `Config` suffix for configuration structs

### Error Handling Patterns
```rust
// Preferred error handling
use thiserror::Error;

#[derive(Error, Debug)]
pub enum AimDbError {
    #[error("Stream operation failed: {0}")]
    StreamError(String),
    #[error("Protocol bridge error: {protocol}")]
    BridgeError { protocol: String },
    #[error("Runtime error")]
    RuntimeError(#[from] RuntimeError),
}
```

### Async Patterns
```rust
// Preferred async patterns
pub async fn process_stream<T>(&mut self, mut stream: T) -> Result<(), AimDbError>
where
    T: Stream<Item = DataEvent> + Unpin,
{
    while let Some(event) = stream.next().await {
        self.handle_event(event).await?;
    }
    Ok(())
}
```

## Testing Guidelines

### Unit Tests
- Test each component in isolation
- Use `tokio-test` for async test utilities
- Mock external dependencies
- Test error conditions explicitly

### Integration Tests
- Test protocol bridges end-to-end
- Verify cross-platform compatibility
- Test performance characteristics
- Include benchmarks for critical paths

### Test Organization
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use tokio_test::{assert_ok, assert_err};
    
    #[tokio::test]
    async fn test_stream_processing() {
        // Test implementation
    }
}
```

## Performance Considerations

### Memory Management
- Prefer stack allocation for small, fixed-size data
- Use `Box<[T]>` for heap-allocated arrays
- Consider `Rc<T>` / `Arc<T>` for shared data
- Use memory pools for frequently allocated objects

### Concurrency
- Use `tokio::sync` primitives for async coordination
- Prefer channels over shared mutable state
- Use `DashMap` for concurrent hash maps
- Consider `crossbeam` for lock-free data structures

### Platform-Specific Optimizations
- Use feature flags for platform-specific code
- Optimize allocation patterns for embedded targets
- Leverage SIMD instructions where applicable
- Consider cache-friendly data layouts

## Documentation Standards

### Public API Documentation
```rust
/// Processes incoming data streams and maintains synchronized state.
///
/// The `StreamProcessor` handles real-time data ingestion from various
/// protocol bridges and ensures consistent state across all connected
/// nodes in the AimDB network.
///
/// # Examples
///
/// ```rust
/// use aimdb::StreamProcessor;
///
/// let processor = StreamProcessor::new();
/// processor.start_processing(stream).await?;
/// ```
///
/// # Performance
///
/// This implementation targets <50ms latency for data processing
/// operations and supports throughput of up to 100k events/second.
pub struct StreamProcessor {
    // Implementation
}
```

### Module Documentation
- Include module-level documentation explaining purpose
- Document feature flags and their effects
- Explain relationships to other modules
- Include architecture diagrams where helpful

## Dependencies

### Core Dependencies
- `tokio` or `async-std` for async runtime
- `embassy-executor` for embedded async
- `serde` for serialization
- `thiserror` for error handling
- `tracing` for observability

### Protocol Dependencies
- `rumqttc` for MQTT
- `rdkafka` for Kafka
- `cyclors` for DDS

### Development Dependencies
- `criterion` for benchmarking
- `proptest` for property-based testing
- `tokio-test` for async testing utilities

## Feature Flags

### Runtime Selection
```toml
[features]
default = ["tokio-runtime"]
tokio-runtime = ["tokio"]
async-std-runtime = ["async-std"]
embassy-runtime = ["embassy-executor"]
```

### Protocol Support
```toml
mqtt-bridge = ["rumqttc"]
kafka-bridge = ["rdkafka"]
dds-bridge = ["cyclors"]
```

### Platform Features
```toml
embedded = ["no-std-compat"]
observability = ["tracing", "metrics"]
```

## Git Commit Guidelines

### Commit Message Format
```
type(scope): brief description

Detailed explanation of changes, focusing on why
rather than what was changed.

- Bullet points for multiple changes
- Reference issue numbers: Fixes #123
```

### Types
- `feat`: New feature implementation
- `fix`: Bug fixes
- `perf`: Performance improvements
- `refactor`: Code restructuring
- `test`: Test additions or modifications
- `docs`: Documentation updates
- `ci`: CI/CD pipeline changes

### Scopes
- `core`: Core engine components
- `connectors`: Protocol connector implementations
- `adapters`: Async runtime integrations
- `embedded`: MCU-specific code
- `perf`: Performance optimizations

## Code Generation Preferences

When generating code for AimDB:

1. **Always consider async/await patterns** - This is an async-first project
2. **Include comprehensive error handling** - Use Result types and proper error propagation
3. **Add performance considerations** - Comment on allocation patterns and performance implications
4. **Consider platform compatibility** - Include feature flags for different target platforms
5. **Include documentation** - Generate doc comments for public APIs
6. **Add tests** - Include unit tests for new functionality
7. **Use domain-specific naming** - Prefer AimDB-specific terminology over generic names
8. **Consider observability** - Add tracing spans for important operations

## Example Code Templates

### New Module Template
```rust
//! Module description and purpose within AimDB
//!
//! This module handles [specific functionality] and integrates with
//! [related components].

extern crate alloc;

use alloc::sync::Arc;
use tracing::{debug, info, warn, error};

use crate::core::AimDbError;

/// Main struct documentation
pub struct ModuleName {
    // Fields with descriptive names
}

impl ModuleName {
    /// Constructor with comprehensive documentation
    pub fn new() -> Self {
        Self {
            // Initialization
        }
    }

    /// Async method template
    pub async fn process(&self) -> Result<(), AimDbError> {
        // Implementation with error handling
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_basic_functionality() {
        // Test implementation
    }
}
```
