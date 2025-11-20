//! Embassy runtime adapter for KNX/IP connector
//!
//! This module provides KNX/IP connectivity for Embassy-based embedded systems.
//!
//! # Architecture
//!
//! - Manual UDP socket management with `embassy-net`
//! - Manual connection state machine (CONNECT_REQUEST/RESPONSE, TUNNELING_ACK)
//! - Manual telegram parsing and routing
//! - Integration with AimDB's ConnectorBuilder pattern
//!
//! # Usage
//!
//! ```rust,ignore
//! use aimdb_knx_connector::KnxConnectorBuilder;
//! use aimdb_core::AimDbBuilder;
//!
//! // Configure database with KNX connector
//! let db = AimDbBuilder::new()
//!     .runtime(embassy_adapter)
//!     .with_connector(
//!         KnxConnectorBuilder::new("knx://192.168.1.19:3671")
//!     )
//!     .configure::<LightState>(|reg| {
//!         // Inbound: Monitor KNX bus for light state changes
//!         reg.link_from("knx://1/0/7")
//!            .with_deserializer(deserialize_light_state)
//!            .finish();
//!     })
//!     .build().await?;
//! ```

use crate::GroupAddress;
use aimdb_core::connector::ConnectorUrl;
use aimdb_core::router::{Router, RouterBuilder};
use aimdb_core::ConnectorBuilder;
use alloc::boxed::Box;
use alloc::string::{String, ToString};
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;
use core::future::Future;
use core::pin::Pin;
use core::str::FromStr;
use embassy_net::udp::{PacketMetadata, UdpSocket};
use embassy_net::{IpAddress, Ipv4Address, Stack};
use knx_pico::protocol::{
    CEMIFrame, ConnectRequest, ConnectResponse, ConnectionHeader, ConnectionStateRequest, Hpai,
    KnxnetIpFrame, ServiceType, TunnelingAck, TunnelingRequest,
};

/// Command sent to KNX connection task for outbound publishing
/// Max data length: 254 bytes (KNX/IP max APDU)
pub struct KnxCommand {
    pub kind: KnxCommandKind,
}

pub enum KnxCommandKind {
    /// Send GroupValueWrite telegram
    GroupWrite(Box<GroupWriteData>),
}

/// Data for GroupValueWrite command (boxed to reduce enum size)
pub struct GroupWriteData {
    pub group_addr: GroupAddress,
    pub data: heapless::Vec<u8, 254>,
}

/// Type alias for outbound route configuration
/// (resource_id, consumer, serializer, config_params)
type OutboundRoute = (
    String,
    Box<dyn aimdb_core::connector::ConsumerTrait>,
    aimdb_core::connector::SerializerFn,
    Vec<(String, String)>,
);

use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;
use static_cell::StaticCell;

/// Static channel for KNX commands (32 slots to match Tokio implementation)
static KNX_COMMAND_CHANNEL: StaticCell<Channel<CriticalSectionRawMutex, KnxCommand, 32>> =
    StaticCell::new();

/// Get or initialize the command channel
fn get_command_channel() -> &'static Channel<CriticalSectionRawMutex, KnxCommand, 32> {
    KNX_COMMAND_CHANNEL.init(Channel::new())
}

/// KNX connector builder for Embassy runtime
pub struct KnxConnectorBuilder {
    gateway_url: heapless::String<128>,
}

impl KnxConnectorBuilder {
    /// Create a new KNX connector builder with gateway URL
    ///
    /// # Arguments
    /// * `gateway_url` - KNX gateway URL (e.g., "knx://192.168.1.19:3671")
    pub fn new(gateway_url: &str) -> Self {
        Self {
            gateway_url: heapless::String::try_from(gateway_url)
                .unwrap_or_else(|_| heapless::String::new()),
        }
    }
}

