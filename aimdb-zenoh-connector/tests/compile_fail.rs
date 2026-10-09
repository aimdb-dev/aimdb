//! `Ros2Connector::register` accepts only `RosMessage` types.

#![cfg(feature = "std")]

#[test]
fn register_needs_a_ros_message() {
    trybuild::TestCases::new().compile_fail("tests/compile-fail/*.rs");
}
