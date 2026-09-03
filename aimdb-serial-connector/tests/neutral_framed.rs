//! The COBS framer and core's `FramedConnection` over the Tokio adapter's byte
//! stream — the same pairing the Embassy side gets from `EmbassyUart`.
#![cfg(feature = "tokio-runtime")]

use aimdb_core::session::Connection;
use aimdb_serial_connector::neutral::{CobsFramer, TokioFramed, WRITE_CHUNK};
use aimdb_tokio_adapter::net::TokioByteStream;

/// A duplex pipe standing in for a `SerialStream`, framed at both ends.
fn pipe() -> (
    TokioFramed<tokio::io::DuplexStream>,
    TokioFramed<tokio::io::DuplexStream>,
) {
    let (a, b) = tokio::io::duplex(8 * 1024);
    (
        TokioFramed::new(TokioByteStream(a), CobsFramer::new()),
        TokioFramed::new(TokioByteStream(b), CobsFramer::new()),
    )
}

#[tokio::test]
async fn frames_round_trip_in_both_directions() {
    let (mut a, mut b) = pipe();

    a.send(b"{\"m\":\"hello\"}").await.expect("a send");
    assert_eq!(
        b.recv().await.expect("b recv"),
        Some(b"{\"m\":\"hello\"}".to_vec())
    );

    b.send(b"{\"m\":\"pong\"}").await.expect("b send");
    assert_eq!(
        a.recv().await.expect("a recv"),
        Some(b"{\"m\":\"pong\"}".to_vec())
    );
}

/// Frame boundaries survive back-to-back sends: COBS delimits on `0x00`, so
/// several frames can arrive in one read.
#[tokio::test]
async fn back_to_back_frames_stay_separate() {
    let (mut a, mut b) = pipe();

    for i in 0u8..4 {
        a.send(&[i, i, i]).await.expect("send");
    }
    for i in 0u8..4 {
        assert_eq!(b.recv().await.expect("recv"), Some(vec![i, i, i]));
    }
}

/// A payload larger than `WRITE_CHUNK` is split across writes and reassembled
/// across reads — the chunking loops on both sides of the connection.
#[tokio::test]
async fn a_payload_larger_than_the_chunk_survives() {
    let (mut a, mut b) = pipe();

    let payload: Vec<u8> = (0..(WRITE_CHUNK * 5 + 7))
        .map(|i| (i % 251) as u8)
        .collect();
    a.send(&payload).await.expect("send");
    assert_eq!(b.recv().await.expect("recv"), Some(payload));
}

/// A payload full of `0x00` — the COBS delimiter — must not be mistaken for
/// frame boundaries.
#[tokio::test]
async fn a_payload_of_delimiters_is_not_split() {
    let (mut a, mut b) = pipe();

    let payload = vec![0u8; 200];
    a.send(&payload).await.expect("send");
    assert_eq!(b.recv().await.expect("recv"), Some(payload));
}

/// A closed peer reads as `Ok(None)`, which is how the session engines detect
/// a hangup.
#[tokio::test]
async fn a_closed_peer_reads_as_end_of_stream() {
    let (a, mut b) = pipe();
    drop(a);
    assert_eq!(b.recv().await.expect("recv"), None);
}

/// The connection crosses a `tokio::spawn` as a boxed `dyn Connection` — the
/// shape `ConnectorBuilder::build` hands the runner.
#[tokio::test]
async fn a_boxed_connection_crosses_a_spawn() {
    let (mut a, b) = pipe();
    let mut boxed: Box<dyn Connection> = Box::new(b);

    let echo = tokio::spawn(async move {
        let frame = boxed.recv().await.expect("recv").expect("frame");
        boxed.send(&frame).await.expect("send");
    });

    a.send(b"across-threads").await.expect("send");
    assert_eq!(
        a.recv().await.expect("recv"),
        Some(b"across-threads".to_vec())
    );
    echo.await.expect("echo task");
}
