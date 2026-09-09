//! Embassy byte sources behind core's runtime-neutral I/O traits, for the
//! transports that need no network stack.
//!
//! A UART is one of core's [`ByteStream`]s, but it borrows nothing from
//! `embassy-net`, so it lives here under `connector-io` rather than in the
//! `net` module under `net` — a board with no networking should not compile
//! `smoltcp` to frame a serial line. `net` enables `connector-io`, so the
//! socket types and these share one feature graph from the caller's side.
//!
//! # Safety invariant
//!
//! As in [`crate::connectors`]: an Embassy executor runs cooperatively on a
//! single core with no preemption or thread migration, so the wrapped `!Send`
//! values are never touched from another thread. The traits declare `+ Send`
//! futures and Embassy's are not, so each impl returns a
//! [`SendFutureWrapper`].

use core::future::Future;

use aimdb_core::session::{ByteStream, TransportError, TransportResult};

use crate::SendFutureWrapper;

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

#[cfg(test)]
mod tests {
    use super::*;
    use aimdb_core::session::{Connection, FrameFault, FramedConnection, Framer};
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
        fn encode(&self, frame: &[u8], out: &mut Vec<u8>) -> Result<(), FrameFault> {
            out.push(frame.len() as u8);
            out.extend_from_slice(frame);
            Ok(())
        }
        fn push_bytes(&mut self, bytes: &[u8]) {
            self.buf.extend_from_slice(bytes);
        }
        fn next_frame(&mut self) -> Option<Result<Vec<u8>, FrameFault>> {
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
}
