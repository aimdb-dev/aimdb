# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added
- Initial implementation of KNX/IP connector
- Dual runtime support (Tokio and Embassy)
- KNXnet/IP Tunneling protocol support
- Inbound monitoring (KNX bus → AimDB records)
- Outbound control (AimDB records → KNX bus)
- Group address parsing (3-level format)
- DPT type support via knx-pico integration
- Automatic reconnection on connection loss
- `tokio-knx-demo` example
- `embassy-knx-demo` example (planned)

## [0.1.0] - TBD

Initial release.
