# 052 — Runtime-neutral connectors

**Status:** proposal, 2026-09-02. No code changes; a design worked out from the
question below, with the mechanism compile-checked on the pinned toolchain.
**Predecessors:** [033 — Unify Embassy connectors](033-M17-unify-connectors-drop-send.md)
(the centralized Embassy spine this design generalizes),
[051 — FreeRTOS portability assessment](051-freertos-portability-assessment.md)
(§5.2 of which this design resolves).
**Scope:** `aimdb-core` (`connector-session`), `aimdb-tokio-adapter`,
`aimdb-embassy-adapter`, and the TCP, serial, KNX and MQTT connector crates. A
semver-major bump for the connector crates; `aimdb-core` gains only additive
traits.

---

## 1. The question

Design 051 found that porting the engine to FreeRTOS is a thin glue layer, but
that every network connector has to be touched, because each ships a Tokio half
and an Embassy half and the Embassy half is written against `embassy_net`. The
question that started this document:

> Can we move the Embassy/Tokio specific logic to the adapters and keep the
> connectors platform independent?

The answer is yes for TCP, serial, UDS and KNX in full, and for MQTT in
everything except the protocol client. The condition is that the boundary
between connector and adapter moves one layer down. Today the adapter bridges
`Send`-ness and the connector owns the sockets; it has to be the other way
round. The adapter owns sockets, clocks and channels behind small
runtime-neutral traits, and the connector owns framing, protocol logic and
sugar. A FreeRTOS port is then one adapter crate and zero connector edits.

A first cut of the design carried real costs on the MCU (§4). The revision in
§5 removes every one of them; the remaining changes are compile-time only.

## 2. What is platform-specific today (audit)

Every `tokio_*` / `embassy_*` half of the four embedded-capable connectors was
read for this assessment. The differences fall into five buckets, and only one
of them lacks a portable abstraction in the workspace already.

| Bucket | Tokio half | Embassy half | Portable option today |
|---|---|---|---|
| Sockets and byte streams | `TcpStream`, `UdpSocket`, `SerialStream` | `embassy_net::tcp::TcpSocket`, `embassy_net::udp::UdpSocket`, `embedded_io_async` UART halves | **None.** The one real fork |
| Channels pumps ⇄ protocol task | `tokio::sync::mpsc` | `embassy_sync::channel::Channel` in a `StaticCell` | `embassy-sync` itself (executor-independent, §5.2) |
| Time and select | `tokio::time::sleep`, `tokio::select!` | `embassy_time::Timer`, `embassy_futures::select3` | `RuntimeOps::now_nanos`; `embassy-futures` (dependency-free) |
| `Send`-ness | native | `SendFutureWrapper`, `OneShotCell`, `NetStack` | Already confined to [`aimdb-embassy-adapter/src/connectors.rs`](../../aimdb-embassy-adapter/src/connectors.rs) by design 033 |
| Protocol library | `rumqttc` | `mountain-mqtt` | KNX already shares a sans-io `TunnelEngine`; MQTT has no shared client |

Two observations that shape the design:

- The KNX halves are already 90 % shared. [`tunnel.rs`](../../aimdb-knx-connector/src/tunnel.rs)
  owns the whole tunnelling lifecycle behind a `TunnelIo` trait with three
  methods; the two client files differ only in socket, channel, timer and
  select glue. Both ignore `RuntimeOps` and call their runtime clock directly
  ([`embassy_client.rs:283`](../../aimdb-knx-connector/src/embassy_client.rs),
  [`tokio_client.rs:691`](../../aimdb-knx-connector/src/tokio_client.rs)).
- The serial Embassy half is already the target shape: it contributes a COBS
  `Framer` and rides `EmbassyConnection<Rd, Wr, F>` from the adapter, generic
  over `embedded_io_async::{Read, Write}`. That type, minus its `unsafe`, is
  the neutral framed connection this design puts in core.

## 3. Target architecture

Three additions to core, then adapters implement them. Connectors become one
module each, generic over the traits, with no runtime `cfg` on the code path.

### 3.1 Core traits (`aimdb-core`, feature `connector-session`)

