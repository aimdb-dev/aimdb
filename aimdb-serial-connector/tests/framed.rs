//! The COBS framer and core's `FramedConnection` over the Tokio adapter's byte
//! stream — the same pairing the Embassy side gets from `EmbassyUart`.
#![cfg(feature = "tokio-runtime")]

use aimdb_core::session::Connection;
use aimdb_serial_connector::framing::{CobsFramer, TokioFramed, WRITE_CHUNK};
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

// ---------------------------------------------------------------------------
// The neutral client/server sugar over an adapter byte stream.
// ---------------------------------------------------------------------------

/// A UART is point-to-point: the stream is served once, so a second `accept`
/// parks rather than erroring — `serve` would otherwise spin on it.
#[tokio::test]
async fn a_one_shot_listener_yields_once_then_parks() {
    use aimdb_core::session::Listener;
    use aimdb_serial_connector::connector::{framed, OneShotListener};

    let (a, _b) = tokio::io::duplex(1024);
    let mut listener = OneShotListener::new(framed(TokioByteStream(a)));

    assert!(listener.accept().await.is_ok(), "first accept yields");

    let parked =
        tokio::time::timeout(std::time::Duration::from_millis(50), listener.accept()).await;
    assert!(parked.is_err(), "a second accept must park, not resolve");
}

/// The dialer's dual: nothing to redial on a UART, so a second attempt is a
/// real error rather than a silent reconnect loop.
#[tokio::test]
async fn a_one_shot_dialer_refuses_a_second_connect() {
    use aimdb_core::session::Dialer;
    use aimdb_serial_connector::connector::{framed, OneShotDialer};

    let (a, _b) = tokio::io::duplex(1024);
    let dialer = OneShotDialer::new(framed(TokioByteStream(a)));

    assert!(dialer.connect().await.is_ok(), "first connect yields");
    assert!(
        dialer.connect().await.is_err(),
        "a UART has no second connection to hand out"
    );
}

/// The stream is moved in, so a second `build` is refused, and a `build()`
/// future dropped before it is polled must leave the stream in place.
#[tokio::test]
async fn the_server_guards_its_moved_in_stream() {
    use aimdb_core::buffer::BufferCfg;
    use aimdb_core::connector::ConnectorBuilder;
    use aimdb_core::AimDbBuilder;
    use aimdb_serial_connector::connector::SerialServer;
    use aimdb_tokio_adapter::{TokioAdapter, TokioRecordRegistrarExt};

    let mut builder = AimDbBuilder::new().runtime(std::sync::Arc::new(TokioAdapter));
    builder.configure::<u64>("counter", |reg| {
        reg.buffer(BufferCfg::SingleLatest).with_remote_access();
    });
    let (db, _runner) = builder.build().await.expect("build db");

    let (a, _b) = tokio::io::duplex(1024);
    let server = SerialServer::new(TokioByteStream(a));

    // Unpolled build: the stream must survive it.
    drop(server.build(&db));

    let futures = server
        .build(&db)
        .await
        .expect("the stream must survive an unpolled build");
    assert_eq!(futures.len(), 1, "one serve future");

    let Err(err) = server.build(&db).await else {
        panic!("a second build must fail");
    };
    assert!(
        format!("{err}").contains("already taken"),
        "unexpected error: {err}"
    );
}
