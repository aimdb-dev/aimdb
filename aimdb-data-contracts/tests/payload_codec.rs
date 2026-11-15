//! Generic operations preserve ordinary LinkCodec decoding while exposing remainders.
#![cfg(any(
    feature = "linkable-json",
    feature = "linkable-postcard",
    feature = "linkable-cdr"
))]

use aimdb_core::connector::SerializeError;
use aimdb_data_contracts::{link_codecs, LinkCodec, PayloadDecoder, PayloadEncoder};
use serde::{Deserialize, Serialize};

#[derive(Debug, PartialEq, Serialize, Deserialize)]
struct OldReading {
    value: f32,
}
#[derive(Debug, PartialEq, Serialize, Deserialize)]
struct Reading {
    value: f32,
    sequence: u32,
}

fn check_codec<C>(codec: C)
where
    C: PayloadDecoder + PayloadEncoder + LinkCodec<Reading>,
{
    let value = Reading {
        value: 21.5,
        sequence: 128,
    };
    let expected = codec.encode(&value).unwrap();
    let mut out = vec![0xa5; expected.len() + 4];
    let len = codec.encode_payload(&value, &mut out).unwrap();
    assert_eq!(&out[..len], expected);
    assert_eq!(&out[len..], &[0xa5; 4]);
    assert_eq!(
        codec.decode_payload::<Reading>(&out[..len]).unwrap(),
        (value, len)
    );
    let (decoded, used) = codec.decode_payload::<Reading>(&out).unwrap();
    assert_eq!(decoded.value, 21.5);
    assert_eq!(
        used, len,
        "must expose trailing data, not consume the whole slice"
    );
    assert_eq!(
        codec.encode_payload(&decoded, &mut out[..len - 1]),
        Err(SerializeError::BufferTooSmall)
    );
    assert!(codec
        .decode_payload::<Reading>(&expected[..expected.len() - 1])
        .is_err());
    assert!(codec.decode_payload::<Reading>(&[]).is_err());

    // One codec decodes distinct schema versions; no type-specific adapter or probe.
    let old = OldReading { value: 19.0 };
    let mut storage = [0; 64];
    let n = codec.encode_payload(&old, &mut storage).unwrap();
    assert_eq!(
        codec.decode_payload::<OldReading>(&storage[..n]).unwrap(),
        (old, n)
    );

    // Encoding must not require Deserialize for a type that is only sent.
    #[derive(Serialize)]
    struct WriteOnly {
        value: u32,
    }
    assert!(codec
        .encode_payload(&WriteOnly { value: 7 }, &mut storage)
        .is_ok());

    struct Invalid;
    impl Serialize for Invalid {
        fn serialize<S: serde::Serializer>(&self, _: S) -> Result<S::Ok, S::Error> {
            Err(serde::ser::Error::custom("cannot encode"))
        }
    }
    assert_eq!(
        codec.encode_payload(&Invalid, &mut storage),
        Err(SerializeError::InvalidData)
    );
}

#[cfg(feature = "linkable-json")]
#[test]
fn json_consumption_and_whitespace() {
    check_codec(link_codecs::Json);
    let codec = link_codecs::Json;
    assert_eq!(codec.decode_payload::<u32>(b" \t42\r\n ").unwrap(), (42, 7));
    assert_eq!(codec.decode_payload::<u32>(b"42 43").unwrap(), (42, 3));
    assert_eq!(codec.decode_payload::<u32>(b"42 garbage").unwrap(), (42, 3));
    assert!(codec.decode_payload::<u32>(b" \n ").is_err());
    assert!(codec.decode_payload::<u32>(b"42x").is_err());
    // LinkCodec keeps its existing strict single-document semantics.
    assert!(LinkCodec::<u32>::decode(&codec, b"42 43").is_err());
    assert_eq!(LinkCodec::<u32>::decode(&codec, b"42 \n").unwrap(), 42);
}

#[cfg(feature = "linkable-postcard")]
#[test]
fn postcard_reports_remainder_without_changing_link_codec() {
    let codec = link_codecs::Postcard::<64>;
    check_codec(codec);
    let bytes = [0, 0, 0xac, 0x41, 0xff]; // f32 21.5 followed by an extra byte
    assert_eq!(
        codec.decode_payload::<OldReading>(&bytes).unwrap(),
        (OldReading { value: 21.5 }, 4)
    );
    assert_eq!(
        LinkCodec::<OldReading>::decode(&codec, &bytes).unwrap(),
        OldReading { value: 21.5 }
    );
}

#[cfg(feature = "linkable-cdr")]
#[test]
fn cdr_reports_remainder_in_both_byte_orders() {
    let codec = link_codecs::Cdr::<64>;
    check_codec(codec);
    let bytes = [0, 0, 0, 0, 0x41, 0xac, 0, 0, 0xff];
    assert_eq!(
        codec.decode_payload::<OldReading>(&bytes).unwrap(),
        (OldReading { value: 21.5 }, 8)
    );
    assert_eq!(
        LinkCodec::<OldReading>::decode(&codec, &bytes).unwrap(),
        OldReading { value: 21.5 }
    );
    assert!(codec
        .decode_payload::<OldReading>(&[0, 2, 0, 0, 0, 0, 0, 0])
        .is_err());
}