```rust
/// An unframed, bidirectional byte stream. Sits *below* `Connection`
/// (which is framed) so the connector keeps its framing and the adapter
/// keeps its socket.
pub trait ByteStream {
    fn read<'a>(&'a mut self, buf: &'a mut [u8])
        -> impl Future<Output = Result<usize, IoError>> + Send + 'a;
    fn write_all<'a>(&'a mut self, buf: &'a [u8])
        -> impl Future<Output = Result<(), IoError>> + Send + 'a;
    fn flush(&mut self) -> impl Future<Output = Result<(), IoError>> + Send + '_;
}

/// Produces streams: the client side. `host` is unresolved; name
/// resolution is the adapter's job (embassy-net DNS, getaddrinfo, lwIP).
pub trait StreamDialer {
    type Stream: ByteStream + Send;
    fn connect<'a>(&'a self, host: &'a str, port: u16)
        -> impl Future<Output = Result<Self::Stream, IoError>> + Send + 'a;
}

/// Produces streams: the server side.
pub trait StreamListener {
    type Stream: ByteStream + Send;
    fn accept(&mut self)
        -> impl Future<Output = Result<(Self::Stream, PeerInfo), IoError>> + Send + '_;
}

/// Connectionless I/O for KNX/IP (and SNTP).
pub trait Datagram {
    fn send_to<'a>(&'a mut self, buf: &'a [u8], to: core::net::SocketAddr)
        -> impl Future<Output = Result<(), IoError>> + Send + 'a;
    fn recv_from<'a>(&'a mut self, buf: &'a mut [u8])
        -> impl Future<Output = Result<(usize, core::net::SocketAddr), IoError>> + Send + 'a;
}

/// A non-allocating sleep. `RuntimeOps::sleep` is dyn and must box; this
/// one is generic and returns the adapter's own timer type.
pub trait Delay {
    fn sleep(&self, d: core::time::Duration) -> impl Future<Output = ()> + Send;
}

/// Frames a byte stream: COBS, length-prefix, NDJSON. Lifted from the
/// adapter's `connector-io` module unchanged.
pub trait Framer { /* encode / push_bytes / next_frame, as today */ }

/// `Connection` over any `ByteStream` + `Framer`. This is
/// `EmbassyConnection` without the `unsafe impl Send`.
pub struct FramedConnection<S: ByteStream, F: Framer, const RC: usize, const WC: usize> { /* … */ }
impl<S: ByteStream + Send, F: Framer + Send, …> Connection for FramedConnection<S, F, …> { /* … */ }

/// Adapt a `StreamDialer` / `StreamListener` plus a framer factory into
/// the existing `Dialer` / `Listener`, so `run_client` / `serve` are untouched.
pub struct FramingDialer<D, FF> { /* … */ }
pub struct FramingListener<L, FF> { /* … */ }
```

All futures are return-position `impl Future … + Send`. That one decision is
what makes the design zero-cost; §5.1 explains why.

### 3.2 Adapters

- **`aimdb-tokio-adapter`**, new feature `net`: `TokioNet::tcp()` implementing
  `StreamDialer` over `tokio::net::TcpStream`, `TokioNet::listen(addr)` for
  `StreamListener`, `TokioNet::udp(bind)` for `Datagram`, and `Delay` over
  `tokio::time::sleep`. Opening a serial device with `tokio-serial` stays in the
  serial connector under `std`; that dependency does not belong in the adapter.
- **`aimdb-embassy-adapter`**: `EmbassyNet::tcp(stack, rx, tx)`,
  `EmbassyNet::listen::<N>(stack, endpoint, rx[N], tx[N])` (the socket-slot pool
  moves here from [`aimdb-tcp-connector/src/embassy_transport.rs`](../../aimdb-tcp-connector/src/embassy_transport.rs)
  unchanged), `EmbassyNet::udp(stack, …)`, `EmbassyUart::split(rx, tx)`, and
  `Delay` returning `embassy_time::Timer`. Each stream/datagram newtype is
  `unsafe impl Send` and wraps the inner future in `SendFutureWrapper`. The
  `unsafe` stays exactly where design 033 put it. `NetStack` construction moves
  inside these constructors, so it leaves user and connector code.
- **A future `aimdb-freertos-adapter`** implements the same five traits over
  lwIP or FreeRTOS+TCP. lwIP handles are plain integers, so its types are
  `Send` naturally and need no wrapper at all.

