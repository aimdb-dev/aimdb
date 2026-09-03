//! Embassy implementations of core's runtime-neutral I/O traits: the adapter
//! owns sockets and clocks, the connector owns framing and protocol.
//!
//! The traits declare `+ Send` futures and Embassy's are not, so every impl
//! here returns a [`SendFutureWrapper`]. That force-`Send` lives here, once,
//! and connector crates carry none.
//!
//! # Safety invariant
//!
//! As in [`crate::connectors`]: an Embassy executor runs cooperatively on a
//! single core with no preemption or thread migration, so the wrapped `!Send`
//! values are never touched from another thread.

use core::cell::RefCell;
use core::future::{poll_fn, Future};
use core::task::{Context, Poll, Waker};

use alloc::sync::Arc;

use aimdb_core::session::{ByteStream, StreamDialer, TransportError, TransportResult};

use embassy_net::tcp::TcpSocket;
use embassy_net::{IpEndpoint, Stack};
use embedded_io_async::Write as _;

use crate::SendFutureWrapper;

// ===========================================================================
// Socket slot.
// ===========================================================================

/// Holds one reusable `embassy-net` TCP socket between uses, so the caller's
/// buffers are allocated once and a dropped [`EmbassyTcpStream`] can hand its
/// socket back.
pub struct TcpSocketSlot {
    socket: RefCell<Option<TcpSocket<'static>>>,
    // One `Waker` is enough: at most one caller waits on a given slot.
    waker: RefCell<Option<Waker>>,
}

// SAFETY: single-core cooperative Embassy executor — see the module invariant.
unsafe impl Send for TcpSocketSlot {}
// SAFETY: same invariant; shared only so a dropped stream can return its socket.
unsafe impl Sync for TcpSocketSlot {}

impl TcpSocketSlot {
    /// Hold `socket` for the next taker.
    pub fn new(socket: TcpSocket<'static>) -> Self {
        Self {
            socket: RefCell::new(Some(socket)),
            waker: RefCell::new(None),
        }
    }

    /// Take the socket if it is free right now.
    pub fn take(&self) -> Option<TcpSocket<'static>> {
        self.socket.borrow_mut().take()
    }

    /// Wait until the socket is free, then take it.
    pub async fn acquire(&self) -> TcpSocket<'static> {
        poll_fn(|cx| self.poll_take(cx)).await
    }

    fn poll_take(&self, cx: &mut Context<'_>) -> Poll<TcpSocket<'static>> {
        let mut slot = self.socket.borrow_mut();
        if let Some(socket) = slot.take() {
            Poll::Ready(socket)
        } else {
            drop(slot);
            *self.waker.borrow_mut() = Some(cx.waker().clone());
            Poll::Pending
        }
    }

    /// Return the socket to the slot, waking whoever is waiting for it.
    pub fn put(&self, socket: TcpSocket<'static>) {
        let mut slot = self.socket.borrow_mut();
        debug_assert!(slot.is_none(), "Embassy TCP socket returned twice");
        if slot.is_none() {
            *slot = Some(socket);
        }
        if let Some(waker) = self.waker.borrow_mut().take() {
            waker.wake();
        }
    }
}

// ===========================================================================
// TCP.
// ===========================================================================

/// One `embassy-net` TCP connection as a [`ByteStream`]. Owns its socket and
/// returns it to its slot on drop.
pub struct EmbassyTcpStream {
    socket: Option<TcpSocket<'static>>,
    recycler: Option<Arc<TcpSocketSlot>>,
}

// SAFETY: single-core cooperative Embassy executor — see the module invariant.
unsafe impl Send for EmbassyTcpStream {}

impl EmbassyTcpStream {
    pub(crate) fn recyclable(socket: TcpSocket<'static>, recycler: Arc<TcpSocketSlot>) -> Self {
        Self {
            socket: Some(socket),
            recycler: Some(recycler),
        }
    }

