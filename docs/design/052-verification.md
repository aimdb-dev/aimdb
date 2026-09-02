# 052 — Verification against the codebase, and effort assessment

**Status:** review, 2026-09-02. Verifies
[052 — Runtime-neutral connectors](052-runtime-neutral-connectors.md) against
the tree at `58c1951`, on the pinned toolchain (rustc 1.98.0, `88d9e12ae`).
**Verdict:** the architecture is sound and the core mechanism is real — it
compile-checks exactly as claimed. Four factual errors and four unresolved
design gaps need fixing before step 1 starts; none of them invalidate the
approach. Effort: **4–6 weeks** for one engineer already fluent in the crates.

---

## 1. What checks out

Every load-bearing claim in §2, §5 and §6 was checked against the source.

| Claim | Verified |
|---|---|
| §5.1 return-type notation is experimental on 1.98 | Yes — `error[E0658]: return type notation is experimental`, issue #109417 |
| §5.1 the trait-declared `+ Send` shape works instead | Yes — a standalone crate with a `!Send` socket, a `SendFutureWrapper` newtype impl, a plain `async fn` impl, a generic `FramedConnection`, and a generic task boxed as `Send + 'static` compiles clean under `rustup run 1.98.0` |
| §5.2 `embassy-sync` pulls in no executor | Yes — `_external/embassy/embassy-sync/Cargo.toml` deps are exactly `critical-section`, `heapless`, `futures-core`, `futures-sink`, `embedded-io-async`, `cfg-if` (+ optional `defmt`/`log`) |
| §5.2 KNX Embassy already uses `StaticCell<Channel<CriticalSectionRawMutex, …, N>>` | Yes — [`embassy_client.rs:76-107`](../../aimdb-knx-connector/src/embassy_client.rs) |
| §5.2 Tokio's `with_command_queue_size` becomes a const generic | Yes — it exists at [`tokio_client.rs:64`](../../aimdb-knx-connector/src/tokio_client.rs); the Embassy side already notes the size must be a constant |
| §5.5 `spin::Mutex` needs no new dependency | Yes — `aimdb-core/Cargo.toml:120` already declares `spin` with `mutex`/`spin_mutex` |
| §4 `RuntimeOps::sleep` boxes | Yes — `fn sleep(&self, d) -> BoxFuture` at [`executor.rs:50`](../../aimdb-core/src/executor.rs); `now_nanos` is a plain call |
| §2 KNX is ~90 % shared behind a three-method `TunnelIo` | Yes — `send`/`forward`/`warn_ack_timeout` at [`tunnel.rs:536-552`](../../aimdb-knx-connector/src/tunnel.rs); `drain_actions` is the shared driver |
| §2 both KNX halves bypass `RuntimeOps` for the clock | Yes — `embassy_time::Instant::now()` at `embassy_client.rs:281`, `tokio::time::Instant` at `tokio_client.rs:221` |
| §2 the serial Embassy half is already the target shape | Yes — it contributes only `CobsFramer` and rides `EmbassyConnection<Rd, Wr, F, RC, WC>`; the file carries **zero** `unsafe` |
| §6 `embassy_tls.rs` already bypasses `run_with_subscriptions` | Yes — `handle_messages` at [`embassy_tls.rs:367`](../../aimdb-mqtt-connector/src/embassy_tls.rs); the plain path still uses `run_with_subscriptions` at `embassy_client.rs:577` |
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

## 3. Gaps that need resolving before step 1

### 3.1 `StreamListener` cannot express the Embassy N-socket accept pool

§3.2 and §6 both say the slot pool "moves into the adapter **unchanged**". It
cannot, as written. The pool's purpose is keeping *N sockets in `accept()`
simultaneously* — `embassy-net` has no central listener, so each socket must
enter `accept()` itself. Today the connector owns that fan-out directly:
`TcpListener::<N>::into_server_futures` spawns one `serve_socket_slot` worker
per slot, each calling `run_session` itself
([`embassy_transport.rs:362-445`](../../aimdb-tcp-connector/src/embassy_transport.rs)).
The `Listener` impl exists only for the `N = 1` compatibility path.

`StreamListener::accept(&mut self)` is single-shot, and core's `serve`
([`server.rs:398`](../../aimdb-core/src/session/server.rs)) is a serial accept
loop. Routing the pool through it gives one pending listener at a time, not N.
Two ways out, both real work:

