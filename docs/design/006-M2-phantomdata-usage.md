# PhantomData Usage in AimDB

**Status**: Implemented  
**Date**: 2025-10-28  
**Author**: AimDB Team

## Overview

This document explains when and why `PhantomData<T>` is used in AimDB's type system, and when it can be safely omitted. Understanding these patterns helps maintain type safety while avoiding unnecessary complexity.

## Background: What is PhantomData?

`PhantomData<T>` is a zero-sized marker type in Rust that tells the compiler "this struct logically owns/uses type `T`, even though `T` doesn't appear in any actual fields."

### When PhantomData is NEEDED

PhantomData is required when:
1. A generic type parameter is used in methods but NOT stored in fields
2. You need to establish ownership/variance properties for unused type parameters
3. The type parameter affects the struct's behavior but isn't part of its data

### When PhantomData is NOT NEEDED

PhantomData is redundant when:
1. The type parameter is already stored in a field
2. The type parameter appears in `Arc<T>`, `Box<T>`, or any other wrapper in a field

## AimDB PhantomData Audit Results

### ✅ PhantomData NEEDED

#### 1. Producer<T, R> and Consumer<T, R>

**Location**: `aimdb-core/src/typed_api.rs`

```rust
pub struct Producer<T, R: aimdb_executor::Spawn + 'static> {
    db: Arc<AimDb<R>>,
    _phantom: PhantomData<T>,  // ✅ NEEDED
}

pub struct Consumer<T, R: aimdb_executor::Spawn + 'static> {
    db: Arc<AimDb<R>>,
    _phantom: PhantomData<T>,  // ✅ NEEDED
}
```

**Reason**: 
- Type parameter `T` (the record type) is NOT stored in any field
- Type parameter `R` is stored in `AimDb<R>`, but `T` only appears in method signatures
- PhantomData ties the record type `T` to the struct at compile time
- Prevents accidentally mixing producers/consumers of different types

**Example of what PhantomData prevents**:
```rust
// Without PhantomData<T>, this would compile (BAD):
let temp_producer: Producer<Temperature, Runtime> = ...;
let pressure_producer: Producer<Pressure, Runtime> = temp_producer; // Should be error!

// With PhantomData<T>, the types are distinct and this is correctly rejected
```

#### 2. AimDbBuilder<R>

**Location**: `aimdb-core/src/builder.rs`

```rust
pub struct AimDbBuilder<R = NoRuntime> {
    records: BTreeMap<TypeId, Box<dyn AnyRecord>>,
    runtime: Option<Arc<R>>,
    connectors: BTreeMap<String, Arc<dyn crate::transport::Connector>>,
    spawn_fns: BTreeMap<TypeId, Box<dyn core::any::Any + Send>>,
    _phantom: PhantomData<R>,  // ✅ NEEDED
}
```

**Reason**:
- When `R = NoRuntime`, the `runtime` field is `None`
- The type parameter `R` distinguishes typed builder from untyped builder
- PhantomData ensures proper variance and type state tracking
- Critical for builder pattern state machine (untyped → typed transition)

**State Machine**:
```rust
// Untyped state
let builder: AimDbBuilder<NoRuntime> = AimDbBuilder::new();

// Transition to typed state
let builder: AimDbBuilder<TokioAdapter> = builder.runtime(adapter);
```

Without PhantomData, the type state wouldn't be properly tracked.

### ❌ PhantomData NOT NEEDED (Examples)

These examples show when PhantomData is redundant and should be avoided:

#### Example 1: Type Parameter in Arc<T>

```rust
// ❌ WRONG: Redundant PhantomData
pub struct Container<R: SomeTrait> {
    runtime: Arc<R>,           // R is stored here!
    _phantom: PhantomData<R>,  // Unnecessary - R is already in runtime field
}

// ✅ CORRECT: No PhantomData needed
pub struct Container<R: SomeTrait> {
    runtime: Arc<R>,  // R is already stored, PhantomData adds nothing
}
```

