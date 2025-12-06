# 🎯 Design: Error Handling & Feature Flags (M1)

**Status**: ✅ Implemented  
**Milestone**: Core Foundation & Error Handling - M1  
**Goal**: Establish unified error model and feature-flag architecture for AimDB's cross-platform async database  
**Platforms**: `std` (Linux/macOS/Cloud), `no_std` (MCU/Embedded)

---

## 🎯 Design Overview

### Problem Statement

AimDB needs a **unified error handling system** that works seamlessly across:
- **MCU devices** running Embassy executor (`no_std`)
- **Edge devices** with Tokio runtime (`std`)  
- **Cloud deployments** with full standard library

The error system must be:
- ✅ **Zero-cost on embedded**: no heap allocations in error paths
- ✅ **Rich on std**: detailed error messages and context chaining
- ✅ **Composable**: errors from different subsystems integrate cleanly
- ✅ **Observable**: structured for logging and metrics

### Success Criteria

**Functional Requirements:**
- [ ] Single `DbError` type compiles on both `std` and `no_std`  
- [ ] Feature flags control platform-specific error behavior
- [ ] `Result<T, DbError>` used consistently across all public APIs
- [ ] Error conversion traits for `std::io::Error`, serde errors
- [ ] Tracing integration (behind feature flags)

**Non-Functional Requirements:**
- [ ] <1μs error construction overhead on embedded targets
- [ ] Zero heap allocations in `no_std` error paths
- [ ] Error size ≤ 64 bytes (cache-friendly)
- [ ] 100% feature gate coverage in CI matrix

---

## 🏗️ Architecture Design

### Error Type Hierarchy

```rust
/// Core error type for all AimDB operations
#[derive(Debug)]
#[cfg_attr(feature = "std", derive(thiserror::Error))]
pub enum DbError {
    /// Network operation failed
    #[cfg_attr(feature = "std", error("Network timeout after {timeout_ms}ms"))]
    NetworkTimeout { timeout_ms: u64 },
    
    /// Memory/storage constraint violated
    #[cfg_attr(feature = "std", error("Buffer capacity exceeded: {used}/{capacity}"))]
    CapacityExceeded { used: usize, capacity: usize },
    
    /// Data serialization/deserialization failed
    #[cfg_attr(feature = "std", error("Serialization error: {context}"))]
    SerializationError { 
        #[cfg(feature = "std")]
        context: String,
        #[cfg(not(feature = "std"))]
        _context: (),
    },
    
    /// Invalid configuration or parameters
    #[cfg_attr(feature = "std", error("Configuration error: {message}"))]
    ConfigurationError {
        #[cfg(feature = "std")]
        message: String,
        #[cfg(not(feature = "std"))]
        error_code: u32,
    },
    
    /// Resource unavailable (connection, memory, etc.)
    #[cfg_attr(feature = "std", error("Resource unavailable: {resource_type}"))]
    ResourceUnavailable { 
        #[cfg(feature = "std")]
        resource_type: String,
        #[cfg(not(feature = "std"))]
        resource_id: u8,
    },
    
    /// Platform-specific I/O errors (std only)
    #[cfg(feature = "std")]
    #[error("I/O operation failed")]
    Io(#[from] std::io::Error),
    
    /// Embedded-specific errors (no_std only)
    #[cfg(not(feature = "std"))]
    HardwareError { error_code: u32 },
}

/// Convenience Result type for all AimDB operations
pub type DbResult<T> = Result<T, DbError>;
```

### Platform-Specific Error Behavior

#### `std` Platforms (Edge/Cloud)
```rust
impl DbError {
    /// Rich error context with chaining
    pub fn with_context<S: Into<String>>(self, context: S) -> Self {
        // Implementation using error chaining
    }
    
    /// Convert to anyhow::Error for application boundaries
    pub fn into_anyhow(self) -> anyhow::Error {
        self.into()
    }
}

impl From<serde_json::Error> for DbError {
    fn from(err: serde_json::Error) -> Self {
        DbError::SerializationError {
            context: err.to_string(),
        }
    }
}
```

#### `no_std` Platforms (MCU/Embedded)  
```rust
impl core::fmt::Display for DbError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            DbError::NetworkTimeout { timeout_ms } => {
                write!(f, "Network timeout: {}ms", timeout_ms)
            }
            DbError::CapacityExceeded { used, capacity } => {
                write!(f, "Buffer full: {}/{}", used, capacity)
            }
            // ... other variants
        }
    }
}

impl DbError {
    /// Error code for embedded systems (no heap allocation)
    pub const fn error_code(&self) -> u32 {
        match self {
            DbError::NetworkTimeout { .. } => 0x1001,
            DbError::CapacityExceeded { .. } => 0x1002,
            DbError::SerializationError { .. } => 0x1003,
            // ... other mappings
        }
    }
}
```

