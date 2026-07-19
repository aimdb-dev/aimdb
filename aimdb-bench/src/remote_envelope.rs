//! Isolated native-CBOR envelope consumer prototype for issue #156.
//!
//! The JSON side runs the production [`AimxCodec`] and its real session
//! [`Inbound`]/[`Outbound`] messages. The CBOR side deliberately remains a
//! benchmark-only protocol sketch: an owned envelope carries the already
//! encoded record payload as a CBOR byte string, preserving the session
//! layer's opaque-payload boundary.
//!
//! This is a CPU/representation benchmark, not a connector benchmark. It does
//! not include framing I/O, syscalls, scheduling, or backpressure.

use std::fmt;
use std::sync::Arc;

use aimdb_core::session::aimx::AimxCodec;
use aimdb_core::session::{CodecError, EnvelopeCodec, Inbound, Outbound, Payload};
use ciborium::Value as CborValue;
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;

use crate::remote_ir::{BinaryBlob, RemoteIrError, RemoteIrFixture};

/// Explicit identifier a real connector would negotiate before exchanging
/// frames. No production connector recognizes it today.
pub const NATIVE_CBOR_PROTOCOL_ID: &str = "aimx-cbor-prototype-v1";

/// Trusted-fixture ceiling used to avoid accidentally feeding a huge frame to
/// the prototype. This is not a complete production decoding policy: nesting
/// and collection limits would still need to be enforced while parsing.
pub const PROTOTYPE_MAX_FRAME_BYTES: usize = 16 * 1024;

/// Logical session flow exercised by the envelope prototype.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EnvelopeOperation {
    /// Client request carrying type-erased parameters.
    Request,
    /// Client fire-and-forget record write.
    Write,
    /// Server subscription event carrying a record update.
    Event,
}

impl EnvelopeOperation {
    /// Stable order shared by size tables, tests, and Criterion identifiers.
    pub const ALL: [Self; 3] = [Self::Request, Self::Write, Self::Event];

    /// Stable benchmark identifier.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Request => "request",
            Self::Write => "write",
            Self::Event => "event",
        }
    }
}

/// Envelope path being compared.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EnvelopeProtocol {
    /// Current record path (`T -> Value -> bytes`) in the production AimX-v2
    /// JSON envelope, followed by dynamic JSON record decode.
    AimxJsonTree,
    /// Required control: direct record serialization into the production
    /// AimX-v2 JSON envelope, followed by dynamic JSON record decode.
    AimxJsonDirect,
    /// Benchmark-only CBOR envelope plus dynamic CBOR record decode.
    NativeCborPrototype,
}

impl EnvelopeProtocol {
    /// Both protocols in stable output order.
    pub const ALL: [Self; 3] = [
        Self::AimxJsonTree,
        Self::AimxJsonDirect,
        Self::NativeCborPrototype,
    ];

    /// Stable benchmark identifier.
    pub const fn name(self) -> &'static str {
        match self {
            Self::AimxJsonTree => "aimx_json_tree",
            Self::AimxJsonDirect => "aimx_json_direct",
            Self::NativeCborPrototype => "native_cbor_prototype",
        }
    }
}

/// Dynamic value returned to the benchmark consumer after envelope decode.
#[derive(Clone, Debug, PartialEq)]
pub enum DynamicRecord {
    /// Existing JSON consumer representation.
    Json(JsonValue),
    /// Native CBOR consumer representation.
    Cbor(CborValue),
}

/// One complete encode-envelope/decode-envelope/dynamic-decode observation.
#[derive(Clone, Debug, PartialEq)]
pub struct EnvelopeObservation {
    /// Complete protocol frame length.
    pub frame_bytes: usize,
    /// Type-erased record delivered to the consumer.
    pub record: DynamicRecord,
}

/// Errors raised by the benchmark-only envelope comparison.
#[derive(Debug)]
pub enum RemoteEnvelopeError {
    /// The production AimX codec rejected a frame or logical message.
    Aimx(CodecError),
    /// JSON payload serialization/deserialization failed.
    Json(serde_json::Error),
    /// Native CBOR payload or frame serialization/deserialization failed.
    Cbor(String),
    /// The decoded logical message did not match the operation under test.
    UnexpectedMessage(&'static str),
    /// The prototype's coarse complete-frame bound was exceeded.
    FrameTooLarge(usize),
    /// The dynamic record did not equal the expected representation.
    RoundTrip(&'static str),
}

impl fmt::Display for RemoteEnvelopeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Aimx(error) => write!(formatter, "AimX codec error: {error:?}"),
            Self::Json(error) => write!(formatter, "JSON error: {error}"),
            Self::Cbor(error) => write!(formatter, "CBOR error: {error}"),
            Self::UnexpectedMessage(message) => write!(formatter, "unexpected message: {message}"),
            Self::FrameTooLarge(bytes) => write!(
                formatter,
                "prototype frame is {bytes} bytes (limit {PROTOTYPE_MAX_FRAME_BYTES})"
            ),
            Self::RoundTrip(protocol) => {
                write!(formatter, "{protocol} envelope failed to round-trip")
            }
        }
    }
}

