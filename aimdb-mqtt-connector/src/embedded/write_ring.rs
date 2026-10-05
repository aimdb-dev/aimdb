//! The session's write queue: one byte ring, allocated once per connector.
//!
//! The session loop encodes every packet straight into a grant of the ring;
//! [`WriteRing::write_out`] sends committed bytes to the socket and releases
//! them. Both ends run in the same task under the session's `select3`, so the
//! ring needs no lock. Two wakers carry the signals the ring's own notifier
//! does not: `data` from a commit to `write_out`, and `room` from a release to
//! whatever waits for space.

use core::future::poll_fn;
use core::task::Poll;

use aimdb_core::session::ByteWrite;
use bbqueue::traits::coordination::cas::AtomicCoord;
use bbqueue::traits::notifier::polling::Polling;
use bbqueue::traits::storage::BoxedSlice;
use bbqueue::BBQueue;
use embassy_sync::waitqueue::AtomicWaker;
use mountain_mqtt::client::ClientError;
use mountain_mqtt::codec::mqtt_writer::{MqttBufWriter, MqttLenWriter, MqttWriter};
use mountain_mqtt::codec::write::Write;
use mountain_mqtt::error::PacketWriteError;

use crate::embedded::manager::Error;

/// Default ring size, the same as the inbound side's `BUFFER_SIZE`.
pub(crate) const DEFAULT_WRITE_BUFFER: usize = 4096;

/// Bytes kept free behind a PUBLISH, so the control packet after it (a
/// PUBACK, a ping) still finds room.
pub(crate) const CONTROL_RESERVE: usize = 64;

/// One connector's write ring.
pub(crate) struct WriteRing {
    queue: BBQueue<BoxedSlice, AtomicCoord, Polling>,
    data: AtomicWaker,
    room: AtomicWaker,
}

impl WriteRing {
    pub(crate) fn new(capacity: usize) -> Self {
        Self {
            queue: BBQueue::new_with_storage(BoxedSlice::new(capacity)),
            data: AtomicWaker::new(),
            room: AtomicWaker::new(),
        }
    }

    /// The largest PUBLISH frame the ring always has room for eventually.
    pub(crate) fn max_publish(&self) -> usize {
        (self.queue.capacity() / 2).saturating_sub(CONTROL_RESERVE)
    }

    /// Whether a `len`-byte frame plus `reserve` always fits eventually.
    ///
    /// A bipbuffer grants only contiguous space: once its pointers have both
    /// moved to `k`, an empty ring grants at most `max(capacity − k, k − 1)`
    /// bytes, about half its capacity in the worst case. A larger grant could
    /// wait forever, even with nothing queued.
    pub(crate) fn fits(&self, len: usize, reserve: usize) -> bool {
        fits_ring(self.queue.capacity(), len, reserve)
    }

    /// Whether a contiguous grant of `n` bytes exists right now.
    ///
    /// bbqueue has no free-space query, so this takes the grant and drops it
    /// uncommitted. A probe that wraps commits an early wraparound (the
    /// unused tail is skipped until the reader passes it); no data changes.
    pub(crate) fn has_room(&self, n: usize) -> bool {
        self.queue.stream_producer().grant_exact(n).is_ok()
    }

    /// [`has_room`](Self::has_room), registering `waker` for the next release
    /// when there is none.
    pub(crate) fn poll_room(&self, n: usize, waker: &core::task::Waker) -> bool {
        if self.has_room(n) {
            return true;
        }
        self.room.register(waker);
        // A release between the probe and the registration woke nobody.
        self.has_room(n)
    }

    /// Wait until a contiguous grant of `n` bytes exists.
    pub(crate) async fn wait_room(&self, n: usize) {
        poll_fn(|cx| {
            if self.poll_room(n, cx.waker()) {
                Poll::Ready(())
            } else {
                Poll::Pending
            }
        })
        .await
    }

    /// Encode `packet` into the ring, waiting for room. `reserve` more bytes
    /// are granted behind it and left free. A packet that does not
    /// [`fit`](Self::fits) fails with `Overflow` instead of waiting forever.
    ///
    /// Waiting cannot deadlock: [`write_out`](Self::write_out) is the ring's
    /// only consumer and a sibling arm of the session's `select3`, so parking
    /// here is what lets it run. It is also the backpressure: a peer that
    /// stops reading stops the session encoding.
    pub(crate) async fn put<P: Write>(&self, packet: &P, reserve: usize) -> Result<(), Error> {
        let len = encoded_len(packet)?;
        if !self.fits(len, reserve) {
            return Err(write_error(PacketWriteError::Overflow));
        }
        self.put_sized(packet, len, reserve).await
    }