    fn socket_mut(&mut self) -> TransportResult<&mut TcpSocket<'static>> {
        self.socket.as_mut().ok_or(TransportError::Closed)
    }
}

impl Drop for EmbassyTcpStream {
    fn drop(&mut self) {
        if let Some(mut socket) = self.socket.take() {
            // Reset now rather than leave the link half-open; the next taker
            // aborts again before reuse.
            socket.abort();
            if let Some(recycler) = &self.recycler {
                recycler.put(socket);
            }
        }
    }
}

impl ByteStream for EmbassyTcpStream {
    fn read<'a>(
        &'a mut self,
        buf: &'a mut [u8],
    ) -> impl Future<Output = TransportResult<usize>> + Send + 'a {
        SendFutureWrapper(async move {
            let socket = self.socket_mut()?;
            socket.read(buf).await.map_err(|_| TransportError::Io)
        })
    }

    fn write_all<'a>(
        &'a mut self,
        buf: &'a [u8],
    ) -> impl Future<Output = TransportResult<()>> + Send + 'a {
        SendFutureWrapper(async move {
            let socket = self.socket_mut()?;
            socket
                .write_all(buf)
                .await
                .map_err(|_| TransportError::Closed)
        })
    }

    fn flush(&mut self) -> impl Future<Output = TransportResult<()>> + Send + '_ {
        SendFutureWrapper(async move {
            let socket = self.socket_mut()?;
            socket.flush().await.map_err(|_| TransportError::Closed)
        })
    }
}

/// Dials TCP connections over one caller-owned socket.
pub struct EmbassyTcpDialer {
    slot: Arc<TcpSocketSlot>,
}

impl StreamDialer for EmbassyTcpDialer {
    type Stream = EmbassyTcpStream;

    fn connect<'a>(
        &'a self,
        host: &'a str,
        port: u16,
    ) -> impl Future<Output = TransportResult<Self::Stream>> + Send + 'a {
        SendFutureWrapper(async move {
            // Resolution belongs to the adapter: IP literals here, hostnames
            // once embassy-net's `dns` feature is on.
            let addr: core::net::IpAddr = host.parse().map_err(|_| TransportError::Io)?;
            let endpoint = IpEndpoint::new(addr.into(), port);

            let Some(mut socket) = self.slot.take() else {
                return Err(TransportError::Io);
            };
            socket.abort();
            match socket.connect(endpoint).await {
                Ok(()) => Ok(EmbassyTcpStream::recyclable(socket, self.slot.clone())),
                Err(_) => {
                    socket.abort();
                    self.slot.put(socket);
                    Err(TransportError::Io)
                }
            }
        })
    }
}

// ===========================================================================
// UART.
// ===========================================================================

/// A UART, or any `embedded-io-async` read/write pair, as one [`ByteStream`] —
/// so the connector names no `embedded-io-async` types of its own.
pub struct EmbassyUart<Rd, Wr> {
    rx: Rd,
    tx: Wr,
}

// SAFETY: single-core cooperative Embassy executor — see the module invariant.
unsafe impl<Rd, Wr> Send for EmbassyUart<Rd, Wr> {}

impl<Rd, Wr> EmbassyUart<Rd, Wr> {
    /// Present an already-split UART's halves as one stream.
    pub fn new(rx: Rd, tx: Wr) -> Self {
        Self { rx, tx }
    }
}

impl<Rd, Wr> ByteStream for EmbassyUart<Rd, Wr>
where
    Rd: embedded_io_async::Read,
    Wr: embedded_io_async::Write,
{
    fn read<'a>(
        &'a mut self,
        buf: &'a mut [u8],
    ) -> impl Future<Output = TransportResult<usize>> + Send + 'a {
        SendFutureWrapper(async move { self.rx.read(buf).await.map_err(|_| TransportError::Io) })
    }

    fn write_all<'a>(
        &'a mut self,
        buf: &'a [u8],
    ) -> impl Future<Output = TransportResult<()>> + Send + 'a {
        SendFutureWrapper(async move {
            self.tx
                .write_all(buf)
                .await
                .map_err(|_| TransportError::Closed)
        })
    }

    fn flush(&mut self) -> impl Future<Output = TransportResult<()>> + Send + '_ {
        SendFutureWrapper(async move { self.tx.flush().await.map_err(|_| TransportError::Closed) })
    }
}