impl std::error::Error for RemoteEnvelopeError {}

impl From<CodecError> for RemoteEnvelopeError {
    fn from(error: CodecError) -> Self {
        Self::Aimx(error)
    }
}

impl From<serde_json::Error> for RemoteEnvelopeError {
    fn from(error: serde_json::Error) -> Self {
        Self::Json(error)
    }
}

impl From<RemoteIrError> for RemoteEnvelopeError {
    fn from(error: RemoteIrError) -> Self {
        Self::Cbor(error.to_string())
    }
}

/// Run one end-to-end-in-memory envelope path.
pub fn round_trip<T>(
    protocol: EnvelopeProtocol,
    operation: EnvelopeOperation,
    value: &T,
) -> Result<EnvelopeObservation, RemoteEnvelopeError>
where
    T: RemoteIrFixture,
{
    match protocol {
        EnvelopeProtocol::AimxJsonTree => round_trip_aimx_json(operation, value, true),
        EnvelopeProtocol::AimxJsonDirect => round_trip_aimx_json(operation, value, false),
        EnvelopeProtocol::NativeCborPrototype => round_trip_native_cbor(operation, value),
    }
}

/// Return complete frame sizes for every operation/protocol pair.
pub fn encoded_frame_sizes<T>(
) -> Result<Vec<(EnvelopeOperation, EnvelopeProtocol, usize)>, RemoteEnvelopeError>
where
    T: RemoteIrFixture,
{
    let sample = T::sample();
    let mut rows = Vec::with_capacity(EnvelopeOperation::ALL.len() * EnvelopeProtocol::ALL.len());
    for operation in EnvelopeOperation::ALL {
        for protocol in EnvelopeProtocol::ALL {
            let observation = round_trip(protocol, operation, &sample)?;
            rows.push((operation, protocol, observation.frame_bytes));
        }
    }
    Ok(rows)
}

/// Verify both dynamic consumers reproduce their protocol-native expected
/// representation for all three session flows.
pub fn verify_envelope_round_trips<T>() -> Result<(), RemoteEnvelopeError>
where
    T: RemoteIrFixture,
{
    let sample = T::sample();
    let expected_json = DynamicRecord::Json(serde_json::to_value(&sample)?);
    let expected_cbor = DynamicRecord::Cbor(
        CborValue::serialized(&sample)
            .map_err(|error| RemoteEnvelopeError::Cbor(error.to_string()))?,
    );

    for operation in EnvelopeOperation::ALL {
        for protocol in [
            EnvelopeProtocol::AimxJsonTree,
            EnvelopeProtocol::AimxJsonDirect,
        ] {
            let json = round_trip(protocol, operation, &sample)?;
            if json.record != expected_json {
                return Err(RemoteEnvelopeError::RoundTrip(protocol.name()));
            }
        }

        let cbor = round_trip(EnvelopeProtocol::NativeCborPrototype, operation, &sample)?;
        if cbor.record != expected_cbor {
            return Err(RemoteEnvelopeError::RoundTrip(NATIVE_CBOR_PROTOCOL_ID));
        }
    }
    Ok(())
}

fn round_trip_aimx_json<T>(
    operation: EnvelopeOperation,
    value: &T,
    materialize_tree: bool,
) -> Result<EnvelopeObservation, RemoteEnvelopeError>
where
    T: RemoteIrFixture,
{
    let record_bytes = if materialize_tree {
        serde_json::to_vec(&serde_json::to_value(value)?)?
    } else {
        serde_json::to_vec(value)?
    };
    let payload: Payload = Arc::from(record_bytes);
    let codec = AimxCodec;
    let mut frame = Vec::new();

    let decoded_payload = match operation {
        EnvelopeOperation::Request => {
            codec.encode_inbound(
                Inbound::Request {
                    id: 42,
                    method: "record.set".to_string(),
                    params: payload,
                },
                &mut frame,
            )?;
            match codec.decode(&frame)? {
                Inbound::Request { params, .. } => params,
                _ => return Err(RemoteEnvelopeError::UnexpectedMessage("AimX request")),
            }
        }
        EnvelopeOperation::Write => {
            codec.encode_inbound(
                Inbound::Write {
                    topic: "plant/line-7/state".to_string(),
                    payload,
                },
                &mut frame,
            )?;
            match codec.decode(&frame)? {
                Inbound::Write { payload, .. } => payload,
                _ => return Err(RemoteEnvelopeError::UnexpectedMessage("AimX write")),
            }
        }
        EnvelopeOperation::Event => {
            codec.encode(
                Outbound::Event {
                    sub: "sub-42",
                    seq: 1_000_042,
                    data: payload,
                },
                &mut frame,
            )?;
            match codec.decode_outbound(&frame)? {
                Outbound::Event { data, .. } => data,
                _ => return Err(RemoteEnvelopeError::UnexpectedMessage("AimX event")),
            }
        }
    };

    let record = serde_json::from_slice(&decoded_payload)?;
    Ok(EnvelopeObservation {
        frame_bytes: frame.len(),
        record: DynamicRecord::Json(record),
    })
}

