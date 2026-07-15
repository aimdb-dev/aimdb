# Changelog - aimdb-tcp-connector

All notable changes to the `aimdb-tcp-connector` crate will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- **New crate — the length-prefixed TCP transport for AimDB remote access (AimX over TCP, refs #121).** Contributes the `Dialer`/`Listener`/`Connection` transport triple plus thin `TcpClient`/`TcpServer` sugar; the AimX codec + dispatch and the runtime-neutral session engines (`run_client`/`serve`) are reused from `aimdb-core`. Every AimX envelope is framed as a `u32` big-endian length prefix. Two runtime halves:
  - **`tokio-runtime`** (std, host/gateway) — TCP transport over `tokio::net`.
  - **`embassy-runtime`** (`no_std + alloc`, MCU) — an explicit pool of caller-buffered `embassy-net` sockets, one accept/session worker per slot (`TcpServer::<N>::with_buffers`), with socket recycling across reconnects. A synchronous `accept()` failure (e.g. a port-0 endpoint rejected as `InvalidPort`) yields instead of spinning the cooperative executor.
- **Embassy TCP runtime smoke test** (`_test-embassy-loopback`) — exercises the socket pool over two real `embassy-net` stacks wired by an in-memory driver-channel crossover: recycle → re-accept, concurrent accept slots, and dialer redial after a failed connect / dropped link.
