//! KNX/IP client management and lifecycle for Tokio runtime
//!
//! This module provides a KNX connector that:
//! - Manages a single KNX/IP gateway connection
//! - Automatic event loop spawning with reconnection
//! - Thread-safe access from multiple consumers
//! - Router-based dispatch for inbound telegrams

use aimdb_core::connector::ConnectorUrl;
use aimdb_core::router::{Router, RouterBuilder};
use aimdb_core::ConnectorBuilder;
use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::UdpSocket;

/// KNX connector for a single gateway connection with router-based dispatch
///
/// Each connector manages ONE KNX/IP gateway connection. The router determines
/// how incoming telegrams are dispatched to AimDB producers.
///
/// # Usage Pattern
///
/// ```rust,ignore
/// use aimdb_knx_connector::KnxConnector;
///
/// // Configure database with KNX links
/// let db = AimDbBuilder::new()
///     .runtime(runtime)
///     .with_connector(KnxConnector::new("knx://192.168.1.19:3671"))
///     .configure::<LightState>(|reg| {
///         reg.link_from("knx://1/0/7")
///            .with_deserializer(deserialize_light)
///            .with_buffer(BufferCfg::SingleLatest)
///            .finish();
///     })
///     .build().await?;
/// ```
///
/// The connector collects routes from the database during build() and
/// automatically monitors all required KNX group addresses.
pub struct KnxConnectorBuilder {
    gateway_url: String,
}

impl KnxConnectorBuilder {
    /// Create a new KNX connector builder
    ///
    /// # Arguments
    /// * `gateway_url` - Gateway URL (knx://host:port)
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let builder = KnxConnector::new("knx://192.168.1.19:3671");
    /// ```
    pub fn new(gateway_url: impl Into<String>) -> Self {
        Self {
            gateway_url: gateway_url.into(),
        }
    }
}

impl<R: aimdb_executor::Spawn + 'static> ConnectorBuilder<R> for KnxConnectorBuilder {
    fn build<'a>(
        &'a self,
        db: &'a aimdb_core::builder::AimDb<R>,
    ) -> Pin<
        Box<
            dyn Future<Output = aimdb_core::DbResult<Arc<dyn aimdb_core::transport::Connector>>>
                + Send
                + 'a,
        >,
    > {
        Box::pin(async move {
            // Collect inbound routes from database
            let inbound_routes = db.collect_inbound_routes("knx");

            #[cfg(feature = "tracing")]
            tracing::info!(
                "Collected {} inbound routes for KNX connector",
                inbound_routes.len()
            );

            // Convert routes to Router
            let router = RouterBuilder::from_routes(inbound_routes).build();

            #[cfg(feature = "tracing")]
            tracing::info!(
                "KNX router has {} group addresses",
                router.resource_ids().len()
            );

            // Build the actual connector
            let connector =
                KnxConnectorImpl::build_internal(&self.gateway_url, router)
                    .await
                    .map_err(|e| {
                        #[cfg(feature = "std")]
                        {
                            aimdb_core::DbError::RuntimeError {
                                message: format!("Failed to build KNX connector: {}", e).into(),
                            }
                        }
                        #[cfg(not(feature = "std"))]
                        {
                            aimdb_core::DbError::RuntimeError { _message: () }
                        }
                    })?;

            // Collect and spawn outbound publishers
            let outbound_routes = db.collect_outbound_routes("knx");

            #[cfg(feature = "tracing")]
            tracing::info!(
                "Collected {} outbound routes for KNX connector",
                outbound_routes.len()
            );

            connector.spawn_outbound_publishers(db, outbound_routes)?;

            Ok(Arc::new(connector) as Arc<dyn aimdb_core::transport::Connector>)
        })
    }

    fn scheme(&self) -> &str {
        "knx"
    }
}

/// Internal KNX connector implementation
///
/// This is the actual connector created after collecting routes from the database.
pub struct KnxConnectorImpl {
    gateway_ip: String,
    gateway_port: u16,
    router: Arc<Router>,
}

