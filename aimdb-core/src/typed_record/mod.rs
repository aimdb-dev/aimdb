//! Type-safe record storage using TypeId
//!
//! Provides type-safe record identification using Rust's `TypeId` for compile-time safety.
//!
//! # Feature Support
//!
//! All layers below work on both `std` and `no_std + alloc`; they are gated by
//! capability feature, not by `std`:
//!
//! - **always**: Core API (`TypedRecord`, `latest()`, `RecordValue`, producer/consumer).
//! - **`remote`**: the JSON value codec — `.with_remote_access()` installs it
//!   and `record.latest()?.as_json()` reads it.
//! - **`remote`**: record metadata (`collect_metadata` → `RecordMetadata`,
//!   the `writable` flag) plus the type-erased JSON read/write/subscribe methods
//!   (`latest_json[_bytes]` / `set_from_json[_bytes]` / `subscribe_json`) used
//!   by the AimX server.
//!
//! Without `remote`, use `record.latest()` + `Deref` for value access and
//! implement custom serialization for embedded protocols (CBOR, MessagePack, etc.).

use alloc::boxed::Box;

mod any_record;
#[cfg(feature = "remote")]
mod events;
#[cfg(feature = "remote")]
mod json_adapters;
mod record;

pub use crate::typed_record::{
    any_record::{AnyRecord, AnyRecordExt},
    record::{RecordFutureCollector, RecordValue, TypedRecord},
};

#[cfg(feature = "remote")]
pub use crate::typed_record::json_adapters::JsonRecordAccess;

#[cfg(feature = "std")]
type Mutex<T> = std::sync::Mutex<T>;
#[cfg(not(feature = "std"))]
type Mutex<T> = spin::Mutex<T>;

/// Locks one of the `TypedRecord` field mutexes, hiding the std/spin API
/// difference (`std::sync::Mutex::lock` returns a `LockResult`; `spin` returns
/// the guard directly). A poisoned std mutex is unrecoverable in this code, so
/// `.unwrap()` is the correct response. The returned guard derefs to `T` on
/// both sides, so call sites are identical.
#[cfg(feature = "std")]
fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}
#[cfg(not(feature = "std"))]
fn lock<T>(m: &Mutex<T>) -> spin::MutexGuard<'_, T> {
    m.lock()
}

// Type alias for boxed futures
pub(crate) type BoxFuture<'a, T> =
    core::pin::Pin<Box<dyn core::future::Future<Output = T> + Send + 'a>>;

/// Type alias for consumer service closure stored in TypedRecord
/// Each consumer receives the [`RuntimeContext`](crate::RuntimeContext) and a
/// Consumer<T> handle for subscribing to the buffer
pub(in crate::typed_record) type ConsumerServiceFn<T> =
    Box<dyn FnOnce(crate::RuntimeContext, crate::Consumer<T>) -> BoxFuture<'static, ()> + Send>;

/// Type alias for producer service closure stored in TypedRecord
/// Takes (RuntimeContext, Producer<T>) and returns a Future
/// This will be auto-spawned during build().
/// `Send` only — `Sync` is not required because the closure is taken out of
/// `Mutex<Option<...>>` exactly once via `.take()`, and `Mutex<T>: Sync`
/// already holds when `T: Send`. Matches the consumer-side `ConsumerServiceFn`.
pub(in crate::typed_record) type ProducerServiceFn<T> =
    Box<dyn FnOnce(crate::RuntimeContext, crate::Producer<T>) -> BoxFuture<'static, ()> + Send>;

#[cfg(all(test, feature = "remote"))]
mod tests;
