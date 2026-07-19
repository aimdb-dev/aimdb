//! AimX-v2 NDJSON envelope codec (`no_std + alloc`, features `connector-session`
//! + `remote`).
//!
//! One JSON object per line, tagged by a `"t"` field, mapping onto the engine's
//! role-neutral [`Inbound`]/[`Outbound`] message set. This is **not**
//! backward-compatible with the legacy AimX wire:
//!
//! - `record.subscribe` is an [`Inbound::Subscribe`] keyed by the request `id`;
//!   events carry the `id` back as [`Outbound::Event::sub`]. An explicit
//!   `{"t":"subscribed"}` ack exists for servers running
//!   `acks_subscribe:true` (the WebSocket connector); UDS/serial/TCP leave it
//!   off and the ack stays implicit.
//! - events carry `{sub, seq, data}` plus an optional `topic` naming the
//!   concrete record on wildcard/bus subscriptions (no server-side
//!   `timestamp`/`dropped`).
//! - snapshots carry the opening subscription's `sub` so the client demux
//!   routes them like events.
//! - the Hello/Welcome handshake is a normal `call("hello", …)`, so
//!   `authenticate` stays peer-only.
//!
//! The record-value `Payload` is spliced into / sliced out of the envelope
//! verbatim via [`serde_json::value::RawValue`] — no intermediate `Value` tree,
//! no re-escaping.

use alloc::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;

use crate::session::{CodecError, EnvelopeCodec, Inbound, Outbound, Payload, RpcError};

/// The zero-sized AimX-v2 NDJSON codec.
#[derive(Clone, Copy, Default)]
pub struct AimxCodec;

/// One wire frame. A single flat, all-optional struct (rather than an internally
/// tagged enum, which cannot borrow) so both directions can zero-copy-borrow the
/// `&str` / [`RawValue`] fields out of the frame slice. Each logical message
/// fills the subset of fields its `"t"` tag implies; the rest skip-serialize.
#[derive(Serialize, Deserialize)]
struct Frame<'a> {
    t: &'a str,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    id: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    seq: Option<u64>,
    #[serde(default, borrow, skip_serializing_if = "Option::is_none")]
    method: Option<&'a str>,
    #[serde(default, borrow, skip_serializing_if = "Option::is_none")]
    topic: Option<&'a str>,
    #[serde(default, borrow, skip_serializing_if = "Option::is_none")]
    sub: Option<&'a str>,
    #[serde(default, borrow, skip_serializing_if = "Option::is_none")]
    params: Option<&'a RawValue>,
    #[serde(default, borrow, skip_serializing_if = "Option::is_none")]
    payload: Option<&'a RawValue>,
    #[serde(default, borrow, skip_serializing_if = "Option::is_none")]
    data: Option<&'a RawValue>,
    #[serde(default, borrow, skip_serializing_if = "Option::is_none")]
    ok: Option<&'a RawValue>,
    #[serde(default, borrow, skip_serializing_if = "Option::is_none")]
    err: Option<&'a str>,
}

impl<'a> Frame<'a> {
    /// An empty frame with only the tag set; fill the relevant fields after.
    fn tagged(t: &'a str) -> Self {
        Self {
            t,
            id: None,
            seq: None,
            method: None,
            topic: None,
            sub: None,
            params: None,
            payload: None,
            data: None,
            ok: None,
            err: None,
        }
    }
}

/// Borrow a [`RawValue`] view over already-serialized payload bytes (validates
/// the bytes are one JSON value, but does not re-serialize the structure).
fn as_raw(bytes: &[u8]) -> Result<&RawValue, CodecError> {
    serde_json::from_slice(bytes).map_err(|_| CodecError::Malformed)
}

/// Slice a [`RawValue`]'s verbatim JSON bytes back out into an owned [`Payload`];
/// a missing field decodes as the JSON literal `null`.
fn payload_of(raw: Option<&RawValue>) -> Payload {
    match raw {
        Some(r) => Arc::from(r.get().as_bytes()),
        None => Arc::from(&b"null"[..]),
    }
}

fn err_code(e: &RpcError) -> &'static str {
    match e {
        RpcError::NotFound => "not_found",
        RpcError::Denied => "denied",
        RpcError::Internal => "internal",
        RpcError::VersionMismatch => "version_mismatch",
    }
}

