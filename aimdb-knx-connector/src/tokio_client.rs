//! KNX/IP client management and lifecycle for Tokio runtime
//!
//! This module provides a KNX connector that:
//! - Manages a single KNX/IP gateway connection (with reconnection)
//! - Rides core's `pump_sink` / `pump_source`: a `KnxSink` (outbound
//!   `GroupValueWrite`) and a `KnxSource` (inbound telegrams) over the
//!   connection task's command / telegram channels

use crate::GroupAddress;
use aimdb_core::connector::ConnectorUrl;
use aimdb_core::transport::{Connector, ConnectorConfig, PublishError};
use aimdb_core::{pump_sink, pump_source, BoxFut, ConnectorBuilder, Payload, Source};
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

/// KNX connector for a single gateway connection.
///
/// Each connector manages ONE KNX/IP gateway connection; inbound telegrams are
/// dispatched to AimDB producers by `pump_source`, outbound records published by
/// `pump_sink`.
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
    /// Capacity of the mpsc channel between outbound publishers and the
    /// connection task. Defaults to 32.
    command_queue_size: usize,
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
            command_queue_size: 32,
        }
    }

    /// Override the internal command channel capacity (default: 32).
    ///
    /// The channel sits between outbound publisher futures and the single
    /// connection task that serializes UDP sends. Increase this for
    /// installations with many outbound routes or bursty publish patterns.
    pub fn with_command_queue_size(mut self, size: usize) -> Self {
        self.command_queue_size = size;
        self
    }
}

type BoxFuture = Pin<Box<dyn Future<Output = ()> + Send + 'static>>;

impl<R: aimdb_executor::RuntimeAdapter + 'static> ConnectorBuilder<R> for KnxConnectorBuilder {
    fn build<'a>(
        &'a self,
        db: &'a aimdb_core::builder::AimDb<R>,
    ) -> Pin<Box<dyn Future<Output = aimdb_core::DbResult<Vec<BoxFuture>>> + Send + 'a>> {
        Box::pin(async move {
            // Build the command channel, the inbound-telegram channel, and the
            // connection task. Inbound flows connection-task → `KnxSource` →
            // `pump_source`; outbound flows `pump_sink` → `KnxSink` → the command
            // channel → connection task. The routing `Router` is (re)built inside
            // `pump_source` from `collect_inbound_routes`.
            let (command_tx, telegram_rx, connection_future) =
                KnxConnectorImpl::build_internal(&self.gateway_url, self.command_queue_size)
                    .await
                    .map_err(|e| {
                        aimdb_core::DbError::runtime_error(format!(
                            "Failed to build KNX connector: {}",
                            e
                        ))
                    })?;

            let mut futures: Vec<BoxFuture> = vec![connection_future];
            // Inbound: the KNX bus source, fanned out to producers by `pump_source`.
            futures.extend(pump_source(db, "knx", KnxSource { telegram_rx }));
            // Outbound: `pump_sink` serializes each record and hands it to `KnxSink`.
            futures.extend(pump_sink(db, "knx", Arc::new(KnxSink { command_tx })));

            Ok(futures)
        })
    }

    fn scheme(&self) -> &str {
        "knx"
    }
}

/// Build-time helper aggregating KNX construction logic.
///
/// `KnxConnectorBuilder::build()` produces a `Vec<BoxFuture>` containing one
/// connection-task future plus one publisher future per outbound route. There
/// is no longer a long-lived `KnxConnectorImpl` value — the previous
/// `Connector::publish` direct-publish path was unreachable through the
/// public API (`AimDbBuilder` discarded the `Arc<dyn Connector>`).
pub struct KnxConnectorImpl;

impl KnxConnectorImpl {
    /// Builds the KNX connection-task future, returning the outbound command
    /// sender and the inbound-telegram receiver for `KnxSink` / `KnxSource`.
    ///
    /// # Arguments
    /// * `gateway_url` - Gateway URL (knx://host:port)
    /// * `command_queue_size` - Capacity of both the command and telegram channels
    async fn build_internal(
        gateway_url: &str,
        command_queue_size: usize,
    ) -> Result<
        (
            mpsc::Sender<KnxCommand>,
            mpsc::Receiver<(String, Payload)>,
            BoxFuture,
        ),
        String,
    > {
        // Parse the gateway URL
        let mut url = gateway_url.to_string();
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

        // Outbound commands (publishers → connection task) and inbound telegrams
        // (connection task → `KnxSource`/`pump_source`).
        let (command_tx, command_rx) = mpsc::channel::<KnxCommand>(command_queue_size);
        let (telegram_tx, telegram_rx) = mpsc::channel::<(String, Payload)>(command_queue_size);

        let connection_future =
            build_connection_future(gateway_ip, gateway_port, telegram_tx, command_rx);

        Ok((command_tx, telegram_rx, connection_future))
    }
}