#[derive(Serialize, Deserialize)]
struct NativeFrame {
    t: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    id: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    seq: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    method: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    topic: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    sub: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    params: Option<BinaryBlob>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    payload: Option<BinaryBlob>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    data: Option<BinaryBlob>,
}

impl NativeFrame {
    fn tagged(tag: &str) -> Self {
        Self {
            t: tag.to_string(),
            id: None,
            seq: None,
            method: None,
            topic: None,
            sub: None,
            params: None,
            payload: None,
            data: None,
        }
    }
}

fn round_trip_native_cbor<T>(
    operation: EnvelopeOperation,
    value: &T,
) -> Result<EnvelopeObservation, RemoteEnvelopeError>
where
    T: RemoteIrFixture,
{
    let mut record_bytes = Vec::new();
    ciborium::ser::into_writer(value, &mut record_bytes)
        .map_err(|error| RemoteEnvelopeError::Cbor(format!("{error:?}")))?;

    let mut native = match operation {
        EnvelopeOperation::Request => {
            let mut frame = NativeFrame::tagged("req");
            frame.id = Some(42);
            frame.method = Some("record.set".to_string());
            frame.params = Some(BinaryBlob(record_bytes));
            frame
        }
        EnvelopeOperation::Write => {
            let mut frame = NativeFrame::tagged("write");
            frame.topic = Some("plant/line-7/state".to_string());
            frame.payload = Some(BinaryBlob(record_bytes));
            frame
        }
        EnvelopeOperation::Event => {
            let mut frame = NativeFrame::tagged("event");
            frame.sub = Some("sub-42".to_string());
            frame.seq = Some(1_000_042);
            frame.data = Some(BinaryBlob(record_bytes));
            frame
        }
    };

    let mut encoded = Vec::new();
    ciborium::ser::into_writer(&native, &mut encoded)
        .map_err(|error| RemoteEnvelopeError::Cbor(format!("{error:?}")))?;
    enforce_frame_bound(encoded.len())?;

    native = ciborium::de::from_reader(encoded.as_slice())
        .map_err(|error| RemoteEnvelopeError::Cbor(format!("{error:?}")))?;
    let payload = match operation {
        EnvelopeOperation::Request
            if native.t == "req"
                && native.id == Some(42)
                && native.method.as_deref() == Some("record.set") =>
        {
            native.params
        }
        EnvelopeOperation::Write
            if native.t == "write" && native.topic.as_deref() == Some("plant/line-7/state") =>
        {
            native.payload
        }
        EnvelopeOperation::Event
            if native.t == "event"
                && native.sub.as_deref() == Some("sub-42")
                && native.seq == Some(1_000_042) =>
        {
            native.data
        }
        _ => return Err(RemoteEnvelopeError::UnexpectedMessage("native CBOR tag")),
    }
    .ok_or(RemoteEnvelopeError::UnexpectedMessage(
        "native CBOR payload field",
    ))?;

    let record = ciborium::de::from_reader(payload.0.as_slice())
        .map_err(|error| RemoteEnvelopeError::Cbor(format!("{error:?}")))?;
    Ok(EnvelopeObservation {
        frame_bytes: encoded.len(),
        record: DynamicRecord::Cbor(record),
    })
}

fn enforce_frame_bound(bytes: usize) -> Result<(), RemoteEnvelopeError> {
    if bytes > PROTOTYPE_MAX_FRAME_BYTES {
        Err(RemoteEnvelopeError::FrameTooLarge(bytes))
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::remote_ir::{ByteHeavySample, NestedState, NumericTelemetry};

    #[test]
    fn both_envelopes_round_trip_all_shapes_and_flows() {
        verify_envelope_round_trips::<NumericTelemetry>().expect("numeric envelopes");
        verify_envelope_round_trips::<NestedState>().expect("nested envelopes");
        verify_envelope_round_trips::<ByteHeavySample>().expect("byte envelopes");
    }

    #[test]
    fn every_frame_is_non_empty_and_bounded() {
        for (_, _, bytes) in encoded_frame_sizes::<ByteHeavySample>().expect("frame sizes") {
            assert!(bytes > 0);
            assert!(bytes <= PROTOTYPE_MAX_FRAME_BYTES);
        }
    }

    #[test]
    fn oversized_frames_are_rejected_before_decode() {
        let error = enforce_frame_bound(PROTOTYPE_MAX_FRAME_BYTES + 1)
            .expect_err("oversized frame must fail");
        assert!(matches!(error, RemoteEnvelopeError::FrameTooLarge(_)));
    }
}
