//! Fan-out event-encoding microbenchmark for design 048 WI4.
//!
//! Measures the per-broadcast cost of turning one shared record value into
//! `N` per-subscriber AimX `event` frames — the only work that differs
//! between the converged AimX-over-WS path and the retired `aimdb-ws-protocol`
//! path (047 §2.2, §3.1). It is a pure CPU/allocation microbenchmark: no
//! sockets, no scheduling, no `ClientManager`, no backpressure.
//!
//! Two strategies produce **byte-identical** frames:
//!
//! - [`encode_naive`] — the production `AimxCodec::encode(Outbound::Event)`,
//!   re-run once per subscriber (re-escapes `topic`/`sub`, re-validates the
//!   record `RawValue`, allocates serde intermediates every time).
//! - [`SharedSuffix`] — the "encode once, patch the per-subscriber fields"
//!   fast path: the shared `topic` is escaped once per broadcast; per
//!   subscriber only the two varying scalars (`sub`, `seq`) are formatted and
//!   the shared record bytes are spliced in.
//!
//! The record payload is copied into each frame by **both** strategies
//! (a contiguous WS text frame embeds it), so this isolates the serialization
//! delta, not a copy the fast path avoids — see design 048 WI4's caveat.

use std::io::Write as _;

use aimdb_core::session::aimx::AimxCodec;
use aimdb_core::session::{EnvelopeCodec, Outbound, Payload};
use serde::Serialize;
use serde_json::value::RawValue;

/// Constant frame head up to (and including) the `seq` key. The AimX event
/// frame serializes its fields in `Frame` declaration order — `t, seq, topic,
/// sub, data` — so `seq` leads and everything after it is either shared per
/// broadcast (`topic`, `data`) or a small per-subscriber scalar (`sub`).
const EVENT_HEAD: &[u8] = br#"{"t":"event","seq":"#;

/// A deterministic record-payload shape used by the fan-out benchmark.
///
/// Payloads are already-serialized JSON (what a record codec hands the
/// session layer): the small shape is a compact structured object; the
/// byte-heavy shape is the JSON integer-array a byte record expands into
/// (JSON has no byte type — see design 046 §6.1), the case where per-subscriber
/// work is most likely to matter.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PayloadShape {
    /// Small structured telemetry object.
    SmallStructured,
    /// ~1 KiB byte record expanded to a JSON integer array.
    ByteHeavy,
}

impl PayloadShape {
    /// Stable order shared by benchmark tables and Criterion identifiers.
    pub const ALL: [Self; 2] = [Self::SmallStructured, Self::ByteHeavy];

    /// Stable benchmark identifier.
    pub const fn name(self) -> &'static str {
        match self {
            Self::SmallStructured => "small_structured",
            Self::ByteHeavy => "byte_heavy",
        }
    }

    /// The concrete record topic the broadcast fires on (shared by every
    /// subscriber of one exact-topic broadcast).
    pub const fn topic(self) -> &'static str {
        "plant/line-7/temperature"
    }

    /// Deterministic already-serialized JSON record value.
    pub fn payload(self) -> Payload {
        match self {
            Self::SmallStructured => Payload::from(
                &br#"{"sensor_id":7,"sequence":1000042,"temperature_c":23.625,"pressure_hpa":1013.25,"rssi_dbm":-67,"healthy":true}"#[..],
            ),
            Self::ByteHeavy => {
                let mut bytes = Vec::with_capacity(4096);
                bytes.push(b'[');
                for index in 0_u16..1_024 {
                    if index != 0 {
                        bytes.push(b',');
                    }
                    write!(bytes, "{}", (index.wrapping_mul(31).wrapping_add(7)) % 256)
                        .expect("write to Vec is infallible");
                }
                bytes.push(b']');
                Payload::from(bytes.as_slice())
            }
        }
    }
}

/// One subscriber's per-event varying fields: its subscription id and the
/// monotonic sequence number the server would stamp for that subscription.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Subscriber {
    /// Subscription routing id (serialized as a JSON string on the wire).
    pub sub: String,
    /// Per-subscription sequence number.
    pub seq: u64,
}

