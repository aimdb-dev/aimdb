//! The TLS transport for the embedded backend.
//!
//! An `embedded-tls` 1.3 session over the caller's transport, split into halves
//! the session loop reads and writes exactly as it does a plaintext socket.
//! Certificate verification is `rustpki` against the application-embedded root
//! CA, dated by the runtime's wall clock; entropy comes from the injected TRNG
//! ([`TlsOptions::new`]). The dialer resolves the host, so there is no network
//! stack here.

use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::future::Future;
use core::net::IpAddr;

use aimdb_core::session::{ByteRead, ByteStream, ByteWrite, TransportError, TransportResult};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::mutex::Mutex;
use embedded_tls::pki::CertVerifier;
use embedded_tls::{
    Aes128GcmSha256, Certificate, CryptoProvider, CryptoRngCore, TlsConfig, TlsConnection,
    TlsContext, TlsError, TlsReader, TlsVerifier, TlsWriter,
};

use crate::embedded::manager::{MqttEvent, Settings};
use crate::embedded::session_loop::run_session;
use mountain_mqtt::client::ConnectionSettings;
use mountain_mqtt::data::quality_of_service::QualityOfService;
use mountain_mqtt::mqtt_manager::ConnectionId;

/// Room for the server's leaf certificate (DER) inside the verifier — 4 KB
/// covers RSA-4096 leaves with headroom.
const CERT_BUFFER_SIZE: usize = 4096;

/// Minimum TLS record read buffer. A peer may send full-size records (2^14 +
/// overhead) whatever `max_fragment_length` we offer, and `embedded-tls` fails
/// any record larger than the buffer, so `build()` rejects a smaller one.
pub(crate) const READ_BUF_MIN: usize = 16_640;

/// TLS materials for a `mqtts://` broker connection.
///
/// All references are `'static`: the session outlives `build()`, so the buffers
/// and the RNG are owned by the application, which knows the memory budget.
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

/// The socket's two halves behind one handle, so `embedded-tls` can clone a
/// "socket" into its reader and its writer.
///
/// One clone is read while another is written, so the directions take
/// **separate async locks** — `TlsReader` only ever touches `rx`, `TlsWriter`
/// only `tx`, and a guard may be held across the inner `.await`. That holds
/// only while `embedded-tls` never writes from its reader.
///
/// Two tests cover that, and only one of them can see the bet go bad:
///
/// * `a_parked_reader_does_not_hold_up_the_writer` below proves the locks
///   really are separate — but over mock halves, with no `embedded-tls` in the
///   picture, so it would keep passing if a version bump started writing from
///   the read path.
/// * `tls_session::a_slow_puback_over_tls_does_not_block_the_ping` is the one
///   that would catch it: a real `TlsConnection`, split, writing a ping while
///   the reader is parked on a PUBACK the broker is withholding. Check that
///   test still passes after bumping `embedded-tls`.
struct DuplexHandle<'a, Rx, Tx> {
    rx: &'a Mutex<CriticalSectionRawMutex, Rx>,
    tx: &'a Mutex<CriticalSectionRawMutex, Tx>,
}

impl<Rx, Tx> Clone for DuplexHandle<'_, Rx, Tx> {
    fn clone(&self) -> Self {
        Self {
            rx: self.rx,
            tx: self.tx,
        }
    }
}

impl<Rx, Tx> embedded_io_async::ErrorType for DuplexHandle<'_, Rx, Tx> {
    type Error = embedded_io_async::ErrorKind;
}

impl<Rx, Tx> embedded_io_async::Read for DuplexHandle<'_, Rx, Tx>
where
    Rx: ByteRead,
{
    async fn read(&mut self, buf: &mut [u8]) -> Result<usize, Self::Error> {
        self.rx
            .lock()
            .await
            .read(buf)
            .await
            .map_err(|_| embedded_io_async::ErrorKind::Other)
    }
}

impl<Rx, Tx> embedded_io_async::Write for DuplexHandle<'_, Rx, Tx>
where
    Tx: ByteWrite,
{
    async fn write(&mut self, buf: &[u8]) -> Result<usize, Self::Error> {
        self.tx
            .lock()
            .await
            .write_all(buf)
            .await
            .map(|()| buf.len())
            .map_err(|_| embedded_io_async::ErrorKind::Other)
    }

    async fn flush(&mut self) -> Result<(), Self::Error> {
        self.tx
            .lock()
            .await
            .flush()
            .await
            .map_err(|_| embedded_io_async::ErrorKind::Other)
    }
}