fn code_err(s: &str) -> RpcError {
    match s {
        "not_found" => RpcError::NotFound,
        "denied" => RpcError::Denied,
        "version_mismatch" => RpcError::VersionMismatch,
        _ => RpcError::Internal,
    }
}

fn write_frame(out: &mut alloc::vec::Vec<u8>, frame: &Frame<'_>) -> Result<(), CodecError> {
    // `serde_json::to_writer` is gated behind serde_json's `std` feature (the
    // workspace builds it on `alloc` only), so serialize via `to_vec` and splice.
    let bytes = serde_json::to_vec(frame).map_err(|_| CodecError::Malformed)?;
    out.extend_from_slice(&bytes);
    Ok(())
}

impl EnvelopeCodec for AimxCodec {
    // --- server direction: read a request, write a reply/event -------------
    fn decode(&self, frame: &[u8]) -> Result<Inbound, CodecError> {
        let f: Frame = serde_json::from_slice(frame).map_err(|_| CodecError::Malformed)?;
        match f.t {
            "req" => Ok(Inbound::Request {
                id: f.id.ok_or(CodecError::Malformed)?,
                method: f.method.ok_or(CodecError::Malformed)?.into(),
                params: payload_of(f.params),
            }),
            "sub" => Ok(Inbound::Subscribe {
                id: f.id.ok_or(CodecError::Malformed)?,
                topic: f.topic.ok_or(CodecError::Malformed)?.into(),
            }),
            "unsub" => Ok(Inbound::Unsubscribe {
                sub: f.sub.ok_or(CodecError::Malformed)?.into(),
            }),
            "write" => Ok(Inbound::Write {
                topic: f.topic.ok_or(CodecError::Malformed)?.into(),
                payload: payload_of(f.payload),
            }),
            "ping" => Ok(Inbound::Ping),
            _ => Err(CodecError::Malformed),
        }
    }

    fn encode(&self, msg: Outbound<'_>, out: &mut alloc::vec::Vec<u8>) -> Result<(), CodecError> {
        match msg {
            Outbound::Reply { id, result } => {
                let mut frame = Frame::tagged("reply");
                frame.id = Some(id);
                match &result {
                    Ok(data) => {
                        let raw = as_raw(data)?;
                        frame.ok = Some(raw);
                        write_frame(out, &frame)
                    }
                    Err(e) => {
                        frame.err = Some(err_code(e));
                        write_frame(out, &frame)
                    }
                }
            }
            Outbound::Event {
                sub,
                seq,
                topic,
                data,
            } => {
                let raw = as_raw(&data)?;
                let mut frame = Frame::tagged("event");
                frame.sub = Some(sub);
                frame.seq = Some(seq);
                frame.topic = topic;
                frame.data = Some(raw);
                write_frame(out, &frame)
            }
            Outbound::Snapshot { sub, topic, data } => {
                let raw = as_raw(&data)?;
                let mut frame = Frame::tagged("snap");
                frame.sub = Some(sub);
                frame.topic = Some(topic);
                frame.data = Some(raw);
                write_frame(out, &frame)
            }
            Outbound::Pong => write_frame(out, &Frame::tagged("pong")),
            // Explicit subscribe ack — only emitted by servers running
            // `acks_subscribe:true` (the WebSocket connector).
            Outbound::Subscribed { sub } => {
                let mut frame = Frame::tagged("subscribed");
                frame.sub = Some(sub);
                write_frame(out, &frame)
            }
        }
    }

    // --- client direction: write a request, read a reply/event -------------
    fn encode_inbound(
        &self,
        msg: Inbound,
        out: &mut alloc::vec::Vec<u8>,
    ) -> Result<(), CodecError> {
        match msg {
            Inbound::Request { id, method, params } => {
                let raw = as_raw(&params)?;
                let mut frame = Frame::tagged("req");
                frame.id = Some(id);
                frame.method = Some(&method);
                frame.params = Some(raw);
                write_frame(out, &frame)
            }
            Inbound::Subscribe { id, topic } => {
                let mut frame = Frame::tagged("sub");
                frame.id = Some(id);
                frame.topic = Some(&topic);
                write_frame(out, &frame)
            }
            Inbound::Unsubscribe { sub } => {
                let mut frame = Frame::tagged("unsub");
                frame.sub = Some(&sub);
                write_frame(out, &frame)
            }
            Inbound::Write { topic, payload } => {
                let raw = as_raw(&payload)?;
                let mut frame = Frame::tagged("write");
                frame.topic = Some(&topic);
                frame.payload = Some(raw);
                write_frame(out, &frame)
            }
            Inbound::Ping => write_frame(out, &Frame::tagged("ping")),
        }
    }

