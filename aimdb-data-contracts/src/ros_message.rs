//! ROS 2 type identity for contracts carried on `ros2://` links.
//!
//! [`RosMessage`] names the ROS interface a contract mirrors. The functions in
//! [`ros2`](crate::ros2) build and check the two names it carries, so a
//! connector can reject a malformed hand-written impl at build time.

use alloc::string::String;
use core::fmt;

use crate::Linkable;

/// The type is a ROS 2 message: its [`Linkable`] encoding is the message's
/// CDR form, and these two constants identify it on the wire.
///
/// A connector unlocks `ros2://` links for a type it registers with
/// `.register::<T>()`; links then carry the data.
///
/// ```rust
/// use aimdb_data_contracts::{Linkable, RosMessage, SchemaType, WireFormat};
///
/// #[derive(Clone, Debug)]
/// pub struct Empty;
///
/// impl SchemaType for Empty {
///     const NAME: &'static str = "std_msgs/msg/Empty";
/// }
///
/// impl Linkable for Empty {
///     const WIRE_FORMAT: WireFormat = WireFormat::Cdr;
///     # fn from_bytes(_: &[u8]) -> Result<Self, String> { Ok(Empty) }
///     # fn to_bytes(&self) -> Result<Vec<u8>, String> { Ok(Vec::new()) }
///     // `from_bytes` / `to_bytes` speak CDR.
/// }
///
/// impl RosMessage for Empty {
///     const ROS_TYPE_NAME: &'static str = "std_msgs::msg::dds_::Empty_";
///     // Copied from `ros2 topic info -v` on the robot.
///     const ROS_TYPE_HASH: &'static str =
///         "RIHS01_0000000000000000000000000000000000000000000000000000000000000000";
/// }
/// ```
pub trait RosMessage: Linkable {
    /// The DDS type name rmw_zenoh puts in data keys and liveliness tokens,
    /// e.g. `std_msgs::msg::dds_::String_`. See [`ros2::dds_type_name`](crate::ros2::dds_type_name).
    const ROS_TYPE_NAME: &'static str;
    /// The REP-2016 type hash: `RIHS01_` followed by 64 lowercase hex digits,
    /// as `ros2 topic info -v` prints it.
    const ROS_TYPE_HASH: &'static str;
}

/// Why a ROS type name or type hash was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum RosNameError {
    /// Not of the form `pkg/msg/Name` (or `pkg::msg::dds_::Name_`).
    Shape,
    /// The package breaks REP 144: lowercase letters, digits and single
    /// underscores, starting with a letter and not ending with `_`.
    Package,
    /// The message name is not an uppercase letter followed by letters and
    /// digits.
    Message,
    /// Not `RIHS01_` followed by 64 lowercase hex digits.
    Hash,
}

impl fmt::Display for RosNameError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Shape => "expected a ROS message type `pkg/msg/Name`",
            Self::Package => {
                "package must be lowercase letters, digits and single underscores, \
                 starting with a letter and not ending with '_'"
            }
            Self::Message => {
                "message name must start with an uppercase letter, then letters and digits"
            }
            Self::Hash => "type hash must be 'RIHS01_' followed by 64 lowercase hex digits",
        })
    }
}

#[cfg(feature = "std")]
impl std::error::Error for RosNameError {}

/// Name and hash checks for [`RosMessage`] implementations.
pub mod ros2 {
    use super::*;

    const DDS_INFIX: &str = "::msg::dds_::";
    const HASH_PREFIX: &str = "RIHS01_";

