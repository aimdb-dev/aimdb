//! Embassy transport shim for the KNX/IP connector
//!
//! This module contributes only socket glue for embedded systems: an
//! `embassy-net` UDP socket, the static channels between the pumps and the
//! connection task, and a select loop driving the shared sans-io
//! [`TunnelEngine`](crate::tunnel::TunnelEngine). The entire tunneling
//! lifecycle (handshake, ACK bookkeeping, keepalive, reconnect backoff) lives
//! in [`crate::tunnel`].
//!
//! # Architecture
//!
//! - **Outbound** (records → telegrams) rides core's `pump_sink` via the
//!   [`Connector`](aimdb_core::transport::Connector) impl (commands go onto a
//!   `CriticalSectionRawMutex` channel the connection task drains).
//! - **Inbound** (telegrams → records) rides core's `pump_source`: the
//!   connection task pushes `(group-address, payload)` onto an inbound channel
//!   that [`KnxSource`] drains.
//! - The connection task is force-`Send`ed once via
//!   [`into_box_future`](aimdb_embassy_adapter::connectors::into_box_future); the
//!   only `unsafe` in this crate is the audited
//!   [`NetStack::new`](aimdb_embassy_adapter::connectors::NetStack) call in the
//!   builder (single-core cooperative executor invariant).
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
//!         KnxConnectorBuilder::new("knx://192.168.1.19:3671", stack)
//!     )
//!     .configure::<LightState>(|reg| {
//!         // Inbound: Monitor KNX bus for light state changes
//!         reg.link_from("knx://1/0/7")
//!            .with_deserializer(deserialize_light_state)
//!            .finish();
//!     })
//!     .build().await?;
//! ```

use crate::tunnel::{drain_actions, GroupWrite, TunnelConfig, TunnelEngine, TunnelIo};
use crate::GroupAddress;
use aimdb_core::connector::ConnectorUrl;
use aimdb_core::session::{pump_sink, pump_source, Payload};
use aimdb_core::ConnectorBuilder;
use aimdb_embassy_adapter::connectors::into_box_future;
use alloc::boxed::Box;
use alloc::string::{String, ToString};
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::future::Future;
use core::pin::Pin;
use core::str::FromStr;
use embassy_net::udp::{PacketMetadata, UdpSocket};
use embassy_net::{IpAddress, Ipv4Address, Stack};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::{Channel, Receiver, Sender};
use static_cell::StaticCell;

/// Inbound telegram item: `(group-address string, payload)` — pushed by the
/// connection task, drained by [`KnxSource`] into core's `pump_source`.
type InboundItem = (String, Payload);

/// Outbound command item — boxed so the static channel stores pointers
/// instead of full `MAX_APDU`-sized payloads.
type CommandItem = Box<GroupWrite>;

/// `'static` reference to the outbound command channel.
type CommandChannelRef =
    &'static Channel<CriticalSectionRawMutex, CommandItem, KNX_COMMAND_QUEUE_SIZE>;
/// Sender / receiver halves of the inbound telegram channel.
type InboundSender = Sender<'static, CriticalSectionRawMutex, InboundItem, KNX_INBOUND_QUEUE_SIZE>;
type InboundReceiver =
    Receiver<'static, CriticalSectionRawMutex, InboundItem, KNX_INBOUND_QUEUE_SIZE>;

/// Capacity of the static KNX command channel.
///
/// Embassy requires a compile-time const generic — runtime configurability is
/// not possible with `StaticCell<Channel<..., N>>`. Adjust this constant and
/// recompile if your installation needs a larger buffer.
const KNX_COMMAND_QUEUE_SIZE: usize = 32;

/// Static channel for KNX commands (capacity: [`KNX_COMMAND_QUEUE_SIZE`])
static KNX_COMMAND_CHANNEL: StaticCell<
    Channel<CriticalSectionRawMutex, CommandItem, KNX_COMMAND_QUEUE_SIZE>,
> = StaticCell::new();

/// Get or initialize the command channel
fn get_command_channel(
) -> &'static Channel<CriticalSectionRawMutex, CommandItem, KNX_COMMAND_QUEUE_SIZE> {
    KNX_COMMAND_CHANNEL.init(Channel::new())
}

/// Capacity of the static inbound telegram channel.
const KNX_INBOUND_QUEUE_SIZE: usize = 32;