// ===========================================================================
// Constructors and clock.
// ===========================================================================

/// Entry point for the Embassy transports, keeping `embassy_net::Stack` inside
/// the adapter.
pub struct EmbassyNet;

impl EmbassyNet {
    /// A reusable TCP dialer over caller-owned socket buffers.
    pub fn tcp(
        stack: Stack<'static>,
        rx_buffer: &'static mut [u8],
        tx_buffer: &'static mut [u8],
    ) -> EmbassyTcpDialer {
        EmbassyTcpDialer {
            slot: Arc::new(TcpSocketSlot::new(TcpSocket::new(
                stack, rx_buffer, tx_buffer,
            ))),
        }
    }
}

/// [`Delay`](aimdb_core::session::Delay) over `embassy_time::Timer`, which is
/// `Send` and allocates nothing.
///
/// Gated on `embassy-time` separately from `net`, so a sockets-only consumer
/// does not pull in `defmt-timestamp-uptime`'s `_defmt_timestamp` symbol.
#[cfg(feature = "embassy-time")]
#[derive(Clone, Copy, Default)]
pub struct EmbassyDelay;

#[cfg(feature = "embassy-time")]
impl aimdb_core::session::Delay for EmbassyDelay {
    fn sleep(&self, d: core::time::Duration) -> impl Future<Output = ()> + Send {
        embassy_time::Timer::after(embassy_time::Duration::from_micros(d.as_micros() as u64))
    }
}

// ===========================================================================
// Compile-time assertions — a regression in the force-`Send` above must
// surface here, not in a connector.
// ===========================================================================

#[allow(dead_code)]
fn _transports_are_send() {
    fn assert_send<T: Send>() {}
    assert_send::<EmbassyTcpStream>();
    assert_send::<TcpSocketSlot>();
    #[cfg(feature = "embassy-time")]
    assert_send::<EmbassyDelay>();
}