- **`select` over N slots inside the adapter's `accept`.** Honours the
  signature and keeps N sockets listening — but every successful accept drops
  the other N−1 pending `accept()` futures, and re-entering restarts them. The
  existing `SlotReturn` guard exists precisely because a dropped mid-`accept`
  future must recycle its socket; whether embassy-net can lose an in-flight SYN
  across that cancel needs verifying on hardware, not asserting.
- **Keep a multi-worker API on the adapter** alongside `StreamListener`. Then
  `TcpServer<N>` still reaches an Embassy-shaped API, which is the fork the
  design set out to remove.

`tests/embassy_loopback.rs::two_concurrent_sessions` drives
`accept_on(0)`/`accept_on(1)` concurrently and is explicitly written so that
"a broken pool (only one socket accepting…) would hang the second session";
it is the test that will catch this, and acceptance 4 requires it to pass.

### 3.2 `Datagram` is too thin for the KNX connection task

Two things the unified `connection_task<U: Datagram, T: Delay>` needs that the
trait does not offer:

- **Local address.** The Tokio half binds `0.0.0.0:0`, reads
  `socket.local_addr()`, and feeds it to `engine.set_local_endpoint(
  LocalEndpoint::Explicit { ip, port })` — required by gateways that reject the
  NAT-style HPAI ([`tunnel.rs:107-121`](../../aimdb-knx-connector/src/tunnel.rs)).
  `Datagram` has `send_to`/`recv_from` only, so unifying the task either adds
  `local_addr()` to the trait or silently downgrades every Tokio deployment to
  `LocalEndpoint::Nat` — a behaviour regression against real gateways.
- **Rebind.** Both halves drop and re-create the socket on each reconnect
  cycle. A moved-in `Datagram` value cannot be rebound; the design needs a
  binder/factory trait, or the engine's socket-reset action loses its effect.

### 3.3 `TunnelIo` needs the same `+ Send` treatment as `ByteStream`

`TunnelIo::send` is a bare `async fn` in a trait, and `drain_actions` is
generic over `impl TunnelIo`. Inside `drain_actions` the returned future is
opaque and not provably `Send`, so `drain_actions`' own future is not `Send`
— exactly the §5.1 problem, one layer up. The unified connection task cannot
be boxed as the runner's `Send + 'static` future until `TunnelIo::send`
declares `-> impl Future<Output = bool> + Send`. Cheap to fix, but it is not
in §11's step list and it is a semver-visible change to a `pub(crate)` trait
whose two impls both live in files the design deletes.

### 3.4 Acceptance 2 collides with §6 on `TlsOptions`

§6 says "`TlsOptions` keeps its signature". Its `rng` field is
`&'static mut dyn CryptoRngCore` — a trait object with no `Send` bound, so
`TlsOptions` is **not** `Send`, so §5.5's `spin::Mutex<Option<L>>` (which
requires `L: Send`) cannot replace `TlsSlot`'s `unsafe impl Send + Sync`
([`embassy_client.rs:287-294`](../../aimdb-mqtt-connector/src/embassy_client.rs)).
Either `TlsOptions::new` gains `dyn CryptoRngCore + Send` (a signature change,
a break for every caller, though `embassy_stm32::rng::Rng` should satisfy it)
or the slot moves into the adapter. Pick one; as written the two sections
contradict each other.

## 4. Two consequences the design understates

- **`CriticalSectionRawMutex` on the host is not free.** §5.2 notes the
  adapter's host *tests* use `critical-section/std`. The same applies to
  production: adopting `embassy_sync::channel::Channel<CriticalSectionRawMutex, …>`
  in the Tokio KNX path means every std binary linking the connector must now
  supply a `critical-section` implementation, or fail at link time. That is a
  new obligation on downstream std users and belongs in §7, not only in a test
  aside.
- **The workspace patches `embassy-sync` to a local checkout**
  (`[patch.crates-io]`, `Cargo.toml:172-175`), and the comment there records
  that this "blocks publishing `aimdb-embassy-adapter`". Putting `embassy-sync`
  on the std side of the KNX connector widens that publishing constraint's
  blast radius. Not a blocker — the KNX channel needs no patched API — but it
  should be a stated assumption.

## 5. Acceptance criteria, reviewed

