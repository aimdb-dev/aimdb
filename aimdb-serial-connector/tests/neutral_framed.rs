//! Design 052 §3.1/§3.3 and acceptance 3, exercised: core's
//! [`FramedConnection`] over this crate's COBS framer and a Tokio byte stream,
//! with **no runtime module in the path** and **no `aimdb-tokio-adapter`
//! dependency** — the two things acceptance 3 asks for and the verification
//! doubted were simultaneously achievable.
//!
//! Run with `--features tokio-runtime` (note: *not* `_test-tokio` — the point
//! is that no adapter is needed).
#![cfg(feature = "tokio-runtime")]

use aimdb_core::session::{Connection, FramedConnection};
use aimdb_serial_connector::neutral::{CobsFramer, SerialByteStream, READ_CHUNK, WRITE_CHUNK};

/// The concrete type a neutral serial connector would build: core's framed
/// connection, this crate's framer, and a byte stream the connector owns.
type SerialConnection<S> =
    FramedConnection<SerialByteStream<S>, CobsFramer, READ_CHUNK, WRITE_CHUNK>;

fn pair() -> (
    SerialConnection<tokio::io::DuplexStream>,
    SerialConnection<tokio::io::DuplexStream>,
) {
    let (a, b) = tokio::io::duplex(4096);
    (
        FramedConnection::new(SerialByteStream(a), CobsFramer::new()),
        FramedConnection::new(SerialByteStream(b), CobsFramer::new()),
    )
}

/// Frames round-trip both ways through the neutral stack.
#[tokio::test]
async fn neutral_framed_connection_round_trips() {
    let (mut client, mut server) = pair();

    client.send(b"hello").await.expect("client send");
    let got = server.recv().await.expect("server recv").expect("frame");
    assert_eq!(got, b"hello");

    server.send(b"world").await.expect("server send");
    let got = client.recv().await.expect("client recv").expect("frame");
    assert_eq!(got, b"world");
}

/// Frame boundaries survive back-to-back writes, and a payload larger than
/// `WRITE_CHUNK` is split and reassembled — the chunking `FramedConnection`
/// does for HAL TX rings that reject oversized writes.
#[tokio::test]
async fn neutral_framed_connection_preserves_boundaries_and_chunks() {
    let (mut client, mut server) = pair();

    let big = vec![0xABu8; WRITE_CHUNK * 3 + 7];
    client.send(b"one").await.unwrap();
    client.send(&big).await.unwrap();
    client.send(b"three").await.unwrap();

    assert_eq!(server.recv().await.unwrap().unwrap(), b"one");
    assert_eq!(server.recv().await.unwrap().unwrap(), big);
    assert_eq!(server.recv().await.unwrap().unwrap(), b"three");
}

/// A closed peer reads as EOF (`Ok(None)`), not as an error — the contract
/// `run_session` relies on to end a session cleanly.
#[tokio::test]
async fn neutral_framed_connection_reports_eof_on_close() {
    let (client, mut server) = pair();
    drop(client);
    assert!(server
        .recv()
        .await
        .expect("recv should not error")
        .is_none());
}

/// The whole point of the `+ Send` bounds: this type satisfies the
/// `Box<dyn Connection>` the session engines hand around.
#[tokio::test]
async fn neutral_framed_connection_is_a_boxed_connection() {
    let (client, _server) = pair();
    let mut boxed: Box<dyn Connection> = Box::new(client);
    // `Connection: Send`, so the box crosses a spawn boundary.
    tokio::spawn(async move {
        let _ = boxed.send(b"x").await;
    })
    .await
    .unwrap();
}