/// Asserts that a TLS half's I/O future is `Send`.
///
/// `embedded-tls` holds a `Range<*const u8>` into the record buffer its own
/// future owns exclusively, which makes that future `!Send` by type. The
/// session's three futures are polled as one task, so nothing here is ever
/// touched from two threads at once.
struct AssertSend<F>(F);

// SAFETY: upheld by the single-task argument above.
unsafe impl<F> Send for AssertSend<F> {}

impl<F: Future> Future for AssertSend<F> {
    type Output = F::Output;

    fn poll(
        self: core::pin::Pin<&mut Self>,
        cx: &mut core::task::Context<'_>,
    ) -> core::task::Poll<F::Output> {
        // SAFETY: a transparent projection; `AssertSend` is never moved out of.
        unsafe { self.map_unchecked_mut(|s| &mut s.0) }.poll(cx)
    }
}

/// The TLS session's read half, as the session loop's [`ByteRead`]. Below this
/// pair the session cannot tell a plaintext socket from a record stream.
struct TlsRead<'a, 'b, Rx, Tx>(TlsReader<'a, DuplexHandle<'b, Rx, Tx>, Aes128GcmSha256>);

/// The TLS session's write half, as the session loop's [`ByteWrite`].
struct TlsWrite<'a, 'b, Rx, Tx>(TlsWriter<'a, DuplexHandle<'b, Rx, Tx>, Aes128GcmSha256>);

impl<'a, 'b, Rx, Tx> ByteRead for TlsRead<'a, 'b, Rx, Tx>
where
    // The socket handle outlives the TLS session borrowed from it.
    'b: 'a,
    Rx: ByteRead + Send + 'b,
    Tx: ByteWrite + Send + 'b,
{
    fn read<'r>(
        &'r mut self,
        buf: &'r mut [u8],
    ) -> impl Future<Output = TransportResult<usize>> + Send + 'r {
        AssertSend(async move {
            use embedded_io_async::Read as _;
            self.0.read(buf).await.map_err(|_| TransportError::Io)
        })
    }
}

impl<'a, 'b, Rx, Tx> ByteWrite for TlsWrite<'a, 'b, Rx, Tx>
where
    'b: 'a,
    Rx: ByteRead + Send + 'b,
    Tx: ByteWrite + Send + 'b,
{
    fn write_all<'w>(
        &'w mut self,
        buf: &'w [u8],
    ) -> impl Future<Output = TransportResult<()>> + Send + 'w {
        AssertSend(async move {
            use embedded_io_async::Write as _;
            self.0
                .write_all(buf)
                .await
                .map_err(|_| TransportError::Closed)
        })
    }

    fn flush(&mut self) -> impl Future<Output = TransportResult<()>> + Send + '_ {
        AssertSend(async move {
            use embedded_io_async::Write as _;
            self.0.flush().await.map_err(|_| TransportError::Closed)
        })
    }
}