impl KnxConnectorImpl {
    /// Create a new KNX connector with pre-configured router (internal)
    ///
    /// Creates a connection to the KNX/IP gateway and monitors telegrams
    /// for all group addresses defined in the router. The connection task
    /// is spawned automatically with reconnection logic.
    ///
    /// # Arguments
    /// * `gateway_url` - Gateway URL (knx://host:port)
    /// * `router` - Pre-configured router with all routes
    async fn build_internal(
        gateway_url: &str,
        router: Router,
    ) -> Result<Self, String> {
        // Parse the gateway URL
        let mut url = gateway_url.to_string();

        // If no group address is provided, add a dummy one for parsing
        if !url.contains('/') || url.matches('/').count() < 3 {
            url = format!("{}/0/0/0", url.trim_end_matches('/'));
        }

        let connector_url =
            ConnectorUrl::parse(&url).map_err(|e| format!("Invalid KNX URL: {}", e))?;

        let gateway_ip = connector_url.host.clone();
        let gateway_port = connector_url.port.unwrap_or(3671);

        #[cfg(feature = "tracing")]
        tracing::info!(
            "Creating KNX connector for gateway {}:{}",
            gateway_ip,
            gateway_port
        );

        let router_arc = Arc::new(router);

        // Spawn background connection task with reconnection
        spawn_connection_task(gateway_ip.clone(), gateway_port, router_arc.clone());

        Ok(Self {
            gateway_ip,
            gateway_port,
            router: router_arc,
        })
    }

    /// Get list of all group addresses this connector monitors
    ///
    /// Returns the unique group addresses from the router configuration.
    /// Useful for debugging and monitoring.
    pub fn group_addresses(&self) -> Vec<Arc<str>> {
        self.router.resource_ids()
    }

    /// Get the number of routes configured in this connector
    ///
    /// Each route represents a (group_address, type) mapping.
    /// Multiple routes can exist for the same address if different types subscribe to it.
    pub fn route_count(&self) -> usize {
        self.router.route_count()
    }

    /// Spawns outbound publisher tasks for all configured routes (internal)
    ///
    /// Called automatically during build() to start publishing data from AimDB to KNX.
    /// Each route spawns an independent task that subscribes to the record
    /// and publishes to the KNX gateway.
    fn spawn_outbound_publishers<R>(
        &self,
        db: &aimdb_core::builder::AimDb<R>,
        routes: Vec<(
            String,
            Box<dyn aimdb_core::connector::ConsumerTrait>,
            aimdb_core::connector::SerializerFn,
            Vec<(String, String)>,
        )>,
    ) -> aimdb_core::DbResult<()>
    where
        R: aimdb_executor::Spawn + 'static,
    {
        let runtime = db.runtime();

        for (group_addr_str, consumer, serializer, _config) in routes {
            let gateway_ip = self.gateway_ip.clone();
            let gateway_port = self.gateway_port;
            let group_addr_clone = group_addr_str.clone();

            runtime.spawn(async move {
                // Parse group address
                let group_addr = match parse_group_address(&group_addr_clone) {
                    Ok(addr) => addr,
                    Err(_e) => {
                        #[cfg(feature = "tracing")]
                        tracing::error!(
                            "Invalid group address for outbound: '{}': {:?}",
                            group_addr_clone,
                            _e
                        );
                        return;
                    }
                };

                // Subscribe to typed values (type-erased)
                let mut reader = match consumer.subscribe_any().await {
                    Ok(r) => r,
                    Err(_e) => {
                        #[cfg(feature = "tracing")]
                        tracing::error!(
                            "Failed to subscribe for outbound group address '{}': {:?}",
                            group_addr_clone,
                            _e
                        );
                        return;
                    }
                };

                #[cfg(feature = "tracing")]
                tracing::info!(
                    "KNX outbound publisher started for group address: {}",
                    group_addr_clone
                );

                while let Ok(value_any) = reader.recv_any().await {
                    // Serialize the type-erased value
                    let bytes = match serializer(&*value_any) {
                        Ok(b) => b,
                        Err(_e) => {
                            #[cfg(feature = "tracing")]
                            tracing::error!(
                                "Failed to serialize for group address '{}': {:?}",
                                group_addr_clone,
                                _e
                            );
                            continue;
                        }
                    };

                    // Send GroupValueWrite
                    if let Err(_e) =
                        send_group_write(&gateway_ip, gateway_port, group_addr, &bytes).await
                    {
                        #[cfg(feature = "tracing")]
                        tracing::error!(
                            "Failed to publish to KNX group address '{}': {:?}",
                            group_addr_clone,
                            _e
                        );
                    } else {
                        #[cfg(feature = "tracing")]
                        tracing::debug!("Published to KNX group address: {}", group_addr_clone);
                    }
                }

                #[cfg(feature = "tracing")]
                tracing::info!(
                    "KNX outbound publisher stopped for group address: {}",
                    group_addr_clone
                );
            })?;
        }

        Ok(())
    }
}