/// Build `count` deterministic subscribers. Ids are engine-style JSON-safe
/// tokens (no characters that would need escaping — asserted byte-identical to
/// the production codec by [`verify_byte_identity`]).
pub fn subscribers(count: usize) -> Vec<Subscriber> {
    (0..count)
        .map(|index| Subscriber {
            sub: format!("s{index}"),
            seq: 1_000_000 + index as u64,
        })
        .collect()
}

/// Encode one subscriber's event frame with the production codec (the naive
/// per-subscriber baseline). Appends into `out`, which the caller supplies
/// fresh per frame (each frame is an independent buffer bound for a socket).
pub fn encode_naive(codec: &AimxCodec, topic: &str, data: &Payload, sub: &str, seq: u64) {
    let mut out = Vec::new();
    encode_naive_into(codec, topic, data, sub, seq, &mut out);
    // Discourage the optimizer from eliding the buffer entirely.
    std::hint::black_box(out);
}

/// As [`encode_naive`] but writes into a caller-owned buffer (used by the
/// allocation bench, which owns the buffer lifecycle).
pub fn encode_naive_into(
    codec: &AimxCodec,
    topic: &str,
    data: &Payload,
    sub: &str,
    seq: u64,
    out: &mut Vec<u8>,
) {
    codec
        .encode(
            Outbound::Event {
                sub,
                seq,
                topic: Some(topic),
                data: data.clone(),
            },
            out,
        )
        .expect("event encode should succeed");
}

/// The shared-suffix fast path: precompute the per-broadcast constant parts
/// once (escaping `topic` a single time), then stamp cheap per-subscriber
/// frames.
pub struct SharedSuffix {
    /// `,"topic":<escaped topic>,"sub":` — built once, reused for every frame.
    mid: Vec<u8>,
    /// The shared already-serialized record value (spliced into every frame,
    /// exactly as the naive path copies it per subscriber).
    data: Payload,
}

