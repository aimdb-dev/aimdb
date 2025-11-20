//! KNX/IP client management and lifecycle for Tokio runtime
//!
//! This module provides a KNX connector that:
//! - Manages a single KNX/IP gateway connection
//! - Automatic event loop spawning with reconnection
//! - Thread-safe access from multiple consumers
//! - Router-based dispatch for inbound telegrams

use crate::GroupAddress;
use aimdb_core::connector::ConnectorUrl;
use aimdb_core::router::{Router, RouterBuilder};
use aimdb_core::ConnectorBuilder;
use knx_pico::protocol::{
    CEMIFrame, ConnectRequest, ConnectResponse, ConnectionHeader, ConnectionStateRequest, Hpai,
    KnxnetIpFrame, ServiceType, TunnelingAck, TunnelingRequest,
};
use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::UdpSocket;
use tokio::sync::mpsc;

/// Command sent from outbound publishers to connection task
#[derive(Debug)]
enum KnxCommand {
    /// Send a GroupValueWrite telegram
    GroupWrite {
        group_addr: GroupAddress,
        data: Vec<u8>,
        /// Optional response channel for error reporting
        response: Option<tokio::sync::oneshot::Sender<Result<(), String>>>,
    },
}

/// Type alias for outbound route configuration
/// (resource_id, consumer, serializer, config_params)
type OutboundRoute = (
    String,
    Box<dyn aimdb_core::connector::ConsumerTrait>,
    aimdb_core::connector::SerializerFn,
    Vec<(String, String)>,
);

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
            let connector = KnxConnectorImpl::build_internal(&self.gateway_url, router)
                .await
                .map_err(|e| {
                    #[cfg(feature = "std")]
                    {
                        aimdb_core::DbError::RuntimeError {
                            message: format!("Failed to build KNX connector: {}", e),
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
    router: Arc<Router>,
    /// Command sender for outbound publishing
    command_tx: mpsc::Sender<KnxCommand>,
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
    async fn build_internal(gateway_url: &str, router: Router) -> Result<Self, String> {
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
        let command_tx =
            spawn_connection_task(gateway_ip.clone(), gateway_port, router_arc.clone());

        Ok(Self {
            router: router_arc,
            command_tx,
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
    /// and publishes to the KNX gateway via the command queue.
    fn spawn_outbound_publishers<R>(
        &self,
        db: &aimdb_core::builder::AimDb<R>,
        routes: Vec<OutboundRoute>,
    ) -> aimdb_core::DbResult<()>
    where
        R: aimdb_executor::Spawn + 'static,
    {
        let runtime = db.runtime();

        for (group_addr_str, consumer, serializer, _config) in routes {
            let command_tx = self.command_tx.clone();
            let group_addr_clone = group_addr_str.clone();

            runtime.spawn(async move {
                // Parse group address using knx-pico's type-safe parser
                let group_addr = match group_addr_clone.parse::<GroupAddress>() {
                    Ok(addr) => addr,
                    Err(_e) => {
                        #[cfg(feature = "tracing")]
                        tracing::error!(
                            "Invalid group address for outbound: '{}'",
                            group_addr_clone
                        );
                        return;
                    }
                };

                // Subscribe to typed values (type-erased)
                let mut reader = match consumer.subscribe_any().await {
                    Ok(r) => r,
                    Err(_e) => {
                        #[cfg(feature = "tracing")]
                        tracing::error!("Failed to subscribe for outbound: '{}'", group_addr_clone);
                        return;
                    }
                };

                #[cfg(feature = "tracing")]
                tracing::info!("KNX outbound publisher started for: {}", group_addr_clone);

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

                    // Send command to connection task
                    let cmd = KnxCommand::GroupWrite {
                        group_addr,
                        data: bytes,
                        response: None, // Fire-and-forget
                    };

                    if let Err(_e) = command_tx.send(cmd).await {
                        #[cfg(feature = "tracing")]
                        tracing::error!(
                            "Failed to send command for group address '{}': channel closed",
                            group_addr_clone
                        );
                        break; // Connection task died, stop publishing
                    }

                    #[cfg(feature = "tracing")]
                    tracing::debug!("Published to KNX: {}", group_addr_clone);
                }

                #[cfg(feature = "tracing")]
                tracing::info!("KNX outbound publisher stopped for: {}", group_addr_clone);
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
    ) -> Pin<Box<dyn Future<Output = Result<(), aimdb_core::transport::PublishError>> + Send + '_>>
    {
        use aimdb_core::transport::PublishError;

        // Destination is the group address (from ConnectorUrl::resource_id())
        let group_addr_str = destination.to_string();
        let payload_owned = payload.to_vec();
        let command_tx = self.command_tx.clone();

        Box::pin(async move {
            // Parse group address using knx-pico's type-safe parser
            let group_addr = group_addr_str
                .parse::<GroupAddress>()
                .map_err(|_| PublishError::InvalidDestination)?;

            // Create response channel for error reporting
            let (response_tx, response_rx) = tokio::sync::oneshot::channel();

            // Send command to connection task
            let cmd = KnxCommand::GroupWrite {
                group_addr,
                data: payload_owned,
                response: Some(response_tx),
            };

            command_tx
                .send(cmd)
                .await
                .map_err(|_| PublishError::ConnectionFailed)?;

            // Wait for response from connection task
            response_rx
                .await
                .map_err(|_| PublishError::ConnectionFailed)?
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
/// - Outbound command processing
/// - Automatic reconnection on failure
///
/// # Arguments
/// * `gateway_ip` - Gateway IP address
/// * `gateway_port` - Gateway port (typically 3671)
/// * `router` - Router for dispatching telegrams to producers
///
/// # Returns
/// * Command sender for publishing outbound telegrams
fn spawn_connection_task(
    gateway_ip: String,
    gateway_port: u16,
    router: Arc<Router>,
) -> mpsc::Sender<KnxCommand> {
    let (command_tx, mut command_rx) = mpsc::channel(32); // Queue size: 32

    tokio::spawn(async move {
        #[cfg(feature = "tracing")]
        tracing::info!(
            "KNX connection task started for {}:{}",
            gateway_ip,
            gateway_port
        );

        loop {
            match connect_and_listen(&gateway_ip, gateway_port, router.clone(), &mut command_rx)
                .await
            {
                Ok(_) => {
                    #[cfg(feature = "tracing")]
                    tracing::info!("KNX connection closed gracefully");
                }
                Err(_e) => {
                    #[cfg(feature = "tracing")]
                    tracing::error!("KNX connection failed: {:?}, reconnecting in 5s...", _e);
                }
            }

            // Wait before reconnecting
            tokio::time::sleep(Duration::from_secs(5)).await;
        }
    });

    command_tx
}

/// Build CONNECTIONSTATE_REQUEST for heartbeat using knx-pico
fn build_connectionstate_request(channel_id: u8) -> Vec<u8> {
    // Use 0.0.0.0:0 for "any" address
    let hpai = Hpai::new([0, 0, 0, 0], 0);
    let request = ConnectionStateRequest::new(channel_id, hpai);

    let mut buffer = [0u8; 32];
    let len = request
        .build(&mut buffer)
        .expect("Buffer too small for CONNECTIONSTATE_REQUEST");
    buffer[..len].to_vec()
}

/// Pending ACK for outbound telegram
struct PendingAck {
    sent_at: std::time::Instant,
    response_tx: Option<tokio::sync::oneshot::Sender<Result<(), String>>>,
}

/// Connection state shared within the connection task
struct ChannelState {
    /// KNXnet/IP channel ID from CONNECT_RESPONSE
    channel_id: u8,

    /// Last received sequence counter (inbound telegrams)
    inbound_seq: u8,

    /// Next sequence counter to use for outbound telegrams
    outbound_seq: u8,

    /// Pending ACKs waiting for confirmation (seq -> PendingAck)
    pending_acks: std::collections::HashMap<u8, PendingAck>,
}

impl ChannelState {
    fn new(channel_id: u8) -> Self {
        Self {
            channel_id,
            inbound_seq: 0,
            outbound_seq: 0,
            pending_acks: std::collections::HashMap::new(),
        }
    }

    fn next_outbound_seq(&mut self) -> u8 {
        let seq = self.outbound_seq;
        self.outbound_seq = self.outbound_seq.wrapping_add(1);
        seq
    }

    /// Track a pending ACK for an outbound telegram
    fn add_pending_ack(
        &mut self,
        seq: u8,
        response_tx: Option<tokio::sync::oneshot::Sender<Result<(), String>>>,
    ) {
        self.pending_acks.insert(
            seq,
            PendingAck {
                sent_at: std::time::Instant::now(),
                response_tx,
            },
        );
    }

    /// Complete a pending ACK (received confirmation)
    fn complete_ack(&mut self, seq: u8) -> bool {
        if let Some(pending) = self.pending_acks.remove(&seq) {
            if let Some(tx) = pending.response_tx {
                let _ = tx.send(Ok(()));
            }
            true
        } else {
            false
        }
    }

    /// Check for timed-out ACKs (> 3 seconds)
    fn check_ack_timeouts(&mut self) -> Vec<u8> {
        let now = std::time::Instant::now();
        let mut timed_out = Vec::new();

        self.pending_acks.retain(|&seq, pending| {
            if now.duration_since(pending.sent_at) > Duration::from_secs(3) {
                timed_out.push(seq);
                if let Some(tx) = pending.response_tx.take() {
                    let _ = tx.send(Err(format!("ACK timeout for seq={}", seq)));
                }
                false // Remove from pending
            } else {
                true // Keep waiting
            }
        });

        timed_out
    }
}

/// Connect to KNX gateway and listen for telegrams
///
/// This function implements the full KNXnet/IP Tunneling lifecycle:
/// 1. Create UDP socket
/// 2. Send CONNECT_REQUEST
/// 3. Receive CONNECT_RESPONSE (get channel_id)
/// 4. Loop: receive TUNNELING_REQUEST, parse, route, send ACK
///    and process outbound commands from the command queue
///
/// # Arguments
/// * `gateway_ip` - Gateway IP address
/// * `gateway_port` - Gateway port
/// * `router` - Router for dispatching messages
/// * `command_rx` - Command receiver for outbound publishing
async fn connect_and_listen(
    gateway_ip: &str,
    gateway_port: u16,
    router: Arc<Router>,
    command_rx: &mut mpsc::Receiver<KnxCommand>,
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
    tracing::debug!("KNX: Connecting from {} to {}", local_addr, gateway_addr);

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

    // 4. Listen loop with command queue and ACK timeout checking
    let mut channel_state = ChannelState::new(channel_id);
    let mut heartbeat_interval = tokio::time::interval(Duration::from_secs(55));
    heartbeat_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    // ACK timeout checker (runs every 500ms)
    let mut ack_timeout_interval = tokio::time::interval(Duration::from_millis(500));
    ack_timeout_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        tokio::select! {
            // Inbound: Receive telegrams from gateway
            result = socket.recv_from(&mut buf) => {
                match result {
                    Ok((len, _)) => {
                        #[cfg(feature = "tracing")]
                        tracing::trace!("Received {} bytes from gateway", len);

                        // Check if this is a TUNNELING_ACK for our outbound telegram
                        if is_tunneling_ack(&buf[..len]) {
                            #[cfg(feature = "tracing")]
                            tracing::debug!("Received TUNNELING_ACK: {:02X?}", &buf[..len]);

                            // Parse ACK - try knx-pico parser first, fallback to manual parsing
                            // Some gateways send non-standard ACK format (missing status byte)
                            let ack_seq = if let Ok(frame) = KnxnetIpFrame::parse(&buf[..len]) {
                                if let Ok(ack) = TunnelingAck::parse(frame.body()) {
                                    // Standard parsing succeeded
                                    ack.connection_header.sequence_counter
                                } else if frame.body().len() >= 4 {
                                    // Fallback: manually extract sequence from ConnectionHeader
                                    // Body format: [struct_len, channel_id, seq, status]
                                    // Gateway may send 4 bytes instead of 5 (missing final status byte)
                                    let seq = frame.body()[2];

                                    #[cfg(feature = "tracing")]
                                    tracing::debug!("Using fallback ACK parsing (non-standard gateway format)");

                                    seq
                                } else {
                                    #[cfg(feature = "tracing")]
                                    tracing::warn!("Failed to parse TUNNELING_ACK body, raw: {:02X?}", &buf[..len]);
                                    continue;
                                }
                            } else {
                                #[cfg(feature = "tracing")]
                                tracing::warn!("Failed to parse frame as TUNNELING_ACK, raw: {:02X?}", &buf[..len]);
                                continue;
                            };

                            // Complete the pending ACK
                            if channel_state.complete_ack(ack_seq) {
                                #[cfg(feature = "tracing")]
                                tracing::trace!("✅ Received TUNNELING_ACK for seq={}", ack_seq);
                            } else {
                                #[cfg(feature = "tracing")]
                                tracing::warn!("⚠️  Received unexpected TUNNELING_ACK for seq={}", ack_seq);
                            }

                            continue; // Don't process ACKs as data telegrams
                        } else {
                            #[cfg(feature = "tracing")]
                            tracing::trace!("Frame is not TUNNELING_ACK, checking if telegram...");
                        }

                        // Parse telegram
                        if let Some((group_addr, data)) = parse_telegram(&buf[..len]) {
                            let resource_id = group_addr.to_string();

                            #[cfg(feature = "tracing")]
                            tracing::debug!("KNX telegram: {} ({} bytes)", resource_id, data.len());

                            // Dispatch via router
                            if let Err(_e) = router.route(&resource_id, &data).await {
                                #[cfg(feature = "tracing")]
                                tracing::warn!("Router dispatch failed for {}: {:?}", resource_id, _e);
                            }
                        } else {
                            #[cfg(feature = "tracing")]
                            tracing::trace!("Ignoring non-GroupWrite or invalid telegram");
                        }

                        // Send ACK if TUNNELING_REQUEST
                        if is_tunneling_request(&buf[..len]) {
                            // Extract received sequence from telegram
                            let recv_seq = if len > 8 { buf[8] } else { 0 };
                            channel_state.inbound_seq = recv_seq;

                            let ack = build_tunneling_ack(channel_state.channel_id, recv_seq);
                            let _ = socket.send_to(&ack, gateway_addr).await;

                            #[cfg(feature = "tracing")]
                            tracing::trace!("Sent TUNNELING_ACK with seq={}", recv_seq);
                        }
                    }
                    Err(e) => {
                        return Err(format!("Socket error: {}", e));
                    }
                }
            }

            // Outbound: Process commands from queue
            Some(cmd) = command_rx.recv() => {
                let KnxCommand::GroupWrite { group_addr, data, response } = cmd;

                // Send the telegram (this increments outbound_seq internally)
                let seq_before = channel_state.outbound_seq;

                let result = send_group_write_internal(
                    &socket,
                    gateway_addr,
                    &mut channel_state,
                    group_addr,
                    &data,
                ).await;

                // If send succeeded, always track pending ACK (even for fire-and-forget)
                if result.is_ok() {
                    channel_state.add_pending_ack(seq_before, response);
                } else if let Some(tx) = response {
                    // Send immediate response if send failed
                    let _ = tx.send(result);
                } else if let Err(_e) = result {
                    #[cfg(feature = "tracing")]
                    tracing::error!("GroupWrite failed: {}", _e);
                }
            }

            // Heartbeat: Send CONNECTIONSTATE_REQUEST every 55s
            _ = heartbeat_interval.tick() => {
                #[cfg(feature = "tracing")]
                tracing::trace!("Sending heartbeat (CONNECTIONSTATE_REQUEST)");

                let heartbeat = build_connectionstate_request(channel_state.channel_id);
                if let Err(e) = socket.send_to(&heartbeat, gateway_addr).await {
                    #[cfg(feature = "tracing")]
                    tracing::error!("Failed to send heartbeat: {}", e);
                    return Err(format!("Heartbeat send failed: {}", e));
                }
            }

            // ACK timeout checker: Check for expired ACKs every 500ms
            _ = ack_timeout_interval.tick() => {
                let timed_out = channel_state.check_ack_timeouts();
                if !timed_out.is_empty() {
                    #[cfg(feature = "tracing")]
                    tracing::warn!("⚠️  ACK timeouts for sequences: {:?}", timed_out);
                }
            }
        }
    }
}

/// Build KNXnet/IP CONNECT_REQUEST frame using knx-pico
fn build_connect_request(local_addr: SocketAddr) -> Result<Vec<u8>, String> {
    use std::net::IpAddr;

    // Convert local address to Hpai
    let ip_bytes = match local_addr.ip() {
        IpAddr::V4(ip) => ip.octets(),
        _ => return Err("IPv6 not supported".to_string()),
    };

    let hpai = Hpai::new(ip_bytes, local_addr.port());
    let request = ConnectRequest::new(hpai, hpai);

    let mut buffer = [0u8; 32];
    let len = request
        .build(&mut buffer)
        .map_err(|e| format!("Failed to build CONNECT_REQUEST: {:?}", e))?;

    Ok(buffer[..len].to_vec())
}

/// Parse CONNECT_RESPONSE using knx-pico and extract channel_id and status
fn parse_connect_response(data: &[u8]) -> Result<(u8, u8), String> {
    let frame =
        KnxnetIpFrame::parse(data).map_err(|e| format!("Failed to parse frame: {:?}", e))?;

    if frame.service_type() != ServiceType::ConnectResponse {
        return Err(format!(
            "Not a CONNECT_RESPONSE, got: {:?}",
            frame.service_type()
        ));
    }

    let response = ConnectResponse::parse(frame.body())
        .map_err(|e| format!("Failed to decode CONNECT_RESPONSE: {:?}", e))?;

    Ok((response.channel_id, response.status))
}

/// Build TUNNELING_ACK frame using knx-pico
fn build_tunneling_ack(channel_id: u8, seq_counter: u8) -> Vec<u8> {
    let conn_header = ConnectionHeader::new(channel_id, seq_counter);
    let ack = TunnelingAck::new(conn_header, 0); // status = 0 (OK)
    let mut buffer = [0u8; 16];
    let len = ack
        .build(&mut buffer)
        .expect("Buffer too small for TUNNELING_ACK");
    buffer[..len].to_vec()
}

/// Check if frame is a TUNNELING_REQUEST using knx-pico
fn is_tunneling_request(data: &[u8]) -> bool {
    if let Ok(frame) = KnxnetIpFrame::parse(data) {
        frame.service_type() == ServiceType::TunnellingRequest
    } else {
        false
    }
}

/// Check if frame is a TUNNELING_ACK using knx-pico
fn is_tunneling_ack(data: &[u8]) -> bool {
    if let Ok(frame) = KnxnetIpFrame::parse(data) {
        frame.service_type() == ServiceType::TunnellingAck
    } else {
        false
    }
}

/// Parse KNX telegram using knx-pico and extract group address and data
///
/// Returns (group_address, payload) if this is a valid L_Data.ind telegram
fn parse_telegram(data: &[u8]) -> Option<(GroupAddress, Vec<u8>)> {
    // Parse KNXnet/IP frame
    let frame = KnxnetIpFrame::parse(data).ok()?;

    // Only process TUNNELLING_REQUEST
    if frame.service_type() != ServiceType::TunnellingRequest {
        return None;
    }

    // Parse tunneling request to get cEMI
    let tunneling_req = TunnelingRequest::parse(frame.body()).ok()?;

    // Parse cEMI frame
    let cemi = CEMIFrame::parse(tunneling_req.cemi_data).ok()?;

    // Only process L_Data frames
    if !cemi.is_ldata() {
        return None;
    }

    // Parse LData frame using knx-pico (handles all encoding variants including 6-bit values)
    let ldata = match cemi.as_ldata() {
        Ok(l) => l,
        Err(_e) => {
            #[cfg(feature = "tracing")]
            tracing::warn!("Failed to parse L_Data frame: {:?}", _e);
            return None;
        }
    };

    #[cfg(feature = "tracing")]
    {
        let dest_addr = ldata.destination_raw;
        let npdu_len = ldata.npdu_length;
        tracing::trace!(
            "LData parsed: dest={:04X}, npdu_len={}, ldata.data.len()={}",
            dest_addr,
            npdu_len,
            ldata.data.len()
        );
    }

    // Only process group write commands
    if !ldata.is_group_write() {
        return None;
    }

    // Only process group addresses (not individual addresses)
    let dest = ldata.destination_group()?;

    // Extract payload (application data)
    // For 6-bit encoded values (DPT1 boolean), ldata.data is empty
    // and the value is encoded in the APCI byte. We need to extract it manually.
    // Note: npdu_length can be 1 (combined TPCI+APCI) or 2 (separate TPCI and APCI)
    let payload = if ldata.data.is_empty() {
        // 6-bit encoding: extract value from APCI byte in raw cEMI data
        // cEMI structure: [msg_code, add_info_len, <add_info>, ctrl1, ctrl2, src(2), dest(2), npdu_len, tpci, apci, ...]
        // APCI byte position = 2 + add_info_len + 6 (ctrl1, ctrl2, src(2), dest(2), npdu_len) + 1 (tpci) = 2 + add_info_len + 7 + 1
        let cemi_data = tunneling_req.cemi_data;
        let add_info_len = if cemi_data.len() > 1 { cemi_data[1] } else { 0 } as usize;
        let apci_pos = 2 + add_info_len + 8; // TPCI is at +7, APCI is at +8

        if cemi_data.len() > apci_pos {
            let apci_byte = cemi_data[apci_pos];
            let value = apci_byte & 0x3F; // Extract 6-bit value

            #[cfg(feature = "tracing")]
            tracing::debug!(
                "6-bit decoding: apci_byte={:02X}, extracted_value={:02X}, add_info_len={}, apci_pos={}",
                apci_byte, value, add_info_len, apci_pos
            );

            vec![value]
        } else {
            vec![]
        }
    } else {
        // Standard encoding: multi-byte data (DPT5, DPT7, DPT9, etc.)
        //
        // cEMI L_Data structure (after msg_code and add_info):
        // [0] ctrl1, [1] ctrl2, [2-3] src, [4-5] dest, [6] npdu_len, [7] TPCI, [8] APCI_low, [9+] data
        //
        // According to knx-pico parser: data starts at position 9 in L_Data
        // In full cEMI frame: position = 2 + add_info_len + 9 = 11 (when add_info_len=0)
        let cemi_data = tunneling_req.cemi_data;
        let add_info_len = if cemi_data.len() > 1 { cemi_data[1] } else { 0 } as usize;

        // Data starts at: msg_code(0) + add_info_len_field(1) + add_info(variable) + L_Data_header(9)
        let ldata_offset = 2 + add_info_len;
        let data_start = ldata_offset + 9; // Position 11 when add_info_len=0

        #[cfg(feature = "tracing")]
        {
            let npdu_len_pos = ldata_offset + 6;
            let tpci_pos = ldata_offset + 7;
            let apci_pos = ldata_offset + 8;

            tracing::debug!(
                "cEMI: len={}, add_info_len={}, NPDU_len@{}={:02X}, TPCI@{}={:02X}, APCI@{}={:02X}, Data@{}+={:02X?}",
                cemi_data.len(),
                add_info_len,
                npdu_len_pos, cemi_data[npdu_len_pos],
                tpci_pos, cemi_data[tpci_pos],
                apci_pos, cemi_data[apci_pos],
                data_start, &cemi_data[data_start..]
            );
        }

        let extracted = if cemi_data.len() > data_start {
            cemi_data[data_start..].to_vec()
        } else {
            // Fallback to knx-pico's parsed data if extraction fails
            ldata.data.to_vec()
        };

        #[cfg(feature = "tracing")]
        tracing::debug!("Extracted {} bytes: {:02X?}", extracted.len(), extracted);

        extracted
    };

    #[cfg(feature = "tracing")]
    tracing::trace!(
        "Parsed telegram for {}: {} payload bytes: {:02X?}",
        dest,
        payload.len(),
        payload
    );

    Some((dest, payload))
}

/// Build GroupValueWrite cEMI frame (L_Data.req)
///
/// Must match knx-pico's exact cEMI structure for proper parsing.
/// Structure: [msg_code, add_info_len, ctrl1, ctrl2, src(2), dest(2), npdu_len, tpci, apci, data...]
fn build_group_write_cemi(group_addr: GroupAddress, data: &[u8]) -> Vec<u8> {
    let mut frame = Vec::with_capacity(16);

    // Message code: L_Data.req (0x11)
    frame.push(0x11);

    // Additional info length: 0
    frame.push(0x00);

    // Control field 1: 0xBC (Standard frame, no repeat, broadcast, priority low)
    // Use 0xBC instead of 0x94 - this is critical for gateway compatibility
    frame.push(0xBC);

    // Control field 2: 0xE0 (Group address, hop count 6)
    frame.push(0xE0);

    // Source address: 0.0.0 (2 bytes, big-endian)
    frame.extend_from_slice(&[0x00, 0x00]);

    // Destination address (group address) - convert to u16 big-endian
    let dest_raw: u16 = group_addr.into();
    let dest_bytes = dest_raw.to_be_bytes();
    frame.extend_from_slice(&dest_bytes);

    // Build NPDU: NPDU_length field + TPCI + APCI + data
    // CRITICAL: NPDU length encoding per KNX spec:
    // - For short telegram: field = 0x01 (special flag)
    // - For long telegram: field = actual_length - 1 (encoded as length-1)
    if data.len() == 1 && data[0] < 64 {
        // 6-bit encoding: value embedded in APCI byte
        // NPDU length = 0x01 (short telegram flag, NOT byte count)
        frame.push(0x01);

        // TPCI (UnnumberedData)
        frame.push(0x00);

        // APCI low byte: GroupValueWrite (0x80) + 6-bit value
        frame.push(0x80 | (data[0] & 0x3F));
    } else {
        // Long telegram: APCI + separate data bytes
        // NPDU length encoding: field = actual_length - 1
        let npdu_actual = 2 + data.len(); // TPCI + APCI + data
        let npdu_len_field = npdu_actual - 1; // Encode as length - 1
        frame.push(npdu_len_field as u8);

        // TPCI (UnnumberedData)
        frame.push(0x00);

        // APCI: GroupValueWrite
        frame.push(0x80);

        // Data bytes
        frame.extend_from_slice(data);
    }

    frame
}

/// Build TUNNELING_REQUEST containing cEMI frame using knx-pico
fn build_tunneling_request(channel_id: u8, seq: u8, cemi: &[u8]) -> Vec<u8> {
    let conn_header = ConnectionHeader::new(channel_id, seq);
    let request = TunnelingRequest::new(conn_header, cemi);
    let mut buffer = [0u8; 256];
    let len = request
        .build(&mut buffer)
        .expect("Buffer too small for TUNNELING_REQUEST");
    buffer[..len].to_vec()
}

/// Send GroupValueWrite telegram (internal, called from connection task)
async fn send_group_write_internal(
    socket: &UdpSocket,
    gateway_addr: SocketAddr,
    channel_state: &mut ChannelState,
    group_addr: GroupAddress,
    data: &[u8],
) -> Result<(), String> {
    // Build cEMI frame
    let cemi = build_group_write_cemi(group_addr, data);

    #[cfg(feature = "tracing")]
    tracing::debug!(
        "Built cEMI frame for {} ({} data bytes): {:02X?}",
        group_addr,
        data.len(),
        &cemi
    );

    // Get next sequence number
    let seq = channel_state.next_outbound_seq();

    // Build TUNNELING_REQUEST
    let telegram = build_tunneling_request(channel_state.channel_id, seq, &cemi);

    #[cfg(feature = "tracing")]
    tracing::debug!(
        "Built TUNNELING_REQUEST: channel={}, seq={}, total_len={} bytes: {:02X?}",
        channel_state.channel_id,
        seq,
        telegram.len(),
        &telegram
    );

    // Send via UDP
    socket
        .send_to(&telegram, gateway_addr)
        .await
        .map_err(|e| format!("Send failed: {}", e))?;

    #[cfg(feature = "tracing")]
    tracing::debug!(
        "Sent GroupWrite: {} seq={} ({} bytes)",
        group_addr, // GroupAddress implements Display
        seq,
        data.len()
    );

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use aimdb_core::router::RouterBuilder;

    #[tokio::test]
    async fn test_connector_creation_with_router() {
        let router = RouterBuilder::new().build();
        let connector = KnxConnectorImpl::build_internal("knx://192.168.1.19:3671", router).await;
        assert!(connector.is_ok());
    }

    #[tokio::test]
    async fn test_connector_with_port() {
        let router = RouterBuilder::new().build();
        let connector = KnxConnectorImpl::build_internal("knx://gateway.local:3672", router).await;
        assert!(connector.is_ok());
    }

    #[test]
    fn test_group_address_parsing() {
        // Test using knx-pico's GroupAddress parser
        assert_eq!("1/0/7".parse::<GroupAddress>().unwrap().raw(), 0x0807);
        assert_eq!("0/0/0".parse::<GroupAddress>().unwrap().raw(), 0x0000);
        assert_eq!("31/7/255".parse::<GroupAddress>().unwrap().raw(), 0xFFFF);

        // knx-pico supports both 3-level (main/middle/sub) and 2-level (main/sub) formats
        assert!("1/0".parse::<GroupAddress>().is_ok()); // 2-level format is valid

        // Invalid formats
        assert!("32/0/0".parse::<GroupAddress>().is_err()); // main > 31
        assert!("0/8/0".parse::<GroupAddress>().is_err()); // middle > 7 in 3-level
        assert!("invalid".parse::<GroupAddress>().is_err()); // not a number
    }

    #[test]
    fn test_group_address_formatting() {
        // Test using knx-pico's GroupAddress Display impl
        assert_eq!(GroupAddress::from(0x0807).to_string(), "1/0/7");
        assert_eq!(GroupAddress::from(0x0000).to_string(), "0/0/0");
        assert_eq!(GroupAddress::from(0xFFFF).to_string(), "31/7/255");
    }

    #[test]
    fn test_group_address_roundtrip() {
        let addresses = vec!["1/0/7", "0/0/0", "31/7/255", "5/3/128"];

        for addr in addresses {
            let parsed = addr.parse::<GroupAddress>().unwrap();
            let formatted = parsed.to_string();
            assert_eq!(formatted, addr);
        }
    }
}
