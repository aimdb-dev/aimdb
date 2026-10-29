//! Per-route publish options, parsed once when the connector builds.

use alloc::format;
use alloc::string::String;

use aimdb_core::transport::ConnectorConfig;
use aimdb_core::RouteInfo;

/// How every message of one outbound route is published.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PublishOpts {
    /// 0, 1 or 2, from `?qos=`. Defaults to 1.
    pub qos: u8,
    /// From `?retain=`. Defaults to `false`.
    pub retain: bool,
}

/// The value of option `key` in a link's configuration.
pub(crate) fn option<'a>(config: &'a ConnectorConfig, key: &str) -> Option<&'a str> {
    config
        .protocol_options
        .iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.as_str())
}

/// Parse a link's `qos`: 0, 1 or 2, defaulting to 1. A value that does not
/// parse is an error naming the route `topic`, not a silent fallback.
pub(crate) fn parse_qos(config: &ConnectorConfig, topic: &str) -> Result<u8, String> {
    match option(config, "qos") {
        None => Ok(1),
        Some("0") => Ok(0),
        Some("1") => Ok(1),
        Some("2") => Ok(2),
        Some(other) => Err(format!(
            "route '{topic}': qos must be 0, 1 or 2, got '{other}'"
        )),
    }
}

impl PublishOpts {
    /// Parse `route`'s `qos` and `retain`. A value that does not parse is an
    /// error naming the route, not a silent fallback to the default.
    pub(crate) fn parse(route: &RouteInfo) -> Result<Self, String> {
        let qos = parse_qos(&route.config, &route.default_topic)?;
        let retain = match option(&route.config, "retain") {
            None | Some("false") => false,
            Some("true") => true,
            Some(other) => {
                return Err(format!(
                    "route '{}': retain must be true or false, got '{other}'",
                    route.default_topic
                ))
            }
        };
        Ok(Self { qos, retain })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::ToString;
    use alloc::sync::Arc;
    use alloc::vec::Vec;

    fn route(query: &[(&str, &str)]) -> RouteInfo {
        let query: Vec<(String, String)> = query
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        RouteInfo {
            id: 0,
            type_id: core::any::TypeId::of::<()>(),
            default_topic: Arc::from("sensors/t"),
            record_index: 0,
            config: ConnectorConfig::from_query(&query),
            topic_capacity: 0,
            payload_capacity: 0,
        }
    }

    #[test]
    fn defaults_are_qos_1_without_retain() {
        assert_eq!(
            PublishOpts::parse(&route(&[])),
            Ok(PublishOpts {
                qos: 1,
                retain: false
            })
        );
    }

    #[test]
    fn every_valid_value_parses() {
        for (qos, expected) in [("0", 0), ("1", 1), ("2", 2)] {
            assert_eq!(
                PublishOpts::parse(&route(&[("qos", qos)])).unwrap().qos,
                expected
            );
        }
        assert!(
            PublishOpts::parse(&route(&[("retain", "true")]))
                .unwrap()
                .retain
        );
        assert!(
            !PublishOpts::parse(&route(&[("retain", "false")]))
                .unwrap()
                .retain
        );
    }

    #[test]
    fn a_value_that_does_not_parse_names_the_route() {
        for query in [[("qos", "3")], [("qos", "abc")], [("retain", "yes")]] {
            let err = PublishOpts::parse(&route(&query)).unwrap_err();
            assert!(err.contains("route 'sensors/t'"), "{err}");
            assert!(err.contains(query[0].1), "{err}");
        }
    }
}