    /// [`put`](Self::put) for a packet whose encoded length is already known
    /// and [fits](Self::fits).
    pub(crate) async fn put_sized<P: Write>(
        &self,
        packet: &P,
        len: usize,
        reserve: usize,
    ) -> Result<(), Error> {
        debug_assert!(self.fits(len, reserve));
        let producer = self.queue.stream_producer();
        let mut grant = poll_fn(|cx| match producer.grant_exact(len + reserve) {
            Ok(grant) => Poll::Ready(grant),
            Err(_) => {
                self.room.register(cx.waker());
                match producer.grant_exact(len + reserve) {
                    Ok(grant) => Poll::Ready(grant),
                    Err(_) => Poll::Pending,
                }
            }
        })
        .await;
        let mut writer = MqttBufWriter::new(&mut grant[..len]);
        writer.put(packet).map_err(write_error)?;
        grant.commit(len);
        self.data.wake();
        Ok(())
    }

    /// Encode `packet` only if there is room now. `Ok(false)`: dropped.
    pub(crate) fn try_put<P: Write>(&self, packet: &P) -> Result<bool, Error> {
        let len = encoded_len(packet)?;
        let Ok(mut grant) = self.queue.stream_producer().grant_exact(len) else {
            return Ok(false);
        };
        let mut writer = MqttBufWriter::new(&mut grant[..len]);
        writer.put(packet).map_err(write_error)?;
        grant.commit(len);
        self.data.wake();
        Ok(true)
    }

    /// Discard every byte still queued, so nothing from an old session
    /// reaches a new socket ahead of its CONNECT.
    pub(crate) fn drain(&self) {
        let consumer = self.queue.stream_consumer();
        // Two reads at most: the tail before a wraparound, then the head.
        while let Ok(grant) = consumer.read() {
            let n = grant.len();
            grant.release(n);
        }
        self.room.wake();
    }

    /// Send committed bytes to the socket, releasing them once written and
    /// flushed. Returns only on a write error. Never cancelled mid-write:
    /// it is a `select3` arm that ends the session when it returns.
    pub(crate) async fn write_out<W: ByteWrite>(&self, mut tx: W) -> Error {
        let consumer = self.queue.stream_consumer();
        loop {
            let grant = poll_fn(|cx| match consumer.read() {
                Ok(grant) => Poll::Ready(grant),
                Err(_) => {
                    self.data.register(cx.waker());
                    match consumer.read() {
                        Ok(grant) => Poll::Ready(grant),
                        Err(_) => Poll::Pending,
                    }
                }
            })
            .await;
            if tx.write_all(&grant).await.is_err() || tx.flush().await.is_err() {
                return Error::Client(ClientError::PacketWrite(PacketWriteError::ConnectionSend));
            }
            let n = grant.len();
            grant.release(n);
            self.room.wake();
        }
    }
}

/// Whether a `len`-byte frame plus `reserve` always fits eventually in a
/// `capacity`-byte ring; see [`WriteRing::fits`].
pub(crate) fn fits_ring(capacity: usize, len: usize, reserve: usize) -> bool {
    len + reserve <= capacity / 2
}

/// Bytes `packet` encodes to.
pub(crate) fn encoded_len<P: Write>(packet: &P) -> Result<usize, Error> {
    let mut len_writer = MqttLenWriter::new();
    len_writer.put(packet).map_err(write_error)?;
    Ok(len_writer.position())
}