    fn decode_outbound<'a>(&self, frame: &'a [u8]) -> Result<Outbound<'a>, CodecError> {
        let f: Frame<'a> = serde_json::from_slice(frame).map_err(|_| CodecError::Malformed)?;
        match f.t {
            "reply" => {
                let id = f.id.ok_or(CodecError::Malformed)?;
                let result = match (f.ok, f.err) {
                    (Some(_), _) => Ok(payload_of(f.ok)),
                    (None, Some(code)) => Err(code_err(code)),
                    (None, None) => return Err(CodecError::Malformed),
                };
                Ok(Outbound::Reply { id, result })
            }
            "event" => Ok(Outbound::Event {
                sub: f.sub.ok_or(CodecError::Malformed)?,
                seq: f.seq.ok_or(CodecError::Malformed)?,
                topic: f.topic,
                data: payload_of(f.data),
            }),
            "snap" => Ok(Outbound::Snapshot {
                sub: f.sub.ok_or(CodecError::Malformed)?,
                topic: f.topic.ok_or(CodecError::Malformed)?,
                data: payload_of(f.data),
            }),
            "subscribed" => Ok(Outbound::Subscribed {
                sub: f.sub.ok_or(CodecError::Malformed)?,
            }),
            "pong" => Ok(Outbound::Pong),
            _ => Err(CodecError::Malformed),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::ToString;
    use alloc::vec::Vec;

    fn payload(s: &str) -> Payload {
        Arc::from(s.as_bytes())
    }

    /// Encode client-direction, decode server-direction (the request path).
    fn roundtrip_inbound(msg: Inbound) -> Inbound {
        let codec = AimxCodec;
        let mut out = Vec::new();
        codec.encode_inbound(msg, &mut out).expect("encode_inbound");
        codec.decode(&out).expect("decode")
    }

    /// Encode server-direction, return the frame bytes (the reply/event path).
    fn encode_outbound(msg: Outbound<'_>) -> Vec<u8> {
        let codec = AimxCodec;
        let mut out = Vec::new();
        codec.encode(msg, &mut out).expect("encode");
        out
    }

    #[test]
    fn request_roundtrip() {
        match roundtrip_inbound(Inbound::Request {
            id: 7,
            method: "record.get".to_string(),
            params: payload(r#"{"name":"temp"}"#),
        }) {
            Inbound::Request { id, method, params } => {
                assert_eq!(id, 7);
                assert_eq!(method, "record.get");
                assert_eq!(&params[..], br#"{"name":"temp"}"#);
            }
            _ => panic!("expected Request"),
        }
    }

    #[test]
    fn subscribe_and_unsubscribe_roundtrip() {
        match roundtrip_inbound(Inbound::Subscribe {
            id: 3,
            topic: "sensors.#".to_string(),
        }) {
            Inbound::Subscribe { id, topic } => {
                assert_eq!(id, 3);
                assert_eq!(topic, "sensors.#");
            }
            _ => panic!("expected Subscribe"),
        }
        match roundtrip_inbound(Inbound::Unsubscribe {
            sub: "3".to_string(),
        }) {
            Inbound::Unsubscribe { sub } => assert_eq!(sub, "3"),
            _ => panic!("expected Unsubscribe"),
        }
    }

    #[test]
    fn write_roundtrip_splices_payload_verbatim() {
        match roundtrip_inbound(Inbound::Write {
            topic: "cfg".to_string(),
            payload: payload(r#"{"value":{"level":9}}"#),
        }) {
            Inbound::Write { topic, payload } => {
                assert_eq!(topic, "cfg");
                assert_eq!(&payload[..], br#"{"value":{"level":9}}"#);
            }
            _ => panic!("expected Write"),
        }
    }

    #[test]
    fn ping_pong_roundtrip() {
        assert!(matches!(roundtrip_inbound(Inbound::Ping), Inbound::Ping));
        let frame = encode_outbound(Outbound::Pong);
        assert!(matches!(
            AimxCodec.decode_outbound(&frame).unwrap(),
            Outbound::Pong
        ));
    }

    #[test]
    fn reply_ok_roundtrip() {
        let frame = encode_outbound(Outbound::Reply {
            id: 11,
            result: Ok(payload(r#"{"status":"success"}"#)),
        });
        match AimxCodec.decode_outbound(&frame).unwrap() {
            Outbound::Reply { id, result } => {
                assert_eq!(id, 11);
                assert_eq!(&result.unwrap()[..], br#"{"status":"success"}"#);
            }
            _ => panic!("expected Reply"),
        }
    }

    #[test]
    fn reply_err_codes_roundtrip() {
        for err in [
            RpcError::NotFound,
            RpcError::Denied,
            RpcError::Internal,
            RpcError::VersionMismatch,
        ] {
            let frame = encode_outbound(Outbound::Reply {
                id: 1,
                result: Err(err.clone()),
            });
            match AimxCodec.decode_outbound(&frame).unwrap() {
                Outbound::Reply { result, .. } => assert_eq!(result.unwrap_err(), err),
                _ => panic!("expected Reply"),
            }
        }
    }

    #[test]
    fn event_roundtrip_without_topic() {
        let frame = encode_outbound(Outbound::Event {
            sub: "5",
            seq: 2,
            topic: None,
            data: payload("42"),
        });
        // The optional field skip-serializes — the exact-topic wire is unchanged.
        assert!(!frame.windows(7).any(|w| w == b"\"topic\""));
        match AimxCodec.decode_outbound(&frame).unwrap() {
            Outbound::Event {
                sub,
                seq,
                topic,
                data,
            } => {
                assert_eq!((sub, seq, topic), ("5", 2, None));
                assert_eq!(&data[..], b"42");
            }
            _ => panic!("expected Event"),
        }
    }

    #[test]
    fn event_roundtrip_with_topic() {
        let frame = encode_outbound(Outbound::Event {
            sub: "5",
            seq: 9,
            topic: Some("temp.vienna"),
            data: payload(r#"{"c":21.5}"#),
        });
        match AimxCodec.decode_outbound(&frame).unwrap() {
            Outbound::Event { sub, topic, .. } => {
                assert_eq!(sub, "5");
                assert_eq!(topic, Some("temp.vienna"));
            }
            _ => panic!("expected Event"),
        }
    }

    #[test]
    fn snapshot_roundtrip_carries_sub_and_topic() {
        let frame = encode_outbound(Outbound::Snapshot {
            sub: "8",
            topic: "temp.berlin",
            data: payload(r#"{"c":18.0}"#),
        });
        match AimxCodec.decode_outbound(&frame).unwrap() {
            Outbound::Snapshot { sub, topic, data } => {
                assert_eq!((sub, topic), ("8", "temp.berlin"));
                assert_eq!(&data[..], br#"{"c":18.0}"#);
            }
            _ => panic!("expected Snapshot"),
        }
    }

    #[test]
    fn subscribed_ack_roundtrip() {
        let frame = encode_outbound(Outbound::Subscribed { sub: "13" });
        match AimxCodec.decode_outbound(&frame).unwrap() {
            Outbound::Subscribed { sub } => assert_eq!(sub, "13"),
            _ => panic!("expected Subscribed"),
        }
    }

    #[test]
    fn malformed_frames_are_rejected() {
        let codec = AimxCodec;
        assert!(codec.decode(b"{not json").is_err());
        assert!(codec.decode(br#"{"t":"nope"}"#).is_err());
        // A `sub` frame missing its topic is malformed.
        assert!(codec.decode(br#"{"t":"sub","id":1}"#).is_err());
        assert!(codec.decode_outbound(br#"{"t":"reply","id":1}"#).is_err());
        // A snap without its routing sub is malformed on the new wire.
        assert!(codec
            .decode_outbound(br#"{"t":"snap","topic":"x","data":1}"#)
            .is_err());
    }
}
