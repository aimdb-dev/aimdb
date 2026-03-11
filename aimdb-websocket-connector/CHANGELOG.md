# Changelog

All notable changes to `aimdb-websocket-connector` will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

No changes yet.

## [0.1.0] - 2026-03-11

### Added

- Initial release of the AimDB WebSocket connector
- **Server mode** (Axum-based): accept incoming WebSocket connections via `link_to("ws://topic")`
  - Configurable bind address, path, and late-join support
  - Client session management with automatic cleanup
  - `AuthHandler` trait for pluggable authentication
- **Client mode** (tokio-tungstenite): connect to remote WebSocket servers via `link_to("ws-client://host/topic")` and `link_from("ws-client://host/topic")`
  - AimDB-to-AimDB sync without intermediary broker
  - Automatic reconnection
- Shared wire protocol via `aimdb-ws-protocol`
- `WebSocketConnector` builder API
