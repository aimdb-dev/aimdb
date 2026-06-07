# Changelog - aimdb-mcp

All notable changes to the `aimdb-mcp` MCP server will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Changed (breaking)

- **Tools take an `endpoint` (a `scheme://` URL), not a `socket_path` (Issue #123).** Every tool's `socket_path` parameter is renamed `endpoint` and now accepts any endpoint URL — `unix://PATH`, `serial://DEVICE?baud=N` (with the `transport-serial` feature), or a bare path (the `unix://` shorthand). The startup `--socket <PATH>` flag is now `--connect <ENDPOINT>`, and the `AIMDB_SOCKET` env var is now `AIMDB_CONNECT` (resolution order: explicit `endpoint` → `--connect` → `AIMDB_CONNECT`). The connection pool is keyed by endpoint URL, and public-mode SSRF stripping now strips `endpoint`. `get_instance_info`'s result field `socket_path` is renamed `endpoint`; `discover_instances` (Unix-socket discovery) keeps `socket_path`. New `transport-serial` feature (off by default; pulls libudev) adds the serial transport to the resolver.

### Changed

- **`list_records`, the `records` resource, and `query_schema` no longer emit `created_at` / `last_update` (Issue #120).** `aimdb-core` dropped per-record timestamp tracking from `RecordMetadata` (it kept no wall-clock state for the `no_std` AimX server), so the AimX `record.list` payload — and these MCP outputs derived from it — no longer carry those two fields. The `RecordInfo` struct drops them too. Every other record-metadata field is unchanged.
- **Migrated to the engine-based `aimdb-client::AimxConnection` (Issue #39).** The connection pool and every tool now use `AimxConnection` instead of the retired `AimxClient`, speaking the reshaped **AimX-v2** protocol. Internal-only; no tool surface or behavior change (drain clients are still pooled per socket behind an `Arc<Mutex<AimxConnection>>`).

## [0.8.0] - 2026-05-22

### Added

- **Stage profiling tools** (Issue #58): Two new MCP tools surfaced from the `aimdb-core` `profiling` feature (enabled here on the `aimdb-core` dependency so the server always understands the `stage_profiling` metadata field).
  - `get_stage_profiling` — per-stage wall-clock timing for `.source()` / `.tap()` / `.link()` callbacks of records matching a `record_key` substring. Returns per-stage `call_count` / `avg_time_ns` / `min_time_ns` / `max_time_ns` plus a `bottleneck` pointing at the stage with the highest average, with a human-readable `recommendation` string. Unnamed stages appear as `source[0]`, `tap[0]`, etc.; names set via `.with_name("...")` on the registrar surface as the `name` field.
  - `reset_stage_profiling` — clears profiling counters for every record on the target instance. Issues the `profiling.reset` AimX request via `aimdb-client`; requires write permission. Falls back to a friendly `{ "reset": false, "message": ... }` response when the target was built without the `profiling` feature.
- **Public mode** (`--public` flag): Restricts the server to read-only tools (`discover_instances`, `list_records`, `get_record`) for safe internet-facing deployments. Suppresses resources and prompts capabilities. Strips client-supplied `socket_path` arguments to prevent SSRF.
- **Default socket flag** (`--socket <PATH>`): Sets a default socket path at startup, removing the need for clients to pass `socket_path` on every call. Resolution order: explicit arg → `--socket` → `AIMDB_SOCKET` env var.
- **CLI argument parsing**: Added `clap` dependency for structured CLI flags.
- **Comprehensive tests**: Public mode tool filtering, tool rejection, socket stripping, and capability suppression.

## [0.7.0] - 2026-03-24

### Changed

- **Optional `socket_path`**: All tools now accept `socket_path` as optional, falling back to `AIMDB_SOCKET` environment variable when omitted. Reduces repetition when working with a single instance.

## [0.6.1] - 2026-03-21

### Fixed

- **list_records Tool**: Added missing `record_key` field to `RecordInfo` response, making it easier to identify which key to use when calling `get_record` or `set_record`

## [0.6.0] - 2026-03-11

### Added

- **Architecture Agent**: Full design-time architecture agent with session state machine (`Idle → Gathering → Proposing → Resolve`)
  - `ArchitectureState` management with file-locking and `.aimdb/state.toml` persistence
  - Conflict detection and resolution module
- **Architecture MCP Tools** (16+ new tools):
  - `propose_add_record` — Add a record to the architecture proposal
  - `propose_add_connector` — Add a connector to the proposal
  - `propose_modify_buffer` — Modify buffer configuration
  - `propose_modify_fields` — Modify record fields
  - `propose_modify_key_variants` — Modify record key variants
  - `remove_record` — Remove a record from architecture
  - `rename_record` — Rename a record
  - `reset_session` — Reset the architecture session
  - `resolve_proposal` — Accept or reject a proposal, triggering codegen
  - `save_memory` — Persist architecture decisions
  - `validate_against_instance` — Validate architecture against a running AimDB instance
  - `get_architecture` — Get current architecture state
  - `get_buffer_metrics` — Get buffer performance metrics
- **Architecture MCP Resources**: Mermaid diagram and validation results as MCP resources
- **Architecture Prompts**: Updated prompts module with architecture agent guidance
- `CONVENTIONS.md` asset for architecture agent prompt context
- New dependencies on `aimdb-codegen` and `toml`

## [0.5.0] - 2026-02-21

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

[Unreleased]: https://github.com/aimdb-dev/aimdb/compare/aimdb-mcp-v0.8.0...HEAD
[0.8.0]: https://github.com/aimdb-dev/aimdb/compare/aimdb-mcp-v0.7.0...aimdb-mcp-v0.8.0
[0.7.0]: https://github.com/aimdb-dev/aimdb/compare/aimdb-mcp-v0.6.1...aimdb-mcp-v0.7.0
[0.6.1]: https://github.com/aimdb-dev/aimdb/compare/aimdb-mcp-v0.6.0...aimdb-mcp-v0.6.1
[0.6.0]: https://github.com/aimdb-dev/aimdb/compare/aimdb-mcp-v0.5.0...aimdb-mcp-v0.6.0
[0.5.0]: https://github.com/aimdb-dev/aimdb/compare/aimdb-mcp-v0.4.0...aimdb-mcp-v0.5.0
[0.4.0]: https://github.com/aimdb-dev/aimdb/compare/v0.3.0...v0.4.0
[0.3.0]: https://github.com/aimdb-dev/aimdb/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/aimdb-dev/aimdb/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/aimdb-dev/aimdb/releases/tag/v0.1.0
