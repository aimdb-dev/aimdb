//! AimX-v2 NDJSON envelope codec (`no_std + alloc`, features `connector-session`
//! + `json-serialize`).
//!
//! One JSON object per line, tagged by a `"t"` field, mapping onto the engine's
//! role-neutral [`Inbound`]/[`Outbound`] message set. This is **not**
//! backward-compatible with the legacy AimX wire:
//!
//! - `record.subscribe` is an [`Inbound::Subscribe`] keyed by the request `id`;
//!   there is no `subscription_id` ack — events carry the `id` back as
//!   [`Outbound::Event::sub`].
//! - events carry only `{sub, seq, data}` (no server-side `timestamp`/`dropped`).
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
    }
}

fn code_err(s: &str) -> RpcError {
    match s {
        "not_found" => RpcError::NotFound,
        "denied" => RpcError::Denied,
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
            Outbound::Event { sub, seq, data } => {
                let raw = as_raw(&data)?;
                let mut frame = Frame::tagged("event");
                frame.sub = Some(sub);
                frame.seq = Some(seq);
                frame.data = Some(raw);
                write_frame(out, &frame)
            }
            Outbound::Snapshot { topic, data } => {
                let raw = as_raw(&data)?;
                let mut frame = Frame::tagged("snap");
                frame.topic = Some(topic);
                frame.data = Some(raw);
                write_frame(out, &frame)
            }
            Outbound::Pong => write_frame(out, &Frame::tagged("pong")),
            // AimX has no explicit subscribe ack; `run_session` only emits this
            // when `acks_subscribe` is set, which the AimX server leaves off.
            Outbound::Subscribed { .. } => Err(CodecError::Malformed),
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
                data: payload_of(f.data),
            }),
            "snap" => Ok(Outbound::Snapshot {
                topic: f.topic.ok_or(CodecError::Malformed)?,
                data: payload_of(f.data),
            }),
            "pong" => Ok(Outbound::Pong),
            _ => Err(CodecError::Malformed),
        }
    }
}
