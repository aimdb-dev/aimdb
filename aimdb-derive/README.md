# aimdb-derive

Derive macros for AimDB record key types.

## Overview

This crate provides the `#[derive(RecordKey)]` macro for defining compile-time
checked record keys in AimDB. This is especially useful for embedded systems
where typos in string keys would cause runtime failures.

## Usage

```rust
use aimdb_derive::RecordKey;

#[derive(RecordKey, Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum AppKey {
    #[key = "temp.indoor"]
    TempIndoor,
    #[key = "temp.outdoor"]
    TempOutdoor,
    #[key = "light.main"]
    LightMain,
}

// Now use in AimDB:
let producer = db.producer::<Temperature>(AppKey::TempIndoor);
```

## Attributes

### `#[key = "..."]` (required on variants)

Specifies the string key for a variant. This is the value returned by `as_str()`.

### `#[key_prefix = "..."]` (optional on enum)

Prepends a prefix to all variant keys:

```rust
#[derive(RecordKey, Clone, Copy, PartialEq, Eq, Hash)]
#[key_prefix = "sensors."]
pub enum SensorKey {
    #[key = "temp"]   // → "sensors.temp"
    Temp,
    #[key = "humid"]  // → "sensors.humid"
    Humidity,
}
```

## Generated Code

The macro generates:

1. `impl RecordKey for YourEnum` with `as_str()` method
2. `impl Borrow<str> for YourEnum` for O(1) HashMap lookups

## no_std Support

This crate is fully `no_std` compatible. The generated code uses only
`core::borrow::Borrow`, not `std::borrow::Borrow`.

## License

Apache-2.0
