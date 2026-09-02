# 052 — Verification against the codebase, and effort assessment

**Status:** review + prototype, 2026-09-02. Verifies
[052 — Runtime-neutral connectors](052-runtime-neutral-connectors.md) against
the tree at `58c1951`, on the pinned toolchain (rustc 1.98.0, `88d9e12ae`).
**Verdict:** the architecture is sound and the core mechanism is real — it
compile-checks exactly as claimed. Four factual errors and four open design
gaps were found; **all four gaps are now closed with working code** (§3), not
with argument, and every correction has been folded back into design 052
itself. Effort, recalibrated against the prototype: **4–6 weeks** for one
engineer already fluent in the crates.

Every claim below that says "verified" or "answered" was executed. The
prototype lives on `claude/design-doc-52-verify-1lsexa` and is **not** on
`main`: this document and the revised design are merged ahead of it, so the
findings are not stranded behind an unmerged change. It is ~2 300 lines across
seven crates — core's neutral I/O layer, both adapters, a unified KNX
connection task, the serial connector reduced to framing, and the tests that
settle each question. §7 lists what landed where.

---

## 1. What checks out

Every load-bearing claim in §2, §5 and §6 was checked against the source.
Line references are to `main` at `58c1951`; where the prototype later
changes a cited item, the change is called out in §3.

| Claim | Verified |
|---|---|
| §5.1 return-type notation is experimental on 1.98 | Yes — `error[E0658]: return type notation is experimental`, issue #109417 |
| §5.1 the trait-declared `+ Send` shape works instead | Yes — a standalone crate with a `!Send` socket, a `SendFutureWrapper` newtype impl, a plain `async fn` impl, a generic `FramedConnection`, and a generic task boxed as `Send + 'static` compiles clean under `rustup run 1.98.0` |
| §5.2 `embassy-sync` pulls in no executor | Yes — `_external/embassy/embassy-sync/Cargo.toml` deps are exactly `critical-section`, `heapless`, `futures-core`, `futures-sink`, `embedded-io-async`, `cfg-if` (+ optional `defmt`/`log`) |
| §5.2 KNX Embassy already uses `StaticCell<Channel<CriticalSectionRawMutex, …, N>>` | Yes — [`embassy_client.rs:76-107`](../../aimdb-knx-connector/src/embassy_client.rs) |
| §5.2 Tokio's `with_command_queue_size` becomes a const generic | Yes — it exists at [`tokio_client.rs:64`](../../aimdb-knx-connector/src/tokio_client.rs); the Embassy side already notes the size must be a constant |
| §5.5 `spin::Mutex` needs no new dependency | Yes — `aimdb-core/Cargo.toml:120` already declares `spin` with `mutex`/`spin_mutex` |
| §4 `RuntimeOps::sleep` boxes | Yes — `fn sleep(&self, d) -> BoxFuture` at [`executor.rs:50`](../../aimdb-core/src/executor.rs); `now_nanos` is a plain call |
| §2 KNX is ~90 % shared behind a three-method `TunnelIo` | Yes — `send`/`forward`/`warn_ack_timeout` at [`tunnel.rs:536`](../../aimdb-knx-connector/src/tunnel.rs); `drain_actions` is the shared driver |
| §2 both KNX halves bypass `RuntimeOps` for the clock | Yes — `embassy_time::Instant::now()` at `embassy_client.rs:281`, `tokio::time::Instant` at `tokio_client.rs:221` |
| §2 the serial Embassy half is already the target shape | Yes — it contributes only `CobsFramer` and rides `EmbassyConnection<Rd, Wr, F, RC, WC>`; the file carries **zero** `unsafe` |
| §5.4 `embassy-futures` is dependency-free | Yes — its `[dependencies]` is empty bar optional `defmt`/`log`, and `select_array` exists for the pooled case |
| §6 `embassy_tls.rs` already bypasses `run_with_subscriptions` | Yes — `handle_messages` at [`embassy_tls.rs:372`](../../aimdb-mqtt-connector/src/embassy_tls.rs); the plain path still uses `run_with_subscriptions` at `embassy_client.rs:575` |
| §6 `set_unix_time` has no neutral home | Yes — it is an inherent `EmbassyAdapter` method ([`runtime.rs:48`](../../aimdb-embassy-adapter/src/runtime.rs)), not on `RuntimeOps` |
| §7.2 hostname resolution is a genuine gain | Yes — the plain Embassy path rejects non-IPv4-literal hosts today ("plain mqtt:// needs an IPv4 literal") |
| §8 design 012 exists and needs a rewrite | Yes — `012-M5-connector-development-guide.md`, marked "✅ Implemented (Reference Documentation)" |
| Acceptance 1 baseline | Yes — `cargo check -p aimdb-core --no-default-features --features alloc,connector-session,remote --target thumbv7em-none-eabihf` passes today, and core's only `unsafe` is a word inside a doc comment (`transport.rs:165`) |

