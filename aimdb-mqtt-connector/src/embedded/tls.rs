//! The TLS transport for the embedded backend.
//!
//! `mqtts://` broker sessions: an `embedded-tls` 1.3 session over the caller's
//! transport, presented to the MQTT layer as its own `Connection` — not
//! `ConnectionEmbedded`, which needs a `ReadReady` a TLS session cannot give
//! (see `TlsSession` below). Certificate verification is `rustpki` (pure Rust)
//! against the application-embedded root CA, dated by the runtime's wall
//! clock; entropy
//! comes from the application-injected TRNG ([`TlsOptions::new`]).
//!
//! The dialer resolves the host, so there is no network stack here: the same
//! session runs on a host over the Tokio adapter's transport.

use alloc::string::String;
use alloc::vec::Vec;
use core::cell::RefCell;
use core::net::IpAddr;

use alloc::sync::Arc;

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

use crate::embedded::{AimdbMqttEvent, BUFFER_SIZE, CHANNEL_SIZE, MAX_PROPERTIES};

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
    /// Where the certificate-validity clock comes from on a board with no RTC.
    /// `None` means the runtime's own wall clock answers.
    #[cfg(feature = "embassy-tls")]
    pub(crate) sntp: Option<(aimdb_embassy_adapter::connectors::NetStack, &'static str)>,
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
            #[cfg(feature = "embassy-tls")]
            sntp: None,
        }
    }

    /// Take the certificate-validity clock from SNTP over `stack`.
    ///
    /// Needed only where the runtime has no wall clock of its own — an MCU
    /// with no RTC. A host runtime answers `unix_time()` and needs no task.
    #[cfg(feature = "embassy-tls")]
    pub fn with_sntp(
        mut self,
        stack: &'static embassy_net::Stack<'static>,
        server: &'static str,
    ) -> Self {
        // SAFETY: AimDB's Embassy integration requires a single-core
        // cooperative executor (the adapter's module-level invariant); the
        // SNTP task touching this stack is polled on that executor.
        self.sntp = Some((
            unsafe { aimdb_embassy_adapter::connectors::NetStack::new(stack) },
            server,
        ));
        self
    }
}

/// The stream shared between the TLS session (its transport) and the
/// MQTT-level readiness probe ([`TlsSession::receive_if_ready`]), which needs
/// to ask the wire after the stream has been handed to `embedded-tls`.
///
/// Borrow discipline: the session task drives exactly one client operation at
/// a time, so a `borrow_mut` held across an I/O `.await` can never overlap
/// the probe's short `borrow` — both are called sequentially from the same
/// loop.
struct SharedStream<'r, S>(&'r RefCell<S>);

impl<S> Clone for SharedStream<'_, S> {
    fn clone(&self) -> Self {
        Self(self.0)
    }
}

impl<S: embedded_io_async::ReadReady> SharedStream<'_, S> {
    fn can_recv(&self) -> bool {
        self.0.borrow_mut().read_ready().unwrap_or(false)
    }
}

impl<S: embedded_io_async::ErrorType> embedded_io_async::ErrorType for SharedStream<'_, S> {
    type Error = S::Error;
}

// The held-across-await borrows below are safe by the struct-level borrow
// discipline (sequential single-task use); a panic would mean a second client
// operation ran concurrently, which the session loop cannot do.
#[allow(clippy::await_holding_refcell_ref)]
impl<S: embedded_io_async::Read> embedded_io_async::Read for SharedStream<'_, S> {
    async fn read(&mut self, buf: &mut [u8]) -> Result<usize, Self::Error> {
        self.0.borrow_mut().read(buf).await
    }
}

#[allow(clippy::await_holding_refcell_ref)]
impl<S: embedded_io_async::Write> embedded_io_async::Write for SharedStream<'_, S> {
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
struct TlsSession<'r, 'b, S>
where
    S: embedded_io_async::Read + embedded_io_async::Write,
{
    tls: TlsConnection<'b, SharedStream<'r, S>, Aes128GcmSha256>,
    socket: SharedStream<'r, S>,
    /// Decrypted-but-unread plaintext left in the TLS record buffer.
    plaintext_remaining: usize,
}

impl<S> Connection for TlsSession<'_, '_, S>
where
    S: embedded_io_async::Read + embedded_io_async::Write + embedded_io_async::ReadReady,
{
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

/// Unix seconds for certificate validity, refreshed before each handshake.
///
/// `embedded_tls::TlsClock::now` is a static method, so the reading has to
/// reach it through a global. The source is whatever the runtime's wall clock
/// reports; a runtime with no clock of its own (an MCU without an RTC) gets
/// one from the SNTP task instead.
static UNIX_SECS: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);

/// The certificate-validity clock. `u32` is unambiguous until 2106 and stays a
/// single atomic on Cortex-M, which has no 64-bit atomics.
pub(crate) struct WallClock;

