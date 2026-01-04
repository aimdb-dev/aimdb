# Weather Mesh Demo

A distributed weather monitoring mesh demonstrating AimDB's multi-tier architecture across MCU, edge, and cloud.

## Architecture

```
                    ┌─────────────────┐
                    │   weather-hub   │
                    │  (Tokio/Cloud)  │
                    └────────┬────────┘
                             │
            ┌────────────────┼────────────────┐
            │                │                │
  ┌─────────▼─────────┐ ┌────▼────────────┐ ┌─▼───────────────────┐
  │ weather-station-  │ │ weather-station-│ │ weather-station-    │
  │      alpha        │ │      beta       │ │       gamma         │
  │   (Tokio/Edge)    │ │   (Tokio/Edge)  │ │  (Embassy/MCU)      │
  └───────────────────┘ └─────────────────┘ └─────────────────────┘
```

## Components

| Component | Platform | Description |
|-----------|----------|-------------|
| `weather-hub` | Tokio (Linux/Cloud) | Central aggregator, exposes AimX API |
| `weather-station-alpha` | Tokio (Linux Edge) | Outdoor weather station |
| `weather-station-beta` | Tokio (Linux Edge) | Indoor weather station |
| `weather-station-gamma` | Embassy (RP2040) | Portable/remote MCU sensor |
| `weather-mesh-common` | no_std compatible | Shared contracts and configuration |

## Data Contracts

All stations publish:
- **Temperature** - Celsius readings with timestamps
- **Humidity** - Relative humidity percentage
- **GpsLocation** - Station identifier and coordinates

## Quick Start

### 1. Start the Hub

```bash
cd examples/weather-mesh-demo/weather-hub
cargo run
```

### 2. Start Edge Stations (in separate terminals)

```bash
cd examples/weather-mesh-demo/weather-station-alpha
cargo run
```

```bash
cd examples/weather-mesh-demo/weather-station-beta
cargo run
```

### 3. Flash MCU Station (optional)

```bash
cd examples/weather-mesh-demo/weather-station-gamma
cargo run --release
```

## Inspecting with MCP

Use the AimDB MCP tools to inspect the running mesh:

```
mcp_aimdb_discover_instances()
mcp_aimdb_list_records(socket_path: "/tmp/weather-hub.sock")
mcp_aimdb_subscribe_record(socket_path: "/tmp/weather-hub.sock", record_name: "alpha::Temperature", max_samples: 10)
```

## License

MIT OR Apache-2.0
