# 052 — Runtime-neutral connectors

**Status:** proposal, revised 2026-09-02 after verification against the tree
and a working prototype (rustc 1.98.0, `88d9e12ae`). The layering is unchanged
and the mechanism holds; what changed is that four gaps the first draft left
open are now closed by code (§3.2, §5.1, §5.5, §5.6), and four factual errors
are corrected (§2 citations, §8's call-site audit, acceptance criteria 2 and
5). Passages marked **[verified]** were executed, not reasoned about;
**[revised]** marks a claim the prototype disproved. The review record, the
evidence, and the effort assessment live in
[052 — Verification](052-verification.md).
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
  ([`embassy_client.rs:281`](../../aimdb-knx-connector/src/embassy_client.rs),
  [`tokio_client.rs:221`](../../aimdb-knx-connector/src/tokio_client.rs)).
- The serial Embassy half is already the target shape: it contributes a COBS
  `Framer` and rides `EmbassyConnection<Rd, Wr, F>` from the adapter, generic
  over `embedded_io_async::{Read, Write}`. That type, minus its `unsafe`, is
  the neutral framed connection this design puts in core.

## 3. Target architecture

Three additions to core, then adapters implement them. Connectors become one
module each, generic over the traits, with no runtime `cfg` on the code path.

### 3.1 Core traits (`aimdb-core`, feature `connector-session`)

**[verified]** — implemented in `aimdb-core/src/session/io.rs`; core still
contains zero `unsafe` and still cross-compiles to `thumbv7em-none-eabihf`.

```rust
/// An unframed, bidirectional byte stream. Sits *below* `Connection`
/// (which is framed) so the connector keeps its framing and the adapter
/// keeps its socket. `Ok(0)` from `read` is EOF, as in both
/// `embedded_io_async::Read` and `tokio::io::AsyncRead`.
pub trait ByteStream {
    fn read<'a>(&'a mut self, buf: &'a mut [u8])
        -> impl Future<Output = TransportResult<usize>> + Send + 'a;
    fn write_all<'a>(&'a mut self, buf: &'a [u8])
        -> impl Future<Output = TransportResult<()>> + Send + 'a;
    fn flush(&mut self) -> impl Future<Output = TransportResult<()>> + Send + '_;
}

/// Produces streams: the client side. `host` is unresolved; name
/// resolution is the adapter's job (embassy-net DNS, getaddrinfo, lwIP).
pub trait StreamDialer {
    type Stream: ByteStream + Send;
    fn connect<'a>(&'a self, host: &'a str, port: u16)
        -> impl Future<Output = TransportResult<Self::Stream>> + Send + 'a;
}

/// Produces streams: the server side. One accept at a time, matching
/// `Listener::accept` and the `serve` loop — see §3.2 for why that does
/// **not** cost the Embassy socket pool its concurrency.
pub trait StreamListener {
    type Stream: ByteStream + Send;
    fn accept(&mut self)
        -> impl Future<Output = TransportResult<(Self::Stream, PeerInfo)>> + Send + '_;
}

/// Connectionless I/O for KNX/IP (and SNTP).
pub trait Datagram {
    fn send_to<'a>(&'a mut self, buf: &'a [u8], to: core::net::SocketAddr)
        -> impl Future<Output = TransportResult<()>> + Send + 'a;
    fn recv_from<'a>(&'a mut self, buf: &'a mut [u8])
        -> impl Future<Output = TransportResult<(usize, core::net::SocketAddr)>> + Send + 'a;
    /// The real bound address, when the stack exposes one. Not incidental:
    /// the KNX handshake advertises the client's own endpoint (HPAI), and
    /// gateways that reject the NAT-style `0.0.0.0:0` form need this.
    fn local_addr(&self) -> Option<core::net::SocketAddr>;
}

/// Binds `Datagram` sockets on demand, so a protocol task can rebind across
/// reconnect cycles — which the KNX engine's `Action::ResetSocket` requires.
pub trait DatagramBinder {
    type Socket: Datagram + Send;
    fn bind(&self, port: u16)
        -> impl Future<Output = TransportResult<Self::Socket>> + Send + '_;
}

/// A non-allocating sleep. `RuntimeOps::sleep` is dyn and must box; this
/// one is generic and returns the adapter's own timer type.
pub trait Delay {
    fn sleep(&self, d: core::time::Duration) -> impl Future<Output = ()> + Send;
}

/// Frames a byte stream: COBS, length-prefix, NDJSON. Lifted from the
/// adapter's `connector-io` module unchanged.
pub trait Framer { /* encode / push_bytes / next_frame, as today */ }

/// Builds a fresh `Framer` per connection. A blanket impl covers closures,
/// so a caller writes `|| CobsFramer::new()`.
pub trait FramerFactory { type Framer: Framer + Send; fn framer(&self) -> Self::Framer; }

/// `Connection` over any `ByteStream` + `Framer`. This is
/// `EmbassyConnection` without the `unsafe impl Send`.
pub struct FramedConnection<S, F, const RC: usize = 256, const WC: usize = 256> { /* … */ }
impl<S: ByteStream + Send, F: Framer + Send, …> Connection for FramedConnection<S, F, …> { /* … */ }

/// Adapt a `StreamDialer` / `StreamListener` plus a framer factory into
/// the existing `Dialer` / `Listener`, so `run_client` / `serve` are untouched.
pub struct FramingDialer<D, FF, const RC: usize, const WC: usize> { /* … */ }
pub struct FramingListener<L, FF, const RC: usize, const WC: usize> { /* … */ }

/// The §5.5 cell, in core so no connector hand-rolls one: `Send + Sync`
/// for any `T: Send`, with no `unsafe`.
pub struct OneShot<T> { /* spin::Mutex<Option<T>> */ }
```

