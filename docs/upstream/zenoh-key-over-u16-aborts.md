# A key over 65,535 bytes aborts the process

- **Upstream:** not filed
- **Affects:** zenoh-transport 1.10.1
- **Status:** guarded in AimDB

## The bug

The wire format bounds a key-expression suffix to `u16`
(`zenoh-protocol/src/core/wire_expr.rs`, `ExprLen = u16`), but `put` accepts
a longer key. On the fragmentation path zenoh-transport unwraps the encode
error (`src/common/pipeline.rs`, line 400 in 1.10.1:
`codec.write(&mut writer, msg).unwrap()`) while holding the stage-in lock. The
unwinding drops the `Session`, whose `disable` locks the poisoned pipeline
again (line 946). That panics inside a destructor during unwinding, and Rust
aborts the process. `catch_unwind` cannot recover from it.

Measured: a 65,535-byte key is delivered; a 65,536-byte key gets SIGABRT.

## Why it matters to AimDB

A topic writer's capacity is a `usize`, so a value's content can produce such
a key, and a fixed `zenoh://` key that long would pass `build()`. The native
backend checks the length itself (`KEY_MAX` in `src/native.rs`): a longer fixed
key or inbound filter fails the build, and a longer written key is skipped.
The guard can go once zenoh returns an error for an over-long key.
