//! Generic message router for efficient connector dispatch
//!
//! Provides O(M) routing complexity instead of O(N×M) filtered streams.
//! Routes incoming messages directly to fused ingest callbacks based on
//! topic/key matching.
//!
//! This router is protocol-agnostic and can be used by any connector:
//! - MQTT: Routes topics to records
//! - Kafka: Routes topics/partitions to records
//! - HTTP: Routes paths to records
//! - DDS: Routes topics to records
//! - Shared Memory: Routes segment names to records

use alloc::{boxed::Box, string::String, sync::Arc, vec::Vec};

use crate::connector::IngestFn;
use crate::inbound_key::{KeyId, KeyTable};
use crate::topic_pattern::{Spans, TopicFilter, TopicGrammar, TopicMatch, MAX_CAPTURES};

/// Generic message router for connector dispatch
///
/// Built by [`AimDb::inbound_router`](crate::AimDb::inbound_router). Routes
/// incoming messages to the matching records' ingest callbacks. Uses linear
/// search which is efficient for <100 routes.
///
/// # Performance
///
/// - O(M) complexity where M = number of routes
/// - May check multiple routes if same resource_id maps to multiple types
/// - Typical routing time: <1μs for <50 routes
///
/// # Protocol Support
///
/// This router is protocol-agnostic. Each connector uses it with their own resource_id format:
/// - **MQTT**: `topic` (e.g., "sensors/temperature")
/// - **Kafka**: `topic` or `topic:partition` (e.g., "events" or "events:0")
/// - **HTTP**: `path` (e.g., "/api/v1/sensors")
/// - **DDS**: `topic_name` (e.g., "TelemetryData")
/// - **Shmem**: `segment_name` (e.g., "temperature_buffer")
pub struct Router {
    /// List of all registered routes
    routes: Vec<CompiledRoute>,
    /// Decides covering in [`subscriptions`](Self::subscriptions).
    grammar: &'static dyn TopicGrammar,
}

/// A route as the router runs it. An exact route is one whose filter matches
/// only itself.
pub(crate) struct CompiledRoute {
    filter: Arc<str>,
    /// `None`: compare `filter` as a string.
    matcher: Option<Box<dyn TopicFilter>>,
    /// Capture names by number.
    names: Box<[Box<str>]>,
    /// Key table and the capture number whose value is keyed.
    key: Option<(Arc<KeyTable>, usize)>,
    ingest: IngestFn,
}

impl CompiledRoute {
    /// A route comparing `topic` as a string.
    #[cfg(all(test, feature = "connector-session"))]
    pub(crate) fn exact(topic: &str, ingest: IngestFn) -> Self {
        use crate::topic_pattern::{ExactGrammar, TopicPattern};
        let pattern = TopicPattern::parse(topic).expect("an exact topic");
        let filter = ExactGrammar.compile(&pattern).expect("an exact topic");
        Self::pattern(filter, Box::new([]), None, ingest)
    }

    /// A route matching through `filter`.
    pub(crate) fn pattern(
        filter: Box<dyn TopicFilter>,
        names: Box<[Box<str>]>,
        key: Option<(Arc<KeyTable>, usize)>,
        ingest: IngestFn,
    ) -> Self {
        Self {
            filter: filter.filter().into(),
            matcher: (!filter.is_literal()).then_some(filter),
            names,
            key,
            ingest,
        }
    }

    fn matches(&self, topic: &str, spans: &mut Spans) -> bool {
        match &self.matcher {
            None => *self.filter == *topic,
            Some(m) => topic.len() <= usize::from(u16::MAX) && m.matches(topic, spans),
        }
    }

    /// The message's key; `None` when the key table is full.
    fn key(&self, topic: &str, spans: &Spans) -> Option<Option<KeyId>> {
        let Some((table, capture)) = &self.key else {
            return Some(None);
        };
        let &(start, end) = spans.get(*capture)?;
        let value = topic.get(usize::from(start)..usize::from(end))?;
        table.key(value).map(Some)
    }
}

impl Router {
    pub(crate) fn new(grammar: &'static dyn TopicGrammar, routes: Vec<CompiledRoute>) -> Self {
        Self { routes, grammar }
    }

