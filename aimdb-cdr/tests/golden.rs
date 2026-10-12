#![cfg(feature = "alloc")]

mod common;

use aimdb_cdr::{from_bytes, to_slice, to_vec, Error};
use common::*;
use serde::{Deserialize, Serialize};

#[test]
fn encodes_golden_little_endian() {
    assert_eq!(to_vec(&mixed()).unwrap(), MIXED_LE);
    assert_eq!(to_vec(&temperature()).unwrap(), TEMPERATURE_LE);
}

#[test]
fn decodes_both_byte_orders() {
    assert_eq!(from_bytes::<Mixed>(&MIXED_LE).unwrap(), mixed());
    assert_eq!(from_bytes::<Mixed>(&MIXED_BE).unwrap(), mixed());
    assert_eq!(
        from_bytes::<Temperature>(&TEMPERATURE_LE).unwrap(),
        temperature()
    );
}

#[test]
fn to_slice_matches_to_vec_and_reports_small_buffers() {
    let mut out = [0u8; 48];
    assert_eq!(to_slice(&mixed(), &mut out), Ok(48));
    assert_eq!(out, MIXED_LE);

    let mut short = [0u8; 47];
    assert_eq!(to_slice(&mixed(), &mut short), Err(Error::BufferTooSmall));
    assert_eq!(
        to_slice(&mixed(), &mut [0u8; 3]),
        Err(Error::BufferTooSmall)
    );
}

#[test]
fn empty_string_and_sequence() {
    #[derive(Debug, PartialEq, Serialize, Deserialize)]
    struct Empty {
        s: String,
        v: Vec<u32>,
    }
    let value = Empty {
        s: String::new(),
        v: Vec::new(),
    };
    let bytes = to_vec(&value).unwrap();
    assert_eq!(bytes, [0, 1, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    assert_eq!(from_bytes::<Empty>(&bytes).unwrap(), value);
}

#[test]
fn nested_sequences_of_structs() {
    #[derive(Debug, PartialEq, Serialize, Deserialize)]
    struct Path {
        poses: Vec<Time>,
        names: Vec<String>,
    }
    let value = Path {
        poses: vec![
            Time {
                sec: -1,
                nanosec: 9,
            },
            Time { sec: 3, nanosec: 4 },
        ],
        names: vec!["x".into(), "yz".into()],
    };
    assert_eq!(from_bytes::<Path>(&to_vec(&value).unwrap()).unwrap(), value);
}

#[test]
fn rejects_malformed_input() {
    assert_eq!(from_bytes::<u32>(&[0, 1, 0]), Err(Error::UnexpectedEof));
    assert_eq!(
        from_bytes::<u32>(&[0, 2, 0, 0, 1, 0, 0, 0]),
        Err(Error::InvalidHeader([0, 2]))
    );
    assert_eq!(
        from_bytes::<bool>(&[0, 1, 0, 0, 2]),
        Err(Error::InvalidBool(2))
    );
    assert_eq!(
        from_bytes::<String>(&[0, 1, 0, 0, 2, 0, 0, 0, b'a', b'b']),
        Err(Error::MissingNul)
    );
    assert_eq!(
        from_bytes::<String>(&[0, 1, 0, 0, 2, 0, 0, 0, 0xff, 0]),
        Err(Error::InvalidUtf8)
    );
    // A huge count is refused before anything allocates.
    assert_eq!(
        from_bytes::<Vec<u64>>(&[0, 1, 0, 0, 0xff, 0xff, 0xff, 0xff]),
        Err(Error::UnexpectedEof)
    );
    assert_eq!(
        from_bytes::<Mixed>(&MIXED_LE[..40]),
        Err(Error::UnexpectedEof)
    );
}

#[test]
fn rejects_constructs_without_a_cdr_mapping() {
    assert_eq!(to_vec(&Some(1u8)), Err(Error::Unsupported("Option")));
    assert_eq!(to_vec(&'x'), Err(Error::Unsupported("char")));
    #[derive(Serialize)]
    enum E {
        A,
    }
    assert_eq!(to_vec(&E::A), Err(Error::Unsupported("enum")));
}

#[test]
fn ignores_trailing_bytes() {
    let mut padded = TEMPERATURE_LE.to_vec();
    padded.extend_from_slice(&[0, 0, 0, 0]);
    assert_eq!(from_bytes::<Temperature>(&padded).unwrap(), temperature());
}
