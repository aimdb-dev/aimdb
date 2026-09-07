//! The TLS transport for the embedded backend.
//!
//! `mqtts://` broker sessions: an `embedded-tls` 1.3 session over an Embassy
//! TCP socket, presented to the MQTT layer as its own `Connection` — not
//! `ConnectionEmbedded`, which needs a `ReadReady` a TLS session cannot give
//! (see `TlsSession` below). Certificate verification is `rustpki` (pure Rust)
//! against the application-embedded root CA, with time from the [`sntp`] task;
//! entropy comes from the application-injected TRNG ([`TlsOptions::new`]).
//!
//! The session loop is mountain-mqtt-embassy's own public `handle_messages`
//! (with `State` / `ChannelEventHandler`):
//! it is transport-agnostic (generic over `Client`), so the only thing this
//! module supplies is the transport — resolve → TCP → TLS handshake → session.
//! Upstream `run()` shares that exact loop, keeping the plain and TLS paths in
//! lock-step with no copied code to drift.

use alloc::string::String;
use alloc::vec::Vec;
use core::cell::RefCell;
use core::net::IpAddr;

use alloc::sync::Arc;
use embassy_net::dns::DnsQueryType;
use embassy_net::tcp::TcpSocket;
use embassy_net::{IpAddress, Stack};
use embassy_time::{Delay, Timer};

use embedded_tls::pki::CertVerifier;
use embedded_tls::{
    Aes128GcmSha256, Certificate, CryptoProvider, CryptoRngCore, TlsConfig, TlsConnection,
    TlsContext, TlsError, TlsVerifier,
};

use embedded_io_async::Write as _;

use crate::embedded::manager::{
    handle_messages, now_ms, ChannelEventHandler, MqttEvent, SessionState, Settings,
};
use mountain_mqtt::client::{ClientNoQueue, ConnectionSettings};
use mountain_mqtt::data::quality_of_service::QualityOfService;
use mountain_mqtt::embedded_hal_async::DelayEmbedded;
use mountain_mqtt::error::{PacketReadError, PacketWriteError};
use mountain_mqtt::mqtt_manager::ConnectionId;
use mountain_mqtt::packet_client::Connection;

use crate::embedded::sntp::{self, SntpClock};
use crate::embedded::{AimdbMqttAction, AimdbMqttEvent, BUFFER_SIZE, CHANNEL_SIZE, MAX_PROPERTIES};

/// Room for the server's leaf certificate (DER) inside the verifier — 4 KB
/// covers RSA-4096 leaves with headroom.
const CERT_BUFFER_SIZE: usize = 4096;

/// Minimum TLS record read buffer: a TLS 1.3 peer may send full-size records
/// (2^14 payload + record overhead) regardless of our `max_fragment_length`
/// offer, and `embedded-tls` fails any record larger than the buffer — a
/// smaller buffer works until the first big record, then reconnect-loops.
/// Enforced at `build()` so undersizing fails loudly instead.
pub(crate) const READ_BUF_MIN: usize = 16_640;

/// TLS materials for a `mqtts://` broker connection.
///
/// All references are `'static`: the session outlives `build()`, so buffers
/// and the RNG live in `StaticCell`s (or equivalents) owned by the
/// application — the one party that knows the board's memory budget.
pub struct TlsOptions {
    pub(crate) rng: &'static mut (dyn CryptoRngCore + Send),
    pub(crate) ca_der: &'static [u8],
    pub(crate) read_buf: &'static mut [u8],
    pub(crate) write_buf: &'static mut [u8],
    pub(crate) sntp_server: &'static str,
}