---

## 🎛️ Feature Flag Architecture

### Core Feature Matrix

```toml
[features]
# Platform targets
default = ["std"]
std = ["thiserror", "tracing/std"]
embedded = []

# Runtime adapters  
tokio-runtime = ["tokio", "std"]
embassy-runtime = ["embassy-executor", "embedded"]

# Protocol connectors
mqtt = ["rumqttc", "std"]
kafka = ["rdkafka", "std"] 
rest = ["reqwest", "std"]
dds = ["cyclonedx", "std"]

# Development features
tracing = ["dep:tracing"]
metrics = ["dep:metrics", "std"]

# Testing features
test-utils = ["std"]
```

### Feature Combinations & Validation

**Valid Platform Combinations:**
```
✅ std + tokio-runtime + mqtt + kafka     # Cloud/Edge deployment
✅ embedded + embassy-runtime             # MCU deployment  
✅ std + tokio-runtime                    # Basic edge device
✅ embedded                               # Minimal MCU
```

**Invalid Combinations (CI enforced):**
```
❌ embedded + std                         # Contradictory
❌ embassy-runtime + tokio-runtime        # Runtime conflict
❌ mqtt + embedded                        # Network stack not available
```

### Conditional Compilation Strategy

```rust
// Core error type available on all platforms
pub use error::DbError;

// Platform-specific modules
#[cfg(feature = "std")]
pub mod std_support {
    pub use crate::error::StdErrorExt;
}

#[cfg(all(not(feature = "std"), feature = "embedded"))]
pub mod embedded_support {
    pub use crate::error::EmbeddedErrorExt;
}

// Tracing integration
#[cfg(feature = "tracing")]
macro_rules! trace_error {
    ($err:expr) => {
        tracing::error!(error = ?$err, error_code = $err.error_code());
    };
}

#[cfg(not(feature = "tracing"))]
macro_rules! trace_error {
    ($err:expr) => {};
}
```

---

## 🎯 Design Principles & Constraints

### Core Design Principles

**1. Platform-Agnostic API Surface**
The error type must present a unified interface regardless of the underlying platform capabilities. Users should write the same error-handling code whether targeting MCU or cloud environments.

**2. Zero-Cost Abstractions**  
Platform-specific behavior is achieved through compile-time feature selection, not runtime polymorphism. No performance penalty for unused platform capabilities.

**3. Composable Error Hierarchies**
Errors from different AimDB subsystems (database, connectors, adapters) must integrate cleanly without losing type information or context.

**4. Observability-First Design**
Error types are designed as structured data that can be efficiently logged, traced, and converted to metrics across all platforms.

### Architectural Constraints

**Memory Constraints (Embedded)**
- Error enum size ≤ 64 bytes (single cache line)
- Zero heap allocations during error construction
- Const-evaluable error codes for compile-time optimization
- Minimal stack usage in error paths

**Performance Constraints (All Platforms)**  
- Error construction overhead <1μs on embedded, <10μs on std
- Error display formatting without additional allocations
- Efficient error propagation through async call stacks
- Lock-free error handling for real-time guarantees

**Compatibility Constraints**
- Support for `no_std` environments without `alloc`
- Graceful degradation when std library features unavailable  
- Forward compatibility with future platform additions
- Stable error code mappings for embedded diagnostics

---

## 🔍 Error Taxonomy & Semantics

### Error Category Design

**Infrastructure Errors** (`0x1000` range)
- Network timeouts and connectivity failures
- Resource exhaustion (memory, connections, file handles)  
- Hardware interface failures (embedded sensors, actuators)

**Data Errors** (`0x2000` range)  
- Serialization/deserialization failures
- Schema validation errors
- Data corruption detection
- Type conversion failures

**Operational Errors** (`0x3000` range)
- Configuration errors and invalid parameters
- Authentication and authorization failures  
- Rate limiting and quota exceeded
- Service unavailability

**System Errors** (`0x4000` range)
- Platform-specific I/O errors (std only)
- Runtime executor failures  
- Memory allocation failures
- Thread/task management errors

### Error Severity & Recovery Strategy

**Fatal Errors** (System integrity compromised)
- Hardware failures requiring restart
- Memory corruption detected
- Critical configuration errors

**Recoverable Errors** (Temporary failures)  
- Network timeouts with retry potential
- Resource temporarily unavailable
- Transient data validation failures

**Warning Conditions** (Degraded performance)
- Buffer capacity approaching limits
- Non-critical service unavailable
- Performance threshold violations

### Error Context & Observability

**Structured Error Data**
Every error carries structured metadata for observability:
- Timestamp (platform-appropriate precision)
- Error category and severity level
- Contextual parameters (timeout values, resource IDs)
- Platform-specific diagnostics