/// Static channel for inbound telegrams (capacity: [`KNX_INBOUND_QUEUE_SIZE`]).
static KNX_INBOUND_CHANNEL: StaticCell<
    Channel<CriticalSectionRawMutex, InboundItem, KNX_INBOUND_QUEUE_SIZE>,
> = StaticCell::new();

/// Get or initialize the inbound telegram channel.
fn get_inbound_channel(
) -> &'static Channel<CriticalSectionRawMutex, InboundItem, KNX_INBOUND_QUEUE_SIZE> {
    KNX_INBOUND_CHANNEL.init(Channel::new())
}

/// Inbound [`Source`](aimdb_core::session::Source): drains the connection task's
/// telegram channel, yielding each `(group-address, payload)` for core's
/// `pump_source` to fan out to the matching record producers. The KNX command/
/// inbound channels use `CriticalSectionRawMutex` (`Send`), so this is a plain
/// `Source` — no force-`Send` wrapper needed.
struct KnxSource {
    receiver: InboundReceiver,
}

impl aimdb_core::session::Source for KnxSource {
    fn next(&mut self) -> aimdb_core::session::BoxFut<'_, Option<(String, Payload)>> {
        Box::pin(async move { Some(self.receiver.receive().await) })
    }
}

/// KNX connector builder for Embassy runtime
pub struct KnxConnectorBuilder {
    gateway_url: heapless::String<128>,
    stack: aimdb_embassy_adapter::connectors::NetStack,
}

impl KnxConnectorBuilder {
    /// Create a new KNX connector builder with gateway URL
    ///
    /// # Arguments
    /// * `gateway_url` - KNX gateway URL (e.g., "knx://192.168.1.19:3671")
    /// * `stack` - The device's network stack (the runtime travels as
    ///   `Arc<dyn RuntimeOps>` since issue #131 and cannot surface it)
    pub fn new(gateway_url: &str, stack: &'static Stack<'static>) -> Self {
        Self {
            gateway_url: heapless::String::try_from(gateway_url)
                .unwrap_or_else(|_| heapless::String::new()),
            // SAFETY: AimDB's Embassy integration requires a single-core
            // cooperative executor (the adapter's module-level invariant);
            // every future touching this stack — including the connection
            // task built from this builder — is polled on that executor.
            stack: unsafe { aimdb_embassy_adapter::connectors::NetStack::new(stack) },
        }
    }
}

type BoxFuture = Pin<Box<dyn Future<Output = ()> + Send + 'static>>;

/// Implement ConnectorBuilder trait for Embassy
impl ConnectorBuilder for KnxConnectorBuilder {
    fn build<'a>(
        &'a self,
        db: &'a aimdb_core::builder::AimDb,
    ) -> Pin<Box<dyn Future<Output = aimdb_core::DbResult<Vec<BoxFuture>>> + Send + 'a>> {
        // No `.await` here, so the build future is `Send` without a wrapper: the
        // tunnelling connection task (which holds the `!Send` UDP socket) is
        // force-`Send`ed once via `into_box_future`; the data-flow rides core's
        // pumps with `Send` `Connector`/`Source` (KNX channels are
        // `CriticalSectionRawMutex`, i.e. `Send`).
        Box::pin(async move {
            let (command_channel, inbound_rx, connection_task) =
                KnxConnectorImpl::setup(self.gateway_url.as_str(), self.stack).map_err(|e| {
                    #[cfg(feature = "defmt")]
                    defmt::error!("Failed to build KNX connector");
                    aimdb_core::DbError::runtime_error(alloc::format!(
                        "Failed to build KNX connector: {}",
                        e
                    ))
                })?;

            // Outbound: records → KNX telegrams via the existing `Connector` impl.
            let mut futures = pump_sink(db, "knx", Arc::new(KnxConnectorImpl { command_channel }));
            // Inbound: KNX telegrams → records via the connection task's channel.
            futures.extend(pump_source(
                db,
                "knx",
                KnxSource {
                    receiver: inbound_rx,
                },
            ));
            // The KNX/IP tunnelling state machine (force-`Send` protocol task).
            futures.push(connection_task);

            Ok(futures)
        })
    }

    fn scheme(&self) -> &str {
        "knx"
    }
}