One structural point the design gets right without saying so: the Embassy TCP
half documents that "`connector-io` cannot be reused directly because
`TcpSocket::split()` only yields borrowed halves, while AimDB's `Connection`
must own the socket". An unsplit `ByteStream` with `&mut self` on both
directions dissolves that blocker, and it costs nothing, because
`Connection::recv`/`send` already take `&mut self` and so already serialize.

## 2. Factual errors

1. **§8: "No tool or library crate constructs these connectors."** False.
   [`aimdb-codegen/src/rust.rs`](../../aimdb-codegen/src/rust.rs) emits
   `MqttConnector::new(&mqtt_url)` (lines 268, 1421) and
   `KnxConnector::new(&knx_gateway)` (lines 269, 1424) into generated user
   code. MQTT is unaffected (the `Native` backend keeps that signature), but
   **KNX is not**: once `KnxConnector` is generic over `Datagram + Delay`, the
   generated line must carry a transport, so `aimdb-codegen` changes and
   `make codegen-drift` has to be re-baselined.
2. **§8's call-site list is incomplete.** It omits
   [`aimdb-tcp-connector/examples/tcp_demo.rs`](../../aimdb-tcp-connector/examples/tcp_demo.rs)
   and
   [`aimdb-serial-connector/examples/serial_demo.rs`](../../aimdb-serial-connector/examples/serial_demo.rs)
   — precisely the two crates whose sugar §8 says will change. It also lists
   all four `weather-mesh-demo` binaries when only `weather-station-gamma`
   (Embassy MQTT) is affected; hub/alpha/beta use the unchanged Tokio path.
3. **§2 cites `tokio_client.rs:691`.** That file is 632 lines. The clock is at
   `tokio_client.rs:221-222`. (`embassy_client.rs:283` is off by two — the fn
   is at 281.) `connectors.rs:75` in §5.1 should be `:74`.
4. **Acceptance 2: "Every `unsafe impl Send`/`Sync` in the workspace remains
   inside `aimdb-embassy-adapter`."** False today and after the change:
   `aimdb-wasm-adapter` carries 16 (`buffer.rs` 9, `ws_bridge.rs` 4,
   `time.rs` 3) and `aimdb-bench/src/alloc.rs` one. The criterion needs
   scoping to the connector crates, which is what it evidently means.

## 3. The four gaps, closed

Each was flagged as needing design work before step 1. Each is now answered by
code on the prototype branch, with a test that fails if the answer is wrong.

### 3.1 `StreamListener` **can** back the Embassy N-socket accept pool

The concern was that `accept(&mut self)` is single-shot while the pool exists
to keep `N` sockets in `accept()` at once, so routing it through core's serial
`serve` loop would drop concurrency to one — and that §6's "moves into the
adapter unchanged" therefore could not hold.

Reading `embassy-net` settles the mechanism. `TcpSocket::accept` is a
*synchronous* `s.listen(endpoint)` followed by a bare `poll_fn` that waits for
the state to leave `Listen`/`SynSent`/`SynReceived`. So **dropping the future
does not un-listen the socket** — but *re-entering* `accept()` on a listening
socket is `InvalidState`, and the `abort()` that makes it re-enterable is what
drops the `LISTEN`.

`EmbassyNet::listen::<N>` therefore stores one accept future per slot and
consumes only the one that completes; the other `N-1` stay pending, untouched,
still listening. Nothing is ever cancelled and no socket leaves `LISTEN`
between calls.

`aimdb-tcp-connector/tests/neutral_pool.rs` proves it over the same two
crossover-wired `embassy-net` stacks the existing loopback smoke uses, driven
in exactly the shape `serve` accepts in:

