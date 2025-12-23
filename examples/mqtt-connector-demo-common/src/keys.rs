//! Compile-time safe record keys for MQTT demo
//!
//! Using enum keys instead of string literals provides:
//! - **Compile-time typo detection**: `MqttKey::TempIndor` won't compile
//! - **Exhaustive matching**: `match` statements catch missing cases
//! - **Zero allocation**: Copy types, no heap usage
//! - **IDE autocomplete**: Full tooling support
//! - **Connector metadata**: MQTT topics defined alongside keys
//!
//! This is especially valuable for embedded systems where runtime
//! errors are harder to debug.

use aimdb_core::RecordKey;

/// Record keys for MQTT temperature sensors (outbound: AimDB → MQTT)
///
/// Each variant maps to a unique record key string and includes
/// the MQTT topic via `#[link_address]`.
#[derive(RecordKey, Clone, Copy, PartialEq, Eq, Hash, Debug)]
#[key_prefix = "sensor."]
pub enum SensorKey {
    /// Indoor temperature sensor record
    #[key = "temp.indoor"]
    #[link_address = "mqtt://sensors/temp/indoor"]
    TempIndoor,

    /// Outdoor temperature sensor record
    #[key = "temp.outdoor"]
    #[link_address = "mqtt://sensors/temp/outdoor"]
    TempOutdoor,

    /// Server room temperature sensor record
    #[key = "temp.server_room"]
    #[link_address = "mqtt://sensors/temp/server_room"]
    TempServerRoom,
}

/// Command keys for receiving MQTT commands (inbound: MQTT → AimDB)
///
/// Each variant includes the MQTT topic to subscribe to via `#[link_address]`.
#[derive(RecordKey, Clone, Copy, PartialEq, Eq, Hash, Debug)]
#[key_prefix = "command."]
pub enum CommandKey {
    /// Indoor temperature commands
    #[key = "temp.indoor"]
    #[link_address = "mqtt://commands/temp/indoor"]
    TempIndoor,

    /// Outdoor temperature commands
    #[key = "temp.outdoor"]
    #[link_address = "mqtt://commands/temp/outdoor"]
    TempOutdoor,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sensor_key_as_str() {
        assert_eq!(SensorKey::TempIndoor.as_str(), "sensor.temp.indoor");
        assert_eq!(SensorKey::TempOutdoor.as_str(), "sensor.temp.outdoor");
        assert_eq!(
            SensorKey::TempServerRoom.as_str(),
            "sensor.temp.server_room"
        );
    }

    #[test]
    fn test_sensor_key_link_address() {
        assert_eq!(
            SensorKey::TempIndoor.link_address(),
            Some("mqtt://sensors/temp/indoor")
        );
        assert_eq!(
            SensorKey::TempOutdoor.link_address(),
            Some("mqtt://sensors/temp/outdoor")
        );
        assert_eq!(
            SensorKey::TempServerRoom.link_address(),
            Some("mqtt://sensors/temp/server_room")
        );
    }

    #[test]
    fn test_command_key_link_address() {
        assert_eq!(
            CommandKey::TempIndoor.link_address(),
            Some("mqtt://commands/temp/indoor")
        );
        assert_eq!(
            CommandKey::TempOutdoor.link_address(),
            Some("mqtt://commands/temp/outdoor")
        );
    }

    #[test]
    fn test_command_key_as_str() {
        assert_eq!(CommandKey::TempIndoor.as_str(), "command.temp.indoor");
        assert_eq!(CommandKey::TempOutdoor.as_str(), "command.temp.outdoor");
    }

    #[test]
    fn test_keys_are_copy() {
        let key = SensorKey::TempIndoor;
        let key2 = key; // Copy, not move
        assert_eq!(key, key2);
    }
}
