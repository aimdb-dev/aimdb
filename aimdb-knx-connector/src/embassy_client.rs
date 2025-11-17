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
use alloc::format;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;
use core::future::Future;
use core::pin::Pin;
use core::str::FromStr;
use embassy_net::udp::{PacketMetadata, UdpSocket};
use embassy_net::{IpAddress, Ipv4Address, Stack};

/// Command sent to KNX connection task for outbound publishing
/// Max data length: 254 bytes (KNX/IP max APDU)
pub struct KnxCommand {
    pub kind: KnxCommandKind,
}

pub enum KnxCommandKind {
    /// Send GroupValueWrite telegram
    GroupWrite(Box<GroupWriteData>),
    /// Graceful shutdown signal
    #[allow(dead_code)]
    Shutdown,
}

/// Data for GroupValueWrite command (boxed to reduce enum size)
pub struct GroupWriteData {
    pub group_addr: u16,
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

#[cfg(all(not(feature = "defmt"), feature = "tracing"))]
use tracing::{debug, error, info, trace, warn};

#[cfg(all(not(feature = "defmt"), not(feature = "tracing")))]
#[allow(unused_macros)]
macro_rules! debug { ($($arg:tt)*) => { let _ = ($($arg)*,); }; }
#[cfg(all(not(feature = "defmt"), not(feature = "tracing")))]
#[allow(unused_macros)]
macro_rules! info { ($($arg:tt)*) => { let _ = ($($arg)*,); }; }
#[cfg(all(not(feature = "defmt"), not(feature = "tracing")))]
#[allow(unused_macros)]
macro_rules! warn { ($($arg:tt)*) => { let _ = ($($arg)*,); }; }
#[cfg(all(not(feature = "defmt"), not(feature = "tracing")))]
#[allow(unused_macros)]
macro_rules! error { ($($arg:tt)*) => { let _ = ($($arg)*,); }; }
#[cfg(all(not(feature = "defmt"), not(feature = "tracing")))]
#[allow(unused_macros)]
macro_rules! trace { ($($arg:tt)*) => { let _ = ($($arg)*,); }; }

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
}