fn write_error(error: PacketWriteError) -> Error {
    Error::Client(ClientError::PacketWrite(error))
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::sync::Arc;
    use alloc::task::Wake;
    use alloc::vec::Vec;
    use core::cell::RefCell;

    use core::future::Future;
    use core::pin::pin;
    use core::sync::atomic::{AtomicUsize, Ordering};
    use core::task::{Context, Waker};
    use embassy_sync::blocking_mutex::CriticalSectionMutex;

    use mountain_mqtt::codec::mqtt_writer::MqttWriter;

    /// A "packet" of `n` copies of one byte.
    struct Bytes(usize, u8);

    impl Write for Bytes {
        fn write<'a, W: MqttWriter<'a>>(&self, writer: &mut W) -> Result<(), PacketWriteError> {
            writer.put_slice(&alloc::vec![self.1; self.0])
        }
    }

    /// Records everything written.
    #[derive(Clone)]
    struct Sink(Arc<CriticalSectionMutex<RefCell<Vec<u8>>>>);

    impl Sink {
        fn new() -> Self {
            Self(Arc::new(CriticalSectionMutex::new(
                RefCell::new(Vec::new()),
            )))
        }

        fn written(&self) -> Vec<u8> {
            self.0.lock(|w| w.borrow().clone())
        }
    }

    impl ByteWrite for Sink {
        async fn write_all(&mut self, buf: &[u8]) -> aimdb_core::session::TransportResult<()> {
            self.0.lock(|w| w.borrow_mut().extend_from_slice(buf));
            Ok(())
        }

        async fn flush(&mut self) -> aimdb_core::session::TransportResult<()> {
            Ok(())
        }
    }

    #[derive(Default)]
    struct Count(AtomicUsize);

    impl Wake for Count {
        fn wake(self: Arc<Self>) {
            self.0.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Poll a future once with `waker`.
    fn poll_once<F: Future>(f: core::pin::Pin<&mut F>, waker: &Waker) -> Poll<F::Output> {
        f.poll(&mut Context::from_waker(waker))
    }

    /// Everything committed, in read order, released as it is read.
    fn read_all(ring: &WriteRing) -> Vec<u8> {
        let consumer = ring.queue.stream_consumer();
        let mut out = Vec::new();
        while let Ok(grant) = consumer.read() {
            out.extend_from_slice(&grant);
            let n = grant.len();
            grant.release(n);
        }
        out
    }

    /// Write `n` bytes and read them back, moving both pointers to `n`.
    fn advance(ring: &WriteRing, n: usize) {
        if n > 0 {
            assert!(ring.try_put(&Bytes(n, 0)).unwrap());
            assert_eq!(read_all(ring).len(), n);
        }
    }

    fn ready(f: impl Future<Output = Result<(), Error>>) {
        let f = pin!(f);
        assert!(matches!(poll_once(f, Waker::noop()), Poll::Ready(Ok(()))));
    }

    #[test]
    fn a_probe_that_wraps_changes_no_data() {
        let ring = WriteRing::new(16);
        advance(&ring, 6);
        ready(ring.put(&Bytes(6, b'b'), 0));
        // 12..16 is too short for 5 bytes; the probe wraps to the start.
        assert!(ring.has_room(5));
        ready(ring.put(&Bytes(3, b'c'), 0));
        assert_eq!(read_all(&ring), b"bbbbbbccc");
    }

    #[test]
    fn an_empty_ring_always_grants_half_its_capacity() {
        const CAPACITY: usize = 64;
        for offset in 0..CAPACITY {
            let ring = WriteRing::new(CAPACITY);
            advance(&ring, offset);
            assert!(ring.has_room(CAPACITY / 2), "offset {offset}");
        }
        // Drained at the middle, one byte more than half never fits.
        let ring = WriteRing::new(CAPACITY);
        advance(&ring, CAPACITY / 2);
        assert!(!ring.has_room(CAPACITY / 2 + 1));
    }

    #[test]
    fn max_publish_leaves_room_for_the_reserve_in_half_the_ring() {
        let ring = WriteRing::new(DEFAULT_WRITE_BUFFER);
        assert_eq!(ring.max_publish(), 1984);
        assert_eq!(
            ring.max_publish() + CONTROL_RESERVE,
            DEFAULT_WRITE_BUFFER / 2
        );
    }

    #[test]
    fn drain_discards_an_old_sessions_bytes() {
        let ring = WriteRing::new(64);
        ready(ring.put(&Bytes(10, b'a'), 0));
        ring.drain();
        ready(ring.put(&Bytes(4, b'b'), 0));
        assert_eq!(read_all(&ring), b"bbbb");
    }

    #[test]
    fn a_puback_waits_for_room_and_goes_out_in_order() {
        let ring = WriteRing::new(256);
        // Filled with `try_put`: 200 bytes is more than `put` accepts.
        assert!(ring.try_put(&Bytes(200, b'p')).unwrap());
        assert!(ring.try_put(&Bytes(40, b'q')).unwrap());

        // 16 bytes left, and nothing has been read: the PUBACK waits.
        let woken = Arc::new(Count::default());
        let waker = Waker::from(woken.clone());
        let mut puback = pin!(ring.put(&Bytes(20, b'r'), 0));
        assert!(poll_once(puback.as_mut(), &waker).is_pending());

        // `write_out` sends what is queued and releases it.
        let sink = Sink::new();
        let mut write_out = pin!(ring.write_out(sink.clone()));
        assert!(poll_once(write_out.as_mut(), Waker::noop()).is_pending());
        assert_eq!(sink.written().len(), 240);
        assert!(
            woken.0.load(Ordering::Relaxed) > 0,
            "the release woke the PUBACK"
        );

        assert!(matches!(
            poll_once(puback.as_mut(), &waker),
            Poll::Ready(Ok(()))
        ));
        assert!(poll_once(write_out.as_mut(), Waker::noop()).is_pending());
        let written = sink.written();
        assert_eq!(written.len(), 260);
        assert!(written[..200].iter().all(|&b| b == b'p'));
        assert!(written[200..240].iter().all(|&b| b == b'q'));
        assert!(written[240..].iter().all(|&b| b == b'r'));
    }

    #[test]
    fn try_put_drops_rather_than_waits() {
        let ring = WriteRing::new(16);
        assert!(ring.try_put(&Bytes(14, b'a')).unwrap());
        assert!(!ring.try_put(&Bytes(4, b'p')).unwrap());
        assert_eq!(read_all(&ring).len(), 14);
    }
}

#[cfg(test)]
mod proofs {
    //! A control packet larger than the ring can always grant fails instead
    //! of parking forever; one that fits goes out from any offset.
    use super::*;
    use core::future::Future;
    use core::pin::pin;
    use core::task::{Context, Waker};
    use mountain_mqtt::packets::connect::Connect;

    fn connect_with_password(password: &[u8]) -> Connect<'_, 1, 0> {
        Connect::new(
            60,
            Some("user"),
            Some(password),
            "proof",
            true,
            None,
            heapless::Vec::new(),
        )
    }

    /// Move both pointers to `n` with nothing left queued, as a session that
    /// sent `n` bytes and then dropped leaves the ring after `drain`.
    fn leave_at(ring: &WriteRing, n: usize) {
        struct Filler(usize);
        impl Write for Filler {
            fn write<'a, W: MqttWriter<'a>>(&self, w: &mut W) -> Result<(), PacketWriteError> {
                w.put_slice(&alloc::vec![0u8; self.0])
            }
        }
        assert!(ring.try_put(&Filler(n)).unwrap());
        ring.drain();
    }

    fn poll<F: Future>(f: core::pin::Pin<&mut F>) -> Poll<F::Output> {
        f.poll(&mut Context::from_waker(Waker::noop()))
    }

    fn is_overflow(r: &Poll<Result<(), Error>>) -> bool {
        matches!(
            r,
            Poll::Ready(Err(Error::Client(ClientError::PacketWrite(
                PacketWriteError::Overflow
            ))))
        )
    }

    #[test]
    fn a_connect_over_half_the_ring_fails_at_any_offset() {
        let password = alloc::vec![b'p'; 2100];
        let connect = connect_with_password(&password);
        for offset in [0, DEFAULT_WRITE_BUFFER / 2] {
            let ring = WriteRing::new(DEFAULT_WRITE_BUFFER);
            leave_at(&ring, offset);
            let mut put = pin!(ring.put(&connect, 0));
            assert!(is_overflow(&poll(put.as_mut())), "offset {offset}");
        }
    }

    #[test]
    fn a_connect_that_fits_goes_out_from_offset_2048() {
        // Just under half the ring once the CONNECT's own fields are added.
        let password = alloc::vec![b'p'; 2000];
        let connect = connect_with_password(&password);
        assert!(encoded_len(&connect).unwrap() <= DEFAULT_WRITE_BUFFER / 2);
        let ring = WriteRing::new(DEFAULT_WRITE_BUFFER);
        leave_at(&ring, DEFAULT_WRITE_BUFFER / 2);
        let mut put = pin!(ring.put(&connect, 0));
        assert!(matches!(poll(put.as_mut()), Poll::Ready(Ok(()))));
    }
}
