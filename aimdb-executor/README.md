# aimdb-executor

Pure async executor trait definitions for AimDB - runtime-agnostic abstractions.

## Overview

`aimdb-executor` provides the trait abstractions that enable AimDB to work across different async runtime environments. This crate has **zero dependencies** and defines only traits, enabling dependency inversion where the core database depends on abstractions rather than concrete implementations.

**Key Features:**
- **Runtime Agnostic**: No concrete runtime dependencies
- **Simple Trait Structure**: 4 focused traits covering all runtime needs
- **Platform Flexible**: Works across std and no_std environments
- **Zero Dependencies**: Pure trait definitions with minimal coupling

## Architecture

```
┌────────────────────────────────────────┐
│         aimdb-executor (traits)        │
│  - RuntimeAdapter                      │
│  - TimeOps                             │
│  - Logger                              │
│  - Spawn                               │
└────────────────────────────────────────┘
            △                △
            │                │
    ┌───────┴────┐      ┌────┴──────┐
    │  Tokio     │      │  Embassy  │
    │  Adapter   │      │  Adapter  │
    └────────────┘      └───────────┘
```

## Trait Overview

### 1. RuntimeAdapter

Platform identification and metadata:

```rust
pub trait RuntimeAdapter: Send + Sync + 'static {
    fn runtime_name() -> &'static str
    where
        Self: Sized;
}
```

**Usage:** Identify runtime at compile-time (associated function, not method).

### 2. TimeOps

Time operations for async contexts:

```rust
pub trait TimeOps: RuntimeAdapter {
    type Instant: Clone + Send + Sync + core::fmt::Debug + 'static;
    type Duration: Clone + Send + Sync + core::fmt::Debug + 'static;

    fn now(&self) -> Self::Instant;
    fn duration_since(&self, later: Self::Instant, earlier: Self::Instant) 
        -> Option<Self::Duration>;
    fn millis(&self, ms: u64) -> Self::Duration;
    fn secs(&self, secs: u64) -> Self::Duration;
    fn micros(&self, micros: u64) -> Self::Duration;
    fn sleep(&self, duration: Self::Duration) -> impl Future<Output = ()> + Send;
}
```

**Usage:** Generic time operations with platform-specific Instant/Duration types.

### 3. Logger

Structured logging abstraction:

```rust
pub trait Logger: RuntimeAdapter {
    fn info(&self, message: &str);
    fn debug(&self, message: &str);
    fn warn(&self, message: &str);
    fn error(&self, message: &str);
}
```

**Usage:** Logging that works across `tracing` (std) and `defmt` (embedded).

### 4. Spawn

Task spawning with platform-specific tokens:

```rust
pub trait Spawn: RuntimeAdapter {
    type SpawnToken: Send + 'static;
    
    fn spawn<F>(&self, future: F) -> ExecutorResult<Self::SpawnToken>
    where
        F: Future<Output = ()> + Send + 'static;
}
```

**Usage:** Spawn async tasks with runtime-appropriate tokens (JoinHandle, SpawnToken, etc.).

## Quick Start

### Implementing an Adapter

