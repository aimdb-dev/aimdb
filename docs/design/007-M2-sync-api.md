# Synchronous API Design for AimDB

**Status**: Draft  
**Author**: AimDB Team  
**Date**: October 29, 2025  
**Issue**: [#35](https://github.com/aimdb-dev/aimdb/issues/35)

---

## Table of Contents

1. [Executive Summary](#executive-summary)
2. [Motivation](#motivation)
3. [Design Goals](#design-goals)
4. [Architecture Overview](#architecture-overview)
5. [API Surface](#api-surface)
6. [Threading Model](#threading-model)
7. [Channel Protocol](#channel-protocol)
8. [Type Safety](#type-safety)
9. [Error Handling](#error-handling)
10. [Resource Management](#resource-management)
11. [Performance Analysis](#performance-analysis)
12. [Thread Safety](#thread-safety)
13. [Alternatives Considered](#alternatives-considered)
14. [Future Extensions](#future-extensions)
15. [Implementation Roadmap](#implementation-roadmap)
16. [Success Criteria](#success-criteria)
17. [References](#references)

---

## Executive Summary

This document describes the design of a **synchronous API wrapper** for AimDB (`aimdb-sync` crate) that enables integration with synchronous codebases, FFI interfaces, and applications where async overhead is undesirable. The design uses a **dedicated runtime thread** running the Tokio async runtime, with blocking channels (`tokio::sync::mpsc`) for bidirectional communication between the main thread and the runtime thread.

**Key Design Decisions:**
- ✅ Use `tokio::sync::mpsc` channels for sync/async bridge
- ✅ Maintain compile-time type safety with `SyncProducer<T>` and `SyncConsumer<T>`
- ✅ Use `set()` and `get()` method naming for producer/consumer operations
- ✅ Tokio-only (no Embassy support)
- ✅ Configure buffers through existing async API before `attach()`
- ✅ Explicit timeout error variants in `DbError`

---

## Motivation

### Use Cases

AimDB's current async-only API presents integration challenges for:

1. **Legacy Codebases**: Synchronous applications that cannot be easily converted to async
2. **FFI Boundaries**: C/C++/Python bindings that expect blocking APIs
3. **Simple Scripts**: Tools where async overhead is unnecessary
4. **Threaded Architectures**: Applications already using thread pools or similar patterns

### User Need

Users need a way to:
- Run AimDB in a background thread
- Interact with the database using **blocking function calls** from synchronous code
- Manage lifecycle (startup/shutdown) cleanly
- Achieve performance close to native async (<1ms overhead)

---

## Design Goals

### Functional Goals

1. **Ergonomic Sync API**: Feel natural to Rust developers familiar with blocking I/O
2. **Type Safety**: Preserve compile-time type checking across thread boundaries
3. **Clean Lifecycle**: Explicit attach/detach with proper resource cleanup
4. **Timeout Support**: Configurable timeouts for all blocking operations
5. **Error Transparency**: Clear error propagation from async to sync contexts

### Non-Functional Goals

1. **Performance**: <1ms added latency vs pure async
2. **Safety**: No data races, proper shutdown, no resource leaks
3. **Simplicity**: Easy to understand threading model
4. **Compatibility**: Works with existing AimDB configuration patterns

### Non-Goals (Out of Scope)

- ❌ Embassy runtime support (Tokio-only initially)
- ❌ Custom buffer semantics for sync API (use existing buffers)
- ❌ Advanced zero-copy techniques like shared memory or ring buffers (Rust's move semantics already provide zero-copy)
- ❌ Multi-threaded runtime pool (single dedicated runtime thread is sufficient - but user code can use unlimited threads!)

**Note on Threading:** The design uses one dedicated thread for the Tokio runtime, but this does **NOT** limit concurrency! Any number of user threads can concurrently call `set()`/`get()` on cloned producers/consumers. The runtime thread efficiently multiplexes all async operations internally via Tokio's work-stealing scheduler.

---

## Architecture Overview

### High-Level Architecture

```
┌─────────────────────────────────────────────────────────────────┐
│                         Main Thread (Sync)                      │
│                                                                 │
│  ┌──────────────┐         ┌──────────────┐                    │
│  │ SyncProducer │         │ SyncConsumer │                    │
│  │     <T>      │         │     <T>      │                    │
│  └──────┬───────┘         └──────┬───────┘                    │
│         │                         │                            │
│         │ set(value)              │ get()                      │
│         ▼                         ▼                            │
│  ┌─────────────────────────────────────────┐                  │
│  │     tokio::sync::mpsc channels          │                  │
│  │  (blocking_send / blocking_recv)        │                  │
│  └─────────────┬───────────────┬───────────┘                  │
└────────────────┼───────────────┼──────────────────────────────┘
                 │               │
         ════════╪═══════════════╪════════════ Thread Boundary
                 │               │
┌────────────────▼───────────────▼──────────────────────────────┐
│                    Runtime Thread (Async)                      │
│                                                                 │
│  ┌───────────────────────────────────────────────────────┐    │
│  │              Tokio Runtime                            │    │
│  │  ┌─────────────────────────────────────────────┐    │    │
│  │  │          AimDB Instance                     │    │    │
│  │  │  ┌──────────────┐    ┌──────────────┐     │    │    │
│  │  │  │  Producer<T> │    │  Consumer<T> │     │    │    │
│  │  │  └──────────────┘    └──────────────┘     │    │    │
│  │  │                                             │    │    │
│  │  │  ┌──────────────────────────────────┐     │    │    │
│  │  │  │     Record Buffers (SPMC/etc)   │     │    │    │
│  │  │  └──────────────────────────────────┘     │    │    │
│  │  └─────────────────────────────────────────────┘    │    │
│  │                                                       │    │
│  │  ┌─────────────────────────────────────────────┐    │    │
│  │  │        Message Pump Loop                    │    │    │
│  │  │  - Receives set commands from channels     │    │    │
│  │  │  - Forwards to async producers             │    │    │
│  │  │  - Polls async consumers                    │    │    │
│  │  │  - Sends data back via channels            │    │    │
│  │  └─────────────────────────────────────────────┘    │    │
│  └───────────────────────────────────────────────────────┘    │
│                                                                 │
└─────────────────────────────────────────────────────────────────┘
```

### Component Responsibilities

| Component | Responsibility |
|-----------|---------------|
| **AimDbHandle** | Manages runtime thread lifecycle, spawns thread, coordinates shutdown |
| **SyncProducer<T>** | Blocking wrapper for async producer, owns channel sender |
| **SyncConsumer<T>** | Blocking wrapper for async consumer, owns channel receiver |
| **Runtime Thread** | Runs Tokio runtime, executes async operations, pumps messages |
| **Message Pump** | Bridges sync channels to async producers/consumers |
| **tokio::sync::mpsc** | Bidirectional channels with blocking send/recv support |

---

## API Surface

### Core Types

```rust
/// Handle to the AimDB runtime thread.
/// 
/// Created by calling `AimDb::attach()`. Provides factory methods
/// for creating typed producers and consumers.
pub struct AimDbHandle {
    // Internal implementation hidden
}

/// Synchronous producer for records of type `T`.
/// 
/// Thread-safe, can be cloned and shared across threads.
pub struct SyncProducer<T: TypedRecord> {
    // Internal implementation hidden
}

/// Synchronous consumer for records of type `T`.
/// 
/// Thread-safe, can be cloned and shared across threads.
pub struct SyncConsumer<T: TypedRecord> {
    // Internal implementation hidden
}
```

### AimDbHandle Methods

```rust
impl AimDb {
    /// Attach the database to a background runtime thread.
    ///
    /// Spawns a new thread running a Tokio runtime, where all async
    /// operations will be executed. The database must be fully configured
    /// before calling this method.
    ///
    /// # Returns
    ///
    /// `AimDbHandle` that can be used to create producers and consumers.
    ///
    /// # Errors
    ///
    /// - `DbError::AttachFailed` if the runtime thread fails to start
    ///
    /// # Example
    ///
    /// ```rust
    /// let db = AimDbBuilder::new()
    ///     .runtime(TokioAdapter::new()?)
    ///     .configure::<Temperature>(|reg| {
    ///         reg.buffer(BufferCfg::SpmcRing { capacity: 100 });
    ///     })
    ///     .build()?;
    ///
    /// let handle = db.attach()?;
    /// ```
    pub fn attach(self) -> DbResult<AimDbHandle>;
}

impl AimDbHandle {
    /// Create a synchronous producer for type `T`.
    ///
    /// # Type Parameters
    ///
    /// - `T`: The record type, must implement `TypedRecord`
    ///
    /// # Errors
    ///
    /// - `DbError::RecordNotFound` if type `T` was not registered
    /// - `DbError::RuntimeShutdown` if the runtime thread has stopped
    ///
    /// # Example
    ///
    /// ```rust
    /// let producer = handle.producer::<Temperature>()?;
    /// producer.set(Temperature { celsius: 25.0 })?;
    /// ```
    pub fn producer<T: TypedRecord>(&self) -> DbResult<SyncProducer<T>>;

    /// Create a synchronous consumer for type `T`.
    ///
    /// # Type Parameters
    ///
    /// - `T`: The record type, must implement `TypedRecord`
    ///
    /// # Errors
    ///
    /// - `DbError::RecordNotFound` if type `T` was not registered
    /// - `DbError::RuntimeShutdown` if the runtime thread has stopped
    ///
    /// # Example
    ///
    /// ```rust
    /// let consumer = handle.consumer::<Temperature>()?;
    /// let temp = consumer.get()?;
    /// ```
    pub fn consumer<T: TypedRecord>(&self) -> DbResult<SyncConsumer<T>>;

    /// Gracefully shut down the runtime thread.
    ///
    /// Signals the runtime to stop, waits for all pending operations
    /// to complete, then joins the thread. This is the preferred way
    /// to shut down.
    ///
    /// # Errors
    ///
    /// - `DbError::DetachFailed` if shutdown fails or times out
    ///
    /// # Example
    ///
    /// ```rust
    /// handle.detach()?;
    /// ```
    pub fn detach(self) -> DbResult<()>;

    /// Gracefully shut down with a timeout.
    ///
    /// Like `detach()`, but fails if shutdown takes longer than
    /// the specified duration.
    ///
    /// # Arguments
    ///
    /// - `timeout`: Maximum time to wait for shutdown
    ///
    /// # Errors
    ///
    /// - `DbError::DetachFailed` if shutdown fails or times out
    ///
    /// # Example
    ///
    /// ```rust
    /// use std::time::Duration;
    /// handle.detach_timeout(Duration::from_secs(5))?;
    /// ```
    pub fn detach_timeout(self, timeout: Duration) -> DbResult<()>;
}

impl Drop for AimDbHandle {
    /// Attempts graceful shutdown if `detach()` was not called.
    ///
    /// Logs a warning and attempts shutdown with a 5-second timeout.
    /// If shutdown fails, the runtime thread may be left running.
    fn drop(&mut self);
}
```

### SyncProducer Methods

```rust
impl<T: TypedRecord> SyncProducer<T> {
    /// Set a value, blocking until space is available.
    ///
    /// Blocks indefinitely until the value can be sent to the runtime
    /// thread and produced into the database.
    ///
    /// # Arguments
    ///
    /// - `value`: The record to produce (ownership transferred)
    ///
    /// # Errors
    ///
    /// - `DbError::RuntimeShutdown` if the runtime thread has stopped
    /// - `DbError::ProduceFailed` if the async produce operation fails
    ///
    /// # Example
    ///
    /// ```rust
    /// producer.set(Temperature { celsius: 25.0 })?;
    /// ```
    pub fn set(&self, value: T) -> DbResult<()>;

    /// Set a value with a timeout.
    ///
    /// Blocks until the value is set or the timeout expires.
    ///
    /// # Arguments
    ///
    /// - `value`: The record to produce
    /// - `timeout`: Maximum time to wait
    ///
    /// # Errors
    ///
    /// - `DbError::SetTimeout` if the timeout expires
    /// - `DbError::RuntimeShutdown` if the runtime thread has stopped
    /// - `DbError::ProduceFailed` if the async produce operation fails
    ///
    /// # Example
    ///
    /// ```rust
    /// use std::time::Duration;
    /// producer.set_timeout(
    ///     Temperature { celsius: 25.0 },
    ///     Duration::from_millis(100)
    /// )?;
    /// ```
    pub fn set_timeout(&self, value: T, timeout: Duration) -> DbResult<()>;

    /// Try to set a value without blocking.
    ///
    /// Returns immediately, either succeeding or returning an error
    /// if the channel is full.
    ///
    /// # Arguments
    ///
    /// - `value`: The record to produce
    ///
    /// # Errors
    ///
    /// - `DbError::SetTimeout` if the channel is full (non-blocking)
    /// - `DbError::RuntimeShutdown` if the runtime thread has stopped
    ///
    /// # Example
    ///
    /// ```rust
    /// match producer.try_set(Temperature { celsius: 25.0 }) {
    ///     Ok(()) => println!("Value set"),
    ///     Err(DbError::SetTimeout) => println!("Channel full, try later"),
    ///     Err(e) => eprintln!("Error: {}", e),
    /// }
    /// ```
    pub fn try_set(&self, value: T) -> DbResult<()>;
}

impl<T: TypedRecord> Clone for SyncProducer<T> {
    /// Clone the producer to share across threads.
    ///
    /// Multiple clones can set values concurrently.
    fn clone(&self) -> Self;
}
```

### SyncConsumer Methods

```rust
impl<T: TypedRecord> SyncConsumer<T> {
    /// Get a value, blocking until one is available.
    ///
    /// Blocks indefinitely until a value is available from the
    /// runtime thread.
    ///
    /// # Returns
    ///
    /// The next available record of type `T`.
    ///
    /// # Errors
    ///
    /// - `DbError::RuntimeShutdown` if the runtime thread has stopped
    /// - `DbError::ConsumeFailed` if the async consume operation fails
    ///
    /// # Example
    ///
    /// ```rust
    /// let temp = consumer.get()?;
    /// println!("Temperature: {}°C", temp.celsius);
    /// ```
    pub fn get(&self) -> DbResult<T>;

    /// Get a value with a timeout.
    ///
    /// Blocks until a value is available or the timeout expires.
    ///
    /// # Arguments
    ///
    /// - `timeout`: Maximum time to wait
    ///
    /// # Errors
    ///
    /// - `DbError::GetTimeout` if the timeout expires
    /// - `DbError::RuntimeShutdown` if the runtime thread has stopped
    /// - `DbError::ConsumeFailed` if the async consume operation fails
    ///
    /// # Example
    ///
    /// ```rust
    /// use std::time::Duration;
    /// match consumer.get_timeout(Duration::from_millis(100)) {
    ///     Ok(temp) => println!("Temperature: {}°C", temp.celsius),
    ///     Err(DbError::GetTimeout) => println!("No data available"),
    ///     Err(e) => eprintln!("Error: {}", e),
    /// }
    /// ```
    pub fn get_timeout(&self, timeout: Duration) -> DbResult<T>;

    /// Try to get a value without blocking.
    ///
    /// Returns immediately with either a value or an error if
    /// no data is available.
    ///
    /// # Errors
    ///
    /// - `DbError::GetTimeout` if no data is available (non-blocking)
    /// - `DbError::RuntimeShutdown` if the runtime thread has stopped
    ///
    /// # Example
    ///
    /// ```rust
    /// match consumer.try_get() {
    ///     Ok(temp) => println!("Temperature: {}°C", temp.celsius),
    ///     Err(DbError::GetTimeout) => println!("No data yet"),
    ///     Err(e) => eprintln!("Error: {}", e),
    /// }
    /// ```
    pub fn try_get(&self) -> DbResult<T>;
}

impl<T: TypedRecord> Clone for SyncConsumer<T> {
    /// Clone the consumer to share across threads.
    ///
    /// Multiple clones can get values concurrently, each receiving
    /// independent data according to buffer semantics (SPMC, etc.).
    fn clone(&self) -> Self;
}
```

---

## Threading Model

### Dedicated Runtime Thread

The sync API uses a **single dedicated thread** to run the Tokio async runtime:

```
┌─────────────────────────────────────────────────────────────┐
│                      AimDb::attach()                        │
└──────────────────────────┬──────────────────────────────────┘
                           │
                           │ spawn
                           ▼
              ┌────────────────────────┐
              │   Runtime Thread       │
              │                        │
              │ ┌────────────────────┐ │
              │ │  Tokio Runtime     │ │
              │ │  - Block on future │ │
              │ │  - Message pump    │ │
              │ └────────────────────┘ │
              │                        │
              │ Runs until shutdown    │
              └────────────────────────┘
                           │
                           │ join
                           ▼
              ┌────────────────────────┐
              │   AimDbHandle::detach()│
              └────────────────────────┘
```

### Thread Lifecycle

1. **Startup (attach)**:
   - Main thread calls `db.attach()`
   - Spawn new thread with `std::thread::spawn()`
   - Create Tokio runtime with `Runtime::new()`
   - Initialize AimDB instance on the runtime
   - Set up channels for all registered types
   - Return `AimDbHandle` to main thread
   - Runtime thread enters message pump loop

2. **Active Phase**:
   - Runtime thread blocks on `tokio::select!` across:
     - All producer command channels (set operations)
     - All consumer data channels (polling async consumers)
     - Shutdown signal channel
   - Process commands and forward to async API
   - Send results back to caller threads

3. **Shutdown (detach)**:
   - Main thread calls `handle.detach()`
   - Send shutdown signal via channel
   - Runtime thread exits message pump
   - Wait for all pending operations to complete
   - Drop AimDB instance (cleanup resources)
   - Exit runtime thread
   - Main thread joins the thread

### Rationale

**Why Dedicated Thread?**
- ✅ Clean separation of sync/async concerns
- ✅ Predictable lifecycle and resource management
- ✅ Simple mental model (one runtime, one thread)
- ✅ No need for complex thread pool coordination
- ✅ Easy to reason about shutdown behavior

**Why Not Thread Pool?**
- ❌ Overkill for this use case (single runtime sufficient)
- ❌ Added complexity in lifecycle management
- ❌ Harder to ensure clean shutdown
- ❌ More potential for subtle bugs

### Concurrency Characteristics

**User-Side Concurrency** (Unlimited ✅):
- Any number of threads can call `producer.set()` concurrently
- Any number of threads can call `consumer.get()` concurrently
- Channels handle synchronization (lock-free MPSC)
- Each thread blocks independently on its operation

**Runtime-Side Concurrency** (Handled by Tokio ✅):
- Tokio's work-stealing scheduler multiplexes async tasks
- Internally concurrent even on single OS thread
- Thousands of async operations can be in-flight
- No need for manual thread management

**Example - Many Threads, One Runtime:**
```rust
let handle = db.attach()?;  // One runtime thread
let producer = handle.producer::<Data>()?;

// Spawn 100 user threads - all work fine!
for i in 0..100 {
    let p = producer.clone();
    std::thread::spawn(move || {
        loop {
            p.set(Data { id: i })?;  // Concurrent, thread-safe
        }
    });
}
// All threads send through channels to single runtime thread
// Runtime thread efficiently processes all requests
```

**Scalability Limits:**
- **User threads**: Unlimited (OS limited, not API limited)
- **Runtime thread**: Could become bottleneck at extreme scale (>100k ops/sec)
- **Future optimization**: Use Tokio's multi_threaded runtime if needed

---

## Channel Protocol

### Channel Architecture

Each registered type `T` has two channels:

```
Main Thread                Runtime Thread
───────────                ───────────────

SyncProducer<T>            Async Producer<T>
     │                            ▲
     │  ProducerCmd<T>           │
     └────────────────────────────┘
         Command Channel
      (bounded, capacity N)


SyncConsumer<T>            Async Consumer<T>
     ▲                            │
     │  ConsumerData<T>          │
     └────────────────────────────┘
         Data Channel
      (bounded, capacity N)
```

### Message Types

```rust
/// Command to produce a value of type T
enum ProducerCmd<T> {
    /// Set a value, with oneshot channel for result
    Set {
        value: T,
        result_tx: tokio::sync::oneshot::Sender<DbResult<()>>,
    },
    /// Shutdown this producer
    Shutdown,
}

/// Data from consuming type T
enum ConsumerData<T> {
    /// A value was consumed
    Value(T),
    /// An error occurred during consume
    Error(DbError),
}
```

### Channel Configuration

```rust
// Producer command channel (main → runtime)
let (cmd_tx, cmd_rx) = tokio::sync::mpsc::channel::<ProducerCmd<T>>(capacity);

// Consumer data channel (runtime → main)
let (data_tx, data_rx) = tokio::sync::mpsc::channel::<ConsumerData<T>>(capacity);
```

**Channel Capacity:**
- Default: 100 messages per type
- Configurable via builder pattern (future extension)
- Provides backpressure when full

### Message Flow - Producer

```
Main Thread                         Runtime Thread
───────────                         ───────────────

producer.set(value)
    │
    ├─ Create oneshot channel for result
    │
    ├─ Send ProducerCmd::Set { value, result_tx }
    │  via blocking_send()
    │      │
    │      ├──────────────────────────▶ Receive cmd from channel
    │                                   │
    │                                   ├─ Call async producer.produce(value).await
    │                                   │
    │                                   ├─ Send result back via result_tx
    │      ◀──────────────────────────┤
    │
    ├─ Receive result via blocking_recv()
    │
    └─ Return DbResult<()>
```

### Message Flow - Consumer

```
Runtime Thread                      Main Thread
───────────────                     ───────────

[Background loop polling]
    │
    ├─ Call async consumer.consume().await
    │
    ├─ Got value or error
    │
    ├─ Send ConsumerData::Value(v) or ConsumerData::Error(e)
    │  via try_send()
    │      │
    │      ├──────────────────────────▶ consumer.get()
    │                                   │
    │                                   ├─ blocking_recv() on channel
    │                                   │
    │                                   └─ Return value or error
```

### Blocking Behavior

Using `tokio::sync::mpsc` with blocking operations:

```rust
// In main thread (sync context)
impl<T> SyncProducer<T> {
    pub fn set(&self, value: T) -> DbResult<()> {
        let (result_tx, result_rx) = tokio::sync::oneshot::channel();
        let cmd = ProducerCmd::Set { value, result_tx };
        
        // Block until channel has space
        self.cmd_tx.blocking_send(cmd)
            .map_err(|_| DbError::RuntimeShutdown)?;
        
        // Block until result is available
        result_rx.blocking_recv()
            .map_err(|_| DbError::RuntimeShutdown)?
    }
}

impl<T> SyncConsumer<T> {
    pub fn get(&self) -> DbResult<T> {
        // Block until data is available
        match self.data_rx.blocking_recv() {
            Some(ConsumerData::Value(v)) => Ok(v),
            Some(ConsumerData::Error(e)) => Err(e),
            None => Err(DbError::RuntimeShutdown),
        }
    }
}
```

### Timeout Implementation

```rust
impl<T> SyncProducer<T> {
    pub fn set_timeout(&self, value: T, timeout: Duration) -> DbResult<()> {
        use std::time::Instant;
        
        let deadline = Instant::now() + timeout;
        let (result_tx, result_rx) = tokio::sync::oneshot::channel();
        let cmd = ProducerCmd::Set { value, result_tx };
        
        // Try to send with timeout
        self.cmd_tx.blocking_send_timeout(cmd, timeout)
            .map_err(|_| DbError::SetTimeout)?;
        
        // Calculate remaining time
        let remaining = deadline.saturating_duration_since(Instant::now());
        
        // Wait for result with remaining timeout
        result_rx.blocking_recv_timeout(remaining)
            .map_err(|_| DbError::SetTimeout)?
    }
}
```

---

## Type Safety

### Compile-Time Type Preservation

The sync API maintains full type safety using generics:

```rust
// Type information flows through the system
AimDb::configure::<Temperature>()  // Register at build time
    ↓
AimDbHandle::producer::<Temperature>()  // Request specific type
    ↓
SyncProducer<Temperature>  // Strongly typed producer
    ↓
set(Temperature { ... })  // Only accepts Temperature
    ↓
[Channel carries Temperature]
    ↓
async Producer<Temperature>  // Async API receives correct type
```

### Type Registry

Internally, `AimDbHandle` maintains a registry mapping `TypeId` to channel endpoints:

```rust
struct TypeRegistry {
    producers: HashMap<TypeId, Box<dyn Any>>,
    consumers: HashMap<TypeId, Box<dyn Any>>,
}

impl AimDbHandle {
    pub fn producer<T: TypedRecord>(&self) -> DbResult<SyncProducer<T>> {
        let type_id = TypeId::of::<T>();
        
        // Look up channel endpoint for this type
        let boxed = self.registry.producers
            .get(&type_id)
            .ok_or(DbError::RecordNotFound)?;
        
        // Downcast to concrete type (guaranteed safe by construction)
        let sender = boxed.downcast_ref::<Sender<ProducerCmd<T>>>()
            .expect("Type registry corrupted");
        
        Ok(SyncProducer {
            cmd_tx: sender.clone(),
            _phantom: PhantomData,
        })
    }
}
```

### Safety Guarantees

1. **No Type Confusion**: Each `SyncProducer<T>` can only send `T`
2. **No Runtime Type Checks**: Type checking happens at compile time
3. **No Unsafe Downcasting**: Downcast is guaranteed safe by registry construction
4. **Channel Type Safety**: Channels are typed with `ProducerCmd<T>` / `ConsumerData<T>`

---

## Error Handling

### New Error Variants

Extend `DbError` in `aimdb-core/src/error.rs`:

```rust
#[derive(Debug, thiserror::Error)]
pub enum DbError {
    // ... existing variants ...
    
    /// Failed to attach database to runtime thread
    #[error("Failed to attach database: {message}")]
    AttachFailed { message: String },
    
    /// Failed to detach database from runtime thread
    #[error("Failed to detach database: {message}")]
    DetachFailed { message: String },
    
    /// Timeout while setting a value
    #[error("Timeout while setting value")]
    SetTimeout,
    
    /// Timeout while getting a value
    #[error("Timeout while getting value")]
    GetTimeout,
    
    /// Runtime thread has shut down
    #[error("Runtime thread has shut down")]
    RuntimeShutdown,
}
```

### Error Propagation Paths

```
Async Context (Runtime Thread)          Sync Context (Main Thread)
───────────────────────────────          ──────────────────────────

producer.produce(value).await
    │
    ├─ Ok(()) ──────────────────────────▶ result_tx.send(Ok(()))
    │                                     │
    │                                     └─▶ set() returns Ok(())
    │
    └─ Err(DbError::BufferFull) ────────▶ result_tx.send(Err(...))
                                          │
                                          └─▶ set() returns Err(DbError::BufferFull)


Timeout in sync API:
    blocking_send_timeout()
        │
        └─ Timeout expired ──────────────▶ set_timeout() returns Err(DbError::SetTimeout)


Runtime shutdown:
    blocking_send()
        │
        └─ Channel closed ───────────────▶ set() returns Err(DbError::RuntimeShutdown)
```

### Error Handling Best Practices

**For Users:**
```rust
// Pattern 1: Panic on fatal errors
let handle = db.attach().expect("Failed to start runtime");

// Pattern 2: Graceful degradation
match producer.set_timeout(value, Duration::from_millis(100)) {
    Ok(()) => { /* success */ }
    Err(DbError::SetTimeout) => { /* retry or skip */ }
    Err(e) => panic!("Fatal error: {}", e),
}

// Pattern 3: Non-blocking with fallback
if let Err(DbError::SetTimeout) = producer.try_set(value) {
    // Queue for later or use default
}
```

**For Implementation:**
```rust
// Always convert channel errors to DbError
self.cmd_tx.blocking_send(cmd)
    .map_err(|_| DbError::RuntimeShutdown)?;

// Use explicit timeout errors
if elapsed > timeout {
    return Err(DbError::SetTimeout);
}

// Provide context in attach/detach errors
Err(DbError::AttachFailed {
    message: format!("Thread spawn failed: {}", e)
})
```

---

## Resource Management

### Lifecycle States

```
┌──────────┐  attach()   ┌──────────┐  detach()   ┌──────────┐
│ Detached │────────────▶│ Attached │────────────▶│ Shutdown │
│          │             │          │             │          │
│  No      │             │  Runtime │             │  Joined  │
│  Thread  │             │  Running │             │  Thread  │
└──────────┘             └──────────┘             └──────────┘
                               │
                               │ Drop without detach()
                               ▼
                         ┌──────────┐
                         │ Forced   │
                         │ Shutdown │
                         └──────────┘
```

### Owned Resources

**AimDbHandle owns:**
- `JoinHandle<()>` for the runtime thread
- Shutdown signal channel sender
- Type registry with all producer/consumer channels

**Runtime Thread owns:**
- Tokio `Runtime` instance
- `AimDb` instance
- All async `Producer<T>` and `Consumer<T>` instances
- Channel receivers for commands
- Channel senders for data

### Clean Shutdown Protocol

```rust
impl AimDbHandle {
    pub fn detach(mut self) -> DbResult<()> {
        // 1. Signal shutdown
        self.shutdown_tx.send(ShutdownSignal)?;
        
        // 2. Close all producer channels (no new commands accepted)
        self.registry.close_producers();
        
        // 3. Wait for runtime thread to finish message pump
        let thread_handle = self.thread_handle.take().unwrap();
        thread_handle.join()
            .map_err(|_| DbError::DetachFailed {
                message: "Runtime thread panicked".to_string()
            })?;
        
        // 4. Resources automatically dropped
        Ok(())
    }
}
```

### Drop Implementation

```rust
impl Drop for AimDbHandle {
    fn drop(&mut self) {
        if let Some(handle) = self.thread_handle.take() {
            // User didn't call detach() explicitly
            eprintln!("Warning: AimDbHandle dropped without calling detach()");
            eprintln!("Attempting emergency shutdown with 5 second timeout");
            
            // Try to signal shutdown
            let _ = self.shutdown_tx.try_send(ShutdownSignal);
            
            // Give runtime thread 5 seconds to clean up
            let timeout_handle = std::thread::spawn(move || {
                std::thread::sleep(Duration::from_secs(5));
            });
            
            // Wait for either thread to finish
            if handle.join().is_err() {
                eprintln!("Error: Runtime thread did not shut down cleanly");
            }
        }
    }
}
```

### Resource Leak Prevention

**Mechanisms:**
1. **RAII**: All resources owned by handles with Drop implementations
2. **Channel Closure**: Channels signal shutdown when senders dropped
3. **Thread Join**: Guaranteed thread cleanup via join in `detach()`
4. **Timeout Shutdown**: Drop implementation prevents indefinite wait
5. **Lint Warnings**: Document that `detach()` must be called

**Testing:**
- Memory leak tests using `valgrind` or similar
- Resource handle count checks before/after attach/detach
- Long-running stress tests (24+ hours)

---

## Performance Analysis

### Overhead Sources

| Source | Estimated Impact | Mitigation |
|--------|------------------|------------|
| Channel crossing | 50-200μs | Use bounded channels, tune capacity |
| Thread context switch | 1-10μs | Minimize switches, batch operations |
| Oneshot result channel | 20-50μs | Necessary for error propagation |
| Lock contention | 1-50μs | tokio channels are lock-free |

**Note**: No cloning/serialization overhead for data values! Rust's move semantics ensure **zero-copy** - values are moved (not cloned) through channels.

**Total Estimated Overhead**: 100-500μs per operation

**Target**: <1ms (1000μs) - **Achievable** ✅

### Performance Optimization Strategies

1. **Channel Capacity Tuning**:
   ```rust
   // Larger capacity = fewer blocks, more memory
   let capacity = 100;  // Default
   let capacity = 1000; // High throughput
   ```

2. **Batching** (future optimization):
   ```rust
   producer.set_batch(vec![value1, value2, value3])?;
   // One channel crossing for multiple values
   ```

3. **Zero-Copy Where Possible**:
   ```rust
   // Take ownership, no clone needed
   pub fn set(&self, value: T) -> DbResult<()>
   ```

4. **Lock-Free Channels**:
   - `tokio::sync::mpsc` is lock-free
   - No mutex contention

### Benchmarking Plan

**Metrics to Measure:**
- Latency: p50, p95, p99 for `set()` and `get()`
- Throughput: operations/second
- CPU overhead: % usage vs pure async
- Memory overhead: bytes per operation

**Benchmark Scenarios:**
1. **Single Producer/Consumer**: Baseline latency
2. **Multiple Producers**: Contention handling
3. **High Throughput**: Sustained load test
4. **Mixed Workload**: Realistic usage pattern

**Comparison Baseline:**
```rust
// Async baseline
let start = Instant::now();
producer.produce(value).await?;
let latency = start.elapsed();

// Sync API
let start = Instant::now();
sync_producer.set(value)?;
let sync_latency = start.elapsed();

// Target: sync_latency < async_latency + 1ms
```

---

## Thread Safety

### Concurrent Access Patterns

All sync API types are **thread-safe** and can be shared:

```rust
impl<T> SyncProducer<T> {
    // Safe to call from multiple threads
    pub fn set(&self, value: T) -> DbResult<()>
}

// SyncProducer can be cloned and shared
let producer2 = producer.clone();
std::thread::spawn(move || {
    producer2.set(value)?;
});
```

### Interior Mutability

```rust
pub struct SyncProducer<T> {
    // Sender has interior mutability (Arc<Mutex<...>> internally)
    cmd_tx: tokio::sync::mpsc::Sender<ProducerCmd<T>>,
    _phantom: PhantomData<T>,
}

// Auto traits:
// - Send: ✅ Can be sent between threads
// - Sync: ✅ Can be shared between threads via &self
unsafe impl<T: Send> Send for SyncProducer<T> {}
unsafe impl<T: Send> Sync for SyncProducer<T> {}
```

### Synchronization Mechanisms

| Component | Synchronization | Rationale |
|-----------|----------------|-----------|
| `tokio::sync::mpsc` | Internal locks | Concurrent send/recv |
| `Arc` | Atomic reference counting | Safe cloning |
| `oneshot::channel` | Single-use, no locking | Result propagation |
| Type registry | `RwLock` or immutable after init | Rare writes (only at attach) |

### Data Race Prevention

**No Data Races Because:**
1. Channels provide happens-before guarantees
2. Each message is owned (moved) through channels
3. No shared mutable state between threads
4. Tokio runtime handles async task scheduling

**Miri Testing:**
```bash
cargo +nightly miri test --features sync
# Verify no undefined behavior in concurrent scenarios
```

---

## Alternatives Considered

### Alternative 1: Thread Pool

**Approach**: Use a thread pool instead of dedicated thread.

**Pros**:
- More efficient resource usage
- Better scalability for many operations

**Cons**:
- Complex lifecycle management
- Harder to reason about shutdown
- Overkill for typical use cases
- Runtime startup cost per task

**Decision**: ❌ Rejected - Unnecessary complexity

---

### Alternative 2: Inline Async Runtime

**Approach**: No background thread, run Tokio runtime on each call.

```rust
pub fn set(&self, value: T) -> DbResult<()> {
    let runtime = Runtime::new()?;
    runtime.block_on(self.async_producer.produce(value))
}
```

**Pros**:
- No threading complexity
- Simpler lifecycle

**Cons**:
- Runtime startup overhead (~1ms) per call
- Can't share runtime between calls
- Defeats purpose of persistent async runtime
- Poor performance

**Decision**: ❌ Rejected - Unacceptable performance

---

### Alternative 3: Callback-Based API

**Approach**: Register callbacks instead of blocking.

```rust
producer.set_async(value, |result| {
    match result {
        Ok(()) => println!("Success"),
        Err(e) => eprintln!("Error: {}", e),
    }
});
```

**Pros**:
- Non-blocking
- Familiar pattern from other languages

**Cons**:
- Not idiomatic Rust
- Callback hell potential
- Lifetime management issues
- Users want blocking API specifically

**Decision**: ❌ Rejected - Not the requested API style

---

### Alternative 4: crossbeam-channel

**Approach**: Use `crossbeam-channel` instead of `tokio::sync::mpsc`.

**Pros**:
- Pure sync, no Tokio dependency (but we already have Tokio)
- Highly optimized
- Familiar API

**Cons**:
- Another dependency
- Need async adapter on runtime thread
- `tokio::sync` integrates better with async runtime
- `tokio::sync` has `blocking_send()` built-in

**Decision**: ❌ Rejected - `tokio::sync` is more suitable

---

## Future Extensions

### Extension 1: Batch Operations

**API:**
```rust
impl<T> SyncProducer<T> {
    pub fn set_batch(&self, values: Vec<T>) -> DbResult<Vec<DbResult<()>>>;
}

impl<T> SyncConsumer<T> {
    pub fn get_batch(&self, count: usize) -> DbResult<Vec<T>>;
}
```

**Benefits**: Amortize channel crossing overhead over multiple operations.

---

### Extension 2: Configurable Channel Capacity

**API:**
```rust
impl AimDb {
    pub fn attach_with_config(self, config: SyncConfig) -> DbResult<AimDbHandle>;
}

pub struct SyncConfig {
    pub channel_capacity: usize,
    pub shutdown_timeout: Duration,
}
```

**Benefits**: Tune for specific workload characteristics.

---

### Extension 3: Embassy Runtime Support

**Design Challenge**: Embassy is `no_std` and designed for embedded.

**Possible Approach**:
- Sync API only for `std` environments (Linux/RTOS with threads)
- Use Embassy async runtime on background thread
- Different channel implementation (not tokio::sync)

**Decision**: Defer until clear use case emerges.

---

### Extension 4: Streaming API

**API:**
```rust
impl<T> SyncConsumer<T> {
    pub fn iter(&self) -> impl Iterator<Item = DbResult<T>>;
}

// Usage:
for result in consumer.iter() {
    let value = result?;
    // process value
}
```

**Benefits**: Idiomatic Rust iterator pattern for continuous consumption.

---

### Extension 5: Async-Compatible Sync API

**API:**
```rust
impl<T> SyncProducer<T> {
    pub async fn set_async(&self, value: T) -> DbResult<()>;
}
```

**Benefits**: Use sync API types from async contexts without blocking.

**Rationale**: Useful for FFI where you have sync handle but call from async.

---

## Implementation Roadmap

### Phase 1: Core Foundation (Week 1-2)

**Deliverables**:
- [ ] Create `aimdb-sync` crate structure
- [ ] Implement `AimDbHandle` with attach/detach
- [ ] Implement basic channel protocol
- [ ] Add error variants to `DbError`

**Tests**:
- [ ] Attach/detach lifecycle
- [ ] Thread spawn and join
- [ ] Error propagation

---

### Phase 2: Producer/Consumer (Week 2-3)

**Deliverables**:
- [ ] Implement `SyncProducer<T>` with `set()`, `set_timeout()`, `try_set()`
- [ ] Implement `SyncConsumer<T>` with `get()`, `get_timeout()`, `try_get()`
- [ ] Type registry with `TypeId` mapping
- [ ] Clone implementations

**Tests**:
- [ ] Set/get correctness
- [ ] Timeout behavior
- [ ] Non-blocking operations
- [ ] Type safety verification

---

### Phase 3: Resource Management (Week 3-4)

**Deliverables**:
- [ ] Graceful shutdown protocol
- [ ] Drop implementation with warnings
- [ ] Resource cleanup verification
- [ ] Thread-safe cloning

**Tests**:
- [ ] Clean shutdown
- [ ] Drop behavior
- [ ] Resource leak detection
- [ ] Multi-threaded access

---

### Phase 4: Examples & Documentation (Week 4-5)

**Deliverables**:
- [ ] `examples/sync-api-demo` - Simple usage
- [ ] `examples/sync-ffi-binding` - C FFI example
- [ ] API documentation with examples
- [ ] Architecture diagram

**Tests**:
- [ ] Example compilation and execution
- [ ] FFI safety verification

---

### Phase 5: Performance & Polish (Week 5-6)

**Deliverables**:
- [ ] Benchmarks vs async baseline
- [ ] Performance tuning
- [ ] Stress tests
- [ ] Final documentation review

**Tests**:
- [ ] Latency benchmarks
- [ ] Throughput tests
- [ ] Long-running stability tests
- [ ] Memory leak tests

---

## Success Criteria

### Functional Requirements

- [x] Design document complete and approved
- [ ] Clean, ergonomic sync API implemented
- [ ] All three operation modes work: blocking, timeout, try
- [ ] Type safety maintained at compile time
- [ ] Graceful attach/detach lifecycle
- [ ] Proper error propagation

### Non-Functional Requirements

- [ ] <1ms overhead vs async in benchmarks
- [ ] No memory leaks in 24-hour stress test
- [ ] No data races in Miri tests
- [ ] Thread-safe concurrent access
- [ ] Clean shutdown in <1 second

### Documentation Requirements

- [ ] Comprehensive API docs with examples
- [ ] Architecture diagram
- [ ] User quickstart guide
- [ ] FFI binding example
- [ ] Performance comparison data

### Testing Requirements

- [ ] >90% code coverage
- [ ] All unit tests passing
- [ ] Integration tests for concurrent scenarios
- [ ] Stress tests under load
- [ ] Examples compile and run

---

## References

### Similar Projects

- **tokio::task::spawn_blocking**: Inspiration for sync/async bridge
- **tokio::sync::mpsc**: Channel implementation we're using
- **crossbeam-channel**: Alternative channel implementation
- **parking_lot**: High-performance synchronization (not needed here)

### Tokio Documentation

- [Bridging with sync code](https://tokio.rs/tokio/topics/bridging)
- [Channels](https://tokio.rs/tokio/tutorial/channels)
- [Spawning](https://tokio.rs/tokio/tutorial/spawning)

### Design Patterns

- **Thread-per-Core**: Not applicable (we use dedicated thread)
- **Message Passing**: Core pattern for sync/async bridge
- **RAII**: Resource management via Drop
- **Type-Safe Builder**: AimDbBuilder pattern

### AimDB Internal References

- `aimdb-core/src/typed_record.rs` - TypedRecord trait
- `aimdb-core/src/error.rs` - DbError enum
- `aimdb-tokio-adapter/` - Async Tokio adapter
- `docs/design/architecture.md` - Overall system architecture

---

## Appendix: Complete Example

```rust
use aimdb_core::{AimDbBuilder, TypedRecord};
use aimdb_tokio_adapter::TokioAdapter;
use aimdb_sync::{AimDbHandle, SyncProducer, SyncConsumer};
use serde::{Serialize, Deserialize};
use std::time::Duration;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Temperature {
    celsius: f32,
    timestamp: u64,
}

impl TypedRecord for Temperature {
    fn type_name() -> &'static str {
        "Temperature"
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 1. Build and configure database (async API)
    let db = AimDbBuilder::new()
        .runtime(TokioAdapter::new()?)
        .configure::<Temperature>(|reg| {
            reg.buffer(BufferCfg::SpmcRing { capacity: 100 });
        })
        .build()?;
    
    // 2. Attach to background thread (sync API starts here)
    let handle = db.attach()?;
    
    // 3. Create producer and consumer
    let producer = handle.producer::<Temperature>()?;
    let consumer = handle.consumer::<Temperature>()?;
    
    // 4. Use from main thread (blocking)
    producer.set(Temperature {
        celsius: 25.0,
        timestamp: 1234567890,
    })?;
    
    let temp = consumer.get()?;
    println!("Temperature: {}°C", temp.celsius);
    
    // 5. Use from other threads
    let producer2 = producer.clone();
    let handle_thread = std::thread::spawn(move || {
        for i in 0..10 {
            let result = producer2.set_timeout(
                Temperature {
                    celsius: 20.0 + i as f32,
                    timestamp: 1234567890 + i,
                },
                Duration::from_millis(100),
            );
            
            match result {
                Ok(()) => println!("Set temperature {}", i),
                Err(e) => eprintln!("Error: {}", e),
            }
        }
    });
    
    handle_thread.join().unwrap();
    
    // 6. Clean shutdown
    handle.detach()?;
    
    Ok(())
}
```

---

**END OF DESIGN DOCUMENT**