/// Unix seconds for certificate validity, refreshed before each handshake.
///
/// A global because `embedded_tls::TlsClock::now` is a static method. Fed by
/// the runtime's wall clock, or by the SNTP task on an MCU with no RTC.
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
/// verification (time from [`WallClock`]). No client-certificate signing: the
/// mesh authenticates with MQTT credentials.
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
{
    let TlsOptions {
        rng,
        ca_der,
        read_buf,
        write_buf,
        ..
    } = options;

    // Re-subscribed by the session on every (re)connection, so inbound
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

        let mut stream = match dialer.connect(&host, port).await {
            Ok(stream) => stream,
            Err(_e) => {
                #[cfg(feature = "defmt")]
                defmt::warn!("MQTT-TLS: connect failed, will retry");
                aimdb_core::session::Delay::sleep(&delay, settings.reconnection_delay).await;
                continue;
            }
        };

        // One lock per direction, so the TLS reader and writer never contend.
        let (rx, tx) = stream.split();
        let rx = Mutex::new(rx);
        let tx = Mutex::new(tx);
        let handle = DuplexHandle { rx: &rx, tx: &tx };

        let tls_config = TlsConfig::new().with_server_name(&host);
        let mut tls = TlsConnection::new(handle.clone(), &mut *read_buf, &mut *write_buf);
        let provider = TrngProvider {
            rng: &mut *rng,
            verifier: CertVerifier::new(Certificate::X509(ca_der)),
        };
        // The handshake reads and writes sequentially through one connection,
        // so it needs no split and takes neither lock twice.
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

        let connection_id = ConnectionId::new(connection_index);
        connection_index += 1;

        // From here the session is the plain path's, byte for byte: the record
        // layer is just another pair of halves.
        let (tls_rx, tls_tx) = tls.split();
        let error = run_session(
            connection_id,
            TlsRead(tls_rx),
            TlsWrite(tls_tx),
            &connection_settings,
            &subscribe_topics,
            &events,
            &actions,
            &settings,
            &delay,
            runtime.as_ref(),
        )
        .await;

        #[cfg(feature = "defmt")]
        defmt::warn!("MQTT-TLS: session errored: {:?}", error);
        events
            .send(MqttEvent::Disconnected {
                connection_id,
                error,
            })
            .await;

        aimdb_core::session::Delay::sleep(&delay, settings.reconnection_delay).await;
    }
}

/// Parse the broker host as an IP literal, brackets optional (`[::1]`).
///
/// `build()` vets `mqtts://` hosts with this: an IPv4 literal can still match
/// through `rustpki`'s CN fallback, so it is allowed with a warning, while an
/// IPv6 literal never can (the hostname charset has no `:`) and is rejected.
pub(crate) fn host_ip_literal(host: &str) -> Option<IpAddr> {
    let host = host
        .strip_prefix('[')
        .and_then(|h| h.strip_suffix(']'))
        .unwrap_or(host);
    host.parse::<IpAddr>().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::pin::pin;
    use core::task::{Context, Poll};

    /// A read half that never completes, so the reader's lock stays held.
    struct PendingRead;

    impl ByteRead for PendingRead {
        async fn read(&mut self, _buf: &mut [u8]) -> TransportResult<usize> {
            core::future::pending().await
        }
    }

    /// A write half that completes immediately, recording what it was given.
    struct RecordingWrite(Vec<u8>);

    impl ByteWrite for RecordingWrite {
        async fn write_all(&mut self, buf: &[u8]) -> TransportResult<()> {
            self.0.extend_from_slice(buf);
            Ok(())
        }

        async fn flush(&mut self) -> TransportResult<()> {
            Ok(())
        }
    }

    /// A read parked inside one clone of the handle must not hold up a write
    /// through another.
    ///
    /// Mock halves, so this is about [`DuplexHandle`]'s own locking and nothing
    /// else: it cannot tell you whether `embedded-tls` still reads and writes
    /// from the halves it was given. `tls_session::a_slow_puback_over_tls_does_not_block_the_ping`
    /// is the test that does.
    #[test]
    fn a_parked_reader_does_not_hold_up_the_writer() {
        let rx = Mutex::new(PendingRead);
        let tx = Mutex::new(RecordingWrite(Vec::new()));
        let handle = DuplexHandle { rx: &rx, tx: &tx };

        // What `TlsConnection::split` does: a clone apiece.
        let mut reader = handle.clone();
        let mut writer = handle;

        let mut cx = Context::from_waker(core::task::Waker::noop());

        let mut buf = [0u8; 4];
        let mut read = pin!(embedded_io_async::Read::read(&mut reader, &mut buf));
        assert!(
            matches!(read.as_mut().poll(&mut cx), Poll::Pending),
            "the read must park — otherwise this test proves nothing"
        );

        let mut write = pin!(embedded_io_async::Write::write(&mut writer, b"ping"));
        assert!(
            matches!(write.as_mut().poll(&mut cx), Poll::Ready(Ok(4))),
            "the write must complete while the read is parked"
        );

        assert!(
            matches!(read.as_mut().poll(&mut cx), Poll::Pending),
            "and the reader must be undisturbed by it"
        );
    }
}