/// Internal KNX connector implementation
pub struct KnxConnectorImpl {
    command_channel: CommandChannelRef,
}

impl KnxConnectorImpl {
    /// Set up the command + inbound channels and the tunnelling connection task.
    ///
    /// Synchronous (no `.await`) so the caller's `build` future stays `Send`.
    /// Returns the command channel (for outbound `pump_sink`), the inbound
    /// receiver (for `pump_source`), and the force-`Send` connection task.
    fn setup(
        gateway_url: &str,
        stack: aimdb_embassy_adapter::connectors::NetStack,
    ) -> Result<(CommandChannelRef, InboundReceiver, BoxFuture), &'static str> {
        // Parse the gateway URL
        let connector_url = ConnectorUrl::parse(gateway_url).map_err(|_| "Invalid KNX URL")?;

        let host = connector_url.host.clone();
        let port = connector_url.port.unwrap_or(3671); // KNX/IP default port

        #[cfg(feature = "defmt")]
        defmt::trace!("Creating KNX connector for {}:{}", host.as_str(), port);

        // Parse gateway IP address
        let gateway_ip = Ipv4Address::from_str(&host).map_err(|_| "Invalid gateway IP address")?;

        // Get network stack for background task
        let network = stack.get();

        // Channels: outbound commands (publish → task) and inbound telegrams (task → pump_source).
        let command_channel = get_command_channel();
        let inbound_channel = get_inbound_channel();
        let inbound_tx = inbound_channel.sender();
        let inbound_rx = inbound_channel.receiver();

        // The KNX/IP tunnelling state machine (holds the `!Send` UDP socket — force-`Send`).
        let knx_task_future = into_box_future(async move {
            #[cfg(feature = "defmt")]
            defmt::trace!("KNX background task starting for {}:{}", gateway_ip, port);

            // Run the connection task (this never returns).
            connection_task(network, gateway_ip, port, command_channel, inbound_tx).await;
        });

        #[cfg(feature = "defmt")]
        defmt::trace!("KNX connector initialized");

        Ok((command_channel, inbound_rx, knx_task_future))
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
        // Validation shared with the tokio shim (same checks, same order);
        // boxed so the static channel stores pointers, not full payloads.
        let cmd = match GroupWrite::try_new(resource_id, payload) {
            Ok(cmd) => Box::new(cmd),
            Err(e) => return Box::pin(async move { Err(e) }),
        };
        let command_channel = self.command_channel;

        Box::pin(async move {
            // Send command to background task via channel
            command_channel.send(cmd).await;

            Ok(())
        })
    }
}

/// Current monotonic time in milliseconds for the engine.
fn now_ms() -> u64 {
    embassy_time::Instant::now().as_millis()
}

/// The connection task: socket I/O around the shared [`TunnelEngine`].
///
/// Creates a UDP socket (recreating it whenever the engine asks for a reset),
/// then loops: fire engine deadlines, apply the engine's actions, and select
/// over inbound datagrams, outbound commands, and the next engine deadline.
async fn connection_task(
    stack: &'static Stack<'static>,
    gateway_addr: Ipv4Address,
    gateway_port: u16,
    command_channel: CommandChannelRef,
    inbound_tx: InboundSender,
) {
    let mut engine = TunnelEngine::new(TunnelConfig::default(), now_ms());

    // Socket buffers outlive each per-connection socket below.
    let mut rx_meta = [PacketMetadata::EMPTY; 4];
    let mut rx_buffer = [0; 512];
    let mut tx_meta = [PacketMetadata::EMPTY; 4];
    let mut tx_buffer = [0; 512];

    loop {
        #[cfg(feature = "defmt")]
        defmt::info!(
            "🔌 Connecting to KNX gateway {}:{}",
            gateway_addr,
            gateway_port
        );

        let mut socket = UdpSocket::new(
            *stack,
            &mut rx_meta,
            &mut rx_buffer,
            &mut tx_meta,
            &mut tx_buffer,
        );

        if socket.bind(0).is_err() {
            #[cfg(feature = "defmt")]
            defmt::error!("Failed to bind KNX socket, retrying in 5s");
            drop(socket);
            embassy_time::Timer::after(embassy_time::Duration::from_secs(5)).await;
            continue;
        }

        // Drive the engine until it asks for a socket reset; the engine is
        // then in its backoff phase, so re-entering with a fresh socket only
        // reconnects once the backoff deadline passes.
        drive_connection(
            &mut engine,
            &mut socket,
            gateway_addr,
            gateway_port,
            command_channel,
            &inbound_tx,
        )
        .await;

        #[cfg(feature = "defmt")]
        defmt::trace!("KNX connection reset, reconnecting after backoff...");
    }
}