/// Implement ConnectorBuilder trait for Embassy runtime with network stack access
impl<R> ConnectorBuilder<R> for KnxConnectorBuilder
where
    R: aimdb_executor::Spawn + aimdb_embassy_adapter::EmbassyNetwork + 'static,
{
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
        // Wrap in SendFutureWrapper since Embassy types aren't Send but we're single-threaded
        Box::pin(SendFutureWrapper(async move {
            // Collect inbound routes from database
            let routes = db.collect_inbound_routes("knx");

            #[cfg(feature = "defmt")]
            defmt::trace!(
                "Collected {} inbound routes for KNX connector",
                routes.len()
            );

            // Convert routes to Router
            let router = RouterBuilder::from_routes(routes).build();

            #[cfg(feature = "defmt")]
            defmt::trace!(
                "KNX router has {} unique group addresses",
                router.resource_ids().len()
            );

            // Build the actual connector
            let connector =
                KnxConnectorImpl::build_internal(self.gateway_url.as_str(), router, db.runtime())
                    .await
                    .map_err(|_e| {
                        #[cfg(feature = "defmt")]
                        defmt::error!("Failed to build KNX connector");

                        aimdb_core::DbError::RuntimeError { _message: () }
                    })?;

            // Collect and spawn outbound publishers
            let outbound_routes = db.collect_outbound_routes("knx");

            #[cfg(feature = "defmt")]
            defmt::trace!(
                "Collected {} outbound routes for KNX connector",
                outbound_routes.len()
            );

            connector.spawn_outbound_publishers(db, outbound_routes)?;

            Ok(Arc::new(connector) as Arc<dyn aimdb_core::transport::Connector>)
        }))
    }

    fn scheme(&self) -> &str {
        "knx"
    }
}

/// Pending ACK entry for outbound telegram (Embassy, no oneshot channels)
struct PendingAck {
    sent_at: embassy_time::Instant,
}

/// Connection state shared within the connection task
struct ChannelState {
    /// KNXnet/IP channel ID from CONNECT_RESPONSE
    channel_id: u8,
    /// Connection status
    connected: bool,
    /// Last received sequence counter (inbound telegrams)
    inbound_seq: u8,
    /// Next sequence counter to use for outbound telegrams
    outbound_seq: u8,
    /// Pending ACKs waiting for confirmation (seq -> PendingAck)
    pending_acks: heapless::FnvIndexMap<u8, PendingAck, 16>,
}

impl ChannelState {
    fn new() -> Self {
        Self {
            channel_id: 0,
            connected: false,
            inbound_seq: 0,
            outbound_seq: 0,
            pending_acks: heapless::FnvIndexMap::new(),
        }
    }

    fn set_channel_id(&mut self, channel_id: u8) {
        self.channel_id = channel_id;
        self.connected = true;
    }

    fn next_outbound_seq(&mut self) -> u8 {
        let seq = self.outbound_seq;
        self.outbound_seq = self.outbound_seq.wrapping_add(1);
        seq
    }

    /// Track a pending ACK for an outbound telegram
    fn add_pending_ack(&mut self, seq: u8) {
        let _ = self.pending_acks.insert(
            seq,
            PendingAck {
                sent_at: embassy_time::Instant::now(),
            },
        );
    }

    /// Complete a pending ACK (received confirmation)
    fn complete_ack(&mut self, seq: u8) -> bool {
        self.pending_acks.remove(&seq).is_some()
    }

    /// Check for timed-out ACKs (> 3 seconds) and return timed out sequences
    fn check_ack_timeouts(&mut self) -> heapless::Vec<u8, 16> {
        let now = embassy_time::Instant::now();
        let mut timed_out = heapless::Vec::new();

        // Collect sequences to remove
        let to_remove: heapless::Vec<u8, 16> = self
            .pending_acks
            .iter()
            .filter_map(|(&seq, pending)| {
                if now.duration_since(pending.sent_at) > embassy_time::Duration::from_secs(3) {
                    Some(seq)
                } else {
                    None
                }
            })
            .collect();

        // Remove timed-out entries
        for seq in &to_remove {
            self.pending_acks.remove(seq);
            let _ = timed_out.push(*seq);
        }

        timed_out
    }
}

/// Internal KNX connector implementation
pub struct KnxConnectorImpl {
    command_channel: &'static Channel<CriticalSectionRawMutex, KnxCommand, 32>,
}

