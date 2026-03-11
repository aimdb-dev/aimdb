# Changelog

All notable changes to `aimdb-ws-protocol` will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

No changes yet.

## [0.1.0] - 2026-03-11

### Added

- Initial release of the shared WebSocket wire protocol
- `ServerMessage` enum: `Data`, `Snapshot`, `Subscribed`, `Error`, `Pong`, `QueryResult`
- `ClientMessage` enum: `Subscribe`, `Unsubscribe`, `Write`, `Ping`, `Query`
- JSON-encoded wire format with `"type"` discriminant tag
- MQTT-style wildcard topic matching (`#` multi-level, `*` single-level)
- Timestamp-tagged data messages for ordering
- Late-join snapshot support
