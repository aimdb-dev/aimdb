# AimX Graph Introspection — Data Lineage & Dependency Visualization

**Version:** 1.0
**Status:** 📋 Proposed
**Last Updated:** February 8, 2026
**Milestone:** M9 — Reactive Transforms
**Depends On:** [008-M3-remote-access](008-M3-remote-access.md), [020-M9-transform-api](020-M9-transform-api.md)

---

## Table of Contents

- [Summary](#summary)
- [Motivation](#motivation)
- [Graph Model Recap](#graph-model-recap)
- [AimX Protocol Extension](#aimx-protocol-extension)
- [Client Library Extension](#client-library-extension)
- [MCP Tool Extension](#mcp-tool-extension)
- [CLI Integration](#cli-integration)
- [Testing Strategy](#testing-strategy)
- [Implementation Plan](#implementation-plan)
- [Open Points](#open-points)

---

## Summary

Expose AimDB's internal dependency graph — built at startup from `.source()`,
`.tap()`, `.transform()`, and `.link()` registrations — over the AimX v1.1
protocol. Three new request methods (`graph.describe`, `graph.lineage`,
`graph.dot`) plus an extension to the existing `record.list` method give
remote clients full visibility into the data-flow architecture.

MCP tools and CLI commands wrap these methods to enable:

- **LLM agents** explaining system architecture and debugging data staleness
- **Operators** performing impact analysis before infrastructure changes
- **Documentation generators** producing live, always-accurate data-flow diagrams

The graph is **immutable after `build()`** — constructed once, serialized once,
served at zero cost on every request.

---

## Motivation

### Why expose the graph?

[020-M9-transform-api](020-M9-transform-api.md) introduces `.transform()` and
`.transform_join()`, making AimDB's records form a directed acyclic graph. The
graph already exists internally for cycle detection and spawn ordering. This
document specifies how to **expose it remotely** — turning a build-time
validation artifact into a live introspection API.

### Concrete use cases

| Scenario | Who benefits | What they need |
|----------|-------------|----------------|
| "Explain how data flows through this system" | LLM agent / new developer | Full graph with node origins |
| "Why is `validation.vienna` stale?" | LLM agent / operator | Upstream lineage to find the broken input |
| "What happens if the MQTT broker goes down?" | Operator | Downstream impact of all `link`-origin records |
| "Generate a system architecture diagram" | Documentation | DOT/SVG export of the live graph |
| "Which records are derived vs. direct?" | MCP agent | `origin` field on `record.list` |
| "How many transform hops between sensor and dashboard?" | Performance tuning | Lineage depth |

### Current gap

Today, AimX v1.0 provides `record.list` (flat list of records with buffer
metadata) and `record.get` / `record.subscribe` (individual record access).
There is **no way** to discover that `validation.vienna` is derived from
`temp.vienna` and `forecast.vienna.24h` — that relationship is invisible to
any remote client.

---

## Graph Model Recap

> Full type definitions live in [020-M9-transform-api § Dependency Graph](020-M9-transform-api.md#dependency-graph).
> This section summarizes the key types for protocol design.

Every record is a **node** with a `RecordOrigin`:

```rust
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RecordOrigin {
    Source,
    Link { protocol: String, address: String },
    Transform { input: String },
    TransformJoin { inputs: Vec<String> },
    Passive,
}
```

**Edges** are directed and classified:

| Edge Type | From | To | Meaning |
|-----------|------|----|---------|
| `source` | null (external) | Record | Autonomous producer |
| `link` | null (external) | Record | Inbound connector |
| `transform` | Record | Record | Single-input derivation |
| `transform_join` | Record(s) | Record | Multi-input derivation |
| `tap` | Record | null (side-effect) | Passive observer |
| `link_out` | Record | null (external) | Outbound connector |

The `DependencyGraph` struct is immutable after `build()`:

```rust
pub(crate) struct DependencyGraph {
    pub nodes: HashMap<String, GraphNode>,
    pub edges: Vec<GraphEdge>,
    pub topo_order: Vec<String>,
}
```

---

## AimX Protocol Extension

Three new methods and one extension to an existing method, all under AimX
v1.1. These are **read-only** — no new permissions required beyond the
existing `"read"` capability.

### Capability Announcement

The server advertises graph support in the `welcome` handshake:

```json
{
  "welcome": {
    "version": "1.1",
    "server": "aimdb/0.5.0",
    "permissions": ["read", "subscribe"],
    "capabilities": ["graph"],
    "writable_records": []
  }
}
```

Clients check for `"graph"` in `capabilities` before calling `graph.*`
methods. Servers without transform support (pre-v1.1) omit this capability;
clients should fall back gracefully.

### `graph.describe`

Returns the full dependency graph — all nodes, all edges, topological order.

**Request:**

```json
{
  "id": 20,
  "method": "graph.describe",
  "params": {}
}
```

**Optional Parameters:**
- `key_prefix` (string, optional): Filter nodes to keys matching this prefix
  (e.g., `"temp."` returns only temperature records and their edges)

**Response:**

```json
{
  "id": 20,
  "result": {
    "nodes": [
      {
        "key": "temp.vienna",
        "origin": { "link": { "protocol": "mqtt", "address": "sensors/vienna/temperature" } },
        "buffer_type": "spmc_ring",
        "buffer_capacity": 100,
        "tap_count": 1,
        "has_outbound_link": false
      },
      {
        "key": "forecast.vienna.24h",
        "origin": { "link": { "protocol": "mqtt", "address": "sensors/vienna/forecast" } },
        "buffer_type": "spmc_ring",
        "buffer_capacity": 100,
        "tap_count": 1,
        "has_outbound_link": false
      },
      {
        "key": "validation.vienna",
        "origin": { "transform_join": { "inputs": ["temp.vienna", "forecast.vienna.24h"] } },
        "buffer_type": "spmc_ring",
        "buffer_capacity": 500,
        "tap_count": 1,
        "has_outbound_link": false
      }
    ],
    "edges": [
      { "from": null, "to": "temp.vienna", "edge_type": { "link": { "protocol": "mqtt" } } },
      { "from": null, "to": "forecast.vienna.24h", "edge_type": { "link": { "protocol": "mqtt" } } },
      { "from": "temp.vienna", "to": "validation.vienna", "edge_type": "transform_join" },
      { "from": "forecast.vienna.24h", "to": "validation.vienna", "edge_type": "transform_join" },
      { "from": "temp.vienna", "to": null, "edge_type": { "tap": { "index": 0 } } },
      { "from": "forecast.vienna.24h", "to": null, "edge_type": { "tap": { "index": 0 } } },
      { "from": "validation.vienna", "to": null, "edge_type": { "tap": { "index": 0 } } }
    ],
    "topo_order": ["temp.vienna", "forecast.vienna.24h", "validation.vienna"]
  }
}
```

**Result Fields:**
- `nodes` (array): All record nodes with metadata
- `edges` (array): All directed edges; `from`/`to` are `null` for
  external sources and sinks
- `topo_order` (array): Records in topological order (inputs before
  their dependents). Useful for ordered processing or display.

### `graph.lineage`

Traces the **upstream lineage** (provenance) or **downstream dependents**
of a single record. This is the primary debugging tool — it answers:

- *"Where does this record's data come from?"* (upstream)
- *"What breaks if this record stops producing?"* (downstream)

**Request (upstream):**

```json
{
  "id": 21,
  "method": "graph.lineage",
  "params": {
    "key": "validation.vienna",
    "direction": "upstream"
  }
}
```

**Response:**

```json
{
  "id": 21,
  "result": {
    "root": "validation.vienna",
    "direction": "upstream",
    "depth": 1,
    "lineage": [
      {
        "key": "validation.vienna",
        "origin": { "transform_join": { "inputs": ["temp.vienna", "forecast.vienna.24h"] } },
        "upstream": [
          {
            "key": "temp.vienna",
            "origin": { "link": { "protocol": "mqtt", "address": "sensors/vienna/temperature" } },
            "upstream": []
          },
          {
            "key": "forecast.vienna.24h",
            "origin": { "link": { "protocol": "mqtt", "address": "sensors/vienna/forecast" } },
            "upstream": []
          }
        ]
      }
    ]
  }
}
```

**Request (downstream):**

```json
{
  "id": 22,
  "method": "graph.lineage",
  "params": {
    "key": "temp.vienna",
    "direction": "downstream"
  }
}
```

**Response:**

```json
{
  "id": 22,
  "result": {
    "root": "temp.vienna",
    "direction": "downstream",
    "depth": 1,
    "lineage": [
      {
        "key": "temp.vienna",
        "downstream": [
          {
            "key": "validation.vienna",
            "origin": { "transform_join": { "inputs": ["temp.vienna", "forecast.vienna.24h"] } },
            "downstream": []
          }
        ]
      }
    ]
  }
}
```

**Request Parameters:**
- `key` (string, required): Record key to trace from
- `direction` (string, required): `"upstream"` or `"downstream"`
- `max_depth` (integer, optional, default: 10): Maximum traversal depth
  (prevents runaway traversal on deep chains)

**Result Fields:**
- `root` (string): The starting record key
- `direction` (string): Echo of the requested direction
- `depth` (integer): Actual depth of the returned tree
- `lineage` (array): Recursive tree of nodes; each node contains
  `upstream` or `downstream` children depending on direction

**Errors:**
- `NOT_FOUND` — Record key doesn't exist

### `graph.dot`

Returns the dependency graph as a Graphviz DOT language string, ready for
rendering with `dot`, `neato`, or any DOT-compatible tool.

**Request:**

```json
{
  "id": 25,
  "method": "graph.dot",
  "params": {}
}
```

**Optional Parameters:**
- `key_prefix` (string, optional): Filter to subgraph matching prefix
- `include_taps` (boolean, optional, default: false): Include tap edges
  (can make large graphs noisy)
- `include_external` (boolean, optional, default: true): Include external
  source/sink nodes (MQTT topics, etc.)

**Response:**

```json
{
  "id": 25,
  "result": {
    "dot": "digraph aimdb {\n  rankdir=LR;\n  node [shape=record];\n  \"mqtt://sensors/vienna/temperature\" [shape=ellipse, style=dashed];\n  \"mqtt://sensors/vienna/forecast\" [shape=ellipse, style=dashed];\n  \"temp.vienna\" [label=\"{temp.vienna|SpmcRing(100)}\"];\n  \"forecast.vienna.24h\" [label=\"{forecast.vienna.24h|SpmcRing(100)}\"];\n  \"validation.vienna\" [label=\"{validation.vienna|SpmcRing(500)}\"];\n  \"mqtt://sensors/vienna/temperature\" -> \"temp.vienna\" [label=\"link\"];\n  \"mqtt://sensors/vienna/forecast\" -> \"forecast.vienna.24h\" [label=\"link\"];\n  \"temp.vienna\" -> \"validation.vienna\" [label=\"transform_join\", style=bold];\n  \"forecast.vienna.24h\" -> \"validation.vienna\" [label=\"transform_join\", style=bold];\n}"
  }
}
```

**DOT Conventions:**
- **Record nodes**: `shape=record` with label `{key|BufferType(capacity)}`
- **External nodes**: `shape=ellipse, style=dashed` (MQTT topics, etc.)
- **Transform edges**: `style=bold` to visually distinguish from links
- **Layout**: `rankdir=LR` (left-to-right) by default

**Rendered output for the Weather Hub (Vienna subset):**

```
┌──────────────────────────┐     ┌──────────────────────────────┐
│ mqtt://sensors/vienna/   │     │ mqtt://sensors/vienna/       │
│ temperature              │     │ forecast                     │
└───────────┬──────────────┘     └───────────┬──────────────────┘
            │ link                            │ link
            ▼                                ▼
┌───────────────────┐             ┌────────────────────────┐
│ temp.vienna       │             │ forecast.vienna.24h    │
│ SpmcRing(100)     │             │ SpmcRing(100)          │
└─────────┬─────────┘             └──────────┬─────────────┘
          │ transform_join                   │ transform_join
          └──────────┬───────────────────────┘
                     ▼
          ┌──────────────────────┐
          │ validation.vienna    │
          │ SpmcRing(500)        │
          └──────────────────────┘
```

### Extending `record.list`

The existing `record.list` response ([008-M3-remote-access](008-M3-remote-access.md))
is extended with an `origin` field so clients can see record provenance
without a dedicated graph call:

**Current response (v1.0):**

```json
{
  "name": "validation.vienna",
  "buffer_type": "spmc_ring",
  "buffer_capacity": 500,
  "producer_count": 1,
  "consumer_count": 1,
  "writable": false
}
```

**Extended response (v1.1):**

```json
{
  "name": "validation.vienna",
  "origin": { "transform_join": { "inputs": ["temp.vienna", "forecast.vienna.24h"] } },
  "buffer_type": "spmc_ring",
  "buffer_capacity": 500,
  "producer_count": 1,
  "consumer_count": 1,
  "writable": false
}
```

This is a **backward-compatible addition** — the `origin` field is new and
optional. Existing v1.0 clients that don't expect it will simply ignore it.

### Error Codes

New error codes for graph methods:

| Code | Method | Description |
|------|--------|-------------|
| `NOT_FOUND` | `graph.lineage` | Record key doesn't exist |
| `NO_GRAPH` | all `graph.*` | Server has no dependency graph (pre-transform build, or no transforms registered) |

The `NO_GRAPH` case is unlikely in practice — even databases with zero
transforms have a trivially valid graph (all nodes are roots). But it provides
a clean error if the graph was not constructed.

---

## Client Library Extension

The `aimdb-client` crate is extended with methods that wrap the new AimX
protocol methods.

```rust
impl AimDbClient {
    /// Get the full dependency graph.
    ///
    /// Returns all nodes, edges, and topological order.
    /// Optionally filter by key prefix.
    pub async fn graph_describe(
        &self,
        key_prefix: Option<&str>,
    ) -> Result<GraphDescription, ClientError>;

    /// Trace upstream or downstream lineage for a record.
    pub async fn graph_lineage(
        &self,
        key: &str,
        direction: LineageDirection,
        max_depth: Option<usize>,
    ) -> Result<LineageResult, ClientError>;

    /// Get the dependency graph as a Graphviz DOT string.
    pub async fn graph_dot(
        &self,
        options: DotOptions,
    ) -> Result<String, ClientError>;
}

pub enum LineageDirection {
    Upstream,
    Downstream,
}

pub struct DotOptions {
    pub key_prefix: Option<String>,
    pub include_taps: bool,
    pub include_external: bool,
}

impl Default for DotOptions {
    fn default() -> Self {
        Self {
            key_prefix: None,
            include_taps: false,
            include_external: true,
        }
    }
}
```

**Response types** mirror the AimX JSON structures:

```rust
pub struct GraphDescription {
    pub nodes: Vec<GraphNode>,
    pub edges: Vec<GraphEdge>,
    pub topo_order: Vec<String>,
}

pub struct LineageResult {
    pub root: String,
    pub direction: LineageDirection,
    pub depth: usize,
    pub lineage: Vec<LineageNode>,
}

pub struct LineageNode {
    pub key: String,
    pub origin: Option<RecordOrigin>,
    pub children: Vec<LineageNode>,  // upstream or downstream
}
```

---

## MCP Tool Extension

The MCP server (`tools/aimdb-mcp`) exposes the dependency graph through
new tools that wrap the AimX `graph.*` methods.

### `mcp_aimdb_get_graph`

Returns the full dependency graph for an AimDB instance.

**Tool definition:**

```json
{
  "name": "mcp_aimdb_get_graph",
  "description": "Get the full dependency graph of an AimDB instance. Returns all records (nodes), their data dependencies (edges), and topological ordering. Use this to understand how data flows through the system.",
  "inputSchema": {
    "type": "object",
    "properties": {
      "socket_path": {
        "type": "string",
        "description": "Unix socket path to the AimDB instance"
      },
      "format": {
        "type": "string",
        "enum": ["json", "dot", "summary"],
        "description": "Output format. 'json' for full graph data, 'dot' for Graphviz DOT, 'summary' for human-readable text.",
        "default": "summary"
      }
    },
    "required": ["socket_path"]
  }
}
```

**Example output (summary format):**

```
AimDB Dependency Graph — 15 records, 20 edges

📡 External Sources:
  mqtt://sensors/vienna/temperature  → temp.vienna
  mqtt://sensors/vienna/humidity     → humidity.vienna
  mqtt://sensors/vienna/forecast     → forecast.vienna.24h
  mqtt://sensors/vienna/location     → location.vienna
  ... (4 more cities)

🔄 Transforms:
  temp.vienna + forecast.vienna.24h  → validation.vienna  (join)
  temp.berlin + forecast.berlin.24h  → validation.berlin   (join)
  ... (3 more cities)

👁 Taps:
  temp.vienna       → [ws_broadcast]
  humidity.vienna   → [ws_broadcast]
  validation.vienna → [ws_broadcast]
  ... (12 more)

📊 Topological Order:
  1. temp.vienna, humidity.vienna, location.vienna, forecast.vienna.24h
  2. temp.berlin, humidity.berlin, location.berlin, forecast.berlin.24h
  ...
  N. validation.vienna, validation.berlin, ...
```

### `mcp_aimdb_get_lineage`

Traces upstream provenance or downstream impact for a single record.

**Tool definition:**

```json
{
  "name": "mcp_aimdb_get_lineage",
  "description": "Trace the data lineage of a record. 'upstream' shows where data comes from (provenance). 'downstream' shows what depends on this record (impact analysis). Use 'upstream' to debug data quality issues. Use 'downstream' to assess impact of record changes.",
  "inputSchema": {
    "type": "object",
    "properties": {
      "socket_path": {
        "type": "string",
        "description": "Unix socket path to the AimDB instance"
      },
      "record_name": {
        "type": "string",
        "description": "Record key to trace lineage for"
      },
      "direction": {
        "type": "string",
        "enum": ["upstream", "downstream"],
        "description": "Trace direction"
      }
    },
    "required": ["socket_path", "record_name", "direction"]
  }
}
```

**Example LLM interaction:**

```
User: Why is validation.vienna not producing data?

Agent: Let me trace the lineage for that record.
→ mcp_aimdb_get_lineage(record_name="validation.vienna", direction="upstream")

The upstream dependency chain is:
  validation.vienna ← forecast.vienna ← mqtt::forecasts/vienna

The root source is an MQTT topic `forecasts/vienna`. If that topic isn't
receiving messages, `forecast.vienna` won't update, which means
`validation.vienna` has nothing to validate. Check your MQTT broker for
activity on that topic.
```

**Use Cases for LLM Agents:**

| Scenario | Tool | Prompt Pattern |
|----------|------|---------------|
| Explain system architecture | `get_graph` (summary) | "Describe the data flow in this AimDB instance" |
| Debug stale data | `get_lineage` (upstream) | "Why is record X not updating?" |
| Impact analysis | `get_lineage` (downstream) | "What happens if MQTT goes down?" |
| Generate documentation | `get_graph` (dot) | "Create a diagram of the data pipeline" |
| Check if record is derived | `get_graph` (json) | "Is record X computed from other records?" |

---

## 7. Testing Strategy

All tests use the standard AimDB testing patterns with `#[tokio::test]`.

### 7.1 AimX Protocol Tests

```rust
#[tokio::test]
async fn graph_describe_returns_all_nodes_and_edges() {
    // Setup: AimDB with source → transform → tap pipeline
    // Action: Send graph.describe request over AimX
    // Assert: Response contains all 3 nodes, 2 edges, correct types
}

#[tokio::test]
async fn graph_lineage_upstream_traces_to_sources() {
    // Setup: Chain source → transform1 → transform2
    // Action: graph.lineage { key: "transform2", direction: "upstream" }
    // Assert: Returns [transform1, source] in order
}

#[tokio::test]
async fn graph_lineage_downstream_traces_to_dependents() {
    // Setup: source → transform → tap1, tap2
    // Action: graph.lineage { key: "source", direction: "downstream" }
    // Assert: Returns transform, tap1, tap2
}

#[tokio::test]
async fn graph_dot_produces_valid_graphviz() {
    // Setup: Multi-node pipeline
    // Action: graph.dot {}
    // Assert: Output starts with "digraph", contains node labels,
    //         valid edge syntax "A" -> "B"
}

#[tokio::test]
async fn record_list_includes_origin_field() {
    // Setup: AimDB with source + transform records
    // Action: record.list {}
    // Assert: Each record entry contains "origin" field matching
    //         "source", "transform", or "external"
}
```

### 7.2 MCP Tool Tests

```rust
#[tokio::test]
async fn mcp_get_graph_json_format() {
    // Setup: Running AimDB instance with known topology
    // Action: Call mcp_aimdb_get_graph with format="json"
    // Assert: Valid JSON matching GraphDescribeResponse schema
}

#[tokio::test]
async fn mcp_get_graph_summary_format() {
    // Setup: Running AimDB instance
    // Action: Call mcp_aimdb_get_graph with format="summary"
    // Assert: Human-readable text with record counts and flow arrows
}

#[tokio::test]
async fn mcp_get_lineage_upstream() {
    // Setup: Known pipeline topology
    // Action: Call mcp_aimdb_get_lineage with direction="upstream"
    // Assert: Correct chain of dependencies
}
```

---

## 8. Implementation Plan

### Phase 1 — AimX Protocol Extension (3–4 days)

**Prerequisites:** 020-M9 Phase 1 (DependencyGraph in aimdb-core)

- [ ] Add `DependencyGraph` accessor to AimX server context
- [ ] Implement `graph.describe` handler with key_prefix filtering
- [ ] Implement `graph.lineage` handler with BFS traversal and max_depth
- [ ] Implement `graph.dot` handler with Graphviz DOT generation
- [ ] Extend `record.list` response with `origin` field
- [ ] Add `graph` to capability announcement
- [ ] Write integration tests for all new AimX methods

### Phase 2 — MCP Tool Extension (1–2 days)

**Prerequisites:** Phase 1

- [ ] Add `graph_describe`, `graph_lineage`, `graph_dot` to `aimdb-client`
- [ ] Implement `mcp_aimdb_get_graph` tool with format parameter
- [ ] Implement `mcp_aimdb_get_lineage` tool
- [ ] Write MCP tool tests

---

## 9. Open Points

### Graph Serialization Cost

**Question:** Should `graph.describe` serialize the full graph on every call,
or should the server pre-serialize?

**Recommendation:** Pre-serialize. The dependency graph is **immutable after
`build()`** — it never changes at runtime. The server can serialize the graph
once during startup and cache the JSON/DOT representations. This makes
`graph.describe` and `graph.dot` essentially free (return cached bytes).

For `graph.lineage`, the BFS traversal is lightweight (typically <20 nodes)
and doesn't warrant caching since the `key` parameter varies per request.

### Graph Versioning

If AimDB ever supports dynamic record registration (post-build), the cached
graph would need invalidation. For now, this is not a concern since the graph
is fixed at build time.

---

## 10. References

- [008-M3 — Remote Access & AimX Protocol](008-M3-remote-access.md)
- [020-M9 — Transform API & Dependency Graph](020-M9-transform-api.md)
- [009-M4 — MCP Integration](009-M4-mcp-integration.md)
- [013-M6 — Client Library](013-M6-client-library.md)
User: Why is validation.vienna not producing data?