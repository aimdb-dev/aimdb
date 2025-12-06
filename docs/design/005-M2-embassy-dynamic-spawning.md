# Embassy Dynamic Task Spawning

**Status**: ✅ Implemented  

## Problem Statement

Embassy's executor requires all tasks to be statically defined with `#[embassy_executor::task]`. This creates a challenge for AimDB, which needs to dynamically spawn producer and consumer services registered via `.source()` and `.tap()` methods.

## Solution: Generic Task Pool

We implement a pre-defined task pool using Embassy's `pool_size` parameter to enable dynamic spawning while respecting Embassy's static task requirements.

### Implementation

```rust
// Type-erased future for dynamic spawning
type BoxedFuture = Box<dyn Future<Output = ()> + Send + 'static>;

// Generic task runner with configurable pool size
#[embassy_executor::task(pool_size = 8)]
async fn generic_task_runner(mut future: BoxedFuture) {
    use core::pin::Pin;
    let pinned = unsafe { Pin::new_unchecked(&mut *future) };
    pinned.await;
}

// Spawn trait implementation
impl Spawn for EmbassyAdapter {
    fn spawn<F>(&self, future: F) -> ExecutorResult<()>
    where
        F: Future<Output = ()> + Send + 'static,
    {
        // Box the future for type erasure
        let boxed_future: BoxedFuture = Box::new(future);
        
        // Get spawn token from the task pool
        match generic_task_runner(boxed_future) {
            Ok(spawn_token) => {
                self.spawner.spawn(spawn_token);
                Ok(())
            }
            Err(_) => Err(ExecutorError::SpawnFailed {
                message: "Task pool exhausted",
            }),
        }
    }
}
```

## How It Works

1. **Type Erasure**: Any `Future<Output = ()> + Send + 'static` is boxed into a `BoxedFuture`
2. **Static Pool**: The `#[embassy_executor::task(pool_size = 8)]` macro creates 8 task slots at compile time
3. **Dynamic Assignment**: Each `spawn()` call allocates a slot from the pool
4. **Pin Safety**: The boxed future is pinned before awaiting

## Memory Layout

```
Static Memory (Compile Time):
┌─────────────────────────────────────┐
│ generic_task_runner Pool            │
│  ┌─────────┐ ┌─────────┐           │
│  │ Slot 0  │ │ Slot 1  │ ... x8    │
│  └─────────┘ └─────────┘           │
└─────────────────────────────────────┘

Heap Memory (Runtime):
┌─────────────────────────────────────┐
│ BoxedFuture instances               │
│  ┌────────────┐ ┌────────────┐     │
│  │ Producer 1 │ │ Consumer 1 │ ... │
│  └────────────┘ └────────────┘     │
└─────────────────────────────────────┘
```

## Benefits

### ✅ API Consistency
- Same `.source()` and `.tap()` API as Tokio
- No special macros or annotations required
- Write-once, run-anywhere code

### ✅ Type Safety
- Compile-time verification of task pool
- No runtime task allocation errors
- Clear error messages when pool is exhausted

### ✅ Performance
- Zero overhead when pool is not full
- Compile-time task structure allocation
- Only heap allocation is for the boxed future content

### ✅ Flexibility
- Configurable pool size via `pool_size` parameter
- Can adjust based on application needs
- Balance between flexibility and memory usage

## Pool Size Configuration

### Default Configuration (8 tasks)
```rust
#[embassy_executor::task(pool_size = 8)]
async fn generic_task_runner(mut future: BoxedFuture) {
    // ...
}
```

### Increased Pool (16 tasks)
```rust
#[embassy_executor::task(pool_size = 16)]
async fn generic_task_runner(mut future: BoxedFuture) {
    // ...
}
```

### Memory Impact

Each task slot costs approximately:
- **32-64 bytes** for the task structure (architecture dependent)
- **Plus** the size of the boxed future (dynamic)

Example: 8 slots ≈ 256-512 bytes static + dynamic future sizes

## Error Handling

When the pool is exhausted:

```rust
Err(ExecutorError::SpawnFailed {
    message: "Task pool exhausted - increase pool_size in generic_task_runner",
})
```

**Solutions**:
1. Increase `pool_size` parameter
2. Reduce number of concurrent `.source()` and `.tap()` registrations
3. Share services across multiple record types

## Comparison with Tokio

### Tokio (Dynamic)
```rust
impl Spawn for TokioAdapter {
    fn spawn<F>(&self, future: F) -> ExecutorResult<()> {
        tokio::spawn(future);  // Fully dynamic, no limits
        Ok(())
    }
}
```

### Embassy (Pool-based)
```rust
impl Spawn for EmbassyAdapter {
    fn spawn<F>(&self, future: F) -> ExecutorResult<()> {
        let boxed = Box::new(future);
        let token = generic_task_runner(boxed)?;  // Pool slot
        self.spawner.spawn(token);
        Ok(())
    }
}
```

## Trade-offs

| Aspect | Tokio | Embassy |
|--------|-------|---------|
| Task Limit | Unlimited (until OOM) | Fixed pool size |
| Memory | Dynamic heap | Static + heap for futures |
| Overhead | Minimal | Type erasure boxing |
| Compile Time | Fast | Slightly slower (macro) |
| API | Identical | Identical ✅ |

## Future Improvements

### Potential Optimizations

1. **Tiered Pools**: Multiple pools for different priority levels
2. **Dynamic Pool Resizing**: Compile-time feature flags for different sizes
3. **Zero-Copy Futures**: Use `Pin<&'static mut Future>` for truly static futures
4. **Pool Monitoring**: Runtime statistics for pool utilization

### Embassy Evolution

If Embassy adds dynamic spawning in the future:
```rust
// Hypothetical future Embassy API
spawner.spawn_dynamic(future);  // No pool needed
```

Our abstraction layer would automatically benefit without API changes!

## Conclusion

The generic task pool approach successfully bridges the gap between Embassy's static task model and AimDB's need for dynamic service registration. This enables a **fully consistent API** across both Tokio and Embassy runtimes while respecting the constraints of each platform.

**Key Achievement**: Developers can use `.source()` and `.tap()` identically on both runtimes, with the framework handling the platform-specific details automatically.