impl TlsOptions {
    /// TLS with certificate verification against `ca_der` (the root CA, DER).
    ///
    /// * `rng` — CSPRNG for the handshake; on STM32 the hardware TRNG
    ///   (`embassy_stm32::rng::Rng` implements `CryptoRngCore`). Must be
    ///   `Send`, which every concrete CSPRNG satisfies.
    /// * `read_buf` — TLS record read buffer. At least 16 640 bytes (a
    ///   TLS 1.3 peer may send full-size records regardless of our
    ///   `max_fragment_length` offer); `build()` rejects smaller buffers.
    /// * `write_buf` — TLS record write buffer; 4 096 bytes is plenty for
    ///   MQTT-sized writes.
    pub fn new(
        rng: &'static mut (dyn CryptoRngCore + Send),
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
/// Known limitation: wire bytes that decrypt to *no* application data
/// (unsolicited session tickets, KeyUpdate) make `receive` wait for the next
/// real record; if the broker stays silent, the keep-alive lapse tears the
/// session down and the manager reconnects.
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
/// instead.
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
/// resolves `host` per attempt instead).
#[allow(clippy::too_many_arguments)]
pub(crate) async fn run_tls(
    stack: Stack<'static>,
    options: TlsOptions,
    host: String,
    port: u16,
    topics: Vec<String>,
    connection_settings: ConnectionSettings<'static>,
    settings: Settings,
    events: Arc<crate::embedded::EventChannel>,
    actions: Arc<crate::embedded::ActionChannel>,
    runtime: Arc<dyn aimdb_core::RuntimeOps>,
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

    // Re-subscribed by `handle_messages` on every (re)connection, so inbound
    // routing survives reconnects. Built once — borrows `topics` for the loop.
    let subscribe_topics: Vec<(&str, QualityOfService)> = topics
        .iter()
        .map(|topic| (topic.as_str(), QualityOfService::Qos1))
        .collect();

    let mut connection_index = 0u32;

    loop {
        // Certificate validity needs real time — hold the first handshake
        // until SNTP has synced.
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
                aimdb_core::session::Delay::sleep(&EmbassyCoreDelay, settings.reconnection_delay)
                    .await;
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
        if let Err(e) = socket.connect((address, port)).await {
            #[cfg(feature = "defmt")]
            defmt::warn!("MQTT-TLS: socket connect error, will retry: {:?}", e);
            #[cfg(not(feature = "defmt"))]
            let _ = e;
            aimdb_core::session::Delay::sleep(&EmbassyCoreDelay, settings.reconnection_delay).await;
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
            aimdb_core::session::Delay::sleep(&EmbassyCoreDelay, settings.reconnection_delay).await;
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

        let state: SessionState<AimdbMqttAction> = SessionState::new(now_ms(runtime.as_ref()));

        let connection_id = ConnectionId::new(connection_index);
        connection_index += 1;

        let event_handler: ChannelEventHandler<
            '_,
            AimdbMqttAction,
            AimdbMqttEvent,
            MAX_PROPERTIES,
            CHANNEL_SIZE,
        > = ChannelEventHandler::new(connection_id, &events, &state, runtime.as_ref());

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
            &subscribe_topics,
            &events,
            &actions,
            &settings,
            &EmbassyCoreDelay,
            runtime.as_ref(),
        )
        .await
        {
            #[cfg(feature = "defmt")]
            defmt::warn!("MQTT-TLS: session errored: {:?}", error);
            events
                .send(MqttEvent::Disconnected {
                    connection_id,
                    error,
                })
                .await;
        }

        aimdb_core::session::Delay::sleep(&EmbassyCoreDelay, settings.reconnection_delay).await;
    }
}

/// The TLS path keeps `embassy_time` for its own waits, so it supplies core's
/// [`Delay`](aimdb_core::session::Delay) to the shared message pump.
struct EmbassyCoreDelay;

impl aimdb_core::session::Delay for EmbassyCoreDelay {
    fn sleep(&self, d: core::time::Duration) -> impl core::future::Future<Output = ()> + Send {
        Timer::after(embassy_time::Duration::from_micros(d.as_micros() as u64))
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

/// Parse the broker host as an IP literal (with or without URL-style
/// brackets, `[::1]`). `build()` uses this to vet `mqtts://` hosts:
/// certificate verification prefers a DNS name, but an IPv4 literal can
/// still pass through `rustpki`'s CN fallback when a private CA pins the
/// dotted quad there (the dev bench does) — allowed with a warning. An IPv6
/// literal can never match (the verifier's hostname charset has no `:`) and
/// is rejected.
pub(crate) fn host_ip_literal(host: &str) -> Option<IpAddr> {
    let host = host
        .strip_prefix('[')
        .and_then(|h| h.strip_suffix(']'))
        .unwrap_or(host);
    host.parse::<IpAddr>().ok()
}
