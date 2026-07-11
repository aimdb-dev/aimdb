# Embassy MQTT Connector Demo

⚠️ **Note**: This example is a template that demonstrates the API usage pattern for the Embassy MQTT connector. It requires hardware-specific configuration and testing on actual STM32H5 hardware with Ethernet support.

## Status

✅ **MQTT Client Implementation**: Complete and compiles successfully  
🚧 **Hardware Example**: Template code - requires hardware-specific tuning

The `aimdb-mqtt-connector` library with Embassy support is fully implemented and tested. This example shows how to use it but needs hardware testing and tuning for the specific board configuration.

## What's Implemented

The core Embassy MQTT client (`aimdb-mqtt-connector::embassy_client`) provides:

- ✅ Async MQTT publishing with mountain-mqtt-embassy
- ✅ Channel-based architecture for background task communication
- ✅ Automatic reconnection handling
- ✅ QoS 0/1/2 support
- ✅ `no_std` compatible (works in embedded environments)

## API Usage Pattern

```rust
use aimdb_mqtt_connector::embassy_client::MqttClientPool;
use embassy_net::Stack;

// Create MQTT client (requires initialized network stack)
let mqtt_result = MqttClientPool::create(
    network_stack,        // embassy_net::Stack
    "192.168.1.100",      // Broker IP
    1883,                 // Broker port
    "my-client-id",       // Client ID
).await?;

// Spawn background task (runs forever, maintains connection)
spawner.spawn(async move {
    mqtt_result.task.run().await
}).unwrap();

// Use the pool to publish messages
mqtt_result.pool.publish_async(
    "sensors/temperature",  // Topic
    b"{\"value\":23.5}",   // Payload
    1,                      // QoS (0, 1, or 2)
    false                   // Retain flag
).await?;
```

## Hardware Requirements (for full example)

- STM32H563ZI Nucleo board (or compatible with Ethernet)
- Ethernet cable + DHCP network
- USB cable for programming (ST-Link)
- MQTT broker on network

## Building

The example code in `src/main.rs` is a template. To adapt it:

1. **Update Hardware Configuration**:
   - Match GPIO pins to your board's Ethernet PHY
   - Configure clock speeds for your MCU
   - Adjust memory layout in `memory.x`

2. **Install Dependencies**:
   ```bash
   rustup target add thumbv8m.main-none-eabihf
   cargo install probe-rs --features cli
   ```

3. **Configure Broker**:
   - Update `MQTT_BROKER_IP` in `src/main.rs`
   - Ensure broker is reachable from device network

4. **Build**:
   ```bash
   cargo build --release
   ```

## TLS (`mqtts://`)

The `tls` feature switches the demo to a TLS broker (`aimdb-mqtt-connector`'s
`embassy-tls` feature underneath): `mqtts://` with hostname resolution via
DNS, optional MQTT username/password, and an automatic SNTP time sync that
gates the first handshake (certificate validity needs real time — the board
has no RTC battery).

1. In `src/main.rs`, set `MQTT_BROKER_HOST` and, if the broker requires it,
   `MQTT_CREDENTIALS`. Prefer a DNS name: an IPv4 literal verifies only when
   the certificate pins that IP in its CN (the repo's `dev/mosquitto` bench
   CA does; public CAs won't issue such certs). IPv6 literals are rejected
   at build.
2. Drop the broker's root CA next to `Cargo.toml`, DER-encoded — for the
   `dev/mosquitto` bench broker:
   ```bash
   openssl x509 -in ../../dev/mosquitto/config/certs/ca.crt -outform der -out ca.der
   ```
3. Build (and flash) from this directory, so its `.cargo/config.toml`
   selects the thumbv8m target and probe-rs runner:
   ```bash
   cargo run --release --features tls
   ```
   Note: broker/CA config is compile-time — env vars like `SSL_CERT_FILE`
   or `MQTT_BROKER_URL` only apply to the host-side Tokio demo.

Cost: ~21 KB extra statically-allocated RAM (TLS record buffers) plus
~150–250 KB flash (pure-Rust crypto: p256 + rsa + p384 + SHA-2).

## Testing the MQTT Client Without Hardware

You can test the MQTT connector implementation using the Tokio runtime version:

```bash
# In aimdb-mqtt-connector directory
cargo test --features tokio-runtime

# Check Embassy features compile
cargo check --features embassy-runtime
```

## Integration with AimDB

The MQTT connector integrates with AimDB's buffer system:

```rust
// Producer writes to buffer
buffer.push(sensor_reading);

// Consumer reads from buffer and publishes to MQTT
let reader = buffer.subscribe();
loop {
    let reading = reader.recv().await?;
    mqtt_pool.publish_async(
        "sensors/data",
        &serialize(reading),
        1,
        false
    ).await?;
}
```

## Next Steps

### For Hardware Testing

1. Get actual STM32H5 Nucleo board
2. Verify Ethernet PHY pinout matches code
3. Test network connectivity (DHCP)
4. Test MQTT broker connection
5. Tune buffer sizes and timings

### For Library Usage

The Embassy MQTT client is ready to use! Just add to your `Cargo.toml`:

```toml
aimdb-mqtt-connector = { path = "../../aimdb-mqtt-connector", features = ["embassy-runtime"] }
```

## Resources

- [MQTT Client Implementation](../../aimdb-mqtt-connector/src/embassy_client.rs)
- [Embassy Documentation](https://embassy.dev/)
- [mountain-mqtt](https://github.com/mountainlizard/mountain-mqtt)
- [AimDB Core Documentation](../../README.md)