- `pool_keeps_every_slot_listening_between_accepts` — client A connects and
  round-trips bytes; **then**, while the server sits between accepts, client B
  dials and also connects and round-trips. A pool holding only one socket in
  `LISTEN` would have had B's SYN refused.
- `naive_pool_loses_the_syn_that_arrives_between_accepts` — the same scenario
  against a pool that re-creates its accepts each call. B's dial fails with
  `TransportError::Io`, asserted. This is what makes the first test a finding
  rather than a coincidence, and it is a regression guard: if embassy-net's
  cancellation semantics ever change, it tells you the stored-accept design
  can be simplified.

So §6's wording needs one correction — the pool moves *reshaped*, not
unchanged — but the trait set is sufficient, and the result is strictly better
than today's: no `abort()`/`LISTEN` window at all.

### 3.2 `Datagram` needed `local_addr` and a binder — both added, both work

Confirmed as a real gap and closed by widening the trait, not by dropping
behaviour. `Datagram::local_addr` is now part of the contract and
`DatagramBinder` supplies sockets, so the engine's `Action::ResetSocket` can
rebind.

Both are implementable on both runtimes. Tokio reads `UdpSocket::local_addr`;
Embassy assembles the address from the socket's bound port and the stack's
IPv4 config, and rebinds by `close()` + `bind()` on the same socket rather
than recreating it (which would strand its buffers).

`neutral::tests::unified_task_advertises_the_real_local_endpoint` drives the
unified task against a real UDP gateway socket and asserts the bytes: the
CONNECT_REQUEST's control HPAI carries `127.0.0.1` and the socket's actual
bound port, not `0.0.0.0:0`. Worth noting the Embassy half never set the
explicit endpoint *at all* today, so unification fixes it rather than
regressing it.

### 3.3 `TunnelIo::send` did block generic code — fixed, and it cost one line

Reproduced exactly. A probe asserting `Send` on `drain_actions`' future in
generic code gave `E0277: future cannot be sent between threads safely`, with
rustc prescribing the fix verbatim:

```
help: `Send` can be made part of the associated future's guarantees for all
      implementations of `tunnel::TunnelIo::send`
-     async fn send(&mut self, frame: &[u8]) -> bool;
+     fn send(&mut self, frame: &[u8]) -> impl Future<Output = bool> + Send;
```

Applied, with `tunnel::tests::drain_actions_future_is_send_in_generic_code` as
the guard. The two existing impls then split exactly along §5.1's predicted
line: the Tokio one stays a plain `async fn`, and the Embassy one — whose
`embassy_net::udp::UdpSocket` future is `!Send` — returns the adapter's
transparent `SendFutureWrapper`. That is §5.1's mechanism exercised on a real
trait in a real connector rather than on a standalone sketch.

### 3.4 §6 and acceptance 2 really did conflict — the signature is what gives

A type assertion on the real struct confirmed it: `TlsOptions` is `!Send`
because its RNG is a bare `&'static mut dyn CryptoRngCore`, so §5.5's
`spin::Mutex<Option<T>>` cannot hold it and the `unsafe impl`s cannot go.

The resolution is one `+ Send` on the trait object — a signature change §6
says will not happen, and the cheapest of the available options, since every
concrete CSPRNG a caller passes already satisfies it. With that:

- core gains `session::OneShot<T>`, §5.5's cell: `Send + Sync` for any
  `T: Send`, no `unsafe`, with a compile-time assertion of that property so a
  regression surfaces in core rather than in a connector;
- `TlsSlot` becomes an alias for it, and **`aimdb-mqtt-connector` now carries
  zero `unsafe impl`s** (was two);
- the whole `embassy-tls` path still cross-compiles to thumbv7em, so
  `embedded-tls` accepts the bound.

§6 should be amended to say `TlsOptions` keeps its *shape* but gains a `Send`
bound on its RNG.

## 4. The two understated consequences, measured

### 4.1 `CriticalSectionRawMutex` on std is a link-time obligation

§5.2 mentions `critical-section/std` only as a host-test detail. A standalone
std binary using `Channel<CriticalSectionRawMutex, _, N>` fails to link:

```
rust-lld: error: undefined symbol: _critical_section_1_0_acquire
rust-lld: error: undefined symbol: _critical_section_1_0_release
```

