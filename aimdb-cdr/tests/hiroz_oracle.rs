//! Little-endian output must match hiroz-cdr, an independent ROS 2 encoder.

#![cfg(feature = "alloc")]

mod common;

use aimdb_cdr::{from_bytes, to_vec, HEADER_LE};
use common::*;
use hiroz_cdr::LittleEndian;
use serde::{de::DeserializeOwned, Serialize};

fn assert_matches_hiroz<T: Serialize + DeserializeOwned + PartialEq + core::fmt::Debug>(value: &T) {
    let ours = to_vec(value).unwrap();
    let theirs = hiroz_cdr::to_vec::<T, LittleEndian>(value, 64).unwrap();
    assert_eq!(&ours[..4], HEADER_LE);
    assert_eq!(
        &ours[4..],
        theirs.as_slice(),
        "encoding differs for {value:?}"
    );

    let mut framed = HEADER_LE.to_vec();
    framed.extend_from_slice(&theirs);
    assert_eq!(&from_bytes::<T>(&framed).unwrap(), value);
}

#[test]
fn golden_fixtures_match_hiroz() {
    assert_matches_hiroz(&mixed());
    assert_matches_hiroz(&temperature());
}

#[test]
fn random_values_match_hiroz() {
    let mut rng = Rng(7);
    for _ in 0..500 {
        let mut value = temperature();
        value.header.stamp.sec = rng.next() as i32;
        value.header.frame_id = "f".repeat(rng.below(9) as usize);
        value.temperature = rng.next() as i32 as f64 / 3.0;
        assert_matches_hiroz(&value);

        let mixed = Mixed {
            s: "s".repeat(rng.below(9) as usize),
            v: (0..rng.below(5)).map(|_| rng.next() as u16).collect(),
            d: rng.next() as i32 as f64,
            ..mixed()
        };
        assert_matches_hiroz(&mixed);
    }
}
