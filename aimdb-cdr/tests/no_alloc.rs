//! Runs on every feature set: borrowed strings and fixed arrays only.

use aimdb_cdr::{from_bytes, to_slice};
use serde::{Deserialize, Serialize};

#[derive(Debug, PartialEq, Serialize, Deserialize)]
struct Reading<'a> {
    id: u16,
    label: &'a str,
    samples: [i32; 2],
}

#[test]
fn round_trips_without_allocating() {
    let value = Reading {
        id: 7,
        label: "t",
        samples: [-1, 2],
    };
    let mut out = [0u8; 32];
    let len = to_slice(&value, &mut out).unwrap();
    #[rustfmt::skip]
    let expected = [
        0, 1, 0, 0,
        7, 0, 0, 0,                 // id, pad to 4
        2, 0, 0, 0, b't', 0, 0, 0,  // label, pad to 4
        0xff, 0xff, 0xff, 0xff,     // samples[0]
        2, 0, 0, 0,                 // samples[1]
    ];
    assert_eq!(&out[..len], expected);
    assert_eq!(from_bytes::<Reading>(&out[..len]).unwrap(), value);
}

#[test]
fn consumed_length_includes_header_and_alignment_but_not_trailing_bytes() {
    let value = Reading {
        id: 7,
        label: "t",
        samples: [-1, 2],
    };
    let mut out = [0xa5; 40];
    let len = to_slice(&value, &mut out).unwrap();
    let (decoded, used) = aimdb_cdr::from_bytes_with_consumed::<Reading>(&out).unwrap();
    assert_eq!(decoded, value);
    assert_eq!(used, len);
    assert_eq!(&out[used..], &[0xa5; 16]);
    // Borrowed strings still point into the supplied frame.
    assert_eq!(decoded.label.as_ptr(), out[12..].as_ptr());
    assert_eq!(from_bytes::<Reading>(&out).unwrap(), value);
    for end in 0..len {
        assert!(aimdb_cdr::from_bytes_with_consumed::<Reading>(&out[..end]).is_err());
    }
}

#[test]
fn consumed_length_handles_big_endian_and_empty_body() {
    assert_eq!(
        aimdb_cdr::from_bytes_with_consumed::<u32>(&[0, 0, 0, 0, 0, 0, 0, 7, 99]).unwrap(),
        (7, 8)
    );
    assert_eq!(
        aimdb_cdr::from_bytes_with_consumed::<()>(&[0, 1, 0, 0, 99]).unwrap(),
        ((), 4)
    );
    assert!(aimdb_cdr::from_bytes_with_consumed::<()>(&[0, 2, 0, 0]).is_err());
}