impl KnxConnectorImpl {
    /// Create a new KNX connector with pre-configured router (internal)
    async fn build_internal<R>(
        gateway_url: &str,
        router: Router,
        runtime: &R,
    ) -> Result<Self, &'static str>
    where
        R: aimdb_executor::Spawn + aimdb_embassy_adapter::EmbassyNetwork + 'static,
    {
        // Parse the gateway URL
        let connector_url = ConnectorUrl::parse(gateway_url).map_err(|_| "Invalid KNX URL")?;

        let host = connector_url.host.clone();
        let port = connector_url.port.unwrap_or(3671); // KNX/IP default port

        #[cfg(feature = "defmt")]
        defmt::trace!("Creating KNX connector for {}:{}", host.as_str(), port);

        // Parse gateway IP address
        let gateway_ip = Ipv4Address::from_str(&host).map_err(|_| "Invalid gateway IP address")?;

        // Clone router for background task
        let router_arc = Arc::new(router);
        let router_for_task = router_arc.clone();

        // Get network stack for background task
        let network = runtime.network_stack();

        // Initialize command channel
        let command_channel = get_command_channel();

        // Spawn KNX connection background task
        let knx_task_future = SendFutureWrapper(async move {
            #[cfg(feature = "defmt")]
            defmt::trace!("KNX background task starting for {}:{}", gateway_ip, port);

            // Run the connection listener (this never returns under normal conditions)
            #[allow(unreachable_code)]
            {
                let _: () = Self::connection_task(
                    network,
                    gateway_ip,
                    port,
                    router_for_task,
                    command_channel,
                )
                .await;
            }
        });

        runtime
            .spawn(Box::pin(knx_task_future))
            .map_err(|_| "Failed to spawn KNX connection task")?;

        #[cfg(feature = "defmt")]
        defmt::trace!("KNX connector initialized");

        Ok(Self { command_channel })
    }

    /// Background task that maintains KNX connection and receives telegrams
    async fn connection_task(
        stack: &'static Stack<'static>,
        gateway_addr: Ipv4Address,
        gateway_port: u16,
        router: Arc<Router>,
        command_channel: &'static Channel<CriticalSectionRawMutex, KnxCommand, 32>,
    ) {
        loop {
            #[cfg(feature = "defmt")]
            defmt::info!(
                "🔌 Connecting to KNX gateway {}:{}",
                gateway_addr,
                gateway_port
            );

            match Self::connect_and_listen(
                stack,
                gateway_addr,
                gateway_port,
                &router,
                command_channel,
            )
            .await
            {
                Ok(()) => {
                    #[cfg(feature = "defmt")]
                    defmt::warn!("KNX connection ended normally (unexpected)");
                }
                Err(_e) => {
                    #[cfg(feature = "defmt")]
                    defmt::error!("❌ KNX connection error: {:?}", _e);
                }
            }

            // Wait before reconnecting
            #[cfg(feature = "defmt")]
            defmt::trace!("Reconnecting to KNX gateway in 5 seconds...");

            embassy_time::Timer::after(embassy_time::Duration::from_secs(5)).await;
        }
    }

    /// Connect to KNX gateway and listen for telegrams
    async fn connect_and_listen(
        stack: &'static Stack<'static>,
        gateway_addr: Ipv4Address,
        gateway_port: u16,
        router: &Router,
        command_channel: &'static Channel<CriticalSectionRawMutex, KnxCommand, 32>,
    ) -> Result<(), &'static str> {
        // Create UDP socket with static buffers
        let mut rx_meta = [PacketMetadata::EMPTY; 4];
        let mut rx_buffer = [0; 512];
        let mut tx_meta = [PacketMetadata::EMPTY; 4];
        let mut tx_buffer = [0; 512];

        let mut socket = UdpSocket::new(
            *stack,
            &mut rx_meta,
            &mut rx_buffer,
            &mut tx_meta,
            &mut tx_buffer,
        );

        // Bind to any local address
        socket.bind(0).map_err(|_| "Failed to bind socket")?;

        // Build CONNECT_REQUEST
        let connect_request = Self::build_connect_request();

        // Send CONNECT_REQUEST
        socket
            .send_to(
                &connect_request,
                (IpAddress::Ipv4(gateway_addr), gateway_port),
            )
            .await
            .map_err(|_| "Failed to send CONNECT_REQUEST")?;

        #[cfg(feature = "defmt")]
        defmt::debug!("Sent CONNECT_REQUEST");

        // Wait for CONNECT_RESPONSE
        let mut recv_buf = [0u8; 512];
        let (len, _peer) = socket
            .recv_from(&mut recv_buf)
            .await
            .map_err(|_| "Failed to receive CONNECT_RESPONSE")?;

        let channel_id = Self::parse_connect_response(&recv_buf[..len])?;

        #[cfg(feature = "defmt")]
        defmt::info!("✅ Connected to KNX gateway, channel_id: {}", channel_id);

        // Initialize connection state
        let mut state = ChannelState::new();
        state.set_channel_id(channel_id);

