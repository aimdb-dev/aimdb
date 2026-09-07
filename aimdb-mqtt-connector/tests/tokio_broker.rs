//! Host smoke for the embedded MQTT backend over `TokioNet::tcp()`
//! (`_test-tokio-broker`).
//!
//! The same session loop the Embassy smoke drives, but over a real TCP socket
//! and a fake broker on the same host — no network stack to stand up. What it
//! adds over that smoke is the reconnect: the broker hangs up after the first
//! SUBACK, and the loop must dial again and re-subscribe.
#![cfg(feature = "_test-tokio-broker")]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

// Each test binary defines these exactly once.
#[defmt::global_logger]
struct HostTestLogger;
unsafe impl defmt::Logger for HostTestLogger {
    fn acquire() {}
    unsafe fn flush() {}
    unsafe fn release() {}
    unsafe fn write(_bytes: &[u8]) {}
}
#[defmt::panic_handler]
fn defmt_panic() -> ! {
    core::panic!("defmt panic in host test")
}

/// Real wall-clock time; the session loop's delays are `embassy_time`'s until
/// it takes core's `Delay`.
struct HostClock;
impl embassy_time_driver::Driver for HostClock {
    fn now(&self) -> u64 {
        use std::sync::OnceLock;
        use std::time::Instant;
        static START: OnceLock<Instant> = OnceLock::new();
        let start = START.get_or_init(Instant::now);
        (start.elapsed().as_micros() * u128::from(embassy_time_driver::TICK_HZ) / 1_000_000) as u64
    }
    fn schedule_wake(&self, _at: u64, waker: &core::task::Waker) {
        waker.wake_by_ref();
    }
}
embassy_time_driver::time_driver_impl!(static HOST_CLOCK: HostClock = HostClock);

// ---------------------------------------------------------------------------
// A fake broker: just enough MQTT 5 to complete a session.
// ---------------------------------------------------------------------------

/// What the broker saw, one entry per accepted connection.
#[derive(Default)]
struct Seen {
    connects: usize,
    subscribes: Vec<Vec<String>>,
}

/// Read one MQTT packet: a fixed header byte, a varint remaining-length, then
/// that many bytes.
async fn read_packet(socket: &mut TcpStream, buf: &mut Vec<u8>) -> Option<(u8, Vec<u8>)> {
    let mut byte = [0u8; 1];
    socket.read_exact(&mut byte).await.ok()?;
    let first = byte[0];

    let mut remaining = 0usize;
    let mut shift = 0;
    loop {
        socket.read_exact(&mut byte).await.ok()?;
        remaining |= ((byte[0] & 0x7F) as usize) << shift;
        if byte[0] & 0x80 == 0 {
            break;
        }
        shift += 7;
    }

    buf.clear();
    buf.resize(remaining, 0);
    socket.read_exact(buf).await.ok()?;
    Some((first, buf.clone()))
}

/// Encode a remaining-length varint.
fn varint(mut n: usize, out: &mut Vec<u8>) {
    loop {
        let mut byte = (n % 128) as u8;
        n /= 128;
        if n > 0 {
            byte |= 0x80;
        }
        out.push(byte);
        if n == 0 {
            break;
        }
    }
}

/// Collect the topics out of a SUBSCRIBE body and answer with a SUBACK
/// granting QoS 1 for each.
fn suback(body: &[u8], topics: &mut Vec<String>) -> Vec<u8> {
    let packet_id = [body[0], body[1]];
    let mut i = 2;
    // Skip the property length varint.
    while i < body.len() && body[i] & 0x80 != 0 {
        i += 1;
    }
    i += 1;

    let mut granted = Vec::new();
    while i + 2 <= body.len() {
        let len = u16::from_be_bytes([body[i], body[i + 1]]) as usize;
        i += 2;
        if i + len > body.len() {
            break;
        }
        topics.push(String::from_utf8_lossy(&body[i..i + len]).into_owned());
        i += len + 1; // topic + subscription options byte
        granted.push(0x01);
    }

    let mut rest = Vec::new();
    rest.extend_from_slice(&packet_id);
    rest.push(0x00); // no properties
    rest.extend_from_slice(&granted);
    let mut ack = vec![0x90];
    varint(rest.len(), &mut ack);
    ack.extend_from_slice(&rest);
    ack
}

