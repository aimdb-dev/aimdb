//! TLS transport for the Embassy MQTT client (design 044).
//!
//! `mqtts://` broker sessions: an `embedded-tls` 1.3 session over the Embassy
//! TCP socket, wrapped in mountain-mqtt's [`ConnectionEmbedded`] so the MQTT
//! layer is identical to the plain path. Certificate verification is
//! `rustpki` (pure Rust) against the application-embedded root CA, with time
//! from the [`sntp`](crate::sntp) task; entropy comes from the
//! application-injected TRNG ([`TlsOptions::new`]).
//!
//! The session loop ([`handle_messages`], [`State`], [`ChannelEventHandler`])
//! is a port of mountain-mqtt-embassy's private equivalents (MIT OR
//! Apache-2.0): upstream `run()` constructs a bare `TcpSocket` internally and
//! offers no seam for a TLS session (design 044 D6). Keep the loop in sync
//! with the fork when bumping it.

use alloc::string::String;
use alloc::vec::Vec;
use core::cell::RefCell;
use core::net::Ipv4Addr;

use embassy_net::dns::DnsQueryType;
use embassy_net::tcp::TcpSocket;
use embassy_net::{IpAddress, Stack};
use embassy_sync::blocking_mutex::raw::NoopRawMutex;
use embassy_sync::channel::{Receiver, Sender};
use embassy_time::{Delay, Instant, Timer};

use embedded_tls::pki::CertVerifier;
use embedded_tls::{
    Aes128GcmSha256, Certificate, CryptoProvider, CryptoRngCore, TlsConfig, TlsConnection,
    TlsContext, TlsError, TlsVerifier,
};

use embedded_io_async::Write as _;

use mountain_mqtt::client::{
    Client, ClientError, ClientNoQueue, ClientReceivedEvent, ConnectionSettings, EventHandler,
    EventHandlerError,
};
use mountain_mqtt::data::quality_of_service::QualityOfService;
use mountain_mqtt::embedded_hal_async::DelayEmbedded;
use mountain_mqtt::error::{PacketReadError, PacketWriteError};
use mountain_mqtt::mqtt_manager::{ConnectionId, MqttOperations};
use mountain_mqtt::packet_client::Connection;
use mountain_mqtt_embassy::mqtt_manager::{Error, FromApplicationMessage, MqttEvent, Settings};

use crate::embassy_client::{
    AimdbMqttAction, AimdbMqttEvent, BUFFER_SIZE, CHANNEL_SIZE, MAX_PROPERTIES,
};
use crate::sntp::{self, SntpClock};

/// Room for the server's leaf certificate (DER) inside the verifier — 4 KB
/// covers RSA-4096 leaves with headroom.
const CERT_BUFFER_SIZE: usize = 4096;

/// TLS materials for a `mqtts://` broker connection (design 044 §4).
///
/// All references are `'static`: the session outlives `build()`, so buffers
/// and the RNG live in `StaticCell`s (or equivalents) owned by the
/// application — the one party that knows the board's memory budget.
pub struct TlsOptions {
    pub(crate) rng: &'static mut dyn CryptoRngCore,
    pub(crate) ca_der: &'static [u8],
    pub(crate) read_buf: &'static mut [u8],
    pub(crate) write_buf: &'static mut [u8],
    pub(crate) sntp_server: &'static str,
}

impl TlsOptions {
    /// TLS with certificate verification against `ca_der` (the root CA, DER).
    ///
    /// * `rng` — CSPRNG for the handshake; on STM32 the hardware TRNG
    ///   (`embassy_stm32::rng::Rng` implements `CryptoRngCore`).
    /// * `read_buf` — TLS record read buffer. 16 640 bytes recommended: a
    ///   TLS 1.3 peer may send full-size records regardless of our
    ///   `max_fragment_length` offer.
    /// * `write_buf` — TLS record write buffer; 4 096 bytes is plenty for
    ///   MQTT-sized writes.
    pub fn new(
        rng: &'static mut dyn CryptoRngCore,
        ca_der: &'static [u8],
        read_buf: &'static mut [u8],
        write_buf: &'static mut [u8],
    ) -> Self {
        Self {
            rng,
            ca_der,
            read_buf,
            write_buf,
            sntp_server: "pool.ntp.org",
        }
    }

