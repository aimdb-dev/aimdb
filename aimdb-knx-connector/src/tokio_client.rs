//! Tokio transport shim for the KNX/IP connector
//!
//! This module contributes only socket glue: a UDP socket, the channels
//! between the pumps and the connection task, and a select loop driving the
//! shared sans-io [`TunnelEngine`](crate::tunnel::TunnelEngine). The entire
//! tunneling lifecycle (handshake, ACK bookkeeping, keepalive, reconnect
//! backoff) lives in [`crate::tunnel`].
//!
//! - Outbound rides core's `pump_sink`: [`KnxSink`] forwards each serialized
//!   record as a [`GroupWrite`] command to the connection task.
//! - Inbound rides core's `pump_source`: the connection task pushes parsed
//!   `(group-address, payload)` telegrams that [`KnxSource`] yields.

use crate::tunnel::{
    drain_actions, GroupWrite, LocalEndpoint, TunnelConfig, TunnelEngine, TunnelIo,
};
use crate::GroupAddress;
use aimdb_core::connector::ConnectorUrl;
use aimdb_core::transport::{Connector, ConnectorConfig, PublishError};
use aimdb_core::{pump_sink, pump_source, BoxFut, ConnectorBuilder, Payload, Source};
use std::future::Future;
use std::net::{IpAddr, SocketAddr};
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::UdpSocket;
use tokio::sync::mpsc;

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

impl ConnectorBuilder for KnxConnectorBuilder {
    fn build<'a>(
        &'a self,
        db: &'a aimdb_core::builder::AimDb,
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
/// connection-task future plus one publisher future per outbound route.
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
            mpsc::Sender<GroupWrite>,
            mpsc::Receiver<(String, Payload)>,
            BoxFuture,
        ),
        String,
    > {
        // Parse the gateway URL (bare `knx://host:port`, like the Embassy shim).
        let connector_url =
            ConnectorUrl::parse(gateway_url).map_err(|e| format!("Invalid KNX URL: {}", e))?;
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
        let (command_tx, command_rx) = mpsc::channel::<GroupWrite>(command_queue_size);
        let (telegram_tx, telegram_rx) = mpsc::channel::<(String, Payload)>(command_queue_size);

        let connection_future: BoxFuture = Box::pin(connection_task(
            gateway_ip,
            gateway_port,
            telegram_tx,
            command_rx,
        ));

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
    command_tx: mpsc::Sender<GroupWrite>,
}

