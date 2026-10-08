//! Behavioral coverage for `#[derive(RosMessage)]`: one struct per row of the
//! ROS-to-Rust type mapping, checked against golden CDR bytes.

#![cfg(feature = "ros2")]

use aimdb_core::connector::SerializeError;
use aimdb_data_contracts::{ros2, Linkable, RosMessage, SchemaType, WireFormat};
use serde::{Deserialize, Serialize};

const HASH: &str = "RIHS01_00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff";
const HEADER: [u8; 4] = [0x00, 0x01, 0x00, 0x00];

fn golden(body: &[u8]) -> Vec<u8> {
    let mut bytes = HEADER.to_vec();
    bytes.extend_from_slice(body);
    bytes
}

/// Encodes through both paths, checks them against `body`, and decodes back.
fn assert_golden<T: Linkable + PartialEq + core::fmt::Debug>(value: &T, body: &[u8]) {
    let expected = golden(body);
    assert_eq!(value.to_bytes().expect("to_bytes"), expected, "owned path");

    let mut buf = vec![0xA5; T::ENCODE_BUFFER_CAPACITY.expect("bounded")];
    let written = value.encode_into(&mut buf).expect("encode_into");
    assert_eq!(&buf[..written], expected.as_slice(), "bounded path");

    assert_eq!(&T::from_bytes(&expected).expect("from_bytes"), value);
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, RosMessage)]
#[ros(
    type = "test_msgs/msg/Flag",
    hash = "RIHS01_00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff"
)]
struct Flag {
    flag: bool,
}

#[test]
fn bool_is_one_byte() {
    assert_golden(&Flag { flag: true }, &[0x01]);
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, RosMessage)]
#[ros(
    type = "test_msgs/msg/Bytes",
    hash = "RIHS01_00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff"
)]
struct Bytes {
    byte: u8,
    int8: i8,
}

#[test]
fn byte_char_uint8_int8_are_one_byte() {
    assert_golden(
        &Bytes {
            byte: 0xFE,
            int8: -2,
        },
        &[0xFE, 0xFE],
    );
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, RosMessage)]
#[ros(
    type = "test_msgs/msg/Integers",
    hash = "RIHS01_00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff"
)]
struct Integers {
    lead: u8,
    uint16: u16,
    int32: i32,
    uint64: u64,
}

#[test]
fn integers_are_little_endian_and_aligned_from_the_header() {
    let mut body = vec![0x01, 0x00]; // lead, pad to 2
    body.extend_from_slice(&0x0203_u16.to_le_bytes()); // offset 2
    body.extend_from_slice(&(-4_i32).to_le_bytes()); // offset 4
    body.extend_from_slice(&0x0506_0708_090A_0B0C_u64.to_le_bytes()); // offset 8
    assert_golden(
        &Integers {
            lead: 1,
            uint16: 0x0203,
            int32: -4,
            uint64: 0x0506_0708_090A_0B0C,
        },
        &body,
    );
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, RosMessage)]
#[ros(
    type = "test_msgs/msg/Floats",
    hash = "RIHS01_00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff"
)]
struct Floats {
    float32: f32,
    float64: f64,
}

#[test]
fn float64_aligns_to_eight() {
    let mut body = 1.5_f32.to_le_bytes().to_vec();
    body.extend_from_slice(&[0; 4]);
    body.extend_from_slice(&(-2.25_f64).to_le_bytes());
    assert_golden(
        &Floats {
            float32: 1.5,
            float64: -2.25,
        },
        &body,
    );
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, RosMessage)]
#[ros(
    type = "test_msgs/msg/Label",
    hash = "RIHS01_00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff"
)]
struct Label {
    lead: u8,
    text: String,
}

#[test]
fn string_is_a_u32_length_including_the_nul() {
    let mut body = vec![0x07, 0, 0, 0]; // lead, pad to 4
    body.extend_from_slice(&3_u32.to_le_bytes());
    body.extend_from_slice(b"hi\0");
    assert_golden(
        &Label {
            lead: 7,
            text: "hi".into(),
        },
        &body,
    );
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, RosMessage)]
#[ros(
    type = "test_msgs/msg/Fixed",
    hash = "RIHS01_00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff"
)]
struct Fixed {
    values: [u16; 3],
}

#[test]
fn fixed_array_has_no_count() {
    assert_golden(
        &Fixed { values: [1, 2, 3] },
        &[0x01, 0x00, 0x02, 0x00, 0x03, 0x00],
    );
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, RosMessage)]
#[ros(
    type = "test_msgs/msg/Samples",
    hash = "RIHS01_00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff"
)]
struct Samples {
    values: Vec<u32>,
}