```rust
use aimdb_executor::{RuntimeAdapter, TimeOps, Logger, Spawn, ExecutorResult};
use core::future::Future;

pub struct MyAdapter;

impl RuntimeAdapter for MyAdapter {
    fn runtime_name() -> &'static str
    where
        Self: Sized,
    {
        "my-runtime"
    }
}

impl TimeOps for MyAdapter {
    type Instant = my_runtime::Instant;
    type Duration = my_runtime::Duration;

    fn now(&self) -> Self::Instant {
        my_runtime::time::now()
    }
    
    fn duration_since(
        &self,
        later: Self::Instant,
        earlier: Self::Instant,
    ) -> Option<Self::Duration> {
        later.checked_duration_since(earlier)
    }
    
    fn millis(&self, ms: u64) -> Self::Duration {
        my_runtime::Duration::from_millis(ms)
    }
    
    fn secs(&self, secs: u64) -> Self::Duration {
        my_runtime::Duration::from_secs(secs)
    }
    
    fn micros(&self, micros: u64) -> Self::Duration {
        my_runtime::Duration::from_micros(micros)
    }
    
    fn sleep(&self, duration: Self::Duration) -> impl Future<Output = ()> + Send {
        my_runtime::time::sleep(duration)
    }
}

impl Logger for MyAdapter {
    fn info(&self, message: &str) {
        my_runtime::log::info!("{}", message);
    }
    
    fn debug(&self, message: &str) {
        my_runtime::log::debug!("{}", message);
    }
    
    fn warn(&self, message: &str) {
        my_runtime::log::warn!("{}", message);
    }
    
    fn error(&self, message: &str) {
        my_runtime::log::error!("{}", message);
    }
}

impl Spawn for MyAdapter {
    type SpawnToken = my_runtime::TaskHandle;
    
    fn spawn<F>(&self, future: F) -> ExecutorResult<Self::SpawnToken>
    where
        F: Future<Output = ()> + Send + 'static,
    {
        my_runtime::spawn(future)
            .map_err(|e| ExecutorError::SpawnFailed {
                message: e.to_string()
            })
    }
}
```

## Error Types

Simple error enum for executor operations:

```rust
pub enum ExecutorError {
    SpawnFailed { message: String },      // or &'static str in no_std
    RuntimeUnavailable { message: String },
    TaskJoinFailed { message: String },
}

pub type ExecutorResult<T> = Result<T, ExecutorError>;
```

## Trait Variants

All traits are directly usable - **no trait_variant macros used**. All traits require `Send + Sync` bounds where appropriate.

## Usage in AimDB Core

The core database is generic over runtime traits:

```rust
pub struct Database<A: RuntimeAdapter + TimeOps + Logger + Spawn> {
    adapter: A,
    // ... fields ...
}

impl<A> Database<A>
where
    A: RuntimeAdapter + TimeOps + Logger + Spawn,
{
    pub fn new(adapter: A) -> Self {
        // Use adapter traits
        adapter.info("Database initialized");
        let now = adapter.now();
        // ...
    }
}
```

## Runtime Trait Bundle

A convenience trait that bundles all requirements:

```rust
pub trait Runtime: RuntimeAdapter + TimeOps + Logger + Spawn {
    fn runtime_info(&self) -> RuntimeInfo {
        RuntimeInfo { name: Self::runtime_name() }
    }
}

// Auto-implemented for any type with all four traits
impl<T> Runtime for T where T: RuntimeAdapter + TimeOps + Logger + Spawn {}
```

## Existing Implementations

Two official adapters are available:

### Tokio Adapter
```toml
[dependencies]
aimdb-tokio-adapter = "0.1"
```
- Standard library environments
- Uses `tokio::time`, `tokio::task`, `tracing`

### Embassy Adapter
```toml
[dependencies]
aimdb-embassy-adapter = "0.1"
```
- Embedded no_std environments
- Uses `embassy_time`, `embassy_executor`, `defmt`

## Design Philosophy

1. **Minimal Surface Area**: Only essential operations
2. **No Concrete Dependencies**: Pure trait definitions
3. **Platform Neutral**: Works across std and no_std
4. **Future Proof**: Easy to extend without breaking changes

## Features

```toml
[features]
std = []  # Enable standard library support
```

## Testing

```bash
# Run tests
cargo test -p aimdb-executor

# Check no_std compatibility
cargo check -p aimdb-executor --no-default-features
```

## Documentation

Generate API docs:
```bash
cargo doc -p aimdb-executor --open
```

## Examples

See adapter implementations:
- `aimdb-tokio-adapter/src/lib.rs` - Full Tokio implementation
- `aimdb-embassy-adapter/src/lib.rs` - Full Embassy implementation

## License

See [LICENSE](../LICENSE) file.
