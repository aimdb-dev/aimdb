# Changelog - aimdb-tcp-connector

All notable changes to the `aimdb-tcp-connector` crate will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Changed

- **One path for both runtimes (breaking).** `TcpServer::new` takes an
  already-bound listener from an adapter (`TokioNet::listen`,
  `EmbassyNet::listen::<N>`) instead of a bind string, and `TcpClient::new`
  takes a dialer. `tokio_transport` and `embassy_transport` are deleted with the
  whole `Tokio*`/`Embassy*` alias set, and with them the crate's last three
  `unsafe impl`s. The library no longer depends on `tokio`, `embassy-net`,
  `embassy-futures` or `embedded-io-async`.

### Added

- **`connector` — runtime-neutral `TcpClient`/`TcpServer`** over core's
  `StreamDialer`/`StreamListener`, plus `framing::LengthFramer` against core's
  `Framer`. A length prefix has no delimiter to resync on, so `LengthFramer`
  reports a bad header as `FrameFault::Fatal` and the connection closes instead
  of reading on; an oversized outbound frame is still dropped whole rather than
  written half-encoded, but is now reported rather than silently discarded.
  The frame cap is settable again: `LengthFramers` is a `FramerFactory` holding
  `max_frame`, where the `fn() -> LengthFramer` it replaces was stateless and
  could only ever produce `DEFAULT_MAX_FRAME`. Reach it through
  `TcpServer::max_frame(n)`, `TcpClient::bounded(..)`, `framed_dialer_bounded`
  or `framed_listener_bounded`; the un-suffixed constructors keep the 64 KiB
  default. It bounds what one connection can make the receiver buffer, which is
  a memory limit on an MCU and a DoS limit on an exposed port.
  `split_host_port` and `framed_dialer_at` carry the `host:port` grammar and
  are fallible, returning `EndpointError`. Brackets are what let an IPv6
  literal carry a port, so an unbracketed one (`fe80::1`, `2001:db8::dead:beef`)
  keeps every colon as address and takes the default port rather than having its
  last group read as one. A port that is written but is not a number in
  `0..=65535` is rejected instead of falling back to `DEFAULT_PORT`, which would
  dial a different — possibly live — service.
- **`tests/accept_pool.rs`** — the adapter's pooled `StreamListener` over two
  crossover-wired `embassy-net` stacks, with a rebuild-and-cancel pool as the
  negative control: it loses a SYN arriving between accepts, the stored-accept
  pool does not.
- **New crate — the length-prefixed TCP transport for AimDB remote access (AimX over TCP, refs #121).** Contributes the `Dialer`/`Listener`/`Connection` transport triple plus thin `TcpClient`/`TcpServer` sugar; the AimX codec + dispatch and the runtime-neutral session engines (`run_client`/`serve`) are reused from `aimdb-core`. Every AimX envelope is framed as a `u32` big-endian length prefix. Two runtime halves:
  - **`tokio-runtime`** (std, host/gateway) — TCP transport over `tokio::net`.
  - **`embassy-runtime`** (`no_std + alloc`, MCU) — an explicit pool of caller-buffered `embassy-net` sockets, one accept/session worker per slot (`TcpServer::<N>::with_buffers`), with socket recycling across reconnects. A synchronous `accept()` failure (e.g. a port-0 endpoint rejected as `InvalidPort`) yields instead of spinning the cooperative executor.
- **Embassy TCP runtime smoke test** (`_test-embassy-loopback`) — exercises the socket pool over two real `embassy-net` stacks wired by an in-memory driver-channel crossover: recycle → re-accept, concurrent accept slots, and dialer redial after a failed connect / dropped link.
