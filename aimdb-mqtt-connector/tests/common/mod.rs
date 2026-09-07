//! A fake MQTT broker over a real TCP socket, speaking just enough of both
//! dialects to complete a session: 3.1.1 for `rumqttc`, 5 for `mountain-mqtt`.
//!
//! The version is read off the CONNECT packet, so one broker serves both
//! backends and a parity test needs only one listener.
//!
//! Compiled into each test binary, so not every item is used by all of them.
#![allow(dead_code)]

use std::sync::{Arc, Mutex};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

/// What the broker saw, accumulated across every connection.
#[derive(Default)]
pub struct Seen {
    pub connects: usize,
    pub client_ids: Vec<String>,
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

/// Step `i` past a varint.
fn skip_varint(body: &[u8], i: &mut usize) {
    while *i < body.len() && body[*i] & 0x80 != 0 {
        *i += 1;
    }
    *i += 1;
}

/// The protocol level a CONNECT declares: 4 is 3.1.1, 5 is MQTT 5.
fn is_v5(body: &[u8]) -> bool {
    body.get(6).is_some_and(|level| *level >= 5)
}

/// The client id a CONNECT carries. It opens the payload, which follows the
/// 10-byte variable header plus, on MQTT 5, a property block.
fn connect_client_id(body: &[u8], v5: bool) -> Option<String> {
    let mut i = 10;
    if v5 {
        let start = i;
        skip_varint(body, &mut i);
        // The varint is the property block's length, which follows it.
        i += *body.get(start)? as usize;
    }
    let len = u16::from_be_bytes([*body.get(i)?, *body.get(i + 1)?]) as usize;
    Some(String::from_utf8_lossy(body.get(i + 2..i + 2 + len)?).into_owned())
}

/// Collect the topics from a SUBSCRIBE body and build the matching SUBACK.
fn suback(body: &[u8], v5: bool, topics: &mut Vec<String>) -> Vec<u8> {
    let packet_id = [body[0], body[1]];
    let mut i = 2;
    if v5 {
        skip_varint(body, &mut i);
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
        skip_varint(body, &mut i);
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
                    if let Some(id) = connect_client_id(&body, v5) {
                        seen.client_ids.push(id);
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

/// Serve several clients at once, which a parity test needs: both backends
/// hold a connection simultaneously.
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
