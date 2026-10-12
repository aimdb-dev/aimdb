//! Randomized checks: decoding arbitrary bytes never panics, and random
//! values round-trip.

#![cfg(feature = "alloc")]

mod common;

use aimdb_cdr::{from_bytes, to_vec};
use common::*;

#[test]
fn decoding_random_and_mutated_input_never_panics() {
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    for _ in 0..20_000 {
        let mut bytes: Vec<u8> = if rng.below(2) == 0 {
            (0..rng.below(96)).map(|_| rng.next() as u8).collect()
        } else {
            let mut golden = MIXED_LE.to_vec();
            for _ in 0..=rng.below(4) {
                let at = rng.below(golden.len() as u64) as usize;
                golden[at] = rng.next() as u8;
            }
            golden.truncate(rng.below(golden.len() as u64 + 1) as usize);
            golden
        };
        if rng.below(2) == 0 && bytes.len() >= 2 {
            bytes[0] = 0;
            bytes[1] = rng.below(2) as u8;
        }
        let _ = from_bytes::<Mixed>(&bytes);
        let _ = from_bytes::<Temperature>(&bytes);
        let _ = from_bytes::<Vec<String>>(&bytes);
    }
}

#[test]
fn random_values_round_trip() {
    let mut rng = Rng(42);
    for _ in 0..2_000 {
        let value = random_mixed(&mut rng);
        assert_eq!(
            from_bytes::<Mixed>(&to_vec(&value).unwrap()).unwrap(),
            value
        );
    }
}

pub fn random_mixed(rng: &mut Rng) -> Mixed {
    Mixed {
        a: rng.next() as u8,
        b: rng.next() as u32,
        c: rng.next() as u16,
        d: rng.next() as i32 as f64 / 7.0,
        s: (0..rng.below(12))
            .map(|_| char::from(b'a' + rng.below(26) as u8))
            .collect(),
        v: (0..rng.below(6)).map(|_| rng.next() as u16).collect(),
        arr: [rng.next() as u8, rng.next() as u8, rng.next() as u8],
        flag: rng.below(2) == 1,
    }
}
