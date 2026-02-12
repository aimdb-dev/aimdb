# Changelog - aimdb-mcp

All notable changes to the `aimdb-mcp` MCP server will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- **Graph Introspection Tools**: Three new MCP tools for dependency graph exploration
  - `graph_nodes`: Get all graph nodes with origin, buffer type, tap count, outbound status
  - `graph_edges`: Get all directed edges showing data flow between records
  - `graph_topo_order`: Get record keys in topological (spawn/initialization) order
- **Record Drain Tool**: `drain_record` tool for batch history access
  - Drains accumulated values since last call (destructive read)
  - Supports optional `limit` parameter
  - Cold-start semantics: first drain creates reader and returns empty
  - Uses persistent drain client for value accumulation between calls

### Removed

- **Subscription System**: Removed complex real-time subscription infrastructure in favor of simpler drain-based polling
  - Removed `subscribe_record`, `unsubscribe_record`, `list_subscriptions`, `get_notification_directory` tools
  - Removed `SubscriptionManager` and `NotificationFileWriter` modules
  - Removed subscription-related prompts
  - Simplified architecture: LLMs use `drain_record` for batch analysis instead of streaming subscriptions

### Changed

- **Simplified Architecture**: MCP server now focused on introspection and batch analysis
- **Connection Pool**: Added persistent drain client management for proper cold-start semantics

## [0.4.0] - 2025-12-25

### Changed

- **Dependency Update**: Updated `aimdb-client` and `aimdb-core` dependencies to 0.4.0

## [0.3.0] - 2025-12-15

### Changed

- **RecordMetadata Updates**: MCP server now exposes `record_id` and `record_key` fields in all record metadata responses
- Tools and resources updated to display RecordId and RecordKey for improved record identification
- Schema inference and introspection tools benefit from key-based identification

## [0.2.0] - 2025-11-20

### Changed

- **Internal: Field Rename**: Updated internal usage of `RecordMetadata` to use `outbound_connector_count` field (renamed from `connector_count`) in tools and resources
- Updated record listing, schema inference, and metadata display to reflect new field name

## [0.1.0] - 2025-11-06

### Added

- Initial release of Model Context Protocol (MCP) server for AimDB
- LLM-powered introspection and debugging capabilities
- Discover running AimDB instances via socket scanning
- Query record values and metadata
- Subscribe to live record updates with configurable sample limits
- Set writable record values
- JSON Schema inference from record values
- Notification persistence to JSONL files for subscription data
- Instance information retrieval (version, protocol, permissions)
- Subscription management (list active subscriptions, unsubscribe)
- Get notification directory for saved subscription data

### Features

- **Tools**: 10 MCP tools for database introspection
- **Prompts**: Helper prompts for subscription workflows
- **Resources**: Dynamic resource generation for records
- **Persistence**: Automatic JSONL file creation for subscriptions

---

[Unreleased]: https://github.com/aimdb-dev/aimdb/compare/v0.4.0...HEAD
[0.4.0]: https://github.com/aimdb-dev/aimdb/compare/v0.3.0...v0.4.0
[0.3.0]: https://github.com/aimdb-dev/aimdb/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/aimdb-dev/aimdb/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/aimdb-dev/aimdb/releases/tag/v0.1.0
