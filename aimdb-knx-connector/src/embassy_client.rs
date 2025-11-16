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
use alloc::vec::Vec;
use core::future::Future;
use core::pin::Pin;
use core::str::FromStr;
use embassy_net::udp::{PacketMetadata, UdpSocket};
use embassy_net::{IpAddress, Ipv4Address, Stack};

#[cfg(feature = "defmt")]
use defmt::{debug, error, info, trace, warn};

#[cfg(all(not(feature = "defmt"), feature = "tracing"))]
use tracing::{debug, error, info, trace, warn};

#[cfg(all(not(feature = "defmt"), not(feature = "tracing")))]
macro_rules! debug { ($($arg:tt)*) => { let _ = ($($arg)*,); }; }
#[cfg(all(not(feature = "defmt"), not(feature = "tracing")))]
macro_rules! info { ($($arg:tt)*) => { let _ = ($($arg)*,); }; }
#[cfg(all(not(feature = "defmt"), not(feature = "tracing")))]
macro_rules! warn { ($($arg:tt)*) => { let _ = ($($arg)*,); }; }
#[cfg(all(not(feature = "defmt"), not(feature = "tracing")))]
macro_rules! error { ($($arg:tt)*) => { let _ = ($($arg)*,); }; }
#[cfg(all(not(feature = "defmt"), not(feature = "tracing")))]
macro_rules! trace { ($($arg:tt)*) => { let _ = ($($arg)*,); }; }

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
            defmt::info!(
                "Collected {} inbound routes for KNX connector",
                routes.len()
            );

            // Convert routes to Router
            let router = RouterBuilder::from_routes(routes).build();

            #[cfg(feature = "defmt")]
            defmt::info!(
                "KNX router has {} group addresses",
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
            defmt::info!(
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

/// Internal KNX connector implementation
#[allow(dead_code)]
pub struct KnxConnectorImpl {
    gateway_ip: Ipv4Address,
    gateway_port: u16,
    router: Arc<Router>,
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
        defmt::info!("Creating KNX connector for {}:{}", host, port);

        // Parse gateway IP address
        let gateway_ip = Ipv4Address::from_str(&host).map_err(|_| "Invalid gateway IP address")?;

        // Clone router for background task
        let router_arc = Arc::new(router);
        let router_for_task = router_arc.clone();

        // Get network stack for background task
        let network = runtime.network_stack();

        // Spawn KNX connection background task
        let knx_task_future = SendFutureWrapper(async move {
            #[cfg(feature = "defmt")]
            defmt::info!("KNX background task starting");

            // Run the connection listener (this never returns under normal conditions)
            #[allow(unreachable_code)]
            {
                let _: () = Self::connection_task(
                    network, // Pass the network stack reference directly
                    gateway_ip,
                    port,
                    router_for_task,
                )
                .await;
            }
        });

        runtime
            .spawn(Box::pin(knx_task_future))
            .map_err(|_| "Failed to spawn KNX connection task")?;

        #[cfg(feature = "defmt")]
        defmt::info!("KNX connector initialized");

        Ok(Self {
            gateway_ip,
            gateway_port: port,
            router: router_arc,
        })
    }

    /// Background task that maintains KNX connection and receives telegrams
    async fn connection_task(
        stack: &'static Stack<'static>,
        gateway_addr: Ipv4Address,
        gateway_port: u16,
        router: Arc<Router>,
    ) {
        loop {
            #[cfg(feature = "defmt")]
            defmt::info!(
                "Connecting to KNX gateway {}:{}",
                gateway_addr,
                gateway_port
            );

            match Self::connect_and_listen(&stack, gateway_addr, gateway_port, &router).await {
                Ok(()) => {
                    #[cfg(feature = "defmt")]
                    defmt::warn!("KNX connection ended normally (unexpected)");
                }
                Err(_e) => {
                    #[cfg(feature = "defmt")]
                    defmt::error!("KNX connection error: {:?}", _e);
                }
            }

            // Wait before reconnecting
            #[cfg(feature = "defmt")]
            defmt::info!("Reconnecting to KNX gateway in 5 seconds...");

            embassy_time::Timer::after(embassy_time::Duration::from_secs(5)).await;
        }
    }

    /// Connect to KNX gateway and listen for telegrams
    async fn connect_and_listen(
        stack: &'static Stack<'static>,
        gateway_addr: Ipv4Address,
        gateway_port: u16,
        router: &Router,
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
        defmt::info!("Connected to KNX gateway, channel_id: {}", channel_id);

        let mut sequence_counter = 0u8;

        // Listen for incoming telegrams
        loop {
            let mut recv_buf = [0u8; 512];
            let (len, _peer) = socket
                .recv_from(&mut recv_buf)
                .await
                .map_err(|_| "Receive failed")?;

            if len < 10 {
                #[cfg(feature = "defmt")]
                defmt::warn!("Received too short packet ({})", len);
                continue;
            }

            // Check if this is a TUNNELING_REQUEST (0x0420)
            let service_type = u16::from_be_bytes([recv_buf[2], recv_buf[3]]);
            if service_type != 0x0420 {
                #[cfg(feature = "defmt")]
                defmt::trace!(
                    "Ignoring non-TUNNELING_REQUEST service type: 0x{:04x}",
                    service_type
                );
                continue;
            }

            // Send TUNNELING_ACK
            let ack = Self::build_tunneling_ack(channel_id, sequence_counter);
            let _ = socket
                .send_to(&ack, (IpAddress::Ipv4(gateway_addr), gateway_port))
                .await;

            sequence_counter = sequence_counter.wrapping_add(1);

            // Parse telegram
            if let Some((addr, data)) = Self::parse_telegram(&recv_buf[..len]) {
                #[cfg(feature = "defmt")]
                defmt::debug!(
                    "Received telegram for {}/{}/{}: {:?}",
                    addr.main(),
                    addr.middle(),
                    addr.sub(),
                    data
                );

                // Route to record producers
                let url = format!("knx://{}/{}/{}", addr.main(), addr.middle(), addr.sub());
                if let Err(_e) = router.route(&url, &data).await {
                    #[cfg(feature = "defmt")]
                    defmt::warn!("Failed to route telegram to {}: {:?}", url, _e);
                }
            }
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

        // Use GroupAddress::from() instead of from_raw()
        let addr = GroupAddress::from(dest_addr);

        // NPDU length (includes TPCI/APCI + data)
        let npdu_len = data[ctrl1_offset + 6] as usize;
        if data.len() < ctrl1_offset + 7 + npdu_len {
            return None;
        }

        // Extract APCI and data
        let tpci_apci_offset = ctrl1_offset + 7;
        let apci_data = &data[tpci_apci_offset..tpci_apci_offset + npdu_len];

        if apci_data.is_empty() {
            return None;
        }

        // Convert to Vec for routing
        let payload = apci_data.to_vec();

        Some((addr, payload))
    }

    /// Spawn outbound publishers for records that link_to() KNX group addresses
    fn spawn_outbound_publishers<R>(
        &self,
        _db: &aimdb_core::builder::AimDb<R>,
        _outbound_routes: Vec<(
            String,
            Box<dyn aimdb_core::connector::ConsumerTrait>,
            aimdb_core::connector::SerializerFn,
            Vec<(String, String)>,
        )>,
    ) -> aimdb_core::DbResult<()>
    where
        R: aimdb_executor::Spawn + 'static,
    {
        // TODO: Implement outbound publishers similar to MQTT
        // For now, just log that we collected them
        #[cfg(feature = "defmt")]
        defmt::info!("Outbound publishers not yet implemented for Embassy KNX");

        Ok(())
    }
}

// Implement the Connector trait
impl aimdb_core::transport::Connector for KnxConnectorImpl {
    fn publish(
        &self,
        _resource_id: &str,
        _config: &aimdb_core::transport::ConnectorConfig,
        _payload: &[u8],
    ) -> Pin<Box<dyn Future<Output = Result<(), aimdb_core::transport::PublishError>> + Send + '_>>
    {
        Box::pin(async move {
            // TODO: Implement KNX telegram publishing
            #[cfg(feature = "defmt")]
            defmt::warn!("KNX publish not yet implemented");

            Err(aimdb_core::transport::PublishError::ConnectionFailed)
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