    /// Override the SNTP server used as the certificate-validation time
    /// source (default `pool.ntp.org`).
    pub fn with_sntp_server(mut self, server: &'static str) -> Self {
        self.sntp_server = server;
        self
    }
}

/// The TCP socket shared between the TLS session (its transport) and the
/// MQTT-level readiness probe ([`TlsSession::receive_if_ready`]), which needs
/// `can_recv()` after the socket has been handed to `embedded-tls`.
///
/// Borrow discipline: the session task drives exactly one client operation at
/// a time, so a `borrow_mut` held across an I/O `.await` can never overlap
/// the probe's short `borrow` — both are called sequentially from the same
/// loop.
struct SharedTcp<'r, 'a>(&'r RefCell<TcpSocket<'a>>);

impl Clone for SharedTcp<'_, '_> {
    fn clone(&self) -> Self {
        Self(self.0)
    }
}

impl SharedTcp<'_, '_> {
    fn can_recv(&self) -> bool {
        self.0.borrow().can_recv()
    }
}

impl embedded_io_async::ErrorType for SharedTcp<'_, '_> {
    type Error = embassy_net::tcp::Error;
}

// The held-across-await borrows below are safe by the struct-level borrow
// discipline (sequential single-task use); a panic would mean a second client
// operation ran concurrently, which the session loop cannot do.
#[allow(clippy::await_holding_refcell_ref)]
impl embedded_io_async::Read for SharedTcp<'_, '_> {
    async fn read(&mut self, buf: &mut [u8]) -> Result<usize, Self::Error> {
        self.0.borrow_mut().read(buf).await
    }
}

#[allow(clippy::await_holding_refcell_ref)]
impl embedded_io_async::Write for SharedTcp<'_, '_> {
    async fn write(&mut self, buf: &[u8]) -> Result<usize, Self::Error> {
        self.0.borrow_mut().write(buf).await
    }

    async fn flush(&mut self) -> Result<(), Self::Error> {
        self.0.borrow_mut().flush().await
    }
}

/// mountain-mqtt [`Connection`] over an open TLS session.
///
/// Not `ConnectionEmbedded`: that adapter needs `ReadReady`, which
/// [`TlsConnection`] cannot offer — and TLS readiness is two-layered anyway.
/// Data can be ready as already-decrypted plaintext left over from a record
/// that carried more than one MQTT packet (`plaintext_remaining`), or as
/// undecrypted bytes on the wire (`can_recv` on the shared socket). Checking
/// both keeps coalesced packets flowing promptly.
///
/// Known limitation (documented in design 044): wire bytes that decrypt to
/// *no* application data (unsolicited session tickets, KeyUpdate) make
/// `receive` wait for the next real record; if the broker stays silent, the
/// keep-alive lapse tears the session down and the manager reconnects.
struct TlsSession<'r, 'a, 'b> {
    tls: TlsConnection<'b, SharedTcp<'r, 'a>, Aes128GcmSha256>,
    socket: SharedTcp<'r, 'a>,
    /// Decrypted-but-unread plaintext left in the TLS record buffer.
    plaintext_remaining: usize,
}

impl Connection for TlsSession<'_, '_, '_> {
    async fn send(&mut self, buf: &[u8]) -> Result<(), PacketWriteError> {
        self.tls
            .write_all(buf)
            .await
            .map_err(|_| PacketWriteError::ConnectionSend)?;
        self.tls
            .flush()
            .await
            .map_err(|_| PacketWriteError::ConnectionSend)
    }

    async fn receive(&mut self, buf: &mut [u8]) -> Result<(), PacketReadError> {
        let mut filled = 0;
        while filled < buf.len() {
            let mut read_buffer = self
                .tls
                .read_buffered()
                .await
                .map_err(|_| PacketReadError::ConnectionReceive)?;
            filled += read_buffer.pop_into(&mut buf[filled..]);
            self.plaintext_remaining = read_buffer.len();
        }
        Ok(())
    }

    async fn receive_if_ready(&mut self, buf: &mut [u8]) -> Result<bool, PacketReadError> {
        if self.plaintext_remaining == 0 && !self.socket.can_recv() {
            return Ok(false);
        }
        self.receive(buf).await?;
        Ok(true)
    }
}

