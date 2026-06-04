# Changelog - aimdb-uds-connector

All notable changes to the `aimdb-uds-connector` crate will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- **New crate — the Unix-domain-socket transport for AimDB remote access (Issue #39, [design doc](../docs/design/remote-access-via-connectors.md)).** A thin, swappable transport that rides the shared session engine in `aimdb-core::session`: it contributes only the `Dialer`/`Listener`/`Connection` triple (`UdsDialer` / `UdsListener` / `UdsConnection`, NDJSON framing in the transport — one line per logical frame); the AimX codec + dispatch and the engine wiring are reused verbatim from core. Two ergonomic constructors:
  - **`UdsServer`** — accepts connections and serves the full AimX toolset over a socket. Register it via `with_connector` to stand up remote access (this replaces `aimdb-core`'s removed `AimDbBuilder::with_remote_access(config)`). Sugar over `SessionServerConnector`; `UdsServer::from_config(config)` is the one-line migration, plus `new`/`max_connections`/`max_subs_per_connection`/`socket_permissions`/`scheme` builders. Binds synchronously (remove-stale → `bind` → `set_permissions`) so bind errors surface from `build()`, and applies the security policy's writable-record marking.
  - **`UdsClient`** — dials a peer over UDS and mirrors records under a scheme (default `"uds"`), using `link_to`/`link_from` like any data-plane connector. Sugar over `SessionClientConnector<UdsDialer, AimxCodec>`; chain `.scheme(...)` / `.with_config(...)`.

### Changed (breaking)

- **`UdsServer::new` takes `impl Into<String>` (was `impl Into<PathBuf>`) (Issue #120).** Follows `aimdb-core`'s de-std of `AimxConfig.socket_path` (`PathBuf` → `String`) for the `no_std` AimX server. `&str` / `String` callers are unaffected; callers passing a `&Path` / `PathBuf` must convert (e.g. `path.to_str().unwrap()`). The internal bind now goes through `std::path::Path::new(&config.socket_path)`, so the synchronous remove-stale → `bind` → `set_permissions` behavior is unchanged.