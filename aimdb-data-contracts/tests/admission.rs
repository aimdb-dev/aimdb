//! Admission ordering and carrier policy, independent of the transport.
#![cfg(feature = "migratable")]

#[path = "fixtures/admission_contracts.rs"]
mod contracts;
use aimdb_core::envelope::{ContractRef, Envelope, HeaderWriter, PayloadCodec};
use aimdb_core::{IngestError, Rejection};
use aimdb_data_contracts::{
    admit, admit_frame, name_id, LinkContract, MigrationChain, PayloadDecoder,
};
use contracts::{V1, V2};
#[cfg(feature = "linkable-postcard")]
use serde::{Deserialize, Serialize};
#[cfg(any(
    feature = "linkable-json",
    feature = "linkable-postcard",
    feature = "linkable-cdr"
))]
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

fn identity(version: u32) -> ContractRef {
    ContractRef::new(name_id("temperature"), version, V2::shape_of(version))
}
fn frame(contract: ContractRef, codec: PayloadCodec, payload: &[u8]) -> Vec<u8> {
    let mut header = [0; 19];
    let n = HeaderWriter::new(&mut header)
        .contract(contract)
        .codec(codec)
        .finish()
        .unwrap();
    let mut frame = header[..n].to_vec();
    frame.extend_from_slice(payload);
    frame
}

struct NeverDecode;
impl PayloadDecoder for NeverDecode {
    fn decode_payload<T: serde::de::DeserializeOwned>(
        &self,
        _: &[u8],
    ) -> Result<(T, usize), String> {
        panic!("metadata rejection must precede decoding")
    }
}
fn assert_reason<T: core::fmt::Debug>(
    result: Result<T, IngestError>,
    reason: Rejection,
) -> IngestError {
    let error = result.unwrap_err();
    assert_eq!(error.reason(), reason, "{error}");
    error
}

#[test]
fn header_failures_are_countable_before_admission() {
    let link = LinkContract::<V2, _>::inline(PayloadCodec::Postcard, NeverDecode);
    for bytes in [
        &b""[..],
        &b"{}"[..],
        &[0xa1],
        &[0xa1, 0],
        &[0xa1, 0, 0, 0, 0, 0, 0x80],
    ] {
        assert_reason(admit_frame(bytes, &link), Rejection::Malformed);
    }
    for bytes in [&[0xa2][..], &[0xa1, 8], &[0xa1, 0x40]] {
        assert_reason(admit_frame(bytes, &link), Rejection::UnsupportedFormat);
    }
}

#[test]
fn metadata_failures_follow_the_specified_precedence() {
    let link = LinkContract::<V2, _>::inline(PayloadCodec::Postcard, NeverDecode);
    let wrong_name = ContractRef::new(name_id("humidity"), 99, Some(0));
    let err = assert_reason(
        admit_frame(&frame(wrong_name, PayloadCodec::Cdr, b"bad"), &link),
        Rejection::ContractMismatch,
    );
    assert!(err
        .message()
        .contains(&format!("{:#010x}", name_id("humidity"))));
    assert!(err
        .message()
        .contains(&format!("{:#010x}", name_id("temperature"))));
    let err = assert_reason(
        admit_frame(
            &frame(
                ContractRef::new(name_id("temperature"), 3, Some(0)),
                PayloadCodec::Cdr,
                b"bad",
            ),
            &link,
        ),
        Rejection::TooNew,
    );
    assert!(err
        .message()
        .contains("received temperature@3, accepts temperature@2..=2"));
    let err = assert_reason(
        admit_frame(
            &frame(
                ContractRef::new(name_id("temperature"), 1, Some(0)),
                PayloadCodec::Cdr,
                b"bad",
            ),
            &link,
        ),
        Rejection::TooOld,
    );
    assert!(err
        .message()
        .contains("received temperature@1, accepts temperature@2..=2"));
    let link = LinkContract::<V2, _>::inline_from::<1>(PayloadCodec::Postcard, NeverDecode);
    for version in [1, 2] {
        let bad_shape = ContractRef::new(name_id("temperature"), version, Some(0xffff));
        let err = assert_reason(
            admit_frame(&frame(bad_shape, PayloadCodec::Cdr, b"bad"), &link),
            Rejection::ShapeMismatch,
        );
        assert!(err.message().contains("0x000000000000ffff"));
        assert!(err
            .message()
            .contains(&format!("{:#018x}", V2::shape_of(version).unwrap())));
        assert_reason(
            admit_frame(&frame(identity(version), PayloadCodec::Cdr, b"bad"), &link),
            Rejection::CodecMismatch,
        );
    }
    let link = LinkContract::<V2, _>::bound::<V1>(PayloadCodec::Postcard, NeverDecode);
    assert_eq!((link.accepted_min(), link.accepted_max()), (1, 1));
    assert_reason(
        admit(
            &Envelope::bound(identity(2), PayloadCodec::Postcard, b"bad"),
            &link,
        ),
        Rejection::TooNew,
    );
}