/// [`CryptoProvider`] pairing the injected TRNG with `rustpki` certificate
/// verification (time from [`SntpClock`]). Client-certificate signing is
/// deliberately absent — the mesh authenticates with MQTT credentials
/// (design 044 non-goals).
struct TrngProvider<'a> {
    rng: &'a mut dyn CryptoRngCore,
    verifier: CertVerifier<'static, Aes128GcmSha256, SntpClock, CERT_BUFFER_SIZE>,
}

impl CryptoProvider for TrngProvider<'_> {
    type CipherSuite = Aes128GcmSha256;
    // Unused (no client certificates); any `AsRef<[u8]>` satisfies the bound.
    type Signature = &'static [u8];

    fn rng(&mut self) -> impl CryptoRngCore {
        &mut *self.rng
    }

    fn verifier(&mut self) -> Result<&mut impl TlsVerifier<Self::CipherSuite>, TlsError> {
        Ok(&mut self.verifier)
    }
}

/// The TLS broker manager: resolve → TCP → TLS handshake → MQTT session,
/// reconnecting forever with the same [`Settings`] cadence as the plain
/// path's `mqtt_manager::run` (`settings.address` is unused — the TLS path
/// resolves `host` per attempt, design 044 D7).
#[allow(clippy::too_many_arguments)]
pub(crate) async fn run_tls(
    stack: Stack<'static>,
    options: TlsOptions,
    host: String,
    topics: Vec<String>,
    connection_settings: ConnectionSettings<'static>,
    settings: Settings,
    event_sender: Sender<'static, NoopRawMutex, MqttEvent<AimdbMqttEvent>, CHANNEL_SIZE>,
    mut action_receiver: Receiver<'static, NoopRawMutex, AimdbMqttAction, CHANNEL_SIZE>,
) -> ! {
    let TlsOptions {
        rng,
        ca_der,
        read_buf,
        write_buf,
        ..
    } = options;

    let mut rx_buffer = [0u8; BUFFER_SIZE];
    let mut tx_buffer = [0u8; BUFFER_SIZE];
    let mut mqtt_buffer = [0u8; BUFFER_SIZE];

    let mut connection_index = 0u32;

    loop {
        // Certificate validity needs real time — hold the first handshake
        // until SNTP has synced (design 044 D5).
        if sntp::unix_now().is_none() {
            #[cfg(feature = "defmt")]
            defmt::info!("MQTT-TLS: waiting for SNTP time sync...");
            while sntp::unix_now().is_none() {
                Timer::after_millis(500).await;
            }
        }

        let address = match resolve(stack, &host).await {
            Some(address) => address,
            None => {
                #[cfg(feature = "defmt")]
                defmt::warn!(
                    "MQTT-TLS: DNS lookup for {} failed, will retry",
                    host.as_str()
                );
                Timer::after(settings.reconnection_delay).await;
                continue;
            }
        };

        let mut socket = TcpSocket::new(stack, &mut rx_buffer, &mut tx_buffer);
        socket.set_timeout(None);

        #[cfg(feature = "defmt")]
        defmt::info!(
            "MQTT-TLS: connecting to {} ({}) port {}...",
            host.as_str(),
            address,
            settings.port
        );
        if let Err(e) = socket.connect((address, settings.port)).await {
            #[cfg(feature = "defmt")]
            defmt::warn!("MQTT-TLS: socket connect error, will retry: {:?}", e);
            #[cfg(not(feature = "defmt"))]
            let _ = e;
            Timer::after(settings.reconnection_delay).await;
            continue;
        }

        let socket = RefCell::new(socket);
        let shared = SharedTcp(&socket);

        let tls_config = TlsConfig::new().with_server_name(&host);
        let mut tls = TlsConnection::new(shared.clone(), &mut *read_buf, &mut *write_buf);
        let provider = TrngProvider {
            rng: &mut *rng,
            verifier: CertVerifier::new(Certificate::X509(ca_der)),
        };
        if let Err(e) = tls.open(TlsContext::new(&tls_config, provider)).await {
            #[cfg(feature = "defmt")]
            defmt::warn!(
                "MQTT-TLS: handshake failed, will retry: {:?}",
                defmt::Debug2Format(&e)
            );
            #[cfg(not(feature = "defmt"))]
            let _ = e;
            Timer::after(settings.reconnection_delay).await;
            continue;
        }
        #[cfg(feature = "defmt")]
        defmt::info!("MQTT-TLS: session established");

        let connection = TlsSession {
            tls,
            socket: shared,
            plaintext_remaining: 0,
        };
        let delay = DelayEmbedded::new(Delay);
        let timeout_millis = settings.response_timeout.as_millis() as u32;

        let state = RefCell::new(State::new());

        let connection_id = ConnectionId::new(connection_index);
        connection_index += 1;

        let event_handler = ChannelEventHandler {
            connection_id,
            event_sender: &event_sender,
            state: &state,
        };

        let mut client = ClientNoQueue::new(
            connection,
            &mut mqtt_buffer,
            delay,
            timeout_millis,
            event_handler,
        );

        if let Err(error) = handle_messages(
            connection_id,
            &mut client,
            &state,
            &connection_settings,
            &topics,
            &event_sender,
            &mut action_receiver,
            &settings,
        )
        .await
        {
            #[cfg(feature = "defmt")]
            defmt::warn!("MQTT-TLS: session errored: {:?}", error);
            event_sender
                .send(MqttEvent::Disconnected {
                    connection_id,
                    error,
                })
                .await;
        }

        Timer::after(settings.reconnection_delay).await;
    }
}