impl ChannelState {
    fn new() -> Self {
        Self {
            channel_id: 0,
            connected: false,
            inbound_seq: 0,
            outbound_seq: 0,
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
}

/// Internal KNX connector implementation
#[allow(dead_code)]
pub struct KnxConnectorImpl {
    gateway_ip: Ipv4Address,
    gateway_port: u16,
    router: Arc<Router>,
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

        Ok(Self {
            gateway_ip,
            gateway_port: port,
            router: router_arc,
            command_channel,
        })
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

        // Main event loop: inbound telegrams, outbound commands, and heartbeat
        loop {
            use embassy_futures::select::{select3, Either3};

            let mut recv_buf = [0u8; 512];

            // Set up three concurrent operations
            let recv_fut = socket.recv_from(&mut recv_buf);
            let cmd_fut = command_channel.receive();
            let heartbeat_fut = heartbeat_ticker.next();

            match select3(recv_fut, cmd_fut, heartbeat_fut).await {
                // Inbound: Process received telegram from KNX gateway
                Either3::First(result) => {
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

                            // For TUNNELING_REQUEST we need at least 10 bytes
                            if service_type == 0x0420 && len < 10 {
                                #[cfg(feature = "defmt")]
                                defmt::warn!("Received too short TUNNELING_REQUEST (len={})", len);
                                continue;
                            }

                            // Check if this is a TUNNELING_REQUEST (0x0420)
                            if service_type != 0x0420 {
                                #[cfg(feature = "defmt")]
                                defmt::trace!("Ignoring service type: 0x{:04x}", service_type);
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
                                let resource_id =
                                    format!("{}/{}/{}", addr.main(), addr.middle(), addr.sub());

                                #[cfg(feature = "defmt")]
                                defmt::trace!(
                                    "KNX telegram: {}/{}/{} (len={}) -> routing",
                                    addr.main(),
                                    addr.middle(),
                                    addr.sub(),
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
                Either3::Second(cmd) => {
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
                Either3::Third(_) => {
                    Self::send_heartbeat(&socket, gateway_addr, gateway_port, &state).await;
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
        match cmd.kind {
            KnxCommandKind::GroupWrite(data_box) => {
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
                    #[cfg(feature = "defmt")]
                    defmt::debug!(
                        "Sent GroupWrite: {}/{}/{} seq={} ({} bytes)",
                        (data_box.group_addr >> 11) & 0x1F,
                        (data_box.group_addr >> 8) & 0x07,
                        data_box.group_addr & 0xFF,
                        seq,
                        data_box.data.len()
                    );
                }
            }

            KnxCommandKind::Shutdown => {
                state.connected = false;
            }
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

    /// Build a CONNECT_REQUEST frame
    fn build_connect_request() -> heapless::Vec<u8, 26> {
        let mut frame = heapless::Vec::new();

        // Header
        let _ = frame.push(0x06); // Header length
        let _ = frame.push(0x10); // Protocol version 1.0
        let _ = frame.extend_from_slice(&[0x02, 0x05]); // CONNECT_REQUEST
        let _ = frame.extend_from_slice(&[0x00, 0x1A]); // Total length: 26 bytes

        // Control endpoint (HPAI)
        let _ = frame.push(0x08); // Structure length
        let _ = frame.push(0x01); // UDP
        let _ = frame.extend_from_slice(&[0, 0, 0, 0]); // IP: 0.0.0.0 (any)
        let _ = frame.extend_from_slice(&[0x00, 0x00]); // Port: 0 (any)

        // Data endpoint (HPAI)
        let _ = frame.push(0x08); // Structure length
        let _ = frame.push(0x01); // UDP
        let _ = frame.extend_from_slice(&[0, 0, 0, 0]); // IP: 0.0.0.0
        let _ = frame.extend_from_slice(&[0x00, 0x00]); // Port: 0

        // CRI (Connection Request Information)
        let _ = frame.push(0x04); // Structure length
        let _ = frame.push(0x04); // TUNNEL_CONNECTION
        let _ = frame.push(0x02); // KNX Layer (Data Link Layer)
        let _ = frame.push(0x00); // Reserved

        frame
    }

    /// Parse CONNECT_RESPONSE to extract channel ID
    fn parse_connect_response(data: &[u8]) -> Result<u8, &'static str> {
        if data.len() < 8 {
            return Err("CONNECT_RESPONSE too short");
        }

        let service_type = u16::from_be_bytes([data[2], data[3]]);
        if service_type != 0x0206 {
            return Err("Not a CONNECT_RESPONSE");
        }

        let channel_id = data[6];
        let status = data[7];

        if status != 0 {
            return Err("CONNECT_RESPONSE error status");
        }

        Ok(channel_id)
    }

    /// Build TUNNELING_ACK frame
    fn build_tunneling_ack(channel_id: u8, seq: u8) -> heapless::Vec<u8, 10> {
        let mut frame = heapless::Vec::new();

        let _ = frame.push(0x06); // Header length
        let _ = frame.push(0x10); // Protocol version
        let _ = frame.extend_from_slice(&[0x04, 0x21]); // TUNNELING_ACK
        let _ = frame.extend_from_slice(&[0x00, 0x0A]); // Total length: 10 bytes
        let _ = frame.push(0x04); // Structure length
        let _ = frame.push(channel_id);
        let _ = frame.push(seq);
        let _ = frame.push(0x00); // Status: OK

        frame
    }

    /// Build GroupValueWrite cEMI frame (L_Data.req)
    fn build_group_write_cemi(group_addr: u16, data: &[u8]) -> heapless::Vec<u8, 64> {
        let mut frame = heapless::Vec::new();

        // cEMI message code: L_Data.req (0x11)
        let _ = frame.push(0x11);

        // Additional info length: 0
        let _ = frame.push(0x00);

        // Control field 1: Standard frame, no repeat, broadcast, priority low
        let _ = frame.push(0xBC);

        // Control field 2: Group address, hop count 6
        let _ = frame.push(0xE0);

        // Source address: 0.0.0 (placeholder)
        let _ = frame.extend_from_slice(&[0x00, 0x00]);

        // Destination address (group)
        let _ = frame.extend_from_slice(&group_addr.to_be_bytes());

        // Check if this is a short telegram (1 byte, value < 64)
        if data.len() == 1 && data[0] < 64 {
            // Short telegram: encode data in APCI lower 6 bits
            let _ = frame.push(0x01); // NPDU length = 1 (TPCI/APCI only)
            let _ = frame.push(0x00); // TPCI
            let _ = frame.push(0x80 | (data[0] & 0x3F)); // APCI: GroupValueWrite + 6-bit data
        } else {
            // Long telegram: APCI + separate data bytes
            let npdu_len = 2 + data.len(); // TPCI + APCI + data
            let _ = frame.push(npdu_len as u8);
            let _ = frame.push(0x00); // TPCI
            let _ = frame.push(0x80); // APCI: GroupValueWrite
            let _ = frame.extend_from_slice(data); // Payload data
        }

        frame
    }

    /// Build TUNNELING_REQUEST containing cEMI frame
    fn build_tunneling_request(
        channel_id: u8,
        seq: u8,
        cemi_frame: &[u8],
    ) -> heapless::Vec<u8, 256> {
        let mut frame = heapless::Vec::new();
        let total_len = 10 + cemi_frame.len();

        // Header length
        let _ = frame.push(0x06);

        // Protocol version
        let _ = frame.push(0x10);

        // Service type: TUNNELING_REQUEST (0x0420)
        let _ = frame.extend_from_slice(&[0x04, 0x20]);

        // Total length
        let _ = frame.extend_from_slice(&(total_len as u16).to_be_bytes());

        // Structure length
        let _ = frame.push(0x04);

        // Channel ID
        let _ = frame.push(channel_id);

        // Sequence counter
        let _ = frame.push(seq);

        // Reserved
        let _ = frame.push(0x00);

        // cEMI frame
        let _ = frame.extend_from_slice(cemi_frame);

        frame
    }

    /// Build CONNECTIONSTATE_REQUEST for heartbeat
    fn build_connectionstate_request(channel_id: u8) -> heapless::Vec<u8, 16> {
        let mut frame = heapless::Vec::new();

        // Header
        let _ = frame.push(0x06); // Header length
        let _ = frame.push(0x10); // Protocol version
        let _ = frame.extend_from_slice(&[0x02, 0x07]); // CONNECTIONSTATE_REQUEST
        let _ = frame.extend_from_slice(&[0x00, 0x10]); // Total length: 16

        // Channel ID
        let _ = frame.push(channel_id);
        let _ = frame.push(0x00); // Reserved

        // Control endpoint HPAI (0.0.0.0:0 for "any")
        let _ = frame.push(0x08); // Structure length
        let _ = frame.push(0x01); // Host protocol: UDP
        let _ = frame.extend_from_slice(&[0, 0, 0, 0]); // IP: 0.0.0.0
        let _ = frame.extend_from_slice(&[0, 0]); // Port: 0

        frame
    }

    /// Parse a KNX telegram from TUNNELING_REQUEST
    fn parse_telegram(data: &[u8]) -> Option<(GroupAddress, Vec<u8>)> {
        if data.len() < 20 {
            return None;
        }

        // cEMI frame starts at offset 10
        let cemi_offset = 10;
        let message_code = data[cemi_offset];

        // L_Data.ind (0x29) or L_Data.req (0x11)
        if message_code != 0x29 && message_code != 0x11 {
            return None;
        }

        // Skip Add.Info length
        let add_info_len = data[cemi_offset + 1] as usize;
        let ctrl1_offset = cemi_offset + 2 + add_info_len;

        if data.len() < ctrl1_offset + 7 {
            return None;
        }

        // Extract destination address (group address)
        let dest_addr = u16::from_be_bytes([data[ctrl1_offset + 4], data[ctrl1_offset + 5]]);
        let addr = GroupAddress::from(dest_addr);

        // NPDU length field + 1 = actual byte count (KNX specification)
        let npdu_len_field = data[ctrl1_offset + 6] as usize;
        let npdu_len = npdu_len_field + 1; // Actual byte count

        if data.len() < ctrl1_offset + 7 + npdu_len {
            return None;
        }

        // Extract APCI and data
        let tpci_apci_offset = ctrl1_offset + 7;

        // Handle short telegrams (6-bit data) vs multi-byte data
        let payload = if npdu_len > 1 {
            // Multi-byte data: return full NPDU (TPCI + APCI + data) just like Tokio
            if data.len() < tpci_apci_offset + npdu_len {
                return None;
            }
            data[tpci_apci_offset..tpci_apci_offset + npdu_len].to_vec()
        } else {
            // Short telegram: extract 6-bit data from APCI byte
            if data.len() < tpci_apci_offset + 2 {
                return None;
            }
            let short_value = data[tpci_apci_offset + 1] & 0x3F;
            vec![short_value]
        };

        Some((addr, payload))
    }

    /// Parse group address string "main/middle/sub" to raw u16
    fn parse_group_address(addr_str: &str) -> Result<u16, &'static str> {
        let mut parts = addr_str.split('/');

        let main: u8 = parts
            .next()
            .and_then(|s| s.parse().ok())
            .ok_or("Invalid main group")?;
        let middle: u8 = parts
            .next()
            .and_then(|s| s.parse().ok())
            .ok_or("Invalid middle group")?;
        let sub: u8 = parts
            .next()
            .and_then(|s| s.parse().ok())
            .ok_or("Invalid sub group")?;

        if main > 31 {
            return Err("Main group must be 0-31");
        }
        if middle > 7 {
            return Err("Middle group must be 0-7");
        }

        // Encode: 5 bits main | 3 bits middle | 8 bits sub
        let raw = ((main as u16) << 11) | ((middle as u16) << 8) | (sub as u16);

        Ok(raw)
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
                // Parse group address
                let group_addr = match Self::parse_group_address(&group_addr_clone) {
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

        // Parse group address from resource_id (format: "1/0/7")
        let group_addr = match Self::parse_group_address(resource_id) {
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
