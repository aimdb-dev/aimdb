# Weather Mesh Demo

A distributed weather monitoring mesh demonstrating AimDB's multi-tier architecture across MCU, edge and cloud.

## Architecture

```
                    ┌─────────────────┐
                    │   weather-hub   │
                    │  (Tokio/Cloud)  │
                    └────────┬────────┘
                             │ MQTT
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
| `weather-station-alpha` | Tokio (Linux Edge) | Real weather data (Open-Meteo API) |
| `weather-station-beta` | Tokio (Linux Edge) | Synthetic sensor data |
| `weather-station-gamma` | Embassy (STM32H563ZITx) | Portable/remote MCU sensor (hardware) |
| `weather-mesh-common` | no_std compatible | Shared contracts and configuration |
| `mqtt` | Eclipse Mosquitto | Message bus for all nodes |

## Data Contracts

All stations publish via MQTT:
- **Temperature** - Celsius readings with timestamps
- **Humidity** - Relative humidity percentage
- **GpsLocation** - Station identifier and coordinates

## Quick Start (Docker)

**Recommended:** Use Docker Compose to run the entire cloud + edge mesh:

```bash
cd examples/weather-mesh-demo
docker compose up
```

This starts:
- **MQTT broker** (Mosquitto) - Message bus
- **weather-hub** - Central cloud aggregator
- **weather-station-alpha** - Edge node with real weather data (Vienna)
- **weather-station-beta** - Edge node with synthetic data

All nodes connect automatically and begin publishing data.

### Viewing Logs

```bash
# All services
docker compose logs -f

# Specific service
docker compose logs -f weather-hub
docker compose logs -f weather-station-alpha
```

### Stopping the Mesh

```bash
docker compose down
```

## Alternative: Manual Start (Development)

For development or debugging, run nodes individually:

### 1. Start MQTT Broker

```bash
docker compose up -d mqtt
```

### 2. Start the Hub

```bash
cd examples/weather-mesh-demo/weather-hub
MQTT_BROKER=localhost cargo run
```

### 3. Start Edge Stations (in separate terminals)

```bash
cd examples/weather-mesh-demo/weather-station-alpha
MQTT_BROKER=localhost cargo run
```

```bash
cd examples/weather-mesh-demo/weather-station-beta
MQTT_BROKER=localhost cargo run
```

### 4. Flash MCU Station (Optional - Requires Hardware)

Requires [probe-rs](https://probe.rs/) and an STM32H563ZITx board with debug probe:

```bash
cd examples/weather-mesh-demo/weather-station-gamma
cargo build --release
probe-rs run --chip STM32H563ZITx target/thumbv8m.main-none-eabihf/release/weather-station-gamma
```

## Inspecting with VS Code Copilot

The weather mesh exposes an AimX API socket that Copilot can read through MCP (Model Context Protocol). Simply ask Copilot in natural language:

**Example queries:**
- "Show me all available weather stations"
- "What's the current temperature from station alpha?"
- "Subscribe to humidity updates from beta"
- "Compare temperature readings across all stations"

Copilot will automatically use the MCP tools (`mcp_aimdb_*`) to discover instances, query records, and interpret the data. This creates a conversational interface for your distributed sensor mesh.

> **Note:** The AimDB MCP server is pre-configured in the devcontainer. If running outside the devcontainer, you'll need to [add the MCP server to VS Code](../../tools/aimdb-mcp/) manually.

## Configuration

The Docker setup uses environment variables for configuration:

| Variable | Default | Description |
|----------|---------|-------------|
| `RUST_LOG` | `info,aimdb_core=debug` | Logging level |
| `MQTT_BROKER` | `mqtt` (service name) | MQTT broker hostname |
| `WEATHER_LAT` | `48.2082` (Vienna) | Latitude for Open-Meteo API |
| `WEATHER_LON` | `16.3738` (Vienna) | Longitude for Open-Meteo API |

## License

Apache-2.0