### 3.3 Connectors

| Crate | Becomes | Deleted |
|---|---|---|
| `aimdb-tcp-connector` | `framing.rs` + `TcpClient::new(dialer)` / `TcpServer::new(listener)` generic over the traits | `tokio_transport.rs`, `embassy_transport.rs` |
| `aimdb-serial-connector` | `framing.rs` + `SerialClient::new(stream)` / `SerialServer::new(stream)` generic over `ByteStream`; the `tokio-serial` open helper stays under `std` | `embassy_transport.rs`, most of `tokio_transport.rs` |
| `aimdb-knx-connector` | `tunnel.rs` + one `connection_task<U: Datagram, T: Delay>` | `tokio_client.rs`, `embassy_client.rs` |
| `aimdb-mqtt-connector` | One `MqttConnector` type with two backends: `Native` (`rumqttc`, `std`) and `Embedded<N: StreamDialer + Delay>` (`mountain-mqtt` over `handle_messages`, as `embassy_tls.rs` already does) | `embassy_client.rs`'s stack plumbing; `tokio_client.rs` shrinks to the backend |
| `aimdb-uds-connector` | Unchanged (std-only by nature); could be `FramedConnection<…, NdjsonFramer>` for uniformity | — |
| `aimdb-websocket-connector` | Unchanged (axum, std-only) | — |

## 4. The first cut, and what it cost

The first version of this design reached the same layering with `dyn` traits:
`ByteStream` methods returning `BoxFut`, `async-channel` between pumps and
protocol task, `RuntimeOps::sleep` for timers. It was rejected for MCU cost:

| Cost | Cause |
|---|---|
| One heap allocation per read chunk | `BoxFut` on `ByteStream::read` |
| Heap ring, atomics, and the `event-listener` spin-lock caveat from 051 §5.3 | `async-channel` replacing `embassy_sync::channel::Channel` |
| One heap allocation per select iteration in the KNX loop | `RuntimeOps::sleep` boxes because it is dyn |
| `unsafe impl Sync` on a one-shot cell | `OneShotCell` holding moved-in peripherals |

Each of these is avoidable. The revision below keeps the layering and removes
the costs.

## 5. The zero-cost revision

### 5.1 `Send` without boxing: the bound goes on the trait's return type

Generic connector code must produce `Send` futures at the boxing boundary
(`ConnectorBuilder::build` returns `Vec<BoxFuture>`, `Connection::recv` returns
`BoxFut`). A generic `S: embedded_io_async::Read` cannot prove that
`S::read(..)` yields a `Send` future; return-type notation would express that
bound, and it is still experimental on the pinned 1.98 toolchain (checked:
`error[E0658]: return type notation is experimental`).

If the trait declaration itself carries the bound, generic code gets it for
free and nothing is boxed:

```rust
pub trait ByteStream {
    fn read<'a>(&'a mut self, buf: &'a mut [u8])
        -> impl Future<Output = Result<usize, IoError>> + Send + 'a;
}
```

- A Tokio impl writes a plain `async fn`; the compiler checks its future is
  `Send`, which `tokio::io::AsyncReadExt::read` is.
- An Embassy impl returns `SendFutureWrapper(self.0.read(buf))`, a transparent
  newtype over the `!Send` `TcpSocket` future. Zero runtime cost, and the
  `unsafe` is the adapter's, as today.
- `impl Connection for FramedConnection<S, F>` boxes once per **frame**, which
  is exactly what `EmbassyConnection` does today. Nothing new is boxed per
  chunk.
- Connectors are monomorphized per platform. That is one instantiation per
  runtime, the same as today's two hand-written halves, so flash does not grow.

