# Rolling firmware upgrade (062)

`contracts_n` is the deployed firmware's `no_std` contracts revision:
`temperature@1 { temp }`. `contracts_n1` preserves that exact locked shape
and adds `temperature@2 { celsius, quality }`, a migration, and a second
record name. Both payloads have no in-band schema version.
`contracts_changed` represents an independently built peer whose
`temperature@2` has a conflicting shape. Each crate has its own committed
lock and runs its generated lock test.

The tests live in
`aimdb-mqtt-connector/tests/rolling_upgrade/mod.rs`, included by the backend
parity binary. A small forwarding broker relays opaque payloads over real
TCP between the MCU's `mountain-mqtt` backend and the hub's `rumqttc`
backend. It implements exact-topic QoS 0 forwarding and acknowledges QoS 1
client publications; it is a test harness, not a production broker.
Tests wait for subscriptions before producing, and a watchdog bounds each run.

```sh
cargo test -p rolling-contracts-n -p rolling-contracts-n1 -p rolling-contracts-changed
cargo test -p aimdb-mqtt-connector --features _test-backend-parity --test backend_parity
cargo check -p embedded-check-fixture --target thumbv7em-none-eabihf
make bench-gate
```

The main cases cover a firmware producer upgraded at the hub, local ingress
stamping, exactly one `temp.indoor` metadata entry declaring v2, per-route
upgrade counters, an independent v2 outdoor sensor, and a downgraded hub
response decoded by the original firmware crate. Both Postcard and CDR run
through actual producers and consumers. Further MQTT cases reject an old
version on a current-only route, a future version, the conflicting shape,
a different contract, and an inline frame on a bound Postcard route. Each
rejected message is followed by a valid one on the same route. Bare v1
Postcard and legacy JSON carrying an ignored `schema_version` also upgrade.
The embedded fixture monomorphizes the shared link declaration functions on
Cortex-M; the host test runs the actual embedded MQTT stack over TokioNet.
Typed record keys (063's key API and its V6 compile-fail case) remain deferred.
Existing compile-fail tests enforce migration reachability and binding identity.

`make test` runs the three lock tests and MQTT parity suite; `make clippy`
lints the fixtures; `make test-embedded` builds their declarations on the MCU.
`make bench-gate` checks allocation counts with inline envelopes and verifies
one decoder call per migration. A contracts lock test must pass for every
firmware/hub build: the wire shape sent is the committed hash, not a hash
computed at runtime from the compiled struct.

## Envelope code size

Measured with rustc 1.98.0, release defaults (`opt-level=3`, no LTO),
`thumbv7em-none-eabihf`, `aimdb-core --no-default-features --features alloc`:

| Symbol | Text bytes |
|---|---:|
| `Envelope::parse` | 492 |
| `HeaderWriter::finish` | 316 |
| `Reader::varint` | 180 |
| Parser/writer subtotal | 988 |
| Envelope error formatters (two symbols) | 88 |
| All emitted envelope symbols | 1,076 |

These are unlinked symbol sizes, excluding alignment, shared library routines,
and downstream generic code. They are not a firmware's total size increase.
Reproduce the measurement with:

```sh
cargo build -p aimdb-core --target thumbv7em-none-eabihf --release --no-default-features --features alloc
LLVM_BIN="$(rustc --print sysroot)/lib/rustlib/$(rustc -vV | sed -n 's/^host: //p')/bin"
"$LLVM_BIN/llvm-nm" --print-size --size-sort --radix=d --demangle --defined-only \
  target/thumbv7em-none-eabihf/release/libaimdb_core.rlib | rg 'aimdb_core::envelope::'
```

## Malformed frames

The core test suite runs a deterministic 100,000-frame malformed-input test
(`arbitrary_frames_do_not_panic` in `aimdb-core/src/envelope.rs`). It checks
that parsing never panics, that a parsed payload lies inside its frame, and
that the parsed identity survives canonical header re-encoding. It needs no
extra toolchain, so normal CI exercises the parser.