    /// Route a message to the appropriate record(s)
    ///
    /// Synchronous: the ingest callback deserializes and produces in place
    /// (`Producer::produce` is sync and infallible) — nothing on
    /// this path awaits.
    ///
    /// # Arguments
    /// * `resource_id` - Resource identifier (topic, path, segment name, etc.)
    /// * `payload` - Raw message payload bytes
    /// * `ctx` - Runtime context, threaded to context-aware deserializers
    ///
    /// # Returns
    /// * `Ok(())` - Always returns Ok, even if no routes matched or processing failed.
    ///   Failures are logged (via tracing/defmt) but do not propagate as errors.
    ///
    /// # Behavior
    /// - Checks all routes that match the resource_id (may be multiple)
    /// - Logs warnings on ingest (deserialization) failures but continues
    /// - Logs debug message if no routes found for resource_id
    pub fn route(
        &self,
        resource_id: &str,
        payload: &[u8],
        ctx: &crate::RuntimeContext,
    ) -> Result<(), String> {
        let mut routed = false;
        let mut matched = false;

        // Linear search through all routes
        // Note: Multiple routes may match the same resource_id (different types)
        let mut spans: Spans = [(0, 0); MAX_CAPTURES];
        for route in &self.routes {
            if route.matches(resource_id, &mut spans) {
                matched = true;
                let Some(key) = route.key(resource_id, &spans) else {
                    log_debug!("Key table full, dropped message on '{}'", resource_id);
                    continue;
                };
                let m = TopicMatch::new(resource_id, &route.names, &spans, key);
                match (route.ingest)(ctx, &m, payload) {
                    Ok(()) => {
                        routed = true;

                        log_debug!("Routed message on '{}' to producer", resource_id);
                    }
                    Err(_e) => {
                        log_warn!("Failed to ingest message on '{}': {}", resource_id, _e);

                        #[cfg(feature = "defmt")]
                        defmt::warn!(
                            "Failed to ingest message on '{}': {}",
                            resource_id,
                            _e.as_str()
                        );
                    }
                }
            }
        }

        if !routed {
            if matched {
                log_debug!(
                    "Route matched for '{}' but message was not produced (ingest errors)",
                    resource_id
                );

                #[cfg(feature = "defmt")]
                defmt::debug!("Route matched for '{}' but not produced", resource_id);
            } else {
                log_debug!("No route found for resource: '{}'", resource_id);

                #[cfg(feature = "defmt")]
                defmt::debug!("No route found for resource: '{}'", resource_id);
            }
        }

        Ok(())
    }

    /// Get list of all resource IDs registered in this router
    ///
    /// Useful for subscribing at the protocol level (e.g., MQTT SUBSCRIBE).
    /// Returns unique resource IDs (deduplicated even if multiple routes per resource).
    pub fn resource_ids(&self) -> Vec<Arc<str>> {
        let mut ids: Vec<Arc<str>> = self.routes.iter().map(|r| r.filter.clone()).collect();

        // Deduplicate by converting to strings for comparison
        ids.sort_unstable_by(|a, b| a.as_ref().cmp(b.as_ref()));
        ids.dedup_by(|a, b| a.as_ref() == b.as_ref());

        ids
    }

    /// Filters to subscribe: [`resource_ids`](Self::resource_ids) without
    /// the filters another one covers.
    pub fn subscriptions(&self) -> Vec<Arc<str>> {
        let ids = self.resource_ids();
        let g = self.grammar;
        ids.iter()
            .enumerate()
            .filter(|&(i, a)| {
                // Of two filters covering each other, the first one stays.
                !ids.iter()
                    .enumerate()
                    .any(|(j, b)| j != i && g.covers(b, a) && (j < i || !g.covers(a, b)))
            })
            .map(|(_, a)| a.clone())
            .collect()
    }

    /// Get the number of routes in this router
    pub fn route_count(&self) -> usize {
        self.routes.len()
    }
}

#[cfg(all(test, feature = "std"))]
mod tests {
    use super::*;
    use crate::topic_pattern::{test_support::Plus, ExactGrammar, TopicPattern};
    use core::num::NonZeroU16;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    /// A `RuntimeContext` backed by the shared no-op RuntimeOps.
    fn test_ctx() -> crate::RuntimeContext {
        crate::RuntimeContext::new(Arc::new(crate::executor::test_support::NoopRuntimeOps))
    }

    /// Ingest callback that counts successful invocations.
    fn counting_ingest(call_count: Arc<AtomicUsize>) -> IngestFn {
        Arc::new(move |_ctx, _m, _payload| {
            call_count.fetch_add(1, Ordering::SeqCst);
            Ok(())
        })
    }

    fn noop() -> IngestFn {
        counting_ingest(Arc::new(AtomicUsize::new(0)))
    }

    type Seen = Arc<Mutex<Vec<(String, Vec<Option<String>>, Option<usize>)>>>;