All futures are return-position `impl Future … + Send`. That one decision is
what makes the design zero-cost; §5.1 explains why.

Three details the prototype settled:

- **No new error type.** The sketch had an `IoError`; the traits use the
  existing `TransportResult`/`TransportError`, so nothing converts at the
  `Connection` boundary. `IoError` remains as an alias.
- **`ByteStream` is unsplit** — one value with `&mut self` on both directions,
  not `Rd`/`Wr` halves like today's `EmbassyConnection`. That is what lets it
  wrap an owned `embassy_net::tcp::TcpSocket`, whose `split()` yields only
  *borrowed* halves while `Connection` must own the socket — the exact reason
  [`embassy_transport.rs`](../../aimdb-tcp-connector/src/embassy_transport.rs)
  gives for not reusing `connector-io` today. It costs nothing: `Connection`'s
  own `recv`/`send` already take `&mut self`, so reads and writes were already
  serialized.
- **`Datagram` grew two members** over the first sketch (`local_addr`, and the
  `DatagramBinder` beside it). Without them a unified KNX task would silently
  downgrade every Tokio deployment to the NAT-style HPAI and lose the
  engine's socket-reset. See §5.6.

### 3.2 Adapters

**[verified]** — `aimdb-tokio-adapter/src/net.rs` and
`aimdb-embassy-adapter/src/net.rs`, both behind a `net` feature.

- **`aimdb-tokio-adapter`**, new feature `net`: `TokioNet::tcp()` implementing
  `StreamDialer` over `tokio::net::TcpStream`, `TokioNet::listen(addr)` for
  `StreamListener`, `TokioNet::udp(bind)` for `Datagram`/`DatagramBinder`, and
  `Delay` over `tokio::time::sleep`. Every future is a plain `async fn` and the
  module contains **no `unsafe`** — the compiler discharges the `+ Send` the
  traits declare. Opening a serial device with `tokio-serial` stays in the
  serial connector under `std`; that dependency does not belong in the adapter.
