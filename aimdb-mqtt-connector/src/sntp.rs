//! SNTP time source for TLS certificate validation.
//!
//! The reference boards have no battery-backed RTC, but checking a
//! certificate's validity window needs the current Unix time. This module
//! keeps one crate-global clock: Unix seconds at the `embassy_time` epoch
//! (boot), written after each SNTP sync and read through [`unix_now`] /
//! [`SntpClock`]. The TLS manager spawns [`run`] alongside its broker loop
//! and holds the first handshake until the first sync lands.

use core::sync::atomic::{AtomicU32, Ordering};

use embassy_net::dns::DnsQueryType;
use embassy_net::udp::{PacketMetadata, UdpSocket};
use embassy_net::{IpEndpoint, Stack};
use embassy_time::{with_timeout, Duration, Instant, Timer};

use crate::sntp_codec;

/// Unix seconds at the `embassy_time` epoch; 0 = not yet synced. `u32` is
/// unambiguous until 2106 and stays a single atomic on Cortex-M (no 64-bit
/// atomics there).
static BOOT_UNIX_SECS: AtomicU32 = AtomicU32::new(0);

/// NTP server port.
const SNTP_PORT: u16 = 123;
/// Local ephemeral-port range for the client socket. smoltcp cannot bind
/// port 0, so "random source port" is randomized here per attempt — a reply
/// must land on the right port *and* echo the request nonce to be accepted.
const LOCAL_PORT_BASE: u16 = 49152;
const LOCAL_PORT_SPAN: u16 = 16384;
/// How long to wait for a server reply before treating the sync as failed.
const REPLY_TIMEOUT: Duration = Duration::from_secs(5);
/// Retry cadence until the first successful sync (TLS is blocked on it).
const RETRY_INTERVAL: Duration = Duration::from_secs(10);
/// Re-sync cadence once synced — bounds clock drift to well under
/// certificate-validity granularity.
const RESYNC_INTERVAL: Duration = Duration::from_secs(3600);

/// Current Unix time in seconds, or `None` before the first SNTP sync.
pub fn unix_now() -> Option<u64> {
    let boot = BOOT_UNIX_SECS.load(Ordering::Relaxed);
    if boot == 0 {
        None
    } else {
        Some(u64::from(boot) + Instant::now().as_secs())
    }
}

/// `embedded-tls` clock over the SNTP-synced time; `None` before the first
/// sync (the TLS manager never handshakes in that state, so certificate
/// validity is always actually checked).
pub struct SntpClock;

impl embedded_tls::TlsClock for SntpClock {
    fn now() -> Option<u64> {
        unix_now()
    }
}

/// Keep the clock synced: query `server` until the first success, then
/// re-sync hourly. Runs forever; spawned by the TLS connector build.
pub(crate) async fn run(stack: Stack<'static>, server: &'static str) -> ! {
    loop {
        match sync_once(stack, server).await {
            // 0 is the "unsynced" sentinel and anything above u32::MAX would
            // truncate (year 2106): store neither — treat the sync as failed
            // rather than poisoning the clock and sleeping an hour on it.
            Ok(unix_secs) => {
                match u32::try_from(unix_secs.saturating_sub(Instant::now().as_secs())) {
                    Ok(boot @ 1..) => {
                        BOOT_UNIX_SECS.store(boot, Ordering::Relaxed);
                        #[cfg(feature = "defmt")]
                        defmt::info!("SNTP: synced, unix time {}", unix_secs);
                        Timer::after(RESYNC_INTERVAL).await;
                    }
                    _ => {
                        #[cfg(feature = "defmt")]
                        defmt::warn!(
                            "SNTP: implausible time {} from {}, retrying",
                            unix_secs,
                            server
                        );
                        Timer::after(RETRY_INTERVAL).await;
                    }
                }
            }
            Err(error) => {
                #[cfg(feature = "defmt")]
                defmt::warn!("SNTP: sync with {} failed: {}, retrying", server, error);
                #[cfg(not(feature = "defmt"))]
                let _ = error;
                Timer::after(RETRY_INTERVAL).await;
            }
        }
    }
}

#[derive(Clone, Copy)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub(crate) enum SntpError {
    /// DNS lookup failed or returned no A record.
    Resolve,
    /// The UDP socket could not be bound or the request not sent.
    Send,
    /// No reply within [`REPLY_TIMEOUT`].
    Timeout,
    /// The reply was not a plausible SNTP server response.
    InvalidReply,
}

/// Best-effort request nonce: the hardware TRNG belongs to the TLS session
/// (injected via `TlsOptions`), so unpredictability comes from the tick
/// counter through a splitmix64 finalizer. Enough to defeat *blind* reply
/// spoofing — an off-path attacker cannot observe when the request fired —
/// while an on-path attacker defeats unauthenticated NTP regardless.
fn request_nonce() -> u64 {
    let mut z = Instant::now()
        .as_ticks()
        .wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

async fn sync_once(stack: Stack<'static>, server: &str) -> Result<u64, SntpError> {
    let addresses = stack
        .dns_query(server, DnsQueryType::A)
        .await
        .map_err(|_| SntpError::Resolve)?;
    let address = *addresses.first().ok_or(SntpError::Resolve)?;
    let server_endpoint = IpEndpoint::new(address, SNTP_PORT);

    let nonce = request_nonce();
    let local_port = LOCAL_PORT_BASE + (nonce >> 48) as u16 % LOCAL_PORT_SPAN;

    let mut rx_meta = [PacketMetadata::EMPTY; 2];
    let mut tx_meta = [PacketMetadata::EMPTY; 2];
    let mut rx_buffer = [0u8; sntp_codec::PACKET_SIZE * 2];
    let mut tx_buffer = [0u8; sntp_codec::PACKET_SIZE * 2];
    let mut socket = UdpSocket::new(
        stack,
        &mut rx_meta,
        &mut rx_buffer,
        &mut tx_meta,
        &mut tx_buffer,
    );
    socket.bind(local_port).map_err(|_| SntpError::Send)?;

    socket
        .send_to(&sntp_codec::request(nonce), server_endpoint)
        .await
        .map_err(|_| SntpError::Send)?;

    let mut reply = [0u8; sntp_codec::PACKET_SIZE];
    let (len, meta) = with_timeout(REPLY_TIMEOUT, socket.recv_from(&mut reply))
        .await
        .map_err(|_| SntpError::Timeout)?
        .map_err(|_| SntpError::InvalidReply)?;

    // Only the queried server may answer; the nonce check inside
    // `parse_reply` covers senders spoofing that source address.
    if meta.endpoint != server_endpoint {
        return Err(SntpError::InvalidReply);
    }

    sntp_codec::parse_reply(&reply[..len], nonce).ok_or(SntpError::InvalidReply)
}