impl WallClock {
    /// Record a wall-clock reading. Ignores a zero, which means "unknown".
    pub(crate) fn set_unix_secs(secs: u32) {
        if secs != 0 {
            UNIX_SECS.store(secs, core::sync::atomic::Ordering::Relaxed);
        }
    }

    fn unix_secs() -> Option<u64> {
        match UNIX_SECS.load(core::sync::atomic::Ordering::Relaxed) {
            0 => None,
            secs => Some(u64::from(secs)),
        }
    }
}

impl embedded_tls::TlsClock for WallClock {
    fn now() -> Option<u64> {
        Self::unix_secs()
    }
}

/// [`CryptoProvider`] pairing the injected TRNG with `rustpki` certificate
/// verification (time from [`WallClock`]). Client-certificate signing is
/// deliberately absent — the mesh authenticates with MQTT credentials
/// instead.
struct TrngProvider<'a> {
    rng: &'a mut (dyn CryptoRngCore + Send),
    verifier: CertVerifier<'static, Aes128GcmSha256, WallClock, CERT_BUFFER_SIZE>,
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

/// The TLS broker manager: dial → TLS handshake → MQTT session, reconnecting
/// forever with the same [`Settings`] cadence as the plain path.
///
/// The dialer resolves the host, so there is no DNS here and no network stack:
/// any runtime whose streams offer the `embedded-io-async` trio can run this.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn run_tls<D>(
    dialer: D,
    options: TlsOptions,
    host: String,
    port: u16,
    topics: Vec<String>,
    connection_settings: ConnectionSettings<'static>,
    settings: Settings,
    events: Arc<crate::embedded::EventChannel>,
    actions: Arc<crate::embedded::ActionChannel>,
    delay: D,
    runtime: Arc<dyn aimdb_core::RuntimeOps>,
) -> !
where
    D: aimdb_core::session::StreamDialer + aimdb_core::session::Delay,
    D::Stream: embedded_io_async::Read + embedded_io_async::Write + embedded_io_async::ReadReady,
{
    let TlsOptions {
        rng,
        ca_der,
        read_buf,
        write_buf,
        ..
    } = options;

    let mut mqtt_buffer = [0u8; BUFFER_SIZE];

    // Re-subscribed by `handle_messages` on every (re)connection, so inbound
    // routing survives reconnects. Built once — borrows `topics` for the loop.
    let subscribe_topics: Vec<(&str, QualityOfService)> = topics
        .iter()
        .map(|topic| (topic.as_str(), QualityOfService::Qos1))
        .collect();

    let mut connection_index = 0u32;

    loop {
        // Certificate validity needs real time. Take it from the runtime when
        // it has a wall clock; otherwise wait for whatever feeds `WallClock`
        // (the SNTP task, on a board with no RTC).
        if let Some((secs, _)) = runtime.unix_time() {
            WallClock::set_unix_secs(secs as u32);
        }
        if WallClock::unix_secs().is_none() {
            #[cfg(feature = "defmt")]
            defmt::info!("MQTT-TLS: waiting for a wall-clock reading...");
            while WallClock::unix_secs().is_none() {
                if let Some((secs, _)) = runtime.unix_time() {
                    WallClock::set_unix_secs(secs as u32);
                }
                aimdb_core::session::Delay::sleep(&delay, core::time::Duration::from_millis(500))
                    .await;
            }
        }

        let stream = match dialer.connect(&host, port).await {
            Ok(stream) => stream,
            Err(_e) => {
                #[cfg(feature = "defmt")]
                defmt::warn!("MQTT-TLS: connect failed, will retry");
                aimdb_core::session::Delay::sleep(&delay, settings.reconnection_delay).await;
                continue;
            }
        };

        let stream = RefCell::new(stream);
        let shared = SharedStream(&stream);

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
            aimdb_core::session::Delay::sleep(&delay, settings.reconnection_delay).await;
            continue;
        }
        #[cfg(feature = "defmt")]
        defmt::info!("MQTT-TLS: session established");

        let connection = TlsSession {
            tls,
            socket: shared,
            plaintext_remaining: 0,
        };
        let timeout_millis = settings.response_timeout.as_millis() as u32;

        let state = SessionState::new(now_ms(runtime.as_ref()));

        let connection_id = ConnectionId::new(connection_index);
        connection_index += 1;

        let event_handler: ChannelEventHandler<'_, AimdbMqttEvent, MAX_PROPERTIES, CHANNEL_SIZE> =
            ChannelEventHandler::new(connection_id, &events, &state, runtime.as_ref());

        let mut client = ClientNoQueue::new(
            connection,
            &mut mqtt_buffer,
            DelayEmbedded::new(crate::embedded::session::ClientDelay(&delay)),
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
            &delay,
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

        aimdb_core::session::Delay::sleep(&delay, settings.reconnection_delay).await;
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