- **`aimdb-embassy-adapter`**: `EmbassyNet::tcp(stack, rx, tx)`,
  `EmbassyNet::listen::<N>(stack, endpoint, rx[N], tx[N])` (the socket-slot pool
  moves here from [`aimdb-tcp-connector/src/embassy_transport.rs`](../../aimdb-tcp-connector/src/embassy_transport.rs)),
  `EmbassyNet::udp(stack, …)`, `EmbassyUart::split(rx, tx)`, and `Delay`
  returning `embassy_time::Timer`. Each stream/datagram newtype is
  `unsafe impl Send` and wraps the inner future in `SendFutureWrapper`. The
  `unsafe` stays exactly where design 033 put it. `NetStack` construction moves
  inside these constructors, so it leaves user and connector code.
- **A future `aimdb-freertos-adapter`** implements the same traits over lwIP or
  FreeRTOS+TCP. lwIP handles are plain integers, so its types are `Send`
  naturally and need no wrapper at all.

#### The accept pool: `StreamListener` is enough, but the pool must be reshaped

**[revised]** — §6 of the first draft said the N-socket pool "moves into the
adapter unchanged". It moves *reshaped*, and the reshaping is the point.

The worry was that a single-shot `accept(&mut self)` can only hold one socket
in `accept()`, so an `N`-socket pool driven by core's serial `serve` loop would
collapse to one pending listener. Reading `embassy-net` settles it:
`TcpSocket::accept` is a **synchronous** `s.listen(endpoint)` followed by a
bare `poll_fn` that waits for the state to leave
`Listen`/`SynSent`/`SynReceived`. Two consequences:

1. Dropping an `accept()` future does **not** un-listen the socket.
2. But *re-entering* `accept()` on a socket that is already listening is
   `InvalidState`, so a pool that rebuilds its futures each call must
   `abort()` first — and that abort is what takes the socket out of `LISTEN`.

`EmbassyNet::listen::<N>` therefore **stores** one accept future per slot and
consumes only the one that completes. The other `N-1` stay pending and
untouched, still in `LISTEN`. Nothing is ever cancelled, and no socket leaves
`LISTEN` between calls — strictly better than today's behaviour, which has an
`abort()`/re-`accept()` window.

`aimdb-tcp-connector/tests/accept_pool.rs` proves both halves over the two
crossover-wired `embassy-net` stacks the existing loopback smoke uses, driven
in exactly the shape `serve` accepts in:

| Test | Result |
|---|---|
| `pool_keeps_every_slot_listening_between_accepts` | A connects and round-trips; **then**, while the server sits between accepts, B dials, connects and round-trips |
| `naive_pool_loses_the_syn_that_arrives_between_accepts` | Same scenario against a rebuild-and-cancel pool: B's dial is refused with `TransportError::Io`, asserted |

The second is what makes the first a finding rather than a coincidence, and it
is a live regression guard: if embassy-net's cancellation semantics ever
change, it says the stored-accept design can be simplified.

#### Sockets and clock must stay separable features

**[verified]** — found by building, not reading. Making the adapter's `net`
feature imply `embassy-time` turns on the workspace's `defmt-timestamp-uptime`,
which defines `_defmt_timestamp` and collides with the `defmt::timestamp!`
every host test binary must supply (`duplicate symbol: _defmt_timestamp`). So
`net` covers sockets only, and `EmbassyDelay` is gated on `embassy-time`
separately.

### 3.3 Connectors