// Implement the connector trait from aimdb-core
impl aimdb_core::transport::Connector for KnxConnectorImpl {
    fn publish(
        &self,
        destination: &str,
        _config: &aimdb_core::transport::ConnectorConfig,
        payload: &[u8],
    ) -> Pin<
        Box<
            dyn Future<Output = Result<(), aimdb_core::transport::PublishError>>
                + Send
                + '_,
        >,
    > {
        use aimdb_core::transport::PublishError;

        // Destination is the group address (from ConnectorUrl::resource_id())
        let group_addr_str = destination.to_string();
        let payload_owned = payload.to_vec();
        let gateway_ip = self.gateway_ip.clone();
        let gateway_port = self.gateway_port;

        Box::pin(async move {
            // Parse group address
            let group_addr = parse_group_address(&group_addr_str)
                .map_err(|_| PublishError::InvalidDestination)?;

            // Send GroupValueWrite
            send_group_write(&gateway_ip, gateway_port, group_addr, &payload_owned)
                .await
                .map_err(|_e| {
                    #[cfg(feature = "tracing")]
                    tracing::error!("KNX publish failed: {}", _e);

                    PublishError::ConnectionFailed
                })?;

            #[cfg(feature = "tracing")]
            tracing::debug!("Published to group address: {}", group_addr_str);
            Ok(())
        })
    }
}

/// Spawn the KNX connection task in the background with reconnection logic
///
/// The connection task handles:
/// - KNXnet/IP connection establishment
/// - Telegram reception and parsing
/// - Router-based dispatch to producers
/// - Automatic reconnection on failure
///
/// # Arguments
/// * `gateway_ip` - Gateway IP address
/// * `gateway_port` - Gateway port (typically 3671)
/// * `router` - Router for dispatching telegrams to producers
fn spawn_connection_task(gateway_ip: String, gateway_port: u16, router: Arc<Router>) {
    tokio::spawn(async move {
        #[cfg(feature = "tracing")]
        tracing::info!(
            "KNX connection task started for {}:{}",
            gateway_ip,
            gateway_port
        );

        loop {
            match connect_and_listen(&gateway_ip, gateway_port, router.clone()).await {
                Ok(_) => {
                    #[cfg(feature = "tracing")]
                    tracing::info!("KNX connection closed gracefully");
                }
                Err(_e) => {
                    #[cfg(feature = "tracing")]
                    tracing::error!(
                        "KNX connection failed: {:?}, reconnecting in 5s...",
                        _e
                    );
                }
            }

            // Wait before reconnecting
            tokio::time::sleep(Duration::from_secs(5)).await;
        }
    });
}