This was compile-checked on rustc 1.98.0 with a `!Send` socket standing in for
`embassy_net::tcp::TcpSocket`, an adapter newtype wrapping in
`SendFutureWrapper`, a Tokio-style `async fn` impl, a generic framed connection,
and a generic protocol task returned as the runner's `Send + 'static` boxed
future. The adapter's `EmbassySinkRaw` at
[`connectors.rs:75`](../../aimdb-embassy-adapter/src/connectors.rs) already
uses this return-position shape, minus the `Send` bound.

The trait is not dyn-compatible. That is fine: connectors are generic
(`TcpServer<L>`, `KnxConnector<U, T>`), and the dyn boundary stays where it is
today, at `Box<dyn Connection>` per frame.

### 5.2 Channels: keep `embassy-sync`, it never depended on the executor

`embassy-sync`'s dependencies are `critical-section`, `heapless`,
`futures-core`, `futures-sink`, `embedded-io-async` and `cfg-if` (upstream
manifest, checked). No executor. Design 051 §2 already built the buffer layer
with no executor crate in the graph, and the adapter's host tests run these
channels on Linux via `critical-section/std`.

So the KNX connector keeps its `Channel<CriticalSectionRawMutex, T, N>`
unchanged and the Tokio half simply adopts it. `CriticalSectionRawMutex` makes
the channel `Send + Sync`, which the KNX Embassy half already relies on (its
`KnxSource` is a plain `Source`, no force-`Send`). Two knock-on effects, both
positive:

- The `async-channel` spin-lock caveat from 051 §5.3 never enters the
  connectors.
- On FreeRTOS the critical section is `taskENTER_CRITICAL`, so another task
  can safely enqueue a command. `NoopRawMutex` could not offer that.

Allocation is a one-line choice: `StaticCell` as today (zero heap, one
connector per image) or `Arc<Channel<…>>` created once in `build` (one
allocation at build, several connectors per host process, consistent with
design 037's allocate-at-build model). Both are zero per message. The Tokio
`with_command_queue_size` becomes a const generic either way.

MQTT's embedded backend keeps the `NoopRawMutex` channels that
`mountain-mqtt-embassy`'s `handle_messages` dictates; they belong to that
backend, not to the neutral layer.

### 5.3 Time: a generic `Delay`, not the dyn sleep

`RuntimeOps::now_nanos()` is a plain call and stays the clock for the KNX
engine's `Millis`. Only sleeping moves: the `Delay` trait (§3.1) with the same
`impl Future + Send` shape. Embassy returns `embassy_time::Timer`, a two-field
struct (`expires_at`, `yielded_once`; upstream source checked) that is `Send`
and allocates nothing. Tokio returns `tokio::time::Sleep`.

### 5.4 Select: `embassy-futures` is dependency-free

`embassy-futures` has no dependencies beyond optional `defmt`/`log` (upstream
manifest, checked). Its `select3` is what the KNX Embassy loop uses now, and it
runs unchanged on Tokio. `futures_util::select_biased`, which core's client
engine already uses in `no_std`, is the other zero-cost option. Either works.

### 5.5 Moved-in peripherals: a `Mutex<Option<L>>` in core

`ConnectorBuilder: Send + Sync` with `build(&self)` means a moved-in listener
must be taken through interior mutability. `spin::Mutex<Option<L>>` with
`L: Send` is `Send + Sync` without `unsafe`, replacing the adapter's
`OneShotCell` and its `unsafe impl Sync`. Taken once at build; no cost after.

### 5.6 Cost table, revised

| Cost in the first cut | Replacement | MCU delta vs today |
|---|---|---|
| Box per read chunk | `impl Future + Send` in the trait | None; per-frame box unchanged |
| `async-channel` | `embassy_sync::channel::Channel<CriticalSectionRawMutex, …>` | None; KNX already uses it |
| `RuntimeOps::sleep` per iteration | `Delay` returning `embassy_time::Timer` | None |
| `futures_util::select_biased` | `embassy_futures::select` or keep | None |
| `OneShotCell` `unsafe impl Sync` | `spin::Mutex<Option<L>>` in core | None after build |
| Force-`Send` | `SendFutureWrapper` inside adapter impls | None; transparent newtype |

## 6. What stays platform-specific, by necessity

- **MQTT protocol backends.** `rumqttc` is Tokio-only. `mountain-mqtt` is
  already generic over an `embedded-io-async` connection; its
  `run_with_subscriptions` helper binds `embassy_net::Stack`, but
  [`embassy_tls.rs`](../../aimdb-mqtt-connector/src/embassy_tls.rs) already
  bypasses that by calling `handle_messages` directly. Doing the same for the
  plain path, with a `StreamDialer` from the adapter, gives FreeRTOS MQTT
  through the embedded backend. The adapter's stream newtype implements
  `embedded_io_async::{Read, Write}` by delegation as well, so `mountain-mqtt`
  and `embedded-tls` see the type they expect.
- **TLS time source.** `TlsOptions` keeps its signature and `embedded-tls`
  layers over any `ByteStream`. The SNTP task becomes generic over `Datagram`,
  but the wall-clock anchor it sets (`EmbassyAdapter::set_unix_time`) has no
  neutral home yet, so the `mqtts://` path stays Embassy-only until it does.