/// Outbound publish adapter driven by `pump_sink`.
///
/// `pump_sink` resolves each record's destination group address (dynamic via a
/// topic provider, or the link's default) and serializes the value; `publish`
/// parses that address and forwards a fire-and-forget `GroupValueWrite` to the
/// connection task over the command channel.
struct KnxSink {
    command_tx: mpsc::Sender<KnxCommand>,
}

impl Connector for KnxSink {
    fn publish(
        &self,
        destination: &str,
        _config: &ConnectorConfig,
        payload: &[u8],
    ) -> Pin<Box<dyn Future<Output = Result<(), PublishError>> + Send + '_>> {
        let group_addr_str = destination.to_string();
        let data = payload.to_vec();
        let command_tx = self.command_tx.clone();
        Box::pin(async move {
            let group_addr = group_addr_str
                .parse::<GroupAddress>()
                .map_err(|_| PublishError::InvalidDestination)?;
            command_tx
                .send(KnxCommand::GroupWrite {
                    group_addr,
                    data,
                    response: None, // fire-and-forget
                })
                .await
                .map_err(|_| PublishError::ConnectionFailed) // connection task gone
        })
    }
}

/// Inbound telegram source driven by `pump_source`.
///
/// Yields each `(group_address, payload)` the connection task parsed off the KNX
/// bus; `pump_source` deserializes and fans it out to the matching producers.
struct KnxSource {
    telegram_rx: mpsc::Receiver<(String, Payload)>,
}

impl Source for KnxSource {
    fn next(&mut self) -> BoxFut<'_, Option<(String, Payload)>> {
        Box::pin(async move { self.telegram_rx.recv().await })
    }
}

/// Builds the KNX connection-task future with reconnection logic.
///
/// The connection task handles:
/// - KNXnet/IP connection establishment
/// - Telegram reception and parsing
/// - Forwarding parsed inbound telegrams to the `telegram_tx` channel (`pump_source`)
/// - Outbound command processing
/// - Automatic reconnection on failure
///
/// # Arguments
/// * `gateway_ip` - Gateway IP address
/// * `gateway_port` - Gateway port (typically 3671)
/// * `telegram_tx` - Sender for inbound telegrams → `KnxSource`/`pump_source`
/// * `command_rx` - Receiver half of the outbound command channel
fn build_connection_future(
    gateway_ip: String,
    gateway_port: u16,
    telegram_tx: mpsc::Sender<(String, Payload)>,
    mut command_rx: mpsc::Receiver<KnxCommand>,
) -> BoxFuture {
    Box::pin(async move {
        #[cfg(feature = "tracing")]
        tracing::info!(
            "KNX connection task started for {}:{}",
            gateway_ip,
            gateway_port
        );

        loop {
            match connect_and_listen(&gateway_ip, gateway_port, &telegram_tx, &mut command_rx).await
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
    })
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
/// * `telegram_tx` - Sender for parsed inbound telegrams → `pump_source`
/// * `command_rx` - Command receiver for outbound publishing
async fn connect_and_listen(
    gateway_ip: &str,
    gateway_port: u16,
    telegram_tx: &mpsc::Sender<(String, Payload)>,
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

                            // Forward to `pump_source` (which routes to producers).
                            // `try_send` so a slow/full sink never stalls the
                            // protocol task (ACKs, keepalive, outbound).
                            if telegram_tx
                                .try_send((resource_id, Payload::from(data.as_slice())))
                                .is_err()
                            {
                                #[cfg(feature = "tracing")]
                                tracing::warn!(
                                    "KNX inbound: dropping telegram for {} (channel full/closed)",
                                    group_addr
                                );
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

    #[tokio::test]
    async fn test_connector_creation() {
        let connector = KnxConnectorImpl::build_internal("knx://192.168.1.19:3671", 32).await;
        assert!(connector.is_ok());
    }

    #[tokio::test]
    async fn test_connector_with_port() {
        let connector = KnxConnectorImpl::build_internal("knx://gateway.local:3672", 32).await;
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
