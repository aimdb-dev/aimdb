# Changelog - aimdb-mcp

All notable changes to the `aimdb-mcp` MCP server will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

No changes in this release.

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

[Unreleased]: https://github.com/aimdb-dev/aimdb/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/aimdb-dev/aimdb/releases/tag/v0.1.0