impl SharedSuffix {
    /// Prepare the shared segments for one broadcast of `data` on `topic`.
    /// This is the fast path's per-broadcast fixed cost — it escapes `topic`
    /// exactly once, versus the naive path re-escaping it per subscriber.
    pub fn new(topic: &str, data: Payload) -> Self {
        let mut mid = Vec::with_capacity(topic.len() + 24);
        mid.extend_from_slice(br#","topic":"#);
        serde_json::to_writer(&mut mid, topic).expect("topic escapes to valid JSON");
        mid.extend_from_slice(br#","sub":"#);
        Self { mid, data }
    }

    /// Stamp one subscriber's frame. Byte-identical to [`encode_naive`] for
    /// JSON-safe `sub` tokens (verified). Only the two varying scalars are
    /// formatted per call; `topic` is not re-escaped and the record bytes are
    /// spliced verbatim.
    pub fn encode_one(&self, sub: &str, seq: u64, out: &mut Vec<u8>) {
        out.clear();
        out.reserve(EVENT_HEAD.len() + self.mid.len() + self.data.len() + sub.len() + 24);
        out.extend_from_slice(EVENT_HEAD);
        write!(out, "{seq}").expect("write to Vec is infallible");
        out.extend_from_slice(&self.mid);
        // `sub` is an engine-generated JSON-safe token: quote it directly
        // rather than paying serde's escape machinery per subscriber.
        out.push(b'"');
        out.extend_from_slice(sub.as_bytes());
        out.push(b'"');
        out.extend_from_slice(br#","data":"#);
        out.extend_from_slice(&self.data);
        out.push(b'}');
    }

    /// Stamp one subscriber's frame into a fresh buffer.
    pub fn encode_one_owned(&self, sub: &str, seq: u64) -> Vec<u8> {
        let mut out = Vec::new();
        self.encode_one(sub, seq, &mut out);
        out
    }
}

/// Event frame subset mirroring `AimxCodec`'s private `Frame` in serialized
/// field order (`t, seq, topic, sub, data`). Used by [`ValidateOnce`] to run
/// the same serde frame serialization as the codec while borrowing a record
/// value that was validated once, rather than re-validating it per subscriber.
#[derive(Serialize)]
struct EventFrameRef<'a> {
    t: &'a str,
    seq: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    topic: Option<&'a str>,
    sub: &'a str,
    data: &'a RawValue,
}

/// Improvement A in isolation: the production per-subscriber serde frame
/// serialization (topic re-escape + scaffolding + verbatim record splice), but
/// with the record value validated **once per broadcast** instead of once per
/// subscriber. The delta from [`encode_naive`] is exactly the cost of the
/// per-subscriber `as_raw` re-validation; the delta to [`SharedSuffix`] is the
/// serde scaffolding the fast path additionally removes.
pub struct ValidateOnce {
    raw: Box<RawValue>,
}

impl ValidateOnce {
    /// Validate the record value a single time (the hoisted `as_raw`).
    pub fn new(data: &Payload) -> Self {
        let raw = serde_json::from_slice::<Box<RawValue>>(data)
            .expect("record payload must be one valid JSON value");
        Self { raw }
    }

    /// Serialize one subscriber's frame via serde, reusing the pre-validated
    /// record value. Byte-identical to [`encode_naive`].
    pub fn encode_one_owned(&self, topic: &str, sub: &str, seq: u64) -> Vec<u8> {
        let frame = EventFrameRef {
            t: "event",
            seq,
            topic: Some(topic),
            sub,
            data: &self.raw,
        };
        serde_json::to_vec(&frame).expect("event frame serializes")
    }
}

/// Assert all three strategies reproduce the production codec's bytes for
/// `shape` across a spread of subscribers. This is what licenses treating them
/// as measuring the same output rather than comparing different frames.
pub fn verify_byte_identity(shape: PayloadShape) -> Result<(), String> {
    let codec = AimxCodec;
    let topic = shape.topic();
    let data = shape.payload();
    let shared = SharedSuffix::new(topic, data.clone());
    let validate_once = ValidateOnce::new(&data);

    for subscriber in subscribers(512) {
        let mut expected = Vec::new();
        encode_naive_into(
            &codec,
            topic,
            &data,
            &subscriber.sub,
            subscriber.seq,
            &mut expected,
        );
        for (strategy, actual) in [
            (
                "shared_suffix",
                shared.encode_one_owned(&subscriber.sub, subscriber.seq),
            ),
            (
                "validate_once",
                validate_once.encode_one_owned(topic, &subscriber.sub, subscriber.seq),
            ),
        ] {
            if actual != expected {
                return Err(format!(
                    "shape {} strategy {} sub {} seq {}: diverged from codec\n  codec: {}\n  {}:  {}",
                    shape.name(),
                    strategy,
                    subscriber.sub,
                    subscriber.seq,
                    String::from_utf8_lossy(&expected),
                    strategy,
                    String::from_utf8_lossy(&actual),
                ));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fast_path_is_byte_identical_to_codec() {
        for shape in PayloadShape::ALL {
            verify_byte_identity(shape).unwrap_or_else(|error| panic!("{error}"));
        }
    }

    #[test]
    fn payloads_are_valid_json_and_non_empty() {
        for shape in PayloadShape::ALL {
            let payload = shape.payload();
            assert!(!payload.is_empty(), "{} payload empty", shape.name());
            serde_json::from_slice::<serde_json::Value>(&payload)
                .unwrap_or_else(|error| panic!("{} payload invalid JSON: {error}", shape.name()));
        }
    }

    #[test]
    fn byte_heavy_is_much_larger_than_small() {
        let small = PayloadShape::SmallStructured.payload().len();
        let heavy = PayloadShape::ByteHeavy.payload().len();
        assert!(
            heavy > small * 8,
            "expected byte-heavy ≫ small ({heavy} vs {small})"
        );
    }
}
