//! Per-filter subscribe QoS, worked out once when the connector builds.
//!
//! One subscribed filter can stand for several inbound links: a pattern link
//! covers an exact one (`sensors/+/temp` covers `sensors/kitchen/temp`), and
//! core subscribes only the covering filter. Each filter is subscribed at the
//! highest `qos` among the links it covers, so every link receives at least
//! what it asked for (#268). Covering is decided by [`MqttGrammar`], the same
//! grammar core's router uses to pick the filters.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::sync::Arc;
use alloc::vec::Vec;

use aimdb_core::{InboundDispatch, InboundRouteInfo, TopicGrammar, TopicPattern};

use crate::publish_opts::parse_qos;
use crate::MqttGrammar;

/// One filter to subscribe, and the QoS to subscribe it at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Subscription {
    pub filter: String,
    /// 0, 1 or 2: the highest `qos` among the links the filter covers.
    pub qos: u8,
}

/// The `qos` an inbound route asks for; an error names the route.
pub(crate) fn route_qos(route: &InboundRouteInfo) -> Result<u8, String> {
    parse_qos(&route.config, &route.topic)
}

/// The filters `inbound` subscribes, each with its QoS. Reports every route
/// whose `qos` does not parse, not only the first.
pub(crate) fn subscriptions(inbound: &InboundDispatch) -> Result<Vec<Subscription>, String> {
    let mut links = Vec::with_capacity(inbound.routes().len());
    let mut problems = Vec::new();
    for route in inbound.routes() {
        match (route_qos(route), filter_of(&route.topic)) {
            (Ok(qos), Ok(filter)) => links.push((filter, qos)),
            (Err(e), _) | (_, Err(e)) => problems.push(e),
        }
    }
    if !problems.is_empty() {
        return Err(problems.join("; "));
    }
    Ok(assign(&inbound.subscriptions(), &links))
}

/// The filter a route topic compiles to: `sensors/{room}/temp` becomes
/// `sensors/+/temp`. Core already compiled every route with this grammar, so
/// this does not fail for a route `InboundDispatch` accepted.
fn filter_of(topic: &str) -> Result<String, String> {
    let pattern = TopicPattern::parse(topic).map_err(|e| format!("route '{topic}': {e}"))?;
    let filter = MqttGrammar
        .compile(&pattern)
        .map_err(|e| format!("route '{topic}': {e}"))?;
    Ok(filter.filter().to_string())
}

/// Each of `filters` with the highest QoS among `links` (compiled filter,
/// QoS) it covers; 1 if it covers none.
fn assign(filters: &[Arc<str>], links: &[(String, u8)]) -> Vec<Subscription> {
    filters
        .iter()
        .map(|filter| Subscription {
            filter: filter.to_string(),
            qos: links
                .iter()
                .filter(|(link, _)| MqttGrammar.covers(filter, link))
                .map(|&(_, qos)| qos)
                .max()
                .unwrap_or(1),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn links(links: &[(&str, u8)]) -> Vec<(String, u8)> {
        links.iter().map(|&(f, q)| (f.to_string(), q)).collect()
    }

    fn qos_of(filters: &[&str], links: &[(String, u8)]) -> Vec<(String, u8)> {
        let filters: Vec<Arc<str>> = filters.iter().map(|&f| Arc::from(f)).collect();
        assign(&filters, links)
            .into_iter()
            .map(|s| (s.filter, s.qos))
            .collect()
    }

    #[test]
    fn a_filter_takes_the_highest_qos_among_the_links_it_covers() {
        let links = links(&[
            ("sensors/+/temp", 0),
            ("sensors/kitchen/temp", 2),
            ("sensors/hall/temp", 1),
        ]);
        assert_eq!(
            qos_of(&["sensors/+/temp"], &links),
            [("sensors/+/temp".to_string(), 2)]
        );
    }

    #[test]
    fn links_a_filter_does_not_cover_do_not_raise_it() {
        let links = links(&[("cmd/in", 0), ("status/out", 2)]);
        assert_eq!(
            qos_of(&["cmd/in", "status/out"], &links),
            [("cmd/in".to_string(), 0), ("status/out".to_string(), 2)]
        );
    }

    #[test]
    fn a_link_two_filters_cover_raises_both() {
        // Neither filter covers the other, so core keeps both; the broker may
        // deliver `a/b/c` through either.
        let links = links(&[("a/+/c", 0), ("a/b/+", 0), ("a/b/c", 2)]);
        assert_eq!(
            qos_of(&["a/+/c", "a/b/+"], &links),
            [("a/+/c".to_string(), 2), ("a/b/+".to_string(), 2)]
        );
    }

    #[test]
    fn the_default_is_qos_1() {
        assert_eq!(qos_of(&["cmd/in"], &[]), [("cmd/in".to_string(), 1)]);
    }

    #[test]
    fn a_route_topic_compiles_to_its_filter() {
        assert_eq!(filter_of("sensors/{room}/temp").unwrap(), "sensors/+/temp");
        assert_eq!(filter_of("logs/{rest..}").unwrap(), "logs/#");
        assert_eq!(filter_of("cmd/in").unwrap(), "cmd/in");
    }

    /// A database with one inbound `u64` link per `(topic, qos)`, and an MQTT
    /// connector to an unreachable broker (`build()` does not dial).
    #[cfg(feature = "std")]
    async fn inbound(links: &'static [(&'static str, Option<&'static str>)]) -> InboundDispatch {
        use aimdb_core::buffer::BufferCfg;
        use aimdb_tokio_adapter::{TokioAdapter, TokioRecordRegistrarExt};

        let mut builder = aimdb_core::AimDbBuilder::new()
            .runtime(Arc::new(TokioAdapter))
            .with_connector(crate::MqttConnector::new("mqtt://127.0.0.1:1"));
        for (i, &(topic, qos)) in links.iter().enumerate() {
            let key = aimdb_core::StringKey::intern(alloc::format!("in{i}"));
            builder.configure::<u64>(key, move |reg| {
                let mut link = reg
                    .buffer(BufferCfg::SingleLatest)
                    .link_from(&alloc::format!("mqtt://{topic}"))
                    .with_deserializer(|_ctx, _b: &[u8]| Ok(0u64));
                if let Some(qos) = qos {
                    link = link.with_config("qos", qos);
                }
                link.finish();
            });
        }
        let (db, _runner) = builder.build().await.expect("build");
        InboundDispatch::new(&db, "mqtt", &MqttGrammar).expect("routes compile")
    }

    #[cfg(feature = "std")]
    #[tokio::test]
    async fn a_covered_links_qos_reaches_its_covering_filter() {
        let inbound = inbound(&[
            ("sensors/{room}/temp", Some("0")),
            ("sensors/kitchen/temp", Some("2")),
            ("cmd/in", None),
        ])
        .await;
        let subscriptions = subscriptions(&inbound).unwrap();
        assert_eq!(
            subscriptions,
            alloc::vec![
                Subscription {
                    filter: "cmd/in".to_string(),
                    qos: 1
                },
                Subscription {
                    filter: "sensors/+/temp".to_string(),
                    qos: 2
                },
            ]
        );
    }
}