    /// The DDS type name for a ROS message type:
    /// `cell_msgs/msg/SpindleCommand` → `cell_msgs::msg::dds_::SpindleCommand_`.
    pub fn dds_type_name(ros_type: &str) -> Result<String, RosNameError> {
        let mut parts = ros_type.split('/');
        let (Some(package), Some("msg"), Some(message), None) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            return Err(RosNameError::Shape);
        };
        check_package(package)?;
        check_message(message)?;
        Ok(alloc::format!("{package}{DDS_INFIX}{message}_"))
    }

    /// Checks a [`RosMessage::ROS_TYPE_NAME`], which is in DDS form.
    pub fn validate_dds_type_name(name: &str) -> Result<(), RosNameError> {
        let (package, rest) = name.split_once(DDS_INFIX).ok_or(RosNameError::Shape)?;
        let message = rest.strip_suffix('_').ok_or(RosNameError::Shape)?;
        check_package(package)?;
        check_message(message)
    }

    /// Checks a [`RosMessage::ROS_TYPE_HASH`].
    pub fn validate_type_hash(hash: &str) -> Result<(), RosNameError> {
        let digits = hash.strip_prefix(HASH_PREFIX).ok_or(RosNameError::Hash)?;
        if digits.len() == 64
            && digits
                .bytes()
                .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
        {
            Ok(())
        } else {
            Err(RosNameError::Hash)
        }
    }

    fn check_package(package: &str) -> Result<(), RosNameError> {
        let bytes = package.as_bytes();
        let valid = bytes.first().is_some_and(u8::is_ascii_lowercase)
            && bytes.last() != Some(&b'_')
            && !package.contains("__")
            && bytes
                .iter()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'_');
        if valid {
            Ok(())
        } else {
            Err(RosNameError::Package)
        }
    }

    fn check_message(message: &str) -> Result<(), RosNameError> {
        let bytes = message.as_bytes();
        if bytes.first().is_some_and(u8::is_ascii_uppercase)
            && bytes.iter().all(u8::is_ascii_alphanumeric)
        {
            Ok(())
        } else {
            Err(RosNameError::Message)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::ros2::*;
    use super::RosNameError;

    #[test]
    fn dds_type_name_mangles_package_and_message() {
        assert_eq!(
            dds_type_name("std_msgs/msg/String").as_deref(),
            Ok("std_msgs::msg::dds_::String_")
        );
        assert_eq!(
            dds_type_name("cell_msgs/msg/SpindleCommand").as_deref(),
            Ok("cell_msgs::msg::dds_::SpindleCommand_")
        );
        assert_eq!(
            dds_type_name("sensor_msgs2/msg/Imu9").as_deref(),
            Ok("sensor_msgs2::msg::dds_::Imu9_")
        );
    }

    #[test]
    fn dds_type_name_refuses_other_shapes() {
        for bad in [
            "",
            "std_msgs",
            "std_msgs/String",
            "std_msgs/srv/String",
            "std_msgs/action/Fibonacci",
            "std_msgs/msg/String/Extra",
            "/std_msgs/msg/String",
        ] {
            assert_eq!(dds_type_name(bad), Err(RosNameError::Shape), "{bad:?}");
        }
    }

    #[test]
    fn package_follows_rep_144() {
        for bad in [
            "Std_msgs",
            "1msgs",
            "_msgs",
            "msgs_",
            "std__msgs",
            "std-msgs",
            "",
        ] {
            let ty = alloc::format!("{bad}/msg/String");
            assert_eq!(dds_type_name(&ty), Err(RosNameError::Package), "{bad:?}");
        }
    }

    #[test]
    fn message_is_upper_camel_alphanumeric() {
        for bad in ["string", "9Lives", "Spindle_Command", "Spindle-Command", ""] {
            let ty = alloc::format!("std_msgs/msg/{bad}");
            assert_eq!(dds_type_name(&ty), Err(RosNameError::Message), "{bad:?}");
        }
    }

    #[test]
    fn dds_names_round_trip_through_validation() {
        for ty in [
            "std_msgs/msg/String",
            "builtin_interfaces/msg/Time",
            "a/msg/B",
        ] {
            let dds = dds_type_name(ty).expect("valid type");
            assert_eq!(validate_dds_type_name(&dds), Ok(()), "{dds}");
        }
    }

    #[test]
    fn validate_dds_type_name_refuses_ros_form_and_damage() {
        assert_eq!(
            validate_dds_type_name("std_msgs/msg/String"),
            Err(RosNameError::Shape)
        );
        assert_eq!(
            validate_dds_type_name("std_msgs::msg::dds_::String"),
            Err(RosNameError::Shape)
        );
        assert_eq!(
            validate_dds_type_name("std_msgs::msg::String_"),
            Err(RosNameError::Shape)
        );
        assert_eq!(
            validate_dds_type_name("Std::msg::dds_::String_"),
            Err(RosNameError::Package)
        );
        assert_eq!(
            validate_dds_type_name("std_msgs::msg::dds_::string_"),
            Err(RosNameError::Message)
        );
    }

    #[test]
    fn type_hash_is_rihs01_and_64_lowercase_hex() {
        let good = alloc::format!("RIHS01_{}", "0123456789abcdef".repeat(4));
        assert_eq!(validate_type_hash(&good), Ok(()));

        let upper = good.to_uppercase();
        for bad in [
            "",
            "RIHS01_",
            &good[..good.len() - 1],
            &alloc::format!("{good}0"),
            &good.replace("RIHS01_", "RIHS02_"),
            &good.replacen('a', "g", 1),
            &upper,
            "TypeHashNotSupported",
        ] {
            assert_eq!(validate_type_hash(bad), Err(RosNameError::Hash), "{bad:?}");
        }
    }
}