/// Connect to KNX gateway and listen for telegrams
///
/// This function implements the full KNXnet/IP Tunneling lifecycle:
/// 1. Create UDP socket
/// 2. Send CONNECT_REQUEST
/// 3. Receive CONNECT_RESPONSE (get channel_id)
/// 4. Loop: receive TUNNELING_REQUEST, parse, route, send ACK
///
/// # Arguments
/// * `gateway_ip` - Gateway IP address
/// * `gateway_port` - Gateway port
/// * `router` - Router for dispatching messages
async fn connect_and_listen(
    gateway_ip: &str,
    gateway_port: u16,
    router: Arc<Router>,
) -> Result<(), String> {
    // 1. Create UDP socket
    let socket = UdpSocket::bind("0.0.0.0:0")
        .await
        .map_err(|e| format!("Failed to bind UDP socket: {}", e))?;

    let local_addr = socket
        .local_addr()
        .map_err(|e| format!("Failed to get local address: {}", e))?;

    let gateway_addr: SocketAddr = format!("{}:{}", gateway_ip, gateway_port)
        .parse()
        .map_err(|e| format!("Invalid gateway address: {}", e))?;

    #[cfg(feature = "tracing")]
    tracing::debug!(
        "KNX: Connecting from {} to {}",
        local_addr,
        gateway_addr
    );

    // 2. Send CONNECT_REQUEST (using knx-pico types)
    let connect_req = build_connect_request(local_addr)?;
    socket
        .send_to(&connect_req, gateway_addr)
        .await
        .map_err(|e| format!("Failed to send CONNECT_REQUEST: {}", e))?;

    // 3. Wait for CONNECT_RESPONSE
    let mut buf = [0u8; 1024];
    let (len, _) = tokio::time::timeout(Duration::from_secs(5), socket.recv_from(&mut buf))
        .await
        .map_err(|_| "Timeout waiting for CONNECT_RESPONSE")?
        .map_err(|e| format!("Failed to receive CONNECT_RESPONSE: {}", e))?;

    let (channel_id, status) = parse_connect_response(&buf[..len])?;

    if status != 0 {
        return Err(format!(
            "Connection rejected by gateway, status: {}",
            status
        ));
    }

    #[cfg(feature = "tracing")]
    tracing::info!("✅ KNX connected, channel_id: {}", channel_id);

    // 4. Listen loop
    let mut seq_counter: u8 = 0;

    loop {
        let result = tokio::time::timeout(
            Duration::from_secs(30),
            socket.recv_from(&mut buf)
        )
        .await;

        match result {
            Ok(Ok((len, _))) => {
                // Parse telegram
                if let Some((group_addr, data)) = parse_telegram(&buf[..len]) {
                    let resource_id = format_group_address(group_addr);

                    #[cfg(feature = "tracing")]
                    tracing::debug!(
                        "KNX telegram: {} ({} bytes)",
                        resource_id,
                        data.len()
                    );

                    // Dispatch via router
                    if let Err(_e) = router.route(&resource_id, &data).await {
                        #[cfg(feature = "tracing")]
                        tracing::warn!("Router dispatch failed for {}: {:?}", resource_id, _e);
                    }
                }

                // Send ACK if TUNNELING_REQUEST
                if is_tunneling_request(&buf[..len]) {
                    seq_counter = seq_counter.wrapping_add(1);
                    let ack = build_tunneling_ack(channel_id, seq_counter);
                    let _ = socket.send_to(&ack, gateway_addr).await;
                }
            }
            Ok(Err(e)) => {
                return Err(format!("UDP error: {}", e));
            }
            Err(_) => {
                // Timeout - continue listening
                continue;
            }
        }
    }
}

