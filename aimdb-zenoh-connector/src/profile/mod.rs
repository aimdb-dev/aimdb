//! rmw_zenoh's wire conventions: names, data keys, liveliness tokens, the
//! QoS field, GIDs and the publication attachment.
//!
//! Pure functions over strings and bytes, `no_std + alloc`, so a ROS node on
//! an MCU can reuse them unchanged.

mod attachment;
mod gid;
mod keys;
mod names;
mod qos;

#[cfg(any(feature = "std", test))]
pub(crate) use attachment::Attachment;
#[cfg(test)]
pub(crate) use attachment::ATTACHMENT_LEN;
#[cfg(any(feature = "std", test))]
pub(crate) use gid::gid;
#[cfg(test)]
pub(crate) use keys::mangle;
#[cfg(any(feature = "std", test))]
pub(crate) use keys::{data_key, entity_token, node_token, EntityKind, Node};
#[cfg(feature = "std")]
pub(crate) use names::{validate_namespace, validate_node_name, validate_topic};
pub(crate) use qos::Qos;
pub use qos::Reliability;
#[cfg(any(feature = "std", test))]
pub(crate) use qos::{Durability, History};

#[cfg(test)]
mod golden;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_qos_is_depth_10_reliable_volatile() {
        assert_eq!(Qos::default().encode(), "::,10:,:,:,,");
        let depth_42 = Qos {
            depth: 42,
            ..Qos::default()
        };
        assert_eq!(depth_42.encode(), "::,:,:,:,,", "rmw_zenoh's default depth");
    }

    #[test]
    fn mangling_turns_slashes_into_percent() {
        let m = |name: &str| alloc::format!("{}", mangle(name));
        assert_eq!(m(""), "%");
        assert_eq!(m("/"), "%");
        assert_eq!(m("/cell4"), "%cell4");
        assert_eq!(m("/cell4/temperature"), "%cell4%temperature");
    }

    #[test]
    fn data_keys_drop_the_outer_slashes() {
        assert_eq!(data_key(3, "/a/b", "T_", "H"), "3/a/b/T_/H");
        assert_eq!(data_key(0, "a/b", "T_", "H"), "0/a/b/T_/H");
    }

    #[test]
    fn attachments_round_trip_and_reject_other_layouts() {
        let a = Attachment {
            sequence: 1,
            timestamp_ns: 0,
            gid: [7; 16],
        };
        let bytes = a.encode();
        assert_eq!(bytes.len(), ATTACHMENT_LEN);
        assert_eq!(
            &bytes[..17],
            &[1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 16]
        );
        assert_eq!(Attachment::decode(&bytes), Some(a));
        assert_eq!(Attachment::decode(&bytes[..32]), None);
        let mut wrong_length_byte = bytes;
        wrong_length_byte[16] = 15;
        assert_eq!(Attachment::decode(&wrong_length_byte), None);
    }
}
