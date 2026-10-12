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