#[cfg(any(
    feature = "linkable-json",
    feature = "linkable-postcard",
    feature = "linkable-cdr"
))]
struct Counting<C> {
    codec: C,
    calls: Arc<AtomicUsize>,
}
#[cfg(any(
    feature = "linkable-json",
    feature = "linkable-postcard",
    feature = "linkable-cdr"
))]
impl<C: PayloadDecoder> PayloadDecoder for Counting<C> {
    fn decode_payload<T: serde::de::DeserializeOwned>(
        &self,
        bytes: &[u8],
    ) -> Result<(T, usize), String> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.codec.decode_payload(bytes)
    }
}

#[cfg(any(
    feature = "linkable-json",
    feature = "linkable-postcard",
    feature = "linkable-cdr"
))]
fn exercise<C: PayloadDecoder + aimdb_data_contracts::PayloadEncoder + Clone>(
    codec: C,
    id: PayloadCodec,
) {
    let calls = Arc::new(AtomicUsize::new(0));
    let decoder = Counting {
        codec: codec.clone(),
        calls: calls.clone(),
    };
    let inline = LinkContract::<V2, _>::inline_from::<1>(id, decoder);
    let bound = LinkContract::<V2, _>::bound::<V1>(id, codec.clone());
    let mut out = [0; 128];
    let n = codec.encode_payload(&V1 { temp: 21.5 }, &mut out).unwrap();
    let payload = &out[..n];
    for shape in [V2::shape_of(1), None] {
        for header_codec in [id, PayloadCodec::Unspecified] {
            let result = admit_frame(
                &frame(
                    ContractRef::new(name_id("temperature"), 1, shape),
                    header_codec,
                    payload,
                ),
                &inline,
            )
            .unwrap();
            assert_eq!(
                result.value(),
                &V2 {
                    celsius: 21.5,
                    quality: 1
                }
            );
            assert_eq!(result.outcome().upgraded_from(), Some(1));
        }
    }
    assert_eq!(calls.load(Ordering::SeqCst), 4);
    let result = admit_frame(payload, &bound).unwrap();
    assert_eq!(result.outcome().upgraded_from(), Some(1));
    assert_eq!(
        result.into_value(),
        V2 {
            celsius: 21.5,
            quality: 1
        }
    );
    // Inserting the binary header into a bare link must not admit a bogus value.
    assert_reason(
        admit_frame(&frame(identity(1), id, payload), &bound),
        Rejection::DecodeError,
    );
    assert_reason(admit_frame(payload, &inline), Rejection::Malformed);
    let n = codec
        .encode_payload(
            &V2 {
                celsius: 22.0,
                quality: 2,
            },
            &mut out,
        )
        .unwrap();
    let result = admit_frame(&frame(identity(2), id, &out[..n]), &inline).unwrap();
    assert_eq!(result.value().quality, 2);
    assert_eq!(result.outcome().upgraded_from(), None);
    let current_bound = LinkContract::<V2, _>::bound_current(id, codec.clone());
    assert!(admit_frame(&out[..n], &current_bound).is_ok());
    assert_eq!(
        calls.load(Ordering::SeqCst),
        5,
        "exactly one decode per admitted inline value"
    );
    assert_reason(
        admit_frame(&frame(identity(2), id, b""), &inline),
        Rejection::DecodeError,
    );
    let n = codec
        .encode_payload(&V1 { temp: -300.0 }, &mut out)
        .unwrap();
    let err = assert_reason(
        admit_frame(&frame(identity(1), id, &out[..n]), &inline),
        Rejection::DecodeError,
    );
    assert!(err.message().contains("below absolute zero"));
    // A shape is checked against the historical type, not the current type.
    assert_reason(
        admit_frame(
            &frame(
                ContractRef::new(name_id("temperature"), 1, V2::shape_of(2)),
                id,
                &out[..n],
            ),
            &inline,
        ),
        Rejection::ShapeMismatch,
    );
}