/// Resolve the broker host to its first A record (IP literals short-circuit
/// inside `dns_query` without a network round trip).
async fn resolve(stack: Stack<'static>, host: &str) -> Option<IpAddress> {
    match stack.dns_query(host, DnsQueryType::A).await {
        Ok(addresses) => addresses.first().copied(),
        Err(_) => None,
    }
}

/// SNI requires a name, not an address — used by `build()` to reject broker
/// URLs that can never verify.
pub(crate) fn host_is_ip_literal(host: &str) -> bool {
    host.parse::<Ipv4Addr>().is_ok()
}

// --- Ported session loop (see module doc) ---------------------------------

struct State {
    /// The instant of the most recent proof the connection is live (set at
    /// connect, refreshed on every Ack from the server).
    last_connection_event: Instant,
    /// A failed action to retry on the next loop turn / connection.
    pending_action: Option<AimdbMqttAction>,
}

impl State {
    fn new() -> Self {
        Self {
            last_connection_event: Instant::now(),
            pending_action: None,
        }
    }
    fn record_connection_event(&mut self) {
        self.last_connection_event = Instant::now();
    }
}

struct ChannelEventHandler<'a> {
    connection_id: ConnectionId,
    event_sender: &'a Sender<'static, NoopRawMutex, MqttEvent<AimdbMqttEvent>, CHANNEL_SIZE>,
    state: &'a RefCell<State>,
}

