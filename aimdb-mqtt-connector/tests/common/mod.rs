//! A fake MQTT broker over a real TCP socket, speaking just enough of both
//! dialects to complete a session: 3.1.1 for `rumqttc`, 5 for `mountain-mqtt`.
//! The version is read off the CONNECT packet, so one listener serves both.
//!
//! Compiled into each test binary, so not every item is used by all of them.
#![allow(dead_code)]

use std::future::Future;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use aimdb_core::session::{Delay, StreamDialer, TransportResult};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

/// What the broker saw, accumulated across every connection.
#[derive(Default)]
pub struct Seen {
    pub connects: usize,
    pub client_ids: Vec<String>,
    /// The username/password each CONNECT carried, when it carried any.
    pub credentials: Vec<Option<(String, String)>>,
    pub subscribes: Vec<Vec<String>>,
    pub published: Vec<(String, Vec<u8>)>,
}

impl Seen {
    /// Every topic subscribed on any connection.
    pub fn subscribed_topics(&self) -> Vec<&str> {
        self.subscribes
            .iter()
            .flatten()
            .map(String::as_str)
            .collect()
    }
}

/// Read one MQTT packet: a fixed header byte, a varint remaining-length, then
/// that many bytes.
async fn read_packet<S>(socket: &mut S, buf: &mut Vec<u8>) -> Option<(u8, Vec<u8>)>
where
    S: tokio::io::AsyncRead + Unpin,
{
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

/// Decode an MQTT variable-byte integer at `i`, stepping past it.
fn take_varint(body: &[u8], i: &mut usize) -> Option<usize> {
    let mut value = 0usize;
    let mut shift = 0;
    loop {
        let byte = *body.get(*i)?;
        *i += 1;
        value |= ((byte & 0x7f) as usize) << shift;
        if byte & 0x80 == 0 {
            return Some(value);
        }
        shift += 7;
        // MQTT caps a variable-byte integer at four bytes.
        if shift > 21 {
            return None;
        }
    }
}

/// Step `i` past an MQTT 5 property block — its length varint, then the
/// properties themselves.
fn skip_properties(body: &[u8], i: &mut usize) -> Option<()> {
    let len = take_varint(body, i)?;
    *i += len;
    Some(())
}

/// The protocol level a CONNECT declares: 4 is 3.1.1, 5 is MQTT 5.
fn is_v5(body: &[u8]) -> bool {
    body.get(6).is_some_and(|level| *level >= 5)
}

/// Read a length-prefixed field and step past it.
fn take_field(body: &[u8], i: &mut usize) -> Option<String> {
    let len = u16::from_be_bytes([*body.get(*i)?, *body.get(*i + 1)?]) as usize;
    let field = String::from_utf8_lossy(body.get(*i + 2..*i + 2 + len)?).into_owned();
    *i += 2 + len;
    Some(field)
}

/// The identity a CONNECT carries: client id, then the credentials its flags
/// advertise. Nothing here sets a will, so the payload fields are contiguous.
fn connect_identity(body: &[u8], v5: bool) -> Option<(String, Option<(String, String)>)> {
    let flags = *body.get(7)?;
    let mut i = 10;
    if v5 {
        skip_properties(body, &mut i)?;
    }

    let client_id = take_field(body, &mut i)?;
    let credentials = if flags & 0x80 != 0 {
        let username = take_field(body, &mut i)?;
        let password = if flags & 0x40 != 0 {
            take_field(body, &mut i)?
        } else {
            String::new()
        };
        Some((username, password))
    } else {
        None
    };
    Some((client_id, credentials))
}

/// Collect the topics from a SUBSCRIBE body and build the matching SUBACK.
fn suback(body: &[u8], v5: bool, topics: &mut Vec<String>) -> Vec<u8> {
    let packet_id = [body[0], body[1]];
    let mut i = 2;
    if v5 {
        // Best-effort: this returns a SUBACK either way, and a malformed
        // property block shows up as an unparsable topic below.
        let _ = skip_properties(body, &mut i);
    }

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
    if v5 {
        rest.push(0x00); // no properties
    }
    rest.extend_from_slice(&granted);

    let mut ack = vec![0x90];
    varint(rest.len(), &mut ack);
    ack.extend_from_slice(&rest);
    ack
}

/// Encode a QoS-0 PUBLISH for the broker to push at the client.
fn publish(topic: &str, payload: &[u8], v5: bool) -> Vec<u8> {
    let mut rest = Vec::new();
    rest.extend_from_slice(&(topic.len() as u16).to_be_bytes());
    rest.extend_from_slice(topic.as_bytes());
    if v5 {
        rest.push(0x00); // no properties
    }
    rest.extend_from_slice(payload);

    let mut packet = vec![0x30];
    varint(rest.len(), &mut packet);
    packet.extend_from_slice(&rest);
    packet
}

/// Decode a PUBLISH the client sent: topic, payload, and the packet id that is
/// present only above QoS 0.
fn parse_publish(first: u8, body: &[u8], v5: bool) -> Option<(String, Vec<u8>, Option<[u8; 2]>)> {
    let topic_len = u16::from_be_bytes([*body.first()?, *body.get(1)?]) as usize;
    let topic = String::from_utf8_lossy(body.get(2..2 + topic_len)?).into_owned();
    let mut i = 2 + topic_len;

    let packet_id = if (first >> 1) & 0x03 > 0 {
        let id = [*body.get(i)?, *body.get(i + 1)?];
        i += 2;
        Some(id)
    } else {
        None
    };

    if v5 {
        skip_properties(body, &mut i)?;
    }
    Some((topic, body.get(i..)?.to_vec(), packet_id))
}

/// How a connection should behave once it has acknowledged a subscribe.
#[derive(Clone, Copy, Default)]
pub struct AfterSuback<'a> {
    /// Close the connection, forcing the client to reconnect.
    pub hang_up: bool,
    /// Push this message at the client.
    pub push: Option<(&'a str, &'a [u8])>,
}