| Crate | Becomes | Deleted |
|---|---|---|
| `aimdb-tcp-connector` | `framing.rs` + `TcpClient::new(dialer)` / `TcpServer::new(listener)` generic over the traits | `tokio_transport.rs`, `embassy_transport.rs` |
| `aimdb-serial-connector` | `framing.rs` (the COBS codec + core `Framer` + one `ByteStream` per byte source) + sugar; the `tokio-serial` open helper stays under `std` | `embassy_transport.rs`, most of `tokio_transport.rs` |
| `aimdb-knx-connector` | `tunnel.rs` + one `connection_task<B: DatagramBinder, D: Delay>` | `tokio_client.rs`, `embassy_client.rs` |
| `aimdb-mqtt-connector` | One `MqttConnector` type with two backends: `Native` (`rumqttc`, `std`) and `Embedded<N: StreamDialer + Delay>` (`mountain-mqtt` over `handle_messages`, as `embassy_tls.rs` already does) | `embassy_client.rs`'s stack plumbing; `tokio_client.rs` shrinks to the backend |
| `aimdb-uds-connector` | Unchanged (std-only by nature); could be `FramedConnection<…, NdjsonFramer>` for uniformity | — |
| `aimdb-websocket-connector` | Unchanged (axum, std-only) | — |

**[verified] for serial.** `aimdb-serial-connector/src/framing.rs` is the
shape: one `CobsFramer` written against core's `Framer`, one `ByteStream` per
byte source, and core's `FramedConnection` doing the rest. It needs **no**
`aimdb-tokio-adapter` dependency — the manifest deliberately avoids one — and
no `tokio/net`: `tokio` for `io-util` alone is enough for a single generic impl
over `AsyncRead + AsyncWrite` that covers `SerialStream` and the
`tokio::io::duplex()` pipe the tests use alike. `tests/neutral_framed.rs` runs
it under `--features tokio-runtime` (pointedly *not* `_test-tokio`, so no
adapter is linked), covering round-trip, frame boundaries across a payload
larger than `WRITE_CHUNK`, EOF-on-close as `Ok(None)`, and the boxed
`dyn Connection` crossing a `tokio::spawn`. A companion type assertion
compiles the *same* `FramedConnection<_, CobsFramer, _, _>` over
`embedded-io-async` halves on `thumbv7em`, so "one connector module, no runtime
`cfg` on the code path" is enforced by the compiler rather than asserted.

**[verified] for KNX.** `aimdb-knx-connector/src/client.rs` is the single
`connection_task`, generic over `DatagramBinder + Delay`, compiling for
Embassy on `thumbv7em` and for Tokio from one body. Two tests hold it to the
contract that matters: `unified_task_is_boxable_as_the_runners_send_future`
(the `Pin<Box<dyn Future + Send + 'static>>` that `ConnectorBuilder::build`
returns) and `unified_task_advertises_the_real_local_endpoint`, which drives it
against a real UDP gateway and asserts the CONNECT_REQUEST's control HPAI
carries the socket's actual bound address rather than `0.0.0.0:0`.

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
[`connectors.rs:74`](../../aimdb-embassy-adapter/src/connectors.rs) already
uses this return-position shape, minus the `Send` bound.

The trait is not dyn-compatible. That is fine: connectors are generic
(`TcpServer<L>`, `KnxConnector<B, D>`), and the dyn boundary stays where it is
today, at `Box<dyn Connection>` per frame.

**[verified], and it applies one layer up too.** Both halves of the asymmetry
are now real code: the Tokio adapter's `ByteStream` impls are plain `async fn`s
the compiler discharges, and the Embassy adapter's return
`SendFutureWrapper(...)` over `!Send` socket futures.

The prototype also found a *second* instance of the same problem, which the
first draft missed: `TunnelIo::send` — the KNX connector's own three-method I/O
trait — is a bare `async fn`, so `drain_actions`, which is generic over
`impl TunnelIo`, produces a future that is not provably `Send`. A generic
connection task therefore cannot be boxed as the runner requires. A probe
reproduced it exactly, with rustc prescribing the fix verbatim:

```
error[E0277]: future cannot be sent between threads safely
help: `Send` can be made part of the associated future's guarantees for all
      implementations of `tunnel::TunnelIo::send`
-     async fn send(&mut self, frame: &[u8]) -> bool;
+     fn send(&mut self, frame: &[u8]) -> impl Future<Output = bool> + Send;
```