**Tracing Integration Points**
```rust
// Error construction automatically creates tracing span
let error = DbError::NetworkTimeout { timeout_ms: 5000 };
// Span: error.category=infrastructure, error.code=0x1001, timeout_ms=5000

// Error propagation maintains causal chain
async fn operation() -> DbResult<()> {
    let result = network_call().await?; // Span links maintained
    Ok(result)
}
```

**Metrics Extraction**
Errors are designed for automatic metrics generation:
- Error rate by category and severity
- Performance impact of error handling overhead
- Platform-specific error distribution
- Recovery success rates

---

## 🧪 Design Validation Strategy

### Design Verification Methods

**Compile-Time Validation**
- Feature flag combination compatibility matrix
- Memory layout optimization verification  
- Zero-cost abstraction confirmation through assembly inspection
- Platform capability matching (no std features on embedded)

**Runtime Behavioral Validation**  
- Error size and alignment constraints
- Performance overhead measurement across platforms
- Memory allocation tracking (zero-alloc verification)
- Error propagation through async contexts

**Cross-Platform Consistency**
- Identical error semantics across platforms
- Consistent error code mapping
- Unified API behavior verification
- Platform-specific extension isolation

---

## 🏛️ Architectural Analysis

### Cross-Platform Abstraction Strategy

**Conditional Compilation Architecture**
The error system uses Rust's cfg attributes to create platform-specific behavior while maintaining a unified API:

```rust
// Unified enum definition with platform-conditional fields
pub enum DbError {
    NetworkTimeout { 
        timeout_ms: u64,
        #[cfg(feature = "std")]
        context: String,           // Rich context on std
        #[cfg(not(feature = "std"))]
        _reserved: (),            // Zero-cost placeholder on embedded
    }
}
```

This approach provides:
- **Single source of truth** for error definitions
- **Platform-optimal implementations** without runtime overhead  
- **API consistency** across deployment environments
- **Future extensibility** without breaking existing code

### Feature Flag Dependency Graph

```
Platform Capabilities:
├── std
│   ├── thiserror (automatic trait derivation)
│   ├── heap allocation (rich error context) 
│   └── std::io integration
└── embedded  
    ├── const error codes (compile-time optimization)
    ├── stack-only operations (zero heap usage)
    └── hardware-specific error variants

Runtime Integration:
├── tokio-runtime → std
└── embassy-runtime → embedded

Protocol Support:
├── mqtt → std (requires network stack)
├── kafka → std (requires complex networking)  
└── dds → std (requires discovery protocols)

Observability:
├── tracing → optional on both platforms
└── metrics → std only (requires aggregation)
```

### Error Propagation Patterns

**Async Context Preservation**
Errors must propagate efficiently through async call stacks while maintaining tracing context:

```rust
// Error spans are automatically linked in async chains
async fn database_operation() -> DbResult<()> {
    let conn = establish_connection().await?;  // NetworkTimeout propagates
    let data = serialize_request(&request)?;   // SerializationError propagates  
    conn.send(data).await?;                   // Io error propagates
    Ok(())
}
```

**Cross-Module Error Boundaries**  
Different AimDB modules (core, connectors, adapters) use the unified `DbError` type to prevent error conversion overhead:

```rust
// Clean error flow across module boundaries
impl AsyncDatabase {
    async fn store(&self, data: &[u8]) -> DbResult<()> {
        self.adapter.runtime_call(|| {        // RuntimeAdapter error
            self.connector.send(data)         // Connector error  
        }).await                              // All use DbError
    }
}
```

### Memory Safety & Performance Guarantees

**Embedded Platform Optimizations**
- Error enum uses `repr(u8)` for minimal memory footprint
- Error codes are const-evaluable for compile-time optimization
- No dynamic string allocation in error construction  
- Stack-based error propagation only

**Standard Platform Extensions**
- Rich error messages with heap-allocated context
- Error chaining through `source()` implementation
- Integration with existing error handling ecosystems
- Structured error data for observability platforms

---

## 📊 Success Metrics & Validation

### Performance Targets

| Platform | Error Construction | Memory Usage | Compilation |
|----------|-------------------|--------------|-------------|
| MCU (no_std) | <1μs | 0 heap allocs | <5s |
| Edge (std) | <10μs | <1KB per error | <10s |
| Cloud (std) | <50μs | Unlimited | <15s |

### Validation Criteria

**Functional Validation:**
- [ ] All feature combinations compile successfully
- [ ] Error conversions work correctly per platform
- [ ] Display formatting works without allocations
- [ ] Error codes are stable and documented

**Performance Validation:**  
- [ ] Benchmark error construction overhead
- [ ] Verify zero heap allocations on embedded
- [ ] Test error message formatting speed
- [ ] Validate error size constraints