/// Serve one connection. `hang_up_after_suback` closes it the moment the
/// subscribe is acknowledged, which is what forces the reconnect.
async fn serve(socket: &mut TcpStream, seen: &Mutex<Seen>, hang_up_after_suback: bool) {
    let mut buf = Vec::new();
    loop {
        let Some((first, body)) = read_packet(socket, &mut buf).await else {
            return;
        };
        match first >> 4 {
            // CONNECT -> CONNACK (session present = 0, reason = success, no props)
            1 => {
                seen.lock().unwrap().connects += 1;
                if socket
                    .write_all(&[0x20, 0x03, 0x00, 0x00, 0x00])
                    .await
                    .is_err()
                {
                    return;
                }
            }
            // SUBSCRIBE -> SUBACK
            8 => {
                let mut topics = Vec::new();
                let ack = suback(&body, &mut topics);
                seen.lock().unwrap().subscribes.push(topics);
                if socket.write_all(&ack).await.is_err() || hang_up_after_suback {
                    return;
                }
            }
            // PINGREQ -> PINGRESP
            12 => {
                if socket.write_all(&[0xD0, 0x00]).await.is_err() {
                    return;
                }
            }
            // DISCONNECT
            14 => return,
            _ => {}
        }
    }
}

/// Accept forever, hanging up on the first `hang_ups` connections.
async fn fake_broker(listener: TcpListener, seen: Arc<Mutex<Seen>>, hang_ups: usize) {
    let mut accepted = 0usize;
    loop {
        let Ok((mut socket, _)) = listener.accept().await else {
            return;
        };
        accepted += 1;
        serve(&mut socket, &seen, accepted <= hang_ups).await;
    }
}

// ---------------------------------------------------------------------------
// The test.
// ---------------------------------------------------------------------------

/// The session loop re-subscribes after the broker hangs up.
///
/// Losing that is silent: publishes keep working and inbound routing simply
/// stops, so this is the assertion the reconnect loop exists for.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_session_loop_reconnects_and_resubscribes() {
    use aimdb_core::buffer::BufferCfg;
    use aimdb_core::AimDbBuilder;
    use aimdb_mqtt_connector::MqttConnector;
    use aimdb_tokio_adapter::net::TokioNet;
    use aimdb_tokio_adapter::{TokioAdapter, TokioRecordRegistrarExt};

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let seen = Arc::new(Mutex::new(Seen::default()));

    let connector = MqttConnector::new(format!("mqtt://127.0.0.1:{port}"))
        .transport(TokioNet::tcp())
        .with_client_id("host-smoke");

    let mut builder = AimDbBuilder::new()
        .runtime(Arc::new(TokioAdapter))
        .with_connector(connector);
    builder.configure::<u64>("temperature", |reg| {
        reg.buffer(BufferCfg::SingleLatest)
            .link_from("mqtt://sensors/temperature")
            .with_deserializer(|_ctx, data: &[u8]| {
                core::str::from_utf8(data)
                    .ok()
                    .and_then(|s| s.trim().parse::<u64>().ok())
                    .ok_or_else(|| String::from("bad payload"))
            })
            .finish();
    });
    let (_db, runner) = builder.build().await.expect("build db");

    let broker = fake_broker(listener, seen.clone(), 1);
    let until_resubscribed = async {
        while seen.lock().unwrap().subscribes.len() < 2 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    };

    tokio::select! {
        _ = runner.run() => panic!("the session loop returned"),
        _ = broker => panic!("the broker returned"),
        _ = until_resubscribed => {}
        _ = tokio::time::sleep(Duration::from_secs(30)) => {
            let seen = seen.lock().unwrap();
            panic!(
                "watchdog: {} connects, {} subscribes",
                seen.connects,
                seen.subscribes.len()
            );
        }
    }

    let seen = seen.lock().unwrap();
    assert!(
        seen.connects >= 2,
        "the loop must redial after the hang-up; saw {} connects",
        seen.connects
    );
    for (n, topics) in seen.subscribes.iter().enumerate() {
        assert!(
            topics.iter().any(|t| t == "sensors/temperature"),
            "connection {n} did not subscribe the inbound topic; saw {topics:?}"
        );
    }
}