        // Create heartbeat ticker (every 55 seconds)
        let mut heartbeat_ticker =
            embassy_time::Ticker::every(embassy_time::Duration::from_secs(55));

        // ACK timeout checker (every 500ms)
        let mut ack_timeout_ticker =
            embassy_time::Ticker::every(embassy_time::Duration::from_millis(500));

        // Main event loop: inbound telegrams, outbound commands, heartbeat, and ACK timeouts
        loop {
            use embassy_futures::select::{select4, Either4};

            let mut recv_buf = [0u8; 512];

            // Set up four concurrent operations
            let recv_fut = socket.recv_from(&mut recv_buf);
            let cmd_fut = command_channel.receive();
            let heartbeat_fut = heartbeat_ticker.next();
            let ack_timeout_fut = ack_timeout_ticker.next();

            match select4(recv_fut, cmd_fut, heartbeat_fut, ack_timeout_fut).await {
                // Inbound: Process received telegram from KNX gateway
                Either4::First(result) => {
                    match result {
                        Ok((len, _peer)) => {
                            // Minimum KNX/IP header is 6 bytes
                            if len < 6 {
                                #[cfg(feature = "defmt")]
                                defmt::warn!("Received malformed packet (len={})", len);
                                continue;
                            }

                            // Check service type
                            let service_type = u16::from_be_bytes([recv_buf[2], recv_buf[3]]);

                            // Handle TUNNELING_ACK (0x0421) - acknowledgment for our outbound telegrams
                            if Self::is_tunneling_ack(&recv_buf[..len]) {
                                #[cfg(feature = "defmt")]
                                defmt::debug!(
                                    "Received TUNNELING_ACK: {=[u8]:02x}",
                                    &recv_buf[..len]
                                );

                                // Parse ACK - try knx-pico parser first, fallback to manual parsing
                                // Some gateways send non-standard ACK format (missing status byte)
                                let ack_seq = if let Ok(frame) =
                                    KnxnetIpFrame::parse(&recv_buf[..len])
                                {
                                    if let Ok(ack) = TunnelingAck::parse(frame.body()) {
                                        // Standard parsing succeeded
                                        ack.connection_header.sequence_counter
                                    } else if frame.body().len() >= 4 {
                                        // Fallback: manually extract sequence from ConnectionHeader
                                        // Body format: [struct_len, channel_id, seq, status]
                                        // Gateway may send 4 bytes instead of 5 (missing final status byte)
                                        let seq = frame.body()[2];

                                        #[cfg(feature = "defmt")]
                                        defmt::debug!("Using fallback ACK parsing (non-standard gateway format)");

                                        seq
                                    } else {
                                        #[cfg(feature = "defmt")]
                                        defmt::warn!(
                                            "Failed to parse TUNNELING_ACK body, raw: {=[u8]:02x}",
                                            &recv_buf[..len]
                                        );
                                        continue;
                                    }
                                } else {
                                    #[cfg(feature = "defmt")]
                                    defmt::warn!(
                                        "Failed to parse frame as TUNNELING_ACK, raw: {=[u8]:02x}",
                                        &recv_buf[..len]
                                    );
                                    continue;
                                };

                                if state.complete_ack(ack_seq) {
                                    #[cfg(feature = "defmt")]
                                    defmt::trace!("✅ Received TUNNELING_ACK for seq={}", ack_seq);
                                } else {
                                    #[cfg(feature = "defmt")]
                                    defmt::warn!(
                                        "⚠️  Unexpected TUNNELING_ACK for seq={}",
                                        ack_seq
                                    );
                                }
                                continue;
                            }

                            // Handle CONNECTIONSTATE_RESPONSE (0x0208) - 8 bytes
                            if service_type == 0x0208 {
                                #[cfg(feature = "defmt")]
                                defmt::trace!("Received CONNECTIONSTATE_RESPONSE");
                                continue;
                            }

                            // Handle DISCONNECT_RESPONSE (0x020A) - 8 bytes
                            if service_type == 0x020A {
                                #[cfg(feature = "defmt")]
                                defmt::warn!("Received DISCONNECT_RESPONSE from gateway");
                                continue;
                            }

                            // Check if this is a TUNNELING_REQUEST using knx-pico
                            if !Self::is_tunneling_request(&recv_buf[..len]) {
                                #[cfg(feature = "defmt")]
                                defmt::trace!("Ignoring non-TUNNELING_REQUEST frame");
                                continue;
                            }

                            // For TUNNELING_REQUEST we need at least 10 bytes
                            if len < 10 {
                                #[cfg(feature = "defmt")]
                                defmt::warn!("Received too short TUNNELING_REQUEST (len={})", len);
                                continue;
                            }

                            // Extract sequence counter from TUNNELING_REQUEST (byte 8)
                            let received_seq = if len > 8 { recv_buf[8] } else { 0 };
                            state.inbound_seq = received_seq;

                            // Send TUNNELING_ACK with the same sequence number
                            let ack = Self::build_tunneling_ack(state.channel_id, received_seq);
                            let _ = socket
                                .send_to(&ack, (IpAddress::Ipv4(gateway_addr), gateway_port))
                                .await;

                            #[cfg(feature = "defmt")]
                            defmt::trace!("Sent TUNNELING_ACK with seq={}", received_seq);

                            // Parse and route telegram
                            if let Some((addr, data)) = Self::parse_telegram(&recv_buf[..len]) {
                                let resource_id = addr.to_string();

                                #[cfg(feature = "defmt")]
                                defmt::trace!(
                                    "KNX telegram: {} (len={}) -> routing",
                                    resource_id.as_str(),
                                    data.len()
                                );

                                if let Err(_e) = router.route(&resource_id, &data).await {
                                    #[cfg(feature = "defmt")]
                                    defmt::warn!(
                                        "Failed to route telegram to {}",
                                        resource_id.as_str()
                                    );
                                }
                            } else {
                                #[cfg(feature = "defmt")]
                                defmt::trace!("❌ Failed to parse telegram (len={})", len);
                            }
                        }
                        Err(_) => {
                            return Err("Socket receive error");
                        }
                    }
                }

                // Outbound: Process command from publish() calls
                Either4::Second(cmd) => {
                    Self::handle_outbound_command(
                        cmd,
                        &mut state,
                        &socket,
                        gateway_addr,
                        gateway_port,
                    )
                    .await;
                }

                // Heartbeat: Send keepalive to gateway
                Either4::Third(_) => {
                    Self::send_heartbeat(&socket, gateway_addr, gateway_port, &state).await;
                }

                // ACK timeout checker: Check for expired ACKs
                Either4::Fourth(_) => {
                    let timed_out = state.check_ack_timeouts();
                    if !timed_out.is_empty() {
                        #[cfg(feature = "defmt")]
                        defmt::warn!("⚠️  ACK timeouts for sequences: {:?}", timed_out);
                    }
                }
            }
        }
    }

    /// Handle outbound command (send GroupValueWrite)
    async fn handle_outbound_command(
        cmd: KnxCommand,
        state: &mut ChannelState,
        socket: &UdpSocket<'_>,
        gateway_addr: Ipv4Address,
        gateway_port: u16,
    ) {
        let KnxCommandKind::GroupWrite(data_box) = cmd.kind;

        if !state.connected {
            #[cfg(feature = "defmt")]
            defmt::warn!("Not connected, dropping GroupWrite");
            return;
        }

        let seq = state.next_outbound_seq();

        // Build frames
        let cemi = Self::build_group_write_cemi(data_box.group_addr, &data_box.data);
        let request = Self::build_tunneling_request(state.channel_id, seq, &cemi);

        // Send to gateway
        if let Err(_e) = socket
            .send_to(&request, (IpAddress::Ipv4(gateway_addr), gateway_port))
            .await
        {
            #[cfg(feature = "defmt")]
            defmt::error!("Failed to send GroupWrite");
        } else {
            // Track pending ACK
            state.add_pending_ack(seq);

            #[cfg(feature = "defmt")]
            defmt::debug!(
                "Sent GroupWrite: {} seq={} ({} bytes)",
                data_box.group_addr, // GroupAddress implements Display
                seq,
                data_box.data.len()
            );
        }
    }

    /// Send heartbeat (CONNECTIONSTATE_REQUEST) to gateway
    async fn send_heartbeat(
        socket: &UdpSocket<'_>,
        gateway_addr: Ipv4Address,
        gateway_port: u16,
        state: &ChannelState,
    ) {
        if !state.connected {
            return;
        }

        let request = Self::build_connectionstate_request(state.channel_id);

        if let Err(_e) = socket
            .send_to(&request, (IpAddress::Ipv4(gateway_addr), gateway_port))
            .await
        {
            #[cfg(feature = "defmt")]
            defmt::error!("Heartbeat failed");
        } else {
            #[cfg(feature = "defmt")]
            defmt::trace!("Sent heartbeat");
        }
    }

    /// Build a CONNECT_REQUEST frame using knx-pico
    fn build_connect_request() -> heapless::Vec<u8, 32> {
        // Use 0.0.0.0:0 for "any" address
        let hpai = Hpai::new([0, 0, 0, 0], 0);
        let request = ConnectRequest::new(hpai, hpai);

        let mut buffer = [0u8; 32];
        let len = request
            .build(&mut buffer)
            .expect("Buffer too small for CONNECT_REQUEST");

        let mut frame = heapless::Vec::new();
        let _ = frame.extend_from_slice(&buffer[..len]);
        frame
    }

    /// Parse CONNECT_RESPONSE using knx-pico to extract channel ID
    fn parse_connect_response(data: &[u8]) -> Result<u8, &'static str> {
        let frame = KnxnetIpFrame::parse(data).map_err(|_| "Failed to parse frame")?;

        if frame.service_type() != ServiceType::ConnectResponse {
            return Err("Not a CONNECT_RESPONSE");
        }

        let response = ConnectResponse::parse(frame.body())
            .map_err(|_| "Failed to decode CONNECT_RESPONSE")?;

        if response.status != 0 {
            return Err("CONNECT_RESPONSE error status");
        }

        Ok(response.channel_id)
    }

    /// Build TUNNELING_ACK frame using knx-pico
    fn build_tunneling_ack(channel_id: u8, seq: u8) -> heapless::Vec<u8, 16> {
        let conn_header = ConnectionHeader::new(channel_id, seq);
        let ack = TunnelingAck::new(conn_header, 0); // status = 0 (OK)

        let mut buffer = [0u8; 16];
        let len = ack
            .build(&mut buffer)
            .expect("Buffer too small for TUNNELING_ACK");

        let mut frame = heapless::Vec::new();
        let _ = frame.extend_from_slice(&buffer[..len]);
        frame
    }

    /// Build GroupValueWrite cEMI frame (L_Data.req)
    ///
    /// Must match knx-pico's exact cEMI structure for proper parsing.
    /// Structure: [msg_code, add_info_len, ctrl1, ctrl2, src(2), dest(2), npdu_len, tpci, apci, data...]
    fn build_group_write_cemi(group_addr: GroupAddress, data: &[u8]) -> heapless::Vec<u8, 64> {
        let mut frame = heapless::Vec::new();

        // Message code: L_Data.req (0x11)
        let _ = frame.push(0x11);

        // Additional info length: 0
        let _ = frame.push(0x00);

        // Control field 1: 0xBC (Standard frame, no repeat, broadcast, priority low)
        // Use 0xBC instead of 0x94 - this is critical for gateway compatibility
        let _ = frame.push(0xBC);

        // Control field 2: 0xE0 (Group address, hop count 6)
        let _ = frame.push(0xE0);

        // Source address: 0.0.0 (2 bytes, big-endian)
        let _ = frame.extend_from_slice(&[0x00, 0x00]);

        // Destination address (group address) - convert to u16 big-endian
        let dest_raw: u16 = group_addr.into();
        let dest_bytes = dest_raw.to_be_bytes();
        let _ = frame.extend_from_slice(&dest_bytes);

        // Build NPDU: NPDU_length field + TPCI + APCI + data
        // CRITICAL: NPDU length encoding per KNX spec:
        // - For short telegram: field = 0x01 (special flag)
        // - For long telegram: field = actual_length - 1 (encoded as length-1)
        if data.len() == 1 && data[0] < 64 {
            // 6-bit encoding: value embedded in APCI byte
            // NPDU length = 0x01 (short telegram flag, NOT byte count)
            let _ = frame.push(0x01);

            // TPCI (UnnumberedData)
            let _ = frame.push(0x00);

            // APCI low byte: GroupValueWrite (0x80) + 6-bit value
            let _ = frame.push(0x80 | (data[0] & 0x3F));
        } else {
            // Long telegram: APCI + separate data bytes
            // NPDU length encoding: field = actual_length - 1
            let npdu_actual = 2 + data.len(); // TPCI + APCI + data
            let npdu_len_field = npdu_actual - 1; // Encode as length - 1
            let _ = frame.push(npdu_len_field as u8);

            // TPCI (UnnumberedData)
            let _ = frame.push(0x00);

            // APCI: GroupValueWrite
            let _ = frame.push(0x80);

            // Data bytes
            let _ = frame.extend_from_slice(data);
        }

        frame
    }

    /// Build TUNNELING_REQUEST containing cEMI frame using knx-pico
    fn build_tunneling_request(
        channel_id: u8,
        seq: u8,
        cemi_frame: &[u8],
    ) -> heapless::Vec<u8, 256> {
        let conn_header = ConnectionHeader::new(channel_id, seq);
        let request = TunnelingRequest::new(conn_header, cemi_frame);

        let mut buffer = [0u8; 256];
        let len = request
            .build(&mut buffer)
            .expect("Buffer too small for TUNNELING_REQUEST");

        let mut frame = heapless::Vec::new();
        let _ = frame.extend_from_slice(&buffer[..len]);
        frame
    }

    /// Build CONNECTIONSTATE_REQUEST for heartbeat using knx-pico
    fn build_connectionstate_request(channel_id: u8) -> heapless::Vec<u8, 32> {
        // Use 0.0.0.0:0 for "any" address
        let hpai = Hpai::new([0, 0, 0, 0], 0);
        let request = ConnectionStateRequest::new(channel_id, hpai);

        let mut buffer = [0u8; 32];
        let len = request
            .build(&mut buffer)
            .expect("Buffer too small for CONNECTIONSTATE_REQUEST");

        let mut frame = heapless::Vec::new();
        let _ = frame.extend_from_slice(&buffer[..len]);
        frame
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

    /// Parse a KNX telegram using knx-pico and extract group address and data
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
                #[cfg(feature = "defmt")]
                defmt::warn!("Failed to parse L_Data frame");
                return None;
            }
        };

        #[cfg(feature = "defmt")]
        {
            let dest_addr = ldata.destination_raw;
            let npdu_len = ldata.npdu_length;
            defmt::trace!(
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
        let payload = if ldata.data.is_empty() {
            // 6-bit encoding: extract value from APCI byte in raw cEMI data
            // cEMI structure: [msg_code, add_info_len, <add_info>, ctrl1, ctrl2, src(2), dest(2), npdu_len, tpci, apci, ...]
            // APCI byte position = 2 + add_info_len + 8
            let cemi_data = tunneling_req.cemi_data;
            let add_info_len = if cemi_data.len() > 1 { cemi_data[1] } else { 0 } as usize;
            let apci_pos = 2 + add_info_len + 8; // TPCI is at +7, APCI is at +8

            if cemi_data.len() > apci_pos {
                let apci_byte = cemi_data[apci_pos];
                let value = apci_byte & 0x3F; // Extract 6-bit value

                #[cfg(feature = "defmt")]
                defmt::debug!(
                    "6-bit decoding: apci_byte={:02X}, extracted_value={:02X}",
                    apci_byte,
                    value
                );

                vec![value]
            } else {
                vec![]
            }
        } else {
            // Standard encoding: multi-byte data (DPT5, DPT7, DPT9, etc.)
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

            #[cfg(feature = "defmt")]
            {
                let npdu_len_pos = ldata_offset + 6;
                let tpci_pos = ldata_offset + 7;
                let apci_pos = ldata_offset + 8;

                defmt::debug!(
                    "cEMI: len={}, add_info_len={}, NPDU_len@{}={:02X}, TPCI@{}={:02X}, APCI@{}={:02X}, Data@{}+={=[u8]:02x}",
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

            #[cfg(feature = "defmt")]
            defmt::debug!(
                "Extracted {} bytes: {=[u8]:02x}",
                extracted.len(),
                extracted
            );

            extracted
        };

        #[cfg(feature = "defmt")]
        defmt::trace!(
            "Parsed telegram for {}: {} payload bytes",
            dest,
            payload.len()
        );

        Some((dest, payload))
    }

    /// Spawn outbound publishers for records that link_to() KNX group addresses
    fn spawn_outbound_publishers<R>(
        &self,
        db: &aimdb_core::builder::AimDb<R>,
        outbound_routes: Vec<OutboundRoute>,
    ) -> aimdb_core::DbResult<()>
    where
        R: aimdb_executor::Spawn + 'static,
    {
        let runtime = db.runtime();

        for (group_addr_str, consumer, serializer, _config) in outbound_routes {
            let command_channel = self.command_channel;
            let group_addr_clone = group_addr_str.clone();

            runtime.spawn(Box::pin(SendFutureWrapper(async move {
                // Parse group address using knx-pico's type-safe parser
                let group_addr = match group_addr_clone.parse::<GroupAddress>() {
                    Ok(addr) => addr,
                    Err(_e) => {
                        #[cfg(feature = "defmt")]
                        defmt::error!(
                            "Invalid group address for outbound: '{}'",
                            group_addr_clone.as_str()
                        );
                        return;
                    }
                };

                // Subscribe to typed values (type-erased)
                let mut reader = match consumer.subscribe_any().await {
                    Ok(r) => r,
                    Err(_e) => {
                        #[cfg(feature = "defmt")]
                        defmt::error!(
                            "Failed to subscribe for outbound: '{}'",
                            group_addr_clone.as_str()
                        );
                        return;
                    }
                };

                #[cfg(feature = "defmt")]
                defmt::info!(
                    "KNX outbound publisher started for: {}",
                    group_addr_clone.as_str()
                );

                while let Ok(value_any) = reader.recv_any().await {
                    // Serialize the type-erased value
                    let bytes = match serializer(&*value_any) {
                        Ok(b) => b,
                        Err(_e) => {
                            #[cfg(feature = "defmt")]
                            defmt::error!(
                                "Failed to serialize for group address '{}'",
                                group_addr_clone.as_str()
                            );
                            continue;
                        }
                    };

                    // Convert to heapless::Vec
                    let mut vec_data = heapless::Vec::<u8, 254>::new();
                    if vec_data.extend_from_slice(&bytes).is_err() {
                        #[cfg(feature = "defmt")]
                        defmt::error!(
                            "Data too large for group address '{}'",
                            group_addr_clone.as_str()
                        );
                        continue;
                    }

                    // Send command to connection task
                    let cmd = KnxCommand {
                        kind: KnxCommandKind::GroupWrite(Box::new(GroupWriteData {
                            group_addr,
                            data: vec_data,
                        })),
                    };

                    command_channel.send(cmd).await;

                    #[cfg(feature = "defmt")]
                    defmt::debug!("Published to KNX: {}", group_addr_clone.as_str());
                }

                #[cfg(feature = "defmt")]
                defmt::info!(
                    "KNX outbound publisher stopped for: {}",
                    group_addr_clone.as_str()
                );
            })))?;
        }

        Ok(())
    }
}

// Implement the Connector trait
impl aimdb_core::transport::Connector for KnxConnectorImpl {
    fn publish(
        &self,
        resource_id: &str,
        _config: &aimdb_core::transport::ConnectorConfig,
        payload: &[u8],
    ) -> Pin<Box<dyn Future<Output = Result<(), aimdb_core::transport::PublishError>> + Send + '_>>
    {
        use aimdb_core::transport::PublishError;

        // Parse group address from resource_id (format: "1/0/7") using knx-pico's type-safe parser
        let group_addr = match resource_id.parse::<GroupAddress>() {
            Ok(addr) => addr,
            Err(_) => {
                return Box::pin(async move { Err(PublishError::InvalidDestination) });
            }
        };

        // Convert payload to heapless::Vec
        let mut vec_data = heapless::Vec::<u8, 254>::new();
        if vec_data.extend_from_slice(payload).is_err() {
            return Box::pin(async move { Err(PublishError::MessageTooLarge) });
        }

        let cmd = KnxCommand {
            kind: KnxCommandKind::GroupWrite(Box::new(GroupWriteData {
                group_addr,
                data: vec_data,
            })),
        };

        let command_channel = self.command_channel;

        Box::pin(async move {
            // Send command to background task via channel
            command_channel.send(cmd).await;

            Ok(())
        })
    }
}

// SAFETY: Embassy is single-threaded, so we can safely implement Send
// even though some Embassy types don't implement it. Embassy executors run
// cooperatively on a single core with no preemption or thread migration.
struct SendFutureWrapper<F>(F);

unsafe impl<F> Send for SendFutureWrapper<F> {}

impl<F: core::future::Future> core::future::Future for SendFutureWrapper<F> {
    type Output = F::Output;

    fn poll(
        self: core::pin::Pin<&mut Self>,
        cx: &mut core::task::Context<'_>,
    ) -> core::task::Poll<Self::Output> {
        // SAFETY: We're just forwarding the poll call
        unsafe { self.map_unchecked_mut(|s| &mut s.0).poll(cx) }
    }
}