impl EventHandler<MAX_PROPERTIES> for ChannelEventHandler<'_> {
    async fn handle_event(
        &mut self,
        event: ClientReceivedEvent<'_, MAX_PROPERTIES>,
    ) -> Result<(), EventHandlerError> {
        match event {
            ClientReceivedEvent::ApplicationMessage(message) => {
                let event = AimdbMqttEvent::from_application_message(&message)?;
                self.event_sender
                    .send(MqttEvent::ApplicationEvent {
                        connection_id: self.connection_id,
                        event,
                    })
                    .await;
            }
            ClientReceivedEvent::Ack => {
                self.state.borrow_mut().record_connection_event();
            }
            ClientReceivedEvent::SubscriptionGrantedBelowMaximumQos {
                granted_qos,
                maximum_qos,
            } => {
                self.event_sender
                    .send(MqttEvent::SubscriptionGrantedBelowMaximumQos {
                        connection_id: self.connection_id,
                        granted_qos,
                        maximum_qos,
                    })
                    .await
            }
            ClientReceivedEvent::PublishedMessageHadNoMatchingSubscribers => {
                self.event_sender
                    .send(MqttEvent::PublishedMessageHadNoMatchingSubscribers {
                        connection_id: self.connection_id,
                    })
                    .await
            }
            ClientReceivedEvent::NoSubscriptionExisted => {
                self.event_sender
                    .send(MqttEvent::NoSubscriptionExisted {
                        connection_id: self.connection_id,
                    })
                    .await
            }
        }
        Ok(())
    }
}

async fn try_action<'a, C>(
    current_connection_id: ConnectionId,
    client: &mut C,
    state: &RefCell<State>,
    connection_settings: &ConnectionSettings<'static>,
    mut action: AimdbMqttAction,
    is_retry: bool,
) -> Result<(), ClientError>
where
    C: Client<'a>,
{
    if let Err(e) = action
        .perform(
            client,
            connection_settings.client_id(),
            current_connection_id,
            is_retry,
        )
        .await
    {
        state.borrow_mut().pending_action = Some(action);
        return Err(e);
    }
    Ok(())
}

/// Connect, re-subscribe the inbound routes, then handle messages until an
/// error ends the session. Unlike the plain path (which queues subscriptions
/// once at startup), subscribing per session means inbound routing survives
/// reconnects.
#[allow(clippy::too_many_arguments)]
async fn handle_messages<'a, C>(
    current_connection_id: ConnectionId,
    client: &mut C,
    state: &RefCell<State>,
    connection_settings: &ConnectionSettings<'static>,
    topics: &[String],
    event_sender: &Sender<'static, NoopRawMutex, MqttEvent<AimdbMqttEvent>, CHANNEL_SIZE>,
    action_receiver: &mut Receiver<'static, NoopRawMutex, AimdbMqttAction, CHANNEL_SIZE>,
    settings: &Settings,
) -> Result<(), Error>
where
    C: Client<'a>,
{
    client.connect(connection_settings).await?;

    event_sender
        .send(MqttEvent::Connected {
            connection_id: current_connection_id,
        })
        .await;

    for topic in topics {
        client.subscribe(topic, QualityOfService::Qos1).await?;
    }

    let mut connection_instant = Some(Instant::now());
    let mut last_ping_instant = Instant::now();

    loop {
        Timer::after(settings.poll_interval).await;

        if last_ping_instant.elapsed() > settings.ping_interval {
            last_ping_instant = Instant::now();
            client.send_ping().await?;
        }

        // Check for stabilisation
        if let Some(instant) = connection_instant {
            if instant.elapsed() > settings.stabilisation_interval {
                connection_instant = None;
                event_sender
                    .send(MqttEvent::ConnectionStable {
                        connection_id: current_connection_id,
                    })
                    .await;
            }
        }

        // Check for too long since last connection event
        let elapsed = state.borrow().last_connection_event.elapsed();
        if elapsed > settings.connection_event_max_interval {
            #[cfg(feature = "defmt")]
            defmt::warn!("MQTT-TLS: server unresponsive");
            return Err(Error::MqttServerUnresponsive);
        }

        // Poll with no delay while we have mqtt packets
        while client.poll(false).await? {}

        // If we have a pending action, try to perform it
        let pending_action = state.borrow_mut().pending_action.take();
        if let Some(action) = pending_action {
            try_action(
                current_connection_id,
                client,
                state,
                connection_settings,
                action,
                true,
            )
            .await?;
        }

        // Handle actions from receiver
        while let Ok(action) = action_receiver.try_receive() {
            try_action(
                current_connection_id,
                client,
                state,
                connection_settings,
                action,
                false,
            )
            .await?;
        }
    }
}