    /// Records the topic, the named captures and the key index.
    fn recording(seen: &Seen, names: &'static [&'static str]) -> IngestFn {
        let seen = seen.clone();
        Arc::new(move |_ctx, m, _payload| {
            let caps = names.iter().map(|n| m.get(n).map(String::from)).collect();
            seen.lock()
                .unwrap()
                .push((m.topic().to_string(), caps, m.key().map(|k| k.index())));
            Ok(())
        })
    }

    fn keyed_route(
        topic: &str,
        ingest: IngestFn,
        key: Option<(Arc<KeyTable>, usize)>,
    ) -> CompiledRoute {
        let pattern = TopicPattern::parse(topic).unwrap();
        let names = pattern.capture_names().map(Box::from).collect();
        CompiledRoute::pattern(Plus.compile(&pattern).unwrap(), names, key, ingest)
    }

    fn route(topic: &str, ingest: IngestFn) -> CompiledRoute {
        keyed_route(topic, ingest, None)
    }

    #[test]
    fn every_matching_route_receives_the_message() {
        let (a, b) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
        let router = Router::new(
            &Plus,
            vec![
                route("shared/resource", counting_ingest(a.clone())),
                route("shared/resource", counting_ingest(b.clone())),
            ],
        );
        let ctx = test_ctx();
        router.route("shared/resource", b"", &ctx).unwrap();
        router.route("unknown/resource", b"", &ctx).unwrap();

        assert_eq!(a.load(Ordering::SeqCst), 1);
        assert_eq!(b.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn ingest_receives_payload_and_its_errors_do_not_propagate() {
        let seen_len = Arc::new(AtomicUsize::new(0));
        let len = seen_len.clone();
        let router = Router::new(
            &Plus,
            vec![
                route(
                    "r",
                    Arc::new(move |_ctx, _m, payload| {
                        len.store(payload.len(), Ordering::SeqCst);
                        Ok(())
                    }),
                ),
                route("r", Arc::new(|_ctx, _m, _payload| Err("bad".into()))),
            ],
        );
        router.route("r", b"dummy", &test_ctx()).unwrap();
        assert_eq!(seen_len.load(Ordering::SeqCst), 5);
    }

    #[test]
    fn resource_ids_are_deduplicated() {
        let router = Router::new(
            &Plus,
            vec![
                route("r1", noop()),
                route("r1", noop()),
                route("r2", noop()),
            ],
        );
        let ids: Vec<String> = router
            .resource_ids()
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(ids, ["r1", "r2"]);
    }

    #[test]
    fn pattern_route_receives_topic_and_captures() {
        let seen: Seen = Default::default();
        let router = Router::new(
            &Plus,
            vec![route(
                "{site}/+/{dev}",
                recording(&seen, &["site", "dev", "nope"]),
            )],
        );
        let ctx = test_ctx();
        router.route("vienna/x/k1", b"", &ctx).unwrap();
        router.route("vienna/k1", b"", &ctx).unwrap();

        assert_eq!(
            *seen.lock().unwrap(),
            vec![(
                "vienna/x/k1".to_string(),
                vec![Some("vienna".into()), Some("k1".into()), None],
                None
            )]
        );
    }

    #[test]
    fn literal_and_pattern_routes_both_receive_a_message() {
        let seen: Seen = Default::default();
        let router = Router::new(
            &Plus,
            vec![
                route("s/{d}/t", recording(&seen, &["d"])),
                route("s/kitchen/t", recording(&seen, &[])),
            ],
        );
        router.route("s/kitchen/t", b"", &test_ctx()).unwrap();

        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 2);
        // A literal route still receives the topic.
        assert_eq!(seen[1].0, "s/kitchen/t");
    }

    #[test]
    fn keyed_route_assigns_keys_and_drops_when_full() {
        let seen: Seen = Default::default();
        let table = Arc::new(KeyTable::new(NonZeroU16::new(2).unwrap()));
        let router = Router::new(
            &Plus,
            vec![keyed_route(
                "s/{d}/t",
                recording(&seen, &["d"]),
                Some((table.clone(), 0)),
            )],
        );
        let ctx = test_ctx();
        for topic in ["s/a/t", "s/b/t", "s/a/t", "s/c/t"] {
            router.route(topic, b"", &ctx).unwrap();
        }

        let keys: Vec<_> = seen.lock().unwrap().iter().map(|s| s.2).collect();
        assert_eq!(keys, [Some(0), Some(1), Some(0)]);
        assert_eq!(table.dropped(), 1);
    }

    #[test]
    fn overlong_topics_skip_pattern_routes() {
        let seen: Seen = Default::default();
        let router = Router::new(&Plus, vec![route("{x}", recording(&seen, &[]))]);
        let topic = "x".repeat(usize::from(u16::MAX) + 1);
        router.route(&topic, b"", &test_ctx()).unwrap();
        assert!(seen.lock().unwrap().is_empty());
    }

    fn subscriptions(router: &Router) -> Vec<String> {
        router
            .subscriptions()
            .iter()
            .map(|s| s.to_string())
            .collect()
    }

    #[test]
    fn subscriptions_drop_covered_filters() {
        let router = Router::new(
            &Plus,
            vec![
                route("s/kitchen/t", noop()),
                route("other/x", noop()),
                route("s/+/t", noop()),
                route("s/{d}/t", noop()),
            ],
        );
        assert_eq!(subscriptions(&router), ["other/x", "s/+/t"]);
    }

    #[test]
    fn exact_grammar_routes_compare_strings() {
        let hits = Arc::new(AtomicUsize::new(0));
        let exact = CompiledRoute::exact("a/+", counting_ingest(hits.clone()));
        let router = Router::new(&ExactGrammar, vec![exact]);
        let ctx = test_ctx();
        router.route("a/b", b"", &ctx).unwrap();
        router.route("a/+", b"", &ctx).unwrap();
        assert_eq!(hits.load(Ordering::SeqCst), 1);
        assert_eq!(subscriptions(&router), ["a/+"]);
    }
}