`NoopRawMutex` cannot substitute — it is `!Sync`, so it cannot back a shared
channel at all (`*mut () cannot be shared between threads safely`).
`CriticalSectionRawMutex` is therefore forced on std, and the obligation with
it.

It is also fully mitigable in one line, which the branch does: the KNX
connector's `tokio-runtime` feature now enables `critical-section/std` itself,
so no downstream std user ever sees the error.
`neutral::tests::shared_embassy_channels_carry_telegrams_on_tokio` then runs
§5.2's claim rather than asserting it — the unified task on Tokio, against a
real UDP gateway, carrying a full handshake, an inbound telegram with its ACK,
and an outbound command, all through the same `embassy_sync` channel types the
MCU uses.

### 4.2 The `embassy-sync` patch, and one feature-unification trap

§5.4's "dependency-free" claim is exact: `embassy-futures` 0.1.2 has an empty
`[dependencies]` bar optional `defmt`/`log`, and its `select3` drives the
unified KNX loop on both runtimes. It is now an unconditional dependency of
the KNX connector, and the std build is unaffected.

One trap found by building rather than reading: making the adapter's `net`
feature imply `embassy-time` turns on the workspace's
`defmt-timestamp-uptime`, which defines `_defmt_timestamp` and collides with
the `defmt::timestamp!` every host test binary must supply. Sockets and clock
have to stay separable features — `net` does not imply `embassy-time`, and
`EmbassyDelay` is gated separately.

## 5. Acceptance criteria, re-reviewed against the prototype

| # | Assessment |
|---|---|
| 1 | **Met.** Core carries the new traits, `FramedConnection`, `FramingDialer`/`FramingListener` and `OneShot`, and still cross-compiles to `thumbv7em-none-eabihf` with zero `unsafe`. |
| 2 | Needs rewording (it overlooks `aimdb-wasm-adapter`'s 16 `unsafe impl`s and `aimdb-bench`'s one), but now reachable: MQTT is at zero, serial was already at zero, and the TCP connector's remaining three live in the module this design deletes — the adapter's `net` module already replaces them. |
| 3 | **Met for serial**, and the doubt was unfounded: `tokio` for `io-util` alone suffices, with no `aimdb-tokio-adapter` dependency. See §3 of `neutral_framed.rs`. |
| 4 | Still the sharpest criterion. `two_concurrent_sessions` and the three other loopback tests pass unchanged, and `neutral_pool.rs` adds the pooled-`StreamListener` equivalent. Note MQTT's `build_internal` tests are Tokio-only unit tests; there is still **no** host test for either Embassy MQTT path. |
| 5 | **Near-vacuous, unchanged.** `examples/embassy-bench-stm32h5` depends only on `aimdb-core`, `aimdb-embassy-adapter` and `aimdb-bench` — no connector — so it will show no change whatever the refactor does. Replace it with an allocation-counting test on a connector path. |
| 6 | Still the most valuable, and still unbuilt. Build it first, not last (§6.3). |

## 6. Effort assessment, recalibrated

### 6.1 Code in scope

Runtime-specific connector code the design deletes or restructures:

| Crate | Files | Lines |
|---|---|---|
| `aimdb-tcp-connector` | `embassy_transport.rs` 591 + `tokio_transport.rs` 302 | 893 |
| `aimdb-serial-connector` | `embassy_transport.rs` 237 + most of `tokio_transport.rs` 312 | 549 |
| `aimdb-knx-connector` | `embassy_client.rs` 468 + `tokio_client.rs` 632 | 1 100 |
| `aimdb-mqtt-connector` | `embassy_client.rs` 712 + `embassy_tls.rs` 415 + `tokio_client.rs` 457 | 1 584 |
| **Total** | | **~4 130** |

Untouched and load-bearing: `tunnel.rs` (1 400), the two `framing.rs`
(93 + 139), `sntp*.rs` (281), `link_ext.rs` (68).

The prototype is 2 324 lines added across 22 files, which is
the first hard data point on the "new code" estimate. It covers step 1 in
full, both halves of step 2, the load-bearing parts of steps 4a and 4b, and
none of steps 3, 5 or 6.

### 6.2 Stage estimates