/// Socket-side glue for [`drain_actions`]: frames ride the `embassy-net` UDP
/// socket, parsed telegrams ride the static inbound channel into [`KnxSource`].
struct EmbassyIo<'a, 'b> {
    socket: &'a UdpSocket<'b>,
    gateway: (IpAddress, u16),
    inbound_tx: &'a InboundSender,
}

impl TunnelIo for EmbassyIo<'_, '_> {
    async fn send(&mut self, frame: &[u8]) {
        // Log-and-continue: a transient send error must not tear down the
        // tunnel; persistent socket death surfaces via the recv path.
        if self.socket.send_to(frame, self.gateway).await.is_err() {
            #[cfg(feature = "defmt")]
            defmt::error!("KNX send failed");
        }
    }

    fn forward(&mut self, addr: GroupAddress, payload: Vec<u8>) {
        let resource_id = addr.to_string();

        #[cfg(feature = "defmt")]
        defmt::trace!(
            "KNX telegram: {} (len={}) -> routing",
            resource_id.as_str(),
            payload.len()
        );

        if self
            .inbound_tx
            .try_send((resource_id, Payload::from(payload)))
            .is_err()
        {
            #[cfg(feature = "defmt")]
            defmt::warn!("KNX inbound channel full; dropped telegram");
        }
    }

    fn warn_ack_timeout(&mut self, seq: u8) {
        let _ = seq;
        #[cfg(feature = "defmt")]
        defmt::warn!("⚠️  ACK timeout for seq={}", seq);
    }
}

/// Drive the engine over one socket lifetime; returns when the engine asks
/// for a socket reset.
async fn drive_connection(
    engine: &mut TunnelEngine,
    socket: &mut UdpSocket<'_>,
    gateway_addr: Ipv4Address,
    gateway_port: u16,
    command_channel: CommandChannelRef,
    inbound_tx: &InboundSender,
) {
    use embassy_futures::select::{select, select3, Either, Either3};

    let gateway = (IpAddress::Ipv4(gateway_addr), gateway_port);

    loop {
        engine.poll(now_ms());

        {
            let mut io = EmbassyIo {
                socket,
                gateway,
                inbound_tx,
            };
            if drain_actions(engine, &mut io).await {
                return;
            }
        }

        let sleep_ms = engine.next_deadline().saturating_sub(now_ms());
        let deadline = embassy_time::Timer::after(embassy_time::Duration::from_millis(sleep_ms));
        let mut recv_buf = [0u8; 512];

        if engine.is_connected() {
            match select3(
                socket.recv_from(&mut recv_buf),
                command_channel.receive(),
                deadline,
            )
            .await
            {
                Either3::First(Ok((len, _peer))) => {
                    engine.handle_datagram(&recv_buf[..len], now_ms());
                }
                Either3::First(Err(_)) => {
                    #[cfg(feature = "defmt")]
                    defmt::error!("Socket receive error");
                    engine.handle_socket_error(now_ms());
                }
                Either3::Second(cmd) => {
                    if !engine.handle_command(*cmd, now_ms()) {
                        #[cfg(feature = "defmt")]
                        defmt::warn!("Not connected, dropping GroupWrite");
                    }
                }
                // Wake for the engine deadline; `poll` at the loop top fires it.
                Either3::Third(()) => {}
            }
        } else {
            // While connecting / backing off, leave commands queued in the
            // channel so they flush once the handshake completes (same as the
            // previous implementation, where the select loop only ran while
            // connected).
            match select(socket.recv_from(&mut recv_buf), deadline).await {
                Either::First(Ok((len, _peer))) => {
                    engine.handle_datagram(&recv_buf[..len], now_ms());
                }
                Either::First(Err(_)) => {
                    #[cfg(feature = "defmt")]
                    defmt::error!("Socket receive error");
                    engine.handle_socket_error(now_ms());
                }
                Either::Second(()) => {}
            }
        }
    }
}
