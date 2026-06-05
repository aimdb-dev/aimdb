# Changelog - aimdb-serial-connector

All notable changes to the `aimdb-serial-connector` crate will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- **New crate — the COBS-framed serial/UART transport for AimDB remote access (Issue #122, follow-up to #39, [doc 041](../docs/design/detailed/041-phase6-embedded-transports.md)).** The serial sibling of `aimdb-uds-connector`: it contributes only the `Dialer`/`Listener`/`Connection` triple plus thin sugar; the AimX codec + dispatch and the runtime-neutral session engines (`run_client`/`serve`) are reused from `aimdb-core`. The wire is the same compact AimX JSON, framed with **COBS** (Consistent Overhead Byte Stuffing) and a `0x00` sentinel instead of a newline — self-synchronizing on a lossy/unframed serial medium, so a receiver that joins mid-stream resynchronizes on the next sentinel. Default scheme `"serial"`. Two runtime halves:
  - **`tokio-runtime`** (std, host/gateway) — `TokioSerialConnection<S>` over any `AsyncRead + AsyncWrite` (a real `tokio_serial::SerialStream` in production, a `tokio::io::duplex()` in tests), with `SerialClient::new(path, baud)` (sugar over `SessionClientConnector<SerialDialer, AimxCodec>`) and `SerialServer` (sugar over `SessionServerConnector` + `AimxDispatch`). The listener is one-shot (serial is point-to-point).
  - **`embassy-runtime`** (`no_std + alloc`, MCU) — `EmbassySerialConnection<Rd, Wr>` generic over `embedded-io-async` `Read`/`Write` halves (the common `Uart::split()` shape), with `SerialClient`/`SerialServer` that hand-roll `ConnectorBuilder` (calling `run_client`/`pump_client`/`serve` directly) and force-`Send` the single-core Embassy futures via `aimdb-embassy-adapter`'s `SendFutureWrapper`. The Embassy *server* half rides the `no_std` `AimxDispatch` landed in #120, so an MCU can answer `record.list`/`get`/`set`/`subscribe`/`drain` over a UART; the *client* half mirrors records to a gateway. Reconnect is disabled by default on Embassy (the UART peripheral is moved in and can't be re-acquired).
- **`framing` module** — the shared COBS frame codec (`encode_frame` + a chunk-tolerant `FrameAccumulator`), pure `no_std + alloc`, so the round-trip is unit-tested independent of any transport.
- **Examples — a real end-to-end serial test.** `examples/serial_demo.rs` (host, `--features _test-tokio`): an AimX client/server over a device path (a board's ST-LINK VCP at `/dev/ttyACM0`, or a `socat` PTY pair). `examples/embassy-serial-connector-demo/` (board): an STM32H563ZI Nucleo serving the `counter` record over USART3 ↔ the ST-LINK Virtual COM Port — the no_std `SerialServer` + `AimxDispatch` on real silicon, flashed via `probe-rs`, queried from the host over the wire.

### Notes

- **The Embassy half sends each frame in ring-sized (64-byte) chunks.** A HAL
  `BufferedUart::write` is atomic-or-error (`embassy-stm32` returns `BufferTooLong`
  for a single write larger than its TX ring), so a frame bigger than the buffer —
  e.g. a `record.list` reply — would otherwise fail the whole send and drop the
  session. Chunking sends a frame of any length given a TX buffer ≥ 64 bytes.
  (Validated end-to-end on an STM32H563ZI: `record.list`/`record.get`/streaming all
  round-trip over the ST-LINK VCP.)
- **`embedded-io-async` is pinned to 0.7** (the workspace dep was bumped 0.6 → 0.7) so a HAL `BufferedUart` (e.g. `embassy-stm32`, which uses 0.7) satisfies the connector's `Read`/`Write` bounds without a trait-version skew. Wire the Embassy half from a `BufferedUart::split()` — the plain async `UartRx` does **not** implement `embedded-io-async::Read`, only the buffered/ring-buffered variants do.

- The `unsafe impl Send`/`Sync` on the Embassy transport + builder types rest on the single-core, cooperative Embassy-executor invariant documented by `SendFutureWrapper` (no preemption / thread migration). This is the first *raw-peripheral* session connector — MQTT/KNX sidestep it by pulling a `Send + Sync` `embassy_net::Stack` from the runtime adapter rather than owning a peripheral.
- The `serial://` scheme constant lands here; the `connect_url` / `--connect <url>` resolver that maps a `serial:///dev/ttyUSB0?baud=115200` URL to `SerialClient`/`SerialServer` is tracked separately (Issue #123).
- The internal `_test-tokio` feature gates the host tokio integration test's adapter dependency; it is kept off the public `tokio-runtime` feature (a connector shouldn't pull a concrete adapter) and out of `[dev-dependencies]` (an unconditional tokio adapter would force `aimdb-core/std` into the `no_std` embassy test build).
