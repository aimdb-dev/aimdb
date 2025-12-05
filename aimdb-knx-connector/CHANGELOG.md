# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Changed

- **Breaking: Record Registration**: Updated demo examples to use new key-based `configure<T>(key, |reg| ...)` API
- All demo records now have explicit keys (e.g., `"lights.state"`, `"temp.livingroom"`, `"lights.control"`)

## [0.1.0] - 2025-11-20

### Added
- Initial implementation of KNX/IP connector
- Dual runtime support (Tokio and Embassy)
- KNXnet/IP Tunneling protocol support
- Inbound monitoring (KNX bus → AimDB records)
- Outbound control (AimDB records → KNX bus)
- Group address parsing (3-level format)
- DPT type support via knx-pico integration
- Automatic reconnection on connection loss (5s interval)
- **ACK timeout handling** with 3-second timeout for outbound telegrams
- **Heartbeat/keepalive** (CONNECTIONSTATE_REQUEST every 55s)
- **Comprehensive unit tests** (group addresses, frames, connection state)
- **Production deployment guide** in README.md
- `tokio-knx-connector-demo` example with bidirectional control
- `embassy-knx-connector-demo` example for embedded systems

### Fixed
- Proper sequence number tracking for ACK validation
- TUNNELING_ACK detection and processing
- Pending ACK cleanup on timeout

### Known Limitations
- No KNX Secure support (plaintext only)
- No group address discovery
- Fire-and-forget publishing (no bus-level confirmation)
- Single connection per gateway instance
- No routing mode support

## [0.1.0] - 2025-11-20

Initial beta release for production evaluation.

[Unreleased]: https://github.com/aimdb-dev/aimdb/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/aimdb-dev/aimdb/releases/tag/v0.1.0
