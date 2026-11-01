//! Private wire shapes of the AimX 4.0 replies that carry record values.
//!
//! `ts` is mandatory wherever a value appears (design 058 §5.1). Core's
//! `Stamped<T>` deliberately has no serde impls, so replies decode into these
//! types and are turned into envelopes with [`WriteMetadata::from_wire_stamp`]:
//! client-side metadata holds exactly what the wire carried.

use aimdb_core::{Stamp, Stamped, WriteMetadata};
use serde::Deserialize;
use serde_json::Value;

use crate::error::{ClientError, ClientResult};

/// `{"value": <value>, "ts": <ns>}`: a `record.get` reply and one
/// `record.drain` element.
#[derive(Deserialize)]
struct WireStamped {
    value: Value,
    ts: u64,
}

impl WireStamped {
    fn into_stamped(self) -> Stamped<Value> {
        Stamped::new(
            self.value,
            WriteMetadata::from_wire_stamp(Stamp::from_nanos(self.ts)),
        )
    }
}

/// A `record.drain` reply.
#[derive(Deserialize)]
pub(crate) struct WireDrain {
    pub(crate) record_name: String,
    values: Vec<WireStamped>,
    pub(crate) count: usize,
}

impl WireDrain {
    pub(crate) fn values(self) -> Vec<Stamped<Value>> {
        self.values
            .into_iter()
            .map(WireStamped::into_stamped)
            .collect()
    }
}

/// Decodes a `record.get` reply.
pub(crate) fn decode_get(reply: &[u8]) -> ClientResult<Stamped<Value>> {
    serde_json::from_slice::<WireStamped>(reply)
        .map(WireStamped::into_stamped)
        .map_err(|e| ClientError::protocol(format!("record.get reply: {e}")))
}

/// Decodes a `record.drain` reply.
pub(crate) fn decode_drain(reply: &[u8]) -> ClientResult<WireDrain> {
    serde_json::from_slice(reply)
        .map_err(|e| ClientError::protocol(format!("record.drain reply: {e}")))
}

/// Checks a `record.set` reply: `{status}` alone, or `{status, value, ts}`. A
/// reply that echoes a `value` (`null` included) must carry an integer `ts`.
pub(crate) fn check_set(reply: Value) -> ClientResult<Value> {
    let echoes_value = reply.get("value").is_some();
    let has_ts = reply.get("ts").is_some_and(Value::is_u64);
    if echoes_value && !has_ts {
        return Err(ClientError::protocol(
            "record.set reply echoes a value without an integer `ts`",
        ));
    }
    Ok(reply)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn get_requires_ts() {
        let got = decode_get(br#"{"value":{"c":21.5},"ts":81234567000}"#).unwrap();
        assert_eq!(got.value, json!({"c": 21.5}));
        assert_eq!(got.stamp(), Stamp::from_nanos(81_234_567_000));

        for reply in [
            &br#"{"value":1}"#[..],
            br#"{"value":1,"ts":"81"}"#,
            br#"{"value":1,"ts":-1}"#,
            br#"7"#,
        ] {
            assert!(matches!(
                decode_get(reply),
                Err(ClientError::Protocol { .. })
            ));
        }
    }

    #[test]
    fn drain_requires_ts_on_every_value() {
        let drained = decode_drain(
            br#"{"record_name":"r","values":[{"value":1,"ts":5},{"value":2,"ts":6}],"count":2}"#,
        )
        .unwrap();
        assert_eq!((drained.record_name.as_str(), drained.count), ("r", 2));
        let stamps: Vec<_> = drained.values().iter().map(Stamped::stamp).collect();
        assert_eq!(stamps, [Stamp::from_nanos(5), Stamp::from_nanos(6)]);

        let missing = br#"{"record_name":"r","values":[{"value":1,"ts":5},{"value":2}],"count":2}"#;
        assert!(matches!(
            decode_drain(missing),
            Err(ClientError::Protocol { .. })
        ));
    }

    #[test]
    fn set_reply_with_a_value_needs_ts() {
        for ok in [
            json!({"status": "success"}),
            json!({"status": "success", "value": 3, "ts": 9}),
            json!({"status": "success", "value": null, "ts": 9}),
        ] {
            assert!(check_set(ok).is_ok());
        }
        for bad in [
            json!({"status": "success", "value": 3}),
            json!({"status": "success", "value": null}),
            json!({"status": "success", "value": 3, "ts": "9"}),
        ] {
            assert!(matches!(check_set(bad), Err(ClientError::Protocol { .. })));
        }
    }
}
