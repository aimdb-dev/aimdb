//! Compile-fail coverage for `#[derive(RosMessage)]`: malformed `#[ros(..)]`
//! attributes and unsupported shapes fail at compile time.

#![cfg(feature = "ros2")]

#[test]
fn compile_fail_ros_messages() {
    let t = trybuild::TestCases::new();
    t.compile_fail("tests/compile-fail-ros/*.rs");
}