- **Embassy TCP accept pool.** The N-socket slot recycling moves into the
  adapter unchanged. It is the adapter's problem to solve, not the connector's.
- **WebSocket and UDS** are std-only by nature.

## 7. User-facing result

The Tokio example does not change. The Embassy example changes in one place:
the network stack goes to the adapter, which hands the connector a transport.
Both sketches are cut down to the connector wiring; the `configure` blocks are
untouched.

### 7.1 Tokio

```rust
use aimdb_mqtt_connector::{MqttConnector, MqttLinkExt, MqttOutboundLinkExt};
use aimdb_tokio_adapter::{TokioAdapter, TokioRecordRegistrarExt};

// Unchanged. The std backend is rumqttc, which owns its own TCP and TLS
// sockets, so the connector needs nothing from the adapter.
let mut builder = AimDbBuilder::new().runtime(runtime).with_connector(
    MqttConnector::new(broker_url).with_client_id("tokio-demo-multi-sensor"),
);
```

One line becomes possible that does not exist today: the embedded MQTT backend
running on the host for parity tests.

```rust
let mqtt = MqttConnector::new(broker_url).transport(TokioNet::tcp());
```

### 7.2 Embassy

```rust
use aimdb_embassy_adapter::{EmbassyAdapter, EmbassyNet, EmbassyUart};
use aimdb_mqtt_connector::MqttConnector;
#[cfg(feature = "tls")]
use aimdb_mqtt_connector::embedded::TlsOptions;
use aimdb_serial_connector::SerialServer;

// NEW: the adapter owns the socket. The TCP buffers mountain-mqtt used to
// allocate internally are now yours, in statics, like the TCP connector
// already does. `EmbassyNet` also resolves hostnames when embassy-net's
// `dns` feature is on.
static MQTT_RX: StaticCell<[u8; 4096]> = StaticCell::new();
static MQTT_TX: StaticCell<[u8; 4096]> = StaticCell::new();
let mqtt_net = EmbassyNet::tcp(stack, MQTT_RX.init([0; 4096]), MQTT_TX.init([0; 4096]));

// CHANGED: `stack` is gone from the constructor; `.transport(...)` takes
// its place. Client id, credentials and TLS options read exactly as before.
let mqtt = MqttConnector::new(&broker_url)
    .with_client_id("embassy-demo-001")
    .transport(mqtt_net);

#[cfg(feature = "tls")]
let mqtt = mqtt.with_tls(TlsOptions::new(
    rng,
    MQTT_CA_DER,
    TLS_READ_BUF.init_with(|| [0; 16_640]),
    TLS_WRITE_BUF.init_with(|| [0; 4_096]),
));

// CHANGED: the UART halves go through the adapter too, instead of the
// connector's `embassy_transport` module.
let (serial_tx, serial_rx) = uart.split();
let serial = SerialServer::new(EmbassyUart::split(serial_rx, serial_tx))
    .security_policy(SecurityPolicy::read_only());

let mut builder = AimDbBuilder::new()
    .runtime(runtime.clone())
    .with_connector(mqtt)
    .with_connector(serial);
```

What moved:

- **Imports.** `aimdb_mqtt_connector::embassy_client::MqttConnectorBuilder` and
  `aimdb_serial_connector::embassy_transport::SerialServer` disappear. There is
  one `MqttConnector` and one `SerialServer`, with no runtime module in the
  path.
- **The `unsafe` stays hidden.** Today `MqttConnectorBuilder::new` wraps the
  stack in `NetStack` internally; tomorrow `EmbassyNet::tcp` does the same
  inside the adapter. User code has no `unsafe` in either version.