**Reason**: Type parameter `R` is already stored in the `runtime` field via `Arc<R>`, so PhantomData adds no value.

#### Example 2: Type Parameter in Multiple Fields

```rust
// ❌ WRONG: Redundant PhantomData
pub struct Wrapper<A: Trait> {
    adapter: A,               // A is stored here!
    container: Box<A>,        // A is also here!
    _phantom: PhantomData<A>, // Completely redundant
}

// ✅ CORRECT: No PhantomData needed
pub struct Wrapper<A: Trait> {
    adapter: A,        // A appears in actual fields
    container: Box<A>, // Compiler knows this struct uses A
}
```

**Reason**: Type parameter `A` appears in multiple fields, making PhantomData completely unnecessary.

### ⚠️ Special Cases (Kept for other reasons)

#### RecordSpawner<T>

**Location**: `aimdb-core/src/typed_record.rs`

```rust
pub struct RecordSpawner<T> {
    _phantom: core::marker::PhantomData<T>,  // ⚠️ KEPT
}
```

**Reason**: 
- Helper struct for spawning tasks for a specific record type
- Type parameter `T` appears only in method signatures
- PhantomData ensures type safety in spawn operations

## Decision Matrix

Use this matrix to decide if PhantomData is needed:

| Scenario | PhantomData Needed? | Example |
|----------|-------------------|---------|
| Type parameter stored in field directly | ❌ NO | `struct Foo<T> { value: T }` |
| Type parameter in `Arc<T>` field | ❌ NO | `struct Foo<T> { value: Arc<T> }` |
| Type parameter in `Box<T>` field | ❌ NO | `struct Foo<T> { value: Box<T> }` |
| Type parameter in `Option<Arc<T>>` field | ❌ NO (usually) | `struct Foo<T> { value: Option<Arc<T>> }` |
| Type parameter only in methods | ✅ YES | `struct Foo<T> { count: usize }` (T only in methods) |
| Type parameter for state machine types | ✅ YES | `struct Builder<State> { ... }` where State is a marker |
| Multiple type params, some not in fields | ✅ YES (for unused ones) | `struct Foo<T, U> { value: T }` (need PhantomData<U>) |

## Code Size Impact

PhantomData is a zero-sized type:
- **Runtime cost**: Zero (optimized away by compiler)
- **Compile-time cost**: Minimal (just type checking)
- **Benefit**: Clear type safety when used correctly
- **Drawback when misused**: Unnecessary cognitive overhead

## Guidelines for Future Development

### Adding New Generic Structs

1. **First**: Define the struct with actual field types
2. **Check**: Are all type parameters used in fields?
3. **Add PhantomData**: Only for type parameters NOT in fields
4. **Document**: Explain why PhantomData is needed

### Example: Adding a New Type

```rust
// ✅ GOOD: PhantomData only for unused type parameter
struct MyStruct<T, R: Runtime> {
    runtime: Arc<R>,          // R is in a field
    _phantom: PhantomData<T>, // T is NOT in a field - needs PhantomData
}

// ❌ BAD: Redundant PhantomData
struct MyStruct<T, R: Runtime> {
    runtime: Arc<R>,           // R is in a field
    _phantom_r: PhantomData<R>, // Redundant!
    _phantom_t: PhantomData<T>, // This one IS needed
}
```

## Testing

PhantomData correctness is verified by:
1. **Compilation**: Wrong usage won't compile
2. **Type safety tests**: Try to mix incompatible types
3. **Zero-cost abstraction**: Check assembly output (PhantomData has no runtime cost)

## References

- [Rust Book: PhantomData](https://doc.rust-lang.org/std/marker/struct.PhantomData.html)
- [Rustonomicon: PhantomData](https://doc.rust-lang.org/nomicon/phantom-data.html)
- AimDB codebase: `aimdb-core/src/typed_api.rs` for correct usage examples