impl Connector for KnxSink {
    fn publish(
        &self,
        destination: &str,
        _config: &ConnectorConfig,
        payload: &[u8],
    ) -> Pin<Box<dyn Future<Output = Result<(), PublishError>> + Send + '_>> {
        // Validation shared with the Embassy shim (same checks, same order).
        let command = GroupWrite::try_new(destination, payload);
        let command_tx = self.command_tx.clone();
        Box::pin(async move {
            command_tx
                .send(command?)
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

/// The connection task: socket I/O around the shared [`TunnelEngine`].
///
/// Binds a UDP socket (rebinding whenever the engine asks for a reset), then
/// loops: fire engine deadlines, apply the engine's actions, and select over
/// inbound datagrams, outbound commands, and the next engine deadline.
async fn connection_task(
    gateway_ip: String,
    gateway_port: u16,
    telegram_tx: mpsc::Sender<(String, Payload)>,
    mut command_rx: mpsc::Receiver<GroupWrite>,
) {
    #[cfg(feature = "tracing")]
    tracing::info!(
        "KNX connection task started for {}:{}",
        gateway_ip,
        gateway_port
    );

    let gateway_addr: SocketAddr = loop {
        match format!("{}:{}", gateway_ip, gateway_port).parse() {
            Ok(addr) => break addr,
            Err(_e) => {
                #[cfg(feature = "tracing")]
                tracing::error!(
                    "Invalid KNX gateway address {}:{}",
                    gateway_ip,
                    gateway_port
                );
                // The address never becomes valid; park instead of spinning.
                tokio::time::sleep(Duration::from_secs(3600)).await;
            }
        }
    };

    let epoch = tokio::time::Instant::now();
    let now_ms = || epoch.elapsed().as_millis() as u64;

    let mut engine = TunnelEngine::new(TunnelConfig::default(), now_ms());
    let mut socket: Option<UdpSocket> = None;
    let mut buf = [0u8; 1024];
    // Set to false once every `KnxSink` is gone. With no outbound routes that
    // happens right at build time (`pump_sink` drops the unused sink), so a
    // closed command channel only disables its select arm — inbound routing
    // keeps running.
    let mut commands_open = true;

    loop {
        // (Re)bind the socket if the engine reset it (or on first entry), and
        // advertise the real bound address in the next CONNECT_REQUEST.
        if socket.is_none() {
            match UdpSocket::bind("0.0.0.0:0").await {
                Ok(s) => {
                    if let Ok(local) = s.local_addr() {
                        if let IpAddr::V4(ip) = local.ip() {
                            engine.set_local_endpoint(LocalEndpoint::Explicit {
                                ip: ip.octets(),
                                port: local.port(),
                            });
                        }
                        #[cfg(feature = "tracing")]
                        tracing::debug!("KNX: Connecting from {} to {}", local, gateway_addr);
                    }
                    socket = Some(s);
                }
                Err(_e) => {
                    #[cfg(feature = "tracing")]
                    tracing::error!("Failed to bind UDP socket: {}, retrying in 5s", _e);
                    tokio::time::sleep(Duration::from_secs(5)).await;
                    continue;
                }
            }
        }

        engine.poll(now_ms());

        // `socket` is always `Some` here (bound above); apply the engine's
        // actions through the shared drain.
        let reset = match socket.as_ref() {
            Some(s) => {
                let mut io = TokioIo {
                    socket: s,
                    gateway: gateway_addr,
                    telegram_tx: &telegram_tx,
                };
                drain_actions(&mut engine, &mut io).await
            }
            None => false,
        };
        if reset {
            #[cfg(feature = "tracing")]
            tracing::error!("KNX connection lost, reconnecting after backoff...");
            // Rebind at the top of the loop.
            socket = None;
            continue;
        }

        let Some(s) = socket.as_ref() else {
            continue;
        };

        let sleep_ms = engine.next_deadline().saturating_sub(now_ms());

        tokio::select! {
            result = s.recv_from(&mut buf) => match result {
                Ok((len, _)) => {
                    #[cfg(feature = "tracing")]
                    tracing::trace!("Received {} bytes from gateway", len);
                    engine.handle_datagram(&buf[..len], now_ms());
                }
                Err(_e) => {
                    #[cfg(feature = "tracing")]
                    tracing::error!("Socket error: {}", _e);
                    engine.handle_socket_error(now_ms());
                }
            },
            // Only drained while connected: commands queue up in the channel
            // during a reconnect cycle and flush once the handshake completes
            // (same as the previous implementation, where the select loop only
            // ran while connected).
            cmd = command_rx.recv(), if commands_open && engine.is_connected() => match cmd {
                Some(cmd) => {
                    if !engine.handle_command(cmd, now_ms()) {
                        #[cfg(feature = "tracing")]
                        tracing::warn!("Not connected, dropping GroupWrite");
                    }
                }
                // All `KnxSink`s dropped — no outbound publisher remains.
                // Inbound monitoring still has to run, so only disable this
                // arm instead of exiting the connection task.
                None => commands_open = false,
            },
            // Wake for the next engine deadline; `poll` at the loop top fires it.
            _ = tokio::time::sleep(Duration::from_millis(sleep_ms)) => {}
        }
    }
}

/// Socket-side glue for [`drain_actions`]: frames ride the bound UDP socket,
/// parsed telegrams ride the mpsc channel into [`KnxSource`].
struct TokioIo<'a> {
    socket: &'a UdpSocket,
    gateway: SocketAddr,
    telegram_tx: &'a mpsc::Sender<(String, Payload)>,
}

impl TunnelIo for TokioIo<'_> {
    async fn send(&mut self, frame: &[u8]) {
        // Log-and-continue: a transient send error must not tear down the
        // tunnel; persistent socket death surfaces via the recv path.
        if let Err(_e) = self.socket.send_to(frame, self.gateway).await {
            #[cfg(feature = "tracing")]
            tracing::error!("KNX send failed: {}", _e);
        }
    }

    fn forward(&mut self, addr: GroupAddress, payload: Vec<u8>) {
        #[cfg(feature = "tracing")]
        tracing::debug!("KNX telegram: {} ({} bytes)", addr, payload.len());

        if self
            .telegram_tx
            .try_send((addr.to_string(), Payload::from(payload)))
            .is_err()
        {
            #[cfg(feature = "tracing")]
            tracing::warn!(
                "KNX inbound: dropping telegram for {} (channel full/closed)",
                addr
            );
        }
    }

    fn warn_ack_timeout(&mut self, _seq: u8) {
        #[cfg(feature = "tracing")]
        tracing::warn!("⚠️  ACK timeout for seq={}", _seq);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::time::timeout;

    const RECV_TIMEOUT: Duration = Duration::from_secs(5);

    fn service_type_of(frame: &[u8]) -> u16 {
        u16::from_be_bytes([frame[2], frame[3]])
    }

    /// CONNECT_RESPONSE: header + [channel_id, status, HPAI(8), CRD(4)].
    fn connect_response(channel_id: u8, status: u8) -> Vec<u8> {
        let mut frame = vec![0x06, 0x10, 0x02, 0x06, 0x00, 0x14];
        frame.extend_from_slice(&[channel_id, status]);
        frame.extend_from_slice(&[0x08, 0x01, 0, 0, 0, 0, 0, 0]); // HPAI 0.0.0.0:0
        frame.extend_from_slice(&[0x04, 0x04, 0x02, 0x00]); // CRD: tunnel
        frame
    }

    /// TUNNELING_REQUEST carrying a 6-bit GroupValueWrite to 1/0/7 (value 1).
    fn inbound_group_write(channel_id: u8, seq: u8) -> Vec<u8> {
        let cemi = [
            0x29, 0x00, 0xBC, 0xE0, // L_Data.ind, no add-info, ctrl1, ctrl2
            0x00, 0x00, 0x08, 0x07, // src 0.0.0, dest 1/0/7
            0x01, 0x00, 0x81, // NPDU len, TPCI, APCI | value 1
        ];
        let total = 6 + 4 + cemi.len() as u16;
        let mut frame = vec![0x06, 0x10, 0x04, 0x20];
        frame.extend_from_slice(&total.to_be_bytes());
        frame.extend_from_slice(&[0x04, channel_id, seq, 0x00]); // connection header
        frame.extend_from_slice(&cemi);
        frame
    }

    /// Full roundtrip against a scripted fake gateway on localhost UDP:
    /// handshake, inbound telegram → `KnxSource` channel, outbound command →
    /// TUNNELING_REQUEST on the wire (then ACKed).
    #[tokio::test]
    async fn tunnel_roundtrip_against_fake_gateway() {
        let gateway = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let gateway_port = gateway.local_addr().unwrap().port();

        let (command_tx, mut telegram_rx, connection_future) =
            KnxConnectorImpl::build_internal(&format!("knx://127.0.0.1:{}", gateway_port), 8)
                .await
                .unwrap();
        let task = tokio::spawn(connection_future);

        // Handshake: CONNECT_REQUEST in, CONNECT_RESPONSE out.
        let mut buf = [0u8; 1024];
        let (len, client_addr) = timeout(RECV_TIMEOUT, gateway.recv_from(&mut buf))
            .await
            .expect("no CONNECT_REQUEST")
            .unwrap();
        assert_eq!(service_type_of(&buf[..len]), 0x0205);
        gateway
            .send_to(&connect_response(7, 0), client_addr)
            .await
            .unwrap();

        // Inbound: gateway pushes a telegram; the client ACKs it and the
        // parsed payload reaches the telegram channel.
        gateway
            .send_to(&inbound_group_write(7, 42), client_addr)
            .await
            .unwrap();
        let (len, _) = timeout(RECV_TIMEOUT, gateway.recv_from(&mut buf))
            .await
            .expect("no TUNNELING_ACK")
            .unwrap();
        assert_eq!(service_type_of(&buf[..len]), 0x0421);
        assert_eq!(buf[8], 42); // sequence echoed
        let (topic, payload) = timeout(RECV_TIMEOUT, telegram_rx.recv())
            .await
            .expect("no telegram routed")
            .unwrap();
        assert_eq!(topic, "1/0/7");
        assert_eq!(&payload[..], &[0x01]);

        // Outbound: a GroupWrite command becomes a TUNNELING_REQUEST.
        let mut data = heapless::Vec::new();
        data.push(0x01).unwrap();
        command_tx
            .send(GroupWrite {
                group_addr: "1/0/8".parse().unwrap(),
                data,
            })
            .await
            .unwrap();
        let (len, _) = timeout(RECV_TIMEOUT, gateway.recv_from(&mut buf))
            .await
            .expect("no TUNNELING_REQUEST")
            .unwrap();
        assert_eq!(service_type_of(&buf[..len]), 0x0420);
        assert_eq!(buf[8], 0); // first outbound sequence
        assert_eq!(&buf[16..18], &[0x08, 0x08]); // cEMI destination = 1/0/8
        assert_eq!(buf[len - 1], 0x81); // APCI GroupValueWrite | 6-bit value 1

        task.abort();
    }

    /// Inbound-only regression: with no outbound routes, `pump_sink` drops the
    /// only `KnxSink` (and with it the sole command sender) at build time. The
    /// connection task must keep routing inbound telegrams — a closed command
    /// channel only disables that select arm.
    #[tokio::test]
    async fn inbound_routing_survives_dropped_command_sender() {
        let gateway = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let gateway_port = gateway.local_addr().unwrap().port();

        let (command_tx, mut telegram_rx, connection_future) =
            KnxConnectorImpl::build_internal(&format!("knx://127.0.0.1:{}", gateway_port), 8)
                .await
                .unwrap();
        drop(command_tx); // inbound-only configuration
        let task = tokio::spawn(connection_future);

        let mut buf = [0u8; 1024];
        let (len, client_addr) = timeout(RECV_TIMEOUT, gateway.recv_from(&mut buf))
            .await
            .expect("no CONNECT_REQUEST")
            .unwrap();
        assert_eq!(service_type_of(&buf[..len]), 0x0205);
        gateway
            .send_to(&connect_response(7, 0), client_addr)
            .await
            .unwrap();

        // The telegram arrives after the handshake completed — the moment the
        // old code observed the closed channel and exited.
        gateway
            .send_to(&inbound_group_write(7, 1), client_addr)
            .await
            .unwrap();
        let (topic, payload) = timeout(RECV_TIMEOUT, telegram_rx.recv())
            .await
            .expect("connection task died — no telegram routed")
            .unwrap();
        assert_eq!(topic, "1/0/7");
        assert_eq!(&payload[..], &[0x01]);

        task.abort();
    }

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