| Step | Work | Days | Status |
|---|---|---|---|
| 1 | Core traits, `FramedConnection`, framing adapters, `OneShot` | 2–3 | **done on the prototype branch** |
| 2a | `aimdb-tokio-adapter` `net` feature | 1–2 | **done** |
| 2b | `EmbassyNet`/`EmbassyUart`/`Delay` + slot-pool move | 3–5 | **done**; §3.1 resolved, so the risk that inflated this is gone |
| 3 | TCP pilot: port the connector onto the traits, migrate `embassy_loopback` | 3–4 | not started; the adapter side it depends on is done |
| 4a | Serial | 1–2 | framing + streams **done**; connector sugar and test migration remain |
| 4b | KNX | 3–5 | unified task **done**; builder/channel wiring and deleting the two clients remain |
| 5 | MQTT `Native`/`Embedded` split | 5–8 | not started |
| 6 | Examples, codegen + drift re-baseline, design 012, CHANGELOGs, major bumps | 2–3 | not started |
| | **Total** | **20–32 days ≈ 4–6 weeks** | |

The estimate holds. What changed is its shape: the week of contingency I
attached to §3.1 is no longer needed, and the four gaps that could each have
forced a rethink are closed. The remaining risk is concentrated almost
entirely in step 5.

### 6.3 Where the risk actually sits

- **MQTT is the long pole and the thinnest-covered.** 1 584 lines restructured
  with no host integration test for either Embassy path. Moving the plain path
  from `run_with_subscriptions` to `handle_messages` means re-implementing the
  reconnect-and-resubscribe loop mountain-mqtt-embassy currently owns
  (`embassy_client.rs:562-577` — "the manager re-subscribes these topics on
  every connection, so inbound routing survives reconnects"). That is
  behaviour, not plumbing. Acceptance 6 is the mitigation and should be built
  **first**.
- **Everything else is now de-risked by running code.** The traits, both
  adapters, the accept pool, the unified KNX task and the neutral framed
  connection all exist and are tested.
- **What is well covered:** CI cross-compiles every Embassy connector to
  `thumbv7em-none-eabihf` (`make test-embedded`, 8 connector configurations),
  and the TCP and serial halves have real host smokes.

## 7. What is on the prototype branch

New:

- `aimdb-core/src/session/io.rs` — `ByteStream`, `StreamDialer`,
  `StreamListener`, `Datagram`, `DatagramBinder`, `Delay`, `Framer`,
  `FramerFactory`, `FramedConnection`, `FramingDialer`, `FramingListener`,
  `OneShot`. Additive; core still has zero `unsafe`.
- `aimdb-embassy-adapter/src/net.rs` (feature `net`) — `EmbassyNet::tcp` /
  `listen::<N>` / `udp`, `EmbassyTcpStream`, the stored-accept pool,
  `EmbassyUdpSocket`/`EmbassyUdpBinder`, `EmbassyDelay`. All the force-`Send`
  for the TCP and UDP paths lives here.
- `aimdb-tokio-adapter/src/net.rs` (feature `net`) — the std duals, every
  future a plain `async fn`, no `unsafe`.
- `aimdb-knx-connector/src/neutral.rs` — one `connection_task` generic over
  `DatagramBinder + Delay`, plus the `embassy_sync` channel bridges.
- `aimdb-serial-connector/src/neutral.rs` — the COBS framer against core's
  trait, and one `ByteStream` per byte source.
- Tests: `aimdb-tcp-connector/tests/neutral_pool.rs`,
  `aimdb-serial-connector/tests/neutral_framed.rs`, and unit tests in
  `neutral.rs` and `tunnel.rs`.

Changed: `TunnelIo::send` gains `+ Send`; `TlsOptions`'s RNG gains `+ Send`
and `TlsSlot` becomes `OneShot`; the KNX `tokio-runtime` feature adopts
`embassy-sync` + `critical-section/std`; `embassy-futures` becomes
unconditional.

Verification run: `make test-embedded` passes (all 8 Embassy connector
configurations), every connector test suite passes, and clippy is clean on
the changed crates. Two pre-existing failures are unrelated to this work and
reproduce on `main`: `aimdb-data-contracts`' `compile_fail` trybuild suite,
and `cargo clippy --target thumbv7em-none-eabihf`, which fails in this
environment for untouched crates too.