**Integration Validation:**
- [ ] Error types integrate with tracing spans
- [ ] Platform-specific error extensions work
- [ ] Feature flag validation catches conflicts
- [ ] Documentation examples compile and run

---

## 🔄 Architectural Decision Record

### Core Design Decisions

**Decision: Unified Error Type vs Platform-Specific Types**
- **Choice**: Single `DbError` enum with conditional fields
- **Rationale**: 
  - Eliminates error conversion overhead at module boundaries
  - Provides consistent API surface for all AimDB operations
  - Enables zero-cost platform specialization through compilation
  - Simplifies error handling patterns for library users
- **Trade-offs**: More complex internal implementation vs simpler external API

**Decision: Error Codes vs String Messages on Embedded**  
- **Choice**: Numeric error codes with const evaluation on `no_std`
- **Rationale**:
  - Enables structured logging without heap allocation
  - Provides deterministic memory usage patterns
  - Supports real-time error handling with predictable overhead
  - Maps to hardware diagnostic patterns familiar to embedded developers
- **Trade-offs**: Less human-readable vs better performance characteristics

**Decision: Feature Flag Hierarchy Design**
- **Choice**: Platform-first feature organization (std/embedded) with runtime/protocol extensions
- **Rationale**:
  - Prevents impossible combinations (embedded + std features)  
  - Makes compilation targets explicit and CI-validatable
  - Aligns with Rust ecosystem patterns for cross-platform libraries
  - Supports incremental feature adoption
- **Trade-offs**: Some feature combinations require multiple flags vs more granular control

**Decision: Conditional Trait Implementation Strategy**
- **Choice**: `thiserror` for std, manual implementations for embedded
- **Rationale**:
  - Leverages ecosystem tools where available (std)
  - Maintains zero-dependency constraint on embedded
  - Provides equivalent functionality across platforms
  - Enables platform-specific optimizations  
- **Trade-offs**: Duplicated trait implementations vs platform-optimal behavior

### Integration Architecture Decisions

**Decision: Error Context Handling Strategy**
- **Choice**: Rich context chaining on std, structured codes on embedded
- **Rationale**:
  - Maximizes debugging capability where resources permit (std)
  - Maintains real-time guarantees where required (embedded)
  - Provides structured observability data on both platforms
  - Enables gradual migration between deployment environments
- **Trade-offs**: Platform-specific behavior vs unified error handling patterns

**Decision: Async Error Propagation Design**
- **Choice**: Native `Result` propagation with tracing span integration
- **Rationale**:
  - Leverages Rust's built-in error handling patterns
  - Maintains zero-cost error propagation through `?` operator  
  - Integrates with async runtime tracing infrastructure
  - Preserves causal error relationships across await boundaries
- **Trade-offs**: Requires tracing integration complexity vs provides rich observability

### Risk Assessment & Mitigation

**Architectural Risk: Feature Flag Complexity**
- **Risk**: Feature combinations create exponential testing matrix
- **Mitigation**: Hierarchical feature design with CI validation matrix
- **Monitoring**: Automated feature combination testing in CI/CD

**Performance Risk: Error Handling Overhead**  
- **Risk**: Error construction impacts real-time performance guarantees
- **Mitigation**: Performance budgets with compile-time size verification
- **Monitoring**: Continuous performance regression testing

**Maintenance Risk: Platform Divergence**
- **Risk**: Platform-specific implementations drift apart over time  
- **Mitigation**: Shared error code namespace with behavioral consistency tests
- **Monitoring**: Cross-platform integration testing in CI matrix

---

## 🎯 Design Outcomes & Foundation

### Architectural Foundation Established

This error handling design creates the **reliability substrate** for AimDB's cross-platform async database:

**Unified Abstraction Layer**
- Single error type works seamlessly from MCU to cloud
- Platform-optimal behavior without compromising API consistency  
- Zero-cost abstractions preserve performance across all deployment targets
- Extensible architecture supports future platform additions

**Observability Integration**  
- Structured error data enables metrics collection and alerting
- Tracing integration provides causal error relationship tracking
- Platform-appropriate error reporting (codes vs messages)
- Foundation for automated error analysis and debugging

**Development Velocity Enhancement**
- Consistent error handling patterns across all AimDB modules
- Feature flag system prevents invalid platform/capability combinations
- CI validation ensures cross-platform compatibility maintenance
- Clear error taxonomy supports effective debugging and monitoring

### Enabling Future Architecture

This foundation directly enables subsequent AimDB milestones:

- **Core Traits (M2)**: All async database operations return `DbResult<T>`
- **Runtime Adapters**: Platform-specific error handling integrated seamlessly  
- **Protocol Connectors**: Network and serialization errors handled uniformly
- **Observability Systems**: Rich error telemetry from embedded to cloud

The error handling system establishes the **reliability contract** that makes AimDB's ambitious cross-platform async database architecture feasible and maintainable.
