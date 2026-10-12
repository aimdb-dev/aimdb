//! Shared fixtures. Golden bytes are laid out by hand from the XCDR1 rules.

#![allow(dead_code)]

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Mixed {
    pub a: u8,
    pub b: u32,
    pub c: u16,
    pub d: f64,
    pub s: String,
    pub v: Vec<u16>,
    pub arr: [u8; 3],
    pub flag: bool,
}

pub fn mixed() -> Mixed {
    Mixed {
        a: 0x11,
        b: 0x2233_4455,
        c: 0x6677,
        d: 1.0,
        s: "hi".into(),
        v: vec![1, 2],
        arr: [7, 8, 9],
        flag: true,
    }
}

#[rustfmt::skip]
pub const MIXED_LE: [u8; 48] = [
    0x00, 0x01, 0x00, 0x00,                         // CDR_LE header
    0x11, 0, 0, 0,                                  // a, pad to 4
    0x55, 0x44, 0x33, 0x22,                         // b
    0x77, 0x66, 0, 0, 0, 0, 0, 0,                   // c, pad to 8
    0, 0, 0, 0, 0, 0, 0xf0, 0x3f,                   // d = 1.0
    3, 0, 0, 0, b'h', b'i', 0, 0,                   // s: len incl. NUL, pad
    2, 0, 0, 0, 1, 0, 2, 0,                         // v: count, elements
    7, 8, 9,                                        // arr: no count
    1,                                              // flag
];

#[rustfmt::skip]
pub const MIXED_BE: [u8; 48] = [
    0x00, 0x00, 0x00, 0x00,                         // CDR_BE header
    0x11, 0, 0, 0,
    0x22, 0x33, 0x44, 0x55,
    0x66, 0x77, 0, 0, 0, 0, 0, 0,
    0x3f, 0xf0, 0, 0, 0, 0, 0, 0,
    0, 0, 0, 3, b'h', b'i', 0, 0,
    0, 0, 0, 2, 0, 1, 0, 2,
    7, 8, 9,
    1,
];

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Time {
    pub sec: i32,
    pub nanosec: u32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Header {
    pub stamp: Time,
    pub frame_id: String,
}

/// `sensor_msgs/msg/Temperature`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Temperature {
    pub header: Header,
    pub temperature: f64,
    pub variance: f64,
}

pub fn temperature() -> Temperature {
    Temperature {
        header: Header {
            stamp: Time { sec: 1, nanosec: 2 },
            frame_id: "a".into(),
        },
        temperature: 0.5,
        variance: 0.0,
    }
}

#[rustfmt::skip]
pub const TEMPERATURE_LE: [u8; 36] = [
    0x00, 0x01, 0x00, 0x00,
    1, 0, 0, 0,                                     // stamp.sec
    2, 0, 0, 0,                                     // stamp.nanosec
    2, 0, 0, 0, b'a', 0, 0, 0,                      // frame_id, pad to 8
    0, 0, 0, 0, 0, 0, 0xe0, 0x3f,                   // temperature = 0.5
    0, 0, 0, 0, 0, 0, 0, 0,                         // variance
];

/// Tiny xorshift PRNG so the randomized tests need no extra dependency.
pub struct Rng(pub u64);

impl Rng {
    pub fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    pub fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}
