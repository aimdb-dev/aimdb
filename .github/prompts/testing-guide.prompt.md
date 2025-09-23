---
mode: agent
description: "Create comprehensive test suites for AimDB components covering unit, integration, performance, and platform-specific testing"
---

# AimDB Testing Strategy Implementation

## Task Definition

Create comprehensive tests for a specified AimDB component with the following requirements:

### Requirements
- **Test Types**: Implement multiple test categories (unit/integration/performance/property-based)
- **Platform Targets**: Cover all deployment environments (embedded/edge/cloud/all)
- **Async Patterns**: Test async behavior (streams/channels/timeouts/cancellation)
- **Error Scenarios**: Validate error handling (network failures/resource exhaustion/invalid input)

### Constraints
- Follow AimDB's async-first testing approach using `tokio-test`
- Maintain `no_std` compatibility for embedded test targets
- Target <50ms latency validation in performance tests
- Use property-based testing for data validation scenarios
- Implement proper test isolation and cleanup
- Include benchmarks for performance-critical components

### Success Criteria
- [ ] Comprehensive unit tests with >90% code coverage
- [ ] Integration tests with mock external dependencies
- [ ] Performance tests validating latency requirements (<50ms)
- [ ] Property-based tests for data integrity
- [ ] Platform-specific tests for embedded/edge/cloud environments
- [ ] Error path testing for all failure scenarios
- [ ] Async operation testing (timeouts, cancellation, concurrency)
- [ ] Memory usage validation and leak detection
- [ ] Benchmark comparisons for performance regressions
- [ ] Test documentation and examples

## Implementation Guidelines

### Test Structure Organization
```rust
#[cfg(test)]
mod tests {
    mod unit_tests;
    mod integration_tests;
    mod performance_tests;
    mod property_tests;
    
    #[cfg(feature = "embedded")]
    mod embedded_tests;
}
```

### Unit Test Requirements
- Test component creation and basic operations
- Validate error handling and recovery
- Test timeout and cancellation scenarios
- Use `tokio-test` utilities for async testing
- Include test fixtures and helper functions

### Integration Test Requirements
- Test end-to-end component interactions
- Mock external dependencies (network, databases, etc.)
- Test concurrent processing scenarios
- Validate network failure recovery
- Use testcontainers for service dependencies

### Performance Test Requirements
- Benchmark processing throughput
- Measure operation latency
- Test memory usage and detect leaks
- Validate performance requirements (<50ms)
- Use `criterion` for statistical analysis

### Property-Based Test Requirements
- Test data serialization/deserialization roundtrips
- Validate configuration parameter ranges
- Test invariant preservation during processing
- Use `proptest` for generating test data

### Platform-Specific Test Requirements
- **Embedded**: Test memory constraints and interrupt safety
- **Edge**: Test resource limitations and performance
- **Cloud**: Test scalability and distributed scenarios

## Output Format

Provide the test implementation in the following structure:
1. Test module organization and setup
2. Unit tests with comprehensive coverage
3. Integration tests with mocked dependencies
4. Performance tests and benchmarks
5. Property-based tests for data validation
6. Platform-specific test implementations
7. Test documentation and usage examples