/// Serve one connection until it closes.
async fn serve(socket: &mut TcpStream, seen: &Mutex<Seen>, after: AfterSuback<'_>) {
    serve_stream(socket, seen, after).await
}

/// The broker loop over any stream, so a TLS session drives the same code.
pub async fn serve_stream<S>(socket: &mut S, seen: &Mutex<Seen>, after: AfterSuback<'_>)
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let mut buf = Vec::new();
    let mut v5 = true;

    loop {
        let Some((first, body)) = read_packet(socket, &mut buf).await else {
            return;
        };
        match first >> 4 {
            // CONNECT -> CONNACK. MQTT 5 carries a property length; 3.1.1 does not.
            1 => {
                v5 = is_v5(&body);
                {
                    let mut seen = seen.lock().unwrap();
                    seen.connects += 1;
                    if let Some((id, credentials)) = connect_identity(&body, v5) {
                        seen.client_ids.push(id);
                        seen.credentials.push(credentials);
                    }
                }
                let ack: &[u8] = if v5 {
                    &[0x20, 0x03, 0x00, 0x00, 0x00]
                } else {
                    &[0x20, 0x02, 0x00, 0x00]
                };
                if socket.write_all(ack).await.is_err() {
                    return;
                }
            }
            // SUBSCRIBE -> SUBACK granting QoS 1 for each requested topic.
            8 => {
                let mut topics = Vec::new();
                let ack = suback(&body, v5, &mut topics);
                seen.lock().unwrap().subscribes.push(topics);
                if socket.write_all(&ack).await.is_err() || after.hang_up {
                    return;
                }
                if let Some((topic, payload)) = after.push {
                    if socket
                        .write_all(&publish(topic, payload, v5))
                        .await
                        .is_err()
                    {
                        return;
                    }
                }
            }
            // PUBLISH from the client: record it, and PUBACK above QoS 0.
            3 => {
                let Some((topic, payload, packet_id)) = parse_publish(first, &body, v5) else {
                    return;
                };
                seen.lock().unwrap().published.push((topic, payload));
                if let Some(id) = packet_id {
                    if socket.write_all(&[0x40, 0x02, id[0], id[1]]).await.is_err() {
                        return;
                    }
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

/// Accept forever. `hang_ups` connections are dropped after their SUBACK;
/// every later one is served normally.
pub async fn fake_broker(
    listener: TcpListener,
    seen: Arc<Mutex<Seen>>,
    hang_ups: usize,
    push: Option<(&str, &[u8])>,
) {
    let mut accepted = 0usize;
    loop {
        let Ok((mut socket, _)) = listener.accept().await else {
            return;
        };
        accepted += 1;
        let after = AfterSuback {
            hang_up: accepted <= hang_ups,
            push,
        };
        serve(&mut socket, &seen, after).await;
    }
}

/// Serve several clients at once, as a parity test needs.
pub async fn fake_broker_concurrent(
    listener: TcpListener,
    seen: Arc<Mutex<Seen>>,
    push: Option<(&'static str, &'static [u8])>,
) {
    loop {
        let Ok((mut socket, _)) = listener.accept().await else {
            return;
        };
        let seen = seen.clone();
        tokio::spawn(async move {
            let after = AfterSuback {
                hang_up: false,
                push,
            };
            serve(&mut socket, &seen, after).await;
        });
    }
}

// ===========================================================================
// The scripted broker: the same wire format as above, but the test decides
// when each answer goes out (design 053's criteria 1, 2, 4 and 11).
// ===========================================================================

/// The topic the scripted broker pushes on.
pub const SCRIPT_TOPIC: &str = "sensors/temperature";

/// What the scripted broker saw, and when.
#[derive(Default)]
pub struct Log {
    pub pings: usize,
    /// Client publishes, as (topic, payload).
    pub publishes: Vec<(String, Vec<u8>)>,
    /// Pings that arrived while the broker was deliberately stalling.
    pub pings_during_stall: usize,
    /// Client publishes that arrived while the broker was stalling.
    pub publishes_during_stall: usize,
}

/// Read one packet: header byte, varint remaining length, body.
async fn read_one<S: tokio::io::AsyncRead + Unpin>(socket: &mut S) -> Option<(u8, Vec<u8>)> {
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

    let mut body = vec![0u8; remaining];
    socket.read_exact(&mut body).await.ok()?;
    Some((first, body))
}

/// How the scripted broker should misbehave after it has SUBACKed.
#[derive(Clone, Copy)]
pub enum Script {
    /// Answer nothing but pings: an idle, healthy session.
    Idle,
    /// Push a PUBLISH split in two with `gap` between the halves.
    SplitPublish { gap: Duration },
    /// Hold every PUBACK back by `delay`.
    SlowPuback { delay: Duration },
    /// Push inbound PUBLISHes as fast as they will go, for `duration`.
    Flood { duration: Duration },
}

/// The broker's write side.
///
/// While `hold` is `Some`, a packet is half-written and nothing else may go on
/// the wire: injecting a PUBACK between the halves of a PUBLISH would corrupt
/// the framing rather than test it. Held bytes go out behind the packet's tail.
struct Wire<S> {
    writer: tokio::io::WriteHalf<S>,
    hold: Option<Vec<u8>>,
}

type Writer<S> = Arc<tokio::sync::Mutex<Wire<S>>>;

async fn send<S: tokio::io::AsyncWrite>(writer: &Writer<S>, bytes: &[u8]) -> bool {
    let mut wire = writer.lock().await;
    match wire.hold.as_mut() {
        Some(held) => {
            held.extend_from_slice(bytes);
            true
        }
        None => wire.writer.write_all(bytes).await.is_ok(),
    }
}

/// Serve one already-accepted connection, following `script`.
///
/// Generic over the stream, so the same script runs over plain TCP and TLS.
///
/// Every scripted delay runs in its own task, so the broker **never stops
/// reading** — without which a ping arriving mid-stall would be counted after
/// the stall rather than during it.
pub async fn scripted_broker<S>(stream: S, log: Arc<Mutex<Log>>, script: Script)
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Send + 'static,
{
    let (mut reader, writer) = tokio::io::split(stream);
    let writer: Writer<S> = Arc::new(tokio::sync::Mutex::new(Wire { writer, hold: None }));

    // Open while the broker is deliberately withholding something.
    let stalling = Arc::new(AtomicUsize::new(0));

    loop {
        let Some((first, body)) = read_one(&mut reader).await else {
            return;
        };
        let in_stall = stalling.load(Ordering::Relaxed) == 1;

        match first >> 4 {
            // CONNECT -> CONNACK
            1 => {
                if !send(&writer, &[0x20, 0x03, 0x00, 0x00, 0x00]).await {
                    return;
                }
            }
            // SUBSCRIBE -> SUBACK, then run the script.
            8 => {
                let packet_id = [body[0], body[1]];
                if !send(
                    &writer,
                    &[0x90, 0x04, packet_id[0], packet_id[1], 0x00, 0x01],
                )
                .await
                {
                    return;
                }

                match script {
                    Script::Idle | Script::SlowPuback { .. } => {}
                    Script::SplitPublish { gap } => {
                        // Half a packet, a long silence, then the rest. The
                        // polled loop's `receive_if_ready` commits to reading
                        // the whole packet and parks here.
                        let writer = writer.clone();
                        let stalling = stalling.clone();
                        tokio::spawn(async move {
                            let packet = publish(SCRIPT_TOPIC, b"21", true);
                            let cut = packet.len() / 2;
                            {
                                let mut wire = writer.lock().await;
                                if wire.writer.write_all(&packet[..cut]).await.is_err() {
                                    return;
                                }
                                // Nothing else may reach the wire until the
                                // tail does.
                                wire.hold = Some(Vec::new());
                            }
                            stalling.store(1, Ordering::Relaxed);
                            tokio::time::sleep(gap).await;
                            stalling.store(0, Ordering::Relaxed);

                            let mut wire = writer.lock().await;
                            let held = wire.hold.take().unwrap_or_default();
                            if wire.writer.write_all(&packet[cut..]).await.is_err() {
                                return;
                            }
                            let _ = wire.writer.write_all(&held).await;
                        });
                    }
                    Script::Flood { duration } => {
                        let writer = writer.clone();
                        tokio::spawn(async move {
                            let deadline = tokio::time::Instant::now() + duration;
                            let packet = publish(SCRIPT_TOPIC, b"7", true);
                            while tokio::time::Instant::now() < deadline {
                                // The lock is taken and released per packet, so
                                // PUBACKs and PINGRESPs interleave with the
                                // flood rather than queueing behind all of it.
                                if !send(&writer, &packet).await {
                                    return;
                                }
                                tokio::task::yield_now().await;
                            }
                        });
                    }
                }
            }
            // PUBLISH from the client.
            3 => {
                let Some((topic, payload, packet_id)) = parse_publish(first, &body, true) else {
                    return;
                };
                {
                    let mut log = log.lock().unwrap();
                    log.publishes.push((topic, payload));
                    if in_stall {
                        log.publishes_during_stall += 1;
                    }
                }
                if let Some(id) = packet_id {
                    match script {
                        // Acknowledge late, in its own task, with the stall
                        // window open: a ping arriving meanwhile is the
                        // assertion, and the read loop has to stay live to see
                        // it.
                        Script::SlowPuback { delay } => {
                            let writer = writer.clone();
                            let stalling = stalling.clone();
                            tokio::spawn(async move {
                                stalling.store(1, Ordering::Relaxed);
                                tokio::time::sleep(delay).await;
                                stalling.store(0, Ordering::Relaxed);
                                send(&writer, &[0x40, 0x02, id[0], id[1]]).await;
                            });
                        }
                        _ => {
                            if !send(&writer, &[0x40, 0x02, id[0], id[1]]).await {
                                return;
                            }
                        }
                    }
                }
            }
            // PINGREQ -> PINGRESP
            12 => {
                {
                    let mut log = log.lock().unwrap();
                    log.pings += 1;
                    if in_stall {
                        log.pings_during_stall += 1;
                    }
                }
                if !send(&writer, &[0xD0, 0x00]).await {
                    return;
                }
            }
            14 => return,
            _ => {}
        }
    }
}

// ===========================================================================
// A dialer that counts what the session sleeps on.
// ===========================================================================

/// `TokioNet::tcp()` with a tally of every `Delay::sleep` the connector asks
/// for — the connector takes its clock from the dialer, so this is where the
/// wake cadence is observable.
#[derive(Clone)]
pub struct CountingDialer {
    inner: aimdb_tokio_adapter::net::TokioTcpDialer,
    sleeps: Arc<AtomicUsize>,
}

impl CountingDialer {
    pub fn new() -> Self {
        Self {
            inner: aimdb_tokio_adapter::net::TokioNet::tcp(),
            sleeps: Arc::new(AtomicUsize::new(0)),
        }
    }

    /// The running tally, shared with the dialer the connector holds.
    pub fn sleeps(&self) -> Arc<AtomicUsize> {
        self.sleeps.clone()
    }
}

impl StreamDialer for CountingDialer {
    type Stream = <aimdb_tokio_adapter::net::TokioTcpDialer as StreamDialer>::Stream;

    fn connect<'a>(
        &'a self,
        host: &'a str,
        port: u16,
    ) -> impl Future<Output = TransportResult<Self::Stream>> + Send + 'a {
        self.inner.connect(host, port)
    }
}

impl Delay for CountingDialer {
    fn sleep(&self, d: Duration) -> impl Future<Output = ()> + Send {
        self.sleeps.fetch_add(1, Ordering::Relaxed);
        Delay::sleep(&self.inner, d)
    }
}