Applied, with `tunnel::tests::drain_actions_future_is_send_in_generic_code` as
the guard, and the two existing impls then split along exactly the predicted
line. The general rule: **every trait a generic connector task calls through
needs the bound on its return type**, not just the ones in §3.1. §11 carries
this as its own step.

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

**[revised] — on std this is a link-time obligation, not just a test detail.**
A std binary using `Channel<CriticalSectionRawMutex, _, N>` does not link:

```
rust-lld: error: undefined symbol: _critical_section_1_0_acquire
rust-lld: error: undefined symbol: _critical_section_1_0_release
```

and `NoopRawMutex` cannot substitute — it is `!Sync`
(`*mut () cannot be shared between threads safely`), so it cannot back a shared
channel at all. `CriticalSectionRawMutex` is the only `Sync` raw mutex
`embassy-sync` offers, so the choice is forced and the obligation comes with
it.

It is fully mitigable in one line, and the connector must carry it rather than
push it downstream: the KNX crate's `tokio-runtime` feature enables
`critical-section/std` itself, so no std user of the connector ever sees the
error. `client::tests::shared_embassy_channels_carry_telegrams_on_tokio` then
runs this section's claim rather than asserting it — the unified task on Tokio,
against a real UDP gateway, carrying a full handshake, an inbound telegram with
its ACK, and an outbound command, all through the same `embassy_sync` channel
types the MCU uses.

One caveat to state explicitly: the workspace `[patch.crates-io]`-es
`embassy-sync` to a local checkout, and that patch already blocks publishing
`aimdb-embassy-adapter` (008 §6). Putting `embassy-sync` on the std side of a
connector widens that constraint's reach. The KNX channel needs none of the
patched APIs, so a published build resolves the crates.io version — but it is
an assumption, not an accident.

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

**[verified]** — `embassy-futures` 0.1.2 has an empty `[dependencies]` section
beyond optional `defmt`/`log` (manifest read from the resolved registry copy),
and it is now an unconditional dependency of the KNX connector, driving the
unified loop on both runtimes with no effect on the std graph. Its `select3` is what the KNX Embassy loop uses now, and it
runs unchanged on Tokio. `futures_util::select_biased`, which core's client
engine already uses in `no_std`, is the other zero-cost option. Either works.

### 5.5 Moved-in peripherals: a `Mutex<Option<L>>` in core

`ConnectorBuilder: Send + Sync` with `build(&self)` means a moved-in listener
must be taken through interior mutability. `spin::Mutex<Option<L>>` with
`L: Send` is `Send + Sync` without `unsafe`, replacing the adapter's
`OneShotCell` and its `unsafe impl Sync`. Taken once at build; no cost after.

**[verified]** — it lives in core as `session::OneShot<T>`, so no connector
hand-rolls one, and `spin` was already an `aimdb-core` dependency, so the
mechanism costs nothing to adopt. A compile-time assertion in core holds
`OneShot<T>: Send + Sync` for `T: Send`, so a regression surfaces there rather
than in a connector.

**The `T: Send` bound is the whole contract, and it bites.** `TlsOptions`
carries `rng: &'static mut dyn CryptoRngCore` — a trait object with no `Send`
bound, so the struct is `!Send`, so `OneShot` cannot hold it, so
`aimdb-mqtt-connector` cannot drop the `unsafe impl Send`/`Sync` on its
`TlsSlot`. That collides head-on with §6's promise that "`TlsOptions` keeps its
signature" and with acceptance criterion 2. A type assertion on the real struct
confirmed it rather than leaving it to argument:

```
error[E0277]: `(dyn CryptoRngCore + 'static)` cannot be sent between threads safely
note: required because it appears within the type `TlsOptions`
```

**[revised]** — the signature is what gives. The fix is one `+ Send` on the
trait object (`&'static mut (dyn CryptoRngCore + Send)`), which every concrete
CSPRNG a caller passes already satisfies; the whole `embassy-tls` path still
cross-compiles to `thumbv7em`, so `embedded-tls` accepts it. With that,
`TlsSlot` becomes an alias for `OneShot<TlsOptions>` and
**`aimdb-mqtt-connector` carries zero `unsafe impl`s** (was two). §6 is amended
accordingly: `TlsOptions` keeps its *shape*, and gains a `Send` bound on its
RNG.

This is a **public, breaking** change to `TlsOptions::new`, whose only call
site is `examples/embassy-mqtt-connector-demo` under `--features tls` — a
configuration `make examples` does **not** build (it builds default features
only), so CI will not catch a break there. The demo passes
`&'static mut Rng<'static, peripherals::RNG>`, and embassy peripheral types
should be `Send`, so no source change is expected; it wants either a human
check on hardware or a CI job that builds that feature.

The rule generalises: wherever a connector today reaches for `unsafe impl Send`
on a moved-in resource, `OneShot` will refuse it, and the refusal is the signal
to fix the type rather than to force the bound.

### 5.6 Sockets that must be rebound: `DatagramBinder`, not a moved-in socket

**[revised]** — the first draft handed the KNX task a `Datagram`. It cannot be
one: both hand-written clients drop and re-create the UDP socket on every
reconnect cycle, because that is what the engine's `Action::ResetSocket` means.
A moved-in socket has nothing to rebind.

So the task takes a `DatagramBinder` and binds per cycle. Both runtimes
implement it without contortion: Tokio binds a fresh `UdpSocket`; Embassy
`close()`s and re-`bind()`s the *same* socket, since `embassy_net::udp::UdpSocket`
owns its buffers for its whole lifetime and recreating it would strand them.

The same section covers `local_addr` (§3.1). The Tokio half reads it after bind
and feeds `LocalEndpoint::Explicit`, which gateways that reject the NAT-style
`0.0.0.0:0` HPAI require; unifying without it would have silently downgraded
every Tokio deployment. Embassy can answer it too — the socket's bound port
plus the stack's IPv4 config — so unification *fixes* the Embassy half, which
never set the explicit endpoint at all.

### 5.7 Cost table, revised

| Cost in the first cut | Replacement | MCU delta vs today |
|---|---|---|
| Box per read chunk | `impl Future + Send` in the trait | None; per-frame box unchanged |
| `async-channel` | `embassy_sync::channel::Channel<CriticalSectionRawMutex, …>` | None; KNX already uses it |
| `RuntimeOps::sleep` per iteration | `Delay` returning `embassy_time::Timer` | None |
| `futures_util::select_biased` | `embassy_futures::select` or keep | None |
| `OneShotCell` `unsafe impl Sync` | `session::OneShot<T>` in core | None after build |
| Force-`Send` | `SendFutureWrapper` inside adapter impls | None; transparent newtype |

Two costs the prototype added to the ledger, neither on the MCU:

| Cost | Where | Delta |
|---|---|---|
| `critical-section` impl must be linked | std binaries only (§5.2) | None on MCU — the HAL already provides one |
| One pending accept future per idle socket slot | Embassy TCP listener (§3.2) | `N` small futures held in the listener instead of rebuilt per call; no heap churn, and it removes the `abort()`/re-`accept()` window |

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
- **TLS time source.** `TlsOptions` keeps its shape — gaining only a `Send`
  bound on its RNG, per §5.5 — and `embedded-tls` layers over any `ByteStream`. The SNTP task becomes generic over `Datagram`,
  but the wall-clock anchor it sets (`EmbassyAdapter::set_unix_time`) has no
  neutral home yet, so the `mqtts://` path stays Embassy-only until it does.
- **Embassy TCP accept pool.** The N-socket slot recycling moves into the
  adapter — **reshaped**, not unchanged: it stores one pending accept per slot
  rather than rebuilding them per call (§3.2). Either way it is the adapter's
  problem to solve, not the connector's.
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
- **The TRNG must be `Send`.** `TlsOptions::new` now takes
  `&'static mut (dyn CryptoRngCore + Send)` (§5.5). The call above is unchanged
  textually — `embassy_stm32::rng::Rng` satisfies it — but a caller passing an
  RNG that does not will see the error at this line rather than a stray
  `unsafe impl` deep in the connector, which is the point.
- **The serial UART goes through the adapter.** `SerialServer::new(rx, tx)`
  becomes `SerialServer::new(EmbassyUart::split(rx, tx))`, so the connector
  names no `embedded-io-async` halves of its own.

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

**[revised]** — the first draft said "no tool or library crate constructs these
connectors". [`aimdb-codegen`](../../aimdb-codegen/src/rust.rs) does: it emits
`MqttConnector::new(&mqtt_url)` (lines 268, 1421) and
`KnxConnector::new(&knx_gateway)` (lines 269, 1424) into generated user code.
MQTT is unaffected — the `Native` backend keeps that signature — but **KNX is
not**: once `KnxConnector` is generic over `DatagramBinder + Delay`, the
generated line must carry a transport. So `aimdb-codegen` changes, and
`make codegen-drift` must be re-baselined.

The remaining call sites are examples and two crate-local demos:

| Site | Change |
|---|---|
| `embassy-knx-connector-demo`, `embassy-mqtt-connector-demo`, `embassy-serial-connector-demo` | one line each: the stack goes to the adapter |
| `weather-station-gamma` (Embassy MQTT) | one line |
| `tokio-knx-connector-demo` | gains a transport argument |
| `tokio-mqtt-connector-demo`, `weather-hub`, `weather-station-alpha`, `weather-station-beta` | **no change** — the Tokio MQTT path is untouched |
| [`aimdb-tcp-connector/examples/tcp_demo.rs`](../../aimdb-tcp-connector/examples/tcp_demo.rs), [`aimdb-serial-connector/examples/serial_demo.rs`](../../aimdb-serial-connector/examples/serial_demo.rs) | the string-sugar change below — these are the two the first draft's list omitted |
| `aimdb-knx-connector/tests/topic_provider_tests.rs`, `aimdb-mqtt-connector/tests/{link_ext,topic_provider}_tests.rs` | instantiation only |

The Embassy sites already pass a stack and static buffers explicitly, so
wrapping those in an adapter call is a one-line change. The Tokio TCP/serial
string sugar (`TcpServer::new("127.0.0.1:7001")`) becomes
`TcpServer::new(TokioNet::listen("127.0.0.1:7001").await?)`. Preserving the
string form would require the connector to depend on `aimdb-tokio-adapter`,
which the serial connector's manifest deliberately avoids today; accept the
one-line change instead.

Two API changes beyond the constructors, both covered above: `TlsOptions::new`
gains `+ Send` on its RNG (§5.5, public and breaking), and `TunnelIo::send`
gains `+ Send` on its return type (§5.1, `pub(crate)`, so invisible outside
the crate).

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

Two of the first draft's six were unsound as written; both are corrected here.

1. `cargo check -p aimdb-core --no-default-features --features alloc,connector-session,remote --target thumbv7em-none-eabihf`
   passes with the new traits and `FramedConnection`; core still contains zero
   `unsafe`. **[verified] — met.**
2. **[revised]** Every `unsafe impl Send`/`Sync` **in the connector crates and
   the two adapters** stays inside `aimdb-embassy-adapter`; the connector
   crates contain none. The original wording said "in the workspace", which is
   false today and after the change: `aimdb-wasm-adapter` carries 16
   (`buffer.rs` 9, `ws_bridge.rs` 4, `time.rs` 3) and `aimdb-bench/src/alloc.rs`
   one, none of them in scope here. Progress: MQTT is at zero (§5.5), serial was
   already at zero, and the TCP connector's remaining three live in the module
   this design deletes.
3. `aimdb-tcp-connector`, `aimdb-serial-connector` and `aimdb-knx-connector`
   have no `tokio_*` / `embassy_*` source modules and no `embassy-net` or
   `embassy-time` dependency. The serial connector keeps `tokio-serial` for its
   open helper **and `tokio` for `io-util`** — one generic `ByteStream` impl
   over `AsyncRead + AsyncWrite`, with no `tokio/net` and no adapter
   dependency (§3.3). The KNX connector keeps `embassy-sync` and
   `embassy-futures`, which are executor-independent by §5.2/§5.4.
4. The existing host tests pass unchanged in behaviour: TCP loopback
   (`_test-embassy-loopback`), serial duplex-pipe tests, the KNX fake-gateway
   tests, and the MQTT `build_internal` tests, each now instantiated through
   the adapter's transport types. Note the MQTT ones are Tokio-only unit tests
   inside `tokio_client.rs`; **neither Embassy MQTT path has a host test at
   all**, which is why criterion 6 matters more than it looks.
5. **[revised]** The original — "`examples/embassy-bench-stm32h5` shows no
   change in cycles per message or allocations after `build()`" — is vacuous:
   that example depends only on `aimdb-core`, `aimdb-embassy-adapter` and
   `aimdb-bench`, with no connector crate in its graph, so it will show no
   change whatever this design does. Replace it with an allocation-counting
   test that actually exercises a connector path, asserting zero allocations
   per message after `build()`.
6. The embedded MQTT backend builds and connects on the host over
   `TokioNet::tcp()` (the parity line in §7.1). This is the only host coverage
   either Embassy MQTT path would ever get, so **build it first**, not last
   (§11).

## 11. Sequencing

Steps 1, 2 and the load-bearing parts of 4 are prototyped; the rest is not.
Effort, recalibrated against that prototype, is in
[052 — Verification](052-verification.md) §6: **4–6 weeks** for one engineer
fluent in these crates, with the risk now concentrated almost entirely in
step 6.

| # | Step | State |
|---|---|---|
| 1 | Core: the §3.1 traits, `FramedConnection`, `FramingDialer`/`FramingListener`, `OneShot`. Lift the `Framer` and framed connection out of the adapter's `connector-io` module. | **done** |
| 2 | Adapters: `TokioNet` (feature `net`) and `EmbassyNet`/`EmbassyUart`, moving the Embassy TCP slot pool in and reshaping it per §3.2. Keep the old spine types for one release with deprecation notes. | **done** |
| 3 | Add `+ Send` to the return type of every trait a generic connector task calls through — `TunnelIo::send` is the one the audit missed (§5.1). Cheap, but it gates step 5. | **done** |
| 4 | TCP connector as the pilot: it exercises every new trait except `Datagram`. Migrate `embassy_loopback` onto the adapter's transports. | not started; the adapter side it depends on is done |
| 5 | Serial, then KNX (adds `Datagram`, `DatagramBinder` and `Delay`). | framing, streams and the unified KNX task **done**; connector sugar, channel wiring, and deleting the four runtime modules remain |
| 6 | MQTT: split into `Native` and `Embedded<N>` backends; route the plain embedded path through `handle_messages`. **Build acceptance 6 first** — it is the only host coverage this path will have, and moving off `run_with_subscriptions` means re-implementing the reconnect-and-resubscribe loop `mountain-mqtt-embassy` owns today, which is behaviour, not plumbing. | not started |
| 7 | Examples, `aimdb-codegen` + `make codegen-drift` re-baseline (§8), design 012, CHANGELOGs; connector crates bump major. | not started |
| 8 | Then the FreeRTOS adapter, per design 051 §8 step 3, with the network connectors coming along for free. | unblocked once 1–2 land |

Steps 1–3 are purely additive — nothing consumes the new code yet — so they
carry no regression risk and can land ahead of the connector work.