- **Two new statics.** The TCP socket buffers are the only genuinely new lines
  on the Embassy side. They replace buffers `mountain-mqtt-embassy` allocated
  out of sight, so RAM use is unchanged and now visible.

Behind the scenes the type is `MqttConnector<Embedded<N>>` for the
`.transport(...)` path and `MqttConnector<Native>` for the rumqttc path.
Neither name needs to appear in user code.

### 7.3 FreeRTOS, once an adapter exists

```rust
let mqtt = MqttConnector::new(&broker_url)
    .with_client_id("freertos-node-001")
    .transport(LwipNet::tcp());
```

No connector crate is touched to get there.

## 8. API impact, quantified

No tool or library crate constructs these connectors. The call sites are all
examples:

- `embassy-knx-connector-demo`, `embassy-mqtt-connector-demo`,
  `embassy-serial-connector-demo`
- `tokio-knx-connector-demo`, `tokio-mqtt-connector-demo`
- the four `weather-mesh-demo` binaries

The Embassy sites already pass a stack and static buffers explicitly, so
wrapping those in an adapter call is a one-line change. The Tokio MQTT sites do
not change. The Tokio TCP/serial string sugar (`TcpServer::new("127.0.0.1:7001")`)
becomes `TcpServer::new(TokioNet::listen("127.0.0.1:7001")?)`. Preserving the
string form would require the connector to depend on `aimdb-tokio-adapter`,
which the serial connector's manifest deliberately avoids today; accept the
one-line change instead.

Design 012 (connector development guide) needs its Tokio and Embassy
implementation sections rewritten around the traits in §3.1.

## 9. Non-goals

- Making `run_client` / `serve` generic to remove the per-frame
  `Box<dyn Connection>`. Not needed for this design; possible later.
- A neutral home for the wall-clock anchor (`set_unix_time`). Needed to make
  `mqtts://` FreeRTOS-capable; tracked separately.
- The FreeRTOS adapter itself. This design is what makes it a single crate;
  design 051 §4 and §6 describe its contents.

## 10. Acceptance criteria

1. `cargo check -p aimdb-core --no-default-features --features alloc,connector-session,remote --target thumbv7em-none-eabihf`
   passes with the new traits and `FramedConnection`; core still contains zero
   `unsafe`.
2. Every `unsafe impl Send`/`Sync` in the workspace remains inside
   `aimdb-embassy-adapter`. Connector crates contain none.
3. `aimdb-tcp-connector`, `aimdb-serial-connector` and `aimdb-knx-connector`
   have no `tokio_*` / `embassy_*` source modules and no `embassy-net`,
   `embassy-time` or `tokio` dependency except the serial connector's
   `tokio-serial` open helper under `std`.
4. The existing host tests pass unchanged in behaviour: TCP loopback
   (`_test-embassy-loopback`), serial duplex-pipe tests, the KNX fake-gateway
   tests, and the MQTT `build_internal` tests, each now instantiated through
   the adapter's transport types.
5. `examples/embassy-bench-stm32h5` shows no change in cycles per message or
   in allocations after `build()`: the connector path does not touch the
   buffer hot path, and no per-message allocation was introduced (§5.6).
6. The embedded MQTT backend builds and connects on the host over
   `TokioNet::tcp()` (the parity line in §7.1).

## 11. Sequencing

1. Core: `ByteStream`, `StreamDialer`, `StreamListener`, `Datagram`, `Delay`,
   `Framer`, `FramedConnection`, `FramingDialer`/`FramingListener`. Lift the
   `Framer` and framed connection out of the adapter's `connector-io` module.
2. Adapters: `TokioNet` (feature `net`) and `EmbassyNet`/`EmbassyUart`, moving
   the Embassy TCP slot pool in. Keep the old spine types for one release with
   deprecation notes.
3. TCP connector as the pilot: it exercises every new trait except `Datagram`.
4. Serial, then KNX (adds `Datagram` and `Delay`).
5. MQTT: split into `Native` and `Embedded<N>` backends; route the plain
   embedded path through `handle_messages`.
6. Examples, design 012, CHANGELOG; connector crates bump major.
7. Then the FreeRTOS adapter, per design 051 §8 step 3, with the network
   connectors coming along for free.