/// A task built from these transports boxes as `ConnectorBuilder::build`
/// requires — the boundary the force-`Send` exists to cross.
#[allow(dead_code)]
fn _dialed_stream_drives_a_boxed_send_task<D>(dialer: D) -> aimdb_core::session::BoxFut<'static, ()>
where
    D: StreamDialer + Send + 'static,
{
    alloc::boxed::Box::pin(async move {
        let mut buf = [0u8; 8];
        if let Ok(mut stream) = dialer.connect("127.0.0.1", 7001).await {
            let _ = stream.read(&mut buf).await;
            let _ = stream.write_all(&buf).await;
            let _ = stream.flush().await;
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use aimdb_core::session::{Connection, FramedConnection, Framer};
    use alloc::vec;
    use alloc::vec::Vec;

    // `host_test_stubs!()` must expand once per binary; `buffer` already does
    // it for this one.

    /// An in-memory read half: hands out queued chunks, then EOF.
    struct MockRx(Vec<Vec<u8>>);

    impl embedded_io_async::ErrorType for MockRx {
        type Error = embedded_io_async::ErrorKind;
    }

    impl embedded_io_async::Read for MockRx {
        async fn read(&mut self, buf: &mut [u8]) -> Result<usize, Self::Error> {
            if self.0.is_empty() {
                return Ok(0);
            }
            let chunk = self.0.remove(0);
            let n = chunk.len().min(buf.len());
            buf[..n].copy_from_slice(&chunk[..n]);
            Ok(n)
        }
    }

    /// An in-memory write half recording what it was given.
    #[derive(Default)]
    struct MockTx {
        written: Vec<u8>,
        flushes: usize,
    }

    impl embedded_io_async::ErrorType for MockTx {
        type Error = embedded_io_async::ErrorKind;
    }

    impl embedded_io_async::Write for MockTx {
        async fn write(&mut self, buf: &[u8]) -> Result<usize, Self::Error> {
            self.written.extend_from_slice(buf);
            Ok(buf.len())
        }
        async fn flush(&mut self) -> Result<(), Self::Error> {
            self.flushes += 1;
            Ok(())
        }
    }

    /// Length-prefixed framer, enough to drive a `FramedConnection`.
    #[derive(Default)]
    struct LenFramer {
        buf: Vec<u8>,
    }

    impl Framer for LenFramer {
        fn encode(&self, frame: &[u8], out: &mut Vec<u8>) {
            out.push(frame.len() as u8);
            out.extend_from_slice(frame);
        }
        fn push_bytes(&mut self, bytes: &[u8]) {
            self.buf.extend_from_slice(bytes);
        }
        fn next_frame(&mut self) -> Option<Result<Vec<u8>, ()>> {
            let len = *self.buf.first()? as usize;
            if self.buf.len() < len + 1 {
                return None;
            }
            let frame = self.buf[1..len + 1].to_vec();
            self.buf.drain(..len + 1);
            Some(Ok(frame))
        }
    }

    fn block_on<F: Future>(f: F) -> F::Output {
        futures::executor::block_on(f)
    }

    #[test]
    fn uart_reads_queued_chunks_then_reports_eof() {
        let mut uart = EmbassyUart::new(MockRx(vec![b"hi".to_vec()]), MockTx::default());
        block_on(async {
            let mut buf = [0u8; 8];
            assert_eq!(uart.read(&mut buf).await.unwrap(), 2);
            assert_eq!(&buf[..2], b"hi");
            assert_eq!(uart.read(&mut buf).await.unwrap(), 0, "EOF is Ok(0)");
        });
    }

    #[test]
    fn uart_writes_every_byte_and_flushes() {
        let mut uart = EmbassyUart::new(MockRx(vec![]), MockTx::default());
        block_on(async {
            uart.write_all(b"payload").await.unwrap();
            uart.flush().await.unwrap();
        });
        assert_eq!(uart.tx.written, b"payload");
        assert_eq!(uart.tx.flushes, 1);
    }

    /// The UART drives core's framed connection, which is what the serial
    /// connector rides.
    #[test]
    fn uart_drives_a_framed_connection() {
        let rx = MockRx(vec![vec![2, b'h', b'i'], vec![3, b'y', b'e', b's']]);
        let mut conn: FramedConnection<_, LenFramer, 64, 64> = FramedConnection::new(
            EmbassyUart::new(rx, MockTx::default()),
            LenFramer::default(),
        );

        block_on(async {
            assert_eq!(conn.recv().await.unwrap(), Some(b"hi".to_vec()));
            assert_eq!(conn.recv().await.unwrap(), Some(b"yes".to_vec()));
            assert_eq!(conn.recv().await.unwrap(), None);
            conn.send(b"ack").await.unwrap();
        });
    }

    /// The force-`Send` has to survive as far as the runner's boxed
    /// `dyn Connection`.
    #[test]
    fn a_uart_connection_is_boxable_as_a_send_dyn_connection() {
        let conn: FramedConnection<_, LenFramer, 64, 64> = FramedConnection::new(
            EmbassyUart::new(MockRx(vec![]), MockTx::default()),
            LenFramer::default(),
        );
        let _boxed: alloc::boxed::Box<dyn Connection> = alloc::boxed::Box::new(conn);
    }

    /// The host time driver pins the clock at 0, so only an already-expired
    /// sleep can be driven here.
    #[test]
    fn delay_completes_an_already_expired_sleep() {
        use aimdb_core::session::Delay;
        block_on(EmbassyDelay.sleep(core::time::Duration::ZERO));
    }
}