/// Build KNXnet/IP CONNECT_REQUEST frame
fn build_connect_request(local_addr: SocketAddr) -> Result<Vec<u8>, String> {
    use std::net::IpAddr;

    // KNXnet/IP Header (6 bytes)
    let mut frame = vec![
        0x06,       // Header length
        0x10,       // Protocol version
        0x02, 0x05, // CONNECT_REQUEST
        0x00, 0x1A, // Total length (26 bytes)
    ];

    // Control endpoint HPAI (8 bytes)
    frame.extend_from_slice(&[
        0x08, // HPAI length
        0x01, // UDP protocol
    ]);

    // Local IP address
    match local_addr.ip() {
        IpAddr::V4(ip) => frame.extend_from_slice(&ip.octets()),
        _ => return Err("IPv6 not supported".to_string()),
    }

    // Local port
    frame.extend_from_slice(&local_addr.port().to_be_bytes());

    // Data endpoint HPAI (8 bytes) - same as control
    frame.extend_from_slice(&[
        0x08, // HPAI length
        0x01, // UDP protocol
    ]);

    match local_addr.ip() {
        IpAddr::V4(ip) => frame.extend_from_slice(&ip.octets()),
        _ => return Err("IPv6 not supported".to_string()),
    }

    frame.extend_from_slice(&local_addr.port().to_be_bytes());

    // Connection Request Information (4 bytes)
    frame.extend_from_slice(&[
        0x04, // Structure length
        0x04, // Connection type: TUNNEL_CONNECTION
        0x02, // KNX layer: TUNNEL_LINKLAYER
        0x00, // Reserved
    ]);

    Ok(frame)
}

/// Parse CONNECT_RESPONSE and extract channel_id and status
fn parse_connect_response(data: &[u8]) -> Result<(u8, u8), String> {
    if data.len() < 8 {
        return Err("CONNECT_RESPONSE too short".to_string());
    }

    // Verify service type (0x0206 = CONNECT_RESPONSE)
    if data[2] != 0x02 || data[3] != 0x06 {
        return Err("Not a CONNECT_RESPONSE".to_string());
    }

    let channel_id = data[6];
    let status = data[7];

    Ok((channel_id, status))
}

/// Build TUNNELING_ACK frame
fn build_tunneling_ack(channel_id: u8, seq_counter: u8) -> Vec<u8> {
    vec![
        0x06,       // Header length
        0x10,       // Protocol version
        0x04, 0x21, // TUNNELING_ACK
        0x00, 0x0A, // Total length (10 bytes)
        0x04,       // Connection header length
        channel_id,
        seq_counter,
        0x00,       // Status (OK)
    ]
}

/// Check if frame is a TUNNELING_REQUEST
fn is_tunneling_request(data: &[u8]) -> bool {
    data.len() >= 4 && data[2] == 0x04 && data[3] == 0x20
}

/// Parse KNX telegram and extract group address and data
///
/// Returns (group_address_raw, payload) if this is a valid L_Data.ind telegram
fn parse_telegram(data: &[u8]) -> Option<(u16, Vec<u8>)> {
    if data.len() < 20 {
        return None;
    }

    // Verify TUNNELING_REQUEST
    if !is_tunneling_request(data) {
        return None;
    }

    // cEMI frame starts at offset 10
    let cemi_start = 10;
    let message_code = data[cemi_start];

    // Only process L_Data.ind (0x29)
    if message_code != 0x29 {
        return None;
    }

    // Parse additional info length
    let add_info_len = data.get(cemi_start + 1).copied()? as usize;
    let addr_start = cemi_start + 2 + add_info_len;

    if data.len() < addr_start + 8 {
        return None;
    }

    // Control field 2 - check if group address
    let control2 = data[addr_start + 1];
    if (control2 & 0x80) == 0 {
        return None; // Physical address, skip
    }

    // Destination address (group)
    let dest_raw = u16::from_be_bytes([
        data[addr_start + 4],
        data[addr_start + 5],
    ]);

    // NPDU length
    let npdu_len = data.get(addr_start + 6).copied()? as usize;

    if npdu_len == 0 {
        return None;
    }

    // Extract payload
    let tpci_apci_pos = addr_start + 7;

    let payload = if npdu_len > 1 {
        // Multi-byte data
        if data.len() < tpci_apci_pos + npdu_len {
            return None;
        }
        data[tpci_apci_pos..tpci_apci_pos + npdu_len].to_vec()
    } else {
        // 6-bit data in APCI (short frame)
        vec![data.get(tpci_apci_pos + 1).copied()? & 0x3F]
    };

    Some((dest_raw, payload))
}

