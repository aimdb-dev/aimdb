//! Embassy TCP transport (feature `embassy-runtime`).
//!
//! This half is client-only for the first TCP PR. `embassy-net` has no
//! `TcpListener`; a server needs an explicit socket/buffer pool design.

use alloc::boxed::Box;
use alloc::vec::Vec;

use aimdb_core::session::aimx::AimxCodec;
use aimdb_core::session::{BoxFut, Connection, Dialer, PeerInfo, TransportError, TransportResult};
use aimdb_embassy_adapter::connectors::{EmbassySessionClient, OneShotCell};
use aimdb_embassy_adapter::SendFutureWrapper;
use embassy_net::tcp::TcpSocket;
use embassy_net::{IpEndpoint, Stack};
use embedded_io_async::Write;

use crate::framing::{encode_frame, FrameAccumulator, HEADER_LEN};
use crate::DEFAULT_SCHEME;

const READ_CHUNK: usize = 256;

/// A framed AimX connection over one `embassy-net` TCP socket.
pub struct TcpConnection {
    socket: TcpSocket<'static>,
    acc: FrameAccumulator,
    peer: PeerInfo,
}

// SAFETY: single-core cooperative Embassy executor; same invariant as
// `aimdb-embassy-adapter::connectors`.
unsafe impl Send for TcpConnection {}

impl TcpConnection {
    /// Wrap an already-connected TCP socket.
    pub fn new(socket: TcpSocket<'static>) -> Self {
        Self {
            socket,
            acc: FrameAccumulator::new(),
            peer: PeerInfo::default(),
        }
    }
}

impl Connection for TcpConnection {
    fn recv(&mut self) -> BoxFut<'_, TransportResult<Option<Vec<u8>>>> {
        Box::pin(SendFutureWrapper(async move {
            loop {
                match self.acc.next_frame() {
                    Some(Ok(frame)) => return Ok(Some(frame)),
                    Some(Err(_)) => return Err(TransportError::Io),
                    None => {}
                }

                let mut chunk = [0u8; READ_CHUNK];
                match self.socket.read(&mut chunk).await {
                    Ok(0) => return Ok(None),
                    Ok(n) => self.acc.push_bytes(&chunk[..n]),
                    Err(_) => return Err(TransportError::Io),
                }
            }
        }))
    }

    fn send<'a>(&'a mut self, frame: &'a [u8]) -> BoxFut<'a, TransportResult<()>> {
        Box::pin(SendFutureWrapper(async move {
            let capacity = HEADER_LEN
                .checked_add(frame.len())
                .ok_or(TransportError::Io)?;
            let mut out = Vec::with_capacity(capacity);
            encode_frame(frame, &mut out).map_err(|_| TransportError::Io)?;
            write_all(&mut self.socket, &out).await?;
            self.socket
                .flush()
                .await
                .map_err(|_| TransportError::Closed)
        }))
    }

    fn peer(&self) -> &PeerInfo {
        &self.peer
    }
}

async fn write_all<W>(writer: &mut W, mut bytes: &[u8]) -> TransportResult<()>
where
    W: Write,
{
    while !bytes.is_empty() {
        let n = writer
            .write(bytes)
            .await
            .map_err(|_| TransportError::Closed)?;
        if n == 0 {
            return Err(TransportError::Closed);
        }
        bytes = &bytes[n..];
    }
    Ok(())
}

/// A one-shot Embassy TCP dialer.
pub struct TcpDialer {
    stack: Stack<'static>,
    endpoint: IpEndpoint,
    rx_buffer: OneShotCell<&'static mut [u8]>,
    tx_buffer: OneShotCell<&'static mut [u8]>,
}

// SAFETY: single-core cooperative Embassy executor; buffers and stack stay on
// the same executor task set.
unsafe impl Send for TcpDialer {}
// SAFETY: same invariant. Interior mutability is provided by `OneShotCell`.
unsafe impl Sync for TcpDialer {}

impl TcpDialer {
    /// Build a one-shot dialer with caller-owned socket buffers.
    pub fn new(
        stack: Stack<'static>,
        endpoint: IpEndpoint,
        rx_buffer: &'static mut [u8],
        tx_buffer: &'static mut [u8],
    ) -> Self {
        Self {
            stack,
            endpoint,
            rx_buffer: OneShotCell::new(rx_buffer),
            tx_buffer: OneShotCell::new(tx_buffer),
        }
    }
}

impl Dialer for TcpDialer {
    fn connect(&self) -> BoxFut<'_, TransportResult<Box<dyn Connection>>> {
        Box::pin(SendFutureWrapper(async move {
            let rx = self.rx_buffer.take().ok_or(TransportError::Io)?;
            let tx = self.tx_buffer.take().ok_or(TransportError::Io)?;
            let mut socket = TcpSocket::new(self.stack, rx, tx);
            socket
                .connect(self.endpoint)
                .await
                .map_err(|_| TransportError::Io)?;
            Ok(Box::new(TcpConnection::new(socket)) as Box<dyn Connection>)
        }))
    }
}

/// Constructs an Embassy session client over TCP.
pub struct TcpClient;

impl TcpClient {
    /// Mirror records to/from an AimX peer over TCP.
    #[allow(clippy::new_ret_no_self)]
    pub fn new(
        stack: Stack<'static>,
        endpoint: IpEndpoint,
        rx_buffer: &'static mut [u8],
        tx_buffer: &'static mut [u8],
    ) -> EmbassySessionClient<TcpDialer, AimxCodec> {
        EmbassySessionClient::new(
            TcpDialer::new(stack, endpoint, rx_buffer, tx_buffer),
            AimxCodec,
        )
        .scheme(DEFAULT_SCHEME)
    }
}