#[test]
fn sequence_is_a_u32_count_then_elements() {
    let mut body = 2_u32.to_le_bytes().to_vec();
    body.extend_from_slice(&10_u32.to_le_bytes());
    body.extend_from_slice(&20_u32.to_le_bytes());
    assert_golden(
        &Samples {
            values: vec![10, 20],
        },
        &body,
    );
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, RosMessage)]
#[ros(
    type = "builtin_interfaces/msg/Time",
    hash = "RIHS01_00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff"
)]
struct Time {
    sec: i32,
    nanosec: u32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, RosMessage)]
#[ros(
    type = "test_msgs/msg/Stamped",
    hash = "RIHS01_00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff"
)]
struct Stamped {
    lead: u8,
    stamp: Time,
}

#[test]
fn nested_message_is_inline() {
    let mut body = vec![0x09, 0, 0, 0]; // lead, pad to 4
    body.extend_from_slice(&5_i32.to_le_bytes());
    body.extend_from_slice(&6_u32.to_le_bytes());
    assert_golden(
        &Stamped {
            lead: 9,
            stamp: Time { sec: 5, nanosec: 6 },
        },
        &body,
    );
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, RosMessage)]
#[ros(
    type = "test_msgs/msg/Bounded",
    hash = "RIHS01_00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff"
)]
struct Bounded {
    #[ros(max_len = 4)]
    name: String,
    #[ros(max_len = 2)]
    values: Vec<u8>,
}

#[test]
fn bounded_string_and_sequence_fail_to_encode_past_their_bound() {
    let at_bound = Bounded {
        name: "abcd".into(),
        values: vec![1, 2],
    };
    assert!(at_bound.to_bytes().is_ok());

    let long_name = Bounded {
        name: "abcde".into(),
        ..at_bound.clone()
    };
    let err = long_name.to_bytes().expect_err("name past bound");
    assert!(err.contains("`name`"), "{err}");
    let mut buf = [0_u8; 256];
    assert_eq!(
        long_name.encode_into(&mut buf),
        Err(SerializeError::InvalidData)
    );

    let long_values = Bounded {
        values: vec![1, 2, 3],
        ..at_bound
    };
    assert!(long_values
        .to_bytes()
        .expect_err("values past bound")
        .contains("`values`"));
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, RosMessage)]
#[ros(
    type = "cell_msgs/msg/SpindleCommand",
    hash = "RIHS01_00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff",
    encode_capacity = 8
)]
struct SpindleCommand {
    rpm: f64,
    enabled: bool,
}

#[test]
fn identity_constants_follow_the_attributes() {
    assert_eq!(SpindleCommand::NAME, "cell_msgs/msg/SpindleCommand");
    assert_eq!(
        SpindleCommand::ROS_TYPE_NAME,
        "cell_msgs::msg::dds_::SpindleCommand_"
    );
    assert_eq!(SpindleCommand::ROS_TYPE_HASH, HASH);
    assert_eq!(SpindleCommand::WIRE_FORMAT, WireFormat::Cdr);
    assert_eq!(SpindleCommand::ENCODE_BUFFER_CAPACITY, Some(8));
    assert_eq!(Flag::ENCODE_BUFFER_CAPACITY, Some(256), "default capacity");
}

#[test]
fn undersized_capacity_reports_buffer_too_small() {
    // Header (4) + f64 at offset 0 (8) + bool = 13 bytes, more than 8.
    let value = SpindleCommand {
        rpm: 1200.0,
        enabled: true,
    };
    let mut buf = [0_u8; 8];
    assert_eq!(
        value.encode_into(&mut buf),
        Err(SerializeError::BufferTooSmall)
    );
    assert_eq!(value.to_bytes().expect("owned fallback").len(), 13);
}

#[test]
fn derived_names_agree_with_the_runtime_rules() {
    fn check<T: RosMessage>() {
        assert_eq!(
            ros2::dds_type_name(T::NAME).as_deref(),
            Ok(T::ROS_TYPE_NAME),
            "{}",
            T::NAME
        );
        assert_eq!(ros2::validate_dds_type_name(T::ROS_TYPE_NAME), Ok(()));
        assert_eq!(ros2::validate_type_hash(T::ROS_TYPE_HASH), Ok(()));
    }
    check::<Flag>();
    check::<Integers>();
    check::<Time>();
    check::<SpindleCommand>();
}
