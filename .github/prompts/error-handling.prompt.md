---
mode: agent
description: "Implement comprehensive error handling for AimDB components with platform-specific considerations"
---

# AimDB Error Handling Implementation

## Task Definition

Implement comprehensive error handling for a specified AimDB component with the following requirements:

### Requirements
- **Error Types**: Define appropriate error types for the component (network/parsing/validation/timeout/resource_exhaustion)
- **Recovery Strategies**: Implement recovery mechanisms (retry/fallback/circuit_breaker/graceful_degradation)
- **Platform Considerations**: Handle differences between embedded/edge/cloud environments
- **Observability**: Include logging, metrics, and tracing for error scenarios

### Constraints
- Follow AimDB's async-first design principles
- Use `thiserror` for error definitions and `anyhow` for error contexts
- Maintain `no_std` compatibility for embedded targets
- Target <50ms reactivity even during error conditions
- Minimize allocations in error paths for embedded environments

### Success Criteria
- [ ] Comprehensive error type definitions using `thiserror`
- [ ] Proper error chaining and context with `anyhow`
- [ ] Retry logic with exponential backoff and jitter
- [ ] Circuit breaker implementation for network operations
- [ ] Graceful degradation strategies with fallback options
- [ ] Structured logging with appropriate log levels
- [ ] Error metrics collection for monitoring
- [ ] Platform-specific error handling (embedded vs edge vs cloud)
- [ ] Comprehensive unit tests for error scenarios
- [ ] Documentation of error handling strategies

## Implementation Guidelines

### Error Type Structure
```rust
use thiserror::Error;

#[derive(Error, Debug)]
pub enum ComponentError {
    #[error("Configuration error: {message}")]
    Config { message: String },
    
    #[error("Network operation failed")]
    Network(#[from] NetworkError),
    
    #[error("Timeout after {duration:?}")]
    Timeout { duration: std::time::Duration },
    
    #[error("Resource exhausted: {resource}")]
    ResourceExhausted { resource: String },
    
    #[error("Protocol error: {protocol} - {message}")]
    Protocol { protocol: String, message: String },
}
```

### Recovery Strategy Implementation
- Implement retry with exponential backoff for transient failures
- Use circuit breaker pattern for external service calls
- Provide graceful degradation with fallback mechanisms
- Include timeout handling for all async operations

### Platform-Specific Considerations
- **Embedded**: Use heapless data structures, minimal error types
- **Edge**: Balance between functionality and resource constraints
- **Cloud**: Full error handling with comprehensive observability

### Observability Requirements
- Use `tracing` for structured logging
- Implement error metrics collection
- Include error context in all error paths
- Provide clear error messages for debugging

## Output Format

Provide the implementation in the following structure:
1. Error type definitions
2. Recovery strategy implementations
3. Component integration code
4. Unit tests for error scenarios
5. Documentation of error handling approach