/// Send GroupValueWrite telegram to KNX gateway
///
/// Note: This is a simplified implementation that creates a new connection
/// for each write. A production implementation should reuse the connection
/// from the listen task.
async fn send_group_write(
    _gateway_ip: &str,
    _gateway_port: u16,
    _group_addr: u16,
    _data: &[u8],
) -> Result<(), String> {
    // TODO: Implement GroupValueWrite using knx-pico frame builders
    // For now, return error to indicate it's not implemented
    #[cfg(feature = "tracing")]
    tracing::warn!("GroupValueWrite not fully implemented yet");

    Err("GroupValueWrite not implemented".to_string())
}

/// Parse group address string "main/middle/sub" to raw u16
fn parse_group_address(addr_str: &str) -> Result<u16, String> {
    let parts: Vec<&str> = addr_str.split('/').collect();

    if parts.len() != 3 {
        return Err(format!("Invalid group address format: {}", addr_str));
    }

    let main: u8 = parts[0]
        .parse()
        .map_err(|_| format!("Invalid main group: {}", parts[0]))?;
    let middle: u8 = parts[1]
        .parse()
        .map_err(|_| format!("Invalid middle group: {}", parts[1]))?;
    let sub: u8 = parts[2]
        .parse()
        .map_err(|_| format!("Invalid sub group: {}", parts[2]))?;

    if main > 31 {
        return Err(format!("Main group must be 0-31, got {}", main));
    }
    if middle > 7 {
        return Err(format!("Middle group must be 0-7, got {}", middle));
    }

    // Encode: 5 bits main | 3 bits middle | 8 bits sub
    let raw = ((main as u16) << 11) | ((middle as u16) << 8) | (sub as u16);

    Ok(raw)
}

/// Format raw group address u16 to string "main/middle/sub"
fn format_group_address(raw: u16) -> String {
    let main = (raw >> 11) & 0x1F;
    let middle = (raw >> 8) & 0x07;
    let sub = raw & 0xFF;

    format!("{}/{}/{}", main, middle, sub)
}

#[cfg(test)]
mod tests {
    use super::*;
    use aimdb_core::router::RouterBuilder;

    #[tokio::test]
    async fn test_connector_creation_with_router() {
        let router = RouterBuilder::new().build();
        let connector =
            KnxConnectorImpl::build_internal("knx://192.168.1.19:3671", router).await;
        assert!(connector.is_ok());
    }

    #[tokio::test]
    async fn test_connector_with_port() {
        let router = RouterBuilder::new().build();
        let connector =
            KnxConnectorImpl::build_internal("knx://gateway.local:3672", router).await;
        assert!(connector.is_ok());
    }

    #[test]
    fn test_group_address_parsing() {
        assert_eq!(parse_group_address("1/0/7").unwrap(), 0x0807);
        assert_eq!(parse_group_address("0/0/0").unwrap(), 0x0000);
        assert_eq!(parse_group_address("31/7/255").unwrap(), 0xFFFF);

        assert!(parse_group_address("32/0/0").is_err());
        assert!(parse_group_address("0/8/0").is_err());
        assert!(parse_group_address("1/0").is_err());
    }

    #[test]
    fn test_group_address_formatting() {
        assert_eq!(format_group_address(0x0807), "1/0/7");
        assert_eq!(format_group_address(0x0000), "0/0/0");
        assert_eq!(format_group_address(0xFFFF), "31/7/255");
    }

    #[test]
    fn test_group_address_roundtrip() {
        let addresses = vec!["1/0/7", "0/0/0", "31/7/255", "5/3/128"];

        for addr in addresses {
            let raw = parse_group_address(addr).unwrap();
            let formatted = format_group_address(raw);
            assert_eq!(formatted, addr);
        }
    }
}
