//! SNTP time source for TLS certificate validation (design 044 §6).
//!
//! The reference boards have no battery-backed RTC, but checking a
//! certificate's validity window needs the current Unix time. This module
//! keeps one crate-global clock: Unix seconds at the `embassy_time` epoch
//! (boot), written after each SNTP sync and read through [`unix_now`] /
//! [`SntpClock`]. The TLS manager spawns [`run`] alongside its broker loop
//! and holds the first handshake until the first sync lands (design 044 D5).

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
/// Fixed local port for the client socket (smoltcp cannot bind port 0).
const LOCAL_PORT: u16 = 55123;
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
            Ok(unix_secs) => {
                let boot = unix_secs.saturating_sub(Instant::now().as_secs());
                BOOT_UNIX_SECS.store(boot as u32, Ordering::Relaxed);
                #[cfg(feature = "defmt")]
                defmt::info!("SNTP: synced, unix time {}", unix_secs);
                Timer::after(RESYNC_INTERVAL).await;
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

async fn sync_once(stack: Stack<'static>, server: &str) -> Result<u64, SntpError> {
    let addresses = stack
        .dns_query(server, DnsQueryType::A)
        .await
        .map_err(|_| SntpError::Resolve)?;
    let address = *addresses.first().ok_or(SntpError::Resolve)?;

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
    socket.bind(LOCAL_PORT).map_err(|_| SntpError::Send)?;

    socket
        .send_to(&sntp_codec::request(), IpEndpoint::new(address, SNTP_PORT))
        .await
        .map_err(|_| SntpError::Send)?;

    let mut reply = [0u8; sntp_codec::PACKET_SIZE];
    let (len, _meta) = with_timeout(REPLY_TIMEOUT, socket.recv_from(&mut reply))
        .await
        .map_err(|_| SntpError::Timeout)?
        .map_err(|_| SntpError::InvalidReply)?;

    sntp_codec::parse_reply(&reply[..len]).ok_or(SntpError::InvalidReply)
}
