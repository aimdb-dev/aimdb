#![no_std]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;

use serde::{Deserialize, Serialize};

/// Same representative nested shape in both linked images. The fixture is
/// intentionally small enough for Cortex-M while still exercising strings,
/// arrays, options, integers, and floats.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EmbeddedState {
    pub device_id: String,
    pub sequence: u64,
    pub readings: [f32; 4],
    pub flags: Vec<bool>,
    pub note: Option<String>,
}

pub fn sample() -> EmbeddedState {
    EmbeddedState {
        device_id: String::from("edge-node-042"),
        sequence: 1_000_042,
        readings: [23.625, 48.125, 0.03125, 1_013.25],
        flags: alloc::vec![true, true, false, true],
        note: Some(String::from("calibration-window")),
    }
}