| # | Assessment |
|---|---|
| 1 | Good. Baseline verified passing; core's zero-`unsafe` property is real. |
| 2 | Needs rewording (see §2.4 above) and is unreachable while §3.4 stands. |
| 3 | Reachable, with one caveat: the serial connector's `ByteStream` impl over `tokio_serial::SerialStream` needs `tokio` (io-util) or an `aimdb-tokio-adapter` dependency the manifest deliberately avoids. §8 flags this for the *sugar* but not for the stream impl itself. |
| 4 | The right criterion, and the sharpest one — `two_concurrent_sessions` is what §3.1 must satisfy. Note that MQTT's `build_internal` tests are Tokio-only unit tests inside `tokio_client.rs`; there is **no** host test for either Embassy MQTT path. |
| 5 | Near-vacuous. `examples/embassy-bench-stm32h5` depends only on `aimdb-core`, `aimdb-embassy-adapter` and `aimdb-bench` — no connector crate — so it will show no change whatever the refactor does. If per-message allocation in the connector path is the property worth guarding, it needs a test that exercises a connector. |
| 6 | Good, and the most valuable one: it is the first host-side coverage the embedded MQTT backend would ever get. |

## 6. Effort assessment

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

New code, estimated: core traits + `FramedConnection` + framing adapters
~450 (of which ~110 is the `framed` module lifted from
`aimdb-embassy-adapter/src/connectors.rs`); `TokioNet` ~300; `EmbassyNet` /
`EmbassyUart` ~450 (including ~250 of moved slot pool); rewritten connector
modules ~1 000. Net: roughly **2 200 lines written, 4 130 replaced**, across
6 crates, 7 example binaries, `aimdb-codegen`, and 12 test files (2 607 lines).

### 6.2 Stage estimates

Following §11's own sequencing, for one engineer fluent in these crates:

| Step | Work | Days |
|---|---|---|
| 1 | Core traits, `FramedConnection`, `FramingDialer`/`FramingListener`; lift `Framer` out of the adapter | 2–3 |
| 2a | `aimdb-tokio-adapter` `net` feature | 1–2 |
| 2b | `EmbassyNet`/`EmbassyUart`/`Delay` + slot-pool move — **includes resolving §3.1** | 3–5 |
| 3 | TCP pilot + `embassy_loopback` (368 lines, two real `embassy-net` stacks) | 3–4 |
| 4a | Serial (already the target shape) | 1–2 |
| 4b | KNX: unify two clients, `TunnelIo` `Send` bound, §3.2 `Datagram` gaps | 3–5 |
| 5 | MQTT `Native`/`Embedded` split; plain path from `run_with_subscriptions` to `handle_messages` | 5–8 |
| 6 | Examples, `aimdb-codegen` + drift re-baseline, design 012 rewrite, CHANGELOGs, major bumps | 2–3 |
| | **Total** | **20–32 days ≈ 4–6 weeks** |

Add roughly a week if §3.1 forces a rethink of `StreamListener`, and a week of
on-hardware validation (STM32H5) for the Embassy MQTT/KNX paths.

### 6.3 Where the risk actually sits

- **MQTT is the long pole and the thinnest-covered.** 1 584 lines restructured
  with no host integration test for either Embassy path — only Tokio-side
  `build_internal` unit tests and topic/link unit tests. Moving the plain path
  from `run_with_subscriptions` to `handle_messages` means re-implementing the
  reconnect-and-resubscribe loop that mountain-mqtt-embassy currently owns
  ([`embassy_client.rs:562-577`](../../aimdb-mqtt-connector/src/embassy_client.rs)
  — "the manager re-subscribes these topics on every connection, so inbound
  routing survives reconnects"). That is behaviour, not plumbing. Acceptance 6
  is the mitigation and should be built *first*, not last.
- **The Embassy TCP pool** (§3.1) is the one place where the design's trait
  set does not yet cover the existing behaviour.
- **What is well covered:** CI cross-compiles every Embassy connector to
  `thumbv7em-none-eabihf` (`make test-embedded`, 8 connector configurations),
  so compile-level regressions surface immediately, and the TCP and serial
  halves have real host smokes.

### 6.4 A cheaper first slice

If the goal is de-risking rather than completeness, steps 1–3 (core traits,
both adapters, TCP pilot) are ~10–14 days and answer every open question in
§3.1 and §5.1 against real code. Serial follows almost for free. KNX and MQTT
— 2 684 of the 4 130 lines — can then be scheduled on evidence instead of on
estimate, and the FreeRTOS adapter that motivates the whole design (051 §8)
only needs steps 1–2 to be startable.
