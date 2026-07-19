# Remote representation Cortex-M footprint fixture

This isolated `no_std` crate links two otherwise equivalent Cortex-M images:

- `remote-repr-json`: `T -> JSON bytes -> serde_json::Value`;
- `remote-repr-cbor`: `T -> CBOR bytes -> ciborium::Value`.

It is deliberately outside the AimDB workspace, so the experimental CBOR
dependency cannot enter a production package or the workspace lockfile.

Run from this directory:

```bash
cargo build --release --target thumbv7em-none-eabihf --features json --bin remote-repr-json
cargo build --release --target thumbv7em-none-eabihf --features cbor --bin remote-repr-cbor
rust-size -A target/thumbv7em-none-eabihf/release/remote-repr-json
rust-size -A target/thumbv7em-none-eabihf/release/remote-repr-cbor
```

These images measure linked serializer plus dynamic-value encode/decode code
for one representative nested record. Their 32 KiB heap region is an equal
static `.bss` control, not a claim about the peak heap required by either
decoder. Stack and peak heap still require on-target instrumentation.