#[cfg(feature = "linkable-postcard")]
#[test]
fn postcard_inline_and_bound_admission() {
    exercise(
        aimdb_data_contracts::link_codecs::Postcard::<128>,
        PayloadCodec::Postcard,
    );
}
#[cfg(feature = "linkable-cdr")]
#[test]
fn cdr_inline_and_bound_admission() {
    exercise(
        aimdb_data_contracts::link_codecs::Cdr::<128>,
        PayloadCodec::Cdr,
    );
}
#[cfg(feature = "linkable-json")]
#[test]
fn json_inline_and_bound_admission() {
    exercise(aimdb_data_contracts::link_codecs::Json, PayloadCodec::Json);
}

#[cfg(feature = "linkable-postcard")]
#[test]
fn postcard_rejects_the_inline_mismatch_and_trailing_bytes_on_every_carrier() {
    use aimdb_data_contracts::{link_codecs::Postcard, LinkCodec};
    let codec = Postcard::<128>;
    let payload = 21.5_f32.to_le_bytes();
    let inline = frame(identity(1), PayloadCodec::Postcard, &payload);
    // This is the original spike failure: ordinary Postcard silently ignores remainder.
    assert!(LinkCodec::<V1>::decode(&codec, &inline).is_ok());
    let bound = LinkContract::<V2, _>::bound::<V1>(PayloadCodec::Postcard, codec);
    assert_reason(admit_frame(&inline, &bound), Rejection::DecodeError);
    let mut payload = payload.to_vec();
    payload.push(99);
    assert_reason(admit_frame(&payload, &bound), Rejection::DecodeError);
    // Inline links follow the same rule: one frame, one value.
    let inline_link = LinkContract::<V2, _>::inline_from::<1>(PayloadCodec::Postcard, codec);
    assert_reason(
        admit_frame(
            &frame(identity(1), PayloadCodec::Postcard, &payload),
            &inline_link,
        ),
        Rejection::DecodeError,
    );
    assert!(admit_frame(&inline, &inline_link).is_ok());
}

#[cfg(feature = "linkable-json")]
#[test]
fn deployed_json_and_whitespace_need_no_probe() {
    use aimdb_data_contracts::link_codecs::Json;
    let calls = Arc::new(AtomicUsize::new(0));
    let bound = LinkContract::<V2, _>::bound::<V1>(
        PayloadCodec::Json,
        Counting {
            codec: Json,
            calls: calls.clone(),
        },
    );
    let result = admit_frame(b" {\"schema_version\":999,\"temp\":21.5} \r\n", &bound).unwrap();
    assert_eq!(result.value().celsius, 21.5);
    assert_eq!(result.outcome().upgraded_from(), Some(1));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_reason(
        admit_frame(b"{\"temp\":21.5} {}", &bound),
        Rejection::DecodeError,
    );
}

#[cfg(feature = "linkable-postcard")]
#[test]
fn missing_locked_shape_skips_only_the_shape_check() {
    #[derive(Serialize, Deserialize)]
    struct Unlocked {
        temp: f32,
    }
    impl aimdb_data_contracts::SchemaType for Unlocked {
        const NAME: &'static str = "unlocked";
    }
    impl aimdb_data_contracts::ContractShape for Unlocked {}
    aimdb_data_contracts::migration_chain! { type Current = Unlocked; steps {} }
    let codec = aimdb_data_contracts::link_codecs::Postcard::<128>;
    let link = LinkContract::<Unlocked, _>::inline(PayloadCodec::Postcard, codec);
    let bytes = frame(
        ContractRef::new(name_id("unlocked"), 1, Some(999)),
        PayloadCodec::Postcard,
        &21.5_f32.to_le_bytes(),
    );
    assert_eq!(admit_frame(&bytes, &link).unwrap().value().temp, 21.5);
}

#[test]
fn invalid_decoder_consumption_is_a_rejection() {
    // A custom decoder is not allowed to claim bytes outside the supplied frame.
    struct InvalidDecoder;
    impl PayloadDecoder for InvalidDecoder {
        fn decode_payload<T: serde::de::DeserializeOwned>(
            &self,
            _: &[u8],
        ) -> Result<(T, usize), String> {
            serde_json::from_slice(b"{\"celsius\":21.5,\"quality\":1}")
                .map(|value| (value, usize::MAX))
                .map_err(|e| e.to_string())
        }
    }
    let link = LinkContract::<V2, _>::inline(PayloadCodec::Json, InvalidDecoder);
    assert_reason(
        admit_frame(&frame(identity(2), PayloadCodec::Json, b""), &link),
        Rejection::DecodeError,
    );
}
